//! Net differences between two readable commits ([`Store::diff`]).
//!
//! Every change a WAL records took effect: an insert is logged only for an absent quad
//! and a delete only for a present one. The net difference between two commits is then
//! the symmetric difference of the changes between them: each change toggles its quad in
//! a map of net changes, and a quad changed back cancels out. Memory is proportional to
//! the quads that changed, and nothing outside the log range is read.
//!
//! A range of commits is walked through the WALs of the retained generations. A
//! compaction does not change the data, so the walk continues in the next generation's
//! log from its base. A bulk commit has no WAL records, and history can have gaps where
//! a generation was collected: those stretches are compared state against state, by
//! translating the older state's quads into the newer generation's ids, sorting them, and
//! merging them with a sorted scan of the newer state. In-memory stores compare the
//! retained snapshots: two states of one generation differ only in their deltas.
//!
//! Generation ids are local to a generation, so the result holds every quad as the
//! vocabulary keys of its terms, which every generation shares.

use super::*;
use crate::history::{At, Resolved};
use rustc_hash::FxHashMap;
use std::collections::hash_map::Entry;
use std::time::Instant;

/// A quad as the vocabulary keys of its graph, subject, predicate and object (the
/// default graph's key is empty). Ordered by graph, then subject, predicate, object.
pub(crate) type QuadKey = [Arc<[u8]>; 4];

/// How a quad changed between `from` and `to`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DiffOp {
    /// in `to`, not in `from`
    Add,
    /// in `from`, not in `to`
    Remove,
}

impl DiffOp {
    /// `+` or `-`
    pub fn sign(self) -> char {
        match self {
            DiffOp::Add => '+',
            DiffOp::Remove => '-',
        }
    }
}

/// How a diff was computed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiffMethod {
    /// the two commits are the same state
    Same,
    /// from the write-ahead logs alone
    Log,
    /// at least one stretch compared state against state (a bulk commit, a gap in the
    /// retained history, or an in-memory store)
    Compare,
}

impl DiffMethod {
    pub fn as_str(self) -> &'static str {
        match self {
            DiffMethod::Same => "same",
            DiffMethod::Log => "log",
            DiffMethod::Compare => "compare",
        }
    }
}

/// Options of [`Store::diff`].
#[derive(Clone, Default)]
pub struct DiffOptions {
    /// only the changes in this graph
    pub graph: Option<GraphName>,
    /// The most quads the net change, and the sort buffer of a state comparison, may
    /// hold (0: no limit). Beyond it the diff fails with a `rows` budget error.
    pub max_quads: u64,
    pub cancel: Option<Arc<AtomicBool>>,
    pub deadline: Option<Instant>,
    /// The graphs the caller may read (`None`: every graph). Changes in other graphs are
    /// left out before they count against `max_quads`.
    pub graphs: Option<Arc<crate::access::GraphAccess>>,
}

impl DiffOptions {
    pub(super) fn check(&self) -> Result<()> {
        if self
            .cancel
            .as_ref()
            .is_some_and(|c| c.load(Ordering::Relaxed))
        {
            return Err(Error::Cancelled);
        }
        if self.deadline.is_some_and(|t| Instant::now() > t) {
            return Err(Error::Timeout);
        }
        Ok(())
    }

    fn over(&self, n: u64) -> Result<()> {
        if self.max_quads > 0 && n > self.max_quads {
            return Err(Error::BudgetExceeded(crate::Budget {
                kind: crate::BudgetKind::Rows,
                limit: self.max_quads,
                requested: n,
            }));
        }
        Ok(())
    }
}

/// The net difference between two commits: what `to` has that `from` lacks (added) and
/// the reverse (removed). Changes are ordered removals first, then additions, each by
/// graph, subject, predicate and object.
pub struct Diff {
    pub from: Resolved,
    pub to: Resolved,
    pub added: u64,
    pub removed: u64,
    pub method: DiffMethod,
    /// WAL changes read
    pub log_changes: u64,
    /// quads read by state comparisons
    pub compared: u64,
    pub(super) changes: Vec<(QuadKey, DiffOp)>,
}

impl Diff {
    pub fn len(&self) -> usize {
        self.changes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.changes.is_empty()
    }

