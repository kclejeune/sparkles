//! W3C Basic Geo points in the spatial index: built with the base, kept up by commits
//! (a point lives while both of its quads do), written to the index files, found by the
//! `spatial:` functions and the map view, and the same with the index as without it.

use super::tests::{opts, rng};
use crate::geo::GeoConfig;
use crate::geo::map::{BoxQuery, features_in_box};
use crate::io::RdfFormat;
use crate::store::Snapshot;
use crate::store::Store;
use std::collections::BTreeSet;
use std::sync::Arc;

const DATA: &str = r#"
@prefix ex: <http://example.org/> .
@prefix pos: <http://www.w3.org/2003/01/geo/wgs84_pos#> .
@prefix xsd: <http://www.w3.org/2001/XMLSchema#> .
ex:a pos:lat 10 ; pos:long 20 .
ex:b pos:lat "5"^^xsd:string ; pos:long "6.5"^^xsd:double .
ex:c pos:lat 1, 2 ; pos:long 3 .
ex:d pos:lat 200 ; pos:long 0 .
ex:e pos:lat 1.5 .
ex:f pos:long 2 .
ex:G { ex:f pos:lat 1 . ex:h pos:lat "-1" ; pos:long "-2" . }
"#;

const P: &str = "PREFIX ex: <http://example.org/> \
    PREFIX pos: <http://www.w3.org/2003/01/geo/wgs84_pos#> \
    PREFIX spatial: <http://jena.apache.org/spatial#> \
    PREFIX uom: <http://www.opengis.net/def/uom/OGC/1.0/> ";

fn cfg() -> GeoConfig {
    GeoConfig {
        wgs84: true,
        ..GeoConfig::default()
    }
}

/// Local names of the `?f` of a query.
fn features(snap: Arc<Snapshot>, q: &str) -> BTreeSet<String> {
    let r = crate::sparql::query(snap, &format!("{P}{q}"), &Default::default())
        .unwrap_or_else(|e| panic!("{q}: {e}"));
    r.rows()
        .into_iter()
        .map(|row| match row.into_iter().next().flatten() {
            Some(oxrdf::Term::NamedNode(n)) => n.as_str().rsplit('/').next().unwrap().into(),
            other => format!("{other:?}"),
        })
        .collect()
}

fn names(v: &[&str]) -> BTreeSet<String> {
    v.iter().map(|s| s.to_string()).collect()
}

const WORLD_BOX: &str = "SELECT ?f { ?f spatial:intersectBox (-90 -180 90 180) }";
const ALL_GRAPHS: &str = "SELECT ?f { GRAPH ?g { ?f spatial:intersectBox (-90 -180 90 180) } }";

/// The answers of a few queries, with the index and by scanning (the index paused).
fn both(ds: &Store, q: &str) -> BTreeSet<String> {
    let snap = ds.snapshot();
    let indexed = features(snap.clone(), q);
    // the same snapshot with a view that is not ready: the searches scan
    let mut s = (*snap).clone();
    let v = snap.geo.as_ref().unwrap();
    s.geo = Some(Arc::new(
        v.without_rows(crate::geo::index::ViewState::Failed),
    ));
    assert_eq!(features(Arc::new(s), q), indexed, "{q}");
    indexed
}

