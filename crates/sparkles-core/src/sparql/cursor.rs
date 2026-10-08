//! Fallible pull execution over a captured snapshot. Unsupported subtrees are
//! explicit, lazy materialization barriers; the eager query API is unchanged.

use super::ctx::OwnedCharge;
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
use std::time::Instant;

mod binary;
mod bind;
mod distinct;
mod filter;
pub mod graph;
mod group;
pub use graph::{GraphBatch, GraphCursor, graph_cursor};
mod merge;

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
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
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

impl CursorPlan {
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

    pub fn row(&self, row: usize) -> Result<Vec<Option<Term>>> {
        if row >= self.len() {
            return Err(Error::invalid("query batch row is out of bounds"));
        }
        (0..self.width()).map(|c| self.term(row, c)).collect()
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
        match DecodedBatch::new(self) {
            Ok(decoded) => Ok(Some(decoded)),
            Err(Error::BudgetExceeded(b)) if b.kind == crate::error::BudgetKind::Memory => Ok(None),
            Err(error) => Err(error),
        }
    }
}

pub(super) struct DecodedBatch<'a> {
    batch: &'a QueryBatch,
    terms: Vec<Term>,
    slots: Vec<u32>,
    _charge: super::ctx::Charge<'a>,
}

impl<'a> DecodedBatch<'a> {
    fn new(batch: &'a QueryBatch) -> Result<Self> {
        let ctx = &batch.ctx;
        ctx.check()?;
        let cells = batch
            .len()
            .checked_mul(batch.width())
            .filter(|&n| n < u32::MAX as usize)
            .ok_or_else(|| Error::invalid("query batch has too many cells"))?;
        // Pairs, slots, unique IDs, term capacity and row adapters coexist during
        // construction. Reserve before allocating, including reconstruction.
        let charge = ctx.charge((cells as u64).saturating_mul(128).saturating_add(1024))?;
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
        pairs.sort_unstable();
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
                charge.add((key.len() as u64).saturating_mul(8).saturating_add(128))?;
                terms.push(crate::id::key_to_term(key));
                Ok::<(), Error>(())
            },
        )?;
        Ok(Self {
            batch,
            terms,
            slots,
            _charge: charge,
        })
    }

    fn cached(&self, row: usize, column: usize) -> Option<&Term> {
        self.terms
            .get(self.slots[column * self.batch.len() + row] as usize)
    }

    pub(super) fn term(&self, row: usize, column: usize) -> Result<Option<Cow<'_, Term>>> {
        match self.cached(row, column) {
            Some(term) => Ok(Some(Cow::Borrowed(term))),
            None => Ok(self.batch.term(row, column)?.map(Cow::Owned)),
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
    let root = Operator::build(&ctx, prepared.node, cursor.fallback)?;
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
    cursor.close();
    let stats = cursor.stats();
    let plan = depth::with_stack(cursor.depth, || cursor.plan_info());
    Ok(QueryResult {
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
    })
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
}
impl Drop for MaterializedResult {
    fn drop(&mut self) {
        depth::with_stack(self.depth, || drop(self.result.take()));
    }
}

/// The completed boolean query result.
pub type AskResult = MaterializedResult;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ExecutionMode {
    #[default]
    Eager,
    Streaming,
    /// Use pull execution for measured large immutable scans and uncached OPTIONAL counts.
    Auto,
}

