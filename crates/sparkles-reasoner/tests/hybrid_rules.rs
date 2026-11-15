//! Jena's hybrid rules under `@hybrid jena` (spec F17 §5.2): a forward rule's nested
//! backward rule becomes a query-time rule, and the materialization leaves it out. The
//! answers are checked against Apache Jena 6.2.0's HYBRID mode on the same rules and data,
//! whose output `hybrid/JenaHybrid.java` wrote to `hybrid/jena-*.nt`.

use sparkles_core::io::{RdfFormat, Source};
use sparkles_core::sparql::{QueryOptions, query};
use sparkles_core::store::{Store, StoreOptions};
use sparkles_reasoner::query::QueryReasoner;
use sparkles_reasoner::{Extras, INFERRED_GRAPH, Inputs, Profile, ReasonOptions, materialize};

const RULES: &str = include_str!("hybrid/hybrid.rules");
const DATA: &str = include_str!("hybrid/data.nt");
const JENA_DEDUCTIONS: &str = include_str!("hybrid/jena-deductions.nt");
const JENA_ANSWERS: &str = include_str!("hybrid/jena-answers.nt");
const PREDICATES: [&str; 5] = [
    "http://ex.org/knows",
    "http://ex.org/hasParent",
    "http://ex.org/hasChild",
    "http://ex.org/ancestor",
    "http://www.w3.org/1999/02/22-rdf-syntax-ns#type",
];

fn store() -> Store {
    let s = Store::in_memory(StoreOptions::default());
    s.load(&[Source::from_bytes(
        DATA.as_bytes().to_vec(),
        RdfFormat::NTriples,
        None,
    )])
    .unwrap();
    s
}

fn hybrid() -> Profile {
    Profile::Rules(format!("@hybrid jena .\n{RULES}"))
}

fn lines(text: &str) -> Vec<String> {
    let mut v: Vec<String> = text
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| l.trim().to_string())
        .collect();
    v.sort();
    v
}

fn triples(s: &Store, q: &str, o: &QueryOptions, p: Option<&str>) -> Vec<String> {
    let r = query(s.snapshot(), q, o).unwrap_or_else(|e| panic!("{q}: {e}"));
    let mut v: Vec<String> = r
        .rows()
        .unwrap()
        .iter()
        .map(|row| {
            let t: Vec<String> = row
                .values()
                .iter()
                .map(|t| t.as_ref().unwrap().to_string())
                .collect();
            match p {
                Some(p) => format!("{} <{p}> {} .", t[0], t[1]),
                None => format!("{} {} {} .", t[0], t[1], t[2]),
            }
        })
        .collect();
    v.sort();
    v
}

fn materialized(profile: &Profile) -> Vec<String> {
    let s = store();
    materialize(&s, profile, &ReasonOptions::default()).unwrap();
    triples(
        &s,
        &format!("SELECT ?s ?p ?o {{ GRAPH <{INFERRED_GRAPH}> {{ ?s ?p ?o }} }}"),
        &QueryOptions::default(),
        None,
    )
}

#[test]
fn the_materialization_holds_jenas_forward_deductions() {
    assert_eq!(materialized(&hybrid()), lines(JENA_DEDUCTIONS));
}

#[test]
fn query_time_answers_are_jenas_hybrid_answers() {
    let s = store();
    let r = QueryReasoner::new(hybrid(), &Extras::default(), Inputs::default()).unwrap();
    let o = QueryOptions {
        no_cache: true,
        reasoning: Some(r.provider()),
        ..Default::default()
    };
    let mut got = Vec::new();
    for p in PREDICATES {
        got.extend(triples(
            &s,
            &format!("SELECT ?s ?o {{ ?s <{p}> ?o }}"),
            &o,
            Some(p),
        ));
    }
    got.sort();
    assert_eq!(got, lines(JENA_ANSWERS));
}

#[test]
fn without_the_directive_the_nested_rules_are_materialized() {
    let flat = materialized(&Profile::Rules(RULES.into()));
    assert!(
        flat.contains(&"<http://ex.org/b> <http://ex.org/knows> <http://ex.org/a> .".to_string()),
        "{flat:?}"
    );
}

#[test]
fn a_split_that_a_query_time_rule_cannot_state_is_refused() {
    for (rule, why) in [
        (
            "[f: (?a <urn:p> ?b) -> [(?a <urn:q> f(?b)) <- (?b <urn:r> ?a)]]",
            "functor",
        ),
        (
            "[t: (?a <urn:p> ?b) makeTemp(?t) -> [(?a <urn:q> ?t) <- (?b <urn:r> ?a)]]",
            "makeTemp",
        ),
    ] {
        let e = Profile::Rules(format!("@hybrid jena .\n{rule}"))
            .rules()
            .unwrap_err();
        assert!(
            e.message.contains("@hybrid jena") && e.message.contains(why),
            "{e}"
        );
    }
    let e = Profile::Rules("@hybrid other .".into())
        .rules()
        .unwrap_err();
    assert!(e.message.contains("@hybrid"), "{e}");
}

#[test]
fn rules_the_split_would_evaluate_unlike_jena_are_refused() {
    for (rule, why) in [
        (
            "[r: (<urn:a> <urn:t> <urn:b>) -> [(<urn:a> <urn:q> <urn:b>) <- noValue(<urn:a>, <urn:p>, <urn:b>)]]",
            "noValue",
        ),
        (
            "[o: (?a <urn:p> ?b) -> [m: (?a <urn:r> ?b) -> [(?a <urn:q> ?b) <- (?a <urn:s> ?b)]]]",
            "nested inside a nested forward rule",
        ),
        (
            "[r: (<urn:a> <urn:t> <urn:b>) -> [(?x <urn:q> <urn:b>) <- (<urn:a> <urn:s> <urn:b>)]]",
            "?x",
        ),
        (
            "[r: (?a <urn:p> ?b) -> [(?a <urn:q> ?t) <- makeInstance(?a, <urn:c>, ?t)]]",
            "makeInstance",
        ),
        (
            "[r: (?a <urn:p> ?b) -> drop(0), [(?a <urn:q> ?b) <- (?b <urn:r> ?a)]]",
            "drop",
        ),
    ] {
        let e = Profile::Rules(format!("@hybrid jena .\n{rule}"))
            .rules()
            .unwrap_err();
        assert!(
            e.message.contains("@hybrid jena") && e.message.contains(why),
            "{e}"
        );
    }
}

#[test]
fn a_head_variable_that_only_a_test_names_is_refused() {
    let e = Profile::Rules(
        "@hybrid jena .\n[r: (<urn:a> <urn:t> <urn:b>) -> [(<urn:a> <urn:q> ?x) <- isLiteral(?x)]]"
            .into(),
    )
    .rules()
    .unwrap_err();
    assert!(e.message.contains("?x"), "{e}");
    // an output of a builtin binds its variable
    Profile::Rules(
        "@hybrid jena .\n[r: (?a <urn:n> ?n) -> [(?a <urn:m> ?m) <- sum(?n, 1, ?m)]]".into(),
    )
    .rules()
    .unwrap();
}
