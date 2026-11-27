//! A circuit shared by the views and live queries that ask one question with one
//! access ([`Cell`]), and its state ([`Slot`]).

use super::*;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Status {
    Building,
    Ready,
    /// `code` is `pass-timeout`, `over-budget` or `view-failed`
    Failed {
        code: String,
        message: String,
    },
}

/// What the routes report of a circuit without taking its lock.
#[derive(Clone, Debug)]
pub(super) struct Info {
    pub(super) status: Status,
    /// the runtime mode generation this circuit has been compiled for
    pub(super) mode_gen: u64,
    pub(super) applied: u64,
    pub(super) rows: u64,
    pub(super) bytes: u64,
    pub(super) published_bytes: u64,
    pub(super) passes: u64,
    pub(super) skipped: u64,
    pub(super) pass_time: Duration,
    pub(super) recompute_time: Duration,
    pub(super) columns: Vec<String>,
    pub(super) plan: PlanNode,
    /// why the whole query is evaluated again, when it is
    pub(super) whole: Option<String>,
}

pub(super) type HistRows = Arc<[(Vec<Option<Term>>, i64)]>;

pub(super) struct Hist {
    from: u64,
    to: u64,
    pub(super) rows: HistRows,
    bytes: u64,
}

/// Replay diffs kept for resumed live queries and past-commit reads.
///
/// Stored behind a leaf [`Mutex`] so [`super::super::budget::ViewBudget`] can evict
/// oldest diffs first whenever active circuit state or pass work hits a memory cap,
/// without ever acquiring `Cell::state` or `Runtime::reg`.
#[derive(Default)]
pub(super) struct HistoryBuf {
    pub(super) entries: VecDeque<Hist>,
    pub(super) rows: usize,
    pub(super) bytes: u64,
}

pub(super) fn hist_bytes(rows: &[(Vec<Option<Term>>, i64)]) -> u64 {
    (std::mem::size_of::<Hist>() + std::mem::size_of_val(rows)) as u64
        + rows
            .iter()
            .map(|(row, _)| crate::incr::circuit::decoded_row_bytes(row))
            .sum::<u64>()
}

pub(super) struct Sub {
    pub(super) shared: Weak<Shared>,
    pub(super) budget: u64,
    /// the commit of the subscriber's latest event, which its next diff starts from
    pub(super) last: u64,
}

/// A circuit's state, shared by its views and live queries.
///
/// `circuit` is declared last so `history` (whose bytes are accounted on `circuit`'s
/// budget) drops strictly before `circuit`'s credit returns to the pool.
pub(crate) struct Slot {
    pub(super) query: Arc<Query>,
    pub(super) query_text_bytes: usize,
    /// why the access makes the whole query be evaluated again, when it does
    pub(super) whole: Option<String>,
    pub(super) vars: Arc<[String]>,
    pub(super) history: Arc<Mutex<HistoryBuf>>,
    pub(super) subs: Vec<Sub>,
    /// each attached view's local `memoryBytes` ceiling, by view name
    pub(super) view_limits: FxHashMap<String, u64>,
    /// set by a switch of the inference until the evaluation that follows it: the change
    /// that evaluation publishes leads from one mode to the other, and diffs from before
    /// it must not rewind a view across the switch
    pub(super) rebased: bool,
    /// the runtime mode generation `circuit` has been compiled for
    pub(super) mode_gen: u64,
    /// Completion of the previous circuit's payload and credit release.
    pub(super) retirement: Option<Arc<RetirementCompletion>>,
    pub(super) circuit: Circuit,
}

impl Slot {
    pub(super) fn pending_retirement(&mut self) -> Option<&Arc<RetirementCompletion>> {
        if self
            .retirement
            .as_ref()
            .is_some_and(|completion| completion.released.get().is_some())
        {
            self.retirement = None;
        }
        self.retirement.as_ref()
    }

