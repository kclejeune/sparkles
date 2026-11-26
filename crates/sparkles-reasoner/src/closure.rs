//! Closure operators for transitive and equivalence rules (spec F23, roadmap item 485).
//!
//! A rule of the form `(?x C ?y), (?y C ?z) -> (?x C ?z)`, with an optional
//! `notEqual(?x, ?z)`, or its guarded form `(?p G O), (?x ?p ?y), (?y ?p ?z) -> (?x ?p ?z)`
//! (`prp-trp`), is not evaluated as a join. After each iteration's merge an operator
//! closes the relation instead: it condenses the strongly connected components of the
//! relation's graph, works out the reachable set of each component once, from the sinks
//! up, and emits the pairs that are not facts yet. When the program also has the
//! symmetric rule `(?x C ?y) [notEqual(?x, ?y)] -> (?y C ?x)` for the same constant
//! predicate (`eq-sym` with `eq-trans`), the operator closes the relation as an
//! equivalence with union-find, so that each class is closed in one pass.
//!
//! The rules stay in the program, so explanations and maintenance still name them, and
//! the closure is the fixpoint the rules reach: the operator only reaches it in fewer
//! iterations. A program with a rule that is not monotonic (see
//! [`crate::engine::incremental_blocker`]), or one that reads lists and builds them,
//! runs without operators, because the order of derivation matters there. A program
//! that reads lists and derives list facts through a variable predicate starts again
//! without operators when such a fact appears ([`Restart`]).
//!
//! The operator is semi-naive. It keeps, per predicate, the row up to which the relation
//! is closed. The relation of the rows before that mark needs nothing more, so with few
//! new edges the operator reads only the nodes that reach a new edge and the nodes they
//! reach, through the graph's indexes. With many new edges it reads the whole relation.
//!
//! The pairs an operator emits flow into the graph in batches of at most [`BATCH`]
//! triples (roadmap item 865): a batch is merged, and its ids given, as soon as it is
//! full, so the operator's output buffer and the merge's tables and buffers are sized to
//! a batch and not to the closure, whose pairs can outnumber the facts of the run so far.
//! Merging the batches in the order they are emitted gives every pair the id one merge
//! of all of them would have given it.
//!
//! `notEqual` compares literal values, so a filter of reflexive pairs differs from a
//! comparison of ids when two literals with distinct ids and equal values meet in one
//! relation. The operator then gives the rule back to the ordinary evaluator for the rest
//! of the run, with one full evaluation, so the result stays the rules' fixpoint.

use crate::budget::{Budget, BudgetFailure, Held, table_bytes, table_capacity, table_grown};
use crate::builtins::{BuiltinKind, same_value};
use crate::engine::{Atom, CHead, CRule, Slot, incremental_blocker, reads_lists};
use crate::facts::Memo;
use crate::graph::{Graph, Triple};
use crate::terms::{Kind, Terms};
use oxrdf::Term;
use oxrdf::vocab::rdf;
use rustc_hash::{FxHashMap, FxHashSet};
use std::mem::size_of;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

/// Inner steps between two polls of the cancellation flag.
const POLL_EVERY: usize = 1 << 16;

/// Counts inner steps and polls the cancellation flag every [`POLL_EVERY`] of them.
struct Poll<'a> {
    cancel: Option<&'a AtomicBool>,
    steps: usize,
}

impl<'a> Poll<'a> {
    fn new(cancel: Option<&'a AtomicBool>) -> Poll<'a> {
        Poll { cancel, steps: 0 }
    }

    #[inline]
    fn tick(&mut self, k: usize) -> anyhow::Result<()> {
        self.steps += k;
        if self.steps >= POLL_EVERY {
            self.steps = 0;
            if self.cancel.is_some_and(|c| c.load(Ordering::Relaxed)) {
                anyhow::bail!("reasoning cancelled");
            }
        }
        Ok(())
    }
}

/// Pairs of a closure per merge into the graph, unless [`set_batch`] chose another size.
const BATCH: usize = 1 << 22;

/// The batch size a test or a measurement chose, or zero for [`BATCH`].
static CHOSEN_BATCH: AtomicUsize = AtomicUsize::new(0);

/// Merge the operators' pairs into the graph every `n` pairs, or every [`BATCH`] with
/// zero. For tests and measurements; it applies to every run of the process.
#[doc(hidden)]
pub fn set_batch(n: usize) {
    CHOSEN_BATCH.store(n, Ordering::Relaxed);
}

fn batch() -> usize {
    match CHOSEN_BATCH.load(Ordering::Relaxed) {
        0 => BATCH,
        n => n,
    }
}

/// An operator's pairs passed the room left for an iteration's candidates.
#[derive(Debug)]
struct Overflow;

impl std::fmt::Display for Overflow {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("closure candidates over the limit")
    }
}

impl std::error::Error for Overflow {}

/// Where an operator's pairs go: a buffer of at most [`batch`] pairs, charged as it grows
/// and merged into the graph's columns and triple index when it is full, which
/// deduplicates the next batch against it. The predicate's new facts enter its orders
/// once, when [`finish`](Self::finish) ends the predicate, as one merge of the whole
/// closure would have added them. The sink holds the graph while it lives, so no probe
/// reads the facts before they are indexed. `room` bounds the pairs of the operator's
/// step over its predicates, as `max_out` bounds the candidates of an iteration, and the
/// check comes before a pair is buffered, so a step that passes the room has merged
/// nothing of its last batch.
struct Sink<'g> {
    g: &'g mut Graph,
    out: Vec<Triple>,
    /// the charge of `out`
    held: Held,
    /// pairs emitted so far in the operator's step, the buffered ones included
    total: usize,
    room: usize,
    batch: usize,
    /// merges done, for the log
    merges: usize,
}

impl<'g> Sink<'g> {
    fn new(g: &'g mut Graph, budget: &Arc<Budget>, total: usize, room: usize) -> Sink<'g> {
        Sink {
            held: Held::new(budget),
            g,
            out: Vec::new(),
            total,
            room,
            batch: batch(),
            merges: 0,
        }
    }

    fn budget(&self) -> &Arc<Budget> {
        self.held.budget()
    }

    /// Buffer a pair, charging each growth of the buffer before it is reserved, and merge
    /// the buffer once it holds a batch.
    #[inline]
    fn emit(&mut self, t: Triple) -> anyhow::Result<()> {
        if self.total >= self.room {
            return Err(Overflow.into());
        }
        if self.out.len() == self.out.capacity() {
            // at most a batch, and at most what the room still allows; the new buffer is
            // charged beside the old one, which a moving growth copies from
            let left = self.room - self.total + self.out.len();
            let before = self.out.capacity();
            let cap = (before * 2).max(1024).min(self.batch).min(left);
            self.held.try_grow(cap * size_of::<Triple>())?;
            self.out.reserve_exact(cap - self.out.len());
            if self.out.capacity() > cap {
                self.held
                    .try_grow((self.out.capacity() - cap) * size_of::<Triple>())?;
            }
            self.held.shrink(before * size_of::<Triple>());
        }
        self.out.push(t);
        self.total += 1;
        if self.out.len() >= self.batch {
            self.flush()?;
        }
        Ok(())
    }

    /// Merge the buffered pairs and index the predicate's new facts.
    fn finish(mut self) -> anyhow::Result<usize> {
        self.flush()?;
        self.g.index_pending()?;
        Ok(self.total)
    }

    /// Drop the buffered pairs, which are not merged, and index what was merged, which
    /// is nothing when the operator gives its rules back, since it decides that before it
    /// emits a pair.
    fn discard(mut self) -> anyhow::Result<()> {
        self.out = Vec::new();
        self.held = Held::new(self.budget());
        self.g.index_pending()?;
        Ok(())
    }

    /// Merge the buffered pairs. The buffer's charge goes with it and is released when the
    /// merge has copied the new facts into the tier.
    fn flush(&mut self) -> anyhow::Result<()> {
        if self.out.is_empty() {
            return Ok(());
        }
        let out = std::mem::take(&mut self.out);
        let fresh = Held::new(self.budget());
        let held = std::mem::replace(&mut self.held, fresh);
        tracing::debug!(
            outputs = out.len(),
            merges = self.merges,
            used_bytes = held.budget().used(),
            peak_bytes = held.budget().peak(),
            tier_bytes = self.g.heap_bytes(),
            "closure operator outputs before their merge"
        );
        self.g.merge_unindexed(out, held)?;
        self.merges += 1;
        Ok(())
    }
}

/// Insert `x` into `set`, charging a growth of its table to `held` before it is reserved.
fn insert(set: &mut FxHashSet<u64>, x: u64, held: &mut Held) -> anyhow::Result<bool> {
    if set.contains(&x) {
        return Ok(false);
    }
    if set.len() == set.capacity() {
        let before = set.capacity();
        let want = (before * 2).max(16);
        held.try_grow(
            table_bytes(want, size_of::<u64>())
                .saturating_sub(table_bytes(before, size_of::<u64>())),
        )?;
        set.reserve(want - set.len());
        if set.capacity() > want {
            held.try_grow(
                table_bytes(set.capacity(), size_of::<u64>())
                    .saturating_sub(table_bytes(want, size_of::<u64>())),
            )?;
        }
    }
    Ok(set.insert(x))
}

/// Push `x` onto `v`, charging a growth of its buffer to `held` before it is reserved.
fn push<T>(v: &mut Vec<T>, x: T, held: &mut Held) -> anyhow::Result<()> {
    if v.len() == v.capacity() {
        let before = v.capacity();
        let want = (before * 2).max(16);
        held.try_grow((want - before) * size_of::<T>())?;
        v.reserve_exact(want - v.len());
        if v.capacity() > want {
            held.try_grow((v.capacity() - want) * size_of::<T>())?;
        }
    }
    v.push(x);
    Ok(())
}

/// Make room in `map` for one more entry. A growth charges the new table to `held` before
/// it is reserved, beside the old one that the move reads from, and gives the old one's
/// charge back after it.
fn reserve_one<V>(map: &mut FxHashMap<u64, V>, held: &mut Held) -> Result<(), BudgetFailure> {
    if map.len() == map.capacity() {
        let bytes = |cap: usize| table_bytes(cap, size_of::<(u64, V)>());
        let before = map.capacity();
        let want = (before * 2).max(16);
        held.try_grow(bytes(want))?;
        map.reserve(want - map.len());
        if map.capacity() > want {
            held.try_grow(bytes(map.capacity()) - bytes(want))?;
        }
        held.shrink(bytes(before));
    }
    Ok(())
}

/// Push `x` onto `v`, charging a growth of its buffer as [`reserve_one`] charges a table.
fn push_moved<T>(v: &mut Vec<T>, x: T, held: &mut Held) -> Result<(), BudgetFailure> {
    if v.len() == v.capacity() {
        let before = v.capacity();
        let want = (before * 2).max(4);
        held.try_grow(want * size_of::<T>())?;
        v.reserve_exact(want - v.len());
        if v.capacity() > want {
            held.try_grow((v.capacity() - want) * size_of::<T>())?;
        }
        held.shrink(before * size_of::<T>());
    }
    v.push(x);
    Ok(())
}

/// The predicates an operator closes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Over {
    /// one constant predicate
    Fixed(u64),
    /// every `?p` with a fact `(?p G O)`, for `(G, O)`
    Guarded(u64, u64),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Shape {
    /// transitive closure by strongly connected components
    Transitive,
    /// symmetric and transitive closure by union-find
    Equivalence,
}

#[derive(Debug)]
struct Op {
    /// the rules the operator stands for, the transitive one first
    rules: Vec<usize>,
    over: Over,
    shape: Shape,
    /// the transitive rule filters `notEqual(?x, ?z)`
    irreflexive: bool,
    /// per predicate: the rows before this one are closed
    marks: FxHashMap<u64, u32>,
    /// given back to the ordinary evaluator
    off: bool,
    /// the charge of `marks` and of the base's cached counts of the predicates the
    /// operator has probed, which both last for the run: it only grows
    kept: Option<Held>,
    /// an adorned copy of a demand program: how its magic atom binds it
    seeded: Option<Seeded>,
    /// the atoms of an adorned copy whose terms are all constants (the guard of a
    /// `prp-trp` copy for one predicate), which must hold for it to derive anything
    conds: Vec<Triple>,
    /// in a demand program, `rdf:first` and `rdf:rest`, which the operator never closes:
    /// a guarded operator that meets either gives its rule back
    lists: Option<[u64; 2]>,
}

/// Which way an adorned copy reads its relation from the values asked for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Dir {
    /// the magic atom binds the subject: the objects each value reaches
    Forward,
    /// the magic atom binds the object: the subjects that reach each value
    Backward,
}

/// How the magic atom of an adorned copy binds it (spec F23 Phase 2). A copy
/// `m(..), (?x P ?y), (?y P ?z) -> (?x P ?z)` whose magic atom binds `?x` derives, at its
/// fixpoint, the pairs `(x, z)` for every `x` its magic facts hold and every `z` that `x`
/// reaches, since each such pair follows from a shorter one from the same `x` and one
/// edge. The mirror holds for `?z`, and a copy that binds neither derives the whole
/// closure once its magic fact holds. The operator emits that fixpoint, so the program
/// derives the facts the joins would.
#[derive(Debug)]
struct Seeded {
    /// the magic predicate
    m: u64,
    /// positions of the magic fact (0 or 2) that hold a constant of the atom, with it
    fixed: Vec<(usize, u64)>,
    /// the position of the bound end in the magic fact, and the way it reads
    end: Option<(usize, Dir)>,
    /// the position of the predicate in the magic fact, for a guarded rule
    p_at: Option<usize>,
    /// the magic facts before this row were read in the run, or none yet
    mm: Option<u32>,
}

/// The pairs that operators of one kind merged, and the steps in which they merged any.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Counts {
    pub pairs: u64,
    pub steps: u64,
}

