//! Query planner: spargebra algebra → physical operator tree.
//!
//! Join groups (BGPs, joins, `GRAPH` blocks, filters) are flattened into items and
//! ordered by a QLever-style dynamic program over connected components that keeps the
//! cheapest plan per (subset, sort order) ("interesting orders"), switching to a greedy
//! strategy for large groups. Filters are placed as soon as all their variables are
//! bound. Other operators follow the algebra, with Jena-style rewrites (filter
//! placement through OPTIONAL / UNION / MINUS / BIND, filter-equality substitution,
//! top-k for ORDER BY + LIMIT).

use super::ctx::Ctx;
use super::expr::{Compiler, ExistsSpec, Expr};
use super::table::{Table, VarId};
use crate::error::{Error, Result};
use crate::id::{Id, Tag};
use crate::index::{G, O, P, Perm, S};
use oxrdf::vocab::xsd;
use oxrdf::{Literal, Term};
use parking_lot::Mutex;
use rustc_hash::{FxHashMap, FxHashSet};
use spargebra::algebra::{
    AggregateExpression, AggregateFunction, Expression, GraphPattern, OrderExpression,
    PropertyPathExpression,
};
use spargebra::term::{GroundTerm, NamedNodePattern, TermPattern, TriplePattern};
use std::sync::Arc;

/// The graph that triple patterns are matched against.
#[derive(Clone, Debug)]
pub enum ActiveGraph {
    Default,
    /// union of all named graphs (`GRAPH <urn:x-arq:UnionGraph>`)
    Union,
    Named(Id),
    Var(VarId),
}

#[derive(Clone, Debug)]
pub enum GraphFilter {
    /// no restriction (graph is an output column or the prefix)
    All,
    Default,
    /// any named graph (not the default graph)
    Named,
    One(u64),
    Set(Vec<u64>),
}

impl GraphFilter {
    #[inline]
    pub fn accepts(&self, g: u64) -> bool {
        match self {
            GraphFilter::All => true,
            GraphFilter::Default => g == Id::DEFAULT_GRAPH.0,
            GraphFilter::Named => g != Id::DEFAULT_GRAPH.0,
            GraphFilter::One(x) => g == *x,
            GraphFilter::Set(s) => s.binary_search(&g).is_ok(),
        }
    }
    fn multi(&self) -> bool {
        matches!(
            self,
            GraphFilter::All | GraphFilter::Named | GraphFilter::Set(_)
        )
    }
}

#[derive(Clone, Debug)]
pub struct ScanSpec {
    pub perm: Perm,
    pub prefix: Vec<u64>,
    /// key column → variable (first occurrence)
    pub cols: Vec<(usize, VarId)>,
    /// key columns that must be equal (repeated variables)
    pub eqs: Vec<(usize, usize)>,
    pub graph: GraphFilter,
    /// key column holding the graph
    pub graph_col: usize,
    /// drop adjacent duplicate rows (same triple in several graphs)
    pub dedup: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub enum JoinAlgo {
    Merge,
    Hash,
    Cross,
}

#[derive(Clone)]
pub struct Agg {
    pub func: AggregateFunction,
    pub expr: Option<Expr>,
    pub distinct: bool,
}

#[derive(Clone, Debug)]
pub enum PathEnd {
    Const(Id),
    Var(VarId),
}

#[derive(Clone)]
pub struct PathSpec {
    pub subj: PathEnd,
    pub obj: PathEnd,
    /// minimum path length (0 or 1)
    pub min: u8,
    /// maximum length 1 (`?` paths)
    pub max_one: bool,
    /// for index-based traversal: predicate + direction (true = reverse)
    pub simple: Option<(u64, bool)>,
    pub graph: GraphFilter,
    pub graph_var: Option<VarId>,
    /// edge variables of the child plan (src, dst) when not simple
    pub edge_vars: Option<(VarId, VarId)>,
}

// `Values` / `Scan` are much larger than the other variants; nodes are few, so this is fine.
#[allow(clippy::large_enum_variant)]
#[derive(Clone)]
pub enum Kind {
    Scan(ScanSpec),
    Values(Table),
    Empty,
    Join {
        algo: JoinAlgo,
        keys: Vec<VarId>,
    },
    LeftJoin {
        expr: Option<Expr>,
    },
    Minus,
    Union,
    Filter(Vec<Expr>),
    Extend(VarId, Expr),
    Sort(Vec<VarId>),
    OrderBy {
        keys: Vec<(Expr, bool)>,
        limit: Option<usize>,
    },
    Project(Vec<VarId>),
    Distinct,
    Slice {
        offset: usize,
        limit: Option<usize>,
    },
    Group {
        keys: Vec<VarId>,
        aggs: Vec<(VarId, Agg)>,
    },
    /// `COUNT(*)` over a single scan answered from index metadata
    /// `GROUP BY ?k` + `COUNT` over a single scan sorted on ?k: counts runs in the index
    /// blocks without materializing the scan (QLever `computeGroupByObjectWithCount`)
    GroupCountScan {
        spec: ScanSpec,
        key: VarId,
        counts: Vec<VarId>,
    },
    CountScan {
        spec: ScanSpec,
        var: VarId,
    },
    /// `COUNT(DISTINCT ?k)` over a single scan sorted on ?k: counts runs of equal ids
    CountDistinctScan {
        spec: ScanSpec,
        var: VarId,
    },
    /// transitive / optional path; child 0 = edges plan (unless simple);
    /// if `bound_from_left`, the last child is the input binding the start variable
    Path {
        spec: Box<PathSpec>,
        bound_from_left: bool,
    },
    Service {
        endpoint: PathEnd,
        query: String,
        silent: bool,
    },
}

#[derive(Clone)]
pub struct Node {
    pub kind: Kind,
    pub children: Vec<Node>,
    pub vars: Vec<VarId>,
    pub certain: Vec<VarId>,
    pub sorted: Vec<VarId>,
    pub est: f64,
    pub cost: f64,
    pub dist: FxHashMap<VarId, f64>,
    pub desc: String,
}

impl Node {
    fn leaf(kind: Kind, vars: Vec<VarId>, est: f64, desc: String) -> Node {
        let dist = vars.iter().map(|&v| (v, est.max(1.0))).collect();
        Node {
            kind,
            children: Vec::new(),
            certain: vars.clone(),
            vars,
            sorted: Vec::new(),
            est,
            cost: est,
            dist,
            desc,
        }
    }

    pub fn empty(vars: Vec<VarId>) -> Node {
        Node::leaf(Kind::Empty, vars, 0.0, "empty".into())
    }

    pub fn unit() -> Node {
        let mut n = Node::leaf(Kind::Values(Table::unit()), Vec::new(), 1.0, "unit".into());
        n.cost = 0.0;
        n
    }

    fn unary(kind: Kind, child: Node, desc: String) -> Node {
        Node {
            kind,
            vars: child.vars.clone(),
            certain: child.certain.clone(),
            sorted: child.sorted.clone(),
            est: child.est,
            cost: child.cost + child.est,
            dist: child.dist.clone(),
            desc,
            children: vec![child],
        }
    }

    pub fn is_empty(&self) -> bool {
        matches!(self.kind, Kind::Empty)
    }

    fn d(&self, v: VarId) -> f64 {
        self.dist
            .get(&v)
            .copied()
            .unwrap_or(self.est)
            .clamp(1.0, self.est.max(1.0))
    }

    pub fn operator(&self) -> &'static str {
        match &self.kind {
            Kind::Scan(_) => "IndexScan",
            Kind::Values(_) => "Values",
            Kind::Empty => "Empty",
            Kind::Join {
                algo: JoinAlgo::Merge,
                ..
            } => "MergeJoin",
            Kind::Join {
                algo: JoinAlgo::Hash,
                ..
            } => "HashJoin",
            Kind::Join {
                algo: JoinAlgo::Cross,
                ..
            } => "CartesianProduct",
            Kind::LeftJoin { .. } => "OptionalJoin",
            Kind::Minus => "Minus",
            Kind::Union => "Union",
            Kind::Filter(_) => "Filter",
            Kind::Extend(..) => "Bind",
            Kind::Sort(_) => "Sort",
            Kind::OrderBy { limit: Some(_), .. } => "TopK",
            Kind::OrderBy { .. } => "OrderBy",
            Kind::Project(_) => "Project",
            Kind::Distinct => "Distinct",
            Kind::Slice { .. } => "Limit",
            Kind::Group { .. } => "GroupBy",
            Kind::CountScan { .. } => "CountFromIndex",
            Kind::CountDistinctScan { .. } => "CountDistinctFromIndex",
            Kind::GroupCountScan { .. } => "GroupCountFromIndex",
            Kind::Path { .. } => "TransitivePath",
            Kind::Service { .. } => "Service",
        }
    }
}

/// A term in a triple pattern.
#[derive(Clone, Copy, Debug, PartialEq)]
enum PT {
    C(Id),
    V(VarId),
}

#[derive(Clone)]
struct Triple {
    t: [PT; 3],
    graph: ActiveGraph,
}

// a Node is much larger than a triple pattern, but items are short-lived and few
#[allow(clippy::large_enum_variant)]
enum Item {
    Triple(Triple),
    Path(PathItem),
    Node(Node),
}

#[derive(Clone)]
struct PathItem {
    s: PT,
    path: PropertyPathExpression,
    o: PT,
    graph: ActiveGraph,
}

const FILTER_SELECTIVITY: f64 = 0.3;
const DP_LIMIT: usize = 12;

pub struct Planner<'a> {
    pub ctx: &'a Ctx,
    pub subst: FxHashMap<VarId, Id>,
    bnode_scope: u32,
}

