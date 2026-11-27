//! Attaching views to circuits, recovering their checkpoints, and compiling every
//! circuit again when the dataset's inference changes.

use super::*;

/// Read `path` from an already-opened file descriptor, bounding both the `ScratchLease`
/// reservation and the actual bytes read to the open file's metadata length so concurrent
/// file growth cannot allocate past the reservation.
pub(super) fn read_bounded_file(
    path: &std::path::Path,
    lease: &mut super::super::budget::ScratchLease,
) -> std::result::Result<Option<Vec<u8>>, Error> {
    use std::io::Read;
    let Ok(mut file) = std::fs::File::open(path) else {
        return Ok(None);
    };
    let Ok(meta) = file.metadata() else {
        return Ok(None);
    };
    let len = meta.len();
    lease.try_reserve(len)?;
    let Ok(cap) = usize::try_from(len) else {
        return Ok(None);
    };
    let mut bytes = Vec::with_capacity(cap);
    if file.by_ref().take(len).read_to_end(&mut bytes).is_err() {
        return Ok(None);
    }
    let mut extra = [0u8; 1];
    match file.read(&mut extra) {
        Ok(0) => Ok(Some(bytes)),
        _ => Ok(None),
    }
}

impl Runtime {
    /// The cell of `q` read with `access`, created (and not built) when absent.
    pub(super) fn cell_for(
        &self,
        reg: &mut Registry,
        q: Query,
        text: &str,
        access: Option<Arc<GraphAccess>>,
        max_memory_bytes: Option<u64>,
    ) -> Result<(Arc<Cell>, bool)> {
        let whole = whole_reason(access.as_ref()).map(str::to_string);
        let key = key_of(&q, text, access.as_ref(), whole.as_deref());
        if let Some(c) = reg.cells.get(&key) {
            return Ok((c.clone(), false));
        }
        let inference = reg.inference.clone();
        let mode = whole
            .clone()
            .or_else(|| inference.as_ref().map(|i| i.reason().to_string()));
        // Compile the same allocation that a rebuild will plan from.
        let q = Arc::new(q);
        let compiled = compile(&q, mode.as_deref())?;
        let history = Arc::new(Mutex::new(HistoryBuf::default()));
        let mut circuit = Circuit::new(compiled, access);
        circuit.set_runtime_pool(self.pool.clone(), self.circuit_parent());
        circuit.set_local_limit(max_memory_bytes);
        circuit.set_reclaim_external(Some(Slot::history_reclaim_hook(&history)));
        circuit.inference = inference;
        circuit.aggregate_errors = Some(reg.aggregate_errors);
        let info = Info {
            status: Status::Building,
            mode_gen: reg.mode_gen,
            applied: 0,
            rows: 0,
            bytes: 0,
            published_bytes: 0,
            passes: 0,
            skipped: 0,
            pass_time: Duration::ZERO,
            recompute_time: Duration::ZERO,
            columns: circuit.columns(),
            plan: circuit.plan(),
            whole: mode,
        };
        let vars: Arc<[String]> = circuit.columns().into();
        let cell = Arc::new(Cell {
            key,
            state: Mutex::new(Slot {
                query: q,
                query_text_bytes: text.len(),
                whole,
                vars,
                history,
                subs: Vec::new(),
                view_limits: FxHashMap::default(),
                rebased: false,
                mode_gen: reg.mode_gen,
                retirement: None,
                circuit,
            }),
            info: Mutex::new(info),
            pending: AtomicUsize::new(0),
        });
        reg.cells.insert(key, cell.clone());
        Ok((cell, true))
    }

    /// Attach view `name` to its circuit, loading its newest checkpoint into a new one.
    pub(super) fn attach(
        &self,
        store: &Store,
        reg: &mut Registry,
        name: &str,
        retired: &mut Vec<MemoryLease>,
    ) -> Result<()> {
        self.attach_persisting(store, reg, name, retired, |_| Ok(()))
    }

    pub(super) fn attach_persisting(
        &self,
        store: &Store,
        reg: &mut Registry,
        name: &str,
        retired: &mut Vec<MemoryLease>,
        persist: impl FnOnce(&ViewDef) -> Result<()>,
    ) -> Result<()> {
        let host = self.host()?;
        let Some(entry) = reg.views.get(name) else {
            return Ok(());
        };
        if entry.cell.is_some() || entry.suspended.is_some() {
            return Ok(());
        }
        // checked again on every attach, since a change of the host's epoch or a
        // rebuild clears the error
        if let Some(e) = unsupported(&entry.def) {
            persist(&entry.def)?;
            let entry = reg.views.get_mut(name).expect("checked above");
            entry.error = Some(e);
            return Ok(());
        }
        let def = entry.def.clone();
        let text = Self::view_text(host.as_ref(), &def)?;
        let q = self.parse(host.as_ref(), &text)?;
        let access = host.access(def.principal.as_deref())?;
        let (cell, new) = self.cell_for(reg, q, &text, access.clone(), Some(def.memory_bytes))?;
        let attached = {
            let mut st = cell.state.lock();
            let was_empty = st.view_limits.is_empty();
            let previous = st.view_limits.insert(name.to_string(), def.memory_bytes);
            st.recompute_local_limit();
            let result = if was_empty {
                st.circuit
                    .attach_version_pool(self.published_views.clone(), || persist(&def))
            } else {
                persist(&def)
            };
            if result.is_err() {
                if let Some(previous) = previous {
                    st.view_limits.insert(name.to_string(), previous);
                } else {
                    st.view_limits.remove(name);
                }
                st.recompute_local_limit();
            } else if was_empty {
                let mut info = cell.info.lock();
                info.bytes = st.circuit.bytes();
                info.published_bytes = st.circuit.published_bytes();
            }
            retired.extend(st.circuit.drain_retired());
            result
        };
        if let Err(error) = attached {
            if new {
                reg.cells.remove(&cell.key);
            }
            return Err(error);
        }
        if new {
            self.ensure_subscription(store);
            if let Some(root) = &self.root {
                self.recover(
                    store,
                    &cell,
                    &views::dir(root, name).join(BRANCH),
                    name,
                    retired,
                );
            }
        }
        let entry = reg.views.get_mut(name).expect("checked above");
        entry.cell = Some(cell);
        entry.access = access;
        entry.error = None;
        Ok(())
    }

    /// Load the newest checkpoint of a view that this release reads into `cell`.
    pub(super) fn recover(
        &self,
        store: &Store,
        cell: &Cell,
        dir: &std::path::Path,
        name: &str,
        retired: &mut Vec<MemoryLease>,
    ) {
        let snap = store.snapshot();
        let kind = persist::view_kind(cell.state.lock().circuit.has_fixpoint());
        for (commit, path) in persist::list(dir) {
            let mut file_lease = super::super::budget::ScratchLease::new_pool_only(Some(
                cell.state.lock().circuit.budget_handle(),
            ));
            let bytes = match read_bounded_file(&path, &mut file_lease) {
                Ok(Some(bytes)) => bytes,
                Ok(None) => {
                    drop(file_lease);
                    continue;
                }
                Err(e) => {
                    tracing::warn!(
                        target: "sparkles::incr::runtime",
                        "view {name}: checkpoint {}: {e}; the view is built again",
                        path.display()
                    );
                    drop(file_lease);
                    continue;
                }
            };
            let (h, body) = match persist::decode_as(kind, &bytes) {
                Ok(x) => x,
                Err(e) => {
                    tracing::warn!(
                        target: "sparkles::incr::runtime",
                        "view {name}: checkpoint {}: {e}; it is left in place",
                        path.display()
                    );
                    drop(bytes);
                    drop(file_lease);
                    continue;
                }
            };
            let mut loaded = false;
            {
                let mut st = cell.state.lock();
                let restricted = st
                    .circuit
                    .access
                    .as_ref()
                    .is_some_and(|a| !a.reads_everything());
                let (snap_gen, snap_ev) = if restricted {
                    snap.evidence_visibility_stamp()
                } else {
                    (snap.managed_generation(), None)
                };
                let commit_ok = if restricted && snap_ev.is_some() {
                    commit == snap.commit
                } else {
                    commit <= snap.commit
                };
                if snap_ev.as_deref() != Some("unavailable")
                    && h.dataset == self.dataset
                    && h.fingerprint == st.fingerprint(cell.key, snap_ev.as_deref())
                    && commit_ok
                {
                    match st
                        .circuit
                        .decode_with_input_charged(body, &snap)
                        .and_then(|()| st.circuit.set_applied(h.commit))
                    {
                        Ok(()) => {
                            st.circuit
                                .set_evidence_stamp(crate::incr::live::EvidenceStamp {
                                    managed_gen: snap_gen,
                                    key: snap_ev,
                                });
                            self.seen.fetch_max(h.commit, Ordering::AcqRel);
                            self.counters.loaded.fetch_add(1, Ordering::Relaxed);
                            let mut info = cell.info.lock();
                            info.status = Status::Ready;
                            info.applied = h.commit;
                            info.rows = st.circuit.result.total().max(0) as u64;
                            info.bytes = st.circuit.bytes();
                            info.published_bytes = st.circuit.published_bytes();
                            tracing::info!(
                                target: "sparkles::incr::runtime",
                                "view {name}: loaded its state at commit {}; {} commits to apply",
                                h.commit,
                                snap.commit - h.commit
                            );
                            loaded = true;
                        }
                        Err(e) => {
                            tracing::warn!(
                                target: "sparkles::incr::runtime",
                                "view {name}: checkpoint {}: {e}; the view is built again",
                                path.display()
                            );
                            st.circuit.clear_state();
                        }
                    }
                }
            }
            // Drop the raw checkpoint buffer strictly before releasing `file_lease`.
            // Keep retired leases in `BudgetState::retired` across retry iterations so
            // subsequent checkpoints can recycle freed credit without hitting `QueryPool`.
            drop(bytes);
            drop(file_lease);
            if loaded {
                break;
            }
        }
        retired.extend(cell.state.lock().circuit.drain_retired());
    }

    pub(super) fn attach_pending(&self, store: &Store) {
        if !self.has_host() {
            return;
        }
        self.refresh_mode();
        let mut deferred = DeferredDrops::default();
        {
            let mut reg = self.reg.lock();
            let (_, Some(host)) = self.current_host() else {
                return;
            };
            let epoch = host.epoch();
            if self.host_epoch.swap(epoch, Ordering::AcqRel) != epoch {
                // grants may have changed: resolve every view's principal again
                for (name, e) in reg.views.iter_mut() {
                    e.error = None;
                    if let Some(c) = e.cell.take() {
                        c.release_view(name, &mut deferred.retired);
                        deferred.cells.push(c);
                    }
                }
            }
            let pending: Vec<String> = reg
                .views
                .iter()
                .filter(|(_, e)| e.cell.is_none() && e.suspended.is_none() && e.error.is_none())
                .map(|(n, _)| n.clone())
                .collect();
            for name in pending {
                if let Err(e) = self.attach(store, &mut reg, &name, &mut deferred.retired) {
                    tracing::warn!(target: "sparkles::incr::runtime", "view {name}: {e}");
                    if let Some(entry) = reg.views.get_mut(&name) {
                        entry.error = Some(e.to_string());
                    }
                }
            }
        }
        drop(deferred);
    }

