//! A SERVICE without SILENT, read from its endpoint a batch at a time (item 829).
//!
//! A remote endpoint's response is parsed as it arrives, so the operator holds one
//! output batch and the parser's buffer between pulls, never the whole answer. A query
//! endpoint of this process is opened as a cursor of its own under the same policy, as
//! the caller, and the operator pulls one of its batches per batch. A cached answer is
//! read from the cache entry, which the operator holds under a charge while it reads it,
//! since the cache may evict the entry meanwhile.
//!
//! Nothing happens before the first pull, because operators are also built only to see
//! whether an EXISTS pattern streams. The request, the policy checks and the endpoint's
//! errors all come at the first pull, as they come when an eager run reaches the node.
//!
//! The endpoint's blank nodes are its own. One map for the whole answer gives each label
//! one new blank node of this query, across batches, as one eager call does.
//!
//! Three things differ from an eager run. A failure after rows were emitted fails the
//! cursor, as any streamed failure does. A response is cached only when it has been read
//! to its end, so a consumer that stops early leaves nothing in the cache, and a copy
//! that grows past the largest entry the cache keeps is dropped as it grows. And the
//! outbound timeout counts from the start of the request, while the query's deadline
//! leaves out the time a consumer keeps the cursor waiting, so a slow consumer can expire
//! a remote transfer that an eager run would finish.
//!
//! A SILENT SERVICE answers one empty solution when the call fails, after any number of
//! rows, so it needs the whole answer before its first row and stays an eager leaf that
//! the strict policy refuses (decision Q-SVC1).
use super::{Buffer, CursorOptions, FallbackPolicy, QueryCursor};
use crate::error::{Error, Result};
use crate::id::Id;
use crate::sparql::ctx::{Ctx, OwnedCharge};
use crate::sparql::enhancer::CacheMode;
use crate::sparql::exec::{LocalCall, RemoteAnswer, ServiceSolution};
use crate::sparql::plan::{Kind, Node, PathEnd};
use crate::sparql::svccache::Rows;
use crate::sparql::table::VarId;
use rustc_hash::FxHashMap;
use std::sync::Arc;
use std::sync::atomic::Ordering;

/// Whether a plan node runs as a streamed SERVICE: one without SILENT, which reads no
/// input.
pub(super) fn eligible(node: &Node) -> bool {
    matches!(node.kind, Kind::Service { silent: false, .. }) && node.children.is_empty()
}

pub(super) struct Service {
    endpoint: PathEnd,
    query: String,
    cache: CacheMode,
    vars: Vec<VarId>,
    /// the names of `vars`, as the endpoint's solutions name their variables
    names: Vec<String>,
    /// the policy an in-process call runs under
    fallback: FallbackPolicy,
    opened: bool,
    source: Option<Source>,
    /// the blank node of this query for each label of the endpoint's answer
    bnodes: FxHashMap<String, Id>,
}

enum Source {
    Remote {
        answer: Box<RemoteAnswer>,
        /// the copy of the answer read so far, for the cache once it has all of it
        tee: Option<Tee>,
    },
    Local {
        /// Declared before `hold`, so that the call ends before its hold is released.
        cursor: Box<QueryCursor>,
        /// the callee's batch being read, and the next row of it
        batch: Option<(super::QueryBatch, usize)>,
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
    rows: Rows,
    /// `rows.bytes()`, kept as rows arrive
    bytes: u64,
    /// the largest entry the cache keeps, with its key
    limit: u64,
    charge: OwnedCharge,
}

impl Tee {
    /// Copy one solution, or give up the copy once it is larger than the cache keeps.
    fn push(mut self, solution: &sparesults::QuerySolution) -> Result<Option<Self>> {
        // the variables of the first solution that binds any, as `fetch_rows` takes them
        if self.rows.vars.is_empty() && !solution.variables().is_empty() {
            self.rows.vars = solution
                .variables()
                .iter()
                .map(|v| v.as_str().to_owned())
                .collect();
            self.bytes += Rows::vars_bytes(&self.rows.vars);
        }
        let row: Vec<_> = self
            .rows
            .vars
            .iter()
            .map(|v| solution.get(v.as_str()).cloned())
            .collect();
        self.bytes += Rows::row_bytes(&row);
        if self.bytes.saturating_add(self.key.len() as u64) > self.limit {
            return Ok(None);
        }
        self.rows.rows.push(row);
        // the spare capacity of the row vector is live too
        let spare = (self.rows.rows.capacity() - self.rows.rows.len()) as u64 * 24;
        self.charge.resize(self.bytes.saturating_add(spare))?;
        Ok(Some(self))
    }
}

impl Service {
    pub(super) fn new(ctx: &Ctx, node: &Node, fallback: FallbackPolicy) -> Self {
        let Kind::Service {
            endpoint,
            query,
            cache,
            ..
        } = &node.kind
        else {
            unreachable!("a SERVICE node")
        };
        Self {
            endpoint: endpoint.clone(),
            query: query.clone(),
            cache: *cache,
            vars: node.vars.clone(),
            names: node.vars.iter().map(|v| ctx.var_name(*v)).collect(),
            fallback,
            opened: false,
            source: None,
            bnodes: FxHashMap::default(),
        }
    }

