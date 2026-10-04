//! Spatial joins and nearest-neighbour ordering end to end: the acceptance cases, the
//! plans and warnings EXPLAIN shows, and random data on which every rewritten query
//! answers as the plain plan does (the `spatial_join` and `spatial_knn` optimizations
//! switched off): in the default graph, under `GRAPH ?g`, over a merged default graph,
//! with optional parts and duplicate rows, and with rows written after the index was
//! built.
#![cfg(feature = "geo")]

use oxrdf::Term;
use sparkles_core::geo::GeoConfig;
use sparkles_core::io::{RdfFormat, Source};
use sparkles_core::sparql::{Optimizations, PlanInfo, QueryOptions, QueryResult, query};
use sparkles_core::store::{Snapshot, Store, StoreOptions};
use std::sync::Arc;

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

fn load(s: &Store, data: &str) {
    s.load(&[Source::from_bytes(
        data.as_bytes().to_vec(),
        RdfFormat::TriG,
        None,
    )])
    .unwrap();
}

/// Query options with the spatial join and nearest-neighbour rewrites on or off.
fn opts(on: bool) -> QueryOptions {
    let mut o = Optimizations::ALL;
    o.spatial_join = on;
    o.spatial_knn = on;
    QueryOptions {
        optimizations: Some(o),
        no_cache: true,
        ..Default::default()
    }
}

fn run(snap: &Arc<Snapshot>, q: &str, o: &QueryOptions) -> QueryResult {
    let q = format!("{P}{q}");
    query(snap.clone(), &q, o).unwrap_or_else(|e| panic!("{q}: {e}"))
}

fn term(t: Option<Term>) -> String {
    t.map_or("UNDEF".into(), |t| {
        t.to_string()
            .replace("<http://example.org/", "")
            .replace('>', "")
    })
}

/// The solutions as text, sorted (a multiset).
fn rows(r: &QueryResult) -> Vec<String> {
    let mut v: Vec<String> = r
        .rows()
        .into_iter()
        .map(|row| row.into_iter().map(term).collect::<Vec<_>>().join(" "))
        .collect();
    v.sort();
    v
}

fn find<'a>(p: &'a PlanInfo, op: &str) -> Option<&'a PlanInfo> {
    if p.operator == op {
        return Some(p);
    }
    p.children.iter().find_map(|c| find(c, op))
}

fn warned(r: &QueryResult, code: &str) -> bool {
    r.plan.warnings.iter().any(|w| w.code == code)
}

/// The answer with the rewrites equals the plain plan's; returns the rewritten result.
fn same(snap: &Arc<Snapshot>, q: &str) -> QueryResult {
    let fast = run(snap, q, &opts(true));
    let slow = run(snap, q, &opts(false));
    assert!(find(&slow.plan, "SpatialJoin").is_none(), "{q}");
    assert!(find(&slow.plan, "SpatialKnn").is_none(), "{q}");
    assert_eq!(rows(&fast), rows(&slow), "{q}");
    fast
}

/// [`same`], and the rewritten plan joins spatially.
fn joined(snap: &Arc<Snapshot>, q: &str) -> QueryResult {
    let r = same(snap, q);
    assert!(
        find(&r.plan, "SpatialJoin").is_some(),
        "{q}: not joined: {:#?}",
        r.plan
    );
    r
}

#[test]
fn a_spatial_join_finds_containment() {
    let s = Store::in_memory(StoreOptions::default());
    load(&s, FIXTURE);
    let q = "SELECT ?a ?b { ?a geo:asWKT ?wa . ?b geo:asWKT ?wb \
             FILTER(geof:sfContains(?wa, ?wb) && ?a != ?b) }";
    // without an index the components are packed into trees for the query
    let r = joined(&s.snapshot(), q);
    assert_eq!(rows(&r), ["gA g1", "gC g2"]);
    let j = find(&r.plan, "SpatialJoin").unwrap();
    assert_eq!(j.description, "?wa sfContains ?wb [tree join]");
    // each WKT geometry of the default graph contains itself (gM too), then gA g1
    // and gC g2
    let c = j.counters.as_ref().unwrap();
    assert_eq!(
        (&c["pairs"], &c["matched"]),
        (&8.into(), &8.into()),
        "{c:?}"
    );
    s.enable_geo(GeoConfig::default()).unwrap();
    let r = joined(&s.snapshot(), q);
    assert_eq!(rows(&r), ["gA g1", "gC g2"]);
    // a literal in an unknown CRS is not in the index, yet relates to itself: the
    // scans are read
    let j = find(&r.plan, "SpatialJoin").unwrap();
    assert!(j.description.ends_with("[tree join]"), "{}", j.description);
    let r = run(
        &s.snapshot(),
        "SELECT ?a { ?a geo:asWKT ?wa . ?b geo:asWKT ?wb \
         FILTER(geof:sfEquals(?wa, ?wb) && ?a = ex:gM) }",
        &opts(true),
    );
    assert_eq!(rows(&r), ["gM"]);
}

