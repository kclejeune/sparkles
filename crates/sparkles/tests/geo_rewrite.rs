//! GeoSPARQL query rewrite and `spatial:equals` through the public API: the topological
//! properties match asserted and derived triples (features through
//! `geo:hasDefaultGeometry`, geometries through their serializations, literals written
//! in the query), with the answers of the equivalent `geof:` FILTER query.
#![cfg(feature = "geo")]

use oxrdf::Term;
use sparkles::geo::{GeoConfig, Relation};
use sparkles::io::{RdfFormat, Source};
use sparkles::sparql::{Optimizations, PlanInfo, QueryOptions, query};
use sparkles::store::{Store, StoreOptions};

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
ex:A geo:sfContains ex:p1 .
"#;

const P: &str = "PREFIX ex: <http://example.org/>
PREFIX geo: <http://www.opengis.net/ont/geosparql#>
PREFIX geof: <http://www.opengis.net/def/function/geosparql/>
PREFIX spatial: <http://jena.apache.org/spatial#>
";

const GEO: &str = "http://www.opengis.net/ont/geosparql#";

fn load(s: &Store, data: &str) {
    s.load(&[Source::from_bytes(
        data.as_bytes().to_vec(),
        RdfFormat::TriG,
        None,
    )])
    .unwrap();
}

/// The fixture with the spatial index on and query rewrite as given.
fn store(rewrite: bool) -> Store {
    let s = Store::in_memory(StoreOptions::default());
    load(&s, FIXTURE);
    s.enable_geo(GeoConfig {
        query_rewrite: rewrite,
        ..GeoConfig::default()
    })
    .unwrap();
    s
}

fn opts() -> QueryOptions {
    QueryOptions {
        no_cache: true,
        ..Default::default()
    }
}

/// Each solution as its values (`ex:` IRIs as local names) joined by spaces, sorted.
fn rows(s: &Store, q: &str, o: &QueryOptions) -> Vec<String> {
    let r = query(s.snapshot(), &format!("{P}{q}"), o).unwrap_or_else(|e| panic!("{q}: {e}"));
    if q.starts_with("ASK") {
        return vec![r.boolean.to_string()];
    }
    let mut v: Vec<String> = r
        .rows()
        .into_iter()
        .map(|row| {
            row.into_iter()
                .map(|t| match t {
                    Some(Term::NamedNode(n)) => n.as_str().replace("http://example.org/", ""),
                    Some(t) => t.to_string(),
                    None => "UNDEF".into(),
                })
                .collect::<Vec<_>>()
                .join(" ")
        })
        .collect();
    v.sort();
    v
}

fn select(s: &Store, q: &str) -> Vec<String> {
    rows(s, q, &opts())
}

fn find<'a>(p: &'a PlanInfo, op: &str) -> Option<&'a PlanInfo> {
    if p.operator == op {
        return Some(p);
    }
    p.children.iter().find_map(|c| find(c, op))
}

#[test]
fn asserted_and_derived_triples() {
    let s = store(true);
    // a point contains itself; the feature through its default geometry
    assert_eq!(
        select(&s, "SELECT ?x { ?x geo:sfContains ex:g1 }"),
        ["A", "g1", "gA"]
    );
    // ex:p1 has a geometry but no default geometry: only the asserted triple
    assert_eq!(select(&s, "SELECT ?x { ?x geo:sfContains ex:p1 }"), ["A"]);
    // the other direction, and both ends constant
    assert_eq!(
        select(&s, "SELECT ?y { ex:A geo:sfContains ?y }"),
        ["A", "g1", "gA", "p1"]
    );
    assert_eq!(select(&s, "ASK { ex:gA geo:sfContains ex:g1 }"), ["true"]);
    assert_eq!(select(&s, "ASK { ex:gA geo:sfWithin ex:g1 }"), ["false"]);
    // a literal end
    assert_eq!(
        select(
            &s,
            "SELECT ?x { ?x geo:sfWithin \"POLYGON((1 1, 3 1, 3 3, 1 3, 1 1))\"^^geo:wktLiteral }"
        ),
        ["g1"]
    );
    // an EPSG:4326 geometry is compared in its own axes (lon 12, lat 2: in C)
    assert_eq!(
        select(&s, "SELECT ?x { ?x geo:sfContains ex:g2 }"),
        ["C", "g2", "gC"]
    );
    // the named graph only under GRAPH, with ?g bound
    assert_eq!(
        select(&s, "SELECT ?x ?g { GRAPH ?g { ?x geo:sfWithin ?y } }"),
        ["g4 G1"]
    );
    // the feature and its geometry relate to each other
    assert_eq!(
        select(&s, "SELECT ?y { ex:B geo:sfEquals ?y }"),
        ["B", "gB"]
    );
    // the unknown CRS equals itself (as the function says), and no one else
    assert_eq!(select(&s, "SELECT ?y { ex:gM geo:sfEquals ?y }"), ["gM"]);
    // a predicate variable matches the asserted triples only
    assert_eq!(
        select(&s, "SELECT ?p { ex:A ?p ex:p1 }"),
        [format!("{GEO}sfContains")]
    );
}

