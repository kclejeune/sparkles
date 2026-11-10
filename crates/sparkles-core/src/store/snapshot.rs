//! Building a [`Snapshot`] from a base, setting only what a path changes.
//!
//! Every path that makes a snapshot starts either from an opened generation, as the
//! store's open, a bulk rebuild, a compaction and a past state do, or from the previous
//! snapshot, as a commit and an uncommitted view do. [`SnapshotBuilder`] holds the
//! defaults of each start, so a new field of the snapshot is added here once, with its
//! default, instead of in every literal. The fields a path changes are set through the
//! builder's setters.

use super::*;

/// What every snapshot of one store shares: the dataset's identity, its block and
/// result caches, the settings that shape its queries and its change log.
pub(crate) struct Shared<'a> {
    pub dataset_id: uuid::Uuid,
    pub cache: &'a Arc<BlockCache>,
    pub results: &'a Arc<crate::sparql::cache::ResultCache>,
    pub union_default_graph: bool,
    pub geo_op_vertices: u64,
    pub change_log: Option<Arc<ChangeLog>>,
}

/// A [`Snapshot`] under construction (see the module documentation).
pub(crate) struct SnapshotBuilder(Snapshot);

impl SnapshotBuilder {
    /// The state of an opened `generation` at `commit`: its base delta is empty, its
    /// delta vocabulary is read as it is now, it has no index views and no mask, it is
    /// live and cacheable, and its version is 0. It shares the caches, settings and change
    /// log of `shared`.
    pub(crate) fn of_generation(
        shared: Shared<'_>,
        generation: Arc<Generation>,
        commit: u64,
    ) -> SnapshotBuilder {
        let dvocab_len = generation.dvocab.len();
        SnapshotBuilder(Snapshot {
            dataset_id: shared.dataset_id,
            generation,
            delta: Delta::default(),
            version: 0,
            cache: shared.cache.clone(),
            results: shared.results.clone(),
            cacheable: true,
            dvocab_len,
            commit,
            text: None,
            geo: None,
            union_default_graph: shared.union_default_graph,
            geo_op_vertices: shared.geo_op_vertices,
            historical: false,
            counts: Default::default(),
            mask: None,
            change_log: shared.change_log,
        })
    }

    /// A state of `base`'s generation at `base`'s version and commit that shares its
    /// caches, index views, settings and change log. Its delta is empty, its delta
    /// vocabulary is read as it is now, since a state made from the live one names the
    /// terms added since, its counts are not yet worked out, it has no mask, and it is
    /// live and cacheable.
    pub(crate) fn of_base(base: &Snapshot) -> SnapshotBuilder {
        SnapshotBuilder(Snapshot {
            dataset_id: base.dataset_id,
            generation: base.generation.clone(),
            delta: Delta::default(),
            version: base.version,
            cache: base.cache.clone(),
            results: base.results.clone(),
            cacheable: true,
            dvocab_len: base.generation.dvocab.len(),
            commit: base.commit,
            text: base.text.clone(),
            geo: base.geo.clone(),
            union_default_graph: base.union_default_graph,
            geo_op_vertices: base.geo_op_vertices,
            historical: false,
            counts: Default::default(),
            mask: None,
            change_log: base.change_log.clone(),
        })
    }

    /// The state after `base`, which commit `commit` makes: [`of_base`](Self::of_base)
    /// at the next version.
    pub(crate) fn after(base: &Snapshot, commit: u64) -> SnapshotBuilder {
        Self::of_base(base).version(base.version + 1).commit(commit)
    }

    pub(crate) fn delta(mut self, delta: Delta) -> SnapshotBuilder {
        self.0.delta = delta;
        self
    }

    pub(crate) fn version(mut self, version: u64) -> SnapshotBuilder {
        self.0.version = version;
        self
    }

    pub(crate) fn commit(mut self, commit: u64) -> SnapshotBuilder {
        self.0.commit = commit;
        self
    }

    /// Whether committed-view identity keys the result and projection caches (see
    /// [`Snapshot::cacheable`]).
    pub(crate) fn cacheable(mut self, cacheable: bool) -> SnapshotBuilder {
        self.0.cacheable = cacheable;
        self
    }

    pub(crate) fn results(
        mut self,
        results: Arc<crate::sparql::cache::ResultCache>,
    ) -> SnapshotBuilder {
        self.0.results = results;
        self
    }

    pub(crate) fn dvocab_len(mut self, dvocab_len: u64) -> SnapshotBuilder {
        self.0.dvocab_len = dvocab_len;
        self
    }

    pub(crate) fn text(mut self, text: Option<Arc<crate::text::TextView>>) -> SnapshotBuilder {
        self.0.text = text;
        self
    }

    pub(crate) fn geo(mut self, geo: Option<Arc<crate::geo::GeoView>>) -> SnapshotBuilder {
        self.0.geo = geo;
        self
    }

    /// A past state, not the live one (see [`Snapshot::historical`]).
    pub(crate) fn historical(mut self, historical: bool) -> SnapshotBuilder {
        self.0.historical = historical;
        self
    }

    pub(crate) fn change_log(mut self, change_log: Option<Arc<ChangeLog>>) -> SnapshotBuilder {
        self.0.change_log = change_log;
        self
    }

    pub(crate) fn build(self) -> Snapshot {
        self.0
    }
}

impl Store {
    /// What this store's snapshots share (see [`Shared`]).
    pub(super) fn shared(&self) -> Shared<'_> {
        Shared {
            dataset_id: self.owner_dataset_id(),
            cache: &self.cache,
            results: &self.results,
            union_default_graph: self.opts.union_default_graph,
            geo_op_vertices: self.opts.geo_op_vertices,
            change_log: self.changelog.clone(),
        }
    }

    /// [`SnapshotBuilder::of_generation`] for a generation of this store.
    pub(super) fn snapshot_of(&self, generation: Arc<Generation>, commit: u64) -> SnapshotBuilder {
        SnapshotBuilder::of_generation(self.shared(), generation, commit)
    }
}
