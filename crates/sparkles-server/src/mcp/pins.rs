//! Snapshots held for `atCommit`: the commit id is the handle that makes several
//! stateless calls read one state.
//!
//! Every call records the snapshot it read. A later call that names that commit gets
//! the same snapshot while the server holds it: at most [`PER_DATASET`] commits per
//! dataset and [`TOTAL`] overall (least recently used first out), each for [`IDLE`] after
//! its last use. A pin keeps its generation's files mapped, so these limits bound the
//! disk and memory an agent can keep alive.
//!
//! A commit the table does not hold is read from the dataset's retained history when
//! the dataset keeps that state (its named snapshots and retention window, F06). A call
//! may also select a state by time or by snapshot name (`at`).

use super::errors::ToolError;
use crate::state::Dataset;
use parking_lot::Mutex;
use sparkles::error::Error;
use sparkles::history::{At, HistoryOptions};
use sparkles::store::Snapshot;
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Weak};
use std::time::{Duration, Instant};

pub const PER_DATASET: usize = 4;
pub const TOTAL: usize = 32;
pub const IDLE: Duration = Duration::from_secs(600);

struct Pin {
    commit: u64,
    snap: Arc<Snapshot>,
    /// the dataset instance the snapshot belongs to: a dataset deleted and recreated
    /// under the same name never serves old pins
    owner: Weak<Dataset>,
    last_used: Instant,
}

#[derive(Default)]
pub struct Pins {
    inner: Mutex<HashMap<String, VecDeque<Pin>>>,
    /// test hook: added to the clock
    offset: Mutex<Duration>,
}

impl Pins {
    fn now(&self) -> Instant {
        Instant::now() + *self.offset.lock()
    }

    /// Test hook: move the pin clock forward.
    #[cfg(test)]
    pub fn advance(&self, d: Duration) {
        *self.offset.lock() += d;
    }

    /// The snapshot a call reads: the head without `at`, else the state `at` selects.
    /// A commit that is the head or still held here is served from memory. Any other
    /// commit is read from the dataset's retained history, when the dataset keeps that
    /// state. The snapshot is recorded (or its pin refreshed), so that the next call
    /// that names its commit is served from memory. `engine` maps the other errors of
    /// reading a past state, such as a timeout or a budget, to tool errors.
    pub fn resolve(
        &self,
        ds: &Arc<Dataset>,
        at: Option<&At>,
        ho: &HistoryOptions,
        engine: &dyn Fn(Error) -> ToolError,
    ) -> Result<Arc<Snapshot>, ToolError> {
        let head = ds.store.snapshot();
        let now = self.now();
        let c = match at {
            None | Some(At::Head) => None,
            Some(At::Commit(c)) => Some(*c),
            // a time or a snapshot name selects a commit of the catalog
            Some(sel) => match ds.store.resolve(sel) {
                Ok(r) => Some(r.commit.seq),
                Err(e) => return Err(history_error(ds, sel, head.commit, e, engine)),
            },
        };
        let c = {
            let mut inner = self.inner.lock();
            expire(&mut inner, now);
            let Some(c) = c.filter(|&c| c != head.commit) else {
                touch(&mut inner, ds, head.clone(), now);
                return Ok(head);
            };
            if let Some(p) = inner.get_mut(&ds.key()).and_then(|pins| {
                pins.iter_mut()
                    .find(|p| p.commit == c && p.owner.ptr_eq(&Arc::downgrade(ds)))
            }) {
                p.last_used = now;
                return Ok(p.snap.clone());
            }
            if c > head.commit {
                return Err(ToolError::new(
                    "unknown-commit",
                    404,
                    format!(
                        "commit {c} does not exist in dataset {} (head is {})",
                        ds.name, head.commit
                    ),
                )
                .hint("use the commit of an earlier result, or omit atCommit to read the head"));
            }
            c
        };
        // Not held here, but the dataset's history may keep the state. The table's lock
        // is not held while the state is materialized.
        let sel = At::Commit(c);
        let snap = match ds.store.snapshot_at(&sel, ho) {
            Ok((snap, _)) => snap,
            Err(e) => return Err(history_error(ds, &sel, head.commit, e, engine)),
        };
        let mut inner = self.inner.lock();
        touch(&mut inner, ds, snap.clone(), self.now());
        Ok(snap)
    }

