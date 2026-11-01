//! Property paths with `+` and `*` (spec F13 §4.3), maintained by the fixed-point
//! operator of [`super::super::fixpoint`].
//!
//! The path's step is a simple path, an alternative of sequences of triple patterns that
//! [`super::bgp::Bgp`] nodes match. The program's facts are `(graph, start, end)`: `end`
//! is reached from `start` by one or more steps in the graph, with the graph `UNDEF`
//! outside `GRAPH ?g`. A fact derived by the step from `start` directly records `UNDEF`,
//! and one derived from `(graph, start, z)` and a step from `z` records `z`, so the
//! records of one start form a tree of walks over the step.
//!
//! The start nodes are every node with a step when both ends are variables, and the
//! constant end otherwise: a constant object walks the step backward. Steps are probed in
//! the store's permutations at the snapshot before or after the pass's changes, and the
//! starts that reach the changed steps are found by walking the steps backward from
//! them, so no copy of the step relation is kept.
//!
//! `*` adds the zero-length rows outside the program. With two variable ends they are
//! the nodes of the active graph, each subject or object of a quad in it, and with a
//! constant end the constant itself, in the default graph and in every named graph that
//! exists, which is how the query engine evaluates them. Neither needs state: the nodes
//! and graphs that the changed quads name are checked at both snapshots.

use super::super::fixpoint::{self, Change, FixedPoint, Program};
use super::super::pass::{GraphSel, Pass, Which};
use super::super::persist::{Reader, Writer};
use super::super::prov::{AChanges, Ann, Atom, Provenance, Witness};
use super::super::zset::{Changes, Row};
use super::bgp::{Bgp, RPat};
use crate::error::{Error, Result};
use crate::id::Id;
use oxrdf::Term;
use rustc_hash::{FxHashMap, FxHashSet};
use std::sync::Arc;

/// One alternative of the step: a pattern node and the positions of its rows' ends.
#[derive(Clone, Debug)]
pub struct Step {
    pub bgp: Bgp,
    /// the circuit variables of the node's rows
    pub schema: Vec<u32>,
    /// the positions of the step's subject, object and graph in a row
    pub x: usize,
    pub y: usize,
    pub g: Option<usize>,
}

/// An end of the path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum End {
    Var(u32),
    Const(Term),
}

/// The active graph of the path.
#[derive(Clone, Debug)]
pub enum Scope {
    Default,
    Graph(Term),
    /// `GRAPH ?g`, with the circuit variable of `?g`
    Var(u32),
}

/// A fact: graph, start, end.
pub type Fact = [Id; 3];

/// The walks of the step, as a program of the fixed-point operator.
#[derive(Clone, Debug)]
pub struct Closure {
    pub steps: Vec<Step>,
    /// whether a walk follows the step from subject to object
    pub forward: bool,
    /// the constant start, or `None` to start from every node
    pub start: Option<Term>,
    pub scope: Scope,
    cache: Cache,
}

/// A changed step: graph, from, to, whether it holds before and after the changes.
type Edge = (Id, Id, Id, bool, bool);

/// What a pass resolves once.
#[derive(Clone, Debug, Default)]
struct Cache {
    resolved: bool,
    rps: Vec<Vec<RPat>>,
    start: Option<Id>,
    /// the neighbours of a node at a snapshot (`true`: the new one), by walking
    /// direction (`true`: forward)
    next: FxHashMap<(bool, bool, Id, Id), Arc<[Id]>>,
    /// the changed steps
    edges: Option<Vec<Edge>>,
    /// the nodes that reach a node at a snapshot, the node itself among them
    reach: FxHashMap<(bool, Id, Id), Arc<[Id]>>,
}

fn side(w: Which) -> bool {
    matches!(w, Which::New)
}

impl Closure {
    pub fn new(steps: Vec<Step>, forward: bool, start: Option<Term>, scope: Scope) -> Closure {
        Closure {
            steps,
            forward,
            start,
            scope,
            cache: Cache::default(),
        }
    }

