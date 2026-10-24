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
//! size, drops the oldest first, and charges each result to the server memory pool for as
//! long as it is kept. A result the pool has no room for is answered but not kept.

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
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
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
        let term = |t: &Option<Term>| match t {
            Some(Term::NamedNode(n)) => n.as_str().len() as u64,
            Some(Term::BlankNode(b)) => b.as_str().len() as u64,
            Some(Term::Literal(l)) => l.value().len() as u64 + 16,
            Some(Term::Triple(_)) => 256,
            None => 0,
        };
        let mut bytes = 0u64;
        for r in &rows {
            bytes += 24 + cell * r.len() as u64 + r.iter().map(term).sum::<u64>();
        }
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

/// Run `req` at `snap` under `opts` and keep every row.
fn compute(
    ds: &Dataset,
    snap: Arc<Snapshot>,
    req: &AlgoRequest,
    opts: &QueryOptions,
) -> sparkles::Result<Kept> {
    let commit = snap.commit;
    let q = ds.dataset.algorithms().query_text_with_summary(req)?;
    let r = sparkles::sparql::query(snap, &q, opts)?;
    let mut vars = r.vars.clone();
    let mut rows = r.rows();
    drop(r);
    // the summary is the last column, the same literal in every row
    let mut summary = None;
    if vars.last().map(String::as_str) == Some("summary") {
        vars.pop();
        for row in &mut rows {
            let s = row.pop().flatten();
            if summary.is_none()
                && let Some(Term::Literal(l)) = s
            {
                summary = Some(serde_json::from_str(l.value()).unwrap_or(J::from(l.value())));
            }
        }
    }
    Ok(Kept::new(vars, rows, summary, commit))
}

// ---------------------------------------------------------------- the cache ------