    /// The changes as quads, in order.
    pub fn iter(&self) -> impl Iterator<Item = (DiffOp, Quad)> + '_ {
        self.changes
            .iter()
            .filter_map(|(k, op)| key_quad(k).map(|q| (*op, q)))
    }

    /// The changes from position `start`, at most `n` of them.
    pub fn slice(&self, start: usize, n: usize) -> impl Iterator<Item = (DiffOp, Quad)> + '_ {
        self.changes
            .iter()
            .skip(start)
            .take(n)
            .filter_map(|(k, op)| key_quad(k).map(|q| (*op, q)))
    }
}

/// The quad of a key (`None` for keys that no quad can have).
pub(super) fn key_quad(k: &QuadKey) -> Option<Quad> {
    let s = match crate::id::key_to_term(&k[1]) {
        Term::NamedNode(n) => NamedOrBlankNode::NamedNode(n),
        Term::BlankNode(b) => NamedOrBlankNode::BlankNode(b),
        _ => return None,
    };
    let Term::NamedNode(p) = crate::id::key_to_term(&k[2]) else {
        return None;
    };
    let o = crate::id::key_to_term(&k[3]);
    let g = if k[0].is_empty() {
        GraphName::DefaultGraph
    } else {
        match crate::id::key_to_term(&k[0]) {
            Term::NamedNode(n) => GraphName::NamedNode(n),
            Term::BlankNode(b) => GraphName::BlankNode(b),
            _ => return None,
        }
    };
    Some(Quad::new(s, p, o, g))
}

/// The vocabulary key of a graph name (empty for the default graph).
fn graph_key(g: &GraphName) -> Vec<u8> {
    match g {
        GraphName::DefaultGraph => Vec::new(),
        GraphName::NamedNode(n) => crate::id::iri_key(n.as_str()),
        GraphName::BlankNode(b) => crate::id::term_key(&Term::BlankNode(b.clone())),
    }
}

/// The id of a graph name in a generation (its whole delta vocabulary included).
fn graph_id(g: &Generation, name: &GraphName) -> Option<Id> {
    match name {
        GraphName::DefaultGraph => Some(Id::DEFAULT_GRAPH),
        GraphName::BlankNode(b) => parse_bnode_label(b.as_str()),
        GraphName::NamedNode(_) => {
            let key = graph_key(name);
            g.vocab
                .find(&key)
                .ok()
                .map(Id::vocab)
                .or_else(|| g.dvocab.find(&key).map(Id::delta))
        }
    }
}

/// Turns one generation's ids into vocabulary keys, remembering the ones it has seen.
pub(super) struct Keys<'a> {
    generation: &'a Generation,
    seen: FxHashMap<Id, Arc<[u8]>>,
}

impl<'a> Keys<'a> {
    pub(super) fn new(generation: &'a Generation) -> Keys<'a> {
        Keys {
            generation,
            seen: FxHashMap::default(),
        }
    }

    fn key(&mut self, id: Id) -> Result<Arc<[u8]>> {
        if let Some(k) = self.seen.get(&id) {
            return Ok(k.clone());
        }
        let g = self.generation;
        let key: Vec<u8> = match id.tag() {
            Tag::Vocab => g.vocab.get(id.payload()).ok_or_else(|| dangling(id))?,
            Tag::Delta => g.dvocab.get(id.payload()).ok_or_else(|| dangling(id))?,
            Tag::BNode => {
                let mut k = vec![b'_'];
                k.extend_from_slice(&id.payload().to_be_bytes());
                k
            }
            Tag::Special if id == Id::DEFAULT_GRAPH => Vec::new(),
            _ => {
                let l = crate::id::inline_to_literal(id).ok_or_else(|| dangling(id))?;
                let mut k = Vec::new();
                crate::id::write_literal_key(&l, &mut k);
                k
            }
        };
        let key: Arc<[u8]> = key.into();
        // the terms of a large diff need not all stay cached
        if self.seen.len() >= 1 << 20 {
            self.seen.clear();
        }
        self.seen.insert(id, key.clone());
        Ok(key)
    }

    /// The key of a quad (`[s, p, o, g]` ids).
    pub(super) fn quad(&mut self, q: &[Id; 4]) -> Result<QuadKey> {
        Ok([
            self.key(q[3])?,
            self.key(q[0])?,
            self.key(q[1])?,
            self.key(q[2])?,
        ])
    }
}

fn dangling(id: Id) -> Error {
    Error::Corrupt(format!("dangling id {id:?} in a diff"))
}

/// The net changes found so far: each effective change toggles its quad.
struct Net<'o> {
    map: FxHashMap<QuadKey, bool>,
    opts: &'o DiffOptions,
    /// whether each graph (by key) is readable, under `opts.graphs`
    readable: FxHashMap<Arc<[u8]>, bool>,
}

