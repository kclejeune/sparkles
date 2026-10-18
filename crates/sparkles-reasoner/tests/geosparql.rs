//! The GeoSPARQL vocabulary (`infer --vocab geosparql`) and default geometries
//! (`infer --geo-default-geometry`).

use oxrdf::Term;
use sparkles_core::io::{RdfFormat, Source};
use sparkles_core::sparql::{QueryOptions, query};
use sparkles_core::store::{Store, StoreOptions};
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
    let rewrite = sparkles_core::geo::GeoConfig {
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

const TYPED: &str = r#"
@prefix ex: <http://example.org/> .
@prefix geo: <http://www.opengis.net/ont/geosparql#> .
ex:w geo:asWKT "<http://www.opengis.net/def/crs/OGC/1.3/CRS84> Polygon((0 0, 1 0, 1 1, 0 0))"^^geo:wktLiteral .
ex:l geo:asWKT "LINESTRING Z (0 0 1, 1 1 1)"^^geo:wktLiteral .
ex:j geo:asGeoJSON "{\"type\":\"MultiPoint\",\"coordinates\":[[0,0]]}"^^geo:geoJSONLiteral .
ex:m geo:asGML "<gml:Polygon xmlns:gml=\"http://www.opengis.net/gml/3.2\"><gml:exterior><gml:LinearRing><gml:posList>0 0 1 0 1 1 0 0</gml:posList></gml:LinearRing></gml:exterior></gml:Polygon>"^^geo:gmlLiteral .
ex:k geo:asKML "<Point><coordinates>1,2</coordinates></Point>"^^geo:kmlLiteral .
ex:s geo:hasSerialization "POINT(1 2)"^^geo:wktLiteral .
ex:e geo:asWKT ""^^geo:wktLiteral .
ex:x geo:asWKT "POINT(1 2)" .
"#;

#[test]
fn geometries_are_typed_from_their_serializations() {
    let s = Store::in_memory(StoreOptions::default());
    s.load(&[Source::from_bytes(
        TYPED.as_bytes().to_vec(),
        RdfFormat::Turtle,
        None,
    )])
    .unwrap();
    run(&s, &["geosparql"], false);
    let typed = |c: &str| select(&s, &format!("SELECT ?x {{ ?x a {c} }}"), true);
    assert_eq!(typed("sf:Polygon"), ["m", "w"]);
    assert_eq!(typed("sf:Surface"), ["m", "w"]);
    assert_eq!(typed("sf:Curve"), ["l"]);
    assert_eq!(typed("sf:MultiPoint"), ["j"]);
    assert_eq!(typed("sf:Point"), ["k", "s"]);
    // the GML type of a GML literal, with GML's hierarchy
    assert_eq!(
        typed("<http://www.opengis.net/ont/gml#AbstractSurface>"),
        ["m"]
    );
    // an empty WKT literal declares no type, and a plain string is no serialization
    assert_eq!(typed("sf:Geometry"), ["j", "k", "l", "m", "s", "w"]);
    assert!(!typed("sf:Geometry").contains(&"x".to_string()));
    // without the vocabulary, nothing is typed
    let s2 = Store::in_memory(StoreOptions::default());
    s2.load(&[Source::from_bytes(
        TYPED.as_bytes().to_vec(),
        RdfFormat::Turtle,
        None,
    )])
    .unwrap();
    run(&s2, &[], false);
    assert!(select(&s2, "SELECT ?x { ?x a sf:Point }", true).is_empty());
}

/// Incremental runs with default geometries write what a full run writes, from a closure
/// kept in memory and from one read back from the dataset.
#[test]
fn default_geometries_follow_changes_incrementally() {
    use sparkles_reasoner::{Cache, Incremental, Method, materialize_incremental};
    let inferred = |s: &Store| {
        let mut v = select_all(s);
        v.sort();
        v
    };
    let changes = [
        "INSERT DATA { ex:p1 geo:hasGeometry ex:g1b }",
        "INSERT DATA { ex:p4 geo:hasGeometry ex:g4 }",
        "DELETE DATA { ex:p2 geo:hasGeometry ex:g2b }",
        "INSERT DATA { ex:p4 geo:hasDefaultGeometry ex:g4 }",
        "DELETE DATA { ex:p4 geo:hasDefaultGeometry ex:g4 }",
        "DELETE DATA { ex:p3 geo:hasDefaultGeometry ex:g3b }",
        "DELETE DATA { ex:p1 geo:hasGeometry ex:g1b }",
    ];
    let dir = tempfile::tempdir().unwrap();
    let persistent = Store::open(&dir.path().join("db"), StoreOptions::default()).unwrap();
    persistent
        .load(&[Source::from_bytes(
            DATA.as_bytes().to_vec(),
            RdfFormat::Turtle,
            None,
        )])
        .unwrap();
    for (s, cache) in [(store(), Some(Cache::new(1 << 20))), (persistent, None)] {
        let extras = Extras::parse(&["geosparql"], true).unwrap();
        let go = |since: Option<u64>| {
            materialize_incremental(
                &s,
                &Profile::Rdfs,
                &extras,
                Incremental {
                    since,
                    cache: cache.as_ref(),
                },
                &ReasonOptions::default(),
            )
            .unwrap_or_else(|e| panic!("{e:#}"))
        };
        let mut since = go(None).receipt.unwrap().commit.seq;
        for (i, u) in changes.iter().enumerate() {
            sparkles_core::sparql::update::update(&s, &format!("{P}{u}"), &QueryOptions::default())
                .unwrap();
            let r = go(Some(since));
            assert_eq!(r.method, Method::Incremental, "{u}: {:?}", r.fallback);
            let got = inferred(&s);
            // every other step continues from a full run's closure
            let r = go(None);
            assert_eq!(got, inferred(&s), "{u}");
            if i % 2 == 0 {
                since = r.receipt.unwrap().commit.seq;
            } else {
                sparkles_core::sparql::update::update(
                    &s,
                    &format!("{P}INSERT DATA {{ ex:z ex:step {i} }}"),
                    &QueryOptions::default(),
                )
                .unwrap();
                since = go(Some(r.receipt.unwrap().commit.seq))
                    .receipt
                    .unwrap()
                    .commit
                    .seq;
            }
        }
        assert_eq!(
            select(&s, "SELECT ?x { ?x geo:hasDefaultGeometry ?g }", true),
            ["A", "p1", "p2", "p3", "p4"]
        );
    }
}

/// The triples of the inferred graph, as N-Triples.
fn select_all(s: &Store) -> Vec<String> {
    let q = format!("SELECT ?s ?p ?o {{ GRAPH <{INFERRED_GRAPH}> {{ ?s ?p ?o }} }}");
    let r = query(s.snapshot(), &q, &QueryOptions::default()).unwrap();
    r.rows()
        .into_iter()
        .map(|row| {
            row.into_iter()
                .map(|t| t.map_or(String::new(), |t| t.to_string()))
                .collect::<Vec<_>>()
                .join(" ")
        })
        .collect()
}

/// A copy of the default graph of `s` in a new store.
fn copy(s: &Store) -> Store {
    use sparkles_core::id::Id;
    use sparkles_core::index::Perm;
    let c = Store::in_memory(StoreOptions::default());
    let snap = s.snapshot();
    let mut txn = c.write();
    let mut labels = Default::default();
    for k in snap.scan_keys(Perm::Gspo, &[Id::DEFAULT_GRAPH.0]).unwrap() {
        let q = snap.quad_to_terms(&Perm::Gspo.to_quad(&k)).unwrap();
        let q = txn.encode_quad(&q, &mut labels).unwrap();
        txn.insert(q).unwrap();
    }
    txn.commit().unwrap();
    c
}

/// F16 review finding F9: an incremental run with default geometries reads only the
/// features whose geometry facts changed, with probes bound to the feature, when it
/// continues from a closure kept in memory. A closure read back from the dataset does
/// not know its default geometries, and that run reads them with one scan per property.
///
/// `baseScans` also counts the reads of the input graphs that rule evaluation makes, the
/// same for both closures, so the test compares the two runs of each step: the closure
/// read back costs the two scans of the default geometries more.
#[test]
fn f16_default_geometry_changed_subjects() {
    use sparkles_reasoner::{Cache, Incremental, Method, materialize_incremental};
    let dir = tempfile::tempdir().unwrap();
    let persistent = Store::open(&dir.path().join("db"), StoreOptions::default()).unwrap();
    persistent
        .load(&[Source::from_bytes(
            DATA.as_bytes().to_vec(),
            RdfFormat::Turtle,
            None,
        )])
        .unwrap();
    // (update, scans with the closure in memory, scans with the closure read back)
    let steps = [
        ("INSERT DATA { ex:z ex:unrelated 1 }", 0, 2),
        ("INSERT DATA { ex:p1 geo:hasGeometry ex:g1b }", 2, 4),
        ("DELETE DATA { ex:p2 geo:hasGeometry ex:g2b }", 2, 4),
        (
            "INSERT DATA { ex:p4 geo:hasGeometry ex:g4 . ex:p3 geo:hasGeometry ex:g3c }",
            4,
            6,
        ),
        ("DELETE DATA { ex:p3 geo:hasDefaultGeometry ex:g3b }", 2, 4),
        ("INSERT DATA { ex:p4 geo:hasDefaultGeometry ex:g4 }", 2, 4),
        ("INSERT DATA { ex:z ex:unrelated 2 }", 0, 2),
    ];
    // per closure, the scans of each step
    let mut scans: Vec<Vec<u64>> = Vec::new();
    for (s, cache) in [(store(), Some(Cache::new(1 << 20))), (persistent, None)] {
        scans.push(Vec::new());
        let extras = Extras::parse::<&str>(&[], true).unwrap();
        let go = |since: Option<u64>| {
            materialize_incremental(
                &s,
                &Profile::Rdfs,
                &extras,
                Incremental {
                    since,
                    cache: cache.as_ref(),
                },
                &ReasonOptions::default(),
            )
            .unwrap_or_else(|e| panic!("{e:#}"))
        };
        let mut since = go(None).receipt.unwrap().commit.seq;
        for (u, in_memory, read_back) in steps {
            sparkles_core::sparql::update::update(&s, &format!("{P}{u}"), &QueryOptions::default())
                .unwrap();
            let r = go(Some(since));
            assert_eq!(r.method, Method::Incremental, "{u}: {:?}", r.fallback);
            let want = if cache.is_some() {
                in_memory
            } else {
                read_back
            };
            assert!(r.base_scans >= want, "{u}: {} scans", r.base_scans);
            scans.last_mut().unwrap().push(r.base_scans);
            since = r.receipt.unwrap().commit.seq;
            // a full run over a copy writes the same
            let mut got = select_all(&s);
            got.sort();
            let c = copy(&s);
            materialize_with(&c, &Profile::Rdfs, &extras, &ReasonOptions::default()).unwrap();
            let mut full = select_all(&c);
            full.sort();
            assert_eq!(got, full, "{u}");
        }
    }
    for (i, (u, in_memory, read_back)) in steps.iter().enumerate() {
        assert_eq!(
            scans[1][i] - scans[0][i],
            read_back - in_memory,
            "{u}: {} scans with the closure in memory, {} with it read back",
            scans[0][i],
            scans[1][i]
        );
    }
}
