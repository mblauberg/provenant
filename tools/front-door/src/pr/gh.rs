//! `gh api` transport for conditional requests.
use std::{collections::BTreeMap, io, time::Duration};

/// The REST API version used by the shared forge adapter.
pub(crate) use provenant_config::ports::REST_API_VERSION_HEADER;

/// A response from an authenticated conditional REST request.
#[derive(Debug)]
pub struct Response {
    pub status: u16,
    pub headers: BTreeMap<String, String>,
    pub body: Vec<u8>,
}

/// A serial REST adapter. Header names in responses must be lowercase.
pub trait Forge {
    fn get(&mut self, path: &str, etag: Option<&str>) -> io::Result<Response>;
    /// Bounds later requests to `left`. Adapters without a deadline ignore it.
    fn bound(&mut self, _left: Duration) {}
}

/// Seconds named by a `Retry-After` header, in seconds or as an HTTP date.
fn retry_after(headers: &BTreeMap<String, String>, now: i64) -> Option<u64> {
    let value = headers.get("retry-after")?;
    value.parse::<u64>().ok().or_else(|| {
        let date = value.strip_suffix(" GMT")?;
        let at =
            jiff::Timestamp::strptime("%a, %d %b %Y %H:%M:%S %z", format!("{date} +0000")).ok()?;
        Some(at.as_second().saturating_sub(now).max(0) as u64)
    })
}

/// The reset of an exhausted primary window. A secondary limit may carry the
/// headers of an unrelated window, which only counts when nothing remains.
fn exhausted_reset(headers: &BTreeMap<String, String>) -> Option<i64> {
    if headers.get("x-ratelimit-remaining").map(String::as_str) != Some("0") {
        return None;
    }
    headers.get("x-ratelimit-reset")?.parse::<i64>().ok()
}

/// The Unix second from which a rate-limited response may be retried, or
/// `None` when `response`, received at `now`, is not a rate limit.
pub fn rate_limit_reset(response: &Response, now: i64) -> Option<i64> {
    let limited = response.status == 429
        || (response.status == 403
            && (exhausted_reset(&response.headers).is_some()
                || response.headers.contains_key("retry-after")
                || String::from_utf8_lossy(&response.body)
                    .to_ascii_lowercase()
                    .contains("rate limit")));
    if !limited {
        return None;
    }
    Some(
        retry_after(&response.headers, now)
            .map(|seconds| now.saturating_add(i64::try_from(seconds).unwrap_or(i64::MAX)))
            .or_else(|| exhausted_reset(&response.headers))
            .unwrap_or(now.saturating_add(60))
            .max(now),
    )
}