#[test]
fn an_indexed_scan_is_probed() {
    let s = Store::in_memory(StoreOptions::default());
    // the fixture without its literal in an unknown CRS
    let data: String = FIXTURE
        .lines()
        .filter(|l| !l.contains("mars"))
        .collect::<Vec<_>>()
        .join("\n");
    load(&s, &data);
    s.enable_geo(GeoConfig::default()).unwrap();
    let snap = s.snapshot();
    let q = "SELECT ?a ?b { ?a geo:asWKT ?wa . ?b geo:asWKT ?wb \
             FILTER(geof:sfContains(?wa, ?wb) && ?a != ?b) }";
    let r = joined(&snap, q);
    assert_eq!(rows(&r), ["gA g1", "gC g2"]);
    let j = find(&r.plan, "SpatialJoin").unwrap();
    assert_eq!(
        j.description,
        "?wa sfContains ?wb [index nested loop on <http://www.opengis.net/ont/geosparql#asWKT>]"
    );
    let c = j.counters.as_ref().unwrap();
    assert_eq!(
        (&c["index"], &c["fallback"]),
        (&"ready".into(), &false.into())
    );
    // a few features of one side: each of their geometries searches the index
    let q = "SELECT ?f ?b { ?f geo:hasDefaultGeometry ?ga . ?ga geo:asWKT ?wa . \
             ?b geo:asWKT ?wb FILTER(geof:sfIntersects(?wa, ?wb)) }";
    let r = joined(&snap, q);
    assert_eq!(
        rows(&r),
        [
            "A g1", "A gA", "A gB", "A gC", "B gA", "B gB", "B gC", "C g2", "C gA", "C gB", "C gC"
        ]
    );
    // distance joins in both argument orders and with the bound on either side
    for q in [
        "SELECT ?a ?b { ?a geo:asWKT ?wa . ?b geo:asWKT ?wb \
         FILTER(geof:metricDistance(?wa, ?wb) < 400000 && ?a != ?b) }",
        "SELECT ?a ?b { ?a geo:asWKT ?wa . ?b geo:asWKT ?wb \
         FILTER(400 >= geof:distance(?wb, ?wa, uom:kilometre) && ?a != ?b) }",
        "SELECT ?a ?b { ?a geo:asWKT ?wa . ?b geo:asWKT ?wb \
         FILTER(geof:distance(?wa, ?wb, uom:degree) < 3.6 && ?a != ?b) }",
    ] {
        let r = joined(&snap, q);
        assert!(
            rows(&r).contains(&"g1 gA".to_string()),
            "{q}: {:?}",
            rows(&r)
        );
    }
}

#[test]
fn unjoinable_shapes_warn() {
    let s = Store::in_memory(StoreOptions::default());
    load(&s, FIXTURE);
    s.enable_geo(GeoConfig::default()).unwrap();
    let snap = s.snapshot();
    for (q, why) in [
        (
            "SELECT ?a ?b { ?a geo:asWKT ?wa . ?b geo:asWKT ?wb FILTER(geof:sfDisjoint(?wa, ?wb)) }",
            "do not meet",
        ),
        (
            "SELECT ?a ?b { ?a geo:asWKT ?wa . ?b geo:asWKT ?wb \
             FILTER(geof:metricDistance(?wa, ?wb) > 1000) }",
            "lower bound",
        ),
        (
            "SELECT ?a ?b { ?a geo:asWKT ?wa . ?b geo:asWKT ?wb \
             FILTER(geof:relate(?wa, ?wb, \"FF*FF****\")) }",
            "do not intersect",
        ),
    ] {
        let r = same(&snap, q);
        assert!(find(&r.plan, "SpatialJoin").is_none(), "{q}");
        let w = r
            .plan
            .warnings
            .iter()
            .find(|w| w.code == "geo-not-joined")
            .unwrap_or_else(|| panic!("{q}: {:?}", r.plan.warnings));
        assert!(w.message.contains(why), "{q}: {}", w.message);
    }
    // a relate pattern that needs an intersection is joined
    joined(
        &snap,
        "SELECT ?a ?b { ?a geo:asWKT ?wa . ?b geo:asWKT ?wb \
         FILTER(geof:relate(?wa, ?wb, \"T*F**F***\")) }",
    );
}

