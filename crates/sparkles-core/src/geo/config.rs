//! The spatial index configuration of a dataset (`<root>/geo.json`) and its status
//! (`GET /$/geo/{ds}`).

use super::vocab;
use crate::error::{Error, Result};
use crate::text::{GraphScope, PredicateSet};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// The version of `geo.json` this build writes and reads.
pub const FORMAT_VERSION: u32 = 1;

/// How distances (and lengths, areas) are measured on geographic coordinates.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DistanceModel {
    /// on the WGS 84 ellipsoid (Karney's algorithms)
    #[default]
    Geodesic,
    /// on a sphere of the mean Earth radius (faster, up to about 0.5 % off)
    Haversine,
}

impl DistanceModel {
    pub fn as_str(self) -> &'static str {
        match self {
            DistanceModel::Geodesic => "geodesic",
            DistanceModel::Haversine => "haversine",
        }
    }
}

/// Spatial index configuration of a dataset (`geo.json`; the body of `PUT /$/geo/{ds}`,
/// where an empty body or `{}` means the defaults).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GeoConfig {
    /// serialization predicates whose geometry literals are indexed
    #[serde(default = "default_predicates")]
    pub predicates: Vec<String>,
    /// feature → geometry links followed by the `spatial:` property functions
    #[serde(default = "default_feature_links")]
    pub feature_links: Vec<String>,
    /// graphs whose quads are indexed (as in `text.json`)
    #[serde(default)]
    pub graphs: GraphScope,
    /// index W3C Basic Geo `lat`/`long` pairs as points
    #[serde(default)]
    pub wgs84: bool,
    /// match the topological `geo:` properties against geometries as well as asserted
    /// triples (GeoSPARQL's query rewrite; off by default)
    #[serde(default)]
    pub query_rewrite: bool,
    /// distance model of geographic coordinates
    #[serde(default)]
    pub distance: DistanceModel,
    /// literals with a longer lexical form are not indexed
    #[serde(default = "default_max_geometry_bytes")]
    pub max_geometry_bytes: usize,
    /// geometries with more vertices are not indexed, and functions refuse them
    #[serde(default = "default_max_vertices")]
    pub max_vertices: u32,
    #[serde(default = "default_format_version")]
    pub format_version: u32,
}

/// The geometry literals this build indexes: 2 added `geo:gmlLiteral` and
/// `geo:kmlLiteral`. Part of [`GeoConfig::index_hash`], so index files that left such
/// literals out are rebuilt.
const LITERALS_VERSION: u32 = 2;

fn default_predicates() -> Vec<String> {
    [
        vocab::AS_WKT,
        vocab::AS_GEOJSON,
        vocab::AS_GML,
        vocab::AS_KML,
        vocab::HAS_SERIALIZATION,
    ]
    .map(String::from)
    .to_vec()
}
fn default_feature_links() -> Vec<String> {
    [vocab::HAS_DEFAULT_GEOMETRY, vocab::HAS_GEOMETRY]
        .map(String::from)
        .to_vec()
}
fn default_max_geometry_bytes() -> usize {
    16 << 20
}
fn default_max_vertices() -> u32 {
    1_000_000
}
fn default_format_version() -> u32 {
    FORMAT_VERSION
}

impl Default for GeoConfig {
    fn default() -> Self {
        GeoConfig {
            predicates: default_predicates(),
            feature_links: default_feature_links(),
            graphs: GraphScope::default(),
            wgs84: false,
            query_rewrite: false,
            distance: DistanceModel::default(),
            max_geometry_bytes: default_max_geometry_bytes(),
            max_vertices: default_max_vertices(),
            format_version: FORMAT_VERSION,
        }
    }
}

impl GeoConfig {
    /// Check the configuration; the error names the offending field.
    pub fn validate(&self) -> Result<()> {
        let bad = |m: String| Err(Error::invalid(m));
        if self.format_version != FORMAT_VERSION {
            return bad(format!(
                "formatVersion: {} is not supported (this build reads {FORMAT_VERSION})",
                self.format_version
            ));
        }
        if self.predicates.is_empty() {
            return bad("predicates: at least one predicate is needed".into());
        }
        let iris = |field: &str, v: &[String]| -> Result<()> {
            for iri in v {
                if oxiri::Iri::parse(iri.as_str()).is_err() {
                    return Err(Error::invalid(format!("{field}: {iri:?} is not an IRI")));
                }
            }
            Ok(())
        };
        iris("predicates", &self.predicates)?;
        iris("featureLinks", &self.feature_links)?;
        if let PredicateSet::Only(v) = &self.graphs.include {
            iris("graphs.include", v)?;
        }
        iris("graphs.exclude", &self.graphs.exclude)?;
        if self.max_geometry_bytes == 0 {
            return bad("maxGeometryBytes: must be positive".into());
        }
        if self.max_vertices == 0 {
            return bad("maxVertices: must be positive".into());
        }
        Ok(())
    }

