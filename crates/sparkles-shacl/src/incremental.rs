//! Incremental write-time validation (C10 §6.2): which focus nodes a write can affect.
//!
//! Evaluating a shape at a focus node `f` reads edges of the data graph: the edges of
//! its path, `rdf:type` edges for `sh:class` and class targets, the edges `sh:equals`
//! and its siblings compare with, every outgoing edge of the value nodes of a closed
//! shape, and the same again for every shape it refers to, at that shape's focus. Each
//! such read is a *dependency* `(π, p, dir)`: the edges with predicate `p` (or any, for
//! `*`) whose subject (`out`) or object (`in`) is a node `π` reaches from `f`.
//!
//! A changed triple `(s, p, o)` can change the result at `f` only if some dependency
//! reads it, that is if `s` (or `o`) is in `π(f)` for a dependency on `p`. The nodes `f`
//! with `x ∈ π(f)` are those the reversed path `^π` reaches from `x`. They are computed
//! in the state before the write and in the state after it, because the first changed
//! edge on the way from `f` is reached over unchanged edges, so it is reached in both.
//! Validating the shape on those nodes (those its targets select, in each state) finds
//! every result the write can add or remove; the results at every other focus node are
//! the same before and after.
//!
//! A SHACL-SPARQL constraint or component reads what its query's patterns match from the
//! focus node, when every pattern is anchored there ([`crate::localize`]). A
//! `sh:targetWhere` target reads what conformance to its shape reads at the focus node,
//! and every edge of the node when the shape does not narrow the nodes that may conform
//! (membership in the data graph's nodes then decides).
//!
//! Shapes this cannot localize are validated in full: shapes with SHACL-SPARQL
//! constraints or components whose queries may read anything, and shapes that reach a
//! reference cycle (recursive shapes).

mod affected;
mod analysis_control;
pub(crate) mod bounded;
mod targets;
pub use affected::{AffectedLimits, check_target_producers, validate_affected};

use crate::data::DataGraph;
use crate::nodeexpr::{Expr, ExprId};
use crate::path::{CPath, PropertyPath};
use crate::shapes::{Candidates, Constraint, ShapeId, Shapes, Target};
use crate::vocab::{rdf, rdfs};
use oxrdf::{NamedNode, Term};
use rustc_hash::{FxHashMap, FxHashSet};
use sparkles_core::id::Id;
use sparkles_core::store::Snapshot;

/// Why a write, or a shape, is validated in full (the `reason` label of
/// `sparkles_validation_fallbacks_total`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Fallback {
    /// the validation state of the head is unknown, or (strict `reject`) not conforming
    Baseline,
    /// the shapes changed
    Shapes,
    /// `rdfs:subClassOf` changed while a shape reads classes, and the classes it changes
    /// have too many instances
    Subclass,
    /// a shape has a SHACL-SPARQL constraint or component whose query is not anchored at
    /// the focus node, or a node expression that reads the graph away from it
    Sparql,
    /// a shape reaches a reference cycle
    Recursive,
    /// a bulk write, whose new generation renumbers terms
    Bulk,
    /// too many affected focus nodes, or a reverse traversal visited too many nodes
    Budget,
}

impl Fallback {
    pub const ALL: [Fallback; 7] = [
        Fallback::Baseline,
        Fallback::Shapes,
        Fallback::Subclass,
        Fallback::Sparql,
        Fallback::Recursive,
        Fallback::Bulk,
        Fallback::Budget,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Fallback::Baseline => "baseline",
            Fallback::Shapes => "shapes",
            Fallback::Subclass => "subclass",
            Fallback::Sparql => "sparql",
            Fallback::Recursive => "recursive",
            Fallback::Bulk => "bulk",
            Fallback::Budget => "budget",
        }
    }
}

/// Limits past which a write is validated in full (C10 §6.2.3 F7).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Tuning {
    /// affected focus nodes over all shapes
    pub max_focus: usize,
    /// affected focus nodes of one shape, as a share of its focus nodes at the last full
    /// validation …
    pub max_share: f64,
    /// … once there are at least this many
    pub share_floor: usize,
    /// nodes visited by the reverse traversals of one write
    pub max_visit: usize,
}

impl Default for Tuning {
    fn default() -> Self {
        Tuning {
            max_focus: 50_000,
            max_share: 0.25,
            share_floor: 512,
            max_visit: 100_000,
        }
    }
}

/// Which side of a changed edge a dependency reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Dir {
    Out,
    In,
}

/// One read of a shape's evaluation, relative to its focus node.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct Dep {
    /// the path from the focus node to the node whose edges are read (a sequence)
    prefix: Vec<PropertyPath>,
    /// `None`: every predicate
    pred: Option<NamedNode>,
    dir: Dir,
}

impl From<crate::localize::Read> for Dep {
    fn from(r: crate::localize::Read) -> Dep {
        Dep {
            prefix: r.prefix,
            pred: r.pred,
            dir: if r.out { Dir::Out } else { Dir::In },
        }
    }
}

/// Predicates a group of dependencies reads.
#[derive(Clone, Debug, Default)]
struct Preds {
    any: bool,
    set: Vec<NamedNode>,
}

/// The dependencies of a shape that share a prefix.
#[derive(Clone, Debug)]
struct Group {
    prefix: Vec<PropertyPath>,
    out: Preds,
    inn: Preds,
}

