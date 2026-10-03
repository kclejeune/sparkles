//! GeoSPARQL behaviour that Apache Jena's `jena-geosparql` unit tests pin down,
//! re-expressed as Sparkles SPARQL tests. Each test names the Jena test it follows; where
//! Sparkles answers otherwise on purpose, the Jena value is in a comment.
//!
//! The city fixture stands in for Jena's `SpatialIndexTestData` (five cities, as points
//! in EPSG:4326, latitude first) with coordinates of our own; the expected answers are
//! Jena's. Function arguments and expected values are the ones of the named Jena tests.
#![cfg(feature = "geo")]

use oxrdf::Term;
use sparkles_core::geo::{DistanceModel, GeoConfig};
use sparkles_core::io::{RdfFormat, Source};
use sparkles_core::sparql::{QueryOptions, query};
use sparkles_core::store::{Store, StoreOptions};

const CITIES: &str = r#"
@prefix ex: <http://example.org/> .
@prefix geo: <http://www.opengis.net/ont/geosparql#> .
ex:London geo:hasGeometry ex:LondonGeom .
ex:LondonGeom geo:asWKT "<http://www.opengis.net/def/crs/EPSG/0/4326> POINT(51.5074 -0.1278)"^^geo:wktLiteral .
ex:NewYork geo:hasGeometry ex:NewYorkGeom .
ex:NewYorkGeom geo:asWKT "<http://www.opengis.net/def/crs/EPSG/0/4326> POINT(40.7128 -74.006)"^^geo:wktLiteral .
ex:Honolulu geo:hasGeometry ex:HonoluluGeom .
ex:HonoluluGeom geo:asWKT "<http://www.opengis.net/def/crs/EPSG/0/4326> POINT(21.3069 -157.8583)"^^geo:wktLiteral .
ex:Perth geo:hasGeometry ex:PerthGeom .
ex:PerthGeom geo:asWKT "<http://www.opengis.net/def/crs/EPSG/0/4326> POINT(-31.9523 115.8613)"^^geo:wktLiteral .
ex:Auckland geo:hasGeometry ex:AucklandGeom .
ex:AucklandGeom geo:asWKT "<http://www.opengis.net/def/crs/EPSG/0/4326> POINT(-36.8485 174.7633)"^^geo:wktLiteral .
"#;

const P: &str = "PREFIX ex: <http://example.org/>
PREFIX geo: <http://www.opengis.net/ont/geosparql#>
PREFIX geof: <http://www.opengis.net/def/function/geosparql/>
PREFIX spatial: <http://jena.apache.org/spatial#>
PREFIX spatialF: <http://jena.apache.org/function/spatial#>
PREFIX uom: <http://www.opengis.net/def/uom/OGC/1.0/>
PREFIX xsd: <http://www.w3.org/2001/XMLSchema#>
";

const KM: &str = "http://www.opengis.net/def/uom/OGC/1.0/kilometer";

/// The cities, with the spatial index on (`distance` as given) or off.
fn cities(index: Option<DistanceModel>) -> Store {
    let s = Store::in_memory(StoreOptions::default());
    s.load(&[Source::from_bytes(
        CITIES.as_bytes().to_vec(),
        RdfFormat::Turtle,
        None,
    )])
    .unwrap();
    if let Some(distance) = index {
        s.enable_geo(GeoConfig {
            distance,
            ..GeoConfig::default()
        })
        .unwrap();
    }
    s
}

/// The local names of `?subj`, sorted.
fn subjects(s: &Store, pattern: &str) -> Vec<String> {
    let text = format!("{P}SELECT ?subj {{ {pattern} }}");
    let r = query(s.snapshot(), &text, &QueryOptions::default())
        .unwrap_or_else(|e| panic!("{text}: {e}"));
    let mut v: Vec<String> = r
        .rows()
        .into_iter()
        .map(|row| match row.into_iter().next().flatten() {
            Some(Term::NamedNode(n)) => n.as_str().rsplit('/').next().unwrap().to_string(),
            other => panic!("{other:?}"),
        })
        .collect();
    v.sort();
    v
}

/// `expr` evaluated once (`None`: unbound).
fn value(s: &Store, expr: &str) -> Option<Term> {
    let text = format!("{P}SELECT ?r {{ BIND({expr} AS ?r) }}");
    let r = query(s.snapshot(), &text, &QueryOptions::default())
        .unwrap_or_else(|e| panic!("{text}: {e}"));
    r.rows().pop().unwrap().pop().unwrap()
}

