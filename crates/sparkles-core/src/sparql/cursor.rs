//! Fallible pull execution over a captured snapshot. Unsupported subtrees are
//! explicit, lazy materialization barriers; the eager query API is unchanged.

use super::ctx::OwnedCharge;
use super::expr::Expr;
use super::exprcache::Report as ExprReport;
use super::plan::{Kind, Node, RangeSpec, ScanSpec};
use super::table::VarId;
use super::{Ctx, PlanInfo, QueryKind, QueryOptions, QueryResult, Table, Timing, depth, exec};
use crate::error::{Error, Result};
use crate::id::Id;
use crate::index::{Key, pad};
use crate::store::{Chunk, Snapshot};
use oxrdf::Term;
use serde::Serialize;
use std::borrow::Cow;
use std::sync::Arc;
use std::sync::atomic::AtomicU64;
use std::time::Instant;

mod binary;
mod bind;
mod distinct;
mod expand;
mod extsort;
mod filter;
pub mod graph;
mod group;
mod joinspill;
pub use graph::{GraphBatch, GraphCursor, graph_cursor};
mod merge;
mod ripple;
mod topk;
mod walk;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum FallbackPolicy {
    /// Unsupported operators execute eagerly on first demand, under the same limits.
    #[default]
    AllowMaterialization,
    /// Reject any unsupported operator before it executes.
    RejectMaterialization,
}

#[derive(Clone, Debug)]
pub struct CursorOptions {
    pub batch_rows: usize,
    /// Target bytes of IDs per batch; a single wide row may exceed the target.
    pub batch_bytes: usize,
    pub fallback: FallbackPolicy,
}

impl Default for CursorOptions {
    fn default() -> Self {
        Self {
            batch_rows: 4096,
            batch_bytes: 1 << 20,
            fallback: Default::default(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum CursorStatus {
    Open,
    Complete,
    Stopped,
    Failed,
}

impl CursorStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Complete => "complete",
            Self::Stopped => "stopped",
            Self::Failed => "failed",
        }
    }
}

/// Capability and partial runtime information. Incremental production can still
/// retain growing state (for example query-created strings).
///
/// A written node and its operator carry the same `id`, the path of child indexes from
/// the root, as the nodes of an eager plan do (see [`PlanInfo`]). A plan deeper than
/// [`MAX_ID_DEPTH`](super::exec::MAX_ID_DEPTH) levels is written without ids, in plan
/// format 2.
#[derive(Clone, Debug)]
pub struct CursorPlan {
    pub operator: PlanInfo,
    pub materializes: bool,
    pub full_input_before_output: bool,
    pub growing_state: bool,
    /// Whether this operator's runtime counts cover its complete execution.
    pub complete: bool,
    pub reason: Option<String>,
    pub children: Vec<CursorPlan>,
}

impl Serialize for CursorPlan {
    fn serialize<S: serde::Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        if self.depth() <= super::exec::MAX_ID_DEPTH {
            self.write(Some("0"), false, s)
        } else {
            self.write(None, true, s)
        }
    }
}

impl CursorPlan {
    /// The levels of the plan: 1 for a plan of one node.
    pub fn depth(&self) -> usize {
        let mut deepest = 0;
        let mut todo = vec![(self, 1)];
        while let Some((n, d)) = todo.pop() {
            deepest = deepest.max(d);
            todo.extend(n.children.iter().map(|c| (c, d + 1)));
        }
        deepest
    }

    /// Write the node `id` and its children, or the plan without ids from its `root`.
    fn write<S: serde::Serializer>(
        &self,
        id: Option<&str>,
        root: bool,
        s: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        struct Operator<'a>(&'a PlanInfo, Option<&'a str>);
        impl Serialize for Operator<'_> {
            fn serialize<S: serde::Serializer>(
                &self,
                s: S,
            ) -> std::result::Result<S::Ok, S::Error> {
                let mut m = s.serialize_map(None)?;
                self.0.serialize_members(self.1, &mut m)?;
                m.serialize_entry("children", &[] as &[PlanInfo])?;
                m.end()
            }
        }
        struct Child<'a>(&'a CursorPlan, Option<String>);
        impl Serialize for Child<'_> {
            fn serialize<S: serde::Serializer>(
                &self,
                s: S,
            ) -> std::result::Result<S::Ok, S::Error> {
                self.0.write(self.1.as_deref(), false, s)
            }
        }
        // a plan nests up to `MAX_PLAN_DEPTH` levels, as in `PlanWithId`
        stacker::maybe_grow(
            super::exec::PLAN_RED_ZONE,
            super::exec::PLAN_STACK_SEGMENT,
            || {
                let mut m = s.serialize_map(None)?;
                if root {
                    m.serialize_entry("planFormat", &super::exec::PLAN_FORMAT)?;
                }
                if let Some(id) = id {
                    m.serialize_entry("id", id)?;
                }
                m.serialize_entry("operator", &Operator(&self.operator, id))?;
                m.serialize_entry("materializes", &self.materializes)?;
                m.serialize_entry("fullInputBeforeOutput", &self.full_input_before_output)?;
                m.serialize_entry("growingState", &self.growing_state)?;
                m.serialize_entry("complete", &self.complete)?;
                m.serialize_entry("reason", &self.reason)?;
                let children: Vec<Child<'_>> = self
                    .children
                    .iter()
                    .enumerate()
                    .map(|(i, c)| Child(c, id.map(|id| format!("{id}.{i}"))))
                    .collect();
                m.serialize_entry("children", &children)?;
                m.end()
            },
        )
    }

    pub fn has_materialization(&self) -> bool {
        self.materializes || self.children.iter().any(Self::has_materialization)
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CursorStats {
    pub status: CursorStatus,
    pub emitted_rows: u64,
    pub rows_produced: u64,
    pub mem_peak_bytes: u64,
    /// the most the query held of the server's memory pool (`None` without a pool)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub granted_bytes: Option<u64>,
    pub timing: Timing,
    pub error: Option<String>,
}

/// An immutable batch. Keeping it alive keeps its capacity on the query's budget
/// and pins the snapshot/local vocabulary needed to resolve its private IDs.
pub struct QueryBatch {
    buffer: BatchBuffer,
    variables: Arc<Vec<String>>,
    ctx: Arc<Ctx>,
}

enum BatchBuffer {
    Owned(Buffer),
    Shared(SharedBatch),
}

struct SharedBatch {
    block: crate::index::Block,
    columns: [usize; 4],
    start: usize,
    end: usize,
    _block_charge: Arc<OwnedCharge>,
    _charge: OwnedCharge,
}

enum SharedAttempt {
    Unsupported,
    Ready(Option<SharedBatch>),
}

impl QueryBatch {
    pub fn variables(&self) -> &[String] {
        &self.variables
    }
    pub fn len(&self) -> usize {
        match &self.buffer {
            BatchBuffer::Owned(buffer) => buffer.table.len(),
            BatchBuffer::Shared(batch) => batch.end - batch.start,
        }
    }
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
    pub fn width(&self) -> usize {
        self.variables.len()
    }

    /// Decode one cell. UNDEF is None; invalid coordinates are an error. Returned
    /// owned terms are application memory after this call.
    pub fn term(&self, row: usize, column: usize) -> Result<Option<Term>> {
        if row >= self.len() || column >= self.width() {
            return Err(Error::invalid("query batch cell is out of bounds"));
        }
        let id = self.id(row, column);
        self.ctx.batch_term(id)
    }

    /// The term of a key that [`QueryBatch::key`] gave for a cell of this batch, decoded
    /// without the checks against the query's memory budget, for a caller that drops the
    /// term at once, such as an encoder that writes it out. The charges of
    /// [`QueryBatch::term`] for the term and its vocabulary block's scratch end with the
    /// call, so they only refuse a term that does not fit in what remains of the budget,
    /// and eager execution decodes its results the same way.
    pub fn transient_term(&self, key: u64) -> Option<Term> {
        self.ctx.transient_batch_term(Id(key))
    }

    /// A key of the cell's term that is the same for every cell of the query that holds
    /// the same internal id, so that a writer can encode each term once without decoding
    /// it first. UNDEF is None. Two keys can still name equal terms.
    pub fn key(&self, row: usize, column: usize) -> Result<Option<u64>> {
        if row >= self.len() || column >= self.width() {
            return Err(Error::invalid("query batch cell is out of bounds"));
        }
        let id = self.id(row, column);
        Ok((!id.is_undef()).then_some(id.0))
    }

    pub fn row(&self, row: usize) -> Result<Vec<Option<Term>>> {
        if row >= self.len() {
            return Err(Error::invalid("query batch row is out of bounds"));
        }
        (0..self.width()).map(|c| self.term(row, c)).collect()
    }

    /// [`QueryBatch::term`] for a writer that has reserved [`QueryBatch::cell_bytes`] for
    /// the cell, through [`QueryBatch::row_bytes`] or [`DecodedBatch::row_bytes`].
    pub(super) fn reserved_term(&self, row: usize, column: usize) -> Result<Option<Term>> {
        if row >= self.len() || column >= self.width() {
            return Err(Error::invalid("query batch cell is out of bounds"));
        }
        self.ctx.reserved_batch_term(self.id(row, column))
    }

    /// [`QueryBatch::term`] for a writer that holds one reservation for its row: `cover`
    /// is asked for the cell's [`QueryBatch::cell_bytes`] before the term is built, and
    /// the key is decoded once.
    pub(super) fn covered_term(
        &self,
        row: usize,
        column: usize,
        cover: impl FnMut(u64) -> Result<()>,
    ) -> Result<Option<Term>> {
        if row >= self.len() || column >= self.width() {
            return Err(Error::invalid("query batch cell is out of bounds"));
        }
        self.ctx.covered_batch_term(self.id(row, column), cover)
    }

    pub(super) fn row_bytes(&self, row: usize) -> Result<u64> {
        (0..self.width())
            .map(|column| self.ctx.decoded_bytes(self.id(row, column)))
            .try_fold(128u64, |total, bytes| Ok(total.saturating_add(bytes?)))
    }

    pub(super) fn cell_bytes(&self, row: usize, column: usize) -> Result<u64> {
        self.ctx.decoded_bytes(self.id(row, column))
    }

    fn id(&self, row: usize, column: usize) -> Id {
        match &self.buffer {
            BatchBuffer::Owned(buffer) => buffer.table.get(row, column),
            BatchBuffer::Shared(batch) => {
                Id(batch.block.cols[batch.columns[column]][batch.start + row])
            }
        }
    }

    fn owned_table(&self) -> &Table {
        let BatchBuffer::Owned(buffer) = &self.buffer else {
            unreachable!("graph inputs use owned ID batches")
        };
        &buffer.table
    }

    /// Optional writer cache: decode each selected base-vocabulary key once in
    /// sorted order. Its terms and construction scratch stay on this query's
    /// budget. A refused cache is discarded before returning the row-wise path.
    pub(super) fn decoded(&self) -> Result<Option<DecodedBatch<'_>>> {
        Ok(self
            .term_cache()?
            .map(|cache| DecodedBatch { batch: self, cache }))
    }

    /// The terms of [`QueryBatch::decoded`] without the borrow of the batch, so that
    /// another thread can build them while the writer serializes the batch before it.
    pub(super) fn term_cache(&self) -> Result<Option<TermCache>> {
        match TermCache::new(self) {
            Ok(cache) => Ok(Some(cache)),
            Err(Error::BudgetExceeded(b)) if b.kind == crate::error::BudgetKind::Memory => Ok(None),
            Err(error) => Err(error),
        }
    }
}

/// The smallest step in which a decoded batch reserves memory for its terms.
const TERM_RESERVE: u64 = 64 << 10;
pub(super) struct DecodedBatch<'a> {
    batch: &'a QueryBatch,
    cache: TermCache,
}

/// The decoded base-vocabulary terms of one batch, each decoded once in key order, and
/// the term of each cell. It holds its reservation until it is dropped.
pub(super) struct TermCache {
    terms: Vec<Term>,
    slots: Vec<u32>,
    _charge: OwnedCharge,
}

impl TermCache {
    fn new(batch: &QueryBatch) -> Result<Self> {
        let ctx = &batch.ctx;
        ctx.check()?;
        let cells = batch
            .len()
            .checked_mul(batch.width())
            .filter(|&n| n < u32::MAX as usize)
            .ok_or_else(|| Error::invalid("query batch has too many cells"))?;
        // Pairs, slots, unique IDs, term capacity and row adapters coexist during
        // construction. Reserve before allocating, including reconstruction.
        let mut held = (cells as u64).saturating_mul(128).saturating_add(1024);
        let mut charge = OwnedCharge::new(ctx, held)?;
        let mut pairs = Vec::with_capacity(cells);
        for column in 0..batch.width() {
            for row in 0..batch.len() {
                if row % 1024 == 0 {
                    ctx.check()?;
                }
                let id = batch.id(row, column);
                if id.tag() == crate::id::Tag::Vocab {
                    pairs.push((id.payload(), (column * batch.len() + row) as u32));
                }
            }
        }
        // A column read in index order is often already ascending, so the pairs are a
        // few sorted runs, which the run-detecting stable sort merges in near-linear
        // time. The pairs are distinct, so its order is that of an unstable sort.
        pairs.sort();
        ctx.check()?;
        let mut slots = vec![u32::MAX; cells];
        let mut ids = Vec::with_capacity(pairs.len());
        for (i, &(id, cell)) in pairs.iter().enumerate() {
            if i % 1024 == 0 {
                ctx.check()?;
            }
            if ids.last() != Some(&id) {
                ids.push(id);
            }
            slots[cell as usize] = (ids.len() - 1) as u32;
        }
        let mut terms = Vec::with_capacity(ids.len());
        let scratch = ctx.charge(0)?;
        let mut reserved = 0;
        // Terms are reserved ahead in chunks of at least TERM_RESERVE bytes. A cursor's
        // charge goes through the memory ledger, and one reservation per term cost a
        // large native JSON answer about 15% of its time. The reservation still comes
        // before each term's allocation and overstates the batch by under one chunk.
        // With under one chunk of budget left, a term reserves its exact size instead,
        // so the cache is declined only when its terms do not fit.
        let mut ahead = 0u64;
        ctx.snap.generation.vocab.get_sorted_checked(
            &ids,
            |need| {
                ctx.check()?;
                let bytes = (need as u64).saturating_mul(2);
                scratch.add(bytes.saturating_sub(reserved))?;
                reserved = bytes;
                Ok::<(), Error>(())
            },
            |_, key| {
                if terms.len() % 1024 == 0 {
                    ctx.check()?;
                }
                let bytes = (key.len() as u64).saturating_mul(8).saturating_add(128);
                if bytes > ahead {
                    let more = bytes.max(TERM_RESERVE);
                    match charge.resize(held.saturating_add(more)) {
                        Ok(()) => {
                            held = held.saturating_add(more);
                            ahead = ahead.saturating_add(more);
                        }
                        // A refused charge leaves the ledger unchanged, so this term can
                        // still reserve what the leftover of the last chunk lacks.
                        Err(Error::BudgetExceeded(b))
                            if b.kind == crate::error::BudgetKind::Memory && more > bytes =>
                        {
                            charge.resize(held.saturating_add(bytes - ahead))?;
                            held = held.saturating_add(bytes - ahead);
                            ahead = bytes;
                        }
                        Err(e) => return Err(e),
                    }
                }
                ahead -= bytes;
                terms.push(crate::id::key_to_term(key));
                Ok::<(), Error>(())
            },
        )?;
        Ok(Self {
            terms,
            slots,
            _charge: charge,
        })
    }
}

impl<'a> DecodedBatch<'a> {
    /// A batch with the terms that [`QueryBatch::term_cache`] built for it.
    pub(super) fn new(batch: &'a QueryBatch, cache: TermCache) -> Self {
        Self { batch, cache }
    }

    fn cached(&self, row: usize, column: usize) -> Option<&Term> {
        self.cache
            .terms
            .get(self.cache.slots[column * self.batch.len() + row] as usize)
    }

    pub(super) fn term(&self, row: usize, column: usize) -> Result<Option<Cow<'_, Term>>> {
        match self.cached(row, column) {
            Some(term) => Ok(Some(Cow::Borrowed(term))),
            None => Ok(self.batch.reserved_term(row, column)?.map(Cow::Owned)),
        }
    }

    pub(super) fn cell_bytes(&self, row: usize, column: usize) -> Result<u64> {
        if self.cached(row, column).is_some() {
            Ok(0)
        } else {
            self.batch.cell_bytes(row, column)
        }
    }

    pub(super) fn row_bytes(&self, row: usize) -> Result<u64> {
        (0..self.batch.width()).try_fold(0u64, |total, col| {
            Ok(total.saturating_add(self.cell_bytes(row, col)?))
        })
    }
}

struct Buffer {
    table: Table,
    charge: OwnedCharge,
}

fn capacity_bytes(table: &Table) -> u64 {
    let columns = table
        .cols
        .iter()
        .map(|c| c.capacity() as u64 * 8)
        .sum::<u64>();
    columns
        .max(table.len as u64 * 8 * u64::from(table.width() == 0))
        .saturating_add(table.vars.capacity() as u64 * 4)
        .saturating_add(table.sorted.capacity() as u64 * 4)
        .saturating_add(table.cols.capacity() as u64 * 24)
        .saturating_add(128)
}

impl Buffer {
    fn new(ctx: &Arc<Ctx>, vars: &[VarId], rows: usize) -> Result<Self> {
        let bytes = super::ctx::table_bytes(rows, vars.len())
            .saturating_add(vars.len() as u64 * 40)
            .saturating_add(128);
        let charge = OwnedCharge::new(ctx, bytes)?;
        let mut table = Table::new(vars.to_vec());
        for col in &mut table.cols {
            col.reserve_exact(rows);
        }
        let mut buffer = Self { table, charge };
        buffer.reconcile()?;
        Ok(buffer)
    }

    fn reconcile(&mut self) -> Result<()> {
        self.charge.resize(capacity_bytes(&self.table))
    }

    fn set_order(&mut self, order: &[VarId]) -> Result<()> {
        if order.len() > self.table.sorted.capacity() {
            // The old backing remains live while the replacement is cloned.
            self.charge
                .resize(capacity_bytes(&self.table).saturating_add(order.len() as u64 * 4))?;
            self.table.sorted = order.to_vec();
            self.reconcile()?;
        } else {
            self.table.sorted.clear();
            self.table.sorted.extend_from_slice(order);
        }
        Ok(())
    }

    fn project(&mut self, vars: &[VarId]) -> Result<()> {
        if self.table.vars == vars {
            let sorted = self
                .table
                .sorted
                .iter()
                .take_while(|v| vars.contains(v))
                .count();
            self.table.sorted.truncate(sorted);
            return Ok(());
        }
        // Missing columns allocate UNDEF; preserve the input capacity while doing so.
        let extra = vars
            .iter()
            .filter(|v| self.table.col_of(**v).is_none())
            .count();
        self.charge.resize(
            capacity_bytes(&self.table)
                .saturating_add(self.table.len as u64 * extra as u64 * 8)
                .saturating_add(vars.len() as u64 * 40),
        )?;
        self.table = std::mem::take(&mut self.table).project(vars);
        self.reconcile()
    }

    fn slice_in_place(&mut self, offset: usize, rows: usize) -> Result<()> {
        let start = offset.min(self.table.len);
        let len = rows.min(self.table.len - start);
        for col in &mut self.table.cols {
            col.copy_within(start..start + len, 0);
            col.truncate(len);
        }
        self.table.len = len;
        self.reconcile()
    }
}

/// Sequential, snapshot-owning SELECT execution. Dropping/closing it releases
/// production state without setting a caller's shared cancellation flag.
pub struct QueryCursor {
    ctx: Arc<Ctx>,
    root: Option<Operator>,
    plan: CursorPlan,
    variables: Arc<Vec<String>>,
    vars: Vec<VarId>,
    bound: Vec<(VarId, Id)>,
    options: CursorOptions,
    status: CursorStatus,
    failure: Option<String>,
    emitted: u64,
    started: Instant,
    ended: Option<Instant>,
    timing: Timing,
    depth: usize,
    shared: bool,
    _plan_charge: OwnedCharge,
}

/// Open an opt-in SELECT cursor. Parse/plan errors and strict fallback rejection
/// occur before any operator produces results.
pub fn select_cursor(
    snapshot: Arc<Snapshot>,
    query: &str,
    options: &QueryOptions,
    cursor: &CursorOptions,
) -> Result<QueryCursor> {
    open_pattern_cursor(snapshot, query, options, cursor, Some(QueryKind::Select))
        .map(|(c, _, _)| c)
}

fn open_pattern_cursor(
    snapshot: Arc<Snapshot>,
    query: &str,
    options: &QueryOptions,
    cursor: &CursorOptions,
    expected: Option<QueryKind>,
) -> Result<(QueryCursor, Option<graph::Spec>, QueryKind)> {
    if cursor.batch_rows == 0 || cursor.batch_bytes == 0 {
        return Err(Error::invalid(
            "cursor batch rows and bytes must be positive",
        ));
    }
    let started = Instant::now();
    // A cursor can be opened on a smaller application thread than the eager
    // executor's usual worker. Protect parsing and destruction of its algebra,
    // not just planning/pulling. The parser still applies its own nesting limits.
    let parser_depth = spargebra::nesting::measure(query)
        .depth
        .min(depth::MAX_ALGEBRA_DEPTH);
    depth::with_stack(parser_depth, || {
        let parsed = super::parse_query(query, options.base_iri.as_deref(), &options.prefixes)?;
        let kind = match &parsed {
            spargebra::Query::Select { .. } => QueryKind::Select,
            spargebra::Query::Ask { .. } => QueryKind::Ask,
            spargebra::Query::Construct { .. } => QueryKind::Construct,
            spargebra::Query::Describe { .. } => QueryKind::Describe,
        };
        if expected.is_some_and(|expected| {
            kind != expected && !(expected == QueryKind::Construct && kind == QueryKind::Describe)
        }) {
            return Err(Error::Unsupported(format!(
                "a {expected:?} cursor requires a {expected:?} query"
            )));
        }
        let parse_ms = started.elapsed().as_secs_f64() * 1000.0;
        let levels = depth::check_query(&parsed)?;
        depth::with_stack(levels, || {
            let planning = Instant::now();
            let prepared = super::prepare_query(snapshot, &parsed, options, Some(started), None)?;
            let plan_ms = planning.elapsed().as_secs_f64() * 1000.0;
            let levels = levels.max(prepared.plan_depth);
            from_prepared(
                prepared, &parsed, query, options, cursor, started, parse_ms, plan_ms, levels, kind,
            )
        })
    })
}

