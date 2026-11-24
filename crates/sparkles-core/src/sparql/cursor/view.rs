//! Native streaming `SERVICE view:read` cursor over immutable shared view versions
//! (item 1121).
//!
//! Current and stale reads hold an `Arc<ViewVersion>` with no circuit lock held across
//! pulls and no whole-result copy or upfront term decoding. History reads rewind using
//! an admitted net delta while streaming the shared version, and expired-history or
//! alternate-reading `SELECT` reads stream through a child cursor charged to the
//! caller's query memory budget. Multiplicities span batch boundaries without expanding
//! more rows than each batch requests.
//!
//! Nothing opens before the first pull, because operators are also built only to learn
//! whether an EXISTS pattern streams.

use super::{Buffer, CursorOptions, CursorPlan, QueryBatch, QueryCursor, graph};
use crate::error::Result;
use crate::id::{self, Id, Tag};
use crate::sparql::QueryOptions;
use crate::sparql::ctx::{Ctx, OwnedCharge};
use crate::sparql::plan::{Kind, Node};
use crate::sparql::table::VarId;
use crate::sparql::viewsvc::{
    self, OpenedView, RewoundView, ViewOpen, ViewRead, ViewRebuild, ViewRows, ViewSpec, ViewVersion,
};
use oxrdf::Term;
use std::sync::Arc;
use std::sync::atomic::Ordering;

/// Every `ViewRead` leaf runs natively through this reader.
pub(super) fn eligible(ctx: &Ctx, node: &Node) -> bool {
    matches!(&node.kind, Kind::ViewRead(_))
        && node.children.is_empty()
        && ctx
            .views
            .as_ref()
            .is_none_or(|source| source.streams_views())
}

pub(super) struct View {
    node: Node,
    opened: bool,
    rebuilt: bool,
    finished_plan: Option<CursorPlan>,
    source: Option<Source>,
    /// Column index in the view (or child cursor) for each variable in `node.vars`.
    cols: Vec<Option<usize>>,
    commit_id: Id,
    commit_var: Option<VarId>,
    /// Translated ids of the current row; retained across batches when a multiplicity
    /// exceeds the remaining room in a batch.
    pending: Vec<Id>,
    pending_rem: u64,
    /// Query memory charge for query-owned open state (rewound delta, owned version,
    /// custom source rows, and small cursor scratch).
    charge: Option<OwnedCharge>,
    scratch_bytes: u64,
}

enum Source {
    /// Shared (zero-copy) or query-owned immutable `ViewVersion`.
    Version {
        version: Arc<ViewVersion>,
        at: usize,
        remaining_rows: u64,
    },
    /// Shared `ViewVersion` rewound to an earlier commit using an admitted net delta.
    Rewound {
        rewound: RewoundView,
        version_at: usize,
        removed_at: usize,
    },
    /// Rebuilt `SELECT` view streamed directly from a child cursor charged to this query.
    Rebuild {
        cursor: Box<QueryCursor>,
        batch: Option<(QueryBatch, usize)>,
        paused: u64,
    },
    Graph {
        cursor: Box<graph::GraphCursor>,
        paused: u64,
    },
    /// Predecoded rows from a streaming source with a preadmission contract.
    Rows {
        rows: ViewRows,
        at: usize,
        remaining_rows: u64,
    },
}

impl Source {
    fn rebuild_plan(&self) -> Option<&CursorPlan> {
        match self {
            Self::Rebuild { cursor, .. } => Some(cursor.plan()),
            Self::Graph { cursor, .. } => Some(cursor.plan()),
            _ => None,
        }
    }
}

