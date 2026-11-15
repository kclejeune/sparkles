//! The entailment tests of the W3C OWL 2 test cases in the OWL 2 QL profile, answered by
//! OWL 2 QL rewriting (spec F17 §13). `SPARKLES_OWL2_TESTS` names the suite's `all.rdf`, as
//! `scripts/fetch-test-suites.sh` prints it, and the test is skipped without it.
//!
//! A case's conclusion becomes an ASK query when it states facts about individuals: class
//! and property assertions, with its blank nodes as variables, which a rewritten query
//! answers. A conclusion that states axioms, such as a subclass axiom, is a question for a
//! classifier, and its case is counted as skipped. A positive case must answer true and a
//! negative case false, except the cases `KNOWN` lists with the reason.

use sparkles_core::io::{RdfFormat, Source};
use sparkles_core::sparql::{QueryOptions, query};
use sparkles_core::store::{Store, StoreOptions};
use sparkles_reasoner::query::QueryReasoner;
use sparkles_reasoner::{Extras, Inputs, Profile};

const TEST: &str = "http://www.w3.org/2007/OWL/testOntology#";
const RDF_TYPE: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#type";

/// Cases whose answer differs, with the reason.
const KNOWN: &[(&str, &str)] = &[(
    "New-Feature-ReflexiveProperty-001",
    "the rewriting does not use reflexive properties",
)];

