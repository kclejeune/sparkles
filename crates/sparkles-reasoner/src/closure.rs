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
//! [`crate::engine::incremental_blocker`]) runs without operators, because the order of
//! derivation matters there.
//!
//! The operator is semi-naive. It keeps, per predicate, the row up to which the relation
//! is closed. The relation of the rows before that mark needs nothing more, so with few
//! new edges the operator reads only the nodes that reach a new edge and the nodes they
//! reach, through the graph's indexes. With many new edges it reads the whole relation.
//!
//! `notEqual` compares literal values, so a filter of reflexive pairs differs from a
//! comparison of ids when two literals with distinct ids and equal values meet in one
//! relation. The operator then gives the rule back to the ordinary evaluator for the rest
//! of the run, with one full evaluation, so the result stays the rules' fixpoint.

use crate::budget::{Budget, Held, MemoryExceeded, table_bytes};
use crate::builtins::{BuiltinKind, same_value};
use crate::engine::{Atom, CHead, CRule, Slot, incremental_blocker};
use crate::facts::Memo;
use crate::graph::{Graph, Triple};
use crate::terms::{Kind, Terms};
use rustc_hash::{FxHashMap, FxHashSet};
use std::mem::size_of;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

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
}

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
            (r.nvars == 3).then_some((Over::Fixed(c), irreflexive))
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
            (r.nvars == 4 && p != x && p != y && p != z)
                .then_some((Over::Guarded(g, o), irreflexive))
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

