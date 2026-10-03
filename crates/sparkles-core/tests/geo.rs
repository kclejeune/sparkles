//! GeoSPARQL end to end through the public API: the acceptance fixture, the spatial
//! index's status across writes, compaction and reopening, and the dataset's distance
//! model in the `geof:` functions.
#![cfg(feature = "geo")]

use oxrdf::Term;
use sparkles_core::geo::{DistanceModel, GeoConfig, GeoStatus};
use sparkles_core::io::{RdfFormat, Source};
use sparkles_core::sparql::{QueryOptions, query};
use sparkles_core::store::{Store, StoreOptions};

const FIXTURE: &str = r#"
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

const P: &str = "PREFIX ex: <http://example.org/> \
    PREFIX geo: <http://www.opengis.net/ont/geosparql#> \
    PREFIX geof: <http://www.opengis.net/def/function/geosparql/> \
    PREFIX uom: <http://www.opengis.net/def/uom/OGC/1.0/> ";

/// The fixture in the base of a store (a load into an empty store, then a compaction).
fn load(s: &Store) {
    s.load(&[Source::from_bytes(
        FIXTURE.as_bytes().to_vec(),
        RdfFormat::TriG,
        None,
    )])
    .unwrap();
    s.compact().unwrap();
}

fn update(s: &Store, u: &str) {
    sparkles_core::sparql::update::update(s, &format!("{P}{u}"), &QueryOptions::default()).unwrap();
}

/// The local names of `?g` (or the value of `?v`) for each solution, sorted.
fn values(s: &Store, q: &str) -> Vec<String> {
    let r = query(s.snapshot(), &format!("{P}{q}"), &QueryOptions::default())
        .unwrap_or_else(|e| panic!("{q}: {e}"));
    let mut v: Vec<String> = r
        .rows()
        .into_iter()
        .map(|row| match row.into_iter().next().flatten() {
            Some(Term::NamedNode(n)) => n.as_str().rsplit('/').next().unwrap().to_string(),
            Some(Term::Literal(l)) => l.value().to_string(),
            other => format!("{other:?}"),
        })
        .collect();
    v.sort();
    v
}

fn rows(s: &GeoStatus) -> (u64, u64, u64) {
    (s.rows.base, s.rows.overlay, s.rows.tail)
}

/// The geometries within polygon A (computed by the function on every row); A is
/// within itself.
const WITHIN_A: &str = "SELECT ?g { ?g geo:asWKT ?w \
    FILTER(geof:sfWithin(?w, \"POLYGON((0 0, 10 0, 10 10, 0 10, 0 0))\"^^geo:wktLiteral)) }";

#[test]
fn status_follows_writes_and_compaction() {
    let s = Store::in_memory(StoreOptions::default());
    load(&s);
    let st = s.enable_geo(GeoConfig::default()).unwrap();
    assert_eq!((st.state.as_str(), rows(&st)), ("ready", (7, 0, 0)));
    assert_eq!(st.literals, 7);
    assert_eq!(values(&s, WITHIN_A), ["g1", "gA"]);
    // an insert lands in the tail; a delete of a base row leaves the base as it is
    update(
        &s,
        "INSERT DATA { ex:p5 geo:hasGeometry ex:g5 . \
         ex:g5 geo:asWKT \"POINT(1 1)\"^^geo:wktLiteral }",
    );
    let reader = s.snapshot();
    assert_eq!(rows(&s.geo_status().unwrap()), (7, 0, 1));
    assert_eq!(values(&s, WITHIN_A), ["g1", "g5", "gA"]);
    update(
        &s,
        "DELETE DATA { ex:g1 geo:asWKT \"POINT(2 2)\"^^geo:wktLiteral }",
    );
    assert_eq!(rows(&s.geo_status().unwrap()), (7, 0, 1));
    assert_eq!(values(&s, WITHIN_A), ["g5", "gA"]);
    // the earlier snapshot keeps its answer
    let r = query(reader, &format!("{P}{WITHIN_A}"), &QueryOptions::default()).unwrap();
    assert_eq!(r.len(), 3);
    // compaction folds the changes into a new base
    s.compact().unwrap();
    let st = s.geo_status().unwrap();
    assert_eq!((st.state.as_str(), rows(&st)), ("ready", (7, 0, 0)));
    assert_eq!(values(&s, WITHIN_A), ["g5", "gA"]);
    // disabled: the functions still answer
    s.disable_geo().unwrap();
    assert!(s.geo_status().is_none());
    assert_eq!(values(&s, WITHIN_A), ["g5", "gA"]);
}

#[test]
fn the_index_is_rebuilt_on_open() {
    let dir = tempfile::tempdir().unwrap();
    {
        let s = Store::open(dir.path(), StoreOptions::default()).unwrap();
        load(&s);
        s.enable_geo(GeoConfig {
            distance: DistanceModel::Haversine,
            ..GeoConfig::default()
        })
        .unwrap();
        update(
            &s,
            "INSERT DATA { ex:g5 geo:asWKT \"POINT(1 1)\"^^geo:wktLiteral }",
        );
    }
    assert!(dir.path().join("geo.json").exists());
    let s = Store::open(dir.path(), StoreOptions::default()).unwrap();
    assert!(s.geo_enabled());
    let st = s.wait_geo().unwrap();
    assert_eq!(st.state, "ready");
    assert_eq!(st.rows.base + st.rows.overlay + st.rows.tail, 8);
    assert_eq!(st.config.distance, DistanceModel::Haversine);
    assert_eq!(values(&s, WITHIN_A), ["g1", "g5", "gA"]);
    // the configuration survives a disable only as long as the index does
    s.disable_geo().unwrap();
    assert!(!dir.path().join("geo.json").exists());
    drop(s);
    let s = Store::open(dir.path(), StoreOptions::default()).unwrap();
    assert!(!s.geo_enabled() && s.wait_geo().is_none());
}

#[test]
fn the_dataset_distance_model() {
    let s = Store::in_memory(StoreOptions::default());
    load(&s);
    let q = "SELECT ?d { BIND(geof:metricDistance(\"POINT(0 0)\"^^geo:wktLiteral, \
             \"POINT(1 0)\"^^geo:wktLiteral) AS ?d) }";
    let d = |s: &Store| values(s, q)[0].parse::<f64>().unwrap();
    // geodesic on WGS 84 without an index, and with the default configuration
    assert!((d(&s) - 111_319.490_793_273_57).abs() < 1e-6, "{}", d(&s));
    s.enable_geo(GeoConfig::default()).unwrap();
    assert!((d(&s) - 111_319.490_793_273_57).abs() < 1e-6);
    // the haversine sphere when the dataset asks for it
    s.enable_geo(GeoConfig {
        distance: DistanceModel::Haversine,
        ..GeoConfig::default()
    })
    .unwrap();
    assert!((d(&s) - 111_195.079_734_36).abs() < 1e-6, "{}", d(&s));
}
