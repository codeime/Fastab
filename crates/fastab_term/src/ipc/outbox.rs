use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use bytes::Bytes;
use fastab_proto::FigProtobufEncodable;
use fastab_proto::prost::Message;
use fastab_proto::remote::Hostbound;
use tokio::sync::{Notify, watch};
use tracing::warn;

// Engineering limits, not measured optima. Count the frame being written too:
// a slow peer must not move an unbounded backlog out of the queue into writers.
const MAX_PENDING_MESSAGES: usize = 256;
const MAX_PENDING_BYTES: usize = 4 * 1024 * 1024;
const FRAME_HEADER_BYTES: usize = 18;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Generation(u64);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RequestOrigin {
    Local,
    Remote(Generation),
}

impl RequestOrigin {
    pub(crate) fn is_current(self, ready: Option<Generation>) -> bool {
        match self {
            Self::Local => true,
            Self::Remote(generation) => ready == Some(generation),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ConnectionPhase {
    Disconnected,
    Connecting(Generation),
    Handshaking(Generation),
    Ready(Generation),
    Stopped,
}

impl ConnectionPhase {
    pub(crate) fn ready_generation(self) -> Option<Generation> {
        match self {
            Self::Ready(generation) => Some(generation),
            _ => None,
        }
    }

    fn generation(self) -> Option<Generation> {
        match self {
            Self::Connecting(generation) | Self::Handshaking(generation) | Self::Ready(generation) => Some(generation),
            Self::Disconnected | Self::Stopped => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct ContextProgress {
    pub(crate) full_context_admitted: bool,
    environment_epoch: Option<u64>,
}

impl ContextProgress {
    pub(crate) fn needs_environment(self, epoch: u64) -> bool {
        self.environment_epoch != Some(epoch)
    }

    pub(crate) fn record(&mut self, admitted: ContextAdmission) {
        self.full_context_admitted |= admitted.full_context;
        if let Some(epoch) = admitted.environment_epoch {
            self.environment_epoch = Some(epoch);
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct ContextAdmission {
    pub(crate) full_context: bool,
    pub(crate) environment_epoch: Option<u64>,
}

#[derive(Debug)]
pub(crate) enum AdmissionError {
    StaleGeneration,
    CapacityExceeded,
    EncodingFailed,
}

struct State {
    phase: ConnectionPhase,
    last_generation: u64,
    accounting_generation: Option<Generation>,
    queue: VecDeque<Bytes>,
    pending_messages: usize,
    pending_bytes: usize,
    context: ContextProgress,
}

struct Shared {
    state: Mutex<State>,
    changed: watch::Sender<ConnectionPhase>,
    queued: Notify,
}

impl Shared {
    fn set_phase(&self, state: &mut State, phase: ConnectionPhase) {
        state.phase = phase;
        self.changed.send_replace(phase);
    }

    fn clear_queue(state: &mut State) {
        while let Some(frame) = state.queue.pop_front() {
            state.pending_messages -= 1;
            state.pending_bytes -= frame.len();
        }
        state.context = ContextProgress::default();
    }

    fn invalidate(&self, state: &mut State, generation: Generation, reason: &str) {
        if state.phase.generation() != Some(generation) {
            return;
        }
        warn!(?generation, reason, "Remote connection retired");
        Self::clear_queue(state);
        self.set_phase(state, ConnectionPhase::Disconnected);
        self.queued.notify_waiters();
    }
}

#[derive(Clone)]
pub(crate) struct RemoteSender {
    shared: Arc<Shared>,
}

#[derive(Clone)]
pub(crate) struct GenerationSender {
    sender: RemoteSender,
    generation: Generation,
}

impl RemoteSender {
    pub(super) fn new() -> Self {
        let (changed, _) = watch::channel(ConnectionPhase::Disconnected);
        Self {
            shared: Arc::new(Shared {
                state: Mutex::new(State {
                    phase: ConnectionPhase::Disconnected,
                    last_generation: 0,
                    accounting_generation: None,
                    queue: VecDeque::new(),
                    pending_messages: 0,
                    pending_bytes: 0,
                    context: ContextProgress::default(),
                }),
                changed,
                queued: Notify::new(),
            }),
        }
    }

    pub(crate) fn phase(&self) -> ConnectionPhase {
        self.shared.state.lock().unwrap().phase
    }

    pub(crate) fn subscribe(&self) -> watch::Receiver<ConnectionPhase> {
        self.shared.changed.subscribe()
    }

    pub(crate) fn for_generation(&self, generation: Generation) -> GenerationSender {
        GenerationSender { sender: self.clone(), generation }
    }

    pub(crate) fn current(&self) -> Option<GenerationSender> {
        self.phase().ready_generation().map(|generation| self.for_generation(generation))
    }

    pub(super) fn begin_attempt(&self) -> Option<Generation> {
        let mut state = self.shared.state.lock().unwrap();
        if state.phase == ConnectionPhase::Stopped {
            return None;
        }
        // The supervisor drops both I/O futures before it starts another attempt.
        debug_assert_eq!(state.pending_messages, 0);
        debug_assert_eq!(state.pending_bytes, 0);
        let Some(next) = state.last_generation.checked_add(1) else {
            self.shared.set_phase(&mut state, ConnectionPhase::Stopped);
            return None;
        };
        let generation = Generation(next);
        state.last_generation = next;
        state.accounting_generation = Some(generation);
        state.context = ContextProgress::default();
        self.shared.set_phase(&mut state, ConnectionPhase::Connecting(generation));
        Some(generation)
    }

    pub(super) fn set_handshaking(&self, generation: Generation) {
        let mut state = self.shared.state.lock().unwrap();
        if state.phase == ConnectionPhase::Connecting(generation) {
            self.shared.set_phase(&mut state, ConnectionPhase::Handshaking(generation));
        }
    }

    pub(super) fn set_ready(&self, generation: Generation) {
        let mut state = self.shared.state.lock().unwrap();
        if state.phase == ConnectionPhase::Handshaking(generation) {
            self.shared.set_phase(&mut state, ConnectionPhase::Ready(generation));
        }
    }

    pub(super) fn retire(&self, generation: Generation, reason: &str) {
        let mut state = self.shared.state.lock().unwrap();
        self.shared.invalidate(&mut state, generation, reason);
    }

    pub(super) fn stop(&self) {
        let mut state = self.shared.state.lock().unwrap();
        Shared::clear_queue(&mut state);
        self.shared.set_phase(&mut state, ConnectionPhase::Stopped);
        self.shared.queued.notify_waiters();
    }

    pub(super) async fn invalidated(&self, generation: Generation) {
        let mut changes = self.subscribe();
        loop {
            if self.phase().generation() != Some(generation) {
                return;
            }
            if changes.changed().await.is_err() {
                return;
            }
        }
    }

    pub(super) async fn next_frame(&self, generation: Generation) -> Option<PendingFrame> {
        loop {
            let notified = self.shared.queued.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            {
                let mut state = self.shared.state.lock().unwrap();
                if state.phase != ConnectionPhase::Ready(generation) {
                    return None;
                }
                if let Some(bytes) = state.queue.pop_front() {
                    return Some(PendingFrame { bytes, sender: self.clone(), generation });
                }
            }
            notified.await;
        }
    }
}

impl GenerationSender {
    pub(crate) fn generation(&self) -> Generation {
        self.generation
    }

    pub(crate) fn is_current_ready(&self) -> bool {
        self.sender.phase() == ConnectionPhase::Ready(self.generation)
    }

    pub(crate) fn context_progress(&self) -> Option<ContextProgress> {
        let state = self.sender.shared.state.lock().unwrap();
        (state.phase == ConnectionPhase::Ready(self.generation)).then_some(state.context)
    }

    pub(crate) fn try_send(&self, message: Hostbound) -> Result<(), AdmissionError> {
        self.try_send_with_context(message, ContextAdmission::default())
    }

    pub(crate) fn try_send_with_context(
        &self,
        message: Hostbound,
        context: ContextAdmission,
    ) -> Result<(), AdmissionError> {
        if !self.is_current_ready() {
            return Err(AdmissionError::StaleGeneration);
        }
        let frame_len = message.encoded_len().checked_add(FRAME_HEADER_BYTES);
        if frame_len.is_none_or(|len| len > MAX_PENDING_BYTES) {
            self.sender.retire(self.generation, "outgoing frame exceeds byte budget");
            return Err(AdmissionError::CapacityExceeded);
        }
        let encoded = match message.encode_fastab_protobuf() {
            Ok(encoded) => encoded,
            Err(err) => {
                self.sender.retire(self.generation, &format!("outgoing frame encoding failed: {err}"));
                return Err(AdmissionError::EncodingFailed);
            },
        };
        let mut state = self.sender.shared.state.lock().unwrap();
        if state.phase != ConnectionPhase::Ready(self.generation) {
            return Err(AdmissionError::StaleGeneration);
        }
        if state.pending_messages >= MAX_PENDING_MESSAGES
            || encoded.len() > MAX_PENDING_BYTES - state.pending_bytes
        {
            self.sender.shared.invalidate(&mut state, self.generation, "outgoing queue exceeds budget");
            return Err(AdmissionError::CapacityExceeded);
        }
        state.pending_messages += 1;
        state.pending_bytes += encoded.len();
        state.queue.push_back(encoded);
        state.context.record(context);
        drop(state);
        self.sender.shared.queued.notify_one();
        Ok(())
    }
}

pub(super) struct PendingFrame {
    pub(super) bytes: Bytes,
    sender: RemoteSender,
    generation: Generation,
}

impl Drop for PendingFrame {
    fn drop(&mut self) {
        let mut state = self.sender.shared.state.lock().unwrap();
        if state.accounting_generation == Some(self.generation) {
            state.pending_messages -= 1;
            state.pending_bytes -= self.bytes.len();
        }
    }
}
