//! Incremental maintenance of a materialization with the backward/forward algorithm
//! (Motik, Nenov, Piro, Horrocks: *Incremental Update of Datalog Materialisation: the
//! Backward/Forward Algorithm*, AAAI 2015).
//!
//! The closure of the previous run is kept as an in-memory [`Closure`]: every derived
//! fact, the generalized ones included, with a flag on the explicit facts (the default
//! graph). Given the explicit facts deleted and inserted since, an update
//!
//! 1. **deletes** what no longer follows. A deleted explicit fact is a candidate. Before a
//!    candidate is deleted, a backward search looks for another proof of it among the
//!    facts not deleted, and keeps it if one exists. Only facts without a proof are
//!    deleted, and the heads of the rule instances that used them become candidates.
//!    DRed (Gupta, Mumick, Subrahmanian 1993) would delete every consequence first and
//!    derive the survivors again; with RDFS that deletes most of the closure for a single
//!    `rdf:type` triple, through `rdfs:Resource` and the class axioms;
//! 2. **inserts** the new explicit facts and runs the rules semi-naively from them, with
//!    the closure that remains as the old facts.
//!
//! The backward search decides provability exactly. It explores the rule instances that
//! derive a fact, depth first, and counts for each instance the body facts not proved
//! yet. Explicit facts and facts proved earlier in the run are proved. When the search
//! finds no proof and has explored every instance it reached, none of the facts it
//! reached has a proof, and they are all remembered as unprovable.

use crate::engine::{self, CHead, CRule, Eval, Limits, Mode, Slot, Step};
use crate::graph::{Cands, Graph, Triple};
use crate::terms::Terms;
use rayon::prelude::*;
use rustc_hash::{FxHashMap, FxHashSet};
use std::sync::atomic::{AtomicBool, Ordering};

/// A run that cannot be maintained incrementally and must materialize in full.
#[derive(Debug, Clone, thiserror::Error)]
#[error("{0}")]
pub(crate) struct Fallback(pub String);

/// A materialization in memory: the closure, with the explicit facts flagged.
pub(crate) struct Closure {
    pub graph: Graph,
    pub terms: Terms,
    /// one bit per row: the row holds an explicit fact
    base: Vec<u64>,
}

impl Closure {
    /// A closure whose rows before `base_len` are the explicit facts.
    pub fn new(graph: Graph, terms: Terms, base_len: u32) -> Closure {
        let mut c = Closure {
            graph,
            terms,
            base: Vec::new(),
        };
        for r in 0..base_len {
            c.set_base(r, true);
        }
        c
    }

    #[inline]
    pub fn is_base(&self, row: u32) -> bool {
        self.base
            .get(row as usize / 64)
            .is_some_and(|w| w & (1 << (row % 64)) != 0)
    }

    pub fn set_base(&mut self, row: u32, on: bool) {
        let w = row as usize / 64;
        if self.base.len() <= w {
            self.base.resize(w + 1, 0);
        }
        if on {
            self.base[w] |= 1 << (row % 64);
        } else {
            self.base[w] &= !(1 << (row % 64));
        }
    }

    /// Whether a live triple is explicit.
    pub fn explicit(&self, t: &Triple) -> bool {
        self.graph.position(t).is_some_and(|r| self.is_base(r))
    }

    /// The live triples, explicit ones first, each group in row order.
    pub fn live(&self) -> impl Iterator<Item = (Triple, bool)> + '_ {
        let rows = (0..self.graph.len()).filter(|&r| self.graph.alive(r));
        rows.map(|r| (self.graph.triples[r as usize], self.is_base(r)))
    }

    /// Replace local ids by store ids (`moved`) in every live row.
    pub fn remap(&mut self, moved: &FxHashMap<u64, u64>) {
        if moved.is_empty() {
            return;
        }
        let m = |x: u64| moved.get(&x).copied().unwrap_or(x);
        let hit: Vec<(Triple, bool)> = self
            .live()
            .filter(|(t, _)| t.iter().any(|x| moved.contains_key(x)))
            .collect();
        for (t, _) in &hit {
            self.graph.kill(t);
        }
        for (t, base) in hit {
            let n = [m(t[0]), m(t[1]), m(t[2])];
            match self.graph.position(&n) {
                Some(r) => {
                    if base {
                        self.set_base(r, true);
                    }
                }
                None => {
                    self.graph.add_batch([n]);
                    let r = self.graph.len() - 1;
                    self.set_base(r, base);
                }
            }
        }
    }

    /// Rebuild the graph without its dead rows when they are a quarter of it or more.
    pub fn compact_if_needed(&mut self) {
        let g = &self.graph;
        if (g.dead_rows() as usize) * 4 < g.len() as usize {
            return;
        }
        let live: Vec<(Triple, bool)> = self.live().collect();
        let mut graph = Graph::with_capacity(live.len());
        graph.add_batch(live.iter().map(|x| x.0));
        let mut base = vec![0u64; live.len().div_ceil(64)];
        for (r, (_, b)) in live.iter().enumerate() {
            if *b {
                base[r / 64] |= 1 << (r % 64);
            }
        }
        self.graph = graph;
        self.base = base;
    }
}