/// Whether the graph of a quad key may be read under `access`, remembered per graph in
/// `memo`.
pub(crate) fn readable_key(
    access: &crate::access::GraphAccess,
    memo: &mut FxHashMap<Arc<[u8]>, bool>,
    k: &QuadKey,
) -> bool {
    if let Some(ok) = memo.get(&k[0]) {
        return *ok;
    }
    let ok = if k[0].is_empty() {
        access.read.default_graph()
    } else {
        access.readable(Some(&crate::id::key_to_term(&k[0])))
    };
    memo.insert(k[0].clone(), ok);
    ok
}

/// Whether a quad key is visible under `access`: its graph is read, and (for rules
/// that depend on the quad alone) no protection hides it.
pub(crate) fn visible_key(
    access: &crate::access::GraphAccess,
    memo: &mut FxHashMap<Arc<[u8]>, bool>,
    k: &QuadKey,
) -> bool {
    if !access.read.is_all() && !readable_key(access, memo, k) {
        return false;
    }
    // rules that depend on the data are applied to the states compared instead
    match access
        .triples
        .as_ref()
        .filter(|t| t.hides() && t.state_independent())
    {
        Some(t) => {
            let oxrdf::Term::NamedNode(p) = crate::id::key_to_term(&k[2]) else {
                return true;
            };
            let g = (!k[0].is_empty()).then(|| crate::id::key_to_term(&k[0]));
            !t.hides_quad(p.as_str(), g.as_ref())
        }
        None => true,
    }
}

impl Net<'_> {
    fn toggle(&mut self, k: QuadKey, added: bool) -> Result<()> {
        if let Some(a) = self.opts.graphs.as_ref().filter(|a| !a.reads_everything())
            && !visible_key(a, &mut self.readable, &k)
        {
            return Ok(());
        }
        match self.map.entry(k) {
            Entry::Occupied(e) => {
                // a change back: the quad is as it was at `from`
                debug_assert_ne!(*e.get(), added);
                e.remove();
            }
            Entry::Vacant(e) => {
                e.insert(added);
                if self.map.len() & 0xFFFF == 0 {
                    self.opts.over(self.map.len() as u64)?;
                    self.opts.check()?;
                }
            }
        }
        Ok(())
    }
}

/// One stretch of a range of commits.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Step {
    /// walk generation `generation`'s log from after commit `after` through `through`
    Log {
        generation: u32,
        after: u64,
        through: u64,
    },
    /// compare state `a` with state `b`
    Compare { a: u64, b: u64 },
}

/// The stretches that lead from commit `lo` to commit `hi` (`lo < hi`, both readable):
/// the logs of the retained generations where they cover the range, state comparisons
/// across bulk commits and gaps. `gens` holds `(number, base, end)` of every retained
/// generation.
fn plan(gens: &[(u32, u64, u64)], lo: u64, hi: u64) -> Vec<Step> {
    let mut steps = Vec::new();
    let mut s = lo;
    while s < hi {
        // the log that reaches furthest from `s` (its base at or before `s`)
        let best = gens
            .iter()
            .filter(|&&(_, b, e)| b <= s && s < e)
            .max_by_key(|&&(no, _, e)| (e, no));
        match best {
            Some(&(no, _, e)) => {
                let through = e.min(hi);
                steps.push(Step::Log {
                    generation: no,
                    after: s,
                    through,
                });
                s = through;
            }
            None => {
                // compare up to the next generation's base (readable), or `hi`
                let next = gens
                    .iter()
                    .map(|&(_, b, _)| b)
                    .filter(|&b| b > s && b <= hi)
                    .min()
                    .unwrap_or(hi);
                steps.push(Step::Compare { a: s, b: next });
                s = next;
            }
        }
    }
    steps
}

