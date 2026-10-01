//! The spatial index's place in the store: enabling and disabling it, its status, and
//! the hooks that keep it in step with commits, generation switches and opening.
//!
//! * **Builds** of a generation's base run on a background thread from a snapshot,
//!   without the writer lock (the base depends only on the generation), and publish
//!   under it: the overlay is rebuilt then from the current snapshot's inserted quads,
//!   so commits made during the build (and the log replayed at open) are covered.
//!   Until then snapshots carry a `building` view and queries run without the index.
//!   `enable_geo` and `rebuild_geo` wait for their build; opening does not.
//! * **Commits** add their inserted rows to the tail (`maintain_geo`). A failure there
//!   never fails the write: the index turns `failed` and queries run without it until a
//!   rebuild (or a compaction) builds it again.
//! * **Bulk commits and compactions** build the new generation's base under the writer
//!   lock before it is published, keeping the epoch. Literals the previous generation's
//!   column holds are taken from it (by their key), so only new literals are parsed.
//! * **Files.** A persistent store writes each base it builds to `gen-NNNN/geo/` and
//!   reads it back from there (in place, decoding geometries on demand), so opening the
//!   store or enabling the same configuration again parses nothing. Files that are
//!   missing, damaged or made for something else are removed and the base is built
//!   again. A generation being replaced is retired first: its files are written no
//!   more, so a compaction never races a write into the directory it removes.

use super::{Snapshot, Store};
use crate::error::Result;
use crate::geo::{GeoConfig, GeoStatus, GeoView};
use crate::id::Id;
use std::sync::Arc;

#[cfg(feature = "geo")]
use crate::geo::column::Reuse;
#[cfg(feature = "geo")]
use crate::geo::index::{
    BuildCtl, GeoBase, GeoIndex, Lookup, Overlay, Row, TailRow, ViewState, build_base, overlay_of,
    tail_limit,
};
#[cfg(feature = "geo")]
use crate::geo::persist::{self, Identity, Problem};
#[cfg(feature = "geo")]
use std::path::PathBuf;
#[cfg(feature = "geo")]
use std::sync::atomic::Ordering;

/// The failpoint name that marks background builds as paused (see
/// [`Store::pause_geo_build`]).
#[cfg(all(feature = "geo", any(test, feature = "failpoints")))]
const PAUSE: &str = "geo-build-paused";

impl Store {
    /// Enable (or reconfigure) the spatial index and build it from the current state.
    /// The configuration is kept in `geo.json`. Writes go on while the index is built;
    /// the returned status is the one after the build.
    pub fn enable_geo(&self, c: GeoConfig) -> Result<GeoStatus> {
        c.validate()?;
        #[cfg(not(feature = "geo"))]
        return Err(crate::geo::not_built());
        #[cfg(feature = "geo")]
        {
            if let Some(root) = &self.root {
                super::write_atomic(
                    &root.join(crate::geo::CONFIG_FILE),
                    &serde_json::to_vec_pretty(&c).expect("serializable"),
                )?;
            }
            let epoch = self.geo.load_full().map_or(1, |g| g.epoch() + 1);
            let (idx, snap) = self.install_geo(c, epoch);
            self.spawn_geo_build(idx.clone(), epoch, snap, true);
            self.wait_geo_epoch(&idx, epoch)
        }
    }