/// Build execution state from the same prepared semantic query.
#[allow(clippy::too_many_arguments)]
fn from_prepared(
    prepared: super::PreparedQuery,
    parsed: &spargebra::Query,
    query: &str,
    options: &QueryOptions,
    cursor: &CursorOptions,
    started: Instant,
    parse_ms: f64,
    plan_ms: f64,
    levels: usize,
    kind: QueryKind,
) -> Result<(QueryCursor, Option<graph::Spec>, QueryKind)> {
    let ctx = prepared.ctx;
    let mut vars =
        super::project_vars(&prepared.pattern, &ctx).unwrap_or_else(|| prepared.node.vars.clone());
    let names = vars.iter().map(|&v| ctx.var_name(v)).collect::<Vec<_>>();
    let order =
        super::select_star_columns(query, &names).unwrap_or_else(|| (0..names.len()).collect());
    vars = order.iter().map(|&i| vars[i]).collect();
    let variables = Arc::new(order.iter().map(|&i| names[i].clone()).collect());
    let bytes = plan_bytes(&prepared.node).saturating_add(query.len() as u64 * 16);
    let charge = OwnedCharge::new(&ctx, bytes)?;
    let spec = graph::Spec::from_parsed(parsed, options, cursor.fallback)?;
    let root = Operator::build_for(&ctx, prepared.node, cursor.fallback, false)?;
    let plan = root.describe(&ctx);
    ctx.check()?;
    let shared = kind == QueryKind::Select && prepared.bound.is_empty();
    Ok((
        QueryCursor {
            ctx,
            root: Some(root),
            plan,
            variables,
            vars,
            bound: prepared.bound,
            options: cursor.clone(),
            status: CursorStatus::Open,
            failure: None,
            emitted: 0,
            started,
            ended: None,
            timing: Timing {
                parse_ms,
                plan_ms,
                ..Default::default()
            },
            depth: levels,
            shared,
            _plan_charge: charge,
        },
        spec,
        kind,
    ))
}

pub fn ask_streaming(
    snapshot: Arc<Snapshot>,
    query: &str,
    options: &QueryOptions,
    cursor: &CursorOptions,
) -> Result<QueryResult> {
    let (cursor, _, _) =
        open_pattern_cursor(snapshot, query, options, cursor, Some(QueryKind::Ask))?;
    finish_ask(cursor)
}

fn finish_ask(mut cursor: QueryCursor) -> Result<QueryResult> {
    let boolean = cursor.next_batch()?.is_some_and(|batch| !batch.is_empty());
    Ok(finish_ask_result(cursor, boolean))
}

fn finish_ask_with_plan(
    mut cursor: QueryCursor,
) -> std::result::Result<QueryResult, Box<super::Failed>> {
    let boolean = match cursor.next_batch() {
        Ok(batch) => batch.is_some_and(|batch| !batch.is_empty()),
        Err(error) => {
            fn incomplete(plan: &mut CursorPlan) {
                if !plan.complete && plan.operator.fidelity().skipped.is_none() {
                    plan.operator.fidelity_mut().incomplete = true;
                }
                plan.children.iter_mut().for_each(incomplete);
            }
            let plan = depth::with_stack(cursor.depth, || {
                incomplete(&mut cursor.plan);
                cursor.plan_info()
            });
            return Err(Box::new(super::Failed {
                error,
                plan: Some(plan),
            }));
        }
    };
    Ok(finish_ask_result(cursor, boolean))
}

fn finish_ask_result(mut cursor: QueryCursor, boolean: bool) -> QueryResult {
    cursor.close();
    let stats = cursor.stats();
    let plan = depth::with_stack(cursor.depth, || cursor.plan_info());
    QueryResult {
        kind: QueryKind::Ask,
        vars: Vec::new(),
        table: Table::default(),
        boolean,
        triples: Vec::new(),
        quads: Vec::new(),
        plan,
        timing: stats.timing,
        mem_peak_bytes: stats.mem_peak_bytes,
        rows_produced: stats.rows_produced,
        describe_truncated: false,
        ctx: cursor.ctx.clone(),
    }
}

/// A collected result with depth-protected metadata and destruction.
pub struct MaterializedResult {
    result: Option<QueryResult>,
    depth: usize,
}
impl MaterializedResult {
    pub fn value(&self) -> bool {
        self.result().boolean
    }
    pub fn result(&self) -> &QueryResult {
        self.result.as_ref().expect("live materialized result")
    }
    pub fn plan_json(&self) -> Result<String> {
        depth::with_stack(self.depth, || {
            serde_json::to_string(&self.result().plan).map_err(|e| Error::invalid(e.to_string()))
        })
    }
    /// The result itself, as the eager query APIs return it. Its plan is then dropped
    /// by its new owner, as theirs is.
    pub fn into_result(mut self) -> QueryResult {
        self.result.take().expect("live materialized result")
    }
}
impl Drop for MaterializedResult {
    fn drop(&mut self) {
        depth::with_stack(self.depth, || drop(self.result.take()));
    }
}

/// The completed boolean query result.
pub type AskResult = MaterializedResult;

/// How a query runs. [`ExecutionMode::Auto`] is the default: it streams the plans that
/// measured faster as cursors and runs every other plan eagerly.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ExecutionMode {
    Eager,
    Streaming,
    /// Use pull execution for immutable scans of an estimated thousand rows or more,
    /// groupings of large inputs, and plans that would outgrow memory when they may
    /// spill. Run other plans eagerly.
    #[default]
    Auto,
}

/// The estimated rows from which automatic execution streams a plain scan. A cursor
/// writes a scan's results faster than eager execution from about a hundred rows on, at
/// one Rayon thread and at twelve, and costs a query of fewer rows a microsecond or two.
pub(super) const AUTO_SCAN_ROWS: f64 = 1_000.0;

/// Production-allocator admission of plans over an immutable snapshot. Automatic
/// execution streams plain scans from [`AUTO_SCAN_ROWS`] estimated rows, groupings from
/// [`AUTO_GROUP_ROWS`] estimated input rows, and any plan of cursor operators that it
/// expects to outgrow half its memory budget when the query may spill. Other plans run
/// eagerly.
///
/// An OPTIONAL count ran eagerly from 2026-10-17 because the cursor's branching merge
/// count took up to 1.23 times as long as eager execution on AMD hosts. The cursor's
/// count is now branch-free and measured 0.79 to 0.88 times eager execution's time at
/// 1.05M and 10.5M, so it streams as a grouping again. It holds 2.2 MB where eager
/// execution holds 19.8 MB at 10.5M.
pub(super) fn auto_streaming(
    ctx: &Ctx,
    node: &Node,
    kind: QueryKind,
    options: &CursorOptions,
) -> bool {
    fn scan(ctx: &Ctx, node: &Node) -> bool {
        match &node.kind {
            Kind::Scan(spec) => {
                !spec.dedup
                    && spec.eqs.is_empty()
                    && (matches!(spec.graph, super::plan::GraphFilter::All)
                        || spec
                            .prefix
                            .get(spec.graph_col)
                            .is_some_and(|g| spec.graph.accepts(*g))
                        || (ctx.snap.generation.stats.graphs.len() == 1
                            && spec.graph.accepts(ctx.snap.generation.stats.graphs[0].0)))
            }
            Kind::Project(_) | Kind::Slice { offset: 0, .. } if node.children.len() == 1 => {
                scan(ctx, &node.children[0])
            }
            _ => false,
        }
    }
    if kind == QueryKind::Ask {
        return filtered_ask_admitted(ctx, node);
    }
    // A plan expected to pass the row budgets stays eager, so that it fails before the
    // first byte of output with an error status rather than partway through a stream.
    if node.est > ctx.max_rows as f64 || node.est > ctx.max_rows_produced as f64 {
        return false;
    }
    if spill_admitted(ctx, node, kind) || memory_admitted(ctx, node, kind, options) {
        return true;
    }
    let scanned = node.est.is_finite() && node.est >= AUTO_SCAN_ROWS && scan(ctx, node);
    if !(scanned || grouped(ctx, node)) {
        return false;
    }
    let headroom = cursor_headroom(ctx, node, options);
    kind == QueryKind::Select
        && ctx.snap.delta.is_empty()
        && batches_admitted(options)
        && headroom < u64::MAX
        && ctx.memory_remaining() >= headroom
}

/// Whether the cursor's batches are large enough for automatic admission to pay off.
fn batches_admitted(options: &CursorOptions) -> bool {
    options.batch_rows >= 4096 && options.batch_bytes >= 4096 * 32
}

/// The memory a cursor over `node` needs besides the state its operators keep: one
/// index block of input, one batch of output and the query's variables.
fn cursor_headroom(ctx: &Ctx, node: &Node, options: &CursorOptions) -> u64 {
    let cap = options
        .batch_rows
        .min(options.batch_bytes / node.vars.len().max(1).saturating_mul(8));
    fn width(node: &Node) -> usize {
        node.children
            .iter()
            .map(width)
            .fold(node.vars.len(), usize::max)
    }
    // A projection can be narrower than its scan. Admission must leave room for the
    // scan's input batches as well as the result.
    let row_bytes = (width(node) as u64).saturating_mul(8).saturating_add(16);
    (crate::index::BLOCK_ROWS as u64 * 32 + 512)
        .saturating_add((cap as u64).saturating_mul(row_bytes))
        .saturating_add((ctx.nvars() as u64).saturating_mul(16))
        .saturating_add(4096)
}

/// An ASK with a pure equality filter can stop at its first matching pair instead
/// of materializing an over-budget Cartesian candidate table. Keep the ordinary
/// path for cheap queries and restrict this policy to the measured native shape.
fn filtered_ask_admitted(ctx: &Ctx, node: &Node) -> bool {
    use super::plan::JoinAlgo;

    if !ctx.snap.delta.is_empty() || ctx.calls_extensions {
        return false;
    }
    let Kind::Slice {
        offset: 0,
        limit: Some(1),
    } = node.kind
    else {
        return false;
    };
    let Some(mut filtered) = node.children.first() else {
        return false;
    };
    // The planner may project the ASK pattern's in-scope variables. Bound this
    // transparent wrapper walk before doing any expression or subtree inspection.
    for _ in 0..8 {
        if !matches!(filtered.kind, Kind::Project(_)) || filtered.children.len() != 1 {
            break;
        }
        filtered = &filtered.children[0];
    }
    // A keyed join reads its inputs alternately under ASK (see `ripple`), so the first
    // matching pair ends both. Small inputs are joined eagerly at less fixed cost.
    if let Kind::KeyedJoin { .. } = filtered.kind {
        return filtered.children.len() == 2
            && filtered
                .children
                .iter()
                .any(|c| c.est.is_finite() && c.est >= AUTO_SCAN_ROWS)
            && native(filtered);
    }
    let Kind::Filter(expressions) = &filtered.kind else {
        return false;
    };
    let [Expr::Eq(left, right)] = expressions.as_slice() else {
        return false;
    };
    let (Expr::Var(left), Expr::Var(right)) = (&**left, &**right) else {
        return false;
    };
    let [product] = filtered.children.as_slice() else {
        return false;
    };
    let Kind::Join {
        algo: JoinAlgo::Cross,
        ..
    } = product.kind
    else {
        return false;
    };
    let [a, b] = product.children.as_slice() else {
        return false;
    };
    if !((a.certain.contains(left) && b.certain.contains(right))
        || (a.certain.contains(right) && b.certain.contains(left)))
    {
        return false;
    }
    if !a.est.is_finite() || !b.est.is_finite() || a.est < 0.0 || b.est < 0.0 {
        return false;
    }
    let rows = product.est;
    let bytes = rows * (product.vars.len().max(1) * 8) as f64;
    if !rows.is_finite()
        || rows <= 0.0
        || !bytes.is_finite()
        || (rows <= ctx.max_rows as f64 && bytes <= ctx.memory_remaining() as f64)
    {
        return false;
    }
    // Only exact scan counts prove that run_limited's first 64-row prefix is
    // cheap. Repeated-variable and graph/dedup estimates can undercount its work.
    fn exact_scan(ctx: &Ctx, node: &Node) -> bool {
        let Kind::Scan(scan) = &node.kind else {
            return false;
        };
        scan.eqs.is_empty()
            && !scan.dedup
            && (matches!(scan.graph, super::plan::GraphFilter::All)
                || scan
                    .prefix
                    .get(scan.graph_col)
                    .is_some_and(|g| scan.graph.accepts(*g))
                || (ctx.snap.generation.stats.graphs.len() == 1
                    && scan.graph.accepts(ctx.snap.generation.stats.graphs[0].0)))
    }
    if exact_scan(ctx, a) && exact_scan(ctx, b) {
        let (small, large) = if a.est <= b.est { (a, b) } else { (b, a) };
        let prefix = large.est.min(64.0);
        let candidates = small.est * prefix;
        let held =
            small.est * (small.vars.len() * 8) as f64 + prefix * (large.vars.len() * 8) as f64;
        let bytes = candidates * ((product.vars.len() + 1) * 8) as f64 + held;
        if candidates <= ctx.max_rows as f64 && bytes <= ctx.memory_remaining() as f64 {
            return false;
        }
    }
    // No effects, expressions or eager fallbacks are hidden below the equality.
    // The count also bounds recursion and work on large rejected plans.
    fn plain(node: &Node, remaining: &mut usize) -> bool {
        if *remaining == 0 {
            return false;
        }
        *remaining -= 1;
        let shape = match &node.kind {
            Kind::Scan(_) | Kind::Values(_) | Kind::Empty => node.children.is_empty(),
            Kind::Join { .. } => node.children.len() == 2,
            Kind::Project(_) | Kind::Sort(_) => node.children.len() == 1,
            _ => false,
        };
        shape && node.children.iter().all(|child| plain(child, remaining))
    }
    plain(product, &mut 64) && native(product)
}

/// Only an admitted pure ASK product is oriented: build the same smaller input
/// that eager limited execution collects, while streaming the larger one. Other
/// cursor and eager plans retain their original children and variable order.
/// Any other shape, which a future cursor admission might pass here, is left alone.
fn orient_filtered_ask(node: &mut Node) {
    use super::plan::JoinAlgo;

    let Some(mut filtered) = node.children.first_mut() else {
        return;
    };
    for _ in 0..8 {
        if !matches!(filtered.kind, Kind::Project(_)) || filtered.children.len() != 1 {
            break;
        }
        filtered = &mut filtered.children[0];
    }
    if let Kind::KeyedJoin { ripple, .. } = &mut filtered.kind {
        // the first matching pair answers the ASK
        *ripple = filtered.children.len() == 2;
        return;
    }
    if !matches!(filtered.kind, Kind::Filter(_)) {
        return;
    }
    let Some(product) = filtered.children.first_mut() else {
        return;
    };
    if matches!(
        product.kind,
        Kind::Join {
            algo: JoinAlgo::Cross,
            ..
        }
    ) && product.children.len() == 2
        && product.children[0].est < product.children[1].est
    {
        product.children.swap(0, 1);
    }
}

/// The estimated input rows from which automatic execution streams a grouping. The
/// cursor's column-wise aggregation measured 0.50 to 0.91 times eager execution's time
/// on every grouped query of the screen, from 30,000 input rows at 1.05M quads to 10M
/// at 10.5M, at one Rayon thread and at eight, on the AMD EPYC hosts of the Namespace
/// screens. Below the threshold the cursor's fixed cost of a few microseconds is not
/// repaid.
pub(super) const AUTO_GROUP_ROWS: f64 = 20_000.0;

/// Whether `node`, under the operators that finish a grouped query, is a grouping that
/// the cursor aggregates column-wise over an estimated [`AUTO_GROUP_ROWS`] input rows
/// or more, with a cursor implementation for every operator. This covers counts over
/// OPTIONAL and MINUS, which the cursor now joins and counts with branch-free kernels.
fn grouped(ctx: &Ctx, node: &Node) -> bool {
    let mut group = node;
    while matches!(
        group.kind,
        Kind::Project(_)
            | Kind::Slice { .. }
            | Kind::Extend(..)
            | Kind::Filter(_)
            | Kind::OrderBy { .. }
    ) && group.children.len() == 1
    {
        group = &group.children[0];
    }
    let rows = match &group.kind {
        Kind::Group { .. } if group::eligible(group) => group.children[0].est,
        Kind::GroupCountScan { .. } => group.est,
        _ => return false,
    };
    rows.is_finite() && rows >= AUTO_GROUP_ROWS && streams_natively(ctx, node)
}

/// Whether automatic execution runs `node` as a cursor because eager execution is
/// expected to outgrow the memory budget (X08 §6.8). The query must have somewhere to
/// spill, read an immutable snapshot, and have a cursor implementation for every
/// operator, and the largest operator output the plan estimates must exceed half of
/// the memory left.
fn spill_admitted(ctx: &Ctx, node: &Node, kind: QueryKind) -> bool {
    kind == QueryKind::Select
        && ctx.spill.is_some()
        && ctx.snap.delta.is_empty()
        && streams_natively(ctx, node)
        && {
            let bytes = largest_output_bytes(node);
            bytes.is_finite() && bytes * 2.0 > admission_room(ctx)
        }
}

/// Whether automatic execution runs `node` as a cursor because eager execution is
/// expected to outgrow the memory it may use, while the state the cursor keeps is
/// expected to fit with room to spare, so that the cursor needs no spill (roadmap item
/// 96). A `FILTER NOT EXISTS` over a Cartesian product under a grouping is the measured
/// case: eager execution holds the whole product, and the cursor holds the product's
/// smaller input, the key set of the NOT EXISTS and one row per group.
fn memory_admitted(ctx: &Ctx, node: &Node, kind: QueryKind, options: &CursorOptions) -> bool {
    let eager = largest_output_bytes(node);
    eager.is_finite() && eager * 2.0 > admission_room(ctx) && cursor_fits(ctx, node, kind, options)
}

/// Whether a cursor over `node` can run without spill: the query is a SELECT over an
/// immutable snapshot, every operator streams natively, and the state the cursor keeps,
/// with its batch headroom, is expected to fit in half of the memory the query may use.
/// Automatic admission asks this when eager execution is expected not to fit, and the
/// eager memory retry asks it when eager execution has failed, so both send the same
/// plans to a cursor without spill (roadmap items 91 and 96).
fn cursor_fits(ctx: &Ctx, node: &Node, kind: QueryKind, options: &CursorOptions) -> bool {
    kind == QueryKind::Select
        && ctx.snap.delta.is_empty()
        && batches_admitted(options)
        && streams_natively(ctx, node)
        && {
            let state = cursor_state_bytes(ctx, node);
            let headroom = cursor_headroom(ctx, node, options) as f64;
            state.is_finite() && state * 2.0 + headroom <= admission_room(ctx)
        }
}

/// The memory automatic admission compares a plan's estimates with: what the query's
/// own budget leaves, and never more than the whole query memory pool of its grant. It
/// does not depend on how much of the pool other queries hold at the moment, so the same
/// query gets the same plan under load.
fn admission_room(ctx: &Ctx) -> f64 {
    let own = ctx.memory_remaining();
    let room = match ctx.grant() {
        Some(grant) => own.min(grant.pool().capacity()),
        None => own,
    };
    room as f64
}

/// The bytes the query-local vocabulary is assumed to keep for each term an expression
/// generates: the term's key, its entry in the vocabulary's map and its slot.
const GENERATED_TERM_BYTES: f64 = 128.0;

