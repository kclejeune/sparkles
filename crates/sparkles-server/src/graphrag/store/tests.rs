//! Reports as triples (C22 §4).

use super::*;
use crate::graphrag::report::{EntityInfo, Fact, check, render_leaf};
use serde_json::json;
use sparkles::sparql::{self, QueryOptions};
use sparkles::store::{Store, StoreOptions};
use std::collections::{BTreeSet, HashMap};

const G: &str = "urn:x-sparkles:graphrag:reports";
const C: &str = "urn:x-sparkles:graphrag:ds:0:c1";

fn fact(s: &str, p: &str, o: &str) -> Fact {
    Fact {
        s: format!("<http://ex/{s}>"),
        p: format!("<http://ex/{p}>"),
        o: o.to_string(),
        reifier: None,
    }
}

fn written() -> (Checked, Rendered) {
    let members: BTreeSet<String> = ["<http://ex/ana>", "<http://ex/team>"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    let mut f2 = fact("team", "owns", "<http://ex/ledger>");
    f2.reifier = Some("<http://ex/rf1>".into());
    let facts = vec![fact("ana", "memberOf", "<http://ex/team>"), f2];
    let input = render_leaf(&members, &facts, &HashMap::<String, EntityInfo>::new());
    let out = json!({
        "title": "Team",
        "summary": "Ana's team owns the ledger.",
        "rating": 6,
        "findings": [
            {"text": "Ana is in the team.", "entities": [1, 2], "cites": [1]},
            {"text": "The team owns the ledger.", "entities": [2], "cites": [2]},
            {"text": "Made up.", "entities": [2], "cites": [9]}
        ]
    });
    let checked = check(&out, &input, &|_| None).unwrap();
    (checked, input)
}

fn report(body: Body) -> NewReport {
    NewReport {
        iri: "urn:uuid:00000000-0000-0000-0000-000000000001".into(),
        community: C.into(),
        status: Status::Unreviewed,
        source_commit: 3,
        footprint: "urn:x-sparkles:graphrag:footprint:ab".into(),
        language: "en".into(),
        activity: Some(Activity {
            model: "p/m".into(),
            input_tokens: 100,
            output_tokens: 20,
            started_at: "2026-10-23T00:00:00Z".into(),
        }),
        revision_of: None,
        body,
    }
}

fn load(quads: &[Quad]) -> Store {
    let s = Store::in_memory(StoreOptions::default());
    let text: String = quads.iter().map(|q| format!("{q} .\n")).collect();
    s.load(&[sparkles::io::Source::from_bytes(
        text.into_bytes(),
        sparkles::io::RdfFormat::NQuads,
        None,
    )])
    .unwrap();
    s
}

fn rows(s: &Store, q: &str) -> Vec<Vec<String>> {
    let mut v: Vec<Vec<String>> = sparql::query(s.snapshot(), q, &QueryOptions::default())
        .unwrap()
        .rows()
        .into_iter()
        .map(|r| {
            r.into_iter()
                .map(|t| t.map(|t| t.to_string()).unwrap_or_default())
                .collect()
        })
        .collect();
    v.sort();
    v
}

#[test]
fn a_report_reads_back_with_citations_as_triple_terms() {
    let (checked, input) = written();
    assert_eq!(checked.findings.len(), 2, "{checked:?}");
    let mut quads = community_quads(
        &CommunityRecord {
            iri: C.into(),
            level: 0,
            parent: None,
            members: vec!["<http://ex/ana>".into(), "<http://ex/team>".into()],
            fact_count: 2,
        },
        G,
    );
    quads.extend(report_quads(&report(Body::Written { checked, input }), G));
    let s = load(&quads);
    let r = rows(
        &s,
        &format!(
            "SELECT ?title ?status ?n WHERE {{ GRAPH <{G}> {{ <{C}> <{}> ?r . ?r <{RDFS_LABEL}> ?title ; <{}> ?status ; <{}> ?n }} }}",
            gr("report"),
            gr("status"),
            gr("droppedCitations")
        ),
    );
    assert_eq!(
        r,
        vec![vec![
            "\"Team\"@en".to_string(),
            "\"unreviewed\"".into(),
            "\"1\"^^<http://www.w3.org/2001/XMLSchema#integer>".into()
        ]]
    );
    // the second finding cites the fact as a triple term and its reifier
    let cites = rows(
        &s,
        &format!(
            "SELECT ?c WHERE {{ GRAPH <{G}> {{ ?f <{}> 2 ; <{}> ?c }} }}",
            gr("order"),
            gr("cites")
        ),
    );
    assert_eq!(
        cites,
        vec![
            vec!["<<( <http://ex/team> <http://ex/owns> <http://ex/ledger> )>>".to_string()],
            vec!["<http://ex/rf1>".to_string()],
        ]
    );
    let about = rows(
        &s,
        &format!(
            "SELECT ?e WHERE {{ GRAPH <{G}> {{ ?f <{}> 1 ; <{}> ?e }} }}",
            gr("order"),
            gr("about")
        ),
    );
    assert_eq!(about.len(), 2);
}

#[test]
fn deletes_remove_a_report_and_unlinked_footprints() {
    let (checked, input) = written();
    let r = report(Body::Written { checked, input });
    let mut quads = report_quads(&r, G);
    let fp = "urn:x-sparkles:graphrag:footprint:ab";
    for (p, o) in [("covers", G), ("footprint", "x")] {
        let o: Term = if p == "covers" {
            NamedNode::new_unchecked(o).into()
        } else {
            Literal::new_simple_literal(o).into()
        };
        quads.push(Quad::new(
            NamedNode::new_unchecked(fp),
            NamedNode::new_unchecked(gr(p)),
            o,
            GraphName::NamedNode(NamedNode::new_unchecked(DISCLOSURE_GRAPH)),
        ));
    }
    let s = load(&quads);
    // a linked footprint stays
    sparql::update::update(&s, &delete_unlinked_footprints(G), &QueryOptions::default()).unwrap();
    assert_eq!(
        rows(
            &s,
            &format!("SELECT ?p WHERE {{ GRAPH <{DISCLOSURE_GRAPH}> {{ <{fp}> ?p ?o }} }}")
        )
        .len(),
        2
    );
    let del = delete_subjects(G, std::slice::from_ref(&r.iri)).unwrap();
    sparql::update::update(&s, &del, &QueryOptions::default()).unwrap();
    // the community's link remains, the report, findings and activity are gone
    let left = rows(
        &s,
        &format!("SELECT ?s WHERE {{ GRAPH <{G}> {{ ?s ?p ?o }} }}"),
    );
    assert_eq!(left, vec![vec![format!("<{C}>")]]);
    sparql::update::update(&s, &delete_unlinked_footprints(G), &QueryOptions::default()).unwrap();
    assert!(
        rows(
            &s,
            &format!("SELECT ?p WHERE {{ GRAPH <{DISCLOSURE_GRAPH}> {{ ?s ?p ?o }} }}")
        )
        .is_empty()
    );
}

#[test]
fn a_failed_report_keeps_its_reason() {
    let r = report(Body::Failed("every finding was dropped".into()));
    let s = load(&report_quads(&r, G));
    let got = rows(
        &s,
        &format!(
            "SELECT ?w WHERE {{ GRAPH <{G}> {{ ?r <{}> ?w }} }}",
            gr("failure")
        ),
    );
    assert_eq!(got, vec![vec!["\"every finding was dropped\"".to_string()]]);
}

#[test]
fn statuses_round_trip() {
    for s in Status::ALL {
        assert_eq!(Status::parse(s.as_str()), Some(s));
    }
    assert_eq!(Status::parse("pending"), None);
}
