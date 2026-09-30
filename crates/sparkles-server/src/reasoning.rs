//! Freshness of materialized inferences, re-runs, automatic re-materialization and
//! inconsistency diagnostics.
//!
//! A materialization records the commit (`seq`) it wrote, or the unchanged head it read
//! when it changed nothing, together with the dataset id. The inferences are fresh while
//! the head is still that commit; every later commit makes them stale (conservatively:
//! also commits that touch only named graphs the reasoner does not read).

use crate::state::{AppState, Dataset, ReasoningInfo};
use axum::http::HeaderValue;
use parking_lot::Mutex;
use serde_json::{Value as J, json};
use sparkles::store::Store;
use std::collections::HashMap;
#[cfg(feature = "reasoning")]
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Response header on reads that include stale (or unknown-freshness) inferences.
pub const SPARKLES_INFERENCES: &str = "sparkles-inferences";

/// How the recorded inferences relate to a store position.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Freshness {
    /// `None`: unknown (no recorded position, or one from another dataset)
    pub stale: Option<bool>,
    pub commits_since: Option<u64>,
    pub reason: Option<String>,
}

/// Freshness of `info` at commit `at` of `store`.
pub fn freshness(info: &ReasoningInfo, store: &Store, at: u64) -> Freshness {
    let unknown = |why: &str| Freshness {
        stale: None,
        commits_since: None,
        reason: Some(why.to_string()),
    };
    if info.inherited_stale {
        return Freshness {
            stale: Some(true),
            commits_since: None,
            reason: Some("inherited from source at clone time".into()),
        };
    }
    let Some(commit) = info.commit else {
        return unknown("no recorded store position (materialized by an older version)");
    };
    if info.position_source.as_deref() != Some("commit") {
        return unknown("recorded position is not a commit");
    }
    if info.dataset_id.as_deref() != Some(store.dataset_id().to_string().as_str()) {
        return unknown("recorded for another dataset");
    }
    match at.cmp(&commit) {
        std::cmp::Ordering::Equal => Freshness {
            stale: Some(false),
            commits_since: Some(0),
            reason: None,
        },
        std::cmp::Ordering::Greater => {
            let n = at - commit;
            Freshness {
                stale: Some(true),
                commits_since: Some(n),
                reason: Some(format!(
                    "{n} commit{} since materialization",
                    if n == 1 { "" } else { "s" }
                )),
            }
        }
        std::cmp::Ordering::Less => Freshness {
            stale: Some(true),
            commits_since: None,
            reason: Some("store position moved backwards".into()),
        },
    }
}

/// `DatasetInfo.reasoning`: the recorded summary plus freshness at the head.
pub fn info_json(ds: &Dataset) -> J {
    let Some(info) = ds.reasoning.read().clone() else {
        return J::Null;
    };
    let f = freshness(&info, &ds.store, ds.store.head_commit().seq);
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
    let Some(info) = ds.reasoning.read().clone() else {
        return J::Null;
    };
    let mut auto = json!({ "enabled": false });
    if let Some(a) = st.auto_reason.as_ref().filter(|_| !st.read_only) {
        auto = json!({ "enabled": true, "debounceSeconds": a.debounce.as_secs_f64() });
        if let Some(at) = a.scheduled(&ds.name) {
            auto["scheduledAt"] = at.into();
        }
    }
    status_value(&info, &ds.store, auto)
}

/// `ReasoningStatus` of a recorded status at the store's head.
pub fn status_value(info: &ReasoningInfo, store: &Store, auto: J) -> J {
    let head = store.head_commit().seq;
    let f = freshness(info, store, head);
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
    j
}

/// One line for the CLI: `owl-rl, 1234 inferred at commit 40 (STALE: 3 commits since)`.
pub fn status_line(info: &ReasoningInfo, store: &Store) -> String {
    let f = freshness(info, store, store.head_commit().seq);
    let at = match info.commit {
        Some(c) => format!("at commit {c}"),
        None => format!("at {}", info.at),
    };
    let state = match (f.stale, f.commits_since) {
        (Some(false), _) => "up to date".to_string(),
        (Some(true), Some(n)) => {
            format!("STALE: {n} commit{} since", if n == 1 { "" } else { "s" })
        }
        (Some(true), None) => format!("STALE: {}", f.reason.unwrap_or_default()),
        (None, _) => format!("freshness unknown: {}", f.reason.unwrap_or_default()),
    };
    format!(
        "{}, {} inferred {at} ({state})",
        info.profile, info.inferred
    )
}