const GA: &str = "\"POINT(9 1)\"^^geo:wktLiteral";

#[test]
fn nearest_neighbours_come_first() {
    let s = Store::in_memory(StoreOptions::default());
    load(&s, FIXTURE);
    s.enable_geo(GeoConfig::default()).unwrap();
    let snap = s.snapshot();
    let q = format!(
        "SELECT ?g ?d {{ ?g geo:asWKT ?w BIND(geof:metricDistance(?w, {GA}) AS ?d) \
         FILTER(BOUND(?d)) }} ORDER BY ?d LIMIT 2"
    );
    let r = same(&snap, &q);
    let k = find(&r.plan, "SpatialKnn").expect("nearest neighbours");
    assert_eq!(k.description, "?w k=2 metricDistance POINT(9 1)");
    assert_eq!(k.counters.as_ref().unwrap()["errorRows"], 0);
    let got: Vec<(String, f64)> = r
        .rows()
        .into_iter()
        .map(|row| {
            let d = match &row[1] {
                Some(Term::Literal(l)) => l.value().parse::<f64>().unwrap(),
                other => panic!("{other:?}"),
            };
            (term(row[0].clone()), d)
        })
        .collect();
    assert_eq!(got[0], ("gA".to_string(), 0.0));
    assert_eq!(got[1].0, "gC");
    // the geodesic from (9 1) to (10 1)
    assert!((got[1].1 - 111_302.6).abs() < 1.0, "{}", got[1].1);
    // without the filter, the rows whose distance is an error sort first
    let q = format!(
        "SELECT ?g ?d {{ ?g geo:asWKT ?w BIND(geof:metricDistance(?w, {GA}) AS ?d) }} \
         ORDER BY ?d LIMIT 2"
    );
    let fast = run(&snap, &q, &opts(true));
    let slow = run(&snap, &q, &opts(false));
    let k = find(&fast.plan, "SpatialKnn").expect("nearest neighbours");
    assert_eq!(k.counters.as_ref().unwrap()["errorRows"], 3);
    for r in [&fast, &slow] {
        let v = rows(r);
        assert_eq!(v.len(), 2);
        for row in v {
            let (g, d) = row.split_once(' ').unwrap();
            assert!(["gX", "gE", "gM"].contains(&g) && d == "UNDEF", "{row}");
        }
    }
    let q = format!(
        "SELECT ?g ?d {{ ?g geo:asWKT ?w BIND(geof:metricDistance(?w, {GA}) AS ?d) }} \
         ORDER BY ?d LIMIT 5"
    );
    assert_eq!(rows(&same(&snap, &q)).len(), 5);
    // the largest LIMIT saturates the search's batch size instead of overflowing
    let q = format!(
        "SELECT ?g ?d {{ ?g geo:asWKT ?w BIND(geof:metricDistance(?w, {GA}) AS ?d) \
         FILTER(BOUND(?d)) }} ORDER BY ?d OFFSET 1 LIMIT {}",
        u64::MAX
    );
    let all = format!(
        "SELECT ?g ?d {{ ?g geo:asWKT ?w BIND(geof:metricDistance(?w, {GA}) AS ?d) \
         FILTER(BOUND(?d)) }}"
    );
    let r = same(&snap, &q);
    assert!(find(&r.plan, "SpatialKnn").is_some(), "{:#?}", r.plan);
    assert_eq!(
        rows(&r).len(),
        rows(&run(&snap, &all, &opts(false))).len() - 1
    );
}

