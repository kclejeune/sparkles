//! The GeoSPARQL vocabulary (`infer --vocab geosparql`) and default geometries
//! (`infer --geo-default-geometry`).

use oxrdf::Term;
use sparkles::io::{RdfFormat, Source};
use sparkles::sparql::{QueryOptions, query};
use sparkles::store::{Store, StoreOptions};
use sparkles_reasoner::{
    Extras, INFERRED_GRAPH, Profile, ReasonOptions, ReasonReport, clear, materialize_with,
};

const DATA: &str = r#"
@prefix ex: <http://example.org/> .
@prefix geo: <http://www.opengis.net/ont/geosparql#> .
@prefix sf: <http://www.opengis.net/ont/sf#> .
ex:A geo:hasDefaultGeometry ex:gA .
ex:gA a sf:Polygon ; geo:asWKT "POLYGON((0 0, 10 0, 10 10, 0 10, 0 0))"^^geo:wktLiteral .
ex:p1 geo:hasGeometry ex:g1 . ex:g1 geo:asWKT "POINT(2 2)"^^geo:wktLiteral .
ex:p2 geo:hasGeometry ex:g2a , ex:g2b .
ex:p3 geo:hasGeometry ex:g3 ; geo:hasDefaultGeometry ex:g3b .
ex:A geo:sfContains ex:p1 .
"#;

const P: &str = "PREFIX ex: <http://example.org/>
PREFIX geo: <http://www.opengis.net/ont/geosparql#>
PREFIX sf: <http://www.opengis.net/ont/sf#>
PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#>
";

fn store() -> Store {
    let s = Store::in_memory(StoreOptions::default());
    s.load(&[Source::from_bytes(
        DATA.as_bytes().to_vec(),
        RdfFormat::Turtle,
        None,
    )])
    .unwrap();
    s
}

fn run(s: &Store, vocab: &[&str], default_geometry: bool) -> ReasonReport {
    let extras = Extras::parse(vocab, default_geometry).unwrap();
    materialize_with(s, &Profile::Rdfs, &extras, &ReasonOptions::default())
        .unwrap_or_else(|e| panic!("{e:#}"))
}

/// `?x` of each solution (`ex:` IRIs as local names), sorted.
fn select(s: &Store, q: &str, reasoning: bool) -> Vec<String> {
    let o = QueryOptions {
        default_graph_extra: if reasoning {
            vec![INFERRED_GRAPH.into()]
        } else {
            Vec::new()
        },
        ..Default::default()
    };
    let r = query(s.snapshot(), &format!("{P}{q}"), &o).unwrap_or_else(|e| panic!("{q}: {e}"));
    let mut v: Vec<String> = r
        .rows()
        .into_iter()
        .map(|row| match row.into_iter().next().flatten() {
            Some(Term::NamedNode(n)) => n.as_str().replace("http://example.org/", ""),
            other => format!("{other:?}"),
        })
        .collect();
    v.sort();
    v
}

#[test]
fn the_geometry_hierarchy_is_entailed() {
    let s = store();
    run(&s, &[], false);
    assert!(!select(&s, "SELECT ?x { ?x a geo:Geometry }", true).contains(&"gA".into()));
    run(&s, &["geosparql"], false);
    // sf:Polygon ⊑ sf:Surface ⊑ sf:Geometry ⊑ geo:Geometry, and ranges and domains
    assert_eq!(
        select(&s, "SELECT ?x { ?x a geo:Geometry }", true),
        ["g1", "g2a", "g2b", "g3", "g3b", "gA"]
    );
    assert_eq!(select(&s, "SELECT ?x { ?x a sf:Surface }", true), ["gA"]);
    assert_eq!(
        select(&s, "SELECT ?x { ?x a geo:Feature }", true),
        ["A", "p1", "p2", "p3"]
    );
    // hasDefaultGeometry ⊑ hasGeometry, asWKT ⊑ hasSerialization
    assert_eq!(
        select(&s, "SELECT ?x { ex:A geo:hasGeometry ?x }", true),
        ["gA"]
    );
    assert_eq!(
        select(&s, "SELECT ?x { ?x geo:hasSerialization ?w }", true),
        ["g1", "gA"]
    );
    // the topological properties relate spatial objects
    assert!(select(&s, "SELECT ?x { ?x a geo:SpatialObject }", true).contains(&"p1".into()));
    // the axioms are inferences only
    assert_eq!(
        select(&s, "SELECT ?x { sf:Polygon rdfs:subClassOf ?x }", false),
        Vec::<String>::new()
    );
    assert!(
        select(&s, "SELECT ?x { sf:Polygon rdfs:subClassOf ?x }", true)
            .contains(&"http://www.opengis.net/ont/sf#Surface".to_string())
    );
}

#[test]
fn features_with_one_geometry_get_it_as_default() {
    let s = store();
    run(&s, &[], true);
    // p1 has one geometry; p2 has two; p3 and A have a default geometry already
    assert_eq!(
        select(&s, "SELECT ?x { ?x geo:hasDefaultGeometry ?g }", true),
        ["A", "p1", "p3"]
    );
    assert_eq!(
        select(&s, "SELECT ?g { ex:p1 geo:hasDefaultGeometry ?g }", false),
        Vec::<String>::new()
    );
    // the rules see them: with the vocabulary, the default geometry is a geometry of a
    // feature
    run(&s, &["geosparql"], true);
    assert!(select(&s, "SELECT ?x { ?x a geo:Feature }", true).contains(&"p1".into()));
    // a run without the switch drops them, as clearing does
    run(&s, &[], false);
    assert_eq!(
        select(&s, "SELECT ?x { ?x geo:hasDefaultGeometry ?g }", true),
        ["A", "p3"]
    );
    run(&s, &[], true);
    clear(&s).unwrap();
    assert_eq!(
        select(&s, "SELECT ?x { ?x geo:hasDefaultGeometry ?g }", true),
        ["A", "p3"]
    );
}

/// Query rewrite over the materialized default geometries: `ex:p1` now relates through
/// `ex:g1`, and `ex:A` (asserted and derived) is one solution.
#[test]
fn rewrite_sees_materialized_default_geometries() {
    let s = store();
    let rewrite = sparkles::geo::GeoConfig {
        query_rewrite: true,
        ..Default::default()
    };
    if s.enable_geo(rewrite).is_err() {
        // built without the `geo` feature of `sparkles`
        return;
    }
    let q = "SELECT ?x { ?x geo:sfContains ex:p1 }";
    assert_eq!(select(&s, q, true), ["A"]);
    run(&s, &[], true);
    assert_eq!(select(&s, q, true), ["A", "g1", "gA", "p1"]);
    assert_eq!(select(&s, q, false), ["A"]);
}