    /// Sample the host outside runtime locks and publish the result in observation order.
    pub(super) fn refresh_mode(&self) {
        loop {
            #[cfg(test)]
            {
                let hook = self.before_mode_host_hook.lock().clone();
                if let Some(hook) = hook {
                    hook();
                }
            }
            let (seq, Some(host)) = self.current_host() else {
                return;
            };
            let sample = self.next_mode_sample.fetch_add(1, Ordering::Relaxed) + 1;
            let now = host.inference();
            let errors = host.aggregate_errors();
            let mut reg = self.reg.lock();
            if self.host_seq() != seq {
                continue;
            }
            if (seq, sample) <= (reg.host_seq, reg.mode_sample) {
                return;
            }
            reg.mode_sample = sample;
            reg.host_seq = seq;
            if !Inference::same(reg.inference.as_deref(), now.as_ref())
                || errors != reg.aggregate_errors
            {
                let now = now.map(Arc::new);
                tracing::info!(
                    target: "sparkles::incr::runtime",
                    "the dataset's inference ({}) or aggregate errors ({errors}) changed: {} circuits evaluate again",
                    now.as_ref().map_or("none", |i| i.reason()),
                    reg.cells.len()
                );
                reg.mode_gen += 1;
                reg.inference = now;
                reg.aggregate_errors = errors;
                self.mode_gen.store(reg.mode_gen, Ordering::Release);
                let boundary = self.seen.load(Ordering::Acquire);
                self.switched.fetch_max(boundary, Ordering::AcqRel);
            }
            return;
        }
    }

    /// Whether a newer mode generation has been recorded, a replacement host was
    /// installed, or the installed host reports a different inference or aggregate-error
    /// setting than `target`.
    pub(super) fn mode_changed_since(&self, target: &TargetMode) -> bool {
        if !self.mode_is_current(target.generation, target.host_seq) {
            return true;
        }
        let (seq, Some(host)) = self.current_host() else {
            return false;
        };
        let inference = host.inference();
        let errors = host.aggregate_errors();
        seq != target.host_seq
            || !self.mode_is_current(target.generation, seq)
            || errors != target.aggregate_errors
            || !Inference::same(target.inference.as_deref(), inference.as_ref())
    }

    pub(super) fn mode_is_current(&self, generation: u64, host_seq: u64) -> bool {
        self.mode_gen.load(Ordering::Acquire) == generation && self.host_seq() == host_seq
    }

    /// The published target and any cells whose mode generation lags behind it.
    pub(super) fn lagging_cells(&self, reg: &mut Registry) -> (TargetMode, Vec<Arc<Cell>>) {
        let target = reg.target_mode();
        if reg.switched_gen < reg.mode_gen {
            let lagging: Vec<Arc<Cell>> = reg
                .cells
                .values()
                .filter(|cell| cell.info.lock().mode_gen < reg.mode_gen)
                .cloned()
                .collect();
            if lagging.is_empty() {
                reg.switched_gen = reg.mode_gen;
            }
            (target, lagging)
        } else {
            (target, Vec::new())
        }
    }

    /// Advance `reg.switched_gen` when every cell has reached `target_gen`.
    pub(super) fn mark_mode_switched(&self, target_gen: u64) {
        let mut reg = self.reg.lock();
        if reg.mode_gen == target_gen
            && reg.switched_gen < target_gen
            && reg
                .cells
                .values()
                .all(|cell| cell.info.lock().mode_gen >= target_gen)
        {
            reg.switched_gen = target_gen;
        }
    }

    /// Recompile `cell` for `target` when its mode generation lags behind `target`.
    /// Caller holds `cell.state` and must NOT hold `self.reg`.
    pub(super) fn switch_cell(
        &self,
        cell: &Cell,
        st: &mut Slot,
        target: &TargetMode,
        deferred: &mut DeferredDrops,
    ) -> bool {
        if st.mode_gen >= target.generation || st.pending_retirement().is_some() {
            return false;
        }
        let completion = Arc::new(RetirementCompletion {
            owner: std::thread::current().id(),
            released: std::sync::OnceLock::new(),
        });
        st.retirement = Some(completion.clone());
        deferred.retirements.push(Retirement {
            completion,
            kick: self.kick.clone(),
        });
        // Every event of the old mode carries a commit at most `seen`, or at most the
        // commit of a circuit whose pass was in flight, which the state lock waited for.
        let boundary = self.seen.load(Ordering::Acquire).max(st.circuit.applied);
        self.switched.fetch_max(boundary, Ordering::AcqRel);
        st.mode_gen = target.generation;
        let fresh = st.fresh(target.inference.clone());
        let c = match fresh {
            Ok(mut c) => {
                c.aggregate_errors = Some(target.aggregate_errors);
                c
            }
            Err(e) => {
                self.fail_at_mode_gen(cell, st, &e, Some(target.generation), deferred);
                deferred.retired.extend(st.circuit.drain_retired());
                return true;
            }
        };
        // The old result stays, for the first evaluation in the new mode to be compared
        // with; transfer the existing budget without a 2x reservation peak, and drop the
        // old operators outside all locks before returning their credit.
        let mut old = std::mem::replace(&mut st.circuit, c);
        st.circuit.result = std::mem::take(&mut old.result);
        st.circuit.terms = std::mem::take(&mut old.terms);
        st.circuit.snap = old.snap.take();
        st.circuit.applied = old.applied;
        st.circuit.version = old.version.take();
        st.circuit.set_evidence_stamp(old.take_evidence_stamp());
        let (pool, budget) = old.take_budget();
        st.circuit.install_budget(pool, budget);
        st.circuit.shrink_budget_state();
        // Diffs of the old mode do not lead to rows of the new one.
        st.clear_history();
        deferred.retired.extend(st.circuit.drain_retired());
        deferred.circuits.push(old);
        st.rebased = true;
        let mut info = cell.info.lock();
        info.mode_gen = target.generation;
        info.plan = st.circuit.plan();
        info.whole = st.whole_reason(target.inference.as_deref());
        if info.status == Status::Ready {
            info.status = Status::Building;
            info.rows = 0;
        }
        true
    }

    /// Try to switch each lagging cell without blocking on any circuit whose pass is
    /// running. Returns whether any cell was busy and left for the worker.
    pub(super) fn try_switch_cells(
        &self,
        cells: &[Arc<Cell>],
        target: &TargetMode,
        deferred: &mut DeferredDrops,
    ) -> bool {
        let mut any_busy = false;
        for cell in cells {
            if let Some(mut st) = cell.state.try_lock() {
                any_busy |= st.pending_retirement().is_some();
                self.switch_cell(cell, &mut st, target, deferred);
            } else {
                any_busy = true;
            }
        }
        any_busy
    }

    /// Record any change of the host's mode and switch every idle circuit outside the
    /// registry lock, leaving any busy circuit for the worker so callers never block on
    /// an unrelated running pass.
    pub(super) fn sync_mode_nonblocking(&self) {
        if !self.has_host() {
            return;
        }
        self.refresh_mode();
        let mut deferred = DeferredDrops::default();
        let (target, lagging) = {
            let mut reg = self.reg.lock();
            self.lagging_cells(&mut reg)
        };
        if !lagging.is_empty() {
            let any_busy = self.try_switch_cells(&lagging, &target, &mut deferred);
            drop(deferred);
            if !any_busy {
                self.mark_mode_switched(target.generation);
            }
            self.kick();
        }
    }
}

#[cfg(test)]
mod switch_tests {
    use super::*;
    use crate::store::StoreOptions;

    struct Rdfs(Mutex<Option<Inference>>);
    impl Host for Rdfs {
        fn access(&self, _: Option<&str>) -> Result<Option<Arc<GraphAccess>>> {
            Ok(None)
        }
        fn stored_query(&self, _: &str) -> Result<String> {
            Err(Error::NotFound("none".into()))
        }
        fn inference(&self) -> Option<Inference> {
            self.0.lock().clone()
        }
    }

    /// The boundary of a switch covers the commit of a pass that finished while the
    /// switch waited for its circuit's lock, before the pass reported it.
    #[test]
    fn the_switch_boundary_covers_a_pass_in_flight() {
        use crate::sparql::rdfs::{RdfsOnRead, SchemaSource};
        let s = Store::in_memory(StoreOptions::default());
        let rt = Runtime::open(None, s.dataset_id(), RuntimeOptions::default());
        let host = Arc::new(Rdfs(Mutex::new(None)));
        rt.set_host(host.clone());
        let _l = rt
            .live(
                &s,
                "SELECT ?s WHERE { ?s ?p ?o }",
                None,
                &LiveOptions::default(),
            )
            .unwrap();
        // a pass moved the circuit to commit 7 and has not reported it yet
        let cell = rt.reg.lock().cells.values().next().unwrap().clone();
        cell.state.lock().circuit.applied = 7;
        assert!(rt.seen.load(Ordering::Acquire) < 7);
        *host.0.lock() = Inference::new(
            Some(Arc::new(RdfsOnRead::new(SchemaSource::Graph(None)))),
            Vec::new(),
        );
        rt.sync_mode_nonblocking();
        assert_eq!(rt.switched.load(Ordering::Acquire), 7);
    }

    /// Planning asks for a view's reading without waiting for another view's pass: a
    /// change of the dataset's reading is not applied to the circuits there, and a
    /// registry held past the query's deadline gives no answer.
    #[test]
    fn the_planning_reading_waits_for_no_pass() {
        use crate::sparql::AggregateErrors;
        use std::time::{Duration, Instant};
        struct Reading(Mutex<AggregateErrors>);
        impl Host for Reading {
            fn access(&self, _: Option<&str>) -> Result<Option<Arc<GraphAccess>>> {
                Ok(None)
            }
            fn stored_query(&self, _: &str) -> Result<String> {
                Err(Error::NotFound("none".into()))
            }
            fn aggregate_errors(&self) -> AggregateErrors {
                *self.0.lock()
            }
        }
        let s = Store::in_memory(StoreOptions::default());
        let rt = Runtime::open(None, s.dataset_id(), RuntimeOptions::default());
        let host = Arc::new(Reading(Mutex::new(AggregateErrors::Jena)));
        rt.set_host(host.clone());
        for (name, q) in [
            ("a", "SELECT (MIN(?o) AS ?m) WHERE { ?s ?p ?o }"),
            ("b", "SELECT ?s WHERE { ?s ?p ?o }"),
        ] {
            rt.put(&s, name, crate::incr::views::ViewDef::table(q))
                .unwrap();
        }
        rt.round(&s);
        assert_eq!(rt.view_reading("a", None), Some(AggregateErrors::Jena));
        // a pass of b holds its state while the dataset's reading changes
        let b = rt.reg.lock().views.get("b").unwrap().cell.clone().unwrap();
        let pass = b.state.lock();
        *host.0.lock() = AggregateErrors::Sparkles;
        rt.refresh_mode();
        let ask = |deadline: Duration| {
            let rt = rt.clone();
            let (tx, rx) = std::sync::mpsc::channel();
            std::thread::spawn(move || {
                tx.send(rt.view_reading("a", Some(Instant::now() + deadline)))
                    .unwrap();
            });
            rx.recv_timeout(Duration::from_secs(2))
                .expect("the planning reading waited past its deadline")
        };
        assert_eq!(
            ask(Duration::from_millis(10)),
            Some(AggregateErrors::Sparkles)
        );
        assert_eq!(rt.view_reading("missing", None), None);
        // a registry held past the deadline gives no answer by the deadline
        let reg = rt.reg.lock();
        assert_eq!(ask(Duration::from_millis(10)), None);
        drop(reg);
        drop(pass);
    }