/// What an update changed.
#[derive(Debug, Default)]
pub(crate) struct Update {
    /// facts removed from the closure (an insertion may have derived some of them again)
    pub deleted: Vec<Triple>,
    /// rows from this one on were added
    pub added_from: u32,
    /// facts whose provability was searched for
    pub checked: u64,
    /// semi-naive iterations of the insertion
    pub iterations: usize,
}

impl Update {
    /// The facts added to the closure.
    pub fn added<'a>(&self, c: &'a Closure) -> impl Iterator<Item = Triple> + 'a {
        let g = &c.graph;
        (self.added_from..g.len())
            .filter(|&r| g.alive(r))
            .map(|r| g.triples[r as usize])
    }
}

fn cancelled(limits: &Limits) -> bool {
    limits
        .cancel
        .as_ref()
        .is_some_and(|c| c.load(Ordering::Relaxed))
}

/// Bring `c` from the closure of the old explicit facts to that of the new ones:
/// `removed` explicit facts are no longer explicit, `inserted` ones are.
///
/// Returns [`Fallback`] (as the error) when the rules read RDF lists and a list fact is
/// derived, removed or added: what list builtins see then depends on the order of
/// derivation. The closure is unusable after any error.
pub(crate) fn update(
    c: &mut Closure,
    rules: &[CRule],
    limits: &Limits,
    inserted: &[Triple],
    removed: &[Triple],
) -> anyhow::Result<Update> {
    let lists = rules.iter().any(engine::reads_lists);
    let (first, rest) = (c.terms.rdf_first, c.terms.rdf_rest);
    let is_list = |t: &Triple| t[1] == first || t[1] == rest;
    if lists {
        if inserted.iter().chain(removed).any(is_list) {
            return Err(Fallback("an RDF list changed and the rules read lists".into()).into());
        }
        let g = &c.graph;
        for p in [first, rest] {
            let rows = g.cands(None, Some(p), None, 0, g.len());
            if (0..rows.len())
                .map(|i| rows.get(i))
                .any(|r| g.alive(r) && !c.is_base(r))
            {
                return Err(Fallback("the rules read RDF lists that are derived".into()).into());
            }
        }
    }
    let mut out = Update::default();

    // ---- deletion
    let mut queue: Vec<Triple> = Vec::new();
    for t in removed {
        if let Some(r) = c.graph.position(t)
            && c.is_base(r)
        {
            c.set_base(r, false);
            queue.push(*t);
        }
    }
    if !queue.is_empty() {
        let plans = Plans::new(&mut c.graph, rules);
        let mut search = Search::default();
        let abort = AtomicBool::new(false);
        while !queue.is_empty() {
            if cancelled(limits) {
                anyhow::bail!("reasoning cancelled");
            }
            let mut dead: Vec<Triple> = Vec::new();
            let mut seen: FxHashSet<Triple> = FxHashSet::default();
            {
                let cx = Cx {
                    c: &*c,
                    rules,
                    plans: &plans,
                    abort: &abort,
                    cancel: limits.cancel.as_deref(),
                };
                for t in queue.drain(..) {
                    if seen.insert(t) && !search.prove(&cx, t) {
                        dead.push(t);
                    }
                }
            }
            if dead.is_empty() {
                break;
            }
            queue = consequences(c, rules, &plans, &dead, limits);
            for t in &dead {
                c.graph.kill(t);
            }
            queue.retain(|t| c.graph.position(t).is_some() && !search.proved.contains(t));
            out.deleted.extend(dead);
        }
        out.checked = search.checked;
        if cancelled(limits) {
            anyhow::bail!("reasoning cancelled");
        }
        if lists && out.deleted.iter().any(is_list) {
            return Err(Fallback("a derived RDF list fact was removed".into()).into());
        }
    }

    // ---- insertion
    let start = c.graph.len();
    out.added_from = start;
    let mut fresh: Vec<Triple> = Vec::new();
    for t in inserted {
        match c.graph.position(t) {
            Some(r) => c.set_base(r, true),
            None => fresh.push(*t),
        }
    }
    c.graph.add_batch(fresh);
    for r in start..c.graph.len() {
        c.set_base(r, true);
    }
    if c.graph.len() > start {
        let o = engine::run(&mut c.graph, rules, &c.terms, limits, Some(start))?;
        out.iterations = o.iterations;
        if lists && out.added(c).any(|t| is_list(&t) && !c.explicit(&t)) {
            return Err(Fallback("an RDF list fact was derived".into()).into());
        }
    }
    Ok(out)
}