#[inline]
fn translate_id(ctx: &Ctx, version: &ViewVersion, id: Id) -> Result<Id> {
    Ok(match id.tag() {
        Tag::Undef
        | Tag::Special
        | Tag::Bool
        | Tag::Int
        | Tag::Double
        | Tag::Decimal
        | Tag::DateTime
        | Tag::Date
        | Tag::BNode => id,
        Tag::Vocab if version.snap.generation.uid == ctx.snap.generation.uid => id,
        Tag::Delta
            if version.snap.generation.uid == ctx.snap.generation.uid
                && id.payload() < ctx.snap.dvocab_len =>
        {
            id
        }
        Tag::Vocab => {
            with_version_key(ctx, version, id, |key| Ok(ctx.intern_key(key)))?.unwrap_or(Id::UNDEF)
        }
        Tag::Delta => {
            // Release the read lock before interning into a potentially shared vocabulary.
            let held = ctx.charge(0)?;
            let key = with_version_key(ctx, version, id, |key| {
                held.resize(key.len() as u64)?;
                Ok(key.to_vec())
            })?;
            key.as_deref().map_or(Id::UNDEF, |k| ctx.intern_key(k))
        }
        Tag::Local => match version.terms.key_bytes(id.payload() as usize) {
            Some(k) if k.first() == Some(&b'_') => {
                let _held = ctx.charge(k.len() as u64 + 64)?;
                ctx.intern_term(&id::key_to_term(k))
            }
            Some(k) => ctx.intern_key(k),
            None => Id::UNDEF,
        },
    })
}

/// Borrow a key from the version, covering vocabulary reconstruction scratch.
fn with_version_key<T>(
    ctx: &Ctx,
    version: &ViewVersion,
    id: Id,
    f: impl FnMut(&[u8]) -> Result<T>,
) -> Result<Option<T>> {
    let generation = &version.snap.generation;
    let mut f = f;
    match id.tag() {
        Tag::Local => version
            .terms
            .key_bytes(id.payload() as usize)
            .map(f)
            .transpose(),
        Tag::Vocab => {
            let held = ctx.charge(0)?;
            generation
                .vocab
                .get_with_controlled(
                    id.payload(),
                    |need| held.resize((need as u64).saturating_mul(2)),
                    |_| ctx.check(),
                    f,
                )?
                .transpose()
        }
        Tag::Delta => generation
            .dvocab
            .with(|v| v.get(id.payload()).map(&mut f).transpose()),
        _ => Ok(None),
    }
}

/// Probe the decoded delta with one admitted row. Its terms die before the charge.
fn subtract_added(
    ctx: &Ctx,
    rewound: &mut RewoundView,
    ids: &[Id],
    weight: i64,
    charge: &mut OwnedCharge,
) -> Result<i64> {
    let held = ctx.charge((ids.len() * std::mem::size_of::<Option<Term>>()) as u64)?;
    let mut terms = Vec::with_capacity(ids.len());
    for &id in ids {
        ctx.check()?;
        let term =
            match id.tag() {
                Tag::Local | Tag::Vocab | Tag::Delta => {
                    with_version_key(ctx, &rewound.version, id, |key| {
                        held.resize(held.bytes().saturating_add(
                            (key.len() as u64).saturating_mul(8).saturating_add(128),
                        ))?;
                        Ok(id::key_to_term(key))
                    })?
                }
                Tag::Undef | Tag::Special => None,
                _ => {
                    held.resize(held.bytes().saturating_add(128))?;
                    rewound.version.snap.term(id)
                }
            };
        terms.push(term);
    }
    let Some(delta) = rewound.added.get_mut(&terms) else {
        return Ok(weight);
    };
    let take = weight.min(*delta);
    *delta -= take;
    if *delta == 0 {
        let (row, _) = rewound.added.remove_entry(&terms).expect("matched delta");
        let freed = viewsvc::row_retained_bytes(&row);
        drop(row);
        charge.resize(charge.bytes().saturating_sub(freed))?;
    }
    Ok(weight - take)
}

