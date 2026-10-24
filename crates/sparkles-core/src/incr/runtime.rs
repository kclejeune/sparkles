//! The per-dataset runtime of incremental views and live queries (spec F13 §3.3, §8).
//!
//! A runtime keeps the circuits of a dataset's views and live queries. Views and live
//! queries that ask the same question with the same access share one circuit, a
//! [`Cell`]. After each commit a round ([`Runtime::round`]) reads the net changes
//! since each circuit's commit and runs one pass per circuit, sharing the changes
//! between circuits at the same commit. A circuit whose footprint the changes miss is
//! skipped after one hash probe per changed quad.
//!
//! The host (the embedding library or the server) runs the rounds on a worker thread
//! ([`Runtime::spawn_worker`]) and tells the runtime how to resolve a view's principal
//! and the dataset's prefixes ([`Host`]). Until a host is set, views are not attached,
//! so a view never reads with access its principal does not have.

use super::circuit::{Advance, Circuit, DeltaSet};
use super::compile::{Kind, compile};
use super::inference::Inference;
use super::live::{Count, LiveEvent, LiveOptions, LiveQuery, LiveRow, Shared};
use super::ops::PlanNode;
use super::persist;
use super::views::{self, ViewDef, ViewInfo, ViewState};
use super::zset::Changes;
use crate::access::GraphAccess;
use crate::error::{Error, Result};
use crate::sparql::viewsvc::{ViewRead, ViewRows, ViewSource};
use crate::store::{DeltaSubscription, Store};
use crate::task::Control;
use oxrdf::Term;
use parking_lot::{Condvar, Mutex, RwLock};
use rustc_hash::FxHashMap;
use sha2::{Digest, Sha256};
use spargebra::Query;
use std::collections::{BTreeMap, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Weak};
use std::time::{Duration, Instant};

/// The version of the compiler, part of a checkpoint's fingerprint: a state written
/// by another compiler is not loaded.
const COMPILER: &str = "f13-phase1-2";
/// The branch whose state Phase 1 keeps.
const BRANCH: &str = "main";
/// The most rows a circuit's diff history keeps for resumed live queries.
const HISTORY_ROWS: usize = 1_000_000;
/// The commits of history a circuit with views and no live queries keeps, so that a
/// query whose snapshot a pass overtook reads the view at its own commit.
const VIEW_HISTORY_COMMITS: usize = 16;
/// The upper bounds of the pass-time histogram, in seconds.
pub const PASS_BUCKETS: [f64; 9] = [0.001, 0.005, 0.01, 0.05, 0.1, 0.5, 1.0, 5.0, 30.0];

/// The runtime's settings (spec F13 §2.3, §3.4, §8.2, §8.4).
#[derive(Clone, Debug)]
pub struct RuntimeOptions {
    /// how often a changed view's state is checkpointed (`checkpointSeconds`)
    pub checkpoint_seconds: u64,
    /// a pass running longer suspends its view with `pass-timeout`
    pub pass_timeout: Duration,
    /// how long a read of a view behind the query's commit waits (`viewWaitMs`)
    pub view_wait: Duration,
    pub max_live: usize,
    pub max_live_per_principal: usize,
    /// the budget of all views' state together (`--views-memory`, `None`: none)
    pub views_memory: Option<u64>,
    /// a re-evaluation whose change has more rows reaches live queries as a `reset`
    pub reset_rows: usize,
    /// the default budget of a live query's circuit
    pub live_memory_bytes: u64,
    /// the most commits of diffs a circuit keeps for resumed live queries
    pub history_commits: usize,
}

impl Default for RuntimeOptions {
    fn default() -> RuntimeOptions {
        RuntimeOptions {
            checkpoint_seconds: 60,
            pass_timeout: Duration::from_secs(30),
            view_wait: Duration::from_millis(2000),
            max_live: 1000,
            max_live_per_principal: 20,
            views_memory: None,
            reset_rows: 100_000,
            live_memory_bytes: views::DEFAULT_MEMORY_BYTES,
            history_commits: 256,
        }
    }
}

/// What the runtime asks of its host.
pub trait Host: Send + Sync {
    /// The view of the dataset that `principal` reads (`None`: every quad). `None` as
    /// the principal is a view without one, which reads every quad.
    fn access(&self, principal: Option<&str>) -> Result<Option<Arc<GraphAccess>>>;
    /// The dataset's declared prefixes (C20), which a view's query may use.
    fn prefixes(&self) -> Vec<(String, String)> {
        Vec::new()
    }
    /// The text of the current version of stored query `name` (C16).
    fn stored_query(&self, name: &str) -> Result<String>;
    /// A number that changes when principals' access may have changed (a policy
    /// reload): the views then resolve their principals again.
    fn epoch(&self) -> u64 {
        0
    }
    /// What makes the dataset's queries read more than the stored quads: RDFS on read,
    /// or materialized inferences added to the default graph. Views and live queries
    /// then evaluate the whole query again with it on each relevant commit (spec F13
    /// §4.5), and a change of it switches them between the two modes.
    fn inference(&self) -> Option<Inference> {
        None
    }
}

/// A host without access control: every principal reads every quad, and stored
/// queries are not available.
pub struct OpenHost;

