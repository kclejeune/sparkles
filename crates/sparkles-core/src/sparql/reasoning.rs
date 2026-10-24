//! Query-time reasoning (spec F17): a query derives, from the snapshot it reads, the facts
//! its patterns need, through a reasoner that the options carry.
//!
//! The engine does not depend on the reasoner. [`QueryOptions::reasoning`] holds a
//! [`ReasoningProvider`], which `sparkles-reasoner` implements. Before the query is
//! planned, [`apply`] collects the query's triple patterns, asks the provider which ones
//! rules can give facts for (the intensional ones), orders the patterns of each basic graph
//! pattern so that bound positions come first, and asks for the facts of each intensional
//! pattern with its bound positions as a demand. A pattern whose variable an earlier
//! pattern of its group binds gets the values of that variable as seeds: the earlier
//! patterns are evaluated first, over the snapshot with the facts derived so far.
//!
//! The derived facts are inserted into the default graph of a view of the caller's
//! snapshot, which the query then reads as it reads any snapshot, with every index and
//! join of the planner. The view is not cached by the result cache. EXPLAIN shows a
//! `Reason` node with a `Derive` node per derived pattern above the query's plan.

use super::ctx::Ctx;
use super::exec::{self, PlanInfo};
use super::plan::{ActiveGraph, Planner};
use super::{QueryOptions, make_ctx};
use crate::error::{Error, Result};
use crate::id::{Id, Tag};
use crate::index::Perm;
use crate::store::Snapshot;
use oxrdf::{Term, Variable};
use rustc_hash::FxHashMap;
use spargebra::algebra::{
    AggregateExpression, Expression, GraphPattern, OrderExpression, PropertyPathExpression,
    QueryDataset,
};
use spargebra::term::{NamedNodePattern, TermPattern, TriplePattern};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Instant;

/// A position of a pattern as a demand.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum DemandTerm {
    /// free: any value
    Var,
    /// a constant of the query
    Const(Term),
    /// bound to the seeds given with the demand (the values an earlier join gave)
    Seeded,
}

/// The facts a pattern asks for: its constants, and the position bound by seeds, if any.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Demand {
    pub s: DemandTerm,
    pub p: DemandTerm,
    pub o: DemandTerm,
}

/// What a provider says about a demand after [`ReasoningSession::prepare`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DemandInfo {
    /// the predicate and the adornment, such as `rdf:type^fbb`
    pub adornment: String,
    /// the rules in the demand's dependency cone
    pub rules: Vec<String>,
}

/// A term that a derivation made and the store does not have.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LocalTerm {
    /// a vocabulary key (an IRI or a literal)
    Key(Vec<u8>),
    /// a blank node
    BNode,
}

/// The facts one call of [`ReasoningSession::derive`] added.
#[derive(Clone, Debug, Default)]
pub struct Derived {
    /// subject, predicate and object; an id tagged [`Tag::Local`] names a term of
    /// `locals` (here or in an earlier call of the same session)
    pub facts: Vec<[u64; 3]>,
    /// the local terms first used by these facts
    pub locals: Vec<(u64, LocalTerm)>,
    /// none of the facts is in the snapshot's default graph, so the view needs no probe
    pub absent_from_default: bool,
}

/// What a session did for one demand.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DemandReport {
    /// the derived facts that match the demand
    pub derived: u64,
    /// whether its table came from the cache
    pub cached: bool,
    /// the seeds it was derived with
    pub seeds: u64,
    /// whether it was derived at all
    pub used: bool,
}

/// What a session did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Report {
    /// derived facts of the query, without the magic facts
    pub derived: u64,
    /// tables the query used, and how many came from the cache
    pub tables: u64,
    pub cached: u64,
    pub demands: Vec<DemandReport>,
    pub warnings: Vec<String>,
}

/// Everything a session needs to know about the query that starts it.
pub struct SessionRequest {
    /// the caller's view of the data
    pub snap: Arc<Snapshot>,
    /// whether the caller may read a graph (`None`: every graph)
    pub readable: Option<Arc<dyn Fn(Id) -> bool + Send + Sync>>,
    /// fails with [`Error::Timeout`] or [`Error::Cancelled`] when the query must stop
    pub stop: Arc<dyn Fn() -> Result<()> + Send + Sync>,
    /// the query's memory budget, in bytes
    pub max_memory: Option<u64>,
    /// neither read nor fill the cache of derived tables
    pub no_cache: bool,
}

