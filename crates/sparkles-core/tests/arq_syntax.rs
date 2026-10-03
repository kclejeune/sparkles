//! Jena ARQ's syntax extensions (spec G06): `LATERAL`, property path ranges and CONSTRUCT
//! templates with `GRAPH`, against ARQ's own tests and Jena 6.2.0's answers.
//!
//! The cases cite their source: the syntax and evaluation tests of Jena's
//! `jena-arq/testing/ARQ` (`Syntax-Lateral`, `Lateral`, `Syntax-ARQ`), the unit tests
//! `TestPath` and `TestPathQuery` of `org.apache.jena.sparql.path`, and the output of
//! Jena 6.2.0's `arq` command for the same data and query ("arq 6.2.0"). Expected
//! solutions are compared as bags: rows sorted, cells abbreviated with the prefixes
//! below and separated by spaces, `-` for unbound.

use oxrdf::Term;
use sparkles_core::io::{RdfFormat, Source};
use sparkles_core::sparql::{QueryKind, QueryOptions, query};
use sparkles_core::store::{Store, StoreOptions};

const PREFIXES: &str = "PREFIX : <http://example/>
PREFIX ex: <http://example.org/>
PREFIX rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#>
";

fn store(data: &str, format: RdfFormat) -> Store {
    let s = Store::in_memory(StoreOptions::default());
    s.load(&[Source::from_bytes(data.as_bytes().to_vec(), format, None)])
        .unwrap();
    s
}

fn ttl(data: &str) -> Store {
    store(data, RdfFormat::Turtle)
}

fn short(t: &Option<Term>) -> String {
    match t {
        None => "-".into(),
        Some(Term::NamedNode(n)) => {
            let s = n.as_str();
            if let Some(l) = s.strip_prefix("http://example/") {
                format!(":{l}")
            } else if let Some(l) = s.strip_prefix("http://example.org/") {
                format!("ex:{l}")
            } else {
                format!("<{s}>")
            }
        }
        Some(Term::Literal(l)) => l.value().to_string(),
        Some(t) => t.to_string(),
    }
}

fn rows(s: &Store, q: &str) -> Vec<String> {
    let text = format!("{PREFIXES}{q}");
    let r =
        query(s.snapshot(), &text, &QueryOptions::default()).unwrap_or_else(|e| panic!("{q}: {e}"));
    let mut out: Vec<String> = r
        .rows()
        .iter()
        .map(|row| row.iter().map(short).collect::<Vec<_>>().join(" "))
        .collect();
    out.sort();
    out
}

/// The query's solutions as a bag, against `want` (one row per line).
fn check(s: &Store, q: &str, want: &str) {
    let mut want: Vec<String> = want
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(|l| l.split_whitespace().collect::<Vec<_>>().join(" "))
        .collect();
    want.sort();
    assert_eq!(rows(s, q), want, "{q}");
}

fn parses(q: &str) -> bool {
    sparkles_core::sparql::parse_query(&format!("{PREFIXES}{q}"), None, &[]).is_ok()
}

fn parses_strict(q: &str) -> bool {
    spargebra::SparqlParser::new()
        .with_arq_syntax(false)
        .parse_query(&format!("{PREFIXES}{q}"))
        .is_ok()
}

// ------------------------------------------------------------------- LATERAL ------

/// `testing/ARQ/Syntax-Lateral`: syntax-lateral-01 … 05 and syntax-lateral-bad-01 … 08.
#[test]
fn lateral_syntax_matches_arq() {
    for ok in [
        "SELECT * { LATERAL {} }",
        "SELECT * { ?s ?p ?o LATERAL { SELECT ?x { ?s ?q ?x } LIMIT 2 } }",
        "SELECT * { ?s ?p ?o LATERAL { OPTIONAL { ?s ?q ?x } } }",
        "SELECT * { ?s ?p ?o LATERAL { SELECT ?s { BIND(123 AS ?o) } } }",
    ] {
        assert!(parses(ok), "{ok}");
        assert!(!parses_strict(ok), "strict: {ok}");
    }
    for bad in [
        "SELECT * { LATERAL }",
        "SELECT * { LATERAL OPTIONAL { ?s ?p ?o } }",
        "SELECT * { OPTIONAL LATERAL { ?s ?p ?o } }",
        "SELECT * { ?s ?p ?o LATERAL { BIND( 123 AS ?o) } }",
        "SELECT * { ?s ?p ?o LATERAL { OPTIONAL { BIND( 123 AS ?o) } } }",
        "SELECT * { ?s ?p ?o LATERAL { ?s ?p ?o VALUES ?o {123 456} } }",
        "SELECT * { ?s ?p ?o LATERAL { SELECT (123 As ?o) {} } }",
        "SELECT * { ?s ?p ?o LATERAL { SELECT * { BIND(123 AS ?o) } } }",
    ] {
        assert!(!parses(bad), "{bad}");
    }
    // a prefixed name is still a prefixed name
    assert!(parses_strict(
        "PREFIX lateral: <http://l/> SELECT * { lateral:s ?p ?o }"
    ));
}