/// What the closure operators of a run did, by the kind of rule they stand for.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Stats {
    /// rules without a magic atom
    pub plain: Counts,
    /// adorned copies whose magic atom binds the subject
    pub forward: Counts,
    /// adorned copies whose magic atom binds the object
    pub backward: Counts,
    /// adorned copies whose magic atom binds neither end
    pub full: Counts,
    /// adorned copies of a rule with a variable predicate (`prp-trp`), of every kind
    /// above
    pub guarded: Counts,
    /// adorned copies of a rule with a constant predicate under a key whose magic facts
    /// hold the predicate (`Bound`), of every kind above
    pub bound_constant: Counts,
    /// graphs of a relation that operators built and closed
    pub walks: u64,
    /// of those, the ones that merged no pair
    pub idle: u64,
    /// of those, the ones that new edges since the operator last closed the relation
    /// called for, for every value asked for or for the whole relation
    pub rewalks: u64,
}

/// What one closure step did.
pub(crate) struct Stepped {
    /// new rows
    pub added: usize,
    /// rules that the ordinary evaluator takes back, each with one full evaluation next
    pub refresh: Vec<usize>,
}

/// The closure operators of a program.
#[derive(Debug)]
pub(crate) struct Closures {
    ops: Vec<Op>,
    start: Option<u32>,
    /// `rdf:first` and `rdf:rest` when a rule reads lists: a derived fact of either makes
    /// what the list builtins read depend on when it appears
    lists: Option<[u64; 2]>,
    stats: Stats,
    ledger: Ledger,
}

/// What the run's operators know of each relation they close, which spares them walks
/// that merge nothing.
#[derive(Debug, Default)]
struct Ledger {
    /// per predicate, the row ranges of the pairs that transitive operators merged, in
    /// order: each pair is in the closure of the edges before it, so it changes what no
    /// value reaches, and edges after a mark that are all such pairs call for no walk
    merged: FxHashMap<u64, Vec<(u32, u32)>>,
    /// per predicate, the row before which a transitive operator closed the whole
    /// relation, and whether it left out the pairs `(x, x)` (a `notEqual` filter): until
    /// an edge that is not a merged pair follows it, every pair an operator over the
    /// predicate could emit is there
    closed: FxHashMap<u64, (u32, bool)>,
    /// the charge of the above, which lasts for the run
    held: Option<Held>,
}

impl Ledger {
    /// The edges of `q` in rows `[lo, now)` that are not pairs operators merged.
    fn edges(&self, g: &Graph, q: u64, lo: u32, now: u32) -> usize {
        if lo >= now {
            return 0;
        }
        let n = g.count_p(q, lo, now);
        let Some(rs) = self.merged.get(&q) else {
            return n;
        };
        let from = rs.partition_point(|r| r.1 <= lo);
        let merged: usize = rs[from..]
            .iter()
            .take_while(|r| r.0 < now)
            .map(|r| (r.1.min(now) - r.0.max(lo)) as usize)
            .sum();
        n - merged.min(n)
    }

    /// Whether the relation of `q` is closed for an operator, with or without its
    /// `notEqual` filter, at row `now`.
    fn holds(&self, g: &Graph, q: u64, irreflexive: bool, now: u32) -> bool {
        self.closed
            .get(&q)
            .is_some_and(|&(at, irr)| (irreflexive || !irr) && self.edges(g, q, at, now) == 0)
    }

    /// Record the rows `[lo, hi)` of pairs of `q` that a transitive operator merged.
    fn merge(&mut self, q: u64, lo: u32, hi: u32, budget: &Arc<Budget>) -> anyhow::Result<()> {
        if lo >= hi {
            return Ok(());
        }
        let held = self.held.get_or_insert_with(|| Held::new(budget));
        if !self.merged.contains_key(&q) {
            reserve_one(&mut self.merged, held)?;
        }
        let rs = self.merged.entry(q).or_default();
        match rs.last_mut() {
            Some(last) if last.1 == lo => last.1 = hi,
            _ => push_moved(rs, (lo, hi), held)?,
        }
        Ok(())
    }

    /// Record that a transitive operator closed the whole relation of `q` from its edges
    /// before row `at`.
    fn close(
        &mut self,
        q: u64,
        at: u32,
        irreflexive: bool,
        budget: &Arc<Budget>,
    ) -> Result<(), BudgetFailure> {
        if !self.closed.contains_key(&q) {
            let held = self.held.get_or_insert_with(|| Held::new(budget));
            reserve_one(&mut self.closed, held)?;
        }
        let e = self.closed.entry(q).or_insert((at, irreflexive));
        // the later closure, and of two at one row the one with the pairs `(x, x)`, which
        // holds for an operator either way
        if at > e.0 || (at == e.0 && !irreflexive) {
            *e = (at, irreflexive);
        }
        Ok(())
    }
}

/// A run with closure operators derived list structure that rules read, so it starts
/// again without them, which gives the result of the rules' own order of derivation.
#[derive(Debug)]
pub(crate) struct Restart;

impl std::fmt::Display for Restart {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("derived list facts: the run starts again without closure operators")
    }
}

impl std::error::Error for Restart {}

#[inline]
fn var(s: Slot) -> Option<usize> {
    match s {
        Slot::Var(v) => Some(v),
        _ => None,
    }
}

/// `(?x P ?y), (?y P ?z)` in this order, with the head's `?x` and `?z`
fn chain(a: &Atom, b: &Atom, x: usize, z: usize) -> Option<usize> {
    let (s1, o1, s2, o2) = (var(a.s)?, var(a.o)?, var(b.s)?, var(b.o)?);
    (s1 == x && o2 == z && o1 == s2 && o1 != x && o1 != z).then_some(o1)
}

/// The only builtin is `notEqual(?a, ?b)` (either order): `Some(true)`; no builtin:
/// `Some(false)`; anything else: `None`.
fn filtered(r: &CRule, a: usize, b: usize) -> Option<bool> {
    match r.builtins.as_slice() {
        [] => Some(false),
        [f] if f.kind == BuiltinKind::NotEqual => {
            let args: Vec<Option<usize>> = f.args.iter().map(|s| var(*s)).collect();
            (args == [Some(a), Some(b)] || args == [Some(b), Some(a)]).then_some(true)
        }
        _ => None,
    }
}

/// A transitive rule: what it closes and whether it filters reflexive pairs.
fn transitive(r: &CRule) -> Option<(Over, bool)> {
    transitive_with(r, r.nvars)
}

/// [`transitive`] for a rule with `nvars` variables.
fn transitive_with(r: &CRule, nvars: usize) -> Option<(Over, bool)> {
    let [CHead::Triple(hs, hp, ho)] = r.head.as_slice() else {
        return None;
    };
    let (x, z) = (var(*hs)?, var(*ho)?);
    if x == z {
        return None;
    }
    let irreflexive = filtered(r, x, z)?;
    match (*hp, r.atoms.as_slice()) {
        (Slot::Const(c), [a, b]) if a.p == Slot::Const(c) && b.p == Slot::Const(c) => {
            chain(a, b, x, z).or_else(|| chain(b, a, x, z))?;
            (nvars == 3).then_some((Over::Fixed(c), irreflexive))
        }
        (Slot::Var(p), atoms) if atoms.len() == 3 => {
            let gi = atoms.iter().position(|a| {
                a.s == Slot::Var(p)
                    && matches!(a.p, Slot::Const(_))
                    && matches!(a.o, Slot::Const(_))
            })?;
            let edges: Vec<&Atom> = (0..3).filter(|&i| i != gi).map(|i| &atoms[i]).collect();
            if edges.iter().any(|a| a.p != Slot::Var(p)) {
                return None;
            }
            let y = chain(edges[0], edges[1], x, z).or_else(|| chain(edges[1], edges[0], x, z))?;
            let (Slot::Const(g), Slot::Const(o)) = (atoms[gi].p, atoms[gi].o) else {
                return None;
            };
            (nvars == 4 && p != x && p != y && p != z).then_some((Over::Guarded(g, o), irreflexive))
        }
        _ => None,
    }
}

/// A symmetric rule `(?x C ?y) -> (?y C ?x)`: `C` and whether it filters `notEqual`.
fn symmetric(r: &CRule) -> Option<(u64, bool)> {
    let [CHead::Triple(hs, Slot::Const(c), ho)] = r.head.as_slice() else {
        return None;
    };
    let [a] = r.atoms.as_slice() else {
        return None;
    };
    let (x, y) = (var(a.s)?, var(a.o)?);
    if x == y || a.p != Slot::Const(*c) || var(*hs)? != y || var(*ho)? != x || r.nvars != 2 {
        return None;
    }
    Some((*c, filtered(r, x, y)?))
}

/// Whether `r` has a builtin of a lowered relation atom (item 839).
fn relation_builtins(r: &CRule) -> bool {
    use crate::builtins::BuiltinKind as B;
    r.builtins
        .iter()
        .any(|b| matches!(b.kind, B::RelTail | B::RelTuple | B::RelAbsent))
}

/// A rule whose body has one atom over a magic predicate, an adorned copy of a demand
/// program: what the copy closes, whether it filters reflexive pairs, how its magic atom
/// binds it, and its atoms whose terms are all constants. `None` when the copy is not
/// one of a transitive rule, or when its magic atom binds both ends, where the join is a
/// lookup of the pairs asked for.
fn adorned(r: &CRule, magic: &FxHashSet<u64>) -> Option<(Over, bool, Seeded, Vec<Triple>)> {
    // [`transitive_with`] closes a rule whose two edge atoms read its head's predicate, so a
    // copy without two such atoms is passed over before it is taken apart: a run detects
    // its operators again for every stage, over every copy of a large program
    let [CHead::Triple(_, hp, _)] = r.head.as_slice() else {
        return None;
    };
    if r.atoms.iter().filter(|a| a.p == *hp).count() < 2 {
        return None;
    }
    let at: Vec<usize> = (0..r.atoms.len())
        .filter(|&i| matches!(r.atoms[i].p, Slot::Const(m) if magic.contains(&m)))
        .collect();
    let [gi] = at[..] else {
        return None;
    };
    let guard = r.atoms[gi].clone();
    let mut conds: Vec<Triple> = Vec::new();
    let mut rest = r.clone_shallow();
    rest.atoms.clear();
    for (i, a) in r.atoms.iter().enumerate() {
        if i == gi {
            continue;
        }
        match (a.s, a.p, a.o) {
            (Slot::Const(s), Slot::Const(p), Slot::Const(o)) => conds.push([s, p, o]),
            _ => rest.atoms.push(a.clone()),
        }
    }
    // the variables the rule uses without its magic atom, which must hold the atom's
    let mut used: FxHashSet<usize> = FxHashSet::default();
    let slots = rest
        .atoms
        .iter()
        .flat_map(|a| [a.s, a.p, a.o])
        .chain(rest.builtins.iter().flat_map(|b| b.args.iter().copied()))
        .chain(rest.head.iter().flat_map(|h| match h {
            CHead::Triple(s, p, o) => vec![*s, *p, *o],
            CHead::Action(_, args) => args.clone(),
        }));
    for s in slots {
        if let Slot::Var(v) = s {
            used.insert(v);
        }
    }
    let (over, irreflexive) = transitive_with(&rest, used.len())?;
    // a guarded rule's guard is an atom with variables, which a copy keeps
    if matches!(over, Over::Guarded(..)) && !conds.is_empty() {
        return None;
    }
    let [CHead::Triple(hs, hp, ho)] = rest.head.as_slice() else {
        return None;
    };
    let (x, z, p) = (var(*hs)?, var(*ho)?, var(*hp));
    let mut seeded = Seeded {
        m: match guard.p {
            Slot::Const(m) => m,
            _ => return None,
        },
        fixed: Vec::new(),
        end: None,
        p_at: None,
        mm: None,
    };
    let (mut xs, mut zs) = (None, None);
    for (pos, s) in [(0usize, guard.s), (2, guard.o)] {
        match s {
            Slot::Const(c) => seeded.fixed.push((pos, c)),
            Slot::Var(v) if v == x => xs = Some(pos),
            Slot::Var(v) if v == z => zs = Some(pos),
            Slot::Var(v) if Some(v) == p => seeded.p_at = Some(pos),
            _ => return None,
        }
    }
    seeded.end = match (xs, zs) {
        (Some(_), Some(_)) => return None,
        (Some(pos), None) => Some((pos, Dir::Forward)),
        (None, Some(pos)) => Some((pos, Dir::Backward)),
        (None, None) => None,
    };
    if seeded.p_at.is_some() && !matches!(over, Over::Guarded(..)) {
        return None;
    }
    Some((over, irreflexive, seeded, conds))
}

/// Whether `r` is a rule of a demand program that derives magic facts alone, whose
/// heads hold magic predicates and blank nodes local to the run.
fn magic_rule(r: &CRule, magic: &FxHashSet<u64>) -> bool {
    !r.head.is_empty()
        && r.head
            .iter()
            .all(|h| matches!(h, CHead::Triple(_, Slot::Const(p), _) if magic.contains(p)))
}

impl Closures {
    /// Whether the rules of a program allow operators: no rule's result depends on the
    /// order of derivation, no rule reads a relation's tail, and no rule both reads lists
    /// and builds them in its head.
    pub(crate) fn admits(rules: &[CRule], terms: &Terms) -> Result<bool, BudgetFailure> {
        // the operators read no relation tails (item 839), which only builtins see
        if rules
            .iter()
            .any(|r| incremental_blocker(r, terms).is_some() || relation_builtins(r))
        {
            return Ok(false);
        }
        // what a list builtin reads depends on when derived list facts appear, and the
        // operators derive their pairs earlier than joins do: a program that reads lists
        // and builds them in a rule head runs without operators, and one whose rules
        // derive list facts through a variable predicate starts again without them
        // (see `Restart`)
        if rules.iter().any(reads_lists) {
            let ids = [
                terms.id_for(&Term::NamedNode(rdf::FIRST.into_owned()))?,
                terms.id_for(&Term::NamedNode(rdf::REST.into_owned()))?,
            ];
            if rules.iter().any(|r| {
                r.head
                    .iter()
                    .any(|h| matches!(h, CHead::Triple(_, Slot::Const(p), _) if ids.contains(p)))
            }) {
                return Ok(false);
            }
        }
        Ok(true)
    }

