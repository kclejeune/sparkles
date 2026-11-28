//! A SERVICE leaf (`silent: false` and `silent: true`), read from its endpoint under the
//! query's memory budget (item 829).
//!
//! For a non-`SILENT` SERVICE, a remote endpoint's response is parsed as it arrives, so
//! the operator holds one output batch and the parser's buffer between pulls, never the
//! whole answer. A query endpoint of this process is opened as a cursor of its own under
//! the same policy as the caller, and the operator pulls one of its batches per batch. A
//! cached answer is read from the cache entry, which the operator holds under a charge
//! while it reads it, since the cache may evict the entry meanwhile.
//!
//! For a `SILENT` SERVICE, SPARQL 1.1 Federation requires replacing any remote or local
//! endpoint error — including a mid-stream truncation or parse failure after rows have
//! arrived — with a single empty solution. The operator therefore drains remote and local
//! sources into a memory-charged buffer before emitting its first row (reporting
//! `fullInputBeforeOutput`), drops any partial buffer and cache copy on endpoint failure,
//! and emits one empty binding row. Local control errors (`Cancelled`, `Timeout`,
//! `NotPermitted`, `BudgetExceeded`, and strict cursor refusals from an in-process
//! callee) are never suppressed. A cache hit is already complete and streams directly.
//!
//! Nothing happens before the first pull, because operators are also built only to see
//! whether an EXISTS pattern streams. The request, the policy checks and the endpoint's
//! errors all come at the first pull, as they come when an eager run reaches the node.
//!
//! The endpoint's blank nodes are its own. One map for the whole answer gives each label
//! one new blank node of this query, across batches, as one eager call does.
//!
//! Three things differ from an eager run for non-`SILENT` SERVICE. A failure after rows
//! were emitted fails the cursor, as any streamed failure does. A response is cached only
//! when it has been read to its end, so a consumer that stops early leaves nothing in the
//! cache, and a copy that grows past the largest entry the cache keeps, or past what the
//! query's budget holds beside the answer, is dropped as it grows. And the outbound
//! timeout counts from the start of the request, while the query's deadline leaves out
//! the time a consumer keeps the cursor waiting, so a slow consumer can expire a remote
//! transfer that an eager run would finish.
//!
//! For non-`SILENT` SERVICE, the parser buffers a JSON response whose results come before
//! its head's variables, so the strict policy refuses one by name and a permissive cursor
//! reads it under a charge for the buffered answer (see `svcread`). For `SILENT` SERVICE,
//! the whole answer is already buffered under a charge, so late-head JSON is accepted
//! under full byte metering.
use super::{Buffer, CursorOptions, FallbackPolicy, QueryCursor, copy_rows, merge};
use crate::error::{Error, Result};
use crate::id::Id;
use crate::sparql::ctx::{Ctx, OwnedCharge};
use crate::sparql::enhancer::CacheMode;
use crate::sparql::exec::{LocalCall, RemoteAnswer, ServiceSolution};
use crate::sparql::plan::{Kind, Node, PathEnd};
use crate::sparql::svccache::{Parts, Rows, ServiceKey, Slice};
use crate::sparql::table::VarId;
use rustc_hash::FxHashMap;
use std::sync::Arc;
use std::sync::atomic::Ordering;

/// Whether a plan node runs as a native SERVICE cursor (both `silent: false` and
/// `silent: true`, reading no plan input).
pub(super) fn eligible(node: &Node) -> bool {
    matches!(node.kind, Kind::Service { .. }) && node.children.is_empty()
}

/// Fixed runtime plan explanation when a permissive SERVICE reader discovers a JSON
/// response whose results precede its head variables.
pub(super) const BUFFERED_REASON: &str =
    "SERVICE JSON results precede head variables and are buffered before output";

/// Plan explanation for a `SILENT` SERVICE cursor, which buffers its answer under the
/// memory budget before emitting rows so mid-stream endpoint errors become one empty
/// solution.
pub(super) const SILENT_REASON: &str = "Service (SILENT) buffers its answer under the memory budget before output so an endpoint error becomes one empty solution";

/// Largest chunk pulled per step while draining a `SILENT` SERVICE into its buffer.
const SILENT_CHUNK_MAX: usize = 4096;

/// Errors that `SILENT` SERVICE must propagate rather than suppress into an empty row,
/// matching [`crate::sparql::ctx::Ctx`] sticky refusal classification plus `NotPermitted`.
fn is_non_suppressible(err: &Error, strict: bool) -> bool {
    matches!(
        err,
        Error::Cancelled | Error::Timeout | Error::NotPermitted(_) | Error::BudgetExceeded(_)
    ) || matches!(err, Error::Unsupported(m) if m.starts_with(super::REFUSAL))
        || matches!(err, Error::Service(m) if strict && m.starts_with(super::LATE_HEAD))
        || crate::sparql::extensions::is_unconfigured(err)
        || matches!(
            err,
            Error::Component(component)
                if component
                    .source
                    .as_ref()
                    .and_then(|source| source.downcast_ref::<sparkles_budget::ParentRefused>())
                    .is_some()
        )
}

pub(super) struct Service {
    endpoint: PathEnd,
    query: String,
    silent: bool,
    cache: CacheMode,
    /// what the cached request keys its entry by (see [`ServiceKey`])
    key: Option<ServiceKey>,
    vars: Vec<VarId>,
    /// the names of `vars`, as the endpoint's solutions name their variables
    names: Vec<String>,
    /// the policy an in-process call runs under
    fallback: FallbackPolicy,
    opened: bool,
    /// whether the opened remote answer had to buffer its solution sequence before
    /// yielding its first row; retained after the source is released
    buffered_before_output: bool,
    source: Option<Source>,
    /// Charged answer buffered for `SILENT` SERVICE (either the complete drained rows or
    /// the single empty solution produced when a suppressible endpoint error occurred).
    buffered: Option<Buffer>,
    at: usize,
    /// the blank node of this query for each label of the endpoint's answer
    bnodes: FxHashMap<String, Id>,
}