/// A reasoner that derives facts for queries (see the module documentation).
pub trait ReasoningProvider: Send + Sync {
    /// The mode and profile, for EXPLAIN, such as `demand, profile owl-rl`.
    fn label(&self) -> String;
    /// A session for one query.
    fn start(&self, req: SessionRequest) -> Result<Box<dyn ReasoningSession>>;
}

/// The derivations of one query.
pub trait ReasoningSession: Send {
    /// Whether rules can give facts that match the pattern (`None` is a variable).
    fn intensional(&self, s: Option<&Term>, p: Option<&Term>, o: Option<&Term>) -> bool;
    /// Make the program for the demands the query can ask for. Demand `i` of later
    /// calls is `demands[i]`.
    fn prepare(&mut self, demands: &[Demand]) -> Result<Vec<DemandInfo>>;
    /// Derive the facts of the demands, each with the seeds of its seeded position (an
    /// empty list for a demand without one), and return the facts not returned before.
    fn derive(&mut self, seeds: Vec<(usize, Vec<u64>)>) -> Result<Derived>;
    /// What the session did so far.
    fn report(&self) -> Report;
}

impl std::fmt::Debug for dyn ReasoningProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "ReasoningProvider({})", self.label())
    }
}

/// Refuse the query forms that query-time reasoning cannot answer in full. A DESCRIBE
/// reads the facts of the resources it describes outside its WHERE pattern, so no demand
/// asks for their derived facts, and its answer would leave them out without saying so
/// (Codex review F17-P1 #8).
pub fn check_form(q: &spargebra::Query) -> Result<()> {
    if matches!(q, spargebra::Query::Describe { .. }) {
        return Err(Error::Unsupported(
            "DESCRIBE is not supported with reasoning=demand; use CONSTRUCT with the \
             patterns of the description, or a materialized dataset"
                .into(),
        ));
    }
    Ok(())
}

/// What query-time reasoning did for a query, kept on its context.
#[derive(Clone, Debug, Default)]
pub struct Applied {
    pub label: String,
    pub report: Report,
    /// a `Derive` node per derived pattern
    pub nodes: Vec<PlanInfo>,
    /// the dataset has RDFS on read, which did not apply
    pub rdfs_skipped: bool,
    /// time spent deriving, in milliseconds
    pub millis: f64,
}

impl Applied {
    /// The value of the `Sparkles-Reasoning` response header.
    pub fn header(&self) -> String {
        format!(
            "demand; derived={}; tables={}; cached={}",
            self.report.derived, self.report.tables, self.report.cached
        )
    }

    /// `plan` under a `Reason` node with the `Derive` nodes before it.
    pub fn wrap(&self, plan: PlanInfo) -> PlanInfo {
        let mut description = format!("query-time reasoning: {}", self.label);
        if self.rdfs_skipped {
            description.push_str("; RDFS on read did not apply");
        }
        let mut counters = serde_json::Map::new();
        counters.insert("derived".into(), self.report.derived.into());
        counters.insert("tables".into(), self.report.tables.into());
        counters.insert("cached".into(), self.report.cached.into());
        let mut children = self.nodes.clone();
        let warnings = plan.warnings.clone();
        let (estimated_rows, estimated_cost, actual_rows, time_ms) = (
            plan.estimated_rows,
            plan.estimated_cost,
            plan.actual_rows,
            plan.time_ms,
        );
        let columns = plan.columns.clone();
        let mut plan = plan;
        plan.warnings = Vec::new();
        children.push(plan);
        PlanInfo {
            operator: "Reason".into(),
            description,
            columns,
            estimated_rows,
            estimated_cost,
            actual_rows,
            time_ms: time_ms + self.millis,
            counters: Some(counters),
            warnings,
            children,
            ..Default::default()
        }
    }
}

/// The most seeds an earlier join may give a pattern. A pattern whose join gives more is
/// derived with its constants alone, since seeding then saves little (spec F17 §3.3).
pub const MAX_SEEDS: usize = 10_000;