/// How many facts DRed would delete before deriving the survivors again: every fact with
/// a derivation that uses a removed explicit fact, transitively. For measurements; the
/// closure is unusable afterwards.
pub(crate) fn dred_overdeletion(
    c: &mut Closure,
    rules: &[CRule],
    limits: &Limits,
    removed: &[Triple],
) -> usize {
    let plans = Plans::new(&mut c.graph, rules);
    let mut gone: FxHashSet<Triple> = FxHashSet::default();
    let mut frontier: Vec<Triple> = removed
        .iter()
        .filter(|t| c.graph.position(t).is_some() && gone.insert(**t))
        .copied()
        .collect();
    while !frontier.is_empty() {
        let next = consequences(c, rules, &plans, &frontier, limits);
        for t in &frontier {
            c.graph.kill(t);
        }
        frontier = next
            .into_iter()
            .filter(|t| c.graph.position(t).is_some() && gone.insert(*t))
            .collect();
    }
    gone.len()
}

// ------------------------------------------------------------------ plans ----

/// The plans an update evaluates with, made once per update.
struct Plans {
    /// per rule and head: the plan with the head's variables bound (those that some atom
    /// binds), and those variables
    back: Vec<Vec<Option<(Vec<Step>, Vec<usize>)>>>,
    /// heads by constant predicate: (rule, head)
    by_pred: FxHashMap<u64, Vec<(usize, usize)>>,
    /// heads with a variable predicate
    any_pred: Vec<(usize, usize)>,
    /// per rule and atom: the plan that starts with that atom
    forward: Vec<Vec<Option<Vec<Step>>>>,
}

impl Plans {
    fn new(g: &mut Graph, rules: &[CRule]) -> Plans {
        let mut p = Plans {
            back: Vec::with_capacity(rules.len()),
            by_pred: FxHashMap::default(),
            any_pred: Vec::new(),
            forward: Vec::with_capacity(rules.len()),
        };
        let all = |r: &CRule| vec![(0, g.len()); r.atoms.len()];
        for (ri, r) in rules.iter().enumerate() {
            let ranges = all(r);
            let mut in_atoms = vec![false; r.nvars];
            for a in &r.atoms {
                for s in [a.s, a.p, a.o] {
                    if let Slot::Var(v) = s {
                        in_atoms[v] = true;
                    }
                }
            }
            let mut heads = Vec::new();
            for (hi, h) in r.head.iter().enumerate() {
                let CHead::Triple(s, pr, o) = h else {
                    heads.push(None);
                    continue;
                };
                let mut pre: Vec<usize> = [s, pr, o]
                    .into_iter()
                    .filter_map(|x| match x {
                        Slot::Var(v) if in_atoms[*v] => Some(*v),
                        _ => None,
                    })
                    .collect();
                pre.sort_unstable();
                pre.dedup();
                let plan = engine::plan_with(r, &ranges, Some(g), None, &pre);
                if plan.is_some() {
                    match pr {
                        Slot::Const(c) => p.by_pred.entry(*c).or_default().push((ri, hi)),
                        _ => p.any_pred.push((ri, hi)),
                    }
                }
                heads.push(plan.map(|s| (s, pre)));
            }
            p.back.push(heads);
            p.forward.push(
                (0..r.atoms.len())
                    .map(|d| engine::plan_with(r, &ranges, Some(g), Some(d), &[]))
                    .collect(),
            );
        }
        // the indexes the plans use
        let (mut need_s, mut need_o) = (false, false);
        for (ri, r) in rules.iter().enumerate() {
            let plans = p.back[ri]
                .iter()
                .flatten()
                .map(|(s, pre)| (s, pre.as_slice()))
                .chain(p.forward[ri].iter().flatten().map(|s| (s, &[][..])));
            for (steps, pre) in plans {
                let (s, o) = engine::index_needs_with(r, steps, pre);
                need_s |= s;
                need_o |= o;
            }
        }
        if need_s {
            g.ensure_s_index();
        }
        if need_o {
            g.ensure_o_index();
        }
        p
    }

