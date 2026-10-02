//! Graph views (`QueryOptions::graphs`): a query through a view answers exactly what the
//! same query answers on a store that holds only the view's graphs, with every fast path,
//! the result cache, a delta over the base and a union default graph; writes outside the
//! view's write graphs fail before anything changes, whether or not the quads exist.

use sparkles::Error;
use sparkles::access::{GraphAccess, GraphRule, Graphs};
use sparkles::guard::WriteOptions;
use sparkles::io::{RdfFormat, Source};
use sparkles::sparql::update::update;
use sparkles::sparql::{QueryKind, QueryOptions, query};
use sparkles::store::{ReplaceTarget, Store, StoreOptions};
use std::sync::Arc;

const PREFIXES: &str = "@prefix ex: <http://ex/> .
@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
@prefix spk: <urn:x-sparkles:> .
@prefix geo: <http://www.opengis.net/ont/geosparql#> .
";

const INFERRED: &str = "urn:x-sparkles:inferred";

/// The base data, by graph (`""` is the default graph, `_:bg` a blank-node graph).
const BASE: &[(&str, &str)] = &[
    (
        "",
        r#"ex:s0 a ex:C1 ; ex:p ex:s1 ; rdfs:label "fox in the default" ; ex:n 1 .
           ex:s1 ex:p ex:s2 ."#,
    ),
    (
        "http://ex/a/1",
        r#"ex:s1 a ex:C1 ; ex:p ex:s3 ; rdfs:label "brown fox one" ; ex:n 2 ;
             ex:emb "[1, 0, 0]"^^spk:vector .
           ex:s3 ex:p ex:s4 .
           ex:pt1 geo:asWKT "POINT(1 1)"^^geo:wktLiteral ."#,
    ),
    (
        "http://ex/a/2",
        r#"ex:s2 a ex:C2 ; ex:p ex:s0 ; ex:q "x" ; rdfs:label "quick fox two" ;
             ex:emb "[0.9, 0.1, 0]"^^spk:vector .
           ex:s0 a ex:C1 ."#,
    ),
    (
        "http://ex/b/1",
        r#"ex:s4 a ex:C1 , ex:C2 ; ex:p ex:s5 ; ex:secret "hidden" ; ex:n 3 ;
             rdfs:label "hidden fox bee" ; ex:emb "[1, 0, 0]"^^spk:vector .
           ex:s2 ex:p ex:s4 .
           ex:s1 ex:p ex:s3 .
           ex:s0 a ex:C2 .
           ex:pt2 geo:asWKT "POINT(2 2)"^^geo:wktLiteral ."#,
    ),
    (INFERRED, r#"ex:s4 a ex:C3 . ex:s0 ex:q "inferred" ."#),
    (
        "_:bg",
        r#"ex:s5 ex:p ex:s0 ; rdfs:label "fox in a blank graph" ."#,
    ),
];

/// Changes after the base was compacted, by graph: inserts, then deletes.
const DELTA_INSERT: &[(&str, &str)] = &[
    (
        "http://ex/b/1",
        "ex:s6 a ex:C1 ; ex:p ex:s0 . ex:s0 ex:p ex:s6 .",
    ),
    (
        "http://ex/a/1",
        "ex:s6 a ex:C2 . ex:s5 a ex:C1 . ex:s6 ex:n 4 .",
    ),
    ("", r#"ex:s6 ex:q "d" ."#),
    ("http://ex/b/2", "ex:s0 a ex:C1 ."),
];
const DELTA_DELETE: &[(&str, &str)] = &[
    ("http://ex/a/2", r#"ex:s2 ex:q "x" ."#),
    ("http://ex/b/1", "ex:s4 a ex:C2 ."),
];

/// TriG of the graphs `keep` accepts.
fn trig(parts: &[(&str, &str)], keep: &dyn Fn(&str) -> bool) -> String {
    let mut s = PREFIXES.to_string();
    for (g, body) in parts.iter().filter(|(g, _)| keep(g)) {
        match *g {
            "" => s.push_str(body),
            g if g.starts_with("_:") => s.push_str(&format!("{g} {{ {body} }}")),
            g => s.push_str(&format!("<{g}> {{ {body} }}")),
        }
        s.push('\n');
    }
    s
}

/// A SPARQL data block of the graphs `keep` accepts.
fn data_block(parts: &[(&str, &str)], keep: &dyn Fn(&str) -> bool) -> String {
    parts
        .iter()
        .filter(|(g, _)| keep(g))
        .map(|(g, body)| match *g {
            "" => body.to_string(),
            g => format!("GRAPH <{g}> {{ {body} }}"),
        })
        .collect::<Vec<_>>()
        .join(" ")
}

const SPARQL_PREFIXES: &str = "PREFIX ex: <http://ex/> \
    PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#> \
    PREFIX spk: <urn:x-sparkles:> \
    PREFIX text: <http://jena.apache.org/text#> \
    PREFIX geo: <http://www.opengis.net/ont/geosparql#> \
    PREFIX geof: <http://www.opengis.net/def/function/geosparql/> ";

/// A store of the graphs `keep` accepts: the base compacted, then the delta.
fn store(keep: &dyn Fn(&str) -> bool, union: bool) -> Store {
    let s = Store::in_memory(StoreOptions {
        union_default_graph: union,
        result_cache_min_ms: 0.0,
        ..Default::default()
    });
    s.load(&[Source::from_bytes(
        trig(BASE, keep).into_bytes(),
        RdfFormat::TriG,
        None,
    )])
    .unwrap();
    s.compact().unwrap();
    #[cfg(feature = "text")]
    s.enable_text(sparkles::text::TextConfig::default())
        .unwrap();
    #[cfg(feature = "geo")]
    s.enable_geo(sparkles::geo::GeoConfig::default()).unwrap();
    let ins = data_block(DELTA_INSERT, keep);
    if !ins.is_empty() {
        update(
            &s,
            &format!("{SPARQL_PREFIXES} INSERT DATA {{ {ins} }}"),
            &QueryOptions::default(),
        )
        .unwrap();
    }
    let del = data_block(DELTA_DELETE, keep);
    if !del.is_empty() {
        update(
            &s,
            &format!("{SPARQL_PREFIXES} DELETE DATA {{ {del} }}"),
            &QueryOptions::default(),
        )
        .unwrap();
    }
    s
}

/// A view that reads `names` (and writes them too).
fn view(names: &[&str]) -> Arc<GraphAccess> {
    let r = Graphs::Only(GraphRule::new(names, &[INFERRED]));
    Arc::new(GraphAccess {
        read: r.clone(),
        write: r,
    })
}

/// Sorted rows (or triples, or the boolean) as strings.
fn answer(s: &Store, q: &str, opts: &QueryOptions) -> Vec<String> {
    let r = query(s.snapshot(), &format!("{SPARQL_PREFIXES}{q}"), opts)
        .unwrap_or_else(|e| panic!("{q}: {e}"));
    let mut out: Vec<String> = match r.kind {
        QueryKind::Select => r
            .rows()
            .into_iter()
            .map(|row| {
                row.iter()
                    .map(|t| t.as_ref().map_or("-".to_string(), |t| t.to_string()))
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .collect(),
        QueryKind::Ask => vec![r.boolean.to_string()],
        _ => r.triples.iter().map(|t| t.to_string()).collect(),
    };
    out.sort();
    out
}

const QUERIES: &[&str] = &[
    "SELECT * { ?s ?p ?o }",
    "SELECT * { GRAPH ?g { ?s ?p ?o } }",
    "SELECT ?g { GRAPH ?g { } }",
    "SELECT (COUNT(*) AS ?n) { ?s ?p ?o }",
    "SELECT (COUNT(*) AS ?n) { GRAPH ?g { ?s ?p ?o } }",
    "SELECT (COUNT(*) AS ?n) { GRAPH ?g { ?s ex:p ?o } }",
    "SELECT (COUNT(DISTINCT ?s) AS ?n) { GRAPH ?g { ?s ?p ?o } }",
    "SELECT (COUNT(DISTINCT ?s) AS ?n) { ?s ?p ?o }",
    "SELECT (COUNT(DISTINCT ?o) AS ?n) { GRAPH ?g { ?s ex:p ?o } }",
    "SELECT (COUNT(*) AS ?n) { ?s a ?c }",
    "SELECT ?c (COUNT(?s) AS ?n) { GRAPH ?g { ?s a ?c } } GROUP BY ?c",
    "SELECT ?c (COUNT(?s) AS ?n) { ?s a ?c } GROUP BY ?c",
    "SELECT ?c (COUNT(DISTINCT ?s) AS ?n) { GRAPH ?g { ?s a ?c } } GROUP BY ?c",
    "SELECT ?p (COUNT(*) AS ?n) { GRAPH ?g { ?s ?p ?o } } GROUP BY ?p",
    "SELECT ?p (COUNT(*) AS ?n) { ?s ?p ?o } GROUP BY ?p",
    "SELECT ?g (COUNT(*) AS ?n) { GRAPH ?g { ?s ?p ?o } } GROUP BY ?g",
    "SELECT ?s (SUM(?n) AS ?t) { GRAPH ?g { ?s ex:n ?n } } GROUP BY ?s",
    "SELECT (COUNT(*) AS ?n) { GRAPH ?g { ?s ex:p ?o . ?o ex:p ?x } }",
    "SELECT (COUNT(*) AS ?n) { ?s ex:p ?o . ?o ex:p ?x }",
    "SELECT ?s ?o { ?s ex:p+ ?o }",
    "SELECT ?g ?s ?o { GRAPH ?g { ?s ex:p+ ?o } }",
    "SELECT ?s ?o { GRAPH ?g { ?s ex:p/ex:p ?o } }",
    "SELECT ?s ?o FROM <http://ex/a/1> FROM <http://ex/b/1> { ?s ex:p* ?o }",
    "SELECT ?s ?o FROM <http://ex/b/1> { ?s ex:p ?o }",
    "SELECT ?g ?s FROM NAMED <http://ex/b/1> FROM NAMED <http://ex/a/2> { GRAPH ?g { ?s a ?c } }",
    "ASK { GRAPH <http://ex/b/1> { } }",
    "ASK { GRAPH <http://ex/b/9> { } }",
    "SELECT * { GRAPH <http://ex/b/1> { ?s ?p ?o } }",
    "SELECT * { GRAPH <urn:x-sparkles:inferred> { ?s ?p ?o } }",
    "SELECT * { GRAPH <urn:x-arq:UnionGraph> { ?s ex:p ?o } }",
    "SELECT (COUNT(*) AS ?n) { GRAPH <urn:x-arq:UnionGraph> { ?s ?p ?o } }",
    "DESCRIBE ex:s4",
    "DESCRIBE ?s WHERE { GRAPH ?g { ?s a ex:C1 } }",
    "CONSTRUCT { ?s ?p ?o } WHERE { GRAPH ?g { ?s ?p ?o } }",
    "SELECT ?s { GRAPH ?g { ?s a ex:C1 } FILTER EXISTS { GRAPH ?h { ?s ex:p ?x } } }",
    "SELECT ?s { GRAPH ?g { ?s a ?c } MINUS { GRAPH ?h { ?s a ex:C2 } } }",
    "SELECT ?s ?x { GRAPH ?g { ?s a ?c } OPTIONAL { GRAPH ?h { ?s ex:secret ?x } } }",
    "SELECT ?s { { SELECT ?s (COUNT(*) AS ?n) { GRAPH ?g { ?s ?p ?o } } GROUP BY ?s } FILTER(?n > 1) }",
    "SELECT ?s ?o { GRAPH ?g { ?s ex:p ?o } } ORDER BY ?o ?s LIMIT 3",
    "SELECT DISTINCT ?s { GRAPH ?g { ?s ex:p ?o } FILTER(STRSTARTS(STR(?o), \"http://ex/s\")) }",
    "SELECT (COUNT(*) AS ?n) { GRAPH ?g { ?s ex:p ?o } FILTER(STRSTARTS(STR(?o), \"http://ex/s\")) }",
    "SELECT ?s ?score { GRAPH ?g { (?s ?score) spk:vectorSearch (ex:emb \"[1,0,0]\"^^spk:vector 10 \"exact:true\") } }",
    "SELECT ?s { (?s ?score) spk:vectorSearch (ex:emb \"[1,0,0]\"^^spk:vector 10 \"exact:true\") }",
    #[cfg(feature = "text")]
    "SELECT ?s ?lit { GRAPH ?g { (?s ?sc ?lit) text:query \"fox\" } }",
    #[cfg(feature = "text")]
    "SELECT ?s ?lit { (?s ?sc ?lit) text:query \"fox\" }",
    // text and vector rankings fused: the subjects found, not their fused scores
    #[cfg(feature = "text")]
    "SELECT DISTINCT ?s { GRAPH ?g { (?s ?score) spk:hybridSearch ((rdfs:label \"fox\") (ex:emb \"[1,0,0]\"^^spk:vector)) } }",
    #[cfg(feature = "text")]
    "SELECT DISTINCT ?s { (?s ?score) spk:hybridSearch ((rdfs:label \"fox\") (ex:emb \"[1,0,0]\"^^spk:vector)) }",
    #[cfg(feature = "geo")]
    "SELECT ?f { GRAPH ?g { ?f geo:asWKT ?w } \
       FILTER(geof:sfWithin(?w, \"POLYGON((0 0, 10 0, 10 10, 0 10, 0 0))\"^^geo:wktLiteral)) }",
    #[cfg(feature = "geo")]
    "SELECT ?f { ?f geo:asWKT ?w \
       FILTER(geof:sfWithin(?w, \"POLYGON((0 0, 10 0, 10 10, 0 10, 0 0))\"^^geo:wktLiteral)) }",
];

/// Which graphs a store of exactly a view's graphs keeps.
type Keep = Box<dyn Fn(&str) -> bool>;

/// The views tested: the names each reads, and the graphs a store of them keeps.
fn views() -> Vec<(Vec<&'static str>, Keep)> {
    vec![
        (
            vec!["default", "http://ex/a/*"],
            Box::new(|g: &str| g.is_empty() || g.starts_with("http://ex/a/")),
        ),
        (
            vec!["http://ex/a/*"],
            Box::new(|g: &str| g.starts_with("http://ex/a/")),
        ),
        (
            vec!["urn:x-arq:DefaultGraph", "*"],
            Box::new(|g: &str| g != INFERRED && !g.starts_with("_:")),
        ),
        (
            vec!["http://ex/b/1", INFERRED],
            Box::new(|g: &str| g == "http://ex/b/1" || g == INFERRED),
        ),
        (vec!["http://ex/none"], Box::new(|_: &str| false)),
    ]
}

fn differential(union: bool) {
    let full = store(&|_| true, union);
    for (names, keep) in views() {
        let expected_store = store(&*keep, union);
        let restricted = QueryOptions {
            graphs: Some(view(&names)),
            ..Default::default()
        };
        for q in QUERIES {
            // DESCRIBE in a store with a union default graph also reads the stored
            // default graph, which a view's union of named graphs leaves out
            if union && q.starts_with("DESCRIBE") {
                continue;
            }
            // fill the result cache without the view first: the view must not read it
            let _ = answer(&full, q, &QueryOptions::default());
            let got = answer(&full, q, &restricted);
            let want = answer(&expected_store, q, &QueryOptions::default());
            assert_eq!(got, want, "view {names:?}, union {union}: {q}");
            // and again, from the cache
            assert_eq!(
                answer(&full, q, &restricted),
                want,
                "cached, {names:?}: {q}"
            );
        }
    }
}

#[test]
fn a_view_answers_like_a_store_of_its_graphs() {
    differential(false);
}

#[test]
fn a_view_answers_like_a_store_of_its_graphs_with_a_union_default_graph() {
    differential(true);
}

#[test]
fn protocol_datasets_and_the_inference_overlay_are_limited_to_the_view() {
    let full = store(&|_| true, false);
    let keep = |g: &str| g.is_empty() || g.starts_with("http://ex/a/");
    let expected = store(&keep, false);
    let v = view(&["default", "http://ex/a/*"]);
    let cases = [
        QueryOptions {
            default_graph_uris: vec!["http://ex/b/1".into(), "http://ex/a/1".into()],
            named_graph_uris: vec!["http://ex/b/1".into(), "http://ex/a/2".into()],
            ..Default::default()
        },
        QueryOptions {
            default_graph_uris: vec!["urn:x-arq:UnionGraph".into()],
            ..Default::default()
        },
        // the inferred graph is not covered by a pattern
        QueryOptions {
            default_graph_extra: vec![INFERRED.into()],
            ..Default::default()
        },
    ];
    for opts in cases {
        for q in [
            "SELECT * { ?s ?p ?o }",
            "SELECT * { GRAPH ?g { ?s ?p ?o } }",
            "SELECT (COUNT(*) AS ?n) { ?s ?p ?o }",
        ] {
            let restricted = QueryOptions {
                graphs: Some(v.clone()),
                ..opts.clone()
            };
            assert_eq!(
                answer(&full, q, &restricted),
                answer(&expected, q, &opts),
                "{q} with {:?} {:?} {:?}",
                opts.default_graph_uris,
                opts.named_graph_uris,
                opts.default_graph_extra
            );
        }
    }
    // named exactly, the inferred graph joins the overlay
    let with_inferred = view(&["default", INFERRED]);
    let r = answer(
        &full,
        "SELECT ?o { ex:s0 ex:q ?o }",
        &QueryOptions {
            graphs: Some(with_inferred),
            default_graph_extra: vec![INFERRED.into()],
            ..Default::default()
        },
    );
    assert_eq!(r, ["\"inferred\""]);
}

/// A view by patterns that happen to cover every graph of the store keeps the plans of
/// the full dataset (its named graphs are not listed), and the same answers.
#[test]
fn a_view_of_every_existing_graph_answers_like_no_view() {
    for union in [false, true] {
        let s = store(&|g| !g.starts_with("_:"), union);
        let every = QueryOptions {
            graphs: Some(view(&["default", "*", INFERRED])),
            ..Default::default()
        };
        for q in QUERIES {
            assert_eq!(
                answer(&s, q, &every),
                answer(&s, q, &QueryOptions::default()),
                "union {union}: {q}"
            );
        }
    }
}

/// Hybrid search through a view finds the subjects of the visible graphs only.
#[cfg(feature = "text")]
#[test]
fn hybrid_search_through_a_view() {
    let full = store(&|_| true, false);
    let q = "SELECT DISTINCT ?s { GRAPH ?g { (?s ?score) spk:hybridSearch \
             ((rdfs:label \"fox\") (ex:emb \"[1,0,0]\"^^spk:vector)) } }";
    let all = answer(&full, q, &QueryOptions::default());
    assert!(all.contains(&"<http://ex/s4>".to_string()), "{all:?}");
    let a = answer(
        &full,
        q,
        &QueryOptions {
            graphs: Some(view(&["http://ex/a/*"])),
            ..Default::default()
        },
    );
    assert_eq!(a, ["<http://ex/s1>", "<http://ex/s2>"]);
}

#[test]
fn a_full_view_changes_nothing() {
    let full = store(&|_| true, false);
    let all = QueryOptions {
        graphs: Some(Arc::new(GraphAccess::all())),
        ..Default::default()
    };
    for q in QUERIES {
        assert_eq!(
            answer(&full, q, &all),
            answer(&full, q, &QueryOptions::default()),
            "{q}"
        );
    }
    // the same plans as without a view: the same operators, estimates and fast paths
    fn shape(p: &sparkles::sparql::PlanInfo, out: &mut Vec<String>) {
        out.push(format!(
            "{} {} {}",
            p.operator, p.estimated_rows, p.estimated_cost
        ));
        for c in &p.children {
            shape(c, out);
        }
    }
    for q in QUERIES {
        let q = format!("{SPARQL_PREFIXES}{q}");
        let (_, a) = sparkles::sparql::explain(full.snapshot(), &q, &all).unwrap();
        let (_, b) =
            sparkles::sparql::explain(full.snapshot(), &q, &QueryOptions::default()).unwrap();
        let (mut x, mut y) = (Vec::new(), Vec::new());
        shape(&a, &mut x);
        shape(&b, &mut y);
        assert_eq!(x, y, "{q}");
    }
}

#[test]
fn plans_of_a_view_carry_no_statistics_of_hidden_graphs() {
    let full = store(&|_| true, false);
    let opts = QueryOptions {
        graphs: Some(view(&["http://ex/a/*"])),
        ..Default::default()
    };
    for q in [
        "SELECT ?c (COUNT(?s) AS ?n) { GRAPH ?g { ?s a ?c } } GROUP BY ?c",
        "SELECT (COUNT(*) AS ?n) { GRAPH ?g { ?s ?p ?o } }",
        "SELECT * { GRAPH ?g { ?s ex:p ?o } }",
    ] {
        let q = format!("{SPARQL_PREFIXES}{q}");
        let (_, plan) = sparkles::sparql::explain(full.snapshot(), &q, &opts).unwrap();
        let text = serde_json::to_string(&plan).unwrap();
        assert!(!text.contains("graphs not read"), "{text}");
        assert_eq!(plan.estimated_rows, -1.0);
        let r = query(full.snapshot(), &q, &opts).unwrap();
        let text = serde_json::to_string(&r.plan).unwrap();
        assert!(!text.contains("graphs not read"), "{text}");
        assert_eq!(r.plan.estimated_rows, -1.0);
    }
}

/// A view that reads the default graph and `http://ex/a/*`, and writes `http://ex/a/1`.
fn writer() -> QueryOptions {
    QueryOptions {
        graphs: Some(Arc::new(GraphAccess {
            read: Graphs::Only(GraphRule::new(["default", "http://ex/a/*"], &[INFERRED])),
            write: Graphs::Only(GraphRule::new(["http://ex/a/1"], &[INFERRED])),
        })),
        ..Default::default()
    }
}

fn refused(s: &Store, u: &str, opts: &QueryOptions) -> String {
    let head = s.head_commit().seq;
    match update(s, &format!("{SPARQL_PREFIXES}{u}"), opts) {
        Err(Error::NotPermitted(m)) => {
            assert_eq!(s.head_commit().seq, head, "{u} changed the store");
            m
        }
        other => panic!(
            "{u}: expected a refusal, got {:?}",
            other.map(|s| s.inserted)
        ),
    }
}

#[test]
fn writes_outside_the_write_graphs_fail_whether_or_not_the_quads_exist() {
    let s = store(&|_| true, false);
    let w = writer();
    // allowed
    update(
        &s,
        &format!(
            "{SPARQL_PREFIXES} INSERT DATA {{ GRAPH <http://ex/a/1> {{ ex:n1 ex:p ex:n2 }} }}"
        ),
        &w,
    )
    .unwrap();
    // readable but not writable, hidden, the default graph
    refused(
        &s,
        "INSERT DATA { GRAPH <http://ex/a/2> { ex:n1 ex:p ex:n2 } }",
        &w,
    );
    refused(
        &s,
        "INSERT DATA { GRAPH <http://ex/b/1> { ex:n1 ex:p ex:n2 } }",
        &w,
    );
    refused(&s, "INSERT DATA { ex:n1 ex:p ex:n2 }", &w);
    // a quad that exists and one that does not: the same refusal
    let a = refused(
        &s,
        "DELETE DATA { GRAPH <http://ex/b/1> { ex:s2 ex:p ex:s4 } }",
        &w,
    );
    let b = refused(
        &s,
        "DELETE DATA { GRAPH <http://ex/b/1> { ex:zz ex:p ex:s4 } }",
        &w,
    );
    assert_eq!(a, b);
    let a = refused(
        &s,
        "DELETE DATA { GRAPH <http://ex/b/9> { ex:zz ex:p ex:s4 } }",
        &w,
    );
    assert_eq!(a, "write access to graph <http://ex/b/9> required");
    // graphs bound by the WHERE clause, and template constants
    refused(
        &s,
        "INSERT { GRAPH ?g { ex:n1 ex:p ex:n2 } } WHERE { BIND(<http://ex/b/1> AS ?g) }",
        &w,
    );
    let a = refused(
        &s,
        "DELETE { GRAPH ?g { ex:s2 ex:p ex:s4 } } WHERE { BIND(<http://ex/b/1> AS ?g) }",
        &w,
    );
    let b = refused(
        &s,
        "DELETE { GRAPH ?g { ex:zz ex:zz ex:zz } } WHERE { BIND(<http://ex/b/1> AS ?g) }",
        &w,
    );
    assert_eq!(a, b);
    refused(
        &s,
        "DELETE { GRAPH <http://ex/b/1> { ?s ?p ?o } } WHERE { ?s ?p ?o }",
        &w,
    );
    refused(&s, "DELETE WHERE { GRAPH ?g { ?s ?p ?o } }", &w);
    refused(
        &s,
        "WITH <http://ex/b/1> DELETE { ?s ?p ?o } WHERE { ?s ?p ?o }",
        &w,
    );
    // graph management names its target before it is looked up
    let a = refused(&s, "CLEAR GRAPH <http://ex/b/1>", &w);
    let b = refused(&s, "CLEAR GRAPH <http://ex/b/9>", &w);
    assert_ne!(a, b);
    assert!(b.contains("http://ex/b/9"));
    refused(&s, "DROP SILENT GRAPH <http://ex/b/1>", &w);
    refused(&s, "CREATE GRAPH <http://ex/b/9>", &w);
    refused(&s, "CLEAR DEFAULT", &w);
    refused(&s, "COPY <http://ex/a/1> TO <http://ex/b/1>", &w);
    refused(
        &s,
        "LOAD <file:///nonexistent.ttl> INTO GRAPH <http://ex/b/1>",
        &w,
    );
    // the view holds http://ex/a/2, which this writer may only read
    refused(&s, "CLEAR ALL", &w);
    refused(&s, "CLEAR NAMED", &w);
    // WITH a writable graph reads and deletes only there
    let before = answer(
        &s,
        "SELECT * { GRAPH ?g { ?s ?p ?o } }",
        &QueryOptions::default(),
    );
    update(
        &s,
        &format!(
            "{SPARQL_PREFIXES} WITH <http://ex/a/1> DELETE {{ ?s ex:p ?o }} WHERE {{ ?s ex:p ?o }}"
        ),
        &w,
    )
    .unwrap();
    let after = answer(
        &s,
        "SELECT * { GRAPH ?g { ?s ?p ?o } }",
        &QueryOptions::default(),
    );
    let gone: Vec<&String> = before.iter().filter(|r| !after.contains(r)).collect();
    assert!(!gone.is_empty());
    assert!(
        gone.iter().all(|r| r.starts_with("<http://ex/a/1>")),
        "{gone:?}"
    );
}

#[test]
fn clear_all_acts_on_the_view() {
    let s = store(&|_| true, false);
    let w = QueryOptions {
        graphs: Some(view(&["default", "http://ex/a/*"])),
        ..Default::default()
    };
    update(&s, "CLEAR ALL", &w).unwrap();
    let left = answer(
        &s,
        "SELECT DISTINCT ?g { GRAPH ?g { ?s ?p ?o } }",
        &QueryOptions::default(),
    );
    assert_eq!(left.len(), 4, "{left:?}");
    assert!(left.iter().all(|g| !g.contains("http://ex/a/")), "{left:?}");
    assert_eq!(
        answer(&s, "SELECT * { ?s ?p ?o }", &QueryOptions::default()),
        Vec::<String>::new()
    );
}

#[test]
fn loads_and_replaces_check_every_quad() {
    let s = store(&|_| true, false);
    let w = WriteOptions {
        graphs: writer().graphs,
        ..Default::default()
    };
    let head = s.head_commit().seq;
    let nq = |g: &str| {
        Source::from_bytes(
            format!("<http://ex/n1> <http://ex/p> <http://ex/n2> <http://ex/a/1> .\n<http://ex/n1> <http://ex/p> <http://ex/n3> <{g}> .\n").into_bytes(),
            RdfFormat::NQuads,
            None,
        )
    };
    let r = s.load_with(
        &[nq("http://ex/b/1")],
        sparkles::commit::CommitKind::Load,
        &w,
    );
    assert!(matches!(r, Err(Error::NotPermitted(_))), "{:?}", r.err());
    assert_eq!(s.head_commit().seq, head);
    s.load_with(
        &[nq("http://ex/a/1")],
        sparkles::commit::CommitKind::Load,
        &w,
    )
    .unwrap();
    let ttl = || {
        Source::from_bytes(
            b"<http://ex/n1> <http://ex/p> <http://ex/n2> .".to_vec(),
            RdfFormat::Turtle,
            Some(oxrdf::NamedNode::new("http://ex/a/1").unwrap()),
        )
    };
    let named = |g: &str| ReplaceTarget::Named(oxrdf::NamedNode::new(g).unwrap());
    let kind = sparkles::commit::CommitKind::GspPut;
    let head = s.head_commit().seq;
    for t in [
        named("http://ex/b/1"),
        named("http://ex/b/9"),
        named("http://ex/a/2"),
        ReplaceTarget::Default,
        ReplaceTarget::All,
    ] {
        let r = s.replace_with(t, &[ttl()], kind, &w);
        assert!(matches!(r, Err(Error::NotPermitted(_))), "{:?}", r.err());
    }
    // an empty body for a hidden graph: refused as well
    let empty = Source::from_bytes(Vec::new(), RdfFormat::Turtle, None);
    let r = s.replace_with(named("http://ex/b/1"), &[empty], kind, &w);
    assert!(matches!(r, Err(Error::NotPermitted(_))));
    assert_eq!(s.head_commit().seq, head);
    s.replace_with(named("http://ex/a/1"), &[ttl()], kind, &w)
        .unwrap();
    assert_eq!(
        answer(
            &s,
            "SELECT ?s ?o { GRAPH <http://ex/a/1> { ?s ?p ?o } }",
            &QueryOptions::default()
        ),
        ["<http://ex/n1> <http://ex/n2>"]
    );
}

#[test]
fn diffs_show_only_the_view() {
    let dir = tempfile::tempdir().unwrap();
    let s = Store::open(dir.path(), StoreOptions::default()).unwrap();
    s.load(&[Source::from_bytes(
        trig(BASE, &|_| true).into_bytes(),
        RdfFormat::TriG,
        None,
    )])
    .unwrap();
    let from = s.head_commit().seq;
    let ins = data_block(DELTA_INSERT, &|_| true);
    update(
        &s,
        &format!("{SPARQL_PREFIXES} INSERT DATA {{ {ins} }}"),
        &QueryOptions::default(),
    )
    .unwrap();
    let del = data_block(DELTA_DELETE, &|_| true);
    update(
        &s,
        &format!("{SPARQL_PREFIXES} DELETE DATA {{ {del} }}"),
        &QueryOptions::default(),
    )
    .unwrap();
    let to = s.head_commit().seq;
    let diff = |graphs| {
        let o = sparkles::store::DiffOptions {
            graphs,
            ..Default::default()
        };
        s.diff(
            &sparkles::history::At::Commit(from),
            &sparkles::history::At::Commit(to),
            &o,
        )
        .unwrap()
    };
    let all = diff(None);
    let d = diff(Some(view(&["http://ex/a/*"])));
    let graphs: Vec<String> = d.iter().map(|(_, q)| q.graph_name.to_string()).collect();
    assert!(!graphs.is_empty());
    assert!(graphs.len() < all.len());
    assert!(
        graphs.iter().all(|g| g.starts_with("<http://ex/a/")),
        "{graphs:?}"
    );
    assert_eq!((d.added + d.removed) as usize, graphs.len());
    // the change feed: each commit lists the changes of the view's graphs only
    let feed = |graphs| {
        s.changes(
            from,
            &sparkles::store::ChangesOptions {
                graphs,
                ..Default::default()
            },
        )
        .unwrap()
    };
    let full = feed(None);
    let page = feed(Some(view(&["http://ex/a/*"])));
    assert_eq!(page.commits.len(), full.commits.len());
    let seen: Vec<String> = page
        .commits
        .iter()
        .flat_map(|c| c.iter().map(|(_, q)| q.graph_name.to_string()))
        .collect();
    let all: usize = full.commits.iter().map(|c| c.iter().count()).sum();
    assert!(!seen.is_empty() && seen.len() < all, "{seen:?}");
    assert!(
        seen.iter().all(|g| g.starts_with("<http://ex/a/")),
        "{seen:?}"
    );
    for c in &page.commits {
        assert_eq!((c.added + c.removed) as usize, c.iter().count());
    }
}

#[test]
fn schema_reports_cover_the_view() {
    use sparkles::schema::{GraphSelection, SchemaError, SchemaOptions, discover};
    let full = store(&|_| true, false);
    let keep = |g: &str| g.is_empty() || g.starts_with("http://ex/a/");
    let expected = store(&keep, false);
    let v = view(&["default", "http://ex/a/*"]);
    for sel in ["union", "default", "http://ex/a/1"] {
        let sel = GraphSelection::parse(sel).unwrap();
        let got = discover(
            &full.snapshot(),
            &SchemaOptions {
                graph: sel.clone(),
                graphs: Some(v.clone()),
                ..Default::default()
            },
        )
        .unwrap();
        let want = discover(
            &expected.snapshot(),
            &SchemaOptions {
                graph: sel.clone(),
                ..Default::default()
            },
        )
        .unwrap();
        let j = |r: &sparkles::schema::SchemaReport| {
            serde_json::json!({
                "triples": r.totals.triples,
                "classes": r.classes,
                "predicates": r.predicates,
            })
        };
        assert_eq!(j(&got), j(&want), "{}", sel.name());
    }
    // a hidden graph is reported like a missing one
    for g in ["http://ex/b/1", "http://ex/b/9"] {
        let r = discover(
            &full.snapshot(),
            &SchemaOptions {
                graph: GraphSelection::parse(g).unwrap(),
                graphs: Some(v.clone()),
                ..Default::default()
            },
        );
        assert!(matches!(r, Err(SchemaError::NoSuchGraph(_))), "{g}");
    }
}