/// How a shape with targets is validated incrementally.
#[derive(Clone, Debug)]
enum Plan {
    Local(Vec<Group>),
    Global(Fallback),
}

/// The incremental analysis of a shapes graph.
#[derive(Clone, Debug, Default)]
pub struct Model {
    /// per shape: `None` for shapes that are not validated on their own (no targets, or
    /// deactivated)
    plans: Vec<Option<Plan>>,
    /// every predicate a validation can read; `None`: any
    reads: Option<FxHashSet<NamedNode>>,
    /// some localized shape reads `rdf:type` with the subclass closure
    uses_classes: bool,
}

/// Dependencies of one shape past which it is validated in full (a shapes graph whose
/// references fan out that far is better served by a full validation).
const MAX_DEPS: usize = 4096;

/// The most steps the analysis of one shape's expressions may take. A shared expression
/// is walked once per path that reaches it, so a graph that doubles at every level would
/// otherwise take exponential time while the guard is installed.
const MAX_EXPR_STEPS: usize = 100_000;

struct Analyzer<'a, 'c> {
    shapes: &'a Shapes,
    control: Option<&'c mut dyn FnMut() -> Result<(), Fallback>>,
    memo: Vec<Option<Result<Vec<Dep>, Fallback>>>,
    on_stack: Vec<bool>,
    uses_classes: bool,
    /// steps of `expr_deps` taken for the shape being analyzed
    expr_steps: usize,
}

impl Analyzer<'_, '_> {
    fn check(&mut self) -> Result<(), Fallback> {
        match self.control.as_mut() {
            Some(check) => check(),
            None => Ok(()),
        }
    }

    fn pred(&mut self, t: crate::shapes::Tid) -> Result<Option<NamedNode>, Fallback> {
        match self.shapes.term(t) {
            Term::NamedNode(n) => {
                analysis_control::admit_iri(n, &mut self.control)?;
                Ok(Some(n.clone()))
            }
            _ => Ok(None),
        }
    }

    /// The reads of shape `si` at its focus node.
    fn deps(&mut self, si: ShapeId) -> Result<Vec<Dep>, Fallback> {
        self.check()?;
        if let Some(d) = &self.memo[si] {
            return analysis_control::result(d, &mut self.control);
        }
        if self.on_stack[si] {
            return Err(Fallback::Recursive);
        }
        self.on_stack[si] = true;
        // the walk of this shape must not refresh the budget of the walk that reached it
        let saved = std::mem::replace(&mut self.expr_steps, 0);
        let r = self.compute(si);
        self.expr_steps = saved;
        self.on_stack[si] = false;
        self.memo[si] = Some(analysis_control::result(&r, &mut self.control));
        r
    }

