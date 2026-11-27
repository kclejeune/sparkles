//! Live queries (spec F13 §2.3).

use super::*;

impl Runtime {
    /// Subscribe to `query` read with `access` (spec F13 §2.3). The first event is
    /// the result, or with `o.after` the change since that commit.
    pub fn live(
        self: &Arc<Self>,
        store: &Store,
        query: &str,
        access: Option<Arc<GraphAccess>>,
        o: &LiveOptions,
    ) -> Result<LiveQuery> {
        let _operation = Operation::enter();
        let host = self.host_or_open();
        let q = self.parse(host.as_ref(), query)?;
        if matches!(q, Query::Describe { .. }) {
            return Err(crate::incr::compile::not_incremental("DESCRIBE"));
        }
        let count = self.admit(o)?;
        let budget = o
            .max_memory_bytes
            .unwrap_or(self.opts.read().live_memory_bytes);
        let restricted = access.as_ref().is_some_and(|a| !a.reads_everything());
        let shared = Arc::new(Shared::new(
            budget,
            restricted,
            Some(store.family_managed()),
        ));
        self.refresh_mode();
        let (cell, mut target, mut has_lagging) = {
            let mut reg = self.reg.lock();
            if self.is_closed() {
                return Err(crate::incr::error(
                    "views-unavailable",
                    "the dataset is closing",
                ));
            }
            let (target, lagging) = self.lagging_cells(&mut reg);
            let res = self.cell_for(&mut reg, q, query, access, Some(budget));
            if let Ok((cell, _)) = &res {
                cell.pending.fetch_add(1, Ordering::AcqRel);
                if reg.live.len() == reg.live.capacity() {
                    reg.live.retain(|entry| entry.queue.strong_count() > 0);
                }
                reg.live.push(LiveRegistration {
                    cell: Arc::downgrade(cell),
                    queue: Arc::downgrade(&shared),
                });
            }
            (res, target, !lagging.is_empty())
        };
        if has_lagging {
            self.kick();
        }
        let (cell, _) = cell?;
        let joining = Joining(&cell);
        self.ensure_subscription(store);
        // Switch any lagging cell and drop its replaced operators and retired budget
        // leases outside `cell.state.lock()` before evaluating, so tight shared pools
        // reclaim the old credit before the rebuild runs.
        let subscribe_res: Result<()> = loop {
            let mut deferred_state = DeferredDrops::default();
            let mut retry_mode = false;
            let step_res: Result<()> = {
                struct LimitGuard<'a>(parking_lot::MutexGuard<'a, Slot>);
                impl Drop for LimitGuard<'_> {
                    fn drop(&mut self) {
                        self.0.recompute_local_limit();
                    }
                }
                let mut state = cell.state.lock();
                if let Some(completion) = state.pending_retirement().cloned() {
                    drop(state);
                    // Returning pool credit can invoke a wake callback synchronously.
                    // A reentrant join cannot wait for its own thread to finish the release.
                    if completion.owner == std::thread::current().id() {
                        break Err(crate::incr::error(
                            "view-behind",
                            "the circuit is still releasing its previous state; retry",
                        ));
                    }
                    #[cfg(test)]
                    if let Some(hook) = self.retirement_wait_hook.lock().clone() {
                        hook();
                    }
                    completion.released.wait();
                    continue;
                }
                let mut guard = LimitGuard(state);
                let st = &mut *guard.0;
                // Provisionally allow the joining subscriber's budget while building or
                // constructing its initial event; `LimitGuard` restores the exact
                // active-consumer ceiling on exit or unwind.
                let result = (|| {
                    if self.is_closed() {
                        return Err(crate::incr::error(
                            "views-unavailable",
                            "the dataset is closing",
                        ));
                    }
                    if !self.mode_is_current(target.generation, target.host_seq) {
                        retry_mode = true;
                        return Ok(());
                    }
                    if st.mode_gen < target.generation {
                        if self.switch_cell(&cell, st, &target, &mut deferred_state) {
                            has_lagging = true;
                        }
                        retry_mode = true;
                        return Ok(());
                    }
                    let next_limit = st
                        .circuit
                        .local_limit()
                        .map_or(budget, |cur| cur.max(budget));
                    st.circuit.set_local_limit(Some(next_limit));
                    let status = cell.info.lock().status.clone();
                    let ev_mismatch = st.evidence_mismatch(&store.snapshot());
                    if (status != Status::Ready || ev_mismatch)
                        && let Err(e) = self.evaluate_for_live(
                            store,
                            &cell,
                            st,
                            ev_mismatch,
                            &mut deferred_state,
                        )
                    {
                        return Err(e);
                    }
                    if st.circuit.bytes() > budget {
                        let bytes = st.circuit.bytes();
                        return Err(crate::incr::error(
                            "over-budget",
                            format!(
                                "the live query's state of {bytes} bytes passes its budget of {budget} bytes"
                            ),
                        ));
                    }
                    let first = match o.after {
                        None => Some(st.event_budgeted(false)?),
                        Some(after) => {
                            self.resume(store, st, after, &mut deferred_state.circuits)?
                        }
                    };
                    // queued before a pass can see the subscriber, so that no later diff
                    // overtakes it
                    if let Some((e, mut lease)) = first {
                        let shared_lease = st.detach_event_lease(&mut lease);
                        deferred_state.queued.extend(shared.push_leased(
                            e,
                            shared_lease,
                            Some(st.circuit.evidence_stamp()),
                        ));
                    }
                    let last = st.circuit.applied;
                    st.subs.push(Sub {
                        shared: Arc::downgrade(&shared),
                        budget,
                        last,
                    });
                    st.recompute_local_limit();
                    Ok(())
                })();
                deferred_state.retired.extend(st.circuit.drain_retired());
                result
            };
            drop(deferred_state);
            if retry_mode {
                if !self.mode_is_current(target.generation, target.host_seq) {
                    self.refresh_mode();
                    target = self.reg.lock().target_mode();
                    has_lagging = true;
                }
                continue;
            }
            break step_res;
        };
        if has_lagging {
            self.mark_mode_switched(target.generation);
        }
        if let Err(e) = subscribe_res {
            drop(joining);
            self.gc();
            return Err(e);
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

    /// Count a new live query against the limits (`too-many-live-queries`).
    fn admit(&self, o: &LiveOptions) -> Result<Count> {
        if self.live_total.fetch_add(1, Ordering::AcqRel) >= self.opts.read().max_live {
            self.live_total.fetch_sub(1, Ordering::AcqRel);
            return Err(crate::incr::error(
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
                return Err(crate::incr::error(
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
        Ok(count)
    }

    /// Evaluate the circuit a live query joins when it is not ready or its evidence view
    /// is superseded, and send its other subscribers the new result as a `reset`.
    fn evaluate_for_live(
        &self,
        store: &Store,
        cell: &Cell,
        st: &mut Slot,
        ev_mismatch: bool,
        deferred: &mut DeferredDrops,
    ) -> Result<()> {
        let snap = store.snapshot();
        let had_snap = st.circuit.snap.is_some();
        let restricted = st
            .circuit
            .access
            .as_ref()
            .is_some_and(|a| !a.reads_everything());
        let prev_stamp = st.circuit.evidence_stamp();
        let ev_mismatch = ev_mismatch || (had_snap && st.evidence_mismatch(&snap));
        let t0 = Instant::now();
        if let Err(e) = st.evaluate(snap, &self.control()) {
            if ev_mismatch {
                st.clear_history();
                self.fail(cell, st, &e, deferred);
            } else if !had_snap {
                st.circuit.clear_state();
            }
            return Err(e);
        }
        self.note_pass(cell, st, t0.elapsed(), round::PassKind::Build);
        let ev_boundary_crossed =
            restricted && had_snap && (ev_mismatch || st.circuit.evidence_stamp() != prev_stamp);
        if std::mem::take(&mut st.rebased) || ev_boundary_crossed {
            st.clear_history();
        }
        self.live_budgets_inner(st, ev_boundary_crossed, deferred);
        st.prune_dead_subs();
        if !st.subs.is_empty() {
            // other subscribers see the new result as a reset
            let applied = st.circuit.applied;
            let stamp = Some(st.circuit.evidence_stamp());
            match st.event_budgeted(true) {
                Ok((reset, mut lease)) => {
                    let shared_lease = st.detach_event_lease(&mut lease);
                    for s in &mut st.subs {
                        if let Some(sh) = s.shared.upgrade() {
                            s.last = applied;
                            let displaced = if ev_boundary_crossed {
                                sh.replace_leased(
                                    reset.clone(),
                                    shared_lease.clone(),
                                    stamp.clone(),
                                )
                            } else {
                                sh.push_leased(reset.clone(), shared_lease.clone(), stamp.clone())
                            };
                            deferred.queued.extend(displaced);
                        }
                    }
                    deferred
                        .queued
                        .push(crate::incr::live::QueuedEvent::new(reset, shared_lease));
                }
                Err(e) => {
                    st.fail_live_subs(&e, deferred);
                }
            }
        }
        Ok(())
    }

    /// The first event of a live query resumed after commit `after`: the change from
    /// it from the circuit's history, or by evaluating the query before the changes
    /// since, or a `reset` (spec F13 §2.3). Any temporary circuit built is pushed into
    /// `temp_circuits` so the caller drops it outside `cell.state.lock()`.
    pub(super) fn resume(
        &self,
        store: &Store,
        st: &Slot,
        after: u64,
        temp_circuits: &mut Vec<Circuit>,
    ) -> Result<Option<(LiveEvent, crate::incr::budget::ScratchLease)>> {
        let applied = st.circuit.applied;
        let switched = self.switched.load(Ordering::Acquire);
        if switched > 0 && after <= switched {
            return st.event_budgeted(true).map(Some);
        }
        // an ordered window keeps no positions for past commits, and a circuit built
        // again since the subscriber's event may order ties otherwise, even at its commit
        if st.circuit.ordered() {
            return st.event_budgeted(true).map(Some);
        }
        // A commit number cannot establish which side of a same-commit evidence
        // configuration change the subscriber saw; return a fresh reset instead of
        // skipping, replaying commit-keyed history, or diffing an earlier snapshot.
        if st.circuit.access.as_ref().is_some_and(|a| {
            !a.reads_everything()
                && (st.circuit.evidence_key().is_some()
                    || store.has_evidence_visibility_for_access(a))
        }) {
            return st.event_budgeted(true).map(Some);
        }
        if after == applied {
            return Ok(None);
        }
        if after > applied {
            return st.event_budgeted(true).map(Some);
        }
        let ask = st.circuit.kind == Kind::Ask;
        let change = st.history_since_budgeted(after).or_else(|| {
            let d = DeltaSet::read(store, after, &Control::none());
            if !d.complete || d.snapshot.commit != applied {
                return None;
            }
            let old = d.old().ok()?;
            let mut c = st.fresh(st.circuit.inference.clone()).ok()?;
            c.set_runtime_pool(self.pool.clone(), self.circuit_parent());
            // Keep the temporary circuit outside the cell lock's drop scope.
            temp_circuits.push(c);
            let c = temp_circuits.last_mut().expect("pushed above");
            c.build(old, &self.control()).ok()?;
            let mut lease = st.circuit.scratch_lease();
            if ask {
                let delta = st.circuit.result.total() - c.result.total();
                return Some((vec![(Vec::new(), delta)], lease));
            }
            let now = st.circuit.rows_charged().ok()?;
            let before = c.rows_charged().ok()?;
            let terms = now
                .0
                .iter()
                .chain(&before.0)
                .map(|(row, _)| crate::incr::circuit::decoded_row_bytes(row))
                .sum::<u64>();
            lease
                .try_reserve(terms + (now.0.len() + before.0.len()) as u64 * 96)
                .ok()?;
            let change = net(now
                .0
                .into_iter()
                .chain(before.0.into_iter().map(|(row, w)| (row, -w))));
            drop(before.1);
            drop(now.1);
            Some((change, lease))
        });
        let Some((change, mut lease)) = change else {
            return st.event_budgeted(true).map(Some);
        };
        if ask {
            let delta: i64 = change.iter().map(|x| x.1).sum();
            let now = st.circuit.result.total();
            if (now - delta > 0) == (now > 0) {
                return Ok(None);
            }
            return Ok(Some((
                LiveEvent::Value {
                    commit: applied,
                    from: after,
                    value: now > 0,
                },
                st.circuit.scratch_lease(),
            )));
        }
        if change.len() > crate::incr::live::QUEUE_ROWS {
            drop(change);
            drop(lease);
            return st.event_budgeted(true).map(Some);
        }
        let row_slot = std::mem::size_of::<LiveRow>() as u64;
        let rows = change.len() as u64;
        let terms = change
            .iter()
            .map(|(row, _)| crate::incr::circuit::decoded_row_bytes(row))
            .sum::<u64>();
        // diff_event temporarily owns both its vectors and the final Arc slices.
        if lease
            .try_set(terms + change.capacity() as u64 * 32 + (3 * rows + 8) * row_slot)
            .is_err()
        {
            drop(change);
            drop(lease);
            return st.event_budgeted(true).map(Some);
        }
        let event = diff_event(after, applied, change.into_iter());
        lease.try_set(terms + rows * row_slot)?;
        Ok(Some((event, lease)))
    }
}

#[cfg(test)]
mod resume_unwind_tests {
    use super::*;
    use crate::store::StoreOptions;

    /// Proves that a charged fallback circuit pushed into caller-owned `DeferredDrops`
    /// before building/unwinding releases its `QueryPool` credit strictly AFTER
    /// `cell.state.lock()` unwinds, so a synchronous waker that re-acquires
    /// `cell.state.lock()` never deadlocks.
    #[test]
    fn resume_unwind_drops_charged_circuit_after_cell_state_lock_releases() {
        let store = Store::in_memory(StoreOptions::default());
        crate::sparql::update::update(
            &store,
            "INSERT DATA { <urn:a> <urn:p> \"1\" . <urn:b> <urn:p> \"2\" }",
            &crate::sparql::QueryOptions::default(),
        )
        .unwrap();
        let pool = QueryPool::new(16 << 20);
        let rt = Runtime::open_with_pool(
            None,
            store.dataset_id(),
            RuntimeOptions::default(),
            Some(pool.clone()),
        );
        rt.set_host(Arc::new(OpenHost));
        let _live = rt
            .live(
                &store,
                "SELECT ?s ?o WHERE { ?s <urn:p> ?o }",
                None,
                &LiveOptions::default(),
            )
            .unwrap();
        let cell = rt.reg.lock().cells.values().next().unwrap().clone();
        let baseline = pool.granted();
        assert!(baseline > 0);

        // Fill pool so free room is exact 4 MiB before building the fallback circuit;
        // once `c.build` charges `c`, entering a 4 MiB waiter queues it, and dropping `c`
        // during unwind frees exact 4 MiB and wakes the waiter synchronously.
        let filler = pool
            .work_parent()
            .reserve(pool.capacity() - (4 << 20) - baseline)
            .unwrap();
        let woke = Arc::new(AtomicUsize::new(0));
        let woke_cb = woke.clone();
        let cell_cb = cell.clone();
        let waiter_slot = Mutex::new(None);

        let snap = store.snapshot();
        let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let mut deferred_state = DeferredDrops::default();
            let st = cell.state.lock();
            let mut c = st.fresh(st.circuit.inference.clone()).unwrap();
            c.set_runtime_pool(Some(pool.clone()), Some(pool.work_parent()));
            // Same ordering as `Runtime::resume`: push into `deferred_state` BEFORE build.
            deferred_state.circuits.push(c);
            let c = deferred_state.circuits.last_mut().unwrap();
            c.build(snap, &Control::none()).unwrap();
            *waiter_slot.lock() = Some(pool.enter(
                4 << 20,
                Some(Arc::new(move || {
                    // Would deadlock immediately if the fallback circuit dropped while
                    // `cell.state.lock()` was still held during unwind.
                    let _st = cell_cb.state.lock();
                    woke_cb.fetch_add(1, Ordering::SeqCst);
                })),
            ));
            panic!("simulated panic after charging fallback circuit inside cell.state.lock()");
        }));
        assert!(res.is_err());
        assert!(
            woke.load(Ordering::SeqCst) >= 1,
            "waker must have acquired cell.state.lock() cleanly after unwind"
        );
        drop(waiter_slot);
        drop(filler);
        drop(_live);
        rt.close(&store);
        assert_eq!(pool.granted(), 0);
    }

    /// Exercises the actual `Runtime::live(..., after: Some(_))` -> `Runtime::resume`
    /// fallback circuit path with an active `QueryPool` waker that re-acquires
    /// `cell.state.lock()`, proving the temporary resume circuit drops strictly outside
    /// `cell.state.lock()`.
    #[test]
    fn actual_runtime_resume_drops_fallback_circuit_outside_cell_state_lock() {
        let store = Store::in_memory(StoreOptions::default());
        let mut init = String::from("INSERT DATA { ");
        for i in 0..200 {
            init.push_str(&format!("<urn:r:{i}> <urn:p> \"{}\". ", "v".repeat(128)));
        }
        init.push('}');
        crate::sparql::update::update(&store, &init, &crate::sparql::QueryOptions::default())
            .unwrap();
        let c1 = store.snapshot().commit;

        let pool = QueryPool::new(16 << 20);
        let rt = Runtime::open_with_pool(
            None,
            store.dataset_id(),
            RuntimeOptions {
                history_commits: 1,
                ..RuntimeOptions::default()
            },
            Some(pool.clone()),
        );
        rt.set_host(Arc::new(OpenHost));
        let q = "SELECT ?s ?o WHERE { ?s <urn:p> ?o }";
        let live1 = rt.live(&store, q, None, &LiveOptions::default()).unwrap();

        // Advance past `c1` and clear history so resuming after `c1` takes the fallback
        // circuit path inside `Runtime::resume`.
        crate::sparql::update::update(
            &store,
            "INSERT DATA { <urn:r:extra> <urn:p> \"extra\" }",
            &crate::sparql::QueryOptions::default(),
        )
        .unwrap();
        rt.round(&store);
        let cell = rt.reg.lock().cells.values().next().unwrap().clone();
        {
            let mut st = cell.state.lock();
            st.clear_history();
            let retired = st.circuit.drain_retired();
            drop(st);
            drop(retired);
        }

        let baseline = pool.granted();
        assert!(baseline > 0);
        // Leave room for the fallback circuit. The resumed event stays leased
        // while queued, so leave a margin between free room and the waiting grant.
        let filler = pool
            .work_parent()
            .reserve(pool.capacity() - (5 << 20) - baseline)
            .unwrap();
        let woke = Arc::new(AtomicUsize::new(0));
        let woke_cb = woke.clone();
        let cell_cb = cell.clone();
        let waiter = pool.enter(
            4 << 20,
            Some(Arc::new(move || {
                // Would deadlock if `Runtime::resume` dropped the fallback circuit under
                // `cell.state.lock()`.
                let _st = cell_cb.state.lock();
                woke_cb.fetch_add(1, Ordering::SeqCst);
            })),
        );

        let live2 = rt
            .live(
                &store,
                q,
                None,
                &LiveOptions {
                    after: Some(c1),
                    ..LiveOptions::default()
                },
            )
            .unwrap();
        // The resumed subscriber received the diff for `<urn:r:extra>`, and the fallback
        // circuit dropped cleanly outside `cell.state.lock()` waking our waiter.
        assert!(
            woke.load(Ordering::SeqCst) >= 1,
            "waker must acquire cell.state.lock() cleanly when Runtime::resume drops fallback circuit"
        );
        assert!(matches!(live2.try_next(), Some(LiveEvent::Diff { .. })));
        drop(waiter);
        drop(filler);
        drop(live2);
        drop(live1);
        rt.close(&store);
        assert_eq!(pool.granted(), 0);
    }
}
