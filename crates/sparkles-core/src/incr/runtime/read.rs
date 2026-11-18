//! Reading a view's rows (spec F13 §2.2).

use super::*;

impl Runtime {
    /// The rows of view `name` (spec F13 §2.2).
    pub fn read(&self, r: &ViewRead<'_>) -> Result<ViewRows> {
        let _operation = Operation::enter();
        let wait = Instant::now() + self.opts.read().view_wait;
        let until = r.deadline.map_or(wait, |d| d.min(wait));
        loop {
            let (cell, access, suspended, error, errors) = {
                let mut reg = self.reg.lock();
                self.sync_for_read(&mut reg);
                let errors = reg.aggregate_errors;
                let e = reg.view(r.name)?;
                (
                    e.cell.clone(),
                    e.access.clone(),
                    e.suspended.clone(),
                    e.error.clone(),
                    errors,
                )
            };
            if let Some(why) = suspended {
                return Err(crate::incr::error(
                    "view-suspended",
                    format!("the view {:?} is suspended ({why}); rebuild it", r.name),
                ));
            }
            if let Some(err) = error {
                return Err(crate::incr::error(
                    "view-failed",
                    format!("the view {:?} failed: {err}", r.name),
                ));
            }
            if let Some(cell) = cell {
                if !covers(r.caller, access.as_ref()) {
                    return Err(crate::incr::error(
                        "view-access",
                        format!("the view {:?} reads data your grants do not cover", r.name),
                    ));
                }
                // another reading of errors than the view's: the view's query at the
                // reader's snapshot, without the view's state
                if let Some(m) = r.aggregate_errors.filter(|m| *m != errors) {
                    let st = cell.state.lock();
                    return self.evaluate_under(st, r, m);
                }
                let status = cell.info.lock().status.clone();
                match status {
                    Status::Failed { code, message } => {
                        let code = if code == "view-failed" {
                            "view-failed"
                        } else {
                            "view-suspended"
                        };
                        return Err(crate::incr::error(
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
                                evaluated_under: None,
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
                return Err(crate::incr::error(
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
    pub(super) fn read_back(
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
                evaluated_under: None,
            });
        }
        let ahead = || {
            crate::incr::error(
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
            evaluated_under: None,
        })
    }
}

impl Runtime {
    /// The rows of the view's query evaluated at the reader's snapshot under the
    /// reader's reading of errors in aggregates `m`, which differs from the view's. The
    /// view's state is neither read nor changed.
    fn evaluate_under(
        &self,
        st: parking_lot::MutexGuard<'_, Slot>,
        r: &ViewRead<'_>,
        m: crate::sparql::AggregateErrors,
    ) -> Result<ViewRows> {
        let Some(snap) = r.snap else {
            return Err(crate::incr::error(
                "view-reading",
                format!(
                    "the view {:?} keeps another reading of aggregate errors than {m}, and there is no snapshot to evaluate its query at",
                    r.name
                ),
            ));
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
        c.aggregate_errors = Some(m);
        let control = Control {
            deadline: r.deadline.or(self.control().deadline),
            ..Control::none()
        };
        c.build(snap.clone(), &control)?;
        Ok(ViewRows {
            columns: c.columns(),
            rows: c.rows(),
            commit: snap.commit,
            evaluated_under: Some(m),
        })
    }
}

impl ViewSource for Runtime {
    fn read_view(&self, r: &ViewRead<'_>) -> Result<ViewRows> {
        self.read(r)
    }
}
