//! The reasoning status as the server shows it, the reasoning task, automatic
//! re-materialization and inconsistency diagnostics.
//!
//! The library's [`sparkles::handles::Reasoning`] materializes, keeps the record and
//! computes the freshness of the inferences (see [`sparkles::reasoning::freshness`]).
//! This module renders the status, runs a materialization as a task, and schedules the
//! automatic re-runs.

use crate::state::{AppState, Dataset, ReasoningInfo};
use axum::http::HeaderValue;
use parking_lot::Mutex;
use serde_json::{Value as J, json};
pub use sparkles::reasoning::{ReasoningStatus, freshness};
#[cfg(feature = "reasoning")]
pub use sparkles::reasoning::{incremental_since, record_inputs, recorded, run_text};
use sparkles::store::Store;
use std::collections::HashMap;
#[cfg(feature = "reasoning")]
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Response header on reads that include stale (or unknown-freshness) inferences.
pub const SPARKLES_INFERENCES: &str = "sparkles-inferences";

/// `DatasetInfo.reasoning`: the recorded summary plus freshness at the head.
pub fn info_json(ds: &Dataset) -> J {
    let Some(s) = ds.dataset.reasoning().status() else {
        return J::Null;
    };
    let (info, f) = (&s.record, &s.freshness);
    json!({
        "profile": info.profile,
        "inferred": info.inferred,
        "at": info.at,
        "commit": info.commit,
        "stale": f.stale,
        "commitsSince": f.commits_since,
    })
}

/// The full `ReasoningStatus`, or `null`.
pub fn status_json(st: &AppState, ds: &Dataset) -> J {
    let Some(s) = ds.dataset.reasoning().status() else {
        return J::Null;
    };
    let auto = auto_json(st, &ds.name, &s.record);
    status_render(&s, auto)
}

/// `ReasoningStatus.auto`: the effective setting, where it comes from, and the next
/// planned run.
pub fn auto_json(st: &AppState, name: &str, info: &ReasoningInfo) -> J {
    let source = if info.auto.is_some() {
        "dataset"
    } else {
        "server"
    };
    let Some(t) = auto_timing(st, info) else {
        return json!({ "enabled": false, "source": source });
    };
    let mut auto = json!({
        "enabled": true,
        "source": source,
        "debounceSeconds": t.debounce.as_secs_f64(),
        "maxDelaySeconds": t.max_delay.as_secs_f64(),
    });
    if let Some(at) = st.auto_reason.as_ref().and_then(|a| a.scheduled(name, t)) {
        auto["scheduledAt"] = at.into();
    }
    auto
}

/// `ReasoningStatus` of a recorded status at the store's head.
#[cfg_attr(not(feature = "reasoning"), allow(dead_code))]
pub fn status_value(info: &ReasoningInfo, store: &Store, auto: J) -> J {
    status_render(&ReasoningStatus::of(info.clone(), store), auto)
}

/// `ReasoningStatus` of the library's status, with the server's `auto`.
fn status_render(s: &ReasoningStatus, auto: J) -> J {
    let (info, f, head) = (&s.record, s.freshness.clone(), s.head);
    let mut j = json!({
        "profile": info.profile,
        "inferred": info.inferred,
        "at": info.at,
        "commit": info.commit,
        "head": head,
        "stale": f.stale,
        "commitsSince": f.commits_since,
        "auto": auto,
        "warnings": info.warnings,
    });
    if let Some(r) = f.reason {
        j["staleReason"] = r.into();
    }
    if let Some(run) = &info.run {
        j["run"] = serde_json::to_value(run).unwrap_or(J::Null);
    }
    if !info.vocabularies.is_empty() {
        j["vocabularies"] = info.vocabularies.clone().into();
    }
    if info.geo_default_geometry {
        j["geoDefaultGeometry"] = true.into();
    }
    if let Some(i) = &info.inputs {
        j["inputs"] = i.clone();
    }
    if let Some(g) = &info.input_graphs {
        j["inputGraphs"] = g.clone().into();
    }
    if let Some(g) = &info.watched_graphs {
        j["watchedGraphs"] = g.clone().into();
    }
    if !info.imports.is_empty() {
        j["imports"] = info.imports.clone().into();
    }
    if !info.fetched_imports.is_empty() {
        j["fetchedImports"] = info.fetched_imports.clone().into();
    }
    j
}