    fn compute(&mut self, si: ShapeId) -> Result<Vec<Dep>, Fallback> {
        let shapes = self.shapes;
        let shape = &shapes.shapes[si];
        self.expr_steps = 0;
        let mut out: Vec<Dep> = Vec::new();
        if shape.deactivated {
            // a deactivated shape conforms without reading anything
            return Ok(out);
        }
        // the value nodes are reached over the path
        let mut path = Vec::new();
        if let Some(p) = &shape.path {
            path.push(analysis_control::nnf(p, false, &mut self.control)?);
        }
        for p in &path {
            for (prefix, pred, dir) in analysis_control::steps(p, &mut self.control)? {
                out.push(Dep {
                    prefix,
                    pred: Some(pred),
                    dir,
                });
            }
        }
        // the value nodes may include the nodes `sh:values` and `sh:defaultValue` compute;
        // `None`: some of them are not reached from the focus node
        let mut anchored_values = true;
        let mut value_prefix = analysis_control::prefix(&path, &mut self.control)?;
        if let Some(e) = shape.deactivated_expr {
            self.expr_deps(e, &Some(Vec::new()), &None, &mut out)?;
        }
        if shape.values.is_some() || shape.default_value.is_some() {
            let mut reaches = vec![Some(analysis_control::prefix(&path, &mut self.control)?)];
            for e in [shape.values, shape.default_value].into_iter().flatten() {
                let value_path = Some(analysis_control::prefix(&path, &mut self.control)?);
                reaches.push(self.expr_deps(e, &Some(Vec::new()), &value_path, &mut out)?);
            }
            match alt(reaches) {
                Some(p) => value_prefix = p,
                None => anchored_values = false,
            }
        }
        let path_for_values = value_prefix;
        // where an expression finds the variable `value`: the value nodes, when each is
        // reached over the path
        let value_pre: Pre = if anchored_values {
            Some(analysis_control::prefix(
                &path_for_values,
                &mut self.control,
            )?)
        } else {
            None
        };
        // only `sh:class` and `sh:closed` read the graph at the value nodes, and both
        // check `anchored_values` first
        let at_values = |pred: Option<NamedNode>, control: &mut _| {
            Ok::<_, Fallback>(Dep {
                prefix: analysis_control::prefix(&path_for_values, control)?,
                pred,
                dir: Dir::Out,
            })
        };
        let mut nested: Vec<ShapeId> = Vec::new();
        // shapes evaluated at the members of the value nodes (`sh:memberShape`)
        let mut members: Vec<ShapeId> = Vec::new();
        // a list constraint reads `rdf:first` and `rdf:rest` along each value node's list
        let along_lists = |control: &mut _| {
            let mut prefix = analysis_control::prefix(&path_for_values, control)?;
            prefix.push(PropertyPath::ZeroOrMore(Box::new(PropertyPath::Predicate(
                rdf::REST.into_owned(),
            ))));
            Ok::<_, Fallback>(prefix)
        };
        let mut lists = false;
        for c in &shape.constraints {
            self.check()?;
            match c {
                Constraint::Class(_) => {
                    if !anchored_values {
                        return Err(Fallback::Sparql);
                    }
                    self.uses_classes = true;
                    out.push(at_values(Some(rdf::TYPE.into_owned()), &mut self.control)?);
                }
                Constraint::Equals(p)
                | Constraint::Disjoint(p)
                | Constraint::LessThan(p)
                | Constraint::LessThanOrEquals(p) => {
                    if let Some(p) = self.pred(*p)? {
                        out.push(Dep {
                            prefix: Vec::new(),
                            pred: Some(p),
                            dir: Dir::Out,
                        });
                    }
                }
                Constraint::Closed { .. } => {
                    if !anchored_values {
                        return Err(Fallback::Sparql);
                    }
                    out.push(at_values(None, &mut self.control)?);
                }
                Constraint::Expression(e) => {
                    self.expr_deps(*e, &Some(Vec::new()), &value_pre, &mut out)?;
                }
                // the shapes an expression computes are known only for a constant
                Constraint::NodeByExpression(e) => match &self.shapes.exprs[*e] {
                    Expr::Const(t) => {
                        if let Some(&s) = self.shapes.by_node.get(self.shapes.term(*t)) {
                            nested.push(s);
                        }
                    }
                    _ => return Err(Fallback::Sparql),
                },
                Constraint::Not(s) | Constraint::Node(s) | Constraint::Property(s) => {
                    nested.push(*s)
                }
                Constraint::And(ss) | Constraint::Or(ss) | Constraint::Xone(ss) => {
                    nested.extend(ss)
                }
                Constraint::QualifiedMin(q) | Constraint::QualifiedMax(q) => {
                    nested.push(q.shape);
                    nested.extend(&q.siblings);
                }
                // a query anchored at the focus node reads its neighbourhood
                Constraint::Sparql(c) => {
                    if self.control.is_some() {
                        return Err(Fallback::Sparql);
                    }
                    let reads = crate::localize::reads(&c.parsed, &[("this", Vec::new())])
                        .ok_or(Fallback::Sparql)?;
                    out.extend(reads.into_iter().map(Dep::from));
                }
                Constraint::Component(c) => {
                    if self.control.is_some() {
                        return Err(Fallback::Sparql);
                    }
                    let mut anchors = vec![("this", Vec::new())];
                    if c.ask {
                        // `$value` is each value node, which a computed one is not
                        // reached over the path
                        if !anchored_values {
                            return Err(Fallback::Sparql);
                        }
                        anchors.push((
                            "value",
                            analysis_control::prefix(&path_for_values, &mut self.control)?,
                        ));
                    }
                    let reads =
                        crate::localize::reads(&c.parsed, &anchors).ok_or(Fallback::Sparql)?;
                    out.extend(reads.into_iter().map(Dep::from));
                }
                Constraint::MemberShape(s) => {
                    lists = true;
                    members.push(*s);
                }
                Constraint::MinListLength(_)
                | Constraint::MaxListLength(_)
                | Constraint::UniqueMembers(_) => lists = true,
                // these depend on the value nodes alone
                Constraint::Datatype(_)
                | Constraint::NodeKind(_)
                | Constraint::MinCount(_)
                | Constraint::MaxCount(_)
                | Constraint::MinExclusive(..)
                | Constraint::MinInclusive(..)
                | Constraint::MaxExclusive(..)
                | Constraint::MaxInclusive(..)
                | Constraint::MinLength(_)
                | Constraint::MaxLength(_)
                | Constraint::Pattern(_)
                | Constraint::LanguageIn(_)
                | Constraint::UniqueLang
                | Constraint::HasValue(_)
                | Constraint::In(_) => {}
            }
        }
        if lists {
            // the lists hang off each value node
            if !anchored_values {
                return Err(Fallback::Sparql);
            }
            for p in [rdf::FIRST, rdf::REST] {
                out.push(Dep {
                    prefix: along_lists(&mut self.control)?,
                    pred: Some(p.into_owned()),
                    dir: Dir::Out,
                });
            }
        }
        // a member shape is evaluated at each member of each value node
        for s in members {
            for d in self.deps(s)? {
                if !anchored_values {
                    return Err(Fallback::Sparql);
                }
                let mut prefix = along_lists(&mut self.control)?;
                prefix.push(PropertyPath::Predicate(rdf::FIRST.into_owned()));
                prefix.extend(d.prefix);
                out.push(Dep { prefix, ..d });
            }
            if out.len() > MAX_DEPS {
                return Err(Fallback::Budget);
            }
        }
        // a referenced shape is evaluated at each value node
        for s in nested {
            for d in self.deps(s)? {
                if !anchored_values {
                    return Err(Fallback::Sparql);
                }
                let mut prefix = analysis_control::prefix(&path_for_values, &mut self.control)?;
                prefix.extend(d.prefix);
                out.push(Dep { prefix, ..d });
            }
            if out.len() > MAX_DEPS {
                return Err(Fallback::Budget);
            }
        }
        if self.control.is_none() {
            let mut seen = FxHashSet::default();
            out.retain(|d| seen.insert(d.clone()));
            return Ok(out);
        }
        let mut seen = FxHashSet::default();
        let mut unique = Vec::new();
        for d in out {
            let copy = analysis_control::deps(std::slice::from_ref(&d), &mut self.control)?
                .pop()
                .unwrap();
            if seen.insert(copy) {
                unique.push(d);
            }
        }
        let out = unique;
        Ok(out)
    }
}

