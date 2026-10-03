//! The rest of Jena ARQ's query language (spec G06, Phase 3): `LET`, the composite
//! datatypes with `FOLD` and `UNFOLD`, and the property function library, against ARQ's
//! tests and Jena 6.2.0's answers.
//!
//! The cases cite their source: the tests of Jena's `jena-arq/testing/ARQ`
//! (`Syntax-ARQ`) and the output of Jena 6.2.0's `arq` command for the same data and
//! query ("arq 6.2.0"). Jena's `SPARQL-CDTs` and `PropertyFunctions` suites run in full
//! from `tests/w3c.rs`.
//! Expected solutions are compared as bags: rows sorted, cells abbreviated with the
//! prefixes below and separated by spaces, `-` for unbound and `""` for the empty
//! string.

use oxrdf::Term;
use sparkles::io::{RdfFormat, Source};
use sparkles::sparql::{QueryOptions, query};
use sparkles::store::{Store, StoreOptions};

const PREFIXES: &str = "PREFIX : <http://example/>
PREFIX ex: <http://example.org/>
PREFIX rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#>
PREFIX xsd: <http://www.w3.org/2001/XMLSchema#>
PREFIX cdt: <http://w3id.org/awslabs/neptune/SPARQL-CDTs/>
PREFIX list: <http://jena.apache.org/ARQ/list#>
PREFIX apf: <http://jena.apache.org/ARQ/property#>
";

fn load(data: &str, format: RdfFormat) -> Store {
    let s = Store::in_memory(StoreOptions::default());
    s.load(&[Source::from_bytes(data.as_bytes().to_vec(), format, None)])
        .unwrap();
    s
}

fn ttl(data: &str) -> Store {
    load(data, RdfFormat::Turtle)
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
        Some(Term::Literal(l)) if l.value().is_empty() => "\"\"".into(),
        Some(Term::Literal(l)) => l.value().replace(' ', ""),
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
    sparkles::sparql::parse_query(&format!("{PREFIXES}{q}"), None, &[]).is_ok()
}

fn parses_strict(q: &str) -> bool {
    spargebra::SparqlParser::new()
        .with_arq_syntax(false)
        .parse_query(&format!("{PREFIXES}{q}"))
        .is_ok()
}

fn explain(s: &Store, q: &str) -> String {
    let (_, plan) = sparkles::sparql::explain(
        s.snapshot(),
        &format!("{PREFIXES}{q}"),
        &QueryOptions::default(),
    )
    .unwrap_or_else(|e| panic!("{q}: {e}"));
    serde_json::to_string(&plan).unwrap()
}

// ----------------------------------------------------------------------- LET ------

/// `testing/ARQ/Syntax-ARQ`: syntax-let-01, syntax-let-02 and syntax-let-bad-01.
#[test]
fn let_syntax_matches_arq() {
    for ok in [
        "SELECT * { LET ( ?x := 3 ) }",
        "SELECT * { ?s ?p ?o . OPTIONAL { ?o :p ?q LET ( ?q := true ) } }",
        // LET may assign a variable in scope, unlike BIND
        "SELECT * { ?s ?p ?o LET (?o := 1) }",
    ] {
        assert!(parses(ok), "{ok}");
        assert!(!parses_strict(ok), "strict: {ok}");
    }
    assert!(!parses("SELECT * { LET ?x := (4+5) }"));
    assert!(parses_strict(
        "PREFIX let: <http://l/> SELECT * { let:s ?p ?o }"
    ));
}

const LET_DATA: &str = "PREFIX : <http://example/>
:a :p 1 .
:b :p 2 .
:c :q 3 .
:c :p \"x\" .
";

