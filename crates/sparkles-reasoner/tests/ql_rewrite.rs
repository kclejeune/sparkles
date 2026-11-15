//! OWL 2 QL query rewriting under `reasoning=demand` with the `owl-ql` profile (spec F17
//! §7): acceptance example A8, a union that reads the stored facts and derives nothing,
//! the fallback to the `owl-rl` demand program past `budget.maxUnion`, cancellation, and
//! the answers of the LUBM queries against an OWL 2 RL closure of LUBM's OWL 2 QL fragment.

use oxrdf::{BlankNode, Triple};
use sparkles_core::io::{RdfFormat, Source};
use sparkles_core::sparql::{QueryOptions, QueryResult, query};
use sparkles_core::store::{Store, StoreOptions};
use sparkles_reasoner::query::QueryReasoner;
use sparkles_reasoner::{Extras, INFERRED_GRAPH, Inputs, Profile, ReasonOptions, materialize};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

#[allow(dead_code)]
#[path = "../examples/common/lubm.rs"]
mod lubm;

const QUERIES: &str = include_str!("../../../scripts/bench-lubm/queries.txt");
const EX: &str = "http://ex.org/";
const HEAD: &str = "@prefix ex: <http://ex.org/> .
@prefix owl: <http://www.w3.org/2002/07/owl#> .
@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
";

fn store(ttl: &str) -> Store {
    let s = Store::in_memory(StoreOptions::default());
    s.load(&[Source::from_bytes(
        format!("{HEAD}{ttl}").into_bytes(),
        RdfFormat::Turtle,
        None,
    )])
    .unwrap();
    s
}

fn ql(max_union: Option<usize>) -> QueryOptions {
    let mut r = QueryReasoner::new(Profile::OwlQl, &Extras::default(), Inputs::default()).unwrap();
    if let Some(n) = max_union {
        r = r.with_max_union(n);
    }
    QueryOptions {
        prefixes: vec![("ex".into(), EX.into())],
        no_cache: true,
        reasoning: Some(r.provider()),
        ..Default::default()
    }
}

fn run(s: &Store, q: &str, o: &QueryOptions) -> QueryResult {
    query(s.snapshot(), q, o).unwrap_or_else(|e| panic!("{q}: {e}"))
}

/// The solutions as sorted strings, each once.
fn answers(r: &QueryResult) -> Vec<String> {
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
    v.dedup();
    v
}

const A8: &str = "ex:Professor a owl:Class .
ex:teaches a owl:ObjectProperty .
ex:Professor rdfs:subClassOf [ a owl:Restriction ; owl:onProperty ex:teaches ;
    owl:someValuesFrom owl:Thing ] .
ex:ann a ex:Professor .
";

#[test]
fn a8_an_existential_answers_ask_and_not_a_distinguished_variable() {
    let s = store(A8);
    let o = ql(None);
    let ask = run(&s, "ASK { ex:ann ex:teaches ?c }", &o);
    assert!(ask.boolean);
    let select = run(&s, "SELECT ?c { ex:ann ex:teaches ?c }", &o);
    assert_eq!(answers(&select), Vec::<String>::new());
    // the existential is anonymous: a non-projected variable takes it, a projected one not
    let who = run(&s, "SELECT ?x { ?x ex:teaches ?c }", &o);
    assert_eq!(answers(&who), vec![format!("<{EX}ann>")]);
    // the rewriting reads the stored facts: no session derives anything
    let report = &ask
        .reasoning()
        .expect("query-time reasoning applied")
        .report;
    assert_eq!((report.derived, report.tables), (0, 0));
    // the same query under owl-rl cannot use the existential
    let rl = QueryReasoner::new(Profile::OwlRl, &Extras::default(), Inputs::default()).unwrap();
    let o = QueryOptions {
        prefixes: vec![("ex".into(), EX.into())],
        reasoning: Some(rl.provider()),
        ..Default::default()
    };
    assert!(!run(&s, "ASK { ex:ann ex:teaches ?c }", &o).boolean);
}