/// A prefix from the focus node to the nodes a part of a shape reaches; `None`: the
/// nodes are constants or computed, not reached from the focus node.
type Pre = Option<Vec<PropertyPath>>;

/// The union of several reaches (`None` when one of them is not anchored).
fn alt(rs: Vec<Pre>) -> Pre {
    let mut seqs: Vec<Vec<PropertyPath>> = Vec::new();
    for r in rs {
        let r = r?;
        if !seqs.contains(&r) {
            seqs.push(r);
        }
    }
    match seqs.len() {
        0 => None,
        1 => seqs.pop(),
        _ => Some(vec![PropertyPath::Alternative(
            seqs.into_iter().map(PropertyPath::Sequence).collect(),
        )]),
    }
}

impl Analyzer<'_, '_> {
    /// The reads of the node expression `e`, evaluated at the focus node that the prefix
    /// `base` reaches from the shape's focus node (`None`: an unanchored node), and the
    /// prefix that reaches its output nodes. `value` is the prefix of the nodes the
    /// variable `value` holds. An expression that reads the graph at an unanchored node,
    /// or finds nodes by what they are (instances, nodes matching a shape), is not
    /// localized.
    fn expr_deps(
        &mut self,
        e: ExprId,
        base: &Pre,
        value: &Pre,
        out: &mut Vec<Dep>,
    ) -> Result<Pre, Fallback> {
        self.check()?;
        let shapes = self.shapes;
        self.expr_steps += 1;
        if self.expr_steps > MAX_EXPR_STEPS {
            return Err(Fallback::Budget);
        }
        if out.len() > MAX_DEPS {
            // repeated reads of one path are one dependency
            if self.control.is_none() {
                let mut seen = FxHashSet::default();
                out.retain(|d| seen.insert(d.clone()));
            } else {
                let mut seen = FxHashSet::default();
                let mut unique = Vec::new();
                for d in out.drain(..) {
                    let copy = analysis_control::deps(std::slice::from_ref(&d), &mut self.control)?
                        .pop()
                        .unwrap();
                    if seen.insert(copy) {
                        unique.push(d);
                    }
                }
                *out = unique;
            }
            if out.len() > MAX_DEPS {
                return Err(Fallback::Budget);
            }
        }
        let anchored =
            |at: &Pre, control: &mut _| analysis_control::pre(at, control)?.ok_or(Fallback::Sparql);
        match &shapes.exprs[e] {
            Expr::Const(_) | Expr::Empty | Expr::List(_) => Ok(None),
            Expr::Var(n) => Ok(match n.as_str() {
                "focusNode" => analysis_control::pre(base, &mut self.control)?,
                "value" => analysis_control::pre(value, &mut self.control)?,
                _ => None,
            }),
            Expr::PathValues { path, focus } => {
                let start = match focus {
                    None => analysis_control::pre(base, &mut self.control)?,
                    Some(f) => self.expr_deps(*f, base, value, out)?,
                };
                let start = anchored(&start, &mut self.control)?;
                let p = analysis_control::nnf(path, false, &mut self.control)?;
                for (prefix, pred, dir) in analysis_control::steps(&p, &mut self.control)? {
                    let mut full = analysis_control::prefix(&start, &mut self.control)?;
                    full.extend(prefix);
                    out.push(Dep {
                        prefix: full,
                        pred: Some(pred),
                        dir,
                    });
                }
                let mut reach = start;
                reach.push(p);
                Ok(Some(reach))
            }
            Expr::Exists(x) | Expr::Count(x) | Expr::Min(x) | Expr::Max(x) | Expr::Sum(x) => {
                self.expr_deps(*x, base, value, out)?;
                Ok(None)
            }
            Expr::Distinct(x) | Expr::Limit { nodes: x, .. } | Expr::Offset { nodes: x, .. } => {
                self.expr_deps(*x, base, value, out)
            }
            Expr::If { cond, then, els } => {
                self.expr_deps(*cond, base, value, out)?;
                let mut rs = Vec::new();
                for b in [then, els].into_iter().flatten() {
                    rs.push(self.expr_deps(*b, base, value, out)?);
                }
                Ok(alt(rs))
            }
            Expr::Intersection(xs) | Expr::Concat(xs) => {
                let mut rs = Vec::new();
                for x in xs {
                    rs.push(self.expr_deps(*x, base, value, out)?);
                }
                Ok(alt(rs))
            }
            Expr::Remove { nodes, remove } => {
                self.expr_deps(*remove, base, value, out)?;
                self.expr_deps(*nodes, base, value, out)
            }
            Expr::FilterShape { nodes, shape } => {
                let r = self.expr_deps(*nodes, base, value, out)?;
                self.shape_at(*shape, &r, out)?;
                Ok(r)
            }
            Expr::OrderBy { nodes, by, .. } => {
                let r = self.expr_deps(*nodes, base, value, out)?;
                self.expr_deps(*by, &r, value, out)?;
                Ok(r)
            }
            Expr::FlatMap { nodes, map } => {
                let r = match nodes {
                    Some(n) => self.expr_deps(*n, base, value, out)?,
                    None => analysis_control::pre(base, &mut self.control)?,
                };
                self.expr_deps(*map, &r, value, out)
            }
            Expr::FindFirst { nodes, shape } | Expr::MatchAll { nodes, shape } => {
                let r = match nodes {
                    Some(n) => self.expr_deps(*n, base, value, out)?,
                    None => analysis_control::pre(base, &mut self.control)?,
                };
                self.shape_at(*shape, &r, out)?;
                Ok(if matches!(shapes.exprs[e], Expr::FindFirst { .. }) {
                    r
                } else {
                    None
                })
            }
            Expr::ConformsToShape { node, shape } => {
                let r = self.expr_deps(*node, base, value, out)?;
                match &shapes.exprs[*shape] {
                    Expr::Const(t) => {
                        if let Some(&s) = shapes.by_node.get(shapes.term(*t)) {
                            self.shape_at(s, &r, out)?;
                        }
                        Ok(None)
                    }
                    _ => Err(Fallback::Sparql),
                }
            }
            Expr::SparqlFn { args, .. } => {
                for a in args {
                    self.expr_deps(*a, base, value, out)?;
                }
                Ok(None)
            }
            Expr::Select(q) | Expr::SparqlExpr(q) => {
                if self.control.is_some() {
                    return Err(Fallback::Sparql);
                }
                let at = anchored(base, &mut self.control)?;
                let reads =
                    crate::localize::reads(&q.query, &[("this", at)]).ok_or(Fallback::Sparql)?;
                out.extend(reads.into_iter().map(Dep::from));
                Ok(None)
            }
            Expr::InstancesOf(_) | Expr::NodesMatching(_) | Expr::Custom { .. } | Expr::Arg(_) => {
                Err(Fallback::Sparql)
            }
        }
    }

