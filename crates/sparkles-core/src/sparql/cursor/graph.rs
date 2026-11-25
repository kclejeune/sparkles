//! Bounded graph output over snapshot-owned WHERE batches. Exact CONSTRUCT
//! deduplication is growing, budgeted state. DESCRIBE describes the resources of each
//! WHERE batch that no earlier batch named, so its triples follow the batches. What it
//! has described, returned and visited is growing, budgeted state, and the labels of
//! the linked IRIs come after the last batch.
use super::{
    Buffer, CursorOptions, CursorPlan, CursorStats, CursorStatus, QueryBatch, QueryCursor,
    open_pattern_cursor,
};
use crate::error::{Error, Result};
use crate::sparql::ctx::OwnedCharge;
use crate::sparql::describe::{DescribeOptions, DescribedBatch};
use crate::sparql::{QueryKind, QueryOptions, depth};
use crate::store::Snapshot;
use oxrdf::{GraphName, Quad};
use rustc_hash::FxHashSet;
use spargebra::GraphTemplate;
use spargebra::term::TriplePattern;
use std::cell::Cell;
use std::sync::Arc;
use std::time::Instant;

pub(super) enum Spec {
    Construct {
        template: Vec<TriplePattern>,
        graphs: Vec<GraphTemplate>,
    },
    Describe {
        options: DescribeOptions,
        explicit: bool,
    },
}
impl Spec {
    pub(super) fn from_parsed(
        parsed: &spargebra::Query,
        options: &QueryOptions,
    ) -> Result<Option<Self>> {
        Ok(match parsed {
            spargebra::Query::Construct {
                template,
                graph_templates,
                ..
            } => Some(Self::Construct {
                template: template.clone(),
                graphs: graph_templates.clone(),
            }),
            spargebra::Query::Describe { dataset, .. } => Some(Self::Describe {
                options: options.describe.clone(),
                explicit: dataset.is_some()
                    || !options.default_graph_uris.is_empty()
                    || !options.named_graph_uris.is_empty()
                    || !options.default_graph_extra.is_empty(),
            }),
            _ => None,
        })
    }
    fn kind(&self) -> QueryKind {
        match self {
            Self::Construct { .. } => QueryKind::Construct,
            Self::Describe { .. } => QueryKind::Describe,
        }
    }
}

/// Immutable decoded graph terms. Retained batches remain on the query budget
/// after the producer is closed, and do not require a live dataset handle.
pub struct GraphBatch {
    quads: Vec<Quad>,
    depth: usize,
    _charge: OwnedCharge,
}
impl GraphBatch {
    pub fn len(&self) -> usize {
        self.quads.len()
    }
    pub fn is_empty(&self) -> bool {
        self.quads.is_empty()
    }
    pub fn quads(&self) -> &[Quad] {
        &self.quads
    }
}

impl Drop for GraphBatch {
    fn drop(&mut self) {
        depth::with_stack(self.depth, || self.quads.clear());
    }
}

struct Pending {
    quads: std::vec::IntoIter<Quad>,
    _charge: OwnedCharge,
}

impl Pending {
    fn from_description(
        mut described: DescribedBatch,
        #[cfg(test)] ctx: &crate::sparql::ctx::Ctx,
    ) -> Result<Self> {
        let mut pending_bytes = 128u64;
        for triple in &described.triples {
            pending_bytes = pending_bytes
                .saturating_add(crate::sparql::graph_triple_bytes(triple).saturating_add(256));
        }
        let quad_slots =
            (described.triples.len() as u64).saturating_mul(std::mem::size_of::<Quad>() as u64);
        // Keep the producer's charge while admitting the additional Quad slots.
        let peak = described
            .charge
            .bytes()
            .max(pending_bytes)
            .saturating_add(quad_slots);
        described.charge.resize(peak)?;
        #[cfg(test)]
        ctx.chunk_event("describe-quad-handoff", described.triples.len());
        let pending = described
            .triples
            .into_iter()
            .map(|triple| triple.in_graph(GraphName::DefaultGraph))
            .collect::<Vec<_>>();
        described.charge.resize(pending_bytes)?;
        Ok(Self {
            quads: pending.into_iter(),
            _charge: described.charge,
        })
    }
}

