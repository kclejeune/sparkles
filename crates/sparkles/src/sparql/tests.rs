use super::*;
use crate::io::{RdfFormat, Source};
use crate::store::{Store, StoreOptions};

const DATA: &str = r#"
@prefix ex: <http://ex.org/> .
@prefix foaf: <http://xmlns.com/foaf/0.1/> .
@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
@prefix xsd: <http://www.w3.org/2001/XMLSchema#> .

ex:alice a foaf:Person ; foaf:name "Alice" ; foaf:age 30 ; foaf:knows ex:bob, ex:carol .
ex:bob   a foaf:Person ; foaf:name "Bob" ; foaf:age 25 ; foaf:knows ex:carol .
ex:carol a foaf:Person ; foaf:name "Carol"@en ; foaf:age 35 .
ex:dave  a foaf:Agent ; foaf:name "Dave" .
foaf:Person rdfs:subClassOf foaf:Agent .
foaf:Agent rdfs:subClassOf ex:Thing .
ex:alice ex:score "1.5"^^xsd:decimal .
ex:bob ex:score "2.5"^^xsd:decimal .
"#;

const TRIG: &str = r#"
@prefix ex: <http://ex.org/> .
ex:g1 { ex:a ex:p 1 . ex:b ex:p 2 . }
ex:g2 { ex:a ex:p 3 . ex:c ex:p 4 . }
ex:x ex:p 9 .
"#;

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

fn q(s: &Store, text: &str) -> QueryResult {
    let prefixes = vec![
        ("ex".to_string(), "http://ex.org/".to_string()),
        ("foaf".to_string(), "http://xmlns.com/foaf/0.1/".to_string()),
        (
            "rdfs".to_string(),
            "http://www.w3.org/2000/01/rdf-schema#".to_string(),
        ),
        (
            "xsd".to_string(),
            "http://www.w3.org/2001/XMLSchema#".to_string(),
        ),
    ];
    let opts = QueryOptions {
        prefixes,
        ..Default::default()
    };
    query(s.snapshot(), text, &opts).unwrap_or_else(|e| panic!("{text}: {e}"))
}

/// Solutions rendered as sorted strings for easy comparison.
fn strs(r: &QueryResult) -> Vec<String> {
    let mut v: Vec<String> = r
        .rows()
        .into_iter()
        .map(|row| {
            row.into_iter()
                .map(|t| {
                    t.map_or("UNDEF".to_string(), |t| match t {
                        Term::NamedNode(n) => {
                            n.as_str().rsplit(['/', '#']).next().unwrap().to_string()
                        }
                        Term::Literal(l) => l.value().to_string(),
                        t => t.to_string(),
                    })
                })
                .collect::<Vec<_>>()
                .join(" ")
        })
        .collect();
    v.sort();
    v
}

fn has_op(p: &PlanInfo, op: &str) -> bool {
    p.operator == op || p.children.iter().any(|c| has_op(c, op))
}

#[test]
fn select_star_order() {
    let s = store();
    let r = q(
        &s,
        "SELECT * WHERE { ?person foaf:age ?age ; foaf:name ?name } LIMIT 1",
    );
    assert_eq!(r.vars, ["person", "age", "name"]);
}

#[test]
fn rejects_rebinding_in_scope_variable() {
    let s = store();
    let r = query(
        s.snapshot(),
        "SELECT (COUNT(*) AS ?c) WHERE { ?a ?b ?c }",
        &QueryOptions::default(),
    );
    assert!(r.is_err());
    let r = query(
        s.snapshot(),
        "SELECT * WHERE { ?a ?b ?c BIND(1 AS ?c) }",
        &QueryOptions::default(),
    );
    assert!(r.is_err());
}

#[test]
fn result_cache_reuses_subtrees() {
    let s = Store::in_memory(StoreOptions {
        result_cache_min_ms: 0.0,
        ..Default::default()
    });
    s.load(&[Source::from_bytes(
        DATA.as_bytes().to_vec(),
        RdfFormat::Turtle,
        None,
    )])
    .unwrap();
    let text = "SELECT ?t (COUNT(?p) AS ?c) WHERE { ?p a ?t . ?p foaf:name ?n } GROUP BY ?t";
    let a = q(&s, text);
    let b = q(&s, text);
    assert_eq!(strs(&a), strs(&b));
    assert!(has_cached(&b.plan), "second run should hit the cache");
    assert!(s.result_cache().hits() > 0);
    // an update invalidates (new snapshot version)
    update::update(&s, "INSERT DATA { <http://ex.org/z> a <http://xmlns.com/foaf/0.1/Person> ; <http://xmlns.com/foaf/0.1/name> \"Z\" }", &QueryOptions::default()).unwrap();
    let c = q(&s, text);
    assert!(!has_cached(&c.plan));
    assert_ne!(strs(&a), strs(&c));
}

