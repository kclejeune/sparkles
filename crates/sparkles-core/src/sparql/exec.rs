//! Physical operator execution (column-at-a-time, fully materialized results).

use super::ctx::Ctx;
use super::expr::{Expr, Row, Val, ebv, eval};
use super::exprcache::Report as ExprReport;
use super::plan::{
    Agg, GraphFilter, JoinAlgo, Kind, Node, OrderedTopK, PathEnd, PathSpec, RangeSpec, ScanSpec,
};
use super::sortkey::{Entry, Screen, SortKey, TopK};
use super::table::{Materialized, Table, VarId};
use super::value::{NumOp, Value, arith, order_cmp};
use crate::error::{Error, Result};
use crate::id::Id;
use crate::index::{Block, ColMask, Key, O, P, Perm, S, pad};
use crate::store::Chunk;
use rayon::prelude::*;
use rustc_hash::{FxHashMap, FxHashSet};
use serde::Serialize;
use spargebra::algebra::AggregateFunction;
use std::cmp::Ordering;
use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};
use std::time::Instant;

pub(super) const PAR_THRESHOLD: usize = 16_384;

/// Parallel iterators over rows hand out pieces of at least this many rows, so a small
/// input is processed on the calling thread: waking the pool for it would cost more
/// than the work, and a short request would wait for threads on idle cores to wake.
pub(super) const PAR_MIN_LEN: usize = 4096;

/// Map `f` over rows `0..n` (in parallel when `par`) in chunks, checking cancellation and
/// the deadline between chunks: one clock read per chunk rather than per row, while an
/// expensive expression still stops within one chunk of the deadline.
pub(super) fn map_rows<T: Send>(
    ctx: &Ctx,
    n: usize,
    par: bool,
    f: impl Fn(usize) -> T + Sync + Send,
) -> Result<Vec<T>> {
    if par && ctx.is_cursor() {
        // Submit one partitioned job and poll within each partition. Repeated
        // 1024-row pool submissions cost more than cheap numeric predicates.
        // Temporary collector capacity is optional and reserved before dispatch.
        let bytes = (n as u64)
            .saturating_mul(std::mem::size_of::<T>() as u64)
            .saturating_mul(2)
            .saturating_add(rayon::current_num_threads() as u64 * 1024);
        if let Ok(_scratch) = ctx.charge(bytes) {
            return (0..n)
                .into_par_iter()
                .with_min_len(PAR_MIN_LEN)
                .map(|i| {
                    if i.is_multiple_of(1024) {
                        ctx.check()?;
                    }
                    Ok(f(i))
                })
                .collect();
        }
    }
    let chunk = if par && !ctx.is_cursor() {
        1 << 16
    } else {
        1 << 10
    };
    let mut out = Vec::with_capacity(n);
    let mut start = 0;
    while start < n {
        ctx.check()?;
        let end = (start + chunk).min(n);
        if par {
            out.par_extend((start..end).into_par_iter().map(&f));
        } else {
            out.extend((start..end).map(&f));
        }
        start = end;
    }
    Ok(out)
}

/// Executed operator tree with runtime information (QLever `RuntimeInformation`).
///
/// A written node carries an `id`, the path of child indexes from the root (`0`, `0.1`,
/// `0.1.2`), which is computed when the tree is serialized. An id is only valid for the
/// plan it was written with (spec C18 §6.6.1). A plan deeper than [`MAX_ID_DEPTH`] levels
/// is written without ids, in plan format 2 (see [`PLAN_FORMAT`]).
#[derive(Clone, Debug, Default)]
pub struct PlanInfo {
    pub operator: String,
    pub description: String,
    pub columns: Vec<String>,
    pub sorted_on: Vec<String>,
    pub estimated_rows: f64,
    pub estimated_cost: f64,
    pub actual_rows: i64,
    pub time_ms: f64,
    pub cached: bool,
    pub children: Vec<PlanInfo>,
    /// operator counters (spatial operators: candidates, refined, matched, …)
    pub counters: Option<serde_json::Map<String, serde_json::Value>>,
    /// notes about the plan (root only)
    pub warnings: Vec<super::ctx::PlanWarning>,
    /// What the node says beyond its counts (see [`PlanInfo::fidelity`]). Few nodes
    /// have any, so it is boxed to keep the plan of a node that simply ran small: every
    /// operator builds one and returns it up the tree.
    pub fidelity: Option<Box<PlanFidelity>>,
    /// The parts of the query the node reads (see [`PlanRefs`]). Only nodes that read
    /// triple patterns, filter or join have any, so it is boxed like `fidelity`.
    pub refs: Option<Box<PlanRefs>>,
}

/// What a plan node reads of the query's text, so that a client can draw the plan on
/// the query's pattern (spec U04 §11.3). Every term here is written in the query, so it
/// tells a caller nothing the query did not.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PlanRefs {
    /// The triple patterns and paths the node reads, in query order.
    pub patterns: Vec<std::sync::Arc<PlanPattern>>,
    /// The query variables a filter reads.
    pub filter_vars: Vec<String>,
    /// The query variables a join joins on.
    pub join_vars: Vec<String>,
}

/// A triple pattern or path of the query, as a plan node names it (spec U04 §11.3).
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize)]
pub struct PlanPattern {
    /// The subject as an N-Triples term, `?name` for a variable of the query, or `?_` for
    /// a blank node or a variable the planner made.
    pub s: String,
    /// The predicate, or a path written as [`path_text`] writes it.
    pub p: String,
    pub o: String,
    /// The graph term of the `GRAPH` around it, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub graph: Option<String>,
    /// Its index among the equal patterns of the query, from 0.
    pub occurrence: u32,
}

/// A property path in the form the plan's `patterns` write it: IRIs in angle brackets,
/// `/` and `|` between the steps and choices of a flattened sequence or alternative,
/// `^`, `*`, `+`, `?` and `{n}`, `{n,m}` or `{n,}` around a step in parentheses unless it
/// is one IRI or a negated set, `!(…)` with `|` for a negated set, and ARQ's `distinct(…)`,
/// `multi(…)` and `shortest(…)`. There are no spaces.
pub fn path_text(p: &spargebra::algebra::PropertyPathExpression) -> String {
    use spargebra::algebra::PropertyPathExpression as PP;
    fn atom(p: &PP) -> bool {
        matches!(p, PP::NamedNode(_) | PP::NegatedPropertySet(_))
    }
    fn wrapped(p: &PP, out: &mut String) {
        if atom(p) {
            write(p, out);
        } else {
            out.push('(');
            write(p, out);
            out.push(')');
        }
    }
    fn seq<'a>(p: &'a PP, out: &mut Vec<&'a PP>) {
        match p {
            PP::Sequence(a, b) => {
                seq(a, out);
                seq(b, out);
            }
            p => out.push(p),
        }
    }
    fn alt<'a>(p: &'a PP, out: &mut Vec<&'a PP>) {
        match p {
            PP::Alternative(a, b) => {
                alt(a, out);
                alt(b, out);
            }
            p => out.push(p),
        }
    }
    fn write(p: &PP, out: &mut String) {
        match p {
            PP::NamedNode(n) => {
                out.push('<');
                out.push_str(n.as_str());
                out.push('>');
            }
            PP::Reverse(x) => {
                out.push('^');
                wrapped(x, out);
            }
            PP::Sequence(..) | PP::Alternative(..) => {
                let mut items = Vec::new();
                let sep = if matches!(p, PP::Sequence(..)) {
                    seq(p, &mut items);
                    '/'
                } else {
                    alt(p, &mut items);
                    '|'
                };
                for (i, x) in items.into_iter().enumerate() {
                    if i > 0 {
                        out.push(sep);
                    }
                    // a step that is itself a choice, or a choice that is a sequence
                    if matches!(x, PP::Sequence(..) | PP::Alternative(..)) {
                        out.push('(');
                        write(x, out);
                        out.push(')');
                    } else {
                        write(x, out);
                    }
                }
            }
            PP::ZeroOrMore(x) | PP::OneOrMore(x) | PP::ZeroOrOne(x) => {
                wrapped(x, out);
                out.push(match p {
                    PP::ZeroOrMore(_) => '*',
                    PP::OneOrMore(_) => '+',
                    _ => '?',
                });
            }
            PP::NegatedPropertySet(items) => {
                out.push_str("!(");
                for (i, n) in items.iter().enumerate() {
                    if i > 0 {
                        out.push('|');
                    }
                    out.push('<');
                    out.push_str(n.as_str());
                    out.push('>');
                }
                out.push(')');
            }
            PP::Range { path, min, max } => {
                wrapped(path, out);
                out.push_str(&match max {
                    Some(m) if m == min => format!("{{{min}}}"),
                    Some(m) => format!("{{{min},{m}}}"),
                    None => format!("{{{min},}}"),
                });
            }
            PP::Distinct(x) | PP::Multi(x) | PP::Shortest(x) => {
                out.push_str(match p {
                    PP::Distinct(_) => "distinct(",
                    PP::Multi(_) => "multi(",
                    _ => "shortest(",
                });
                write(x, out);
                out.push(')');
            }
        }
    }
    let mut out = String::new();
    write(p, &mut out);
    out
}

/// What a plan node says beyond its counts (spec C18 §6.6.5).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PlanFidelity {
    /// Why the operator never ran, when it did not. Its counts are then `-1` rows and
    /// 0 ms, and the reason holds for its whole subtree.
    pub skipped: Option<String>,
    /// It ran only until a `LIMIT`, `ASK` or `EXISTS` had enough rows.
    pub stopped_early: bool,
    /// Its counts stop where a failure, such as a spent budget, stopped the query.
    /// Written as `complete: false`.
    pub incomplete: bool,
    /// How many more times than once it ran, because a parent reran it with a larger
    /// budget (`LIMIT` without `ORDER BY`) or once per batch (spatial k nearest). Its
    /// time and its children's add up over the runs, and its rows are the last run's.
    /// Written as `runs`, the number of runs, when it ran more than once.
    pub reruns: u32,
    /// The filter conjuncts that a scan or an index join tests itself and that have no
    /// node of their own.
    pub pushed_filters: Vec<String>,
    /// The estimate is a fixed guess rather than a statistic (`SERVICE`).
    pub estimate_guessed: bool,
}

static NO_FIDELITY: PlanFidelity = PlanFidelity {
    skipped: None,
    stopped_early: false,
    incomplete: false,
    reruns: 0,
    pushed_filters: Vec::new(),
    estimate_guessed: false,
};

/// The deepest plan whose nodes are written with their ids. A node's id is its path from
/// the root, so the ids of a plan grow with the square of its depth: those of a chain of
/// 2000 triple patterns, 4000 levels deep, took 24 MB of its 54 MB. Every query of the
/// test suites plans far less deep than this.
pub const MAX_ID_DEPTH: usize = 256;

/// The format a plan deeper than [`MAX_ID_DEPTH`] is written in, which its root names as
/// `planFormat`. Its nodes have no `id`, and the id of each is the path of child indexes
/// from the root, as format 1 writes it. A plan without `planFormat` is in format 1.
pub const PLAN_FORMAT: u64 = 2;

impl Serialize for PlanInfo {
    fn serialize<S: serde::Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        if self.depth() <= MAX_ID_DEPTH {
            self.with_id("0").serialize(s)
        } else {
            PlanWithId {
                info: self,
                id: None,
                root: true,
            }
            .serialize(s)
        }
    }
}

/// A [`PlanInfo`] written as the node `id` of its plan, with its children as `id.0`,
/// `id.1` and so on, or in format 2 without ids.
pub struct PlanWithId<'a> {
    info: &'a PlanInfo,
    id: Option<std::borrow::Cow<'a, str>>,
    /// it is the root of a plan written in format 2
    root: bool,
}

impl Serialize for PlanWithId<'_> {
    fn serialize<S: serde::Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        struct Children<'a>(&'a [PlanInfo], Option<&'a str>);
        impl Serialize for Children<'_> {
            fn serialize<S: serde::Serializer>(
                &self,
                s: S,
            ) -> std::result::Result<S::Ok, S::Error> {
                s.collect_seq(self.0.iter().enumerate().map(|(i, c)| PlanWithId {
                    info: c,
                    id: self.1.map(|id| format!("{id}.{i}").into()),
                    root: false,
                }))
            }
        }
        // a plan nests up to `MAX_PLAN_DEPTH` levels, more than the stack of a small
        // thread holds in a debug build, so each level makes sure of its own stack
        stacker::maybe_grow(PLAN_RED_ZONE, PLAN_STACK_SEGMENT, || {
            let mut m = s.serialize_map(None)?;
            if self.root {
                m.serialize_entry("planFormat", &PLAN_FORMAT)?;
            }
            let id = self.id.as_deref();
            self.info.serialize_members(id, &mut m)?;
            m.serialize_entry("children", &Children(&self.info.children, id))?;
            m.end()
        })
    }
}

/// The stack a level of a recursion over a [`PlanInfo`] wants left before it runs.
pub(super) const PLAN_RED_ZONE: usize = 64 << 10;

/// The stack segment a recursion over a [`PlanInfo`] grows by when less is left.
pub(super) const PLAN_STACK_SEGMENT: usize = 2 << 20;

impl PlanInfo {
    /// This node written as the node `id` of its plan.
    pub fn with_id<'a>(&'a self, id: impl Into<std::borrow::Cow<'a, str>>) -> PlanWithId<'a> {
        PlanWithId {
            info: self,
            id: Some(id.into()),
            root: false,
        }
    }

    /// Write `id`, when the plan is written with ids, and every member except `children`,
    /// for a caller that writes the children in its own form (the operator of a
    /// [`CursorPlan`](super::CursorPlan)).
    pub(super) fn serialize_members<M: serde::ser::SerializeMap>(
        &self,
        id: Option<&str>,
        m: &mut M,
    ) -> std::result::Result<(), M::Error> {
        if let Some(id) = id {
            m.serialize_entry("id", id)?;
        }
        m.serialize_entry("operator", &self.operator)?;
        m.serialize_entry("description", &self.description)?;
        m.serialize_entry("columns", &self.columns)?;
        m.serialize_entry("sortedOn", &self.sorted_on)?;
        m.serialize_entry("estimatedRows", &self.estimated_rows)?;
        m.serialize_entry("estimatedCost", &self.estimated_cost)?;
        m.serialize_entry("actualRows", &self.actual_rows)?;
        m.serialize_entry("timeMs", &self.time_ms)?;
        m.serialize_entry("cached", &self.cached)?;
        if let Some(c) = &self.counters {
            m.serialize_entry("counters", c)?;
        }
        if !self.warnings.is_empty() {
            m.serialize_entry("warnings", &self.warnings)?;
        }
        let f = self.fidelity();
        if let Some(r) = &f.skipped {
            m.serialize_entry("skipped", r)?;
        }
        if f.stopped_early {
            m.serialize_entry("stoppedEarly", &true)?;
        }
        if f.incomplete {
            m.serialize_entry("complete", &false)?;
        }
        if f.reruns > 0 {
            m.serialize_entry("runs", &(u64::from(f.reruns) + 1))?;
        }
        if !f.pushed_filters.is_empty() {
            m.serialize_entry("pushedFilters", &f.pushed_filters)?;
        }
        if f.estimate_guessed {
            m.serialize_entry("estimateGuessed", &true)?;
        }
        if let Some(r) = &self.refs {
            if !r.patterns.is_empty() {
                let patterns: Vec<&PlanPattern> = r.patterns.iter().map(|p| &**p).collect();
                m.serialize_entry("patterns", &patterns)?;
            }
            if !r.filter_vars.is_empty() {
                m.serialize_entry("filterVars", &r.filter_vars)?;
            }
            if !r.join_vars.is_empty() {
                m.serialize_entry("joinVars", &r.join_vars)?;
            }
        }
        Ok(())
    }

    /// Remove what the plan tells about graphs a graph view does not read: estimates
    /// (from statistics of every graph), the quads of unread graphs that statistics
    /// notes count, and operator counters (a spatial index counts candidates of every
    /// graph). Actual rows and times describe the view and stay.
    pub fn redact(&mut self) {
        self.estimated_rows = -1.0;
        self.estimated_cost = -1.0;
        self.counters = None;
        const NOTE: &str = " [from statistics";
        if let Some(i) = self.description.find(NOTE)
            && let Some(end) = self.description[i..].find(']')
        {
            self.description
                .replace_range(i..i + end + 1, " [from statistics]");
        }
        for c in &mut self.children {
            c.redact();
        }
    }

    /// What this node says beyond its counts: whether it was skipped, stopped early,
    /// rerun or left incomplete, and the filters it applied itself.
    pub fn fidelity(&self) -> &PlanFidelity {
        self.fidelity.as_deref().unwrap_or(&NO_FIDELITY)
    }

    /// [`PlanInfo::fidelity`], to change.
    pub fn fidelity_mut(&mut self) -> &mut PlanFidelity {
        self.fidelity.get_or_insert_default()
    }

    /// This node and its subtree marked as never run, for `why`.
    pub(super) fn skip(mut self, why: &str) -> PlanInfo {
        fn mark(i: &mut PlanInfo, why: &str) {
            i.fidelity_mut().skipped = Some(why.to_string());
            i.actual_rows = -1;
            i.time_ms = 0.0;
            for c in &mut i.children {
                mark(c, why);
            }
        }
        mark(&mut self, why);
        self
    }

    /// How many levels the plan nests, counted without recursion: 1 for a leaf. A
    /// caller that walks the plan recursively runs on
    /// [`with_stack`](super::depth::with_stack) of this depth.
    pub fn depth(&self) -> usize {
        let mut deepest = 0;
        let mut todo = vec![(self, 1)];
        while let Some((n, d)) = todo.pop() {
            deepest = deepest.max(d);
            todo.extend(n.children.iter().map(|c| (c, d + 1)));
        }
        deepest
    }

    /// The node `id` (`0`, `0.1`, …) of this plan, when it has one.
    pub fn node(&self, id: &str) -> Option<&PlanInfo> {
        let mut parts = id.split('.');
        if parts.next()? != "0" {
            return None;
        }
        let mut n = self;
        for p in parts {
            n = n.children.get(p.parse::<usize>().ok()?)?;
        }
        Some(n)
    }

    /// `next` after this earlier run of the same node: times add up, the counts are
    /// the last run's, and children of the same shape merge pairwise.
    pub(super) fn merge_run(self, mut next: PlanInfo) -> PlanInfo {
        next.time_ms += self.time_ms;
        let reruns = self.fidelity().reruns;
        let f = next.fidelity_mut();
        f.reruns = f.reruns.saturating_add(reruns).saturating_add(1);
        if self.children.len() == next.children.len()
            && self
                .children
                .iter()
                .zip(&next.children)
                .all(|(a, b)| a.operator == b.operator)
        {
            let children = std::mem::take(&mut next.children);
            next.children = self
                .children
                .into_iter()
                .zip(children)
                .map(
                    |(a, b)| match (&a.fidelity().skipped, &b.fidelity().skipped) {
                        // only the earlier run ran it
                        (None, Some(_)) => a,
                        (Some(_), _) => b,
                        (None, None) => a.merge_run(b),
                    },
                )
                .collect();
        }
        next
    }
}

/// What the plan of `n` says beyond its counts before it runs: the filters it tests
/// itself, as text, and whether its estimate is a guess. Most nodes have neither.
#[inline]
fn node_fidelity(ctx: &Ctx, n: &Node) -> Option<Box<PlanFidelity>> {
    #[cold]
    #[inline(never)]
    fn of(ctx: &Ctx, n: &Node) -> Option<Box<PlanFidelity>> {
        let pushed_filters: Vec<String> = match &n.kind {
            Kind::RangeScan(_, range) => range.filter.iter().map(|e| e.display(ctx)).collect(),
            Kind::IndexJoin(spec) => spec
                .probes
                .iter()
                .flat_map(|p| p.filter.iter().map(|e| e.display(ctx)))
                .collect(),
            _ => Vec::new(),
        };
        let estimate_guessed = matches!(n.kind, Kind::Service { .. });
        (estimate_guessed || !pushed_filters.is_empty()).then(|| {
            Box::new(PlanFidelity {
                pushed_filters,
                estimate_guessed,
                ..Default::default()
            })
        })
    }
    match &n.kind {
        Kind::RangeScan(..) | Kind::IndexJoin(_) | Kind::Service { .. } => of(ctx, n),
        _ => None,
    }
}

/// What `n` reads of the query's text (spec U04 §11.3): the patterns of a scan, an index
/// join or a path, the variables of a filter and those a join joins on. Most nodes read
/// none of it, and no node names it when the plan names no patterns
/// ([`Ctx::plan_patterns`]).
#[inline]
fn node_refs(ctx: &Ctx, n: &Node) -> Option<Box<PlanRefs>> {
    #[cold]
    #[inline(never)]
    fn of(ctx: &Ctx, n: &Node) -> Option<Box<PlanRefs>> {
        let mut patterns = Vec::new();
        let mut scan = |s: &ScanSpec| {
            if let Some(p) = &s.src
                && !patterns.contains(p)
            {
                patterns.push(p.clone());
            }
        };
        let mut filter_vars = Vec::new();
        let mut join_vars = Vec::new();
        let shared = |a: &Node, b: &Node| -> Vec<VarId> {
            a.vars
                .iter()
                .copied()
                .filter(|v| b.vars.contains(v))
                .collect()
        };
        match &n.kind {
            Kind::Scan(s) | Kind::RangeScan(s, _) => scan(s),
            Kind::CountScan { spec, .. }
            | Kind::CountDistinctScan { spec, .. }
            | Kind::GroupCountScan { spec, .. } => scan(spec),
            Kind::CountFilterScan { spec, filter, .. } => {
                scan(spec);
                filter.iter().for_each(|e| e.vars(&mut filter_vars));
            }
            Kind::OrderedTopK(t) => scan(&t.scan),
            Kind::SpatialScan(t) => scan(&t.scan),
            Kind::IndexJoin(j) => {
                j.probes.iter().for_each(|p| scan(&p.scan));
                join_vars.push(j.key);
            }
            Kind::Path { spec, .. } => {
                if let Some(p) = &spec.src {
                    patterns.push(p.clone());
                }
            }
            Kind::Filter(exprs) => exprs.iter().for_each(|e| e.vars(&mut filter_vars)),
            Kind::Join { keys, .. } => join_vars.extend_from_slice(keys),
            Kind::PatternCount(spec) => {
                if let Some(p) = &spec.src {
                    patterns.push(p.clone());
                }
            }
            Kind::PatternScan(spec) => {
                if let Some(p) = &spec.src {
                    patterns.push(p.clone());
                }
            }
            // the variable a count join holds is the count's: the key is what its two
            // inputs share
            Kind::CountJoin { .. }
            | Kind::CountJoinRuns { .. }
            | Kind::LeftJoin { .. }
            | Kind::KeyedLeftJoin { .. }
            | Kind::KeyedJoin { .. }
            | Kind::Minus
            | Kind::HalfJoin { .. }
                if n.children.len() == 2 =>
            {
                join_vars = shared(&n.children[0], &n.children[1]);
            }
            _ => {}
        }
        filter_vars.sort_unstable();
        filter_vars.dedup();
        let refs = PlanRefs {
            patterns,
            filter_vars: names(ctx, &filter_vars),
            join_vars: names(ctx, &join_vars),
        };
        (refs != PlanRefs::default()).then(|| Box::new(refs))
    }
    if !ctx.plan_patterns {
        return None;
    }
    match &n.kind {
        Kind::Scan(_)
        | Kind::RangeScan(..)
        | Kind::CountScan { .. }
        | Kind::CountDistinctScan { .. }
        | Kind::GroupCountScan { .. }
        | Kind::CountFilterScan { .. }
        | Kind::OrderedTopK(_)
        | Kind::SpatialScan(_)
        | Kind::IndexJoin(_)
        | Kind::Path { .. }
        | Kind::Filter(_)
        | Kind::Join { .. }
        | Kind::CountJoin { .. }
        | Kind::CountJoinRuns { .. }
        | Kind::LeftJoin { .. }
        | Kind::KeyedLeftJoin { .. }
        | Kind::KeyedJoin { .. }
        | Kind::Minus
        | Kind::HalfJoin { .. }
        | Kind::PatternCount(_)
        | Kind::PatternScan(_) => of(ctx, n),
        _ => None,
    }
}

fn names(ctx: &Ctx, vars: &[VarId]) -> Vec<String> {
    let all = ctx.var_names.read();
    vars.iter()
        .map(|v| &all[*v as usize])
        .filter(|n| !n.starts_with(' '))
        .cloned()
        .collect()
}

/// Plan description without execution (EXPLAIN).
pub fn describe(ctx: &Ctx, n: &Node) -> PlanInfo {
    PlanInfo {
        children: described_children(ctx, n),
        ..describe_node(ctx, n)
    }
}

/// [`describe`] that stops at the query's timeout or cancellation: describing a plan at
/// the plan limit names thousands of columns for each of thousands of nodes.
pub(super) fn try_describe(ctx: &Ctx, n: &Node) -> Result<PlanInfo> {
    ctx.check()?;
    // the node before its children, so that each check follows the work of a node
    let mut info = describe_node(ctx, n);
    info.children = match &n.kind {
        Kind::OrderedTopK(spec) => vec![try_describe(ctx, &spec.fallback)?],
        _ => n
            .children
            .iter()
            .map(|c| try_describe(ctx, c))
            .collect::<Result<_>>()?,
    };
    Ok(info)
}

/// [`describe`] of `n` alone, without its children. A caller that describes each node of
/// a plan would otherwise describe each subtree once per node above it.
pub(super) fn describe_node(ctx: &Ctx, n: &Node) -> PlanInfo {
    PlanInfo {
        operator: n.operator().to_string(),
        description: n.desc.clone(),
        columns: names(ctx, &n.vars),
        sorted_on: names(ctx, &n.sorted),
        estimated_rows: n.est.round(),
        estimated_cost: n.cost.round(),
        actual_rows: -1,
        fidelity: node_fidelity(ctx, n),
        refs: node_refs(ctx, n),
        ..Default::default()
    }
}

/// The children of `n`'s description. An ordered top-k shows the plan it falls back
/// to as its only child, so that its tree has the same shape whether that plan ran or
/// not.
fn described_children(ctx: &Ctx, n: &Node) -> Vec<PlanInfo> {
    match &n.kind {
        Kind::OrderedTopK(spec) => vec![describe(ctx, &spec.fallback)],
        _ => n.children.iter().map(|c| describe(ctx, c)).collect(),
    }
}

/// Why the right side of a join did not run.
const LEFT_EMPTY: &str = "the left side produced no rows";

/// Execute with the result cache: subtrees that are cheap to recompute (leaf scans and
/// value tables) bypass it; everything else is looked up and stored.
pub fn execute(ctx: &Ctx, n: &Node) -> Result<(Table, PlanInfo)> {
    let (output, info) = execute_owned(ctx, n)?;
    Ok((output.table, info))
}
pub(super) fn execute_owned(ctx: &Ctx, n: &Node) -> Result<(Materialized, PlanInfo)> {
    execute_owned_inner(ctx, n, contains_algorithm(n))
}
fn contains_algorithm(n: &Node) -> bool {
    matches!(n.kind, Kind::AlgoCall(_)) || n.children.iter().any(contains_algorithm)
}
fn execute_owned_inner(ctx: &Ctx, n: &Node, algorithm: bool) -> Result<(Materialized, PlanInfo)> {
    ctx.check()
        .inspect_err(|_| fail(ctx, n, Instant::now(), Vec::new()))?;
    let results = &ctx.snap.results;
    let cacheable = ctx.snap.cacheable
        && ctx.use_cache
        && results.enabled()
        && !matches!(
            n.kind,
            Kind::Scan(_)
                | Kind::RangeScan(..)
                | Kind::Values(_)
                | Kind::Empty
                | Kind::CountScan { .. }
                | Kind::CountDistinctScan { .. }
                | Kind::GroupCountScan { .. }
                | Kind::CountJoinRuns { .. }
        );
    // (spatial operators are cached: an index search is not cheap to repeat)
    let key = if cacheable {
        super::cache::key(n, ctx)
    } else {
        None
    };
    if let Some(k) = &key {
        static DEBUG: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        if *DEBUG.get_or_init(|| std::env::var_os("SPARKLES_DEBUG_CACHE").is_some()) {
            eprintln!("CACHEKEY {} :: {}", n.operator(), k.key);
        }
        let start = Instant::now();
        let hit = if algorithm {
            results.get_owned(k, ctx)?
        } else {
            results.get(k, ctx)?.map(Materialized::plain)
        };
        if let Some(t) = hit {
            ctx.produced(t.table.len())?;
            let mut info = describe(ctx, n);
            info.actual_rows = t.table.len() as i64;
            info.time_ms = start.elapsed().as_secs_f64() * 1000.0;
            info.cached = true;
            // the subtree it was computed from, which did not run this time
            info.children = std::mem::take(&mut info.children)
                .into_iter()
                .map(|c| c.skip("the result of a parent was read from the result cache"))
                .collect();
            return Ok((t, info));
        }
    }
    let (t, info) = execute_uncached(ctx, n, algorithm)?;
    if let Some(k) = key {
        // only worth caching when recomputation is not trivially cheap
        if info.time_ms >= results.min_ms {
            results.put(k, &t.table, ctx);
        }
    }
    Ok((t, info))
}

fn execute_uncached(ctx: &Ctx, n: &Node, algorithm: bool) -> Result<(Materialized, PlanInfo)> {
    execute_node(ctx, n, None, algorithm, None)
}

/// The inputs of an operator, from a caller that produces each child's table itself.
pub(super) type Inputs<'a> = &'a mut dyn FnMut(usize) -> Result<(Materialized, PlanInfo)>;

/// Whether `execute_with_inputs` reads every child of a node of this kind only as an
/// input table. Other kinds run a child plan themselves, read their child scans in
/// their own way, or may never need a child.
pub(super) fn reads_inputs(n: &Node) -> bool {
    match &n.kind {
        Kind::CountJoinRuns { .. } | Kind::SpatialKnn(_) | Kind::Join { .. } => false,
        Kind::VectorSearch(spec) => !spec.order_fallback,
        Kind::Slice { limit, .. } => limit.is_none(),
        _ => true,
    }
}

/// Run one operator over input tables that `inputs` produces, child by child, without
/// the result cache. Only kinds that `reads_inputs` admits may run this way.
pub(super) fn execute_with_inputs(
    ctx: &Ctx,
    n: &Node,
    inputs: Inputs<'_>,
) -> Result<(Materialized, PlanInfo)> {
    debug_assert!(reads_inputs(n));
    execute_node(ctx, n, Some(inputs), contains_algorithm(n), None)
}

fn execute_node(
    ctx: &Ctx,
    n: &Node,
    inputs: Option<Inputs<'_>>,
    algorithm: bool,
    equality: Option<(VarId, VarId)>,
) -> Result<(Materialized, PlanInfo)> {
    let start = Instant::now();
    let mut failing = Failing {
        ctx,
        n,
        start,
        infos: Vec::new(),
        armed: true,
    };
    // The result goes straight to the caller, and the guard keeps the plan only when
    // `run_node` returns an error, so the success path pays nothing for it.
    run_node(
        ctx,
        n,
        inputs,
        start,
        &mut failing.infos,
        &mut failing.armed,
        algorithm,
        equality,
    )
}

/// Keeps the plan of a node that failed, as far as it ran (see [`fail`]), when it is
/// dropped while still armed: `run_node` disarms it just before it succeeds.
struct Failing<'a> {
    ctx: &'a Ctx,
    n: &'a Node,
    start: Instant,
    infos: Vec<PlanInfo>,
    armed: bool,
}

impl Drop for Failing<'_> {
    fn drop(&mut self) {
        if self.armed && !std::thread::panicking() {
            fail(
                self.ctx,
                self.n,
                self.start,
                std::mem::take(&mut self.infos),
            );
        }
    }
}

/// Keep the plan of `n`, which failed after `start` with its first children's plans in
/// `infos`, for its parent (see [`fail_at`]).
#[cold]
fn fail(ctx: &Ctx, n: &Node, start: Instant, infos: Vec<PlanInfo>) {
    fail_at(ctx, n, start, infos.into_iter().enumerate().collect());
}

/// Keep the plan of `n`, which failed after `start`, for its parent: the children in
/// `known` by index, the partial plan of a child that failed, and the others marked as
/// never run. The node's own counts are unknown, and it is marked incomplete.
#[cold]
fn fail_at(ctx: &Ctx, n: &Node, start: Instant, mut known: Vec<(usize, PlanInfo)>) {
    let mut info = describe(ctx, n);
    info.time_ms = start.elapsed().as_secs_f64() * 1000.0;
    info.fidelity_mut().incomplete = true;
    for (i, c) in n.children.iter().enumerate() {
        if !known.iter().any(|(k, _)| *k == i)
            && let Some(p) = ctx.take_failed(c)
        {
            known.push((i, p));
        }
    }
    let described = std::mem::take(&mut info.children);
    info.children = described
        .into_iter()
        .enumerate()
        .map(|(i, d)| match known.iter().position(|(k, _)| *k == i) {
            Some(at) => known.swap_remove(at).1,
            None => d.skip("the query stopped before it ran"),
        })
        .collect();
    if ctx.graphs.is_some() {
        info.redact();
    }
    ctx.set_failed(n, info);
}