/// The `Sparkles-Inferences` header for a read of commit `seq` that included the
/// inferred graph; `None` when the inferences are fresh.
pub fn inferences_header(ds: &Dataset, seq: u64) -> Option<HeaderValue> {
    let info = ds.reasoning.read().clone()?;
    let f = freshness(&info, &ds.store, seq);
    let v = match (f.stale, f.commits_since) {
        (Some(false), _) => return None,
        (Some(true), Some(n)) => format!("stale; commits-since={n}"),
        (Some(true), None) => "stale".to_string(),
        (None, _) => "unknown".to_string(),
    };
    HeaderValue::from_str(&v).ok()
}

// ---------------------------------------------------------------- running ------

/// The status recorded after a materialization.
#[cfg(feature = "reasoning")]
pub fn recorded(
    profile: &sparkles_reasoner::Profile,
    report: &sparkles_reasoner::ReasonReport,
    store: &Store,
) -> ReasoningInfo {
    let receipt = report.receipt.as_ref();
    ReasoningInfo {
        reasoning_format: 2,
        profile: profile.name().to_string(),
        inferred: report.inferred,
        at: crate::state::now(),
        // the run's own commit, or the head it read when it changed nothing
        commit: receipt.map(|r| r.commit.seq),
        position_source: receipt.map(|_| "commit".to_string()),
        dataset_id: Some(
            receipt
                .map_or(store.dataset_id(), |r| r.dataset_id)
                .to_string(),
        ),
        rules: match profile {
            sparkles_reasoner::Profile::Rules(t) => Some(t.clone()),
            _ => None,
        },
        warnings: report.warnings.clone(),
        millis: Some(report.millis),
        inherited_stale: false,
    }
}

/// The profile a recorded status re-runs.
#[cfg(feature = "reasoning")]
pub fn recorded_profile(info: &ReasoningInfo) -> anyhow::Result<sparkles_reasoner::Profile> {
    if info.profile == "rules" {
        return match &info.rules {
            Some(t) => Ok(sparkles_reasoner::Profile::Rules(t.clone())),
            None => anyhow::bail!("the recorded custom rules are not available to re-run"),
        };
    }
    Ok(info.profile.parse()?)
}

/// Start a `reason` task: materialize, then record the status (data first, then the
/// status file, so a crash in between reads as stale).
#[cfg(feature = "reasoning")]
pub fn start_reason(
    st: &Arc<AppState>,
    ds: Arc<Dataset>,
    profile: sparkles_reasoner::Profile,
    auto: bool,
) -> crate::state::Task {
    let st2 = st.clone();
    let name = ds.name.clone();
    let prefix = if auto { "auto: " } else { "" };
    st.start_task("reason", &name, move |h| {
        let h2 = h.clone();
        let progress: sparkles_reasoner::ProgressFn =
            Arc::new(move |p, msg: &str| h2.progress(p, &format!("{prefix}{msg}")));
        h.progress(0.05, &format!("{prefix}loading triples"));
        let opts = sparkles_reasoner::ReasonOptions {
            progress: Some(progress),
            ..Default::default()
        };
        let report = match sparkles_reasoner::materialize(&ds.store, &profile, &opts) {
            Ok(r) => r,
            Err(e) => {
                let e = match rejection_text(&e) {
                    Some(text) => anyhow::anyhow!("{prefix}{text}"),
                    None if auto => e.context("auto: reasoning failed"),
                    None => e,
                };
                return Err(e);
            }
        };
        ds.set_reasoning(Some(recorded(&profile, &report, &ds.store)))?;
        st2.save_registry()?;
        Ok(format!(
            "{prefix}{} inferred triples in {} ms ({} iterations){}",
            report.inferred,
            report.millis,
            report.iterations,
            if report.warnings.is_empty() {
                String::new()
            } else {
                format!("; warnings: {}", report.warnings.join("; "))
            }
        ))
    })
}