    /// Forget what the previous pass resolved.
    pub fn begin(&mut self, p: &mut Pass) {
        self.cache = Cache::default();
        self.cache.rps = self.steps.iter().map(|s| s.bgp.resolve(p)).collect();
        self.cache.start = self.start.as_ref().and_then(|t| p.constant(t));
        self.cache.resolved = true;
    }

    fn ensure(&mut self, p: &mut Pass) {
        if !self.cache.resolved {
            self.begin(p);
        }
    }

    /// The seeds of a step's probe: the walk's `from` and `to` ends and the graph.
    fn seed(&self, s: &Step, from: Option<Id>, to: Option<Id>, g: Id) -> Vec<(usize, Id)> {
        let (a, b) = if self.forward { (s.x, s.y) } else { (s.y, s.x) };
        let mut seed = Vec::with_capacity(3);
        if let Some(f) = from {
            seed.push((a, f));
        }
        if let Some(t) = to {
            seed.push((b, t));
        }
        if let Some(gp) = s.g
            && !g.is_undef()
        {
            seed.push((gp, g));
        }
        seed
    }

    /// The step's rows at `w` with the given ends, as (graph, from, to).
    fn rows(
        &mut self,
        p: &mut Pass,
        w: Which,
        from: Option<Id>,
        to: Option<Id>,
        g: Id,
    ) -> Result<Vec<Fact>> {
        self.ensure(p);
        let mut out = Vec::new();
        for (i, s) in self.steps.iter().enumerate() {
            let seed = self.seed(s, from, to, g);
            for (r, _) in s.bgp.probe_with(p, &self.cache.rps[i], w, &seed)? {
                let (a, b) = if self.forward {
                    (r[s.x], r[s.y])
                } else {
                    (r[s.y], r[s.x])
                };
                let gg = s.g.map_or(Id::UNDEF, |gp| r[gp]);
                out.push([gg, a, b]);
            }
        }
        Ok(out)
    }

    /// The nodes one step from `z` in graph `g` at `w`, walking forward or backward.
    fn next(&mut self, p: &mut Pass, w: Which, fwd: bool, g: Id, z: Id) -> Result<Arc<[Id]>> {
        let key = (side(w), fwd, g, z);
        if let Some(v) = self.cache.next.get(&key) {
            return Ok(v.clone());
        }
        let rows = if fwd {
            self.rows(p, w, Some(z), None, g)?
        } else {
            self.rows(p, w, None, Some(z), g)?
        };
        let mut v: Vec<Id> = rows
            .into_iter()
            .map(|r| if fwd { r[2] } else { r[1] })
            .collect();
        v.sort_unstable();
        v.dedup();
        let v: Arc<[Id]> = v.into();
        self.cache.next.insert(key, v.clone());
        Ok(v)
    }

    /// Whether a step leads from `a` to `b` in graph `g` at `w`.
    fn edge(&mut self, p: &mut Pass, w: Which, g: Id, a: Id, b: Id) -> Result<bool> {
        Ok(self.next(p, w, true, g, a)?.binary_search(&b).is_ok())
    }

    /// The smallest support of a step from `a` to `b` at `w`.
    fn support(&mut self, p: &mut Pass, w: Which, g: Id, a: Id, b: Id) -> Result<Witness> {
        self.ensure(p);
        let saved = p.prov;
        p.prov = Provenance::why();
        let mut best: Option<Witness> = None;
        let mut err = None;
        for s in &self.steps {
            let seed = self.seed(s, Some(a), Some(b), g);
            match s.bgp.probe_a(p, w, &seed) {
                Ok(rows) => {
                    for (_, _, ann) in rows {
                        if let Ann::Why(x) = ann {
                            for (wit, _) in x.supports {
                                if best.as_ref().is_none_or(|b| wit < *b) {
                                    best = Some(wit);
                                }
                            }
                        }
                    }
                }
                Err(e) => {
                    err = Some(e);
                    break;
                }
            }
        }
        p.prov = saved;
        if let Some(e) = err {
            return Err(e);
        }
        best.ok_or_else(|| Error::Corrupt("a path's recorded step does not hold".into()))
    }

