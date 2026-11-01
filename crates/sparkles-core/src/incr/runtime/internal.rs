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
    self as internal, Counts, Dec, Enc, InternalAccess, InternalRows, InternalSpec, Sink, Tally,
};
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
}

pub(super) struct InternalSlot {
    pub(super) circuit: Circuit,
    pub(super) status: Status,
    /// the counts of an accumulator, from its first build
    pub(super) tally: Option<Tally>,
    pub(super) key_cols: Vec<usize>,
    pub(super) checkpointed: u64,
    /// the counts changed without a pass (counts were discarded)
    pub(super) dirty: bool,
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
            if self.commit() >= commit {
                return true;
            }
            let Some(rt) = self.rt.upgrade() else {
                return false;
            };
            if rt.is_closed() {
                return false;
            }
            rt.kick();
            if !rt.wait_round(until) {
                return self.commit() >= commit;
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
        Ok(tally.counts(after, st.circuit.applied))
    }

    /// Drop an accumulator's counts of the commits up to `commit`, which server code
    /// has acted on. Later reads count from `commit` at the earliest.
    pub fn discard_through(&self, commit: u64) {
        let mut st = self.cell.state.lock();
        let through = commit.min(st.circuit.applied);
        if let Some(t) = st.tally.as_mut() {
            t.discard_through(through);
            st.dirty = true;
        }
    }

    /// The circuit's rows at its commit.
    pub fn rows(&self) -> Result<InternalRows> {
        let st = self.cell.state.lock();
        if let Some(e) = status_error(&st.status) {
            return Err(e);
        }
        Ok(InternalRows {
            commit: st.circuit.applied,
            columns: st.circuit.columns(),
            rows: st
                .circuit
                .rows()
                .into_iter()
                .map(|(r, w)| (r, w.unsigned_abs()))
                .collect(),
        })
    }

    /// The circuit's state's approximate memory, its counts' included.
    pub fn bytes(&self) -> u64 {
        let st = self.cell.state.lock();
        st.circuit.bytes() + st.tally.as_ref().map_or(0, Tally::bytes)
    }

    /// The most memory the counts took right after a commit was counted, and the drops
    /// that the budget made.
    #[cfg(test)]
    pub(crate) fn tally_peak(&self) -> (u64, u64) {
        self.cell
            .state
            .lock()
            .tally
            .as_ref()
            .map_or((0, 0), |t| (t.peak, t.drops))
    }