    /// Turn the spatial index off: queries run without it, and `geo.json` is removed.
    pub fn disable_geo(&self) -> Result<()> {
        {
            let _w = self.writer.lock();
            #[cfg(feature = "geo")]
            if let Some(old) = self.geo.swap(None) {
                old.retired.store(true, Ordering::SeqCst);
                old.wake();
            }
            #[cfg(not(feature = "geo"))]
            self.geo.store(None);
            let snap = self.snapshot();
            if snap.geo.is_some() {
                let mut s = (*snap).clone();
                s.geo = None;
                self.current.store(Arc::new(s));
            }
            // the index files go with the index (a build still running writes none: its
            // index is retired)
            #[cfg(feature = "geo")]
            if let Some(root) = &self.root
                && let Some(gdir) = snap.generation.dir.as_ref().filter(|d| d.starts_with(root))
            {
                snap.generation
                    .geo
                    .with_files(|| persist::remove(&persist::dir_of(gdir)));
            }
        }
        if let Some(root) = &self.root {
            match std::fs::remove_file(root.join(crate::geo::CONFIG_FILE)) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e.into()),
            }
        }
        Ok(())
    }

    /// Rebuild the spatial index of the current generation from RDF (after a failure,
    /// for instance). Writes go on meanwhile.
    pub fn rebuild_geo(&self) -> Result<GeoStatus> {
        if !self.geo_enabled() {
            return Err(crate::error::Error::invalid("spatial index is not enabled"));
        }
        #[cfg(not(feature = "geo"))]
        return Err(crate::geo::not_built());
        #[cfg(feature = "geo")]
        {
            let w = self.writer.lock();
            let Some(idx) = self.geo.load_full() else {
                return Err(crate::error::Error::invalid("spatial index is not enabled"));
            };
            let epoch = idx.epoch.fetch_add(1, Ordering::SeqCst) + 1;
            let snap = self.snapshot();
            // a ready index serves queries until the new base replaces it
            let ready = snap.geo.as_ref().is_some_and(|v| v.usable().is_some());
            let snap = if ready {
                snap
            } else {
                self.publish_geo(&snap, self.building_view(&idx, epoch, &snap))
            };
            drop(w);
            // from RDF: the files are written again, not read
            self.spawn_geo_build(idx.clone(), epoch, snap, false);
            self.wait_geo_epoch(&idx, epoch)
        }
    }

    /// Spatial index status (`None`: not enabled).
    pub fn geo_status(&self) -> Option<GeoStatus> {
        #[cfg(feature = "geo")]
        {
            let idx = self.geo.load_full()?;
            Some(idx.status(&self.snapshot()))
        }
        #[cfg(not(feature = "geo"))]
        None
    }

    /// Whether the spatial index is enabled.
    pub fn geo_enabled(&self) -> bool {
        self.geo.load().is_some()
    }

    /// Wait for a running build of the spatial index (the one started at open, say) and
    /// return the status after it (`None`: not enabled). Returns at once while builds
    /// are paused.
    pub fn wait_geo(&self) -> Option<GeoStatus> {
        #[cfg(feature = "geo")]
        if let Some(idx) = self.geo.load_full()
            && !idx.paused.load(Ordering::SeqCst)
        {
            idx.wait(idx.epoch());
        }
        self.geo_status()
    }

    /// Test hook: hold (or release) background builds of the spatial index before they
    /// publish, so tests see the index while it is building.
    #[cfg(any(test, feature = "failpoints"))]
    pub fn pause_geo_build(&self, on: bool) {
        #[cfg(feature = "geo")]
        {
            let mut f = self.failpoints.lock();
            if on {
                f.insert(PAUSE, Arc::new(|_: &Store| {}));
            } else {
                f.remove(PAUSE);
            }
            if let Some(idx) = self.geo.load_full() {
                idx.paused.store(on, Ordering::SeqCst);
            }
        }
        #[cfg(not(feature = "geo"))]
        let _ = on;
    }

    /// Test hook: make the spatial index's next commit-path update fail (the index then
    /// turns `failed`).
    #[cfg(any(test, feature = "failpoints"))]
    #[doc(hidden)]
    pub fn fail_next_geo_commit(&self) {
        #[cfg(feature = "geo")]
        if let Some(idx) = self.geo.load_full() {
            idx.fail_next.store(true, Ordering::SeqCst);
        }
    }

    /// At open: start the spatial index if `geo.json` enables it. Its base is built in
    /// the background; queries run without it until then.
    pub(super) fn open_geo(&self) {
        let Some(root) = &self.root else {
            return;
        };
        #[cfg(not(feature = "geo"))]
        if root.join(crate::geo::CONFIG_FILE).exists() {
            tracing::warn!(
                "{}: a spatial index is configured but this build has no `geo` feature",
                root.display()
            );
        }
        #[cfg(feature = "geo")]
        {
            let cfg = match crate::geo::read_config(root).and_then(|c| match c {
                Some(c) => c.validate().map(|()| Some(c)),
                None => Ok(None),
            }) {
                Ok(Some(c)) => c,
                Ok(None) => return,
                Err(e) => {
                    tracing::error!("spatial index of {}: {e}", root.display());
                    return;
                }
            };
            let (idx, snap) = self.install_geo(cfg, 1);
            self.spawn_geo_build(idx, 1, snap, true);
        }
    }

    /// In the commit path, before `snap` is published: index the geometries of the
    /// commit's logged quads and give `snap` its view.
    pub(super) fn maintain_geo(&self, snap: &mut Snapshot, log: &[(u8, [Id; 4])]) {
        #[cfg(feature = "geo")]
        {
            let Some(idx) = self.geo.load_full() else {
                snap.geo = None;
                return;
            };
            let Some(prev) = snap.geo.take() else {
                return;
            };
            let mut v = (*prev).clone();
            v.commit = snap.commit;
            if v.usable().is_some() {
                // the snapshot this commit started from: still the published one
                let before = self.current.load_full();
                let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    index_commit(&idx, &mut v, snap, &before, log)
                }));
                let failure = match r {
                    Ok(Ok(())) => None,
                    Ok(Err(e)) => Some(e.to_string()),
                    Err(p) => Some(panic_message(&*p)),
                };
                if let Some(m) = failure {
                    tracing::error!("spatial index update failed, queries run without it: {m}");
                    idx.note(format!("update failed: {m}; rebuild the index"));
                    v = v.without_rows(ViewState::Failed);
                } else if v.bytes() > idx.budget {
                    idx.note(format!(
                        "the index needs more than its budget of {} bytes; compact the dataset or raise --geo-mb",
                        idx.budget
                    ));
                    v = v.without_rows(ViewState::OverBudget);
                }
            }
            snap.geo = Some(Arc::new(v));
        }
        #[cfg(not(feature = "geo"))]
        let _ = (snap, log);
    }

    /// After a bulk commit or a compaction, under the writer lock and before `new` is
    /// published: build the new generation's base (`prev` is the snapshot it replaces).
    pub(super) fn rebuild_geo_locked(&self, new: &mut Snapshot, prev: &Snapshot) {
        let _ = prev;
        #[cfg(feature = "geo")]
        {
            // the replaced generation's directory may go now: nothing writes there any more
            prev.generation.geo.retire();
            let Some(idx) = self.geo.load_full() else {
                new.geo = None;
                return;
            };
            let epoch = idx.epoch();
            let lookup = Arc::new(Lookup::new(new, &idx.config));
            let never = || false;
            let ctl = BuildCtl {
                budget: idx.budget,
                progress: &idx.progress,
                cancel: &never,
            };
            // the previous base's literals, if it indexed the same way
            let prev_base = prev
                .geo
                .as_ref()
                .filter(|v| v.config.index_hash() == idx.config.index_hash())
                .and_then(|v| v.usable().cloned());
            let reuse = prev_base.as_ref().map(|b| Reuse {
                snap: prev,
                column: &b.column,
            });
            let files = self.geo_files(new, &idx.config);
            let built = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                base_for(
                    new,
                    &idx.config,
                    &lookup,
                    &ctl,
                    files.as_ref(),
                    false,
                    reuse.as_ref(),
                )
            }));
            let (view, message, last) = finish_view(&idx, epoch, new, lookup, built);
            new.geo = Some(Arc::new(view));
            idx.finish(epoch, message, last);
        }
        #[cfg(not(feature = "geo"))]
        let _ = new;
    }

    /// The view of a past state (`?at=`): the configuration, never ready.
    pub(super) fn historical_geo(&self) -> Option<Arc<GeoView>> {
        #[cfg(feature = "geo")]
        {
            let idx = self.geo.load_full()?;
            Some(Arc::new(GeoView {
                config: idx.config.clone(),
                epoch: idx.epoch(),
                generation: 0,
                commit: 0,
                state: ViewState::Historical,
                lookup: Arc::new(Lookup::empty(&idx.config)),
                base: None,
                overlay: Arc::new(Overlay::empty()),
                tail: imbl::Vector::new(),
                skipped: imbl::Vector::new(),
            }))
        }
        #[cfg(not(feature = "geo"))]
        None
    }
}