#[test]
fn a_rewritten_union_gives_each_answer_once_and_explains_itself() {
    let s = store(
        "ex:Person a owl:Class . ex:Professor a owl:Class . ex:Student a owl:Class .
         ex:advises a owl:ObjectProperty .
         ex:Professor rdfs:subClassOf ex:Person . ex:Student rdfs:subClassOf ex:Person .
         ex:advises rdfs:range ex:Student .
         ex:ann a ex:Professor , ex:Person . ex:bob a ex:Student . ex:ann ex:advises ex:cy .",
    );
    let o = ql(None);
    let r = run(&s, "SELECT ?x { ?x a ex:Person }", &o);
    assert_eq!(
        answers(&r),
        vec![
            format!("<{EX}ann>"),
            format!("<{EX}bob>"),
            format!("<{EX}cy>")
        ]
    );
    // ann is a Person twice over, and the union gives her once
    assert_eq!(r.rows().unwrap().len(), 3);
    let (_, plan) =
        sparkles_core::sparql::explain(s.snapshot(), "SELECT ?x { ?x a ex:Person }", &o).unwrap();
    let text = format!("{plan:?}");
    assert!(text.contains("Rewrite"), "{text}");
    assert!(text.contains("into 4 branches"), "{text}");
}

#[test]
fn past_the_bound_the_owl_rl_demand_program_answers_with_a_warning() {
    let mut onto = String::from("ex:A a owl:Class . ex:B a owl:Class .\n");
    for i in 0..12 {
        onto.push_str(&format!(
            "ex:C{i} a owl:Class ; rdfs:subClassOf ex:A . ex:D{i} a owl:Class ; rdfs:subClassOf ex:B .\n"
        ));
    }
    onto.push_str("ex:x a ex:C3 . ex:y a ex:D7 .\n");
    let s = store(&onto);
    let q = "SELECT ?x ?y { ?x a ex:A . ?y a ex:B }";
    let rewritten = run(&s, q, &ql(None));
    let fallback = run(&s, q, &ql(Some(50)));
    assert_eq!(answers(&rewritten), answers(&fallback));
    assert_eq!(answers(&rewritten).len(), 1);
    assert_eq!(rewritten.reasoning().unwrap().report.tables, 0);
    assert!(fallback.reasoning().unwrap().report.tables > 0);
    let warned = format!("{:?}", fallback.plan().warnings);
    assert!(warned.contains("budget.maxUnion"), "{warned}");
}

#[test]
fn a_cancelled_query_stops_the_rewriting() {
    let mut onto = String::from("ex:A a owl:Class .\n");
    for i in 0..30 {
        onto.push_str(&format!("ex:C{i} a owl:Class ; rdfs:subClassOf ex:A .\n"));
    }
    let s = store(&onto);
    let cancel = Arc::new(AtomicBool::new(true));
    let o = QueryOptions {
        cancel: Some(cancel.clone()),
        ..ql(Some(100_000))
    };
    let e = query(
        s.snapshot(),
        "SELECT * { ?a a ex:A . ?b a ex:A . ?c a ex:A }",
        &o,
    )
    .err()
    .expect("a cancelled query fails");
    assert!(e.to_string().to_lowercase().contains("cancel"), "{e}");
    cancel.store(false, Ordering::Relaxed);
}

#[test]
fn materialization_refuses_owl_ql() {
    let s = store(A8);
    let e = materialize(&s, &Profile::OwlQl, &ReasonOptions::default()).unwrap_err();
    assert!(e.to_string().contains("owl-ql"), "{e}");
}

fn lubm_query(name: &str) -> String {
    let mut text = None::<String>;
    for line in QUERIES.lines() {
        if let Some(n) = line.strip_prefix("## ") {
            if text.is_some() {
                break;
            }
            if n.trim() == name {
                text = Some(String::new());
            }
        } else if let Some(t) = &mut text {
            t.push_str(line);
            t.push('\n');
        }
    }
    text.unwrap_or_else(|| panic!("no LUBM query {name}"))
}

/// The OWL 2 QL axioms of LUBM's ontology, written back as triples.
fn ql_fragment(ontology: &str) -> String {
    let triples: Vec<Triple> = oxttl::TurtleParser::new()
        .for_slice(ontology.as_bytes())
        .map(|t| t.unwrap())
        .collect();
    let o = sparkles_owl::read::read_graph(triples, Default::default());
    let part = sparkles_owl::profiles::partition(&o, sparkles_owl::profiles::Profile::Ql);
    assert!(!part.outside.is_empty(), "LUBM has axioms outside OWL 2 QL");
    let mut n = 0;
    let mut fresh = || {
        n += 1;
        BlankNode::new_unchecked(format!("q{n}"))
    };
    let mut out = String::new();
    for ax in &part.inside {
        for t in sparkles_owl::write::axiom_triples(ax, &mut fresh) {
            out.push_str(&format!("{t} .\n"));
        }
    }
    out
}

