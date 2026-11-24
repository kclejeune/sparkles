//! Reading a view's rows (spec F13 §2.2), eagerly or as an immutable shared version
//! for streaming cursors (item 1121).

use super::*;
use sparkles_budget::sizes::{table_bytes, table_grown};

/// Step duration while waiting for a view to catch up or build so cooperative
/// cancellation checks run promptly without busy-spinning.
const WAIT_STEP: Duration = Duration::from_millis(20);

/// Build the view's snapshot with the view's own `union_default_graph` setting.
fn view_snapshot(
    circuit: &Circuit,
    snap: &Arc<crate::store::Snapshot>,
) -> Arc<crate::store::Snapshot> {
    let union = circuit
        .snap
        .as_ref()
        .map_or(snap.union_default_graph, |s| s.union_default_graph);
    if union == snap.union_default_graph {
        snap.clone()
    } else {
        let mut s = (**snap).clone();
        s.union_default_graph = union;
        Arc::new(s)
    }
}

/// Fold history diff slices outside the circuit lock while admitting every allocation
/// through `admit` and polling `check`.
fn build_rewound(
    version: Arc<ViewVersion>,
    commit: u64,
    parts: Vec<HistRows>,
    check: &dyn Fn() -> Result<()>,
    admit: &mut dyn FnMut(u64) -> Result<()>,
) -> Result<RewoundView> {
    type NetEntry = (Vec<Option<Term>>, (i64, usize));
    let entry_size = std::mem::size_of::<NetEntry>();
    let mut net_map: FxHashMap<Vec<Option<Term>>, (i64, usize)> = FxHashMap::default();
    let mut entries_bytes = 0u64;
    let mut seq = 0usize;
    for part in &parts {
        for (r, w) in part.iter() {
            if seq.is_multiple_of(256) {
                check()?;
            }
            if *w == 0 {
                continue;
            }
            if let Some((cur, _)) = net_map.get_mut(r) {
                *cur += *w;
                continue;
            }
            check()?;
            let row_b = row_retained_bytes(r);
            let old_cap = net_map.capacity();
            let new_cap = table_grown(old_cap, net_map.len(), 1);
            let old_table = table_bytes(old_cap, entry_size) as u64;
            let new_table = table_bytes(new_cap, entry_size) as u64;
            let table_peak = if new_cap != old_cap {
                old_table.saturating_add(new_table)
            } else {
                old_table
            };
            admit(
                entries_bytes
                    .saturating_add(row_b)
                    .saturating_add(table_peak),
            )?;
            net_map.insert(r.clone(), (*w, seq));
            entries_bytes = entries_bytes.saturating_add(row_b);
            seq += 1;
        }
    }
    check()?;
    let mut n_added = 0usize;
    let mut n_removed = 0usize;
    for &(w, _) in net_map.values() {
        if w > 0 {
            n_added += 1;
        } else if w < 0 {
            n_removed += 1;
        }
    }
    let old_table = table_bytes(net_map.capacity(), entry_size) as u64;
    let added_cap = if n_added == 0 {
        0
    } else {
        table_grown(0, 0, n_added)
    };
    let added_table =
        table_bytes(added_cap, std::mem::size_of::<(Vec<Option<Term>>, i64)>()) as u64;
    let removed_indexed_bytes =
        (n_removed * std::mem::size_of::<(usize, Vec<Option<Term>>, i64)>()) as u64;
    let removed_final_bytes = (n_removed * std::mem::size_of::<(Vec<Option<Term>>, i64)>()) as u64;
    // Admit the peak during the split (old map table + new containers, while row payloads
    // move out without cloning).
    admit(
        entries_bytes
            .saturating_add(old_table)
            .saturating_add(added_table)
            .saturating_add(removed_indexed_bytes)
            .saturating_add(removed_final_bytes),
    )?;
    let mut added: FxHashMap<Vec<Option<Term>>, i64> = FxHashMap::default();
    if n_added > 0 {
        added.reserve(n_added);
    }
    let mut removed_indexed: Vec<(usize, Vec<Option<Term>>, i64)> = Vec::with_capacity(n_removed);
    for (row, (w, idx)) in net_map {
        if w > 0 {
            added.insert(row, w);
        } else if w < 0 {
            removed_indexed.push((idx, row, -w));
        }
    }
    removed_indexed.sort_unstable_by_key(|(idx, _, _)| *idx);
    let mut removed: Vec<(Vec<Option<Term>>, i64)> = Vec::with_capacity(removed_indexed.len());
    for (_, row, w) in removed_indexed {
        removed.push((row, w));
    }
    let rewound = RewoundView {
        version,
        commit,
        added,
        removed,
    };
    admit(rewound.retained_bytes())?;
    Ok(rewound)
}