fn validate_and_map_columns(
    spec: &ViewSpec,
    vars: &[VarId],
    view_columns: &[String],
) -> Result<Vec<Option<usize>>> {
    for (name, _) in &spec.bind {
        if !view_columns.iter().any(|c| c == name) {
            return Err(viewsvc::bad(format!(
                "the view {:?} has no column ?{name} (its columns: {})",
                spec.name,
                view_columns
                    .iter()
                    .map(|c| format!("?{c}"))
                    .collect::<Vec<_>>()
                    .join(" ")
            )));
        }
    }
    Ok(vars
        .iter()
        .map(|v| {
            spec.bind
                .iter()
                .find(|b| b.1 == *v)
                .and_then(|b| view_columns.iter().position(|c| *c == b.0))
        })
        .collect())
}

impl View {
    pub(super) fn new(node: Node) -> Self {
        Self {
            node,
            opened: false,
            rebuilt: false,
            finished_plan: None,
            source: None,
            cols: Vec::new(),
            commit_id: Id::UNDEF,
            commit_var: None,
            pending: Vec::new(),
            pending_rem: 0,
            charge: None,
            scratch_bytes: 0,
        }
    }

    pub(super) fn rebuild_plan(&self) -> Option<&CursorPlan> {
        self.source
            .as_ref()
            .and_then(Source::rebuild_plan)
            .or(self.finished_plan.as_ref())
    }

    /// Release the view version, child cursor, rewound delta and memory charge.
    pub(super) fn release(&mut self) {
        self.opened = true;
        if let Some(source) = self.source.take() {
            self.finished_plan = source.rebuild_plan().cloned();
        }
        self.cols = Vec::new();
        self.pending = Vec::new();
        self.pending_rem = 0;
        self.charge = None;
        self.scratch_bytes = 0;
    }

    /// Whether all rows have been emitted or the reader was released.
    pub(super) fn done(&self) -> bool {
        self.opened && self.source.is_none() && self.pending_rem == 0
    }

    /// The next batch of at most `cap` rows, or `None` once the view has ended.
    pub(super) fn next(
        &mut self,
        ctx: &Arc<Ctx>,
        options: &CursorOptions,
        cap: usize,
    ) -> Result<Option<Buffer>> {
        let r = self.pull(ctx, options, cap);
        if r.is_err() {
            self.release();
        }
        r
    }