    /// Leaf reclaim hook for `ViewBudget`: drops oldest replay diffs when active state
    /// needs headroom under the local cap or parent pool. Captures a `Weak` reference so
    /// `Slot::history` remains the sole strong owner and drops strictly before
    /// `Slot::circuit`'s credit returns to the pool.
    pub(super) fn history_reclaim_hook(
        history: &Arc<Mutex<HistoryBuf>>,
    ) -> crate::incr::budget::ReclaimHook {
        let hist = Arc::downgrade(history);
        Arc::new(move |needed: u64| {
            let Some(hist) = hist.upgrade() else {
                return 0;
            };
            let mut h = hist.lock();
            let mut freed = 0u64;
            while freed < needed {
                let Some(old) = h.entries.pop_front() else {
                    break;
                };
                h.rows = h.rows.saturating_sub(old.rows.len());
                h.bytes = h.bytes.saturating_sub(old.bytes);
                freed = freed.saturating_add(old.bytes);
                drop(old);
            }
            if h.entries.is_empty() {
                h.entries = VecDeque::new();
                h.rows = 0;
                h.bytes = 0;
            }
            freed
        })
    }

    pub(super) fn views_count(&self) -> usize {
        self.view_limits.len()
    }

    /// Prune dropped or explicitly closed live subscribers and recompute the effective
    /// local limit when any subscriber left.
    pub(super) fn prune_dead_subs(&mut self) {
        let before = self.subs.len();
        self.subs
            .retain(|s| s.shared.upgrade().is_some_and(|sh| !sh.is_closed()));
        if self.subs.len() != before {
            self.recompute_local_limit();
        }
    }

    /// Recompute the shared circuit's local byte limit from its currently attached views
    /// and live subscribers so detaching a larger consumer lowers the ceiling.
    pub(super) fn recompute_local_limit(&mut self) {
        let max_view = self.view_limits.values().copied().max();
        let max_sub = self
            .subs
            .iter()
            .filter(|s| s.shared.upgrade().is_some_and(|sh| !sh.is_closed()))
            .map(|s| s.budget)
            .max();
        let effective = match (max_view, max_sub) {
            (Some(a), Some(b)) => Some(a.max(b)),
            (a, b) => a.or(b),
        };
        if let Some(limit) = effective {
            self.circuit.set_local_limit(Some(limit));
        }
    }
    /// Why the whole query is evaluated again: protections that depend on the data, or
    /// the dataset's inference.
    pub(super) fn whole_reason(&self, inference: Option<&Inference>) -> Option<String> {
        self.whole
            .clone()
            .or_else(|| inference.map(|i| i.reason().to_string()))
    }

    /// A circuit of the slot's query, access and inference without state.
    pub(super) fn fresh(&self, inference: Option<Arc<Inference>>) -> Result<Circuit> {
        let whole = self.whole_reason(inference.as_deref());
        let mut c = Circuit::new(
            compile(&self.query, whole.as_deref())?,
            self.circuit.access.clone(),
        );
        c.max_memory_bytes = self.circuit.max_memory_bytes;
        c.version_pool = self.circuit.version_pool.clone();
        c.inference = inference;
        c.aggregate_errors = self.circuit.aggregate_errors;
        Ok(c)
    }

    /// The fingerprint of the circuit's checkpoints: the cell's key, the inference its
    /// state was built with, the reading of errors in aggregates its results follow, and
    /// for restricted circuits the active support-aware evidence visibility key.
    pub(super) fn fingerprint(&self, key: u64, ev_key: Option<&str>) -> u64 {
        let mut h = Sha256::new();
        h.update(key.to_le_bytes());
        if let Some(i) = &self.circuit.inference {
            h.update(i.key().as_bytes());
        }
        h.update(b"\0aggregate-errors\0");
        let errors = self.circuit.aggregate_errors.unwrap_or_default();
        h.update(errors.as_str().as_bytes());
        if self
            .circuit
            .access
            .as_ref()
            .is_some_and(|a| !a.reads_everything())
            && let Some(ev) = ev_key
        {
            h.update(b"\0evidence\0");
            h.update(ev.as_bytes());
        }
        u64::from_le_bytes(h.finalize()[..8].try_into().expect("8 bytes"))
    }