impl<'a> Planner<'a> {
    pub fn new(ctx: &'a Ctx) -> Planner<'a> {
        Planner {
            ctx,
            subst: FxHashMap::default(),
            bnode_scope: 0,
        }
    }

    pub fn compile(&self, e: &Expression, graph: &ActiveGraph) -> Expr {
        let ctx = self.ctx;
        let g = graph.clone();
        let exists = move |p: &GraphPattern| -> Arc<ExistsSpec> {
            let mut names = Vec::new();
            collect_pattern_vars(p, &mut names);
            let mut vars: Vec<VarId> = names.iter().map(|n| ctx.var(n)).collect();
            vars.sort_unstable();
            vars.dedup();
            Arc::new(ExistsSpec {
                pattern: p.clone(),
                graph: g.clone(),
                vars,
                memo: Mutex::new(FxHashMap::default()),
            })
        };
        Compiler {
            ctx,
            subst: &self.subst,
            exists: &exists,
        }
        .compile(e)
    }

    fn var(&self, name: &str) -> PT {
        let v = self.ctx.var(name);
        match self.subst.get(&v) {
            Some(id) => PT::C(*id),
            None => PT::V(v),
        }
    }

    fn term_pattern(&self, t: &TermPattern) -> PT {
        match t {
            TermPattern::Variable(v) => self.var(v.as_str()),
            TermPattern::BlankNode(b) => {
                // blank nodes in patterns are non-distinguished variables
                PT::V(
                    self.ctx
                        .var(&format!(" bn{}_{}", self.bnode_scope, b.as_str())),
                )
            }
            TermPattern::NamedNode(n) => PT::C(self.ctx.intern_term(&Term::NamedNode(n.clone()))),
            TermPattern::Literal(l) => PT::C(self.ctx.intern_term(&Term::Literal(l.clone()))),
            #[allow(unreachable_patterns)]
            _ => PT::C(Id::local(u64::MAX >> 5)),
        }
    }

    fn named_pattern(&self, n: &NamedNodePattern) -> PT {
        match n {
            NamedNodePattern::NamedNode(n) => {
                PT::C(self.ctx.intern_term(&Term::NamedNode(n.clone())))
            }
            NamedNodePattern::Variable(v) => self.var(v.as_str()),
        }
    }

    // ------------------------------------------------------------------ plan ------

    pub fn plan(&mut self, gp: &GraphPattern, g: &ActiveGraph, filters: Vec<Expr>) -> Result<Node> {
        self.ctx.check()?;
        use GraphPattern as GP;
        match gp {
            GP::Bgp { .. } | GP::Path { .. } | GP::Join { .. } | GP::Graph { .. } => {
                let mut items = Vec::new();
                self.collect(gp, g, &mut items)?;
                self.plan_group(items, filters)
            }
            GP::Filter { expr, inner } => {
                let mut fs = filters;
                fs.extend(self.compile(expr, g).conjuncts());
                self.plan(inner, g, fs)
            }
            GP::LeftJoin {
                left,
                right,
                expression,
            } => {
                let certain = certain_vars(left, self.ctx);
                let (push, top) = self.split_filters(filters, |v| certain.contains(v));
                let l = self.plan(left, g, push)?;
                let r = self.plan(right, g, Vec::new())?;
                let expr = expression.as_ref().map(|e| self.compile(e, g));
                let n = left_join(l, r, expr, self.ctx);
                Ok(self.apply_filters(n, top))
            }
            GP::Union { .. } => {
                let mut branches = Vec::new();
                flatten_union(gp, &mut branches);
                let mut children = Vec::new();
                for b in branches {
                    let c = self.plan(b, g, filters.clone())?;
                    if !c.is_empty() {
                        children.push(c);
                    }
                }
                Ok(union(children))
            }
            GP::Minus { left, right } => {
                let l = self.plan(left, g, filters)?;
                let r = self.plan(right, g, Vec::new())?;
                if l.is_empty() {
                    return Ok(l);
                }
                let shared: Vec<VarId> = l
                    .vars
                    .iter()
                    .filter(|v| r.vars.contains(v))
                    .copied()
                    .collect();
                if r.is_empty() || shared.is_empty() {
                    return Ok(l);
                }
                let mut n = Node::unary(Kind::Minus, l, "MINUS".into());
                n.cost += r.cost + r.est;
                n.children.push(r);
                Ok(n)
            }
            GP::Extend {
                inner,
                variable,
                expression,
            } => {
                let v = self.ctx.var(variable.as_str());
                let (push, top) = self.split_filters(filters, |x| *x != v);
                let child = self.plan(inner, g, push)?;
                let e = self.compile(expression, g);
                let desc = format!("?{} := {}", variable.as_str(), e.display(self.ctx));
                let mut n = Node::unary(Kind::Extend(v, e), child, desc);
                n.vars.push(v);
                n.dist.insert(v, n.est);
                Ok(self.apply_filters(n, top))
            }
            GP::Values {
                variables,
                bindings,
            } => {
                let vars: Vec<VarId> = variables.iter().map(|v| self.ctx.var(v.as_str())).collect();
                let mut t = Table::new(vars.clone());
                for row in bindings {
                    let ids: Vec<Id> = row
                        .iter()
                        .map(|t| match t {
                            None => Id::UNDEF,
                            Some(GroundTerm::NamedNode(n)) => {
                                self.ctx.intern_term(&Term::NamedNode(n.clone()))
                            }
                            Some(GroundTerm::Literal(l)) => {
                                self.ctx.intern_term(&Term::Literal(l.clone()))
                            }
                            #[allow(unreachable_patterns)]
                            Some(_) => Id::UNDEF,
                        })
                        .collect();
                    t.push_row(&ids);
                }
                // substitution: restrict to rows compatible with substituted values
                let certain: Vec<VarId> = vars
                    .iter()
                    .enumerate()
                    .filter(|(c, _)| t.cols[*c].iter().all(|id| !id.is_undef()))
                    .map(|(_, v)| *v)
                    .collect();
                let est = t.len() as f64;
                let mut n = Node::leaf(Kind::Values(t), vars, est, format!("{} rows", est));
                n.certain = certain;
                Ok(self.apply_filters(n, filters))
            }
            GP::OrderBy { inner, expression } => {
                let child = self.plan(inner, g, Vec::new())?;
                let keys = expression
                    .iter()
                    .map(|o| match o {
                        OrderExpression::Asc(e) => (self.compile(e, g), true),
                        OrderExpression::Desc(e) => (self.compile(e, g), false),
                    })
                    .collect::<Vec<_>>();
                let desc = keys
                    .iter()
                    .map(|(e, asc)| {
                        format!(
                            "{}({})",
                            if *asc { "ASC" } else { "DESC" },
                            e.display(self.ctx)
                        )
                    })
                    .collect::<Vec<_>>()
                    .join(" ");
                let mut n = Node::unary(Kind::OrderBy { keys, limit: None }, child, desc);
                n.sorted.clear();
                n.cost += n.est * n.est.max(2.0).log2() * 0.5;
                Ok(self.apply_filters(n, filters))
            }
            GP::Project { inner, variables } => {
                // sub-select: fresh scope for blank node labels
                self.bnode_scope += 1;
                let child = self.plan(inner, g, Vec::new())?;
                let vars: Vec<VarId> = variables.iter().map(|v| self.ctx.var(v.as_str())).collect();
                let n = project(child, vars, self.ctx);
                Ok(self.apply_filters(n, filters))
            }
            GP::Distinct { inner } => {
                let child = self.plan(inner, g, Vec::new())?;
                let mut n = Node::unary(Kind::Distinct, child, String::new());
                n.est = n.children[0].est; // unknown reduction
                Ok(self.apply_filters(n, filters))
            }
            GP::Reduced { inner } => {
                let child = self.plan(inner, g, Vec::new())?;
                Ok(self.apply_filters(child, filters))
            }
            GP::Slice {
                inner,
                start,
                length,
            } => {
                let child = self.plan(inner, g, Vec::new())?;
                let n = slice(child, *start, *length);
                Ok(self.apply_filters(n, filters))
            }
            GP::Group {
                inner,
                variables,
                aggregates,
            } => {
                let child = self.plan(inner, g, Vec::new())?;
                let keys: Vec<VarId> = variables.iter().map(|v| self.ctx.var(v.as_str())).collect();
                let aggs: Vec<(VarId, Agg)> = aggregates
                    .iter()
                    .map(|(v, a)| {
                        let agg = match a {
                            AggregateExpression::CountSolutions { distinct } => Agg {
                                func: AggregateFunction::Count,
                                expr: None,
                                distinct: *distinct,
                            },
                            AggregateExpression::FunctionCall {
                                name,
                                expr,
                                distinct,
                            } => Agg {
                                func: name.clone(),
                                expr: Some(self.compile(expr, g)),
                                distinct: *distinct,
                            },
                        };
                        (self.ctx.var(v.as_str()), agg)
                    })
                    .collect();
                let n = group(child, keys, aggs, self.ctx);
                Ok(self.apply_filters(n, filters))
            }
            GP::Service {
                name,
                inner,
                silent,
            } => {
                let endpoint = match self.named_pattern(name) {
                    PT::C(id) => PathEnd::Const(id),
                    PT::V(v) => PathEnd::Var(v),
                };
                let mut names = Vec::new();
                collect_pattern_vars(inner, &mut names);
                let mut vars: Vec<VarId> = names.iter().map(|n| self.ctx.var(n)).collect();
                vars.sort_unstable();
                vars.dedup();
                if let PathEnd::Var(v) = endpoint
                    && !vars.contains(&v)
                {
                    vars.push(v);
                }
                let query = format!("SELECT * WHERE {{ {inner} }}");
                let mut n = Node::leaf(
                    Kind::Service {
                        endpoint,
                        query: query.clone(),
                        silent: *silent,
                    },
                    vars,
                    1000.0,
                    query,
                );
                n.certain.clear();
                n.cost = 100_000.0;
                Ok(self.apply_filters(n, filters))
            }
            #[allow(unreachable_patterns)]
            _ => Err(Error::unsupported("unsupported graph pattern (LATERAL)")),
        }
    }

    fn split_filters(
        &self,
        filters: Vec<Expr>,
        ok: impl Fn(&VarId) -> bool,
    ) -> (Vec<Expr>, Vec<Expr>) {
        filters
            .into_iter()
            .partition(|f| !f.has_exists() && f.var_set().iter().all(&ok))
    }

    fn apply_filters(&self, n: Node, filters: Vec<Expr>) -> Node {
        if filters.is_empty() || n.is_empty() {
            return n;
        }
        filter(n, filters, self.ctx)
    }

    /// Flatten a join group into items + filters.
    fn collect(&mut self, gp: &GraphPattern, g: &ActiveGraph, items: &mut Vec<Item>) -> Result<()> {
        use GraphPattern as GP;
        match gp {
            GP::Bgp { patterns } => {
                for tp in patterns {
                    items.push(Item::Triple(self.triple(tp, g)));
                }
            }
            GP::Path {
                subject,
                path,
                object,
            } => {
                let s = self.term_pattern(subject);
                let o = self.term_pattern(object);
                self.collect_path(s, path, o, g, items);
            }
            GP::Join { left, right } => {
                self.collect(left, g, items)?;
                self.collect(right, g, items)?;
            }
            GP::Graph { name, inner } if !self.simple_graph_group(name, inner) => {
                // Per-graph evaluation (Jena OpGraph semantics): evaluate the inner pattern
                // against each named graph with the graph variable unbound, then join
                // with ?g = graph.
                let NamedNodePattern::Variable(gv) = name else {
                    unreachable!()
                };
                let v = self.ctx.var(gv.as_str());
                let accept = |gid: &Id| {
                    self.ctx
                        .dataset
                        .named
                        .as_ref()
                        .is_none_or(|s| s.contains(gid))
                };
                let graphs: Vec<Id> = self
                    .ctx
                    .snap
                    .graph_ids()?
                    .into_iter()
                    .filter(accept)
                    .collect();
                let mut branches = Vec::new();
                for gid in graphs {
                    let node = self.plan(inner, &ActiveGraph::Named(gid), Vec::new())?;
                    if node.is_empty() {
                        continue;
                    }
                    let mut t = Table::new(vec![v]);
                    t.push_row(&[gid]);
                    let bind = Node::leaf(Kind::Values(t), vec![v], 1.0, "graph".into());
                    branches.push(join(node, bind, self.ctx));
                }
                let mut n = union(branches);
                if n.vars.is_empty() && n.children.is_empty() {
                    n = Node::empty(vec![v]);
                }
                items.push(Item::Node(n));
            }
            GP::Graph { name, inner } => {
                let g2 = match name {
                    NamedNodePattern::NamedNode(n)
                        if n.as_str() == super::ctx::DEFAULT_GRAPH_IRI =>
                    {
                        ActiveGraph::Named(Id::DEFAULT_GRAPH)
                    }
                    NamedNodePattern::NamedNode(n) if n.as_str() == super::ctx::UNION_GRAPH_IRI => {
                        ActiveGraph::Union
                    }
                    _ => match self.named_pattern(name) {
                        PT::C(id) => ActiveGraph::Named(id),
                        PT::V(v) => ActiveGraph::Var(v),
                    },
                };
                if let GP::Filter { .. } = &**inner {
                    items.push(Item::Node(self.plan(inner, &g2, Vec::new())?));
                } else {
                    self.collect(inner, &g2, items)?;
                }
                // GRAPH ?g / GRAPH <g> iterate over (existing) named graphs even when the
                // inner pattern does not touch the data
                items.push(Item::Node(self.graph_names(&g2)?));
            }
            other => items.push(Item::Node(self.plan(other, g, Vec::new())?)),
        }
        Ok(())
    }

    fn collect_path(
        &mut self,
        s: PT,
        path: &PropertyPathExpression,
        o: PT,
        g: &ActiveGraph,
        items: &mut Vec<Item>,
    ) {
        use PropertyPathExpression as PP;
        match path {
            PP::NamedNode(n) => items.push(Item::Triple(Triple {
                t: [
                    s,
                    PT::C(self.ctx.intern_term(&Term::NamedNode(n.clone()))),
                    o,
                ],
                graph: g.clone(),
            })),
            PP::Reverse(p) => self.collect_path(o, p, s, g, items),
            PP::Sequence(a, b) => {
                let mid = PT::V(self.ctx.fresh_var());
                self.collect_path(s, a, mid, g, items);
                self.collect_path(mid, b, o, g, items);
            }
            _ => items.push(Item::Path(PathItem {
                s,
                path: path.clone(),
                o,
                graph: g.clone(),
            })),
        }
    }

    fn triple(&self, tp: &TriplePattern, g: &ActiveGraph) -> Triple {
        Triple {
            t: [
                self.term_pattern(&tp.subject),
                self.named_pattern(&tp.predicate),
                self.term_pattern(&tp.object),
            ],
            graph: g.clone(),
        }
    }

    /// `GRAPH ?g { P }` can bind ?g as a scan column when P is a plain join group that
    /// does not mention ?g itself.
    fn simple_graph_group(&self, name: &NamedNodePattern, inner: &GraphPattern) -> bool {
        let NamedNodePattern::Variable(v) = name else {
            return true;
        };
        fn plain(gp: &GraphPattern) -> bool {
            match gp {
                GraphPattern::Bgp { .. } | GraphPattern::Path { .. } => true,
                GraphPattern::Join { left, right } => plain(left) && plain(right),
                GraphPattern::Filter { inner, expr } => plain(inner) && !has_exists(expr),
                _ => false,
            }
        }
        let mut names = Vec::new();
        collect_pattern_vars(inner, &mut names);
        plain(inner) && !names.iter().any(|n| n == v.as_str())
    }

    /// One row per named graph of the active dataset (bound to the graph variable), or
    /// the unit table if a constant graph exists.
    fn graph_names(&self, g: &ActiveGraph) -> Result<Node> {
        let accept = |gid: Id| match &self.ctx.dataset.named {
            Some(set) => set.contains(&gid),
            None => true,
        };
        let exists = |gid: Id| -> Result<bool> {
            Ok(accept(gid)
                && gid.tag() != Tag::Local
                && self.ctx.snap.count(Perm::Gspo, &[gid.0])? > 0)
        };
        Ok(match g {
            ActiveGraph::Var(v) => match self.subst.get(v) {
                Some(id) => {
                    if exists(*id)? {
                        Node::unit()
                    } else {
                        Node::empty(Vec::new())
                    }
                }
                None => {
                    let mut t = Table::new(vec![*v]);
                    for gid in self.ctx.snap.graph_ids()? {
                        if accept(gid) {
                            t.push_row(&[gid]);
                        }
                    }
                    let est = t.len() as f64;
                    let mut n = Node::leaf(
                        Kind::Values(t),
                        vec![*v],
                        est,
                        format!("{est} named graphs"),
                    );
                    n.cost = est;
                    n
                }
            },
            ActiveGraph::Named(id) if *id == Id::DEFAULT_GRAPH => Node::unit(),
            ActiveGraph::Named(id) => {
                if exists(*id)? {
                    Node::unit()
                } else {
                    Node::empty(Vec::new())
                }
            }
            _ => Node::unit(),
        })
    }

    // --------------------------------------------------------- graph filters ------

    fn graph_filter(&self, g: &ActiveGraph) -> Option<(GraphFilter, Option<VarId>)> {
        let ds = &self.ctx.dataset;
        Some(match g {
            ActiveGraph::Default => match &ds.default {
                Some(set) if set.len() == 1 => (GraphFilter::One(set[0].0), None),
                Some(set) => {
                    let mut s: Vec<u64> = set.iter().map(|i| i.0).collect();
                    s.sort_unstable();
                    (GraphFilter::Set(s), None)
                }
                None if ds.union_default || self.ctx.snap.union_default_graph => {
                    (GraphFilter::Named, None)
                }
                None => (GraphFilter::Default, None),
            },
            ActiveGraph::Union => (GraphFilter::Named, None),
            ActiveGraph::Named(id) => {
                if id.tag() == Tag::Local {
                    return None;
                }
                if let Some(named) = &ds.named
                    && !named.contains(id)
                {
                    return None;
                }
                (GraphFilter::One(id.0), None)
            }
            ActiveGraph::Var(v) => match self.subst.get(v) {
                Some(id) => return self.graph_filter(&ActiveGraph::Named(*id)),
                None => match &ds.named {
                    Some(set) => {
                        let mut s: Vec<u64> = set.iter().map(|i| i.0).collect();
                        s.sort_unstable();
                        (GraphFilter::Set(s), Some(*v))
                    }
                    None => (GraphFilter::Named, Some(*v)),
                },
            },
        })
    }

    // ----------------------------------------------------------------- scans ------

    /// All useful scan plans for a triple pattern (one per sort order).
    fn scan_options(&self, t: &Triple) -> Result<Vec<Node>> {
        let vars_of = |t: &Triple| {
            let mut v: Vec<VarId> =
                t.t.iter()
                    .filter_map(|x| if let PT::V(v) = x { Some(*v) } else { None })
                    .collect();
            if let ActiveGraph::Var(g) = t.graph {
                v.push(g);
            }
            v.sort_unstable();
            v.dedup();
            v
        };
        let Some((gf, gvar)) = self.graph_filter(&t.graph) else {
            return Ok(vec![Node::empty(vars_of(t))]);
        };
        // unknown constants → empty
        if t.t
            .iter()
            .any(|x| matches!(x, PT::C(id) if id.tag() == Tag::Local))
        {
            return Ok(vec![Node::empty(vars_of(t))]);
        }
        // positions S=0,P=1,O=2,G=3 → PT
        let mut pos: [Option<PT>; 4] = [Some(t.t[0]), Some(t.t[1]), Some(t.t[2]), None];
        if let Some(gv) = gvar {
            pos[3] = Some(PT::V(gv));
        }
        let bound: Vec<usize> = (0..3)
            .filter(|&i| matches!(pos[i], Some(PT::C(_))))
            .collect();
        let mut out = Vec::new();
        let mut perms: Vec<Perm> = [
            Perm::Spo,
            Perm::Sop,
            Perm::Pso,
            Perm::Pos,
            Perm::Osp,
            Perm::Ops,
        ]
        .into_iter()
        .filter(|p| {
            let o = p.order();
            let mut first: Vec<usize> = o[..bound.len()].to_vec();
            first.sort_unstable();
            first == bound
        })
        .collect();
        // GSPO for a single constant graph with a compatible prefix
        let use_gspo = matches!(gf, GraphFilter::One(_))
            && (bound.is_empty() || bound == [S] || bound == [S, P] || bound == [S, P, O]);
        if use_gspo {
            perms.push(Perm::Gspo);
        }
        // de-duplicate perms that produce the same sort order for this pattern
        let mut seen_orders: FxHashSet<Vec<VarId>> = FxHashSet::default();
        let stats = &self.ctx.snap.generation.stats;
        let snap = &self.ctx.snap;
        for perm in perms {
            let order = perm.order();
            let (prefix, first_free): (Vec<u64>, usize) = if perm == Perm::Gspo {
                let GraphFilter::One(gid) = gf else {
                    unreachable!()
                };
                let mut p = vec![gid];
                for &c in &order[1..] {
                    match pos[c] {
                        Some(PT::C(id)) => p.push(id.0),
                        _ => break,
                    }
                }
                let n = p.len();
                (p, n)
            } else {
                (
                    bound
                        .iter()
                        .map(|_| 0)
                        .enumerate()
                        .map(|(i, _)| match pos[order[i]] {
                            Some(PT::C(id)) => id.0,
                            _ => unreachable!(),
                        })
                        .collect(),
                    bound.len(),
                )
            };
            let mut cols: Vec<(usize, VarId)> = Vec::new();
            let mut eqs = Vec::new();
            for (kc, &c) in order.iter().enumerate().skip(first_free) {
                if let Some(PT::V(v)) = pos[c] {
                    match cols.iter().find(|(_, x)| *x == v) {
                        Some((k0, _)) => eqs.push((*k0, kc)),
                        None => cols.push((kc, v)),
                    }
                }
            }
            let sort_vars: Vec<VarId> = cols.iter().map(|(_, v)| *v).collect();
            if !seen_orders.insert(sort_vars.iter().take(1).copied().collect()) {
                continue;
            }
            let graph_col = perm.col_of(G);
            let graph = if perm == Perm::Gspo
                || gvar.is_some() && !matches!(gf, GraphFilter::Set(_) | GraphFilter::Named)
            {
                GraphFilter::All
            } else {
                gf.clone()
            };
            let dedup = gvar.is_none() && gf.multi() && perm != Perm::Gspo;
            let mut est = if prefix.is_empty() {
                snap.len() as f64
            } else {
                snap.count(perm, &prefix)? as f64
            };
            if matches!(gf, GraphFilter::Default) && !stats.graphs.is_empty() && perm != Perm::Gspo
            {
                let total: u64 = stats.graphs.iter().map(|(_, n)| n).sum();
                let dflt = stats
                    .graphs
                    .iter()
                    .find(|(g, _)| *g == Id::DEFAULT_GRAPH.0)
                    .map_or(0, |(_, n)| *n);
                if total > 0 {
                    est *= (dflt as f64 / total as f64).max(0.01);
                }
            }
            if !eqs.is_empty() {
                est *= 0.1;
            }
            let est = est.max(if est > 0.0 { 1.0 } else { 0.0 });
            // distinct estimates
            let mut dist = FxHashMap::default();
            let pstat = match pos[P] {
                Some(PT::C(p)) => stats.predicate(p.0),
                _ => None,
            };
            let quads = snap.len().max(1) as f64;
            for &(kc, v) in &cols {
                let comp = order[kc];
                let d = match (comp, pstat) {
                    (S, Some(ps)) if pstat.is_some() => {
                        ps.distinct_subjects as f64 * est / ps.count.max(1) as f64
                    }
                    (O, Some(ps)) => ps.distinct_objects as f64 * est / ps.count.max(1) as f64,
                    (S, None) => est * (stats.distinct_subjects as f64 / quads).min(1.0),
                    (O, None) => est * (stats.distinct_objects as f64 / quads).min(1.0),
                    (P, _) => (stats.distinct_predicates as f64).min(est),
                    _ => est,
                };
                let d = if kc == first_free { d } else { est };
                dist.insert(v, d.clamp(1.0, est.max(1.0)));
            }
            let desc = format!(
                "{} {}",
                perm.name().to_uppercase(),
                t.t.iter()
                    .map(|x| self.pt_str(x))
                    .collect::<Vec<_>>()
                    .join(" ")
            ) + &match &t.graph {
                ActiveGraph::Default => String::new(),
                g => format!(
                    " GRAPH {}",
                    match g {
                        ActiveGraph::Named(id) => self.pt_str(&PT::C(*id)),
                        ActiveGraph::Var(v) => self.pt_str(&PT::V(*v)),
                        _ => String::new(),
                    }
                ),
            };
            let vars: Vec<VarId> = cols.iter().map(|(_, v)| *v).collect();
            let mut n = Node::leaf(
                Kind::Scan(ScanSpec {
                    perm,
                    prefix,
                    cols,
                    eqs,
                    graph,
                    graph_col,
                    dedup,
                }),
                vars,
                est,
                desc,
            );
            n.sorted = sort_vars;
            n.dist = dist;
            if est == 0.0 {
                return Ok(vec![Node::empty(vars_of(t))]);
            }
            out.push(n);
        }
        if out.is_empty() {
            return Ok(vec![Node::empty(vars_of(t))]);
        }
        Ok(out)
    }

    fn pt_str(&self, x: &PT) -> String {
        match x {
            PT::V(v) => {
                let n = self.ctx.var_name(*v);
                if n.starts_with(' ') {
                    "?_".into()
                } else {
                    format!("?{n}")
                }
            }
            PT::C(id) => self
                .ctx
                .term(*id)
                .map_or_else(|| format!("{id:?}"), |t| short(&t)),
        }
    }

    // ------------------------------------------------------------- join groups ------

    fn plan_group(&mut self, items: Vec<Item>, mut filters: Vec<Expr>) -> Result<Node> {
        let mut triples = Vec::new();
        let mut paths: Vec<PathItem> = Vec::new();
        let mut nodes = Vec::new();
        for it in items {
            match it {
                Item::Triple(t) => triples.push(t),
                Item::Path(p) => paths.push(p),
                Item::Node(n) => nodes.push(n),
            }
        }

        // Filter-equality substitution (Jena TransformFilterEquality): FILTER(?x = <iri>)
        // with ?x only used in triple patterns → substitute the constant.
        let mut binds: Vec<(VarId, Id)> = Vec::new();
        let mut i = 0;
        while i < filters.len() {
            if let Some((v, c)) = equality_constant(&filters[i], self.ctx)
                && triples.iter().any(|t| t.t.contains(&PT::V(v)))
                && !nodes.iter().any(|n| n.vars.contains(&v))
                && !paths.iter().any(|p| p.s == PT::V(v) || p.o == PT::V(v))
                && !triples
                    .iter()
                    .any(|t| matches!(t.graph, ActiveGraph::Var(g) if g == v))
            {
                filters.remove(i);
                for t in &mut triples {
                    for x in &mut t.t {
                        if *x == PT::V(v) {
                            *x = PT::C(c);
                        }
                    }
                }
                for f in &mut filters {
                    substitute(f, v, c);
                }
                binds.push((v, c));
                continue;
            }
            i += 1;
        }

        let mut leaves: Vec<Vec<Node>> = Vec::new();
        for t in &triples {
            leaves.push(self.scan_options(t)?);
        }
        for n in nodes {
            leaves.push(vec![n]);
        }
        let mut result = if leaves.is_empty() {
            Node::unit()
        } else if leaves.iter().any(|l| l[0].is_empty()) {
            let mut vars: Vec<VarId> = leaves.iter().flat_map(|l| l[0].vars.clone()).collect();
            vars.sort_unstable();
            vars.dedup();
            Node::empty(vars)
        } else {
            self.join_order(leaves, &mut filters)?
        };

        // attach property paths, preferring those whose endpoint is already bound
        while !paths.is_empty() {
            let idx = paths
                .iter()
                .position(|p| {
                    [p.s, p.o].iter().any(|e| {
                        matches!(e, PT::C(_)) || matches!(e, PT::V(v) if result.vars.contains(v))
                    })
                })
                .unwrap_or(0);
            let p = paths.remove(idx);
            result = self.attach_path(result, p)?;
            let (now, later): (Vec<Expr>, Vec<Expr>) =
                std::mem::take(&mut filters).into_iter().partition(|f| {
                    !f.has_exists() && f.var_set().iter().all(|v| result.vars.contains(v))
                });
            filters = later;
            if !now.is_empty() {
                result = filter(result, now, self.ctx);
            }
        }
        if !filters.is_empty() {
            result = filter(result, filters, self.ctx);
        }
        for (v, c) in binds {
            let mut n = Node::unary(
                Kind::Extend(v, Expr::Const(c)),
                result,
                format!("?{} := {}", self.ctx.var_name(v), self.pt_str(&PT::C(c))),
            );
            n.vars.push(v);
            n.certain.push(v);
            n.dist.insert(v, 1.0);
            result = n;
        }
        Ok(result)
    }

    /// DP / greedy join ordering over connected components (QLever QueryPlanner).
    fn join_order(&self, leaves: Vec<Vec<Node>>, filters: &mut Vec<Expr>) -> Result<Node> {
        let n = leaves.len();
        // connected components by shared variables
        let mut comp: Vec<usize> = (0..n).collect();
        fn find(c: &mut [usize], x: usize) -> usize {
            if c[x] != x {
                let r = find(c, c[x]);
                c[x] = r;
            }
            c[x]
        }
        for i in 0..n {
            for j in i + 1..n {
                if leaves[i][0]
                    .vars
                    .iter()
                    .any(|v| leaves[j][0].vars.contains(v))
                {
                    let (a, b) = (find(&mut comp, i), find(&mut comp, j));
                    comp[a] = b;
                }
            }
        }
        let mut groups: FxHashMap<usize, Vec<usize>> = FxHashMap::default();
        for i in 0..n {
            let r = find(&mut comp, i);
            groups.entry(r).or_default().push(i);
        }
        let mut leaves: Vec<Option<Vec<Node>>> = leaves.into_iter().map(Some).collect();
        let mut parts: Vec<Node> = Vec::new();
        let mut group_list: Vec<Vec<usize>> = groups.into_values().collect();
        group_list.sort();
        for members in group_list {
            let items: Vec<Vec<Node>> =
                members.iter().map(|&i| leaves[i].take().unwrap()).collect();
            let plan = if items.len() == 1 {
                let mut opts = items.into_iter().next().unwrap();
                opts.sort_by(|a, b| a.cost.total_cmp(&b.cost));
                let best = opts.swap_remove(0);
                self.place_filters(best, filters)
            } else if items.len() <= DP_LIMIT {
                self.dp(items, filters)?
            } else {
                self.greedy(items, filters)?
            };
            parts.push(plan);
        }
        // cross products between components, smallest first
        parts.sort_by(|a, b| a.est.total_cmp(&b.est));
        let mut it = parts.into_iter();
        let mut acc = it.next().unwrap();
        for p in it {
            acc = self.place_filters(join(acc, p, self.ctx), filters);
        }
        Ok(acc)
    }

    /// Apply (and remove) filters whose variables are all bound by `n`.
    fn place_filters(&self, n: Node, filters: &mut Vec<Expr>) -> Node {
        let (now, later): (Vec<Expr>, Vec<Expr>) =
            std::mem::take(filters).into_iter().partition(|f| {
                !f.has_exists() && {
                    let vs = f.var_set();
                    !vs.is_empty() && vs.iter().all(|v| n.vars.contains(v))
                }
            });
        *filters = later;
        if now.is_empty() {
            n
        } else {
            filter(n, now, self.ctx)
        }
    }

    fn dp(&self, items: Vec<Vec<Node>>, filters: &mut Vec<Expr>) -> Result<Node> {
        let n = items.len();
        let filter_vars: Vec<Vec<VarId>> = filters
            .iter()
            .map(|f| {
                if f.has_exists() {
                    vec![VarId::MAX]
                } else {
                    f.var_set()
                }
            })
            .collect();
        // table: mask → candidates (one per first sort var)
        let mut table: FxHashMap<u32, Vec<(Node, u64)>> = FxHashMap::default();
        for (i, opts) in items.into_iter().enumerate() {
            let mut v = Vec::new();
            for o in opts {
                let (o, fm) = self.apply_dp_filters(o, 0, filters, &filter_vars);
                v.push((o, fm));
            }
            table.insert(1 << i, v);
        }
        for size in 2..=n {
            let masks: Vec<u32> = (1u32..(1 << n))
                .filter(|m| m.count_ones() as usize == size)
                .collect();
            for mask in masks {
                self.ctx.check()?;
                let mut cands: Vec<(Node, u64)> = Vec::new();
                // enumerate splits (sub, mask^sub) with sub < rest to avoid duplicates
                let mut sub = (mask - 1) & mask;
                while sub > 0 {
                    let rest = mask ^ sub;
                    if sub < rest
                        && let (Some(l), Some(r)) = (table.get(&sub), table.get(&rest))
                    {
                        for (a, fa) in l {
                            for (b, fb) in r {
                                if !a.vars.iter().any(|v| b.vars.contains(v)) {
                                    continue;
                                }
                                for c in join_candidates(a, b, self.ctx) {
                                    let (c, fm) =
                                        self.apply_dp_filters(c, fa | fb, filters, &filter_vars);
                                    cands.push((c, fm));
                                }
                            }
                        }
                    }
                    sub = (sub - 1) & mask;
                }
                if cands.is_empty() {
                    continue;
                }
                // prune: best per first sort var
                let mut best: FxHashMap<Option<VarId>, (Node, u64)> = FxHashMap::default();
                for (c, fm) in cands {
                    let key = c.sorted.first().copied();
                    match best.get(&key) {
                        Some((b, _)) if b.cost <= c.cost => {}
                        _ => {
                            best.insert(key, (c, fm));
                        }
                    }
                }
                table.insert(mask, best.into_values().collect());
            }
        }
        let full = (1u32 << n) - 1;
        let cands = table
            .remove(&full)
            .ok_or_else(|| Error::invalid("join planning failed"))?;
        let (best, fm) = cands
            .into_iter()
            .min_by(|a, b| a.0.cost.total_cmp(&b.0.cost))
            .unwrap();
        // remove applied filters
        let mut i = 0;
        filters.retain(|_| {
            let keep = fm & (1 << i) == 0;
            i += 1;
            keep
        });
        Ok(best)
    }

    fn apply_dp_filters(
        &self,
        n: Node,
        applied: u64,
        filters: &[Expr],
        fvars: &[Vec<VarId>],
    ) -> (Node, u64) {
        let mut now = Vec::new();
        let mut mask = applied;
        for (i, f) in filters.iter().enumerate().take(64) {
            if mask & (1 << i) == 0
                && !fvars[i].is_empty()
                && fvars[i].iter().all(|v| n.vars.contains(v))
            {
                now.push(f.clone());
                mask |= 1 << i;
            }
        }
        if now.is_empty() {
            (n, mask)
        } else {
            (filter(n, now, self.ctx), mask)
        }
    }

    fn greedy(&self, items: Vec<Vec<Node>>, filters: &mut Vec<Expr>) -> Result<Node> {
        let mut plans: Vec<Node> = items
            .into_iter()
            .map(|mut o| {
                o.sort_by(|a, b| a.cost.total_cmp(&b.cost));
                o.swap_remove(0)
            })
            .map(|p| self.place_filters(p, filters))
            .collect();
        while plans.len() > 1 {
            self.ctx.check()?;
            let mut best: Option<(usize, usize, Node)> = None;
            for i in 0..plans.len() {
                for j in i + 1..plans.len() {
                    if !plans[i].vars.iter().any(|v| plans[j].vars.contains(v)) {
                        continue;
                    }
                    for c in join_candidates(&plans[i], &plans[j], self.ctx) {
                        if best.as_ref().is_none_or(|(_, _, b)| {
                            c.est < b.est || (c.est == b.est && c.cost < b.cost)
                        }) {
                            best = Some((i, j, c));
                        }
                    }
                }
            }
            let Some((i, j, c)) = best else { break };
            plans.remove(j);
            plans.remove(i);
            plans.push(self.place_filters(c, filters));
        }
        let mut it = plans.into_iter();
        let mut acc = it.next().unwrap();
        for p in it {
            acc = self.place_filters(join(acc, p, self.ctx), filters);
        }
        Ok(acc)
    }

    // --------------------------------------------------------------- paths ------

    fn attach_path(&mut self, left: Node, p: PathItem) -> Result<Node> {
        use PropertyPathExpression as PP;
        let (min, max_one, inner) = match &p.path {
            PP::ZeroOrMore(i) => (0u8, false, &**i),
            PP::OneOrMore(i) => (1, false, &**i),
            PP::ZeroOrOne(i) => (0, true, &**i),
            PP::Alternative(..) => {
                let mut alts = Vec::new();
                flatten_alt(&p.path, &mut alts);
                let mut branches = Vec::new();
                for a in alts {
                    let mut items = Vec::new();
                    self.collect_path(p.s, a, p.o, &p.graph, &mut items);
                    branches.push(self.plan_group(items, Vec::new())?);
                }
                return Ok(join(left, union(branches), self.ctx));
            }
            PP::NegatedPropertySet(set) => {
                // ?s ?p ?o FILTER(?p NOT IN set) (forward only; spargebra splits inverses)
                let pv = self.ctx.fresh_var();
                let t = Triple {
                    t: [p.s, PT::V(pv), p.o],
                    graph: p.graph.clone(),
                };
                let mut opts = self.scan_options(&t)?;
                opts.sort_by(|a, b| a.cost.total_cmp(&b.cost));
                let scan = opts.swap_remove(0);
                let list: Vec<Expr> = set
                    .iter()
                    .map(|n| Expr::Const(self.ctx.intern_term(&Term::NamedNode(n.clone()))))
                    .collect();
                let f = Expr::Not(Box::new(Expr::In(Box::new(Expr::Var(pv)), list)));
                let mut n = filter(scan, vec![f], self.ctx);
                n = project(
                    n.clone(),
                    n.vars.iter().copied().filter(|v| *v != pv).collect(),
                    self.ctx,
                );
                return Ok(join(left, n, self.ctx));
            }
            _ => {
                let mut items = Vec::new();
                self.collect_path(p.s, &p.path, p.o, &p.graph, &mut items);
                let n = self.plan_group(items, Vec::new())?;
                return Ok(join(left, n, self.ctx));
            }
        };
        let Some((gf, gvar)) = self.graph_filter(&p.graph) else {
            return Ok(Node::empty(left.vars.clone()));
        };
        let to_end = |x: PT| match x {
            PT::C(id) => PathEnd::Const(id),
            PT::V(v) => PathEnd::Var(v),
        };
        let simple = match inner {
            PP::NamedNode(n) => Some((self.ctx.intern_term(&Term::NamedNode(n.clone())), false)),
            PP::Reverse(r) => match &**r {
                PP::NamedNode(n) => Some((self.ctx.intern_term(&Term::NamedNode(n.clone())), true)),
                _ => None,
            },
            _ => None,
        };
        let mut children = Vec::new();
        let mut edge_vars = None;
        let simple = match simple {
            Some((id, _)) if id.tag() == Tag::Local && min == 1 => {
                return Ok(Node::empty(left.vars.clone()));
            }
            Some((id, rev)) if id.tag() != Tag::Local => Some((id.0, rev)),
            _ => {
                let (a, b) = (self.ctx.fresh_var(), self.ctx.fresh_var());
                let mut items = Vec::new();
                self.collect_path(PT::V(a), inner, PT::V(b), &p.graph, &mut items);
                let edges = self.plan_group(items, Vec::new())?;
                let mut keep = vec![a, b];
                if let Some(g) = gvar {
                    keep.push(g);
                }
                children.push(project(edges, keep, self.ctx));
                edge_vars = Some((a, b));
                None
            }
        };
        let spec = PathSpec {
            subj: to_end(p.s),
            obj: to_end(p.o),
            min,
            max_one,
            simple,
            graph: gf,
            graph_var: gvar,
            edge_vars,
        };
        let mut vars = Vec::new();
        for e in [&spec.subj, &spec.obj] {
            if let PathEnd::Var(v) = e
                && !vars.contains(v)
            {
                vars.push(*v);
            }
        }
        if let Some(g) = gvar {
            vars.push(g);
        }
        let bound_var = [&spec.subj, &spec.obj].into_iter().find_map(|e| match e {
            PathEnd::Var(v) if left.vars.contains(v) && left.certain.contains(v) => Some(*v),
            _ => None,
        });
        let desc = format!("{} {} {}", self.pt_str(&p.s), p.path, self.pt_str(&p.o));
        let fanout = 10.0;
        if let Some(bv) = bound_var
            && !left.vars.is_empty()
            && gvar.is_none()
        {
            // bound-side traversal from the left input (QLever TransitivePath with bound side)
            let est = left.est * fanout;
            let mut out_vars = left.vars.clone();
            for v in &vars {
                if !out_vars.contains(v) {
                    out_vars.push(*v);
                }
            }
            let mut certain = left.certain.clone();
            certain.extend(vars.iter().copied());
            let dist = out_vars.iter().map(|&v| (v, est)).collect();
            let cost = left.cost + est;
            children.push(left);
            let _ = bv;
            return Ok(Node {
                kind: Kind::Path {
                    spec: Box::new(spec),
                    bound_from_left: true,
                },
                children,
                vars: out_vars,
                certain,
                sorted: Vec::new(),
                est,
                cost,
                dist,
                desc,
            });
        }
        let est = match (&spec.subj, &spec.obj) {
            (PathEnd::Const(_), PathEnd::Const(_)) => 1.0,
            (PathEnd::Const(_), _) | (_, PathEnd::Const(_)) => 100.0,
            _ => (self.ctx.snap.len() as f64).max(1.0),
        };
        let mut n = Node::leaf(
            Kind::Path {
                spec: Box::new(spec),
                bound_from_left: false,
            },
            vars,
            est,
            desc,
        );
        n.children = children;
        n.cost = est * 2.0 + n.children.iter().map(|c| c.cost).sum::<f64>();
        if left.vars.is_empty() && matches!(left.kind, Kind::Values(_)) {
            return Ok(n);
        }
        Ok(join(left, n, self.ctx))
    }
}

// ------------------------------------------------------------------------------
// node constructors (with estimates)
// ------------------------------------------------------------------------------

fn merge_dist(a: &Node, b: &Node, est: f64) -> FxHashMap<VarId, f64> {
    let mut d = FxHashMap::default();
    for v in a.vars.iter().chain(b.vars.iter()) {
        let x = match (a.vars.contains(v), b.vars.contains(v)) {
            (true, true) => a.d(*v).min(b.d(*v)),
            (true, false) => a.d(*v),
            _ => b.d(*v),
        };
        d.insert(*v, x.min(est).max(1.0));
    }
    d
}

fn join_est(a: &Node, b: &Node, keys: &[VarId]) -> f64 {
    if keys.is_empty() {
        return a.est * b.est;
    }
    let denom = keys
        .iter()
        .map(|v| a.d(*v).max(b.d(*v)))
        .fold(1.0f64, f64::max);
    // QLever correction factor
    (a.est * b.est / denom * 0.7).max(if a.est > 0.0 && b.est > 0.0 { 1.0 } else { 0.0 })
}

fn sort_cost(n: f64) -> f64 {
    n * n.max(2.0).log2() * 0.25
}

fn mk_join(a: Node, b: Node, algo: JoinAlgo, keys: Vec<VarId>, extra_cost: f64) -> Node {
    let est = join_est(&a, &b, &keys);
    let mut vars = a.vars.clone();
    for v in &b.vars {
        if !vars.contains(v) {
            vars.push(*v);
        }
    }
    let mut certain = a.certain.clone();
    for v in &b.certain {
        if !certain.contains(v) {
            certain.push(*v);
        }
    }
    let sorted = match algo {
        JoinAlgo::Merge => vec![keys[0]],
        JoinAlgo::Hash => {
            // probe side (larger) order is preserved
            if a.est >= b.est {
                a.sorted.clone()
            } else {
                b.sorted.clone()
            }
        }
        JoinAlgo::Cross => a.sorted.clone(),
    };
    let base = match algo {
        JoinAlgo::Merge => a.est + b.est,
        JoinAlgo::Hash => 2.0 * a.est.min(b.est) + a.est.max(b.est),
        JoinAlgo::Cross => a.est * b.est,
    };
    let cost = a.cost + b.cost + base + est + extra_cost;
    let dist = merge_dist(&a, &b, est);
    let desc = format!(
        "on {}",
        if keys.is_empty() {
            "()".to_string()
        } else {
            format!("{} var(s)", keys.len())
        }
    );
    Node {
        kind: Kind::Join { algo, keys },
        children: vec![a, b],
        vars,
        certain,
        sorted,
        est,
        cost,
        dist,
        desc,
    }
}

fn sort_node(n: Node, v: VarId) -> Node {
    if n.sorted.first() == Some(&v) {
        return n;
    }
    let c = sort_cost(n.est);
    let mut s = Node::unary(Kind::Sort(vec![v]), n, String::new());
    s.sorted = vec![v];
    s.cost += c;
    s
}

/// Join alternatives for the DP (merge join on each shared certain var, hash join).
fn join_candidates(a: &Node, b: &Node, ctx: &Ctx) -> Vec<Node> {
    let keys: Vec<VarId> = a
        .vars
        .iter()
        .filter(|v| b.vars.contains(v))
        .copied()
        .collect();
    let mut out = Vec::new();
    let certain_both: Vec<VarId> = keys
        .iter()
        .filter(|v| a.certain.contains(v) && b.certain.contains(v))
        .copied()
        .collect();
    for &v in &certain_both {
        let (sa, sb) = (a.sorted.first() == Some(&v), b.sorted.first() == Some(&v));
        let extra =
            if sa { 0.0 } else { sort_cost(a.est) } + if sb { 0.0 } else { sort_cost(b.est) };
        if sa && sb || extra < (a.est + b.est) * 4.0 {
            let (x, y) = (sort_node(a.clone(), v), sort_node(b.clone(), v));
            let mut k = vec![v];
            k.extend(keys.iter().filter(|x| **x != v));
            let mut j = mk_join(x, y, JoinAlgo::Merge, k, 0.0);
            j.desc = format!("on ?{}", ctx.var_name(v));
            out.push(j);
        }
    }
    let mut h = mk_join(a.clone(), b.clone(), JoinAlgo::Hash, keys.clone(), 0.0);
    h.desc = format!(
        "on {}",
        keys.iter()
            .map(|v| format!("?{}", ctx.var_name(*v)))
            .collect::<Vec<_>>()
            .join(" ")
    );
    out.push(h);
    out
}

/// Best single join of two plans (used outside the DP).
pub fn join(a: Node, b: Node, ctx: &Ctx) -> Node {
    if a.is_empty() || b.is_empty() {
        let mut vars = a.vars.clone();
        vars.extend(b.vars.iter().filter(|v| !a.vars.contains(v)));
        return Node::empty(vars);
    }
    if a.vars.is_empty() && matches!(&a.kind, Kind::Values(t) if t.len() == 1) {
        return b;
    }
    if b.vars.is_empty() && matches!(&b.kind, Kind::Values(t) if t.len() == 1) {
        return a;
    }
    if !a.vars.iter().any(|v| b.vars.contains(v)) {
        let mut j = mk_join(a, b, JoinAlgo::Cross, Vec::new(), 0.0);
        j.desc = "cross product".into();
        return j;
    }
    join_candidates(&a, &b, ctx)
        .into_iter()
        .min_by(|x, y| x.cost.total_cmp(&y.cost))
        .unwrap()
}

fn left_join(l: Node, r: Node, expr: Option<Expr>, ctx: &Ctx) -> Node {
    if l.is_empty() {
        return l;
    }
    let keys: Vec<VarId> = l
        .vars
        .iter()
        .filter(|v| r.vars.contains(v))
        .copied()
        .collect();
    let est = join_est(&l, &r, &keys).max(l.est);
    let mut vars = l.vars.clone();
    vars.extend(r.vars.iter().filter(|v| !l.vars.contains(v)));
    let desc = match &expr {
        Some(e) => format!("filter {}", e.display(ctx)),
        None => format!(
            "on {}",
            keys.iter()
                .map(|v| format!("?{}", ctx.var_name(*v)))
                .collect::<Vec<_>>()
                .join(" ")
        ),
    };
    let dist = merge_dist(&l, &r, est);
    Node {
        kind: Kind::LeftJoin { expr },
        vars,
        certain: l.certain.clone(),
        sorted: Vec::new(),
        est,
        cost: l.cost + r.cost + 2.0 * r.est + l.est + est,
        dist,
        desc,
        children: vec![l, r],
    }
}

fn union(children: Vec<Node>) -> Node {
    match children.len() {
        0 => return Node::empty(Vec::new()),
        1 => return children.into_iter().next().unwrap(),
        _ => {}
    }
    let mut vars: Vec<VarId> = Vec::new();
    for c in &children {
        for v in &c.vars {
            if !vars.contains(v) {
                vars.push(*v);
            }
        }
    }
    let certain: Vec<VarId> = vars
        .iter()
        .filter(|v| children.iter().all(|c| c.certain.contains(v)))
        .copied()
        .collect();
    let est: f64 = children.iter().map(|c| c.est).sum();
    let cost: f64 = children.iter().map(|c| c.cost).sum::<f64>() + est;
    let dist = vars.iter().map(|&v| (v, est.max(1.0))).collect();
    Node {
        kind: Kind::Union,
        vars,
        certain,
        sorted: Vec::new(),
        est,
        cost,
        dist,
        desc: format!("{} branches", children.len()),
        children,
    }
}

pub fn filter(n: Node, exprs: Vec<Expr>, ctx: &Ctx) -> Node {
    let desc = exprs
        .iter()
        .map(|e| e.display(ctx))
        .collect::<Vec<_>>()
        .join(" && ");
    let sel = FILTER_SELECTIVITY.powi(exprs.len() as i32);
    let mut f = Node::unary(Kind::Filter(exprs), n, desc);
    f.est = (f.est * sel).max(if f.est > 0.0 { 1.0 } else { 0.0 });
    for d in f.dist.values_mut() {
        *d = d.min(f.est.max(1.0));
    }
    f
}

pub fn project(n: Node, vars: Vec<VarId>, ctx: &Ctx) -> Node {
    let desc = vars
        .iter()
        .map(|v| format!("?{}", ctx.var_name(*v)))
        .collect::<Vec<_>>()
        .join(" ");
    let mut p = Node::unary(Kind::Project(vars.clone()), n, desc);
    p.cost = p.children[0].cost;
    p.certain.retain(|v| vars.contains(v));
    p.sorted = p
        .sorted
        .iter()
        .take_while(|v| vars.contains(v))
        .copied()
        .collect();
    p.vars = vars;
    p
}

fn slice(child: Node, start: usize, length: Option<usize>) -> Node {
    // ORDER BY + LIMIT → top-k (also through a projection)
    let limit = length.map(|l| l + start);
    let child = match (child, limit) {
        (mut n, Some(k)) if matches!(n.kind, Kind::OrderBy { limit: None, .. }) => {
            if let Kind::OrderBy { limit, .. } = &mut n.kind {
                *limit = Some(k);
            }
            n
        }
        (mut n, Some(k))
            if matches!(n.kind, Kind::Project(_))
                && matches!(n.children[0].kind, Kind::OrderBy { limit: None, .. }) =>
        {
            if let Kind::OrderBy { limit, .. } = &mut n.children[0].kind {
                *limit = Some(k);
            }
            n
        }
        (n, _) => n,
    };
    let desc = format!(
        "offset {start} limit {}",
        length.map_or("∞".to_string(), |l| l.to_string())
    );
    let mut n = Node::unary(
        Kind::Slice {
            offset: start,
            limit: length,
        },
        child,
        desc,
    );
    n.est = (n.est - start as f64).max(0.0);
    if let Some(l) = length {
        n.est = n.est.min(l as f64);
    }
    n
}

fn group(child: Node, keys: Vec<VarId>, aggs: Vec<(VarId, Agg)>, ctx: &Ctx) -> Node {
    // COUNT(*) over a single scan without grouping → index metadata (QLever)
    if keys.is_empty()
        && aggs.len() == 1
        && matches!(aggs[0].1.func, AggregateFunction::Count)
        && aggs[0].1.expr.is_none()
        && !aggs[0].1.distinct
        && let Kind::Scan(spec) = &child.kind
        && spec.eqs.is_empty()
        && !spec.dedup
    {
        let direct = match &spec.graph {
            GraphFilter::All => Some((spec.perm, spec.prefix.clone())),
            GraphFilter::Default | GraphFilter::One(_) if spec.prefix.is_empty() => {
                let g = match &spec.graph {
                    GraphFilter::One(g) => *g,
                    _ => Id::DEFAULT_GRAPH.0,
                };
                Some((Perm::Gspo, vec![g]))
            }
            _ => None,
        };
        if let Some((perm, prefix)) = direct {
            let var = aggs[0].0;
            let mut spec = spec.clone();
            spec.perm = perm;
            spec.prefix = prefix;
            let mut n = Node::leaf(
                Kind::CountScan { spec, var },
                vec![var],
                1.0,
                child.desc.clone(),
            );
            n.cost = 1.0;
            return n;
        }
    }
    // COUNT(DISTINCT ?k) over a single scan: in a permutation sorted on ?k the distinct
    // values are the runs of equal ids, counted without materializing or hashing rows
    if keys.is_empty()
        && aggs.len() == 1
        && matches!(aggs[0].1.func, AggregateFunction::Count)
        && aggs[0].1.distinct
        && let Some(Expr::Var(k)) = &aggs[0].1.expr
        && let Kind::Scan(spec) = &child.kind
        && let Some(spec) = reorder_scan(spec, *k)
    {
        let var = aggs[0].0;
        let desc = format!(
            "{} distinct ?{}",
            retarget_desc(&child.desc, &spec),
            ctx.var_name(*k)
        );
        let mut n = Node::leaf(Kind::CountDistinctScan { spec, var }, vec![var], 1.0, desc);
        n.cost = child.est;
        return n;
    }
    // GROUP BY ?k with only COUNT(*) / COUNT(?v) over a single scan
    if keys.len() == 1
        && !aggs.is_empty()
        && let Kind::Scan(spec) = &child.kind
        && aggs.iter().all(|(_, a)| {
            matches!(a.func, AggregateFunction::Count)
                && !a.distinct
                && match &a.expr {
                    None => true,
                    Some(Expr::Var(v)) => child.vars.contains(v),
                    _ => false,
                }
        })
        && let Some(spec) = reorder_scan(spec, keys[0])
    {
        let key = keys[0];
        let counts: Vec<VarId> = aggs.iter().map(|(v, _)| *v).collect();
        let est = child.d(key).min(child.est).max(1.0);
        let mut vars = vec![key];
        vars.extend(&counts);
        let desc = format!(
            "{} by ?{}",
            retarget_desc(&child.desc, &spec),
            ctx.var_name(key)
        );
        let mut n = Node::leaf(Kind::GroupCountScan { spec, key, counts }, vars, est, desc);
        n.cost = child.est;
        n.sorted = vec![key];
        return n;
    }
    let est = if keys.is_empty() {
        1.0
    } else {
        keys.iter()
            .map(|k| child.d(*k))
            .fold(1.0, f64::max)
            .min(child.est)
            .max(1.0)
    };
    let mut vars = keys.clone();
    vars.extend(aggs.iter().map(|(v, _)| *v));
    let desc = format!(
        "by {} aggs {}",
        keys.iter()
            .map(|v| format!("?{}", ctx.var_name(*v)))
            .collect::<Vec<_>>()
            .join(" "),
        aggs.iter()
            .map(|(v, a)| format!("?{}={:?}", ctx.var_name(*v), a.func))
            .collect::<Vec<_>>()
            .join(" ")
    );
    let dist = vars.iter().map(|&v| (v, est)).collect();
    let certain = keys
        .iter()
        .filter(|k| child.certain.contains(k))
        .copied()
        .collect();
    Node {
        kind: Kind::Group { keys, aggs },
        vars,
        certain,
        sorted: Vec::new(),
        est,
        cost: child.cost + child.est * 1.5,
        dist,
        desc,
        children: vec![child],
    }
}

// ------------------------------------------------------------------------------
// helpers
// ------------------------------------------------------------------------------

/// A scan description (`PSO ?s <p> ?o`) naming the permutation of a re-targeted scan.
fn retarget_desc(desc: &str, spec: &ScanSpec) -> String {
    match desc.split_once(' ') {
        Some((_, rest)) => format!("{} {rest}", spec.perm.name().to_uppercase()),
        None => desc.to_string(),
    }
}

/// Re-target a scan to a permutation whose first free column holds `first` (same bound
/// prefix, same variables), if one exists.
fn reorder_scan(spec: &ScanSpec, first: VarId) -> Option<ScanSpec> {
    let order = spec.perm.order();
    if spec.perm == Perm::Gspo {
        return (spec.cols.first().map(|c| c.1) == Some(first)).then(|| spec.clone());
    }
    // component (S/P/O/G) of each bound prefix entry and of each variable column
    let bound: Vec<(usize, u64)> = spec
        .prefix
        .iter()
        .enumerate()
        .map(|(i, v)| (order[i], *v))
        .collect();
    let comp_of = |kc: usize| order[kc];
    let key_comp = comp_of(spec.cols.iter().find(|(_, v)| *v == first)?.0);
    if key_comp == G {
        return None;
    }
    let mut bound_comps: Vec<usize> = bound.iter().map(|b| b.0).collect();
    bound_comps.sort_unstable();
    let perm = [
        Perm::Spo,
        Perm::Sop,
        Perm::Pso,
        Perm::Pos,
        Perm::Osp,
        Perm::Ops,
    ]
    .into_iter()
    .find(|p| {
        let o = p.order();
        let mut f: Vec<usize> = o[..bound.len()].to_vec();
        f.sort_unstable();
        f == bound_comps && o[bound.len()] == key_comp
    })?;
    if perm == spec.perm {
        return Some(spec.clone());
    }
    let po = perm.order();
    let prefix: Vec<u64> = po[..bound.len()]
        .iter()
        .map(|c| bound.iter().find(|b| b.0 == *c).unwrap().1)
        .collect();
    let remap = |kc: usize| perm.col_of(comp_of(kc));
    let mut cols: Vec<(usize, VarId)> = spec.cols.iter().map(|&(kc, v)| (remap(kc), v)).collect();
    cols.sort_by_key(|c| c.0);
    let eqs = spec
        .eqs
        .iter()
        .map(|&(a, b)| (remap(a), remap(b)))
        .collect();
    Some(ScanSpec {
        perm,
        prefix,
        cols,
        eqs,
        graph: spec.graph.clone(),
        graph_col: perm.col_of(G),
        dedup: spec.dedup,
    })
}

fn flatten_union<'g>(gp: &'g GraphPattern, out: &mut Vec<&'g GraphPattern>) {
    if let GraphPattern::Union { left, right } = gp {
        flatten_union(left, out);
        flatten_union(right, out);
    } else {
        out.push(gp);
    }
}

