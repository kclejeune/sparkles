//! Internal circuits (spec F13 §3.6): attaching them for server code, maintaining
//! them on the runtime's rounds, and checkpointing them.
//!
//! Internal circuits live beside the views' circuits and never share one: a view or a
//! live query that asks the same question has a circuit of its own, so no caller reads
//! or changes an internal circuit's state. They read the stored quads with the access
//! they were attached with, and the dataset's inference does not apply to them.
//!
//! **Ordering.** The worker maintains an internal circuit after commits are published,
//! under the circuit's state lock, as it does views. An accumulator's circuit is
//! advanced one commit at a time in commit order, and each commit's counts are recorded
//! before the next commit is read, so no later change cancels them. A reader takes the
//! same lock, so it sees the circuit between two runs of commits, with every commit up
//! to the circuit's commit counted and none after it. A checkpoint holds the circuit's
//! state and the counts at one commit, written under the lock, so a restart that loads
//! it counts exactly the commits after it again from the change log, and a gap the log
//! cannot bridge is recorded as unknown.

use super::*;
use crate::incr::internal::{
    self as internal, AnnotatedRows, Counts, Dec, Enc, InternalAccess, InternalRows, InternalSpec,
    Sink, Tally,
};
use crate::incr::prov::{Annotation, FactKey};
use spargebra::algebra::GraphPattern;

/// An internal circuit with its state.
pub(crate) struct InternalCell {
    pub(super) spec: InternalSpec,
    pub(super) fingerprint: u64,
    pub(super) state: Mutex<InternalSlot>,
    /// the handles server code holds; the circuit is collected after the last one
    pub(super) handles: AtomicUsize,
    /// the circuit's commit, for waiting without its lock
    pub(super) applied: AtomicU64,
    /// whether `state.status` is `Status::Failed`, for waiting without its lock
    pub(super) failed: AtomicBool,
}

/// `circuit` is declared last so `tally` (whose bytes are accounted on `circuit`'s
/// budget) drops strictly before `circuit`'s credit returns to the pool.
pub(super) struct InternalSlot {
    pub(super) status: Status,
    /// the counts of an accumulator, behind a leaf mutex so `ViewBudget` can evict
    /// oldest counts when active pass work needs headroom under the cap or pool
    /// (spec F13 §3.6).
    pub(super) tally: Option<Arc<Mutex<Tally>>>,
    /// set by `tally_reclaim_hook` when background reclaim drops oldest counts
    pub(super) tally_dirty: Arc<AtomicBool>,
    pub(super) key_cols: Vec<usize>,
    pub(super) checkpointed: u64,
    /// the counts changed without a pass (counts were discarded)
    pub(super) dirty: bool,
    pub(super) circuit: Circuit,
}

impl InternalSlot {
    /// Leaf reclaim hook for `ViewBudget`: evicts oldest accumulator counts when active
    /// circuit pass work needs headroom under the local cap or parent pool. Captures a
    /// `Weak` reference so `InternalSlot::tally` remains the sole strong owner and drops
    /// strictly before `InternalSlot::circuit`'s credit returns to the pool.
    pub(super) fn tally_reclaim_hook(
        tally: &Arc<Mutex<Tally>>,
        dirty: &Arc<AtomicBool>,
    ) -> crate::incr::budget::ReclaimHook {
        let weak = Arc::downgrade(tally);
        let dirty = dirty.clone();
        Arc::new(move |needed: u64| {
            let Some(tally) = weak.upgrade() else {
                return 0;
            };
            let (freed, evicted) = tally.lock().evict_for_reclaim(needed);
            if evicted {
                dirty.store(true, Ordering::Release);
            }
            freed
        })
    }

    pub(super) fn install_tally(&mut self, tally: Tally) {
        let arc = Arc::new(Mutex::new(tally));
        self.circuit
            .set_reclaim_external(Some(Self::tally_reclaim_hook(&arc, &self.tally_dirty)));
        self.tally = Some(arc);
    }

    pub(super) fn sync_tally_dirty(&mut self) {
        if self.tally_dirty.swap(false, Ordering::AcqRel) {
            self.dirty = true;
        }
    }
}

/// A handle on an internal circuit that server code attached
/// ([`Runtime::attach_internal`]). The circuit is maintained while a handle lives, and
/// collected on a round after the last one is dropped.
pub struct InternalCircuit {
    cell: Arc<InternalCell>,
    rt: Weak<Runtime>,
}

impl std::fmt::Debug for InternalCircuit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InternalCircuit")
            .field("id", &self.cell.spec.id)
            .finish()
    }
}

impl Clone for InternalCircuit {
    fn clone(&self) -> Self {
        self.cell.handles.fetch_add(1, Ordering::AcqRel);
        InternalCircuit {
            cell: self.cell.clone(),
            rt: self.rt.clone(),
        }
    }
}

impl Drop for InternalCircuit {
    fn drop(&mut self) {
        if self.cell.handles.fetch_sub(1, Ordering::AcqRel) == 1
            && let Some(rt) = self.rt.upgrade()
        {
            rt.kick();
        }
    }
}

fn status_error(s: &Status) -> Option<Error> {
    match s {
        Status::Ready => None,
        Status::Building => Some(crate::incr::error(
            "views-unavailable",
            "the internal circuit is being built",
        )),
        Status::Failed { code, message } => Some(crate::incr::error(code, message.clone())),
    }
}