impl Host for OpenHost {
    fn access(&self, _: Option<&str>) -> Result<Option<Arc<GraphAccess>>> {
        Ok(None)
    }
    fn stored_query(&self, name: &str) -> Result<String> {
        Err(super::error(
            "invalid-view",
            format!("no stored query {name:?} here"),
        ))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Status {
    Building,
    Ready,
    /// `code` is `pass-timeout`, `over-budget` or `view-failed`
    Failed {
        code: String,
        message: String,
    },
}

/// What the routes report of a circuit without taking its lock.
#[derive(Clone, Debug)]
struct Info {
    status: Status,
    applied: u64,
    rows: u64,
    bytes: u64,
    passes: u64,
    skipped: u64,
    pass_time: Duration,
    recompute_time: Duration,
    columns: Vec<String>,
    plan: PlanNode,
    /// why the whole query is evaluated again, when it is
    whole: Option<String>,
}

struct Hist {
    from: u64,
    to: u64,
    rows: Arc<[(Vec<Option<Term>>, i64)]>,
}

struct Sub {
    shared: Weak<Shared>,
    budget: u64,
    /// the commit of the subscriber's latest event, which its next diff starts from
    last: u64,
}

/// A circuit's state, shared by its views and live queries.
pub(crate) struct Slot {
    circuit: Circuit,
    query: Query,
    /// why the access makes the whole query be evaluated again, when it does
    whole: Option<String>,
    vars: Arc<[String]>,
    history: VecDeque<Hist>,
    history_rows: usize,
    subs: Vec<Sub>,
    /// the views attached to the circuit
    views: usize,
    /// set by a switch of the inference until the evaluation that follows it: the change
    /// that evaluation publishes leads from one mode to the other, and diffs from before
    /// it must not rewind a view across the switch
    rebased: bool,
}

impl Slot {
    /// Why the whole query is evaluated again: protections that depend on the data, or
    /// the dataset's inference.
    fn whole_reason(&self, inference: Option<&Inference>) -> Option<String> {
        self.whole
            .clone()
            .or_else(|| inference.map(|i| i.reason().to_string()))
    }

    /// A circuit of the slot's query, access and inference without state.
    fn fresh(&self, inference: Option<Arc<Inference>>) -> Result<Circuit> {
        let whole = self.whole_reason(inference.as_deref());
        let mut c = Circuit::new(
            compile(&self.query, whole.as_deref())?,
            self.circuit.access.clone(),
        );
        c.max_memory_bytes = self.circuit.max_memory_bytes;
        c.inference = inference;
        Ok(c)
    }

    /// The fingerprint of the circuit's checkpoints: the cell's key, and the inference
    /// its state was built with.
    fn fingerprint(&self, key: u64) -> u64 {
        match &self.circuit.inference {
            None => key,
            Some(i) => {
                let mut h = Sha256::new();
                h.update(key.to_le_bytes());
                h.update(i.key().as_bytes());
                u64::from_le_bytes(h.finalize()[..8].try_into().expect("8 bytes"))
            }
        }
    }

    fn live_rows(&self, rows: Vec<(Vec<Option<Term>>, i64)>) -> Vec<LiveRow> {
        rows.into_iter()
            .map(|(terms, w)| LiveRow {
                terms,
                count: w.unsigned_abs(),
            })
            .collect()
    }

    fn event(&self, reset: bool) -> LiveEvent {
        let ask = self.circuit.kind == Kind::Ask;
        LiveEvent::Snapshot {
            commit: self.circuit.applied,
            vars: self.vars.clone(),
            rows: if ask {
                Arc::from(Vec::new())
            } else {
                self.live_rows(self.circuit.rows()).into()
            },
            value: ask.then(|| self.circuit.result.total() > 0),
            reset,
        }
    }

    /// The whole result as a `reset` event for the subscriber of `shared`, whose next
    /// diff then starts from the circuit's commit.
    pub(crate) fn resync(&mut self, shared: &Arc<Shared>) -> LiveEvent {
        let applied = self.circuit.applied;
        for s in &mut self.subs {
            if std::ptr::eq(s.shared.as_ptr(), Arc::as_ptr(shared)) {
                s.last = applied;
            }
        }
        self.event(true)
    }

    fn alive(&mut self) -> bool {
        self.subs.retain(|s| s.shared.strong_count() > 0);
        if self.subs.is_empty() && self.views == 0 {
            self.history.clear();
            self.history_rows = 0;
        }
        !self.subs.is_empty() || self.views > 0
    }

    fn remember(&mut self, from: u64, to: u64, rows: Arc<[(Vec<Option<Term>>, i64)]>, max: usize) {
        let max = if self.subs.is_empty() {
            if self.views == 0 {
                return;
            }
            VIEW_HISTORY_COMMITS
        } else {
            max
        };
        if rows.is_empty()
            && let Some(last) = self.history.back_mut()
            && last.rows.is_empty()
            && last.to == from
        {
            last.to = to;
            return;
        }
        self.history_rows += rows.len();
        self.history.push_back(Hist { from, to, rows });
        while self.history.len() > max.max(1) || self.history_rows > HISTORY_ROWS {
            match self.history.pop_front() {
                Some(h) => self.history_rows -= h.rows.len(),
                None => break,
            }
        }
    }

    /// The net change from commit `after` to the circuit's commit from the history.
    fn history_since(&self, after: u64) -> Option<Vec<(Vec<Option<Term>>, i64)>> {
        let start = self.history.iter().position(|h| {
            h.from == after || (h.rows.is_empty() && h.from <= after && after < h.to)
        })?;
        let mut at = after;
        let mut parts = Vec::new();
        for h in self.history.iter().skip(start) {
            if h.from > at {
                return None;
            }
            parts.push(&h.rows);
            at = h.to;
        }
        if at != self.circuit.applied {
            return None;
        }
        Some(net(parts.iter().flat_map(|r| r.iter().cloned())))
    }
}

/// The net multiset change of term rows.
fn net(rows: impl Iterator<Item = (Vec<Option<Term>>, i64)>) -> Vec<(Vec<Option<Term>>, i64)> {
    let mut m: FxHashMap<Vec<Option<Term>>, i64> = FxHashMap::default();
    let mut order = Vec::new();
    for (r, w) in rows {
        match m.get_mut(&r) {
            Some(n) => *n += w,
            None => {
                order.push(r.clone());
                m.insert(r, w);
            }
        }
    }
    order
        .into_iter()
        .filter_map(|r| {
            let w = m[&r];
            (w != 0).then_some((r, w))
        })
        .collect()
}

/// A circuit with what the routes read of it.
pub(crate) struct Cell {
    key: u64,
    pub(crate) state: Mutex<Slot>,
    info: Mutex<Info>,
    /// live queries joining the cell, which keep it from collection until they are
    /// among its subscribers
    pending: AtomicUsize,
}

/// A live query's claim on a cell until it subscribes.
struct Joining<'a>(&'a Cell);

impl Drop for Joining<'_> {
    fn drop(&mut self) {
        self.0.pending.fetch_sub(1, Ordering::AcqRel);
    }
}

struct ViewEntry {
    def: ViewDef,
    cell: Option<Arc<Cell>>,
    access: Option<Arc<GraphAccess>>,
    /// suspended by its budget until a rebuild
    suspended: Option<String>,
    /// why the view could not be attached (its query, its principal)
    error: Option<String>,
    checkpointed: u64,
}

#[derive(Default)]
struct Registry {
    cells: FxHashMap<u64, Arc<Cell>>,
    /// the dataset's inference that every circuit is compiled for
    inference: Option<Arc<Inference>>,
    views: BTreeMap<String, ViewEntry>,
    /// why the definitions could not be read; views cannot be changed until it is fixed
    broken: Option<String>,
}

#[derive(Default)]
struct Counters {
    passes: AtomicU64,
    skipped: AtomicU64,
    resets: AtomicU64,
    live_events: AtomicU64,
    recompute_ns: AtomicU64,
    pass_ns: AtomicU64,
    pass_hist: [AtomicU64; PASS_BUCKETS.len() + 1],
    checkpoints: AtomicU64,
    loaded: AtomicU64,
}

/// A view's numbers for the metrics.
#[derive(Clone, Debug)]
pub struct ViewStat {
    pub name: String,
    pub lag_commits: u64,
    pub state_bytes: u64,
}

/// The runtime's counters (spec F13 §8.2).
#[derive(Clone, Debug, Default)]
pub struct RuntimeStats {
    pub views: Vec<ViewStat>,
    pub live_queries: usize,
    pub circuits: usize,
    pub passes: u64,
    pub skipped: u64,
    pub resets: u64,
    pub live_events: u64,
    pub recompute_seconds: f64,
    pub pass_seconds_sum: f64,
    /// the count of passes at or under each bound of [`PASS_BUCKETS`], then all
    pub pass_buckets: Vec<u64>,
    pub checkpoints: u64,
    /// views whose state was loaded from a checkpoint
    pub loaded: u64,
}

/// The views and live queries of one dataset.
pub struct Runtime {
    opts: RwLock<RuntimeOptions>,
    root: Option<PathBuf>,
    dataset: uuid::Uuid,
    host: RwLock<Option<Arc<dyn Host>>>,
    reg: Mutex<Registry>,
    /// a counter of finished rounds, which readers waiting for a view watch
    epoch: Mutex<u64>,
    cv: Condvar,
    kick: Arc<tokio::sync::Notify>,
    subscription: Mutex<Option<DeltaSubscription>>,
    live_total: Arc<AtomicUsize>,
    live_by: Arc<Mutex<FxHashMap<String, usize>>>,
    counters: Counters,
    last_checkpoint: Mutex<Instant>,
    closed: AtomicBool,
    /// the host's epoch the attached views resolved their principals under
    host_epoch: AtomicU64,
    /// the worker holds the dataset for a round
    busy: AtomicBool,
    /// the highest commit a circuit's result has reflected
    seen: AtomicU64,
    /// `seen` at the latest switch of the inference (0: none). The switch changes results
    /// without a commit, so a live query resumed at or before it gets the whole result,
    /// even when its circuit was collected and created again since.
    switched: AtomicU64,
}

impl std::fmt::Debug for Runtime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Runtime").field("root", &self.root).finish()
    }
}

/// Whether `access` has protections that depend on the data, with the reason the
/// whole query is then evaluated again (spec F13 §6.2).
pub fn whole_reason(access: Option<&Arc<GraphAccess>>) -> Option<&'static str> {
    access
        .and_then(|a| a.triples.as_ref())
        .filter(|t| t.hides() && !t.state_independent())
        .map(|_| "protections that depend on the data")
}

/// A stable hash of what `access` lets a reader see: equal for equal grants. Guards a
/// request decides at its snapshot count as keeping their graphs out, as they do in a
/// circuit ([`Circuit::new`]).
pub fn access_fingerprint(access: Option<&Arc<GraphAccess>>) -> u64 {
    let settled = access.map(|a| a.settled());
    match settled.as_ref() {
        None => 0,
        Some(a) if a.reads_everything() => 0,
        Some(a) => {
            let mut h = Sha256::new();
            h.update(a.read_key().as_bytes());
            h.update([0]);
            if let Some(t) = a.triples.as_ref().filter(|t| t.hides()) {
                h.update(format!("{:?}", t.rules).as_bytes());
                if !t.state_independent() {
                    h.update(format!("{:?}", t.caller).as_bytes());
                }
            }
            u64::from_le_bytes(h.finalize()[..8].try_into().expect("8 bytes")) | 1
        }
    }
}

/// Whether a reader with `caller` may read a view whose principal reads `view`: the
/// caller reads everything, or reads exactly what the principal reads (spec F13 §6.1).
/// Comparing grant sets for containment is left to Phase 2; equal grants and
/// unrestricted callers cover every case Phase 1 needs.
pub fn covers(caller: Option<&Arc<GraphAccess>>, view: Option<&Arc<GraphAccess>>) -> bool {
    let c = access_fingerprint(caller);
    c == 0 || c == access_fingerprint(view)
}