/// The data of `testing/ARQ/Lateral/data.ttl`.
const LATERAL_DATA: &str = r#"PREFIX : <http://example/>
:s1 :p 1 .
:s1 :label "s1-one" .
:s2 :p 2 .
:s2 :label "s2-one" .
:s2 :label "s2-two" .
:s3 :p 3 .
:s3 :label "s3-one" .
:s3 :label "s3-two" .
:s3 :label "s3-three" .
:x1 :q 1 .
:z1 :q 1 .
:x0 :q "a" .
:z0 :q "b" .
"#;

/// The data of `testing/ARQ/Lateral/data2.ttl`.
const LATERAL_DATA2: &str = r#"@prefix ex: <http://example.org/> .
ex:s1 a ex:T ; ex:p "11" , "12" , "13" .
ex:s2 a ex:T ; ex:p "21" , "22" , "23" .
ex:s3 a ex:T .
"#;

/// `testing/ARQ/Lateral`: lateral-1 … lateral-5, lateral-in-optional and
/// optional-in-lateral, with the results of their `.srj` files.
#[test]
fn lateral_matches_arq() {
    let s = ttl(LATERAL_DATA);
    // lateral-1: the sub-select orders by a constant, so which two labels of :s3 come
    // first is up to the engine (ARQ's file has s3-one and s3-three); the count per
    // subject is not
    let r = rows(
        &s,
        "SELECT ?s ?label { ?s :p ?o LATERAL { SELECT * { ?s :label ?label } ORDER BY ?s LIMIT 2 } }",
    );
    let count = |p: &str| r.iter().filter(|l| l.starts_with(p)).count();
    assert_eq!(
        (count(":s1 "), count(":s2 "), count(":s3 ")),
        (1, 2, 2),
        "{r:?}"
    );
    assert!(r.iter().all(|l| {
        let (s, label) = l.split_once(' ').unwrap();
        label.starts_with(&s[1..])
    }));
    // lateral-2
    check(
        &s,
        "SELECT * { LATERAL { ?s :label ?label } }",
        ":s1 s1-one\n:s2 s2-one\n:s2 s2-two\n:s3 s3-one\n:s3 s3-two\n:s3 s3-three",
    );
    // lateral-3: the FILTERs see ?s and ?z
    check(
        &s,
        "SELECT ?s ?z ?x ?v { ?s :q ?z . LATERAL { ?x :q ?v . FILTER( ?x != ?s) FILTER( ?v = ?z ) } }",
        ":z1 1 :x1 1\n:x1 1 :z1 1",
    );
    // lateral-4: ?s in scope in the sub-select
    check(
        &s,
        "SELECT ?s ?o ?z { ?s :p ?o . LATERAL { SELECT ?s ?z { ?s :p ?z } } }",
        ":s1 1 1\n:s2 2 2\n:s3 3 3",
    );
    // lateral-5: the sub-select's ?s is another variable
    check(
        &s,
        "SELECT ?s ?o ?z { ?s :p ?o . LATERAL { SELECT ?z { ?s :p ?z } } }",
        ":s1 1 1\n:s1 1 2\n:s1 1 3\n:s2 2 1\n:s2 2 2\n:s2 2 3\n:s3 3 1\n:s3 3 2\n:s3 3 3",
    );
    let s2 = ttl(LATERAL_DATA2);
    // lateral-in-optional: the LATERAL sees only the OPTIONAL's group
    check(
        &s2,
        "SELECT ?s ?o { ?s a ex:T OPTIONAL { LATERAL { SELECT ?s ?o { ?s ex:p ?o } ORDER BY ?o LIMIT 2 } } }",
        "ex:s1 11\nex:s1 12\nex:s2 -\nex:s3 -",
    );
    // optional-in-lateral: the top two per subject
    check(
        &s2,
        "SELECT ?s ?o { ?s a ex:T LATERAL { OPTIONAL { SELECT ?s ?o { ?s ex:p ?o } ORDER BY ?o LIMIT 2 } } }",
        "ex:s1 11\nex:s1 12\nex:s2 21\nex:s2 22\nex:s3 -",
    );
}