/// The profile with its extras: `rdfs + geosparql + default geometries`.
fn profile_text(info: &ReasoningInfo) -> String {
    let mut s = info.profile.clone();
    for v in &info.vocabularies {
        s.push_str(" + ");
        s.push_str(v);
    }
    if info.geo_default_geometry {
        s.push_str(" + default geometries");
    }
    s
}

/// "up to date", noting the later commits that left the input graphs unchanged.
pub fn up_to_date(commits_since: Option<u64>) -> String {
    match commits_since {
        Some(n) if n > 0 => format!(
            "up to date; {n} later commit{} left the input graphs unchanged",
            if n == 1 { "" } else { "s" }
        ),
        _ => "up to date".to_string(),
    }
}

/// One line for the CLI: `owl-rl, 1234 inferred at commit 40 (STALE: 3 commits since)`.
pub fn status_line(info: &ReasoningInfo, store: &Store) -> String {
    let f = freshness(info, store, store.head_commit().seq);
    let at = match info.commit {
        Some(c) => format!("at commit {c}"),
        None => format!("at {}", info.at),
    };
    let state = match (f.stale, f.commits_since) {
        (Some(false), n) => up_to_date(n),
        (Some(true), Some(n)) => {
            format!("STALE: {n} commit{} since", if n == 1 { "" } else { "s" })
        }
        (Some(true), None) => format!("STALE: {}", f.reason.unwrap_or_default()),
        (None, _) => format!("freshness unknown: {}", f.reason.unwrap_or_default()),
    };
    format!(
        "{}, {} inferred {at} ({state})",
        profile_text(info),
        info.inferred
    )
}

/// The `Sparkles-Inferences` header for a read of commit `seq` that included the
/// inferred graph; `None` when the inferences are fresh.
pub fn inferences_header(ds: &Dataset, seq: u64) -> Option<HeaderValue> {
    let f = ds.dataset.reasoning().freshness_at(seq)?;
    let v = match (f.stale, f.commits_since) {
        (Some(false), _) => return None,
        (Some(true), Some(n)) => format!("stale; commits-since={n}"),
        (Some(true), None) => "stale".to_string(),
        (None, _) => "unknown".to_string(),
    };
    HeaderValue::from_str(&v).ok()
}

// ---------------------------------------------------------------- running ------

/// What started a reasoning run.
#[cfg(feature = "reasoning")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Trigger {
    /// `POST /$/reason/{ds}`
    Request,
    /// the automatic run loop; a superseded run stops as soon as a write transaction
    /// waits for the writer lock it holds
    Auto { superseded_by_writes: bool },
}