#[cfg(feature = "geo")]
impl Store {
    /// Make a new index current (retiring the previous one) and publish the current
    /// snapshot with a `building` view; returns the index and that snapshot.
    fn install_geo(&self, mut cfg: GeoConfig, epoch: u64) -> (Arc<GeoIndex>, Arc<Snapshot>) {
        // the server can switch query rewrite off whatever `geo.json` says
        cfg.query_rewrite &= self.opts.geo_query_rewrite;
        let _w = self.writer.lock();
        let idx = Arc::new(GeoIndex::new(cfg, epoch, self.opts.geo_budget_bytes));
        #[cfg(any(test, feature = "failpoints"))]
        idx.paused
            .store(self.failpoints.lock().contains_key(PAUSE), Ordering::SeqCst);
        if let Some(old) = self.geo.swap(Some(idx.clone())) {
            old.retired.store(true, Ordering::SeqCst);
            old.wake();
        }
        let snap = self.snapshot();
        let snap = self.publish_geo(&snap, self.building_view(&idx, epoch, &snap));
        (idx, snap)
    }

    fn building_view(&self, idx: &GeoIndex, epoch: u64, snap: &Snapshot) -> GeoView {
        idx.progress.store(0, Ordering::Relaxed);
        GeoView::pending(
            idx.config.clone(),
            epoch,
            snap,
            Arc::new(Lookup::new(snap, &idx.config)),
            ViewState::Building(idx.progress.clone()),
        )
    }

    /// Publish `snap` with view `v` (writer lock held).
    fn publish_geo(&self, snap: &Snapshot, v: GeoView) -> Arc<Snapshot> {
        let mut s = snap.clone();
        s.geo = Some(Arc::new(v));
        let s = Arc::new(s);
        self.current.store(s.clone());
        s
    }

    /// The status after the build of `epoch` (at once while builds are paused).
    fn wait_geo_epoch(&self, idx: &GeoIndex, epoch: u64) -> Result<GeoStatus> {
        if !idx.paused.load(Ordering::SeqCst) {
            idx.wait(epoch);
        }
        self.geo_status()
            .ok_or_else(|| crate::error::Error::invalid("spatial index is not enabled"))
    }

    /// Where the index files of `snap`'s generation go, and what they must match
    /// (`None`: the store keeps none, being in memory, or this platform cannot read
    /// them in place).
    fn geo_files(&self, snap: &Snapshot, cfg: &GeoConfig) -> Option<(PathBuf, Identity)> {
        if !persist::SUPPORTED {
            return None;
        }
        let root = self.root.as_ref()?;
        let gdir = snap
            .generation
            .dir
            .as_ref()
            .filter(|d| d.starts_with(root))?;
        Some((
            persist::dir_of(gdir),
            Identity::of(
                gdir,
                snap.generation.meta.quads,
                snap.generation.meta.terms,
                cfg.index_hash(),
            ),
        ))
    }

    /// Build the base of `snap`'s generation on a background thread (or, with `load`,
    /// read it from the generation's files when they fit) and publish it (with the
    /// overlay of the snapshot current then) unless it was superseded: by a newer
    /// build, a generation switch (which builds its own), a disable or the store's
    /// closing.
    fn spawn_geo_build(&self, idx: Arc<GeoIndex>, epoch: u64, snap: Arc<Snapshot>, load: bool) {
        let files = self.geo_files(&snap, &idx.config);
        let current = Arc::downgrade(&self.current);
        let writer = Arc::downgrade(&self.writer);
        let uid = snap.generation.uid;
        let failed = idx.clone();
        let spawned = std::thread::Builder::new()
            .name("geo-build".into())
            .spawn(move || {
                let superseded = || {
                    idx.retired.load(Ordering::SeqCst)
                        || idx.epoch() != epoch
                        || current
                            .upgrade()
                            .is_none_or(|c| c.load().generation.uid != uid)
                };
                let lookup = Arc::new(Lookup::new(&snap, &idx.config));
                let ctl = BuildCtl {
                    budget: idx.budget,
                    progress: &idx.progress,
                    cancel: &superseded,
                };
                let reading = snap.without_cache_fill();
                let built = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    base_for(
                        &reading,
                        &idx.config,
                        &lookup,
                        &ctl,
                        files.as_ref(),
                        load,
                        None,
                    )
                }));
                drop(reading);
                while idx.paused.load(Ordering::SeqCst) && !superseded() {
                    std::thread::sleep(std::time::Duration::from_millis(5));
                }
                let (Some(current), Some(writer)) = (current.upgrade(), writer.upgrade()) else {
                    idx.finish(epoch, None, None);
                    return;
                };
                let w = writer.lock();
                if w.closed || superseded() {
                    drop(w);
                    idx.finish(epoch, None, None);
                    return;
                }
                let cur = current.load_full();
                let (view, message, last) = finish_view(&idx, epoch, &cur, lookup, built);
                let mut s = (*cur).clone();
                s.geo = Some(Arc::new(view));
                current.store(Arc::new(s));
                drop(w);
                idx.finish(epoch, message, last);
            });
        if let Err(e) = spawned {
            tracing::error!("cannot start the spatial index build: {e}");
            failed.finish(epoch, Some(format!("build failed: {e}")), None);
        }
    }
}