struct Entry {
    key: Key,
    kept: Arc<Kept>,
    /// the pool memory the result holds while it is kept
    _grant: Option<PoolGrant>,
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
    fn put(
        &self,
        pool: Option<&Arc<sparkles::sparql::pool::QueryPool>>,
        key: Key,
        kept: Arc<Kept>,
    ) -> bool {
        let bytes = kept.bytes;
        if bytes > MAX_BYTES {
            REFUSED.fetch_add(1, Ordering::Relaxed);
            return false;
        }
        let mut c = self.entries.lock();
        if let Some(i) = c.iter().position(|e| e.key == key) {
            c.remove(i);
        }
        let total = |c: &VecDeque<Entry>| c.iter().map(|e| e.kept.bytes).sum::<u64>();
        while c.len() >= MAX_ENTRIES || (!c.is_empty() && total(&c) + bytes > MAX_BYTES) {
            c.pop_front();
        }
        let grant = match pool {
            None => None,
            Some(pool) => loop {
                match pool.try_grant(bytes).filter(|g| g.granted() >= bytes) {
                    Some(g) => break Some(g),
                    // the oldest results give their memory back for a newer one
                    None if !c.is_empty() => {
                        c.pop_front();
                    }
                    None => {
                        REFUSED.fetch_add(1, Ordering::Relaxed);
                        return false;
                    }
                }
            },
        };
        c.push_back(Entry {
            key,
            kept,
            _grant: grant,
        });
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
/// The callers of each background run's task: the one that started it first. They may
/// follow the task even when their view of the dataset is restricted, and the first may
/// cancel it.
static CALLERS: Mutex<VecDeque<(String, Vec<String>)>> = parking_lot::const_mutex(VecDeque::new());
/// Tasks whose callers are remembered at most.
const MAX_CALLERS: usize = 256;

fn who(p: &Principal) -> String {
    format!("{:?}\u{1f}{}", p.kind, p.name)
}

/// A task's key among every server's: task ids count per server.
fn task_key(st: &AppState, id: &str) -> String {
    format!("{:p}/{id}", st)
}

fn add_caller(st: &AppState, id: &str, p: &Principal) {
    let id = &task_key(st, id);
    let me = who(p);
    let mut c = CALLERS.lock();
    if let Some((_, v)) = c.iter_mut().find(|(t, _)| t == id) {
        if !v.contains(&me) {
            v.push(me);
        }
        return;
    }
    if c.len() >= MAX_CALLERS {
        c.pop_front();
    }
    c.push_back((id.to_string(), vec![me]));
}

/// Whether `p` started or joined the background run of task `id`.
pub(crate) fn follows(st: &AppState, id: &str, p: &Principal) -> bool {
    let id = task_key(st, id);
    let me = who(p);
    CALLERS
        .lock()
        .iter()
        .any(|(t, v)| *t == id && v.contains(&me))
}

/// Whether `p` started the background run of task `id`.
pub(crate) fn started(st: &AppState, id: &str, p: &Principal) -> bool {
    let id = task_key(st, id);
    let me = who(p);
    CALLERS
        .lock()
        .iter()
        .any(|(t, v)| *t == id && v.first() == Some(&me))
}

/// Datasets whose background runs wait before they compute, so that tests can join and
/// cancel them while they run.
#[cfg(test)]
pub(crate) static PAUSED: Mutex<Vec<String>> = parking_lot::const_mutex(Vec::new());

/// Removes a run from [`RUNNING`] when its task ends, however it ends.
struct Running(Key);

impl Drop for Running {
    fn drop(&mut self) {
        RUNNING.lock().retain(|(k, _)| *k != self.0);
    }
}

// ------------------------------------------------------------- the route ------

/// The part of the key that names the request: the request without its view-only
/// members, and the definition of a named projection, which can change without a commit.
fn request_key(ds: &Dataset, req: &AlgoRequest) -> String {
    let mut k = serde_json::to_string(req).unwrap_or_default();
    if let Some(ProjectionRef::Named(n)) = &req.projection {
        let def = ds.dataset.algorithms().projections().get(n);
        k.push('\u{1f}');
        k.push_str(
            &def.map(|s| serde_json::to_string(&s).unwrap_or_default())
                .unwrap_or_default(),
        );
    }
    k
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
    crate::auth::restrict(&mut opts, &p, &ds.name, Endpoint::Query);
    // the whole result is computed, and the answer is cut to the server's row limit
    let max_rows = opts.max_rows.take().unwrap_or(usize::MAX);
    let request = request_key(&ds, &req);
    if view.background {
        return background(st, ds, p, params, req, view, at, opts, request, max_rows).await;
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
            return Ok(Json(kept.answer(&view, true, max_rows)).into_response());
        }
        MISSES.fetch_add(1, Ordering::Relaxed);
        let kept = Arc::new(
            compute(&ds, snap, &req, &opts)
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
        Ok(Json(kept.answer(&view, false, max_rows)).into_response())
    })
    .await
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
    request: String,
    max_rows: usize,
) -> ApiResult {
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
            return Ok(Json(kept.answer(&view, true, max_rows)).into_response());
        }
        let mut running = RUNNING.lock();
        if let Some((_, id)) = running.iter().find(|(k, _)| *k == key)
            && let Some(task) = st.tasks.lock().iter().find(|t| t.id == *id).cloned()
        {
            JOINS.fetch_add(1, Ordering::Relaxed);
            add_caller(&st, id, &p);
            return Ok(task_answer(&task, true));
        }
        // a result kept between the check above and taking the lock
        if let Some(kept) = get(&key) {
            HITS.fetch_add(1, Ordering::Relaxed);
            return Ok(Json(kept.answer(&view, true, max_rows)).into_response());
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
        add_caller(&st, &id, &p);
        let state = st.clone();
        let d = ds.clone();
        let task = st.start_task_opts(id, TASK, &ds.name, None, true, move |h| {
            let _running = Running(key.clone());
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
            let kept = Arc::new(compute(&d, snap, &req, &opts)?);
            opts.grant = None;
            h.progress(0.95, "keeping the result");
            let rows = kept.rows.len();
            let commit = kept.commit;
            let summary = kept.summary.clone();
            let stored = put(state.catalog.pool(), key, kept);
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
            assert!(cache.put(Some(&pool), key(1, "pool"), k.clone()));
            assert!(pool.granted() >= k.bytes);
        }
        assert_eq!(pool.granted(), 0);
        // a pool without room keeps nothing
        let held = pool.try_grant(64 << 20).unwrap();
        let cache = ViewCache::new();
        assert!(!cache.put(Some(&pool), key(2, "pool"), k.clone()));
        assert!(cache.get(&key(2, "pool")).is_none());
        drop(held);
        // a newer result takes the memory of older ones when the pool is short
        let small = sparkles::sparql::pool::QueryPool::new(1 << 20);
        assert!(cache.put(Some(&small), key(3, "pool"), k.clone()));
        assert!(cache.put(Some(&small), key(4, "pool"), k.clone()));
        assert!(cache.get(&key(3, "pool")).is_none());
        assert!(cache.get(&key(4, "pool")).is_some());
        // a result larger than the whole cache is never kept
        let mut big = communities();
        big.bytes = MAX_BYTES + 1;
        assert!(!cache.put(None, key(5, "pool"), Arc::new(big)));
    }
}