/// One triple pattern of the query, as the derivation sees it.
struct Pat {
    /// the group (basic graph pattern) it belongs to, and its place in the group's order
    group: usize,
    t: TriplePattern,
    /// the demand with seeds from the earlier patterns of the group, and the one with the
    /// constants alone; an index into the demands
    seeded: Option<(usize, Variable)>,
    plain: Option<usize>,
}

/// Collects the patterns that can read derived facts: those outside `GRAPH` and
/// `SERVICE`, in groups, and the predicates of property paths.
#[derive(Default)]
struct Collect {
    groups: Vec<Vec<TriplePattern>>,
    /// the IRIs of property path steps (`None`: a negated property set, any predicate)
    paths: Vec<Option<oxrdf::NamedNode>>,
}

impl Collect {
    fn walk(&mut self, gp: &GraphPattern) {
        use GraphPattern as G;
        match gp {
            G::Bgp { patterns } => {
                if !patterns.is_empty() {
                    self.groups.push(patterns.clone());
                }
            }
            G::Path { path, .. } => self.path(path),
            G::Join { left, right }
            | G::Lateral { left, right }
            | G::Union { left, right }
            | G::Minus { left, right }
            | G::SemiJoin { left, right }
            | G::AntiJoin { left, right } => {
                self.walk(left);
                self.walk(right);
            }
            G::LeftJoin {
                left,
                right,
                expression,
            } => {
                self.walk(left);
                self.walk(right);
                if let Some(e) = expression {
                    self.expr(e);
                }
            }
            G::Filter { expr, inner } => {
                self.expr(expr);
                self.walk(inner);
            }
            G::Extend {
                inner, expression, ..
            }
            | G::Assign {
                inner, expression, ..
            }
            | G::Unfold {
                inner, expression, ..
            } => {
                self.expr(expression);
                self.walk(inner);
            }
            G::Project { inner, .. }
            | G::Distinct { inner }
            | G::Reduced { inner }
            | G::Slice { inner, .. } => self.walk(inner),
            G::OrderBy { inner, expression } => {
                for o in expression {
                    match o {
                        OrderExpression::Asc(e) | OrderExpression::Desc(e) => self.expr(e),
                    }
                }
                self.walk(inner);
            }
            G::Group {
                inner, aggregates, ..
            } => {
                for (_, a) in aggregates {
                    match a {
                        AggregateExpression::CountSolutions { .. } => {}
                        AggregateExpression::FunctionCall { expr, .. } => self.expr(expr),
                        AggregateExpression::Fold { expr, value, .. } => {
                            self.expr(expr);
                            if let Some(v) = value {
                                self.expr(v);
                            }
                        }
                    }
                }
                self.walk(inner);
            }
            // derived facts are in the default graph; SERVICE runs remotely
            G::Graph { .. } | G::Service { .. } | G::Values { .. } => {}
        }
    }

    fn path(&mut self, p: &PropertyPathExpression) {
        use PropertyPathExpression as P;
        match p {
            P::NamedNode(n) => self.paths.push(Some(n.clone())),
            P::Reverse(a) | P::ZeroOrMore(a) | P::OneOrMore(a) | P::ZeroOrOne(a) => self.path(a),
            P::Sequence(a, b) | P::Alternative(a, b) => {
                self.path(a);
                self.path(b);
            }
            P::NegatedPropertySet(_) => self.paths.push(None),
            #[allow(unreachable_patterns)]
            _ => self.paths.push(None),
        }
    }

    fn expr(&mut self, e: &Expression) {
        use Expression as E;
        match e {
            E::Exists(p) => self.walk(p),
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
                self.expr(a);
                self.expr(b);
            }
            E::In(a, l) => {
                self.expr(a);
                l.iter().for_each(|x| self.expr(x));
            }
            E::UnaryPlus(a) | E::UnaryMinus(a) | E::Not(a) => self.expr(a),
            E::If(a, b, c) => {
                self.expr(a);
                self.expr(b);
                self.expr(c);
            }
            E::Coalesce(l) | E::FunctionCall(_, l) => l.iter().for_each(|x| self.expr(x)),
            E::NamedNode(_) | E::Literal(_) | E::Variable(_) | E::Bound(_) => {}
        }
    }
}

fn const_term(t: &TermPattern) -> Option<Term> {
    match t {
        TermPattern::NamedNode(n) => Some(Term::NamedNode(n.clone())),
        TermPattern::Literal(l) => Some(Term::Literal(l.clone())),
        _ => None,
    }
}

