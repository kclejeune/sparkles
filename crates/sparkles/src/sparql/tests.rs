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

#[test]
fn result_cache_hits_for_aggregates() {
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
    // spargebra names aggregate variables randomly on every parse
    let text =
        "SELECT ?t (COUNT(?p) AS ?c) (MAX(?n) AS ?m) WHERE { ?p a ?t ; foaf:name ?n } GROUP BY ?t";
    let a = q(&s, text);
    let b = q(&s, text);
    assert_eq!(strs(&a), strs(&b));
    fn op_cached(p: &PlanInfo, op: &str) -> bool {
        (p.operator == op && p.cached) || p.children.iter().any(|c| op_cached(c, op))
    }
    assert!(
        op_cached(&b.plan, "GroupBy")
            || op_cached(&b.plan, "Bind")
            || op_cached(&b.plan, "Project"),
        "the aggregation itself should come from the cache"
    );
}

#[test]
fn result_cache_keeps_hex_like_iris_apart() {
    let s = Store::in_memory(StoreOptions {
        result_cache_min_ms: 0.0,
        ..Default::default()
    });
    let a = "0123456789abcdef0123456789abcdef";
    let b = "fedcba9876543210fedcba9876543210";
    let data = format!(
        "<http://x/s> <http://x/p> <http://x/?{a}> .\n<http://x/t> <http://x/p> <http://x/?{b}> .\n"
    );
    s.load(&[Source::from_bytes(
        data.into_bytes(),
        RdfFormat::NTriples,
        None,
    )])
    .unwrap();
    for (iri, subject) in [(a, "s"), (b, "t"), (a, "s")] {
        let r = q(
            &s,
            &format!(
                "SELECT ?s (COUNT(*) AS ?n) WHERE {{ ?s <http://x/p> <http://x/?{iri}> }} GROUP BY ?s"
            ),
        );
        assert_eq!(strs(&r), [format!("{subject} 1")]);
    }
}

fn has_cached(p: &PlanInfo) -> bool {
    p.cached || p.children.iter().any(has_cached)
}

#[test]
fn initial_bindings() {
    let s = store();
    let opts = QueryOptions {
        initial_bindings: vec![(
            "p".into(),
            Term::NamedNode(oxrdf::NamedNode::new_unchecked("http://ex.org/bob")),
        )],
        ..Default::default()
    };
    let r = query(
        s.snapshot(),
        "PREFIX foaf: <http://xmlns.com/foaf/0.1/> SELECT ?p ?n WHERE { ?p foaf:name ?n }",
        &opts,
    )
    .unwrap();
    assert_eq!(strs(&r), ["bob Bob"]);
    let r = query(
        s.snapshot(),
        "ASK { ?p <http://xmlns.com/foaf/0.1/knows> <http://ex.org/alice> }",
        &opts,
    )
    .unwrap();
    assert!(!r.boolean);
}