    /// The diff of an ordered window's latest pass, decoded incrementally under a
    /// `ScratchLease`. Returns `Ok(None)` when the pass left the window alone.
    pub(super) fn moves_event_budgeted(
        &self,
        from: u64,
        to: u64,
    ) -> Result<Option<(LiveEvent, crate::incr::budget::ScratchLease)>> {
        let Some((removed, added, lease)) = self.circuit.moves_live_budgeted()? else {
            return Ok(None);
        };
        Ok(Some((
            LiveEvent::Diff {
                commit: to,
                from,
                removed,
                added,
            },
            lease,
        )))
    }

    /// Build a snapshot event incrementally under a `ScratchLease`, covering its decoded
    /// rows before returning.
    pub(super) fn event_budgeted(
        &self,
        reset: bool,
    ) -> Result<(LiveEvent, crate::incr::budget::ScratchLease)> {
        let ask = self.circuit.kind == Kind::Ask;
        if ask {
            return Ok((
                LiveEvent::Snapshot {
                    commit: self.circuit.applied,
                    vars: self.vars.clone(),
                    rows: Arc::from(Vec::new()),
                    value: Some(self.circuit.result.total() > 0),
                    reset,
                },
                self.circuit.scratch_lease(),
            ));
        }
        let (rows, lease) = self.circuit.live_rows_budgeted()?;
        Ok((
            LiveEvent::Snapshot {
                commit: self.circuit.applied,
                vars: self.vars.clone(),
                rows,
                value: None,
                reset,
            },
            lease,
        ))
    }

    /// Build an unordered diff event from compact `c` incrementally under a `ScratchLease`.
    pub(super) fn diff_event_budgeted(
        &self,
        from: u64,
        to: u64,
        c: &Changes,
    ) -> Result<Option<(LiveEvent, crate::incr::budget::ScratchLease)>> {
        let Some((removed, added, lease)) = self.circuit.diff_live_budgeted(c)? else {
            return Ok(None);
        };
        Ok(Some((
            LiveEvent::Diff {
                commit: to,
                from,
                removed,
                added,
            },
            lease,
        )))
    }

    /// Whether `snap`'s support-aware evidence visibility configuration or managed
    /// publication generation differs from what this restricted circuit was built with,
    /// or is currently unavailable.
    pub(super) fn evidence_mismatch(&self, snap: &crate::store::Snapshot) -> bool {
        let restricted = self
            .circuit
            .access
            .as_ref()
            .is_some_and(|a| !a.reads_everything());
        if !restricted {
            return false;
        }
        let (snap_gen, snap_ev) = snap.evidence_visibility_stamp();
        self.circuit.managed_generation() != snap_gen
            || self.circuit.evidence_key() != snap_ev.as_deref()
            || snap_ev.as_deref() == Some("unavailable")
    }

    /// Detach a single live subscriber from this slot, update the active local limit, and
    /// return any retired memory leases to drop outside locks.
    pub(crate) fn detach_sub(&mut self, shared: &Arc<Shared>) -> Vec<MemoryLease> {
        self.subs
            .retain(|s| !std::ptr::eq(s.shared.as_ptr(), Arc::as_ptr(shared)));
        self.recompute_local_limit();
        self.circuit.drain_retired()
    }

    /// End all active live subscribers on this slot with an error event and lower
    /// `local_limit` accordingly without failing co-located named views.
    pub(super) fn fail_live_subs(&mut self, e: &Error, deferred: &mut DeferredDrops) {
        let (code, message) = super::round::failure(e);
        for s in self.subs.drain(..) {
            if let Some(sh) = s.shared.upgrade() {
                deferred.queued.extend(sh.replace_leased(
                    LiveEvent::Error {
                        code: code.clone(),
                        message: message.clone(),
                    },
                    None,
                    None,
                ));
            }
        }
        self.recompute_local_limit();
    }