fn const_pred(p: &NamedNodePattern) -> Option<Term> {
    match p {
        NamedNodePattern::NamedNode(n) => Some(Term::NamedNode(n.clone())),
        NamedNodePattern::Variable(_) => None,
    }
}

/// The variable a position joins on: a variable, or a blank node of the pattern (which
/// is a variable scoped to its group).
fn join_var(t: &TermPattern) -> Option<Variable> {
    match t {
        TermPattern::Variable(v) => Some(v.clone()),
        TermPattern::BlankNode(b) => Some(Variable::new_unchecked(format!("__bn_{}", b.as_str()))),
        _ => None,
    }
}

fn pattern_vars(t: &TriplePattern) -> Vec<Variable> {
    let mut out = Vec::new();
    out.extend(join_var(&t.subject));
    if let NamedNodePattern::Variable(v) = &t.predicate {
        out.push(v.clone());
    }
    out.extend(join_var(&t.object));
    out
}

/// `t` with its blank nodes as the variables [`join_var`] names.
fn with_vars(t: &TriplePattern) -> TriplePattern {
    let f = |x: &TermPattern| match x {
        TermPattern::BlankNode(_) => TermPattern::Variable(join_var(x).expect("blank node")),
        x => x.clone(),
    };
    TriplePattern {
        subject: f(&t.subject),
        predicate: t.predicate.clone(),
        object: f(&t.object),
    }
}

/// The text of a pattern for EXPLAIN.
fn pattern_text(t: &TriplePattern) -> String {
    format!("{} {} {}", t.subject, t.predicate, t.object)
}