/// Start a `reason` task: the library's materialization
/// ([`sparkles::handles::Reasoning::run_with`]) with the task's cancellation and
/// progress, then the registry saved. The task can be cancelled.
///
/// With `incremental`, the run updates the recorded materialization when it can, with
/// the closure the dataset keeps in memory or, after a restart, the one saved with it.
/// The run reads the graphs of `inputs`. With imports fetched, it first loads the
/// missing ones under the server's `LOAD` rules, and with `refresh` loads again those
/// that earlier runs fetched.
#[cfg(feature = "reasoning")]
#[allow(clippy::too_many_arguments)]
pub fn start_reason(
    st: &Arc<AppState>,
    ds: Arc<Dataset>,
    profile: sparkles_reasoner::Profile,
    extras: sparkles_reasoner::Extras,
    inputs: sparkles_reasoner::Inputs,
    trigger: Trigger,
    incremental: bool,
    refresh: bool,
) -> crate::state::Task {
    let st2 = st.clone();
    let name = ds.name.clone();
    let auto = matches!(trigger, Trigger::Auto { .. });
    let yields = trigger
        == Trigger::Auto {
            superseded_by_writes: true,
        };
    let prefix = if auto { "auto: " } else { "" };
    st.start_task_opts(st.next_task_id(), "reason", &name, None, true, move |h| {
        let task = h.control();
        let progress = task.progress.clone();
        let ctl = sparkles::task::Control {
            progress: sparkles::task::Progress::new(move |p, msg: &str| {
                progress.report(p, &format!("{prefix}{msg}"))
            }),
            ..task.clone()
        };
        let req = sparkles::reasoning::ReasonRequest {
            profile,
            extras,
            inputs,
            incremental,
            refresh_imports: refresh,
            yield_to_writers: yields,
            // the rules of the server's LOAD
            load: sparkles::sparql::QueryOptions {
                outbound: st2.outbound.clone(),
                file_loads: st2.file_loads.clone(),
                ..Default::default()
            },
        };
        ds.closure.set_max_triples(st2.reason_cache_triples);
        let outcome = match ds.dataset.reasoning().run_with(&req, &ctl) {
            Ok(o) => o,
            Err(sparkles::Error::Component(c))
                if c.code == sparkles::reasoning::run::SUPERSEDED =>
            {
                return Err(anyhow::Error::new(sparkles::Error::Cancelled)
                    .context("auto: superseded by a write"));
            }
            Err(sparkles::Error::Cancelled) => {
                return Err(
                    anyhow::Error::new(sparkles::Error::Cancelled).context("reasoning cancelled")
                );
            }
            Err(e) => {
                let e = match rejection_text(&e) {
                    Some(text) => anyhow::anyhow!("{prefix}{text}"),
                    None if auto => anyhow::Error::new(e).context("auto: reasoning failed"),
                    None => e.into(),
                };
                return Err(e);
            }
        };
        st2.save_registry()?;
        let report = &outcome.report;
        let mut detail =
            serde_json::to_value(sparkles::reasoning::run_info(report)).unwrap_or(J::Null);
        detail["inferred"] = report.inferred.into();
        detail["millis"] = report.millis.into();
        detail["iterations"] = report.iterations.into();
        h.set_detail(detail);
        Ok(format!(
            "{prefix}{} inferred triples in {} ms ({}){}",
            report.inferred,
            report.millis,
            run_text(report),
            if report.warnings.is_empty() {
                String::new()
            } else {
                format!("; warnings: {}", report.warnings.join("; "))
            }
        ))
    })
}

/// The task error of a materialization rejected by write-time validation:
/// `inferences rejected by SHACL validation: 2 blocking results (first: <shape> at <node>)`
/// (ShEx: `… 2 nonconformant associations …`).
#[cfg(feature = "reasoning")]
pub fn rejection_text(e: &sparkles::Error) -> Option<String> {
    let sparkles::Error::Rejected(r) = e else {
        return None;
    };
    let s = &r.summary;
    let (lang, what) = match s.language {
        sparkles::guard::GuardLanguage::Shacl => ("SHACL", "blocking result"),
        sparkles::guard::GuardLanguage::Shex => ("ShEx", "nonconformant association"),
    };
    Some(match &s.shapes_error {
        Some(err) => {
            format!(
                "inferences rejected by {lang} validation: the {} cannot be read ({err})",
                if lang == "ShEx" {
                    "schema"
                } else {
                    "shapes graph"
                }
            )
        }
        None => format!(
            "inferences rejected by {lang} validation: {} {what}{}{}",
            s.blocking,
            if s.blocking == 1 { "" } else { "s" },
            crate::obs::first_result(s)
                .map(|(shape, node)| format!(" (first: {shape} at {node})"))
                .unwrap_or_default()
        ),
    })
}

#[cfg(feature = "reasoning")]
fn reason_running(st: &AppState, name: &str) -> bool {
    st.tasks
        .lock()
        .iter()
        .any(|t| t.kind == "reason" && t.dataset == name && t.active())
}

// -------------------------------------------------------------- auto mode ------