    fn open(&mut self, ctx: &Arc<Ctx>, options: &CursorOptions) -> Result<()> {
        let Kind::ViewRead(spec) = &self.node.kind else {
            unreachable!("a ViewRead node")
        };
        ctx.check()?;
        let Some(source) = ctx.views.as_ref() else {
            return Err(crate::incr::error(
                "view-not-found",
                format!("no view {:?}: views are not available here", spec.name),
            ));
        };
        let scratch = (self.node.vars.len()
            * (std::mem::size_of::<Option<usize>>() + std::mem::size_of::<Id>()))
            as u64;
        let mut charge = OwnedCharge::new(ctx, scratch)?;
        let open = ViewOpen {
            read: ViewRead {
                name: &spec.name,
                at: ctx.snap.commit,
                snap: Some(&ctx.snap),
                allow_stale: spec.allow_stale,
                caller: ctx.graphs.as_ref(),
                deadline: ctx.current_deadline(),
                aggregate_errors: Some(ctx.aggregate_errors),
            },
        };
        let opened = source.open_view(&open, &|| ctx.check(), &mut |bytes| {
            charge.resize(scratch.saturating_add(bytes))
        })?;
        ctx.check()?;
        self.scratch_bytes = scratch;
        self.commit_var = spec.commit;
        self.pending = vec![Id::UNDEF; self.node.vars.len()];
        self.pending_rem = 0;

        match opened {
            OpenedView::Shared(version) | OpenedView::Owned(version) => {
                if let Some(m) = version.evaluated_under {
                    ctx.warn(viewsvc::evaluated(&spec.name, m));
                }
                self.cols = validate_and_map_columns(spec, &self.node.vars, &version.columns)?;
                self.commit_id = Id::from_i64(version.commit as i64).unwrap_or(Id::UNDEF);
                let remaining_rows = version.table.total_rows;
                self.charge = Some(charge);
                if remaining_rows > 0 {
                    self.source = Some(Source::Version {
                        version,
                        at: 0,
                        remaining_rows,
                    });
                } else {
                    self.release();
                }
            }
            OpenedView::Rewound(rewound) => {
                self.cols =
                    validate_and_map_columns(spec, &self.node.vars, &rewound.version.columns)?;
                self.commit_id = Id::from_i64(rewound.commit as i64).unwrap_or(Id::UNDEF);
                self.charge = Some(charge);
                if !rewound.version.table.is_empty() || !rewound.removed.is_empty() {
                    self.source = Some(Source::Rewound {
                        rewound,
                        version_at: 0,
                        removed_at: 0,
                    });
                } else {
                    self.release();
                }
            }
            OpenedView::Rebuild(rebuild) => {
                if let Some(m) = rebuild.evaluated_under {
                    ctx.warn(viewsvc::evaluated(&spec.name, m));
                }
                self.cols = validate_and_map_columns(spec, &self.node.vars, &rebuild.columns)?;
                self.commit_id = Id::from_i64(rebuild.commit as i64).unwrap_or(Id::UNDEF);
                let paused = ctx.deadline_pause.load(Ordering::Relaxed);
                let (cursor, graph) = Self::open_rebuild(ctx, options, &rebuild)?;
                let source = if let Some(graph) = graph {
                    Source::Graph {
                        cursor: Box::new(graph::GraphCursor::for_view(
                            cursor,
                            graph,
                            rebuild.template_columns.as_deref(),
                        )?),
                        paused,
                    }
                } else {
                    self.cols =
                        validate_and_map_columns(spec, &self.node.vars, cursor.variables())?;
                    Source::Rebuild {
                        cursor: Box::new(cursor),
                        batch: None,
                        paused,
                    }
                };
                let snapshot_bytes = rebuild.snapshot_bytes();
                drop(rebuild);
                charge.resize(scratch.saturating_add(snapshot_bytes))?;
                self.rebuilt = true;
                self.charge = Some(charge);
                self.source = Some(source);
            }
            OpenedView::Rows(rows) => {
                if let Some(m) = rows.evaluated_under {
                    ctx.warn(viewsvc::evaluated(&spec.name, m));
                }
                self.cols = validate_and_map_columns(spec, &self.node.vars, &rows.columns)?;
                self.commit_id = Id::from_i64(rows.commit as i64).unwrap_or(Id::UNDEF);
                let remaining_rows: u64 = rows
                    .rows
                    .iter()
                    .map(|(_, w)| (*w).max(0) as u64)
                    .fold(0u64, u64::saturating_add);
                self.charge = Some(charge);
                if remaining_rows > 0 {
                    self.source = Some(Source::Rows {
                        rows,
                        at: 0,
                        remaining_rows,
                    });
                } else {
                    self.release();
                }
            }
        }
        Ok(())
    }