    /// Whether commit `c` of `ds` is held (for tests).
    #[cfg(test)]
    pub fn holds(&self, ds: &Arc<Dataset>, c: u64) -> bool {
        self.inner.lock().get(&ds.key()).is_some_and(|pins| {
            pins.iter()
                .any(|p| p.commit == c && p.owner.ptr_eq(&Arc::downgrade(ds)))
        })
    }
}

fn expire(inner: &mut HashMap<String, VecDeque<Pin>>, now: Instant) {
    for pins in inner.values_mut() {
        pins.retain(|p| {
            now.saturating_duration_since(p.last_used) < IDLE && p.owner.strong_count() > 0
        });
    }
    inner.retain(|_, pins| !pins.is_empty());
}

fn touch(
    inner: &mut HashMap<String, VecDeque<Pin>>,
    ds: &Arc<Dataset>,
    snap: Arc<Snapshot>,
    now: Instant,
) {
    let owner = Arc::downgrade(ds);
    let pins = inner.entry(ds.key()).or_default();
    // pins of an older dataset instance under this name are dead
    pins.retain(|p| p.owner.ptr_eq(&owner));
    if let Some(p) = pins.iter_mut().find(|p| p.commit == snap.commit) {
        p.last_used = now;
        p.snap = snap;
    } else {
        pins.push_back(Pin {
            commit: snap.commit,
            snap,
            owner,
            last_used: now,
        });
    }
    while pins.len() > PER_DATASET {
        let oldest = lru(pins.iter());
        pins.remove(oldest);
    }
    // the overall limit: drop the least recently used pin of any dataset
    while inner.values().map(VecDeque::len).sum::<usize>() > TOTAL {
        let Some((name, i)) = inner
            .iter()
            .flat_map(|(n, pins)| {
                pins.iter()
                    .enumerate()
                    .map(move |(i, p)| (n, i, p.last_used))
            })
            .min_by_key(|(_, _, t)| *t)
            .map(|(n, i, _)| (n.clone(), i))
        else {
            break;
        };
        if let Some(pins) = inner.get_mut(&name) {
            pins.remove(i);
        }
        inner.retain(|_, pins| !pins.is_empty());
    }
}

fn lru<'a>(pins: impl Iterator<Item = &'a Pin>) -> usize {
    pins.enumerate()
        .min_by_key(|(_, p)| p.last_used)
        .map_or(0, |(i, _)| i)
}

/// The tool error for a state that cannot be read: a commit, time or snapshot that does
/// not exist, or a state the dataset no longer keeps.
fn history_error(
    ds: &Dataset,
    at: &At,
    head: u64,
    e: Error,
    engine: &dyn Fn(Error) -> ToolError,
) -> ToolError {
    match e {
        Error::NotFound(m) => {
            ToolError::new("unknown-commit", 404, format!("{m} in dataset {}", ds.name)).hint(
                "list the commits with list_commits, or omit at and atCommit to read the head",
            )
        }
        Error::HistoryGone(_) | Error::HistoryUnsupported(_) => {
            let what = match at {
                At::Commit(c) => format!("commit {c}"),
                other => other.to_string(),
            };
            let t = ToolError::new(
                "unknown-commit",
                410,
                format!(
                    "{what} is no longer held (head is {head}); rerun without atCommit — results may differ from earlier pages"
                ),
            );
            let readable: Vec<String> = ds
                .store
                .history()
                .reconstructable
                .iter()
                .filter(|r| r.0 < head)
                .map(|&(a, b)| match b.min(head) {
                    b if b == a => a.to_string(),
                    b => format!("{a} to {b}"),
                })
                .collect();
            if readable.is_empty() {
                t.hint("this dataset keeps no past states; an administrator can keep them with a retention window or a named snapshot")
            } else {
                t.hint(format!(
                    "this dataset still keeps commits {}",
                    readable.join(", ")
                ))
            }
        }
        e => engine(e),
    }
}