/// `expr` as a lexical form and datatype.
fn literal(s: &Store, expr: &str) -> (String, String) {
    match value(s, expr) {
        Some(Term::Literal(l)) => (l.value().to_string(), l.datatype().as_str().to_string()),
        other => panic!("{expr}: {other:?}"),
    }
}

fn number(s: &Store, expr: &str) -> f64 {
    literal(s, expr).0.parse().unwrap()
}

fn close(s: &Store, expr: &str, want: f64, tolerance: f64) {
    let got = number(s, expr);
    assert!(
        (got - want).abs() <= tolerance,
        "{expr}: {got}, want {want}"
    );
}

const WKT: &str = "http://www.opengis.net/ont/geosparql#wktLiteral";

fn wkt4326(lat: f64, lon: f64) -> String {
    format!("\"<http://www.opengis.net/def/crs/EPSG/0/4326> POINT({lat} {lon})\"^^geo:wktLiteral")
}

// ------------------------------------------------------------- spatial: ------

/// `NearbyPFTest.testExecEvaluated` / `testExecEvaluated_fail`: London is about 344 km
/// from the point in Paris (kilometres are the default unit).
#[test]
fn nearby() {
    for index in [
        None,
        Some(DistanceModel::Geodesic),
        Some(DistanceModel::Haversine),
    ] {
        let s = cities(index);
        assert_eq!(
            subjects(&s, "?subj spatial:nearby(48.857487 2.373047 350)"),
            ["London"],
            "{index:?}"
        );
        assert!(subjects(&s, "?subj spatial:nearby(48.857487 2.373047 340)").is_empty());
        // NearbyGeomPFTest, with the point as a constant geometry
        let paris = wkt4326(48.857487, 2.373047);
        assert_eq!(
            subjects(&s, &format!("?subj spatial:nearbyGeom({paris} 350)")),
            ["London"]
        );
        assert!(subjects(&s, &format!("?subj spatial:nearbyGeom({paris} 340)")).is_empty());
        assert!(subjects(&s, "?subj spatial:nearby(0.0 0.0 10)").is_empty());
    }
}

/// `WithinBoxPFTest.testExecEvaluated`, `IntersectBoxPFTest.testExecEvaluated`.
#[test]
fn boxes() {
    for index in [None, Some(DistanceModel::Geodesic)] {
        let s = cities(index);
        assert_eq!(
            subjects(&s, "?subj spatial:withinBox(51.4 -0.13 51.6 -0.12)"),
            ["London"]
        );
        assert_eq!(
            subjects(&s, "?subj spatial:intersectBox(51.4 -0.13 51.6 -0.12)"),
            ["London"]
        );
    }
}

/// `NorthPFTest`, `SouthPFTest`, `EastPFTest`, `WestPFTest` (`testExecEvaluated`): from
/// the point in Paris; east and west reach 180° of longitude (Auckland is 187.6° west).
#[test]
fn cardinal_directions() {
    for index in [None, Some(DistanceModel::Geodesic)] {
        let s = cities(index);
        let from = |f: &str| subjects(&s, &format!("?subj spatial:{f}(48.857487 2.373047)"));
        assert_eq!(from("north"), ["London"]);
        assert_eq!(from("south"), ["Auckland", "Honolulu", "NewYork", "Perth"]);
        assert_eq!(from("east"), ["Auckland", "Perth"]);
        assert_eq!(from("west"), ["Honolulu", "London", "NewYork"]);
    }
}

// ------------------------------------------------------------ spatialF: ------

/// `ConvertLatLonFFTest`, `ConvertLatLonBoxFFTest`: EPSG:4326 literals, latitude first;
/// numeric strings are numbers, other strings and coordinates out of range are errors.
#[test]
fn convert_lat_lon() {
    let s = cities(None);
    let epsg = "<http://www.opengis.net/def/crs/EPSG/0/4326>";
    assert_eq!(
        literal(&s, "spatialF:convertLatLon(10, 20)"),
        (format!("{epsg} POINT(10 20)"), WKT.into())
    );
    assert_eq!(
        literal(&s, "spatialF:convertLatLon(0.0, 10.0)").0,
        format!("{epsg} POINT(0 10)")
    );
    assert_eq!(
        literal(&s, "spatialF:convertLatLon(\"10.0\", 20)").0,
        format!("{epsg} POINT(10 20)")
    );
    assert_eq!(
        literal(&s, "spatialF:convertLatLonBox(0.0, 1.0, 10.0, 11.0)").0,
        format!("{epsg} POLYGON((0 1, 10 1, 10 11, 0 11, 0 1))")
    );
    for e in [
        "spatialF:convertLatLon(\"e\", 20)",
        "spatialF:convertLatLon(91, 0)",
        "spatialF:convertLatLon(0, 181)",
        "spatialF:convertLatLonBox(0, 1, 10, \"e\")",
        "spatialF:convertLatLon(1)",
    ] {
        assert_eq!(value(&s, e), None, "{e}");
    }
    // the point lies where the literal says: longitude 20, latitude 10
    assert!(matches!(
        literal(
            &s,
            "geof:sfEquals(spatialF:convertLatLon(10, 20), \"POINT(20 10)\"^^geo:wktLiteral)"
        )
        .0
        .as_str(),
        "true"
    ));
}