    pub(super) fn detach_event_lease(
        &self,
        lease: &mut crate::incr::budget::ScratchLease,
    ) -> Option<Arc<crate::incr::live::QueuedLease>> {
        crate::incr::live::QueuedLease::new(lease.bytes(), self.circuit.detach_scratch_lease(lease))
    }

    /// The whole result as a `reset` event for the subscriber of `shared`, built under
    /// budget admission. Returns the event and retired credit to release after unlocking.
    pub(crate) fn resync_budgeted(
        &mut self,
        shared: &Arc<Shared>,
    ) -> (crate::incr::live::QueuedEvent, Vec<MemoryLease>) {
        if self
            .circuit
            .snap
            .as_ref()
            .is_some_and(|s| self.evidence_mismatch(s))
        {
            self.subs
                .retain(|s| !std::ptr::eq(s.shared.as_ptr(), Arc::as_ptr(shared)));
            self.recompute_local_limit();
            let retired = self.circuit.drain_retired();
            return (
                crate::incr::live::QueuedEvent::new(
                    LiveEvent::Error {
                        code: "access-changed".into(),
                        message: "the evidence configuration changed; reconnect to read under the new rules".into(),
                    },
                    None,
                ),
                retired,
            );
        }
        let applied = self.circuit.applied;
        let event = self.event_budgeted(true).and_then(|(event, lease)| {
            if lease.bytes() <= shared.byte_limit() {
                Ok((event, lease))
            } else {
                Err(Error::BudgetExceeded(crate::error::Budget {
                    kind: crate::error::BudgetKind::Memory,
                    limit: shared.byte_limit(),
                    requested: lease.bytes(),
                }))
            }
        });
        match event {
            Ok((ev, mut lease)) => {
                for s in &mut self.subs {
                    if std::ptr::eq(s.shared.as_ptr(), Arc::as_ptr(shared)) {
                        s.last = applied;
                    }
                }
                let detached = self.detach_event_lease(&mut lease);
                let stamp = Some(self.circuit.evidence_stamp());
                let retired = self.circuit.drain_retired();
                (
                    crate::incr::live::QueuedEvent::stamped(ev, detached, stamp),
                    retired,
                )
            }
            Err(e) => {
                self.subs
                    .retain(|s| !std::ptr::eq(s.shared.as_ptr(), Arc::as_ptr(shared)));
                self.recompute_local_limit();
                let retired = self.circuit.drain_retired();
                let (code, message) = super::round::failure(&e);
                (
                    crate::incr::live::QueuedEvent::new(LiveEvent::Error { code, message }, None),
                    retired,
                )
            }
        }
    }

    /// Evaluate the query at `snap` from scratch. This is the first build, or a rebuild
    /// whose change from the old result is returned with its scratch lease for the live
    /// queries.
    pub(super) fn evaluate(
        &mut self,
        snap: Arc<crate::store::Snapshot>,
        control: &Control,
    ) -> Result<Option<(Changes, crate::incr::budget::ScratchLease)>> {
        if self.circuit.snap.is_some() {
            self.circuit
                .reset_charged(&DeltaSet::at(snap), control)
                .map(Some)
        } else {
            self.circuit.build(snap, control).map(|()| None)
        }
    }

    /// Forget the diffs kept for resumed live queries and reads at past commits.
    pub(super) fn clear_history(&mut self) {
        {
            let mut h = self.history.lock();
            h.entries = VecDeque::new();
            h.rows = 0;
            h.bytes = 0;
        }
        self.circuit.set_external_best_effort(0);
    }

    pub(super) fn alive(&mut self) -> bool {
        self.prune_dead_subs();
        if self.subs.is_empty() && self.view_limits.is_empty() {
            self.clear_history();
        }
        !self.subs.is_empty() || !self.view_limits.is_empty()
    }

    pub(super) fn remember(
        &mut self,
        from: u64,
        to: u64,
        rows: Arc<[(Vec<Option<Term>>, i64)]>,
        max: usize,
    ) {
        let mut empty = crate::incr::budget::ScratchLease::default();
        self.remember_charged(from, to, rows, max, &mut empty);
    }