/// The query's text with the variables the parser names at random (aggregates' and
/// others' that the source `text` does not hold) renamed in order of appearance, so
/// that one query parsed twice gives one text.
fn normalized(q: &Query, text: &str) -> String {
    let word = |c: char| c.is_alphanumeric() || c == '_';
    let in_source = |name: &str| {
        ['?', '$'].iter().any(|p| {
            let pat = format!("{p}{name}");
            text.match_indices(&pat).any(|(i, _)| {
                text[i + pat.len()..]
                    .chars()
                    .next()
                    .is_none_or(|c| !word(c))
            })
        })
    };
    let shown = q.to_string();
    let mut out = String::with_capacity(shown.len());
    let mut names: Vec<String> = Vec::new();
    let mut rest = shown.as_str();
    while let Some(i) = rest.find('?') {
        out.push_str(&rest[..i]);
        let after = &rest[i + 1..];
        let len = after.find(|c: char| !word(c)).unwrap_or(after.len());
        let name = &after[..len];
        if len == 0 || in_source(name) {
            out.push('?');
            out.push_str(name);
        } else {
            let n = match names.iter().position(|x| x == name) {
                Some(n) => n,
                None => {
                    names.push(name.to_string());
                    names.len() - 1
                }
            };
            out.push_str(&format!("?_v{n}"));
        }
        rest = &after[len..];
    }
    out.push_str(rest);
    out
}

fn key_of(q: &Query, text: &str, access: Option<&Arc<GraphAccess>>, whole: Option<&str>) -> u64 {
    let mut h = Sha256::new();
    h.update(COMPILER.as_bytes());
    h.update([0]);
    h.update(normalized(q, text).as_bytes());
    h.update([0]);
    h.update(access_fingerprint(access).to_le_bytes());
    h.update(whole.unwrap_or("").as_bytes());
    u64::from_le_bytes(h.finalize()[..8].try_into().expect("8 bytes"))
}

fn failure(e: &Error) -> (String, String) {
    let code = match e {
        Error::Timeout | Error::Cancelled => "pass-timeout",
        Error::BudgetExceeded(_) => "over-budget",
        _ => "view-failed",
    };
    (code.to_string(), e.to_string())
}

impl Runtime {
    /// A runtime for the dataset at `root` (`None`: in memory, without checkpoints)
    /// with the definitions found there. Views are attached once a host is set.
    pub fn open(root: Option<PathBuf>, dataset: uuid::Uuid, opts: RuntimeOptions) -> Arc<Runtime> {
        let mut reg = Registry::default();
        if let Some(r) = &root {
            match views::read_all(r) {
                Ok(defs) => {
                    for (name, def) in defs {
                        reg.views.insert(
                            name,
                            ViewEntry {
                                def,
                                cell: None,
                                access: None,
                                suspended: None,
                                error: None,
                                checkpointed: 0,
                            },
                        );
                    }
                }
                Err(e) => {
                    tracing::error!(
                        target: "sparkles::incr::runtime",
                        "views of {}: {e}; they cannot be changed until it is fixed",
                        r.display()
                    );
                    reg.broken = Some(e.to_string());
                }
            }
        }
        Arc::new(Runtime {
            opts: RwLock::new(opts),
            root,
            dataset,
            host: RwLock::new(None),
            reg: Mutex::new(reg),
            epoch: Mutex::new(0),
            cv: Condvar::new(),
            kick: Arc::new(tokio::sync::Notify::new()),
            subscription: Mutex::new(None),
            live_total: Arc::new(AtomicUsize::new(0)),
            live_by: Default::default(),
            counters: Counters::default(),
            last_checkpoint: Mutex::new(Instant::now()),
            closed: AtomicBool::new(false),
            host_epoch: AtomicU64::new(0),
            busy: AtomicBool::new(false),
            seen: AtomicU64::new(0),
            switched: AtomicU64::new(0),
        })
    }

    pub fn options(&self) -> RuntimeOptions {
        self.opts.read().clone()
    }

    /// Change the settings; circuits keep their budgets until they are built again.
    pub fn set_options(&self, opts: RuntimeOptions) {
        *self.opts.write() = opts;
    }

    /// Set how principals, prefixes and stored queries resolve. Views attach on the
    /// next round.
    pub fn set_host(&self, host: Arc<dyn Host>) {
        *self.host.write() = Some(host);
        self.kick();
    }

    pub fn has_host(&self) -> bool {
        self.host.read().is_some()
    }

    fn host(&self) -> Result<Arc<dyn Host>> {
        self.host.read().clone().ok_or_else(|| {
            super::error(
                "views-unavailable",
                "the views of this dataset have not started",
            )
        })
    }

    /// Whether the dataset defines views.
    pub fn has_views(&self) -> bool {
        !self.reg.lock().views.is_empty()
    }

    /// Wake the worker for a round.
    pub fn kick(&self) {
        self.kick.notify_one();
    }

    fn parse(&self, host: &dyn Host, text: &str) -> Result<Query> {
        crate::sparql::parse_query(text, None, &host.prefixes())
    }

    fn view_text(host: &dyn Host, def: &ViewDef) -> Result<String> {
        match (&def.query, &def.stored_query) {
            (Some(q), _) => Ok(q.clone()),
            (None, Some(n)) => host.stored_query(n),
            (None, None) => Err(super::error("invalid-view", "a view needs a query")),
        }
    }

    /// Compile `query` and report its plan with each operator's mode, without
    /// keeping it.
    pub fn explain(
        &self,
        query: &str,
        access: Option<Arc<GraphAccess>>,
    ) -> Result<(Kind, PlanNode)> {
        let host = self
            .host
            .read()
            .clone()
            .unwrap_or_else(|| Arc::new(OpenHost));
        let q = self.parse(host.as_ref(), query)?;
        let inference = host.inference();
        let whole = whole_reason(access.as_ref()).or(inference.as_ref().map(Inference::reason));
        let c = compile(&q, whole)?;
        let circuit = Circuit::new(c, access);
        Ok((circuit.kind, circuit.plan()))
    }

    fn ensure_subscription(&self, store: &Store) {
        let mut s = self.subscription.lock();
        if s.is_none() {
            *s = Some(store.subscribe_deltas());
        }
    }

    /// The cell of `q` read with `access`, created (and not built) when absent.
    fn cell_for(
        &self,
        reg: &mut Registry,
        q: Query,
        text: &str,
        access: Option<Arc<GraphAccess>>,
        max_memory_bytes: Option<u64>,
    ) -> Result<(Arc<Cell>, bool)> {
        let whole = whole_reason(access.as_ref()).map(str::to_string);
        let key = key_of(&q, text, access.as_ref(), whole.as_deref());
        if let Some(c) = reg.cells.get(&key) {
            return Ok((c.clone(), false));
        }
        let inference = reg.inference.clone();
        let mode = whole
            .clone()
            .or_else(|| inference.as_ref().map(|i| i.reason().to_string()));
        let compiled = compile(&q, mode.as_deref())?;
        let mut circuit = Circuit::new(compiled, access);
        circuit.max_memory_bytes = max_memory_bytes;
        circuit.inference = inference;
        let info = Info {
            status: Status::Building,
            applied: 0,
            rows: 0,
            bytes: 0,
            passes: 0,
            skipped: 0,
            pass_time: Duration::ZERO,
            recompute_time: Duration::ZERO,
            columns: circuit.columns(),
            plan: circuit.plan(),
            whole: mode,
        };
        let vars: Arc<[String]> = circuit.columns().into();
        let cell = Arc::new(Cell {
            key,
            state: Mutex::new(Slot {
                circuit,
                query: q,
                whole,
                vars,
                history: VecDeque::new(),
                history_rows: 0,
                subs: Vec::new(),
                views: 0,
                rebased: false,
            }),
            info: Mutex::new(info),
            pending: AtomicUsize::new(0),
        });
        reg.cells.insert(key, cell.clone());
        Ok((cell, true))
    }

    /// Attach view `name` to its circuit, loading its newest checkpoint into a new one.
    fn attach(&self, store: &Store, reg: &mut Registry, name: &str) -> Result<()> {
        let host = self.host()?;
        let Some(entry) = reg.views.get(name) else {
            return Ok(());
        };
        if entry.cell.is_some() || entry.suspended.is_some() {
            return Ok(());
        }
        let def = entry.def.clone();
        let text = Self::view_text(host.as_ref(), &def)?;
        let q = self.parse(host.as_ref(), &text)?;
        let access = host.access(def.principal.as_deref())?;
        let (cell, new) = self.cell_for(reg, q, &text, access.clone(), None)?;
        if new {
            self.ensure_subscription(store);
            if let Some(root) = &self.root {
                self.recover(store, &cell, &views::dir(root, name).join(BRANCH), name);
            }
        }
        cell.state.lock().views += 1;
        let entry = reg.views.get_mut(name).expect("checked above");
        entry.cell = Some(cell);
        entry.access = access;
        entry.error = None;
        Ok(())
    }