    fn open_rebuild(
        ctx: &Arc<Ctx>,
        options: &CursorOptions,
        rebuild: &ViewRebuild,
    ) -> Result<(QueryCursor, Option<graph::Spec>)> {
        let child_opts = QueryOptions {
            graphs: rebuild.access.clone(),
            deadline: rebuild.deadline,
            cancel: Some(ctx.cancel.clone()),
            cancellation: ctx.cancellation.clone(),
            no_cache: true,
            max_memory_bytes: Some(
                rebuild
                    .max_memory_bytes
                    .map_or(ctx.mem_limit, |limit| limit.min(ctx.mem_limit)),
            ),
            max_rows: Some(ctx.max_rows),
            max_rows_produced: Some(ctx.max_rows_produced),
            work: Some(ctx.rows_produced.clone()),
            union_default_graph: Some(rebuild.union_default_graph),
            grant: ctx.grant().cloned(),
            pool: None,
            allow_service: false,
            forbid_service: true,
            defaults_applied: true,
            no_plan_patterns: true,
            aggregate_errors: Some(rebuild.aggregate_errors),
            rdfs: rebuild.inference.as_ref().and_then(|i| i.rdfs.clone()),
            default_graph_extra: rebuild
                .inference
                .as_ref()
                .map(|i| i.default_graph_extra.clone())
                .unwrap_or_default(),
            ..Default::default()
        };
        let child_cursor = CursorOptions {
            batch_rows: options.batch_rows,
            batch_bytes: options.batch_bytes,
            fallback: options.fallback.at_pull(ctx),
        };
        let inherited = crate::sparql::ViewExecution {
            preparation: ctx.source_preparation.clone().or_else(|| {
                ctx.read_source
                    .as_ref()
                    .map(|source| source.query_input().2.clone())
            }),
            spill: ctx.spill.clone(),
            patterns: rebuild.patterns.clone(),
        };
        super::call_parsed_cursor(
            rebuild.snap.clone(),
            &rebuild.query,
            &child_opts,
            &child_cursor,
            ctx.memory_parent()?,
            &inherited,
        )
    }

    fn push_pending(&mut self, ctx: &Arc<Ctx>, b: &mut Buffer, cap: usize) -> Result<()> {
        while self.pending_rem > 0 && b.table.len < cap {
            ctx.check()?;
            let chunk = self.pending_rem.min((cap - b.table.len) as u64).min(256);
            for _ in 0..chunk {
                b.table.push_row(&self.pending);
            }
            self.pending_rem -= chunk;
        }
        Ok(())
    }

    fn emit_row_copies(
        &mut self,
        ctx: &Arc<Ctx>,
        b: &mut Buffer,
        cap: usize,
        weight: u64,
    ) -> Result<()> {
        let room = (cap - b.table.len) as u64;
        let emit = weight.min(room);
        let mut left = emit;
        while left > 0 {
            ctx.check()?;
            let chunk = left.min(256);
            for _ in 0..chunk {
                b.table.push_row(&self.pending);
            }
            left -= chunk;
        }
        self.pending_rem = weight - emit;
        Ok(())
    }

    fn fill_ids_from_version(
        &mut self,
        ctx: &Ctx,
        version: &ViewVersion,
        row_ids: &[Id],
    ) -> Result<()> {
        for ((dst, v), col) in self.pending.iter_mut().zip(&self.node.vars).zip(&self.cols) {
            *dst = match col {
                Some(i) => translate_id(ctx, version, row_ids[*i])?,
                None if Some(*v) == self.commit_var => self.commit_id,
                None => Id::UNDEF,
            };
        }
        Ok(())
    }

    fn fill_ids_from_terms(&mut self, ctx: &Ctx, terms: &[Option<Term>]) {
        for ((dst, v), col) in self.pending.iter_mut().zip(&self.node.vars).zip(&self.cols) {
            *dst = match col {
                Some(i) => terms[*i].as_ref().map_or(Id::UNDEF, |t| ctx.intern_term(t)),
                None if Some(*v) == self.commit_var => self.commit_id,
                None => Id::UNDEF,
            };
        }
    }

    fn batch_capacity(&self, cap: usize) -> usize {
        let cap = cap.max(1);
        match &self.source {
            Some(Source::Version { remaining_rows, .. })
            | Some(Source::Rows { remaining_rows, .. }) => {
                let rem = remaining_rows.saturating_add(self.pending_rem);
                usize::try_from(rem).map_or(cap, |r| cap.min(r.max(1)))
            }
            None if self.pending_rem > 0 => {
                usize::try_from(self.pending_rem).map_or(cap, |r| cap.min(r.max(1)))
            }
            _ => cap,
        }
    }