#[test]
fn explain_names_the_operator() {
    let s = store(true);
    let r = query(
        s.snapshot(),
        &format!("{P}SELECT ?x {{ ?x geo:sfContains ex:g1 }}"),
        &opts(),
    )
    .unwrap();
    let n = find(&r.plan, "SpatialRelate").expect("a SpatialRelate node");
    assert_eq!(
        n.description,
        "?x geo:sfContains <http://example.org/g1> [asserted ∪ derived]"
    );
    let c = n.counters.as_ref().unwrap();
    assert_eq!(c["index"], "ready");
    assert_eq!(c["asserted"], 0);
    let r = query(
        s.snapshot(),
        &format!("{P}SELECT ?x {{ ?x spatial:equals ex:A }}"),
        &opts(),
    )
    .unwrap();
    let n = find(&r.plan, "SpatialRelate").unwrap();
    assert!(n.description.ends_with("[derived]"), "{}", n.description);
}

#[test]
fn rewrite_is_off_unless_configured() {
    // the index alone does not rewrite
    let s = store(false);
    assert_eq!(select(&s, "SELECT ?x { ?x geo:sfContains ex:g1 }"), [""; 0]);
    assert_eq!(select(&s, "SELECT ?x { ?x geo:sfContains ex:p1 }"), ["A"]);
    // the server's switch overrides geo.json
    let s = Store::in_memory(StoreOptions {
        geo_query_rewrite: false,
        ..StoreOptions::default()
    });
    load(&s, FIXTURE);
    let st = s
        .enable_geo(GeoConfig {
            query_rewrite: true,
            ..GeoConfig::default()
        })
        .unwrap();
    assert!(!st.config.query_rewrite);
    assert_eq!(select(&s, "SELECT ?x { ?x geo:sfContains ex:g1 }"), [""; 0]);
    // geo.json may now ask for it
    assert!(
        GeoConfig {
            query_rewrite: true,
            ..GeoConfig::default()
        }
        .validate()
        .is_ok()
    );
}

#[test]
fn spatial_equals_needs_neither_rewrite_nor_an_index() {
    let s = Store::in_memory(StoreOptions::default());
    load(
        &s,
        &format!(
            "{FIXTURE}
            @prefix spatial: <http://jena.apache.org/spatial#> .
            ex:D geo:hasDefaultGeometry ex:gD .
            ex:gD geo:asWKT \"POLYGON((0 0, 0 10, 10 10, 10 0, 0 0))\"^^geo:wktLiteral .
            ex:A spatial:equals ex:B ."
        ),
    );
    for s in [&s, &store(false)] {
        // between features, geometries and literals; never the asserted triple
        let a = select(s, "SELECT ?y { ex:A spatial:equals ?y }");
        assert!(a == ["A", "D", "gA", "gD"] || a == ["A", "gA"], "{a:?}");
        assert_eq!(
            select(
                s,
                "SELECT ?y { ?y spatial:equals \"Polygon ((10 10, 0 10, 0 0, 10 0, 10 10))\"^^geo:wktLiteral }"
            )
            .iter()
            .filter(|x| ["A", "gA"].contains(&x.as_str()))
            .count(),
            2
        );
        assert_eq!(
            select(
                s,
                "ASK { \"POINT(1 1)\"^^geo:wktLiteral spatial:equals \"Point (1.0 1.0)\"^^geo:wktLiteral }"
            ),
            ["true"]
        );
        assert_eq!(select(s, "SELECT ?y { ex:p1 spatial:equals ?y }"), [""; 0]);
    }
    assert_eq!(
        select(&s, "SELECT ?y { ex:A spatial:equals ?y }"),
        ["A", "D", "gA", "gD"]
    );
}