    /// The reads of shape `si` evaluated at the nodes `at` reaches.
    fn shape_at(&mut self, si: ShapeId, at: &Pre, out: &mut Vec<Dep>) -> Result<(), Fallback> {
        let deps = self.deps(si)?;
        if deps.is_empty() {
            return Ok(());
        }
        let at = analysis_control::pre(at, &mut self.control)?.ok_or(Fallback::Sparql)?;
        for d in deps {
            let mut prefix = analysis_control::prefix(&at, &mut self.control)?;
            prefix.extend(d.prefix);
            out.push(Dep { prefix, ..d });
        }
        Ok(())
    }
}

/// Push inverses down to the predicates (`^(a/b)` is `^b/^a`).
pub(crate) fn nnf(p: &PropertyPath, inv: bool) -> PropertyPath {
    use PropertyPath as P;
    let b = |x: &PropertyPath| Box::new(nnf(x, inv));
    match p {
        P::Predicate(n) if inv => P::Inverse(Box::new(P::Predicate(n.clone()))),
        P::Predicate(n) => P::Predicate(n.clone()),
        P::Inverse(x) => nnf(x, !inv),
        P::Sequence(xs) => {
            let mut v: Vec<PropertyPath> = xs.iter().map(|x| nnf(x, inv)).collect();
            if inv {
                v.reverse();
            }
            P::Sequence(v)
        }
        P::Alternative(xs) => P::Alternative(xs.iter().map(|x| nnf(x, inv)).collect()),
        P::ZeroOrMore(x) => P::ZeroOrMore(b(x)),
        P::OneOrMore(x) => P::OneOrMore(b(x)),
        P::ZeroOrOne(x) => P::ZeroOrOne(b(x)),
    }
}

/// The edges evaluating a path (in negation normal form) reads: `(prefix, p, dir)` for
/// each predicate step, where `prefix` reaches the node whose `p` edges it follows.
pub(crate) fn steps(p: &PropertyPath) -> Vec<(Vec<PropertyPath>, NamedNode, Dir)> {
    use PropertyPath as P;
    match p {
        P::Predicate(n) => vec![(Vec::new(), n.clone(), Dir::Out)],
        P::Inverse(x) => match x.as_ref() {
            P::Predicate(n) => vec![(Vec::new(), n.clone(), Dir::In)],
            // not in negation normal form
            other => steps(&nnf(other, true)),
        },
        P::Sequence(xs) => {
            let mut out = Vec::new();
            for (i, x) in xs.iter().enumerate() {
                for (pre, n, d) in steps(x) {
                    let mut prefix = xs[..i].to_vec();
                    prefix.extend(pre);
                    out.push((prefix, n, d));
                }
            }
            out
        }
        P::Alternative(xs) => xs.iter().flat_map(steps).collect(),
        // a closure follows its steps at every node it reaches, the start included
        P::ZeroOrMore(x) | P::OneOrMore(x) => steps(x)
            .into_iter()
            .map(|(pre, n, d)| {
                let mut prefix = vec![P::ZeroOrMore(x.clone())];
                prefix.extend(pre);
                (prefix, n, d)
            })
            .collect(),
        P::ZeroOrOne(x) => steps(x),
    }
}