// One caller: inlined so that the success path returns its table and plan directly.
#[inline(always)]
#[allow(clippy::too_many_arguments)]
fn run_node(
    ctx: &Ctx,
    n: &Node,
    mut inputs: Option<Inputs<'_>>,
    start: Instant,
    infos: &mut Vec<PlanInfo>,
    failing: &mut bool,
    algorithm: bool,
    equality: Option<(VarId, VarId)>,
) -> Result<(Materialized, PlanInfo)> {
    // the children's tables count against the memory budget until this operator is done
    let held = ctx.charge(0)?;
    let owned_input = std::cell::Cell::new(false);
    let filtered_cross = inputs.is_none().then(|| string_cross_hint(n)).flatten();
    let mut child = |i: usize, infos: &mut Vec<PlanInfo>| -> Result<Table> {
        let (mut t, info) = match inputs.as_mut() {
            Some(inputs) => inputs(i)?,
            // A filtered subset is not the unfiltered child's result. In particular,
            // never look it up or store it under the ordinary Cross cache key. The
            // logical parent Filter and both complete inputs still cache normally.
            None if i == 0 && filtered_cross.is_some() => {
                execute_node(ctx, &n.children[i], None, algorithm, filtered_cross)?
            }
            None => execute_owned_inner(
                ctx,
                &n.children[i],
                algorithm && contains_algorithm(&n.children[i]),
            )?,
        };
        infos.push(info);
        if t.lease.is_some() {
            held.adopt(&mut t.lease)?;
            owned_input.set(true);
        } else {
            held.add(t.table.mem_bytes())?;
        }
        Ok(t.table)
    };
    // runtime detail for EXPLAIN (which variant of the operator ran)
    let mut note: Option<String> = None;
    let mut counters = None;
    // expressions evaluated per distinct value
    let mut expr_report = ExprReport::default();
    let mut output_lease = None;
    let mut table = match &n.kind {
        Kind::Empty => Table::empty(n.vars.clone()),
        Kind::Values(t) => t.clone(),
        Kind::Scan(spec) => {
            note = column_note(ctx, spec);
            scan(ctx, spec, &n.vars)?
        }
        Kind::RangeScan(spec, range) => {
            note = column_note(ctx, spec);
            range_scan(ctx, spec, range, &n.vars)?
        }
        Kind::OrderedTopK(spec) => match ordered_topk(ctx, spec, &n.vars)? {
            Some((t, read)) => {
                note = Some(format!("[read {read} rows]"));
                infos.push(describe(ctx, &spec.fallback).skip("the ordered index answered"));
                t
            }
            None => {
                // the values are not totally ordered: the generic plan decides
                let (mut out, info) = execute_owned_inner(
                    ctx,
                    &spec.fallback,
                    algorithm && contains_algorithm(&spec.fallback),
                )?;
                if out.lease.is_some() {
                    held.adopt(&mut out.lease)?;
                    owned_input.set(true);
                }
                infos.push(info);
                note = Some("[values not totally ordered: ran the generic plan]".into());
                out.table
            }
        },
        Kind::GroupCountScan {
            spec,
            key: _,
            counts,
            metadata,
        } => match metadata {
            Some(c) => metadata_counts(c, &n.vars, counts.len()),
            None => group_count_scan(ctx, spec, &n.vars, counts.len())?,
        },
        Kind::CountJoinRuns { var } => {
            let mut sides = Vec::with_capacity(2);
            for c in &n.children {
                let t0 = Instant::now();
                let Kind::Scan(spec) = &c.kind else {
                    unreachable!("CountJoinRuns children are scans")
                };
                let runs = key_runs(ctx, spec)?;
                let mut info = describe(ctx, c);
                info.actual_rows = runs.0.len() as i64;
                info.time_ms = t0.elapsed().as_secs_f64() * 1000.0;
                infos.push(info);
                held.add(super::ctx::table_bytes(runs.0.len(), 2))?;
                sides.push(runs);
            }
            let ((lk, lc), (rk, rc)) = (&sides[0], &sides[1]);
            let (mut i, mut j, mut total) = (0, 0, 0u64);
            while i < lk.len() && j < rk.len() {
                match lk[i].cmp(&rk[j]) {
                    Ordering::Less => i += 1,
                    Ordering::Greater => j += 1,
                    Ordering::Equal => {
                        total = total.saturating_add(lc[i].saturating_mul(rc[j]));
                        i += 1;
                        j += 1;
                    }
                }
            }
            let mut t = Table::new(vec![*var]);
            t.push_row(&[Id::from_i64(total.min(i64::MAX as u64) as i64).unwrap_or(Id::UNDEF)]);
            t
        }
        Kind::CountScan { spec, var } => {
            let c = ctx.snap.count(spec.perm, &spec.prefix)?;
            let mut t = Table::new(vec![*var]);
            t.push_row(&[Id::from_i64(c as i64).unwrap_or(Id::UNDEF)]);
            t
        }
        Kind::Join { algo, .. } => {
            let l = child(0, infos)?;
            if l.is_empty() {
                infos.push(describe(ctx, &n.children[1]).skip(LEFT_EMPTY));
                Table::empty(n.vars.clone())
            } else {
                let r = match text_pushdown(ctx, &l, &n.children[1])? {
                    Some((r, info)) => {
                        infos.push(info);
                        held.add(r.mem_bytes())?;
                        r
                    }
                    None => child(1, infos)?,
                };
                match algo {
                    JoinAlgo::Cross => match equality {
                        Some(keys) => match string_cross(ctx, n, &l, &r, keys)? {
                            Some(t) => {
                                note = Some(
                                    "[string equality candidates; residual FILTER retained]".into(),
                                );
                                t
                            }
                            None => cross(ctx, &l, &r)?,
                        },
                        None => cross(ctx, &l, &r)?,
                    },
                    JoinAlgo::Merge => join_noted(ctx, &l, &r, true, &mut note)?,
                    JoinAlgo::Hash => join_noted(ctx, &l, &r, false, &mut note)?,
                }
            }
        }
        Kind::CountDistinctScan {
            spec,
            var,
            metadata,
        } => {
            let c = match metadata {
                Some(c) => *c,
                None => count_distinct_scan(ctx, spec)?,
            };
            let mut t = Table::new(vec![*var]);
            t.push_row(&[Id::from_i64(c as i64).unwrap_or(Id::UNDEF)]);
            t
        }
        Kind::CountFilterScan {
            spec,
            key,
            filter,
            var,
            distinct,
        } => {
            let (c, why) = count_filter_scan(ctx, spec, *key, filter, *distinct)?;
            note = Some(why);
            let mut t = Table::new(vec![*var]);
            t.push_row(&[Id::from_i64(c as i64).unwrap_or(Id::UNDEF)]);
            t
        }
        Kind::CountJoin { algo, var } => {
            let l = child(0, infos)?;
            let r = if l.is_empty() {
                infos.push(describe(ctx, &n.children[1]).skip(LEFT_EMPTY));
                Table::empty(n.children[1].vars.clone())
            } else {
                child(1, infos)?
            };
            let c = if l.is_empty() || r.is_empty() {
                0
            } else {
                join_count(ctx, &l, &r, *algo == JoinAlgo::Merge)?
            };
            let mut t = Table::new(vec![*var]);
            t.push_row(&[Id::from_i64(c as i64).unwrap_or(Id::UNDEF)]);
            t
        }
        Kind::Unpack { t, parts } => {
            let input = child(0, infos)?;
            unpack(ctx, input, *t, parts, &n.vars)?
        }
        Kind::IndexJoin(spec) => {
            let l = child(0, infos)?;
            let (t, stats) = super::indexjoin::run(ctx, spec, &l, &n.vars)?;
            note = Some(stats.note());
            counters = Some(stats.counters());
            t
        }
        Kind::LeftJoin { expr } => {
            let l = child(0, infos)?;
            let r = child(1, infos)?;
            left_join(ctx, &l, &r, expr.as_ref(), &mut note)?
        }
        Kind::KeyedLeftJoin { expr, left, right } => {
            let l = child(0, infos)?;
            let r = child(1, infos)?;
            keyed_left_join(ctx, &l, &r, expr, (*left, *right), &mut note)?
        }
        Kind::KeyedJoin {
            expr, left, right, ..
        } => {
            let l = child(0, infos)?;
            let r = child(1, infos)?;
            keyed_join(ctx, &l, &r, expr, (*left, *right), false, &mut note)?
        }
        Kind::Minus => {
            let l = child(0, infos)?;
            let r = child(1, infos)?;
            minus(ctx, l, &r, &mut note)?
        }
        Kind::HalfJoin { anti } => {
            let l = child(0, infos)?;
            let r = child(1, infos)?;
            half_join(ctx, l, &r, *anti)?
        }
        Kind::Union => {
            let mut out = Table::new(n.vars.clone());
            for i in 0..n.children.len() {
                out.append(child(i, infos)?);
                ctx.check_output(out.len(), out.width())?;
            }
            out
        }
        Kind::Filter(es) => {
            let runs = match &n.children[0].kind {
                Kind::Scan(spec) if ctx.opt.filter_scan_runs => {
                    filter_scan_runs(ctx, spec, &n.children[0], es)?
                }
                _ => None,
            };
            let (mut t, rest, runs_note) = match runs {
                Some(r) => {
                    let mut info = describe(ctx, &n.children[0]);
                    info.actual_rows = r.read as i64;
                    infos.push(info);
                    held.add(r.table.mem_bytes())?;
                    (r.table, r.rest, Some(r.note))
                }
                None => (child(0, infos)?, es.clone(), None),
            };
            expr_report = apply_filter(ctx, &mut t, &rest)?;
            (note, counters) = super::exists::explain(ctx, es);
            if let Some(r) = runs_note {
                note = Some(match note {
                    Some(n) => format!("{r} {n}"),
                    None => r,
                });
            }
            t
        }
        Kind::Extend(v, e) => {
            let mut t = child(0, infos)?;
            ctx.check_output(t.len(), 1)?;
            let col = compute_column(ctx, &t, e, &mut expr_report)?;
            t.vars.push(*v);
            t.cols.push(col);
            t
        }
        Kind::Assign(v, e) => {
            let mut t = child(0, infos)?;
            let col = compute_column(ctx, &t, e, &mut expr_report)?;
            assign(ctx, &mut t, *v, col)?;
            t
        }
        Kind::Unfold { expr, var, second } => {
            let t = child(0, infos)?;
            unfold(ctx, &t, expr, *var, *second, &mut expr_report)?
        }
        Kind::Sort(vars) => {
            let mut t = child(0, infos)?;
            // sorting copies the rows
            ctx.check_output(t.len(), t.width())?;
            t.sort_by_vars(vars);
            t
        }
        Kind::OrderBy { keys, limit } => {
            let t = child(0, infos)?;
            let (t, pre) = order_by(ctx, t, keys, *limit, &mut expr_report)?;
            note = pre;
            t
        }
        Kind::Project(vars) => child(0, infos)?.project(vars),
        Kind::Distinct => distinct(child(0, infos)?),
        Kind::PatternCount(spec) => {
            let input = if n.children.is_empty() {
                None
            } else {
                Some(child(0, infos)?)
            };
            let (t, nt) = super::patternplan::run_count(ctx, spec, input.as_ref(), &n.vars)?;
            note = Some(nt);
            t
        }
        Kind::PatternScan(spec) => {
            let (t, nt, _) = super::patternplan::run_scan(ctx, spec, &n.vars, None)?;
            note = Some(nt);
            t
        }
        Kind::Slice {
            offset,
            limit: Some(limit),
        } => {
            // LIMIT without ORDER BY: any `offset + limit` solutions will do, so the
            // input is computed with early termination
            let (mut t, info, _) =
                execute_limited(ctx, &n.children[0], offset.saturating_add(*limit))?;
            infos.push(info);
            if t.lease.is_some() {
                held.adopt(&mut t.lease)?;
                owned_input.set(true);
            } else {
                held.add(t.table.mem_bytes())?;
            }
            t.table.slice(*offset, Some(*limit))
        }
        Kind::Slice { offset, limit } => child(0, infos)?.slice(*offset, *limit),
        Kind::Group { keys, aggs } => {
            let t = child(0, infos)?;
            group(ctx, &t, keys, aggs, &mut expr_report)?
        }
        Kind::Path {
            spec,
            bound_from_left,
        } => {
            let mut inputs = Vec::new();
            for i in 0..n.children.len() {
                inputs.push(child(i, infos)?);
            }
            let (t, sweeps) = path(ctx, spec, *bound_from_left, inputs, &n.vars)?;
            if sweeps > 0 {
                note = Some(format!(
                    "[{sweeps} frontier levels expanded by index sweeps]"
                ));
            }
            t
        }
        Kind::Lateral(spec) if spec.service.is_some() => {
            let l = child(0, infos)?;
            let rl = spec.service.as_deref().expect("a remote loop");
            let (t, st) = super::enhancer::run_remote(ctx, spec, rl, &l, &n.vars)?;
            note = Some(format!(
                "[{} inputs, {} requests, {} from the cache{}]",
                st.inputs,
                st.requests,
                st.cache_hits,
                if st.retried > 0 {
                    format!(", {} bulk requests cut short and sent again", st.retried)
                } else {
                    String::new()
                }
            ));
            let mut c = Counters::new();
            c.insert("serviceInputs".into(), st.inputs.into());
            c.insert("serviceRequests".into(), st.requests.into());
            c.insert("serviceCacheHits".into(), st.cache_hits.into());
            c.insert("serviceRetried".into(), st.retried.into());
            c.insert("serviceSolutions".into(), st.solutions.into());
            counters = Some(c);
            t
        }
        Kind::Lateral(spec) => {
            let l = child(0, infos)?;
            let (t, groups, solutions) = super::lateral::run(ctx, spec, &l, &n.vars)?;
            note = Some(format!("[{groups} groups evaluated]"));
            let mut c = Counters::new();
            c.insert("lateralGroups".into(), groups.into());
            c.insert("lateralSolutions".into(), solutions.into());
            counters = Some(c);
            t
        }
        Kind::RegisteredProperty(spec) => {
            let input = child(0, infos)?;
            super::propertyext::run(ctx, spec, &input, &n.vars)?
        }
        Kind::PropertyFn(spec) => {
            let input = match n.children.len() {
                0 => None,
                _ => Some(child(0, infos)?),
            };
            super::arqpf::run(ctx, spec, input.as_ref(), &n.vars)?
        }
        Kind::TextSearch(spec) if !n.children.is_empty() => {
            let input = child(0, infos)?;
            let (t, c) = text_bound(ctx, spec, &input, &mut note)?;
            counters = Some(c);
            t
        }
        Kind::TextSearch(spec) => crate::text::search(ctx, spec, &n.vars)?,
        Kind::VectorSearch(spec) if spec.order_fallback => {
            // the k best rows of an ORDER BY: the generic plan when fewer than k rows
            // have a score, or when the search cannot run
            let searched = match vector_search(ctx, spec, None, &n.vars) {
                Ok((t, c)) if t.len() >= spec.k => Some((t, c)),
                Ok(_) => None,
                Err(Error::BudgetExceeded(_) | Error::Invalid(_)) => None,
                Err(e) => return Err(e),
            };
            match searched {
                Some((t, c)) => {
                    infos.push(describe(ctx, &n.children[0]).skip("the vector index answered"));
                    counters = Some(c);
                    t
                }
                None => {
                    note = Some(format!(
                        "[fewer than {} rows have a score: ran the generic plan]",
                        spec.k
                    ));
                    child(0, infos)?
                }
            }
        }
        Kind::VectorSearch(spec) => {
            let input = match n.children.len() {
                0 => None,
                _ => Some(child(0, infos)?),
            };
            let (t, c) = vector_search(ctx, spec, input, &n.vars)?;
            counters = Some(c);
            t
        }
        Kind::PathSearch(spec) => {
            // the input when the search reads its group, then the nested pattern of edges
            let reads_input = n.children.len() > spec.edge_pattern.is_some() as usize;
            let input = match reads_input {
                false => None,
                true => Some(child(0, infos)?),
            };
            let edges = match spec.edge_pattern {
                Some(_) => Some(child(n.children.len() - 1, infos)?),
                None => None,
            };
            let (t, c) = super::pathsearch::run(ctx, spec, input, edges, &n.vars)?;
            counters = Some(c);
            t
        }
        Kind::AlgoCall(spec) => {
            // the input when a source is a variable, then the nested pattern of edges
            let reads_input = n.children.len() > spec.proj.pattern.is_some() as usize;
            let input = match reads_input {
                false => None,
                true => Some(child(0, infos)?),
            };
            let edges = match spec.proj.pattern {
                Some(_) => Some(child(n.children.len() - 1, infos)?),
                None => None,
            };
            let (t, c) = super::algosvc::run(ctx, spec, input, edges, &n.vars)?;
            counters = Some(c);
            output_lease = t.lease;
            t.table
        }
        Kind::ViewRead(spec) => super::viewsvc::run(ctx, spec, &n.vars)?,
        Kind::HistoryChanges(spec) => {
            let input = match n.children.len() {
                0 => None,
                _ => Some(child(0, infos)?),
            };
            super::history_svc::run(ctx, spec, input.as_ref(), &n.vars)?
        }
        Kind::HybridSearch(spec) if !n.children.is_empty() => {
            let input = child(0, infos)?;
            let (t, c) = super::hybrid::search_bound(ctx, spec, &input, &mut note)?;
            counters = Some(c);
            t
        }
        Kind::HybridSearch(spec) => {
            let (t, c) = super::hybrid::search(ctx, spec, &n.vars)?;
            counters = Some(c);
            t
        }
        Kind::SpatialScan(spec) => {
            let (t, c) = spatial_scan(ctx, spec, &n.vars)?;
            counters = Some(c);
            t
        }
        Kind::SpatialPf(spec) => {
            let input = match n.children.len() {
                0 => None,
                _ => Some(child(0, infos)?),
            };
            let (t, c) = spatial_pf(ctx, spec, input, &n.vars)?;
            counters = Some(c);
            t
        }
        Kind::SpatialJoin(spec) => {
            let mut inputs = Vec::with_capacity(n.children.len());
            for i in 0..n.children.len() {
                inputs.push(child(i, infos)?);
            }
            let (t, c) = spatial_join(ctx, spec, inputs, &n.vars)?;
            counters = Some(c);
            t
        }
        Kind::SpatialKnn(spec) => {
            // the template runs once per batch of candidates
            let (t, c, runs) = spatial_knn(ctx, spec, &n.children[0], &n.vars)?;
            // one node for the template, its runs added up
            infos.push(
                runs.into_iter()
                    .reduce(PlanInfo::merge_run)
                    .unwrap_or_else(|| {
                        describe(ctx, &n.children[0]).skip("no candidate needed it")
                    }),
            );
            counters = Some(c);
            t
        }
        Kind::SpatialRelate(spec) => {
            let (t, c) = spatial_relate(ctx, spec, &n.vars)?;
            counters = Some(c);
            t
        }
        Kind::Service {
            endpoint,
            query,
            silent,
            cache,
        } => match service(ctx, endpoint, query, &n.vars, *cache) {
            Ok(t) => t,
            // SILENT hides failures of the remote service, not a refusal or a spent
            // budget
            Err(e @ (Error::NotPermitted(_) | Error::BudgetExceeded(_))) => return Err(e),
            Err(_) if *silent => Table::unit(),
            Err(e) => return Err(e),
        },
    };
    if !matches!(
        n.kind,
        Kind::Scan(_)
            | Kind::RangeScan(..)
            | Kind::Sort(_)
            | Kind::Join {
                algo: JoinAlgo::Merge,
                ..
            }
    ) {
        // the planner's order holds when the rows are sorted on it, perhaps on more
        if table.sorted.starts_with(&n.sorted) {
            table.sorted.truncate(n.sorted.len());
        } else {
            table.sorted.clear();
        }
    } else {
        table.sorted = n.sorted.clone();
    }
    if let Some(x) = expr_report.note() {
        note = Some(note.map_or(x.clone(), |n| format!("{n} {x}")));
        counters = merge_counters(counters, expr_report.counters());
    }
    ctx.check()?;
    // the inputs are gone (or became the output): only the output is alive now
    if output_lease.is_none() && owned_input.get() {
        output_lease = Some(held.output_lease(super::algosvc::table_capacity(&table)?)?);
    }
    drop(held);
    if let Some(lease) = &mut output_lease {
        lease.resize(ctx.mem_limit, super::algosvc::table_capacity(&table)?)?;
        ctx.check_output_rows(table.len())?;
    } else {
        ctx.check_output(table.len(), table.width())?;
    }
    ctx.produced(table.len())?;
    let info = PlanInfo {
        fidelity: node_fidelity(ctx, n),
        refs: node_refs(ctx, n),
        operator: n.operator().to_string(),
        description: match note {
            Some(x) => format!("{} {x}", n.desc),
            None => n.desc.clone(),
        },
        columns: names(ctx, &n.vars),
        sorted_on: names(ctx, &n.sorted),
        estimated_rows: n.est.round(),
        estimated_cost: n.cost.round(),
        actual_rows: table.len() as i64,
        time_ms: start.elapsed().as_secs_f64() * 1000.0,
        cached: false,
        children: std::mem::take(infos),
        counters,
        warnings: Vec::new(),
    };
    *failing = false;
    Ok((
        Materialized {
            table,
            lease: output_lease,
        },
        info,
    ))
}

/// Both operators' counters (the expression cache's next to those of EXISTS).
fn merge_counters(
    a: Option<serde_json::Map<String, serde_json::Value>>,
    b: Option<serde_json::Map<String, serde_json::Value>>,
) -> Option<serde_json::Map<String, serde_json::Value>> {
    match (a, b) {
        (Some(mut a), Some(b)) => {
            a.extend(b);
            Some(a)
        }
        (a, b) => a.or(b),
    }
}

// ------------------------------------------------------------------ scans ------

/// Ask the kernel for the blocks of the scans of a plan before any operator runs (spec X07
/// §5.2). A scan with few blocks is then read in parallel instead of one block after the
/// other, and the two blocks at the ends of its range, which `count` decodes while the
/// scan is set up, arrive together with the rest. The scan still decodes its blocks as it
/// goes, so that decoding overlaps the reads of the blocks after them.
///
/// Each block is asked for the columns the reader decodes: a block wholly inside the range
/// needs the scan's columns only, and a block at an end of the range also the columns
/// that bound it. A range scan asks for the blocks of its id ranges. A scan whose first
/// block is cached is skipped, so a warm plan costs one lookup per scan. A scan of many
/// blocks, or one under a `LIMIT`, reads ahead as it goes instead (see
/// [`crate::index::ReadAhead`]).
pub(super) fn prefetch_scans(ctx: &Ctx, n: &Node) {
    /// scans of more blocks than this are left to their own read-ahead
    const FEW_BLOCKS: usize = 64;
    if !ctx.opt.prefetch_first_scan || !crate::index::io_hints() {
        return;
    }
    /// The columns the reader decodes of block `b` of the range `[lo, hi]`: the scan's own
    /// inside the range, and the ones that bound it at an end.
    fn wanted(
        index: &crate::index::PermIndex,
        scan: ColMask,
        (lo, hi): &(Key, Key),
        b: usize,
    ) -> ColMask {
        let m = &index.blocks[b];
        if m.first >= *lo && m.last <= *hi {
            scan
        } else {
            scan | crate::index::bound_cols(lo, hi)
        }
    }
    /// The blocks of a scan with the columns the reader decodes of each, or nothing for a
    /// scan of more than `FEW_BLOCKS` blocks.
    fn blocks(
        ctx: &Ctx,
        spec: &ScanSpec,
        ranges: &[(Key, Key)],
    ) -> Vec<(usize, ColMask, Key, Key)> {
        let index = ctx.snap.perm(spec.perm);
        let scan = scan_mask(ctx, spec);
        let mut out: Vec<(usize, ColMask, Key, Key)> = Vec::new();
        for range in ranges {
            let (b0, b1) = index.key_block_range(&range.0, &range.1);
            if b1 - b0 > FEW_BLOCKS {
                return Vec::new();
            }
            for b in b0..b1 {
                let mask = wanted(index, scan, range, b);
                // a block that two ranges share is asked for by its union
                match out.last_mut() {
                    Some((last, wanted, _, hi)) if *last == b => {
                        *wanted |= mask;
                        *hi = range.1;
                    }
                    _ => out.push((b, mask, range.0, range.1)),
                }
            }
            if out.len() > FEW_BLOCKS {
                return Vec::new();
            }
        }
        out
    }
    fn walk(ctx: &Ctx, n: &Node, limited: bool) {
        // (a `COUNT` scan answers from the block metadata and decodes at most the blocks at
        // the ends of its range, which `count` asks for itself)
        let ranges = match &n.kind {
            Kind::Scan(spec) if !limited => Some((
                spec,
                vec![(pad(&spec.prefix, 0), pad(&spec.prefix, u64::MAX))],
            )),
            Kind::RangeScan(spec, range) if !limited => {
                let at = |v: u64, fill: u64| {
                    let mut k = pad(&spec.prefix, fill);
                    k[spec.prefix.len()] = v;
                    k
                };
                let ranges = (range.ranges.iter())
                    .map(|r| (at(r.lo, 0), at(r.hi, u64::MAX)))
                    .collect();
                Some((spec, ranges))
            }
            _ => None,
        };
        if let Some((spec, ranges)) = ranges {
            let index = ctx.snap.perm(spec.perm);
            // a warm scan costs this one probe, before any other block is listed
            let first = ranges.iter().find_map(|r| {
                let (b0, b1) = index.key_block_range(&r.0, &r.1);
                (b0 < b1).then(|| (b0, wanted(index, scan_mask(ctx, spec), r, b0), r))
            });
            if let Some((b, mask, r)) = first
                && !index.range_cached(&ctx.snap.cache, b, &r.0, &r.1, mask)
            {
                // a range of a few rows of a segmented block asks for its segments only
                for (b, mask, lo, hi) in blocks(ctx, spec, &ranges) {
                    index.prefetch_range(&ctx.snap.cache, b, &lo, &hi, mask);
                }
            }
        }
        let limited = limited || matches!(n.kind, Kind::Slice { limit: Some(_), .. });
        for c in &n.children {
            walk(ctx, c, limited);
        }
    }
    walk(ctx, n, false);
    crate::ioacct::mark("scans prefetched");
}

/// Key columns a scan reads: its variables, the graph (unless every graph passes) and
/// the repeated-variable columns. The others are never decoded for whole blocks.
pub(super) fn scan_mask(ctx: &Ctx, spec: &ScanSpec) -> crate::index::ColMask {
    if !ctx.opt.selective_columns {
        return crate::index::ALL_COLS;
    }
    let mut m: crate::index::ColMask = 0;
    for &(c, _) in &spec.cols {
        m |= 1 << c;
    }
    if !matches!(spec.graph, GraphFilter::All) {
        m |= 1 << spec.graph_col;
    }
    for &(a, b) in &spec.eqs {
        m |= (1 << a) | (1 << b);
    }
    m
}

/// EXPLAIN note naming the key columns a scan decodes, when it skips any.
fn column_note(ctx: &Ctx, spec: &ScanSpec) -> Option<String> {
    let m = scan_mask(ctx, spec);
    (m != crate::index::ALL_COLS).then(|| {
        let names: Vec<&str> = (0..4)
            .filter(|c| m & (1 << c) != 0)
            .map(|c| ["S", "P", "O", "G"][spec.perm.order()[c]])
            .collect();
        format!("[decodes {}]", names.join(""))
    })
}

/// Whether every row of `b[s..e]` passes the scan's graph filter and repeated-variable
/// checks, so the slice can be taken column-wise (for a default-graph query over a store
/// with few or no named graphs, this is one pass over the graph column).
pub(super) fn block_passes(spec: &ScanSpec, b: &Block, s: usize, e: usize) -> bool {
    spec.eqs.is_empty()
        && (matches!(spec.graph, GraphFilter::All)
            || b.cols[spec.graph_col][s..e]
                .iter()
                .all(|&g| spec.graph.accepts(g)))
}

/// Counts per key from the index statistics (exact for the snapshot), in key order like
/// the index runs.
fn metadata_counts(c: &super::stats::Counts, vars: &[VarId], naggs: usize) -> Table {
    let mut t = Table::new(vars.to_vec());
    t.cols[0] = c.counts.iter().map(|&(k, _)| Id(k)).collect();
    let cnt: Vec<Id> = c
        .counts
        .iter()
        .map(|&(_, n)| Id::from_i64(n as i64).unwrap_or(Id::UNDEF))
        .collect();
    for a in 0..naggs {
        t.cols[1 + a] = cnt.clone();
    }
    t.len = c.counts.len();
    t
}

/// Count runs of the first free key column of a scan (graph filter / repeated variables /
/// union-graph dedup applied row by row; plain blocks counted directly).
fn group_count_scan(ctx: &Ctx, spec: &ScanSpec, vars: &[VarId], naggs: usize) -> Result<Table> {
    let (keys, counts) = key_runs(ctx, spec)?;
    let mut t = Table::new(vars.to_vec());
    let cnt: Vec<Id> = counts
        .iter()
        .map(|&c| Id::from_i64(c as i64).unwrap_or(Id::UNDEF))
        .collect();
    t.len = keys.len();
    t.cols[0] = keys;
    for a in 0..naggs {
        t.cols[1 + a] = cnt.clone();
    }
    Ok(t)
}

/// Length of the run of `v` at the start of the sorted slice `col`: a short linear probe
/// (most runs are short), then binary search for long runs.
#[inline]
fn run_len(col: &[u64], v: u64) -> usize {
    const PROBE: usize = 16;
    let end = col.len().min(PROBE);
    match col[..end].iter().position(|x| *x != v) {
        Some(n) => n,
        None if end < col.len() => end + col[end..].partition_point(|x| *x == v),
        None => end,
    }
}

/// End of the run of ids equal to `col[i]` in a sorted column: short runs are stepped
/// through instead of binary-searching the rest of the column.
#[inline]
pub(super) fn run_end(col: &[Id], i: usize) -> usize {
    const PROBE: usize = 16;
    let v = col[i];
    let end = col.len().min(i + PROBE);
    match col[i..end].iter().position(|x| *x != v) {
        Some(n) => i + n,
        None if end < col.len() => end + col[end..].partition_point(|x| *x == v),
        None => end,
    }
}

/// Key columns read for the runs of a scan's first free column: that column and the
/// columns of the graph filter and repeated-variable checks, or every variable column
/// when duplicates across graphs are dropped (rows are compared on all of them).
fn run_mask(ctx: &Ctx, spec: &ScanSpec) -> crate::index::ColMask {
    let all = scan_mask(ctx, spec);
    if spec.dedup || all == crate::index::ALL_COLS {
        return all;
    }
    let mut m: crate::index::ColMask = 1 << spec.cols[0].0;
    if !matches!(spec.graph, GraphFilter::All) {
        m |= 1 << spec.graph_col;
    }
    for &(a, b) in &spec.eqs {
        m |= (1 << a) | (1 << b);
    }
    m
}

/// COUNT over a FILTER on the first free key column of a scan: the filter is tested
/// once per distinct value of the column (on vocabulary keys when it can be), and the
/// rows of the values that pass are counted from the runs, or the values themselves for
/// `COUNT(DISTINCT)`. Also returns the EXPLAIN note.
fn count_filter_scan(
    ctx: &Ctx,
    spec: &ScanSpec,
    key: VarId,
    filter: &[Expr],
    distinct: bool,
) -> Result<(u64, String)> {
    let kf = super::keyfilter::KeyFilter::new_for(ctx, filter, key);
    if let Some(kf) = &kf
        && let Some(c) = par_count_on_keys(ctx, spec, kf, &key_ranges(ctx, spec, Some(kf)))?
    {
        // the other values: inline literals and blank nodes
        let ids: Vec<Id> = c.other.iter().map(|r| r.0).collect();
        let (hit, _) = super::exprcache::filter_values(ctx, &ids, key, filter)?;
        let (mut rows, mut passed) = (c.rows, c.passed);
        for (h, (_, n)) in hit.iter().zip(&c.other) {
            if *h {
                rows += n;
                passed += 1;
            }
        }
        let tested = c.tested + ids.len() as u64;
        let n = if distinct { passed } else { rows };
        return Ok((
            n,
            format!(
                "[{tested} values{} tested on vocabulary keys, {passed} passed]",
                ranges_note(c.ranges)
            ),
        ));
    }
    let (keys, counts) = key_runs(ctx, spec)?;
    ctx.check()?;
    let (hit, on_keys) = super::exprcache::filter_values(ctx, &keys, key, filter)?;
    let passed = hit.iter().filter(|h| **h).count();
    let n = if distinct {
        passed as u64
    } else {
        hit.iter()
            .zip(&counts)
            .filter(|(h, _)| **h)
            .map(|(_, c)| *c)
            .sum()
    };
    let note = format!(
        "[{} values tested{}, {passed} passed]",
        keys.len(),
        if on_keys { " on vocabulary keys" } else { "" }
    );
    Ok((n, note))
}

/// Rows of an index block read by one parallel task of the filters on runs.
const PIECE: usize = 8192;

/// All keys of a scan.
fn whole_range(spec: &ScanSpec) -> (Key, Key) {
    (pad(&spec.prefix, 0), pad(&spec.prefix, u64::MAX))
}

/// Key ranges of a scan that hold every row whose first free column can pass a key
/// filter: when the filter fixes the start of the string (`STRSTARTS`, `REGEX("^…")`),
/// the base-vocabulary ids of the keys with that start, and every id outside the base
/// vocabulary (inline values, terms added by updates), which are tested as usual.
/// Otherwise the whole scan.
pub(super) fn key_ranges(
    ctx: &Ctx,
    spec: &ScanSpec,
    kf: Option<&super::keyfilter::KeyFilter>,
) -> Vec<(Key, Key)> {
    let c = spec.prefix.len();
    let Some(prefixes) = kf.and_then(|k| k.key_prefixes()) else {
        return vec![whole_range(spec)];
    };
    if !ctx.opt.filter_key_ranges || spec.cols.first().map(|x| x.0) != Some(c) || c >= 4 {
        return vec![whole_range(spec)];
    }
    let vocab = &ctx.snap.generation.vocab;
    let at = |v: u64, fill: u64| {
        let mut k = pad(&spec.prefix, fill);
        k[c] = v;
        k
    };
    // ids sort by tag first: the base vocabulary is one block of ids
    let mut ranges = vec![(at(0, 0), at(Id::vocab(0).0 - 1, u64::MAX))];
    for p in prefixes {
        let (a, b) = vocab.prefix_range(&p);
        if a < b {
            ranges.push((at(Id::vocab(a).0, 0), at(Id::vocab(b - 1).0, u64::MAX)));
        }
    }
    ranges.push((at(Id::vocab(vocab.len()).0, 0), at(u64::MAX, u64::MAX)));
    ranges
}

/// EXPLAIN text for a read restricted to key ranges.
fn ranges_note(n: usize) -> String {
    if n > 1 {
        format!(" in {n} key ranges")
    } else {
        String::new()
    }
}

/// What the index blocks of a scan contribute to a count on vocabulary keys.
#[derive(Default)]
struct KeyCount {
    /// rows of the vocabulary values that passed
    rows: u64,
    /// distinct vocabulary values tested, and those that passed
    tested: u64,
    passed: u64,
    /// the block's first and last vocabulary value and whether it passed (a run that
    /// continues into the next block is tested in both)
    first: Option<(u64, bool)>,
    last: Option<(u64, bool)>,
    /// the runs of the other values (inline literals, blank nodes), in order
    other: Vec<(Id, u64)>,
    /// key ranges read
    ranges: usize,
    _cursor_charge: Option<super::ctx::RetainedCharge>,
}

/// The rows of a scan whose first free key column passes a key filter, counted per
/// index block in parallel: each block's runs of vocabulary ids are tested on their keys
/// as the front-coded vocabulary blocks are decoded, without collecting the runs of the
/// whole scan. The runs of other ids are returned for the general evaluator. `None`
/// when the blocks cannot be read in parallel (see [`par_key_runs`]).
fn par_count_on_keys(
    ctx: &Ctx,
    spec: &ScanSpec,
    kf: &super::keyfilter::KeyFilter,
    ranges: &[(Key, Key)],
) -> Result<Option<KeyCount>> {
    if spec.dedup {
        return Ok(None);
    }
    let vocab = &ctx.snap.generation.vocab;
    let piece = if ctx.is_cursor() {
        PIECE.min(
            (ctx.memory_remaining() / rayon::current_num_threads().max(1) as u64 / 128).max(1)
                as usize,
        )
    } else {
        PIECE
    };
    let parts = ctx.snap.par_blocks_in_ranges(
        spec.perm,
        ranges,
        run_mask(ctx, spec),
        piece,
        |b, s, e| {
            let _scratch = if ctx.is_cursor() {
                Some(ctx.charge((e - s) as u64 * 48 + 1024)?)
            } else {
                None
            };
            let mut out = KeyCount {
                _cursor_charge: ctx.retained_charge((e - s) as u64 * 32 + 256)?,
                ..Default::default()
            };
            // Reserve required work before optional worker-local regex caches.
            let kf = kf.clone();
            let (mut ids, mut counts): (Vec<u64>, Vec<u64>) = (Vec::new(), Vec::new());
            block_runs(spec, b, s, e, |k, n| {
                if Id(k).tag() == crate::id::Tag::Vocab {
                    ids.push(Id(k).payload());
                    counts.push(n);
                } else {
                    out.other.push((Id(k), n));
                }
            });
            let mut pass = vec![false; ids.len()];
            let mut j = 0;
            let mut test = |p, key: &[u8]| {
                while ids[j] != p {
                    j += 1;
                }
                pass[j] = kf.test(key);
            };
            if ctx.is_cursor() {
                let scratch = ctx.charge(0)?;
                let mut reserved = 0;
                let mut tested = 0usize;
                vocab.get_sorted_checked(
                    &ids,
                    |need| {
                        ctx.check()?;
                        let bytes = (need as u64).saturating_mul(2);
                        scratch.add(bytes.saturating_sub(reserved))?;
                        reserved = bytes;
                        Ok::<(), Error>(())
                    },
                    |p, key| {
                        test(p, key);
                        tested += 1;
                        if tested.is_multiple_of(1024) {
                            ctx.check()?;
                        }
                        Ok::<(), Error>(())
                    },
                )?;
            } else {
                vocab.get_sorted(&ids, test);
            }
            for (h, n) in pass.iter().zip(&counts) {
                if *h {
                    out.rows += n;
                    out.passed += 1;
                }
            }
            out.tested = ids.len() as u64;
            let raw = |i: usize| Id::vocab(ids[i]).0;
            if !ids.is_empty() {
                out.first = Some((raw(0), pass[0]));
                out.last = Some((raw(ids.len() - 1), pass[ids.len() - 1]));
            }
            ctx.check()?;
            if let Some(charge) = &mut out._cursor_charge {
                charge.resize(out.other.capacity() as u64 * 16 + 256)?;
            }
            Ok(out)
        },
    )?;
    let Some(parts) = parts else {
        return Ok(None);
    };
    let mut all = KeyCount {
        ranges: ranges.len(),
        _cursor_charge: ctx.retained_charge(256)?,
        ..Default::default()
    };
    let mut last: Option<(u64, bool)> = None;
    for p in parts {
        if let Some(charge) = &mut all._cursor_charge {
            charge.resize(
                all.other.capacity() as u64 * 16
                    + (all.other.len() + p.other.len()) as u64 * 16
                    + 512,
            )?;
            all.other.reserve_exact(p.other.len());
        }
        all.rows += p.rows;
        all.tested += p.tested;
        all.passed += p.passed;
        // a value whose run continues from the block before was tested twice
        if let (Some((a, passed)), Some((b, _))) = (last, p.first)
            && a == b
        {
            all.tested -= 1;
            all.passed -= passed as u64;
        }
        if p.last.is_some() {
            last = p.last;
        }
        for (id, n) in p.other {
            match all.other.last_mut() {
                Some((x, m)) if *x == id => *m += n,
                _ => all.other.push((id, n)),
            }
        }
        if let Some(charge) = &mut all._cursor_charge {
            charge.resize(all.other.capacity() as u64 * 16 + 256)?;
        }
    }
    ctx.check_output(all.other.len(), 2)?;
    Ok(Some(all))
}

/// The runs of the first free key column in rows `[s, e)` of an index block that pass
/// the scan's graph filter and repeated-variable checks: `f(id, rows)` per run, in
/// order (the first and last may continue in the neighbouring blocks).
fn block_runs(spec: &ScanSpec, b: &Block, s: usize, e: usize, mut f: impl FnMut(u64, u64)) {
    let kc = spec.cols[0].0;
    if block_passes(spec, b, s, e) {
        let col = &b.cols[kc][s..e];
        let mut i = 0;
        while i < col.len() {
            let run = run_len(&col[i..], col[i]);
            f(col[i], run as u64);
            i += run;
        }
        return;
    }
    let mut cur: Option<(u64, u64)> = None;
    for i in s..e {
        let k = b.key(i);
        if !spec.graph.accepts(k[spec.graph_col]) || spec.eqs.iter().any(|&(a, b)| k[a] != k[b]) {
            continue;
        }
        match &mut cur {
            Some((v, n)) if *v == k[kc] => *n += 1,
            _ => {
                if let Some((v, n)) = cur {
                    f(v, n);
                }
                cur = Some((k[kc], 1));
            }
        }
    }
    if let Some((v, n)) = cur {
        f(v, n);
    }
}

/// A scan filtered on its first free key column by [`filter_scan_runs`].
struct FilteredScan {
    /// the rows whose value passed, in scan order
    table: Table,
    /// conjuncts that read other variables, still to be applied
    rest: Vec<Expr>,
    /// rows of the scan, passed or not
    read: u64,
    note: String,
}

/// A FILTER over a scan sorted on a variable it tests: the conjuncts that read only that
/// variable are tested once per run of its values (on vocabulary keys when they can be),
/// and only the rows of the values that pass are copied out of the index blocks, in
/// parallel. `None` when no conjunct reads only the sort variable, when union-graph dedup
/// compares neighbouring rows, or when the delta has keys in the scan's range.
fn filter_scan_runs(
    ctx: &Ctx,
    spec: &ScanSpec,
    scan: &Node,
    exprs: &[Expr],
) -> Result<Option<FilteredScan>> {
    let Some(&(kc, key)) = spec.cols.first() else {
        return Ok(None);
    };
    if spec.dedup || exprs.is_empty() {
        return Ok(None);
    }
    let (on_key, rest): (Vec<Expr>, Vec<Expr>) = exprs
        .iter()
        .cloned()
        .partition(|e| super::exprcache::input(&[e]) == Ok(Some(key)));
    if on_key.is_empty() {
        return Ok(None);
    }
    let kf = super::keyfilter::KeyFilter::new_for(ctx, &on_key, key);
    let ranges = key_ranges(ctx, spec, kf.as_ref());
    let Some((keys, counts)) = par_key_runs(ctx, spec, &ranges)? else {
        return Ok(None);
    };
    let read: u64 = counts.iter().sum();
    let (hit, on_keys) = super::exprcache::filter_values(ctx, &keys, key, &on_key)?;
    drop(counts);
    let passed = hit.iter().filter(|h| **h).count();
    let width = spec.cols.len();
    let parts = ctx.snap.par_blocks_in_ranges(
        spec.perm,
        &ranges,
        scan_mask(ctx, spec),
        PIECE,
        |b, s, e| {
            let mut cols: Vec<Vec<Id>> = vec![Vec::new(); width];
            if s >= e {
                return Ok(cols);
            }
            let col = &b.cols[kc];
            // the block's values among the runs, and whether any of them passed
            let j0 = keys.partition_point(|k| k.0 < col[s]);
            let j1 = keys.partition_point(|k| k.0 <= col[e - 1]);
            if !hit[j0..j1].iter().any(|h| *h) {
                return Ok(cols);
            }
            let mut j = j0;
            if block_passes(spec, b, s, e) {
                let mut i = s;
                while i < e {
                    let run = run_len(&col[i..e], col[i]);
                    while keys[j].0 < col[i] {
                        j += 1;
                    }
                    if hit[j] {
                        for (c, &(kc, _)) in spec.cols.iter().enumerate() {
                            cols[c].extend(b.cols[kc][i..i + run].iter().map(|&x| Id(x)));
                        }
                    }
                    i += run;
                }
            } else {
                for i in s..e {
                    let k = b.key(i);
                    if !spec.graph.accepts(k[spec.graph_col])
                        || spec.eqs.iter().any(|&(a, b)| k[a] != k[b])
                    {
                        continue;
                    }
                    while keys[j].0 < k[kc] {
                        j += 1;
                    }
                    if hit[j] {
                        for (c, &(kc, _)) in spec.cols.iter().enumerate() {
                            cols[c].push(Id(k[kc]));
                        }
                    }
                }
            }
            ctx.check()?;
            Ok(cols)
        },
    )?;
    let Some(parts) = parts else {
        return Ok(None);
    };
    let rows: usize = parts.iter().map(|p| p[0].len()).sum();
    ctx.check_output(rows, width)?;
    let mut table = Table::new(scan.vars.clone());
    for c in &mut table.cols {
        c.reserve_exact(rows);
    }
    for p in parts {
        for (c, part) in p.into_iter().enumerate() {
            table.cols[c].extend(part);
        }
    }
    table.len = rows;
    table.sorted = scan.sorted.clone();
    let note = format!(
        "[runs of ?{}: {} values{} tested{}, {passed} passed]",
        ctx.var_name(key),
        keys.len(),
        ranges_note(ranges.len()),
        if on_keys { " on vocabulary keys" } else { "" }
    );
    Ok(Some(FilteredScan {
        table,
        rest,
        read,
        note,
    }))
}

/// The variable of a scan's first free key column when every FILTER conjunct reads only
/// it, so that [`filter_scan_morsel`] can test each of its values once. `None` when the
/// first free column does not follow the bound prefix, when union-graph dedup compares
/// neighbouring rows or when a conjunct reads another variable.
pub(super) fn run_filter_split(spec: &ScanSpec, exprs: &[Expr]) -> Option<VarId> {
    let &(kc, key) = spec.cols.first()?;
    if spec.dedup || exprs.is_empty() || kc != spec.prefix.len() {
        return None;
    }
    exprs
        .iter()
        .all(|e| super::exprcache::input(&[e]) == Ok(Some(key)))
        .then_some(key)
}