#[test]
fn nearest_neighbour_shapes_that_stay_generic_warn() {
    let s = Store::in_memory(StoreOptions::default());
    load(&s, FIXTURE);
    let q =
        format!("SELECT ?g {{ ?g geo:asWKT ?w }} ORDER BY geof:metricDistance(?w, {GA}) LIMIT 2");
    let r = same(&s.snapshot(), &q);
    assert!(warned(&r, "geo-not-knn"), "{:?}", r.plan.warnings);
    s.enable_geo(GeoConfig::default()).unwrap();
    let snap = s.snapshot();
    let r = same(&snap, &q);
    assert!(find(&r.plan, "SpatialKnn").is_some());
    for (q, why) in [
        (
            format!(
                "SELECT ?g {{ ?g geo:asWKT ?w }} ORDER BY DESC(geof:metricDistance(?w, {GA})) LIMIT 2"
            ),
            "descending",
        ),
        (
            format!(
                "SELECT ?g {{ ?g geo:asWKT ?w }} ORDER BY geof:distance(?w, {GA}, uom:degree) LIMIT 2"
            ),
            "angle",
        ),
        (
            format!(
                "SELECT ?g {{ ?g geo:asWKT ?w . ?x geo:hasGeometry ?y }} \
                 ORDER BY geof:metricDistance(?w, {GA}) LIMIT 2"
            ),
            "join",
        ),
    ] {
        let r = same(&snap, &q);
        assert!(find(&r.plan, "SpatialKnn").is_none(), "{q}");
        let w = r.plan.warnings.iter().find(|w| w.code == "geo-not-knn");
        if !why.is_empty() {
            let w = w.unwrap_or_else(|| panic!("{q}: {:?}", r.plan.warnings));
            assert!(w.message.contains(why), "{q}: {}", w.message);
        }
    }
}

// ------------------------------------------------------------------ random data ------