impl Model {
    /// Analyze the shapes with targets.
    pub fn new(shapes: &Shapes) -> Model {
        Self::new_controlled(shapes, None)
    }

    fn new_controlled(
        shapes: &Shapes,
        mut control: Option<&mut dyn FnMut() -> Result<(), Fallback>>,
    ) -> Model {
        let n = shapes.shapes.len();
        if let Some(check) = &mut control {
            for _ in 0..n.saturating_mul(3) {
                if check().is_err() {
                    return Model::default();
                }
            }
        }
        let mut a = Analyzer {
            shapes,
            control,
            memo: vec![None; n],
            on_stack: vec![false; n],
            uses_classes: false,
            expr_steps: 0,
        };
        let mut plans = vec![None; n];
        let mut reads: Option<FxHashSet<NamedNode>> = Some(FxHashSet::default());
        for (si, shape) in shapes.shapes.iter().enumerate() {
            if a.check().is_err() {
                plans[si] = Some(Plan::Global(Fallback::Budget));
                continue;
            }
            if shape.targets.is_empty() || shape.deactivated {
                continue;
            }
            let plan = match a.deps(si) {
                Ok(mut deps) => {
                    // the targets read the focus node's own edges
                    let mut global = None;
                    for t in &shape.targets {
                        if let Err(f) = a.check() {
                            global = Some(f);
                            break;
                        }
                        let (pred, dir) = match *t {
                            Target::Node(_) => continue,
                            // a target expression that reads the data graph is not
                            // localized; one that computes constants reads nothing
                            Target::Expr(e) => {
                                a.expr_steps = 0;
                                if let Err(f) = a.expr_deps(e, &None, &None, &mut Vec::new()) {
                                    global = Some(f);
                                    break;
                                }
                                continue;
                            }
                            // membership reads what conformance to the shape reads, and
                            // being a node of the data graph reads every edge of the node
                            Target::Where(w) => {
                                match a.deps(w) {
                                    Ok(d) => deps.extend(d),
                                    Err(f) => {
                                        global = Some(f);
                                        break;
                                    }
                                }
                                if matches!(
                                    shapes.candidates(w),
                                    Candidates::All | Candidates::Terms(_)
                                ) {
                                    for dir in [Dir::Out, Dir::In] {
                                        deps.push(Dep {
                                            prefix: Vec::new(),
                                            pred: None,
                                            dir,
                                        });
                                    }
                                }
                                continue;
                            }
                            Target::Class(_) => {
                                a.uses_classes = true;
                                (Some(rdf::TYPE.into_owned()), Dir::Out)
                            }
                            Target::SubjectsOf(p) | Target::ObjectsOf(p) => {
                                let pred = match a.pred(p) {
                                    Ok(p) => p,
                                    Err(f) => {
                                        global = Some(f);
                                        break;
                                    }
                                };
                                (
                                    pred,
                                    if matches!(t, Target::SubjectsOf(_)) {
                                        Dir::Out
                                    } else {
                                        Dir::In
                                    },
                                )
                            }
                        };
                        // a target predicate that is not an IRI selects nothing
                        if let Some(pred) = pred {
                            deps.push(Dep {
                                prefix: Vec::new(),
                                pred: Some(pred),
                                dir,
                            });
                        }
                    }
                    match global {
                        Some(f) => {
                            reads = None;
                            Plan::Global(f)
                        }
                        None => {
                            let admission = (|| {
                                for d in &deps {
                                    analysis_control::admit_prefix(&d.prefix, &mut a.control)?;
                                    analysis_control::admit_prefix(&d.prefix, &mut a.control)?;
                                    a.check()?;
                                    if let Some(p) = &d.pred {
                                        analysis_control::admit_iri(p, &mut a.control)?;
                                    }
                                }
                                Ok::<_, Fallback>(())
                            })();
                            match admission {
                                Ok(()) => Plan::Local(group(deps, &mut reads)),
                                Err(f) => Plan::Global(f),
                            }
                        }
                    }
                }
                Err(f) => {
                    reads = None;
                    Plan::Global(f)
                }
            };
            plans[si] = Some(plan);
        }
        if a.uses_classes
            && let Some(r) = &mut reads
        {
            r.insert(rdfs::SUB_CLASS_OF.into_owned());
        }
        Model {
            plans,
            reads,
            uses_classes: a.uses_classes,
        }
    }