    /// The steps the pass's changes touch, each with whether it holds before and after.
    fn edges(&mut self, p: &mut Pass) -> Result<Vec<Edge>> {
        self.ensure(p);
        if let Some(e) = &self.cache.edges {
            return Ok(e.clone());
        }
        let mut seen: FxHashSet<Fact> = FxHashSet::default();
        if !(p.added.is_empty() && p.removed.is_empty()) {
            for s in &mut self.steps {
                let rows: Vec<Row> = if p.prov.is_none() {
                    s.bgp.delta(p)?.into_iter().map(|(r, _)| r).collect()
                } else {
                    s.bgp.delta_a(p)?.into_iter().map(|(r, _, _)| r).collect()
                };
                for r in rows {
                    let (a, b) = if self.forward {
                        (r[s.x], r[s.y])
                    } else {
                        (r[s.y], r[s.x])
                    };
                    let g = s.g.map_or(Id::UNDEF, |gp| r[gp]);
                    seen.insert([g, a, b]);
                }
            }
        }
        let mut out = Vec::with_capacity(seen.len());
        let mut seen: Vec<Fact> = seen.into_iter().collect();
        seen.sort_unstable();
        for [g, a, b] in seen {
            let old = self.edge(p, Which::Old, g, a, b)?;
            let new = self.edge(p, Which::New, g, a, b)?;
            out.push((g, a, b, old, new));
        }
        self.cache.edges = Some(out.clone());
        Ok(out)
    }

    /// The nodes from which walks reach `z` in graph `g` at `w`, `z` among them.
    fn reaching(&mut self, p: &mut Pass, w: Which, g: Id, z: Id) -> Result<Arc<[Id]>> {
        let key = (side(w), g, z);
        if let Some(v) = self.cache.reach.get(&key) {
            return Ok(v.clone());
        }
        let mut seen: FxHashSet<Id> = FxHashSet::default();
        seen.insert(z);
        let mut stack = vec![z];
        while let Some(x) = stack.pop() {
            p.tick()?;
            for &y in self.next(p, w, false, g, x)?.iter() {
                if seen.insert(y) {
                    stack.push(y);
                }
            }
        }
        let v: Arc<[Id]> = seen.into_iter().collect::<Vec<_>>().into();
        self.cache.reach.insert(key, v.clone());
        Ok(v)
    }

    /// The starts whose facts a step from `a` in graph `g` at `w` can extend.
    fn starts(&mut self, p: &mut Pass, w: Which, g: Id, a: Id) -> Result<Arc<[Id]>> {
        self.ensure(p);
        match self.start {
            None => self.reaching(p, w, g, a),
            Some(_) => Ok(self.cache.start.into_iter().collect::<Vec<_>>().into()),
        }
    }
}

impl Program for Closure {
    type Fact = Fact;
    type Record = Id;
    type Cx<'c> = Pass<'c>;

    fn premises(&self, f: &Fact, r: &Id, out: &mut Vec<Fact>) {
        if !r.is_undef() {
            out.push([f[0], f[1], *r]);
        }
    }

    fn axioms(&mut self, p: &mut Pass, out: &mut Vec<(Fact, Id)>) -> Result<()> {
        self.ensure(p);
        match self.start {
            Some(_) => {
                if let Some(s) = self.cache.start {
                    for f in self.rows(p, Which::New, Some(s), None, Id::UNDEF)? {
                        out.push((f, Id::UNDEF));
                    }
                }
            }
            None => {
                for i in 0..self.steps.len() {
                    let s = &self.steps[i];
                    let rows = p.evaluate(&s.bgp.source, &s.schema, None)?;
                    for (r, _) in rows {
                        let (a, b) = if self.forward {
                            (r[s.x], r[s.y])
                        } else {
                            (r[s.y], r[s.x])
                        };
                        if a.is_undef() || b.is_undef() {
                            continue;
                        }
                        let g = s.g.map_or(Id::UNDEF, |gp| r[gp]);
                        out.push(([g, a, b], Id::UNDEF));
                    }
                }
            }
        }
        Ok(())
    }