/// arq 6.2.0: a bound variable keeps a solution whose value is the same value (`1.0` is
/// the same value as `1`, `1e0` is not), an error keeps every solution, and an unbound
/// one takes the value.
#[test]
fn let_compares_a_bound_variable() {
    let s = ttl(LET_DATA);
    check(&s, "SELECT * { ?s :p ?o LET (?o := 1) }", ":a 1");
    check(&s, "SELECT * { ?s :p ?o LET (?o := 1.0) }", ":a 1");
    check(&s, "SELECT * { ?s :p ?o LET (?o := 1e0) }", "");
    check(
        &s,
        "SELECT * { ?s :p ?o LET (?o := 1/0) }",
        ":a 1 \n :b 2 \n :c x",
    );
    check(
        &s,
        "SELECT * { ?s :p ?o LET (?x := ?o + 1) LET (?x := 2) }",
        ":a 1 2 \n :c x 2",
    );
    check(
        &s,
        "SELECT ?s ?p ?v ?o { ?s ?p ?v OPTIONAL { ?s :q ?o } LET (?o := 3) }",
        ":a :p 1 3 \n :b :p 2 3 \n :c :q 3 3 \n :c :p x 3",
    );
    check(&s, "SELECT * { LET (?x := 3) LET (?x := 4) }", "");
    check(&s, "SELECT ?s ?o { ?s :p ?o LET (?o := \"x\") }", ":c x");
}

/// A LET of a variable the pattern does not bind is planned as BIND.
#[test]
fn let_of_a_new_variable_is_bind() {
    let s = ttl(LET_DATA);
    let plan = explain(&s, "SELECT * { ?s :p ?o LET (?x := ?o + 1) }");
    assert!(plan.contains("\"Bind\""), "{plan}");
    assert!(!plan.contains("\"Let\""), "{plan}");
    let plan = explain(&s, "SELECT * { ?s :p ?o LET (?o := 1) }");
    assert!(plan.contains("\"Let\""), "{plan}");
}

// ---------------------------------------------------------- FOLD and UNFOLD ------