#[test]
fn derived_default_geometries_join_in() {
    // as `infer --geo-default-geometry` writes them: into the inferred graph
    let s = store(true);
    load(
        &s,
        "<urn:x-sparkles:inferred> { <http://example.org/p1> \
         <http://www.opengis.net/ont/geosparql#hasDefaultGeometry> <http://example.org/g1> }",
    );
    let reasoning = QueryOptions {
        default_graph_extra: vec!["urn:x-sparkles:inferred".into()],
        ..opts()
    };
    assert_eq!(
        rows(&s, "SELECT ?x { ?x geo:sfContains ex:p1 }", &reasoning),
        ["A", "g1", "gA", "p1"]
    );
    // without the inferred graph, as before
    assert_eq!(select(&s, "SELECT ?x { ?x geo:sfContains ex:p1 }"), ["A"]);
}

#[test]
fn constants_with_the_same_description_have_different_cache_keys() {
    let s = store(true);
    let cached = QueryOptions::default();
    let q = |poly: &str| {
        format!("SELECT ?x {{ ?x geo:sfWithin \"POLYGON(({poly}))\"^^geo:wktLiteral }}")
    };
    let a = rows(&s, &q("1 1, 3 1, 3 3, 1 3, 1 1"), &cached);
    let b = rows(&s, &q("29 29, 31 29, 31 31, 29 31, 29 29"), &cached);
    assert_eq!((a, b), (vec!["g1".to_string()], vec!["g3".to_string()]));
}

#[test]
fn disjoint_pairs_stay_within_the_row_budget() {
    let s = store(true);
    let small = QueryOptions {
        max_rows: Some(20),
        ..opts()
    };
    let r = query(
        s.snapshot(),
        &format!("{P}SELECT * {{ ?x geo:sfDisjoint ?y }}"),
        &small,
    );
    assert!(r.is_err());
    // ex:p3 (30 30) is disjoint from every polygon and point but itself
    let d = select(&s, "SELECT ?y { ex:g3 geo:sfDisjoint ?y }");
    assert!(d.contains(&"gA".to_string()) && !d.contains(&"g3".to_string()));
    // an empty geometry is disjoint from everything
    assert!(select(&s, "SELECT ?y { ex:gE geo:sfDisjoint ?y }").contains(&"gA".to_string()));
}

// ------------------------------------------------------------------ equivalence --

/// A small random generator (xorshift), for seeded data.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
    fn coord(&mut self) -> f64 {
        self.below(40) as f64 / 2.0
    }
}

