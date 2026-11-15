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
    }
    assert_eq!(equal, 14, "every query agrees with the closure");
}