/// Every LUBM query under `owl-ql` over the whole ontology gives the answers of an OWL 2 RL
/// closure of its OWL 2 QL fragment, or more where the rewriting uses an existential
/// that OWL 2 RL cannot.
#[test]
fn lubm_answers_match_an_owl_2_rl_closure_of_the_ql_fragment() {
    let data = lubm::university(0);
    let full = Store::in_memory(StoreOptions::default());
    full.load(&[
        Source::from_bytes(lubm::ontology().into_bytes(), RdfFormat::Turtle, None),
        Source::from_bytes(data.clone().into_bytes(), RdfFormat::NTriples, None),
    ])
    .unwrap();
    let fragment = Store::in_memory(StoreOptions::default());
    fragment
        .load(&[
            Source::from_bytes(
                ql_fragment(&lubm::ontology()).into_bytes(),
                RdfFormat::NTriples,
                None,
            ),
            Source::from_bytes(data.into_bytes(), RdfFormat::NTriples, None),
        ])
        .unwrap();
    materialize(&fragment, &Profile::OwlRl, &ReasonOptions::default()).unwrap();
    let closure = QueryOptions {
        default_graph_extra: vec![INFERRED_GRAPH.into()],
        ..Default::default()
    };
    let o = QueryOptions {
        prefixes: Vec::new(),
        ..ql(None)
    };
    let mut equal = 0;
    for i in 1..=14 {
        let name = format!("q{i}");
        let q = lubm_query(&name);
        let reference = answers(&run(&fragment, &q, &closure));
        let got = run(&full, &q, &o);
        let rewritten = answers(&got);
        let missing: Vec<&String> = reference
            .iter()
            .filter(|a| !rewritten.contains(a))
            .collect();
        assert!(
            missing.is_empty(),
            "{name}: {} answers missing, such as {:?}",
            missing.len(),
            missing.first()
        );
        eprintln!(
            "{name}: {} answers, {} from the RL closure, {} derived",
            rewritten.len(),
            reference.len(),
            got.reasoning().unwrap().report.derived
        );
        equal += (rewritten.len() == reference.len()) as usize;
        // the rows too, each as many times as the closure gives it
        let closure_rows = rows(&run(&fragment, &q, &closure));
        assert_eq!(rows(&got).len(), closure_rows.len(), "{name}: row counts");
        assert_eq!(rows(&got), closure_rows, "{name}: rows");
    }
    assert_eq!(equal, 14, "every query agrees with the closure");
}

/// Review findings of the rewriting: initial bindings, generated variables, repeated
/// atoms, property paths and EXPLAIN of a query that also derives.
#[test]
fn an_initial_binding_makes_its_variable_distinguished() {
    let s = store(A8);
    let o = QueryOptions {
        initial_bindings: vec![(
            "c".into(),
            oxrdf::NamedNode::new_unchecked(format!("{EX}none")).into(),
        )],
        ..ql(None)
    };
    assert!(!run(&s, "ASK { ex:ann ex:teaches ?c }", &o).boolean);
}

#[test]
fn a_generated_variable_does_not_take_the_name_of_a_query_variable() {
    let s = store(
        "ex:A a owl:Class . ex:B a owl:Class . ex:p a owl:ObjectProperty .
         ex:B rdfs:subClassOf ex:A . ex:a a ex:A . ex:s ex:p ex:b .",
    );
    let r = run(
        &s,
        "SELECT ?__ql1 { ?__ql1 a ex:A . ex:s ex:p ?o }",
        &ql(None),
    );
    assert_eq!(answers(&r), vec![format!("<{EX}a>")]);
}

#[test]
fn a_repeated_atom_still_takes_an_existential() {
    let s = store(A8);
    assert!(
        run(
            &s,
            "ASK { ex:ann ex:teaches ?c . ex:ann ex:teaches ?c }",
            &ql(None)
        )
        .boolean
    );
}

#[test]
fn a_property_path_warns_that_it_may_miss_unnamed_individuals() {
    let s = store(&format!("{A8} ex:advises a owl:ObjectProperty ."));
    let r = run(&s, "ASK { ex:ann (ex:teaches|ex:advises) ?c }", &ql(None));
    let warned = format!("{:?}", r.plan().warnings);
    assert!(warned.contains("property path"), "{warned}");
}

#[test]
fn explain_of_a_query_that_also_derives_shows_its_rewriting() {
    let s =
        store("ex:A a owl:Class . ex:B a owl:Class . ex:B rdfs:subClassOf ex:A . ex:a a ex:B .");
    let (_, plan) = sparkles_core::sparql::explain(
        s.snapshot(),
        "SELECT ?x { ?x a ex:A OPTIONAL { ?x ?p ?o } }",
        &ql(None),
    )
    .unwrap();
    let text = format!("{plan:?}");
    assert!(text.contains("OWL 2 QL rewriting"), "{text}");
}