    /// Whether the answer has been read to its end.
    pub(super) fn done(&self) -> bool {
        self.opened && self.source.is_none()
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
            // The callee's deadline is the caller's without its pauses, and each pull
            // gives it the caller's pauses so far, so the call ends when its caller does.
            let call = LocalCall::new(ctx, target, ctx.deadline)?;
            ctx.check()?;
            let cursor = CursorOptions {
                fallback: self.fallback,
                ..options.clone()
            };
            let execution = super::query_cursor(call.snap, &self.query, &call.options, &cursor)?;
            let super::QueryExecution::Select(cursor) = execution else {
                return Err(Error::Service(format!(
                    "SERVICE <{}> did not answer with solutions",
                    url.as_str()
                )));
            };
            self.source = Some(Source::Local {
                cursor,
                batch: None,
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
            // the whole result under one key: a SERVICE without `loop` has one input
            let key =
                crate::sparql::svccache::key(&ctx.service_scope, url.as_str(), &self.query, &[]);
            let hit = if self.cache == CacheMode::Default {
                store.get(&key)
            } else {
                store.remove(&key);
                None
            };
            if let Some(rows) = hit {
                let charge = OwnedCharge::new(ctx, rows.bytes())?;
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
            tee = Some(Tee {
                key,
                rows: Rows {
                    vars: Vec::new(),
                    rows: Vec::new(),
                },
                bytes: 64,
                limit: store.max_entry_bytes(),
                charge: OwnedCharge::new(ctx, 64)?,
            });
        }
        let answer = RemoteAnswer::open(ctx, &url, &self.query)?;
        self.source = Some(Source::Remote {
            answer: Box::new(answer),
            tee,
        });
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
            self.open(ctx, options)?;
        }
        if self.source.is_none() {
            return Ok(None);
        }
        let result = self.fill(ctx, cap);
        if result.is_err() {
            // a failed answer is closed, and its body and its copy released, now
            self.source = None;
        }
        result
    }

    fn fill(&mut self, ctx: &Arc<Ctx>, cap: usize) -> Result<Option<Buffer>> {
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
            Source::Remote { answer, tee } => {
                let _entered = answer.span.clone().entered();
                loop {
                    if b.table.len >= cap {
                        break false;
                    }
                    let Some(solution) = answer.next_solution() else {
                        if let Some(tee) = tee.take() {
                            ctx.snap.results.service.put(tee.key, Arc::new(tee.rows));
                        }
                        break true;
                    };
                    let solution = solution?;
                    if let Some(copy) = tee.take() {
                        *tee = copy.push(&solution)?;
                    }
                    push(&mut b, bnodes, &ServiceSolution::Remote(&solution));
                }
            }
            Source::Local { cursor, batch, .. } => loop {
                if b.table.len >= cap {
                    break false;
                }
                if batch.as_ref().is_none_or(|(rows, at)| *at >= rows.len()) {
                    *batch = None;
                    cursor.deadline_pause().store(
                        ctx.deadline_pause.load(Ordering::Relaxed),
                        Ordering::Relaxed,
                    );
                    match cursor.next_batch_at_most(cap - b.table.len)? {
                        Some(next) => *batch = Some((next, 0)),
                        None => break true,
                    }
                }
                let (rows, at) = batch.as_mut().expect("a callee batch");
                let names = rows.variables();
                while *at < rows.len() && b.table.len < cap {
                    let row = rows.row(*at)?;
                    push(&mut b, bnodes, &ServiceSolution::Local { names, row: &row });
                    *at += 1;
                }
            },
            Source::Cached { rows, cols, at, .. } => {
                let end = rows.rows.len().min(*at + cap);
                let mut ids = vec![Id::UNDEF; cols.len()];
                for r in &rows.rows[*at..end] {
                    for (to, c) in ids.iter_mut().zip(cols.iter()) {
                        *to = c
                            .and_then(|c| r[c].as_ref())
                            .map_or(Id::UNDEF, |term| ctx.intern_remote_term(term, bnodes));
                    }
                    b.table.push_row(&ids);
                }
                *at = end;
                end == rows.rows.len()
            }
        };
        if ended {
            self.source = None;
        }
        b.reconcile()?;
        Ok((b.table.len > 0).then_some(b))
    }
}