#[cfg(test)]
thread_local! {
    static WAIT_TIMEOUT_HOOK: std::cell::RefCell<Option<Box<dyn FnMut()>>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
pub(crate) struct WaitTimeoutHookGuard;

#[cfg(test)]
impl Drop for WaitTimeoutHookGuard {
    fn drop(&mut self) {
        WAIT_TIMEOUT_HOOK.with(|h| *h.borrow_mut() = None);
    }
}

#[cfg(test)]
pub(crate) fn set_wait_timeout_hook(f: impl FnMut() + 'static) -> WaitTimeoutHookGuard {
    WAIT_TIMEOUT_HOOK.with(|h| *h.borrow_mut() = Some(Box::new(f)));
    WaitTimeoutHookGuard
}

#[cfg(test)]
fn fire_wait_timeout_hook() {
    let f = WAIT_TIMEOUT_HOOK.with(|h| h.borrow_mut().take());
    if let Some(mut f) = f {
        f();
        WAIT_TIMEOUT_HOOK.with(|h| {
            if h.borrow().is_none() {
                *h.borrow_mut() = Some(f);
            }
        });
    }
}

impl InternalCircuit {
    pub fn id(&self) -> &str {
        &self.cell.spec.id
    }

    /// The commit the circuit reflects.
    pub fn commit(&self) -> u64 {
        self.cell.applied.load(Ordering::Acquire)
    }

    /// Wait until the circuit reflects commit `commit`, at most `timeout`. Whether it
    /// does. The runtime's worker runs the rounds.
    pub fn wait(&self, commit: u64, timeout: Duration) -> bool {
        let until = Instant::now() + timeout;
        loop {
            if self.cell.failed.load(Ordering::Acquire) {
                return false;
            }
            if self.commit() >= commit {
                return !self.cell.failed.load(Ordering::Acquire);
            }
            let Some(rt) = self.rt.upgrade() else {
                return false;
            };
            if rt.is_closed() {
                return false;
            }
            let epoch = rt.round_epoch();
            if self.cell.failed.load(Ordering::Acquire) {
                return false;
            }
            if self.commit() >= commit {
                return !self.cell.failed.load(Ordering::Acquire);
            }
            if rt.is_closed() {
                return false;
            }
            rt.kick();
            if !rt.wait_round_after(epoch, until) {
                let reached = self.commit() >= commit;
                #[cfg(test)]
                fire_wait_timeout_hook();
                return reached && !self.cell.failed.load(Ordering::Acquire);
            }
        }
    }

    /// An accumulator's changed facts per key in the commits after `after` up to the
    /// circuit's commit (spec F13 §3.6). The counts are exact for any `after` from the
    /// commit they start after, and [`Counts::unknown`] says why they are not when some
    /// commit in the range was not counted per key.
    pub fn counts(&self, after: u64) -> Result<Counts> {
        let st = self.cell.state.lock();
        if let Some(e) = status_error(&st.status) {
            return Err(e);
        }
        let tally = st.tally.as_ref().ok_or_else(|| {
            crate::incr::error("invalid-view", "the internal circuit is not an accumulator")
        })?;
        Ok(tally.lock().counts(after, st.circuit.applied))
    }

    /// Drop an accumulator's counts of the commits up to `commit`, which server code
    /// has acted on. Later reads count from `commit` at the earliest.
    pub fn discard_through(&self, commit: u64) {
        let retired = {
            let mut st = self.cell.state.lock();
            let through = commit.min(st.circuit.applied);
            if let Some(tally) = st.tally.clone() {
                let bytes = {
                    let mut t = tally.lock();
                    t.discard_through(through);
                    t.bytes()
                };
                st.circuit.set_external_best_effort(bytes);
                st.dirty = true;
            }
            st.circuit.drain_retired()
        };
        drop(retired);
    }

    /// The circuit's rows at its commit.
    pub fn rows(&self) -> Result<InternalRows> {
        let st = self.cell.state.lock();
        if let Some(e) = status_error(&st.status) {
            return Err(e);
        }
        let (rows, mut lease) = st.circuit.rows_charged()?;
        let columns = st.circuit.columns_charged(&mut lease)?;
        lease.try_reserve(std::mem::size_of_val(rows.as_slice()) as u64)?;
        let rows = rows
            .into_iter()
            .map(|(r, w)| (r, w.unsigned_abs()))
            .collect();
        Ok(InternalRows {
            commit: st.circuit.applied,
            columns,
            rows,
            _memory: internal::RowsMemory {
                _lease: st.circuit.detach_scratch_lease(&mut lease),
            },
        })
    }

    /// The circuit's rows at its commit, each with its annotation (spec F13 §5). The
    /// spec must keep a provenance setting.
    pub fn annotated_rows(&self) -> Result<AnnotatedRows> {
        let st = self.cell.state.lock();
        if let Some(e) = status_error(&st.status) {
            return Err(e);
        }
        if st.circuit.provenance().is_none() {
            return Err(crate::incr::error(
                "invalid-view",
                "the internal circuit keeps no provenance",
            ));
        }
        let mut lease = st.circuit.scratch_lease();
        let columns = st.circuit.columns_charged(&mut lease)?;
        let rows = st.circuit.annotated_rows_charged(&mut lease)?;
        Ok(AnnotatedRows {
            commit: st.circuit.applied,
            columns,
            rows,
            _memory: internal::RowsMemory {
                _lease: st.circuit.detach_scratch_lease(&mut lease),
            },
        })
    }

    /// The quads that fact keys of the circuit's annotations name, at the circuit's
    /// commit: each key with its subject, predicate, object and graph (`None`: the
    /// default graph), or `None` when no quad of the circuit's patterns has the key.
    #[allow(clippy::type_complexity)]
    pub fn resolve(
        &self,
        keys: &[FactKey],
    ) -> Result<Vec<(FactKey, Option<([Term; 3], Option<Term>)>)>> {
        let st = self.cell.state.lock();
        if let Some(e) = status_error(&st.status) {
            return Err(e);
        }
        let control = self
            .rt
            .upgrade()
            .map_or_else(Control::none, |rt| rt.control());
        st.circuit.resolve_facts(keys, &control)
    }

    /// The negative conditions that the circuit's witnesses name: each number with its
    /// operator and the pattern that must have no match.
    pub fn conditions(&self) -> Vec<(u32, String)> {
        self.cell.state.lock().circuit.conditions()
    }

    /// The annotation of one row of the circuit's result (`None` without provenance or
    /// without the row).
    pub fn annotation(&self, row: &[Option<Term>]) -> Option<Annotation> {
        let st = self.cell.state.lock();
        st.circuit
            .annotated_rows()
            .into_iter()
            .find(|(r, _, _)| r.as_slice() == row)
            .and_then(|(_, _, a)| a)
    }

    /// The circuit's state's approximate memory, its counts' included.
    pub fn bytes(&self) -> u64 {
        let st = self.cell.state.lock();
        st.circuit.bytes() + st.tally.as_ref().map_or(0, |t| t.lock().bytes())
    }

    /// The most memory the counts took right after a commit was counted, and the drops
    /// that the budget made.
    #[cfg(test)]
    pub(crate) fn tally_peak(&self) -> (u64, u64) {
        self.cell.state.lock().tally.as_ref().map_or((0, 0), |t| {
            let t = t.lock();
            (t.peak, t.drops)
        })
    }

    /// Evaluate the circuit again from scratch on the next round, lifting a failure.
    /// An accumulator keeps its counts and records the commits since its last pass as
    /// unknown.
    pub fn rebuild(&self) {
        let mut st = self.cell.state.lock();
        st.status = Status::Building;
        self.cell.failed.store(false, Ordering::Release);
        drop(st);
        if let Some(rt) = self.rt.upgrade() {
            rt.kick();
        }
    }
}

/// Whether `gp` groups its solutions (an aggregate).
fn groups(gp: &GraphPattern) -> bool {
    use GraphPattern as G;
    match gp {
        G::Group { .. } => true,
        G::Bgp { .. } | G::Path { .. } | G::Values { .. } => false,
        G::Join { left, right }
        | G::SemiJoin { left, right }
        | G::AntiJoin { left, right }
        | G::LeftJoin { left, right, .. }
        | G::Lateral { left, right }
        | G::Union { left, right }
        | G::Minus { left, right } => groups(left) || groups(right),
        G::Filter { inner, .. }
        | G::Graph { inner, .. }
        | G::Extend { inner, .. }
        | G::Assign { inner, .. }
        | G::Unfold { inner, .. }
        | G::OrderBy { inner, .. }
        | G::Project { inner, .. }
        | G::Distinct { inner }
        | G::Reduced { inner }
        | G::Slice { inner, .. }
        | G::Service { inner, .. } => groups(inner),
    }
}

/// The kind of checkpoint a circuit writes and reads: format 2 when it keeps
/// annotations and format 1 otherwise, and two more with a fixed-point operator.
fn checkpoint_kind(c: &Circuit) -> persist::Kind {
    persist::internal_kind(!c.provenance().is_none(), c.has_fixpoint())
}

/// The directory of internal circuit `id`'s checkpoints.
fn dir(root: &std::path::Path, id: &str) -> PathBuf {
    root.join(internal::DIR).join(id).join(BRANCH)
}

impl Runtime {
    /// Attach the internal circuit `spec` read with `access`, or join the one attached
    /// with the same id and spec (spec F13 §3.6). The circuit loads its newest
    /// checkpoint or is built before this returns, so an accumulator counts the commits
    /// after the one it reflects then.
    pub fn attach_internal(
        self: &Arc<Self>,
        store: &Store,
        spec: InternalSpec,
        access: InternalAccess,
    ) -> Result<InternalCircuit> {
        let _operation = Operation::enter();
        let invalid = |m: String| crate::incr::error("invalid-view", m);
        if !views::valid_name(&spec.id) {
            return Err(invalid(format!(
                "{:?} cannot name an internal circuit: use letters, digits, '_' and '-'",
                spec.id
            )));
        }
        let q = crate::sparql::parse_query(&spec.query, None, &[])?;
        match (&spec.sink, &q) {
            (_, Query::Describe { .. }) => {
                return Err(crate::incr::compile::not_incremental("DESCRIBE"));
            }
            (Sink::Accumulator { .. }, Query::Select { pattern, .. }) => {
                if groups(pattern) {
                    return Err(invalid(
                        "an accumulator counts changed facts, which a grouped query nets out"
                            .into(),
                    ));
                }
            }
            (Sink::Accumulator { .. }, _) => {
                return Err(invalid("an accumulator's query is a SELECT query".into()));
            }
            (Sink::Rows, _) => {}
        }
        spec.provenance.check()?;
        if !spec.provenance.is_none() && !matches!(spec.sink, Sink::Rows) {
            return Err(invalid(
                "provenance annotates rows, which an accumulator does not keep".into(),
            ));
        }
        let graphs = access.graphs;
        let whole = whole_reason(graphs.as_ref()).map(str::to_string);
        let compiled = compile(&q, whole.as_deref())?;
        let mut circuit = Circuit::new(compiled, graphs.clone());
        circuit.set_runtime_pool(self.pool.clone(), self.circuit_parent());
        circuit.set_local_limit(Some(spec.max_memory_bytes));
        // the dataset's reading of errors in aggregates when the circuit attaches
        let errors = self.host_or_open().aggregate_errors();
        circuit.aggregate_errors = Some(errors);
        circuit.set_provenance(spec.provenance)?;
        // an accumulator counts the changes and keeps no rows
        circuit.keep_result = matches!(spec.sink, Sink::Rows);
        let columns = circuit.columns();
        let key_cols = match &spec.sink {
            Sink::Rows => Vec::new(),
            Sink::Accumulator { key } => {
                let cols = key
                    .iter()
                    .map(|k| {
                        let k = k.trim_start_matches(['?', '$']);
                        columns.iter().position(|c| c == k).ok_or_else(|| {
                            invalid(format!("the key column ?{k} is not in the query's result"))
                        })
                    })
                    .collect::<Result<Vec<usize>>>()?;
                if key.is_empty() || cols.len() == columns.len() {
                    return Err(invalid(
                        "an accumulator needs key columns and the changed fact's own columns"
                            .into(),
                    ));
                }
                cols
            }
        };
        let fingerprint = {
            let mut h = Sha256::new();
            h.update(COMPILER.as_bytes());
            h.update(b"\0internal\0");
            h.update(spec.id.as_bytes());
            h.update([0]);
            h.update(normalized(&q, &spec.query).as_bytes());
            h.update([0]);
            h.update(access_fingerprint(graphs.as_ref()).to_le_bytes());
            h.update(whole.as_deref().unwrap_or("").as_bytes());
            // whether the default graph is the union of the named graphs changes what
            // the query reads, so a state of the other setting is not loaded
            h.update([u8::from(store.snapshot().union_default_graph)]);
            h.update([0]);
            // and so does the reading of errors in aggregates
            h.update(errors.as_str().as_bytes());
            h.update([0]);
            if !spec.provenance.is_none() {
                h.update(b"provenance\0");
                h.update(spec.provenance.name().as_bytes());
                h.update((spec.provenance.max_witnesses() as u64).to_le_bytes());
                h.update([0]);
            }
            match &spec.sink {
                Sink::Rows => h.update(b"rows"),
                Sink::Accumulator { .. } => {
                    h.update(b"accumulator");
                    for c in &key_cols {
                        h.update((*c as u64).to_le_bytes());
                    }
                }
            }
            u64::from_le_bytes(h.finalize()[..8].try_into().expect("8 bytes"))
        };
        let mut reg = self.reg.lock();
        if self.is_closed() {
            return Err(crate::incr::error(
                "views-unavailable",
                "the dataset is closing",
            ));
        }
        if let Some(c) = reg.internal.get(&spec.id) {
            if c.fingerprint != fingerprint || c.spec != spec {
                return Err(crate::incr::error(
                    "internal-conflict",
                    format!(
                        "another internal circuit {:?} is attached; drop its handles first",
                        spec.id
                    ),
                ));
            }
            c.handles.fetch_add(1, Ordering::AcqRel);
            return Ok(InternalCircuit {
                cell: c.clone(),
                rt: Arc::downgrade(self),
            });
        }
        let cell = Arc::new(InternalCell {
            spec,
            fingerprint,
            state: Mutex::new(InternalSlot {
                status: Status::Building,
                tally: None,
                tally_dirty: Arc::new(AtomicBool::new(false)),
                key_cols,
                checkpointed: 0,
                dirty: false,
                circuit,
            }),
            handles: AtomicUsize::new(1),
            applied: AtomicU64::new(0),
            failed: AtomicBool::new(false),
        });
        // the state is locked before a round can see the circuit, so a round never
        // builds it ahead of its recovery
        let mut st = cell.state.lock();
        reg.internal.insert(cell.spec.id.clone(), cell.clone());
        self.ensure_subscription(store);
        drop(reg);
        if let Some(root) = &self.root {
            self.recover_internal(store, &cell, &mut st, &dir(root, &cell.spec.id));
            // the checkpoint was written under the budget of an earlier attach
            if st.status == Status::Ready
                && let Some(e) = self.internal_budget(&mut st)
            {
                self.fail_internal(&cell, &mut st, &e);
            }
        }
        if st.status == Status::Building {
            self.build_internal(store, &cell, &mut st);
        }
        let failed = status_error(&st.status);
        let retired = st.circuit.drain_retired();
        drop(st);
        drop(retired);
        if let Some(e) = failed {
            // a circuit that could not be built is not attached
            let removed = {
                let mut reg = self.reg.lock();
                if reg
                    .internal
                    .get(&cell.spec.id)
                    .is_some_and(|c| Arc::ptr_eq(c, &cell))
                    && cell.handles.fetch_sub(1, Ordering::AcqRel) == 1
                {
                    reg.internal.remove(&cell.spec.id)
                } else {
                    None
                }
            };
            drop(removed);
            drop(cell);
            return Err(e);
        }
        let handle = InternalCircuit {
            cell,
            rt: Arc::downgrade(self),
        };
        self.kick();
        Ok(handle)
    }

    /// The internal circuits attached.
    pub fn internal_count(&self) -> usize {
        self.reg.lock().internal.len()
    }

    fn build_internal(&self, store: &Store, cell: &InternalCell, st: &mut InternalSlot) {
        let snap = store.snapshot();
        let before = st.circuit.applied;
        let had_state = st.circuit.snap.is_some();
        match st.circuit.build(snap.clone(), &self.control()) {
            Ok(()) => {
                st.status = Status::Ready;
                cell.failed.store(false, Ordering::Release);
                // a build's state is checkpointed even at the commit of the last one,
                // such as commit 0 of a new store
                st.dirty = true;
                if !st.key_cols.is_empty() {
                    if let Some(tally) = st.tally.clone() {
                        // A rebuild cannot count commits since the last applied pass.
                        let last_counted = if had_state {
                            before
                        } else {
                            tally.lock().since
                        };
                        if let Err(e) = Self::record_lost(
                            &st.circuit,
                            &tally,
                            &mut st.dirty,
                            last_counted + 1,
                            snap.commit,
                        ) {
                            self.fail_internal(cell, st, &e);
                        }
                    } else {
                        st.install_tally(Tally::new(st.key_cols.clone(), snap.commit));
                    }
                }
                if st.status == Status::Ready
                    && let Some(e) = self.internal_budget(st)
                {
                    self.fail_internal(cell, st, &e);
                }
            }
            Err(e) => self.fail_internal(cell, st, &e),
        }
        cell.applied.store(st.circuit.applied, Ordering::Release);
    }

    fn fail_internal(&self, cell: &InternalCell, st: &mut InternalSlot, e: &Error) {
        let (code, message) = super::round::failure(e);
        tracing::warn!(
            target: "sparkles::incr::runtime",
            "internal circuit {} failed ({code}): {message}",
            cell.spec.id
        );
        // Clear both circuit state and retained tally storage so a failed handle never
        // pins QueryPool credit while alive (a future rebuild records missed commits as
        // unknown from `since`).
        let applied = st.circuit.applied;
        if let Some(tally) = st.tally.clone() {
            {
                let mut t = tally.lock();
                t.discard_through(applied);
                t.clear_storage();
            }
            st.circuit.set_external_best_effort(0);
        }
        st.circuit.clear_state();
        st.status = Status::Failed { code, message };
        cell.failed.store(true, Ordering::Release);
    }

    /// Reconcile an accumulator's tally bytes against the circuit's budget and parent
    /// pool, shrinking spare capacity first and oldest counts when needed. Never holds
    /// `tally.lock()` across `circuit` budget calls.
    fn charge_tally(
        circuit: &Circuit,
        tally: &Mutex<Tally>,
        dirty: &mut bool,
        applied: u64,
    ) -> Result<()> {
        loop {
            let cur = tally.lock().bytes();
            let Err(e) = circuit.try_set_external(cur) else {
                return Ok(());
            };
            // First try releasing spare backing capacity without dropping any commits.
            let (shrunk_cap, shrunk_to, after) = {
                let mut t = tally.lock();
                let cur = t.bytes();
                if cur > 0 && t.shrink_spare_capacity() {
                    (true, false, t.bytes())
                } else {
                    let dropped = if cur > 0 { t.shrink_to(cur / 2) } else { false };
                    (false, dropped, t.bytes())
                }
            };
            if shrunk_to {
                *dirty = true;
            }
            if shrunk_cap || after < cur {
                continue;
            }
            let discarded_bytes = {
                let mut t = tally.lock();
                if t.since < applied {
                    t.discard_through(applied);
                    *dirty = true;
                    Some(t.bytes())
                } else {
                    None
                }
            };
            if let Some(b) = discarded_bytes
                && circuit.try_set_external(b).is_ok()
            {
                return Ok(());
            }
            // Release all backing capacity before zeroing external accounting so no
            // retained capacity survives uncharged.
            tally.lock().clear_storage();
            circuit.set_external_best_effort(0);
            return Err(e);
        }
    }

    /// Fail the circuit whose state passed its budget, and drop the oldest counts of an
    /// accumulator whose counts take the rest of it.
    fn internal_budget(&self, st: &mut InternalSlot) -> Option<Error> {
        st.sync_tally_dirty();
        let max = st.circuit.max_memory_bytes.unwrap_or(u64::MAX);
        let bytes = st.circuit.bytes();
        if bytes > max {
            return Some(Error::BudgetExceeded(crate::error::Budget {
                kind: crate::error::BudgetKind::Memory,
                limit: max,
                requested: bytes,
            }));
        }
        let applied = st.circuit.applied;
        if let Some(tally) = st.tally.clone() {
            {
                let mut t = tally.lock();
                if t.shrink_to(max - bytes) {
                    st.dirty = true;
                    tracing::warn!(
                        target: "sparkles::incr::runtime",
                        "an internal circuit's counts passed its budget: counts start after commit {}",
                        t.since
                    );
                }
            }
            if let Err(e) = Self::charge_tally(&st.circuit, &tally, &mut st.dirty, applied) {
                if matches!(e, Error::BudgetExceeded(_)) {
                    // Counts alone exceeded the remaining budget: record commits through
                    // `applied` as unavailable per F13 §3.6 without failing the circuit.
                    {
                        let mut t = tally.lock();
                        t.discard_through(applied);
                        t.clear_storage();
                    }
                    st.circuit.set_external_best_effort(0);
                    st.dirty = true;
                } else {
                    return Some(e);
                }
            }
        }
        None
    }

    /// One round's work on internal circuit `cell`: build it, or advance it to the head.
    pub(super) fn round_internal(&self, store: &Store, cell: &InternalCell) -> bool {
        let (worked, retired) = {
            let mut st = cell.state.lock();
            let worked = match st.status {
                Status::Building => {
                    self.build_internal(store, cell, &mut st);
                    true
                }
                Status::Ready => self.advance_internal(store, cell, &mut st),
                Status::Failed { .. } => false,
            };
            cell.applied.store(st.circuit.applied, Ordering::Release);
            (worked, st.circuit.drain_retired())
        };
        drop(retired);
        worked
    }

    fn advance_internal(&self, store: &Store, cell: &InternalCell, st: &mut InternalSlot) -> bool {
        let from = st.circuit.applied;
        if from >= store.snapshot().commit {
            return false;
        }
        let control = self.control();
        let r = if st.tally.is_none() {
            let d = DeltaSet::read(store, from, &Control::none());
            if d.snapshot.commit <= from {
                return false;
            }
            st.circuit.advance(&d, &control).map(|_| ())
        } else {
            self.step_accumulator(store, st, from, &control)
        };
        let r = r.and_then(|()| self.internal_budget(st).map_or(Ok(()), Err));
        if let Err(e) = r {
            self.fail_internal(cell, st, &e);
            return true;
        }
        // the pass is applied in memory and not yet checkpointed (a test hook; it must
        // not call the runtime, whose locks the pass holds)
        store.failpoint("internal-pass-applied");
        true
    }

    /// Advance an accumulator's circuit one commit at a time, counting each commit's
    /// change before the next is read (spec F13 §3.6). Changes that are not known per
    /// commit evaluate the circuit again at the head and are recorded as unknown.
    /// Pre-admit an unknown count range. If even an empty tally cannot store
    /// it, advance the first readable commit past the range.
    fn record_lost(
        circuit: &Circuit,
        tally: &Mutex<Tally>,
        dirty: &mut bool,
        first: u64,
        last: u64,
    ) -> Result<()> {
        if first > last {
            return Ok(());
        }
        *dirty = true;
        loop {
            let estimate = tally.lock().bytes_after_loss(first, last);
            match circuit.try_set_external(estimate) {
                Ok(()) => {
                    let bytes = {
                        let mut t = tally.lock();
                        t.lose(first, last);
                        t.bytes()
                    };
                    circuit.set_external_best_effort(bytes);
                    return Ok(());
                }
                Err(Error::BudgetExceeded(_)) => {
                    let (freed, bytes) = {
                        let mut t = tally.lock();
                        let target = (t.bytes() / 2).max(1);
                        let (freed, _) = t.evict_for_reclaim(target);
                        if freed == 0 {
                            t.discard_through(last);
                            t.clear_storage();
                        }
                        (freed, t.bytes())
                    };
                    circuit.set_external_best_effort(bytes);
                    if freed == 0 {
                        return Ok(());
                    }
                }
                Err(e) => return Err(e),
            }
        }
    }

    fn step_accumulator(
        &self,
        store: &Store,
        st: &mut InternalSlot,
        from: u64,
        control: &Control,
    ) -> Result<()> {
        let InternalSlot {
            circuit,
            tally,
            dirty,
            ..
        } = st;
        let tally = tally.as_ref().expect("an accumulator").clone();
        let max = circuit.max_memory_bytes.unwrap_or(u64::MAX);
        let Some(mut steps) = DeltaSet::steps(store, from, control) else {
            let snap = store.snapshot();
            let (diff, diff_lease) = circuit.reset_charged(&DeltaSet::at(snap.clone()), control)?;
            drop(diff);
            drop(diff_lease);
            let state = circuit.bytes();
            if state > max {
                return Err(Error::BudgetExceeded(crate::error::Budget {
                    kind: crate::error::BudgetKind::Memory,
                    limit: max,
                    requested: state,
                }));
            }
            Self::record_lost(circuit, &tally, dirty, from + 1, snap.commit)?;
            return Ok(());
        };
        while let Some(d) = steps.next() {
            let d = d?;
            let at = d.snapshot.commit;
            match circuit.advance_charged(&d, control)? {
                crate::incr::circuit::ChargedAdvance::Skipped => {}
                crate::incr::circuit::ChargedAdvance::Changed(c, c_lease) => {
                    // Check actual circuit state first: state over budget fails the circuit.
                    let state = circuit.bytes();
                    if state > max {
                        drop(c);
                        drop(c_lease);
                        return Err(Error::BudgetExceeded(crate::error::Budget {
                            kind: crate::error::BudgetKind::Memory,
                            limit: max,
                            requested: state,
                        }));
                    }
                    let snap = d.snapshot.clone();
                    let terms = &circuit.terms;
                    Tally::add_budgeted_charged_shared(
                        &tally,
                        at,
                        c,
                        c_lease,
                        |i, lease| terms.term_budgeted(&snap, i, lease),
                        Some(circuit),
                        dirty,
                    )?;
                }
                crate::incr::circuit::ChargedAdvance::Reset(c, c_lease) => {
                    drop(c);
                    drop(c_lease);
                    let state = circuit.bytes();
                    if state > max {
                        return Err(Error::BudgetExceeded(crate::error::Budget {
                            kind: crate::error::BudgetKind::Memory,
                            limit: max,
                            requested: state,
                        }));
                    }
                    Self::record_lost(circuit, &tally, dirty, d.after + 1, at)?;
                }
            }
        }
        Ok(())
    }

    /// Checkpoint every internal circuit whose state moved since its last checkpoint.
    pub(super) fn checkpoint_internal(&self, store: &Store, cells: &[Arc<InternalCell>]) {
        let Some(root) = self.root.clone() else {
            return;
        };
        for cell in cells {
            let mut st = cell.state.lock();
            st.sync_tally_dirty();
            if st.status != Status::Ready {
                continue;
            }
            let applied = st.circuit.applied;
            if applied <= st.checkpointed && !st.dirty {
                continue;
            }
            let Some(snap) = st.circuit.snap.clone() else {
                continue;
            };
            let credit = CheckpointCredit(&st.circuit);
            let r = st
                .circuit
                .encode_charged(&snap)
                .and_then(|(body, mut lease)| {
                    let tally_bytes = st.tally.as_ref().map_or(0, |t| t.lock().bytes());
                    lease.try_reserve(
                        (body.len() as u64)
                            .saturating_mul(3)
                            .saturating_add(tally_bytes.saturating_mul(8))
                            .saturating_add(64),
                    )?;
                    let mut e = Enc::default();
                    e.u64(body.len() as u64);
                    e.raw(&body);
                    match &st.tally {
                        None => e.u64(0),
                        Some(t) => {
                            e.u64(1);
                            t.lock().encode(&mut e);
                        }
                    }
                    persist::write_as(
                        checkpoint_kind(&st.circuit),
                        &dir(&root, &cell.spec.id),
                        &persist::Header {
                            dataset: self.dataset,
                            commit: applied,
                            fingerprint: cell.fingerprint,
                        },
                        &e.0,
                        || store.failpoint("internal-checkpoint-written"),
                    )
                });
            drop(credit);
            match r {
                Ok(()) => {
                    st.checkpointed = applied;
                    st.dirty = false;
                    self.counters.checkpoints.fetch_add(1, Ordering::Relaxed);
                }
                Err(e) => tracing::warn!(
                    target: "sparkles::incr::runtime",
                    "internal circuit {}: checkpoint at commit {applied}: {e}",
                    cell.spec.id
                ),
            }
        }
    }

    /// Load the newest checkpoint of `cell` that this release reads.
    fn recover_internal(
        &self,
        store: &Store,
        cell: &InternalCell,
        st: &mut InternalSlot,
        dir: &std::path::Path,
    ) {
        let snap = store.snapshot();
        let id = &cell.spec.id;
        for (commit, path) in persist::list(dir) {
            let mut file_lease =
                super::super::budget::ScratchLease::new_pool_only(Some(st.circuit.budget_handle()));
            let bytes = match super::attach::read_bounded_file(&path, &mut file_lease) {
                Ok(Some(bytes)) => bytes,
                Ok(None) => {
                    drop(file_lease);
                    continue;
                }
                Err(e) => {
                    tracing::warn!(
                        target: "sparkles::incr::runtime",
                        "internal circuit {id}: checkpoint {}: {e}; the circuit is built again",
                        path.display()
                    );
                    drop(file_lease);
                    continue;
                }
            };
            let (h, body) = match persist::decode_as(checkpoint_kind(&st.circuit), &bytes) {
                Ok(x) => x,
                Err(e) => {
                    tracing::warn!(
                        target: "sparkles::incr::runtime",
                        "internal circuit {id}: checkpoint {}: {e}; it is left in place",
                        path.display()
                    );
                    continue;
                }
            };
            if h.dataset != self.dataset
                || h.fingerprint != cell.fingerprint
                || commit != h.commit
                || h.commit > snap.commit
            {
                continue;
            }
            let mut tally_lease =
                super::super::budget::ScratchLease::new_pool_only(Some(st.circuit.budget_handle()));
            let r = (|| {
                let mut d = Dec { b: body, at: 0 };
                let n = d.u64()? as usize;
                let state = d.raw(n)?;
                let tally = match (d.u64()?, st.key_cols.is_empty()) {
                    (0, true) => None,
                    (1, false) => Some(Tally::decode_budgeted(
                        st.key_cols.clone(),
                        &mut d,
                        &mut tally_lease,
                    )?),
                    _ => {
                        return Err(Error::Corrupt(
                            "an internal circuit's state has another sink".into(),
                        ));
                    }
                };
                if !d.done() {
                    return Err(Error::Corrupt(
                        "an internal circuit's state has trailing bytes".into(),
                    ));
                }
                st.circuit.decode_with_input_charged(state, &snap)?;
                Ok(tally)
            })();
            // Drop raw checkpoint bytes before releasing `file_lease`.
            drop(bytes);
            drop(file_lease);
            match r {
                Ok(tally) => {
                    if let Some(mut t) = tally {
                        if let Some(max) = st.circuit.max_memory_bytes {
                            let avail = max.saturating_sub(st.circuit.bytes());
                            if t.shrink_to(avail) {
                                st.dirty = true;
                            }
                        }
                        let t_bytes = t.bytes();
                        let tally_arc = Arc::new(Mutex::new(t));
                        if let Err(e) = st
                            .circuit
                            .commit_external_from_scratch(t_bytes, &mut tally_lease)
                        {
                            drop(tally_lease);
                            if Self::charge_tally(&st.circuit, &tally_arc, &mut st.dirty, h.commit)
                                .is_err()
                            {
                                tracing::warn!(
                                    target: "sparkles::incr::runtime",
                                    "internal circuit {id}: checkpoint {}: {e}; the circuit is built again",
                                    path.display()
                                );
                                drop(tally_arc);
                                st.circuit.clear_state();
                                continue;
                            }
                        }
                        st.circuit
                            .set_reclaim_external(Some(InternalSlot::tally_reclaim_hook(
                                &tally_arc,
                                &st.tally_dirty,
                            )));
                        st.tally = Some(tally_arc);
                    } else {
                        drop(tally_lease);
                        st.tally = None;
                    }
                    st.circuit.applied = h.commit;
                    st.checkpointed = h.commit;
                    st.status = Status::Ready;
                    cell.failed.store(false, Ordering::Release);
                    cell.applied.store(h.commit, Ordering::Release);
                    self.counters.loaded.fetch_add(1, Ordering::Relaxed);
                    tracing::info!(
                        target: "sparkles::incr::runtime",
                        "internal circuit {id}: loaded its state at commit {}; {} commits to apply",
                        h.commit,
                        snap.commit - h.commit
                    );
                    return;
                }
                Err(e) => {
                    tracing::warn!(
                        target: "sparkles::incr::runtime",
                        "internal circuit {id}: checkpoint {}: {e}; the circuit is built again",
                        path.display()
                    );
                    drop(tally_lease);
                    st.circuit.set_external_best_effort(0);
                    st.circuit.clear_state();
                }
            }
        }
    }

    /// Drop the internal circuits whose handles are all gone, after checkpointing them.
    /// A circuit stays registered while its final checkpoint is written, so an attach
    /// of the same id meanwhile joins it rather than building a new circuit that would
    /// miss the checkpoint, and such a circuit is then kept.
    pub(super) fn collect_internal(&self, store: &Store) {
        let idle: Vec<Arc<InternalCell>> = self
            .reg
            .lock()
            .internal
            .values()
            .filter(|c| c.handles.load(Ordering::Acquire) == 0)
            .cloned()
            .collect();
        if idle.is_empty() {
            return;
        }
        self.checkpoint_internal(store, &idle);
        let mut dead: Vec<Arc<InternalCell>> = Vec::new();
        {
            let mut reg = self.reg.lock();
            for c in &idle {
                if c.handles.load(Ordering::Acquire) == 0
                    && reg
                        .internal
                        .get(&c.spec.id)
                        .is_some_and(|r| Arc::ptr_eq(r, c))
                    && let Some(removed) = reg.internal.remove(&c.spec.id)
                {
                    dead.push(removed);
                }
            }
        }
        drop(dead);
        drop(idle);
    }
}

#[cfg(test)]
mod charge_tally_tests {
    use super::*;
    use crate::sparql::parse_query;

    #[test]
    fn charge_tally_preserves_counts_on_capacity_only_shrink_and_zeroes_on_terminal_refusal() {
        let q = parse_query("SELECT ?s ?o WHERE { ?s <urn:p> ?o }", None, &[]).unwrap();
        let mut circuit = Circuit::new(compile(&q, None).unwrap(), None);
        let pool = QueryPool::new(1 << 20);
        circuit.set_runtime_pool(Some(pool.clone()), Some(pool.work_parent()));

        // Populate 50 commits (VecDeque capacity 64), then discard 1..=45, keeping
        // 46..=50 (5 entries) while retaining spare VecDeque capacity (`64 <= 5 + 1 + 64`).
        let mut tally = Tally::new(vec![0], 0);
        let s_id = crate::id::Id::from_i64(1).unwrap();
        let o_id = crate::id::Id::from_i64(2).unwrap();
        let row: crate::incr::zset::Row = vec![s_id, o_id].into();
        for c in 1..=50u64 {
            tally.add(c, &vec![(row.clone(), 1)], |i| {
                Some(oxrdf::Term::Literal(oxrdf::Literal::new_simple_literal(
                    format!("k{}", i.0),
                )))
            });
        }
        tally.discard_through(45);
        let before_bytes = tally.bytes();
        // Set local limit just below `before_bytes` so spare-capacity shrink alone suffices.
        let tight = before_bytes.saturating_sub(1);
        circuit.set_local_limit(Some(tight));
        let tally = Mutex::new(tally);
        let mut dirty = false;
        let res = Runtime::charge_tally(&circuit, &tally, &mut dirty, 50);
        assert!(
            res.is_ok(),
            "capacity-only shrink must succeed without wiping counts: {res:?}"
        );
        assert!(!dirty, "capacity-only shrink must not mark counts dirty");
        assert!(tally.lock().bytes() <= tight);
        let counts = tally.lock().counts(45, 50);
        assert_eq!(
            counts.unknown, None,
            "capacity-only shrink must preserve accumulated counts"
        );
        assert_eq!(counts.keys.len(), 1);
        assert_eq!(counts.keys[0].1, 5);

        // Now build circuit state (so `state_bytes > 0`) and lower `local_limit` to 0 to
        // force terminal refusal: `charge_tally` must release all retained tally storage
        // so `tally.bytes() == 0` and pool credit returns to 0 on drop.
        let store = Store::in_memory(crate::store::StoreOptions::default());
        crate::sparql::update::update(
            &store,
            "INSERT DATA { <urn:a> <urn:p> \"1\" }",
            &crate::sparql::QueryOptions::default(),
        )
        .unwrap();
        circuit.set_local_limit(None);
        circuit.build(store.snapshot(), &Control::none()).unwrap();
        assert!(circuit.bytes() > 0);
        circuit.set_local_limit(Some(0));
        tally.lock().add(51, &vec![(row, 1)], |i| {
            Some(oxrdf::Term::Literal(oxrdf::Literal::new_simple_literal(
                format!("k{}", i.0),
            )))
        });
        let err = Runtime::charge_tally(&circuit, &tally, &mut dirty, 51);
        assert!(err.is_err());
        assert_eq!(
            tally.lock().bytes(),
            0,
            "terminal refusal in charge_tally must clear retained tally storage"
        );
        drop(circuit);
        assert_eq!(pool.granted(), 0);
    }
}