    fn consequences(
        &mut self,
        p: &mut Pass,
        w: Which,
        f: &Fact,
        _held: &dyn Fn(&Fact) -> bool,
        out: &mut Vec<(Fact, Id)>,
    ) -> Result<()> {
        let [g, s, y] = *f;
        for &b in self.next(p, w, true, g, y)?.iter() {
            out.push(([g, s, b], y));
        }
        Ok(())
    }

    fn prove(
        &mut self,
        p: &mut Pass,
        f: &Fact,
        held: &dyn Fn(&Fact) -> bool,
    ) -> Result<Option<Id>> {
        let [g, s, y] = *f;
        if self.edge(p, Which::New, g, s, y)? {
            return Ok(Some(Id::UNDEF));
        }
        for &z in self.next(p, Which::New, false, g, y)?.iter() {
            if held(&[g, s, z]) {
                return Ok(Some(z));
            }
        }
        Ok(None)
    }

    fn broken(
        &mut self,
        p: &mut Pass,
        recorded: &dyn Fn(&Fact) -> Option<Id>,
        out: &mut Vec<Fact>,
    ) -> Result<()> {
        for (g, a, b, old, new) in self.edges(p)? {
            if !old || new {
                continue;
            }
            for &s in self.starts(p, Which::Old, g, a)?.iter() {
                let f = [g, s, b];
                match recorded(&f) {
                    Some(r) if r.is_undef() && s == a => out.push(f),
                    Some(r) if r == a => out.push(f),
                    _ => {}
                }
            }
        }
        Ok(())
    }

    fn added(
        &mut self,
        p: &mut Pass,
        held: &dyn Fn(&Fact) -> bool,
        out: &mut Vec<(Fact, Id)>,
    ) -> Result<()> {
        for (g, a, b, old, new) in self.edges(p)? {
            if old || !new {
                continue;
            }
            for &s in self.starts(p, Which::New, g, a)?.iter() {
                if s == a {
                    out.push(([g, s, b], Id::UNDEF));
                } else if held(&[g, s, a]) {
                    out.push(([g, s, b], a));
                }
            }
        }
        Ok(())
    }

    fn moved(
        &mut self,
        p: &mut Pass,
        recorded: &dyn Fn(&Fact) -> Option<Id>,
        out: &mut Vec<Fact>,
    ) -> Result<()> {
        for (g, a, b, old, new) in self.edges(p)? {
            if !(old && new) {
                continue;
            }
            if self.support(p, Which::Old, g, a, b)? == self.support(p, Which::New, g, a, b)? {
                continue;
            }
            for &s in self.starts(p, Which::New, g, a)?.iter() {
                let f = [g, s, b];
                match recorded(&f) {
                    Some(r) if r.is_undef() && s == a => out.push(f),
                    Some(r) if r == a => out.push(f),
                    _ => {}
                }
            }
        }
        Ok(())
    }

    fn annotate(
        &mut self,
        p: &mut Pass,
        mode: Provenance,
        f: &Fact,
        r: &Id,
        premises: &[&Ann],
    ) -> Result<Ann> {
        if matches!(mode, Provenance::Count | Provenance::None) {
            return Ok(Ann::derivation(mode, Vec::new()));
        }
        let [g, s, y] = *f;
        let from = if r.is_undef() { s } else { *r };
        let w = self.support(p, Which::New, g, from, y)?;
        let mut atoms: Vec<Atom> = w.atoms().to_vec();
        for a in premises {
            atoms.extend(fixpoint::atoms_of(a));
        }
        Ok(Ann::derivation(mode, atoms))
    }