/// The plan: a LATERAL that substitution cannot change is a join; one with a LIMIT per
/// row is a `Lateral` operator.
#[test]
fn lateral_plans() {
    let s = ttl(LATERAL_DATA);
    let ops = |q: &str| {
        let (_, plan) = sparkles_core::sparql::explain(
            s.snapshot(),
            &format!("{PREFIXES}{q}"),
            &QueryOptions::default(),
        )
        .unwrap();
        serde_json::to_string(&plan).unwrap()
    };
    let join = ops("SELECT * { ?s :p ?o LATERAL { ?s :label ?l } }");
    assert!(!join.contains("\"Lateral\""), "{join}");
    let per_row = ops("SELECT * { ?s :p ?o LATERAL { SELECT ?s ?l { ?s :label ?l } LIMIT 1 } }");
    assert!(per_row.contains("\"Lateral\""), "{per_row}");
    // the sub-select's ?s is another variable: a cross product
    let hidden = ops("SELECT * { ?s :p ?o LATERAL { SELECT ?l { ?s :label ?l } LIMIT 1 } }");
    assert!(!hidden.contains("\"Lateral\""), "{hidden}");
    // a FILTER over a variable the right side does not bind needs substitution
    let filtered = ops("SELECT * { ?s :q ?z LATERAL { ?x :q ?v FILTER(?x != ?s) } }");
    assert!(filtered.contains("\"Lateral\""), "{filtered}");
}

/// Substitution inside the right side: GRAPH, aggregates, BIND, a variable left unbound
/// by OPTIONAL, and EXISTS (arq 6.2.0).
#[test]
fn lateral_substitutes_everywhere() {
    let s = store(
        r#"
@prefix : <http://example/> .
:a :knows :b , :c , :d .
:b :knows :c .
:c :age 30 .
:d :age 40 .
:b :age 20 .
:g1 { :a :in :g1 . :b :in :g1 }
:g2 { :a :in :g2 }
"#,
        RdfFormat::TriG,
    );
    // an aggregate per row; without ?p in the projection, the sub-select's ?p is
    // another variable
    check(
        &s,
        "SELECT ?p ?n { ?p :knows ?x LATERAL { SELECT ?p (COUNT(*) AS ?n) { ?p :knows ?y } GROUP BY ?p } }",
        ":a 3\n:a 3\n:a 3\n:b 1",
    );
    check(
        &s,
        "SELECT ?p ?n { ?p :knows ?x LATERAL { SELECT (COUNT(*) AS ?n) { ?p :knows ?y } } }",
        ":a 4\n:a 4\n:a 4\n:b 4",
    );
    // the oldest friend per person
    check(
        &s,
        "SELECT ?p ?f { VALUES ?p { :a :b } LATERAL { SELECT ?p ?f { ?p :knows ?f . ?f :age ?age } ORDER BY DESC(?age) LIMIT 1 } }",
        ":a :d\n:b :c",
    );
    check(
        &s,
        "SELECT ?p ?f { VALUES ?p { :a :b } LATERAL { SELECT ?f { ?p :knows ?f . ?f :age ?age } ORDER BY DESC(?age) LIMIT 1 } }",
        ":a :d\n:b :d",
    );
    // a GRAPH name bound on the left
    check(
        &s,
        "SELECT ?g ?x { VALUES ?g { :g1 :g2 } LATERAL { GRAPH ?g { ?x :in ?g OPTIONAL { ?x :knows ?k } } } }",
        ":g1 :a\n:g1 :b\n:g2 :a",
    );
    // BIND over a left value, and an unbound left value is not substituted
    check(
        &s,
        "SELECT ?p ?age ?next { ?p :knows ?f OPTIONAL { ?p :age ?age } LATERAL { BIND(?age + 1 AS ?next) } }",
        ":a - -\n:a - -\n:a - -\n:b 20 21",
    );
    // EXISTS inside the right side sees the left value
    check(
        &s,
        "SELECT ?p ?f { ?p :knows ?f LATERAL { FILTER NOT EXISTS { ?p :knows ?x . ?x :knows ?f } } }",
        ":a :b\n:a :d\n:b :c",
    );
}