pub struct GraphCursor {
    where_cursor: QueryCursor,
    spec: Spec,
    input: Option<QueryBatch>,
    row: usize,
    map: Vec<Option<usize>>,
    pending: Option<Pending>,
    seen: FxHashSet<Quad>,
    seen_charge: OwnedCharge,
    seen_bytes: u64,
    base_bytes: u64,
    plan: CursorPlan,
    status: CursorStatus,
    failure: Option<String>,
    emitted: u64,
    template_work: usize,
    transform_ms: f64,
    ended: Option<Instant>,
    truncated: bool,
    /// What a DESCRIBE keeps between WHERE batches.
    described: crate::sparql::describe::Streamed,
    /// Whether a DESCRIBE has described its last WHERE batch.
    described_all: bool,
}

pub fn graph_cursor(
    snapshot: Arc<Snapshot>,
    query: &str,
    options: &QueryOptions,
    cursor: &CursorOptions,
) -> Result<GraphCursor> {
    let (where_cursor, spec, _) =
        open_pattern_cursor(snapshot, query, options, cursor, Some(QueryKind::Construct))?;
    from_pattern(
        where_cursor,
        spec.expect("graph query has a graph specification"),
        query.len(),
    )
}

pub(super) fn from_pattern(
    where_cursor: QueryCursor,
    spec: Spec,
    _query_bytes: usize,
) -> Result<GraphCursor> {
    depth::with_stack(where_cursor.depth, || {
        let base_bytes = where_cursor
            ._plan_charge
            .bytes()
            .saturating_mul(2)
            .saturating_add(where_cursor.ctx.nvars() as u64 * 16 + 4096);
        let seen_charge = OwnedCharge::new(&where_cursor.ctx, base_bytes)?;
        let map = where_cursor.variables.ids.iter().enumerate().fold(
            vec![None; where_cursor.ctx.nvars()],
            |mut map, (column, var)| {
                map[*var as usize] = Some(column);
                map
            },
        );
        let mut plan = where_cursor.plan.clone();
        plan.children = vec![plan.clone()];
        plan.operator.children.clear();
        plan.operator.operator = format!("{:?}", spec.kind()).to_uppercase();
        plan.operator.description.clear();
        plan.operator.columns = vec!["s".into(), "p".into(), "o".into(), "g".into()];
        plan.operator.actual_rows = 0;
        plan.growing_state = true;
        plan.materializes = false;
        plan.full_input_before_output = false;
        plan.reason = matches!(spec, Spec::Describe { .. }).then(|| {
            "DESCRIBE describes the new resources of each batch and keeps what it described under the memory budget".into()
        });
        Ok(GraphCursor {
            where_cursor,
            spec,
            input: None,
            row: 0,
            map,
            pending: None,
            seen: Default::default(),
            seen_charge,
            seen_bytes: base_bytes,
            base_bytes,
            plan,
            status: CursorStatus::Open,
            failure: None,
            emitted: 0,
            template_work: 0,
            transform_ms: 0.0,
            ended: None,
            truncated: false,
            described: Default::default(),
            described_all: false,
        })
    })
}
impl GraphCursor {
    pub fn kind(&self) -> QueryKind {
        self.spec.kind()
    }
    pub fn status(&self) -> CursorStatus {
        self.status
    }
    pub fn plan(&self) -> &CursorPlan {
        &self.plan
    }
    pub fn plan_json(&self) -> Result<String> {
        depth::with_stack(self.where_cursor.depth, || {
            serde_json::to_string(&self.plan).map_err(|e| Error::invalid(e.to_string()))
        })
    }
    pub fn cancellation_token(&self) -> Arc<std::sync::atomic::AtomicBool> {
        self.where_cursor.cancellation_token()
    }
    /// What query-time reasoning derived for the query (spec F17).
    pub fn reasoning(&self) -> Option<Arc<crate::sparql::reasoning::Applied>> {
        self.where_cursor.reasoning()
    }
    pub fn deadline(&self) -> Option<Instant> {
        self.where_cursor.deadline()
    }
    pub fn deadline_pause(&self) -> Arc<std::sync::atomic::AtomicU64> {
        self.where_cursor.deadline_pause()
    }
    pub fn describe_truncated(&self) -> bool {
        self.truncated
    }
    pub fn stats(&self) -> CursorStats {
        let mut stats = self.where_cursor.stats();
        stats.status = self.status;
        stats.emitted_rows = self.emitted;
        stats.error = self.failure.clone();
        stats.timing.exec_ms += self.transform_ms;
        stats.timing.total_ms = self
            .ended
            .unwrap_or_else(Instant::now)
            .duration_since(self.where_cursor.started)
            .as_secs_f64()
            * 1000.0;
        stats
    }
    pub fn next_batch(&mut self) -> Result<Option<GraphBatch>> {
        self.next_at_most(self.where_cursor.options.batch_rows)
    }
    pub(crate) fn next_at_most(&mut self, rows: usize) -> Result<Option<GraphBatch>> {
        if self.status != CursorStatus::Open {
            return Ok(None);
        }
        let began = Instant::now();
        let before = self.where_cursor.timing.exec_ms;
        let result = depth::with_stack(self.where_cursor.depth, || self.pull(rows));
        self.transform_ms += (began.elapsed().as_secs_f64() * 1000.0
            - (self.where_cursor.timing.exec_ms - before))
            .max(0.0);
        depth::with_stack(self.where_cursor.depth, || {
            self.plan.children[0] = self.where_cursor.plan.clone()
        });
        match result {
            Ok(batch) => {
                if let Some(batch) = &batch {
                    self.emitted = self.emitted.saturating_add(batch.len() as u64);
                }
                self.plan.operator.actual_rows = self.emitted.min(i64::MAX as u64) as i64;
                self.plan.operator.time_ms = self.stats().timing.exec_ms;
                self.plan.operator.warnings = self.where_cursor.ctx.warnings();
                if batch.is_none() {
                    self.finish(CursorStatus::Complete);
                    self.plan.complete = true;
                }
                Ok(batch)
            }
            Err(error) => {
                self.failure = Some(error.to_string());
                self.finish(CursorStatus::Failed);
                Err(error)
            }
        }
    }
    fn pull(&mut self, rows: usize) -> Result<Option<GraphBatch>> {
        let ctx = self.where_cursor.ctx.clone();
        ctx.check()?;
        let mut quads = Vec::new();
        let mut bytes = 128;
        let mut charge = OwnedCharge::new(&ctx, bytes)?;
        while quads.len() < rows.min(self.where_cursor.options.batch_rows)
            && (quads.is_empty() || bytes < self.where_cursor.options.batch_bytes as u64)
        {
            ctx.check()?;
            if let Some(pending) = &mut self.pending {
                if let Some(quad) = pending.quads.next() {
                    bytes = bytes
                        .saturating_add(crate::sparql::graph_quad_bytes(&quad).saturating_add(256));
                    charge.resize(bytes)?;
                    quads.push(quad);
                    continue;
                }
                self.pending = None;
            }
            match &self.spec {
                Spec::Describe { options, explicit } => {
                    if self.described_all {
                        break;
                    }
                    let batch = self
                        .where_cursor
                        .pull(self.where_cursor.options.batch_rows)?;
                    let last = batch.is_none();
                    let table = match batch {
                        Some(buffer) => buffer,
                        None => Buffer::new(&ctx, &self.where_cursor.variables.ids, 0)?,
                    };
                    let described = crate::sparql::describe::describe_batch(
                        &ctx,
                        &table.table,
                        options,
                        *explicit,
                        &mut self.described,
                        last,
                    )?;
                    drop(table);
                    self.truncated = self.described.truncated();
                    self.described_all = last || self.truncated;
                    self.pending = Some(Pending::from_description(
                        described,
                        #[cfg(test)]
                        &ctx,
                    )?);
                }
                Spec::Construct { template, graphs } => {
                    if self.input.as_ref().is_none_or(|b| self.row == b.len()) {
                        self.input = None;
                        self.row = 0;
                        self.input = self.where_cursor.next_batch()?;
                    }
                    let Some(batch) = &self.input else { break };
                    // Guard precedes pending values so partial output is destroyed
                    // before credit on every refusal. Incoming terms carry their own
                    // predecode admission through the shared emitter.
                    let mut pending_charge = OwnedCharge::new(&ctx, 0)?;
                    let mut pending = Vec::new();
                    let mut pending_storage = 0u64;
                    crate::sparql::construct_row(
                        &ctx,
                        batch.owned_table(),
                        &self.map,
                        self.row,
                        template,
                        graphs,
                        &mut self.template_work,
                        &mut |quad, incoming| {
                            if self.seen.contains(&quad) {
                                return Ok(());
                            }
                            ctx.check_rows(self.seen.len().saturating_add(1))?;
                            ctx.produced(1)?;
                            let bytes = crate::sparql::graph_quad_bytes(&quad).saturating_add(256);
                            self.seen_bytes =
                                self.seen_bytes.saturating_add(bytes.saturating_mul(2));
                            self.seen_charge.resize(self.seen_bytes)?;
                            pending_charge.adopt_charge(incoming)?;
                            if pending.len() == pending.capacity() {
                                let cap = pending.capacity().saturating_mul(2).max(4);
                                let storage = cap
                                    .checked_mul(std::mem::size_of::<Quad>())
                                    .and_then(|n| u64::try_from(n).ok())
                                    .ok_or_else(|| ctx.memory_exceeded(u64::MAX))?;
                                pending_charge.resize(
                                    pending_charge
                                        .bytes()
                                        .checked_add(storage)
                                        .ok_or_else(|| ctx.memory_exceeded(u64::MAX))?,
                                )?;
                                pending
                                    .try_reserve_exact(cap - pending.len())
                                    .map_err(|_| ctx.memory_exceeded(storage))?;
                                if pending.capacity() > cap {
                                    return Err(Error::invalid("pending graph exceeds admission"));
                                }
                                pending_charge.resize(pending_charge.bytes() - pending_storage)?;
                                pending_storage = storage;
                            }
                            self.seen.insert(quad.clone());
                            pending.push(quad);
                            Ok(())
                        },
                    )?;
                    self.row += 1;
                    self.pending = Some(Pending {
                        quads: pending.into_iter(),
                        _charge: pending_charge,
                    });
                }
            }
        }
        ctx.check()?;
        if quads.is_empty() {
            Ok(None)
        } else {
            Ok(Some(GraphBatch {
                quads,
                depth: self.where_cursor.depth,
                _charge: charge,
            }))
        }
    }
    fn finish(&mut self, status: CursorStatus) {
        self.status = status;
        self.ended = Some(Instant::now());
        self.where_cursor.close();
        depth::with_stack(self.where_cursor.depth, || {
            self.input = None;
            self.pending = None;
            self.seen.clear();
            self.seen.shrink_to_fit();
            // what a DESCRIBE kept, with its charge; `truncated` stays
            self.described = Default::default();
            self.described_all = true;
        });
        let _ = self.seen_charge.resize(self.base_bytes);
    }
    pub(crate) fn check(&self) -> Result<()> {
        self.where_cursor.ctx.check()
    }
    pub(in crate::sparql) fn charge(&self, bytes: u64) -> Result<OwnedCharge> {
        OwnedCharge::new(&self.where_cursor.ctx, bytes)
    }
    pub(crate) fn stack_depth(&self) -> usize {
        self.where_cursor.depth
    }
    pub(crate) fn fail_output(&mut self, error: &Error) {
        self.failure = Some(error.to_string());
        self.finish(CursorStatus::Failed);
    }
    pub fn close(&mut self) {
        if self.status == CursorStatus::Open {
            self.finish(CursorStatus::Stopped);
        }
    }
}