/// Derive the facts the query needs and give the context a view of its snapshot with
/// them. With `execute` false (EXPLAIN), only the `Derive` nodes are made.
#[allow(clippy::too_many_arguments)]
pub(crate) fn apply(
    ctx: &mut Ctx,
    provider: &Arc<dyn ReasoningProvider>,
    pattern: &GraphPattern,
    opts: &QueryOptions,
    dataset: Option<&QueryDataset>,
    base: Option<&oxiri::Iri<String>>,
    rdfs_skipped: bool,
    execute: bool,
) -> Result<()> {
    let t0 = Instant::now();
    // the derived facts go to the store's default graph, which the query must read
    let reads_default = match &ctx.dataset.default {
        None => !ctx.dataset.union_default && !ctx.snap.union_default_graph,
        Some(l) => l.contains(&Id::DEFAULT_GRAPH),
    };
    if !reads_default {
        return Err(Error::Unsupported(
            "query-time reasoning adds the derived facts to the store's default graph, \
             which this query does not read (its dataset names other graphs, or the default \
             graph is the union of the named graphs)"
                .into(),
        ));
    }
    let mut collect = Collect::default();
    collect.walk(pattern);
    let stop = {
        let cancel: Arc<AtomicBool> = ctx.cancel.clone();
        let cancellation = ctx.cancellation.clone();
        let deadline = ctx.deadline;
        Arc::new(move || -> Result<()> {
            if cancel.load(std::sync::atomic::Ordering::Relaxed) {
                return Err(Error::Cancelled);
            }
            if let Some(c) = &cancellation {
                c.check()?;
            }
            if deadline.is_some_and(|d| Instant::now() >= d) {
                return Err(Error::Timeout);
            }
            Ok(())
        }) as Arc<dyn Fn() -> Result<()> + Send + Sync>
    };
    let readable: Option<Arc<dyn Fn(Id) -> bool + Send + Sync>> = match &ctx.graphs {
        Some(a) if !a.reads_all() => {
            let a = a.clone();
            let snap = ctx.snap.clone();
            Some(Arc::new(move |g: Id| a.readable_id(&snap, g)))
        }
        _ => None,
    };
    let mut session = provider.start(SessionRequest {
        snap: ctx.snap.clone(),
        readable,
        stop,
        max_memory: opts.max_memory_bytes,
        no_cache: opts.no_cache,
    })?;
    // ---- demands, in the order of each group
    let mut demands: Vec<Demand> = Vec::new();
    let mut index: FxHashMap<Demand, usize> = FxHashMap::default();
    let mut add = |d: Demand, demands: &mut Vec<Demand>| -> usize {
        *index.entry(d.clone()).or_insert_with(|| {
            demands.push(d);
            demands.len() - 1
        })
    };
    let mut pats: Vec<Pat> = Vec::new();
    for (gi, group) in collect.groups.iter().enumerate() {
        let intensional: Vec<bool> = group
            .iter()
            .map(|t| {
                session.intensional(
                    const_term(&t.subject).as_ref(),
                    const_pred(&t.predicate).as_ref(),
                    const_term(&t.object).as_ref(),
                )
            })
            .collect();
        // the stored facts each pattern's constants match: of an intensional pattern with
        // none, its derived facts may be many, so it counts as the largest
        let estimates: Vec<u64> = group
            .iter()
            .zip(&intensional)
            .map(|(t, &int)| match stored_count(&ctx.snap, t) {
                0 if int => u64::MAX,
                n => n,
            })
            .collect();
        // bound positions first, then patterns read without derivation, then the fewest
        // stored facts. The class of an `rdf:type` pattern counts after a bound subject or
        // object of any other, since a class has many instances.
        let mut left: Vec<usize> = (0..group.len()).collect();
        let mut bound: Vec<Variable> = Vec::new();
        while !left.is_empty() {
            let score = |i: usize| {
                let t = &group[i];
                let b = |x: &TermPattern| {
                    const_term(x).is_some() || join_var(x).is_some_and(|v| bound.contains(&v))
                };
                let class = matches!(&t.predicate, NamedNodePattern::NamedNode(p) if p.as_ref() == oxrdf::vocab::rdf::TYPE);
                let ends = b(&t.subject) as u8 + (b(&t.object) && !class) as u8;
                let n = b(&t.subject) as u8
                    + matches!(&t.predicate, NamedNodePattern::NamedNode(_)) as u8
                    + b(&t.object) as u8;
                (
                    ends,
                    n,
                    !intensional[i] as u8,
                    std::cmp::Reverse(estimates[i]),
                )
            };
            let k = (0..left.len())
                .max_by(|&a, &b| score(left[a]).cmp(&score(left[b])).then(b.cmp(&a)))
                .expect("patterns left");
            let i = left.remove(k);
            let t = &group[i];
            if intensional[i] {
                let pos = |x: &TermPattern| match const_term(x) {
                    Some(c) => DemandTerm::Const(c),
                    None => DemandTerm::Var,
                };
                let p = match const_pred(&t.predicate) {
                    Some(c) => DemandTerm::Const(c),
                    None => DemandTerm::Var,
                };
                let plain = Demand {
                    s: pos(&t.subject),
                    p: p.clone(),
                    o: pos(&t.object),
                };
                // the subject, else the object, bound by an earlier pattern: seeded
                let seeded_var = [(&t.subject, true), (&t.object, false)]
                    .into_iter()
                    .find_map(|(x, s)| join_var(x).filter(|v| bound.contains(v)).map(|v| (v, s)));
                let seeded = seeded_var.map(|(v, subject)| {
                    let mut d = plain.clone();
                    if subject {
                        d.s = DemandTerm::Seeded;
                    } else {
                        d.o = DemandTerm::Seeded;
                    }
                    (add(d, &mut demands), v)
                });
                // a seeded pattern uses its plain demand only when the seeds are too many
                let plain_idx = add(plain, &mut demands);
                pats.push(Pat {
                    group: gi,
                    t: t.clone(),
                    seeded,
                    plain: Some(plain_idx),
                });
            } else {
                pats.push(Pat {
                    group: gi,
                    t: t.clone(),
                    seeded: None,
                    plain: None,
                });
            }
            for v in pattern_vars(t) {
                if !bound.contains(&v) {
                    bound.push(v);
                }
            }
        }
    }
    // property paths: every step's predicate, free
    let mut path_demands: Vec<usize> = Vec::new();
    for p in &collect.paths {
        let pt = p.as_ref().map(|n| Term::NamedNode(n.clone()));
        if session.intensional(None, pt.as_ref(), None) {
            let d = Demand {
                s: DemandTerm::Var,
                p: pt.map_or(DemandTerm::Var, DemandTerm::Const),
                o: DemandTerm::Var,
            };
            path_demands.push(add(d, &mut demands));
        }
    }
    let infos = if demands.is_empty() {
        Vec::new()
    } else {
        session.prepare(&demands)?
    };
    let label = provider.label();
    let node = |d: usize, text: &str, how: &str, report: Option<&DemandReport>| {
        let info = infos.get(d).cloned().unwrap_or_default();
        let mut counters = serde_json::Map::new();
        counters.insert("adornment".into(), info.adornment.clone().into());
        counters.insert(
            "rules".into(),
            serde_json::Value::Array(info.rules.iter().map(|r| r.clone().into()).collect()),
        );
        counters.insert("seeds".into(), how.into());
        if let Some(r) = report {
            counters.insert("derived".into(), r.derived.into());
            counters.insert("seedCount".into(), r.seeds.into());
        }
        PlanInfo {
            operator: "Derive".into(),
            description: format!("{text} as {}", info.adornment),
            estimated_rows: 0.0,
            actual_rows: report.map_or(-1, |r| r.derived as i64),
            cached: report.is_some_and(|r| r.cached),
            counters: Some(counters),
            ..Default::default()
        }
    };
    if !execute {
        let mut nodes = Vec::new();
        for p in &pats {
            if let Some(d) = p.seeded.as_ref().map(|s| s.0).or(p.plain) {
                let how = if p.seeded.is_some() {
                    "join"
                } else {
                    "constants"
                };
                nodes.push(node(d, &pattern_text(&p.t), how, None));
            }
        }
        for &d in &path_demands {
            nodes.push(node(d, "property path step", "constants", None));
        }
        ctx.reasoning = Some(Arc::new(Applied {
            label,
            report: Report::default(),
            nodes,
            rdfs_skipped,
            millis: t0.elapsed().as_secs_f64() * 1000.0,
        }));
        return Ok(());
    }
    // ---- round 0: the demands with constants alone
    let mut view = Overlay::new(ctx.snap.clone());
    let mut first: Vec<(usize, Vec<u64>)> = Vec::new();
    for p in &pats {
        if p.seeded.is_none()
            && let Some(d) = p.plain
            && !first.iter().any(|(x, _)| *x == d)
        {
            first.push((d, Vec::new()));
        }
    }
    for &d in &path_demands {
        if !first.iter().any(|(x, _)| *x == d) {
            first.push((d, Vec::new()));
        }
    }
    if !first.is_empty() {
        let out = session.derive(first)?;
        view.add(ctx, out)?;
    }
    // ---- seeded patterns, in the order of their groups
    let mut used_plain: Vec<usize> = Vec::new();
    for (k, p) in pats.iter().enumerate() {
        let Some((d, var)) = &p.seeded else {
            continue;
        };
        // the earlier patterns of the group that connect to the variable
        let earlier: Vec<&TriplePattern> = pats[..k]
            .iter()
            .filter(|q| q.group == p.group)
            .map(|q| &q.t)
            .collect();
        let mut reach: Vec<Variable> = vec![var.clone()];
        let mut chosen: Vec<TriplePattern> = Vec::new();
        let mut taken = vec![false; earlier.len()];
        loop {
            let mut grew = false;
            for (i, t) in earlier.iter().enumerate() {
                if taken[i] {
                    continue;
                }
                let vs = pattern_vars(t);
                if vs.iter().any(|v| reach.contains(v)) {
                    taken[i] = true;
                    grew = true;
                    chosen.push(with_vars(t));
                    for v in vs {
                        if !reach.contains(&v) {
                            reach.push(v);
                        }
                    }
                }
            }
            if !grew {
                break;
            }
        }
        let seeds = seed_values(ctx, &view, &chosen, var, opts, dataset, base)?;
        match seeds.and_then(|s| view.reasoner_ids(&s)) {
            Some(ids) => {
                let out = session.derive(vec![(*d, ids)])?;
                view.add(ctx, out)?;
            }
            None => {
                let plain = p.plain.expect("an intensional pattern has a plain demand");
                if !used_plain.contains(&plain) {
                    used_plain.push(plain);
                    let out = session.derive(vec![(plain, Vec::new())])?;
                    view.add(ctx, out)?;
                }
            }
        }
    }
    let report = session.report();
    let mut nodes = Vec::new();
    for p in &pats {
        let chosen = match (&p.seeded, p.plain) {
            (Some((d, _)), Some(pl)) => {
                if report.demands.get(*d).is_some_and(|r| r.used) {
                    Some((*d, "join"))
                } else {
                    Some((pl, "constants (too many seeds)"))
                }
            }
            (None, Some(pl)) => Some((pl, "constants")),
            _ => None,
        };
        if let Some((d, how)) = chosen {
            nodes.push(node(d, &pattern_text(&p.t), how, report.demands.get(d)));
        }
    }
    for &d in &path_demands {
        nodes.push(node(
            d,
            "property path step",
            "constants",
            report.demands.get(d),
        ));
    }
    for w in &report.warnings {
        ctx.warn(super::ctx::PlanWarning {
            code: "reasoning",
            message: w.clone(),
        });
    }
    ctx.local_facts = view.has_locals();
    ctx.reasoning_memory.append(&mut view.leases);
    ctx.snap = view.finish();
    ctx.reasoning = Some(Arc::new(Applied {
        label,
        report,
        nodes,
        rdfs_skipped,
        millis: t0.elapsed().as_secs_f64() * 1000.0,
    }));
    Ok(())
}