#[test]
fn points_of_lat_long_pairs() {
    let ds = crate::store::Store::in_memory(opts());
    ds.load_str(DATA, RdfFormat::TriG).unwrap();
    ds.compact().unwrap();
    // off by default
    ds.enable_geo(GeoConfig::default()).unwrap();
    assert!(features(ds.snapshot(), WORLD_BOX).is_empty());
    let s = ds.enable_geo(cfg()).unwrap();
    assert_eq!(s.state, "ready");
    // a, b, c twice (two latitudes), h in its graph; not d (out of range), e and f
    assert_eq!((s.rows.wgs84, s.rows.base), (5, 5));
    assert_eq!(s.literals, 0);
    assert_eq!(both(&ds, WORLD_BOX), names(&["a", "b", "c"]));
    assert_eq!(both(&ds, ALL_GRAPHS), names(&["h"]));
    assert_eq!(
        both(
            &ds,
            "SELECT ?f { ?f spatial:nearby (10 20 1 uom:kilometre) }"
        ),
        names(&["a"])
    );
    assert_eq!(
        both(
            &ds,
            "SELECT ?f { ?f spatial:nearby (1.5 3 100 uom:kilometre 1) }"
        ),
        names(&["c"])
    );
    assert_eq!(
        both(&ds, "SELECT ?f { ?f spatial:withinBox (0 0 6 7) }"),
        names(&["b", "c"])
    );
    // a given feature: its own points
    assert_eq!(
        both(
            &ds,
            "SELECT ?f { BIND(ex:a AS ?f) ex:a spatial:intersectBox (0 0 20 30) }"
        ),
        names(&["a"])
    );
    assert!(
        both(
            &ds,
            "SELECT ?f { BIND(ex:a AS ?f) ex:a spatial:intersectBox (0 0 1 1) }"
        )
        .is_empty()
    );
    // the map view: the subject is the feature, the predicate lat_long
    let world = BoxQuery {
        bbox: [-180.0, -90.0, 180.0, 90.0],
        graph: None,
        predicate: None,
        limit: 100,
        tolerance: None,
    };
    let v = features_in_box(&ds.snapshot(), &world).unwrap();
    let fs = v["features"].as_array().unwrap();
    assert_eq!(fs.len(), 5);
    let a = fs
        .iter()
        .find(|f| f["id"] == "http://example.org/a")
        .unwrap();
    assert_eq!(a["properties"]["feature"], "http://example.org/a");
    assert_eq!(a["properties"]["predicate"], crate::geo::wgs84::LAT_LONG);
    assert_eq!(a["geometry"]["coordinates"], serde_json::json!([20, 10]));
    let v = features_in_box(
        &ds.snapshot(),
        &BoxQuery {
            predicate: Some(crate::geo::wgs84::LAT_LONG.into()),
            ..world.clone()
        },
    )
    .unwrap();
    assert_eq!(v["features"].as_array().unwrap().len(), 5);
    let v = features_in_box(
        &ds.snapshot(),
        &BoxQuery {
            predicate: Some(crate::geo::vocab::AS_WKT.into()),
            ..world
        },
    )
    .unwrap();
    assert!(v["features"].as_array().unwrap().is_empty());

    // commits: a point lives while both of its quads do
    let reader = ds.snapshot();
    let up = |u: &str| ds.update(&format!("{P}{u}")).unwrap();
    up("INSERT DATA { ex:e pos:long 1.5 . ex:n pos:lat 7 ; pos:long 8 }");
    up("DELETE DATA { ex:a pos:lat 10 . ex:c pos:lat 1 }");
    assert_eq!(both(&ds, WORLD_BOX), names(&["b", "c", "e", "n"]));
    assert_eq!(features(reader.clone(), WORLD_BOX), names(&["a", "b", "c"]));
    // the other half of a base point comes back; a new latitude for a new point
    up("INSERT DATA { ex:a pos:lat 11 }");
    assert_eq!(both(&ds, WORLD_BOX), names(&["a", "b", "c", "e", "n"]));
    assert_eq!(
        both(
            &ds,
            "SELECT ?f { ?f spatial:nearby (11 20 1 uom:kilometre) }"
        ),
        names(&["a"])
    );
    up("DELETE DATA { ex:a pos:lat 11 } ; INSERT DATA { ex:a pos:lat 10 }");
    assert_eq!(
        both(
            &ds,
            "SELECT ?f { ?f spatial:nearby (10 20 1 uom:kilometre) }"
        ),
        names(&["a"])
    );
    up("DELETE DATA { ex:n pos:long 8 }");
    assert_eq!(both(&ds, WORLD_BOX), names(&["a", "b", "c", "e"]));
    up("INSERT DATA { ex:n pos:long 8 }");
    assert_eq!(both(&ds, WORLD_BOX), names(&["a", "b", "c", "e", "n"]));
    let s = ds.geo_status().unwrap();
    assert!(s.rows.wgs84 >= 7, "{:?}", s.rows);
    // a compaction folds them into the base
    ds.compact().unwrap();
    let s = ds.geo_status().unwrap();
    assert_eq!((s.rows.wgs84, s.rows.overlay, s.rows.tail), (6, 0, 0));
    assert_eq!(both(&ds, WORLD_BOX), names(&["a", "b", "c", "e", "n"]));
    assert_eq!(features(reader, WORLD_BOX), names(&["a", "b", "c"]));
}

