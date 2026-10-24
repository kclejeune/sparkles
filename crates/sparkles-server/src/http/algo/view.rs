//! View-only algorithm runs (spec U10 §3.2): the `nodes`, `keep`, `async` and `top`
//! members of a `POST /$/algo/{ds}/run` request without a `write` part.
//!
//! Such a run computes the whole result under the caller's view at the state the request
//! reads, and answers with the rows of the listed `nodes` only, the size of each
//! community or component in the whole projection, and the run's summary. With `keep`
//! the whole result goes into a per-server cache keyed by the dataset's branch, the
//! commit, the caller's read view, the dataset the request names and its inferred
//! overlay, and the request without these members, so a later request for other nodes
//! reads the cache instead of running again. With `keep` and `async` the run is a task
//! of kind [`TASK`], which needs only `read` and writes nothing, and a request for the
//! same key while it runs joins it.
//!
//! The cache keeps at most [`MAX_ENTRIES`] results and [`MAX_BYTES`] of their estimated
//! size, and never more than half of the server memory pool, so that the other half stays
//! for the admission of queries. It drops the oldest first, and charges each result to the
//! pool for as long as the cache or a reader holds it. A result the pool has no room for is
//! answered but not kept. The decoded rows of a run count against the request's memory
//! budget and pool grant as they are decoded, and the answer against its result-byte
//! limit.

use super::super::graph::cache::{Key, view_key};
use super::super::{ApiError, ApiResult, Params, St, blocking, err};
use crate::auth::{Endpoint, Principal};
use crate::state::{AppState, Dataset};
use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use oxrdf::Term;
use parking_lot::Mutex;
use serde_json::{Value as J, json};
use sparkles::sparql::QueryOptions;
use sparkles::sparql::algorun::{AlgoRequest, ProjectionRef};
use sparkles::sparql::pool::PoolGrant;
use sparkles::store::Snapshot;
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Instant;

/// The kind of the task of a background view-only run.
pub const TASK: &str = "algo-view";
/// Node terms a request may list.
pub(crate) const MAX_NODES: usize = 20_000;
/// Results kept at most.
pub(crate) const MAX_ENTRIES: usize = 8;
/// Estimated bytes of kept results at most.
pub(crate) const MAX_BYTES: u64 = 512 << 20;
/// The outputs that `top` orders by, in order of preference.
const NUMERIC: &[&str] = &[
    "score",
    "triangles",
    "core",
    "depth",
    "distance",
    "coefficient",
    "similarity",
];
/// The outputs whose values name a group whose size the answer reports.
const GROUPS: &[&str] = &["community", "component"];

/// The view-only members of a run request.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct ViewOpts {
    /// the IRIs whose rows the answer holds (`None`: every row)
    pub nodes: Option<Vec<String>>,
    pub keep: bool,
    /// run as a task (needs `keep`)
    pub background: bool,
    /// only the rows with the highest values of the first numeric output
    pub top: Option<usize>,
}

fn bad(m: impl Into<String>) -> ApiError {
    err(StatusCode::BAD_REQUEST, m)
}

impl ViewOpts {
    /// Take the view-only members out of a request's JSON, so that the rest parses as an
    /// [`AlgoRequest`]. `None` when it has none of them.
    pub(crate) fn split(j: &mut J) -> Result<Option<ViewOpts>, ApiError> {
        let Some(o) = j.as_object_mut() else {
            return Ok(None);
        };
        let nodes = o.remove("nodes");
        let keep = o.remove("keep");
        let background = o.remove("async");
        let top = o.remove("top");
        if nodes.is_none() && keep.is_none() && background.is_none() && top.is_none() {
            return Ok(None);
        }
        let flag = |v: Option<J>, name: &str| match v {
            None | Some(J::Null) => Ok(false),
            Some(J::Bool(b)) => Ok(b),
            Some(_) => Err(bad(format!("{name} takes true or false"))),
        };
        let keep = flag(keep, "keep")?;
        let background = flag(background, "async")?;
        if background && !keep {
            return Err(bad("async needs keep: a background run's result is kept"));
        }
        let nodes = match nodes {
            None | Some(J::Null) => None,
            Some(J::Array(items)) => {
                if items.len() > MAX_NODES {
                    return Err(bad(format!(
                        "nodes lists {} terms; at most {MAX_NODES} are allowed",
                        items.len()
                    )));
                }
                let mut out = Vec::with_capacity(items.len());
                for i in items {
                    let J::String(s) = i else {
                        return Err(bad("nodes takes IRIs as strings"));
                    };
                    let s = s
                        .strip_prefix('<')
                        .and_then(|s| s.strip_suffix('>'))
                        .unwrap_or(&s);
                    let iri = oxrdf::NamedNode::new(s)
                        .map_err(|e| bad(format!("'{s}' in nodes is not an IRI: {e}")))?;
                    out.push(iri.into_string());
                }
                Some(out)
            }
            Some(_) => return Err(bad("nodes takes an array of IRIs")),
        };
        let top = match top {
            None | Some(J::Null) => None,
            Some(v) => match v.as_u64() {
                Some(n) if (1..=MAX_NODES as u64).contains(&n) => Some(n as usize),
                _ => return Err(bad(format!("top takes a number from 1 to {MAX_NODES}"))),
            },
        };
        Ok(Some(ViewOpts {
            nodes,
            keep,
            background,
            top,
        }))
    }
}