impl Runtime {
    /// The rows of view `name` (spec F13 §2.2).
    pub fn read(&self, r: &ViewRead<'_>) -> Result<ViewRows> {
        let open = ViewOpen {
            read: ViewRead {
                name: r.name,
                at: r.at,
                snap: r.snap,
                allow_stale: r.allow_stale,
                caller: r.caller,
                deadline: r.deadline,
                aggregate_errors: r.aggregate_errors,
            },
            cancel: None,
            max_memory_bytes: None,
        };
        let opened = self.open_view(&open, &|| Ok(()), &mut |_| Ok(()))?;
        match opened {
            OpenedView::Shared(v) | OpenedView::Owned(v) => Ok(v.to_view_rows()),
            OpenedView::Rows(rows) => Ok(rows),
            OpenedView::Rewound(rewound) => {
                let columns = rewound.version.columns.to_vec();
                let mut added = rewound.added;
                let mut rows = Vec::new();
                for i in 0..rewound.version.table.len() {
                    let (ids, w) = rewound.version.table.row(i);
                    let terms: Vec<Option<Term>> = ids
                        .iter()
                        .map(|&id| rewound.version.terms.term(&rewound.version.snap, id))
                        .collect();
                    let emit = if let Some(d) = added.get_mut(&terms) {
                        let take = w.min(*d);
                        *d -= take;
                        if *d == 0 {
                            added.remove(&terms);
                        }
                        w - take
                    } else {
                        w
                    };
                    if emit > 0 {
                        rows.push((terms, emit));
                    }
                }
                rows.extend(rewound.removed);
                Ok(ViewRows {
                    columns,
                    rows,
                    commit: rewound.commit,
                    evaluated_under: None,
                })
            }
            OpenedView::Rebuild(rebuild) => {
                let whole = whole_reason(rebuild.access.as_ref())
                    .map(str::to_string)
                    .or_else(|| rebuild.inference.as_ref().map(|i| i.reason().to_string()));
                let mut c = Circuit::new(
                    compile(&rebuild.query, whole.as_deref())?,
                    rebuild.access.clone(),
                );
                c.inference = rebuild.inference.clone();
                c.aggregate_errors = Some(rebuild.aggregate_errors);
                let control = Control {
                    deadline: r.deadline.or(self.control().deadline),
                    ..Control::none()
                };
                c.build(rebuild.snap, &control)?;
                Ok(ViewRows {
                    columns: rebuild.columns.to_vec(),
                    rows: c.rows(),
                    commit: rebuild.commit,
                    evaluated_under: rebuild.evaluated_under,
                })
            }
        }
    }