fn has_cached(p: &PlanInfo) -> bool {
    p.cached || p.children.iter().any(has_cached)
}

#[test]
fn bgp_join() {
    let s = store();
    let r = q(
        &s,
        "SELECT ?n WHERE { ?p a foaf:Person ; foaf:name ?n ; foaf:knows ex:carol }",
    );
    assert_eq!(strs(&r), ["Alice", "Bob"]);
}

#[test]
fn optional_filter_order() {
    let s = store();
    let r = q(
        &s,
        "SELECT ?n ?k WHERE { ?p foaf:name ?n OPTIONAL { ?p foaf:knows ?k } FILTER(?n != \"Dave\") } ORDER BY ?n ?k",
    );
    assert_eq!(r.table.len(), 4);
    let rows = strs(&r);
    assert!(rows.contains(&"Carol UNDEF".to_string()), "{rows:?}");
    // ORDER BY: Alice rows first
    let first = r.term(r.table.cols[0][0]).unwrap();
    assert_eq!(first.to_string(), "\"Alice\"");
}

#[test]
fn optional_with_expr() {
    let s = store();
    let r = q(
        &s,
        "SELECT ?p ?a WHERE { ?p a foaf:Person OPTIONAL { ?p foaf:age ?a FILTER(?a > 26) } }",
    );
    assert_eq!(strs(&r), ["alice 30", "bob UNDEF", "carol 35"]);
}

#[test]
fn union_minus_values() {
    let s = store();
    let r = q(
        &s,
        "SELECT ?x WHERE { { ?x a foaf:Person } UNION { ?x a foaf:Agent } MINUS { ?x foaf:age 25 } }",
    );
    assert_eq!(strs(&r), ["alice", "carol", "dave"]);
    let r = q(
        &s,
        "SELECT ?x ?n WHERE { VALUES ?x { ex:alice ex:dave ex:nobody } ?x foaf:name ?n }",
    );
    assert_eq!(strs(&r), ["alice Alice", "dave Dave"]);
}

#[test]
fn aggregates() {
    let s = store();
    let r = q(
        &s,
        "SELECT (COUNT(*) AS ?c) (SUM(?a) AS ?s) (AVG(?a) AS ?avg) (MIN(?a) AS ?mn) (MAX(?a) AS ?mx) WHERE { ?p foaf:age ?a }",
    );
    assert_eq!(strs(&r), ["3 90 30 25 35"]);
    let r = q(
        &s,
        "SELECT ?t (COUNT(?p) AS ?c) WHERE { ?p a ?t } GROUP BY ?t HAVING (COUNT(?p) > 1)",
    );
    assert_eq!(strs(&r), ["Person 3"]);
    assert!(has_op(&r.plan, "GroupCountFromIndex"));
    let r = q(
        &s,
        "SELECT ?t (COUNT(*) AS ?c) WHERE { ?p a ?t } GROUP BY ?t",
    );
    assert_eq!(strs(&r), ["Agent 1", "Person 3"]);
    let r = q(
        &s,
        "SELECT (GROUP_CONCAT(?n; SEPARATOR=\",\") AS ?g) WHERE { SELECT ?n WHERE { ?p foaf:age ?a ; foaf:name ?n } ORDER BY ?a }",
    );
    assert_eq!(strs(&r), ["Bob,Alice,Carol"]);
    let r = q(&s, "SELECT (SUM(?x) AS ?s) WHERE { ?p ex:score ?x }");
    assert_eq!(strs(&r), ["4"]);
    // COUNT over a full scan comes from index metadata
    let r = q(&s, "SELECT (COUNT(*) AS ?c) WHERE { ?s ?p ?o }");
    assert_eq!(strs(&r), ["18"]);
    assert!(has_op(&r.plan, "CountFromIndex"));
    let r = q(
        &s,
        "SELECT (COUNT(*) AS ?c) WHERE { ?s ?p ?o FILTER(false) }",
    );
    assert_eq!(strs(&r), ["0"]);
}