/// Random geometries (points, boxes, lines, some in EPSG:4326, an unknown CRS, empty and
/// malformed literals), features with default geometries (some with two, some with
/// `hasGeometry` only), a named graph, and asserted topological triples.
fn random_data(seed: u64) -> String {
    let mut r = Rng(seed);
    let mut out = String::from(
        "@prefix ex: <http://example.org/> .\n@prefix geo: <http://www.opengis.net/ont/geosparql#> .\n",
    );
    let wkt = |r: &mut Rng| -> String {
        let lit = match r.below(10) {
            0..=2 => format!("POINT({} {})", r.coord(), r.coord()),
            3..=5 => {
                let (x, y) = (r.coord(), r.coord());
                let (w, h) = (1.0 + r.below(8) as f64, 1.0 + r.below(8) as f64);
                format!(
                    "POLYGON(({x} {y}, {} {y}, {} {}, {x} {}, {x} {y}))",
                    x + w,
                    x + w,
                    y + h,
                    y + h
                )
            }
            6 | 7 => format!(
                "LINESTRING({} {}, {} {})",
                r.coord(),
                r.coord(),
                r.coord(),
                r.coord()
            ),
            8 => format!(
                "<http://www.opengis.net/def/crs/EPSG/0/4326> POINT({} {})",
                r.coord(),
                r.coord()
            ),
            _ => match r.below(3) {
                0 => "<http://example.org/crs/mars> POINT(1 1)".into(),
                1 => String::new(),
                _ => "POINT(1".into(),
            },
        };
        format!("\"{lit}\"^^geo:wktLiteral")
    };
    for i in 0..36 {
        let g = format!("ex:geom{i}");
        let mut triples = format!("{g} geo:asWKT {} .\n", wkt(&mut r));
        if r.below(6) == 0 {
            triples += &format!("{g} geo:asWKT {} .\n", wkt(&mut r));
        }
        match r.below(4) {
            0 => {}
            1 => triples += &format!("ex:f{i} geo:hasGeometry {g} .\n"),
            _ => triples += &format!("ex:f{i} geo:hasDefaultGeometry {g} .\n"),
        }
        if r.below(5) == 0 {
            // a second default geometry for an earlier feature
            triples += &format!("ex:f{} geo:hasDefaultGeometry {g} .\n", r.below(i + 1));
        }
        if r.below(6) == 0 {
            out += &format!("ex:G{} {{\n{triples}}}\n", r.below(2));
        } else {
            out += &triples;
        }
    }
    for _ in 0..12 {
        let rel = Relation::ALL[r.below(24) as usize];
        out += &format!(
            "ex:f{} geo:{} ex:geom{} .\n",
            r.below(36),
            rel.local(),
            r.below(36)
        );
    }
    out
}