#[cfg(test)]
#[test]
fn describe_handoff_credits_triples_and_refuses_extra_quad_slots() {
    use crate::io::{RdfFormat, Source};
    use crate::sparql::ctx::Ctx;
    use crate::sparql::describe::{Streamed, describe_batch};
    use crate::sparql::table::Table;
    use crate::store::{Store, StoreOptions};
    use oxrdf::{NamedNode, Term};
    use std::fmt::Write;
    use std::sync::atomic::{AtomicU64, Ordering};

    const ROWS: usize = 64;
    let store = Store::in_memory(StoreOptions::default());
    let mut ttl = String::new();
    for i in 0..ROWS {
        writeln!(ttl, "<urn:s> <urn:p> \"value{i}\" .").unwrap();
    }
    store
        .load(&[Source::from_bytes(
            ttl.into_bytes(),
            RdfFormat::Turtle,
            None,
        )])
        .unwrap();
    let mut ctx = Ctx::new(store.snapshot());
    ctx.mem_limit = 1 << 20;
    ctx.enable_cursor_accounting().unwrap();
    let subject = ctx
        .snap
        .lookup_term(&Term::NamedNode(NamedNode::new_unchecked("urn:s")))
        .unwrap();
    let mut table = Table::new(vec![0]);
    table.push_row(&[subject]);
    let ctx = ctx.into_shared();

    for refuse in [true, false] {
        let mut state = Streamed::default();
        let described = describe_batch(
            &ctx,
            &table,
            &DescribeOptions::default(),
            false,
            &mut state,
            true,
        )
        .unwrap();
        assert_eq!(described.triples.len(), ROWS);
        let producer_bytes = described.charge.bytes();
        assert!(producer_bytes > 0);
        let state_bytes = ctx.live_bytes() - producer_bytes;
        let pending_bytes = 128
            + described
                .triples
                .iter()
                .map(|triple| crate::sparql::graph_triple_bytes(triple) + 256)
                .sum::<u64>();
        let held_at_conversion =
            producer_bytes.max(pending_bytes) + (ROWS * std::mem::size_of::<Quad>()) as u64;
        let observed = Arc::new(AtomicU64::new(0));
        let output = observed.clone();
        let weak = Arc::downgrade(&ctx);
        *ctx.chunk_hook.lock() = Some(Arc::new(move |stage, count| {
            if stage == "describe-quad-handoff" {
                assert_eq!(count, ROWS);
                output.store(weak.upgrade().unwrap().live_bytes(), Ordering::Relaxed);
            }
        }));
        if refuse {
            let hold = ctx
                .charge(ctx.mem_limit - state_bytes - held_at_conversion + 1)
                .unwrap();
            let live = ctx.live_bytes();
            let Err(err) = Pending::from_description(described, &ctx) else {
                panic!("quad slots must refuse after triple decode");
            };
            assert!(
                matches!(err, Error::BudgetExceeded(b) if b.kind == crate::BudgetKind::Memory && b.requested == ctx.mem_limit + 1)
            );
            assert_eq!(observed.load(Ordering::Relaxed), 0);
            assert_eq!(ctx.live_bytes(), live - producer_bytes);
            drop(hold);
        } else {
            let pending = Pending::from_description(described, &ctx).unwrap();
            assert_eq!(
                observed.load(Ordering::Relaxed),
                state_bytes + held_at_conversion
            );
            assert_eq!(pending.quads.len(), ROWS);
            assert_eq!(ctx.live_bytes(), state_bytes + pending_bytes);
            drop(pending);
            assert_eq!(ctx.live_bytes(), state_bytes);
        }
        *ctx.chunk_hook.lock() = None;
        drop(state);
    }
}
impl Drop for GraphCursor {
    fn drop(&mut self) {
        depth::with_stack(self.where_cursor.depth, || {
            self.close();
            self.spec = Spec::Construct {
                template: Vec::new(),
                graphs: Vec::new(),
            };
            self.plan.children.clear();
            self.plan.operator.children.clear();
        });
    }
}