    /// Store `rows` in replay history while atomically handing off any pre-admitted
    /// scratch `lease` into `external_bytes`, avoiding any uncharged gap or 2× peak.
    pub(super) fn remember_charged(
        &mut self,
        from: u64,
        to: u64,
        rows: Arc<[(Vec<Option<Term>>, i64)]>,
        max: usize,
        lease: &mut crate::incr::budget::ScratchLease,
    ) {
        let max = if self.subs.is_empty() {
            if self.view_limits.is_empty() {
                return;
            }
            VIEW_HISTORY_COMMITS
        } else {
            max
        };
        if rows.is_empty() {
            let mut h = self.history.lock();
            if let Some(last) = h.entries.back_mut()
                && last.rows.is_empty()
                && last.to == from
            {
                last.to = to;
                return;
            }
        }
        let bytes = hist_bytes(&rows);
        let new_rows = rows.len();
        {
            let mut h = self.history.lock();
            while h.entries.len() >= max.max(1) || h.rows.saturating_add(new_rows) > HISTORY_ROWS {
                match h.entries.pop_front() {
                    Some(old) => {
                        h.rows = h.rows.saturating_sub(old.rows.len());
                        h.bytes = h.bytes.saturating_sub(old.bytes);
                    }
                    None => break,
                }
            }
        }
        // Pre-admit the incoming diff before storing it in history, evicting oldest
        // entries first when memory is tight so replay history never escapes the pool
        // or breaks an otherwise healthy view.
        loop {
            let cur_bytes = self.history.lock().bytes;
            let next_bytes = cur_bytes.saturating_add(bytes);
            if self
                .circuit
                .commit_external_from_scratch(next_bytes, lease)
                .is_ok()
            {
                let mut h = self.history.lock();
                h.rows = h.rows.saturating_add(new_rows);
                h.bytes = h.bytes.saturating_add(bytes);
                h.entries.push_back(Hist {
                    from,
                    to,
                    rows,
                    bytes,
                });
                return;
            }
            let popped = {
                let mut h = self.history.lock();
                let popped = h.entries.pop_front();
                if let Some(ref old) = popped {
                    h.rows = h.rows.saturating_sub(old.rows.len());
                    h.bytes = h.bytes.saturating_sub(old.bytes);
                }
                if h.entries.is_empty() {
                    h.entries = VecDeque::new();
                    h.rows = 0;
                    h.bytes = 0;
                }
                popped
            };
            if popped.is_none() {
                drop(rows);
                self.circuit.set_external_best_effort(0);
                return;
            }
        }
    }

    /// Collect the non-empty history diff slices from `after` to the circuit's commit,
    /// admitting the output vector outside the history lock and polling `check`.
    pub(super) fn history_rows_since(
        &self,
        after: u64,
        check: &dyn Fn() -> Result<()>,
        admit: &mut dyn FnMut(u64) -> Result<()>,
    ) -> Result<Option<Vec<HistRows>>> {
        let count = {
            let h = self.history.lock();
            let Some(start) = h.entries.iter().position(|entry| {
                entry.from == after
                    || (entry.rows.is_empty() && entry.from <= after && after < entry.to)
            }) else {
                return Ok(None);
            };
            let mut at = after;
            let mut count = 0usize;
            for entry in h.entries.iter().skip(start) {
                if entry.from > at {
                    return Ok(None);
                }
                if !entry.rows.is_empty() {
                    count += 1;
                }
                at = entry.to;
            }
            if at != self.circuit.applied {
                return Ok(None);
            }
            count
        };
        admit((count * std::mem::size_of::<HistRows>()) as u64)?;
        let h = self.history.lock();
        let Some(start) = h.entries.iter().position(|entry| {
            entry.from == after
                || (entry.rows.is_empty() && entry.from <= after && after < entry.to)
        }) else {
            return Ok(None);
        };
        let mut at = after;
        let mut parts = Vec::with_capacity(count);
        for (i, entry) in h.entries.iter().skip(start).enumerate() {
            if i.is_multiple_of(256) {
                check()?;
            }
            if entry.from > at {
                return Ok(None);
            }
            if !entry.rows.is_empty() {
                parts.push(entry.rows.clone());
            }
            at = entry.to;
        }
        Ok((at == self.circuit.applied).then_some(parts))
    }