/// The columns a task of [`filter_scan_morsel`] copied out, one per output variable.
pub(super) type MorselPart = Vec<Vec<Id>>;

/// One morsel of a cursor's FILTER over a scan: the rows of the scan in the key `ranges`
/// (sorted, disjoint and within the scan) whose first free key column, bound to `key`,
/// passes the key filter `kf` of the conjuncts `on_key`, whose copies `filters` keeps
/// between tasks and morsels. Each parallel task takes a piece
/// of an index block, finds the runs of the column's values, tests each run's value once
/// on its vocabulary key and copies the rows of the runs that pass, so the morsel is read,
/// tested and copied in one pass. Values outside the base vocabulary are tested as
/// [`super::exprcache::filter_values`] tests them. Output column `i` is read from key
/// column `cols[i]`. Returns the parts in key order and the rows read, or `None` when
/// the delta has keys in a range.
#[allow(clippy::too_many_arguments)]
pub(super) fn filter_scan_morsel(
    ctx: &Ctx,
    spec: &ScanSpec,
    cols: &[usize],
    key: VarId,
    on_key: &[Expr],
    kf: &super::keyfilter::KeyFilter,
    filters: &parking_lot::Mutex<Vec<super::keyfilter::KeyFilter>>,
    ranges: &[(Key, Key)],
) -> Result<Option<(Vec<MorselPart>, u64)>> {
    use crate::id::Tag;
    let kc = spec.cols[0].0;
    let vocab = &ctx.snap.generation.vocab;
    let width = cols.len();
    let parts = ctx.snap.par_blocks_in_ranges(
        spec.perm,
        ranges,
        scan_mask(ctx, spec),
        PIECE,
        |b, s, e| {
            let mut out: MorselPart = vec![Vec::new(); width];
            if s >= e {
                return Ok((out, 0));
            }
            // The rows of the piece that pass the scan's graph filter and repeated
            // variable checks, when not all of them do.
            let sel: Option<Vec<u32>> = (!block_passes(spec, b, s, e)).then(|| {
                (s..e)
                    .filter(|&i| {
                        let k = b.key(i);
                        spec.graph.accepts(k[spec.graph_col])
                            && spec.eqs.iter().all(|&(x, y)| k[x] == k[y])
                    })
                    .map(|i| i as u32)
                    .collect()
            });
            let col = &b.cols[kc];
            let n = sel.as_ref().map_or(e - s, Vec::len);
            let row = |j: usize| sel.as_ref().map_or(s + j, |v| v[j] as usize);
            let _scratch = ctx.charge((n as u64).saturating_mul(24).saturating_add(1024))?;
            // The distinct values of the sorted column, in order. Raw ids sort by tag, so
            // the base-vocabulary values are one stretch of them.
            let mut values: Vec<u64> = Vec::new();
            for j in 0..n {
                let v = col[row(j)];
                if values.last() != Some(&v) {
                    values.push(v);
                }
            }
            let lo = values.partition_point(|&v| Id(v).tag() < Tag::Vocab);
            let hi = lo + values[lo..].partition_point(|&v| Id(v).tag() == Tag::Vocab);
            let payloads: Vec<u64> = values[lo..hi].iter().map(|&v| Id(v).payload()).collect();
            let mut pass = vec![false; values.len()];
            // A copy of the filter per task: tasks sharing a regular expression contend
            // for its match caches. The copies are kept for later tasks, whose regular
            // expressions then start with warm caches.
            let kf = filters.lock().pop().unwrap_or_else(|| kf.clone());
            let scratch = ctx.charge(0)?;
            let mut reserved = 0;
            let mut at = 0;
            vocab.get_sorted_checked(
                &payloads,
                |need| {
                    ctx.check()?;
                    let bytes = (need as u64).saturating_mul(2);
                    scratch.add(bytes.saturating_sub(reserved))?;
                    reserved = bytes;
                    Ok::<(), Error>(())
                },
                |p, k| {
                    while payloads[at] != p {
                        at += 1;
                    }
                    pass[lo + at] = kf.test(k);
                    Ok::<(), Error>(())
                },
            )?;
            drop(payloads);
            filters.lock().push(kf);
            if lo > 0 || hi < values.len() {
                let other: Vec<Id> = values[..lo]
                    .iter()
                    .chain(&values[hi..])
                    .map(|&v| Id(v))
                    .collect();
                let (hit, _) = super::exprcache::filter_values(ctx, &other, key, on_key)?;
                for (i, h) in (0..lo).chain(hi..values.len()).zip(hit) {
                    pass[i] = h;
                }
            }
            // The passing rows, gathered without a branch per row, then each column.
            let mut at: Vec<u32> = vec![0; n];
            let mut w = 0;
            let mut k = 0;
            for j in 0..n {
                let i = row(j);
                if col[i] != values[k] {
                    k += 1;
                }
                at[w] = i as u32;
                w += usize::from(pass[k]);
            }
            at.truncate(w);
            if w > 0 {
                for (to, &from) in out.iter_mut().zip(cols) {
                    let column = &b.cols[from];
                    to.extend(at.iter().map(|&i| Id(column[i as usize])));
                }
            }
            ctx.check()?;
            Ok((out, n as u64))
        },
    )?;
    Ok(parts.map(|parts| {
        let read = parts.iter().map(|p| p.1).sum();
        (
            parts
                .into_iter()
                .filter(|p| p.0.first().is_some_and(|c| !c.is_empty()))
                .map(|p| p.0)
                .collect(),
            read,
        )
    }))
}

/// [`key_runs`] with the index blocks read in parallel, when the snapshot's delta has no
/// key in the scan's range and rows need no comparison with their neighbours (no
/// union-graph dedup); `None` otherwise.
fn par_key_runs(
    ctx: &Ctx,
    spec: &ScanSpec,
    ranges: &[(Key, Key)],
) -> Result<Option<(Vec<Id>, Vec<u64>)>> {
    if spec.dedup {
        return Ok(None);
    }
    let parts = ctx.snap.par_blocks_in_ranges(
        spec.perm,
        ranges,
        run_mask(ctx, spec),
        PIECE,
        |b, s, e| {
            let mut keys: Vec<Id> = Vec::new();
            let mut counts: Vec<u64> = Vec::new();
            block_runs(spec, b, s, e, |k, n| {
                keys.push(Id(k));
                counts.push(n);
            });
            ctx.check()?;
            Ok((keys, counts))
        },
    )?;
    let Some(parts) = parts else {
        return Ok(None);
    };
    let total: usize = parts.iter().map(|p| p.0.len()).sum();
    // a key and a count per run
    ctx.check_output(total, 2)?;
    let (mut keys, mut counts) = (Vec::with_capacity(total), Vec::with_capacity(total));
    for (k, c) in parts {
        // a run that continues from the block before
        let skip = match (keys.last(), k.first()) {
            (Some(a), Some(b)) if a == b => {
                *counts.last_mut().unwrap() += c[0];
                1
            }
            _ => 0,
        };
        keys.extend_from_slice(&k[skip..]);
        counts.extend_from_slice(&c[skip..]);
    }
    Ok(Some((keys, counts)))
}

/// The distinct values of a scan's first free key column with the number of rows of each,
/// in key order.
fn key_runs(ctx: &Ctx, spec: &ScanSpec) -> Result<(Vec<Id>, Vec<u64>)> {
    if let Some(runs) = par_key_runs(ctx, spec, &[whole_range(spec)])? {
        return Ok(runs);
    }
    let kc = spec.cols[0].0;
    let kcs: Vec<usize> = spec.cols.iter().map(|(k, _)| *k).collect();
    let mut keys: Vec<Id> = Vec::new();
    let mut counts: Vec<u64> = Vec::new();
    let runs = std::cell::Cell::new(0usize);
    let mut bump = |k: u64, n: u64| {
        if keys.last() == Some(&Id(k)) {
            *counts.last_mut().unwrap() += n;
        } else {
            keys.push(Id(k));
            counts.push(n);
            runs.set(keys.len());
        }
    };
    let mut last: Option<[u64; 4]> = None;
    let (lo, hi) = (pad(&spec.prefix, 0), pad(&spec.prefix, u64::MAX));
    ctx.snap
        .scan_between_cols(spec.perm, lo, hi, run_mask(ctx, spec), |chunk| {
            let mut row = |k: &[u64; 4]| {
                if !spec.graph.accepts(k[spec.graph_col])
                    || spec.eqs.iter().any(|&(a, b)| k[a] != k[b])
                {
                    return;
                }
                if spec.dedup {
                    let mut proj = [0u64; 4];
                    for (i, &c) in kcs.iter().enumerate() {
                        proj[i] = k[c];
                    }
                    if last == Some(proj) {
                        return;
                    }
                    last = Some(proj);
                }
                bump(k[kc], 1);
            };
            match chunk {
                Chunk::Block(b, s, e) if !spec.dedup && block_passes(spec, b, s, e) => {
                    let col = &b.cols[kc][s..e];
                    let mut i = 0;
                    while i < col.len() {
                        let v = col[i];
                        let run = run_len(&col[i..], v);
                        bump(v, run as u64);
                        i += run;
                    }
                }
                Chunk::Block(b, s, e) => (s..e).for_each(|i| row(&b.key(i))),
                Chunk::Row(k) => row(&k),
            }
            ctx.check()?;
            // a key and a count per run
            ctx.check_output(runs.get(), 2)?;
            Ok(true)
        })?;
    Ok((keys, counts))
}

/// Number of distinct values in the first free key column of a scan sorted on it: the
/// runs of equal ids. Whole blocks are compared column-wise; rows are checked one by one
/// only when a graph filter or repeated variables apply (duplicates across graphs fall
/// into the same run, so union-graph dedup needs nothing extra).
fn count_distinct_scan(ctx: &Ctx, spec: &ScanSpec) -> Result<u64> {
    // new runs in `col` given the last value seen before it
    fn runs(col: &[u64], last: &mut Option<u64>) -> u64 {
        let Some((&first, _)) = col.split_first() else {
            return 0;
        };
        let n =
            (*last != Some(first)) as u64 + col.windows(2).filter(|w| w[0] != w[1]).count() as u64;
        *last = col.last().copied();
        n
    }
    let kc = spec.cols[0].0;
    let mut n = 0u64;
    let mut last: Option<u64> = None;
    let (lo, hi) = (pad(&spec.prefix, 0), pad(&spec.prefix, u64::MAX));
    ctx.snap
        .scan_between_cols(spec.perm, lo, hi, run_mask(ctx, spec), |chunk| {
            let mut row = |k: &[u64; 4]| {
                if spec.graph.accepts(k[spec.graph_col])
                    && spec.eqs.iter().all(|&(a, b)| k[a] == k[b])
                {
                    n += runs(&[k[kc]], &mut last);
                }
            };
            match chunk {
                Chunk::Block(b, s, e) if block_passes(spec, b, s, e) => {
                    n += runs(&b.cols[kc][s..e], &mut last)
                }
                Chunk::Block(b, s, e) => (s..e).for_each(|i| row(&b.key(i))),
                Chunk::Row(k) => row(&k),
            }
            ctx.check()?;
            Ok(true)
        })?;
    Ok(n)
}

fn scan(ctx: &Ctx, spec: &ScanSpec, vars: &[VarId]) -> Result<Table> {
    Ok(scan_limited(ctx, spec, vars, None)?.0)
}

/// Apply a row-preserving / row-reducing unary operator to its input.
pub(super) fn apply_unary(ctx: &Ctx, n: &Node, t: Table, report: &mut ExprReport) -> Result<Table> {
    apply_unary_noted(ctx, n, t, report, &mut None, &mut None)
}

/// [`apply_unary`] with the note and counters an index join reports.
fn apply_unary_noted(
    ctx: &Ctx,
    n: &Node,
    mut t: Table,
    report: &mut ExprReport,
    note: &mut Option<String>,
    counters: &mut Option<Counters>,
) -> Result<Table> {
    Ok(match &n.kind {
        Kind::Filter(exprs) => {
            report.extend(apply_filter(ctx, &mut t, exprs)?);
            t
        }
        Kind::Extend(v, e) => {
            let col = compute_column(ctx, &t, e, report)?;
            t.vars.push(*v);
            t.cols.push(col);
            t
        }
        Kind::Project(vars) => t.project(vars),
        Kind::Unpack { t: tv, parts } => unpack(ctx, t, *tv, parts, &n.vars)?,
        Kind::Distinct => distinct(t),
        Kind::IndexJoin(spec) => {
            let (t, stats) = super::indexjoin::run(ctx, spec, &t, &n.vars)?;
            *note = Some(stats.note());
            *counters = Some(stats.counters());
            t
        }
        _ => unreachable!("not a unary streaming operator"),
    })
}

/// A native blocking operator over cursor-produced input. Its caller owns and
/// charges the collected IDs; ORDER keys retain their own charges until sorted.
pub(super) fn apply_blocking(
    ctx: &Ctx,
    node: &Node,
    mut table: Table,
    report: &mut ExprReport,
) -> Result<Table> {
    match &node.kind {
        Kind::Sort(vars) => {
            ctx.check_output(table.len(), table.width())?;
            table.sort_by_vars(vars);
            ctx.check()?;
            Ok(table)
        }
        Kind::OrderBy { keys, limit } => Ok(order_by(ctx, table, keys, *limit, report)?.0),
        Kind::PatternCount(spec) => {
            let out = super::patternplan::run_count(ctx, spec, Some(&table), &node.vars)?.0;
            // one row per predicate, checked as the eager executor checks an output
            ctx.check_output(out.len(), out.width())?;
            Ok(out)
        }
        _ => unreachable!("not a native blocking cursor operator"),
    }
}

/// Execute `n` so that it produces at least `want` solutions if it has that many; any
/// subset of the solutions is acceptable (LIMIT without ORDER BY, ASK, EXISTS). Scans stop
/// early; operators whose output over a prefix of their input is a subset of their full
/// output re-run with a geometrically growing input budget until they have enough rows.
/// Returns `(table, info, complete)`; `complete` means no further rows exist.
fn execute_limited(ctx: &Ctx, n: &Node, want: usize) -> Result<(Materialized, PlanInfo, bool)> {
    ctx.check()
        .inspect_err(|_| fail(ctx, n, Instant::now(), Vec::new()))?;
    let start = Instant::now();
    let mut done = Vec::new();
    run_limited(ctx, n, want, start, &mut done).inspect_err(|_| {
        match ctx.take_failed(n) {
            // `execute` ran the node and recorded its failure
            Some(own) => ctx.set_failed(n, own),
            None => fail_at(ctx, n, start, done),
        }
    })
}

/// [`execute_limited`] after its first check. Children whose plans are final before a
/// failure go to `done` with their index.
fn run_limited(
    ctx: &Ctx,
    n: &Node,
    want: usize,
    start: Instant,
    done: &mut Vec<(usize, PlanInfo)>,
) -> Result<(Materialized, PlanInfo, bool)> {
    let finish = |t: Materialized, children: Vec<PlanInfo>, complete: bool| {
        ctx.produced(t.table.len())?;
        let mut info = describe(ctx, n);
        info.children = children;
        info.actual_rows = t.table.len() as i64;
        info.time_ms = start.elapsed().as_secs_f64() * 1000.0;
        if !complete {
            info.description = format!("{} [stopped early]", info.description);
            info.fidelity_mut().stopped_early = true;
        }
        Ok((t, info, complete))
    };
    match &n.kind {
        Kind::Scan(spec) => {
            let (t, truncated) = scan_limited(ctx, spec, &n.vars, Some(want))?;
            finish(Materialized::plain(t), Vec::new(), !truncated)
        }
        Kind::PatternScan(spec) => {
            let (t, _, stopped) = super::patternplan::run_scan(ctx, spec, &n.vars, Some(want))?;
            finish(Materialized::plain(t), Vec::new(), !stopped)
        }
        Kind::Filter(_)
        | Kind::Extend(..)
        | Kind::Project(_)
        | Kind::Unpack { .. }
        | Kind::Distinct
        | Kind::IndexJoin(_) => {
            let mut budget = if super::unionpush::limited_input(ctx, n) {
                want
            } else {
                want.max(64)
            };
            // the child's runs so far, added up
            let mut runs: Option<PlanInfo> = None;
            loop {
                let (mut input, cinfo, complete) = execute_limited(ctx, &n.children[0], budget)?;
                let cinfo = match runs.take() {
                    Some(earlier) => earlier.merge_run(cinfo),
                    None => cinfo,
                };
                let mut report = ExprReport::default();
                let mut note = None;
                let mut counters = None;
                let input_held = ctx.charge(0)?;
                let owned = input.lease.is_some();
                if owned {
                    input_held.adopt(&mut input.lease)?;
                }
                let out = match apply_unary_noted(
                    ctx,
                    n,
                    input.table,
                    &mut report,
                    &mut note,
                    &mut counters,
                ) {
                    Ok(out) => out,
                    Err(e) => {
                        done.push((0, cinfo));
                        return Err(e);
                    }
                };
                let lease = if owned {
                    Some(input_held.output_lease(super::algosvc::table_capacity(&out)?)?)
                } else {
                    None
                };
                let out = Materialized { table: out, lease };
                if out.table.len() >= want || complete {
                    let (t, mut info, complete) = finish(out, vec![cinfo], complete)?;
                    if let Kind::Filter(exprs) = &n.kind {
                        super::exists::annotate(ctx, &mut info, exprs);
                    }
                    if let Some(x) = note {
                        info.description = format!("{} {x}", info.description);
                    }
                    if counters.is_some() {
                        info.counters = merge_counters(info.counters.take(), counters);
                    }
                    if let Some(x) = report.note() {
                        info.description = format!("{} {x}", info.description);
                        info.counters = merge_counters(info.counters.take(), report.counters());
                    }
                    return Ok((t, info, complete));
                }
                runs = Some(cinfo);
                budget = budget.saturating_mul(8);
            }
        }
        Kind::Join { .. } | Kind::KeyedJoin { .. } => {
            // limit the (estimated) larger side, compute the other one fully
            let lim = if n.children[0].est >= n.children[1].est {
                0
            } else {
                1
            };
            let (mut other, oinfo) = execute_owned(ctx, &n.children[1 - lim])?;
            let other_owned = other.lease.is_some();
            let other_held = ctx.charge(0)?;
            if other_owned {
                other_held.adopt(&mut other.lease)?;
            } else {
                other_held.add(other.table.mem_bytes())?;
            }
            let other = other.table;
            if other.is_empty() {
                let mut e = Table::empty(n.vars.clone());
                e.sorted.clear();
                let skipped =
                    describe(ctx, &n.children[lim]).skip("the other side produced no rows");
                let infos = if lim == 0 {
                    vec![skipped, oinfo]
                } else {
                    vec![oinfo, skipped]
                };
                return finish(Materialized::plain(e), infos, true);
            }
            let mut budget = if super::unionpush::limited_input(ctx, n) {
                want
            } else {
                want.max(64)
            };
            let mut runs: Option<PlanInfo> = None;
            loop {
                let (mut part, pinfo, complete) =
                    match execute_limited(ctx, &n.children[lim], budget) {
                        Ok(r) => r,
                        Err(e) => {
                            // the other side ran
                            done.push((1 - lim, oinfo));
                            return Err(e);
                        }
                    };
                let pinfo = match runs.take() {
                    Some(earlier) => earlier.merge_run(pinfo),
                    None => pinfo,
                };
                let part_owned = part.lease.is_some();
                let part_held = ctx.charge(0)?;
                if part_owned {
                    part_held.adopt(&mut part.lease)?;
                }
                let part = part.table;
                let (l, r) = if lim == 0 {
                    (&part, &other)
                } else {
                    (&other, &part)
                };
                let out = match &n.kind {
                    Kind::Join { algo, .. } => match algo {
                        JoinAlgo::Cross => cross(ctx, l, r)?,
                        JoinAlgo::Merge => join_tables(ctx, l, r, &[], true)?,
                        JoinAlgo::Hash => join_tables(ctx, l, r, &[], false)?,
                    },
                    Kind::KeyedJoin {
                        expr, left, right, ..
                    } => keyed_join(ctx, l, r, expr, (*left, *right), false, &mut None)?,
                    _ => unreachable!("limited inner join"),
                };
                if out.len() >= want || complete {
                    let infos = if lim == 0 {
                        vec![pinfo, oinfo]
                    } else {
                        vec![oinfo, pinfo]
                    };
                    let lease = if other_owned || part_owned {
                        Some(ctx.output_lease(super::algosvc::table_capacity(&out)?)?)
                    } else {
                        None
                    };
                    return finish(Materialized { table: out, lease }, infos, complete);
                }
                runs = Some(pinfo);
                budget = budget.saturating_mul(8);
            }
        }
        Kind::Union => {
            let held = ctx.charge(0)?;
            let mut owned = false;
            let mut out = Table::new(n.vars.clone());
            let mut complete = true;
            for (i, c) in n.children.iter().enumerate() {
                if out.len() >= want {
                    complete = false;
                    done.extend(n.children[i..].iter().enumerate().map(|(k, c)| {
                        (
                            i + k,
                            describe(ctx, c).skip("earlier branches met the limit"),
                        )
                    }));
                    break;
                }
                let (mut t, info, c_complete) = execute_limited(ctx, c, want - out.len())?;
                complete &= c_complete;
                done.push((i, info));
                if t.lease.is_some() {
                    held.adopt(&mut t.lease)?;
                    owned = true;
                }
                out.append(t.table);
                ctx.check_output(out.len(), out.width())?;
            }
            let infos = std::mem::take(done).into_iter().map(|(_, i)| i).collect();
            let lease = if owned {
                Some(held.output_lease(super::algosvc::table_capacity(&out)?)?)
            } else {
                None
            };
            finish(Materialized { table: out, lease }, infos, complete)
        }
        Kind::Slice {
            offset,
            limit: Some(limit),
        } => {
            let (mut t, info, _) =
                execute_limited(ctx, &n.children[0], offset.saturating_add(*limit))?;
            // Slice consumes the same backing; keep its ownership through the move.
            t.table = t.table.slice(*offset, Some(*limit));
            if let Some(lease) = &mut t.lease {
                lease.resize(ctx.mem_limit, super::algosvc::table_capacity(&t.table)?)?;
            }
            finish(t, vec![info], true)
        }
        _ => {
            let (t, info) = execute_owned(ctx, n)?;
            Ok((t, info, true))
        }
    }
}

/// Scan with an optional row limit; returns `(table, truncated)`.
pub(super) fn scan_limited(
    ctx: &Ctx,
    spec: &ScanSpec,
    vars: &[VarId],
    limit: Option<usize>,
) -> Result<(Table, bool)> {
    let mut t = Table::new(vars.to_vec());
    // the exact number of rows under the prefix (at most two block decodes, and those
    // blocks are read by the scan anyway) bounds the output: reserve it once instead of
    // growing every column by repeated doubling
    let bound = ctx.snap.count(spec.perm, &spec.prefix)?;
    let cap = usize::try_from(bound)
        .unwrap_or(usize::MAX)
        .min(limit.unwrap_or(usize::MAX))
        .min(ctx.max_rows.saturating_add(1))
        .min(ctx.rows_within_budget(vars.len()).saturating_add(1));
    for c in &mut t.cols {
        c.reserve_exact(cap);
    }
    let truncated = scan_into(
        ctx,
        spec,
        pad(&spec.prefix, 0),
        pad(&spec.prefix, u64::MAX),
        limit,
        &mut t,
    )?;
    Ok((t, truncated))
}

/// A scan restricted to id ranges of its first free column: exact ranges are copied,
/// the others filtered. The output keeps the scan's order.
fn range_scan(ctx: &Ctx, spec: &ScanSpec, range: &RangeSpec, vars: &[VarId]) -> Result<Table> {
    let mut t = Table::new(vars.to_vec());
    let bound = |v: u64, fill: u64| {
        let mut k = pad(&spec.prefix, fill);
        k[spec.prefix.len()] = v;
        k
    };
    for r in &range.ranges {
        let (lo, hi) = (bound(r.lo, 0), bound(r.hi, u64::MAX));
        if r.exact {
            scan_into(ctx, spec, lo, hi, None, &mut t)?;
        } else {
            let mut part = Table::new(vars.to_vec());
            scan_into(ctx, spec, lo, hi, None, &mut part)?;
            if !part.is_empty() {
                apply_range_filter(ctx, &mut part, range.var, &range.filter)?;
                t.append(part);
            }
        }
        ctx.check_output(t.len(), t.width())?;
    }
    Ok(t)
}

/// A pushed range conjunct `?v op c` (see [`super::plan::RangeSpec`]) in the form the
/// numeric test reads: the constant prepared for comparisons, and its term id when the
/// expression evaluates to that term (then `=` holds on the identical term, as in
/// `val_eq`).
struct NumAtom {
    op: super::plan::RangeOp,
    c: super::value::NumConst,
    id: Option<Id>,
}

impl NumAtom {
    /// Whether the conjunct holds for the term `x` whose value is the number `v`, as the
    /// generic evaluator answers it. For two numbers, `compare` and `equals` are
    /// `num_cmp`, and an incomparable pair (NaN) is false.
    #[inline]
    fn holds(&self, x: Id, v: super::value::Num) -> bool {
        use super::plan::RangeOp;
        use Ordering::*;
        if self.op == RangeOp::Eq && self.id == Some(x) {
            return true;
        }
        let o = self.c.order(v);
        match self.op {
            RangeOp::Lt => o == Some(Less),
            RangeOp::Le => matches!(o, Some(Less | Equal)),
            RangeOp::Gt => o == Some(Greater),
            RangeOp::Ge => matches!(o, Some(Greater | Equal)),
            RangeOp::Eq => o == Some(Equal),
        }
    }
}

/// The conjuncts of a pushed range filter on `var` as [`NumAtom`]s, `None` when one is
/// not a range conjunct.
fn num_atoms(ctx: &Ctx, var: VarId, exprs: &[Expr]) -> Option<Vec<NumAtom>> {
    exprs
        .iter()
        .map(|e| {
            let (op, c) = super::plan::range_atom(e, var, ctx)?;
            let c = super::value::NumConst::new(super::value::Num::of(&c).ok()?);
            let id = match e {
                Expr::Cmp(a, b, _) | Expr::Eq(a, b) => match (&**a, &**b) {
                    (Expr::Var(_), k) | (k, Expr::Var(_)) => k.const_id(),
                    _ => None,
                },
                _ => None,
            };
            Some(NumAtom { op, c, id })
        })
        .collect()
}

/// Apply the pushed range filter `exprs` on `var` (see [`super::plan::RangeSpec`]) to
/// the rows of an inexact id range. Numbers of the base vocabulary are tested on their
/// value from the numeric column, inline numbers on their id, and a term the column
/// knows is no number fails every conjunct. That is the generic filter's answer for each
/// of them without decoding a key. The other rows (the column's unknown numbers, delta
/// terms) go through the generic filter.
pub(super) fn apply_range_filter(
    ctx: &Ctx,
    t: &mut Table,
    var: VarId,
    exprs: &[Expr],
) -> Result<()> {
    use crate::id::Tag;
    use crate::vocab::numeric::Numeric;
    let (Some(num), Some(c), Some(atoms)) = (
        ctx.snap.generation.vocab.numeric(),
        t.col_of(var),
        num_atoms(ctx, var, exprs),
    ) else {
        apply_filter(ctx, t, exprs)?;
        return Ok(());
    };
    let col = &t.cols[c];
    use super::value::Num;
    let test = |x: Id, v: Num| atoms.iter().all(|a| a.holds(x, v));
    // `Some(passes)`, or `None` for the generic filter
    let decide = |x: Id, n: Option<Numeric>| -> Option<bool> {
        match (x.tag(), n) {
            (Tag::Vocab, Some(n)) => match n {
                Numeric::NotNumeric => Some(false),
                Numeric::Unknown => None,
                Numeric::Integer(i) => Some(test(x, Num::Integer(i.into()))),
                Numeric::Decimal(d) => Some(test(x, Num::Decimal(d))),
                Numeric::Float(f) => Some(test(x, Num::Float(f.into()))),
                Numeric::Double(d) => Some(test(x, Num::Double(d.into()))),
            },
            (Tag::Int | Tag::Double | Tag::Decimal, _) => ctx
                .value(x)
                .and_then(|v| Num::of(&v).ok())
                .map(|v| test(x, v)),
            _ => None,
        }
    };
    // the distinct vocabulary ids of a run of rows read from the column at once, and
    // runs of one id (the scan's sorted column) decided once
    let chunk = |ids: &[Id]| -> Vec<Option<bool>> {
        let mut vocab: Vec<u64> = ids
            .iter()
            .filter(|x| x.tag() == Tag::Vocab)
            .map(|x| x.payload())
            .collect();
        // already sorted when the column is the scan's sorted column
        if !vocab.is_sorted() {
            vocab.sort_unstable();
        }
        vocab.dedup();
        let found = num.get_many(&vocab);
        let mut at = 0;
        let mut last: Option<(Id, Option<bool>)> = None;
        ids.iter()
            .map(|&x| match last {
                Some((l, d)) if l == x => d,
                _ => {
                    let n = if x.tag() == Tag::Vocab {
                        let p = x.payload();
                        if vocab.get(at) != Some(&p) {
                            at = if vocab.get(at + 1) == Some(&p) {
                                at + 1
                            } else {
                                vocab.binary_search(&p).unwrap_or(at)
                            };
                        }
                        found.get(at).copied()
                    } else {
                        None
                    };
                    let d = decide(x, n);
                    last = Some((x, d));
                    d
                }
            })
            .collect()
    };
    let decided: Vec<Option<bool>> = if col.len() > PAR_THRESHOLD {
        col.par_chunks(PAR_MIN_LEN.max(4096))
            .flat_map_iter(chunk)
            .collect()
    } else {
        chunk(col)
    };
    let rest: Vec<usize> = (0..decided.len())
        .filter(|&i| decided[i].is_none())
        .collect();
    let mut keep: Vec<bool> = decided.iter().map(|d| d.unwrap_or(false)).collect();
    if !rest.is_empty() {
        let sub = t.take_rows(&rest);
        let mask = filter_mask(ctx, &sub, exprs)?;
        for (&i, k) in rest.iter().zip(mask) {
            keep[i] = k;
        }
    }
    let sorted = t.sorted.clone();
    t.filter_rows(&keep);
    t.sorted = sorted;
    Ok(())
}

// ---------------------------------------------------------- ordered top-k ------

/// The unread part `[lo, hi]` of a monotone piece of an [`OrderedTopK`] scan.
struct TopKRun {
    lo: u64,
    hi: u64,
    exact: bool,
    /// the best values are at the high end of the id range
    best_high: bool,
    /// the rows read so far that can still be among the first k
    kept: Table,
    /// base rows to read in the next step (grows geometrically)
    want: usize,
    /// no unread row of the piece can be among the first k
    done: bool,
}

/// Rows of an [`OrderedTopK`] scan whose order-column ids lie in `[lo, hi]`, in key order,
/// with the filters applied; also returns how many rows were scanned.
fn topk_read(
    ctx: &Ctx,
    spec: &OrderedTopK,
    lo: u64,
    hi: u64,
    exact: bool,
    vars: &[VarId],
) -> Result<(Table, usize)> {
    let scan = &spec.scan;
    let bound = |v: u64, fill: u64| {
        let mut k = pad(&scan.prefix, fill);
        k[scan.prefix.len()] = v;
        k
    };
    topk_read_keys(ctx, spec, bound(lo, 0), bound(hi, u64::MAX), exact, vars)
}

/// [`topk_read`] of the keys `[lo, hi]` of the scan.
fn topk_read_keys(
    ctx: &Ctx,
    spec: &OrderedTopK,
    lo: crate::index::Key,
    hi: crate::index::Key,
    exact: bool,
    vars: &[VarId],
) -> Result<(Table, usize)> {
    topk_read_keys_last(ctx, spec, lo, hi, exact, vars).map(|(t, n, _)| (t, n))
}

/// [`topk_read_keys`], with the last row the scan produced before any filter when the
/// scan drops duplicate rows of a union default graph: the next read of the same piece
/// must drop that row again if it starts with it.
fn topk_read_keys_last(
    ctx: &Ctx,
    spec: &OrderedTopK,
    lo: crate::index::Key,
    hi: crate::index::Key,
    exact: bool,
    vars: &[VarId],
) -> Result<(Table, usize, Option<Vec<Id>>)> {
    let mut t = Table::new(vars.to_vec());
    scan_into(ctx, &spec.scan, lo, hi, None, &mut t)?;
    let scanned = t.len();
    let last =
        (spec.scan.dedup && scanned > 0).then(|| t.cols.iter().map(|c| c[scanned - 1]).collect());
    if !exact && !spec.range_filter.is_empty() && !t.is_empty() {
        apply_range_filter(ctx, &mut t, spec.var, &spec.range_filter)?;
    }
    if !spec.filter.is_empty() && !t.is_empty() {
        apply_filter(ctx, &mut t, &spec.filter)?;
    }
    Ok((t, scanned, last))
}

/// The order-column id of the base row `want` rows from the best end of the ids
/// `[lo, hi]` of an [`OrderedTopK`] scan (`lo` or `hi` when there are fewer). Only the
/// leading key columns of the blocks at that end are decoded.
fn topk_boundary(
    ctx: &Ctx,
    spec: &OrderedTopK,
    lo: u64,
    hi: u64,
    best_high: bool,
    mut want: usize,
) -> Result<u64> {
    let scan = &spec.scan;
    let c = scan.prefix.len();
    let bound = |v: u64, fill: u64| {
        let mut k = pad(&scan.prefix, fill);
        k[c] = v;
        k
    };
    let (klo, khi) = (bound(lo, 0), bound(hi, u64::MAX));
    let perm = ctx.snap.perm(scan.perm);
    let (b0, b1) = perm.key_block_range(&klo, &khi);
    let mask = crate::index::bound_cols(&klo, &khi);
    let mut visit = |b: usize| -> Result<Option<u64>> {
        let blk = ctx.snap.cache.get_cols(perm, b, mask)?;
        let (s, e) = blk.key_range(&klo, &khi);
        if e - s >= want {
            let col = &blk.cols[c];
            return Ok(Some(if best_high {
                col[e - want]
            } else {
                col[s + want - 1]
            }));
        }
        want -= e - s;
        Ok(None)
    };
    if best_high {
        for b in (b0..b1).rev() {
            if let Some(id) = visit(b)? {
                return Ok(id);
            }
        }
        Ok(lo)
    } else {
        for b in b0..b1 {
            if let Some(id) = visit(b)? {
                return Ok(id);
            }
        }
        Ok(hi)
    }
}

/// Read the next rows of a monotone piece from its best end, keeping those that can
/// still be among the first k; returns the rows scanned.
fn topk_step(ctx: &Ctx, spec: &OrderedTopK, run: &mut TopKRun, vars: &[VarId]) -> Result<usize> {
    // base rows only: delta rows in between are merged by the scan
    let edge = topk_boundary(ctx, spec, run.lo, run.hi, run.best_high, run.want)?;
    let (a, b) = if run.best_high {
        (edge, run.hi)
    } else {
        (run.lo, edge)
    };
    let (mut part, scanned) = topk_read(ctx, spec, a, b, run.exact, vars)?;
    if run.best_high {
        run.done |= a == run.lo;
        run.hi = a.saturating_sub(1);
    } else {
        run.done |= b == run.hi;
        run.lo = b.saturating_add(1);
    }
    run.want = run.want.saturating_mul(4);
    // the rows come in id order, and earlier steps read only better ids: the `need`-th
    // best row of this step and its ties complete the piece's best k, and every other
    // row of the piece has k rows with strictly better values ahead of it (distinct ids
    // of a monotone piece are distinct numbers)
    let need = spec.k - run.kept.len();
    if part.len() >= need {
        let col = &part.cols[0];
        let keep: Vec<bool> = if run.best_high {
            let cut = col[part.len() - need];
            col.iter().map(|&id| id >= cut).collect()
        } else {
            let cut = col[need - 1];
            col.iter().map(|&id| id <= cut).collect()
        };
        part.filter_rows(&keep);
        run.done = true;
    }
    run.kept.append(part);
    Ok(scanned)
}

/// Whether ORDER BY's comparison of `id` with other values is a total order (it is not
/// for NaN, which compares with numbers by its lexical form, nor for partially ordered
/// dates, times and durations).
fn totally_ordered(ctx: &Ctx, id: Id) -> bool {
    use crate::id::Tag;
    match id.tag() {
        Tag::Double => !id.as_f64().is_nan(),
        Tag::DateTime | Tag::Date => false,
        Tag::Vocab | Tag::Delta => match ctx.value(id) {
            Some(Value::Double(d)) => !d.is_nan(),
            Some(Value::Float(f)) => !f.is_nan(),
            Some(
                Value::DateTime(_)
                | Value::Date(_)
                | Value::Time(_)
                | Value::Duration(_)
                | Value::YearMonth(_)
                | Value::DayTime(_)
                | Value::Triple(_),
            ) => false,
            _ => true,
        },
        _ => true,
    }
}

fn cursor_totally_ordered(value: Option<&Value>) -> bool {
    match value {
        Some(Value::Double(d)) => !d.is_nan(),
        Some(Value::Float(f)) => !f.is_nan(),
        Some(
            Value::DateTime(_)
            | Value::Date(_)
            | Value::Time(_)
            | Value::Duration(_)
            | Value::YearMonth(_)
            | Value::DayTime(_)
            | Value::Triple(_),
        ) => false,
        _ => true,
    }
}

/// The best k rows of the pieces of an [`OrderedTopK`] scan without value order, which
/// are read whole, in no particular order; `None` when one of their values is not
/// totally ordered. Adds the rows read to `scanned`.
fn topk_rest(
    ctx: &Ctx,
    spec: &OrderedTopK,
    vars: &[VarId],
    pieces: &[&super::plan::TopKPiece],
    scanned: &mut usize,
) -> Result<Option<Table>> {
    let mut rest = Table::new(vars.to_vec());
    for p in pieces {
        let (t, n) = topk_read(ctx, spec, p.lo, p.hi, p.exact, vars)?;
        *scanned += n;
        rest.append(t);
        ctx.check_output(rest.len(), rest.width())?;
    }
    // Cursor contexts deliberately have no eager decoded-value cache. Decode
    // dictionary ORDER values in one charged, operation-local pass instead of
    // reconstructing a front-coded key again for every row of every check.
    let decoded = if ctx.is_cursor() {
        super::exprcache::cursor_dictionary_values(ctx, &rest, &Expr::Var(spec.var))?
    } else {
        None
    };
    let total = match &decoded {
        Some(values) => cursor_rest_total(ctx, &rest, values)?,
        None => {
            let total = |id: &Id| totally_ordered(ctx, *id);
            if rest.len() > PAR_THRESHOLD / 4 {
                rest.cols[0].par_iter().all(total)
            } else {
                rest.cols[0].iter().all(total)
            }
        }
    };
    if !total {
        return Ok(None);
    }
    let keys = [(Expr::Var(spec.var), spec.asc)];
    // A cursor ranks the rounded values it decoded above, rather than decoding the same
    // dictionary values again in the prefilter of `order_by`.
    if let Some(values) = &decoded
        && ctx.opt.topk_prefilter
        && rest.len() > spec.k
        && let Some((rows, charge)) =
            cursor_topk_candidates(ctx, &rest, &keys, spec.k, Some(values))?
        && rows.len() < rest.len()
    {
        ctx.check_output(rows.len(), rest.width())?;
        rest = rest.take_rows(&rows);
        drop(charge);
    }
    drop(decoded);
    // only their first k can be among the first k of all rows (the order of two rows
    // does not depend on the others)
    if rest.len() > spec.k {
        rest = order_by(ctx, rest, &keys, Some(spec.k), &mut ExprReport::default())?.0;
    }
    Ok(Some(rest))
}

