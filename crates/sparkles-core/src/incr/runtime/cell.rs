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
    pub(super) applied: u64,
    pub(super) rows: u64,
    pub(super) bytes: u64,
    pub(super) passes: u64,
    pub(super) skipped: u64,
    pub(super) pass_time: Duration,
    pub(super) recompute_time: Duration,
    pub(super) columns: Vec<String>,
    pub(super) plan: PlanNode,
    /// why the whole query is evaluated again, when it is
    pub(super) whole: Option<String>,
}

pub(super) struct Hist {
    from: u64,
    to: u64,
    rows: Arc<[(Vec<Option<Term>>, i64)]>,
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

fn term_bytes(t: &Term) -> u64 {
    match t {
        Term::NamedNode(n) => 32 + n.as_str().len() as u64,
        Term::BlankNode(b) => 32 + b.as_str().len() as u64,
        Term::Literal(l) => {
            48 + l.value().len() as u64
                + l.language().map_or(0, |s| s.len() as u64)
                + l.datatype().as_str().len() as u64
        }
        Term::Triple(tr) => {
            64 + term_bytes(&Term::from(tr.subject.clone()))
                + term_bytes(&Term::NamedNode(tr.predicate.clone()))
                + term_bytes(&tr.object)
        }
    }
}

pub(super) fn hist_bytes(rows: &[(Vec<Option<Term>>, i64)]) -> u64 {
    let mut total = std::mem::size_of::<Hist>() as u64;
    for (row, _) in rows {
        total = total.saturating_add(32);
        for cell in row {
            total = total.saturating_add(match cell {
                Some(t) => term_bytes(t),
                None => 8,
            });
        }
    }
    total
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
    pub(super) query: Query,
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
    pub(super) circuit: Circuit,
}

impl Slot {
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
        c.inference = inference;
        c.aggregate_errors = self.circuit.aggregate_errors;
        Ok(c)
    }

    /// The fingerprint of the circuit's checkpoints: the cell's key, the inference its
    /// state was built with and the reading of errors in aggregates its results follow.
    pub(super) fn fingerprint(&self, key: u64) -> u64 {
        let mut h = Sha256::new();
        h.update(key.to_le_bytes());
        if let Some(i) = &self.circuit.inference {
            h.update(i.key().as_bytes());
        }
        h.update(b"\0aggregate-errors\0");
        let errors = self.circuit.aggregate_errors.unwrap_or_default();
        h.update(errors.as_str().as_bytes());
        u64::from_le_bytes(h.finalize()[..8].try_into().expect("8 bytes"))
    }

    /// The rows of a snapshot. An ordered window's rows come in order with their
    /// positions.
    pub(super) fn live_rows(&self, rows: Vec<(Vec<Option<Term>>, i64)>) -> Vec<LiveRow> {
        let ordered = self.circuit.ordered();
        let mut at = 0u64;
        rows.into_iter()
            .map(|(terms, w)| {
                let count = w.unsigned_abs();
                let position = ordered.then_some(at);
                at += count;
                LiveRow {
                    terms,
                    count,
                    position,
                }
            })
            .collect()
    }

    /// The diff of an ordered window's latest pass, `None` when the pass left the
    /// window alone.
    pub(super) fn moves_event(&self, from: u64, to: u64) -> Option<LiveEvent> {
        let (removed, added) = self.circuit.moves()?;
        if removed.is_empty() && added.is_empty() {
            return None;
        }
        let rows = |v: Vec<(u64, Vec<Option<Term>>, u64)>| -> Arc<[LiveRow]> {
            v.into_iter()
                .map(|(position, terms, count)| LiveRow {
                    terms,
                    count,
                    position: Some(position),
                })
                .collect()
        };
        Some(LiveEvent::Diff {
            commit: to,
            from,
            removed: rows(removed),
            added: rows(added),
        })
    }

    pub(super) fn event(&self, reset: bool) -> LiveEvent {
        let ask = self.circuit.kind == Kind::Ask;
        LiveEvent::Snapshot {
            commit: self.circuit.applied,
            vars: self.vars.clone(),
            rows: if ask {
                Arc::from(Vec::new())
            } else {
                self.live_rows(self.circuit.rows()).into()
            },
            value: ask.then(|| self.circuit.result.total() > 0),
            reset,
        }
    }

    /// The whole result as a `reset` event for the subscriber of `shared`, whose next
    /// diff then starts from the circuit's commit.
    pub(crate) fn resync(&mut self, shared: &Arc<Shared>) -> LiveEvent {
        let applied = self.circuit.applied;
        for s in &mut self.subs {
            if std::ptr::eq(s.shared.as_ptr(), Arc::as_ptr(shared)) {
                s.last = applied;
            }
        }
        self.event(true)
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
        if at != self.circuit.applied {
            return None;
        }
        Some(net(parts.iter().flat_map(|r| r.iter().cloned())))
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
            query: q,
            whole: None,
            vars,
            history,
            subs: Vec::new(),
            view_limits,
            rebased: false,
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