    /// Load the newest checkpoint of a view that this release reads into `cell`.
    fn recover(&self, store: &Store, cell: &Cell, dir: &std::path::Path, name: &str) {
        let snap = store.snapshot();
        for (commit, path) in persist::list(dir) {
            let Ok(bytes) = std::fs::read(&path) else {
                continue;
            };
            let (h, body) = match persist::decode(&bytes) {
                Ok(x) => x,
                Err(e) => {
                    tracing::warn!(
                        target: "sparkles::incr::runtime",
                        "view {name}: checkpoint {}: {e}; it is left in place",
                        path.display()
                    );
                    continue;
                }
            };
            let mut st = cell.state.lock();
            if h.dataset != self.dataset
                || h.fingerprint != st.fingerprint(cell.key)
                || commit > snap.commit
            {
                continue;
            }
            match st.circuit.decode(body, &snap) {
                Ok(()) => {
                    st.circuit.applied = h.commit;
                    self.seen.fetch_max(h.commit, Ordering::AcqRel);
                    self.counters.loaded.fetch_add(1, Ordering::Relaxed);
                    let mut info = cell.info.lock();
                    info.status = Status::Ready;
                    info.applied = h.commit;
                    info.rows = st.circuit.result.total().max(0) as u64;
                    info.bytes = st.circuit.bytes();
                    tracing::info!(
                        target: "sparkles::incr::runtime",
                        "view {name}: loaded its state at commit {}; {} commits to apply",
                        h.commit,
                        snap.commit - h.commit
                    );
                    return;
                }
                Err(e) => {
                    tracing::warn!(
                        target: "sparkles::incr::runtime",
                        "view {name}: checkpoint {}: {e}; the view is built again",
                        path.display()
                    );
                    st.circuit.result = Default::default();
                }
            }
        }
    }

    fn attach_pending(&self, store: &Store) {
        let Some(host) = self.host.read().clone() else {
            return;
        };
        let mut reg = self.reg.lock();
        self.sync_inference(&mut reg, host.as_ref());
        let epoch = host.epoch();
        if self.host_epoch.swap(epoch, Ordering::AcqRel) != epoch {
            // grants may have changed: resolve every view's principal again
            for e in reg.views.values_mut() {
                e.error = None;
                if let Some(c) = e.cell.take() {
                    let mut st = c.state.lock();
                    st.views = st.views.saturating_sub(1);
                }
            }
        }
        let pending: Vec<String> = reg
            .views
            .iter()
            .filter(|(_, e)| e.cell.is_none() && e.suspended.is_none() && e.error.is_none())
            .map(|(n, _)| n.clone())
            .collect();
        for name in pending {
            if let Err(e) = self.attach(store, &mut reg, &name) {
                tracing::warn!(target: "sparkles::incr::runtime", "view {name}: {e}");
                if let Some(entry) = reg.views.get_mut(&name) {
                    entry.error = Some(e.to_string());
                }
            }
        }
    }

    /// Compile every circuit for the host's inference when it has changed. Each circuit
    /// then evaluates its query again in the new mode on the next round, and the
    /// difference reaches its live queries as a diff or a `reset`, so views stay
    /// attached and live queries keep running.
    fn sync_inference(&self, reg: &mut Registry, host: &dyn Host) {
        let now = host.inference();
        if Inference::same(reg.inference.as_deref(), now.as_ref()) {
            return;
        }
        let now = now.map(Arc::new);
        tracing::info!(
            target: "sparkles::incr::runtime",
            "the dataset's inference changed ({}): {} circuits evaluate again",
            now.as_ref().map_or("none", |i| i.reason()),
            reg.cells.len()
        );
        reg.inference = now.clone();
        // every event of the old mode carries a commit at most `seen`, or at most the
        // commit of a circuit whose pass was in flight, which the state lock waits for
        let mut boundary = self.seen.load(Ordering::Acquire);
        self.switched.fetch_max(boundary, Ordering::AcqRel);
        for cell in reg.cells.values() {
            let mut st = cell.state.lock();
            boundary = boundary.max(st.circuit.applied);
            self.switched.fetch_max(boundary, Ordering::AcqRel);
            let fresh = st.fresh(now.clone());
            let mut c = match fresh {
                Ok(c) => c,
                Err(e) => {
                    self.fail(cell, &mut st, &e);
                    continue;
                }
            };
            // the old result stays, for the first evaluation in the new mode to be
            // compared with
            let old = &mut st.circuit;
            c.result = std::mem::take(&mut old.result);
            c.terms = std::mem::take(&mut old.terms);
            c.snap = old.snap.take();
            c.applied = old.applied;
            st.circuit = c;
            // diffs of the old mode do not lead to rows of the new one
            st.history.clear();
            st.history_rows = 0;
            st.rebased = true;
            let mut info = cell.info.lock();
            info.plan = st.circuit.plan();
            info.whole = st.whole_reason(now.as_deref());
            if info.status == Status::Ready {
                info.status = Status::Building;
            }
        }
    }

    /// [`sync_inference`](Self::sync_inference) for a reader, so that a read after a
    /// change of the setting never sees a circuit of the old mode as current.
    fn sync_for_read(&self, reg: &mut Registry) {
        if let Some(host) = self.host.read().clone() {
            self.sync_inference(reg, host.as_ref());
        }
    }

    /// Run one round: attach pending views, build what needs building and advance
    /// every circuit to the store's head. Returns whether anything was done.
    pub fn round(&self, store: &Store) -> bool {
        if self.closed.load(Ordering::Acquire) {
            return false;
        }
        self.attach_pending(store);
        let cells: Vec<Arc<Cell>> = self.reg.lock().cells.values().cloned().collect();
        let mut deltas: FxHashMap<u64, Arc<DeltaSet>> = FxHashMap::default();
        let mut worked = false;
        let mut over: Vec<(Arc<Cell>, u64)> = Vec::new();
        for cell in &cells {
            // the status is read under the state lock: a switch of the inference replaces
            // the circuit and marks it `building` under that lock
            let mut st = cell.state.lock();
            let status = cell.info.lock().status.clone();
            match status {
                Status::Building => {
                    self.build(store, cell, &mut st);
                    worked = true;
                }
                Status::Ready => {
                    if self.advance(store, cell, &mut st, &mut deltas) {
                        worked = true;
                    }
                }
                Status::Failed { .. } => {}
            }
            drop(st);
            let bytes = cell.info.lock().bytes;
            over.push((cell.clone(), bytes));
        }
        self.budgets(&over);
        self.gc();
        if self.root.is_some() {
            let due = self.last_checkpoint.lock().elapsed()
                >= Duration::from_secs(self.opts.read().checkpoint_seconds);
            if due {
                self.checkpoint(store);
            }
        }
        *self.epoch.lock() += 1;
        self.cv.notify_all();
        worked
    }

    fn control(&self) -> Control {
        Control {
            deadline: Some(Instant::now() + self.opts.read().pass_timeout),
            ..Control::none()
        }
    }

    fn note_pass(&self, cell: &Cell, st: &Slot, took: Duration, skipped: bool) {
        self.seen.fetch_max(st.circuit.applied, Ordering::AcqRel);
        let mut info = cell.info.lock();
        info.applied = st.circuit.applied;
        info.rows = st.circuit.result.total().max(0) as u64;
        info.bytes = st.circuit.bytes();
        if skipped {
            info.skipped += 1;
            self.counters.skipped.fetch_add(1, Ordering::Relaxed);
            return;
        }
        info.passes += 1;
        info.pass_time += took;
        info.recompute_time += st.circuit.last.recompute;
        self.counters.passes.fetch_add(1, Ordering::Relaxed);
        self.counters
            .pass_ns
            .fetch_add(took.as_nanos() as u64, Ordering::Relaxed);
        self.counters.recompute_ns.fetch_add(
            st.circuit.last.recompute.as_nanos() as u64,
            Ordering::Relaxed,
        );
        let s = took.as_secs_f64();
        let b = PASS_BUCKETS
            .iter()
            .position(|&u| s <= u)
            .unwrap_or(PASS_BUCKETS.len());
        self.counters.pass_hist[b].fetch_add(1, Ordering::Relaxed);
    }