    /// The operators of `rules`, or `None` when no rule has one or the program has a rule
    /// whose result depends on the order of derivation. `start` is the row before which
    /// the facts are closed under the rules, as for [`crate::engine::run`].
    ///
    /// With `magic`, the magic predicates of a demand program (spec F23 Phase 2), the
    /// rules are a stage of that program, and an adorned copy of a transitive rule gets an
    /// operator that closes the relation for the values its magic facts ask for. The
    /// caller checks the source rules with [`Closures::admits`]: a stage's copies of a
    /// rule with a variable predicate can have a constant `rdf:first` or `rdf:rest` head
    /// that the source rule does not, and its magic rules have blank node heads. The
    /// stage's other rules are checked for the order of derivation and relation tails
    /// here too, and its operators never close `rdf:first` or `rdf:rest`.
    pub(crate) fn detect(
        rules: &[CRule],
        terms: &Terms,
        start: Option<u32>,
        magic: Option<&FxHashSet<u64>>,
    ) -> Result<Option<Closures>, BudgetFailure> {
        match magic {
            None => {
                if !Closures::admits(rules, terms)? {
                    return Ok(None);
                }
            }
            Some(mp) => {
                if rules.iter().any(|r| {
                    !magic_rule(r, mp)
                        && (incremental_blocker(r, terms).is_some() || relation_builtins(r))
                }) {
                    return Ok(None);
                }
            }
        }
        let list_ids = [
            terms.id_for(&Term::NamedNode(rdf::FIRST.into_owned()))?,
            terms.id_for(&Term::NamedNode(rdf::REST.into_owned()))?,
        ];
        let lists = rules.iter().any(reads_lists).then_some(list_ids);
        let syms: Vec<(usize, u64)> = rules
            .iter()
            .enumerate()
            .filter_map(|(i, r)| symmetric(r).map(|(c, _)| (i, c)))
            .collect();
        let mut ops: Vec<Op> = Vec::new();
        for (i, r) in rules.iter().enumerate() {
            let found = match magic {
                Some(mp)
                    if r.atoms
                        .iter()
                        .any(|a| matches!(a.p, Slot::Const(m) if mp.contains(&m))) =>
                {
                    adorned(r, mp).map(|(o, irr, s, c)| (o, irr, Some(s), c))
                }
                _ => transitive(r).map(|(o, irr)| (o, irr, None, Vec::new())),
            };
            let Some((over, irreflexive, seeded, conds)) = found else {
                continue;
            };
            if magic.is_some() && matches!(over, Over::Fixed(c) if list_ids.contains(&c)) {
                continue;
            }
            let mut op = Op {
                rules: vec![i],
                over,
                shape: Shape::Transitive,
                irreflexive,
                marks: FxHashMap::default(),
                off: false,
                kept: None,
                seeded,
                conds,
                lists: magic.map(|_| list_ids),
            };
            if op.seeded.is_none()
                && let Over::Fixed(c) = over
                && let Some(&(si, _)) = syms.iter().find(|s| s.1 == c)
            {
                op.shape = Shape::Equivalence;
                op.rules.push(si);
            }
            ops.push(op);
        }
        // a symmetric rule that two equivalence operators share stays with the first
        let mut taken = FxHashSet::default();
        for op in &mut ops {
            op.rules.retain(|&r| taken.insert(r));
        }
        ops.retain(|op| !op.rules.is_empty());
        Ok((!ops.is_empty()).then_some(Closures {
            ops,
            start,
            lists,
            stats: Stats::default(),
            ledger: Ledger::default(),
        }))
    }

    /// Whether rule `ri` is evaluated by an operator now.
    pub(crate) fn replaces(&self, ri: usize) -> bool {
        self.ops.iter().any(|op| !op.off && op.rules.contains(&ri))
    }

    /// Force every operator covering any predicate in `preds` to re-close that predicate
    /// from row 0 in the next step, even when no new edges arrive after `start`. Used by
    /// incremental saturation after a deletion removed facts from an operator relation.
    pub(crate) fn reset_predicates(&mut self, preds: &[u64]) {
        for op in &mut self.ops {
            if op.off {
                continue;
            }
            for &q in preds {
                let covers = match op.over {
                    Over::Fixed(c) => c == q,
                    Over::Guarded(..) => true,
                };
                if covers {
                    op.marks.insert(q, 0);
                }
            }
        }
    }

    /// What the operators did so far.
    pub(crate) fn stats(&self) -> Stats {
        self.stats
    }

    /// The operators' names, for the log.
    pub(crate) fn describe(&self, rules: &[CRule]) -> Vec<String> {
        self.ops
            .iter()
            .map(|op| {
                let names: Vec<&str> = op.rules.iter().map(|&r| rules[r].name.as_str()).collect();
                let mode = match &op.seeded {
                    None => "",
                    Some(s) => match s.end {
                        Some((_, Dir::Forward)) => " forward",
                        Some((_, Dir::Backward)) => " backward",
                        None => " full",
                    },
                };
                format!("{:?}{mode} {}", op.shape, names.join("+"))
            })
            .collect()
    }

    /// Close every relation with new edges since its mark, and merge the new pairs into
    /// `g`, one operator at a time. `max_out` bounds the candidates of one operator, as
    /// it bounds those of an iteration.
    pub(crate) fn step(
        &mut self,
        g: &mut Graph,
        terms: &Terms,
        budget: &Arc<Budget>,
        cancel: Option<&AtomicBool>,
        (max_out, limit): (usize, usize),
        rules: &[CRule],
    ) -> anyhow::Result<Stepped> {
        let mut stepped = Stepped {
            added: 0,
            refresh: Vec::new(),
        };
        self.check_lists(g)?;
        let start = self.start;
        let mut poll = Poll::new(cancel);
        let mut ops_ms = 0f64;
        for op in &mut self.ops {
            if op.off {
                continue;
            }
            let t_op = std::time::Instant::now();
            let (mut walks, mut idle, mut rewalks, mut edges, mut nsrc) =
                (0u64, 0u64, 0u64, 0usize, 0usize);
            let now = g.len();
            // the magic facts of an adorned copy, kept while they are read
            let seeds = match &op.seeded {
                Some(s) => {
                    let f = s.facts(g, now, budget, &mut poll)?;
                    if f.rows.is_empty() {
                        continue;
                    }
                    Some(f)
                }
                None => None,
            };
            // the predicates and their charge, kept while they are read
            let (mut preds, _preds_held) = op.predicates(g, now, budget, &mut poll)?;
            let prep_ms = t_op.elapsed().as_secs_f64() * 1e3;
            let npreds = preds.len();
            if let (Some(s), Some(f)) = (&op.seeded, &seeds)
                && s.p_at.is_some()
            {
                preds.retain(|q| !f.of(*q, true).is_empty());
            }
            // the marks the operator keeps and the base's cached count of each predicate
            let kept = op.kept.get_or_insert_with(|| Held::new(budget));
            let want = table_bytes(
                op.marks.capacity().max(preds.len()),
                size_of::<(u64, u32)>(),
            ) + table_bytes(preds.len(), size_of::<(u64, u64)>());
            if want > kept.bytes() {
                kept.try_set(want)?;
            }
            let before = g.len();
            let mut done: Vec<u64> = Vec::new();
            let mut total = 0usize;
            // in a demand program an operator never closes `rdf:first` or `rdf:rest`: a
            // guarded one whose guard admits either gives its rule back, so that joins
            // derive their facts, before it merges a pair
            let mut back = op
                .lists
                .is_some_and(|ids| preds.iter().any(|q| ids.contains(q)));
            // a guarded operator with a `notEqual` filter gives its rules back when any of
            // its predicates has a literal subject, before it merges a pair of any of
            // them, as it did when it held every pair until the step was done
            if op.irreflexive && matches!(op.over, Over::Guarded(..)) {
                for &q in &preds {
                    poll.tick(1)?;
                    let Some((local, _)) = op.local(
                        g,
                        q,
                        now,
                        start,
                        seeds.as_ref(),
                        &self.ledger,
                        budget,
                        cancel,
                    )?
                    else {
                        continue;
                    };
                    if local.literal_subject(terms) {
                        back = true;
                        break;
                    }
                }
            }
            for q in preds {
                if back {
                    break;
                }
                poll.tick(1)?;
                let Some((local, again)) = op.local(
                    g,
                    q,
                    now,
                    start,
                    seeds.as_ref(),
                    &self.ledger,
                    budget,
                    cancel,
                )?
                else {
                    continue;
                };
                walks += 1;
                rewalks += u64::from(again);
                edges += local.adj.len();
                nsrc += if local.emit.is_empty() {
                    local.ids.len()
                } else {
                    local.emit.iter().filter(|&&e| e).count()
                };
                let rows0 = g.len();
                let mut sink = Sink::new(g, budget, total, max_out);
                let closed = match op.shape {
                    Shape::Transitive => {
                        if op.irreflexive && local.literal_subject(terms) {
                            Ok(false)
                        } else {
                            local
                                .transitive(q, op.irreflexive, &mut sink, cancel)
                                .map(|()| true)
                        }
                    }
                    Shape::Equivalence => {
                        local.equivalence(q, op.irreflexive, terms, &mut sink, cancel)
                    }
                };
                drop(local);
                match closed {
                    Ok(true) => {}
                    Ok(false) => {
                        // nothing was merged: the check comes before the first pair
                        sink.discard()?;
                        back = true;
                        break;
                    }
                    Err(e) if e.is::<Overflow>() => {
                        return Err(crate::InferredLimitExceeded {
                            limit,
                            context: format!(
                                " (the closure of rule '{}' produced more than {max_out} candidate triples)",
                                rules[op.rules[0]].name
                            ),
                        }
                        .into());
                    }
                    Err(e) => return Err(e),
                }
                if cancel.is_some_and(|c| c.load(Ordering::Relaxed)) {
                    anyhow::bail!("reasoning cancelled");
                }
                total = sink.finish()?;
                if g.len() == rows0 {
                    idle += 1;
                }
                if op.shape == Shape::Transitive {
                    self.ledger.merge(q, rows0, g.len(), budget)?;
                    // a copy that binds no end closes the whole relation of its edges
                    if op.seeded.as_ref().is_some_and(|s| s.end.is_none()) {
                        self.ledger.close(q, now, op.irreflexive, budget)?;
                    }
                }
                done.push(q);
            }
            let nseeds = seeds.as_ref().map_or(0, |f| f.rows.len());
            drop(seeds);
            let after = g.len();
            self.stats.walks += walks;
            self.stats.idle += idle;
            self.stats.rewalks += rewalks;
            let op_ms = t_op.elapsed().as_secs_f64() * 1e3;
            ops_ms += op_ms;
            if op_ms >= 2.0 {
                let mode = match op.seeded.as_ref().map(|s| s.end) {
                    None => "plain",
                    Some(Some((_, Dir::Forward))) => "forward",
                    Some(Some((_, Dir::Backward))) => "backward",
                    Some(None) => "full",
                };
                tracing::debug!(
                    rule = %rules[op.rules[0]].name,
                    mode,
                    op_ms,
                    prep_ms,
                    preds = npreds,
                    walks,
                    idle,
                    rewalks,
                    edges,
                    sources = nsrc,
                    seeds = nseeds,
                    new = after - before,
                    "closure operator step"
                );
            }
            stepped.added += (after - before) as usize;
            if after > before {
                let n = (after - before) as u64;
                let counts = self.stats.of(op.seeded.as_ref());
                counts.pairs += n;
                counts.steps += 1;
                if op.seeded.is_some() && matches!(op.over, Over::Guarded(..)) {
                    self.stats.guarded.pairs += n;
                    self.stats.guarded.steps += 1;
                }
                if let (Some(s), Over::Fixed(c)) = (&op.seeded, op.over)
                    && s.fixed.iter().any(|&(_, v)| v == c)
                {
                    self.stats.bound_constant.pairs += n;
                    self.stats.bound_constant.steps += 1;
                }
            }
            if back {
                tracing::debug!(rule = %rules[op.rules[0]].name, "closure operator gives its rules back");
                op.off = true;
                stepped.refresh.extend(op.rules.iter().copied());
                continue;
            }
            for q in done {
                op.marks.insert(q, after);
            }
            if let Some(s) = op.seeded.as_mut() {
                s.mm = Some(now);
            }
        }
        tracing::debug!(ops_ms, "closure operators' own time in the step");
        self.check_lists(g)?;
        Ok(stepped)
    }

    /// [`Restart`] when the program reads lists and the run has derived list structure
    /// (alive, non-explicit `rdf:first` or `rdf:rest` rows in the derived tier).
    fn check_lists(&self, g: &Graph) -> anyhow::Result<()> {
        if let Some([first, rest]) = self.lists {
            let (lo, hi) = (g.offset(), g.len());
            for p in [first, rest] {
                if g.count_p(p, lo, hi) > 0
                    && g.cands(None, Some(p), None, lo, hi)
                        .iter()
                        .any(|r| g.alive(r) && !g.flagged(r) && g.triple(r)[1] == p)
                {
                    return Err(Restart.into());
                }
            }
        }
        Ok(())
    }
}

impl Stats {
    fn of(&mut self, seeded: Option<&Seeded>) -> &mut Counts {
        match seeded.map(|s| s.end) {
            None => &mut self.plain,
            Some(Some((_, Dir::Forward))) => &mut self.forward,
            Some(Some((_, Dir::Backward))) => &mut self.backward,
            Some(None) => &mut self.full,
        }
    }

    /// The counts of `self` and `other` together.
    pub(crate) fn add(&mut self, other: &Stats) {
        for (a, b) in [
            (&mut self.plain, other.plain),
            (&mut self.forward, other.forward),
            (&mut self.backward, other.backward),
            (&mut self.full, other.full),
            (&mut self.guarded, other.guarded),
            (&mut self.bound_constant, other.bound_constant),
        ] {
            a.pairs += b.pairs;
            a.steps += b.steps;
        }
        self.walks += other.walks;
        self.idle += other.idle;
        self.rewalks += other.rewalks;
    }
}

/// The magic facts of an adorned copy, as (row, value of the bound end, predicate),
/// sorted by predicate and row, with their charge.
struct Seeds {
    rows: Vec<(u64, u32, u64)>,
    _held: Held,
}

impl Seeds {
    /// The facts that ask for predicate `q`, or all of them when the magic atom does not
    /// hold the predicate (`by_p` false).
    fn of(&self, q: u64, by_p: bool) -> &[(u64, u32, u64)] {
        if !by_p {
            return &self.rows;
        }
        let lo = self.rows.partition_point(|r| r.0 < q);
        let hi = self.rows.partition_point(|r| r.0 <= q);
        &self.rows[lo..hi]
    }
}