/// The column of a group output, the column of the level, and the members of each group
/// in the whole projection, by level and group.
type Sizes = (usize, Option<usize>, HashMap<(Option<Term>, Term), u64>);

/// A run's whole result.
pub(crate) struct Kept {
    vars: Vec<String>,
    rows: Vec<Vec<Option<Term>>>,
    /// the rows of each node, by IRI
    index: HashMap<Box<str>, Vec<u32>>,
    summary: Option<J>,
    sizes: Option<Sizes>,
    commit: u64,
    bytes: u64,
    /// the pool memory the result holds while the cache keeps it, or a reader still has
    /// it after the cache dropped it
    grant: OnceLock<PoolGrant>,
}

/// The heap of one decoded cell, estimated.
fn cell_bytes(t: &Option<Term>) -> u64 {
    match t {
        Some(Term::NamedNode(n)) => n.as_str().len() as u64,
        Some(Term::BlankNode(b)) => b.as_str().len() as u64,
        Some(Term::Literal(l)) => l.value().len() as u64 + 16,
        Some(Term::Triple(_)) => 256,
        None => 0,
    }
}

/// The heap of one decoded row, estimated: the row vector and its terms.
fn row_bytes(r: &[Option<Term>]) -> u64 {
    let cell = std::mem::size_of::<Option<Term>>() as u64;
    24 + cell * r.len() as u64 + r.iter().map(cell_bytes).sum::<u64>()
}

/// The heap a row adds to the index and the group sizes at most, estimated.
fn row_index_bytes(r: &[Option<Term>]) -> u64 {
    let cell = std::mem::size_of::<Option<Term>>() as u64;
    let iri = r.first().map_or(0, cell_bytes);
    iri + 64 + 2 * cell + 32
}

impl Kept {
    fn new(
        vars: Vec<String>,
        rows: Vec<Vec<Option<Term>>>,
        summary: Option<J>,
        commit: u64,
    ) -> Kept {
        let col = |n: &str| vars.iter().position(|v| v == n);
        let node = col("node");
        let mut index: HashMap<Box<str>, Vec<u32>> = HashMap::new();
        if let Some(c) = node {
            for (i, r) in rows.iter().enumerate() {
                if let Some(Term::NamedNode(n)) = &r[c] {
                    index.entry(n.as_str().into()).or_default().push(i as u32);
                }
            }
        }
        let sizes = GROUPS.iter().find_map(|g| col(g)).map(|gc| {
            let lc = col("level");
            let mut m: HashMap<(Option<Term>, Term), u64> = HashMap::new();
            for r in &rows {
                if let Some(g) = &r[gc] {
                    let level = lc.and_then(|l| r[l].clone());
                    *m.entry((level, g.clone())).or_default() += 1;
                }
            }
            (gc, lc, m)
        });
        // An estimate of the heap the result holds: the row vectors and their terms, the
        // index and the group sizes.
        let cell = std::mem::size_of::<Option<Term>>() as u64;
        let mut bytes: u64 = rows.iter().map(|r| row_bytes(r)).sum();
        bytes += index.keys().map(|k| k.len() as u64 + 64).sum::<u64>();
        if let Some((_, _, m)) = &sizes {
            bytes += m.len() as u64 * (2 * cell + 32);
        }
        bytes += summary.as_ref().map_or(0, |s| s.to_string().len() as u64);
        Kept {
            vars,
            rows,
            index,
            summary,
            sizes,
            commit,
            bytes,
            grant: OnceLock::new(),
        }
    }

    /// The answer for `view`: the rows of its nodes (or all, up to `max_rows`), the
    /// highest `top` of them by the first numeric output, with the size of each row's
    /// group in the whole projection.
    fn answer(&self, view: &ViewOpts, cached: bool, max_rows: usize) -> J {
        let mut picked: Vec<u32> = match &view.nodes {
            Some(nodes) => {
                let mut seen = HashSet::new();
                nodes
                    .iter()
                    .filter(|n| seen.insert(n.as_str()))
                    .filter_map(|n| self.index.get(n.as_str()))
                    .flatten()
                    .copied()
                    .collect()
            }
            None => (0..self.rows.len() as u32).collect(),
        };
        if let Some(top) = view.top
            && let Some(c) = NUMERIC
                .iter()
                .find_map(|n| self.vars.iter().position(|v| v == n))
        {
            let value = |i: &u32| match &self.rows[*i as usize][c] {
                Some(Term::Literal(l)) => l.value().parse::<f64>().ok().filter(|v| !v.is_nan()),
                _ => None,
            };
            // highest first, rows without a value last, ties in row order
            picked.sort_by(|a, b| match (value(a), value(b)) {
                (Some(x), Some(y)) => y.total_cmp(&x),
                (Some(_), None) => std::cmp::Ordering::Less,
                (None, Some(_)) => std::cmp::Ordering::Greater,
                (None, None) => std::cmp::Ordering::Equal,
            });
            picked.truncate(top);
        }
        let truncated = picked.len() > max_rows;
        picked.truncate(max_rows);
        let mut vars = self.vars.clone();
        if self.sizes.is_some() {
            vars.push("communitySize".into());
        }
        let bindings: Vec<J> = picked
            .iter()
            .map(|&i| {
                let row = &self.rows[i as usize];
                let mut b = serde_json::Map::new();
                for (v, t) in self.vars.iter().zip(row) {
                    if let Some(t) = t {
                        b.insert(v.clone(), sparkles::sparql::results::term_json(t));
                    }
                }
                if let Some((gc, lc, m)) = &self.sizes
                    && let Some(g) = &row[*gc]
                {
                    let level = lc.and_then(|l| row[l].clone());
                    if let Some(n) = m.get(&(level, g.clone())) {
                        b.insert(
                            "communitySize".into(),
                            json!({
                                "type": "literal",
                                "value": n.to_string(),
                                "datatype": "http://www.w3.org/2001/XMLSchema#integer",
                            }),
                        );
                    }
                }
                J::Object(b)
            })
            .collect();
        json!({
            "head": { "vars": vars },
            "results": { "bindings": bindings },
            "summary": self.summary,
            "run": {
                "commit": self.commit,
                "cached": cached,
                "rows": self.rows.len(),
                "returned": bindings.len(),
                "truncated": truncated,
            },
        })
    }
}