    fn pull(
        &mut self,
        ctx: &Arc<Ctx>,
        options: &CursorOptions,
        cap: usize,
    ) -> Result<Option<Buffer>> {
        if !self.opened {
            self.opened = true;
            self.open(ctx, options)?;
        }
        let cap = cap.max(1);
        loop {
            if self.source.is_none() && self.pending_rem == 0 {
                return Ok(None);
            }
            ctx.check()?;
            let init_cap = self.batch_capacity(cap);
            let mut b = Buffer::new(ctx, &self.node.vars, init_cap)?;
            self.push_pending(ctx, &mut b, cap)?;

            let mut ended = self.source.is_none();
            if b.table.len < cap
                && let Some(mut source) = self.source.take()
            {
                let res = self.fill_from_source(ctx, &mut source, &mut b, cap);
                match res {
                    Ok(source_ended) => {
                        ended = source_ended;
                        if source_ended {
                            self.finished_plan = source.rebuild_plan().cloned();
                        } else {
                            self.source = Some(source);
                        }
                    }
                    Err(e) => {
                        self.source = Some(source);
                        return Err(e);
                    }
                }
            }

            if ended && self.pending_rem == 0 {
                self.release();
            }
            b.reconcile()?;
            if b.table.len > 0 {
                // Eager `viewsvc::run` counts its rows once itself and once more as the
                // executor's node, which the cursor counts above; count once here so
                // `work` matches eager execution.
                if !self.rebuilt {
                    ctx.produced(b.table.len)?;
                }
                return Ok(Some(b));
            }
            if self.done() {
                return Ok(None);
            }
        }
    }