/// Automatic re-materialization: stale inferences are re-materialized once the head has
/// not changed for the debounce, or the maximum delay after they became stale when
/// writes never pause that long. `serve --auto-reason SECS [--auto-reason-max-delay
/// SECS]` sets the server-wide timing; a dataset's own setting (`PUT
/// /$/reason/{ds}/auto`) takes precedence over it.
pub struct AutoReason {
    /// the server-wide setting; `None` runs only datasets that enable it themselves
    pub server: Option<AutoTiming>,
    pending: Mutex<HashMap<String, Pending>>,
}

/// When automatic runs start.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AutoTiming {
    pub debounce: Duration,
    pub max_delay: Duration,
}

/// The debounce of a dataset that enables automatic runs without one, on a server
/// without `--auto-reason`.
pub const DEFAULT_DEBOUNCE: Duration = Duration::from_secs(5);

/// The automatic run timing that applies to a dataset, if automatic runs are on for it:
/// its own setting, else the server's. Never on a read-only server, or without the
/// automatic run loop.
pub fn auto_timing(st: &AppState, info: &ReasoningInfo) -> Option<AutoTiming> {
    if st.read_only {
        return None;
    }
    let auto = st.auto_reason.as_ref()?;
    let Some(own) = &info.auto else {
        return auto.server;
    };
    if !own.enabled {
        return None;
    }
    let secs = |s: Option<f64>| s.map(Duration::from_secs_f64);
    let debounce = secs(own.debounce_seconds)
        .or(auto.server.map(|t| t.debounce))
        .unwrap_or(DEFAULT_DEBOUNCE);
    let max_delay = match (secs(own.max_delay_seconds), auto.server) {
        (Some(m), _) => m,
        // the server's maximum delay goes with the server's debounce
        (None, Some(s)) if own.debounce_seconds.is_none() => s.max_delay,
        (None, _) => debounce * 12,
    };
    Some(AutoTiming {
        debounce,
        max_delay,
    })
}

struct Pending {
    head: u64,
    /// when `head` last changed
    changed: Instant,
    /// when the current stale period (or the last automatic run) started
    since: Instant,
    /// the automatic run started, and the head it started at
    task: Option<(String, u64)>,
    /// an automatic run failed at this head: wait for the next change
    failed: Option<u64>,
}

impl AutoReason {
    /// The server-wide setting of `--auto-reason`.
    pub fn new(debounce: Duration, max_delay: Option<Duration>) -> AutoReason {
        AutoReason {
            server: Some(AutoTiming {
                debounce,
                max_delay: max_delay.unwrap_or(debounce * 12),
            }),
            pending: Mutex::new(HashMap::new()),
        }
    }

    /// No server-wide setting: only datasets that enable automatic runs get them.
    pub fn per_dataset() -> AutoReason {
        AutoReason {
            server: None,
            pending: Mutex::new(HashMap::new()),
        }
    }

    /// RFC 3339 time of the next planned run of a dataset, if one is planned.
    fn scheduled(&self, name: &str, t: AutoTiming) -> Option<String> {
        let p = self.pending.lock();
        let p = p.get(name)?;
        if p.task.is_some() || p.failed == Some(p.head) {
            return None;
        }
        let due = (p.changed + t.debounce).min(p.since + t.max_delay);
        let wait = due.saturating_duration_since(Instant::now());
        let at = std::time::SystemTime::now() + wait;
        let ms = at
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_millis() as i64);
        Some(sparkles::commit::rfc3339_ms(ms))
    }
}