/// The base of `snap`'s generation: with `load`, read from the generation's index files
/// (`files`) when they fit it (files that do not are removed); else built (taking what
/// it can from `reuse`) and, with `files`, written there and read back, so the base is
/// read in place like one opened later.
#[cfg(feature = "geo")]
fn base_for(
    snap: &Snapshot,
    cfg: &GeoConfig,
    lookup: &Lookup,
    ctl: &BuildCtl<'_>,
    files: Option<&(PathBuf, Identity)>,
    load: bool,
    reuse: Option<&Reuse<'_>>,
) -> Result<GeoBase> {
    if let (Some((dir, ident)), true) = (files, load) {
        match GeoBase::read_files(dir, ident, snap, cfg, true) {
            Ok(b) => return Ok(b),
            Err((_, Problem::Missing)) => {}
            Err((f, p)) => {
                tracing::warn!(
                    "spatial index file {}: {p}; it is built again",
                    dir.join(f).display()
                );
                snap.generation.geo.with_files(|| persist::remove(dir));
            }
        }
    }
    let base = build_base(snap, cfg, lookup, ctl, reuse)?;
    let Some((dir, ident)) = files else {
        return Ok(base);
    };
    let written = snap.generation.geo.with_files(|| {
        if (ctl.cancel)() {
            return None;
        }
        let r = base
            .write_files(dir, ident)
            .map_err(|e| e.to_string())
            .and_then(|()| {
                GeoBase::read_files(dir, ident, snap, cfg, false).map_err(|(_, p)| p.to_string())
            });
        if r.is_err() {
            persist::remove(dir);
        }
        Some(r)
    });
    match written.flatten() {
        Some(Ok(mut b)) => {
            b.opened = false;
            b.built_ms = base.built_ms;
            b.column.parsed = base.column.parsed;
            b.column.reused = base.column.reused;
            Ok(b)
        }
        Some(Err(e)) => {
            tracing::warn!(
                "cannot write the spatial index files of {}: {e}; the index stays in memory",
                snap.generation.name
            );
            Ok(base)
        }
        None => Ok(base),
    }
}

/// The view for a finished build of `epoch` on `snap` (whose generation the base
/// belongs to), with the status message and the build record.
#[cfg(feature = "geo")]
fn finish_view(
    idx: &GeoIndex,
    epoch: u64,
    snap: &Snapshot,
    lookup: Arc<Lookup>,
    built: std::thread::Result<Result<GeoBase>>,
) -> (
    GeoView,
    Option<String>,
    Option<crate::geo::config::GeoBuild>,
) {
    use crate::error::Error;
    let pending =
        |s: ViewState| GeoView::pending(idx.config.clone(), epoch, snap, lookup.clone(), s);
    let base = match built {
        Ok(Ok(b)) => b,
        Ok(Err(Error::BudgetExceeded(b))) => {
            let m = format!(
                "the index needs more than its budget of {} bytes (--geo-mb); queries run without it",
                b.limit
            );
            tracing::warn!("spatial index: {m}");
            return (pending(ViewState::OverBudget), Some(m), None);
        }
        Ok(Err(e)) => {
            tracing::error!("spatial index build failed: {e}");
            return (
                pending(ViewState::Failed),
                Some(format!("build failed: {e}")),
                None,
            );
        }
        Err(p) => {
            let m = panic_message(&*p);
            tracing::error!("spatial index build failed: {m}");
            return (
                pending(ViewState::Failed),
                Some(format!("build failed: {m}")),
                None,
            );
        }
    };
    let overlay = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        overlay_of(snap, &idx.config, &base, &lookup)
    }));
    let Ok(overlay) = overlay else {
        return (
            pending(ViewState::Failed),
            Some("build failed: the overlay could not be built".into()),
            None,
        );
    };
    let last = crate::geo::config::GeoBuild {
        at: crate::commit::rfc3339_ms(crate::commit::now_ms()),
        ms: base.built_ms,
        rows: base.rows.len() as u64,
    };
    let view = GeoView::ready(
        idx.config.clone(),
        epoch,
        snap.commit,
        lookup,
        Arc::new(base),
        overlay,
    );
    if view.bytes() > idx.budget {
        let m = format!(
            "the index needs more than its budget of {} bytes (--geo-mb); queries run without it",
            idx.budget
        );
        return (
            view.without_rows(ViewState::OverBudget),
            Some(m),
            Some(last),
        );
    }
    (view, None, Some(last))
}

/// Add a commit's inserted rows to the tail of `v` (whose base is usable), rebuilding
/// the overlay when the tail grows past its limit.
#[cfg(feature = "geo")]
fn index_commit(
    idx: &GeoIndex,
    v: &mut GeoView,
    snap: &Snapshot,
    before: &Snapshot,
    log: &[(u8, [Id; 4])],
) -> Result<()> {
    use crate::index::Perm;
    #[cfg(any(test, feature = "failpoints"))]
    if idx.fail_next.swap(false, Ordering::SeqCst) {
        panic!("injected spatial index failure");
    }
    #[cfg(not(any(test, feature = "failpoints")))]
    let _ = idx;
    let Some(base) = v.base.clone() else {
        return Ok(());
    };
    let cfg = v.config.clone();
    let ins = &snap.delta.ins[Perm::Pso.index()];
    let was = &before.delta.ins[Perm::Pso.index()];
    let mut seen: rustc_hash::FxHashSet<Row> = Default::default();
    for (op, q) in log {
        if *op != super::WAL_INSERT || v.lookup.slot(q[1], snap, &cfg).is_none() {
            continue;
        }
        let row = Row::of(q);
        let k = row.pso();
        // a quad already inserted before this commit has its row; re-adding a base quad
        // this commit had deleted needs none (the base row is valid again)
        if !ins.contains(&k) || was.contains(&k) || !v.lookup.graph(q[3], snap, &cfg) {
            continue;
        }
        if !seen.insert(row) {
            continue;
        }
        match base.column.get_or_classify(q[2], snap, &cfg) {
            crate::geo::column::Slot::Geom(entry) => v.tail.push_back(TailRow { row, entry }),
            s if s.rechecked() => v.skipped.push_back(row),
            _ => {}
        }
    }
    if v.tail.len() + v.skipped.len() > tail_limit(v.overlay.rows.len()) {
        let rows: Vec<TailRow> = v
            .overlay
            .rows
            .iter()
            .chain(v.tail.iter())
            .filter(|r| ins.contains(&r.row.pso()))
            .cloned()
            .collect();
        let skipped: Vec<Row> = v
            .overlay
            .skipped
            .iter()
            .chain(v.skipped.iter())
            .filter(|r| ins.contains(&r.pso()))
            .copied()
            .collect();
        v.overlay = Arc::new(Overlay::build(rows, skipped));
        v.tail = imbl::Vector::new();
        v.skipped = imbl::Vector::new();
    }
    Ok(())
}