impl Seeded {
    /// The magic facts among rows `< now` that the copy's magic atom matches.
    fn facts(
        &self,
        g: &Graph,
        now: u32,
        budget: &Arc<Budget>,
        poll: &mut Poll,
    ) -> anyhow::Result<Seeds> {
        let mut held = Held::new(budget);
        let mut rows: Vec<(u64, u32, u64)> = Vec::new();
        let c = g.cands(None, Some(self.m), None, g.offset(), now);
        for r in c.iter() {
            poll.tick(1)?;
            if !g.alive(r) {
                continue;
            }
            let t = g.triple(r);
            if t[1] != self.m || self.fixed.iter().any(|&(pos, v)| t[pos] != v) {
                continue;
            }
            let end = self.end.map_or(0, |(pos, _)| t[pos]);
            let q = self.p_at.map_or(0, |pos| t[pos]);
            push(&mut rows, (q, r, end), &mut held)?;
        }
        rows.sort_unstable();
        Ok(Seeds { rows, _held: held })
    }
}

impl Op {
    /// The predicates the operator closes now.
    fn predicates(
        &self,
        g: &Graph,
        now: u32,
        budget: &Arc<Budget>,
        poll: &mut Poll,
    ) -> anyhow::Result<(Vec<u64>, Held)> {
        match self.over {
            Over::Fixed(c) => Ok((vec![c], Held::new(budget))),
            Over::Guarded(gp, go) => {
                // charged as it grows, and returned with the vector
                let mut held = Held::new(budget);
                let mut out: Vec<u64> = Vec::new();
                if let Some(b) = g.base()
                    && g.reads_base(0, now)
                {
                    let mut err: Option<anyhow::Error> = None;
                    b.for_each(None, Some(gp), Some(go), |t| {
                        match push(&mut out, t[0], &mut held).and_then(|()| poll.tick(1)) {
                            Ok(()) => true,
                            Err(e) => {
                                err = Some(e);
                                false
                            }
                        }
                    })?;
                    if let Some(e) = err {
                        return Err(e);
                    }
                }
                let c = g.cands(None, Some(gp), Some(go), g.offset(), now);
                for r in c.iter() {
                    poll.tick(1)?;
                    if g.alive(r) {
                        let t = g.triple(r);
                        if t[1] == gp && t[2] == go {
                            push(&mut out, t[0], &mut held)?;
                        }
                    }
                }
                out.sort_unstable();
                out.dedup();
                Ok((out, held))
            }
        }
    }

    /// The mark of a predicate the operator has not closed in this run: the facts
    /// before `start` are closed under the rules, the guard and the constant atoms of an
    /// adorned copy included.
    fn initial(&self, g: &Graph, q: u64, start: Option<u32>) -> u32 {
        let Some(st) = start else {
            return 0;
        };
        let mut memo = Memo::default();
        if !self.conds.iter().all(|c| g.holds_before(&mut memo, c, st)) {
            return 0;
        }
        match self.over {
            Over::Fixed(_) => st,
            Over::Guarded(gp, go) => {
                if g.holds_before(&mut memo, &[q, gp, go], st) {
                    st
                } else {
                    0
                }
            }
        }
    }

    /// The graph of predicate `q` that the operator closes in this step, or `None` when
    /// nothing changed for it. A rule without a magic atom reads the nodes that new edges
    /// since the mark change. An adorned copy reads, from the values its magic facts ask
    /// for, the nodes they reach: from every such value when the predicate has new edges
    /// since its mark or the operator meets the predicate first, and from the values of
    /// new magic facts alone otherwise. A copy whose magic atom binds no end closes the
    /// whole relation once a magic fact asks for it, from row 0 when that fact is new.
    #[allow(clippy::too_many_arguments)]
    fn local(
        &self,
        g: &Graph,
        q: u64,
        now: u32,
        start: Option<u32>,
        seeds: Option<&Seeds>,
        ledger: &Ledger,
        budget: &Arc<Budget>,
        cancel: Option<&AtomicBool>,
    ) -> anyhow::Result<Option<(Local, bool)>> {
        if !self.conds.is_empty() {
            let mut memo = Memo::default();
            if !self.conds.iter().all(|c| g.holds_before(&mut memo, c, now)) {
                return Ok(None);
            }
        }
        // a transitive operator has nothing to add to a relation closed as a whole
        if self.shape == Shape::Transitive && ledger.holds(g, q, self.irreflexive, now) {
            return Ok(None);
        }
        let fresh = |mark: u32| ledger.edges(g, q, mark, now) > 0;
        let (Some(s), Some(f)) = (&self.seeded, seeds) else {
            let mark = match self.marks.get(&q) {
                Some(&m) => m,
                None => self.initial(g, q, start),
            };
            if !fresh(mark) {
                return Ok(None);
            }
            let again = self.marks.contains_key(&q);
            return Local::build(g, q, mark, now, self.shape, budget, cancel)
                .map(|l| Some((l, again)));
        };
        let mine = f.of(q, s.p_at.is_some());
        if mine.is_empty() {
            return Ok(None);
        }
        match s.end {
            None => {
                // closed before `start` only for a magic fact from before it
                let mark = match self.marks.get(&q) {
                    Some(&m) => m,
                    None if start.is_some_and(|st| mine.iter().any(|r| r.1 < st)) => {
                        self.initial(g, q, start)
                    }
                    None => 0,
                };
                if !fresh(mark) {
                    return Ok(None);
                }
                let again = self.marks.contains_key(&q);
                Local::build(g, q, mark, now, self.shape, budget, cancel).map(|l| Some((l, again)))
            }
            Some((_, dir)) => {
                let mark = match self.marks.get(&q) {
                    Some(&m) => m,
                    None => self.initial(g, q, start),
                };
                let mm = s.mm.or(start).unwrap_or(0);
                let all = fresh(mark);
                let mut held = Held::new(budget);
                let mut sources: Vec<u64> = Vec::new();
                for r in mine {
                    if all || r.1 >= mm {
                        push(&mut sources, r.2, &mut held)?;
                    }
                }
                if sources.is_empty() {
                    return Ok(None);
                }
                sources.sort_unstable();
                sources.dedup();
                let again = all && self.marks.contains_key(&q);
                Local::seeded(g, q, &sources, now, dir, budget, cancel).map(|l| Some((l, again)))
            }
        }
    }
}

/// A relation's graph over dense local node numbers: the out-edges of every node in it,
/// sorted, and which nodes emit their missing pairs.
struct Local {
    ids: Vec<u64>,
    off: Vec<u32>,
    adj: Vec<u32>,
    /// the nodes whose pairs to emit; all of them when empty
    emit: Vec<bool>,
    /// the edges are the relation's reversed: node `x`'s out-edges are the subjects of
    /// the facts whose object `x` is, and a pair `(x, z)` is the fact `(z q x)`
    reversed: bool,
    /// the charge of the above
    _held: Held,
}

struct Builder {
    ids: Vec<u64>,
    at: FxHashMap<u64, u32>,
    pairs: Vec<(u32, u32)>,
    held: Held,
}

impl Builder {
    fn new(budget: &Arc<Budget>) -> Builder {
        Builder {
            ids: Vec::new(),
            at: FxHashMap::default(),
            pairs: Vec::new(),
            held: Held::new(budget),
        }
    }

    fn bytes(&self) -> usize {
        self.ids.capacity() * size_of::<u64>()
            + table_bytes(self.at.capacity(), size_of::<(u64, u32)>())
            + self.pairs.capacity() * size_of::<(u32, u32)>()
    }

    fn charge(&mut self) -> Result<(), BudgetFailure> {
        let b = self.bytes();
        self.held.try_set(b)
    }

    /// Reserve room for `nodes` more nodes and one more pair, charging the growth of
    /// each buffer before it is reserved.
    fn room(&mut self, nodes: usize) -> Result<(), BudgetFailure> {
        let ids = self.ids.len() + nodes > self.ids.capacity();
        let at = self.at.len() + nodes > self.at.capacity();
        let pairs = self.pairs.len() == self.pairs.capacity();
        if !(ids || at || pairs) {
            return Ok(());
        }
        let grow = |cap: usize, need: usize| {
            if need > cap {
                (cap * 2).max(need).max(16)
            } else {
                cap
            }
        };
        let ids_cap = grow(self.ids.capacity(), self.ids.len() + nodes);
        let at_cap = grow(self.at.capacity(), self.at.len() + nodes);
        let pairs_cap = grow(self.pairs.capacity(), self.pairs.len() + 1);
        // a growing buffer keeps its old allocation while it builds the new one, so
        // both are charged until the growth is done
        let grown = |old: usize, new: usize| if new > old { old + new } else { old };
        self.held.try_set(
            grown(
                self.ids.capacity() * size_of::<u64>(),
                ids_cap * size_of::<u64>(),
            ) + grown(
                table_bytes(self.at.capacity(), size_of::<(u64, u32)>()),
                table_bytes(at_cap, size_of::<(u64, u32)>()),
            ) + grown(
                self.pairs.capacity() * size_of::<(u32, u32)>(),
                pairs_cap * size_of::<(u32, u32)>(),
            ),
        )?;
        self.ids.reserve_exact(ids_cap - self.ids.len());
        self.at.reserve(at_cap - self.at.len());
        self.pairs.reserve_exact(pairs_cap - self.pairs.len());
        self.charge()
    }

    #[inline]
    fn node(&mut self, id: u64) -> Result<(u32, bool), BudgetFailure> {
        if let Some(&n) = self.at.get(&id) {
            return Ok((n, false));
        }
        self.room(1)?;
        let n = self.ids.len() as u32;
        self.ids.push(id);
        self.at.insert(id, n);
        Ok((n, true))
    }

    #[inline]
    fn edge(&mut self, s: u64, o: u64) -> Result<(), BudgetFailure> {
        let (a, _) = self.node(s)?;
        let (b, _) = self.node(o)?;
        self.room(0)?;
        self.pairs.push((a, b));
        Ok(())
    }

    /// The compressed adjacency of the pairs, sorted and without duplicates.
    fn finish(self, emit: Vec<bool>) -> Result<Local, BudgetFailure> {
        let (local, _at, _at_held) = self.finish_with_map(emit)?;
        Ok(local)
    }

    /// [`finish`](Self::finish), also returning the node-id map and its charge without
    /// cloning.
    fn finish_with_map(
        mut self,
        emit: Vec<bool>,
    ) -> Result<(Local, FxHashMap<u64, u32>, Held), BudgetFailure> {
        self.charge()?;
        let n = self.ids.len();
        let mut pairs = std::mem::take(&mut self.pairs);
        pairs.sort_unstable();
        pairs.dedup();
        self.held
            .try_grow((n + 1 + pairs.len()) * size_of::<u32>())?;
        let mut off = vec![0u32; n + 1];
        for &(a, _) in &pairs {
            off[a as usize + 1] += 1;
        }
        for i in 0..n {
            off[i + 1] += off[i];
        }
        let adj: Vec<u32> = pairs.iter().map(|&(_, b)| b).collect();
        drop(pairs);
        let at = std::mem::take(&mut self.at);
        let at_bytes = table_bytes(at.capacity(), size_of::<(u64, u32)>());
        let local_bytes = self.ids.capacity() * size_of::<u64>()
            + (off.capacity() + adj.capacity()) * size_of::<u32>()
            + emit.capacity();
        let mut held = self.held;
        held.try_set(local_bytes + at_bytes)?;
        let at_held = held.split(at_bytes);
        Ok((
            Local {
                ids: self.ids,
                off,
                adj,
                emit,
                reversed: false,
                _held: held,
            },
            at,
            at_held,
        ))
    }
}

impl Local {
    /// The graph of predicate `q`'s edges in rows `< now` that the edges from row `mark`
    /// on change: all of it when those are many, or include the base tier.
    fn build(
        g: &Graph,
        q: u64,
        mark: u32,
        now: u32,
        shape: Shape,
        budget: &Arc<Budget>,
        cancel: Option<&AtomicBool>,
    ) -> anyhow::Result<Local> {
        let total = g.count_p(q, 0, now);
        let fresh = g.count_p(q, mark, now);
        let mut poll = Poll::new(cancel);
        if mark >= g.offset()
            && fresh.saturating_mul(4) < total
            && let Some(l) = Local::around(g, q, (mark, now), shape, budget, total / 2, &mut poll)?
        {
            return Ok(l);
        }
        Local::scan(g, q, now, budget, &mut poll)
    }

    /// Every edge of `q` in rows `< now`.
    fn scan(
        g: &Graph,
        q: u64,
        now: u32,
        budget: &Arc<Budget>,
        poll: &mut Poll,
    ) -> anyhow::Result<Local> {
        let mut b = Builder::new(budget);
        if let Some(base) = g.base()
            && g.reads_base(0, now)
        {
            let mut err: Option<anyhow::Error> = None;
            base.for_each(None, Some(q), None, |t| {
                match b
                    .edge(t[0], t[2])
                    .map_err(anyhow::Error::from)
                    .and_then(|()| poll.tick(1))
                {
                    Ok(()) => true,
                    Err(e) => {
                        err = Some(e);
                        false
                    }
                }
            })?;
            if let Some(e) = err {
                return Err(e);
            }
        }
        let c = g.cands(None, Some(q), None, g.offset(), now);
        for r in c.iter() {
            poll.tick(1)?;
            if !g.alive(r) {
                continue;
            }
            let t = g.triple(r);
            if t[1] == q {
                b.edge(t[0], t[2])?;
            }
        }
        Ok(b.finish(Vec::new())?)
    }