#[test]
fn bind_functions() {
    let s = store();
    let r = q(
        &s,
        r#"SELECT ?u ?l ?len ?c ?sub ?h WHERE {
            ex:carol foaf:name ?n
            BIND(UCASE(?n) AS ?u) BIND(LANG(?n) AS ?l) BIND(STRLEN(?n) AS ?len)
            BIND(CONCAT(?n, "!") AS ?c) BIND(SUBSTR(?n, 2, 3) AS ?sub) BIND(MD5("abc") AS ?h)
        }"#,
    );
    assert_eq!(
        strs(&r),
        ["CAROL en 5 Carol! aro 900150983cd24fb0d6963f7d28e17f72"]
    );
    let r = q(
        &s,
        r#"SELECT ?x WHERE { BIND(xsd:integer("42") + 1 AS ?x) }"#,
    );
    assert_eq!(strs(&r), ["43"]);
    let r = q(
        &s,
        r#"SELECT ?x WHERE { BIND(REPLACE("abcabc", "b(c)", "[$1]") AS ?x) }"#,
    );
    assert_eq!(strs(&r), ["a[c]a[c]"]);
    let r = q(
        &s,
        r#"SELECT ?x WHERE { BIND(IF(1/0 > 1, "a", "b") AS ?x) }"#,
    );
    assert_eq!(strs(&r), ["UNDEF"]);
    let r = q(
        &s,
        r#"SELECT ?y WHERE { BIND(YEAR("2020-03-04T10:00:00Z"^^xsd:dateTime) AS ?y) }"#,
    );
    assert_eq!(strs(&r), ["2020"]);
}

#[test]
fn filters_and_regex() {
    let s = store();
    let r = q(
        &s,
        "SELECT ?n WHERE { ?p foaf:name ?n FILTER(REGEX(?n, \"^a|^b\", \"i\")) }",
    );
    assert_eq!(strs(&r), ["Alice", "Bob"]);
    let r = q(
        &s,
        "SELECT ?p WHERE { ?p foaf:age ?a FILTER(?a >= 30 && ?a < 35) }",
    );
    assert_eq!(strs(&r), ["alice"]);
    let r = q(
        &s,
        "SELECT ?p WHERE { ?p foaf:name ?n FILTER(isLiteral(?n) && LANGMATCHES(LANG(?n), \"EN\")) }",
    );
    assert_eq!(strs(&r), ["carol"]);
    // filter equality substitution
    let r = q(
        &s,
        "SELECT ?n WHERE { ?p foaf:name ?n FILTER(?p = ex:bob) }",
    );
    assert_eq!(strs(&r), ["Bob"]);
    let r = q(
        &s,
        "SELECT ?p WHERE { ?p foaf:age ?a FILTER(?a IN (25, 35)) }",
    );
    assert_eq!(strs(&r), ["bob", "carol"]);
}

#[test]
fn exists() {
    let s = store();
    let r = q(
        &s,
        "SELECT ?p WHERE { ?p a foaf:Person FILTER NOT EXISTS { ?p foaf:knows ?x } }",
    );
    assert_eq!(strs(&r), ["carol"]);
    let r = q(
        &s,
        "SELECT ?p WHERE { ?p a foaf:Person FILTER EXISTS { ?x foaf:knows ?p } }",
    );
    assert_eq!(strs(&r), ["bob", "carol"]);
}

#[test]
fn property_paths() {
    let s = store();
    let r = q(&s, "SELECT ?c WHERE { foaf:Person rdfs:subClassOf* ?c }");
    assert_eq!(strs(&r), ["Agent", "Person", "Thing"]);
    let r = q(&s, "SELECT ?c WHERE { foaf:Person rdfs:subClassOf+ ?c }");
    assert_eq!(strs(&r), ["Agent", "Thing"]);
    let r = q(&s, "SELECT ?x WHERE { ?x a/rdfs:subClassOf* ex:Thing }");
    assert_eq!(strs(&r), ["alice", "bob", "carol", "dave"]);
    let r = q(&s, "SELECT ?x WHERE { ex:alice foaf:knows/foaf:knows ?x }");
    assert_eq!(strs(&r), ["carol"]);
    let r = q(&s, "SELECT ?x WHERE { ex:carol ^foaf:knows ?x }");
    assert_eq!(strs(&r), ["alice", "bob"]);
    let r = q(&s, "SELECT ?x WHERE { ex:alice (foaf:knows|foaf:name) ?x }");
    assert_eq!(strs(&r), ["Alice", "bob", "carol"]);
    let r = q(&s, "SELECT ?x ?y WHERE { ?x foaf:knows+ ?y }");
    assert_eq!(r.table.len(), 3);
    // bound from the left: join with a transitive path
    let r = q(
        &s,
        "SELECT ?p ?c WHERE { ?p foaf:age 25 . ?p a ?t . ?t rdfs:subClassOf* ?c }",
    );
    assert_eq!(strs(&r), ["bob Agent", "bob Person", "bob Thing"]);
    let r = q(&s, "ASK { ex:alice !foaf:name ex:bob }");
    assert!(r.boolean);
}

