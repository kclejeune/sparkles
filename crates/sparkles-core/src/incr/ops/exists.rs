//! A filter whose condition has a correlated `EXISTS` (spec F13 §4.2): the condition
//! reads outer bindings that no join key carries, such as a variable in a filter of the
//! inner pattern or one the outer rows do not always bind, so the condition is evaluated
//! by substitution, as a one-off query evaluates it, for the rows it can have changed.
//!
//! The state holds each input row with its weight and the condition's last answer. A
//! commit changes an answer only through the quads the condition's patterns read, so a
//! pass outside their footprint keeps every answer. A pass inside it scopes the rows to
//! evaluate again: each changed quad is matched against the condition's triple patterns,
//! and a match binds the outer variables the pattern names, so only rows with those
//! bindings, or without them, can have a new answer. A pattern that no triple pattern
//! scopes, such as a property path, a property function, a `GRAPH` whose body reads no
//! triple or that holds another `GRAPH`, or one of ARQ's special graphs, sends every row
//! to the engine again. The rows the input changes are evaluated when they are new, and
//! keep their answer otherwise.
//!
//! The evaluation runs the query engine on the inner pattern with the row's bindings
//! substituted, so the answer is the engine's own, with its treatment of nested scopes
//! and paths. The engine does not report the derivations of an answer, so with a
//! provenance setting every row's derivations are unknown, as they are for a `recompute`
//! node.

use super::super::footprint::{Footprint, Resolved};
use super::super::pass::Pass;
use super::super::persist::{Reader, Writer};
use super::super::prov::{self, AChanges, Ann, Provenance};
use super::super::zset::{Changes, Row, consolidate, row_bytes};
use super::Node;
use crate::error::Result;
use crate::id::Id;
use oxrdf::Term;
use rustc_hash::FxHashMap;
use spargebra::algebra::{AggregateExpression, Expression, GraphPattern, OrderExpression};
use spargebra::term::{NamedNodePattern, TermPattern};

/// The graph a probe's quads are in.
#[derive(Clone, Debug)]
enum PGraph {
    /// the default graph, or any named graph when the dataset reads their union as the
    /// default graph
    Default,
    /// any named graph (`GRAPH ?g` with `?g` not an outer variable)
    Named,
    Graph(Term),
    /// `GRAPH ?g` with `?g` this input column
    Var(usize),
}

/// A position of a triple pattern.
#[derive(Clone, Debug)]
enum Slot {
    Any,
    Const(Term),
    /// an outer variable, bound from this input column
    Col(usize),
}

/// A triple pattern of the condition: the quads it reads and the input columns a
/// matching quad binds.
#[derive(Clone, Debug)]
struct Probe {
    g: PGraph,
    s: Slot,
    p: Slot,
    o: Slot,
}

/// A probe with its constants resolved to ids of a snapshot.
#[derive(Clone, Copy, Debug)]
enum RSlot {
    Any,
    Const(Id),
    Col(usize),
}

#[derive(Clone, Copy, Debug)]
enum RGraph {
    Default,
    Named,
    Graph(Id),
    Var(usize),
}

#[derive(Clone, Copy, Debug)]
struct RProbe {
    g: RGraph,
    s: RSlot,
    p: RSlot,
    o: RSlot,
}

/// The triple patterns of `patterns` as probes, and whether a part of them escapes the
/// probes. `outer` names the input columns' variables.
pub struct Probes {
    probes: Vec<Probe>,
    unscoped: bool,
    /// inside a `GRAPH`
    nested: bool,
}

impl Probes {
    pub fn of(patterns: &[GraphPattern], outer: &[String]) -> Probes {
        let mut out = Probes {
            probes: Vec::new(),
            unscoped: false,
            nested: false,
        };
        let cols: Vec<(String, usize)> = outer
            .iter()
            .enumerate()
            .map(|(i, v)| (v.clone(), i))
            .collect();
        for p in patterns {
            out.walk(p, &PGraph::Default, &cols);
        }
        out
    }

