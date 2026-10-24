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
use crate::incr::{Host, Kind, LiveOptions, LiveQuery, PlanNode, ViewDef, ViewInfo};
use crate::sparql::viewsvc::{ViewRead, ViewRows, ViewSource};
use std::sync::{Arc, Weak};
use std::time::Duration;

/// The dataset's views (from [`Dataset::views`]).
#[derive(Clone)]
pub struct Views {
    pub(crate) ds: Dataset,
}

/// What the embedded library tells the runtime: every principal reads every quad,
/// the dataset's prefixes apply and stored queries resolve from its catalog.
struct LibraryHost {
    state: Weak<DatasetState>,
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

/// Answers `SERVICE view:read` for a dataset's queries.
#[derive(Debug)]
pub(crate) struct Source {
    pub(crate) state: Weak<DatasetState>,
}

impl ViewSource for Source {
    fn read_view(&self, r: &ViewRead<'_>) -> Result<ViewRows> {
        let state = self
            .state
            .upgrade()
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
                state: Arc::downgrade(state),
            }));
        }
        let weak = Arc::downgrade(state);
        let name = format!("sparkles-views-{}", state.name.as_deref().unwrap_or("ds"));
        let name: String = name.chars().take(15).collect();
        // a dataset only the worker holds is gone: it is closed rather than maintained
        if let Err(e) = state
            .views
            .spawn_worker(name, state.store.subscribe_commits(), move || {
                weak.upgrade()
                    .filter(|s| Arc::strong_count(s) > 1)
                    .map(StoreRef)
            })
        {
            tracing::error!("the views' worker did not start: {e}");
        }
    });
}

impl Drop for Dataset {
    fn drop(&mut self) {
        // The last handle waits for a round in progress, so that the dataset closes
        // here and not on the views' thread a moment later.
        if self.inner.views_worker.get().is_some() && Arc::strong_count(&self.inner) == 2 {
            let t = std::time::Instant::now();
            while self.inner.views.worker_busy() && t.elapsed() < Duration::from_secs(60) {
                std::thread::sleep(Duration::from_millis(1));
            }
        }
    }
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
        let at = self.ds.inner.store.snapshot().commit;
        self.rt().read(&ViewRead {
            name,
            at,
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