/// The stored facts that match the constants of `t`, in all graphs, as the index counts
/// them (`u64::MAX` when it has none).
fn stored_count(snap: &Snapshot, t: &TriplePattern) -> u64 {
    let id = |x: Option<Term>| x.map(|t| snap.lookup_term(&t).map(|i| i.0));
    let (s, p, o) = (
        id(const_term(&t.subject)),
        id(const_pred(&t.predicate)),
        id(const_term(&t.object)),
    );
    if [s, p, o].iter().any(|x| matches!(x, Some(None))) {
        return 0;
    }
    let (s, p, o) = (s.flatten(), p.flatten(), o.flatten());
    let (perm, prefix) = match (s, p, o) {
        (Some(s), Some(p), Some(o)) => (Perm::Spo, vec![s, p, o]),
        (Some(s), Some(p), None) => (Perm::Spo, vec![s, p]),
        (Some(s), None, Some(o)) => (Perm::Sop, vec![s, o]),
        (None, Some(p), Some(o)) => (Perm::Pos, vec![p, o]),
        (Some(s), None, None) => (Perm::Spo, vec![s]),
        (None, Some(p), None) => (Perm::Pso, vec![p]),
        (None, None, Some(o)) => (Perm::Osp, vec![o]),
        (None, None, None) => return u64::MAX,
    };
    snap.count(perm, &prefix).unwrap_or(u64::MAX)
}