/// Read the changes of the commits after `after` through `through` from a generation's
/// WAL, streaming from the cursor (at or before the end of `after`). The commits are
/// numbered as [`replay_wal`] numbers them. A checksum mismatch, or a log that ends
/// before `through`, is [`Error::Corrupt`].
fn walk_wal(
    c: &mut super::wal::WalCursor,
    after: u64,
    through: u64,
    o: &DiffOptions,
    f: &mut dyn FnMut(u8, [Id; 4]) -> Result<()>,
) -> Result<u64> {
    if after >= through {
        return Ok(0);
    }
    let mut changes = 0u64;
    loop {
        let Some((seq, txn)) = c.next()? else {
            return Err(Error::Corrupt(format!(
                "{}: the log ends before commit {through}",
                c.path().display()
            )));
        };
        if seq > after && seq <= through {
            for d in txn.as_chunks::<WAL_REC>().0 {
                f(d[0], super::wal::record_quad(d))?;
                changes += 1;
                if changes & 0xFFFF == 0 {
                    o.check()?;
                }
            }
        }
        if seq >= through {
            return Ok(changes);
        }
    }
}

/// Add the difference between states `a` and `b` (as changes from `a` to `b`) to `net`.
/// Returns the quads read.
fn compare_states(a: &Snapshot, b: &Snapshot, net: &mut Net<'_>) -> Result<u64> {
    let opts = net.opts;
    if Arc::ptr_eq(&a.generation, &b.generation) {
        return compare_deltas(a, b, net);
    }
    let (ga, gb) = (&a.generation, &b.generation);
    let prefix_a: Vec<u64>;
    let prefix_b: Vec<u64>;
    match &opts.graph {
        Some(g) => {
            let (ia, ib) = (graph_id(ga, g), graph_id(gb, g));
            if ia.is_none() && ib.is_none() {
                return Ok(0);
            }
            // a graph one side does not know is empty there
            prefix_a = vec![ia.map_or(u64::MAX, |i| i.0)];
            prefix_b = vec![ib.map_or(u64::MAX, |i| i.0)];
        }
        None => {
            prefix_a = Vec::new();
            prefix_b = Vec::new();
        }
    }
    let mut keys_a = Keys::new(ga);
    let mut keys_b = Keys::new(gb);
    // a's quads in b's ids, as GSPO keys; quads with a term b does not know are removed
    let mut map: FxHashMap<Id, Option<Id>> = FxHashMap::default();
    let mut sorted: Vec<Key> = Vec::new();
    let mut read = 0u64;
    let mut gone: Vec<[Id; 4]> = Vec::new();
    let mut err: Option<Error> = None;
    a.scan(Perm::Gspo, &prefix_a, |c| {
        let mut visit = |k: &Key| -> Result<()> {
            read += 1;
            if read & 0xFFFF == 0 {
                opts.check()?;
            }
            let q = Perm::Gspo.to_quad(k);
            let mut t = [Id::UNDEF; 4];
            for i in 0..4 {
                let id = q[i];
                t[i] = if matches!(id.tag(), Tag::Vocab | Tag::Delta) {
                    let m = match map.get(&id) {
                        Some(m) => *m,
                        None => {
                            let key = a.key(id).ok_or_else(|| dangling(id))?;
                            let m = b.lookup_key(&key);
                            if map.len() >= 1 << 22 {
                                map.clear();
                            }
                            map.insert(id, m);
                            m
                        }
                    };
                    match m {
                        Some(m) => m,
                        None => {
                            gone.push(q);
                            return Ok(());
                        }
                    }
                } else {
                    id
                };
            }
            sorted.push(Perm::Gspo.to_key(&t));
            opts.over(sorted.len() as u64)?;
            Ok(())
        };
        let r = match c {
            Chunk::Block(blk, s, e) => (s..e).try_for_each(|i| visit(&blk.key(i))),
            Chunk::Row(k) => visit(&k),
        };
        match r {
            Ok(()) => Ok(true),
            Err(e) => {
                err = Some(e);
                Ok(false)
            }
        }
    })?;
    if let Some(e) = err {
        return Err(e);
    }
    for q in gone {
        let k = keys_a.quad(&q)?;
        net.toggle(k, false)?;
    }
    sorted.sort_unstable();
    // merge with b's sorted scan
    let mut i = 0usize;
    let mut err: Option<Error> = None;
    b.scan(Perm::Gspo, &prefix_b, |c| {
        let mut visit = |k: Key| -> Result<()> {
            read += 1;
            if read & 0xFFFF == 0 {
                opts.check()?;
            }
            while i < sorted.len() && sorted[i] < k {
                let q = keys_b.quad(&Perm::Gspo.to_quad(&sorted[i]))?;
                net.toggle(q, false)?;
                i += 1;
            }
            if i < sorted.len() && sorted[i] == k {
                i += 1;
            } else {
                let q = keys_b.quad(&Perm::Gspo.to_quad(&k))?;
                net.toggle(q, true)?;
            }
            Ok(())
        };
        let r = match c {
            Chunk::Block(blk, s, e) => (s..e).try_for_each(|j| visit(blk.key(j))),
            Chunk::Row(k) => visit(k),
        };
        match r {
            Ok(()) => Ok(true),
            Err(e) => {
                err = Some(e);
                Ok(false)
            }
        }
    })?;
    if let Some(e) = err {
        return Err(e);
    }
    for k in &sorted[i..] {
        let q = keys_b.quad(&Perm::Gspo.to_quad(k))?;
        net.toggle(q, false)?;
    }
    Ok(read)
}