/// Production-allocator admission for immutable scans and uncached large
/// OPTIONAL counts. Other plans retain eager execution.
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
    fn count(ctx: &Ctx, node: &Node) -> bool {
        match &node.kind {
            Kind::Project(_)
            | Kind::Slice { offset: 0, .. }
            | Kind::Extend(_, super::expr::Expr::Var(_))
                if node.children.len() == 1 =>
            {
                count(ctx, &node.children[0])
            }
            Kind::Group { keys, aggs } if keys.is_empty() && group::eligible(node) => {
                let child = &node.children[0];
                let rows = child.est + child.children.iter().map(|child| child.est).sum::<f64>();
                aggs.iter().all(|(_, agg)| {
                    agg.expr.is_none()
                        && !agg.distinct
                        && matches!(agg.func, spargebra::algebra::AggregateFunction::Count)
                }) && matches!(child.kind, Kind::LeftJoin { expr: None })
                    && merge::eligible(child)
                    && child.children[0]
                        .vars
                        .iter()
                        .filter(|v| child.children[1].vars.contains(v))
                        .count()
                        == 1
                    && child
                        .children
                        .iter()
                        .all(|child| matches!(child.kind, Kind::Scan(_)) && scan(ctx, child))
                    && rows.is_finite()
                    && rows >= 1_000_000.0
            }
            _ => false,
        }
    }
    let scans = if node.est.is_finite() && node.est >= 1_000_000.0 && scan(ctx, node) {
        1
    } else if (!ctx.use_cache || !ctx.snap.results.enabled()) && count(ctx, node) {
        2
    } else {
        return false;
    };
    let cap = options
        .batch_rows
        .min(options.batch_bytes / node.vars.len().max(1).saturating_mul(8));
    fn width(node: &Node) -> usize {
        node.children
            .iter()
            .map(width)
            .fold(node.vars.len(), usize::max)
    }
    // Aggregate output can be narrower than either scan or the join layout.
    // Admission must leave room for their input batches as well as the result.
    let row_bytes = (width(node) as u64).saturating_mul(8).saturating_add(16);
    let headroom = (crate::index::BLOCK_ROWS as u64 * 32 + 512)
        .saturating_mul(scans)
        .saturating_add((cap as u64).saturating_mul(row_bytes).saturating_mul(scans))
        .saturating_add((ctx.nvars() as u64).saturating_mul(16))
        .saturating_add(4096);
    kind == QueryKind::Select
        && ctx.snap.delta.is_empty()
        && options.batch_rows >= 4096
        && options.batch_bytes >= 4096 * 32
        && headroom < u64::MAX
        && ctx.memory_remaining() >= headroom
}