    /// The nodes the new edges of rows `[mark, now)` change, and every node they reach,
    /// read through the indexes; `None` when that passes `limit` edges.
    fn around(
        g: &Graph,
        q: u64,
        (mark, now): (u32, u32),
        shape: Shape,
        budget: &Arc<Budget>,
        limit: usize,
        poll: &mut Poll,
    ) -> anyhow::Result<Option<Local>> {
        let mut memo = Memo::default();
        let mut b = Builder::new(budget);
        // the search's own sets and queues, each growth charged before it is reserved
        let mut search = Held::new(budget);
        let c = g.cands(None, Some(q), None, mark.max(g.offset()), now);
        let per = if shape == Shape::Equivalence { 2 } else { 1 };
        search.try_grow(c.len() * per * size_of::<u64>())?;
        let mut seeds: Vec<u64> = Vec::with_capacity(c.len() * per);
        for r in c.iter() {
            poll.tick(1)?;
            if g.alive(r) {
                let t = g.triple(r);
                if t[1] == q {
                    seeds.push(t[0]);
                    if shape == Shape::Equivalence {
                        seeds.push(t[2]);
                    }
                }
            }
        }
        seeds.sort_unstable();
        seeds.dedup();
        let result = (|| -> anyhow::Result<Option<Local>> {
            // the sources whose pairs change: those that reach a new edge (or, for an
            // equivalence, the classes of its ends)
            let mut sources: FxHashSet<u64> = FxHashSet::default();
            let mut queue: Vec<u64> = Vec::new();
            for &s in &seeds {
                if insert(&mut sources, s, &mut search)? {
                    push(&mut queue, s, &mut search)?;
                }
            }
            let mut steps = 0usize;
            while let Some(x) = queue.pop() {
                let mut visit = |p: u64| -> anyhow::Result<bool> {
                    steps += 1;
                    if steps > limit {
                        return Ok(false);
                    }
                    poll.tick(1)?;
                    if insert(&mut sources, p, &mut search)? {
                        push(&mut queue, p, &mut search)?;
                    }
                    Ok(true)
                };
                g.each_subject(&mut memo, q, x, now, &mut visit)?;
                if shape == Shape::Equivalence {
                    g.each_object(&mut memo, x, q, now, &mut visit)?;
                }
                if steps > limit {
                    return Ok(None);
                }
            }
            // every node the sources reach, with its out-edges
            let mut seen: FxHashSet<u64> = FxHashSet::default();
            let mut order: Vec<u64> = Vec::new();
            for &s in &sources {
                push(&mut order, s, &mut search)?;
            }
            order.sort_unstable();
            for &s in &order {
                insert(&mut seen, s, &mut search)?;
                b.node(s)?;
            }
            let mut queue = order;
            let mut edges = 0usize;
            while let Some(x) = queue.pop() {
                g.each_object(&mut memo, x, q, now, |o| {
                    edges += 1;
                    if edges > limit {
                        return Ok(false);
                    }
                    poll.tick(1)?;
                    b.edge(x, o)?;
                    if insert(&mut seen, o, &mut search)? {
                        push(&mut queue, o, &mut search)?;
                    }
                    Ok(true)
                })?;
                if edges > limit {
                    return Ok(None);
                }
            }
            search.try_grow(b.ids.len())?;
            let emit: Vec<bool> = b.ids.iter().map(|id| sources.contains(id)).collect();
            Ok(Some(
                std::mem::replace(&mut b, Builder::new(budget)).finish(emit)?,
            ))
        })();
        if let Some(base) = g.base() {
            base.add_scans(memo.scans);
        }
        result
    }

    /// The nodes that `sources` reach through predicate `q`'s edges in rows `< now`,
    /// forward or, with the edges reversed, backward, each with all its edges, read
    /// through the indexes. Only the sources emit their pairs.
    fn seeded(
        g: &Graph,
        q: u64,
        sources: &[u64],
        now: u32,
        dir: Dir,
        budget: &Arc<Budget>,
        cancel: Option<&AtomicBool>,
    ) -> anyhow::Result<Local> {
        let mut poll = Poll::new(cancel);
        let mut memo = Memo::default();
        let mut b = Builder::new(budget);
        // the search's own sets and queue, each growth charged before it is reserved
        let mut search = Held::new(budget);
        let result = (|| -> anyhow::Result<Local> {
            let mut from: FxHashSet<u64> = FxHashSet::default();
            let mut seen: FxHashSet<u64> = FxHashSet::default();
            let mut queue: Vec<u64> = Vec::new();
            for &s in sources {
                poll.tick(1)?;
                insert(&mut from, s, &mut search)?;
                if insert(&mut seen, s, &mut search)? {
                    b.node(s)?;
                    push(&mut queue, s, &mut search)?;
                }
            }
            while let Some(x) = queue.pop() {
                let visit = |y: u64| -> anyhow::Result<bool> {
                    poll.tick(1)?;
                    b.edge(x, y)?;
                    if insert(&mut seen, y, &mut search)? {
                        push(&mut queue, y, &mut search)?;
                    }
                    Ok(true)
                };
                match dir {
                    Dir::Forward => g.each_object(&mut memo, x, q, now, visit)?,
                    Dir::Backward => g.each_subject(&mut memo, q, x, now, visit)?,
                }
            }
            search.try_grow(b.ids.len())?;
            let emit: Vec<bool> = b.ids.iter().map(|id| from.contains(id)).collect();
            let mut l = std::mem::replace(&mut b, Builder::new(budget)).finish(emit)?;
            l.reversed = dir == Dir::Backward;
            Ok(l)
        })();
        if let Some(base) = g.base() {
            base.add_scans(memo.scans);
        }
        result
    }

    #[inline]
    fn out(&self, x: u32) -> &[u32] {
        &self.adj[self.off[x as usize] as usize..self.off[x as usize + 1] as usize]
    }

    #[inline]
    fn emits(&self, x: u32) -> bool {
        self.emit.is_empty() || self.emit[x as usize]
    }

    /// Whether a literal has an out-edge, so that a `notEqual` of values may differ from
    /// a comparison of ids.
    fn literal_subject(&self, terms: &Terms) -> bool {
        if self.reversed {
            // a subject of the relation is the end of a reversed edge
            return self
                .adj
                .iter()
                .any(|&y| terms.kind(self.ids[y as usize]) == Kind::Literal);
        }
        (0..self.ids.len())
            .any(|x| self.off[x] < self.off[x + 1] && terms.kind(self.ids[x]) == Kind::Literal)
    }

    /// The pairs `(x, z)` of the transitive closure, for the nodes that emit, that are
    /// not edges, without `x = z` when `irreflexive`, into `sink`.
    fn transitive(
        &self,
        q: u64,
        irreflexive: bool,
        sink: &mut Sink<'_>,
        cancel: Option<&AtomicBool>,
    ) -> anyhow::Result<()> {
        let n = self.ids.len();
        let mut poll = Poll::new(cancel);
        let (comp, ncomp) = self.components(sink.budget(), &mut poll)?;
        // comp, members, stamp and set by node; moff, fill, readers, the successor stamp
        // and succ by component; and the outer vector of the reachable sets
        let mut work = Held::try_new(
            sink.budget(),
            n * 4 * size_of::<u32>()
                + ncomp * (4 * size_of::<u32>() + size_of::<usize>() + size_of::<Vec<u32>>()),
        )?;
        // the members of each component, in the order of the components
        let mut moff = vec![0u32; ncomp + 1];
        for &c in &comp {
            moff[c as usize + 1] += 1;
        }
        for i in 0..ncomp {
            moff[i + 1] += moff[i];
        }
        let mut members = vec![0u32; n];
        let mut fill = moff.clone();
        for (x, &c) in comp.iter().enumerate() {
            members[fill[c as usize] as usize] = x as u32;
            fill[c as usize] += 1;
        }
        drop(fill);
        // the distinct components that point into each one, so that its reachable set is
        // released once they have read it
        let mut stamp = vec![u32::MAX; ncomp.max(n)];
        let mut readers = vec![0u32; ncomp];
        for c in 0..ncomp {
            for &x in &members[moff[c] as usize..moff[c + 1] as usize] {
                poll.tick(self.out(x).len() + 1)?;
                for &y in self.out(x) {
                    let d = comp[y as usize] as usize;
                    if d != c && stamp[d] != c as u32 {
                        stamp[d] = c as u32;
                        readers[d] += 1;
                    }
                }
            }
        }
        stamp.iter_mut().for_each(|s| *s = u32::MAX);
        // per component: the nodes reachable from it, itself included
        let mut reach: Vec<Vec<u32>> = (0..ncomp).map(|_| Vec::new()).collect();
        let mut succ: Vec<usize> = Vec::with_capacity(ncomp);
        let mut seen_succ = vec![u32::MAX; ncomp];
        let mut set: Vec<u32> = Vec::with_capacity(n);
        // Tarjan numbers components in reverse topological order: a component's
        // successors come before it
        for c in 0..ncomp {
            poll.tick(1)?;
            let mine = &members[moff[c] as usize..moff[c + 1] as usize];
            succ.clear();
            let mut cyclic = mine.len() > 1;
            for &x in mine {
                poll.tick(self.out(x).len())?;
                for &y in self.out(x) {
                    let d = comp[y as usize] as usize;
                    if d == c {
                        cyclic = true;
                    } else if seen_succ[d] != c as u32 {
                        seen_succ[d] = c as u32;
                        succ.push(d);
                    }
                }
            }
            // the nodes reachable by a path of one edge or more
            set.clear();
            let stamp_c = c as u32;
            for &d in &succ {
                poll.tick(reach[d].len())?;
                for &z in &reach[d] {
                    if stamp[z as usize] != stamp_c {
                        stamp[z as usize] = stamp_c;
                        set.push(z);
                    }
                }
            }
            for &d in &succ {
                readers[d] -= 1;
                if readers[d] == 0 {
                    work.shrink(reach[d].capacity() * size_of::<u32>());
                    reach[d] = Vec::new();
                }
            }
            if cyclic {
                for &x in mine {
                    if stamp[x as usize] != stamp_c {
                        stamp[x as usize] = stamp_c;
                        set.push(x);
                    }
                }
            }
            set.sort_unstable();
            for &x in mine {
                if !self.emits(x) {
                    continue;
                }
                poll.tick(set.len())?;
                let have = self.out(x);
                let mut h = 0;
                for &z in &set {
                    if irreflexive && z == x {
                        continue;
                    }
                    while h < have.len() && have[h] < z {
                        h += 1;
                    }
                    if h < have.len() && have[h] == z {
                        continue;
                    }
                    let (a, b) = (self.ids[x as usize], self.ids[z as usize]);
                    sink.emit(if self.reversed { [b, q, a] } else { [a, q, b] })?;
                }
            }
            if readers[c] > 0 {
                let mut r = Vec::with_capacity(set.len() + if cyclic { 0 } else { mine.len() });
                r.extend_from_slice(&set);
                if !cyclic {
                    r.extend_from_slice(mine);
                    r.sort_unstable();
                }
                work.try_grow(r.capacity() * size_of::<u32>())?;
                reach[c] = r;
            }
        }
        Ok(())
    }

    /// Tarjan's strongly connected components, iteratively: the component of each node
    /// and the number of components, numbered in reverse topological order.
    fn components(
        &self,
        budget: &Arc<Budget>,
        poll: &mut Poll,
    ) -> anyhow::Result<(Vec<u32>, usize)> {
        let n = self.ids.len();
        // index, low, comp, on, and the node and call stacks at their deepest
        let _work = Held::try_new(
            budget,
            n * (3 * size_of::<u32>()
                + size_of::<bool>()
                + size_of::<u32>()
                + size_of::<(u32, u32)>()),
        )?;
        const NONE: u32 = u32::MAX;
        let mut index = vec![NONE; n];
        let mut low = vec![0u32; n];
        let mut on = vec![false; n];
        let mut comp = vec![NONE; n];
        let mut stack: Vec<u32> = Vec::new();
        let mut call: Vec<(u32, u32)> = Vec::new();
        let mut next = 0u32;
        let mut ncomp = 0u32;
        for root in 0..n as u32 {
            if index[root as usize] != NONE {
                continue;
            }
            call.push((root, self.off[root as usize]));
            index[root as usize] = next;
            low[root as usize] = next;
            next += 1;
            stack.push(root);
            on[root as usize] = true;
            while let Some(&mut (v, ref mut e)) = call.last_mut() {
                poll.tick(1)?;
                let end = self.off[v as usize + 1];
                if *e < end {
                    let w = self.adj[*e as usize];
                    *e += 1;
                    if index[w as usize] == NONE {
                        index[w as usize] = next;
                        low[w as usize] = next;
                        next += 1;
                        stack.push(w);
                        on[w as usize] = true;
                        call.push((w, self.off[w as usize]));
                    } else if on[w as usize] {
                        low[v as usize] = low[v as usize].min(index[w as usize]);
                    }
                    continue;
                }
                call.pop();
                if let Some(&(u, _)) = call.last() {
                    low[u as usize] = low[u as usize].min(low[v as usize]);
                }
                if low[v as usize] == index[v as usize] {
                    loop {
                        let w = stack.pop().expect("the component's nodes are stacked");
                        on[w as usize] = false;
                        comp[w as usize] = ncomp;
                        if w == v {
                            break;
                        }
                    }
                    ncomp += 1;
                }
            }
        }
        Ok((comp, ncomp as usize))
    }