fn flatten_alt<'g>(p: &'g PropertyPathExpression, out: &mut Vec<&'g PropertyPathExpression>) {
    if let PropertyPathExpression::Alternative(a, b) = p {
        flatten_alt(a, out);
        flatten_alt(b, out);
    } else {
        out.push(p);
    }
}

fn has_exists(e: &Expression) -> bool {
    let mut found = false;
    fn walk(e: &Expression, f: &mut bool) {
        use Expression as E;
        match e {
            E::Exists(_) => *f = true,
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
                walk(a, f);
                walk(b, f);
            }
            E::UnaryPlus(a) | E::UnaryMinus(a) | E::Not(a) => walk(a, f),
            E::In(a, l) => {
                walk(a, f);
                l.iter().for_each(|x| walk(x, f));
            }
            E::If(a, b, c) => {
                walk(a, f);
                walk(b, f);
                walk(c, f);
            }
            E::Coalesce(l) | E::FunctionCall(_, l) => l.iter().for_each(|x| walk(x, f)),
            _ => {}
        }
    }
    walk(e, &mut found);
    found
}

/// `?x = <iri>` / `sameTerm(?x, c)` where the constant is an IRI (value = term equality).
fn equality_constant(e: &Expr, ctx: &Ctx) -> Option<(VarId, Id)> {
    let (a, b) = match e {
        Expr::Eq(a, b) => (a, b),
        Expr::SameTerm(a, b) => {
            return match (&**a, &**b) {
                (Expr::Var(v), c) | (c, Expr::Var(v)) => c
                    .const_id()
                    .filter(|c| c.tag() != Tag::Local)
                    .map(|c| (*v, c)),
                _ => None,
            };
        }
        _ => return None,
    };
    match (&**a, &**b) {
        (Expr::Var(v), c) | (c, Expr::Var(v)) => c
            .const_id()
            .filter(|c| ctx.kind(*c) == super::ctx::TermKind::Iri)
            .map(|c| (*v, c)),
        _ => None,
    }
}