/// The decoded bytes a run holds, charged to the request's pool grant and checked
/// against its memory budget as they grow, and given back when the run's answer is
/// written.
pub(crate) struct Held {
    grant: Option<Arc<PoolGrant>>,
    limit: Option<u64>,
    held: u64,
}

impl Held {
    pub(crate) fn new(opts: &QueryOptions) -> Held {
        Held {
            grant: opts.grant.clone(),
            limit: opts.max_memory_bytes,
            held: 0,
        }
    }

    /// Account for `n` more bytes: `507` with `budget: "memory"` past the request's
    /// budget, and the pool's own refusal when it cannot give them.
    fn charge(&mut self, n: u64) -> sparkles::Result<()> {
        let total = self.held.saturating_add(n);
        if let Some(limit) = self.limit
            && total > limit
        {
            return Err(sparkles::Error::BudgetExceeded(sparkles::Budget {
                kind: sparkles::BudgetKind::Memory,
                limit,
                requested: total,
            }));
        }
        if let Some(g) = &self.grant {
            g.charge(n)?;
        }
        self.held = total;
        Ok(())
    }

    /// Make the bytes held `n`, charging or giving back the difference.
    fn settle(&mut self, n: u64) -> sparkles::Result<()> {
        if n > self.held {
            self.charge(n - self.held)
        } else {
            let less = self.held - n;
            if let Some(g) = &self.grant {
                g.release(less);
            }
            self.held = n;
            Ok(())
        }
    }
}

impl Drop for Held {
    fn drop(&mut self) {
        if let Some(g) = &self.grant {
            g.release(self.held);
        }
    }
}

/// Rows decoded between two charges.
const CHARGE_EVERY: usize = 1024;

/// Run the query `q` at `snap` under `opts` and keep every row. The decoded rows are
/// charged to `held` as they are decoded, before they can grow past the request's
/// budget or the pool, and the index and group sizes before they are built.
fn compute(
    snap: Arc<Snapshot>,
    q: &str,
    opts: &QueryOptions,
    held: &mut Held,
) -> sparkles::Result<Kept> {
    let commit = snap.commit;
    let r = sparkles::sparql::query(snap, q, opts)?;
    let mut vars = r.vars.clone();
    // the summary is the last column, the same literal in every row: decoded once
    let summary_col = (vars.last().map(String::as_str) == Some("summary")).then(|| {
        vars.pop();
        vars.len()
    });
    let n = r.table.len();
    let width = vars.len();
    let mut summary = None;
    if let Some(c) = summary_col
        && n > 0
        && let Some(Term::Literal(l)) = r.term(r.table.cols[c][0])
    {
        summary = Some(serde_json::from_str(l.value()).unwrap_or(J::from(l.value())));
    }
    let mut rows: Vec<Vec<Option<Term>>> = Vec::new();
    let mut pending = 0u64;
    for i in 0..n {
        let row: Vec<Option<Term>> = r.table.cols[..width].iter().map(|c| r.term(c[i])).collect();
        pending += row_bytes(&row) + row_index_bytes(&row);
        rows.push(row);
        if rows.len().is_multiple_of(CHARGE_EVERY) {
            held.charge(pending)?;
            pending = 0;
        }
    }
    held.charge(pending)?;
    drop(r);
    let kept = Kept::new(vars, rows, summary, commit);
    held.settle(kept.bytes)?;
    Ok(kept)
}

// ---------------------------------------------------------------- the cache ------

struct Entry {
    key: Key,
    kept: Arc<Kept>,
}

/// A bounded cache of results. The server has one, [`CACHE`].
pub(crate) struct ViewCache {
    entries: Mutex<VecDeque<Entry>>,
}

static CACHE: ViewCache = ViewCache::new();
static HITS: AtomicU64 = AtomicU64::new(0);
static MISSES: AtomicU64 = AtomicU64::new(0);
static JOINS: AtomicU64 = AtomicU64::new(0);
static REFUSED: AtomicU64 = AtomicU64::new(0);

impl ViewCache {
    const fn new() -> ViewCache {
        ViewCache {
            entries: parking_lot::const_mutex(VecDeque::new()),
        }
    }

    fn get(&self, key: &Key) -> Option<Arc<Kept>> {
        let mut c = self.entries.lock();
        let i = c.iter().position(|e| e.key == *key)?;
        let e = c.remove(i)?;
        let kept = e.kept.clone();
        c.push_back(e);
        Some(kept)
    }

