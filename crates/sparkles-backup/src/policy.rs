//! Lifecycle policies as pure functions: schedules (cron in a time zone, or
//! `every <duration>`), name templates, and retention. The server's scheduler and the
//! CLI's `backup policy preview` call these; nothing here reads a clock.
//!
//! Schedule rules (a test pins them):
//! * cron has 5 fields, or 6 with seconds first, evaluated in naive local time of the
//!   policy's IANA zone, then mapped to UTC: a local time that does not exist (spring
//!   forward) runs at the first instant after the gap; an ambiguous one (fall back)
//!   runs once, at its first occurrence;
//! * `every <duration>` (at least 1 minute) counts from the Unix epoch in UTC
//!   (`every 6h`: 00:00, 06:00, … UTC), so it is stable across restarts.

use crate::error::Result;
use crate::{BackupError, BackupSummary, Code, Retention};
use chrono::{DateTime, Utc};
pub use chrono_tz::Tz;
use std::collections::HashSet;
use std::time::Duration;

/// A parsed schedule.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Schedule {
    /// the text it was parsed from
    pub(crate) source: String,
}

impl Schedule {
    pub fn source(&self) -> &str {
        &self.source
    }
}

/// Parse a schedule: cron (5 or 6 fields) or `every <n><s|m|h|d>`. Errors are
/// `400 invalid-schedule` with a message for people.
pub fn parse_schedule(s: &str) -> Result<Schedule> {
    let _ = s;
    Err(BackupError::unsupported("schedules"))
}

/// An IANA time zone name (`400 invalid-schedule` for an unknown one).
pub fn parse_timezone(s: &str) -> Result<Tz> {
    s.parse::<Tz>()
        .map_err(|_| BackupError::new(Code::InvalidSchedule, format!("unknown time zone {s:?}")))
}

/// A retention or grace duration: `<n>` followed by `m`, `h`, `d` or `w` (`30d`,
/// `12h`); `400 invalid-config` otherwise.
pub fn parse_duration(s: &str) -> Result<Duration> {
    let _ = s;
    Err(BackupError::unsupported("durations"))
}

/// The next `n` instants of `s` strictly after `after`.
pub fn next_runs(s: &Schedule, tz: Tz, after: DateTime<Utc>, n: usize) -> Vec<DateTime<Utc>> {
    let _ = (s, tz, after, n);
    Vec::new()
}

/// The most recent instant of `s` at or before `now` that is later than `since` (the
/// persisted `lastScheduledFor`; `None`: never ran, and then only an instant at or
/// before `now` within one period counts): the instant a (catch-up) run is due for.
/// `None` when nothing is due. Monotonic in `since`, so a backward clock jump never
/// re-runs an instant.
pub fn latest_due(
    s: &Schedule,
    tz: Tz,
    now: DateTime<Utc>,
    since: Option<DateTime<Utc>>,
) -> Option<DateTime<Utc>> {
    let _ = (s, tz, now, since);
    None
}

/// A human description (`at 02:30 every day (Europe/Berlin)`, `every 6 hours`).
pub fn describe(s: &Schedule, tz: Tz) -> String {
    format!("{} ({tz})", s.source)
}

/// What a name template can refer to.
#[derive(Clone, Debug)]
pub struct NameCtx<'a> {
    pub policy: &'a str,
    pub dataset: &'a str,
    /// the dataset's head commit
    pub seq: u64,
    /// the run id (`{run}` is its first 8 hex digits)
    pub run: uuid::Uuid,
    /// the scheduled instant (`{time}`: `YYYYMMDDtHHMMSSz` in UTC)
    pub time: DateTime<Utc>,
    /// the policy's zone (`{date:FMT}` is rendered in it; FMT is a subset of strftime:
    /// `%Y %m %d %H %M %S %j %V`)
    pub tz: Tz,
}

/// Render a name template (`{policy}`, `{dataset}`, `{seq}`, `{run}`, `{time}`,
/// `{date:FMT}`). The result must be a valid backup name (`400 invalid-config`
/// otherwise, as are unknown placeholders and format directives).
pub fn render_name(template: &str, ctx: &NameCtx) -> Result<String> {
    let _ = (template, ctx);
    Err(BackupError::unsupported("name templates"))
}

/// What retention does to a policy's backups.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RetentionPlan {
    pub delete: Vec<BackupSummary>,
    pub keep: Vec<BackupSummary>,
}

/// Retention of policy `policy`: only backups whose `policy` is `policy` are
/// considered (others are in neither list). Per dataset id, newest `completed` first:
/// the first `minCount` are kept; of the rest, those at index ≥ `maxCount` or with
/// `completed + expireAfter < now` are deleted, except names in `busy` (a restore or
/// verify uses them), which are kept until the next evaluation.
///
/// Until retention is implemented, everything is kept.
pub fn retention(
    backups: &[BackupSummary],
    policy: &str,
    r: &Retention,
    now: DateTime<Utc>,
    busy: &HashSet<String>,
) -> RetentionPlan {
    let _ = (r, now, busy);
    RetentionPlan {
        delete: Vec::new(),
        keep: backups
            .iter()
            .filter(|b| b.policy.as_deref() == Some(policy))
            .cloned()
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn time_zones() {
        assert_eq!(parse_timezone("Europe/Berlin").unwrap(), Tz::Europe__Berlin);
        assert_eq!(parse_timezone("UTC").unwrap(), Tz::UTC);
        assert_eq!(
            parse_timezone("Mars/Base").unwrap_err().code(),
            Code::InvalidSchedule
        );
    }
}
