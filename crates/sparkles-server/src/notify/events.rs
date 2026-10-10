//! The events of spec C21 §3.1 and when they fire.
//!
//! [`spawn`] checks the lasting conditions once a minute: the review branches of agent
//! memory that wait longer than a dataset's `memory.review.notifyAfter`, and the model
//! providers whose daily token budget is spent. [`backup_run`] reports the end of a
//! backup policy run.

use super::{Condition, Event, Raised, Severity, clear, raise, settings};
use crate::state::AppState;
use chrono::{DateTime, TimeDelta, Utc};
use serde_json::{Value, json};
use std::sync::Arc;
use std::time::Duration;

/// How often the conditions are checked.
const TICK: Duration = Duration::from_secs(60);
/// The branches an event lists.
const MAX_BRANCHES: usize = 10;

/// Check the conditions once a minute, from the first minute after the start.
pub fn spawn(st: Arc<AppState>) {
    let weak = Arc::downgrade(&st);
    drop(st);
    tokio::spawn(async move {
        let mut t = tokio::time::interval_at(tokio::time::Instant::now() + TICK, TICK);
        loop {
            t.tick().await;
            let Some(st) = weak.upgrade() else { return };
            let _ = tokio::task::spawn_blocking(move || check(&st, Utc::now())).await;
        }
    });
}

/// One check of the lasting conditions at `now`.
pub fn check(st: &Arc<AppState>, now: DateTime<Utc>) {
    if !settings(st).enabled {
        return;
    }
    for ds in st.datasets().values() {
        memory_review(st, &ds.name, now);
    }
    model_budgets(st, now);
}

fn delta(days: f64) -> TimeDelta {
    TimeDelta::milliseconds((days * 86_400_000.0) as i64)
}

/// The branch name prefixes of review work, as the memory inbox reads them.
fn review_kind(name: &str) -> Option<&'static str> {
    if name.starts_with("ingest.") || name.starts_with("ingest-") {
        Some("ingest")
    } else if name.starts_with("review.") {
        Some("review")
    } else if name.starts_with("consolidation.") {
        Some("consolidation")
    } else if name.starts_with("proposals.") || name.starts_with("proposals-") {
        Some(if name.contains("consolidat") {
            "consolidation"
        } else {
            "proposal"
        })
    } else {
        None
    }
}

fn days_text(d: TimeDelta) -> String {
    let h = d.num_hours();
    if h >= 48 {
        format!("{} days", h / 24)
    } else if h >= 2 {
        format!("{h} hours")
    } else {
        format!("{} minutes", d.num_minutes().max(1))
    }
}

/// `memory.review.pending` for the dataset `name` (§3.1): the oldest open review
/// branch is older than `notifyAfter`. The condition clears when no review branch is
/// open, and a dataset without `notifyAfter` has none.
pub fn memory_review(st: &Arc<AppState>, name: &str, now: DateTime<Utc>) -> Option<Raised> {
    let key = format!("memory.review.pending/{name}");
    let ds = st.get(name)?;
    let review = crate::assist::memory_settings(st, &ds).review;
    let Some(after) = review
        .as_ref()
        .and_then(|r| r.notify_after.as_deref())
        .and_then(crate::assist::duration_days)
    else {
        clear(st, &key);
        return None;
    };
    let mut open: Vec<_> = ds
        .store
        .branches()
        .ok()?
        .into_iter()
        .filter(|b| review_kind(&b.name).is_some() && b.ahead > 0)
        .collect();
    if open.is_empty() {
        clear(st, &key);
        return None;
    }
    open.sort_by_key(|b| b.created_ms);
    let oldest = &open[0];
    let created = DateTime::from_timestamp_millis(oldest.created_ms)?;
    let age = now - created;
    if age < delta(after) {
        return None;
    }
    let cfg = settings(st);
    let repeat = review
        .as_ref()
        .and_then(|r| r.repeat_every.as_deref())
        .and_then(crate::assist::duration_days)
        .map(delta)
        .unwrap_or_else(|| cfg.repeat());
    let n = open.len();
    let enc = |s: &str| crate::settings_cmd::enc(s);
    let ev = Event {
        kind: "memory.review.pending",
        dataset: Some(name.to_string()),
        severity: Severity::Warning,
        title: format!("Memory review waiting in {name}"),
        summary: format!(
            "{n} review branch{} in {name} wait{} for review. The oldest, {}, was created {} ago.",
            if n == 1 { "" } else { "es" },
            if n == 1 { "s" } else { "" },
            oldest.name,
            days_text(age)
        ),
        link: Some(format!("/ui/memory?ds={}&tab=inbox", enc(name))),
        data: json!({
            "open": n,
            "oldest": {
                "branch": oldest.name,
                "kind": review_kind(&oldest.name),
                "created": super::rfc3339(created),
            },
            "branches": open.iter().take(MAX_BRANCHES).map(|b| b.name.clone()).collect::<Vec<_>>(),
            "notifyAfter": review.as_ref().and_then(|r| r.notify_after.clone()),
            "repeatEvery": review.as_ref().and_then(|r| r.repeat_every.clone()).unwrap_or(cfg.repeat_every),
        }),
    };
    Some(raise(
        st,
        ev,
        Some(Condition {
            key,
            repeat: Some(repeat),
        }),
        now,
    ))
}

