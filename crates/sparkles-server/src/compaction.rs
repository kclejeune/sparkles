//! Automatic compaction in the server (the C13 spec): the policy from `serve
//! --auto-compact-*` and each dataset's `compaction.json`, the scheduler that starts
//! compaction tasks when a dataset's delta, log, age or idle time asks for one, the
//! status at `/$/compaction/{ds}` and in `/$/stats`, and the metrics.
//!
//! Authorization (the route table in `auth/routes.rs`): `GET /$/compaction/{ds}` needs
//! `read` on `{ds}`, `PUT` and `DELETE` need `admin`.

use crate::http::{AdminBody, ApiResult, blocking, dataset, err};
use crate::state::{AppState, Dataset, Task};
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::get;
use axum::{Json, Router};
use parking_lot::Mutex;
use serde_json::{Value as J, json};
use sparkles::store::{
    CompactOptions, CompactReport, CompactionPolicy, CompactionSettings, PartialMode, Trigger,
};
use std::collections::{BTreeMap, HashMap};
use std::fmt::Write as _;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

type St = State<Arc<AppState>>;

/// The task kind of compactions, automatic or not.
pub const TASK: &str = "compact";

/// The first wait after a failed automatic compaction; it doubles up to [`MAX_BACKOFF`].
const FIRST_BACKOFF: Duration = Duration::from_secs(60);
const MAX_BACKOFF: Duration = Duration::from_secs(3600);

pub fn routes() -> Router<Arc<AppState>> {
    Router::new().route(
        "/$/compaction/{ds}",
        get(get_status).put(put_settings).delete(delete_settings),
    )
}

/// The server's side of automatic compaction: its switch, policy and build limits, and
/// what it knows of each dataset's compactions.
pub struct AutoCompact {
    /// `false` under `--no-auto-compact`
    pub enabled: bool,
    pub policy: CompactionPolicy,
    /// threads of an automatic compaction's build
    pub threads: usize,
    /// the average write rate of an automatic build, in bytes per second
    pub io_bytes_per_sec: Option<u64>,
    /// automatic compactions running at once on the server
    pub max_running: usize,
    states: Mutex<HashMap<String, DsState>>,
}

impl Default for AutoCompact {
    fn default() -> Self {
        AutoCompact {
            enabled: true,
            policy: CompactionPolicy::default(),
            threads: default_threads(),
            io_bytes_per_sec: None,
            max_running: 1,
            states: Default::default(),
        }
    }
}

/// A quarter of the cores, at least one.
pub fn default_threads() -> usize {
    std::thread::available_parallelism().map_or(1, |n| (n.get() / 4).max(1))
}

/// What the scheduler knows of one dataset.
#[derive(Default)]
struct DsState {
    /// the running compaction task, and whether the scheduler started it
    task: Option<(String, bool)>,
    /// when the last compaction ended
    ended: Option<Instant>,
    /// consecutive failed automatic compactions, and the wait before the next
    failures: u32,
    backoff_until: Option<Instant>,
    /// what the last pass of the scheduler saw
    seen: Seen,
    last: Option<J>,
    automatic_runs: u64,
    /// compactions by (mode, outcome)
    outcomes: BTreeMap<(&'static str, &'static str), u64>,
    seconds_sum: f64,
    lock_seconds_sum: f64,
    lock_seconds_max: f64,
    published: u64,
}

/// The state of a dataset for its status.
#[derive(Clone, Default)]
struct Seen {
    state: &'static str,
    trigger: Option<Trigger>,
    deferred: Option<(&'static str, String)>,
}

/// The decision of one pass for one dataset.
enum Decision {
    Off,
    Idle,
    Running,
    Deferred(Trigger, &'static str, String),
    Start(Trigger),
}

impl AutoCompact {
    /// The policy that applies to `ds`: the server's with the dataset's own settings.
    pub fn policy_for(&self, ds: &Dataset) -> CompactionPolicy {
        self.policy.with(&ds.dataset.settings().compaction().get())
    }

