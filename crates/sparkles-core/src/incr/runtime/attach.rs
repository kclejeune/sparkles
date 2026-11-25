//! Attaching views to circuits, recovering their checkpoints, and compiling every
//! circuit again when the dataset's inference changes.

use super::*;

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
            applied: 0,
            rows: 0,
            bytes: 0,
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
                whole,
                vars,
                history,
                subs: Vec::new(),
                view_limits: FxHashMap::default(),
                rebased: false,
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
            st.view_limits.insert(name.to_string(), def.memory_bytes);
            st.recompute_local_limit();
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
            let Ok(meta) = std::fs::metadata(&path) else {
                continue;
            };
            let mut file_lease = super::super::budget::ScratchLease::new_pool_only(Some(
                cell.state.lock().circuit.budget_handle(),
            ));
            if let Err(e) = file_lease.try_reserve(meta.len()) {
                tracing::warn!(
                    target: "sparkles::incr::runtime",
                    "view {name}: checkpoint {}: {e}; the view is built again",
                    path.display()
                );
                retired.extend(cell.state.lock().circuit.drain_retired());
                continue;
            }
            let Ok(bytes) = std::fs::read(&path) else {
                drop(file_lease);
                retired.extend(cell.state.lock().circuit.drain_retired());
                continue;
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
                    retired.extend(cell.state.lock().circuit.drain_retired());
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
                            st.circuit.applied = h.commit;
                            self.seen.fetch_max(h.commit, Ordering::AcqRel);
                            self.counters.loaded.fetch_add(1, Ordering::Relaxed);
                            let mut info = cell.info.lock();
                            info.status = Status::Ready;
                            info.applied = h.commit;
                            info.rows = st.circuit.result.total().max(0) as u64;
                            info.bytes = st.circuit.bytes();
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
            drop(bytes);
            drop(file_lease);
            retired.extend(cell.state.lock().circuit.drain_retired());
            if loaded {
                return;
            }
        }
    }

    pub(super) fn attach_pending(&self, store: &Store) {
        let Some(host) = self.host.read().clone() else {
            return;
        };
        let mut deferred = DeferredDrops::default();
        {
            let mut reg = self.reg.lock();
            self.sync_inference(&mut reg, host.as_ref(), &mut deferred);
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

    /// Compile every circuit for the host's inference and reading of errors in
    /// aggregates when either has changed. Each circuit then evaluates its query again
    /// in the new mode on the next round, and the difference reaches its live queries as
    /// a diff or a `reset`, so views stay attached and live queries keep running.
    pub(super) fn sync_inference(
        &self,
        reg: &mut Registry,
        host: &dyn Host,
        deferred: &mut DeferredDrops,
    ) {
        let now = host.inference();
        let errors = host.aggregate_errors();
        if Inference::same(reg.inference.as_deref(), now.as_ref()) && errors == reg.aggregate_errors
        {
            return;
        }
        let now = now.map(Arc::new);
        tracing::info!(
            target: "sparkles::incr::runtime",
            "the dataset's inference ({}) or aggregate errors ({errors}) changed: {} circuits evaluate again",
            now.as_ref().map_or("none", |i| i.reason()),
            reg.cells.len()
        );
        reg.inference = now.clone();
        reg.aggregate_errors = errors;
        // every event of the old mode carries a commit at most `seen`, or at most the
        // commit of a circuit whose pass was in flight, which the state lock waits for
        let mut boundary = self.seen.load(Ordering::Acquire);
        self.switched.fetch_max(boundary, Ordering::AcqRel);
        for cell in reg.cells.values() {
            let mut st = cell.state.lock();
            boundary = boundary.max(st.circuit.applied);
            self.switched.fetch_max(boundary, Ordering::AcqRel);
            let fresh = st.fresh(now.clone());
            let c = match fresh {
                Ok(mut c) => {
                    c.aggregate_errors = Some(errors);
                    c
                }
                Err(e) => {
                    self.fail(cell, &mut st, &e);
                    deferred.retired.extend(st.circuit.drain_retired());
                    continue;
                }
            };
            // the old result stays, for the first evaluation in the new mode to be
            // compared with; transfer the existing budget without a 2× reservation peak,
            // and drop the old operators outside all locks before returning their credit
            let mut old = std::mem::replace(&mut st.circuit, c);
            st.circuit.result = std::mem::take(&mut old.result);
            st.circuit.terms = std::mem::take(&mut old.terms);
            st.circuit.snap = old.snap.take();
            st.circuit.applied = old.applied;
            let (pool, budget) = old.take_budget();
            st.circuit.install_budget(pool, budget);
            st.circuit.shrink_budget_state();
            // diffs of the old mode do not lead to rows of the new one
            st.clear_history();
            deferred.retired.extend(st.circuit.drain_retired());
            deferred.circuits.push(old);
            st.rebased = true;
            let mut info = cell.info.lock();
            info.plan = st.circuit.plan();
            info.whole = st.whole_reason(now.as_deref());
            if info.status == Status::Ready {
                info.status = Status::Building;
            }
        }
    }

    /// [`sync_inference`](Self::sync_inference) for a reader, so that a read after a
    /// change of the setting never sees a circuit of the old mode as current.
    pub(super) fn sync_for_read(&self, reg: &mut Registry, deferred: &mut DeferredDrops) {
        if let Some(host) = self.host.read().clone() {
            self.sync_inference(reg, host.as_ref(), deferred);
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
        let mut deferred = DeferredDrops::default();
        {
            let mut reg = rt.reg.lock();
            rt.sync_inference(&mut reg, host.as_ref(), &mut deferred);
        }
        drop(deferred);
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
}
