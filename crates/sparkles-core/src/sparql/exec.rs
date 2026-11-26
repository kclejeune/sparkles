//! Physical operator execution (column-at-a-time, fully materialized results).

use super::ctx::Ctx;
use super::expr::{Expr, Row, Val, ebv, eval};
use super::exprcache::Report as ExprReport;
use super::join::hash::{
    self, EqualityIndex, HashIndex, Join, Mode, NO_ROW, PROBE_PIECE, Probe, gather,
};
use super::join::merge;
use super::plan::{
    Agg, GraphFilter, JoinAlgo, Kind, Node, OrderedTopK, PathEnd, PathSpec, RangeSpec, ScanSpec,
};
use super::sortkey::{Entry, Screen, SortKey, TopK};
use super::table::{InputCredit, Materialized, Table, VarId};
use super::value::{NumOp, Value, arith, order_cmp};
use crate::access::Reservation;
use crate::error::{Error, Result};
use crate::id::Id;
use crate::index::{Block, ColMask, Key, Perm, pad};
use crate::store::Chunk;
use rayon::prelude::*;
use rustc_hash::{FxHashMap, FxHashSet};
use serde::Serialize;
use spargebra::algebra::AggregateFunction;
use std::cmp::Ordering;
use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};
use std::time::Instant;

pub(super) mod graph_diagnostics;

pub(super) const PAR_THRESHOLD: usize = 16_384;

/// Parallel iterators over rows hand out pieces of at least this many rows, so a small
/// input is processed on the calling thread: waking the pool for it would cost more
/// than the work, and a short request would wait for threads on idle cores to wake.
pub(super) const PAR_MIN_LEN: usize = 4096;

/// Rows that the partitioned job of [`map_rows`] maps between two polls of cancellation
/// and the deadline.
const MAP_POLL_ROWS: usize = 1024;

/// Map `f` over rows `0..n` (in parallel when `par`) in chunks, checking cancellation and
/// the deadline between chunks: one clock read per chunk rather than per row, while an
/// expensive expression still stops within one chunk of the deadline.
pub(super) fn map_rows<T: Send>(
    ctx: &Ctx,
    n: usize,
    par: bool,
    f: impl Fn(usize) -> T + Sync + Send,
) -> Result<Vec<T>> {
    if par {
        // Submit one partitioned job and poll within each partition. Repeated pool
        // submissions cost more than cheap numeric predicates. Temporary collector
        // capacity is optional and reserved before dispatch. A piece holds at least
        // one poll's rows, and no more, so that a table just above `PAR_THRESHOLD`
        // still spreads over every thread: pieces of `PAR_MIN_LEN` rows left the
        // 20,000 groups of `subquery-agg` at 10.5M triples on four of twelve threads,
        // which made its eager FILTER about 12% slower.
        let bytes = (n as u64)
            .saturating_mul(std::mem::size_of::<T>() as u64)
            .saturating_mul(2)
            .saturating_add(rayon::current_num_threads() as u64 * 1024);
        if let Ok(_scratch) = ctx.charge(bytes) {
            let out: Vec<T> = (0..n)
                .into_par_iter()
                .with_min_len(MAP_POLL_ROWS)
                .map(|i| {
                    if i.is_multiple_of(MAP_POLL_ROWS) {
                        ctx.check()?;
                    }
                    Ok(f(i))
                })
                .collect::<Result<Vec<T>>>()?;
            ctx.check()?;
            return Ok(out);
        }
    }
    // Without the collector's charge, a parallel map runs in pieces that each collect
    // into the output.
    let chunk = if par { 1 << 16 } else { 1 << 10 };
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
    ctx.check()?;
    Ok(out)
}

/// Rows of a cursor batch that [`map_batch`] maps on the calling thread while it times
/// them.
const BATCH_SAMPLE_ROWS: usize = 256;

/// The projected time of a batch's remaining rows from which [`map_batch`] maps them in
/// parallel. Waking the pool's threads and joining them costs tens of microseconds.
const BATCH_PAR_NANOS: u128 = 200_000;

/// Rows per parallel piece of a cursor batch.
const BATCH_PAR_PIECE: usize = 256;

/// Whether [`map_batch`] may spread a batch of `n` rows over the pool: the rows after its
/// timed sample fill at least two pieces.
#[inline]
pub(super) fn batch_may_split(n: usize) -> bool {
    n >= BATCH_SAMPLE_ROWS + 2 * BATCH_PAR_PIECE
}

/// [`map_rows`] for a batch of at most [`PAR_THRESHOLD`] rows, such as one of a cursor,
/// whose rows are too few for the row count alone to say whether the pool pays. The first [`BATCH_SAMPLE_ROWS`] rows are mapped
/// on this thread and timed, and the rest are mapped in parallel when `par` allows it
/// and their projected time reaches [`BATCH_PAR_NANOS`]. A cheap expression, such as
/// arithmetic on inline numbers, stays on this thread, and an expensive one, such as
/// date functions and decimal arithmetic on every row, is spread over the pool instead
/// of leaving the whole batch on one core.
pub(super) fn map_batch<T: Send>(
    ctx: &Ctx,
    n: usize,
    par: bool,
    f: impl Fn(usize) -> T + Sync + Send,
) -> Result<Vec<T>> {
    let sample = n.min(BATCH_SAMPLE_ROWS);
    let rest = n - sample;
    let mut out = Vec::with_capacity(n);
    ctx.check()?;
    // Only a batch whose rest may go to the pool is timed. A small one, such as the few
    // rows of an aggregate's BIND, would pay a clock read for nothing.
    let may_split = par && batch_may_split(n) && rayon::current_num_threads() > 1;
    let t0 = may_split.then(Instant::now);
    out.extend((0..sample).map(&f));
    ctx.check()?;
    if let Some(t0) = t0
        && t0.elapsed().as_nanos() * rest as u128 >= BATCH_PAR_NANOS * sample as u128
    {
        // Each piece of the rest is its own vector until it is appended to `out`, which
        // already holds room for every row, so the pieces are the memory this adds.
        let starts: Vec<usize> = (sample..n).step_by(BATCH_PAR_PIECE).collect();
        let bytes = (rest as u64)
            .saturating_mul(std::mem::size_of::<T>() as u64)
            .saturating_add(
                (starts.len() as u64)
                    .saturating_mul(std::mem::size_of::<Result<Vec<T>>>() as u64 + 16),
            );
        if let Ok(_scratch) = ctx.charge(bytes) {
            // an indexed collect writes each piece's slot in place
            let pieces: Vec<Result<Vec<T>>> = starts
                .into_par_iter()
                .map(|start| {
                    ctx.check()?;
                    Ok((start..(start + BATCH_PAR_PIECE).min(n)).map(&f).collect())
                })
                .collect();
            for piece in pieces {
                out.extend(piece?);
            }
            ctx.check()?;
            return Ok(out);
        }
    }
    let mut start = sample;
    while start < n {
        ctx.check()?;
        let end = (start + 1024).min(n);
        out.extend((start..end).map(&f));
        start = end;
    }
    ctx.check()?;
    Ok(out)
}

/// [`map_batch`] writing directly into an already-allocated output slice `out` for
/// rows `base..base + out.len()`, avoiding intermediate piece vectors and copies.
fn fill_batch_with<S, T: Send>(
    ctx: &Ctx,
    out: &mut [T],
    base: usize,
    init: impl Fn() -> S + Sync + Send,
    f: impl Fn(&mut S, usize) -> T + Sync + Send,
) -> Result<()> {
    let n = out.len();
    let mut state = init();
    let sample = n.min(BATCH_SAMPLE_ROWS);
    let rest = n - sample;
    ctx.check()?;
    let may_split = batch_may_split(n) && rayon::current_num_threads() > 1;
    let t0 = may_split.then(Instant::now);
    for (offset, slot) in out[..sample].iter_mut().enumerate() {
        *slot = f(&mut state, base + offset);
    }
    if let Some(t0) = t0
        && t0.elapsed().as_nanos() * rest as u128 >= BATCH_PAR_NANOS * sample as u128
    {
        drop(state);
        out[sample..]
            .par_chunks_mut(BATCH_PAR_PIECE)
            .enumerate()
            .try_for_each_init(&init, |state, (chunk_idx, chunk)| -> Result<()> {
                ctx.check()?;
                let start = base + sample + chunk_idx * BATCH_PAR_PIECE;
                for (offset, slot) in chunk.iter_mut().enumerate() {
                    *slot = f(state, start + offset);
                }
                Ok(())
            })?;
        return ctx.check();
    }
    let mut start = sample;
    while start < n {
        ctx.check()?;
        let end = (start + 1024).min(n);
        for (offset, slot) in out[start..end].iter_mut().enumerate() {
            *slot = f(&mut state, base + start + offset);
        }
        start = end;
    }
    ctx.check()
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
    #[doc(hidden)]
    pub prepared_diagnostics: Option<std::sync::Arc<PreparedDiagnostics>>,
}

/// Owns the storage admitted while constructing an internal diagnostic tree.
#[doc(hidden)]
pub struct PreparedDiagnostics(
    pub(super) Reservation,
    Option<std::sync::Arc<PreparedDiagnostics>>,
);

impl PreparedDiagnostics {
    pub(super) fn attach(info: &mut PlanInfo, held: Reservation) {
        info.prepared_diagnostics = Some(std::sync::Arc::new(Self(
            held,
            info.prepared_diagnostics.take(),
        )));
    }
}

impl std::fmt::Debug for PreparedDiagnostics {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("PreparedDiagnostics")
            .field(&self.0.bytes())
            .field(&self.1)
            .finish()
    }
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
    if matches!(n.kind, Kind::GraphBranches(_))
        && let Some(preparation) = &ctx.source_preparation
    {
        return graph_diagnostics::tree(ctx, n, preparation)
            .unwrap_or_else(|_| describe_node(ctx, n.graph_singleton().unwrap_or(n)));
    }
    PlanInfo {
        children: described_children(ctx, n),
        ..describe_node(ctx, n)
    }
}

/// [`describe`] that stops at the query's timeout or cancellation: describing a plan at
/// the plan limit names thousands of columns for each of thousands of nodes.
pub(super) fn try_describe(ctx: &Ctx, n: &Node) -> Result<PlanInfo> {
    if matches!(n.kind, Kind::GraphBranches(_))
        && let Some(preparation) = &ctx.source_preparation
    {
        return graph_diagnostics::tree(ctx, n, preparation);
    }
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
pub(super) const LEFT_EMPTY: &str = "the left side produced no rows";

/// Execute with the result cache: subtrees that are cheap to recompute (leaf scans and
/// value tables) bypass it; everything else is looked up and stored.
/// This legacy raw facade returns caller-owned, uncredited storage. Internal
/// bounded consumers retain execute_materialized's actual owner instead.
///
/// The context is shared first when it was not, as [`Ctx::into_shared`] shares it, so
/// that a LIMIT, an ASK or an EXISTS pattern in `n` reads its input only to the rows it
/// needs whoever built the context (item 980).
pub fn execute(ctx: &std::sync::Arc<Ctx>, n: &Node) -> Result<(Table, PlanInfo)> {
    if ctx.shared().is_none() {
        ctx.attach_owner();
    }
    let (output, info) = execute_materialized(ctx, n)?;
    Ok((output.table, info))
}
pub(super) fn execute_owned(ctx: &Ctx, n: &Node) -> Result<(Materialized<'static>, PlanInfo)> {
    let (output, info) = execute_owned_inner(ctx, n, contains_algorithm(n))?;
    Ok((output.into_lease_owned(ctx)?, info))
}
pub(crate) fn execute_materialized<'ctx>(
    ctx: &'ctx Ctx,
    n: &Node,
) -> Result<(Materialized<'ctx>, PlanInfo)> {
    execute_owned_inner(ctx, n, contains_algorithm(n))
}
pub(super) fn contains_algorithm(n: &Node) -> bool {
    matches!(n.kind, Kind::AlgoCall(_))
        || match &n.kind {
            Kind::GraphBranches(branches) => branches.contains_algorithm,
            _ => n.children.iter().any(contains_algorithm),
        }
}
fn execute_owned_inner<'ctx>(
    ctx: &'ctx Ctx,
    n: &Node,
    algorithm: bool,
) -> Result<(Materialized<'ctx>, PlanInfo)> {
    execute_owned_with_diagnostics(ctx, n, algorithm, false)
}

fn execute_owned_with_diagnostics<'ctx>(
    ctx: &'ctx Ctx,
    n: &Node,
    algorithm: bool,
    checked_diagnostics: bool,
) -> Result<(Materialized<'ctx>, PlanInfo)> {
    if let Some(branch) = n.graph_singleton() {
        return execute_owned_with_diagnostics(ctx, branch, algorithm, true).inspect_err(|_| {
            if let Some(info) = ctx.take_failed(branch) {
                ctx.set_failed(n, info);
            }
        });
    }
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
                | Kind::GraphNames(_)
                | Kind::GraphBranches(_)
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
        let start = Instant::now();
        let hit = if algorithm {
            results.get_owned(k, ctx)?
        } else {
            results.get(k, ctx)?.map(Materialized::plain)
        };
        if let Some(t) = hit {
            ctx.produced(t.table.len())?;
            let mut info = if checked_diagnostics {
                let preparation = ctx.source_preparation.as_ref().expect("checked GRAPH");
                graph_diagnostics::tree(ctx, n, preparation)?
            } else {
                describe(ctx, n)
            };
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
    let (t, info) = execute_node(ctx, n, None, algorithm, None, checked_diagnostics)?;
    if let Some(k) = key {
        // only worth caching when recomputation is not trivially cheap
        if info.time_ms >= results.min_ms {
            results.put(k, &t.table, ctx);
        }
    }
    Ok((t, info))
}

/// The inputs of an operator, from a caller that produces each child's table itself.
pub(super) type Inputs<'scope, 'ctx> =
    &'scope mut dyn FnMut(usize) -> Result<(Materialized<'ctx>, PlanInfo)>;

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
pub(super) fn execute_with_inputs<'ctx>(
    ctx: &'ctx Ctx,
    n: &Node,
    inputs: Inputs<'_, 'ctx>,
) -> Result<(Materialized<'ctx>, PlanInfo)> {
    debug_assert!(reads_inputs(n));
    execute_node(ctx, n, Some(inputs), contains_algorithm(n), None, false)
}

fn execute_node<'ctx>(
    ctx: &'ctx Ctx,
    n: &Node,
    inputs: Option<Inputs<'_, 'ctx>>,
    algorithm: bool,
    equality: Option<(VarId, VarId)>,
    checked_diagnostics: bool,
) -> Result<(Materialized<'ctx>, PlanInfo)> {
    let start = Instant::now();
    let checked_diagnostics = checked_diagnostics || matches!(n.kind, Kind::GraphBranches(_));
    let mut failing = Failing {
        ctx,
        n,
        start,
        infos: Vec::new(),
        prepared: None,
        armed: true,
    };
    if checked_diagnostics {
        let preparation = ctx
            .source_preparation
            .as_ref()
            .expect("checked GRAPH diagnostics");
        let mut held = graph_diagnostics::owner(preparation)?;
        let children = n.graph_diagnostic_children();
        let bytes = children
            .len()
            .checked_mul(size_of::<PlanInfo>())
            .ok_or_else(|| preparation.exhausted())?;
        held.grow_by(bytes as u64)?;
        held.grow_by(size_of::<PlanFidelity>() as u64)?;
        let header = graph_diagnostics::leaf(ctx, n, &mut held, None)?;
        failing.prepared = Some(DiagnosticState { header, held });
        failing
            .infos
            .try_reserve_exact(children.len())
            .map_err(|_| preparation.exhausted())?;
        if matches!(n.kind, Kind::GraphBranches(_)) {
            for branch in children {
                failing
                    .infos
                    .push(graph_diagnostics::pending(ctx, branch, preparation)?);
            }
        }
    }
    // The result goes straight to the caller, and the guard keeps the plan only when
    // `run_node` returns an error, so the success path pays nothing for it.
    run_node(
        ctx,
        n,
        inputs,
        start,
        &mut failing.infos,
        &mut failing.prepared,
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
    prepared: Option<DiagnosticState>,
    armed: bool,
}

struct DiagnosticState {
    header: PlanInfo,
    held: Reservation,
}

impl DiagnosticState {
    fn finish(mut self, infos: Vec<PlanInfo>) -> PlanInfo {
        self.header.children = infos;
        PreparedDiagnostics::attach(&mut self.header, self.held);
        self.header
    }
}

impl Drop for Failing<'_> {
    fn drop(&mut self) {
        if self.armed && !std::thread::panicking() {
            if let Some(mut diagnostic) = self.prepared.take() {
                diagnostic.header.fidelity_mut().incomplete = true;
                diagnostic.header.time_ms = self.start.elapsed().as_secs_f64() * 1000.0;
                if matches!(self.n.kind, Kind::GraphBranches(_)) {
                    for (branch, info) in self
                        .n
                        .graph_diagnostic_children()
                        .iter()
                        .zip(&mut self.infos)
                    {
                        if let Some(failure) = self.ctx.take_failed(branch) {
                            *info = failure;
                        }
                    }
                }
                let mut info = diagnostic.finish(std::mem::take(&mut self.infos));
                if self.ctx.graphs.is_some() {
                    info.redact();
                }
                self.ctx.set_failed(self.n, info);
                return;
            }
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
fn run_node<'ctx>(
    ctx: &'ctx Ctx,
    n: &Node,
    mut inputs: Option<Inputs<'_, 'ctx>>,
    start: Instant,
    infos: &mut Vec<PlanInfo>,
    diagnostics: &mut Option<DiagnosticState>,
    failing: &mut bool,
    algorithm: bool,
    equality: Option<(VarId, VarId)>,
) -> Result<(Materialized<'ctx>, PlanInfo)> {
    // the children's tables count against the memory budget until this operator is done
    let held = ctx.charge(0)?;
    let owned_input = std::cell::Cell::new(false);
    let last_input = std::cell::Cell::new(InputCredit::Plain);
    let filtered_cross = inputs.is_none().then(|| string_cross_hint(n)).flatten();
    let checked_diagnostics = diagnostics.is_some();
    let mut child = |i: usize, infos: &mut Vec<PlanInfo>| -> Result<Table> {
        let (mut t, info) = match inputs.as_mut() {
            Some(inputs) => inputs(i)?,
            // A filtered subset is not the unfiltered child's result. In particular,
            // never look it up or store it under the ordinary Cross cache key. The
            // logical parent Filter and both complete inputs still cache normally.
            None if i == 0 && filtered_cross.is_some() => execute_node(
                ctx,
                &n.children[i],
                None,
                algorithm,
                filtered_cross,
                checked_diagnostics,
            )?,
            None => execute_owned_with_diagnostics(
                ctx,
                &n.children[i],
                algorithm && contains_algorithm(&n.children[i]),
                checked_diagnostics,
            )?,
        };
        infos.push(info);
        let credit = t.adopt_input(ctx, &held)?;
        last_input.set(credit);
        match credit {
            InputCredit::Lease => owned_input.set(true),
            InputCredit::Fast => {}
            InputCredit::Plain => held.add(t.table.mem_bytes())?,
        }
        Ok(t.table)
    };
    // runtime detail for EXPLAIN (which variant of the operator ran)
    let mut note: Option<String> = None;
    let mut counters = None;
    // expressions evaluated per distinct value
    let mut expr_report = ExprReport::default();
    let mut output_lease = None;
    let mut output_fast = None;
    let mut preserve_fast_input = false;
    let mut table = match &n.kind {
        Kind::Empty => Table::empty(n.vars.clone()),
        Kind::Values(t) => {
            let (table, credit) = Materialized::clone_fast(ctx, t)?.into_fast_parts()?;
            output_fast = Some(credit);
            table
        }
        Kind::GraphNames(names) => {
            let (table, credit) = Materialized::clone_fast(ctx, &names.table)?.into_fast_parts()?;
            output_fast = Some(credit);
            table
        }
        Kind::GraphBranches(branches) => {
            let mut out = Materialized::empty_fast(ctx, &n.vars, &[])?;
            for (index, branch) in branches.inputs.iter().enumerate() {
                ctx.check()?;
                let (rows, info) = execute_owned_with_diagnostics(
                    ctx,
                    branch,
                    algorithm && contains_algorithm(branch),
                    true,
                )?;
                infos[index] = info;
                let rows = rows.as_table();
                let count = out
                    .table
                    .len
                    .checked_add(rows.len)
                    .ok_or_else(|| Error::invalid("graph group row count overflow"))?;
                ctx.check_output_rows(count)?;
                out.reserve_rows(ctx, count)?;
                for (&var, target) in n.vars.iter().zip(&mut out.table.cols) {
                    if let Some(column) = rows.col_of(var) {
                        for part in rows.cols[column].chunks(1024) {
                            ctx.check()?;
                            target.extend_from_slice(part);
                        }
                    } else {
                        while target.len() < count {
                            ctx.check()?;
                            target.resize((target.len() + 1024).min(count), Id::UNDEF);
                        }
                    }
                }
                out.table.len = count;
            }
            let (table, credit) = out.into_fast_parts()?;
            output_fast = Some(credit);
            table
        }
        Kind::Scan(spec) => {
            note = column_note(spec);
            let (table, credit) = scan(ctx, spec, &n.vars, &n.sorted)?.into_fast_parts()?;
            output_fast = Some(credit);
            table
        }
        Kind::RangeScan(spec, range) => {
            note = column_note(spec);
            let (table, credit) =
                range_scan(ctx, spec, range, &n.vars, &n.sorted)?.into_fast_parts()?;
            output_fast = Some(credit);
            table
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
                note = Some(
                    "[values not totally ordered, or ids cannot decide: ran the generic plan]"
                        .into(),
                );
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
                    JoinAlgo::Merge => join_or_spill(ctx, n, l, r, &mut note, |l, r, note| {
                        join_noted(ctx, l, r, true, note)
                    })?,
                    JoinAlgo::Hash => join_or_spill(ctx, n, l, r, &mut note, |l, r, note| {
                        join_noted(ctx, l, r, false, note)
                    })?,
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
        Kind::CountJoin { algo, var, .. } => {
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
            let r = match optional_probe(ctx, &l, &n.children[1])? {
                Some((r, info)) => {
                    infos.push(info);
                    held.add(r.mem_bytes())?;
                    r
                }
                None => child(1, infos)?,
            };
            let t = join_or_spill(ctx, n, l, r, &mut note, |l, r, note| {
                left_join(ctx, l, r, expr.as_ref(), note)
            })?;
            if let Some(e) = expr {
                let (exists, c) = super::exists::explain(ctx, std::slice::from_ref(e));
                if let Some(x) = exists {
                    note = Some(note.map_or(x.clone(), |n| format!("{n} {x}")));
                }
                counters = merge_counters(counters, c);
            }
            t
        }
        Kind::KeyedLeftJoin { expr, left, right } => {
            let l = child(0, infos)?;
            let r = child(1, infos)?;
            // a keyed join partitions in neither mode, since its sides share no variable
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
            join_or_spill(ctx, n, l, r, &mut note, |l, r, note| minus(ctx, l, r, note))?
        }
        Kind::HalfJoin { anti } => {
            let l = child(0, infos)?;
            let r = child(1, infos)?;
            join_or_spill(ctx, n, l, r, &mut note, |l, r, _| {
                half_join(ctx, l, r, *anti)
            })?
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
            (note, counters) = super::exists::explain(ctx, std::slice::from_ref(e));
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
            let exprs: Vec<Expr> = keys
                .iter()
                .filter(|(e, _)| e.has_exists())
                .map(|(e, _)| e.clone())
                .collect();
            if !exprs.is_empty() {
                let (exists, c) = super::exists::explain(ctx, &exprs);
                if let Some(x) = exists {
                    note = Some(note.map_or(x.clone(), |n| format!("{n} {x}")));
                }
                counters = merge_counters(counters, c);
            }
            t
        }
        Kind::Project(vars) => {
            let input = child(0, infos)?;
            match last_input.get() {
                InputCredit::Plain => input.project(vars),
                credit => {
                    preserve_fast_input = credit == InputCredit::Fast;
                    Materialized::project_paid(ctx, input, &held, vars)?
                }
            }
        }
        Kind::Distinct => distinct_of(ctx, n, child(0, infos)?)?,
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
            let (t, nt) = super::patternplan::run_scan(ctx, spec, &n.vars)?;
            note = Some(nt);
            t
        }
        Kind::Slice {
            offset,
            limit: Some(limit),
            whole,
        } if !super::demand::ordered_input(&n.children[0]) => {
            // LIMIT without ORDER BY: any `offset + limit` solutions will do, so the
            // input runs as cursor operators that stop when they have them (item 827),
            // or whole when the planner found it small
            let want = offset.saturating_add(*limit);
            let (mut t, info, _) = match leaf_rows(ctx, &n.children[0], want)? {
                Some(rows) => rows,
                None if *whole => match whole_or_refused(ctx, &n.children[0])? {
                    Some((t, info)) => (t, info, true),
                    None => super::cursor::pull(ctx, &n.children[0], want)?,
                },
                None => super::cursor::pull(ctx, &n.children[0], want)?,
            };
            infos.push(info);
            t = t.slice(ctx, *offset, Some(*limit))?;
            match t.adopt_input(ctx, &held)? {
                InputCredit::Plain => held.add(t.table.mem_bytes())?,
                InputCredit::Fast => preserve_fast_input = true,
                InputCredit::Lease => owned_input.set(true),
            }
            t.table
        }
        Kind::Slice { offset, limit, .. } => {
            let input = child(0, infos)?;
            match last_input.get() {
                InputCredit::Plain => input.slice(*offset, *limit),
                credit => {
                    preserve_fast_input = credit == InputCredit::Fast;
                    Materialized::slice_paid(ctx, input, &held, *offset, *limit)?
                }
            }
        }
        Kind::Group { keys, aggs } => {
            let t = child(0, infos)?;
            let out = group_or_spill(ctx, n, t, keys, aggs, &mut expr_report, &mut note)?;
            let exprs: Vec<Expr> = aggs
                .iter()
                .filter_map(|(_, agg)| agg.expr.as_ref())
                .filter(|e| e.has_exists())
                .cloned()
                .collect();
            if !exprs.is_empty() {
                let (exists, c) = super::exists::explain(ctx, &exprs);
                if let Some(x) = exists {
                    note = Some(note.map_or(x.clone(), |n| format!("{n} {x}")));
                }
                counters = merge_counters(counters, c);
            }
            out
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
            note = Some(st.note());
            counters = Some(st.counters());
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
        Kind::ViewRead(spec) => {
            if let Some(shared) = ctx.shared()
                && ctx
                    .views
                    .as_ref()
                    .is_some_and(|source| source.streams_views())
            {
                let table = super::cursor::eager_view(&shared, n, &held)?;
                output_lease = Some(held.output_lease(super::algosvc::table_capacity(&table)?)?);
                table
            } else {
                super::viewsvc::run(ctx, spec, &n.vars)?
            }
        }
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
            let (output, c, runs) = spatial_knn(ctx, spec, &n.children[0], &n.vars)?;
            let (t, credit) = output.into_fast_parts()?;
            output_fast = Some(credit);
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
    } else if output_fast.is_none() {
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
    let paid_fast = output_fast.is_some() || preserve_fast_input;
    let mut output = if preserve_fast_input {
        Materialized::with_fast(table, held)?
    } else {
        drop(held);
        match output_fast {
            Some(credit) => Materialized::with_fast(table, credit)?,
            None => Materialized::with_lease(table, output_lease),
        }
    };
    if let Some(lease) = &mut output.lease {
        lease.resize(
            ctx.mem_limit,
            super::algosvc::table_capacity(&output.table)?,
        )?;
        ctx.check_output_rows(output.table.len())?;
    } else if paid_fast {
        ctx.check_output_rows(output.table.len())?;
    } else {
        ctx.check_output(output.table.len(), output.table.width())?;
    }
    ctx.produced(output.table.len())?;
    if let (Some(diag), Some(note)) = (diagnostics.as_mut(), note.as_deref()) {
        diag.header.description = graph_diagnostics::text(&mut diag.held, |out| {
            out.write_str(&n.desc)?;
            out.write_char(' ')?;
            out.write_str(note)
        })?;
    }
    let mut info = if let Some(diag) = diagnostics.take() {
        diag.finish(std::mem::take(infos))
    } else {
        PlanInfo {
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
            children: std::mem::take(infos),
            ..Default::default()
        }
    };
    info.actual_rows = output.table.len() as i64;
    info.time_ms = start.elapsed().as_secs_f64() * 1000.0;
    info.counters = counters;
    *failing = false;
    Ok((output, info))
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
        let scan = scan_mask(spec);
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
                (b0 < b1).then(|| (b0, wanted(index, scan_mask(spec), r, b0), r))
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
pub(super) fn scan_mask(spec: &ScanSpec) -> crate::index::ColMask {
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
fn column_note(spec: &ScanSpec) -> Option<String> {
    let m = scan_mask(spec);
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
fn run_mask(spec: &ScanSpec) -> crate::index::ColMask {
    let all = scan_mask(spec);
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
    if spec.cols.first().map(|x| x.0) != Some(c) || c >= 4 {
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
    let piece = PIECE.min(
        (ctx.memory_remaining() / rayon::current_num_threads().max(1) as u64 / 128).max(1) as usize,
    );
    let parts =
        ctx.snap
            .par_blocks_in_ranges(spec.perm, ranges, run_mask(spec), piece, |b, s, e| {
                let _scratch = ctx.charge((e - s) as u64 * 48 + 1024)?;
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
                let bound = kf.bind();
                let mut test = |p, key: &[u8]| {
                    while ids[j] != p {
                        j += 1;
                    }
                    pass[j] = bound.test(key);
                };
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
            })?;
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
    let parts =
        ctx.snap
            .par_blocks_in_ranges(spec.perm, &ranges, scan_mask(spec), PIECE, |b, s, e| {
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
            })?;
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
/// the delta has more keys in the ranges than a parallel read merges.
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
    let parts =
        ctx.snap
            .par_blocks_in_ranges(spec.perm, ranges, scan_mask(spec), PIECE, |b, s, e| {
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
                let bound = kf.bind();
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
                        pass[lo + at] = bound.test(k);
                        Ok::<(), Error>(())
                    },
                )?;
                drop(bound);
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
            })?;
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

/// [`key_runs`] with the index blocks read in parallel and the snapshot's delta merged in,
/// when rows need no comparison with their neighbours (no union-graph dedup) and
/// [`Snapshot::par_blocks_in_ranges`](crate::store::Snapshot::par_blocks_in_ranges) takes
/// the delta in the scan's range; `None` otherwise.
fn par_key_runs(
    ctx: &Ctx,
    spec: &ScanSpec,
    ranges: &[(Key, Key)],
) -> Result<Option<(Vec<Id>, Vec<u64>)>> {
    if spec.dedup {
        return Ok(None);
    }
    let parts =
        ctx.snap
            .par_blocks_in_ranges(spec.perm, ranges, run_mask(spec), PIECE, |b, s, e| {
                let mut keys: Vec<Id> = Vec::new();
                let mut counts: Vec<u64> = Vec::new();
                block_runs(spec, b, s, e, |k, n| {
                    keys.push(Id(k));
                    counts.push(n);
                });
                ctx.check()?;
                Ok((keys, counts))
            })?;
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
        .scan_between_cols(spec.perm, lo, hi, run_mask(spec), |chunk| {
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
        .scan_between_cols(spec.perm, lo, hi, run_mask(spec), |chunk| {
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

fn scan<'ctx>(
    ctx: &'ctx Ctx,
    spec: &ScanSpec,
    vars: &[VarId],
    sorted: &[VarId],
) -> Result<Materialized<'ctx>> {
    Ok(scan_limited(ctx, spec, vars, sorted, None)?.0)
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
        Kind::Distinct => distinct_of(ctx, n, t)?,
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

/// The first `want` rows of a scan, a pattern scan or a table of values, under
/// projections, read directly for a LIMIT without ORDER BY: a scan stops at them, as a
/// cursor scan does, without the cursor operators that a larger input runs as (item 827). `None` for
/// another input. Also returns the plan and whether the input ended.
pub(super) fn leaf_rows<'ctx>(
    ctx: &'ctx Ctx,
    n: &Node,
    want: usize,
) -> Result<Option<(Materialized<'ctx>, PlanInfo, bool)>> {
    let start = Instant::now();
    let finish = |t: Materialized<'ctx>, children: Vec<PlanInfo>, complete: bool| {
        ctx.produced(t.table.len())?;
        let mut info = describe(ctx, n);
        info.children = children;
        info.actual_rows = t.table.len() as i64;
        info.time_ms = start.elapsed().as_secs_f64() * 1000.0;
        if !complete {
            info.description = format!("{} [stopped early]", info.description);
            info.fidelity_mut().stopped_early = true;
        }
        Ok(Some((t, info, complete)))
    };
    match &n.kind {
        Kind::Scan(spec) => {
            let (t, truncated) = scan_limited(ctx, spec, &n.vars, &n.sorted, Some(want))
                .inspect_err(|_| fail(ctx, n, start, Vec::new()))?;
            finish(t, Vec::new(), !truncated)
        }
        Kind::GraphNames(names) => {
            let count = want.min(names.table.len());
            let mut out = Materialized::empty_fast(ctx, &n.vars, &[])?;
            ctx.check_output_rows(count)?;
            out.reserve_rows(ctx, count)?;
            for ids in names.table.cols[0][..count].chunks(1024) {
                ctx.check()?;
                out.table.cols[0].extend_from_slice(ids);
            }
            out.table.len = count;
            finish(out, Vec::new(), count == names.table.len())
        }
        Kind::Values(_) | Kind::Empty => {
            let (t, info) = execute_materialized(ctx, n)?;
            Ok(Some((t, info, true)))
        }
        // the pattern reader stops at the rows asked for, as the cursor's does
        Kind::PatternScan(spec) => {
            let mut t = Table::new(n.vars.clone());
            let mut reader = super::patternplan::scan_reader(spec);
            let held = ctx.charge(0)?;
            let complete =
                super::patternplan::fill_scan(ctx, spec, &mut reader, &mut t, want, &mut |bytes| {
                    held.resize(bytes)
                })
                .inspect_err(|_| fail(ctx, n, start, Vec::new()))?;
            drop(reader);
            drop(held);
            let mut rows = finish(Materialized::plain(t), Vec::new(), complete)?;
            if let Some((_, info, _)) = &mut rows {
                info.description = format!("{} {}", info.description, spec.uses.note());
            }
            Ok(rows)
        }
        Kind::Project(vars) if n.children.len() == 1 => {
            let Some((input, info, complete)) = leaf_rows(ctx, &n.children[0], want)? else {
                return Ok(None);
            };
            let output = match input.project(ctx, vars) {
                Ok(output) => output,
                Err(error) => {
                    fail(ctx, n, start, vec![info]);
                    return Err(error);
                }
            };
            finish(output, vec![info], complete)
        }
        _ => Ok(None),
    }
}

/// Run `n` whole for a LIMIT, an ASK or an EXISTS pattern whose input the planner chose
/// to run whole. `None` when a row or memory budget refused the run: its bounds keep the
/// input small, but not every allocation, the pool's headroom or what the query already
/// holds, so the consumer then pulls the input as it would have otherwise. Nothing of
/// the refused run is kept, and its partial plan is forgotten.
pub(super) fn whole_or_refused<'ctx>(
    ctx: &'ctx Ctx,
    n: &Node,
) -> Result<Option<(Materialized<'ctx>, PlanInfo)>> {
    match execute_materialized(ctx, n) {
        Ok(run) => Ok(Some(run)),
        Err(Error::BudgetExceeded(b))
            if matches!(
                b.kind,
                crate::BudgetKind::Rows
                    | crate::BudgetKind::Memory
                    | crate::BudgetKind::ServerMemory
            ) =>
        {
            ctx.clear_failed();
            Ok(None)
        }
        Err(error) => Err(error),
    }
}

/// Scan with an optional row limit; returns `(table, truncated)`.
pub(super) fn scan_limited<'ctx>(
    ctx: &'ctx Ctx,
    spec: &ScanSpec,
    vars: &[VarId],
    sorted: &[VarId],
    limit: Option<usize>,
) -> Result<(Materialized<'ctx>, bool)> {
    let mut t = Materialized::empty_fast(ctx, vars, sorted)?;
    // the exact number of rows under the prefix (at most two block decodes, and those
    // blocks are read by the scan anyway) bounds the output: reserve it once instead of
    // growing every column by repeated doubling
    let bound = ctx.snap.count(spec.perm, &spec.prefix)?;
    let cap = usize::try_from(bound)
        .unwrap_or(usize::MAX)
        .min(limit.unwrap_or(usize::MAX))
        .min(ctx.max_rows.saturating_add(1))
        .min(ctx.rows_within_budget(vars.len()));
    t.reserve_rows(ctx, cap)?;
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
fn range_scan<'ctx>(
    ctx: &'ctx Ctx,
    spec: &ScanSpec,
    range: &RangeSpec,
    vars: &[VarId],
    sorted: &[VarId],
) -> Result<Materialized<'ctx>> {
    let mut t = Materialized::empty_fast(ctx, vars, sorted)?;
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
            let mut part = Materialized::empty_fast(ctx, vars, &[])?;
            scan_into(ctx, spec, lo, hi, None, &mut part)?;
            if !part.table.is_empty() {
                apply_range_filter(ctx, &mut part.table, range.var, &range.filter)?;
                let rows = t
                    .table
                    .len
                    .checked_add(part.table.len)
                    .ok_or_else(|| Error::invalid("range output rows overflow"))?;
                ctx.check_output_rows(rows)?;
                t.reserve_rows(ctx, rows)?;
                for (column, source) in t.table.cols.iter_mut().zip(&part.table.cols) {
                    for block in source.chunks(1024) {
                        ctx.check()?;
                        column.extend_from_slice(block);
                    }
                }
                t.table.len = rows;
            }
        }
        ctx.check_output_rows(t.table.len())?;
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
    fn holds(&self, x: Id, v: super::value::Num<'_>) -> bool {
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
        let _scratch = ctx.charge((t.len() as u64).saturating_add(
            (ctx.nvars() as u64).saturating_mul(size_of::<Option<usize>>() as u64),
        ))?;
        apply_filter(ctx, t, exprs)?;
        return Ok(());
    };
    let col = &t.cols[c];
    let n = col.len();
    let vocab_count = col.iter().filter(|x| x.tag() == Tag::Vocab).count();
    let decided_bytes = n.saturating_mul(size_of::<Option<bool>>()) as u64;
    let vocab_bytes = (vocab_count as u64)
        .saturating_mul((size_of::<u64>() + size_of::<(u8, u64)>() + size_of::<Numeric>()) as u64);
    let scratch = ctx.charge(decided_bytes.saturating_add(vocab_bytes))?;
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
                .and_then(|v| Num::of(&v).ok().map(|n| test(x, n))),
            _ => None,
        }
    };
    // the distinct vocabulary ids of a run of rows read from the column at once, and
    // runs of one id (the scan's sorted column) decided once, written directly into
    // disjoint slices of the pre-admitted `decided` buffer
    let fill_chunk = |ids: &[Id], out: &mut [Option<bool>]| {
        let vocab_len = ids.iter().filter(|x| x.tag() == Tag::Vocab).count();
        let mut vocab = Vec::with_capacity(vocab_len);
        if vocab_len > 0 {
            vocab.extend(
                ids.iter()
                    .filter(|x| x.tag() == Tag::Vocab)
                    .map(|x| x.payload()),
            );
            // already sorted when the column is the scan's sorted column
            if !vocab.is_sorted() {
                vocab.sort_unstable();
            }
            vocab.dedup();
        }
        let found = if vocab.is_empty() {
            Vec::new()
        } else {
            num.get_many(&vocab)
        };
        let mut at = 0;
        let mut last: Option<(Id, Option<bool>)> = None;
        for (&x, slot) in ids.iter().zip(out.iter_mut()) {
            *slot = match last {
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
            };
        }
    };
    let mut decided = vec![None; n];
    if col.len() > PAR_THRESHOLD {
        let step = PAR_MIN_LEN.max(4096);
        col.par_chunks(step)
            .zip(decided.par_chunks_mut(step))
            .for_each(|(ids, out)| fill_chunk(ids, out));
    } else {
        fill_chunk(col, &mut decided);
    }
    let rest_len = decided.iter().filter(|d| d.is_none()).count();
    let keep_bytes = n.saturating_mul(size_of::<bool>()) as u64;
    let rest_bytes = rest_len.saturating_mul(size_of::<usize>()) as u64;
    scratch.resize(
        decided_bytes
            .saturating_add(keep_bytes)
            .saturating_add(rest_bytes),
    )?;
    let mut rest = Vec::with_capacity(rest_len);
    let mut keep = Vec::with_capacity(n);
    for (i, d) in decided.into_iter().enumerate() {
        match d {
            Some(k) => keep.push(k),
            None => {
                keep.push(false);
                rest.push(i);
            }
        }
    }
    scratch.resize(keep_bytes.saturating_add(rest_bytes))?;
    if !rest.is_empty() {
        let sub_bytes = topk_table_bytes(rest.len(), t.width());
        let mask_bytes =
            rest.len()
                .saturating_mul(size_of::<bool>())
                .saturating_add(ctx.nvars().saturating_mul(size_of::<Option<usize>>()))
                .saturating_add(exprs.len().saturating_mul(size_of::<&Expr>())) as u64;
        scratch.resize(
            keep_bytes
                .saturating_add(rest_bytes)
                .saturating_add(sub_bytes)
                .saturating_add(mask_bytes),
        )?;
        let sub = t.take_rows(&rest);
        let mask = filter_mask(ctx, &sub, exprs)?;
        for (&i, k) in rest.iter().zip(mask) {
            keep[i] = k;
        }
    }
    drop(rest);
    scratch.resize(keep_bytes)?;
    let sorted = std::mem::take(&mut t.sorted);
    t.filter_rows(&keep);
    t.sorted = sorted;
    drop(keep);
    drop(scratch);
    Ok(())
}

// ---------------------------------------------------------- ordered top-k ------

/// The unread part `[lo, hi]` of a monotone piece of an [`OrderedTopK`] scan.
struct TopKRun<'ctx> {
    lo: u64,
    hi: u64,
    exact: bool,
    /// the best values are at the high end of the id range
    best_high: bool,
    /// the rows read so far that can still be among the first k
    kept: Materialized<'ctx>,
    /// base rows to read in the next step (grows geometrically)
    want: usize,
    /// no unread row of the piece can be among the first k
    done: bool,
    /// a piece of base-vocabulary literals read in ascending order, whose ids follow
    /// their lexical forms except for prefixes (see [`TopKPiece::lexical`])
    lexical: bool,
    /// ids of a lexical piece above `lo` whose rows were read already
    extra: Vec<u64>,
    /// rank the candidates again before the next step (prefixes were just read)
    hold: bool,
}

/// What the k-th candidate proves about the unread ids of a lexical [`TopKRun`].
enum Lexical {
    /// none of them can be among the first k
    Done,
    /// read the piece on
    Step,
    /// these ids, proper prefixes of the last one read, can still win: read their rows
    Read(Vec<u64>),
    /// the ids cannot decide: the k-th candidate is a literal that unread plain strings
    /// may beat, or the last string read has too many prefixes to look up. The generic
    /// plan answers instead, which also keeps its order of tied rows.
    Generic,
}

/// The longest prefix, in bytes, that a check of a lexical run looks up. A check looks up
/// every prefix of the last string read up to the k-th candidate, each a search of
/// about the prefix's length, so a longer string gives the query to the generic plan
/// rather than spend time quadratic in its length.
const LEXICAL_PREFIX_BYTES: usize = 256;

/// The [`Lexical`] verdict on a lexical run, after a step, for the k-th candidate `kth`.
///
/// The ids `[lo, last]` of the piece have been read. An unread literal's key is greater
/// than the last one's, so an unread plain string is either greater than the last
/// lexical form, with no prefix relation, or a proper prefix of it. With a plain string
/// as the k-th candidate that is not greater than the last lexical form, only those
/// prefixes can still win, and each is a vocabulary lookup. An unread literal of another
/// kind ranks after every plain string.
fn lexical_check(
    ctx: &Ctx,
    spec: &OrderedTopK,
    run: &TopKRun<'_>,
    kth: Option<&Value>,
) -> Result<Lexical> {
    let s = match kth {
        None | Some(Value::BNode(_) | Value::Iri(_)) => return Ok(Lexical::Done),
        Some(Value::Str(s)) => s,
        // an unread plain string would beat it
        Some(_) => return Ok(Lexical::Generic),
    };
    let vocab = &ctx.snap.generation.vocab;
    let last = run.lo - 1;
    // Reconstruct the last key with controlled scratch admission and live controls,
    // copying only the bounded prefix needed for prefix lookups.
    let prefix = match ctx.with_controlled_vocab_key(Id(last).payload(), |key| {
        let lex = match key.split_first() {
            Some((b'"', rest)) => match rest.iter().position(|&b| b == crate::id::KEY_SEP) {
                Some(end) => &rest[..end],
                None => return Ok(Err(Lexical::Step)),
            },
            _ => return Ok(Err(Lexical::Step)),
        };
        if s.as_bytes() > lex {
            return Ok(Err(Lexical::Step));
        }
        let Ok(lex_str) = std::str::from_utf8(lex) else {
            return Ok(Err(Lexical::Step));
        };
        // Retain only the characters needed to enumerate the eligible prefixes.
        let mut take = 0usize;
        for (end, c) in lex_str.char_indices() {
            let p = &lex_str[..end];
            if p > &**s {
                break;
            }
            if end > LEXICAL_PREFIX_BYTES {
                return Ok(Err(Lexical::Generic));
            }
            take = end + c.len_utf8();
        }
        Ok(Ok(lex_str[..take].to_owned()))
    })? {
        Some(Ok(prefix)) => prefix,
        Some(Err(verdict)) => return Ok(verdict),
        None => return Ok(Lexical::Step),
    };
    let scan = &spec.scan;
    let bound = |v: u64, fill: u64| {
        let mut k = pad(&scan.prefix, fill);
        k[scan.prefix.len()] = v;
        k
    };
    let mut read = Vec::new();
    // the key of the prefix, grown a character at a time, without its 0xFF
    let mut probe = vec![b'"'];
    // the proper prefixes, shortest first: once one is greater than the k-th
    // candidate, so are the longer ones
    for (n, c) in prefix.chars().enumerate() {
        if n % 32 == 31 {
            ctx.check()?;
        }
        probe.push(crate::id::KEY_SEP);
        let found = vocab.find_exact_controlled(&probe, |_| ctx.check())?;
        probe.pop();
        let mut utf8 = [0; 4];
        probe.extend_from_slice(c.encode_utf8(&mut utf8).as_bytes());
        let Ok(at) = found else {
            continue;
        };
        let id = Id::vocab(at).0;
        if id <= last || id > run.hi || run.extra.contains(&id) {
            continue;
        }
        if ctx
            .snap
            .count_between(scan.perm, bound(id, 0), bound(id, u64::MAX))?
            > 0
        {
            read.push(id);
        }
    }
    Ok(if read.is_empty() {
        Lexical::Done
    } else {
        Lexical::Read(read)
    })
}

#[cfg(test)]
#[test]
fn ordered_lexical_check_refuses_oversized_probe_after_candidate_decode() {
    use crate::io::{RdfFormat, Source};
    use crate::store::{Store, StoreOptions};
    use oxrdf::{Literal, NamedNode, Term};

    let store = Store::in_memory(StoreOptions::default());
    let oversized = format!("ab{}", "x".repeat(32 * 1024));
    let data = format!("<urn:s> <urn:p> \"a\", \"abc\", \"{oversized}\" .");
    store
        .load(&[Source::from_bytes(
            data.into_bytes(),
            RdfFormat::Turtle,
            None,
        )])
        .unwrap();
    store.compact().unwrap();
    let snap = store.snapshot();
    let literal = |s| {
        snap.lookup_term(&Term::Literal(Literal::new_simple_literal(s)))
            .unwrap()
    };
    let candidate = literal("a");
    let short = literal("abc");
    let long = literal(oversized.as_str());
    let pred = snap
        .lookup_term(&Term::NamedNode(NamedNode::new_unchecked("urn:p")))
        .unwrap();
    let scan = super::plan::ScanSpec {
        perm: crate::index::Perm::Pos,
        prefix: vec![pred.0],
        cols: vec![(1, 0)],
        eqs: Vec::new(),
        graph: super::plan::GraphFilter::Default,
        graph_col: 3,
        dedup: false,
        src: None,
    };
    let spec = OrderedTopK {
        fallback: Box::new(Node::empty(vec![0])),
        scan,
        var: 0,
        asc: true,
        k: 1,
        pieces: Vec::new(),
        filter: Vec::new(),
        range_filter: Vec::new(),
        tie_order: vec![0],
    };
    fn make_run<'ctx>(ctx: &'ctx Ctx, last: Id, candidate: Id) -> TopKRun<'ctx> {
        TopKRun {
            lo: last.0 + 1,
            hi: candidate.0,
            exact: true,
            best_high: false,
            kept: Materialized::empty_fast(ctx, &[0], &[]).unwrap(),
            want: 1,
            done: false,
            lexical: true,
            extra: Vec::new(),
            hold: false,
        }
    }

    let control = Ctx::new(snap.clone());
    let kth = control.value(candidate).expect("short candidate decodes");
    assert!(matches!(
        lexical_check(&control, &spec, &make_run(&control, short, candidate), Some(&kth)).unwrap(),
        Lexical::Read(ids) if ids == [candidate.0]
    ));
    assert!(matches!(
        lexical_check(&control, &spec, &make_run(&control, long, candidate), Some(&kth)).unwrap(),
        Lexical::Read(ids) if ids == [candidate.0]
    ));

    let mut target = Ctx::new(snap);
    target.mem_limit = 24 * 1024;
    let kth = target
        .value(candidate)
        .expect("candidate fits tight budget");
    target.check().unwrap();
    assert!(matches!(
        lexical_check(&target, &spec, &make_run(&target, long, candidate), Some(&kth)),
        Err(Error::BudgetExceeded(b)) if b.kind == crate::BudgetKind::Memory
    ));
}

/// Rows of an [`OrderedTopK`] scan whose order-column ids lie in `[lo, hi]`, in key order,
/// with the filters applied; also returns how many rows were scanned.
fn topk_read<'ctx>(
    ctx: &'ctx Ctx,
    spec: &OrderedTopK,
    lo: u64,
    hi: u64,
    exact: bool,
    vars: &[VarId],
) -> Result<(Materialized<'ctx>, usize)> {
    let scan = &spec.scan;
    let bound = |v: u64, fill: u64| {
        let mut k = pad(&scan.prefix, fill);
        k[scan.prefix.len()] = v;
        k
    };
    topk_read_keys(ctx, spec, bound(lo, 0), bound(hi, u64::MAX), exact, vars)
}

/// [`topk_read`] of the keys `[lo, hi]` of the scan.
fn topk_read_keys<'ctx>(
    ctx: &'ctx Ctx,
    spec: &OrderedTopK,
    lo: crate::index::Key,
    hi: crate::index::Key,
    exact: bool,
    vars: &[VarId],
) -> Result<(Materialized<'ctx>, usize)> {
    topk_read_keys_last(ctx, spec, lo, hi, exact, vars).map(|(t, n, _)| (t, n))
}

/// [`topk_read_keys`], with the last row the scan produced before any filter when the
/// scan drops duplicate rows of a union default graph: the next read of the same piece
/// must drop that row again if it starts with it.
fn topk_read_keys_last<'ctx>(
    ctx: &'ctx Ctx,
    spec: &OrderedTopK,
    lo: crate::index::Key,
    hi: crate::index::Key,
    exact: bool,
    vars: &[VarId],
) -> Result<MorselBest<'ctx>> {
    let mut t = Materialized::empty_fast(ctx, vars, &[])?;
    scan_into(ctx, &spec.scan, lo, hi, None, &mut t)?;
    let table = &mut t.table;
    let scanned = table.len();
    let last = (spec.scan.dedup && scanned > 0).then(|| {
        let mut last = [Id::UNDEF; 4];
        for (dst, col) in last.iter_mut().zip(&table.cols) {
            *dst = col[scanned - 1];
        }
        last
    });
    if !exact && !spec.range_filter.is_empty() && !table.is_empty() {
        apply_range_filter(ctx, table, spec.var, &spec.range_filter)?;
    }
    if !spec.filter.is_empty() && !table.is_empty() {
        let _filter_scratch = ctx.charge((table.len() as u64).saturating_add(
            (ctx.nvars() as u64).saturating_mul(size_of::<Option<usize>>() as u64),
        ))?;
        apply_filter(ctx, table, &spec.filter)?;
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
fn topk_table_bytes(rows: usize, width: usize) -> u64 {
    128 + width as u64 * (size_of::<VarId>() * 2 + size_of::<Vec<Id>>()) as u64
        + super::ctx::table_bytes(rows, width)
}

fn topk_append(ctx: &Ctx, dst: &mut Materialized<'_>, src: Materialized<'_>) -> Result<()> {
    dst.reserve_rows(ctx, dst.table.len().saturating_add(src.table.len()))?;
    dst.table.append(src.table);
    Ok(())
}

fn topk_take_rows<'ctx>(
    ctx: &'ctx Ctx,
    source: &Table,
    rows: &[usize],
) -> Result<Materialized<'ctx>> {
    let mut out = Materialized::empty_fast(ctx, &source.vars, &[])?;
    out.reserve_rows(ctx, rows.len())?;
    for (dst, src) in out.table.cols.iter_mut().zip(&source.cols) {
        dst.extend(rows.iter().map(|&r| src[r]));
    }
    out.table.len = rows.len();
    Ok(out)
}

fn topk_step(
    ctx: &Ctx,
    spec: &OrderedTopK,
    run: &mut TopKRun<'_>,
    vars: &[VarId],
) -> Result<usize> {
    // base rows only: delta rows in between are merged by the scan
    let edge = topk_boundary(ctx, spec, run.lo, run.hi, run.best_high, run.want)?;
    let (a, b) = if run.best_high {
        (edge, run.hi)
    } else {
        (run.lo, edge)
    };
    let (mut part, scanned) = topk_read(ctx, spec, a, b, run.exact, vars)?;
    if run.lexical {
        // the prefixes read out of order are not read again, and the ids say nothing
        // about which rows can be cut
        run.done |= b == run.hi;
        run.lo = b.saturating_add(1);
        run.want = run.want.saturating_mul(4);
        if !run.extra.is_empty() {
            let _mask = ctx.charge(part.table.len() as u64)?;
            let keep: Vec<bool> = part.table.cols[0]
                .iter()
                .map(|id| !run.extra.contains(&id.0))
                .collect();
            part.table.filter_rows(&keep);
        }
        topk_append(ctx, &mut run.kept, part)?;
        return Ok(scanned);
    }
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
    let need = spec.k - run.kept.table.len();
    if part.table.len() >= need {
        let col = &part.table.cols[0];
        let (first, end) = if run.best_high {
            let cut = col[part.table.len() - need];
            (col.partition_point(|&id| id < cut), col.len())
        } else {
            let cut = col[need - 1];
            (0, col.partition_point(|&id| id <= cut))
        };
        for col in &mut part.table.cols {
            col.copy_within(first..end, 0);
            col.truncate(end - first);
        }
        part.table.len = end - first;
        run.done = true;
    }
    topk_append(ctx, &mut run.kept, part)?;
    Ok(scanned)
}

/// Whether ORDER BY's comparison of `id` with other values is a total order (it is not
/// for NaN, which compares with numbers by its lexical form, nor for partially ordered
/// dates, times and durations).
fn totally_ordered(ctx: &Ctx, id: Id) -> Result<bool> {
    use crate::id::Tag;
    match id.tag() {
        Tag::Double => Ok(!id.as_f64().is_nan()),
        Tag::DateTime | Tag::Date => Ok(false),
        Tag::Vocab | Tag::Delta | Tag::Local => {
            Ok(value_totally_ordered(ctx.value_checked(id)?.as_ref()))
        }
        _ => Ok(true),
    }
}

/// [`totally_ordered`] for a decoded dictionary value.
fn value_totally_ordered(value: Option<&Value>) -> bool {
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
fn topk_rest<'ctx>(
    ctx: &'ctx Ctx,
    spec: &OrderedTopK,
    vars: &[VarId],
    pieces: &[&super::plan::TopKPiece],
    scanned: &mut usize,
) -> Result<Option<Materialized<'ctx>>> {
    let mut rest = Materialized::empty_fast(ctx, vars, &[])?;
    for p in pieces {
        let (t, n) = topk_read(ctx, spec, p.lo, p.hi, p.exact, vars)?;
        *scanned += n;
        topk_append(ctx, &mut rest, t)?;
        ctx.check_output(rest.table.len(), rest.table.width())?;
    }
    // Up to `PAR_THRESHOLD` rows, decode the dictionary ORDER values once each in key
    // order, in one charged, operation-local pass, for the checks and the prefilter
    // below. A larger input reads them through the value cache in parallel.
    let decoded = if rest.table.len() <= PAR_THRESHOLD {
        super::exprcache::dictionary_values(ctx, &rest.table, &Expr::Var(spec.var))?
    } else {
        None
    };
    let total = match &decoded {
        Some(values) => rest_total(ctx, &rest.table, values)?,
        None => {
            if rest.table.len() > PAR_THRESHOLD / 4 {
                let not_total = std::sync::atomic::AtomicBool::new(false);
                rest.table.cols[0].par_iter().enumerate().try_for_each(
                    |(i, &id)| -> Result<()> {
                        if not_total.load(std::sync::atomic::Ordering::Relaxed) {
                            return Ok(());
                        }
                        if i.is_multiple_of(1024) {
                            ctx.check()?;
                        }
                        if !totally_ordered(ctx, id)? {
                            not_total.store(true, std::sync::atomic::Ordering::Relaxed);
                        }
                        Ok(())
                    },
                )?;
                ctx.check()?;
                !not_total.load(std::sync::atomic::Ordering::Relaxed)
            } else {
                let mut ok = true;
                for (i, &id) in rest.table.cols[0].iter().enumerate() {
                    if i.is_multiple_of(1024) {
                        ctx.check()?;
                    }
                    if !totally_ordered(ctx, id)? {
                        ok = false;
                        break;
                    }
                }
                ctx.check()?;
                ok
            }
        }
    };
    if !total {
        return Ok(None);
    }
    let keys = [(Expr::Var(spec.var), spec.asc)];
    // Rank the rounded values decoded above, rather than decoding the same dictionary
    // values again in the prefilter of `order_by`.
    if let Some(values) = &decoded
        && ctx.opt.topk_prefilter
        && rest.table.len() > spec.k
        && let Some((rows, charge)) =
            topk_candidates(ctx, &rest.table, &keys, spec.k, Some(values))?
        && rows.len() < rest.table.len()
    {
        ctx.check_output(rows.len(), rest.table.width())?;
        rest = topk_take_rows(ctx, &rest.table, &rows)?;
        drop(charge);
    }
    drop(decoded);
    // only their first k can be among the first k of all rows (the order of two rows
    // does not depend on the others)
    if rest.table.len() > spec.k {
        let (input, _input_charge) = rest.into_fast_parts()?;
        let credit = ctx.charge(topk_table_bytes(spec.k, vars.len()))?;
        let sorted = order_by(ctx, input, &keys, Some(spec.k), &mut ExprReport::default())?.0;
        rest = Materialized::with_fast(sorted, credit)?;
    }
    Ok(Some(rest))
}

/// Whether every value of the first column of `rest` is totally ordered, with its
/// dictionary values decoded in `values`.
fn rest_total(
    ctx: &Ctx,
    rest: &Table,
    values: &super::exprcache::PerValue<Option<Value>>,
) -> Result<bool> {
    for (i, value) in values.vals.iter().enumerate() {
        if i.is_multiple_of(1024) {
            ctx.check()?;
        }
        if !value_totally_ordered(value.as_ref()) {
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
        ) && !totally_ordered(ctx, id)?
        {
            return Ok(false);
        }
    }
    Ok(true)
}

/// The rows that the heap of [`topk_rest_morsels`] holds, a slot of the scan's ids for
/// each row.
struct RestSlots {
    width: usize,
    ids: Vec<Id>,
    free: Vec<u32>,
    key_bytes: u64,
}

/// The rows of one morsel offered to the heap of [`topk_rest_morsels`].
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

/// The rows a morsel of [`topk_rest_morsels`] keeps, the rows its scan produced and, when
/// the scan drops union-graph duplicates, the last of those rows.
type MorselBest<'ctx> = (Materialized<'ctx>, usize, Option<[Id; 4]>);

/// The keys `[first, last]` of one morsel of [`topk_rest_morsels`], the piece it belongs
/// to and the rows of its blocks.
struct Morsel {
    first: Key,
    last: Key,
    exact: bool,
    piece: usize,
}

/// [`topk_rest`] with a heap. The pieces are split into morsels of whole index blocks,
/// which are read in parallel. Each morsel keeps its rows that have fewer than k rows of
/// the morsel ahead of them, ties broken by arrival, in arrival order, and those rows are
/// then offered in order to one heap of the best k. A row that a morsel drops has k rows
/// of the same morsel at least as good that arrived before it, so the heap keeps the
/// rows that [`order_by`] keeps over the concatenated pieces. The query holds a morsel
/// per worker thread and the rows they keep, instead of every row of the pieces. A morsel
/// is checked for values that are not totally ordered before it ranks its rows, and one
/// such value sends the whole query to the generic plan, as it does eagerly.
fn topk_rest_morsels<'ctx>(
    ctx: &'ctx Ctx,
    spec: &OrderedTopK,
    vars: &[VarId],
    pieces: &[&super::plan::TopKPiece],
    scanned: &mut usize,
) -> Result<Option<Materialized<'ctx>>> {
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
    let mut morsel_list = ctx.retained_charge(0)?;
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
            if morsels.len() == morsels.capacity() {
                let next_cap = morsels.capacity().saturating_mul(2).max(8);
                if let Some(h) = &mut morsel_list {
                    h.resize((next_cap * std::mem::size_of::<Morsel>()) as u64)?;
                }
            }
            morsels.push(Morsel {
                first,
                last,
                exact: p.exact,
                piece,
            });
            if e >= b1 {
                break;
            }
            b = e;
        }
    }
    ctx.check()?;
    if let Some(h) = &mut morsel_list {
        h.resize((morsels.capacity() * std::mem::size_of::<Morsel>()) as u64)?;
    }
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
    let mut carried: Option<(usize, [Id; 4])> = None;
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
                && !t.table.is_empty()
                && t.table.cols.iter().zip(row).all(|(c, &id)| c[0] == id)
            {
                // a read of the whole piece drops this copy, as eager execution does
                let _rows = ctx.charge(t.table.len() as u64 * size_of::<usize>() as u64)?;
                let rows: Vec<usize> = (1..t.table.len()).collect();
                t = topk_take_rows(ctx, &t.table, &rows)?;
                n -= 1;
            }
            if let Some(last) = last {
                carried = Some((m.piece, last));
            }
            *scanned += n;
            if t.table.is_empty() {
                continue;
            }
            let room = heap.k().saturating_add(1).min(t.table.len()) as u64;
            if let Some(held) = &mut held {
                held.resize(
                    1024 + (heap.kept() as u64 + room) * entry_bytes
                        + slots.key_bytes
                        + slots.ids.capacity() as u64 * 8,
                )?;
            }
            let mut offer = RestOffer {
                slots: &mut slots,
                table: &t.table,
            };
            offer_rows(
                ctx,
                &mut heap,
                &t.table,
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
    let mut rest = Materialized::empty_fast(ctx, vars, &[])?;
    rest.reserve_rows(ctx, entries.len())?;
    for (j, col) in rest.table.cols.iter_mut().enumerate() {
        col.extend(
            entries
                .iter()
                .map(|e| slots.ids[e.payload as usize * width + j]),
        );
    }
    rest.table.len = entries.len();
    Ok(Some(rest))
}

/// The rows of one morsel of [`topk_rest_morsels`] that have fewer than k rows of the
/// morsel ahead of them, in arrival order, and the rows the morsel read; `None` when one
/// of its values is not totally ordered.
fn morsel_best<'ctx>(
    ctx: &'ctx Ctx,
    spec: &OrderedTopK,
    vars: &[VarId],
    keys: &[(Expr, bool)],
    m: &Morsel,
) -> Result<Option<MorselBest<'ctx>>> {
    let (mut t, n, last) = topk_read_keys_last(ctx, spec, m.first, m.last, m.exact, vars)?;
    if t.table.is_empty() {
        return Ok(Some((t, n, last)));
    }
    let decoded = super::exprcache::dictionary_values(ctx, &t.table, &Expr::Var(spec.var))?;
    let total = match &decoded {
        Some(values) => rest_total(ctx, &t.table, values)?,
        // without a decoded column, values are read one at a time, as eager execution
        // reads them
        None => {
            let mut ok = true;
            for (i, &id) in t.table.cols[0].iter().enumerate() {
                if i.is_multiple_of(1024) {
                    ctx.check()?;
                }
                if !totally_ordered(ctx, id)? {
                    ok = false;
                    break;
                }
            }
            ctx.check()?;
            ok
        }
    };
    if !total {
        return Ok(None);
    }
    if ctx.opt.topk_prefilter
        && t.table.len() > spec.k
        && let Some((rows, charge)) =
            topk_candidates(ctx, &t.table, keys, spec.k, decoded.as_ref())?
        && rows.len() < t.table.len()
    {
        t = topk_take_rows(ctx, &t.table, &rows)?;
        drop(charge);
    }
    drop(decoded);
    if t.table.len() > spec.k {
        let mut heap_rows = EagerHeapRows::new(ctx)?;
        let mut heap: TopK<usize> = TopK::new(spec.k, vec![spec.asc]);
        offer_rows(
            ctx,
            &mut heap,
            &t.table,
            keys,
            &mut ExprReport::default(),
            &mut heap_rows,
        )?;
        let mut rows: Vec<usize> = heap.into_sorted().into_iter().map(|e| e.payload).collect();
        drop(heap_rows);
        rows.sort_unstable();
        t = topk_take_rows(ctx, &t.table, &rows)?;
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
    ordered_topk_inner(ctx, spec, out, heap_eligible(ctx))
}

/// [`ordered_topk`] using morsel-wise heap selection for non-monotone pieces regardless
/// of `ctx.opt.topk_heap`, transferring the projected result directly into a pre-admitted
/// cursor [`OwnedCharge`](super::ctx::OwnedCharge) before releasing working charges.
pub(super) fn ordered_topk_morsels_charged(
    ctx: &std::sync::Arc<Ctx>,
    spec: &OrderedTopK,
    out: &[VarId],
) -> Result<Option<(Table, super::ctx::OwnedCharge, usize)>> {
    ordered_topk_inner_charged(ctx, spec, out, true)
}

fn ordered_topk_inner(
    ctx: &Ctx,
    spec: &OrderedTopK,
    out: &[VarId],
    morsels: bool,
) -> Result<Option<(Table, usize)>> {
    let mut table_out = None;
    let scanned = ordered_topk_run(ctx, spec, out, morsels, &mut |top| {
        table_out = Some(top.project(out));
        Ok(())
    })?;
    Ok(table_out.map(|t| (t, scanned)))
}

fn ordered_topk_inner_charged(
    ctx: &std::sync::Arc<Ctx>,
    spec: &OrderedTopK,
    out: &[VarId],
    morsels: bool,
) -> Result<Option<(Table, super::ctx::OwnedCharge, usize)>> {
    let mut result: Option<(Table, super::ctx::OwnedCharge)> = None;
    let scanned = ordered_topk_run(ctx, spec, out, morsels, &mut |top| {
        let len = top.len();
        let width = out.len();
        let mut charge = super::ctx::OwnedCharge::new(ctx, topk_table_bytes(len, width))?;
        let mut projected = Table::new(out.to_vec());
        for (col, &var) in projected.cols.iter_mut().zip(out) {
            col.reserve_exact(len);
            if let Some(src) = top.col_of(var) {
                col.extend_from_slice(&top.cols[src]);
            } else {
                col.resize(len, Id::UNDEF);
            }
        }
        projected.len = len;
        charge.resize(super::algosvc::table_capacity(&projected)?)?;
        result = Some((projected, charge));
        Ok(())
    })?;
    Ok(result.map(|(t, c)| (t, c, scanned)))
}

fn ordered_topk_run(
    ctx: &Ctx,
    spec: &OrderedTopK,
    _out: &[VarId],
    morsels: bool,
    finish: &mut dyn FnMut(Table) -> Result<()>,
) -> Result<usize> {
    let vars: Vec<VarId> = spec.scan.cols.iter().map(|c| c.1).collect();
    debug_assert_eq!(vars.first(), Some(&spec.var));
    let mut scanned = 0;
    let mut whole = Vec::new();
    let mut runs = Vec::new();
    for p in &spec.pieces {
        match p.mono {
            // the pieces without value order are read whole
            None if !p.lexical => whole.push(p),
            mono => runs.push(TopKRun {
                lo: p.lo,
                hi: p.hi,
                exact: p.exact,
                best_high: mono.is_some_and(|up| up != spec.asc),
                kept: Materialized::empty_fast(ctx, &vars, &[])?,
                want: spec.k.max(16).saturating_mul(2),
                done: false,
                lexical: mono.is_none(),
                extra: Vec::new(),
                hold: false,
            }),
        }
    }
    let rest = if morsels {
        topk_rest_morsels(ctx, spec, &vars, &whole, &mut scanned)?
    } else {
        topk_rest(ctx, spec, &vars, &whole, &mut scanned)?
    };
    let Some(rest) = rest else {
        return Ok(scanned);
    };
    let keys = [(Expr::Var(spec.var), spec.asc)];
    let better = if spec.asc {
        Ordering::Less
    } else {
        Ordering::Greater
    };
    loop {
        ctx.check()?;
        for run in runs.iter_mut().filter(|r| !r.done) {
            if std::mem::take(&mut run.hold) {
                continue;
            }
            scanned += topk_step(ctx, spec, run, &vars)?;
        }
        // the candidates in the generic plan's row order, ranked by the generic ORDER BY
        let n = rest.table.len() + runs.iter().map(|r| r.kept.table.len()).sum::<usize>();
        ctx.check_output(n, vars.len())?;
        let cand_est = topk_table_bytes(n, vars.len());
        let cand_charge = ctx.charge(cand_est)?;
        let mut cand = Table::new(vars.clone());
        for col in &mut cand.cols {
            col.reserve_exact(n);
        }
        for (dst, src) in cand.cols.iter_mut().zip(&rest.table.cols) {
            dst.extend_from_slice(src);
        }
        cand.len = rest.table.len();
        for run in &runs {
            for (dst, src) in cand.cols.iter_mut().zip(&run.kept.table.cols) {
                dst.extend_from_slice(src);
            }
            cand.len += run.kept.table.len();
        }
        cand.sorted.clear();
        {
            let sort_cols = spec.tie_order.len() as u64 * size_of::<usize>() as u64;
            let sort_rows = n as u64 * size_of::<usize>() as u64;
            let _ties = ctx.charge(cand_est.saturating_add(sort_cols).saturating_add(sort_rows))?;
            cand.sort_by_vars(&spec.tie_order);
        }
        let top_charge = ctx.charge(topk_table_bytes(n.min(spec.k), vars.len()))?;
        let (top, _) = order_by(ctx, cand, &keys, Some(spec.k), &mut ExprReport::default())?;
        drop(cand_charge);
        if top.len() == spec.k {
            // a piece is finished once the k-th row is strictly better than its best
            // unread value, and so than every unread value
            let kth_credit = ctx.charge(0)?;
            let kth = ctx.value_checked_into(top.cols[0][spec.k - 1], &kth_credit)?;
            for run in runs.iter_mut().filter(|r| !r.done) {
                if run.lexical {
                    match lexical_check(ctx, spec, run, kth.as_ref())? {
                        Lexical::Done => run.done = true,
                        Lexical::Step => {}
                        Lexical::Read(ids) => {
                            for id in ids {
                                let (t, n) = topk_read(ctx, spec, id, id, run.exact, &vars)?;
                                scanned += n;
                                topk_append(ctx, &mut run.kept, t)?;
                                run.extra.push(id);
                            }
                            run.hold = true;
                        }
                        Lexical::Generic => return Ok(scanned),
                    }
                    continue;
                }
                let best = Id(if run.best_high { run.hi } else { run.lo });
                let best_credit = ctx.charge(0)?;
                let best_v = ctx.value_checked_into(best, &best_credit)?;
                if order_cmp(kth.as_ref(), best_v.as_ref()) == better {
                    run.done = true;
                }
                drop(best_v);
                drop(best_credit);
            }
            drop(kth);
            drop(kth_credit);
        }
        if runs.iter().all(|r| r.done) {
            finish(top)?;
            drop(top_charge);
            return Ok(scanned);
        }
    }
}

/// Append the scan's rows with keys in `[lo, hi]`, stopping at the row limit.
fn scan_into(
    ctx: &Ctx,
    spec: &ScanSpec,
    lo: crate::index::Key,
    hi: crate::index::Key,
    limit: Option<usize>,
    t: &mut Materialized<'_>,
) -> Result<bool> {
    let limit = limit.unwrap_or(usize::MAX);
    let mut truncated = false;
    let mut last: Option<[u64; 4]> = None;
    let mut n = 0usize;
    let mut row = |k: &[u64; 4], t: &mut Materialized<'_>| -> Result<()> {
        if t.table.len >= limit || !spec.graph.accepts(k[spec.graph_col]) {
            return Ok(());
        }
        if spec.eqs.iter().any(|&(a, b)| k[a] != k[b]) {
            return Ok(());
        }
        if spec.dedup {
            let mut proj = [0u64; 4];
            for (i, &(kc, _)) in spec.cols.iter().enumerate() {
                proj[i] = k[kc];
            }
            if last == Some(proj) {
                return Ok(());
            }
            last = Some(proj);
        }
        let rows = t
            .table
            .len
            .checked_add(1)
            .ok_or_else(|| Error::invalid("scan output rows overflow"))?;
        ctx.check_output_rows(rows)?;
        t.reserve_rows(ctx, rows)?;
        let table = &mut t.table;
        for (c, &(kc, _)) in spec.cols.iter().enumerate() {
            table.cols[c].push(Id(k[kc]));
        }
        table.len = rows;
        Ok(())
    };
    ctx.snap
        .scan_between_cols(spec.perm, lo, hi, scan_mask(spec), |chunk| {
            if t.table.len >= limit {
                truncated = true;
                return Ok(false);
            }
            match chunk {
                Chunk::Block(b, s, e) if !spec.dedup && block_passes(spec, b, s, e) => {
                    let e = e.min(s.saturating_add(limit - t.table.len));
                    let rows = t
                        .table
                        .len
                        .checked_add(e - s)
                        .ok_or_else(|| Error::invalid("scan output rows overflow"))?;
                    ctx.check_output_rows(rows)?;
                    t.reserve_rows(ctx, rows)?;
                    let table = &mut t.table;
                    for (c, &(kc, _)) in spec.cols.iter().enumerate() {
                        table.cols[c].extend(b.cols[kc][s..e].iter().map(|&x| Id(x)));
                    }
                    table.len = rows;
                    n += e - s;
                }
                Chunk::Block(b, s, e) => {
                    for i in s..e {
                        row(&b.key(i), t)?;
                    }
                    n += e - s;
                }
                Chunk::Row(k) => {
                    row(&k, t)?;
                    n += 1;
                }
            }
            if n > 1 << 16 {
                n = 0;
                ctx.check()?;
                ctx.check_output_rows(t.table.len())?;
            }
            if t.table.len >= limit {
                truncated = true;
                return Ok(false);
            }
            Ok(true)
        })?;
    Ok(truncated)
}

// ------------------------------------------------------------------ joins ------

/// Run `join` over the tables `l` and `r` of the binary join `n`, and when it fails on a
/// memory budget that a query with spill answers by spilling, join them again as the
/// cursor's partitioned hash join over the two tables, so that an eager join whose
/// build does not fit spills instead of failing.
fn join_or_spill(
    ctx: &Ctx,
    n: &Node,
    l: Table,
    r: Table,
    note: &mut Option<String>,
    join: impl FnOnce(&Table, &Table, &mut Option<String>) -> Result<Table>,
) -> Result<Table> {
    match join(&l, &r, note) {
        Err(error) if hash::spills(ctx, &error) => spilled_join(ctx, n, l, r, note, error),
        other => other,
    }
}

/// The join `n` of the tables `l` and `r` through the cursor's partitioned hash join,
/// after the eager join failed with `error`. A context that is not shared, or a join
/// the cursor cannot run, keeps the eager failure.
fn spilled_join(
    ctx: &Ctx,
    n: &Node,
    l: Table,
    r: Table,
    note: &mut Option<String>,
    error: Error,
) -> Result<Table> {
    let Some(shared) = ctx.shared() else {
        return Err(error);
    };
    let leaf = values_leaf;
    let mut node = Node::leaf(n.kind.clone(), n.vars.clone(), n.est, String::new());
    node.certain.clone_from(&n.certain);
    node.children = vec![leaf(l), leaf(r)];
    let mut out = Table::new(n.vars.clone());
    match super::cursor::spilled_join(&shared, node, &mut out) {
        Ok(()) => {}
        // the cursor could not run the join, so the eager failure stands
        Err(Error::Unsupported(_)) => return Err(error),
        Err(other) => return Err(other),
    }
    let spilled = "[build did not fit: partitioned to disk]";
    *note = Some(match note.take() {
        Some(note) => format!("{note} {spilled}"),
        None => spilled.to_string(),
    });
    Ok(out)
}

/// A VALUES leaf that holds the table `t`, with the order and the bound variables it has,
/// for an eager operator that falls back to its cursor twin over its input tables.
fn values_leaf(t: Table) -> Node {
    let certain: Vec<VarId> = t
        .vars
        .iter()
        .enumerate()
        .filter(|&(c, _)| !has_undef(&t, c))
        .map(|(_, v)| *v)
        .collect();
    let sorted = t.sorted.clone();
    let vars = t.vars.clone();
    let est = t.len() as f64;
    let mut leaf = Node::leaf(Kind::Values(t), vars, est, String::new());
    leaf.certain = certain;
    leaf.sorted = sorted;
    leaf
}

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

/// The rows of a join from left row indices `li` and right row indices `rj` (`NO_ROW`
/// for none). The left side's unbound shared variables take the right row's values. Each
/// column is allocated at exactly the row count, since the algorithm join admits the
/// table's bytes before it is built and a column grown from empty would take the
/// allocator's minimum capacity instead for a few rows.
fn materialize(l: &Table, r: &Table, lay: &JoinLayout, li: &[u32], rj: &[u32]) -> Table {
    let mut t = Table::new(lay.vars.clone());
    for col in &mut t.cols {
        col.reserve_exact(li.len());
    }
    let columns = hash::columns(&lay.vars, &l.vars, &r.vars);
    gather(&mut t, &columns, l, r, li, rj);
    t
}

fn has_undef(t: &Table, c: usize) -> bool {
    t.cols[c].iter().any(|v| v.is_undef())
}

/// Run `piece` over the rows `0..n` of a probe in pieces of [`PROBE_PIECE`] rows, in
/// parallel when there are at least two, and collect the pieces in row order.
fn probe_pieces<T: Send>(
    n: usize,
    piece: impl Fn(usize, usize) -> Result<T> + Sync,
) -> Result<Vec<T>> {
    if n < 2 * PROBE_PIECE {
        return Ok(vec![piece(0, n)?]);
    }
    (0..n.div_ceil(PROBE_PIECE))
        .into_par_iter()
        .map(|p| piece(p * PROBE_PIECE, ((p + 1) * PROBE_PIECE).min(n)))
        .collect()
}

/// The pairs of an inner or OPTIONAL join without a condition over the whole probe
/// table of `join`, as (probe rows, build rows), found in parallel pieces. The pairs of
/// all pieces are checked against the output limits of `w` columns as they grow.
fn hash_pairs(ctx: &Ctx, join: &Join, w: usize) -> Result<(Vec<u32>, Vec<u32>)> {
    let total = AtomicUsize::new(0);
    let pieces = probe_pieces(join.probe.len(), |s, e| {
        let mut at = Probe::new(s..e);
        let (mut lp, mut rp) = (Vec::new(), Vec::new());
        let mut counted = 0;
        while at.row < e {
            ctx.check()?;
            join.pairs(ctx, &mut at, PROBE_PIECE, &mut lp, &mut rp)?;
            let n = lp.len() - counted;
            counted = lp.len();
            let all = total.fetch_add(n, AtomicOrdering::Relaxed) + n;
            ctx.check_output(all, w)?;
        }
        Ok((lp, rp))
    })?;
    Ok(match pieces.len() {
        1 => pieces.into_iter().next().expect("one piece"),
        _ => {
            let n = pieces.iter().map(|(lp, _)| lp.len()).sum();
            let (mut lp, mut rp) = (Vec::with_capacity(n), Vec::with_capacity(n));
            for (l, r) in pieces {
                lp.extend_from_slice(&l);
                rp.extend_from_slice(&r);
            }
            (lp, rp)
        }
    })
}

/// The candidate pairs a join with a condition tests at a time.
pub(super) const KEYED_CHUNK: usize = 1 << 16;

/// The pairs of a join with a condition over the whole probe table of `join`, as (probe
/// rows, build rows), with the number of candidate pairs the condition was tested on.
/// The condition is tested on the merged rows of a chunk of candidates at a time, and a
/// chunk takes at most a quarter of the room the query has left.
fn hash_condition(
    ctx: &Ctx,
    join: &Join,
    lay: &JoinLayout,
    expr: &Expr,
) -> Result<(Vec<u32>, Vec<u32>, u64)> {
    let w = lay.vars.len() + 1;
    let columns = hash::columns(&lay.vars, &join.probe.vars, &join.build.vars);
    let per_pair = 8 + w as u64 * 8 + 1;
    let room = usize::try_from(ctx.memory_room() / 4 / per_pair).unwrap_or(usize::MAX);
    let cap = KEYED_CHUNK.min(room).max(1);
    let mut at = Probe::new(0..join.probe.len());
    let (mut lp, mut rp) = (Vec::new(), Vec::new());
    let mut candidates = 0u64;
    while at.row < at.end {
        ctx.check()?;
        candidates += join.condition(
            ctx, &mut at, &columns, &lay.vars, expr, cap, &mut lp, &mut rp,
        )? as u64;
        ctx.check_output(lp.len(), w)?;
    }
    Ok((lp, rp, candidates))
}

/// Which probe rows of `join`, a semi, anti or MINUS join, pass, found in parallel
/// pieces.
fn hash_filter(ctx: &Ctx, join: &Join) -> Result<Vec<bool>> {
    let n = join.probe.len();
    let pieces = probe_pieces(n, |s, e| {
        let mut at = Probe::new(s..e);
        let mut lp = Vec::new();
        while at.row < e {
            ctx.check()?;
            join.filter(ctx, &mut at, PROBE_PIECE, false, &mut lp)?;
        }
        Ok(lp)
    })?;
    let mut keep = vec![false; n];
    for lp in pieces {
        for row in lp {
            keep[row as usize] = true;
        }
    }
    Ok(keep)
}

/// The hash table of an eager join's build side: the table itself, the probe side's
/// composite keys when the table is keyed on several columns, and the charge of the
/// keys while the probe runs.
struct EagerIndex<'a> {
    index: Option<HashIndex>,
    probe_keys: Option<Vec<Id>>,
    _keys: Option<super::ctx::Charge<'a>>,
}

impl EagerIndex<'_> {
    /// The kernel's view of the probe table `probe` against `build` in `mode`.
    fn join<'a>(
        &'a self,
        mode: Mode,
        probe: &'a Table,
        build: &'a Table,
        shared: &'a [(usize, usize)],
    ) -> Join<'a> {
        Join {
            mode,
            probe,
            build,
            shared,
            index: self.index.as_ref(),
            probe_keys: self.probe_keys.as_deref(),
            equality: None,
        }
    }
}

/// The hash table of `build` for the shared columns `shared` (probe column, build
/// column). Two or more shared columns that neither side leaves unbound key the table
/// on a hash of them all, so that a probe row meets about the rows of its own key, and
/// the kernel checks the columns themselves on each candidate. Otherwise the table is
/// keyed on the one column the kernel chooses, or there is none and every probe row
/// visits every build row. The note names the table.
fn hash_index<'a>(
    ctx: &'a Ctx,
    probe: &Table,
    build: &Table,
    shared: &[(usize, usize)],
    note: &mut Option<String>,
) -> Result<EagerIndex<'a>> {
    if build.is_empty() {
        // nothing to find: every probe row visits the no rows there are
        return Ok(EagerIndex {
            index: None,
            probe_keys: None,
            _keys: None,
        });
    }
    let exact: Vec<(usize, usize)> = shared
        .iter()
        .copied()
        .filter(|&(p, b)| !has_undef(probe, p) && !has_undef(build, b))
        .collect();
    let (index, probe_keys, keys) = if let [(p, b)] = exact[..] {
        // the one column neither side leaves unbound is the key
        (Some(HashIndex::new(ctx, build, b, p)?), None, None)
    } else if exact.len() > 1 {
        let keys = ctx.charge((probe.len() + build.len()) as u64 * 8 + 64)?;
        let composite = |t: &Table, cols: &[usize]| -> Result<Vec<Id>> {
            map_rows(ctx, t.len(), t.len() >= PAR_THRESHOLD, |i| {
                use std::hash::{Hash, Hasher};
                let mut h = rustc_hash::FxHasher::default();
                for &c in cols {
                    t.cols[c][i].0.hash(&mut h);
                }
                Id(h.finish())
            })
        };
        let bcols: Vec<usize> = exact.iter().map(|&(_, b)| b).collect();
        let pcols: Vec<usize> = exact.iter().map(|&(p, _)| p).collect();
        let build_keys = composite(build, &bcols)?;
        let index = HashIndex::from_keys(ctx, &build_keys, usize::MAX)?;
        drop(build_keys);
        (Some(index), Some(composite(probe, &pcols)?), Some(keys))
    } else {
        match hash::key_column(ctx, probe, build, shared)? {
            Some((p, b)) => (Some(HashIndex::new(ctx, build, b, p)?), None, None),
            None => (None, None, None),
        }
    };
    if let Some(index) = &index {
        *note = Some(format!(
            "[flat hash table: {} keys in {} parts]",
            index.keys(),
            index.partitions()
        ));
    }
    Ok(EagerIndex {
        index,
        probe_keys,
        _keys: keys,
    })
}

/// Pairs of compatible rows (inner join), as (left rows, right rows).
fn join_pairs(
    ctx: &Ctx,
    l: &Table,
    r: &Table,
    lay: &JoinLayout,
    merge: bool,
    note: &mut Option<String>,
) -> Result<(Vec<u32>, Vec<u32>)> {
    // a pair per output row now, the materialized row later
    let w = lay.vars.len() + 1;
    if lay.shared.is_empty() {
        // the product size is known up front: reject it before allocating anything
        let n = l.len().saturating_mul(r.len());
        ctx.check_output(n, w)?;
        let (mut li, mut rj) = (Vec::with_capacity(n), Vec::with_capacity(n));
        for i in 0..l.len() {
            if i % 1024 == 0 {
                ctx.check()?;
            }
            li.extend(std::iter::repeat_n(i as u32, r.len()));
            rj.extend(0..r.len() as u32);
        }
        return Ok((li, rj));
    }
    // key columns without UNDEF on both sides can be matched by equality
    let exact: Vec<(usize, usize)> = lay
        .shared
        .iter()
        .copied()
        .filter(|&(lc, rc)| !has_undef(l, lc) && !has_undef(r, rc))
        .collect();
    if merge
        && let Some(&(lk, rk)) = exact.first()
        && sorted_on(l, lk)
        && sorted_on(r, rk)
    {
        return merge_pairs(ctx, l, r, lay, (lk, rk), w);
    }
    // hash join: build on the smaller side, probe preserves the larger side's order
    let build_left = l.len() < r.len();
    let (probe, build) = if build_left { (r, l) } else { (l, r) };
    let shared: Vec<(usize, usize)> = lay
        .shared
        .iter()
        .map(|&(lc, rc)| if build_left { (rc, lc) } else { (lc, rc) })
        .collect();
    let index = hash_index(ctx, probe, build, &shared, note)?;
    let join = index.join(Mode::Inner, probe, build, &shared);
    let (lp, rp) = hash_pairs(ctx, &join, w)?;
    Ok(if build_left { (rp, lp) } else { (lp, rp) })
}

/// The pairs of a merge join of `l` and `r` on the key columns `(lk, rk)`, which both
/// sides are sorted on and neither leaves unbound, as (left rows, right rows). Rows of
/// the two sides that agree on the key are paired run by run, and pairs that disagree on
/// another shared variable are dropped.
fn merge_pairs(
    ctx: &Ctx,
    l: &Table,
    r: &Table,
    lay: &JoinLayout,
    (lk, rk): (usize, usize),
    w: usize,
) -> Result<(Vec<u32>, Vec<u32>)> {
    let (a, b) = (&l.cols[lk], &r.cols[rk]);
    // most merge joins match each row of the smaller side about once
    let (mut li, mut rj) = (
        Vec::with_capacity(a.len().min(b.len())),
        Vec::with_capacity(a.len().min(b.len())),
    );
    let admit = |pairs: usize| ctx.check_output(pairs, w);
    if lay.shared.len() > 1 {
        let same = |i: usize, j: usize| compatible(l, r, i, j, &lay.shared);
        merge::zip(ctx, a, b, false, same, admit, &mut li, &mut rj)?;
    } else {
        merge::zip(ctx, a, b, true, |_, _| true, admit, &mut li, &mut rj)?;
    }
    ctx.check_output(li.len(), w)?;
    Ok((li, rj))
}

/// Every left row of the sorted key column `a` with the right rows of the sorted key
/// column `b` that hold its key, or alone with [`NO_ROW`] when `optional`, as (left
/// rows, right rows). The pairs are checked against the output limits of `w` columns
/// a piece at a time, and a run too long for a piece is checked before it is expanded.
fn merge_expand(
    ctx: &Ctx,
    a: &[Id],
    b: &[Id],
    optional: bool,
    w: usize,
    compatible: impl Fn(usize, usize) -> bool,
) -> Result<(Vec<u32>, Vec<u32>)> {
    // `first[i]`: the first right row whose key is at least left row i's
    let _bounds = ctx.charge(a.len() as u64 * 4 + 64)?;
    let first = merge::bounds(ctx, a, b)?;
    // most merge joins match each row of the smaller side about once
    let (mut li, mut rj) = (
        Vec::with_capacity(a.len().min(b.len())),
        Vec::with_capacity(a.len().min(b.len())),
    );
    let mut from = 0;
    while from < a.len() {
        ctx.check()?;
        let done = merge::expand(
            ctx,
            a,
            b,
            &first,
            from,
            optional,
            PROBE_PIECE,
            &compatible,
            &mut li,
            &mut rj,
        )?;
        if done == 0 {
            // a run longer than a piece: expand it a piece at a time, checking the pairs
            // it gives against the output limits as they grow
            let at = first[from] as usize;
            let end = run_end(b, at);
            let before = li.len();
            let mut pos = at;
            while pos < end {
                ctx.check()?;
                pos = merge::expand_run(
                    ctx,
                    from,
                    pos..end,
                    PROBE_PIECE,
                    &compatible,
                    &mut li,
                    &mut rj,
                )?;
                ctx.check_output(li.len(), w)?;
            }
            if optional && li.len() == before {
                li.push(from as u32);
                rj.push(NO_ROW);
            }
            from += 1;
        } else {
            from += done;
        }
        ctx.check_output(li.len(), w)?;
    }
    Ok((li, rj))
}

/// Number of join results; merge joins on a single exact key count run products.
pub(super) fn join_count(ctx: &Ctx, l: &Table, r: &Table, merge: bool) -> Result<u64> {
    let lay = layout(l, r);
    if merge && lay.shared.len() == 1 {
        let (lk, rk) = lay.shared[0];
        let lsorted = l.sorted.first().is_some_and(|v| l.col_of(*v) == Some(lk));
        let rsorted = r.sorted.first().is_some_and(|v| r.col_of(*v) == Some(rk));
        if lsorted && rsorted && !has_undef(l, lk) && !has_undef(r, rk) {
            let (a, b) = (&l.cols[lk], &r.cols[rk]);
            let _bounds = ctx.charge(a.len() as u64 * 4 + 64)?;
            let first = merge::bounds(ctx, a, b)?;
            return merge::count(ctx, a, b, &first, false);
        }
    }
    Ok(join_pairs(ctx, l, r, &lay, merge, &mut None)?.0.len() as u64)
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
        let decode = ctx.charge(0)?;
        let Some(oxrdf::Term::Triple(tr)) = ctx.term_checked_into(input.cols[tc][i], &decode)?
        else {
            continue;
        };
        let comps = [
            ctx.intern_term(&tr.subject.clone().into()),
            ctx.intern_term(&oxrdf::Term::NamedNode(tr.predicate.clone())),
            ctx.intern_term(&tr.object),
        ];
        drop(tr);
        drop(decode);
        ctx.check()?;
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
    ctx.check()?;
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
pub(super) fn join_algorithm_tables(
    ctx: &Ctx,
    l: &Table,
    r: &Table,
) -> Result<Materialized<'static>> {
    use crate::algo::memory;
    let lay = layout(l, r);
    let (li, rj) = join_pairs(ctx, l, r, &lay, false, &mut None)?;
    ctx.check_output(li.len(), lay.vars.len() + 1)?;
    let sorted_len = kept_order_iter(l, r, &lay, &li, &rj).count();
    let col = memory::bytes::<Id>(li.len())?;
    let bytes = memory::sum(&[
        col.checked_mul(lay.vars.len() as u64)
            .ok_or_else(|| Error::invalid("algorithm join capacity overflows"))?,
        memory::bytes::<VarId>(lay.vars.len())?,
        memory::bytes::<Vec<Id>>(lay.vars.len())?,
        memory::bytes::<VarId>(sorted_len)?,
        128,
        if lay.vars.is_empty() { col } else { 0 },
    ])?;
    let mut out =
        Materialized::with_lease(Table::empty(Vec::new()), Some(ctx.output_lease(bytes)?));
    out.table = materialize(l, r, &lay, &li, &rj);
    let ctl = crate::algo::Control::with_cancellation(
        ctx.cancel.clone(),
        ctx.cancellation.clone(),
        ctx.current_deadline(),
        u64::MAX,
    );
    let mut sorted = memory::allocate("algorithm-join-order", sorted_len, &ctl)?;
    for v in kept_order_iter(l, r, &lay, &li, &rj) {
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
    let (li, rj) = join_pairs(ctx, l, r, &lay, merge, note)?;
    ctx.check_output(li.len(), lay.vars.len() + 1)?;
    let mut t = materialize(l, r, &lay, &li, &rj);
    t.sorted = kept_order(l, r, &lay, &li, &rj);
    Ok(t)
}

/// The sort order that joined rows keep from an input. A hash join emits its pairs in
/// the order of the side it probes, so that side's sort variables stay sorted, up to the
/// first one shared with the other side where the side holds unbound values (the other
/// side fills them).
fn kept_order(l: &Table, r: &Table, lay: &JoinLayout, li: &[u32], rj: &[u32]) -> Vec<VarId> {
    kept_order_iter(l, r, lay, li, rj).collect()
}
fn kept_order_iter<'a>(
    l: &'a Table,
    r: &'a Table,
    lay: &'a JoinLayout,
    li: &[u32],
    rj: &[u32],
) -> impl Iterator<Item = VarId> + 'a {
    let left_ordered = li.windows(2).all(|w| w[0] <= w[1]);
    let right_ordered = rj.windows(2).all(|w| w[0] <= w[1]);
    let (t, left) = if left_ordered {
        (l, true)
    } else if right_ordered {
        (r, false)
    } else {
        (l, false)
    };
    let sorted = if left_ordered || right_ordered {
        &t.sorted[..]
    } else {
        &[]
    };
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
        let key = string_lexical_boxed(ctx, id, &index_charge)?.expect("proved string domain");
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
        let (begin, end) =
            with_string_lexical(ctx, id, |s| Ok(range(s)))?.expect("proved string domain");
        rows = rows.saturating_add(end - begin);
        ctx.check_output_rows(rows)?;
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
        let (begin, end) =
            with_string_lexical(ctx, id, |s| Ok(range(s)))?.expect("proved string domain");
        for (_, j) in &index[begin..end] {
            if pairs.len().is_multiple_of(1024) {
                ctx.check()?;
            }
            pairs.push((i as u32, *j as u32));
        }
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

/// Read a borrowed lexical key with controlled vocabulary scratch and live request
/// controls. `f` must only inspect the borrowed string without calling `ctx.charge()`.
/// Unlike `Ctx::value` this does not populate the eager decoded-value cache.
fn with_string_lexical<T>(
    ctx: &Ctx,
    id: Id,
    mut f: impl FnMut(&str) -> Result<T>,
) -> Result<Option<T>> {
    ctx.with_controlled_key(id, |key| match string_lexical_key(key) {
        Some(s) => f(s).map(Some),
        None => Ok(None),
    })
    .map(Option::flatten)
}

/// Copy one string lexical form into a boxed string, admitting its bytes onto `credit`
/// before allocating and never calling `credit.add` while holding a delta or local lock.
fn string_lexical_boxed(
    ctx: &Ctx,
    id: Id,
    credit: &super::ctx::Charge<'_>,
) -> Result<Option<Box<str>>> {
    use crate::id::Tag;
    ctx.check()?;
    match id.tag() {
        Tag::Vocab => ctx
            .with_controlled_vocab_key(id.payload(), |key| {
                let Some(s) = string_lexical_key(key) else {
                    return Ok(None);
                };
                credit.add(s.len() as u64)?;
                ctx.check()?;
                Ok(Some(Box::<str>::from(s)))
            })
            .map(Option::flatten),
        Tag::Delta | Tag::Local => {
            let Some(len) = with_string_lexical(ctx, id, |s| Ok(s.len()))? else {
                return Ok(None);
            };
            credit.add(len as u64)?;
            ctx.check()?;
            with_string_lexical(ctx, id, |s| Ok(Box::<str>::from(s)))
        }
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
    let (li, rj) = join_pairs(ctx, l, r, &lay, false, &mut None)?;
    Ok(materialize(l, r, &lay, &li, &rj))
}

/// Whether `t` is sorted on its column `c` first.
fn sorted_on(t: &Table, c: usize) -> bool {
    t.sorted.first().is_some_and(|v| t.col_of(*v) == Some(c))
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
    let index = hash_index(ctx, l, r, &lay.shared, note)?;
    let join = index.join(Mode::Optional, l, r, &lay.shared);
    let (li, rj) = match expr {
        Some(e) => {
            let (li, rj, _) = hash_condition(ctx, &join, &lay, e)?;
            (li, rj)
        }
        None => hash_pairs(ctx, &join, lay.vars.len() + 1)?,
    };
    Ok(materialize(l, r, &lay, &li, &rj))
}

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
    let lay = layout(l, r);
    let generic = |note: &mut Option<String>| {
        if optional {
            left_join(ctx, l, r, Some(expr), note)
        } else {
            let mut t = cross(ctx, l, r)?;
            let keep = condition_mask(ctx, &t, expr)?;
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
    let index = EqualityIndex::new(ctx, r, rc, lc)?;
    let join = Join {
        mode: if optional {
            Mode::Optional
        } else {
            Mode::Inner
        },
        probe: l,
        build: r,
        shared: &[],
        index: None,
        probe_keys: None,
        equality: Some((lc, &index)),
    };
    let (li, rj, candidates) = hash_condition(ctx, &join, &lay, expr)?;
    *note = Some(format!(
        "[keyed on ?{} = ?{}: {} candidate pairs]",
        ctx.var_name(a),
        ctx.var_name(b),
        candidates
    ));
    Ok(materialize(l, r, &lay, &li, &rj))
}

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
    let w = lay.vars.len() + 1;
    // each left row with the right rows of its key, or with none
    let (mut li, mut rj) = merge_expand(ctx, a, b, true, w, |_, _| true)?;
    if let Some(e) = expr {
        // the FILTER is tested on the matched rows; a left row none of whose matches
        // pass is kept alone
        let (matched_l, matched_r): (Vec<u32>, Vec<u32>) = li
            .iter()
            .zip(&rj)
            .filter(|(_, j)| **j != NO_ROW)
            .map(|(&i, &j)| (i, j))
            .unzip();
        let joined = materialize(l, r, lay, &matched_l, &matched_r);
        let mut keep = condition_mask(ctx, &joined, e)?.into_iter();
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
    let mut t = materialize(l, r, lay, &li, &rj);
    t.sorted = l.sorted.clone();
    Ok(t)
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
        let decode = ctx.charge(0)?;
        let a = ctx.term_checked_into(old, &decode)?;
        let b = ctx.term_checked_into(new, &decode)?;
        keep[i] = match (a, b) {
            (Some(a), Some(b)) => super::cdt::same_value(&a, &b).unwrap_or(false),
            _ => false,
        };
    }
    ctx.check()?;
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
    let seen_charge = ctx.charge(0)?;
    let mut seen: FxHashMap<Id, Option<Vec<(Id, Id)>>> = FxHashMap::default();
    for (i, id) in col.into_iter().enumerate() {
        if i % 4096 == 0 {
            ctx.check()?;
            ctx.check_output(out.len(), out.width())?;
        }
        if let std::collections::hash_map::Entry::Vacant(e) = seen.entry(id) {
            let decode = ctx.charge(0)?;
            let entry = match ctx.term_checked_into(id, &decode)? {
                Some(t) => match super::cdt::unfold(&t) {
                    Some(parts) => {
                        let entry_bytes =
                            (parts.len() as u64).saturating_mul(16).saturating_add(64);
                        seen_charge.add(entry_bytes)?;
                        let mut pairs = Vec::with_capacity(parts.len());
                        for (a, b) in parts {
                            let ia = a.map_or(Id::UNDEF, |x| ctx.intern_term(&x));
                            let ib = b.map_or(Id::UNDEF, |x| ctx.intern_term(&x));
                            ctx.check()?;
                            pairs.push((ia, ib));
                        }
                        Some(pairs)
                    }
                    None => {
                        seen_charge.add(64)?;
                        None
                    }
                },
                None => {
                    seen_charge.add(64)?;
                    None
                }
            };
            e.insert(entry);
            drop(decode);
        }
        let elems = &seen[&id];
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
    ctx.check()?;
    ctx.check_output(out.len(), out.width())?;
    drop(seen);
    drop(seen_charge);
    Ok(out)
}

/// ARQ's SEMIJOIN and ANTIJOIN (`QueryIterHalfJoin`): the rows of `l` compatible with a
/// row of `r` (with none, when `anti`), each once and unchanged. Unlike MINUS, a row of
/// `r` that shares no bound variable with a row of `l` is compatible with it.
fn half_join(ctx: &Ctx, l: &Table, r: &Table, anti: bool) -> Result<Table> {
    let lay = layout(l, r);
    let index = hash_index(ctx, l, r, &lay.shared, &mut None)?;
    let join = index.join(
        if anti { Mode::Anti } else { Mode::Semi },
        l,
        r,
        &lay.shared,
    );
    let keep = hash_filter(ctx, &join)?;
    drop(index);
    kept_rows(ctx, l, &keep)
}

/// The rows of `l` that `keep` marks, in order, with its sort order.
fn kept_rows(ctx: &Ctx, l: &Table, keep: &[bool]) -> Result<Table> {
    let n = keep.iter().filter(|k| **k).count();
    ctx.check_output(n, l.width())?;
    let mut t = Table::new(l.vars.clone());
    for (column, source) in t.cols.iter_mut().zip(&l.cols) {
        column.reserve_exact(n);
        column.extend(
            source
                .iter()
                .zip(keep)
                .filter(|(_, k)| **k)
                .map(|(v, _)| *v),
        );
    }
    t.len = n;
    t.sorted = l.sorted.clone();
    Ok(t)
}

fn minus(ctx: &Ctx, l: &Table, r: &Table, note: &mut Option<String>) -> Result<Table> {
    let lay = layout(l, r);
    if lay.shared.is_empty() || r.is_empty() {
        return Ok(l.clone());
    }
    // One shared variable that neither side leaves unbound: compatibility is equality
    // of the key, so a merge of two sorted sides or the hash kernel decides each row.
    let anti = match lay.shared[..] {
        [(lc, rc)] if ctx.opt.anti_join && !has_undef(l, lc) && !has_undef(r, rc) => Some((lc, rc)),
        _ => None,
    };
    let keep = match anti {
        Some((lc, rc)) if sorted_on(l, lc) && sorted_on(r, rc) => {
            *note = Some(format!(
                "[anti-join on ?{} by merge]",
                ctx.var_name(l.vars[lc])
            ));
            merge_anti_join(ctx, &l.cols[lc], &r.cols[rc])?
        }
        _ => {
            let index = hash_index(ctx, l, r, &lay.shared, &mut None)?;
            let join = index.join(Mode::Minus, l, r, &lay.shared);
            if let Some((lc, _)) = anti {
                *note = Some(format!(
                    "[anti-join on ?{} by hash]",
                    ctx.var_name(l.vars[lc])
                ));
            }
            hash_filter(ctx, &join)?
        }
    };
    kept_rows(ctx, l, &keep)
}

/// Which ids of the sorted column `a` are not in the sorted column `b`: a left row is
/// kept when the first right id not less than its own differs.
fn merge_anti_join(ctx: &Ctx, a: &[Id], b: &[Id]) -> Result<Vec<bool>> {
    let _bounds = ctx.charge(a.len() as u64 * 4 + 64)?;
    let first = merge::bounds(ctx, a, b)?;
    Ok(merge::filter(a, b, &first, false))
}

// ------------------------------------------------------------ expressions ------

/// Row-aligned decoded value columns with the charge that holds their slots and values.
struct Decoded {
    cols: super::expr::DecodedCols,
    _charge: Option<super::ctx::RetainedCharge>,
}

impl Decoded {
    fn cols(decoded: &Option<Decoded>) -> Option<&super::expr::DecodedCols> {
        decoded.as_ref().map(|d| &d.cols)
    }
}

/// Decode the base-vocabulary ids of the columns the expressions read into row-aligned
/// value columns for a table of more than [`PAR_THRESHOLD`] rows, through the one sorted
/// pre-decode ([`super::exprcache::vocabulary_values`]): every distinct term is decoded
/// once in key order with its storage admitted before it is built, and its value moves
/// or is cloned to its rows, so that per-row lookups are O(1) and lock-free. A smaller
/// table reads its values through the value cache row by row. Each column's slots are
/// charged before it is decoded and its rows' values after, and a column whose charge
/// or decoding the budget refuses is left to the rows, as are the columns after it.
/// Only cancellation and the deadline fail it.
fn decode_for(ctx: &Ctx, t: &Table, exprs: &[&Expr]) -> Result<Option<Decoded>> {
    if t.len() < 2 || !exprs.iter().any(|e| super::expr::needs_values(e)) {
        return Ok(None);
    }
    // Only the variables whose values the rows read. A column that the expressions
    // answer from ids, such as `?p = ex:a` or an EXISTS pattern's key, costs a decode of
    // every distinct term and gains nothing.
    let mut vars = Vec::new();
    for e in exprs {
        super::expr::value_vars(e, ctx, &mut vars);
    }
    vars.sort_unstable();
    vars.dedup();
    if vars.is_empty() {
        return Ok(None);
    }
    if t.len() < 4096 {
        // The rows are evaluated one at a time, each reading the terms it needs. On a
        // cold vocabulary every term is a page fault that waits for the one before it.
        // Ask for all their blocks first, so that the reads overlap.
        if ctx.opt.prefetch_terms && crate::index::io_hints() {
            prefetch_column_terms(ctx, t, &vars);
        }
        return Ok(None);
    }
    if t.len() <= PAR_THRESHOLD {
        return Ok(None);
    }
    // The charge is declared before the columns it covers, so that on every return the
    // columns are freed before it is released.
    let mut charge: Option<super::ctx::RetainedCharge> = None;
    let mut out: super::expr::DecodedCols = vec![None; t.width()];
    // Charge `total` bytes in all, `false` when the query cannot hold them. Shrinking
    // always fits.
    let mut set = |total: u64| -> bool {
        match &mut charge {
            Some(charge) => charge.resize(total).is_ok(),
            None => match ctx.retained_charge(total) {
                Ok(c) => {
                    charge = c;
                    true
                }
                Err(_) => false,
            },
        }
    };
    // what the finished columns hold
    let mut kept = 0u64;
    for v in vars {
        let Some(c) = t.col_of(v) else { continue };
        let col = &t.cols[c];
        if col
            .iter()
            .filter(|id| id.tag() == crate::id::Tag::Vocab)
            .count()
            < 4096
        {
            continue;
        }
        // the column's slots, before the distinct values are decoded with their own
        // charge
        let slots = col.len() as u64 * std::mem::size_of::<Option<Value>>() as u64;
        if !set(kept.saturating_add(slots)) {
            set(kept);
            break;
        }
        let mut values = match super::exprcache::vocabulary_values(ctx, t, v) {
            Ok(Some(values)) => values,
            Ok(None) => {
                set(kept);
                break;
            }
            Err(crate::Error::BudgetExceeded(b))
                if matches!(
                    b.kind,
                    crate::BudgetKind::Memory | crate::BudgetKind::ServerMemory
                ) =>
            {
                set(kept);
                break;
            }
            Err(error) => return Err(error),
        };
        let vals = values.sentinel_rows();
        let payload: u64 = vals
            .par_iter()
            .with_min_len(PAR_MIN_LEN)
            .map(key_payload)
            .sum();
        // A column that does not fit frees its rows and its distinct values, and then gives
        // back everything it charged. The distinct values' own charge goes once the rows'
        // values are charged.
        let total = kept.saturating_add(slots).saturating_add(payload);
        if !set(total) {
            drop(vals);
            drop(values);
            set(kept);
            break;
        }
        drop(values);
        kept = total;
        out[c] = Some(vals);
    }
    drop(set);
    Ok(Some(Decoded {
        cols: out,
        _charge: charge,
    }))
}

#[cfg(test)]
thread_local! {
    /// Count of admitted `prefetch_column_terms` ID-vector allocations on this thread
    /// (for tests that verify budget-checked admission and scalar fallback).
    static PREFETCH_COLUMN_TERMS_ALLOCATED: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
    /// Count of admitted `prefetch_column_terms` mapped block-index resizes on this thread.
    static PREFETCH_COLUMN_TERMS_BLOCKS_ADMITTED: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
}

/// Ask for the vocabulary pages of the terms in the columns of `vars` of a small table
/// (see [`decode_for`]). Skips without charging or allocating when neither the base
/// vocabulary nor its numeric column has unhinted mapped pages for these ids. In cursor
/// mode, transient prefetch scratch reuses the paid decoding envelope and admits any
/// extra optional headroom as reclaimable ledger bytes so repeated batches take no
/// per-batch ledger locks while required charges can reclaim idle headroom before
/// failing; in eager mode, it is charged and released directly on the fast account. If
/// the remaining budget cannot hold the scratch, prefetching is skipped cleanly.
fn prefetch_column_terms(ctx: &Ctx, t: &Table, vars: &[VarId]) {
    let vocab = &ctx.snap.generation.vocab;
    let ids_iter = || {
        vars.iter()
            .filter_map(|&v| t.col_of(v))
            .flat_map(|c| t.cols[c].iter())
            .filter(|id| id.tag() == crate::id::Tag::Vocab)
            .map(|id| id.payload())
    };
    if !vocab.has_unhinted_value_pages(ids_iter()) {
        return;
    }
    let count: usize = vars
        .iter()
        .filter_map(|&v| t.col_of(v))
        .map(|c| {
            t.cols[c]
                .iter()
                .filter(|id| id.tag() == crate::id::Tag::Vocab)
                .count()
        })
        .sum();
    if count < 2 {
        return;
    }
    let Some(ids_bytes) = count
        .checked_mul(size_of::<u64>())
        .and_then(|b| u64::try_from(b).ok())
    else {
        return;
    };
    let max_blocks = count
        .checked_mul(size_of::<usize>())
        .and_then(|b| u64::try_from(b).ok())
        .unwrap_or(0);
    let Ok(mut scratch) =
        ctx.admit_prefetch_scratch(ids_bytes, ids_bytes.saturating_add(max_blocks))
    else {
        return;
    };
    #[cfg(test)]
    PREFETCH_COLUMN_TERMS_ALLOCATED.with(|n| n.set(n.get() + 1));
    let mut ids = Vec::with_capacity(count);
    ids.extend(ids_iter());
    ids.sort_unstable();
    ids.dedup();
    if ids.len() >= 2 {
        let _ = vocab.prefetch_value_pages_checked(&ids, |block_bytes| {
            scratch.grow(ids_bytes.saturating_add(block_bytes as u64))?;
            #[cfg(test)]
            PREFETCH_COLUMN_TERMS_BLOCKS_ADMITTED.with(|n| n.set(n.get() + 1));
            Ok(())
        });
    }
    drop(ids);
    drop(scratch);
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
    if let Some((keep, rest)) = super::rangemask::mask(ctx, t, exprs, &mut report)? {
        t.filter_rows(&keep);
        t.sorted = sorted;
        if !rest.is_empty() {
            report.extend(apply_filter(ctx, t, &rest)?);
        }
        return Ok(report);
    }
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

/// Evaluate the filter row by row. A large table has the terms its expressions need
/// decoded once per distinct value first (see [`decode_for`]).
pub(super) fn filter_mask(ctx: &Ctx, t: &Table, exprs: &[Expr]) -> Result<Vec<bool>> {
    // `EXISTS { P }` from the key set of P, as an OPTIONAL condition tests it
    if let [e] = exprs
        && let Some(mask) = super::exists::mask(ctx, t, e)?
    {
        return Ok(mask);
    }
    if let [Expr::Cmp(a, b, op)] = exprs
        && let (Expr::Var(x), Expr::Var(y)) = (&**a, &**b)
        && let Some(keep) = string_order_mask(ctx, t, (*x, *y), *op, &exprs[0])?
    {
        return Ok(keep);
    }
    let t0 = Instant::now();
    let dec = decode_for(ctx, t, &exprs.iter().collect::<Vec<_>>())?;
    tracing::debug!(target: "sparkles::sparql::exec", "decoded values in {:?}", t0.elapsed());
    evaluate_mask(ctx, t, exprs, Decoded::cols(&dec))
}

/// Which merged candidate rows `t` of a join pass its condition `e`, from their ids. A
/// condition is not decoded first as a FILTER's expressions are in [`filter_mask`],
/// because the candidate rows of a join come in chunks of a probe and the common
/// conditions, such as `?o != <term>` or a comparison with a bound variable, are decided
/// by the ids: decoding the terms of 300,000 candidate rows cost an OPTIONAL four times
/// its run. A condition `EXISTS { P }` is answered from the key set of P.
pub(super) fn condition_mask(ctx: &Ctx, t: &Table, e: &Expr) -> Result<Vec<bool>> {
    if let Some(mask) = super::exists::mask(ctx, t, e)? {
        return Ok(mask);
    }
    evaluate_mask(ctx, t, std::slice::from_ref(e), None)
}

/// Evaluate `exprs` on every row of `t`, with the decoded columns `dec` when there are
/// any, in parallel when the expressions allow it and the rows are enough.
fn evaluate_mask(
    ctx: &Ctx,
    t: &Table,
    exprs: &[Expr],
    dec: Option<&super::expr::DecodedCols>,
) -> Result<Vec<bool>> {
    let t0 = Instant::now();
    let nested = super::exists::nested(ctx, t, &exprs.iter().collect::<Vec<_>>())?;
    let exists = nested.as_ref().map(|n| &n.answers);
    let map = t.var_map(ctx.nvars());
    let test = |i: usize| {
        let row = Row {
            table: t,
            i,
            map: &map,
            dec,
            exists,
        };
        exprs.iter().all(|e| ebv(e, &row, ctx).unwrap_or(false))
    };
    // Whether the rows may go to the pool is asked only of an input that can split,
    // since walking the expressions is wasted on the few rows of a small query.
    let keep = if t.len() <= PAR_THRESHOLD {
        let parallel = batch_may_split(t.len()) && exprs.iter().all(Expr::parallel);
        map_batch(ctx, t.len(), parallel, test)?
    } else {
        map_rows(ctx, t.len(), exprs.iter().all(Expr::parallel), test)?
    };
    tracing::debug!(target: "sparkles::sparql::exec", "filter evaluated {} rows in {:?}", t.len(), t0.elapsed());
    Ok(keep)
}

/// The rows from which [`string_order_mask`] replaces decoding both columns.
const STRING_ORDER_ROWS: usize = 4096;

/// The masks [`string_order_mask`] has built (for tests that a query reaches it).
#[cfg(test)]
static STRING_ORDER_MASKS: AtomicUsize = AtomicUsize::new(0);

/// The rows whose values the first test of [`string_order_mask`] reads.
const STRING_ORDER_SAMPLE: usize = 64;

/// Stratified sample rows used to size the first shared floor table without heap allocation.
const FLOOR_ESTIMATE_SAMPLE: usize = 1024;

/// Distinct count and frequency counts `(distinct, singletons, doubletons)` of a sorted
/// non-empty slice of raw ids.
fn sorted_id_freq(sorted: &[u64]) -> (usize, usize, usize) {
    let (mut d, mut f1, mut f2) = (0, 0, 0);
    let mut i = 0;
    while i < sorted.len() {
        let v = sorted[i];
        let start = i;
        while i < sorted.len() && sorted[i] == v {
            i += 1;
        }
        d += 1;
        match i - start {
            1 => f1 += 1,
            2 => f2 += 1,
            _ => {}
        }
    }
    (d, f1, f2)
}

/// Combined `(distinct, singletons, doubletons, cross_overlap)` across two sorted slices
/// of raw ids in one linear merge without allocation.
fn merged_id_freq(a: &[u64], b: &[u64]) -> (usize, usize, usize, usize) {
    let (mut d, mut f1, mut f2, mut cross) = (0, 0, 0, 0);
    let (mut ia, mut ib) = (0, 0);
    while ia < a.len() || ib < b.len() {
        let v = match (a.get(ia), b.get(ib)) {
            (Some(&va), Some(&vb)) => va.min(vb),
            (Some(&va), None) => va,
            (None, Some(&vb)) => vb,
            (None, None) => break,
        };
        let mut ca = 0;
        while ia < a.len() && a[ia] == v {
            ca += 1;
            ia += 1;
        }
        let mut cb = 0;
        while ib < b.len() && b[ib] == v {
            cb += 1;
            ib += 1;
        }
        d += 1;
        if ca > 0 && cb > 0 {
            cross += 1;
        }
        match ca + cb {
            1 => f1 += 1,
            2 => f2 += 1,
            _ => {}
        }
    }
    (d, f1, f2, cross)
}

/// Estimate `(distinct, has_repeats)` for one column of `n` rows from a stratified sample
/// of `s` rows with adjacent-repeat count `adj`.
fn col_distinct_estimate(
    n: usize,
    s: usize,
    d: usize,
    f1: usize,
    f2: usize,
    adj: usize,
) -> (usize, bool) {
    if f1 == d && adj == 0 {
        return (n, false);
    }
    let run_est = if adj > 0 {
        let est = ((n as u128) * (s.saturating_sub(adj) as u128))
            .checked_div(s as u128)
            .unwrap_or(1);
        usize::try_from(est).unwrap_or(n).max(1)
    } else {
        n
    };
    let chao = if f1 < d {
        d.saturating_add(f1.saturating_mul(f1.saturating_sub(1)) / (2 * (f2 + 1)))
    } else {
        run_est
    };
    (chao.min(run_est).min(n).max(d), true)
}

/// Inclusive-exclusive row range `[lo, hi)` for stratum `k` of `s` across `n` rows.
#[inline]
fn stratum_bounds(k: usize, n: usize, s: usize) -> (usize, usize) {
    let n128 = n as u128;
    let s128 = s as u128;
    let lo = ((k as u128) * n128 / s128) as usize;
    let hi = (((k + 1) as u128) * n128 / s128) as usize;
    (lo, hi)
}

struct FloorEstimate {
    slots: usize,
    /// Whether the sampled rows repeat enough strings for the floor table to pay off
    /// over direct key comparison on this mask.
    worth_caching: bool,
}

/// Estimate the initial slot count and repetition payoff of the shared floor table from a
/// stack-allocated stratified sample of `(xs, ys)`.
///
/// Inputs whose strings are few or never repeat stay compact so small queries avoid
/// over-allocating. Inputs with one non-repeating column and one small repeating column
/// (such as a unique person name compared against a handful of cities) size a compact table
/// wide enough that non-repeating writes do not trigger spurious growth. Large inputs with
/// many repeating strings pre-size directly to avoid growth migrations and recomputations.
fn estimate_floor_slots(xs: &[Id], ys: &[Id]) -> FloorEstimate {
    use super::ctx::{FLOOR_BYPASS_HIT_DENOM, FLOOR_TABLE_FEWEST, FLOOR_TABLE_FIRST};
    let n = xs.len().min(ys.len());
    let s = FLOOR_ESTIMATE_SAMPLE.min(n);
    if s == 0 {
        return FloorEstimate {
            slots: FLOOR_TABLE_FEWEST,
            worth_caching: false,
        };
    }
    let mut bx = [0u64; FLOOR_ESTIMATE_SAMPLE];
    let mut by = [0u64; FLOOR_ESTIMATE_SAMPLE];
    let (mut adj_x, mut adj_y) = (0usize, 0usize);
    for k in 0..s {
        let (lo, hi) = stratum_bounds(k, n, s);
        let r = (k as u64 + 1).wrapping_mul(0x9E37_79B9_7F4A_7C15) >> 11;
        let i = lo + (r % (hi - lo) as u64) as usize;
        let (x, y) = (xs[i], ys[i]);
        bx[k] = x.0;
        by[k] = y.0;
        if i + 1 < n {
            adj_x += usize::from(xs[i + 1] == x);
            adj_y += usize::from(ys[i + 1] == y);
        }
    }
    let sx = &mut bx[..s];
    let sy = &mut by[..s];
    sx.sort_unstable();
    sy.sort_unstable();
    let (dx, f1x, f2x) = sorted_id_freq(sx);
    let (dy, f1y, f2y) = sorted_id_freq(sy);
    let (d_all, f1_all, f2_all, cross) = merged_id_freq(sx, sy);
    let (est_x, rep_x) = col_distinct_estimate(n, s, dx, f1x, f2x, adj_x);
    let (est_y, rep_y) = col_distinct_estimate(n, s, dy, f1y, f2y, adj_y);
    let max_pow2 = 1usize << (usize::BITS - 1);
    let round_slots = |target: usize| {
        target
            .max(FLOOR_TABLE_FEWEST)
            .checked_next_power_of_two()
            .unwrap_or(max_pow2)
    };
    if !rep_x && !rep_y && cross == 0 {
        return FloorEstimate {
            slots: FLOOR_TABLE_FEWEST,
            worth_caching: false,
        };
    }
    if cross == 0 && (rep_x ^ rep_y) {
        let r = if rep_x { est_x } else { est_y };
        let writes = n.max(FLOOR_TABLE_FIRST);
        // Keep expected read-again evictions `writes * r / M` below `M / 32`.
        let guard = usize::try_from(
            (32u128)
                .saturating_mul(writes as u128)
                .saturating_mul(r as u128)
                .isqrt(),
        )
        .unwrap_or(usize::MAX)
        .saturating_mul(2);
        let expected_hits = (n as u128).saturating_sub(r as u128);
        let worth_caching = expected_hits.saturating_mul(FLOOR_BYPASS_HIT_DENOM as u128)
            >= (n as u128).saturating_mul(2);
        return FloorEstimate {
            slots: round_slots(r.saturating_mul(4).max(guard.min(FLOOR_TABLE_FIRST))),
            worth_caching,
        };
    }
    let u = if f1_all < d_all {
        d_all.saturating_add(f1_all.saturating_mul(f1_all.saturating_sub(1)) / (2 * (f2_all + 1)))
    } else {
        est_x.saturating_add(est_y)
    };
    let cells = n.saturating_mul(2);
    let u_f = u.max(1) as f64;
    let lambda = cells as f64 / u_f;
    let neg_exp = (-lambda).exp();
    let distinct = ((u_f * (1.0 - neg_exp)).round() as usize)
        .clamp(d_all, est_x.saturating_add(est_y).min(cells));
    let poisson_rep = (u_f * (1.0 - neg_exp * (1.0 + lambda))).round() as usize;
    let local_rep = if adj_x > 0 {
        est_x.min(n.saturating_sub(est_x))
    } else {
        0
    }
    .saturating_add(if adj_y > 0 {
        est_y.min(n.saturating_sub(est_y))
    } else {
        0
    });
    let repeating = poisson_rep.max(local_rep).min(distinct).min(n);
    let target = repeating
        .saturating_mul(4)
        .max(distinct.saturating_mul(5).min(FLOOR_TABLE_FIRST));
    let expected_hits = (cells as u128).saturating_sub(distinct as u128);
    let worth_caching =
        expected_hits.saturating_mul(FLOOR_BYPASS_HIT_DENOM as u128) >= cells as u128;
    FloorEstimate {
        slots: round_slots(target),
        worth_caching,
    }
}

struct InitialFloorSample {
    rows: [(u64, u64, bool, bool); STRING_ORDER_SAMPLE],
    keep: [bool; STRING_ORDER_SAMPLE],
    has_floors: bool,
}

/// Probe the initial sample of [`string_order_mask`] before the shared floor cache is
/// initialized, confirming in left-to-right order that every sampled row holds two
/// base-vocabulary simple literals without running [`estimate_floor_slots`], installing
/// a floor table, setting `initialized` or allocating the mask buffer on fallback.
/// Floor-only decoding-envelope refusals fall back to direct key comparison without
/// recording a sticky failure unless a required operand decode itself refuses.
fn probe_initial_floor_sample(
    ctx: &Ctx,
    xs: &[Id],
    ys: &[Id],
    sample: usize,
    order: impl Fn(Id, Id, u64, u64) -> bool,
    cmp_op: impl Fn(Ordering) -> bool,
) -> Result<Option<InitialFloorSample>> {
    enum ProbeFloor {
        Floor(u64, bool),
        NotSimple,
        Refused,
    }
    let mut scratch = super::ctx::FloorScratch::default();
    let mut keep = [false; STRING_ORDER_SAMPLE];
    let finish_direct = |start: usize,
                         keep: &mut [bool; STRING_ORDER_SAMPLE],
                         scratch: &mut super::ctx::FloorScratch|
     -> Result<Option<InitialFloorSample>> {
        ctx.check_memory_failure()?;
        for i in start..sample {
            let Some(ord) = ctx.simple_literal_cmp_in(xs[i], ys[i], scratch) else {
                ctx.check_memory_failure()?;
                return Ok(None);
            };
            keep[i] = cmp_op(ord);
        }
        Ok(Some(InitialFloorSample {
            rows: [(0u64, 0u64, false, false); STRING_ORDER_SAMPLE],
            keep: *keep,
            has_floors: false,
        }))
    };
    // Check row 0 before touching the 256-slot stack table so immediately-ineligible masks
    // pay no stack initialization cost.
    let fa0 = match ctx.try_string_floor_in(xs[0], &mut scratch) {
        Ok(Some(floor)) => floor,
        Ok(None) => return Ok(None),
        Err(_) => return finish_direct(0, &mut keep, &mut scratch),
    };
    let y0_new = ys[0] != xs[0];
    let fb0 = if y0_new {
        match ctx.try_string_floor_in(ys[0], &mut scratch) {
            Ok(Some(floor)) => floor,
            Ok(None) => return Ok(None),
            Err(_) => return finish_direct(0, &mut keep, &mut scratch),
        }
    } else {
        fa0
    };

    const SEEN_SLOTS: usize = 256;
    const _: () = assert!(SEEN_SLOTS >= 2 * STRING_ORDER_SAMPLE);
    // Open-addressed stack table keyed by raw `Id`. `Id::UNDEF.0` marks an empty slot;
    // every key checked or inserted passes `may_be_base_literal` and is never `Id::UNDEF`.
    let mut seen = [(Id::UNDEF.0, 0u64); SEEN_SLOTS];
    let insert_known = |seen: &mut [(u64, u64); SEEN_SLOTS], id: Id, floor: u64| {
        let mut slot = (id.payload() as usize) & (SEEN_SLOTS - 1);
        while seen[slot].0 != Id::UNDEF.0 && seen[slot].0 != id.0 {
            slot = (slot + 1) & (SEEN_SLOTS - 1);
        }
        seen[slot] = (id.0, floor);
    };
    insert_known(&mut seen, xs[0], fa0);
    if y0_new {
        insert_known(&mut seen, ys[0], fb0);
    }
    let mut lookup_or_compute = |seen: &mut [(u64, u64); SEEN_SLOTS], id: Id| -> ProbeFloor {
        if !ctx.may_be_base_literal(id) {
            return ProbeFloor::NotSimple;
        }
        let mut slot = (id.payload() as usize) & (SEEN_SLOTS - 1);
        loop {
            let (k, v) = seen[slot];
            if k == id.0 {
                return ProbeFloor::Floor(v, false);
            }
            if k == Id::UNDEF.0 {
                return match ctx.try_string_floor_in(id, &mut scratch) {
                    Ok(Some(floor)) => {
                        seen[slot] = (id.0, floor);
                        ProbeFloor::Floor(floor, true)
                    }
                    Ok(None) => ProbeFloor::NotSimple,
                    Err(_) => ProbeFloor::Refused,
                };
            }
            slot = (slot + 1) & (SEEN_SLOTS - 1);
        }
    };

    let mut rows = [(0u64, 0u64, false, false); STRING_ORDER_SAMPLE];
    rows[0] = (fa0, fb0, true, y0_new);
    keep[0] = order(xs[0], ys[0], fa0, fb0);

    for i in 1..sample {
        let (a, b) = (xs[i], ys[i]);
        let (fa, a_new) = match lookup_or_compute(&mut seen, a) {
            ProbeFloor::Floor(floor, is_new) => (floor, is_new),
            ProbeFloor::NotSimple => return Ok(None),
            ProbeFloor::Refused => return finish_direct(i, &mut keep, &mut scratch),
        };
        let (fb, b_new) = match lookup_or_compute(&mut seen, b) {
            ProbeFloor::Floor(floor, is_new) => (floor, is_new),
            ProbeFloor::NotSimple => return Ok(None),
            ProbeFloor::Refused => return finish_direct(i, &mut keep, &mut scratch),
        };
        rows[i] = (fa, fb, a_new, b_new);
        keep[i] = order(a, b, fa, fb);
    }
    Ok(Some(InitialFloorSample {
        rows,
        keep,
        has_floors: true,
    }))
}

/// Trial-compute string floors for a sample already validated by direct key comparison,
/// without recording a sticky memory failure on decoding-envelope refusal. Returns
/// `Ok(true)` when every sampled operand's floor was admitted (populating `sample_rows`),
/// `Ok(false)` when floor-only decoding refused admission, or `Err` if a sticky memory
/// failure was already recorded on `ctx`.
fn try_sample_floors(
    ctx: &Ctx,
    table_ref: Option<&super::ctx::FloorTable>,
    xs: &[Id],
    ys: &[Id],
    sample: usize,
    scratch: &mut super::ctx::FloorScratch,
    sample_rows: &mut [(u64, u64, bool, bool); STRING_ORDER_SAMPLE],
) -> Result<bool> {
    const SEEN_SLOTS: usize = 256;
    const _: () = assert!(SEEN_SLOTS >= 2 * STRING_ORDER_SAMPLE);
    let mut seen: Option<[(u64, u64); SEEN_SLOTS]> = None;
    let mut lookup = |id: Id| -> Result<Option<(u64, bool)>> {
        if let Some(cur) = table_ref
            && let Some(cached) = cur.get_cached(id.payload())
        {
            return Ok(cached.map(|floor| (floor, false)));
        }
        if let Some(seen_table) = seen.as_mut() {
            let mut slot = (id.payload() as usize) & (SEEN_SLOTS - 1);
            loop {
                let (k, v) = seen_table[slot];
                if k == id.0 {
                    return Ok(Some((v, false)));
                }
                if k == Id::UNDEF.0 {
                    let Some(floor) = ctx.try_string_floor_in(id, scratch)? else {
                        return Ok(None);
                    };
                    seen_table[slot] = (id.0, floor);
                    return Ok(Some((floor, true)));
                }
                slot = (slot + 1) & (SEEN_SLOTS - 1);
            }
        }
        let Some(floor) = ctx.try_string_floor_in(id, scratch)? else {
            return Ok(None);
        };
        let mut seen_table = [(Id::UNDEF.0, 0u64); SEEN_SLOTS];
        seen_table[(id.payload() as usize) & (SEEN_SLOTS - 1)] = (id.0, floor);
        seen = Some(seen_table);
        Ok(Some((floor, true)))
    };
    for i in 0..sample {
        let fa = match lookup(xs[i]) {
            Ok(Some(v)) => v,
            Ok(None) => return Ok(false),
            Err(_) => {
                ctx.check_memory_failure()?;
                return Ok(false);
            }
        };
        let fb = match lookup(ys[i]) {
            Ok(Some(v)) => v,
            Ok(None) => return Ok(false),
            Err(_) => {
                ctx.check_memory_failure()?;
                return Ok(false);
            }
        };
        sample_rows[i] = (fa.0, fb.0, fa.1, fb.1);
    }
    Ok(true)
}

/// `FILTER(?x < ?y)`, or `<=`, `>` or `>=`, over a table whose values of both variables
/// are stored simple literals. Their order is read from their base-vocabulary ids and
/// each string's floor ([`Ctx::string_floor`]), which the query's floor cache keeps for
/// each distinct id across tables and the batches of a cursor (see
/// [`super::ctx::FloorCache`]), so neither column is decoded. When the floor table's hit
/// rate stays low, a mask bypasses the table and compares the two vocabulary keys
/// directly with worker-local reusable scratch buffers, probing periodically so inputs
/// that repeat across later batches re-enable floors. A row with another kind of value is
/// evaluated as the generic filter would. `None` when the table is small or its first
/// rows hold other values, so that the generic filter decodes the columns instead. The
/// floors and direct comparisons decode inside the envelope, and a refused decode fails
/// the mask with the refusal it recorded.
fn string_order_mask(
    ctx: &Ctx,
    t: &Table,
    (x, y): (VarId, VarId),
    op: super::expr::CmpOp,
    expr: &Expr,
) -> Result<Option<Vec<bool>>> {
    use super::ctx::{DirectCommitPlan, FloorLookup, FloorMaskPlan, FloorScratch};
    use super::expr::CmpOp;
    let n = t.len();
    let (Some(cx), Some(cy)) = (t.col_of(x), t.col_of(y)) else {
        return Ok(None);
    };
    if n < STRING_ORDER_ROWS {
        return Ok(None);
    }
    let (xs, ys) = (&t.cols[cx], &t.cols[cy]);
    if !ctx.may_be_base_literal(xs[0]) {
        return Ok(None);
    }
    let sample = n.min(STRING_ORDER_SAMPLE);
    let cmp_op = |ord: Ordering| match op {
        CmpOp::Lt => ord == Ordering::Less,
        CmpOp::Le => ord != Ordering::Greater,
        CmpOp::Gt => ord == Ordering::Greater,
        CmpOp::Ge => ord != Ordering::Less,
    };
    let order = |a: Id, b: Id, fa: u64, fb: u64| -> bool {
        let (pa, pb) = (a.payload(), b.payload());
        let ord = match pa.cmp(&pb) {
            Ordering::Equal => Ordering::Equal,
            Ordering::Less if pa < fb => Ordering::Less,
            Ordering::Less => Ordering::Greater,
            Ordering::Greater if pb < fa => Ordering::Greater,
            Ordering::Greater => Ordering::Less,
        };
        cmp_op(ord)
    };
    let first_triple = ctx.first_triple();
    // The sample's rows keep the order their floors or direct comparisons give, so that
    // no row is looked up twice, which would mark floors as read again without a repeat
    // and grow the cache for strings that do not recur.
    let (sample_keep, plan, refused_floor_trial) = if ctx.floor_cache_needs_init() {
        let Some(probed) = probe_initial_floor_sample(ctx, xs, ys, sample, order, cmp_op)? else {
            return Ok(None);
        };
        let est = estimate_floor_slots(xs, ys);
        if probed.has_floors {
            let plan = ctx.init_floor_cache(est.slots, est.worth_caching);
            if let FloorMaskPlan::Floors(ref table) = plan {
                let mut floors = ctx.floor_map_borrowed(table.as_deref(), first_triple);
                for (i, &(fa, fb, a_new, b_new)) in probed.rows[..sample].iter().enumerate() {
                    floors.record_floor(xs[i], fa, a_new);
                    floors.record_floor(ys[i], fb, b_new);
                }
            }
            (probed.keep, plan, false)
        } else {
            let plan = ctx.init_floor_cache_after_refused_floor_trial(est.slots);
            (probed.keep, plan, true)
        }
    } else {
        if !ctx.may_be_base_literal(ys[0]) {
            // Check `xs[0]` first so a refused left-operand decode still reports its failure,
            // without taking the floor-table lock, decoding an optional floor predecessor, or
            // allocating a mask buffer.
            let _ = ctx.simple_literal_cmp(xs[0], ys[0]);
            ctx.check_memory_failure()?;
            return Ok(None);
        }
        let peek = ctx.peek_floor_plan();
        let mut sample_keep = [false; STRING_ORDER_SAMPLE];
        let mut sample_rows = [(0u64, 0u64, false, false); STRING_ORDER_SAMPLE];
        let mut scratch = FloorScratch::default();
        let mut has_floors = false;
        let mut refused_floor_trial = false;
        let strings = match &peek {
            FloorMaskPlan::Floors(table) => {
                enum SampleFloor {
                    Floor(u64, bool),
                    NotSimple,
                    Refused,
                }
                has_floors = true;
                let table_ref = table.as_deref();
                const SEEN_SLOTS: usize = 256;
                const _: () = assert!(SEEN_SLOTS >= 2 * STRING_ORDER_SAMPLE);
                let mut seen: Option<[(u64, u64); SEEN_SLOTS]> = None;
                let mut lookup = |id: Id| -> SampleFloor {
                    if !ctx.may_be_base_literal(id) {
                        return SampleFloor::NotSimple;
                    }
                    if let Some(cur) = table_ref
                        && let Some(cached) = cur.get_cached(id.payload())
                    {
                        return match cached {
                            Some(floor) => SampleFloor::Floor(floor, false),
                            None => SampleFloor::NotSimple,
                        };
                    }
                    if let Some(seen_table) = seen.as_mut() {
                        let mut slot = (id.payload() as usize) & (SEEN_SLOTS - 1);
                        loop {
                            let (k, v) = seen_table[slot];
                            if k == id.0 {
                                return SampleFloor::Floor(v, false);
                            }
                            if k == Id::UNDEF.0 {
                                return match ctx.try_string_floor_in(id, &mut scratch) {
                                    Ok(Some(floor)) => {
                                        seen_table[slot] = (id.0, floor);
                                        SampleFloor::Floor(floor, true)
                                    }
                                    Ok(None) => SampleFloor::NotSimple,
                                    Err(_) => SampleFloor::Refused,
                                };
                            }
                            slot = (slot + 1) & (SEEN_SLOTS - 1);
                        }
                    }
                    match ctx.try_string_floor_in(id, &mut scratch) {
                        Ok(Some(floor)) => {
                            let mut seen_table = [(Id::UNDEF.0, 0u64); SEEN_SLOTS];
                            seen_table[(id.payload() as usize) & (SEEN_SLOTS - 1)] = (id.0, floor);
                            seen = Some(seen_table);
                            SampleFloor::Floor(floor, true)
                        }
                        Ok(None) => SampleFloor::NotSimple,
                        Err(_) => SampleFloor::Refused,
                    }
                };
                let mut ok = true;
                let mut direct_from = None;
                for i in 0..sample {
                    let (a, b) = (xs[i], ys[i]);
                    // the second floor is read only when the first is a string's, as the generic
                    // comparison reads the second value only when the first has one
                    let (fa, a_new) = match lookup(a) {
                        SampleFloor::Floor(floor, is_new) => (floor, is_new),
                        SampleFloor::NotSimple => {
                            ok = false;
                            break;
                        }
                        SampleFloor::Refused => {
                            direct_from = Some(i);
                            break;
                        }
                    };
                    let (fb, b_new) = match lookup(b) {
                        SampleFloor::Floor(floor, is_new) => (floor, is_new),
                        SampleFloor::NotSimple => {
                            ok = false;
                            break;
                        }
                        SampleFloor::Refused => {
                            direct_from = Some(i);
                            break;
                        }
                    };
                    sample_rows[i] = (fa, fb, a_new, b_new);
                    sample_keep[i] = order(a, b, fa, fb);
                }
                if let Some(start) = direct_from {
                    ctx.check_memory_failure()?;
                    has_floors = false;
                    refused_floor_trial = true;
                    ok = (start..sample).all(|i| {
                        let Some(ord) = ctx.simple_literal_cmp_in(xs[i], ys[i], &mut scratch)
                        else {
                            return false;
                        };
                        sample_keep[i] = cmp_op(ord);
                        true
                    });
                }
                ok
            }
            FloorMaskPlan::Direct => (0..sample).all(|i| {
                let Some(ord) = ctx.simple_literal_cmp_in(xs[i], ys[i], &mut scratch) else {
                    return false;
                };
                sample_keep[i] = cmp_op(ord);
                true
            }),
        };
        if !strings {
            // a refused decode is not another kind of value
            ctx.check_memory_failure()?;
            return Ok(None);
        }
        let plan = if refused_floor_trial {
            ctx.begin_floor_mask_after_refused_floor_trial()
        } else if has_floors {
            ctx.begin_floor_mask()
        } else {
            match ctx.begin_floor_mask_after_direct() {
                DirectCommitPlan::Direct => FloorMaskPlan::Direct,
                DirectCommitPlan::NeedsFloorTrial(trial_table) => {
                    if try_sample_floors(
                        ctx,
                        trial_table.as_deref(),
                        xs,
                        ys,
                        sample,
                        &mut scratch,
                        &mut sample_rows,
                    )? {
                        has_floors = true;
                        ctx.begin_floor_mask()
                    } else {
                        refused_floor_trial = true;
                        ctx.begin_floor_mask_after_refused_floor_trial()
                    }
                }
            }
        };
        if has_floors && let FloorMaskPlan::Floors(ref table) = plan {
            let mut floors = ctx.floor_map_borrowed(table.as_deref(), first_triple);
            for (i, &(fa, fb, a_new, b_new)) in sample_rows[..sample].iter().enumerate() {
                floors.record_floor(xs[i], fa, a_new);
                floors.record_floor(ys[i], fb, b_new);
            }
        }
        (sample_keep, plan, refused_floor_trial)
    };
    let mut keep = vec![false; n];
    keep[..sample].copy_from_slice(&sample_keep[..sample]);
    let threads = rayon::current_num_threads().max(1);
    let map = t.var_map(ctx.nvars());
    let generic = |i: usize| {
        let row = Row {
            table: t,
            i,
            map: &map,
            dec: None,
            exists: None,
        };
        ebv(expr, &row, ctx).unwrap_or(false)
    };
    let direct_of = |scratch: &mut FloorScratch, i: usize| -> bool {
        match ctx.simple_literal_cmp_in(xs[i], ys[i], scratch) {
            Some(ord) => cmp_op(ord),
            None => generic(i),
        }
    };
    let order_of = |floors: &mut super::ctx::FloorMap<'_>, i: usize| -> bool {
        let (a, b) = (xs[i], ys[i]);
        let fa = match floors.try_floor(a) {
            FloorLookup::Floor(fa) => fa,
            FloorLookup::NotSimple => return generic(i),
            FloorLookup::Refused => return direct_of(floors.scratch_mut(), i),
        };
        let fb = match floors.try_floor(b) {
            FloorLookup::Floor(fb) => fb,
            FloorLookup::NotSimple => return generic(i),
            FloorLookup::Refused => return direct_of(floors.scratch_mut(), i),
        };
        order(a, b, fa, fb)
    };
    let fill_direct = |out: &mut [bool], lo: usize| -> Result<()> {
        if threads > 1 {
            let chunk_len = out.len().div_ceil(threads).max(1);
            out.par_chunks_mut(chunk_len).enumerate().try_for_each_init(
                FloorScratch::default,
                |scratch, (chunk_idx, chunk)| {
                    ctx.check()?;
                    let start = lo + chunk_idx * chunk_len;
                    for (offset, slot) in chunk.iter_mut().enumerate() {
                        let i = start + offset;
                        if offset > 0 && i.is_multiple_of(1024) {
                            ctx.check()?;
                        }
                        *slot = direct_of(scratch, i);
                    }
                    Ok(())
                },
            )
        } else {
            ctx.check()?;
            let mut scratch = FloorScratch::default();
            for (offset, slot) in out.iter_mut().enumerate() {
                let i = lo + offset;
                if offset > 0 && i.is_multiple_of(1024) {
                    ctx.check()?;
                }
                *slot = direct_of(&mut scratch, i);
            }
            Ok(())
        }
    };
    if n <= PAR_THRESHOLD && threads > 1 {
        // A table of at most `PAR_THRESHOLD` rows, such as a cursor's batch, spreads over
        // the pool as the generic filter's batch does, so that a cursor's comparison is
        // not left on one thread where the generic filter would use them all. The batch
        // checks the deadline itself and writes directly into `keep`.
        match plan {
            FloorMaskPlan::Floors(table) => {
                let table_ref = table.as_deref();
                fill_batch_with(
                    ctx,
                    &mut keep[sample..],
                    sample,
                    || ctx.floor_map_borrowed(table_ref, first_triple),
                    order_of,
                )?;
            }
            FloorMaskPlan::Direct => {
                fill_batch_with(
                    ctx,
                    &mut keep[sample..],
                    sample,
                    FloorScratch::default,
                    direct_of,
                )?;
            }
        }
    } else {
        // A larger table (or a single-threaded mask) is compared in parts of
        // `PAR_THRESHOLD` rows and then of the rows before them, so that the floor cache
        // can grow, bypass low-hit-rate parts, and probe periodically between parts as it
        // does between a cursor's batches.
        let mut plan = plan;
        let mut lo = sample;
        while lo < n {
            if lo > sample && !refused_floor_trial {
                plan = ctx.begin_floor_mask();
            }
            let hi = n.min(lo + lo.max(PAR_THRESHOLD));
            match &plan {
                FloorMaskPlan::Direct => {
                    fill_direct(&mut keep[lo..hi], lo)?;
                }
                FloorMaskPlan::Floors(table) => {
                    let table_ref = table.as_deref();
                    if threads > 1 {
                        let chunk_len = (hi - lo).div_ceil(threads).max(1);
                        keep[lo..hi]
                            .par_chunks_mut(chunk_len)
                            .enumerate()
                            .try_for_each_init(
                                || ctx.floor_map_borrowed(table_ref, first_triple),
                                |floors, (chunk_idx, chunk)| -> Result<()> {
                                    ctx.check()?;
                                    let start = lo + chunk_idx * chunk_len;
                                    for (offset, slot) in chunk.iter_mut().enumerate() {
                                        let i = start + offset;
                                        if offset > 0 && i.is_multiple_of(1024) {
                                            ctx.check()?;
                                        }
                                        *slot = order_of(floors, i);
                                    }
                                    Ok(())
                                },
                            )?;
                    } else {
                        ctx.check()?;
                        let mut floors = ctx.floor_map_borrowed(table_ref, first_triple);
                        for (offset, slot) in keep[lo..hi].iter_mut().enumerate() {
                            let i = lo + offset;
                            if offset > 0 && i.is_multiple_of(1024) {
                                ctx.check()?;
                            }
                            *slot = order_of(&mut floors, i);
                        }
                    }
                }
            }
            lo = hi;
        }
    }
    debug_assert_eq!(keep.len(), n);
    // Catch any refused decode (which fell back to generic) or late-chunk cancellation
    // before returning the completed mask.
    ctx.check()?;
    #[cfg(test)]
    STRING_ORDER_MASKS.fetch_add(1, AtomicOrdering::Relaxed);
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
    if let Expr::Var(v) = e {
        // Like var_map, use the last column if the variable occurs more than once.
        let column = t.vars.iter().rposition(|var| var == v).map(|c| &t.cols[c]);
        let mut out = Vec::with_capacity(t.len());
        loop {
            ctx.check()?;
            let start = out.len();
            let end = (start + MAP_POLL_ROWS).min(t.len());
            match column {
                Some(ids) => out.extend_from_slice(&ids[start..end]),
                None => out.resize(end, Id::UNDEF),
            }
            if end == t.len() {
                return Ok(out);
            }
        }
    }
    // `EXISTS { P }` from the key set of P when the rows have enough distinct keys
    if let Some(col) = super::exists::column(ctx, t, e)? {
        return Ok(col);
    }
    match super::exprcache::column(ctx, t, e, report, |v| column_rows(ctx, v, e))? {
        Some(ids) => Ok(ids),
        None => column_rows(ctx, t, e),
    }
}

fn column_rows(ctx: &Ctx, t: &Table, e: &Expr) -> Result<Vec<Id>> {
    let nested = super::exists::nested(ctx, t, &[e])?;
    let exists = nested.as_ref().map(|n| &n.answers);
    let dec = decode_for(ctx, t, &[e])?;
    let map = t.var_map(ctx.nvars());
    let f = |i: usize| match eval(
        e,
        &Row {
            table: t,
            i,
            map: &map,
            dec: Decoded::cols(&dec),
            exists,
        },
        ctx,
    ) {
        Ok(v) => v.into_id(ctx),
        Err(_) => Id::UNDEF,
    };
    if t.len() <= PAR_THRESHOLD {
        return map_batch(ctx, t.len(), batch_may_split(t.len()) && e.parallel(), f);
    }
    map_rows(ctx, t.len(), e.parallel(), f)
}

#[cfg(test)]
mod column_tests {
    use super::*;

    #[test]
    fn row_map_starts_at_an_aligned_address() {
        assert!(std::mem::align_of::<Row<'_>>() >= 16);
        assert_eq!(std::mem::offset_of!(Row<'_>, map), 0);
    }

    #[test]
    fn variable_column_preserves_ids_and_unbound_rows() {
        let store = crate::store::Store::in_memory(Default::default());
        let ctx = Ctx::new(store.snapshot());
        let bound = ctx.var("bound");
        let unbound = ctx.var("unbound");
        let mut report = ExprReport::default();
        let mut table = Table::new(vec![bound]);
        assert!(
            compute_column(&ctx, &table, &Expr::Var(bound), &mut report)
                .unwrap()
                .is_empty()
        );

        let ids = [Id::from_i64(42).unwrap(), Id::UNDEF, Id::from_bool(false)];
        for id in ids {
            table.push_row(&[id]);
        }
        assert_eq!(
            compute_column(&ctx, &table, &Expr::Var(bound), &mut report).unwrap(),
            ids
        );
        assert_eq!(
            compute_column(&ctx, &table, &Expr::Var(unbound), &mut report).unwrap(),
            [Id::UNDEF; 3]
        );

        table.vars.insert(0, bound);
        table.cols.insert(0, vec![Id::UNDEF; table.len()]);
        assert_eq!(
            compute_column(&ctx, &table, &Expr::Var(bound), &mut report).unwrap(),
            ids
        );
    }

    #[test]
    fn variable_column_checks_cancellation_on_empty_input() {
        let store = crate::store::Store::in_memory(Default::default());
        let ctx = Ctx::new(store.snapshot());
        let var = ctx.var("v");
        ctx.cancel.store(true, AtomicOrdering::Relaxed);
        assert!(matches!(
            compute_column(
                &ctx,
                &Table::new(vec![var]),
                &Expr::Var(var),
                &mut ExprReport::default(),
            ),
            Err(Error::Cancelled)
        ));
    }
}

type Candidates = (Vec<usize>, Option<super::ctx::RetainedCharge>);

/// Candidate rows for `ORDER BY ?v LIMIT k` when every `?v` is a number other than NaN:
/// a rounded `f64` per row is cheap, and rows whose rounded key is worse than the k-th
/// best rounded key have at least k rows strictly ahead of them in the exact order (the
/// rounding is monotone and numbers order by value first), so they cannot be among the
/// first k. `None` when the shortcut does not apply, or when its charge does not fit.
///
/// Dictionary values come from `decoded` when a caller has them. Otherwise an input of
/// at most [`PAR_THRESHOLD`] rows decodes its dictionary values once each in key order,
/// and a larger input reads them through the value cache in parallel.
fn topk_candidates(
    ctx: &Ctx,
    t: &Table,
    keys: &[(Expr, bool)],
    k: usize,
    decoded: Option<&super::exprcache::PerValue<Option<Value>>>,
) -> Result<Option<Candidates>> {
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
        None if t.len() <= PAR_THRESHOLD => {
            owned = super::exprcache::dictionary_values(ctx, t, expr)?;
            owned.as_ref()
        }
        None => None,
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
    ctx.check()?;
    let Some(numbers) = approximate.into_iter().collect::<Option<Vec<f64>>>() else {
        return Ok(None);
    };
    let mut sorted = numbers.clone();
    let (_, kth, _) = sorted.select_nth_unstable_by(k - 1, |a, b| b.total_cmp(a));
    // The other numeric conversions are correctly rounded. Fixed decimals round
    // once converting the mantissa and again dividing by an exactly representable
    // power of ten. The two rounding errors on either side can reverse the exact
    // order by up to four representable steps. Keep these rows for exact comparison.
    let threshold = if kth.is_finite() {
        kth.next_down().next_down().next_down().next_down()
    } else {
        *kth
    };
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
    let mut candidate_charge = None;
    if let Some(k) = limit
        && ctx.opt.topk_prefilter
        && let Some(cand) = topk_candidates(ctx, &t, keys, k, None)?.map(|(rows, charge)| {
            candidate_charge = charge;
            rows
        })
        && cand.len() < t.len()
    {
        // the candidates keep their relative order, so ties break as in the full sort
        notes.push(format!("[numeric prefilter kept {} rows]", cand.len()));
        ctx.check_output(cand.len(), t.width())?;
        t = t.take_rows(&cand);
    }
    drop(candidate_charge);
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
        // Declare heap credit BEFORE `heap` so entries drop before their credit on unwind.
        let mut rows = EagerHeapRows::new(ctx)?;
        let mut heap = TopK::new(k, keys.iter().map(|(_, asc)| *asc).collect());
        let ranked = offer_rows(ctx, &mut heap, &t, keys, report, &mut rows)?;
        ctx.check()?;
        let idx: Vec<usize> = heap.into_sorted().into_iter().map(|e| e.payload).collect();
        drop(rows);
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

/// The key of a variable's id, propagating any latched decode refusal.
#[inline]
fn id_key(ctx: &Ctx, id: Id) -> Result<SortKey> {
    let v = ctx.value(id);
    if v.is_none() && ctx.has_memory_failure() {
        ctx.check()?;
    }
    Ok(SortKey::new(v))
}

/// Where [`offer_rows`] keeps the rows that may enter a [`TopK`].
pub(super) trait HeapRows<P> {
    /// Keep row `i` of the input for an entry with these keys.
    fn keep(&mut self, i: usize, keys: &[SortKey]) -> Result<P>;
    /// An entry left the heap, or an offered row did not enter.
    fn release(&mut self, entry: Entry<P>) -> Result<()>;
}

/// Eager execution keeps the input table, so an entry is a row index plus live credit
/// for the `SortKey` payloads retained inside the heap until eviction or drop.
struct EagerHeapRows<'a> {
    charge: super::ctx::Charge<'a>,
    bytes: u64,
    step: u64,
}

impl<'a> EagerHeapRows<'a> {
    fn new(ctx: &'a Ctx) -> Result<Self> {
        Ok(Self {
            charge: ctx.charge(0)?,
            bytes: 0,
            step: ctx.reservation_step(),
        })
    }

    fn entry_bytes(keys: &[SortKey]) -> u64 {
        (std::mem::size_of::<Entry<usize>>() as u64)
            .saturating_add(
                (keys.len() as u64).saturating_mul(std::mem::size_of::<SortKey>() as u64),
            )
            .saturating_add(keys.iter().map(SortKey::payload_bytes).sum::<u64>())
    }
}

impl HeapRows<usize> for EagerHeapRows<'_> {
    fn keep(&mut self, i: usize, keys: &[SortKey]) -> Result<usize> {
        let added = Self::entry_bytes(keys);
        let next = self.bytes.saturating_add(added);
        self.charge.cover(next, self.step.min(next / 4))?;
        self.bytes = next;
        Ok(i)
    }

    fn release(&mut self, entry: Entry<usize>) -> Result<()> {
        let freed = Self::entry_bytes(&entry.keys);
        // Drop the evicted entry's payloads before releasing their credit.
        drop(entry);
        self.bytes = self.bytes.saturating_sub(freed);
        let slack = self.step.min(self.bytes / 4).max(256);
        if self.charge.bytes() > self.bytes.saturating_add(slack) {
            self.charge.resize(self.bytes)?;
        }
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
    // the first key's column of SortKeys, beyond the values its column charges
    let _first_charge = ctx.charge(t.len() as u64 * std::mem::size_of::<SortKey>() as u64 + 128)?;
    let mut sources = Vec::with_capacity(keys.len());
    for (i, (e, _)) in keys.iter().enumerate() {
        sources.push(match e {
            Expr::Var(v) if i > 0 => HeapKey::Var(t.col_of(*v).map(|c| t.cols[c].as_slice())),
            _ if i == 0 => HeapKey::Column(key_column(ctx, t, e, report)?.map(SortKey::new)),
            _ => HeapKey::Expr(e),
        });
    }
    let later: Vec<&Expr> = keys.iter().skip(1).map(|(e, _)| e).collect();
    let nested = super::exists::nested(ctx, t, &later)?;
    let exists = nested.as_ref().map(|n| &n.answers);
    let map = t.var_map(ctx.nvars());
    let key_of = |i: usize, k: usize| -> Result<SortKey> {
        match &sources[k] {
            HeapKey::Var(Some(col)) => id_key(ctx, col[i]),
            HeapKey::Var(None) => Ok(SortKey::NULL),
            HeapKey::Column(c) => Ok(c.get(i).clone()),
            HeapKey::Expr(e) => {
                let val = match eval(
                    e,
                    &Row {
                        table: t,
                        i,
                        map: &map,
                        dec: None,
                        exists,
                    },
                    ctx,
                ) {
                    Ok(Val::Id(id)) => {
                        let v = ctx.value(id);
                        if v.is_none() && ctx.has_memory_failure() {
                            ctx.check()?;
                        }
                        v
                    }
                    Ok(Val::V(v) | Val::Dec(_, v)) => Some(v),
                    Err(_) => {
                        if ctx.has_memory_failure() {
                            ctx.check()?;
                        }
                        None
                    }
                };
                Ok(SortKey::new(val))
            }
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
                let first = key_of(i, 0)?;
                if heap.screen(&first) == Screen::Reject {
                    heap.skip();
                    continue;
                }
                first
            }
        };
        let row_decode = ctx.charge(0)?;
        let mut row = Vec::with_capacity(keys.len());
        row.push(first);
        for (k, source) in sources.iter().enumerate().skip(1) {
            let sk = match source {
                HeapKey::Var(Some(col)) => {
                    SortKey::new(ctx.value_checked_into(col[i], &row_decode)?)
                }
                _ => {
                    let sk = key_of(i, k)?;
                    let p = sk.payload_bytes();
                    if p > 0 {
                        row_decode.add(p)?;
                    }
                    sk
                }
            };
            row.push(sk);
        }
        ranked += 1;
        let payload = rows.keep(i, &row)?;
        let evicted = heap.offer(row, payload);
        drop(row_decode);
        if let Some(out) = evicted {
            rows.release(out)?;
        }
    }
    ctx.check()?;
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

/// Evaluate a column with one variable map and one set of EXISTS answers.
fn with_key_evaluator<T>(
    ctx: &Ctx,
    t: &Table,
    e: &Expr,
    dec: Option<&super::expr::DecodedCols>,
    run: impl FnOnce(&(dyn Fn(usize) -> Result<Option<Value>> + Sync)) -> Result<T>,
) -> Result<T> {
    let _map_charge = ctx.charge(ctx.nvars() as u64 * 16 + 128)?;
    let nested = super::exists::nested(ctx, t, &[e])?;
    let exists = nested.as_ref().map(|n| &n.answers);
    let map = t.var_map(ctx.nvars());
    let result = run(&|i| match eval(
        e,
        &Row {
            table: t,
            i,
            map: &map,
            dec,
            exists,
        },
        ctx,
    ) {
        Ok(Val::Id(id)) => ctx.cursor_value_checked(id),
        Ok(Val::V(v) | Val::Dec(_, v)) => Ok(Some(v)),
        Err(_) => {
            if ctx.has_memory_failure() {
                ctx.check()?;
            }
            Ok(None)
        }
    })?;
    ctx.check()?;
    Ok(result)
}

/// Classify each spill merge key after admitting its owned payload.
pub(super) fn spill_order_keys(
    ctx: &Ctx,
    t: &Table,
    e: &Expr,
    mut admit: impl FnMut(u64) -> Result<()>,
) -> Result<Vec<super::sortkey::SortKey>> {
    with_key_evaluator(ctx, t, e, None, |eval| {
        let mut values = Vec::with_capacity(t.len());
        for i in 0..t.len() {
            if i.is_multiple_of(MAP_POLL_ROWS) {
                ctx.check()?;
            }
            let value = eval(i)?;
            admit(key_payload(&value))?;
            values.push(super::sortkey::SortKey::new(value));
        }
        Ok(values)
    })
}

/// The bytes an ORDER key's value holds beyond its slot.
fn key_payload(value: &Option<Value>) -> u64 {
    match value {
        Some(Value::Iri(s) | Value::BNode(s) | Value::Str(s)) => s.len() as u64 + 64,
        Some(Value::Lang(s, lang) | Value::LangDir(s, lang, _)) => {
            (s.len() + lang.len()) as u64 + 128
        }
        Some(Value::Other { lex, dt }) => (lex.len() + dt.len()) as u64 + 128,
        Some(Value::BigInteger(i)) => i.heap_bytes() + 32,
        Some(Value::BigDecimal(d)) => d.heap_bytes() + 32,
        Some(Value::Triple(triple)) => super::graph_triple_bytes(triple),
        _ => 0,
    }
}

/// Own decoded ORDER keys until the sort releases them. Admit a value's payload
/// before keeping it in the key column.
fn key_column_charged(
    ctx: &Ctx,
    t: &Table,
    e: &Expr,
    dec: Option<&super::expr::DecodedCols>,
) -> Result<super::exprcache::Column<Option<Value>>> {
    let mut bytes = t.len() as u64 * std::mem::size_of::<Option<Value>>() as u64 + 128;
    let mut charge = ctx.retained_charge(bytes)?;
    let vals = key_rows_retained(ctx, t, e, dec, &mut charge, &mut bytes)?;
    Ok(super::exprcache::Column::Rows {
        vals,
        _charge: charge,
    })
}

fn key_rows_retained(
    ctx: &Ctx,
    t: &Table,
    e: &Expr,
    dec: Option<&super::expr::DecodedCols>,
    charge: &mut Option<super::ctx::RetainedCharge>,
    bytes: &mut u64,
) -> Result<Vec<Option<Value>>> {
    with_key_evaluator(ctx, t, e, dec, |eval| {
        let may_split = t.len() > PAR_THRESHOLD;
        let parallel_key = (may_split || cfg!(feature = "geo")) && e.parallel_order_key();
        let step = if cfg!(feature = "geo") && !parallel_key {
            // A later key evaluation may need credit to serialize a geometry.
            // Keep only the payload we already evaluated.
            0
        } else {
            ctx.reservation_step()
        };
        let eval_retained =
            |i: usize, charge: &mut Option<super::ctx::RetainedCharge>, bytes: &mut u64| {
                if i.is_multiple_of(MAP_POLL_ROWS) {
                    ctx.check()?;
                }
                let value = eval(i)?;
                *bytes = bytes.saturating_add(key_payload(&value));
                if let Some(charge) = charge {
                    charge.cover(*bytes, step)?;
                }
                Ok::<_, Error>(value)
            };
        // Fill the final slots directly. Bound the number of pieces so each can
        // keep one transient result if its retained charge runs out of room.
        const TRANSIENT: u64 = super::bignum::AGG_SUM_PAYLOAD_BYTES;
        const PIECE_BYTES: u64 = 128;
        // Resume offsets fit on the stack so the heap piece records can be
        // freed before the serial suffixes need their memory.
        const MAX_PIECES: usize = 64;
        let piece_rows = if may_split && parallel_key {
            t.len()
                .div_ceil(rayon::current_num_threads().min(MAX_PIECES))
                .max(PAR_MIN_LEN)
        } else {
            t.len().max(1)
        };
        let pieces = t.len().div_ceil(piece_rows) as u64;
        let piece_bytes = pieces * PIECE_BYTES;
        let reserve_scratch = || {
            let bytes = piece_bytes + pieces * TRANSIENT;
            let Some(owner) = charge.as_ref() else {
                return ctx.charge(bytes);
            };
            let scratch = owner.part(ctx);
            scratch.add(bytes)?;
            Ok(scratch)
        };
        if pieces > 1
            && let Ok(scratch) = reserve_scratch()
        {
            let mut values = vec![None; t.len()];
            let part_owner = charge.as_ref();
            let mut parts: Vec<_> = values
                .par_chunks_mut(piece_rows)
                .enumerate()
                .map(|(piece, slots)| {
                    let start = piece * piece_rows;
                    let part = part_owner.map(|owner| owner.part(ctx));
                    let mut payload = 0u64;
                    let progress = (|| {
                        for (offset, slot) in slots.iter_mut().enumerate() {
                            if offset.is_multiple_of(MAP_POLL_ROWS) {
                                ctx.check()?;
                            }
                            let value = eval(start + offset)?;
                            let key_bytes = key_payload(&value);
                            let needed = payload.saturating_add(key_bytes);
                            if let Some(part) = &part
                                && needed > part.bytes()
                                && part.resize(needed.saturating_add(step)).is_err()
                            {
                                match part.resize(needed) {
                                    Ok(()) => {}
                                    Err(Error::BudgetExceeded(_)) if key_bytes <= TRANSIENT => {
                                        *slot = value;
                                        return Ok((offset + 1, key_bytes));
                                    }
                                    // A wider result cannot stay in scratch.
                                    // Retry this row only if evaluation is repeatable.
                                    Err(Error::BudgetExceeded(_))
                                        if super::exprcache::repeatable(e) =>
                                    {
                                        return Ok((offset, 0));
                                    }
                                    Err(error) => return Err(error),
                                }
                            }
                            payload = needed;
                            *slot = value;
                        }
                        Ok((slots.len(), 0))
                    })();
                    if let Some(part) = &part {
                        part.resize(payload).expect("shrinking a piece's charge");
                    }
                    (progress, payload, part)
                })
                .collect();
            for (_, payload, part) in &mut parts {
                if let (Some(charge), Some(part)) = (charge.as_mut(), part.take()) {
                    charge.absorb(part);
                }
                *bytes = bytes.saturating_add(*payload);
            }
            let mut resume = [0; MAX_PIECES];
            let mut pending = 0;
            let mut has_suffix = false;
            for (((progress, _, _), slots), offset) in parts
                .into_iter()
                .zip(values.chunks(piece_rows))
                .zip(&mut resume)
            {
                let (done, saved) = progress?;
                *offset = done;
                pending += saved;
                has_suffix |= done < slots.len();
            }
            // The collection is gone. Release its charge and the unused
            // transient allowance before admitting any more key payload.
            scratch.resize(pending)?;
            if pending > 0 {
                charge.as_mut().expect("retained pieces").absorb(scratch);
                *bytes = bytes.saturating_add(pending);
            }
            if has_suffix {
                ctx.check()?;
                for (piece, (slots, &done)) in
                    values.chunks_mut(piece_rows).zip(&resume).enumerate()
                {
                    let start = piece * piece_rows;
                    for (offset, slot) in slots.iter_mut().enumerate().skip(done) {
                        *slot = eval_retained(start + offset, charge, bytes)?;
                    }
                }
                charge.as_mut().expect("retained pieces").resize(*bytes)?;
            }
            return Ok(values);
        }
        let mut values = Vec::with_capacity(t.len());
        for i in 0..t.len() {
            values.push(eval_retained(i, charge, bytes)?);
        }
        if let Some(charge) = charge {
            charge.resize(*bytes)?;
        }
        Ok(values)
    })
}

#[cfg(test)]
#[test]
fn order_keys_refuse_before_accumulating_an_oversized_payload() {
    use crate::store::{Store, StoreOptions};
    use oxrdf::{Literal, Term};
    use spargebra::algebra::Function;

    let store = Store::in_memory(StoreOptions::default());
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(1)
        .build()
        .unwrap();
    pool.install(|| {
        // An allocation probe on this worker observes the parallel chunks, too.
        let mut ctx = Ctx::new(store.snapshot());
        ctx.mem_limit = 4 << 20;
        let ctx = ctx.into_shared();
        let a = ctx.var("a");
        let b = ctx.var("b");
        let x = ctx.intern_term(&Term::Literal(Literal::new_simple_literal("a".repeat(512))));
        let y = ctx.intern_term(&Term::Literal(Literal::new_simple_literal("b".repeat(512))));
        let n = PAR_THRESHOLD + 1;
        let mut table = Table::new(vec![a, b]);
        table.cols = vec![vec![x; n], vec![y; n]];
        table.len = n;
        let expr = Expr::Call(
            super::expr::Func::Builtin(Function::Concat),
            (0..4).flat_map(|_| [Expr::Var(a), Expr::Var(b)]).collect(),
        );

        let peak = crate::test_alloc::record_peak_live(|| {
            let error = key_column_charged(&ctx, &table, &expr, None).err();
            assert!(
                matches!(error, Some(Error::BudgetExceeded(b)) if b.kind == crate::BudgetKind::Memory),
                "large ORDER expression column returned {error:?}"
            );
        });
        assert!(peak < 8 << 20, "sort allocated {peak} bytes before refusing");

        let keys = [(Expr::Const(Id::from_bool(true)), true), (expr, true)];
        let peak = crate::test_alloc::record_peak_live(|| {
            let error = order_by(
                &ctx,
                table,
                &keys,
                Some(3_000),
                &mut ExprReport::default(),
            )
            .err();
            assert!(
                matches!(error, Some(Error::BudgetExceeded(b)) if b.kind == crate::BudgetKind::Memory),
                "eager top-k returned {error:?}"
            );
        });
        assert!(peak < 8 << 20, "heap allocated {peak} bytes before refusing");
    });
}

/// The value of an ORDER BY key once per distinct input value, where the key is pure
/// over one variable and its values repeat (see [`super::exprcache`]).
fn key_per_value(
    ctx: &Ctx,
    t: &Table,
    e: &Expr,
    report: &mut ExprReport,
) -> Result<Option<super::exprcache::PerValue<Option<Value>>>> {
    // Numeric keys over inline ids and variable keys have specialized reuse.
    // The general cache is useful only when enough rows can evaluate in parallel.
    if t.len() < super::exprcache::MIN_ROWS {
        return Ok(None);
    }
    if let Some(values) =
        super::exprcache::cursor_numeric_values(ctx, t, e, report, |v, charge, bytes| {
            key_rows_retained(ctx, v, e, None, charge, bytes)
        })?
    {
        return Ok(Some(values));
    }
    if let Some(values) = super::exprcache::cursor_variable_values(ctx, t, e)? {
        return Ok(Some(values));
    }
    if t.len() <= PAR_THRESHOLD {
        return Ok(None);
    }
    super::exprcache::per_value_retained(ctx, t, &[e], false, report, |v, charge, bytes| {
        let dec = decode_for(ctx, v, &[e])?;
        key_rows_retained(ctx, v, e, Decoded::cols(&dec), charge, bytes)
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
        None if t.len() <= PAR_THRESHOLD => key_column_charged(ctx, t, e, None)?,
        None => key_column_charged(ctx, t, e, Decoded::cols(&decode_for(ctx, t, &[e])?))?,
    })
}

fn order_by_rows(
    ctx: &Ctx,
    t: Table,
    keys: &[(Expr, bool)],
    limit: Option<usize>,
    report: &mut ExprReport,
) -> Result<Table> {
    // the sort's positions and the sorted copy of the table
    let _sort_charge = ctx.charge(
        (t.len() as u64)
            .saturating_mul(24)
            .saturating_add(t.mem_bytes().saturating_mul(2))
            .saturating_add(keys.len() as u64 * 128 + 1024),
    )?;
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
    let dec = if t.len() > PAR_THRESHOLD {
        decode_for(ctx, &t, &rest)?
    } else {
        None
    };
    let key_vals: Vec<super::exprcache::Column<Option<Value>>> = keys
        .iter()
        .zip(cached)
        .map(|((e, _), c)| {
            Ok(match c {
                Some(p) => super::exprcache::Column::Values(p),
                None => key_column_charged(ctx, &t, e, Decoded::cols(&dec))?,
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
    // Sorting has used the last key. Free the key columns on the pool before
    // checking room for the output table.
    key_vals
        .into_iter()
        .for_each(super::exprcache::Column::drop_in_parallel);
    ctx.check_output(idx.len(), t.width())?;
    Ok(t.take_rows(&idx))
}

/// The rows of `t` that DISTINCT keeps, each value's first, in input order.
fn distinct_rows(t: &Table) -> Vec<usize> {
    if t.width() == 1 {
        let mut seen = FxHashSet::default();
        return (0..t.len())
            .filter(|&i| seen.insert(t.cols[0][i]))
            .collect();
    }
    if t.width() == 0 {
        return (0..t.len().min(1)).collect();
    }
    let mut seen: FxHashSet<Vec<Id>> = FxHashSet::default();
    (0..t.len()).filter(|&i| seen.insert(t.row(i))).collect()
}

fn distinct(t: Table) -> Table {
    if t.width() == 0 {
        let mut t = t;
        t.len = t.len.min(1);
        return t;
    }
    let idx = distinct_rows(&t);
    let sorted = t.sorted.clone();
    let mut out = t.take_rows(&idx);
    out.sorted = sorted;
    out
}

/// The DISTINCT `n` over `t`. A numbered DISTINCT below an ORDER BY (item 85) adds to
/// each row it keeps the number of that row's first occurrence in its input, as the
/// cursor's DISTINCT does, and the sort above orders ties on it.
fn distinct_of(ctx: &Ctx, n: &Node, t: Table) -> Result<Table> {
    if !super::demand::numbered_distinct(n) {
        return Ok(distinct(t));
    }
    let number = *n.vars.last().expect("a numbered DISTINCT's number");
    let idx = distinct_rows(&t);
    ctx.check_output(idx.len(), t.width() + 1)?;
    let column = idx
        .iter()
        .map(|&i| Id::from_i64(i as i64).ok_or_else(|| Error::invalid("too many DISTINCT rows")))
        .collect::<Result<Vec<Id>>>()?;
    let mut out = t.take_rows(&idx);
    out.sorted.clear();
    out.vars.push(number);
    out.cols.push(column);
    Ok(out)
}

// ------------------------------------------------------------------ group ------

/// GROUP BY over the whole input `t`, through the grouping operator. A query that may
/// spill charges the groups' state, and when they do not fit its memory budget the
/// grouping runs again as the cursor's grouping over `t`, which passes the rows of the
/// groups that do not fit to disk in partitions.
fn group_or_spill(
    ctx: &Ctx,
    n: &Node,
    t: Table,
    keys: &[VarId],
    aggs: &[(VarId, Agg)],
    report: &mut ExprReport,
    note: &mut Option<String>,
) -> Result<Table> {
    let charged = ctx.spill.is_some() || ctx.mem_limit < u64::MAX || ctx.grant().is_some();
    match super::group::whole(ctx, &t, keys, aggs, charged, true, report) {
        Err(error) if hash::spills(ctx, &error) && super::group::hashable(ctx, aggs) => {
            let Some(shared) = ctx.shared() else {
                return Err(error);
            };
            let mut node = Node::leaf(n.kind.clone(), n.vars.clone(), n.est, String::new());
            node.certain.clone_from(&n.certain);
            node.children = vec![values_leaf(t)];
            let mut out = Table::new(n.vars.clone());
            match super::cursor::spilled_join(&shared, node, &mut out) {
                Ok(()) => {}
                Err(Error::Unsupported(_)) => return Err(error),
                Err(other) => return Err(other),
            }
            let spilled = "[groups did not fit: partitioned to disk]";
            *note = Some(match note.take() {
                Some(note) => format!("{note} {spilled}"),
                None => spilled.to_string(),
            });
            Ok(out)
        }
        other => other,
    }
}

/// The ARQ variance or deviation aggregate a custom aggregate is, if it is one.
pub(super) fn stat_aggregate(func: &AggregateFunction) -> Option<super::aggext::Arq> {
    match func {
        AggregateFunction::Custom(iri) => super::aggext::Arq::of(iri.as_str())
            .filter(|a| matches!(a, super::aggext::Arq::Stat { .. })),
        _ => None,
    }
}

/// How a caller admits transient big-number scratch (directly on `ctx` or by reclaiming
/// speculative headroom from [`super::group::op::GroupMemory`] under budget pressure).
pub(crate) trait ScratchAdmit {
    fn charge_scratch<'a>(&mut self, ctx: &'a Ctx, bytes: u64) -> Result<super::ctx::Charge<'a>> {
        ctx.charge(bytes)
    }
}

pub(super) struct DirectScratch;

impl ScratchAdmit for DirectScratch {}

/// Scratch and retained-subtotal admission for [`AggState::add_admitted`].
pub(super) trait SumAdmit: ScratchAdmit {
    fn admit(&mut self, old: u64, new: u64) -> Result<bool>;
}

struct CallbackAdmit<'a>(&'a mut dyn FnMut(u64) -> Result<()>);

impl ScratchAdmit for CallbackAdmit<'_> {}

impl SumAdmit for CallbackAdmit<'_> {
    fn admit(&mut self, _old: u64, new: u64) -> Result<bool> {
        (self.0)(new)?;
        Ok(true)
    }
}

/// Upper bound on bytes [`Ctx::try_intern_value`] can charge to `ctx.retain_bytes()`
/// for a `Value::Double(x)`. Rust's `f64` `Display` writes fixed-point decimal text
/// (up to 327 bytes for negative subnormals / `~1e-308`, up to 310 bytes near `f64::MAX`),
/// plus typed-literal key framing (`"` + `KEY_SEP` + `^` + `xsd:double`, 43 bytes) and
/// the interner's 96-byte local-vocabulary overhead.
pub(super) fn double_intern_bytes(x: f64) -> u64 {
    if Id::from_f64(x).is_some() {
        return 0;
    }
    const OVERHEAD: u64 = 144;
    if !x.is_finite() {
        return 4 + OVERHEAD;
    }
    let biased = (x.to_bits() >> 52) & 0x7ff;
    let lex_bound = if biased >= 1023 {
        let e2 = biased - 1023;
        ((e2 + 1) * 30_103) / 100_000 + 20
    } else {
        let e2_neg = if biased == 0 { 1074 } else { 1023 - biased };
        (e2_neg * 30_103) / 100_000 + 21
    };
    lex_bound + OVERHEAD
}

/// Heap bytes owned by a `SumAcc::Decimal(Box<DecimalSum>)` allocation.
pub(super) const DECIMAL_SUM_BYTES: u64 = size_of::<DecimalSum>() as u64;
/// Heap bytes owned by a `SumAcc::Int128(Box<Int128Sum>)` allocation.
pub(super) const INT128_SUM_BYTES: u64 = size_of::<Int128Sum>() as u64;

/// Value-space hazard flags tracked on `AggState::Best` so `can_merge` only combines
/// slice winners when `order_cmp` is guaranteed to be a transitive total preorder
/// across every value seen in the merged group.
const BEST_EXACT_NUM: u8 = 1 << 0;
const BEST_FLOAT: u8 = 1 << 1;
const BEST_INCOMPARABLE: u8 = 1 << 2;

#[inline]
fn best_value_flag(v: Option<&Value>) -> u8 {
    match v {
        None => BEST_INCOMPARABLE,
        Some(
            Value::Integer(_) | Value::Decimal(_) | Value::BigInteger(_) | Value::BigDecimal(_),
        ) => BEST_EXACT_NUM,
        Some(Value::Float(f)) => {
            if f32::from(*f).is_nan() {
                BEST_INCOMPARABLE
            } else {
                BEST_FLOAT
            }
        }
        Some(Value::Double(d)) => {
            if f64::from(*d).is_nan() {
                BEST_INCOMPARABLE
            } else {
                BEST_FLOAT
            }
        }
        Some(
            Value::BNode(_)
            | Value::Iri(_)
            | Value::Str(_)
            | Value::Lang(..)
            | Value::LangDir(..)
            | Value::Bool(_),
        ) => 0,
        Some(
            Value::DateTime(_)
            | Value::Date(_)
            | Value::Time(_)
            | Value::Duration(_)
            | Value::YearMonth(_)
            | Value::DayTime(_)
            | Value::Triple(_)
            | Value::Other { .. },
        ) => BEST_INCOMPARABLE,
    }
}

/// Decimal accumulator state with exact prefix bounds so merging two exact sums matches
/// a single sequential pass on every intermediate overflow boundary.
#[derive(Clone)]
pub(super) struct DecimalSum {
    /// Current scaled decimal sum (`value × 10^18`).
    val: i128,
    /// Minimum scaled decimal value across all decimal steps in this partition.
    min: i128,
    /// Maximum scaled decimal value across all decimal steps in this partition.
    max: i128,
    /// Unscaled integer sum just before the first decimal arrived.
    int_before: i128,
    /// Minimum unscaled integer prefix sum before the first decimal arrived (including 0).
    int_min: i128,
    /// Maximum unscaled integer prefix sum before the first decimal arrived (including 0).
    int_max: i128,
    /// Whether `int_min`/`int_max` are exact (false only after `retract`).
    exact_int_prefix: bool,
}

/// Boxed 128-bit integer accumulator with exact prefix bounds, used only when an integer
/// sum overflows `i64` so ordinary `i64` sums keep [`SumAcc`] and [`AggState`] compact.
#[derive(Clone)]
pub(super) struct Int128Sum {
    sum: i128,
    min: i128,
    max: i128,
}

/// Running numeric state of `SUM` / `AVG`.
///
/// Big-number variants keep bounded headroom ([`super::bignum::AGG_MAX_INT_DIGITS`] /
/// [`super::bignum::AGG_MAX_DEC_DIGITS`]) so cancellation and averaging at the digit cap
/// stay independent of row order, while enforcing the final value limits in
/// [`AggState::finish_with`].
#[derive(Clone)]
pub(super) enum SumAcc {
    /// Pure exact integer sum in `i64`, with exact prefix bounds `[min, max]` (including 0).
    Int {
        sum: i64,
        min: i64,
        max: i64,
    },
    /// Pure exact integer sum in `i128`, with exact prefix bounds `[min, max]` (including 0).
    Int128(Box<Int128Sum>),
    /// Pure integer sum after a retraction, whose historical prefix bounds are unknown.
    IntRetracted(i128),
    /// Exact fixed-width decimal sum with prefix bounds for merge verification.
    Decimal(Box<DecimalSum>),
    /// Arbitrary-precision integer sum.
    BigInt(Box<super::bignum::BigInt>),
    /// Arbitrary-precision decimal sum.
    BigDec(Box<super::bignum::BigDec>),
    Float(oxsdatatypes::Float),
    Double(oxsdatatypes::Double),
    /// Failed on `Id::UNDEF` or a non-numeric value: guaranteed to fail any single pass.
    HardError,
    /// Failed on exceeding big-number digit limits: may succeed if preceded by a non-empty
    /// left partition that cancels or promotes to float/double.
    Overflow,
}

impl SumAcc {
    #[inline]
    fn heap_bytes(&self) -> u64 {
        match self {
            SumAcc::Int128(_) => INT128_SUM_BYTES,
            SumAcc::Decimal(_) => DECIMAL_SUM_BYTES,
            SumAcc::BigInt(i) => i.heap_bytes() + 32,
            SumAcc::BigDec(d) => d.heap_bytes() + 32,
            _ => 0,
        }
    }

    #[inline]
    fn int_bounds(&self) -> Option<(i128, i128, i128)> {
        match self {
            SumAcc::Int { sum, min, max } => Some(((*sum).into(), (*min).into(), (*max).into())),
            SumAcc::Int128(b) => Some((b.sum, b.min, b.max)),
            _ => None,
        }
    }

    #[inline]
    fn int_sum(&self) -> Option<i128> {
        match self {
            SumAcc::Int { sum, .. } => Some((*sum).into()),
            SumAcc::Int128(b) => Some(b.sum),
            SumAcc::IntRetracted(s) => Some(*s),
            _ => None,
        }
    }

    #[inline]
    fn is_retracted_int(&self) -> bool {
        matches!(self, SumAcc::IntRetracted(_))
    }

    /// Compute the next `SumAcc` for any transition or arbitrary-precision operation that
    /// could not update `self` in place. Returns any transient scratch charge alongside
    /// the candidate so it stays covered until retained admission completes and the old
    /// subtotal drops (or refusal drops the candidate).
    fn add_value_slow<'a>(
        &self,
        ctx: &'a Ctx,
        rhs: &Value,
        admit: &mut dyn ScratchAdmit,
    ) -> Result<(Self, Option<super::ctx::Charge<'a>>)> {
        use super::bignum::{
            AGG_MAX_DEC_DIGITS, AGG_MAX_INT_DIGITS, AGG_SUM_SCRATCH_BYTES, BigDec, BigInt,
            TO_F32_SCRATCH_BYTES, TO_F64_SCRATCH_BYTES,
        };
        use super::value::Num;
        use oxsdatatypes::{Double, Float};
        let Ok(n) = Num::of(rhs) else {
            return Ok((SumAcc::HardError, None));
        };
        Ok(match (self, n) {
            (SumAcc::Int { sum, min, max }, Num::Integer(b)) => {
                let s = i128::from(*sum) + i128::from(i64::from(b));
                let lo = i128::from(*min).min(s);
                let hi = i128::from(*max).max(s);
                let scratch = admit.charge_scratch(ctx, INT128_SUM_BYTES)?;
                (
                    SumAcc::Int128(Box::new(Int128Sum {
                        sum: s,
                        min: lo,
                        max: hi,
                    })),
                    Some(scratch),
                )
            }
            (SumAcc::Int { .. } | SumAcc::Int128(_), Num::Decimal(b)) => {
                let (int_before, int_min, int_max) = self.int_bounds().expect("int variant");
                if let Some(v) = int_before
                    .checked_mul(super::value::DECIMAL_SCALE)
                    .and_then(|s| s.checked_add(super::value::decimal_raw(b)))
                {
                    let scratch = admit.charge_scratch(ctx, DECIMAL_SUM_BYTES)?;
                    return Ok((
                        SumAcc::Decimal(Box::new(DecimalSum {
                            val: v,
                            min: v,
                            max: v,
                            int_before,
                            int_min,
                            int_max,
                            exact_int_prefix: true,
                        })),
                        Some(scratch),
                    ));
                }
                let scratch = admit.charge_scratch(ctx, AGG_SUM_SCRATCH_BYTES)?;
                let next = BigDec::from_bigint(BigInt::from_i128(int_before))
                    .checked_add_with_limit(&BigDec::from_oxs_decimal(b), AGG_MAX_DEC_DIGITS)
                    .map_or(SumAcc::Overflow, |s| SumAcc::BigDec(Box::new(s)));
                (next, Some(scratch))
            }
            (SumAcc::IntRetracted(sum), Num::Decimal(b)) => {
                let int_before = *sum;
                if let Some(v) = int_before
                    .checked_mul(super::value::DECIMAL_SCALE)
                    .and_then(|s| s.checked_add(super::value::decimal_raw(b)))
                {
                    let scratch = admit.charge_scratch(ctx, DECIMAL_SUM_BYTES)?;
                    return Ok((
                        SumAcc::Decimal(Box::new(DecimalSum {
                            val: v,
                            min: v,
                            max: v,
                            int_before,
                            int_min: 0,
                            int_max: 0,
                            exact_int_prefix: false,
                        })),
                        Some(scratch),
                    ));
                }
                let scratch = admit.charge_scratch(ctx, AGG_SUM_SCRATCH_BYTES)?;
                let next = BigDec::from_bigint(BigInt::from_i128(int_before))
                    .checked_add_with_limit(&BigDec::from_oxs_decimal(b), AGG_MAX_DEC_DIGITS)
                    .map_or(SumAcc::Overflow, |s| SumAcc::BigDec(Box::new(s)));
                (next, Some(scratch))
            }
            (SumAcc::Int128(_) | SumAcc::IntRetracted(_), Num::Integer(b)) => {
                let sum = self.int_sum().expect("int variant");
                let scratch = admit.charge_scratch(ctx, AGG_SUM_SCRATCH_BYTES)?;
                let next = BigInt::from_i128(sum)
                    .checked_add_with_limit(&BigInt::from_i64(b.into()), AGG_MAX_INT_DIGITS)
                    .map_or(SumAcc::Overflow, |s| SumAcc::BigInt(Box::new(s)));
                (next, Some(scratch))
            }
            (
                SumAcc::Int { .. } | SumAcc::Int128(_) | SumAcc::IntRetracted(_),
                Num::BigInteger(b),
            ) => {
                let sum = self.int_sum().expect("int variant");
                let scratch = admit.charge_scratch(ctx, AGG_SUM_SCRATCH_BYTES)?;
                let next = BigInt::from_i128(sum)
                    .checked_add_with_limit(b, AGG_MAX_INT_DIGITS)
                    .map_or(SumAcc::Overflow, |s| SumAcc::BigInt(Box::new(s)));
                (next, Some(scratch))
            }
            (
                SumAcc::Int { .. } | SumAcc::Int128(_) | SumAcc::IntRetracted(_),
                Num::BigDecimal(b),
            ) => {
                let sum = self.int_sum().expect("int variant");
                let scratch = admit.charge_scratch(ctx, AGG_SUM_SCRATCH_BYTES)?;
                let next = BigDec::from_bigint(BigInt::from_i128(sum))
                    .checked_add_with_limit(b, AGG_MAX_DEC_DIGITS)
                    .map_or(SumAcc::Overflow, |s| SumAcc::BigDec(Box::new(s)));
                (next, Some(scratch))
            }
            (SumAcc::Int { .. } | SumAcc::Int128(_) | SumAcc::IntRetracted(_), Num::Float(b)) => {
                let sum = self.int_sum().expect("int variant");
                (SumAcc::Float(Float::from(sum as f32) + b), None)
            }
            (SumAcc::Int { .. } | SumAcc::Int128(_) | SumAcc::IntRetracted(_), Num::Double(b)) => {
                let sum = self.int_sum().expect("int variant");
                (SumAcc::Double(Double::from(sum as f64) + b), None)
            }
            (SumAcc::Decimal(ds), Num::Integer(b)) => {
                let scratch = admit.charge_scratch(ctx, AGG_SUM_SCRATCH_BYTES)?;
                let a = super::value::decimal_from_raw(ds.val);
                let next = BigDec::from_oxs_decimal(a)
                    .checked_add_with_limit(&BigDec::from_i64(b.into()), AGG_MAX_DEC_DIGITS)
                    .map_or(SumAcc::Overflow, |s| SumAcc::BigDec(Box::new(s)));
                (next, Some(scratch))
            }
            (SumAcc::Decimal(ds), Num::Decimal(b)) => {
                let scratch = admit.charge_scratch(ctx, AGG_SUM_SCRATCH_BYTES)?;
                let a = super::value::decimal_from_raw(ds.val);
                let next = BigDec::from_oxs_decimal(a)
                    .checked_add_with_limit(&BigDec::from_oxs_decimal(b), AGG_MAX_DEC_DIGITS)
                    .map_or(SumAcc::Overflow, |s| SumAcc::BigDec(Box::new(s)));
                (next, Some(scratch))
            }
            (SumAcc::Decimal(ds), Num::BigInteger(b)) => {
                let scratch = admit.charge_scratch(ctx, AGG_SUM_SCRATCH_BYTES)?;
                let a = super::value::decimal_from_raw(ds.val);
                let next = BigDec::from_oxs_decimal(a)
                    .checked_add_with_limit(&BigDec::from_bigint(b.clone()), AGG_MAX_DEC_DIGITS)
                    .map_or(SumAcc::Overflow, |s| SumAcc::BigDec(Box::new(s)));
                (next, Some(scratch))
            }
            (SumAcc::Decimal(ds), Num::BigDecimal(b)) => {
                let scratch = admit.charge_scratch(ctx, AGG_SUM_SCRATCH_BYTES)?;
                let a = super::value::decimal_from_raw(ds.val);
                let next = BigDec::from_oxs_decimal(a)
                    .checked_add_with_limit(b, AGG_MAX_DEC_DIGITS)
                    .map_or(SumAcc::Overflow, |s| SumAcc::BigDec(Box::new(s)));
                (next, Some(scratch))
            }
            (SumAcc::Decimal(ds), Num::Float(b)) => {
                let a = super::value::decimal_from_raw(ds.val);
                (SumAcc::Float(Float::from(a) + b), None)
            }
            (SumAcc::Decimal(ds), Num::Double(b)) => {
                let a = super::value::decimal_from_raw(ds.val);
                (SumAcc::Double(Double::from(a) + b), None)
            }
            (SumAcc::BigInt(a), Num::Integer(b)) => {
                let scratch = admit.charge_scratch(ctx, AGG_SUM_SCRATCH_BYTES)?;
                let next = a
                    .checked_add_with_limit(&BigInt::from_i64(b.into()), AGG_MAX_INT_DIGITS)
                    .map_or(SumAcc::Overflow, |s| SumAcc::BigInt(Box::new(s)));
                (next, Some(scratch))
            }
            (SumAcc::BigInt(a), Num::BigInteger(b)) => {
                let scratch = admit.charge_scratch(ctx, AGG_SUM_SCRATCH_BYTES)?;
                let next = a
                    .checked_add_with_limit(b, AGG_MAX_INT_DIGITS)
                    .map_or(SumAcc::Overflow, |s| SumAcc::BigInt(Box::new(s)));
                (next, Some(scratch))
            }
            (SumAcc::BigInt(a), Num::Decimal(b)) => {
                let scratch = admit.charge_scratch(ctx, AGG_SUM_SCRATCH_BYTES)?;
                let next = BigDec::from_bigint((**a).clone())
                    .checked_add_with_limit(&BigDec::from_oxs_decimal(b), AGG_MAX_DEC_DIGITS)
                    .map_or(SumAcc::Overflow, |s| SumAcc::BigDec(Box::new(s)));
                (next, Some(scratch))
            }
            (SumAcc::BigInt(a), Num::BigDecimal(b)) => {
                let scratch = admit.charge_scratch(ctx, AGG_SUM_SCRATCH_BYTES)?;
                let next = BigDec::from_bigint((**a).clone())
                    .checked_add_with_limit(b, AGG_MAX_DEC_DIGITS)
                    .map_or(SumAcc::Overflow, |s| SumAcc::BigDec(Box::new(s)));
                (next, Some(scratch))
            }
            (SumAcc::BigInt(a), Num::Float(b)) => {
                let _scratch = admit.charge_scratch(ctx, TO_F32_SCRATCH_BYTES)?;
                (SumAcc::Float(Float::from(a.to_f32()) + b), None)
            }
            (SumAcc::BigInt(a), Num::Double(b)) => {
                let _scratch = admit.charge_scratch(ctx, TO_F64_SCRATCH_BYTES)?;
                (SumAcc::Double(Double::from(a.to_f64()) + b), None)
            }
            (SumAcc::BigDec(a), Num::Integer(b)) => {
                let scratch = admit.charge_scratch(ctx, AGG_SUM_SCRATCH_BYTES)?;
                let next = a
                    .checked_add_with_limit(&BigDec::from_i64(b.into()), AGG_MAX_DEC_DIGITS)
                    .map_or(SumAcc::Overflow, |s| SumAcc::BigDec(Box::new(s)));
                (next, Some(scratch))
            }
            (SumAcc::BigDec(a), Num::BigInteger(b)) => {
                let scratch = admit.charge_scratch(ctx, AGG_SUM_SCRATCH_BYTES)?;
                let next = a
                    .checked_add_with_limit(&BigDec::from_bigint(b.clone()), AGG_MAX_DEC_DIGITS)
                    .map_or(SumAcc::Overflow, |s| SumAcc::BigDec(Box::new(s)));
                (next, Some(scratch))
            }
            (SumAcc::BigDec(a), Num::Decimal(b)) => {
                let scratch = admit.charge_scratch(ctx, AGG_SUM_SCRATCH_BYTES)?;
                let next = a
                    .checked_add_with_limit(&BigDec::from_oxs_decimal(b), AGG_MAX_DEC_DIGITS)
                    .map_or(SumAcc::Overflow, |s| SumAcc::BigDec(Box::new(s)));
                (next, Some(scratch))
            }
            (SumAcc::BigDec(a), Num::BigDecimal(b)) => {
                let scratch = admit.charge_scratch(ctx, AGG_SUM_SCRATCH_BYTES)?;
                let next = a
                    .checked_add_with_limit(b, AGG_MAX_DEC_DIGITS)
                    .map_or(SumAcc::Overflow, |s| SumAcc::BigDec(Box::new(s)));
                (next, Some(scratch))
            }
            (SumAcc::BigDec(a), Num::Float(b)) => {
                let _scratch = admit.charge_scratch(ctx, TO_F32_SCRATCH_BYTES)?;
                (SumAcc::Float(Float::from(a.to_f32()) + b), None)
            }
            (SumAcc::BigDec(a), Num::Double(b)) => {
                let _scratch = admit.charge_scratch(ctx, TO_F64_SCRATCH_BYTES)?;
                (SumAcc::Double(Double::from(a.to_f64()) + b), None)
            }
            (SumAcc::Float(a), Num::Double(b)) => (SumAcc::Double(Double::from(*a) + b), None),
            (SumAcc::Float(a), Num::BigInteger(b)) => {
                let _scratch = admit.charge_scratch(ctx, TO_F32_SCRATCH_BYTES)?;
                (SumAcc::Float(*a + Float::from(b.to_f32())), None)
            }
            (SumAcc::Float(a), Num::BigDecimal(b)) => {
                let _scratch = admit.charge_scratch(ctx, TO_F32_SCRATCH_BYTES)?;
                (SumAcc::Float(*a + Float::from(b.to_f32())), None)
            }
            (SumAcc::Float(a), other) => (SumAcc::Float(*a + other.to_float()), None),
            (SumAcc::Double(a), Num::BigInteger(b)) => {
                let _scratch = admit.charge_scratch(ctx, TO_F64_SCRATCH_BYTES)?;
                (SumAcc::Double(*a + Double::from(b.to_f64())), None)
            }
            (SumAcc::Double(a), Num::BigDecimal(b)) => {
                let _scratch = admit.charge_scratch(ctx, TO_F64_SCRATCH_BYTES)?;
                (SumAcc::Double(*a + Double::from(b.to_f64())), None)
            }
            (SumAcc::Double(a), other) => (SumAcc::Double(*a + other.to_double()), None),
            (SumAcc::HardError | SumAcc::Overflow, _) => unreachable!(),
        })
    }
}

/// Running state of one aggregate of one group (same results as [`aggregate`]).
#[derive(Clone)]
pub(super) enum AggState {
    /// COUNT(*) / COUNT(?v): rows / bound values
    Count(u64),
    /// SUM / AVG: an exact integer sum while every value is an integer, then decimal,
    /// arbitrary-precision integer/decimal, float or double, or a recorded failure. `errs`
    /// counts rows whose argument was `Id::UNDEF`, for the reading in which AVG over only
    /// errors is zero.
    Sum {
        acc: SumAcc,
        rows: u64,
        errs: u64,
        deferred: bool,
    },
    /// MIN / MAX: the best id and its value, the count of arguments that were errors
    /// (which some readings make the result unbound, see [`super::AggregateErrors`]),
    /// and a value-space hazard bitmask for exact merge validation.
    Best(Option<Id>, Option<Value>, u64, u8),
    Sample(Option<Id>),
    /// ARQ's variance and deviation aggregates
    Stat(super::aggext::Arq, super::aggext::StatAcc),
}

const _: () = assert!(size_of::<AggState>() == 80);

impl AggState {
    pub(super) fn new(agg: &Agg) -> AggState {
        if let Some(arq) = stat_aggregate(&agg.func) {
            return AggState::Stat(arq, Default::default());
        }
        match agg.func {
            AggregateFunction::Sum | AggregateFunction::Avg => AggState::Sum {
                acc: SumAcc::Int {
                    sum: 0,
                    min: 0,
                    max: 0,
                },
                rows: 0,
                errs: 0,
                deferred: false,
            },
            AggregateFunction::Min | AggregateFunction::Max => AggState::Best(None, None, 0, 0),
            AggregateFunction::Sample => AggState::Sample(None),
            _ => AggState::Count(0),
        }
    }

    /// Owned heap bytes held by a `SUM`/`AVG` subtotal (`Box<DecimalSum>`, `Box<BigInt>`,
    /// or `Box<BigDec>`).
    #[inline]
    pub(super) fn sum_heap_bytes(&self) -> u64 {
        match self {
            AggState::Sum { acc, .. } => acc.heap_bytes(),
            _ => 0,
        }
    }

    /// Upper bound on bytes [`AggState::finish_with`] can charge to `ctx.retain_bytes()`
    /// when interning the finalized result value.
    pub(super) fn finish_intern_bytes(&self, agg: &Agg) -> u64 {
        match self {
            AggState::Count(_) | AggState::Best(..) | AggState::Sample(_) => 0,
            AggState::Sum {
                acc, rows, errs, ..
            } => {
                let is_avg = matches!(agg.func, AggregateFunction::Avg);
                if *rows > 0 && *errs == *rows && is_avg {
                    return 0;
                }
                match acc {
                    SumAcc::HardError | SumAcc::Overflow => 0,
                    SumAcc::Int { .. } | SumAcc::Int128(_) | SumAcc::IntRetracted(_) => {
                        let sum = acc.int_sum().expect("int variant");
                        if is_avg {
                            if *rows == 0 {
                                0
                            } else {
                                let needs_big = match (i64::try_from(sum), i64::try_from(*rows)) {
                                    (Ok(i), Ok(r)) => super::value::int_div_needs_big(i, r),
                                    _ => true,
                                };
                                if needs_big {
                                    super::bignum::AGG_SUM_SCRATCH_BYTES + 4500
                                } else {
                                    256
                                }
                            }
                        } else if i64::try_from(sum)
                            .ok()
                            .is_some_and(|i| Id::from_i64(i).is_some())
                        {
                            0
                        } else {
                            256
                        }
                    }
                    SumAcc::Decimal(ds) => {
                        if is_avg {
                            if *rows == 0 {
                                0
                            } else {
                                let d = super::value::decimal_from_raw(ds.val);
                                let needs_big = match i64::try_from(*rows)
                                    .ok()
                                    .map(oxsdatatypes::Decimal::from)
                                {
                                    Some(rd) => {
                                        super::value::decimal_div_needs_big(d, rd)
                                            || d.checked_div(rd).is_none()
                                    }
                                    None => true,
                                };
                                if needs_big {
                                    super::bignum::AGG_SUM_SCRATCH_BYTES + 4500
                                } else {
                                    256
                                }
                            }
                        } else {
                            256
                        }
                    }
                    SumAcc::BigInt(bi) => {
                        if is_avg {
                            super::bignum::AGG_SUM_SCRATCH_BYTES + 4500
                        } else {
                            super::bignum::intern_scratch_bytes(bi.lexical_len())
                                .saturating_add(bi.lexical_len() as u64 + 144)
                        }
                    }
                    SumAcc::BigDec(bd) => {
                        if is_avg {
                            super::bignum::AGG_SUM_SCRATCH_BYTES + 4500
                        } else {
                            super::bignum::intern_scratch_bytes(bd.lexical_len())
                                .saturating_add(bd.lexical_len() as u64 + 144)
                        }
                    }
                    SumAcc::Float(_) => 256,
                    SumAcc::Double(d) => {
                        let x = if is_avg {
                            if *rows == 0 {
                                return 0;
                            }
                            f64::from(*d) / (*rows as f64)
                        } else {
                            f64::from(*d)
                        };
                        double_intern_bytes(x)
                    }
                }
            }
            AggState::Stat(arq, acc) => acc.finish_value(*arq).map_or(0, double_intern_bytes),
        }
    }

    /// Add one row; `id` is the aggregated column's value (`None` for COUNT(*)).
    /// `on_payload` admits any retained `MIN`/`MAX` or `SUM`/`AVG` payload before
    /// replacing the state.
    #[inline(always)]
    pub(super) fn add(
        &mut self,
        ctx: &Ctx,
        agg: &Agg,
        id: Option<Id>,
        on_payload: &mut dyn FnMut(u64) -> Result<()>,
    ) -> Result<()> {
        let mut admit = CallbackAdmit(on_payload);
        let _ = self.add_admitted(ctx, agg, id, &mut admit)?;
        Ok(())
    }

    /// Whether later arguments of this subtotal must be replayed from disk.
    pub(super) fn sum_deferred(&self) -> bool {
        matches!(self, Self::Sum { deferred: true, .. })
    }

    pub(super) fn defer_sum(&mut self) {
        if let Self::Sum { deferred, .. } = self {
            *deferred = true;
        }
    }

    /// Try one row. If a subtotal's new heap capacity is refused, preserve its
    /// prefix so the caller can replay this argument and the rest of the group.
    #[inline(always)]
    pub(super) fn add_admitted(
        &mut self,
        ctx: &Ctx,
        agg: &Agg,
        id: Option<Id>,
        admit: &mut dyn SumAdmit,
    ) -> Result<bool> {
        match self {
            AggState::Count(n) => *n += id.is_none_or(|id| !id.is_undef()) as u64,
            AggState::Sum { acc, rows, .. } => {
                *rows += 1;
                let id = id.unwrap_or(Id::UNDEF);
                if let SumAcc::Int { sum, min, max } = acc
                    && id.tag() == crate::id::Tag::Int
                {
                    let x = id.as_i64();
                    if let Some(s) = sum.checked_add(x) {
                        *sum = s;
                        if x >= 0 {
                            if s > *max {
                                *max = s;
                            }
                        } else if s < *min {
                            *min = s;
                        }
                        return Ok(true);
                    }
                }
                return self.add_sum_slow(ctx, id, admit);
            }
            AggState::Best(best, best_v, errs, flags) => {
                let Some(id) = id.filter(|id| !id.is_undef()) else {
                    *errs += 1;
                    return Ok(true);
                };
                let decode = ctx.charge(0)?;
                let v = ctx.value_checked_into(id, &decode)?;
                *flags |= best_value_flag(v.as_ref());
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
                    let _ = admit.admit(0, key_payload(&v))?;
                    *best = Some(id);
                    *best_v = v;
                } else {
                    drop(v);
                }
                drop(decode);
            }
            AggState::Sample(x) => {
                if x.is_none() {
                    *x = id.filter(|id| !id.is_undef());
                }
            }
            AggState::Stat(_, acc) => acc.add(ctx, id)?,
        }
        Ok(true)
    }

    #[cold]
    #[inline(never)]
    fn add_sum_slow(&mut self, ctx: &Ctx, id: Id, admit: &mut dyn SumAdmit) -> Result<bool> {
        let AggState::Sum {
            acc, rows, errs, ..
        } = self
        else {
            unreachable!()
        };
        let err = u64::from(id.is_undef());
        *errs += err;
        if matches!(acc, SumAcc::HardError | SumAcc::Overflow) {
            return Ok(true);
        }
        if id.is_undef() {
            let old_bytes = acc.heap_bytes();
            if old_bytes != 0 {
                let _ = admit.admit(old_bytes, 0)?;
            }
            *acc = SumAcc::HardError;
            return Ok(true);
        }
        match (&mut *acc, id.tag()) {
            (SumAcc::Int { sum, min, max }, crate::id::Tag::Int) => {
                let x = id.as_i64();
                if let Some(s) = sum.checked_add(x) {
                    *sum = s;
                    if x >= 0 {
                        if s > *max {
                            *max = s;
                        }
                    } else if s < *min {
                        *min = s;
                    }
                    return Ok(true);
                }
                let s = i128::from(*sum) + i128::from(x);
                let lo = i128::from(*min).min(s);
                let hi = i128::from(*max).max(s);
                match admit.admit(0, INT128_SUM_BYTES) {
                    Ok(true) => {
                        *acc = SumAcc::Int128(Box::new(Int128Sum {
                            sum: s,
                            min: lo,
                            max: hi,
                        }));
                        return Ok(true);
                    }
                    Ok(false) => {
                        *rows -= 1;
                        *errs -= err;
                        return Ok(false);
                    }
                    Err(error) => {
                        *rows -= 1;
                        *errs -= err;
                        return Err(error);
                    }
                }
            }
            (SumAcc::Int128(b), crate::id::Tag::Int) => {
                let x = id.as_i64();
                if let Some(s) = b.sum.checked_add(x.into()) {
                    b.sum = s;
                    if x >= 0 {
                        if s > b.max {
                            b.max = s;
                        }
                    } else if s < b.min {
                        b.min = s;
                    }
                    return Ok(true);
                }
            }
            (SumAcc::IntRetracted(sum), crate::id::Tag::Int) => {
                if let Some(s) = sum.checked_add(id.as_i64().into()) {
                    *sum = s;
                    return Ok(true);
                }
            }
            (SumAcc::Decimal(ds), crate::id::Tag::Int) => {
                let raw = i128::from(id.as_i64()) * super::value::DECIMAL_SCALE;
                if let Some(v) = ds.val.checked_add(raw) {
                    ds.val = v;
                    ds.min = ds.min.min(v);
                    ds.max = ds.max.max(v);
                    return Ok(true);
                }
            }
            (SumAcc::Float(f), crate::id::Tag::Int) => {
                *f = *f + oxsdatatypes::Float::from(oxsdatatypes::Integer::from(id.as_i64()));
                return Ok(true);
            }
            (SumAcc::Float(f), crate::id::Tag::Double) => {
                *acc = SumAcc::Double(
                    oxsdatatypes::Double::from(*f) + oxsdatatypes::Double::from(id.as_f64()),
                );
                return Ok(true);
            }
            (SumAcc::Double(d), crate::id::Tag::Int) => {
                *d = *d + oxsdatatypes::Double::from(oxsdatatypes::Integer::from(id.as_i64()));
                return Ok(true);
            }
            (SumAcc::Double(d), crate::id::Tag::Double) => {
                *d = *d + oxsdatatypes::Double::from(id.as_f64());
                return Ok(true);
            }
            _ => {}
        }
        let value = match ctx.value_checked(id) {
            Ok(v) => v,
            Err(error) => {
                *rows -= 1;
                *errs -= err;
                return Err(error);
            }
        };
        let Some(value) = value else {
            let old_bytes = acc.heap_bytes();
            if old_bytes != 0 {
                let _ = admit.admit(old_bytes, 0)?;
            }
            *acc = SumAcc::HardError;
            return Ok(true);
        };
        // Fast in-place updates that never change heap ownership or need big-number scratch,
        // plus fixed-size boxed transitions admitted strictly before allocation so spilling
        // refusal preserves the admitted prefix without allocating.
        match (&mut *acc, &value) {
            (SumAcc::Int { sum, min, max }, Value::Integer(i)) => {
                let x = i64::from(*i);
                if let Some(s) = sum.checked_add(x) {
                    *sum = s;
                    if x >= 0 {
                        if s > *max {
                            *max = s;
                        }
                    } else if s < *min {
                        *min = s;
                    }
                    return Ok(true);
                }
                let s = i128::from(*sum) + i128::from(x);
                let lo = i128::from(*min).min(s);
                let hi = i128::from(*max).max(s);
                match admit.admit(0, INT128_SUM_BYTES) {
                    Ok(true) => {
                        *acc = SumAcc::Int128(Box::new(Int128Sum {
                            sum: s,
                            min: lo,
                            max: hi,
                        }));
                        return Ok(true);
                    }
                    Ok(false) => {
                        *rows -= 1;
                        *errs -= err;
                        return Ok(false);
                    }
                    Err(error) => {
                        *rows -= 1;
                        *errs -= err;
                        return Err(error);
                    }
                }
            }
            (
                SumAcc::Int { .. } | SumAcc::Int128(_) | SumAcc::IntRetracted(_),
                Value::Decimal(d),
            ) => {
                let (int_before, int_min, int_max, exact_int_prefix) = match &*acc {
                    SumAcc::Int { sum, min, max } => {
                        (i128::from(*sum), i128::from(*min), i128::from(*max), true)
                    }
                    SumAcc::Int128(b) => (b.sum, b.min, b.max, true),
                    SumAcc::IntRetracted(sum) => (*sum, 0, 0, false),
                    _ => unreachable!(),
                };
                if let Some(v) = int_before
                    .checked_mul(super::value::DECIMAL_SCALE)
                    .and_then(|s| s.checked_add(super::value::decimal_raw(*d)))
                {
                    let old_bytes = acc.heap_bytes();
                    match admit.admit(old_bytes, DECIMAL_SUM_BYTES) {
                        Ok(true) => {
                            *acc = SumAcc::HardError;
                            *acc = SumAcc::Decimal(Box::new(DecimalSum {
                                val: v,
                                min: v,
                                max: v,
                                int_before,
                                int_min,
                                int_max,
                                exact_int_prefix,
                            }));
                            return Ok(true);
                        }
                        Ok(false) => {
                            *rows -= 1;
                            *errs -= err;
                            return Ok(false);
                        }
                        Err(error) => {
                            *rows -= 1;
                            *errs -= err;
                            return Err(error);
                        }
                    }
                }
            }
            (SumAcc::Int128(b), Value::Integer(i)) => {
                let x = i64::from(*i);
                if let Some(s) = b.sum.checked_add(x.into()) {
                    b.sum = s;
                    if x >= 0 {
                        if s > b.max {
                            b.max = s;
                        }
                    } else if s < b.min {
                        b.min = s;
                    }
                    return Ok(true);
                }
            }
            (SumAcc::IntRetracted(sum), Value::Integer(i)) => {
                if let Some(s) = sum.checked_add(i64::from(*i).into()) {
                    *sum = s;
                    return Ok(true);
                }
            }
            (SumAcc::Decimal(ds), Value::Integer(i)) => {
                let raw = i128::from(i64::from(*i)) * super::value::DECIMAL_SCALE;
                if let Some(v) = ds.val.checked_add(raw) {
                    ds.val = v;
                    ds.min = ds.min.min(v);
                    ds.max = ds.max.max(v);
                    return Ok(true);
                }
            }
            (SumAcc::Decimal(ds), Value::Decimal(d)) => {
                if let Some(v) = ds.val.checked_add(super::value::decimal_raw(*d)) {
                    ds.val = v;
                    ds.min = ds.min.min(v);
                    ds.max = ds.max.max(v);
                    return Ok(true);
                }
            }
            (SumAcc::Float(f), Value::Integer(i)) => {
                *f = *f + oxsdatatypes::Float::from(*i);
                return Ok(true);
            }
            (SumAcc::Float(f), Value::Decimal(d)) => {
                *f = *f + oxsdatatypes::Float::from(*d);
                return Ok(true);
            }
            (SumAcc::Float(f), Value::Float(g)) => {
                *f = *f + *g;
                return Ok(true);
            }
            (SumAcc::Float(f), Value::Double(d)) => {
                *acc = SumAcc::Double(oxsdatatypes::Double::from(*f) + *d);
                return Ok(true);
            }
            (SumAcc::Double(d), Value::Integer(i)) => {
                *d = *d + oxsdatatypes::Double::from(*i);
                return Ok(true);
            }
            (SumAcc::Double(d), Value::Decimal(x)) => {
                *d = *d + oxsdatatypes::Double::from(*x);
                return Ok(true);
            }
            (SumAcc::Double(d), Value::Float(f)) => {
                *d = *d + oxsdatatypes::Double::from(*f);
                return Ok(true);
            }
            (SumAcc::Double(d), Value::Double(x)) => {
                *d = *d + *x;
                return Ok(true);
            }
            _ => {}
        }

        let (next, scratch) = match acc.add_value_slow(ctx, &value, admit) {
            Ok(pair) => pair,
            Err(error) => {
                *rows -= 1;
                *errs -= err;
                return Err(error);
            }
        };
        let old_bytes = acc.heap_bytes();
        let new_bytes = next.heap_bytes();
        // All arithmetic temporaries have been dropped; narrow the scratch guard to cover
        // the live candidate until retained admission completes and the old subtotal drops,
        // or until refusal drops the candidate.
        if let Some(guard) = &scratch
            && let Err(error) = guard.resize(new_bytes)
        {
            drop(next);
            drop(scratch);
            *rows -= 1;
            *errs -= err;
            return Err(error);
        }
        if old_bytes != new_bytes {
            match admit.admit(old_bytes, new_bytes) {
                Ok(true) => {}
                Ok(false) => {
                    drop(next);
                    drop(scratch);
                    *rows -= 1;
                    *errs -= err;
                    return Ok(false);
                }
                Err(error) => {
                    drop(next);
                    drop(scratch);
                    *rows -= 1;
                    *errs -= err;
                    return Err(error);
                }
            }
        }
        let old = std::mem::replace(acc, next);
        drop(old);
        drop(scratch);
        Ok(true)
    }

    /// Whether [`AggState::merge`] of `other` into `self` is guaranteed to match a single
    /// sequential pass bit-for-bit without mutating either state.
    pub(super) fn can_merge(&self, other: &AggState) -> bool {
        match (self, other) {
            (AggState::Count(_), AggState::Count(_))
            | (AggState::Sample(_), AggState::Sample(_)) => true,
            (AggState::Best(lb, _, _, lf), AggState::Best(rb, _, _, rf)) => {
                if lb.is_none() || rb.is_none() {
                    return true;
                }
                let combined = lf | rf;
                (combined & BEST_INCOMPARABLE) == 0
                    && (combined & (BEST_EXACT_NUM | BEST_FLOAT)) != (BEST_EXACT_NUM | BEST_FLOAT)
            }
            (
                AggState::Sum {
                    acc: la,
                    rows: lr,
                    deferred: ld,
                    ..
                },
                AggState::Sum {
                    acc: ra,
                    rows: rr,
                    errs: re,
                    deferred: rd,
                },
            ) => {
                if *ld || *rd {
                    return false;
                }
                if *lr == 0 || *rr == 0 {
                    return true;
                }
                if matches!(la, SumAcc::HardError | SumAcc::Overflow) {
                    return true;
                }
                if matches!(ra, SumAcc::HardError) {
                    return true;
                }
                if matches!(ra, SumAcc::Overflow) {
                    return *re > 0;
                }
                match (la, ra) {
                    (l, r) if l.int_bounds().is_some() && r.int_bounds().is_some() => {
                        let (ls, _, _) = l.int_bounds().expect("checked above");
                        let (rs, rmin, rmax) = r.int_bounds().expect("checked above");
                        ls.checked_add(rs).is_some()
                            && ls.checked_add(rmin).is_some()
                            && ls.checked_add(rmax).is_some()
                    }
                    (l, r) if l.int_sum().is_some() && r.int_sum().is_some() => {
                        let ls = l.int_sum().expect("checked above");
                        let rs = r.int_sum().expect("checked above");
                        ls.checked_add(rs).is_some()
                    }
                    (SumAcc::Decimal(l), r) if r.int_bounds().is_some() => {
                        let (osum, omin, omax) = r.int_bounds().expect("checked above");
                        super::value::add_scaled_int(l.val, omin).is_some()
                            && super::value::add_scaled_int(l.val, omax).is_some()
                            && super::value::add_scaled_int(l.val, osum).is_some()
                    }
                    (l, SumAcc::Decimal(r)) if l.int_bounds().is_some() => {
                        let (ls, _, _) = l.int_bounds().expect("checked above");
                        r.exact_int_prefix
                            && ls
                                .checked_add(r.int_before)
                                .and_then(|t| t.checked_mul(super::value::DECIMAL_SCALE))
                                .is_some()
                            && ls.checked_add(r.int_min).is_some()
                            && ls.checked_add(r.int_max).is_some()
                            && super::value::add_scaled_int(r.min, ls).is_some()
                            && super::value::add_scaled_int(r.max, ls).is_some()
                            && super::value::add_scaled_int(r.val, ls).is_some()
                    }
                    (SumAcc::Decimal(l), SumAcc::Decimal(r)) => {
                        r.exact_int_prefix
                            && super::value::add_scaled_int(l.val, r.int_min).is_some()
                            && super::value::add_scaled_int(l.val, r.int_max).is_some()
                            && l.val.checked_add(r.min).is_some()
                            && l.val.checked_add(r.max).is_some()
                            && l.val.checked_add(r.val).is_some()
                    }
                    (SumAcc::Decimal(_), r) if r.is_retracted_int() => false,
                    (l, SumAcc::Decimal(_)) if l.is_retracted_int() => false,
                    (SumAcc::BigInt(_) | SumAcc::BigDec(_), _)
                    | (_, SumAcc::BigInt(_) | SumAcc::BigDec(_)) => false,
                    (SumAcc::Float(_) | SumAcc::Double(_), r) if r.is_retracted_int() => false,
                    (
                        SumAcc::Float(_) | SumAcc::Double(_),
                        SumAcc::Int { .. }
                        | SumAcc::Int128(_)
                        | SumAcc::Decimal(_)
                        | SumAcc::Float(_)
                        | SumAcc::Double(_),
                    )
                    | (
                        SumAcc::Int { .. }
                        | SumAcc::Int128(_)
                        | SumAcc::IntRetracted(_)
                        | SumAcc::Decimal(_),
                        SumAcc::Float(_) | SumAcc::Double(_),
                    ) => *rr == 1,
                    (SumAcc::HardError | SumAcc::Overflow, _)
                    | (_, SumAcc::HardError | SumAcc::Overflow) => unreachable!(),
                    _ => false,
                }
            }
            _ => false,
        }
    }

    /// Exact heap bytes that `self` will own after [`AggState::merge`] folds `other` into
    /// `self`, assuming `self.can_merge(other)` is true. Allocates nothing.
    pub(super) fn merged_sum_heap_bytes(&self, other: &AggState) -> u64 {
        let (
            AggState::Sum {
                acc: la, rows: lr, ..
            },
            AggState::Sum {
                acc: ra,
                rows: rr,
                errs: re,
                ..
            },
        ) = (self, other)
        else {
            return 0;
        };
        if *rr == 0 {
            return la.heap_bytes();
        }
        if *lr == 0 {
            return ra.heap_bytes();
        }
        if matches!(la, SumAcc::HardError | SumAcc::Overflow) {
            return la.heap_bytes();
        }
        if matches!(ra, SumAcc::HardError) || (matches!(ra, SumAcc::Overflow) && *re > 0) {
            return 0;
        }
        match (la, ra) {
            (l, r) if l.int_bounds().is_some() && r.int_bounds().is_some() => {
                let (base, lmin, lmax) = l.int_bounds().expect("checked above");
                let (osum, omin, omax) = r.int_bounds().expect("checked above");
                let new_sum = base + osum;
                let new_min = lmin.min(base + omin);
                let new_max = lmax.max(base + omax);
                if i64::try_from(new_sum).is_ok()
                    && i64::try_from(new_min).is_ok()
                    && i64::try_from(new_max).is_ok()
                {
                    0
                } else {
                    INT128_SUM_BYTES
                }
            }
            (l, r) if l.int_sum().is_some() && r.int_sum().is_some() => 0,
            (SumAcc::Decimal(_), r) if r.int_bounds().is_some() => DECIMAL_SUM_BYTES,
            (l, SumAcc::Decimal(_)) if l.int_bounds().is_some() => DECIMAL_SUM_BYTES,
            (SumAcc::Decimal(_), SumAcc::Decimal(_)) => DECIMAL_SUM_BYTES,
            _ => 0,
        }
    }

    /// Fold `other`, the state of the same aggregate over rows that come after this
    /// state's rows, into this one, so that the result is what one state over all the
    /// rows in that order gives. `false`, with this state unchanged, for a state that
    /// cannot be combined exactly, such as ARQ's shifted sums, multi-row float/double
    /// subtotals, big-number arithmetic merges, or a right-side numeric overflow that a
    /// single pass might have avoided.
    pub(super) fn merge(&mut self, ctx: &Ctx, agg: &Agg, other: AggState) -> bool {
        if !self.can_merge(&other) {
            let _ = ctx;
            return false;
        }
        match (&mut *self, other) {
            (AggState::Count(a), AggState::Count(b)) => *a += b,
            (
                AggState::Sum {
                    acc, rows, errs, ..
                },
                AggState::Sum {
                    acc: oacc,
                    rows: orows,
                    errs: oerrs,
                    ..
                },
            ) => {
                if orows == 0 {
                    return true;
                }
                if *rows == 0 {
                    *acc = oacc;
                    *rows = orows;
                    *errs = oerrs;
                    return true;
                }
                *rows += orows;
                *errs += oerrs;
                if matches!(acc, SumAcc::HardError | SumAcc::Overflow) {
                    return true;
                }
                if matches!(oacc, SumAcc::HardError)
                    || (matches!(oacc, SumAcc::Overflow) && oerrs > 0)
                {
                    *acc = SumAcc::HardError;
                    return true;
                }
                match (&mut *acc, oacc) {
                    (l, r) if l.int_bounds().is_some() && r.int_bounds().is_some() => {
                        let (base, lmin, lmax) = l.int_bounds().expect("checked above");
                        let (osum, omin, omax) = r.int_bounds().expect("checked above");
                        let new_sum = base + osum;
                        let new_min = lmin.min(base + omin);
                        let new_max = lmax.max(base + omax);
                        if let (Ok(s), Ok(lo), Ok(hi)) = (
                            i64::try_from(new_sum),
                            i64::try_from(new_min),
                            i64::try_from(new_max),
                        ) {
                            *l = SumAcc::Int {
                                sum: s,
                                min: lo,
                                max: hi,
                            };
                        } else if let SumAcc::Int128(b) = l {
                            b.sum = new_sum;
                            b.min = new_min;
                            b.max = new_max;
                        } else if let SumAcc::Int128(mut b) = r {
                            b.sum = new_sum;
                            b.min = new_min;
                            b.max = new_max;
                            *l = SumAcc::Int128(b);
                        } else {
                            *l = SumAcc::Int128(Box::new(Int128Sum {
                                sum: new_sum,
                                min: new_min,
                                max: new_max,
                            }));
                        }
                    }
                    (l, r) if l.int_sum().is_some() && r.int_sum().is_some() => {
                        let sum = l.int_sum().expect("checked above")
                            + r.int_sum().expect("checked above");
                        *l = SumAcc::IntRetracted(sum);
                    }
                    (SumAcc::Decimal(l), r) if r.int_bounds().is_some() => {
                        let (osum, omin, omax) = r.int_bounds().expect("checked above");
                        let smin = super::value::add_scaled_int(l.val, omin)
                            .expect("validated by can_merge");
                        let smax = super::value::add_scaled_int(l.val, omax)
                            .expect("validated by can_merge");
                        let val = super::value::add_scaled_int(l.val, osum)
                            .expect("validated by can_merge");
                        l.val = val;
                        l.min = l.min.min(smin);
                        l.max = l.max.max(smax);
                    }
                    (l, SumAcc::Decimal(mut r)) if l.int_bounds().is_some() => {
                        let (base, lmin, lmax) = l.int_bounds().expect("checked above");
                        let trans_int = base + r.int_before;
                        let smin = super::value::add_scaled_int(r.min, base)
                            .expect("validated by can_merge");
                        let smax = super::value::add_scaled_int(r.max, base)
                            .expect("validated by can_merge");
                        let val = super::value::add_scaled_int(r.val, base)
                            .expect("validated by can_merge");
                        r.val = val;
                        r.min = smin;
                        r.max = smax;
                        r.int_before = trans_int;
                        debug_assert!(r.exact_int_prefix);
                        r.int_min = lmin.min(base + r.int_min);
                        r.int_max = lmax.max(base + r.int_max);
                        *l = SumAcc::Decimal(r);
                    }
                    (SumAcc::Decimal(l), SumAcc::Decimal(r)) => {
                        let imin = super::value::add_scaled_int(l.val, r.int_min)
                            .expect("validated by can_merge");
                        let imax = super::value::add_scaled_int(l.val, r.int_max)
                            .expect("validated by can_merge");
                        let dmin = l.val.checked_add(r.min).expect("validated by can_merge");
                        let dmax = l.val.checked_add(r.max).expect("validated by can_merge");
                        let val = l.val.checked_add(r.val).expect("validated by can_merge");
                        l.val = val;
                        l.min = l.min.min(imin).min(dmin);
                        l.max = l.max.max(imax).max(dmax);
                    }
                    // Single-row right partition with Float or Double on either side.
                    (left_acc, right_acc) => {
                        debug_assert_eq!(orows, 1);
                        use oxsdatatypes::{Double, Float};
                        match (&*left_acc, right_acc) {
                            (l, SumAcc::Float(f)) if l.int_sum().is_some() => {
                                let sum = l.int_sum().expect("checked above");
                                *left_acc = SumAcc::Float(Float::from(sum as f32) + f);
                            }
                            (l, SumAcc::Double(d)) if l.int_sum().is_some() => {
                                let sum = l.int_sum().expect("checked above");
                                *left_acc = SumAcc::Double(Double::from(sum as f64) + d);
                            }
                            (SumAcc::Decimal(ds), SumAcc::Float(f)) => {
                                *left_acc = SumAcc::Float(
                                    Float::from(super::value::decimal_from_raw(ds.val)) + f,
                                );
                            }
                            (SumAcc::Decimal(ds), SumAcc::Double(d)) => {
                                *left_acc = SumAcc::Double(
                                    Double::from(super::value::decimal_from_raw(ds.val)) + d,
                                );
                            }
                            (SumAcc::Float(f), r) if r.int_sum().is_some() => {
                                let sum = r.int_sum().expect("checked above");
                                *left_acc = SumAcc::Float(*f + Float::from(sum as f32));
                            }
                            (SumAcc::Float(f), SumAcc::Decimal(ds)) => {
                                *left_acc = SumAcc::Float(
                                    *f + Float::from(super::value::decimal_from_raw(ds.val)),
                                );
                            }
                            (SumAcc::Float(f), SumAcc::Float(g)) => {
                                *left_acc = SumAcc::Float(*f + g);
                            }
                            (SumAcc::Float(f), SumAcc::Double(d)) => {
                                *left_acc = SumAcc::Double(Double::from(*f) + d);
                            }
                            (SumAcc::Double(d), r) if r.int_sum().is_some() => {
                                let sum = r.int_sum().expect("checked above");
                                *left_acc = SumAcc::Double(*d + Double::from(sum as f64));
                            }
                            (SumAcc::Double(d), SumAcc::Decimal(ds)) => {
                                *left_acc = SumAcc::Double(
                                    *d + Double::from(super::value::decimal_from_raw(ds.val)),
                                );
                            }
                            (SumAcc::Double(d), SumAcc::Float(f)) => {
                                *left_acc = SumAcc::Double(*d + Double::from(f));
                            }
                            (SumAcc::Double(d), SumAcc::Double(x)) => {
                                *left_acc = SumAcc::Double(*d + x);
                            }
                            _ => unreachable!("validated by can_merge"),
                        }
                    }
                }
            }
            (
                AggState::Best(best, best_v, errs, flags),
                AggState::Best(Some(id), v, oerrs, oflags),
            ) => {
                *errs += oerrs;
                *flags |= oflags;
                let better = match best_v.as_ref() {
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
            (AggState::Best(_, _, errs, flags), AggState::Best(None, _, oerrs, oflags)) => {
                *errs += oerrs;
                *flags |= oflags;
            }
            (AggState::Sample(x), AggState::Sample(y)) => {
                if x.is_none() {
                    *x = y;
                }
            }
            _ => unreachable!("validated by can_merge"),
        }
        true
    }

    /// Add `weight` copies of one row; `id` is the aggregated column's value (`None` for COUNT(*)).
    pub(super) fn add_weighted(
        &mut self,
        ctx: &Ctx,
        agg: &Agg,
        id: Option<Id>,
        weight: u64,
        admit: &mut dyn SumAdmit,
    ) -> Result<()> {
        if weight == 0 {
            return Ok(());
        }
        match self {
            AggState::Count(n) => {
                if id.is_none_or(|id| !id.is_undef()) {
                    *n += weight;
                }
            }
            AggState::Best(_, _, errs, _) => {
                if id.is_none_or(|id| id.is_undef()) {
                    *errs += weight;
                } else {
                    let _ = self.add_admitted(ctx, agg, id, admit)?;
                }
            }
            AggState::Sample(_) => {
                let _ = self.add_admitted(ctx, agg, id, admit)?;
            }
            AggState::Sum {
                acc: SumAcc::HardError | SumAcc::Overflow,
                rows,
                errs,
                ..
            } => {
                *rows += weight;
                if id.is_none_or(|id| id.is_undef()) {
                    *errs += weight;
                }
            }
            AggState::Sum {
                acc, rows, errs, ..
            } if id.is_none_or(|id| id.is_undef()) => {
                *rows += weight;
                *errs += weight;
                *acc = SumAcc::HardError;
            }
            AggState::Sum {
                acc: SumAcc::Int { sum, min, max },
                rows,
                ..
            } if id.is_some_and(|id| id.tag() == crate::id::Tag::Int) => {
                let id = id.expect("checked above");
                let x = id.as_i64();
                if let Ok(w) = i64::try_from(weight)
                    && let Some(delta) = x.checked_mul(w)
                    && let Some(s) = sum.checked_add(delta)
                {
                    *sum = s;
                    if x >= 0 {
                        if s > *max {
                            *max = s;
                        }
                    } else if s < *min {
                        *min = s;
                    }
                    *rows += weight;
                    return Ok(());
                }
                for i in 0..weight {
                    if i.is_multiple_of(1024) {
                        ctx.check()?;
                    }
                    let _ = self.add_admitted(ctx, agg, Some(id), admit)?;
                    if let AggState::Sum {
                        acc: SumAcc::HardError | SumAcc::Overflow,
                        rows,
                        ..
                    } = self
                    {
                        *rows += weight - 1 - i;
                        break;
                    }
                }
            }
            _ => {
                for i in 0..weight {
                    if i.is_multiple_of(1024) {
                        ctx.check()?;
                    }
                    let _ = self.add_admitted(ctx, agg, id, admit)?;
                    if let AggState::Sum {
                        acc: SumAcc::HardError | SumAcc::Overflow,
                        rows,
                        errs,
                        ..
                    } = self
                    {
                        let rem = weight - 1 - i;
                        *rows += rem;
                        if id.is_none_or(|id| id.is_undef()) {
                            *errs += rem;
                        }
                        break;
                    }
                }
            }
        }
        Ok(())
    }

    /// Take back `weight` copies of one row that [`AggState::add`] counted, for a state
    /// that can: a count, and an exact sum of inline integers. `false`, with this state
    /// unchanged, for any other, since a minimum, a sample or a sum that left the exact
    /// integers keeps no record of the rows it would have to give up.
    pub(super) fn retract(&mut self, id: Option<Id>, weight: u64) -> bool {
        if weight == 0 {
            return true;
        }
        match self {
            AggState::Count(n) => {
                if id.is_none_or(|id| !id.is_undef()) {
                    *n = n.saturating_sub(weight);
                }
                true
            }
            AggState::Sum { acc, rows, .. } => {
                let Some(sum) = acc.int_sum() else {
                    return false;
                };
                let Some(id) = id.filter(|id| id.tag() == crate::id::Tag::Int) else {
                    return false;
                };
                let Some(delta) = i128::from(id.as_i64()).checked_mul(i128::from(weight)) else {
                    return false;
                };
                let Some(s) = sum.checked_sub(delta) else {
                    return false;
                };
                *acc = SumAcc::IntRetracted(s);
                *rows = rows.saturating_sub(weight);
                true
            }
            _ => false,
        }
    }

    /// Read the aggregate's current value without consuming this state.
    pub(super) fn read(&self, ctx: &Ctx, agg: &Agg) -> Result<Id> {
        match self {
            AggState::Count(n) => Ok(Id::from_i64(*n as i64).unwrap_or(Id::UNDEF)),
            AggState::Sum { .. } => self.clone().finish(ctx, agg),
            AggState::Best(best, _, errs, _) => Ok(super::group::acc::finish_best(
                ctx,
                matches!(agg.func, AggregateFunction::Min),
                *errs,
                *best,
            )),
            AggState::Sample(x) => Ok(x.unwrap_or(Id::UNDEF)),
            AggState::Stat(arq, acc) => acc.try_finish(ctx, *arq),
        }
    }

    pub(super) fn finish(self, ctx: &Ctx, agg: &Agg) -> Result<Id> {
        self.finish_with(ctx, agg, &mut DirectScratch)
    }

    pub(super) fn finish_with(
        self,
        ctx: &Ctx,
        agg: &Agg,
        scratch_admit: &mut dyn ScratchAdmit,
    ) -> Result<Id> {
        match self {
            AggState::Count(n) => Ok(Id::from_i64(n as i64).unwrap_or(Id::UNDEF)),
            AggState::Sum {
                acc, rows, errs, ..
            } => {
                // AVG is Sum over Count, and Count leaves the errors out
                if rows > 0
                    && errs == rows
                    && matches!(agg.func, AggregateFunction::Avg)
                    && ctx.aggregate_errors.avg_of_errors_zero()
                {
                    return ctx.try_intern_value(&Value::Integer(0.into()));
                }
                if matches!(acc, SumAcc::HardError | SumAcc::Overflow) {
                    return Ok(Id::UNDEF);
                }
                let is_avg = matches!(agg.func, AggregateFunction::Avg);
                if is_avg && rows == 0 {
                    return ctx.try_intern_value(&Value::Integer(0.into()));
                }
                let denom_value = || match i64::try_from(rows) {
                    Ok(r) => Value::Integer(r.into()),
                    Err(_) => Value::from_bigint(super::bignum::BigInt::from_u64(rows)),
                };
                let (v, scratch) = match acc {
                    SumAcc::Int { .. } | SumAcc::Int128(_) | SumAcc::IntRetracted(_) => {
                        let int = acc.int_sum().expect("int variant");
                        let needs_big = is_avg
                            && match (i64::try_from(int), i64::try_from(rows)) {
                                (Ok(i), Ok(r)) => super::value::int_div_needs_big(i, r),
                                _ => true,
                            };
                        let scratch = if needs_big {
                            Some(
                                scratch_admit
                                    .charge_scratch(ctx, super::bignum::AGG_SUM_SCRATCH_BYTES)?,
                            )
                        } else {
                            None
                        };
                        let sum = super::value::exact_sum(int);
                        let v = if is_avg {
                            let denom = denom_value();
                            arith(NumOp::Div, &sum, &denom).ok()
                        } else {
                            Some(sum)
                        };
                        (v, scratch)
                    }
                    SumAcc::Decimal(ds) => {
                        let d = super::value::decimal_from_raw(ds.val);
                        let needs_big = is_avg
                            && match i64::try_from(rows).ok().map(oxsdatatypes::Decimal::from) {
                                Some(rd) => {
                                    super::value::decimal_div_needs_big(d, rd)
                                        || d.checked_div(rd).is_none()
                                }
                                None => true,
                            };
                        let scratch = if needs_big {
                            Some(
                                scratch_admit
                                    .charge_scratch(ctx, super::bignum::AGG_SUM_SCRATCH_BYTES)?,
                            )
                        } else {
                            None
                        };
                        let sum = Value::Decimal(d);
                        let v = if is_avg {
                            let denom = denom_value();
                            arith(NumOp::Div, &sum, &denom).ok()
                        } else {
                            Some(sum)
                        };
                        (v, scratch)
                    }
                    SumAcc::Float(f) => {
                        let v = if is_avg {
                            Some(Value::Float(f / oxsdatatypes::Float::from(rows as f32)))
                        } else {
                            Some(Value::Float(f))
                        };
                        (v, None)
                    }
                    SumAcc::Double(d) => {
                        let v = if is_avg {
                            Some(Value::Double(d / oxsdatatypes::Double::from(rows as f64)))
                        } else {
                            Some(Value::Double(d))
                        };
                        (v, None)
                    }
                    SumAcc::BigInt(bi) => {
                        if is_avg {
                            let scratch = scratch_admit
                                .charge_scratch(ctx, super::bignum::AGG_SUM_SCRATCH_BYTES)?;
                            let v = super::value::avg_big_int(*bi, rows);
                            (v, Some(scratch))
                        } else {
                            (bi.fits_limits().then(|| Value::from_bigint(*bi)), None)
                        }
                    }
                    SumAcc::BigDec(bd) => {
                        if is_avg {
                            let scratch = scratch_admit
                                .charge_scratch(ctx, super::bignum::AGG_SUM_SCRATCH_BYTES)?;
                            let v = super::value::avg_big_decimal(&bd, rows);
                            drop(bd);
                            (v, Some(scratch))
                        } else {
                            (bd.fits_limits().then(|| Value::from_bigdec(*bd)), None)
                        }
                    }
                    SumAcc::HardError | SumAcc::Overflow => unreachable!(),
                };
                let intern_scratch = if scratch.is_none() {
                    match v.as_ref() {
                        Some(Value::BigInteger(i)) => Some(scratch_admit.charge_scratch(
                            ctx,
                            super::bignum::intern_scratch_bytes(i.lexical_len()),
                        )?),
                        Some(Value::BigDecimal(d)) => Some(scratch_admit.charge_scratch(
                            ctx,
                            super::bignum::intern_scratch_bytes(d.lexical_len()),
                        )?),
                        _ => None,
                    }
                } else {
                    None
                };
                let id = match v.as_ref() {
                    Some(v) => ctx.try_intern_value(v)?,
                    None => Id::UNDEF,
                };
                drop(v);
                drop(intern_scratch);
                drop(scratch);
                Ok(id)
            }
            AggState::Best(best, _, errs, _) => Ok(super::group::acc::finish_best(
                ctx,
                matches!(agg.func, AggregateFunction::Min),
                errs,
                best,
            )),
            AggState::Sample(x) => Ok(x.unwrap_or(Id::UNDEF)),
            AggState::Stat(arq, acc) => acc.try_finish(ctx, arq),
        }
    }
}

/// One aggregate of one group, over the group's rows; `arg` holds the argument's value
/// in every row of `t` when it was evaluated beforehand (unbound in it: an error).
pub(super) fn aggregate(
    ctx: &Ctx,
    t: &Table,
    map: &[Option<usize>],
    rows: &[u32],
    agg: &Agg,
    arg: Option<&[Id]>,
    exists: Option<&super::exists::Answers<'_>>,
) -> Result<Id> {
    if let (Some(spec), Some(e)) = (&agg.fold, &agg.expr) {
        return fold(ctx, t, map, rows, (e, spec, agg.distinct), exists);
    }
    let Some(e) = &agg.expr else {
        // COUNT(*)
        let n = if agg.distinct {
            let _seen_charge = ctx.charge(
                (rows.len() as u64)
                    .saturating_mul((t.cols.len() as u64).saturating_mul(8).saturating_add(64))
                    .saturating_add(64),
            )?;
            let mut seen = FxHashSet::default();
            for (idx, &i) in rows.iter().enumerate() {
                if idx.is_multiple_of(1024) {
                    ctx.check()?;
                }
                seen.insert(t.row(i as usize));
            }
            seen.len()
        } else {
            rows.len()
        };
        return Ok(Id::from_i64(n as i64).unwrap_or(Id::UNDEF));
    };
    let _vals_charge = ctx.charge(
        (rows.len() as u64)
            .saturating_mul(std::mem::size_of::<std::result::Result<Id, ()>>() as u64)
            .saturating_add(64),
    )?;
    let mut vals: Vec<std::result::Result<Id, ()>> = Vec::with_capacity(rows.len());
    for (idx, &i) in rows.iter().enumerate() {
        if idx.is_multiple_of(1024) {
            ctx.check()?;
        }
        let item = if let Some(a) = arg {
            let id = a[i as usize];
            if id.is_undef() { Err(()) } else { Ok(id) }
        } else {
            match eval(
                e,
                &Row {
                    table: t,
                    i: i as usize,
                    map,
                    dec: None,
                    exists,
                },
                ctx,
            ) {
                Ok(v) => {
                    let id = v.into_id(ctx);
                    ctx.check()?;
                    Ok(id)
                }
                Err(_) => {
                    if ctx.has_memory_failure() {
                        ctx.check()?;
                    }
                    Err(())
                }
            }
        };
        vals.push(item);
    }
    ctx.check()?;
    if agg.distinct {
        let _seen_charge = ctx.charge((vals.len() as u64).saturating_mul(32).saturating_add(64))?;
        let mut seen = FxHashSet::default();
        vals.retain(|v| match v {
            Ok(id) => seen.insert(*id),
            Err(_) => true,
        });
    }
    match &agg.func {
        AggregateFunction::Count => {
            Ok(Id::from_i64(vals.iter().filter(|v| v.is_ok()).count() as i64).unwrap_or(Id::UNDEF))
        }
        // The running sum finds an integer sum in every order of the rows.
        AggregateFunction::Sum | AggregateFunction::Avg => {
            let mut state = AggState::new(agg);
            for (idx, v) in vals.iter().enumerate() {
                if idx.is_multiple_of(1024) {
                    ctx.check()?;
                }
                state.add(ctx, agg, Some(v.unwrap_or(Id::UNDEF)), &mut |_| Ok(()))?;
            }
            let id = state.finish(ctx, agg)?;
            ctx.check()?;
            Ok(id)
        }
        AggregateFunction::Min | AggregateFunction::Max => {
            let is_min = matches!(agg.func, AggregateFunction::Min);
            if vals.iter().any(|v| v.is_err()) && ctx.aggregate_errors.best_errs(is_min) {
                return Ok(Id::UNDEF);
            }
            let mut best_credit = ctx.charge(0)?;
            let mut best: Option<Id> = None;
            let mut best_v: Option<Value> = None;
            for (idx, id) in vals.iter().filter_map(|v| v.ok()).enumerate() {
                if idx.is_multiple_of(1024) {
                    ctx.check()?;
                }
                let decode = ctx.charge(0)?;
                let v = ctx.value_checked_into(id, &decode)?;
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
                    // Drop the old decoded value while its old credit is still live,
                    // then swap in the new credit before storing the new value.
                    drop(best_v.take());
                    best_credit = decode;
                    best_v = v;
                } else {
                    drop(v);
                    drop(decode);
                }
            }
            drop(best_v);
            drop(best_credit);
            Ok(best.unwrap_or(Id::UNDEF))
        }
        AggregateFunction::Sample => Ok(vals.iter().find_map(|v| v.ok()).unwrap_or(Id::UNDEF)),
        AggregateFunction::GroupConcat { separator } => {
            if agg.distinct
                && rows.is_empty()
                && ctx.aggregate_errors.distinct_concat_of_nothing_unbound()
            {
                return Ok(Id::UNDEF);
            }
            if vals.iter().any(|v| v.is_err()) {
                return Ok(Id::UNDEF);
            }
            let sep = separator.as_deref().unwrap_or(" ");
            let strings = ctx.aggregate_errors.concat_strings_only();
            let out_charge = ctx.charge(0)?;
            let mut out = String::new();
            let mut first = true;
            for (idx, id) in vals.iter().filter_map(|v| v.ok()).enumerate() {
                if idx.is_multiple_of(1024) {
                    ctx.check()?;
                }
                let decode = ctx.charge(0)?;
                let Some(v) = ctx.value_checked_into(id, &decode)? else {
                    continue;
                };
                // CONCAT takes only strings
                if strings && !v.is_string() {
                    return Ok(Id::UNDEF);
                }
                let Ok(s) = v.lexical() else {
                    return Ok(Id::UNDEF);
                };
                let add_len = s.len().saturating_add(if first { 0 } else { sep.len() });
                if out.len().saturating_add(add_len) > out.capacity() {
                    let before = out.capacity();
                    let needed = out.len().saturating_add(add_len);
                    let next_cap = needed.max(before.saturating_mul(2));
                    let delta = (next_cap.saturating_sub(before) as u64).saturating_mul(2);
                    if out_charge.add(delta).is_err() {
                        let exact = (needed.saturating_sub(before) as u64).saturating_mul(2);
                        out_charge.add(exact)?;
                        out.reserve_exact(add_len);
                    } else {
                        out.reserve(next_cap - out.len());
                    }
                }
                if !first {
                    out.push_str(sep);
                }
                out.push_str(&s);
                first = false;
                drop(s);
                drop(v);
                drop(decode);
            }
            // SPARQL 1.1: the result is a simple literal (language tags are dropped)
            let id = ctx.intern_value(&Value::Str(out.into()));
            drop(out_charge);
            ctx.check()?;
            Ok(id)
        }
        AggregateFunction::Custom(iri) => super::aggext::aggregate(ctx, iri.as_str(), &vals),
    }
}

/// ARQ's `FOLD` of one group (`AggFoldList`, `AggFoldMap`): the values of `e` in the
/// group's rows, sorted by the fold's ORDER BY first, as a `cdt:List` literal (an error
/// is a null, and DISTINCT keeps the first of equal terms and one null), or the `e`
/// keys with the `value` values as a `cdt:Map` literal (a row whose key is an error or a
/// blank node is skipped, a later row replaces an earlier one's value).
/// The fold is given as its argument, its spec and its DISTINCT flag.
fn fold(
    ctx: &Ctx,
    t: &Table,
    map: &[Option<usize>],
    rows: &[u32],
    (e, fold, distinct): (&Expr, &super::plan::Fold, bool),
    exists: Option<&super::exists::Answers<'_>>,
) -> Result<Id> {
    use super::cdt::{Elem, Map, map_put};
    let row = |i: u32| Row {
        table: t,
        i: i as usize,
        map,
        dec: None,
        exists,
    };
    let fold_charge = ctx.charge((rows.len() as u64).saturating_mul(4).saturating_add(64))?;
    let mut order: Vec<u32> = rows.to_vec();
    if !fold.order.is_empty() {
        let key_slots = (rows.len() as u64)
            .saturating_mul(fold.order.len() as u64)
            .saturating_mul(std::mem::size_of::<Option<Value>>() as u64)
            .saturating_add((rows.len() as u64).saturating_mul(8))
            .saturating_add(64);
        fold_charge.add(key_slots)?;
        let mut keys: Vec<Vec<Option<Value>>> = Vec::with_capacity(rows.len());
        for (r_idx, &i) in rows.iter().enumerate() {
            if r_idx.is_multiple_of(1024) {
                ctx.check()?;
            }
            let mut row_keys = Vec::with_capacity(fold.order.len());
            for (k, _) in &fold.order {
                let val = match eval(k, &row(i), ctx) {
                    Ok(Val::Id(id)) => ctx.value_checked_into(id, &fold_charge)?,
                    Ok(Val::V(v) | Val::Dec(_, v)) => {
                        let opt = Some(v);
                        fold_charge.add(key_payload(&opt))?;
                        opt
                    }
                    Err(_) => {
                        if ctx.has_memory_failure() {
                            ctx.check()?;
                        }
                        None
                    }
                };
                row_keys.push(val);
            }
            keys.push(row_keys);
        }
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
        drop(keys);
        order = idx.into_iter().map(|j| rows[j]).collect();
    }
    let eval_term = |x: &Expr, i: u32| -> Result<Option<oxrdf::Term>> {
        match eval(x, &row(i), ctx) {
            Ok(v) => {
                let id = v.into_id(ctx);
                ctx.check()?;
                ctx.term_checked_into(id, &fold_charge)
            }
            Err(_) => {
                if ctx.has_memory_failure() {
                    ctx.check()?;
                }
                Ok(None)
            }
        }
    };
    let value = match &fold.value {
        None => {
            fold_charge.add(
                (order.len() as u64)
                    .saturating_mul(std::mem::size_of::<Elem>() as u64)
                    .saturating_add(64),
            )?;
            let mut list = Vec::with_capacity(order.len());
            let mut seen = FxHashSet::default();
            let mut null = false;
            for (idx, i) in order.into_iter().enumerate() {
                if idx.is_multiple_of(1024) {
                    ctx.check()?;
                }
                match eval(e, &row(i), ctx) {
                    Ok(v) => {
                        let id = v.into_id(ctx);
                        ctx.check()?;
                        if distinct {
                            if seen.contains(&id) {
                                continue;
                            }
                            fold_charge.add(32)?;
                            seen.insert(id);
                        }
                        let term = ctx.term_checked_into(id, &fold_charge)?;
                        list.push(term.map_or(Elem::Null, Elem::Term));
                    }
                    Err(_) => {
                        if ctx.has_memory_failure() {
                            ctx.check()?;
                        }
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
            for (idx, i) in order.into_iter().enumerate() {
                if idx.is_multiple_of(1024) {
                    ctx.check()?;
                }
                let Some(k @ (oxrdf::Term::NamedNode(_) | oxrdf::Term::Literal(_))) =
                    eval_term(e, i)?
                else {
                    continue;
                };
                let val_elem = eval_term(v, i)?.map_or(Elem::Null, Elem::Term);
                fold_charge.add(64)?;
                map_put(&mut m, k, val_elem);
            }
            super::cdt::map_value(&m)
        }
    };
    let opt = Some(value);
    fold_charge.add(key_payload(&opt))?;
    let id = ctx.intern_value(opt.as_ref().expect("folded value"));
    drop(opt);
    drop(fold_charge);
    ctx.check()?;
    Ok(id)
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

/// The growth of a neighbour buffer while lookups or a sweep fill it: the capacity
/// added since it started is charged every few thousand nodes, and the deadline is
/// checked every few thousand nodes added or rows scanned, since one lookup may read
/// millions of rows, kept or not. The charge ends with the expansion, once the closure
/// charges what it keeps.
struct Growth<'a> {
    ctx: &'a Ctx,
    held: super::ctx::Charge<'a>,
    charged: usize,
    scanned: usize,
}

impl<'a> Growth<'a> {
    /// A charge for the growth of `out` beyond its present capacity, which its owner
    /// has charged already.
    #[allow(clippy::ptr_arg)]
    fn new(ctx: &'a Ctx, out: &Vec<u64>) -> Result<Self> {
        Ok(Self {
            ctx,
            held: ctx.charge(0)?,
            charged: out.capacity(),
            scanned: 0,
        })
    }

    // the capacity, which a slice does not have
    #[allow(clippy::ptr_arg)]
    fn track(&mut self, out: &Vec<u64>) -> Result<()> {
        if out.capacity() >= self.charged.saturating_add(4096) {
            self.held.add((out.capacity() - self.charged) as u64 * 8)?;
            self.charged = out.capacity();
            self.ctx.check()?;
        }
        Ok(())
    }

    /// Count `rows` index rows read, and check the deadline every 65,536.
    fn scan(&mut self, rows: usize) -> Result<()> {
        let before = self.scanned;
        self.scanned = self.scanned.saturating_add(rows);
        if before / 65_536 != self.scanned / 65_536 {
            self.ctx.check()?;
        }
        Ok(())
    }
}

/// The bytes a path walk adds to what it holds before it charges them, at most a few
/// hundred reached nodes.
const WALK_CHARGE_BYTES: u64 = 16 << 10;

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
        // one charge for the buffer's growth over the whole expansion
        let mut grown = Growth::new(self.ctx, out)?;
        if let Some((p, rev)) = self.spec.simple {
            return self.expand_pred(p, rev, frontier, forward, out, &mut grown);
        }
        if let Some(preds) = &self.spec.preds {
            // each predicate of an alternative as a simple path would expand it, so a
            // wide frontier takes one sweep per predicate rather than a lookup per node
            for &(p, rev) in preds {
                self.expand_pred(p, rev, frontier, forward, out, &mut grown)?;
            }
            return Ok(());
        }
        self.expand_each(frontier, forward, out, &mut grown)
    }

    /// [`Graph::expand`] over predicate `p` (`rev`: its inverse) alone.
    fn expand_pred(
        &self,
        p: u64,
        rev: bool,
        frontier: &[u64],
        forward: bool,
        out: &mut Vec<u64>,
        grown: &mut Growth<'_>,
    ) -> Result<()> {
        let perm = if forward != rev { Perm::Pso } else { Perm::Pos };
        let rows = self.ctx.snap.estimate(perm, &[p]);
        if !self.ctx.opt.batched_paths
            || frontier.len() < 64
            || (frontier.len() as u64).saturating_mul(SWEEP_RATIO) < rows
        {
            for (i, &x) in frontier.iter().enumerate() {
                self.seek(p, rev, x, forward, out, grown)?;
                if i % 1024 == 1023 {
                    self.ctx.check()?;
                }
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
            (1 << 1) | (1 << 2) | (1 << gc),
            |c| {
                match c {
                    Chunk::Block(b, s, e) => {
                        grown.scan(e - s)?;
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
                        grown.scan(1)?;
                        j += frontier[j..].partition_point(|&x| x < k[1]);
                        if j < frontier.len() && frontier[j] == k[1] && self.graph.accepts(k[gc]) {
                            out.push(k[2]);
                        }
                    }
                }
                grown.track(out)?;
                Ok(j < frontier.len())
            },
        )?;
        self.ctx.check()?;
        Ok(())
    }

    /// [`Graph::expand`] by one lookup per frontier node, checking the deadline and
    /// the neighbour buffer's memory between lookups, since one level of a wide
    /// closure may hold millions of nodes.
    fn expand_each(
        &self,
        frontier: &[u64],
        forward: bool,
        out: &mut Vec<u64>,
        grown: &mut Growth<'_>,
    ) -> Result<()> {
        for (i, &x) in frontier.iter().enumerate() {
            self.neighbours_into(x, forward, out, grown)?;
            if i % 1024 == 1023 {
                self.ctx.check()?;
            }
        }
        Ok(())
    }

    fn neighbours(&self, x: u64, forward: bool) -> Result<Vec<u64>> {
        let mut out = Vec::new();
        let mut grown = Growth::new(self.ctx, &out)?;
        self.neighbours_into(x, forward, &mut out, &mut grown)?;
        Ok(out)
    }

    /// Append the neighbours of `x` to `out`, whose growth `grown` charges.
    fn neighbours_into(
        &self,
        x: u64,
        forward: bool,
        out: &mut Vec<u64>,
        grown: &mut Growth<'_>,
    ) -> Result<()> {
        if let Some((p, rev)) = self.spec.simple {
            return self.seek(p, rev, x, forward, out, grown);
        }
        if let Some(preds) = &self.spec.preds {
            // one lookup per predicate of the alternative, with a neighbour once per
            // predicate that links to it, as the union of the edge plan has it
            for &(p, rev) in preds {
                self.seek(p, rev, x, forward, out, grown)?;
            }
            return Ok(());
        }
        let m = if forward { self.fwd } else { self.bwd };
        if let Some(ys) = m.and_then(|m| m.get(&x)) {
            out.extend_from_slice(ys);
            grown.track(out)?;
        }
        Ok(())
    }

    /// Append the neighbours of `x` over predicate `p` (`rev`: its inverse) to `out`,
    /// read by one index lookup. A hub's neighbours are charged, and the deadline
    /// checked, as they are read, kept or not, since one lookup may read millions of
    /// rows.
    fn seek(
        &self,
        p: u64,
        rev: bool,
        x: u64,
        forward: bool,
        out: &mut Vec<u64>,
        grown: &mut Growth<'_>,
    ) -> Result<()> {
        let dir = forward != rev;
        // forward: (x p ?o) via PSO; backward: (?s p x) via POS
        let perm = if dir { Perm::Pso } else { Perm::Pos };
        let gc = perm.col_of(crate::index::G);
        let mask = (1 << 2) | (1 << gc);
        let (lo, hi) = (pad(&[p, x], 0), pad(&[p, x], u64::MAX));
        let from = out.len();
        self.ctx.snap.scan_between_cols(perm, lo, hi, mask, |c| {
            match c {
                Chunk::Block(b, s, e) => {
                    grown.scan(e - s)?;
                    for i in s..e {
                        if self.graph.accepts(b.cols[gc][i]) {
                            out.push(b.cols[2][i]);
                        }
                    }
                }
                Chunk::Row(k) => {
                    grown.scan(1)?;
                    if self.graph.accepts(k[gc]) {
                        out.push(k[2]);
                    }
                }
            }
            grown.track(out)?;
            Ok(true)
        })?;
        // the same neighbour in several graphs of the active graph, once
        let mut w = from;
        for r in from..out.len() {
            if w == from || out[r] != out[w - 1] {
                out[w] = out[r];
                w += 1;
            }
        }
        out.truncate(w);
        Ok(())
    }

    /// Nodes reachable from `start` according to min/max length (ARQ ranges: once per
    /// way).
    fn reach(&self, start: u64, forward: bool) -> Result<Vec<u64>> {
        if let Some((lo, hi)) = self.spec.count {
            return self.reach_counted(start, forward, lo, hi);
        }
        let mut out = Vec::new();
        let mut seen = FxHashSet::default();
        // The visited set, the reached nodes and the frontier are charged as they grow,
        // in eager execution as in a cursor, so that a wide closure fails at the memory
        // budget rather than growing without bound.
        // The charge takes what the walk adds once it comes to [`WALK_CHARGE_BYTES`],
        // as the neighbour buffer's growth is charged, and the output's bytes are
        // checked again each time it doubles, so that a long narrow walk takes the
        // query's memory ledger, when it has one, once per few hundred nodes rather
        // than several times per level. Neither depends on the budget, so a run within
        // a budget of its own peak charges as one without a budget does.
        let held = self.ctx.charge(64)?;
        let mut pending = 0u64;
        let mut checked_rows = 0usize;
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
            // the set entry, the reached node and the frontier node of each, and the
            // growth of the neighbour buffer
            let grown = found.capacity().saturating_sub(found_charged);
            found_charged = found_charged.max(found.capacity());
            pending = pending.saturating_add(next.len() as u64 * 48 + grown as u64 * 8);
            if pending >= WALK_CHARGE_BYTES {
                held.add(std::mem::take(&mut pending))?;
            }
            if self.spec.max_one && depth >= 1 {
                break;
            }
            frontier = next;
            self.ctx.check_output_rows(out.len())?;
            if out.len() >= checked_rows.saturating_mul(2).max(1024) {
                self.ctx.check_output_bytes(out.len(), 1)?;
                checked_rows = out.len();
            }
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

    /// Call `f` with every start node of a path with two unbound ends, in order: the
    /// sources of the edge relation of a path over a sequence or alternative without a
    /// zero-length match, otherwise the nodes that [`StartNodes`] reads chunk by chunk,
    /// so that the start nodes are never all held at once.
    fn each_start(&self, f: &mut dyn FnMut(u64) -> Result<()>) -> Result<()> {
        if self.spec.min > 0 && self.spec.simple.is_none() {
            let held = self.ctx.charge(0)?;
            let mut starts: Vec<u64> = self
                .fwd
                .map(|m| m.keys().copied().collect())
                .unwrap_or_default();
            held.add(starts.capacity() as u64 * 8)?;
            starts.sort_unstable();
            for x in starts {
                f(x)?;
            }
            return Ok(());
        }
        let mut starts = StartNodes::new(self.spec);
        while let Some(chunk) = starts.next(self.ctx, &self.graph)? {
            for x in chunk {
                f(x)?;
            }
        }
        Ok(())
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
    // The edge relation of a path over a sequence or alternative, charged while the
    // path is evaluated, as a cursor reads it.
    let held = ctx.charge(0)?;
    let edges = match (edges, spec.edge_vars) {
        (Some(e), Some(_)) => {
            let mut rel = PathEdges::default();
            held.add(rel.bytes_with(e.len()))?;
            rel.add(ctx, spec, &e)?;
            drop(e);
            Some(rel)
        }
        _ => None,
    };
    for (gf, gid) in graphs {
        let (fwd, bwd) = match &edges {
            Some(rel) => {
                let (f, b) = rel.maps(gid);
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
            let Some(l) = left else {
                return g.each_start(&mut |x| {
                    for y in g.reach(x, true)? {
                        push(x, y, out);
                    }
                    ctx.check_output(out.len(), out.width())
                });
            };
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
                // variable–variable paths range over graph nodes only: a zero-length
                // match needs the start term to occur in the graph
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
    pub(super) fn add(&mut self, ctx: &Ctx, spec: &PathSpec, e: &Table) -> Result<()> {
        let Some((a, b)) = spec.edge_vars else {
            return Err(Error::invalid("a path without an edge plan"));
        };
        let (Some(ac), Some(bc)) = (e.col_of(a), e.col_of(b)) else {
            return Err(Error::invalid("a path's edge plan does not bind its ends"));
        };
        let gc = spec.graph_var.and_then(|g| e.col_of(g));
        for i in 0..e.len() {
            if i % 65_536 == 65_535 {
                ctx.check()?;
            }
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
/// Between calls it keeps a chunk of the start nodes of the graph it is walking.
pub(super) struct PathWalk {
    spec: PathSpec,
    vars: Vec<VarId>,
    graphs: Vec<(GraphFilter, Option<Id>)>,
    graph: usize,
    starts: Option<Vec<u64>>,
    start: usize,
    /// Where the start nodes of the graph being walked continue, and whether no start
    /// node follows the ones held.
    cursor: Option<StartNodes>,
    last_chunk: bool,
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
            cursor: None,
            last_chunk: false,
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
            + self.cursor.as_ref().map_or(0, StartNodes::retained)
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
                        // As [`Graph::each_start`] finds them: the sources of the edge
                        // relation when the path has no zero-length match, otherwise
                        // the nodes of the next chunk.
                        let starts = match g.fwd {
                            Some(f) if spec.min > 0 => {
                                let mut starts: Vec<u64> = f.keys().copied().collect();
                                starts.sort_unstable();
                                self.last_chunk = true;
                                starts
                            }
                            _ => {
                                let cursor =
                                    self.cursor.get_or_insert_with(|| StartNodes::new(spec));
                                match cursor.next(ctx, &g.graph)? {
                                    Some(chunk) => chunk,
                                    None => {
                                        self.last_chunk = true;
                                        Vec::new()
                                    }
                                }
                            }
                        };
                        self.starts = Some(starts);
                        self.start = 0;
                    }
                    let starts = self.starts.as_ref().expect("start nodes");
                    let Some(&x) = starts.get(self.start) else {
                        self.starts = None;
                        if self.last_chunk {
                            self.cursor = None;
                            self.last_chunk = false;
                            self.graph += 1;
                        }
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

/// The distinct start nodes that [`StartNodes`] reads at a time from each index.
const START_CHUNK: usize = 4096;

#[cfg(test)]
thread_local! {
    /// Index rows that start-node reads have visited on this thread.
    static START_ROWS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// The start nodes of a path with two variable ends, in order, a chunk at a time: the
/// subjects of a simple path's predicate when the path has no zero-length match,
/// otherwise every subject and object of the active graph. Each index is read on from
/// where its last chunk stopped, so every index row is read once, and at most about
/// [`START_CHUNK`] nodes of each index are held at a time rather than a set of all of
/// them.
pub(super) struct StartNodes {
    sides: Vec<StartSide>,
}

/// The distinct nodes of one index column, read a chunk at a time.
struct StartSide {
    perm: Perm,
    prefix: Option<u64>,
    held: Vec<u64>,
    at: usize,
    from: u64,
    done: bool,
}

impl StartSide {
    fn new(perm: Perm, prefix: Option<u64>) -> Self {
        Self {
            perm,
            prefix,
            held: Vec::new(),
            at: 0,
            from: 0,
            done: false,
        }
    }

    fn pending(&self) -> &[u64] {
        &self.held[self.at..]
    }

    /// Read the next chunk once the held nodes are used up.
    fn fill(&mut self, ctx: &Ctx, graph: &GraphFilter) -> Result<()> {
        if self.at < self.held.len() || self.done {
            return Ok(());
        }
        let (held, next) = distinct_from(ctx, self.perm, self.prefix, self.from, graph)?;
        self.held = held;
        self.at = 0;
        match next {
            Some(n) => self.from = n,
            None => self.done = true,
        }
        Ok(())
    }
}

impl StartNodes {
    pub(super) fn new(spec: &PathSpec) -> Self {
        let sides = if spec.min > 0 {
            match spec.simple {
                Some((p, rev)) => {
                    vec![StartSide::new(
                        if rev { Perm::Pos } else { Perm::Pso },
                        Some(p),
                    )]
                }
                None => Vec::new(),
            }
        } else {
            vec![
                StartSide::new(Perm::Spo, None),
                StartSide::new(Perm::Osp, None),
            ]
        };
        Self { sides }
    }

    /// The nodes held from each index.
    fn retained(&self) -> usize {
        self.sides.iter().map(|s| s.held.capacity()).sum()
    }

    /// The next start nodes in order, or `None` after the last.
    pub(super) fn next(&mut self, ctx: &Ctx, graph: &GraphFilter) -> Result<Option<Vec<u64>>> {
        for side in &mut self.sides {
            side.fill(ctx, graph)?;
        }
        // Up to the smallest last node of a side with more to read, every side holds
        // every node it has.
        let bound = self
            .sides
            .iter()
            .filter(|s| !s.done)
            .filter_map(|s| s.pending().last().copied())
            .min()
            .unwrap_or(u64::MAX);
        let mut out: Vec<u64> = Vec::new();
        for side in &mut self.sides {
            let take = side.pending().partition_point(|&x| x <= bound);
            out.extend_from_slice(&side.pending()[..take]);
            side.at += take;
        }
        if out.is_empty() {
            return Ok(None);
        }
        out.sort_unstable();
        out.dedup();
        Ok(Some(out))
    }
}

/// The distinct values from `from` on of the key column after `prefix` in `perm`,
/// in order, of the quads in `graph`: at most [`START_CHUNK`] of them, and the value
/// the next read starts from, `None` when the scan reached the end of the range. The rows of a value already taken are
/// skipped by a new seek past them once they run long, so a value with millions of
/// quads, such as a common class, costs a seek rather than a read of its rows. The
/// deadline is checked as rows are read.
fn distinct_from(
    ctx: &Ctx,
    perm: Perm,
    prefix: Option<u64>,
    mut from: u64,
    graph: &GraphFilter,
) -> Result<(Vec<u64>, Option<u64>)> {
    /// Rows of a value already taken that are read before seeking past it.
    const RUN: usize = 512;
    let col = usize::from(prefix.is_some());
    let gc = perm.col_of(crate::index::G);
    let mask = (1 << col) | (1 << gc);
    let mut out: Vec<u64> = Vec::with_capacity(START_CHUNK.min(1024));
    let mut rows = 0usize;
    loop {
        let (lo, hi) = match prefix {
            Some(p) => ([p, from, 0, 0], [p, u64::MAX, u64::MAX, u64::MAX]),
            None => ([from, 0, 0, 0], [u64::MAX; 4]),
        };
        // `Some(v)`: seek to v, past a long run; `cut`: the chunk is full at value v
        let mut resume: Option<u64> = None;
        let mut cut: Option<u64> = None;
        let mut run = 0usize;
        let mut take = |v: u64, g: u64, out: &mut Vec<u64>| -> Result<bool> {
            rows += 1;
            #[cfg(test)]
            START_ROWS.with(|n| n.set(n.get() + 1));
            if rows.is_multiple_of(65_536) {
                ctx.check()?;
            }
            if out.last() == Some(&v) {
                run += 1;
                if run >= RUN && v < u64::MAX {
                    resume = Some(v + 1);
                    return Ok(false);
                }
                return Ok(true);
            }
            run = 0;
            if graph.accepts(g) {
                if out.len() == START_CHUNK {
                    cut = Some(v);
                    return Ok(false);
                }
                out.push(v);
            }
            Ok(true)
        };
        ctx.snap.scan_between_cols(perm, lo, hi, mask, |c| {
            match c {
                Chunk::Block(b, s, e) => {
                    let vals = &b.cols[col][s..e];
                    let gs = &b.cols[gc][s..e];
                    let mut i = 0;
                    while i < vals.len() {
                        if !take(vals[i], gs[i], &mut out)? {
                            return Ok(false);
                        }
                        i += 1;
                    }
                }
                Chunk::Row(k) => return take(k[col], k[gc], &mut out),
            }
            Ok(true)
        })?;
        ctx.check()?;
        match (cut, resume) {
            (Some(v), _) => return Ok((out, Some(v))),
            (None, Some(next)) => from = next,
            (None, None) => return Ok((out, None)),
        }
    }
}

// ----------------------------------------------------------------- vectors ------

/// Exact top-k vector search (`spk:vectorSearch`).
#[cfg(feature = "geo")]
use crate::geo::exec::{spatial_pf, spatial_scan};
#[cfg(feature = "geo")]
use crate::geo::join::spatial_join;
#[cfg(feature = "geo")]
use crate::geo::knn::spatial_knn_materialized as spatial_knn;
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
fn spatial_knn<'ctx>(
    _: &'ctx Ctx,
    _: &super::geojoin::SpatialKnnSpec,
    _: &Node,
    _: &[VarId],
) -> Result<(Materialized<'ctx>, Counters, Vec<PlanInfo>)> {
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
pub(super) fn entity_vector(
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
            .with_controlled_key(*o, |k| Ok(crate::vector::from_key(k)))?
            .flatten()
            .map(Some)
            .ok_or_else(|| Error::invalid("the entity's vector is not a valid spk:vector")),
        many => Err(Error::invalid(format!(
            "entity {} has {} vectors for the predicate; pass a vector literal",
            ctx.plan_term(e).map_or("?".into(), |t| t.to_string()),
            many.len()
        ))),
    }
}

/// Distinct query vectors a variable query may bind.
pub(super) const MAX_QUERY_VECTORS: usize = 1000;

/// The IRI of a search's predicate.
fn pred_iri(ctx: &Ctx, pred: Id) -> Result<Option<String>> {
    let decode = ctx.charge(0)?;
    Ok(match ctx.term_checked_into(pred, &decode)? {
        Some(oxrdf::Term::NamedNode(n)) => Some(n.into_string()),
        _ => None,
    })
}

/// Whether the index of `pred` embeds query text.
fn text_queries(ctx: &Ctx, pred: Id) -> Result<bool> {
    Ok(pred_iri(ctx, pred)?.is_some_and(|iri| {
        ctx.snap
            .generation
            .vectors
            .configured()
            .values()
            .any(|c| c.predicate == iri && c.embedding.as_ref().is_some_and(|e| e.query_text))
    }))
}

/// The vector of a search's text, from the provider of the predicate's index.
fn text_vector(ctx: &Ctx, pred: Id, text: &str) -> Result<std::sync::Arc<[f32]>> {
    ctx.check()?;
    let iri = pred_iri(ctx, pred)?
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

/// An OPTIONAL's right side that is a triple pattern sorted on a key that every row of
/// the evaluated left side `l` binds, read only for the left side's distinct keys when
/// that costs less than reading the pattern (see [`super::indexjoin::probe_for`]),
/// with its plan entry. The filters of the pattern are applied to the rows read.
fn optional_probe(ctx: &Ctx, l: &Table, right: &Node) -> Result<Option<(Table, PlanInfo)>> {
    let start = Instant::now();
    let Some((t, stats)) = super::indexjoin::probe_for(ctx, right, l)? else {
        return Ok(None);
    };
    let mut info = describe(ctx, right);
    info.description = format!(
        "{} [probed for the left side's keys: {}]",
        right.desc,
        stats.note().trim_start_matches('[').trim_end_matches(']')
    );
    info.actual_rows = t.len() as i64;
    info.time_ms = start.elapsed().as_secs_f64() * 1000.0;
    info.counters = Some(stats.counters());
    info.children = std::mem::take(&mut info.children)
        .into_iter()
        .map(|c| c.skip("the OPTIONAL probed the pattern for its left side's keys"))
        .collect();
    Ok(Some((t, info)))
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
pub(super) const MAX_TEXT_QUERIES: usize = 1000;

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
        let decode = ctx.charge(0)?;
        let Some(oxrdf::Term::Literal(l)) = ctx.term_checked_into(id, &decode)? else {
            continue;
        };
        let lang = match l.language() {
            Some(t) => Some(t.to_ascii_lowercase()),
            None if l.datatype() == oxrdf::vocab::xsd::STRING => None,
            None => continue,
        };
        decode.add(l.value().len() as u64)?;
        let mut one = spec.clone();
        one.query = l.value().to_string();
        drop(l);
        one.query_var = None;
        one.lang = spec.lang.clone().or(lang);
        let subjects = subject_col.and_then(|c| {
            let rows = &by_value[&id];
            distinct_bound(input, c, &mut rows.iter().copied(), TEXT_PUSHDOWN_SUBJECTS)
        });
        restricted += subjects.is_some() as u64;
        let mut t = crate::text::search_in(ctx, &one, &svars, subjects.as_deref(), true)?;
        drop(one);
        drop(decode);
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

/// Whether the vector search counter `key` describes the index the search read rather
/// than work: one call over several query vectors reports these from its first search,
/// and sums the others (`scored`, `searches`). A cursor that runs the kernel once per
/// batch merges its counters by the same rule.
pub(super) fn vector_counter_is_property(key: &str) -> bool {
    matches!(
        key,
        "exactBecause"
            | "index"
            | "ef"
            | "rows"
            | "overlayInserts"
            | "overlayPending"
            | "overlayDeletes"
    )
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
                let decode = ctx.charge(0)?;
                let v = match ctx.term_checked_into(id, &decode)? {
                    Some(oxrdf::Term::Literal(l))
                        if (l.datatype() == oxrdf::vocab::xsd::STRING
                            || l.datatype() == oxrdf::vocab::rdf::LANG_STRING)
                            && text_queries(ctx, pred)? =>
                    {
                        let vec = text_vector(ctx, pred, l.value())?.to_vec();
                        drop(l);
                        drop(decode);
                        Some(vec)
                    }
                    Some(oxrdf::Term::Literal(l)) => {
                        match vector::parse_typed(l.value(), l.datatype().as_str())
                            .and_then(|v| v.ok())
                        {
                            Some(v) => {
                                drop(l);
                                drop(decode);
                                Some(v)
                            }
                            // a literal that is not a vector matches nothing
                            None => continue,
                        }
                    }
                    other => {
                        drop(other);
                        drop(decode);
                        entity_vector(ctx, spec, pred, id)?
                    }
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
        let mut search_charge = ctx.retained_charge(0)?;
        let (hits, info) = vector::search_with_admit(&ctx.snap, &q, &|| ctx.check(), &mut |b| {
            if let Some(h) = &mut search_charge {
                h.resize(b)?;
            }
            Ok(())
        })?;
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

pub(super) type ChargedVectorOrder = (Table, super::ctx::OwnedCharge, Counters);

/// Cursor entry for [`Kind::VectorSearch`] with `order_fallback`: pre-admits search
/// scratch and builds the projected output table directly under an [`OwnedCharge`]
/// before returning so the caller never holds an uncharged table or marks the generic
/// child skipped before admission succeeds.
pub(super) fn vector_search_order_charged(
    ctx: &std::sync::Arc<Ctx>,
    spec: &super::plan::VectorSpec,
    node_vars: &[VarId],
    out_vars: &[VarId],
) -> Result<Option<ChargedVectorOrder>> {
    use super::plan::VectorQuery;
    use crate::vector;
    let mut counters = serde_json::Map::new();
    let Some(pred) = spec.pred else {
        return Ok(None);
    };
    let query = match &spec.query {
        VectorQuery::Vector(v) => v.to_vec(),
        VectorQuery::Text(s) => text_vector(ctx, pred, s)?.to_vec(),
        VectorQuery::Entity(e) => match entity_vector(ctx, spec, pred, *e)? {
            Some(v) => v,
            None => return Ok(None),
        },
        VectorQuery::Var(_) => {
            return Err(Error::invalid(
                "spk:vectorSearch: the query variable is not bound by the rest of the group",
            ));
        }
    };
    let graph = |g: u64| spec.graph.accepts(g);
    let q = vector::Search {
        pred: pred.0,
        query: &query,
        k: spec.k,
        metric: spec.metric,
        graph: &graph,
        dedup: spec.dedup,
        distinct_subject: spec.distinct_subject,
        subjects: None,
        mode: spec.mode,
    };
    let mut scratch = super::ctx::OwnedCharge::new(ctx, 0)?;
    let (hits, info) =
        vector::search_with_admit(&ctx.snap, &q, &|| ctx.check(), &mut |b| scratch.resize(b))?;
    let col = |v: Option<VarId>| v.and_then(|v| node_vars.iter().position(|x| *x == v));
    let cs = match spec.subject {
        PathEnd::Var(v) => col(Some(v)),
        _ => None,
    };
    let (cscore, cvec, cg) = (col(spec.score), col(spec.vector), col(spec.graph_var));
    let est = (hits.len() as u64)
        .saturating_mul(node_vars.len() as u64 * 8)
        .max(hits.len() as u64 * 8 * u64::from(node_vars.is_empty()))
        .saturating_add(node_vars.len() as u64 * 4);
    ctx.check_output(hits.len(), node_vars.len())?;
    let mut out_charge = super::ctx::OwnedCharge::new(ctx, est)?;
    let mut t = Table::new(node_vars.to_vec());
    for col in &mut t.cols {
        col.reserve_exact(hits.len());
    }
    let mut row = vec![Id::UNDEF; node_vars.len()];
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
    drop(hits);
    drop(scratch);
    if t.len() < spec.k {
        return Ok(None);
    }
    let t = if node_vars == out_vars {
        t
    } else {
        let proj_est = (t.len() as u64)
            .saturating_mul(out_vars.len() as u64 * 8)
            .max(t.len() as u64 * 8 * u64::from(out_vars.is_empty()))
            .saturating_add(out_vars.len() as u64 * 4);
        out_charge.resize(est.saturating_add(proj_est))?;
        t.project(out_vars)
    };
    let actual: u64 = t
        .cols
        .iter()
        .map(|c| c.capacity() as u64 * 8)
        .sum::<u64>()
        .max(t.len as u64 * 8 * u64::from(t.width() == 0))
        .saturating_add(t.vars.capacity() as u64 * 4)
        .saturating_add(t.sorted.capacity() as u64 * 4);
    out_charge.resize(actual)?;
    counters.insert("method".into(), info.method.into());
    if let Some(r) = info.reason {
        counters.insert("exactBecause".into(), r.into());
    }
    if let Some(n) = info.index {
        counters.insert("index".into(), n.into());
    }
    if info.ef > 0 {
        counters.insert("ef".into(), info.ef.into());
    }
    counters.insert("rows".into(), info.rows.into());
    counters.insert("scored".into(), info.scored.into());
    counters.insert("overlayInserts".into(), info.inserted.into());
    counters.insert("overlayPending".into(), info.pending.into());
    counters.insert("overlayDeletes".into(), info.deleted.into());
    Ok(Some((t, out_charge, counters)))
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
    let endpoint_credit = ctx.charge(0)?;
    let Some(oxrdf::Term::NamedNode(url)) = ctx.term_checked_into(*id, &endpoint_credit)? else {
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
        let key_credit = ctx.charge(0)?;
        let mut key_reserved = 0usize;
        let key = super::svccache::try_key(&ctx.service_scope, url.as_str(), query, &[], |need| {
            if need > key_reserved {
                key_credit.add((need - key_reserved) as u64)?;
                key_reserved = need;
            }
            Ok::<_, Error>(())
        })?;
        let hit = if cache == CacheMode::Default {
            store.get(&key)
        } else {
            store.remove(&key);
            None
        };
        let rows = match hit {
            Some(rows) => {
                drop(key);
                drop(key_credit);
                rows
            }
            None => {
                let rows = std::sync::Arc::new(super::enhancer::fetch_rows(ctx, &url, query)?);
                store.put(key, rows.clone());
                drop(key_credit);
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
            ctx.check()?;
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
        ctx.check()?;
        t.push_row(&row);
        Ok(())
    })?;
    ctx.check()?;
    Ok(t)
}

/// Whether `url` names a query endpoint of this process (see [`super::LocalEndpoints`]).
pub(super) fn is_local(ctx: &Ctx, url: &str) -> bool {
    ctx.local_endpoints
        .as_ref()
        .is_some_and(|l| l.resolve(url).is_some())
}

// A local decoded row retains its real query owner through the complete callback.
// Remote parser storage is likewise borrowed for that callback, not copied into a
// second uncredited sparesults vector merely to share the dispatch signature.
pub(super) enum ServiceSolution<'a> {
    Remote(&'a sparesults::QuerySolution),
    Local {
        names: &'a [String],
        row: &'a super::output::OwnedRow,
    },
}
impl ServiceSolution<'_> {
    pub(super) fn get(&self, name: &str) -> Option<&oxrdf::Term> {
        match self {
            Self::Remote(solution) => solution.get(name),
            Self::Local { names, row } => row
                .values()
                .get(names.iter().position(|variable| variable == name)?)?
                .as_ref(),
        }
    }
    pub(super) fn variables(&self) -> impl ExactSizeIterator<Item = &str> {
        let (remote, local) = match self {
            Self::Remote(solution) => (Some(solution.variables()), None),
            Self::Local { names, .. } => (None, Some(*names)),
        };
        (0..remote.map_or_else(|| local.expect("local variables").len(), <[_]>::len)).map(
            move |i| match remote {
                Some(names) => names[i].as_str(),
                None => local.expect("local variables")[i].as_str(),
            },
        )
    }
}

/// An in-process SERVICE call: the dataset it reads, the options it runs under and
/// the callee's hold, which lasts as long as the call.
pub(super) struct LocalCall {
    pub(super) snap: std::sync::Arc<crate::store::Snapshot>,
    pub(super) options: super::QueryOptions,
    pub(super) hold: Option<Box<dyn std::any::Any + Send + Sync>>,
}

impl LocalCall {
    /// The call to `target`, as the caller. It keeps the caller's cancellation, grant,
    /// row count and outbound budget, and `deadline`, so it can neither wait for
    /// admission behind its caller nor outlive it.
    pub(super) fn new(
        ctx: &Ctx,
        target: Result<super::LocalTarget>,
        deadline: Option<Instant>,
    ) -> Result<Self> {
        if ctx.service_depth >= super::MAX_SERVICE_DEPTH {
            return Err(Error::Service(format!(
                "SERVICE calls to this server nest more than {} deep",
                super::MAX_SERVICE_DEPTH
            )));
        }
        let super::LocalTarget {
            snap,
            options: mut opts,
            hold,
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
        opts.deadline = deadline;
        opts.cancel = Some(ctx.cancel.clone());
        opts.cancellation = ctx.cancellation.clone();
        opts.grant = ctx.grant().cloned();
        opts.pool = None;
        opts.work = Some(ctx.rows_produced.clone());
        opts.outbound_budget = Some(ctx.outbound_budget.clone());
        opts.service_depth = ctx.service_depth + 1;
        // the call's plan is dropped below, so it names no patterns (item 180)
        opts.no_plan_patterns = true;
        Ok(Self {
            snap,
            options: opts,
            hold,
        })
    }
}

/// Run `query` in process on the dataset `target` names, as the caller, and hand each
/// solution to `on_solution`. The call keeps the caller's deadline, cancellation, grant,
/// row count and outbound budget, so it can neither wait for admission behind its
/// caller nor outlive it.
fn fetch_local(
    ctx: &Ctx,
    target: Result<super::LocalTarget>,
    query: &str,
    on_solution: &mut dyn FnMut(ServiceSolution<'_>) -> Result<()>,
) -> Result<()> {
    let LocalCall {
        snap,
        options: opts,
        hold: _hold,
    } = LocalCall::new(ctx, target, ctx.current_deadline())?;
    ctx.check()?;
    let r = super::query(snap, query, &opts)?;
    for i in 0..r.table().len() {
        if i % 1024 == 0 {
            ctx.check()?;
        }
        let row = r.row(i)?;
        on_solution(ServiceSolution::Local {
            names: r.variables(),
            row: &row,
        })?;
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
    on_solution: &mut dyn FnMut(ServiceSolution<'_>) -> Result<()>,
) -> Result<()> {
    if let Some(target) = ctx
        .local_endpoints
        .as_ref()
        .and_then(|l| l.resolve(url.as_str()))
    {
        return fetch_local(ctx, target, query, on_solution);
    }
    let mut answer = RemoteAnswer::open(ctx, url, query, None)?;
    let _entered = answer.span.clone().entered();
    while let Some(solution) = answer.next_solution() {
        on_solution(ServiceSolution::Remote(&solution?))?;
    }
    Ok(())
}

/// The response of a remote endpoint to one SERVICE request, whose solutions are parsed
/// as they are read. Dropping it closes the response, and the request's time charge in
/// the outbound budget ends then.
pub(super) struct RemoteAnswer {
    /// The client span of the request, which each read enters.
    pub(super) span: tracing::Span,
    url: String,
    solutions: Option<sparesults::ReaderSolutionsParser<Box<dyn std::io::Read + Send>>>,
    /// what the reader under the parser charges by, for a streamed answer
    window: Option<std::sync::Arc<super::svcread::Window>>,
    /// whether the metered JSON route had to buffer the solution sequence before output
    buffered_before_output: bool,
}

/// How a streamed SERVICE answer is charged to its query (see [`super::svcread`]). An
/// eager call reads without one.
pub(super) struct Meter<'a> {
    pub(super) ctx: &'a std::sync::Arc<Ctx>,
    /// whether a response that sparesults would buffer whole is refused
    pub(super) strict: bool,
}

impl RemoteAnswer {
    /// Send `query` to `url` through the outbound policy and read the head of its
    /// response. The transfer's timeout is the outbound policy's, cut to the query's
    /// deadline as it stands now, and it counts from the start of the request.
    pub(super) fn open(
        ctx: &Ctx,
        url: &oxrdf::NamedNode,
        query: &str,
        meter: Option<Meter<'_>>,
    ) -> Result<Self> {
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
        // A streamed answer reads the start of a JSON response first to see whether
        // sparesults would buffer it, and charges what the parser holds as it reads.
        // Late-head JSON responses are validated to EOF before sparesults reads them so
        // trailing truncation or garbage after `"vars"` cannot look like clean EOF.
        let mut buffered_before_output = false;
        let (body, window): (Box<dyn std::io::Read + Send>, _) = match meter {
            None => {
                if syntax == crate::nesting::Syntax::Json {
                    let mut body = body;
                    let mut staging = super::svcread::eager_charge(ctx)?;
                    let start = super::svcread::json_start(&mut body, &mut staging, ctx)
                        .map_err(|e| Self::failed(url.as_str(), e.into()))?;
                    let reader: Box<dyn std::io::Read + Send> = if start.late {
                        let bytes =
                            super::svcread::read_late_json(start, &mut body, &mut staging, ctx)
                                .map_err(|e| Self::failed(url.as_str(), e.into()))?;
                        let parser_reserve = (bytes.len() as u64)
                            .saturating_mul(super::svcread::buffered_factor())
                            .saturating_add(super::svcread::SLACK);
                        Box::new(
                            super::svcread::ChargedCursor::with_parser_reserve(
                                bytes,
                                staging,
                                parser_reserve,
                            )
                            .map_err(|e| Self::failed(url.as_str(), e.into()))?,
                        )
                    } else {
                        let parser_reserve = (start.decided as u64)
                            .saturating_mul(super::svcread::head_factor())
                            .saturating_add(super::svcread::SLACK);
                        let head = super::svcread::ChargedCursor::with_parser_reserve(
                            start.bytes,
                            staging,
                            parser_reserve,
                        )
                        .map_err(|e| Self::failed(url.as_str(), e.into()))?;
                        Box::new(std::io::Read::chain(head, body))
                    };
                    (reader, None)
                } else {
                    (Box::new(body), None)
                }
            }
            Some(meter) => {
                let mut body = body;
                let charge = super::svcread::charge(meter.ctx)?;
                let window = std::sync::Arc::new(super::svcread::Window::default());
                let reader: Box<dyn std::io::Read + Send> =
                    if syntax == crate::nesting::Syntax::Json {
                        let mut staging = super::svcread::StagingCharge::from_cursor(charge);
                        let start = super::svcread::json_start(&mut body, &mut staging, meter.ctx)
                            .map_err(|e| Self::failed(url.as_str(), e.into()))?;
                        if start.late && meter.strict {
                            return Err(Error::Service(format!(
                                "{}<{}>",
                                super::cursor::LATE_HEAD,
                                url.as_str()
                            )));
                        }
                        buffered_before_output = start.late;
                        if start.late {
                            let bytes = super::svcread::read_late_json(
                                start,
                                &mut body,
                                &mut staging,
                                meter.ctx,
                            )
                            .map_err(|e| Self::failed(url.as_str(), e.into()))?;
                            Box::new(super::svcread::Metered::precharged(
                                std::io::Cursor::new(bytes),
                                staging.into_cursor(),
                                window.clone(),
                            ))
                        } else {
                            let base = (start.decided as u64)
                                .saturating_mul(super::svcread::head_factor());
                            let bytes = std::io::Cursor::new(start.bytes);
                            Box::new(super::svcread::Metered::new(
                                std::io::Read::chain(bytes, body),
                                staging.into_cursor(),
                                base,
                                false,
                                window.clone(),
                            ))
                        }
                    } else {
                        Box::new(super::svcread::Metered::new(
                            body,
                            charge,
                            0,
                            false,
                            window.clone(),
                        ))
                    };
                (reader, Some(window))
            }
        };
        // parsed as it streams in, under the policy's byte ceiling and deadline
        let parser = sparesults::QueryResultsParser::from_format(fmt);
        let solutions = match parser
            .for_reader(body)
            .map_err(|e| Self::failed(url.as_str(), e))?
        {
            sparesults::ReaderQueryResultsParserOutput::Solutions(sols) => Some(sols),
            sparesults::ReaderQueryResultsParserOutput::Boolean(_) => None,
        };
        drop(_entered);
        Ok(Self {
            span,
            url: url.as_str().to_string(),
            solutions,
            window,
            buffered_before_output,
        })
    }

    /// Whether the metered parser route buffers the solution sequence before yielding
    /// its first solution (for example, JSON results preceding head variables).
    pub(super) fn buffered_before_output(&self) -> bool {
        self.buffered_before_output
    }

    /// Construct a `RemoteAnswer` directly over an arbitrary reader for unit tests of
    /// `Service::fill`, using the ordinary `sparesults` parser with no network state.
    #[cfg(test)]
    pub(super) fn from_reader(
        reader: Box<dyn std::io::Read + Send>,
        format: sparesults::QueryResultsFormat,
        url: &str,
    ) -> Result<Self> {
        let parser = sparesults::QueryResultsParser::from_format(format);
        let solutions = match parser
            .for_reader(reader)
            .map_err(|e| Self::failed(url, e))?
        {
            sparesults::ReaderQueryResultsParserOutput::Solutions(sols) => Some(sols),
            sparesults::ReaderQueryResultsParserOutput::Boolean(_) => None,
        };
        Ok(Self {
            span: tracing::Span::none(),
            url: url.to_string(),
            solutions,
            window: None,
            buffered_before_output: false,
        })
    }

    /// The next solution of the response, or `None` once it has ended. A failure ends
    /// the answer, and the response is closed then.
    pub(super) fn next_solution(&mut self) -> Option<Result<sparesults::QuerySolution>> {
        let next = self.solutions.as_mut()?.next();
        match next {
            Some(Ok(solution)) => {
                if let Some(window) = &self.window {
                    window.solution();
                }
                Some(Ok(solution))
            }
            Some(Err(e)) => {
                self.solutions = None;
                Some(Err(Self::failed(&self.url, e)))
            }
            None => {
                self.solutions = None;
                None
            }
        }
    }

    fn failed(url: &str, e: sparesults::QueryResultsParseError) -> Error {
        match e {
            // a spent budget, or the body's own words (timeout, size, connection)
            sparesults::QueryResultsParseError::Io(e) => match crate::codec::io_error(e) {
                Error::Io(e) => Error::Service(format!("<{url}>: {e}")),
                e => e,
            },
            e => Error::Service(format!("<{url}>: {e}")),
        }
    }
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
mod path_lookup_tests {
    use super::*;
    use crate::io::{RdfFormat, Source};
    use crate::store::{Store, StoreOptions};

    /// Start nodes are read on from where each index stopped, so that rows of another
    /// graph that one index has already passed are not read again for each chunk.
    #[test]
    fn start_nodes_read_each_index_row_once() {
        let store = Store::in_memory(StoreOptions::default());
        let mut data = String::new();
        for i in 0..20_000 {
            data.push_str(&format!("_:b{i} <urn:p> <urn:one> <urn:g1> .\n"));
        }
        for i in 0..50_000 {
            data.push_str(&format!("<urn:s:{i}> <urn:q> <urn:o:{i}> <urn:g2> .\n"));
        }
        store
            .load(&[Source::from_bytes(
                data.into_bytes(),
                RdfFormat::NQuads,
                None,
            )])
            .unwrap();
        let ctx = Ctx::new(store.snapshot());
        let g1 = ctx
            .intern_term(&oxrdf::Term::NamedNode(
                oxrdf::NamedNode::new("urn:g1").unwrap(),
            ))
            .0;
        let v = ctx.var("s");
        let spec = PathSpec {
            subj: PathEnd::Var(v),
            obj: PathEnd::Var(ctx.var("o")),
            min: 0,
            max_one: false,
            simple: None,
            preds: None,
            graph: GraphFilter::One(g1),
            graph_var: None,
            edge_vars: None,
            count: None,
            src: None,
        };
        let before = START_ROWS.with(|n| n.get());
        let mut starts = StartNodes::new(&spec);
        let mut nodes = Vec::new();
        while let Some(chunk) = starts.next(&ctx, &GraphFilter::One(g1)).unwrap() {
            assert!(nodes.last().is_none_or(|l| chunk[0] > *l));
            nodes.extend(chunk);
        }
        let read = START_ROWS.with(|n| n.get()) - before;
        assert_eq!(nodes.len(), 20_001);
        // each of the 70,000 quads once in SPO and once in OSP, and a row per chunk
        assert!(read <= 2 * 70_000 + 64, "{read} rows read");
    }

    /// A lookup that reads many index rows checks the deadline even when the active
    /// graph rejects every row, so that no neighbour is added to its buffer.
    #[test]
    fn lookup_checks_the_deadline_over_rejected_rows() {
        let store = Store::in_memory(StoreOptions::default());
        let mut data = String::new();
        for i in 0..200_000 {
            let p = if i % 2 == 0 { "p" } else { "q" };
            data.push_str(&format!("<urn:hub> <urn:{p}> <urn:n:{i}> <urn:g1> .\n"));
        }
        data.push_str("<urn:a> <urn:p> <urn:b> <urn:g2> .\n");
        store
            .load(&[Source::from_bytes(
                data.into_bytes(),
                RdfFormat::NQuads,
                None,
            )])
            .unwrap();
        let mut ctx = Ctx::new(store.snapshot());
        let term = |ctx: &Ctx, iri: &str| {
            ctx.intern_term(&oxrdf::Term::NamedNode(oxrdf::NamedNode::new(iri).unwrap()))
                .0
        };
        let (hub, p, q, g2) = (
            term(&ctx, "urn:hub"),
            term(&ctx, "urn:p"),
            term(&ctx, "urn:q"),
            term(&ctx, "urn:g2"),
        );
        ctx.deadline = Some(Instant::now());
        let v = ctx.var("t");
        for (simple, preds) in [
            (Some((p, false)), None),
            (None, Some(vec![(p, false), (q, false)])),
        ] {
            let spec = PathSpec {
                subj: PathEnd::Const(Id(hub)),
                obj: PathEnd::Var(v),
                min: 1,
                max_one: false,
                simple,
                preds,
                graph: GraphFilter::One(g2),
                graph_var: None,
                edge_vars: None,
                count: None,
                src: None,
            };
            let sweeps = std::cell::Cell::new(0);
            let g = Graph {
                ctx: &ctx,
                spec: &spec,
                graph: GraphFilter::One(g2),
                fwd: None,
                bwd: None,
                sweeps: &sweeps,
            };
            assert!(matches!(g.neighbours(hub, true), Err(Error::Timeout)));
        }
    }
}

#[cfg(test)]
mod paid_eager_producer_tests {
    use super::*;
    use crate::store::{Store, StoreOptions};

    #[test]
    fn r2_topk_scan_refuses_numeric_ties_and_delta_before_the_full_read() {
        use crate::io::Source;
        use crate::sparql::{QueryOptions, update::update};
        use oxrdfio::RdfFormat;

        let store = Store::in_memory(StoreOptions::default());
        let data = (0..512)
            .map(|i| format!("<urn:tie:{i}> <urn:p> 42 .\n"))
            .collect::<String>();
        store
            .load(&[Source::from_bytes(
                data.into_bytes(),
                RdfFormat::Turtle,
                None,
            )])
            .unwrap();
        let data = (0..512)
            .map(|i| format!("<urn:delta:{i}> <urn:p> \"value-{i}\" .\n"))
            .collect::<String>();
        update(
            &store,
            &format!("INSERT DATA {{ {data} }}"),
            &QueryOptions::default(),
        )
        .unwrap();
        let mut ctx = Ctx::new(store.snapshot());
        ctx.mem_limit = 512;
        let pred = ctx.snap.lookup_iri("urn:p").unwrap();
        let scan = ScanSpec {
            perm: Perm::Pos,
            prefix: vec![pred.0],
            cols: vec![(1, 0), (2, 1)],
            eqs: Vec::new(),
            graph: GraphFilter::Default,
            graph_col: 3,
            dedup: false,
            src: None,
        };
        let spec = OrderedTopK {
            scan,
            var: 0,
            asc: true,
            k: 2,
            pieces: Vec::new(),
            filter: Vec::new(),
            range_filter: Vec::new(),
            tie_order: vec![1],
            fallback: Box::new(Node::empty(vec![0, 1])),
        };
        let tied = ctx
            .snap
            .lookup_term(&oxrdf::Literal::from(42_i64).into())
            .unwrap();
        let delta = ctx
            .snap
            .lookup_term(&oxrdf::Literal::new_simple_literal("value-0").into())
            .unwrap();
        let [lo, hi] = [0, crate::id::PAYLOAD_MASK].map(|payload| Id::new(delta.tag(), payload).0);
        for (lower, upper) in [(tied.0, tied.0), (lo, hi)] {
            let err = topk_read(&ctx, &spec, lower, upper, true, &[0, 1])
                .err()
                .unwrap();
            assert!(matches!(
                err,
                Error::BudgetExceeded(b) if b.kind == crate::error::BudgetKind::Memory && b.requested > 512
            ));
            assert_eq!(ctx.live_bytes(), 0);
        }
    }

    #[test]
    fn r3_topk_residual_range_filter_refuses_before_copying_delta_rows() {
        use crate::io::Source;
        use crate::sparql::{QueryOptions, expr::CmpOp, update::update};
        use oxrdfio::RdfFormat;

        let store = Store::in_memory(StoreOptions::default());
        store
            .load(&[Source::from_bytes(
                b"<urn:base> <urn:p> \"9999.5\"^^<http://www.w3.org/2001/XMLSchema#float> .\n"
                    .to_vec(),
                RdfFormat::Turtle,
                None,
            )])
            .unwrap();
        let data = (0..512)
            .map(|i| {
                format!(
                    "<urn:delta:{i}> <urn:p> \"{i}.5\"^^<http://www.w3.org/2001/XMLSchema#float> .\n"
                )
            })
            .collect::<String>();
        update(
            &store,
            &format!("INSERT DATA {{ {data} }}"),
            &QueryOptions::default(),
        )
        .unwrap();

        let snap = store.snapshot();
        assert!(snap.generation.vocab.numeric().is_some());
        let probe = Ctx::new(snap.clone());
        let pred = probe.snap.lookup_iri("urn:p").unwrap();
        let delta = probe
            .snap
            .lookup_term(&oxrdf::Literal::from(0.5_f32).into())
            .unwrap();
        assert_eq!(delta.tag(), crate::id::Tag::Delta);
        let [lo, hi] = [0, crate::id::PAYLOAD_MASK].map(|payload| Id::new(delta.tag(), payload).0);
        let threshold = Id::from_i64(100).unwrap();
        let spec = OrderedTopK {
            scan: ScanSpec {
                perm: Perm::Pos,
                prefix: vec![pred.0],
                cols: vec![(1, 0), (2, 1)],
                eqs: Vec::new(),
                graph: GraphFilter::Default,
                graph_col: 3,
                dedup: false,
                src: None,
            },
            var: 0,
            asc: true,
            k: 2,
            pieces: Vec::new(),
            filter: Vec::new(),
            range_filter: vec![Expr::Cmp(
                Box::new(Expr::Var(0)),
                Box::new(Expr::Const(threshold)),
                CmpOp::Ge,
            )],
            tie_order: vec![1],
            fallback: Box::new(Node::empty(vec![0, 1])),
        };

        // Measure the raw delta table's admitted charge with no residual copy.
        let (raw, scanned) = topk_read(&probe, &spec, lo, hi, true, &[0, 1]).unwrap();
        assert_eq!(scanned, 512);
        let raw_bytes = probe.live_bytes();
        drop(raw);
        assert_eq!(probe.live_bytes(), 0);

        // Allow the raw table plus `decided`/`keep`/`rest`, but not the residual `sub` table copy.
        let mut tight = Ctx::new(snap.clone());
        let scratch_without_copy = 512u64 * (1 + 1 + size_of::<usize>() as u64);
        tight.mem_limit = raw_bytes + scratch_without_copy + 256;
        let err = topk_read(&tight, &spec, lo, hi, false, &[0, 1])
            .err()
            .unwrap();
        assert!(matches!(
            err,
            Error::BudgetExceeded(b)
                if b.kind == crate::error::BudgetKind::Memory && b.requested > tight.mem_limit
        ));
        assert_eq!(tight.live_bytes(), 0);

        // With room for the residual copy, the non-exact range filter keeps the 412 matching rows.
        let ok_ctx = Ctx::new(snap);
        let (filtered, scanned) = topk_read(&ok_ctx, &spec, lo, hi, false, &[0, 1]).unwrap();
        assert_eq!(scanned, 512);
        assert_eq!(filtered.table.len(), 412);
        let before_drop = ok_ctx.live_bytes();
        drop(filtered);
        assert_eq!(before_drop - ok_ctx.live_bytes(), raw_bytes);
    }

    #[test]
    fn r4_topk_parallel_range_filter_admits_disjoint_decisions() {
        use crate::io::Source;
        use crate::sparql::{QueryOptions, expr::CmpOp, update::update};
        use oxrdfio::RdfFormat;

        let n = PAR_THRESHOLD + 64;
        let store = Store::in_memory(StoreOptions::default());
        store
            .load(&[Source::from_bytes(
                b"<urn:base> <urn:p> \"999999.5\"^^<http://www.w3.org/2001/XMLSchema#float> .\n"
                    .to_vec(),
                RdfFormat::Turtle,
                None,
            )])
            .unwrap();
        let data = (0..n)
            .map(|i| {
                format!(
                    "<urn:delta:{i}> <urn:p> \"{i}.5\"^^<http://www.w3.org/2001/XMLSchema#float> .\n"
                )
            })
            .collect::<String>();
        update(
            &store,
            &format!("INSERT DATA {{ {data} }}"),
            &QueryOptions::default(),
        )
        .unwrap();

        let snap = store.snapshot();
        assert!(snap.generation.vocab.numeric().is_some());
        let probe = Ctx::new(snap.clone());
        let pred = probe.snap.lookup_iri("urn:p").unwrap();
        let delta = probe
            .snap
            .lookup_term(&oxrdf::Literal::from(0.5_f32).into())
            .unwrap();
        assert_eq!(delta.tag(), crate::id::Tag::Delta);
        let [lo, hi] = [0, crate::id::PAYLOAD_MASK].map(|payload| Id::new(delta.tag(), payload).0);
        let threshold = Id::from_i64(100).unwrap();
        let spec = OrderedTopK {
            scan: ScanSpec {
                perm: Perm::Pos,
                prefix: vec![pred.0],
                cols: vec![(1, 0), (2, 1)],
                eqs: Vec::new(),
                graph: GraphFilter::Default,
                graph_col: 3,
                dedup: false,
                src: None,
            },
            var: 0,
            asc: true,
            k: 2,
            pieces: Vec::new(),
            filter: Vec::new(),
            range_filter: vec![Expr::Cmp(
                Box::new(Expr::Var(0)),
                Box::new(Expr::Const(threshold)),
                CmpOp::Ge,
            )],
            tie_order: vec![1],
            fallback: Box::new(Node::empty(vec![0, 1])),
        };

        let (raw, scanned) = topk_read(&probe, &spec, lo, hi, true, &[0, 1]).unwrap();
        assert_eq!(scanned, n);
        let raw_bytes = probe.live_bytes();

        let decided_bytes = n as u64 * size_of::<Option<bool>>() as u64;
        let keep_bytes = n as u64 * size_of::<bool>() as u64;
        let rest_bytes = n as u64 * size_of::<usize>() as u64;
        let sub_bytes = topk_table_bytes(n, 2);
        let mask_bytes = (n * size_of::<bool>()
            + probe.nvars() * size_of::<Option<usize>>()
            + size_of::<&Expr>()) as u64;

        // Direct Phase 1 & Phase 2 checks on the already-scanned table (so `scan_into`'s
        // power-of-two doubling peak cannot mask the exact Phase 1/2 boundaries):
        // 1 byte below Phase 1 (`decided` disjoint output slice) refuses cleanly.
        let mut p1 = Ctx::new(snap.clone());
        p1.mem_limit = raw_bytes + decided_bytes - 1;
        let held = p1.charge(raw_bytes).unwrap();
        let mut tbl = raw.table.clone();
        let err = apply_range_filter(&p1, &mut tbl, 0, &spec.range_filter)
            .err()
            .unwrap();
        assert!(matches!(
            err,
            Error::BudgetExceeded(b)
                if b.kind == crate::error::BudgetKind::Memory
                    && b.requested == raw_bytes + decided_bytes
        ));
        drop(held);
        assert_eq!(p1.live_bytes(), 0);

        // Room for a single `decided` copy passes Phase 1 (no extra Rayon collector copy)
        // and refuses at Phase 2 (`decided + keep + rest`).
        let mut p2 = Ctx::new(snap.clone());
        p2.mem_limit = raw_bytes + decided_bytes + 512;
        let held = p2.charge(raw_bytes).unwrap();
        let mut tbl = raw.table.clone();
        let err = apply_range_filter(&p2, &mut tbl, 0, &spec.range_filter)
            .err()
            .unwrap();
        assert!(matches!(
            err,
            Error::BudgetExceeded(b)
                if b.kind == crate::error::BudgetKind::Memory
                    && b.requested == raw_bytes + decided_bytes + keep_bytes + rest_bytes
        ));
        drop(held);
        assert_eq!(p2.live_bytes(), 0);
        drop(raw);
        assert_eq!(probe.live_bytes(), 0);

        // End-to-end `topk_read` 1 byte below Phase 3 (`keep + rest + sub + mask`) passes
        // `scan_into`, Phase 1 parallel slices, and Phase 2, then refuses before copying `sub`.
        let mut p3 = Ctx::new(snap.clone());
        let p3_peak = raw_bytes + keep_bytes + rest_bytes + sub_bytes + mask_bytes;
        p3.mem_limit = p3_peak - 1;
        let err = topk_read(&p3, &spec, lo, hi, false, &[0, 1]).err().unwrap();
        assert!(matches!(
            err,
            Error::BudgetExceeded(b)
                if b.kind == crate::error::BudgetKind::Memory && b.requested == p3_peak
        ));
        assert_eq!(p3.live_bytes(), 0);

        // With room for the residual copy, all `n - 100` matching rows survive.
        let ok_ctx = Ctx::new(snap);
        let (filtered, scanned) = topk_read(&ok_ctx, &spec, lo, hi, false, &[0, 1]).unwrap();
        assert_eq!(scanned, n);
        assert_eq!(filtered.table.len(), n - 100);
        let before_drop = ok_ctx.live_bytes();
        drop(filtered);
        assert_eq!(before_drop - ok_ctx.live_bytes(), raw_bytes);
    }

    #[test]
    fn r4_filter_vocabulary_prefetch_admits_or_skips_under_tight_budget() {
        use crate::io::Source;
        use crate::sparql::expr::CmpOp;
        use crate::vocab::numeric::Numeric;
        use oxrdfio::RdfFormat;

        let n = 2048usize;
        let half = n / 2;
        let dir = tempfile::tempdir().unwrap();
        // Use a disk-backed store so the base vocabulary is file-mapped (`Bytes::Map`)
        // and exercises `prefetch_sorted_checked`'s block-index admission callback.
        let store = Store::open(dir.path(), StoreOptions::default()).unwrap();
        // Include one packed float on an unrelated predicate so `vocab.num` is built, plus
        // 2,048 21-digit decimals (half negative, half positive) that exceed both inline
        // Tag::Decimal and vocab.num's 58-bit packed mantissa so they remain Tag::Vocab +
        // Numeric::Unknown and reach the residual filter + prefetch path.
        let mut data = String::from(
            "<urn:base> <urn:other> \"999999.5\"^^<http://www.w3.org/2001/XMLSchema#float> .\n",
        );
        for i in 0..half {
            data.push_str(&format!(
                "<urn:dec:neg:{i}> <urn:p> \"-100000000000000000000.{i:04}\"^^<http://www.w3.org/2001/XMLSchema#decimal> .\n"
            ));
        }
        for i in 0..(n - half) {
            data.push_str(&format!(
                "<urn:dec:pos:{i}> <urn:p> \"100000000000000000000.{i:04}\"^^<http://www.w3.org/2001/XMLSchema#decimal> .\n"
            ));
        }
        store
            .load(&[Source::from_bytes(
                data.into_bytes(),
                RdfFormat::Turtle,
                None,
            )])
            .unwrap();
        let snap = store.snapshot();
        let num = snap
            .generation
            .vocab
            .numeric()
            .expect("base vocab numeric column");
        let decimal_dt =
            oxrdf::NamedNode::new_unchecked("http://www.w3.org/2001/XMLSchema#decimal");
        let neg_sample = snap
            .lookup_term(
                &oxrdf::Literal::new_typed_literal(
                    "-100000000000000000000.0000",
                    decimal_dt.clone(),
                )
                .into(),
            )
            .unwrap();
        let pos_sample = snap
            .lookup_term(
                &oxrdf::Literal::new_typed_literal("100000000000000000000.0000", decimal_dt).into(),
            )
            .unwrap();
        assert_eq!(neg_sample.tag(), crate::id::Tag::Vocab);
        assert_eq!(pos_sample.tag(), crate::id::Tag::Vocab);
        assert_eq!(
            num.get_many(&[neg_sample.payload(), pos_sample.payload()]),
            vec![Numeric::Unknown, Numeric::Unknown]
        );

        let make_ctx = || {
            let ctx = Ctx::new(snap.clone());
            assert_eq!(ctx.var("v"), 0);
            assert_eq!(ctx.var("s"), 1);
            ctx
        };
        let probe = make_ctx();
        let pred = probe.snap.lookup_iri("urn:p").unwrap();
        let [lo, hi] =
            [0, crate::id::PAYLOAD_MASK].map(|payload| Id::new(pos_sample.tag(), payload).0);
        let threshold = Id::from_i64(0).unwrap();
        let filter_expr = Expr::Cmp(
            Box::new(Expr::Var(0)),
            Box::new(Expr::Const(threshold)),
            CmpOp::Ge,
        );
        let base_scan = || ScanSpec {
            perm: Perm::Pos,
            prefix: vec![pred.0],
            cols: vec![(1, 0), (2, 1)],
            eqs: Vec::new(),
            graph: GraphFilter::Default,
            graph_col: 3,
            dedup: false,
            src: None,
        };
        let raw_spec = OrderedTopK {
            scan: base_scan(),
            var: 0,
            asc: true,
            k: 2,
            pieces: Vec::new(),
            filter: Vec::new(),
            range_filter: Vec::new(),
            tie_order: vec![1],
            fallback: Box::new(Node::empty(vec![0, 1])),
        };
        let generic_spec = OrderedTopK {
            scan: base_scan(),
            var: 0,
            asc: true,
            k: 2,
            pieces: Vec::new(),
            filter: vec![filter_expr.clone()],
            range_filter: Vec::new(),
            tie_order: vec![1],
            fallback: Box::new(Node::empty(vec![0, 1])),
        };
        let range_spec = OrderedTopK {
            scan: base_scan(),
            var: 0,
            asc: true,
            k: 2,
            pieces: Vec::new(),
            filter: Vec::new(),
            range_filter: vec![filter_expr],
            tie_order: vec![1],
            fallback: Box::new(Node::empty(vec![0, 1])),
        };

        let (raw, scanned) = topk_read(&probe, &raw_spec, lo, hi, true, &[0, 1]).unwrap();
        assert_eq!(scanned, n);
        let raw_bytes = probe.live_bytes();
        assert_eq!(probe.mem_peak(), raw_bytes);

        // Compute the expected surviving rows once under an unconstrained budget.
        let baseline_ctx = make_ctx();
        let (baseline, scanned) =
            topk_read(&baseline_ctx, &generic_spec, lo, hi, true, &[0, 1]).unwrap();
        assert_eq!(scanned, n);
        let expected_kept = baseline.table.cols[0].clone();
        assert_eq!(expected_kept.len(), n - half);
        assert!(expected_kept.contains(&pos_sample));
        assert!(!expected_kept.contains(&neg_sample));
        drop(baseline);

        let ids_count = || PREFETCH_COLUMN_TERMS_ALLOCATED.with(|c| c.get());
        let blocks_count = || PREFETCH_COLUMN_TERMS_BLOCKS_ADMITTED.with(|c| c.get());
        let io_hints = crate::index::io_hints();
        let ids_bytes = (n * size_of::<u64>()) as u64;
        let blocks_bytes = (n * size_of::<usize>()) as u64;

        // Direct check 1: 1 byte below `ids_bytes` skips the ID vector and block-index hint.
        let mut tight_ids = make_ctx();
        tight_ids.mem_limit = ids_bytes - 1;
        let (before_ids, before_blocks) = (ids_count(), blocks_count());
        prefetch_column_terms(&tight_ids, &raw.table, &[0]);
        assert_eq!(ids_count(), before_ids);
        assert_eq!(blocks_count(), before_blocks);
        assert_eq!(tight_ids.mem_peak(), 0);
        assert_eq!(tight_ids.live_bytes(), 0);

        // Direct check 2: room for `ids_bytes` alone (1 byte below `ids_bytes + blocks_bytes`)
        // admits the ID vector, refuses the mapped block-index vector, and releases cleanly.
        let mut ids_only = make_ctx();
        ids_only.mem_limit = ids_bytes + blocks_bytes - 1;
        let (before_ids, before_blocks) = (ids_count(), blocks_count());
        prefetch_column_terms(&ids_only, &raw.table, &[0]);
        assert_eq!(ids_count(), before_ids + 1);
        assert_eq!(blocks_count(), before_blocks);
        assert_eq!(ids_only.mem_peak(), ids_bytes);
        assert_eq!(ids_only.live_bytes(), 0);

        // Direct check 3: exact room for both ID and mapped block-index vectors admits both
        // (when I/O hints are enabled) and releases cleanly.
        let mut both_direct = make_ctx();
        both_direct.mem_limit = ids_bytes + blocks_bytes;
        let (before_ids, before_blocks) = (ids_count(), blocks_count());
        prefetch_column_terms(&both_direct, &raw.table, &[0]);
        assert_eq!(ids_count(), before_ids + 1);
        if io_hints {
            assert_eq!(blocks_count(), before_blocks + 1);
            assert_eq!(both_direct.mem_peak(), ids_bytes + blocks_bytes);
        } else {
            assert_eq!(blocks_count(), before_blocks);
            assert_eq!(both_direct.mem_peak(), ids_bytes);
        }
        assert_eq!(both_direct.live_bytes(), 0);

        drop(raw);
        assert_eq!(probe.live_bytes(), 0);

        // Generic top-k filter entry point (`topk_read_keys_last` with non-empty `spec.filter`):
        // verify exact scalar results on ID refusal, on block-index refusal, and when both
        // prefetch vectors are admitted. Pre-admit the parallel decode envelope for the
        // decimal keys so strict eager decode accounting fits regardless of host thread count
        // while `mem_limit` pins the exact remaining headroom around prefetch scratch.
        let filter_scratch = (n as u64).saturating_add(
            (probe.nvars() as u64).saturating_mul(size_of::<Option<usize>>() as u64),
        );
        let warm_decode = |ctx: &Ctx| -> u64 {
            rayon::join(
                || {
                    let _ = ctx.value(neg_sample);
                    let _ = ctx.value(pos_sample);
                },
                || (),
            );
            ctx.live_bytes()
        };

        // Generic top-k case 1: 1 byte below `ids_bytes` skips prefetch and keeps exact scalar
        // results within budget.
        let mut tight_ids_topk = make_ctx();
        let decode_bytes = warm_decode(&tight_ids_topk);
        tight_ids_topk.opt.prefetch_terms = true;
        tight_ids_topk.mem_limit = decode_bytes + raw_bytes + filter_scratch + ids_bytes - 1;
        let (before_ids, before_blocks) = (ids_count(), blocks_count());
        let (filtered, scanned) =
            topk_read(&tight_ids_topk, &generic_spec, lo, hi, true, &[0, 1]).unwrap();
        assert_eq!(scanned, n);
        assert_eq!(filtered.table.cols[0], expected_kept);
        assert_eq!(ids_count(), before_ids);
        assert_eq!(blocks_count(), before_blocks);
        assert!(tight_ids_topk.mem_peak() <= tight_ids_topk.mem_limit);
        let before_drop = tight_ids_topk.live_bytes();
        drop(filtered);
        assert_eq!(before_drop - tight_ids_topk.live_bytes(), raw_bytes);

        // Generic top-k case 2: room for `ids_bytes` alone admits IDs, refuses the mapped
        // block-index resize, and still returns exact scalar results.
        let mut ids_only_topk = make_ctx();
        let decode_bytes = warm_decode(&ids_only_topk);
        ids_only_topk.opt.prefetch_terms = true;
        ids_only_topk.mem_limit =
            decode_bytes + raw_bytes + filter_scratch + ids_bytes + blocks_bytes - 1;
        let (before_ids, before_blocks) = (ids_count(), blocks_count());
        let (filtered, scanned) =
            topk_read(&ids_only_topk, &generic_spec, lo, hi, true, &[0, 1]).unwrap();
        assert_eq!(scanned, n);
        assert_eq!(filtered.table.cols[0], expected_kept);
        if io_hints {
            assert_eq!(ids_count(), before_ids + 1);
            assert_eq!(blocks_count(), before_blocks);
            assert!(
                ids_only_topk.mem_peak() >= decode_bytes + raw_bytes + filter_scratch + ids_bytes
            );
            assert!(ids_only_topk.mem_peak() <= ids_only_topk.mem_limit);
        } else {
            assert_eq!(ids_count(), before_ids);
            assert_eq!(blocks_count(), before_blocks);
        }
        let before_drop = ids_only_topk.live_bytes();
        drop(filtered);
        assert_eq!(before_drop - ids_only_topk.live_bytes(), raw_bytes);

        // Generic top-k case 3: exact room for both ID and mapped block-index vectors admits
        // both through `topk_read` -> `apply_filter` -> `decode_for` -> `prefetch_column_terms`.
        let mut both_topk = make_ctx();
        let decode_bytes = warm_decode(&both_topk);
        both_topk.opt.prefetch_terms = true;
        both_topk.mem_limit = decode_bytes + raw_bytes + filter_scratch + ids_bytes + blocks_bytes;
        let (before_ids, before_blocks) = (ids_count(), blocks_count());
        let (filtered, scanned) =
            topk_read(&both_topk, &generic_spec, lo, hi, true, &[0, 1]).unwrap();
        assert_eq!(scanned, n);
        assert_eq!(filtered.table.cols[0], expected_kept);
        if io_hints {
            assert_eq!(ids_count(), before_ids + 1);
            assert_eq!(blocks_count(), before_blocks + 1);
            assert_eq!(
                both_topk.mem_peak(),
                decode_bytes + raw_bytes + filter_scratch + ids_bytes + blocks_bytes
            );
        } else {
            assert_eq!(ids_count(), before_ids);
            assert_eq!(blocks_count(), before_blocks);
        }
        let before_drop = both_topk.live_bytes();
        drop(filtered);
        assert_eq!(before_drop - both_topk.live_bytes(), raw_bytes);

        // End-to-end `topk_read` (`exact = false`) over the 2,048 `Numeric::Unknown` base
        // decimals keeps the exact positive half and releases the table charge cleanly.
        let ok_topk = make_ctx();
        let (filtered, scanned) = topk_read(&ok_topk, &range_spec, lo, hi, false, &[0, 1]).unwrap();
        assert_eq!(scanned, n);
        assert_eq!(filtered.table.cols[0], expected_kept);
        let before_drop = ok_topk.live_bytes();
        drop(filtered);
        assert_eq!(before_drop - ok_topk.live_bytes(), raw_bytes);
    }

    #[test]
    fn prefetch_column_terms_bounds_cursor_ledger_ops_and_skips_warm_batches() {
        use crate::io::Source;
        use oxrdfio::RdfFormat;

        let dir = tempfile::tempdir().unwrap();
        let disk = Store::open(dir.path(), StoreOptions::default()).unwrap();
        let mut data = String::from("@prefix ex: <http://ex.org/> .\n");
        for i in 0..256 {
            data.push_str(&format!("ex:s{i} ex:p \"word {i:04}\" .\n"));
        }
        disk.load(&[Source::from_bytes(
            data.as_bytes().to_vec(),
            RdfFormat::Turtle,
            None,
        )])
        .unwrap();
        let snap = disk.snapshot();
        let make_batch = |range: std::ops::Range<usize>| {
            let mut tbl = Table::new(vec![0]);
            for i in range {
                let id = snap
                    .lookup_term(&oxrdf::Literal::new_simple_literal(format!("word {i:04}")).into())
                    .unwrap();
                tbl.push_row(&[id]);
            }
            tbl
        };
        let batch1 = make_batch(0..64);
        let batch2 = make_batch(64..128);
        let batch3 = make_batch(128..192);
        let ids_bytes = (64 * size_of::<u64>()) as u64;

        // Under a tight cursor budget below `ids_bytes`, repeated cold batches skip
        // cleanly without allocating scratch, poisoning `ctx.check()`, or repeating
        // failed ledger operations.
        let mut tight = Ctx::new(snap.clone());
        tight.mem_limit = ids_bytes - 1;
        tight.enable_cursor_accounting().unwrap();
        let before_tight = PREFETCH_COLUMN_TERMS_ALLOCATED.with(|c| c.get());
        prefetch_column_terms(&tight, &batch1, &[0]);
        assert_eq!(
            PREFETCH_COLUMN_TERMS_ALLOCATED.with(|c| c.get()),
            before_tight
        );
        assert!(tight.check().is_ok());
        assert_eq!(tight.live_bytes(), 0);

        let before_tight2_allocs = PREFETCH_COLUMN_TERMS_ALLOCATED.with(|c| c.get());
        let before_tight2_ops = super::super::query_memory::LEDGER_OPS.with(|n| n.get());
        prefetch_column_terms(&tight, &batch1, &[0]);
        assert_eq!(
            PREFETCH_COLUMN_TERMS_ALLOCATED.with(|c| c.get()),
            before_tight2_allocs
        );
        assert_eq!(
            super::super::query_memory::LEDGER_OPS.with(|n| n.get()),
            before_tight2_ops,
            "second tight-budget cold call must not repeat failed ledger admissions"
        );
        assert!(tight.check().is_ok());
        assert_eq!(tight.live_bytes(), 0);

        // When only `ids_bytes` fits (below the upper bound `ids_bytes + max_blocks`),
        // the first cold batch admits only `ids_bytes`, and a second cold batch reuses
        // it without repeating failed upper-bound or block-vector ledger admissions.
        let mut ids_only = Ctx::new(snap.clone());
        ids_only.mem_limit = ids_bytes;
        ids_only.enable_cursor_accounting().unwrap();
        let before_ids1 = PREFETCH_COLUMN_TERMS_ALLOCATED.with(|c| c.get());
        prefetch_column_terms(&ids_only, &batch1, &[0]);
        assert_eq!(
            PREFETCH_COLUMN_TERMS_ALLOCATED.with(|c| c.get()),
            before_ids1 + 1
        );
        assert_eq!(ids_only.live_bytes(), ids_bytes);

        let before_ids2_allocs = PREFETCH_COLUMN_TERMS_ALLOCATED.with(|c| c.get());
        let before_ids2_ops = super::super::query_memory::LEDGER_OPS.with(|n| n.get());
        prefetch_column_terms(&ids_only, &batch3, &[0]);
        assert_eq!(
            PREFETCH_COLUMN_TERMS_ALLOCATED.with(|c| c.get()),
            before_ids2_allocs + 1
        );
        assert_eq!(
            super::super::query_memory::LEDGER_OPS.with(|n| n.get()),
            before_ids2_ops,
            "second ID-only tight-budget cold call must not repeat failed ledger growth"
        );
        ids_only.release_cursor_work();
        assert_eq!(ids_only.live_bytes(), 0);

        // Under a normal cursor budget, the first cold batch admits its scratch once on
        // the decode envelope; subsequent cold batches of the same size take 0 ledger
        // operations, warm batches skip allocation entirely, and `release_cursor_work`
        // frees the envelope.
        let mut ctx = Ctx::new(snap);
        ctx.mem_limit = 1 << 20;
        ctx.enable_cursor_accounting().unwrap();
        let before_cold1 = PREFETCH_COLUMN_TERMS_ALLOCATED.with(|c| c.get());
        prefetch_column_terms(&ctx, &batch1, &[0]);
        assert_eq!(
            PREFETCH_COLUMN_TERMS_ALLOCATED.with(|c| c.get()),
            before_cold1 + 1
        );
        assert!(ctx.live_bytes() >= ids_bytes);

        let before_cold2_allocs = PREFETCH_COLUMN_TERMS_ALLOCATED.with(|c| c.get());
        let before_cold2_ops = super::super::query_memory::LEDGER_OPS.with(|n| n.get());
        prefetch_column_terms(&ctx, &batch2, &[0]);
        assert_eq!(
            PREFETCH_COLUMN_TERMS_ALLOCATED.with(|c| c.get()),
            before_cold2_allocs + 1
        );
        assert_eq!(
            super::super::query_memory::LEDGER_OPS.with(|n| n.get()),
            before_cold2_ops,
            "second cold cursor batch reuses the decoding envelope with zero ledger ops"
        );

        if crate::index::io_hints() {
            let before_warm_allocs = PREFETCH_COLUMN_TERMS_ALLOCATED.with(|c| c.get());
            let before_warm_ops = super::super::query_memory::LEDGER_OPS.with(|n| n.get());
            prefetch_column_terms(&ctx, &batch1, &[0]);
            assert_eq!(
                PREFETCH_COLUMN_TERMS_ALLOCATED.with(|c| c.get()),
                before_warm_allocs
            );
            assert_eq!(
                super::super::query_memory::LEDGER_OPS.with(|n| n.get()),
                before_warm_ops
            );
        }

        ctx.release_cursor_work();
        assert_eq!(ctx.live_bytes(), 0);
        assert!(ctx.check().is_ok());
    }

    #[test]
    fn prefetch_column_terms_optional_reservation_reclaimed_by_required_cursor_work() {
        use crate::io::Source;
        use oxrdfio::RdfFormat;
        use std::sync::Arc;

        let dir = tempfile::tempdir().unwrap();
        let disk = Store::open(dir.path(), StoreOptions::default()).unwrap();
        let mut data = String::from("@prefix ex: <http://ex.org/> .\n");
        for i in 0..16 {
            data.push_str(&format!("ex:s{i} ex:p \"short {i:02}\" .\n"));
        }
        disk.load(&[Source::from_bytes(
            data.as_bytes().to_vec(),
            RdfFormat::Turtle,
            None,
        )])
        .unwrap();
        let snap = disk.snapshot();
        let id0 = snap
            .lookup_term(&oxrdf::Literal::new_simple_literal("short 00").into())
            .unwrap();
        let id1 = snap
            .lookup_term(&oxrdf::Literal::new_simple_literal("short 01").into())
            .unwrap();

        // 1,024 cells containing two distinct short strings reserve an upper bound of
        // `1024 * (size_of::<u64>() + size_of::<usize>())` bytes (16 KiB on 64-bit).
        let mut big_batch = Table::new(vec![0]);
        for i in 0..1024 {
            big_batch.push_row(&[if i % 2 == 0 { id0 } else { id1 }]);
        }
        let ids_bytes = (1024 * size_of::<u64>()) as u64;
        let max_blocks = (1024 * size_of::<usize>()) as u64;
        let eager_total = ids_bytes + max_blocks;

        // 1) Prefetch succeeds on a cold batch, filling the entire budget, and subsequent
        // required cursor work (`ctx.charge` and `OwnedCharge::resize` as used for BIND
        // column reservations) reclaims that idle optional reservation instead of failing
        // with `BudgetExceeded`.
        let mut ctx = Ctx::new(snap.clone());
        ctx.mem_limit = eager_total;
        ctx.enable_cursor_accounting().unwrap();
        let ctx = Arc::new(ctx);
        let before_allocs = PREFETCH_COLUMN_TERMS_ALLOCATED.with(|c| c.get());
        prefetch_column_terms(&ctx, &big_batch, &[0]);
        assert_eq!(
            PREFETCH_COLUMN_TERMS_ALLOCATED.with(|c| c.get()),
            before_allocs + 1
        );
        assert_eq!(ctx.live_bytes(), eager_total);

        let required = ctx
            .charge(eager_total)
            .expect("required ctx.charge must reclaim idle optional prefetch reservation");
        assert_eq!(ctx.live_bytes(), eager_total);
        drop(required);
        assert_eq!(ctx.live_bytes(), 0);

        // Fresh cold store so prefetch runs again before an `OwnedCharge::resize` (the
        // cursor's fallible BIND column reservation path).
        let dir2 = tempfile::tempdir().unwrap();
        let disk2 = Store::open(dir2.path(), StoreOptions::default()).unwrap();
        disk2
            .load(&[Source::from_bytes(
                data.as_bytes().to_vec(),
                RdfFormat::Turtle,
                None,
            )])
            .unwrap();
        let snap2 = disk2.snapshot();
        let id2_0 = snap2
            .lookup_term(&oxrdf::Literal::new_simple_literal("short 00").into())
            .unwrap();
        let id2_1 = snap2
            .lookup_term(&oxrdf::Literal::new_simple_literal("short 01").into())
            .unwrap();
        let mut big_batch2 = Table::new(vec![0]);
        for i in 0..1024 {
            big_batch2.push_row(&[if i % 2 == 0 { id2_0 } else { id2_1 }]);
        }
        let mut small_batch2 = Table::new(vec![0]);
        for i in 0..32 {
            small_batch2.push_row(&[if i % 2 == 0 { id2_0 } else { id2_1 }]);
        }

        let mut ctx2 = Ctx::new(snap2);
        ctx2.mem_limit = eager_total;
        ctx2.enable_cursor_accounting().unwrap();
        let ctx2 = Arc::new(ctx2);

        // Pay a non-reclaimable decode envelope first by decoding one short term.
        assert!(ctx2.term(id2_0).is_some());
        let decode_peak = ctx2.live_bytes();
        assert!(decode_peak > 0 && decode_peak < eager_total);

        // A small cold batch that fits inside the paid decode envelope reuses it with
        // zero ledger ops and zero extra bytes.
        let before_reuse_ops = super::super::query_memory::LEDGER_OPS.with(|n| n.get());
        prefetch_column_terms(&ctx2, &small_batch2, &[0]);
        assert_eq!(
            super::super::query_memory::LEDGER_OPS.with(|n| n.get()),
            before_reuse_ops,
            "prefetch within paid decode envelope must take zero ledger ops"
        );
        assert_eq!(ctx2.live_bytes(), decode_peak);

        // A larger cold batch grows the optional prefetch reservation above `decode_peak`.
        let dir3 = tempfile::tempdir().unwrap();
        let disk3 = Store::open(dir3.path(), StoreOptions::default()).unwrap();
        disk3
            .load(&[Source::from_bytes(
                data.into_bytes(),
                RdfFormat::Turtle,
                None,
            )])
            .unwrap();
        let snap3 = disk3.snapshot();
        let id3_0 = snap3
            .lookup_term(&oxrdf::Literal::new_simple_literal("short 00").into())
            .unwrap();
        let id3_1 = snap3
            .lookup_term(&oxrdf::Literal::new_simple_literal("short 01").into())
            .unwrap();
        let mut big_batch3 = Table::new(vec![0]);
        for i in 0..1024 {
            big_batch3.push_row(&[if i % 2 == 0 { id3_0 } else { id3_1 }]);
        }
        let mut ctx3 = Ctx::new(snap3);
        ctx3.mem_limit = eager_total;
        ctx3.enable_cursor_accounting().unwrap();
        let ctx3 = Arc::new(ctx3);
        assert!(ctx3.term(id3_0).is_some());
        let decode_peak3 = ctx3.live_bytes();
        assert!(decode_peak3 > 0 && decode_peak3 < eager_total);

        let before_grow_allocs = PREFETCH_COLUMN_TERMS_ALLOCATED.with(|c| c.get());
        prefetch_column_terms(&ctx3, &big_batch3, &[0]);
        assert_eq!(
            PREFETCH_COLUMN_TERMS_ALLOCATED.with(|c| c.get()),
            before_grow_allocs + 1
        );
        assert_eq!(ctx3.live_bytes(), eager_total);

        // Required `OwnedCharge::resize` (as in cursor BIND column reservation) reclaims
        // the extra optional prefetch bytes above `decode_peak3`, while preserving the
        // non-reclaimable `decode_peak3` envelope.
        let mut bind_charge = super::super::ctx::OwnedCharge::new(&ctx3, 0).unwrap();
        let headroom = eager_total - decode_peak3;
        bind_charge
            .resize(headroom)
            .expect("OwnedCharge::resize must reclaim idle optional prefetch excess");
        assert_eq!(ctx3.live_bytes(), eager_total);
        assert!(
            ctx3.charge(1).is_err(),
            "non-reclaimable decode envelope must stay accounted"
        );
        drop(bind_charge);
        assert_eq!(ctx3.live_bytes(), decode_peak3);
        ctx3.release_cursor_work();
        assert_eq!(ctx3.live_bytes(), 0);

        // Unused bindings from the reuse check above.
        drop(big_batch2);
        ctx2.release_cursor_work();
        assert_eq!(ctx2.live_bytes(), 0);
    }

    #[test]
    fn eager_and_nested_limited_slice_keep_prepaid_columns_without_ledger() {
        let store = scan_fixture();
        {
            let mut ctx = Ctx::new(store.snapshot());
            ctx.use_cache = false;
            let spec = default_subject_scan(&ctx);
            let child = Node::leaf(Kind::Scan(spec), vec![0], 3.0, "slice source".into());
            let mut node = Node::leaf(
                Kind::Slice {
                    offset: 1,
                    limit: Some(1),
                    whole: false,
                },
                vec![0],
                1.0,
                "slice".into(),
            );
            node.children.push(child);
            let before = super::super::query_memory::LEDGER_OPS.with(|n| n.get());
            let output = execute_materialized(&ctx, &node).unwrap().0;
            assert_eq!(output.table.len, 1);
            assert_eq!(output.table.cols[0].capacity(), 2);
            assert_eq!(
                ctx.live_bytes(),
                super::super::algosvc::table_capacity(&output.table).unwrap()
            );
            assert_eq!(
                super::super::query_memory::LEDGER_OPS.with(|n| n.get()),
                before
            );
            drop(output);
            assert_eq!(ctx.live_bytes(), 0);
        }
    }

    #[test]
    fn review_f1_slice_adopts_one_actual_scan_backing() {
        let store = scan_fixture();
        let mut ctx = Ctx::new(store.snapshot());
        ctx.use_cache = false;
        ctx.mem_limit = 200;
        let spec = default_subject_scan(&ctx);
        let (direct, _) = scan_limited(&ctx, &spec, &[0], &[0], Some(1)).unwrap();
        assert_eq!(ctx.live_bytes(), 168);
        let expected = direct.table.cols[0][0];
        drop(direct);
        assert_eq!(ctx.live_bytes(), 0);
        let before = super::super::query_memory::LEDGER_OPS.with(|n| n.get());
        let mut child = Node::leaf(Kind::Scan(spec), vec![0], 3.0, "review scan".into());
        child.sorted = vec![0];
        let mut node = Node::leaf(
            Kind::Slice {
                offset: 0,
                limit: Some(1),
                whole: false,
            },
            vec![0],
            1.0,
            "review slice".into(),
        );
        node.children.push(child);
        let (output, _) = execute_materialized(&ctx, &node).unwrap();
        assert_eq!(output.table.cols, [vec![expected]]);
        assert_eq!(
            ctx.live_bytes(),
            168,
            "one physical scan must keep exactly its one prepaid owner"
        );
        assert_eq!(
            super::super::query_memory::LEDGER_OPS.with(|n| n.get()),
            before
        );
        drop(output);
        assert_eq!(ctx.live_bytes(), 0);
    }

    #[test]
    fn review_f2_prepared_values_limit_and_ask_keep_ordinary_eager_execution_fast() {
        let store = Store::in_memory(StoreOptions::default());
        for query in [
            "SELECT ?value WHERE { VALUES ?value { UNDEF } } LIMIT 1",
            "ASK WHERE { VALUES ?value { UNDEF } }",
            "SELECT ?value WHERE { VALUES ?value { 1 } FILTER EXISTS { VALUES ?value { 1 } } }",
        ] {
            let options = super::super::QueryOptions {
                no_cache: true,
                ..Default::default()
            };
            let before = super::super::query_memory::LEDGER_OPS.with(|n| n.get());
            let result = super::super::query(store.snapshot(), query, &options).unwrap();
            assert_eq!(result.len(), 1);
            if result.kind == super::super::QueryKind::Ask {
                assert!(result.boolean);
            }
            assert_eq!(
                super::super::query_memory::LEDGER_OPS.with(|n| n.get()),
                before,
                "{query}"
            );
            drop(result);
        }
    }

    #[test]
    fn review_f2_raw_application_facade_does_not_promote_before_returning_unowned_table() {
        let store = Store::in_memory(StoreOptions::default());
        let mut ctx = Ctx::new(store.snapshot());
        ctx.use_cache = false;
        ctx.var("value");
        let table = Table {
            vars: vec![0],
            cols: vec![vec![Id::UNDEF]],
            len: 1,
            sorted: Vec::new(),
        };
        let node = Node::leaf(Kind::Values(table), vec![0], 1.0, "raw facade".into());
        let ctx = std::sync::Arc::new(ctx);
        let before = super::super::query_memory::LEDGER_OPS.with(|n| n.get());
        let (table, _) = execute(&ctx, &node).unwrap();
        assert_eq!(table.cols, [vec![Id::UNDEF]]);
        assert_eq!(
            super::super::query_memory::LEDGER_OPS.with(|n| n.get()),
            before
        );
        // This explicitly raw application facade is not the retained engine API.
        assert_eq!(ctx.live_bytes(), 0);
    }

    #[test]
    fn review_f2_update_where_values_consumes_borrowed_backing_without_promotion() {
        let store = Store::in_memory(StoreOptions::default());
        let before = super::super::query_memory::LEDGER_OPS.with(|n| n.get());
        super::super::update::update(
            &store,
            "INSERT { <urn:out> <urn:p> ?value } WHERE { VALUES ?value { 1 } }",
            &Default::default(),
        )
        .unwrap();
        assert_eq!(store.snapshot().len(), 1);
        assert_eq!(
            super::super::query_memory::LEDGER_OPS.with(|n| n.get()),
            before
        );
    }

    #[test]
    fn values_backing_keeps_its_actual_credit_until_drop() {
        let store = Store::in_memory(StoreOptions::default());
        let mut ctx = Ctx::new(store.snapshot());
        ctx.use_cache = false;
        ctx.var("value");
        let table = Table {
            vars: vec![0],
            cols: vec![vec![Id::UNDEF]],
            len: 1,
            sorted: Vec::new(),
        };
        let expected = super::super::algosvc::table_capacity(&table).unwrap();
        ctx.mem_limit = expected;
        let node = Node::leaf(Kind::Values(table), vec![0], 1.0, "paid values".into());
        let (output, _) = execute_owned_inner(&ctx, &node, false).unwrap();
        assert_eq!(output.table.cols, [vec![Id::UNDEF]]);
        assert_eq!(
            ctx.live_bytes(),
            expected,
            "values backing must retain its actual prepaid credit"
        );
        drop(output);
        assert_eq!(ctx.live_bytes(), 0);
    }

    fn scan_fixture() -> Store {
        let store = Store::in_memory(StoreOptions::default());
        store.load(&[crate::io::Source::from_bytes(
            b"<urn:s0> <urn:p> 1 . <urn:s1> <urn:p> 2 . <urn:s2> <urn:p> 3 . <urn:g> { <urn:s0> <urn:p> 1 . }".to_vec(),
            crate::io::RdfFormat::TriG, None,
        )]).unwrap();
        store
    }

    fn default_subject_scan(ctx: &Ctx) -> ScanSpec {
        ScanSpec {
            perm: Perm::Gspo,
            prefix: vec![Id::DEFAULT_GRAPH.0],
            cols: vec![(1, ctx.var("subject"))],
            eqs: Vec::new(),
            graph: GraphFilter::Default,
            graph_col: 0,
            dedup: false,
            src: None,
        }
    }

    #[test]
    fn actual_full_and_limited_scan_retain_prepaid_spare_capacity() {
        let store = scan_fixture();
        for limit in [None, Some(1)] {
            let mut ctx = Ctx::new(store.snapshot());
            let spec = default_subject_scan(&ctx);
            ctx.mem_limit = if limit.is_some() { 168 } else { 184 };
            let (output, truncated) = scan_limited(&ctx, &spec, &[0], &[0], limit).unwrap();
            assert_eq!(output.table.len, limit.unwrap_or(3));
            assert_eq!(output.table.cols[0].capacity(), limit.unwrap_or(3));
            assert_eq!(output.table.sorted, [0]);
            assert_eq!(truncated, limit.is_some());
            assert_eq!(ctx.live_bytes(), ctx.mem_limit);
            drop(output);
            assert_eq!(ctx.live_bytes(), 0);
        }
    }

    #[test]
    fn scan_metadata_refusal_allocation_failure_and_cancel_precede_growth() {
        use crate::algo::memory::probe;
        use std::{cell::Cell, rc::Rc};
        let store = scan_fixture();
        for reason in ["budget", "allocation", "cancel"] {
            let mut ctx = Ctx::new(store.snapshot());
            let spec = default_subject_scan(&ctx);
            if reason == "budget" {
                ctx.mem_limit = 159;
            }
            if reason == "cancel" {
                ctx.cancel.store(true, std::sync::atomic::Ordering::Release);
            }
            let seen = Rc::new(Cell::new(0));
            let observed = seen.clone();
            let hook = probe::install(
                move |point| {
                    if let probe::Point::Allocation(name, _) = point
                        && name.starts_with("eager-")
                    {
                        observed.set(observed.get() + 1);
                    }
                },
                (reason == "allocation").then_some("eager-vars"),
            );
            let result = scan_limited(&ctx, &spec, &[0], &[0], None);
            match reason {
                "budget" => assert!(matches!(result, Err(Error::BudgetExceeded(_)))),
                "cancel" => assert!(matches!(result, Err(Error::Cancelled))),
                _ => assert!(matches!(result, Err(Error::Invalid(_)))),
            }
            assert_eq!(seen.get(), usize::from(reason == "allocation"));
            drop(hook);
            assert_eq!(ctx.live_bytes(), 0);
        }
    }

    #[test]
    fn paid_scan_dedup_keeps_actual_reserved_columns_until_drop() {
        let store = scan_fixture();
        let ctx = Ctx::new(store.snapshot());
        let subject = ctx.var("subject");
        let object = ctx.var("object");
        let predicate = ctx.intern_term(&oxrdf::NamedNode::new("urn:p").unwrap().into());
        let spec = ScanSpec {
            perm: Perm::Pso,
            prefix: vec![predicate.0],
            cols: vec![(1, subject), (2, object)],
            eqs: Vec::new(),
            graph: GraphFilter::All,
            graph_col: 3,
            dedup: true,
            src: None,
        };
        let before = ctx.live_bytes();
        let (output, _) =
            scan_limited(&ctx, &spec, &[subject, object], &[subject, object], None).unwrap();
        assert_eq!(output.table.len, 3);
        assert_eq!(output.table.cols[0].capacity(), 4);
        assert_eq!(
            ctx.live_bytes() - before,
            super::super::algosvc::table_capacity(&output.table).unwrap()
        );
        drop(output);
        assert_eq!(ctx.live_bytes(), before);
    }

    #[test]
    fn exact_and_filtered_ranges_keep_destination_and_partial_owners() {
        use super::super::plan::IdRange;
        let store = scan_fixture();
        for exact in [false, true] {
            let ctx = Ctx::new(store.snapshot());
            let object = ctx.var("object");
            let subject = ctx.var("subject");
            let predicate = ctx.intern_term(&oxrdf::NamedNode::new("urn:p").unwrap().into());
            let spec = ScanSpec {
                perm: Perm::Pos,
                prefix: vec![predicate.0],
                cols: vec![(1, object), (2, subject)],
                eqs: Vec::new(),
                graph: GraphFilter::Default,
                graph_col: 3,
                dedup: false,
                src: None,
            };
            let range = RangeSpec {
                var: object,
                ranges: vec![IdRange {
                    lo: 0,
                    hi: u64::MAX,
                    exact,
                }],
                filter: Vec::new(),
            };
            let before = ctx.live_bytes();
            let output =
                range_scan(&ctx, &spec, &range, &[object, subject], &[object, subject]).unwrap();
            assert_eq!(output.table.len, 3);
            assert!(output.table.cols[0].windows(2).all(|w| w[0] < w[1]));
            assert_eq!(
                ctx.live_bytes() - before,
                super::super::algosvc::table_capacity(&output.table).unwrap()
            );
            drop(output);
            assert_eq!(ctx.live_bytes(), before);
        }
    }

    #[test]
    fn identity_projection_keeps_actual_fast_credit_without_reallocation() {
        let store = Store::in_memory(StoreOptions::default());
        let mut ctx = Ctx::new(store.snapshot());
        ctx.use_cache = false;
        ctx.var("value");
        let table = Table {
            vars: vec![0],
            cols: vec![vec![Id::UNDEF]],
            len: 1,
            sorted: Vec::new(),
        };
        let expected = super::super::algosvc::table_capacity(&table).unwrap();
        ctx.mem_limit = expected;
        let child = Node::leaf(Kind::Values(table), vec![0], 1.0, "paid values".into());
        let mut node = Node::leaf(
            Kind::Project(vec![0]),
            vec![0],
            1.0,
            "paid projection".into(),
        );
        node.children.push(child);
        let (output, _) = execute_owned_inner(&ctx, &node, false).unwrap();
        assert_eq!(output.table.cols, [vec![Id::UNDEF]]);
        assert_eq!(
            ctx.live_bytes(),
            expected,
            "identity projection must retain its actual child credit"
        );
        drop(output);
        assert_eq!(ctx.live_bytes(), 0);
    }
}

#[cfg(test)]
mod string_order_tests {
    use super::*;
    use crate::error::BudgetKind;
    use crate::id::Tag;
    use crate::io::Source;
    use crate::store::{Store, StoreOptions};
    use oxrdfio::RdfFormat;

    /// Words over a small alphabet, so that many are prefixes of others.
    fn words() -> Vec<String> {
        let mut out = vec![String::new()];
        for len in 1..=4 {
            let mut next = Vec::new();
            for w in out.iter().filter(|w| w.chars().count() == len - 1) {
                for c in ['a', 'b', 'é'] {
                    next.push(format!("{w}{c}"));
                }
            }
            out.extend(next);
        }
        out
    }

    /// A store of the words, a few other terms and one string longer than the budget of
    /// the refusal case, with the ids of each in that order.
    fn store(long: &str) -> (Store, Vec<Id>) {
        let words = words();
        let mut nt = String::new();
        for (i, w) in words.iter().enumerate() {
            nt.push_str(&format!("<urn:s:{i}> <urn:p> \"{w}\" .\n"));
        }
        let extra = [
            "\"5\"^^<http://www.w3.org/2001/XMLSchema#integer>".to_string(),
            "\"ab\"@en".to_string(),
            "<urn:iri>".to_string(),
            "\"a\"^^<urn:datatype>".to_string(),
            format!("\"{long}\""),
        ];
        for (k, v) in extra.iter().enumerate() {
            nt.push_str(&format!("<urn:z:{k}> <urn:p> {v} .\n"));
        }
        let s = Store::in_memory(StoreOptions::default());
        s.load(&[Source::from_bytes(
            nt.into_bytes(),
            RdfFormat::NTriples,
            None,
        )])
        .unwrap();
        let snap = s.snapshot();
        let mut ids: Vec<Id> = words
            .iter()
            .map(|w| {
                snap.lookup_term(&oxrdf::Literal::new_simple_literal(w).into())
                    .unwrap()
            })
            .collect();
        ids.push(
            snap.lookup_term(
                &oxrdf::Literal::new_typed_literal("5", oxrdf::vocab::xsd::INTEGER).into(),
            )
            .unwrap(),
        );
        ids.push(
            snap.lookup_term(
                &oxrdf::Literal::new_language_tagged_literal("ab", "en")
                    .unwrap()
                    .into(),
            )
            .unwrap(),
        );
        ids.push(
            snap.lookup_term(&oxrdf::NamedNode::new("urn:iri").unwrap().into())
                .unwrap(),
        );
        ids.push(
            snap.lookup_term(
                &oxrdf::Literal::new_typed_literal(
                    "a",
                    oxrdf::NamedNode::new("urn:datatype").unwrap(),
                )
                .into(),
            )
            .unwrap(),
        );
        ids.push(
            snap.lookup_term(&oxrdf::Literal::new_simple_literal(long).into())
                .unwrap(),
        );
        assert!(
            ids[..words.len()]
                .iter()
                .chain(ids.last())
                .all(|id| id.tag() == Tag::Vocab)
        );
        drop(snap);
        (s, ids)
    }

    /// The product of `ids` with itself, strings first, as two columns, and a local
    /// string in one row.
    fn product(ctx: &Ctx, ids: &[Id]) -> (Table, VarId, VarId) {
        let (x, y) = (ctx.var("x"), ctx.var("y"));
        let local = ctx.intern_value(&Value::Str("local".into()));
        let mut t = Table::new(vec![x, y]);
        for &a in ids {
            for &b in ids {
                t.push_row(&[a, b]);
            }
        }
        t.push_row(&[local, ids[1]]);
        t.push_row(&[Id::UNDEF, ids[1]]);
        t.push_row(&[ids[1], Id::UNDEF]);
        (t, x, y)
    }

    fn cmp(x: VarId, y: VarId, op: super::super::expr::CmpOp) -> Expr {
        Expr::Cmp(Box::new(Expr::Var(x)), Box::new(Expr::Var(y)), op)
    }

    /// A cursor's batch takes the comparison through the vocabulary floors as an eager
    /// table does, with the same mask as the generic filter that decodes every row.
    #[test]
    fn cursor_batches_order_strings_through_their_floors_as_eager_tables_do() {
        use super::super::expr::CmpOp;
        let (s, ids) = store("long");
        assert!(ids.len() * ids.len() > STRING_ORDER_ROWS);
        for op in [CmpOp::Lt, CmpOp::Le, CmpOp::Gt, CmpOp::Ge] {
            let mut masks = Vec::new();
            for cursor in [false, true] {
                let mut ctx = Ctx::new(s.snapshot());
                ctx.mem_limit = 64 << 20;
                if cursor {
                    ctx.enable_cursor_accounting().unwrap();
                }
                let (t, x, y) = product(&ctx, &ids);
                let e = cmp(x, y, op);
                let mask = string_order_mask(&ctx, &t, (x, y), op, &e)
                    .unwrap()
                    .unwrap_or_else(|| panic!("cursor {cursor}: the floors apply"));
                let generic = evaluate_mask(&ctx, &t, std::slice::from_ref(&e), None).unwrap();
                assert_eq!(mask, generic, "cursor {cursor} {op:?}");
                masks.push(mask);
            }
            assert_eq!(masks[0], masks[1], "{op:?}");
        }
    }

    #[test]
    fn floor_masks_match_direct_comparison_at_low_reuse() {
        use super::super::expr::CmpOp;
        let rows = STRING_ORDER_ROWS;
        let (s, ids) = word_store(2 * rows);
        for cursor in [false, true] {
            for distinct_pairs in [rows - rows / 8, rows] {
                let mut ctx = Ctx::new(s.snapshot());
                ctx.mem_limit = 64 << 20;
                if cursor {
                    ctx.enable_cursor_accounting().unwrap();
                }
                let (x, y) = (ctx.var("x"), ctx.var("y"));
                let mut t = Table::new(vec![x, y]);
                for i in 0..rows {
                    let pair = i % distinct_pairs;
                    t.push_row(&ids[2 * pair..2 * pair + 2]);
                }
                let e = cmp(x, y, CmpOp::Lt);
                let mask = string_order_mask(&ctx, &t, (x, y), CmpOp::Lt, &e)
                    .unwrap()
                    .expect("the floors apply");
                let direct = evaluate_mask(&ctx, &t, &[e], None).unwrap();
                assert_eq!(
                    mask, direct,
                    "cursor {cursor}, {distinct_pairs} distinct pairs"
                );
                assert!(ctx.check().is_ok());
            }
        }
    }

    #[test]
    fn a_cursor_floor_mask_polls_cancellation_after_decoding_starts() {
        use super::super::expr::CmpOp;
        let (s, ids) = store("long");
        let mut ctx = Ctx::new(s.snapshot());
        ctx.mem_limit = 64 << 20;
        ctx.enable_cursor_accounting().unwrap();
        let (t, x, y) = product(&ctx, &ids);
        let e = cmp(x, y, CmpOp::Lt);
        let cancel = ctx.cancel.clone();
        crate::algo::set_projection_observer(Some(std::sync::Arc::new(move |event| {
            if let crate::algo::ProjectionEvent::Materialized(
                "checked-vocab-scratch-allocated",
                _,
            ) = event
            {
                cancel.store(true, AtomicOrdering::Relaxed);
            }
        })));
        let result = string_order_mask(&ctx, &t, (x, y), CmpOp::Lt, &e);
        crate::algo::set_projection_observer(None);
        assert!(matches!(result, Err(Error::Cancelled)));
    }

    /// The floor of every word is the first id whose key starts with its lexical form, in
    /// both modes, over words that span many front-coded blocks so that a string often
    /// starts a block or follows one of its extensions.
    #[test]
    fn string_floors_match_a_search_across_blocks() {
        let (s, ids) = store("long");
        let snap = s.snapshot();
        let v = &snap.generation.vocab;
        let all: Vec<u64> = (0..v.first_triple()).collect();
        assert!(all.len() > 4 * crate::vocab::FC_BLOCK);
        let expected: Vec<Option<u64>> = ids
            .iter()
            .map(|id| {
                if id.tag() != Tag::Vocab || id.payload() >= v.first_triple() {
                    return None;
                }
                let key = v.get(id.payload()).unwrap();
                if key.len() < 2 || key[0] != b'"' || key[key.len() - 1] != crate::id::KEY_SEP {
                    return None;
                }
                let prefix = &key[..key.len() - 1];
                all.iter()
                    .copied()
                    .find(|&q| v.get(q).unwrap().starts_with(prefix))
            })
            .collect();
        assert!(
            expected
                .iter()
                .zip(&ids)
                .filter(|(f, id)| f.is_some_and(|f| f < id.payload()))
                .any(|(_, id)| (id.payload() as usize).is_multiple_of(crate::vocab::FC_BLOCK)),
            "a block's first string extends another"
        );
        for cursor in [false, true] {
            let mut ctx = Ctx::new(s.snapshot());
            ctx.mem_limit = 64 << 20;
            if cursor {
                ctx.enable_cursor_accounting().unwrap();
            }
            let floors: Vec<Option<u64>> = ids.iter().map(|&id| ctx.string_floor(id)).collect();
            assert_eq!(floors, expected, "cursor {cursor}");
            let mut map = ctx.floor_map();
            let cached: Vec<Option<u64>> = ids.iter().map(|&id| map.floor(id)).collect();
            assert_eq!(cached, expected, "cursor {cursor}");
            drop(map);
            assert!(ctx.check().is_ok(), "cursor {cursor}");
        }
    }

    /// The floors a query computes for one table serve its later tables, as a cursor's
    /// batches are, in both modes and on one thread or several: a second mask over the
    /// same strings computes none, since every worker reads and writes one table.
    #[test]
    fn the_floor_cache_lasts_the_query() {
        let (s, ids) = store("long");
        for threads in [1, 4] {
            let pool = rayon::ThreadPoolBuilder::new()
                .num_threads(threads)
                .build()
                .unwrap();
            pool.install(|| the_floor_cache_lasts_the_query_in(&s, &ids));
        }
    }

    fn the_floor_cache_lasts_the_query_in(s: &Store, ids: &[Id]) {
        use super::super::expr::CmpOp;
        for cursor in [false, true] {
            let mut ctx = Ctx::new(s.snapshot());
            ctx.mem_limit = 64 << 20;
            if cursor {
                ctx.enable_cursor_accounting().unwrap();
            }
            let mut masks = Vec::new();
            let mut computed = Vec::new();
            for op in [CmpOp::Lt, CmpOp::Ge] {
                let (t, x, y) = product(&ctx, ids);
                let e = cmp(x, y, op);
                let mask = string_order_mask(&ctx, &t, (x, y), op, &e)
                    .unwrap()
                    .expect("the floors apply");
                let generic = evaluate_mask(&ctx, &t, std::slice::from_ref(&e), None).unwrap();
                assert_eq!(mask, generic, "cursor {cursor} {op:?}");
                masks.push(mask);
                computed.push(ctx.floor_cache().computed());
            }
            let cache = ctx.floor_cache();
            // every base literal at least once, twice when two workers compute it at once
            let v = &ctx.snap.generation.vocab;
            let literals = ids
                .iter()
                .filter(|id| id.tag() == Tag::Vocab && id.payload() < v.first_triple())
                .count();
            assert!(literals > 0, "cursor {cursor}");
            assert!(computed[0] >= literals, "cursor {cursor}: {computed:?}");
            assert_eq!(computed[1], computed[0], "cursor {cursor}: no floor again");
            assert!(cache.entries() >= literals, "cursor {cursor}");
            assert!(
                cache.reserved() > 0,
                "cursor {cursor}: the maps are charged"
            );
            assert!(ctx.check().is_ok(), "cursor {cursor}");
        }
    }

    /// A cursor's batch of at most `PAR_THRESHOLD` rows spreads its floor comparison over
    /// the pool as the generic filter's batch does, instead of leaving it on the calling
    /// thread, so that more than one thread looks up floors. The pool takes the batch
    /// when its timed sample projects at least `BATCH_PAR_NANOS` for the rest, which the
    /// sample's floors exceed many times over, and the test allows several attempts.
    #[test]
    fn a_cursor_batch_compares_floors_on_the_pool() {
        use super::super::expr::CmpOp;
        let (s, ids) = store("long");
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(4)
            .build()
            .unwrap();
        let mut spread = false;
        for _ in 0..20 {
            let mut ctx = Ctx::new(s.snapshot());
            ctx.mem_limit = 64 << 20;
            ctx.enable_cursor_accounting().unwrap();
            let (t, x, y) = product(&ctx, &ids);
            assert!(t.len() <= PAR_THRESHOLD && batch_may_split(t.len()));
            let e = cmp(x, y, CmpOp::Lt);
            let mask = pool
                .install(|| string_order_mask(&ctx, &t, (x, y), CmpOp::Lt, &e))
                .unwrap()
                .expect("the floors apply");
            let generic = evaluate_mask(&ctx, &t, std::slice::from_ref(&e), None).unwrap();
            assert_eq!(mask, generic);
            assert!(ctx.check().is_ok());
            if ctx.floor_cache().threads() > 1 {
                spread = true;
                break;
            }
        }
        assert!(spread, "a cursor batch compared its floors on one thread");
    }

    /// A floor cache whose growth the budget refuses takes no entries, records no
    /// failure and leaves the mask exact, in both modes: the floors it cannot keep are
    /// computed each time.
    #[test]
    fn a_refused_floor_cache_growth_leaves_the_mask_exact() {
        use super::super::expr::CmpOp;
        let (s, ids) = store("long");
        for cursor in [false, true] {
            let mut ctx = Ctx::new(s.snapshot());
            ctx.mem_limit = 1 << 20;
            if cursor {
                ctx.enable_cursor_accounting().unwrap();
            }
            let (t, x, y) = product(&ctx, &ids);
            let e = cmp(x, y, CmpOp::Lt);
            let generic = evaluate_mask(&ctx, &t, std::slice::from_ref(&e), None).unwrap();
            // room for the decoding of the short keys, not for the reservation step of
            // 16 KiB that the first table's charge takes
            let room = 12 << 10;
            let held = ctx.charge(ctx.mem_limit - ctx.live_bytes() - room).unwrap();
            let mask = string_order_mask(&ctx, &t, (x, y), CmpOp::Lt, &e)
                .unwrap()
                .expect("the floors apply");
            assert_eq!(mask, generic, "cursor {cursor}");
            let cache = ctx.floor_cache();
            assert_eq!(cache.entries(), 0, "cursor {cursor}");
            assert_eq!(cache.reserved(), 0, "cursor {cursor}");
            assert!(
                cache.computed() > t.len(),
                "cursor {cursor}: computed each time"
            );
            assert!(ctx.check().is_ok(), "cursor {cursor}");
            drop(held);
        }
    }

    /// A streamed `FILTER(?x < ?y)` over a product of stored strings reaches the floors
    /// in a batch and keeps the rows the eager query keeps.
    #[test]
    fn a_streamed_string_comparison_matches_the_eager_query() {
        use crate::sparql::{CursorOptions, QueryOptions, query, select_cursor};
        let (s, ids) = store("long");
        assert!(ids.len() * ids.len() > STRING_ORDER_ROWS);
        let q = "SELECT ?a ?b WHERE { ?a <urn:p> ?x . ?b <urn:p> ?y FILTER(?x < ?y) }";
        let options = QueryOptions {
            no_cache: true,
            ..Default::default()
        };
        let text = |t: &Option<oxrdf::Term>| t.as_ref().map(|t| t.to_string());
        let mut eager: Vec<_> = query(s.snapshot(), q, &options)
            .unwrap()
            .rows()
            .unwrap()
            .iter()
            .map(|r| r.values().iter().map(text).collect::<Vec<_>>())
            .collect();
        eager.sort();
        let before = STRING_ORDER_MASKS.load(AtomicOrdering::Relaxed);
        let cursor_options = CursorOptions {
            batch_rows: 1 << 16,
            ..Default::default()
        };
        let mut cursor = select_cursor(s.snapshot(), q, &options, &cursor_options).unwrap();
        let mut streamed = Vec::new();
        while let Some(batch) = cursor.next_batch().unwrap() {
            for row in 0..batch.len() {
                streamed.push(
                    batch
                        .row(row)
                        .unwrap()
                        .values()
                        .iter()
                        .map(text)
                        .collect::<Vec<_>>(),
                );
            }
        }
        assert!(
            STRING_ORDER_MASKS.load(AtomicOrdering::Relaxed) > before,
            "a batch reaches the floors"
        );
        streamed.sort();
        assert!(!eager.is_empty());
        assert_eq!(streamed, eager);
    }

    /// The rows of the pre-decode tests, more than `PAR_THRESHOLD`.
    const WIDE_ROWS: usize = PAR_THRESHOLD + 4096;

    /// A store of `WIDE_ROWS` distinct stored strings and the ids of its strings in order.
    fn unique_store() -> (Store, Vec<Id>) {
        let mut nt = String::new();
        for i in 0..WIDE_ROWS {
            nt.push_str(&format!("<urn:s:{i}> <urn:p> \"string number {i:06}\" .\n"));
        }
        let s = Store::in_memory(StoreOptions::default());
        s.load(&[Source::from_bytes(
            nt.into_bytes(),
            RdfFormat::NTriples,
            None,
        )])
        .unwrap();
        let snap = s.snapshot();
        let ids = (0..WIDE_ROWS)
            .map(|i| {
                let term = oxrdf::Literal::new_simple_literal(format!("string number {i:06}"));
                snap.lookup_term(&term.into()).unwrap()
            })
            .collect();
        drop(snap);
        (s, ids)
    }

    /// A table of `WIDE_ROWS` rows over `distinct` of the strings, every one distinct when
    /// it is `usize::MAX`, with an inline number and a local string among them.
    fn wide(ctx: &Ctx, strings: &[Id], distinct: usize) -> (Table, VarId) {
        let x = ctx.var("x");
        let mut t = Table::new(vec![x]);
        let local = ctx.intern_value(&Value::Str("local".into()));
        for i in 0..WIDE_ROWS {
            let id = match i % 64 {
                62 => Id::from_i64(i as i64).unwrap(),
                63 => local,
                _ => strings[i % distinct.min(strings.len())],
            };
            t.push_row(&[id]);
        }
        (t, x)
    }

    fn str_of(x: VarId) -> Expr {
        use super::super::expr::Func;
        Expr::Call(
            Func::Builtin(spargebra::algebra::Function::Str),
            vec![Expr::Var(x)],
        )
    }

    /// Whether every base-vocabulary row of `t` holds its value in `col`, as `ctx` reads
    /// it, and every other row holds none.
    fn assert_rows(ctx: &Ctx, t: &Table, col: &[Option<Value>], what: &str) {
        for (i, (&id, value)) in t.cols[0].iter().zip(col).enumerate() {
            if id.tag() == Tag::Vocab {
                assert_eq!(
                    format!("{value:?}"),
                    format!("{:?}", ctx.value(id)),
                    "{what}: row {i}"
                );
            } else {
                assert!(value.is_none(), "{what}: row {i}");
            }
        }
    }

    /// The row-aligned pre-decode decodes every base-vocabulary row to its value in both
    /// modes, with or without the `expr_cache` switch and with shared terms, which it
    /// clones, or distinct ones, which it moves, and leaves the other rows to the row
    /// path.
    #[test]
    fn the_row_aligned_pre_decode_matches_the_values_of_its_rows() {
        let (s, strings) = unique_store();
        for cursor in [false, true] {
            for expr_cache in [true, false] {
                for distinct in [7, usize::MAX] {
                    let mut ctx = Ctx::new(s.snapshot());
                    ctx.mem_limit = 256 << 20;
                    ctx.opt.expr_cache = expr_cache;
                    if cursor {
                        ctx.enable_cursor_accounting().unwrap();
                    }
                    let ctx = std::sync::Arc::new(ctx);
                    ctx.attach_owner();
                    let (t, x) = wide(&ctx, &strings, distinct);
                    let e = str_of(x);
                    let what = format!("cursor {cursor} cache {expr_cache} distinct {distinct}");
                    let decoded = decode_for(&ctx, &t, &[&e]).unwrap().expect("decoded");
                    let col = decoded.cols[0].as_ref().expect("the column is decoded");
                    assert_rows(&ctx, &t, col, &what);
                    assert!(ctx.check().is_ok(), "{what}");
                }
            }
        }
    }

    /// The pre-decode leaves alone a column that the expressions answer from ids: one
    /// compared with a constant IRI by `=`, `!=` or `IN`, one tested by `isIRI`, and an
    /// EXISTS pattern's key, alone or under `IF`, as in `SUM(IF(?p = ex:a, 1, 0))`. That
    /// decode cost an eager grouping over 300,000 IRIs four times its time (item 1098). A
    /// comparison with a constant string reads values and still decodes the column.
    #[test]
    fn the_pre_decode_skips_columns_answered_from_ids() {
        use super::super::expr::{ExistsSpec, Func};
        use spargebra::algebra::{Function, GraphPattern};
        let (s, strings) = unique_store();
        let mut ctx = Ctx::new(s.snapshot());
        ctx.mem_limit = 256 << 20;
        let ctx = std::sync::Arc::new(ctx);
        ctx.attach_owner();
        let (t, x) = wide(&ctx, &strings, usize::MAX);
        let iri = s
            .snapshot()
            .lookup_term(&oxrdf::NamedNode::new("urn:s:0").unwrap().into())
            .expect("a stored IRI");
        let var = || Box::new(Expr::Var(x));
        let int = |i: i64| Box::new(Expr::Const(Id::from_i64(i).unwrap()));
        let eq = Expr::Eq(var(), Box::new(Expr::Const(iri)));
        let exists = Expr::Exists(std::sync::Arc::new(ExistsSpec::new(
            GraphPattern::Bgp {
                patterns: Vec::new(),
            },
            super::super::plan::ActiveGraph::Default,
            vec![x],
            Vec::new(),
        )));
        let skipped = [
            ("if-eq", Expr::If(Box::new(eq.clone()), int(1), int(0))),
            ("not-eq", Expr::Not(Box::new(eq.clone()))),
            (
                "in",
                Expr::In(var(), vec![Expr::Const(iri), Expr::Const(iri)]),
            ),
            (
                "is-iri",
                Expr::Call(Func::Builtin(Function::IsIri), vec![Expr::Var(x)]),
            ),
            ("if-exists", Expr::If(Box::new(exists), int(1), int(0))),
        ];
        for (what, e) in &skipped {
            let decoded = decode_for(&ctx, &t, &[e]).unwrap();
            assert!(
                decoded.is_none_or(|d| d.cols.iter().all(Option::is_none)),
                "{what}: the column is decoded"
            );
        }
        // Each skipped expression gives on every row what it gives over the decoded column.
        let full = decode_for(&ctx, &t, &[&str_of(x)])
            .unwrap()
            .expect("decoded");
        let map = t.var_map(ctx.nvars());
        for (what, e) in skipped.iter().filter(|(what, _)| *what != "if-exists") {
            for i in 0..t.len() {
                let at = |dec| {
                    let row = Row {
                        table: &t,
                        i,
                        map: &map,
                        dec,
                        exists: None,
                    };
                    eval(e, &row, &ctx).ok().map(|v| v.into_id(&ctx))
                };
                assert_eq!(at(None), at(Some(&full.cols)), "{what}: row {i}");
            }
        }
        let string = Expr::Eq(var(), Box::new(Expr::Const(strings[1])));
        let read = [
            ("eq-string", Expr::If(Box::new(string), int(1), int(0))),
            ("str", str_of(x)),
        ];
        for (what, e) in &read {
            let decoded = decode_for(&ctx, &t, &[e]).unwrap().expect("decoded");
            let col = decoded.cols[0].as_ref().expect("the column is decoded");
            assert_rows(&ctx, &t, col, what);
        }
    }

    /// Whatever room the budget leaves, the pre-decode never fails: the column is decoded
    /// whole or left to the rows, the charges it took are given back when it drops, and
    /// the context stays usable. The room runs from too little for the slots, through the
    /// sorted decoding, to the expansion of the values to their rows.
    #[test]
    fn a_refused_pre_decode_leaves_the_column_to_the_rows() {
        let (s, strings) = unique_store();
        const LIMIT: u64 = 64 << 20;
        for cursor in [false, true] {
            let (mut declined, mut decoded) = (0, 0);
            for room in (0..=96).map(|k| (k as u64) * (128 << 10)) {
                let mut ctx = Ctx::new(s.snapshot());
                ctx.mem_limit = LIMIT;
                if cursor {
                    ctx.enable_cursor_accounting().unwrap();
                }
                let ctx = std::sync::Arc::new(ctx);
                ctx.attach_owner();
                let (t, x) = wide(&ctx, &strings, usize::MAX);
                let e = str_of(x);
                let held = ctx.charge(LIMIT - ctx.live_bytes() - room).unwrap();
                let before = ctx.live_bytes();
                let what = format!("cursor {cursor} room {room}");
                let result = decode_for(&ctx, &t, &[&e]).unwrap_or_else(|e| panic!("{what}: {e}"));
                match result.as_ref().and_then(|d| d.cols[0].as_ref()) {
                    Some(col) => {
                        decoded += 1;
                        // values read through another context, whose cache this one
                        // does not charge
                        assert_rows(&Ctx::new(s.snapshot()), &t, col, &what);
                    }
                    None => declined += 1,
                }
                drop(result);
                assert_eq!(
                    ctx.live_bytes(),
                    before,
                    "{what}: the charges are given back"
                );
                drop(held);
                assert!(ctx.check().is_ok(), "{what}");
            }
            assert!(
                declined > 0 && decoded > 0,
                "cursor {cursor}: {declined} declined, {decoded} decoded"
            );
        }
    }

    /// A column of a few long strings repeated over every row decodes its distinct values
    /// within a budget that does not hold their copies on every row. The pre-decode then
    /// declines at the expansion, with every charge given back, and decodes the column
    /// when the budget holds the copies.
    #[test]
    fn a_refused_expansion_leaves_the_column_to_the_rows() {
        let long = |i: usize| format!("{i} {}", "x".repeat(4096));
        let mut nt = String::new();
        for i in 0..8 {
            nt.push_str(&format!("<urn:s:{i}> <urn:p> \"{}\" .\n", long(i)));
        }
        let s = Store::in_memory(StoreOptions::default());
        s.load(&[Source::from_bytes(
            nt.into_bytes(),
            RdfFormat::NTriples,
            None,
        )])
        .unwrap();
        let snap = s.snapshot();
        let strings: Vec<Id> = (0..8)
            .map(|i| {
                snap.lookup_term(&oxrdf::Literal::new_simple_literal(long(i)).into())
                    .unwrap()
            })
            .collect();
        drop(snap);
        // The copies on every row take about 80 MiB, and the slots and the sorted
        // decoding a few.
        for (limit, expanded) in [(48u64 << 20, false), (512 << 20, true)] {
            for cursor in [false, true] {
                let mut ctx = Ctx::new(s.snapshot());
                ctx.mem_limit = limit;
                if cursor {
                    ctx.enable_cursor_accounting().unwrap();
                }
                let ctx = std::sync::Arc::new(ctx);
                ctx.attach_owner();
                let (t, x) = wide(&ctx, &strings, 8);
                let e = str_of(x);
                let before = ctx.live_bytes();
                let what = format!("cursor {cursor} limit {limit}");
                let result = decode_for(&ctx, &t, &[&e]).unwrap_or_else(|e| panic!("{what}: {e}"));
                let col = result.as_ref().and_then(|d| d.cols[0].as_ref());
                assert_eq!(col.is_some(), expanded, "{what}");
                if let Some(col) = col {
                    assert_rows(&Ctx::new(s.snapshot()), &t, col, &what);
                }
                drop(result);
                assert_eq!(
                    ctx.live_bytes(),
                    before,
                    "{what}: the charges are given back"
                );
                assert!(ctx.check().is_ok(), "{what}");
            }
        }
    }

    /// Both accounting modes reserve the keys their floors read, so a string longer than
    /// the budget fails the mask with a memory refusal in eager and cursor modes alike.
    #[test]
    fn a_refused_floor_fails_the_cursor_mask() {
        use super::super::expr::CmpOp;
        // It sorts after the other strings, so the sample's keys do not reach it.
        let (s, ids) = store(&"ÿ".repeat(1 << 19));
        for cursor in [false, true] {
            let mut ctx = Ctx::new(s.snapshot());
            ctx.mem_limit = 4 << 20;
            if cursor {
                ctx.enable_cursor_accounting().unwrap();
            }
            let (t, x, y) = product(&ctx, &ids);
            let e = cmp(x, y, CmpOp::Lt);
            let outcome = string_order_mask(&ctx, &t, (x, y), CmpOp::Lt, &e);
            assert!(
                matches!(&outcome, Err(Error::BudgetExceeded(b)) if b.kind == BudgetKind::Memory),
                "cursor={cursor}: {outcome:?}"
            );
        }
    }

    /// A short language-tagged string after a long string needs no floor, so its key is
    /// read without the key before it, whether that key is longer than the walk's scratch
    /// or than the room the query has left: the mask declines without a refusal in both
    /// modes, and the generic filter then compares the rows.
    #[test]
    fn a_long_string_before_a_tagged_one_does_not_fail_the_mask() {
        use super::super::expr::CmpOp;
        // a key past the scratch of the walk, and one within it that the room refuses
        for (len, room) in [(1usize << 20, None), (2048, Some(8u64 << 10))] {
            let long = "a".repeat(len);
            let nt = format!(
                "<urn:s:0> <urn:p> \"{long}\" .\n<urn:s:1> <urn:p> \"b\"@en .\n<urn:s:2> <urn:p> \"c\"@en .\n"
            );
            let s = Store::in_memory(StoreOptions::default());
            s.load(&[Source::from_bytes(
                nt.into_bytes(),
                RdfFormat::NTriples,
                None,
            )])
            .unwrap();
            let snap = s.snapshot();
            let tagged = |l: &str| {
                snap.lookup_term(
                    &oxrdf::Literal::new_language_tagged_literal(l, "en")
                        .unwrap()
                        .into(),
                )
                .unwrap()
            };
            let (b, c) = (tagged("b"), tagged("c"));
            let first = snap
                .lookup_term(&oxrdf::Literal::new_simple_literal(&long).into())
                .unwrap();
            // the long string is the key before "b"@en, in the same block
            assert_eq!(first.payload() + 1, b.payload());
            assert!(!(b.payload() as usize).is_multiple_of(crate::vocab::FC_BLOCK));
            for cursor in [false, true] {
                let mut ctx = Ctx::new(s.snapshot());
                ctx.mem_limit = 4 << 20;
                if cursor {
                    ctx.enable_cursor_accounting().unwrap();
                }
                let (x, y) = (ctx.var("x"), ctx.var("y"));
                let mut t = Table::new(vec![x, y]);
                for _ in 0..STRING_ORDER_ROWS {
                    t.push_row(&[b, c]);
                }
                let held =
                    room.map(|room| ctx.charge(ctx.mem_limit - ctx.live_bytes() - room).unwrap());
                let e = cmp(x, y, CmpOp::Lt);
                let outcome = string_order_mask(&ctx, &t, (x, y), CmpOp::Lt, &e);
                assert!(
                    matches!(outcome, Ok(None)),
                    "{len} cursor {cursor}: {outcome:?}"
                );
                assert!(ctx.check().is_ok(), "{len} cursor {cursor}");
                // the generic filter decodes the values, which takes more than that room
                drop(held);
                let generic = evaluate_mask(&ctx, &t, std::slice::from_ref(&e), None).unwrap();
                assert!(generic.iter().all(|&k| k), "{len} cursor {cursor}");
                assert!(ctx.check().is_ok(), "{len} cursor {cursor}");
            }
        }
    }

    #[test]
    fn a_short_floor_after_an_unrelated_long_key_fits_the_cursor_budget() {
        use super::super::expr::CmpOp;
        let s = Store::in_memory(StoreOptions::default());
        s.load(&[Source::from_bytes(
            format!(
                "<urn:s:0> <urn:p> \"a{}\" .\n<urn:s:1> <urn:p> \"b\" .\n<urn:s:2> <urn:p> \"c\" .\n",
                "é".repeat(8192)
            )
            .into_bytes(),
            RdfFormat::NTriples,
            None,
        )])
        .unwrap();
        let snap = s.snapshot();
        let id = |word| {
            snap.lookup_term(&oxrdf::Literal::new_simple_literal(word).into())
                .unwrap()
        };
        let (b, c) = (id("b"), id("c"));
        assert!(!(b.payload() as usize).is_multiple_of(crate::vocab::FC_BLOCK));
        let mut ctx = Ctx::new(snap);
        ctx.mem_limit = 64 << 20;
        ctx.enable_cursor_accounting().unwrap();
        let (x, y) = (ctx.var("x"), ctx.var("y"));
        let mut t = Table::new(vec![x, y]);
        for _ in 0..STRING_ORDER_ROWS {
            t.push_row(&[b, c]);
        }
        // On one worker this admits short keys while refusing the long predecessor.
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(1)
            .build()
            .unwrap();
        let _held = ctx
            .charge(ctx.mem_limit - ctx.live_bytes() - (8 << 10))
            .unwrap();
        pool.install(|| {
            let e = cmp(x, y, CmpOp::Lt);
            let mask = string_order_mask(&ctx, &t, (x, y), CmpOp::Lt, &e)
                .unwrap()
                .expect("the floors apply");
            let direct = evaluate_mask(&ctx, &t, &[e], None).unwrap();
            assert_eq!(mask, direct);
            assert!(mask.iter().all(|&keep| keep));
            assert!(ctx.check().is_ok());
        });
    }

    /// The strings of the growth tests: `n` distinct words, each the object of one subject.
    fn word_store(n: usize) -> (Store, Vec<Id>) {
        // distinct for every i, in an order that spreads neighbours apart
        let word = |i: usize| format!("w{}-{i}", i * 7919 % 100_003);
        let mut nt = String::new();
        for i in 0..n {
            nt.push_str(&format!("<urn:s:{i}> <urn:p> \"{}\" .\n", word(i)));
        }
        let s = Store::in_memory(StoreOptions::default());
        s.load(&[Source::from_bytes(
            nt.into_bytes(),
            RdfFormat::NTriples,
            None,
        )])
        .unwrap();
        let snap = s.snapshot();
        let ids: Vec<Id> = (0..n)
            .map(|i| {
                snap.lookup_term(&oxrdf::Literal::new_simple_literal(word(i)).into())
                    .unwrap()
            })
            .collect();
        drop(snap);
        (s, ids)
    }

    /// Strings whose floors are read again evict each other from a small first table, in
    /// a pool of several threads, so the table grows between the parts of a mask and
    /// between masks: the masks match the generic filter in both modes, the tables are
    /// charged and take the query's room, and a cap that refuses a later table keeps the
    /// one the query has and leaves the masks exact without a failure.
    #[test]
    fn parallel_masks_grow_the_floor_table_within_the_charge() {
        use super::super::expr::CmpOp;
        let n = 6000usize;
        let (s, ids) = word_store(n);
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(4)
            .build()
            .unwrap();
        pool.install(|| {
            for (mem_limit, roomy) in [(256 << 20, true), (1 << 20, false)] {
                for cursor in [false, true] {
                    let mut ctx = Ctx::new(s.snapshot());
                    ctx.mem_limit = mem_limit;
                    if cursor {
                        ctx.enable_cursor_accounting().unwrap();
                    }
                    ctx.floor_cache().set_first_slots(64);
                    let room = ctx.memory_room();
                    for (round, op) in [CmpOp::Lt, CmpOp::Ge].into_iter().enumerate() {
                        let (x, y) = (ctx.var("x"), ctx.var("y"));
                        let mut t = Table::new(vec![x, y]);
                        for i in 0..4 * n {
                            t.push_row(&[ids[i % n], ids[(i * 13 + round) % n]]);
                        }
                        assert!(t.len() > PAR_THRESHOLD);
                        let e = cmp(x, y, op);
                        let mask = string_order_mask(&ctx, &t, (x, y), op, &e)
                            .unwrap()
                            .expect("the floors apply");
                        let generic =
                            evaluate_mask(&ctx, &t, std::slice::from_ref(&e), None).unwrap();
                        assert_eq!(mask, generic, "{mem_limit} cursor {cursor} {op:?}");
                        assert!(ctx.check().is_ok(), "{mem_limit} cursor {cursor}");
                    }
                    let cache = ctx.floor_cache();
                    let cap = (64u64 << 20).min(mem_limit / 16);
                    assert!(cache.reserved() <= cap, "{mem_limit} cursor {cursor}");
                    // every table allocated, each with its overhead, is charged
                    assert!(
                        cache.reserved() >= cache.used(),
                        "{mem_limit} cursor {cursor}"
                    );
                    let tables: u64 = if roomy { 4 } else { 3 };
                    let words: u64 = [64u64, 512, 4096, 8192][..tables as usize].iter().sum();
                    assert_eq!(
                        cache.used(),
                        8 * words + tables * super::super::ctx::FloorTable::OVERHEAD,
                        "{mem_limit} cursor {cursor}"
                    );
                    assert!(cache.reserved() > 0, "{mem_limit} cursor {cursor}");
                    assert!(
                        ctx.memory_room() + cache.reserved() <= room,
                        "{mem_limit} cursor {cursor}: the cache takes the query's room"
                    );
                    // 64, 512 and 4096 slots fit under the smaller cap, 8192 more do not
                    let slots = cache.slots();
                    if roomy {
                        assert_eq!(slots, n.next_power_of_two(), "cursor {cursor}");
                        assert!(cache.entries() > n / 2, "cursor {cursor}");
                    } else {
                        assert_eq!(slots, 4096, "cursor {cursor}: the cap refused");
                        assert!(cache.entries() < n, "cursor {cursor}");
                    }
                }
            }
        });
    }

    /// A sample row whose first value has no floor does not read the second one's, as
    /// the generic comparison does not, so a long second string that a cursor's budget
    /// refuses leaves the mask to the generic filter without a failure.
    #[test]
    fn a_sample_row_without_a_first_string_reads_no_second_floor() {
        use super::super::expr::CmpOp;
        let long = "a".repeat(1 << 20);
        let s = Store::in_memory(StoreOptions::default());
        s.load(&[Source::from_bytes(
            format!("<urn:s:0> <urn:p> \"{long}\" .\n").into_bytes(),
            RdfFormat::NTriples,
            None,
        )])
        .unwrap();
        let snap = s.snapshot();
        let y_id = snap
            .lookup_term(&oxrdf::Literal::new_simple_literal(&long).into())
            .unwrap();
        drop(snap);
        let mut ctx = Ctx::new(s.snapshot());
        ctx.mem_limit = 4 << 20;
        ctx.enable_cursor_accounting().unwrap();
        let (x, y) = (ctx.var("x"), ctx.var("y"));
        let mut t = Table::new(vec![x, y]);
        for _ in 0..STRING_ORDER_ROWS {
            t.push_row(&[Id::UNDEF, y_id]);
        }
        let held = ctx.charge(ctx.mem_limit - ctx.live_bytes() - 1024).unwrap();
        let e = cmp(x, y, CmpOp::Lt);
        let outcome = string_order_mask(&ctx, &t, (x, y), CmpOp::Lt, &e);
        assert!(matches!(outcome, Ok(None)), "{outcome:?}");
        assert!(ctx.check().is_ok(), "no floor of the long string was read");
        let generic = evaluate_mask(&ctx, &t, std::slice::from_ref(&e), None).unwrap();
        assert!(generic.iter().all(|&k| !k));
        drop(held);
    }

    /// Strings that rarely repeat keep the first table: masks over pairs of words that no
    /// other row holds evict floors that were never read again, which does not grow it,
    /// however many masks there are, since no row's floors are looked up twice.
    #[test]
    fn floors_read_once_do_not_grow_the_table() {
        use super::super::expr::CmpOp;
        let rows = STRING_ORDER_ROWS;
        let masks = 40;
        let (s, ids) = word_store(masks * 2 * rows);
        let distinct: std::collections::HashSet<Id> = ids.iter().copied().collect();
        assert_eq!(distinct.len(), ids.len(), "no word recurs");
        let mut ctx = Ctx::new(s.snapshot());
        ctx.mem_limit = 256 << 20;
        // 2^10 slots hold the threshold at 32, under the 128 floors of one sample
        let first = 1 << 10;
        ctx.floor_cache().set_first_slots(first);
        for part in ids.chunks(2 * rows) {
            let (x, y) = (ctx.var("x"), ctx.var("y"));
            let mut t = Table::new(vec![x, y]);
            for pair in part.chunks(2) {
                t.push_row(pair);
            }
            let e = cmp(x, y, CmpOp::Lt);
            let mask = string_order_mask(&ctx, &t, (x, y), CmpOp::Lt, &e)
                .unwrap()
                .expect("the floors apply");
            let generic = evaluate_mask(&ctx, &t, std::slice::from_ref(&e), None).unwrap();
            assert_eq!(mask, generic);
        }
        assert!(
            ids.len().next_power_of_two() > first,
            "the table could grow"
        );
        assert_eq!(ctx.floor_cache().slots(), first);
    }

    /// Workers that look up the floors of ids sharing the slots of a tiny table at once,
    /// while the table grows under a map that still reads the old one, each get the floor
    /// [`Ctx::string_floor`] computes, in both modes.
    #[test]
    fn concurrent_floor_lookups_match_the_computed_floors() {
        use rayon::prelude::*;
        let (s, ids) = store("long");
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(8)
            .build()
            .unwrap();
        for cursor in [false, true] {
            let mut ctx = Ctx::new(s.snapshot());
            ctx.mem_limit = 64 << 20;
            if cursor {
                ctx.enable_cursor_accounting().unwrap();
            }
            ctx.floor_cache().set_first_slots(16);
            let expected: Vec<Option<u64>> = ids.iter().map(|&id| ctx.string_floor(id)).collect();
            assert!(expected.iter().any(Option::is_some));
            assert!(ids.len() > 4 * 16, "ids share the slots");
            pool.install(|| {
                let held = ctx.floor_map();
                for round in 0..8usize {
                    (0..64usize).into_par_iter().for_each(|task| {
                        let mut map = ctx.floor_map();
                        for k in 0..4 * ids.len() {
                            let j = (k * 31 + task * 17 + round) % ids.len();
                            assert_eq!(map.floor(ids[j]), expected[j], "cursor {cursor}");
                        }
                    });
                    ctx.grow_floor_cache();
                }
                // a map taken before the growth still reads its tables
                let mut held = held;
                for (j, &id) in ids.iter().enumerate() {
                    assert_eq!(held.floor(id), expected[j], "cursor {cursor}");
                }
            });
            assert!(
                ctx.floor_cache().slots() > 16,
                "cursor {cursor}: the table grew"
            );
            assert!(ctx.check().is_ok(), "cursor {cursor}");
        }
    }

    /// A mask over few distinct strings in a large vocabulary starts at a compact floor
    /// table instead of `FLOOR_TABLE_FIRST`, keeps its charge small, never grows and
    /// matches direct comparison in both modes.
    #[test]
    fn small_distinct_floor_input_uses_compact_initial_table() {
        use super::super::ctx::{FLOOR_TABLE_FEWEST, FLOOR_TABLE_FIRST, FloorTable};
        use super::super::expr::CmpOp;
        let (s, mut ids) = word_store(20_000);
        // Contiguous vocabulary ids have no direct-mapped collisions in 1,024 slots,
        // verifying both compact initial sizing and zero spurious growth over 20,000 rows.
        ids.sort_by_key(|id| id.payload());
        let small = &ids[..200];
        let rows = 20_000usize;
        for cursor in [false, true] {
            let mut ctx = Ctx::new(s.snapshot());
            ctx.mem_limit = 64 << 20;
            if cursor {
                ctx.enable_cursor_accounting().unwrap();
            }
            let (x, y) = (ctx.var("x"), ctx.var("y"));
            let mut t = Table::new(vec![x, y]);
            for i in 0..rows {
                t.push_row(&[small[i % small.len()], small[(i * 7 + 3) % small.len()]]);
            }
            let e = cmp(x, y, CmpOp::Lt);
            let mask = string_order_mask(&ctx, &t, (x, y), CmpOp::Lt, &e)
                .unwrap()
                .expect("the floors apply");
            let generic = evaluate_mask(&ctx, &t, std::slice::from_ref(&e), None).unwrap();
            assert_eq!(mask, generic, "cursor {cursor}");
            let cache = ctx.floor_cache();
            assert_eq!(
                cache.slots(),
                FLOOR_TABLE_FEWEST,
                "cursor {cursor}: stayed compact below {FLOOR_TABLE_FIRST}"
            );
            assert_eq!(
                cache.used(),
                FloorTable::bytes(FLOOR_TABLE_FEWEST),
                "cursor {cursor}: no growth allocations"
            );
            assert!(
                cache.reserved() <= 64 << 10,
                "cursor {cursor}: compact charge ({})",
                cache.reserved()
            );
            assert!(ctx.check().is_ok(), "cursor {cursor}");
        }
    }

    /// Comparing one unique string column against a small repeating string column (like
    /// `str-pair-lt`) allocates a compact table below `FLOOR_TABLE_FIRST`, never grows
    /// across eager or batched execution and matches direct comparison.
    #[test]
    fn one_unique_and_one_small_column_stays_compact_without_growing() {
        use super::super::ctx::{FLOOR_TABLE_FIRST, FloorTable};
        use super::super::expr::CmpOp;
        let unique_rows = 20_000usize;
        let cities = 10usize;
        let (s, ids) = word_store(unique_rows + cities);
        let city_ids = &ids[..cities];
        let name_ids = &ids[cities..];
        for (cursor, batched) in [(false, false), (true, true)] {
            let mut ctx = Ctx::new(s.snapshot());
            ctx.mem_limit = 64 << 20;
            if cursor {
                ctx.enable_cursor_accounting().unwrap();
            }
            let (x, y) = (ctx.var("x"), ctx.var("y"));
            let e = cmp(x, y, CmpOp::Lt);
            let step = if batched {
                STRING_ORDER_ROWS
            } else {
                unique_rows
            };
            let mut at = 0;
            while at + STRING_ORDER_ROWS <= unique_rows {
                let end = (at + step).min(unique_rows);
                let mut t = Table::new(vec![x, y]);
                for i in at..end {
                    t.push_row(&[name_ids[i], city_ids[i % cities]]);
                }
                let mask = string_order_mask(&ctx, &t, (x, y), CmpOp::Lt, &e)
                    .unwrap()
                    .expect("the floors apply");
                let generic = evaluate_mask(&ctx, &t, std::slice::from_ref(&e), None).unwrap();
                assert_eq!(mask, generic, "cursor {cursor} batched {batched} at {at}");
                at = end;
            }
            let cache = ctx.floor_cache();
            assert!(
                cache.slots() < FLOOR_TABLE_FIRST,
                "cursor {cursor} batched {batched}: {} slots",
                cache.slots()
            );
            assert_eq!(
                cache.used(),
                FloorTable::bytes(cache.slots()),
                "cursor {cursor} batched {batched}: never grew"
            );
            assert!(ctx.check().is_ok(), "cursor {cursor} batched {batched}");
        }
    }

    /// A large repeating input whose distinct strings exceed `FLOOR_TABLE_FIRST` pre-sizes
    /// its initial table cleanly to the vocabulary clamp without growth reallocations or
    /// repeated floor computations, in both modes and on one or several threads.
    #[test]
    fn large_distinct_repeating_input_presizes_without_growth() {
        use super::super::ctx::{FLOOR_TABLE_FIRST, FloorTable};
        use super::super::expr::CmpOp;
        let distinct = 70_000usize;
        let rows = 3 * distinct;
        let (s, ids) = word_store(distinct);
        let expected_slots = s
            .snapshot()
            .generation
            .vocab
            .first_triple()
            .next_power_of_two() as usize;
        assert!(expected_slots > FLOOR_TABLE_FIRST);
        // Reference mask computed once to keep debug-mode runtime short.
        let ref_ctx = Ctx::new(s.snapshot());
        let (rx, ry) = (ref_ctx.var("x"), ref_ctx.var("y"));
        let mut ref_t = Table::new(vec![rx, ry]);
        for i in 0..rows {
            ref_t.push_row(&[ids[i % distinct], ids[(i * 13 + 7) % distinct]]);
        }
        let ref_e = cmp(rx, ry, CmpOp::Lt);
        let generic = evaluate_mask(&ref_ctx, &ref_t, std::slice::from_ref(&ref_e), None).unwrap();
        for threads in [1, 4] {
            let pool = rayon::ThreadPoolBuilder::new()
                .num_threads(threads)
                .build()
                .unwrap();
            pool.install(|| {
                for cursor in [false, true] {
                    let mut ctx = Ctx::new(s.snapshot());
                    ctx.mem_limit = 256 << 20;
                    if cursor {
                        ctx.enable_cursor_accounting().unwrap();
                    }
                    let (x, y) = (ctx.var("x"), ctx.var("y"));
                    let mut t = Table::new(vec![x, y]);
                    t.cols[0].clone_from(&ref_t.cols[0]);
                    t.cols[1].clone_from(&ref_t.cols[1]);
                    t.len = ref_t.len;
                    let e = cmp(x, y, CmpOp::Lt);
                    let mask = string_order_mask(&ctx, &t, (x, y), CmpOp::Lt, &e)
                        .unwrap()
                        .expect("the floors apply");
                    assert_eq!(mask, generic, "threads {threads} cursor {cursor}");
                    let cache = ctx.floor_cache();
                    assert_eq!(
                        cache.slots(),
                        expected_slots,
                        "threads {threads} cursor {cursor}: pre-sized directly"
                    );
                    assert_eq!(
                        cache.used(),
                        FloorTable::bytes(expected_slots),
                        "threads {threads} cursor {cursor}: single allocation, no growth"
                    );
                    if threads == 1 {
                        assert_eq!(
                            cache.computed(),
                            distinct,
                            "cursor {cursor}: zero evicted recomputations"
                        );
                    }
                    assert!(ctx.check().is_ok(), "threads {threads} cursor {cursor}");
                }
            });
        }
    }

    /// Tight memory budgets refuse or clamp an oversized estimated initial table before
    /// allocation without recording a query failure, preserving exact parity with direct
    /// comparison in both eager and cursor modes.
    #[test]
    fn tight_memory_budget_refuses_or_clamps_oversized_initial_table() {
        use super::super::ctx::{FLOOR_TABLE_FEWEST, FloorTable};
        use super::super::expr::CmpOp;
        let distinct = 70_000usize;
        let rows = 3 * distinct;
        let (s, ids) = word_store(distinct);
        let ref_ctx = Ctx::new(s.snapshot());
        let (rx, ry) = (ref_ctx.var("x"), ref_ctx.var("y"));
        let mut ref_t = Table::new(vec![rx, ry]);
        for i in 0..rows {
            ref_t.push_row(&[ids[i % distinct], ids[(i * 13 + 7) % distinct]]);
        }
        let ref_e = cmp(rx, ry, CmpOp::Lt);
        let generic = evaluate_mask(&ref_ctx, &ref_t, std::slice::from_ref(&ref_e), None).unwrap();
        let full_slots = s
            .snapshot()
            .generation
            .vocab
            .first_triple()
            .next_power_of_two() as usize;
        assert!(estimate_floor_slots(&ref_t.cols[0], &ref_t.cols[1]).slots >= full_slots);

        for cursor in [false, true] {
            // (a) Small cache cap clamps initial table before allocation.
            {
                let mut ctx = Ctx::new(s.snapshot());
                ctx.mem_limit = 4 << 20;
                if cursor {
                    ctx.enable_cursor_accounting().unwrap();
                }
                let cap = ctx.mem_limit / 16;
                let (x, y) = (ctx.var("x"), ctx.var("y"));
                let mut t = Table::new(vec![x, y]);
                t.cols[0].clone_from(&ref_t.cols[0]);
                t.cols[1].clone_from(&ref_t.cols[1]);
                t.len = ref_t.len;
                let e = cmp(x, y, CmpOp::Lt);
                let mask = string_order_mask(&ctx, &t, (x, y), CmpOp::Lt, &e)
                    .unwrap()
                    .expect("the floors apply");
                assert_eq!(mask, generic, "cap clamp cursor {cursor}");
                let cache = ctx.floor_cache();
                assert!(cache.slots() >= FLOOR_TABLE_FEWEST && cache.slots() < full_slots);
                assert!(cache.used() <= cap, "cursor {cursor}");
                assert!(cache.reserved() <= cap, "cursor {cursor}");
                assert!(ctx.check().is_ok(), "cursor {cursor}");
            }

            // (b) Remaining room smaller than one reservation step refuses allocation
            // cleanly with zero reserved bytes and exact mask parity.
            {
                let mut ctx = Ctx::new(s.snapshot());
                ctx.mem_limit = 64 << 20;
                if cursor {
                    ctx.enable_cursor_accounting().unwrap();
                }
                let (x, y) = (ctx.var("x"), ctx.var("y"));
                let mut t = Table::new(vec![x, y]);
                t.cols[0].clone_from(&ref_t.cols[0]);
                t.cols[1].clone_from(&ref_t.cols[1]);
                t.len = ref_t.len;
                let e = cmp(x, y, CmpOp::Lt);
                // 32 KiB leaves headroom for decoding short keys, not for a 64 KiB step.
                let room = 32 << 10;
                let held = ctx.charge(ctx.mem_limit - ctx.live_bytes() - room).unwrap();
                let mask = string_order_mask(&ctx, &t, (x, y), CmpOp::Lt, &e)
                    .unwrap()
                    .expect("the floors apply");
                assert_eq!(mask, generic, "refused initial cursor {cursor}");
                let cache = ctx.floor_cache();
                assert_eq!(cache.slots(), 0, "cursor {cursor}: refused before alloc");
                assert_eq!(cache.used(), 0, "cursor {cursor}");
                assert_eq!(cache.reserved(), 0, "cursor {cursor}");
                assert!(ctx.check().is_ok(), "cursor {cursor}");
                drop(held);
            }

            // (c) Remaining room smaller than the estimated 1 MiB table steps down to a
            // smaller admitted table without allocating the oversized table.
            {
                let mut ctx = Ctx::new(s.snapshot());
                ctx.mem_limit = 64 << 20;
                if cursor {
                    ctx.enable_cursor_accounting().unwrap();
                }
                let (x, y) = (ctx.var("x"), ctx.var("y"));
                let mut t = Table::new(vec![x, y]);
                t.cols[0].clone_from(&ref_t.cols[0]);
                t.cols[1].clone_from(&ref_t.cols[1]);
                t.len = ref_t.len;
                let e = cmp(x, y, CmpOp::Lt);
                let room = 200 << 10;
                let held = ctx.charge(ctx.mem_limit - ctx.live_bytes() - room).unwrap();
                let mask = string_order_mask(&ctx, &t, (x, y), CmpOp::Lt, &e)
                    .unwrap()
                    .expect("the floors apply");
                assert_eq!(mask, generic, "stepped-down initial cursor {cursor}");
                let cache = ctx.floor_cache();
                assert!(
                    cache.slots() >= FLOOR_TABLE_FEWEST && cache.slots() < full_slots,
                    "cursor {cursor}: stepped down to {} slots",
                    cache.slots()
                );
                assert!(
                    FloorTable::bytes(cache.slots()) <= room,
                    "cursor {cursor}: fits remaining room"
                );
                assert!(cache.reserved() <= room, "cursor {cursor}");
                assert!(ctx.check().is_ok(), "cursor {cursor}");
                drop(held);
            }
        }
    }

    /// Language-tagged and non-inline typed base-vocabulary literals (whether on row 0 or
    /// later in the initial sample) fall back without sizing, allocating or initializing
    /// the floor cache, leaving a later eligible simple-literal mask on the same context
    /// free to choose its own compact initial table.
    #[test]
    fn ineligible_base_literals_leave_floor_cache_uninitialized_for_later_mask() {
        use super::super::ctx::{FLOOR_TABLE_FEWEST, FLOOR_TABLE_FIRST, FloorTable};
        use super::super::expr::CmpOp;
        let lang_n = 20_000usize;
        let typed_n = 5_000usize;
        let simple_n = 200usize;
        let mut nt = String::new();
        for i in 0..lang_n {
            nt.push_str(&format!("<urn:l:{i}> <urn:p> \"l{i:05}\"@en .\n"));
        }
        for i in 0..typed_n {
            nt.push_str(&format!(
                "<urn:t:{i}> <urn:p> \"t{i:05}\"^^<urn:custom> .\n"
            ));
        }
        for i in 0..simple_n {
            nt.push_str(&format!("<urn:s:{i}> <urn:p> \"s{i:04}\" .\n"));
        }
        let s = Store::in_memory(StoreOptions::default());
        s.load(&[Source::from_bytes(
            nt.into_bytes(),
            RdfFormat::NTriples,
            None,
        )])
        .unwrap();
        let snap = s.snapshot();
        let lang_ids: Vec<Id> = (0..lang_n)
            .map(|i| {
                snap.lookup_term(
                    &oxrdf::Literal::new_language_tagged_literal(format!("l{i:05}"), "en")
                        .unwrap()
                        .into(),
                )
                .unwrap()
            })
            .collect();
        let custom_dt = oxrdf::NamedNode::new_unchecked("urn:custom");
        let typed_ids: Vec<Id> = (0..typed_n)
            .map(|i| {
                snap.lookup_term(
                    &oxrdf::Literal::new_typed_literal(format!("t{i:05}"), custom_dt.clone())
                        .into(),
                )
                .unwrap()
            })
            .collect();
        let mut simple_ids: Vec<Id> = (0..simple_n)
            .map(|i| {
                snap.lookup_term(&oxrdf::Literal::new_simple_literal(format!("s{i:04}")).into())
                    .unwrap()
            })
            .collect();
        simple_ids.sort_by_key(|id| id.payload());
        drop(snap);

        for cursor in [false, true] {
            let mut ctx = Ctx::new(s.snapshot());
            ctx.mem_limit = 64 << 20;
            if cursor {
                ctx.enable_cursor_accounting().unwrap();
            }
            assert!(ctx.may_be_base_literal(lang_ids[0]));
            assert!(ctx.may_be_base_literal(typed_ids[0]));
            let (x, y) = (ctx.var("x"), ctx.var("y"));
            let e = cmp(x, y, CmpOp::Lt);

            // (a) Large repeating language-tagged mask whose raw-id pattern would estimate
            // >= FLOOR_TABLE_FIRST must fall back without initializing or reserving the cache.
            let mut lang_t = Table::new(vec![x, y]);
            for i in 0..(3 * lang_n) {
                lang_t.push_row(&[lang_ids[i % lang_n], lang_ids[(i * 13 + 7) % lang_n]]);
            }
            assert!(
                estimate_floor_slots(&lang_t.cols[0], &lang_t.cols[1]).slots >= FLOOR_TABLE_FIRST
            );
            let out = string_order_mask(&ctx, &lang_t, (x, y), CmpOp::Lt, &e).unwrap();
            assert!(out.is_none(), "lang-tagged mask must fall back");
            assert!(ctx.floor_cache_needs_init(), "cursor {cursor}");
            assert_eq!(ctx.floor_cache().slots(), 0, "cursor {cursor}");
            assert_eq!(ctx.floor_cache().used(), 0, "cursor {cursor}");
            assert_eq!(ctx.floor_cache().reserved(), 0, "cursor {cursor}");
            assert!(ctx.check().is_ok(), "cursor {cursor}");

            // (b) Non-inline typed base-vocabulary literal mask also leaves cache uninitialized.
            let mut typed_t = Table::new(vec![x, y]);
            for i in 0..(3 * typed_n) {
                typed_t.push_row(&[typed_ids[i % typed_n], typed_ids[(i * 7 + 3) % typed_n]]);
            }
            let out = string_order_mask(&ctx, &typed_t, (x, y), CmpOp::Lt, &e).unwrap();
            assert!(out.is_none(), "typed mask must fall back");
            assert!(ctx.floor_cache_needs_init(), "cursor {cursor}");
            assert_eq!(ctx.floor_cache().slots(), 0, "cursor {cursor}");
            assert_eq!(ctx.floor_cache().used(), 0, "cursor {cursor}");
            assert_eq!(ctx.floor_cache().reserved(), 0, "cursor {cursor}");
            assert!(ctx.check().is_ok(), "cursor {cursor}");

            // (c) Mixed mask with simple literals on row 0 and a language-tagged literal
            // later in the initial sample still leaves cache uninitialized.
            let mut mixed_t = Table::new(vec![x, y]);
            mixed_t.push_row(&[simple_ids[0], simple_ids[1]]);
            for i in 1..STRING_ORDER_ROWS {
                mixed_t.push_row(&[lang_ids[i % lang_n], lang_ids[(i + 1) % lang_n]]);
            }
            let out = string_order_mask(&ctx, &mixed_t, (x, y), CmpOp::Lt, &e).unwrap();
            assert!(out.is_none(), "mixed sample must fall back");
            assert!(ctx.floor_cache_needs_init(), "cursor {cursor}");
            assert_eq!(ctx.floor_cache().slots(), 0, "cursor {cursor}");
            assert_eq!(ctx.floor_cache().used(), 0, "cursor {cursor}");
            assert_eq!(ctx.floor_cache().reserved(), 0, "cursor {cursor}");
            assert!(ctx.check().is_ok(), "cursor {cursor}");

            // (d) A subsequent eligible small-distinct simple-literal mask on the same
            // context initializes a compact FLOOR_TABLE_FEWEST table.
            let mut simple_t = Table::new(vec![x, y]);
            for i in 0..20_000usize {
                simple_t.push_row(&[
                    simple_ids[i % simple_ids.len()],
                    simple_ids[(i * 7 + 3) % simple_ids.len()],
                ]);
            }
            let mask = string_order_mask(&ctx, &simple_t, (x, y), CmpOp::Lt, &e)
                .unwrap()
                .expect("eligible simple-literal mask applies");
            let generic = evaluate_mask(&ctx, &simple_t, std::slice::from_ref(&e), None).unwrap();
            assert_eq!(mask, generic, "cursor {cursor}");
            assert!(!ctx.floor_cache_needs_init(), "cursor {cursor}");
            assert_eq!(
                ctx.floor_cache().slots(),
                FLOOR_TABLE_FEWEST,
                "cursor {cursor}: sized from eligible mask, not earlier ineligible masks"
            );
            assert_eq!(
                ctx.floor_cache().used(),
                FloorTable::bytes(FLOOR_TABLE_FEWEST),
                "cursor {cursor}"
            );
            assert!(ctx.check().is_ok(), "cursor {cursor}");
        }
    }

    /// A refused cursor floor decode on `xs[0]` still surfaces its memory failure when
    /// `ys[0]` is not a base-vocabulary literal, whether the floor cache is uninitialized
    /// or already initialized.
    #[test]
    fn refused_left_decode_with_ineligible_right_still_fails() {
        use super::super::expr::CmpOp;
        let long = "a".repeat(1 << 20);
        let s = Store::in_memory(StoreOptions::default());
        s.load(&[Source::from_bytes(
            format!("<urn:s:0> <urn:p> \"{long}\" .\n<urn:s:1> <urn:p> <urn:iri> .\n").into_bytes(),
            RdfFormat::NTriples,
            None,
        )])
        .unwrap();
        let snap = s.snapshot();
        let x_id = snap
            .lookup_term(&oxrdf::Literal::new_simple_literal(&long).into())
            .unwrap();
        let iri_id = snap
            .lookup_term(&oxrdf::NamedNode::new_unchecked("urn:iri").into())
            .unwrap();
        drop(snap);

        for pre_init in [false, true] {
            for y_id in [Id::UNDEF, iri_id] {
                let mut ctx = Ctx::new(s.snapshot());
                ctx.mem_limit = 4 << 20;
                ctx.enable_cursor_accounting().unwrap();
                assert!(ctx.may_be_base_literal(x_id));
                assert!(!ctx.may_be_base_literal(y_id));
                if pre_init {
                    let _ = ctx.floor_map();
                    assert!(!ctx.floor_cache_needs_init());
                }
                let (x, y) = (ctx.var("x"), ctx.var("y"));
                let mut t = Table::new(vec![x, y]);
                for _ in 0..STRING_ORDER_ROWS {
                    t.push_row(&[x_id, y_id]);
                }
                let held = ctx.charge(ctx.mem_limit - ctx.live_bytes() - 1024).unwrap();
                let e = cmp(x, y, CmpOp::Lt);
                let outcome = string_order_mask(&ctx, &t, (x, y), CmpOp::Lt, &e);
                assert!(
                    outcome.is_err(),
                    "pre_init {pre_init} y_id {y_id:?}: refused left decode must fail, got {outcome:?}"
                );
                assert!(
                    ctx.check().is_err(),
                    "pre_init {pre_init} y_id {y_id:?}: memory failure must be recorded"
                );
                if !pre_init {
                    assert!(ctx.floor_cache_needs_init());
                    assert_eq!(ctx.floor_cache().slots(), 0);
                    assert_eq!(ctx.floor_cache().reserved(), 0);
                }
                drop(held);
            }
        }
    }

    /// Growing the shared floor cache via `grow_floor_cache` migrates live entries so
    /// subsequent lookups return the exact floors without recomputing them.
    #[test]
    fn grow_floor_cache_preserves_entries_without_recomputing() {
        let (s, ids) = word_store(512);
        let mut by_slot: Vec<Vec<Id>> = vec![Vec::new(); 64];
        for &id in &ids {
            by_slot[(id.payload() as usize) & 63].push(id);
        }
        let preserved: Vec<Id> = by_slot[..32]
            .iter()
            .filter_map(|bucket| bucket.first().copied())
            .collect();
        let evictors: Vec<(Id, Id)> = by_slot[32..]
            .iter()
            .filter(|bucket| bucket.len() >= 2)
            .map(|bucket| (bucket[0], bucket[1]))
            .take(4)
            .collect();
        assert!(preserved.len() >= 16);
        assert_eq!(evictors.len(), 4);

        for cursor in [false, true] {
            let mut ctx = Ctx::new(s.snapshot());
            ctx.mem_limit = 64 << 20;
            if cursor {
                ctx.enable_cursor_accounting().unwrap();
            }
            ctx.floor_cache().set_first_slots(64);
            let expected: Vec<Option<u64>> =
                preserved.iter().map(|&id| ctx.string_floor(id)).collect();
            assert!(expected.iter().all(Option::is_some));

            {
                let mut map = ctx.floor_map();
                for (&id, &exp) in preserved.iter().zip(&expected) {
                    assert_eq!(map.floor(id), exp);
                }
                // Trigger >= 4 read-again evictions in slots 32..64 (threshold for 64 slots is 2).
                for &(a, b) in &evictors {
                    let _ = map.floor(a);
                    let _ = map.floor(a);
                    let _ = map.floor(b);
                }
            }
            assert_eq!(ctx.floor_cache().slots(), 64, "cursor {cursor}");
            let before_computed = ctx.floor_cache().computed();
            ctx.grow_floor_cache();
            assert!(
                ctx.floor_cache().slots() > 64,
                "cursor {cursor}: table must grow"
            );

            {
                let mut map = ctx.floor_map();
                for (&id, &exp) in preserved.iter().zip(&expected) {
                    assert_eq!(map.floor(id), exp, "cursor {cursor}");
                }
            }
            assert_eq!(
                ctx.floor_cache().computed(),
                before_computed,
                "cursor {cursor}: preserved entries must not be recomputed after growth"
            );
            assert!(ctx.check().is_ok(), "cursor {cursor}");
        }
    }

    /// `col_distinct_estimate` and `stratum_bounds` compute large-`n` products without `usize`
    /// overflow.
    #[test]
    fn col_distinct_estimate_handles_large_n_without_overflow() {
        let s = FLOOR_ESTIMATE_SAMPLE;
        let large_n = usize::MAX / 2;
        let (est, has_repeats) = col_distinct_estimate(large_n, s, s, s, 0, s / 2);
        assert!(has_repeats);
        assert_eq!(est, large_n / 2);

        // Last of 1,024 strata at n = 1 << 22: (k + 1) * n = 2^32 overflows 32-bit usize.
        let n_32bit_overflow = 1usize << 22;
        let (lo, hi) = stratum_bounds(s - 1, n_32bit_overflow, s);
        assert_eq!(lo, n_32bit_overflow - n_32bit_overflow / s);
        assert_eq!(hi, n_32bit_overflow);
        assert!(hi > lo);

        // Same boundary check at usize::MAX / 2 overflows both 32-bit and 64-bit usize products.
        let (large_lo, large_hi) = stratum_bounds(s - 1, large_n, s);
        assert_eq!(large_lo, large_n - large_n / s - 1);
        assert_eq!(large_hi, large_n);
        assert!(large_hi > large_lo);
    }

    /// A low-reuse stream (all distinct string pairs) defers floor table allocation on its
    /// initial masks, compares keys directly, probes on every 4th mask without growing the
    /// table, and matches `evaluate_mask` across modes and thread counts.
    #[test]
    fn low_reuse_stream_defers_table_bypasses_and_probes_without_growing() {
        use super::super::expr::CmpOp;
        let rows = STRING_ORDER_ROWS;
        let masks = 7;
        let (s, ids) = word_store(masks * 2 * rows);
        for threads in [1, 4] {
            let pool = rayon::ThreadPoolBuilder::new()
                .num_threads(threads)
                .build()
                .unwrap();
            pool.install(|| {
                for cursor in [false, true] {
                    let mut ctx = Ctx::new(s.snapshot());
                    ctx.mem_limit = 256 << 20;
                    if cursor {
                        ctx.enable_cursor_accounting().unwrap();
                    }
                    let (x, y) = (ctx.var("x"), ctx.var("y"));
                    let e = cmp(x, y, CmpOp::Lt);
                    for (idx, part) in ids.chunks(2 * rows).enumerate() {
                        let mut t = Table::new(vec![x, y]);
                        for pair in part.chunks(2) {
                            t.push_row(pair);
                        }
                        let mask = string_order_mask(&ctx, &t, (x, y), CmpOp::Lt, &e)
                            .unwrap()
                            .expect("string_order_mask handles low-reuse pairs");
                        let generic =
                            evaluate_mask(&ctx, &t, std::slice::from_ref(&e), None).unwrap();
                        assert_eq!(
                            mask, generic,
                            "threads {threads} cursor {cursor} mask {idx}"
                        );
                        if idx < 3 {
                            assert_eq!(
                                ctx.floor_cache().slots(),
                                0,
                                "threads {threads} cursor {cursor} mask {idx}: table stays deferred"
                            );
                            assert_eq!(ctx.floor_cache().used(), 0);
                            assert_eq!(ctx.floor_cache().bypassed(), idx + 1);
                        } else if idx == 3 {
                            assert!(
                                ctx.floor_cache().slots() > 0,
                                "threads {threads} cursor {cursor}: probe mask installs table"
                            );
                            assert_eq!(ctx.floor_cache().bypassed(), 3);
                        }
                    }
                    let slots_after_probe = ctx.floor_cache().slots();
                    assert_eq!(
                        ctx.floor_cache().bypassed(),
                        6,
                        "threads {threads} cursor {cursor}: 6 of 7 low-reuse masks bypassed"
                    );
                    assert_eq!(ctx.floor_cache().slots(), slots_after_probe);
                    assert!(ctx.check().is_ok(), "threads {threads} cursor {cursor}");
                }
            });
        }
    }

    /// A shuffled stream whose first batch repeats too few strings to cache initially starts
    /// in bypass mode, populates the table on periodic probe masks, and re-enables floors
    /// once cross-batch reuse rises above the hit-rate threshold.
    #[test]
    fn shuffled_repeating_stream_reenables_floors_after_probing() {
        use super::super::expr::CmpOp;
        let rows = STRING_ORDER_ROWS;
        let distinct = 32_000usize;
        let (s, ids) = word_store(distinct);
        let mut ctx = Ctx::new(s.snapshot());
        ctx.mem_limit = 256 << 20;
        ctx.enable_cursor_accounting().unwrap();
        let (x, y) = (ctx.var("x"), ctx.var("y"));
        let e = cmp(x, y, CmpOp::Lt);

        // Deterministic pseudo-random shuffle over 32,000 distinct strings.
        let mut state: u64 = 0x517c_c1b7_2722_0a95;
        let mut next_id = || {
            state = state
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            ids[((state >> 32) as usize) % distinct]
        };

        let mut bypassed_by_batch = Vec::new();
        for batch in 0..16 {
            let mut t = Table::new(vec![x, y]);
            for _ in 0..rows {
                t.push_row(&[next_id(), next_id()]);
            }
            let mask = string_order_mask(&ctx, &t, (x, y), CmpOp::Lt, &e)
                .unwrap()
                .expect("the mask applies");
            if batch < 2 || batch == 15 {
                let generic = evaluate_mask(&ctx, &t, std::slice::from_ref(&e), None).unwrap();
                assert_eq!(mask, generic, "batch {batch}");
            }
            bypassed_by_batch.push(ctx.floor_cache().bypassed());
        }
        assert!(
            bypassed_by_batch[0] == 1,
            "first low-repeat batch starts bypassed: {bypassed_by_batch:?}"
        );
        assert!(
            ctx.floor_cache().slots() > 0,
            "probe installed the floor table"
        );
        // Once cross-batch hits exceed 1/8, bypass stops growing for the trailing batches.
        let tail_start = bypassed_by_batch[11];
        assert_eq!(
            bypassed_by_batch[15], tail_start,
            "floors stayed enabled once cross-batch reuse rose: {bypassed_by_batch:?}"
        );
        assert!(ctx.check().is_ok());
    }

    /// On the bypassed direct-compare path in cursor mode, a budget-exceeding literal after
    /// the sample fails with `BudgetExceeded(Memory)` and cancellation returns `Cancelled`.
    #[test]
    fn bypassed_direct_mask_enforces_strict_memory_refusal_and_cancellation() {
        use super::super::expr::CmpOp;
        let rows = STRING_ORDER_ROWS;
        let long = format!("z{}", "y".repeat(1 << 19));
        let mut nt = String::new();
        for i in 0..(2 * rows) {
            nt.push_str(&format!("<urn:s:{i}> <urn:p> \"w{i:06}\" .\n"));
        }
        nt.push_str(&format!("<urn:s:long> <urn:p> \"{long}\" .\n"));
        let s = Store::in_memory(StoreOptions::default());
        s.load(&[Source::from_bytes(
            nt.into_bytes(),
            RdfFormat::NTriples,
            None,
        )])
        .unwrap();
        let snap = s.snapshot();
        let ids: Vec<Id> = (0..(2 * rows))
            .map(|i| {
                snap.lookup_term(&oxrdf::Literal::new_simple_literal(format!("w{i:06}")).into())
                    .unwrap()
            })
            .collect();
        let long_id = snap
            .lookup_term(&oxrdf::Literal::new_simple_literal(&long).into())
            .unwrap();
        drop(snap);

        // (a) Strict memory refusal after the initial sample on a bypassed mask.
        {
            let mut ctx = Ctx::new(s.snapshot());
            ctx.mem_limit = 4 << 20;
            ctx.enable_cursor_accounting().unwrap();
            let (x, y) = (ctx.var("x"), ctx.var("y"));
            let mut t = Table::new(vec![x, y]);
            for i in 0..rows {
                let b = if i == STRING_ORDER_SAMPLE + 50 {
                    long_id
                } else {
                    ids[2 * i + 1]
                };
                t.push_row(&[ids[2 * i], b]);
            }
            let e = cmp(x, y, CmpOp::Lt);
            let outcome = string_order_mask(&ctx, &t, (x, y), CmpOp::Lt, &e);
            assert!(
                matches!(&outcome, Err(Error::BudgetExceeded(b)) if b.kind == BudgetKind::Memory),
                "expected memory refusal on bypassed mask, got {outcome:?}"
            );
            assert_eq!(ctx.floor_cache().bypassed(), 1);
            assert!(ctx.check().is_err());
        }

        // (b) Cancellation after decoding scratch allocation on a bypassed mask.
        {
            let mut ctx = Ctx::new(s.snapshot());
            ctx.mem_limit = 64 << 20;
            ctx.enable_cursor_accounting().unwrap();
            let (x, y) = (ctx.var("x"), ctx.var("y"));
            let mut t = Table::new(vec![x, y]);
            for i in 0..rows {
                t.push_row(&[ids[2 * i], ids[2 * i + 1]]);
            }
            let e = cmp(x, y, CmpOp::Lt);
            let cancel = ctx.cancel.clone();
            crate::algo::set_projection_observer(Some(std::sync::Arc::new(move |event| {
                if let crate::algo::ProjectionEvent::Materialized(
                    "checked-vocab-scratch-allocated",
                    _,
                ) = event
                {
                    cancel.store(true, AtomicOrdering::Relaxed);
                }
            })));
            let result = string_order_mask(&ctx, &t, (x, y), CmpOp::Lt, &e);
            crate::algo::set_projection_observer(None);
            assert!(matches!(result, Err(Error::Cancelled)), "{result:?}");
            assert_eq!(ctx.floor_cache().bypassed(), 1);
        }
    }

    /// A bypassed direct-compare mask falls back to the generic row evaluator whenever a
    /// row after the sample holds a non-simple-literal term, matching `evaluate_mask`.
    #[test]
    fn bypassed_direct_mask_falls_back_to_generic_on_mixed_rows() {
        use super::super::expr::CmpOp;
        let rows = STRING_ORDER_ROWS;
        let mut nt = String::new();
        for i in 0..(2 * rows) {
            nt.push_str(&format!("<urn:s:{i}> <urn:p> \"w{i:06}\" .\n"));
        }
        nt.push_str("<urn:s:lang> <urn:p> \"w000100\"@en .\n");
        nt.push_str("<urn:s:typed> <urn:p> \"w000101\"^^<urn:dt> .\n");
        let s = Store::in_memory(StoreOptions::default());
        s.load(&[Source::from_bytes(
            nt.into_bytes(),
            RdfFormat::NTriples,
            None,
        )])
        .unwrap();
        let snap = s.snapshot();
        let ids: Vec<Id> = (0..(2 * rows))
            .map(|i| {
                snap.lookup_term(&oxrdf::Literal::new_simple_literal(format!("w{i:06}")).into())
                    .unwrap()
            })
            .collect();
        let lang = snap
            .lookup_term(
                &oxrdf::Literal::new_language_tagged_literal("w000100", "en")
                    .unwrap()
                    .into(),
            )
            .unwrap();
        let typed = snap
            .lookup_term(
                &oxrdf::Literal::new_typed_literal(
                    "w000101",
                    oxrdf::NamedNode::new_unchecked("urn:dt"),
                )
                .into(),
            )
            .unwrap();
        let iri = snap
            .lookup_term(&oxrdf::NamedNode::new_unchecked("urn:s:0").into())
            .unwrap();
        drop(snap);

        for threads in [1, 4] {
            let pool = rayon::ThreadPoolBuilder::new()
                .num_threads(threads)
                .build()
                .unwrap();
            pool.install(|| {
                for cursor in [false, true] {
                    for op in [CmpOp::Lt, CmpOp::Le, CmpOp::Gt, CmpOp::Ge] {
                        let mut ctx = Ctx::new(s.snapshot());
                        ctx.mem_limit = 64 << 20;
                        if cursor {
                            ctx.enable_cursor_accounting().unwrap();
                        }
                        let local = ctx.intern_value(&Value::Str("w000200".into()));
                        let (x, y) = (ctx.var("x"), ctx.var("y"));
                        let mut t = Table::new(vec![x, y]);
                        for i in 0..rows {
                            let pair = match i {
                                100 => [lang, ids[2 * i + 1]],
                                101 => [ids[2 * i], lang],
                                102 => [typed, ids[2 * i + 1]],
                                103 => [iri, ids[2 * i + 1]],
                                104 => [local, ids[2 * i + 1]],
                                105 => [Id::UNDEF, ids[2 * i + 1]],
                                106 => [ids[2 * i], Id::UNDEF],
                                107 => [ids[2 * i], ids[2 * i]],
                                _ => [ids[2 * i], ids[2 * i + 1]],
                            };
                            t.push_row(&pair);
                        }
                        let e = cmp(x, y, op);
                        let mask = string_order_mask(&ctx, &t, (x, y), op, &e)
                            .unwrap()
                            .expect("the mask applies");
                        let generic =
                            evaluate_mask(&ctx, &t, std::slice::from_ref(&e), None).unwrap();
                        assert_eq!(mask, generic, "threads {threads} cursor {cursor} {op:?}");
                        assert_eq!(ctx.floor_cache().bypassed(), 1);
                        assert!(ctx.check().is_ok());
                    }
                }
            });
        }
    }

    /// Cancellation raised after entering a direct-comparison partition—including inside a
    /// final chunk shorter than 1,024 rows with no later modulo-1024 poll—must fail the
    /// mask with `Error::Cancelled` on both cursor and eager paths.
    #[test]
    fn direct_mask_cancels_after_entering_partition_and_in_short_final_chunk() {
        use super::super::expr::CmpOp;
        let eager_rows = PAR_THRESHOLD + 500;
        let medium = format!("z{}", "m".repeat(320));
        let mut nt = String::new();
        for i in 0..(2 * eager_rows) {
            nt.push_str(&format!("<urn:s> <urn:p> \"w{i:06}\" .\n"));
        }
        nt.push_str(&format!("<urn:s> <urn:p> \"{medium}\" .\n"));
        let s = Store::in_memory(StoreOptions::default());
        s.load(&[Source::from_bytes(
            nt.into_bytes(),
            RdfFormat::NTriples,
            None,
        )])
        .unwrap();
        let snap = s.snapshot();
        let ids: Vec<Id> = (0..(2 * eager_rows))
            .map(|i| {
                snap.lookup_term(&oxrdf::Literal::new_simple_literal(format!("w{i:06}")).into())
                    .unwrap()
            })
            .collect();
        let medium_id = snap
            .lookup_term(&oxrdf::Literal::new_simple_literal(&medium).into())
            .unwrap();
        drop(snap);

        // Trigger cancellation only when a worker grows scratch past 256 bytes (i.e. when
        // decoding `medium_id` inside the direct-comparison partition, never during the
        // initial sample whose keys are short).
        let run_case = |threads: usize, rows: usize, trigger_row: usize| {
            let pool = rayon::ThreadPoolBuilder::new()
                .num_threads(threads)
                .build()
                .unwrap();
            pool.install(|| {
                let mut ctx = Ctx::new(s.snapshot());
                ctx.mem_limit = 64 << 20;
                ctx.enable_cursor_accounting().unwrap();
                let (x, y) = (ctx.var("x"), ctx.var("y"));
                let mut t = Table::new(vec![x, y]);
                for i in 0..rows {
                    let b = if i == trigger_row {
                        medium_id
                    } else {
                        ids[2 * i + 1]
                    };
                    t.push_row(&[ids[2 * i], b]);
                }
                let e = cmp(x, y, CmpOp::Lt);
                let cancel = ctx.cancel.clone();
                let triggered = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
                let seen = triggered.clone();
                let observer = std::sync::Arc::new(move |event| {
                    if let crate::algo::ProjectionEvent::Materialized(
                        "checked-vocab-scratch-allocated",
                        cap,
                    ) = event
                        && cap > 256
                    {
                        seen.store(true, AtomicOrdering::Relaxed);
                        cancel.store(true, AtomicOrdering::Relaxed);
                    }
                });
                pool.broadcast(|_| crate::algo::set_projection_observer(Some(observer.clone())));
                let result = string_order_mask(&ctx, &t, (x, y), CmpOp::Lt, &e);
                pool.broadcast(|_| crate::algo::set_projection_observer(None));
                assert!(
                    triggered.load(AtomicOrdering::Relaxed),
                    "threads {threads} rows {rows} trigger {trigger_row}: growth observer must fire"
                );
                assert!(
                    matches!(result, Err(Error::Cancelled)),
                    "threads {threads} rows {rows} trigger {trigger_row}: expected Cancelled, got {result:?}"
                );
            });
        };

        // Cursor-sized batch (n <= PAR_THRESHOLD): trigger past the final multiple of 1024
        // (row 4090 in 0..4096 has no later modulo-1024 poll).
        run_case(4, STRING_ORDER_ROWS, STRING_ORDER_ROWS - 6);
        run_case(1, STRING_ORDER_ROWS, STRING_ORDER_ROWS - 6);
        // Eager multi-part direct mask (Part 2 is 16448..16884, 436 rows containing zero
        // multiples of 1024):
        // (1) cancellation inside the short final part;
        // (2) cancellation near the end of Part 1 before entering the short final part.
        run_case(4, eager_rows, eager_rows - 4);
        run_case(4, eager_rows, PAR_THRESHOLD + STRING_ORDER_SAMPLE - 8);
        run_case(1, eager_rows, eager_rows - 4);
    }

    /// Multi-part eager masks apply low-hit-rate bypass and periodic probing at eager part
    /// boundaries, matching `evaluate_mask`.
    #[test]
    fn eager_multi_part_mask_transitions_between_floors_and_direct() {
        use super::super::expr::CmpOp;
        let distinct_count = 132_000usize;
        let mut nt = String::new();
        for i in 0..distinct_count {
            nt.push_str(&format!("<urn:s> <urn:p> \"w{i:06}\" .\n"));
        }
        let s = Store::in_memory(StoreOptions::default());
        s.load(&[Source::from_bytes(
            nt.into_bytes(),
            RdfFormat::NTriples,
            None,
        )])
        .unwrap();
        let snap = s.snapshot();
        let ids: Vec<Id> = (0..distinct_count)
            .map(|i| {
                snap.lookup_term(&oxrdf::Literal::new_simple_literal(format!("w{i:06}")).into())
                    .unwrap()
            })
            .collect();
        drop(snap);

        // (a) Floors -> Direct transition across eager parts when Part 1 observes low hits.
        {
            let mut ctx = Ctx::new(s.snapshot());
            ctx.mem_limit = 256 << 20;
            let (x, y) = (ctx.var("x"), ctx.var("y"));
            let e = cmp(x, y, CmpOp::Lt);
            // Warm up with a small repeating mask so the cache starts on Floors with a
            // compact table (<= FLOOR_TABLE_FIRST).
            let mut warm = Table::new(vec![x, y]);
            for i in 0..STRING_ORDER_ROWS {
                warm.push_row(&[ids[i % 16], ids[(i + 5) % 16]]);
            }
            string_order_mask(&ctx, &warm, (x, y), CmpOp::Lt, &e)
                .unwrap()
                .expect("warm mask applies");
            assert_eq!(ctx.floor_cache().bypassed(), 0);

            // Multi-part eager mask (2 parts: 64..16448 on Floors with 0% hits, then
            // 16448..20000 transitioning to Direct).
            let rows = 20_000usize;
            let mut t = Table::new(vec![x, y]);
            for i in 0..rows {
                t.push_row(&[ids[2 * i], ids[2 * i + 1]]);
            }
            let mask = string_order_mask(&ctx, &t, (x, y), CmpOp::Lt, &e)
                .unwrap()
                .expect("eager mask applies");
            let generic = evaluate_mask(&ctx, &t, std::slice::from_ref(&e), None).unwrap();
            assert_eq!(mask, generic);
            assert_eq!(
                ctx.floor_cache().bypassed(),
                1,
                "Part 2 must switch to Direct after Part 1 low-hit-rate floor pass"
            );
            assert!(ctx.check().is_ok());
        }

        // (b) A lone eager mask started in bypass probes on Part 4 and installs its table.
        {
            let mut ctx = Ctx::new(s.snapshot());
            ctx.mem_limit = 256 << 20;
            let (x, y) = (ctx.var("x"), ctx.var("y"));
            let e = cmp(x, y, CmpOp::Lt);
            let rows = 66_000usize;
            let mut t = Table::new(vec![x, y]);
            for i in 0..rows {
                t.push_row(&[ids[2 * i], ids[2 * i + 1]]);
            }
            let mask = string_order_mask(&ctx, &t, (x, y), CmpOp::Lt, &e)
                .unwrap()
                .expect("lone eager mask applies");
            assert_eq!(
                ctx.floor_cache().bypassed(),
                3,
                "Parts 1..3 bypass and Part 4 probes on Floors"
            );
            assert!(
                ctx.floor_cache().slots() > 0,
                "Part 4 probe must install the deferred floor table"
            );
            assert_eq!(mask.len(), rows);
            assert!(ctx.check().is_ok());
        }

        // (c) An eager mask started in bypass that probes on a high-reuse part stays on
        // Floors for subsequent parts.
        {
            let mut ctx = Ctx::new(s.snapshot());
            ctx.mem_limit = 256 << 20;
            let (x, y) = (ctx.var("x"), ctx.var("y"));
            let e = cmp(x, y, CmpOp::Lt);
            let mut first = Table::new(vec![x, y]);
            for i in 0..STRING_ORDER_ROWS {
                first.push_row(&[ids[2 * i], ids[2 * i + 1]]);
            }
            string_order_mask(&ctx, &first, (x, y), CmpOp::Lt, &e)
                .unwrap()
                .expect("first low-reuse mask applies");
            assert_eq!(ctx.floor_cache().bypassed(), 1);
            assert_eq!(ctx.floor_cache().slots(), 0);

            // Parts 1 & 2 (64..32896) stay Direct (bypassed = 2, 3), Part 3 (32896..65792)
            // probes on Floors with high reuse, Part 4 (65792..70000) stays on Floors.
            let rows = 70_000usize;
            let mut t = Table::new(vec![x, y]);
            for i in 0..rows {
                if i < 32_896 {
                    t.push_row(&[ids[2 * i], ids[2 * i + 1]]);
                } else {
                    t.push_row(&[ids[i % 128], ids[(i * 7 + 3) % 128]]);
                }
            }
            let mask = string_order_mask(&ctx, &t, (x, y), CmpOp::Lt, &e)
                .unwrap()
                .expect("second eager mask applies");
            let generic = evaluate_mask(&ctx, &t, std::slice::from_ref(&e), None).unwrap();
            assert_eq!(mask, generic);
            assert_eq!(
                ctx.floor_cache().bypassed(),
                3,
                "Part 3 probe must re-enable Floors so Part 4 does not bypass"
            );
            assert!(ctx.floor_cache().slots() > 0);
            assert!(ctx.check().is_ok());
        }
    }

    /// An ineligible subsequent mask (holding base-vocabulary non-simple literals) must
    /// not advance floor-cache bypass or probe state before sample validation succeeds.
    #[test]
    fn ineligible_subsequent_mask_does_not_advance_floor_bypass_state() {
        use super::super::expr::CmpOp;
        let rows = STRING_ORDER_ROWS;
        let mut nt = String::new();
        for i in 0..(2 * rows) {
            nt.push_str(&format!("<urn:s> <urn:p> \"w{i:06}\" .\n"));
        }
        nt.push_str("<urn:s> <urn:p> \"a\"@en .\n");
        nt.push_str("<urn:s> <urn:p> \"b\"@en .\n");
        let s = Store::in_memory(StoreOptions::default());
        s.load(&[Source::from_bytes(
            nt.into_bytes(),
            RdfFormat::NTriples,
            None,
        )])
        .unwrap();
        let snap = s.snapshot();
        let ids: Vec<Id> = (0..(2 * rows))
            .map(|i| {
                snap.lookup_term(&oxrdf::Literal::new_simple_literal(format!("w{i:06}")).into())
                    .unwrap()
            })
            .collect();
        let lang_a = snap
            .lookup_term(
                &oxrdf::Literal::new_language_tagged_literal("a", "en")
                    .unwrap()
                    .into(),
            )
            .unwrap();
        let lang_b = snap
            .lookup_term(
                &oxrdf::Literal::new_language_tagged_literal("b", "en")
                    .unwrap()
                    .into(),
            )
            .unwrap();
        drop(snap);

        let mut ctx = Ctx::new(s.snapshot());
        ctx.mem_limit = 64 << 20;
        let (x, y) = (ctx.var("x"), ctx.var("y"));
        let e = cmp(x, y, CmpOp::Lt);

        // First mask enters bypass with deferred table allocation.
        let mut t1 = Table::new(vec![x, y]);
        for i in 0..rows {
            t1.push_row(&[ids[2 * i], ids[2 * i + 1]]);
        }
        string_order_mask(&ctx, &t1, (x, y), CmpOp::Lt, &e)
            .unwrap()
            .expect("first mask applies");
        assert_eq!(ctx.floor_cache().bypassed(), 1);
        assert_eq!(ctx.floor_cache().slots(), 0);

        // Repeated ineligible base-literal masks must not advance bypass_streak or install
        // the deferred table.
        let mut ineligible = Table::new(vec![x, y]);
        for _ in 0..rows {
            ineligible.push_row(&[lang_a, lang_b]);
        }
        for _ in 0..5 {
            assert!(
                string_order_mask(&ctx, &ineligible, (x, y), CmpOp::Lt, &e)
                    .unwrap()
                    .is_none()
            );
            assert_eq!(ctx.floor_cache().bypassed(), 1);
            assert_eq!(ctx.floor_cache().slots(), 0);
        }
        assert!(ctx.check().is_ok());
    }

    /// A cursor mask entering bypass on a pending `Floors -> Direct` transition must validate
    /// its sample via direct key comparison so an uncached block-start literal does not
    /// trigger an unnecessary floor predecessor decode under a tight cursor budget.
    #[test]
    fn floors_to_direct_transition_skips_predecessor_decode_under_cursor_budget() {
        use super::super::expr::CmpOp;
        use crate::builder::{BuildOptions, VocabCodec};
        use oxrdf::Literal;

        let mut nt = String::new();
        // IDs 0..=14: 15 short simple literals sorting before "m".
        let mut a_terms = Vec::with_capacity(15);
        for i in 0..15usize {
            let term: oxrdf::Term = Literal::new_simple_literal(format!("a{i:02}")).into();
            nt.push_str(&format!("<urn:s> <urn:p> {term} .\n"));
            a_terms.push(term);
        }
        // ID 15: a 1 MiB extension of "m" (`"mxxx...\xFF"` sorts immediately before `"m\xFF"`).
        let huge_term: oxrdf::Term =
            Literal::new_simple_literal(format!("m{}", "x".repeat(1 << 20))).into();
        nt.push_str(&format!("<urn:s> <urn:p> {huge_term} .\n"));
        // ID 16: short simple literal "m" at block start (`16 % FC_BLOCK == 0`).
        let m_term: oxrdf::Term = Literal::new_simple_literal("m").into();
        nt.push_str(&format!("<urn:s> <urn:p> {m_term} .\n"));
        // Cheap distinct simple literals sorting after "m" to drive a low-hit Floors batch.
        let distinct_count = 2 * STRING_ORDER_ROWS;
        for i in 0..distinct_count {
            nt.push_str(&format!("<urn:s> <urn:p> \"z{i:06}\" .\n"));
        }

        let s = Store::in_memory(StoreOptions {
            build: BuildOptions {
                vocab_codec: Some(VocabCodec::Front),
                ..Default::default()
            },
            ..Default::default()
        });
        s.load(&[Source::from_bytes(
            nt.into_bytes(),
            RdfFormat::NTriples,
            None,
        )])
        .unwrap();
        let snap = s.snapshot();
        assert_eq!(snap.generation.vocab_codec(), VocabCodec::Front);
        let a_ids: Vec<Id> = a_terms
            .iter()
            .map(|t| snap.lookup_term(t).unwrap())
            .collect();
        let huge_id = snap.lookup_term(&huge_term).unwrap();
        let m_id = snap.lookup_term(&m_term).unwrap();
        assert_eq!(huge_id.payload(), 15);
        assert_eq!(m_id.payload(), 16);
        assert!((m_id.payload() as usize).is_multiple_of(crate::vocab::FC_BLOCK));
        let z_ids: Vec<Id> = (0..distinct_count)
            .map(|i| {
                snap.lookup_term(&Literal::new_simple_literal(format!("z{i:06}")).into())
                    .unwrap()
            })
            .collect();
        drop(snap);

        // Sanity check: computing `m`'s floor under a 4 MiB cursor budget fails because
        // decoding its 1 MiB predecessor at ID 15 exceeds the decoding envelope.
        {
            let mut probe_ctx = Ctx::new(s.snapshot());
            probe_ctx.mem_limit = 4 << 20;
            probe_ctx.enable_cursor_accounting().unwrap();
            assert_eq!(probe_ctx.string_floor(m_id), None);
            assert!(
                matches!(
                    probe_ctx.check(),
                    Err(Error::BudgetExceeded(b)) if b.kind == BudgetKind::Memory
                ),
                "computing m's floor must fail when decoding ID 15 under 4 MiB cursor budget"
            );
        }

        let mut ctx = Ctx::new(s.snapshot());
        ctx.mem_limit = 4 << 20;
        ctx.enable_cursor_accounting().unwrap();
        let (x, y) = (ctx.var("x"), ctx.var("y"));
        let e = cmp(x, y, CmpOp::Gt);

        // Batch 1: establish a compact floor table on `Floors` with a repeating batch.
        let mut warm = Table::new(vec![x, y]);
        for i in 0..STRING_ORDER_ROWS {
            warm.push_row(&[a_ids[i % a_ids.len()], a_ids[(i + 5) % a_ids.len()]]);
        }
        string_order_mask(&ctx, &warm, (x, y), CmpOp::Gt, &e)
            .unwrap()
            .expect("warm mask applies");
        assert!(ctx.floor_cache().slots() > 0);
        assert_eq!(ctx.floor_cache().bypassed(), 0);

        // Batch 2: distinct cheap literals drive misses past `FLOOR_BYPASS_MIN_LOOKUPS` with
        // at most 15 read-again evictions (below the growth threshold), queuing `Floors -> Direct`.
        let mut low_hit = Table::new(vec![x, y]);
        for i in 0..STRING_ORDER_ROWS {
            low_hit.push_row(&[z_ids[2 * i], z_ids[2 * i + 1]]);
        }
        string_order_mask(&ctx, &low_hit, (x, y), CmpOp::Gt, &e)
            .unwrap()
            .expect("low-hit mask applies");
        assert_eq!(ctx.floor_cache().bypassed(), 0);

        // Batch 3: comparing uncached block-start ID 16 ("m") with short literal `a00` must
        // preview the pending bypass, validate via `simple_literal_cmp_in`, and succeed.
        let mut transition = Table::new(vec![x, y]);
        for i in 0..STRING_ORDER_ROWS {
            let rhs = if i % 2 == 0 { a_ids[0] } else { z_ids[0] };
            transition.push_row(&[m_id, rhs]);
        }
        let mask = string_order_mask(&ctx, &transition, (x, y), CmpOp::Gt, &e)
            .unwrap()
            .expect("transition mask applies on Direct");
        let generic = evaluate_mask(&ctx, &transition, std::slice::from_ref(&e), None).unwrap();
        assert_eq!(mask, generic);
        assert_eq!(
            ctx.floor_cache().bypassed(),
            1,
            "transition mask must enter Direct bypass"
        );
        assert!(ctx.check().is_ok());
    }

    /// A subsequent `Floors` mask must count its validated 64-row sample's lookups and hits
    /// toward floor hit-rate feedback so boundary decisions do not drop the sampled rows.
    #[test]
    fn later_mask_sample_counts_toward_floor_hit_rate_feedback() {
        use super::super::expr::CmpOp;
        let (s, ids) = word_store(2_500);
        let non_base_a = Id::from_i64(1).unwrap();
        let non_base_b = Id::from_i64(2).unwrap();

        // (a) Sample lookups count toward `FLOOR_BYPASS_MIN_LOOKUPS` (2,048):
        // 64 sampled rows (128 lookups) + 960 tail rows (1,920 lookups) = 2,048 distinct
        // base-literal lookups, followed by non-base rows. Without sample accounting only
        // 1,920 lookups would be recorded and the next mask would stay on Floors.
        {
            let mut ctx = Ctx::new(s.snapshot());
            ctx.mem_limit = 64 << 20;
            let (x, y) = (ctx.var("x"), ctx.var("y"));
            let e = cmp(x, y, CmpOp::Lt);

            let mut warm = Table::new(vec![x, y]);
            for i in 0..STRING_ORDER_ROWS {
                warm.push_row(&[ids[i % 16], ids[(i + 5) % 16]]);
            }
            string_order_mask(&ctx, &warm, (x, y), CmpOp::Lt, &e)
                .unwrap()
                .expect("warm mask applies");
            assert_eq!(ctx.floor_cache().bypassed(), 0);

            let mut boundary = Table::new(vec![x, y]);
            for i in 0..1_024usize {
                boundary.push_row(&[ids[16 + 2 * i], ids[16 + 2 * i + 1]]);
            }
            for _ in 1_024..STRING_ORDER_ROWS {
                boundary.push_row(&[non_base_a, non_base_b]);
            }
            let mask = string_order_mask(&ctx, &boundary, (x, y), CmpOp::Lt, &e)
                .unwrap()
                .expect("boundary mask applies");
            let generic = evaluate_mask(&ctx, &boundary, std::slice::from_ref(&e), None).unwrap();
            assert_eq!(mask, generic);
            assert_eq!(ctx.floor_cache().bypassed(), 0);

            string_order_mask(&ctx, &warm, (x, y), CmpOp::Lt, &e)
                .unwrap()
                .expect("next mask applies");
            assert_eq!(
                ctx.floor_cache().bypassed(),
                1,
                "64 sampled rows + 960 tail rows must reach 2,048 lookups and trigger bypass"
            );
            assert!(ctx.check().is_ok());
        }

        // (b) Sample hits count toward the 1/8 hit-rate threshold:
        // 64 cached sampled rows (128 hits / 128 lookups) + 72 cached tail rows (144 hits) +
        // 952 distinct uncached tail rows (1,904 misses) = 272 hits / 2,176 lookups (12.5%).
        // Without sample hits, 144 / 2,048 (7.0%) would wrongly trigger bypass.
        {
            let mut ctx = Ctx::new(s.snapshot());
            ctx.mem_limit = 64 << 20;
            let (x, y) = (ctx.var("x"), ctx.var("y"));
            let e = cmp(x, y, CmpOp::Lt);

            let mut warm = Table::new(vec![x, y]);
            for i in 0..STRING_ORDER_ROWS {
                warm.push_row(&[ids[i % 16], ids[(i + 5) % 16]]);
            }
            string_order_mask(&ctx, &warm, (x, y), CmpOp::Lt, &e)
                .unwrap()
                .expect("warm mask applies");
            assert_eq!(ctx.floor_cache().bypassed(), 0);

            let mut boundary = Table::new(vec![x, y]);
            // 64 sampled rows + 72 tail rows hitting the 16 cached ids.
            for i in 0..(STRING_ORDER_SAMPLE + 72) {
                boundary.push_row(&[ids[i % 16], ids[(i + 3) % 16]]);
            }
            // 952 distinct uncached tail rows (1,904 misses).
            for i in 0..952usize {
                boundary.push_row(&[ids[16 + 2 * i], ids[16 + 2 * i + 1]]);
            }
            for _ in (STRING_ORDER_SAMPLE + 1_024)..STRING_ORDER_ROWS {
                boundary.push_row(&[non_base_a, non_base_b]);
            }
            let mask = string_order_mask(&ctx, &boundary, (x, y), CmpOp::Lt, &e)
                .unwrap()
                .expect("boundary mask applies");
            let generic = evaluate_mask(&ctx, &boundary, std::slice::from_ref(&e), None).unwrap();
            assert_eq!(mask, generic);

            string_order_mask(&ctx, &warm, (x, y), CmpOp::Lt, &e)
                .unwrap()
                .expect("next mask applies");
            assert_eq!(
                ctx.floor_cache().bypassed(),
                0,
                "sample hits must keep 272 / 2,176 (12.5%) at or above the cutoff on Floors"
            );
            assert!(ctx.check().is_ok());
        }
    }

    /// When both floor-table growth (1,024 -> 8,192 slots) and low-hit bypass are pending,
    /// sample decoding can enlarge the cursor decode envelope and shrink remaining room
    /// below the half-room growth threshold. Sample validation must compare keys directly
    /// so a later sampled block-start literal with an oversized predecessor does not poison
    /// a mask that commits `Direct`, while still growing and counting the sample when room
    /// stays above the threshold.
    #[test]
    fn pending_growth_and_bypass_sample_crossing_half_room_switches_to_direct() {
        use super::super::ctx::FLOOR_TABLE_FEWEST;
        use super::super::expr::CmpOp;
        use crate::builder::{BuildOptions, VocabCodec};
        use oxrdf::Literal;

        let mut nt = String::new();
        // IDs 0..=14: 15 short simple literals sorting before "m".
        let mut a_terms = Vec::with_capacity(15);
        for i in 0..15usize {
            let term: oxrdf::Term = Literal::new_simple_literal(format!("a{i:02}")).into();
            nt.push_str(&format!("<urn:s> <urn:p> {term} .\n"));
            a_terms.push(term);
        }
        // ID 15: a 1 MiB extension of "m" (`"mxxx...\xFF"` sorts immediately before `"m\xFF"`).
        let huge_term: oxrdf::Term =
            Literal::new_simple_literal(format!("m{}", "x".repeat(1 << 20))).into();
        nt.push_str(&format!("<urn:s> <urn:p> {huge_term} .\n"));
        // ID 16: short simple literal "m" at block start (`16 % FC_BLOCK == 0`).
        let m_term: oxrdf::Term = Literal::new_simple_literal("m").into();
        nt.push_str(&format!("<urn:s> <urn:p> {m_term} .\n"));
        // 64 short simple literals sorting after "m" so Batch 1 marks >= 64 read-again slots
        // in the 1,024-slot table (well above the 1,024 / 32 = 32 eviction threshold).
        let mut n_terms = Vec::with_capacity(64);
        for i in 0..64usize {
            let term: oxrdf::Term = Literal::new_simple_literal(format!("n{i:02}")).into();
            nt.push_str(&format!("<urn:s> <urn:p> {term} .\n"));
            n_terms.push(term);
        }
        // An uncached 1 KiB simple literal with no extension.
        let medium_term: oxrdf::Term =
            Literal::new_simple_literal(format!("y{}", "q".repeat(1023))).into();
        nt.push_str(&format!("<urn:s> <urn:p> {medium_term} .\n"));
        // Cheap distinct simple literals sorting after "y..." to drive low-hit evictions.
        let distinct_count = 3 * STRING_ORDER_ROWS;
        for i in 0..distinct_count {
            nt.push_str(&format!("<urn:s> <urn:p> \"z{i:06}\" .\n"));
        }

        let s = Store::in_memory(StoreOptions {
            build: BuildOptions {
                vocab_codec: Some(VocabCodec::Front),
                ..Default::default()
            },
            ..Default::default()
        });
        s.load(&[Source::from_bytes(
            nt.into_bytes(),
            RdfFormat::NTriples,
            None,
        )])
        .unwrap();
        let snap = s.snapshot();
        assert_eq!(snap.generation.vocab_codec(), VocabCodec::Front);
        let a_ids: Vec<Id> = a_terms
            .iter()
            .map(|t| snap.lookup_term(t).unwrap())
            .collect();
        let huge_id = snap.lookup_term(&huge_term).unwrap();
        let m_id = snap.lookup_term(&m_term).unwrap();
        let n_ids: Vec<Id> = n_terms
            .iter()
            .map(|t| snap.lookup_term(t).unwrap())
            .collect();
        let medium_id = snap.lookup_term(&medium_term).unwrap();
        assert_eq!(huge_id.payload(), 15);
        assert_eq!(m_id.payload(), 16);
        assert!((m_id.payload() as usize).is_multiple_of(crate::vocab::FC_BLOCK));
        let z_ids: Vec<Id> = (0..distinct_count)
            .map(|i| {
                snap.lookup_term(&Literal::new_simple_literal(format!("z{i:06}")).into())
                    .unwrap()
            })
            .collect();
        drop(snap);

        // Sanity check: computing `m`'s floor under a 4 MiB cursor budget fails because
        // decoding its 1 MiB predecessor at ID 15 exceeds the decoding envelope.
        {
            let mut probe_ctx = Ctx::new(s.snapshot());
            probe_ctx.mem_limit = 4 << 20;
            probe_ctx.enable_cursor_accounting().unwrap();
            assert_eq!(probe_ctx.string_floor(m_id), None);
            assert!(
                matches!(
                    probe_ctx.check(),
                    Err(Error::BudgetExceeded(b)) if b.kind == BudgetKind::Memory
                ),
                "computing m's floor must fail when decoding ID 15 under 4 MiB cursor budget"
            );
        }

        // Run in a single-thread pool so `decode_workers()` stays deterministic across hosts
        // and the 1 KiB sample literal always enlarges the decode envelope past the 128 KiB
        // (2 * 64 KiB step) growth threshold.
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(1)
            .build()
            .unwrap();
        pool.install(|| {
            // (a) Sample decoding enlarges the decode envelope across the half-room threshold,
            // so table growth (1,024 -> 8,192) is declined and the mask must commit `Direct`
            // without decoding `m`'s 1 MiB predecessor.
            {
                let mut ctx = Ctx::new(s.snapshot());
                ctx.mem_limit = 4 << 20;
                ctx.enable_cursor_accounting().unwrap();
                ctx.floor_cache().set_first_slots(FLOOR_TABLE_FEWEST);
                let (x, y) = (ctx.var("x"), ctx.var("y"));
                let e = cmp(x, y, CmpOp::Gt);

                // Batch 1: install a 1,024-slot table and mark 64 distinct slots as read-again.
                let mut warm = Table::new(vec![x, y]);
                for i in 0..STRING_ORDER_ROWS {
                    warm.push_row(&[n_ids[i % n_ids.len()], n_ids[(i + 7) % n_ids.len()]]);
                }
                string_order_mask(&ctx, &warm, (x, y), CmpOp::Gt, &e)
                    .unwrap()
                    .expect("warm mask applies");
                assert_eq!(ctx.floor_cache().slots(), FLOOR_TABLE_FEWEST);
                assert_eq!(ctx.floor_cache().bypassed(), 0);

                // Batch 2: 2 * STRING_ORDER_ROWS distinct cheap literals wrap the 1,024-slot
                // table multiple times, evicting all 64 read-again slots (>= 32 growth threshold)
                // while producing 0% hits across 8,192 lookups (>= 2,048 bypass threshold).
                let mut low_hit = Table::new(vec![x, y]);
                for i in 0..STRING_ORDER_ROWS {
                    low_hit.push_row(&[z_ids[2 * i], z_ids[2 * i + 1]]);
                }
                string_order_mask(&ctx, &low_hit, (x, y), CmpOp::Gt, &e)
                    .unwrap()
                    .expect("low-hit mask applies");
                assert_eq!(ctx.floor_cache().slots(), FLOOR_TABLE_FEWEST);
                assert_eq!(ctx.floor_cache().bypassed(), 0);

                // Leave 132 KiB of room: next reservation step for 8,192 slots is 64 KiB,
                // which initially fits `room / 2` (66 KiB).
                let initial_room = 132 << 10;
                let _hold = ctx.charge(ctx.memory_room() - initial_room).unwrap();
                assert_eq!(ctx.memory_room(), initial_room);

                // Batch 3: row 0 decodes the uncached 1 KiB literal, shrinking remaining room
                // below 128 KiB; subsequent sampled pairs compare `m_id` (1 MiB predecessor).
                let mut transition = Table::new(vec![x, y]);
                transition.push_row(&[medium_id, a_ids[0]]);
                for i in 1..STRING_ORDER_ROWS {
                    let rhs = if i % 2 == 0 { a_ids[0] } else { z_ids[0] };
                    transition.push_row(&[m_id, rhs]);
                }
                let mask = string_order_mask(&ctx, &transition, (x, y), CmpOp::Gt, &e)
                    .unwrap()
                    .expect("transition mask applies on Direct");
                let generic =
                    evaluate_mask(&ctx, &transition, std::slice::from_ref(&e), None).unwrap();
                assert_eq!(mask, generic);
                assert!(
                    ctx.memory_room() < (128 << 10),
                    "sample decode must shrink room below 2 * 64 KiB step: room={}",
                    ctx.memory_room()
                );
                assert_eq!(
                    ctx.floor_cache().slots(),
                    FLOOR_TABLE_FEWEST,
                    "growth to 8,192 slots must be declined once room drops below 128 KiB"
                );
                assert_eq!(
                    ctx.floor_cache().bypassed(),
                    1,
                    "mask must transition cleanly to Direct without sticky memory failure"
                );
                assert!(ctx.check().is_ok());
            }

            // (b) When room stays above the half-room threshold, growth (1,024 -> 8,192) still
            // commits `Floors` and the 64 sampled rows count toward the grown table's feedback.
            {
                let mut ctx = Ctx::new(s.snapshot());
                ctx.mem_limit = 4 << 20;
                ctx.enable_cursor_accounting().unwrap();
                ctx.floor_cache().set_first_slots(FLOOR_TABLE_FEWEST);
                let (x, y) = (ctx.var("x"), ctx.var("y"));
                let e = cmp(x, y, CmpOp::Gt);

                let mut warm = Table::new(vec![x, y]);
                for i in 0..STRING_ORDER_ROWS {
                    warm.push_row(&[n_ids[i % n_ids.len()], n_ids[(i + 7) % n_ids.len()]]);
                }
                string_order_mask(&ctx, &warm, (x, y), CmpOp::Gt, &e)
                    .unwrap()
                    .expect("warm mask applies");

                let mut low_hit = Table::new(vec![x, y]);
                for i in 0..STRING_ORDER_ROWS {
                    low_hit.push_row(&[z_ids[2 * i], z_ids[2 * i + 1]]);
                }
                string_order_mask(&ctx, &low_hit, (x, y), CmpOp::Gt, &e)
                    .unwrap()
                    .expect("low-hit mask applies");

                // Hold 132 KiB of room; cheap short literals do not enlarge the envelope, so
                // `begin_floor_mask` grows to 8,192 slots. Put 1,024 distinct base-literal rows
                // (64 sampled + 960 tail = 2,048 lookups) followed by non-base rows so bypass on
                // the next mask proves the 64 sampled rows were counted on the grown table.
                let initial_room = 132 << 10;
                let _hold = ctx.charge(ctx.memory_room() - initial_room).unwrap();
                let non_base_a = Id::from_i64(1).unwrap();
                let non_base_b = Id::from_i64(2).unwrap();
                let mut grown = Table::new(vec![x, y]);
                for i in 0..1_024usize {
                    let base = 2 * STRING_ORDER_ROWS + 2 * i;
                    grown.push_row(&[z_ids[base], z_ids[base + 1]]);
                }
                for _ in 1_024..STRING_ORDER_ROWS {
                    grown.push_row(&[non_base_a, non_base_b]);
                }
                let mask = string_order_mask(&ctx, &grown, (x, y), CmpOp::Gt, &e)
                    .unwrap()
                    .expect("grown mask applies on Floors");
                let generic = evaluate_mask(&ctx, &grown, std::slice::from_ref(&e), None).unwrap();
                assert_eq!(mask, generic);
                assert_eq!(
                    ctx.floor_cache().slots(),
                    FLOOR_TABLE_FEWEST * 8,
                    "table must grow to 8,192 slots when room stays sufficient"
                );
                assert_eq!(ctx.floor_cache().bypassed(), 0);

                // The next mask must enter Direct, proving all 64 sampled rows counted toward
                // the 2,048-lookup threshold on the newly grown 8,192-slot table.
                string_order_mask(&ctx, &warm, (x, y), CmpOp::Gt, &e)
                    .unwrap()
                    .expect("next mask applies");
                assert_eq!(
                    ctx.floor_cache().bypassed(),
                    1,
                    "64 sampled rows + 960 tail rows on grown table must reach 2,048 lookups"
                );
                assert!(ctx.check().is_ok());
            }

            // (c) When room stays at 132 KiB after direct sample validation, a sampled
            // block-start literal whose 1 MiB predecessor cannot fit even before growth
            // must be refused non-stickily during the floor trial so growth is skipped
            // and the mask finishes cleanly on Direct across all eager parts (including
            // the 4th-part boundary at 65,792) and across later periodic probe masks
            // (both when the refused literal appears in the probe's sample and when it
            // appears only in the probe's tail).
            {
                let mut ctx = Ctx::new(s.snapshot());
                ctx.mem_limit = 4 << 20;
                ctx.enable_cursor_accounting().unwrap();
                ctx.floor_cache().set_first_slots(FLOOR_TABLE_FEWEST);
                let (x, y) = (ctx.var("x"), ctx.var("y"));
                let e = cmp(x, y, CmpOp::Gt);

                let ref_ctx = Ctx::new(s.snapshot());
                let (rx, ry) = (ref_ctx.var("x"), ref_ctx.var("y"));
                assert_eq!((rx, ry), (x, y));
                let ref_e = cmp(rx, ry, CmpOp::Gt);

                let mut warm = Table::new(vec![x, y]);
                for i in 0..STRING_ORDER_ROWS {
                    warm.push_row(&[n_ids[i % n_ids.len()], n_ids[(i + 7) % n_ids.len()]]);
                }
                string_order_mask(&ctx, &warm, (x, y), CmpOp::Gt, &e)
                    .unwrap()
                    .expect("warm mask applies");

                let mut low_hit = Table::new(vec![x, y]);
                for i in 0..STRING_ORDER_ROWS {
                    low_hit.push_row(&[z_ids[2 * i], z_ids[2 * i + 1]]);
                }
                string_order_mask(&ctx, &low_hit, (x, y), CmpOp::Gt, &e)
                    .unwrap()
                    .expect("low-hit mask applies");

                let initial_room = 132 << 10;
                let _hold = ctx.charge(ctx.memory_room() - initial_room).unwrap();
                assert_eq!(ctx.memory_room(), initial_room);

                // Cross the 4th-part boundary (`64 -> 16,448 -> 32,896 -> 65,792 -> 65,793`)
                // so a chunked loop that exited bypass on part 4 would retry `m_id` via
                // the sticky floor path.
                let mut transition = Table::new(vec![x, y]);
                for i in 0..65_793usize {
                    let rhs = if i % 2 == 0 { a_ids[0] } else { z_ids[0] };
                    transition.push_row(&[m_id, rhs]);
                }
                let mask = string_order_mask(&ctx, &transition, (x, y), CmpOp::Gt, &e)
                    .unwrap()
                    .expect("transition mask applies on Direct after refused floor trial");
                let generic =
                    evaluate_mask(&ref_ctx, &transition, std::slice::from_ref(&ref_e), None)
                        .unwrap();
                assert_eq!(mask, generic);
                assert_eq!(
                    ctx.floor_cache().slots(),
                    FLOOR_TABLE_FEWEST,
                    "growth to 8,192 slots must be skipped when sample floor trial refuses"
                );
                assert_eq!(
                    ctx.floor_cache().bypassed(),
                    1,
                    "refused floor trial must keep Direct for every part without sticky failure"
                );
                assert!(ctx.check().is_ok());

                // Advance through the remaining bypassed masks (`bypass_streak` 1 -> 2 -> 3)
                // to reach the periodic probe boundary (`FLOOR_BYPASS_PROBE_INTERVAL == 4`).
                let mut short_transition = Table::new(vec![x, y]);
                for i in 0..STRING_ORDER_ROWS {
                    let rhs = if i % 2 == 0 { a_ids[0] } else { z_ids[0] };
                    short_transition.push_row(&[m_id, rhs]);
                }
                let short_generic = evaluate_mask(
                    &ref_ctx,
                    &short_transition,
                    std::slice::from_ref(&ref_e),
                    None,
                )
                .unwrap();
                for expected_bypassed in [2usize, 3] {
                    let mask = string_order_mask(&ctx, &short_transition, (x, y), CmpOp::Gt, &e)
                        .unwrap()
                        .expect("bypassed mask applies on Direct");
                    assert_eq!(mask, short_generic);
                    assert_eq!(ctx.floor_cache().slots(), FLOOR_TABLE_FEWEST);
                    assert_eq!(ctx.floor_cache().bypassed(), expected_bypassed);
                    assert!(ctx.check().is_ok());
                }

                // Periodic probe 1: `m_id` appears in the probe's sample (and across 4 parts).
                // The probe must decline the floor-only refusal non-stickily and stay Direct.
                let probe_sample_mask = string_order_mask(&ctx, &transition, (x, y), CmpOp::Gt, &e)
                    .unwrap()
                    .expect("periodic probe with refused sample floor must succeed on Direct");
                assert_eq!(probe_sample_mask, generic);
                assert_eq!(ctx.floor_cache().slots(), FLOOR_TABLE_FEWEST);
                assert_eq!(
                    ctx.floor_cache().bypassed(),
                    4,
                    "refused periodic probe must reset bypass streak and stay Direct"
                );
                assert!(ctx.check().is_ok());

                // Advance `bypass_streak` 1 -> 2 -> 3 to reach a second periodic probe.
                for expected_bypassed in [5usize, 6] {
                    let mask = string_order_mask(&ctx, &short_transition, (x, y), CmpOp::Gt, &e)
                        .unwrap()
                        .expect("bypassed mask applies on Direct");
                    assert_eq!(mask, short_generic);
                    assert_eq!(ctx.floor_cache().bypassed(), expected_bypassed);
                    assert!(ctx.check().is_ok());
                }

                // Periodic probe 2: sample rows are cheap literals so the probe enters Floors,
                // while tail rows contain `m_id`. FloorMap must decline the floor-only refusal
                // non-stickily and fall back to direct comparison per row.
                let mut probe_tail = Table::new(vec![x, y]);
                for i in 0..STRING_ORDER_ROWS {
                    let lhs = if i < STRING_ORDER_SAMPLE {
                        a_ids[i % a_ids.len()]
                    } else {
                        m_id
                    };
                    let rhs = if i % 2 == 0 { a_ids[0] } else { z_ids[0] };
                    probe_tail.push_row(&[lhs, rhs]);
                }
                let probe_tail_mask = string_order_mask(&ctx, &probe_tail, (x, y), CmpOp::Gt, &e)
                    .unwrap()
                    .expect("periodic probe entering Floors with refused tail floor must succeed");
                let probe_tail_generic =
                    evaluate_mask(&ref_ctx, &probe_tail, std::slice::from_ref(&ref_e), None)
                        .unwrap();
                assert_eq!(probe_tail_mask, probe_tail_generic);
                assert_eq!(
                    ctx.floor_cache().bypassed(),
                    6,
                    "periodic probe with cheap sample enters Floors and falls back per refused row"
                );
                assert!(ctx.check().is_ok());
            }
        });
    }

    /// When both floor-table growth (1,024 -> 8,192 slots) and low-hit bypass are pending,
    /// direct sample validation does not decode a block-start operand's predecessor. Before
    /// spending 64 KiB on optional table growth, the mask must account for the sample's
    /// floor decode: if a 6 KiB predecessor fits in the 132 KiB pre-growth room but would
    /// exceed the 68 KiB left after growth, reserving it first makes growth ineligible so
    /// the mask enters `Direct` and succeeds without a sticky memory failure.
    #[test]
    fn pending_growth_and_bypass_accounts_for_sampled_floor_before_growth() {
        use super::super::ctx::FLOOR_TABLE_FEWEST;
        use super::super::expr::CmpOp;
        use crate::builder::{BuildOptions, VocabCodec};
        use oxrdf::Literal;

        let mut nt = String::new();
        // IDs 0..=14: 15 short simple literals sorting before "l..." and "m".
        let mut a_terms = Vec::with_capacity(15);
        for i in 0..15usize {
            let term: oxrdf::Term = Literal::new_simple_literal(format!("a{i:02}")).into();
            nt.push_str(&format!("<urn:s> <urn:p> {term} .\n"));
            a_terms.push(term);
        }
        // ID 15: a 6 KiB simple literal starting with "l", immediately preceding block-start
        // "m" at ID 16 without extending it.
        let pred_term: oxrdf::Term =
            Literal::new_simple_literal(format!("l{}", "x".repeat(6143))).into();
        nt.push_str(&format!("<urn:s> <urn:p> {pred_term} .\n"));
        // ID 16: short simple literal "m" at block start (`16 % FC_BLOCK == 0`).
        let m_term: oxrdf::Term = Literal::new_simple_literal("m").into();
        nt.push_str(&format!("<urn:s> <urn:p> {m_term} .\n"));
        // 64 short simple literals sorting after "m" to mark >= 64 read-again slots.
        let mut n_terms = Vec::with_capacity(64);
        for i in 0..64usize {
            let term: oxrdf::Term = Literal::new_simple_literal(format!("n{i:02}")).into();
            nt.push_str(&format!("<urn:s> <urn:p> {term} .\n"));
            n_terms.push(term);
        }
        // Cheap distinct simple literals sorting after "n..." to drive low-hit evictions.
        let distinct_count = 3 * STRING_ORDER_ROWS;
        for i in 0..distinct_count {
            nt.push_str(&format!("<urn:s> <urn:p> \"z{i:06}\" .\n"));
        }

        let s = Store::in_memory(StoreOptions {
            build: BuildOptions {
                vocab_codec: Some(VocabCodec::Front),
                ..Default::default()
            },
            ..Default::default()
        });
        s.load(&[Source::from_bytes(
            nt.into_bytes(),
            RdfFormat::NTriples,
            None,
        )])
        .unwrap();
        let snap = s.snapshot();
        assert_eq!(snap.generation.vocab_codec(), VocabCodec::Front);
        let a_ids: Vec<Id> = a_terms
            .iter()
            .map(|t| snap.lookup_term(t).unwrap())
            .collect();
        let pred_id = snap.lookup_term(&pred_term).unwrap();
        let m_id = snap.lookup_term(&m_term).unwrap();
        let n_ids: Vec<Id> = n_terms
            .iter()
            .map(|t| snap.lookup_term(t).unwrap())
            .collect();
        assert_eq!(pred_id.payload(), 15);
        assert_eq!(m_id.payload(), 16);
        assert!((m_id.payload() as usize).is_multiple_of(crate::vocab::FC_BLOCK));
        let z_ids: Vec<Id> = (0..distinct_count)
            .map(|i| {
                snap.lookup_term(&Literal::new_simple_literal(format!("z{i:06}")).into())
                    .unwrap()
            })
            .collect();
        drop(snap);

        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(1)
            .build()
            .unwrap();
        pool.install(|| {
            let initial_room = 132 << 10;
            let growth_step = 64 << 10;

            // Sanity check 1: ID 16's 6 KiB predecessor fits within 132 KiB pre-growth room
            // (under the two-worker allowance) and leaves less than 128 KiB remaining.
            {
                let mut probe = Ctx::new(s.snapshot());
                probe.mem_limit = 4 << 20;
                probe.enable_cursor_accounting().unwrap();
                let _hold = probe.charge(probe.memory_room() - initial_room).unwrap();
                assert_eq!(probe.string_floor(m_id), Some(m_id.payload()));
                assert!(probe.check().is_ok());
                assert!(
                    probe.memory_room() < (2 * growth_step),
                    "decoding 6 KiB predecessor must leave room below 2 * 64 KiB growth threshold: room={}",
                    probe.memory_room()
                );
            }

            // Sanity check 2: after spending 64 KiB on table growth (68 KiB left), decoding
            // ID 16's 6 KiB predecessor exceeds the remaining room.
            {
                let mut probe = Ctx::new(s.snapshot());
                probe.mem_limit = 4 << 20;
                probe.enable_cursor_accounting().unwrap();
                let _hold = probe
                    .charge(probe.memory_room() - (initial_room - growth_step))
                    .unwrap();
                assert_eq!(probe.string_floor(m_id), None);
                assert!(
                    matches!(
                        probe.check(),
                        Err(Error::BudgetExceeded(b)) if b.kind == BudgetKind::Memory
                    ),
                    "decoding 6 KiB predecessor must exceed the 68 KiB left after 64 KiB growth"
                );
            }

            let mut ctx = Ctx::new(s.snapshot());
            ctx.mem_limit = 4 << 20;
            ctx.enable_cursor_accounting().unwrap();
            ctx.floor_cache().set_first_slots(FLOOR_TABLE_FEWEST);
            let (x, y) = (ctx.var("x"), ctx.var("y"));
            let e = cmp(x, y, CmpOp::Gt);

            // Batch 1: install a 1,024-slot table and mark 64 distinct slots as read-again.
            let mut warm = Table::new(vec![x, y]);
            for i in 0..STRING_ORDER_ROWS {
                warm.push_row(&[n_ids[i % n_ids.len()], n_ids[(i + 7) % n_ids.len()]]);
            }
            string_order_mask(&ctx, &warm, (x, y), CmpOp::Gt, &e)
                .unwrap()
                .expect("warm mask applies");
            assert_eq!(ctx.floor_cache().slots(), FLOOR_TABLE_FEWEST);
            assert_eq!(ctx.floor_cache().bypassed(), 0);

            // Batch 2: drive >= 32 read-again evictions and 0% hit rate over 8,192 lookups.
            let mut low_hit = Table::new(vec![x, y]);
            for i in 0..STRING_ORDER_ROWS {
                low_hit.push_row(&[z_ids[2 * i], z_ids[2 * i + 1]]);
            }
            string_order_mask(&ctx, &low_hit, (x, y), CmpOp::Gt, &e)
                .unwrap()
                .expect("low-hit mask applies");
            assert_eq!(ctx.floor_cache().slots(), FLOOR_TABLE_FEWEST);
            assert_eq!(ctx.floor_cache().bypassed(), 0);

            // Leave 132 KiB of room (64 KiB growth step <= 66 KiB half-room).
            let _hold = ctx.charge(ctx.memory_room() - initial_room).unwrap();
            assert_eq!(ctx.memory_room(), initial_room);

            // Batch 3: compare short block-start "m" with short literals from row 0 (no 1 KiB
            // operand). Direct validation fits in the existing short-key envelope, so the mask
            // must account for "m"'s floor decode before spending 64 KiB on optional growth.
            let mut transition = Table::new(vec![x, y]);
            for i in 0..STRING_ORDER_ROWS {
                let rhs = if i % 2 == 0 { a_ids[0] } else { z_ids[0] };
                transition.push_row(&[m_id, rhs]);
            }
            let mask = string_order_mask(&ctx, &transition, (x, y), CmpOp::Gt, &e)
                .unwrap()
                .expect("transition mask must succeed on Direct without sticky memory failure");
            let generic =
                evaluate_mask(&ctx, &transition, std::slice::from_ref(&e), None).unwrap();
            assert_eq!(mask, generic);
            assert_eq!(
                ctx.floor_cache().slots(),
                FLOOR_TABLE_FEWEST,
                "reserving 6 KiB predecessor before growth must make 64 KiB growth ineligible"
            );
            assert_eq!(
                ctx.floor_cache().bypassed(),
                1,
                "mask must enter Direct cleanly without sticky memory failure"
            );
            assert!(ctx.check().is_ok());
        });
    }
}

#[cfg(test)]
#[test]
fn cursor_big_numeric_sort_keys_are_admitted_as_they_are_evaluated() {
    use super::expr::ArithOp;

    let store = crate::store::Store::in_memory(Default::default());
    let mut ctx = Ctx::new(store.snapshot());
    ctx.mem_limit = 360 * 1024;
    let var = ctx.var("v");
    let ctx = std::sync::Arc::new(ctx);
    ctx.attach_owner();

    let mut table = Table::new(vec![var]);
    for i in 1..=512 {
        let id = Id::from_i64(i).unwrap();
        table.push_row(&[id]);
        table.push_row(&[id]);
    }
    let factor = Id::from_i64(100_000_000_000_000_003).unwrap();
    let expr = (0..96).fold(Expr::Var(var), |expr, _| {
        Expr::Arith(Box::new(expr), Box::new(Expr::Const(factor)), ArithOp::Mul)
    });
    let mut report = ExprReport::default();
    let result = key_per_value(&ctx, &table, &expr, &mut report);
    assert!(matches!(result, Err(Error::BudgetExceeded(_))));
    // The initial fixed reservation is about 260 KiB. A one-shot payload charge
    // would fail there, before the ledger had admitted any of the big values.
    assert!(ctx.mem_peak() >= 310 * 1024, "peak: {}", ctx.mem_peak());
    assert_eq!(ctx.live_bytes(), 0);
}

#[cfg(test)]
#[test]
fn cursor_fixed_width_sort_reuses_keys_under_a_tight_budget() {
    use super::expr::ArithOp;

    let store = crate::store::Store::in_memory(Default::default());
    let mut ctx = Ctx::new(store.snapshot());
    ctx.mem_limit = 260 * 1024;
    let var = ctx.var("v");
    let ctx = std::sync::Arc::new(ctx);
    ctx.attach_owner();

    let mut table = Table::new(vec![var]);
    for i in 1..=512 {
        let id = Id::from_i64(i).unwrap();
        table.push_row(&[id]);
        table.push_row(&[id]);
    }
    let expr = Expr::Arith(
        Box::new(Expr::Var(var)),
        Box::new(Expr::Const(Id::from_i64(1).unwrap())),
        ArithOp::Add,
    );
    let mut report = ExprReport::default();
    let keys = key_per_value(&ctx, &table, &expr, &mut report)
        .unwrap()
        .expect("fixed-width keys fit in the cache");
    assert_eq!(keys.vals.len(), 512);
    assert!(matches!(
        keys.get(1023),
        Some(Value::Integer(value)) if i64::from(*value) == 513
    ));
    drop(keys);
    assert_eq!(ctx.live_bytes(), 0);
}

#[cfg(test)]
#[test]
fn cursor_large_sparse_and_uncached_sort_keys_admit_payload_incrementally() {
    use super::expr::ArithOp;

    for cached in [true, false] {
        let store = crate::store::Store::in_memory(Default::default());
        let mut ctx = Ctx::new(store.snapshot());
        ctx.mem_limit = 5 << 20;
        let var = ctx.var("v");
        let ctx = std::sync::Arc::new(ctx);
        ctx.attach_owner();

        let mut table = Table::new(vec![var]);
        for i in 1..=PAR_THRESHOLD as i64 {
            let id = Id::from_i64(i * 16).unwrap();
            table.push_row(&[id]);
            table.push_row(&[id]);
        }
        let factor = Value::from_typed(&"9".repeat(1700), oxrdf::vocab::xsd::INTEGER.as_str());
        let expr = Expr::Arith(
            Box::new(Expr::Var(var)),
            Box::new(Expr::Lit(Id::UNDEF, factor)),
            ArithOp::Mul,
        );
        let mut report = ExprReport::default();
        let err = if cached {
            key_per_value(&ctx, &table, &expr, &mut report).err()
        } else {
            key_column_charged(&ctx, &table, &expr, None).err()
        };
        assert!(
            matches!(err, Some(Error::BudgetExceeded(_))),
            "cached {cached}"
        );
        assert!(
            ctx.mem_peak() >= 4 << 20,
            "cached {cached}, peak {}",
            ctx.mem_peak()
        );
        assert_eq!(ctx.live_bytes(), 0);
    }
}

#[cfg(test)]
#[test]
fn parallel_full_sort_fits_without_a_second_key_column() {
    use super::expr::ArithOp;

    let store = crate::store::Store::in_memory(Default::default());
    let mut ctx = Ctx::new(store.snapshot());
    ctx.opt.expr_cache = false;
    let var = ctx.var("v");
    let rows = 2 * PAR_THRESHOLD;
    let mut table = Table::new(vec![var]);
    for i in (1..=rows as i64).rev() {
        table.push_row(&[Id::from_i64(i).unwrap()]);
    }
    let factor = Value::from_typed(&"9".repeat(1700), oxrdf::vocab::xsd::INTEGER.as_str());
    let expr = Expr::Arith(
        Box::new(Expr::Var(var)),
        Box::new(Expr::Lit(Id::UNDEF, factor)),
        ArithOp::Mul,
    );
    let payload = with_key_evaluator(&ctx, &table, &expr, None, |eval| {
        (0..rows).try_fold(0, |bytes, i| Ok(bytes + key_payload(&eval(i)?)))
    })
    .unwrap();
    let slots = (rows * size_of::<Option<Value>>()) as u64;
    assert!(payload > slots);
    let sort = rows as u64 * 24 + 2 * table.mem_bytes() + 128 + 1024;
    let evaluator = ctx.nvars() as u64 * 16 + 128;
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(4)
        .build()
        .unwrap();
    // The complete sort has room for only half the old scratch column's slots.
    ctx.mem_limit = sort + slots + 128 + evaluator + payload + slots / 2;
    let ctx = ctx.into_shared();
    let sorted = pool
        .install(|| {
            order_by_rows(
                &ctx,
                table.clone(),
                &[(expr.clone(), true)],
                None,
                &mut ExprReport::default(),
            )
        })
        .unwrap();
    assert!(
        sorted.cols[0]
            .iter()
            .map(|id| id.as_i64())
            .eq(1..=rows as i64)
    );
    assert_eq!(ctx.live_bytes(), 0);

    // The column alone has room for the keys and piece metadata. The workers
    // must hand off when the transient allowance occupies the remaining room.
    let mut tight = Ctx::new(store.snapshot());
    assert_eq!(tight.var("v"), var);
    tight.mem_limit = slots + 128 + evaluator + payload + 4 * 128;
    let tight = tight.into_shared();
    let keys = pool
        .install(|| key_column_charged(&tight, &table, &expr, None))
        .unwrap();
    with_key_evaluator(&ctx, &table, &expr, None, |eval| {
        for i in 0..rows {
            assert_eq!(
                order_cmp(keys.get(i).as_ref(), eval(i)?.as_ref()),
                Ordering::Equal
            );
        }
        Ok(())
    })
    .unwrap();
    drop(keys);
    assert_eq!(tight.live_bytes(), 0);
}

#[cfg(test)]
#[test]
fn parallel_full_sort_hands_off_wide_string_keys() {
    use super::expr::Func;
    use spargebra::algebra::Function;

    let (ctx, table) = parallel_full_sort_fixture(2 * PAR_THRESHOLD);
    let var = table.vars[0];
    let expr = Expr::Call(
        Func::Builtin(Function::Concat),
        vec![
            Expr::Call(Func::Builtin(Function::Str), vec![Expr::Var(var)]),
            Expr::Lit(Id::UNDEF, Value::Str("#".repeat(4096).into())),
        ],
    );
    // The final column fits with room for the old piece metadata, but a
    // worker that runs out of room cannot stash its string in the transient.
    let mut expected: Vec<_> = (1..=table.len() as i64).collect();
    expected.sort_by_cached_key(i64::to_string);
    assert_parallel_full_sort_at_budget(ctx, table, expr, 4 * 128, expected);
}

#[cfg(test)]
#[test]
fn parallel_full_sort_releases_piece_metadata_before_finishing_keys() {
    use super::expr::{ArithOp, CmpOp};

    let (ctx, table) = parallel_full_sort_fixture(2 * PAR_THRESHOLD);
    let var = table.vars[0];
    let wide_rows = 32;
    let factor = Value::from_typed(&"9".repeat(1700), oxrdf::vocab::xsd::INTEGER.as_str());
    let expr = Expr::If(
        Box::new(Expr::Cmp(
            Box::new(Expr::Var(var)),
            Box::new(Expr::Const(Id::from_i64(wide_rows).unwrap())),
            CmpOp::Le,
        )),
        Box::new(Expr::Arith(
            Box::new(Expr::Var(var)),
            Box::new(Expr::Lit(Id::UNDEF, factor)),
            ArithOp::Mul,
        )),
        Box::new(Expr::Var(var)),
    );
    // The sparse wide keys leave room to start the parallel evaluation, but
    // finishing the suffix needs every byte remaining after the handoff.
    let expected = (wide_rows + 1..=table.len() as i64).chain(1..=wide_rows);
    assert_parallel_full_sort_at_budget(ctx, table, expr, 0, expected);
}

#[cfg(all(test, feature = "geo"))]
#[test]
fn parallel_full_sort_preserves_wide_geometry_keys_geof() {
    assert_full_sort_with_wide_geometry_key(format!("{}asWKT", crate::geo::vocab::GEOF), vec![]);
}

#[cfg(all(test, feature = "geo"))]
#[test]
fn parallel_full_sort_preserves_wide_geometry_keys_spatialf() {
    let dt = crate::geo::WKT_LITERAL;
    assert_full_sort_with_wide_geometry_key(
        format!("{}transformDatatype", crate::geo::vocab::SPATIALF),
        vec![Expr::Lit(Id::UNDEF, Value::Iri(dt.into()))],
    );
}

#[cfg(all(test, feature = "geo"))]
fn assert_full_sort_with_wide_geometry_key(iri: String, extra_args: Vec<Expr>) {
    use super::expr::Func;
    use crate::geo::{WKT_LITERAL, memo::GeoMemo};
    use spargebra::algebra::Function;

    const KEY_LEN: i64 = 20_000;
    let coordinates: Vec<_> = ["0 0", "1 1"].into_iter().cycle().take(6000).collect();
    let wkt = format!("LINESTRING({})", coordinates.join(", "));
    let geometry = crate::geo::parse(&wkt, WKT_LITERAL).unwrap();
    let geometry_bytes = crate::geo::to_wkt(&geometry).len() as u64 + 64;
    let key_bytes = KEY_LEN as u64 + 64;
    assert!(geometry_bytes > key_bytes);
    let literal =
        oxrdf::Literal::new_typed_literal(wkt, oxrdf::NamedNodeRef::new_unchecked(WKT_LITERAL));

    for threads in [1, 4] {
        let (mut ctx, table) = parallel_full_sort_fixture(2 * PAR_THRESHOLD);
        ctx.geo = GeoMemo::with_budget(0);
        let var = table.vars[0];
        let unbound = ctx.var("unbound");
        let geometry_id = ctx.intern_term(&oxrdf::Term::Literal(literal.clone()));
        assert_ne!(geometry_id, Id::UNDEF);
        let mut args = vec![Expr::Lit(geometry_id, Value::from_literal(&literal))];
        args.extend(extra_args.iter().cloned());
        let first = table.cols[0][0];
        let key = Expr::If(
            Box::new(Expr::SameTerm(
                Box::new(Expr::Var(var)),
                Box::new(Expr::Const(first)),
            )),
            Box::new(Expr::Call(
                Func::Builtin(Function::SubStr),
                vec![
                    Expr::Call(
                        Func::Builtin(Function::Str),
                        vec![Expr::Call(Func::Ext(iri.clone()), args)],
                    ),
                    Expr::Const(Id::from_i64(1).unwrap()),
                    Expr::Const(Id::from_i64(KEY_LEN).unwrap()),
                ],
            )),
            Box::new(Expr::Var(unbound)),
        );
        let rows = table.len();
        let slots = (rows * size_of::<Option<Value>>()) as u64 + 128;
        let sort = rows as u64 * 24 + 2 * table.mem_bytes() + 128 + 1024;
        let evaluator = ctx.nvars() as u64 * 16 + 128;
        let setup = ctx.live_bytes();
        // One serialization and one retained key fit together. Four workers'
        // scratch fits under the key's allowance, but leaves too little room
        // to keep the evaluated key in the column.
        ctx.mem_limit = setup + sort + evaluator + slots + geometry_bytes + key_bytes;
        let ctx = ctx.into_shared();
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(threads)
            .build()
            .unwrap();
        let sorted = pool
            .install(|| {
                order_by_rows(
                    &ctx,
                    table,
                    &[(key, true)],
                    None,
                    &mut ExprReport::default(),
                )
            })
            .unwrap();
        assert_eq!(
            sorted.cols[0].last().copied(),
            Some(first),
            "{threads} threads"
        );
        assert!(
            sorted.cols[0][..rows - 1]
                .iter()
                .map(|id| id.as_i64())
                .eq((1..rows as i64).rev()),
            "{threads} threads"
        );
        assert_eq!(
            ctx.live_bytes(),
            setup + geometry_bytes,
            "{threads} threads"
        );
    }
}

#[cfg(all(test, feature = "geo"))]
#[test]
fn parallel_full_sort_keeps_credit_for_later_geometry_keys() {
    use super::expr::Func;
    use crate::geo::{WKT_LITERAL, memo::GeoMemo};
    use spargebra::algebra::Function;

    const ROWS: usize = 8 * PAR_THRESHOLD;
    const KEY_LEN: i64 = 20_000;
    for threads in [1, 4] {
        let (mut ctx, table) = parallel_full_sort_fixture(ROWS);
        ctx.geo = GeoMemo::with_budget(0);
        let var = table.vars[0];
        let unbound = ctx.var("unbound");
        let wkt_key = |points| {
            let coordinates: Vec<_> = ["0 0", "1 1"].into_iter().cycle().take(points).collect();
            let wkt = format!("LINESTRING({})", coordinates.join(", "));
            let geometry = crate::geo::parse(&wkt, WKT_LITERAL).unwrap();
            let bytes = crate::geo::to_wkt(&geometry).len() as u64 + 64;
            let literal = oxrdf::Literal::new_typed_literal(
                wkt,
                oxrdf::NamedNodeRef::new_unchecked(WKT_LITERAL),
            );
            let id = ctx.intern_term(&oxrdf::Term::Literal(literal.clone()));
            assert_ne!(id, Id::UNDEF);
            let as_wkt = Expr::Call(
                Func::Ext(format!("{}asWKT", crate::geo::vocab::GEOF)),
                vec![Expr::Lit(id, Value::from_literal(&literal))],
            );
            (
                Expr::Call(Func::Builtin(Function::Str), vec![as_wkt]),
                bytes,
            )
        };
        let (first_wkt, first_bytes) = wkt_key(6000);
        let (second_wkt, second_bytes) = wkt_key(12000);
        let first = table.cols[0][0];
        let second = table.cols[0][1];
        let is_row = |id| {
            Box::new(Expr::SameTerm(
                Box::new(Expr::Var(var)),
                Box::new(Expr::Const(id)),
            ))
        };
        let key = Expr::If(
            is_row(first),
            Box::new(Expr::Call(
                Func::Builtin(Function::SubStr),
                vec![
                    first_wkt,
                    Expr::Const(Id::from_i64(1).unwrap()),
                    Expr::Const(Id::from_i64(KEY_LEN).unwrap()),
                ],
            )),
            Box::new(Expr::If(
                is_row(second),
                Box::new(Expr::Call(
                    Func::Builtin(Function::StrLen),
                    vec![second_wkt],
                )),
                Box::new(Expr::Var(unbound)),
            )),
        );
        let slots = (ROWS * size_of::<Option<Value>>()) as u64 + 128;
        let sort = ROWS as u64 * 24 + 2 * table.mem_bytes() + 128 + 1024;
        let evaluator = ctx.nvars() as u64 * 16 + 128;
        let setup = ctx.live_bytes();
        let geometry_bytes = first_bytes + second_bytes;
        let key_bytes = KEY_LEN as u64 + 64;
        // The first key can reserve a growth step, but its unused credit
        // would leave too little for the second WKT serialization.
        let headroom = 4 * (128 + super::bignum::AGG_SUM_PAYLOAD_BYTES);
        ctx.mem_limit = setup + sort + evaluator + slots + geometry_bytes + key_bytes + headroom;
        assert!(ctx.reservation_step() > headroom);
        assert!(ctx.reservation_step() <= headroom + second_bytes);
        let ctx = ctx.into_shared();
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(threads)
            .build()
            .unwrap();
        let sorted = pool
            .install(|| {
                order_by_rows(
                    &ctx,
                    table,
                    &[(key, true)],
                    None,
                    &mut ExprReport::default(),
                )
            })
            .unwrap();
        let (unbound_rows, bound_rows) = sorted.cols[0].split_at(ROWS - 2);
        assert_eq!(bound_rows, &[first, second], "{threads} threads");
        assert!(
            unbound_rows
                .iter()
                .map(|id| id.as_i64())
                .eq((1..=ROWS as i64 - 2).rev()),
            "{threads} threads"
        );
        assert_eq!(
            ctx.live_bytes(),
            setup + geometry_bytes,
            "{threads} threads"
        );
    }
}

#[cfg(test)]
#[cfg(feature = "geo")]
#[test]
fn full_sort_releases_serial_key_slack_before_geometry_column() {
    use super::expr::Func;
    use crate::geo::{WKT_LITERAL, memo::GeoMemo};
    use spargebra::algebra::Function;

    let wkt = format!("LINESTRING({}0 0)", "0 0, 1 1, ".repeat(7000));
    let geometry = crate::geo::parse(&wkt, WKT_LITERAL).unwrap();
    let geometry_bytes = crate::geo::to_wkt(&geometry).len() as u64 + 64;
    let literal =
        oxrdf::Literal::new_typed_literal(wkt, oxrdf::NamedNodeRef::new_unchecked(WKT_LITERAL));
    let text = Value::Str("x".repeat(20_000).into());
    let key_bytes = key_payload(&Some(text.clone()));
    for (rows, threads) in [
        (PAR_THRESHOLD, 1),
        (PAR_THRESHOLD, 4),
        (8 * PAR_THRESHOLD, 1),
    ] {
        let (mut ctx, table) = parallel_full_sort_fixture(rows);
        ctx.geo = GeoMemo::with_budget(0);
        let var = table.vars[0];
        let unbound = ctx.var("unbound");
        let id = ctx.intern_term(&oxrdf::Term::Literal(literal.clone()));
        let [first, second, ..] = table.cols[0][..] else {
            unreachable!("the fixture has at least two rows");
        };
        let key_for = |row, value| {
            Expr::If(
                Box::new(Expr::SameTerm(
                    Box::new(Expr::Var(var)),
                    Box::new(Expr::Const(row)),
                )),
                Box::new(value),
                Box::new(Expr::Var(unbound)),
            )
        };
        let as_wkt = Expr::Call(
            Func::Ext(format!("{}asWKT", crate::geo::vocab::GEOF)),
            vec![Expr::Lit(id, Value::from_literal(&literal))],
        );
        let length = Expr::Call(
            Func::Builtin(Function::StrLen),
            vec![Expr::Call(Func::Builtin(Function::Str), vec![as_wkt])],
        );
        let keys = [
            (key_for(first, Expr::Lit(Id::UNDEF, text.clone())), true),
            (key_for(second, length), true),
        ];
        let slots = (rows * size_of::<Option<Value>>()) as u64 + 128;
        let sort = rows as u64 * 24 + 2 * table.mem_bytes() + 2 * 128 + 1024;
        let evaluator = ctx.nvars() as u64 * 16 + 128;
        let setup = ctx.live_bytes();
        ctx.mem_limit = setup + sort + evaluator + 2 * slots + key_bytes + geometry_bytes;
        assert!(ctx.reservation_step() < geometry_bytes);
        let ctx = ctx.into_shared();
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(threads)
            .build()
            .unwrap();
        let sorted = pool
            .install(|| order_by_rows(&ctx, table, &keys, None, &mut ExprReport::default()))
            .unwrap();
        let (unbound_rows, bound_rows) = sorted.cols[0].split_at(rows - 2);
        assert_eq!(
            bound_rows,
            &[second, first],
            "{rows} rows, {threads} threads"
        );
        assert!(
            unbound_rows
                .iter()
                .map(|id| id.as_i64())
                .eq((1..=rows as i64 - 2).rev()),
            "{rows} rows, {threads} threads"
        );
        assert_eq!(ctx.live_bytes(), setup + geometry_bytes);
    }
}

#[cfg(test)]
fn parallel_full_sort_fixture(rows: usize) -> (Ctx, Table) {
    let store = crate::store::Store::in_memory(Default::default());
    let mut ctx = Ctx::new(store.snapshot());
    ctx.opt.expr_cache = false;
    let var = ctx.var("v");
    let mut table = Table::new(vec![var]);
    for i in (1..=rows as i64).rev() {
        table.push_row(&[Id::from_i64(i).unwrap()]);
    }
    (ctx, table)
}

#[cfg(test)]
fn assert_parallel_full_sort_at_budget(
    mut ctx: Ctx,
    table: Table,
    expr: Expr,
    headroom: u64,
    expected: impl IntoIterator<Item = i64>,
) {
    let rows = table.len();
    let payload = with_key_evaluator(&ctx, &table, &expr, None, |eval| {
        (0..rows).try_fold(0, |bytes, i| Ok(bytes + key_payload(&eval(i)?)))
    })
    .unwrap();
    let slots = (rows * size_of::<Option<Value>>()) as u64;
    let sort = rows as u64 * 24 + 2 * table.mem_bytes() + 128 + 1024;
    let evaluator = ctx.nvars() as u64 * 16 + 128;
    ctx.mem_limit = sort + slots + 128 + evaluator + payload + headroom;
    let ctx = ctx.into_shared();
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(4)
        .build()
        .unwrap();
    let sorted = pool
        .install(|| {
            order_by_rows(
                &ctx,
                table,
                &[(expr, true)],
                None,
                &mut ExprReport::default(),
            )
        })
        .unwrap();
    assert!(sorted.cols[0].iter().map(|id| id.as_i64()).eq(expected));
    assert_eq!(ctx.live_bytes(), 0);
}
