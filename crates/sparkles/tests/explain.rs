//! `Dataset::reasoning().explain` (spec U10 §3.3).

use oxrdf::{NamedNode, Term};
use sparkles::Dataset;
use sparkles::access::{GraphAccess, GraphRule, Graphs};
use sparkles::explain::{Derivation, DerivationKind, Explained};
use sparkles::io::RdfFormat;
use sparkles::reasoning::explain::{ExplainRequest, ExplainSource};
use std::sync::Arc;

const RDF_TYPE: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#type";
#[cfg(feature = "reasoning")]
const HR: &str = "http://ex.org/hr";
const ONTO: &str = "http://ex.org/ontology";

fn ex(l: &str) -> Term {
    Term::NamedNode(NamedNode::new(format!("http://ex.org/{l}")).unwrap())
}

fn ty() -> Term {
    Term::NamedNode(NamedNode::new(RDF_TYPE).unwrap())
}

fn open() -> (tempfile::TempDir, Dataset) {
    let dir = tempfile::tempdir().unwrap();
    let ds = Dataset::open(dir.path().join("db")).unwrap();
    (dir, ds)
}

fn only(graphs: &[&str]) -> Option<Arc<GraphAccess>> {
    Some(Arc::new(GraphAccess::graphs(
        Graphs::Only(GraphRule::new(graphs.iter().copied(), &[])),
        Graphs::none(),
    )))
}

fn found(e: Explained) -> Vec<Derivation> {
    match e {
        Explained::Found { trees, .. } => trees,
        Explained::NotFound => panic!("no derivation"),
    }
}

fn rule(d: &Derivation) -> &str {
    &d.rule.as_ref().unwrap().name
}

#[cfg(feature = "reasoning")]
fn materialized() -> (tempfile::TempDir, Dataset) {
    use sparkles::reasoning::ReasonRequest;
    use sparkles::reasoning::reasoner::{GraphRef, Inputs};
    let (dir, ds) = open();
    ds.load_str(
        "@prefix ex: <http://ex.org/> . @prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
         ex:hr { ex:alice ex:manages ex:team7 . }
         ex:ontology { ex:Manager rdfs:subClassOf ex:Employee . ex:manages rdfs:domain ex:Manager . }",
        RdfFormat::TriG,
    )
    .unwrap();
    let req = ReasonRequest {
        inputs: Inputs {
            data_graphs: vec![GraphRef::Named(HR.into())],
            ontology_graphs: vec![GraphRef::Named(ONTO.into())],
            ..Default::default()
        },
        ..Default::default()
    };
    ds.reasoning().run(&req).unwrap();
    (dir, ds)
}

#[cfg(feature = "reasoning")]
#[test]
fn materialized_inferences_are_explained_from_their_input_graphs() {
    let (_dir, ds) = materialized();
    let rec = ds.reasoning().record().unwrap();
    let e = ds
        .reasoning()
        .explain(&ExplainRequest::new([ex("alice"), ty(), ex("Employee")]))
        .unwrap();
    assert_eq!(e.source, Some(ExplainSource::Materialized));
    assert_eq!(Some(e.commit), rec.commit);
    let t = &found(e.explained)[0];
    assert_eq!(rule(t), "rdfs9");
    let m = t
        .premises
        .iter()
        .find(|p| p.triple[2] == ex("Manager"))
        .unwrap();
    assert_eq!(rule(m), "rdfs2");
    let graphs: Vec<_> = m
        .premises
        .iter()
        .map(|p| p.graph.clone().unwrap())
        .collect();
    assert!(graphs.contains(&HR.to_string()) && graphs.contains(&ONTO.to_string()));
}

#[cfg(feature = "reasoning")]
#[test]
fn a_caller_who_cannot_read_the_ontology_gets_no_derivation() {
    let (_dir, ds) = materialized();
    let mut req = ExplainRequest::new([ex("alice"), ty(), ex("Employee")]);
    req.access = only(&[HR, "urn:x-sparkles:inferred"]);
    let e = ds.reasoning().explain(&req).unwrap();
    assert!(matches!(e.explained, Explained::NotFound));
    assert_eq!(e.source, None);
    // the same answer as for a triple that is not entailed
    let e = ds
        .reasoning()
        .explain(&ExplainRequest::new([ex("bob"), ty(), ex("Employee")]))
        .unwrap();
    assert!(matches!(e.explained, Explained::NotFound));
    // with both graphs the tree is found
    req.access = only(&[HR, ONTO, "urn:x-sparkles:inferred"]);
    found(ds.reasoning().explain(&req).unwrap().explained);
}