#[cfg(feature = "geo")]
fn panic_message(p: &(dyn std::any::Any + Send)) -> String {
    p.downcast_ref::<&str>()
        .map(|s| s.to_string())
        .or_else(|| p.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "panic".into())
}

#[cfg(all(test, feature = "geo"))]
mod tests {
    use super::*;
    use crate::dataset::Dataset;
    use crate::geo::IndexState;
    use crate::geo::search::{self, SearchStats};
    use crate::io::RdfFormat;
    use crate::sparql::ctx::Ctx;
    use crate::sparql::plan::GraphFilter;
    use crate::store::StoreOptions;
    use std::collections::BTreeSet;

    pub(super) const GEO: &str = "http://www.opengis.net/ont/geosparql#";
    pub(super) const EX: &str = "http://example.org/";

    /// The fixture of the GeoSPARQL acceptance examples.
    pub(super) const FIXTURE: &str = r#"
@prefix ex: <http://example.org/> .
@prefix geo: <http://www.opengis.net/ont/geosparql#> .
ex:A geo:hasDefaultGeometry ex:gA . ex:gA geo:asWKT "POLYGON((0 0, 10 0, 10 10, 0 10, 0 0))"^^geo:wktLiteral .
ex:B geo:hasDefaultGeometry ex:gB . ex:gB geo:asWKT "POLYGON((5 5, 15 5, 15 15, 5 15, 5 5))"^^geo:wktLiteral .
ex:C geo:hasDefaultGeometry ex:gC . ex:gC geo:asWKT "POLYGON((10 0, 20 0, 20 10, 10 10, 10 0))"^^geo:wktLiteral .
ex:p1 geo:hasGeometry ex:g1 .  ex:g1 geo:asWKT "POINT(2 2)"^^geo:wktLiteral .
ex:p2 geo:hasGeometry ex:g2 .  ex:g2 geo:asWKT "<http://www.opengis.net/def/crs/EPSG/0/4326> POINT(2 12)"^^geo:wktLiteral .
ex:p3 geo:hasGeometry ex:g3 .  ex:g3 geo:asGeoJSON "{\"type\":\"Point\",\"coordinates\":[30,30]}"^^geo:geoJSONLiteral .
ex:bad geo:hasGeometry ex:gX . ex:gX geo:asWKT "POINT(1)"^^geo:wktLiteral .
ex:nil geo:hasGeometry ex:gE . ex:gE geo:asWKT ""^^geo:wktLiteral .
ex:mars geo:hasGeometry ex:gM . ex:gM geo:asWKT "<http://example.org/crs/mars> POINT(1 1)"^^geo:wktLiteral .
ex:G1 { ex:p4 geo:hasGeometry ex:g4 . ex:g4 geo:asWKT "POINT(3 3)"^^geo:wktLiteral . }
"#;

    pub(super) const WORLD: [f64; 4] = [-180.0, -90.0, 180.0, 90.0];
    pub(super) const NEAR: [f64; 4] = [0.5, 0.5, 2.5, 2.5];

    pub(super) fn opts() -> StoreOptions {
        StoreOptions::default()
    }

    /// An in-memory dataset holding the fixture in its base.
    pub(super) fn fixture(o: StoreOptions) -> Dataset {
        let ds = Dataset::from_store(Store::in_memory(o));
        ds.load_str(FIXTURE, RdfFormat::TriG).unwrap();
        ds
    }

    pub(super) fn update(ds: &Dataset, op: &str, triples: &str) {
        ds.update(&format!(
            "PREFIX geo: <{GEO}> PREFIX ex: <{EX}> {op} DATA {{ {triples} }}"
        ))
        .unwrap();
    }

    pub(super) fn preds(snap: &Snapshot) -> Vec<Id> {
        ["asWKT", "asGeoJSON", "hasSerialization"]
            .iter()
            .filter_map(|p| snap.lookup_iri(&format!("{GEO}{p}")))
            .collect()
    }

    /// Local names of the subjects of the rows in window `w`, and the statistics.
    pub(super) fn window(
        snap: Arc<Snapshot>,
        w: [f64; 4],
        graph: GraphFilter,
    ) -> (BTreeSet<String>, SearchStats, usize) {
        let ctx = Ctx::new(snap.clone());
        let mut st = SearchStats::default();
        let mut out = BTreeSet::new();
        let mut n = 0;
        search::window(&ctx, &preds(&snap), &[w], &graph, &mut st, &mut |hits| {
            for h in hits {
                let s = snap.term(h.s).unwrap().to_string();
                out.insert(
                    s.trim_matches(['<', '>'])
                        .trim_start_matches(EX)
                        .to_string(),
                );
                assert!(h.entry.geom(&snap).unwrap().vertices > 0);
                n += 1;
            }
            Ok(())
        })
        .unwrap();
        (out, st, n)
    }

    pub(super) fn names(v: &[&str]) -> BTreeSet<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    fn state(ds: &Dataset) -> IndexState {
        ds.snapshot().geo.as_ref().unwrap().state()
    }

    /// The snapshot without its index view (the searches then scan).
    pub(super) fn unindexed(snap: &Snapshot) -> Arc<Snapshot> {
        let mut s = snap.clone();
        s.geo = None;
        Arc::new(s)
    }