/// The estimated bytes of the state a cursor over `node` keeps at once: the build
/// side of each join with its hash index, the groups of each grouping with their
/// aggregates, the rows of each DISTINCT and full sort, the rows each top-k keeps, the
/// key set of each EXISTS it answers from one, and the terms each BIND generates, which
/// the query-local vocabulary keeps until the query ends. The cursor streams the rest.
/// Operators the estimate does not know count their whole output, and state whose size
/// does not follow from the row estimates, such as the text of a GROUP_CONCAT, makes the
/// estimate infinite so that the plan is not admitted on it.
fn cursor_state_bytes(ctx: &Ctx, node: &Node) -> f64 {
    let width = |n: &Node| (n.vars.len().max(1) * 8) as f64;
    // rows kept in a growing vector, which may hold twice their bytes while it grows
    let rows = |n: &Node, est: f64| est.max(0.0) * width(n) * 2.0;
    // rows found through a hash table, which coexists with its old buckets while it
    // grows, as the cursor's own charges count it
    let hashed = |n: &Node, est: f64| {
        let est = est.max(0.0);
        let entries = est.min(usize::MAX as f64) as usize;
        rows(n, est) + binary::map_bytes(entries, 16) as f64 * 1.5 + est * 12.0 + 1024.0
    };
    let own = match &node.kind {
        _ if binary::eligible(node) => {
            let probe = &node.children[binary::probe_side(node)];
            let build = &node.children[1 - binary::probe_side(node)];
            // a join with no shared variable has no key, so it builds no hash index,
            // except a keyed OPTIONAL, which indexes its equality column
            if build.vars.iter().any(|v| probe.vars.contains(v))
                || matches!(
                    node.kind,
                    Kind::KeyedLeftJoin { .. } | Kind::KeyedJoin { .. }
                )
            {
                hashed(build, build.est)
            } else {
                rows(build, build.est)
            }
        }
        Kind::Group { aggs, .. } => {
            let input = node.children.first().map_or(0.0, |c| c.est.max(0.0));
            let mut bytes = hashed(node, node.est) + node.est.max(0.0) * aggs.len() as f64 * 64.0;
            for (_, agg) in aggs {
                if matches!(
                    agg.func,
                    spargebra::algebra::AggregateFunction::GroupConcat { .. }
                ) {
                    return f64::INFINITY;
                }
                // a DISTINCT aggregate keeps every distinct input row of its group
                if agg.distinct {
                    bytes += node.children.first().map_or(0.0, |c| hashed(c, input));
                }
            }
            bytes
        }
        Kind::Distinct => hashed(node, node.est),
        Kind::Sort(_) => rows(node, node.est),
        Kind::OrderBy { limit: Some(k), .. } => rows(node, (*k as f64).min(node.est)),
        Kind::OrderBy { limit: None, .. } => rows(node, node.est),
        Kind::Filter(exprs) if node.children.len() == 1 => exprs
            .iter()
            .filter(|e| e.has_exists())
            .map(|e| {
                super::exists::keyed_plan(ctx, e, &node.children[0])
                    .map_or(f64::INFINITY, |planned| {
                        hashed(&planned, planned.est) + cursor_state_bytes(ctx, &planned)
                    })
            })
            .sum(),
        // a variable or a constant generates no term
        Kind::Extend(_, Expr::Var(_) | Expr::Const(_) | Expr::Lit(..)) => 0.0,
        Kind::Extend(..) => node.est.max(0.0) * GENERATED_TERM_BYTES,
        _ if supported(&node.kind) => 0.0,
        _ => rows(node, node.est),
    };
    own + node
        .children
        .iter()
        .map(|c| cursor_state_bytes(ctx, c))
        .sum::<f64>()
}

/// The estimated bytes of the largest operator output in `node`, as eager execution
/// would hold it.
fn largest_output_bytes(node: &Node) -> f64 {
    let own = node.est.max(0.0) * (node.vars.len().max(1) * 8) as f64;
    node.children
        .iter()
        .map(largest_output_bytes)
        .fold(own, f64::max)
}

/// Whether every operator of `node` has a cursor implementation, so that a cursor
/// over it runs no eager subtree. An operator that evaluates EXISTS counts as not
/// native here, since whether its patterns stream is known only once they are built.
pub(super) fn native(node: &Node) -> bool {
    !own_exprs(&node.kind).iter().any(|e| e.has_exists())
        && (supported(&node.kind)
            || binary::eligible(node)
            || group::eligible(node)
            || expand::eligible(node)
            || walk::eligible(node)
            || (matches!(node.kind, Kind::Sort(_) | Kind::OrderBy { .. })
                && node.children.len() == 1))
        && node.children.iter().all(native)
}

/// How deep automatic admission follows EXISTS patterns nested in EXISTS patterns. A
/// deeper nesting keeps the plan eager.
const AUTO_EXISTS_DEPTH: usize = 4;

/// Whether every operator of `node` has a cursor implementation, as [`native`] asks,
/// where a FILTER whose conjuncts include `EXISTS { P }` or `NOT EXISTS { P }` also
/// counts when the cursor can answer each of them from a key set and the plan of each
/// `P` counts in turn. The key set is filled by a child cursor under the query's
/// charges. An EXISTS anywhere else, such as in BIND, in an OPTIONAL condition or
/// inside a larger expression, is evaluated once per distinct outer key. On Namespace
/// at 10.5M quads that took 1.15 times as long in the cursor as in eager execution,
/// while the key-set filters took 0.68 to 0.99 times as long, so such plans stay eager.
/// The patterns are planned here only to read their operators. Planning interns their
/// variables in `ctx` and runs no part of them.
pub(super) fn streams_natively(ctx: &Ctx, node: &Node) -> bool {
    fn at(ctx: &Ctx, node: &Node, depth: usize) -> bool {
        let exists = match &node.kind {
            Kind::Filter(exprs) => exprs.iter().all(|e| {
                !e.has_exists()
                    || (depth < AUTO_EXISTS_DEPTH
                        && node.children.len() == 1
                        && super::exists::keyed_plan(ctx, e, &node.children[0])
                            .is_some_and(|planned| at(ctx, &planned, depth + 1)))
            }),
            kind => !own_exprs(kind).iter().any(|e| e.has_exists()),
        };
        exists
            && (supported(&node.kind)
                || binary::eligible(node)
                || group::eligible(node)
                || expand::eligible(node)
                || walk::eligible(node)
                || (matches!(node.kind, Kind::Sort(_) | Kind::OrderBy { .. })
                    && node.children.len() == 1))
            && node.children.iter().all(|child| at(ctx, child, depth))
    }
    at(ctx, node, 0)
}

/// Whether nothing in `node` has effects that running it twice would repeat or
/// expose, so that a query that failed eagerly can run again as a cursor.
fn retryable(node: &Node) -> bool {
    !matches!(node.kind, Kind::Service { .. } | Kind::AlgoCall(_))
        && node.children.iter().all(retryable)
}

/// Whether `pattern` holds a SERVICE anywhere, in an EXISTS, an OPTIONAL condition, a
/// LATERAL, an aggregate or an ORDER BY key as well as in the pattern itself. The plan
/// keeps an EXISTS that it evaluates per row as a pattern, so [`retryable`] does not
/// see a SERVICE there.
pub(super) fn calls_service(pattern: &spargebra::algebra::GraphPattern) -> bool {
    use spargebra::algebra::{AggregateExpression, GraphPattern as P, OrderExpression};
    let order = |keys: &[OrderExpression]| {
        keys.iter().any(|k| match k {
            OrderExpression::Asc(e) | OrderExpression::Desc(e) => expr_calls_service(e),
        })
    };
    match pattern {
        P::Service { .. } => true,
        P::Bgp { .. } | P::Path { .. } | P::Values { .. } => false,
        P::Join { left, right }
        | P::Union { left, right }
        | P::Minus { left, right }
        | P::SemiJoin { left, right }
        | P::AntiJoin { left, right }
        | P::Lateral { left, right } => calls_service(left) || calls_service(right),
        P::LeftJoin {
            left,
            right,
            expression,
        } => {
            calls_service(left)
                || calls_service(right)
                || expression.as_ref().is_some_and(expr_calls_service)
        }
        P::Filter { expr, inner } => expr_calls_service(expr) || calls_service(inner),
        P::Extend {
            inner, expression, ..
        }
        | P::Assign {
            inner, expression, ..
        }
        | P::Unfold {
            inner, expression, ..
        } => expr_calls_service(expression) || calls_service(inner),
        P::OrderBy { inner, expression } => order(expression) || calls_service(inner),
        P::Group {
            inner, aggregates, ..
        } => {
            calls_service(inner)
                || aggregates.iter().any(|(_, agg)| match agg {
                    AggregateExpression::CountSolutions { .. } => false,
                    AggregateExpression::FunctionCall { expr, .. } => expr_calls_service(expr),
                    AggregateExpression::Fold {
                        expr,
                        value,
                        order: keys,
                        ..
                    } => {
                        expr_calls_service(expr)
                            || value.as_ref().is_some_and(expr_calls_service)
                            || order(keys)
                    }
                })
        }
        P::Graph { inner, .. }
        | P::Project { inner, .. }
        | P::Distinct { inner }
        | P::Reduced { inner }
        | P::Slice { inner, .. } => calls_service(inner),
    }
}

/// Whether `expr` holds an EXISTS whose pattern holds a SERVICE.
fn expr_calls_service(expr: &spargebra::algebra::Expression) -> bool {
    use spargebra::algebra::Expression as E;
    match expr {
        E::Exists(pattern) => calls_service(pattern),
        E::NamedNode(_) | E::Literal(_) | E::Variable(_) | E::Bound(_) => false,
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
        | E::Divide(a, b) => expr_calls_service(a) || expr_calls_service(b),
        E::UnaryPlus(a) | E::UnaryMinus(a) | E::Not(a) => expr_calls_service(a),
        E::In(a, list) => expr_calls_service(a) || list.iter().any(expr_calls_service),
        E::If(a, b, c) => expr_calls_service(a) || expr_calls_service(b) || expr_calls_service(c),
        E::Coalesce(list) | E::FunctionCall(_, list) => list.iter().any(expr_calls_service),
    }
}

/// Select execution without parsing or planning twice in automatic mode.
pub fn query_execution(
    snapshot: Arc<Snapshot>,
    query: &str,
    options: &QueryOptions,
    cursor: &CursorOptions,
    mode: ExecutionMode,
) -> Result<QueryExecution> {
    query_execution_with_plan(snapshot, query, options, cursor, mode).map_err(|f| f.error)
}

/// The stack left on a thread that parses without measuring the text first. It covers the
/// parser at the deepest nesting spargebra accepts, about 1.5 MiB.
const PARSE_STACK: usize = 2 << 20;

/// [`query_execution`], with the plan of an eager query that failed while it ran, as
/// [`super::query_with_plan`] gives it.
pub fn query_execution_with_plan(
    snapshot: Arc<Snapshot>,
    query: &str,
    options: &QueryOptions,
    cursor: &CursorOptions,
    mode: ExecutionMode,
) -> std::result::Result<QueryExecution, Box<super::Failed>> {
    if mode == ExecutionMode::Streaming
        || (mode == ExecutionMode::Auto && cursor.fallback == FallbackPolicy::RejectMaterialization)
    {
        return Ok(query_cursor(snapshot, query, options, cursor)?);
    }
    let started = Instant::now();
    // The parser recurses once per bracket, and spargebra refuses a text with more than
    // MAX_NESTING of them, which takes about 1.5 MiB. A thread with more stack left than
    // that, such as a server worker, parses as the eager query APIs do. Measuring the text
    // first would scan it a second time, which costs a small query about 0.5% of its
    // instructions. A smaller thread measures the text and grows its stack to fit.
    let parser_depth = if stacker::remaining_stack().is_some_and(|left| left >= PARSE_STACK) {
        0
    } else {
        spargebra::nesting::measure(query)
            .depth
            .min(depth::MAX_ALGEBRA_DEPTH)
    };
    let retry_snapshot = snapshot.clone();
    depth::with_stack(parser_depth, move || {
        let parsed = super::parse_query(query, options.base_iri.as_deref(), &options.prefixes)?;
        let parse_ms = started.elapsed().as_secs_f64() * 1000.0;
        let levels = depth::check_query(&parsed)?;
        // the algebra is dropped on the stack sized for its depth
        depth::with_stack(levels, move || {
            if mode == ExecutionMode::Eager {
                let mut result =
                    super::execute_query_with_plan(snapshot, &parsed, options, parse_ms)?;
                if result.kind == QueryKind::Select {
                    super::select_star_order(query, &mut result);
                }
                let depth = levels.max(result.plan.depth());
                return Ok(QueryExecution::Eager(Box::new(MaterializedResult {
                    result: Some(result),
                    depth,
                })));
            }
            if cursor.batch_rows == 0 || cursor.batch_bytes == 0 {
                return Err(Error::invalid("cursor batch rows and bytes must be positive").into());
            }
            let planning = Instant::now();
            let mut prepared =
                super::prepare_query(snapshot, &parsed, options, Some(started), Some(cursor))?;
            let plan_ms = planning.elapsed().as_secs_f64() * 1000.0;
            let kind = prepared.kind;
            let levels = levels.max(prepared.plan_depth);
            // An eager SELECT that runs out of memory runs again once as a cursor that
            // may spill (X08 §6.8). Eager execution writes nothing before it finishes,
            // so the failure always comes before any output. Without spill it runs
            // again when automatic admission would have streamed it had its estimates
            // foreseen the failure: its operators all stream and their state fits.
            // That test plans EXISTS patterns, so it waits for the failure.
            let retry = kind == QueryKind::Select
                && options.extensions.is_none()
                && options.initial_bindings.is_empty()
                && retryable(&prepared.node)
                && !matches!(&parsed, spargebra::Query::Select { pattern, .. } if calls_service(pattern));
            if prepared.ctx.is_cursor() {
                if kind == QueryKind::Ask {
                    orient_filtered_ask(&mut prepared.node);
                }
                let (cursor, _, _) = from_prepared(
                    prepared, &parsed, query, options, cursor, started, parse_ms, plan_ms, levels,
                    kind,
                )?;
                if kind == QueryKind::Ask {
                    let depth = cursor.depth;
                    let result = finish_ask_with_plan(cursor)?;
                    Ok(QueryExecution::Ask(Box::new(AskResult {
                        result: Some(result),
                        depth,
                    })))
                } else {
                    Ok(QueryExecution::Select(Box::new(cursor)))
                }
            } else {
                let ran = depth::with_stack(levels, || {
                    super::execute_prepared(&parsed, options, parse_ms, plan_ms, prepared)
                });
                let mut result = match ran {
                    Err(failed)
                        if retry
                            && matches!(
                                &failed.error,
                                Error::BudgetExceeded(b) if matches!(
                                    b.kind,
                                    crate::BudgetKind::Memory | crate::BudgetKind::ServerMemory
                                )
                            )
                            && (options.spill.is_some()
                                || fits_without_spill(
                                    &retry_snapshot,
                                    &parsed,
                                    options,
                                    cursor,
                                    started,
                                )) =>
                    {
                        return retry_as_cursor(retry_snapshot, query, options, cursor, started);
                    }
                    ran => ran?,
                };
                if result.kind == QueryKind::Select {
                    super::select_star_order(query, &mut result);
                }
                let depth = levels.max(result.plan.depth());
                Ok(QueryExecution::Eager(Box::new(MaterializedResult {
                    result: Some(result),
                    depth,
                })))
            }
        })
    })
}

/// Whether a cursor over `parsed` could run without spill, as automatic admission asks
/// of it through [`cursor_fits`]. The eager run that failed has released its memory, so
/// the query is planned again against its whole budget.
fn fits_without_spill(
    snapshot: &Arc<Snapshot>,
    parsed: &spargebra::Query,
    options: &QueryOptions,
    cursor: &CursorOptions,
    started: Instant,
) -> bool {
    let options = within_first_deadline(options, started);
    super::prepare_query(snapshot.clone(), parsed, &options, None, None)
        .is_ok_and(|p| cursor_fits(&p.ctx, &p.node, p.kind, cursor))
}

/// `options` with a relative timeout turned into the deadline of the run that started
/// at `started`, so that what runs after an eager failure keeps the first run's time.
fn within_first_deadline(options: &QueryOptions, started: Instant) -> QueryOptions {
    let mut options = options.clone();
    if let Some(timeout) = options.timeout {
        let deadline = started + timeout;
        options.deadline = Some(options.deadline.map_or(deadline, |d| d.min(deadline)));
        options.timeout = None;
    }
    options
}

/// Run `query` again as a cursor after its eager run exceeded the memory budget, within
/// the deadline the first run started under. The root of the cursor plan counts the
/// retry as `eagerMemoryRetry`.
fn retry_as_cursor(
    snapshot: Arc<Snapshot>,
    query: &str,
    options: &QueryOptions,
    cursor: &CursorOptions,
    started: Instant,
) -> std::result::Result<QueryExecution, Box<super::Failed>> {
    if let Some(spill) = &options.spill {
        spill.count_eager_retry();
    }
    let options = within_first_deadline(options, started);
    let mut execution = query_cursor(snapshot, query, &options, cursor)?;
    if let QueryExecution::Select(cursor) = &mut execution {
        let counters = cursor
            .plan
            .operator
            .counters
            .get_or_insert_with(Default::default);
        counters.insert("eagerMemoryRetry".into(), 1.into());
    }
    Ok(execution)
}

/// Explicit execution of any query form. SELECT and graph output are pull
/// cursors; ASK runs the minimum necessary input to return its boolean.
pub enum QueryExecution {
    Eager(Box<MaterializedResult>),
    Select(Box<QueryCursor>),
    Graph(Box<GraphCursor>),
    Ask(Box<AskResult>),
}
impl QueryExecution {
    pub fn kind(&self) -> QueryKind {
        match self {
            Self::Eager(r) => r.result().kind,
            Self::Select(_) => QueryKind::Select,
            Self::Graph(c) => c.kind(),
            Self::Ask(_) => QueryKind::Ask,
        }
    }
    pub fn stats(&self) -> CursorStats {
        match self {
            Self::Select(c) => c.stats(),
            Self::Graph(c) => c.stats(),
            Self::Eager(r) | Self::Ask(r) => CursorStats {
                status: CursorStatus::Complete,
                emitted_rows: r.result().len() as u64,
                rows_produced: r.result().rows_produced,
                mem_peak_bytes: r.result().mem_peak_bytes,
                granted_bytes: r.result().ctx.grant().map(|g| g.peak()),
                timing: r.result().timing.clone(),
                error: None,
            },
        }
    }
    /// What query-time reasoning derived for the query (spec F17).
    pub fn reasoning(&self) -> Option<Arc<super::reasoning::Applied>> {
        match self {
            Self::Select(c) => c.reasoning(),
            Self::Graph(c) => c.reasoning(),
            Self::Eager(r) | Self::Ask(r) => r.result().ctx.reasoning.clone(),
        }
    }
    pub fn cancellation_token(&self) -> Arc<std::sync::atomic::AtomicBool> {
        match self {
            Self::Select(c) => c.cancellation_token(),
            Self::Graph(c) => c.cancellation_token(),
            Self::Eager(r) | Self::Ask(r) => r.result().ctx.cancel.clone(),
        }
    }
    /// The deadline without the time the query spent waiting for its consumer. The
    /// effective deadline is `deadline() + deadline_pause()`.
    pub fn deadline(&self) -> Option<Instant> {
        match self {
            Self::Select(c) => c.deadline(),
            Self::Graph(c) => c.deadline(),
            Self::Eager(r) | Self::Ask(r) => r.result().ctx.deadline,
        }
    }
    /// The nanoseconds the query spent waiting for its consumer. Adding to the counter
    /// moves the effective deadline back by as much.
    pub fn deadline_pause(&self) -> Arc<AtomicU64> {
        match self {
            Self::Select(c) => c.deadline_pause(),
            Self::Graph(c) => c.deadline_pause(),
            Self::Eager(r) | Self::Ask(r) => r.result().ctx.deadline_pause.clone(),
        }
    }
    pub fn plan_json(&self) -> Result<String> {
        match self {
            Self::Select(c) => c.plan_json(),
            Self::Graph(c) => c.plan_json(),
            Self::Eager(r) | Self::Ask(r) => r.plan_json(),
        }
    }
    pub fn close(&mut self) {
        match self {
            Self::Select(c) => c.close(),
            Self::Graph(c) => c.close(),
            Self::Eager(_) | Self::Ask(_) => {}
        }
    }
}

pub fn query_cursor(
    snapshot: Arc<Snapshot>,
    query: &str,
    options: &QueryOptions,
    cursor: &CursorOptions,
) -> Result<QueryExecution> {
    let (where_cursor, spec, kind) = open_pattern_cursor(snapshot, query, options, cursor, None)?;
    match kind {
        QueryKind::Select => Ok(QueryExecution::Select(Box::new(where_cursor))),
        QueryKind::Ask => {
            let levels = where_cursor.depth;
            finish_ask(where_cursor).map(|result| {
                QueryExecution::Ask(Box::new(AskResult {
                    result: Some(result),
                    depth: levels,
                }))
            })
        }
        _ => graph::from_pattern(
            where_cursor,
            spec.expect("graph specification"),
            query.len(),
        )
        .map(|cursor| QueryExecution::Graph(Box::new(cursor))),
    }
}

impl QueryCursor {
    pub fn variables(&self) -> &[String] {
        &self.variables
    }
    pub fn plan(&self) -> &CursorPlan {
        &self.plan
    }
    /// Serialize capability/runtime metadata on a depth-protected stack. This
    /// never drains execution; foreign bindings need not recurse on their stack.
    pub fn plan_json(&self) -> Result<String> {
        depth::with_stack(self.depth, || {
            serde_json::to_string(&self.plan).map_err(|e| Error::invalid(e.to_string()))
        })
    }
    pub fn status(&self) -> CursorStatus {
        self.status
    }

    /// Cancellation/deadline controls remain valid through response serialization.
    pub fn cancellation_token(&self) -> Arc<std::sync::atomic::AtomicBool> {
        self.ctx.cancel.clone()
    }
    /// What query-time reasoning derived for the query (spec F17).
    pub fn reasoning(&self) -> Option<Arc<super::reasoning::Applied>> {
        self.ctx.reasoning.clone()
    }
    pub fn deadline(&self) -> Option<Instant> {
        self.ctx.deadline
    }
    /// The nanoseconds this cursor's consumer kept it waiting, which its deadline does
    /// not count.
    pub fn deadline_pause(&self) -> Arc<AtomicU64> {
        self.ctx.deadline_pause.clone()
    }

    pub(super) fn check(&self) -> Result<()> {
        self.ctx.check()
    }
    pub(super) fn charge(&self, bytes: u64) -> Result<OwnedCharge> {
        OwnedCharge::new(&self.ctx, bytes)
    }

    pub(super) fn stack_depth(&self) -> usize {
        self.depth
    }

    pub(super) fn fail_output(&mut self, error: &Error) {
        self.failure = Some(error.to_string());
        if self.status == CursorStatus::Open {
            self.terminate(CursorStatus::Failed);
        }
    }

