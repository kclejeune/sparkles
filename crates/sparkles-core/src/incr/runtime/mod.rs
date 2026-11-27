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

use super::circuit::{Circuit, DeltaSet};
use super::compile::{Kind, compile};
use super::inference::Inference;
use super::live::{Count, LiveEvent, LiveOptions, LiveQuery, LiveRow, Shared};
use super::ops::PlanNode;
use super::persist;
use super::views::{self, ViewDef, ViewInfo, ViewState};
use super::zset::Changes;
use crate::access::GraphAccess;
use crate::error::{Error, Result};
use crate::sparql::pool::QueryPool;
use crate::sparql::viewsvc::{
    OpenedView, RewoundView, ViewOpen, ViewRead, ViewRebuild, ViewRows, ViewSource, ViewVersion,
    row_retained_bytes,
};
use crate::store::definitions::{self, DefinitionFile, DefinitionSource};
use crate::store::{DeltaSubscription, Store};
use crate::task::Control;
use oxrdf::Term;
use parking_lot::{Condvar, Mutex, ReentrantMutex, RwLock};
use rustc_hash::FxHashMap;
use sha2::{Digest, Sha256};
use spargebra::Query;
use sparkles_budget::{MemoryLease, ParentHandle};
use std::cell::RefCell;
use std::collections::{BTreeMap, VecDeque};
use std::mem::ManuallyDrop;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Weak};
use std::time::{Duration, Instant};

mod attach;
mod catalog;
mod cell;
mod internal;
pub(in crate::incr) mod key;
mod read;
mod registry;
mod round;
#[cfg(test)]
pub(crate) use catalog::set_wait_built_final_hook;
#[cfg(test)]
pub(crate) use internal::set_wait_timeout_hook;
#[cfg(test)]
pub(crate) use read::set_open_view_state_hook;
#[cfg(test)]
pub(crate) use round::PASS_CANCEL;
mod stats;
mod subscribe;
mod worker;

use cell::*;
use key::*;
use registry::*;
use worker::*;

pub(crate) use cell::{Cell, Slot};
use internal::InternalCell;
pub use internal::InternalCircuit;
#[cfg(test)]
pub(crate) use key::COMPILER;
pub use key::{access_fingerprint, covers, whole_reason};
#[cfg(test)]
pub(crate) fn checkpoint_fingerprint_for_compiler(compiler: &str, q: &Query, text: &str) -> u64 {
    let key = key::key_with_compiler(compiler, q, text, None, None);
    let mut h = Sha256::new();
    h.update(key.to_le_bytes());
    h.update(b"\0aggregate-errors\0");
    let errors = crate::sparql::AggregateErrors::default();
    h.update(errors.as_str().as_bytes());
    u64::from_le_bytes(h.finalize()[..8].try_into().expect("8 bytes"))
}
use stats::Counters;
pub use stats::{RuntimeStats, ViewStat};
pub use worker::WorkerPause;

/// Payloads and retired leases collected while holding runtime/cell locks.
///
/// Rust drops struct fields in declaration order, guaranteeing that all circuit and
/// cell allocations are freed strictly before any detached [`MemoryLease`] returns
/// credit to the parent pool on normal return, early `?`, and panic unwind.
#[derive(Default)]
pub(super) struct DeferredDrops {
    pub(super) circuits: Vec<Circuit>,
    pub(super) cells: Vec<Arc<Cell>>,
    pub(super) queued: Vec<super::live::QueuedEvent>,
    pub(super) retired: Vec<MemoryLease>,
    // Drop these guards after the retired leases have returned their credit.
    pub(super) retirements: Vec<Retirement>,
}

pub(super) struct Retirement {
    completion: Arc<RetirementCompletion>,
    kick: Arc<tokio::sync::Notify>,
}

pub(super) struct RetirementCompletion {
    owner: std::thread::ThreadId,
    released: std::sync::OnceLock<()>,
}

impl Drop for Retirement {
    fn drop(&mut self) {
        let _ = self.completion.released.set(());
        self.kick.notify_one();
    }
}

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

