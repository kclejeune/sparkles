//! A round of passes after commits (spec F13 §3.3), with budgets, collection and
//! checkpoints.

use super::*;

pub(super) fn failure(e: &Error) -> (String, String) {
    let code = match e {
        Error::Timeout | Error::Cancelled => "pass-timeout",
        Error::BudgetExceeded(_) => "over-budget",
        _ => "view-failed",
    };
    (code.to_string(), e.to_string())
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
        let internal: Vec<Arc<InternalCell>> = self.reg.lock().internal.values().cloned().collect();
        for cell in &internal {
            if cell.handles.load(Ordering::Acquire) > 0 && self.round_internal(store, cell) {
                worked = true;
            }
        }
        drop(internal);
        self.collect_internal(store);
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

    pub(super) fn control(&self) -> Control {
        Control {
            deadline: Some(Instant::now() + self.opts.read().pass_timeout),
            ..Control::none()
        }
    }

    pub(super) fn note_pass(&self, cell: &Cell, st: &Slot, took: Duration, skipped: bool) {
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

    pub(super) fn fail(&self, cell: &Cell, st: &mut Slot, e: &Error) {
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
        st.clear_history();
        cell.info.lock().status = Status::Failed { code, message };
    }

    pub(super) fn build(&self, store: &Store, cell: &Cell, st: &mut Slot) {
        let snap = store.snapshot();
        let before = st.circuit.applied;
        let t0 = Instant::now();
        // a rebuild's change reaches live queries
        match st.evaluate(snap, &self.control()) {
            Ok(change) => {
                cell.info.lock().status = Status::Ready;
                self.note_pass(cell, st, t0.elapsed(), false);
                self.live_budgets(st);
                if let Some(c) = change {
                    self.publish(st, before, c, true);
                }
                if std::mem::take(&mut st.rebased) {
                    st.clear_history();
                }
            }
            Err(e) => self.fail(cell, st, &e),
        }
    }

    pub(super) fn advance(
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
        // the pass is applied in memory and not yet checkpointed (a test hook; it must
        // not call the runtime, whose locks the pass holds)
        store.failpoint("view-pass-applied");
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
    pub(super) fn live_budgets(&self, st: &mut Slot) {
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
    pub(super) fn publish(&self, st: &mut Slot, from: u64, c: Changes, reset: bool) {
        st.subs.retain(|s| s.shared.strong_count() > 0);
        let to = st.circuit.applied;
        if st.subs.is_empty() {
            if st.views == 0 || (reset && c.len() > self.opts.read().reset_rows) {
                st.clear_history();
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
            st.clear_history();
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
    pub(super) fn budgets(&self, cells: &[(Arc<Cell>, u64)]) {
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
                if let Some(c) = e.cell.take()
                    && c.release_view() == 0
                {
                    total = total.saturating_sub(*bytes);
                }
            }
        }
    }

    /// Drop the circuits nobody uses.
    pub(super) fn gc(&self) {
        let mut reg = self.reg.lock();
        reg.cells.retain(|_, c| {
            c.pending.load(Ordering::Acquire) > 0
                || match c.state.try_lock() {
                    Some(mut st) => st.alive(),
                    None => true,
                }
        });
        if reg.cells.is_empty() && reg.internal.is_empty() {
            *self.subscription.lock() = None;
        }
    }

    /// Checkpoint every view whose state moved since its last checkpoint.
    pub fn checkpoint(&self, store: &Store) {
        let Some(root) = self.root.clone() else {
            return;
        };
        *self.last_checkpoint.lock() = Instant::now();
        let internal: Vec<Arc<InternalCell>> = self.reg.lock().internal.values().cloned().collect();
        self.checkpoint_internal(store, &internal);
        let todo: Vec<(String, Arc<Cell>, u64)> = self
            .reg
            .lock()
            .views
            .iter()
            .filter_map(|(n, e)| Some((n.clone(), e.cell.clone()?, e.checkpointed)))
            .collect();
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
            let kind = persist::view_kind(st.circuit.has_fixpoint());
            let r = st.circuit.encode(&snap).and_then(|body| {
                persist::write_as(
                    kind,
                    &views::dir(&root, &name).join(BRANCH),
                    &persist::Header {
                        dataset: self.dataset,
                        commit: applied,
                        fingerprint: st.fingerprint(cell.key),
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
