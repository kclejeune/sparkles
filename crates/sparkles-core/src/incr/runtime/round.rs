//! A round of passes after commits (spec F13 §3.3), with budgets, collection and
//! checkpoints.

use super::*;

#[cfg(test)]
thread_local! {
    /// a cancellation flag that the passes of rounds on this thread obey, for tests of a
    /// pass that fails midway
    pub(crate) static PASS_CANCEL: std::cell::RefCell<Option<Arc<std::sync::atomic::AtomicBool>>> =
        const { std::cell::RefCell::new(None) };
}

pub(super) fn failure(e: &Error) -> (String, String) {
    let code = match e {
        Error::Timeout | Error::Cancelled => "pass-timeout",
        Error::BudgetExceeded(_) => "over-budget",
        _ => "view-failed",
    };
    (code.to_string(), e.to_string())
}

#[derive(Clone, Copy)]
pub(super) enum PassKind {
    Build,
    Advance,
    Skip,
}

enum CellPass {
    Done,
    Busy,
    Rotated,
}

impl Runtime {
    /// Run one round: attach pending views, build what needs building and advance
    /// every circuit to the store's head. Returns whether anything was done.
    pub fn round(&self, store: &Store) -> bool {
        let _operation = Operation::enter();
        if self.closed.load(Ordering::Acquire) {
            return false;
        }
        self.attach_pending(store);
        let (target, cells) = {
            let reg = self.reg.lock();
            (
                reg.target_mode(),
                reg.cells.values().cloned().collect::<Vec<_>>(),
            )
        };
        let mut deltas: FxHashMap<u64, Arc<DeltaSet>> = FxHashMap::default();
        let mut worked = false;
        let mut mode_rotated = false;
        let mut over: Vec<(Arc<Cell>, u64)> = Vec::new();
        let mut busy: Vec<Arc<Cell>> = Vec::new();
        // Process idle cells first so a concurrent pass or live-query build on one
        // circuit cannot delay unrelated views from switching mode and rebuilding.
        for cell in &cells {
            if self.closed.load(Ordering::Acquire)
                || !self.mode_is_current(target.generation, target.host_seq)
            {
                mode_rotated = true;
                break;
            }
            match self.try_round_cell(store, cell, &target, &mut deltas, &mut worked) {
                CellPass::Done => {
                    let bytes = cell.info.lock().bytes;
                    over.push((cell.clone(), bytes));
                }
                CellPass::Busy => {
                    busy.push(cell.clone());
                }
                CellPass::Rotated => {
                    mode_rotated = true;
                    break;
                }
            }
        }
        if !mode_rotated {
            for cell in busy.drain(..) {
                let outcome = {
                    let Some(st) = self.wait_for_busy_cell(&cell, &target) else {
                        mode_rotated = true;
                        break;
                    };
                    self.round_cell_locked(
                        store,
                        &cell,
                        st,
                        &target,
                        &mut deltas,
                        &mut worked,
                        true,
                    )
                };
                match outcome {
                    CellPass::Done => {
                        let bytes = cell.info.lock().bytes;
                        over.push((cell, bytes));
                    }
                    CellPass::Busy => {}
                    CellPass::Rotated => {
                        mode_rotated = true;
                        break;
                    }
                }
            }
        }
        drop(busy);
        if !mode_rotated {
            self.mark_mode_switched(target.generation);
            let internal: Vec<Arc<InternalCell>> =
                self.reg.lock().internal.values().cloned().collect();
            for cell in &internal {
                if cell.handles.load(Ordering::Acquire) > 0 && self.round_internal(store, cell) {
                    worked = true;
                }
            }
            drop(internal);
            self.collect_internal(store);
        }
        self.budgets(&over);
        drop(over);
        drop(cells);
        self.gc();
        if !mode_rotated && self.root.is_some() {
            let due = self.last_checkpoint.lock().elapsed()
                >= Duration::from_secs(self.opts.read().checkpoint_seconds);
            if due {
                self.checkpoint(store);
            }
        }
        *self.epoch.lock() += 1;
        self.cv.notify_all();
        if mode_rotated && !self.closed.load(Ordering::Acquire) {
            self.kick();
        }
        worked
    }