    /// Evaluate the circuit again from scratch on the next round, lifting a failure.
    /// An accumulator keeps its counts and records the commits since its last pass as
    /// unknown.
    pub fn rebuild(&self) {
        self.cell.state.lock().status = Status::Building;
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
        let graphs = access.graphs;
        let whole = whole_reason(graphs.as_ref()).map(str::to_string);
        let compiled = compile(&q, whole.as_deref())?;
        let mut circuit = Circuit::new(compiled, graphs.clone());
        circuit.max_memory_bytes = Some(spec.max_memory_bytes);
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
                circuit,
                status: Status::Building,
                tally: None,
                key_cols,
                checkpointed: 0,
                dirty: false,
            }),
            handles: AtomicUsize::new(1),
            applied: AtomicU64::new(0),
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
        drop(st);
        if let Some(e) = failed {
            // a circuit that could not be built is not attached
            let mut reg = self.reg.lock();
            if reg
                .internal
                .get(&cell.spec.id)
                .is_some_and(|c| Arc::ptr_eq(c, &cell))
                && cell.handles.fetch_sub(1, Ordering::AcqRel) == 1
            {
                reg.internal.remove(&cell.spec.id);
            }
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
                // a build's state is checkpointed even at the commit of the last one,
                // such as commit 0 of a new store
                st.dirty = true;
                if !st.key_cols.is_empty() {
                    match st.tally.as_mut() {
                        // a rebuild: the commits since the last pass are not counted
                        Some(t) if had_state => t.lose(before + 1, snap.commit),
                        Some(t) => t.lose(t.since + 1, snap.commit),
                        None => st.tally = Some(Tally::new(st.key_cols.clone(), snap.commit)),
                    }
                }
                if let Some(e) = self.internal_budget(st) {
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
        st.status = Status::Failed { code, message };
    }

    /// Fail the circuit whose state passed its budget, and drop the oldest counts of an
    /// accumulator whose counts take the rest of it.
    fn internal_budget(&self, st: &mut InternalSlot) -> Option<Error> {
        let max = st.circuit.max_memory_bytes.unwrap_or(u64::MAX);
        let bytes = st.circuit.bytes();
        if bytes > max {
            return Some(Error::BudgetExceeded(crate::error::Budget {
                kind: crate::error::BudgetKind::Memory,
                limit: max,
                requested: bytes,
            }));
        }
        if let Some(t) = st.tally.as_mut()
            && t.shrink_to(max - bytes)
        {
            st.dirty = true;
            tracing::warn!(
                target: "sparkles::incr::runtime",
                "an internal circuit's counts passed its budget: counts start after commit {}",
                t.since
            );
        }
        None
    }

    /// One round's work on internal circuit `cell`: build it, or advance it to the head.
    pub(super) fn round_internal(&self, store: &Store, cell: &InternalCell) -> bool {
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
        let tally = tally.as_mut().expect("an accumulator");
        let max = circuit.max_memory_bytes.unwrap_or(u64::MAX);
        let Some(mut steps) = DeltaSet::steps(store, from, control) else {
            let snap = store.snapshot();
            circuit.reset(&DeltaSet::at(snap.clone()), control)?;
            tally.lose(from + 1, snap.commit);
            return Ok(());
        };
        while let Some(d) = steps.next() {
            let d = d?;
            let at = d.snapshot.commit;
            match circuit.advance(&d, control)? {
                Advance::Skipped => {}
                Advance::Changed(c) => {
                    let snap = d.snapshot.clone();
                    let terms = &circuit.terms;
                    tally.add(at, &c, |i| terms.term(&snap, i));
                    // the budget holds after each commit of a run, not only at its end
                    let state = circuit.bytes();
                    if state > max {
                        return Err(Error::BudgetExceeded(crate::error::Budget {
                            kind: crate::error::BudgetKind::Memory,
                            limit: max,
                            requested: state,
                        }));
                    }
                    if tally.shrink_to(max - state) {
                        *dirty = true;
                    }
                }
                Advance::Reset(_) => tally.lose(d.after + 1, at),
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
            let r = st.circuit.encode(&snap).and_then(|body| {
                let mut e = Enc::default();
                e.u64(body.len() as u64);
                e.raw(&body);
                match &st.tally {
                    None => e.u64(0),
                    Some(t) => {
                        e.u64(1);
                        t.encode(&mut e);
                    }
                }
                persist::write_as(
                    persist::INTERNAL,
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
            let Ok(bytes) = std::fs::read(&path) else {
                continue;
            };
            let (h, body) = match persist::decode_as(persist::INTERNAL, &bytes) {
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
            let r = (|| {
                let mut d = Dec { b: body, at: 0 };
                let n = d.u64()? as usize;
                let state = d.raw(n)?;
                let tally = match (d.u64()?, st.key_cols.is_empty()) {
                    (0, true) => None,
                    (1, false) => Some(Tally::decode(st.key_cols.clone(), &mut d)?),
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
                st.circuit.decode(state, &snap)?;
                Ok(tally)
            })();
            match r {
                Ok(tally) => {
                    st.tally = tally;
                    st.circuit.applied = h.commit;
                    st.checkpointed = h.commit;
                    st.status = Status::Ready;
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
                    st.circuit.result = Default::default();
                    st.circuit.snap = None;
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
        let mut reg = self.reg.lock();
        for c in idle {
            if c.handles.load(Ordering::Acquire) == 0
                && reg
                    .internal
                    .get(&c.spec.id)
                    .is_some_and(|r| Arc::ptr_eq(r, &c))
            {
                reg.internal.remove(&c.spec.id);
            }
        }
    }
}