    fn fill_from_source(
        &mut self,
        ctx: &Arc<Ctx>,
        source: &mut Source,
        b: &mut Buffer,
        cap: usize,
    ) -> Result<bool> {
        match source {
            Source::Version {
                version,
                at,
                remaining_rows,
            } => {
                while b.table.len < cap && *at < version.table.len() {
                    ctx.check()?;
                    let (row_ids, w) = version.table.row(*at);
                    *at += 1;
                    let w = w.max(0) as u64;
                    if w == 0 {
                        continue;
                    }
                    *remaining_rows = remaining_rows.saturating_sub(w);
                    self.fill_ids_from_version(ctx, version, row_ids)?;
                    ctx.check()?;
                    self.emit_row_copies(ctx, b, cap, w)?;
                }
                Ok(*at >= version.table.len())
            }
            Source::Rewound {
                rewound,
                version_at,
                removed_at,
            } => {
                // Phase 1: stream the shared version, subtracting positive net deltas.
                let version = rewound.version.clone();
                while b.table.len < cap && *version_at < version.table.len() {
                    ctx.check()?;
                    let (row_ids, w) = version.table.row(*version_at);
                    *version_at += 1;
                    let w = w.max(0);
                    if w == 0 {
                        continue;
                    }
                    let emit = if rewound.added.is_empty() {
                        w
                    } else {
                        subtract_added(
                            ctx,
                            rewound,
                            row_ids,
                            w,
                            self.charge.as_mut().expect("rewind admission"),
                        )?
                    };
                    if emit <= 0 {
                        continue;
                    }
                    self.fill_ids_from_version(ctx, &version, row_ids)?;
                    ctx.check()?;
                    self.emit_row_copies(ctx, b, cap, emit as u64)?;
                }
                // Release `added` hash map once Phase 1 finishes so its charge is freed early.
                if (*version_at >= version.table.len() || rewound.added.is_empty())
                    && rewound.added.capacity() > 0
                {
                    rewound.added = Default::default();
                    if let Some(charge) = &mut self.charge {
                        charge.resize(
                            self.scratch_bytes
                                .saturating_add(rewound.removed_retained_bytes()),
                        )?;
                    }
                }
                // Phase 2: stream rows removed since `rewound.commit`.
                while b.table.len < cap && *removed_at < rewound.removed.len() {
                    ctx.check()?;
                    let (terms, w) = &rewound.removed[*removed_at];
                    *removed_at += 1;
                    let w = (*w).max(0) as u64;
                    if w == 0 {
                        continue;
                    }
                    self.fill_ids_from_terms(ctx, terms);
                    ctx.check()?;
                    self.emit_row_copies(ctx, b, cap, w)?;
                }
                Ok(*version_at >= rewound.version.table.len()
                    && *removed_at >= rewound.removed.len())
            }
            Source::Rebuild {
                cursor,
                batch,
                paused,
            } => loop {
                if b.table.len >= cap {
                    return Ok(false);
                }
                ctx.check()?;
                if batch.as_ref().is_none_or(|(rows, at)| *at >= rows.len()) {
                    *batch = None;
                    ctx.check()?;
                    cursor.deadline_pause().store(
                        ctx.deadline_pause
                            .load(Ordering::Relaxed)
                            .saturating_sub(*paused),
                        Ordering::Relaxed,
                    );
                    match cursor.next_batch_at_most(cap - b.table.len)? {
                        Some(next) => *batch = Some((next, 0)),
                        None => return Ok(true),
                    }
                    ctx.check()?;
                }
                let (rows, at) = batch.as_mut().expect("child batch");
                while *at < rows.len() && b.table.len < cap {
                    ctx.check()?;
                    let row = rows.row(*at)?;
                    self.fill_ids_from_terms(ctx, row.values());
                    ctx.check()?;
                    b.table.push_row(&self.pending);
                    *at += 1;
                }
            },
            Source::Graph { cursor, paused } => {
                cursor.deadline_pause().store(
                    ctx.deadline_pause
                        .load(Ordering::Relaxed)
                        .saturating_sub(*paused),
                    Ordering::Relaxed,
                );
                let Some(batch) = cursor.next_at_most(cap - b.table.len)? else {
                    return Ok(true);
                };
                for quad in batch.quads() {
                    ctx.check()?;
                    let _term =
                        ctx.charge(crate::sparql::graph_quad_bytes(quad).saturating_mul(2))?;
                    let terms = [
                        Some(quad.subject.clone().into()),
                        Some(quad.predicate.clone().into()),
                        Some(quad.object.clone()),
                    ];
                    self.fill_ids_from_terms(ctx, &terms);
                    ctx.check()?;
                    b.table.push_row(&self.pending);
                }
                Ok(false)
            }
            Source::Rows {
                rows,
                at,
                remaining_rows,
            } => {
                while b.table.len < cap && *at < rows.rows.len() {
                    ctx.check()?;
                    let (terms, w) = &rows.rows[*at];
                    *at += 1;
                    let w = (*w).max(0) as u64;
                    if w == 0 {
                        continue;
                    }
                    *remaining_rows = remaining_rows.saturating_sub(w);
                    self.fill_ids_from_terms(ctx, terms);
                    ctx.check()?;
                    self.emit_row_copies(ctx, b, cap, w)?;
                }
                Ok(*at >= rows.rows.len())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::access::Preparation;
    use crate::error::Error;
    use crate::sparql::{AggregateErrors, parse_query};
    use crate::store::{Store, StoreOptions};
    use crate::task::{Cancellation, Control};
    use std::sync::atomic::AtomicBool;
    use std::time::{Duration, Instant};

    fn rebuild(s: &Store, query: &str) -> ViewRebuild {
        ViewRebuild {
            columns: Default::default(),
            query: Arc::new(parse_query(query, None, &[]).unwrap()),
            access: None,
            inference: None,
            deadline: None,
            max_memory_bytes: None,
            patterns: None,
            union_default_graph: false,
            template_columns: None,
            aggregate_errors: AggregateErrors::Sparkles,
            commit: 0,
            snap: s.snapshot(),
            evaluated_under: None,
        }
    }

    #[test]
    fn rebuilt_children_poll_original_controls_without_a_parent_pull() {
        let s = Store::in_memory(StoreOptions::default());
        let source = Arc::new(AtomicBool::new(false));
        let additional = Arc::new(AtomicBool::new(false));
        let mut ctx = Ctx::new(s.snapshot());
        ctx.source_preparation =
            Some(Preparation::new(Control::with_cancel(source.clone()), 16 << 20).unwrap());
        ctx.cancellation = Some(Cancellation::from_flags([additional.clone()]));
        let ctx = ctx.into_shared();
        let select = rebuild(&s, "SELECT ?x { VALUES ?x { 1 } }");
        let (mut child, _) = View::open_rebuild(&ctx, &CursorOptions::default(), &select).unwrap();
        source.store(true, Ordering::Relaxed);
        assert!(matches!(child.next_batch(), Err(Error::Cancelled)));
        source.store(false, Ordering::Relaxed);

        let construct = rebuild(
            &s,
            "CONSTRUCT { <urn:s> <urn:p> ?x } WHERE { VALUES ?x { 1 } }",
        );
        let (child, spec) =
            View::open_rebuild(&ctx, &CursorOptions::default(), &construct).unwrap();
        let mut graph = graph::GraphCursor::for_view(child, spec.unwrap(), None).unwrap();
        additional.store(true, Ordering::Relaxed);
        assert!(matches!(graph.next_batch(), Err(Error::Cancelled)));
    }

    #[test]
    fn rebuild_caps_rows_and_memory_and_shares_the_spill_account() {
        use crate::spill::{Spill, SpillOptions};

        let s = Store::in_memory(StoreOptions::default());
        let dir = tempfile::tempdir().unwrap();
        let spill = Spill::open(&SpillOptions {
            dir: dir.path().to_path_buf(),
            max_bytes: 8 << 20,
            query_bytes: 4 << 20,
            min_free_bytes: 0,
            encrypted: false,
        })
        .unwrap();
        let mut ctx = Ctx::new(s.snapshot());
        ctx.spill = Some(Arc::new(spill.query(Some(1 << 20))));
        ctx.max_rows = 71;
        ctx.mem_limit = 8 << 20;
        let ctx = ctx.into_shared();
        let mut query = rebuild(&s, "SELECT ?x { VALUES ?x { 1 } }");
        query.max_memory_bytes = Some(3 << 20);
        let (child, _) = View::open_rebuild(&ctx, &CursorOptions::default(), &query).unwrap();
        assert_eq!(child.ctx.max_rows, 71);
        assert_eq!(child.ctx.mem_limit, 3 << 20);
        assert!(Arc::ptr_eq(
            child.ctx.spill.as_ref().unwrap(),
            ctx.spill.as_ref().unwrap()
        ));
    }

    #[test]
    fn consumer_pause_cannot_extend_an_absolute_source_deadline_in_a_rebuild() {
        let s = Store::in_memory(StoreOptions::default());
        let mut ctx = Ctx::new(s.snapshot());
        let end = Instant::now() + Duration::from_millis(200);
        ctx.source_preparation = Some(
            Preparation::new(
                Control {
                    deadline: Some(end),
                    ..Control::none()
                },
                16 << 20,
            )
            .unwrap(),
        );
        let ctx = ctx.into_shared();
        let mut select = rebuild(&s, "SELECT ?x { VALUES ?x { 1 } }");
        select.deadline = Some(end);
        let (mut child, _) = View::open_rebuild(&ctx, &CursorOptions::default(), &select).unwrap();
        child
            .deadline_pause()
            .store(10_000_000_000, Ordering::Relaxed);
        std::thread::sleep(end.saturating_duration_since(Instant::now()));
        assert!(matches!(child.next_batch(), Err(Error::Timeout)));
    }
}