    fn tick(&mut self, p: &mut Pass) -> Result<()> {
        p.tick()
    }

    fn save(&self, w: &mut Writer, f: &Fact, r: &Id) -> Result<()> {
        for &i in f {
            w.id(i)?;
        }
        w.id(*r)
    }

    fn load(&self, r: &mut Reader) -> Result<(Fact, Id)> {
        let f = [r.id()?, r.id()?, r.id()?];
        let rec = r.id()?;
        if f[1].is_undef() || f[2].is_undef() {
            return Err(Error::Corrupt(
                "a path's state holds a fact without ends".into(),
            ));
        }
        Ok((f, rec))
    }

    fn fact_bytes(&self) -> u64 {
        // the key, the record and the map's slot overhead
        24 + 8 + 8
    }
}

/// A path with `+` or `*` between two ends.
#[derive(Clone, Debug)]
pub struct PathOp {
    pub fixed: FixedPoint<Closure>,
    pub subj: End,
    pub obj: End,
    /// `*`: the zero-length rows besides the walks
    pub zero: bool,
    /// the path's text, for EXPLAIN
    pub text: String,
    prov: Provenance,
}

impl PathOp {
    pub fn new(closure: Closure, subj: End, obj: End, zero: bool, text: String) -> PathOp {
        PathOp {
            fixed: FixedPoint::new(closure),
            subj,
            obj,
            zero,
            text,
            prov: Provenance::None,
        }
    }

    pub fn enable_prov(&mut self, mode: Provenance) {
        self.prov = mode;
        self.fixed.set_provenance(mode);
    }

    fn scope(&self) -> &Scope {
        &self.fixed.program.scope
    }

    /// The row of a walk from `subj` to `obj` in graph `g`, or `None` when the path's
    /// ends exclude it.
    fn row(&self, schema: &[u32], ends: &Ends, g: Id, subj: Id, obj: Id) -> Option<Row> {
        if let End::Const(_) = &self.obj
            && Some(obj) != ends.obj
        {
            return None;
        }
        if let End::Const(_) = &self.subj
            && Some(subj) != ends.subj
        {
            return None;
        }
        if let (End::Var(a), End::Var(b)) = (&self.subj, &self.obj)
            && a == b
            && subj != obj
        {
            return None;
        }
        Some(
            schema
                .iter()
                .map(|v| {
                    if End::Var(*v) == self.subj {
                        subj
                    } else if End::Var(*v) == self.obj {
                        obj
                    } else {
                        g
                    }
                })
                .collect(),
        )
    }

    /// The row of a fact, when the path's ends keep it and a zero-length row does not
    /// already stand for it.
    fn fact_row(&self, schema: &[u32], ends: &Ends, f: &Fact) -> Option<Row> {
        let [g, s, y] = *f;
        if self.zero && s == y {
            return None;
        }
        let (a, b) = if self.fixed.program.forward {
            (s, y)
        } else {
            (y, s)
        };
        self.row(schema, ends, g, a, b)
    }

    fn ends(&self, p: &mut Pass) -> Ends {
        let id = |e: &End, p: &mut Pass| match e {
            End::Var(_) => None,
            End::Const(t) => Some(p.constant(t).unwrap_or_else(|| p.intern(t))),
        };
        Ends {
            subj: id(&self.subj, p),
            obj: id(&self.obj, p),
        }
    }

    fn rows_of(&self, schema: &[u32], ends: &Ends, ch: Vec<Change<Fact>>) -> AChanges {
        ch.into_iter()
            .filter_map(|c| {
                let r = self.fact_row(schema, ends, &c.fact)?;
                let a = c.ann.unwrap_or(Ann::Count(c.weight));
                Some((r, c.weight, a))
            })
            .collect()
    }

    pub fn init(&mut self, p: &mut Pass, schema: &[u32]) -> Result<Changes> {
        Ok(self
            .init_a(p, schema)?
            .into_iter()
            .map(|(r, w, _)| (r, w))
            .collect())
    }