/// A seeded generator (xorshift).
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn f(&mut self, lo: f64, hi: f64) -> f64 {
        lo + (self.next() % 1_000_000) as f64 / 1_000_000.0 * (hi - lo)
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

fn r1(x: f64) -> f64 {
    (x * 10.0).round() / 10.0
}

/// A geometry literal: points, boxes on a grid (so neighbours touch), triangles and
/// lines, some in EPSG:4326 (latitude first); with `odd`, also malformed, empty and
/// unknown-CRS ones.
fn geometry(rng: &mut Rng, odd: bool) -> String {
    let (x, y) = (r1(rng.f(-12.0, 12.0)), r1(rng.f(-12.0, 12.0)));
    let wkt = match rng.below(if odd { 11 } else { 8 }) {
        0 | 1 => format!("POINT({x} {y})"),
        2 => {
            let (i, j) = (x.floor(), y.floor());
            format!(
                "POLYGON(({i} {j}, {} {j}, {} {}, {i} {}, {i} {j}))",
                i + 2.0,
                i + 2.0,
                j + 2.0,
                j + 2.0
            )
        }
        3 => format!(
            "POLYGON(({x} {y}, {} {y}, {x} {}, {x} {y}))",
            r1(x + rng.f(0.5, 4.0)),
            r1(y + rng.f(0.5, 4.0))
        ),
        4 => format!(
            "LINESTRING({x} {y}, {} {}, {} {})",
            r1(x + rng.f(-3.0, 3.0)),
            r1(y + rng.f(-3.0, 3.0)),
            r1(x + rng.f(-3.0, 3.0)),
            r1(y + rng.f(-3.0, 3.0))
        ),
        5 => format!("<http://www.opengis.net/def/crs/EPSG/0/4326> POINT({y} {x})"),
        6 => format!("MULTIPOINT(({x} {y}), ({} {}))", r1(-x), r1(y + 1.0)),
        // Web Mercator, where distances are measured in the projection
        7 => {
            let r = 6_378_137.0f64;
            let my = r
                * (std::f64::consts::FRAC_PI_4 + y.to_radians() / 2.0)
                    .tan()
                    .ln();
            format!(
                "<http://www.opengis.net/def/crs/EPSG/0/3857> POINT({} {})",
                (r * x.to_radians()).round(),
                my.round()
            )
        }
        8 => "POINT(1)".into(),
        9 => String::new(),
        _ => format!(
            "<http://example.org/crs/mars> POINT({} {})",
            x.round(),
            y.round()
        ),
    };
    format!("\"{wkt}\"^^geo:wktLiteral")
}

/// `n` geometries in the default graph and two named graphs (some in both), each with a
/// feature; a third of the features have a kind, a few are rare.
fn random_data(seed: u64, n: usize, odd: bool) -> String {
    let mut rng = Rng(seed);
    let mut out = String::from(
        "@prefix ex: <http://example.org/> .\n\
         @prefix geo: <http://www.opengis.net/ont/geosparql#> .\n",
    );
    for i in 0..n {
        let lit = geometry(&mut rng, odd);
        let triple = format!(
            "ex:f{i} geo:hasDefaultGeometry ex:g{i} . ex:g{i} geo:asWKT {lit} .{}{}\n",
            match i % 3 {
                0 => format!(" ex:f{i} ex:kind \"k{}\" .", rng.below(3)),
                _ => String::new(),
            },
            if i % 37 == 0 {
                format!(" ex:f{i} ex:rare true .")
            } else {
                String::new()
            }
        );
        match rng.below(6) {
            0 => out.push_str(&format!("ex:G1 {{ {triple} }}\n")),
            1 => out.push_str(&format!("ex:G2 {{ {triple} }}\n")),
            // the same triples in the default graph and a named graph
            2 => out.push_str(&format!("{triple}ex:G1 {{ {triple} }}\n")),
            _ => out.push_str(&triple),
        }
    }
    out
}

const JOIN_TESTS: &[&str] = &[
    "geof:sfIntersects(?wa, ?wb)",
    "geof:sfContains(?wa, ?wb)",
    "geof:sfWithin(?wa, ?wb)",
    "geof:sfTouches(?wa, ?wb)",
    "geof:sfCrosses(?wa, ?wb)",
    "geof:sfEquals(?wa, ?wb)",
    "geof:ehOverlap(?wb, ?wa)",
    "geof:rcc8ntpp(?wa, ?wb)",
    "geof:relate(?wa, ?wb, \"T*F**F***\")",
    "geof:relate(?wa, ?wb, \"****T****\")",
    "geof:metricDistance(?wa, ?wb) < 150000",
    "geof:distance(?wb, ?wa, uom:kilometre) <= 200",
    "1.5 > geof:distance(?wa, ?wb, uom:degree)",
];

/// Two components, each binding one geometry, in several shapes.
fn join_queries(test: &str) -> Vec<String> {
    vec![
        format!("SELECT ?a ?b {{ ?a geo:asWKT ?wa . ?b geo:asWKT ?wb FILTER({test}) }}"),
        format!(
            "SELECT ?f ?b {{ ?f ex:rare true ; geo:hasDefaultGeometry ?a . ?a geo:asWKT ?wa . \
             ?b geo:asWKT ?wb FILTER({test}) }}"
        ),
        format!(
            "SELECT ?f ?k ?b {{ ?f ex:kind ?k ; geo:hasDefaultGeometry ?a . ?a geo:asWKT ?wa . \
             ?h geo:hasDefaultGeometry ?b . ?b geo:asWKT ?wb FILTER({test} && ?k != \"k2\") }}"
        ),
        format!(
            "SELECT ?a ?b ?k {{ ?a geo:asWKT ?wa . ?f geo:hasDefaultGeometry ?b . \
             ?b geo:asWKT ?wb OPTIONAL {{ ?f ex:kind ?k }} FILTER({test}) }}"
        ),
        format!(
            "SELECT ?a ?b ?g ?h {{ GRAPH ?g {{ ?a geo:asWKT ?wa }} GRAPH ?h {{ ?b geo:asWKT ?wb }} \
             FILTER({test}) }}"
        ),
        format!(
            "SELECT ?a ?b ?g {{ GRAPH ?g {{ ?a geo:asWKT ?wa . ?b geo:asWKT ?wb FILTER({test}) }} }}"
        ),
        format!(
            "SELECT ?a ?b {{ GRAPH ex:G1 {{ ?a geo:asWKT ?wa }} ?b geo:asWKT ?wb FILTER({test}) }}"
        ),
        format!(
            "SELECT ?a ?b {{ VALUES ?a {{ ex:g1 ex:g2 ex:g3 ex:g1 }} ?a geo:asWKT ?wa . \
             ?b geo:asWKT ?wb FILTER({test}) }}"
        ),
        format!(
            "SELECT ?a ?b {{ ?a geo:asWKT ?wa . ?b geo:asWKT ?wb FILTER({test} && \
             NOT EXISTS {{ ?f geo:hasDefaultGeometry ?a ; ex:kind \"k0\" }}) }}"
        ),
    ]
}

/// Every join shape answers as the cross product with its filter, on `snap` and over a
/// merged default graph; returns how many rewritten plans probed the index.
fn joins_agree(snap: &Arc<Snapshot>) -> usize {
    let mut probed = 0;
    let union = |on: bool| QueryOptions {
        default_graph_uris: vec!["urn:x-arq:UnionGraph".into()],
        ..opts(on)
    };
    for test in JOIN_TESTS {
        for (i, q) in join_queries(test).iter().enumerate() {
            let r = same(snap, q);
            // the query whose geometries are in one GRAPH pattern is one component
            if i != 5 {
                let j = find(&r.plan, "SpatialJoin")
                    .unwrap_or_else(|| panic!("{q}: not joined: {:#?}", r.plan));
                if j.counters.as_ref().unwrap()["indexProbes"] == true {
                    probed += 1;
                }
            }
            if i == 0 || i == 3 {
                let fast = run(snap, q, &union(true));
                let slow = run(snap, q, &union(false));
                assert_eq!(rows(&fast), rows(&slow), "union default graph: {q}");
                assert!(find(&fast.plan, "SpatialJoin").is_some());
            }
        }
    }
    probed
}

fn knn_queries(c: &str, k: usize) -> Vec<String> {
    vec![
        format!(
            "SELECT ?g ?d {{ ?g geo:asWKT ?w BIND(geof:metricDistance(?w, {c}) AS ?d) \
             FILTER(BOUND(?d)) }} ORDER BY ?d LIMIT {k}"
        ),
        format!(
            "SELECT ?g ?d {{ ?g geo:asWKT ?w BIND(geof:metricDistance({c}, ?w) AS ?d) }} \
             ORDER BY ?d ?g LIMIT {k}"
        ),
        format!(
            "SELECT ?g ?d {{ ?g geo:asWKT ?w BIND(geof:distance(?w, {c}, uom:kilometre) AS ?d) \
             FILTER(?d < 1500) }} ORDER BY ?d LIMIT {k} OFFSET 2"
        ),
        format!(
            "SELECT ?g (geof:metricDistance(?w, {c}) AS ?d) {{ ?g geo:asWKT ?w }} \
             ORDER BY geof:metricDistance(?w, {c}) LIMIT {k}"
        ),
        format!(
            "SELECT ?g ?d ?k {{ ?f geo:hasDefaultGeometry ?g . ?g geo:asWKT ?w . \
             ?f ex:kind ?k BIND(geof:metricDistance(?w, {c}) AS ?d) FILTER(BOUND(?d)) }} \
             ORDER BY ?d LIMIT {k}"
        ),
        format!(
            "SELECT ?g ?d ?gr {{ GRAPH ?gr {{ ?g geo:asWKT ?w }} \
             BIND(geof:metricDistance(?w, {c}) AS ?d) FILTER(BOUND(?d)) }} ORDER BY ?d LIMIT {k}"
        ),
        format!(
            "SELECT ?g ?d ?f {{ ?g geo:asWKT ?w OPTIONAL {{ ?f geo:hasDefaultGeometry ?g }} \
             BIND(geof:metricDistance(?w, {c}) AS ?d) }} ORDER BY ?d LIMIT {k}"
        ),
        format!(
            "SELECT ?g ?d {{ ?g geo:asWKT ?w BIND(geof:metricDistance(?w, {c}) AS ?d) \
             FILTER(EXISTS {{ ?f geo:hasDefaultGeometry ?g ; ex:kind ?k }}) }} \
             ORDER BY ?d LIMIT {k}"
        ),
    ]
}

/// The ordering keys of the first rows (the distances; ties may pick other rows).
fn keys(r: &QueryResult) -> Vec<String> {
    let col = r.vars.iter().position(|v| v == "d").unwrap();
    r.rows()
        .into_iter()
        .map(|row| term(row[col].clone()))
        .collect()
}

/// Every nearest-neighbour shape orders as the generic sort does.
fn knn_agrees(snap: &Arc<Snapshot>, seed: u64) {
    let mut rng = Rng(seed);
    let union = |on: bool| QueryOptions {
        default_graph_uris: vec!["urn:x-arq:UnionGraph".into()],
        ..opts(on)
    };
    for k in [1, 3, 10, 40] {
        let c = format!(
            "\"POINT({} {})\"^^geo:wktLiteral",
            r1(rng.f(-15.0, 15.0)),
            r1(rng.f(-15.0, 15.0))
        );
        for (i, q) in knn_queries(&c, k).iter().enumerate() {
            let fast = run(snap, q, &opts(true));
            let slow = run(snap, q, &opts(false));
            assert_eq!(keys(&fast), keys(&slow), "{q}");
            assert!(
                find(&fast.plan, "SpatialKnn").is_some(),
                "{q}: {:?} {:#?}",
                fast.plan.warnings,
                fast.plan
            );
            if i == 0 || i == 6 {
                let fast = run(snap, q, &union(true));
                let slow = run(snap, q, &union(false));
                assert_eq!(keys(&fast), keys(&slow), "union default graph: {q}");
            }
        }
    }
}

#[test]
fn random_joins_and_nearest_neighbours_agree() {
    for (seed, odd) in [(7, false), (11, true)] {
        let s = Store::in_memory(StoreOptions::default());
        load(&s, &random_data(seed, 160, odd));
        // without an index: trees packed per query
        joins_agree(&s.snapshot());
        s.enable_geo(GeoConfig::default()).unwrap();
        let probed = joins_agree(&s.snapshot());
        if !odd {
            assert!(probed > 0, "no plan probed the index");
        }
        knn_agrees(&s.snapshot(), seed);
        // rows written after the build: in the tail, then the overlay
        let mut rng = Rng(seed + 1);
        let mut ins = String::from("INSERT DATA { ");
        for i in 0..40 {
            ins.push_str(&format!(
                "ex:n{i} geo:hasDefaultGeometry ex:m{i} . ex:m{i} geo:asWKT {} . ",
                geometry(&mut rng, odd)
            ));
        }
        ins.push_str("GRAPH ex:G2 { ex:m1 geo:asWKT \"POINT(0 0)\"^^geo:wktLiteral } }");
        sparkles_core::sparql::update::update(&s, &format!("{P}{ins}"), &QueryOptions::default())
            .unwrap();
        sparkles_core::sparql::update::update(
            &s,
            &format!("{P}DELETE WHERE {{ ex:g3 geo:asWKT ?w }} ; DELETE WHERE {{ ex:g10 ?p ?o }}"),
            &QueryOptions::default(),
        )
        .unwrap();
        joins_agree(&s.snapshot());
        knn_agrees(&s.snapshot(), seed + 2);
    }
}

#[test]
fn the_candidate_budget_is_the_row_limit() {
    let s = Store::in_memory(StoreOptions::default());
    load(&s, &random_data(3, 200, false));
    s.enable_geo(GeoConfig::default()).unwrap();
    let q = format!(
        "{P}SELECT ?a ?b {{ ?a geo:asWKT ?wa . ?b geo:asWKT ?wb \
         FILTER(geof:metricDistance(?wa, ?wb) < 5000000) }}"
    );
    let o = QueryOptions {
        max_rows: Some(1000),
        ..opts(true)
    };
    match query(s.snapshot(), &q, &o) {
        Err(sparkles_core::Error::BudgetExceeded(b)) => {
            assert_eq!((b.kind, b.limit), (sparkles_core::BudgetKind::Rows, 1000));
        }
        other => panic!("{:?}", other.map(|r| r.table.len())),
    }
}

/// A reopened database reads its index from files; W3C Basic Geo points (indexed under
/// `wgs84_pos:lat`) are not geometry literals and stay out of joins and orders over
/// `geo:asWKT`.
#[test]
fn a_reopened_index_with_basic_geo_points_agrees() {
    let dir = tempfile::tempdir().unwrap();
    let mut data = random_data(5, 120, true);
    data.push_str(
        "@prefix wgs: <http://www.w3.org/2003/01/geo/wgs84_pos#> .\n\
         ex:here wgs:lat 1.5 ; wgs:long 2.5 .\n",
    );
    {
        let s = Store::open(dir.path(), StoreOptions::default()).unwrap();
        load(&s, &data);
        s.compact().unwrap();
        s.enable_geo(GeoConfig {
            wgs84: true,
            ..GeoConfig::default()
        })
        .unwrap();
    }
    let s = Store::open(dir.path(), StoreOptions::default()).unwrap();
    assert_eq!(s.wait_geo().unwrap().state, "ready");
    let snap = s.snapshot();
    joins_agree(&snap);
    knn_agrees(&snap, 5);
}