    fn heads(&self, p: u64) -> impl Iterator<Item = (usize, usize)> + '_ {
        self.by_pred
            .get(&p)
            .into_iter()
            .flatten()
            .chain(&self.any_pred)
            .copied()
    }
}

/// The heads (in the closure) of the rule instances with a body atom in `dead`, whose
/// rows are still live.
fn consequences(
    c: &Closure,
    rules: &[CRule],
    plans: &Plans,
    dead: &[Triple],
    limits: &Limits,
) -> Vec<Triple> {
    let g = &c.graph;
    let rows: Vec<u32> = dead.iter().filter_map(|t| g.position(t)).collect();
    let mut work: Vec<(usize, usize, Vec<u32>)> = Vec::new();
    for (ri, r) in rules.iter().enumerate() {
        for (d, a) in r.atoms.iter().enumerate() {
            if plans.forward[ri][d].is_none() {
                continue;
            }
            let fits = |x: Slot, v: u64| !matches!(x, Slot::Const(k) if k != v);
            let mine: Vec<u32> = rows
                .iter()
                .copied()
                .filter(|&row| {
                    let t = g.triples[row as usize];
                    fits(a.s, t[0]) && fits(a.p, t[1]) && fits(a.o, t[2])
                })
                .collect();
            for ch in mine.chunks(256) {
                work.push((ri, d, ch.to_vec()));
            }
        }
    }
    let abort = AtomicBool::new(false);
    let heads: Vec<Vec<Triple>> = work
        .par_iter()
        .map(|(ri, d, rows)| {
            let r = &rules[*ri];
            let steps = plans.forward[*ri][*d].as_ref().unwrap();
            let mut ev = Eval::new(
                g,
                &c.terms,
                r,
                steps,
                g.len(),
                &abort,
                limits.cancel.as_deref(),
                usize::MAX,
                Mode::Existing,
            );
            let mut b = vec![0u64; r.nvars];
            ev.run(0, &mut b, Some(Cands::Slice(rows)));
            ev.out
        })
        .collect();
    let mut seen: FxHashSet<Triple> = FxHashSet::default();
    heads
        .into_iter()
        .flatten()
        .filter(|t| seen.insert(*t))
        .collect()
}

// ------------------------------------------------------------------ proofs ----

/// How deep the search recurses before it queues the facts it reaches instead.
const MAX_DEPTH: u32 = 32;

#[derive(Clone, Copy, PartialEq, Eq)]
enum State {
    /// reached, rule instances not explored yet
    Fresh,
    /// its instances are being explored
    Open,
    /// every instance explored, no proof found (yet)
    Explored,
    Proved,
}

struct Node {
    t: Triple,
    state: State,
    /// the instances with this fact in the body
    uses: Vec<u32>,
}

struct Inst {
    head: u32,
    /// body facts not proved yet
    open: u32,
}

/// What a search reads.
struct Cx<'a> {
    c: &'a Closure,
    rules: &'a [CRule],
    plans: &'a Plans,
    abort: &'a AtomicBool,
    cancel: Option<&'a AtomicBool>,
}

/// Provability of facts among the live ones: what is known across the searches of one
/// update, and the state of the current search.
#[derive(Default)]
struct Search {
    proved: FxHashSet<Triple>,
    unprovable: FxHashSet<Triple>,
    checked: u64,
    nodes: Vec<Node>,
    node_of: FxHashMap<Triple, u32>,
    insts: Vec<Inst>,
    queue: Vec<u32>,
}