    /// A read compares its reading with the circuit whose rows it returns. A switch of
    /// the dataset's reading can rebuild a view between the read's copy of the
    /// registry's reading and its lock of the view's state, which leaves the registry's
    /// copy naming the old reading while the circuit holds rows of the new one. A read
    /// that asks for the old reading then evaluates the view's query under it.
    #[test]
    fn a_read_compares_its_reading_with_the_circuit() {
        use crate::sparql::AggregateErrors;
        use crate::sparql::viewsvc::ViewRead;
        let s = Store::in_memory(StoreOptions::default());
        crate::sparql::update::update(
            &s,
            "INSERT DATA { <urn:a> <urn:g> \"x\" ; <urn:v> 1 . <urn:b> <urn:g> \"x\" }",
            &crate::sparql::QueryOptions::default(),
        )
        .unwrap();
        let rt = Runtime::open(None, s.dataset_id(), RuntimeOptions::default());
        rt.set_host(Arc::new(OpenHost));
        rt.put(
            &s,
            "m",
            crate::incr::views::ViewDef::table(
                "SELECT (MIN(?v) AS ?m) WHERE { ?s <urn:g> ?g OPTIONAL { ?s <urn:v> ?v } }",
            ),
        )
        .unwrap();
        rt.round(&s);
        let snap = s.snapshot();
        let read = |m: Option<AggregateErrors>| {
            let r = rt
                .read(&ViewRead {
                    name: "m",
                    at: snap.commit,
                    snap: Some(&snap),
                    allow_stale: false,
                    caller: None,
                    deadline: None,
                    aggregate_errors: m,
                    memory_parent: None,
                    memory_pool: None,
                    max_memory_bytes: None,
                })
                .unwrap();
            let cells: Vec<String> = r
                .rows
                .iter()
                .map(|(row, _)| row[0].as_ref().map_or("UNDEF".into(), |t| t.to_string()))
                .collect();
            (cells, r.evaluated_under)
        };
        assert_eq!(
            read(Some(AggregateErrors::Jena)),
            (vec!["UNDEF".into()], None)
        );
        // the circuit as a switch to sparkles rebuilt it, while the registry still
        // names jena
        let cell = rt.reg.lock().cells.values().next().unwrap().clone();
        {
            let mut st = cell.state.lock();
            let mut c = st.fresh(None).unwrap();
            c.aggregate_errors = Some(AggregateErrors::Sparkles);
            c.build(snap.clone(), &Control::none()).unwrap();
            st.circuit = c;
        }
        assert_eq!(rt.reg.lock().aggregate_errors, AggregateErrors::Jena);
        let one = "\"1\"^^<http://www.w3.org/2001/XMLSchema#integer>".to_string();
        assert_eq!(read(None), (vec![one], None));
        assert_eq!(
            read(Some(AggregateErrors::Jena)),
            (vec!["UNDEF".into()], Some(AggregateErrors::Jena))
        );
    }

    struct ModeHost {
        errors: Mutex<crate::sparql::AggregateErrors>,
        inference: Mutex<Option<Inference>>,
    }

    impl Host for ModeHost {
        fn access(&self, _: Option<&str>) -> Result<Option<Arc<GraphAccess>>> {
            Ok(None)
        }
        fn stored_query(&self, _: &str) -> Result<String> {
            Err(Error::NotFound("none".into()))
        }
        fn inference(&self) -> Option<Inference> {
            self.inference.lock().clone()
        }
        fn aggregate_errors(&self) -> crate::sparql::AggregateErrors {
            *self.errors.lock()
        }
    }

    fn read_cells(rt: &Runtime, snap: &Arc<crate::store::Snapshot>, name: &str) -> Vec<String> {
        let mut cells: Vec<String> = rt
            .read(&ViewRead {
                name,
                at: snap.commit,
                snap: Some(snap),
                allow_stale: false,
                caller: None,
                deadline: Some(Instant::now() + Duration::from_secs(5)),
                aggregate_errors: None,
                memory_parent: None,
                memory_pool: None,
                max_memory_bytes: None,
            })
            .unwrap()
            .rows
            .into_iter()
            .map(|(row, _)| row[0].as_ref().map_or("UNDEF".into(), |t| t.to_string()))
            .collect();
        cells.sort();
        cells
    }

    /// A view read after a change of aggregate-error reading or inference honors its
    /// deadline and cancellation while the worker is stuck inside a slow pass of an
    /// unrelated view, never hangs or returns old-mode rows, and succeeds once the
    /// slow pass finishes and the switch completes (item 1101).
    #[test]
    fn a_read_honors_deadline_and_cancel_during_worker_slow_pass_on_mode_change() {
        use crate::sparql::AggregateErrors;
        use crate::sparql::rdfs::{RdfsOnRead, SchemaSource};

        let s = Arc::new(Store::in_memory(StoreOptions::default()));
        crate::sparql::update::update(
            &s,
            "INSERT DATA { \
                <urn:a> <urn:g> \"x\" ; <urn:v> 1 . \
                <urn:b> <urn:g> \"x\" . \
                <urn:Student> <http://www.w3.org/2000/01/rdf-schema#subClassOf> <urn:Person> . \
                <urn:alice> a <urn:Student> . \
                <urn:other0> <urn:other> 0 \
            }",
            &crate::sparql::QueryOptions::default(),
        )
        .unwrap();

        let rt = Runtime::open(None, s.dataset_id(), RuntimeOptions::default());
        let host = Arc::new(ModeHost {
            errors: Mutex::new(AggregateErrors::Jena),
            inference: Mutex::new(None),
        });
        rt.set_host(host.clone());
        rt.put(
            &s,
            "agg",
            ViewDef::table(
                "SELECT (MIN(?v) AS ?m) WHERE { ?s <urn:g> ?g OPTIONAL { ?s <urn:v> ?v } }",
            ),
        )
        .unwrap();
        rt.put(
            &s,
            "people",
            ViewDef::table("SELECT ?x WHERE { ?x a <urn:Person> }"),
        )
        .unwrap();
        rt.put(
            &s,
            "slow",
            ViewDef::table("SELECT ?s WHERE { ?s <urn:other> ?o }"),
        )
        .unwrap();

        let worker_store = s.clone();
        let worker = rt
            .spawn_worker("test-views-1101".into(), s.subscribe_commits(), move || {
                Some(worker_store.clone())
            })
            .unwrap();
        for name in ["agg", "people", "slow"] {
            assert_eq!(
                rt.wait_built(name, Duration::from_secs(5)).unwrap().state,
                ViewState::Current
            );
        }
        let snap = s.snapshot();
        assert_eq!(read_cells(&rt, &snap, "agg"), vec!["UNDEF".to_string()]);
        assert!(read_cells(&rt, &snap, "people").is_empty());

        for (step, target_view, trigger_change) in [
            (
                1usize,
                "agg",
                Box::new(|host: &ModeHost| {
                    *host.errors.lock() = AggregateErrors::Sparkles;
                }) as Box<dyn Fn(&ModeHost)>,
            ),
            (
                2usize,
                "people",
                Box::new(|host: &ModeHost| {
                    *host.inference.lock() = Inference::new(
                        Some(Arc::new(RdfsOnRead::new(SchemaSource::Graph(None)))),
                        Vec::new(),
                    );
                }) as Box<dyn Fn(&ModeHost)>,
            ),
        ] {
            // Deterministically pause the worker inside `slow`'s pass while it holds
            // `slow`'s state lock (and does not hold the registry lock). Checking
            // `slow_cell.state.try_lock().is_none()` ensures the failpoint fires on
            // `slow`'s changed pass rather than an earlier cell's skipped pass.
            let slow_cell = rt
                .reg
                .lock()
                .views
                .get("slow")
                .unwrap()
                .cell
                .clone()
                .unwrap();
            let (entered_tx, entered_rx) = std::sync::mpsc::channel();
            let (release_tx, release_rx) = std::sync::mpsc::channel();
            let armed = Arc::new(AtomicBool::new(true));
            let release_rx = Mutex::new(release_rx);
            s.set_failpoint(
                "view-pass-applied",
                Some(Arc::new(move |_| {
                    if slow_cell.state.try_lock().is_none() && armed.swap(false, Ordering::AcqRel) {
                        entered_tx.send(()).unwrap();
                        let _ = release_rx.lock().recv_timeout(Duration::from_secs(10));
                    }
                })),
            );
            crate::sparql::update::update(
                &s,
                &format!("INSERT DATA {{ <urn:other{step}> <urn:other> {step} }}"),
                &crate::sparql::QueryOptions::default(),
            )
            .unwrap();
            entered_rx
                .recv_timeout(Duration::from_secs(5))
                .expect("worker did not enter slow pass");

            // Change the dataset mode while the worker is paused inside `slow`'s pass.
            trigger_change(&host);
            rt.mode_changed();
            let snap = s.snapshot();

            // Non-blocking metadata/planning operations return promptly and never report
            // the old mode as current. For the busy lagging `slow` view, metadata omits
            // the obsolete plan and row count rather than mixing target mode with old plan.
            let info = rt.get(&s, target_view).unwrap();
            assert_eq!(info.state, ViewState::Building, "step {step}: {info:?}");
            assert_eq!(info.commit, None);
            assert_eq!(info.rows, 0);
            let slow_info = rt.get(&s, "slow").unwrap();
            assert_eq!(slow_info.state, ViewState::Building);
            assert_eq!(slow_info.commit, None);
            assert!(slow_info.plan.is_none());
            assert_eq!(slow_info.rows, 0);
            if step == 2 {
                assert_eq!(
                    slow_info.evaluation,
                    Some(crate::incr::ops::Mode::Recompute)
                );
                assert!(slow_info.evaluation_reason.is_some());
            }
            assert!(!rt.list(&s).is_empty(), "list must not block on slow pass");
            let wait_t0 = Instant::now();
            let waited = rt
                .wait_built(target_view, Duration::from_millis(30))
                .unwrap();
            assert_eq!(waited.state, ViewState::Building);
            assert!(wait_t0.elapsed() < Duration::from_secs(2));

            // A missing view fails immediately rather than waiting for the mode switch.
            let missing_err = rt
                .read(&ViewRead {
                    name: "missing",
                    at: snap.commit,
                    snap: Some(&snap),
                    allow_stale: false,
                    caller: None,
                    deadline: Some(Instant::now() + Duration::from_secs(5)),
                    aggregate_errors: None,
                    memory_parent: None,
                    memory_pool: None,
                    max_memory_bytes: None,
                })
                .err()
                .unwrap();
            assert_eq!(missing_err.code(), "view-not-found");

            // Reads with a short deadline return Timeout promptly for both strict and
            // allow_stale reads without returning stale old-mode rows.
            for allow_stale in [false, true] {
                let t0 = Instant::now();
                let res = rt.read(&ViewRead {
                    name: target_view,
                    at: snap.commit,
                    snap: Some(&snap),
                    allow_stale,
                    caller: None,
                    deadline: Some(Instant::now() + Duration::from_millis(40)),
                    aggregate_errors: None,
                    memory_parent: None,
                    memory_pool: None,
                    max_memory_bytes: None,
                });
                assert!(
                    matches!(res, Err(Error::Timeout)),
                    "step {step} allow_stale={allow_stale}: expected Timeout, got {:?}",
                    res.err()
                );
                assert!(
                    t0.elapsed() < Duration::from_secs(2),
                    "read exceeded deadline while another view's pass was running"
                );
            }

            // A streaming open_view with cancellation returns Cancelled promptly.
            let cancel = Arc::new(AtomicBool::new(false));
            let cancel_flag = cancel.clone();
            let (started_tx, started_rx) = std::sync::mpsc::channel();
            let (done_tx, done_rx) = std::sync::mpsc::channel();
            let reader_rt = rt.clone();
            let reader_snap = snap.clone();
            let reader_view = target_view.to_string();
            let reader = std::thread::spawn(move || {
                let open = ViewOpen {
                    read: ViewRead {
                        name: &reader_view,
                        at: reader_snap.commit,
                        snap: Some(&reader_snap),
                        allow_stale: true,
                        caller: None,
                        deadline: Some(Instant::now() + Duration::from_secs(10)),
                        aggregate_errors: None,
                        memory_parent: None,
                        memory_pool: None,
                        max_memory_bytes: None,
                    },
                };
                let sent = std::cell::Cell::new(false);
                let got = reader_rt.open_view(
                    &open,
                    &|| {
                        if !sent.replace(true) {
                            started_tx.send(()).unwrap();
                        }
                        if cancel_flag.load(Ordering::Relaxed) {
                            Err(Error::Cancelled)
                        } else {
                            Ok(())
                        }
                    },
                    &mut |_| Ok(()),
                );
                done_tx.send(matches!(got, Err(Error::Cancelled))).unwrap();
            });
            started_rx.recv_timeout(Duration::from_secs(2)).unwrap();
            cancel.store(true, Ordering::Relaxed);
            assert!(
                done_rx.recv_timeout(Duration::from_secs(2)).unwrap(),
                "step {step}: open_view must return Cancelled promptly"
            );
            reader.join().unwrap();

            // Release `slow`'s pass and verify subsequent reads succeed in the new mode.
            s.set_failpoint("view-pass-applied", None);
            release_tx.send(()).unwrap();
            rt.kick();

            let expected = if step == 1 {
                vec!["\"1\"^^<http://www.w3.org/2001/XMLSchema#integer>".to_string()]
            } else {
                vec!["<urn:alice>".to_string()]
            };
            assert_eq!(read_cells(&rt, &snap, target_view), expected);
            assert_eq!(read_cells(&rt, &snap, "slow").len(), step + 1);
        }

        rt.close(&s);
        worker.join().unwrap();
    }