    fn try_round_cell(
        &self,
        store: &Store,
        cell: &Arc<Cell>,
        target: &TargetMode,
        deltas: &mut FxHashMap<u64, Arc<DeltaSet>>,
        worked: &mut bool,
    ) -> CellPass {
        let Some(st) = cell.state.try_lock() else {
            return CellPass::Busy;
        };
        self.round_cell_locked(store, cell, st, target, deltas, worked, false)
    }

    fn wait_for_busy_cell<'a>(
        &self,
        cell: &'a Cell,
        target: &TargetMode,
    ) -> Option<parking_lot::MutexGuard<'a, Slot>> {
        loop {
            if self.closed.load(Ordering::Acquire) || self.mode_changed_since(target) {
                return None;
            }
            if let Some(st) = cell.state.try_lock_for(read::WAIT_STEP) {
                #[cfg(test)]
                if let Some(hook) = self.busy_locked_hook.lock().clone() {
                    hook();
                }
                // Hosts can consult the registry while a catalog attach holds it
                // and waits for this cell. Release state before probing the host.
                drop(st);
                if self.mode_changed_since(target) {
                    return None;
                }
                if let Some(st) = cell.state.try_lock() {
                    return (!self.closed.load(Ordering::Acquire)
                        && self.mode_is_current(target.generation, target.host_seq))
                    .then_some(st);
                }
            }
            #[cfg(test)]
            self.busy_waits.fetch_add(1, Ordering::Release);
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn round_cell_locked<'a>(
        &self,
        store: &Store,
        cell: &'a Arc<Cell>,
        state: parking_lot::MutexGuard<'a, Slot>,
        target: &TargetMode,
        deltas: &mut FxHashMap<u64, Arc<DeltaSet>>,
        worked: &mut bool,
        blocking: bool,
    ) -> CellPass {
        // Declare deferred drops first so an unwind releases the state lock before
        // returning retired credit, which may synchronously wake a pool waiter.
        let mut deferred = DeferredDrops::default();
        let mut st = state;
        // Release replaced operators and detached budget leases outside `cell.state`
        // before starting the replacement build so tight shared pools can recycle the
        // freed credit instead of failing with `over-budget`.
        if st.pending_retirement().is_some() {
            return CellPass::Busy;
        }
        if st.mode_gen < target.generation {
            let switched = self.switch_cell(cell, &mut st, target, &mut deferred);
            drop(st);
            drop(std::mem::take(&mut deferred));
            if switched {
                *worked = true;
            }
            if self.closed.load(Ordering::Acquire)
                || !self.mode_is_current(target.generation, target.host_seq)
            {
                return CellPass::Rotated;
            }
            st = if blocking {
                let Some(st) = self.wait_for_busy_cell(cell, target) else {
                    return CellPass::Rotated;
                };
                st
            } else {
                let Some(st) = cell.state.try_lock() else {
                    return CellPass::Busy;
                };
                st
            };
        }

        // Revalidate that no newer mode generation was recorded before starting a build
        // or advance pass under `target`.
        if self.closed.load(Ordering::Acquire)
            || !self.mode_is_current(target.generation, target.host_seq)
            || st.mode_gen < target.generation
        {
            drop(st);
            return CellPass::Rotated;
        }

        if st.pending_retirement().is_some() {
            return CellPass::Busy;
        }
        // Prune departed or explicitly closed live subscribers before admitting
        // a build or advance pass so the first pass uses the exact active ceiling.
        st.prune_dead_subs();
        let status = cell.info.lock().status.clone();
        match status {
            Status::Building => {
                self.build(store, cell, &mut st, &mut deferred);
                *worked = true;
            }
            Status::Ready => {
                if self.advance(store, cell, &mut st, deltas, &mut deferred) {
                    *worked = true;
                }
            }
            Status::Failed { .. } => {}
        }
        deferred.retired.extend(st.circuit.drain_retired());
        drop(st);
        drop(deferred);
        CellPass::Done
    }

    pub(super) fn control(&self) -> Control {
        let deadline = Some(Instant::now() + self.opts.read().pass_timeout);
        #[cfg(test)]
        if let Some(flag) = PASS_CANCEL.with(|c| c.borrow().clone()) {
            return Control {
                deadline,
                ..Control::with_cancel(flag)
            };
        }
        Control {
            deadline,
            ..Control::none()
        }
    }

    pub(super) fn note_pass(&self, cell: &Cell, st: &Slot, took: Duration, kind: PassKind) {
        self.seen.fetch_max(st.circuit.applied, Ordering::AcqRel);
        let rows = st.circuit.result.total().max(0) as u64;
        let bytes = st.circuit.bytes();
        let published_bytes = st
            .circuit
            .version
            .as_ref()
            .map_or(0, |v| v.published_bytes());
        #[cfg(test)]
        if let Some(hook) = self.note_pass_hook.lock().clone() {
            hook();
        }
        let mut info = cell.info.lock();
        info.applied = st.circuit.applied;
        info.rows = rows;
        info.bytes = bytes;
        info.published_bytes = published_bytes;
        if matches!(kind, PassKind::Build) {
            info.status = Status::Ready;
        }
        if matches!(kind, PassKind::Skip) {
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

    pub(super) fn fail(&self, cell: &Cell, st: &mut Slot, e: &Error, deferred: &mut DeferredDrops) {
        self.fail_at_mode_gen(cell, st, e, None, deferred);
    }

    pub(super) fn fail_at_mode_gen(
        &self,
        cell: &Cell,
        st: &mut Slot,
        e: &Error,
        mode_gen: Option<u64>,
        deferred: &mut DeferredDrops,
    ) {
        let (code, message) = failure(e);
        tracing::warn!(
            target: "sparkles::incr::runtime",
            "a view circuit failed ({code}): {message}"
        );
        st.fail_live_subs(e, deferred);
        st.clear_history();
        st.circuit.clear_state();
        let mut info = cell.info.lock();
        if let Some(generation) = mode_gen {
            info.mode_gen = generation;
        }
        info.rows = 0;
        info.bytes = 0;
        info.status = Status::Failed { code, message };
    }

    pub(super) fn build(
        &self,
        store: &Store,
        cell: &Cell,
        st: &mut Slot,
        deferred: &mut DeferredDrops,
    ) {
        let snap = store.snapshot();
        let before = st.circuit.applied;
        let had_snap = st.circuit.snap.is_some();
        let restricted = st
            .circuit
            .access
            .as_ref()
            .is_some_and(|a| !a.reads_everything());
        let prev_stamp = st.circuit.evidence_stamp();
        let ev_mismatch = had_snap && st.evidence_mismatch(&snap);
        let t0 = Instant::now();
        // a rebuild's change reaches live queries
        match st.evaluate(snap, &self.control()) {
            Ok(change) => {
                self.note_pass(cell, st, t0.elapsed(), PassKind::Build);
                let ev_boundary_crossed = restricted
                    && had_snap
                    && (ev_mismatch || st.circuit.evidence_stamp() != prev_stamp);
                if ev_boundary_crossed {
                    drop(change);
                    st.rebased = false;
                    self.live_budgets_inner(st, true, deferred);
                    self.publish_evidence_reset(st, deferred);
                } else {
                    self.live_budgets(st, deferred);
                    if let Some((c, c_lease)) = change {
                        self.publish(st, before, c, c_lease, true, deferred);
                    }
                    let ev_restricted = restricted && st.circuit.evidence_key().is_some();
                    if std::mem::take(&mut st.rebased) || ev_restricted {
                        st.clear_history();
                    }
                }
            }
            Err(e) => self.fail(cell, st, &e, deferred),
        }
    }

    pub(super) fn advance(
        &self,
        store: &Store,
        cell: &Cell,
        st: &mut Slot,
        deltas: &mut FxHashMap<u64, Arc<DeltaSet>>,
        deferred: &mut DeferredDrops,
    ) -> bool {
        if !st.alive() {
            return false;
        }
        let from = st.circuit.applied;
        let head = store.snapshot();
        let restricted = st
            .circuit
            .access
            .as_ref()
            .is_some_and(|a| !a.reads_everything());
        let prev_stamp = st.circuit.evidence_stamp();
        let ev_changed = st.evidence_mismatch(&head);
        if from >= head.commit && !ev_changed {
            return false;
        }
        if from >= head.commit && ev_changed {
            // Managed settings, protection nonces, or availability changed without an RDF
            // commit: rebuild at `head`, clear replay history, and replace subscriber queues
            // with a fresh reset so no superseded events or same-commit replay diffs remain.
            cell.info.lock().status = Status::Building;
            let t0 = Instant::now();
            match st
                .circuit
                .reset_charged(&DeltaSet::at(head), &self.control())
            {
                Ok((c, c_lease)) => {
                    drop(c);
                    drop(c_lease);
                    self.counters.resets.fetch_add(1, Ordering::Relaxed);
                    self.note_pass(cell, st, t0.elapsed(), PassKind::Build);
                    self.live_budgets_inner(st, true, deferred);
                    self.publish_evidence_reset(st, deferred);
                }
                Err(e) => {
                    st.clear_history();
                    self.fail(cell, st, &e, deferred);
                    return true;
                }
            }
            return true;
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
        let r = st.circuit.advance_charged(&d, &self.control());
        let took = t0.elapsed();
        match r {
            Ok(crate::incr::circuit::ChargedAdvance::Skipped) => {
                self.note_pass(cell, st, took, PassKind::Skip);
                let to = st.circuit.applied;
                let max = self.opts.read().history_commits;
                st.remember(from, to, Arc::from(Vec::new()), max);
            }
            Ok(crate::incr::circuit::ChargedAdvance::Changed(c, c_lease)) => {
                self.note_pass(cell, st, took, PassKind::Advance);
                self.publish(st, from, c, c_lease, false, deferred);
            }
            Ok(crate::incr::circuit::ChargedAdvance::Reset(c, c_lease)) => {
                self.counters.resets.fetch_add(1, Ordering::Relaxed);
                if from > 0 {
                    tracing::info!(
                        target: "sparkles::incr::runtime",
                        "a view circuit was evaluated again at commit {}: the changes after commit {from} are not all known",
                        st.circuit.applied
                    );
                }
                self.note_pass(cell, st, took, PassKind::Build);
                let ev_now_changed =
                    restricted && (ev_changed || st.circuit.evidence_stamp() != prev_stamp);
                if ev_now_changed {
                    drop(c);
                    drop(c_lease);
                    self.live_budgets_inner(st, true, deferred);
                    self.publish_evidence_reset(st, deferred);
                } else {
                    self.publish(st, from, c, c_lease, true, deferred);
                    if restricted && st.circuit.evidence_key().is_some() {
                        st.clear_history();
                    }
                }
            }
            Err(e) => {
                if restricted {
                    st.clear_history();
                }
                self.fail(cell, st, &e, deferred);
                return true;
            }
        }
        // the pass is applied in memory and not yet checkpointed (a test hook; it must
        // not call the runtime, whose locks the pass holds)
        store.failpoint("view-pass-applied");
        if d.complete && st.circuit.applied > from + 1 && from > 0 && st.views_count() > 0 {
            tracing::debug!(
                target: "sparkles::incr::runtime",
                "a view circuit applied commits {}..={}",
                from + 1,
                st.circuit.applied
            );
        }
        self.live_budgets(st, deferred);
        true
    }

    /// End the live queries of the circuit whose budget its state passed.
    pub(super) fn live_budgets(&self, st: &mut Slot, deferred: &mut DeferredDrops) {
        self.live_budgets_inner(st, false, deferred);
    }

    pub(super) fn live_budgets_inner(
        &self,
        st: &mut Slot,
        replace: bool,
        deferred: &mut DeferredDrops,
    ) {
        let bytes = st.circuit.bytes();
        let before = st.subs.len();
        st.subs.retain(|s| {
            if bytes <= s.budget {
                return true;
            }
            if let Some(sh) = s.shared.upgrade() {
                let err = LiveEvent::Error {
                    code: "over-budget".into(),
                    message: format!(
                        "the live query's state of {bytes} bytes passed its budget of {} bytes",
                        s.budget
                    ),
                };
                let displaced = if replace {
                    sh.replace_leased(err, None, None)
                } else {
                    sh.push_leased(err, None, None)
                };
                deferred.queued.extend(displaced);
            }
            false
        });
        if st.subs.len() != before {
            st.recompute_local_limit();
        }
    }

    /// Decode `c` incrementally under a `ScratchLease` for replay history admission.
    /// Returns `Some((rows, lease))` when the decoded diff fits within the circuit's
    /// budget, or `None` after dropping partial rows and releasing scratch.
    #[allow(clippy::type_complexity)]
    fn decode_history_charged(
        st: &Slot,
        c: &Changes,
    ) -> Option<(
        Arc<[(Vec<Option<Term>>, i64)]>,
        crate::incr::budget::ScratchLease,
    )> {
        let mut lease = st.circuit.scratch_lease();
        let mut decoded = Vec::new();
        if let Some(snap) = &st.circuit.snap {
            for (r, w) in c {
                crate::incr::circuit::reserve_vec_slot(&decoded, &mut lease).ok()?;
                let row = st
                    .circuit
                    .decode_row_budgeted(snap, None, r, &mut lease)
                    .ok()?;
                decoded.push((row, *w));
            }
        }
        Some((
            crate::incr::circuit::into_arc_rows(decoded, &mut lease).ok()?,
            lease,
        ))
    }

    /// Clone a live diff event's decoded rows into a replay-history entry under an
    /// independent `ScratchLease`, returning `None` when replay cannot coexist with the
    /// already-leased live event.
    #[allow(clippy::type_complexity)]
    fn history_from_diff_charged(
        st: &Slot,
        ev: &LiveEvent,
    ) -> Option<(
        Arc<[(Vec<Option<Term>>, i64)]>,
        crate::incr::budget::ScratchLease,
    )> {
        let LiveEvent::Diff { removed, added, .. } = ev else {
            return None;
        };
        let mut lease = st.circuit.scratch_lease();
        let mut decoded = Vec::new();
        for (rows, sign) in [(removed.as_ref(), -1i64), (added.as_ref(), 1i64)] {
            for r in rows {
                crate::incr::circuit::reserve_vec_slot(&decoded, &mut lease).ok()?;
                lease
                    .try_reserve(super::super::circuit::decoded_row_bytes(&r.terms))
                    .ok()?;
                let weight = if sign < 0 {
                    (r.count as i64).wrapping_neg()
                } else {
                    r.count as i64
                };
                decoded.push((r.terms.clone(), weight));
            }
        }
        Some((
            crate::incr::circuit::into_arc_rows(decoded, &mut lease).ok()?,
            lease,
        ))
    }

    /// Clear commit-keyed replay history and replace every live subscriber's queued
    /// events with a fresh `reset` reflecting the circuit's current evidence view.
    fn publish_evidence_reset(&self, st: &mut Slot, deferred: &mut DeferredDrops) {
        st.prune_dead_subs();
        st.clear_history();
        if st.subs.is_empty() {
            return;
        }
        let to = st.circuit.applied;
        let event_rows = st
            .circuit
            .ordered_row_count()
            .unwrap_or_else(|| st.circuit.result.len());
        if event_rows > crate::incr::live::QUEUE_ROWS {
            for s in &mut st.subs {
                if let Some(sh) = s.shared.upgrade() {
                    deferred.queued.extend(sh.mark_overflow());
                    s.last = to;
                    self.counters.live_events.fetch_add(1, Ordering::Relaxed);
                }
            }
            return;
        }
        let (event, mut event_lease) = match st.event_budgeted(true) {
            Ok(pair) => pair,
            Err(e) => {
                st.fail_live_subs(&e, deferred);
                return;
            }
        };
        let shared_lease = st.detach_event_lease(&mut event_lease);
        let stamp = Some(st.circuit.evidence_stamp());
        for s in &mut st.subs {
            if let Some(sh) = s.shared.upgrade() {
                s.last = to;
                deferred.queued.extend(sh.replace_leased(
                    event.clone(),
                    shared_lease.clone(),
                    stamp.clone(),
                ));
                self.counters.live_events.fetch_add(1, Ordering::Relaxed);
            }
        }
        deferred
            .queued
            .push(crate::incr::live::QueuedEvent::new(event, shared_lease));
    }

    /// Send the change `c` from commit `from` to the circuit's commit to the
    /// subscribers, keeping `c` covered by `c_lease` until consumed and dropped.
    pub(super) fn publish(
        &self,
        st: &mut Slot,
        from: u64,
        c: Changes,
        c_lease: crate::incr::budget::ScratchLease,
        reset: bool,
        deferred: &mut DeferredDrops,
    ) {
        st.prune_dead_subs();
        let to = st.circuit.applied;
        if st.subs.is_empty() {
            if st.views_count() == 0 || (reset && c.len() > self.opts.read().reset_rows) {
                drop(c);
                drop(c_lease);
                st.clear_history();
            } else if let Some((rows, mut lease)) = Self::decode_history_charged(st, &c) {
                drop(c);
                drop(c_lease);
                st.remember_charged(from, to, rows, VIEW_HISTORY_COMMITS, &mut lease);
                drop(lease);
            } else {
                drop(c);
                drop(c_lease);
                st.clear_history();
            }
            return;
        }
        let max = self.opts.read().history_commits;
        let reset_rows = self.opts.read().reset_rows;
        let (event, mut event_lease) = if st.circuit.kind == Kind::Ask {
            let delta: i64 = c.iter().map(|x| x.1).sum();
            drop(c);
            drop(c_lease);
            let now = st.circuit.result.total();
            st.remember(from, to, Arc::from(vec![(Vec::new(), delta)]), max);
            if (now - delta > 0) == (now > 0) {
                return;
            }
            (
                LiveEvent::Value {
                    commit: to,
                    from,
                    value: now > 0,
                },
                st.circuit.scratch_lease(),
            )
        } else if reset && (c.len() > reset_rows || st.circuit.ordered()) {
            // an ordered window's evaluation has no positions to diff: the subscribers
            // replace their copy
            if c.len() > reset_rows {
                drop(c);
                drop(c_lease);
                st.clear_history();
            } else if let Some((rows, mut lease)) = Self::decode_history_charged(st, &c) {
                drop(c);
                drop(c_lease);
                st.remember_charged(from, to, rows, max, &mut lease);
                drop(lease);
            } else {
                drop(c);
                drop(c_lease);
                st.clear_history();
            }
            let event_rows = st
                .circuit
                .ordered_row_count()
                .unwrap_or_else(|| st.circuit.result.len());
            if !st.subs.iter().any(|s| {
                s.shared
                    .upgrade()
                    .is_some_and(|sh| sh.can_accept_rows(event_rows))
            }) {
                for s in &mut st.subs {
                    if let Some(sh) = s.shared.upgrade() {
                        deferred.queued.extend(sh.mark_overflow());
                        s.last = to;
                        self.counters.live_events.fetch_add(1, Ordering::Relaxed);
                    }
                }
                return;
            }
            match st.event_budgeted(true) {
                Ok(pair) => pair,
                Err(e) => {
                    st.fail_live_subs(&e, deferred);
                    return;
                }
            }
        } else if st.circuit.ordered() {
            // the positions come from the window: a row that moves nets out of `c`
            if let Some((rows, mut lease)) = Self::decode_history_charged(st, &c) {
                drop(c);
                drop(c_lease);
                st.remember_charged(from, to, rows, max, &mut lease);
                drop(lease);
            } else {
                drop(c);
                drop(c_lease);
                st.clear_history();
            }
            let Some(event_rows) = st.circuit.moves_row_count() else {
                return;
            };
            if event_rows == 0 {
                return;
            }
            if !st.subs.iter().any(|s| {
                s.shared
                    .upgrade()
                    .is_some_and(|sh| sh.can_accept_rows(event_rows))
            }) {
                for s in &mut st.subs {
                    if let Some(sh) = s.shared.upgrade() {
                        deferred.queued.extend(sh.mark_overflow());
                        s.last = to;
                        self.counters.live_events.fetch_add(1, Ordering::Relaxed);
                    }
                }
                return;
            }
            match st.moves_event_budgeted(from, to) {
                Ok(Some(pair)) => pair,
                Ok(None) => return,
                Err(e) => {
                    st.fail_live_subs(&e, deferred);
                    return;
                }
            }
        } else {
            if c.is_empty() {
                drop(c);
                drop(c_lease);
                st.remember(from, to, Arc::from(Vec::new()), max);
                return;
            }
            let event_rows = c.len();
            if !st.subs.iter().any(|s| {
                s.shared
                    .upgrade()
                    .is_some_and(|sh| sh.can_accept_rows(event_rows))
            }) {
                if let Some((rows, mut lease)) = Self::decode_history_charged(st, &c) {
                    drop(c);
                    drop(c_lease);
                    st.remember_charged(from, to, rows, max, &mut lease);
                    drop(lease);
                } else {
                    drop(c);
                    drop(c_lease);
                    st.clear_history();
                }
                for s in &mut st.subs {
                    if let Some(sh) = s.shared.upgrade() {
                        deferred.queued.extend(sh.mark_overflow());
                        s.last = to;
                        self.counters.live_events.fetch_add(1, Ordering::Relaxed);
                    }
                }
                return;
            }
            // Build and bound the live event independently of replay history so replay
            // refusal never triggers an unbounded decode, and replay cannot steal the
            // live event's lease.
            let built = st.diff_event_budgeted(from, to, &c);
            drop(c);
            drop(c_lease);
            match built {
                Ok(Some((ev, ev_lease))) => {
                    // Best-effort replay admission while `ev_lease` keeps `ev` covered.
                    if let Some((rows, mut hist_lease)) = Self::history_from_diff_charged(st, &ev) {
                        st.remember_charged(from, to, rows, max, &mut hist_lease);
                        drop(hist_lease);
                    } else {
                        st.clear_history();
                    }
                    (ev, ev_lease)
                }
                Ok(None) => {
                    st.remember(from, to, Arc::from(Vec::new()), max);
                    return;
                }
                Err(e) => {
                    st.clear_history();
                    st.fail_live_subs(&e, deferred);
                    return;
                }
            }
        };
        let shared_lease = st.detach_event_lease(&mut event_lease);
        let stamp = Some(st.circuit.evidence_stamp());
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
                deferred
                    .queued
                    .extend(sh.push_leased(e, shared_lease.clone(), stamp.clone()));
                self.counters.live_events.fetch_add(1, Ordering::Relaxed);
            }
        }
        // Keep `event` and `shared_lease` alive until outside `cell.state.lock()` so a
        // lease with zero remaining subscribers never releases credit under the mutex.
        deferred
            .queued
            .push(crate::incr::live::QueuedEvent::new(event, shared_lease));
    }

    /// Suspend the views whose circuit passed their budget or the pool's.
    pub(super) fn budgets(&self, cells: &[(Arc<Cell>, u64)]) {
        let mut detached: Vec<Arc<Cell>> = Vec::new();
        {
            let mut reg = self.reg.lock();
            let mut seen = rustc_hash::FxHashSet::default();
            let mut state: u64 = reg
                .views
                .values()
                .filter_map(|e| e.cell.as_ref())
                .filter(|cell| seen.insert(cell.key))
                .map(|cell| {
                    let info = cell.info.lock();
                    info.bytes.saturating_sub(info.published_bytes)
                })
                .sum();
            let pool = self.opts.read().views_memory;
            for (cell, bytes) in cells {
                let total = state.saturating_add(self.published_views.load(Ordering::Relaxed));
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
                        if c.release_view(&n) == 0 {
                            let info = c.info.lock();
                            state = state
                                .saturating_sub(info.bytes.saturating_sub(info.published_bytes));
                        }
                        detached.push(c);
                    }
                }
            }
        }
        drop(detached);
    }

    /// Drop the circuits nobody uses.
    pub(super) fn gc(&self) {
        let mut deferred = DeferredDrops::default();
        {
            let mut reg = self.reg.lock();
            let mode_gen = reg.mode_gen;
            reg.cells.retain(|_, c| {
                if c.pending.load(Ordering::Acquire) > 0 {
                    return true;
                }
                match c.state.try_lock() {
                    Some(mut st) => {
                        let keep = st.alive();
                        if !keep && st.mode_gen < mode_gen {
                            let boundary =
                                self.seen.load(Ordering::Acquire).max(st.circuit.applied);
                            self.switched.fetch_max(boundary, Ordering::AcqRel);
                        }
                        deferred.retired.extend(st.circuit.drain_retired());
                        if !keep {
                            deferred.cells.push(c.clone());
                        }
                        keep
                    }
                    None => true,
                }
            });
            if reg.cells.is_empty() && reg.internal.is_empty() {
                *self.subscription.lock() = None;
            }
        }
        drop(deferred);
    }

    /// Checkpoint every view whose state moved since its last checkpoint.
    pub fn checkpoint(&self, store: &Store) {
        let Some(root) = self.root.clone() else {
            return;
        };
        *self.last_checkpoint.lock() = Instant::now();
        let internal: Vec<Arc<InternalCell>> = self.reg.lock().internal.values().cloned().collect();
        self.checkpoint_internal(store, &internal);
        let todo: Vec<(String, Arc<Cell>, u64, Option<String>)> = self
            .reg
            .lock()
            .views
            .iter()
            .filter_map(|(n, e)| {
                Some((
                    n.clone(),
                    e.cell.clone()?,
                    e.checkpointed,
                    e.checkpointed_ev_key.clone(),
                ))
            })
            .collect();
        let live_ev = store.snapshot().evidence_visibility_key();
        for (name, cell, done, done_ev) in todo {
            // under the state lock, which a switch of the inference takes to replace the
            // circuit
            let st = cell.state.lock();
            if cell.info.lock().status != Status::Ready {
                continue;
            }
            let applied = st.circuit.applied;
            let restricted = st
                .circuit
                .access
                .as_ref()
                .is_some_and(|a| !a.reads_everything());
            let circuit_ev = st.circuit.evidence_key().map(str::to_string);
            if restricted && (circuit_ev.as_deref() == Some("unavailable") || circuit_ev != live_ev)
            {
                continue;
            }
            if applied <= done && (!restricted || circuit_ev == done_ev) {
                continue;
            }
            let Some(snap) = st.circuit.snap.clone() else {
                continue;
            };
            let kind = persist::view_kind(st.circuit.has_fixpoint());
            let r = st.circuit.encode(&snap).and_then(|body| {
                persist::write_as(
                    kind,
                    &views::dir(&root, &name).join(BRANCH),
                    &persist::Header {
                        dataset: self.dataset,
                        commit: applied,
                        fingerprint: st.fingerprint(cell.key, circuit_ev.as_deref()),
                    },
                    &body,
                    || store.failpoint("view-checkpoint-written"),
                )
            });
            drop(st);
            match r {
                Ok(()) => {
                    self.counters.checkpoints.fetch_add(1, Ordering::Relaxed);
                    if let Some(e) = self.reg.lock().views.get_mut(&name) {
                        e.checkpointed = applied;
                        e.checkpointed_ev_key = circuit_ev;
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
}