#[test]
fn points_in_the_index_files() {
    let dir = tempfile::tempdir().unwrap();
    let expected;
    {
        let ds = Store::open(dir.path(), opts()).unwrap();
        ds.load_str(DATA, RdfFormat::TriG).unwrap();
        ds.compact().unwrap();
        ds.enable_geo(cfg()).unwrap();
        ds.update(&format!("{P}INSERT DATA {{ ex:e pos:long 1.5 }}"))
            .unwrap();
        expected = both(&ds, WORLD_BOX);
        assert!(expected.contains("e"));
    }
    let ds = Store::open(dir.path(), opts()).unwrap();
    let s = ds.wait_geo().unwrap();
    assert!(s.files.unwrap().opened);
    assert_eq!((s.rows.wgs84, s.rows.base, s.rows.overlay), (6, 5, 1));
    assert_eq!(both(&ds, WORLD_BOX), expected);
    assert_eq!(both(&ds, ALL_GRAPHS), names(&["h"]));
    // the points' quads still decide
    ds.update(&format!(
        "{P}DELETE DATA {{ ex:b pos:long \"6.5\"^^<http://www.w3.org/2001/XMLSchema#double> }}"
    ))
    .unwrap();
    let mut less = expected.clone();
    less.remove("b");
    assert_eq!(both(&ds, WORLD_BOX), less);
}

#[test]
fn random_points_match_a_scan() {
    let ds = crate::store::Store::in_memory(opts());
    let mut seed = 11;
    let mut q = String::new();
    for i in 0..300 {
        let (y, x) = (rng(&mut seed) * 40.0, rng(&mut seed) * 40.0);
        q += &format!("ex:s{i} pos:lat {y:.4} ; pos:long {x:.4} .\n");
    }
    ds.load_str(
        &format!("@prefix ex: <http://example.org/> . @prefix pos: <http://www.w3.org/2003/01/geo/wgs84_pos#> .\n{q}"),
        RdfFormat::Turtle,
    )
    .unwrap();
    ds.compact().unwrap();
    ds.enable_geo(cfg()).unwrap();
    let mut found = 0;
    for c in 0..30 {
        let mut ops = vec![String::from("INSERT DATA { ex:x pos:lat 0 }")];
        for _ in 0..10 {
            let i = (rng(&mut seed) * 400.0) as usize;
            let v = format!("{:.4}", rng(&mut seed) * 40.0);
            let p = if (c + i).is_multiple_of(2) {
                "lat"
            } else {
                "long"
            };
            ops.push(if rng(&mut seed) < 0.6 {
                format!("INSERT DATA {{ ex:s{i} pos:{p} {v} }}")
            } else {
                format!("DELETE WHERE {{ ex:s{i} pos:{p} ?o }}")
            });
        }
        ds.update(&format!("{P}{}", ops.join(" ; "))).unwrap();
        for _ in 0..3 {
            let (y, x) = (rng(&mut seed) * 40.0, rng(&mut seed) * 40.0);
            let (h, w) = (rng(&mut seed) * 15.0, rng(&mut seed) * 15.0);
            found += both(
                &ds,
                &format!(
                    "SELECT ?f {{ ?f spatial:intersectBox ({y} {x} {} {}) }}",
                    y + h,
                    x + w
                ),
            )
            .len();
            both(
                &ds,
                &format!("SELECT ?f {{ ?f spatial:nearby ({y} {x} 500 uom:kilometre 5) }}"),
            );
        }
    }
    assert!(found > 100, "{found}");
    let s = ds.geo_status().unwrap();
    assert!(s.rows.overlay + s.rows.tail > 0, "{:?}", s.rows);
}