// --------------------------------------------------------------- path ranges ------

/// `TestPath`'s linear graph `graph1` and DAG `graph3`.
const TESTPATH: &str = r#"
@prefix : <http://example/> .
:n1 :p :n2 . :n2 :p :n3 . :n3 :p :n4 .
:d1 :r :d2 . :d1 :r :d3 . :d2 :r :d4 . :d3 :r :d4 .
"#;

/// `TestPath` path_02 … path_09 (graph1) and path_32 … path_34 (graph3).
#[test]
fn path_ranges_match_testpath() {
    let s = ttl(TESTPATH);
    for (path, want) in [
        (":p{0}", ":n1"),
        (":p{1}", ":n2"),
        (":p{2}", ":n3"),
        (":p{0,1}", ":n1 :n2"),
        (":p{0,2}", ":n1 :n2 :n3"),
        (":p{1,2}", ":n2 :n3"),
        (":p{9,9}", ""),
        (":p{0,9}", ":n1 :n2 :n3 :n4"),
        (":p*", ":n1 :n2 :n3 :n4"),
        (":p+", ":n2 :n3 :n4"),
    ] {
        check(
            &s,
            &format!("SELECT ?o {{ :n1 {path} ?o }}"),
            &want.replace(' ', "\n"),
        );
    }
    for (path, want) in [
        (":r{*}", ":d1 :d2 :d3 :d4 :d4"),
        (":r*", ":d1 :d2 :d3 :d4"),
        (":r+", ":d2 :d3 :d4"),
    ] {
        check(
            &s,
            &format!("SELECT ?o {{ :d1 {path} ?o }}"),
            &want.replace(' ', "\n"),
        );
    }
}