enum Source {
    Remote {
        answer: Box<RemoteAnswer>,
        /// the copy of the answer read so far, for the cache once it has all of it
        tee: Option<Tee>,
        /// what the answer gave after the solutions a LIMIT asks for, read ahead to
        /// learn whether the response ended cleanly there: `Some(None)` for its end
        peeked: Option<Option<Result<sparesults::QuerySolution>>>,
    },
    Local {
        /// Declared before `hold`, so that the call ends before its hold is released.
        cursor: Box<QueryCursor>,
        /// the callee's batch being read, and the next row of it
        batch: Option<(super::QueryBatch, usize)>,
        /// the caller's pauses when the call opened, which its deadline counts
        paused: u64,
        _hold: Option<Box<dyn std::any::Any + Send + Sync>>,
    },
    Cached {
        rows: Arc<Rows>,
        /// the column of `rows` of each of the operator's variables
        cols: Vec<Option<usize>>,
        at: usize,
        /// The entry's bytes, which the query holds for as long as it reads the entry.
        _charge: OwnedCharge,
    },
}

/// A charged copy of a remote answer, kept while it is small enough to be cached.
struct Tee {
    key: String,
    /// the parts of `key`, and the slice of the request it copies
    parts: Parts,
    slice: Slice,
    rows: Rows,
    /// `rows.bytes()`, the cache's weight of the copy, kept as rows arrive
    bytes: u64,
    /// the largest entry the cache keeps, with its key
    limit: u64,
    /// what the copy's rows and names keep allocated, by `Rows::retained_row_bytes`
    retained: u64,
    charge: OwnedCharge,
}

impl Tee {
    fn new(ctx: &Arc<Ctx>, key: String, limit: u64) -> Result<Self> {
        let rows = Rows {
            vars: Vec::new(),
            rows: Vec::new(),
        };
        let retained = rows.retained_bytes();
        let charge = OwnedCharge::new(ctx, retained.saturating_add(key.capacity() as u64))?;
        Ok(Self {
            key,
            parts: Parts::default(),
            slice: Slice::ALL,
            rows,
            bytes: 64,
            limit,
            retained,
            charge,
        })
    }

    /// The copy of the request whose key has `parts`, sent with `slice`.
    fn of(mut self, parts: Parts, slice: Slice) -> Self {
        self.parts = parts;
        self.slice = slice;
        self
    }

    /// Copy one solution, or give up the copy once it is larger than the cache keeps or
    /// the budget can hold. What the copy will hold, names and row included, is sized
    /// from the borrowed solution and charged before anything is copied.
    fn push(mut self, solution: &sparesults::QuerySolution) -> Result<Option<Self>> {
        // the variables of the first solution that binds any, as `fetch_rows` takes them
        if self.rows.vars.is_empty() && !solution.variables().is_empty() {
            let names = solution.variables();
            self.bytes += names
                .iter()
                .map(|v| v.as_str().len() as u64 + 24)
                .sum::<u64>();
            if self.over_limit() {
                return Ok(None);
            }
            let kept = names.len() * std::mem::size_of::<String>()
                + names.iter().map(|v| v.as_str().len()).sum::<usize>();
            self.retained += kept as u64;
            if !self.charge_for(self.rows.rows.capacity()) {
                return Ok(None);
            }
            #[cfg(test)]
            COPIES.with(|c| c.set(c.get() + 1));
            let mut vars = Vec::with_capacity(names.len());
            vars.extend(names.iter().map(|v| v.as_str().to_owned()));
            self.rows.vars = vars;
        }
        // the names are moved out while the row is sized and charged; a dropped copy
        // never puts them back
        let vars = std::mem::take(&mut self.rows.vars);
        let terms = || vars.iter().map(|v| solution.get(v.as_str()));
        self.bytes += Rows::row_bytes_of(terms());
        if self.over_limit() {
            return Ok(None);
        }
        let width = vars.len();
        let row_bytes = Rows::retained_slots_bytes(width, terms().flatten());
        self.retained += row_bytes;
        let (len, capacity) = (self.rows.rows.len(), self.rows.rows.capacity());
        let grown = if len == capacity {
            (capacity * 2).max(4)
        } else {
            capacity
        };
        // The cache is an optimisation: a copy the budget cannot hold is dropped, and
        // the answer streams on uncached, as it does without the cache.
        if !self.charge_for(grown) {
            return Ok(None);
        }
        #[cfg(test)]
        COPIES.with(|c| c.set(c.get() + 1));
        let mut row = Vec::with_capacity(width);
        row.extend(terms().map(|t| t.cloned()));
        debug_assert_eq!(Rows::retained_row_bytes(&row), row_bytes);
        self.rows.vars = vars;
        self.rows.rows.reserve_exact(grown - len);
        self.rows.rows.push(row);
        Ok(Some(self))
    }

    /// Whether the copy outgrew the largest entry the cache keeps.
    fn over_limit(&self) -> bool {
        self.bytes.saturating_add(self.key.len() as u64) > self.limit
    }