    /// Admit the history copies and netting buffers before materializing a resumed diff.
    #[allow(clippy::type_complexity)]
    pub(super) fn history_since_budgeted(
        &self,
        after: u64,
    ) -> Option<(
        Vec<(Vec<Option<Term>>, i64)>,
        crate::incr::budget::ScratchLease,
    )> {
        let mut lease = self.circuit.scratch_lease();
        let (bytes, rows) = {
            let history = self.history.lock();
            let start = history.entries.iter().position(|entry| {
                entry.from == after
                    || (entry.rows.is_empty() && entry.from <= after && after < entry.to)
            })?;
            history
                .entries
                .iter()
                .skip(start)
                .fold((0u64, 0usize), |(bytes, rows), h| {
                    (
                        bytes.saturating_add(h.bytes),
                        rows.saturating_add(h.rows.len()),
                    )
                })
        };
        // Netting keeps a map of row keys, a copy of each distinct row in its input
        // order, and an output vector. Admission may evict history, so history_since
        // checks continuity again after the reservation.
        lease
            .try_reserve(bytes.saturating_mul(2).saturating_add(rows as u64 * 96))
            .ok()?;
        let change = self.history_since(after)?;
        Some((change, lease))
    }

    /// The net change from commit `after` to the circuit's commit from the history.
    pub(super) fn history_since(&self, after: u64) -> Option<Vec<(Vec<Option<Term>>, i64)>> {
        let h = self.history.lock();
        let start = h.entries.iter().position(|entry| {
            entry.from == after
                || (entry.rows.is_empty() && entry.from <= after && after < entry.to)
        })?;
        let mut at = after;
        let mut parts = Vec::new();
        for entry in h.entries.iter().skip(start) {
            if entry.from > at {
                return None;
            }
            parts.push(&entry.rows);
            at = entry.to;
        }
        (at == self.circuit.applied)
            .then(|| net(parts.into_iter().flat_map(|rows| rows.iter().cloned())))
    }
}

/// The net multiset change of term rows.
pub(super) fn net(
    rows: impl Iterator<Item = (Vec<Option<Term>>, i64)>,
) -> Vec<(Vec<Option<Term>>, i64)> {
    let mut m: FxHashMap<Vec<Option<Term>>, i64> = FxHashMap::default();
    let mut order = Vec::new();
    for (r, w) in rows {
        match m.get_mut(&r) {
            Some(n) => *n += w,
            None => {
                order.push(r.clone());
                m.insert(r, w);
            }
        }
    }
    order
        .into_iter()
        .filter_map(|r| {
            let w = m[&r];
            (w != 0).then_some((r, w))
        })
        .collect()
}

/// A circuit with what the routes read of it.
pub(crate) struct Cell {
    pub(super) key: u64,
    pub(crate) state: Mutex<Slot>,
    pub(super) info: Mutex<Info>,
    /// live queries joining the cell, which keep it from collection until they are
    /// among its subscribers
    pub(super) pending: AtomicUsize,
}

impl Cell {
    /// Detach view `name` from the circuit and recompute its effective local limit,
    /// returning the number of views still attached.
    pub(super) fn release_view(&self, name: &str) -> usize {
        let mut st = self.state.lock();
        st.view_limits.remove(name);
        st.recompute_local_limit();
        if st.view_limits.is_empty() {
            st.circuit.set_version_pool(None);
        }
        st.view_limits.len()
    }
}

/// A live query's claim on a cell until it subscribes.
pub(super) struct Joining<'a>(pub(super) &'a Cell);