    /// The shapes validated in full whatever the write, and why.
    pub fn global(&self) -> impl Iterator<Item = (ShapeId, Fallback)> + '_ {
        self.plans.iter().enumerate().filter_map(|(si, p)| match p {
            Some(Plan::Global(f)) => Some((si, *f)),
            _ => None,
        })
    }

    /// Whether shape `si` is validated on its own (it has targets and is active).
    pub fn validated(&self, si: ShapeId) -> bool {
        self.plans.get(si).is_some_and(Option::is_some)
    }

    /// Some localized shape reads classes: a change to `rdfs:subClassOf` then affects
    /// the instances of the classes whose subclasses it changes.
    pub fn uses_classes(&self) -> bool {
        self.uses_classes
    }

    /// Whether a change with one of these predicates can change a validation.
    pub fn reads_any(&self, view: &Snapshot, preds: &FxHashSet<Id>) -> bool {
        match &self.reads {
            None => true,
            Some(r) => r.iter().any(|p| {
                view.lookup_iri(p.as_str())
                    .is_some_and(|id| preds.contains(&id))
            }),
        }
    }

    /// The focus nodes each localized shape must be validated on after the changed
    /// triples `changes` (`(s, p, o)`, ids of `view`): those whose result can differ
    /// between `base` and `view`. `None` for the shapes validated in full and those not
    /// validated on their own. Ids are those of `view`.
    pub(crate) fn affected(
        &self,
        view: &Snapshot,
        states: [Option<&DataGraph>; 2],
        changes: &[[Id; 3]],
        tuning: &Tuning,
        last_focus: &[Option<usize>],
    ) -> anyhow::Result<Result<Vec<Option<Vec<Id>>>, Fallback>> {
        let resolve = |p: &Preds| -> (bool, FxHashSet<Id>) {
            (
                p.any,
                p.set
                    .iter()
                    .filter_map(|n| view.lookup_iri(n.as_str()))
                    .collect(),
            )
        };
        let mut visited = 0usize;
        let mut total = 0usize;
        let mut cache: FxHashMap<&[PropertyPath], CPath> = FxHashMap::default();
        let mut out = Vec::with_capacity(self.plans.len());
        for (si, plan) in self.plans.iter().enumerate() {
            let Some(Plan::Local(groups)) = plan else {
                out.push(None);
                continue;
            };
            let mut nodes: FxHashSet<Id> = FxHashSet::default();
            for g in groups {
                let (out_any, out_set) = resolve(&g.out);
                let (in_any, in_set) = resolve(&g.inn);
                let mut xs: FxHashSet<Id> = FxHashSet::default();
                for &[s, p, o] in changes {
                    if out_any || out_set.contains(&p) {
                        xs.insert(s);
                    }
                    if in_any || in_set.contains(&p) {
                        xs.insert(o);
                    }
                }
                if xs.is_empty() {
                    continue;
                }
                if g.prefix.is_empty() {
                    nodes.extend(xs);
                    continue;
                }
                let rev = cache.entry(&g.prefix).or_insert_with(|| {
                    let seq = PropertyPath::Sequence(g.prefix.clone());
                    CPath::Inverse(Box::new(CPath::compile(&seq, &mut |n| {
                        view.lookup_iri(n.as_str())
                            .unwrap_or(Id::local(u64::MAX >> 8))
                    })))
                });
                for x in xs {
                    for data in states.iter().flatten() {
                        let fs = rev.eval(data, x)?;
                        visited += fs.len();
                        if visited > tuning.max_visit {
                            return Ok(Err(Fallback::Budget));
                        }
                        nodes.extend(fs);
                    }
                }
            }
            total += nodes.len();
            let share_limit = last_focus
                .get(si)
                .copied()
                .flatten()
                .map(|n| (n as f64 * tuning.max_share) as usize);
            if total > tuning.max_focus
                || (nodes.len() > tuning.share_floor
                    && share_limit.is_some_and(|l| nodes.len() > l))
            {
                return Ok(Err(Fallback::Budget));
            }
            let mut nodes: Vec<Id> = nodes.into_iter().collect();
            nodes.sort_unstable();
            out.push(Some(nodes));
        }
        Ok(Ok(out))
    }
}