    #[test]
    fn status_after_load() {
        let ds = fixture(opts());
        let s = ds.store().enable_geo(GeoConfig::default()).unwrap();
        assert_eq!(s.state, "ready");
        assert_eq!((s.rows.base, s.rows.overlay, s.rows.tail), (7, 0, 0));
        assert_eq!(s.literals, 7);
        assert_eq!(
            (
                s.skipped.malformed,
                s.skipped.unknown_crs,
                s.skipped.too_large,
                s.skipped.empty
            ),
            (1, 1, 0, 1)
        );
        assert_eq!(s.crs.get("http://example.org/crs/mars"), Some(&1));
        assert_eq!(s.crs[crate::geo::crs::CRS84_IRI], 6);
        assert_eq!(s.crs["http://www.opengis.net/def/crs/EPSG/0/4326"], 1);
        assert_eq!(s.crs.len(), 3);
        assert!(s.last_build.is_some() && s.memory.tree_bytes > 0);
        assert_eq!(s.memory.budget_bytes, 4 << 30);
        let j = serde_json::to_value(&s).unwrap();
        assert_eq!(j["enabled"], true);
        assert_eq!(j["formatVersion"], 1);
        // every indexed geometry, in every graph; g4 only in its graph
        let (all, st, _) = window(ds.snapshot(), WORLD, GraphFilter::All);
        assert_eq!(all, names(&["gA", "gB", "gC", "g1", "g2", "g3", "g4"]));
        assert!(!st.fallback && st.nodes > 0 && st.candidates == 7);
        let (dflt, _, _) = window(ds.snapshot(), WORLD, GraphFilter::Default);
        assert!(!dflt.contains("g4") && dflt.len() == 6);
        // g2 is at longitude 12, latitude 2
        let (w, _, _) = window(ds.snapshot(), [11.0, 1.0, 13.0, 3.0], GraphFilter::All);
        assert_eq!(w, names(&["gC", "g2"]));
        let v = ds.snapshot().geo.clone().unwrap();
        let wkt = ds.snapshot().lookup_iri(&format!("{GEO}asWKT")).unwrap();
        assert_eq!(v.predicate_slot(wkt), Some(0));
        assert!(v.estimate(&[0], &[WORLD]) >= 6.0);
        assert!(v.levels() >= 1);
        ds.store().disable_geo().unwrap();
        assert!(ds.store().geo_status().is_none() && ds.snapshot().geo.is_none());
        // without an index, the same rows by a scan
        let (all2, st, _) = window(ds.snapshot(), WORLD, GraphFilter::All);
        assert_eq!(all, all2);
        assert!(st.fallback);
    }