/// A variable of the initial bindings unifies only with its own value.
#[test]
fn a_unification_keeps_the_value_of_an_initial_binding() {
    let s = store(
        "ex:p a owl:ObjectProperty . ex:q a owl:ObjectProperty . ex:q rdfs:subPropertyOf ex:p .
         ex:a ex:p ex:b .",
    );
    let bound = |v: &str| QueryOptions {
        initial_bindings: vec![(
            "c".into(),
            oxrdf::NamedNode::new_unchecked(format!("{EX}{v}")).into(),
        )],
        ..ql(None)
    };
    let q = "ASK { ex:a ex:p ?c . ex:a ex:p ex:b }";
    assert!(!run(&s, q, &bound("none")).boolean);
    assert!(run(&s, q, &bound("b")).boolean);
    let r = run(
        &s,
        "SELECT ?c { ex:a ex:p ?c . ex:a ex:p ex:b }",
        &bound("b"),
    );
    assert_eq!(answers(&r), vec![format!("<{EX}b>")]);
}

/// The queries the rewriting keeps are charged to the query's memory budget.
#[test]
fn the_rewriting_is_charged_to_the_memory_budget() {
    let mut onto = String::from("ex:A a owl:Class .\n");
    for i in 0..30 {
        onto.push_str(&format!("ex:C{i} a owl:Class ; rdfs:subClassOf ex:A .\n"));
    }
    let s = store(&onto);
    let o = QueryOptions {
        // past the terminology's charge, far below the rewriting's
        max_memory_bytes: Some(4 << 20),
        ..ql(Some(1_000_000))
    };
    let e = query(
        s.snapshot(),
        "SELECT * { ?a a ex:A . ?b a ex:A . ?c a ex:A . ?d a ex:A }",
        &o,
    )
    .err()
    .expect("the rewriting passes the memory budget");
    assert!(e.to_string().contains("the OWL 2 QL rewriting"), "{e}");
}

/// The rows of a result, sorted, each as many times as the result gives it.
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

/// The rows of `q` under `owl-rl` over a materialization of the store's own ontology.
fn materialized_rows(ttl: &str, q: &str) -> Vec<String> {
    let s = store(ttl);
    materialize(&s, &Profile::OwlRl, &ReasonOptions::default()).unwrap();
    let o = QueryOptions {
        prefixes: vec![("ex".into(), EX.into())],
        default_graph_extra: vec![INFERRED_GRAPH.into()],
        ..Default::default()
    };
    rows(&run(&s, q, &o))
}

/// A variable that the query does not project but that stored facts bind keeps its
/// values, so the rows are those of plain SPARQL and of the `owl-rl` modes.
#[test]
fn a_variable_bound_by_stored_facts_keeps_the_rows_of_plain_sparql() {
    let ttl = "ex:p a owl:ObjectProperty . ex:q a owl:ObjectProperty .
               ex:q rdfs:subPropertyOf ex:p . ex:a ex:p ex:b , ex:c .";
    let q = "SELECT ?s { ?s ex:p ?o }";
    let got = rows(&run(&store(ttl), q, &ql(None)));
    assert_eq!(got, vec![format!("<{EX}a>"), format!("<{EX}a>")]);
    assert_eq!(got, materialized_rows(ttl, q));
    let n = run(
        &store(ttl),
        "SELECT (COUNT(?s) AS ?n) { ?s ex:p ?o }",
        &ql(None),
    );
    assert_eq!(
        rows(&n),
        vec!["\"2\"^^<http://www.w3.org/2001/XMLSchema#integer>"]
    );
}

/// A pattern answered partly from the stored facts and partly by a rewritten branch gives
/// each solution once, as the `owl-rl` closure does, and an existential adds a row only
/// where no stored fact names the variable.
#[test]
fn stored_and_rewritten_answers_give_the_rows_of_the_closure() {
    let ttl = "ex:p a owl:ObjectProperty . ex:q a owl:ObjectProperty .
               ex:q rdfs:subPropertyOf ex:p .
               ex:a ex:p ex:b . ex:a ex:q ex:b , ex:c . ex:d ex:q ex:e .";
    for q in [
        "SELECT ?s { ?s ex:p ?o }",
        "SELECT ?s ?o { ?s ex:p ?o }",
        "SELECT ?s { ?s ex:p ?o . ?s ex:p ?o2 }",
    ] {
        let got = rows(&run(&store(ttl), q, &ql(None)));
        assert_eq!(got, materialized_rows(ttl, q), "{q}");
    }
    let ttl = format!("{A8} ex:bob a ex:Professor ; ex:teaches ex:c1 , ex:c2 .");
    let got = rows(&run(
        &store(&ttl),
        "SELECT ?x { ?x ex:teaches ?c }",
        &ql(None),
    ));
    assert_eq!(
        got,
        vec![
            format!("<{EX}ann>"),
            format!("<{EX}bob>"),
            format!("<{EX}bob>")
        ]
    );
}