/// Whether every value of the first column of `rest` is totally ordered, with its
/// dictionary values decoded in `values`.
fn cursor_rest_total(
    ctx: &Ctx,
    rest: &Table,
    values: &super::exprcache::PerValue<Option<Value>>,
) -> Result<bool> {
    for (i, value) in values.vals.iter().enumerate() {
        if i.is_multiple_of(1024) {
            ctx.check()?;
        }
        if !cursor_totally_ordered(value.as_ref()) {
            return Ok(false);
        }
    }
    for (i, &id) in rest.cols[0].iter().enumerate() {
        if i.is_multiple_of(1024) {
            ctx.check()?;
        }
        if !matches!(
            id.tag(),
            crate::id::Tag::Vocab | crate::id::Tag::Delta | crate::id::Tag::Local
        ) && !totally_ordered(ctx, id)
        {
            return Ok(false);
        }
    }
    Ok(true)
}

/// The rows that the heap of [`cursor_topk_rest`] holds, a slot of the scan's ids for
/// each row.
struct RestSlots {
    width: usize,
    ids: Vec<Id>,
    free: Vec<u32>,
    key_bytes: u64,
}

/// The rows of one morsel offered to the heap of [`cursor_topk_rest`].
struct RestOffer<'a> {
    slots: &'a mut RestSlots,
    table: &'a Table,
}

impl HeapRows<u32> for RestOffer<'_> {
    fn keep(&mut self, i: usize, keys: &[SortKey]) -> Result<u32> {
        let s = &mut *self.slots;
        let slot = match s.free.pop() {
            Some(slot) => slot,
            None => {
                let slot = (s.ids.len() / s.width.max(1)) as u32;
                s.ids.resize(s.ids.len() + s.width, Id::UNDEF);
                slot
            }
        };
        let at = slot as usize * s.width;
        for (j, col) in self.table.cols.iter().enumerate() {
            s.ids[at + j] = col[i];
        }
        s.key_bytes = s
            .key_bytes
            .saturating_add(keys.iter().map(SortKey::payload_bytes).sum::<u64>());
        Ok(slot)
    }

    fn release(&mut self, entry: Entry<u32>) -> Result<()> {
        let s = &mut *self.slots;
        s.key_bytes = s
            .key_bytes
            .saturating_sub(entry.keys.iter().map(SortKey::payload_bytes).sum::<u64>());
        s.free.push(entry.payload);
        Ok(())
    }
}

/// Rows of the index blocks of one morsel that a cursor's [`OrderedTopK`] reads from a
/// piece without value order: a single block.
const TOPK_MORSEL_ROWS: usize = crate::index::BLOCK_ROWS;

/// The rows a morsel of [`cursor_topk_rest`] keeps, the rows its scan produced and, when
/// the scan drops union-graph duplicates, the last of those rows.
type MorselBest = (Table, usize, Option<Vec<Id>>);

/// The keys `[first, last]` of one morsel of [`cursor_topk_rest`], the piece it belongs
/// to and the rows of its blocks.
struct Morsel {
    first: Key,
    last: Key,
    exact: bool,
    piece: usize,
    rows: usize,
}

/// [`topk_rest`] for a cursor. The pieces are split into morsels of whole index blocks,
/// which are read in parallel. Each morsel keeps its rows that have fewer than k rows of
/// the morsel ahead of them, ties broken by arrival, in arrival order, and those rows are
/// then offered in order to one heap of the best k. A row that a morsel drops has k rows
/// of the same morsel at least as good that arrived before it, so the heap keeps the
/// rows that [`order_by`] keeps over the concatenated pieces. The cursor holds a morsel
/// per worker thread and the rows they keep, instead of every row of the pieces. A morsel
/// is checked for values that are not totally ordered before it ranks its rows, and one
/// such value sends the whole query to the generic plan, as it does eagerly.
fn cursor_topk_rest(
    ctx: &Ctx,
    spec: &OrderedTopK,
    vars: &[VarId],
    pieces: &[&super::plan::TopKPiece],
    scanned: &mut usize,
) -> Result<Option<Table>> {
    let keys = [(Expr::Var(spec.var), spec.asc)];
    let scan = &spec.scan;
    let perm = ctx.snap.perm(scan.perm);
    let bound = |v: u64, fill: u64| {
        let mut k = pad(&scan.prefix, fill);
        k[scan.prefix.len()] = v;
        k
    };
    // The morsels of every piece, in key order. Delta keys between two morsels belong to
    // the one before the gap, so each morsel but a piece's last ends just before the next
    // one's first block, and a piece with no base block is one morsel.
    let mut morsels: Vec<Morsel> = Vec::new();
    for (piece, p) in pieces.iter().enumerate() {
        let (lo, hi) = (bound(p.lo, 0), bound(p.hi, u64::MAX));
        let (b0, b1) = perm.key_block_range(&lo, &hi);
        let mut b = b0;
        loop {
            let mut e = b;
            let mut rows = 0;
            while e < b1 && (e == b || rows < TOPK_MORSEL_ROWS) {
                rows += perm.blocks[e].rows as usize;
                e += 1;
            }
            let first = if b == b0 {
                lo
            } else {
                perm.blocks[b].first.max(lo)
            };
            let last = if e >= b1 {
                hi
            } else {
                let mut next = perm.blocks[e].first;
                // the key just before the next morsel's first key
                for c in (0..4).rev() {
                    if next[c] > 0 {
                        next[c] -= 1;
                        next[c + 1..].fill(u64::MAX);
                        break;
                    }
                }
                next.min(hi)
            };
            morsels.push(Morsel {
                first,
                last,
                exact: p.exact,
                piece,
                rows,
            });
            if e >= b1 {
                break;
            }
            b = e;
        }
    }
    ctx.check()?;
    let _list = ctx.charge((morsels.capacity() * std::mem::size_of::<Morsel>()) as u64)?;
    let mut heap: TopK<u32> = TopK::new(spec.k, vec![spec.asc]);
    let mut slots = RestSlots {
        width: vars.len(),
        ids: Vec::new(),
        free: Vec::new(),
        key_bytes: 0,
    };
    let entry_bytes = std::mem::size_of::<Entry<u32>>() as u64
        + std::mem::size_of::<SortKey>() as u64
        + vars.len() as u64 * 8;
    let mut held = ctx.retained_charge(1024)?;
    // The last row the scan of the previous morsel of the same piece produced, when the
    // scan drops duplicate rows of a union default graph. The copies of a triple are
    // adjacent in key order, and the scan of one morsel drops them within the morsel,
    // so a morsel can only repeat the previous one's last row, as its first row.
    let mut carried: Option<(usize, Vec<Id>)> = None;
    // A wave of one morsel per worker at a time: a worker that waits inside a morsel's
    // own parallel work could otherwise steal and start every other morsel.
    for wave in morsels.chunks(rayon::current_num_threads().max(1)) {
        let parts = wave
            .par_iter()
            .map(|m| morsel_best(ctx, spec, vars, &keys, m))
            .collect::<Result<Vec<_>>>()?;
        let Some(parts) = parts.into_iter().collect::<Option<Vec<_>>>() else {
            return Ok(None);
        };
        for (m, (mut t, mut n, last)) in wave.iter().zip(parts) {
            if let Some((piece, row)) = &carried
                && *piece == m.piece
                && !t.is_empty()
                && t.cols.iter().zip(row).all(|(c, &id)| c[0] == id)
            {
                // a read of the whole piece drops this copy, as eager execution does
                let rows: Vec<usize> = (1..t.len()).collect();
                t = t.take_rows(&rows);
                n -= 1;
            }
            if let Some(last) = last {
                carried = Some((m.piece, last));
            }
            *scanned += n;
            if t.is_empty() {
                continue;
            }
            let room = heap.k().saturating_add(1).min(t.len()) as u64;
            if let Some(held) = &mut held {
                held.resize(
                    1024 + (heap.kept() as u64 + room) * entry_bytes
                        + slots.key_bytes
                        + slots.ids.capacity() as u64 * 8,
                )?;
            }
            let mut offer = RestOffer {
                slots: &mut slots,
                table: &t,
            };
            offer_rows(
                ctx,
                &mut heap,
                &t,
                &keys,
                &mut ExprReport::default(),
                &mut offer,
            )?;
        }
    }
    ctx.check()?;
    let width = slots.width;
    let entries = heap.into_sorted();
    ctx.check_output(entries.len(), width)?;
    let mut rest = Table::new(vars.to_vec());
    for (j, col) in rest.cols.iter_mut().enumerate() {
        col.extend(
            entries
                .iter()
                .map(|e| slots.ids[e.payload as usize * width + j]),
        );
    }
    rest.len = entries.len();
    Ok(Some(rest))
}

/// The rows of one morsel of [`cursor_topk_rest`] that have fewer than k rows of the
/// morsel ahead of them, in arrival order, and the rows the morsel read; `None` when one
/// of its values is not totally ordered.
fn morsel_best(
    ctx: &Ctx,
    spec: &OrderedTopK,
    vars: &[VarId],
    keys: &[(Expr, bool)],
    m: &Morsel,
) -> Result<Option<MorselBest>> {
    // the morsel's rows, charged from its blocks before they are read and then at their
    // real size
    let estimate = (m.rows as u64).saturating_mul(vars.len() as u64 * 8);
    let rows = ctx.charge(estimate)?;
    let (mut t, n, last) = topk_read_keys_last(ctx, spec, m.first, m.last, m.exact, vars)?;
    if t.is_empty() {
        return Ok(Some((t, n, last)));
    }
    rows.add(t.mem_bytes().saturating_sub(estimate))?;
    let decoded = super::exprcache::cursor_dictionary_values(ctx, &t, &Expr::Var(spec.var))?;
    let total = match &decoded {
        Some(values) => cursor_rest_total(ctx, &t, values)?,
        // without a decoded column, values are read one at a time, as eager execution
        // reads them
        None => t.cols[0].iter().all(|&id| totally_ordered(ctx, id)),
    };
    if !total {
        return Ok(None);
    }
    if ctx.opt.topk_prefilter
        && t.len() > spec.k
        && let Some((rows, charge)) =
            cursor_topk_candidates(ctx, &t, keys, spec.k, decoded.as_ref())?
        && rows.len() < t.len()
    {
        t = t.take_rows(&rows);
        drop(charge);
    }
    drop(decoded);
    if t.len() > spec.k {
        let mut heap: TopK<usize> = TopK::new(spec.k, vec![spec.asc]);
        offer_rows(
            ctx,
            &mut heap,
            &t,
            keys,
            &mut ExprReport::default(),
            &mut RowIndex,
        )?;
        let mut rows: Vec<usize> = heap.into_sorted().into_iter().map(|e| e.payload).collect();
        rows.sort_unstable();
        t = t.take_rows(&rows);
    }
    ctx.check()?;
    Ok(Some((t, n, last)))
}

/// [`OrderedTopK`]: the first k rows of the ORDER BY, in its order; `None` when a value
/// outside the monotone pieces is not totally ordered (the generic plan must decide).
/// Also returns how many rows were scanned.
pub(super) fn ordered_topk(
    ctx: &Ctx,
    spec: &OrderedTopK,
    out: &[VarId],
) -> Result<Option<(Table, usize)>> {
    let vars: Vec<VarId> = spec.scan.cols.iter().map(|c| c.1).collect();
    debug_assert_eq!(vars.first(), Some(&spec.var));
    let mut scanned = 0;
    let mut whole = Vec::new();
    let mut runs = Vec::new();
    for p in &spec.pieces {
        match p.mono {
            // the pieces without value order are read whole
            None => whole.push(p),
            Some(up) => runs.push(TopKRun {
                lo: p.lo,
                hi: p.hi,
                exact: p.exact,
                best_high: up != spec.asc,
                kept: Table::new(vars.clone()),
                want: spec.k.max(16).saturating_mul(2),
                done: false,
            }),
        }
    }
    let rest = if ctx.is_cursor() && heap_eligible(ctx) {
        cursor_topk_rest(ctx, spec, &vars, &whole, &mut scanned)?
    } else {
        topk_rest(ctx, spec, &vars, &whole, &mut scanned)?
    };
    let Some(rest) = rest else {
        return Ok(None);
    };
    let keys = [(Expr::Var(spec.var), spec.asc)];
    let held = ctx.charge(rest.mem_bytes())?;
    let better = if spec.asc {
        Ordering::Less
    } else {
        Ordering::Greater
    };
    loop {
        ctx.check()?;
        for run in runs.iter_mut().filter(|r| !r.done) {
            scanned += topk_step(ctx, spec, run, &vars)?;
        }
        // the candidates in the generic plan's row order, ranked by the generic ORDER BY
        let n = rest.len() + runs.iter().map(|r| r.kept.len()).sum::<usize>();
        ctx.check_output(n, vars.len())?;
        let mut cand = rest.clone();
        for run in &runs {
            cand.append(run.kept.clone());
        }
        cand.sorted.clear();
        cand.sort_by_vars(&spec.tie_order);
        let (top, _) = order_by(ctx, cand, &keys, Some(spec.k), &mut ExprReport::default())?;
        if top.len() == spec.k {
            // a piece is finished once the k-th row is strictly better than its best
            // unread value, and so than every unread value
            let kth = ctx.value(top.cols[0][spec.k - 1]);
            for run in runs.iter_mut().filter(|r| !r.done) {
                let best = Id(if run.best_high { run.hi } else { run.lo });
                if order_cmp(kth.as_ref(), ctx.value(best).as_ref()) == better {
                    run.done = true;
                }
            }
        }
        if runs.iter().all(|r| r.done) {
            drop(held);
            return Ok(Some((top.project(out), scanned)));
        }
    }
}

/// Append the scan's rows with keys in `[lo, hi]` to `t`, stopping after `limit` rows in
/// total; returns whether rows may remain.
fn scan_into(
    ctx: &Ctx,
    spec: &ScanSpec,
    lo: crate::index::Key,
    hi: crate::index::Key,
    limit: Option<usize>,
    t: &mut Table,
) -> Result<bool> {
    let limit = limit.unwrap_or(usize::MAX);
    let mut truncated = false;
    let kcs: Vec<usize> = spec.cols.iter().map(|(k, _)| *k).collect();
    let mut last: Option<[u64; 4]> = None;
    let mut n = 0usize;
    let mut row = |k: &[u64; 4], t: &mut Table| {
        if !spec.graph.accepts(k[spec.graph_col]) {
            return;
        }
        if spec.eqs.iter().any(|&(a, b)| k[a] != k[b]) {
            return;
        }
        if spec.dedup {
            let mut proj = [0u64; 4];
            for (i, &kc) in kcs.iter().enumerate() {
                proj[i] = k[kc];
            }
            if last == Some(proj) {
                return;
            }
            last = Some(proj);
        }
        for (c, &kc) in kcs.iter().enumerate() {
            t.cols[c].push(Id(k[kc]));
        }
        t.len += 1;
    };
    ctx.snap
        .scan_between_cols(spec.perm, lo, hi, scan_mask(ctx, spec), |chunk| {
            if t.len >= limit {
                truncated = true;
                return Ok(false);
            }
            match chunk {
                Chunk::Block(b, s, e) if !spec.dedup && block_passes(spec, b, s, e) => {
                    let e = e.min(s.saturating_add(limit - t.len));
                    for (c, &kc) in kcs.iter().enumerate() {
                        t.cols[c].extend(b.cols[kc][s..e].iter().map(|&x| Id(x)));
                    }
                    t.len += e - s;
                    n += e - s;
                }
                Chunk::Block(b, s, e) => {
                    for i in s..e {
                        row(&b.key(i), t);
                    }
                    n += e - s;
                }
                Chunk::Row(k) => {
                    row(&k, t);
                    n += 1;
                }
            }
            if n > 1 << 16 {
                n = 0;
                ctx.check()?;
                ctx.check_output(t.len(), t.width())?;
            }
            if t.len >= limit {
                // there may be more matching rows after this point
                truncated = true;
                return Ok(false);
            }
            Ok(true)
        })?;
    Ok(truncated)
}

// ------------------------------------------------------------------ joins ------

/// Output layout of a join: left vars, then right-only vars.
struct JoinLayout {
    vars: Vec<VarId>,
    /// for each shared var: (left col, right col)
    shared: Vec<(usize, usize)>,
    /// right-only columns
    right_only: Vec<usize>,
}

fn layout(l: &Table, r: &Table) -> JoinLayout {
    let mut vars = l.vars.clone();
    let mut shared = Vec::new();
    let mut right_only = Vec::new();
    for (rc, v) in r.vars.iter().enumerate() {
        match l.col_of(*v) {
            Some(lc) => shared.push((lc, rc)),
            None => {
                vars.push(*v);
                right_only.push(rc);
            }
        }
    }
    JoinLayout {
        vars,
        shared,
        right_only,
    }
}

#[inline]
fn compatible(l: &Table, r: &Table, i: usize, j: usize, shared: &[(usize, usize)]) -> bool {
    shared.iter().all(|&(lc, rc)| {
        let (a, b) = (l.cols[lc][i], r.cols[rc][j]);
        a.is_undef() || b.is_undef() || a == b
    })
}

fn materialize(l: &Table, r: &Table, lay: &JoinLayout, pairs: &[(u32, u32)]) -> Table {
    let mut cols: Vec<Vec<Id>> = Vec::with_capacity(lay.vars.len());
    for (lc, col) in l.cols.iter().enumerate() {
        let fill = lay.shared.iter().find(|(x, _)| *x == lc).map(|(_, rc)| *rc);
        cols.push(
            pairs
                .iter()
                .map(|&(i, j)| {
                    let v = col[i as usize];
                    match (v.is_undef(), fill) {
                        (true, Some(rc)) => r.cols[rc][j as usize],
                        _ => v,
                    }
                })
                .collect(),
        );
    }
    for &rc in &lay.right_only {
        cols.push(pairs.iter().map(|&(_, j)| r.cols[rc][j as usize]).collect());
    }
    Table {
        vars: lay.vars.clone(),
        cols,
        len: pairs.len(),
        sorted: Vec::new(),
    }
}

fn has_undef(t: &Table, c: usize) -> bool {
    t.cols[c].iter().any(|v| v.is_undef())
}

/// Exponential + binary search for the first index `>= target` starting at `from`.
#[inline]
pub(super) fn gallop(col: &[Id], from: usize, target: Id) -> usize {
    let mut step = 1;
    let mut hi = from;
    while hi < col.len() && col[hi] < target {
        hi = from + step;
        step *= 2;
    }
    let lo = from + step / 4;
    let hi = hi.min(col.len());
    lo.min(hi) + col[lo.min(hi)..hi].partition_point(|x| *x < target)
}

/// The rows of a hash join's build side grouped by key. Within a partition, `map` finds
/// the span of `rows` that holds a key's rows in row order (the key is the id of a single
/// key column, or a hash of several). A partition is laid out by counting each key's rows
/// and then placing every row, so the build allocates a few arrays instead of a list per
/// key. A build side of [`PART_MIN`] rows or more is split by bits of a hash of the key
/// into partitions of about [`PART_ROWS`] rows, whose tables stay in the CPU caches while
/// they are built, in parallel.
struct KeyGroups {
    /// the partition of a key is its mixed hash shifted right by this (64: one)
    shift: u32,
    parts: Vec<KeyPart>,
}

struct KeyPart {
    map: FxHashMap<u64, (u32, u32)>,
    rows: Vec<u32>,
}

/// Rows per partition of a hash join's build side: a partition's table of about 24 bytes
/// per row fits in the per-core cache.
const PART_ROWS: usize = 8192;

/// Build sides with fewer rows have one partition, built on one thread: their table fits
/// in the caches anyway, and handing pieces to other threads costs more than it saves.
const PART_MIN: usize = 1 << 16;

/// Probe rows per parallel piece of a hash join.
const PROBE_PIECE: usize = 1 << 15;

impl KeyGroups {
    fn build(ctx: &Ctx, t: &Table, cols: &[usize]) -> Result<KeyGroups> {
        let n = t.len();
        let keys: Vec<u64> = map_rows(ctx, n, n >= PART_MIN, |i| Self::key_of(t, cols, i))?;
        let bits = if n < PART_MIN {
            0
        } else {
            (n / PART_ROWS).next_power_of_two().trailing_zeros().min(12)
        };
        let shift = 64 - bits;
        if bits == 0 {
            let part = KeyPart::build(&keys, (0..n as u32).collect());
            return Ok(KeyGroups {
                shift,
                parts: vec![part],
            });
        }
        // the rows of each partition, in row order (a counting sort on the partition)
        let part_of = |k: u64| (mix(k) >> shift) as usize;
        let mut count = vec![0u32; 1 << bits];
        for &k in &keys {
            count[part_of(k)] += 1;
        }
        let mut at: Vec<u32> = Vec::with_capacity(count.len() + 1);
        let mut sum = 0u32;
        at.push(0);
        for c in &count {
            sum += c;
            at.push(sum);
        }
        let mut next = at.clone();
        let mut order = vec![0u32; n];
        for (i, &k) in keys.iter().enumerate() {
            let p = &mut next[part_of(k)];
            order[*p as usize] = i as u32;
            *p += 1;
        }
        ctx.check()?;
        let parts = (0..count.len())
            .into_par_iter()
            .map(|p| KeyPart::build(&keys, order[at[p] as usize..at[p + 1] as usize].to_vec()))
            .collect();
        Ok(KeyGroups { shift, parts })
    }

    /// The table key of row `i` of `t` on the key columns `cols`.
    #[inline]
    fn key_of(t: &Table, cols: &[usize], i: usize) -> u64 {
        match cols {
            [c] => t.cols[*c][i].0,
            _ => {
                use std::hash::{Hash, Hasher};
                let mut h = rustc_hash::FxHasher::default();
                for &c in cols {
                    t.cols[c][i].0.hash(&mut h);
                }
                h.finish()
            }
        }
    }

    /// The build rows of key `k`, in row order (none for a key not in the table).
    #[inline]
    fn rows(&self, k: u64) -> &[u32] {
        let part = if self.shift == 64 {
            &self.parts[0]
        } else {
            &self.parts[(mix(k) >> self.shift) as usize]
        };
        match part.map.get(&k) {
            Some(&(s, e)) => &part.rows[s as usize..e as usize],
            None => &[],
        }
    }

    fn len(&self) -> usize {
        self.parts.iter().map(|p| p.map.len()).sum()
    }
}

impl KeyPart {
    /// The groups of the rows `rows` (in row order) of the keys `keys`.
    fn build(keys: &[u64], rows: Vec<u32>) -> KeyPart {
        // each key's number while counting, then the span of its rows
        let mut map: FxHashMap<u64, (u32, u32)> = FxHashMap::default();
        map.reserve(rows.len().min(PART_MIN));
        let mut group: Vec<u32> = Vec::with_capacity(rows.len());
        let mut count: Vec<u32> = Vec::new();
        for &i in &rows {
            let next = count.len() as u32;
            let g = map.entry(keys[i as usize]).or_insert((next, 0)).0;
            if g == next {
                count.push(0);
            }
            count[g as usize] += 1;
            group.push(g);
        }
        // `start[g]`: where the rows of key `g` begin
        let mut start: Vec<u32> = Vec::with_capacity(count.len());
        let mut at = 0u32;
        for c in &count {
            start.push(at);
            at += c;
        }
        for v in map.values_mut() {
            let g = v.0 as usize;
            *v = (start[g], start[g] + count[g]);
        }
        let mut placed = vec![0u32; rows.len()];
        for (&i, &g) in rows.iter().zip(&group) {
            let p = &mut start[g as usize];
            placed[*p as usize] = i;
            *p += 1;
        }
        KeyPart { map, rows: placed }
    }
}

/// A hash of an id for choosing its partition, unlike the hash tables' own hash (whose
/// high bits pick the slots' tags).
#[inline]
fn mix(k: u64) -> u64 {
    (k ^ (k >> 29)).wrapping_mul(0x9e37_79b9_7f4a_7c15)
}

/// Pairs of compatible rows (inner join).
fn join_pairs(
    ctx: &Ctx,
    l: &Table,
    r: &Table,
    lay: &JoinLayout,
    merge: bool,
    note: &mut Option<String>,
) -> Result<Vec<(u32, u32)>> {
    let mut pairs: Vec<(u32, u32)> = Vec::new();
    // a pair per output row now, the materialized row later
    let w = lay.vars.len() + 1;
    if lay.shared.is_empty() {
        // the product size is known up front: reject it before allocating anything
        ctx.check_output(l.len().saturating_mul(r.len()), w)?;
        for i in 0..l.len() {
            if i % 1024 == 0 {
                ctx.check()?;
            }
            for j in 0..r.len() {
                pairs.push((i as u32, j as u32));
            }
            ctx.check_output(pairs.len(), w)?;
        }
        return Ok(pairs);
    }
    // key columns without UNDEF on both sides can be matched by equality
    let exact: Vec<(usize, usize)> = lay
        .shared
        .iter()
        .copied()
        .filter(|&(lc, rc)| !has_undef(l, lc) && !has_undef(r, rc))
        .collect();
    if exact.is_empty() {
        // nested loop with compatibility check (UNDEF on join columns)
        for i in 0..l.len() {
            if i % 1024 == 0 {
                ctx.check()?;
            }
            for j in 0..r.len() {
                if compatible(l, r, i, j, &lay.shared) {
                    pairs.push((i as u32, j as u32));
                }
            }
            ctx.check_output(pairs.len(), w)?;
        }
        return Ok(pairs);
    }
    let (lk, rk) = exact[0];
    let lsorted = l.sorted.first().is_some_and(|v| l.col_of(*v) == Some(lk));
    let rsorted = r.sorted.first().is_some_and(|v| r.col_of(*v) == Some(rk));
    if merge && lsorted && rsorted {
        // zipper join with galloping (QLever)
        let (a, b) = (&l.cols[lk], &r.cols[rk]);
        // most merge joins match each row of the smaller side about once
        pairs.reserve(a.len().min(b.len()));
        let (mut i, mut j) = (0usize, 0usize);
        let mut steps = 0usize;
        while i < a.len() && j < b.len() {
            steps += 1;
            if steps.is_multiple_of(4096) {
                ctx.check()?;
                ctx.check_output(pairs.len(), w)?;
            }
            match a[i].cmp(&b[j]) {
                Ordering::Less => i = gallop(a, i, b[j]),
                Ordering::Greater => j = gallop(b, j, a[i]),
                Ordering::Equal => {
                    let ie = run_end(a, i);
                    let je = run_end(b, j);
                    // an equal-key run emits up to the product of its lengths: with a
                    // single key that is exact, so an oversized run fails before it is
                    // expanded; otherwise the budget is checked while expanding. Small
                    // runs are covered by the periodic check above.
                    let run = (ie - i).saturating_mul(je - j);
                    if lay.shared.len() == 1 && run > 64 {
                        ctx.check_output(pairs.len().saturating_add(run), w)?;
                    }
                    for ii in i..ie {
                        if (ii - i) % 1024 == 1023 {
                            ctx.check()?;
                            ctx.check_output(pairs.len(), w)?;
                        }
                        for jj in j..je {
                            if lay.shared.len() == 1 || compatible(l, r, ii, jj, &lay.shared) {
                                pairs.push((ii as u32, jj as u32));
                            }
                        }
                    }
                    i = ie;
                    j = je;
                }
            }
        }
        return Ok(pairs);
    }
    // hash join: build on the smaller side, probe preserves the larger side's order
    let build_left = l.len() < r.len();
    let (bt, pt) = if build_left { (l, r) } else { (r, l) };
    let bcols: Vec<usize> = exact
        .iter()
        .map(|&(lc, rc)| if build_left { lc } else { rc })
        .collect();
    let pcols: Vec<usize> = exact
        .iter()
        .map(|&(lc, rc)| if build_left { rc } else { lc })
        .collect();
    let others_needed = exact.len() < lay.shared.len();
    let emit = |bi: usize, pi: usize, pairs: &mut Vec<(u32, u32)>| {
        let (i, j) = if build_left { (bi, pi) } else { (pi, bi) };
        if !others_needed || compatible(l, r, i, j, &lay.shared) {
            pairs.push((i as u32, j as u32));
        }
    };
    if ctx.opt.flat_hash_join {
        let groups = KeyGroups::build(ctx, bt, &bcols)?;
        let single = bcols.len() == 1;
        // with several key columns the table is keyed by a hash of them, so a key's
        // rows can include rows of other keys with the same hash
        let same = |bi: usize, pi: usize| {
            single
                || bcols
                    .iter()
                    .zip(&pcols)
                    .all(|(&b, &p)| bt.cols[b][bi] == pt.cols[p][pi])
        };
        // the pairs of the probe rows `[s, e)`, in probe order; `total` counts the pairs
        // of all pieces for the output limits
        let total = AtomicUsize::new(0);
        let pcol = single.then(|| &pt.cols[pcols[0]]);
        let piece = |s: usize, e: usize| -> Result<Vec<(u32, u32)>> {
            let mut out = Vec::new();
            let mut counted = 0;
            for pi in s..e {
                if (pi - s) % 4096 == 4095 {
                    let n = out.len() - counted;
                    counted = out.len();
                    let all = total.fetch_add(n, AtomicOrdering::Relaxed) + n;
                    ctx.check()?;
                    ctx.check_output(all, w)?;
                }
                let k = match pcol {
                    Some(c) => c[pi].0,
                    None => KeyGroups::key_of(pt, &pcols, pi),
                };
                let m = groups.rows(k);
                if m.len() > 1024 {
                    let all = total.load(AtomicOrdering::Relaxed);
                    ctx.check_output(all.saturating_add(out.len() - counted + m.len()), w)?;
                }
                for &bi in m {
                    if same(bi as usize, pi) {
                        emit(bi as usize, pi, &mut out);
                    }
                }
            }
            total.fetch_add(out.len() - counted, AtomicOrdering::Relaxed);
            Ok(out)
        };
        let n = pt.len();
        if n < 2 * PROBE_PIECE {
            pairs = piece(0, n)?;
        } else {
            let parts: Vec<Vec<(u32, u32)>> = (0..n.div_ceil(PROBE_PIECE))
                .into_par_iter()
                .map(|p| piece(p * PROBE_PIECE, ((p + 1) * PROBE_PIECE).min(n)))
                .collect::<Result<_>>()?;
            ctx.check_output(parts.iter().map(Vec::len).sum(), w)?;
            pairs = parts.concat();
        }
        *note = Some(format!(
            "[flat hash table: {} keys in {} parts]",
            groups.len(),
            groups.parts.len()
        ));
    } else if bcols.len() == 1 {
        let mut map: FxHashMap<Id, Vec<u32>> = FxHashMap::default();
        for (i, v) in bt.cols[bcols[0]].iter().enumerate() {
            map.entry(*v).or_default().push(i as u32);
        }
        for (pi, v) in pt.cols[pcols[0]].iter().enumerate() {
            if pi % 65536 == 0 {
                ctx.check()?;
                ctx.check_output(pairs.len(), w)?;
            }
            if let Some(m) = map.get(v) {
                if m.len() > 1024 {
                    ctx.check_output(pairs.len().saturating_add(m.len()), w)?;
                }
                for &bi in m {
                    emit(bi as usize, pi, &mut pairs);
                }
            }
        }
    } else {
        let mut map: FxHashMap<Vec<Id>, Vec<u32>> = FxHashMap::default();
        for i in 0..bt.len() {
            map.entry(bcols.iter().map(|&c| bt.cols[c][i]).collect())
                .or_default()
                .push(i as u32);
        }
        let mut key = Vec::with_capacity(pcols.len());
        for pi in 0..pt.len() {
            if pi % 65536 == 0 {
                ctx.check()?;
                ctx.check_output(pairs.len(), w)?;
            }
            key.clear();
            key.extend(pcols.iter().map(|&c| pt.cols[c][pi]));
            if let Some(m) = map.get(&key) {
                if m.len() > 1024 {
                    ctx.check_output(pairs.len().saturating_add(m.len()), w)?;
                }
                for &bi in m {
                    emit(bi as usize, pi, &mut pairs);
                }
            }
        }
    }
    Ok(pairs)
}

/// Number of join results; merge joins on a single exact key count run products.
fn join_count(ctx: &Ctx, l: &Table, r: &Table, merge: bool) -> Result<u64> {
    let lay = layout(l, r);
    if merge && lay.shared.len() == 1 {
        let (lk, rk) = lay.shared[0];
        let lsorted = l.sorted.first().is_some_and(|v| l.col_of(*v) == Some(lk));
        let rsorted = r.sorted.first().is_some_and(|v| r.col_of(*v) == Some(rk));
        if lsorted && rsorted && !has_undef(l, lk) && !has_undef(r, rk) {
            let (a, b) = (&l.cols[lk], &r.cols[rk]);
            let (mut i, mut j, mut n) = (0usize, 0usize, 0u64);
            while i < a.len() && j < b.len() {
                match a[i].cmp(&b[j]) {
                    Ordering::Less => i = gallop(a, i, b[j]),
                    Ordering::Greater => j = gallop(b, j, a[i]),
                    Ordering::Equal => {
                        let ie = run_end(a, i);
                        let je = run_end(b, j);
                        n += ((ie - i) * (je - j)) as u64;
                        i = ie;
                        j = je;
                    }
                }
            }
            return Ok(n);
        }
    }
    Ok(join_pairs(ctx, l, r, &lay, merge, &mut None)?.len() as u64)
}

/// Decompose RDF 1.2 triple terms: rows whose `t` is not a triple term, or whose
/// components do not match the constants / already bound variables, are dropped.
fn unpack(
    ctx: &Ctx,
    input: Table,
    t: VarId,
    parts: &[PathEnd; 3],
    vars: &[VarId],
) -> Result<Table> {
    let tc = input.col_of(t);
    let mut out = Table::new(vars.to_vec());
    let map: Vec<Option<usize>> = vars.iter().map(|v| input.col_of(*v)).collect();
    let mut row = vec![Id::UNDEF; vars.len()];
    for i in 0..input.len() {
        if i % 4096 == 0 {
            ctx.check()?;
        }
        let Some(tc) = tc else { break };
        let Some(oxrdf::Term::Triple(tr)) = ctx.term(input.cols[tc][i]) else {
            continue;
        };
        let comps = [
            ctx.intern_term(&tr.subject.clone().into()),
            ctx.intern_term(&oxrdf::Term::NamedNode(tr.predicate.clone())),
            ctx.intern_term(&tr.object),
        ];
        for (j, m) in map.iter().enumerate() {
            row[j] = m.map_or(Id::UNDEF, |c| input.cols[c][i]);
        }
        let mut ok = true;
        for (p, c) in parts.iter().zip(comps) {
            match p {
                PathEnd::Const(k) => ok &= *k == c,
                PathEnd::Var(v) => {
                    let j = vars.iter().position(|x| x == v).unwrap();
                    if row[j].is_undef() {
                        row[j] = c;
                    } else {
                        ok &= row[j] == c;
                    }
                }
            }
            if !ok {
                break;
            }
        }
        if ok {
            out.push_row(&row);
        }
    }
    Ok(out)
}

pub(super) fn join_tables(
    ctx: &Ctx,
    l: &Table,
    r: &Table,
    _keys: &[VarId],
    merge: bool,
) -> Result<Table> {
    join_noted(ctx, l, r, merge, &mut None)
}

/// Algorithm output uses the same pair-generation and materialization semantics.
/// The final backing is admitted while the two inputs and pair array are still live.
pub(super) fn join_algorithm_tables(ctx: &Ctx, l: &Table, r: &Table) -> Result<Materialized> {
    use crate::algo::memory;
    let lay = layout(l, r);
    let pairs = join_pairs(ctx, l, r, &lay, false, &mut None)?;
    ctx.check_output(pairs.len(), lay.vars.len() + 1)?;
    let sorted_len = kept_order_iter(l, r, &lay, &pairs).count();
    let col = memory::bytes::<Id>(pairs.len())?;
    let bytes = memory::sum(&[
        col.checked_mul(lay.vars.len() as u64)
            .ok_or_else(|| Error::invalid("algorithm join capacity overflows"))?,
        memory::bytes::<VarId>(lay.vars.len())?,
        memory::bytes::<Vec<Id>>(lay.vars.len())?,
        memory::bytes::<VarId>(sorted_len)?,
        128,
        if lay.vars.is_empty() { col } else { 0 },
    ])?;
    let mut out = Materialized {
        table: Table::empty(Vec::new()),
        lease: Some(ctx.output_lease(bytes)?),
    };
    out.table = materialize(l, r, &lay, &pairs);
    let ctl = crate::algo::Control::with_cancellation(
        ctx.cancel.clone(),
        ctx.cancellation.clone(),
        ctx.current_deadline(),
        u64::MAX,
    );
    let mut sorted = memory::allocate("algorithm-join-order", sorted_len, &ctl)?;
    for v in kept_order_iter(l, r, &lay, &pairs) {
        sorted.push(v);
    }
    out.table.sorted = sorted;
    if super::algosvc::table_capacity(&out.table)? != bytes {
        return Err(Error::invalid(
            "algorithm join allocator exceeded admitted capacity",
        ));
    }
    Ok(out)
}

/// The join of two tables, with a note on how the rows were matched.
pub(super) fn join_noted(
    ctx: &Ctx,
    l: &Table,
    r: &Table,
    merge: bool,
    note: &mut Option<String>,
) -> Result<Table> {
    let lay = layout(l, r);
    let pairs = join_pairs(ctx, l, r, &lay, merge, note)?;
    ctx.check_output(pairs.len(), lay.vars.len() + 1)?;
    let mut t = materialize(l, r, &lay, &pairs);
    t.sorted = kept_order(l, r, &lay, &pairs);
    Ok(t)
}

/// The sort order that joined rows keep from an input. A hash join emits its pairs in
/// the order of the side it probes, so that side's sort variables stay sorted, up to the
/// first one shared with the other side where the side holds unbound values (the other
/// side fills them).
fn kept_order(l: &Table, r: &Table, lay: &JoinLayout, pairs: &[(u32, u32)]) -> Vec<VarId> {
    kept_order_iter(l, r, lay, pairs).collect()
}
fn kept_order_iter<'a>(
    l: &'a Table,
    r: &'a Table,
    lay: &'a JoinLayout,
    pairs: &[(u32, u32)],
) -> impl Iterator<Item = VarId> + 'a {
    let (t, left) = if pairs.windows(2).all(|w| w[0].0 <= w[1].0) {
        (l, true)
    } else if pairs.windows(2).all(|w| w[0].1 <= w[1].1) {
        (r, false)
    } else {
        (l, false)
    };
    let ordered =
        pairs.windows(2).all(|w| w[0].0 <= w[1].0) || pairs.windows(2).all(|w| w[0].1 <= w[1].1);
    let sorted = if ordered { &t.sorted[..] } else { &[] };
    sorted
        .iter()
        .take_while(move |v| {
            let Some(c) = t.col_of(**v) else {
                return false;
            };
            let shared = lay
                .shared
                .iter()
                .any(|&(lc, rc)| if left { lc == c } else { rc == c });
            !shared || !has_undef(t, c)
        })
        .copied()
}

/// A single variable value-equality over two independent, certainly-bound inputs.
/// This is an execution hint, not an RDF-term join or an algebra rewrite. The
/// original FILTER is evaluated after candidate generation in every case.
fn string_cross_hint(n: &Node) -> Option<(VarId, VarId)> {
    let Kind::Filter(es) = &n.kind else {
        return None;
    };
    let [Expr::Eq(a, b)] = es.as_slice() else {
        return None;
    };
    let (Expr::Var(a), Expr::Var(b)) = (&**a, &**b) else {
        return None;
    };
    let [cross] = n.children.as_slice() else {
        return None;
    };
    if !matches!(
        cross.kind,
        Kind::Join {
            algo: JoinAlgo::Cross,
            ..
        }
    ) {
        return None;
    }
    let [l, r] = cross.children.as_slice() else {
        return None;
    };
    if l.certain.contains(a) && !r.vars.contains(a) && r.certain.contains(b) && !l.vars.contains(b)
    {
        Some((*a, *b))
    } else if l.certain.contains(b)
        && !r.vars.contains(b)
        && r.certain.contains(a)
        && !l.vars.contains(a)
    {
        Some((*b, *a))
    } else {
        None
    }
}