    /// When one circuit's state lock is held outside the worker across a mode change,
    /// the worker switches and rebuilds unrelated idle views first without holding the
    /// registry lock, while reads of the busy view honor their deadline and cancel and
    /// succeed once the busy pass ends (item 1101).
    #[test]
    fn an_unrelated_busy_view_does_not_block_switching_other_views_on_mode_change() {
        use crate::sparql::AggregateErrors;

        let s = Arc::new(Store::in_memory(StoreOptions::default()));
        crate::sparql::update::update(
            &s,
            "INSERT DATA { <urn:a> <urn:g> \"x\" ; <urn:v> 1 . <urn:b> <urn:g> \"x\" . \
             <urn:c> <urn:h> \"y\" ; <urn:w> 2 . <urn:d> <urn:h> \"y\" }",
            &crate::sparql::QueryOptions::default(),
        )
        .unwrap();

        let rt = Runtime::open(None, s.dataset_id(), RuntimeOptions::default());
        let host = Arc::new(ModeHost {
            errors: Mutex::new(AggregateErrors::Jena),
            inference: Mutex::new(None),
        });
        rt.set_host(host.clone());
        rt.put(
            &s,
            "fast",
            ViewDef::table(
                "SELECT (MIN(?v) AS ?m) WHERE { ?s <urn:g> ?g OPTIONAL { ?s <urn:v> ?v } }",
            ),
        )
        .unwrap();
        rt.put(
            &s,
            "busy",
            ViewDef::table(
                "SELECT (MIN(?w) AS ?m) WHERE { ?s <urn:h> ?h OPTIONAL { ?s <urn:w> ?w } }",
            ),
        )
        .unwrap();

        let worker_store = s.clone();
        let worker = rt
            .spawn_worker("test-busy-1101".into(), s.subscribe_commits(), move || {
                Some(worker_store.clone())
            })
            .unwrap();
        for name in ["fast", "busy"] {
            assert_eq!(
                rt.wait_built(name, Duration::from_secs(5)).unwrap().state,
                ViewState::Current
            );
        }
        rt.settle(Duration::from_secs(5));

        // Hold `busy`'s state lock, start a round that explicitly captures the old mode
        // (`Jena`) and parks waiting on `busy` in Pass 2, then toggle to `Sparkles`.
        let busy_cell = rt
            .reg
            .lock()
            .views
            .get("busy")
            .unwrap()
            .cell
            .clone()
            .unwrap();
        let busy_guard = busy_cell.state.lock();
        let waits_before = rt.busy_waits.load(Ordering::Acquire);
        rt.kick();
        let wait_deadline = Instant::now() + Duration::from_secs(5);
        while rt.busy_waits.load(Ordering::Acquire) == waits_before {
            assert!(
                Instant::now() < wait_deadline,
                "worker did not enter Pass 2 waiting on busy under the old mode"
            );
            std::thread::yield_now();
        }
        *host.errors.lock() = AggregateErrors::Sparkles;
        rt.mode_changed();

        let snap = s.snapshot();
        // The worker rotates out of the old-mode Pass-2 wait and switches/rebuilds
        // `fast` in Pass 1 without waiting for `busy`.
        assert_eq!(
            read_cells(&rt, &snap, "fast"),
            vec!["\"1\"^^<http://www.w3.org/2001/XMLSchema#integer>".to_string()]
        );

        // Lagging metadata for `busy` omits the stale plan and row count while building.
        let busy_info = rt.get(&s, "busy").unwrap();
        assert_eq!(busy_info.state, ViewState::Building);
        assert_eq!(busy_info.commit, None);
        assert!(busy_info.plan.is_none());
        assert_eq!(busy_info.rows, 0);

        // `busy` itself honors deadline and cancellation promptly.
        let busy_err = rt.read(&ViewRead {
            name: "busy",
            at: snap.commit,
            snap: Some(&snap),
            allow_stale: false,
            caller: None,
            deadline: Some(Instant::now() + Duration::from_millis(40)),
            aggregate_errors: None,
            memory_parent: None,
            memory_pool: None,
            max_memory_bytes: None,
        });
        assert!(matches!(busy_err, Err(Error::Timeout)));

        let cancel = Arc::new(AtomicBool::new(true));
        let open_busy = ViewOpen {
            read: ViewRead {
                name: "busy",
                at: snap.commit,
                snap: Some(&snap),
                allow_stale: true,
                caller: None,
                deadline: Some(Instant::now() + Duration::from_secs(5)),
                aggregate_errors: None,
                memory_parent: None,
                memory_pool: None,
                max_memory_bytes: None,
            },
        };
        let cancelled = rt.open_view(
            &open_busy,
            &|| {
                if cancel.load(Ordering::Relaxed) {
                    Err(Error::Cancelled)
                } else {
                    Ok(())
                }
            },
            &mut |_| Ok(()),
        );
        assert!(matches!(cancelled, Err(Error::Cancelled)));

        // Releasing `busy` lets the worker finish switching and rebuilding it.
        drop(busy_guard);
        assert_eq!(
            read_cells(&rt, &snap, "busy"),
            vec!["\"2\"^^<http://www.w3.org/2001/XMLSchema#integer>".to_string()]
        );

        rt.close(&s);
        worker.join().unwrap();
    }

    #[test]
    fn pass_two_detects_equal_schema_replacement() {
        check_pass_two_rotation(false);
    }

    #[test]
    fn pass_two_rechecks_mode_after_acquiring_busy_cell() {
        check_pass_two_rotation(true);
    }

    #[test]
    fn pass_two_samples_host_outside_state_during_catalog_attach() {
        use crate::sparql::AggregateErrors;
        use std::sync::mpsc::channel;

        struct InspectingHost {
            probe: TestHook,
        }

        impl Host for InspectingHost {
            fn access(&self, _: Option<&str>) -> Result<Option<Arc<GraphAccess>>> {
                Ok(None)
            }
            fn stored_query(&self, _: &str) -> Result<String> {
                Err(Error::NotFound("none".into()))
            }
            fn aggregate_errors(&self) -> AggregateErrors {
                (self.probe)();
                AggregateErrors::Jena
            }
        }

        let store = Arc::new(Store::in_memory(StoreOptions::default()));
        let rt = Runtime::open(None, store.dataset_id(), RuntimeOptions::default());
        rt.set_host(Arc::new(OpenHost));
        let def = ViewDef::table("SELECT ?s WHERE { ?s <urn:p> ?o }");
        rt.put(&store, "busy", def.clone()).unwrap();
        rt.round(&store);
        let busy = rt.reg.lock().view("busy").unwrap().cell.clone().unwrap();
        let armed = Arc::new(AtomicBool::new(false));
        let probe = armed.clone();
        let probe_cell = Arc::downgrade(&busy);
        let probe_runtime = Arc::downgrade(&rt);
        let probe_store = store.clone();
        rt.set_host(Arc::new(InspectingHost {
            probe: Arc::new(move || {
                if probe.swap(false, Ordering::AcqRel) {
                    // Bound the failure on the old lock order so the catalog and the
                    // worker can unwind instead of leaving the test deadlocked.
                    let cell = probe_cell.upgrade().unwrap();
                    let state = cell.state.try_lock_for(Duration::from_secs(1));
                    assert!(state.is_some(), "host callback ran under circuit state");
                    drop(state);
                    assert_eq!(
                        probe_runtime
                            .upgrade()
                            .unwrap()
                            .stats(&probe_store)
                            .circuits,
                        1
                    );
                }
            }),
        }));

        let (locked, at_lock) = channel();
        let (attaching, at_attach) = channel();
        let at_attach = Mutex::new(at_attach);
        *rt.busy_locked_hook.lock() = Some(Arc::new(move || {
            locked.send(()).unwrap();
            at_attach
                .lock()
                .recv_timeout(Duration::from_secs(5))
                .unwrap();
            armed.store(true, Ordering::Release);
        }));
        std::thread::scope(|scope| {
            let guard = busy.state.lock();
            let waits = rt.busy_waits.load(Ordering::Acquire);
            let round = scope.spawn(|| rt.round(&store));
            let until = Instant::now() + Duration::from_secs(5);
            while rt.busy_waits.load(Ordering::Acquire) == waits {
                assert!(Instant::now() < until, "round did not reach Pass 2");
                std::thread::yield_now();
            }
            drop(guard);
            at_lock.recv_timeout(Duration::from_secs(5)).unwrap();
            let catalog = scope.spawn(|| {
                let mut deferred = DeferredDrops::default();
                let mut reg = rt.reg.lock();
                reg.views
                    .insert("alias".into(), ViewEntry::new("alias", def, None));
                attaching.send(()).unwrap();
                rt.attach(&store, &mut reg, "alias", &mut deferred.retired)
                    .unwrap();
            });
            catalog.join().unwrap();
            round.join().unwrap();
        });
    }

