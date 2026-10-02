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
//!
//! Croner finds the matching wall clock times (it sees them as `NaiveDateTime`s, so it
//! knows nothing of the zone); [`resolve`] maps each to an instant with the rules above.
//! That mapping never decreases as the wall clock time grows, which is what lets
//! [`next_runs`] and [`latest_due`] walk the wall clock and keep the first instant on
//! the right side of their bound.

use crate::error::Result;
use crate::layout::{time_tag, valid_backup_name, valid_repo_name};
use crate::{BackupError, BackupSummary, Code, PolicyConfig, Retention};
use chrono::{DateTime, LocalResult, NaiveDateTime, TimeDelta, TimeZone, Timelike, Utc};
pub use chrono_tz::Tz;
use croner::Cron;
use croner::parser::{CronParser, Seconds, Year};
use std::collections::{BTreeMap, HashSet};
use std::time::Duration;

/// A parsed schedule.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Schedule {
    /// the text it was parsed from
    pub(crate) source: String,
    kind: Kind,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Kind {
    /// wall clock times in the policy's zone
    Cron(Box<Cron>),
    /// every so many seconds since the Unix epoch
    Every(i64),
}

impl Schedule {
    pub fn source(&self) -> &str {
        &self.source
    }

    /// The interval of an `every <duration>` schedule (`None` for cron).
    pub fn interval(&self) -> Option<Duration> {
        match self.kind {
            Kind::Every(s) => Some(Duration::from_secs(s as u64)),
            Kind::Cron(_) => None,
        }
    }
}

/// Bounds the walks over wall clock matches that map to instants already passed (the
/// repeated hour of a fall-back transition, or the times inside a spring-forward gap):
/// at most one match per second of a few hours.
const MAX_STEPS: usize = 100_000;

fn bad_schedule(msg: impl std::fmt::Display) -> BackupError {
    BackupError::new(Code::InvalidSchedule, format!("invalid schedule: {msg}"))
}

/// Parse a schedule: cron (5 or 6 fields) or `every <duration>` (see
/// [`parse_duration`]; at least 1 minute). Errors are `400 invalid-schedule` with a
/// message for people.
pub fn parse_schedule(s: &str) -> Result<Schedule> {
    let src = s.trim();
    if let Some(rest) = strip_every(src) {
        let d = parse_duration(rest).map_err(|e| bad_schedule(e.message()))?;
        if d < Duration::from_secs(60) {
            return Err(bad_schedule("an interval must be at least 1 minute"));
        }
        if d.subsec_nanos() != 0 || d.as_secs() > i64::MAX as u64 {
            return Err(bad_schedule("an interval must be whole seconds"));
        }
        return Ok(Schedule {
            source: src.to_string(),
            kind: Kind::Every(d.as_secs() as i64),
        });
    }
    let fields = src.split_whitespace().count();
    if src.contains('@') || !(fields == 5 || fields == 6) {
        return Err(bad_schedule(
            "a cron schedule has 5 fields (or 6 with seconds first), or use \"every <duration>\"",
        ));
    }
    let cron = CronParser::builder()
        .seconds(Seconds::Optional)
        .year(Year::Disallowed)
        .build()
        .parse(src)
        .map_err(bad_schedule)?;
    // a pattern like `0 0 31 2 *` parses but never runs
    let probe = chrono::NaiveDate::from_ymd_opt(2026, 1, 1)
        .and_then(|d| d.and_hms_opt(0, 0, 0))
        .expect("a valid date");
    if cron.find_next_occurrence(&probe, false).is_err() {
        return Err(bad_schedule("the schedule never runs"));
    }
    Ok(Schedule {
        source: src.to_string(),
        kind: Kind::Cron(Box::new(cron)),
    })
}

/// `every <rest>` (any case) → `<rest>`.
fn strip_every(s: &str) -> Option<&str> {
    let head = s.get(..5)?;
    let rest = &s[5..];
    (head.eq_ignore_ascii_case("every") && rest.starts_with(char::is_whitespace))
        .then(|| rest.trim_start())
}

/// An IANA time zone name (`400 invalid-schedule` for an unknown one).
pub fn parse_timezone(s: &str) -> Result<Tz> {
    s.parse::<Tz>()
        .map_err(|_| BackupError::new(Code::InvalidSchedule, format!("unknown time zone {s:?}")))
}

/// A retention, grace or schedule duration: one or more `<n><unit>` terms, spaces
/// allowed between them (`30d`, `12h`, `1w`, `90m`, `1d 12h`). Units: `s`, `m`, `h`,
/// `d`, `w`, also spelled `sec`, `min`, `hr`, `hour`, `day`, `week` (and plurals).
/// `400 invalid-config` otherwise, and for a zero duration.
pub fn parse_duration(s: &str) -> Result<Duration> {
    let bad = || {
        BackupError::new(
            Code::InvalidConfig,
            format!("cannot read the duration {s:?}"),
        )
    };
    let src = s.trim().to_ascii_lowercase();
    let mut rest = src.as_str();
    if rest.is_empty() {
        return Err(bad());
    }
    let mut total: u64 = 0;
    while !rest.is_empty() {
        let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
        if digits == 0 {
            return Err(bad());
        }
        let n: u64 = rest[..digits].parse().map_err(|_| bad())?;
        rest = rest[digits..].trim_start();
        let letters = rest.bytes().take_while(u8::is_ascii_alphabetic).count();
        let unit: u64 = match &rest[..letters] {
            "s" | "sec" | "secs" | "second" | "seconds" => 1,
            "m" | "min" | "mins" | "minute" | "minutes" => 60,
            "h" | "hr" | "hrs" | "hour" | "hours" => 3600,
            "d" | "day" | "days" => 86_400,
            "w" | "week" | "weeks" => 604_800,
            _ => return Err(bad()),
        };
        total = n
            .checked_mul(unit)
            .and_then(|x| total.checked_add(x))
            .ok_or_else(bad)?;
        rest = rest[letters..].trim_start();
    }
    if total == 0 {
        return Err(BackupError::new(
            Code::InvalidConfig,
            format!("the duration {s:?} is zero"),
        ));
    }
    Ok(Duration::from_secs(total))
}