    pub fn stats(&self) -> CursorStats {
        let mut timing = self.timing.clone();
        timing.total_ms = self
            .ended
            .unwrap_or_else(Instant::now)
            .duration_since(self.started)
            .as_secs_f64()
            * 1000.0;
        CursorStats {
            status: self.status,
            emitted_rows: self.emitted,
            rows_produced: self.ctx.rows_produced(),
            mem_peak_bytes: self.ctx.mem_peak(),
            granted_bytes: self.ctx.grant().map(|g| g.peak()),
            timing,
            error: self.failure.clone(),
        }
    }

    pub fn next_batch(&mut self) -> Result<Option<QueryBatch>> {
        self.next_batch_at_most(self.options.batch_rows)
    }

    /// Hand each batch, with the terms of its base-vocabulary cells decoded once in key
    /// order, to `each`, until `send` rows have been pulled or the cursor ends. Returns the
    /// rows of `send` left over, which without `send` count down from `usize::MAX`, so that
    /// 0 means the writer stopped at `send`.
    ///
    /// With `decode_ahead` and more than one Rayon worker, every batch after the first is
    /// pulled before `each` handles the one before it, and its terms are decoded on the pool
    /// while `each` runs. Two batches and their terms are then live at once, both charged.
    /// The first batch goes to `each` before the second is pulled, so the first rows of an
    /// answer wait for one batch only, however slowly the plan produces the next. Decoding
    /// ahead then starts only when the memory budget has room for twice what the first
    /// batch took, and the writer otherwise goes on in turn. The first batch is only an
    /// estimate of the others. When later batches hold larger terms, a budget within a
    /// few KiB of what the writer in turn needs can still fail with decoding ahead, with a
    /// memory budget error and never with other output.
    ///
    /// An error while pulling a batch ahead is returned after `each` has been called with the
    /// batch before it. A memory budget failure inside the plan also fails every later
    /// check of the query, so `each` then stops at its first check, and the rows of that
    /// batch are not written. Without decoding ahead those rows would be written before the
    /// pull that fails. The answer is an error either way.
    pub(super) fn for_each_decoded(
        &mut self,
        send: Option<usize>,
        mut each: impl FnMut(&Self, &QueryBatch, Option<&DecodedBatch<'_>>) -> Result<()>,
    ) -> Result<usize> {
        let mut remaining = send.unwrap_or(usize::MAX);
        let mut ahead = self.ctx.opt.decode_ahead && rayon::current_num_threads() >= 2;
        // In turn: every batch without decoding ahead, and the first batch with it.
        let mut first = true;
        while remaining > 0 && (first || !ahead) {
            let before = self.ctx.live_bytes();
            let Some(batch) = self.next_batch_at_most(remaining)? else {
                return Ok(remaining);
            };
            let decoded = batch.decoded()?;
            each(self, &batch, decoded.as_ref())?;
            remaining -= batch.len().min(remaining);
            if first {
                first = false;
                // Decoding ahead holds a second batch and its terms. It starts only when
                // the memory budget has room for twice what the first batch took at its
                // peak, which keeps most budgets that the writer fits in turn from failing
                // it. Batches larger than the first can still use up the room.
                let took = self.ctx.mem_peak().saturating_sub(before);
                ahead = ahead && self.ctx.memory_remaining() >= took.saturating_mul(2);
            }
        }
        if remaining == 0 {
            return Ok(remaining);
        }
        let mut current = match self.next_batch_at_most(remaining)? {
            Some(batch) => {
                let cache = batch.term_cache()?;
                Some((batch, cache))
            }
            None => None,
        };
        while let Some((batch, cache)) = current.take() {
            remaining -= batch.len().min(remaining);
            let next = if remaining > 0 {
                self.next_batch_at_most(remaining)
            } else {
                Ok(None)
            };
            let decoded = cache.map(|cache| DecodedBatch::new(&batch, cache));
            match next {
                Ok(Some(next)) => {
                    let mut built = None;
                    let (slot, upcoming, this) = (&mut built, &next, &*self);
                    let written = rayon::in_place_scope(|scope| {
                        scope.spawn(move |_| *slot = Some(upcoming.term_cache()));
                        each(this, &batch, decoded.as_ref())
                    });
                    drop(decoded);
                    drop(batch);
                    written?;
                    let cache = built.expect("the decoding task ran")?;
                    current = Some((next, cache));
                }
                Ok(None) => each(self, &batch, decoded.as_ref())?,
                Err(error) => {
                    each(self, &batch, decoded.as_ref())?;
                    return Err(error);
                }
            }
        }
        Ok(remaining)
    }

    pub(super) fn next_batch_at_most(&mut self, rows: usize) -> Result<Option<QueryBatch>> {
        if self.shared && self.status == CursorStatus::Open {
            let began = Instant::now();
            let attempt = depth::with_stack(self.depth, || {
                self.ctx.check()?;
                self.root.as_mut().expect("open cursor").next_shared(
                    &self.ctx,
                    &self.options,
                    &self.vars,
                    rows,
                )
            });
            self.timing.exec_ms += began.elapsed().as_secs_f64() * 1000.0;
            match attempt {
                Ok(SharedAttempt::Unsupported) => self.shared = false,
                Ok(SharedAttempt::Ready(batch)) => {
                    let root = self.root.as_ref().expect("open cursor");
                    root.update_plan(&mut self.plan);
                    let complete = root.done || batch.is_none();
                    self.plan.operator.warnings = self.ctx.warnings();
                    if let Some(batch) = &batch {
                        self.emitted = self
                            .emitted
                            .saturating_add((batch.end - batch.start) as u64);
                    }
                    if complete {
                        self.terminate(CursorStatus::Complete);
                    }
                    return Ok(batch.map(|batch| QueryBatch {
                        buffer: BatchBuffer::Shared(batch),
                        variables: self.variables.clone(),
                        ctx: self.ctx.clone(),
                    }));
                }
                Err(error) => {
                    // the counts so far, as the pull path keeps them
                    if let Some(root) = &self.root {
                        root.update_plan(&mut self.plan);
                    }
                    self.plan.operator.warnings = self.ctx.warnings();
                    self.failure = Some(error.to_string());
                    self.terminate(CursorStatus::Failed);
                    return Err(error);
                }
            }
        }
        self.pull(rows).map(|b| {
            b.map(|buffer| QueryBatch {
                buffer: BatchBuffer::Owned(buffer),
                variables: self.variables.clone(),
                ctx: self.ctx.clone(),
            })
        })
    }

    fn pull(&mut self, rows: usize) -> Result<Option<Buffer>> {
        if self.status != CursorStatus::Open {
            return Ok(None);
        }
        let started = Instant::now();
        let result: Result<Option<Buffer>> = depth::with_stack(self.depth, || {
            self.ctx.check()?;
            let root = self.root.as_mut().expect("an open cursor has an operator");
            let mut batch = root.next(&self.ctx, &self.options, rows)?;
            if let Some(b) = &mut batch {
                b.project(&self.vars)?;
                for (var, id) in &self.bound {
                    if let Some(c) = b.table.col_of(*var) {
                        b.table.cols[c].fill(*id);
                    }
                }
            }
            self.ctx.check()?;
            Ok(batch)
        });
        self.timing.exec_ms += started.elapsed().as_secs_f64() * 1000.0;
        if let Some(root) = &self.root {
            root.update_plan(&mut self.plan);
        }
        self.plan.operator.warnings = self.ctx.warnings();
        match result {
            Ok(batch) => {
                if let Some(b) = &batch {
                    self.emitted = self.emitted.saturating_add(b.table.len as u64);
                }
                if batch.is_none() || self.root.as_ref().is_some_and(|r| r.done) {
                    self.terminate(CursorStatus::Complete);
                }
                Ok(batch)
            }
            Err(error) => {
                self.failure = Some(error.to_string());
                self.terminate(CursorStatus::Failed);
                Err(error)
            }
        }
    }

    fn terminate(&mut self, status: CursorStatus) {
        if status == CursorStatus::Complete {
            // the query finished: an operator that did not finish had enough rows
            fn stopped(p: &mut CursorPlan) {
                if !p.complete
                    && p.operator.actual_rows >= 0
                    && p.operator.fidelity().skipped.is_none()
                {
                    p.operator.fidelity_mut().stopped_early = true;
                }
                p.children.iter_mut().for_each(stopped);
            }
            stopped(&mut self.plan);
        }
        self.status = status;
        self.ended = Some(Instant::now());
        // Destruction stays on the depth-protected calling thread.
        depth::with_stack(self.depth, || {
            self.root = None;
        });
        self.ctx.release_cursor_work();
    }

    pub fn close(&mut self) {
        if self.status == CursorStatus::Open {
            self.terminate(CursorStatus::Stopped);
        }
    }

    /// Collect a fresh cursor explicitly, under its memory budget. Previously
    /// yielded batches cannot be recovered, so collection after a pull is refused.
    pub fn collect(mut self) -> Result<QueryResult> {
        if self.emitted != 0 || matches!(self.status, CursorStatus::Failed | CursorStatus::Stopped)
        {
            return Err(Error::invalid(self.failure.clone().unwrap_or_else(|| {
                "cannot collect a partially consumed or stopped cursor".into()
            })));
        }
        let mut output = Buffer::new(&self.ctx, &self.vars, 0)?;
        while let Some(buffer) = self.pull(self.options.batch_rows)? {
            let len = output
                .table
                .len
                .checked_add(buffer.table.len)
                .ok_or_else(|| Error::invalid("collected query row count overflow"))?;
            let bytes = super::ctx::table_bytes(len, self.vars.len())
                .saturating_add(self.vars.len() as u64 * 40 + 128);
            output.charge.resize(bytes)?;
            for (column, incoming) in output.table.cols.iter_mut().zip(&buffer.table.cols) {
                column.reserve_exact(incoming.len());
                column.extend_from_slice(incoming);
            }
            output.table.len = len;
            output.reconcile()?;
        }
        let stats = self.stats();
        output.charge.retain();
        Ok(QueryResult {
            kind: QueryKind::Select,
            vars: (*self.variables).clone(),
            table: output.table,
            boolean: false,
            triples: Vec::new(),
            quads: Vec::new(),
            plan: self.plan_info(),
            timing: stats.timing,
            mem_peak_bytes: stats.mem_peak_bytes,
            rows_produced: stats.rows_produced,
            describe_truncated: false,
            ctx: self.ctx.clone(),
        })
    }

    fn plan_info(&self) -> PlanInfo {
        fn build(p: &CursorPlan) -> PlanInfo {
            let mut info = p.operator.clone();
            info.children = p.children.iter().map(build).collect();
            info
        }
        build(&self.plan)
    }
}

impl Drop for QueryCursor {
    fn drop(&mut self) {
        depth::with_stack(self.depth, || {
            self.close();
            self.plan.children.clear();
            self.plan.operator.children.clear();
        });
    }
}

fn plan_bytes(node: &Node) -> u64 {
    let values = match &node.kind {
        Kind::Values(t) => capacity_bytes(t),
        _ => 0,
    };
    (node.desc.len() as u64)
        .saturating_add(node.vars.len() as u64 * 32)
        .saturating_add(1024)
        .saturating_add(values)
        .saturating_add(node.children.iter().map(plan_bytes).sum::<u64>())
}

enum State {
    Scalar(Node),
    /// Counts per key, read from the index or its statistics into charged group state
    /// before the first output.
    Counts {
        node: Node,
        loaded: Option<Buffer>,
        at: usize,
    },
    Merge(Box<merge::Merge>),
    Distinct(distinct::Distinct),
    Binary(Box<binary::Binary>),
    Ripple(Box<ripple::Ripple>),
    Group(Box<group::Group>),
    Filter(Box<filter::Filter>),
    Expand(Box<expand::Expand>),
    Walk(Box<walk::Walk>),
    Scan(Scan),
    Values {
        table: Table,
        at: usize,
    },
    Empty,
    Unary(Node),
    Slice {
        offset: usize,
        remaining: Option<usize>,
    },
    Union {
        at: usize,
    },
    /// An operator without a cursor implementation. With `inputs`, its children are
    /// cursors whose output it collects before running. Otherwise its whole subtree
    /// runs eagerly.
    Fallback {
        node: Node,
        loaded: Option<Buffer>,
        at: usize,
        inputs: bool,
        /// Scans among the inputs, which an eager scan reads straight into a table of
        /// the right size. The fallback holds its whole input either way.
        scans: Vec<Option<Node>>,
    },
    Blocking {
        node: Node,
        loaded: Option<Buffer>,
        at: usize,
        /// The external sort, once the input has spilled.
        external: Option<Box<extsort::External>>,
    },
    TopK(Box<topk::TopK>),
    IndexTopK(Box<topk::IndexTopK>),
}

struct Operator {
    state: State,
    children: Vec<Operator>,
    info: PlanInfo,
    vars: Vec<VarId>,
    rows: usize,
    done: bool,
    materializes: bool,
    growing: bool,
    reason: Option<String>,
    bind_reuse: Option<Box<bind::Reuse>>,
    /// The variables every output batch, and the concatenation of all of them, is
    /// sorted on in raw ID order. It is fixed when the operator is built, and each
    /// output batch carries it.
    order: Vec<VarId>,
    /// The order key of the last row produced, to check the order across batches.
    #[cfg(debug_assertions)]
    last: Option<Vec<Id>>,
}

fn supported(kind: &Kind) -> bool {
    matches!(
        kind,
        Kind::Scan(_)
            | Kind::Values(_)
            | Kind::Empty
            | Kind::Project(_)
            | Kind::Distinct
            | Kind::CountScan { .. }
            | Kind::CountDistinctScan { .. }
            | Kind::GroupCountScan { .. }
            | Kind::CountJoinRuns { .. }
            | Kind::Slice { .. }
            | Kind::Union
            | Kind::RangeScan(..)
            | Kind::Filter(_)
            | Kind::Extend(..)
            | Kind::CountFilterScan { .. }
    )
}

/// The expressions an operator evaluates itself, not those of its inputs.
fn own_exprs(kind: &Kind) -> Vec<&Expr> {
    match kind {
        Kind::Filter(exprs)
        | Kind::RangeScan(_, RangeSpec { filter: exprs, .. })
        | Kind::CountFilterScan { filter: exprs, .. } => exprs.iter().collect(),
        Kind::Extend(_, expr) | Kind::Assign(_, expr) | Kind::Unfold { expr, .. } => vec![expr],
        Kind::LeftJoin { expr } => expr.iter().collect(),
        Kind::KeyedLeftJoin { expr, .. } | Kind::KeyedJoin { expr, .. } => vec![expr],
        Kind::OrderBy { keys, .. } => keys.iter().map(|(key, _)| key).collect(),
        Kind::Group { aggs, .. } => aggs
            .iter()
            .flat_map(|(_, agg)| {
                agg.expr.iter().chain(agg.fold.iter().flat_map(|fold| {
                    fold.value
                        .iter()
                        .chain(fold.order.iter().map(|(key, _)| key))
                }))
            })
            .collect(),
        Kind::IndexJoin(spec) => spec
            .probes
            .iter()
            .flat_map(|probe| probe.filter.iter())
            .collect(),
        _ => Vec::new(),
    }
}

/// Whether every EXISTS that an operator evaluates itself runs without an eager
/// fallback. Each pattern is planned as the key set plans it, and that plan is built as
/// cursor operators under the strict policy. A row evaluated on its own runs the same
/// pattern with its values substituted and a limit of one solution.
fn exists_streams(ctx: &Arc<Ctx>, kind: &Kind) -> Result<bool> {
    let mut specs = Vec::new();
    for expr in own_exprs(kind) {
        expr.exists_specs(&mut specs);
    }
    for spec in specs {
        let planned = super::plan::Planner::new(ctx).plan(&spec.pattern, &spec.graph, Vec::new());
        let built = planned
            .and_then(|node| Operator::build(ctx, node, FallbackPolicy::RejectMaterialization));
        match built {
            Ok(_) => {}
            Err(error @ (Error::Cancelled | Error::Timeout | Error::BudgetExceeded(_))) => {
                return Err(error);
            }
            Err(_) => return Ok(false),
        }
    }
    Ok(true)
}

/// Run `node` as a cursor and hand each of its batches to `each`. EXISTS key sets use
/// it to read their pattern without materializing its solutions.
pub(super) fn drain(
    ctx: &Arc<Ctx>,
    node: Node,
    each: &mut dyn FnMut(&Table) -> Result<()>,
) -> Result<()> {
    let options = CursorOptions::default();
    let mut root = Operator::build(ctx, node, FallbackPolicy::AllowMaterialization)?;
    while let Some(batch) = root.next(ctx, &options, options.batch_rows)? {
        each(&batch.table)?;
    }
    Ok(())
}

/// What runs an operator, which decides the order its output keeps.
enum Role {
    Merge,
    Binary,
    Group,
    Blocking,
    Fallback,
    Streamed,
}

/// Plan kinds whose eager output order a caller may observe without ORDER BY, such as
/// a ranking. A fallback keeps their order rather than re-sorting them.
fn ranked(kind: &Kind) -> bool {
    matches!(
        kind,
        Kind::OrderBy { .. }
            | Kind::OrderedTopK(_)
            | Kind::TextSearch(_)
            | Kind::VectorSearch(_)
            | Kind::HybridSearch(_)
            | Kind::SpatialKnn(_)
            | Kind::Service { .. }
            | Kind::PathSearch(_)
            | Kind::AlgoCall(_)
    )
}

/// Whether the rows of `node` come in an order a caller observes that operator orders
/// do not record, such as an ORDER BY's or a ranking's, through the operators that pass
/// an input's order on, as [`output_order`] lists them. A DISTINCT over such rows must
/// not pass rows to a later pass, which would emit them after rows that come after them.
/// A join is followed into every input whose order its output may keep, whichever way
/// the cursor runs it, so the answer errs on the side of keeping rows in memory.
fn value_ordered(node: &Node) -> bool {
    if ranked(&node.kind) {
        return true;
    }
    let first = || node.children.first().is_some_and(value_ordered);
    match &node.kind {
        Kind::Project(_)
        | Kind::Distinct
        | Kind::Filter(_)
        | Kind::Extend(..)
        | Kind::Assign(..)
        | Kind::Slice { .. }
        | Kind::IndexJoin(_)
        | Kind::Unfold { .. }
        | Kind::Unpack { .. }
        | Kind::HalfJoin { .. }
        | Kind::Minus
        | Kind::LeftJoin { .. }
        | Kind::KeyedLeftJoin { .. }
        | Kind::KeyedJoin { .. }
        | Kind::Lateral(_) => first(),
        Kind::Union => node.children.len() == 1 && first(),
        // A merge join keeps its left input's order, and a hash join its probe side's,
        // which may be either input.
        Kind::Join { .. } => node.children.iter().any(value_ordered),
        // A path over its input expands each input row in place.
        Kind::Path {
            bound_from_left: true,
            ..
        } => node.children.last().is_some_and(value_ordered),
        _ => false,
    }
}

/// The order an operator's output keeps, from its plan node (whose children are
/// shells carrying the order of their operators) and its child operators.
fn output_order(node: &Node, children: &[Operator], role: Role) -> Vec<VarId> {
    let child = |i: usize| children.get(i).map_or(&[][..], |c| c.order.as_slice());
    // A join fills an unbound input value from the other input, which moves the row
    // out of the input's order on that variable.
    let kept = |order: &[VarId], side: &Node, other: &Node| -> Vec<VarId> {
        order
            .iter()
            .take_while(|v| side.certain.contains(v) || !other.vars.contains(v))
            .copied()
            .collect()
    };
    let order = match role {
        // Output is left-major: each left row with its matches in turn.
        Role::Merge => kept(child(0), &node.children[0], &node.children[1]),
        // Output is probe-major. Semi, anti and MINUS joins pass probe rows unchanged.
        Role::Binary => {
            let probe = binary::probe_side(node);
            match node.kind {
                Kind::HalfJoin { .. } | Kind::Minus => child(probe).to_vec(),
                _ => kept(
                    child(probe),
                    &node.children[probe],
                    &node.children[1 - probe],
                ),
            }
        }
        Role::Group => Vec::new(),
        Role::Blocking => match &node.kind {
            Kind::Sort(vars) => vars.clone(),
            _ => Vec::new(),
        },
        // The eager subtree's output is re-sorted on load when it does not hold the
        // planner's order.
        Role::Fallback if ranked(&node.kind) => Vec::new(),
        Role::Fallback => node.sorted.clone(),
        Role::Streamed => match &node.kind {
            Kind::Scan(spec) | Kind::RangeScan(spec, _) => {
                spec.cols.iter().map(|&(_, v)| v).collect()
            }
            Kind::Values(table) => table.sorted.clone(),
            Kind::Path {
                bound_from_left: false,
                ..
            } => walk::order(node),
            // A path over its input joins each batch with the paths of its start nodes.
            Kind::Path { .. } => Vec::new(),
            // Keys come in index order, one row each.
            Kind::GroupCountScan { key, .. } => vec![*key],
            Kind::Project(_)
            | Kind::Distinct
            | Kind::Filter(_)
            | Kind::Extend(..)
            | Kind::Slice { .. } => child(0).to_vec(),
            Kind::Union if children.len() == 1 => child(0).to_vec(),
            // Each input row expands in place, but an unbound input value may be
            // filled.
            Kind::IndexJoin(_) | Kind::Assign(..) | Kind::Unfold { .. } | Kind::Unpack { .. } => {
                let fills = expand::fills(node);
                child(0)
                    .iter()
                    .take_while(|v| node.children[0].certain.contains(v) || !fills.contains(v))
                    .copied()
                    .collect()
            }
            // Concatenated UNION arms are not ordered, and counts are a single row.
            _ => Vec::new(),
        },
    };
    order
        .into_iter()
        .take_while(|v| node.vars.contains(v))
        .collect()
}

/// `SELECT DISTINCT … ORDER BY` deduplicates below its sort when the query may spill
/// (item 85), so that the DISTINCT can pass rows to later passes: the DISTINCT of a
/// projection of an ORDER BY becomes a projection of an ORDER BY of a numbered DISTINCT
/// of the projection. Equal projected rows have equal sort keys when every key is a pure
/// expression of projected variables without EXISTS, and the numbered DISTINCT keeps
/// each row's first input number, on which the sort orders ties, so the rows and their
/// order are those that sorting first gives. Other shapes keep the DISTINCT above the
/// sort.
fn distinct_below_order(ctx: &Ctx, node: Node) -> Node {
    let rewrites = matches!(node.kind, Kind::Distinct)
        && node.children.len() == 1
        && matches!(&node.children[0].kind, Kind::Project(vars) if *vars == node.vars && !vars.is_empty())
        && node.children[0].children.len() == 1
        && match &node.children[0].children[0].kind {
            Kind::OrderBy { keys, limit: None } => keys.iter().all(|(key, _)| {
                !key.has_exists()
                    && crate::sparql::exprcache::impurity(key).is_none()
                    && key.var_set().iter().all(|v| node.vars.contains(v))
            }),
            _ => false,
        };
    if !rewrites {
        return node;
    }
    let mut distinct = node;
    let mut project = distinct.children.pop().expect("a projection");
    let mut order = project.children.pop().expect("an ORDER BY");
    let input = order.children.pop().expect("the sorted input");
    let Kind::OrderBy { mut keys, .. } = order.kind else {
        unreachable!("an ORDER BY")
    };
    let vars = distinct.vars.clone();
    let number = ctx.var("\u{1}distinct number");
    let below = super::plan::project(input, vars.clone(), ctx);
    let mut numbered = Node::leaf(
        Kind::Distinct,
        [vars.clone(), vec![number]].concat(),
        below.est,
        "below the ORDER BY, so that it may spill".into(),
    );
    numbered.certain = [below.certain.clone(), vec![number]].concat();
    numbered.cost = below.cost;
    numbered.children = vec![below];
    keys.push((Expr::Var(number), true));
    let mut sort = Node::leaf(
        Kind::OrderBy { keys, limit: None },
        numbered.vars.clone(),
        numbered.est,
        format!("{} then the first input row", order.desc),
    );
    sort.certain = numbered.certain.clone();
    sort.cost = numbered.cost + numbered.est * numbered.est.max(2.0).log2() * 0.5;
    sort.children = vec![numbered];
    project.children.clear();
    super::plan::project(sort, vars, ctx)
}

impl Operator {
    fn build(ctx: &Arc<Ctx>, node: Node, fallback: FallbackPolicy) -> Result<Self> {
        Self::build_for(ctx, node, fallback, true)
    }

