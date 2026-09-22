use std::time::Duration;

pub const MAX_CANDIDATES: usize = 20;
pub const MAX_DESCRIPTION_BYTES: usize = 256;
pub const MAX_REQUEST_BYTES: usize = 16 * 1024;
pub const MAX_RESPONSE_BYTES: usize = 64 * 1024;
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(2);
pub const REQUESTS_PER_MINUTE: usize = 20;
pub const PROBABILITY_TOLERANCE: f64 = 0.000_001;
pub const DEFAULT_COOLDOWN: Duration = Duration::from_secs(30);
pub const MIN_COOLDOWN: Duration = Duration::from_secs(1);
pub const MAX_COOLDOWN: Duration = Duration::from_secs(24 * 60 * 60);
pub const KEEP_LOCAL: &str = "keep_local";
pub const QUESTION_ID: &str = "recommendation";
pub const DATA_POLICY_VERSION: u32 = 1;
pub const MAX_PROFILES: usize = 16;

/// Header parsing is deliberately independent of response bodies, which may echo input.
/// Clamp server delays to one second through one day; absent/invalid values use
/// thirty seconds. A cooldown never schedules a retry by itself.
pub fn cooldown(headers: &reqwest::header::HeaderMap) -> Duration {
    let milliseconds = headers
        .get("retry-after-ms")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok())
        .map(Duration::from_millis);
    let seconds_or_date = || {
        let value = headers.get(reqwest::header::RETRY_AFTER)?.to_str().ok()?;
        if let Ok(seconds) = value.parse::<u64>() {
            return Some(Duration::from_secs(seconds));
        }
        let date = time::OffsetDateTime::parse(value, &time::format_description::well_known::Rfc2822).ok()?;
        let seconds = (date - time::OffsetDateTime::now_utc()).whole_seconds().max(0);
        u64::try_from(seconds).ok().map(Duration::from_secs)
    };
    milliseconds
        .or_else(seconds_or_date)
        .unwrap_or(DEFAULT_COOLDOWN)
        .clamp(MIN_COOLDOWN, MAX_COOLDOWN)
}