    /// Charge what the copy keeps with its rows vector at `slots` capacity, or say that
    /// the budget cannot hold it.
    fn charge_for(&mut self, slots: usize) -> bool {
        let slot = std::mem::size_of::<Vec<Option<oxrdf::Term>>>();
        self.charge
            .resize(
                self.retained
                    .saturating_add((slots * slot) as u64)
                    .saturating_add(self.key.capacity() as u64),
            )
            .is_ok()
    }
}

#[cfg(test)]
thread_local! {
    /// names and rows the tee copied on this thread (for tests of admission before copying)
    static COPIES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

impl Service {
    pub(super) fn new(ctx: &Ctx, node: &Node, fallback: FallbackPolicy) -> Self {
        let Kind::Service {
            endpoint,
            query,
            silent,
            cache,
            key,
        } = &node.kind
        else {
            unreachable!("a SERVICE node")
        };
        Self {
            endpoint: endpoint.clone(),
            query: query.clone(),
            silent: *silent,
            cache: *cache,
            key: key.as_deref().cloned(),
            vars: node.vars.clone(),
            names: node.vars.iter().map(|v| ctx.var_name(*v)).collect(),
            fallback,
            opened: false,
            buffered_before_output: false,
            source: None,
            buffered: None,
            at: 0,
            bnodes: FxHashMap::default(),
        }
    }

    /// Whether this reader buffers its full answer before emitting its first row
    /// (`SILENT` SERVICE, or a permissive non-`SILENT` reader whose remote JSON response
    /// placed results before head variables).
    pub(super) fn full_input_before_output(&self) -> bool {
        self.silent || self.buffered_before_output
    }

    /// Close the answer, its response, buffered rows, copy, entry or in-process call,
    /// once nothing reads it any more.
    pub(super) fn release(&mut self) {
        self.opened = true;
        self.source = None;
        self.buffered = None;
        self.at = 0;
        self.bnodes.clear();
    }

    /// Whether the answer has been read to its end.
    pub(super) fn done(&self) -> bool {
        self.opened && self.source.is_none() && self.buffered.is_none()
    }

    /// Send the request, or open the in-process call or the cache entry.
    fn open(&mut self, ctx: &Arc<Ctx>, options: &CursorOptions) -> Result<()> {
        crate::sparql::enhancer::check_allowed(ctx)?;
        let PathEnd::Const(id) = &self.endpoint else {
            return Err(Error::unsupported("SERVICE with a variable endpoint"));
        };
        let Some(oxrdf::Term::NamedNode(url)) = ctx.term(*id) else {
            return Err(Error::Service("invalid SERVICE endpoint".into()));
        };
        if let Some(target) = ctx
            .local_endpoints
            .as_ref()
            .and_then(|l| l.resolve(url.as_str()))
        {
            // The callee's deadline is the caller's as it stands now, and each pull gives
            // it the pauses its caller's consumer has taken since, so the call ends when
            // its caller does.
            let paused = ctx.deadline_pause.load(Ordering::Relaxed);
            let call = LocalCall::new(ctx, target, ctx.current_deadline())?;
            ctx.check()?;
            let cursor = CursorOptions {
                fallback: self.fallback.at_pull(ctx),
                ..options.clone()
            };
            // the callee's memory counts against this query's too
            let parent = ctx.memory_parent()?;
            let cursor = super::call_cursor(call.snap, &self.query, &call.options, &cursor, parent)
                .map(Box::new)
                .map_err(|e| match e {
                    Error::Unsupported(m) if m.starts_with("a Select cursor requires") => {
                        Error::Service(format!(
                            "SERVICE <{}> did not answer with solutions",
                            url.as_str()
                        ))
                    }
                    e => e,
                })?;
            self.source = Some(Source::Local {
                cursor,
                batch: None,
                paused,
                _hold: call.hold,
            });
            return Ok(());
        }
        let store = &ctx.snap.results.service;
        let mut tee = None;
        // an in-process call reads the current data, so its answer is never cached
        if matches!(self.cache, CacheMode::Default | CacheMode::Clear)
            && ctx.use_cache
            && store.enabled()
        {
            // the whole result under one key: a SERVICE without `loop` has one input, and
            // the key leaves out the request's outermost slice, which bounds the entry
            let (text, slice) = self
                .key
                .as_ref()
                .map_or((self.query.as_str(), Slice::ALL), |k| {
                    (k.text.as_str(), k.slice)
                });
            let key = crate::sparql::svccache::key(&ctx.service_scope, url.as_str(), text, &[]);
            let parts = Parts::of(&ctx.service_scope, url.as_str(), text);
            let lookup = ctx.charge(0)?;
            let hit = if self.cache == CacheMode::Default {
                store.get_admitted(&key, slice, &|n| lookup.add(n))?
            } else {
                store.remove(&key);
                None
            };
            if let Some(rows) = hit {
                let charge = OwnedCharge::new(ctx, rows.retained_bytes())?;
                drop(lookup);
                let cols = self
                    .names
                    .iter()
                    .map(|n| rows.vars.iter().position(|x| x == n))
                    .collect();
                self.source = Some(Source::Cached {
                    rows,
                    cols,
                    at: 0,
                    _charge: charge,
                });
                return Ok(());
            }
            // a copy the budget cannot hold is not made
            tee = Tee::new(ctx, key, store.max_entry_bytes())
                .ok()
                .map(|t| t.of(parts, slice));
        }
        let meter = crate::sparql::exec::Meter {
            ctx,
            strict: !self.silent
                && self.fallback.at_pull(ctx) == FallbackPolicy::RejectMaterialization,
        };
        let answer = RemoteAnswer::open(ctx, &url, &self.query, Some(meter))?;
        self.buffered_before_output = answer.buffered_before_output();
        self.source = Some(Source::Remote {
            answer: Box::new(answer),
            tee,
            peeked: None,
        });
        Ok(())
    }

    /// Drain a remote or local source for `SILENT` SERVICE into a charged buffer before
    /// emitting any row, so a mid-stream endpoint error can be replaced cleanly with one
    /// empty binding row.
    fn drain_silent(&mut self, ctx: &Arc<Ctx>) -> Result<Buffer> {
        let mut all = Buffer::new(ctx, &self.vars, 0)?;
        let mut chunk = 64usize;
        while self.source.is_some() {
            ctx.check()?;
            let cap = chunk
                .min(SILENT_CHUNK_MAX)
                .min(ctx.rows_within_budget(self.vars.len()).max(1));
            if let Some(batch) = self.fill(ctx, cap, Some(all.table.len))? {
                merge::append(ctx, &mut all, &batch.table, 0..batch.table.len)?;
                ctx.check_output_rows(all.table.len)?;
            }
            chunk = chunk.saturating_mul(2).min(SILENT_CHUNK_MAX);
        }
        Ok(all)
    }

    /// Handle an error during `SILENT` SERVICE open or draining: release partial state,
    /// propagate local control/policy/budget/strict-refusal errors, and suppress endpoint
    /// failures into a single empty solution row.
    fn handle_silent_error(&mut self, ctx: &Arc<Ctx>, err: Error) -> Result<()> {
        self.source = None;
        self.buffered = None;
        self.at = 0;
        self.bnodes.clear();
        ctx.check()?;
        let strict = self.fallback.at_pull(ctx) == FallbackPolicy::RejectMaterialization;
        if is_non_suppressible(&err, strict) {
            return Err(err);
        }
        ctx.check_output_rows(1)?;
        let mut unit = Buffer::new(ctx, &self.vars, 1)?;
        let row = vec![Id::UNDEF; self.vars.len()];
        unit.table.push_row(&row);
        unit.reconcile()?;
        self.buffered = Some(unit);
        self.at = 0;
        Ok(())
    }

    /// The next batch of at most `cap` rows, or `None` once the answer has ended.
    pub(super) fn next(
        &mut self,
        ctx: &Arc<Ctx>,
        options: &CursorOptions,
        cap: usize,
    ) -> Result<Option<Buffer>> {
        if !self.opened {
            self.opened = true;
            if self.silent {
                match self.open(ctx, options) {
                    Ok(()) => {
                        if !matches!(self.source, Some(Source::Cached { .. })) {
                            match self.drain_silent(ctx) {
                                Ok(buf) => {
                                    self.source = None;
                                    self.bnodes.clear();
                                    if !buf.table.is_empty() {
                                        self.buffered = Some(buf);
                                        self.at = 0;
                                    }
                                }
                                Err(err) => self.handle_silent_error(ctx, err)?,
                            }
                        }
                    }
                    Err(err) => self.handle_silent_error(ctx, err)?,
                }
            } else {
                self.open(ctx, options)?;
            }
        }
        if let Some(buf) = &self.buffered {
            if let Err(err) = ctx.check() {
                self.release();
                return Err(err);
            }
            if self.at == 0 && buf.table.len <= cap {
                return Ok(self.buffered.take());
            }
            let batch = match copy_rows(ctx, &buf.table, self.at, cap) {
                Ok(batch) => batch,
                Err(err) => {
                    self.release();
                    return Err(err);
                }
            };
            self.at += batch.as_ref().map_or(0, |b| b.table.len);
            if self.at >= buf.table.len {
                self.buffered = None;
                self.at = 0;
            }
            return Ok(batch);
        }
        if self.source.is_none() {
            return Ok(None);
        }
        let result = self.fill(ctx, cap, None);
        if result.is_err() {
            // a failed answer is closed, and its body and its copy released, now
            self.source = None;
        }
        result
    }

    fn fill(
        &mut self,
        ctx: &Arc<Ctx>,
        cap: usize,
        silent_prior: Option<usize>,
    ) -> Result<Option<Buffer>> {
        let mut b = Buffer::new(ctx, &self.vars, cap)?;
        let mut row = vec![Id::UNDEF; self.vars.len()];
        let (names, bnodes) = (&self.names, &mut self.bnodes);
        let mut push =
            |b: &mut Buffer, bnodes: &mut FxHashMap<String, Id>, solution: &ServiceSolution<'_>| {
                for (to, name) in row.iter_mut().zip(names) {
                    *to = solution
                        .get(name)
                        .map_or(Id::UNDEF, |term| ctx.intern_remote_term(term, bnodes));
                }
                b.table.push_row(&row);
            };
        let ended = match self.source.as_mut().expect("an open answer") {
            Source::Remote {
                answer,
                tee,
                peeked,
            } => {
                let _entered = answer.span.clone().entered();
                loop {
                    if b.table.len >= cap {
                        break false;
                    }
                    ctx.check()?;
                    let next = match peeked.take() {
                        Some(next) => next,
                        None => answer.next_solution(),
                    };
                    let Some(solution) = next else {
                        ctx.check()?;
                        // a response with more solutions than its LIMIT is not the
                        // answer the request asked for
                        if let Some(tee) = tee.take()
                            && tee.slice.limit.is_none_or(|l| tee.rows.rows.len() <= l)
                        {
                            ctx.snap.results.service.put(
                                tee.key,
                                tee.parts,
                                tee.slice,
                                Arc::new(tee.rows),
                            );
                        }
                        break true;
                    };
                    let solution = solution?;
                    ctx.check()?;
                    if let Some(prior) = silent_prior {
                        ctx.check_output_rows(
                            prior
                                .checked_add(b.table.len)
                                .and_then(|n| n.checked_add(1))
                                .unwrap_or(usize::MAX),
                        )?;
                    }
                    if let Some(copy) = tee.take() {
                        *tee = copy.push(&solution)?;
                    }
                    // A SILENT call reads its whole answer before returning any row and
                    // caches it at the end. A non-SILENT call that reached the LIMIT
                    // drops the copy without blocking on the response tail, so the
                    // query returns promptly even when the endpoint stalls after the
                    // last demanded row. The response is cached only when EOF has
                    // already been observed (the loop breaks naturally).
                    if silent_prior.is_none()
                        && tee
                            .as_ref()
                            .is_some_and(|t| t.slice.limit == Some(t.rows.rows.len()))
                    {
                        *tee = None;
                    }
                    push(&mut b, bnodes, &ServiceSolution::Remote(&solution));
                    ctx.check()?;
                }
            }
            Source::Local {
                cursor,
                batch,
                paused,
                ..
            } => loop {
                if b.table.len >= cap {
                    break false;
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
                        None => break true,
                    }
                    ctx.check()?;
                }
                let (rows, at) = batch.as_mut().expect("a callee batch");
                let names = rows.variables();
                while *at < rows.len() && b.table.len < cap {
                    ctx.check()?;
                    if let Some(prior) = silent_prior {
                        ctx.check_output_rows(
                            prior
                                .checked_add(b.table.len)
                                .and_then(|n| n.checked_add(1))
                                .unwrap_or(usize::MAX),
                        )?;
                    }
                    let row = rows.row(*at)?;
                    push(&mut b, bnodes, &ServiceSolution::Local { names, row: &row });
                    ctx.check()?;
                    *at += 1;
                }
            },
            Source::Cached { rows, cols, at, .. } => {
                ctx.check()?;
                let end = rows.rows.len().min(*at + cap);
                let mut ids = vec![Id::UNDEF; cols.len()];
                while *at < end {
                    ctx.check()?;
                    let r = &rows.rows[*at];
                    for (to, c) in ids.iter_mut().zip(cols.iter()) {
                        *to = c
                            .and_then(|c| r[c].as_ref())
                            .map_or(Id::UNDEF, |term| ctx.intern_remote_term(term, bnodes));
                    }
                    ctx.check()?;
                    b.table.push_row(&ids);
                    *at += 1;
                }
                *at == rows.rows.len()
            }
        };
        if ended {
            self.source = None;
        }
        b.reconcile()?;
        Ok((b.table.len > 0).then_some(b))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{Store, StoreOptions};
    use oxrdf::{Literal, Term, Variable};

    fn ctx(limit: u64) -> Arc<Ctx> {
        let store = Store::in_memory(StoreOptions::default());
        let mut ctx = Ctx::new(store.snapshot());
        ctx.mem_limit = limit;
        ctx.enable_cursor_accounting().unwrap();
        let ctx = Arc::new(ctx);
        ctx.attach_owner();
        ctx
    }

    /// A wide solution of mostly unbound slots that the budget cannot hold.
    fn wide(width: usize) -> sparesults::QuerySolution {
        let vars: Arc<[Variable]> = (0..width)
            .map(|i| Variable::new(format!("v{i}")).unwrap())
            .collect();
        let mut values = vec![None; width];
        values[0] = Some(Term::Literal(Literal::from(1)));
        (vars, values).into()
    }

    /// The tee charges a copy before it makes it: names and row the budget cannot hold
    /// are not copied at all, not copied and then dropped.
    #[test]
    fn a_copy_is_charged_before_it_is_made() {
        let width = 8_192;
        // the names fit and the row of slots does not
        let names = (width * (std::mem::size_of::<String>() + 6)) as u64;
        let slots = (width * std::mem::size_of::<Option<Term>>()) as u64;
        for (limit, copies) in [(names / 2, 0), (names + slots / 2, 1)] {
            let ctx = ctx(limit);
            let tee = Tee::new(&ctx, "key".into(), u64::MAX).unwrap();
            COPIES.with(|c| c.set(0));
            assert!(tee.push(&wide(width)).unwrap().is_none(), "{limit}");
            assert_eq!(COPIES.with(std::cell::Cell::get), copies, "{limit}");
        }
        // with room for both, both are copied
        let ctx = ctx(16 << 20);
        let tee = Tee::new(&ctx, "key".into(), u64::MAX).unwrap();
        COPIES.with(|c| c.set(0));
        let tee = tee.push(&wide(width)).unwrap().unwrap();
        assert_eq!(COPIES.with(std::cell::Cell::get), 2);
        assert_eq!(tee.rows.rows.len(), 1);
        assert_eq!(tee.rows.vars.len(), width);
    }

    struct ChunkedReader {
        chunks: std::collections::VecDeque<Vec<u8>>,
        armed: Arc<std::sync::atomic::AtomicBool>,
        cancel_on_first_armed_read: Option<Arc<std::sync::atomic::AtomicBool>>,
        triggered: bool,
        reads_after_first_armed: Arc<std::sync::atomic::AtomicUsize>,
    }

    impl std::io::Read for ChunkedReader {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            if self.triggered {
                self.reads_after_first_armed.fetch_add(1, Ordering::SeqCst);
            }
            let Some(mut chunk) = self.chunks.pop_front() else {
                return Ok(0);
            };
            let n = chunk.len().min(buf.len());
            buf[..n].copy_from_slice(&chunk[..n]);
            if n < chunk.len() {
                self.chunks.push_front(chunk.split_off(n));
            } else if self.armed.load(Ordering::SeqCst) && !self.triggered {
                self.triggered = true;
                if let Some(cancel) = &self.cancel_on_first_armed_read {
                    cancel.store(true, Ordering::SeqCst);
                }
            }
            Ok(n)
        }
    }

    fn test_service(ctx: &Ctx, var: VarId, source: Source) -> Service {
        Service {
            endpoint: PathEnd::Const(Id::UNDEF),
            query: "SELECT ?s { ?s ?p ?o }".into(),
            silent: false,
            cache: CacheMode::Default,
            key: None,
            vars: vec![var],
            names: vec![ctx.var_name(var)],
            fallback: FallbackPolicy::RejectMaterialization,
            opened: true,
            buffered_before_output: false,
            source: Some(source),
            buffered: None,
            at: 0,
            bnodes: FxHashMap::default(),
        }
    }

    /// Cancellation raised while completing a remote solution stops `Service::fill`
    /// before reading any later solution in the batch.
    #[test]
    fn remote_fill_checks_cancellation_between_solutions() {
        let ctx = ctx(16 << 20);
        let var = ctx.var("s");
        let mut chunks = std::collections::VecDeque::new();
        chunks.push_back(br#"{"head":{"vars":["s"]},"results":{"bindings":["#.to_vec());
        for i in 0..12usize {
            let prefix = if i == 0 { "" } else { "," };
            chunks.push_back(
                format!(r#"{prefix}{{"s":{{"type":"uri","value":"urn:s:{i}"}}}}"#).into_bytes(),
            );
        }
        chunks.push_back(b"]}}".to_vec());
        let armed = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let reads_after = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let reader = ChunkedReader {
            chunks,
            armed: armed.clone(),
            cancel_on_first_armed_read: Some(ctx.cancel.clone()),
            triggered: false,
            reads_after_first_armed: reads_after.clone(),
        };
        let answer = RemoteAnswer::from_reader(
            Box::new(reader),
            sparesults::QueryResultsFormat::Json,
            "http://test.invalid/sparql",
        )
        .unwrap();
        armed.store(true, Ordering::SeqCst);
        let mut svc = test_service(
            &ctx,
            var,
            Source::Remote {
                answer: Box::new(answer),
                tee: None,
                peeked: None,
            },
        );
        let err = svc.fill(&ctx, 10, None).err();
        assert!(
            matches!(err, Some(Error::Cancelled)),
            "expected Cancelled, got {err:?}"
        );
        assert!(ctx.cancel.load(Ordering::SeqCst));
        assert_eq!(
            reads_after.load(Ordering::SeqCst),
            0,
            "fill read past the cancelled solution boundary"
        );
    }

    /// A sticky memory refusal during query-local term interning stops `Service::fill`
    /// before reading any later solution and never publishes the cache tee.
    #[test]
    fn remote_fill_does_not_cache_after_an_interning_refusal() {
        // Enough credit for Buffer + Tee(sol_0) + Tee(sol_1), but not also interning sol_0's 8 KiB literal.
        let ctx = ctx(12_000);
        let var = ctx.var("s");
        let big = "a".repeat(8_000);
        let mut chunks = std::collections::VecDeque::new();
        chunks.push_back(br#"{"head":{"vars":["s"]},"results":{"bindings":["#.to_vec());
        chunks.push_back(format!(r#"{{"s":{{"type":"literal","value":"{big}"}}}}"#).into_bytes());
        chunks.push_back(br#",{"s":{"type":"literal","value":"b"}}"#.to_vec());
        chunks.push_back(b"]}}".to_vec());
        let armed = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let reads_after = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let reader = ChunkedReader {
            chunks,
            armed: armed.clone(),
            cancel_on_first_armed_read: None,
            triggered: false,
            reads_after_first_armed: reads_after.clone(),
        };
        let answer = RemoteAnswer::from_reader(
            Box::new(reader),
            sparesults::QueryResultsFormat::Json,
            "http://test.invalid/sparql",
        )
        .unwrap();
        let tee = Tee::new(
            &ctx,
            "test-interning-refusal".into(),
            ctx.snap.results.service.max_entry_bytes(),
        )
        .unwrap();
        armed.store(true, Ordering::SeqCst);
        let mut svc = test_service(
            &ctx,
            var,
            Source::Remote {
                answer: Box::new(answer),
                tee: Some(tee),
                peeked: None,
            },
        );
        let err = svc.fill(&ctx, 4, None).err();
        assert!(
            matches!(
                err,
                Some(Error::BudgetExceeded(b)) if b.kind == crate::BudgetKind::Memory
            ),
            "expected Memory BudgetExceeded, got {err:?}"
        );
        assert_eq!(
            reads_after.load(Ordering::SeqCst),
            0,
            "fill read another solution after interning failed"
        );
        assert_eq!(
            ctx.snap.results.service.entries(),
            0,
            "a failed fill must not publish its cache tee"
        );
    }

    /// `Source::Cached` checks pending cancellation, expired deadline, and sticky memory
    /// refusal before advancing `at`.
    #[test]
    fn cached_fill_checks_pending_stop() {
        let make_rows = || {
            Arc::new(Rows {
                vars: vec!["s".into()],
                rows: (0..4)
                    .map(|i| {
                        vec![Some(Term::NamedNode(
                            oxrdf::NamedNode::new(format!("urn:s:{i}")).unwrap(),
                        ))]
                    })
                    .collect(),
            })
        };
        // 1. Pre-set cancellation
        {
            let ctx = ctx(16 << 20);
            let var = ctx.var("s");
            let rows = make_rows();
            let charge = OwnedCharge::new(&ctx, rows.retained_bytes()).unwrap();
            let mut svc = test_service(
                &ctx,
                var,
                Source::Cached {
                    rows,
                    cols: vec![Some(0)],
                    at: 0,
                    _charge: charge,
                },
            );
            ctx.cancel.store(true, Ordering::Relaxed);
            let err = svc.fill(&ctx, 4, None).err();
            assert!(matches!(err, Some(Error::Cancelled)), "{err:?}");
            assert!(matches!(svc.source, Some(Source::Cached { at: 0, .. })));
        }
        // 2. Expired deadline
        {
            let store = Store::in_memory(StoreOptions::default());
            let mut raw = Ctx::new(store.snapshot());
            raw.mem_limit = 16 << 20;
            raw.deadline = Some(std::time::Instant::now() - std::time::Duration::from_secs(1));
            raw.enable_cursor_accounting().unwrap();
            let ctx = Arc::new(raw);
            ctx.attach_owner();
            let var = ctx.var("s");
            let rows = make_rows();
            let charge = OwnedCharge::new(&ctx, rows.retained_bytes()).unwrap();
            let mut svc = test_service(
                &ctx,
                var,
                Source::Cached {
                    rows,
                    cols: vec![Some(0)],
                    at: 0,
                    _charge: charge,
                },
            );
            let err = svc.fill(&ctx, 4, None).err();
            assert!(matches!(err, Some(Error::Timeout)), "{err:?}");
            assert!(matches!(svc.source, Some(Source::Cached { at: 0, .. })));
        }
        // 3. Sticky memory refusal from a failed `intern_term` while Buffer headroom remains
        {
            let ctx = ctx(16 << 10);
            let var = ctx.var("s");
            let rows = make_rows();
            let charge = OwnedCharge::new(&ctx, rows.retained_bytes()).unwrap();
            let mut svc = test_service(
                &ctx,
                var,
                Source::Cached {
                    rows,
                    cols: vec![Some(0)],
                    at: 0,
                    _charge: charge,
                },
            );
            let oversized = Term::Literal(Literal::new_simple_literal("x".repeat(32 << 10)));
            assert_eq!(ctx.intern_term(&oversized), Id::UNDEF);
            let err = svc.fill(&ctx, 4, None).err();
            assert!(
                matches!(
                    err,
                    Some(Error::BudgetExceeded(b)) if b.kind == crate::BudgetKind::Memory
                ),
                "{err:?}"
            );
            assert!(matches!(svc.source, Some(Source::Cached { at: 0, .. })));
        }
    }

    #[test]
    fn is_non_suppressible_matches_strict_and_resource_refusals() {
        let late_head = Error::Service(format!("{}<http://ep/sparql>", super::super::LATE_HEAD));
        assert!(is_non_suppressible(&late_head, true));
        assert!(!is_non_suppressible(&late_head, false));

        let ordinary = Error::Service("<http://ep/sparql>: 500".into());
        assert!(!is_non_suppressible(&ordinary, true));
        assert!(!is_non_suppressible(&ordinary, false));

        let unconfigured = crate::sparql::extensions::unconfigured();
        assert!(is_non_suppressible(&unconfigured, true));
        assert!(is_non_suppressible(&unconfigured, false));

        let parent_refused = Error::Component(Box::new(
            crate::error::ComponentError::new("query-memory", "ownership", "overflow")
                .with_source(sparkles_budget::ParentRefused::Overflow),
        ));
        assert!(is_non_suppressible(&parent_refused, true));
        assert!(is_non_suppressible(&parent_refused, false));

        let other_component = Error::Component(Box::new(crate::error::ComponentError::new(
            "service",
            "other",
            "transient",
        )));
        assert!(!is_non_suppressible(&other_component, true));
    }

    /// If copying out of `Service::buffered` fails (e.g. tight memory budget), `Service::next`
    /// releases `self.buffered` so its charge is not leaked.
    #[test]
    fn buffered_copy_rows_error_releases_buffered_charge() {
        let ctx = ctx(16 << 10);
        let var = ctx.var("s");
        let mut buf = Buffer::new(&ctx, &[var], 128).unwrap();
        for i in 0..128u64 {
            buf.table.push_row(&[Id(i + 1)]);
        }
        buf.reconcile().unwrap();
        let mut svc = Service {
            endpoint: PathEnd::Const(Id::UNDEF),
            query: "SELECT ?s { ?s ?p ?o }".into(),
            silent: true,
            cache: CacheMode::Default,
            key: None,
            vars: vec![var],
            names: vec![ctx.var_name(var)],
            fallback: FallbackPolicy::RejectMaterialization,
            opened: true,
            buffered_before_output: false,
            source: None,
            buffered: Some(buf),
            at: 0,
            bnodes: FxHashMap::default(),
        };
        assert!(ctx.memory().live.load(Ordering::Relaxed) > 0);
        // Leave less than `copy_rows`'s 64-row buffer needs so `copy_rows` itself fails.
        let hold = OwnedCharge::new(&ctx, ctx.memory_remaining().saturating_sub(64)).unwrap();
        let opts = CursorOptions::default();
        let err = svc.next(&ctx, &opts, 64).err();
        assert!(
            matches!(
                err,
                Some(Error::BudgetExceeded(b)) if b.kind == crate::BudgetKind::Memory
            ),
            "expected Memory BudgetExceeded, got {err:?}"
        );
        assert!(svc.buffered.is_none(), "buffered rows must be released");
        assert!(svc.done());
        drop(hold);
        assert_eq!(ctx.memory().live.load(Ordering::Relaxed), 0);
    }

    /// The streamed answer to a request with a LIMIT is cached only once the response
    /// is known to end at the limit: a response that fails after the rows it was asked
    /// for, or sends more of them, leaves the cache as it was, with SILENT or without.
    /// A read-ahead error (such as a timeout) also leaves the cache as it was.
    #[test]
    fn a_limited_answer_is_cached_only_when_it_ends_at_the_limit() {
        use crate::sparql::svccache::{Parts, Slice};
        let head = br#"{"head":{"vars":["s"]},"results":{"bindings":["#.to_vec();
        let row = |v: &str| format!(r#"{{"s":{{"type":"literal","value":"{v}"}}}}"#);
        for (tail, cached) in [
            (format!("{},{}]}}}}", row("a"), row("b")), true),
            (
                format!("{},{},{}]}}}}", row("a"), row("b"), row("c")),
                false,
            ),
            (format!("{},{},{{\"s\":", row("a"), row("b")), false),
        ] {
            for silent in [None, Some(0)] {
                let ctx = ctx(1 << 20);
                let var = ctx.var("s");
                let mut chunks = std::collections::VecDeque::new();
                chunks.push_back(head.clone());
                chunks.push_back(tail.clone().into_bytes());
                let reader = ChunkedReader {
                    chunks,
                    armed: Arc::new(std::sync::atomic::AtomicBool::new(false)),
                    cancel_on_first_armed_read: None,
                    triggered: false,
                    reads_after_first_armed: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
                };
                let answer = RemoteAnswer::from_reader(
                    Box::new(reader),
                    sparesults::QueryResultsFormat::Json,
                    "http://test.invalid/sparql",
                )
                .unwrap();
                let key = "limited".to_string();
                let slice = Slice::ALL.limited(2);
                let tee = Tee::new(
                    &ctx,
                    key.clone(),
                    ctx.snap.results.service.max_entry_bytes(),
                )
                .unwrap()
                .of(Parts::default(), slice);
                let mut svc = test_service(
                    &ctx,
                    var,
                    Source::Remote {
                        answer: Box::new(answer),
                        tee: Some(tee),
                        peeked: None,
                    },
                );
                // the query reads the two rows it asked for, and no further
                let first = svc.fill(&ctx, 2, silent);
                if silent.is_some() {
                    // a SILENT drain reads on to the end
                    let mut r = first.map(|_| ());
                    while r.is_ok() && svc.source.is_some() {
                        r = svc.fill(&ctx, 2, silent).map(|_| ());
                    }
                }
                let expect = if silent.is_some() { cached } else { false };
                assert_eq!(
                    ctx.snap.results.service.get(&key, slice).is_some(),
                    expect,
                    "{tail} silent {silent:?}"
                );
            }
        }
    }

    /// A non-SILENT fill returns the demanded rows from the ready head without reading
    /// the stalled tail, so a slow endpoint does not block the query past its rows.
    #[test]
    fn non_silent_fill_does_not_read_the_stalled_tail() {
        use crate::sparql::svccache::{Parts, Slice};
        let head = br#"{"head":{"vars":["s"]},"results":{"bindings":["#.to_vec();
        let row = |v: &str| format!(r#"{{"s":{{"type":"literal","value":"{v}"}}}}"#);
        let ctx = ctx(1 << 20);
        let var = ctx.var("s");
        let tail_reads = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let mut chunks = std::collections::VecDeque::new();
        chunks.push_back(head);
        // include a comma after row b so the parser can emit it without reading ahead
        chunks.push_back(format!("{},{},", row("a"), row("b")).into_bytes());
        let reader = StalledTailReader {
            chunks,
            tail_reads: tail_reads.clone(),
        };
        let answer = RemoteAnswer::from_reader(
            Box::new(reader),
            sparesults::QueryResultsFormat::Json,
            "http://test.invalid/sparql",
        )
        .unwrap();
        let slice = Slice::ALL.limited(2);
        let tee = Tee::new(
            &ctx,
            "stalled".to_string(),
            ctx.snap.results.service.max_entry_bytes(),
        )
        .unwrap()
        .of(Parts::default(), slice);
        let mut svc = test_service(
            &ctx,
            var,
            Source::Remote {
                answer: Box::new(answer),
                tee: Some(tee),
                peeked: None,
            },
        );
        let buf = svc.fill(&ctx, 2, None).unwrap();
        let rows = buf.map(|b| b.table.len).unwrap_or(0);
        assert_eq!(rows, 2, "both demanded rows returned");
        assert_eq!(
            tail_reads.load(Ordering::SeqCst),
            0,
            "no tail reads attempted"
        );
        assert!(
            ctx.snap.results.service.get("stalled", slice).is_none(),
            "stalled response is not cached"
        );
    }

    struct StalledTailReader {
        chunks: std::collections::VecDeque<Vec<u8>>,
        tail_reads: Arc<std::sync::atomic::AtomicUsize>,
    }

    impl std::io::Read for StalledTailReader {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            if let Some(mut chunk) = self.chunks.pop_front() {
                let n = chunk.len().min(buf.len());
                buf[..n].copy_from_slice(&chunk[..n]);
                if n < chunk.len() {
                    self.chunks.push_front(chunk.split_off(n));
                }
                Ok(n)
            } else {
                self.tail_reads.fetch_add(1, Ordering::SeqCst);
                Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "stalled tail",
                ))
            }
        }
    }
}