    /// Open view `open.read.name` for streaming reads without holding the circuit lock
    /// across pulls or copying its rows on current and stale reads (item 1121).
    pub fn open_view(
        &self,
        open: &ViewOpen<'_>,
        check: &dyn Fn() -> Result<()>,
        admit: &mut dyn FnMut(u64) -> Result<()>,
    ) -> Result<OpenedView> {
        let _operation = Operation::enter();
        let r = &open.read;
        let wait = Instant::now() + self.opts.read().view_wait;
        let until = r.deadline.map_or(wait, |d| d.min(wait));
        loop {
            check()?;
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
                // The reading and the status are read under the state lock, which a
                // switch of the reading takes to replace the circuit and mark it
                // `building`, so they describe the circuit whose version is returned. A
                // build holds that lock, so wait in short cooperative steps until `until`.
                let step = (Instant::now() + WAIT_STEP).min(until);
                if let Some(mut st) = cell.state.try_lock_until(step) {
                    let held = st.circuit.aggregate_errors.unwrap_or(errors);
                    // another reading of errors than the view's: evaluate the view's query
                    // at the reader's snapshot without touching the view's state
                    if let Some(m) = r.aggregate_errors.filter(|m| *m != held) {
                        return self.open_evaluate_under(st, open, m, check, admit);
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
                            if st.circuit.applied == r.at || r.allow_stale {
                                let version = st.circuit.version_or_build().ok_or_else(|| {
                                    crate::incr::error(
                                        "view-failed",
                                        format!("the view {:?} has no materialized result", r.name),
                                    )
                                })?;
                                drop(st);
                                return Ok(OpenedView::Shared(version));
                            }
                            if st.circuit.applied > r.at {
                                return self.open_read_back(st, open, held, check, admit);
                            }
                        }
                        Status::Building => {}
                    }
                }
            }
            check()?;
            self.kick();
            let step = (Instant::now() + WAIT_STEP).min(until);
            let woke = self.wait_round(step);
            check()?;
            if !woke && Instant::now() >= until {
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

    /// Open a view at `r.at` when its circuit has moved past it: rewind from history
    /// outside the circuit lock, or prepare a snapshot evaluation when history expired.
    fn open_read_back(
        &self,
        mut st: parking_lot::MutexGuard<'_, Slot>,
        open: &ViewOpen<'_>,
        held: crate::sparql::AggregateErrors,
        check: &dyn Fn() -> Result<()>,
        admit: &mut dyn FnMut(u64) -> Result<()>,
    ) -> Result<OpenedView> {
        let r = &open.read;
        if let Some(parts) = st.history_parts_since(r.at) {
            let version = st.circuit.version_or_build().ok_or_else(|| {
                crate::incr::error(
                    "view-failed",
                    format!("the view {:?} has no materialized result", r.name),
                )
            })?;
            drop(st);
            if parts.is_empty() {
                return Ok(OpenedView::Shared(Arc::new(version.at_commit(r.at))));
            }
            let rewound = build_rewound(version, r.at, parts, check, admit)?;
            return Ok(OpenedView::Rewound(rewound));
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
        let snap = view_snapshot(&st.circuit, snap);
        if matches!(st.query, Query::Select { .. }) {
            let rebuild = ViewRebuild {
                columns: st.vars.clone(),
                query: st.query.clone(),
                snap,
                access: st.circuit.access.clone(),
                inference: st.circuit.inference.clone(),
                aggregate_errors: held,
                commit: r.at,
                evaluated_under: None,
            };
            drop(st);
            return Ok(OpenedView::Rebuild(Box::new(rebuild)));
        }
        let c = st.fresh(st.circuit.inference.clone())?;
        drop(st);
        self.build_owned_version(c, open, snap, r.at, None, check, admit)
    }

    /// Open a view evaluated under the reader's `AggregateErrors` reading `m`, which
    /// differs from the view's. The view's state is neither read nor changed.
    fn open_evaluate_under(
        &self,
        st: parking_lot::MutexGuard<'_, Slot>,
        open: &ViewOpen<'_>,
        m: crate::sparql::AggregateErrors,
        check: &dyn Fn() -> Result<()>,
        admit: &mut dyn FnMut(u64) -> Result<()>,
    ) -> Result<OpenedView> {
        let r = &open.read;
        let Some(snap) = r.snap else {
            return Err(crate::incr::error(
                "view-reading",
                format!(
                    "the view {:?} keeps another reading of aggregate errors than {m}, and there is no snapshot to evaluate its query at",
                    r.name
                ),
            ));
        };
        let snap = view_snapshot(&st.circuit, snap);
        let commit = snap.commit;
        if matches!(st.query, Query::Select { .. }) {
            let rebuild = ViewRebuild {
                columns: st.vars.clone(),
                query: st.query.clone(),
                snap,
                access: st.circuit.access.clone(),
                inference: st.circuit.inference.clone(),
                aggregate_errors: m,
                commit,
                evaluated_under: Some(m),
            };
            drop(st);
            return Ok(OpenedView::Rebuild(Box::new(rebuild)));
        }
        let mut c = st.fresh(st.circuit.inference.clone())?;
        drop(st);
        c.aggregate_errors = Some(m);
        self.build_owned_version(c, open, snap, commit, Some(m), check, admit)
    }

    /// Evaluate a non-SELECT (`CONSTRUCT`) view circuit for one read under the caller's
    /// deadline, cancellation and memory limit, admitting the owned version's bytes.
    #[allow(clippy::too_many_arguments)]
    fn build_owned_version(
        &self,
        mut c: Circuit,
        open: &ViewOpen<'_>,
        snap: Arc<crate::store::Snapshot>,
        commit: u64,
        evaluated_under: Option<crate::sparql::AggregateErrors>,
        check: &dyn Fn() -> Result<()>,
        admit: &mut dyn FnMut(u64) -> Result<()>,
    ) -> Result<OpenedView> {
        check()?;
        if let Some(limit) = open.max_memory_bytes {
            c.max_memory_bytes = Some(c.max_memory_bytes.map_or(limit, |m| m.min(limit)));
        }
        let deadline = open.read.deadline.or(self.control().deadline);
        let control = match &open.cancel {
            Some(flag) => Control {
                deadline,
                ..Control::with_cancel(flag.clone())
            },
            None => Control {
                deadline,
                ..Control::none()
            },
        };
        c.build(snap, &control)?;
        check()?;
        let mut version = c
            .version_or_build()
            .map(|v| (*v).clone())
            .ok_or_else(|| crate::incr::error("view-failed", "rebuilt view has no result"))?;
        drop(c);
        version.commit = commit;
        version.evaluated_under = evaluated_under;
        admit(version.retained_bytes())?;
        Ok(OpenedView::Owned(Arc::new(version)))
    }
}

impl Runtime {
    /// The reading of errors in aggregates that view `name` keeps: the dataset's
    /// current reading, which a read waits for the view to be built under. `None`
    /// without such a view, or when the registry stays locked past `deadline` or the
    /// read wait.
    ///
    /// Planning asks this only to note a read that evaluates the view's query, so it
    /// asks the host for its reading instead of switching the circuits to it, which
    /// would wait for the passes of every view.
    pub fn view_reading(
        &self,
        name: &str,
        deadline: Option<Instant>,
    ) -> Option<crate::sparql::AggregateErrors> {
        // the host's callback can drop the last handle, so the close waits for the
        // operation's end
        let _operation = Operation::enter();
        let host = self.host.read().clone();
        let host_reading = host.as_ref().map(|h| h.aggregate_errors());
        drop(host);
        let wait = Instant::now() + self.opts.read().view_wait;
        let until = deadline.map_or(wait, |d| d.min(wait));
        let reg = self.reg.try_lock_until(until)?;
        reg.view(name).ok()?;
        Some(host_reading.unwrap_or(reg.aggregate_errors))
    }
}

impl ViewSource for Runtime {
    fn read_view(&self, r: &ViewRead<'_>) -> Result<ViewRows> {
        self.read(r)
    }
    fn open_view(
        &self,
        open: &ViewOpen<'_>,
        check: &dyn Fn() -> Result<()>,
        admit: &mut dyn FnMut(u64) -> Result<()>,
    ) -> Result<OpenedView> {
        Runtime::open_view(self, open, check, admit)
    }
    fn view_reading(
        &self,
        name: &str,
        deadline: Option<Instant>,
    ) -> Option<crate::sparql::AggregateErrors> {
        Runtime::view_reading(self, name, deadline)
    }
}