    /// Build the operator of `node`. `needed` tells whether anything above relies on
    /// the order of its output. A hash join whose order nothing relies on, with spill
    /// on, claims no order, so that it may emit a spilled join partition by partition
    /// (item 85).
    fn build_for(
        ctx: &Arc<Ctx>,
        node: Node,
        fallback: FallbackPolicy,
        needed: bool,
    ) -> Result<Self> {
        ctx.check()?;
        let mut node = if ctx.spill.is_some() {
            distinct_below_order(ctx, node)
        } else {
            node
        };
        // Every merge-eligible join is also binary-eligible, so whether the node
        // materializes does not depend on the order its children turn out to have.
        // An operator that evaluates EXISTS streams when every EXISTS pattern does.
        let exists = own_exprs(&node.kind).iter().any(|e| e.has_exists());
        let exists_streams = !exists || exists_streams(ctx, &node.kind)?;
        let joins = exists_streams && binary::eligible(&node);
        let incremental_group = exists_streams && group::eligible(&node);
        let expanding = exists_streams && expand::eligible(&node);
        let walking = walk::eligible(&node);
        // A pattern count over an input reads all of it, as a sort does, and then has one
        // row per predicate.
        let native_blocking = matches!(
            node.kind,
            Kind::Sort(_) | Kind::OrderBy { .. } | Kind::PatternCount(_)
        ) && node.children.len() == 1;
        // A pattern count over every subject reads the pattern table, one row per
        // predicate, as a grouped count scan does.
        let pattern_counts = matches!(node.kind, Kind::PatternCount(_)) && node.children.is_empty();
        // An ordered top-k keeps a heap of its best rows, as a streaming top-k does.
        let index_topk = matches!(node.kind, Kind::OrderedTopK(_)) && exec::heap_eligible(ctx);
        // A native sort consumes cursor batches into charged state. It blocks before
        // its first output but is not an eager fallback, so strict policy admits it.
        let materializes = !(exists_streams && supported(&node.kind))
            && !joins
            && !incremental_group
            && !expanding
            && !walking
            && !index_topk
            && !pattern_counts
            && (!native_blocking || !exists_streams);
        if materializes && fallback == FallbackPolicy::RejectMaterialization {
            return Err(Error::Unsupported(format!(
                "cursor requires materialization at {}; use AllowMaterialization or eager execution",
                node.operator()
            )));
        }
        let mut info = exec::describe_node(ctx, &node);
        if ctx.graphs.is_some() {
            info.redact();
        }
        info.actual_rows = 0;
        let vars = node.vars.clone();
        // Read before the children below become shells that keep only their order.
        let value_ordered_input =
            matches!(node.kind, Kind::Distinct) && node.children.first().is_some_and(value_ordered);
        // A DISTINCT that numbers its input rows has one variable more than its input.
        let numbered_distinct = matches!(node.kind, Kind::Distinct)
            && node.children.first().is_some_and(|child| {
                child.vars.len() + 1 == node.vars.len()
                    && node.vars.last().is_some_and(|v| !child.vars.contains(v))
            });
        // A hash join that spills keeps its probe input's order when that input comes in
        // an order a caller observes.
        let probe_value_ordered = needed
            && joins
            && node
                .children
                .get(binary::probe_side(&node))
                .is_some_and(value_ordered);
        // Which children's order this operator relies on. A projection or BIND passes
        // on what its parent needs. A join that cannot merge on what the plan claims
        // reads its build input into a hash table and keeps only the probe input's
        // order. Other operators may use their inputs' order.
        let hashes = joins && ctx.spill.is_some() && !merge::eligible(&node);
        let needs: Vec<bool> = (0..node.children.len())
            .map(|i| match node.kind {
                Kind::Project(_) | Kind::Extend(..) | Kind::Slice { .. } => needed,
                _ if hashes => needed && i == binary::probe_side(&node),
                _ => true,
            })
            .collect();
        // A fallback materializes only its own operator when it reads its children as
        // input tables. Those children then stream as cursors of their own.
        let inputs = materializes
            && !native_blocking
            && !node.children.is_empty()
            && exec::reads_inputs(&node);
        let scans: Vec<Option<Node>> = if inputs {
            node.children
                .iter()
                .map(|child| {
                    (matches!(child.kind, Kind::Scan(_) | Kind::RangeScan(..))
                        && supported(&child.kind))
                    .then(|| child.clone())
                })
                .collect()
        } else {
            Vec::new()
        };
        // A count over the key runs of two scans reads those scans itself.
        let reads_scans = matches!(node.kind, Kind::CountJoinRuns { .. });
        // An ordered top-k builds its generic plan only when it needs it.
        let children = if (materializes && !native_blocking && !inputs) || reads_scans || index_topk
        {
            Vec::new()
        } else {
            let built = std::mem::take(&mut node.children)
                .into_iter()
                .zip(needs)
                .map(|(child, needed)| {
                    let shell = (
                        child.vars.clone(),
                        child.certain.clone(),
                        child.est,
                        child.cost,
                    );
                    Self::build_for(ctx, child, fallback, needed).map(|op| (shell, op))
                })
                .collect::<Result<Vec<_>>>()?;
            // Join operators read their children's shape from the node. Leave each
            // child's variables, certainty and estimates there, with the order that
            // its operator guarantees in place of the planner's claim.
            let mut children = Vec::with_capacity(built.len());
            for ((vars, certain, est, cost), op) in built {
                let mut shell = Node::leaf(Kind::Empty, vars, est, String::new());
                shell.certain = certain;
                shell.cost = cost;
                shell.sorted = op.order.clone();
                node.children.push(shell);
                children.push(op);
            }
            children
        };
        // A merge join needs both inputs ordered on its key. When an input cannot
        // promise that order, the hash join takes its place.
        let incremental_merge = !materializes && merge::eligible(&node);
        let incremental_binary = !incremental_merge && joins;
        let growing = matches!(
            node.kind,
            Kind::Extend(..)
                | Kind::Filter(_)
                | Kind::RangeScan(..)
                | Kind::Distinct
                | Kind::GroupCountScan { .. }
                | Kind::CountJoinRuns { .. }
        ) || materializes
            || native_blocking
            || index_topk
            || incremental_merge
            || incremental_binary
            || incremental_group
            || expanding
            || walking
            || exists;
        let mut order = output_order(
            &node,
            &children,
            if incremental_merge {
                Role::Merge
            } else if incremental_binary {
                Role::Binary
            } else if incremental_group {
                Role::Group
            } else if native_blocking {
                Role::Blocking
            } else if materializes {
                Role::Fallback
            } else {
                Role::Streamed
            },
        );
        let merge = incremental_merge
            .then(|| merge::Merge::new(ctx, &node))
            .transpose()?;
        let group = incremental_group
            .then(|| group::Group::new(ctx, &node))
            .transpose()?;
        if incremental_binary && !needed && ctx.spill.is_some() {
            order.clear();
        }
        let ripple = (incremental_binary && ripple::eligible(&node))
            .then(|| ripple::Ripple::new(ctx, &node))
            .transpose()?;
        let binary = (incremental_binary && ripple.is_none())
            .then(|| binary::Binary::new(ctx, &node, !order.is_empty() || probe_value_ordered))
            .transpose()?;
        if binary.is_some()
            && let Kind::Join {
                algo: super::plan::JoinAlgo::Merge,
                ..
            } = node.kind
        {
            // the plan names what runs
            info.operator = "HashJoin".into();
            info.description = format!(
                "{} [planned as a merge join: an input is not ordered on the key, so a hash join runs]",
                info.description
            );
        }
        let heap = native_blocking && !materializes && topk::eligible(ctx, &node);
        let reason = (materializes || native_blocking || exists || index_topk).then(|| {
            if let Kind::OrderedTopK(spec) = &node.kind {
                format!(
                    "{} reads the index in value order where ids follow values, and the rest of its range a morsel at a time, keeping only its best {} rows under the memory budget before output",
                    node.operator(),
                    spec.k
                )
            } else if let (true, Kind::OrderBy { limit: Some(k), .. }) = (heap, &node.kind) {
                format!(
                    "{} reads all input but keeps only its best {k} rows under the memory budget before output",
                    node.operator()
                )
            } else if native_blocking {
                format!(
                    "{} consumes all input under the memory budget before output",
                    node.operator()
                )
            } else if !materializes {
                format!(
                    "{} answers EXISTS from a key set of its pattern, built under the memory budget on first demand, or evaluates it once per distinct outer key",
                    node.operator()
                )
            } else {
                format!("{} has no resumable cursor implementation", node.operator())
            }
        });
        let bind_reuse = match &node.kind {
            Kind::Extend(_, expr) => bind::Reuse::new(ctx, expr).map(Box::new),
            _ => None,
        };
        let state = if let Some(merge) = merge {
            State::Merge(Box::new(merge))
        } else if let Some(binary) = binary {
            State::Binary(Box::new(binary))
        } else if let Some(ripple) = ripple {
            State::Ripple(Box::new(ripple))
        } else if let Some(group) = group {
            State::Group(Box::new(group))
        } else if filter::eligible(&node, &children) {
            State::Filter(Box::new(filter::Filter::new(node)))
        } else if index_topk {
            State::IndexTopK(Box::new(topk::IndexTopK::new(node, fallback)))
        } else if heap {
            State::TopK(Box::new(topk::TopK::new(ctx, &node, &children[0].vars)?))
        } else if expanding {
            State::Expand(Box::new(expand::Expand::new(node)))
        } else if walking {
            State::Walk(Box::new(walk::Walk::new(ctx, &node)?))
        } else if native_blocking {
            State::Blocking {
                node,
                loaded: None,
                at: 0,
                external: None,
            }
        } else if materializes {
            State::Fallback {
                node,
                loaded: None,
                at: 0,
                inputs,
                scans,
            }
        } else {
            match &node.kind {
                Kind::Scan(spec) => State::Scan(Scan::new(spec.clone(), None)),
                Kind::RangeScan(spec, range) => State::Scan(Scan::new(spec.clone(), Some(range))),
                Kind::Values(_) => {
                    let Kind::Values(table) = node.kind else {
                        unreachable!()
                    };
                    State::Values { table, at: 0 }
                }
                Kind::Empty => State::Empty,
                Kind::CountScan { .. }
                | Kind::CountDistinctScan { .. }
                | Kind::CountFilterScan { .. }
                | Kind::CountJoinRuns { .. } => State::Scalar(node),
                Kind::GroupCountScan { .. } | Kind::PatternCount(_) => State::Counts {
                    node,
                    loaded: None,
                    at: 0,
                },
                Kind::Distinct => State::Distinct(distinct::Distinct::new(
                    ctx,
                    &node.vars,
                    &children[0].order,
                    value_ordered_input,
                    numbered_distinct,
                )?),
                Kind::Slice { offset, limit } => State::Slice {
                    offset: *offset,
                    remaining: *limit,
                },
                Kind::Union => State::Union { at: 0 },
                _ => State::Unary(node),
            }
        };
        if let State::Distinct(distinct) = &state
            && distinct.may_reorder()
        {
            order.clear();
        }
        Ok(Self {
            state,
            children,
            info,
            vars,
            rows: 0,
            done: false,
            materializes,
            growing,
            reason,
            bind_reuse,
            order,
            #[cfg(debug_assertions)]
            last: None,
        })
    }

    /// Check, in debug builds, that a batch keeps the operator's order, within the
    /// batch and from the last row of the previous one.
    #[cfg(debug_assertions)]
    fn check_order(&mut self, ctx: &Ctx, table: &Table) {
        if self.order.is_empty() || table.is_empty() {
            return;
        }
        let columns: Vec<&[Id]> = self
            .order
            .iter()
            .map(|v| {
                let c = table
                    .col_of(*v)
                    .expect("an operator's order names its output variables");
                table.cols[c].as_slice()
            })
            .collect();
        let describe = |op: &Self| {
            let names: Vec<String> = op.order.iter().map(|v| ctx.var_name(*v)).collect();
            format!("{} claims order on {names:?}", op.info.operator)
        };
        if let Some(last) = &self.last {
            let first: Vec<Id> = columns.iter().map(|c| c[0]).collect();
            assert!(
                *last <= first,
                "{}, but a batch starts before the end of the previous one",
                describe(self)
            );
        }
        for row in 1..table.len {
            let ordering = columns
                .iter()
                .map(|c| c[row - 1].cmp(&c[row]))
                .find(|o| o.is_ne())
                .unwrap_or(std::cmp::Ordering::Equal);
            assert!(
                ordering.is_le(),
                "{}, but row {row} of a batch is out of order",
                describe(self)
            );
        }
        self.last = Some(columns.iter().map(|c| c[table.len - 1]).collect());
    }

    fn describe(&self, ctx: &Ctx) -> CursorPlan {
        // The subtree an eager kernel runs itself, in the shape of its eager plan. A
        // fallback materializes it, while a scalar kernel reads its scans as runs.
        fn eager(mut operator: PlanInfo, reason: &str, materializes: bool) -> CursorPlan {
            let children = std::mem::take(&mut operator.children);
            CursorPlan {
                operator,
                materializes,
                full_input_before_output: materializes,
                growing_state: materializes,
                complete: false,
                reason: Some(reason.into()),
                children: children
                    .into_iter()
                    .map(|c| eager(c, reason, materializes))
                    .collect(),
            }
        }
        let described = |node: &Node| {
            let mut d = exec::describe(ctx, node);
            if ctx.graphs.is_some() {
                d.redact();
            }
            d.children
        };
        let children = match &self.state {
            State::Fallback {
                node,
                inputs: false,
                ..
            } => described(node)
                .into_iter()
                .map(|c| eager(c, "inside an eager fallback subtree", true))
                .collect(),
            State::Scalar(node) if !node.children.is_empty() => described(node)
                .into_iter()
                .map(|c| eager(c, "read by its parent", false))
                .collect(),
            _ => self.children.iter().map(|c| c.describe(ctx)).collect(),
        };
        CursorPlan {
            operator: self.info.clone(),
            materializes: self.materializes,
            full_input_before_output: self.materializes
                || matches!(
                    self.state,
                    State::Binary(_)
                        | State::Group(_)
                        | State::Scalar(_)
                        | State::Counts { .. }
                        | State::Blocking { .. }
                        | State::TopK(_)
                        | State::IndexTopK(_)
                ),
            growing_state: self.growing,
            complete: false,
            reason: self.reason.clone(),
            children,
        }
    }

    fn update_plan(&self, plan: &mut CursorPlan) {
        plan.operator.actual_rows = self.info.actual_rows;
        plan.operator.time_ms = self.info.time_ms;
        plan.complete = self.done;
        if self.info.counters.is_some() {
            plan.operator.counters.clone_from(&self.info.counters);
        }
        fn update(info: &PlanInfo, plan: &mut CursorPlan) {
            plan.operator.actual_rows = info.actual_rows;
            plan.operator.time_ms = info.time_ms;
            plan.complete = info.actual_rows >= 0;
            if plan.operator.description != info.description {
                plan.operator.description.clone_from(&info.description);
            }
            plan.operator.counters.clone_from(&info.counters);
            plan.operator.fidelity.clone_from(&info.fidelity);
            plan.operator.cached = info.cached;
            for (info, plan) in info.children.iter().zip(&mut plan.children) {
                update(info, plan);
            }
        }
        match &self.state {
            State::Fallback {
                loaded: Some(_), ..
            } if self.materializes => update(&self.info, plan),
            State::Scalar(_) if self.done => {
                for (info, plan) in self.info.children.iter().zip(&mut plan.children) {
                    update(info, plan);
                }
            }
            _ => {}
        }
        // The generic plan of an ordered top-k joins the cursor plan once it is built.
        if let State::IndexTopK(topk) = &self.state
            && plan.children.is_empty()
            && let Some(generic) = topk.generic_plan()
        {
            plan.children.push(generic.clone());
        }
        for (child, plan) in self.children.iter().zip(&mut plan.children) {
            child.update_plan(plan);
        }
    }

    fn restrict_count_scan(&mut self, ctx: &Ctx, key: VarId) {
        if let State::Scan(scan) = &mut self.state
            && !scan.spec.dedup
            && scan.filter.is_empty()
        {
            scan.spec.cols.retain(|(_, v)| *v == key);
            self.vars.retain(|v| *v == key);
            let kept = self.order.iter().take_while(|v| **v == key).count();
            self.order.truncate(kept);
            self.info.columns = vec![ctx.var_name(key)];
            self.info.sorted_on = vec![ctx.var_name(key)];
        }
    }

    fn count_input(&mut self, ctx: &Arc<Ctx>, options: &CursorOptions) -> Result<Option<u64>> {
        let State::Merge(merge) = &mut self.state else {
            return Ok(None);
        };
        if !merge.supports_count() {
            return Ok(None);
        }
        let began = Instant::now();
        let count = merge.count(ctx, &mut self.children, options)?;
        self.info.time_ms += began.elapsed().as_secs_f64() * 1000.0;
        self.rows =
            usize::try_from(count).map_err(|_| Error::invalid("cursor row count overflow"))?;
        ctx.check_rows(self.rows)?;
        ctx.produced(self.rows)?;
        self.info.actual_rows = self.rows.min(i64::MAX as usize) as i64;
        self.done = true;
        ctx.check()?;
        Ok(Some(count))
    }

    fn next_shared(
        &mut self,
        ctx: &Arc<Ctx>,
        options: &CursorOptions,
        vars: &[VarId],
        want: usize,
    ) -> Result<SharedAttempt> {
        if self.done {
            return Ok(SharedAttempt::Ready(None));
        }
        ctx.check()?;
        let began = Instant::now();
        let cap = want
            .min(options.batch_rows)
            .min(crate::index::BLOCK_ROWS)
            .min((options.batch_bytes / self.vars.len().max(1).saturating_mul(8)).max(1));
        let result = match &mut self.state {
            State::Scan(scan) => {
                let result = scan.next_shared(ctx, vars, cap);
                self.done = scan.done();
                result
            }
            State::Unary(node) if matches!(node.kind, Kind::Project(_)) => {
                let result = self.children[0].next_shared(ctx, options, vars, cap);
                self.done = self.children[0].done;
                result
            }
            State::Slice {
                offset: 0,
                remaining,
            } => {
                if *remaining == Some(0) {
                    self.done = true;
                    Ok(SharedAttempt::Ready(None))
                } else {
                    let result = self.children[0].next_shared(
                        ctx,
                        options,
                        vars,
                        cap.min(remaining.unwrap_or(usize::MAX)),
                    );
                    if let Ok(SharedAttempt::Ready(Some(batch))) = &result
                        && let Some(remaining) = remaining
                    {
                        *remaining -= batch.end - batch.start;
                    }
                    self.done = *remaining == Some(0) || self.children[0].done;
                    result
                }
            }
            _ => Ok(SharedAttempt::Unsupported),
        };
        self.info.time_ms += began.elapsed().as_secs_f64() * 1000.0;
        let attempt = result?;
        if let SharedAttempt::Ready(batch) = &attempt {
            if let Some(batch) = batch {
                let rows = batch.end - batch.start;
                self.rows = self
                    .rows
                    .checked_add(rows)
                    .ok_or_else(|| Error::invalid("cursor operator row count overflow"))?;
                ctx.check_rows(self.rows)?;
                ctx.produced(rows)?;
                self.info.actual_rows = self.rows.min(i64::MAX as usize) as i64;
            } else {
                self.done = true;
            }
        }
        ctx.check()?;
        Ok(attempt)
    }