    #[test]
    fn commits_and_snapshots() {
        let ds = fixture(opts());
        ds.store().enable_geo(GeoConfig::default()).unwrap();
        let reader = ds.snapshot();
        update(
            &ds,
            "INSERT",
            r#"ex:p5 geo:hasGeometry ex:g5 . ex:g5 geo:asWKT "POINT(1 1)"^^geo:wktLiteral"#,
        );
        let near = |s| window(s, NEAR, GraphFilter::All).0;
        assert_eq!(near(ds.snapshot()), names(&["gA", "g1", "g5"]));
        assert_eq!(near(reader.clone()), names(&["gA", "g1"]));
        // a configured predicate the base does not have
        update(
            &ds,
            "INSERT",
            r#"ex:g7 geo:hasSerialization "POINT(30 30)"^^geo:wktLiteral"#,
        );
        assert_eq!(
            window(ds.snapshot(), [29.0, 29.0, 31.0, 31.0], GraphFilter::All).0,
            names(&["g3", "g7"])
        );
        update(
            &ds,
            "DELETE",
            r#"ex:g1 geo:asWKT "POINT(2 2)"^^geo:wktLiteral"#,
        );
        let (now, st, _) = window(ds.snapshot(), NEAR, GraphFilter::All);
        assert_eq!(now, names(&["gA", "g5"]));
        assert!(!st.fallback);
        assert_eq!(near(reader), names(&["gA", "g1"]));
        let s = ds.store().geo_status().unwrap();
        assert_eq!((s.rows.base, s.rows.overlay, s.rows.tail), (7, 0, 2));
        assert_eq!(s.literals, 9);
        // deleting and inserting a quad again leaves one row
        for op in ["DELETE", "INSERT", "DELETE", "INSERT"] {
            update(&ds, op, r#"ex:g5 geo:asWKT "POINT(1 1)"^^geo:wktLiteral"#);
        }
        assert_eq!(window(ds.snapshot(), NEAR, GraphFilter::All).2, 2);
        // a compaction moves the rows into the base and keeps the epoch
        let epoch = ds.snapshot().geo.as_ref().unwrap().epoch;
        ds.compact().unwrap();
        let s = ds.store().geo_status().unwrap();
        assert_eq!(
            (s.state.as_str(), s.rows.base, s.rows.overlay, s.rows.tail),
            ("ready", 8, 0, 0)
        );
        assert_eq!(ds.snapshot().geo.as_ref().unwrap().epoch, epoch);
        assert_eq!(near(ds.snapshot()), names(&["gA", "g5"]));
        // a rebuild bumps it
        ds.store().rebuild_geo().unwrap();
        assert_eq!(ds.snapshot().geo.as_ref().unwrap().epoch, epoch + 1);
        assert_eq!(near(ds.snapshot()), names(&["gA", "g5"]));
    }

    /// Splitmix64, for reproducible random data.
    pub(super) fn rng(seed: &mut u64) -> f64 {
        *seed = seed.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = *seed;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        ((z ^ (z >> 31)) >> 11) as f64 / (1u64 << 53) as f64
    }

    #[test]
    fn many_commits_match_a_scan() {
        let ds = fixture(opts());
        ds.store().enable_geo(GeoConfig::default()).unwrap();
        let mut seed = 7;
        let mut pts: Vec<(usize, f64, f64)> = Vec::new();
        for c in 0..100 {
            let mut q = String::new();
            for i in 0..100 {
                let n = c * 100 + i;
                let (x, y) = (
                    rng(&mut seed) * 360.0 - 180.0,
                    rng(&mut seed) * 180.0 - 90.0,
                );
                q += &format!("ex:r{n} geo:asWKT \"POINT({x} {y})\"^^geo:wktLiteral . ");
                pts.push((n, x, y));
            }
            update(&ds, "INSERT", &q);
        }
        let mut q = String::new();
        for (n, x, y) in pts.iter().step_by(10) {
            q += &format!("ex:r{n} geo:asWKT \"POINT({x} {y})\"^^geo:wktLiteral . ");
        }
        update(&ds, "DELETE", &q);
        let s = ds.store().geo_status().unwrap();
        assert!(s.rows.overlay > 4096, "{:?}", s.rows);
        assert_eq!(s.rows.base, 7);
        let snap = ds.snapshot();
        let scan = unindexed(&snap);
        for _ in 0..200 {
            let (x, y) = (
                rng(&mut seed) * 340.0 - 170.0,
                rng(&mut seed) * 160.0 - 80.0,
            );
            let (w, h) = (rng(&mut seed) * 40.0, rng(&mut seed) * 20.0);
            let b = [x, y, x + w, y + h];
            let (a, st, _) = window(snap.clone(), b, GraphFilter::All);
            let (e, sf, _) = window(scan.clone(), b, GraphFilter::All);
            assert!(!st.fallback && sf.fallback);
            assert_eq!(a, e, "{b:?}");
        }
    }

    #[test]
    fn paused_build_answers_by_scanning() {
        let ds = fixture(opts());
        ds.store().pause_geo_build(true);
        let s = ds.store().enable_geo(GeoConfig::default()).unwrap();
        assert_eq!(s.state, "building");
        assert!(matches!(state(&ds), IndexState::Building(_)));
        let (rows, st, _) = window(ds.snapshot(), WORLD, GraphFilter::All);
        assert!(st.fallback);
        // commits go on meanwhile, with a predicate the build did not know
        update(
            &ds,
            "INSERT",
            r#"ex:g5 geo:asWKT "POINT(1 1)"^^geo:wktLiteral .
            ex:g7 geo:hasSerialization "POINT(1.5 1.5)"^^geo:wktLiteral"#,
        );
        assert!(matches!(state(&ds), IndexState::Building(_)));
        ds.store().pause_geo_build(false);
        let s = ds.store().wait_geo().unwrap();
        assert_eq!(s.state, "ready");
        assert_eq!((s.rows.base, s.rows.overlay, s.rows.tail), (7, 2, 0));
        let (after, st, _) = window(ds.snapshot(), WORLD, GraphFilter::All);
        assert!(!st.fallback);
        let mut expected = rows;
        expected.insert("g5".into());
        expected.insert("g7".into());
        assert_eq!(after, expected);
    }

    #[test]
    fn failures_fall_back_to_scans() {
        let ds = fixture(opts());
        ds.store().enable_geo(GeoConfig::default()).unwrap();
        ds.store().fail_next_geo_commit();
        update(
            &ds,
            "INSERT",
            r#"ex:g5 geo:asWKT "POINT(1 1)"^^geo:wktLiteral"#,
        );
        assert_eq!(state(&ds), IndexState::Failed);
        let s = ds.store().geo_status().unwrap();
        assert_eq!(s.state, "failed");
        let m = s.message.unwrap();
        assert!(m.contains("injected"), "{m}");
        let (rows, st, _) = window(ds.snapshot(), NEAR, GraphFilter::All);
        assert!(st.fallback);
        assert_eq!(rows, names(&["gA", "g1", "g5"]));
        // later commits keep the state; a rebuild recovers
        update(
            &ds,
            "INSERT",
            r#"ex:g6 geo:asWKT "POINT(2.2 2.2)"^^geo:wktLiteral"#,
        );
        assert_eq!(state(&ds), IndexState::Failed);
        let s = ds.store().rebuild_geo().unwrap();
        assert_eq!(s.state, "ready");
        assert!(s.message.is_none());
        let (rows, st, _) = window(ds.snapshot(), NEAR, GraphFilter::All);
        assert!(!st.fallback);
        assert_eq!(rows, names(&["gA", "g1", "g5", "g6"]));
    }

    #[test]
    fn over_budget_falls_back_to_scans() {
        let ds = fixture(StoreOptions {
            geo_budget_bytes: 300,
            ..opts()
        });
        let s = ds.store().enable_geo(GeoConfig::default()).unwrap();
        assert_eq!(s.state, "over-budget");
        assert!(s.message.unwrap().contains("budget"));
        let (rows, st, _) = window(ds.snapshot(), WORLD, GraphFilter::All);
        assert!(st.fallback && rows.len() == 7);
        update(
            &ds,
            "INSERT",
            r#"ex:g5 geo:asWKT "POINT(1 1)"^^geo:wktLiteral"#,
        );
        assert_eq!(state(&ds), IndexState::OverBudget);
    }

    #[test]
    fn transactions_see_their_own_rows() {
        let ds = fixture(opts());
        ds.store().enable_geo(GeoConfig::default()).unwrap();
        let mut txn = ds.store().write();
        let quad = oxrdf::Quad::new(
            oxrdf::NamedNode::new_unchecked(format!("{EX}g5")),
            oxrdf::NamedNode::new_unchecked(format!("{GEO}asWKT")),
            oxrdf::Literal::new_typed_literal(
                "POINT(1 1)",
                oxrdf::NamedNode::new_unchecked(crate::geo::WKT_LITERAL),
            ),
            oxrdf::GraphName::DefaultGraph,
        );
        let q = txn.encode_quad(&quad, &mut Default::default()).unwrap();
        txn.insert(q).unwrap();
        let view = Arc::new(txn.view());
        assert_eq!(view.geo.as_ref().unwrap().state(), IndexState::Txn);
        let (rows, st, _) = window(view, NEAR, GraphFilter::All);
        assert!(st.fallback);
        assert_eq!(rows, names(&["gA", "g1", "g5"]));
        txn.commit().unwrap();
        let (rows, st, _) = window(ds.snapshot(), NEAR, GraphFilter::All);
        assert!(!st.fallback);
        assert_eq!(rows, names(&["gA", "g1", "g5"]));
    }

    #[test]
    fn reopen_replays_the_log_into_the_overlay() {
        let dir = tempfile::tempdir().unwrap();
        {
            let ds = Dataset::open_with(dir.path(), opts()).unwrap();
            ds.load_str(FIXTURE, RdfFormat::TriG).unwrap();
            ds.store().enable_geo(GeoConfig::default()).unwrap();
            update(
                &ds,
                "INSERT",
                r#"ex:g5 geo:asWKT "POINT(1 1)"^^geo:wktLiteral"#,
            );
            update(
                &ds,
                "DELETE",
                r#"ex:g1 geo:asWKT "POINT(2 2)"^^geo:wktLiteral"#,
            );
            assert!(dir.path().join(crate::geo::CONFIG_FILE).exists());
        }
        let ds = Dataset::open_with(dir.path(), opts()).unwrap();
        let s = ds.store().wait_geo().unwrap();
        assert_eq!(s.state, "ready");
        assert_eq!((s.rows.base, s.rows.overlay, s.rows.tail), (7, 1, 0));
        let (rows, st, _) = window(ds.snapshot(), NEAR, GraphFilter::All);
        assert!(!st.fallback);
        assert_eq!(rows, names(&["gA", "g5"]));
        // a past state has the configuration but no index
        let head = ds.head_commit().seq;
        let (past, _) = ds
            .store()
            .snapshot_at(&crate::history::At::Commit(head - 1), &Default::default())
            .unwrap();
        assert_eq!(past.geo.as_ref().unwrap().state(), IndexState::Historical);
        let (rows, st, _) = window(past, NEAR, GraphFilter::All);
        assert!(st.fallback);
        assert_eq!(rows, names(&["gA", "g1", "g5"]));
        ds.store().disable_geo().unwrap();
        assert!(!dir.path().join(crate::geo::CONFIG_FILE).exists());
    }

    #[test]
    fn nearest_first() {
        let ds = fixture(opts());
        ds.store().enable_geo(GeoConfig::default()).unwrap();
        update(
            &ds,
            "INSERT",
            r#"ex:g5 geo:asWKT "POINT(1 1)"^^geo:wktLiteral"#,
        );
        let q = crate::geo::Geom::from_geometry(
            crate::geo::crs::CrsRef::Known(crate::geo::crs::CRS84),
            georust::Geometry::Point(georust::Point::new(2.0, 2.0)),
        );
        for snap in [ds.snapshot(), unindexed(&ds.snapshot())] {
            let ctx = Ctx::new(snap.clone());
            let mut seen = Vec::new();
            let mut last = 0.0;
            let mut st = SearchStats::default();
            search::nearest(
                &ctx,
                &preds(&snap),
                &q,
                &GraphFilter::All,
                &mut st,
                &mut |hits, bound| {
                    assert!(bound >= last);
                    last = bound;
                    seen.extend(hits.iter().map(|h| h.s));
                    Ok(true)
                },
            )
            .unwrap();
            assert_eq!(seen.len(), 8);
            assert_eq!(seen.iter().collect::<BTreeSet<_>>().len(), 8);
            // with a lower bound that tells points apart, the farthest row comes last
            if crate::geo::ops::distance::lower_bound_m([2.0, 2.0], [30.0, 30.0, 30.0, 30.0]) > 0.0
            {
                let g3 = snap.lookup_iri(&format!("{EX}g3")).unwrap();
                assert_eq!(seen.last(), Some(&g3));
            }
            assert_eq!(st.fallback, snap.geo.is_none());
            // the sink stops the search
            let mut calls = 0;
            search::nearest(
                &ctx,
                &preds(&snap),
                &q,
                &GraphFilter::All,
                &mut SearchStats::default(),
                &mut |_, _| {
                    calls += 1;
                    Ok(false)
                },
            )
            .unwrap();
            assert_eq!(calls, 1);
        }
    }

    /// Commit latency with the index on. Rough; run it in release on a quiet machine:
    /// `cargo test --release -p sparkles --features geo commit_latency -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn commit_latency() {
        let mut seed = 1;
        let mut time = |ds: &Dataset, n: usize, geo: bool| {
            let mut q = String::new();
            for _ in 0..n {
                let k = rng(&mut seed);
                q += &if geo {
                    format!(
                        "ex:t{k} geo:asWKT \"POINT({} {})\"^^geo:wktLiteral . ",
                        k * 10.0,
                        k * 5.0
                    )
                } else {
                    format!("ex:t{k} ex:label \"{k}\" . ")
                };
            }
            let t = std::time::Instant::now();
            update(ds, "INSERT", &q);
            t.elapsed().as_secs_f64() * 1000.0
        };
        let median = |mut v: Vec<f64>| {
            v.sort_by(f64::total_cmp);
            v[v.len() / 2]
        };
        let plain = fixture(opts());
        let indexed = fixture(opts());
        indexed.store().enable_geo(GeoConfig::default()).unwrap();
        let mut runs = |ds: &Dataset, k: usize, n: usize, geo: bool| {
            median((0..k).map(|_| time(ds, n, geo)).collect())
        };
        let one_off = runs(&plain, 200, 1, false);
        let one_on = runs(&indexed, 200, 1, false);
        let pts_off = runs(&plain, 20, 1000, true);
        let pts_on = runs(&indexed, 20, 1000, true);
        println!(
            "1-triple non-geo commit: {one_off:.3} ms without the index, {one_on:.3} ms with it; \
             1000 points: {pts_off:.3} ms without, {pts_on:.3} ms with (+{:.3} ms)",
            pts_on - pts_off
        );
        assert!(one_on < one_off * 1.5 + 0.05);
        assert!(pts_on - pts_off < 1.0 + pts_off * 0.5);
    }
}

#[cfg(all(test, feature = "geo"))]
#[path = "geo_files_tests.rs"]
mod files_tests;
