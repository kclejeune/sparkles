//! Incremental views and live queries (spec F13): `table` views kept current after
//! every commit, read with `SERVICE view:read`, and queries whose result changes reach
//! a subscriber as diffs.
//!
//! ```
//! use sparkles::Dataset;
//! use sparkles::incr::{LiveEvent, LiveOptions, ViewDef};
//!
//! let ds = Dataset::memory();
//! ds.update("INSERT DATA { <urn:a> <urn:in> <urn:t> . <urn:b> <urn:in> <urn:t> }")?;
//! let q = "SELECT ?t (COUNT(?p) AS ?n) WHERE { ?p <urn:in> ?t } GROUP BY ?t";
//! ds.views().put("sizes", ViewDef::table(q))?;
//! ds.views().wait_built("sizes", std::time::Duration::from_secs(10))?;
//! let rows = ds.views().read("sizes")?;
//! assert_eq!(rows.rows.len(), 1);
//!
//! let live = ds.live(q, &LiveOptions::default())?;
//! assert!(matches!(live.next(), Some(LiveEvent::Snapshot { .. })));
//! ds.update("INSERT DATA { <urn:c> <urn:in> <urn:t> }")?;
//! let diff = live.next_timeout(std::time::Duration::from_secs(10));
//! assert!(matches!(diff, Some(LiveEvent::Diff { .. })));
//! # Ok::<_, sparkles::Error>(())
//! ```

use crate::Dataset;
use crate::access::GraphAccess;
use crate::dataset::{DatasetState, StoreHandle};
use crate::error::{Error, Result};
use crate::incr::{Host, Inference, Kind, LiveOptions, LiveQuery, PlanNode, ViewDef, ViewInfo};
use crate::sparql::viewsvc::{ViewRead, ViewRows, ViewSource};
use std::sync::Arc;
#[cfg(test)]
use std::sync::Weak;
use std::time::Duration;

/// The dataset's views (from [`Dataset::views`]).
#[derive(Clone)]
pub struct Views {
    pub(crate) ds: Dataset,
}

/// What the embedded library tells the runtime: every principal reads every quad,
/// the dataset's prefixes apply and stored queries resolve from its catalog.
struct LibraryHost {
    state: crate::dataset::StateReference,
}

impl Host for LibraryHost {
    fn access(&self, _: Option<&str>) -> Result<Option<Arc<GraphAccess>>> {
        Ok(None)
    }

    fn prefixes(&self) -> Vec<(String, String)> {
        self.state
            .upgrade()
            .map(|s| s.store.prefixes().into_iter().collect())
            .unwrap_or_default()
    }

    fn inference(&self) -> Option<Inference> {
        inference(&*self.state.upgrade()?)
    }

    fn stored_query(&self, name: &str) -> Result<String> {
        let state = self
            .state
            .upgrade()
            .ok_or_else(|| Error::invalid("the dataset is closed"))?;
        state
            .queries
            .get(name, None)
            .map(|s| s.definition.query)
            .ok_or_else(|| crate::incr::error("invalid-view", format!("no stored query {name:?}")))
    }
}

/// What the dataset's queries read beyond the stored quads, as
/// [`Dataset::query_options`] sets it: RDFS on read, and the graph of materialized
/// inferences while the dataset has a reasoning record. Views and live queries evaluate
/// their whole query again with it.
#[doc(hidden)]
pub fn inference(state: &DatasetState) -> Option<Inference> {
    Inference::new(
        state.rdfs.read().clone(),
        if state.reasoning.read().is_some() {
            vec![crate::reasoning::INFERRED_GRAPH.to_string()]
        } else {
            Vec::new()
        },
    )
}

/// Answers `SERVICE view:read` for a dataset's queries.
#[derive(Debug)]
pub(crate) struct Source {
    pub(crate) state: crate::dataset::StateReference,
}

impl ViewSource for Source {
    fn read_view(&self, r: &ViewRead<'_>) -> Result<ViewRows> {
        let state = self
            .state
            .try_upgrade()?
            .ok_or_else(|| Error::invalid("the dataset is closed"))?;
        start(&state, None);
        let rt = state.views.clone();
        drop(state);
        rt.read(r)
    }
}