    fn next(
        &mut self,
        ctx: &Arc<Ctx>,
        options: &CursorOptions,
        want: usize,
    ) -> Result<Option<Buffer>> {
        if self.done {
            return Ok(None);
        }
        ctx.check()?;
        let began = Instant::now();
        let row_bytes = self.vars.len().max(1).saturating_mul(8);
        let cap = want
            .min(options.batch_rows)
            .min((options.batch_bytes / row_bytes).max(1));
        // Leave headroom for layouts and unary scratch; allocation itself still checks.
        let cap = cap.min(ctx.rows_within_budget(self.vars.len()).max(1));
        let result = (|| {
            let result = self.next_inner(ctx, options, cap);
            self.info.time_ms += began.elapsed().as_secs_f64() * 1000.0;
            let mut batch = result?;
            if let Some(b) = &mut batch {
                b.set_order(&self.order)?;
                #[cfg(debug_assertions)]
                self.check_order(ctx, &b.table);
                self.rows = self
                    .rows
                    .checked_add(b.table.len)
                    .ok_or_else(|| Error::invalid("cursor operator row count overflow"))?;
                ctx.check_rows(self.rows)?;
                // Eager kernels already counted all their operator work during execution.
                if !matches!(
                    self.state,
                    State::Fallback { .. }
                        | State::Scalar(_)
                        | State::Counts { .. }
                        | State::Expand(_)
                ) {
                    ctx.produced(b.table.len)?;
                }
                // a fallback counts the rows it emitted; `materializedRows` holds its table
                self.info.actual_rows = self.rows.min(i64::MAX as usize) as i64;
            } else {
                self.done = true;
            }
            ctx.check()?;
            Ok(batch)
        })();
        // Input exhaustion alone is not successful completion. A failed output
        // charge or final request check leaves this operator incomplete.
        if result.is_err() {
            self.done = false;
        }
        result
    }

    fn next_inner(
        &mut self,
        ctx: &Arc<Ctx>,
        options: &CursorOptions,
        cap: usize,
    ) -> Result<Option<Buffer>> {
        loop {
            ctx.check()?;
            let batch = match &mut self.state {
                State::Scalar(node) => {
                    let (table, mut info) = exec::execute(ctx, node)?;
                    if ctx.graphs.is_some() {
                        info.redact();
                    }
                    // the scans a count over key runs read itself
                    self.info.children = info.children;
                    let charge = OwnedCharge::new(ctx, capacity_bytes(&table))?;
                    debug_assert!(table.len <= 1);
                    self.done = true;
                    Some(Buffer { table, charge })
                }
                State::Counts { node, loaded, at } => {
                    if loaded.is_none() {
                        let (table, _) = exec::execute(ctx, node)?;
                        let charge = OwnedCharge::new(ctx, capacity_bytes(&table))?;
                        *loaded = Some(Buffer { table, charge });
                    }
                    let table = &loaded.as_ref().expect("loaded counts").table;
                    let batch = copy_rows(ctx, table, *at, cap)?;
                    if let Some(batch) = &batch {
                        *at += batch.table.len;
                    }
                    self.done = *at == table.len;
                    batch
                }
                State::Merge(merge) => {
                    let batch = merge.next(ctx, &mut self.children, options, &self.vars, cap)?;
                    if merge.spooled_runs > 0 {
                        let mut counters = self.info.counters.take().unwrap_or_default();
                        counters.insert("spilledBytes".into(), merge.spilled.into());
                        counters.insert("spooledRuns".into(), merge.spooled_runs.into());
                        self.info.counters = Some(counters);
                    }
                    batch
                }
                State::Binary(binary) => {
                    let batch = binary.next(ctx, &mut self.children, options, &self.vars, cap)?;
                    if let Some(spill) = &binary.spill {
                        let mut counters = self.info.counters.take().unwrap_or_default();
                        let stats = &spill.stats;
                        counters.insert("spilledBytes".into(), stats.written.into());
                        counters.insert("spillPartitions".into(), stats.partitions.into());
                        counters.insert("spillLevels".into(), stats.levels.into());
                        if stats.chunks > 0 {
                            counters.insert("spillChunks".into(), stats.chunks.into());
                        }
                        if binary.ordered {
                            counters.insert("spillMergePasses".into(), stats.merge_passes.into());
                        }
                        self.info.counters = Some(counters);
                    }
                    if batch.is_none() {
                        binary.release_working_sets();
                    }
                    batch
                }
                State::Ripple(ripple) => {
                    let batch = ripple.next(ctx, &mut self.children, options, &self.vars, cap)?;
                    if batch.is_none() {
                        ripple.release_working_sets();
                    }
                    batch
                }
                State::Group(group) => {
                    let batch = group.next(ctx, &mut self.children[0], options, &self.vars, cap)?;
                    if group.spilled > 0 {
                        let mut counters = self.info.counters.take().unwrap_or_default();
                        counters.insert("spilledBytes".into(), group.spilled.into());
                        counters.insert("spillPasses".into(), (group.passes + 1).into());
                        self.info.counters = Some(counters);
                    }
                    batch
                }
                State::Filter(filter) => {
                    let batch = filter.next(ctx, &mut self.children[0], options, cap)?;
                    self.done = filter.done(self.children[0].done);
                    batch
                }
                State::Walk(walk) => {
                    let batch = walk.next(ctx, &mut self.children, options, &self.vars, cap)?;
                    self.done = walk.done();
                    batch
                }
                State::Expand(expand) => {
                    let batch = expand.next(ctx, &mut self.children, options, &self.vars, cap)?;
                    let input = self.children.last().is_none_or(|c| c.done);
                    self.done = expand.done(input);
                    batch
                }
                State::Distinct(distinct) => {
                    let batch = match self.children[0].next(ctx, options, cap)? {
                        Some(mut buffer) => {
                            if let Some(&number) = self.vars.last() {
                                distinct.number(&mut buffer, number)?;
                            }
                            distinct.apply(ctx, &mut buffer)?;
                            Some(buffer)
                        }
                        // Rows that did not fit in memory are deduplicated last.
                        None => distinct.drain(ctx, &self.vars)?,
                    };
                    if distinct.spilled > 0 {
                        let mut counters = self.info.counters.take().unwrap_or_default();
                        counters.insert("spilledBytes".into(), distinct.spilled.into());
                        counters.insert("spillPasses".into(), (distinct.passes + 1).into());
                        self.info.counters = Some(counters);
                    }
                    self.done = self.children[0].done && !distinct.has_more();
                    match batch {
                        Some(batch) => Some(batch),
                        None if self.done => None,
                        // A batch the set emptied is still a batch.
                        None => Some(Buffer::new(ctx, &self.vars, 0)?),
                    }
                }
                State::Empty => None,
                State::Values { table, at } => {
                    let b = copy_rows(ctx, table, *at, cap)?;
                    if let Some(b) = &b {
                        *at += b.table.len;
                    }
                    self.done = *at == table.len;
                    b
                }
                State::Scan(scan) => {
                    let b = scan.next(ctx, &self.vars, cap)?;
                    self.done = scan.done();
                    b
                }
                State::TopK(topk) => {
                    let b = topk.next(ctx, &mut self.children[0], options, cap)?;
                    self.done = topk.done();
                    b
                }
                State::IndexTopK(topk) => {
                    let b = topk.next(ctx, &mut self.children, options, &mut self.info, cap)?;
                    self.done = topk.done(&self.children);
                    b
                }
                State::Blocking {
                    node,
                    loaded,
                    at,
                    external,
                } => 'blocking: {
                    if let Some(sort) = external {
                        break 'blocking sort.next(ctx, cap)?;
                    }
                    if loaded.is_none() {
                        // A sort with somewhere to spill writes sorted runs when its
                        // input outgrows memory and then merges them.
                        let mut spill = if extsort::eligible(node) {
                            extsort::External::new(ctx, node, &self.children[0].vars)
                        } else {
                            None
                        };
                        let input = match &mut spill {
                            Some(sort) => {
                                let input =
                                    extsort::load(ctx, sort, &mut self.children[0], options)?;
                                if input.is_none() {
                                    let mut counters =
                                        self.info.counters.take().unwrap_or_default();
                                    counters.insert("spilledBytes".into(), sort.written.into());
                                    counters.insert("spillRuns".into(), sort.run_count.into());
                                    counters.insert("mergePasses".into(), sort.passes.into());
                                    self.info.counters = Some(counters);
                                }
                                input
                            }
                            None => {
                                let mut input = Buffer::new(ctx, &self.children[0].vars, 0)?;
                                // The concatenated input keeps the child's order, which
                                // lets a sort on a prefix of it skip reordering.
                                input.set_order(&self.children[0].order)?;
                                // Blocking input demand is independent of the requested
                                // output prefix.
                                while let Some(batch) =
                                    self.children[0].next(ctx, options, options.batch_rows)?
                                {
                                    merge::append(
                                        ctx,
                                        &mut input,
                                        &batch.table,
                                        0..batch.table.len,
                                    )?;
                                }
                                Some(input)
                            }
                        };
                        let Some(mut input) = input else {
                            let mut sort = Box::new(spill.expect("a spilled sort"));
                            let batch = sort.next(ctx, cap)?;
                            *external = Some(sort);
                            break 'blocking batch;
                        };
                        // Row reordering, output copies and their index arrays can
                        // coexist with the input columns. ORDER BY charges them itself,
                        // and a Sort reserves them here before sorting.
                        if matches!(node.kind, Kind::Sort(_)) {
                            input.charge.resize(
                                capacity_bytes(&input.table)
                                    .saturating_mul(3)
                                    .saturating_add(input.table.len as u64 * 24 + 1024),
                            )?;
                        }
                        let mut report = ExprReport::default();
                        input.table = exec::apply_blocking(ctx, node, input.table, &mut report)?;
                        input.reconcile()?;
                        *loaded = Some(input);
                    }
                    let table = &loaded.as_ref().expect("loaded blocking state").table;
                    let batch = copy_rows(ctx, table, *at, cap)?;
                    if let Some(batch) = &batch {
                        *at += batch.table.len;
                    }
                    self.done = *at == table.len;
                    batch
                }
                State::Fallback {
                    node,
                    loaded,
                    at,
                    inputs,
                    scans,
                } => {
                    if loaded.is_none() {
                        let (mut output, mut info) = if *inputs {
                            let children = &mut self.children;
                            let mut collect = |i: usize| match &scans[i] {
                                Some(scan) => {
                                    let (output, info) = exec::execute_owned(ctx, scan)?;
                                    let table = &output.table;
                                    let child = &mut children[i];
                                    child.rows = table.len;
                                    child.info.actual_rows = info.actual_rows;
                                    child.info.time_ms = info.time_ms;
                                    child.done = true;
                                    Ok((output, info))
                                }
                                None => collect_input(ctx, &mut children[i], options),
                            };
                            exec::execute_with_inputs(ctx, node, &mut collect)?
                        } else {
                            exec::execute_owned(ctx, node)?
                        };
                        if *inputs {
                            // The child operators describe themselves.
                            info.children.clear();
                        }
                        if ctx.graphs.is_some() {
                            info.redact();
                        }
                        info.time_ms = self.info.time_ms;
                        let mut counters = info.counters.take().unwrap_or_default();
                        counters.insert("materializedRows".into(), output.table.len.into());
                        info.counters = Some(counters);
                        info.actual_rows = self.rows.min(i64::MAX as usize) as i64;
                        self.info = info;
                        #[cfg(feature = "failpoints")]
                        if output.lease.is_some() {
                            crate::algo::observe_projection(
                                crate::algo::ProjectionEvent::Materialized(
                                    "algorithm-fallback-transfer",
                                    capacity_bytes(&output.table) as usize,
                                ),
                            );
                        }
                        let mut charge = if output.lease.is_some() {
                            OwnedCharge::adopt(ctx, &mut output.lease)?
                        } else {
                            OwnedCharge::new(ctx, capacity_bytes(&output.table))?
                        };
                        let mut table = output.table;
                        // Parents rely on the planner's order, which the eager result
                        // reports only when its rows hold it.
                        if !table.sorted.starts_with(&self.order) {
                            // Row indices and the reordered copy coexist with the input.
                            charge.resize(
                                capacity_bytes(&table)
                                    .saturating_mul(2)
                                    .saturating_add(table.len as u64 * 8),
                            )?;
                            table.sort_by_vars(&self.order);
                            ctx.check()?;
                            charge.resize(capacity_bytes(&table))?;
                        }
                        *loaded = Some(Buffer { table, charge });
                    }
                    let table = &loaded.as_ref().expect("loaded fallback").table;
                    let b = copy_rows(ctx, table, *at, cap)?;
                    if let Some(b) = &b {
                        *at += b.table.len;
                    }
                    self.done = *at == table.len;
                    b
                }
                State::Unary(node) => {
                    let Some(mut b) = self.children[0].next(ctx, options, cap)? else {
                        return Ok(None);
                    };
                    if let Kind::Project(vars) = &node.kind {
                        b.project(vars)?;
                        self.done = self.children[0].done;
                        if b.table.is_empty() && !self.done {
                            continue;
                        }
                        return Ok(Some(b));
                    }
                    if matches!(node.kind, Kind::Extend(..)) {
                        b.charge.resize(
                            capacity_bytes(&b.table)
                                .saturating_add(b.table.len as u64 * 8)
                                .saturating_add(b.table.width() as u64 * 40 + 128),
                        )?;
                    }
                    // Scratch vectors and a possible new BIND column coexist with input.
                    let scratch = OwnedCharge::new(
                        ctx,
                        b.table.len as u64 * 16 + ctx.nvars() as u64 * 16 + 128,
                    )?;
                    let mut report = ExprReport::default();
                    let reused = match (&mut self.bind_reuse, &node.kind) {
                        (Some(reuse), Kind::Extend(target, expr)) => {
                            reuse.apply(ctx, &mut b.table, *target, expr)?
                        }
                        _ => false,
                    };
                    if !reused {
                        b.table = exec::apply_unary(ctx, node, b.table, &mut report)?;
                    }
                    b.reconcile()?;
                    drop(scratch);
                    self.done = self.children[0].done;
                    Some(b)
                }
                State::Slice { offset, remaining } => {
                    if *remaining == Some(0) {
                        self.done = true;
                        return Ok(None);
                    }
                    let needed = remaining.map_or(cap, |n| n.saturating_add(*offset).min(cap));
                    let Some(mut b) = self.children[0].next(ctx, options, needed)? else {
                        return Ok(None);
                    };
                    let skip = (*offset).min(b.table.len);
                    *offset -= skip;
                    let n = remaining.map_or(b.table.len - skip, |r| r.min(b.table.len - skip));
                    b.slice_in_place(skip, n)?;
                    if let Some(r) = remaining {
                        *r -= n;
                    }
                    self.done = *remaining == Some(0) || self.children[0].done;
                    Some(b)
                }
                State::Union { at } => {
                    if *at == self.children.len() {
                        self.done = true;
                        return Ok(None);
                    }
                    match self.children[*at].next(ctx, options, cap)? {
                        Some(mut b) => {
                            b.project(&self.vars)?;
                            if self.children[*at].done {
                                *at += 1;
                            }
                            self.done = *at == self.children.len();
                            Some(b)
                        }
                        None => {
                            *at += 1;
                            continue;
                        }
                    }
                }
            };
            match batch {
                Some(b) if b.table.is_empty() && !self.done => continue,
                Some(b) if b.table.is_empty() => return Ok(None),
                b => return Ok(b),
            }
        }
    }
}

/// Collect a child cursor's output as the input table of an eager operator. The table
/// keeps the order of its operator, and its charge passes to the eager operator, which
/// holds its input tables while it runs.
fn collect_input(
    ctx: &Arc<Ctx>,
    child: &mut Operator,
    options: &CursorOptions,
) -> Result<(super::table::Materialized, PlanInfo)> {
    fn algorithm(n: &Node) -> bool {
        matches!(n.kind, Kind::AlgoCall(_)) || n.children.iter().any(algorithm)
    }
    fn owns_algorithm(op: &Operator) -> bool {
        matches!(&op.state,State::Fallback {node,..} if algorithm(node))
            || op.children.iter().any(owns_algorithm)
    }
    let keep_lease = owns_algorithm(child);
    let mut input = Buffer::new(ctx, &child.vars, 0)?;
    input.set_order(&child.order)?;
    while let Some(batch) = child.next(ctx, options, options.batch_rows)? {
        merge::append(ctx, &mut input, &batch.table, 0..batch.table.len)?;
    }
    if !keep_lease {
        let Buffer { mut table, charge } = input;
        drop(charge);
        table.sorted.clone_from(&child.order);
        return Ok((super::table::Materialized::plain(table), child.info.clone()));
    }
    input.set_order(&child.order)?;
    let info = child.info.clone();
    Ok((
        super::table::Materialized {
            table: input.table,
            lease: Some(input.charge.into_lease()),
        },
        info,
    ))
}

fn copy_rows(ctx: &Arc<Ctx>, table: &Table, at: usize, cap: usize) -> Result<Option<Buffer>> {
    let n = cap.min(table.len.saturating_sub(at));
    if n == 0 {
        return Ok(None);
    }
    let mut b = Buffer::new(ctx, &table.vars, n)?;
    for (to, from) in b.table.cols.iter_mut().zip(&table.cols) {
        to.extend_from_slice(&from[at..at + n]);
    }
    b.table.len = n;
    b.reconcile()?;
    Ok(Some(b))
}

struct Scan {
    spec: ScanSpec,
    ranges: Vec<(Key, Key, bool)>,
    at: usize,
    position: Option<Key>,
    last: Option<Key>,
    filter: Vec<super::expr::Expr>,
    /// the variable of the range filter
    var: VarId,
    base_checked: bool,
    base: Option<BaseScan>,
}

struct BaseScan {
    /// read-ahead over the blocks of the current range
    ahead: crate::index::ReadAhead,
    next: usize,
    end: usize,
    block: Option<crate::index::Block>,
    row: usize,
    row_end: usize,
    passes: bool,
    _charge: Arc<OwnedCharge>,
}

impl Scan {
    fn new(spec: ScanSpec, range: Option<&RangeSpec>) -> Self {
        let ranges = match range {
            None => vec![(pad(&spec.prefix, 0), pad(&spec.prefix, u64::MAX), true)],
            Some(range) => range
                .ranges
                .iter()
                .map(|r| {
                    let mut lo = pad(&spec.prefix, 0);
                    let mut hi = pad(&spec.prefix, u64::MAX);
                    lo[spec.prefix.len()] = r.lo;
                    hi[spec.prefix.len()] = r.hi;
                    (lo, hi, r.exact)
                })
                .collect(),
        };
        Self {
            spec,
            ranges,
            at: 0,
            position: None,
            last: None,
            filter: range.map_or_else(Vec::new, |r| r.filter.clone()),
            var: range.map_or(0, |r| r.var),
            base_checked: false,
            base: None,
        }
    }

    fn done(&self) -> bool {
        self.at == self.ranges.len()
    }

    fn next_shared(&mut self, ctx: &Arc<Ctx>, vars: &[VarId], cap: usize) -> Result<SharedAttempt> {
        if self.done() {
            return Ok(SharedAttempt::Ready(None));
        }
        if !ctx.snap.delta.is_empty()
            || self.spec.dedup
            || !self.spec.eqs.is_empty()
            || !self.filter.is_empty()
            || self.ranges.iter().any(|r| !r.2)
            || vars.len() > 4
        {
            return Ok(SharedAttempt::Unsupported);
        }
        let mut columns = [0; 4];
        for (column, var) in columns.iter_mut().zip(vars) {
            let Some((source, _)) = self.spec.cols.iter().find(|(_, v)| v == var) else {
                return Ok(SharedAttempt::Unsupported);
            };
            *column = *source;
        }
        if !self.base_checked {
            let cache_bytes = crate::index::BLOCK_ROWS as u64 * 32 + 512;
            // Leave room for consumers' decoding scratch and an owned fallback.
            let output_bytes =
                cap as u64 * (vars.len() as u64 * 8 + 16) + ctx.nvars() as u64 * 16 + 4096;
            if ctx.memory_remaining() < cache_bytes.saturating_add(output_bytes) {
                return Ok(SharedAttempt::Unsupported);
            }
            let charge = OwnedCharge::new(ctx, cache_bytes)?;
            self.base = Some(BaseScan {
                ahead: Default::default(),
                next: 0,
                end: 0,
                block: None,
                row: 0,
                row_end: 0,
                passes: false,
                _charge: Arc::new(charge),
            });
            self.base_checked = true;
        }
        let Some(base) = &mut self.base else {
            return Ok(SharedAttempt::Unsupported);
        };
        let index = ctx.snap.perm(self.spec.perm);
        let mask = exec::scan_mask(ctx, &self.spec);
        while self.at < self.ranges.len() {
            ctx.check()?;
            let (lo, hi, _) = self.ranges[self.at];
            if base.next == 0 && base.end == 0 && base.block.is_none() {
                (base.next, base.end) = index.key_block_range(&lo, &hi);
            }
            if base.block.is_none() {
                if base.next == base.end {
                    self.at += 1;
                    base.next = 0;
                    base.end = 0;
                    base.ahead = Default::default();
                    continue;
                }
                if Arc::strong_count(&base._charge) > 1 {
                    base._charge = Arc::new(OwnedCharge::new(
                        ctx,
                        crate::index::BLOCK_ROWS as u64 * 32 + 512,
                    )?);
                }
                let metadata = &index.blocks[base.next];
                let whole = metadata.first >= lo && metadata.last <= hi;
                let columns = |b: usize| {
                    let m = &index.blocks[b];
                    if m.first >= lo && m.last <= hi {
                        mask
                    } else {
                        mask | crate::index::bound_cols(&lo, &hi)
                    }
                };
                let wanted = columns(base.next);
                let block = match ctx.snap.cache.get_cols_cached(index, base.next, wanted) {
                    Some(block) => block,
                    None => {
                        let hint = base.ahead.before(
                            index,
                            &ctx.snap.cache,
                            base.next,
                            base.end,
                            &columns,
                        );
                        ctx.snap
                            .cache
                            .get_cols_hinted(index, base.next, wanted, hint)?
                    }
                };
                (base.row, base.row_end) = if whole {
                    (0, block.len())
                } else {
                    block.key_range(&lo, &hi)
                };
                base.next += 1;
                base.passes = exec::block_passes(&self.spec, &block, base.row, base.row_end);
                base.block = Some(block);
            }
            if base.row == base.row_end {
                base.block = None;
                continue;
            }
            let block = base.block.as_ref().expect("loaded immutable block");
            if !base.passes {
                // No row was consumed: the owned path can resume this block.
                return Ok(SharedAttempt::Unsupported);
            }
            let charge = OwnedCharge::new(ctx, 512)?;
            let end = base.row_end.min(base.row.saturating_add(cap));
            let batch = SharedBatch {
                block: block.clone(),
                columns,
                start: base.row,
                end,
                _block_charge: base._charge.clone(),
                _charge: charge,
            };
            base.row = end;
            if base.row == base.row_end {
                base.block = None;
                if base.next == base.end {
                    self.at += 1;
                    base.next = 0;
                    base.end = 0;
                    base.ahead = Default::default();
                }
            }
            return Ok(SharedAttempt::Ready(Some(batch)));
        }
        Ok(SharedAttempt::Ready(None))
    }