/// Keep the first admission deliberately narrow: no expression-bearing inputs,
/// OPTIONAL, SERVICE or extension operators. Walk iteratively, and only after an
/// actually oversized product has made candidate pruning worth considering.
fn plain_cross_input(ctx: &Ctx, n: &Node) -> Result<bool> {
    let mut todo = vec![n];
    let mut examined = 0usize;
    while let Some(n) = todo.pop() {
        if examined.is_multiple_of(1024) {
            ctx.check()?;
        }
        examined += 1;
        if !matches!(
            n.kind,
            Kind::Scan(_) | Kind::Values(_) | Kind::Project(_) | Kind::Sort(_) | Kind::Join { .. }
        ) {
            return Ok(false);
        }
        todo.extend(&n.children);
    }
    Ok(true)
}

/// Generate only string-equal pairs when the complete product would exceed an
/// existing row or memory limit. Numeric promotion is not an equivalence relation
/// suitable for this index, and language/error/unbound cases keep the generic
/// evaluator. Declining never reruns the inputs or partially emits their result.
fn string_cross(
    ctx: &Ctx,
    n: &Node,
    l: &Table,
    r: &Table,
    (lv, rv): (VarId, VarId),
) -> Result<Option<Table>> {
    let product = l.len().saturating_mul(r.len());
    if product <= ctx.max_rows
        && (product as u64) <= ctx.max_rows_produced.saturating_sub(ctx.rows_produced())
        && product <= ctx.rows_within_budget(n.vars.len() + 1)
    {
        return Ok(None);
    }
    for child in &n.children {
        if !plain_cross_input(ctx, child)? {
            return Ok(None);
        }
    }
    let lay = layout(l, r);
    if !lay.shared.is_empty() {
        return Ok(None);
    }
    let (Some(lc), Some(rc)) = (l.col_of(lv), r.col_of(rv)) else {
        return Ok(None);
    };
    // Inspect the encoded domain without filling the uncharged eager value cache.
    // Vocabulary reconstruction scratch is charged by with_string_lexical; no
    // lexical allocation can escape into the index before its reservation.
    for col in [&l.cols[lc], &r.cols[rc]] {
        for (i, &id) in col.iter().enumerate() {
            if i.is_multiple_of(1024) {
                ctx.check()?;
            }
            if with_string_lexical(ctx, id, |_| Ok(()))?.is_none() {
                return Ok(None);
            }
        }
    }
    type StringEntry = (Box<str>, usize);
    let index_charge =
        ctx.charge((r.len() as u64).saturating_mul(std::mem::size_of::<StringEntry>() as u64))?;
    let mut index = Vec::<StringEntry>::with_capacity(r.len());
    for (j, &id) in r.cols[rc].iter().enumerate() {
        if j.is_multiple_of(1024) {
            ctx.check()?;
        }
        let key = with_string_lexical(ctx, id, |s| {
            index_charge.add(s.len() as u64)?;
            Ok(Box::<str>::from(s))
        })?
        .expect("proved string domain");
        index.push((key, j));
    }
    // One array, no extra sort scratch. Including row numbers in the key keeps
    // equal strings in right-row order, preserving the original product order.
    sort_candidates(ctx, &mut index)?;
    let range = |s: &str| {
        let begin = index.partition_point(|(key, _)| key.as_ref() < s);
        let end = begin + index[begin..].partition_point(|(key, _)| key.as_ref() == s);
        (begin, end)
    };
    // Count matching pairs before allocating them. This avoids a reallocation for
    // each short key run and admits exactly the actual output, not the product or
    // a geometric capacity that might itself exceed a real query limit.
    let mut rows = 0usize;
    for (i, &id) in l.cols[lc].iter().enumerate() {
        if i.is_multiple_of(1024) {
            ctx.check()?;
        }
        with_string_lexical(ctx, id, |s| {
            let (begin, end) = range(s);
            rows = rows.saturating_add(end - begin);
            ctx.check_output_rows(rows)
        })?
        .expect("proved string domain");
    }
    // Includes the pair array while the matching table is materialized, with all
    // lexical index payloads and the input tables still charged and alive.
    ctx.check_output(rows, lay.vars.len() + 1)?;
    // This buffer remains live while subsequent key reconstruction reserves its
    // own scratch. A peak-only output estimate cannot account for that overlap.
    let _pairs_charge =
        ctx.charge((rows as u64).saturating_mul(std::mem::size_of::<(u32, u32)>() as u64))?;
    let mut pairs = Vec::with_capacity(rows);
    for (i, &id) in l.cols[lc].iter().enumerate() {
        if i.is_multiple_of(1024) {
            ctx.check()?;
        }
        with_string_lexical(ctx, id, |s| {
            let (begin, end) = range(s);
            for (_, j) in &index[begin..end] {
                if pairs.len().is_multiple_of(1024) {
                    ctx.check()?;
                }
                pairs.push((i as u32, *j as u32));
            }
            Ok(())
        })?
        .expect("proved string domain");
    }
    ctx.check()?;
    // Pair capacity is already live; admit only the output columns here.
    ctx.check_output(rows, lay.vars.len())?;
    Ok(Some(materialize_candidates(ctx, l, r, &lay, &pairs)?))
}

/// Exactly the raw key encodings that Value::from_key reads as a string. Invalid
/// UTF-8 declines this optimization instead of changing lossy-decoding equality.
fn string_lexical_key(key: &[u8]) -> Option<&str> {
    if key.first() != Some(&b'"') {
        return None;
    }
    let sep = key
        .iter()
        .rposition(|&b| b == crate::id::KEY_SEP)
        .unwrap_or(key.len());
    let suffix = key.get(sep + 1..).unwrap_or(&[]);
    if !suffix.is_empty() && suffix != b"^http://www.w3.org/2001/XMLSchema#string" {
        return None;
    }
    std::str::from_utf8(&key[1..sep]).ok()
}

/// Read a borrowed lexical key with the existing charged vocabulary scratch.
/// Unlike Ctx::value this does not populate the eager decoded-value cache.
fn with_string_lexical<T>(
    ctx: &Ctx,
    id: Id,
    mut f: impl FnMut(&str) -> Result<T>,
) -> Result<Option<T>> {
    use crate::id::Tag;
    let mut read = |key: &[u8]| match string_lexical_key(key) {
        Some(s) => f(s).map(Some),
        None => Ok(None),
    };
    match id.tag() {
        Tag::Vocab => ctx
            .with_cursor_vocab_key(id.payload(), &mut read)?
            .unwrap_or(Ok(None)),
        Tag::Delta => ctx
            .snap
            .generation
            .dvocab
            .with(|v| v.get(id.payload()).map(&mut read).unwrap_or(Ok(None))),
        Tag::Local => ctx.with_local_key(id.payload(), read).unwrap_or(Ok(None)),
        _ => Ok(None),
    }
}

/// In-place heap sort, polling per comparison rather than waiting for a complete
/// library sort. This is used only by the admitted string candidate index.
fn sort_candidates<T: Ord>(ctx: &Ctx, values: &mut [T]) -> Result<()> {
    fn sift<T>(
        values: &mut [T],
        mut root: usize,
        end: usize,
        cmp: &mut impl FnMut(&T, &T) -> Result<Ordering>,
    ) -> Result<()> {
        while root < end / 2 {
            let mut child = root * 2 + 1;
            if child + 1 < end && cmp(&values[child], &values[child + 1])? == Ordering::Less {
                child += 1;
            }
            if cmp(&values[root], &values[child])? != Ordering::Less {
                break;
            }
            values.swap(root, child);
            root = child;
        }
        Ok(())
    }
    ctx.check()?;
    let mut compared = 0usize;
    let mut cmp = |a: &T, b: &T| {
        if compared.is_multiple_of(1024) {
            ctx.check()?;
        }
        compared += 1;
        Ok(a.cmp(b))
    };
    let n = values.len();
    for root in (0..n / 2).rev() {
        sift(values, root, n, &mut cmp)?;
    }
    for end in (1..n).rev() {
        values.swap(0, end);
        sift(values, 0, end, &mut cmp)?;
    }
    ctx.check()
}

/// The admitted string product keeps the ordinary join layout, but copies each
/// column in checked chunks instead of entering an unbounded materialize loop.
fn materialize_candidates(
    ctx: &Ctx,
    l: &Table,
    r: &Table,
    lay: &JoinLayout,
    pairs: &[(u32, u32)],
) -> Result<Table> {
    ctx.check()?;
    let mut cols = Vec::with_capacity(lay.vars.len());
    for (lc, col) in l.cols.iter().enumerate() {
        let fill = lay.shared.iter().find(|(x, _)| *x == lc).map(|(_, rc)| *rc);
        let mut out = Vec::with_capacity(pairs.len());
        for part in pairs.chunks(1024) {
            ctx.check()?;
            out.extend(part.iter().map(|&(i, j)| {
                let v = col[i as usize];
                match (v.is_undef(), fill) {
                    (true, Some(rc)) => r.cols[rc][j as usize],
                    _ => v,
                }
            }));
        }
        cols.push(out);
    }
    for &rc in &lay.right_only {
        let mut out = Vec::with_capacity(pairs.len());
        for part in pairs.chunks(1024) {
            ctx.check()?;
            out.extend(part.iter().map(|&(_, j)| r.cols[rc][j as usize]));
        }
        cols.push(out);
    }
    ctx.check()?;
    Ok(Table {
        vars: lay.vars.clone(),
        cols,
        len: pairs.len(),
        sorted: Vec::new(),
    })
}

fn cross(ctx: &Ctx, l: &Table, r: &Table) -> Result<Table> {
    let lay = layout(l, r);
    ctx.check_output(l.len().saturating_mul(r.len()), lay.vars.len() + 1)?;
    let pairs = join_pairs(ctx, l, r, &lay, false, &mut None)?;
    Ok(materialize(l, r, &lay, &pairs))
}

/// Whether `t` is sorted on its column `c` first.
fn sorted_on(t: &Table, c: usize) -> bool {
    t.sorted.first().is_some_and(|v| t.col_of(*v) == Some(c))
}

/// Which rows of `joined` pass the OPTIONAL's FILTER `e`.
fn left_filter(ctx: &Ctx, joined: &Table, e: &Expr) -> Vec<bool> {
    let map = joined.var_map(ctx.nvars());
    (0..joined.len())
        .map(|i| {
            ebv(
                e,
                &Row {
                    table: joined,
                    i,
                    map: &map,
                    dec: None,
                },
                ctx,
            )
            .unwrap_or(false)
        })
        .collect()
}

pub(super) fn left_join(
    ctx: &Ctx,
    l: &Table,
    r: &Table,
    expr: Option<&Expr>,
    note: &mut Option<String>,
) -> Result<Table> {
    let lay = layout(l, r);
    if ctx.opt.merge_left_join
        && let [(lc, rc)] = lay.shared[..]
        && sorted_on(l, lc)
        && sorted_on(r, rc)
        && !has_undef(l, lc)
        && !has_undef(r, rc)
    {
        *note = Some(format!("[merge on ?{}]", ctx.var_name(l.vars[lc])));
        return merge_left_join(ctx, l, r, &lay, (lc, rc), expr);
    }
    let mut pairs = join_pairs(ctx, l, r, &lay, false, note)?;
    let mut joined = materialize(l, r, &lay, &pairs);
    if let Some(e) = expr {
        let keep = left_filter(ctx, &joined, e);
        joined.filter_rows(&keep);
        let mut k = keep.iter();
        pairs.retain(|_| *k.next().unwrap());
    }
    let mut matched = vec![false; l.len()];
    for (i, _) in &pairs {
        matched[*i as usize] = true;
    }
    append_unmatched(l, &mut joined, &matched);
    Ok(joined)
}

/// Append to `joined` each row of `l` that `matched` does not mark, with the right
/// side's variables unbound.
fn append_unmatched(l: &Table, joined: &mut Table, matched: &[bool]) {
    let missing: Vec<usize> = (0..l.len()).filter(|&i| !matched[i]).collect();
    if missing.is_empty() {
        return;
    }
    for (c, col) in l.cols.iter().enumerate() {
        joined.cols[c].extend(missing.iter().map(|&i| col[i]));
    }
    for c in l.width()..joined.width() {
        joined.cols[c].extend(std::iter::repeat_n(Id::UNDEF, missing.len()));
    }
    joined.len += missing.len();
}

/// The candidate pairs a keyed OPTIONAL tests its condition on at a time.
const KEYED_CHUNK: usize = 1 << 16;

/// OPTIONAL keyed on its condition's conjunct `?a = ?b` (see [`Kind::KeyedLeftJoin`]).
pub(super) fn keyed_left_join(
    ctx: &Ctx,
    l: &Table,
    r: &Table,
    expr: &Expr,
    keys: (VarId, VarId),
    note: &mut Option<String>,
) -> Result<Table> {
    keyed_join(ctx, l, r, expr, keys, true, note)
}

/// A join keyed on the conjunct `?a = ?b` of its condition `expr`, an OPTIONAL when
/// `optional` is set ([`Kind::KeyedLeftJoin`]) and an inner join otherwise
/// ([`Kind::KeyedJoin`]). Each value gets a candidate key ([`super::eqkey`]): a left row
/// meets the right rows with the same key, a loose value meets every loose value, and an
/// unbound value meets no row, because `=` raises an error on it and the conjunction
/// cannot hold. The whole condition is tested on the candidate pairs a chunk at a time,
/// so neither the product nor the candidate pairs are held at once.
fn keyed_join(
    ctx: &Ctx,
    l: &Table,
    r: &Table,
    expr: &Expr,
    (a, b): (VarId, VarId),
    optional: bool,
    note: &mut Option<String>,
) -> Result<Table> {
    use super::eqkey::{EqKey, eq_key};
    let lay = layout(l, r);
    let generic = |note: &mut Option<String>| {
        if optional {
            left_join(ctx, l, r, Some(expr), note)
        } else {
            let mut t = cross(ctx, l, r)?;
            let keep = left_filter(ctx, &t, expr);
            t.filter_rows(&keep);
            Ok(t)
        }
    };
    let (Some(lc), Some(rc)) = (l.col_of(a), r.col_of(b)) else {
        return generic(note);
    };
    if !lay.shared.is_empty() {
        return generic(note);
    }
    // The right rows by key, sorted, and the rows of loose values in row order. Both are
    // charged before they are allocated, the second for its growth.
    let _index = ctx.charge((r.len() as u64).saturating_mul(16 + 8))?;
    let mut keyed: Vec<(Id, u32)> = Vec::with_capacity(r.len());
    let mut loose: Vec<u32> = Vec::new();
    for (j, &v) in r.cols[rc].iter().enumerate() {
        if j % 65536 == 0 {
            ctx.check()?;
        }
        match eq_key(ctx, v)? {
            EqKey::Unbound => {}
            EqKey::Key(k) => keyed.push((k, j as u32)),
            EqKey::Loose => loose.push(j as u32),
        }
    }
    keyed.sort_unstable();
    let w = lay.vars.len() + 1;
    // A chunk's pairs, their merged rows and their verdicts, and a mark per left row.
    // The chunk takes at most a quarter of the room the query has left.
    let per_pair = 8 + w as u64 * 8 + 1;
    let room = usize::try_from(ctx.memory_room() / 4 / per_pair).unwrap_or(usize::MAX);
    let cap = KEYED_CHUNK
        .min(l.len().saturating_mul(r.len()))
        .min(room)
        .max(1);
    let _scratch = ctx.charge(
        (cap as u64)
            .saturating_mul(per_pair)
            .saturating_add(l.len() as u64),
    )?;
    let mut matched = if optional {
        vec![false; l.len()]
    } else {
        Vec::new()
    };
    let mut out = Table::new(lay.vars.clone());
    let mut chunk: Vec<(u32, u32)> = Vec::with_capacity(cap);
    let mut candidates = 0u64;
    let flush =
        |chunk: &mut Vec<(u32, u32)>, out: &mut Table, matched: &mut [bool]| -> Result<()> {
            ctx.check()?;
            ctx.check_output(out.len().saturating_add(chunk.len()), w)?;
            let mut joined = materialize(l, r, &lay, chunk);
            let keep = left_filter(ctx, &joined, expr);
            if optional {
                for (k, &(i, _)) in keep.iter().zip(chunk.iter()) {
                    if *k {
                        matched[i as usize] = true;
                    }
                }
            }
            joined.filter_rows(&keep);
            out.append(joined);
            chunk.clear();
            Ok(())
        };
    for (i, &v) in l.cols[lc].iter().enumerate() {
        if i % 4096 == 0 {
            ctx.check()?;
        }
        let rows: &[u32] = match eq_key(ctx, v)? {
            EqKey::Unbound => continue,
            EqKey::Key(k) => {
                let s = keyed.partition_point(|&(x, _)| x < k);
                let e = s + keyed[s..].partition_point(|&(x, _)| x == k);
                for &(_, j) in &keyed[s..e] {
                    chunk.push((i as u32, j));
                    if chunk.len() >= cap {
                        candidates += chunk.len() as u64;
                        flush(&mut chunk, &mut out, &mut matched)?;
                    }
                }
                continue;
            }
            EqKey::Loose => &loose,
        };
        for &j in rows {
            chunk.push((i as u32, j));
            if chunk.len() >= cap {
                candidates += chunk.len() as u64;
                flush(&mut chunk, &mut out, &mut matched)?;
            }
        }
    }
    if !chunk.is_empty() {
        candidates += chunk.len() as u64;
        flush(&mut chunk, &mut out, &mut matched)?;
    }
    if optional {
        append_unmatched(l, &mut out, &matched);
    }
    ctx.check_output(out.len(), lay.vars.len())?;
    *note = Some(format!(
        "[keyed on ?{} = ?{}: {} candidate pairs]",
        ctx.var_name(a),
        ctx.var_name(b),
        candidates
    ));
    Ok(out)
}

/// A right row index standing for no row: the left row is kept with the right side's
/// variables unbound.
const NO_ROW: u32 = u32::MAX;

/// OPTIONAL by a merge on the key columns `(lc, rc)`, which both sides are sorted on and
/// neither leaves unbound. The output keeps the left side's rows in order, each one with
/// its matches or alone, so it is sorted like the left side.
fn merge_left_join(
    ctx: &Ctx,
    l: &Table,
    r: &Table,
    lay: &JoinLayout,
    (lc, rc): (usize, usize),
    expr: Option<&Expr>,
) -> Result<Table> {
    let (a, b) = (&l.cols[lc], &r.cols[rc]);
    // `first[i]`: the first right row whose key is at least left row i's
    let first = super::zipper::lower_bounds(ctx, a, b)?;
    // each left row with the right rows of its key, or with none
    let w = lay.vars.len() + 1;
    let mut li: Vec<u32> = Vec::with_capacity(a.len());
    let mut rj: Vec<u32> = Vec::with_capacity(a.len());
    let mut run = (usize::MAX, 0);
    for (i, (&x, &s)) in a.iter().zip(&first).enumerate() {
        if i % 65536 == 65535 {
            ctx.check()?;
            ctx.check_output(li.len(), w)?;
        }
        let s = s as usize;
        if s < b.len() && b[s] == x {
            if run.0 != s {
                run = (s, run_end(b, s));
            }
            let e = run.1;
            if e - s > 1024 {
                ctx.check_output(li.len().saturating_add(e - s), w)?;
            }
            li.extend(std::iter::repeat_n(i as u32, e - s));
            rj.extend(s as u32..e as u32);
        } else {
            li.push(i as u32);
            rj.push(NO_ROW);
        }
    }
    drop(first);
    if let Some(e) = expr {
        // the FILTER is tested on the matched rows; a left row none of whose matches
        // pass is kept alone
        let matched: Vec<(u32, u32)> = li
            .iter()
            .zip(&rj)
            .filter(|(_, j)| **j != NO_ROW)
            .map(|(&i, &j)| (i, j))
            .collect();
        let joined = materialize(l, r, lay, &matched);
        let mut keep = left_filter(ctx, &joined, e).into_iter();
        drop(joined);
        let (mut li2, mut rj2) = (Vec::with_capacity(li.len()), Vec::with_capacity(li.len()));
        let mut k = 0;
        while k < li.len() {
            let i = li[k];
            let mut e = k;
            let mut any = false;
            while e < li.len() && li[e] == i {
                if rj[e] != NO_ROW && keep.next().unwrap_or(false) {
                    li2.push(i);
                    rj2.push(rj[e]);
                    any = true;
                }
                e += 1;
            }
            if !any {
                li2.push(i);
                rj2.push(NO_ROW);
            }
            k = e;
        }
        (li, rj) = (li2, rj2);
    }
    ctx.check_output(li.len(), w)?;
    let mut t = materialize_left(l, r, lay, &li, &rj);
    t.sorted = l.sorted.clone();
    Ok(t)
}

/// The rows of a left join from left row indices and right row indices (`NO_ROW` for
/// none). The left side's unbound shared variables take the right row's values.
fn materialize_left(l: &Table, r: &Table, lay: &JoinLayout, li: &[u32], rj: &[u32]) -> Table {
    // every left row once, in order: the left columns are copied whole
    let whole = li.len() == l.len() && li.iter().enumerate().all(|(n, &i)| n == i as usize);
    let mut cols: Vec<Vec<Id>> = Vec::with_capacity(lay.vars.len());
    for (lc, col) in l.cols.iter().enumerate() {
        let fill = lay.shared.iter().find(|(x, _)| *x == lc).map(|(_, rc)| *rc);
        let fill = fill.filter(|_| has_undef(l, lc));
        cols.push(match fill {
            None if whole => col.clone(),
            None => li.iter().map(|&i| col[i as usize]).collect(),
            Some(rc) => li
                .iter()
                .zip(rj)
                .map(|(&i, &j)| {
                    let v = col[i as usize];
                    if v.is_undef() && j != NO_ROW {
                        r.cols[rc][j as usize]
                    } else {
                        v
                    }
                })
                .collect(),
        });
    }
    for &rc in &lay.right_only {
        let col = &r.cols[rc];
        cols.push(
            rj.iter()
                .map(|&j| col.get(j as usize).copied().unwrap_or(Id::UNDEF))
                .collect(),
        );
    }
    Table {
        vars: lay.vars.clone(),
        cols,
        len: li.len(),
        sorted: Vec::new(),
    }
}

/// ARQ's `LET (?v := e)` where ?v may be bound (`QueryIterAssign`): `col` holds the
/// expression's value per row (`UNDEF` for an error). An unbound ?v takes the value; a
/// bound ?v keeps the solution when the value is the same value (Jena's
/// `Node.sameValueAs`) and drops it otherwise; an error leaves the solution as it is.
fn assign(ctx: &Ctx, t: &mut Table, v: VarId, col: Vec<Id>) -> Result<()> {
    let Some(c) = t.col_of(v) else {
        t.vars.push(v);
        t.cols.push(col);
        return Ok(());
    };
    let mut keep = vec![true; t.len()];
    for (i, new) in col.into_iter().enumerate() {
        if i % 4096 == 0 {
            ctx.check()?;
        }
        let old = t.cols[c][i];
        if new.is_undef() || old == new {
            continue;
        }
        if old.is_undef() {
            t.cols[c][i] = new;
            continue;
        }
        keep[i] = match (ctx.term(old), ctx.term(new)) {
            (Some(a), Some(b)) => super::cdt::same_value(&a, &b).unwrap_or(false),
            _ => false,
        };
    }
    if keep.iter().any(|k| !k) {
        let sorted = t.sorted.clone();
        t.filter_rows(&keep);
        t.sorted = sorted;
    }
    Ok(())
}

/// ARQ's `UNFOLD(e AS ?v, ?w)` (`QueryIterUnfold`): each solution once per element of
/// the `cdt:List` (?v the element, ?w its position from 1) or entry of the `cdt:Map`
/// (?v the key, ?w the value) that `e` gives. A null leaves its variable unbound, an
/// empty list or map gives no solutions, and any other value or an error gives the
/// solution once with both variables unbound.
fn unfold(
    ctx: &Ctx,
    t: &Table,
    e: &Expr,
    v: VarId,
    w: Option<VarId>,
    report: &mut ExprReport,
) -> Result<Table> {
    let col = compute_column(ctx, t, e, report)?;
    let mut vars = t.vars.clone();
    vars.push(v);
    vars.extend(w);
    let mut out = Table::new(vars);
    let mut row: Vec<Id> = Vec::with_capacity(out.width());
    // the elements of each distinct literal, interned once
    let mut seen: FxHashMap<Id, Option<Vec<(Id, Id)>>> = FxHashMap::default();
    for (i, id) in col.into_iter().enumerate() {
        if i % 4096 == 0 {
            ctx.check()?;
            ctx.check_output(out.len(), out.width())?;
        }
        let elems = seen.entry(id).or_insert_with(|| {
            let t = ctx.term(id)?;
            let parts = super::cdt::unfold(&t)?;
            let intern = |x: Option<oxrdf::Term>| x.map_or(Id::UNDEF, |x| ctx.intern_term(&x));
            Some(
                parts
                    .into_iter()
                    .map(|(a, b)| (intern(a), intern(b)))
                    .collect(),
            )
        });
        row.clear();
        row.extend(t.cols.iter().map(|c| c[i]));
        match elems {
            Some(elems) => {
                for &(a, b) in elems.iter() {
                    row.truncate(t.width());
                    row.push(a);
                    if w.is_some() {
                        row.push(b);
                    }
                    out.push_row(&row);
                }
            }
            None => {
                row.push(Id::UNDEF);
                if w.is_some() {
                    row.push(Id::UNDEF);
                }
                out.push_row(&row);
            }
        }
    }
    Ok(out)
}

/// ARQ's SEMIJOIN and ANTIJOIN (`QueryIterHalfJoin`): the rows of `l` compatible with a
/// row of `r` (with none, when `anti`), each once and unchanged. Unlike MINUS, a row of
/// `r` that shares no bound variable with a row of `l` is compatible with it.
fn half_join(ctx: &Ctx, mut l: Table, r: &Table, anti: bool) -> Result<Table> {
    let lay = layout(&l, r);
    let r_undef = lay.shared.iter().any(|&(_, rc)| has_undef(r, rc));
    let set: FxHashSet<Vec<Id>> = if r_undef {
        FxHashSet::default()
    } else {
        (0..r.len())
            .map(|j| lay.shared.iter().map(|&(_, rc)| r.cols[rc][j]).collect())
            .collect()
    };
    let mut keep = vec![false; l.len()];
    for (i, k) in keep.iter_mut().enumerate() {
        if i % 4096 == 0 {
            ctx.check()?;
        }
        let all_defined = lay.shared.iter().all(|&(lc, _)| !l.cols[lc][i].is_undef());
        let matched = if r.is_empty() {
            false
        } else if lay.shared.is_empty() {
            true
        } else if !r_undef && all_defined {
            let key: Vec<Id> = lay.shared.iter().map(|&(lc, _)| l.cols[lc][i]).collect();
            set.contains(&key)
        } else {
            (0..r.len()).any(|j| compatible(&l, r, i, j, &lay.shared))
        };
        *k = matched != anti;
    }
    let sorted = l.sorted.clone();
    l.filter_rows(&keep);
    l.sorted = sorted;
    Ok(l)
}

fn minus(ctx: &Ctx, mut l: Table, r: &Table, note: &mut Option<String>) -> Result<Table> {
    let lay = layout(&l, r);
    if lay.shared.is_empty() || r.is_empty() {
        return Ok(l);
    }
    let r_undef = lay.shared.iter().any(|&(_, rc)| has_undef(r, rc));
    if ctx.opt.anti_join
        && let [(lc, rc)] = lay.shared[..]
        && !r_undef
        && !has_undef(&l, lc)
    {
        let (keep, how) = anti_join(ctx, &l, lc, r, rc)?;
        *note = Some(format!(
            "[anti-join on ?{} by {how}]",
            ctx.var_name(l.vars[lc])
        ));
        let sorted = l.sorted.clone();
        l.filter_rows(&keep);
        l.sorted = sorted;
        return Ok(l);
    }
    let set: FxHashSet<Vec<Id>> = if r_undef {
        FxHashSet::default()
    } else {
        (0..r.len())
            .map(|j| lay.shared.iter().map(|&(_, rc)| r.cols[rc][j]).collect())
            .collect()
    };
    let mut keep = vec![true; l.len()];
    for (i, k) in keep.iter_mut().enumerate() {
        if i % 4096 == 0 {
            ctx.check()?;
        }
        let all_defined = lay.shared.iter().all(|&(lc, _)| !l.cols[lc][i].is_undef());
        let removed = if !r_undef && all_defined {
            let key: Vec<Id> = lay.shared.iter().map(|&(lc, _)| l.cols[lc][i]).collect();
            set.contains(&key)
        } else {
            (0..r.len()).any(|j| {
                compatible(&l, r, i, j, &lay.shared)
                    && lay
                        .shared
                        .iter()
                        .any(|&(lc, rc)| !l.cols[lc][i].is_undef() && !r.cols[rc][j].is_undef())
            })
        };
        *k = !removed;
    }
    let sorted = l.sorted.clone();
    l.filter_rows(&keep);
    l.sorted = sorted;
    Ok(l)
}

/// The rows of `l` whose id in column `lc` is not in column `rc` of `r` (neither column
/// holds UNDEF, so compatibility is equality): a galloping merge when both sides are
/// sorted on the key, else a probe of the right side's ids. Also names the method.
fn anti_join(
    ctx: &Ctx,
    l: &Table,
    lc: usize,
    r: &Table,
    rc: usize,
) -> Result<(Vec<bool>, &'static str)> {
    let (a, b) = (&l.cols[lc], &r.cols[rc]);
    let lsorted = l.sorted.first().is_some_and(|v| l.col_of(*v) == Some(lc));
    let rsorted = r.sorted.first().is_some_and(|v| r.col_of(*v) == Some(rc));
    if lsorted && rsorted {
        // a left row is kept when the first right id not less than its own differs
        let first = super::zipper::lower_bounds(ctx, a, b)?;
        let keep = a
            .iter()
            .zip(&first)
            .map(|(&x, &j)| b.get(j as usize) != Some(&x))
            .collect();
        return Ok((keep, "merge"));
    }
    let _held = ctx.charge((b.len() * 16) as u64)?;
    let set: FxHashSet<Id> = b.iter().copied().collect();
    ctx.check()?;
    Ok((
        a.par_iter()
            .with_min_len(PAR_MIN_LEN)
            .map(|id| !set.contains(id))
            .collect(),
        "hash",
    ))
}

// ------------------------------------------------------------ expressions ------

/// Decode the base-vocabulary ids of the columns the expressions read into row-aligned
/// value columns: rows are argsorted by id so every distinct term is decoded once and
/// each front-coded block is touched once (in parallel); per-row lookups are then O(1)
/// and lock-free.
fn decode_for(ctx: &Ctx, t: &Table, exprs: &[&Expr]) -> Option<super::expr::DecodedCols> {
    if t.len() < 2 || !exprs.iter().any(|e| super::expr::needs_values(e)) {
        return None;
    }
    let mut vars = Vec::new();
    for e in exprs {
        e.vars(&mut vars);
    }
    vars.sort_unstable();
    vars.dedup();
    if t.len() < 4096 {
        // The rows are evaluated one at a time, each reading the terms it needs. On a
        // cold vocabulary every term is a page fault that waits for the one before it.
        // Ask for all their blocks first, so that the reads overlap.
        if ctx.opt.prefetch_terms && crate::index::io_hints() {
            prefetch_column_terms(ctx, t, &vars);
        }
        return None;
    }
    if ctx.is_cursor() {
        return None;
    }
    let vocab = &ctx.snap.generation.vocab;
    let mut out: super::expr::DecodedCols = vec![None; t.width()];
    for v in vars {
        let Some(c) = t.col_of(v) else { continue };
        let col = &t.cols[c];
        let mut idx: Vec<u32> = (0..col.len() as u32)
            .filter(|&i| col[i as usize].tag() == crate::id::Tag::Vocab)
            .collect();
        if idx.len() < 4096 {
            continue;
        }
        idx.par_sort_unstable_by_key(|&i| col[i as usize]);
        let mut uniq: Vec<u64> = idx.iter().map(|&i| col[i as usize].payload()).collect();
        uniq.dedup();
        let decoded = decode_sorted(vocab, &uniq);
        if decoded.len() != uniq.len() {
            continue;
        }
        let mut vals: Vec<Option<Value>> = vec![None; col.len()];
        if decoded.len() == idx.len() {
            // every row holds a different term: move the values instead of cloning
            for (&i, v) in idx.iter().zip(decoded) {
                vals[i as usize] = Some(v);
            }
        } else {
            let mut j = 0;
            for &i in &idx {
                let p = col[i as usize].payload();
                while uniq[j] != p {
                    j += 1;
                }
                vals[i as usize] = Some(decoded[j].clone());
            }
        }
        out[c] = Some(vals);
    }
    Some(out)
}

/// Ask for the vocabulary pages of the terms in the columns of `vars` of a small table
/// (see [`decode_for`]).
fn prefetch_column_terms(ctx: &Ctx, t: &Table, vars: &[VarId]) {
    let mut ids: Vec<u64> = vars
        .iter()
        .filter_map(|&v| t.col_of(v))
        .flat_map(|c| t.cols[c].iter())
        .filter(|id| id.tag() == crate::id::Tag::Vocab)
        .map(|id| id.payload())
        .collect();
    ids.sort_unstable();
    ids.dedup();
    if ids.len() >= 2 {
        ctx.snap.generation.vocab.prefetch_value_pages(&ids);
    }
}

/// The values of base vocabulary ids (sorted, distinct), in their order: numbers from
/// the numeric column when the vocabulary has one, the others decoded from their keys,
/// each front-coded block touched once (in parallel). Fewer values than ids when a key
/// is missing.
fn decode_sorted(vocab: &crate::vocab::Vocab, uniq: &[u64]) -> Vec<Value> {
    let decode = |ids: &[u64]| -> Vec<Value> {
        ids.par_chunks(4096)
            .flat_map_iter(|chunk| {
                let mut part = Vec::with_capacity(chunk.len());
                vocab.get_sorted(chunk, |_, k| part.push(Value::from_key(k)));
                part
            })
            .collect()
    };
    let Some(num) = vocab.numeric() else {
        return decode(uniq);
    };
    if !uniq.iter().any(|&id| num.may_hold(id)) {
        return decode(uniq);
    }
    if (2..=1 << 16).contains(&uniq.len()) {
        // the keys of the ids outside the column's segments are read meanwhile
        let keys: Vec<u64> = uniq
            .iter()
            .copied()
            .filter(|&id| !num.may_hold(id))
            .collect();
        vocab.prefetch_sorted(&keys);
    }
    let from_column: Vec<Option<Value>> = uniq
        .par_chunks(4096)
        .flat_map_iter(|chunk| num.get_many(chunk).into_iter().map(|n| n.value()))
        .collect();
    let rest: Vec<u64> = uniq
        .iter()
        .zip(&from_column)
        .filter(|(_, v)| v.is_none())
        .map(|(&p, _)| p)
        .collect();
    if rest.len() == uniq.len() {
        return decode(uniq);
    }
    let mut rest = decode(&rest).into_iter();
    let mut out = Vec::with_capacity(uniq.len());
    for v in from_column {
        match v {
            Some(v) => out.push(v),
            None => match rest.next() {
                Some(v) => out.push(v),
                // a missing key: the caller sees fewer values than ids
                None => break,
            },
        }
    }
    out
}

/// Keep the rows that pass every conjunct: EXISTS conjuncts answered from a key set
/// first, then conjuncts that are pure over one variable once per distinct value (see
/// [`super::exprcache`]), the others row by row on the rows that are left.
pub(super) fn apply_filter(ctx: &Ctx, t: &mut Table, exprs: &[Expr]) -> Result<ExprReport> {
    let mut report = ExprReport::default();
    let rest = super::exists::apply(ctx, t, exprs)?;
    let exprs = rest.as_deref().unwrap_or(exprs);
    if exprs.is_empty() {
        return Ok(report);
    }
    let sorted = t.sorted.clone();
    match super::exprcache::filter(ctx, t, exprs, &mut report)? {
        Some((keep, rest)) => {
            t.filter_rows(&keep);
            if !rest.is_empty() {
                let keep = filter_mask(ctx, t, &rest)?;
                t.filter_rows(&keep);
            }
        }
        None => {
            let keep = filter_mask(ctx, t, exprs)?;
            t.filter_rows(&keep);
        }
    }
    t.sorted = sorted;
    Ok(report)
}

/// Evaluate the filter row by row.
pub(super) fn filter_mask(ctx: &Ctx, t: &Table, exprs: &[Expr]) -> Result<Vec<bool>> {
    if let [Expr::Cmp(a, b, op)] = exprs
        && let (Expr::Var(x), Expr::Var(y)) = (&**a, &**b)
        && let Some(keep) = string_order_mask(ctx, t, (*x, *y), *op, &exprs[0])?
    {
        return Ok(keep);
    }
    let t0 = Instant::now();
    let dec = decode_for(ctx, t, &exprs.iter().collect::<Vec<_>>());
    tracing::debug!(target: "sparkles::sparql::exec", "decoded values in {:?}", t0.elapsed());
    let t0 = Instant::now();
    let map = t.var_map(ctx.nvars());
    let test = |i: usize| {
        let row = Row {
            table: t,
            i,
            map: &map,
            dec: dec.as_ref(),
        };
        exprs.iter().all(|e| ebv(e, &row, ctx).unwrap_or(false))
    };
    let par = t.len() > PAR_THRESHOLD && exprs.iter().all(Expr::parallel);
    let keep = map_rows(ctx, t.len(), par, test)?;
    tracing::debug!(target: "sparkles::sparql::exec", "filter evaluated {} rows in {:?}", t.len(), t0.elapsed());
    Ok(keep)
}

/// The rows from which [`string_order_mask`] replaces decoding both columns.
const STRING_ORDER_ROWS: usize = 4096;

/// The rows whose values the first test of [`string_order_mask`] reads.
const STRING_ORDER_SAMPLE: usize = 64;

/// The most ids a worker's cache of string floors holds in [`string_order_mask`].
const STRING_FLOOR_ENTRIES: usize = 1 << 20;