    fn fail(&self, cell: &Cell, st: &mut Slot, e: &Error) {
        let (code, message) = failure(e);
        tracing::warn!(
            target: "sparkles::incr::runtime",
            "a view circuit failed ({code}): {message}"
        );
        for s in st.subs.drain(..) {
            if let Some(sh) = s.shared.upgrade() {
                sh.push(LiveEvent::Error {
                    code: code.clone(),
                    message: message.clone(),
                });
            }
        }
        st.history.clear();
        st.history_rows = 0;
        cell.info.lock().status = Status::Failed { code, message };
    }

    fn build(&self, store: &Store, cell: &Cell, st: &mut Slot) {
        let snap = store.snapshot();
        let before = st.circuit.applied;
        let t0 = Instant::now();
        let had = st.circuit.snap.is_some();
        let r = if had {
            // a rebuild: the change reaches live queries
            st.circuit
                .reset(&DeltaSet::at(snap.clone()), &self.control())
                .map(Some)
        } else {
            st.circuit.build(snap, &self.control()).map(|()| None)
        };
        match r {
            Ok(change) => {
                cell.info.lock().status = Status::Ready;
                self.note_pass(cell, st, t0.elapsed(), false);
                self.live_budgets(st);
                if let Some(c) = change {
                    self.publish(st, before, c, true);
                }
                if std::mem::take(&mut st.rebased) {
                    st.history.clear();
                    st.history_rows = 0;
                }
            }
            Err(e) => self.fail(cell, st, &e),
        }
    }

    fn advance(
        &self,
        store: &Store,
        cell: &Cell,
        st: &mut Slot,
        deltas: &mut FxHashMap<u64, Arc<DeltaSet>>,
    ) -> bool {
        if !st.alive() {
            return false;
        }
        let from = st.circuit.applied;
        if from >= store.snapshot().commit {
            return false;
        }
        let d = deltas
            .entry(from)
            .or_insert_with(|| Arc::new(DeltaSet::read(store, from, &Control::none())))
            .clone();
        if d.snapshot.commit <= from {
            return false;
        }
        if !d.complete {
            // evaluated again: the view is `building` meanwhile (spec F13 §8.4)
            cell.info.lock().status = Status::Building;
        }
        let t0 = Instant::now();
        let r = st.circuit.advance(&d, &self.control());
        let took = t0.elapsed();
        match r {
            Ok(Advance::Skipped) => {
                self.note_pass(cell, st, took, true);
                let to = st.circuit.applied;
                let max = self.opts.read().history_commits;
                st.remember(from, to, Arc::from(Vec::new()), max);
            }
            Ok(Advance::Changed(c)) => {
                self.note_pass(cell, st, took, false);
                self.publish(st, from, c, false);
            }
            Ok(Advance::Reset(c)) => {
                self.counters.resets.fetch_add(1, Ordering::Relaxed);
                if from > 0 {
                    tracing::info!(
                        target: "sparkles::incr::runtime",
                        "a view circuit was evaluated again at commit {}: the changes after commit {from} are not all known",
                        st.circuit.applied
                    );
                }
                cell.info.lock().status = Status::Ready;
                self.note_pass(cell, st, took, false);
                self.publish(st, from, c, true);
            }
            Err(e) => {
                self.fail(cell, st, &e);
                return true;
            }
        }
        if d.complete && st.circuit.applied > from + 1 && from > 0 && st.views > 0 {
            tracing::debug!(
                target: "sparkles::incr::runtime",
                "a view circuit applied commits {}..={}",
                from + 1,
                st.circuit.applied
            );
        }
        self.live_budgets(st);
        true
    }

    /// End the live queries of the circuit whose budget its state passed.
    fn live_budgets(&self, st: &mut Slot) {
        let bytes = st.circuit.bytes();
        st.subs.retain(|s| {
            if bytes <= s.budget {
                return true;
            }
            if let Some(sh) = s.shared.upgrade() {
                sh.push(LiveEvent::Error {
                    code: "over-budget".into(),
                    message: format!(
                        "the live query's state of {bytes} bytes passed its budget of {} bytes",
                        s.budget
                    ),
                });
            }
            false
        });
    }