    /// Keep `kept` for `key`, dropping the oldest results until it fits the cache's
    /// bounds and the pool grants its bytes. `false` when it could not be kept.
    ///
    /// The cache holds at most half of the pool, so that at least half stays for the
    /// admission of queries, and a result larger than that is answered but not kept. The
    /// grant lives with the result, so a reader that still holds a result the cache
    /// dropped keeps it charged until it lets go.
    fn put(
        &self,
        pool: Option<&Arc<sparkles::sparql::pool::QueryPool>>,
        key: Key,
        kept: Arc<Kept>,
    ) -> bool {
        let bytes = kept.bytes;
        let share = pool.map_or(MAX_BYTES, |p| (p.capacity() / 2).min(MAX_BYTES));
        if bytes > share {
            REFUSED.fetch_add(1, Ordering::Relaxed);
            return false;
        }
        let mut c = self.entries.lock();
        if let Some(i) = c.iter().position(|e| e.key == key) {
            c.remove(i);
        }
        // what the kept results hold of the pool, or their estimate without one
        let held = |e: &Entry| e.kept.grant.get().map_or(e.kept.bytes, |g| g.granted());
        let total = |c: &VecDeque<Entry>| c.iter().map(held).sum::<u64>();
        while c.len() >= MAX_ENTRIES || (!c.is_empty() && total(&c) + bytes > share) {
            c.pop_front();
        }
        if let Some(pool) = pool
            && kept.grant.get().is_none()
        {
            let grant = loop {
                match pool
                    .try_grant(bytes)
                    .filter(|g| g.granted() >= bytes && total(&c) + g.granted() <= share)
                {
                    Some(g) => break g,
                    // the oldest results give their memory back for a newer one
                    None if !c.is_empty() => {
                        c.pop_front();
                    }
                    None => {
                        REFUSED.fetch_add(1, Ordering::Relaxed);
                        return false;
                    }
                }
            };
            // another thread that kept the same result first holds its grant
            let _ = kept.grant.set(grant);
        }
        c.push_back(Entry { key, kept });
        true
    }

    /// The kept results and their estimated bytes.
    fn stats(&self) -> (usize, u64) {
        let c = self.entries.lock();
        (c.len(), c.iter().map(|e| e.kept.bytes).sum())
    }
}

fn get(key: &Key) -> Option<Arc<Kept>> {
    CACHE.get(key)
}

fn put(pool: Option<&Arc<sparkles::sparql::pool::QueryPool>>, key: Key, kept: Arc<Kept>) -> bool {
    CACHE.put(pool, key, kept)
}

/// The server's kept results and their estimated bytes.
pub(crate) fn stats() -> (usize, u64) {
    CACHE.stats()
}

#[cfg(test)]
pub(crate) fn counts() -> (u64, u64, u64) {
    (
        HITS.load(Ordering::Relaxed),
        MISSES.load(Ordering::Relaxed),
        JOINS.load(Ordering::Relaxed),
    )
}

/// The cache figures for `/metrics`.
pub(crate) fn metrics(out: &mut String) {
    use std::fmt::Write;
    let (entries, bytes) = stats();
    let mut family = |name: &str, kind: &str, help: &str, v: u64| {
        let _ = writeln!(out, "# HELP {name} {help}");
        let _ = writeln!(out, "# TYPE {name} {kind}");
        let _ = writeln!(out, "{name} {v}");
    };
    family(
        "sparkles_algo_view_cache_entries",
        "gauge",
        "View-only algorithm results kept for reuse.",
        entries as u64,
    );
    family(
        "sparkles_algo_view_cache_bytes",
        "gauge",
        "Estimated bytes of the kept view-only algorithm results.",
        bytes,
    );
    family(
        "sparkles_algo_view_cache_hits_total",
        "counter",
        "View-only algorithm requests answered from a kept result.",
        HITS.load(Ordering::Relaxed),
    );
    family(
        "sparkles_algo_view_cache_misses_total",
        "counter",
        "View-only algorithm requests that ran the algorithm.",
        MISSES.load(Ordering::Relaxed),
    );
    family(
        "sparkles_algo_view_cache_joins_total",
        "counter",
        "Background view-only requests that joined a run already in progress.",
        JOINS.load(Ordering::Relaxed),
    );
    family(
        "sparkles_algo_view_cache_refused_total",
        "counter",
        "View-only results not kept because they exceeded the cache or the memory pool had no room.",
        REFUSED.load(Ordering::Relaxed),
    );
}

// ------------------------------------------------------- background runs ------

/// The runs in progress, by key, with their task.
static RUNNING: Mutex<Vec<(Key, String)>> = parking_lot::const_mutex(Vec::new());
/// A caller of a background run: who it is and its read view.
type Caller = (String, String);

/// The callers of each background run's task, the one that started it first, each with
/// the read view it had then. They may follow the task even when their view of the
/// dataset is restricted, and the first may cancel it, for as long as they can still
/// read the dataset through the same view.
static CALLERS: Mutex<VecDeque<(String, Vec<Caller>)>> = parking_lot::const_mutex(VecDeque::new());
/// Tasks whose callers are remembered at most. Records of tasks the server still lists
/// are kept below this, and only past it does the oldest go.
const MAX_CALLERS: usize = 4096;