/// Select execution without parsing or planning twice in automatic mode.
pub fn query_execution(
    snapshot: Arc<Snapshot>,
    query: &str,
    options: &QueryOptions,
    cursor: &CursorOptions,
    mode: ExecutionMode,
) -> Result<QueryExecution> {
    if mode == ExecutionMode::Streaming
        || (mode == ExecutionMode::Auto && cursor.fallback == FallbackPolicy::RejectMaterialization)
    {
        return query_cursor(snapshot, query, options, cursor);
    }
    let started = Instant::now();
    let parser_depth = spargebra::nesting::measure(query)
        .depth
        .min(depth::MAX_ALGEBRA_DEPTH);
    depth::with_stack(parser_depth, || {
        let parsed = super::parse_query(query, options.base_iri.as_deref(), &options.prefixes)?;
        let parse_ms = started.elapsed().as_secs_f64() * 1000.0;
        let levels = depth::check_query(&parsed)?;
        depth::with_stack(levels, || {
            if mode == ExecutionMode::Eager {
                let mut result = super::execute_query(snapshot, &parsed, options, parse_ms)?;
                if result.kind == QueryKind::Select {
                    super::select_star_order(query, &mut result);
                }
                return Ok(QueryExecution::Eager(Box::new(MaterializedResult {
                    result: Some(result),
                    depth: levels,
                })));
            }
            if cursor.batch_rows == 0 || cursor.batch_bytes == 0 {
                return Err(Error::invalid(
                    "cursor batch rows and bytes must be positive",
                ));
            }
            let planning = Instant::now();
            let prepared =
                super::prepare_query(snapshot, &parsed, options, Some(started), Some(cursor))?;
            let plan_ms = planning.elapsed().as_secs_f64() * 1000.0;
            let kind = prepared.kind;
            if prepared.ctx.is_cursor() {
                let (cursor, _, _) = from_prepared(
                    prepared, &parsed, query, options, cursor, started, parse_ms, plan_ms, levels,
                    kind,
                )?;
                Ok(QueryExecution::Select(Box::new(cursor)))
            } else {
                let mut result =
                    super::execute_prepared(&parsed, options, parse_ms, plan_ms, prepared)?;
                if result.kind == QueryKind::Select {
                    super::select_star_order(query, &mut result);
                }
                Ok(QueryExecution::Eager(Box::new(MaterializedResult {
                    result: Some(result),
                    depth: levels,
                })))
            }
        })
    })
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
                timing: r.result().timing.clone(),
                error: None,
            },
        }
    }
    pub fn cancellation_token(&self) -> Arc<std::sync::atomic::AtomicBool> {
        match self {
            Self::Select(c) => c.cancellation_token(),
            Self::Graph(c) => c.cancellation_token(),
            Self::Eager(r) | Self::Ask(r) => r.result().ctx.cancel.clone(),
        }
    }
    pub fn deadline(&self) -> Option<Instant> {
        match self {
            Self::Select(c) => c.deadline(),
            Self::Graph(c) => c.deadline(),
            Self::Eager(r) | Self::Ask(r) => r.result().ctx.deadline,
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
    pub fn deadline(&self) -> Option<Instant> {
        self.ctx.deadline
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
            timing,
            error: self.failure.clone(),
        }
    }

    pub fn next_batch(&mut self) -> Result<Option<QueryBatch>> {
        self.next_batch_at_most(self.options.batch_rows)
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
    Merge(Box<merge::Merge>),
    Distinct(distinct::Distinct),
    Binary(Box<binary::Binary>),
    Group(Box<group::Group>),
    Filter(Box<filter::Filter>),
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
    Fallback {
        node: Node,
        loaded: Option<Buffer>,
        at: usize,
    },
    Blocking {
        node: Node,
        loaded: Option<Buffer>,
        at: usize,
    },
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
}

fn supported(kind: &Kind) -> bool {
    match kind {
        Kind::Scan(_)
        | Kind::Values(_)
        | Kind::Empty
        | Kind::Project(_)
        | Kind::Distinct
        | Kind::CountScan { .. }
        | Kind::CountDistinctScan { .. }
        | Kind::Slice { .. }
        | Kind::Union => true,
        Kind::RangeScan(_, r) => !r.filter.iter().any(super::expr::Expr::has_exists),
        Kind::Filter(e) => !e.iter().any(super::expr::Expr::has_exists),
        Kind::Extend(_, e) => !e.has_exists(),
        Kind::CountFilterScan { filter, .. } => !filter.iter().any(super::expr::Expr::has_exists),
        _ => false,
    }
}

impl Operator {
    fn build(ctx: &Arc<Ctx>, mut node: Node, fallback: FallbackPolicy) -> Result<Self> {
        ctx.check()?;
        let incremental_merge = merge::eligible(&node);
        let incremental_binary = !incremental_merge && binary::eligible(&node);
        let incremental_group = group::eligible(&node);
        let native_blocking =
            matches!(node.kind, Kind::Sort(_) | Kind::OrderBy { .. }) && node.children.len() == 1;
        // A native sort consumes cursor batches into charged state. It blocks before
        // its first output but is not an eager fallback, so strict policy admits it
        // unless an ORDER key evaluates EXISTS, which runs whole subqueries.
        let exists_keys = matches!(&node.kind, Kind::OrderBy { keys, .. }
            if keys.iter().any(|(key, _)| key.has_exists()));
        let materializes = !supported(&node.kind)
            && !incremental_merge
            && !incremental_binary
            && !incremental_group
            && (!native_blocking || exists_keys);
        if materializes && fallback == FallbackPolicy::RejectMaterialization {
            return Err(Error::Unsupported(format!(
                "cursor requires materialization at {}; use AllowMaterialization or eager execution",
                node.operator()
            )));
        }
        let mut info = exec::describe(ctx, &node);
        if ctx.graphs.is_some() {
            info.redact();
        }
        info.children.clear();
        info.actual_rows = 0;
        let vars = node.vars.clone();
        let growing = matches!(
            node.kind,
            Kind::Extend(..) | Kind::Filter(_) | Kind::RangeScan(..) | Kind::Distinct
        ) || materializes
            || native_blocking
            || incremental_merge
            || incremental_binary
            || incremental_group;
        let merge = incremental_merge
            .then(|| merge::Merge::new(ctx, &node))
            .transpose()?;
        let group = incremental_group
            .then(|| group::Group::new(ctx, &node))
            .transpose()?;
        let binary = incremental_binary
            .then(|| binary::Binary::new(ctx, &node))
            .transpose()?;
        let children = if materializes && !native_blocking {
            Vec::new()
        } else {
            std::mem::take(&mut node.children)
                .into_iter()
                .map(|n| Self::build(ctx, n, fallback))
                .collect::<Result<Vec<_>>>()?
        };
        let reason = (materializes || native_blocking).then(|| {
            if native_blocking {
                format!(
                    "{} consumes all input under the memory budget before output",
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
        } else if let Some(group) = group {
            State::Group(Box::new(group))
        } else if filter::eligible(&node, &children) {
            State::Filter(Box::new(filter::Filter::new(node)))
        } else if native_blocking {
            State::Blocking {
                node,
                loaded: None,
                at: 0,
            }
        } else if materializes {
            State::Fallback {
                node,
                loaded: None,
                at: 0,
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
                | Kind::CountFilterScan { .. } => State::Scalar(node),
                Kind::Distinct => State::Distinct(distinct::Distinct::new(ctx)?),
                Kind::Slice { offset, limit } => State::Slice {
                    offset: *offset,
                    remaining: *limit,
                },
                Kind::Union => State::Union { at: 0 },
                _ => State::Unary(node),
            }
        };
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
        })
    }

    fn describe(&self, ctx: &Ctx) -> CursorPlan {
        let children = match &self.state {
            State::Fallback { node, .. } => node
                .children
                .iter()
                .map(|n| {
                    fn fallback(ctx: &Ctx, node: &Node) -> CursorPlan {
                        let mut operator = exec::describe(ctx, node);
                        if ctx.graphs.is_some() {
                            operator.redact();
                        }
                        operator.children.clear();
                        CursorPlan {
                            operator,
                            materializes: true,
                            full_input_before_output: true,
                            growing_state: true,
                            complete: false,
                            reason: Some("inside an eager fallback subtree".into()),
                            children: node.children.iter().map(|n| fallback(ctx, n)).collect(),
                        }
                    }
                    fallback(ctx, n)
                })
                .collect(),
            _ => self.children.iter().map(|c| c.describe(ctx)).collect(),
        };
        CursorPlan {
            operator: self.info.clone(),
            materializes: self.materializes,
            full_input_before_output: self.materializes
                || matches!(
                    self.state,
                    State::Binary(_) | State::Group(_) | State::Scalar(_) | State::Blocking { .. }
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
        if self.materializes {
            fn update(info: &PlanInfo, plan: &mut CursorPlan) {
                plan.operator.actual_rows = info.actual_rows;
                plan.operator.time_ms = info.time_ms;
                plan.complete = info.actual_rows >= 0;
                for (info, plan) in info.children.iter().zip(&mut plan.children) {
                    update(info, plan);
                }
            }
            if let State::Fallback {
                loaded: Some(_), ..
            } = &self.state
            {
                update(&self.info, plan);
            }
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
        let result = self.next_inner(ctx, options, cap);
        self.info.time_ms += began.elapsed().as_secs_f64() * 1000.0;
        let batch = result?;
        if let Some(b) = &batch {
            self.rows = self
                .rows
                .checked_add(b.table.len)
                .ok_or_else(|| Error::invalid("cursor operator row count overflow"))?;
            ctx.check_rows(self.rows)?;
            // Eager fallback already counted all its operator work during execution.
            if !matches!(self.state, State::Fallback { .. } | State::Scalar(_)) {
                ctx.produced(b.table.len)?;
            }
            if !matches!(self.state, State::Fallback { .. }) {
                self.info.actual_rows = self.rows.min(i64::MAX as usize) as i64;
            }
        } else {
            self.done = true;
        }
        ctx.check()?;
        Ok(batch)
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
                    let (table, _) = exec::execute(ctx, node)?;
                    let charge = OwnedCharge::new(ctx, capacity_bytes(&table))?;
                    debug_assert!(table.len <= 1);
                    self.done = true;
                    Some(Buffer { table, charge })
                }
                State::Merge(merge) => {
                    merge.next(ctx, &mut self.children, options, &self.vars, cap)?
                }
                State::Binary(binary) => {
                    binary.next(ctx, &mut self.children, options, &self.vars, cap)?
                }
                State::Group(group) => {
                    group.next(ctx, &mut self.children[0], options, &self.vars, cap)?
                }
                State::Filter(filter) => {
                    let batch = filter.next(ctx, &mut self.children[0], options, cap)?;
                    self.done = filter.done(self.children[0].done);
                    batch
                }
                State::Distinct(distinct) => {
                    let Some(mut buffer) = self.children[0].next(ctx, options, cap)? else {
                        return Ok(None);
                    };
                    distinct.apply(ctx, &mut buffer)?;
                    self.done = self.children[0].done;
                    Some(buffer)
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
                State::Blocking { node, loaded, at } => {
                    if loaded.is_none() {
                        let mut input = Buffer::new(ctx, &self.children[0].vars, 0)?;
                        // Blocking input demand is independent of the requested output prefix.
                        while let Some(batch) =
                            self.children[0].next(ctx, options, options.batch_rows)?
                        {
                            input.table.sorted = batch.table.sorted.clone();
                            merge::append(ctx, &mut input, &batch.table, 0..batch.table.len)?;
                        }
                        // Row reordering/output copies and their index arrays can
                        // coexist with the input columns. Reserve before sorting.
                        input.charge.resize(
                            capacity_bytes(&input.table)
                                .saturating_mul(3)
                                .saturating_add(input.table.len as u64 * 24 + 1024),
                        )?;
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
                State::Fallback { node, loaded, at } => {
                    if loaded.is_none() {
                        let (table, mut info) = exec::execute(ctx, node)?;
                        if ctx.graphs.is_some() {
                            info.redact();
                        }
                        info.time_ms = self.info.time_ms;
                        self.info = info;
                        let charge = OwnedCharge::new(ctx, capacity_bytes(&table))?;
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
                            // Each arm may be sorted, but their concatenation is not.
                            // A consumer such as a blocking sort reads a batch's order
                            // as the order of the whole stream.
                            if self.children.len() > 1 {
                                b.table.sorted.clear();
                            }
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
    b.table.sorted = table.sorted.clone();
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
    base_checked: bool,
    base: Option<BaseScan>,
}

struct BaseScan {
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
                let block = ctx.snap.cache.get_cols(
                    index,
                    base.next,
                    if whole {
                        mask
                    } else {
                        mask | crate::index::bound_cols(&lo, &hi)
                    },
                )?;
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
                let block = ctx.snap.cache.get_cols(
                    index,
                    base.next,
                    if whole {
                        mask
                    } else {
                        mask | crate::index::bound_cols(&lo, &hi)
                    },
                )?;
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
            exec::apply_filter(ctx, &mut out.table, &self.filter)?;
        }
        out.table.sorted = self.spec.cols.iter().map(|&(_, v)| v).collect();
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
            exec::apply_filter(ctx, &mut b.table, &self.filter)?;
            drop(scratch);
        }
        b.table.sorted = self
            .spec
            .cols
            .iter()
            .map(|&(_, variable)| variable)
            .collect();
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
    fn automatic_counts_preserve_enabled_result_caches() {
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
        let q = super::super::parse_query(
            "SELECT (COUNT(*) AS ?n) {?s <urn:p> ?a OPTIONAL {?s <urn:q> ?b}}",
            None,
            &[],
        )
        .unwrap();
        let options = QueryOptions {
            no_cache: true,
            ..Default::default()
        };
        let mut p =
            super::super::prepare_query(store.snapshot(), &q, &options, None, None).unwrap();
        let mut group = &mut p.node;
        while matches!(
            group.kind,
            Kind::Project(_)
                | Kind::Slice { offset: 0, .. }
                | Kind::Extend(_, super::super::expr::Expr::Var(_))
        ) {
            group = &mut group.children[0];
        }
        assert!(matches!(group.kind, Kind::Group { .. }));
        group.children[0].est = 1_000_000.0;
        assert!(auto_streaming(&p.ctx, &p.node, p.kind, &Default::default()));
        let cached =
            super::super::prepare_query(store.snapshot(), &q, &Default::default(), None, None)
                .unwrap();
        assert!(!auto_streaming(
            &cached.ctx,
            &p.node,
            p.kind,
            &Default::default()
        ));
        let low = QueryOptions {
            no_cache: true,
            max_memory_bytes: Some(2 << 20),
            ..Default::default()
        };
        let low = super::super::prepare_query(store.snapshot(), &q, &low, None, None).unwrap();
        assert!(!auto_streaming(
            &low.ctx,
            &p.node,
            p.kind,
            &Default::default()
        ));
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

    #[test]
    fn successor_carries_and_stops_at_maximum() {
        assert_eq!(successor([7, 9, u64::MAX, u64::MAX]), Some([7, 10, 0, 0]));
        assert_eq!(successor([u64::MAX; 4]), None);
    }
}