/// The net changes from state `a` to state `b`, removals first, each by graph, subject,
/// predicate and object. A state comparison under `o`'s budget and deadline (a write
/// preview of a bulk write).
pub(super) fn compare_changes(
    a: &Snapshot,
    b: &Snapshot,
    o: &DiffOptions,
) -> Result<Vec<(DiffOp, Quad)>> {
    let mut net = Net {
        map: FxHashMap::default(),
        opts: o,
        readable: FxHashMap::default(),
    };
    compare_states(a, b, &mut net)?;
    o.over(net.map.len() as u64)?;
    let mut changes: Vec<(QuadKey, bool)> = net.map.into_iter().collect();
    changes.sort_unstable_by(|x, y| (x.1, &x.0).cmp(&(y.1, &y.0)));
    Ok(changes
        .into_iter()
        .filter_map(|(k, added)| {
            let op = if added { DiffOp::Add } else { DiffOp::Remove };
            key_quad(&k).map(|q| (op, q))
        })
        .collect())
}

/// Two states of one generation differ only where their deltas do.
fn compare_deltas(a: &Snapshot, b: &Snapshot, net: &mut Net<'_>) -> Result<u64> {
    let spo = Perm::Spo.index();
    let gid = match &net.opts.graph {
        Some(g) => match graph_id(&a.generation, g) {
            Some(i) => Some(i),
            None => return Ok(0),
        },
        None => None,
    };
    let mut keys = Keys::new(&a.generation);
    let mut seen: rustc_hash::FxHashSet<Key> = Default::default();
    let mut read = 0u64;
    let sets = [
        &a.delta.ins[spo],
        &a.delta.del[spo],
        &b.delta.ins[spo],
        &b.delta.del[spo],
    ];
    for set in sets {
        for k in set.iter() {
            read += 1;
            if !seen.insert(*k) {
                continue;
            }
            let q = Perm::Spo.to_quad(k);
            if gid.is_some_and(|g| q[3] != g) {
                continue;
            }
            let (pa, pb) = (a.contains(&q)?, b.contains(&q)?);
            if pa != pb {
                net.toggle(keys.quad(&q)?, pb)?;
            }
        }
    }
    Ok(read)
}