/// What the runtime asks of its host. After changing the installed host's inference
/// or aggregate-error settings, call [`Runtime::mode_changed`] so timed reads reject
/// the previous mode until the runtime publishes the change.
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
    /// How the dataset's aggregates treat errors (see
    /// [`AggregateErrors`](crate::sparql::AggregateErrors)): its own setting, else the
    /// store's. Views and live queries run with it, a change of it evaluates them again,
    /// and a query that reads a view under another reading evaluates the view's query
    /// itself instead of reading the view's state.
    fn aggregate_errors(&self) -> crate::sparql::AggregateErrors {
        crate::sparql::AggregateErrors::default()
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

#[cfg(test)]
type TestHook = Arc<dyn Fn() + Send + Sync>;
#[cfg(test)]
type WaitHook = Arc<dyn Fn(Instant) + Send + Sync>;

/// The views and live queries of one dataset.
pub struct Runtime {
    opts: RwLock<RuntimeOptions>,
    root: Option<PathBuf>,
    dataset: uuid::Uuid,
    pool: Option<Arc<QueryPool>>,
    host: RwLock<Option<Arc<dyn Host>>>,
    /// Invalidates the published mode on host replacement and settings changes.
    host_seq: AtomicU64,
    /// Orders mode observations taken concurrently outside the registry.
    next_mode_sample: AtomicU64,
    reg: Mutex<Registry>,
    /// Atomic mirror of `reg.mode_gen` updated under `reg` so readers and rounds can
    /// detect a newer recorded mode generation without reacquiring `reg` under `cell.state`.
    mode_gen: AtomicU64,
    /// The definitions as backups and clones capture them, replaced while `reg` is
    /// held after every change. A leaf lock: a capture reads it under the store's
    /// writer lock, and nothing that holds it waits for another lock.
    defs: RwLock<Defs>,
    /// a counter of finished rounds, which readers waiting for a view watch
    epoch: Mutex<u64>,
    cv: Condvar,
    kick: Arc<tokio::sync::Notify>,
    subscription: Mutex<Option<DeltaSubscription>>,
    live_total: Arc<AtomicUsize>,
    live_by: Arc<Mutex<FxHashMap<String, usize>>>,
    counters: Counters,
    published_views: Arc<AtomicU64>,
    last_checkpoint: Mutex<Instant>,
    closed: AtomicBool,
    /// the host's epoch the attached views resolved their principals under
    host_epoch: AtomicU64,
    /// Serializes worker store acquisition and rounds with shutdown. Reentrancy
    /// allows a worker store wrapper to close the runtime from its destructor.
    worker: ReentrantMutex<()>,
    /// A host may drop its last Dataset handle from inside a round. Cleanup then
    /// waits until that round has released its registry and circuit locks.
    worker_round: AtomicBool,
    deferred_close: AtomicBool,
    /// the highest commit a circuit's result has reflected
    seen: AtomicU64,
    /// `seen` at the latest switch of the inference (0: none). The switch changes results
    /// without a commit, so a live query resumed at or before it gets the whole result,
    /// even when its circuit was collected and created again since.
    switched: AtomicU64,
    /// Test hook invoked in `open_view` between releasing `reg` and acquiring `cell.state`.
    #[cfg(test)]
    before_read_state_hook: Mutex<Option<TestHook>>,
    #[cfg(test)]
    observer_wait_hook: Mutex<Option<WaitHook>>,
    #[cfg(test)]
    busy_locked_hook: Mutex<Option<TestHook>>,
    #[cfg(test)]
    before_mode_host_hook: Mutex<Option<TestHook>>,
    #[cfg(test)]
    wait_info_hook: Mutex<Option<TestHook>>,
    #[cfg(test)]
    note_pass_hook: Mutex<Option<TestHook>>,
    #[cfg(test)]
    retirement_wait_hook: Mutex<Option<TestHook>>,
    /// Test counter incremented whenever `round` waits on a busy cell in Pass 2.
    #[cfg(test)]
    busy_waits: AtomicUsize,
}

impl std::fmt::Debug for Runtime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Runtime").field("root", &self.root).finish()
    }
}

impl Runtime {
    /// A runtime for the dataset at `root` (`None`: in memory, without checkpoints)
    /// with the definitions found there. Views are attached once a host is set.
    pub fn open(root: Option<PathBuf>, dataset: uuid::Uuid, opts: RuntimeOptions) -> Arc<Runtime> {
        Self::open_with_pool(root, dataset, opts, None)
    }