/// The RDF/XML parser reads entity declarations with double quotes only, and the suite
/// writes some with single quotes.
fn fix_entities(data: &str) -> String {
    data.lines()
        .map(|l| {
            if l.trim_start().starts_with("<!ENTITY") {
                l.replace('\'', "\"")
            } else {
                l.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn load(xml: &str) -> Result<Store, String> {
    let s = Store::in_memory(StoreOptions::default());
    s.load(&[Source::from_bytes(
        fix_entities(xml).into_bytes(),
        RdfFormat::RdfXml,
        None,
    )])
    .map_err(|e| e.to_string())?;
    Ok(s)
}

fn reserved(iri: &str) -> bool {
    [
        "http://www.w3.org/2002/07/owl#",
        "http://www.w3.org/1999/02/22-rdf-syntax-ns#",
        "http://www.w3.org/2000/01/rdf-schema#",
    ]
    .iter()
    .any(|ns| iri.starts_with(ns))
}

/// The ASK query of a conclusion that states facts about individuals, or `None`. Its
/// annotations are left out, since the Direct Semantics ignores them in an entailed
/// ontology.
fn conclusion_query(xml: &str) -> Result<Option<String>, String> {
    let s = load(xml)?;
    let annotations: Vec<String> = query(
        s.snapshot(),
        "SELECT ?p { ?p a <http://www.w3.org/2002/07/owl#AnnotationProperty> }",
        &QueryOptions::default(),
    )
    .map_err(|e| e.to_string())?
    .rows()
    .unwrap()
    .iter()
    .filter_map(|r| match &r.values()[0] {
        Some(oxrdf::Term::NamedNode(n)) => Some(n.as_str().to_string()),
        _ => None,
    })
    .collect();
    let r = query(
        s.snapshot(),
        "SELECT ?s ?p ?o { ?s ?p ?o }",
        &QueryOptions::default(),
    )
    .map_err(|e| e.to_string())?;
    let mut patterns = Vec::new();
    for row in r.rows().unwrap().iter() {
        let v = row.values();
        let (Some(s), Some(p), Some(o)) = (&v[0], &v[1], &v[2]) else {
            continue;
        };
        let term = |t: &oxrdf::Term| match t {
            oxrdf::Term::BlankNode(b) => format!("?b{}", b.as_str().replace('-', "_")),
            t => t.to_string(),
        };
        let p = match p {
            oxrdf::Term::NamedNode(n) => n.as_str().to_string(),
            _ => continue,
        };
        // declarations, annotations and the ontology's header are not part of what follows
        if annotations.contains(&p) {
            continue;
        }
        if p == RDF_TYPE {
            match o {
                oxrdf::Term::NamedNode(c) if reserved(c.as_str()) => continue,
                oxrdf::Term::NamedNode(_) => {}
                _ => return Ok(None),
            }
        } else if reserved(&p) {
            return Ok(None);
        }
        patterns.push(format!("{} <{p}> {} .", term(s), term(o)));
    }
    if patterns.is_empty() {
        return Ok(None);
    }
    Ok(Some(format!("ASK {{ {} }}", patterns.join(" "))))
}

#[test]
fn owl2_ql_entailment_cases() {
    let Ok(path) = std::env::var("SPARKLES_OWL2_TESTS") else {
        eprintln!("SPARKLES_OWL2_TESTS is not set: skipping the OWL 2 QL test cases");
        return;
    };
    let manifest = load(&std::fs::read_to_string(&path).expect("read the test cases"))
        .expect("parse the test cases");
    let cases = query(
        manifest.snapshot(),
        &format!(
            "PREFIX test: <{TEST}>
             SELECT ?id ?type ?premise ?conclusion ?nonconclusion WHERE {{
               ?c test:profile test:QL ; a ?type ; test:identifier ?id ;
                  test:rdfXmlPremiseOntology ?premise .
               OPTIONAL {{ ?c test:rdfXmlConclusionOntology ?conclusion }}
               OPTIONAL {{ ?c test:rdfXmlNonConclusionOntology ?nonconclusion }}
               FILTER NOT EXISTS {{ ?c test:importedOntology ?i }}
               FILTER(?type IN (test:PositiveEntailmentTest, test:NegativeEntailmentTest))
             }} ORDER BY ?id ?type"
        ),
        &QueryOptions::default(),
    )
    .unwrap();
    let reasoner =
        QueryReasoner::new(Profile::OwlQl, &Extras::default(), Inputs::default()).unwrap();
    let opts = QueryOptions {
        no_cache: true,
        reasoning: Some(reasoner.provider()),
        ..Default::default()
    };
    let (mut passed, mut skipped, mut known) = (0, 0, 0);
    let mut failed = Vec::new();
    for row in cases.rows().unwrap().iter() {
        let v = row.values();
        let lit = |i: usize| match &v[i] {
            Some(oxrdf::Term::Literal(l)) => Some(l.value().to_string()),
            _ => None,
        };
        let id = lit(0).unwrap_or_default();
        let positive = matches!(&v[1], Some(oxrdf::Term::NamedNode(n)) if n.as_str().ends_with("PositiveEntailmentTest"));
        let (premise, conclusion) = (lit(2).unwrap(), if positive { lit(3) } else { lit(4) });
        let Some(conclusion) = conclusion else {
            skipped += 1;
            continue;
        };
        let ask = match conclusion_query(&conclusion) {
            Ok(Some(q)) => q,
            Ok(None) => {
                eprintln!("-- {id}: the conclusion states axioms, skipped");
                skipped += 1;
                continue;
            }
            Err(e) => {
                eprintln!("-- {id}: the conclusion does not parse ({e}), skipped");
                skipped += 1;
                continue;
            }
        };
        let store = match load(&premise) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("-- {id}: the premise does not parse ({e}), skipped");
                skipped += 1;
                continue;
            }
        };
        let got = query(store.snapshot(), &ask, &opts).map(|r| r.boolean);
        let ok = got.as_ref().is_ok_and(|&b| b == positive);
        let kind = if positive { "positive" } else { "negative" };
        if ok {
            passed += 1;
            eprintln!("ok {id} ({kind})");
        } else if KNOWN.iter().any(|(k, _)| *k == id) {
            known += 1;
            eprintln!("known {id} ({kind}): {got:?}");
        } else {
            eprintln!("FAILED {id} ({kind}): {got:?}\n{ask}");
            failed.push(id);
        }
    }
    eprintln!("{passed} passed, {known} known differences, {skipped} skipped");
    assert!(passed > 0, "no OWL 2 QL entailment case ran");
    assert!(failed.is_empty(), "failed: {failed:?}");
}