#[test]
fn rdf12_triple_terms() {
    let data = r#"
        PREFIX : <http://ex.org/>
        PREFIX rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#>
        :alice :says <<( :bob :age 42 )>> .
        _:r rdf:reifies <<( :bob :knows _:x )>> ; :source :census .
        _:x :name "X" .
        :t :label "hello"@en--rtl .
    "#;
    for persistent in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let s = if persistent {
            Store::open(dir.path(), StoreOptions::default()).unwrap()
        } else {
            Store::in_memory(StoreOptions::default())
        };
        s.load(&[Source::from_bytes(
            data.as_bytes().to_vec(),
            RdfFormat::Turtle,
            None,
        )])
        .unwrap();
        // triple pattern with variables inside a triple term
        let r = q(
            &s,
            "SELECT ?s ?o WHERE { ex:alice ex:says <<( ?s ex:age ?o )>> }",
        );
        assert_eq!(strs(&r), ["bob 42"]);
        // constant triple term
        assert!(q(&s, "ASK { ex:alice ex:says <<( ex:bob ex:age 42 )>> }").boolean);
        // reification syntax, blank node inside the triple term joined with data
        let r = q(
            &s,
            "SELECT ?src ?n WHERE { << ex:bob ex:knows ?x >> ex:source ?src . ?x ex:name ?n }",
        );
        assert_eq!(strs(&r), ["census X"]);
        // functions
        let r = q(
            &s,
            "SELECT ?p (isTRIPLE(?t) AS ?is) WHERE { ex:alice ex:says ?t BIND(PREDICATE(?t) AS ?p) }",
        );
        assert_eq!(strs(&r), ["age true"]);
        let r = q(
            &s,
            "SELECT (LANGDIR(?l) AS ?d) (hasLANGDIR(?l) AS ?h) WHERE { ex:t ex:label ?l }",
        );
        assert_eq!(strs(&r), ["rtl true"]);
        let r = q(&s, "SELECT ?t WHERE { BIND(TRIPLE(ex:a, ex:b, 1) AS ?t) }");
        assert_eq!(r.table.len(), 1);
        // updates with triple terms
        update::update(
            &s,
            "PREFIX : <http://ex.org/> INSERT DATA { :carol :says <<( :bob :age 43 )>> }",
            &QueryOptions::default(),
        )
        .unwrap();
        let r = q(
            &s,
            "SELECT ?who WHERE { ?who ex:says <<( ex:bob ex:age ?a )>> FILTER(?a > 42) }",
        );
        assert_eq!(strs(&r), ["carol"]);
        update::update(
            &s,
            "PREFIX : <http://ex.org/> DELETE DATA { :carol :says <<( :bob :age 43 )>> }",
            &QueryOptions::default(),
        )
        .unwrap();
        assert!(!q(&s, "ASK { ?who ex:says <<( ex:bob ex:age 43 )>> }").boolean);
        let r = q(
            &s,
            "CONSTRUCT { ?s ex:claimed <<( ?s ex:said ?t )>> } WHERE { ?s ex:says ?t }",
        );
        assert_eq!(r.triples.len(), 1);
    }
}

fn has_desc(p: &PlanInfo, needle: &str) -> bool {
    p.description.contains(needle) || p.children.iter().any(|c| has_desc(c, needle))
}