/// `FILTER(?x < ?y)`, or `<=`, `>` or `>=`, over a table whose values of both variables
/// are stored simple literals. Their order is read from their base-vocabulary ids and
/// each string's floor ([`Ctx::string_floor`]), computed once per distinct id in a cache
/// of each worker, so neither column is decoded. A row with another kind of value is
/// evaluated as the generic filter would. `None` when the table is small or its first
/// rows hold other values, so that the generic filter decodes the columns instead, and
/// in a cursor.
fn string_order_mask(
    ctx: &Ctx,
    t: &Table,
    (x, y): (VarId, VarId),
    op: super::expr::CmpOp,
    expr: &Expr,
) -> Result<Option<Vec<bool>>> {
    use super::expr::CmpOp;
    let n = t.len();
    let (Some(cx), Some(cy)) = (t.col_of(x), t.col_of(y)) else {
        return Ok(None);
    };
    // A cursor's batch would rebuild the cache for each batch. Its rows compare their
    // two keys directly instead (see `val_cmp`).
    if n < STRING_ORDER_ROWS || ctx.is_cursor() {
        return Ok(None);
    }
    let (xs, ys) = (&t.cols[cx], &t.cols[cy]);
    let sample = n.min(STRING_ORDER_SAMPLE);
    if !(0..sample).all(|i| ctx.string_floor(xs[i]).is_some() && ctx.string_floor(ys[i]).is_some())
    {
        return Ok(None);
    }
    let threads = rayon::current_num_threads().max(1);
    let par = n > PAR_THRESHOLD && threads > 1;
    let workers = if par { threads } else { 1 };
    // Each worker's cache holds at most `entries` ids and starts again when it is full.
    // A table of 16-byte entries with their control bytes, at most 7/8 full, coexists
    // with its old table while it grows.
    let entries = n.saturating_mul(2).min(STRING_FLOOR_ENTRIES);
    let table = ((entries.saturating_mul(8) / 7).next_power_of_two() as u64).saturating_mul(17);
    let Ok(_cache) = ctx.charge(table.saturating_mul(3) / 2 * workers as u64) else {
        return Ok(None);
    };
    let map = t.var_map(ctx.nvars());
    let generic = |i: usize| {
        let row = Row {
            table: t,
            i,
            map: &map,
            dec: None,
        };
        ebv(expr, &row, ctx).unwrap_or(false)
    };
    let floor = |cache: &mut FxHashMap<Id, u64>, id: Id| -> Option<u64> {
        if cache.len() >= entries && !cache.contains_key(&id) {
            cache.clear();
        }
        let f = *cache
            .entry(id)
            .or_insert_with(|| ctx.string_floor(id).unwrap_or(u64::MAX));
        (f != u64::MAX).then_some(f)
    };
    let test = |cache: &mut FxHashMap<Id, u64>, i: usize| -> Result<bool> {
        if i.is_multiple_of(1024) {
            ctx.check()?;
        }
        let (a, b) = (xs[i], ys[i]);
        let (Some(fa), Some(fb)) = (floor(cache, a), floor(cache, b)) else {
            return Ok(generic(i));
        };
        let (pa, pb) = (a.payload(), b.payload());
        let order = match pa.cmp(&pb) {
            Ordering::Equal => Ordering::Equal,
            Ordering::Less if pa < fb => Ordering::Less,
            Ordering::Less => Ordering::Greater,
            Ordering::Greater if pb < fa => Ordering::Greater,
            Ordering::Greater => Ordering::Less,
        };
        Ok(match op {
            CmpOp::Lt => order == Ordering::Less,
            CmpOp::Le => order != Ordering::Greater,
            CmpOp::Gt => order == Ordering::Greater,
            CmpOp::Ge => order != Ordering::Less,
        })
    };
    let keep = if par {
        (0..n)
            .into_par_iter()
            .with_min_len(n.div_ceil(workers))
            .map_init(FxHashMap::default, |cache, i| test(cache, i))
            .collect::<Result<Vec<bool>>>()?
    } else {
        let mut cache = FxHashMap::default();
        (0..n)
            .map(|i| test(&mut cache, i))
            .collect::<Result<Vec<bool>>>()?
    };
    Ok(Some(keep))
}

/// BIND: the value of `e` on every row (unbound on an error), once per distinct input
/// value when the expression is pure over one variable.
pub(super) fn compute_column(
    ctx: &Ctx,
    t: &Table,
    e: &Expr,
    report: &mut ExprReport,
) -> Result<Vec<Id>> {
    if ctx.is_cursor() {
        return match super::exprcache::cursor_column(ctx, t, e, report, |v| column_rows(ctx, v, e))?
        {
            Some(ids) => Ok(ids),
            None => column_rows(ctx, t, e),
        };
    }
    if t.len() >= super::exprcache::MIN_ROWS
        && super::exprcache::eligible(&[e]).is_ok()
        && let Some(p) =
            super::exprcache::per_value(ctx, t, &[e], false, report, |v| column_rows(ctx, v, e))?
    {
        return Ok(p.rows(t.len()));
    }
    column_rows(ctx, t, e)
}

fn column_rows(ctx: &Ctx, t: &Table, e: &Expr) -> Result<Vec<Id>> {
    let dec = decode_for(ctx, t, &[e]);
    let map = t.var_map(ctx.nvars());
    let f = |i: usize| match eval(
        e,
        &Row {
            table: t,
            i,
            map: &map,
            dec: dec.as_ref(),
        },
        ctx,
    ) {
        Ok(v) => v.into_id(ctx),
        Err(_) => Id::UNDEF,
    };
    map_rows(ctx, t.len(), t.len() > PAR_THRESHOLD && e.parallel(), f)
}

/// Candidate rows for `ORDER BY ?v LIMIT k` when every `?v` is a number other than NaN:
/// a rounded `f64` per row is cheap, and rows whose rounded key is worse than the k-th
/// best rounded key have at least k rows strictly ahead of them in the exact order (the
/// rounding is monotone and numbers order by value first), so they cannot be among the
/// first k. `None` when the shortcut does not apply.
fn topk_candidates(ctx: &Ctx, t: &Table, keys: &[(Expr, bool)], k: usize) -> Option<Vec<usize>> {
    let [(Expr::Var(v), asc)] = keys else {
        return None;
    };
    if k == 0 || k.saturating_mul(8) > t.len() {
        return None;
    }
    let col = &t.cols[t.col_of(*v)?];
    let approx = |id: Id| -> Option<f64> {
        use crate::id::Tag;
        match id.tag() {
            Tag::Int => Some(id.as_i64() as f64),
            Tag::Double => Some(id.as_f64()).filter(|d| !d.is_nan()),
            Tag::Decimal => Some(super::value::decimal_f64(crate::id::unpack_decimal(
                id.payload(),
            ))),
            Tag::Vocab | Tag::Delta => super::value::approx_f64(&ctx.value(id)?),
            _ => None,
        }
    };
    // ascending order ⇔ smallest first; negate so that "best" is always the largest
    let sign = if *asc { -1.0 } else { 1.0 };
    let f: Vec<f64> = col
        .par_iter()
        .with_min_len(PAR_MIN_LEN)
        .map(|&id| approx(id).map(|d| d * sign))
        .collect::<Option<Vec<f64>>>()?;
    let mut sorted = f.clone();
    let (_, kth, _) = sorted.select_nth_unstable_by(k - 1, |a, b| b.total_cmp(a));
    let tau = *kth;
    Some((0..f.len()).filter(|&i| f[i] >= tau).collect())
}

type CursorCandidates = (Vec<usize>, Option<super::ctx::RetainedCharge>);

fn cursor_topk_candidates(
    ctx: &Ctx,
    t: &Table,
    keys: &[(Expr, bool)],
    k: usize,
    decoded: Option<&super::exprcache::PerValue<Option<Value>>>,
) -> Result<Option<CursorCandidates>> {
    let [(expr @ Expr::Var(var), asc)] = keys else {
        return Ok(None);
    };
    if k == 0 || k.saturating_mul(8) > t.len() {
        return Ok(None);
    }
    let Some(column) = t.col_of(*var) else {
        return Ok(None);
    };
    // Covers the optional numeric vectors, temporary row options, sorting copy
    // and selected row indices. Declining this optimization keeps exact sorting.
    let Ok(charge) = ctx.retained_charge(t.len() as u64 * 64 + 4096) else {
        return Ok(None);
    };
    // the dictionary values a caller decoded already, or this table's
    let owned;
    let values = match decoded {
        Some(values) => Some(values),
        None => {
            owned = super::exprcache::cursor_dictionary_values(ctx, t, expr)?;
            owned.as_ref()
        }
    };
    let column = &t.cols[column];
    let sign = if *asc { -1.0 } else { 1.0 };
    let approximate = |i: usize| {
        use crate::id::Tag;
        let id = column[i];
        let number = match id.tag() {
            Tag::Int => Some(id.as_i64() as f64),
            Tag::Double => Some(id.as_f64()).filter(|v| !v.is_nan()),
            Tag::Vocab | Tag::Delta if values.is_some() => values
                .unwrap()
                .get(i)
                .as_ref()
                .and_then(super::value::approx_f64),
            Tag::Decimal => Some(super::value::decimal_f64(crate::id::unpack_decimal(
                id.payload(),
            ))),
            Tag::Vocab | Tag::Delta => ctx.value(id).as_ref().and_then(super::value::approx_f64),
            _ => None,
        };
        number.map(|v| v * sign)
    };
    let approximate = if t.len() > PAR_THRESHOLD {
        (0..t.len())
            .into_par_iter()
            .with_min_len(PAR_MIN_LEN)
            .map(|i| {
                if i.is_multiple_of(1024) {
                    ctx.check()?;
                }
                Ok(approximate(i))
            })
            .collect::<Result<Vec<_>>>()?
    } else {
        map_rows(ctx, t.len(), false, approximate)?
    };
    let Some(numbers) = approximate.into_iter().collect::<Option<Vec<f64>>>() else {
        return Ok(None);
    };
    ctx.check()?;
    let mut sorted = numbers.clone();
    let (_, kth, _) = sorted.select_nth_unstable_by(k - 1, |a, b| b.total_cmp(a));
    let threshold = *kth;
    let mut selected = Vec::new();
    for (i, number) in numbers.into_iter().enumerate() {
        if i.is_multiple_of(1024) {
            ctx.check()?;
        }
        if number >= threshold {
            selected.push(i);
        }
    }
    Ok(Some((selected, charge)))
}

/// ORDER BY (with an optional LIMIT); also returns how many rows the numeric top-k
/// prefilter kept, if it ran.
fn order_by(
    ctx: &Ctx,
    mut t: Table,
    keys: &[(Expr, bool)],
    limit: Option<usize>,
    report: &mut ExprReport,
) -> Result<(Table, Option<String>)> {
    // Small sorts do not use decode_for's bulk value columns. Their dictionary
    // keys can still span hundreds of cold pages (e.g. country populations).
    if (256..4096).contains(&t.len()) && crate::index::io_hints() {
        let mut cols: Vec<usize> = keys
            .iter()
            .filter_map(|(e, _)| match e {
                Expr::Var(v) => t.col_of(*v),
                _ => None,
            })
            .collect();
        cols.sort_unstable();
        cols.dedup();
        let mut ids: Vec<u64> = cols
            .into_iter()
            .flat_map(|c| t.cols[c].iter())
            .filter(|id| id.tag() == crate::id::Tag::Vocab)
            .map(|id| id.payload())
            .collect();
        ids.sort_unstable();
        ids.dedup();
        ctx.snap.generation.vocab.prefetch_values(&ids);
    }
    let mut notes = Vec::new();
    let mut cursor_candidate_charge = None;
    if let Some(k) = limit
        && ctx.opt.topk_prefilter
        && let Some(cand) = if ctx.is_cursor() {
            cursor_topk_candidates(ctx, &t, keys, k, None)?.map(|(rows, charge)| {
                cursor_candidate_charge = charge;
                rows
            })
        } else {
            topk_candidates(ctx, &t, keys, k)
        }
        && cand.len() < t.len()
    {
        // the candidates keep their relative order, so ties break as in the full sort
        notes.push(format!("[numeric prefilter kept {} rows]", cand.len()));
        ctx.check_output(cand.len(), t.width())?;
        t = t.take_rows(&cand);
    }
    drop(cursor_candidate_charge);
    if let Some(k) = limit
        && ctx.opt.topk_first_key
        && keys.len() > 1
        && k > 0
        && k.saturating_mul(8) <= t.len()
        && let Some(cand) = first_key_candidates(ctx, &t, keys, k, report)?
    {
        // As above, the later keys and the row order break the ties among the
        // candidates. This runs before the heap: ranking the first key's distinct values
        // and selecting among the rows' ranks is far cheaper on a large input than a
        // sort key per row, which made DBpedia's top-linked (tens of millions of groups)
        // about 50% slower.
        notes.push(format!("[first-key prefilter kept {} rows]", cand.len()));
        ctx.check_output(cand.len(), t.width())?;
        t = t.take_rows(&cand);
    }
    // Both prefilters keep the candidates in input order and drop only rows with k rows
    // strictly ahead, so the heap over the candidates keeps the same rows.
    if let Some(k) = limit
        && k > 0
        && k < t.len()
        && heap_eligible(ctx)
    {
        let n = t.len();
        let mut heap = TopK::new(k, keys.iter().map(|(_, asc)| *asc).collect());
        let ranked = offer_rows(ctx, &mut heap, &t, keys, report, &mut RowIndex)?;
        ctx.check()?;
        let idx: Vec<usize> = heap.into_sorted().into_iter().map(|e| e.payload).collect();
        ctx.check_output(idx.len(), t.width())?;
        notes.push(format!(
            "[top-k heap kept {} of {n} rows, {ranked} ranked on every key]",
            idx.len()
        ));
        return Ok((t.take_rows(&idx), Some(notes.join(" "))));
    }
    let note = (!notes.is_empty()).then(|| notes.join(" "));
    Ok((order_by_rows(ctx, t, keys, limit, report)?, note))
}

/// Whether ORDER BY with LIMIT keeps its rows in a [`TopK`] heap. Extension callbacks
/// in a key would be called for fewer rows than the full sort calls them for, so their
/// queries keep the full sort.
pub(super) fn heap_eligible(ctx: &Ctx) -> bool {
    ctx.opt.topk_heap && !ctx.calls_extensions
}

/// How [`offer_rows`] reads an ORDER BY key of a row.
enum HeapKey<'a> {
    /// A variable's column, or unbound on every row when the input lacks it.
    Var(Option<&'a [Id]>),
    /// The first key, evaluated for the whole input once per distinct value or per row.
    Column(super::exprcache::Column<SortKey>),
    /// A later key, evaluated only when a row's first key lets it enter.
    Expr(&'a Expr),
}

/// The key of a variable's id. Inline numbers need no decoding.
#[inline]
fn id_key(ctx: &Ctx, id: Id) -> SortKey {
    SortKey::new(ctx.value(id))
}

/// Where [`offer_rows`] keeps the rows that may enter a [`TopK`].
pub(super) trait HeapRows<P> {
    /// Keep row `i` of the input for an entry with these keys.
    fn keep(&mut self, i: usize, keys: &[SortKey]) -> Result<P>;
    /// An entry left the heap, or an offered row did not enter.
    fn release(&mut self, entry: Entry<P>) -> Result<()>;
}

/// Eager execution keeps the input table, so an entry is a row index.
struct RowIndex;

impl HeapRows<usize> for RowIndex {
    fn keep(&mut self, i: usize, _: &[SortKey]) -> Result<usize> {
        Ok(i)
    }
    fn release(&mut self, _: Entry<usize>) -> Result<()> {
        Ok(())
    }
}

/// Offer every row of `t` to `heap`, in row order. The first key is read for every row.
/// The later keys are evaluated only for rows whose first key does not already rank
/// them behind every kept row, so later keys that must be decoded or computed cost
/// nothing for most rows. Returns how many rows were evaluated on every key.
pub(super) fn offer_rows<P>(
    ctx: &Ctx,
    heap: &mut TopK<P>,
    t: &Table,
    keys: &[(Expr, bool)],
    report: &mut ExprReport,
    rows: &mut impl HeapRows<P>,
) -> Result<usize> {
    // the first key's column of SortKeys, beyond the values a cursor column charges
    let _first_charge = if ctx.is_cursor() {
        Some(ctx.charge(t.len() as u64 * std::mem::size_of::<SortKey>() as u64 + 128)?)
    } else {
        None
    };
    let mut sources = Vec::with_capacity(keys.len());
    for (i, (e, _)) in keys.iter().enumerate() {
        sources.push(match e {
            // A large first key is decoded in parallel in eager execution, where values
            // come from the shared cache. A cursor has no such cache, and decodes each
            // dictionary value of the batch once, in key order.
            Expr::Var(v) if i == 0 && !ctx.is_cursor() && t.len() > PAR_THRESHOLD => {
                match t.col_of(*v) {
                    Some(c) => {
                        let col = &t.cols[c];
                        HeapKey::Column(super::exprcache::Column::Rows {
                            vals: col
                                .par_iter()
                                .with_min_len(PAR_MIN_LEN)
                                .map(|&id| id_key(ctx, id))
                                .collect(),
                            _charge: None,
                        })
                    }
                    None => HeapKey::Var(None),
                }
            }
            Expr::Var(_) if i == 0 && ctx.is_cursor() => {
                HeapKey::Column(key_column(ctx, t, e, report)?.map(SortKey::new))
            }
            Expr::Var(v) => HeapKey::Var(t.col_of(*v).map(|c| t.cols[c].as_slice())),
            _ if i == 0 => HeapKey::Column(key_column(ctx, t, e, report)?.map(SortKey::new)),
            _ => HeapKey::Expr(e),
        });
    }
    let map = t.var_map(ctx.nvars());
    let key_of = |i: usize, k: usize| -> SortKey {
        match &sources[k] {
            HeapKey::Var(Some(col)) => id_key(ctx, col[i]),
            HeapKey::Var(None) => SortKey::NULL,
            HeapKey::Column(c) => c.get(i).clone(),
            HeapKey::Expr(e) => SortKey::new(
                eval(
                    e,
                    &Row {
                        table: t,
                        i,
                        map: &map,
                        dec: None,
                    },
                    ctx,
                )
                .ok()
                .and_then(|v| match v {
                    Val::Id(id) => ctx.value(id),
                    Val::V(v) | Val::Dec(_, v) => Some(v),
                }),
            ),
        }
    };
    let mut ranked = 0;
    // When the first key holds one result per distinct value and its values are totally
    // ordered, a value the heap screened out is remembered, and its later rows cost a
    // load instead of a comparison of sort keys. A rejected value is worse than the
    // heap's worst row, and in a total order the worst row only improves as rows enter,
    // so the value stays rejected. Dates with and without a timezone, for example, can
    // form a cycle, where a value rejected once ranks ahead of a later worst row, so
    // their rows are screened one by one. The answers are those of screening every row.
    let mut screened: Vec<bool> = match &sources[0] {
        HeapKey::Column(c @ super::exprcache::Column::Values(p))
            if p.vals.len() < t.len() && p.vals.iter().all(SortKey::totally_ordered) =>
        {
            vec![false; c.distinct().unwrap_or(0)]
        }
        _ => Vec::new(),
    };
    for i in 0..t.len() {
        if i.is_multiple_of(4096) {
            ctx.check()?;
        }
        let first = match &sources[0] {
            HeapKey::Column(c) if !screened.is_empty() => {
                let slot = c.index(i);
                if screened[slot] {
                    heap.skip();
                    continue;
                }
                if heap.screen(c.get(i)) == Screen::Reject {
                    screened[slot] = true;
                    heap.skip();
                    continue;
                }
                c.get(i).clone()
            }
            HeapKey::Column(c) => {
                // screen by reference: most rows of a large input leave here
                if heap.screen(c.get(i)) == Screen::Reject {
                    heap.skip();
                    continue;
                }
                c.get(i).clone()
            }
            _ => {
                let first = key_of(i, 0);
                if heap.screen(&first) == Screen::Reject {
                    heap.skip();
                    continue;
                }
                first
            }
        };
        let mut row = Vec::with_capacity(keys.len());
        row.push(first);
        for k in 1..keys.len() {
            row.push(key_of(i, k));
        }
        ranked += 1;
        let payload = rows.keep(i, &row)?;
        if let Some(out) = heap.offer(row, payload) {
            rows.release(out)?;
        }
    }
    Ok(ranked)
}

/// Candidates for `ORDER BY k1 k2 … LIMIT k`: a row whose first key is worse than the
/// first key of the k-th row in the order of the first key alone has at least k rows
/// ahead of it, so only the rows at least as good as that one are ranked on all keys.
/// `None` when that keeps every row.
fn first_key_candidates(
    ctx: &Ctx,
    t: &Table,
    keys: &[(Expr, bool)],
    k: usize,
    report: &mut ExprReport,
) -> Result<Option<Vec<usize>>> {
    let (e, asc) = &keys[0];
    let col = key_column(ctx, t, e, report)?;
    let cmp = |a: &Option<Value>, b: &Option<Value>| {
        let o = order_cmp(a.as_ref(), b.as_ref());
        if *asc { o } else { o.reverse() }
    };
    ctx.check()?;
    // Ranking and selecting need a total order. Dates with and without a timezone, for
    // example, are only partially ordered, and the heap ranks those rows instead.
    let total = match &col {
        super::exprcache::Column::Values(p) => p.vals.iter(),
        super::exprcache::Column::Rows { vals, .. } => vals.iter(),
    }
    .all(|v| super::sortkey::totally_ordered(v.as_ref()));
    if !total {
        return Ok(None);
    }
    let cand: Vec<usize> = match &col {
        super::exprcache::Column::Values(p) => {
            // rank the distinct values once (equal values share a rank), then select
            // among the rows' ranks
            let mut order: Vec<usize> = (0..p.vals.len()).collect();
            order.sort_by(|&a, &b| cmp(&p.vals[a], &p.vals[b]));
            let mut rank = vec![0u32; p.vals.len()];
            for w in 1..order.len() {
                let same = cmp(&p.vals[order[w - 1]], &p.vals[order[w]]) == Ordering::Equal;
                rank[order[w]] = rank[order[w - 1]] + u32::from(!same);
            }
            let rows: Vec<u32> = (0..t.len()).map(|i| rank[p.index(i)]).collect();
            let mut sel = rows.clone();
            let (_, &mut tau, _) = sel.select_nth_unstable(k - 1);
            (0..t.len()).filter(|&i| rows[i] <= tau).collect()
        }
        super::exprcache::Column::Rows { vals: v, .. } => {
            let mut idx: Vec<usize> = (0..t.len()).collect();
            let (_, &mut kth, _) = idx.select_nth_unstable_by(k - 1, |&a, &b| cmp(&v[a], &v[b]));
            (0..t.len())
                .filter(|&i| cmp(&v[i], &v[kth]) != Ordering::Greater)
                .collect()
        }
    };
    Ok((cand.len() < t.len()).then_some(cand))
}

/// The value of an ORDER BY key on each row of `t`, for a merge that compares rows
/// read back from disk. Values the key computes are not interned.
pub(super) fn key_values(ctx: &Ctx, t: &Table, e: &Expr) -> Result<Vec<Option<Value>>> {
    key_rows(ctx, t, e, None)
}

fn key_rows(
    ctx: &Ctx,
    t: &Table,
    e: &Expr,
    dec: Option<&super::expr::DecodedCols>,
) -> Result<Vec<Option<Value>>> {
    let map = t.var_map(ctx.nvars());
    let f = |i: usize| {
        eval(
            e,
            &Row {
                table: t,
                i,
                map: &map,
                dec,
            },
            ctx,
        )
        .ok()
        .and_then(|v| match v {
            Val::Id(id) => ctx.value(id),
            Val::V(v) | Val::Dec(_, v) => Some(v),
        })
    };
    map_rows(ctx, t.len(), t.len() > PAR_THRESHOLD && e.parallel(), f)
}

/// Own decoded ORDER keys until the sort releases them. The current evaluated
/// value is transient expression memory; admit its payload before retaining it
/// in the column, rather than retaining an uncharged whole-answer value array.
fn cursor_key_rows(
    ctx: &Ctx,
    t: &Table,
    e: &Expr,
) -> Result<super::exprcache::Column<Option<Value>>> {
    let _map_charge = ctx.charge(ctx.nvars() as u64 * 16 + 128)?;
    let map = t.var_map(ctx.nvars());
    let mut bytes = t.len() as u64 * std::mem::size_of::<Option<Value>>() as u64 + 128;
    let mut charge = ctx.retained_charge(bytes)?;
    let mut values = Vec::with_capacity(t.len());
    for i in 0..t.len() {
        if i.is_multiple_of(1024) {
            ctx.check()?;
        }
        let value = eval(
            e,
            &Row {
                table: t,
                i,
                map: &map,
                dec: None,
            },
            ctx,
        )
        .ok()
        .and_then(|v| match v {
            Val::Id(id) => ctx.value(id),
            Val::V(v) | Val::Dec(_, v) => Some(v),
        });
        let payload = match &value {
            Some(Value::Iri(s) | Value::BNode(s) | Value::Str(s)) => s.len() as u64 + 64,
            Some(Value::Lang(s, lang) | Value::LangDir(s, lang, _)) => {
                (s.len() + lang.len()) as u64 + 128
            }
            Some(Value::Other { lex, dt }) => (lex.len() + dt.len()) as u64 + 128,
            Some(Value::Triple(triple)) => super::graph_triple_bytes(triple),
            _ => 0,
        };
        bytes = bytes.saturating_add(payload);
        if let Some(charge) = &mut charge {
            charge.resize(bytes)?;
        }
        values.push(value);
    }
    ctx.check()?;
    Ok(super::exprcache::Column::Rows {
        vals: values,
        _charge: charge,
    })
}

/// The value of an ORDER BY key once per distinct input value, where the key is pure
/// over one variable and its values repeat (see [`super::exprcache`]).
fn key_per_value(
    ctx: &Ctx,
    t: &Table,
    e: &Expr,
    report: &mut ExprReport,
) -> Result<Option<super::exprcache::PerValue<Option<Value>>>> {
    if ctx.is_cursor() {
        if let Some(values) = super::exprcache::cursor_numeric_values(ctx, t, e, report, |v| {
            key_rows(ctx, v, e, None)
        })? {
            return Ok(Some(values));
        }
        return super::exprcache::cursor_variable_values(ctx, t, e);
    }
    if t.len() < super::exprcache::MIN_ROWS {
        return Ok(None);
    }
    super::exprcache::per_value(ctx, t, &[e], false, report, |v| {
        key_rows(ctx, v, e, decode_for(ctx, v, &[e]).as_ref())
    })
}

/// The value of an ORDER BY key on every row, per distinct value where it can be.
fn key_column(
    ctx: &Ctx,
    t: &Table,
    e: &Expr,
    report: &mut ExprReport,
) -> Result<super::exprcache::Column<Option<Value>>> {
    Ok(match key_per_value(ctx, t, e, report)? {
        Some(p) => super::exprcache::Column::Values(p),
        None => {
            if ctx.is_cursor() {
                return cursor_key_rows(ctx, t, e);
            }
            let dec = decode_for(ctx, t, &[e]);
            super::exprcache::Column::Rows {
                vals: key_rows(ctx, t, e, dec.as_ref())?,
                _charge: None,
            }
        }
    })
}

fn order_by_rows(
    ctx: &Ctx,
    t: Table,
    keys: &[(Expr, bool)],
    limit: Option<usize>,
    report: &mut ExprReport,
) -> Result<Table> {
    let _cursor_sort_charge = if ctx.is_cursor() {
        Some(
            ctx.charge(
                (t.len() as u64)
                    .saturating_mul(24)
                    .saturating_add(t.mem_bytes().saturating_mul(2))
                    .saturating_add(keys.len() as u64 * 128 + 1024),
            )?,
        )
    } else {
        None
    };
    let mut cached = Vec::with_capacity(keys.len());
    for (e, _) in keys {
        cached.push(key_per_value(ctx, &t, e, report)?);
    }
    // the keys evaluated row by row share one decoding of the columns they read
    let rest: Vec<&Expr> = keys
        .iter()
        .zip(&cached)
        .filter(|(_, c)| c.is_none())
        .map(|((e, _), _)| e)
        .collect();
    let dec = decode_for(ctx, &t, &rest);
    let key_vals: Vec<super::exprcache::Column<Option<Value>>> = keys
        .iter()
        .zip(cached)
        .map(|((e, _), c)| {
            Ok(match c {
                Some(p) => super::exprcache::Column::Values(p),
                None if ctx.is_cursor() => cursor_key_rows(ctx, &t, e)?,
                None => super::exprcache::Column::Rows {
                    vals: key_rows(ctx, &t, e, dec.as_ref())?,
                    _charge: None,
                },
            })
        })
        .collect::<Result<_>>()?;
    ctx.check()?;
    // The rows compare as the heap's SortKey compares them, but classifying the values
    // for each comparison measured about 2% slower on a 102,000-row string sort, so
    // the full sort calls order_cmp directly. A cursor heap that kept every row sorts
    // its rows with the same comparator and parallel merge sort (see `sort_positions`).
    let cmp = |a: &usize, b: &usize| {
        for (k, (_, asc)) in keys.iter().enumerate() {
            let o = order_cmp(key_vals[k].get(*a).as_ref(), key_vals[k].get(*b).as_ref());
            let o = if *asc { o } else { o.reverse() };
            if o != Ordering::Equal {
                return o;
            }
        }
        a.cmp(b)
    };
    let mut idx: Vec<usize> = (0..t.len()).collect();
    match limit {
        Some(k) if k < idx.len() => {
            if k > 0 {
                idx.select_nth_unstable_by(k - 1, cmp);
            }
            idx.truncate(k);
            idx.sort_by(cmp);
        }
        _ => idx.par_sort_by(cmp),
    }
    ctx.check()?;
    ctx.check_output(idx.len(), t.width())?;
    Ok(t.take_rows(&idx))
}

fn distinct(t: Table) -> Table {
    if t.width() == 1 {
        let mut seen = FxHashSet::default();
        let idx: Vec<usize> = (0..t.len())
            .filter(|&i| seen.insert(t.cols[0][i]))
            .collect();
        let sorted = t.sorted.clone();
        let mut out = t.take_rows(&idx);
        out.sorted = sorted;
        return out;
    }
    if t.width() == 0 {
        let mut t = t;
        t.len = t.len.min(1);
        return t;
    }
    let mut seen: FxHashSet<Vec<Id>> = FxHashSet::default();
    let idx: Vec<usize> = (0..t.len()).filter(|&i| seen.insert(t.row(i))).collect();
    let sorted = t.sorted.clone();
    let mut out = t.take_rows(&idx);
    out.sorted = sorted;
    out
}

// ------------------------------------------------------------------ group ------

fn group(
    ctx: &Ctx,
    t: &Table,
    keys: &[VarId],
    aggs: &[(VarId, Agg)],
    report: &mut ExprReport,
) -> Result<Table> {
    if ctx.opt.incremental_group
        && let Some(out) = group_incremental(ctx, t, keys, aggs)?
    {
        return Ok(out);
    }
    let kcols: Vec<Option<usize>> = keys.iter().map(|k| t.col_of(*k)).collect();
    // the keys of each group, in order of first appearance, and each row's group
    let mut order: Vec<Vec<Id>> = Vec::new();
    let mut gid: Vec<u32> = Vec::with_capacity(t.len());
    let key_of = |c: &Option<usize>, i: usize| c.map_or(Id::UNDEF, |c| t.cols[c][i]);
    if let [kc] = kcols.as_slice() {
        let mut ids: FxHashMap<Id, u32> = FxHashMap::default();
        for i in 0..t.len() {
            if i % 65_536 == 0 {
                ctx.check()?;
                ctx.check_output(order.len(), keys.len() + aggs.len())?;
            }
            let k = key_of(kc, i);
            gid.push(*ids.entry(k).or_insert_with(|| {
                order.push(vec![k]);
                (order.len() - 1) as u32
            }));
        }
    } else {
        let mut ids: FxHashMap<Vec<Id>, u32> = FxHashMap::default();
        let mut key = Vec::with_capacity(kcols.len());
        for i in 0..t.len() {
            if i % 65_536 == 0 {
                ctx.check()?;
                ctx.check_output(order.len(), keys.len() + aggs.len())?;
            }
            key.clear();
            key.extend(kcols.iter().map(|c| key_of(c, i)));
            let g = match ids.get(&key) {
                Some(g) => *g,
                None => {
                    order.push(key.clone());
                    ids.insert(key.clone(), (order.len() - 1) as u32);
                    (order.len() - 1) as u32
                }
            };
            gid.push(g);
        }
    }
    if t.is_empty() && keys.is_empty() {
        order.push(Vec::new());
    }
    // the rows of each group, in row order: group g's rows are rows[start[g]..start[g + 1]]
    let mut start = vec![0u32; order.len() + 1];
    for &g in &gid {
        start[g as usize + 1] += 1;
    }
    for g in 0..order.len() {
        start[g + 1] += start[g];
    }
    let mut rows = vec![0u32; gid.len()];
    let mut next = start.clone();
    for (i, &g) in gid.iter().enumerate() {
        rows[next[g as usize] as usize] = i as u32;
        next[g as usize] += 1;
    }
    let mut vars = keys.to_vec();
    vars.extend(aggs.iter().map(|(v, _)| *v));
    let mut out = Table::new(vars);
    let map = t.var_map(ctx.nvars());
    // aggregate arguments that are pure over one variable, once per distinct value
    let mut args = Vec::with_capacity(aggs.len());
    for (_, agg) in aggs {
        args.push(match &agg.expr {
            // FOLD evaluates its expressions itself, in its ORDER BY's order
            Some(e)
                if agg.fold.is_none()
                    && agg.registered.is_none()
                    && t.len() >= super::exprcache::MIN_ROWS
                    && super::exprcache::eligible(&[e]).is_ok() =>
            {
                super::exprcache::per_value(ctx, t, &[e], false, report, |v| {
                    let map = v.var_map(ctx.nvars());
                    let f = |i: usize| {
                        let row = Row {
                            table: v,
                            i,
                            map: &map,
                            dec: None,
                        };
                        eval(e, &row, ctx).ok().map(|x| x.into_id(ctx))
                    };
                    map_rows(ctx, v.len(), v.len() > PAR_THRESHOLD && e.parallel(), f)
                })?
            }
            _ => None,
        });
    }
    for (g, mut row) in order.into_iter().enumerate() {
        ctx.check()?;
        let rows = &rows[start[g] as usize..start[g + 1] as usize];
        for ((_, agg), arg) in aggs.iter().zip(&args) {
            row.push(if let Some(descriptor) = &agg.registered {
                super::registeredagg::aggregate(ctx, t, &map, rows, agg, descriptor)?
            } else {
                aggregate(ctx, t, &map, rows, agg, arg.as_ref())
            });
            if ctx.calls_extensions {
                ctx.check()?;
            }
        }
        out.push_row(&row);
    }
    Ok(out)
}

/// Whether `group` can run incrementally: at most one key variable, and aggregates that
/// are `COUNT(*)` or COUNT / SUM / AVG / MIN / MAX / SAMPLE or one of ARQ's variance and
/// deviation aggregates of an input column, without DISTINCT. The planner uses this to
/// name the operator in EXPLAIN.
pub fn incremental_group_ok(keys: &[VarId], aggs: &[(VarId, Agg)], input: &[VarId]) -> bool {
    keys.len() <= 1
        && keys.iter().all(|k| input.contains(k))
        && aggs.iter().all(|(_, a)| {
            a.registered.is_none()
                && !a.distinct
                && match &a.expr {
                    None => matches!(a.func, AggregateFunction::Count),
                    Some(Expr::Var(v)) => {
                        input.contains(v)
                            && (matches!(
                                a.func,
                                AggregateFunction::Count
                                    | AggregateFunction::Sum
                                    | AggregateFunction::Avg
                                    | AggregateFunction::Min
                                    | AggregateFunction::Max
                                    | AggregateFunction::Sample
                            ) || stat_aggregate(&a.func).is_some())
                    }
                    _ => false,
                }
        })
}

/// The ARQ variance or deviation aggregate a custom aggregate is, if it is one.
pub(super) fn stat_aggregate(func: &AggregateFunction) -> Option<super::aggext::Arq> {
    match func {
        AggregateFunction::Custom(iri) => super::aggext::Arq::of(iri.as_str())
            .filter(|a| matches!(a, super::aggext::Arq::Stat { .. })),
        _ => None,
    }
}

/// Running state of one aggregate of one group (same results as [`aggregate`]).
pub(super) enum AggState {
    /// COUNT(*) / COUNT(?v): rows / bound values
    Count(u64),
    /// SUM / AVG: an exact integer sum while every value is an inline integer, then the
    /// generic numeric sum; `None` once a value was unbound or not numeric
    Sum {
        int: i64,
        generic: Option<Value>,
        ok: bool,
        rows: u64,
    },
    /// MIN / MAX: the best id and its value
    Best(Option<Id>, Option<Value>),
    Sample(Option<Id>),
    /// ARQ's variance and deviation aggregates
    Stat(super::aggext::Arq, super::aggext::StatAcc),
}

impl AggState {
    pub(super) fn new(agg: &Agg) -> AggState {
        if let Some(arq) = stat_aggregate(&agg.func) {
            return AggState::Stat(arq, Default::default());
        }
        match agg.func {
            AggregateFunction::Sum | AggregateFunction::Avg => AggState::Sum {
                int: 0,
                generic: None,
                ok: true,
                rows: 0,
            },
            AggregateFunction::Min | AggregateFunction::Max => AggState::Best(None, None),
            AggregateFunction::Sample => AggState::Sample(None),
            _ => AggState::Count(0),
        }
    }

    /// Add one row; `id` is the aggregated column's value (`None` for COUNT(*)).
    #[inline(always)]
    pub(super) fn add(&mut self, ctx: &Ctx, agg: &Agg, id: Option<Id>) {
        match self {
            AggState::Count(n) => *n += id.is_none_or(|id| !id.is_undef()) as u64,
            AggState::Sum {
                int,
                generic,
                ok,
                rows,
            } => {
                *rows += 1;
                let id = id.unwrap_or(Id::UNDEF);
                if !*ok {
                    return;
                }
                if generic.is_none()
                    && id.tag() == crate::id::Tag::Int
                    && let Some(s) = int.checked_add(id.as_i64())
                {
                    *int = s;
                    return;
                }
                let acc = generic
                    .take()
                    .unwrap_or_else(|| Value::Integer((*int).into()));
                match (!id.is_undef()).then(|| ctx.value(id)).flatten() {
                    Some(v) => match arith(NumOp::Add, &acc, &v) {
                        Ok(v) => *generic = Some(v),
                        Err(_) => *ok = false,
                    },
                    None => *ok = false,
                }
            }
            AggState::Best(best, best_v) => {
                let Some(id) = id.filter(|id| !id.is_undef()) else {
                    return;
                };
                let v = ctx.value(id);
                let better = match best_v {
                    None => best.is_none(),
                    Some(b) => {
                        let o = order_cmp(v.as_ref(), Some(b));
                        if matches!(agg.func, AggregateFunction::Min) {
                            o == Ordering::Less
                        } else {
                            o == Ordering::Greater
                        }
                    }
                };
                if better {
                    *best = Some(id);
                    *best_v = v;
                }
            }
            AggState::Sample(x) => {
                if x.is_none() {
                    *x = id.filter(|id| !id.is_undef());
                }
            }
            AggState::Stat(_, acc) => acc.add(ctx, id),
        }
    }

    pub(super) fn finish(self, ctx: &Ctx, agg: &Agg) -> Id {
        match self {
            AggState::Count(n) => Id::from_i64(n as i64).unwrap_or(Id::UNDEF),
            AggState::Sum {
                int,
                generic,
                ok,
                rows,
            } => {
                if !ok {
                    return Id::UNDEF;
                }
                let sum = generic.unwrap_or_else(|| Value::Integer(int.into()));
                let v = if matches!(agg.func, AggregateFunction::Avg) {
                    if rows == 0 {
                        Some(Value::Integer(0.into()))
                    } else {
                        arith(NumOp::Div, &sum, &Value::Integer((rows as i64).into())).ok()
                    }
                } else {
                    Some(sum)
                };
                v.map_or(Id::UNDEF, |v| ctx.intern_value(&v))
            }
            AggState::Best(best, _) => best.unwrap_or(Id::UNDEF),
            AggState::Sample(x) => x.unwrap_or(Id::UNDEF),
            AggState::Stat(arq, acc) => acc.finish(ctx, arq),
        }
    }
}

/// GROUP BY in one pass: groups are found through a hash map on the single key id and
/// every aggregate keeps a running state, instead of collecting per-group row lists and
/// value vectors. `None` when the query shape is not admitted (see
/// [`incremental_group_ok`]).
fn group_incremental(
    ctx: &Ctx,
    t: &Table,
    keys: &[VarId],
    aggs: &[(VarId, Agg)],
) -> Result<Option<Table>> {
    if !incremental_group_ok(keys, aggs, &t.vars) {
        return Ok(None);
    }
    let kcol = keys.first().and_then(|k| t.col_of(*k));
    let acols: Vec<Option<usize>> = aggs
        .iter()
        .map(|(_, a)| match &a.expr {
            Some(Expr::Var(v)) => t.col_of(*v),
            _ => None,
        })
        .collect();
    let mut index: FxHashMap<Id, u32> = FxHashMap::default();
    let mut order: Vec<Id> = Vec::new();
    let mut states: Vec<AggState> = Vec::new();
    let na = aggs.len();
    if kcol.is_none() {
        // no grouping: one group, also for empty input
        order.push(Id::UNDEF);
        states.extend(aggs.iter().map(|(_, a)| AggState::new(a)));
    }
    for i in 0..t.len() {
        if i % 65_536 == 0 {
            ctx.check()?;
            ctx.check_output(order.len(), keys.len() + aggs.len())?;
        }
        let g = match kcol {
            None => 0,
            Some(c) => {
                let k = t.cols[c][i];
                *index.entry(k).or_insert_with(|| {
                    order.push(k);
                    states.extend(aggs.iter().map(|(_, a)| AggState::new(a)));
                    (order.len() - 1) as u32
                }) as usize
            }
        };
        for (a, ((_, agg), col)) in aggs.iter().zip(&acols).enumerate() {
            states[g * na + a].add(ctx, agg, col.map(|c| t.cols[c][i]));
        }
    }
    let mut vars = keys.to_vec();
    vars.extend(aggs.iter().map(|(v, _)| *v));
    let mut out = Table::new(vars);
    let mut states = states.into_iter();
    let mut row = Vec::with_capacity(1 + na);
    for k in order {
        ctx.check()?;
        row.clear();
        if kcol.is_some() {
            row.push(k);
        }
        for (_, agg) in aggs {
            row.push(states.next().unwrap().finish(ctx, agg));
        }
        out.push_row(&row);
    }
    Ok(Some(out))
}

/// One aggregate of one group; `arg` holds the argument's value per row when it was
/// evaluated per distinct value (`None` in it: an error).
fn aggregate(
    ctx: &Ctx,
    t: &Table,
    map: &[Option<usize>],
    rows: &[u32],
    agg: &Agg,
    arg: Option<&super::exprcache::PerValue<Option<Id>>>,
) -> Id {
    if let (Some(spec), Some(e)) = (&agg.fold, &agg.expr) {
        return fold(ctx, t, map, rows, e, agg.distinct, spec);
    }
    let Some(e) = &agg.expr else {
        // COUNT(*)
        let n = if agg.distinct {
            rows.iter()
                .map(|&i| t.row(i as usize))
                .collect::<FxHashSet<_>>()
                .len()
        } else {
            rows.len()
        };
        return Id::from_i64(n as i64).unwrap_or(Id::UNDEF);
    };
    let mut vals: Vec<Result<Id, ()>> = rows
        .iter()
        .map(|&i| {
            if let Some(a) = arg {
                return a.get(i as usize).ok_or(());
            }
            eval(
                e,
                &Row {
                    table: t,
                    i: i as usize,
                    map,
                    dec: None,
                },
                ctx,
            )
            .map(|v| v.into_id(ctx))
            .map_err(|_| ())
        })
        .collect();
    if agg.distinct {
        let mut seen = FxHashSet::default();
        vals.retain(|v| match v {
            Ok(id) => seen.insert(*id),
            Err(_) => true,
        });
    }
    let values = || {
        vals.iter()
            .filter_map(|v| v.ok())
            .filter_map(|id| ctx.value(id))
    };
    let fold_sum = || -> Option<Value> {
        let mut acc = Value::Integer(0.into());
        for v in &vals {
            let v = ctx.value((*v).ok()?)?;
            acc = arith(NumOp::Add, &acc, &v).ok()?;
        }
        Some(acc)
    };
    let result: Option<Value> = match &agg.func {
        AggregateFunction::Count => {
            return Id::from_i64(vals.iter().filter(|v| v.is_ok()).count() as i64)
                .unwrap_or(Id::UNDEF);
        }
        AggregateFunction::Sum => fold_sum(),
        AggregateFunction::Avg => {
            if vals.is_empty() {
                Some(Value::Integer(0.into()))
            } else {
                fold_sum().and_then(|s| {
                    arith(NumOp::Div, &s, &Value::Integer((vals.len() as i64).into())).ok()
                })
            }
        }
        AggregateFunction::Min | AggregateFunction::Max => {
            let is_min = matches!(agg.func, AggregateFunction::Min);
            let mut best: Option<Id> = None;
            let mut best_v: Option<Value> = None;
            for id in vals.iter().filter_map(|v| v.ok()) {
                let v = ctx.value(id);
                let better = match &best_v {
                    None => best.is_none(),
                    Some(b) => {
                        let o = order_cmp(v.as_ref(), Some(b));
                        if is_min {
                            o == Ordering::Less
                        } else {
                            o == Ordering::Greater
                        }
                    }
                };
                if better {
                    best = Some(id);
                    best_v = v;
                }
            }
            return best.unwrap_or(Id::UNDEF);
        }
        AggregateFunction::Sample => return vals.iter().find_map(|v| v.ok()).unwrap_or(Id::UNDEF),
        AggregateFunction::GroupConcat { separator } => {
            let sep = separator.as_deref().unwrap_or(" ");
            let mut parts = Vec::new();
            for v in values() {
                let Ok(s) = v.lexical() else { return Id::UNDEF };
                parts.push(s.to_string());
            }
            if vals.iter().any(|v| v.is_err()) {
                return Id::UNDEF;
            }
            // SPARQL 1.1: the result is a simple literal (language tags are dropped)
            Some(Value::Str(parts.join(sep).into()))
        }
        AggregateFunction::Custom(iri) => {
            return super::aggext::aggregate(ctx, iri.as_str(), &vals);
        }
    };
    result.map_or(Id::UNDEF, |v| ctx.intern_value(&v))
}

/// ARQ's `FOLD` of one group (`AggFoldList`, `AggFoldMap`): the values of `e` in the
/// group's rows, sorted by the fold's ORDER BY first, as a `cdt:List` literal (an error
/// is a null, and DISTINCT keeps the first of equal terms and one null), or the `e`
/// keys with the `value` values as a `cdt:Map` literal (a row whose key is an error or a
/// blank node is skipped, a later row replaces an earlier one's value).
fn fold(
    ctx: &Ctx,
    t: &Table,
    map: &[Option<usize>],
    rows: &[u32],
    e: &Expr,
    distinct: bool,
    fold: &super::plan::Fold,
) -> Id {
    use super::cdt::{Elem, Map, map_put};
    let row = |i: u32| Row {
        table: t,
        i: i as usize,
        map,
        dec: None,
    };
    let mut order: Vec<u32> = rows.to_vec();
    if !fold.order.is_empty() {
        let keys: Vec<Vec<Option<Value>>> = rows
            .iter()
            .map(|&i| {
                fold.order
                    .iter()
                    .map(|(k, _)| eval(k, &row(i), ctx).ok().and_then(|v| v.value(ctx).ok()))
                    .collect()
            })
            .collect();
        let mut idx: Vec<usize> = (0..rows.len()).collect();
        idx.sort_by(|&a, &b| {
            for (k, (_, asc)) in fold.order.iter().enumerate() {
                let o = order_cmp(keys[a][k].as_ref(), keys[b][k].as_ref());
                let o = if *asc { o } else { o.reverse() };
                if o != Ordering::Equal {
                    return o;
                }
            }
            Ordering::Equal
        });
        order = idx.into_iter().map(|j| rows[j]).collect();
    }
    let term = |x: &Expr, i: u32| {
        eval(x, &row(i), ctx)
            .ok()
            .and_then(|v| ctx.term(v.into_id(ctx)))
    };
    let value = match &fold.value {
        None => {
            let mut list = Vec::with_capacity(order.len());
            let mut seen = FxHashSet::default();
            let mut null = false;
            for i in order {
                match eval(e, &row(i), ctx) {
                    Ok(v) => {
                        let id = v.into_id(ctx);
                        if distinct && !seen.insert(id) {
                            continue;
                        }
                        list.push(ctx.term(id).map_or(Elem::Null, Elem::Term));
                    }
                    Err(_) => {
                        if distinct && null {
                            continue;
                        }
                        null = true;
                        list.push(Elem::Null);
                    }
                }
            }
            super::cdt::list_value(&list)
        }
        Some(v) => {
            let mut m = Map::new();
            for i in order {
                let Some(k @ (oxrdf::Term::NamedNode(_) | oxrdf::Term::Literal(_))) = term(e, i)
                else {
                    continue;
                };
                map_put(&mut m, k, term(v, i).map_or(Elem::Null, Elem::Term));
            }
            super::cdt::map_value(&m)
        }
    };
    ctx.intern_value(&value)
}

// ------------------------------------------------------------------ paths ------

struct Graph<'a> {
    ctx: &'a Ctx,
    spec: &'a PathSpec,
    graph: GraphFilter,
    fwd: Option<&'a FxHashMap<u64, Vec<u64>>>,
    bwd: Option<&'a FxHashMap<u64, Vec<u64>>>,
    /// frontier levels expanded by one sweep over the predicate's index range
    sweeps: &'a std::cell::Cell<usize>,
}

