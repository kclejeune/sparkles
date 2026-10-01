//! The spatial index's place in the store: enabling and disabling it, its status, and
//! the hooks that keep it in step with commits, generation switches and opening.
//!
//! Stub: the index cannot be enabled yet; the hooks do nothing.

use super::{Snapshot, Store};
use crate::error::Result;
use crate::geo::{GeoConfig, GeoStatus, GeoView};
use crate::id::Id;
use std::sync::Arc;

impl Store {
    /// Enable (or reconfigure) the spatial index and build it from the current state.
    /// The configuration is kept in `geo.json`.
    pub fn enable_geo(&self, c: GeoConfig) -> Result<GeoStatus> {
        c.validate()?;
        #[cfg(not(feature = "geo"))]
        return Err(crate::geo::not_built());
        #[cfg(feature = "geo")]
        Err(crate::error::Error::Unsupported(
            "the spatial index is not supported yet".into(),
        ))
    }

    /// Turn the spatial index off: queries run without it, and `geo.json` is removed.
    pub fn disable_geo(&self) -> Result<()> {
        self.geo.store(None);
        if let Some(root) = &self.root {
            match std::fs::remove_file(root.join(crate::geo::CONFIG_FILE)) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e.into()),
            }
        }
        Ok(())
    }

    /// Rebuild the spatial index of the current generation from RDF.
    pub fn rebuild_geo(&self) -> Result<GeoStatus> {
        if !self.geo_enabled() {
            return Err(crate::error::Error::invalid("spatial index is not enabled"));
        }
        #[cfg(not(feature = "geo"))]
        return Err(crate::geo::not_built());
        #[cfg(feature = "geo")]
        Err(crate::error::Error::Unsupported(
            "the spatial index is not supported yet".into(),
        ))
    }

    /// Spatial index status (`None`: not enabled).
    pub fn geo_status(&self) -> Option<GeoStatus> {
        let _ = self.geo.load();
        None
    }

    /// Whether the spatial index is enabled.
    pub fn geo_enabled(&self) -> bool {
        self.geo.load().is_some()
    }

    /// Test hook: hold (or release) background builds of the spatial index before they
    /// publish, so tests see the index while it is building.
    #[cfg(any(test, feature = "failpoints"))]
    pub fn pause_geo_build(&self, on: bool) {
        let _ = on;
    }

    /// At open: start the spatial index if `geo.json` enables it.
    pub(super) fn open_geo(&self) {
        #[cfg(not(feature = "geo"))]
        if let Some(root) = &self.root
            && root.join(crate::geo::CONFIG_FILE).exists()
        {
            tracing::warn!(
                "{}: a spatial index is configured but this build has no `geo` feature",
                root.display()
            );
        }
    }

    /// In the commit path, before `snap` is published: index the geometries of the
    /// commit's logged quads and give `snap` its view.
    pub(super) fn maintain_geo(&self, snap: &mut Snapshot, log: &[(u8, [Id; 4])]) {
        let _ = (snap, log);
    }

    /// After a bulk commit or a compaction, under the writer lock and before `new` is
    /// published: build the new generation's base (`prev` is the snapshot it replaces).
    pub(super) fn rebuild_geo_locked(&self, new: &mut Snapshot, prev: &Snapshot) {
        let _ = (new, prev);
    }

    /// The view of a past state (`?at=`): the configuration, never ready.
    pub(super) fn historical_geo(&self) -> Option<Arc<GeoView>> {
        None
    }
}