#[test]
fn subquery_limit_distinct() {
    let s = store();
    let r = q(&s, "SELECT DISTINCT ?t WHERE { ?x a ?t }");
    assert_eq!(strs(&r), ["Agent", "Person"]);
    let r = q(
        &s,
        "SELECT ?n WHERE { ?p foaf:age ?a ; foaf:name ?n } ORDER BY DESC(?a) LIMIT 2",
    );
    assert_eq!(r.table.len(), 2);
    assert_eq!(
        r.term(r.table.cols[0][0]).unwrap().to_string(),
        "\"Carol\"@en"
    );
    let r = q(
        &s,
        "SELECT ?n WHERE { ?p foaf:age ?a ; foaf:name ?n } ORDER BY ?a OFFSET 1 LIMIT 1",
    );
    assert_eq!(strs(&r), ["Alice"]);
    let r = q(
        &s,
        "SELECT ?p ?max WHERE { { SELECT (MAX(?a) AS ?max) WHERE { ?x foaf:age ?a } } ?p foaf:age ?max }",
    );
    assert_eq!(strs(&r), ["carol 35"]);
}

#[test]
fn ask_construct_describe() {
    let s = store();
    assert!(q(&s, "ASK { ex:alice foaf:knows ex:bob }").boolean);
    assert!(!q(&s, "ASK { ex:bob foaf:knows ex:alice }").boolean);
    let r = q(
        &s,
        "CONSTRUCT { ?b ex:knownBy ?a } WHERE { ?a foaf:knows ?b }",
    );
    assert_eq!(r.triples.len(), 3);
    let r = q(&s, "DESCRIBE ex:dave");
    assert_eq!(r.triples.len(), 2);
}

#[test]
fn named_graphs() {
    let s = Store::in_memory(StoreOptions::default());
    s.load(&[Source::from_bytes(
        TRIG.as_bytes().to_vec(),
        RdfFormat::TriG,
        None,
    )])
    .unwrap();
    let r = q(&s, "SELECT ?o WHERE { ?s ex:p ?o }");
    assert_eq!(strs(&r), ["9"]);
    let r = q(&s, "SELECT ?g ?o WHERE { GRAPH ?g { ex:a ex:p ?o } }");
    assert_eq!(strs(&r), ["g1 1", "g2 3"]);
    let r = q(&s, "SELECT ?o WHERE { GRAPH ex:g2 { ?s ex:p ?o } }");
    assert_eq!(strs(&r), ["3", "4"]);
    let r = q(&s, "SELECT ?o FROM ex:g1 FROM ex:g2 WHERE { ?s ex:p ?o }");
    assert_eq!(strs(&r), ["1", "2", "3", "4"]);
    let r = q(&s, "SELECT ?s FROM ex:g1 FROM ex:g2 WHERE { ?s ex:p ?o }");
    assert_eq!(strs(&r), ["a", "a", "b", "c"]);
    let r = q(
        &s,
        "SELECT ?s (COUNT(*) AS ?c) FROM ex:g1 FROM ex:g2 WHERE { ?s ex:p ?o } GROUP BY ?s",
    );
    assert_eq!(strs(&r), ["a 2", "b 1", "c 1"]);
    assert!(has_op(&r.plan, "GroupCountFromIndex"));
    let r = q(
        &s,
        "SELECT ?g (COUNT(*) AS ?c) WHERE { GRAPH ?g { ?s ?p ?o } } GROUP BY ?g",
    );
    assert_eq!(strs(&r), ["g1 2", "g2 2"]);
    let r = q(&s, "SELECT DISTINCT ?g WHERE { GRAPH ?g { ?s ?p ?o } }");
    assert_eq!(strs(&r), ["g1", "g2"]);
    let r = q(
        &s,
        "SELECT ?o WHERE { GRAPH <urn:x-arq:DefaultGraph> { ?s ex:p ?o } }",
    );
    assert_eq!(strs(&r), ["9"]);
    let r = q(
        &s,
        "SELECT ?o WHERE { GRAPH <urn:x-arq:UnionGraph> { ex:a ex:p ?o } }",
    );
    assert_eq!(strs(&r), ["1", "3"]);
    let opts = QueryOptions {
        default_graph_extra: vec!["http://ex.org/g1".into()],
        ..Default::default()
    };
    let r = query(
        s.snapshot(),
        "SELECT ?o WHERE { ?s <http://ex.org/p> ?o }",
        &opts,
    )
    .unwrap();
    assert_eq!(strs(&r), ["1", "2", "9"]);
    let opts = QueryOptions {
        default_graph_uris: vec!["urn:x-arq:DefaultGraph".into()],
        ..Default::default()
    };
    let r = query(s.snapshot(), "SELECT ?o WHERE { { ?s <http://ex.org/p> ?o } UNION { GRAPH ?g { ?s <http://ex.org/p> ?o } } }", &opts).unwrap();
    assert_eq!(strs(&r), ["9"]);
    let r = q(&s, "SELECT ?g ?x WHERE { GRAPH ?g { ex:a ex:p* ?x } }");
    assert_eq!(r.table.len(), 4);
}