    /// The pairs `(x, y)` of the classes of the symmetric and transitive closure, for the
    /// nodes that emit, that are not edges, without `x = y` when `irreflexive`, into
    /// `sink`. `false`, before any pair is emitted, when a class holds two literals with
    /// distinct ids and equal values, where the rules' `notEqual` differs from a
    /// comparison of ids.
    fn equivalence(
        &self,
        q: u64,
        irreflexive: bool,
        terms: &Terms,
        sink: &mut Sink<'_>,
        cancel: Option<&AtomicBool>,
    ) -> anyhow::Result<bool> {
        let n = self.ids.len();
        let mut poll = Poll::new(cancel);
        // parent, roots, moff, members and fill
        let _work = Held::try_new(sink.budget(), (n * 5 + 1) * size_of::<u32>())?;
        let mut parent: Vec<u32> = (0..n as u32).collect();
        fn find(parent: &mut [u32], mut x: u32) -> u32 {
            while parent[x as usize] != x {
                let p = parent[x as usize];
                parent[x as usize] = parent[p as usize];
                x = p;
            }
            x
        }
        for x in 0..n as u32 {
            poll.tick(self.out(x).len() + 1)?;
            for &y in self.out(x) {
                let (a, b) = (find(&mut parent, x), find(&mut parent, y));
                if a != b {
                    let (lo, hi) = if a < b { (a, b) } else { (b, a) };
                    parent[hi as usize] = lo;
                }
            }
        }
        // the members of each class, by root
        let roots: Vec<u32> = (0..n as u32).map(|x| find(&mut parent, x)).collect();
        let mut moff = vec![0u32; n + 1];
        for &r in &roots {
            moff[r as usize + 1] += 1;
        }
        for i in 0..n {
            moff[i + 1] += moff[i];
        }
        let mut members = vec![0u32; n];
        let mut fill = moff.clone();
        for (x, &r) in roots.iter().enumerate() {
            members[fill[r as usize] as usize] = x as u32;
            fill[r as usize] += 1;
        }
        drop(fill);
        let classes = (0..n).filter_map(|r| {
            let class = &members[moff[r] as usize..moff[r + 1] as usize];
            (class.len() >= 2 && class.iter().any(|&x| self.emits(x))).then_some(class)
        });
        // every class's literals are checked before a pair is emitted, so that a class
        // the rules' `notEqual` treats differently leaves nothing of the predicate merged
        for class in classes.clone() {
            poll.tick(1)?;
            let mut literals: Vec<u64> = Vec::new();
            for &x in class {
                let id = self.ids[x as usize];
                if terms.kind(id) == Kind::Literal {
                    if literals.len() == 64 {
                        return Ok(false);
                    }
                    literals.push(id);
                }
            }
            for (i, &a) in literals.iter().enumerate() {
                if literals[i + 1..].iter().any(|&b| same_value(terms, a, b)) {
                    return Ok(false);
                }
            }
        }
        for class in classes {
            poll.tick(1)?;
            // members are in ascending local order, as the adjacency is
            for &x in class {
                if !self.emits(x) {
                    continue;
                }
                poll.tick(class.len())?;
                let have = self.out(x);
                let mut h = 0;
                for &z in class {
                    if irreflexive && z == x {
                        continue;
                    }
                    while h < have.len() && have[h] < z {
                        h += 1;
                    }
                    if h < have.len() && have[h] == z {
                        continue;
                    }
                    sink.emit([self.ids[x as usize], q, self.ids[z as usize]])?;
                }
            }
        }
        Ok(true)
    }
}

/// A guarded symmetric rule `(?p G O), (?x ?p ?y) [notLiteral(?y)] -> (?y ?p ?x)`,
/// such as `prp-symp`: returns `(G, O, not_literal_y)`.
fn guarded_symmetric(r: &CRule) -> Option<(u64, u64, bool)> {
    let [CHead::Triple(hs, hp, ho)] = r.head.as_slice() else {
        return None;
    };
    let (y, p, x) = (var(*hs)?, var(*hp)?, var(*ho)?);
    if x == y || p == x || p == y || r.nvars != 3 || r.atoms.len() != 2 {
        return None;
    }
    let not_lit_y = match r.builtins.as_slice() {
        [] => false,
        [b] if b.kind == BuiltinKind::NotLiteral && b.args.as_slice() == [Slot::Var(y)] => true,
        _ => return None,
    };
    let gi = r.atoms.iter().position(|a| {
        a.s == Slot::Var(p) && matches!(a.p, Slot::Const(_)) && matches!(a.o, Slot::Const(_))
    })?;
    let edge = &r.atoms[1 - gi];
    if edge.s != Slot::Var(x) || edge.p != Slot::Var(p) || edge.o != Slot::Var(y) {
        return None;
    }
    let (Slot::Const(gp), Slot::Const(go)) = (r.atoms[gi].p, r.atoms[gi].o) else {
        return None;
    };
    Some((gp, go, not_lit_y))
}

/// Maximum number of condensed SCCs for which [`BaseReach::Directed`] precomputes an
/// O(1) SCC reachability bitset (`2048 * 32 * 8 = 512 KiB`).
const MAX_BITSET_SCCS: usize = 2048;

/// Maximum explicit-graph edges traversed by a single fallback DFS probe in
/// [`BaseReach::Directed::proves`]. Exceeding this cap conservatively returns `false` so
/// saturation re-closes the predicate in one O(|V| + |E|) SCC pass.
const MAX_PROBE_STEPS: usize = 4096;

/// Visit batch size for operator reachability probes and scans.
const REACH_VISIT_BATCH: u64 = 1024;

/// Reusable, budget-charged scratch workspace for directed explicit-edge reachability
/// probes in [`BaseReach::proves`].
pub(crate) struct ReachScratch {
    seen: FxHashSet<u32>,
    stack: Vec<u32>,
    held: Held,
}

impl ReachScratch {
    pub(crate) fn new(budget: &Arc<Budget>) -> ReachScratch {
        ReachScratch {
            seen: FxHashSet::default(),
            stack: Vec::new(),
            held: Held::new(budget),
        }
    }

    fn clear(&mut self) {
        self.seen.clear();
        self.stack.clear();
    }

    fn insert_seen(&mut self, v: u32) -> Result<bool, BudgetFailure> {
        if self.seen.contains(&v) {
            return Ok(false);
        }
        let (cap, len) = (self.seen.capacity(), self.seen.len());
        let to = table_grown(cap, len, 1);
        if to != cap {
            self.held.try_grow(table_bytes(to, size_of::<u32>()))?;
            self.seen.reserve(1);
            self.held.shrink(table_bytes(cap, size_of::<u32>()));
        }
        Ok(self.seen.insert(v))
    }

    fn push_stack(&mut self, v: u32) -> Result<(), BudgetFailure> {
        push_moved(&mut self.stack, v, &mut self.held)
    }
}

/// Reachability over the confirmed explicit edges of one operator-governed predicate `q`
/// during incremental deletion.
enum BaseReach {
    /// Undirected connected components (equivalence operator, or transitive + explicit
    /// symmetric property with no literal objects).
    Undirected {
        /// node id -> (component root, whether the component has >= 2 nodes or a self-loop)
        comp: FxHashMap<u64, (u32, bool)>,
        irreflexive: bool,
        _held: Held,
    },
    /// Directed strongly connected components + compressed adjacency over explicit edges,
    /// with an optional precomputed SCC reachability bitset for O(1) lookups.
    Directed {
        at: FxHashMap<u64, u32>,
        off: Vec<u32>,
        adj: Vec<u32>,
        comp: Vec<u32>,
        cyclic: Vec<bool>,
        reach_bits: Option<(Vec<u64>, usize)>,
        irreflexive: bool,
        _at_held: Held,
        _graph_held: Held,
        _extra_held: Held,
        _bits_held: Option<Held>,
    },
}

impl BaseReach {
    /// Whether `[x, q, z]` follows from the confirmed explicit edges of `q` via the
    /// closure operator. Counts DFS visits against `visit` and polls `cancel`.
    fn proves(
        &self,
        x: u64,
        z: u64,
        scratch: &mut ReachScratch,
        cancel: Option<&AtomicBool>,
        visit: &mut dyn FnMut(u64) -> anyhow::Result<()>,
    ) -> anyhow::Result<bool> {
        match self {
            BaseReach::Undirected {
                comp, irreflexive, ..
            } => {
                if *irreflexive && x == z {
                    return Ok(false);
                }
                let (Some(&(cx, cyc)), Some(&(cz, _))) = (comp.get(&x), comp.get(&z)) else {
                    return Ok(false);
                };
                Ok(cx == cz && (x != z || cyc))
            }
            BaseReach::Directed {
                at,
                off,
                adj,
                comp,
                cyclic,
                reach_bits,
                irreflexive,
                ..
            } => {
                if *irreflexive && x == z {
                    return Ok(false);
                }
                let (Some(&ix), Some(&iz)) = (at.get(&x), at.get(&z)) else {
                    return Ok(false);
                };
                let (cx, cz) = (comp[ix as usize], comp[iz as usize]);
                if cx == cz {
                    return Ok(x != z || cyclic[cx as usize]);
                }
                // Tarjan numbers SCCs in reverse topological order: a lower SCC id cannot
                // reach a higher SCC id.
                if cx < cz {
                    return Ok(false);
                }
                if let Some((bits, words)) = reach_bits {
                    let hit = (bits[cx as usize * *words + (cz as usize / 64)]
                        & (1u64 << (cz as usize % 64)))
                        != 0;
                    return Ok(hit);
                }
                // Bounded, charged, cancellable DFS over the explicit base graph of `q`,
                // pruned by SCC order and short-circuiting on entering `cz`.
                scratch.clear();
                scratch.insert_seen(ix)?;
                scratch.push_stack(ix)?;
                let mut steps: usize = 0;
                let mut pending: u64 = 0;
                while let Some(u) = scratch.stack.pop() {
                    let lo = off[u as usize] as usize;
                    let hi = off[u as usize + 1] as usize;
                    for &v in &adj[lo..hi] {
                        steps += 1;
                        pending += 1;
                        let cv = comp[v as usize];
                        if cv == cz {
                            if pending > 0 {
                                visit(pending)?;
                            }
                            return Ok(true);
                        }
                        if pending >= REACH_VISIT_BATCH {
                            visit(std::mem::take(&mut pending))?;
                            if cancel.is_some_and(|c| c.load(Ordering::Relaxed)) {
                                anyhow::bail!("reasoning cancelled");
                            }
                        }
                        if steps >= MAX_PROBE_STEPS {
                            if pending > 0 {
                                visit(pending)?;
                            }
                            return Ok(false);
                        }
                        if cv > cz && scratch.insert_seen(v)? {
                            scratch.push_stack(v)?;
                        }
                    }
                }
                if pending > 0 {
                    visit(pending)?;
                }
                Ok(false)
            }
        }
    }