fn substitute(e: &mut Expr, v: VarId, c: Id) {
    match e {
        Expr::Var(x) if *x == v => *e = Expr::Const(c),
        Expr::Bound(x) if *x == v => *e = Expr::Const(Id::from_bool(true)),
        Expr::Or(a, b)
        | Expr::And(a, b)
        | Expr::Eq(a, b)
        | Expr::SameTerm(a, b)
        | Expr::Cmp(a, b, _)
        | Expr::Arith(a, b, _) => {
            substitute(a, v, c);
            substitute(b, v, c);
        }
        Expr::Not(a) | Expr::Neg(a) | Expr::Pos(a) => substitute(a, v, c),
        Expr::In(a, l) => {
            substitute(a, v, c);
            l.iter_mut().for_each(|x| substitute(x, v, c));
        }
        Expr::If(a, b, d) => {
            substitute(a, v, c);
            substitute(b, v, c);
            substitute(d, v, c);
        }
        Expr::Coalesce(l) | Expr::Call(_, l) => l.iter_mut().for_each(|x| substitute(x, v, c)),
        _ => {}
    }
}

/// Variables certainly bound by a pattern (used for filter placement).
pub fn certain_vars(gp: &GraphPattern, ctx: &Ctx) -> FxHashSet<VarId> {
    let mut names = Vec::new();
    certain_names(gp, &mut names);
    names.iter().map(|n| ctx.var(n)).collect()
}

