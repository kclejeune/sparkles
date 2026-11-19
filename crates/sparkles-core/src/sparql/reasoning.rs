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
#[derive(Clone, Debug, Default, PartialEq)]
pub struct DemandInfo {
    /// the predicate and the adornment, such as `rdf:type^fbb`
    pub adornment: String,
    /// the rules in the demand's dependency cone
    pub rules: Vec<String>,
    /// the facts a derivation of the demand is estimated to produce, from the dependency
    /// cone and the statistics of the input graphs (spec F17 §4.3)
    pub estimate: f64,
    /// whether a materialization with the same rules holds every fact of the demand: it
    /// does not when a top-level backward rule, which the materializer skips, lies in
    /// the demand's cone
    pub materialized: bool,
    /// whether the demand's table, with its constants alone, is in the cache
    pub cached: bool,
}

/// Whether the materialized inferences can answer a query's patterns under `auto`, for
/// the snapshot a session reads (spec F17 §4.1, §4.2).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Materialization {
    /// the query derives every intensional pattern and does not read the inferred graph
    /// (`reasoning=demand`)
    #[default]
    NotRead,
    /// fresh at the snapshot's commit, with the query-time profile, rules and inputs, and
    /// readable by the caller: a pattern may read it
    Fresh,
    /// stale, and the dataset's setting allows reading it: patterns read it and the
    /// response says so
    StaleRead { reason: String },
    /// the inferred graph cannot answer, for the reason given, so every intensional
    /// pattern is derived
    Derive { reason: String },
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

/// A query rewritten before it is planned, as OWL 2 QL rewriting does (spec F17 §7).
/// The rewritten query reads the stored facts for the basic graph patterns the provider
/// rewrote, and the derivation of the module documentation answers the others.
#[derive(Clone, Debug)]
pub struct Rewritten {
    /// the query with its rewritten pattern
    pub query: spargebra::Query,
    /// the basic graph patterns left for derivation, as the query wrote them; a group
    /// the walk of the rewritten pattern finds is derived only when it is one of these
    pub derive: Vec<Vec<TriplePattern>>,
    /// a `Rewrite` node per basic graph pattern, for EXPLAIN
    pub nodes: Vec<PlanInfo>,
    /// warnings for the response, such as axioms the rewriting ignored
    pub warnings: Vec<String>,
}

/// A reasoner that derives facts for queries (see the module documentation).
pub trait ReasoningProvider: Send + Sync {
    /// The mode and profile, for EXPLAIN, such as `demand, profile owl-rl`.
    fn label(&self) -> String;
    /// A session for one query.
    fn start(&self, req: SessionRequest) -> Result<Box<dyn ReasoningSession>>;
    /// Whether the provider chooses per pattern between the materialized inferences and
    /// derivation (`reasoning=auto`, spec F17 §4). The query then keeps the inferred
    /// graph in its default graph until the session says it cannot answer.
    fn auto(&self) -> bool {
        false
    }
    /// Under `auto`, the state of the materialized inferences when they answer every
    /// pattern of a query on `req`'s snapshot, so that the query reads them and starts no
    /// session: the materialization can answer and the rules hold no backward rule, whose
    /// facts it would lack (spec F17 §4.3 as built). `None` starts a session.
    fn reads_all(&self, req: &SessionRequest) -> Option<Materialization> {
        let _ = req;
        None
    }
    /// The query rewritten before planning, for the snapshot and view of `req`, or
    /// `None` when the provider rewrites nothing (spec F17 §7). The rewriting reads the
    /// provider's schema and not the query's data, and stops when `req.stop` fails.
    /// `bound` gives the variables of the query's initial bindings, without their sigil,
    /// with their values, which the planner substitutes into the rewritten query.
    fn rewrite(
        &self,
        query: &spargebra::Query,
        bound: &[(String, Term)],
        req: &SessionRequest,
    ) -> Result<Option<Rewritten>> {
        let _ = (query, bound, req);
        Ok(None)
    }
}

