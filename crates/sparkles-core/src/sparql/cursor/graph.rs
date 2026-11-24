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

    fn retained_bytes(&self) -> u64 {
        fn named_bytes(pattern: &spargebra::term::NamedNodePattern) -> u64 {
            match pattern {
                spargebra::term::NamedNodePattern::NamedNode(n) => n.as_str().len() as u64,
                spargebra::term::NamedNodePattern::Variable(v) => v.as_str().len() as u64,
            }
        }
        fn term_bytes(pattern: &spargebra::term::TermPattern) -> u64 {
            use spargebra::term::TermPattern as T;
            match pattern {
                T::NamedNode(n) => n.as_str().len() as u64,
                T::BlankNode(b) => b.as_str().len() as u64,
                T::Variable(v) => v.as_str().len() as u64,
                T::Literal(l) => (l.value().len() as u64)
                    .saturating_add(l.datatype().as_str().len() as u64)
                    .saturating_add(l.language().map_or(0, |s| s.len() as u64)),
                T::Triple(tp) => {
                    (std::mem::size_of::<TriplePattern>() as u64).saturating_add(triple_bytes(tp))
                }
            }
        }
        fn triple_bytes(tp: &TriplePattern) -> u64 {
            term_bytes(&tp.subject)
                .saturating_add(named_bytes(&tp.predicate))
                .saturating_add(term_bytes(&tp.object))
        }
        match self {
            Self::Construct { template, graphs } => {
                let mut bytes = (template
                    .capacity()
                    .saturating_mul(std::mem::size_of::<TriplePattern>())
                    as u64)
                    .saturating_add(
                        graphs
                            .capacity()
                            .saturating_mul(std::mem::size_of::<GraphTemplate>())
                            as u64,
                    );
                for tp in template {
                    bytes = bytes.saturating_add(triple_bytes(tp));
                }
                for graph in graphs {
                    bytes = bytes
                        .saturating_add(term_bytes(&graph.name))
                        .saturating_add(
                            graph
                                .triples
                                .capacity()
                                .saturating_mul(std::mem::size_of::<TriplePattern>())
                                as u64,
                        );
                    for tp in &graph.triples {
                        bytes = bytes.saturating_add(triple_bytes(tp));
                    }
                }
                bytes
            }
            Self::Describe { .. } => 0,
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

/// CONSTRUCT views number repeated solutions before minting their template blanks.
struct ViewBlanks {
    columns: Vec<Option<usize>>,
    counts: rustc_hash::FxHashMap<Vec<crate::id::Id>, i64>,
    charge: OwnedCharge,
}

impl ViewBlanks {
    fn occurrence(
        &mut self,
        ctx: &crate::sparql::ctx::Ctx,
        table: &crate::sparql::table::Table,
        row: usize,
        scratch: &crate::sparql::ctx::Charge<'_>,
    ) -> Result<(Vec<Vec<u8>>, Option<i64>)> {
        use crate::id::{Id, Tag};
        use sparkles_budget::sizes::{table_bytes, table_grown};
        let width = self.columns.len();
        scratch
            .add((width * (std::mem::size_of::<Id>() + std::mem::size_of::<Vec<u8>>())) as u64)?;
        let ids = self
            .columns
            .iter()
            .map(|c| c.map_or(Id::UNDEF, |c| table.cols[c][row]))
            .collect::<Vec<_>>();
        let occurrence;
        if let Some(n) = self.counts.get_mut(&ids) {
            occurrence = *n;
            *n = n
                .checked_add(1)
                .ok_or_else(|| Error::invalid("template multiplicity overflows"))?;
        } else {
            let capacity = self.counts.capacity();
            let grown = table_grown(capacity, self.counts.len(), 1);
            let slot = std::mem::size_of::<(Vec<Id>, i64)>();
            let old = table_bytes(capacity, slot) as u64;
            let new = table_bytes(grown, slot) as u64;
            let bytes = self
                .charge
                .bytes()
                .saturating_add((width * std::mem::size_of::<Id>()) as u64);
            self.charge.resize(if grown == capacity {
                bytes
            } else {
                bytes.saturating_add(new)
            })?;
            self.counts.insert(ids.clone(), 1);
            if grown != capacity {
                self.charge
                    .resize(self.charge.bytes().saturating_sub(old))?;
            }
            occurrence = 0;
        }
        let mut keys = Vec::with_capacity(width);
        for id in ids {
            ctx.check()?;
            let key = match id.tag() {
                Tag::Undef | Tag::Special => Vec::new(),
                _ => {
                    scratch.add(ctx.decoded_bytes(id)?.saturating_mul(2))?;
                    ctx.reserved_batch_term(id)?
                        .map_or_else(Vec::new, |term| crate::id::term_key(&term))
                }
            };
            keys.push(key);
        }
        Ok((keys, (occurrence > 0).then_some(occurrence)))
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
    view_blanks: Option<ViewBlanks>,
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
            .saturating_add(spec.retained_bytes())
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
            view_blanks: None,
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
    pub(super) fn for_view(
        where_cursor: QueryCursor,
        mut spec: Spec,
        template_columns: Option<&[String]>,
    ) -> Result<Self> {
        use spargebra::term::TermPattern;
        if let Spec::Construct { template, .. } = &mut spec {
            // The maintained template does not instantiate nested triple patterns.
            template.retain(|t| {
                !matches!(t.subject, TermPattern::Triple(_))
                    && !matches!(t.object, TermPattern::Triple(_))
            });
            template.shrink_to_fit();
        }
        let mut graph = from_pattern(where_cursor, spec, 0)?;
        if let Some(template_columns) = template_columns {
            let charge = OwnedCharge::new(
                &graph.where_cursor.ctx,
                (template_columns.len() * std::mem::size_of::<Option<usize>>()) as u64,
            )?;
            let columns = template_columns
                .iter()
                .map(|name| {
                    graph
                        .where_cursor
                        .variables
                        .names
                        .iter()
                        .position(|v| v == name)
                })
                .collect();
            graph.view_blanks = Some(ViewBlanks {
                columns,
                counts: Default::default(),
                charge,
            });
        }
        Ok(graph)
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
                    let triples = crate::sparql::describe::describe_batch(
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
                    let mut pending_bytes = 128u64;
                    for triple in &triples {
                        pending_bytes = pending_bytes.saturating_add(
                            crate::sparql::graph_triple_bytes(triple).saturating_add(256),
                        );
                    }
                    let pending_charge = OwnedCharge::new(&ctx, pending_bytes.saturating_mul(2))?;
                    let pending = triples
                        .into_iter()
                        .map(|triple| triple.in_graph(GraphName::DefaultGraph))
                        .collect::<Vec<_>>();
                    self.pending = Some(Pending {
                        quads: pending.into_iter(),
                        _charge: pending_charge,
                    });
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
                    let scratch = ctx.charge(0)?;
                    let skolem = self
                        .view_blanks
                        .as_mut()
                        .map(|blanks| {
                            blanks.occurrence(&ctx, batch.owned_table(), self.row, &scratch)
                        })
                        .transpose()?;
                    let mint = |label: &str| {
                        let (keys, occurrence) = skolem.as_ref().expect("view blank solution");
                        let b = crate::incr::ops::template::skolem("", label, keys, *occurrence);
                        crate::store::bnode_for(crate::id::Id::bnode(crate::id::default_bnode_id(
                            &b,
                        )))
                    };
                    crate::sparql::construct_row(
                        &ctx,
                        batch.owned_table(),
                        &self.map,
                        self.row,
                        template,
                        graphs,
                        skolem
                            .as_ref()
                            .map(|_| &mint as &dyn Fn(&str) -> oxrdf::BlankNode),
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
            self.view_blanks = None;
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
    /// The query context that the output writer charges its buffer to.
    pub(in crate::sparql) fn output_ctx(&self) -> Arc<crate::sparql::ctx::Ctx> {
        self.where_cursor.ctx.clone()
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
    let rows = crate::sparql::results::RowCount::new();
    let options = crate::sparql::results::WriteOptions {
        send,
        prefixes,
        ..crate::sparql::results::WriteOptions::new(
            crate::sparql::results::Format::Rdf(format),
            &rows,
        )
    };
    crate::sparql::results::write(
        crate::sparql::results::Source::Graph(cursor),
        options,
        output,
    )
    .map(|s| s.expect("cursor statistics"))
}