/// `AngleFFTest`, `AngleDegreesFFTest`, `AzimuthFFTest`, `AzimuthDegreesFFTest`.
#[test]
fn angles_and_azimuths() {
    let s = cities(None);
    close(
        &s,
        "spatialF:angle(25, 45, 75, 100)",
        0.737_815_060_120_464_9,
        1e-15,
    );
    assert_eq!(number(&s, "spatialF:angleDeg(25, 45, 75, 100)"), 42.273_689);
    close(
        &s,
        "spatialF:azimuth(0, 0, 0, 10)",
        std::f64::consts::FRAC_PI_2,
        1e-15,
    );
    assert_eq!(number(&s, "spatialF:azimuthDeg(0, 0, 0, 10)"), 90.0);
    // Jena's angle is off by a quarter turn south-east and north-west of the first
    // point (it gives π/2 for due south); Sparkles follows its documented meaning
    close(
        &s,
        "spatialF:angle(0, 0, 0, -1)",
        std::f64::consts::PI,
        1e-15,
    );
    assert_eq!(value(&s, "spatialF:angle(0, 0, \"1\", 1)"), None);
}

/// `GreatCircleFFTest`, `GreatCircleGeomFFTest`, `DistanceFFTest` (spatial),
/// `NearbyFFTest`: Jena's numbers are on its haversine sphere, so the dataset uses the
/// haversine distance model; the geodesic model (the default) is within 0.6 %.
#[test]
fn great_circle_distances() {
    let s = cities(Some(DistanceModel::Haversine));
    close(
        &s,
        &format!("spatialF:greatCircle(10.0, 20.0, 10.0, 21.0, <{KM}>)"),
        109.5057,
        1e-4,
    );
    close(
        &s,
        &format!("spatialF:greatCircle(10.0, 20.0, 11.0, 20.0, \"{KM}\")"),
        111.1950,
        1e-4,
    );
    close(
        &s,
        &format!("spatialF:greatCircle(48.85341, 2.34880, 51.50853, -0.12574, \"{KM}\")"),
        343.7713,
        1e-4,
    );
    close(
        &s,
        &format!("spatialF:greatCircle(51.50853, -0.12574, 48.857487, 2.373047, <{KM}>)"),
        344.266_423,
        1e-6,
    );
    let (a, b) = (wkt4326(10.0, 20.0), wkt4326(10.0, 21.0));
    close(
        &s,
        &format!("spatialF:greatCircleGeom({a}, {b}, <{KM}>)"),
        109.5057,
        1e-4,
    );
    close(
        &s,
        &format!("spatialF:distance({a}, {b}, <{KM}>)"),
        109.5057,
        1e-4,
    );
    let (london, paris) = (wkt4326(51.50853, -0.12574), wkt4326(48.857487, 2.373047));
    close(
        &s,
        &format!("spatialF:greatCircleGeom({london}, {paris}, uom:kilometer)"),
        344.266_423,
        1e-6,
    );
    // nearby: strictly closer than the radius; the unit as an IRI, a string or anyURI
    let c = wkt4326(10.0, 20.0001);
    for unit in [
        format!("<{KM}>"),
        format!("\"{KM}\""),
        format!("\"{KM}\"^^xsd:anyURI"),
    ] {
        let e = format!("spatialF:nearby({a}, {c}, 20, {unit})");
        assert_eq!(literal(&s, &e).0, "true", "{e}");
        let e = format!("spatialF:withinCircle({a}, {b}, 100, {unit})");
        assert_eq!(literal(&s, &e).0, "false", "{e}");
    }
    // great circles take length units only; numbers only
    for e in [
        "spatialF:greatCircle(10.0, 20.0, 10.0, 20.0001, uom:radian)".to_string(),
        format!("spatialF:greatCircle(\"10.0\", 20.0, 10.0, 20.0001, <{KM}>)"),
        format!("spatialF:greatCircleGeom({a}, {b}, uom:degree)"),
    ] {
        assert_eq!(value(&s, &e), None, "{e}");
    }
    // the dataset's default model: geodesic
    let g = cities(None);
    let geodesic = number(
        &g,
        &format!("spatialF:greatCircle(10.0, 20.0, 10.0, 21.0, <{KM}>)"),
    );
    assert!((geodesic - 109.5057).abs() / 109.5057 < 0.006, "{geodesic}");
}