/// The instant a wall clock time `local` of `tz` stands for: itself when it exists
/// once, its first occurrence when the clock goes back over it, and the first instant
/// after the gap when the clock skips it (transitions fall on whole minutes).
fn resolve(tz: Tz, local: NaiveDateTime) -> Option<DateTime<Utc>> {
    let pick = |r: LocalResult<DateTime<Tz>>| match r {
        LocalResult::Single(t) => Some(t.to_utc()),
        LocalResult::Ambiguous(a, b) => Some(a.min(b).to_utc()),
        LocalResult::None => None,
    };
    if let Some(t) = pick(tz.from_local_datetime(&local)) {
        return Some(t);
    }
    let mut m = local.with_second(0)?.with_nanosecond(0)?;
    for _ in 0..48 * 60 {
        m += TimeDelta::minutes(1);
        if let Some(t) = pick(tz.from_local_datetime(&m)) {
            return Some(t);
        }
    }
    None
}

/// The first instant of `s` strictly after `after`.
fn next_after(s: &Schedule, tz: Tz, after: DateTime<Utc>) -> Option<DateTime<Utc>> {
    match &s.kind {
        Kind::Every(step) => {
            let t = after.timestamp().div_euclid(*step).checked_add(1)? * step;
            DateTime::from_timestamp(t, 0)
        }
        Kind::Cron(cron) => {
            // wall clock matches after `after`'s own wall clock time; those that map
            // to `after` or earlier (the repeated hour, or a gap already resolved to
            // `after`) ran already
            let mut cursor = after.with_timezone(&tz).naive_local();
            for _ in 0..MAX_STEPS {
                let cand = cron.find_next_occurrence(&cursor, false).ok()?;
                let t = resolve(tz, cand)?;
                if t > after {
                    return Some(t);
                }
                cursor = cand;
            }
            None
        }
    }
}

/// The last instant of `s` at or before `now`.
fn last_at_or_before(s: &Schedule, tz: Tz, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
    match &s.kind {
        Kind::Every(step) => DateTime::from_timestamp(now.timestamp().div_euclid(*step) * step, 0),
        Kind::Cron(cron) => {
            let mut cursor = now.with_timezone(&tz).naive_local();
            // in the second pass of a repeated hour, wall clock times later than now's
            // ran already in the first pass: start past the repeated range
            if matches!(tz.from_local_datetime(&cursor), LocalResult::Ambiguous(..)) {
                cursor += TimeDelta::hours(3);
            }
            let mut inclusive = true;
            for _ in 0..MAX_STEPS {
                let cand = cron.find_previous_occurrence(&cursor, inclusive).ok()?;
                let t = resolve(tz, cand)?;
                if t <= now {
                    return Some(t);
                }
                cursor = cand;
                inclusive = false;
            }
            None
        }
    }
}

/// The next `n` instants of `s` strictly after `after`.
pub fn next_runs(s: &Schedule, tz: Tz, after: DateTime<Utc>, n: usize) -> Vec<DateTime<Utc>> {
    let mut out = Vec::with_capacity(n);
    let mut at = after;
    while out.len() < n {
        match next_after(s, tz, at) {
            Some(t) => {
                out.push(t);
                at = t;
            }
            None => break,
        }
    }
    out
}

/// The most recent instant of `s` at or before `now` that is later than `since` (the
/// persisted `lastScheduledFor`; `None`: never ran, and then the most recent instant
/// counts — the server records one when it first sees a policy, so a new policy waits
/// for its next instant): the instant a (catch-up) run is due for. `None` when nothing
/// is due. Monotonic in `since`, so a backward clock jump never re-runs an instant.
pub fn latest_due(
    s: &Schedule,
    tz: Tz,
    now: DateTime<Utc>,
    since: Option<DateTime<Utc>>,
) -> Option<DateTime<Utc>> {
    let t = last_at_or_before(s, tz, now)?;
    match since {
        Some(since) if t <= since => None,
        _ => Some(t),
    }
}

/// Whether a run for `due` also covers earlier instants missed since `since` (then it
/// is a catch-up run; otherwise `due` is simply the instant after `since`).
pub fn missed_before(s: &Schedule, tz: Tz, since: DateTime<Utc>, due: DateTime<Utc>) -> bool {
    next_after(s, tz, since).is_some_and(|t| t < due)
}

const DAY_NAMES: [&str; 7] = [
    "Sunday",
    "Monday",
    "Tuesday",
    "Wednesday",
    "Thursday",
    "Friday",
    "Saturday",
];

