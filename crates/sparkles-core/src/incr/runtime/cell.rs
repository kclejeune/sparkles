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
}

pub(super) struct Sub {
    pub(super) shared: Weak<Shared>,
    pub(super) budget: u64,
    /// the commit of the subscriber's latest event, which its next diff starts from
    pub(super) last: u64,
}

/// A circuit's state, shared by its views and live queries.
pub(crate) struct Slot {
    pub(super) circuit: Circuit,
    pub(super) query: Query,
    /// why the access makes the whole query be evaluated again, when it does
    pub(super) whole: Option<String>,
    pub(super) vars: Arc<[String]>,
    pub(super) history: VecDeque<Hist>,
    pub(super) history_rows: usize,
    pub(super) subs: Vec<Sub>,
    /// the views attached to the circuit
    pub(super) views: usize,
    /// set by a switch of the inference until the evaluation that follows it: the change
    /// that evaluation publishes leads from one mode to the other, and diffs from before
    /// it must not rewind a view across the switch
    pub(super) rebased: bool,
}

impl Slot {
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
        Ok(c)
    }

    /// The fingerprint of the circuit's checkpoints: the cell's key, and the inference
    /// its state was built with.
    pub(super) fn fingerprint(&self, key: u64) -> u64 {
        match &self.circuit.inference {
            None => key,
            Some(i) => {
                let mut h = Sha256::new();
                h.update(key.to_le_bytes());
                h.update(i.key().as_bytes());
                u64::from_le_bytes(h.finalize()[..8].try_into().expect("8 bytes"))
            }
        }
    }

    pub(super) fn live_rows(&self, rows: Vec<(Vec<Option<Term>>, i64)>) -> Vec<LiveRow> {
        rows.into_iter()
            .map(|(terms, w)| LiveRow {
                terms,
                count: w.unsigned_abs(),
            })
            .collect()
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
    /// whose change from the old result is returned for the live queries.
    pub(super) fn evaluate(
        &mut self,
        snap: Arc<crate::store::Snapshot>,
        control: &Control,
    ) -> Result<Option<Changes>> {
        if self.circuit.snap.is_some() {
            self.circuit.reset(&DeltaSet::at(snap), control).map(Some)
        } else {
            self.circuit.build(snap, control).map(|()| None)
        }
    }

    /// Forget the diffs kept for resumed live queries and reads at past commits.
    pub(super) fn clear_history(&mut self) {
        self.history.clear();
        self.history_rows = 0;
    }

    pub(super) fn alive(&mut self) -> bool {
        self.subs.retain(|s| s.shared.strong_count() > 0);
        if self.subs.is_empty() && self.views == 0 {
            self.clear_history();
        }
        !self.subs.is_empty() || self.views > 0
    }

    pub(super) fn remember(
        &mut self,
        from: u64,
        to: u64,
        rows: Arc<[(Vec<Option<Term>>, i64)]>,
        max: usize,
    ) {
        let max = if self.subs.is_empty() {
            if self.views == 0 {
                return;
            }
            VIEW_HISTORY_COMMITS
        } else {
            max
        };
        if rows.is_empty()
            && let Some(last) = self.history.back_mut()
            && last.rows.is_empty()
            && last.to == from
        {
            last.to = to;
            return;
        }
        self.history_rows += rows.len();
        self.history.push_back(Hist { from, to, rows });
        while self.history.len() > max.max(1) || self.history_rows > HISTORY_ROWS {
            match self.history.pop_front() {
                Some(h) => self.history_rows -= h.rows.len(),
                None => break,
            }
        }
    }

    /// The net change from commit `after` to the circuit's commit from the history.
    pub(super) fn history_since(&self, after: u64) -> Option<Vec<(Vec<Option<Term>>, i64)>> {
        let start = self.history.iter().position(|h| {
            h.from == after || (h.rows.is_empty() && h.from <= after && after < h.to)
        })?;
        let mut at = after;
        let mut parts = Vec::new();
        for h in self.history.iter().skip(start) {
            if h.from > at {
                return None;
            }
            parts.push(&h.rows);
            at = h.to;
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
    /// Detach one view from the circuit, which leaves the views still attached.
    pub(super) fn release_view(&self) -> usize {
        let mut st = self.state.lock();
        st.views = st.views.saturating_sub(1);
        st.views
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