/// `models.budget.exceeded` (§3.1): a provider's tokens counted today reach its daily
/// budget. Once per provider until the count falls below the budget again, which it
/// does at midnight UTC.
pub fn model_budgets(st: &Arc<AppState>, now: DateTime<Utc>) {
    let Some(m) = st.models() else { return };
    let d = m.describe();
    for p in d["providers"].as_array().into_iter().flatten() {
        let Some(name) = p["name"].as_str() else {
            continue;
        };
        let key = format!("models.budget.exceeded/{name}");
        let (Some(cap), Some(used)) = (
            p["budget"]["tokensPerDay"].as_u64(),
            p["budget"]["usedToday"].as_u64(),
        ) else {
            clear(st, &key);
            continue;
        };
        if used < cap {
            clear(st, &key);
            continue;
        }
        let ev = Event {
            kind: "models.budget.exceeded",
            dataset: None,
            severity: Severity::Warning,
            title: format!("Model budget used up: {name}"),
            summary: format!(
                "Provider {name} has used {used} of its {cap} tokens for today. Requests to it fail until midnight UTC."
            ),
            link: Some("/ui/server".into()),
            data: json!({ "provider": name, "tokensPerDay": cap, "usedToday": used }),
        };
        raise(st, ev, Some(Condition { key, repeat: None }), now);
    }
}

/// The end of a backup policy run (§3.1): `backup.failed` for a failed or partial run,
/// held back for `repeatEvery` per policy, and the condition cleared by a successful
/// run.
#[cfg_attr(not(feature = "backup"), allow(dead_code))]
pub fn backup_run(
    st: &Arc<AppState>,
    policy: &str,
    repository: &str,
    result: &str,
    run: &Value,
    consecutive_failures: u64,
    now: DateTime<Utc>,
) -> Option<Raised> {
    let key = format!("backup.failed/{policy}");
    match result {
        "ok" => {
            clear(st, &key);
            return None;
        }
        "failed" | "partial" => {}
        _ => return None,
    }
    let failed: Vec<Value> = run["datasets"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|d| d["result"] == "failed")
        .map(|d| {
            let mut o = json!({ "dataset": d["dataset"] });
            if let Some(e) = d.get("reason").filter(|e| !e.is_null()) {
                o["reason"] = e.clone();
            }
            o
        })
        .collect();
    let names: Vec<&str> = failed
        .iter()
        .filter_map(|d| d["dataset"].as_str())
        .collect();
    let ev = Event {
        kind: "backup.failed",
        dataset: None,
        severity: if result == "failed" {
            Severity::Critical
        } else {
            Severity::Warning
        },
        title: format!("Backup policy {policy} {result}"),
        summary: if names.is_empty() {
            format!("The run of backup policy {policy} ended {result}.")
        } else {
            format!(
                "The run of backup policy {policy} ended {result}. It did not back up {}.",
                names.join(", ")
            )
        },
        link: Some("/ui/backups".into()),
        data: json!({
            "policy": policy,
            "run": run["id"],
            "result": result,
            "repository": repository,
            "failedDatasets": failed,
            "consecutiveFailures": consecutive_failures,
        }),
    };
    let repeat = settings(st).repeat();
    Some(raise(
        st,
        ev,
        Some(Condition {
            key,
            repeat: Some(repeat),
        }),
        now,
    ))
}