/// A human description (`Every day at 02:30 (Europe/Berlin)`, `Every 6 hours, counted
/// from 00:00 UTC`); uncommon cron patterns get croner's English description.
pub fn describe(s: &Schedule, tz: Tz) -> String {
    let cron = match &s.kind {
        Kind::Every(secs) => {
            let secs = *secs;
            let (n, unit) = if secs % 86_400 == 0 {
                (secs / 86_400, "day")
            } else if secs % 3600 == 0 {
                (secs / 3600, "hour")
            } else if secs % 60 == 0 {
                (secs / 60, "minute")
            } else {
                (secs, "second")
            };
            return if n == 1 {
                format!("Every {unit}, counted from 00:00 UTC")
            } else {
                format!("Every {n} {unit}s, counted from 00:00 UTC")
            };
        }
        Kind::Cron(c) => c,
    };
    let f: Vec<&str> = s.source.split_whitespace().collect();
    let (sec, rest) = if f.len() == 6 {
        (f[0], &f[1..])
    } else {
        ("0", &f[..])
    };
    let num = |x: &str| !x.is_empty() && x.len() <= 2 && x.bytes().all(|c| c.is_ascii_digit());
    if let [min, hour, dom, mon, dow] = rest
        && sec == "0"
        && *dom == "*"
        && *mon == "*"
    {
        let dow = dow.to_ascii_uppercase();
        if num(min) && *hour == "*" && dow == "*" {
            return format!("Every hour at minute {}", min.parse::<u32>().unwrap_or(0));
        }
        if let Some(n) = min.strip_prefix("*/")
            && num(n)
            && *hour == "*"
            && dow == "*"
        {
            return format!("Every {} minutes", n.parse::<u32>().unwrap_or(0));
        }
        if num(min) && num(hour) {
            let (h, m) = (
                hour.parse::<u32>().unwrap_or(0),
                min.parse::<u32>().unwrap_or(0),
            );
            let hm = format!("{h:02}:{m:02}");
            if dow == "*" {
                return format!("Every day at {hm} ({tz})");
            }
            if num(&dow) {
                let d = dow.parse::<usize>().unwrap_or(0) % 7;
                return format!("Every {} at {hm} ({tz})", DAY_NAMES[d]);
            }
            if dow == "1-5" || dow == "MON-FRI" {
                return format!("At {hm} on weekdays ({tz})");
            }
        }
    }
    let d = cron.describe();
    format!("{} ({tz})", d.trim_end_matches('.'))
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

fn bad_template(msg: impl std::fmt::Display) -> BackupError {
    BackupError::new(Code::InvalidConfig, format!("name template: {msg}"))
}

/// Render `template` with `dataset` in place of `{dataset}`.
fn render_with(template: &str, ctx: &NameCtx, dataset: &str) -> Result<String> {
    let mut out = String::new();
    let mut rest = template;
    while let Some(i) = rest.find(['{', '}']) {
        out.push_str(&rest[..i]);
        if rest.as_bytes()[i] == b'}' {
            return Err(bad_template("a '}' without its '{'"));
        }
        let Some(len) = rest[i + 1..].find('}') else {
            return Err(bad_template("a '{' without its '}'"));
        };
        let name = &rest[i + 1..i + 1 + len];
        match name {
            "policy" => out.push_str(ctx.policy),
            "dataset" => out.push_str(dataset),
            "seq" => out.push_str(&ctx.seq.to_string()),
            "run" => out.push_str(&ctx.run.simple().to_string()[..8]),
            "time" => out.push_str(&time_tag(ctx.time)),
            _ => match name.strip_prefix("date:") {
                Some(fmt) => render_date(fmt, ctx, &mut out)?,
                None if name.contains('{') => {
                    return Err(bad_template("a '{' inside a placeholder"));
                }
                None => return Err(bad_template(format!("unknown placeholder {{{name}}}"))),
            },
        }
        rest = &rest[i + 2 + len..];
    }
    out.push_str(rest);
    Ok(out)
}

/// `{date:FMT}`: `%Y %m %d %H %M %S %j %V` in the policy's zone, other characters as
/// they are.
fn render_date(fmt: &str, ctx: &NameCtx, out: &mut String) -> Result<()> {
    let local = ctx.time.with_timezone(&ctx.tz);
    let mut chars = fmt.chars();
    while let Some(c) = chars.next() {
        if c != '%' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some(d @ ('Y' | 'm' | 'd' | 'H' | 'M' | 'S' | 'j' | 'V')) => {
                out.push_str(&local.format(&format!("%{d}")).to_string())
            }
            Some(d) => {
                return Err(bad_template(format!(
                    "unknown date directive %{d} (use %Y %m %d %H %M %S %j %V)"
                )));
            }
            None => return Err(bad_template("a '%' at the end of a date format")),
        }
    }
    Ok(())
}

/// Render a name template (`{policy}`, `{dataset}`, `{seq}`, `{run}`, `{time}`,
/// `{date:FMT}`). The result must be a valid backup name (`400 invalid-config`
/// otherwise, as are unknown placeholders and format directives). Characters of the
/// dataset name outside the backup-name grammar become `-`, and a dataset name that
/// makes the result longer than 64 characters is shortened to fit.
pub fn render_name(template: &str, ctx: &NameCtx) -> Result<String> {
    let dataset: String = ctx
        .dataset
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                c
            } else {
                '-'
            }
        })
        .collect();
    let mut out = render_with(template, ctx, &dataset)?;
    let uses = template.matches("{dataset}").count();
    if out.len() > 64 && uses > 0 {
        let cut = (out.len() - 64).div_ceil(uses);
        if cut < dataset.len() {
            out = render_with(template, ctx, &dataset[..dataset.len() - cut])?;
        }
    }
    if !valid_backup_name(&out) {
        return Err(bad_template(format!(
            "it renders {out:?}, which is not a valid backup name \
             (1 to 64 of A-Z, a-z, 0-9, '.', '_', '-', starting with a letter or digit)"
        )));
    }
    Ok(out)
}