/// The derivations of one query.
pub trait ReasoningSession: Send {
    /// Whether rules can give facts that match the pattern (`None` is a variable).
    /// Interning a constant may require memory. Return its refusal directly; the
    /// caller stops before preparing demands or checking later patterns.
    fn intensional(&self, s: Option<&Term>, p: Option<&Term>, o: Option<&Term>) -> Result<bool>;
    /// Make the program for the demands the query can ask for. Demand `i` of later
    /// calls is `demands[i]`. With `estimates` false the infos' estimates are left at
    /// zero, for a caller that neither chooses by them nor shows them, since the
    /// estimate walks the dependency cone against the statistics for every demand.
    fn prepare(&mut self, demands: &[Demand], estimates: bool) -> Result<Vec<DemandInfo>>;
    /// Derive the facts of the demands, each with the seeds of its seeded position (an
    /// empty list for a demand without one), and return the facts not returned before.
    fn derive(&mut self, seeds: Vec<(usize, Vec<u64>)>) -> Result<Derived>;
    /// What the session did so far.
    fn report(&self) -> Report;
    /// Whether the materialized inferences can answer the query's patterns, for the
    /// snapshot the session reads. A provider that is not [`ReasoningProvider::auto`]
    /// answers [`Materialization::NotRead`].
    fn materialization(&self) -> Materialization {
        Materialization::NotRead
    }
}

/// The estimated cost of reading a pattern's facts from the inferred graph and of
/// deriving them, in the planner's unit of one scanned row (spec F17 §4.3). EXPLAIN shows
/// both, and they do not decide: a pattern that `auto` derives still reads the inferred
/// graph, which stays in the query's default graph, so deriving costs the read and the
/// derivation and is never the cheaper source ([`choose`]). The derivation constants
/// are provisional: they are measured once roadmap item 911 repairs the demand path, as
/// the F17 Outcome says. A derivation's fixed cost is the session's setup,
/// the program made for the query and the merge of its facts into the query's view, and
/// a table in the cache spares the program and the evaluation but not the merge.
pub mod cost {
    /// An index probe for a pattern's constants.
    pub const READ_BASE: f64 = 50.0;
    /// Reading one fact of the inferred graph.
    pub const READ_ROW: f64 = 1.0;
    /// Starting a derivation: the session, its program and the view's merge.
    pub const DERIVE_BASE: f64 = 500_000.0;
    /// Deriving one fact, with its magic facts and joins.
    pub const DERIVE_FACT: f64 = 1_000.0;
    /// Copying one fact of a cached table into the query's view.
    pub const CACHED_FACT: f64 = 100.0;

    /// The estimated cost of reading `rows` facts of the inferred graph.
    pub fn read(rows: f64) -> f64 {
        READ_BASE + rows * READ_ROW
    }

    /// The estimated cost of deriving `facts` facts, or of copying them from a cached
    /// table, beside the read of the pattern.
    pub fn derive(facts: f64, cached: bool) -> f64 {
        if cached {
            READ_BASE + facts * CACHED_FACT
        } else {
            DERIVE_BASE + facts * DERIVE_FACT
        }
    }
}

/// What `auto` chose for one intensional pattern, and why (spec F17 §2.4).
#[derive(Clone, Debug, PartialEq)]
pub enum Choice {
    /// read the materialized inferences
    Read,
    /// derive the pattern's facts
    Derive,
}

/// The source `auto` chooses for a pattern when the materialization can answer, with
/// the estimated costs of reading `read_rows` facts and of deriving the pattern. A
/// pattern reads unless a backward rule lies in its cone, whose facts the
/// materialization lacks. Its derivation would cost the read as well, since the inferred
/// graph stays in the query's default graph, so reading is never the dearer source
/// (spec F17 §4.3 as built).
pub fn choose(info: &DemandInfo, read_rows: u64) -> (Choice, f64, f64) {
    let read = cost::read(read_rows as f64);
    let derive = read + cost::derive(info.estimate, info.cached);
    let c = if info.materialized {
        Choice::Read
    } else {
        Choice::Derive
    };
    (c, read, derive)
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
    /// a `Derive` node per intensional pattern
    pub nodes: Vec<PlanInfo>,
    /// the dataset has RDFS on read, which did not apply
    pub rdfs_skipped: bool,
    /// time spent deriving, in milliseconds
    pub millis: f64,
    /// `reasoning=auto` (spec F17 §4)
    pub auto: bool,
    /// under `auto`, what the session said about the materialized inferences
    pub materialization: Materialization,
    /// under `auto`, the intensional patterns that read the materialized inferences
    pub read: u64,
}

