//! Reading a view's rows (spec F13 §2.2), eagerly or as an immutable shared version
//! for streaming cursors (item 1121).

use super::*;
use sparkles_budget::sizes::{table_bytes, table_grown};

/// Step duration while waiting for a view to catch up or build so cooperative
/// cancellation checks run promptly without busy-spinning.
pub(super) const WAIT_STEP: Duration = Duration::from_millis(20);

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

fn columns_bytes(columns: &[String]) -> u64 {
    (std::mem::size_of_val(columns) + columns.iter().map(String::len).sum::<usize>()) as u64
}

type WeightedRow = (Vec<Option<Term>>, i64);

/// Decoded rows with the scratch credit that covers them. The rows are declared
/// first so that they are freed before the credit returns.
pub(super) struct ChargedRows {
    pub(super) rows: Vec<WeightedRow>,
    pub(super) lease: crate::incr::budget::ScratchLease,
}
type HistoryOverlay<'a> = (&'a mut FxHashMap<Vec<Option<Term>>, i64>, &'a [WeightedRow]);

fn decode_rows(
    budget: &crate::incr::budget::ViewBudget,
    version: &ViewVersion,
    commit: u64,
    mut rewind: Option<HistoryOverlay<'_>>,
    check: &dyn Fn() -> Result<()>,
) -> Result<ViewRows> {
    let mut lease = budget.scratch_lease();
    let removed = rewind.as_ref().map_or(&[][..], |(_, removed)| *removed);
    let slots = version.table.len().saturating_add(removed.len());
    lease.try_reserve(
        columns_bytes(&version.columns)
            .saturating_add(slots.saturating_mul(std::mem::size_of::<WeightedRow>()) as u64),
    )?;
    let columns = version.columns.to_vec();
    let mut rows = Vec::with_capacity(slots);
    for i in 0..version.table.len() {
        check()?;
        let (ids, weight) = version.table.row(i);
        let before = lease.bytes();
        lease.try_reserve((ids.len() * std::mem::size_of::<Option<Term>>()) as u64)?;
        let mut terms = Vec::with_capacity(ids.len());
        for &id in ids {
            terms.push(version.terms.term_budgeted(&version.snap, id, &mut lease)?);
        }
        let weight = rewind
            .as_mut()
            .and_then(|(added, _)| added.get_mut(&terms))
            .map_or(weight, |added| {
                let consumed = weight.min(*added);
                *added -= consumed;
                weight - consumed
            });
        if weight > 0 {
            rows.push((terms, weight));
        } else {
            drop(terms);
            lease.try_set(before)?;
        }
    }
    for (terms, weight) in removed {
        check()?;
        lease.try_reserve(row_retained_bytes(terms))?;
        rows.push((terms.clone(), *weight));
    }
    let lease = budget.detach_scratch_lease(&mut lease);
    Ok(ViewRows {
        columns,
        rows,
        commit,
        evaluated_under: version.evaluated_under,
        lease,
    })
}

fn history_weight_overflow() -> Error {
    crate::incr::error("view-failed", "view history multiplicity overflows")
}

impl Runtime {
    /// The rows of view `name` (spec F13 §2.2).
    pub fn read(&self, r: &ViewRead<'_>) -> Result<ViewRows> {
        self.read_with_check(r, &|| Ok(()))
    }

    /// Like [`read`](Self::read), polling `check` cooperatively while waiting for the
    /// view, rewinding history or evaluating a fallback.
    pub fn read_with_check(
        &self,
        r: &ViewRead<'_>,
        check: &dyn Fn() -> Result<()>,
    ) -> Result<ViewRows> {
        self.read_controlled(r, check, &crate::task::Cancellation::default)
    }