/// Check a policy's settings (the repository's existence is the server's business):
/// the name (the repository-name grammar, and not `preview`), the schedule and time
/// zone (`invalid-schedule`), `expireAfter`, `maxCount ≥ 1`, the dataset patterns, and
/// the name template rendered with a sample (`invalid-config`). Returns the parsed
/// schedule and zone.
pub fn check_policy(p: &PolicyConfig) -> Result<(Schedule, Tz)> {
    if !valid_repo_name(&p.name) || p.name == "preview" {
        return Err(BackupError::new(
            Code::InvalidName,
            format!(
                "invalid policy name {:?}: 1 to 64 of a-z, 0-9, '_' and '-', starting with a letter or digit, and not \"preview\"",
                p.name
            ),
        ));
    }
    let tz = parse_timezone(&p.timezone)?;
    let schedule = parse_schedule(&p.schedule)?;
    if let Some(e) = &p.retention.expire_after {
        parse_duration(e).map_err(|e| {
            BackupError::new(Code::InvalidConfig, format!("expireAfter: {}", e.message()))
        })?;
    }
    if p.retention.max_count == Some(0) {
        return Err(BackupError::new(
            Code::InvalidConfig,
            "maxCount must be at least 1 (or null)",
        ));
    }
    for d in &p.datasets {
        let ok = !d.is_empty()
            && d.len() <= 64
            && d.chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | '*'));
        if !ok {
            return Err(BackupError::new(
                Code::InvalidConfig,
                format!("invalid dataset name or pattern {d:?} (names, or '*' globs)"),
            ));
        }
    }
    render_name(
        &p.name_template,
        &NameCtx {
            policy: &p.name,
            dataset: "dataset",
            seq: 42,
            run: uuid::Uuid::from_u128(0x0123_4567_89ab_4def_8123_4567_89ab_cdef),
            time: DateTime::from_timestamp(1_790_000_000, 0).expect("a valid instant"),
            tz,
        },
    )?;
    Ok((schedule, tz))
}

/// Why retention keeps or deletes a backup.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RetentionReason {
    /// one of the newest `minCount`
    MinCount,
    /// within `maxCount` and younger than `expireAfter`
    Retained,
    /// would be deleted, but a restore or verify uses it (until the next evaluation)
    Busy,
    /// at an index ≥ `maxCount`
    MaxCount,
    /// completed longer than `expireAfter` ago
    Expired,
}

impl RetentionReason {
    pub fn deletes(self) -> bool {
        matches!(self, RetentionReason::MaxCount | RetentionReason::Expired)
    }

    /// A short explanation for people.
    pub fn describe(self) -> &'static str {
        match self {
            RetentionReason::MinCount => "kept: one of the newest minCount",
            RetentionReason::Retained => "kept: within maxCount and expireAfter",
            RetentionReason::Busy => "kept: a restore or verify uses it",
            RetentionReason::MaxCount => "deleted: beyond maxCount",
            RetentionReason::Expired => "deleted: older than expireAfter",
        }
    }
}

/// What retention does to a policy's backups.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RetentionPlan {
    /// newest `completed` first
    pub delete: Vec<BackupSummary>,
    /// newest `completed` first
    pub keep: Vec<BackupSummary>,
    /// why, by backup name
    pub reasons: BTreeMap<String, RetentionReason>,
}

fn completed_at(b: &BackupSummary) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(&b.completed)
        .ok()
        .map(|t| t.to_utc())
}

fn newest_first(a: &BackupSummary, b: &BackupSummary) -> std::cmp::Ordering {
    completed_at(b)
        .cmp(&completed_at(a))
        .then_with(|| b.completed.cmp(&a.completed))
        .then_with(|| b.name.cmp(&a.name))
}