#[test]
fn updates() {
    let s = store();
    let opts = QueryOptions::default();
    let st = update::update(
        &s,
        "PREFIX ex: <http://ex.org/> PREFIX foaf: <http://xmlns.com/foaf/0.1/>
         INSERT DATA { ex:erin a foaf:Person ; foaf:name \"Erin\" ; foaf:age 41 . GRAPH ex:g { ex:erin ex:tag \"x\" } } ;
         DELETE { ?p foaf:age ?a } INSERT { ?p foaf:age ?b } WHERE { ?p foaf:age ?a BIND(?a + 1 AS ?b) } ;
         DELETE WHERE { ex:dave ?p ?o }",
        &opts,
    )
    .unwrap();
    assert!(st.inserted >= 8, "{st:?}");
    let r = q(&s, "SELECT ?p ?a WHERE { ?p foaf:age ?a }");
    assert_eq!(strs(&r), ["alice 31", "bob 26", "carol 36", "erin 42"]);
    assert!(!q(&s, "ASK { ex:dave ?p ?o }").boolean);
    assert!(q(&s, "ASK { GRAPH ex:g { ex:erin ex:tag \"x\" } }").boolean);
    update::update(&s, "CLEAR GRAPH <http://ex.org/g>", &opts).unwrap();
    assert!(!q(&s, "ASK { GRAPH ?g { ?s ?p ?o } }").boolean);
    update::update(&s, "INSERT DATA { _:b <http://ex.org/p> _:b }", &opts).unwrap();
    let r = q(&s, "SELECT ?x WHERE { ?x ex:p ?x }");
    assert_eq!(r.table.len(), 1);
    update::update(&s, "DROP ALL", &opts).unwrap();
    assert!(s.snapshot().is_empty());
}

#[test]
fn result_formats() {
    let s = store();
    let r = q(&s, "SELECT ?n WHERE { ex:carol foaf:name ?n }");
    let mut buf = Vec::new();
    results::write_solutions(&r, results::SolutionsFormat::Json, &mut buf, None).unwrap();
    let j: serde_json::Value = serde_json::from_slice(&buf).unwrap();
    assert_eq!(j["results"]["bindings"][0]["n"]["xml:lang"], "en");
    let mut buf = Vec::new();
    results::write_solutions(&r, results::SolutionsFormat::Sparkles, &mut buf, None).unwrap();
    let j: serde_json::Value = serde_json::from_slice(&buf).unwrap();
    assert_eq!(j["meta"]["totalRows"], 1);
    assert!(j["meta"]["plan"]["operator"].is_string());
    let (sse, plan) = explain(
        s.snapshot(),
        "SELECT * WHERE { ?s ?p ?o }",
        &QueryOptions::default(),
    )
    .unwrap();
    assert!(sse.contains("bgp"));
    assert_eq!(plan.actual_rows, -1);
}

#[test]
fn persistent_query_after_update_and_compact() {
    let dir = tempfile::tempdir().unwrap();
    let s = Store::open(dir.path(), StoreOptions::default()).unwrap();
    s.load(&[Source::from_bytes(
        DATA.as_bytes().to_vec(),
        RdfFormat::Turtle,
        None,
    )])
    .unwrap();
    update::update(
        &s,
        "INSERT DATA { <http://ex.org/zed> <http://xmlns.com/foaf/0.1/age> 99 }",
        &QueryOptions::default(),
    )
    .unwrap();
    let r = q(
        &s,
        "SELECT ?p WHERE { ?p foaf:age ?a FILTER(?a > 30) } ORDER BY ?a",
    );
    assert_eq!(strs(&r), ["carol", "zed"]);
    s.compact().unwrap();
    let r = q(
        &s,
        "SELECT ?p WHERE { ?p foaf:age ?a FILTER(?a > 30) } ORDER BY ?a",
    );
    assert_eq!(strs(&r), ["carol", "zed"]);
}

/// Enough rows for the per-distinct-value filter paths (≥ 4096): labels of every term
/// kind that repeat, `ex:knows` edges with repeated objects, and a named graph.
fn mixed_store() -> Store {
    let mut trig = String::from("@prefix ex: <http://ex.org/> .\n");
    for i in 0..6000 {
        let o = match i % 6 {
            0 => format!("\"label {}\"", i % 7),
            1 => format!("\"label {}\"@en", i % 7),
            2 => format!("\"Label {}\"@de-CH", i % 11),
            3 => format!("<http://ex.org/label/{}>", i % 13),
            4 => format!("{}", i % 17),
            _ => format!("\"label {}\"^^ex:dt", i % 5),
        };
        trig.push_str(&format!(
            "ex:s{i} ex:label {o} ; ex:knows ex:s{} .\n",
            (i * 7) % 900
        ));
    }
    trig.push_str("ex:s4 ex:knows ex:s4 . ex:g { ex:s1 ex:label \"label 3 in g\" . ex:x ex:knows ex:only-in-g , ex:s1 . }\n");
    let s = Store::in_memory(StoreOptions::default());
    s.load(&[Source::from_bytes(trig.into_bytes(), RdfFormat::TriG, None)])
        .unwrap();
    s
}

fn count(s: &Store, text: &str) -> String {
    strs(&q(s, text)).join(",")
}
#[test]
fn count_distinct_from_index() {
    let s = mixed_store();
    let cases = [
        ("?s ex:knows ?o", "?o"),
        ("?s ex:knows ?o", "?s"),
        ("?s ex:label ?o", "?o"),
        ("?s ?p ?o", "?p"),
        ("?s ex:knows ?s", "?s"),
        ("GRAPH ?g { ?s ex:knows ?o }", "?o"),
        ("GRAPH ex:g { ?s ex:knows ?o }", "?o"),
        ("GRAPH <urn:x-arq:UnionGraph> { ?s ex:knows ?o }", "?o"),
    ];
    let check = |s: &Store| {
        for (pattern, v) in cases {
            let r = q(
                s,
                &format!("SELECT (COUNT(DISTINCT {v}) AS ?c) WHERE {{ {pattern} }}"),
            );
            // a graph variable keeps the general plan (the scan is not the group's input)
            assert_eq!(
                has_op(&r.plan, "CountDistinctFromIndex"),
                !pattern.contains("?g"),
                "{pattern} {v}"
            );
            let expected = count(
                s,
                &format!(
                    "SELECT (COUNT(*) AS ?c) WHERE {{ SELECT DISTINCT {v} WHERE {{ {pattern} }} }}"
                ),
            );
            assert_eq!(strs(&r).join(","), expected, "{pattern} {v}");
        }
    };
    check(&s);
    assert_eq!(
        count(
            &s,
            "SELECT (COUNT(DISTINCT ?o) AS ?c) WHERE { ?s ex:knows ?o }"
        ),
        "900"
    );
    update::update(
        &s,
        "PREFIX ex: <http://ex.org/>
         DELETE WHERE { ?s ex:knows ex:s0 } ;
         INSERT DATA { ex:new ex:knows ex:s0, ex:fresh1, ex:fresh2 . GRAPH ex:g { ex:y ex:knows ex:fresh3 } }",
        &QueryOptions::default(),
    )
    .unwrap();
    check(&s);
    assert_eq!(
        count(
            &s,
            "SELECT (COUNT(DISTINCT ?o) AS ?c) WHERE { ?s ex:knows ?o }"
        ),
        "902"
    );
}