    fn walk(&mut self, gp: &GraphPattern, g: &PGraph, cols: &[(String, usize)]) {
        use GraphPattern as GP;
        let col = |v: &str| cols.iter().find(|(n, _)| n == v).map(|(_, i)| *i);
        let term = |t: &TermPattern| match t {
            TermPattern::Variable(v) => col(v.as_str()).map_or(Slot::Any, Slot::Col),
            TermPattern::NamedNode(n) => Slot::Const(Term::NamedNode(n.clone())),
            TermPattern::Literal(l) => Slot::Const(Term::Literal(l.clone())),
            TermPattern::BlankNode(_) | TermPattern::Triple(_) => Slot::Any,
        };
        match gp {
            GP::Bgp { patterns } => {
                for t in patterns {
                    // a property function reads triples its pattern does not name
                    if let NamedNodePattern::NamedNode(n) = &t.predicate
                        && super::super::compile::property_function(n.as_str())
                    {
                        self.unscoped = true;
                        continue;
                    }
                    self.probes.push(Probe {
                        g: g.clone(),
                        s: term(&t.subject),
                        p: match &t.predicate {
                            NamedNodePattern::NamedNode(n) => {
                                Slot::Const(Term::NamedNode(n.clone()))
                            }
                            NamedNodePattern::Variable(v) => {
                                col(v.as_str()).map_or(Slot::Any, Slot::Col)
                            }
                        },
                        o: term(&t.object),
                    });
                }
            }
            GP::Path { .. } => self.unscoped = true,
            GP::Join { left, right }
            | GP::Union { left, right }
            | GP::Minus { left, right }
            | GP::Lateral { left, right }
            | GP::SemiJoin { left, right }
            | GP::AntiJoin { left, right } => {
                self.walk(left, g, cols);
                self.walk(right, g, cols);
            }
            GP::LeftJoin {
                left,
                right,
                expression,
            } => {
                self.walk(left, g, cols);
                self.walk(right, g, cols);
                if let Some(e) = expression {
                    self.expr(e, g, cols);
                }
            }
            GP::Filter { expr, inner } => {
                self.expr(expr, g, cols);
                self.walk(inner, g, cols);
            }
            GP::Extend {
                inner, expression, ..
            }
            | GP::Assign {
                inner, expression, ..
            }
            | GP::Unfold {
                inner, expression, ..
            } => {
                self.expr(expression, g, cols);
                self.walk(inner, g, cols);
            }
            GP::Graph { name, inner } => {
                let g = match name {
                    // the special graphs of ARQ read other graphs
                    NamedNodePattern::NamedNode(n) if n.as_str().starts_with("urn:x-arq:") => {
                        self.unscoped = true;
                        return;
                    }
                    NamedNodePattern::NamedNode(n) => PGraph::Graph(Term::NamedNode(n.clone())),
                    NamedNodePattern::Variable(v) => {
                        col(v.as_str()).map_or(PGraph::Named, PGraph::Var)
                    }
                };
                // a GRAPH inside a GRAPH still ranges over every graph, and a GRAPH
                // whose body reads no triple depends on which graphs exist
                if self.nested {
                    self.unscoped = true;
                }
                let before = self.probes.len();
                let outer = std::mem::replace(&mut self.nested, true);
                self.walk(inner, &g, cols);
                self.nested = outer;
                if self.probes.len() == before {
                    self.unscoped = true;
                }
            }
            // the engine substitutes a subquery's projected variables only
            GP::Project { inner, variables } => {
                let kept: Vec<(String, usize)> = cols
                    .iter()
                    .filter(|(n, _)| variables.iter().any(|v| v.as_str() == n))
                    .cloned()
                    .collect();
                self.walk(inner, g, &kept);
            }
            GP::Group {
                inner,
                variables,
                aggregates,
            } => {
                for (_, a) in aggregates {
                    match a {
                        AggregateExpression::FunctionCall { expr, .. } => self.expr(expr, g, cols),
                        AggregateExpression::Fold { expr, value, .. } => {
                            self.expr(expr, g, cols);
                            if let Some(v) = value {
                                self.expr(v, g, cols);
                            }
                        }
                        AggregateExpression::CountSolutions { .. } => {}
                    }
                }
                let kept: Vec<(String, usize)> = cols
                    .iter()
                    .filter(|(n, _)| variables.iter().any(|v| v.as_str() == n))
                    .cloned()
                    .collect();
                self.walk(inner, g, &kept);
            }
            GP::OrderBy { inner, expression } => {
                for o in expression {
                    let (OrderExpression::Asc(e) | OrderExpression::Desc(e)) = o;
                    self.expr(e, g, cols);
                }
                self.walk(inner, g, cols);
            }
            GP::Distinct { inner } | GP::Reduced { inner } | GP::Slice { inner, .. } => {
                self.walk(inner, g, cols)
            }
            GP::Values { .. } => {}
            _ => self.unscoped = true,
        }
    }