    /// Read with the query's cancellation flags for nested SPARQL in a fallback circuit.
    #[doc(hidden)]
    pub fn read_controlled(
        &self,
        r: &ViewRead<'_>,
        check: &dyn Fn() -> Result<()>,
        cancellation: &dyn Fn() -> crate::task::Cancellation,
    ) -> Result<ViewRows> {
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
        let combined = || {
            control.check()?;
            check()
        };
        // The memory of the read is set up once the view admits the caller, so that a
        // refusal comes before any memory parent is resolved or reserved.
        let mut memory = None;
        let opened = self.open_view(&open, &combined, &mut |bytes| {
            let (_, _, _, scratch) = self.fallback_memory(r, &mut memory)?;
            scratch.try_reserve(bytes.saturating_sub(scratch.bytes()))
        })?;
        self.fallback_memory(r, &mut memory)?;
        let (pool, parent, budget, mut scratch) = memory.expect("set up above");
        match opened {
            OpenedView::Shared(version) | OpenedView::Owned(version) => {
                decode_rows(&budget, &version, version.commit, None, &combined)
            }
            OpenedView::Rows(rows) => Ok(rows),
            OpenedView::Rewound(mut rewound) => {
                scratch.try_set(rewound.retained_bytes())?;
                decode_rows(
                    &budget,
                    &rewound.version,
                    rewound.commit,
                    Some((&mut rewound.added, &rewound.removed)),
                    &combined,
                )
            }
            OpenedView::Rebuild(rebuild) => {
                let cancellation = cancellation();
                let check_fallback = || {
                    cancellation.check()?;
                    combined()
                };
                check_fallback()?;
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
                c.set_runtime_pool(pool, parent);
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
                #[cfg(test)]
                fire_fallback_hook(FallbackEvent::Evaluating);
                c.build_for_read(
                    snap,
                    &control,
                    crate::incr::pass::BuildChecks {
                        check: &check_fallback,
                        cancellation: &cancellation,
                    },
                )?;
                #[cfg(test)]
                fire_fallback_hook(FallbackEvent::Built);
                // The decoded rows stay with the scratch credit that covers them in an
                // owner that frees the rows first, and the detached credit goes straight
                // into the returned rows, so no error return releases the credit while
                // the rows are still allocated.
                let mut charged = c
                    .rows_charged_checked(&check_fallback)
                    .map(|(rows, lease)| ChargedRows { rows, lease })?;
                #[cfg(test)]
                fire_fallback_hook(FallbackEvent::Decoded(&charged.rows));
                charged.lease.try_reserve(columns_bytes(&rebuild.columns))?;
                let mut out = ViewRows {
                    columns: rebuild.columns.to_vec(),
                    rows: charged.rows,
                    commit: rebuild.commit,
                    evaluated_under: rebuild.evaluated_under,
                    lease: None,
                };
                out.lease = c.detach_scratch_lease(&mut charged.lease);
                check_fallback()?;
                Ok(out)
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
        let mut final_check = false;
        // a read is audited once for each (decision, generation) pair so that a
        // view replacement with equal grants across a wait emits a fresh record
        let mut last: Option<(ViewDecision, u64)> = None;
        loop {
            let round_start = self.round_epoch();
            check()?;
            if self.is_closed() {
                return Err(crate::incr::error(
                    "views-unavailable",
                    "the dataset is closing",
                ));
            }
            if r.deadline.is_some_and(|d| Instant::now() >= d) {
                return Err(Error::Timeout);
            }
            let now = Instant::now();
            let reg_guard = if final_check || now >= until {
                self.reg.try_lock()
            } else {
                self.reg.try_lock_until((now + WAIT_STEP).min(until))
            };
            let entry = if let Some(reg) = reg_guard {
                check()?;
                if r.deadline.is_some_and(|d| Instant::now() >= d) {
                    return Err(Error::Timeout);
                }
                if self.is_closed() {
                    return Err(crate::incr::error(
                        "views-unavailable",
                        "the dataset is closing",
                    ));
                }
                let e = reg.view(r.name)?;
                let (grants, generation) = self.view_grants(e);
                Some((
                    e.cell.clone(),
                    grants,
                    generation,
                    e.suspended.clone(),
                    e.error.clone(),
                    reg.aggregate_errors,
                    reg.mode_gen,
                    reg.host_seq,
                ))
            } else {
                None
            };
            if let Some((cell, grants, generation, suspended, error, errors, mode_gen, host_seq)) =
                entry
            {
                // Decided on the entry just read, before its state is reported and
                // before any wait, evaluation or allocation for the read. An entry
                // without a circuit, which is not attached, suspended or failed to
                // attach, is decided as a view that reads every quad.
                let decided = self.decide_cached(r.name, r.caller, grants.as_ref(), generation);
                if last != Some((decided.0, generation)) {
                    self.audit(r.name, r.caller, ViewOp::Read, decided);
                }
                last = Some((decided.0, generation));
                if !decided.0.allowed() {
                    return Err(super::auth::refused(r.name));
                }
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
                    // The mode generation, reading and status are checked against the
                    // current runtime mode generation under the state lock so a delayed
                    // reader never accepts an old-generation version published after a
                    // newer mode was recorded.
                    'observe: {
                        if !self.mode_is_current(mode_gen, host_seq)
                            || cell.info.try_lock().is_none_or(|i| i.mode_gen != mode_gen)
                        {
                            break 'observe;
                        }
                        #[cfg(test)]
                        if let Some(hook) = self.before_read_state_hook.lock().clone() {
                            hook();
                        }
                        #[cfg(test)]
                        fire_open_view_state_hook();
                        let now = Instant::now();
                        let st_guard = if final_check || now >= until {
                            cell.state.try_lock()
                        } else {
                            cell.state.try_lock_until((now + WAIT_STEP).min(until))
                        };
                        let Some(st) = st_guard else {
                            break 'observe;
                        };
                        check()?;
                        if self.is_closed() {
                            return Err(crate::incr::error(
                                "views-unavailable",
                                "the dataset is closing",
                            ));
                        }
                        if r.deadline.is_some_and(|d| Instant::now() >= d) {
                            return Err(Error::Timeout);
                        }
                        if !self.mode_is_current(mode_gen, host_seq) || st.mode_gen != mode_gen {
                            break 'observe;
                        }
                        let held = st.circuit.aggregate_errors.unwrap_or(errors);
                        // another reading of errors than the view's: evaluate the
                        // view's query at the reader's snapshot without touching
                        // the view's state
                        if let Some(m) = r.aggregate_errors.filter(|m| *m != held) {
                            return self.open_evaluate_under(st, open, m, admit);
                        }
                        let Some(status) = cell.info.try_lock().map(|i| i.status.clone()) else {
                            break 'observe;
                        };
                        check()?;
                        if r.deadline.is_some_and(|d| Instant::now() >= d) {
                            return Err(Error::Timeout);
                        }
                        if !self.mode_is_current(mode_gen, host_seq) {
                            break 'observe;
                        }
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
                                let restricted = st
                                    .circuit
                                    .access
                                    .as_ref()
                                    .is_some_and(|a| !a.reads_everything());
                                let ev_mismatch = if restricted {
                                    r.snap.or(st.circuit.snap.as_ref()).is_some_and(|s| {
                                        let (snap_gen, snap_ev) = s.evidence_visibility_stamp();
                                        snap_ev.as_deref() != st.circuit.evidence_key()
                                            || snap_ev.as_deref() == Some("unavailable")
                                            || (s.pinned_evidence.is_none()
                                                && snap_gen != st.circuit.managed_generation())
                                    })
                                } else {
                                    false
                                };
                                if ev_mismatch {
                                    // A pinned historical snapshot with a distinct evidence
                                    // configuration evaluates directly rather than waiting
                                    // for live head to change; otherwise let a runtime
                                    // round refresh or fail closed on live head.
                                    if let Some(snap) = r.snap
                                        && snap.pinned_evidence.is_some()
                                    {
                                        return self
                                            .prepare_rebuild(&st, open, snap, held, None, admit);
                                    }
                                } else if st.circuit.applied == r.at || r.allow_stale {
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
                                } else if st.circuit.applied > r.at {
                                    return self.open_read_back(st, open, held, check, admit);
                                }
                            }
                            Status::Building => {}
                        }
                    }
                }
            }
            check()?;
            if self.is_closed() {
                return Err(crate::incr::error(
                    "views-unavailable",
                    "the dataset is closing",
                ));
            }
            if r.deadline.is_some_and(|d| Instant::now() >= d) {
                return Err(Error::Timeout);
            }
            if final_check || Instant::now() >= until {
                return Err(crate::incr::error(
                    "view-behind",
                    format!(
                        "the view {:?} has not reached commit {} yet; retry, or read it with view:allowStale true",
                        r.name, r.at
                    ),
                ));
            }
            self.kick();
            let step = (Instant::now() + WAIT_STEP).min(until);
            #[cfg(test)]
            self.before_observer_wait(until);
            self.wait_round_after(round_start, step);
            check()?;
            if r.deadline.is_some_and(|d| Instant::now() >= d) {
                return Err(Error::Timeout);
            }
            final_check = Instant::now() >= until;
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
        let restricted = st
            .circuit
            .access
            .as_ref()
            .is_some_and(|a| !a.reads_everything());
        let ev_active = restricted
            && (st.circuit.evidence_key().is_some()
                || r.snap
                    .or(st.circuit.snap.as_ref())
                    .is_some_and(|s| s.has_evidence_projection_or_unavailable()));
        let parts = if ev_active {
            None
        } else {
            st.history_rows_since(r.at, check, admit)?
        };
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
    /// The fallback pool and memory parent of read `r`, its view budget and its scratch
    /// lease, set up in `memory` on first use.
    #[allow(clippy::type_complexity)]
    fn fallback_memory<'m>(
        &self,
        r: &ViewRead<'_>,
        memory: &'m mut Option<(
            Option<Arc<QueryPool>>,
            Option<ParentHandle>,
            crate::incr::budget::ViewBudget,
            crate::incr::budget::ScratchLease,
        )>,
    ) -> Result<(
        &'m mut Option<Arc<QueryPool>>,
        &'m mut Option<ParentHandle>,
        &'m mut crate::incr::budget::ViewBudget,
        &'m mut crate::incr::budget::ScratchLease,
    )> {
        if memory.is_none() {
            let (pool, parent) = self.resolve_fallback_budget(r)?;
            let budget = crate::incr::budget::ViewBudget::new(r.max_memory_bytes, parent.clone());
            let scratch = budget.scratch_lease();
            *memory = Some((pool, parent, budget, scratch));
        }
        let (pool, parent, budget, scratch) = memory.as_mut().expect("set up above");
        Ok((pool, parent, budget, scratch))
    }

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
    /// The published aggregate-error reading for an optional planning warning.
    pub fn view_reading(
        &self,
        name: &str,
        deadline: Option<Instant>,
    ) -> Option<crate::sparql::AggregateErrors> {
        if deadline.is_some_and(|d| Instant::now() >= d) {
            return None;
        }
        let reg = self.reg.try_lock()?;
        reg.view(name).ok()?;
        (reg.host_seq != 0 && reg.host_seq == self.host_seq()).then_some(reg.aggregate_errors)
    }
}