fn who(p: &Principal) -> String {
    format!("{:?}\u{1f}{}", p.kind, p.name)
}

/// The read view of `p` on `ds`, as the cache keys it.
fn read_view(p: &Principal, ds: &str) -> String {
    view_key(p.view(ds, Endpoint::Query).as_ref())
}

/// A task's key among every server's: task ids count per server.
fn task_key(st: &AppState, id: &str) -> String {
    format!("{:p}/{id}", st)
}

pub(crate) fn add_caller(st: &AppState, id: &str, ds: &str, p: &Principal) {
    let id = &task_key(st, id);
    let me = (who(p), read_view(p, ds));
    let mut c = CALLERS.lock();
    if let Some((_, v)) = c.iter_mut().find(|(t, _)| t == id) {
        if !v.contains(&me) {
            v.push(me);
        }
        return;
    }
    if c.len() >= MAX_CALLERS {
        // the records of this server's tasks that it no longer lists go first
        let mine = format!("{:p}/", st);
        let listed: HashSet<String> = st
            .tasks
            .lock()
            .iter()
            .map(|t| task_key(st, &t.id))
            .collect();
        c.retain(|(t, _)| !t.starts_with(&mine) || listed.contains(t));
        while c.len() >= MAX_CALLERS {
            c.pop_front();
        }
    }
    c.push_back((id.to_string(), vec![me]));
}

/// The callers of task `id`, if `p` may still act as one: it reads `ds` now, and through
/// the same view as when it started or joined the run.
fn caller_pos(st: &AppState, id: &str, ds: &str, p: &Principal) -> Option<usize> {
    if !p.can(ds, crate::auth::Level::Read) {
        return None;
    }
    let id = task_key(st, id);
    let me = (who(p), read_view(p, ds));
    CALLERS
        .lock()
        .iter()
        .find(|(t, _)| *t == id)
        .and_then(|(_, v)| v.iter().position(|c| *c == me))
}

/// Whether `p` started or joined the background run of task `id` on `ds`, and may still
/// follow it.
pub(crate) fn follows(st: &AppState, id: &str, ds: &str, p: &Principal) -> bool {
    caller_pos(st, id, ds, p).is_some()
}

/// Whether `p` started the background run of task `id` on `ds`, and may still cancel it.
pub(crate) fn started(st: &AppState, id: &str, ds: &str, p: &Principal) -> bool {
    caller_pos(st, id, ds, p) == Some(0)
}

/// The tasks of the runs in progress, for tests.
#[cfg(test)]
pub(crate) fn running_ids() -> Vec<String> {
    RUNNING.lock().iter().map(|(_, id)| id.clone()).collect()
}

/// Datasets whose background runs wait before they compute, so that tests can join and
/// cancel them while they run.
#[cfg(test)]
pub(crate) static PAUSED: Mutex<Vec<String>> = parking_lot::const_mutex(Vec::new());

/// Removes a run from [`RUNNING`] when its task ends, however it ends.
struct Running(Key, String);

impl Drop for Running {
    fn drop(&mut self) {
        RUNNING
            .lock()
            .retain(|(k, id)| !(*k == self.0 && *id == self.1));
    }
}

// ------------------------------------------------------------- the route ------

/// The part of the key that names the request: the query text it stands for, with every
/// prefixed name expanded by the dataset's prefixes as they are now, and the definition
/// of a named projection, which can change without a commit. The run computes this same
/// text, so a changed prefix is a different key.
fn request_key(ds: &Dataset, req: &AlgoRequest) -> Result<(String, String), ApiError> {
    let q = ds
        .dataset
        .algorithms()
        .query_text_with_summary(req)
        .map_err(ApiError::from)?;
    let mut k = q.clone();
    if let Some(ProjectionRef::Named(n)) = &req.projection {
        let def = ds.dataset.algorithms().projections().get(n);
        k.push('\u{1f}');
        k.push_str(
            &def.map(|s| serde_json::to_string(&s).unwrap_or_default())
                .unwrap_or_default(),
        );
    }
    Ok((q, k))
}

fn key(ds: &Dataset, commit: u64, opts: &QueryOptions, request: String) -> Key {
    Key {
        branch: ds.store.branch_id(),
        commit,
        view: view_key(opts.graphs.as_ref()),
        extra: format!(
            "{:?}|{:?}|{:?}|{}",
            opts.default_graph_uris,
            opts.named_graph_uris,
            opts.default_graph_extra,
            super::super::graph::rdfs_key(opts.rdfs.as_deref())
        ),
        request,
    }
}

/// The answer as a response, `507` with `budget: "result-bytes"` when its JSON is larger
/// than `limit`.
fn respond(answer: J, limit: Option<u64>) -> ApiResult {
    let body = serde_json::to_vec(&answer).map_err(|e| {
        err(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("could not write the answer: {e}"),
        )
    })?;
    if let Some(limit) = limit
        && body.len() as u64 > limit
    {
        return Err(sparkles::Error::BudgetExceeded(sparkles::Budget {
            kind: sparkles::BudgetKind::ResultBytes,
            limit,
            requested: body.len() as u64,
        })
        .into());
    }
    Ok((
        [(axum::http::header::CONTENT_TYPE, "application/json")],
        body,
    )
        .into_response())
}