    fn check_pass_two_rotation(acquired: bool) {
        use crate::sparql::AggregateErrors;
        use crate::sparql::rdfs::{RdfsOnRead, SchemaSource};

        let store = Store::in_memory(StoreOptions::default());
        let rt = Runtime::open(None, store.dataset_id(), RuntimeOptions::default());
        let new_inference = || {
            Inference::new(
                Some(Arc::new(RdfsOnRead::new(SchemaSource::Graph(None)))),
                vec![],
            )
        };
        let host = Arc::new(ModeHost {
            errors: Mutex::new(AggregateErrors::Jena),
            inference: Mutex::new(new_inference()),
        });
        rt.set_host(host.clone());
        rt.put(
            &store,
            "busy",
            ViewDef::table("SELECT ?s WHERE { ?s <urn:b> ?o }"),
        )
        .unwrap();
        rt.put(
            &store,
            "idle",
            ViewDef::table("SELECT ?s WHERE { ?s <urn:i> ?o }"),
        )
        .unwrap();
        rt.round(&store);
        let cell = |name| rt.reg.lock().view(name).unwrap().cell.clone().unwrap();
        let busy = cell("busy");
        let idle = cell("idle");
        let generation = rt.mode_gen.load(Ordering::Acquire);
        let passes = busy.info.lock().passes;

        let (locked, at_lock) = std::sync::mpsc::channel();
        let (resume, resumed) = std::sync::mpsc::channel();
        if acquired {
            let resumed = Mutex::new(resumed);
            *rt.busy_locked_hook.lock() = Some(Arc::new(move || {
                locked.send(()).unwrap();
                resumed.lock().recv_timeout(Duration::from_secs(5)).unwrap();
            }));
        }
        let (done, completed) = std::sync::mpsc::channel();
        std::thread::scope(|scope| {
            let guard = busy.state.lock();
            let waits = rt.busy_waits.load(Ordering::Acquire);
            rt.rebuild("busy").unwrap();
            let round = scope.spawn(|| {
                rt.round(&store);
                done.send(()).unwrap();
            });
            let until = Instant::now() + Duration::from_secs(5);
            while rt.busy_waits.load(Ordering::Acquire) == waits {
                assert!(Instant::now() < until, "round did not reach Pass 2");
                std::thread::yield_now();
            }

            if acquired {
                drop(guard);
                at_lock.recv_timeout(Duration::from_secs(5)).unwrap();
                // No other caller records this mode. The probe before the timed
                // acquisition saw Jena, and the probe after it must see Sparkles.
                *host.errors.lock() = AggregateErrors::Sparkles;
                resume.send(()).unwrap();
                completed.recv_timeout(Duration::from_secs(5)).unwrap();
                assert_eq!(busy.info.lock().passes, passes);
            } else {
                // The checkpoint key is unchanged, but this is a new RDFS setting.
                *host.inference.lock() = new_inference();
                let rotated = completed.recv_timeout(Duration::from_secs(2));
                let next_passes = idle.info.lock().passes;
                drop(guard);
                rotated.expect("equal-schema replacement did not rotate Pass 2");
                assert_eq!(next_passes, 1);
            }
            round.join().unwrap();
        });

        assert_eq!(rt.mode_gen.load(Ordering::Acquire), generation);
        rt.round(&store);
        assert_eq!(idle.info.lock().mode_gen, generation + 1);
        assert_eq!(busy.info.lock().passes, passes + 1);
    }

    /// Releasing replaced operators and detached budget leases before running the
    /// replacement build lets both a worker round and a `live()` join succeed under a
    /// tight shared `QueryPool` (Sol r1 P1).
    #[test]
    fn mode_switch_releases_old_circuit_and_retired_credit_before_rebuild_in_round_and_live() {
        use crate::sparql::AggregateErrors;

        use std::fmt::Write;

        let s = Store::in_memory(StoreOptions::default());
        // The DISTINCT decimal aggregate keeps a large operator state and a tiny
        // result. The switch must detach more than the 1 MiB live-credit slack.
        for batch in 0..18 {
            let mut insert = String::from("INSERT DATA { <urn:g> <urn:o> ");
            for i in 0..1500 {
                if i > 0 {
                    insert.push_str(", ");
                }
                write!(insert, "{}.0", batch * 1500 + i).unwrap();
            }
            insert.push_str(" }");
            crate::sparql::update::update(&s, &insert, &crate::sparql::QueryOptions::default())
                .unwrap();
        }
        let q = "SELECT (SUM(DISTINCT ?o) AS ?sum) WHERE { <urn:g> <urn:o> ?o }";

        // Measure the fresh build's peak. The remaining pool capacity covers that
        // peak and the tiny retained result, but cannot also cover the old operators.
        let peak = Arc::new(AtomicU64::new(0));
        let used = Arc::new(AtomicU64::new(0));
        let (peak_grow, used_grow, used_drop) = (peak.clone(), used.clone(), used.clone());
        let parent = sparkles_budget::MemoryDomain::unpooled().parent(
            move |bytes| {
                let total = used_grow.fetch_add(bytes, Ordering::Relaxed) + bytes;
                peak_grow.fetch_max(total, Ordering::Relaxed);
                Ok(())
            },
            move |bytes| {
                used_drop.fetch_sub(bytes, Ordering::Relaxed);
            },
        );
        let query = crate::sparql::parse_query(q, None, &[]).unwrap();
        let mut probe = Circuit::new(compile(&query, None).unwrap(), None);
        probe.set_pool(None, Some(parent));
        probe.build(s.snapshot(), &Control::none()).unwrap();
        assert!(probe.root.bytes() > sparkles_budget::LIVE_SLACK);
        drop(probe);
        assert_eq!(used.load(Ordering::Relaxed), 0);
        let tight_cap = peak.load(Ordering::Relaxed) + (128 << 10);

        // 1. A catalog switch publishes the replacement but keeps its retired lease
        // while a competing round tries to start rebuilding it.
        {
            let pool = QueryPool::new(64 << 20);
            let rt = Runtime::open_with_pool(
                None,
                s.dataset_id(),
                RuntimeOptions::default(),
                Some(pool.clone()),
            );
            let host = Arc::new(ModeHost {
                errors: Mutex::new(AggregateErrors::Jena),
                inference: Mutex::new(None),
            });
            rt.set_host(host.clone());
            rt.put(&s, "agg", ViewDef::table(q)).unwrap();
            rt.round(&s);
            assert!(pool.granted() > sparkles_budget::LIVE_SLACK);

            let filler = pool
                .work_parent()
                .reserve(pool.capacity() - tight_cap)
                .unwrap();
            *host.errors.lock() = AggregateErrors::Sparkles;
            rt.refresh_mode();
            let (target, lagging) = rt.lagging_cells(&mut rt.reg.lock());
            let mut retirement = DeferredDrops::default();
            assert!(!rt.try_switch_cells(&lagging, &target, &mut retirement));
            assert!(
                !retirement.retired.is_empty(),
                "switch must retain pool credit"
            );
            rt.round(&s);
            assert_eq!(lagging[0].info.lock().status, Status::Building);
            drop(lagging);
            drop(retirement);
            rt.round(&s);

            let info = rt.get(&s, "agg").unwrap();
            assert_eq!(
                info.state,
                ViewState::Current,
                "worker rebuild must not fail over-budget: {info:?}"
            );
            drop(filler);
            rt.close(&s);
            assert_eq!(pool.granted(), 0);
        }

        // 2. A competing live join waits outside the state lock until the catalog's
        // retirement finishes.
        {
            let pool = QueryPool::new(64 << 20);
            let rt = Runtime::open_with_pool(
                None,
                s.dataset_id(),
                RuntimeOptions::default(),
                Some(pool.clone()),
            );
            let host = Arc::new(ModeHost {
                errors: Mutex::new(AggregateErrors::Jena),
                inference: Mutex::new(None),
            });
            rt.set_host(host.clone());
            let live1 = rt.live(&s, q, None, &LiveOptions::default()).unwrap();
            assert!(pool.granted() > sparkles_budget::LIVE_SLACK);

            let filler = pool
                .work_parent()
                .reserve(pool.capacity() - tight_cap)
                .unwrap();
            *host.errors.lock() = AggregateErrors::Sparkles;
            rt.refresh_mode();
            let (target, lagging) = rt.lagging_cells(&mut rt.reg.lock());
            let mut retirement = DeferredDrops::default();
            assert!(!rt.try_switch_cells(&lagging, &target, &mut retirement));
            assert!(
                !retirement.retired.is_empty(),
                "switch must retain pool credit"
            );
            drop(lagging);
            // A synchronous pool wake on this thread cannot wait for its own release.
            let reentrant = rt.live(&s, q, None, &LiveOptions::default());
            assert_eq!(reentrant.err().unwrap().code(), "view-behind");
            let (waiting, reached) = std::sync::mpsc::channel();
            *rt.retirement_wait_hook.lock() = Some(Arc::new(move || waiting.send(()).unwrap()));
            let joined = std::thread::scope(|scope| {
                let joiner = scope.spawn(|| rt.live(&s, q, None, &LiveOptions::default()));
                let waited = reached.recv_timeout(Duration::from_secs(2));
                drop(retirement);
                (waited, joiner.join().unwrap())
            });
            joined.0.expect("live join did not wait for retirement");
            let live2 = joined
                .1
                .expect("live join rebuild must not fail over-budget");
            assert!(matches!(live2.try_next(), Some(LiveEvent::Snapshot { .. })));

            drop(live2);
            drop(live1);
            drop(filler);
            rt.close(&s);
            assert_eq!(pool.granted(), 0);
        }
    }