/// `EqualsFFTest`: topological equality, across CRSs.
#[test]
fn equals() {
    let s = cities(None);
    for (a, b, want) in [
        (wkt4326(10.0, 20.0), wkt4326(10.0, 20.0), "true"),
        (
            wkt4326(10.0, 20.0),
            "\"POINT(20 10)\"^^geo:wktLiteral".to_string(),
            "true",
        ),
        (wkt4326(10.0, 20.0), wkt4326(10.0, 21.0), "false"),
    ] {
        assert_eq!(literal(&s, &format!("spatialF:equals({a}, {b})")).0, want);
    }
}

/// `TransformFFTest`, `TransformDatatypeFFTest`, `TransformSRSFFTest`: datatype and CRS
/// as IRIs or strings.
#[test]
fn transforms() {
    let s = cities(None);
    let p = wkt4326(10.0, 20.0);
    let gj = "http://www.opengis.net/ont/geosparql#geoJSONLiteral";
    assert_eq!(
        literal(&s, &format!("spatialF:transformDatatype({p}, <{gj}>)")),
        (
            r#"{"type":"Point","coordinates":[20,10]}"#.into(),
            gj.into()
        )
    );
    assert_eq!(
        literal(
            &s,
            &format!(
                "spatialF:transformSRS({p}, \"http://www.opengis.net/def/crs/OGC/1.3/CRS84\")"
            )
        ),
        ("POINT(20 10)".into(), WKT.into())
    );
    assert_eq!(
        literal(
            &s,
            &format!(
                "spatialF:transform({p}, <{WKT}>, <http://www.opengis.net/def/crs/OGC/1.3/CRS84>)"
            )
        )
        .0,
        "POINT(20 10)"
    );
    // into a UTM zone, as GeoJSON (always CRS84) and back
    let (lex, _) = literal(
        &s,
        &format!(
            "spatialF:transform({p}, \"{gj}\", <http://www.opengis.net/def/crs/EPSG/0/32634>)"
        ),
    );
    let j: serde_json::Value = serde_json::from_str(&lex).unwrap();
    let (lon, lat) = (
        j["coordinates"][0].as_f64().unwrap(),
        j["coordinates"][1].as_f64().unwrap(),
    );
    assert!(
        (lon - 20.0).abs() < 1e-12 && (lat - 10.0).abs() < 1e-12,
        "{lex}"
    );
    for e in [
        format!("spatialF:transformDatatype({p}, <http://example.org/dt>)"),
        format!("spatialF:transformSRS({p}, <http://example.org/crs>)"),
        format!("spatialF:transform({p}, <{WKT}>)"),
    ] {
        assert_eq!(value(&s, &e), None, "{e}");
    }
}

// ---------------------------------------------------------- aggregates ------

/// Not in Jena: the GeoSPARQL 1.1 aggregates over the cities, grouped and with
/// DISTINCT (here as a regression check of the acceptance example's shape).
#[test]
fn aggregates_over_features() {
    let s = cities(None);
    let text = format!(
        "{P}SELECT (geof:aggBoundingBox(?w) AS ?b) (COUNT(?w) AS ?n) \
         {{ ?f geo:hasGeometry/geo:asWKT ?w }}"
    );
    let r = query(s.snapshot(), &text, &QueryOptions::default()).unwrap();
    let row = r.rows().pop().unwrap();
    let Some(Term::Literal(b)) = &row[0] else {
        panic!("{row:?}")
    };
    // the first input is EPSG:4326, so is the box (latitude first)
    assert!(
        b.value()
            .starts_with("<http://www.opengis.net/def/crs/EPSG/0/4326> POLYGON(("),
        "{b}"
    );
    let text = format!(
        "{P}ASK {{ {{ SELECT (geof:aggBoundingBox(?w) AS ?b) {{ ?f geo:hasGeometry/geo:asWKT ?w }} }} \
         FILTER(geof:sfEquals(?b, \"POLYGON((-157.8583 -36.8485, 174.7633 -36.8485, 174.7633 51.5074, -157.8583 51.5074, -157.8583 -36.8485))\"^^geo:wktLiteral)) }}"
    );
    assert!(
        query(s.snapshot(), &text, &QueryOptions::default())
            .unwrap()
            .boolean
    );
}
