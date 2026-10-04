//! Freshness of materialized inferences.
//!
//! A materialization records the commit (`seq`) it wrote, or the unchanged head it read
//! when it changed nothing, together with the dataset id and the graphs it read. The
//! inferences are fresh while no later commit changed one of those graphs (the default
//! graph alone, unless the run read others or followed imports). Commits to other
//! graphs, the inferred graph included, leave them fresh; `commits_since` still counts
//! every commit.

use super::ReasoningRecord;
use crate::store::Store;
use parking_lot::Mutex;
use serde::Serialize;
use std::collections::HashMap;

/// How the recorded inferences relate to a commit of the dataset.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct Freshness {
    /// `None`: unknown (no recorded position, or one from another dataset)
    pub stale: Option<bool>,
    /// the commits since the materialization, when they can be counted
    pub commits_since: Option<u64>,
    /// why the inferences are stale, or why their freshness is unknown
    pub reason: Option<String>,
}

/// The freshness of the inferences `record` describes at commit `at` of `store`.
pub fn freshness(record: &ReasoningRecord, store: &Store, at: u64) -> Freshness {
    let unknown = |why: &str| Freshness {
        stale: None,
        commits_since: None,
        reason: Some(why.to_string()),
    };
    if record.inherited_stale {
        return Freshness {
            stale: Some(true),
            commits_since: None,
            reason: Some("inherited from source at clone time".into()),
        };
    }
    let Some(commit) = record.commit else {
        return unknown("no recorded store position (materialized by an older version)");
    };
    if record.position_source.as_deref() != Some("commit") {
        return unknown("recorded position is not a commit");
    }
    if record.dataset_id.as_deref() != Some(store.dataset_id().to_string().as_str()) {
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
            // commits to graphs the run did not read, the inferred graph included,
            // leave the inferences fresh
            match inputs_changed(record, store, commit, at) {
                Ok(false) => Freshness {
                    stale: Some(false),
                    commits_since: Some(n),
                    reason: None,
                },
                Ok(true) => Freshness {
                    stale: Some(true),
                    commits_since: Some(n),
                    reason: Some(format!(
                        "{n} commit{} since materialization",
                        if n == 1 { "" } else { "s" }
                    )),
                },
                Err(e) => {
                    tracing::debug!("reading the changes since commit {commit}: {e}");
                    Freshness {
                        stale: Some(true),
                        commits_since: Some(n),
                        reason: Some(
                            "the changes since the materialization can no longer be read".into(),
                        ),
                    }
                }
            }
        }
        std::cmp::Ordering::Less => Freshness {
            stale: Some(true),
            commits_since: None,
            reason: Some("store position moved backwards".into()),
        },
    }
}

/// The graphs whose changes make `record`'s inferences stale: `default` or IRIs.
pub fn watched_graphs(record: &ReasoningRecord) -> Vec<String> {
    record
        .watched_graphs
        .clone()
        .unwrap_or_else(|| vec!["default".to_string()])
}

/// Whether a commit after `commit`, up to `at`, changed a graph the run read. The commit
/// flag answers for the default graph. For named graphs, the commit diff restricted to
/// each graph answers, and the answer is remembered by dataset and recorded commit, so
/// that each new head reads the commits since the last one asked about.
fn inputs_changed(
    record: &ReasoningRecord,
    store: &Store,
    commit: u64,
    at: u64,
) -> Result<bool, String> {
    let watched = watched_graphs(record);
    if watched.iter().any(|g| g == "default") && store.default_graph_changed(commit, at) {
        return Ok(true);
    }
    let named: Vec<&str> = watched
        .iter()
        .map(String::as_str)
        .filter(|g| *g != "default")
        .collect();
    if named.is_empty() {
        return Ok(false);
    }
    type Memo = HashMap<(String, u64, String), (u64, bool)>;
    static MEMO: std::sync::OnceLock<Mutex<Memo>> = std::sync::OnceLock::new();
    let memo = MEMO.get_or_init(Default::default);
    let key = (store.dataset_id().to_string(), commit, named.join("\n"));
    // the commits from `start` on still need reading
    let start = match memo.lock().get(&key).copied() {
        Some((to, changed)) if to == at || (changed && to <= at) => return Ok(changed),
        Some((to, false)) if to < at => to,
        _ => commit,
    };
    let mut changed = false;
    for g in &named {
        let o = crate::store::DiffOptions {
            graph: Some(oxrdf::GraphName::NamedNode(
                oxrdf::NamedNode::new_unchecked(*g),
            )),
            ..Default::default()
        };
        use crate::history::At;
        let d = store
            .diff(&At::Commit(start), &At::Commit(at), &o)
            .map_err(|e| e.to_string())?;
        if !d.is_empty() {
            changed = true;
            break;
        }
    }
    let mut m = memo.lock();
    if m.len() > 4096 {
        m.clear();
    }
    let later = m.get(&key).is_some_and(|(to, _)| *to > at);
    if !later {
        m.insert(key, (at, changed));
    }
    Ok(changed)
}

/// The reasoning record of a dataset with the freshness of its inferences at the head.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct ReasoningStatus {
    pub record: ReasoningRecord,
    /// the head commit
    pub head: u64,
    pub freshness: Freshness,
}

impl ReasoningStatus {
    /// The status of `record` at the head of `store`.
    pub fn of(record: ReasoningRecord, store: &Store) -> ReasoningStatus {
        let head = store.head_commit().seq;
        let freshness = freshness(&record, store, head);
        ReasoningStatus {
            record,
            head,
            freshness,
        }
    }
}