/// Stream graph terms without collecting the completed result. Prefix/send limits
/// stop upstream execution. Writer failures fuse the cursor and leave partial output.
pub fn write_cursor_graph(
    cursor: &mut GraphCursor,
    format: oxrdfio::RdfFormat,
    output: &mut dyn std::io::Write,
    send: Option<usize>,
    prefixes: &[(String, String)],
) -> Result<CursorStats> {
    write_cursor_graph_counted(cursor, format, output, send, prefixes, &Cell::new(0))
}

/// [`write_cursor_graph`], counting in `sent` the triples and quads that reached `output`
/// whole, as the eager graph writers count them. The statements go out through the
/// buffer of the eager writers.
pub fn write_cursor_graph_counted(
    cursor: &mut GraphCursor,
    format: oxrdfio::RdfFormat,
    output: &mut dyn std::io::Write,
    send: Option<usize>,
    prefixes: &[(String, String)],
    sent: &Cell<u64>,
) -> Result<CursorStats> {
    let result = crate::sparql::results::write_buffered(output, sent, &mut |w, staged| {
        graph_to(cursor, format, w, send, prefixes, staged)
    });
    if let Err(error) = result {
        cursor.failure = Some(error.to_string());
        cursor.finish(CursorStatus::Failed);
        return Err(error);
    }
    Ok(cursor.stats())
}