/// A row left to an unnamed individual is dropped when a branch that names more of the
/// pattern's variables extends it, whether or not they share a variable.
#[test]
fn an_existential_row_gives_way_to_any_row_that_names_more() {
    let ttl = format!("{A8} ex:ann ex:teaches ex:c1 , ex:c2 .");
    let got = rows(&run(
        &store(&ttl),
        "SELECT (1 AS ?n) { ex:ann ex:teaches ?c }",
        &ql(None),
    ));
    assert_eq!(got.len(), 2, "{got:?}");
    let ttl = "ex:A a owl:Class . ex:B a owl:Class .
               ex:p a owl:ObjectProperty . ex:q a owl:ObjectProperty .
               ex:A rdfs:subClassOf [ a owl:Restriction ; owl:onProperty ex:p ;
                   owl:someValuesFrom owl:Thing ] .
               ex:B rdfs:subClassOf [ a owl:Restriction ; owl:onProperty ex:q ;
                   owl:someValuesFrom owl:Thing ] .
               ex:a a ex:A , ex:B ; ex:p ex:b , ex:c .";
    let got = rows(&run(
        &store(ttl),
        "SELECT ?s { ?s ex:p ?o . ?s ex:q ?v }",
        &ql(None),
    ));
    assert_eq!(got, vec![format!("<{EX}a>"), format!("<{EX}a>")]);
}

/// A wider branch's bindings are tests of a row's values inside NOT EXISTS, so a branch
/// that unification bound to other constants does not drop the row.
#[test]
fn a_bound_wider_branch_keeps_rows_with_other_values() {
    let ttl = "ex:A a owl:Class . ex:p a owl:ObjectProperty .
               ex:A rdfs:subClassOf [ a owl:Restriction ; owl:onProperty ex:p ;
                   owl:someValuesFrom owl:Thing ] .
               ex:a a ex:A . ex:c ex:p ex:d .";
    let got = rows(&run(
        &store(ttl),
        "SELECT ?s { ?s ex:p ?o . ex:c ex:p ex:d }",
        &ql(None),
    ));
    assert_eq!(got, vec![format!("<{EX}a>"), format!("<{EX}c>")]);
}

/// Copies of wider branches count against `budget.maxUnion`, past which the pattern
/// falls back to the `owl-rl` demand program with a warning.
#[test]
fn copies_of_wider_branches_count_against_the_bound() {
    let mut onto = String::new();
    let mut q = String::from("ASK { ");
    for i in 0..10 {
        onto.push_str(&format!(
            "ex:A{i} a owl:Class . ex:p{i} a owl:ObjectProperty .
             ex:A{i} rdfs:subClassOf [ a owl:Restriction ; owl:onProperty ex:p{i} ;
                 owl:someValuesFrom owl:Thing ] . ex:a a ex:A{i} .\n"
        ));
        q.push_str(&format!("ex:a ex:p{i} ?v{i} . "));
    }
    q.push('}');
    // the owl-rl fallback cannot use the existentials, as its warning says
    let r = run(&store(&onto), &q, &ql(Some(2048)));
    assert!(!r.boolean);
    let warned = format!("{:?}", r.plan().warnings);
    assert!(warned.contains("copies"), "{warned}");
}

/// The copies of wider branches are charged with the strings of their terms.
#[test]
fn copies_are_charged_with_their_strings() {
    let long = "x".repeat(64 * 1024);
    let mut onto = String::from(
        "ex:A a owl:Class . ex:p a owl:ObjectProperty .
         ex:A rdfs:subClassOf [ a owl:Restriction ; owl:onProperty ex:p ;
             owl:someValuesFrom owl:Thing ] .\n",
    );
    for i in 0..100 {
        onto.push_str(&format!(
            "ex:q{i} a owl:ObjectProperty ; rdfs:subPropertyOf ex:p .\n"
        ));
    }
    let o = QueryOptions {
        max_memory_bytes: Some(4 << 20),
        ..ql(None)
    };
    let q = format!("ASK {{ <{EX}{long}> ex:p ?o }}");
    let e = query(store(&onto).snapshot(), &q, &o)
        .err()
        .expect("the copies pass the memory budget");
    assert!(e.to_string().contains("the OWL 2 QL rewriting"), "{e}");
}