#[cfg(feature = "reasoning")]
#[test]
fn stale_inferences_are_explained_at_their_run() {
    let (_dir, ds) = materialized();
    let rec = ds.reasoning().record().unwrap();
    ds.update("DELETE DATA { GRAPH <http://ex.org/hr> { <http://ex.org/alice> <http://ex.org/manages> <http://ex.org/team7> } }")
        .unwrap();
    let e = ds
        .reasoning()
        .explain(&ExplainRequest::new([ex("alice"), ty(), ex("Employee")]))
        .unwrap();
    assert_eq!(Some(e.commit), rec.commit);
    assert!(!e.read_at_head);
    let t = &found(e.explained)[0];
    assert_eq!(rule(t), "rdfs9");
}

fn on_read() -> (tempfile::TempDir, Dataset) {
    let (dir, ds) = open();
    ds.load_str(
        "@prefix ex: <http://ex.org/> . @prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
         { ex:alice ex:manages ex:team7 . ex:alice ex:worksFor ex:acme . }
         ex:ontology {
           ex:Manager rdfs:subClassOf ex:Staff . ex:Staff rdfs:subClassOf ex:Employee .
           ex:manages rdfs:domain ex:Manager .
           ex:worksFor rdfs:subPropertyOf ex:affiliatedWith .
         }",
        RdfFormat::TriG,
    )
    .unwrap();
    ds.reasoning()
        .rdfs()
        .set(sparkles::reasoning::rdfs::NewSchema::Graph(ONTO.into()))
        .unwrap();
    (dir, ds)
}

#[test]
fn rdfs_on_read_is_explained_with_a_subclass_chain() {
    let (_dir, ds) = on_read();
    let e = ds
        .reasoning()
        .explain(&ExplainRequest::new([ex("alice"), ty(), ex("Employee")]))
        .unwrap();
    assert_eq!(e.source, Some(ExplainSource::RdfsOnRead));
    let t = &found(e.explained)[0];
    assert_eq!(rule(t), "rdfs9");
    assert_eq!(t.rule.as_ref().unwrap().profile, "rdfs-on-read");
    let m = &t.premises[0];
    assert_eq!(rule(m), "rdfs2");
    assert_eq!(m.premises[0].graph.as_deref(), Some("default"));
    assert_eq!(m.premises[1].graph.as_deref(), Some(ONTO));
    let sc = &t.premises[1];
    assert_eq!(rule(sc), "rdfs11");
    assert_eq!(sc.premises.len(), 2);
    assert!(
        sc.premises
            .iter()
            .all(|p| p.kind == DerivationKind::Asserted)
    );

    let t = &found(
        ds.reasoning()
            .explain(&ExplainRequest::new([
                ex("alice"),
                ex("affiliatedWith"),
                ex("acme"),
            ]))
            .unwrap()
            .explained,
    )[0];
    assert_eq!(rule(t), "rdfs7");
}

#[test]
fn rdfs_on_read_needs_a_readable_schema() {
    let (_dir, ds) = on_read();
    let mut req = ExplainRequest::new([ex("alice"), ty(), ex("Employee")]);
    req.access = only(&["default"]);
    assert!(matches!(
        ds.reasoning().explain(&req).unwrap().explained,
        Explained::NotFound
    ));
    req.access = only(&["default", ONTO]);
    found(ds.reasoning().explain(&req).unwrap().explained);
}

#[test]
fn rdfs_on_read_stops_at_the_deadline() {
    let (_dir, ds) = on_read();
    let mut req = ExplainRequest::new([ex("alice"), ty(), ex("Manager")]);
    req.timeout = Some(std::time::Duration::ZERO);
    let e = ds.reasoning().explain(&req).unwrap_err();
    assert!(
        matches!(&e, sparkles::Error::Component(c) if c.code == sparkles::reasoning::explain::REASONING_BUDGET),
        "{e}"
    );
    // a timeout too large for the clock means none
    req.timeout = Some(std::time::Duration::MAX);
    found(ds.reasoning().explain(&req).unwrap().explained);
}