/// The task error of a materialization rejected by write-time validation:
/// `inferences rejected by SHACL validation: 2 blocking results (first: <shape> at <node>)`.
#[cfg(feature = "reasoning")]
pub fn rejection_text(e: &anyhow::Error) -> Option<String> {
    let Some(sparkles::Error::Rejected(r)) = e.downcast_ref::<sparkles::Error>() else {
        return None;
    };
    let s = &r.summary;
    Some(match &s.shapes_error {
        Some(err) => {
            format!(
                "inferences rejected by SHACL validation: the shapes graph cannot be read ({err})"
            )
        }
        None => format!(
            "inferences rejected by SHACL validation: {} blocking result{}{}",
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
        .any(|t| t.kind == "reason" && t.dataset == name && t.state == "running")
}

// -------------------------------------------------------------- auto mode ------

/// `serve --auto-reason SECS [--auto-reason-max-delay SECS]`: re-materialize stale
/// inferences once the head has not changed for `debounce`, or `max_delay` after they
/// became stale when writes never pause that long.
pub struct AutoReason {
    pub debounce: Duration,
    pub max_delay: Duration,
    pending: Mutex<HashMap<String, Pending>>,
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
    pub fn new(debounce: Duration, max_delay: Option<Duration>) -> AutoReason {
        AutoReason {
            debounce,
            max_delay: max_delay.unwrap_or(debounce * 12),
            pending: Mutex::new(HashMap::new()),
        }
    }

    /// RFC 3339 time of the next planned run of a dataset, if one is planned.
    fn scheduled(&self, name: &str) -> Option<String> {
        let p = self.pending.lock();
        let p = p.get(name)?;
        if p.task.is_some() || p.failed == Some(p.head) {
            return None;
        }
        let due = (p.changed + self.debounce).min(p.since + self.max_delay);
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
    let datasets: Vec<Arc<Dataset>> = st.datasets.read().values().cloned().collect();
    let mut pending = auto.pending.lock();
    pending.retain(|n, _| datasets.iter().any(|d| &d.name == n));
    for ds in datasets {
        let info = ds.reasoning.read().clone();
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
                Some("running") => continue,
                Some("failed") => p.failed = Some(at),
                // stale again after a run: a new stale period
                _ => p.since = now,
            }
            p.task = None;
        }
        if p.failed == Some(head) || reason_running(st, &ds.name) {
            continue;
        }
        let due = now.duration_since(p.changed) >= auto.debounce
            || now.duration_since(p.since) >= auto.max_delay;
        if !due {
            continue;
        }
        let profile = match info.as_ref().map(recorded_profile) {
            Some(Ok(p)) => p,
            Some(Err(e)) => {
                tracing::warn!("auto-reason /{}: {e:#}", ds.name);
                p.failed = Some(head);
                continue;
            }
            None => continue,
        };
        tracing::info!("auto-reason /{}: re-running {}", ds.name, profile.name());
        let task = start_reason(st, ds.clone(), profile, true);
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
/// and the inferences' freshness at that snapshot.
#[cfg(feature = "reasoning")]
pub fn diagnostics_json(
    name: &str,
    store: &Store,
    info: Option<&ReasoningInfo>,
    opts: &sparkles_reasoner::diagnostics::DiagnoseOptions,
) -> anyhow::Result<(sparkles_reasoner::diagnostics::DiagnosticsReport, J)> {
    let snap = store.snapshot();
    let at = snap.commit;
    let report = sparkles_reasoner::diagnostics::diagnose(snap, opts)?;
    let mut j = report.to_json();
    j["dataset"] = name.into();
    if opts.inferences
        && let Some(info) = info
    {
        let f = freshness(info, store, at);
        let inf = &mut j["scope"]["inferences"];
        inf["profile"] = info.profile.clone().into();
        inf["stale"] = f.stale.into();
        inf["commitsSince"] = f.commits_since.into();
    }
    Ok((report, j))
}
