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
        let cell = {
            let mut reg = self.reg.lock();
            if self.is_closed() {
                return Err(crate::incr::error(
                    "views-unavailable",
                    "the dataset is closing",
                ));
            }
            self.sync_inference(&mut reg, host.as_ref());
            let (cell, _) = self.cell_for(&mut reg, q, query, access, Some(budget))?;
            cell.pending.fetch_add(1, Ordering::AcqRel);
            cell
        };
        let joining = Joining(&cell);
        self.ensure_subscription(store);
        let shared = Arc::new(Shared::default());
        {
            let mut st = cell.state.lock();
            let status = cell.info.lock().status.clone();
            if status != Status::Ready
                && let Err(e) = self.evaluate_for_live(store, &cell, &mut st)
            {
                drop(st);
                drop(joining);
                self.gc();
                return Err(e);
            }
            if st.circuit.bytes() > budget {
                let bytes = st.circuit.bytes();
                drop(st);
                drop(joining);
                self.gc();
                return Err(crate::incr::error(
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

    /// Evaluate the circuit a live query joins when it is not ready, and send its
    /// other subscribers the new result as a `reset`.
    fn evaluate_for_live(&self, store: &Store, cell: &Cell, st: &mut Slot) -> Result<()> {
        let snap = store.snapshot();
        let t0 = Instant::now();
        st.evaluate(snap, &self.control())?;
        cell.info.lock().status = Status::Ready;
        self.note_pass(cell, st, t0.elapsed(), false);
        if std::mem::take(&mut st.rebased) {
            st.clear_history();
        }
        self.live_budgets(st);
        // other subscribers see the new result as a reset
        let applied = st.circuit.applied;
        let reset = st.event(true);
        for s in &mut st.subs {
            if let Some(sh) = s.shared.upgrade() {
                s.last = applied;
                sh.push(reset.clone());
            }
        }
        Ok(())
    }

    /// The first event of a live query resumed after commit `after`: the change from
    /// it from the circuit's history, or by evaluating the query before the changes
    /// since, or a `reset` (spec F13 §2.3).
    pub(super) fn resume(&self, store: &Store, st: &Slot, after: u64) -> Option<LiveEvent> {
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
}
