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
/// reads every table from the cache. The predicate is in use before, so that `rdf1`, which
/// gives `rdf:type rdf:Property` to every predicate in use, gives nothing new.
#[test]
fn an_unrelated_commit_keeps_the_tables() {
    let r = case(
        &Profile::OwlRl,
        SCHEMA,
        "INSERT DATA { ex:y ex:knows ex:w }",
        &format!("{SCHEMA} ex:y ex:knows ex:w ."),
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

/// A rule whose every head is a schema relation that the proof closed is left out of the
/// specialized program, and what its body reads still changes the schema (Codex review
/// qtr-step2 #1). The terms the change holds are all in the store before it.
#[test]
fn a_premise_of_a_schema_rule_derives_again() {
    let rules = Profile::Rules(
        "[edge: (?c <http://ex.org/edge> ?d) -> (?c rdfs:subClassOf ?d)]
         [sco: (?c rdfs:subClassOf ?d) (?x rdf:type ?c) -> (?x rdf:type ?d)]"
            .into(),
    );
    let before = "ex:C ex:edge ex:B . ex:z a ex:C . ex:x a ex:D .
        ex:C rdfs:subClassOf ex:Top . ex:D rdfs:subClassOf ex:Top . ex:B rdfs:subClassOf ex:Top .";
    let r = case(
        &rules,
        before,
        "INSERT DATA { ex:D ex:edge ex:B }",
        &format!("{before} ex:D ex:edge ex:B ."),
        TYPES_OF_B,
    );
    assert_eq!(r.cached, 0);
}

/// A session of a later commit that reads an earlier commit's tables does not keep a
/// table that holds a term the earlier commit lacked under that commit's key, where an
/// earlier snapshot would read the term's id as unknown (Codex review qtr-step2 #2).
#[test]
fn a_later_term_is_not_kept_under_an_earlier_commit() {
    let rules = Profile::Rules(
        "[k: (?x <http://ex.org/p> ?y) -> (?x <http://ex.org/q> <http://ex.org/K>)]".into(),
    );
    let s = store("ex:a ex:p ex:b .");
    let qr = reasoner(&rules);
    let o = demand(&qr);
    // the earlier commit's view anchors, and its snapshot stays readable
    let first = run(&s, "SELECT ?x ?o { ?x ex:q ?o }", &o);
    assert_eq!(rows(&first), vec!["<http://ex.org/a> <http://ex.org/K>"]);
    let earlier = s.snapshot();
    // the store gains the term in a graph the rules do not read
    update(&s, "INSERT DATA { GRAPH ex:g { ex:z ex:r ex:K } }");
    let bound = "SELECT ?x { ?x ex:q ex:K }";
    assert_eq!(rows(&run(&s, bound, &o)), vec!["<http://ex.org/a>"]);
    let again = query(earlier, bound, &o).unwrap();
    assert_eq!(rows(&again), vec!["<http://ex.org/a>"]);
}

/// `rdf1` reads only which predicates are in use. A predicate that comes into use, or
/// goes out of it, derives again, though no other rule reads it.
#[test]
fn a_predicate_in_use_or_not_derives_again() {
    let q = "SELECT ?p { ?p a <http://www.w3.org/1999/02/22-rdf-syntax-ns#Property> }";
    let r = case(
        &Profile::OwlRl,
        SCHEMA,
        "INSERT DATA { ex:y ex:note \"n\" }",
        &format!("{SCHEMA} ex:y ex:note \"n\" ."),
        q,
    );
    assert_eq!(r.cached, 0);
    let r = case(
        &Profile::OwlRl,
        SCHEMA,
        "DELETE DATA { ex:u ex:knows ex:v }",
        "ex:C rdfs:subClassOf ex:B . ex:y a ex:C .",
        q,
    );
    assert_eq!(r.cached, 0);
}

/// The proof closes `rdfs:subClassOf` while no trigger fact holds, so specialization binds
/// `?p` and leaves the instance out. A trigger fact then derives again, whether the
/// anchor came from a session that made the proof or one that read it from the cache
/// (Codex review qtr-step2, round 3).
#[test]
fn a_premise_of_an_instance_left_out_derives_again() {
    let rules = Profile::Rules(
        "[schema: (?p rdfs:subPropertyOf <http://ex.org/marker>)
                  (<http://ex.org/x> <http://ex.org/trigger> <http://ex.org/y>)
                  -> (<http://ex.org/C> ?p <http://ex.org/B>)]
         [sco: (?c rdfs:subClassOf ?d) (?s rdf:type ?c) -> (?s rdf:type ?d)]"
            .into(),
    );
    let before = "rdfs:subClassOf rdfs:subPropertyOf ex:marker . ex:x a ex:C .
        ex:D rdfs:subClassOf ex:B . ex:w a ex:D . ex:trigger ex:note ex:y .";
    let change = "INSERT DATA { ex:x ex:trigger ex:y }";
    let after = format!("{before} ex:x ex:trigger ex:y .");
    let r = case(&rules, before, change, &after, TYPES_OF_B);
    assert_eq!(r.cached, 0);
    // At one commit, the first query makes the schema's table, the second reads it and
    // makes the completeness proof, and the third reads both and anchors the view again.
    let s = store(before);
    let qr = reasoner(&rules);
    let o = demand(&qr);
    run(&s, TYPES_OF_B, &o);
    run(&s, "SELECT ?x { ?x a ex:D }", &o);
    run(&s, "SELECT ?x { ?x a ex:C }", &o);
    update(&s, change);
    let expected = run(&store(&after), TYPES_OF_B, &demand(&reasoner(&rules)));
    assert_eq!(rows(&run(&s, TYPES_OF_B, &o)), rows(&expected));
}

/// A rule program (item 839) through a relation and a negated triple.
fn program() -> Profile {
    Profile::Rules(
        "@prefix ex: <http://ex.org/> .
.decl r(x)
r(?x) :- (?x ex:p ?y).
(?x rdf:type ex:B) :- r(?x).
(?x rdf:type ex:B) :- (?x rdf:type ex:T), not (?x ex:blocked ex:yes).
"
        .into(),
    )
}

const PROGRAM_DATA: &str = "ex:a ex:p ex:b . ex:t a ex:T . ex:u ex:knows ex:v .";

/// A rule program's tables are kept under the digest of its text: a commit that no rule
/// reads keeps them.
#[test]
fn a_program_keeps_its_tables() {
    let r = case(
        &program(),
        PROGRAM_DATA,
        "INSERT DATA { ex:y ex:knows ex:w }",
        &format!("{PROGRAM_DATA} ex:y ex:knows ex:w ."),
        TYPES_OF_B,
    );
    assert!(
        r.tables > 0 && r.cached == r.tables,
        "{} {}",
        r.tables,
        r.cached
    );
}

/// A fact that reaches the answer only through a relation derives again.
#[test]
fn a_premise_of_a_relation_derives_again() {
    let r = case(
        &program(),
        PROGRAM_DATA,
        "INSERT DATA { ex:z ex:p ex:w }",
        &format!("{PROGRAM_DATA} ex:z ex:p ex:w ."),
        TYPES_OF_B,
    );
    assert_eq!(r.cached, 0);
}

/// A fact that a negated triple reads derives again, so the blocked answer goes.
#[test]
fn a_negated_premise_derives_again() {
    let r = case(
        &program(),
        PROGRAM_DATA,
        "INSERT DATA { ex:t ex:blocked ex:yes }",
        &format!("{PROGRAM_DATA} ex:t ex:blocked ex:yes ."),
        TYPES_OF_B,
    );
    assert_eq!(r.cached, 0);
}
