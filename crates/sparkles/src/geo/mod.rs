//! GeoSPARQL: geometry literals, the `geof:` functions, the Jena `spatial:` property
//! functions and a spatial index per dataset.
//!
//! The vocabulary, the configuration (`geo.json`), and the CRS and unit tables are pure
//! data and always compiled, so the planner recognizes GeoSPARQL terms in any build. The
//! geometry engine (the `geo` crate family) and the index need the cargo feature `geo`;
//! without it, `geof:` calls are unknown functions (a type error, with a plan warning)
//! and `spatial:` property functions fail with [`not_built`].
//!
//! Geometries are held in internal (east, north) order: longitude, latitude for
//! geographic CRSs, whatever the literal's own axis order.

pub mod config;
pub mod crs;
pub mod units;
mod validate;
pub mod vocab;

#[cfg(feature = "geo")]
pub mod aggregates;
#[cfg(feature = "geo")]
pub mod column;
#[cfg(feature = "geo")]
pub mod convert;
#[cfg(feature = "geo")]
pub mod exec;
#[cfg(feature = "geo")]
pub mod functions;
#[cfg(feature = "geo")]
pub mod geom;
#[cfg(feature = "geo")]
pub mod index;
#[cfg(feature = "geo")]
pub mod join;
#[cfg(feature = "geo")]
pub mod knn;
#[cfg(feature = "geo")]
pub mod map;
#[cfg(feature = "geo")]
pub mod memo;
#[cfg(feature = "geo")]
pub mod ops;
#[cfg(feature = "geo")]
pub mod parse;
#[cfg(feature = "geo")]
pub mod persist;
#[cfg(feature = "geo")]
pub mod probe;
#[cfg(feature = "geo")]
pub mod rewrite;
#[cfg(feature = "geo")]
pub mod search;
#[cfg(feature = "geo")]
pub mod spatialf;
#[cfg(feature = "geo")]
pub(crate) mod tree;
#[cfg(feature = "geo")]
pub mod write;

#[cfg(all(test, feature = "geo"))]
mod smoke_tests;

/// The per-query geometry memo (nothing to remember without the feature).
#[cfg(not(feature = "geo"))]
pub mod memo {
    #[derive(Default)]
    pub struct GeoMemo;
}

pub use config::{DistanceModel, GeoConfig, GeoStatus, IndexState};
pub use validate::validate_query;
pub use vocab::{GEOJSON_LITERAL, Relation, SpatialPfKind, WKT_LITERAL};

#[cfg(feature = "geo")]
pub use geom::{Geom, GeomError, GeomType, Layout};
#[cfg(feature = "geo")]
pub use index::{GenerationGeo, GeoIndex, GeoView};
#[cfg(feature = "geo")]
pub use parse::{parse, parse_limited};
#[cfg(feature = "geo")]
pub use write::{literal, to_geojson, to_wkt};

#[cfg(not(feature = "geo"))]
pub use off::{GenerationGeo, GeoIndex, GeoView};

/// A parsed geometry shared between the index, the per-query memo and plans.
#[cfg(feature = "geo")]
pub type GeomRef = std::sync::Arc<geom::Geom>;
/// A parsed geometry (none exist without the feature).
#[cfg(not(feature = "geo"))]
pub type GeomRef = std::convert::Infallible;

use crate::error::Error;
use std::path::Path;

/// Error for a build without the `geo` feature.
pub fn not_built() -> Error {
    Error::Unsupported("built without GeoSPARQL (cargo feature \"geo\")".into())
}

/// The configuration file of a dataset's spatial index.
pub const CONFIG_FILE: &str = "geo.json";

