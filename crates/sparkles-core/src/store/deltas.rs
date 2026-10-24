//! The id-level changes of commits ([`Store::commit_deltas`]), which incremental views
//! read (spec F13 §3.1).
//!
//! While a reader holds a [`DeltaSubscription`], every commit made through the
//! write-ahead log leaves its changes, as the ids of its generation, in a bounded ring in
//! memory before it is published. A reader that keeps up therefore gets each commit's
//! changes without decoding a term. Without a subscription the commit path does one
//! atomic load and nothing else. Commits the ring does not hold, such as bulk commits
//! and commits older than its capacity, are read from the change feed
//! ([`Store::changes`]), whose vocabulary keys are mapped to the ids of the current
//! generation.

use super::diff::QuadKey;
use super::*;
use rustc_hash::FxHashMap;
use std::collections::VecDeque;
use std::collections::hash_map::Entry;

/// The most commits the ring keeps.
const RING_COMMITS: usize = 4096;
/// The most changes the ring keeps, all its commits together.
const RING_CHANGES: usize = 4 << 20;

struct RingEntry {
    seq: u64,
    generation: Arc<Generation>,
    changes: Vec<(u8, [Id; 4])>,
}

/// Recent commits' changes, kept while someone subscribes.
#[derive(Default)]
pub(crate) struct Ring {
    subscribers: Arc<AtomicUsize>,
    inner: Mutex<RingInner>,
}

#[derive(Default)]
struct RingInner {
    entries: VecDeque<RingEntry>,
    changes: usize,
}

impl Ring {
    /// Keep commit `seq`'s changes `log` (in log order, ids of `generation`) when
    /// someone subscribes.
    pub(crate) fn noted(&self, seq: u64, generation: &Arc<Generation>, log: &[(u8, [Id; 4])]) {
        if self.subscribers.load(Ordering::Acquire) == 0 {
            return;
        }
        let mut inner = self.inner.lock();
        // a commit older than the newest kept one cannot follow it (a reopened writer)
        if inner.entries.back().is_some_and(|e| e.seq >= seq) {
            inner.entries.clear();
            inner.changes = 0;
        }
        inner.changes += log.len();
        inner.entries.push_back(RingEntry {
            seq,
            generation: generation.clone(),
            changes: log.to_vec(),
        });
        while inner.entries.len() > RING_COMMITS
            || (inner.changes > RING_CHANGES && inner.entries.len() > 1)
        {
            if let Some(e) = inner.entries.pop_front() {
                inner.changes -= e.changes.len();
            }
        }
    }
}

/// Keeps the ring of recent changes filled until it is dropped.
pub struct DeltaSubscription {
    subscribers: Arc<AtomicUsize>,
}

impl Drop for DeltaSubscription {
    fn drop(&mut self) {
        self.subscribers.fetch_sub(1, Ordering::AcqRel);
    }
}

/// One commit's net changes as ids of the page's generation.
#[derive(Clone, Debug)]
pub struct CommitDelta {
    pub seq: u64,
    /// the quads the commit removed
    pub removed: Vec<[Id; 4]>,
    /// the quads the commit added
    pub added: Vec<[Id; 4]>,
    /// `false` when the changes are not listed: a bulk commit listed with its counts
    /// only, or a change whose terms the current generation does not hold
    pub complete: bool,
}

/// A page of [`Store::commit_deltas`].
pub struct DeltaPage {
    pub after: u64,
    /// the snapshot whose generation the ids belong to, at the head when the page was read
    pub snapshot: Arc<Snapshot>,
    pub commits: Vec<CommitDelta>,
}

impl DeltaPage {
    /// The commit the next page starts after: the last one listed.
    pub fn next(&self) -> u64 {
        self.commits.last().map_or(self.after, |c| c.seq)
    }
}

/// The net effect of a log of changes: `true` adds the quad.
fn net(log: &[(u8, [Id; 4])]) -> FxHashMap<[Id; 4], bool> {
    let mut m: FxHashMap<[Id; 4], bool> = FxHashMap::default();
    for (op, q) in log {
        match m.entry(*q) {
            Entry::Occupied(e) => {
                e.remove();
            }
            Entry::Vacant(e) => {
                e.insert(*op == WAL_INSERT);
            }
        }
    }
    m
}

/// The vocabulary key of a stored id of `generation` (`None` for ids that are the same
/// in every generation: inline values and blank nodes).
fn gen_key(generation: &Generation, id: Id) -> Option<Vec<u8>> {
    match id.tag() {
        Tag::Vocab => generation.vocab.get(id.payload()),
        Tag::Delta => generation.dvocab.get(id.payload()),
        _ => None,
    }
}

/// An id of `from`'s generation as an id of `to`, or `None` when `to` lacks the term.
fn translate(from: &Generation, to: &Snapshot, id: Id) -> Option<Id> {
    match id.tag() {
        Tag::Vocab | Tag::Delta => to.lookup_key(&gen_key(from, id)?),
        _ => Some(id),
    }
}

/// A quad key's id in `snap` (the default graph's key is empty).
fn key_id(snap: &Snapshot, k: &[u8], graph: bool) -> Option<Id> {
    if graph && k.is_empty() {
        return Some(Id::DEFAULT_GRAPH);
    }
    let t = crate::id::key_to_term(k);
    snap.lookup_term(&t)
}