impl Search {
    /// Whether a live fact follows from the explicit facts.
    fn prove(&mut self, cx: &Cx<'_>, t: Triple) -> bool {
        if self.proved.contains(&t) {
            return true;
        }
        if self.unprovable.contains(&t) {
            return false;
        }
        let Some(row) = cx.c.graph.position(&t) else {
            return false;
        };
        if cx.c.is_base(row) {
            self.proved.insert(t);
            return true;
        }
        self.checked += 1;
        self.nodes.clear();
        self.node_of.clear();
        self.insts.clear();
        self.queue.clear();
        let root = self.node(t);
        self.expand(cx, root, root, 0);
        while self.nodes[root as usize].state != State::Proved
            && let Some(n) = self.queue.pop()
        {
            if self.nodes[n as usize].state == State::Fresh {
                self.expand(cx, n, root, 0);
            }
        }
        let proved = self.nodes[root as usize].state == State::Proved;
        for n in &self.nodes {
            match n.state {
                State::Proved => {
                    self.proved.insert(n.t);
                }
                // with no proof of the root, the search explored everything it reached
                _ if !proved => {
                    self.unprovable.insert(n.t);
                }
                _ => {}
            }
        }
        proved
    }

    fn node(&mut self, t: Triple) -> u32 {
        if let Some(&n) = self.node_of.get(&t) {
            return n;
        }
        let n = self.nodes.len() as u32;
        self.nodes.push(Node {
            t,
            state: State::Fresh,
            uses: Vec::new(),
        });
        self.node_of.insert(t, n);
        self.queue.push(n);
        n
    }

    fn mark_proved(&mut self, n: u32) {
        let mut stack = vec![n];
        while let Some(x) = stack.pop() {
            let node = &mut self.nodes[x as usize];
            if node.state == State::Proved {
                continue;
            }
            node.state = State::Proved;
            for i in std::mem::take(&mut node.uses) {
                let inst = &mut self.insts[i as usize];
                inst.open -= 1;
                if inst.open == 0 {
                    stack.push(inst.head);
                }
            }
        }
    }

    fn done(&self, n: u32, root: u32) -> bool {
        self.nodes[n as usize].state == State::Proved
            || self.nodes[root as usize].state == State::Proved
    }

    /// Explore the rule instances that derive node `n`, depth first.
    fn expand(&mut self, cx: &Cx<'_>, n: u32, root: u32, depth: u32) {
        self.nodes[n as usize].state = State::Open;
        let t = self.nodes[n as usize].t;
        let g = &cx.c.graph;
        for (ri, hi) in cx.plans.heads(t[1]) {
            let Some((steps, pre)) = &cx.plans.back[ri][hi] else {
                continue;
            };
            let rule = &cx.rules[ri];
            let Some(mut b) = bind_head(rule, hi, pre, t) else {
                continue;
            };
            {
                let mut sink = |rows: &[u32], implicit: &[Triple]| -> bool {
                    self.instance(cx, n, root, depth, rows, implicit)
                };
                let mut ev = Eval::new(
                    g,
                    &cx.c.terms,
                    rule,
                    steps,
                    g.len(),
                    cx.abort,
                    cx.cancel,
                    usize::MAX,
                    Mode::Instances {
                        head: hi,
                        target: t,
                        sink: &mut sink,
                    },
                );
                ev.run(0, &mut b, None);
            }
            if self.done(n, root) || cx.cancel.is_some_and(|c| c.load(Ordering::Relaxed)) {
                break;
            }
        }
        let node = &mut self.nodes[n as usize];
        if node.state == State::Open {
            node.state = State::Explored;
        }
    }

    /// One instance deriving node `n`; returns false to stop exploring `n`.
    fn instance(
        &mut self,
        cx: &Cx<'_>,
        n: u32,
        root: u32,
        depth: u32,
        rows: &[u32],
        implicit: &[Triple],
    ) -> bool {
        let g = &cx.c.graph;
        let t = self.nodes[n as usize].t;
        let body = rows
            .iter()
            .map(|&r| g.triples[r as usize])
            .chain(implicit.iter().copied());
        let mut facts: Vec<Triple> = Vec::with_capacity(rows.len() + implicit.len());
        for u in body {
            // an instance that uses its own head or an unprovable fact proves nothing
            if u == t || self.unprovable.contains(&u) {
                return true;
            }
            if self.proved.contains(&u) || cx.c.explicit(&u) {
                continue;
            }
            facts.push(u);
        }
        let inst = self.insts.len() as u32;
        let mut open = 0;
        let mut fresh: Vec<u32> = Vec::new();
        for u in facts {
            let known = self.node_of.contains_key(&u);
            let m = self.node(u);
            if self.nodes[m as usize].state == State::Proved {
                continue;
            }
            if !known {
                fresh.push(m);
            }
            open += 1;
            self.nodes[m as usize].uses.push(inst);
        }
        self.insts.push(Inst { head: n, open });
        if open == 0 {
            self.mark_proved(n);
            return false;
        }
        if depth < MAX_DEPTH {
            for m in fresh {
                if self.nodes[m as usize].state == State::Fresh {
                    self.expand(cx, m, root, depth + 1);
                }
                if self.done(n, root) {
                    return false;
                }
            }
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sparkles::io::{RdfFormat, Source};
    use sparkles::store::{Snapshot, Store, StoreOptions};
    use std::sync::Arc;

    const RDF: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#";
    const RDFS: &str = "http://www.w3.org/2000/01/rdf-schema#";
    const OWL: &str = "http://www.w3.org/2002/07/owl#";
    const EX: &str = "http://ex.org/";

    /// xorshift64*
    struct Rng(u64);
    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 >> 12;
            self.0 ^= self.0 << 25;
            self.0 ^= self.0 >> 27;
            self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
        }
        fn below(&mut self, n: usize) -> usize {
            (self.next() % n as u64) as usize
        }
        fn pick<T: Copy>(&mut self, v: &[T]) -> T {
            v[self.below(v.len())]
        }
    }