/// [`write_cursor_graph_counted`] into the buffer of the eager writers.
fn graph_to(
    cursor: &mut GraphCursor,
    format: oxrdfio::RdfFormat,
    output: &mut crate::sparql::results::Buffered<'_>,
    send: Option<usize>,
    prefixes: &[(String, String)],
    staged: &Cell<u64>,
) -> Result<()> {
    use std::io::Write as _;
    depth::with_stack(cursor.where_cursor.depth, || {
        let _serializer_charge = OwnedCharge::new(
            &cursor.where_cursor.ctx,
            (128 << 10)
                + prefixes
                    .iter()
                    .map(|(a, b)| (a.len() + b.len()) as u64 * 4 + 128)
                    .sum::<u64>(),
        )?;
        let serializer = crate::io::with_prefixes(
            oxrdfio::RdfSerializer::from_format(format),
            prefixes.iter().cloned(),
        );
        let shared = std::cell::RefCell::new(output);
        let mut writer = serializer.for_writer(crate::sparql::results::Shared(&shared));
        let mut sent = 0usize;
        let mut first = true;
        while send.is_none_or(|max| sent < max) {
            let Some(batch) =
                cursor.next_at_most(send.map_or(usize::MAX, |max| max.saturating_sub(sent)))?
            else {
                break;
            };
            for quad in batch.quads() {
                if send.is_some_and(|max| sent == max) {
                    break;
                }
                if sent.is_multiple_of(1024) {
                    cursor.where_cursor.ctx.check()?;
                }
                if format.supports_datasets() || quad.graph_name == GraphName::DefaultGraph {
                    writer
                        .serialize_quad(quad)
                        .map_err(|e| Error::Io(std::io::Error::other(e.to_string())))?;
                    staged.set(staged.get() + 1);
                }
                sent += 1;
            }
            // what the first batch wrote reaches a writer that commits on a flush; a
            // serializer that groups triples by subject may still hold the last ones
            if std::mem::take(&mut first) {
                shared.borrow_mut().flush()?;
            }
        }
        if send.is_some_and(|max| sent >= max) {
            cursor.close();
        }
        writer
            .finish()
            .map_err(|e| Error::Io(std::io::Error::other(e.to_string())))?;
        Ok(())
    })
}