fn certain_names(gp: &GraphPattern, out: &mut Vec<String>) {
    use GraphPattern as GP;
    match gp {
        GP::Bgp { patterns } => {
            for t in patterns {
                for x in [&t.subject, &t.object] {
                    if let TermPattern::Variable(v) = x {
                        out.push(v.as_str().to_string());
                    }
                }
                if let NamedNodePattern::Variable(v) = &t.predicate {
                    out.push(v.as_str().to_string());
                }
            }
        }
        GP::Path {
            subject, object, ..
        } => {
            for x in [subject, object] {
                if let TermPattern::Variable(v) = x {
                    out.push(v.as_str().to_string());
                }
            }
        }
        GP::Join { left, right } => {
            certain_names(left, out);
            certain_names(right, out);
        }
        GP::LeftJoin { left, .. } | GP::Minus { left, .. } => certain_names(left, out),
        GP::Filter { inner, .. }
        | GP::Distinct { inner }
        | GP::Reduced { inner }
        | GP::Slice { inner, .. }
        | GP::OrderBy { inner, .. } => certain_names(inner, out),
        GP::Graph { name, inner } => {
            if let NamedNodePattern::Variable(v) = name {
                out.push(v.as_str().to_string());
            }
            certain_names(inner, out);
        }
        GP::Extend { inner, .. } => certain_names(inner, out),
        GP::Project { inner, variables } => {
            let mut inner_names = Vec::new();
            certain_names(inner, &mut inner_names);
            out.extend(
                inner_names
                    .into_iter()
                    .filter(|n| variables.iter().any(|v| v.as_str() == n)),
            );
        }
        GP::Union { left, right } => {
            let (mut a, mut b) = (Vec::new(), Vec::new());
            certain_names(left, &mut a);
            certain_names(right, &mut b);
            out.extend(a.into_iter().filter(|n| b.contains(n)));
        }
        GP::Group { variables, .. } => {
            let _ = variables;
        }
        _ => {}
    }
}