/// Start the thread that runs the views' passes, with `host` (the library's own when
/// `None` and no host is set). Later calls do nothing.
pub(crate) fn start(state: &Arc<DatasetState>, host: Option<Arc<dyn Host>>) {
    if let Some(h) = host {
        state.views.set_host(h);
    }
    state.views_worker.get_or_init(|| {
        if !state.views.has_host() {
            state.views.set_host(Arc::new(LibraryHost {
                state: state.reference.clone(),
            }));
        }
        let weak = state.reference.clone();
        let name = format!("sparkles-views-{}", state.name.as_deref().unwrap_or("ds"));
        let name: String = name.chars().take(15).collect();
        // The last Dataset handle closes the runtime before releasing its state.
        if let Err(e) = state
            .views
            .spawn_worker(name, state.store.subscribe_commits(), move || {
                weak.upgrade().map(StoreRef)
            })
        {
            tracing::error!("the views' worker did not start: {e}");
        }
    });
}

/// A store reached through its dataset, which keeps it open for a round.
struct StoreRef(Arc<DatasetState>);

impl std::ops::Deref for StoreRef {
    type Target = crate::store::Store;
    fn deref(&self) -> &crate::store::Store {
        &self.0.store
    }
}

impl Dataset {
    /// The incremental views (spec F13).
    pub fn views(&self) -> Views {
        Views { ds: self.clone() }
    }

    /// Start the views' passes with `host`, which resolves view principals, before any
    /// other use: the server does this when it opens a dataset. Without it, the first
    /// use starts them with the library's host, under which every principal reads every
    /// quad.
    pub fn start_views(&self, host: Arc<dyn Host>) {
        start(&self.inner, Some(host));
    }

    /// Subscribe to `query`, a SELECT, ASK or CONSTRUCT query that a view could be
    /// (spec F13 §2.3). The first event is the result, and each commit that changes
    /// it sends the change.
    pub fn live(&self, query: &str, o: &LiveOptions) -> Result<LiveQuery> {
        self.live_as(query, None, o)
    }

    /// [`live`](Self::live) read with the view `access` of a caller.
    pub fn live_as(
        &self,
        query: &str,
        access: Option<Arc<GraphAccess>>,
        o: &LiveOptions,
    ) -> Result<LiveQuery> {
        start(&self.inner, None);
        self.inner.views.live(&self.inner.store, query, access, o)
    }
}

impl Views {
    fn rt(&self) -> &Arc<crate::incr::Runtime> {
        start(&self.ds.inner, None);
        &self.ds.inner.views
    }

    /// Every view with its state, by name.
    pub fn list(&self) -> Vec<ViewInfo> {
        self.rt().list(&self.ds.inner.store)
    }

    /// View `name` with its state, or `None`.
    /// Whether a reader with `access` may read view `name`'s rows and what they
    /// disclose, such as their count (`None`: every graph).
    pub fn readable_by(&self, name: &str, access: Option<&Arc<GraphAccess>>) -> bool {
        self.rt().readable_by(name, access)
    }

    pub fn get(&self, name: &str) -> Option<ViewInfo> {
        self.rt().get(&self.ds.inner.store, name).ok()
    }

    /// Define or replace view `name`. Its first evaluation runs on the views' thread;
    /// [`wait_built`](Self::wait_built) waits for it.
    pub fn put(&self, name: &str, def: ViewDef) -> Result<ViewInfo> {
        if matches!(self.ds.inner.store, StoreHandle::Branch(_)) {
            return Err(crate::incr::error(
                "unsupported-option",
                "views on a branch are not supported yet (spec F13 Phase 2)",
            ));
        }
        self.rt().put(&self.ds.inner.store, name, def)
    }

    /// Remove view `name` and its state; whether it existed.
    pub fn delete(&self, name: &str) -> Result<bool> {
        match self.rt().delete(name) {
            Ok(()) => Ok(true),
            Err(Error::Component(c)) if c.code == "view-not-found" => Ok(false),
            Err(e) => Err(e),
        }
    }

    /// Evaluate view `name` again from scratch, lifting a suspension.
    pub fn rebuild(&self, name: &str) -> Result<()> {
        self.rt().rebuild(name)
    }

    /// Wait until view `name` has been evaluated, at most `timeout`.
    pub fn wait_built(&self, name: &str, timeout: Duration) -> Result<ViewInfo> {
        self.rt().wait_built(name, timeout)
    }