/// A view-only run: answered at once, or with `async` started (or joined) as a task.
pub(crate) async fn run(
    State(st): St,
    ds: Arc<Dataset>,
    p: Principal,
    params: Params,
    req: Arc<AlgoRequest>,
    view: ViewOpts,
) -> ApiResult {
    let at = super::super::history::at_param(&params)?;
    let mut opts = super::super::query_options(&st, &ds, &params);
    // budgets the request asked for, never above the server's
    let asked = super::super::budgets::Overrides::parse(&params)?;
    asked.apply(&mut opts);
    let limit = asked.result_limit(st.limits.max_result_bytes);
    crate::auth::restrict(&mut opts, &p, &ds.name, Endpoint::Query);
    // the whole result is computed, and the answer is cut to the request's row limit
    let max_rows = opts.max_rows.take().unwrap_or(usize::MAX);
    let (q, request) = request_key(&ds, &req)?;
    if view.background {
        let run = Background {
            q,
            request,
            max_rows,
            limit,
        };
        return background(st, ds, p, params, req, view, at, opts, run).await;
    }
    let timeout = opts.timeout;
    let (cancel, _cancel_on_drop) = super::super::cancel_on_drop();
    opts.cancel = Some(cancel);
    blocking(move || {
        let started = Instant::now();
        let (snap, _) = super::super::history::snapshot_for(&ds, at.as_ref(), &opts)?;
        let key = key(&ds, snap.commit, &opts, request);
        if let Some(kept) = get(&key) {
            HITS.fetch_add(1, Ordering::Relaxed);
            return respond(kept.answer(&view, true, max_rows), limit);
        }
        MISSES.fetch_add(1, Ordering::Relaxed);
        // the decoded rows stay charged until the answer is written
        let mut held = Held::new(&opts);
        let kept = Arc::new(
            compute(snap, &q, &opts, &mut held)
                .map_err(|e| super::super::with_timeout(e.into(), timeout))?,
        );
        tracing::debug!(
            "/{}: view-only {} run in {:?}",
            ds.name,
            req.algorithm,
            started.elapsed()
        );
        if view.keep {
            put(st.catalog.pool(), key, kept.clone());
        }
        let out = respond(kept.answer(&view, false, max_rows), limit);
        drop(held);
        out
    })
    .await
}

/// What a background run computes and how its answer is bounded.
struct Background {
    /// the query text the request stands for
    q: String,
    /// the request's part of the key
    request: String,
    max_rows: usize,
    /// the result-byte limit of an answer from the cache
    limit: Option<u64>,
}