    struct Vocab {
        snap: Arc<Snapshot>,
        classes: Vec<u64>,
        props: Vec<u64>,
        inds: Vec<u64>,
        lits: Vec<u64>,
        list_nodes: Vec<u64>,
        iri: FxHashMap<String, u64>,
    }

    impl Vocab {
        fn new() -> Vocab {
            let mut ttl = String::new();
            let mut names = Vec::new();
            for i in 0..6 {
                names.push(format!("{EX}C{i}"));
            }
            for i in 0..5 {
                names.push(format!("{EX}p{i}"));
            }
            for i in 0..8 {
                names.push(format!("{EX}i{i}"));
            }
            for i in 0..3 {
                names.push(format!("{EX}l{i}"));
            }
            for l in ["type", "first", "rest", "nil", "Property", "List"] {
                names.push(format!("{RDF}{l}"));
            }
            for l in [
                "subClassOf",
                "subPropertyOf",
                "domain",
                "range",
                "Class",
                "Resource",
                "Literal",
            ] {
                names.push(format!("{RDFS}{l}"));
            }
            for l in [
                "sameAs",
                "inverseOf",
                "TransitiveProperty",
                "SymmetricProperty",
                "FunctionalProperty",
                "InverseFunctionalProperty",
                "equivalentClass",
                "equivalentProperty",
                "intersectionOf",
                "unionOf",
                "hasValue",
                "onProperty",
                "someValuesFrom",
                "allValuesFrom",
                "Thing",
                "Class",
                "Restriction",
            ] {
                names.push(format!("{OWL}{l}"));
            }
            for n in &names {
                ttl.push_str(&format!("<urn:v> <urn:has> <{n}> .\n"));
            }
            ttl.push_str("<urn:v> <urn:has> \"a\", \"b\", 7 .\n");
            let store = Store::in_memory(StoreOptions::default());
            store
                .load(&[Source::from_bytes(
                    ttl.into_bytes(),
                    RdfFormat::Turtle,
                    None,
                )])
                .unwrap();
            let snap = store.snapshot();
            let iri: FxHashMap<String, u64> = names
                .iter()
                .map(|n| (n.clone(), snap.lookup_iri(n).unwrap().0))
                .collect();
            let get = |p: &str| -> Vec<u64> {
                let mut v: Vec<(String, u64)> = iri
                    .iter()
                    .filter(|(k, _)| k.starts_with(&format!("{EX}{p}")))
                    .map(|(k, v)| (k.clone(), *v))
                    .collect();
                v.sort();
                v.into_iter().map(|x| x.1).collect()
            };
            let terms = Terms::new(snap.clone());
            let lits = ["\"a\"", "\"b\""]
                .iter()
                .map(|l| {
                    terms.id_for(&oxrdf::Term::Literal(oxrdf::Literal::new_simple_literal(
                        &l[1..2],
                    )))
                })
                .chain([sparkles::id::Id::from_i64(7).unwrap().0])
                .collect();
            Vocab {
                classes: get("C"),
                props: get("p"),
                inds: get("i"),
                list_nodes: get("l"),
                lits,
                snap,
                iri,
            }
        }

        fn id(&self, ns: &str, l: &str) -> u64 {
            self.iri[&format!("{ns}{l}")]
        }

