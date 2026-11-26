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
        circuit.set_pool(self.pool.clone(), self.circuit_parent());
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
            let entry = reg.views.get_mut(name).expect("checked above");
            entry.error = Some(e);
            return Ok(());
        }
        let def = entry.def.clone();
        let text = Self::view_text(host.as_ref(), &def)?;
        let q = self.parse(host.as_ref(), &text)?;
        let access = host.access(def.principal.as_deref())?;
        let (cell, new) = self.cell_for(reg, q, &text, access.clone(), Some(def.memory_bytes))?;
        {
            let mut st = cell.state.lock();
            let was_empty = st.view_limits.is_empty();
            st.view_limits.insert(name.to_string(), def.memory_bytes);
            st.recompute_local_limit();
            if was_empty {
                st.circuit
                    .set_version_pool(Some(self.published_views.clone()));
                let mut info = cell.info.lock();
                info.bytes = st.circuit.bytes();
                info.published_bytes = st
                    .circuit
                    .version
                    .as_ref()
                    .map_or(0, |v| v.published_bytes());
            }
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
                if h.dataset == self.dataset
                    && h.fingerprint == st.fingerprint(cell.key)
                    && commit <= snap.commit
                {
                    match st.circuit.decode_with_input_charged(body, &snap) {
                        Ok(()) => {
                            st.circuit.set_applied(h.commit);
                            self.seen.fetch_max(h.commit, Ordering::AcqRel);
                            self.counters.loaded.fetch_add(1, Ordering::Relaxed);
                            let mut info = cell.info.lock();
                            info.status = Status::Ready;
                            info.applied = h.commit;
                            info.rows = st.circuit.result.total().max(0) as u64;
                            info.bytes = st.circuit.bytes();
                            info.published_bytes = st
                                .circuit
                                .version
                                .as_ref()
                                .map_or(0, |v| v.published_bytes());
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
        let Some(host) = self.host.read().clone() else {
            return;
        };
        let mut deferred = DeferredDrops::default();
        {
            let mut reg = self.reg.lock();
            let _ = self.note_mode(&mut reg, host.as_ref());
            let epoch = host.epoch();
            if self.host_epoch.swap(epoch, Ordering::AcqRel) != epoch {
                // grants may have changed: resolve every view's principal again
                for (name, e) in reg.views.iter_mut() {
                    e.error = None;
                    if let Some(c) = e.cell.take() {
                        c.release_view(name);
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

    /// Record any change of the host's inference or aggregate-error reading on `reg`
    /// without acquiring any circuit state lock, returning the current target mode and
    /// any circuits whose mode generation lags behind it.
    pub(super) fn note_mode(
        &self,
        reg: &mut Registry,
        host: &dyn Host,
    ) -> (TargetMode, Vec<Arc<Cell>>) {
        let now = host.inference();
        let errors = host.aggregate_errors();
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
            let boundary = self.seen.load(Ordering::Acquire);
            self.switched.fetch_max(boundary, Ordering::AcqRel);
        }
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
        if st.mode_gen >= target.generation {
            return false;
        }
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
                self.fail(cell, st, &e, deferred);
                cell.info.lock().mode_gen = target.generation;
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
        let Some(host) = self.host.read().clone() else {
            return;
        };
        let mut deferred = DeferredDrops::default();
        let (target, lagging) = {
            let mut reg = self.reg.lock();
            self.note_mode(&mut reg, host.as_ref())
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
            // `slow`'s state lock (and does not hold the registry lock).
            let (entered_tx, entered_rx) = std::sync::mpsc::channel();
            let (release_tx, release_rx) = std::sync::mpsc::channel();
            let armed = Arc::new(AtomicBool::new(true));
            let release_rx = Mutex::new(release_rx);
            s.set_failpoint(
                "view-pass-applied",
                Some(Arc::new(move |_| {
                    if armed.swap(false, Ordering::AcqRel) {
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
            let snap = s.snapshot();

            // Non-blocking metadata/planning operations return promptly and never report
            // the old mode as current.
            let info = rt.get(&s, target_view).unwrap();
            assert_eq!(info.state, ViewState::Building, "step {step}: {info:?}");
            assert_eq!(info.commit, None);
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

        // Hold `busy`'s state lock across the mode change.
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
        *host.errors.lock() = AggregateErrors::Sparkles;
        rt.kick();

        let snap = s.snapshot();
        // `fast` switches and rebuilds in pass 1 without waiting for `busy`.
        assert_eq!(
            read_cells(&rt, &snap, "fast"),
            vec!["\"1\"^^<http://www.w3.org/2001/XMLSchema#integer>".to_string()]
        );

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
}