impl Closures {
    /// The operators of `rules`, or `None` when no rule has one or the program has a rule
    /// whose result depends on the order of derivation. `start` is the row before which
    /// the facts are closed under the rules, as for [`crate::engine::run`].
    pub(crate) fn detect(rules: &[CRule], terms: &Terms, start: Option<u32>) -> Option<Closures> {
        if rules
            .iter()
            .any(|r| incremental_blocker(r, terms).is_some())
        {
            return None;
        }
        let syms: Vec<(usize, u64)> = rules
            .iter()
            .enumerate()
            .filter_map(|(i, r)| symmetric(r).map(|(c, _)| (i, c)))
            .collect();
        let mut ops: Vec<Op> = Vec::new();
        for (i, r) in rules.iter().enumerate() {
            let Some((over, irreflexive)) = transitive(r) else {
                continue;
            };
            let mut op = Op {
                rules: vec![i],
                over,
                shape: Shape::Transitive,
                irreflexive,
                marks: FxHashMap::default(),
                off: false,
            };
            if let Over::Fixed(c) = over
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
        (!ops.is_empty()).then_some(Closures { ops, start })
    }

    /// Whether rule `ri` is evaluated by an operator now.
    pub(crate) fn replaces(&self, ri: usize) -> bool {
        self.ops.iter().any(|op| !op.off && op.rules.contains(&ri))
    }

    /// The operators' names, for the log.
    pub(crate) fn describe(&self, rules: &[CRule]) -> Vec<String> {
        self.ops
            .iter()
            .map(|op| {
                let names: Vec<&str> = op.rules.iter().map(|&r| rules[r].name.as_str()).collect();
                format!("{:?} {}", op.shape, names.join("+"))
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
        let start = self.start;
        for op in &mut self.ops {
            if op.off {
                continue;
            }
            let now = g.len();
            let preds = op.predicates(g, now)?;
            let mut outs: Vec<Vec<Triple>> = Vec::new();
            let mut held = Held::new(budget);
            let mut done: Vec<u64> = Vec::new();
            let mut total = 0usize;
            let mut back = false;
            for q in preds {
                let mark = match op.marks.get(&q) {
                    Some(&m) => m,
                    None => op.initial(g, q, start),
                };
                if mark >= now || g.count_p(q, mark, now) == 0 {
                    continue;
                }
                let local = Local::build(g, q, mark, now, op.shape, budget)?;
                let out = match op.shape {
                    Shape::Transitive => {
                        if op.irreflexive && local.literal_subject(terms) {
                            None
                        } else {
                            Some(local.transitive(q, op.irreflexive, &mut held, cancel)?)
                        }
                    }
                    Shape::Equivalence => {
                        local.equivalence(q, op.irreflexive, terms, &mut held, cancel)?
                    }
                };
                let Some(out) = out else {
                    back = true;
                    break;
                };
                if cancel.is_some_and(|c| c.load(Ordering::Relaxed)) {
                    anyhow::bail!("reasoning cancelled");
                }
                total += out.len();
                if total > max_out {
                    return Err(crate::InferredLimitExceeded {
                        limit,
                        context: format!(
                            " (the closure of rule '{}' produced {total} candidate triples)",
                            rules[op.rules[0]].name
                        ),
                    }
                    .into());
                }
                done.push(q);
                if !out.is_empty() {
                    outs.push(out);
                }
            }
            if back {
                tracing::debug!(rule = %rules[op.rules[0]].name, "closure operator gives its rules back");
                op.off = true;
                stepped.refresh.extend(op.rules.iter().copied());
                continue;
            }
            let before = g.len();
            if !outs.is_empty() {
                g.merge(outs, held)?;
            }
            let after = g.len();
            stepped.added += (after - before) as usize;
            for q in done {
                op.marks.insert(q, after);
            }
        }
        Ok(stepped)
    }
}

impl Op {
    /// The predicates the operator closes now.
    fn predicates(&self, g: &Graph, now: u32) -> anyhow::Result<Vec<u64>> {
        match self.over {
            Over::Fixed(c) => Ok(vec![c]),
            Over::Guarded(gp, go) => {
                let mut out: Vec<u64> = Vec::new();
                if let Some(b) = g.base()
                    && g.reads_base(0, now)
                {
                    b.for_each(None, Some(gp), Some(go), |t| {
                        out.push(t[0]);
                        true
                    })?;
                }
                let c = g.cands(None, Some(gp), Some(go), g.offset(), now);
                for i in 0..c.len() {
                    let r = c.get(i);
                    if g.alive(r) {
                        let t = g.triple(r);
                        if t[1] == gp && t[2] == go {
                            out.push(t[0]);
                        }
                    }
                }
                out.sort_unstable();
                out.dedup();
                Ok(out)
            }
        }
    }

    /// The mark of a predicate the operator has not closed in this run: the facts
    /// before `start` are closed under the rules, the guard included.
    fn initial(&self, g: &Graph, q: u64, start: Option<u32>) -> u32 {
        let Some(st) = start else {
            return 0;
        };
        match self.over {
            Over::Fixed(_) => st,
            Over::Guarded(gp, go) => {
                let mut memo = Memo::default();
                if g.holds_before(&mut memo, &[q, gp, go], st) {
                    st
                } else {
                    0
                }
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
    /// the charge of the above
    _held: Held,
}

/// Edges a scan or a search collects before it charges them.
const CHARGE_EVERY: usize = 1 << 16;

struct Builder {
    ids: Vec<u64>,
    at: FxHashMap<u64, u32>,
    pairs: Vec<(u32, u32)>,
    held: Held,
    since: usize,
}

impl Builder {
    fn new(budget: &Arc<Budget>) -> Builder {
        Builder {
            ids: Vec::new(),
            at: FxHashMap::default(),
            pairs: Vec::new(),
            held: Held::new(budget),
            since: 0,
        }
    }

    fn bytes(&self) -> usize {
        self.ids.capacity() * size_of::<u64>()
            + table_bytes(self.at.capacity(), size_of::<(u64, u32)>())
            + self.pairs.capacity() * size_of::<(u32, u32)>()
    }

    fn charge(&mut self) -> Result<(), MemoryExceeded> {
        let b = self.bytes();
        self.held.try_set(b)
    }

    #[inline]
    fn node(&mut self, id: u64) -> (u32, bool) {
        if let Some(&n) = self.at.get(&id) {
            return (n, false);
        }
        let n = self.ids.len() as u32;
        self.ids.push(id);
        self.at.insert(id, n);
        (n, true)
    }

    #[inline]
    fn edge(&mut self, s: u64, o: u64) -> Result<(), MemoryExceeded> {
        let (a, _) = self.node(s);
        let (b, _) = self.node(o);
        self.pairs.push((a, b));
        self.since += 1;
        if self.since >= CHARGE_EVERY {
            self.since = 0;
            self.charge()?;
        }
        Ok(())
    }

    /// The compressed adjacency of the pairs, sorted and without duplicates.
    fn finish(mut self, emit: Vec<bool>) -> Result<Local, MemoryExceeded> {
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
        drop(at);
        let mut held = self.held;
        held.try_set(
            self.ids.capacity() * size_of::<u64>()
                + (off.capacity() + adj.capacity()) * size_of::<u32>()
                + emit.capacity(),
        )?;
        Ok(Local {
            ids: self.ids,
            off,
            adj,
            emit,
            _held: held,
        })
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
    ) -> anyhow::Result<Local> {
        let total = g.count_p(q, 0, now);
        let fresh = g.count_p(q, mark, now);
        if mark >= g.offset()
            && fresh.saturating_mul(4) < total
            && let Some(l) = Local::around(g, q, mark, now, shape, budget, total / 2)?
        {
            return Ok(l);
        }
        Local::scan(g, q, now, budget)
    }

    /// Every edge of `q` in rows `< now`.
    fn scan(g: &Graph, q: u64, now: u32, budget: &Arc<Budget>) -> anyhow::Result<Local> {
        let mut b = Builder::new(budget);
        if let Some(base) = g.base()
            && g.reads_base(0, now)
        {
            let mut err = None;
            base.for_each(None, Some(q), None, |t| match b.edge(t[0], t[2]) {
                Ok(()) => true,
                Err(e) => {
                    err = Some(e);
                    false
                }
            })?;
            if let Some(e) = err {
                return Err(e.into());
            }
        }
        let c = g.cands(None, Some(q), None, g.offset(), now);
        for i in 0..c.len() {
            let r = c.get(i);
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
        mark: u32,
        now: u32,
        shape: Shape,
        budget: &Arc<Budget>,
        limit: usize,
    ) -> anyhow::Result<Option<Local>> {
        let mut memo = Memo::default();
        let mut b = Builder::new(budget);
        let mut seeds: Vec<u64> = Vec::new();
        let c = g.cands(None, Some(q), None, mark.max(g.offset()), now);
        for i in 0..c.len() {
            let r = c.get(i);
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
                if sources.insert(s) {
                    queue.push(s);
                }
            }
            let mut steps = 0usize;
            while let Some(x) = queue.pop() {
                let mut back = g.try_subjects(&mut memo, q, x, now)?;
                if shape == Shape::Equivalence {
                    back.extend(g.try_objects(&mut memo, x, q, now)?);
                }
                steps += back.len();
                if steps > limit {
                    return Ok(None);
                }
                for p in back {
                    if sources.insert(p) {
                        queue.push(p);
                    }
                }
            }
            // every node the sources reach, with its out-edges
            let mut seen: FxHashSet<u64> = FxHashSet::default();
            let mut order: Vec<u64> = sources.iter().copied().collect();
            order.sort_unstable();
            for &s in &order {
                seen.insert(s);
                b.node(s);
            }
            let mut queue = order;
            let mut edges = 0usize;
            while let Some(x) = queue.pop() {
                for o in g.try_objects(&mut memo, x, q, now)? {
                    b.edge(x, o)?;
                    edges += 1;
                    if seen.insert(o) {
                        queue.push(o);
                    }
                }
                if edges > limit {
                    return Ok(None);
                }
            }
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
        (0..self.ids.len())
            .any(|x| self.off[x] < self.off[x + 1] && terms.kind(self.ids[x]) == Kind::Literal)
    }

    /// The pairs `(x, z)` of the transitive closure, for the nodes that emit, that are
    /// not edges, without `x = z` when `irreflexive`.
    fn transitive(
        &self,
        q: u64,
        irreflexive: bool,
        held: &mut Held,
        cancel: Option<&AtomicBool>,
    ) -> anyhow::Result<Vec<Triple>> {
        let n = self.ids.len();
        let (comp, ncomp) = self.components();
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
        let mut work = Held::try_new(held.budget(), (n * 3 + ncomp * 3) * size_of::<u32>())?;
        // the distinct components that point into each one, so that its reachable set is
        // released once they have read it
        let mut stamp = vec![u32::MAX; ncomp.max(n)];
        let mut readers = vec![0u32; ncomp];
        for c in 0..ncomp {
            for &x in &members[moff[c] as usize..moff[c + 1] as usize] {
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
        let mut out: Vec<Triple> = Vec::new();
        let mut out_charged = 0usize;
        let mut succ: Vec<usize> = Vec::new();
        let mut set: Vec<u32> = Vec::new();
        // Tarjan numbers components in reverse topological order: a component's
        // successors come before it
        for c in 0..ncomp {
            if c % 4096 == 0 && cancel.is_some_and(|f| f.load(Ordering::Relaxed)) {
                anyhow::bail!("reasoning cancelled");
            }
            let mine = &members[moff[c] as usize..moff[c + 1] as usize];
            succ.clear();
            let mut cyclic = mine.len() > 1;
            for &x in mine {
                for &y in self.out(x) {
                    let d = comp[y as usize] as usize;
                    if d == c {
                        cyclic = true;
                    } else if !succ.contains(&d) {
                        succ.push(d);
                    }
                }
            }
            // the nodes reachable by a path of one edge or more
            set.clear();
            let stamp_c = c as u32;
            for &d in &succ {
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
                    out.push([self.ids[x as usize], q, self.ids[z as usize]]);
                }
                let bytes = out.capacity() * size_of::<Triple>();
                if bytes > out_charged {
                    held.try_grow(bytes - out_charged)?;
                    out_charged = bytes;
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
        Ok(out)
    }

    /// Tarjan's strongly connected components, iteratively: the component of each node
    /// and the number of components, numbered in reverse topological order.
    fn components(&self) -> (Vec<u32>, usize) {
        let n = self.ids.len();
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
        (comp, ncomp as usize)
    }

    /// The pairs `(x, y)` of the classes of the symmetric and transitive closure, for the
    /// nodes that emit, that are not edges, without `x = y` when `irreflexive`. `None`
    /// when a class holds two literals with distinct ids and equal values, where the
    /// rules' `notEqual` differs from a comparison of ids.
    fn equivalence(
        &self,
        q: u64,
        irreflexive: bool,
        terms: &Terms,
        held: &mut Held,
        cancel: Option<&AtomicBool>,
    ) -> anyhow::Result<Option<Vec<Triple>>> {
        let n = self.ids.len();
        let mut parent: Vec<u32> = (0..n as u32).collect();
        let _work = Held::try_new(held.budget(), n * 3 * size_of::<u32>())?;
        fn find(parent: &mut [u32], mut x: u32) -> u32 {
            while parent[x as usize] != x {
                let p = parent[x as usize];
                parent[x as usize] = parent[p as usize];
                x = p;
            }
            x
        }
        for x in 0..n as u32 {
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
        let mut out: Vec<Triple> = Vec::new();
        let mut out_charged = 0usize;
        for r in 0..n {
            if r % 4096 == 0 && cancel.is_some_and(|f| f.load(Ordering::Relaxed)) {
                anyhow::bail!("reasoning cancelled");
            }
            let class = &members[moff[r] as usize..moff[r + 1] as usize];
            if class.len() < 2 || !class.iter().any(|&x| self.emits(x)) {
                continue;
            }
            let literals: Vec<u64> = class
                .iter()
                .map(|&x| self.ids[x as usize])
                .filter(|&id| terms.kind(id) == Kind::Literal)
                .collect();
            if literals.len() > 64 {
                return Ok(None);
            }
            for (i, &a) in literals.iter().enumerate() {
                if literals[i + 1..].iter().any(|&b| same_value(terms, a, b)) {
                    return Ok(None);
                }
            }
            // members are in ascending local order, as the adjacency is
            for &x in class {
                if !self.emits(x) {
                    continue;
                }
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
                    out.push([self.ids[x as usize], q, self.ids[z as usize]]);
                }
                let bytes = out.capacity() * size_of::<Triple>();
                if bytes > out_charged {
                    held.try_grow(bytes - out_charged)?;
                    out_charged = bytes;
                }
            }
        }
        Ok(Some(out))
    }
}