        /// A random triple: schema statements, typing, property assertions.
        fn triple(&self, r: &mut Rng, owl: bool) -> Triple {
            let rdf = |l| self.id(RDF, l);
            let rdfs = |l| self.id(RDFS, l);
            let o = |l| self.id(OWL, l);
            let (c, p, i) = (&self.classes, &self.props, &self.inds);
            let k = r.below(if owl { 20 } else { 10 });
            match k {
                0 => [r.pick(c), rdfs("subClassOf"), r.pick(c)],
                1 => [r.pick(p), rdfs("subPropertyOf"), r.pick(p)],
                2 => [r.pick(p), rdfs("domain"), r.pick(c)],
                3 => [r.pick(p), rdfs("range"), r.pick(c)],
                4 | 5 => [r.pick(i), rdf("type"), r.pick(c)],
                6..=8 => [r.pick(i), r.pick(p), r.pick(i)],
                9 => [r.pick(i), r.pick(p), r.pick(&self.lits)],
                10 => [r.pick(i), o("sameAs"), r.pick(i)],
                11 => [r.pick(p), o("inverseOf"), r.pick(p)],
                12 => [
                    r.pick(p),
                    rdf("type"),
                    r.pick(&[
                        o("TransitiveProperty"),
                        o("SymmetricProperty"),
                        o("FunctionalProperty"),
                        o("InverseFunctionalProperty"),
                    ]),
                ],
                13 => [r.pick(c), o("equivalentClass"), r.pick(c)],
                14 => [r.pick(c), o("hasValue"), r.pick(i)],
                15 => [r.pick(c), o("onProperty"), r.pick(p)],
                16 => [r.pick(c), o("someValuesFrom"), r.pick(c)],
                17 => [r.pick(c), o("allValuesFrom"), r.pick(c)],
                18 => [r.pick(p), o("equivalentProperty"), r.pick(p)],
                _ => [r.pick(c), rdf("type"), o("Class")],
            }
        }

        /// The triples of a two-member list for `owl:intersectionOf` or `owl:unionOf`.
        fn list(&self, r: &mut Rng) -> Vec<Triple> {
            let (first, rest, nil) = (
                self.id(RDF, "first"),
                self.id(RDF, "rest"),
                self.id(RDF, "nil"),
            );
            let (l0, l1) = (r.pick(&self.list_nodes), r.pick(&self.list_nodes));
            if l0 == l1 {
                return Vec::new();
            }
            let op = if r.below(2) == 0 {
                "intersectionOf"
            } else {
                "unionOf"
            };
            vec![
                [r.pick(&self.classes), self.id(OWL, op), l0],
                [l0, first, r.pick(&self.classes)],
                [l0, rest, l1],
                [l1, first, r.pick(&self.classes)],
                [l1, rest, nil],
            ]
        }
    }

    fn limits() -> Limits {
        Limits {
            max_iterations: 10_000,
            max_inferred: 10_000_000,
            cancel: None,
            progress: None,
        }
    }

    fn compiled(text: &str, terms: &Terms) -> Vec<CRule> {
        let rules = crate::parse_rules(text).unwrap();
        let mut w = Vec::new();
        let c = engine::compile(&rules, terms, &mut w);
        assert!(
            c.iter()
                .all(|r| engine::incremental_blocker(r, terms).is_none())
        );
        c
    }

    fn full(v: &Vocab, text: &str, base: &FxHashSet<Triple>) -> Closure {
        let terms = Terms::new(v.snap.clone());
        let rules = compiled(text, &terms);
        let mut sorted: Vec<Triple> = base.iter().copied().collect();
        sorted.sort_unstable();
        let mut g = Graph::default();
        g.add_batch(sorted);
        let n = g.len();
        engine::run(&mut g, &rules, &terms, &limits(), None).unwrap();
        Closure::new(g, terms, n)
    }

    /// The live facts as text, explicit ones marked.
    fn render(c: &Closure) -> std::collections::BTreeSet<String> {
        let s = |x: u64| c.terms.term(x).map_or(format!("?{x:x}"), |t| t.to_string());
        c.live()
            .map(|(t, b)| {
                format!(
                    "{} {} {}{}",
                    s(t[0]),
                    s(t[1]),
                    s(t[2]),
                    if b { " ." } else { "" }
                )
            })
            .collect()
    }