impl Drop for Joining<'_> {
    fn drop(&mut self) {
        self.0.pending.fetch_sub(1, Ordering::AcqRel);
    }
}

impl Info {
    pub(super) fn status_ready(&self) -> bool {
        self.status == Status::Ready
    }
}

pub(super) fn diff_event(
    from: u64,
    to: u64,
    rows: impl Iterator<Item = (Vec<Option<Term>>, i64)>,
) -> LiveEvent {
    let (mut removed, mut added) = (Vec::new(), Vec::new());
    for (terms, w) in rows {
        let r = LiveRow {
            terms,
            count: w.unsigned_abs(),
            position: None,
        };
        if w < 0 {
            removed.push(r);
        } else if w > 0 {
            added.push(r);
        }
    }
    LiveEvent::Diff {
        commit: to,
        from,
        removed: removed.into(),
        added: added.into(),
    }
}

#[cfg(test)]
mod drop_order_tests {
    use super::*;
    use crate::sparql::parse_query;
    use crate::store::StoreOptions;
    use sparkles_budget::MemoryDomain;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    /// Proves using the real `Slot` and `Slot::history_reclaim_hook` that dropping a
    /// populated `Slot` destroys `HistoryBuf` strictly before `Slot::circuit`'s credit
    /// returns to the parent.
    #[test]
    fn real_slot_drop_frees_history_buf_before_parent_release() {
        let store = Store::in_memory(StoreOptions::default());
        crate::sparql::update::update(
            &store,
            "INSERT DATA { <urn:a> <urn:p> \"seed\" }",
            &crate::sparql::QueryOptions::default(),
        )
        .unwrap();

        let history = Arc::new(Mutex::new(HistoryBuf::default()));
        let weak_hist = Arc::downgrade(&history);
        let saw_dead = Arc::new(AtomicBool::new(false));
        let releases = Arc::new(AtomicUsize::new(0));
        let saw_dead_cb = saw_dead.clone();
        let releases_cb = releases.clone();
        let weak_cb = weak_hist.clone();
        let parent = MemoryDomain::unpooled().parent(
            |_| Ok(()),
            move |bytes| {
                if bytes > 0 {
                    releases_cb.fetch_add(1, Ordering::SeqCst);
                    assert_eq!(
                        weak_cb.strong_count(),
                        0,
                        "Slot::history must be dropped before Slot::circuit returns parent credit"
                    );
                    saw_dead_cb.store(true, Ordering::SeqCst);
                }
            },
        );

        let q = parse_query("SELECT ?s ?o WHERE { ?s <urn:p> ?o }", None, &[]).unwrap();
        let mut circuit = Circuit::new(compile(&q, None).unwrap(), None);
        circuit.set_pool(None, Some(parent));
        circuit.set_reclaim_external(Some(Slot::history_reclaim_hook(&history)));
        circuit.build(store.snapshot(), &Control::none()).unwrap();
        let vars: Arc<[String]> = circuit.columns().into();
        let mut view_limits = FxHashMap::default();
        view_limits.insert("v".to_string(), 1 << 20);
        let mut slot = Slot {
            query: Arc::new(q),
            query_text_bytes: 0,
            whole: None,
            vars,
            history,
            subs: Vec::new(),
            view_limits,
            rebased: false,
            mode_gen: 0,
            retirement: None,
            circuit,
        };
        let rows: Arc<[(Vec<Option<Term>>, i64)]> = Arc::from(vec![(
            vec![
                Some(Term::NamedNode(oxrdf::NamedNode::new_unchecked("urn:a"))),
                Some(Term::Literal(oxrdf::Literal::new_simple_literal("v"))),
            ],
            1,
        )]);
        slot.remember(0, 1, rows, VIEW_HISTORY_COMMITS);
        assert_eq!(weak_hist.strong_count(), 1);
        drop(slot);
        assert!(releases.load(Ordering::SeqCst) >= 1);
        assert!(saw_dead.load(Ordering::SeqCst));
    }
}
