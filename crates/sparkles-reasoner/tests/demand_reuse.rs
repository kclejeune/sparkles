//! Reuse of derived tables across commits (spec F17 §6.2): a commit whose changed quads the
//! rules cannot read or give leaves the tables of the commit before it valid, and every
//! other commit derives again. Each case runs a query, commits a change, runs it again and
//! compares the answer with a fresh store that holds the data after the change.

use sparkles_core::io::{RdfFormat, Source};
use sparkles_core::sparql::{QueryOptions, QueryResult, query};
use sparkles_core::store::{Store, StoreOptions};
use sparkles_reasoner::query::QueryReasoner;
use sparkles_reasoner::{Extras, Inputs, Profile};

const PREFIXES: &str = "@prefix ex: <http://ex.org/> .
@prefix rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#> .
@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
@prefix owl: <http://www.w3.org/2002/07/owl#> .
";
const SPARQL_PREFIXES: &str = "PREFIX ex: <http://ex.org/>
PREFIX rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#>
PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#>
PREFIX owl: <http://www.w3.org/2002/07/owl#>
";

fn store(ttl: &str) -> Store {
    let s = Store::in_memory(StoreOptions::default());
    s.load(&[Source::from_bytes(
        format!("{PREFIXES}{ttl}").into_bytes(),
        RdfFormat::Turtle,
        None,
    )])
    .unwrap();
    s
}

fn update(s: &Store, u: &str) {
    sparkles_core::sparql::update::update(
        s,
        &format!("{SPARQL_PREFIXES}{u}"),
        &QueryOptions::default(),
    )
    .unwrap_or_else(|e| panic!("{u}: {e}"));
}

fn demand(r: &QueryReasoner) -> QueryOptions {
    QueryOptions {
        prefixes: vec![("ex".into(), "http://ex.org/".into())],
        reasoning: Some(r.provider()),
        ..Default::default()
    }
}

fn reasoner(p: &Profile) -> QueryReasoner {
    QueryReasoner::new(p.clone(), &Extras::default(), Inputs::default()).unwrap()
}

/// The solutions as sorted strings, duplicates kept.
fn rows(r: &QueryResult) -> Vec<String> {
    let mut v: Vec<String> = r
        .rows()
        .unwrap()
        .iter()
        .map(|row| {
            row.values()
                .iter()
                .map(|t| t.as_ref().map_or("UNDEF".into(), |t| t.to_string()))
                .collect::<Vec<_>>()
                .join(" ")
        })
        .collect();
    v.sort();
    v
}

fn run(s: &Store, q: &str, o: &QueryOptions) -> QueryResult {
    query(s.snapshot(), q, o).unwrap_or_else(|e| panic!("{q}: {e}"))
}

/// What the second run of the query reported: (tables, cached).
struct Second {
    tables: u64,
    cached: u64,
}

/// Run `q` over `before`, commit `change`, run it again, and check the second answer
/// against a fresh store of `after` with a reasoner of its own.
fn case(profile: &Profile, before: &str, change: &str, after: &str, q: &str) -> Second {
    let s = store(before);
    let qr = reasoner(profile);
    let first = run(&s, q, &demand(&qr));
    assert_eq!(
        rows(&first),
        rows(&run(&store(before), q, &demand(&reasoner(profile))))
    );
    update(&s, change);
    let second = run(&s, q, &demand(&qr));
    let expected = run(&store(after), q, &demand(&reasoner(profile)));
    assert_eq!(rows(&second), rows(&expected), "after {change}");
    let a = second.reasoning().cloned().unwrap();
    Second {
        tables: a.report.tables,
        cached: a.report.cached,
    }
}

const SCHEMA: &str = "ex:C rdfs:subClassOf ex:B . ex:y a ex:C . ex:u ex:knows ex:v .";
const TYPES_OF_B: &str = "SELECT ?x { ?x a ex:B }";

/// A commit of facts that no rule reads or gives keeps the tables: the second query
/// reads every table from the cache.
#[test]
fn an_unrelated_commit_keeps_the_tables() {
    let r = case(
        &Profile::OwlRl,
        SCHEMA,
        "INSERT DATA { ex:y ex:note \"n\" }",
        &format!("{SCHEMA} ex:y ex:note \"n\" ."),
        TYPES_OF_B,
    );
    assert!(
        r.tables > 0 && r.cached == r.tables,
        "{} {}",
        r.tables,
        r.cached
    );
}

/// A commit in a graph the rules do not read keeps the tables.
#[test]
fn a_commit_outside_the_input_graphs_keeps_the_tables() {
    let r = case(
        &Profile::OwlRl,
        SCHEMA,
        "INSERT DATA { GRAPH ex:g { ex:z a ex:C } }",
        SCHEMA,
        TYPES_OF_B,
    );
    assert!(
        r.tables > 0 && r.cached == r.tables,
        "{} {}",
        r.tables,
        r.cached
    );
}

/// An added fact that a rule's atom reads derives again.
#[test]
fn an_added_premise_derives_again() {
    let r = case(
        &Profile::OwlRl,
        SCHEMA,
        "INSERT DATA { ex:z a ex:C }",
        &format!("{SCHEMA} ex:z a ex:C ."),
        TYPES_OF_B,
    );
    assert_eq!(r.cached, 0);
}