    /// `round()` drops its `busy` cell list before `self.gc()` so a circuit that was
    /// busy in Pass 1 and detached before GC releases its `QueryPool` credit before
    /// checkpointing and round-completion notification (Neon r1 P2).
    #[test]
    fn round_drops_busy_cells_before_gc_and_checkpoint() {
        let dir = tempfile::tempdir().unwrap();
        let s = Arc::new(Store::open(dir.path(), StoreOptions::default()).unwrap());
        let mut init = String::from("INSERT DATA { <urn:k0> <urn:keep> 1 . ");
        for i in 0..256 {
            init.push_str(&format!("<urn:b:{i}> <urn:busy> \"{}\". ", "x".repeat(64)));
        }
        init.push('}');
        crate::sparql::update::update(&s, &init, &crate::sparql::QueryOptions::default()).unwrap();

        let pool = QueryPool::new(16 << 20);
        let rt = Runtime::open_with_pool(
            Some(dir.path().to_path_buf()),
            s.dataset_id(),
            RuntimeOptions {
                checkpoint_seconds: 0,
                ..RuntimeOptions::default()
            },
            Some(pool.clone()),
        );
        rt.set_host(Arc::new(OpenHost));
        rt.put(
            &s,
            "keeper",
            ViewDef::table("SELECT ?s WHERE { ?s <urn:keep> ?o }"),
        )
        .unwrap();
        rt.round(&s);
        let keeper_only_granted = pool.granted();

        let live_busy = rt
            .live(
                &s,
                "SELECT ?s ?o WHERE { ?s <urn:busy> ?o }",
                None,
                &LiveOptions::default(),
            )
            .unwrap();
        let busy_cell = {
            let reg = rt.reg.lock();
            let keeper_key = reg
                .views
                .get("keeper")
                .and_then(|e| e.cell.as_ref())
                .map(|k| k.key);
            reg.cells
                .values()
                .find(|c| Some(c.key) != keeper_key)
                .cloned()
                .unwrap()
        };
        let weak_busy = Arc::downgrade(&busy_cell);
        assert!(pool.granted() > keeper_only_granted);

        // Advance keeper so checkpoint runs on the next round, and inspect `weak_busy`
        // and `pool.granted()` from inside `view-checkpoint-written` (after `self.gc()`
        // and before `round()` returns or notifies `epoch`).
        crate::sparql::update::update(
            &s,
            "INSERT DATA { <urn:k1> <urn:keep> 2 }",
            &crate::sparql::QueryOptions::default(),
        )
        .unwrap();
        let observed = Arc::new(Mutex::new(None));
        let observed_cb = observed.clone();
        let weak_cb = weak_busy.clone();
        let pool_cb = pool.clone();
        s.set_failpoint(
            "view-checkpoint-written",
            Some(Arc::new(move |_| {
                *observed_cb.lock() = Some((weak_cb.strong_count(), pool_cb.granted()));
            })),
        );

        let (released, release) = std::sync::mpsc::channel();
        let release = Mutex::new(release);
        *rt.busy_locked_hook.lock() = Some(Arc::new(move || {
            release.lock().recv_timeout(Duration::from_secs(5)).unwrap();
        }));
        let busy_guard = busy_cell.state.lock();
        let waits_before = rt.busy_waits.load(Ordering::Acquire);
        let round_rt = rt.clone();
        let round_store = s.clone();
        let handle = std::thread::spawn(move || round_rt.round(&round_store));

        // Wait until Pass 1 pushed `busy_cell` into `busy` and Pass 2 started waiting.
        let deadline = Instant::now() + Duration::from_secs(5);
        while rt.busy_waits.load(Ordering::Acquire) == waits_before {
            assert!(Instant::now() < deadline, "round did not enter Pass 2");
            std::thread::yield_now();
        }

        // Detach `busy_cell` before releasing its state lock so `gc()` collects it.
        drop(live_busy);
        drop(busy_guard);
        drop(busy_cell);
        released.send(()).unwrap();
        handle.join().unwrap();
        s.set_failpoint("view-checkpoint-written", None);

        let (strong_at_checkpoint, granted_at_checkpoint) =
            observed.lock().expect("checkpoint failpoint did not fire");
        assert_eq!(
            strong_at_checkpoint, 0,
            "busy Arc<Cell> must be dropped before gc() and checkpoint()"
        );
        assert!(
            granted_at_checkpoint <= keeper_only_granted + (64 << 10),
            "dead busy cell credit must be released before checkpoint(): got {granted_at_checkpoint}, baseline {keeper_only_granted}"
        );

        rt.close(&s);
        assert_eq!(pool.granted(), 0);
    }