impl Store {
    /// The net difference between the states at `from` and `to`, two readable commits
    /// in either order. Within the retained generations' logs it reads only the changes
    /// between the two commits; across a bulk commit, a gap in the retained history, or
    /// in an in-memory store it compares the two states.
    pub fn diff(&self, from: &At, to: &At, o: &DiffOptions) -> Result<Diff> {
        let head = self.head_commit();
        let rf = self.resolve_with(from, head)?;
        let rt = self.resolve_with(to, head)?;
        let (a, b) = (rf.commit.seq, rt.commit.seq);
        let (lo, hi) = (a.min(b), a.max(b));
        let mut net = Net {
            map: FxHashMap::default(),
            opts: o,
            readable: FxHashMap::default(),
        };
        let (mut log_changes, mut compared) = (0u64, 0u64);
        let mut method = DiffMethod::Same;
        // protections that depend on the data: the difference of the two states as the
        // view sees them, so that triples whose visibility changed count too
        let data_view = o.graphs.as_ref().filter(|a| {
            a.triples
                .as_ref()
                .is_some_and(|t| t.hides() && !t.state_independent())
        });
        if lo != hi
            && let Some(view) = data_view
        {
            for (r, at) in [(&rf, from), (&rt, to)] {
                self.check_readable(r, at)?;
            }
            method = DiffMethod::Compare;
            let (sa, _) = self.snapshot_at(&At::Commit(lo), &self.history_opts(o))?;
            let (sb, _) = self.snapshot_at(&At::Commit(hi), &self.history_opts(o))?;
            let (sa, sb) = (view.masked(&sa)?, view.masked(&sb)?);
            compared += compare_states(&sa, &sb, &mut net)?;
        } else if lo != hi {
            if self.root.is_some() {
                // both ends must be readable, whatever path leads between them
                for (r, at) in [(&rf, from), (&rt, to)] {
                    self.check_readable(r, at)?;
                }
                let steps = self.diff_plan(lo, hi)?;
                method = DiffMethod::Log;
                for step in steps {
                    match step {
                        Step::Log {
                            generation,
                            after,
                            through,
                        } => {
                            log_changes +=
                                self.diff_log(generation, after, through, o, &mut net)?;
                        }
                        Step::Compare { a, b } => {
                            method = DiffMethod::Compare;
                            compared += self.diff_compare(a, b, o, &mut net)?;
                        }
                    }
                }
            } else {
                method = DiffMethod::Compare;
                let (sa, _) = self.snapshot_at(&At::Commit(lo), &self.history_opts(o))?;
                let (sb, _) = self.snapshot_at(&At::Commit(hi), &self.history_opts(o))?;
                compared += compare_states(&sa, &sb, &mut net)?;
            }
        }
        o.over(net.map.len() as u64)?;
        // the changes were collected from the older commit to the newer one
        let flip = a > b;
        let mut changes: Vec<(QuadKey, DiffOp)> = net
            .map
            .into_iter()
            .map(|(k, added)| {
                let op = if added != flip {
                    DiffOp::Add
                } else {
                    DiffOp::Remove
                };
                (k, op)
            })
            .collect();
        changes
            .sort_unstable_by(|x, y| (x.1 == DiffOp::Add, &x.0).cmp(&(y.1 == DiffOp::Add, &y.0)));
        let added = changes.iter().filter(|c| c.1 == DiffOp::Add).count() as u64;
        Ok(Diff {
            removed: changes.len() as u64 - added,
            added,
            from: rf,
            to: rt,
            method,
            log_changes,
            compared,
            changes,
        })
    }

    fn history_opts(&self, o: &DiffOptions) -> crate::history::HistoryOptions {
        crate::history::HistoryOptions {
            cancel: o.cancel.clone(),
            deadline: o.deadline,
        }
    }

    /// `HistoryGone` unless some retained generation covers the resolved commit.
    fn check_readable(&self, r: &Resolved, at: &At) -> Result<()> {
        let Some(hist) = &self.history else {
            return Ok(());
        };
        let current = commit::generation_number(&self.snapshot().generation.name);
        let h = hist.lock();
        if h.owner(r.commit.seq, current, r.head).is_none() {
            let name = match at {
                At::Snapshot(n) => Some(n.clone()),
                _ => None,
            };
            return Err(self.history_gone(&h, r.commit.seq, r.head, name, Some(r.commit)));
        }
        Ok(())
    }

    pub(super) fn diff_plan(&self, lo: u64, hi: u64) -> Result<Vec<Step>> {
        let Some(hist) = &self.history else {
            return Ok(vec![Step::Compare { a: lo, b: hi }]);
        };
        let live = self.snapshot();
        let current = commit::generation_number(&live.generation.name);
        let head = self.head_commit().seq;
        let h = hist.lock();
        let gens: Vec<(u32, u64, u64)> = h
            .gens
            .iter()
            .map(|(&no, g)| (no, g.base.seq, if no == current { head } else { g.end }))
            .collect();
        Ok(plan(&gens, lo, hi))
    }