#[test]
fn limit_and_ask_stop_early() {
    let s = Store::in_memory(StoreOptions::default());
    let mut nt = String::new();
    for i in 0..5000 {
        nt.push_str(&format!("<http://ex.org/s{i}> <http://ex.org/p> {i} .\n<http://ex.org/s{i}> <http://ex.org/q> <http://ex.org/o{}> .\n", i % 7));
    }
    s.load(&[Source::from_bytes(nt.into_bytes(), RdfFormat::Turtle, None)])
        .unwrap();
    let r = q(&s, "SELECT * WHERE { ?s ?p ?o } LIMIT 10");
    assert_eq!(r.table.len(), 10);
    assert!(has_desc(&r.plan, "stopped early"));
    let r = q(
        &s,
        "SELECT ?s WHERE { ?s ex:p ?v FILTER(?v > 4000) } LIMIT 5 OFFSET 2",
    );
    assert_eq!(r.table.len(), 5);
    let r = q(&s, "SELECT ?s ?o WHERE { ?s ex:p ?v . ?s ex:q ?o } LIMIT 3");
    assert_eq!(r.table.len(), 3);
    assert!(q(&s, "ASK { ?s ex:q ex:o3 }").boolean);
    assert!(!q(&s, "ASK { ?s ex:q ex:o9 }").boolean);
    // a limit larger than the result returns everything
    let r = q(
        &s,
        "SELECT ?s WHERE { ?s ex:p ?v FILTER(?v < 20) } LIMIT 100",
    );
    assert_eq!(r.table.len(), 20);
    let r = q(&s, "SELECT DISTINCT ?o WHERE { ?s ex:q ?o } LIMIT 100");
    assert_eq!(r.table.len(), 7);
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
    // exact per-class counts come from the statistics (no delta, default graph only)
    assert!(has_op(&r.plan, "GroupCountFromMetadata"));
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
        "SELECT (COUNT(*) AS ?n) WHERE { ?a foaf:knows ?b . ?b foaf:knows ?c }",
    );
    assert_eq!(strs(&r), ["1"]);
    // two scans joined on one variable: counted from per-key runs
    assert!(has_op(&r.plan, "CountJoinFromRuns"));
    let r = q(
        &s,
        "SELECT (COUNT(*) AS ?c) WHERE { ?a foaf:knows ?b . ?b foaf:name ?n }",
    );
    assert_eq!(strs(&r), ["3"]);
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
#[test]
fn filters_on_keys_match_row_evaluation() {
    let s = mixed_store();
    let filters = [
        r#"CONTAINS(?o, "label 3")"#,
        r#"CONTAINS(?o, "label"@en)"#,
        r#"STRSTARTS(STR(?o), "http://ex.org/label/1")"#,
        r#"STRENDS(STR(?o), "3")"#,
        r#"REGEX(?o, "^label [0-4]$", "i")"#,
        r#"REGEX(STR(?o), "label")"#,
        r#"LANGMATCHES(LANG(?o), "en")"#,
        r#"LANGMATCHES(LANG(?o), "de") && CONTAINS(?o, "1")"#,
        r#"LANGMATCHES(LANG(?o), "*")"#,
        r#"LANG(?o) = """#,
        r#"!CONTAINS(?o, "label")"#,
    ];
    let check = |s: &Store| {
        for f in filters {
            let fast = count(
                s,
                &format!("SELECT (COUNT(*) AS ?c) WHERE {{ ?s ex:label ?o FILTER({f}) }}"),
            );
            // RAND() keeps the filter on the row-by-row evaluator
            let rows = count(
                s,
                &format!(
                    "SELECT (COUNT(*) AS ?c) WHERE {{ ?s ex:label ?o FILTER(RAND() < 2 && ({f})) }}"
                ),
            );
            assert_eq!(fast, rows, "{f}");
            assert_ne!(fast, "0", "{f}");
        }
    };
    check(&s);
    // terms added by updates live in the delta vocabulary
    update::update(
        &s,
        r#"PREFIX ex: <http://ex.org/>
           INSERT DATA { ex:n1 ex:label "label 3, new"@en-US . ex:n2 ex:label "brand new label 3" .
                         ex:n3 ex:label <http://ex.org/label/1new> }"#,
        &QueryOptions::default(),
    )
    .unwrap();
    check(&s);
}

// ---------------------------------------------------------------------------------------
// regressions from the implementation review
// ---------------------------------------------------------------------------------------

fn cached_store(data: &str, format: RdfFormat) -> Store {
    let s = Store::in_memory(StoreOptions {
        result_cache_min_ms: 0.0,
        ..Default::default()
    });
    s.load(&[Source::from_bytes(data.as_bytes().to_vec(), format, None)])
        .unwrap();
    s
}

const PREFIXES: &str = "PREFIX ex: <http://ex.org/> PREFIX foaf: <http://xmlns.com/foaf/0.1/> ";

#[test]
fn update_operations_do_not_share_cached_where_results() {
    let s = cached_store(DATA, RdfFormat::Turtle);
    let pattern = "?s foaf:knows ?o . ?s a foaf:Person";
    // warm the committed cache with the join both WHERE clauses use
    q(&s, &format!("SELECT ?s WHERE {{ {pattern} }}"));
    update::update(
        &s,
        &format!(
            "{PREFIXES} INSERT {{ ?s ex:first 1 }} WHERE {{ {pattern} }} ;
             DELETE DATA {{ ex:bob foaf:knows ex:carol }} ;
             INSERT {{ ?s ex:second 1 }} WHERE {{ {pattern} }}"
        ),
        &QueryOptions::default(),
    )
    .unwrap();
    assert_eq!(
        strs(&q(&s, "SELECT ?s WHERE { ?s ex:first 1 }")),
        ["alice", "bob"]
    );
    // the second WHERE runs after the delete: bob no longer knows anyone
    assert_eq!(
        strs(&q(&s, "SELECT ?s WHERE { ?s ex:second 1 }")),
        ["alice"]
    );
    // and the committed state is what later queries see
    assert_eq!(
        strs(&q(&s, &format!("SELECT DISTINCT ?s WHERE {{ {pattern} }}"))),
        ["alice"]
    );
}

#[test]
fn cached_paths_respect_the_graph() {
    let s = cached_store(
        "@prefix ex: <http://ex.org/> .
         ex:g1 { ex:a ex:next ex:b . }
         ex:g2 { ex:a ex:next ex:c . }",
        RdfFormat::TriG,
    );
    for _ in 0..2 {
        for (g, end) in [("g1", "b"), ("g2", "c")] {
            let r = q(
                &s,
                &format!("SELECT ?x WHERE {{ GRAPH ex:{g} {{ ex:a ex:next+ ?x }} }}"),
            );
            assert_eq!(strs(&r), [end], "GRAPH ex:{g}");
        }
        let r = q(&s, "SELECT ?g ?x WHERE { GRAPH ?g { ex:a ex:next+ ?x } }");
        assert_eq!(strs(&r), ["g1 b", "g2 c"]);
    }
}

#[test]
fn replace_is_atomic_and_leaves_data_on_parse_errors() {
    use crate::store::ReplaceTarget;
    // the in-place path, then the bulk rebuild path of large replaces
    for bulk_threshold in [StoreOptions::default().bulk_threshold, 0] {
        let s = Store::in_memory(StoreOptions {
            bulk_threshold,
            ..Default::default()
        });
        s.load(&[Source::from_bytes(
            TRIG.as_bytes().to_vec(),
            RdfFormat::TriG,
            None,
        )])
        .unwrap();
        // a comment long enough for the size estimate to count as large
        let pad = format!("# {}\n", "x".repeat(200));
        let g1 = oxrdf::NamedNode::new_unchecked("http://ex.org/g1");
        let bad = Source::from_bytes(
            format!("{pad}<http://ex.org/n> <http://ex.org/p> 7 . this is not turtle").into_bytes(),
            RdfFormat::Turtle,
            Some(g1.clone()),
        );
        let head = s.head_commit().seq;
        assert!(s.replace(ReplaceTarget::Named(g1.clone()), &[bad]).is_err());
        assert_eq!(s.head_commit().seq, head, "{bulk_threshold}");
        assert_eq!(
            strs(&q(&s, "SELECT ?o WHERE { GRAPH ex:g1 { ?s ex:p ?o } }")),
            ["1", "2"],
            "{bulk_threshold}"
        );
        let good = Source::from_bytes(
            format!("{pad}<http://ex.org/n> <http://ex.org/p> 7 .").into_bytes(),
            RdfFormat::Turtle,
            Some(g1.clone()),
        );
        assert_eq!(
            s.replace(ReplaceTarget::Named(g1), &[good]).unwrap(),
            1,
            "{bulk_threshold}"
        );
        assert_eq!(
            strs(&q(&s, "SELECT ?o WHERE { GRAPH ex:g1 { ?s ex:p ?o } }")),
            ["7"]
        );
        // other graphs are untouched
        assert_eq!(
            strs(&q(&s, "SELECT ?o WHERE { GRAPH ex:g2 { ?s ex:p ?o } }")),
            ["3", "4"]
        );
        assert_eq!(strs(&q(&s, "SELECT ?o WHERE { ?s ex:p ?o }")), ["9"]);
        // the commit counts the replacement
        let c = s.head_commit();
        assert_eq!((c.inserted, c.deleted), (1, 2), "{bulk_threshold}");
        // replacing everything
        let all = Source::from_bytes(
            format!("{pad}<http://ex.org/z> <http://ex.org/p> 0 .").into_bytes(),
            RdfFormat::Turtle,
            None,
        );
        assert_eq!(s.replace(ReplaceTarget::All, &[all]).unwrap(), 1);
        assert_eq!(s.snapshot().len(), 1, "{bulk_threshold}");
    }
}

#[test]
fn join_expansion_respects_the_row_budget() {
    // 300 subjects share one object: the self-join on ?o is a 90,000-pair run
    let mut ttl = String::from("@prefix ex: <http://ex.org/> .\n");
    for i in 0..300 {
        ttl.push_str(&format!("ex:s{i} ex:p ex:x .\n"));
    }
    let s = cached_store(&ttl, RdfFormat::Turtle);
    let opts = QueryOptions {
        max_rows: Some(1000),
        ..Default::default()
    };
    let r = query(
        s.snapshot(),
        "SELECT * WHERE { ?a <http://ex.org/p> ?o . ?b <http://ex.org/p> ?o }",
        &opts,
    );
    assert!(
        matches!(
            r,
            Err(crate::error::Error::BudgetExceeded(crate::error::Budget {
                kind: crate::error::BudgetKind::Rows,
                ..
            }))
        ),
        "{:?}",
        r.err()
    );
}

#[test]
fn updates_store_only_valid_rdf() {
    let s = store();
    let opts = QueryOptions::default();
    for u in [
        // literal predicate, new to the store (delta vocabulary)
        r#"INSERT { <urn:z> ?p <urn:o> } WHERE { VALUES ?p { "invalid predicate" } }"#,
        // literal subject
        r#"INSERT { ?s <urn:p> <urn:o> } WHERE { BIND("not a subject" AS ?s) }"#,
        // triple term as subject
        r#"INSERT { ?t <urn:p> <urn:o> } WHERE { BIND(<<( <urn:a> <urn:b> <urn:c> )>> AS ?t) }"#,
        // literal graph name
        r#"INSERT { GRAPH ?g { <urn:a> <urn:p> 1 } } WHERE { BIND("g" AS ?g) }"#,
    ] {
        let st = update::update(&s, u, &opts).unwrap();
        assert_eq!(st.inserted, 0, "{u}");
    }
    let st = update::update(
        &s,
        "INSERT { <urn:z> ?p ?t } WHERE { VALUES ?p { <urn:p> } BIND(<<( <urn:a> <urn:b> <urn:c> )>> AS ?t) }",
        &opts,
    )
    .unwrap();
    assert_eq!(st.inserted, 1);
    // everything stored can be exported
    let mut buf = Vec::new();
    let n = s.dump_nquads(&mut buf).unwrap();
    assert_eq!(n, s.snapshot().len());
}

#[test]
fn filters_beyond_the_planner_mask_are_kept() {
    let s = store();
    for n in [63, 64, 65, 80] {
        let mut text = String::from("SELECT ?p WHERE { ?p foaf:name ?n . ?p foaf:age ?a ");
        for _ in 0..n - 1 {
            text.push_str("FILTER(?a > 0) ");
        }
        // the last, restrictive filter must survive planning
        text.push_str("FILTER(?a > 30) }");
        assert_eq!(strs(&q(&s, &text)), ["carol"], "{n} filters");
    }
}

#[test]
fn initial_bindings_restrict_values() {
    let s = store();
    let one = Term::Literal(oxrdf::Literal::from(1i64));
    let opts = QueryOptions {
        initial_bindings: vec![("x".into(), one)],
        ..Default::default()
    };
    let run = |text: &str| strs(&query(s.snapshot(), text, &opts).unwrap());
    assert_eq!(run("SELECT ?x WHERE { VALUES ?x { 1 2 } }"), ["1"]);
    assert_eq!(
        run("SELECT ?x WHERE { VALUES ?x { 2 3 } }"),
        Vec::<String>::new()
    );
    // UNDEF cells take the bound value; correlated columns keep their rows
    assert_eq!(
        run(
            "SELECT ?x ?y WHERE { VALUES (?x ?y) { (1 \"a\") (2 \"b\") (UNDEF \"c\") (1 \"a\") } }"
        ),
        ["1 a", "1 a", "1 c"]
    );
}

#[test]
fn named_only_protocol_dataset_has_an_empty_default_graph() {
    let s = Store::in_memory(StoreOptions::default());
    s.load(&[Source::from_bytes(
        TRIG.as_bytes().to_vec(),
        RdfFormat::TriG,
        None,
    )])
    .unwrap();
    let opts = QueryOptions {
        named_graph_uris: vec!["http://ex.org/g1".into()],
        ..Default::default()
    };
    let run = |text: &str| strs(&query(s.snapshot(), text, &opts).unwrap());
    assert_eq!(
        run("SELECT ?o WHERE { ?s <http://ex.org/p> ?o }"),
        Vec::<String>::new()
    );
    assert_eq!(
        run("SELECT ?g ?o WHERE { GRAPH ?g { ?s <http://ex.org/p> ?o } }"),
        ["g1 1", "g1 2"]
    );
    let opts = QueryOptions {
        default_graph_uris: vec!["http://ex.org/g2".into()],
        ..Default::default()
    };
    let r = query(
        s.snapshot(),
        "SELECT ?o WHERE { ?s <http://ex.org/p> ?o }",
        &opts,
    )
    .unwrap();
    assert_eq!(strs(&r), ["3", "4"]);
}

#[test]
fn cancelled_or_expired_updates_publish_nothing() {
    let s = store();
    let before = s.snapshot().len();
    let cancelled = QueryOptions {
        cancel: Some(std::sync::Arc::new(std::sync::atomic::AtomicBool::new(
            true,
        ))),
        ..Default::default()
    };
    let r = update::update(&s, "INSERT DATA { <urn:a> <urn:b> <urn:c> }", &cancelled);
    assert!(
        matches!(r, Err(crate::error::Error::Cancelled)),
        "{:?}",
        r.err()
    );
    let expired = QueryOptions {
        timeout: Some(std::time::Duration::ZERO),
        ..Default::default()
    };
    let r = update::update(
        &s,
        "INSERT { ?s <urn:seen> 1 } WHERE { ?s ?p ?o }",
        &expired,
    );
    assert!(
        matches!(r, Err(crate::error::Error::Timeout)),
        "{:?}",
        r.err()
    );
    assert_eq!(s.snapshot().len(), before);
}

#[test]
fn update_where_clauses_resolve_against_base() {
    let s = store();
    update::update(
        &s,
        r#"BASE <http://example.org/> INSERT { <urn:s> <urn:p> ?x } WHERE { BIND(IRI("relative") AS ?x) }"#,
        &QueryOptions::default(),
    )
    .unwrap();
    assert!(q(&s, "ASK { <urn:s> <urn:p> <http://example.org/relative> }").boolean);
}

#[test]
fn cached_results_obey_the_row_budget() {
    let s = cached_store(DATA, RdfFormat::Turtle);
    let text = "SELECT ?p ?n WHERE { ?p foaf:knows ?o . ?o foaf:name ?n }";
    let full = strs(&q(&s, text));
    assert_eq!(full.len(), 3);
    // the next run is served from the cache
    assert!(has_cached(&q(&s, text).plan));
    let limited = QueryOptions {
        max_rows: Some(2),
        prefixes: vec![("foaf".into(), "http://xmlns.com/foaf/0.1/".into())],
        ..Default::default()
    };
    let r = query(s.snapshot(), text, &limited);
    assert!(
        matches!(
            r,
            Err(crate::error::Error::BudgetExceeded(crate::error::Budget {
                kind: crate::error::BudgetKind::Rows,
                ..
            }))
        ),
        "{:?}",
        r.err()
    );
}

#[test]
fn memory_budget_fails_the_query_and_releases_its_charges() {
    use crate::error::{BudgetKind, Error};
    let s = store();
    let snap = s.snapshot();
    let o = QueryOptions {
        max_memory_bytes: Some(1024),
        ..Default::default()
    };
    match query(snap.clone(), "SELECT * { ?a ?b ?c . ?d ?e ?f }", &o) {
        Err(Error::BudgetExceeded(b)) => {
            assert_eq!((b.kind, b.limit), (BudgetKind::Memory, 1024));
            assert!(b.requested > 1024);
            let msg = Error::BudgetExceeded(b).to_string();
            assert!(
                msg.starts_with("query exceeds its memory budget: needs about "),
                "{msg}"
            );
            assert!(msg.ends_with("limit 1.0 KiB"), "{msg}");
        }
        r => panic!("{:?}", r.map(|r| r.len())),
    }
    // a small query under the same budget runs
    let r = query(snap.clone(), "SELECT * { ?s ?p ?o } LIMIT 1", &o).unwrap();
    assert_eq!(r.len(), 1);
    // charges are per context: nothing leaked into later queries
    let r = query(snap, "SELECT * { ?s ?p ?o }", &QueryOptions::default()).unwrap();
    let n = r.len() as u64;
    assert!(
        r.mem_peak_bytes >= n * 3 * 8,
        "{} for {n} rows",
        r.mem_peak_bytes
    );
}

#[test]
fn memory_budget_applies_to_cached_results_and_updates() {
    use crate::error::{BudgetKind, Error};
    let s = cached_store(DATA, RdfFormat::Turtle);
    let text = "SELECT * WHERE { ?a ?b ?c . ?d ?e ?f }";
    let full = q(&s, text);
    assert!(has_cached(&q(&s, text).plan));
    let o = QueryOptions {
        max_memory_bytes: Some(full.mem_peak_bytes / 2),
        ..Default::default()
    };
    assert!(matches!(
        query(s.snapshot(), text, &o),
        Err(Error::BudgetExceeded(b)) if b.kind == BudgetKind::Memory
    ));
    // the WHERE clause of an update fails before anything is written
    let before = s.snapshot().len();
    let o = QueryOptions {
        max_memory_bytes: Some(1024),
        ..Default::default()
    };
    let r = update::update(
        &s,
        "INSERT { ?a <urn:x> ?d } WHERE { ?a ?b ?c . ?d ?e ?f }",
        &o,
    );
    assert!(
        matches!(r, Err(Error::BudgetExceeded(b)) if b.kind == BudgetKind::Memory),
        "{:?}",
        r.map(|s| s.inserted)
    );
    assert_eq!(s.snapshot().len(), before);
    let st = update::update(
        &s,
        "INSERT { ?a <urn:x> 1 } WHERE { ?a a <http://xmlns.com/foaf/0.1/Person> }",
        &QueryOptions::default(),
    )
    .unwrap();
    assert_eq!(st.inserted, 3);
    assert!(st.mem_peak_bytes >= 3 * 8);
}

#[test]
fn limited_writer_enforces_the_result_size() {
    use crate::error::{BudgetKind, Error};
    use results::{LimitedWriter, SolutionsFormat};
    let s = store();
    let r = q(&s, "SELECT * { ?s ?p ?o }");
    let mut buf = Vec::new();
    results::write_solutions(&r, SolutionsFormat::Tsv, &mut buf, None).unwrap();
    let size = buf.len() as u64;
    // exactly the size fits
    let mut w = LimitedWriter::new(Vec::new(), Some(size), None);
    results::write_solutions(&r, SolutionsFormat::Tsv, &mut w, None).unwrap();
    assert_eq!(w.written(), size);
    // one byte less does not
    let mut w = LimitedWriter::new(Vec::new(), Some(size - 1), None);
    let e = results::write_solutions(&r, SolutionsFormat::Tsv, &mut w, None).unwrap_err();
    match w.classify(e) {
        Error::BudgetExceeded(b) => {
            assert_eq!((b.kind, b.limit), (BudgetKind::ResultBytes, size - 1));
            assert!(b.requested > size - 1);
        }
        e => panic!("{e}"),
    }
    // a cancelled request stops writing
    let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
    let mut w = LimitedWriter::new(Vec::new(), None, Some(cancel));
    let big = vec![b'x'; 128 << 10];
    let e = std::io::Write::write_all(&mut w, &big).unwrap_err();
    assert!(matches!(w.classify(Error::Io(e)), Error::Cancelled));
}

#[test]
fn exports_read_through_the_block_cache() {
    let s = store();
    let before = s.cache().bytes();
    let mut out = Vec::new();
    let n = s.dump_nquads(&mut out).unwrap();
    assert!(n > 0);
    // a full export decodes every block but keeps none of them
    assert_eq!(s.cache().bytes(), before);
    // a query still fills the cache, and the export can use what it holds
    q(&s, "SELECT * { ?s ?p ?o }");
    let warm = s.cache().bytes();
    assert!(warm > before);
    let mut again = Vec::new();
    assert_eq!(s.dump_nquads(&mut again).unwrap(), n);
    assert_eq!(s.cache().bytes(), warm);
    assert_eq!(out, again);
}