    /// A reader paused between releasing the registry and locking `cell.state` must not
    /// accept an old-generation version published after a newer mode was recorded, even
    /// under `allow_stale: true` (Sol r1 P2 #1).
    #[test]
    fn reader_rejects_old_generation_published_after_mode_change() {
        use crate::sparql::AggregateErrors;

        let s = Arc::new(Store::in_memory(StoreOptions::default()));
        crate::sparql::update::update(
            &s,
            "INSERT DATA { <urn:a> <urn:g> \"x\" ; <urn:v> 1 . <urn:b> <urn:g> \"x\" }",
            &crate::sparql::QueryOptions::default(),
        )
        .unwrap();
        let rt = Runtime::open(None, s.dataset_id(), RuntimeOptions::default());
        let host = Arc::new(ModeHost {
            errors: Mutex::new(AggregateErrors::Jena),
            inference: Mutex::new(None),
        });
        rt.set_host(host.clone());
        rt.put(
            &s,
            "agg",
            ViewDef::table(
                "SELECT (MIN(?v) AS ?m) WHERE { ?s <urn:g> ?g OPTIONAL { ?s <urn:v> ?v } }",
            ),
        )
        .unwrap();
        rt.round(&s);
        let snap_n = s.snapshot();

        let (entered_tx, entered_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let armed = Arc::new(AtomicBool::new(true));
        let release_rx = Mutex::new(release_rx);
        *rt.before_read_state_hook.lock() = Some(Arc::new(move || {
            if armed.swap(false, Ordering::AcqRel) {
                entered_tx.send(()).unwrap();
                let _ = release_rx.lock().recv_timeout(Duration::from_secs(10));
            }
        }));

        let cancelled = Arc::new(AtomicBool::new(false));
        let after_incomplete = cancelled.clone();
        *rt.observer_wait_hook.lock() = Some(Arc::new(move |_| {
            after_incomplete.store(true, Ordering::Release);
        }));
        let reader_rt = rt.clone();
        let reader_snap = snap_n.clone();
        let reader = std::thread::spawn(move || {
            reader_rt.read_with_check(
                &ViewRead {
                    name: "agg",
                    at: reader_snap.commit,
                    snap: Some(&reader_snap),
                    allow_stale: true,
                    caller: None,
                    deadline: Some(Instant::now() + Duration::from_secs(5)),
                    aggregate_errors: None,
                    memory_parent: None,
                    memory_pool: None,
                    max_memory_bytes: None,
                },
                &|| {
                    if cancelled.load(Ordering::Acquire) {
                        Err(Error::Cancelled)
                    } else {
                        Ok(())
                    }
                },
            )
        });

        entered_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("reader did not hit before_read_state_hook");

        // Record generation B via a live join on a different query, then land commit N+1
        // and advance `agg` under generation A (simulating a stale round snapshot).
        *host.errors.lock() = AggregateErrors::Sparkles;
        let _other = rt
            .live(
                &s,
                "SELECT ?s WHERE { ?s <urn:other> ?o }",
                None,
                &LiveOptions::default(),
            )
            .unwrap();
        assert_eq!(rt.mode_gen.load(Ordering::Acquire), 1);

        crate::sparql::update::update(
            &s,
            "INSERT DATA { <urn:c> <urn:g> \"x\" ; <urn:v> 0 }",
            &crate::sparql::QueryOptions::default(),
        )
        .unwrap();
        let agg_cell = rt
            .reg
            .lock()
            .views
            .get("agg")
            .unwrap()
            .cell
            .clone()
            .unwrap();
        {
            let mut st = agg_cell.state.lock();
            assert_eq!(st.mode_gen, 0);
            let mut deltas = FxHashMap::default();
            let mut deferred = DeferredDrops::default();
            assert!(rt.advance(&s, &agg_cell, &mut st, &mut deltas, &mut deferred));
            assert_eq!(st.circuit.applied, snap_n.commit + 1);
            drop(st);
            drop(deferred);
        }

        // Release the paused reader: it must reject generation A at commit N+1 rather
        // than returning old-mode rows published after B was recorded.
        release_tx.send(()).unwrap();
        let res = reader.join().unwrap();
        *rt.before_read_state_hook.lock() = None;
        *rt.observer_wait_hook.lock() = None;
        assert!(
            matches!(res, Err(Error::Cancelled)),
            "reader accepted stale generation A after B was recorded: {:?}",
            res.map(|r| (r.commit, r.rows))
        );

        // Once a round runs under B, the reader gets the Sparkles result cleanly.
        rt.round(&s);
        let snap_after = s.snapshot();
        assert_eq!(
            read_cells(&rt, &snap_after, "agg"),
            vec!["\"0\"^^<http://www.w3.org/2001/XMLSchema#integer>".to_string()]
        );
    }

    /// An older host paused while sampling must never supersede a replacement host
    /// installed via `set_host` (Sol r1 P2 #3).
    #[test]
    fn older_host_cannot_supersede_replacement_host_mode() {
        use crate::sparql::AggregateErrors;

        struct PausingHost {
            errors: AggregateErrors,
            entered: std::sync::mpsc::Sender<()>,
            release: Mutex<std::sync::mpsc::Receiver<()>>,
            armed: AtomicBool,
        }
        impl Host for PausingHost {
            fn access(&self, _: Option<&str>) -> Result<Option<Arc<GraphAccess>>> {
                Ok(None)
            }
            fn stored_query(&self, _: &str) -> Result<String> {
                Err(Error::NotFound("none".into()))
            }
            fn inference(&self) -> Option<Inference> {
                if self.armed.swap(false, Ordering::AcqRel) {
                    self.entered.send(()).unwrap();
                    let _ = self.release.lock().recv_timeout(Duration::from_secs(10));
                }
                None
            }
            fn aggregate_errors(&self) -> AggregateErrors {
                self.errors
            }
        }

        let s = Arc::new(Store::in_memory(StoreOptions::default()));
        let rt = Runtime::open(None, s.dataset_id(), RuntimeOptions::default());
        let (entered_tx, entered_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let h1 = Arc::new(PausingHost {
            errors: AggregateErrors::Jena,
            entered: entered_tx,
            release: Mutex::new(release_rx),
            armed: AtomicBool::new(false),
        });
        rt.set_host(h1.clone());
        rt.put(
            &s,
            "agg",
            ViewDef::table("SELECT (MIN(?v) AS ?m) WHERE { ?s <urn:v> ?v }"),
        )
        .unwrap();
        rt.round(&s);

        // Arm H1 so `get` pauses while sampling H1, then install H2 (`Sparkles`).
        h1.armed.store(true, Ordering::Release);
        let rt_bg = rt.clone();
        let s_bg = s.clone();
        let t1 = std::thread::spawn(move || rt_bg.get(&s_bg, "agg").unwrap());
        entered_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("get did not enter H1::inference");

        let h2 = Arc::new(ModeHost {
            errors: Mutex::new(AggregateErrors::Sparkles),
            inference: Mutex::new(None),
        });
        rt.set_host(h2);
        release_tx.send(()).unwrap();
        let info = t1.join().unwrap();
        assert_eq!(info.state, ViewState::Building);
        assert_eq!(rt.reg.lock().aggregate_errors, AggregateErrors::Sparkles);

        // Calling `get` again sees Sparkles and never reverts to Jena.
        rt.round(&s);
        assert_eq!(rt.reg.lock().aggregate_errors, AggregateErrors::Sparkles);
        assert_eq!(rt.get(&s, "agg").unwrap().state, ViewState::Current);
    }

    #[test]
    fn a_new_host_revision_publishes_after_a_concurrent_older_revision() {
        use crate::sparql::AggregateErrors;
        use std::sync::mpsc::channel;

        let store = Store::in_memory(StoreOptions::default());
        let rt = Runtime::open(None, store.dataset_id(), RuntimeOptions::default());
        let host = Arc::new(ModeHost {
            errors: Mutex::new(AggregateErrors::Jena),
            inference: Mutex::new(None),
        });
        rt.set_host(host.clone());

        let (before_host, paused) = channel();
        let (resume, resumed) = channel();
        let resumed = Mutex::new(resumed);
        let once = AtomicBool::new(true);
        *rt.before_mode_host_hook.lock() = Some(Arc::new(move || {
            if once.swap(false, Ordering::AcqRel) {
                before_host.send(()).unwrap();
                resumed.lock().recv_timeout(Duration::from_secs(5)).unwrap();
            }
        }));

        std::thread::scope(|scope| {
            let refresh = scope.spawn(|| rt.refresh_mode());
            paused.recv_timeout(Duration::from_secs(5)).unwrap();
            rt.refresh_mode();
            assert_eq!(rt.reg.lock().host_seq, rt.host_seq());
            *host.errors.lock() = AggregateErrors::Sparkles;
            rt.mode_changed();
            resume.send(()).unwrap();
            refresh.join().unwrap();
        });
        let reg = rt.reg.lock();
        assert_eq!(reg.host_seq, rt.host_seq());
        assert_eq!(reg.aggregate_errors, AggregateErrors::Sparkles);
        assert_eq!(reg.mode_gen, 1);
    }

    #[test]
    fn timed_view_read_expires_while_metadata_is_held() {
        use std::sync::mpsc::channel;

        let store = Store::in_memory(StoreOptions::default());
        let rt = Runtime::open(
            None,
            store.dataset_id(),
            RuntimeOptions {
                view_wait: Duration::from_secs(5),
                ..RuntimeOptions::default()
            },
        );
        rt.set_host(Arc::new(OpenHost));
        rt.put(
            &store,
            "v",
            ViewDef::table("SELECT ?s WHERE { ?s <urn:p> ?o }"),
        )
        .unwrap();
        rt.round(&store);
        let cell = rt.reg.lock().view("v").unwrap().cell.clone().unwrap();

        let (probed, after_probe) = channel();
        let (resume, resumed) = channel();
        let resumed = Mutex::new(resumed);
        *rt.before_read_state_hook.lock() = Some(Arc::new(move || {
            probed.send(()).unwrap();
            resumed.lock().recv_timeout(Duration::from_secs(5)).unwrap();
        }));
        let (waiting, waited) = channel();
        *rt.observer_wait_hook.lock() = Some(Arc::new(move |_| {
            waiting.send(()).unwrap();
        }));
        let (done, completed) = channel();
        std::thread::scope(|scope| {
            let read = scope.spawn(|| {
                let snap = store.snapshot();
                done.send(
                    rt.read(&ViewRead {
                        name: "v",
                        at: snap.commit,
                        snap: Some(&snap),
                        caller: None,
                        allow_stale: true,
                        deadline: Some(Instant::now() + Duration::from_secs(1)),
                        aggregate_errors: None,
                        memory_parent: None,
                        memory_pool: None,
                        max_memory_bytes: None,
                    })
                    .map(|_| ()),
                )
                .unwrap();
            });
            after_probe.recv_timeout(Duration::from_secs(5)).unwrap();
            let metadata = cell.info.lock();
            resume.send(()).unwrap();
            let observed = waited.recv_timeout(Duration::from_secs(2));
            let result = completed.recv_timeout(Duration::from_secs(2));
            drop(metadata);
            read.join().unwrap();
            observed.expect("the read did not reach a bounded wait with metadata held");
            assert!(matches!(result, Ok(Err(Error::Timeout))), "{result:?}");
        });
    }

    #[test]
    fn timed_view_observers_do_not_sample_a_paused_host() {
        struct PausedHost {
            entered: std::sync::mpsc::Sender<()>,
            release: Arc<(Mutex<bool>, Condvar)>,
        }
        impl Host for PausedHost {
            fn access(&self, _: Option<&str>) -> Result<Option<Arc<GraphAccess>>> {
                Ok(None)
            }
            fn stored_query(&self, _: &str) -> Result<String> {
                unreachable!()
            }
            fn inference(&self) -> Option<Inference> {
                let (ready, cv) = &*self.release;
                let mut ready = ready.lock();
                let _ = self.entered.send(());
                while !*ready {
                    cv.wait(&mut ready);
                }
                None
            }
            fn aggregate_errors(&self) -> crate::sparql::AggregateErrors {
                self.inference();
                crate::sparql::AggregateErrors::default()
            }
        }

        let store = Store::in_memory(StoreOptions::default());
        let rt = Runtime::open(None, store.dataset_id(), RuntimeOptions::default());
        rt.set_host(Arc::new(OpenHost));
        rt.put(&store, "v", ViewDef::table("SELECT ?s WHERE { ?s ?p ?o }"))
            .unwrap();
        rt.round(&store);

        let (entered, sampling) = std::sync::mpsc::channel();
        let release = Arc::new((Mutex::new(false), Condvar::new()));
        rt.set_host(Arc::new(PausedHost {
            entered,
            release: release.clone(),
        }));
        std::thread::scope(|scope| {
            let round = scope.spawn(|| rt.round(&store));
            sampling.recv_timeout(Duration::from_secs(5)).unwrap();
            let (done, completed) = std::sync::mpsc::channel();
            let rt = &rt;
            let store = &store;
            let observers = scope.spawn(move || {
                let snap = store.snapshot();
                let read = |name| {
                    rt.read(&ViewRead {
                        name,
                        at: snap.commit,
                        snap: Some(&snap),
                        allow_stale: true,
                        caller: None,
                        deadline: Some(Instant::now() + Duration::from_millis(30)),
                        aggregate_errors: None,
                        memory_parent: None,
                        memory_pool: None,
                        max_memory_bytes: None,
                    })
                };
                done.send((
                    rt.view_reading("v", None),
                    read("missing").err().unwrap(),
                    read("v").err(),
                    rt.wait_built("v", Duration::from_millis(30)),
                ))
                .unwrap();
            });
            let result = completed.recv_timeout(Duration::from_secs(2));
            *release.0.lock() = true;
            release.1.notify_all();
            observers.join().unwrap();
            round.join().unwrap();
            let (warning, missing, read, waited) =
                result.expect("timed observer waited for a host callback");
            assert!(warning.is_none());
            assert_eq!(missing.code(), "view-not-found");
            assert!(matches!(read, Some(Error::Timeout)));
            let info = waited.unwrap();
            assert_eq!(info.state, ViewState::Building);
            assert_eq!(info.commit, None);
        });
    }

    #[test]
    fn wait_built_returns_a_single_snapshot() {
        let store = Store::in_memory(StoreOptions::default());
        crate::sparql::update::update(
            &store,
            "INSERT DATA { <urn:s> <urn:p> <urn:o> }",
            &crate::sparql::QueryOptions::default(),
        )
        .unwrap();
        let rt = Runtime::open(None, store.dataset_id(), RuntimeOptions::default());
        rt.set_host(Arc::new(OpenHost));
        let query = "SELECT ?s WHERE { ?s <urn:p> ?o }";
        rt.put(&store, "v", ViewDef::table(query)).unwrap();
        let cell = rt.reg.lock().view("v").unwrap().cell.clone().unwrap();

        // Pause after the waiter has rendered Building, then finish a worker build
        // under the state lock while the registry is still held by the waiter.
        let (observed, at_observation) = std::sync::mpsc::channel();
        let (finish, finished) = std::sync::mpsc::channel();
        let finished = Mutex::new(Some(finished));
        *rt.wait_info_hook.lock() = Some(Arc::new(move || {
            if let Some(finished) = finished.lock().take() {
                observed.send(()).unwrap();
                finished.recv_timeout(Duration::from_secs(5)).unwrap();
            }
        }));
        std::thread::scope(|scope| {
            let waiter = scope.spawn(|| rt.wait_built("v", Duration::from_secs(2)).unwrap());
            at_observation.recv_timeout(Duration::from_secs(2)).unwrap();
            let mut deferred = DeferredDrops::default();
            rt.build(&store, &cell, &mut cell.state.lock(), &mut deferred);
            finish.send(()).unwrap();
            let info = waiter.join().unwrap();
            assert_eq!(info.state, ViewState::Current);
            assert_eq!(info.commit, Some(store.snapshot().commit));
            assert_eq!(info.rows, 1);
        });
    }

    #[test]
    fn ready_pass_publishes_commit_and_rows_atomically() {
        let store = Store::in_memory(StoreOptions::default());
        crate::sparql::update::update(
            &store,
            "INSERT DATA { <urn:s> <urn:p> <urn:o> }",
            &crate::sparql::QueryOptions::default(),
        )
        .unwrap();
        let rt = Runtime::open(None, store.dataset_id(), RuntimeOptions::default());
        rt.set_host(Arc::new(OpenHost));
        let query = "SELECT ?s WHERE { ?s <urn:p> ?o }";
        rt.put(&store, "v", ViewDef::table(query)).unwrap();
        let cell = rt.reg.lock().view("v").unwrap().cell.clone().unwrap();

        // For both builders, metadata remains Building until the pass is published.
        for live in [false, true] {
            rt.rebuild("v").unwrap();
            let before = cell.info.lock().passes;
            let (publishing, at_publish) = std::sync::mpsc::channel();
            let (publish, published) = std::sync::mpsc::channel();
            let published = Mutex::new(published);
            *rt.note_pass_hook.lock() = Some(Arc::new(move || {
                publishing.send(()).unwrap();
                published
                    .lock()
                    .recv_timeout(Duration::from_secs(5))
                    .unwrap();
            }));
            std::thread::scope(|scope| {
                let builder = scope.spawn(|| {
                    if live {
                        rt.live(&store, query, None, &LiveOptions::default())
                            .unwrap();
                    } else {
                        rt.round(&store);
                    }
                });
                at_publish.recv_timeout(Duration::from_secs(2)).unwrap();
                let during = rt.wait_built("v", Duration::ZERO).unwrap();
                publish.send(()).unwrap();
                builder.join().unwrap();
                assert_eq!(during.state, ViewState::Building);
                assert_eq!(during.commit, None);
                let after = rt.wait_built("v", Duration::ZERO).unwrap();
                assert_eq!(after.state, ViewState::Current);
                assert_eq!(after.commit, Some(store.snapshot().commit));
                assert_eq!(after.rows, 1);
                assert_eq!(after.passes, before + 1);
            });
        }
    }

    #[test]
    fn eager_view_fallback_observes_cancellation_during_evaluation() {
        use crate::sparql::AggregateErrors;

        let store = Store::in_memory(StoreOptions::default());
        crate::sparql::update::update(
            &store,
            "INSERT DATA { <urn:s> <urn:p> <urn:o> }",
            &crate::sparql::QueryOptions::default(),
        )
        .unwrap();
        let rt = Runtime::open(None, store.dataset_id(), RuntimeOptions::default());
        rt.set_host(Arc::new(ModeHost {
            errors: Mutex::new(AggregateErrors::Sparkles),
            inference: Mutex::new(None),
        }));
        rt.put(
            &store,
            "v",
            ViewDef::table("SELECT ?s WHERE { ?s <urn:p> ?o }"),
        )
        .unwrap();
        rt.round(&store);

        let cancelled = Arc::new(AtomicBool::new(false));
        let flag = cancelled.clone();
        let parent = sparkles_budget::MemoryDomain::unpooled().parent(
            move |_| {
                flag.store(true, Ordering::Release);
                Ok(())
            },
            |_| {},
        );
        let snap = store.snapshot();
        let r = ViewRead {
            name: "v",
            at: snap.commit,
            snap: Some(&snap),
            allow_stale: true,
            caller: None,
            deadline: Some(Instant::now() + Duration::from_secs(5)),
            aggregate_errors: Some(AggregateErrors::Jena),
            memory_parent: Some(&|| Ok(parent.clone())),
            memory_pool: None,
            max_memory_bytes: None,
        };
        let result = rt.read_controlled(&r, &|| Ok(()), &|| {
            crate::task::Cancellation::from_flags([cancelled.clone()])
        });
        assert!(
            cancelled.load(Ordering::Acquire),
            "fallback did not begin allocating"
        );
        assert!(matches!(result, Err(Error::Cancelled)));
    }

    #[test]
    fn eager_view_fallback_stops_installing_rows_after_cancellation() {
        use crate::sparql::AggregateErrors;

        let store = Store::in_memory(StoreOptions::default());
        let rt = Runtime::open(None, store.dataset_id(), RuntimeOptions::default());
        rt.set_host(Arc::new(OpenHost));
        let values = (0..300)
            .map(|i| i.to_string())
            .collect::<Vec<_>>()
            .join(" ");
        rt.put(
            &store,
            "v",
            ViewDef::table(format!("SELECT * WHERE {{ VALUES ?n {{ {values} }} }}")),
        )
        .unwrap();
        rt.round(&store);

        let cancelled = Arc::new(AtomicBool::new(false));
        let flag = cancelled.clone();
        let installed = Arc::new(AtomicU64::new(0));
        let admitted = installed.clone();
        let row_bytes = crate::incr::zset::row_bytes(1);
        // Inline integer VALUES needs no budget reservation before result installation
        // beyond the read's exact admission of the fallback's staging, which follows the
        // refusal of its rounded chunk. Refuse rounded credit so each installed row
        // reaches the parent separately.
        let staged = AtomicBool::new(false);
        let parent = sparkles_budget::MemoryDomain::unpooled().parent(
            move |bytes| {
                if bytes > row_bytes {
                    if !bytes.is_multiple_of(sparkles_budget::LIVE_STEP)
                        && admitted.load(Ordering::Relaxed) == 0
                        && !staged.swap(true, Ordering::Relaxed)
                    {
                        return Ok(());
                    }
                    return Err(sparkles_budget::ParentRefused::Overflow);
                }
                assert_eq!(bytes, row_bytes);
                admitted.fetch_add(1, Ordering::Relaxed);
                flag.store(true, Ordering::Release);
                Ok(())
            },
            |_| {},
        );
        let snap = store.snapshot();
        let result = rt.read_controlled(
            &ViewRead {
                name: "v",
                at: snap.commit,
                snap: Some(&snap),
                allow_stale: true,
                caller: None,
                deadline: None,
                aggregate_errors: Some(AggregateErrors::Sparkles),
                memory_parent: Some(&|| Ok(parent.clone())),
                memory_pool: None,
                max_memory_bytes: None,
            },
            &|| Ok(()),
            &|| crate::task::Cancellation::from_flags([cancelled.clone()]),
        );
        assert!(matches!(result, Err(Error::Cancelled)));
        assert_eq!(installed.load(Ordering::Relaxed), 256);
    }

    fn assert_ordered_eager_fallback_stops_window_scan(
        hook: &'static std::thread::LocalKey<crate::incr::ops::window::ScanHook>,
        cancel_at: usize,
    ) {
        use crate::sparql::AggregateErrors;
        use std::{cell::Cell, rc::Rc};

        let store = Store::in_memory(StoreOptions::default());
        let rt = Runtime::open(None, store.dataset_id(), RuntimeOptions::default());
        rt.set_host(Arc::new(OpenHost));
        let values = (0..600)
            .map(|i| i.to_string())
            .collect::<Vec<_>>()
            .join(" ");
        rt.put(
            &store,
            "v",
            ViewDef::table(format!(
                "SELECT * WHERE {{ VALUES ?n {{ {values} }} }} ORDER BY ?n LIMIT 1 OFFSET 550"
            )),
        )
        .unwrap();
        rt.round(&store);

        let cancelled = Arc::new(AtomicBool::new(false));
        let flag = cancelled.clone();
        let steps = Rc::new(Cell::new(0));
        let visits = steps.clone();
        hook.with(|slot| {
            *slot.borrow_mut() = Some(Box::new(move || {
                let step = visits.get() + 1;
                visits.set(step);
                if step == cancel_at {
                    flag.store(true, Ordering::Release);
                }
            }));
        });
        let snap = store.snapshot();
        let result = rt.read_controlled(
            &ViewRead {
                name: "v",
                at: snap.commit,
                snap: Some(&snap),
                allow_stale: true,
                caller: None,
                deadline: None,
                aggregate_errors: Some(AggregateErrors::Sparkles),
                memory_parent: None,
                memory_pool: None,
                max_memory_bytes: None,
            },
            &|| Ok(()),
            &|| crate::task::Cancellation::from_flags([cancelled.clone()]),
        );
        hook.with(|slot| *slot.borrow_mut() = None);
        assert!(matches!(result, Err(Error::Cancelled)));
        let visited = steps.get();
        assert!(
            (cancel_at..=cancel_at + 256).contains(&visited),
            "visited {visited} entries with cancellation triggered at {cancel_at}"
        );
    }

    #[test]
    fn ordered_eager_fallback_stops_window_output_scan_after_cancellation() {
        use crate::incr::ops::window::OUTPUT_SCAN_STEP;

        assert_ordered_eager_fallback_stops_window_scan(&OUTPUT_SCAN_STEP, 1);
    }

    #[test]
    fn ordered_eager_fallback_stops_window_fill_accounting_after_cancellation() {
        use crate::incr::ops::window::BYTE_SCAN_STEP;

        assert_ordered_eager_fallback_stops_window_scan(&BYTE_SCAN_STEP, 1);
    }

    #[test]
    fn ordered_eager_fallback_stops_window_final_accounting_after_cancellation() {
        use crate::incr::ops::window::BYTE_SCAN_STEP;

        // The fill accounts for all 600 distinct retained entries before finalization.
        assert_ordered_eager_fallback_stops_window_scan(&BYTE_SCAN_STEP, 601);
    }

    /// `wait_built` enforces its timeout while the registry is held, and both `open_view`
    /// and `wait_built` terminate when a round advances the epoch after observation
    /// and before the timed wait begins.
    #[test]
    fn timed_observers_honor_bounds_when_the_epoch_advances_at_expiry() {
        let s = Store::in_memory(StoreOptions::default());
        let rt = Runtime::open(
            None,
            s.dataset_id(),
            RuntimeOptions {
                view_wait: Duration::from_millis(40),
                ..RuntimeOptions::default()
            },
        );
        rt.set_host(Arc::new(OpenHost));
        rt.put(&s, "v", ViewDef::table("SELECT ?s WHERE { ?s <urn:p> ?o }"))
            .unwrap();

        // 1. Registry held past timeout: `wait_built` returns `Timeout` promptly.
        {
            let _reg = rt.reg.lock();
            let t0 = Instant::now();
            let res = rt.wait_built("v", Duration::from_millis(30));
            assert!(
                matches!(res, Err(Error::Timeout)),
                "expected Timeout while registry held, got {res:?}"
            );
            assert!(t0.elapsed() < Duration::from_secs(2));
        }

        let waits = Arc::new(AtomicUsize::new(0));
        let waits_in_hook = waits.clone();
        let runtime = Arc::downgrade(&rt);
        *rt.observer_wait_hook.lock() = Some(Arc::new(move |until| {
            std::thread::sleep(until.saturating_duration_since(Instant::now()));
            let rt = runtime.upgrade().unwrap();
            *rt.epoch.lock() += 1;
            waits_in_hook.fetch_add(1, Ordering::Release);
        }));

        let snap = s.snapshot();
        let t0 = Instant::now();
        let open_res = rt.open_view(
            &ViewOpen {
                read: ViewRead {
                    name: "v",
                    at: snap.commit,
                    snap: Some(&snap),
                    allow_stale: false,
                    caller: None,
                    deadline: None,
                    aggregate_errors: None,
                    memory_parent: None,
                    memory_pool: None,
                    max_memory_bytes: None,
                },
            },
            &|| Ok(()),
            &mut |_| Ok(()),
        );
        assert_eq!(open_res.err().unwrap().code(), "view-behind");
        assert!(t0.elapsed() < Duration::from_secs(2));

        let t1 = Instant::now();
        let waited = rt.wait_built("v", Duration::from_millis(40)).unwrap();
        assert_eq!(waited.state, ViewState::Building);
        assert!(t1.elapsed() < Duration::from_secs(2));

        assert_eq!(waits.load(Ordering::Acquire), 2, "one wait per call");
    }
}