/// One end of a pattern: a variable, a node of the data, or a literal.
#[derive(Clone, Copy)]
enum End {
    Var(&'static str),
    Node(&'static str),
    Lit(&'static str),
}

impl End {
    fn term(self) -> String {
        match self {
            End::Var(v) => format!("?{v}"),
            End::Node(n) => format!("ex:{n}"),
            End::Lit(l) => format!("\"{l}\"^^geo:wktLiteral"),
        }
    }

    /// Bind `?w{name}` to the end's literals.
    fn literals(self, name: &str, graph: bool) -> String {
        let ser = "(geo:asWKT|geo:asGeoJSON|geo:hasSerialization)";
        let pattern = |t: String| {
            let p = format!(
                "{{ {t} {ser} ?w{name} }} UNION {{ {t} geo:hasDefaultGeometry/{ser} ?w{name} }}"
            );
            if graph {
                format!("GRAPH ?g {{ {p} }}")
            } else {
                p
            }
        };
        match self {
            End::Lit(_) => format!("VALUES ?w{name} {{ {} }}", self.term()),
            e => pattern(e.term()),
        }
    }
}

/// The rewrite's answer and the answer of the equivalent query (the asserted triples
/// through a predicate variable, the derived ones through `geof:` in a FILTER).
/// Returns the number of solutions.
fn compare(s: &Store, rel: Relation, a: End, b: End, graph: bool, o: &QueryOptions) -> usize {
    let mut vars: Vec<String> = [a, b]
        .iter()
        .filter_map(|e| match e {
            End::Var(v) => Some(format!("?{v}")),
            _ => None,
        })
        .collect();
    vars.dedup();
    if graph {
        vars.push("?g".into());
    }
    // two constants: whether the triple holds
    let (select, proj) = if vars.is_empty() {
        ("ASK".to_string(), String::new())
    } else {
        (
            format!("SELECT DISTINCT {}", vars.join(" ")),
            vars.join(" "),
        )
    };
    let triple = format!("{} geo:{} {}", a.term(), rel.local(), b.term());
    let rewritten = match (graph, proj.is_empty()) {
        (true, _) => format!("SELECT {proj} {{ GRAPH ?g {{ {triple} }} }}"),
        (false, true) => format!("ASK {{ {triple} }}"),
        (false, false) => format!("SELECT {proj} {{ {triple} }}"),
    };
    let asserted = format!(
        "{} ?p {} FILTER(?p = geo:{})",
        a.term(),
        b.term(),
        rel.local()
    );
    let asserted = if graph {
        format!("GRAPH ?g {{ {asserted} }}")
    } else {
        asserted
    };
    let derived = format!(
        "{} {} FILTER(geof:{}(?wa, ?wb))",
        a.literals("a", graph),
        b.literals("b", graph),
        rel.local()
    );
    let reference = format!("{select} {{ {{ {asserted} }} UNION {{ {derived} }} }}");
    let got = rows(s, &rewritten, o);
    let want = rows(s, &reference, o);
    assert_eq!(got, want, "{rewritten}\nvs\n{reference}");
    got.iter().filter(|r| r.as_str() != "false").count()
}

fn equivalent_answers(s: &Store, o: &QueryOptions, all: bool) {
    let mut found = [0usize; 2];
    let ends = [
        (End::Var("x"), End::Var("y")),
        (End::Var("x"), End::Var("x")),
        (End::Var("x"), End::Node("geom3")),
        (End::Node("f5"), End::Var("y")),
        (End::Node("geom7"), End::Var("y")),
        (
            End::Var("x"),
            End::Lit("POLYGON((2 2, 12 2, 12 12, 2 12, 2 2))"),
        ),
        (End::Lit("POINT(4 4)"), End::Var("y")),
        (
            End::Lit("<http://example.org/crs/mars> POINT(1 1)"),
            End::Var("y"),
        ),
        (End::Node("f1"), End::Node("geom9")),
        (End::Lit("POINT(1 1)"), End::Node("geom2")),
    ];
    for rel in Relation::ALL {
        for (a, b) in ends {
            // pairs of variables for a few relations (the others' cases are the same code)
            let pair = matches!((a, b), (End::Var(_), End::Var(_)));
            if pair
                && !all
                && !matches!(
                    rel,
                    Relation::SfIntersects | Relation::SfWithin | Relation::SfDisjoint
                )
            {
                continue;
            }
            for graph in [false, true] {
                found[usize::from(graph)] += compare(s, rel, a, b, graph, o);
            }
        }
    }
    // the data has matches in the default and the named graphs
    assert!(found[0] > 200 && found[1] > 5, "{found:?}");
}

fn random_store(seed: u64) -> Store {
    let s = Store::in_memory(StoreOptions::default());
    load(&s, &random_data(seed));
    s
}

#[test]
fn the_same_answers_as_the_functions() {
    for seed in [7, 1234] {
        let s = random_store(seed);
        // the index ready
        s.enable_geo(GeoConfig {
            query_rewrite: true,
            ..GeoConfig::default()
        })
        .unwrap();
        equivalent_answers(&s, &opts(), seed == 7);
        // a merged default graph
        let merged = QueryOptions {
            default_graph_extra: vec!["http://example.org/G0".into()],
            ..opts()
        };
        equivalent_answers(&s, &merged, false);
        // pushdown off: the reference runs plain filters (the rewrite is unchanged)
        let plain = QueryOptions {
            optimizations: Some(Optimizations {
                spatial_pushdown: false,
                ..Optimizations::ALL
            }),
            ..opts()
        };
        equivalent_answers(&s, &plain, false);
    }
}

#[test]
fn an_update_sees_its_own_geometries() {
    let s = store(true);
    sparkles::sparql::update::update(
        &s,
        &format!(
            "{P}INSERT {{ ?x ex:inside ex:A }} WHERE {{ ?x geo:sfWithin ex:A FILTER(?x != ex:A) }} ;
             INSERT DATA {{ ex:p9 geo:hasDefaultGeometry ex:g9 .
                            ex:g9 geo:asWKT \"POINT(1 1)\"^^geo:wktLiteral }} ;
             INSERT {{ ?x ex:inside2 ex:A }} WHERE {{ ?x geo:sfWithin ex:A }}"
        ),
        &QueryOptions::default(),
    )
    .unwrap();
    assert_eq!(select(&s, "SELECT ?x { ?x ex:inside ex:A }"), ["g1", "gA"]);
    assert_eq!(
        select(&s, "SELECT ?x { ?x ex:inside2 ex:A }"),
        ["A", "g1", "g9", "gA", "p9"]
    );
}