/// arq 6.2.0: FOLD with ORDER BY, and UNFOLD of what FOLD built, per group.
#[test]
fn fold_and_unfold_round_trip() {
    let s = ttl("PREFIX : <http://example/>
:a :p 3, 1, 2 .
:b :p 5 .
");
    check(
        &s,
        "SELECT ?s ?l { { SELECT ?s (FOLD(?o ORDER BY DESC(?o)) AS ?l) { ?s :p ?o } GROUP BY ?s } }",
        ":a [3,2,1] \n :b [5]",
    );
    check(
        &s,
        "SELECT ?s ?x ?i { { SELECT ?s (FOLD(?o ORDER BY ?o) AS ?l) { ?s :p ?o } GROUP BY ?s } UNFOLD(?l AS ?x, ?i) }",
        ":a 1 1 \n :a 2 2 \n :a 3 3 \n :b 5 1",
    );
    check(
        &s,
        "SELECT ?k ?v { { SELECT (FOLD(?o, ?s) AS ?m) { ?s :p ?o } } UNFOLD(?m AS ?k, ?v) }",
        "1 :a \n 2 :a \n 3 :a \n 5 :b",
    );
    // a value that is not a composite literal leaves both variables unbound
    check(&s, "SELECT ?x ?y { UNFOLD(42 AS ?x, ?y) }", "- -");
    // an empty list gives no solutions
    check(&s, "SELECT ?x { UNFOLD(cdt:List() AS ?x) }", "");
    // FOLD over no solutions is the empty list
    check(&s, "SELECT (FOLD(?o) AS ?l) { ?s :nothing ?o }", "[]");
}

/// `testing/ARQ` UNFOLD scope: neither variable may be in scope (`checkUNFOLD`).
#[test]
fn unfold_and_fold_syntax() {
    assert!(parses(
        "SELECT * { BIND(cdt:List(1) AS ?l) UNFOLD(?l AS ?x) }"
    ));
    assert!(parses(
        "SELECT * { BIND(cdt:List(1) AS ?l) UNFOLD(?l AS ?x, ?i) }"
    ));
    assert!(!parses(
        "SELECT * { BIND(cdt:List(1) AS ?l) UNFOLD(?l AS ?l) }"
    ));
    assert!(!parses("SELECT * { ?s ?p ?o UNFOLD(?o AS ?x, ?s) }"));
    assert!(!parses_strict("SELECT * { UNFOLD(1 AS ?x) }"));
    assert!(parses(
        "SELECT (FOLD(DISTINCT ?o ORDER BY ?s DESC(?o)) AS ?l) { ?s ?p ?o }"
    ));
    assert!(parses("SELECT (FOLD(?s, ?o) AS ?m) { ?s ?p ?o }"));
    assert!(!parses_strict("SELECT (FOLD(?o) AS ?l) { ?s ?p ?o }"));
    assert!(!parses("SELECT (FOLD(COUNT(?o)) AS ?l) { ?s ?p ?o }"));
}

// ------------------------------------------------------- property functions ------

const PF_DATA: &str = "PREFIX : <http://example/>
:a :name \"x y\" ; :list (\"b\" \"c\") .
:b :name \"z\" ; :list () .
:g1 { :s :p (\"p\" \"q\") }
:g2 { :s :p (\"r\") }
";

/// arq 6.2.0 on `PF_DATA`: the list functions over lists the group binds or finds, per
/// named graph, and the `apf:` functions with inputs from the group.
#[test]
fn property_functions_match_arq() {
    let s = load(PF_DATA, RdfFormat::TriG);
    check(
        &s,
        "SELECT ?t { ?t apf:strSplit (\"a, b,,c, , \" \",\") }",
        "a \n b \n \"\" \n c \n \"\" \n \"\"",
    );
    check(
        &s,
        "SELECT ?t { ?t apf:strSplit (\",a\" \",\") }",
        "\"\" \n a",
    );
    check(
        &s,
        "SELECT ?s ?t { ?s :name ?n . ?t apf:strSplit (?n \" \") }",
        ":a x \n :a y \n :b z",
    );
    check(
        &s,
        "SELECT ?x { \"b\" apf:strSplit (\"a b c\" \" \") BIND(1 AS ?x) }",
        "1",
    );
    check(
        &s,
        "SELECT ?z { ?s :name ?n . ?z apf:concat (\"<\" ?n \">\") }",
        "<xy> \n <z>",
    );
    check(
        &s,
        "SELECT ?g ?m { GRAPH ?g { ?l list:member ?m } }",
        ":g1 p \n :g1 q \n :g2 r",
    );
    check(
        &s,
        "SELECT ?s ?n { ?s :list ?l . ?l list:length ?n }",
        ":a 2 \n :b 0",
    );
    check(
        &s,
        "SELECT ?s ?i ?m { ?s :list ?l . ?l list:index (?i ?m) }",
        ":a 0 b \n :a 1 c",
    );
    check(
        &s,
        "SELECT ?s ?ln { ?s :name ?n OPTIONAL { ?s apf:splitIRI (?ns ?ln) } }",
        ":a a \n :b b",
    );
    check(
        &s,
        "SELECT ?n ?ln { ?s :name ?n . ?s apf:splitIRI (\"http://example/\" ?ln) }",
        "xy a \n z b",
    );
    let heads = rows(&s, "SELECT ?l { ?l list:member \"b\" }");
    assert_eq!(heads.len(), 1, "{heads:?}");
    let v = rows(&s, "SELECT ?s ?v { ?s apf:versionARQ ?v }");
    assert_eq!(
        v,
        vec![format!("<urn:x-sparkles:> {}", env!("CARGO_PKG_VERSION"))]
    );
}

/// A call that reads no variable of its group is a leaf of the join order; one that
/// reads a variable bound before it runs over the rest of the group.
#[test]
fn property_function_plans() {
    let s = load(PF_DATA, RdfFormat::TriG);
    let leaf = explain(&s, "SELECT * { ?l list:member ?m . ?x :list ?l }");
    assert!(leaf.contains("\"PropertyFunction\""), "{leaf}");
    let dependent = explain(&s, "SELECT * { ?x :list ?l . ?l list:member ?m }");
    assert!(dependent.contains("\"PropertyFunction\""), "{dependent}");
    // the OPTIONAL runs per left row, so splitIRI reads ?s
    let optional = explain(
        &s,
        "SELECT * { ?s :name ?n OPTIONAL { ?s apf:splitIRI (?ns ?ln) } }",
    );
    assert!(optional.contains("\"Lateral\""), "{optional}");
    // a query without the library plans as before
    let plain = explain(&s, "SELECT * { ?s :name ?n OPTIONAL { ?s :list ?l } }");
    assert!(plain.contains("\"OptionalJoin\""), "{plain}");
}
