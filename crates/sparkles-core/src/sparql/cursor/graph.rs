//! Bounded graph output over snapshot-owned WHERE batches. Exact CONSTRUCT
//! deduplication is growing, budgeted state. DESCRIBE has an explicit barrier.
use super::{
    Buffer, CursorOptions, CursorPlan, CursorStats, CursorStatus, FallbackPolicy, QueryBatch,
    QueryCursor, capacity_bytes, merge, open_pattern_cursor,
};
use crate::error::{Error, Result};
use crate::sparql::ctx::OwnedCharge;
use crate::sparql::describe::DescribeOptions;
use crate::sparql::{QueryKind, QueryOptions, depth};
use crate::store::Snapshot;
use oxrdf::{GraphName, Quad};
use rustc_hash::FxHashSet;
use spargebra::GraphTemplate;
use spargebra::term::TriplePattern;
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
        policy: FallbackPolicy,
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
            spargebra::Query::Describe { dataset, .. } => {
                if policy == FallbackPolicy::RejectMaterialization {
                    return Err(Error::Unsupported(
                        "DESCRIBE traversal is an explicit materialization barrier".into(),
                    ));
                }
                Some(Self::Describe {
                    options: options.describe.clone(),
                    explicit: dataset.is_some()
                        || !options.default_graph_uris.is_empty()
                        || !options.named_graph_uris.is_empty()
                        || !options.default_graph_extra.is_empty(),
                })
            }
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
    template_bytes: u64,
    transform_ms: f64,
    ended: Option<Instant>,
    truncated: bool,
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
    query_bytes: usize,
) -> Result<GraphCursor> {
    depth::with_stack(where_cursor.depth, || {
        let base_bytes = where_cursor
            ._plan_charge
            .bytes()
            .saturating_mul(2)
            .saturating_add(where_cursor.ctx.nvars() as u64 * 16 + 4096);
        let seen_charge = OwnedCharge::new(&where_cursor.ctx, base_bytes)?;
        let map = where_cursor.vars.iter().enumerate().fold(
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
        plan.materializes = matches!(spec, Spec::Describe { .. });
        plan.full_input_before_output = plan.materializes;
        plan.reason = plan
            .materializes
            .then(|| "DESCRIBE traversal is an explicit materialization barrier".into());
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
            template_bytes: query_bytes as u64 * 16,
            transform_ms: 0.0,
            ended: None,
            truncated: false,
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
    pub fn deadline(&self) -> Option<Instant> {
        self.where_cursor.deadline()
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
                    if self.input.is_some() {
                        break;
                    }
                    let mut table = Buffer::new(&ctx, &self.where_cursor.vars, 0)?;
                    while let Some(buffer) = self
                        .where_cursor
                        .pull(self.where_cursor.options.batch_rows)?
                    {
                        merge::append(&ctx, &mut table, &buffer.table, 0..buffer.table.len)?;
                    }
                    let described =
                        crate::sparql::describe::describe(&ctx, &table.table, options, *explicit)?;
                    self.truncated = described.truncated;
                    let mut pending_bytes = 128u64;
                    let mut pending_charge = OwnedCharge::new(&ctx, capacity_bytes(&table.table))?;
                    for triple in &described.triples {
                        pending_bytes = pending_bytes.saturating_add(
                            crate::sparql::graph_triple_bytes(triple).saturating_add(256),
                        );
                    }
                    pending_charge.resize(pending_bytes.saturating_mul(2))?;
                    let pending = described
                        .triples
                        .into_iter()
                        .map(|triple| triple.in_graph(GraphName::DefaultGraph))
                        .collect::<Vec<_>>();
                    self.pending = Some(Pending {
                        quads: pending.into_iter(),
                        _charge: pending_charge,
                    });
                    // A zero-row captured batch marks the one-time traversal complete.
                    self.input = Some(QueryBatch {
                        buffer: super::BatchBuffer::Owned(Buffer::new(
                            &ctx,
                            &self.where_cursor.vars,
                            0,
                        )?),
                        variables: self.where_cursor.variables.clone(),
                        ctx: ctx.clone(),
                    });
                }
                Spec::Construct { template, graphs } => {
                    if self.input.as_ref().is_none_or(|b| self.row == b.len()) {
                        self.input = None;
                        self.row = 0;
                        self.input = self.where_cursor.next_batch()?;
                    }
                    let Some(batch) = &self.input else { break };
                    let count = template
                        .len()
                        .saturating_add(graphs.iter().map(|g| g.triples.len()).sum::<usize>());
                    let scratch = batch
                        .row_bytes(self.row)?
                        .saturating_mul(4)
                        .saturating_add(self.template_bytes)
                        .saturating_mul(count.max(1) as u64)
                        .saturating_add(4096);
                    let temporary = OwnedCharge::new(&ctx, scratch)?;
                    let mut pending = Vec::new();
                    let mut pending_charge = OwnedCharge::new(&ctx, 128)?;
                    let mut pending_bytes = 128u64;
                    crate::sparql::construct_row(
                        &ctx,
                        batch.owned_table(),
                        &self.map,
                        self.row,
                        template,
                        graphs,
                        &mut self.template_work,
                        &mut |quad| {
                            if self.seen.contains(&quad) {
                                return Ok(());
                            }
                            ctx.check_rows(self.seen.len().saturating_add(1))?;
                            ctx.produced(1)?;
                            let bytes = crate::sparql::graph_quad_bytes(&quad).saturating_add(256);
                            self.seen_bytes =
                                self.seen_bytes.saturating_add(bytes.saturating_mul(2));
                            self.seen_charge.resize(self.seen_bytes)?;
                            pending_bytes = pending_bytes.saturating_add(bytes.saturating_mul(2));
                            pending_charge.resize(pending_bytes)?;
                            self.seen.insert(quad.clone());
                            pending.push(quad);
                            Ok(())
                        },
                    )?;
                    drop(temporary);
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
    let result: Result<()> = depth::with_stack(cursor.where_cursor.depth, || {
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
        let mut writer = serializer.for_writer(output);
        let mut sent = 0usize;
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
                }
                sent += 1;
            }
        }
        if send.is_some_and(|max| sent >= max) {
            cursor.close();
        }
        writer
            .finish()
            .map_err(|e| Error::Io(std::io::Error::other(e.to_string())))?;
        Ok(())
    });
    if let Err(error) = result {
        cursor.failure = Some(error.to_string());
        cursor.finish(CursorStatus::Failed);
        return Err(error);
    }
    Ok(cursor.stats())
}
