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

impl Status {
    pub fn label(self, zh: bool) -> &'static str {
        match (self, zh) {
            (Self::Idle, true) => "本次启动尚无记录",
            (Self::Idle, false) => "No activity since launch",
            (Self::Disabled, true) => "AI 未启用",
            (Self::Disabled, false) => "AI is disabled",
            (Self::NotReady, true) => "配置或密钥未就绪",
            (Self::NotReady, false) => "Configuration or key is not ready",
            (Self::PendingLocal, true) => "等待本地候选完成",
            (Self::PendingLocal, false) => "Local candidates are still loading",
            (Self::Unsupported, true) => "场景未覆盖或公开来源校验未通过",
            (Self::Unsupported, false) => "Unsupported context or unverified public source",
            (Self::TooFewCandidates, true) => "无需推荐：合格候选不足两项",
            (Self::TooFewCandidates, false) => "Fewer than two eligible candidates",
            (Self::ChangedContext, true) => "上下文已变化或未完整读取",
            (Self::ChangedContext, false) => "Context changed or could not be fully read",
            (Self::Navigating, true) => "正在浏览候选，保留当前顺序",
            (Self::Navigating, false) => "Browsing candidates; keeping the current order",
            (Self::Waiting, true) => "等待输入稳定",
            (Self::Waiting, false) => "Waiting for input to settle",
            (Self::Collecting, true) => "读取有界上下文",
            (Self::Collecting, false) => "Collecting bounded context",
            (Self::Requesting, true) => "已发起推荐请求",
            (Self::Requesting, false) => "Recommendation request started",
            (Self::CacheHit, true) => "复用当前上下文的短时缓存",
            (Self::CacheHit, false) => "Reused a recent result for the current context",
            (Self::KeptLocal, true) => "保持本地首项和顺序",
            (Self::KeptLocal, false) => "Kept the local first item and order",
            (Self::Promoted, true) => "已调整首项，等待用户选择",
            (Self::Promoted, false) => "Changed the first item; awaiting user choice",
            (Self::Accepted, true) => "推荐项接受动作已发送",
            (Self::Accepted, false) => "Acceptance of the recommended item was sent",
            (Self::NotAccepted, true) => "本次推荐未采用",
            (Self::NotAccepted, false) => "This recommendation was not accepted",
            (Self::Cancelled, true) => "输入或浏览操作取消了在途推荐",
            (Self::Cancelled, false) => "Input or browsing cancelled the pending recommendation",
            (Self::Cooldown, true) => "服务冷却中",
            (Self::Cooldown, false) => "Provider cooldown is active",
            (Self::RateLimited, true) => "达到本机每分钟请求上限",
            (Self::RateLimited, false) => "Local requests-per-minute limit reached",
            (Self::Busy, true) => "前一请求仍在结束",
            (Self::Busy, false) => "The previous request is still ending",
            (Self::Failed, true) => "服务请求失败，保留本地结果",
            (Self::Failed, false) => "Request failed; local results were retained",
            (Self::Timeout, true) => "请求超时，保留本地结果",
            (Self::Timeout, false) => "Request timed out; local results were retained",
            (Self::Authentication, true) => "需要更新服务密钥",
            (Self::Authentication, false) => "Update the provider key",
            (Self::PaymentRequired, true) => "需要检查服务商额度",
            (Self::PaymentRequired, false) => "Check provider account credit",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Ticket {
    epoch: u64,
    serial: u64,
}

#[derive(Clone, Default)]
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
    epoch: u64,
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
        Ticket {
            epoch: self.epoch,
            serial: self.serial,
        }
    }

    fn current(&self, ticket: Ticket) -> bool {
        ticket.epoch == self.epoch
    }

    fn status(&mut self, ticket: Ticket, status: Status) {
        if self.current(ticket) && ticket.serial == self.serial && self.closed != Some(ticket.serial) {
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
        if self.current(ticket) {
            let count = &mut self.summary.counts[metric as usize];
            *count = count.saturating_add(1);
            self.status(ticket, status);
        }
    }

    fn skip(&mut self, ticket: Ticket, status: Status) {
        if !self.current(ticket) {
            return;
        }
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

    fn latency(&mut self, ticket: Ticket, elapsed: Duration) {
        if !self.current(ticket) {
            return;
        }
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

    fn reset(&mut self) {
        self.epoch = self.epoch.wrapping_add(1);
        self.summary = Snapshot::default();
        self.latencies.clear();
        self.closed = None;
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
pub fn reset() {
    with(Diagnostics::reset);
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
    fn old_requests_cannot_replace_new_status_or_pollute_reset_stats() {
        let mut state = Diagnostics::default();
        let old = state.begin();
        let current = state.begin();
        state.record(current, Metric::Request, Status::Requesting);
        state.record(old, Metric::Cancelled, Status::Cancelled);
        assert_eq!(state.snapshot().last_status, Status::Requesting);
        assert_eq!(state.snapshot().count(Metric::Cancelled), 1);
        state.reset();
        state.record(current, Metric::Response, Status::Promoted);
        state.skip(old, Status::Failed);
        state.latency(current, Duration::from_millis(80));
        assert_eq!(state.snapshot().evaluated, 0);
        assert_eq!(state.snapshot().skipped, 0);
        assert_eq!(state.snapshot().count(Metric::Response), 0);
        assert_eq!(state.snapshot().latency_samples, 0);
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