    /// Open generation `no`'s log to read the commits after `after`: the generation, and
    /// a cursor at the nearest indexed position at or before the end of `after`. The
    /// generation and its log are opened under the history lock: collection takes the
    /// same lock, and an open file outlives an unlink.
    pub(super) fn open_log(
        &self,
        no: u32,
        after: u64,
    ) -> Result<(Arc<Generation>, super::wal::WalCursor)> {
        let hist = self.history.as_ref().expect("a persistent store");
        let (generation, entry, file) = {
            let live = self.snapshot();
            let current = commit::generation_number(&live.generation.name);
            let mut h = hist.lock();
            let Some(entry) = h.gens.get(&no).cloned() else {
                return Err(Error::Corrupt(format!(
                    "generation {no} was collected while it was read; retry"
                )));
            };
            let generation = self.history_generation(&mut h, no, current, &live, &entry)?;
            let file = File::open(entry.dir.join("wal.log"))?;
            (generation, entry, file)
        };
        let path = entry.dir.join("wal.log");
        let from = {
            let mut ix = generation.wal_index.lock();
            if ix.is_none() {
                *ix = Some(super::wal::WalIndex::scan(
                    &path,
                    entry.base.seq,
                    entry.fold_legacy,
                )?);
            }
            ix.as_ref().map(|ix| ix.floor(after)).expect("built above")
        };
        let cursor = super::wal::WalCursor::new(file, &path, from)?;
        Ok((generation, cursor))
    }

    /// Walk generation `no`'s log from after `after` through `through` into `net`.
    fn diff_log(
        &self,
        no: u32,
        after: u64,
        through: u64,
        o: &DiffOptions,
        net: &mut Net<'_>,
    ) -> Result<u64> {
        let (generation, mut cursor) = self.open_log(no, after)?;
        let gid = match &o.graph {
            Some(g) => match graph_id(&generation, g) {
                Some(i) => Some(i),
                // no change in this generation can touch a graph it never named
                None => return Ok(0),
            },
            None => None,
        };
        let mut local: FxHashMap<[Id; 4], bool> = FxHashMap::default();
        let changes = walk_wal(&mut cursor, after, through, o, &mut |op, q| {
            if gid.is_some_and(|g| q[3] != g) {
                return Ok(());
            }
            let added = op == WAL_INSERT;
            match local.entry(q) {
                Entry::Occupied(e) => {
                    e.remove();
                }
                Entry::Vacant(e) => {
                    e.insert(added);
                    if local.len() & 0xFFFF == 0 {
                        o.over(local.len() as u64)?;
                    }
                }
            }
            Ok(())
        })?;
        let mut keys = Keys::new(&generation);
        for (q, added) in local {
            net.toggle(keys.quad(&q)?, added)?;
        }
        Ok(changes)
    }

    fn diff_compare(&self, a: u64, b: u64, o: &DiffOptions, net: &mut Net<'_>) -> Result<u64> {
        let ho = self.history_opts(o);
        let (sa, _) = self.snapshot_at(&At::Commit(a), &ho)?;
        let (sb, _) = self.snapshot_at(&At::Commit(b), &ho)?;
        compare_states(&sa, &sb, net)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plans_follow_the_logs() {
        // gen 1: 0..=5, gen 2: compaction at 5, ..=9, gen 3: bulk commit 10, ..=12
        let gens = [(1, 0, 5), (2, 5, 9), (3, 10, 12)];
        let log = |generation, after, through| Step::Log {
            generation,
            after,
            through,
        };
        assert_eq!(plan(&gens, 1, 3), [log(1, 1, 3)]);
        assert_eq!(plan(&gens, 1, 7), [log(1, 1, 5), log(2, 5, 7)]);
        assert_eq!(
            plan(&gens, 2, 12),
            [
                log(1, 2, 5),
                log(2, 5, 9),
                Step::Compare { a: 9, b: 10 },
                log(3, 10, 12)
            ]
        );
        // a gap: gen 2 collected
        let gens = [(1, 0, 5), (3, 10, 12)];
        assert_eq!(
            plan(&gens, 3, 11),
            [log(1, 3, 5), Step::Compare { a: 5, b: 10 }, log(3, 10, 11)]
        );
        // an empty log at the end of a generation
        let gens = [(1, 0, 5), (2, 5, 5)];
        assert_eq!(plan(&gens, 4, 5), [log(1, 4, 5)]);
    }
}
