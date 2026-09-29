//! Process-local aggregate diagnostics. No commands, paths, credentials or bodies.

use std::collections::VecDeque;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

const MAX_SAMPLES: usize = 128;

#[derive(Clone, Copy, Debug)]
pub enum Metric {
    Eligible,
    Request,
    CacheHit,
    Response,
    KeptLocal,
    Promoted,
    Accepted,
    NotAccepted,
    Cancelled,
    Failed,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Status {
    #[default]
    Idle,
    Disabled,
    NotReady,
    PendingLocal,
    Unsupported,
    TooFewCandidates,
    ChangedContext,
    Navigating,
    Waiting,
    Collecting,
    Requesting,
    CacheHit,
    KeptLocal,
    Promoted,
    Accepted,
    NotAccepted,
    Cancelled,
    Cooldown,
    RateLimited,
    Busy,
    Failed,
    Timeout,
    Authentication,
    PaymentRequired,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Ticket {
    serial: u64,
}

#[derive(Clone, Debug, Default)]
pub struct Snapshot {
    pub evaluated: u64,
    pub skipped: u64,
    pub skip_reasons: Vec<(Status, u64)>,
    pub last_status: Status,
    pub latency_samples: usize,
    pub average_latency_ms: Option<u64>,
    pub p95_latency_ms: Option<u64>,
    counts: [u64; 10],
}

impl Snapshot {
    pub fn count(&self, metric: Metric) -> u64 {
        self.counts[metric as usize]
    }
}

#[derive(Default)]
struct Diagnostics {
    serial: u64,
    summary: Snapshot,
    latencies: VecDeque<u64>,
    closed: Option<u64>,
}

impl Diagnostics {
    fn begin(&mut self) -> Ticket {
        self.serial = self.serial.wrapping_add(1);
        self.closed = None;
        self.summary.last_status = Status::Waiting;
        self.summary.evaluated = self.summary.evaluated.saturating_add(1);
        Ticket { serial: self.serial }
    }

    fn status(&mut self, ticket: Ticket, status: Status) {
        if ticket.serial == self.serial && self.closed != Some(ticket.serial) {
            if self.summary.last_status == Status::Promoted && !matches!(status, Status::Accepted | Status::NotAccepted)
            {
                return;
            }
            self.summary.last_status = status;
            if !matches!(
                status,
                Status::Idle
                    | Status::Waiting
                    | Status::Collecting
                    | Status::Requesting
                    | Status::CacheHit
                    | Status::Promoted
            ) {
                self.closed = Some(ticket.serial);
            }
        }
    }

    fn record(&mut self, ticket: Ticket, metric: Metric, status: Status) {
        let count = &mut self.summary.counts[metric as usize];
        *count = count.saturating_add(1);
        self.status(ticket, status);
    }

    fn skip(&mut self, ticket: Ticket, status: Status) {
        self.summary.skipped = self.summary.skipped.saturating_add(1);
        if let Some((_, count)) = self
            .summary
            .skip_reasons
            .iter_mut()
            .find(|(reason, _)| *reason == status)
        {
            *count = count.saturating_add(1);
        } else {
            self.summary.skip_reasons.push((status, 1));
        }
        self.status(ticket, status);
    }

    fn latency(&mut self, _ticket: Ticket, elapsed: Duration) {
        // Late requests still contribute to the process-wide latency sample.
        if self.latencies.len() == MAX_SAMPLES {
            self.latencies.pop_front();
        }
        self.latencies
            .push_back(elapsed.as_millis().min(u128::from(u64::MAX)) as u64);
    }

    fn snapshot(&self) -> Snapshot {
        let mut snapshot = self.summary.clone();
        let mut samples: Vec<_> = self.latencies.iter().copied().collect();
        snapshot.latency_samples = samples.len();
        if !samples.is_empty() {
            snapshot.average_latency_ms =
                Some((samples.iter().map(|ms| u128::from(*ms)).sum::<u128>() / samples.len() as u128) as u64);
            samples.sort_unstable();
            snapshot.p95_latency_ms = Some(samples[(samples.len() * 95).div_ceil(100) - 1]);
        }
        snapshot
    }
}

fn state() -> &'static Mutex<Diagnostics> {
    static STATE: OnceLock<Mutex<Diagnostics>> = OnceLock::new();
    STATE.get_or_init(Mutex::default)
}

fn with<R>(f: impl FnOnce(&mut Diagnostics) -> R) -> R {
    f(&mut state().lock().unwrap_or_else(|error| error.into_inner()))
}

pub fn begin() -> Ticket {
    with(Diagnostics::begin)
}
pub fn snapshot() -> Snapshot {
    with(|state| state.snapshot())
}
pub fn status(ticket: Ticket, status: Status) {
    with(|state| state.status(ticket, status));
}
pub fn record(ticket: Ticket, metric: Metric, status: Status) {
    with(|state| state.record(ticket, metric, status));
}
pub fn skip(ticket: Ticket, status: Status) {
    with(|state| state.skip(ticket, status));
}
pub fn latency(ticket: Ticket, elapsed: Duration) {
    with(|state| state.latency(ticket, elapsed));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn old_requests_cannot_replace_new_status_but_remain_in_aggregate_counts() {
        let mut state = Diagnostics::default();
        let old = state.begin();
        let current = state.begin();
        state.record(current, Metric::Request, Status::Requesting);
        state.record(old, Metric::Cancelled, Status::Cancelled);
        assert_eq!(state.snapshot().last_status, Status::Requesting);
        assert_eq!(state.snapshot().count(Metric::Cancelled), 1);
        state.skip(old, Status::Failed);
        state.latency(old, Duration::from_millis(80));
        let snapshot = state.snapshot();
        assert_eq!(snapshot.last_status, Status::Requesting);
        assert_eq!(snapshot.evaluated, 2);
        assert_eq!(snapshot.skipped, 1);
        assert_eq!(snapshot.latency_samples, 1);
    }

    #[test]
    fn samples_are_bounded_and_different_business_outcomes_stay_separate() {
        let mut state = Diagnostics::default();
        let ticket = state.begin();
        state.record(ticket, Metric::Response, Status::KeptLocal);
        state.record(ticket, Metric::KeptLocal, Status::KeptLocal);
        for ms in 1..=200 {
            state.latency(ticket, Duration::from_millis(ms));
        }
        let snapshot = state.snapshot();
        assert_eq!(snapshot.count(Metric::Response), 1);
        assert_eq!(snapshot.count(Metric::Promoted), 0);
        assert_eq!(snapshot.count(Metric::Cancelled), 0);
        assert_eq!(snapshot.latency_samples, MAX_SAMPLES);
        assert_eq!(snapshot.average_latency_ms, Some(136));
        assert_eq!(snapshot.p95_latency_ms, Some(194));
    }

    #[test]
    fn cancelled_status_survives_late_transport_but_counts_the_request() {
        let mut state = Diagnostics::default();
        let ticket = state.begin();
        state.record(ticket, Metric::Cancelled, Status::Cancelled);
        state.record(ticket, Metric::Request, Status::Requesting);
        state.record(ticket, Metric::Response, Status::Collecting);
        assert_eq!(state.snapshot().last_status, Status::Cancelled);
        assert_eq!(state.snapshot().count(Metric::Request), 1);
        assert_eq!(state.snapshot().count(Metric::Response), 1);
        let next = state.begin();
        state.record(next, Metric::Promoted, Status::Promoted);
        state.record(next, Metric::Accepted, Status::Accepted);
        assert_eq!(state.snapshot().last_status, Status::Accepted);
    }
}