    pub fn delta(&mut self, p: &mut Pass, schema: &[u32]) -> Result<Changes> {
        Ok(self
            .delta_a(p, schema)?
            .into_iter()
            .filter(|(_, w, _)| *w != 0)
            .map(|(r, w, _)| (r, w))
            .collect())
    }

    pub fn init_a(&mut self, p: &mut Pass, schema: &[u32]) -> Result<AChanges> {
        self.fixed.program.begin(p);
        let ends = self.ends(p);
        let max = p.max_memory_bytes;
        let ch = self.fixed.build(p, max)?;
        let mut out = self.rows_of(schema, &ends, ch);
        if self.zero {
            out.extend(self.zero_build(p, schema, &ends)?);
        }
        Ok(out)
    }

    pub fn delta_a(&mut self, p: &mut Pass, schema: &[u32]) -> Result<AChanges> {
        if p.added.is_empty() && p.removed.is_empty() {
            return Ok(Vec::new());
        }
        self.fixed.program.begin(p);
        let ends = self.ends(p);
        let max = p.max_memory_bytes;
        let ch = self.fixed.advance(p, max)?;
        let mut out = self.rows_of(schema, &ends, ch);
        if self.zero {
            out.extend(self.zero_delta(p, schema, &ends)?);
        }
        Ok(out)
    }

    /// The graph selector of the active graph `g` (`UNDEF` outside `GRAPH ?g`).
    fn sel(&self, p: &Pass, g: Id) -> Option<GraphSel> {
        match self.scope() {
            Scope::Default if p.new.union_default_graph => Some(GraphSel::Named),
            Scope::Default => Some(GraphSel::Default),
            Scope::Graph(t) => p.constant(t).map(GraphSel::Exact),
            Scope::Var(_) => Some(GraphSel::Exact(g)),
        }
    }

    /// Whether quad `q` is in the active graph, and the graph it stands for.
    fn in_scope(&self, p: &Pass, q: &[Id; 4]) -> Option<Id> {
        let named = q[3] != Id::DEFAULT_GRAPH;
        match self.scope() {
            Scope::Default if p.new.union_default_graph => named.then_some(Id::UNDEF),
            Scope::Default => (!named).then_some(Id::UNDEF),
            Scope::Graph(t) => (p.constant(t) == Some(q[3])).then_some(Id::UNDEF),
            Scope::Var(_) => named.then_some(q[3]),
        }
    }

    /// The quads of `w` in graph `g` with `n` as subject or object, visiting each until
    /// `f` returns `false`.
    fn node_quads(
        &self,
        p: &Pass,
        w: Which,
        g: Id,
        n: Id,
        f: &mut dyn FnMut([Id; 4]) -> bool,
    ) -> Result<()> {
        let Some(sel) = self.sel(p, g) else {
            return Ok(());
        };
        let mut go = true;
        p.scan(w, Some(n), None, None, sel, &mut |q| {
            go = f(q);
            Ok(go)
        })?;
        if go {
            p.scan(w, None, None, Some(n), sel, &mut |q| Ok(f(q)))?;
        }
        Ok(())
    }

    fn is_node(&self, p: &Pass, w: Which, g: Id, n: Id) -> Result<bool> {
        let mut found = false;
        self.node_quads(p, w, g, n, &mut |_| {
            found = true;
            false
        })?;
        Ok(found)
    }