/// `TestPathQuery`: testPathByQuery_unboundEnds_05 and the zero-length cases.
#[test]
fn path_ranges_match_testpathquery() {
    let s = ttl(r#"PREFIX : <http://example/>
:s :p 123 . :s :p 456 . :s :q 'abc'. :s :q 'def'. :x :p :s . :x :q :s ."#);
    assert_eq!(
        rows(&s, "SELECT ?s ?o { ?s :p{1,3} ?o }"),
        rows(&s, "SELECT ?s ?o { { ?s :p ?o } UNION { ?s :p [:p ?o] } }"),
    );
    let empty = ttl("");
    let count = |s: &Store, q: &str| rows(s, q).len();
    assert_eq!(count(&empty, "SELECT * { VALUES ?v { 1 } ?v :p{0} ?v }"), 0);
    assert_eq!(
        count(&empty, "SELECT * { VALUES ?v { 1 } ?v :p{0,3} ?v }"),
        0
    );
    assert_eq!(count(&s, "SELECT * { VALUES ?v { :s } ?v :p{0} ?v }"), 1);
    let loop_ = ttl("PREFIX : <http://example/> :s :p :s .");
    assert_eq!(
        count(&loop_, "SELECT * { VALUES ?v { :s } ?v :p{1} ?v }"),
        1
    );
}

/// A DAG and a cycle (arq 6.2.0): ranges count each way through the graph, `*` and `+`
/// do not.
const WAYS: &str = r#"
@prefix : <http://example/> .
:n1 :p :n2 . :n1 :p :n3 . :n2 :p :n4 . :n3 :p :n4 .
:c1 :q :c2 . :c2 :q :c1 . :c2 :q :c3 .
"#;

#[test]
fn path_ranges_match_arq() {
    let s = ttl(WAYS);
    for (q, want) in [
        (":n1 :p{2} ?o", ":n4\n:n4"),
        (":n1 :p{1,} ?o", ":n2\n:n3\n:n4\n:n4"),
        (":n1 :p{,2} ?o", ":n1\n:n2\n:n3\n:n4\n:n4"),
        (":n1 :p{0,2} ?o", ":n1\n:n2\n:n3\n:n4\n:n4"),
        (":n1 :p{*} ?o", ":n1\n:n2\n:n3\n:n4\n:n4"),
        (":n1 :p* ?o", ":n1\n:n2\n:n3\n:n4"),
        (":c1 :q{1,} ?o", ":c1\n:c2\n:c3"),
        (":c1 :q{2,} ?o", ":c1\n:c2\n:c3\n:c3"),
        (":c1 :q{3,} ?o", ":c1\n:c2\n:c3"),
        (":c1 :q{1,3} ?o", ":c1\n:c2\n:c2\n:c3"),
        (":c1 :q{*} ?o", ":c1\n:c2\n:c3"),
        (":c1 :q{+} ?o", ":c1\n:c2\n:c3"),
        (":c2 :q{1,} ?o", ":c1\n:c2\n:c3\n:c3"),
        (":c3 :q{+} ?o", ""),
        (":c1 :q{0,1} ?o", ":c1\n:c2"),
        (":n1 (:p/:p){1} ?o", ":n4\n:n4"),
        (":n4 ^:p{2} ?o", ":n1\n:n1"),
        (":n1 ^:p{2} ?o", ""),
        (":zz :q{0} ?o", ":zz"),
        (":zz :q{0,2} ?o", ":zz"),
    ] {
        check(&s, &format!("SELECT ?o {{ {q} }}"), want);
    }
    for (q, want) in [
        ("?s :q{2,} :c3", ":c1\n:c1\n:c2"),
        ("?s :p{1,2} :n4", ":n1\n:n1\n:n2\n:n3"),
        ("?s (:q|:q){2} :c1", ":c1\n:c1\n:c1\n:c1"),
        ("?s :q{0} :zz", ":zz"),
        ("?s :q{2} ?s", ":c1\n:c2"),
    ] {
        check(&s, &format!("SELECT ?s {{ {q} }}"), want);
    }
    check(
        &s,
        "SELECT ?s ?o { ?s :q{1,2} ?o }",
        ":c1 :c1\n:c1 :c2\n:c1 :c3\n:c2 :c1\n:c2 :c2\n:c2 :c3",
    );
    // both ends constant: one solution per walk
    assert_eq!(rows(&s, "SELECT * { :n1 :p{2} :n4 }").len(), 2);
    // with two variable ends, a zero-length range covers the graph's nodes
    assert_eq!(rows(&s, "SELECT * { ?s :p{0} ?o }").len(), 7);
}

/// `{0,}` means `{*}` (Jena 6.2.0 evaluates it as `{+}`, see spec G06 §4.2), and a range
/// beyond the planner's unrolling runs in the path operator with the same counts.
#[test]
fn path_range_forms() {
    let s = ttl(WAYS);
    assert_eq!(
        rows(&s, "SELECT ?o { :n1 :p{0,} ?o }"),
        rows(&s, "SELECT ?o { :n1 :p{*} ?o }")
    );
    assert_eq!(
        rows(&s, "SELECT ?s ?o { ?s :q{40,} ?o }"),
        rows(&s, "SELECT ?s ?o { ?s :q{40} ?m . ?m :q{*} ?o }")
    );
    check(&s, "SELECT ?o { :c1 :q{40,41} ?o }", ":c1\n:c2\n:c3");
    assert!(!parses("SELECT * { ?s :p{3,2} ?o }"));
    assert!(!parses("SELECT * { ?s :p{-1} ?o }"));
    assert!(parses("SELECT * { ?s :p { 1 , 2 } ?o }"));
    assert!(!parses_strict("SELECT * { ?s :p{2} ?o }"));
    // the algebra and its SPARQL form
    for (path, sse, text) in [
        (":p{2}", "(pathN 2 ", "{2}"),
        (":p{1,3}", "(mod 1 3 ", "{1,3}"),
        (":p{2,}", "(mod 2 _ ", "{2,}"),
        (":p{,3}", "(mod 0 3 ", "{0,3}"),
        (":p{*}", "(pathN* ", "{*}"),
        (":p{+}", "(mod 1 _ ", "{1,}"),
    ] {
        let q = sparkles_core::sparql::parse_query(
            &format!("{PREFIXES}SELECT * {{ ?s {path} ?o }}"),
            None,
            &[],
        )
        .unwrap();
        assert!(q.to_sse().contains(sse), "{path}: {}", q.to_sse());
        assert!(q.to_string().contains(text), "{path}: {q}");
        assert_eq!(
            spargebra::SparqlParser::new()
                .parse_query(&q.to_string())
                .unwrap(),
            q
        );
    }
}

// ------------------------------------------------------------ CONSTRUCT quads ------

/// `testing/ARQ/Syntax-ARQ`: syntax-quad-construct-01 … 12 and -bad-01.
#[test]
fn construct_quads_syntax_matches_arq() {
    for ok in [
        "CONSTRUCT { GRAPH :g { :s :p :o } } WHERE {}",
        "CONSTRUCT { GRAPH ?g { ?s ?p ?o } } WHERE { ?s ?p ?o }",
        "CONSTRUCT { :s :p :o } WHERE {}",
        "CONSTRUCT { GRAPH ?g { :s :p :o } ?s ?p ?o } WHERE { GRAPH ?g { ?s ?p ?o } }",
        "CONSTRUCT { ?s ?p ?o GRAPH ?g { :s :p :o } } WHERE { GRAPH ?g { ?s ?p ?o } }",
        "CONSTRUCT { GRAPH ?g { :s :p :o } ?s ?p ?o . ?s ?p ?o . GRAPH ?g { ?s ?p ?o } ?s ?p ?o . ?s ?p ?o GRAPH ?g { ?s ?p ?o } } WHERE { GRAPH ?g { ?s ?p ?o } }",
        "CONSTRUCT { GRAPH <urn:x-arq:DefaultGraphNode> {:s :p :o .} } WHERE {}",
        "CONSTRUCT { GRAPH ?g { :s :p :o } GRAPH ?g1 { :s :p :o } } WHERE { }",
        "CONSTRUCT { { ?s ?p ?o } } WHERE { }",
        "CONSTRUCT WHERE { ?s ?p ?o }",
        "CONSTRUCT WHERE { GRAPH ?g { ?s ?p ?o } }",
        "CONSTRUCT WHERE { { ?s ?p ?o } }",
    ] {
        assert!(parses(ok), "{ok}");
    }
    assert!(!parses(
        "CONSTRUCT WHERE { GRAPH ?g { ?s ?p ?o. FILTER isIRI(?o) } }"
    ));
    assert!(!parses_strict(
        "CONSTRUCT { GRAPH :g { :s :p :o } } WHERE {}"
    ));
    assert!(parses_strict("CONSTRUCT { :s :p :o } WHERE {}"));
}

#[test]
fn construct_quads() {
    let s = store(
        r#"
@prefix : <http://example/> .
:a :p 1 .
:g1 { :b :p 2 . :c :p 3 }
:g2 { :d :p 4 }
"#,
        RdfFormat::TriG,
    );
    let run = |q: &str| {
        query(
            s.snapshot(),
            &format!("{PREFIXES}{q}"),
            &QueryOptions::default(),
        )
        .unwrap()
    };
    let r = run("CONSTRUCT { GRAPH ?g { ?s :p ?o } ?s :in ?g } WHERE { GRAPH ?g { ?s :p ?o } }");
    assert_eq!(r.kind, QueryKind::Construct);
    assert_eq!(r.triples.len(), 3);
    assert_eq!(r.quads.len(), 3);
    assert_eq!(r.len(), 6);
    let mut graphs: Vec<String> = r.quads.iter().map(|q| q.graph_name.to_string()).collect();
    graphs.sort();
    assert_eq!(
        graphs,
        [
            "<http://example/g1>",
            "<http://example/g1>",
            "<http://example/g2>"
        ]
    );
    // the default graph by name, an unbound graph name, and a blank node per solution
    let r = run(
        "CONSTRUCT { GRAPH <urn:x-arq:DefaultGraphNode> { ?s :p ?o } GRAPH ?none { ?s :q ?o } GRAPH _:b { ?s :r ?o } } WHERE { ?s :p ?o }",
    );
    assert_eq!(r.triples.len(), 1);
    assert_eq!(r.quads.len(), 1);
    assert!(r.quads[0].graph_name.is_blank_node());
    // CONSTRUCT WHERE with a GRAPH block
    let r = run("CONSTRUCT WHERE { GRAPH ?g { ?s :p ?o } }");
    assert_eq!((r.triples.len(), r.quads.len()), (0, 3));
    // N-Quads carries the quads, Turtle the default graph only (as Fuseki)
    let r = run("CONSTRUCT { ?s :p ?o GRAPH ?g { ?s :p ?o } } WHERE { GRAPH ?g { ?s :p ?o } }");
    let write = |f: RdfFormat| {
        let mut out = Vec::new();
        sparkles_core::sparql::results::write_graph(&r, f, &Default::default(), &mut out).unwrap();
        String::from_utf8(out).unwrap()
    };
    assert_eq!(write(RdfFormat::NQuads).lines().count(), 6);
    let nt = write(RdfFormat::NTriples);
    assert_eq!(nt.lines().count(), 3);
    assert!(!nt.contains("g1"));
}