/// One pass of the automatic re-materialization loop.
#[cfg(feature = "reasoning")]
pub fn auto_reason_tick(st: &Arc<AppState>, now: Instant) {
    let Some(auto) = st.auto_reason.as_ref() else {
        return;
    };
    if st.read_only {
        return;
    }
    let datasets: Vec<Arc<Dataset>> = st.datasets().values().cloned().collect();
    let mut pending = auto.pending.lock();
    pending.retain(|n, _| datasets.iter().any(|d| &d.name == n));
    for ds in datasets {
        let info = ds.reasoning.read().clone();
        let Some(timing) = info.as_ref().and_then(|i| auto_timing(st, i)) else {
            pending.remove(&ds.name);
            continue;
        };
        let head = ds.store.head_commit().seq;
        let stale = info.as_ref().map(|i| freshness(i, &ds.store, head).stale);
        // never guess: only runs for inferences known to be stale
        if stale != Some(Some(true)) {
            pending.remove(&ds.name);
            continue;
        }
        let p = pending.entry(ds.name.clone()).or_insert(Pending {
            head,
            changed: now,
            since: now,
            task: None,
            failed: None,
        });
        if p.head != head {
            p.head = head;
            p.changed = now;
        }
        if let Some((id, at)) = p.task.clone() {
            let state = st
                .tasks
                .lock()
                .iter()
                .find(|t| t.id == id)
                .map(|t| t.state.clone());
            match state.as_deref() {
                Some("running" | "queued") => continue,
                Some("failed") => p.failed = Some(at),
                // superseded by a write (or cancelled): the stale period goes on
                Some("cancelled") => {}
                // stale again after a run: a new stale period
                _ => p.since = now,
            }
            p.task = None;
        }
        if p.failed == Some(head) || reason_running(st, &ds.name) {
            continue;
        }
        let quiet = now.duration_since(p.changed) >= timing.debounce;
        // a run forced by the maximum delay is not superseded, or it would never finish
        // while writes go on
        let forced = !quiet && now.duration_since(p.since) >= timing.max_delay;
        if !quiet && !forced {
            continue;
        }
        let run = info
            .as_ref()
            .map(|i| Ok::<_, sparkles::Error>((i.profile()?, i.extras()?, i.run_inputs()?)));
        let (profile, extras, inputs) = match run {
            Some(Ok(p)) => p,
            Some(Err(e)) => {
                tracing::warn!("auto-reason /{}: {e:#}", ds.name);
                p.failed = Some(head);
                continue;
            }
            None => continue,
        };
        tracing::info!("auto-reason /{}: re-running {}", ds.name, profile.name());
        let trigger = Trigger::Auto {
            superseded_by_writes: !forced,
        };
        let task = start_reason(
            st,
            ds.clone(),
            profile,
            extras,
            inputs,
            trigger,
            true,
            false,
        );
        p.task = Some((task.id, head));
        p.since = now;
    }
}

/// Run [`auto_reason_tick`] once per second in a background thread.
#[cfg(feature = "reasoning")]
pub fn spawn_auto_reason(st: Arc<AppState>) {
    std::thread::Builder::new()
        .name("auto-reason".into())
        .spawn(move || {
            loop {
                std::thread::sleep(Duration::from_secs(1));
                auto_reason_tick(&st, Instant::now());
            }
        })
        .expect("spawning the auto-reason thread");
}

// ------------------------------------------------------------ diagnostics ------

/// The diagnostics report of a store's current snapshot as JSON, with the dataset name
/// and the inferences' freshness at that snapshot (see [`sparkles::reasoning::diagnose`]).
#[cfg(feature = "reasoning")]
pub fn diagnostics_json(
    name: &str,
    store: &Store,
    info: Option<&ReasoningInfo>,
    opts: &sparkles_reasoner::diagnostics::DiagnoseOptions,
) -> anyhow::Result<(sparkles_reasoner::diagnostics::DiagnosticsReport, J)> {
    let d = sparkles::reasoning::diagnose(store, info, opts)?;
    Ok(diagnostics_render(name, d))
}

/// The JSON of a diagnostics report, with the dataset name and the inferences' freshness.
#[cfg(feature = "reasoning")]
pub fn diagnostics_render(
    name: &str,
    d: sparkles::reasoning::Diagnostics,
) -> (sparkles_reasoner::diagnostics::DiagnosticsReport, J) {
    let mut j = d.report.to_json();
    j["dataset"] = name.into();
    if let Some((profile, f)) = &d.inferences {
        let inf = &mut j["scope"]["inferences"];
        inf["profile"] = profile.clone().into();
        inf["stale"] = f.stale.into();
        inf["commitsSince"] = f.commits_since.into();
    }
    (d.report, j)
}