    /// [`open`](Self::open) wired to a shared [`QueryPool`], so every view, live query
    /// and internal circuit reserves its state, history/tally and nested SPARQL work
    /// from the pool.
    pub fn open_with_pool(
        root: Option<PathBuf>,
        dataset: uuid::Uuid,
        opts: RuntimeOptions,
        pool: Option<Arc<QueryPool>>,
    ) -> Arc<Runtime> {
        let reg = Registry::load(root.as_deref());
        let defs = RwLock::new(reg.defs());
        Arc::new(Runtime {
            opts: RwLock::new(opts),
            root,
            dataset,
            pool,
            host: RwLock::new(None),
            host_seq: AtomicU64::new(0),
            next_mode_sample: AtomicU64::new(0),
            reg: Mutex::new(reg),
            mode_gen: AtomicU64::new(0),
            defs,
            epoch: Mutex::new(0),
            cv: Condvar::new(),
            kick: Arc::new(tokio::sync::Notify::new()),
            subscription: Mutex::new(None),
            live_total: Arc::new(AtomicUsize::new(0)),
            live_by: Default::default(),
            counters: Counters::default(),
            published_views: Arc::new(AtomicU64::new(0)),
            last_checkpoint: Mutex::new(Instant::now()),
            closed: AtomicBool::new(false),
            host_epoch: AtomicU64::new(0),
            worker: ReentrantMutex::new(()),
            worker_round: AtomicBool::new(false),
            deferred_close: AtomicBool::new(false),
            seen: AtomicU64::new(0),
            switched: AtomicU64::new(0),
            #[cfg(test)]
            before_read_state_hook: Mutex::new(None),
            #[cfg(test)]
            busy_waits: AtomicUsize::new(0),
            #[cfg(test)]
            observer_wait_hook: Mutex::new(None),
            #[cfg(test)]
            busy_locked_hook: Mutex::new(None),
            #[cfg(test)]
            before_mode_host_hook: Mutex::new(None),
            #[cfg(test)]
            wait_info_hook: Mutex::new(None),
            #[cfg(test)]
            note_pass_hook: Mutex::new(None),
            #[cfg(test)]
            retirement_wait_hook: Mutex::new(None),
        })
    }

    pub(super) fn circuit_parent(&self) -> Option<ParentHandle> {
        self.pool.as_ref().map(QueryPool::work_parent)
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
        let _operation = Operation::enter();
        let old = {
            let mut slot = self.host.write();
            let old = slot.replace(host);
            self.host_seq.fetch_add(1, Ordering::AcqRel);
            old
        };
        drop(old);
        self.kick();
    }

    pub(super) fn current_host(&self) -> (u64, Option<Arc<dyn Host>>) {
        let slot = self.host.read();
        (self.host_seq(), slot.clone())
    }

    pub(super) fn host_seq(&self) -> u64 {
        self.host_seq.load(Ordering::Acquire)
    }

    /// Invalidate the published mode after changing the installed host's settings.
    /// Timed view reads wait for a fresh publication before accepting cached rows.
    pub fn mode_changed(&self) {
        self.host_seq.fetch_add(1, Ordering::AcqRel);
        self.kick();
    }

    /// The host, or a host without access control before one is set.
    fn host_or_open(&self) -> Arc<dyn Host> {
        self.host
            .read()
            .clone()
            .unwrap_or_else(|| Arc::new(OpenHost))
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

    #[cfg(test)]
    fn before_observer_wait(&self, until: Instant) {
        if let Some(hook) = self.observer_wait_hook.lock().clone() {
            hook(until);
        }
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
        let _operation = Operation::enter();
        let host = self.host_or_open();
        let q = self.parse(host.as_ref(), query)?;
        let inference = host.inference();
        let whole = whole_reason(access.as_ref()).or(inference.as_ref().map(Inference::reason));
        let c = compile(&q, whole)?;
        let circuit = Circuit::new(c, access);
        Ok((circuit.kind, circuit.plan()))
    }

    fn ensure_subscription(&self, store: &Store) {
        let mut s = self.subscription.lock();
        if s.is_none() && !self.is_closed() {
            *s = Some(store.subscribe_deltas());
        }
    }
}