#[allow(clippy::too_many_arguments)]
async fn background(
    st: Arc<AppState>,
    ds: Arc<Dataset>,
    p: Principal,
    params: Params,
    req: Arc<AlgoRequest>,
    view: ViewOpts,
    at: Option<sparkles::history::At>,
    mut opts: QueryOptions,
    run: Background,
) -> ApiResult {
    let Background {
        q,
        request,
        max_rows,
        limit,
    } = run;
    // A background run is not bound to the request: it has the deadline of an update,
    // none unless the request or `--update-timeout` gives one, and takes its grant of
    // the memory pool when its task starts, as a write-back run does.
    opts.timeout = super::super::update_timeout(&st, &params);
    opts.grant = None;
    blocking(move || {
        let (snap, _) = super::super::history::snapshot_for(&ds, at.as_ref(), &opts)?;
        let key = key(&ds, snap.commit, &opts, request);
        if let Some(kept) = get(&key) {
            HITS.fetch_add(1, Ordering::Relaxed);
            return respond(kept.answer(&view, true, max_rows), limit);
        }
        let mut running = RUNNING.lock();
        // a run that was cancelled is not joined: it is ending, and a new one starts
        let live = running.iter().find_map(|(k, id)| {
            (*k == key)
                .then(|| {
                    st.tasks
                        .lock()
                        .iter()
                        .find(|t| {
                            t.id == *id && t.active() && !t.cancel.load(Ordering::Relaxed)
                        })
                        .cloned()
                })
                .flatten()
        });
        if let Some(task) = live {
            let id = &task.id;
            JOINS.fetch_add(1, Ordering::Relaxed);
            add_caller(&st, id, &ds.name, &p);
            return Ok(task_answer(&task, true));
        }
        // a result kept between the check above and taking the lock
        if let Some(kept) = get(&key) {
            HITS.fetch_add(1, Ordering::Relaxed);
            return respond(kept.answer(&view, true, max_rows), limit);
        }
        super::super::task_start_check(&st, None, &ds.name)?;
        MISSES.fetch_add(1, Ordering::Relaxed);
        let admission = st.catalog.pool().map(|pool| {
            (
                pool.clone(),
                crate::server_memory::initial_grant(&st, pool),
                st.admission.wait,
            )
        });
        let id = st.next_task_id();
        running.push((key.clone(), id.clone()));
        // made before the task is queued, so that a task cancelled before it starts,
        // whose closure is dropped without running, still leaves the list
        let cleanup = Running(key.clone(), id.clone());
        add_caller(&st, &id, &ds.name, &p);
        let state = st.clone();
        let d = ds.clone();
        let task = st.start_task_opts(id, TASK, &ds.name, None, true, move |h| {
            let _running = cleanup;
            let mut opts = opts;
            opts.cancel = Some(h.cancel_flag());
            if let Some((pool, want, wait)) = admission {
                let control = h.control();
                let grant = pool.admit_blocking(want, wait, || control.cancel.is_cancelled())?;
                opts.grant = Some(Arc::new(grant));
            }
            #[cfg(test)]
            while PAUSED.lock().contains(&d.name) && !h.cancel_flag().load(Ordering::Relaxed) {
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            if h.cancel_flag().load(Ordering::Relaxed) {
                return Err(sparkles::Error::Cancelled.into());
            }
            // F14's algorithms report no iterations, so progress is by phase
            h.progress(0.05, "computing");
            let mut held = Held::new(&opts);
            let kept = Arc::new(compute(snap, &q, &opts, &mut held)?);
            h.progress(0.95, "keeping the result");
            let rows = kept.rows.len();
            let commit = kept.commit;
            let summary = kept.summary.clone();
            let stored = put(state.catalog.pool(), key, kept);
            drop(held);
            opts.grant = None;
            h.set_detail(json!({
                "algorithm": req.algorithm,
                "commit": commit,
                "rows": rows,
                "kept": stored,
                "summary": summary,
            }));
            Ok(if stored {
                format!("{}: kept a view-only run at commit {commit}", req.algorithm)
            } else {
                format!(
                    "{}: the view-only run finished at commit {commit}, but there was no room to keep it",
                    req.algorithm
                )
            })
        });
        drop(running);
        Ok(task_answer(&task, false))
    })
    .await
}

fn task_answer(task: &crate::state::Task, joined: bool) -> Response {
    let mut j = serde_json::to_value(task).unwrap_or_default();
    if let Some(o) = j.as_object_mut() {
        o.insert("joined".into(), joined.into());
    }
    (StatusCode::ACCEPTED, Json(j)).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use oxrdf::{Literal, NamedNode};

    fn iri(s: &str) -> Option<Term> {
        Some(NamedNode::new_unchecked(format!("http://ex/{s}")).into())
    }
    fn int(n: i64) -> Option<Term> {
        Some(Literal::from(n).into())
    }
    fn dbl(v: f64) -> Option<Term> {
        Some(Literal::from(v).into())
    }

    fn communities() -> Kept {
        let vars = vec!["node".into(), "community".into()];
        let rows = vec![
            vec![iri("a"), int(1)],
            vec![iri("b"), int(1)],
            vec![iri("c"), int(1)],
            vec![iri("d"), int(2)],
        ];
        Kept::new(vars, rows, Some(json!({"communities": 2})), 7)
    }

    #[test]
    fn the_answer_holds_the_listed_nodes_with_their_community_sizes() {
        let k = communities();
        let view = ViewOpts {
            nodes: Some(vec![
                "http://ex/d".into(),
                "http://ex/a".into(),
                "http://ex/a".into(),
                "http://ex/hidden".into(),
            ]),
            ..Default::default()
        };
        let a = k.answer(&view, true, usize::MAX);
        assert_eq!(
            a["head"]["vars"],
            json!(["node", "community", "communitySize"])
        );
        let b = a["results"]["bindings"].as_array().unwrap();
        // listed order, each node once, and nothing for a node the projection lacks
        assert_eq!(b.len(), 2);
        assert_eq!(b[0]["node"]["value"], "http://ex/d");
        assert_eq!(b[0]["communitySize"]["value"], "1");
        assert_eq!(b[1]["communitySize"]["value"], "3");
        assert_eq!(a["run"]["rows"], 4);
        assert_eq!(a["run"]["cached"], true);
        assert_eq!(a["summary"]["communities"], 2);
    }

    #[test]
    fn top_orders_by_the_score_and_puts_missing_values_last() {
        let vars = vec!["node".into(), "score".into()];
        let rows = vec![
            vec![iri("a"), dbl(0.1)],
            vec![iri("b"), None],
            vec![iri("c"), dbl(0.7)],
            vec![iri("d"), dbl(0.3)],
        ];
        let k = Kept::new(vars, rows, None, 1);
        let view = ViewOpts {
            top: Some(3),
            ..Default::default()
        };
        let a = k.answer(&view, false, usize::MAX);
        let names: Vec<&str> = a["results"]["bindings"]
            .as_array()
            .unwrap()
            .iter()
            .map(|b| b["node"]["value"].as_str().unwrap())
            .collect();
        assert_eq!(names, ["http://ex/c", "http://ex/d", "http://ex/a"]);
        assert_eq!(a["head"]["vars"], json!(["node", "score"]));
        // the row limit cuts the answer and says so
        let a = k.answer(&ViewOpts::default(), false, 2);
        assert_eq!(a["run"]["truncated"], true);
        assert_eq!(a["run"]["returned"], 2);
    }

    #[test]
    fn hierarchical_sizes_count_per_level() {
        let vars = vec!["node".into(), "community".into(), "level".into()];
        let rows = vec![
            vec![iri("a"), int(1), int(0)],
            vec![iri("b"), int(1), int(0)],
            vec![iri("a"), int(1), int(1)],
            vec![iri("b"), int(2), int(1)],
        ];
        let k = Kept::new(vars, rows, None, 1);
        let view = ViewOpts {
            nodes: Some(vec!["http://ex/a".into()]),
            ..Default::default()
        };
        let a = k.answer(&view, false, usize::MAX);
        let b = a["results"]["bindings"].as_array().unwrap();
        assert_eq!(b.len(), 2, "a row for each level");
        assert_eq!(b[0]["communitySize"]["value"], "2");
        assert_eq!(b[1]["communitySize"]["value"], "1");
    }

    #[test]
    fn view_members_are_split_and_checked() {
        let mut j =
            json!({"algorithm": "pageRank", "nodes": ["<http://ex/a>"], "keep": true, "top": 5});
        let v = ViewOpts::split(&mut j).ok().flatten().unwrap();
        assert_eq!(v.nodes, Some(vec!["http://ex/a".to_string()]));
        assert!(v.keep && !v.background);
        assert_eq!(v.top, Some(5));
        assert_eq!(j, json!({"algorithm": "pageRank"}));
        let mut j = json!({"algorithm": "pageRank"});
        assert!(matches!(ViewOpts::split(&mut j), Ok(None)));
        for bad in [
            json!({"async": true}),
            json!({"keep": "yes"}),
            json!({"nodes": "http://ex/a"}),
            json!({"nodes": ["not an iri"]}),
            json!({"nodes": [1]}),
            json!({"top": 0}),
            json!({"top": 20001}),
            json!({"nodes": vec!["http://ex/a"; MAX_NODES + 1]}),
        ] {
            let mut j = bad.clone();
            assert!(ViewOpts::split(&mut j).is_err(), "{bad}");
        }
    }

    fn key(commit: u64, view: &str) -> Key {
        Key {
            branch: uuid::Uuid::nil(),
            commit,
            view: view.into(),
            extra: String::new(),
            request: "view-tests".into(),
        }
    }

    #[test]
    fn the_cache_is_keyed_bounded_and_charged_to_the_pool() {
        let cache = ViewCache::new();
        let k = Arc::new(communities());
        assert!(cache.put(None, key(u64::MAX, "a"), k.clone()));
        assert!(cache.get(&key(u64::MAX, "a")).is_some());
        assert!(cache.get(&key(u64::MAX, "b")).is_none(), "another view");
        assert!(
            cache.get(&key(u64::MAX - 1, "a")).is_none(),
            "another commit"
        );
        for i in 0..(MAX_ENTRIES as u64 + 3) {
            assert!(cache.put(None, key(i, "bound"), k.clone()));
        }
        assert_eq!(cache.stats().0, MAX_ENTRIES);
        assert!(
            cache.get(&key(0, "bound")).is_none(),
            "the oldest went first"
        );
        // a pool holds the result's bytes while it is kept, and gets them back after
        let pool = sparkles::sparql::pool::QueryPool::new(64 << 20);
        {
            let cache = ViewCache::new();
            assert!(cache.put(Some(&pool), key(1, "pool"), Arc::new(communities())));
            assert!(pool.granted() >= k.bytes);
            // the result holds the grant, so it is given back when the cache drops it
        }
        assert_eq!(pool.granted(), 0);
        // a pool without room keeps nothing
        let held = pool.try_grant(64 << 20).unwrap();
        let cache = ViewCache::new();
        assert!(!cache.put(Some(&pool), key(2, "pool"), Arc::new(communities())));
        assert!(cache.get(&key(2, "pool")).is_none());
        drop(held);
        // a newer result takes the memory of older ones when the pool is short
        // (8 MiB: half of it is one 4 MiB granule, room for one result)
        let small = sparkles::sparql::pool::QueryPool::new(8 << 20);
        assert!(cache.put(Some(&small), key(3, "pool"), Arc::new(communities())));
        assert!(cache.put(Some(&small), key(4, "pool"), Arc::new(communities())));
        assert!(cache.get(&key(3, "pool")).is_none());
        assert!(cache.get(&key(4, "pool")).is_some());
        // a result larger than the whole cache is never kept
        let mut big = communities();
        big.bytes = MAX_BYTES + 1;
        assert!(!cache.put(None, key(5, "pool"), Arc::new(big)));
    }

    fn sized(mb: u64) -> Arc<Kept> {
        let mut k = communities();
        k.bytes = mb << 20;
        Arc::new(k)
    }

    #[test]
    fn the_cache_holds_at_most_half_the_pool() {
        let pool = sparkles::sparql::pool::QueryPool::new(64 << 20);
        let cache = ViewCache::new();
        assert!(!cache.put(Some(&pool), key(1, "half"), sized(40)));
        assert_eq!(pool.granted(), 0);
        assert!(cache.put(Some(&pool), key(2, "half"), sized(20)));
        assert!(cache.put(Some(&pool), key(3, "half"), sized(20)));
        assert!(pool.granted() <= 32 << 20, "{}", pool.granted());
        assert!(
            cache.get(&key(2, "half")).is_none(),
            "the older one gave way"
        );
        assert!(cache.get(&key(3, "half")).is_some());
    }

    #[test]
    fn a_reader_keeps_a_dropped_result_charged() {
        let pool = sparkles::sparql::pool::QueryPool::new(64 << 20);
        let cache = ViewCache::new();
        assert!(cache.put(Some(&pool), key(1, "reader"), sized(20)));
        let reading = cache.get(&key(1, "reader")).unwrap();
        // a newer result pushes it out of the cache while it is read
        assert!(cache.put(Some(&pool), key(2, "reader"), sized(20)));
        assert!(cache.get(&key(1, "reader")).is_none());
        assert!(pool.granted() >= 40 << 20, "{}", pool.granted());
        drop(reading);
        assert!(pool.granted() < 40 << 20, "{}", pool.granted());
    }
}