/// A deleted premise derives again, so its consequence goes.
#[test]
fn a_deleted_premise_derives_again() {
    let r = case(
        &Profile::OwlRl,
        SCHEMA,
        "DELETE DATA { ex:y a ex:C }",
        "ex:C rdfs:subClassOf ex:B . ex:u ex:knows ex:v .",
        TYPES_OF_B,
    );
    assert_eq!(r.cached, 0);
}

/// A schema fact changes the specialized rules, so it derives again.
#[test]
fn a_schema_fact_derives_again() {
    let before = format!("{SCHEMA} ex:w a ex:D .");
    let r = case(
        &Profile::OwlRl,
        &before,
        "INSERT DATA { ex:D rdfs:subClassOf ex:B }",
        &format!("{before} ex:D rdfs:subClassOf ex:B ."),
        TYPES_OF_B,
    );
    assert_eq!(r.cached, 0);
}

/// A schema predicate that the store did not hold, which the rules named by a local id,
/// derives again once a fact of it is stored.
#[test]
fn an_absent_schema_predicate_derives_again() {
    let before = format!("{SCHEMA} ex:w a ex:D .");
    let r = case(
        &Profile::OwlRl,
        &before,
        "INSERT DATA { ex:D owl:equivalentClass ex:B }",
        &format!("{before} ex:D owl:equivalentClass ex:B ."),
        TYPES_OF_B,
    );
    assert_eq!(r.cached, 0);
}

/// A stored fact that rules also derive is no derived fact of the commit that stores it,
/// so its tables leave it out. Once it is deleted, it is an answer only as a derived fact,
/// so the next commit derives again.
#[test]
fn a_deleted_fact_that_rules_derive_derives_again() {
    let before = format!("{SCHEMA} ex:y a ex:B .");
    let r = case(
        &Profile::OwlRl,
        &before,
        "DELETE DATA { ex:y a ex:B }",
        SCHEMA,
        TYPES_OF_B,
    );
    assert_eq!(r.cached, 0);
}

/// A stored copy of a derived fact is a fact that rules give, so a commit that adds one
/// derives again rather than read tables that hold the fact as derived.
#[test]
fn a_stored_copy_of_a_derived_fact_derives_again() {
    let r = case(
        &Profile::OwlRl,
        SCHEMA,
        "INSERT DATA { ex:y a ex:B }",
        &format!("{SCHEMA} ex:y a ex:B ."),
        TYPES_OF_B,
    );
    assert_eq!(r.cached, 0);
}

/// A term the store gains may be a constant of the rules that the store did not hold
/// before, which the rules' patterns named by a local id.
#[test]
fn a_new_term_that_rules_name_derives_again() {
    let rules = Profile::Rules(
        "[hit: (?x <http://ex.org/p> <http://ex.org/Special>) -> (?x rdf:type <http://ex.org/Hit>)]"
            .into(),
    );
    let before = "ex:b ex:p ex:Other .";
    let r = case(
        &rules,
        before,
        "INSERT DATA { ex:a ex:p ex:Special }",
        &format!("{before} ex:a ex:p ex:Special ."),
        "SELECT ?x { ?x a ex:Hit }",
    );
    assert_eq!(r.cached, 0);
}

/// The first `owl:sameAs` fact brings in the equality rules, which read every predicate.
#[test]
fn equality_derives_again() {
    let r = case(
        &Profile::OwlRl,
        SCHEMA,
        "INSERT DATA { ex:y owl:sameAs ex:y2 }",
        &format!("{SCHEMA} ex:y owl:sameAs ex:y2 ."),
        TYPES_OF_B,
    );
    assert_eq!(r.cached, 0);
}

/// A fact that a `noValue` tests retracts a consequence, so it derives again.
#[test]
fn a_negated_pattern_derives_again() {
    let rules = Profile::Rules(
        "[ok: (?x rdf:type <http://ex.org/P>) noValue(?x <http://ex.org/blocked> ?b) -> (?x rdf:type <http://ex.org/Ok>)]"
            .into(),
    );
    let before = "ex:a a ex:P . ex:c a ex:P . ex:c ex:blocked ex:t .";
    let r = case(
        &rules,
        before,
        "INSERT DATA { ex:a ex:blocked ex:t }",
        &format!("{before} ex:a ex:blocked ex:t ."),
        "SELECT ?x { ?x a ex:Ok }",
    );
    assert_eq!(r.cached, 0);
}

/// A changed RDF list changes what specialization read, so it derives again.
#[test]
fn a_changed_list_derives_again() {
    let before = "ex:I owl:intersectionOf ( ex:A ex:B2 ) . ex:z a ex:A .";
    let r = case(
        &Profile::OwlRl,
        before,
        "DELETE { ?l rdf:first ex:B2 } INSERT { ?l rdf:first ex:A } WHERE { ?l rdf:first ex:B2 }",
        "ex:I owl:intersectionOf ( ex:A ex:A ) . ex:z a ex:A .",
        "SELECT ?x { ?x a ex:I }",
    );
    assert_eq!(r.cached, 0);
}