impl Applied {
    /// The value of the `Sparkles-Reasoning` response header: present when the query
    /// derived facts for a pattern, which under `demand` is every query (spec F17 §2.3).
    pub fn header(&self) -> Option<String> {
        if self.auto && self.report.tables == 0 {
            return None;
        }
        let mode = if self.auto { "auto" } else { "demand" };
        Some(format!(
            "{mode}; derived={}; tables={}; cached={}",
            self.report.derived, self.report.tables, self.report.cached
        ))
    }

    /// Whether the query read the inferred graph as part of its default graph, so that
    /// the freshness of the materialized inferences concerns its answer.
    pub fn reads_inferred(&self) -> bool {
        matches!(
            self.materialization,
            Materialization::Fresh | Materialization::StaleRead { .. }
        )
    }

    /// `plan` under a `Reason` node with the `Derive` nodes before it.
    pub fn wrap(&self, plan: PlanInfo) -> PlanInfo {
        let mut description = format!("query-time reasoning: {}", self.label);
        if self.rdfs_skipped {
            description.push_str("; RDFS on read did not apply");
        }
        match &self.materialization {
            Materialization::NotRead => {}
            Materialization::Fresh => {
                description.push_str("; the materialized inferences are fresh");
            }
            Materialization::StaleRead { reason } => {
                description.push_str(&format!(
                    "; the materialized inferences are read although {reason}"
                ));
            }
            Materialization::Derive { reason } => {
                description.push_str(&format!(
                    "; the materialized inferences are not read: {reason}"
                ));
            }
        }
        let mut counters = serde_json::Map::new();
        counters.insert("derived".into(), self.report.derived.into());
        counters.insert("tables".into(), self.report.tables.into());
        counters.insert("cached".into(), self.report.cached.into());
        if self.auto {
            counters.insert("read".into(), self.read.into());
            counters.insert(
                "materialization".into(),
                match &self.materialization {
                    Materialization::NotRead => "not read",
                    Materialization::Fresh => "fresh",
                    Materialization::StaleRead { .. } => "stale, read",
                    Materialization::Derive { .. } => "not read",
                }
                .into(),
            );
        }
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
    /// the stored facts the pattern's constants match (`u64::MAX`: none, intensional)
    stored: u64,
    /// under `auto` with a readable materialization, the source chosen for the pattern
    choice: Option<Decision>,
    /// a seeded pattern derived with its constants alone, since its seeds were too many
    fell_to_plain: bool,
}

impl Pat {
    /// Whether the pattern reads the materialized inferences instead of deriving.
    fn reads(&self) -> bool {
        self.choice
            .as_ref()
            .is_some_and(|d| d.choice == Choice::Read)
    }
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
    rewritten: Option<&Rewritten>,
    opts: &QueryOptions,
    dataset: Option<&QueryDataset>,
    base: Option<&oxiri::Iri<String>>,
    rdfs_skipped: bool,
    execute: bool,
) -> Result<()> {
    let t0 = Instant::now();
    let mut collect = Collect::default();
    collect.walk(pattern);
    let auto = provider.auto();
    if let Some(r) = rewritten {
        // a rewritten pattern reads the stored facts: only the groups the provider left
        // for derivation, and the property paths, which no rewriting covers, are derived
        collect.groups.retain(|g| r.derive.contains(g));
        for w in &r.warnings {
            ctx.warn(super::ctx::PlanWarning {
                code: "reasoning",
                message: w.clone(),
            });
        }
        if collect.groups.is_empty() && collect.paths.is_empty() {
            // nothing to derive: no session starts, and `auto` reads no materialization
            // of another profile (§4.1)
            let materialization = if auto {
                drop_extra_graphs(ctx, &opts.default_graph_extra);
                Materialization::Derive {
                    reason: "the query-time profile rewrites the query".into(),
                }
            } else {
                Materialization::NotRead
            };
            ctx.reasoning = Some(Arc::new(Applied {
                label: provider.label(),
                nodes: r.nodes.clone(),
                rdfs_skipped,
                millis: t0.elapsed().as_secs_f64() * 1000.0,
                auto,
                materialization,
                ..Default::default()
            }));
            return Ok(());
        }
    }
    let req = request(ctx, opts);
    let reads_all = if auto && rewritten.is_none() {
        provider.reads_all(&req)
    } else {
        None
    };
    apply_derive(
        ctx,
        provider,
        opts,
        dataset,
        base,
        rdfs_skipped,
        execute,
        collect,
        req,
        reads_all,
        rewritten,
        t0,
    )
}

/// The request of a session for the query of `ctx`: its view, its stop check and its
/// memory budget.
pub(crate) fn request(ctx: &Ctx, opts: &QueryOptions) -> SessionRequest {
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
    SessionRequest {
        snap: ctx.snap.clone(),
        readable,
        stop,
        max_memory: opts.max_memory_bytes,
        no_cache: opts.no_cache,
    }
}

/// The query rewritten by `provider` before it is planned, if the provider rewrites it
/// (spec F17 §7).
pub(crate) fn rewrite(
    ctx: &Ctx,
    provider: &Arc<dyn ReasoningProvider>,
    query: &spargebra::Query,
    opts: &QueryOptions,
) -> Result<Option<Rewritten>> {
    let bound: Vec<(String, Term)> = opts
        .initial_bindings
        .iter()
        .map(|(name, t)| (name.trim_start_matches(['?', '$']).to_string(), t.clone()))
        .collect();
    provider.rewrite(query, &bound, &request(ctx, opts))
}

#[allow(clippy::too_many_arguments)]
fn apply_derive(
    ctx: &mut Ctx,
    provider: &Arc<dyn ReasoningProvider>,
    opts: &QueryOptions,
    dataset: Option<&QueryDataset>,
    base: Option<&oxiri::Iri<String>>,
    rdfs_skipped: bool,
    execute: bool,
    collect: Collect,
    req: SessionRequest,
    reads_all: Option<Materialization>,
    rewritten: Option<&Rewritten>,
    t0: Instant,
) -> Result<()> {
    let auto = provider.auto();
    // ---- under `auto`, a materialization that answers every pattern: the query reads
    // it as materialized reasoning does, and EXPLAIN alone starts a session to show the
    // estimates (spec F17 §4.3 as built)
    if execute && let Some(materialization) = reads_all.clone() {
        ctx.reasoning = Some(Arc::new(Applied {
            label: provider.label(),
            rdfs_skipped,
            millis: t0.elapsed().as_secs_f64() * 1000.0,
            auto,
            materialization,
            ..Default::default()
        }));
        return Ok(());
    }
    // the derived facts go to the store's default graph, which the query must read,
    // unless the materialization answers every pattern and nothing is derived
    let reads_default = match &ctx.dataset.default {
        None => !ctx.dataset.union_default && !ctx.snap.union_default_graph,
        Some(l) => l.contains(&Id::DEFAULT_GRAPH),
    };
    if !reads_default && reads_all.is_none() {
        return Err(Error::Unsupported(
            "query-time reasoning adds the derived facts to the store's default graph, \
             which this query does not read (its dataset names other graphs, or the default \
             graph is the union of the named graphs)"
                .into(),
        ));
    }
    let mut session = provider.start(req)?;
    // ---- under `auto`, whether the materialized inferences answer (spec F17 §4.1, §4.2)
    let materialization = if auto {
        session.materialization()
    } else {
        Materialization::NotRead
    };
    let reads_inferred = matches!(
        materialization,
        Materialization::Fresh | Materialization::StaleRead { .. }
    );
    if auto && !reads_inferred {
        drop_extra_graphs(ctx, &opts.default_graph_extra);
    }
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
            .collect::<Result<Vec<_>>>()?;
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
                    stored: estimates[i],
                    choice: None,
                    fell_to_plain: false,
                });
            } else {
                pats.push(Pat {
                    group: gi,
                    t: t.clone(),
                    seeded: None,
                    plain: None,
                    stored: estimates[i],
                    choice: None,
                    fell_to_plain: false,
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
        if session.intensional(None, pt.as_ref(), None)? {
            let d = Demand {
                s: DemandTerm::Var,
                p: pt.map_or(DemandTerm::Var, DemandTerm::Const),
                o: DemandTerm::Var,
            };
            path_demands.push(add(d, &mut demands));
        }
    }
    // EXPLAIN shows the estimates and `auto` chooses by them, so an executed query that
    // reads no materialization leaves them out of its `Derive` nodes
    let estimates = !execute || reads_inferred;
    let infos = if demands.is_empty() {
        Vec::new()
    } else {
        session.prepare(&demands, estimates)?
    };
    // ---- under `auto` with a readable materialization, the patterns that read it
    // (spec F17 §4.3). A seeded pattern has the cone, and so the choice, of its plain
    // demand.
    let decide = |pats: &mut Vec<Pat>, infos: &[DemandInfo]| {
        for p in pats.iter_mut() {
            let Some(d) = p.plain else { continue };
            let info = &infos[d];
            let variable = matches!(&p.t.predicate, NamedNodePattern::Variable(_));
            let rows = if p.stored == u64::MAX { 0 } else { p.stored };
            let (c, read, derive) = choose(info, rows);
            let reason = decision_reason(&c, variable);
            p.choice = Some(Decision {
                choice: c,
                read,
                derive,
                reason,
            });
        }
    };
    if reads_inferred {
        decide(&mut pats, &infos);
    }
    let label = provider.label();
    let reason_text = |auto: bool, mat: &Materialization| -> Option<String> {
        if !auto {
            return None;
        }
        Some(match mat {
            Materialization::Fresh => "the materialization is fresh".to_string(),
            Materialization::StaleRead { reason } | Materialization::Derive { reason } => {
                reason.clone()
            }
            Materialization::NotRead => "no materialization".to_string(),
        })
    };
    let node = |d: usize,
                text: &str,
                how: &str,
                report: Option<&DemandReport>,
                decision: Option<&Decision>| {
        let info = infos.get(d).cloned().unwrap_or_default();
        let mut counters = serde_json::Map::new();
        counters.insert("adornment".into(), info.adornment.clone().into());
        counters.insert(
            "rules".into(),
            serde_json::Value::Array(info.rules.iter().map(|r| r.clone().into()).collect()),
        );
        counters.insert("seeds".into(), how.into());
        if estimates {
            counters.insert("estimatedDerived".into(), info.estimate.into());
        }
        if let Some(r) = report {
            counters.insert("derived".into(), r.derived.into());
            counters.insert("seedCount".into(), r.seeds.into());
        }
        let read = decision.is_some_and(|d| d.choice == Choice::Read);
        if auto {
            counters.insert(
                "source".into(),
                if read { "materialized" } else { "derived" }.into(),
            );
            if let Some(d) = decision {
                counters.insert("readCost".into(), d.read.into());
                counters.insert("deriveCost".into(), d.derive.into());
                counters.insert("reason".into(), d.reason.clone().into());
            } else if let Some(r) = reason_text(auto, &materialization) {
                counters.insert("reason".into(), r.into());
            }
        }
        let description = if read {
            format!("{text} as {}, read from the inferred graph", info.adornment)
        } else {
            format!("{text} as {}", info.adornment)
        };
        PlanInfo {
            operator: "Derive".into(),
            description,
            estimated_rows: 0.0,
            actual_rows: report.map_or(-1, |r| r.derived as i64),
            cached: report.is_some_and(|r| r.cached),
            counters: Some(counters),
            ..Default::default()
        }
    };
    let path_decision = |d: usize| -> Option<Decision> {
        if !reads_inferred {
            return None;
        }
        let info = &infos[d];
        let rows = stored_count_of(&ctx.snap, &demands[d]);
        let (c, read, derive) = choose(info, rows);
        Some(Decision {
            reason: decision_reason(&c, true),
            choice: c,
            read,
            derive,
        })
    };
    if !execute {
        // the Rewrite nodes come first, as in the executed form
        let mut nodes = rewritten.map_or_else(Vec::new, |r| r.nodes.clone());
        let mut read = 0;
        for p in &pats {
            if let Some(d) = p.seeded.as_ref().map(|s| s.0).or(p.plain) {
                let how = if p.seeded.is_some() {
                    "join"
                } else {
                    "constants"
                };
                read += p.reads() as u64;
                nodes.push(node(d, &pattern_text(&p.t), how, None, p.choice.as_ref()));
            }
        }
        for &d in &path_demands {
            let dec = path_decision(d);
            read += dec.as_ref().is_some_and(|d| d.choice == Choice::Read) as u64;
            nodes.push(node(
                d,
                "property path step",
                "constants",
                None,
                dec.as_ref(),
            ));
        }
        ctx.reasoning = Some(Arc::new(Applied {
            label,
            report: Report::default(),
            nodes,
            rdfs_skipped,
            millis: t0.elapsed().as_secs_f64() * 1000.0,
            auto,
            materialization,
            read,
        }));
        return Ok(());
    }
    // ---- round 0: the demands with constants alone
    let mut view = Overlay::new(ctx.snap.clone());
    let mut first: Vec<(usize, Vec<u64>)> = Vec::new();
    for p in &pats {
        if p.seeded.is_none()
            && !p.reads()
            && let Some(d) = p.plain
            && !first.iter().any(|(x, _)| *x == d)
        {
            first.push((d, Vec::new()));
        }
    }
    let mut path_decisions: Vec<Option<Decision>> = Vec::new();
    for &d in &path_demands {
        let dec = path_decision(d);
        let read = dec.as_ref().is_some_and(|x| x.choice == Choice::Read);
        path_decisions.push(dec);
        if !read && !first.iter().any(|(x, _)| *x == d) {
            first.push((d, Vec::new()));
        }
    }
    if !first.is_empty() {
        let out = session.derive(first)?;
        view.add(ctx, out)?;
    }
    // ---- seeded patterns, in the order of their groups
    let mut used_plain: Vec<usize> = Vec::new();
    for k in 0..pats.len() {
        let Some((d, var)) = pats[k].seeded.clone() else {
            continue;
        };
        // under `auto`, a pattern that reads needs no seeds
        if pats[k].reads() {
            continue;
        }
        // the earlier patterns of the group that connect to the variable
        let group = pats[k].group;
        let earlier: Vec<&TriplePattern> = pats[..k]
            .iter()
            .filter(|q| q.group == group)
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
        let seeds = seed_values(ctx, &view, &chosen, &var, opts, dataset, base)?;
        match seeds.and_then(|s| view.reasoner_ids(&s)) {
            Some(ids) => {
                let out = session.derive(vec![(d, ids)])?;
                view.add(ctx, out)?;
            }
            None => {
                pats[k].fell_to_plain = true;
                let plain = pats[k]
                    .plain
                    .expect("an intensional pattern has a plain demand");
                if !used_plain.contains(&plain) {
                    used_plain.push(plain);
                    let out = session.derive(vec![(plain, Vec::new())])?;
                    view.add(ctx, out)?;
                }
            }
        }
    }
    let report = session.report();
    let mut nodes = rewritten.map_or_else(Vec::new, |r| r.nodes.clone());
    let mut read = 0;
    for p in &pats {
        let chosen = match (&p.seeded, p.plain) {
            (Some((d, _)), Some(pl)) => {
                if p.fell_to_plain {
                    Some((pl, "constants (too many seeds)"))
                } else {
                    Some((*d, "join"))
                }
            }
            (None, Some(pl)) => Some((pl, "constants")),
            _ => None,
        };
        if let Some((d, how)) = chosen {
            read += p.reads() as u64;
            nodes.push(node(
                d,
                &pattern_text(&p.t),
                how,
                report.demands.get(d),
                p.choice.as_ref(),
            ));
        }
    }
    for (i, &d) in path_demands.iter().enumerate() {
        let dec = path_decisions.get(i).and_then(|x| x.as_ref());
        read += dec.is_some_and(|x| x.choice == Choice::Read) as u64;
        nodes.push(node(
            d,
            "property path step",
            "constants",
            report.demands.get(d),
            dec,
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
        auto,
        materialization,
        read,
    }));
    Ok(())
}

/// Under `auto`, what the planner chose for one intensional pattern (spec F17 §2.4).
#[derive(Clone, Debug)]
struct Decision {
    choice: Choice,
    /// the estimated cost of reading the inferred graph, in the planner's row units
    read: f64,
    /// the estimated cost of deriving
    derive: f64,
    reason: String,
}

/// Why `auto` chose as it did for a pattern.
fn decision_reason(c: &Choice, variable_predicate: bool) -> String {
    match c {
        Choice::Derive => "backward rules in the cone, which the materialization lacks".to_string(),
        Choice::Read if variable_predicate => {
            "the materialization holds the facts, and a variable predicate demands the whole \
             program"
                .to_string()
        }
        Choice::Read => {
            "the materialization holds the facts, and deriving them would add to reading them"
                .to_string()
        }
    }
}

/// Take the graphs of `opts.default_graph_extra`, the inferred graph, out of the
/// query's default graph: under `auto`, when the materialization cannot answer (spec F17
/// §4.2).
fn drop_extra_graphs(ctx: &mut Ctx, extra: &[String]) {
    if extra.is_empty() {
        return;
    }
    let ids: Vec<Id> = extra.iter().map(|i| ctx.graph_id(i)).collect();
    if let Some(d) = &mut ctx.dataset.default {
        d.retain(|g| !ids.contains(g));
        if d.as_slice() == [Id::DEFAULT_GRAPH] && !ctx.snap.union_default_graph {
            ctx.dataset.default = None;
        }
    }
}

/// [`stored_count`] of a demand's constants.
fn stored_count_of(snap: &Snapshot, d: &Demand) -> u64 {
    let term = |x: &DemandTerm| match x {
        DemandTerm::Const(Term::NamedNode(n)) => TermPattern::NamedNode(n.clone()),
        DemandTerm::Const(Term::Literal(l)) => TermPattern::Literal(l.clone()),
        _ => TermPattern::Variable(Variable::new_unchecked("x")),
    };
    let t = TriplePattern {
        subject: term(&d.s),
        predicate: match &d.p {
            DemandTerm::Const(Term::NamedNode(n)) => NamedNodePattern::NamedNode(n.clone()),
            _ => NamedNodePattern::Variable(Variable::new_unchecked("p")),
        },
        object: term(&d.o),
    };
    match stored_count(snap, &t) {
        u64::MAX => 0,
        n => n,
    }
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
/// facts derived so far, or `None` when there are more than [`MAX_SEEDS`] or a chosen
/// pattern calls a registered property function. Such a call runs only in the query
/// itself, so the seeded pattern falls back to its demand with constants.
fn seed_values(
    ctx: &Ctx,
    view: &Overlay,
    chosen: &[TriplePattern],
    var: &Variable,
    opts: &QueryOptions,
    dataset: Option<&QueryDataset>,
    base: Option<&oxiri::Iri<String>>,
) -> Result<Option<Vec<Id>>> {
    if super::propertyext::has_calls(ctx, chosen) {
        return Ok(None);
    }
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
    sub.attach_owner();
    let mut planner = Planner::new(&sub);
    let node = planner.plan(&gp, &ActiveGraph::Default, Vec::new())?;
    let (input, _) = exec::execute_materialized(&sub, &node)?;
    let table = &input.table;
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

#[cfg(test)]
mod tests {
    use super::*;

    fn info(estimate: f64, materialized: bool, cached: bool) -> DemandInfo {
        DemandInfo {
            adornment: "p^bf".into(),
            rules: vec!["r".into()],
            estimate,
            materialized,
            cached,
        }
    }

    /// A pattern reads whenever the materialization holds its facts, since its
    /// derivation would add to the read, and derives when a backward rule lies in its
    /// cone. EXPLAIN's costs say so (spec F17 §4.3 as built).
    #[test]
    fn auto_reads_what_the_materialization_holds() {
        for rows in [0, 100, 1_000_000_000] {
            for estimate in [0.0, 10.0, 1e9] {
                for cached in [false, true] {
                    let (c, read, derive) = choose(&info(estimate, true, cached), rows);
                    assert_eq!(c, Choice::Read, "{rows} {estimate} {cached}");
                    assert!(read < derive, "{read} {derive}");
                }
            }
        }
        // a cached table costs less to copy than a derivation
        let (_, _, cached) = choose(&info(10.0, true, true), 100);
        let (_, _, fresh) = choose(&info(10.0, true, false), 100);
        assert!(cached < fresh);
        assert_eq!(
            choose(&info(10.0, false, false), 1).0,
            Choice::Derive,
            "backward rules in the cone"
        );
        assert!(decision_reason(&Choice::Derive, false).contains("backward"));
        assert!(decision_reason(&Choice::Read, true).contains("variable"));
    }

    /// The header is present under `demand` always and under `auto` only when a pattern
    /// was derived.
    #[test]
    fn the_header_names_the_mode_and_is_left_out_when_auto_read_everything() {
        let mut a = Applied::default();
        assert_eq!(
            a.header().as_deref(),
            Some("demand; derived=0; tables=0; cached=0")
        );
        a.auto = true;
        a.materialization = Materialization::Fresh;
        assert_eq!(a.header(), None);
        assert!(a.reads_inferred());
        a.report.tables = 2;
        a.report.derived = 7;
        assert_eq!(
            a.header().as_deref(),
            Some("auto; derived=7; tables=2; cached=0")
        );
        a.materialization = Materialization::Derive {
            reason: "stale".into(),
        };
        assert!(!a.reads_inferred());
        a.materialization = Materialization::StaleRead {
            reason: "stale".into(),
        };
        assert!(a.reads_inferred());
    }
}