    fn running_auto(states: &HashMap<String, DsState>) -> usize {
        states
            .values()
            .filter(|s| s.task.as_ref().is_some_and(|t| t.1))
            .count()
    }
}

/// Whether a task is still queued or running.
fn task_active(st: &AppState, id: &str) -> bool {
    st.tasks.lock().iter().any(|t| t.id == id && t.active())
}

/// What the scheduler would do for `ds` now.
fn decide(st: &AppState, ds: &Dataset, s: &DsState, running_auto: usize, now: Instant) -> Decision {
    let ac = &st.compaction;
    let policy = ac.policy_for(ds);
    if st.read_only || !ac.enabled || !policy.enabled {
        return Decision::Off;
    }
    if s.task.is_some() {
        return Decision::Running;
    }
    let m = ds.dataset.settings().compaction().measures();
    let Some(trigger) = policy.verdict(&m) else {
        return Decision::Idle;
    };
    let defer = |reason, detail: String| Decision::Deferred(trigger.clone(), reason, detail);
    if let Some(until) = s.backoff_until
        && until > now
    {
        return defer(
            "backoff",
            format!(
                "the last automatic compaction failed; the next try is in {} s",
                (until - now).as_secs()
            ),
        );
    }
    let gap = Duration::from_secs(policy.min_interval_seconds);
    if let Some(ended) = s.ended
        && now.duration_since(ended) < gap
    {
        return defer(
            "min-interval",
            format!(
                "the last compaction ended {} s ago (minIntervalSeconds {})",
                now.duration_since(ended).as_secs(),
                policy.min_interval_seconds
            ),
        );
    }
    if let Some(t) = st.catalog.restoring_by(&ds.name) {
        return defer("restore", format!("task {t} is restoring the dataset"));
    }
    for (kind, reason) in [
        ("load", "bulk-load"),
        ("reason", "reasoning"),
        ("backup", "backup"),
        ("backup-create", "backup"),
        ("backup-restore", "restore"),
        ("clone", "clone"),
    ] {
        if let Some(id) = st.active_task(kind, &ds.key()) {
            return defer(reason, format!("a {kind} task ({id}) is running"));
        }
    }
    if let Some(b) = ds.store.compaction_blocker(true) {
        if b.reason == "running" {
            return Decision::Running;
        }
        return defer(b.reason, b.detail);
    }
    if running_auto >= ac.max_running.max(1) {
        return defer(
            "running-limit",
            format!(
                "{running_auto} automatic compactions are running (--auto-compact-max-running)"
            ),
        );
    }
    if !st.task_queue.has_free_slot() {
        return defer(
            "slots",
            "no task slot is free (--max-tasks); automatic compactions do not queue".into(),
        );
    }
    Decision::Start(trigger)
}

/// One pass of the scheduler: start the compactions that are due and may run.
pub fn tick(st: &Arc<AppState>, now: Instant) {
    let datasets: Vec<Arc<Dataset>> = st.datasets_and_branches();
    let mut start = Vec::new();
    {
        let mut states = st.compaction.states.lock();
        states.retain(|n, _| datasets.iter().any(|d| &d.key() == n));
        for ds in &datasets {
            let s = states.entry(ds.key()).or_default();
            if let Some((id, _)) = &s.task
                && !task_active(st, id)
            {
                s.task = None;
            }
        }
        let mut running = AutoCompact::running_auto(&states);
        for ds in &datasets {
            let s = states.get_mut(&ds.key()).expect("added above");
            let d = decide(st, ds, s, running, now);
            s.seen = match d {
                Decision::Off => Seen {
                    state: "off",
                    ..Default::default()
                },
                Decision::Idle => Seen {
                    state: "idle",
                    ..Default::default()
                },
                Decision::Running => Seen {
                    state: "running",
                    ..s.seen.clone()
                },
                Decision::Deferred(t, reason, detail) => Seen {
                    state: "deferred",
                    trigger: Some(t),
                    deferred: Some((reason, detail)),
                },
                Decision::Start(t) => {
                    running += 1;
                    // listed as running before the task starts, so the next pass and a
                    // manual request see it
                    s.task = Some((String::new(), true));
                    start.push((ds.clone(), t.clone()));
                    Seen {
                        state: "running",
                        trigger: Some(t),
                        deferred: None,
                    }
                }
            };
        }
    }
    for (ds, t) in start {
        tracing::info!("auto-compact /{}: {}", ds.key(), t.detail);
        start_compaction(st, ds, Some(t));
    }
}

/// Run [`tick`] once per second in a background thread.
pub fn spawn(st: Arc<AppState>) {
    std::thread::Builder::new()
        .name("auto-compact".into())
        .spawn(move || {
            loop {
                std::thread::sleep(Duration::from_secs(1));
                tick(&st, Instant::now());
            }
        })
        .expect("spawning the auto-compact thread");
}

/// Start a compaction task of `ds`: automatic with the trigger that made it due, manual
/// (`POST /$/compact/{ds}`) without. A manual one builds with every core and no rate
/// limit, an automatic one within the server's limits at a lower priority. Both stop
/// on `DELETE /$/tasks/{id}`.
pub fn start_compaction(st: &Arc<AppState>, ds: Arc<Dataset>, trigger: Option<Trigger>) -> Task {
    let auto = trigger.is_some();
    let ac = &st.compaction;
    let opts = CompactOptions {
        partial: Some(ac.policy_for(&ds).partial),
        threads: auto.then_some(ac.threads.max(1)),
        low_priority: auto,
        io_bytes_per_sec: if auto { ac.io_bytes_per_sec } else { None },
        ..Default::default()
    };
    let name = ds.key();
    let id = st.next_task_id();
    st.compaction
        .states
        .lock()
        .entry(name.clone())
        .or_default()
        .task = Some((id.clone(), auto));
    let state = st.clone();
    st.start_task_opts(id, TASK, &name, None, true, move |h| {
        let prefix = if auto { "auto: " } else { "" };
        let ctl = h.control();
        ctl.progress
            .report(0.05, &format!("{prefix}rebuilding index"));
        let started = SystemTime::now();
        let t0 = Instant::now();
        let r = ds.store.compact_with(&CompactOptions {
            cancel: Some(ctl.cancel.flag()),
            ..opts
        });
        record(&state, &ds.key(), trigger.as_ref(), started, t0.elapsed(), &r);
        let rep = r?;
        Ok(match &rep.abandoned {
            Some(why) => format!("{prefix}abandoned: {why}"),
            None => format!(
                "{prefix}compacted to {} in {:.2} s{}{}; carried over {} commits; the writer lock was held {:.1} ms",
                rep.generation,
                rep.total_ms / 1e3,
                trigger
                    .as_ref()
                    .map(|t| format!(" ({})", t.detail))
                    .unwrap_or_default(),
                partial_note(&rep),
                rep.caught_up_commits,
                rep.lock_ms
            ),
        })
    })
}

/// How much of the index a partial compaction rewrote ("" for a full one).
pub fn partial_note(rep: &CompactReport) -> String {
    if rep.mode != "partial" {
        return String::new();
    }
    format!(
        ", partially: {} of {} blocks rewritten",
        rep.blocks_rewritten,
        rep.blocks_rewritten + rep.blocks_copied
    )
}

/// Keep what a compaction did, for the status and the metrics.
fn record(
    st: &AppState,
    name: &str,
    trigger: Option<&Trigger>,
    started: SystemTime,
    took: Duration,
    r: &sparkles::Result<CompactReport>,
) {
    let auto = trigger.is_some();
    let mut states = st.compaction.states.lock();
    let s = states.entry(name.to_string()).or_default();
    let now = Instant::now();
    s.task = None;
    s.ended = Some(now);
    let outcome: &'static str = match r {
        Ok(rep) if rep.abandoned.is_some() => "abandoned",
        Ok(_) => "done",
        Err(sparkles::Error::Cancelled) => "cancelled",
        Err(_) => "failed",
    };
    *s.outcomes
        .entry((if auto { "auto" } else { "manual" }, outcome))
        .or_default() += 1;
    if auto {
        s.automatic_runs += 1;
        if outcome == "failed" {
            s.failures += 1;
            let wait = FIRST_BACKOFF
                .saturating_mul(1 << (s.failures - 1).min(6))
                .min(MAX_BACKOFF);
            s.backoff_until = Some(now + wait);
        }
    }
    if outcome == "done" {
        s.failures = 0;
        s.backoff_until = None;
    }
    let ms = |t: SystemTime| {
        t.duration_since(SystemTime::UNIX_EPOCH)
            .map_or(0, |d| d.as_millis() as i64)
    };
    let mut last = json!({
        "automatic": auto,
        "startedAt": sparkles::commit::rfc3339_ms(ms(started)),
        "finishedAt": sparkles::commit::rfc3339_ms(ms(SystemTime::now())),
        "seconds": took.as_secs_f64(),
        "outcome": outcome,
    });
    if let Some(t) = trigger {
        last["trigger"] = t.detail.clone().into();
    }
    match r {
        Ok(rep) => {
            last["generation"] = rep.generation.clone().into();
            last["lockMs"] = rep.lock_ms.into();
            last["buildMs"] = rep.build_ms.into();
            last["caughtUpCommits"] = rep.caught_up_commits.into();
            if rep.abandoned.is_none() {
                last["mode"] = rep.mode.clone().into();
                last["blocksRewritten"] = rep.blocks_rewritten.into();
                last["blocksCopied"] = rep.blocks_copied.into();
                if let Some(why) = &rep.full_reason {
                    last["fullReason"] = why.clone().into();
                }
            }
            if let Some(why) = &rep.abandoned {
                last["error"] = why.clone().into();
            } else {
                s.published += 1;
                s.seconds_sum += took.as_secs_f64();
                s.lock_seconds_sum += rep.lock_ms / 1e3;
                s.lock_seconds_max = s.lock_seconds_max.max(rep.lock_ms / 1e3);
            }
        }
        Err(e) => last["error"] = e.to_string().into(),
    }
    s.last = Some(last);
}

/// Cancel the running compaction of `name`, if there is one (an in-place restore needs
/// the dataset to itself).
#[cfg_attr(not(any(test, feature = "backup")), allow(dead_code))]
pub fn cancel(st: &AppState, name: &str) {
    if let Some(id) = st.active_task(TASK, name) {
        let _ = st.cancel_task(&id);
    }
}

/// `CompactionStatus` of a dataset (`/$/compaction/{ds}`, `compaction` in `/$/stats`).
pub fn status_json(st: &AppState, ds: &Dataset) -> J {
    use sparkles::handles::CompactionState;
    let ac = &st.compaction;
    // the policy in force, the measures and the verdict; the scheduler's state is the
    // server's
    let status = ds.dataset.settings().compaction().status(&ac.policy);
    let policy = &status.policy;
    let states = ac.states.lock();
    let s = states.get(&ds.key());
    let enabled = ac.enabled && policy.enabled && !st.read_only;
    // (with the policy enabled, the status is `running` while a compaction runs)
    let running =
        s.and_then(|s| s.task.as_ref()).is_some() || status.state == CompactionState::Running;
    // between two passes, a verdict of its own (the scheduler may not have run yet)
    let seen = s.map(|s| s.seen.clone()).unwrap_or_default();
    let trigger = seen.trigger.clone().or_else(|| status.trigger.clone());
    let state = if !enabled {
        "off"
    } else if running {
        "running"
    } else if seen.state == "deferred" {
        "deferred"
    } else if trigger.is_some() {
        "due"
    } else {
        "idle"
    };
    let mut j = json!({
        "dataset": ds.name,
        "enabled": enabled,
        "serverEnabled": ac.enabled,
        "policy": policy,
        "own": status.own,
        "state": state,
        "measures": status.measures,
        "automaticRuns": s.map_or(0, |s| s.automatic_runs),
        "failures": s.map_or(0, |s| s.failures),
    });
    if enabled
        && state != "idle"
        && let Some(t) = &trigger
    {
        j["trigger"] = t.detail.clone().into();
        j["triggerKind"] = t.kind.as_str().into();
    }
    if state == "deferred"
        && let Some((reason, detail)) = &seen.deferred
    {
        j["deferred"] = (*reason).into();
        j["deferredDetail"] = detail.clone().into();
    }
    if let Some((id, _)) = s.and_then(|s| s.task.as_ref())
        && !id.is_empty()
    {
        j["task"] = id.clone().into();
    }
    if let Some(last) = s.and_then(|s| s.last.clone()) {
        j["last"] = last;
    }
    j
}

async fn get_status(State(st): St, Path(name): Path<String>) -> ApiResult<Json<J>> {
    let ds = dataset(&st, &name)?;
    blocking(move || Ok(Json(status_json(&st, &ds)))).await
}

/// `PUT /$/compaction/{ds}`: replace the dataset's own settings with the JSON object's.
async fn put_settings(
    State(st): St,
    Path(name): Path<String>,
    AdminBody(body): AdminBody,
) -> ApiResult<Json<J>> {
    if st.read_only {
        return Err(err(StatusCode::FORBIDDEN, "server is read-only"));
    }
    let ds = dataset(&st, &name)?;
    let j: J = serde_json::from_slice(&body)
        .map_err(|e| err(StatusCode::BAD_REQUEST, format!("invalid JSON: {e}")))?;
    let s = CompactionSettings::from_json(&j)
        .map_err(|e| err(StatusCode::BAD_REQUEST, e.to_string()))?;
    blocking(move || {
        ds.dataset.settings().compaction().set(s)?;
        Ok(Json(status_json(&st, &ds)))
    })
    .await
}

/// `DELETE /$/compaction/{ds}`: remove the dataset's own settings.
async fn delete_settings(State(st): St, Path(name): Path<String>) -> ApiResult<Json<J>> {
    if st.read_only {
        return Err(err(StatusCode::FORBIDDEN, "server is read-only"));
    }
    let ds = dataset(&st, &name)?;
    blocking(move || {
        ds.dataset.settings().compaction().reset()?;
        Ok(Json(status_json(&st, &ds)))
    })
    .await
}

/// The compaction figures of one metrics label (datasets past `--metrics-max-datasets`
/// share `$other`).
#[derive(Default)]
struct Agg {
    outcomes: BTreeMap<(&'static str, &'static str), u64>,
    published: u64,
    seconds_sum: f64,
    lock_seconds_sum: f64,
    lock_seconds_max: f64,
    running: u64,
    due: u64,
}

/// The compaction metrics (Prometheus text).
pub fn metrics(st: &AppState, out: &mut String) {
    let label = |s: &str| {
        s.replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace('\n', "\\n")
    };
    let family = |o: &mut String, name: &str, kind: &str, help: &str| {
        let _ = writeln!(o, "# HELP {name} {help}");
        let _ = writeln!(o, "# TYPE {name} {kind}");
    };
    let datasets: Vec<Arc<Dataset>> = st.datasets().values().cloned().collect();
    let mut all: BTreeMap<String, Agg> = BTreeMap::new();
    for d in &datasets {
        let p = st.compaction.policy_for(d);
        let m = d.dataset.settings().compaction().measures();
        let on = st.compaction.enabled && p.enabled && !st.read_only;
        let a = all
            .entry(st.metrics.dataset_label(Some(&d.name)))
            .or_default();
        a.due += u64::from(on && p.verdict(&m).is_some());
        a.running += u64::from(m.compacting);
    }
    for (name, s) in st.compaction.states.lock().iter() {
        let a = all.entry(st.metrics.dataset_label(Some(name))).or_default();
        for (k, n) in &s.outcomes {
            *a.outcomes.entry(*k).or_default() += n;
        }
        a.published += s.published;
        a.seconds_sum += s.seconds_sum;
        a.lock_seconds_sum += s.lock_seconds_sum;
        a.lock_seconds_max = a.lock_seconds_max.max(s.lock_seconds_max);
    }
    family(
        out,
        "sparkles_compactions_total",
        "counter",
        "Compactions by mode (auto or manual) and outcome (done, abandoned, cancelled, failed).",
    );
    for (ds, a) in &all {
        for ((mode, outcome), n) in &a.outcomes {
            let _ = writeln!(
                out,
                "sparkles_compactions_total{{dataset=\"{}\",mode=\"{mode}\",outcome=\"{outcome}\"}} {n}",
                label(ds)
            );
        }
    }
    for (name, help, f) in [
        (
            "sparkles_compaction_seconds",
            "Duration of the compactions that published a generation.",
            (|a: &Agg| a.seconds_sum) as fn(&Agg) -> f64,
        ),
        (
            "sparkles_compaction_lock_seconds",
            "How long the switch of each published compaction held the writer lock.",
            |a: &Agg| a.lock_seconds_sum,
        ),
    ] {
        family(out, name, "summary", help);
        for (ds, a) in all.iter().filter(|(_, a)| a.published > 0) {
            let _ = writeln!(out, "{name}_sum{{dataset=\"{}\"}} {:.6}", label(ds), f(a));
            let _ = writeln!(
                out,
                "{name}_count{{dataset=\"{}\"}} {}",
                label(ds),
                a.published
            );
        }
    }
    family(
        out,
        "sparkles_compaction_lock_seconds_max",
        "gauge",
        "The longest the switch of a compaction held the writer lock since the server started.",
    );
    for (ds, a) in all.iter().filter(|(_, a)| a.published > 0) {
        let _ = writeln!(
            out,
            "sparkles_compaction_lock_seconds_max{{dataset=\"{}\"}} {:.6}",
            label(ds),
            a.lock_seconds_max
        );
    }
    for (name, help, pick) in [
        (
            "sparkles_compaction_running",
            "Compactions of the dataset running now.",
            (|a: &Agg| a.running) as fn(&Agg) -> u64,
        ),
        (
            "sparkles_compaction_due",
            "Datasets whose compaction policy says a compaction is due (0 or 1 per dataset).",
            |a: &Agg| a.due,
        ),
    ] {
        family(out, name, "gauge", help);
        for (ds, a) in &all {
            let _ = writeln!(out, "{name}{{dataset=\"{}\"}} {}", label(ds), pick(a));
        }
    }
}

#[cfg(test)]
#[path = "compaction_tests.rs"]
mod tests;

/// The `serve` flags of automatic compaction.
#[derive(clap::Args, Debug, Clone)]
pub struct AutoCompactArgs {
    /// Never compact automatically (POST /$/compact/{ds} and `sparkles compact` still
    /// work)
    #[arg(long)]
    pub no_auto_compact: bool,
    /// Automatic compaction: no trigger on the number of quads, idle time or age fires
    /// with a smaller delta (inserted plus deleted quads)
    #[arg(long, value_name = "N", default_value_t = 10_000)]
    pub auto_compact_min_quads: u64,
    /// Automatic compaction: compact once the delta reaches the minimum plus this share
    /// of the base index's quads
    #[arg(long, value_name = "R", default_value_t = 0.05)]
    pub auto_compact_ratio: f64,
    /// Automatic compaction: compact at this many delta quads, whatever the base (0: no
    /// limit)
    #[arg(long, value_name = "N", default_value_t = 1_000_000)]
    pub auto_compact_max_quads: u64,
    /// Automatic compaction: compact when the delta takes about this many MiB of memory
    /// (0: no limit)
    #[arg(long, value_name = "MIB", default_value_t = 512)]
    pub auto_compact_max_delta_mb: u64,
    /// Automatic compaction: compact when the write-ahead log passes this many MiB (0:
    /// no limit)
    #[arg(long, value_name = "MIB", default_value_t = 1024)]
    pub auto_compact_max_wal_mb: u64,
    /// Automatic compaction: compact a delta of at least the minimum once no commit has
    /// been made for this many seconds (0: off)
    #[arg(long, value_name = "SECS", default_value_t = 300)]
    pub auto_compact_idle: u64,
    /// Automatic compaction: compact when the oldest change not compacted is this many
    /// seconds old (0: off)
    #[arg(long, value_name = "SECS", default_value_t = 86_400)]
    pub auto_compact_max_age: u64,
    /// Automatic compaction: start none sooner than this many seconds after the
    /// previous compaction of the dataset ended
    #[arg(long, value_name = "SECS", default_value_t = 60)]
    pub auto_compact_min_interval: u64,
    /// Threads of an automatic compaction's build, which runs at a lower priority
    /// (default: a quarter of the cores)
    #[arg(long, value_name = "N")]
    pub auto_compact_threads: Option<usize>,
    /// The average rate at which an automatic compaction may write its new index, in
    /// MiB per second (0: no limit)
    #[arg(long, value_name = "MIB", default_value_t = 0)]
    pub auto_compact_io_mb: u64,
    /// Automatic compactions that may run at once on the server
    #[arg(long, value_name = "N", default_value_t = 1)]
    pub auto_compact_max_running: usize,
    /// Whether a compaction, automatic or not, may rewrite only the index blocks its
    /// delta touches: auto (when the delta adds no terms and that is estimated to be
    /// quicker), off or always (whenever the delta adds no terms)
    #[arg(long, value_name = "MODE", default_value = "auto", value_parser = parse_partial)]
    pub auto_compact_partial: PartialMode,
}

fn parse_partial(s: &str) -> Result<PartialMode, String> {
    PartialMode::parse(s).ok_or_else(|| format!("expected auto, off or always, not {s:?}"))
}

impl AutoCompactArgs {
    /// The server's automatic compaction from the flags.
    pub fn state(&self) -> anyhow::Result<AutoCompact> {
        let r = self.auto_compact_ratio;
        if !(r.is_finite() && (0.0..=1000.0).contains(&r)) {
            anyhow::bail!("--auto-compact-ratio expects a number from 0 to 1000");
        }
        Ok(AutoCompact {
            enabled: !self.no_auto_compact,
            policy: CompactionPolicy {
                enabled: true,
                min_delta_quads: self.auto_compact_min_quads,
                delta_ratio: r,
                max_delta_quads: self.auto_compact_max_quads,
                max_delta_mb: self.auto_compact_max_delta_mb,
                max_wal_mb: self.auto_compact_max_wal_mb,
                idle_seconds: self.auto_compact_idle,
                max_age_seconds: self.auto_compact_max_age,
                min_interval_seconds: self.auto_compact_min_interval,
                partial: self.auto_compact_partial,
            },
            threads: self
                .auto_compact_threads
                .unwrap_or_else(default_threads)
                .max(1),
            io_bytes_per_sec: (self.auto_compact_io_mb > 0)
                .then_some(self.auto_compact_io_mb << 20),
            max_running: self.auto_compact_max_running.max(1),
            states: Default::default(),
        })
    }
}