fn key_quad(snap: &Snapshot, k: &QuadKey) -> Option<[Id; 4]> {
    Some([
        key_id(snap, &k[1], false)?,
        key_id(snap, &k[2], false)?,
        key_id(snap, &k[3], false)?,
        key_id(snap, &k[0], true)?,
    ])
}

impl Store {
    /// Keep the changes of the commits made from now on in memory, as ids, until the
    /// subscription is dropped, so that [`commit_deltas`](Self::commit_deltas) reads
    /// them without decoding terms.
    pub fn subscribe_deltas(&self) -> DeltaSubscription {
        self.deltas.subscribers.fetch_add(1, Ordering::AcqRel);
        DeltaSubscription {
            subscribers: self.deltas.subscribers.clone(),
        }
    }

    /// The commits after commit `after` with their net changes as ids of the current
    /// generation, in order (spec F13 §3.1). A page holds at most `o.max_commits`
    /// commits and is read from the ring of recent changes when it holds them, and from
    /// the change feed ([`changes`](Self::changes)) otherwise. A commit whose changes are
    /// not listed, or whose terms the current generation lacks, is `complete: false`, and
    /// a reader then compares states instead. The access view of `o.graphs` is not
    /// applied: a reader filters the changes itself.
    pub fn commit_deltas(&self, after: u64, o: &ChangesOptions) -> Result<DeltaPage> {
        let snap = self.snapshot();
        let head = snap.commit;
        let mut page = DeltaPage {
            after,
            snapshot: snap.clone(),
            commits: Vec::new(),
        };
        if after >= head {
            return Ok(page);
        }
        let max = o.max_commits.max(1);
        {
            let inner = self.deltas.inner.lock();
            if let Some(first) = inner.entries.front().map(|e| e.seq)
                && first <= after + 1
            {
                for e in inner.entries.iter() {
                    if e.seq <= after + page.commits.len() as u64 {
                        continue;
                    }
                    if e.seq != after + 1 + page.commits.len() as u64
                        || e.seq > head
                        || page.commits.len() >= max
                    {
                        break;
                    }
                    let same = e.generation.uid == snap.generation.uid;
                    let mut d = CommitDelta {
                        seq: e.seq,
                        removed: Vec::new(),
                        added: Vec::new(),
                        complete: true,
                    };
                    for (q, add) in net(&e.changes) {
                        let q = if same {
                            Some(q)
                        } else {
                            (|| {
                                Some([
                                    translate(&e.generation, &snap, q[0])?,
                                    translate(&e.generation, &snap, q[1])?,
                                    translate(&e.generation, &snap, q[2])?,
                                    if q[3] == Id::DEFAULT_GRAPH {
                                        Id::DEFAULT_GRAPH
                                    } else {
                                        translate(&e.generation, &snap, q[3])?
                                    },
                                ])
                            })()
                        };
                        match q {
                            Some(q) if add => d.added.push(q),
                            Some(q) => d.removed.push(q),
                            None => d.complete = false,
                        }
                    }
                    page.commits.push(d);
                }
            }
        }
        if !page.commits.is_empty() {
            return Ok(page);
        }
        let changes = self.changes(
            after,
            &ChangesOptions {
                max_commits: max,
                ..o.clone()
            },
        )?;
        for c in changes.commits {
            if c.commit.seq > head {
                break;
            }
            let mut d = CommitDelta {
                seq: c.commit.seq,
                removed: Vec::new(),
                added: Vec::new(),
                complete: c.complete(),
            };
            for (k, op) in c.keys() {
                match key_quad(&snap, k) {
                    Some(q) if op == DiffOp::Add => d.added.push(q),
                    Some(q) => d.removed.push(q),
                    None => d.complete = false,
                }
            }
            page.commits.push(d);
        }
        Ok(page)
    }
}

impl Snapshot {
    /// This snapshot as it was before the changes `added` and `removed` (net changes that
    /// lead to it, as ids of its generation): a view for reading the state before a
    /// commit without materializing it from history. Its caches start empty and it
    /// reads as commit `commit`. Changes that do not fit this snapshot, such as an
    /// addition it does not hold, are left out.
    pub fn before_changes(
        &self,
        added: &[[Id; 4]],
        removed: &[[Id; 4]],
        commit: u64,
    ) -> Result<Snapshot> {
        let mut delta = self.delta.clone();
        let spo = Perm::Spo.index();
        for q in added {
            let k = Perm::Spo.to_key(q);
            if delta.ins[spo].contains(&k) {
                apply(&mut delta, q, false, false);
            } else if !delta.del[spo].contains(&k)
                && self.perm(Perm::Spo).contains(&self.cache, &k)?
            {
                apply(&mut delta, q, false, true);
            }
        }
        for q in removed {
            let k = Perm::Spo.to_key(q);
            if delta.del[spo].contains(&k) {
                apply(&mut delta, q, true, true);
            } else if !delta.ins[spo].contains(&k)
                && !self.perm(Perm::Spo).contains(&self.cache, &k)?
            {
                apply(&mut delta, q, true, false);
            }
        }
        Ok(Snapshot {
            delta,
            counts: Default::default(),
            cacheable: false,
            historical: true,
            mask: None,
            commit,
            ..self.clone()
        })
    }
}