    /// The patterns of the `EXISTS` in `e`.
    fn expr(&mut self, e: &Expression, g: &PGraph, cols: &[(String, usize)]) {
        let mut found = Vec::new();
        exists_patterns(e, &mut found);
        for p in found {
            self.walk(p, g, cols);
        }
    }
}

/// The patterns of every `EXISTS` in `e`, outermost first.
pub fn exists_patterns<'a>(e: &'a Expression, out: &mut Vec<&'a GraphPattern>) {
    use Expression as E;
    match e {
        E::Exists(p) => out.push(p),
        E::Or(a, b)
        | E::And(a, b)
        | E::Equal(a, b)
        | E::SameTerm(a, b)
        | E::Greater(a, b)
        | E::GreaterOrEqual(a, b)
        | E::Less(a, b)
        | E::LessOrEqual(a, b)
        | E::Add(a, b)
        | E::Subtract(a, b)
        | E::Multiply(a, b)
        | E::Divide(a, b) => {
            exists_patterns(a, out);
            exists_patterns(b, out);
        }
        E::UnaryPlus(a) | E::UnaryMinus(a) | E::Not(a) => exists_patterns(a, out),
        E::In(a, l) => {
            exists_patterns(a, out);
            l.iter().for_each(|x| exists_patterns(x, out));
        }
        E::If(a, b, c) => {
            exists_patterns(a, out);
            exists_patterns(b, out);
            exists_patterns(c, out);
        }
        E::Coalesce(l) | E::FunctionCall(_, l) => l.iter().for_each(|x| exists_patterns(x, out)),
        _ => {}
    }
}

#[derive(Clone, Debug)]
pub struct ExistsOp {
    pub inner: Node,
    /// the condition, over the inner's rows
    pub expr: Expression,
    /// the footprint of the condition's patterns
    pub footprint: Footprint,
    probes: Vec<Probe>,
    unscoped: bool,
    /// the input rows: weight and whether the condition held
    rows: FxHashMap<Row, (i64, bool)>,
    resolved: Option<Resolved>,
    prov: Provenance,
}

impl ExistsOp {
    pub fn new(inner: Node, expr: Expression, footprint: Footprint, probes: Probes) -> ExistsOp {
        ExistsOp {
            inner,
            expr,
            footprint,
            probes: probes.probes,
            unscoped: probes.unscoped,
            rows: FxHashMap::default(),
            resolved: None,
            prov: Provenance::None,
        }
    }

    pub fn enable_prov(&mut self, mode: Provenance) {
        self.prov = mode;
    }

    /// Whether the condition's answers can scope to fewer than every row, for EXPLAIN.
    pub fn scoped(&self) -> bool {
        !self.unscoped
    }

    /// The condition for each of `rows`.
    fn answers(&self, p: &mut Pass, rows: &[Row]) -> Result<Vec<bool>> {
        if rows.is_empty() {
            return Ok(Vec::new());
        }
        let refs: Vec<&[Id]> = rows.iter().map(|r| &r[..]).collect();
        let v = p.eval(&self.inner.schema, &refs, &self.expr, true)?;
        Ok(v.into_iter()
            .map(|b| b == Some(Id::from_bool(true)))
            .collect())
    }

