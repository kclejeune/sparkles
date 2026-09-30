//! Snapshots held for `atCommit`: the commit id is the handle that makes several
//! stateless calls read one state.
//!
//! Every call records the snapshot it read. A later call that names that commit gets
//! the same snapshot while the server holds it: at most [`PER_DATASET`] commits per
//! dataset and [`TOTAL`] overall (least recently used first out), each for [`IDLE`] after
//! its last use. A pin keeps its generation's files mapped, so these limits bound the
//! disk and memory an agent can keep alive.

use super::errors::ToolError;
use crate::state::Dataset;
use parking_lot::Mutex;
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

    /// The snapshot a call reads: the head without `at`, else commit `at` if it is the
    /// head or still held. The snapshot is recorded (or its pin refreshed).
    pub fn resolve(&self, ds: &Arc<Dataset>, at: Option<u64>) -> Result<Arc<Snapshot>, ToolError> {
        let head = ds.store.snapshot();
        let now = self.now();
        let mut inner = self.inner.lock();
        expire(&mut inner, now);
        let Some(c) = at.filter(|&c| c != head.commit) else {
            touch(&mut inner, ds, head.clone(), now);
            return Ok(head);
        };
        if let Some(p) = inner.get_mut(&ds.name).and_then(|pins| {
            pins.iter_mut()
                .find(|p| p.commit == c && p.owner.ptr_eq(&Arc::downgrade(ds)))
        }) {
            p.last_used = now;
            return Ok(p.snap.clone());
        }
        Err(if c > head.commit {
            ToolError::new(
                "unknown-commit",
                404,
                format!(
                    "commit {c} does not exist in dataset {} (head is {})",
                    ds.name, head.commit
                ),
            )
            .hint("use the commit of an earlier result, or omit atCommit to read the head")
        } else {
            ToolError::new(
                "unknown-commit",
                410,
                format!(
                    "commit {c} is no longer held (head is {}); rerun without atCommit — results may differ from earlier pages",
                    head.commit
                ),
            )
        })
    }

    /// Whether commit `c` of `ds` is held (for tests).
    #[cfg(test)]
    pub fn holds(&self, ds: &Arc<Dataset>, c: u64) -> bool {
        self.inner.lock().get(&ds.name).is_some_and(|pins| {
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
    let pins = inner.entry(ds.name.clone()).or_default();
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
