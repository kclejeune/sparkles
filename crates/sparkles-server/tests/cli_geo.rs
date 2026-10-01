//! `sparkles geo-index`: enable, status, reconfigure, rebuild and disable a database's
//! spatial index, run as the real binary; `sparkles check` and `clone` with `geo.json`.

use serde_json::Value as J;
use std::path::Path;
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_sparkles");

const FIXTURE: &str = r#"
@prefix ex: <http://example.org/> .
@prefix geo: <http://www.opengis.net/ont/geosparql#> .
ex:gA geo:asWKT "POLYGON((0 0, 10 0, 10 10, 0 10, 0 0))"^^geo:wktLiteral .
ex:g1 geo:asWKT "POINT(2 2)"^^geo:wktLiteral .
ex:g2 geo:asWKT "<http://www.opengis.net/def/crs/EPSG/0/4326> POINT(2 12)"^^geo:wktLiteral .
ex:gX geo:asWKT "POINT(1)"^^geo:wktLiteral .
"#;

#[track_caller]
fn expect(args: &[&str], code: i32) -> Output {
    let o = Command::new(BIN).args(args).output().unwrap();
    assert_eq!(
        o.status.code(),
        Some(code),
        "{args:?}\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    );
    o
}

#[cfg_attr(not(feature = "geo"), allow(dead_code))]
fn status(db: &str) -> J {
    let o = expect(&["geo-index", "--loc", db, "--status"], 0);
    serde_json::from_slice(&o.stdout).unwrap()
}

#[cfg_attr(not(feature = "geo"), allow(dead_code))]
fn total(s: &J) -> u64 {
    ["base", "overlay", "tail"]
        .iter()
        .map(|p| s["rows"][p].as_u64().unwrap())
        .sum()
}

fn load(dir: &Path) -> String {
    let data = dir.join("data.ttl");
    std::fs::write(&data, FIXTURE).unwrap();
    let db = dir.join("db").to_str().unwrap().to_string();
    expect(&["load", "--loc", &db, data.to_str().unwrap()], 0);
    db
}

#[cfg(feature = "geo")]
#[test]
fn geo_index_lifecycle() {
    let dir = tempfile::tempdir().unwrap();
    let db = load(dir.path());
    assert_eq!(status(&db), serde_json::json!({ "enabled": false }));
    // enable with the defaults: the status goes to stderr
    let o = expect(&["geo-index", "--loc", &db], 0);
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(err.contains("3 rows") && err.contains("ready"), "{err}");
    assert!(Path::new(&db).join("geo.json").exists());
    // a fresh process builds the index when it opens the database, and reports it built
    let s = status(&db);
    assert_eq!(
        (s["enabled"].as_bool(), s["state"].as_str()),
        (Some(true), Some("ready"))
    );
    assert_eq!((total(&s), s["literals"].as_u64()), (3, Some(3)));
    assert_eq!(s["skipped"]["malformed"], 1);
    // reconfigure, rebuild
    expect(&["geo-index", "--loc", &db, "--distance", "haversine"], 0);
    assert_eq!(status(&db)["config"]["distance"], "haversine");
    let o = expect(&["geo-index", "--loc", &db, "--distance", "flat"], 1);
    assert!(String::from_utf8_lossy(&o.stderr).contains("--distance flat"));
    expect(&["geo-index", "--loc", &db, "--rebuild"], 0);
    let s = status(&db);
    assert_eq!((s["state"].as_str(), total(&s)), (Some("ready"), 3));
    assert_eq!(s["config"]["distance"], "haversine");
    // only these predicates
    expect(
        &[
            "geo-index",
            "--loc",
            &db,
            "--predicate",
            "http://www.opengis.net/ont/geosparql#asGeoJSON",
        ],
        0,
    );
    assert_eq!(total(&status(&db)), 0);
    // check validates geo.json; clone copies it
    let o = expect(&["check", "--loc", &db, "--format", "json"], 0);
    let report: J = serde_json::from_slice(&o.stdout).unwrap();
    let geo = report["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"] == "geo")
        .unwrap_or_else(|| panic!("{report}"));
    assert_eq!(geo["status"], "ok", "{geo}");
    let copy = dir.path().join("copy");
    expect(&["clone", "--loc", &db, "--to", copy.to_str().unwrap()], 0);
    assert!(copy.join("geo.json").exists());
    // a broken geo.json is an error of check
    std::fs::write(Path::new(&db).join("geo.json"), br#"{"queryRewrite":true}"#).unwrap();
    let o = expect(&["check", "--loc", &db, "--format", "json"], 1);
    assert!(String::from_utf8_lossy(&o.stdout).contains("queryRewrite"));
    std::fs::write(Path::new(&db).join("geo.json"), b"{}").unwrap();
    // disable
    expect(&["geo-index", "--loc", &db, "--disable"], 0);
    assert!(!Path::new(&db).join("geo.json").exists());
    assert_eq!(status(&db)["enabled"], false);
}

#[cfg(not(feature = "geo"))]
#[test]
fn geo_index_needs_the_feature() {
    let dir = tempfile::tempdir().unwrap();
    let db = load(dir.path());
    let o = expect(&["geo-index", "--loc", &db], 2);
    assert!(
        String::from_utf8_lossy(&o.stderr)
            .contains("built without GeoSPARQL (cargo feature \"geo\")")
    );
}