    /// The probes with their constants as ids of the pass's new snapshot. A probe whose
    /// constant the snapshot lacks matches nothing.
    fn resolved_probes(&self, p: &Pass) -> Vec<RProbe> {
        let slot = |s: &Slot| -> Option<RSlot> {
            Some(match s {
                Slot::Any => RSlot::Any,
                Slot::Col(c) => RSlot::Col(*c),
                Slot::Const(t) => RSlot::Const(p.constant(t)?),
            })
        };
        self.probes
            .iter()
            .filter_map(|pr| {
                let g = match &pr.g {
                    PGraph::Default => RGraph::Default,
                    PGraph::Named => RGraph::Named,
                    PGraph::Var(c) => RGraph::Var(*c),
                    PGraph::Graph(t) => RGraph::Graph(p.constant(t)?),
                };
                Some(RProbe {
                    g,
                    s: slot(&pr.s)?,
                    p: slot(&pr.p)?,
                    o: slot(&pr.o)?,
                })
            })
            .collect()
    }

    /// The input columns a quad matching `pr` binds, or `None` when it does not match.
    fn bindings(pr: &RProbe, q: &[Id; 4], union: bool) -> Option<Vec<(usize, Id)>> {
        let mut out: Vec<(usize, Id)> = Vec::new();
        let mut bind = |c: usize, v: Id| -> bool {
            match out.iter().find(|(x, _)| *x == c) {
                Some((_, w)) => *w == v,
                None => {
                    out.push((c, v));
                    true
                }
            }
        };
        let ok = match pr.g {
            RGraph::Default => q[3] == Id::DEFAULT_GRAPH || (union && q[3] != Id::DEFAULT_GRAPH),
            RGraph::Named => q[3] != Id::DEFAULT_GRAPH,
            RGraph::Graph(g) => q[3] == g,
            RGraph::Var(c) => q[3] != Id::DEFAULT_GRAPH && bind(c, q[3]),
        };
        if !ok {
            return None;
        }
        for (slot, v) in [(pr.s, q[0]), (pr.p, q[1]), (pr.o, q[2])] {
            let ok = match slot {
                RSlot::Any => true,
                RSlot::Const(x) => x == v,
                RSlot::Col(c) => bind(c, v),
            };
            if !ok {
                return None;
            }
        }
        Some(out)
    }