/// The distinct values of `var` over the patterns `chosen`, read from the view with the
/// facts derived so far, or `None` when there are more than [`MAX_SEEDS`].
fn seed_values(
    ctx: &Ctx,
    view: &Overlay,
    chosen: &[TriplePattern],
    var: &Variable,
    opts: &QueryOptions,
    dataset: Option<&QueryDataset>,
    base: Option<&oxiri::Iri<String>>,
) -> Result<Option<Vec<Id>>> {
    let gp = GraphPattern::Slice {
        inner: Box::new(GraphPattern::Distinct {
            inner: Box::new(GraphPattern::Project {
                inner: Box::new(GraphPattern::Bgp {
                    patterns: chosen.to_vec(),
                }),
                variables: vec![var.clone()],
            }),
        }),
        start: 0,
        length: Some(MAX_SEEDS + 1),
    };
    let sub_opts = QueryOptions {
        reasoning: None,
        rdfs: None,
        default_graph_extra: Vec::new(),
        graphs: None,
        initial_bindings: Vec::new(),
        no_cache: true,
        ..opts.clone()
    };
    let mut sub = make_ctx(ctx.snap.clone(), &sub_opts, dataset, base)?;
    sub.dataset = ctx.dataset.clone();
    sub.graphs = ctx.graphs.clone();
    sub.snap = view.current();
    sub.share_local(ctx);
    sub.local_facts = view.has_locals();
    let sub = Arc::new(sub);
    let mut planner = Planner::new(&sub);
    let node = planner.plan(&gp, &ActiveGraph::Default, Vec::new())?;
    let (table, _) = exec::execute(&sub, &node)?;
    let v = sub.var(var.as_str());
    let Some(c) = table.col_of(v) else {
        return Ok(Some(Vec::new()));
    };
    if table.len > MAX_SEEDS {
        return Ok(None);
    }
    Ok(Some(
        table.cols[c]
            .iter()
            .copied()
            .filter(|id| *id != Id::UNDEF)
            .collect(),
    ))
}