    fn build_undirected(
        q: u64,
        g: &Graph,
        irreflexive: bool,
        budget: &Arc<Budget>,
        cancel: Option<&AtomicBool>,
        visit: &mut dyn FnMut(u64) -> anyhow::Result<()>,
    ) -> anyhow::Result<BaseReach> {
        let mut poll = Poll::new(cancel);
        let mut b = Builder::new(budget);
        let mut loops_held = Held::new(budget);
        let mut self_loops: Vec<u32> = Vec::new();
        let mut pending: u64 = 0;
        let mut add = |s: u64, o: u64| -> anyhow::Result<()> {
            let (a, _) = b.node(s)?;
            let (c, _) = b.node(o)?;
            if a == c {
                push_moved(&mut self_loops, a, &mut loops_held)?;
            }
            b.room(0)?;
            b.pairs.push((a, c));
            pending += 1;
            if pending >= REACH_VISIT_BATCH {
                visit(std::mem::take(&mut pending))?;
            }
            poll.tick(1)
        };
        if let Some(base) = g.base() {
            let mut err: Option<anyhow::Error> = None;
            base.for_each(None, Some(q), None, |t| match add(t[0], t[2]) {
                Ok(()) => true,
                Err(e) => {
                    err = Some(e);
                    false
                }
            })?;
            if let Some(e) = err {
                return Err(e);
            }
        }
        let c = g.cands(None, Some(q), None, g.offset(), g.len());
        for r in c.iter() {
            if g.alive(r) && g.flagged(r) {
                let t = g.triple(r);
                if t[1] == q {
                    add(t[0], t[2])?;
                }
            }
        }
        if pending > 0 {
            visit(std::mem::take(&mut pending))?;
        }
        let n = b.ids.len();
        let _work = Held::try_new(budget, n * (2 * size_of::<u32>() + size_of::<bool>()))?;
        let mut parent: Vec<u32> = (0..n as u32).collect();
        fn find(parent: &mut [u32], mut x: u32) -> u32 {
            while parent[x as usize] != x {
                let p = parent[x as usize];
                parent[x as usize] = parent[p as usize];
                x = p;
            }
            x
        }
        for &(a, c) in &b.pairs {
            poll.tick(1)?;
            let (ra, rb) = (find(&mut parent, a), find(&mut parent, c));
            if ra != rb {
                let (lo, hi) = if ra < rb { (ra, rb) } else { (rb, ra) };
                parent[hi as usize] = lo;
            }
        }
        let mut sizes = vec![0u32; n];
        let mut has_loop = vec![false; n];
        for i in 0..n as u32 {
            let r = find(&mut parent, i);
            sizes[r as usize] += 1;
        }
        for a in self_loops {
            let r = find(&mut parent, a);
            has_loop[r as usize] = true;
        }
        drop(loops_held);
        let mut comp_held = Held::try_new(
            budget,
            table_bytes(table_capacity(n), size_of::<(u64, (u32, bool))>()),
        )?;
        let mut comp: FxHashMap<u64, (u32, bool)> =
            FxHashMap::with_capacity_and_hasher(n, Default::default());
        for (i, &id) in b.ids.iter().enumerate() {
            let r = find(&mut parent, i as u32);
            comp.insert(id, (r, sizes[r as usize] >= 2 || has_loop[r as usize]));
        }
        comp_held.try_set(table_bytes(
            comp.capacity(),
            size_of::<(u64, (u32, bool))>(),
        ))?;
        visit(0)?;
        Ok(BaseReach::Undirected {
            comp,
            irreflexive,
            _held: comp_held,
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn build_directed(
        q: u64,
        g: &Graph,
        irreflexive: bool,
        add_non_literal_reverse: bool,
        terms: &Terms,
        budget: &Arc<Budget>,
        cancel: Option<&AtomicBool>,
        visit: &mut dyn FnMut(u64) -> anyhow::Result<()>,
    ) -> anyhow::Result<BaseReach> {
        let mut poll = Poll::new(cancel);
        let mut b = Builder::new(budget);
        let mut pending: u64 = 0;
        let mut add = |s: u64, o: u64| -> anyhow::Result<()> {
            b.edge(s, o)?;
            if add_non_literal_reverse && terms.kind(o) != Kind::Literal {
                b.edge(o, s)?;
            }
            pending += 1;
            if pending >= REACH_VISIT_BATCH {
                visit(std::mem::take(&mut pending))?;
            }
            poll.tick(1)
        };
        if let Some(base) = g.base() {
            let mut err: Option<anyhow::Error> = None;
            base.for_each(None, Some(q), None, |t| match add(t[0], t[2]) {
                Ok(()) => true,
                Err(e) => {
                    err = Some(e);
                    false
                }
            })?;
            if let Some(e) = err {
                return Err(e);
            }
        }
        let c = g.cands(None, Some(q), None, g.offset(), g.len());
        for r in c.iter() {
            if g.alive(r) && g.flagged(r) {
                let t = g.triple(r);
                if t[1] == q {
                    add(t[0], t[2])?;
                }
            }
        }
        if pending > 0 {
            visit(std::mem::take(&mut pending))?;
        }
        let (local, at, at_held) = b.finish_with_map(Vec::new())?;
        let (comp, ncomp) = local.components(budget, &mut poll)?;
        let mut extra_held = Held::try_new(budget, comp.capacity() * size_of::<u32>() + ncomp)?;
        let sizes_held = Held::try_new(budget, ncomp * size_of::<u32>())?;
        let mut sizes = vec![0u32; ncomp];
        for &c in &comp {
            sizes[c as usize] += 1;
        }
        let mut cyclic: Vec<bool> = sizes.iter().map(|&sz| sz >= 2).collect();
        drop(sizes);
        drop(sizes_held);
        for u in 0..local.ids.len() as u32 {
            let cu = comp[u as usize] as usize;
            if !cyclic[cu] && local.out(u).binary_search(&u).is_ok() {
                cyclic[cu] = true;
            }
        }
        extra_held.try_set(comp.capacity() * size_of::<u32>() + cyclic.capacity())?;
        visit(0)?;
        // Precompute O(1) SCC reachability bitset for moderate DAGs when budget allows.
        let mut reach_bits: Option<(Vec<u64>, usize)> = None;
        let mut bits_held: Option<Held> = None;
        if ncomp > 0 && ncomp <= MAX_BITSET_SCCS {
            let words = ncomp.div_ceil(64);
            let bits_bytes = ncomp.saturating_mul(words).saturating_mul(size_of::<u64>());
            let n = local.ids.len();
            let work_bytes = (ncomp + 1) * 2 * size_of::<u32>() + n * size_of::<u32>();
            if let Ok(bh) = Held::try_new(budget, bits_bytes)
                && let Ok(_wh) = Held::try_new(budget, work_bytes)
            {
                visit(0)?;
                let mut moff = vec![0u32; ncomp + 1];
                for &c in &comp {
                    moff[c as usize + 1] += 1;
                }
                for i in 0..ncomp {
                    moff[i + 1] += moff[i];
                }
                let mut members = vec![0u32; n];
                let mut fill = moff.clone();
                for (x, &c) in comp.iter().enumerate() {
                    members[fill[c as usize] as usize] = x as u32;
                    fill[c as usize] += 1;
                }
                drop(fill);
                let mut bits = vec![0u64; ncomp * words];
                let mut bit_visits: u64 = 0;
                for c in 0..ncomp {
                    bits[c * words + (c / 64)] |= 1u64 << (c % 64);
                    let mine = &members[moff[c] as usize..moff[c + 1] as usize];
                    for &u in mine {
                        let out = local.out(u);
                        poll.tick(out.len() + 1)?;
                        for &v in out {
                            let d = comp[v as usize] as usize;
                            if d != c {
                                let (lo_s, hi_s) = bits.split_at_mut(c * words);
                                let src = &lo_s[d * words..(d + 1) * words];
                                let dst = &mut hi_s[..words];
                                for (a, &b) in dst.iter_mut().zip(src) {
                                    *a |= b;
                                }
                                bit_visits += 1;
                                if bit_visits >= REACH_VISIT_BATCH {
                                    visit(std::mem::take(&mut bit_visits))?;
                                }
                            }
                        }
                    }
                }
                if bit_visits > 0 {
                    visit(bit_visits)?;
                }
                reach_bits = Some((bits, words));
                bits_held = Some(bh);
            }
        }
        Ok(BaseReach::Directed {
            at,
            off: local.off,
            adj: local.adj,
            comp,
            cyclic,
            reach_bits,
            irreflexive,
            _at_held: at_held,
            _graph_held: local._held,
            _extra_held: extra_held,
            _bits_held: bits_held,
        })
    }
}

/// Aggregate of all active closure operators currently governing a predicate `q`.
#[derive(Clone, Copy)]
struct Covering {
    any_equiv: bool,
    all_irreflexive: bool,
}

/// Operator-maintained transitive and equivalence relations during incremental deletion
/// (F23 Phase 4, item 837 / item 521 second half).
pub(crate) struct DeletionOps {
    ops: Vec<Op>,
    /// guarded symmetric rules `(gp, go, not_literal_y)` such as `prp-symp`
    guarded_sym: Vec<(u64, u64, bool)>,
    /// per predicate `q` materialized on demand: confirmed explicit-edge reachability
    /// structures (empty if all covering operators have derived, non-explicit guards)
    base_graphs: FxHashMap<u64, Vec<BaseReach>>,
    /// guarded triples `(q, gp, go)` that were deleted in a dead-guard batch
    dead_guards: FxHashSet<(u64, u64, u64)>,
    /// predicates whose live derived facts were scanned once for operator consequences
    scanned_all: FxHashSet<u64>,
    /// operator predicates that lost facts during deletion, so saturation resets their marks
    dirty: FxHashSet<u64>,
    /// dead edges seen per operator predicate so far
    dead_counts: FxHashMap<u64, usize>,
    /// budget charge for bookkeeping tables
    held: Held,
}

impl DeletionOps {
    /// Detect active closure operators for incremental deletion, disabling any operator
    /// whose predicate currently meets literal value ambiguity so its rules stay with the
    /// join evaluator.
    pub(crate) fn detect(
        rules: &[CRule],
        g: &Graph,
        terms: &Terms,
        budget: &Arc<Budget>,
    ) -> Result<Option<DeletionOps>, BudgetFailure> {
        let Some(mut closures) = Closures::detect(rules, terms, None, None)? else {
            return Ok(None);
        };
        let guarded_sym: Vec<(u64, u64, bool)> =
            rules.iter().filter_map(guarded_symmetric).collect();
        for op in &mut closures.ops {
            let unsafe_lits = match (op.over, op.shape) {
                // Match `Local::equivalence`: check ambiguous literals unconditionally so
                // a symmetric rule with `notEqual` paired with an unfiltered transitive
                // rule is also covered.
                (Over::Fixed(c), Shape::Equivalence) => {
                    Self::pred_has_ambiguous_literals(g, c, terms)
                }
                (Over::Fixed(c), Shape::Transitive) if op.irreflexive => {
                    Self::pred_has_literal_subject(g, c, terms)
                }
                (Over::Guarded(gp, go), Shape::Transitive) if op.irreflexive => {
                    Self::guarded_has_literal_subject(g, gp, go, terms)
                }
                _ => false,
            };
            if unsafe_lits {
                op.off = true;
            }
        }
        closures.ops.retain(|op| !op.off);
        if closures.ops.is_empty() {
            return Ok(None);
        }
        Ok(Some(DeletionOps {
            ops: closures.ops,
            guarded_sym,
            base_graphs: FxHashMap::default(),
            dead_guards: FxHashSet::default(),
            scanned_all: FxHashSet::default(),
            dirty: FxHashSet::default(),
            dead_counts: FxHashMap::default(),
            held: Held::new(budget),
        }))
    }

    fn charge_tables(&mut self) -> Result<(), BudgetFailure> {
        let bytes = table_bytes(
            self.base_graphs.capacity(),
            size_of::<(u64, Vec<BaseReach>)>(),
        ) + table_bytes(self.dead_guards.capacity(), size_of::<(u64, u64, u64)>())
            + table_bytes(self.scanned_all.capacity(), size_of::<u64>())
            + table_bytes(self.dirty.capacity(), size_of::<u64>())
            + table_bytes(self.dead_counts.capacity(), size_of::<(u64, usize)>());
        self.held.try_set(bytes)
    }

    fn pred_has_literal_subject(g: &Graph, q: u64, terms: &Terms) -> bool {
        if let Some(base) = g.base() {
            let mut found = false;
            let _ = base.for_each(None, Some(q), None, |t| {
                if terms.kind(t[0]) == Kind::Literal {
                    found = true;
                    return false;
                }
                true
            });
            if found {
                return true;
            }
        }
        let c = g.cands(None, Some(q), None, g.offset(), g.len());
        c.iter().any(|r| {
            g.alive(r) && {
                let t = g.triple(r);
                t[1] == q && terms.kind(t[0]) == Kind::Literal
            }
        })
    }

    fn pred_has_ambiguous_literals(g: &Graph, q: u64, terms: &Terms) -> bool {
        let mut lits: Vec<u64> = Vec::new();
        let mut see = |id: u64| -> bool {
            if terms.kind(id) != Kind::Literal {
                return false;
            }
            if lits.contains(&id) {
                return false;
            }
            if lits.len() >= 64 || lits.iter().any(|&other| same_value(terms, id, other)) {
                return true;
            }
            lits.push(id);
            false
        };
        if let Some(base) = g.base() {
            let mut bad = false;
            let _ = base.for_each(None, Some(q), None, |t| {
                if see(t[0]) || see(t[2]) {
                    bad = true;
                    return false;
                }
                true
            });
            if bad {
                return true;
            }
        }
        let c = g.cands(None, Some(q), None, g.offset(), g.len());
        for r in c.iter() {
            if g.alive(r) {
                let t = g.triple(r);
                if t[1] == q && (see(t[0]) || see(t[2])) {
                    return true;
                }
            }
        }
        false
    }

    fn guarded_has_literal_subject(g: &Graph, gp: u64, go: u64, terms: &Terms) -> bool {
        let mut preds: Vec<u64> = Vec::new();
        if let Some(base) = g.base() {
            let _ = base.for_each(None, Some(gp), Some(go), |t| {
                preds.push(t[0]);
                true
            });
        }
        let c = g.cands(None, Some(gp), Some(go), g.offset(), g.len());
        for r in c.iter() {
            if g.alive(r) {
                let t = g.triple(r);
                if t[1] == gp && t[2] == go {
                    preds.push(t[0]);
                }
            }
        }
        preds.sort_unstable();
        preds.dedup();
        preds
            .into_iter()
            .any(|q| Self::pred_has_literal_subject(g, q, terms))
    }

    /// Whether rule `ri` is maintained by an active closure operator.
    pub(crate) fn replaces(&self, ri: usize) -> bool {
        self.ops.iter().any(|op| !op.off && op.rules.contains(&ri))
    }

    /// Whether `t` is currently present as an explicit fact (in the base tier or as a
    /// live flagged row in the derived tier).
    fn is_explicit(g: &Graph, t: &Triple) -> bool {
        g.base().is_some_and(|b| b.contains(t).unwrap_or(false))
            || g.position(t).is_some_and(|r| g.flagged(r))
    }

    /// Whether `t` is currently alive in `g` (base or derived).
    fn is_alive(g: &Graph, t: &Triple) -> bool {
        g.position(t).is_some() || g.base().is_some_and(|b| b.contains(t).unwrap_or(false))
    }

    /// Aggregate of all active operators governing predicate `q` in `g`.
    fn covering(&self, q: u64, g: &Graph) -> Option<Covering> {
        let mut found = false;
        let mut any_equiv = false;
        let mut all_irreflexive = true;
        for op in &self.ops {
            if op.off {
                continue;
            }
            let active = match op.over {
                Over::Fixed(c) => c == q,
                Over::Guarded(gp, go) => {
                    !self.dead_guards.contains(&(q, gp, go)) && Self::is_alive(g, &[q, gp, go])
                }
            };
            if active {
                found = true;
                any_equiv |= op.shape == Shape::Equivalence;
                all_irreflexive &= op.irreflexive;
            }
        }
        found.then_some(Covering {
            any_equiv,
            all_irreflexive,
        })
    }

    /// Whether predicate `q` is governed by an active closure operator.
    pub(crate) fn is_governed(&self, q: u64, g: &Graph) -> bool {
        self.covering(q, g).is_some()
    }

    /// Ensure the confirmed explicit-edge reachability structures for predicate `q` are
    /// built over all active covering operators whose guard is explicit. Operators whose
    /// guard is derived (non-explicit) stay governed for consequences and `dirty_preds`,
    /// without fast-proving pairs via `base_graphs` or poisoning `Search::unprovable`.
    pub(crate) fn ensure_pred(
        &mut self,
        q: u64,
        g: &Graph,
        terms: &Terms,
        budget: &Arc<Budget>,
        cancel: Option<&AtomicBool>,
        visit: &mut dyn FnMut(u64) -> anyhow::Result<()>,
    ) -> anyhow::Result<()> {
        if self.base_graphs.contains_key(&q) {
            return Ok(());
        }
        // Determine which explicit-guard reachability modes are needed across all active
        // covering operators on `q`.
        let mut undir_unfilt = false;
        let mut undir_irref = false;
        let mut dir_rev_unfilt = false;
        let mut dir_rev_irref = false;
        let mut dir_unfilt = false;
        let mut dir_irref = false;
        let mut checked_sym: Option<Option<bool>> = None;
        for op in &self.ops {
            if op.off {
                continue;
            }
            let explicit_guard = match op.over {
                Over::Fixed(c) => c == q,
                Over::Guarded(gp, go) => {
                    !self.dead_guards.contains(&(q, gp, go)) && Self::is_explicit(g, &[q, gp, go])
                }
            };
            if !explicit_guard {
                continue;
            }
            match op.shape {
                Shape::Equivalence => {
                    if op.irreflexive {
                        undir_irref = true;
                    } else {
                        undir_unfilt = true;
                    }
                }
                Shape::Transitive => {
                    // If `q` also has an explicit guarded symmetric declaration (`prp-symp`),
                    // include 1-hop symmetric reverses of explicit edges.
                    let sym_mode = *checked_sym.get_or_insert_with(|| {
                        let mut any_explicit_sym = false;
                        let mut any_unfiltered_sym = false;
                        for &(sgp, sgo, not_lit_y) in &self.guarded_sym {
                            if Self::is_explicit(g, &[q, sgp, sgo]) {
                                any_explicit_sym = true;
                                if !not_lit_y {
                                    any_unfiltered_sym = true;
                                }
                            }
                        }
                        if !any_explicit_sym {
                            None
                        } else if any_unfiltered_sym
                            || !Self::pred_has_explicit_literal_obj(g, q, terms)
                        {
                            Some(false) // full undirected
                        } else {
                            Some(true) // directed + non-literal reverse
                        }
                    });
                    match sym_mode {
                        Some(false) => {
                            if op.irreflexive {
                                undir_irref = true;
                            } else {
                                undir_unfilt = true;
                            }
                        }
                        Some(true) => {
                            if op.irreflexive {
                                dir_rev_irref = true;
                            } else {
                                dir_rev_unfilt = true;
                            }
                        }
                        None => {
                            if op.irreflexive {
                                dir_irref = true;
                            } else {
                                dir_unfilt = true;
                            }
                        }
                    }
                }
            }
        }
        // Prune strictly subsumed shapes so overlapping operators build the minimal set.
        if undir_unfilt {
            undir_irref = false;
            dir_rev_unfilt = false;
            dir_rev_irref = false;
            dir_unfilt = false;
            dir_irref = false;
        }
        if undir_irref {
            dir_rev_irref = false;
            dir_irref = false;
        }
        if dir_rev_unfilt {
            dir_rev_irref = false;
            dir_unfilt = false;
            dir_irref = false;
        }
        if dir_rev_irref {
            dir_irref = false;
        }
        if dir_unfilt {
            dir_irref = false;
        }

        let mut reaches: Vec<BaseReach> = Vec::new();
        if undir_unfilt {
            reaches.push(BaseReach::build_undirected(
                q, g, false, budget, cancel, visit,
            )?);
        }
        if undir_irref {
            reaches.push(BaseReach::build_undirected(
                q, g, true, budget, cancel, visit,
            )?);
        }
        if dir_rev_unfilt {
            reaches.push(BaseReach::build_directed(
                q, g, false, true, terms, budget, cancel, visit,
            )?);
        }
        if dir_rev_irref {
            reaches.push(BaseReach::build_directed(
                q, g, true, true, terms, budget, cancel, visit,
            )?);
        }
        if dir_unfilt {
            reaches.push(BaseReach::build_directed(
                q, g, false, false, terms, budget, cancel, visit,
            )?);
        }
        if dir_irref {
            reaches.push(BaseReach::build_directed(
                q, g, true, false, terms, budget, cancel, visit,
            )?);
        }
        self.base_graphs.insert(q, reaches);
        self.charge_tables()?;
        Ok(())
    }

    fn pred_has_explicit_literal_obj(g: &Graph, q: u64, terms: &Terms) -> bool {
        if let Some(base) = g.base() {
            let mut found = false;
            let _ = base.for_each(None, Some(q), None, |t| {
                if terms.kind(t[2]) == Kind::Literal {
                    found = true;
                    return false;
                }
                true
            });
            if found {
                return true;
            }
        }
        let c = g.cands(None, Some(q), None, g.offset(), g.len());
        c.iter().any(|r| {
            g.alive(r) && g.flagged(r) && {
                let t = g.triple(r);
                t[1] == q && terms.kind(t[2]) == Kind::Literal
            }
        })
    }

    /// Read-only O(1)/bounded check whether `t` is proved by the confirmed explicit edges
    /// of its operator relation (`Sync` across parallel `prove_all` chunks).
    #[inline]
    pub(crate) fn proved_by_base(
        &self,
        t: &Triple,
        scratch: &mut ReachScratch,
        cancel: Option<&AtomicBool>,
        visit: &mut dyn FnMut(u64) -> anyhow::Result<()>,
    ) -> anyhow::Result<bool> {
        let Some(reaches) = self.base_graphs.get(&t[1]) else {
            return Ok(false);
        };
        for r in reaches {
            if r.proves(t[0], t[2], scratch, cancel, visit)? {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// Enumerate operator consequences of `dead` while `dead` rows are still live in `g`.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn consequences(
        &mut self,
        dead: &[Triple],
        g: &Graph,
        terms: &Terms,
        budget: &Arc<Budget>,
        cancel: Option<&AtomicBool>,
        visit: &mut dyn FnMut(u64) -> anyhow::Result<()>,
        out: &mut dyn FnMut(Triple) -> anyhow::Result<()>,
    ) -> anyhow::Result<()> {
        let mut poll = Poll::new(cancel);
        let mut scratch = ReachScratch::new(budget);
        let mut pending: u64 = 0;
        // 1. Dead guards invalidate the operator for those predicates: emit all live
        //    non-explicit derived facts of each such predicate.
        let mut dead_guards_held = Held::new(budget);
        let mut dead_guards: Vec<u64> = Vec::new();
        for op in &self.ops {
            if let Over::Guarded(gp, go) = op.over {
                for d in dead {
                    if d[1] == gp && d[2] == go {
                        let q = d[0];
                        self.dead_guards.insert((q, gp, go));
                        self.base_graphs.remove(&q);
                        push_moved(&mut dead_guards, q, &mut dead_guards_held)?;
                    }
                }
            }
        }
        dead_guards.sort_unstable();
        dead_guards.dedup();
        for q in dead_guards {
            self.dirty.insert(q);
            self.scanned_all.insert(q);
            self.charge_tables()?;
            let c = g.cands(None, Some(q), None, g.offset(), g.len());
            for r in c.iter() {
                poll.tick(1)?;
                pending += 1;
                if pending >= REACH_VISIT_BATCH {
                    visit(std::mem::take(&mut pending))?;
                }
                if g.alive(r) && !g.flagged(r) {
                    let t = g.triple(r);
                    if t[1] == q {
                        out(t)?;
                    }
                }
            }
        }
        drop(dead_guards_held);
        // 2. Group dead edges by governed predicate `q`, charging before growth.
        let mut by_pred_held = Held::new(budget);
        let mut by_pred: FxHashMap<u64, (Covering, Vec<Triple>)> = FxHashMap::default();
        for &d in dead {
            if let Some(cov) = self.covering(d[1], g) {
                if !by_pred.contains_key(&d[1]) {
                    reserve_one(&mut by_pred, &mut by_pred_held)?;
                }
                let entry = by_pred.entry(d[1]).or_insert_with(|| (cov, Vec::new()));
                push_moved(&mut entry.1, d, &mut by_pred_held)?;
            }
        }
        // 3. Emit operator consequences for each affected predicate.
        let mut memo = Memo::default();
        for (q, (cov, edges)) in by_pred {
            self.dirty.insert(q);
            if self.scanned_all.contains(&q) {
                continue;
            }
            // Ensure `base_graphs[q]` exists for any explicit-guard operators so we skip
            // surviving pairs immediately.
            self.ensure_pred(q, g, terms, budget, cancel, visit)?;
            let total = self.dead_counts.entry(q).or_default();
            *total = total.saturating_add(edges.len());
            let total_dead = *total;
            self.charge_tables()?;
            if total_dead <= 32 && edges.len() <= 32 {
                // Small deletion: 1-step operator consequences via the graph indexes.
                let mut emit = |cand: Triple| -> anyhow::Result<()> {
                    pending += 1;
                    if pending >= REACH_VISIT_BATCH {
                        visit(std::mem::take(&mut pending))?;
                    }
                    if cov.all_irreflexive && cand[0] == cand[2] {
                        return Ok(());
                    }
                    if g.position(&cand).is_some_and(|r| !g.flagged(r))
                        && !self.proved_by_base(&cand, &mut scratch, cancel, visit)?
                    {
                        out(cand)?;
                    }
                    Ok(())
                };
                for d in &edges {
                    let (u, v) = (d[0], d[2]);
                    if cov.any_equiv {
                        emit([v, q, u])?;
                    }
                    g.each_subject(&mut memo, q, u, g.len(), |x| {
                        poll.tick(1)?;
                        emit([x, q, v])?;
                        Ok(true)
                    })?;
                    g.each_object(&mut memo, v, q, g.len(), |z| {
                        poll.tick(1)?;
                        emit([u, q, z])?;
                        Ok(true)
                    })?;
                }
            } else {
                // Larger deletion (e.g. a bridge split in a clique): one linear scan of
                // live non-explicit derived rows of `q`, skipping pairs proved by
                // `base_graphs[q]`, so total work stays O(|E_derived(q)|).
                self.scanned_all.insert(q);
                self.charge_tables()?;
                let c = g.cands(None, Some(q), None, g.offset(), g.len());
                for r in c.iter() {
                    poll.tick(1)?;
                    pending += 1;
                    if pending >= REACH_VISIT_BATCH {
                        visit(std::mem::take(&mut pending))?;
                    }
                    if g.alive(r) && !g.flagged(r) {
                        let t = g.triple(r);
                        if t[1] == q && !self.proved_by_base(&t, &mut scratch, cancel, visit)? {
                            out(t)?;
                        }
                    }
                }
            }
        }
        if pending > 0 {
            visit(pending)?;
        }
        if let Some(base) = g.base() {
            base.add_scans(memo.scans);
        }
        Ok(())
    }

    /// Operator predicates that lost facts during deletion and need their saturation
    /// marks reset to row 0.
    pub(crate) fn dirty_preds(&self) -> Vec<u64> {
        self.dirty.iter().copied().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A growth of the sink's buffer charges the new buffer beside the old one, which a
    /// moving growth copies from: the growth from 1,024 to 2,048 pairs holds 3,072 pairs
    /// of 24 bytes, 73,728, so a budget one byte short refuses it, where a charge of the
    /// difference alone admitted 49,152.
    #[test]
    fn buffer_growth_charges_both_buffers() {
        let budget = Budget::new(Some(3072 * size_of::<Triple>() - 1));
        let mut g = Graph::default();
        let mut sink = Sink::new(&mut g, &budget, 0, usize::MAX);
        for i in 0..1024u64 {
            sink.emit([i, 1, 2]).expect("the first buffer fits");
        }
        let e = sink
            .emit([1024, 1, 2])
            .expect_err("the growth does not fit");
        assert!(e.is::<crate::budget::MemoryExceeded>(), "{e:#}");
        let budget = Budget::new(Some(3072 * size_of::<Triple>()));
        let mut g = Graph::default();
        let mut sink = Sink::new(&mut g, &budget, 0, usize::MAX);
        for i in 0..1025u64 {
            sink.emit([i, 1, 2]).expect("both buffers fit");
        }
        assert_eq!(sink.held.bytes(), 2048 * size_of::<Triple>());
    }

    /// The ledger charges a growth of its tables and range lists before it reserves them:
    /// a budget too small for the first table refuses the record and leaves nothing
    /// allocated, and extending the last range of a predicate charges nothing.
    #[test]
    fn ledger_charges_before_it_grows() {
        let first = table_bytes(16, size_of::<(u64, Vec<(u32, u32)>)>());
        let budget = Budget::new(Some(first - 1));
        let mut l = Ledger::default();
        let e = l
            .merge(7, 0, 4, &budget)
            .expect_err("the table does not fit");
        assert!(e.is::<crate::budget::MemoryExceeded>(), "{e:#}");
        assert_eq!(l.merged.capacity(), 0);
        let budget = Budget::new(Some(table_bytes(16, size_of::<(u64, (u32, bool))>()) - 1));
        let mut l = Ledger::default();
        let e = l
            .close(7, 4, false, &budget)
            .expect_err("the table does not fit");
        assert!(anyhow::Error::from(e).is::<crate::budget::MemoryExceeded>());
        assert_eq!(l.closed.capacity(), 0);
        let budget = Budget::new(None);
        let mut l = Ledger::default();
        l.merge(7, 0, 4, &budget).unwrap();
        let held = l.held.as_ref().unwrap().bytes();
        assert_eq!(held, first + 4 * size_of::<(u32, u32)>());
        l.merge(7, 4, 9, &budget).unwrap();
        assert_eq!(l.merged[&7], vec![(0, 9)]);
        assert_eq!(l.held.as_ref().unwrap().bytes(), held);
    }

    /// A growth of a full ledger table charges the old and the new table together, and
    /// gives the old one's charge back after: a budget one byte short of both refuses it
    /// and leaves the table as it was.
    #[test]
    fn ledger_growth_charges_both_tables() {
        let bytes = |cap: usize| table_bytes(cap, size_of::<(u64, (u32, bool))>());
        let fill = |l: &mut Ledger, budget: &Arc<Budget>| {
            let mut q = 0;
            while l.closed.is_empty() || l.closed.len() < l.closed.capacity() {
                l.close(q, 1, false, budget).unwrap();
                q += 1;
            }
            q
        };
        let mut l = Ledger::default();
        let q = fill(&mut l, &Budget::new(None));
        let cap = l.closed.capacity();
        assert_eq!(l.held.as_ref().unwrap().bytes(), bytes(cap));
        let both = bytes(cap) + bytes(2 * cap);
        let budget = Budget::new(Some(both - 1));
        let mut l = Ledger::default();
        assert_eq!(fill(&mut l, &budget), q);
        let e = l
            .close(q, 1, false, &budget)
            .expect_err("both tables do not fit");
        assert!(anyhow::Error::from(e).is::<crate::budget::MemoryExceeded>());
        assert_eq!(l.closed.capacity(), cap);
        let budget = Budget::new(Some(both));
        let mut l = Ledger::default();
        fill(&mut l, &budget);
        l.close(q, 1, false, &budget).expect("both tables fit");
        assert_eq!(l.closed.capacity(), 2 * cap);
        assert_eq!(l.held.as_ref().unwrap().bytes(), bytes(2 * cap));
    }

    /// A growth of a full range list charges the old and the new list together, as a
    /// table's growth does: four ranges apart fill the first list, and the fifth needs
    /// room for twelve.
    #[test]
    fn ledger_range_growth_charges_both_lists() {
        let four = |l: &mut Ledger, budget: &Arc<Budget>| {
            for r in 0..4u32 {
                l.merge(7, 2 * r, 2 * r + 1, budget).unwrap();
            }
        };
        let mut l = Ledger::default();
        four(&mut l, &Budget::new(None));
        assert_eq!(l.merged[&7].capacity(), 4);
        let h = l.held.as_ref().unwrap().bytes();
        let pair = size_of::<(u32, u32)>();
        let budget = Budget::new(Some(h + 8 * pair - 1));
        let mut l = Ledger::default();
        four(&mut l, &budget);
        let e = l
            .merge(7, 8, 9, &budget)
            .expect_err("both lists do not fit");
        assert!(e.is::<crate::budget::MemoryExceeded>(), "{e:#}");
        assert_eq!(l.merged[&7].len(), 4);
        assert_eq!(l.merged[&7].capacity(), 4);
        let budget = Budget::new(Some(h + 8 * pair));
        let mut l = Ledger::default();
        four(&mut l, &budget);
        l.merge(7, 8, 9, &budget).expect("both lists fit");
        assert_eq!(l.merged[&7].capacity(), 8);
        assert_eq!(l.held.as_ref().unwrap().bytes(), h + 4 * pair);
    }
}