    /// The rows of view `name` at the head commit, waiting for it as `SERVICE
    /// view:read` does.
    pub fn read(&self, name: &str) -> Result<ViewRows> {
        let snap = self.ds.inner.store.snapshot();
        self.rt().read(&ViewRead {
            name,
            at: snap.commit,
            snap: Some(&snap),
            allow_stale: false,
            caller: None,
            deadline: None,
        })
    }

    /// Wait for one round of the views' thread, at most `timeout`: the views defined
    /// in the dataset are then attached and recovered.
    pub fn settle(&self, timeout: Duration) {
        self.rt().settle(timeout);
    }

    /// The plan `query` would have as a view, with each operator's mode.
    pub fn explain(&self, query: &str) -> Result<(Kind, PlanNode)> {
        self.rt().explain(query, None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::incr::LiveOptions;
    use crate::sparql::rdfs::{RdfsOnRead, SchemaSource};

    /// With RDFS on read, a view and a live query evaluate the whole query again with
    /// it, so they answer as the query does.
    #[test]
    fn rdfs_on_read_views_and_live_queries_recompute() {
        let ds = Dataset::memory();
        ds.update(
            "INSERT DATA { <urn:D> <http://www.w3.org/2000/01/rdf-schema#subClassOf> <urn:C> . \
             <urn:a> a <urn:D> }",
        )
        .unwrap();
        *ds.state().rdfs.write() = Some(Arc::new(RdfsOnRead::new(SchemaSource::Graph(None))));
        let q = "SELECT ?s WHERE { ?s a <urn:C> }";
        let v = ds.views().put("v", ViewDef::table(q)).unwrap();
        assert_eq!(v.evaluation_reason.as_deref(), Some("RDFS on read"));
        ds.views().wait_built("v", Duration::from_secs(10)).unwrap();
        assert_eq!(ds.views().read("v").unwrap().rows.len(), 1);
        let live = ds.live(q, &LiveOptions::default()).unwrap();
        let first = live.next_timeout(Duration::from_secs(10));
        assert!(
            matches!(&first, Some(crate::incr::LiveEvent::Snapshot { rows, .. }) if rows.len() == 1),
            "{first:?}"
        );
    }

    /// A host's temporary dataset reference must not let the final handle skip
    /// waiting for the worker and leave the directory locked after drop returns.
    #[test]
    fn last_handle_waits_for_a_host_holding_the_dataset() {
        struct PausedHost {
            state: Weak<DatasetState>,
            entered: std::sync::mpsc::Sender<()>,
            release: parking_lot::Mutex<std::sync::mpsc::Receiver<()>>,
        }
        impl Host for PausedHost {
            fn access(&self, _: Option<&str>) -> Result<Option<Arc<GraphAccess>>> {
                Ok(None)
            }
            fn stored_query(&self, _: &str) -> Result<String> {
                unreachable!()
            }
            fn inference(&self) -> Option<Inference> {
                let state = self.state.upgrade().unwrap();
                self.entered.send(()).unwrap();
                self.release
                    .lock()
                    .recv_timeout(Duration::from_secs(10))
                    .unwrap();
                inference(&state)
            }
        }
        let dir = tempfile::tempdir().unwrap();
        let ds = Dataset::open(dir.path()).unwrap();
        let (entered_tx, entered) = std::sync::mpsc::channel();
        let (release, release_rx) = std::sync::mpsc::channel();
        ds.start_views(Arc::new(PausedHost {
            state: Arc::downgrade(&ds.inner),
            entered: entered_tx,
            release: parking_lot::Mutex::new(release_rx),
        }));
        entered.recv_timeout(Duration::from_secs(10)).unwrap();
        let (done_tx, done) = std::sync::mpsc::channel();
        let closer = std::thread::spawn(move || {
            drop(ds);
            done_tx.send(()).unwrap();
        });
        let early = done.recv_timeout(Duration::from_millis(100));
        release.send(()).unwrap();
        closer.join().unwrap();
        assert!(
            matches!(early, Err(std::sync::mpsc::RecvTimeoutError::Timeout)),
            "the last handle returned while the worker held the dataset"
        );
        let reopened = Dataset::open(dir.path()).unwrap();
        assert_eq!(reopened.store().snapshot().commit, 0);
    }

    /// Other handles keep the worker usable, and simultaneous final drops still
    /// stop it before the directory is reopened.
    #[test]
    fn cloned_handles_close_the_worker_only_after_the_last_drop() {
        let dir = tempfile::tempdir().unwrap();
        let ds = Dataset::open(dir.path()).unwrap();
        let clone = ds.clone();
        ds.views()
            .put("v", ViewDef::table("SELECT ?s WHERE { ?s <urn:p> ?o }"))
            .unwrap();
        drop(ds);
        clone.update("INSERT DATA { <urn:s> <urn:p> 1 }").unwrap();
        clone
            .views()
            .wait_built("v", Duration::from_secs(10))
            .unwrap();
        assert_eq!(clone.views().read("v").unwrap().rows.len(), 1);
        let barrier = Arc::new(std::sync::Barrier::new(8));
        let mut handles = Vec::new();
        for _ in 0..8 {
            let ds = clone.clone();
            let barrier = barrier.clone();
            handles.push(std::thread::spawn(move || {
                barrier.wait();
                drop(ds);
            }));
        }
        drop(clone);
        for handle in handles {
            handle.join().unwrap();
        }
        let reopened = Dataset::open(dir.path()).unwrap();
        assert_eq!(reopened.store().snapshot().commit, 1);
    }

    /// A panicking round releases the shutdown gate during unwinding.
    #[test]
    fn a_panicking_views_worker_does_not_hold_the_dataset_open() {
        struct PanickingHost(std::sync::mpsc::Sender<()>);
        impl Host for PanickingHost {
            fn access(&self, _: Option<&str>) -> Result<Option<Arc<GraphAccess>>> {
                Ok(None)
            }
            fn stored_query(&self, _: &str) -> Result<String> {
                unreachable!()
            }
            fn inference(&self) -> Option<Inference> {
                self.0.send(()).unwrap();
                panic!("worker fault");
            }
        }
        let dir = tempfile::tempdir().unwrap();
        let ds = Dataset::open(dir.path()).unwrap();
        let (entered_tx, entered) = std::sync::mpsc::channel();
        ds.start_views(Arc::new(PanickingHost(entered_tx)));
        entered.recv_timeout(Duration::from_secs(10)).unwrap();
        drop(ds);
        let _reopened = Dataset::open(dir.path()).unwrap();
    }

    /// A host can release the last handle from a callback while the worker holds
    /// the registry lock. Checkpointing must wait until that round ends.
    #[test]
    fn a_host_can_release_the_last_dataset_handle_during_a_round() {
        struct ReleasingHost {
            ds: parking_lot::Mutex<Option<Dataset>>,
            release: parking_lot::Mutex<std::sync::mpsc::Receiver<()>>,
            done: std::sync::mpsc::Sender<()>,
        }
        impl Host for ReleasingHost {
            fn access(&self, _: Option<&str>) -> Result<Option<Arc<GraphAccess>>> {
                Ok(None)
            }
            fn stored_query(&self, _: &str) -> Result<String> {
                unreachable!()
            }
            fn inference(&self) -> Option<Inference> {
                self.release
                    .lock()
                    .recv_timeout(Duration::from_secs(10))
                    .unwrap();
                drop(self.ds.lock().take());
                self.done.send(()).unwrap();
                None
            }
        }
        let dir = tempfile::tempdir().unwrap();
        let ds = Dataset::open(dir.path()).unwrap();
        let state = ds.inner.clone();
        let rt = state.views.clone();
        let (release, release_rx) = std::sync::mpsc::channel();
        let (done_tx, done) = std::sync::mpsc::channel();
        let host = Arc::new(ReleasingHost {
            ds: parking_lot::Mutex::new(None),
            release: parking_lot::Mutex::new(release_rx),
            done: done_tx,
        });
        ds.start_views(host.clone());
        *host.ds.lock() = Some(ds);
        release.send(()).unwrap();
        done.recv_timeout(Duration::from_secs(10)).unwrap();
        // Join the deferred shutdown after the callback has released its locks.
        rt.close(&state.store);
        assert!(rt.is_closed());
        drop(state);
        let _reopened = Dataset::open(dir.path()).unwrap();
    }

    /// Synchronous host calls hold the registry too, even without a worker.
    #[test]
    fn a_synchronous_host_can_release_the_last_dataset_handle() {
        struct ReleasingHost(parking_lot::Mutex<Option<Dataset>>);
        impl Host for ReleasingHost {
            fn access(&self, _: Option<&str>) -> Result<Option<Arc<GraphAccess>>> {
                Ok(None)
            }
            fn stored_query(&self, _: &str) -> Result<String> {
                unreachable!()
            }
            fn inference(&self) -> Option<Inference> {
                drop(self.0.lock().take());
                None
            }
        }
        let dir = tempfile::tempdir().unwrap();
        let ds = Dataset::open(dir.path()).unwrap();
        let state = ds.inner.clone();
        state
            .views
            .set_host(Arc::new(ReleasingHost(parking_lot::Mutex::new(Some(ds)))));
        let (done_tx, done) = std::sync::mpsc::channel();
        let reader = std::thread::spawn(move || {
            assert!(state.views.list(&state.store).is_empty());
            assert!(state.views.is_closed());
            done_tx.send(()).unwrap();
        });
        done.recv_timeout(Duration::from_secs(2))
            .expect("synchronous host shutdown deadlocked");
        reader.join().unwrap();
        let _reopened = Dataset::open(dir.path()).unwrap();
    }

    const CLOSE_QUERY: &str = "SELECT ?s WHERE { ?s <urn:p> ?o }";

    struct ClosingHost {
        ds: parking_lot::Mutex<Option<Dataset>>,
        method: &'static str,
        call: usize,
        calls: std::sync::atomic::AtomicUsize,
        panic: bool,
    }

    impl ClosingHost {
        fn release(&self, method: &str) {
            if method == self.method
                && self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1 == self.call
            {
                drop(self.ds.lock().take());
                assert!(!self.panic, "host fault after releasing the final handle");
            }
        }
    }

    impl Host for ClosingHost {
        fn access(&self, _: Option<&str>) -> Result<Option<Arc<GraphAccess>>> {
            self.release("access");
            Ok(None)
        }
        fn prefixes(&self) -> Vec<(String, String)> {
            self.release("prefixes");
            Vec::new()
        }
        fn stored_query(&self, _: &str) -> Result<String> {
            self.release("stored-query");
            Ok(CLOSE_QUERY.into())
        }
        fn inference(&self) -> Option<Inference> {
            self.release("inference");
            None
        }
        fn epoch(&self) -> u64 {
            self.release("epoch");
            0
        }
    }

    /// Every synchronous operation that invokes a Host must defer its final close,
    /// including callbacks repeated while an attachment holds the registry lock.
    #[test]
    fn synchronous_operations_defer_all_host_callback_kinds() {
        let mut cases = vec![
            ("list", "inference", 1),
            ("get", "inference", 1),
            ("wait", "inference", 1),
            ("read", "inference", 1),
            ("live", "inference", 1),
            ("explain", "inference", 1),
            ("put", "inference", 1),
            ("round", "inference", 1),
            ("round", "epoch", 1),
        ];
        for method in ["access", "prefixes", "stored-query"] {
            cases.push(("put", method, 1));
            cases.push(("put", method, 2));
        }
        for (operation, method, call) in cases {
            let ds = Dataset::memory();
            let state = ds.inner.clone();
            let host = Arc::new(ClosingHost {
                ds: parking_lot::Mutex::new(Some(ds)),
                method,
                call,
                calls: std::sync::atomic::AtomicUsize::new(0),
                panic: false,
            });
            state.views.set_host(host.clone());
            let (done_tx, done) = std::sync::mpsc::channel();
            let reader = std::thread::spawn(move || {
                match operation {
                    "list" => {
                        state.views.list(&state.store);
                    }
                    "get" => {
                        let _ = state.views.get(&state.store, "v");
                    }
                    "wait" => {
                        let _ = state.views.wait_built("v", Duration::ZERO);
                    }
                    "read" => {
                        let _ = state.views.read(&ViewRead {
                            name: "v",
                            at: 0,
                            snap: None,
                            allow_stale: false,
                            caller: None,
                            deadline: None,
                        });
                    }
                    "live" => {
                        let _ = state.views.live(
                            &state.store,
                            CLOSE_QUERY,
                            None,
                            &LiveOptions::default(),
                        );
                    }
                    "explain" => {
                        let _ = state.views.explain(CLOSE_QUERY, None);
                    }
                    "put" => {
                        let _ = state.views.put(
                            &state.store,
                            "v",
                            ViewDef {
                                stored_query: Some("q".into()),
                                ..Default::default()
                            },
                        );
                    }
                    "round" => {
                        state.views.round(&state.store);
                    }
                    _ => unreachable!(),
                }
                assert!(
                    host.ds.lock().is_none(),
                    "{operation}/{method}/{call} did not release the handle"
                );
                assert!(
                    state.views.is_closed(),
                    "{operation}/{method}/{call} did not close"
                );
                done_tx.send(()).unwrap();
            });
            done.recv_timeout(Duration::from_secs(2))
                .unwrap_or_else(|e| panic!("{operation}/{method}/{call}: {e}"));
            reader.join().unwrap();
        }
    }

    /// Nested operations on the same or another runtime must finish before owned
    /// cleanup runs, since an outer operation can still hold a metadata lock.
    #[test]
    fn nested_runtime_operations_defer_close_until_the_outer_call_finishes() {
        struct NestedHost {
            target: parking_lot::Mutex<Option<Dataset>>,
            nested: Weak<crate::incr::Runtime>,
            own: Weak<DatasetState>,
            reentered: std::sync::atomic::AtomicBool,
        }
        impl Host for NestedHost {
            fn access(&self, _: Option<&str>) -> Result<Option<Arc<GraphAccess>>> {
                Ok(None)
            }
            fn stored_query(&self, _: &str) -> Result<String> {
                unreachable!()
            }
            fn inference(&self) -> Option<Inference> {
                if !self
                    .reentered
                    .swap(true, std::sync::atomic::Ordering::SeqCst)
                {
                    let nested = self.nested.upgrade().unwrap();
                    nested.explain(CLOSE_QUERY, None).unwrap();
                    assert!(!self.own.upgrade().unwrap().views.is_closed());
                    assert!(!nested.is_closed());
                } else {
                    drop(self.target.lock().take());
                }
                None
            }
        }
        for same in [true, false] {
            let ds = Dataset::memory();
            let state = ds.inner.clone();
            let other = (!same).then(Dataset::memory);
            let other_state = other.as_ref().map(|d| d.inner.clone());
            let nested = other_state
                .as_ref()
                .map_or_else(|| state.views.clone(), |s| s.views.clone());
            if let Some(other) = other {
                nested.set_host(Arc::new(ClosingHost {
                    ds: parking_lot::Mutex::new(Some(other)),
                    method: "inference",
                    call: 1,
                    calls: std::sync::atomic::AtomicUsize::new(0),
                    panic: false,
                }));
            }
            let host = Arc::new(NestedHost {
                target: parking_lot::Mutex::new(Some(ds)),
                nested: Arc::downgrade(&nested),
                own: Arc::downgrade(&state),
                reentered: std::sync::atomic::AtomicBool::new(false),
            });
            state.views.set_host(host.clone());
            let (done_tx, done) = std::sync::mpsc::channel();
            let reader = std::thread::spawn(move || {
                state.views.list(&state.store);
                if !same {
                    drop(host.target.lock().take());
                }
                assert!(state.views.is_closed());
                if let Some(other) = other_state {
                    assert!(other.views.is_closed());
                }
                done_tx.send(()).unwrap();
            });
            done.recv_timeout(Duration::from_secs(2))
                .expect("nested close deadlocked");
            reader.join().unwrap();
        }
    }

    /// Replacing a host can destroy the last handle while the host write lock is held.
    #[test]
    fn replacing_a_host_defers_cleanup_from_its_destructor() {
        struct OwningHost {
            ds: Option<Dataset>,
            state: Weak<DatasetState>,
        }
        impl Host for OwningHost {
            fn access(&self, _: Option<&str>) -> Result<Option<Arc<GraphAccess>>> {
                Ok(None)
            }
            fn stored_query(&self, _: &str) -> Result<String> {
                unreachable!()
            }
        }
        impl Drop for OwningHost {
            fn drop(&mut self) {
                drop(self.ds.take());
                assert!(
                    !self.state.upgrade().unwrap().views.is_closed(),
                    "close ran inside the old host destructor"
                );
            }
        }
        let ds = Dataset::memory();
        let state = ds.inner.clone();
        state.views.set_host(Arc::new(OwningHost {
            ds: Some(ds),
            state: Arc::downgrade(&state),
        }));
        state.views.set_host(Arc::new(crate::incr::OpenHost));
        assert!(state.views.is_closed());
    }

    /// Panic unwinding drops registry guards before draining the owned close action.
    #[test]
    fn synchronous_host_panic_finishes_its_deferred_close() {
        let ds = Dataset::memory();
        let state = ds.inner.clone();
        state.views.set_host(Arc::new(ClosingHost {
            ds: parking_lot::Mutex::new(Some(ds)),
            method: "inference",
            call: 1,
            calls: std::sync::atomic::AtomicUsize::new(0),
            panic: true,
        }));
        let (done_tx, done) = std::sync::mpsc::channel();
        let reader = std::thread::spawn(move || {
            let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                state.views.list(&state.store)
            }));
            assert!(panic.is_err());
            assert!(state.views.is_closed());
            done_tx.send(()).unwrap();
        });
        done.recv_timeout(Duration::from_secs(2))
            .expect("panic cleanup deadlocked");
        reader.join().unwrap();
    }

    /// An owner initialized before operation tracking can use views from its TLS
    /// destructor. Run in a subprocess because a TLS destructor panic aborts.
    #[test]
    fn tls_owner_can_close_views_during_teardown() {
        const CHILD: &str = "SPARKLES_TEST_VIEWS_TLS_TEARDOWN";
        const TEST: &str = "handles::views::tests::tls_owner_can_close_views_during_teardown";
        if std::env::var(CHILD).as_deref() == Ok(TEST) {
            struct Owner(Arc<DatasetState>);
            impl Drop for Owner {
                fn drop(&mut self) {
                    assert!(!self.0.views.is_closed());
                    assert!(self.0.views.list(&self.0.store).is_empty());
                    assert!(self.0.views.is_closed());
                }
            }
            thread_local! {
                static OWNER: std::cell::RefCell<Option<Owner>> = const { std::cell::RefCell::new(None) };
            }
            let dir = tempfile::tempdir().unwrap();
            let root = dir.path().to_path_buf();
            std::thread::spawn(move || {
                let ds = Dataset::open(&root).unwrap();
                // Keep existing store-access TLS alive past OWNER, while operation
                // tracking is first initialized afterward by set_host.
                drop(ds.store().snapshot());
                let state = ds.inner.clone();
                OWNER.with(|owner| *owner.borrow_mut() = Some(Owner(state.clone())));
                state.views.set_host(Arc::new(ClosingHost {
                    ds: parking_lot::Mutex::new(Some(ds)),
                    method: "inference",
                    call: 1,
                    calls: std::sync::atomic::AtomicUsize::new(0),
                    panic: false,
                }));
            })
            .join()
            .unwrap();
            let _reopened = Dataset::open(dir.path()).unwrap();
            return;
        }
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", TEST, "--nocapture"])
            .env(CHILD, TEST)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while child.try_wait().unwrap().is_none() {
            if std::time::Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                panic!("TLS teardown child deadlocked");
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        let output = child.wait_with_output().unwrap();
        assert!(
            output.status.success(),
            "TLS teardown child failed: {}\n{}\n{}",
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    /// The last handle of a dataset with running views closes it on its own thread, so
    /// the directory opens again at once.
    #[test]
    fn a_dataset_with_views_reopens_at_once() {
        let dir = tempfile::tempdir().unwrap();
        let q = "SELECT ?s WHERE { ?s <urn:p> ?o }";
        for i in 0..5 {
            let ds = Dataset::open(dir.path()).unwrap();
            ds.update(&format!("INSERT DATA {{ <urn:s{i}> <urn:p> 1 }}"))
                .unwrap();
            if i == 0 {
                ds.views().put("v", ViewDef::table(q)).unwrap();
            }
            let v = ds.views().wait_built("v", Duration::from_secs(10)).unwrap();
            assert!(v.commit.is_some());
            assert_eq!(ds.views().read("v").unwrap().rows.len(), i + 1);
        }
    }
}