/// The view of the caller's snapshot with the derived facts in its default graph, and
/// the ids the derived terms have in the query.
struct Overlay {
    snap: Arc<Snapshot>,
    /// the query's id of each local term of the session, and the reverse
    to_query: FxHashMap<u64, Id>,
    to_session: FxHashMap<Id, u64>,
    /// the memory charged for the overlay's facts, held while the query runs
    leases: Vec<super::query_memory::QueryMemoryLease>,
}

/// The bytes one derived quad holds in the overlay: a 32-byte key in each of the seven
/// permutations of the delta, with the node overhead of the persistent sets.
const OVERLAY_QUAD_BYTES: u64 = 7 * 64;

impl Overlay {
    fn new(snap: Arc<Snapshot>) -> Overlay {
        Overlay {
            snap,
            to_query: FxHashMap::default(),
            to_session: FxHashMap::default(),
            leases: Vec::new(),
        }
    }

    fn add(&mut self, ctx: &Ctx, d: Derived) -> Result<()> {
        for (id, t) in &d.locals {
            let q = match t {
                LocalTerm::Key(k) => ctx.intern_key(k),
                LocalTerm::BNode => ctx.fresh_bnode(),
            };
            self.to_query.insert(*id, q);
            self.to_session.insert(q, *id);
        }
        if d.facts.is_empty() {
            return Ok(());
        }
        let map = |x: u64| -> Id {
            if Id(x).tag() == Tag::Local {
                self.to_query.get(&x).copied().unwrap_or(Id::UNDEF)
            } else {
                Id(x)
            }
        };
        let quads: Vec<[Id; 4]> = d
            .facts
            .iter()
            .map(|t| [map(t[0]), map(t[1]), map(t[2]), Id::DEFAULT_GRAPH])
            .filter(|q| !q[..3].contains(&Id::UNDEF))
            .collect();
        // The facts come from the session's tables, which a cache hit shares without a
        // charge, so the overlay's growth is charged here (Codex review F17-P1 #4). Tables
        // of several demands can hold the same facts, which the overlay keeps once. The
        // facts the overlay does not hold yet are reserved before it grows, and the
        // reservation then shrinks to the keys it grew by, which leaves out those the
        // base index holds.
        let spo = Perm::Spo.index();
        let held = &self.snap.delta.ins[spo];
        let mut seen: rustc_hash::FxHashSet<_> = rustc_hash::FxHashSet::default();
        let upper = quads
            .iter()
            .filter(|q| {
                let k = Perm::Spo.to_key(q);
                !held.contains(&k) && seen.insert(k)
            })
            .count();
        drop(seen);
        let bytes = |n: usize| (n as u64).saturating_mul(OVERLAY_QUAD_BYTES);
        let mut lease = ctx.output_lease(bytes(upper))?;
        let before = held.len();
        let snap = self.snap.with_overlay(&quads, d.absent_from_default)?;
        let grown = snap.delta.ins[spo].len().saturating_sub(before);
        if grown < upper {
            drop(lease);
            lease = ctx.output_lease(bytes(grown))?;
        }
        self.leases.push(lease);
        self.snap = Arc::new(snap);
        Ok(())
    }

    /// The session's ids of query ids: the same for stored terms, the session's own for
    /// derived terms. `None` when an id is neither.
    fn reasoner_ids(&self, ids: &[Id]) -> Option<Vec<u64>> {
        ids.iter()
            .map(|id| {
                let minted = id.tag() == Tag::BNode && id.payload() & Id::LOCAL_BNODE_BIT != 0;
                if id.tag() == Tag::Local || minted {
                    self.to_session.get(id).copied()
                } else {
                    Some(id.0)
                }
            })
            .collect()
    }

    fn current(&self) -> Arc<Snapshot> {
        self.snap.clone()
    }

    fn has_locals(&self) -> bool {
        self.to_query.values().any(|q| q.tag() == Tag::Local)
    }

    fn finish(self) -> Arc<Snapshot> {
        self.snap
    }
}