/// All variable names mentioned in a pattern (for EXISTS / SERVICE).
pub fn collect_pattern_vars(gp: &GraphPattern, out: &mut Vec<String>) {
    gp.on_in_scope_variable(|v| out.push(v.as_str().to_string()));
    // variables used only in filters also matter for substitution
    fn walk(gp: &GraphPattern, out: &mut Vec<String>) {
        use GraphPattern as GP;
        match gp {
            GP::Filter { expr, inner } => {
                expr_vars(expr, out);
                walk(inner, out);
            }
            GP::LeftJoin {
                left,
                right,
                expression,
            } => {
                if let Some(e) = expression {
                    expr_vars(e, out);
                }
                walk(left, out);
                walk(right, out);
            }
            GP::Join { left, right } | GP::Union { left, right } | GP::Minus { left, right } => {
                walk(left, out);
                walk(right, out);
            }
            GP::Extend {
                inner, expression, ..
            } => {
                expr_vars(expression, out);
                walk(inner, out);
            }
            GP::Graph { inner, .. }
            | GP::Distinct { inner }
            | GP::Reduced { inner }
            | GP::Slice { inner, .. }
            | GP::OrderBy { inner, .. } => walk(inner, out),
            _ => {}
        }
    }
    walk(gp, out);
}

fn expr_vars(e: &Expression, out: &mut Vec<String>) {
    use Expression as E;
    match e {
        E::Variable(v) | E::Bound(v) => out.push(v.as_str().to_string()),
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
            expr_vars(a, out);
            expr_vars(b, out);
        }
        E::UnaryPlus(a) | E::UnaryMinus(a) | E::Not(a) => expr_vars(a, out),
        E::In(a, l) => {
            expr_vars(a, out);
            l.iter().for_each(|x| expr_vars(x, out));
        }
        E::If(a, b, c) => {
            expr_vars(a, out);
            expr_vars(b, out);
            expr_vars(c, out);
        }
        E::Coalesce(l) | E::FunctionCall(_, l) => l.iter().for_each(|x| expr_vars(x, out)),
        E::Exists(p) => collect_pattern_vars(p, out),
        _ => {}
    }
}