/// Group dependencies by prefix, and add their predicates to `reads`.
fn group(deps: Vec<Dep>, reads: &mut Option<FxHashSet<NamedNode>>) -> Vec<Group> {
    let mut groups: Vec<Group> = Vec::new();
    let mut index: FxHashMap<Vec<PropertyPath>, usize> = FxHashMap::default();
    for d in deps {
        let i = *index.entry(d.prefix.clone()).or_insert_with(|| {
            groups.push(Group {
                prefix: d.prefix.clone(),
                out: Preds::default(),
                inn: Preds::default(),
            });
            groups.len() - 1
        });
        let g = &mut groups[i];
        let preds = match d.dir {
            Dir::Out => &mut g.out,
            Dir::In => &mut g.inn,
        };
        match d.pred {
            None => {
                preds.any = true;
                *reads = None;
            }
            Some(p) => {
                if let Some(r) = reads {
                    r.insert(p.clone());
                }
                if !preds.set.contains(&p) {
                    preds.set.push(p);
                }
            }
        }
    }
    groups
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::RdfFormat;

    fn model(ttl: &str) -> (Shapes, Model) {
        let s = Shapes::parse(
            &format!(
                "@prefix sh: <http://www.w3.org/ns/shacl#> . @prefix ex: <http://ex.org/> . {ttl}"
            ),
            RdfFormat::Turtle,
            None,
        )
        .unwrap();
        let m = Model::new(&s);
        (s, m)
    }

    fn plan_of<'m>(s: &Shapes, m: &'m Model, iri: &str) -> &'m Plan {
        let node = Term::NamedNode(NamedNode::new(iri).unwrap());
        let si = s.shapes.iter().position(|x| x.node == node).unwrap();
        m.plans[si].as_ref().unwrap()
    }

    #[test]
    fn reads_of_paths_and_references() {
        let (s, m) = model(
            "ex:S a sh:NodeShape ; sh:targetClass ex:C ;
               sh:property [ sh:path ( ex:a [ sh:inversePath ex:b ] ) ; sh:class ex:D ] ;
               sh:property [ sh:path [ sh:zeroOrMorePath ex:c ] ; sh:node ex:T ] .
             ex:T a sh:NodeShape ; sh:property [ sh:path ex:d ; sh:minCount 1 ] .",
        );
        let reads: Vec<String> = {
            let mut v: Vec<String> = m
                .reads
                .as_ref()
                .unwrap()
                .iter()
                .map(|n| n.as_str().rsplit(['/', '#']).next().unwrap().to_string())
                .collect();
            v.sort();
            v
        };
        assert_eq!(reads, ["a", "b", "c", "d", "subClassOf", "type"]);
        assert!(m.uses_classes());
        let Plan::Local(groups) = plan_of(&s, &m, "http://ex.org/S") else {
            panic!("local")
        };
        let prefixes: FxHashSet<String> = groups
            .iter()
            .map(|g| {
                g.prefix
                    .iter()
                    .map(|p| p.to_sparql())
                    .collect::<Vec<_>>()
                    .join(" / ")
            })
            .collect();
        // ε (a, c, rdf:type of the target), a (^b), a/^b (rdf:type of sh:class),
        // c* (c, d of ex:T at each value)
        for p in [
            "",
            "<http://ex.org/a>",
            "<http://ex.org/a>/(^<http://ex.org/b>)",
            "<http://ex.org/c>*",
        ] {
            assert!(prefixes.contains(p), "{p} in {prefixes:?}");
        }
    }

    #[test]
    fn sparql_recursion_and_closed_shapes() {
        let (s, m) = model(
            "ex:R a sh:NodeShape ; sh:targetNode ex:x ; sh:property [ sh:path ex:p ; sh:node ex:R ] .
             ex:Q a sh:NodeShape ; sh:targetNode ex:y ;
               sh:sparql [ sh:select \"SELECT $this WHERE { ?x <http://ex.org/p> ?y }\" ] .
             ex:L a sh:NodeShape ; sh:targetNode ex:y ;
               sh:sparql [ sh:select \"SELECT $this WHERE { $this <http://ex.org/k> ?k . ?o <http://ex.org/k> ?k }\" ] .
             ex:W a sh:NodeShape ;
               sh:targetWhere [ sh:property [ sh:path ex:age ; sh:minCount 1 ] ] .
             ex:Z a sh:NodeShape ; sh:targetNode ex:z ; sh:closed true .",
        );
        // an anchored query reads ex:k at the focus node and at its ex:k values
        let Plan::Local(groups) = plan_of(&s, &m, "http://ex.org/L") else {
            panic!("local")
        };
        assert_eq!(groups.len(), 2, "{groups:?}");
        // a where target reads what the where shape reads
        let Plan::Local(groups) = plan_of(&s, &m, "http://ex.org/W") else {
            panic!("local")
        };
        assert_eq!(groups[0].out.set.len(), 1, "{groups:?}");
        assert!(matches!(
            plan_of(&s, &m, "http://ex.org/R"),
            Plan::Global(Fallback::Recursive)
        ));
        assert!(matches!(
            plan_of(&s, &m, "http://ex.org/Q"),
            Plan::Global(Fallback::Sparql)
        ));
        assert!(matches!(plan_of(&s, &m, "http://ex.org/Z"), Plan::Local(_)));
        assert!(m.reads.is_none());
    }

    /// Each target expression below takes about 65,000 steps to walk, and the two of them
    /// together take more than the budget of one analysis. The budget is per expression, so
    /// the shape stays localized.
    #[test]
    fn each_target_expression_has_its_own_step_budget() {
        let mut ttl = String::from("@prefix shnex: <http://www.w3.org/ns/shacl-node-expr#> .\n");
        ttl.push_str("_:e1 shnex:concat ( ex:k ex:k ) .\n");
        for i in 2..=15 {
            ttl.push_str(&format!(
                "_:e{i} shnex:concat ( _:e{} _:e{} ) .\n",
                i - 1,
                i - 1
            ));
        }
        ttl.push_str(
            "ex:S a sh:NodeShape ; sh:property [ sh:path ex:p ; sh:minCount 1 ] ;
               sh:targetNode [ shnex:concat ( _:e15 ) ] ,
                             [ shnex:concat ( _:e15 ex:z ) ] .",
        );
        let (s, m) = model(&ttl);
        assert!(
            matches!(plan_of(&s, &m, "http://ex.org/S"), Plan::Local(_)),
            "{:?}",
            plan_of(&s, &m, "http://ex.org/S")
        );
    }

    #[test]
    fn inverse_of_a_sequence() {
        let p = PropertyPath::Inverse(Box::new(PropertyPath::Sequence(vec![
            PropertyPath::Predicate(NamedNode::new("http://ex.org/a").unwrap()),
            PropertyPath::Predicate(NamedNode::new("http://ex.org/b").unwrap()),
        ])));
        assert_eq!(
            nnf(&p, false).to_sparql(),
            "(^<http://ex.org/b>)/(^<http://ex.org/a>)"
        );
        let st = steps(&nnf(&p, false));
        assert_eq!(st.len(), 2);
        assert_eq!((st[0].0.len(), st[0].2), (0, Dir::In));
        assert_eq!((st[1].0.len(), st[1].2), (1, Dir::In));
    }
}
