//! When to retry, and how long to wait: `Retry-After` (RFC 9110 §10.2.3), the `RateLimit`
//! field of draft-ietf-httpapi-ratelimit-headers, and exponential backoff with jitter.

use reqwest::StatusCode;
use reqwest::header::HeaderMap;
use std::time::{Duration, SystemTime};

/// How often and how patiently a call is retried. See the crate documentation for which
/// responses are retried.
#[derive(Clone, Debug)]
pub struct RetryPolicy {
    /// Retries after the first attempt (0: never retry).
    pub max_retries: u32,
    /// The first backoff when the server gives no delay; it doubles with each retry.
    pub initial_backoff: Duration,
    /// The longest backoff the client computes itself.
    pub max_backoff: Duration,
    /// The longest `Retry-After` the client waits for; a longer one ends the retries and
    /// the error is returned.
    pub max_retry_after: Duration,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        RetryPolicy {
            max_retries: 3,
            initial_backoff: Duration::from_millis(250),
            max_backoff: Duration::from_secs(30),
            max_retry_after: Duration::from_secs(60),
        }
    }
}

impl RetryPolicy {
    /// A policy that never retries.
    pub fn none() -> Self {
        RetryPolicy {
            max_retries: 0,
            ..Default::default()
        }
    }

    /// The computed backoff before retry `attempt` (0 for the first retry), with full
    /// jitter: a uniformly random time up to the exponential bound.
    pub(crate) fn backoff(&self, attempt: u32) -> Duration {
        let bound = self
            .initial_backoff
            .saturating_mul(1u32 << attempt.min(16))
            .min(self.max_backoff);
        let nanos = bound.as_nanos() as u64;
        if nanos == 0 {
            return Duration::ZERO;
        }
        Duration::from_nanos(jitter() % (nanos + 1))
    }
}

/// A cheap random number: the clock's nanoseconds mixed with a counter (SplitMix64).
fn jitter() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(0);
    let t = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    let mut z = t ^ N.fetch_add(0x9E37_79B9_7F4A_7C15, Ordering::Relaxed);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// Whether a response with this status may be retried. `safe` requests (reads, PUT,
/// DELETE, queries) are retried on 429, 502, 503 and 504. Other writes only when the
/// server refused them before doing any work: 429 from the rate limiter, and 503 with
/// `Retry-After` from a concurrency cap or a restore.
pub(crate) fn retryable_status(status: StatusCode, headers: &HeaderMap, safe: bool) -> bool {
    match status.as_u16() {
        429 => true,
        503 if headers.contains_key(reqwest::header::RETRY_AFTER) => true,
        502..=504 => safe,
        _ => false,
    }
}

/// `Retry-After` as a duration: delta-seconds, or an HTTP date relative to now.
pub(crate) fn retry_after(headers: &HeaderMap) -> Option<Duration> {
    let v = headers
        .get(reqwest::header::RETRY_AFTER)?
        .to_str()
        .ok()?
        .trim();
    if let Ok(s) = v.parse::<u64>() {
        return Some(Duration::from_secs(s));
    }
    let when = httpdate::parse_http_date(v).ok()?;
    Some(
        when.duration_since(SystemTime::now())
            .unwrap_or(Duration::ZERO),
    )
}

/// The `RateLimit` field of a response: what is left of the quota, and when it resets.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RateLimit {
    /// The policy's name, such as `query` or `query@public`.
    pub policy: String,
    /// Requests available now (`r`).
    pub remaining: u64,
    /// Seconds until the quota is full again (`t`).
    pub reset: Duration,
}

/// Parse `RateLimit: "query";r=57;t=1` (the first item when several are listed).
pub(crate) fn rate_limit(headers: &HeaderMap) -> Option<RateLimit> {
    let v = headers.get("ratelimit")?.to_str().ok()?;
    let item = v.split(',').next()?.trim();
    let mut parts = item.split(';');
    let policy = parts.next()?.trim().trim_matches('"').to_string();
    let (mut r, mut t) = (None, None);
    for p in parts {
        let (k, v) = p.trim().split_once('=')?;
        match k.trim() {
            "r" => r = v.trim().parse::<u64>().ok(),
            "t" => t = v.trim().parse::<u64>().ok(),
            _ => {}
        }
    }
    Some(RateLimit {
        policy,
        remaining: r?,
        reset: Duration::from_secs(t.unwrap_or(0)),
    })
}

/// The server's delay for a refused request: `Retry-After`, else the reset time of an
/// exhausted `RateLimit`.
pub(crate) fn server_delay(headers: &HeaderMap) -> Option<Duration> {
    retry_after(headers).or_else(|| {
        rate_limit(headers)
            .filter(|r| r.remaining == 0)
            .map(|r| r.reset)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use reqwest::header::HeaderValue;

    fn h(name: &'static str, v: &str) -> HeaderMap {
        let mut m = HeaderMap::new();
        m.insert(name, HeaderValue::from_str(v).unwrap());
        m
    }

    #[test]
    fn retry_after_forms() {
        assert_eq!(
            retry_after(&h("retry-after", "2")),
            Some(Duration::from_secs(2))
        );
        let later = SystemTime::now() + Duration::from_secs(120);
        let d = retry_after(&h("retry-after", &httpdate::fmt_http_date(later))).unwrap();
        assert!(
            d > Duration::from_secs(110) && d <= Duration::from_secs(120),
            "{d:?}"
        );
        assert_eq!(
            retry_after(&h("retry-after", "Wed, 21 Oct 2015 07:28:00 GMT")),
            Some(Duration::ZERO)
        );
        assert_eq!(retry_after(&h("retry-after", "soon")), None);
        assert_eq!(retry_after(&HeaderMap::new()), None);
    }

    #[test]
    fn rate_limit_field() {
        assert_eq!(
            rate_limit(&h("ratelimit", "\"query\";r=57;t=1")),
            Some(RateLimit {
                policy: "query".into(),
                remaining: 57,
                reset: Duration::from_secs(1)
            })
        );
        assert_eq!(
            rate_limit(&h("ratelimit", "\"q@ds\";r=0;t=4, \"other\";r=1;t=1"))
                .unwrap()
                .remaining,
            0
        );
        assert_eq!(
            server_delay(&h("ratelimit", "\"query\";r=0;t=3")),
            Some(Duration::from_secs(3))
        );
        assert_eq!(server_delay(&h("ratelimit", "\"query\";r=5;t=3")), None);
        assert_eq!(rate_limit(&h("ratelimit", "garbage")), None);
    }

    #[test]
    fn which_statuses_retry() {
        let none = HeaderMap::new();
        let ra = h("retry-after", "1");
        let s = |c: u16| StatusCode::from_u16(c).unwrap();
        assert!(retryable_status(s(429), &none, false));
        assert!(retryable_status(s(503), &ra, false));
        assert!(!retryable_status(s(503), &none, false));
        assert!(retryable_status(s(503), &none, true));
        assert!(!retryable_status(s(502), &none, false));
        assert!(retryable_status(s(504), &none, true));
        assert!(!retryable_status(s(500), &none, true));
        assert!(!retryable_status(s(400), &ra, true));
    }

    #[test]
    fn backoff_is_bounded() {
        let p = RetryPolicy::default();
        for a in 0..40 {
            assert!(p.backoff(a) <= p.max_backoff);
        }
        assert!(p.backoff(0) <= Duration::from_millis(250));
    }
}