    /// Whether the active graph `g` exists for the circuit's reader at `w`, as the query
    /// engine decides it for a zero-length path from a constant end. A named graph
    /// exists while it holds a quad and the reader may read it. The default graph exists
    /// when the reader may read it, and the union default graph unless some named graph
    /// holds a quad and the reader may read none of those that do.
    fn graph_exists(&self, p: &Pass, w: Which, g: Id) -> Result<bool> {
        let snap = p.snap(w);
        let access = p.access.as_ref().filter(|a| !a.reads_all());
        let readable = |g: Id| access.is_none_or(|a| a.readable_id(snap, g));
        let holds = |g: Id| -> Result<bool> {
            let mut found = false;
            snap.scan(crate::index::Perm::Gspo, &[g.0], |_| {
                found = true;
                Ok(false)
            })?;
            Ok(found)
        };
        match self.scope() {
            Scope::Default => {
                let Some(a) = access else {
                    return Ok(true);
                };
                if !p.new.union_default_graph {
                    return Ok(a.read.default_graph());
                }
                let graphs = snap.graph_ids()?;
                let seen = graphs.iter().filter(|&&g| a.readable_id(snap, g)).count();
                Ok(seen == graphs.len() || seen > 0)
            }
            Scope::Graph(t) => match p.constant(t) {
                Some(id) => Ok(readable(id) && holds(id)?),
                None => Ok(false),
            },
            Scope::Var(_) => Ok(readable(g) && holds(g)?),
        }
    }

    /// The annotation of the zero-length row of node `n` in graph `g` at `w`: with a
    /// constant end it is no atom in the default graph and unknown in a named one, which
    /// holds while the graph exists; otherwise the quad with the smallest key that makes
    /// `n` a node of the graph.
    fn zero_ann(&self, p: &mut Pass, w: Which, g: Id, n: Id) -> Result<Ann> {
        let mode = p.prov;
        if self.fixed.program.start.is_some() {
            return Ok(match self.scope() {
                Scope::Default => Ann::one(mode),
                _ => Ann::unknown(mode, 1),
            });
        }
        if matches!(mode, Provenance::Count | Provenance::None) {
            return Ok(Ann::one(mode));
        }
        let mut quads: Vec<[Id; 4]> = Vec::new();
        self.node_quads(p, w, g, n, &mut |q| {
            quads.push(q);
            true
        })?;
        // in the union default graph, the named-graph quads that hold the triples
        let best = quads.into_iter().map(|q| (p.fact_key_of(&q), q)).min();
        let Some((_, q)) = best else {
            return Err(Error::Corrupt(
                "a zero-length path row's node is in no quad".into(),
            ));
        };
        Ok(p.fact_ann(&[q]))
    }

    fn zero_row(&self, schema: &[u32], ends: &Ends, g: Id, n: Id) -> Option<Row> {
        // a constant end walks to itself; two constant ends match when they are equal
        if let (End::Const(a), End::Const(b)) = (&self.subj, &self.obj)
            && a != b
        {
            return None;
        }
        let n = match (&ends.subj, &ends.obj) {
            (Some(s), _) => *s,
            (None, Some(o)) => *o,
            _ => n,
        };
        self.row(
            schema,
            &Ends {
                subj: ends.subj.map(|_| n),
                obj: ends.obj.map(|_| n),
            },
            g,
            n,
            n,
        )
    }

    fn zero_build(&self, p: &mut Pass, schema: &[u32], ends: &Ends) -> Result<AChanges> {
        let mut keys: Vec<(Id, Id)> = Vec::new();
        if self.fixed.program.start.is_some() {
            for g in self.graphs(p)? {
                keys.push((g, Id::UNDEF));
            }
        } else {
            let mut seen: FxHashSet<(Id, Id)> = FxHashSet::default();
            let sel = match self.scope() {
                Scope::Default if p.new.union_default_graph => Some(GraphSel::Named),
                Scope::Default => Some(GraphSel::Default),
                Scope::Graph(t) => p.constant(t).map(GraphSel::Exact),
                Scope::Var(_) => Some(GraphSel::Named),
            };
            if let Some(sel) = sel {
                let var = matches!(self.scope(), Scope::Var(_));
                p.scan(Which::New, None, None, None, sel, &mut |q| {
                    let g = if var { q[3] } else { Id::UNDEF };
                    seen.insert((g, q[0]));
                    seen.insert((g, q[2]));
                    Ok(true)
                })?;
            }
            keys = seen.into_iter().collect();
            keys.sort_unstable();
        }
        let mut out = AChanges::new();
        for (g, n) in keys {
            p.tick()?;
            let Some(r) = self.zero_row(schema, ends, g, n) else {
                continue;
            };
            let a = self.zero_ann(p, Which::New, g, n)?;
            out.push((r, 1, a));
        }
        Ok(out)
    }