/// Compact term rendering for plan descriptions.
pub fn short(t: &Term) -> String {
    match t {
        Term::NamedNode(n) => {
            let s = n.as_str();
            for (p, ns) in [
                ("rdf:", "http://www.w3.org/1999/02/22-rdf-syntax-ns#"),
                ("rdfs:", "http://www.w3.org/2000/01/rdf-schema#"),
                ("owl:", "http://www.w3.org/2002/07/owl#"),
                ("xsd:", "http://www.w3.org/2001/XMLSchema#"),
            ] {
                if let Some(l) = s.strip_prefix(ns) {
                    return format!("{p}{l}");
                }
            }
            format!("<{s}>")
        }
        Term::Literal(l) if l.datatype() == xsd::INTEGER => l.value().to_string(),
        t => {
            let s = t.to_string();
            if s.len() > 60 {
                format!(
                    "{}…",
                    &s[..s.char_indices().nth(57).map_or(s.len(), |x| x.0)]
                )
            } else {
                s
            }
        }
    }
}

/// Evaluate `EXISTS { pattern }` for one outer binding (substitution semantics).
pub fn eval_exists(ctx: &Ctx, spec: &ExistsSpec, key: &[Id]) -> Result<bool> {
    let mut p = Planner::new(ctx);
    for (v, id) in spec.vars.iter().zip(key) {
        if !id.is_undef() {
            p.subst.insert(*v, *id);
        }
    }
    let graph = match &spec.graph {
        ActiveGraph::Var(v) => match p.subst.get(v) {
            Some(id) => ActiveGraph::Named(*id),
            None => spec.graph.clone(),
        },
        g => g.clone(),
    };
    let node = p.plan(&spec.pattern, &graph, Vec::new())?;
    let node = slice(node, 0, Some(1));
    let (t, _) = super::exec::execute(ctx, &node)?;
    Ok(!t.is_empty())
}

/// Literal helper for tests.
pub fn lit_int(i: i64) -> Term {
    Term::Literal(Literal::new_typed_literal(i.to_string(), xsd::INTEGER))
}