    fn visit(
        &mut self,
        ctx: &Ctx,
        columns: &[usize],
        key: Key,
        out: &mut Buffer,
        cap: usize,
        step: &mut ScanStep,
    ) -> Result<bool> {
        step.visited += 1;
        if step.visited.is_multiple_of(1024) {
            ctx.check()?;
        }
        step.next = successor(key);
        if !self.spec.graph.accepts(key[self.spec.graph_col])
            || self.spec.eqs.iter().any(|&(a, b)| key[a] != key[b])
        {
            return Ok(true);
        }
        let mut projection = [0u64; 4];
        for (i, &column) in columns.iter().enumerate() {
            projection[i] = key[column];
        }
        if self.spec.dedup && self.last == Some(projection) {
            return Ok(true);
        }
        if self.spec.dedup {
            self.last = Some(projection);
        }
        for (column, &source) in out.table.cols.iter_mut().zip(columns) {
            column.push(Id(key[source]));
        }
        out.table.len += 1;
        if out.table.len == cap {
            step.stopped = true;
            return Ok(false);
        }
        Ok(true)
    }

    fn next_base(
        &mut self,
        ctx: &Arc<Ctx>,
        vars: &[VarId],
        cap: usize,
        base: &mut BaseScan,
    ) -> Result<Option<Buffer>> {
        let (lo, hi, exact) = self.ranges[self.at];
        let index = ctx.snap.perm(self.spec.perm);
        if base.next == 0 && base.end == 0 && base.block.is_none() {
            (base.next, base.end) = index.key_block_range(&lo, &hi);
        }
        let columns = vars
            .iter()
            .map(|v| self.spec.cols.iter().find(|(_, x)| x == v).unwrap().0)
            .collect::<Vec<_>>();
        let mut out = Buffer::new(ctx, vars, cap)?;
        let mask = exec::scan_mask(ctx, &self.spec);
        let mut step = ScanStep::default();
        while out.table.len < cap {
            ctx.check()?;
            if base.block.is_none() {
                if base.next == base.end {
                    self.at += 1;
                    base.next = 0;
                    base.end = 0;
                    base.ahead = Default::default();
                    break;
                }
                let metadata = &index.blocks[base.next];
                let whole = metadata.first >= lo && metadata.last <= hi;
                if Arc::strong_count(&base._charge) > 1 {
                    base._charge = Arc::new(OwnedCharge::new(
                        ctx,
                        crate::index::BLOCK_ROWS as u64 * 32 + 512,
                    )?);
                }
                let columns = |b: usize| {
                    let m = &index.blocks[b];
                    if m.first >= lo && m.last <= hi {
                        mask
                    } else {
                        mask | crate::index::bound_cols(&lo, &hi)
                    }
                };
                let wanted = columns(base.next);
                let block = match ctx.snap.cache.get_cols_cached(index, base.next, wanted) {
                    Some(block) => block,
                    None => {
                        let hint = base.ahead.before(
                            index,
                            &ctx.snap.cache,
                            base.next,
                            base.end,
                            &columns,
                        );
                        ctx.snap
                            .cache
                            .get_cols_hinted(index, base.next, wanted, hint)?
                    }
                };
                (base.row, base.row_end) = if whole {
                    (0, block.len())
                } else {
                    block.key_range(&lo, &hi)
                };
                base.next += 1;
                base.passes = exec::block_passes(&self.spec, &block, base.row, base.row_end);
                base.block = Some(block);
            }
            let block = base.block.as_ref().unwrap();
            if !self.spec.dedup && base.passes {
                let end = base.row_end.min(base.row + cap - out.table.len);
                for (column, &source) in out.table.cols.iter_mut().zip(&columns) {
                    column.extend(block.cols[source][base.row..end].iter().copied().map(Id));
                }
                out.table.len += end - base.row;
                base.row = end;
            } else {
                while base.row < base.row_end && out.table.len < cap {
                    let mut key = block.key(base.row);
                    key[..self.spec.prefix.len()].copy_from_slice(&self.spec.prefix);
                    base.row += 1;
                    self.visit(ctx, &columns, key, &mut out, cap, &mut step)?;
                }
            }
            if base.row == base.row_end {
                base.block = None;
            }
        }
        if !exact && !out.table.is_empty() {
            let _scratch = ctx.charge(out.table.len as u64 * 16 + ctx.nvars() as u64 * 16)?;
            exec::apply_range_filter(ctx, &mut out.table, self.var, &self.filter)?;
        }
        out.reconcile()?;
        ctx.check()?;
        Ok(Some(out))
    }

    fn next(&mut self, ctx: &Arc<Ctx>, vars: &[VarId], cap: usize) -> Result<Option<Buffer>> {
        if self.done() {
            return Ok(None);
        }
        if !self.base_checked {
            self.base_checked = true;
            // A captured empty delta never changes. Keep a row position inside
            // a selectively decoded immutable block rather than reseeking a
            // complete quad key for each output batch. Retained decoded columns
            // have a conservative charge, and this optional path may decline.
            let cache_bytes = crate::index::BLOCK_ROWS as u64 * 32 + 512;
            let output_bytes =
                cap as u64 * (vars.len() as u64 * 8 + 16) + ctx.nvars() as u64 * 16 + 4096;
            if ctx.snap.delta.is_empty()
                && ctx.memory_remaining() >= cache_bytes.saturating_add(output_bytes)
                && let Ok(charge) = OwnedCharge::new(ctx, cache_bytes)
            {
                self.base = Some(BaseScan {
                    ahead: Default::default(),
                    next: 0,
                    end: 0,
                    block: None,
                    row: 0,
                    row_end: 0,
                    passes: false,
                    _charge: Arc::new(charge),
                });
            }
        }
        if let Some(mut base) = self.base.take() {
            let result = self.next_base(ctx, vars, cap, &mut base);
            self.base = Some(base);
            return result;
        }
        let (lo, hi, exact) = self.ranges[self.at];
        let start = self.position.unwrap_or(lo);
        let mut b = Buffer::new(ctx, vars, cap)?;
        let columns: Vec<usize> = vars
            .iter()
            .map(|v| {
                self.spec
                    .cols
                    .iter()
                    .find(|(_, var)| var == v)
                    .expect("scan output variable")
                    .0
            })
            .collect();
        let mut step = ScanStep::default();
        // Resume requires the complete lexicographic key. Only the last copied
        // row needs its key reconstructed when a block passes every predicate.
        ctx.snap
            .scan_between(self.spec.perm, start, hi, |chunk| match chunk {
                Chunk::Row(key) => self.visit(ctx, &columns, key, &mut b, cap, &mut step),
                Chunk::Block(block, start, end) => {
                    if !self.spec.dedup && exec::block_passes(&self.spec, block, start, end) {
                        let end = end.min(start + cap - b.table.len);
                        if end > start {
                            for (column, &source) in b.table.cols.iter_mut().zip(&columns) {
                                column
                                    .extend(block.cols[source][start..end].iter().copied().map(Id));
                            }
                            b.table.len += end - start;
                            step.visited += end - start;
                            step.next = successor(block.key(end - 1)).filter(|key| *key <= hi);
                            ctx.check()?;
                        }
                        if b.table.len == cap {
                            step.stopped = true;
                            return Ok(false);
                        }
                        return Ok(true);
                    }
                    for row in start..end {
                        if !self.visit(ctx, &columns, block.key(row), &mut b, cap, &mut step)? {
                            return Ok(false);
                        }
                    }
                    Ok(true)
                }
            })?;
        let ScanStep { stopped, next, .. } = step;
        let next = next.filter(|key| *key <= hi);
        if stopped && next.is_some() {
            self.position = next;
        } else {
            self.at += 1;
            self.position = None;
        }
        if !exact && !b.table.is_empty() {
            let scratch = ctx.charge(b.table.len as u64 * 16 + ctx.nvars() as u64 * 16)?;
            exec::apply_range_filter(ctx, &mut b.table, self.var, &self.filter)?;
            drop(scratch);
        }
        ctx.check()?;
        b.reconcile()?;
        Ok(Some(b))
    }
}

#[derive(Default)]
struct ScanStep {
    visited: usize,
    stopped: bool,
    next: Option<Key>,
}