    fn differential(text: &str, owl: bool, lists: bool, seed: u64, steps: usize) -> (usize, usize) {
        let v = Vocab::new();
        let mut r = Rng(seed);
        let mut base: FxHashSet<Triple> = FxHashSet::default();
        for _ in 0..25 {
            base.insert(v.triple(&mut r, owl));
        }
        let mut c = full(&v, text, &base);
        let (mut incremental, mut fallbacks) = (0, 0);
        for step in 0..steps {
            let mut removed = Vec::new();
            let mut inserted = Vec::new();
            let current: Vec<Triple> = {
                let mut x: Vec<Triple> = base.iter().copied().collect();
                x.sort_unstable();
                x
            };
            for _ in 0..r.below(4) {
                if !current.is_empty() {
                    let t = r.pick(&current);
                    if base.remove(&t) {
                        removed.push(t);
                    }
                }
            }
            for _ in 0..r.below(5) {
                let t = v.triple(&mut r, owl);
                if base.insert(t) {
                    inserted.push(t);
                }
            }
            if lists && r.below(6) == 0 {
                for t in v.list(&mut r) {
                    if base.insert(t) {
                        inserted.push(t);
                    }
                }
            }
            let rules = compiled(text, &c.terms);
            match update(&mut c, &rules, &limits(), &inserted, &removed) {
                Ok(_) => incremental += 1,
                Err(e) => {
                    assert!(e.downcast_ref::<Fallback>().is_some(), "{e:#}");
                    fallbacks += 1;
                    c = full(&v, text, &base);
                    continue;
                }
            }
            let want = full(&v, text, &base);
            let (a, b) = (render(&c), render(&want));
            if a != b {
                let extra: Vec<_> = a.difference(&b).collect();
                let missing: Vec<_> = b.difference(&a).collect();
                panic!(
                    "seed {seed} step {step}: removed {removed:?} inserted {inserted:?}\nextra {extra:#?}\nmissing {missing:#?}"
                );
            }
            c.compact_if_needed();
        }
        (incremental, fallbacks)
    }

    #[test]
    fn rdfs_matches_full() {
        for seed in 1..25 {
            differential(crate::RDFS_RULES, false, false, seed, 25);
        }
    }

    #[test]
    fn rdfs_simple_matches_full() {
        for seed in 1..25 {
            differential(crate::RDFS_SIMPLE_RULES, false, false, seed, 25);
        }
    }

    #[test]
    fn owl_rl_matches_full() {
        let mut n = (0, 0);
        for seed in 1..25 {
            let (i, f) = differential(crate::OWL_RL_RULES, true, true, seed, 25);
            n = (n.0 + i, n.1 + f);
        }
        assert!(n.0 > n.1, "{n:?}");
    }

    #[test]
    fn jena_rules_match_full() {
        let rules = "@prefix ex: <http://ex.org/>.
            [chain: (?a ex:p0 ?b), (?b ex:p0 ?c), notEqual(?a, ?c) -> (?a ex:p1 ?c)]
            [lit: (?a ex:p2 ?v), isLiteral(?v), strConcat(?v, '!', ?w) -> (?a ex:p3 ?w)]
            [back: (?a ex:p3 ?w), regex(?w, '(.*)!', ?x) -> (?a ex:p4 ?x)]
            [num: (?a ex:p4 ?v), lessThan(?v, 10) -> (?a rdf:type ex:C0)]
            [sym: (?a ex:p1 ?b) -> (?b ex:p1 ?a)]
            [ty: (?a rdf:type ex:C0), (?a ?p ?b), notEqual(?p, rdf:type) -> (?b rdf:type ex:C1)]
            [any: (?a ?p ?b), (?p rdf:type owl:TransitiveProperty), (?b ?p ?c) -> (?a ?p ?c)]";
        for seed in 1..25 {
            differential(rules, true, false, seed, 25);
        }
    }
}

/// Bindings of rule `ri`'s head `hi` unified with `t` (only the variables in `pre`), or
/// `None` when a constant differs.
fn bind_head(rule: &CRule, hi: usize, pre: &[usize], t: Triple) -> Option<Vec<u64>> {
    let CHead::Triple(s, p, o) = &rule.head[hi] else {
        return None;
    };
    let mut b = vec![0u64; rule.nvars];
    for (slot, v) in [(s, t[0]), (p, t[1]), (o, t[2])] {
        match *slot {
            Slot::Const(c) if c != v => return None,
            Slot::Var(x) if pre.contains(&x) => {
                if b[x] != 0 && b[x] != v {
                    return None;
                }
                b[x] = v;
            }
            _ => {}
        }
    }
    Some(b)
}