    /// FNV-1a of the canonical JSON form: equal for equal configurations, so an index
    /// built for one configuration is recognized (and result-cache keys change with it).
    pub fn hash(&self) -> u64 {
        let mut h = super::Fnv::new();
        h.bytes(&serde_json::to_vec(self).expect("serializable"));
        h.finish()
    }

    /// FNV-1a of the fields that decide what is indexed (predicates, graphs, `wgs84`,
    /// the size limits, the format version): the identity of persisted index files, which
    /// a change of distance model, feature links or query rewrite leaves valid.
    pub fn index_hash(&self) -> u64 {
        let key = serde_json::json!([
            LITERALS_VERSION,
            self.predicates,
            self.graphs,
            self.wgs84,
            self.max_geometry_bytes,
            self.max_vertices,
            self.format_version,
        ]);
        let mut h = super::Fnv::new();
        h.bytes(&serde_json::to_vec(&key).expect("serializable"));
        h.finish()
    }

    /// Whether quads of graph `g` (an IRI; [`crate::text::DEFAULT_GRAPH_IRI`] for the
    /// default graph) are indexed.
    pub fn graph_in_scope(&self, g: &str) -> bool {
        self.graphs.include.contains(g) && !self.graphs.exclude.iter().any(|x| x == g)
    }
}

/// State of the spatial index as a snapshot sees it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum IndexState {
    /// queries can use the index
    Ready,
    /// the base is being built (progress 0–1): queries use the plans without the index
    Building(f32),
    /// a build or commit-path update failed: plans without the index until a rebuild
    Failed,
    /// the index would exceed its memory budget: plans without the index
    OverBudget,
    /// the view of an update's own uncommitted changes: plans without the index
    Txn,
    /// a past state (`?at=`): plans without the index
    Historical,
    /// not enabled
    Off,
}

impl IndexState {
    pub fn ready(self) -> bool {
        self == IndexState::Ready
    }

    /// The `state` of [`GeoStatus`] and of explain counters.
    pub fn as_str(self) -> &'static str {
        match self {
            IndexState::Ready => "ready",
            IndexState::Building(_) => "building",
            IndexState::Failed => "failed",
            IndexState::OverBudget => "over-budget",
            IndexState::Txn => "transaction",
            IndexState::Historical => "historical",
            IndexState::Off => "off",
        }
    }
}

impl std::fmt::Display for IndexState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            IndexState::Building(p) => write!(f, "building ({:.0}%)", p * 100.0),
            s => f.write_str(s.as_str()),
        }
    }
}

/// Status of a dataset's spatial index (`GET /$/geo/{ds}`, `sparkles geo-index --status`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GeoStatus {
    pub enabled: bool,
    /// `ready`, `building`, `failed` or `over-budget`
    pub state: String,
    /// build progress (0–1) while `building`
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub progress: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    /// the generation the base was built for
    pub generation: String,
    /// the commit of the snapshot the status describes
    pub commit: u64,
    pub rows: GeoRows,
    /// distinct parsed geometries (the geometry column)
    pub literals: u64,
    pub skipped: GeoSkipped,
    /// literals per CRS IRI (unknown CRSs included)
    pub crs: BTreeMap<String, u64>,
    pub memory: GeoMemory,
    pub config: GeoConfig,
    pub format_version: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_build: Option<GeoBuild>,
    /// the index files the base is read from (persistent stores)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub files: Option<GeoFiles>,
}

/// The index files of the base (`gen-NNNN/geo/`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GeoFiles {
    pub bytes: u64,
    /// the base was read from files written before (no literal parsed), not built
    pub opened: bool,
}

fn is_zero(n: &u64) -> bool {
    *n == 0
}

/// Indexed rows: in the generation's base, the overlay of committed transactions, and
/// the tail not yet in the overlay's tree.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GeoRows {
    pub base: u64,
    pub overlay: u64,
    pub tail: u64,
    /// rows (of the three) that are W3C Basic Geo points
    #[serde(default, skip_serializing_if = "is_zero")]
    pub wgs84: u64,
}

/// Geometry literals of indexed predicates that are not indexed, by reason.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GeoSkipped {
    pub malformed: u64,
    pub unknown_crs: u64,
    pub too_large: u64,
    pub empty: u64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GeoMemory {
    pub tree_bytes: u64,
    pub geometry_bytes: u64,
    pub overlay_bytes: u64,
    pub budget_bytes: u64,
    /// bytes of the index files read in place (not counted against the budget)
    #[serde(default, skip_serializing_if = "is_zero")]
    pub mapped_bytes: u64,
}