    /// The active graphs with a quad at the new snapshot, for a constant end.
    fn graphs(&self, p: &Pass) -> Result<Vec<Id>> {
        Ok(match self.scope() {
            Scope::Default => {
                if self.graph_exists(p, Which::New, Id::UNDEF)? {
                    vec![Id::UNDEF]
                } else {
                    vec![]
                }
            }
            Scope::Graph(_) => {
                if self.graph_exists(p, Which::New, Id::UNDEF)? {
                    vec![Id::UNDEF]
                } else {
                    vec![]
                }
            }
            Scope::Var(_) => {
                let mut out = Vec::new();
                for g in p.new.graph_ids()? {
                    if self.graph_exists(p, Which::New, g)? {
                        out.push(g);
                    }
                }
                out
            }
        })
    }

    fn zero_delta(&self, p: &mut Pass, schema: &[u32], ends: &Ends) -> Result<AChanges> {
        let constant = self.fixed.program.start.is_some();
        // the default graph's existence changes only with the named graphs of the union
        if constant && matches!(self.scope(), Scope::Default) && !p.new.union_default_graph {
            return Ok(AChanges::new());
        }
        let mut keys: FxHashSet<(Id, Id)> = FxHashSet::default();
        for q in p.added.iter().chain(p.removed.iter()) {
            let Some(g) = self.in_scope(p, q) else {
                continue;
            };
            if constant {
                keys.insert((g, Id::UNDEF));
            } else {
                keys.insert((g, q[0]));
                keys.insert((g, q[2]));
            }
        }
        let mut keys: Vec<(Id, Id)> = keys.into_iter().collect();
        keys.sort_unstable();
        let annotated = !p.prov.is_none();
        let mut out = AChanges::new();
        for (g, n) in keys {
            p.tick()?;
            let (old, new) = if constant {
                (
                    self.graph_exists(p, Which::Old, g)?,
                    self.graph_exists(p, Which::New, g)?,
                )
            } else {
                (
                    self.is_node(p, Which::Old, g, n)?,
                    self.is_node(p, Which::New, g, n)?,
                )
            };
            let Some(r) = self.zero_row(schema, ends, g, n) else {
                continue;
            };
            match (old, new) {
                (false, true) => {
                    let a = self.zero_ann(p, Which::New, g, n)?;
                    out.push((r, 1, a));
                }
                (true, false) => {
                    let a = self.zero_ann(p, Which::Old, g, n)?;
                    out.push((r, -1, a.scaled(-1)));
                }
                (true, true) if annotated && !constant => {
                    let a = self.zero_ann(p, Which::Old, g, n)?;
                    let b = self.zero_ann(p, Which::New, g, n)?;
                    if a != b {
                        out.push((r, 0, b.sub(&a)));
                    }
                }
                _ => {}
            }
        }
        Ok(out)
    }

    pub fn bytes(&self) -> u64 {
        self.fixed.bytes()
    }

    pub fn save(&self, w: &mut Writer) -> Result<()> {
        self.fixed.save(w)
    }

    pub fn load(&mut self, r: &mut Reader) -> Result<()> {
        self.fixed.load(r)
    }

    pub fn describe(&self) -> String {
        self.text.clone()
    }

    /// The step's patterns, for resolving fact keys.
    pub fn bgps(&self) -> impl Iterator<Item = &Bgp> + '_ {
        self.fixed.program.steps.iter().map(|s| &s.bgp)
    }
}

/// The ids of the path's constant ends at a pass.
struct Ends {
    subj: Option<Id>,
    obj: Option<Id>,
}