/// Retention of policy `policy`: only backups whose `policy` is `policy` are
/// considered (others are in neither list). Per dataset id, newest `completed` first:
/// the first `minCount` are kept; of the rest, those at index ≥ `maxCount` or with
/// `completed + expireAfter < now` are deleted, except names in `busy` (a restore or
/// verify uses them), which are kept until the next evaluation. An `expireAfter` that
/// does not parse sets no age limit (policies are checked when saved).
pub fn retention(
    backups: &[BackupSummary],
    policy: &str,
    r: &Retention,
    now: DateTime<Utc>,
    busy: &HashSet<String>,
) -> RetentionPlan {
    let expire = r
        .expire_after
        .as_deref()
        .and_then(|s| parse_duration(s).ok())
        .and_then(|d| TimeDelta::from_std(d).ok());
    let mut groups: BTreeMap<uuid::Uuid, Vec<&BackupSummary>> = BTreeMap::new();
    for b in backups
        .iter()
        .filter(|b| b.policy.as_deref() == Some(policy))
    {
        groups.entry(b.dataset.id).or_default().push(b);
    }
    let mut plan = RetentionPlan::default();
    for group in groups.values_mut() {
        group.sort_by(|a, b| newest_first(a, b));
        for (i, b) in group.iter().enumerate() {
            let expired = expire.is_some_and(|e| {
                completed_at(b).is_some_and(|c| c.checked_add_signed(e).is_some_and(|x| x < now))
            });
            let mut why = if i < r.min_count as usize {
                RetentionReason::MinCount
            } else if r.max_count.is_some_and(|m| i >= m as usize) {
                RetentionReason::MaxCount
            } else if expired {
                RetentionReason::Expired
            } else {
                RetentionReason::Retained
            };
            if why.deletes() && busy.contains(&b.name) {
                why = RetentionReason::Busy;
            }
            if why.deletes() {
                plan.delete.push((*b).clone());
            } else {
                plan.keep.push((*b).clone());
            }
            plan.reasons.insert(b.name.clone(), why);
        }
    }
    plan.delete.sort_by(newest_first);
    plan.keep.sort_by(newest_first);
    plan
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CommitRef, DatasetRef};

    fn t(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().to_utc()
    }

    fn iso(v: &[DateTime<Utc>]) -> Vec<String> {
        v.iter()
            .map(|t| t.format("%Y-%m-%dT%H:%M:%SZ").to_string())
            .collect()
    }

    fn sched(s: &str) -> Schedule {
        parse_schedule(s).unwrap()
    }

    #[test]
    fn time_zones() {
        assert_eq!(parse_timezone("Europe/Berlin").unwrap(), Tz::Europe__Berlin);
        assert_eq!(parse_timezone("UTC").unwrap(), Tz::UTC);
        assert_eq!(
            parse_timezone("Mars/Base").unwrap_err().code(),
            Code::InvalidSchedule
        );
    }

    #[test]
    fn durations() {
        let d = |s: &str| parse_duration(s).unwrap().as_secs();
        assert_eq!(d("30d"), 30 * 86_400);
        assert_eq!(d("12h"), 12 * 3600);
        assert_eq!(d("1w"), 7 * 86_400);
        assert_eq!(d("90m"), 90 * 60);
        assert_eq!(d("1d 12h"), 36 * 3600);
        assert_eq!(d("1d12h"), 36 * 3600);
        assert_eq!(d(" 2 days "), 2 * 86_400);
        assert_eq!(d("45s"), 45);
        assert_eq!(d("1H 30Min"), 5400);
        for bad in ["", "d", "30", "30x", "1.5h", "-1d", "0d", "1d -", "h1"] {
            let e = parse_duration(bad).unwrap_err();
            assert_eq!(e.code(), Code::InvalidConfig, "{bad:?}");
        }
        assert!(parse_duration("99999999999999999999w").is_err());
    }

    #[test]
    fn schedules_parse() {
        assert!(parse_schedule("30 2 * * *").is_ok());
        assert!(parse_schedule("0 30 2 * * *").is_ok());
        assert!(parse_schedule("*/10 * * * *").is_ok());
        assert!(parse_schedule("0 3 * * sun").is_ok());
        assert!(parse_schedule("0 9 * * MON-FRI").is_ok());
        assert!(parse_schedule("every 6h").is_ok());
        assert!(parse_schedule("Every 1d 12h").is_ok());
        assert_eq!(
            sched("every 90m").interval(),
            Some(Duration::from_secs(5400))
        );
        assert_eq!(sched(" 30 2 * * * ").source(), "30 2 * * *");
        for bad in [
            "",
            "* * * *",
            "0 0 0 * * * 2027",
            "@daily",
            "61 * * * *",
            "* 25 * * *",
            "0 0 31 2 *",
            "every",
            "every 30s",
            "every 0m",
            "every soon",
            "everyday",
            "nonsense words go here ok",
        ] {
            let e = parse_schedule(bad).unwrap_err();
            assert_eq!(e.code(), Code::InvalidSchedule, "{bad:?}");
        }
    }

    // `*/10` with the clock at 12:05 → 12:10, then 12:20
    #[test]
    fn every_ten_minutes() {
        let s = sched("*/10 * * * *");
        let now = t("2026-09-30T12:05:00Z");
        assert_eq!(
            iso(&next_runs(&s, Tz::UTC, now, 2)),
            ["2026-09-30T12:10:00Z", "2026-09-30T12:20:00Z"]
        );
        // strictly after: an instant is not its own successor
        assert_eq!(
            iso(&next_runs(&s, Tz::UTC, t("2026-09-30T12:10:00Z"), 1)),
            ["2026-09-30T12:20:00Z"]
        );
        assert_eq!(
            iso(&next_runs(&s, Tz::UTC, t("2026-09-30T12:09:59.999Z"), 1)),
            ["2026-09-30T12:10:00Z"]
        );
        // 12:12, after the 12:10 run: nothing new is due, and the next is 12:20
        let at = t("2026-09-30T12:12:00Z");
        assert_eq!(
            latest_due(&s, Tz::UTC, at, Some(t("2026-09-30T12:10:00Z"))),
            None
        );
        assert_eq!(
            latest_due(&s, Tz::UTC, at, Some(t("2026-09-30T12:00:00Z"))),
            Some(t("2026-09-30T12:10:00Z"))
        );
    }

    // `30 2 * * *` in Europe/Berlin from 2026-10-24
    #[test]
    fn berlin_fall_back_runs_once() {
        let s = sched("30 2 * * *");
        let tz = Tz::Europe__Berlin;
        assert_eq!(
            iso(&next_runs(&s, tz, t("2026-10-24T00:00:00Z"), 3)),
            [
                "2026-10-24T00:30:00Z", // 02:30 CEST
                "2026-10-25T00:30:00Z", // 02:30 CEST, the first of the two 02:30s
                "2026-10-26T01:30:00Z", // 02:30 CET
            ]
        );
        // from inside the repeated hour (02:10 CET, the second pass): 02:30 ran already
        assert_eq!(
            iso(&next_runs(&s, tz, t("2026-10-25T01:10:00Z"), 1)),
            ["2026-10-26T01:30:00Z"]
        );
        // from the first pass, before 02:30 CEST
        assert_eq!(
            iso(&next_runs(&s, tz, t("2026-10-25T00:10:00Z"), 1)),
            ["2026-10-25T00:30:00Z"]
        );
    }

    // 2027-03-28, 02:30 does not exist in Berlin → 03:00 CEST
    #[test]
    fn berlin_spring_forward_runs_after_the_gap() {
        let s = sched("30 2 * * *");
        let tz = Tz::Europe__Berlin;
        assert_eq!(
            iso(&next_runs(&s, tz, t("2027-03-27T00:00:00Z"), 3)),
            [
                "2027-03-27T01:30:00Z", // 02:30 CET
                "2027-03-28T01:00:00Z", // 03:00 CEST, the first instant after the gap
                "2027-03-29T00:30:00Z", // 02:30 CEST
            ]
        );
        // every 10 minutes: the gap's six times collapse into one run at 03:00 CEST
        let s = sched("*/10 * * * *");
        assert_eq!(
            iso(&next_runs(&s, tz, t("2027-03-28T00:45:00Z"), 4)),
            [
                "2027-03-28T00:50:00Z", // 01:50 CET
                "2027-03-28T01:00:00Z", // 03:00 CEST (02:00 … 02:50 and 03:00)
                "2027-03-28T01:10:00Z",
                "2027-03-28T01:20:00Z",
            ]
        );
        assert_eq!(
            latest_due(&s, tz, t("2027-03-28T01:05:00Z"), None),
            Some(t("2027-03-28T01:00:00Z"))
        );
    }

    #[test]
    fn berlin_repeated_hour_every_ten_minutes() {
        let s = sched("*/10 * * * *");
        let tz = Tz::Europe__Berlin;
        // 02:50 CEST, then the repeated 02:00 … 02:50 CET are skipped, then 03:00 CET
        assert_eq!(
            iso(&next_runs(&s, tz, t("2026-10-25T00:45:00Z"), 3)),
            [
                "2026-10-25T00:50:00Z",
                "2026-10-25T02:00:00Z",
                "2026-10-25T02:10:00Z",
            ]
        );
        // in the second pass, the latest instant is the first pass's 02:50 CEST
        assert_eq!(
            latest_due(&s, tz, t("2026-10-25T01:35:00Z"), None),
            Some(t("2026-10-25T00:50:00Z"))
        );
        // a daily 02:30 seen from the second pass: that day's 02:30 CEST
        let daily = sched("30 2 * * *");
        assert_eq!(
            latest_due(&daily, tz, t("2026-10-25T01:15:00Z"), None),
            Some(t("2026-10-25T00:30:00Z"))
        );
        assert_eq!(
            latest_due(
                &daily,
                tz,
                t("2026-10-25T01:15:00Z"),
                Some(t("2026-10-25T00:30:00Z"))
            ),
            None
        );
    }

    // America/New_York falls back on 2026-11-01 at 02:00 EDT → 01:00 EST
    #[test]
    fn new_york_fall_back() {
        let tz = Tz::America__New_York;
        let s = sched("30 1 * * *");
        assert_eq!(
            iso(&next_runs(&s, tz, t("2026-10-31T00:00:00Z"), 3)),
            [
                "2026-10-31T05:30:00Z", // 01:30 EDT
                "2026-11-01T05:30:00Z", // 01:30 EDT, the first of the two
                "2026-11-02T06:30:00Z", // 01:30 EST
            ]
        );
        // hourly: 01:00 EDT runs, 01:00 EST does not, 02:00 EST does
        let s = sched("0 * * * *");
        assert_eq!(
            iso(&next_runs(&s, tz, t("2026-11-01T04:30:00Z"), 3)),
            [
                "2026-11-01T05:00:00Z", // 01:00 EDT
                "2026-11-01T07:00:00Z", // 02:00 EST
                "2026-11-01T08:00:00Z",
            ]
        );
        // daily at 02:30 stays at 02:30 local across the change
        let s = sched("30 2 * * *");
        assert_eq!(
            iso(&next_runs(&s, tz, t("2026-10-31T12:00:00Z"), 2)),
            ["2026-11-01T07:30:00Z", "2026-11-02T07:30:00Z"]
        );
    }

    // America/New_York springs forward on 2027-03-14 at 02:00 EST → 03:00 EDT
    #[test]
    fn new_york_spring_forward() {
        let tz = Tz::America__New_York;
        let s = sched("30 2 * * *");
        assert_eq!(
            iso(&next_runs(&s, tz, t("2027-03-13T12:00:00Z"), 3)),
            [
                "2027-03-14T07:00:00Z", // 03:00 EDT, the first instant after the gap
                "2027-03-15T06:30:00Z", // 02:30 EDT
                "2027-03-16T06:30:00Z",
            ]
        );
    }

    // `every 6h` → 00:00Z, 06:00Z, … whatever the start and zone
    #[test]
    fn every_six_hours_counts_from_the_epoch() {
        let s = sched("every 6h");
        for tz in [Tz::UTC, Tz::Europe__Berlin, Tz::Asia__Kolkata] {
            assert_eq!(
                iso(&next_runs(&s, tz, t("2026-10-24T13:47:12Z"), 4)),
                [
                    "2026-10-24T18:00:00Z",
                    "2026-10-25T00:00:00Z",
                    "2026-10-25T06:00:00Z",
                    "2026-10-25T12:00:00Z",
                ]
            );
        }
        assert_eq!(
            iso(&next_runs(&s, Tz::UTC, t("2026-10-25T06:00:00Z"), 1)),
            ["2026-10-25T12:00:00Z"]
        );
        assert_eq!(
            latest_due(&s, Tz::UTC, t("2026-10-25T06:00:00Z"), None),
            Some(t("2026-10-25T06:00:00Z"))
        );
        assert_eq!(
            latest_due(&s, Tz::UTC, t("2026-10-25T05:59:59Z"), None),
            Some(t("2026-10-25T00:00:00Z"))
        );
    }

    // an hourly policy, last run 09:00, back at 12:40
    #[test]
    fn missed_runs() {
        let s = sched("0 * * * *");
        let last = t("2026-09-30T09:00:00Z");
        let since = Some(last);
        let now = t("2026-09-30T12:41:00Z");
        let due = latest_due(&s, Tz::UTC, now, since).unwrap();
        assert_eq!(due, t("2026-09-30T12:00:00Z"));
        assert!(missed_before(&s, Tz::UTC, last, due));
        assert_eq!(
            iso(&next_runs(&s, Tz::UTC, now, 1)),
            ["2026-09-30T13:00:00Z"]
        );
        // the instant right after the last one is a plain scheduled run
        let due = latest_due(&s, Tz::UTC, t("2026-09-30T10:00:30Z"), since).unwrap();
        assert!(!missed_before(&s, Tz::UTC, last, due));
        // a clock that went back never re-runs an instant
        assert_eq!(
            latest_due(&s, Tz::UTC, t("2026-09-30T08:30:00Z"), since),
            None
        );
    }

    #[test]
    fn six_field_cron_has_seconds() {
        let s = sched("15 30 2 * * *");
        assert_eq!(
            iso(&next_runs(&s, Tz::UTC, t("2026-09-30T00:00:00Z"), 1)),
            ["2026-09-30T02:30:15Z"]
        );
        // in a gap, the seconds do not matter: the first instant after it
        assert_eq!(
            iso(&next_runs(
                &s,
                Tz::Europe__Berlin,
                t("2027-03-28T00:00:00Z"),
                1
            )),
            ["2027-03-28T01:00:00Z"]
        );
    }

    #[test]
    fn day_of_month_or_day_of_week() {
        // Vixie cron: both restricted → either matches
        let s = sched("0 0 1 * MON");
        assert_eq!(
            iso(&next_runs(&s, Tz::UTC, t("2026-09-30T12:00:00Z"), 3)),
            [
                "2026-10-01T00:00:00Z",
                "2026-10-05T00:00:00Z",
                "2026-10-12T00:00:00Z"
            ]
        );
    }

    #[test]
    fn descriptions() {
        let d = |s: &str, tz: Tz| describe(&sched(s), tz);
        assert_eq!(
            d("30 2 * * *", Tz::Europe__Berlin),
            "Every day at 02:30 (Europe/Berlin)"
        );
        assert_eq!(d("0 3 * * 0", Tz::UTC), "Every Sunday at 03:00 (UTC)");
        assert_eq!(d("0 9 * * 1-5", Tz::UTC), "At 09:00 on weekdays (UTC)");
        assert_eq!(d("5 * * * *", Tz::UTC), "Every hour at minute 5");
        assert_eq!(d("*/10 * * * *", Tz::UTC), "Every 10 minutes");
        assert_eq!(d("0 30 2 * * *", Tz::UTC), "Every day at 02:30 (UTC)");
        assert_eq!(
            d("every 6h", Tz::UTC),
            "Every 6 hours, counted from 00:00 UTC"
        );
        assert_eq!(d("every 1d", Tz::UTC), "Every day, counted from 00:00 UTC");
        assert_eq!(
            d("every 90m", Tz::UTC),
            "Every 90 minutes, counted from 00:00 UTC"
        );
        let other = d("0 12 1 1 *", Tz::Europe__Berlin);
        assert!(other.ends_with(" (Europe/Berlin)"), "{other}");
        assert!(other.contains("12:00"), "{other}");
    }

    fn ctx(time: &str) -> NameCtx<'static> {
        NameCtx {
            policy: "nightly",
            dataset: "ds",
            seq: 42,
            run: uuid::Uuid::parse_str("0b6e5c1a-0000-4000-8000-000000000001").unwrap(),
            time: t(time),
            tz: Tz::Europe__Berlin,
        }
    }

    #[test]
    fn name_templates() {
        let c = ctx("2026-09-30T12:10:00Z");
        let r = |tpl: &str| render_name(tpl, &c);
        assert_eq!(
            r("{policy}-{dataset}-{time}").unwrap(),
            "nightly-ds-20260930t121000z"
        );
        // {date:…} is in the policy's zone (14:10 CEST)
        assert_eq!(
            r("{policy}-{dataset}-{date:%Y%m%d-%H%M%S}").unwrap(),
            "nightly-ds-20260930-141000"
        );
        assert_eq!(r("{dataset}.{date:%Y-w%V.%j}").unwrap(), "ds.2026-w40.273");
        assert_eq!(r("{dataset}-{seq}-{run}").unwrap(), "ds-42-0b6e5c1a");
        // an instant late in the UTC day is the next day in Berlin
        let late = ctx("2026-09-30T23:30:00Z");
        assert_eq!(
            render_name("{policy}-{date:%Y%m%d}", &late).unwrap(),
            "nightly-20261001"
        );
        for (bad, why) in [
            ("{policy}-{nope}", "unknown placeholder"),
            ("{policy}-{date:%A}", "unknown date directive"),
            ("{policy}-{date:%}", "end of a date format"),
            ("{policy", "without its '}'"),
            ("policy}", "without its '{'"),
            ("{policy}/{dataset}", "not a valid backup name"),
            ("-{policy}", "not a valid backup name"),
            ("{policy} {dataset}", "not a valid backup name"),
            ("", "not a valid backup name"),
        ] {
            let e = r(bad).unwrap_err();
            assert_eq!(e.code(), Code::InvalidConfig, "{bad:?}");
            assert!(e.message().contains(why), "{bad:?}: {}", e.message());
        }
    }

    #[test]
    fn long_dataset_names_are_shortened() {
        let long = "d".repeat(64);
        let c = NameCtx {
            dataset: &long,
            ..ctx("2026-09-30T12:10:00Z")
        };
        let n = render_name("{policy}-{dataset}-{time}", &c).unwrap();
        assert_eq!(n.len(), 64);
        assert!(n.starts_with("nightly-ddd") && n.ends_with("-20260930t121000z"));
        // characters outside the grammar become '-'
        let c = NameCtx {
            dataset: "a b",
            ..ctx("2026-09-30T12:10:00Z")
        };
        assert_eq!(render_name("{dataset}", &c).unwrap(), "a-b");
        // a long literal part cannot be fixed by shortening
        let tpl = format!("{}-{{dataset}}", "x".repeat(70));
        assert!(render_name(&tpl, &ctx("2026-09-30T12:10:00Z")).is_err());
    }

    fn policy(v: serde_json::Value) -> PolicyConfig {
        serde_json::from_value(v).unwrap()
    }

    #[test]
    fn policies_are_checked() {
        let ok = policy(serde_json::json!({
            "name": "nightly", "repository": "local", "schedule": "30 2 * * *",
            "timezone": "Europe/Berlin", "nameTemplate": "{policy}-{dataset}-{date:%Y%m%d}",
            "retention": {"expireAfter": "30d", "minCount": 7, "maxCount": 60}
        }));
        let (s, tz) = check_policy(&ok).unwrap();
        assert_eq!(s.source(), "30 2 * * *");
        assert_eq!(tz, Tz::Europe__Berlin);
        let code = |f: &dyn Fn(&mut PolicyConfig)| {
            let mut p = ok.clone();
            f(&mut p);
            check_policy(&p).unwrap_err().code()
        };
        assert_eq!(code(&|p| p.name = "preview".into()), Code::InvalidName);
        assert_eq!(code(&|p| p.name = "Nightly".into()), Code::InvalidName);
        assert_eq!(
            code(&|p| p.schedule = "whenever".into()),
            Code::InvalidSchedule
        );
        assert_eq!(
            code(&|p| p.timezone = "Mars/Base".into()),
            Code::InvalidSchedule
        );
        assert_eq!(
            code(&|p| p.retention.expire_after = Some("soon".into())),
            Code::InvalidConfig
        );
        assert_eq!(
            code(&|p| p.retention.max_count = Some(0)),
            Code::InvalidConfig
        );
        assert_eq!(
            code(&|p| p.name_template = "{x}".into()),
            Code::InvalidConfig
        );
        assert_eq!(
            code(&|p| p.name_template = "{policy}:{dataset}".into()),
            Code::InvalidConfig
        );
        assert_eq!(
            code(&|p| p.datasets = vec!["a/b".into()]),
            Code::InvalidConfig
        );
        assert_eq!(code(&|p| p.datasets = vec!["".into()]), Code::InvalidConfig);
    }

    fn backup(
        name: &str,
        id: u128,
        policy: Option<&str>,
        completed: DateTime<Utc>,
    ) -> BackupSummary {
        let at = completed.to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
        BackupSummary {
            name: name.into(),
            repository: "local".into(),
            dataset: DatasetRef {
                name: "ds".into(),
                id: uuid::Uuid::from_u128(id),
                kind: "persistent".into(),
            },
            commit: CommitRef {
                seq: 1,
                timestamp: at.clone(),
                quads: 1,
                reference: "commit:1".into(),
            },
            created: at.clone(),
            completed: at,
            millis: 1,
            logical_bytes: 1,
            added_bytes: 1,
            policy: policy.map(str::to_string),
            run: None,
            note: None,
            same_lineage: None,
            verified: None,
        }
    }

    fn names(v: &[BackupSummary]) -> Vec<&str> {
        v.iter().map(|b| b.name.as_str()).collect()
    }

    // retention by count and age, never touching other backups
    #[test]
    fn retention_by_count_and_age() {
        let now = t("2026-09-30T12:00:00Z");
        let ago = |days: f64| now - TimeDelta::seconds((days * 86_400.0) as i64);
        let list = vec![
            backup("d3", 1, Some("p"), ago(3.0)),
            backup("d0.5", 1, Some("p"), ago(0.5)),
            backup("d5", 1, Some("p"), ago(5.0)),
            backup("manual", 1, None, ago(10.0)),
            backup("d1", 1, Some("p"), ago(1.0)),
            backup("d4", 1, Some("p"), ago(4.0)),
            backup("other", 1, Some("q"), ago(10.0)),
        ];
        let r = Retention {
            expire_after: Some("2d".into()),
            min_count: 2,
            max_count: Some(3),
        };
        let plan = retention(&list, "p", &r, now, &HashSet::new());
        assert_eq!(names(&plan.delete), ["d3", "d4", "d5"]);
        assert_eq!(names(&plan.keep), ["d0.5", "d1"]);
        assert_eq!(plan.reasons["d3"], RetentionReason::Expired);
        assert_eq!(plan.reasons["d4"], RetentionReason::MaxCount);
        assert_eq!(plan.reasons["d1"], RetentionReason::MinCount);
        assert!(!plan.reasons.contains_key("manual") && !plan.reasons.contains_key("other"));
        // a busy backup is kept until the next evaluation
        let busy: HashSet<String> = ["d4".to_string()].into();
        let plan = retention(&list, "p", &r, now, &busy);
        assert_eq!(names(&plan.delete), ["d3", "d5"]);
        assert_eq!(names(&plan.keep), ["d0.5", "d1", "d4"]);
        assert_eq!(plan.reasons["d4"], RetentionReason::Busy);
        assert!(RetentionReason::Busy.describe().starts_with("kept"));
    }

    #[test]
    fn retention_is_per_dataset_id() {
        let now = t("2026-09-30T12:00:00Z");
        let ago = |h: i64| now - TimeDelta::hours(h);
        let list = vec![
            backup("a1", 1, Some("p"), ago(1)),
            backup("a2", 1, Some("p"), ago(2)),
            backup("a3", 1, Some("p"), ago(3)),
            backup("b1", 2, Some("p"), ago(10)),
            backup("b2", 2, Some("p"), ago(20)),
        ];
        let r = Retention {
            expire_after: None,
            min_count: 1,
            max_count: Some(2),
        };
        let plan = retention(&list, "p", &r, now, &HashSet::new());
        assert_eq!(names(&plan.delete), ["a3"]);
        assert_eq!(names(&plan.keep), ["a1", "a2", "b1", "b2"]);
        // the defaults (minCount 1, no limits) keep everything
        let plan = retention(&list, "p", &Retention::default(), now, &HashSet::new());
        assert!(plan.delete.is_empty() && plan.keep.len() == 5);
        // minCount 0 with an age limit can delete a dataset's every backup
        let r = Retention {
            expire_after: Some("5h".into()),
            min_count: 0,
            max_count: None,
        };
        let plan = retention(&list, "p", &r, now, &HashSet::new());
        assert_eq!(names(&plan.delete), ["b1", "b2"]);
    }
}