/// The last build of the base.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct GeoBuild {
    pub at: String,
    pub ms: f64,
    pub rows: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_object_is_the_default() {
        let c: GeoConfig = serde_json::from_str("{}").unwrap();
        assert_eq!(c, GeoConfig::default());
        assert!(c.validate().is_ok());
        assert_eq!(c.predicates.len(), 5);
        assert_eq!(c.feature_links.len(), 2);
        assert_eq!(c.distance, DistanceModel::Geodesic);
        assert_eq!(c.max_geometry_bytes, 16 << 20);
        assert_eq!(c.max_vertices, 1_000_000);
    }

    #[test]
    fn json_form() {
        let c: GeoConfig = serde_json::from_str(
            r#"{"predicates":["http://www.opengis.net/ont/geosparql#asWKT"],
                "graphs":{"exclude":["urn:x-sparkles:inferred"]},"distance":"haversine",
                "maxVertices":10}"#,
        )
        .unwrap();
        assert_eq!(c.distance, DistanceModel::Haversine);
        assert_eq!(c.max_vertices, 10);
        assert!(!c.graph_in_scope("urn:x-sparkles:inferred"));
        assert!(c.graph_in_scope(crate::text::DEFAULT_GRAPH_IRI));
        let j = serde_json::to_value(&c).unwrap();
        for k in [
            "featureLinks",
            "queryRewrite",
            "maxGeometryBytes",
            "formatVersion",
        ] {
            assert!(j.get(k).is_some(), "{k}: {j}");
        }
        assert_eq!(serde_json::from_value::<GeoConfig>(j).unwrap(), c);
        assert!(serde_json::from_str::<GeoConfig>(r#"{"distance":"flat"}"#).is_err());
    }

    #[test]
    fn validation_names_the_field() {
        let err = |c: GeoConfig| c.validate().unwrap_err().to_string();
        let d = GeoConfig::default;
        assert!(
            GeoConfig {
                query_rewrite: true,
                ..d()
            }
            .validate()
            .is_ok()
        );
        assert!(GeoConfig { wgs84: true, ..d() }.validate().is_ok());
        assert!(
            err(GeoConfig {
                predicates: vec!["not an iri".into()],
                ..d()
            })
            .contains("predicates:")
        );
        assert!(
            err(GeoConfig {
                predicates: vec![],
                ..d()
            })
            .contains("predicates:")
        );
        assert!(
            err(GeoConfig {
                format_version: 2,
                ..d()
            })
            .contains("formatVersion:")
        );
        assert!(
            err(GeoConfig {
                max_vertices: 0,
                ..d()
            })
            .contains("maxVertices:")
        );
    }

    #[test]
    fn hash_follows_the_content() {
        let a = GeoConfig::default();
        assert_eq!(a.hash(), GeoConfig::default().hash());
        let b = GeoConfig {
            distance: DistanceModel::Haversine,
            ..GeoConfig::default()
        };
        assert_ne!(a.hash(), b.hash());
        // the distance model does not change what is indexed; the predicates do
        assert_eq!(a.index_hash(), b.index_hash());
        let c = GeoConfig {
            predicates: vec![vocab::AS_WKT.into()],
            ..GeoConfig::default()
        };
        assert_ne!(a.index_hash(), c.index_hash());
    }

    #[test]
    fn status_json_form() {
        let s = GeoStatus {
            enabled: true,
            state: IndexState::OverBudget.as_str().into(),
            progress: None,
            message: None,
            generation: "gen-0001".into(),
            commit: 3,
            rows: GeoRows::default(),
            literals: 0,
            skipped: GeoSkipped::default(),
            crs: BTreeMap::new(),
            memory: GeoMemory::default(),
            config: GeoConfig::default(),
            format_version: FORMAT_VERSION,
            last_build: None,
            files: None,
        };
        let j = serde_json::to_value(&s).unwrap();
        assert_eq!(j["state"], "over-budget");
        assert_eq!(j["formatVersion"], 1);
        assert!(j["skipped"].get("unknownCrs").is_some());
        assert!(j["memory"].get("budgetBytes").is_some());
        assert!(j.get("progress").is_none() && j.get("lastBuild").is_none());
        assert!(j.get("files").is_none() && j["memory"].get("mappedBytes").is_none());
        assert_eq!(IndexState::Building(0.37).to_string(), "building (37%)");
    }
}