fn successor(mut key: Key) -> Option<Key> {
    for column in (0..4).rev() {
        if key[column] != u64::MAX {
            key[column] += 1;
            key[column + 1..].fill(0);
            return Some(key);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grouping_releases_an_exhausted_hash_join_working_set() {
        use crate::sparql::plan::{Agg, JoinAlgo};
        use crate::store::{Store, StoreOptions};
        use spargebra::algebra::AggregateFunction;

        const ROWS: usize = 65_536;
        const GROUPS: usize = 64;
        let store = Store::in_memory(StoreOptions::default());
        let mut ctx = Ctx::new(store.snapshot());
        ctx.mem_limit = 16 << 20;
        ctx.enable_cursor_accounting().unwrap();
        let ctx = Arc::new(ctx);
        ctx.attach_cursor_owner();
        let join = ctx.var("join");
        let key = ctx.var("key");
        let value = ctx.var("value");
        let count = ctx.var("count");
        let mut left = Table::new(vec![join, key]);
        let mut right = Table::new(vec![join, value]);
        for row in 0..ROWS {
            let id = Id::from_i64(row as i64).unwrap();
            left.push_row(&[id, Id::from_i64((row % GROUPS) as i64).unwrap()]);
            right.push_row(&[id, Id::from_i64((row + 7) as i64).unwrap()]);
        }
        let mut hash = Node::leaf(
            Kind::Join {
                algo: JoinAlgo::Hash,
                keys: vec![join],
            },
            vec![join, key, value],
            ROWS as f64,
            "linear unique-key fixture".into(),
        );
        hash.children = vec![
            Node::leaf(
                Kind::Values(left),
                vec![join, key],
                ROWS as f64,
                "left".into(),
            ),
            Node::leaf(
                Kind::Values(right),
                vec![join, value],
                ROWS as f64,
                "right".into(),
            ),
        ];
        let mut node = Node::leaf(
            Kind::Group {
                keys: vec![key],
                aggs: vec![(
                    count,
                    Agg {
                        func: AggregateFunction::Count,
                        expr: None,
                        distinct: false,
                        fold: None,
                        registered: None,
                    },
                )],
            },
            vec![key, count],
            GROUPS as f64,
            "64 exact groups".into(),
        );
        node.children = vec![hash];
        // Literal input storage is still owned by the plan, just as in QueryCursor.
        let _plan = OwnedCharge::new(&ctx, plan_bytes(&node)).unwrap();
        let retained_plan = ctx.live_bytes();
        let mut root = Operator::build(&ctx, node, FallbackPolicy::RejectMaterialization).unwrap();
        let mut plan = root.describe(&ctx);
        let options = CursorOptions::default();
        let batch = root
            .next(&ctx, &options, options.batch_rows)
            .unwrap()
            .unwrap();
        assert_eq!(batch.table.len, GROUPS);
        for group in 0..GROUPS {
            assert_eq!(
                batch.table.cols[0][group],
                Id::from_i64(group as i64).unwrap()
            );
            assert_eq!(
                batch.table.cols[1][group],
                Id::from_i64((ROWS / GROUPS) as i64).unwrap()
            );
        }
        assert!(
            ctx.live_bytes() <= retained_plan + (256 << 10),
            "the completed join still holds its large input/index: live={}, plan={retained_plan}",
            ctx.live_bytes(),
        );
        // Keep the output batch alive while terminal calls and metadata are checked.
        assert!(
            root.next(&ctx, &options, options.batch_rows)
                .unwrap()
                .is_none()
        );
        assert!(root.next(&ctx, &options, 1).unwrap().is_none());
        root.update_plan(&mut plan);
        let join_plan = &plan.children[0];
        assert_eq!(join_plan.operator.operator, "HashJoin");
        assert_eq!(join_plan.operator.actual_rows, ROWS as i64);
        assert!(join_plan.full_input_before_output);
        assert!(join_plan.complete);
        assert_eq!(batch.table.len, GROUPS);
    }

    #[cfg(unix)]
    fn evicted_skipped_read_ahead_column(mode: &str) {
        use crate::index::{
            BLOCK_ROWS, BlockCache, Perm, advice_page_size, read_ahead_fixture,
            read_ahead_reference,
        };
        use crate::store::{Store, StoreOptions};
        let (_dir, idx) = read_ahead_fixture(6);
        let store = Store::in_memory(StoreOptions::default());
        let mut snapshot = (*store.snapshot()).clone();
        drop(store);
        Arc::get_mut(&mut snapshot.generation)
            .expect("owned fixture generation")
            .perms[Perm::Spo.index()] = idx;
        snapshot.cache = Arc::new(BlockCache::new(64 << 20));
        let ctx = Arc::new(Ctx::new(Arc::new(snapshot)));
        let var = ctx.var("s");
        let spec = ScanSpec {
            perm: Perm::Spo,
            prefix: vec![],
            cols: vec![(1, var)],
            eqs: vec![],
            graph: super::super::plan::GraphFilter::All,
            graph_col: 3,
            dedup: false,
            src: None,
        };
        let index = ctx.snap.perm(Perm::Spo);
        ctx.snap.cache.get_cols(index, 1, 2).unwrap();
        let before = ctx.snap.cache.misses();
        let target = index.blocks[1].offset as usize + index.blocks[1].col_len[0] as usize;
        let target_end = target + index.blocks[1].col_len[1] as usize;
        assert!(target_end - target > 128 << 10);
        assert!(target_end - target > advice_page_size());
        assert!(index.blocks[0].col_len[2] > 64 << 10);
        assert!(index.blocks[1].col_len[2] > 64 << 10);
        let expected = read_ahead_reference(6 * BLOCK_ROWS);
        let trace = index.trace_hints();
        let mut scan = Scan::new(spec, None);
        let mut n = 0;
        loop {
            let rows = if mode == "shared" {
                match scan.next_shared(&ctx, &[var], BLOCK_ROWS).unwrap() {
                    SharedAttempt::Ready(Some(batch)) => {
                        for &id in &batch.block.cols[1][batch.start..batch.end] {
                            assert_eq!(id, expected[n as usize]);
                            n += 1;
                        }
                        batch.end - batch.start
                    }
                    SharedAttempt::Ready(None) => break,
                    _ => panic!("shared fixture unexpectedly declined"),
                }
            } else {
                let vars = if mode == "count" { &[][..] } else { &[var][..] };
                let Some(buffer) = scan.next(&ctx, vars, BLOCK_ROWS).unwrap() else {
                    break;
                };
                if mode == "count" {
                    n += buffer.table.len as u64;
                } else {
                    for id in &buffer.table.cols[0] {
                        assert_eq!(id.0, expected[n as usize]);
                        n += 1;
                    }
                }
                buffer.table.len
            };
            if n == BLOCK_ROWS as u64 {
                assert!(
                    !trace
                        .spans()
                        .iter()
                        .any(|&(a, z)| a < target_end && z > target),
                    "fixture accidentally hinted skipped block"
                );
                ctx.snap.cache.evict_column(index, 1, 1);
            }
            if rows == 0 {
                break;
            }
        }
        assert_eq!(n, 6 * BLOCK_ROWS as u64);
        assert_eq!(ctx.snap.cache.misses() - before, 6);
        let spans = trace.spans();
        eprintln!(
            "{mode} target=({target},{target_end}), page={}, misses={}, rows={n}, target advice={:?}",
            advice_page_size(),
            ctx.snap.cache.misses() - before,
            spans
                .iter()
                .filter(|&&(a, z)| a < target_end && z > target)
                .collect::<Vec<_>>()
        );
        assert_eq!(
            spans.iter().any(|&(a, z)| a < target_end && z > target),
            crate::index::io_hints(),
            "{mode}: an evicted column requires fallback advice when hints are enabled"
        );
        assert_eq!(
            crate::index::advice_covers_span(&spans, (target, target_end)),
            crate::index::io_hints(),
            "{mode}: complete evicted column needs advice"
        );
        if !crate::index::io_hints() {
            assert!(spans.is_empty());
        }
    }

    #[cfg(unix)]
    #[test]
    fn shared_cursor_eviction_restores_read_ahead() {
        evicted_skipped_read_ahead_column("shared");
    }
    #[cfg(unix)]
    #[test]
    fn owned_cursor_eviction_restores_read_ahead() {
        evicted_skipped_read_ahead_column("owned");
    }
    #[cfg(unix)]
    #[test]
    fn count_cursor_eviction_restores_read_ahead() {
        evicted_skipped_read_ahead_column("count");
    }

    /// The decision table of automatic execution for groupings, counts over OPTIONAL
    /// and MINUS among them: a cursor from [`AUTO_GROUP_ROWS`] estimated input rows on,
    /// whether results are cached or not, and eager below it or with an operator the
    /// cursor runs eagerly.
    #[test]
    fn automatic_execution_streams_groupings_over_large_inputs() {
        use crate::io::Source;
        use crate::store::{Store, StoreOptions};
        let store = Store::in_memory(StoreOptions::default());
        store
            .load(&[Source::from_bytes(
                b"<urn:s> <urn:p> 1; <urn:q> 2 . <urn:t> <urn:p> 3 . <urn:u> <urn:q> 4 .".to_vec(),
                oxrdfio::RdfFormat::Turtle,
                None,
            )])
            .unwrap();
        let cases = [
            (
                "SELECT (COUNT(*) AS ?n) {?s <urn:p> ?a OPTIONAL {?s <urn:q> ?b}}",
                true,
            ),
            (
                "SELECT (COUNT(*) AS ?n) {?s <urn:p> ?a MINUS {?s <urn:q> ?b}}",
                true,
            ),
            (
                "SELECT ?a (AVG(?b) AS ?m) {?s <urn:p> ?a ; <urn:q> ?b} GROUP BY ?a ORDER BY DESC(?m) LIMIT 3",
                true,
            ),
            (
                "SELECT ?a (COUNT(*) AS ?n) {?s <urn:p> ?a FILTER EXISTS {?s <urn:q> ?b}} GROUP BY ?a",
                true,
            ),
            (
                "SELECT (COUNT(*) AS ?n) {?s <urn:p> ?a FILTER NOT EXISTS {?s <urn:q> ?b . FILTER NOT EXISTS {?b <urn:p> ?s}}}",
                true,
            ),
            // an EXISTS evaluated per distinct key, in an OPTIONAL condition or BIND
            (
                "SELECT (COUNT(*) AS ?n) {?s <urn:p> ?a OPTIONAL {?s <urn:q> ?b FILTER EXISTS {?b <urn:p> ?c}}}",
                false,
            ),
            (
                "SELECT ?e (COUNT(*) AS ?n) {?s <urn:p> ?a BIND(EXISTS {?s <urn:q> ?b} AS ?e)} GROUP BY ?e",
                false,
            ),
            (
                "SELECT (COUNT(*) AS ?n) {?s <urn:p> ?a FILTER EXISTS {SERVICE <http://example.org/sparql> {?s ?p ?o}}}",
                false,
            ),
            (
                "SELECT (COUNT(*) AS ?n) {?s <urn:p> ?a FILTER EXISTS {?s <urn:q> ?b FILTER EXISTS {?b <urn:p> ?c FILTER EXISTS {?c <urn:p> ?d FILTER EXISTS {?d <urn:p> ?e FILTER EXISTS {?e <urn:p> ?f}}}}}}",
                false,
            ),
        ];
        for (q, streams) in cases {
            let q = super::super::parse_query(q, None, &[]).unwrap();
            for no_cache in [true, false] {
                for (est, large) in [
                    (1_000.0, false),
                    (AUTO_GROUP_ROWS - 1.0, false),
                    (AUTO_GROUP_ROWS, true),
                    (1_000_000.0, true),
                    (f64::INFINITY, false),
                ] {
                    let options = QueryOptions {
                        no_cache,
                        ..Default::default()
                    };
                    let mut p =
                        super::super::prepare_query(store.snapshot(), &q, &options, None, None)
                            .unwrap();
                    let mut group = &mut p.node;
                    while !matches!(group.kind, Kind::Group { .. }) {
                        group = &mut group.children[0];
                    }
                    fn scale(node: &mut Node, est: f64) {
                        node.est = est;
                        for child in &mut node.children {
                            scale(child, est);
                        }
                    }
                    scale(&mut group.children[0], est);
                    group.est = 10.0;
                    assert_eq!(
                        auto_streaming(&p.ctx, &p.node, p.kind, &Default::default()),
                        streams && large,
                        "{q}: no_cache {no_cache}, {est} rows"
                    );
                }
            }
        }
    }

    #[test]
    fn orienting_leaves_other_ask_shapes_alone() {
        use crate::store::{Store, StoreOptions};
        let store = Store::in_memory(StoreOptions::default());
        for text in ["ASK {?s ?p ?o}", "ASK {?s ?p ?o FILTER(?s = ?o)}", "ASK {}"] {
            let query = super::super::parse_query(text, None, &[]).unwrap();
            let options = QueryOptions {
                no_cache: true,
                ..Default::default()
            };
            let mut p = super::super::prepare_query(store.snapshot(), &query, &options, None, None)
                .unwrap();
            fn shape(node: &Node) -> String {
                let children: Vec<String> = node.children.iter().map(shape).collect();
                format!("{:?}/{}[{}]", node.vars, node.est, children.join(","))
            }
            let before = shape(&p.node);
            orient_filtered_ask(&mut p.node);
            assert_eq!(shape(&p.node), before, "{text}");
        }
    }

    #[test]
    fn filtered_ask_admission_keeps_small_effectful_and_mutable_plans_ordinary() {
        use crate::io::Source;
        use crate::store::{Store, StoreOptions};
        let store = Store::in_memory(StoreOptions::default());
        let mut data = String::new();
        for i in 0..64 {
            data.push_str(&format!("<urn:left:{i}> <urn:left> {i} .\n"));
            data.push_str(&format!("<urn:right:{i}> <urn:right> {i} .\n"));
        }
        store
            .load(&[Source::from_bytes(
                data.into_bytes(),
                oxrdfio::RdfFormat::Turtle,
                None,
            )])
            .unwrap();
        let text = "ASK {?a <urn:left> ?x . ?b <urn:right> ?y FILTER(?x = ?y)}";
        let prepare = |text: &str, max_rows| {
            let query = super::super::parse_query(text, None, &[]).unwrap();
            let options = QueryOptions {
                max_rows: Some(max_rows),
                no_cache: true,
                ..Default::default()
            };
            super::super::prepare_query(store.snapshot(), &query, &options, None, None).unwrap()
        };
        let p = prepare(text, 1024);
        assert!(filtered_ask_admitted(&p.ctx, &p.node));
        let small = prepare(text, 8192);
        assert!(!filtered_ask_admitted(&small.ctx, &small.node));
        for text in [
            "ASK {?a <urn:left> ?x}",
            "SELECT * {?a <urn:left> ?x . ?b <urn:right> ?y FILTER(?x = ?y)}",
            "ASK {?a <urn:left> ?x . ?b <urn:right> ?y FILTER(?x = ?y && RAND() < 1)}",
            "ASK {?a <urn:left> ?x . ?b <urn:right> ?y FILTER(?x = UUID())}",
            "ASK {?a <urn:left> ?x . ?b <urn:right> ?y FILTER(?x = ?a)}",
            "ASK {?a <urn:left> ?x OPTIONAL {?b <urn:right> ?y} FILTER(?x = ?y)}",
            "ASK {VALUES ?x {UNDEF 1} VALUES ?y {UNDEF 1} FILTER(?x = ?y)}",
        ] {
            let p = prepare(text, 1);
            assert!(!filtered_ask_admitted(&p.ctx, &p.node), "{text}");
        }
        let mut effects = prepare(text, 1024);
        Arc::get_mut(&mut effects.ctx).unwrap().calls_extensions = true;
        assert!(!filtered_ask_admitted(&effects.ctx, &effects.node));
        // A retained immutable context stays eligible; a new delta keeps the
        // conservative ordinary policy until it has an immutable generation.
        super::super::update::update(
            &store,
            "INSERT DATA {<urn:new> <urn:left> 0}",
            &Default::default(),
        )
        .unwrap();
        let changed = prepare(text, 1024);
        assert!(!filtered_ask_admitted(&changed.ctx, &changed.node));
        assert!(filtered_ask_admitted(&p.ctx, &p.node));
    }

    #[test]
    fn automatic_admission_requires_the_measured_shared_scan_conditions() {
        use crate::io::Source;
        use crate::store::{Store, StoreOptions};
        let store = Store::in_memory(StoreOptions::default());
        store
            .load(&[Source::from_bytes(
                b"<urn:s> <urn:p> 1 .".to_vec(),
                oxrdfio::RdfFormat::Turtle,
                None,
            )])
            .unwrap();
        let q = super::super::parse_query("SELECT ?s ?o {?s <urn:p> ?o}", None, &[]).unwrap();
        let prepare = |snapshot, options: &QueryOptions| {
            let mut prepared =
                super::super::prepare_query(snapshot, &q, options, None, None).unwrap();
            // Exercise policy thresholds without allocating a million-row fixture.
            prepared.node.est = 1_000_000.0;
            prepared
        };
        let p = prepare(store.snapshot(), &Default::default());
        let options = CursorOptions::default();
        assert!(auto_streaming(&p.ctx, &p.node, p.kind, &options));
        // a scan streams from the measured size on
        let mut small = prepare(store.snapshot(), &Default::default());
        small.node.est = AUTO_SCAN_ROWS;
        assert!(auto_streaming(
            &small.ctx,
            &small.node,
            small.kind,
            &options
        ));
        small.node.est = AUTO_SCAN_ROWS - 1.0;
        assert!(!auto_streaming(
            &small.ctx,
            &small.node,
            small.kind,
            &options
        ));
        small.node.est = f64::INFINITY;
        assert!(!auto_streaming(
            &small.ctx,
            &small.node,
            small.kind,
            &options
        ));
        assert!(!auto_streaming(
            &p.ctx,
            &p.node,
            p.kind,
            &CursorOptions {
                batch_rows: 1,
                ..options.clone()
            }
        ));
        assert!(!auto_streaming(
            &p.ctx,
            &p.node,
            p.kind,
            &CursorOptions {
                batch_bytes: 8,
                ..options.clone()
            }
        ));
        assert!(!auto_streaming(
            &p.ctx,
            &p.node,
            p.kind,
            &CursorOptions {
                batch_rows: usize::MAX,
                batch_bytes: usize::MAX,
                ..options
            }
        ));
        let low = prepare(
            store.snapshot(),
            &QueryOptions {
                max_memory_bytes: Some(1_000_000),
                ..Default::default()
            },
        );
        assert!(!auto_streaming(
            &low.ctx,
            &low.node,
            low.kind,
            &Default::default()
        ));
        // A scan expected to pass a row budget stays eager, so that the budget fails it
        // before any output.
        for budget in [
            QueryOptions {
                max_rows: Some(999_999),
                ..Default::default()
            },
            QueryOptions {
                max_rows_produced: Some(999_999),
                ..Default::default()
            },
        ] {
            let capped = prepare(store.snapshot(), &budget);
            assert!(!auto_streaming(
                &capped.ctx,
                &capped.node,
                capped.kind,
                &Default::default()
            ));
        }
        let roomy = prepare(
            store.snapshot(),
            &QueryOptions {
                max_rows: Some(1_000_000),
                max_rows_produced: Some(1_000_000),
                ..Default::default()
            },
        );
        assert!(auto_streaming(
            &roomy.ctx,
            &roomy.node,
            roomy.kind,
            &Default::default()
        ));
        super::super::update::update(
            &store,
            "INSERT DATA {<urn:new> <urn:p> 2}",
            &Default::default(),
        )
        .unwrap();
        let changed = prepare(store.snapshot(), &Default::default());
        assert!(!auto_streaming(
            &changed.ctx,
            &changed.node,
            changed.kind,
            &Default::default()
        ));
        // A retained immutable context stays admissible after a concurrent write.
        assert!(auto_streaming(&p.ctx, &p.node, p.kind, &Default::default()));
    }

    /// The decision table of spill admission (X08 §6.8): a query streams when it may
    /// spill, reads an immutable snapshot, has only cursor operators and is expected
    /// to hold more than half its memory budget eagerly.
    #[test]
    fn automatic_admission_streams_a_product_whose_cursor_state_fits_without_spill() {
        use super::super::plan::JoinAlgo;
        use crate::io::Source;
        use crate::store::{Store, StoreOptions};
        let store = Store::in_memory(StoreOptions::default());
        store
            .load(&[Source::from_bytes(
                b"<urn:s> <urn:p> 1 . <urn:s> <urn:q> <urn:f> .".to_vec(),
                oxrdfio::RdfFormat::Turtle,
                None,
            )])
            .unwrap();
        // BSBM BI q4's shape: two groupings joined, one over a NOT EXISTS above a
        // Cartesian product of offers and features
        let q4 = "SELECT * {
            { SELECT ?f (COUNT(?o) AS ?n) {
                ?s <urn:p> ?o . { SELECT DISTINCT ?f { ?x <urn:q> ?f } }
                FILTER NOT EXISTS { ?s <urn:q> ?f } } GROUP BY ?f }
            { SELECT ?f (COUNT(?y) AS ?m) { ?y <urn:q> ?f } GROUP BY ?f } }";
        let sort = "SELECT ?s ?o { ?s <urn:p> ?o } ORDER BY ?o";
        // every scan and grouping holds 1,000 rows, and a product the rows of both
        // inputs, so only the product is large
        fn estimate(node: &mut Node) -> f64 {
            let kids: Vec<f64> = node.children.iter_mut().map(estimate).collect();
            node.est = match &node.kind {
                Kind::Join {
                    algo: JoinAlgo::Cross,
                    ..
                } => kids.iter().product(),
                Kind::Group { .. } => 1_000.0,
                _ if kids.is_empty() => 1_000.0,
                _ => kids.iter().copied().fold(0.0, f64::max),
            };
            node.est
        }
        let decide = |q: &str, memory: u64| {
            let q = super::super::parse_query(q, None, &[]).unwrap();
            let options = QueryOptions {
                max_memory_bytes: Some(memory),
                ..Default::default()
            };
            let mut p =
                super::super::prepare_query(store.snapshot(), &q, &options, None, None).unwrap();
            estimate(&mut p.node);
            auto_streaming(&p.ctx, &p.node, p.kind, &Default::default())
        };
        // a product of a million rows is about 24 MB eagerly, more than half of 32 MiB,
        // while the cursor keeps a thousand features, the key set and the groups
        assert!(decide(q4, 32 << 20));
        // when eager execution fits, it stays eager
        assert!(!decide(q4, 1 << 30));
        // a full sort keeps all of its rows in the cursor too, so without spill it stays
        // eager
        assert!(!decide(sort, 32 << 20));
        // the same product under aggregates that keep more than a row per group: a
        // DISTINCT aggregate keeps its distinct input rows and GROUP_CONCAT its text, so
        // neither is admitted on the memory estimate
        let memory = |q: &str| {
            let q = super::super::parse_query(q, None, &[]).unwrap();
            let options = QueryOptions {
                max_memory_bytes: Some(32 << 20),
                ..Default::default()
            };
            let mut p =
                super::super::prepare_query(store.snapshot(), &q, &options, None, None).unwrap();
            estimate(&mut p.node);
            memory_admitted(&p.ctx, &p.node, p.kind, &Default::default())
        };
        let counted = "SELECT ?f (COUNT(?o) AS ?n) { ?s <urn:p> ?o . ?x <urn:q> ?f } GROUP BY ?f";
        assert!(memory(counted));
        assert!(!memory(
            "SELECT ?f (COUNT(DISTINCT ?o) AS ?n) { ?s <urn:p> ?o . ?x <urn:q> ?f } GROUP BY ?f"
        ));
        assert!(!memory(
            "SELECT ?f (GROUP_CONCAT(?o) AS ?n) { ?s <urn:p> ?o . ?x <urn:q> ?f } GROUP BY ?f"
        ));
        // a BIND that generates a term per row of the product keeps a term per row
        assert!(!memory(
            "SELECT ?u { ?s <urn:p> ?o . ?x <urn:q> ?f BIND(STRUUID() AS ?u) }"
        ));
    }

    #[test]
    fn automatic_admission_streams_plans_expected_to_outgrow_memory_when_they_can_spill() {
        use crate::io::Source;
        use crate::store::{Store, StoreOptions};
        let store = Store::in_memory(StoreOptions::default());
        store
            .load(&[Source::from_bytes(
                b"<urn:s> <urn:p> 1 . <urn:s> <urn:q> 2 .".to_vec(),
                oxrdfio::RdfFormat::Turtle,
                None,
            )])
            .unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let spill = crate::spill::Spill::open(&crate::spill::SpillOptions {
            dir: tmp.path().to_path_buf(),
            max_bytes: 1 << 30,
            query_bytes: 1 << 30,
            min_free_bytes: 0,
            encrypted: false,
        })
        .unwrap();
        let decide = |q: &str, spill: Option<Arc<crate::spill::Spill>>, memory: u64, est: f64| {
            let q = super::super::parse_query(q, None, &[]).unwrap();
            let options = QueryOptions {
                spill,
                max_memory_bytes: Some(memory),
                ..Default::default()
            };
            let mut p =
                super::super::prepare_query(store.snapshot(), &q, &options, None, None).unwrap();
            fn scale(node: &mut Node, est: f64) {
                node.est = est;
                for child in &mut node.children {
                    scale(child, est);
                }
            }
            scale(&mut p.node, est);
            auto_streaming(&p.ctx, &p.node, p.kind, &Default::default())
        };
        let sort = "SELECT ?s ?o { ?s <urn:p> ?o } ORDER BY ?o";
        let group = "SELECT ?o (COUNT(?s) AS ?n) { ?s <urn:p> ?o } GROUP BY ?o";
        let join = "SELECT * { ?s <urn:p> ?o OPTIONAL { ?s <urn:q> ?x } } ORDER BY ?x";
        let exists = "SELECT ?s { ?s <urn:p> ?o FILTER EXISTS { ?s <urn:q> ?x } } ORDER BY ?s";
        let service = "SELECT * { SERVICE <http://example.org/sparql> { ?s ?p ?o } } ORDER BY ?s";
        let some = Some(spill.clone());
        // 1M rows of two columns is 16 MB, more than half of 16 MiB.
        // A large grouping streams by its own rule, with or without spill.
        assert!(decide(group, None, u64::MAX, 1e6));
        for q in [sort, join] {
            assert!(decide(q, some.clone(), 16 << 20, 1e6), "{q}");
            // without spill, or under half the budget, the plan stays eager
            assert!(!decide(q, None, 16 << 20, 1e6), "{q}");
            assert!(!decide(q, some.clone(), 64 << 20, 1e6), "{q}");
            // an unlimited budget never admits by memory
            assert!(!decide(q, some.clone(), u64::MAX, 1e6), "{q}");
        }
        // EXISTS whose patterns have cursor plans runs in the cursor
        assert!(decide(exists, some.clone(), 16 << 20, 1e6));
        assert!(!decide(exists, None, 16 << 20, 1e6));
        // plans with an operator the cursor runs eagerly stay eager
        let remote = "SELECT ?s { ?s <urn:p> ?o FILTER EXISTS { SERVICE <http://example.org/sparql> { ?s ?p ?x } } } ORDER BY ?s";
        assert!(!decide(remote, some.clone(), 16 << 20, 1e6));
        assert!(!decide(service, some.clone(), 16 << 20, 1e6));
        // an unknown estimate stays eager
        assert!(!decide(sort, some.clone(), 16 << 20, f64::INFINITY));
        // uncompacted changes keep the plan eager, and the eager run retries instead
        super::super::update::update(
            &store,
            "INSERT DATA {<urn:new> <urn:p> 2}",
            &Default::default(),
        )
        .unwrap();
        assert!(!decide(sort, some, 16 << 20, 1e6));
    }

    /// A cursor over a few dozen distinct IRIs and literals, and its first batch of
    /// eight rows, for the tests of the decoded batch's reservation.
    fn first_batch(memory: u64) -> QueryBatch {
        use crate::io::Source;
        use crate::store::{Store, StoreOptions};
        let store = Store::in_memory(StoreOptions::default());
        let data = (0..48)
            .map(|i| format!("<urn:s:{i}> <urn:p> \"object number {i}\" .\n"))
            .collect::<String>();
        store
            .load(&[Source::from_bytes(
                data.into_bytes(),
                oxrdfio::RdfFormat::Turtle,
                None,
            )])
            .unwrap();
        let options = QueryOptions {
            max_memory_bytes: Some(memory),
            ..Default::default()
        };
        let cursor = CursorOptions {
            batch_rows: 8,
            ..Default::default()
        };
        let mut cursor = select_cursor(
            store.snapshot(),
            "SELECT ?s ?o { ?s <urn:p> ?o }",
            &options,
            &cursor,
        )
        .unwrap();
        let batch = cursor.next_batch().unwrap().unwrap();
        assert_eq!(batch.len(), 8);
        batch
    }

    #[test]
    fn decoded_batch_reserves_exactly_when_under_one_chunk_remains() {
        let batch = first_batch(4 << 20);
        let ctx = batch.ctx.clone();
        let filler = ctx.charge(ctx.memory_remaining() - (32 << 10)).unwrap();
        assert!(ctx.memory_remaining() < TERM_RESERVE);
        let decoded = batch.decoded().unwrap();
        assert!(
            decoded.is_some(),
            "a batch whose terms fit in the budget decodes, whatever the chunk size"
        );
        assert!(ctx.memory_remaining() < 32 << 10);
        drop(decoded);
        drop(filler);
    }

    #[test]
    fn decoded_batch_releases_its_reservation() {
        let batch = first_batch(4 << 20);
        let ctx = batch.ctx.clone();
        let before = ctx.memory_remaining();
        let decoded = batch.decoded().unwrap().unwrap();
        assert!(ctx.memory_remaining() < before);
        drop(decoded);
        assert_eq!(ctx.memory_remaining(), before);
        // A budget that holds the pairs but not the terms declines the cache, and the
        // failed attempt gives back everything it reserved.
        let filler = ctx.charge(before - (4 << 10)).unwrap();
        let tight = ctx.memory_remaining();
        assert!(batch.decoded().unwrap().is_none());
        assert_eq!(ctx.memory_remaining(), tight);
        drop(filler);
        assert_eq!(ctx.memory_remaining(), before);
    }

    #[test]
    fn successor_carries_and_stops_at_maximum() {
        assert_eq!(successor([7, 9, u64::MAX, u64::MAX]), Some([7, 10, 0, 0]));
        assert_eq!(successor([u64::MAX; 4]), None);
    }

    /// A writer that decodes ahead pulls each batch after the first before it writes the
    /// one before, and it writes the first batch before it pulls the second, so that the
    /// first rows wait for one batch only. Its batches and term caches go back to the
    /// ledger as they do without decoding ahead.
    #[test]
    fn decoding_ahead_writes_the_first_batch_before_pulling_the_second() {
        use crate::io::Source;
        use crate::store::{Store, StoreOptions};
        let store = Store::in_memory(StoreOptions::default());
        let data = (0..10_000)
            .map(|i| format!("<urn:s:{i}> <urn:p> \"v{i}\" .\n"))
            .collect::<String>();
        store
            .load(&[Source::from_bytes(
                data.into_bytes(),
                oxrdfio::RdfFormat::Turtle,
                None,
            )])
            .unwrap();
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(2)
            .build()
            .unwrap();
        let run = |ahead: bool, send: Option<usize>| {
            let mut opt = crate::sparql::Optimizations::default();
            if !ahead {
                opt = opt.disable("decode_ahead").unwrap();
            }
            let options = QueryOptions {
                optimizations: Some(opt),
                ..Default::default()
            };
            let mut cursor = select_cursor(
                store.snapshot(),
                "SELECT ?s ?o {?s <urn:p> ?o}",
                &options,
                &CursorOptions {
                    batch_rows: 1000,
                    ..Default::default()
                },
            )
            .unwrap();
            let mut produced = Vec::new();
            let mut rows = 0;
            let left = pool
                .install(|| {
                    cursor.for_each_decoded(send, |cursor, batch, decoded| {
                        assert!(decoded.is_some(), "the terms of every batch are decoded");
                        produced.push(cursor.ctx.rows_produced());
                        rows += batch.len();
                        Ok(())
                    })
                })
                .unwrap();
            (produced, rows, left, cursor.ctx.live_bytes())
        };
        let (serial, rows, left, serial_live) = run(false, None);
        assert_eq!((rows, left), (10_000, usize::MAX - 10_000));
        // the operators' rows of one batch
        let unit = serial[0];
        assert!(unit >= 1000);
        assert_eq!(serial, (1..=10).map(|i| i * unit).collect::<Vec<u64>>());
        let (ahead, rows, left, ahead_live) = run(true, None);
        assert_eq!((rows, left), (10_000, usize::MAX - 10_000));
        // the first batch alone, then each batch with the next one pulled
        let mut want = vec![unit, 3 * unit];
        want.extend((4..=10).map(|i| i * unit));
        want.push(10 * unit);
        assert_eq!(ahead, want);
        assert_eq!(
            ahead_live, serial_live,
            "decoding ahead holds nothing afterwards"
        );
        // `send` stops the pulls at its rows
        for send in [0, 1, 999, 1000, 1001, 2500, 10_000] {
            let (serial, rows, left, _) = run(false, Some(send));
            let (ahead, ahead_rows, ahead_left, _) = run(true, Some(send));
            assert_eq!((rows, left), (send, 0), "send {send}");
            assert_eq!((ahead_rows, ahead_left), (send, 0), "send {send}");
            assert_eq!(ahead.first(), serial.first(), "send {send}");
            assert_eq!(ahead.last(), serial.last(), "send {send}");
        }
    }
}