    /// The rows whose answer the pass can have changed: none when its changes miss the
    /// condition's footprint, and otherwise those whose bindings a changed quad touches.
    fn affected(&mut self, p: &mut Pass) -> Result<Vec<Row>> {
        if p.full || (p.added.is_empty() && p.removed.is_empty()) {
            return Ok(Vec::new());
        }
        if !self.resolved.as_ref().is_some_and(|r| r.current(&p.new)) {
            self.resolved = Some(self.footprint.resolve(&p.new));
        }
        let r = self.resolved.as_ref().expect("resolved above");
        if !r.touches(p.added.iter().chain(p.removed.iter())) {
            return Ok(Vec::new());
        }
        let mut all = self.unscoped;
        let mut sets: Vec<Vec<(usize, Id)>> = Vec::new();
        if !all {
            let probes = self.resolved_probes(p);
            let union = p.new.union_default_graph;
            'quads: for q in p.added.iter().chain(p.removed.iter()) {
                for pr in &probes {
                    if let Some(b) = Self::bindings(pr, q, union) {
                        if b.is_empty() {
                            all = true;
                            break 'quads;
                        }
                        if !sets.contains(&b) {
                            sets.push(b);
                        }
                    }
                }
            }
        }
        let mut out = Vec::new();
        for (row, (w, _)) in &self.rows {
            p.tick()?;
            if *w == 0 {
                continue;
            }
            let hit = all
                || sets
                    .iter()
                    .any(|s| s.iter().all(|(c, v)| row[*c] == *v || row[*c].is_undef()));
            if hit {
                out.push(row.clone());
            }
        }
        Ok(out)
    }

    fn apply(&mut self, p: &mut Pass, dl: Changes, affected: Vec<Row>) -> Result<Changes> {
        let mut out = Changes::new();
        // the rows to ask the engine about: those a change touched, and new input rows
        let mut ask: Vec<Row> = affected;
        let mut seen: rustc_hash::FxHashSet<Row> = ask.iter().cloned().collect();
        for (r, _) in &dl {
            if !self.rows.contains_key(r) && seen.insert(r.clone()) {
                ask.push(r.clone());
            }
        }
        let answers = self.answers(p, &ask)?;
        let mut held: FxHashMap<Row, bool> = ask.into_iter().zip(answers).collect();
        // rows whose answer changed, with their weight unchanged so far
        for (r, h) in &held {
            if let Some((w, old)) = self.rows.get_mut(r)
                && *old != *h
            {
                out.push((r.clone(), if *h { *w } else { -*w }));
                *old = *h;
            }
        }
        // the input's own changes
        for (r, dw) in dl {
            let (ow, oh) = self.rows.get(&r).copied().unwrap_or((0, false));
            let nw = ow + dw;
            let nh = held.remove(&r).unwrap_or(oh);
            let before = if oh { ow } else { 0 };
            let after = if nh { nw } else { 0 };
            if after != before {
                out.push((r.clone(), after - before));
            }
            if nw == 0 {
                self.rows.remove(&r);
            } else {
                self.rows.insert(r, (nw, nh));
            }
        }
        Ok(consolidate(out))
    }

    pub fn init(&mut self, p: &mut Pass) -> Result<Changes> {
        self.rows.clear();
        let c = consolidate(self.inner.init(p)?);
        self.apply(p, c, Vec::new())
    }

    pub fn delta(&mut self, p: &mut Pass) -> Result<Changes> {
        let dl = consolidate(self.inner.delta(p)?);
        let affected = self.affected(p)?;
        if dl.is_empty() && affected.is_empty() {
            return Ok(dl);
        }
        self.apply(p, dl, affected)
    }

    /// [`init`](Self::init) with annotations. The engine does not report why the
    /// condition holds, so every derivation's atoms are unknown.
    pub fn init_a(&mut self, p: &mut Pass) -> Result<AChanges> {
        let mode = p.prov;
        self.rows.clear();
        let c = prov::consolidate(self.inner.init_a(p)?);
        let plain: Changes = c
            .into_iter()
            .filter(|(_, w, _)| *w != 0)
            .map(|(r, w, _)| (r, w))
            .collect();
        Ok(self
            .apply(p, plain, Vec::new())?
            .into_iter()
            .map(|(r, w)| (r, w, Ann::unknown(mode, w)))
            .collect())
    }

    pub fn delta_a(&mut self, p: &mut Pass) -> Result<AChanges> {
        let mode = p.prov;
        let c = prov::consolidate(self.inner.delta_a(p)?);
        let dl: Changes = c
            .into_iter()
            .filter(|(_, w, _)| *w != 0)
            .map(|(r, w, _)| (r, w))
            .collect();
        let affected = self.affected(p)?;
        if dl.is_empty() && affected.is_empty() {
            return Ok(Vec::new());
        }
        Ok(self
            .apply(p, dl, affected)?
            .into_iter()
            .map(|(r, w)| (r, w, Ann::unknown(mode, w)))
            .collect())
    }

    pub fn bytes(&self) -> u64 {
        self.rows.len() as u64 * (row_bytes(self.inner.schema.len()) + 16)
    }

    pub fn save(&self, w: &mut Writer) -> Result<()> {
        w.u64(self.rows.len() as u64);
        for (r, (n, h)) in &self.rows {
            w.row(r)?;
            w.i64(*n);
            w.u64(u64::from(*h));
        }
        Ok(())
    }

    pub fn load(&mut self, r: &mut Reader) -> Result<()> {
        self.rows.clear();
        for _ in 0..r.u64()? {
            let row = r.row()?;
            let n = r.i64()?;
            let h = r.u64()? != 0;
            self.rows.insert(row, (n, h));
        }
        self.resolved = None;
        Ok(())
    }
}