    /// Send the change `c` from commit `from` to the circuit's commit to the
    /// subscribers.
    fn publish(&self, st: &mut Slot, from: u64, c: Changes, reset: bool) {
        st.subs.retain(|s| s.shared.strong_count() > 0);
        let to = st.circuit.applied;
        if st.subs.is_empty() {
            if st.views == 0 || (reset && c.len() > self.opts.read().reset_rows) {
                st.history.clear();
                st.history_rows = 0;
            } else {
                let rows = st.circuit.decode_rows(&c).into();
                st.remember(from, to, rows, VIEW_HISTORY_COMMITS);
            }
            return;
        }
        let max = self.opts.read().history_commits;
        let event = if st.circuit.kind == Kind::Ask {
            let delta: i64 = c.iter().map(|x| x.1).sum();
            let now = st.circuit.result.total();
            st.remember(from, to, Arc::from(vec![(Vec::new(), delta)]), max);
            if (now - delta > 0) == (now > 0) {
                return;
            }
            LiveEvent::Value {
                commit: to,
                from,
                value: now > 0,
            }
        } else if reset && c.len() > self.opts.read().reset_rows {
            st.history.clear();
            st.history_rows = 0;
            st.event(true)
        } else {
            let rows: Arc<[(Vec<Option<Term>>, i64)]> = st.circuit.decode_rows(&c).into();
            st.remember(from, to, rows.clone(), max);
            if rows.is_empty() {
                return;
            }
            diff_event(from, to, rows.iter().cloned())
        };
        for s in &mut st.subs {
            if let Some(sh) = s.shared.upgrade() {
                // a diff starts from the subscriber's latest event, which commits that
                // left the result alone may have left behind
                let mut e = event.clone();
                match &mut e {
                    LiveEvent::Diff { from, .. } | LiveEvent::Value { from, .. } => *from = s.last,
                    _ => {}
                }
                s.last = to;
                sh.push(e);
                self.counters.live_events.fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    /// Suspend the views whose circuit passed their budget or the pool's.
    fn budgets(&self, cells: &[(Arc<Cell>, u64)]) {
        let mut reg = self.reg.lock();
        let mut total: u64 = reg
            .views
            .values()
            .filter_map(|e| e.cell.as_ref())
            .map(|c| c.info.lock().bytes)
            .sum();
        let pool = self.opts.read().views_memory;
        for (cell, bytes) in cells {
            let names: Vec<String> = reg
                .views
                .iter()
                .filter(|(_, e)| e.cell.as_ref().is_some_and(|c| c.key == cell.key))
                .filter(|(_, e)| *bytes > e.def.memory_bytes || pool.is_some_and(|p| total > p))
                .map(|(n, _)| n.clone())
                .collect();
            for n in names {
                let e = reg.views.get_mut(&n).expect("listed");
                let why = if *bytes > e.def.memory_bytes {
                    format!(
                        "over-budget: its state of {bytes} bytes passed memoryBytes {}",
                        e.def.memory_bytes
                    )
                } else {
                    format!(
                        "over-budget: the views' state passed --views-memory {}",
                        pool.unwrap_or(0)
                    )
                };
                tracing::warn!(target: "sparkles::incr::runtime", "view {n} suspended: {why}");
                e.suspended = Some(why);
                if let Some(c) = e.cell.take() {
                    let mut st = c.state.lock();
                    st.views = st.views.saturating_sub(1);
                    if st.views == 0 {
                        total = total.saturating_sub(*bytes);
                    }
                }
            }
        }
    }

    /// Drop the circuits nobody uses.
    fn gc(&self) {
        let mut reg = self.reg.lock();
        reg.cells.retain(|_, c| {
            c.pending.load(Ordering::Acquire) > 0
                || match c.state.try_lock() {
                    Some(mut st) => st.alive(),
                    None => true,
                }
        });
        if reg.cells.is_empty() {
            *self.subscription.lock() = None;
        }
    }

    /// Checkpoint every view whose state moved since its last checkpoint.
    pub fn checkpoint(&self, store: &Store) {
        let Some(root) = self.root.clone() else {
            return;
        };
        *self.last_checkpoint.lock() = Instant::now();
        let todo: Vec<(String, Arc<Cell>, u64)> = self
            .reg
            .lock()
            .views
            .iter()
            .filter_map(|(n, e)| Some((n.clone(), e.cell.clone()?, e.checkpointed)))
            .collect();
        let _ = store;
        for (name, cell, done) in todo {
            // under the state lock, which a switch of the inference takes to replace the
            // circuit
            let st = cell.state.lock();
            if cell.info.lock().status != Status::Ready {
                continue;
            }
            let applied = st.circuit.applied;
            if applied <= done {
                continue;
            }
            let Some(snap) = st.circuit.snap.clone() else {
                continue;
            };
            let r = st.circuit.encode(&snap).and_then(|body| {
                persist::write(
                    &views::dir(&root, &name).join(BRANCH),
                    &persist::Header {
                        dataset: self.dataset,
                        commit: applied,
                        fingerprint: st.fingerprint(cell.key),
                    },
                    &body,
                )
            });
            drop(st);
            match r {
                Ok(()) => {
                    self.counters.checkpoints.fetch_add(1, Ordering::Relaxed);
                    if let Some(e) = self.reg.lock().views.get_mut(&name) {
                        e.checkpointed = applied;
                    }
                }
                Err(e) => {
                    tracing::warn!(
                        target: "sparkles::incr::runtime",
                        "view {name}: checkpoint at commit {applied}: {e}")
                }
            }
        }
    }

    /// Stop the rounds and checkpoint every view (a clean close).
    pub fn close(&self, store: &Store) {
        if self.closed.swap(true, Ordering::AcqRel) {
            return;
        }
        self.checkpoint(store);
        let cells: Vec<Arc<Cell>> = self.reg.lock().cells.drain().map(|(_, c)| c).collect();
        for c in cells {
            let mut st = c.state.lock();
            for s in st.subs.drain(..) {
                if let Some(sh) = s.shared.upgrade() {
                    sh.close();
                }
            }
        }
        *self.subscription.lock() = None;
        self.kick();
        self.cv.notify_all();
    }

    pub fn is_closed(&self) -> bool {
        self.closed.load(Ordering::Acquire)
    }

    /// Run rounds on a thread of its own until `store` returns `None` (the dataset is
    /// gone) or the runtime closes. A round runs after each commit and each
    /// [`kick`](Self::kick).
    pub fn spawn_worker<S, F>(
        self: &Arc<Self>,
        name: String,
        mut commits: tokio::sync::watch::Receiver<u64>,
        store: F,
    ) -> std::io::Result<std::thread::JoinHandle<()>>
    where
        S: std::ops::Deref<Target = Store>,
        F: Fn() -> Option<S> + Send + 'static,
    {
        let rt = Arc::downgrade(self);
        let kick = self.kick.clone();
        std::thread::Builder::new().name(name).spawn(move || {
            let Ok(exec) = tokio::runtime::Builder::new_current_thread().build() else {
                return;
            };
            loop {
                let Some(runtime) = rt.upgrade() else { return };
                if runtime.is_closed() {
                    return;
                }
                commits.borrow_and_update();
                {
                    runtime.busy.store(true, Ordering::Release);
                    let Some(s) = store() else {
                        runtime.busy.store(false, Ordering::Release);
                        return;
                    };
                    runtime.round(&s);
                    drop(s);
                    runtime.busy.store(false, Ordering::Release);
                }
                drop(runtime);
                let gone = exec.block_on(async {
                    let mut changed = std::pin::pin!(commits.changed());
                    let mut kicked = std::pin::pin!(kick.notified());
                    std::future::poll_fn(|cx| {
                        if let std::task::Poll::Ready(r) = changed.as_mut().poll(cx) {
                            return std::task::Poll::Ready(r.is_err());
                        }
                        if kicked.as_mut().poll(cx).is_ready() {
                            return std::task::Poll::Ready(false);
                        }
                        std::task::Poll::Pending
                    })
                    .await
                });
                if gone {
                    return;
                }
            }
        })
    }

    /// Whether the worker holds the dataset for a round.
    pub fn worker_busy(&self) -> bool {
        self.busy.load(Ordering::Acquire)
    }

    /// Run a round on the worker and wait for it, at most `timeout`: views defined in
    /// the dataset are then attached and recovered.
    pub fn settle(&self, timeout: Duration) {
        self.kick();
        self.wait_round(Instant::now() + timeout);
    }

    /// Wait until a round has finished after this call, at most until `until`.
    fn wait_round(&self, until: Instant) -> bool {
        let mut e = self.epoch.lock();
        let start = *e;
        while *e == start {
            if self.cv.wait_until(&mut e, until).timed_out() {
                return false;
            }
        }
        true
    }

    // ----- views -----

    fn broken(&self, reg: &Registry) -> Result<()> {
        match &reg.broken {
            Some(e) => Err(Error::Unsupported(format!(
                "the views cannot be changed until their definitions are fixed: {e}"
            ))),
            None => Ok(()),
        }
    }

    /// Define or replace view `name` (spec F13 §2.1) and attach it; the first
    /// evaluation runs on the next round. Returns the view with its plan.
    pub fn put(&self, store: &Store, name: &str, def: ViewDef) -> Result<ViewInfo> {
        if !views::valid_name(name) {
            return Err(super::error(
                "invalid-view",
                format!("{name:?} cannot name a view: use letters, digits, '_' and '-'"),
            ));
        }
        def.check()?;
        let host = self.host()?;
        let text = Self::view_text(host.as_ref(), &def)?;
        let q = self.parse(host.as_ref(), &text)?;
        if matches!(q, Query::Ask { .. } | Query::Describe { .. }) {
            return Err(super::error(
                "invalid-view",
                "a view is a SELECT or CONSTRUCT query",
            ));
        }
        let access = host.access(def.principal.as_deref())?;
        compile(&q, whole_reason(access.as_ref()))?;
        let mut reg = self.reg.lock();
        self.broken(&reg)?;
        self.sync_inference(&mut reg, host.as_ref());
        if let Some(root) = &self.root {
            views::write(root, name, &def)?;
        }
        if let Some(old) = reg.views.remove(name)
            && let Some(c) = old.cell
        {
            let mut st = c.state.lock();
            st.views = st.views.saturating_sub(1);
        }
        reg.views.insert(
            name.to_string(),
            ViewEntry {
                def,
                cell: None,
                access: None,
                suspended: None,
                error: None,
                checkpointed: 0,
            },
        );
        self.attach(store, &mut reg, name)?;
        drop(reg);
        self.kick();
        self.get(store, name)
    }

    /// Remove view `name` and its state.
    pub fn delete(&self, name: &str) -> Result<()> {
        let mut reg = self.reg.lock();
        self.broken(&reg)?;
        let Some(old) = reg.views.remove(name) else {
            return Err(super::error("view-not-found", format!("no view {name:?}")));
        };
        if let Some(c) = old.cell {
            let mut st = c.state.lock();
            st.views = st.views.saturating_sub(1);
        }
        if let Some(root) = &self.root {
            views::remove(root, name)?;
        }
        drop(reg);
        self.kick();
        Ok(())
    }

    /// Evaluate view `name` again from scratch on the next round, lifting a
    /// suspension.
    pub fn rebuild(&self, name: &str) -> Result<()> {
        let mut reg = self.reg.lock();
        let Some(e) = reg.views.get_mut(name) else {
            return Err(super::error("view-not-found", format!("no view {name:?}")));
        };
        e.suspended = None;
        e.error = None;
        if let Some(c) = &e.cell {
            c.info.lock().status = Status::Building;
        }
        drop(reg);
        self.kick();
        Ok(())
    }

    fn info(&self, name: &str, e: &ViewEntry, head: u64) -> ViewInfo {
        let mut v = ViewInfo {
            name: name.to_string(),
            definition: e.def.clone(),
            state: ViewState::Building,
            commit: None,
            columns: Vec::new(),
            rows: 0,
            state_bytes: 0,
            passes: 0,
            skipped: 0,
            pass_ms: 0.0,
            recompute_ms: 0.0,
            evaluation: None,
            evaluation_reason: None,
            plan: None,
        };
        if let Some(why) = &e.suspended {
            v.state = ViewState::Suspended {
                reason: why.clone(),
            };
        } else if let Some(err) = &e.error {
            v.state = ViewState::Failed { error: err.clone() };
        }
        let Some(cell) = &e.cell else {
            return v;
        };
        let i = cell.info.lock().clone();
        if i.status_ready() {
            v.commit = Some(i.applied);
        }
        v.columns = i.columns;
        v.evaluation = Some(if i.whole.is_some() {
            super::ops::Mode::Recompute
        } else {
            super::ops::Mode::Incremental
        });
        v.evaluation_reason = i.whole;
        v.plan = Some(i.plan);
        v.rows = i.rows;
        v.state_bytes = i.bytes;
        v.passes = i.passes;
        v.skipped = i.skipped;
        v.pass_ms = i.pass_time.as_secs_f64() * 1e3;
        v.recompute_ms = i.recompute_time.as_secs_f64() * 1e3;
        v.state = match i.status {
            Status::Building => ViewState::Building,
            Status::Ready if i.applied >= head => ViewState::Current,
            Status::Ready => ViewState::Behind {
                commits: head - i.applied,
            },
            Status::Failed { code, message } if code != "view-failed" => ViewState::Suspended {
                reason: format!("{code}: {message}"),
            },
            Status::Failed { message, .. } => ViewState::Failed { error: message },
        };
        v
    }

    /// View `name` with its state.
    /// Whether a reader with `caller` may read view `name`'s rows and what they
    /// disclose, such as their count.
    pub fn readable_by(&self, name: &str, caller: Option<&Arc<GraphAccess>>) -> bool {
        let reg = self.reg.lock();
        reg.views
            .get(name)
            .is_some_and(|e| covers(caller, e.access.as_ref()))
    }

    pub fn get(&self, store: &Store, name: &str) -> Result<ViewInfo> {
        let head = store.snapshot().commit;
        let mut reg = self.reg.lock();
        self.sync_for_read(&mut reg);
        let e = reg
            .views
            .get(name)
            .ok_or_else(|| super::error("view-not-found", format!("no view {name:?}")))?;
        Ok(self.info(name, e, head))
    }

    /// Every view with its state, by name.
    pub fn list(&self, store: &Store) -> Vec<ViewInfo> {
        let head = store.snapshot().commit;
        let mut reg = self.reg.lock();
        self.sync_for_read(&mut reg);
        reg.views
            .iter()
            .map(|(n, e)| self.info(n, e, head))
            .collect()
    }

    /// The names and definitions of the views (for backups and clones).
    pub fn definitions(&self) -> Vec<(String, ViewDef)> {
        self.reg
            .lock()
            .views
            .iter()
            .map(|(n, e)| (n.clone(), e.def.clone()))
            .collect()
    }

    /// Wait until view `name` has left `building`, at most `timeout`.
    pub fn wait_built(&self, name: &str, timeout: Duration) -> Result<ViewInfo> {
        let until = Instant::now() + timeout;
        loop {
            let (cell, err) = {
                let mut reg = self.reg.lock();
                self.sync_for_read(&mut reg);
                let e = reg
                    .views
                    .get(name)
                    .ok_or_else(|| super::error("view-not-found", format!("no view {name:?}")))?;
                (
                    e.cell.clone(),
                    e.suspended.clone().or_else(|| e.error.clone()),
                )
            };
            if err.is_some() {
                break;
            }
            if cell.is_some_and(|c| c.info.lock().status != Status::Building) {
                break;
            }
            self.kick();
            if !self.wait_round(until) && Instant::now() >= until {
                break;
            }
        }
        let reg = self.reg.lock();
        let e = reg
            .views
            .get(name)
            .ok_or_else(|| super::error("view-not-found", format!("no view {name:?}")))?;
        let head = e.cell.as_ref().map_or(0, |c| c.info.lock().applied);
        Ok(self.info(name, e, head))
    }

    /// The rows of view `name` (spec F13 §2.2).
    pub fn read(&self, r: &ViewRead<'_>) -> Result<ViewRows> {
        let wait = Instant::now() + self.opts.read().view_wait;
        let until = r.deadline.map_or(wait, |d| d.min(wait));
        loop {
            let (cell, access, suspended, error) = {
                let mut reg = self.reg.lock();
                self.sync_for_read(&mut reg);
                let e = reg.views.get(r.name).ok_or_else(|| {
                    super::error("view-not-found", format!("no view {:?}", r.name))
                })?;
                (
                    e.cell.clone(),
                    e.access.clone(),
                    e.suspended.clone(),
                    e.error.clone(),
                )
            };
            if let Some(why) = suspended {
                return Err(super::error(
                    "view-suspended",
                    format!("the view {:?} is suspended ({why}); rebuild it", r.name),
                ));
            }
            if let Some(err) = error {
                return Err(super::error(
                    "view-failed",
                    format!("the view {:?} failed: {err}", r.name),
                ));
            }
            if let Some(cell) = cell {
                if !covers(r.caller, access.as_ref()) {
                    return Err(super::error(
                        "view-access",
                        format!("the view {:?} reads data your grants do not cover", r.name),
                    ));
                }
                let status = cell.info.lock().status.clone();
                match status {
                    Status::Failed { code, message } => {
                        let code = if code == "view-failed" {
                            "view-failed"
                        } else {
                            "view-suspended"
                        };
                        return Err(super::error(
                            code,
                            format!("the view {:?}: {message}", r.name),
                        ));
                    }
                    Status::Ready => {
                        let st = cell.state.lock();
                        if st.circuit.applied == r.at || r.allow_stale {
                            return Ok(ViewRows {
                                columns: st.circuit.columns(),
                                rows: st.circuit.rows(),
                                commit: st.circuit.applied,
                            });
                        }
                        if st.circuit.applied > r.at {
                            return self.read_back(st, r);
                        }
                    }
                    Status::Building => {}
                }
            }
            self.kick();
            if !self.wait_round(until) && Instant::now() >= until {
                return Err(super::error(
                    "view-behind",
                    format!(
                        "the view {:?} has not reached commit {} yet; retry, or read it with view:allowStale true",
                        r.name, r.at
                    ),
                ));
            }
        }
    }

    /// The rows of a view at `r.at` when its circuit has moved past it: the current
    /// rows less the changes since, from the history, or else the view's query
    /// evaluated at the query's snapshot.
    fn read_back(
        &self,
        st: parking_lot::MutexGuard<'_, Slot>,
        r: &ViewRead<'_>,
    ) -> Result<ViewRows> {
        let columns = st.circuit.columns();
        if let Some(change) = st.history_since(r.at) {
            let rows = net(st
                .circuit
                .rows()
                .into_iter()
                .chain(change.into_iter().map(|(t, w)| (t, -w))))
            .into_iter()
            .filter(|(_, w)| *w > 0)
            .collect();
            return Ok(ViewRows {
                columns,
                rows,
                commit: r.at,
            });
        }
        let ahead = || {
            super::error(
                "view-ahead",
                format!(
                    "the view {:?} has moved past commit {} and cannot read it again; retry, or read it with view:allowStale true",
                    r.name, r.at
                ),
            )
        };
        let Some(snap) = r.snap.filter(|s| s.commit == r.at) else {
            return Err(ahead());
        };
        // the view's own default graph, whatever the reading query asked for
        let union = st
            .circuit
            .snap
            .as_ref()
            .map_or(snap.union_default_graph, |s| s.union_default_graph);
        let snap = if union == snap.union_default_graph {
            snap.clone()
        } else {
            let mut s = (**snap).clone();
            s.union_default_graph = union;
            Arc::new(s)
        };
        let mut c = st.fresh(st.circuit.inference.clone())?;
        drop(st);
        let control = Control {
            deadline: r.deadline.or(self.control().deadline),
            ..Control::none()
        };
        c.build(snap, &control)?;
        Ok(ViewRows {
            columns,
            rows: c.rows(),
            commit: r.at,
        })
    }

    // ----- live queries -----

    /// Subscribe to `query` read with `access` (spec F13 §2.3). The first event is
    /// the result, or with `o.after` the change since that commit.
    pub fn live(
        self: &Arc<Self>,
        store: &Store,
        query: &str,
        access: Option<Arc<GraphAccess>>,
        o: &LiveOptions,
    ) -> Result<LiveQuery> {
        let host = self
            .host
            .read()
            .clone()
            .unwrap_or_else(|| Arc::new(OpenHost));
        let q = self.parse(host.as_ref(), query)?;
        if matches!(q, Query::Describe { .. }) {
            return Err(super::compile::not_incremental("DESCRIBE"));
        }
        // the limits
        if self.live_total.fetch_add(1, Ordering::AcqRel) >= self.opts.read().max_live {
            self.live_total.fetch_sub(1, Ordering::AcqRel);
            return Err(super::error(
                "too-many-live-queries",
                format!(
                    "the server runs its limit of {} live queries",
                    self.opts.read().max_live
                ),
            ));
        }
        let mut count = Count {
            total: self.live_total.clone(),
            principal: None,
        };
        if let Some(p) = &o.principal {
            let mut m = self.live_by.lock();
            let n = m.entry(p.clone()).or_insert(0);
            if *n >= self.opts.read().max_live_per_principal {
                return Err(super::error(
                    "too-many-live-queries",
                    format!(
                        "{p} runs its limit of {} live queries",
                        self.opts.read().max_live_per_principal
                    ),
                ));
            }
            *n += 1;
            count.principal = Some((self.live_by.clone(), p.clone()));
        }
        let budget = o
            .max_memory_bytes
            .unwrap_or(self.opts.read().live_memory_bytes);
        let (cell, _) = {
            let mut reg = self.reg.lock();
            if self.is_closed() {
                return Err(super::error("views-unavailable", "the dataset is closing"));
            }
            self.sync_inference(&mut reg, host.as_ref());
            let (cell, new) = self.cell_for(&mut reg, q, query, access, Some(budget))?;
            cell.pending.fetch_add(1, Ordering::AcqRel);
            (cell, new)
        };
        let joining = Joining(&cell);
        self.ensure_subscription(store);
        let shared = Arc::new(Shared::default());
        {
            let mut st = cell.state.lock();
            let status = cell.info.lock().status.clone();
            if status != Status::Ready {
                let snap = store.snapshot();
                let t0 = Instant::now();
                let r = if st.circuit.snap.is_some() {
                    st.circuit
                        .reset(&DeltaSet::at(snap), &self.control())
                        .map(|_| ())
                } else {
                    st.circuit.build(snap, &self.control())
                };
                if let Err(e) = r {
                    drop(st);
                    drop(joining);
                    self.gc();
                    return Err(e);
                }
                cell.info.lock().status = Status::Ready;
                self.note_pass(&cell, &st, t0.elapsed(), false);
                if std::mem::take(&mut st.rebased) {
                    st.history.clear();
                    st.history_rows = 0;
                }
                self.live_budgets(&mut st);
                // other subscribers see the new result as a reset
                let applied = st.circuit.applied;
                let reset = st.event(true);
                for s in &mut st.subs {
                    if let Some(sh) = s.shared.upgrade() {
                        s.last = applied;
                        sh.push(reset.clone());
                    }
                }
            }
            if st.circuit.bytes() > budget {
                let bytes = st.circuit.bytes();
                drop(st);
                drop(joining);
                self.gc();
                return Err(super::error(
                    "over-budget",
                    format!(
                        "the live query's state of {bytes} bytes passes its budget of {budget} bytes"
                    ),
                ));
            }
            let first = match o.after {
                None => Some(st.event(false)),
                Some(after) => self.resume(store, &st, after),
            };
            // queued before a pass can see the subscriber, so that no later diff
            // overtakes it
            if let Some(e) = first {
                shared.push(e);
            }
            let last = st.circuit.applied;
            st.subs.push(Sub {
                shared: Arc::downgrade(&shared),
                budget,
                last,
            });
        }
        drop(joining);
        let (kind, vars) = {
            let st = cell.state.lock();
            (st.circuit.kind, st.vars.clone())
        };
        self.kick();
        Ok(LiveQuery {
            shared,
            slot: cell,
            kind,
            vars,
            _count: count,
        })
    }

    /// The first event of a live query resumed after commit `after`: the change from
    /// it from the circuit's history, or by evaluating the query before the changes
    /// since, or a `reset` (spec F13 §2.3).
    fn resume(&self, store: &Store, st: &Slot, after: u64) -> Option<LiveEvent> {
        let applied = st.circuit.applied;
        let switched = self.switched.load(Ordering::Acquire);
        if switched > 0 && after <= switched {
            return Some(st.event(true));
        }
        if after == applied {
            return None;
        }
        if after > applied {
            return Some(st.event(true));
        }
        let ask = st.circuit.kind == Kind::Ask;
        let change = st.history_since(after).or_else(|| {
            let d = DeltaSet::read(store, after, &Control::none());
            if !d.complete || d.snapshot.commit != applied {
                return None;
            }
            let old = d.old().ok()?;
            let mut c = st.fresh(st.circuit.inference.clone()).ok()?;
            c.build(old, &self.control()).ok()?;
            if ask {
                let delta = st.circuit.result.total() - c.result.total();
                return Some(vec![(Vec::new(), delta)]);
            }
            let now = st.circuit.rows();
            let before = c.rows();
            Some(net(now
                .into_iter()
                .chain(before.into_iter().map(|(r, w)| (r, -w)))))
        });
        let Some(change) = change else {
            return Some(st.event(true));
        };
        if ask {
            let delta: i64 = change.iter().map(|x| x.1).sum();
            let now = st.circuit.result.total();
            if (now - delta > 0) == (now > 0) {
                return None;
            }
            return Some(LiveEvent::Value {
                commit: applied,
                from: after,
                value: now > 0,
            });
        }
        Some(diff_event(after, applied, change.into_iter()))
    }

    /// The live queries running.
    pub fn live_count(&self) -> usize {
        self.live_total.load(Ordering::Acquire)
    }

    /// The counters for the metrics.
    pub fn stats(&self, store: &Store) -> RuntimeStats {
        let head = store.snapshot().commit;
        let reg = self.reg.lock();
        let views = reg
            .views
            .iter()
            .map(|(n, e)| {
                let (applied, bytes) = e.cell.as_ref().map_or((0, 0), |c| {
                    let i = c.info.lock();
                    (i.applied, i.bytes)
                });
                ViewStat {
                    name: n.clone(),
                    lag_commits: head.saturating_sub(applied),
                    state_bytes: bytes,
                }
            })
            .collect();
        let c = &self.counters;
        RuntimeStats {
            views,
            live_queries: self.live_count(),
            circuits: reg.cells.len(),
            passes: c.passes.load(Ordering::Relaxed),
            skipped: c.skipped.load(Ordering::Relaxed),
            resets: c.resets.load(Ordering::Relaxed),
            live_events: c.live_events.load(Ordering::Relaxed),
            recompute_seconds: c.recompute_ns.load(Ordering::Relaxed) as f64 / 1e9,
            pass_seconds_sum: c.pass_ns.load(Ordering::Relaxed) as f64 / 1e9,
            pass_buckets: c
                .pass_hist
                .iter()
                .map(|b| b.load(Ordering::Relaxed))
                .collect(),
            checkpoints: c.checkpoints.load(Ordering::Relaxed),
            loaded: c.loaded.load(Ordering::Relaxed),
        }
    }
}

impl Info {
    fn status_ready(&self) -> bool {
        self.status == Status::Ready
    }
}

fn diff_event(
    from: u64,
    to: u64,
    rows: impl Iterator<Item = (Vec<Option<Term>>, i64)>,
) -> LiveEvent {
    let (mut removed, mut added) = (Vec::new(), Vec::new());
    for (terms, w) in rows {
        let r = LiveRow {
            terms,
            count: w.unsigned_abs(),
        };
        if w < 0 {
            removed.push(r);
        } else if w > 0 {
            added.push(r);
        }
    }
    LiveEvent::Diff {
        commit: to,
        from,
        removed: removed.into(),
        added: added.into(),
    }
}

impl ViewSource for Runtime {
    fn read_view(&self, r: &ViewRead<'_>) -> Result<ViewRows> {
        self.read(r)
    }
}

#[cfg(test)]
mod switch_tests {
    use super::*;
    use crate::store::StoreOptions;

    struct Rdfs(Mutex<Option<Inference>>);
    impl Host for Rdfs {
        fn access(&self, _: Option<&str>) -> Result<Option<Arc<GraphAccess>>> {
            Ok(None)
        }
        fn stored_query(&self, _: &str) -> Result<String> {
            Err(Error::NotFound("none".into()))
        }
        fn inference(&self) -> Option<Inference> {
            self.0.lock().clone()
        }
    }

    /// The boundary of a switch covers the commit of a pass that finished while the
    /// switch waited for its circuit's lock, before the pass reported it.
    #[test]
    fn the_switch_boundary_covers_a_pass_in_flight() {
        use crate::sparql::rdfs::{RdfsOnRead, SchemaSource};
        let s = Store::in_memory(StoreOptions::default());
        let rt = Runtime::open(None, s.dataset_id(), RuntimeOptions::default());
        let host = Arc::new(Rdfs(Mutex::new(None)));
        rt.set_host(host.clone());
        let _l = rt
            .live(
                &s,
                "SELECT ?s WHERE { ?s ?p ?o }",
                None,
                &LiveOptions::default(),
            )
            .unwrap();
        // a pass moved the circuit to commit 7 and has not reported it yet
        let cell = rt.reg.lock().cells.values().next().unwrap().clone();
        cell.state.lock().circuit.applied = 7;
        assert!(rt.seen.load(Ordering::Acquire) < 7);
        *host.0.lock() = Inference::new(
            Some(Arc::new(RdfsOnRead::new(SchemaSource::Graph(None)))),
            Vec::new(),
        );
        let mut reg = rt.reg.lock();
        rt.sync_inference(&mut reg, host.as_ref());
        assert_eq!(rt.switched.load(Ordering::Acquire), 7);
    }
}