/// A frontier at least `rows / SWEEP_RATIO` long is expanded by one merged pass over the
/// predicate's rows instead of one index seek per node (a seek costs about as much as
/// merging a few hundred rows).
const SWEEP_RATIO: u64 = 512;

/// A walk that has reached this many nodes for each block of its predicate's index rows
/// reads those blocks whole from then on (spec X07 §6.5). A walk that wide meets most of
/// the blocks again, and reading their segments one by one first costs more than one
/// whole read.
const WALK_WHOLE_PER_BLOCK: usize = 2;

impl Graph<'_> {
    /// Have readers of a segmented index read the blocks of a simple path's predicate
    /// whole, once a walk from one node has reached [`WALK_WHOLE_PER_BLOCK`] nodes per
    /// block. Returns whether it did, so the walk asks once.
    fn read_walk_whole(&self, forward: bool, reached: usize) -> bool {
        let Some((p, rev)) = self.spec.simple else {
            return true;
        };
        let perm = if forward != rev { Perm::Pso } else { Perm::Pos };
        let idx = self.ctx.snap.perm(perm);
        if !idx.segmented() {
            return true;
        }
        let (b0, b1) = idx.block_range(&[p]);
        if reached < (b1 - b0).saturating_mul(WALK_WHOLE_PER_BLOCK) {
            return false;
        }
        for b in b0..b1 {
            idx.mark_dense(b);
        }
        true
    }

    /// Neighbours of every node of a sorted, duplicate-free frontier (with duplicates),
    /// for simple predicate paths: per-node seeks for a small frontier, otherwise one
    /// pass over the predicate's index rows between the first and last frontier node,
    /// merged with the frontier.
    fn expand(&self, frontier: &[u64], forward: bool, out: &mut Vec<u64>) -> Result<()> {
        let Some((p, rev)) = self.spec.simple else {
            for &x in frontier {
                out.extend(self.neighbours(x, forward)?);
            }
            return Ok(());
        };
        let perm = if forward != rev { Perm::Pso } else { Perm::Pos };
        let rows = self.ctx.snap.estimate(perm, &[p]);
        if !self.ctx.opt.batched_paths
            || frontier.len() < 64
            || (frontier.len() as u64).saturating_mul(SWEEP_RATIO) < rows
        {
            for &x in frontier {
                out.extend(self.neighbours(x, forward)?);
            }
            return Ok(());
        }
        self.sweeps.set(self.sweeps.get() + 1);
        let gc = perm.col_of(crate::index::G);
        let (first, last) = (frontier[0], frontier[frontier.len() - 1]);
        let mut j = 0;
        self.ctx.snap.scan_between_cols(
            perm,
            [p, first, 0, 0],
            [p, last, u64::MAX, u64::MAX],
            if self.ctx.opt.selective_columns {
                (1 << 1) | (1 << 2) | (1 << gc)
            } else {
                crate::index::ALL_COLS
            },
            |c| {
                match c {
                    Chunk::Block(b, s, e) => {
                        let (keys, vals, gs) =
                            (&b.cols[1][s..e], &b.cols[2][s..e], &b.cols[gc][s..e]);
                        let mut i = 0;
                        while i < keys.len() && j < frontier.len() {
                            let (k, f) = (keys[i], frontier[j]);
                            if k < f {
                                // skip to the next frontier node
                                i += keys[i..].partition_point(|&x| x < f);
                            } else if k > f {
                                j += frontier[j..].partition_point(|&x| x < k);
                            } else {
                                // the run may continue in the next chunk: `j` moves on only
                                // once a larger key is seen
                                while i < keys.len() && keys[i] == f {
                                    if self.graph.accepts(gs[i]) {
                                        out.push(vals[i]);
                                    }
                                    i += 1;
                                }
                            }
                        }
                    }
                    Chunk::Row(k) => {
                        j += frontier[j..].partition_point(|&x| x < k[1]);
                        if j < frontier.len() && frontier[j] == k[1] && self.graph.accepts(k[gc]) {
                            out.push(k[2]);
                        }
                    }
                }
                Ok(j < frontier.len())
            },
        )?;
        self.ctx.check()?;
        Ok(())
    }

    fn neighbours(&self, x: u64, forward: bool) -> Result<Vec<u64>> {
        if let Some((p, rev)) = self.spec.simple {
            let dir = forward != rev;
            // forward: (x p ?o) via PSO; backward: (?s p x) via POS
            let perm = if dir { Perm::Pso } else { Perm::Pos };
            let gc = perm.col_of(crate::index::G);
            let mut out = Vec::new();
            let mask = if self.ctx.opt.selective_columns {
                (1 << 2) | (1 << gc)
            } else {
                crate::index::ALL_COLS
            };
            let (lo, hi) = (pad(&[p, x], 0), pad(&[p, x], u64::MAX));
            self.ctx.snap.scan_between_cols(perm, lo, hi, mask, |c| {
                match c {
                    Chunk::Block(b, s, e) => {
                        for i in s..e {
                            if self.graph.accepts(b.cols[gc][i]) {
                                out.push(b.cols[2][i]);
                            }
                        }
                    }
                    Chunk::Row(k) => {
                        if self.graph.accepts(k[gc]) {
                            out.push(k[2]);
                        }
                    }
                }
                Ok(true)
            })?;
            out.dedup();
            return Ok(out);
        }
        let m = if forward { self.fwd } else { self.bwd };
        Ok(m.and_then(|m| m.get(&x)).cloned().unwrap_or_default())
    }

    /// Nodes reachable from `start` according to min/max length (ARQ ranges: once per
    /// way).
    fn reach(&self, start: u64, forward: bool) -> Result<Vec<u64>> {
        if let Some((lo, hi)) = self.spec.count {
            return self.reach_counted(start, forward, lo, hi);
        }
        let mut out = Vec::new();
        let mut seen = FxHashSet::default();
        // A cursor charges the visited set and the reached nodes as they grow.
        let held = if self.ctx.is_cursor() {
            Some(self.ctx.charge(64)?)
        } else {
            None
        };
        if self.spec.min == 0 {
            out.push(start);
            seen.insert(start);
        }
        let mut frontier = vec![start];
        let mut depth = 0;
        let mut found = Vec::new();
        let mut found_charged = 0usize;
        let mut whole = self.spec.max_one;
        while !frontier.is_empty() {
            self.ctx.check()?;
            depth += 1;
            if !whole {
                whole = self.read_walk_whole(forward, seen.len());
            }
            let mut next = Vec::new();
            frontier.sort_unstable();
            found.clear();
            self.expand(&frontier, forward, &mut found)?;
            for &y in &found {
                if seen.insert(y) {
                    out.push(y);
                    next.push(y);
                }
            }
            if let Some(held) = &held {
                // the set entry, the reached node and the frontier node of each, and
                // the growth of the neighbour buffer
                let grown = found.capacity().saturating_sub(found_charged);
                found_charged = found_charged.max(found.capacity());
                held.add(next.len() as u64 * 48 + grown as u64 * 8)?;
            }
            if self.spec.max_one && depth >= 1 {
                break;
            }
            frontier = next;
            self.ctx.check_output(out.len(), 1)?;
        }
        Ok(out)
    }

    /// The ends of an ARQ path range from `start`, once per way (see
    /// [`PathSpec::count`]): walks of `lo` to `hi` steps, or walks of `lo` steps each
    /// followed by every simple path from its end. From the object end (`!forward`) the
    /// simple paths come first, so the counts agree with the forward evaluation.
    fn reach_counted(
        &self,
        start: u64,
        forward: bool,
        lo: u64,
        hi: Option<u64>,
    ) -> Result<Vec<u64>> {
        let mut out = Vec::new();
        let mut nbrs: FxHashMap<u64, Vec<u64>> = FxHashMap::default();
        let emit = |out: &mut Vec<u64>, x: u64, n: u64| -> Result<()> {
            let n = usize::try_from(n).unwrap_or(usize::MAX);
            self.ctx.check_output(out.len().saturating_add(n), 1)?;
            out.extend(std::iter::repeat_n(x, n));
            Ok(())
        };
        let mut level: FxHashMap<u64, u64> = FxHashMap::default();
        match hi {
            Some(hi) => {
                level.insert(start, 1);
                for k in 0..=hi {
                    if k >= lo {
                        let mut ends: Vec<(u64, u64)> =
                            level.iter().map(|(x, n)| (*x, *n)).collect();
                        ends.sort_unstable();
                        for (x, n) in ends {
                            emit(&mut out, x, n)?;
                        }
                    }
                    if k == hi {
                        break;
                    }
                    level = self.step(&level, forward, &mut nbrs)?;
                    if level.is_empty() {
                        break;
                    }
                }
            }
            None if forward => {
                level.insert(start, 1);
                for _ in 0..lo {
                    level = self.step(&level, forward, &mut nbrs)?;
                }
                let mut mids: Vec<(u64, u64)> = level.into_iter().collect();
                mids.sort_unstable();
                for (m, n) in mids {
                    self.simple_paths(m, forward, &mut nbrs, &mut |x| emit(&mut out, x, n))?;
                }
            }
            None => {
                self.simple_paths(start, forward, &mut nbrs, &mut |x| {
                    *level.entry(x).or_default() += 1;
                    Ok(())
                })?;
                for _ in 0..lo {
                    level = self.step(&level, forward, &mut nbrs)?;
                }
                let mut ends: Vec<(u64, u64)> = level.into_iter().collect();
                ends.sort_unstable();
                for (x, n) in ends {
                    emit(&mut out, x, n)?;
                }
            }
        }
        Ok(out)
    }

    /// The neighbours of `x`, with one entry per edge, memoized in `nbrs`.
    fn neighbours_memo<'m>(
        &self,
        x: u64,
        forward: bool,
        nbrs: &'m mut FxHashMap<u64, Vec<u64>>,
    ) -> Result<&'m [u64]> {
        Ok(match nbrs.entry(x) {
            std::collections::hash_map::Entry::Occupied(e) => e.into_mut(),
            std::collections::hash_map::Entry::Vacant(e) => e.insert(self.neighbours(x, forward)?),
        })
    }

    /// One step of walks: the number of walks ending at each node.
    fn step(
        &self,
        level: &FxHashMap<u64, u64>,
        forward: bool,
        nbrs: &mut FxHashMap<u64, Vec<u64>>,
    ) -> Result<FxHashMap<u64, u64>> {
        self.ctx.check()?;
        let mut next: FxHashMap<u64, u64> = FxHashMap::default();
        for (&x, &n) in level {
            for &y in self.neighbours_memo(x, forward, nbrs)? {
                let c = next.entry(y).or_default();
                *c = c.saturating_add(n);
            }
        }
        Ok(next)
    }

    /// Every simple path from `start` (no node twice, the zero-length path included):
    /// `f` is called with the end of each, depth first. The number of simple paths can
    /// be exponential in the size of the graph; the deadline and the row budget bound
    /// the enumeration.
    fn simple_paths(
        &self,
        start: u64,
        forward: bool,
        nbrs: &mut FxHashMap<u64, Vec<u64>>,
        f: &mut dyn FnMut(u64) -> Result<()>,
    ) -> Result<()> {
        f(start)?;
        let mut on_path: FxHashSet<u64> = FxHashSet::default();
        on_path.insert(start);
        // (node, index of its next neighbour)
        let mut stack: Vec<(u64, usize)> = vec![(start, 0)];
        let mut steps = 0u32;
        while let Some(&mut (x, ref mut i)) = stack.last_mut() {
            steps = steps.wrapping_add(1);
            if steps.is_multiple_of(4096) {
                self.ctx.check()?;
            }
            let ns = self.neighbours_memo(x, forward, nbrs)?;
            match ns.get(*i).copied() {
                Some(y) => {
                    *i += 1;
                    if on_path.insert(y) {
                        f(y)?;
                        stack.push((y, 0));
                    }
                }
                None => {
                    on_path.remove(&x);
                    stack.pop();
                }
            }
        }
        Ok(())
    }

    /// Does the term occur as subject or object in the active graph?
    fn is_node(&self, x: u64) -> Result<bool> {
        for perm in [Perm::Spo, Perm::Osp] {
            let gc = perm.col_of(crate::index::G);
            let mut found = false;
            self.ctx.snap.scan(perm, &[x], |c| {
                found = match c {
                    Chunk::Block(b, s, e) => (s..e).any(|i| self.graph.accepts(b.cols[gc][i])),
                    Chunk::Row(k) => self.graph.accepts(k[gc]),
                };
                Ok(!found)
            })?;
            if found {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// All start nodes when both ends are unbound.
    fn all_nodes(&self) -> Result<Vec<u64>> {
        let mut set = FxHashSet::default();
        if self.spec.min > 0 {
            if let Some((p, rev)) = self.spec.simple {
                let perm = if rev { Perm::Pos } else { Perm::Pso };
                let gc = perm.col_of(crate::index::G);
                for k in self.ctx.snap.scan_keys(perm, &[p])? {
                    if self.graph.accepts(k[gc]) {
                        set.insert(k[1]);
                    }
                }
            } else if let Some(f) = self.fwd {
                set.extend(f.keys().copied());
            }
        } else {
            // every subject and object of the active graph
            let gc = Perm::Spo.col_of(crate::index::G);
            for k in self.ctx.snap.scan_keys(Perm::Spo, &[])? {
                if self.graph.accepts(k[gc]) {
                    set.insert(k[S]);
                    set.insert(k[Perm::Spo.col_of(O)]);
                }
            }
            let _ = P;
        }
        let mut v: Vec<u64> = set.into_iter().collect();
        v.sort_unstable();
        Ok(v)
    }
}

/// The variables of a path's own solutions: its variable ends, then its graph
/// variable.
fn path_vars(spec: &PathSpec) -> (Option<VarId>, Option<VarId>, Vec<VarId>) {
    let (sv, ov) = (
        match spec.subj {
            PathEnd::Var(v) => Some(v),
            _ => None,
        },
        match spec.obj {
            PathEnd::Var(v) => Some(v),
            _ => None,
        },
    );
    let mut pvars = Vec::new();
    for v in [sv, ov].into_iter().flatten() {
        if !pvars.contains(&v) {
            pvars.push(v);
        }
    }
    if let Some(g) = spec.graph_var {
        pvars.push(g);
    }
    (sv, ov, pvars)
}

/// The graphs a path is evaluated in, each with the id that its graph variable takes.
fn path_graphs(ctx: &Ctx, spec: &PathSpec) -> Result<Vec<(GraphFilter, Option<Id>)>> {
    Ok(match spec.graph_var {
        None => vec![(spec.graph.clone(), None)],
        Some(_) => {
            let all = ctx.snap.graph_ids()?;
            all.into_iter()
                .filter(|g| spec.graph.accepts(g.0))
                .map(|g| (GraphFilter::One(g.0), Some(g)))
                .collect()
        }
    })
}

fn path(
    ctx: &Ctx,
    spec: &PathSpec,
    bound_from_left: bool,
    mut inputs: Vec<Table>,
    vars: &[VarId],
) -> Result<(Table, usize)> {
    let left = if bound_from_left { inputs.pop() } else { None };
    let edges = inputs.pop();
    let graphs = path_graphs(ctx, spec)?;
    let (sv, ov, pvars) = path_vars(spec);
    let mut out = Table::new(pvars.clone());
    let sweeps = std::cell::Cell::new(0);
    for (gf, gid) in graphs {
        let (fwd, bwd) = match (&edges, spec.edge_vars) {
            (Some(e), Some((a, b))) => {
                let (ac, bc) = (e.col_of(a).unwrap(), e.col_of(b).unwrap());
                let gc = spec.graph_var.and_then(|g| e.col_of(g));
                let mut f: FxHashMap<u64, Vec<u64>> = FxHashMap::default();
                let mut bw: FxHashMap<u64, Vec<u64>> = FxHashMap::default();
                for i in 0..e.len() {
                    if let (Some(gc), Some(g)) = (gc, gid)
                        && e.cols[gc][i] != g
                    {
                        continue;
                    }
                    let (x, y) = (e.cols[ac][i].0, e.cols[bc][i].0);
                    if x == 0 || y == 0 {
                        continue;
                    }
                    f.entry(x).or_default().push(y);
                    bw.entry(y).or_default().push(x);
                }
                (Some(f), Some(bw))
            }
            _ => (None, None),
        };
        let g = Graph {
            ctx,
            spec,
            graph: gf,
            fwd: fwd.as_ref(),
            bwd: bwd.as_ref(),
            sweeps: &sweeps,
        };
        path_graph(&g, gid, left.as_ref(), (sv, ov, &pvars), &mut out)?;
    }
    let t = match left {
        Some(l) => join_tables(ctx, &l, &out, &[], false)?.project(vars),
        None => out.project(vars),
    };
    Ok((t, sweeps.get()))
}

/// Add the solutions of a path in one graph to `out`: from the start nodes of `left`
/// when it has one, otherwise from every node of the graph.
fn path_graph(
    g: &Graph<'_>,
    gid: Option<Id>,
    left: Option<&Table>,
    (sv, ov, pvars): (Option<VarId>, Option<VarId>, &[VarId]),
    out: &mut Table,
) -> Result<()> {
    let (ctx, spec) = (g.ctx, g.spec);
    let push = |s: u64, o: u64, out: &mut Table| {
        let mut row = Vec::with_capacity(pvars.len());
        if let Some(v) = sv {
            let _ = v;
            row.push(Id(s));
        }
        if let Some(v) = ov
            && Some(v) != sv
        {
            row.push(Id(o));
        }
        if sv.is_some() && sv == ov && s != o {
            return;
        }
        if let Some(g) = gid {
            row.push(g);
        }
        out.push_row(&row);
    };
    match (&spec.subj, &spec.obj) {
        (PathEnd::Const(s), PathEnd::Const(o)) => {
            // once, or once per way for a range
            for _ in g.reach(s.0, true)?.into_iter().filter(|y| *y == o.0) {
                push(s.0, o.0, out);
            }
        }
        (PathEnd::Const(s), PathEnd::Var(_)) => {
            for o in g.reach(s.0, true)? {
                push(s.0, o, out);
            }
        }
        (PathEnd::Var(_), PathEnd::Const(o)) => {
            for s in g.reach(o.0, false)? {
                push(s, o.0, out);
            }
        }
        (PathEnd::Var(a), PathEnd::Var(b)) => {
            let starts: Vec<u64> = match left {
                Some(l) => {
                    let (col, forward) = match (l.col_of(*a), l.col_of(*b)) {
                        (Some(c), _) => (c, true),
                        (None, Some(c)) => (c, false),
                        _ => return Err(Error::invalid("path bound variable missing")),
                    };
                    let mut s: Vec<u64> = l.cols[col]
                        .iter()
                        .map(|x| x.0)
                        .filter(|x| *x != 0)
                        .collect();
                    s.sort_unstable();
                    s.dedup();
                    for x in s {
                        // variable–variable paths range over graph nodes only: a
                        // zero-length match needs the start term to occur in the graph
                        let in_graph = spec.min > 0 || g.is_node(x)?;
                        for y in g.reach(x, forward)? {
                            if y == x && spec.min == 0 && !in_graph {
                                continue;
                            }
                            if forward {
                                push(x, y, out)
                            } else {
                                push(y, x, out)
                            }
                        }
                    }
                    return Ok(());
                }
                None => g.all_nodes()?,
            };
            for x in starts {
                for y in g.reach(x, true)? {
                    push(x, y, out);
                }
                ctx.check_output(out.len(), out.width())?;
            }
        }
    }
    Ok(())
}

type Adjacency = FxHashMap<u64, Vec<u64>>;

/// The edge relation of a path over a sequence or alternative, read batch by batch
/// from a cursor over its edge plan, as adjacency lists in both directions. When the
/// edge plan binds the path's graph variable, the edges are kept per graph, the way
/// [`path`] filters them.
#[derive(Default)]
pub(super) struct PathEdges {
    shared: (Adjacency, Adjacency),
    per_graph: FxHashMap<Id, (Adjacency, Adjacency)>,
    keys: usize,
    edges: usize,
}

impl PathEdges {
    /// Estimated bytes once `more` edges are added. Each edge is in both lists, and
    /// each may add a map entry with a list header at both of its ends.
    pub(super) fn bytes_with(&self, more: usize) -> u64 {
        let edges = self.edges.saturating_add(more) as u64;
        let keys = self.keys.saturating_add(more.saturating_mul(2)) as u64;
        edges
            .saturating_mul(32)
            .saturating_add(keys.saturating_mul(80))
    }

    /// Add the edges of one batch of the edge plan.
    pub(super) fn add(&mut self, spec: &PathSpec, e: &Table) -> Result<()> {
        let Some((a, b)) = spec.edge_vars else {
            return Err(Error::invalid("a path without an edge plan"));
        };
        let (Some(ac), Some(bc)) = (e.col_of(a), e.col_of(b)) else {
            return Err(Error::invalid("a path's edge plan does not bind its ends"));
        };
        let gc = spec.graph_var.and_then(|g| e.col_of(g));
        for i in 0..e.len() {
            let (x, y) = (e.cols[ac][i].0, e.cols[bc][i].0);
            if x == 0 || y == 0 {
                continue;
            }
            let (f, bw) = match gc {
                Some(gc) => self.per_graph.entry(e.cols[gc][i]).or_default(),
                None => &mut self.shared,
            };
            let fwd = f.entry(x).or_default();
            self.keys += usize::from(fwd.is_empty());
            fwd.push(y);
            let back = bw.entry(y).or_default();
            self.keys += usize::from(back.is_empty());
            back.push(x);
            self.edges += 1;
        }
        Ok(())
    }

    /// The adjacency lists of the graph that `gid` names, or of every graph.
    fn maps(&self, gid: Option<Id>) -> (&Adjacency, &Adjacency) {
        static EMPTY: std::sync::OnceLock<Adjacency> = std::sync::OnceLock::new();
        match gid.map(|g| self.per_graph.get(&g)) {
            Some(Some((f, b))) => (f, b),
            // The edge plan binds the graph, and none of its edges is in this one.
            Some(None) if !self.per_graph.is_empty() || self.shared.0.is_empty() => {
                let empty = EMPTY.get_or_init(Adjacency::default);
                (empty, empty)
            }
            _ => (&self.shared.0, &self.shared.1),
        }
    }
}

/// A path over its edge relation from the start nodes of one input batch, joined with
/// the batch, as [`path`] evaluates it over its whole input.
pub(super) fn path_batch(
    ctx: &Ctx,
    spec: &PathSpec,
    edges: &PathEdges,
    left: &Table,
    vars: &[VarId],
) -> Result<Table> {
    let (sv, ov, pvars) = path_vars(spec);
    let mut out = Table::new(pvars.clone());
    let sweeps = std::cell::Cell::new(0);
    for (gf, gid) in path_graphs(ctx, spec)? {
        let (fwd, bwd) = edges.maps(gid);
        let g = Graph {
            ctx,
            spec,
            graph: gf,
            fwd: Some(fwd),
            bwd: Some(bwd),
            sweeps: &sweeps,
        };
        path_graph(&g, gid, Some(left), (sv, ov, &pvars), &mut out)?;
    }
    Ok(join_tables(ctx, left, &out, &[], false)?.project(vars))
}

/// A transitive path without inputs, evaluated one start node at a time for cursors.
/// Between calls it keeps the start nodes of the graph it is walking.
pub(super) struct PathWalk {
    spec: PathSpec,
    vars: Vec<VarId>,
    graphs: Vec<(GraphFilter, Option<Id>)>,
    graph: usize,
    starts: Option<Vec<u64>>,
    start: usize,
    sweeps: std::cell::Cell<usize>,
    /// The edge relation of a path over a sequence or alternative.
    edges: Option<PathEdges>,
}

impl PathWalk {
    pub(super) fn new(ctx: &Ctx, spec: &PathSpec, vars: &[VarId]) -> Result<Self> {
        let graphs = path_graphs(ctx, spec)?;
        Ok(Self {
            edges: None,
            spec: spec.clone(),
            vars: vars.to_vec(),
            graphs,
            graph: 0,
            starts: None,
            start: 0,
            sweeps: std::cell::Cell::new(0),
        })
    }

    /// Whether the output is in order of its subject variable: every start node of one
    /// graph, in order.
    pub(super) fn ordered_on(spec: &PathSpec) -> Option<VarId> {
        match (&spec.subj, &spec.obj) {
            (PathEnd::Var(a), PathEnd::Var(b)) if a != b && spec.graph_var.is_none() => Some(*a),
            _ => None,
        }
    }

    /// Walk the edge relation read from the path's edge plan rather than the index.
    pub(super) fn set_edges(&mut self, edges: PathEdges) {
        self.edges = Some(edges);
    }

    /// The start nodes kept for the graph being walked.
    pub(super) fn retained(&self) -> usize {
        self.starts.as_ref().map_or(0, Vec::capacity)
    }

    /// The solutions of the next start node with any, or None when every graph has
    /// been walked. A constant end is walked in one step.
    pub(super) fn next(&mut self, ctx: &Ctx) -> Result<Option<Table>> {
        let spec = &self.spec;
        let (sv, ov) = (
            match spec.subj {
                PathEnd::Var(v) => Some(v),
                _ => None,
            },
            match spec.obj {
                PathEnd::Var(v) => Some(v),
                _ => None,
            },
        );
        let mut pvars = Vec::new();
        for v in [sv, ov].into_iter().flatten() {
            if !pvars.contains(&v) {
                pvars.push(v);
            }
        }
        if let Some(g) = spec.graph_var {
            pvars.push(g);
        }
        loop {
            ctx.check()?;
            let Some((gf, gid)) = self.graphs.get(self.graph).cloned() else {
                return Ok(None);
            };
            let (fwd, bwd) = match &self.edges {
                Some(edges) => {
                    let (f, b) = edges.maps(gid);
                    (Some(f), Some(b))
                }
                None => (None, None),
            };
            let g = Graph {
                ctx,
                spec,
                graph: gf,
                fwd,
                bwd,
                sweeps: &self.sweeps,
            };
            let mut out = Table::new(pvars.clone());
            let push = |s: u64, o: u64, out: &mut Table| {
                if sv.is_some() && sv == ov && s != o {
                    return;
                }
                let mut row = Vec::with_capacity(pvars.len());
                if sv.is_some() {
                    row.push(Id(s));
                }
                if ov.is_some() && ov != sv {
                    row.push(Id(o));
                }
                if let Some(g) = gid {
                    row.push(g);
                }
                out.push_row(&row);
            };
            match (&spec.subj, &spec.obj) {
                (PathEnd::Const(s), PathEnd::Const(o)) => {
                    for _ in g.reach(s.0, true)?.into_iter().filter(|y| *y == o.0) {
                        push(s.0, o.0, &mut out);
                    }
                    self.graph += 1;
                }
                (PathEnd::Const(s), PathEnd::Var(_)) => {
                    for o in g.reach(s.0, true)? {
                        push(s.0, o, &mut out);
                    }
                    self.graph += 1;
                }
                (PathEnd::Var(_), PathEnd::Const(o)) => {
                    for s in g.reach(o.0, false)? {
                        push(s, o.0, &mut out);
                    }
                    self.graph += 1;
                }
                (PathEnd::Var(_), PathEnd::Var(_)) => {
                    if self.starts.is_none() {
                        // As [`Graph::all_nodes`] finds them: the sources of the edge
                        // relation when the path has no zero-length match.
                        self.starts = Some(match g.fwd {
                            Some(f) if spec.min > 0 => {
                                let mut starts: Vec<u64> = f.keys().copied().collect();
                                starts.sort_unstable();
                                starts
                            }
                            _ => walk_starts(ctx, spec, &g.graph)?,
                        });
                        self.start = 0;
                    }
                    let starts = self.starts.as_ref().expect("start nodes");
                    let Some(&x) = starts.get(self.start) else {
                        self.starts = None;
                        self.graph += 1;
                        continue;
                    };
                    self.start += 1;
                    for y in g.reach(x, true)? {
                        push(x, y, &mut out);
                    }
                }
            }
            if !out.is_empty() {
                return Ok(Some(out.project(&self.vars)));
            }
        }
    }
}

/// The start nodes of a path with two variable ends, in order, as
/// [`Graph::all_nodes`] finds them, read block by block under a charge rather than as
/// a list of every key.
fn walk_starts(ctx: &Ctx, spec: &PathSpec, graph: &GraphFilter) -> Result<Vec<u64>> {
    let held = ctx.charge(0)?;
    let mut set: FxHashSet<u64> = FxHashSet::default();
    let mut charged = 0usize;
    let mut grow = |set: &FxHashSet<u64>| -> Result<()> {
        if set.len() >= charged + 1024 {
            // a set entry and its share of a growing table
            held.add((set.len() - charged) as u64 * 48)?;
            charged = set.len();
            ctx.check()?;
        }
        Ok(())
    };
    let (perm, prefix, columns): (Perm, Vec<u64>, Vec<usize>) = if spec.min > 0 {
        let Some((p, rev)) = spec.simple else {
            return Ok(Vec::new());
        };
        let perm = if rev { Perm::Pos } else { Perm::Pso };
        (perm, vec![p], vec![1])
    } else {
        // every subject and object of the active graph
        (Perm::Spo, Vec::new(), vec![S, Perm::Spo.col_of(O)])
    };
    let gc = perm.col_of(crate::index::G);
    ctx.snap.scan(perm, &prefix, |chunk| {
        match chunk {
            Chunk::Block(b, s, e) => {
                for i in s..e {
                    let k = b.key(i);
                    if graph.accepts(k[gc]) {
                        for &c in &columns {
                            set.insert(k[c]);
                        }
                    }
                    grow(&set)?;
                }
            }
            Chunk::Row(k) => {
                if graph.accepts(k[gc]) {
                    for &c in &columns {
                        set.insert(k[c]);
                    }
                }
                grow(&set)?;
            }
        }
        Ok(true)
    })?;
    let mut v: Vec<u64> = set.into_iter().collect();
    v.sort_unstable();
    Ok(v)
}

// ----------------------------------------------------------------- vectors ------

/// Exact top-k vector search (`spk:vectorSearch`).
#[cfg(feature = "geo")]
use crate::geo::exec::{spatial_pf, spatial_scan};
#[cfg(feature = "geo")]
use crate::geo::join::spatial_join;
#[cfg(feature = "geo")]
use crate::geo::knn::spatial_knn;
#[cfg(feature = "geo")]
use crate::geo::rewrite::spatial_relate;

type Counters = serde_json::Map<String, serde_json::Value>;

#[cfg(not(feature = "geo"))]
fn spatial_scan(
    _: &Ctx,
    _: &super::geopf::SpatialScanSpec,
    _: &[VarId],
) -> Result<(Table, Counters)> {
    Err(crate::geo::not_built())
}

#[cfg(not(feature = "geo"))]
fn spatial_pf(
    _: &Ctx,
    _: &super::geopf::SpatialPfSpec,
    _: Option<Table>,
    _: &[VarId],
) -> Result<(Table, Counters)> {
    Err(crate::geo::not_built())
}

#[cfg(not(feature = "geo"))]
fn spatial_join(
    _: &Ctx,
    _: &super::geojoin::SpatialJoinSpec,
    _: Vec<Table>,
    _: &[VarId],
) -> Result<(Table, Counters)> {
    Err(crate::geo::not_built())
}

#[cfg(not(feature = "geo"))]
fn spatial_knn(
    _: &Ctx,
    _: &super::geojoin::SpatialKnnSpec,
    _: &Node,
    _: &[VarId],
) -> Result<(Table, Counters, Vec<PlanInfo>)> {
    Err(crate::geo::not_built())
}

#[cfg(not(feature = "geo"))]
fn spatial_relate(
    _: &Ctx,
    _: &super::georewrite::SpatialRelateSpec,
    _: &[VarId],
) -> Result<(Table, Counters)> {
    Err(crate::geo::not_built())
}

/// The vector of entity `e` under `pred` in the active graph (`None`: it has none).
fn entity_vector(
    ctx: &Ctx,
    spec: &super::plan::VectorSpec,
    pred: Id,
    e: Id,
) -> Result<Option<Vec<f32>>> {
    let mut found: Vec<Id> = Vec::new();
    for k in ctx.snap.scan_keys(Perm::Pso, &[pred.0, e.0])? {
        if spec.graph.accepts(k[3]) && !found.contains(&Id(k[2])) {
            found.push(Id(k[2]));
        }
    }
    match found.as_slice() {
        [] => Ok(None),
        [o] => ctx
            .snap
            .key(*o)
            .and_then(|k| crate::vector::from_key(&k))
            .map(Some)
            .ok_or_else(|| Error::invalid("the entity's vector is not a valid spk:vector")),
        many => Err(Error::invalid(format!(
            "entity {} has {} vectors for the predicate; pass a vector literal",
            ctx.term(e).map_or("?".into(), |t| t.to_string()),
            many.len()
        ))),
    }
}

/// Distinct query vectors a variable query may bind.
const MAX_QUERY_VECTORS: usize = 1000;

/// The IRI of a search's predicate.
fn pred_iri(ctx: &Ctx, pred: Id) -> Option<String> {
    match ctx.snap.term(pred) {
        Some(oxrdf::Term::NamedNode(n)) => Some(n.into_string()),
        _ => None,
    }
}

/// Whether the index of `pred` embeds query text.
fn text_queries(ctx: &Ctx, pred: Id) -> bool {
    pred_iri(ctx, pred).is_some_and(|iri| {
        ctx.snap
            .generation
            .vectors
            .configured()
            .values()
            .any(|c| c.predicate == iri && c.embedding.as_ref().is_some_and(|e| e.query_text))
    })
}

/// The vector of a search's text, from the provider of the predicate's index.
fn text_vector(ctx: &Ctx, pred: Id, text: &str) -> Result<std::sync::Arc<[f32]>> {
    ctx.check()?;
    let iri = pred_iri(ctx, pred)
        .ok_or_else(|| Error::invalid("spk:vectorSearch: the predicate is not an IRI"))?;
    crate::store::embed_query_text(&ctx.snap, &iri, text)
}

/// Top-k vector search (`spk:vectorSearch`). With `input` (a variable query or
/// `candidates:join`), the search runs once per distinct query and joins with the input
/// rows; else it is a leaf.
/// Distinct subjects of a join's left side at most, for which the text search on its
/// right side is restricted to those subjects (a filter of the search).
const TEXT_PUSHDOWN_SUBJECTS: usize = 4096;

/// The distinct values of column `c` of `t` (in first-seen order), or `None` when there
/// are more than `max` or a row leaves the column unbound.
fn distinct_bound(
    t: &Table,
    c: usize,
    rows: &mut dyn Iterator<Item = usize>,
    max: usize,
) -> Option<Vec<Id>> {
    let mut seen: rustc_hash::FxHashSet<Id> = Default::default();
    let mut out = Vec::new();
    for r in rows {
        let id = t.get(r, c);
        if id == Id::UNDEF {
            return None;
        }
        if seen.insert(id) {
            if out.len() == max {
                return None;
            }
            out.push(id);
        }
    }
    Some(out)
}

/// A hash join's right side that is a `text:query` call without a limit or rank, when
/// the left side binds its subject to few distinct values in every row: the search for
/// those subjects only, with its plan entry. The join then drops nothing more than it
/// would have dropped from the unrestricted search.
fn text_pushdown(ctx: &Ctx, l: &Table, right: &Node) -> Result<Option<(Table, PlanInfo)>> {
    let Kind::TextSearch(spec) = &right.kind else {
        return Ok(None);
    };
    if !ctx.opt.text_subject_pushdown || !right.children.is_empty() {
        return Ok(None);
    }
    let Some(sv) = spec.subjects_pushable() else {
        return Ok(None);
    };
    let Some(c) = l.col_of(sv) else {
        return Ok(None);
    };
    let Some(subjects) = distinct_bound(l, c, &mut (0..l.len()), TEXT_PUSHDOWN_SUBJECTS) else {
        return Ok(None);
    };
    let start = Instant::now();
    let t = crate::text::search_in(ctx, spec, &right.vars, Some(&subjects), false)?;
    let mut info = describe(ctx, right);
    info.description = format!(
        "{} [searched the {} subjects of the join's left side]",
        right.desc,
        subjects.len()
    );
    info.actual_rows = t.len() as i64;
    info.time_ms = start.elapsed().as_secs_f64() * 1000.0;
    Ok(Some((t, info)))
}

/// The most distinct query strings a `text:query` call with a variable query runs.
const MAX_TEXT_QUERIES: usize = 1000;

/// A `text:query` call whose query string is a variable that its input binds: one
/// search per distinct value, joined with the input rows of that value. A value that is
/// not a string, or does not parse, matches nothing. Without a limit or rank, the search
/// for a value is restricted to the subjects of its rows when they are few.
fn text_bound(
    ctx: &Ctx,
    spec: &super::plan::TextSpec,
    input: &Table,
    note: &mut Option<String>,
) -> Result<(Table, Counters)> {
    let qv = spec
        .query_var
        .expect("a text search over its group has a query variable");
    let svars = spec.output_vars();
    let mut hvars = svars.clone();
    hvars.push(qv);
    let mut hits = Table::new(hvars);
    let col = input.col_of(qv).ok_or_else(|| {
        Error::invalid("text:query: the query variable is not bound by the rest of the group")
    })?;
    let mut by_value: rustc_hash::FxHashMap<Id, Vec<usize>> = Default::default();
    let mut order: Vec<Id> = Vec::new();
    for r in 0..input.len() {
        let id = input.get(r, col);
        if id == Id::UNDEF {
            continue;
        }
        by_value
            .entry(id)
            .or_insert_with(|| {
                order.push(id);
                Vec::new()
            })
            .push(r);
    }
    if order.len() > MAX_TEXT_QUERIES {
        return Err(Error::BudgetExceeded(crate::Budget {
            kind: crate::BudgetKind::Rows,
            limit: MAX_TEXT_QUERIES as u64,
            requested: order.len() as u64,
        }));
    }
    let subject_col = spec
        .subjects_pushable()
        .filter(|_| ctx.opt.text_subject_pushdown)
        .and_then(|sv| input.col_of(sv));
    let (mut searches, mut restricted) = (0u64, 0u64);
    for id in order {
        ctx.check()?;
        let Some(oxrdf::Term::Literal(l)) = ctx.term(id) else {
            continue;
        };
        let lang = match l.language() {
            Some(t) => Some(t.to_ascii_lowercase()),
            None if l.datatype() == oxrdf::vocab::xsd::STRING => None,
            None => continue,
        };
        let mut one = spec.clone();
        one.query = l.value().to_string();
        one.query_var = None;
        one.lang = spec.lang.clone().or(lang);
        let subjects = subject_col.and_then(|c| {
            let rows = &by_value[&id];
            distinct_bound(input, c, &mut rows.iter().copied(), TEXT_PUSHDOWN_SUBJECTS)
        });
        restricted += subjects.is_some() as u64;
        let mut t = crate::text::search_in(ctx, &one, &svars, subjects.as_deref(), true)?;
        searches += 1;
        t.vars.push(qv);
        t.cols.push(vec![id; t.len()]);
        hits.append(t);
        ctx.check_rows(hits.len())?;
    }
    let t = join_noted(ctx, input, &hits, false, note)?;
    *note = Some(format!(
        "[{searches} searches{}]",
        if restricted > 0 {
            format!(", {restricted} restricted to the subjects of their rows")
        } else {
            String::new()
        }
    ));
    let mut c = Counters::new();
    c.insert("searches".into(), searches.into());
    c.insert("restricted".into(), restricted.into());
    c.insert("hits".into(), (hits.len() as u64).into());
    Ok((t, c))
}

pub(super) fn vector_search(
    ctx: &Ctx,
    spec: &super::plan::VectorSpec,
    input: Option<Table>,
    vars: &[VarId],
) -> Result<(Table, Counters)> {
    use super::plan::VectorQuery;
    use crate::vector;
    let mut t = Table::new(vars.to_vec());
    let mut counters = Counters::new();
    let Some(pred) = spec.pred else {
        return Ok((t, counters));
    };
    let graph = |g: u64| spec.graph.accepts(g);
    // (query, the input rows it serves) per search
    let input_rows = |rows: &mut dyn Iterator<Item = usize>| rows.collect::<Vec<usize>>();
    let mut runs: Vec<(Option<Vec<f32>>, Vec<usize>)> = Vec::new();
    match (&spec.query, &input) {
        (VectorQuery::Vector(v), None) => runs.push((Some(v.to_vec()), Vec::new())),
        (VectorQuery::Text(t), inp) => {
            let v = text_vector(ctx, pred, t)?;
            let rows = match inp {
                Some(inp) => input_rows(&mut (0..inp.len())),
                None => Vec::new(),
            };
            runs.push((Some(v.to_vec()), rows));
        }
        (VectorQuery::Entity(e), None) => {
            runs.push((entity_vector(ctx, spec, pred, *e)?, Vec::new()))
        }
        (VectorQuery::Vector(v), Some(inp)) => {
            runs.push((Some(v.to_vec()), input_rows(&mut (0..inp.len()))))
        }
        (VectorQuery::Entity(e), Some(inp)) => runs.push((
            entity_vector(ctx, spec, pred, *e)?,
            input_rows(&mut (0..inp.len())),
        )),
        (VectorQuery::Var(qv), Some(inp)) => {
            let col = inp
                .col_of(*qv)
                .ok_or_else(|| Error::invalid("spk:vectorSearch: the query variable is unbound"))?;
            let mut by_value: rustc_hash::FxHashMap<Id, Vec<usize>> = Default::default();
            let mut order: Vec<Id> = Vec::new();
            for r in 0..inp.len() {
                let id = inp.get(r, col);
                if id == Id::UNDEF {
                    continue;
                }
                by_value
                    .entry(id)
                    .or_insert_with(|| {
                        order.push(id);
                        Vec::new()
                    })
                    .push(r);
            }
            if order.len() > MAX_QUERY_VECTORS {
                return Err(Error::BudgetExceeded(crate::Budget {
                    kind: crate::BudgetKind::Rows,
                    limit: MAX_QUERY_VECTORS as u64,
                    requested: order.len() as u64,
                }));
            }
            for id in order {
                ctx.check()?;
                let v = match ctx.term(id) {
                    Some(oxrdf::Term::Literal(l))
                        if (l.datatype() == oxrdf::vocab::xsd::STRING
                            || l.datatype() == oxrdf::vocab::rdf::LANG_STRING)
                            && text_queries(ctx, pred) =>
                    {
                        Some(text_vector(ctx, pred, l.value())?.to_vec())
                    }
                    Some(oxrdf::Term::Literal(l)) => {
                        match vector::parse_typed(l.value(), l.datatype().as_str())
                            .and_then(|v| v.ok())
                        {
                            Some(v) => Some(v),
                            // a literal that is not a vector matches nothing
                            None => continue,
                        }
                    }
                    _ => entity_vector(ctx, spec, pred, id)?,
                };
                runs.push((v, by_value.remove(&id).unwrap_or_default()));
            }
        }
        (VectorQuery::Var(_), None) => {
            return Err(Error::invalid(
                "spk:vectorSearch: the query variable is not bound by the rest of the group",
            ));
        }
    }
    let col = |v: Option<VarId>| v.and_then(|v| vars.iter().position(|x| *x == v));
    let cs = match spec.subject {
        PathEnd::Var(v) => col(Some(v)),
        _ => None,
    };
    let (cscore, cvec, cg) = (col(spec.score), col(spec.vector), col(spec.graph_var));
    // where each input column goes
    let in_map: Vec<(usize, usize)> = input
        .as_ref()
        .map(|inp| {
            inp.vars
                .iter()
                .enumerate()
                .filter_map(|(i, v)| vars.iter().position(|x| x == v).map(|o| (i, o)))
                .collect()
        })
        .unwrap_or_default();
    let mut row = vec![Id::UNDEF; vars.len()];
    let mut info_all: Option<vector::SearchInfo> = None;
    let mut searches = 0u64;
    for (query, rows) in runs {
        let Some(query) = query else { continue };
        if input.is_some() && rows.is_empty() {
            continue;
        }
        // candidates:join: the subjects of the input rows
        let subjects: Option<Vec<u64>> = match (&input, spec.candidates, &spec.subject) {
            (Some(inp), true, PathEnd::Var(sv)) => {
                let sv = *sv;
                let c = inp.col_of(sv).expect("planned with the subject bound");
                let mut s: Vec<u64> = rows
                    .iter()
                    .map(|&r| inp.get(r, c).0)
                    .filter(|&x| x != Id::UNDEF.0)
                    .collect();
                s.sort_unstable();
                s.dedup();
                Some(s)
            }
            _ => None,
        };
        let q = vector::Search {
            pred: pred.0,
            query: &query,
            k: spec.k,
            metric: spec.metric,
            graph: &graph,
            dedup: spec.dedup,
            distinct_subject: spec.distinct_subject,
            subjects: subjects.as_deref(),
            mode: spec.mode,
        };
        let (hits, info) = vector::search(&ctx.snap, &q, &|| ctx.check())?;
        searches += 1;
        match &mut info_all {
            None => info_all = Some(info),
            Some(a) => {
                a.scored += info.scored;
                if a.method != info.method {
                    a.method = "mixed";
                }
            }
        }
        let fill = |row: &mut Vec<Id>, h: &vector::Hit| -> bool {
            let mut set = |c: Option<usize>, id: Id| -> bool {
                match c {
                    Some(c) if row[c] != Id::UNDEF && row[c] != id => false,
                    Some(c) => {
                        row[c] = id;
                        true
                    }
                    None => true,
                }
            };
            let score = Id::from_f64(h.score as f64)
                .unwrap_or_else(|| ctx.intern_value(&Value::Double((h.score as f64).into())));
            set(cs, Id(h.s)) && set(cscore, score) && set(cvec, Id(h.o)) && set(cg, Id(h.g))
        };
        match &input {
            None => {
                ctx.check_output(t.len() + hits.len(), vars.len())?;
                for h in &hits {
                    if let PathEnd::Const(s) = spec.subject
                        && s.0 != h.s
                    {
                        continue;
                    }
                    row.fill(Id::UNDEF);
                    if fill(&mut row, h) {
                        t.push_row(&row);
                    }
                }
            }
            Some(inp) => {
                ctx.check_output(
                    t.len() + rows.len() * hits.len().min(rows.len().max(1)),
                    vars.len(),
                )?;
                for &r in &rows {
                    for h in &hits {
                        if let PathEnd::Const(s) = spec.subject
                            && s.0 != h.s
                        {
                            continue;
                        }
                        row.fill(Id::UNDEF);
                        for &(i, o) in &in_map {
                            row[o] = inp.get(r, i);
                        }
                        if fill(&mut row, h) {
                            t.push_row(&row);
                        }
                    }
                    ctx.check_rows(t.len())?;
                }
            }
        }
    }
    if let Some(i) = info_all {
        counters.insert("method".into(), i.method.into());
        if let Some(r) = i.reason {
            counters.insert("exactBecause".into(), r.into());
        }
        if let Some(n) = i.index {
            counters.insert("index".into(), n.into());
        }
        if i.ef > 0 {
            counters.insert("ef".into(), i.ef.into());
        }
        counters.insert("rows".into(), i.rows.into());
        counters.insert("scored".into(), i.scored.into());
        counters.insert("overlayInserts".into(), i.inserted.into());
        counters.insert("overlayPending".into(), i.pending.into());
        counters.insert("overlayDeletes".into(), i.deleted.into());
    }
    if input.is_some() {
        counters.insert("searches".into(), searches.into());
    }
    Ok((t, counters))
}

// ---------------------------------------------------------------- service ------

fn service(
    ctx: &Ctx,
    endpoint: &PathEnd,
    query: &str,
    vars: &[VarId],
    cache: super::enhancer::CacheMode,
) -> Result<Table> {
    use super::enhancer::CacheMode;
    super::enhancer::check_allowed(ctx)?;
    let PathEnd::Const(id) = endpoint else {
        return Err(Error::unsupported("SERVICE with a variable endpoint"));
    };
    let Some(oxrdf::Term::NamedNode(url)) = ctx.term(*id) else {
        return Err(Error::Service("invalid SERVICE endpoint".into()));
    };
    let store = &ctx.snap.results.service;
    // an in-process call reads the current data, so its answer is never cached
    if matches!(cache, CacheMode::Default | CacheMode::Clear)
        && ctx.use_cache
        && store.enabled()
        && !is_local(ctx, url.as_str())
    {
        // the whole result under one key: a SERVICE without `loop` has one input
        let key = super::svccache::key(&ctx.service_scope, url.as_str(), query, &[]);
        let hit = if cache == CacheMode::Default {
            store.get(&key)
        } else {
            store.remove(&key);
            None
        };
        let rows = match hit {
            Some(rows) => rows,
            None => {
                let rows = std::sync::Arc::new(super::enhancer::fetch_rows(ctx, &url, query)?);
                store.put(key, rows.clone());
                rows
            }
        };
        let cols: Vec<Option<usize>> = vars
            .iter()
            .map(|v| {
                let n = ctx.var_name(*v);
                rows.vars.iter().position(|x| *x == n)
            })
            .collect();
        let mut t = Table::new(vars.to_vec());
        let mut bnodes = FxHashMap::default();
        for r in &rows.rows {
            let row: Vec<Id> = cols
                .iter()
                .map(|c| {
                    c.and_then(|c| r[c].as_ref())
                        .map_or(Id::UNDEF, |term| ctx.intern_remote_term(term, &mut bnodes))
                })
                .collect();
            t.push_row(&row);
        }
        return Ok(t);
    }
    let mut t = Table::new(vars.to_vec());
    // the endpoint's blank nodes are its own: new ones here, one per label of the result
    let mut bnodes = FxHashMap::default();
    fetch(ctx, &url, query, &mut |sol| {
        let row: Vec<Id> = vars
            .iter()
            .map(|v| {
                sol.get(ctx.var_name(*v).as_str())
                    .map_or(Id::UNDEF, |term| ctx.intern_remote_term(term, &mut bnodes))
            })
            .collect();
        t.push_row(&row);
        Ok(())
    })?;
    Ok(t)
}

/// Whether `url` names a query endpoint of this process (see [`super::LocalEndpoints`]).
pub(super) fn is_local(ctx: &Ctx, url: &str) -> bool {
    ctx.local_endpoints
        .as_ref()
        .is_some_and(|l| l.resolve(url).is_some())
}

/// Run `query` in process on the dataset `target` names, as the caller, and hand each
/// solution to `on_solution`. The call keeps the caller's deadline, cancellation, grant,
/// row count and outbound budget, so it can neither wait for admission behind its
/// caller nor outlive it.
fn fetch_local(
    ctx: &Ctx,
    target: Result<super::LocalTarget>,
    query: &str,
    on_solution: &mut dyn FnMut(sparesults::QuerySolution) -> Result<()>,
) -> Result<()> {
    if ctx.service_depth >= super::MAX_SERVICE_DEPTH {
        return Err(Error::Service(format!(
            "SERVICE calls to this server nest more than {} deep",
            super::MAX_SERVICE_DEPTH
        )));
    }
    let super::LocalTarget {
        snap,
        options: mut opts,
        hold: _hold,
    } = target?;
    // the call can never be given more than its caller has
    opts.max_rows = Some(opts.max_rows.map_or(ctx.max_rows, |m| m.min(ctx.max_rows)));
    opts.max_memory_bytes = Some(
        opts.max_memory_bytes
            .map_or(ctx.mem_limit, |m| m.min(ctx.mem_limit)),
    );
    opts.max_rows_produced = Some(
        opts.max_rows_produced
            .map_or(ctx.max_rows_produced, |m| m.min(ctx.max_rows_produced)),
    );
    opts.timeout = None;
    opts.deadline = ctx.current_deadline();
    opts.cancel = Some(ctx.cancel.clone());
    opts.cancellation = ctx.cancellation.clone();
    opts.grant = ctx.grant().cloned();
    opts.work = Some(ctx.rows_produced.clone());
    opts.outbound_budget = Some(ctx.outbound_budget.clone());
    opts.service_depth = ctx.service_depth + 1;
    // the call's plan is dropped below, so it names no patterns (item 180)
    opts.no_plan_patterns = true;
    ctx.check()?;
    let r = super::query(snap, query, &opts)?;
    let vars: std::sync::Arc<[oxrdf::Variable]> = r
        .vars
        .iter()
        .map(|v| oxrdf::Variable::new_unchecked(v.clone()))
        .collect();
    for i in 0..r.table.len() {
        if i % 1024 == 0 {
            ctx.check()?;
        }
        let row: Vec<Option<oxrdf::Term>> = r.table.row(i).iter().map(|id| r.term(*id)).collect();
        on_solution(sparesults::QuerySolution::from((vars.clone(), row)))?;
    }
    Ok(())
}

/// Send `query` to the endpoint `url` through the outbound policy and hand each
/// solution of the response to `on_solution` as it is parsed. A query endpoint of this
/// process answers in process instead (see [`fetch_local`]).
pub(super) fn fetch(
    ctx: &Ctx,
    url: &oxrdf::NamedNode,
    query: &str,
    on_solution: &mut dyn FnMut(sparesults::QuerySolution) -> Result<()>,
) -> Result<()> {
    if let Some(target) = ctx
        .local_endpoints
        .as_ref()
        .and_then(|l| l.resolve(url.as_str()))
    {
        return fetch_local(ctx, target, query, on_solution);
    }
    // the host of the endpoint (no user info, no port)
    let host = oxiri::Iri::parse(url.as_str())
        .ok()
        .and_then(|i| {
            let a = i.authority()?;
            let a = a.rsplit('@').next().unwrap_or(a);
            Some(match a.strip_prefix('[') {
                Some(v6) => v6.split(']').next().unwrap_or(v6).to_string(),
                None => a.split(':').next().unwrap_or(a).to_string(),
            })
        })
        .unwrap_or_default();
    // a client span: its context is what the outbound headers hook propagates
    let span = tracing::info_span!(
        target: "sparkles::sparql::exec",
        "sparql.service",
        otel.kind = "client",
        http.request.method = "POST",
        server.address = %host,
        http.response.status_code = tracing::field::Empty,
    );
    let _entered = span.enter();
    // within the query's deadline too
    let timeout = ctx.current_deadline().map_or(ctx.outbound.timeout, |d| {
        d.saturating_duration_since(Instant::now())
    });
    let resp = ctx
        .outbound
        .send(&ctx.outbound_budget, url.as_str(), timeout, |client, u| {
            client
                .post(u)
                .header(
                    "Accept",
                    "application/sparql-results+json, application/sparql-results+xml;q=0.8",
                )
                .form(&[("query", query)])
        })
        .map_err(|f| {
            f.into_error(&format!("SERVICE <{}>", url.as_str()), |m| {
                Error::Service(format!("<{}>: {m}", url.as_str()))
            })
        })?;
    span.record("http.response.status_code", i64::from(resp.status.as_u16()));
    if !resp.status.is_success() {
        return Err(Error::Service(format!(
            "{} returned {}",
            url.as_str(),
            resp.status
        )));
    }
    let (fmt, syntax) = if resp.content_type.contains("xml") {
        (
            sparesults::QueryResultsFormat::Xml,
            crate::nesting::Syntax::Xml,
        )
    } else {
        (
            sparesults::QueryResultsFormat::Json,
            crate::nesting::Syntax::Json,
        )
    };
    // the results parsers read a nested triple term by recursion
    let body = crate::nesting::Guarded::new(
        resp.body,
        syntax,
        crate::nesting::MAX_ELEMENTS,
        &format!("<{}>", url.as_str()),
        Error::Service,
    );
    // parsed as it streams in, under the policy's byte ceiling and deadline
    let parser = sparesults::QueryResultsParser::from_format(fmt);
    let failed = |e: sparesults::QueryResultsParseError| match e {
        // a spent budget, or the body's own words (timeout, size, connection)
        sparesults::QueryResultsParseError::Io(e) => match crate::codec::io_error(e) {
            Error::Io(e) => Error::Service(format!("<{}>: {e}", url.as_str())),
            e => e,
        },
        e => Error::Service(format!("<{}>: {e}", url.as_str())),
    };
    match parser.for_reader(body).map_err(failed)? {
        sparesults::ReaderQueryResultsParserOutput::Solutions(sols) => {
            for sol in sols {
                on_solution(sol.map_err(failed)?)?;
            }
        }
        sparesults::ReaderQueryResultsParserOutput::Boolean(_) => {}
    }
    Ok(())
}

#[cfg(test)]
mod string_cross_tests {
    use super::*;
    use crate::store::{Store, StoreOptions};

    #[test]
    fn candidate_output_copy_checks_its_work_in_bounded_chunks() {
        let store = Store::in_memory(StoreOptions::default());
        let mut ctx = Ctx::new(store.snapshot());
        ctx.max_rows = 10_000;
        ctx.mem_limit = 32 << 20;
        let variables = (0..64)
            .map(|i| ctx.var(&format!("v{i}")))
            .collect::<Vec<_>>();
        let same = ctx.intern_value(&Value::Str("same".into()));
        let other = ctx.intern_value(&Value::Str("other".into()));
        let mut left = Table::new(variables[..32].to_vec());
        left.push_row(&[same; 32]);
        left.push_row(&[other; 32]);
        let mut right = Table::new(variables[32..].to_vec());
        for _ in 0..8_192 {
            right.push_row(&[same; 32]);
        }
        let mut cross = Node::leaf(
            Kind::Join {
                algo: JoinAlgo::Cross,
                keys: Vec::new(),
            },
            variables,
            16_384.0,
            "cross product".into(),
        );
        cross.children = vec![
            Node::leaf(
                Kind::Values(left.clone()),
                left.vars.clone(),
                2.0,
                "left".into(),
            ),
            Node::leaf(
                Kind::Values(right.clone()),
                right.vars.clone(),
                8_192.0,
                "right".into(),
            ),
        ];
        let before = crate::sparql::ctx::CHECKS.with(|n| n.get());
        let table = string_cross(&ctx, &cross, &left, &right, (left.vars[0], right.vars[0]))
            .unwrap()
            .unwrap();
        let checks = crate::sparql::ctx::CHECKS.with(|n| n.get()) - before;
        assert_eq!(table.len(), 8_192);
        assert!(table.cols.iter().all(|c| c.iter().all(|&id| id == same)));
        // Each output column is its own long copy loop. This lower bound only
        // counts its materialization work, independently of proof/index/probes.
        let minimum = table.width() * table.len().div_ceil(1_024);
        assert!(
            checks >= minimum,
            "{checks} checks for {} output cells, need at least {minimum}",
            table.width() * table.len()
        );
        // Isolate copying from sort/proof/probe checkpoints as well.
        drop(table);
        let pairs = (0..right.len()).map(|j| (0, j as u32)).collect::<Vec<_>>();
        let before = crate::sparql::ctx::CHECKS.with(|n| n.get());
        let copied =
            materialize_candidates(&ctx, &left, &right, &layout(&left, &right), &pairs).unwrap();
        let checks = crate::sparql::ctx::CHECKS.with(|n| n.get()) - before;
        assert_eq!(copied.len(), 8_192);
        assert!(copied.cols.iter().all(|c| c.iter().all(|&id| id == same)));
        assert!(
            (minimum..=2 * minimum + 4).contains(&checks),
            "{checks} isolated copy checks"
        );
    }

    #[test]
    fn string_key_domain_matches_values_without_term_unification() {
        use oxrdf::{Literal, NamedNode, Term};
        let terms = [
            Term::Literal(Literal::new_simple_literal("")),
            Term::Literal(Literal::new_simple_literal("quote\"\\\n café🦀")),
            Term::Literal(Literal::new_typed_literal(
                "typed",
                oxrdf::vocab::xsd::STRING,
            )),
            Term::Literal(Literal::new_language_tagged_literal("same", "en").unwrap()),
            Term::Literal(Literal::new_typed_literal("01", oxrdf::vocab::xsd::INTEGER)),
            Term::Literal(Literal::new_typed_literal(
                "bad",
                NamedNode::new("urn:unknown").unwrap(),
            )),
            Term::NamedNode(NamedNode::new("urn:iri").unwrap()),
        ];
        for term in terms {
            let key = crate::id::term_key(&term);
            let expected = match Value::from_key(&key) {
                Value::Str(s) => Some(s),
                _ => None,
            };
            assert_eq!(string_lexical_key(&key), expected.as_deref(), "{term}");
        }
        assert_eq!(
            string_lexical_key(b"\"typed\xff^http://www.w3.org/2001/XMLSchema#string"),
            Some("typed")
        );
        assert_eq!(string_lexical_key(b"\"\xfe\xff"), None);
    }

    #[test]
    fn checked_candidate_sort_matches_total_key_and_row_order() {
        let store = Store::in_memory(StoreOptions::default());
        let ctx = Ctx::new(store.snapshot());
        for len in [0, 1, 2, 3, 31, 257, 8_192] {
            let mut values = (0..len)
                .map(|i| (format!("key:{}", i * 7_919 % 17), i))
                .rev()
                .collect::<Vec<_>>();
            let mut expected = values.clone();
            expected.sort_unstable();
            sort_candidates(&ctx, &mut values).unwrap();
            assert_eq!(values, expected);
        }
    }

    #[test]
    fn candidate_sort_cancels_after_comparisons_have_started() {
        use std::sync::{
            Arc,
            atomic::{AtomicBool, AtomicUsize, Ordering as MemOrder},
        };
        struct Observed {
            value: usize,
            compared: Arc<AtomicUsize>,
            cancel: Arc<AtomicBool>,
        }
        impl PartialEq for Observed {
            fn eq(&self, other: &Self) -> bool {
                self.value == other.value
            }
        }
        impl Eq for Observed {}
        impl PartialOrd for Observed {
            fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
                Some(self.cmp(other))
            }
        }
        impl Ord for Observed {
            fn cmp(&self, other: &Self) -> Ordering {
                let n = self.compared.fetch_add(1, MemOrder::Relaxed) + 1;
                if n == 2_000 {
                    self.cancel.store(true, MemOrder::Relaxed);
                }
                self.value.cmp(&other.value)
            }
        }
        let store = Store::in_memory(StoreOptions::default());
        let ctx = Ctx::new(store.snapshot());
        let compared = Arc::new(AtomicUsize::new(0));
        let mut values = (0..8_192)
            .map(|i| Observed {
                value: i * 7_919 % 8_192,
                compared: compared.clone(),
                cancel: ctx.cancel.clone(),
            })
            .collect::<Vec<_>>();
        assert!(!ctx.cancel.load(MemOrder::Relaxed));
        assert!(matches!(
            sort_candidates(&ctx, &mut values),
            Err(Error::Cancelled)
        ));
        let work = compared.load(MemOrder::Relaxed);
        assert!(
            (2_000..=3_024).contains(&work),
            "{work} comparisons after cancellation at 2,000"
        );
    }

    #[test]
    fn probe_reconstruction_must_admit_live_pair_capacity() {
        use crate::io::Source;
        use oxrdfio::RdfFormat;
        let store = Store::in_memory(StoreOptions::default());
        let huge = "z".repeat(2 << 20);
        store
            .load(&[Source::from_bytes(
                format!("<urn:s> <urn:p> \"a\" ; <urn:q> \"{huge}\" .").into_bytes(),
                RdfFormat::Turtle,
                None,
            )])
            .unwrap();
        let snapshot = store.snapshot();
        let stored = |s: &str| {
            let key = crate::id::term_key(&oxrdf::Literal::new_simple_literal(s).into());
            Id::new(
                crate::id::Tag::Vocab,
                snapshot.generation.vocab.find(&key).unwrap(),
            )
        };
        let same = stored("a");
        let large = stored(&huge);
        let mut left = Table::new(vec![0]);
        left.push_row(&[same]);
        left.push_row(&[large]);
        let mut right = Table::new(vec![1]);
        for _ in 0..65_536 {
            right.push_row(&[same]);
        }
        let mut cross = Node::leaf(
            Kind::Join {
                algo: JoinAlgo::Cross,
                keys: Vec::new(),
            },
            vec![0, 1],
            131_072.0,
            "cross product".into(),
        );
        cross.children = vec![
            Node::leaf(
                Kind::Values(left.clone()),
                left.vars.clone(),
                2.0,
                "left".into(),
            ),
            Node::leaf(
                Kind::Values(right.clone()),
                right.vars.clone(),
                65_536.0,
                "right".into(),
            ),
        ];
        let run = |memory| {
            let mut ctx = Ctx::new(snapshot.clone());
            ctx.max_rows = 100_000;
            ctx.mem_limit = memory;
            let _inputs = ctx.charge(left.mem_bytes() + right.mem_bytes()).unwrap();
            let result = string_cross(&ctx, &cross, &left, &right, (0, 1));
            (result, ctx.mem_peak())
        };
        // The approximately4MiB reconstruction envelope fits alongside inputs
        // and the index. It must also include the already allocated512KiB pairs.
        let (result, peak) = run(25 << 18);
        let observed = result
            .as_ref()
            .map(|t| t.as_ref().map(Table::len))
            .map_err(ToString::to_string);
        assert!(
            matches!(result, Err(Error::BudgetExceeded(b)) if b.kind == crate::error::BudgetKind::Memory),
            "{observed:?}, peak {peak}"
        );
        let (result, peak) = run(8 << 20);
        let table = result.unwrap().unwrap();
        assert_eq!(table.len(), 65_536);
        assert!(table.cols.iter().all(|c| c.iter().all(|&id| id == same)));
        assert!(peak <= 8 << 20);
    }
}

#[cfg(test)]
mod limited_startup_tests {
    use super::*;
    use crate::io::{RdfFormat, Source};
    use crate::store::{Store, StoreOptions};

    fn unary(kind: Kind, child: Node, description: &str) -> Node {
        let mut node = Node::leaf(kind, child.vars.clone(), child.est, description.into());
        node.cost = child.cost;
        node.children.push(child);
        node
    }

    fn run(
        reduced: bool,
        row_limit: usize,
        build_rows: usize,
        duplicate_keys: bool,
        match_row: usize,
        memory: Option<u64>,
    ) -> Result<u32> {
        let store = Store::in_memory(StoreOptions::default());
        let data: String = (0..2000)
            .map(|i| format!("<urn:record:{i:04}> <urn:key> <urn:bucket:0> .\n"))
            .collect();
        store
            .load(&[Source::from_bytes(
                data.into_bytes(),
                RdfFormat::Turtle,
                None,
            )])
            .unwrap();
        let mut ctx = Ctx::new(store.snapshot());
        ctx.use_cache = false;
        ctx.max_rows = row_limit;
        ctx.mem_limit = memory.unwrap_or(u64::MAX);
        let subject = ctx.var("subject");
        let key = ctx.var("key");
        let target = ctx.var("target");
        let term = |text: &str| ctx.intern_term(&oxrdf::NamedNode::new(text).unwrap().into());
        let predicate = term("urn:key");
        let witness = term(&format!("urn:record:{match_row:04}"));
        let scan = Node::leaf(
            Kind::Scan(ScanSpec {
                perm: Perm::Pso,
                prefix: vec![predicate.0],
                cols: vec![(1, subject), (2, key)],
                eqs: Vec::new(),
                graph: GraphFilter::Default,
                graph_col: 3,
                dedup: false,
                src: None,
            }),
            vec![subject, key],
            2000.0,
            "driving scan".into(),
        );
        let mut values = Table::new(vec![key, target]);
        for i in 0..build_rows {
            let bucket = if duplicate_keys { 0 } else { i };
            values.push_row(&[term(&format!("urn:bucket:{bucket}")), witness]);
        }
        let build = Node::leaf(
            Kind::Values(values),
            vec![key, target],
            build_rows as f64,
            "full build".into(),
        );
        let mut joined = Node::leaf(
            Kind::Join {
                algo: JoinAlgo::Hash,
                keys: vec![key],
            },
            vec![subject, key, target],
            2000.0,
            "hash join".into(),
        );
        joined.children = vec![scan, build];
        joined.cost = 6000.0;
        let mut filtered = unary(
            Kind::Filter(vec![Expr::SameTerm(
                Box::new(Expr::Var(subject)),
                Box::new(Expr::Var(target)),
            )]),
            joined,
            "matching both inputs",
        );
        filtered.est = 1.0;
        let mut union = Node::leaf(Kind::Union, filtered.vars.clone(), 1.0, "pure union".into());
        union.cost = filtered.cost;
        union.children = vec![filtered, Node::empty(vec![subject, key, target])];
        let mut root = unary(
            Kind::Slice {
                offset: 0,
                limit: Some(1),
            },
            union,
            "ASK demand",
        );
        super::super::unionpush::order_ask(&ctx, &mut root);
        assert!(ctx.eager_union_demand.load(AtomicOrdering::Relaxed));
        if !reduced {
            ctx.eager_union_demand.store(false, AtomicOrdering::Relaxed);
        }
        let (output, plan) = execute(&ctx, &root)?;
        assert_eq!(output.len(), 1);
        assert_eq!(output.cols[0][0], witness);
        fn find_build(p: &PlanInfo) -> Option<&PlanInfo> {
            if p.operator == "Values" && p.description == "full build" {
                return Some(p);
            }
            p.children.iter().find_map(find_build)
        }
        let build = find_build(&plan).unwrap();
        assert_eq!(build.actual_rows, build_rows as i64);
        Ok(build.fidelity().reruns + 1)
    }

    #[test]
    fn admitted_ask_filters_do_not_restart_full_join_builds() {
        assert_eq!(
            run(false, 2_000_000, 1000, false, 8, None).unwrap(),
            1,
            "ordinary prefixes build the input once"
        );
        assert_eq!(
            run(true, 2_000_000, 1000, false, 8, None).unwrap(),
            1,
            "reduced filter demand must not rebuild full inputs"
        );
    }

    #[test]
    fn early_filtered_join_keeps_its_budget_safe_exact_prefix() {
        assert_eq!(run(true, 64, 64, true, 0, None).unwrap(), 1);
    }

    #[test]
    fn tight_memory_keeps_exact_demand_when_wider_work_cannot_be_proven() {
        assert_eq!(
            run(true, 2_000_000, 1000, false, 8, Some(2 << 20)).unwrap(),
            3
        );
    }
}
