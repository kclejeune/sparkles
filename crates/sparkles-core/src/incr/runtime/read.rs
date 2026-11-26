//! Reading a view's rows (spec F13 §2.2), eagerly or as an immutable shared version
//! for streaming cursors (item 1121).

use super::*;
use sparkles_budget::sizes::{table_bytes, table_grown};

/// Step duration while waiting for a view to catch up or build so cooperative
/// cancellation checks run promptly without busy-spinning.
const WAIT_STEP: Duration = Duration::from_millis(20);

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
    let mut seen = 0usize;
    let parts_bytes = (parts.capacity() * std::mem::size_of::<HistRows>()) as u64;
    for part in &parts {
        for (r, w) in part.iter() {
            seen += 1;
            if seen.is_multiple_of(256) {
                check()?;
            }
            if *w == 0 {
                continue;
            }
            if let Some((cur, _)) = net_map.get_mut(r) {
                *cur = cur.checked_add(*w).ok_or_else(history_weight_overflow)?;
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
                    .saturating_add(table_peak)
                    .saturating_add(parts_bytes),
            )?;
            net_map.insert(r.clone(), (*w, seq));
            entries_bytes = entries_bytes.saturating_add(row_b);
            seq += 1;
        }
    }
    drop(parts);
    check()?;
    let mut n_added = 0usize;
    let mut n_removed = 0usize;
    for (i, &(w, _)) in net_map.values().enumerate() {
        if i.is_multiple_of(256) {
            check()?;
        }
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
    for (i, (row, (w, idx))) in net_map.into_iter().enumerate() {
        if i.is_multiple_of(256) {
            check()?;
        }
        if w > 0 {
            added.insert(row, w);
        } else if w < 0 {
            removed_indexed.push((
                idx,
                row,
                w.checked_neg().ok_or_else(history_weight_overflow)?,
            ));
        }
    }
    removed_indexed.sort_unstable_by_key(|(idx, _, _)| *idx);
    let mut removed: Vec<(Vec<Option<Term>>, i64)> = Vec::with_capacity(removed_indexed.len());
    for (i, (_, row, w)) in removed_indexed.into_iter().enumerate() {
        if i.is_multiple_of(256) {
            check()?;
        }
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

fn history_weight_overflow() -> Error {
    crate::incr::error("view-failed", "view history multiplicity overflows")
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
                memory_parent: r.memory_parent,
                memory_pool: r.memory_pool,
                max_memory_bytes: r.max_memory_bytes,
            },
        };
        let control = Control {
            deadline: r.deadline,
            ..Control::none()
        };
        let opened = self.open_view(&open, &|| control.check(), &mut |_| Ok(()))?;
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
                    lease: None,
                })
            }
            OpenedView::Rebuild(rebuild) => {
                let whole = key::whole_reason_on(rebuild.access.as_ref(), &rebuild.snap)
                    .map(str::to_string)
                    .or_else(|| rebuild.inference.as_ref().map(|i| i.reason().to_string()));
                let mut c = Circuit::new(
                    compile(&rebuild.query, whole.as_deref())?,
                    rebuild.access.clone(),
                );
                c.inference = rebuild.inference.clone();
                c.aggregate_errors = Some(rebuild.aggregate_errors);
                c.max_memory_bytes = rebuild.max_memory_bytes;
                let (pool, parent) = self.resolve_fallback_budget(r)?;
                c.set_pool(pool, parent);
                if let Some(limit) = r.max_memory_bytes {
                    let effective = c.max_memory_bytes.map_or(limit, |m| m.min(limit));
                    c.set_local_limit(Some(effective));
                }
                let control = Control {
                    deadline: rebuild.deadline,
                    ..Control::none()
                };
                let snap = crate::sparql::with_union_default(
                    rebuild.snap,
                    &crate::sparql::QueryOptions {
                        union_default_graph: Some(rebuild.union_default_graph),
                        ..Default::default()
                    },
                );
                c.build(snap, &control)?;
                let (rows, lease) = c.rows_budgeted()?;
                Ok(ViewRows {
                    columns: rebuild.columns.to_vec(),
                    rows,
                    commit: rebuild.commit,
                    evaluated_under: rebuild.evaluated_under,
                    lease,
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
            let host = self.host.read().clone();
            let mode = host.map(|host| (host.aggregate_errors(), host.inference()));
            let step = (Instant::now() + WAIT_STEP).min(until);
            let entry = if let Some(reg) = self.reg.try_lock_until(step) {
                check()?;
                let mode_ready = mode.as_ref().is_none_or(|(errors, inference)| {
                    reg.aggregate_errors == *errors
                        && Inference::same(reg.inference.as_deref(), inference.as_ref())
                });
                if mode_ready {
                    let e = reg.view(r.name)?;
                    Some((
                        e.cell.clone(),
                        e.access.clone(),
                        e.suspended.clone(),
                        e.error.clone(),
                        reg.aggregate_errors,
                    ))
                } else {
                    None
                }
            } else {
                None
            };
            if let Some((cell, access, suspended, error, errors)) = entry {
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
                    if let Some(st) = cell.state.try_lock_until(step) {
                        check()?;
                        let held = st.circuit.aggregate_errors.unwrap_or(errors);
                        // another reading of errors than the view's: evaluate the view's query
                        // at the reader's snapshot without touching the view's state
                        if let Some(m) = r.aggregate_errors.filter(|m| *m != held) {
                            return self.open_evaluate_under(st, open, m, admit);
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
                                    let version = st.circuit.version.clone().ok_or_else(|| {
                                        crate::incr::error(
                                            "view-failed",
                                            format!(
                                                "the view {:?} has no materialized result",
                                                r.name
                                            ),
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
        st: parking_lot::MutexGuard<'_, Slot>,
        open: &ViewOpen<'_>,
        held: crate::sparql::AggregateErrors,
        check: &dyn Fn() -> Result<()>,
        admit: &mut dyn FnMut(u64) -> Result<()>,
    ) -> Result<OpenedView> {
        let r = &open.read;
        let parts = st.history_rows_since(r.at, check, admit)?;
        if let Some(parts) = parts {
            let version = st.circuit.version.clone().ok_or_else(|| {
                crate::incr::error(
                    "view-failed",
                    format!("the view {:?} has no materialized result", r.name),
                )
            })?;
            drop(st);
            if parts.is_empty() {
                return Ok(OpenedView::Rewound(RewoundView {
                    version,
                    commit: r.at,
                    added: Default::default(),
                    removed: Vec::new(),
                }));
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
        self.prepare_rebuild(&st, open, snap, held, None, admit)
    }

    /// Resolve a coherent `(pool, parent)` pair for fallback evaluation (`read_back` /
    /// `evaluate_under`) so `parent.domain()` always matches `pool`, unpooled callers
    /// never bypass `self.pool`, and `Ctx::memory_parent()` is invoked lazily.
    fn resolve_fallback_budget(
        &self,
        r: &ViewRead<'_>,
    ) -> Result<(Option<Arc<QueryPool>>, Option<ParentHandle>)> {
        let unpooled_id = sparkles_budget::MemoryDomain::unpooled().id();
        if let Some(rp) = r.memory_pool {
            if let Some(get_parent) = r.memory_parent {
                let parent = get_parent()?;
                if parent.domain() != rp.domain() {
                    return Err(Error::invalid(
                        "view read memory_parent domain does not match memory_pool",
                    ));
                }
                return Ok((Some(rp.clone()), Some(parent)));
            }
            return Ok((Some(rp.clone()), Some(rp.work_parent())));
        }
        if let Some(sp) = &self.pool {
            if let Some(get_parent) = r.memory_parent {
                let parent = get_parent()?;
                if parent.domain() == sp.domain() {
                    return Ok((Some(sp.clone()), Some(parent)));
                }
                if parent.domain() == unpooled_id {
                    // An unpooled query reading a pooled dataset's view must still enforce
                    // both the outer query's aggregate limit and the dataset's QueryPool
                    // during fallback evaluation.
                    return Ok((Some(sp.clone()), Some(sp.combine_unpooled_parent(parent))));
                }
                return Err(Error::invalid(
                    "view read memory_parent belongs to another QueryPool without matching memory_pool",
                ));
            }
            return Ok((Some(sp.clone()), Some(sp.work_parent())));
        }
        if let Some(get_parent) = r.memory_parent {
            let parent = get_parent()?;
            if parent.domain() != unpooled_id {
                return Err(Error::invalid(
                    "pooled view read memory_parent requires its matching memory_pool",
                ));
            }
            return Ok((None, Some(parent)));
        }
        Ok((None, None))
    }

    /// Open a view evaluated under the reader's `AggregateErrors` reading `m`, which
    /// differs from the view's. The view's state is neither read nor changed.
    fn open_evaluate_under(
        &self,
        st: parking_lot::MutexGuard<'_, Slot>,
        open: &ViewOpen<'_>,
        m: crate::sparql::AggregateErrors,
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
        self.prepare_rebuild(&st, open, snap, m, Some(m), admit)
    }

    fn prepare_rebuild(
        &self,
        st: &Slot,
        open: &ViewOpen<'_>,
        snap: &Arc<crate::store::Snapshot>,
        aggregate_errors: crate::sparql::AggregateErrors,
        evaluated_under: Option<crate::sparql::AggregateErrors>,
        admit: &mut dyn FnMut(u64) -> Result<()>,
    ) -> Result<OpenedView> {
        let union_default_graph = st
            .circuit
            .snap
            .as_ref()
            .map_or(snap.union_default_graph, |s| s.union_default_graph);
        let rebuild = ViewRebuild {
            columns: st.vars.clone(),
            query: st.query.clone(),
            snap: snap.clone(),
            access: st.circuit.access.clone(),
            deadline: open.read.deadline.or_else(|| self.control().deadline),
            max_memory_bytes: st.circuit.max_memory_bytes,
            union_default_graph,
            patterns: st.circuit.rebuild_patterns.clone(),
            template_columns: st.circuit.template_columns.clone(),
            inference: st.circuit.inference.clone(),
            aggregate_errors,
            commit: snap.commit,
            evaluated_under,
            fallback_pool: self.pool.clone(),
        };
        // The planner clones algebra and template terms from the stored query.
        // Cover that staging before handing the parsed query to the child.
        let planning_bytes = (st.query_text_bytes as u64).saturating_mul(16);
        admit(
            (std::mem::size_of::<ViewRebuild>() as u64)
                .saturating_add(rebuild.snapshot_bytes())
                .saturating_add(planning_bytes),
        )?;
        Ok(OpenedView::Rebuild(Box::new(rebuild)))
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
        let reg = self
            .reg
            .try_lock_until(until.min(Instant::now() + WAIT_STEP))?;
        reg.view(name).ok()?;
        Some(host_reading.unwrap_or(reg.aggregate_errors))
    }
}

impl ViewSource for Runtime {
    fn read_view(&self, r: &ViewRead<'_>) -> Result<ViewRows> {
        self.read(r)
    }
    fn streams_views(&self) -> bool {
        true
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::StoreOptions;

    fn fixture() -> (Store, Arc<Runtime>) {
        let s = Store::in_memory(StoreOptions::default());
        let rt = Runtime::open(None, s.dataset_id(), RuntimeOptions::default());
        rt.set_host(Arc::new(OpenHost));
        rt.put(&s, "v", ViewDef::table("SELECT ?s WHERE { ?s <urn:p> ?o }"))
            .unwrap();
        let data = (0..512)
            .map(|i| format!("<urn:s{i}> <urn:p> 1 . "))
            .collect::<String>();
        crate::sparql::update::update(
            &s,
            &format!("INSERT DATA {{ {data} }}"),
            &Default::default(),
        )
        .unwrap();
        rt.round(&s);
        (s, rt)
    }

    #[test]
    fn readers_check_controls_while_registry_is_busy() {
        let (s, rt) = fixture();
        let reader = rt.clone();
        let reg = rt.reg.lock();
        let (ready, entered) = std::sync::mpsc::channel();
        let (sender, result) = std::sync::mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let flag = cancel.clone();
        let snap = s.snapshot();
        let child = std::thread::spawn(move || {
            let open = ViewOpen {
                read: ViewRead {
                    name: "v",
                    at: snap.commit,
                    snap: Some(&snap),
                    allow_stale: true,
                    caller: None,
                    deadline: None,
                    aggregate_errors: None,
                    memory_parent: None,
                    memory_pool: None,
                    max_memory_bytes: None,
                },
            };
            let announced = std::cell::Cell::new(false);
            let got = reader.open_view(
                &open,
                &|| {
                    if !announced.replace(true) {
                        ready.send(()).unwrap();
                    }
                    if flag.load(Ordering::Relaxed) {
                        Err(Error::Cancelled)
                    } else {
                        Ok(())
                    }
                },
                &mut |_| Ok(()),
            );
            sender.send(matches!(got, Err(Error::Cancelled))).unwrap();
        });
        entered.recv_timeout(Duration::from_secs(1)).unwrap();
        cancel.store(true, Ordering::Relaxed);
        let stopped = result.recv_timeout(Duration::from_secs(1));
        drop(reg);
        child.join().unwrap();
        assert!(
            stopped.unwrap(),
            "the read must stop before the registry is released"
        );
    }

    #[test]
    fn paused_versions_spend_pool_credit_until_last_reader() {
        let (s, rt) = fixture();
        let version = {
            let reg = rt.reg.lock();
            let cell = reg.views.get("v").unwrap().cell.as_ref().unwrap();
            cell.state.lock().circuit.version.clone().unwrap()
        };
        let second_reader = version.clone();
        let before = rt.published_views.load(Ordering::Relaxed);
        assert_eq!(before, version.published_bytes());
        let state = rt.get(&s, "v").unwrap().state_bytes;
        rt.set_options(RuntimeOptions {
            views_memory: Some(state + before / 2),
            ..rt.options()
        });
        crate::sparql::update::update(
            &s,
            "DELETE DATA { <urn:s0> <urn:p> 1 }",
            &Default::default(),
        )
        .unwrap();
        rt.round(&s);
        assert!(matches!(
            rt.get(&s, "v").unwrap().state,
            ViewState::Suspended { .. }
        ));
        assert_eq!(rt.published_views.load(Ordering::Relaxed), before);
        drop(version);
        assert_eq!(rt.published_views.load(Ordering::Relaxed), before);
        drop(second_reader);
        assert_eq!(rt.published_views.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn overlapping_term_tables_share_the_large_key_charge() {
        let (s, rt) = fixture();
        let update =
            |text: &str| crate::sparql::update::update(&s, text, &Default::default()).unwrap();
        update(&format!(
            "INSERT DATA {{ <urn:a> <urn:label> \"{}\" }}",
            "x".repeat(256 << 10)
        ));
        rt.put(
            &s,
            "local",
            ViewDef::table("SELECT (CONCAT(\"v-\", ?text) AS ?value) { ?s <urn:label> ?text }"),
        )
        .unwrap();
        rt.round(&s);
        let cell = rt.reg.lock().views["local"].cell.clone().unwrap();
        let get_version = || cell.state.lock().circuit.version.clone().unwrap();
        let old = get_version();
        assert_eq!(old.terms.len(), 1);
        let before = rt.published_views.load(Ordering::Relaxed);
        let local_state = rt.get(&s, "local").unwrap().state_bytes;
        let other_state = rt.get(&s, "v").unwrap().state_bytes;
        rt.set_options(RuntimeOptions {
            views_memory: Some(local_state + other_state + (64 << 10)),
            ..rt.options()
        });
        update("INSERT DATA { <urn:b> <urn:label> \"y\" }");
        rt.round(&s);
        assert!(matches!(
            rt.get(&s, "local").unwrap().state,
            ViewState::Current
        ));
        let new = get_version();
        assert_eq!(new.terms.len(), 2);
        assert!(!Arc::ptr_eq(&old.terms, &new.terms));
        assert!(rt.published_views.load(Ordering::Relaxed) - before < 64 << 10);
        rt.delete("local").unwrap();
        rt.delete("v").unwrap();
        rt.round(&s);
        drop(new);
        assert!(rt.published_views.load(Ordering::Relaxed) >= 256 << 10);
        drop(old);
        assert_eq!(rt.published_views.load(Ordering::Relaxed), 0);
        // A subscriber can keep the circuit's term backing alive after detachment.
        assert_eq!(cell.state.lock().circuit.terms.key_count(), 2);
    }

    #[test]
    fn repeated_history_keys_poll_controls_during_the_fold() {
        let (_, rt) = fixture();
        let version = {
            let reg = rt.reg.lock();
            let cell = reg.views.get("v").unwrap().cell.as_ref().unwrap();
            cell.state.lock().circuit.version.clone().unwrap()
        };
        let part: HistRows = vec![(vec![None], 1); 1600].into();
        let admitted = std::cell::Cell::new(0);
        let outcome = build_rewound(
            version,
            0,
            vec![part],
            &|| {
                let checks = admitted.get() + 1;
                admitted.set(checks);
                if checks == 5 {
                    Err(Error::Cancelled)
                } else {
                    Ok(())
                }
            },
            &mut |_| Ok(()),
        );
        assert!(matches!(outcome, Err(Error::Cancelled)));
    }
}