impl ViewSource for Runtime {
    fn read_view(&self, r: &ViewRead<'_>) -> Result<ViewRows> {
        self.read(r)
    }
    fn read_view_with_check(
        &self,
        r: &ViewRead<'_>,
        check: &dyn Fn() -> Result<()>,
        cancellation: &dyn Fn() -> crate::task::Cancellation,
    ) -> Result<ViewRows> {
        self.read_controlled(r, check, cancellation)
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

#[cfg(test)]
thread_local! {
    static OPEN_VIEW_STATE_HOOK: std::cell::RefCell<Option<Box<dyn FnMut()>>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
pub(crate) struct OpenViewStateHookGuard;

#[cfg(test)]
impl Drop for OpenViewStateHookGuard {
    fn drop(&mut self) {
        OPEN_VIEW_STATE_HOOK.with(|h| *h.borrow_mut() = None);
    }
}

#[cfg(test)]
pub(crate) fn set_open_view_state_hook(f: impl FnMut() + 'static) -> OpenViewStateHookGuard {
    OPEN_VIEW_STATE_HOOK.with(|h| *h.borrow_mut() = Some(Box::new(f)));
    OpenViewStateHookGuard
}

#[cfg(test)]
fn fire_open_view_state_hook() {
    let f = OPEN_VIEW_STATE_HOOK.with(|h| h.borrow_mut().take());
    if let Some(mut f) = f {
        f();
        OPEN_VIEW_STATE_HOOK.with(|h| {
            if h.borrow().is_none() {
                *h.borrow_mut() = Some(f);
            }
        });
    }
}

/// A point in an eager fallback rebuild that a unit test observes.
#[cfg(test)]
pub(crate) enum FallbackEvent<'a> {
    /// Evaluation is about to start, after the rebuild's staging was admitted.
    Evaluating,
    /// The evaluation pass returned.
    Built,
    /// The rows were decoded under their scratch credit.
    Decoded(&'a [WeightedRow]),
}

#[cfg(test)]
type FallbackHook = Box<dyn FnMut(FallbackEvent<'_>)>;

#[cfg(test)]
thread_local! {
    static FALLBACK_HOOK: std::cell::RefCell<Option<FallbackHook>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
pub(crate) struct FallbackHookGuard;

#[cfg(test)]
impl Drop for FallbackHookGuard {
    fn drop(&mut self) {
        FALLBACK_HOOK.with(|h| *h.borrow_mut() = None);
    }
}

/// Observe this thread's eager fallback rebuilds until the guard drops.
#[cfg(test)]
pub(crate) fn set_fallback_hook(f: impl FnMut(FallbackEvent<'_>) + 'static) -> FallbackHookGuard {
    FALLBACK_HOOK.with(|h| *h.borrow_mut() = Some(Box::new(f)));
    FallbackHookGuard
}

#[cfg(test)]
fn fire_fallback_hook(event: FallbackEvent<'_>) {
    let f = FALLBACK_HOOK.with(|h| h.borrow_mut().take());
    if let Some(mut f) = f {
        f(event);
        FALLBACK_HOOK.with(|h| {
            if h.borrow().is_none() {
                *h.borrow_mut() = Some(f);
            }
        });
    }
}