/// Read `<root>/geo.json` (`None`: the index is not enabled).
pub fn read_config(root: &Path) -> crate::error::Result<Option<GeoConfig>> {
    match std::fs::read(root.join(CONFIG_FILE)) {
        Ok(b) => serde_json::from_slice(&b)
            .map(Some)
            .map_err(|e| Error::Corrupt(format!("{CONFIG_FILE}: {e}"))),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// 64-bit FNV-1a, for configuration hashes and result-cache keys (stable across runs
/// and builds, unlike `std`'s hasher).
#[derive(Clone, Copy, Debug)]
pub struct Fnv(u64);

impl Fnv {
    pub fn new() -> Fnv {
        Fnv(0xcbf2_9ce4_8422_2325)
    }
    pub fn bytes(&mut self, b: &[u8]) {
        for &x in b {
            self.0 ^= u64::from(x);
            self.0 = self.0.wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
    /// A length-prefixed field, so `("ab", "c")` and `("a", "bc")` differ.
    pub fn field(&mut self, b: &[u8]) {
        self.bytes(&(b.len() as u64).to_le_bytes());
        self.bytes(b);
    }
    pub fn finish(self) -> u64 {
        self.0
    }
}

impl Default for Fnv {
    fn default() -> Self {
        Fnv::new()
    }
}

/// What [`probe`] found for a dataset's spatial index (read only, for `sparkles check`).
#[derive(Debug, Default)]
pub struct GeoProbe {
    /// `geo.json` exists
    pub configured: bool,
    /// `geo.json` does not parse, or does not validate
    pub config_error: Option<String>,
    /// this build has no GeoSPARQL support: only the configuration was read
    pub unsupported: bool,
    pub config: Option<GeoConfig>,
    /// problems of the index's own files, as (file, problem)
    pub damaged: Vec<(String, String)>,
    /// the index files that are good, and their bytes
    pub files: Vec<String>,
    pub file_bytes: u64,
}

/// Inspect the spatial index of the database at `root` without opening it.
pub fn probe(root: &Path, checksums: bool) -> GeoProbe {
    let mut p = GeoProbe::default();
    match read_config(root) {
        Ok(None) => return p,
        Ok(Some(c)) => {
            p.configured = true;
            if let Err(e) = c.validate() {
                p.config_error = Some(e.to_string());
            }
            p.config = Some(c);
        }
        Err(e) => {
            p.configured = true;
            p.config_error = Some(e.to_string());
            return p;
        }
    }
    p.unsupported = !cfg!(feature = "geo");
    #[cfg(feature = "geo")]
    probe::files(root, checksums, &mut p);
    #[cfg(not(feature = "geo"))]
    let _ = checksums;
    p
}

/// Stand-ins for the index types in a build without the feature: a snapshot never has
/// a view, a store never has an index.
#[cfg(not(feature = "geo"))]
mod off {
    use super::config::{GeoConfig, IndexState};
    use std::sync::Arc;

    #[derive(Default)]
    pub struct GenerationGeo;

    pub struct GeoIndex;

    #[derive(Clone)]
    pub struct GeoView {
        pub config: Arc<GeoConfig>,
        pub epoch: u64,
        pub generation: u64,
        pub commit: u64,
    }

    impl GeoView {
        pub fn state(&self) -> IndexState {
            IndexState::Off
        }
        pub fn for_txn(&self) -> GeoView {
            self.clone()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fnv_reference_values() {
        // published FNV-1a 64 test vectors
        assert_eq!(Fnv::new().finish(), 0xcbf2_9ce4_8422_2325);
        let mut h = Fnv::new();
        h.bytes(b"a");
        assert_eq!(h.finish(), 0xaf63_dc4c_8601_ec8c);
        let mut h = Fnv::new();
        h.bytes(b"foobar");
        assert_eq!(h.finish(), 0x8594_4171_f739_67e8);
    }

    #[test]
    fn probe_reads_the_configuration() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!probe(dir.path(), false).configured);
        std::fs::write(dir.path().join(CONFIG_FILE), b"{}").unwrap();
        let p = probe(dir.path(), false);
        assert!(p.configured && p.config_error.is_none());
        assert_eq!(p.unsupported, !cfg!(feature = "geo"));
        std::fs::write(dir.path().join(CONFIG_FILE), br#"{"queryRewrite":true}"#).unwrap();
        let p = probe(dir.path(), false);
        assert!(p.config_error.unwrap().contains("queryRewrite"));
        std::fs::write(dir.path().join(CONFIG_FILE), b"{").unwrap();
        assert!(probe(dir.path(), false).config_error.is_some());
    }
}
