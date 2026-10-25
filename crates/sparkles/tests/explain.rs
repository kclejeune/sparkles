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
        Explained::NotFound | Explained::Partial { .. } => panic!("no complete derivation"),
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
    let mut req = ExplainRequest::new([ex("alice"), ty(), ex("Employee")]);
    req.source = Some(ExplainSource::Materialized);
    let e = ds.reasoning().explain(&req).unwrap();
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

/// A dataset with RDFS on read over `schema`, a TriG body for graph ex:ontology.
fn on_read_with(data: &str, schema: &str) -> (tempfile::TempDir, Dataset) {
    let (dir, ds) = open();
    ds.load_str(
        &format!(
            "@prefix ex: <http://ex.org/> . @prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
             @prefix rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#> .
             {{ {data} }} ex:ontology {{ {schema} }}"
        ),
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
fn rdfs_on_read_is_held_to_the_memory_limit() {
    let big = "x".repeat(1_000_000);
    let (_dir, ds) = on_read_with(&format!("ex:a ex:p \"{big}\" ."), "ex:p rdfs:domain ex:C .");
    let mut req = ExplainRequest::new([ex("a"), ty(), ex("C")]);
    found(ds.reasoning().explain(&req).unwrap().explained);
    req.max_memory = Some(65_536);
    let e = ds.reasoning().explain(&req).map(|_| ()).unwrap_err();
    assert!(
        matches!(&e, sparkles::Error::Component(c) if c.code == sparkles::reasoning::explain::REASONING_BUDGET),
        "{e}"
    );
}

#[test]
fn rdfs_on_read_explains_a_superproperty_of_a_derived_type() {
    let (_dir, ds) = on_read_with(
        "ex:a ex:p ex:b .",
        "ex:p rdfs:domain ex:C . rdf:type rdfs:subPropertyOf ex:kind .",
    );
    let t = &found(
        ds.reasoning()
            .explain(&ExplainRequest::new([ex("a"), ex("kind"), ex("C")]))
            .unwrap()
            .explained,
    )[0];
    assert_eq!(rule(t), "rdfs7");
    assert_eq!(rule(&t.premises[0]), "rdfs2");
}

#[test]
fn rdfs_on_read_explains_a_schema_chain_on_its_own() {
    // what Expand asks for when a subclass chain is cut at the depth limit
    let (_dir, ds) = on_read();
    let sc =
        Term::NamedNode(NamedNode::new("http://www.w3.org/2000/01/rdf-schema#subClassOf").unwrap());
    let t = &found(
        ds.reasoning()
            .explain(&ExplainRequest::new([ex("Manager"), sc, ex("Employee")]))
            .unwrap()
            .explained,
    )[0];
    assert_eq!(rule(t), "rdfs11");
    assert_eq!(t.premises.len(), 2);
}

#[test]
fn rdfs_on_read_counts_schema_neighbours_against_its_reads() {
    // a class with 60,000 direct superclasses: visiting them makes 60,001 reads, under the
    // limit of 100,000, so only the reads of their 60,000 rows as neighbours exceed it
    let mut schema = String::new();
    for i in 0..60_000 {
        schema.push_str(&format!("ex:A rdfs:subClassOf ex:S{i} . "));
    }
    schema.push_str("ex:p rdfs:domain ex:A . ex:Z rdfs:subClassOf ex:Top .");
    let (_dir, ds) = on_read_with("ex:a ex:p ex:b .", &schema);
    let sc =
        Term::NamedNode(NamedNode::new("http://www.w3.org/2000/01/rdf-schema#subClassOf").unwrap());
    // A reaches no Top, and the search reads every superclass of A on the way
    let e = ds
        .reasoning()
        .explain(&ExplainRequest::new([ex("A"), sc, ex("Top")]))
        .map(|_| ())
        .unwrap_err();
    assert!(
        matches!(&e, sparkles::Error::Component(c) if c.code == sparkles::reasoning::explain::REASONING_BUDGET),
        "{e}"
    );
}

#[test]
fn rdfs_on_read_holds_an_asserted_answer_to_the_memory_limit() {
    let big = "x".repeat(1_000_000);
    let (_dir, ds) = on_read_with(&format!("ex:a ex:p \"{big}\" ."), "ex:p rdfs:domain ex:C .");
    let mut req = ExplainRequest::new([
        ex("a"),
        ex("p"),
        Term::Literal(oxrdf::Literal::new_simple_literal(big)),
    ]);
    found(ds.reasoning().explain(&req).unwrap().explained);
    req.max_memory = Some(65_536);
    let e = ds.reasoning().explain(&req).map(|_| ()).unwrap_err();
    assert!(
        matches!(&e, sparkles::Error::Component(c) if c.code == sparkles::reasoning::explain::REASONING_BUDGET),
        "{e}"
    );
}

#[test]
fn an_asserted_triple_is_explained_without_the_schema() {
    let (_dir, ds) = on_read();
    let mut req = ExplainRequest::new([ex("alice"), ex("manages"), ex("team7")]);
    req.access = only(&["default"]);
    let t = &found(ds.reasoning().explain(&req).unwrap().explained)[0];
    assert_eq!(t.kind, DerivationKind::Asserted);
}

/// A dataset with RDFS on read over a schema given as triples, and `data` in its default
/// graph.
fn on_read_document(data: &str, schema: &[[&str; 3]]) -> (tempfile::TempDir, Dataset) {
    let (dir, ds) = open();
    if !data.is_empty() {
        ds.load_str(
            &format!("@prefix ex: <http://ex.org/> . {data}"),
            RdfFormat::Turtle,
        )
        .unwrap();
    }
    let iri = |s: &str| NamedNode::new(s).unwrap();
    let triples = schema
        .iter()
        .map(|[s, p, o]| oxrdf::Triple::new(iri(s), iri(p), iri(o)))
        .collect();
    ds.reasoning()
        .rdfs()
        .set(sparkles::reasoning::rdfs::NewSchema::Triples(triples))
        .unwrap();
    (dir, ds)
}

const SC: &str = "http://www.w3.org/2000/01/rdf-schema#subClassOf";
const SP: &str = "http://www.w3.org/2000/01/rdf-schema#subPropertyOf";

#[test]
fn a_document_schema_explains_only_the_facts_of_its_closure() {
    let iri = |s: &str| Term::NamedNode(NamedNode::new(s).unwrap());
    for pred in [SC, SP] {
        for data in ["", "ex:x ex:y ex:z ."] {
            let (_dir, ds) = on_read_document(
                data,
                &[
                    ["http://ex.org/A", pred, "http://ex.org/B"],
                    ["http://ex.org/B", pred, "http://ex.org/C"],
                ],
            );
            let ask = |a: &str, b: &str| {
                ds.reasoning()
                    .explain(&ExplainRequest::new([ex(a), iri(pred), ex(b)]))
                    .unwrap()
                    .explained
            };
            for (a, b) in [
                ("unrelated", "absent"),
                ("A", "absent"),
                ("C", "A"),
                ("B", "A"),
            ] {
                assert!(
                    matches!(ask(a, b), Explained::NotFound),
                    "{pred} {a} {b} on {data:?}"
                );
            }
            // a fact of the closure is a leaf of the document
            for (a, b) in [("A", "B"), ("A", "C")] {
                let t = &found(ask(a, b))[0];
                assert_eq!(t.kind, DerivationKind::Asserted, "{pred} {a} {b}");
                assert_eq!(t.graph, None);
            }
        }
    }
}

#[cfg(feature = "reasoning")]
#[test]
fn auto_explanations_derive_current_facts_when_materialization_is_stale() {
    let (_dir, ds) = materialized();
    ds.update("DELETE DATA { GRAPH <http://ex.org/hr> { <http://ex.org/alice> <http://ex.org/manages> <http://ex.org/team7> } }").unwrap();
    let e = ds
        .reasoning()
        .explain(&ExplainRequest::new([ex("alice"), ty(), ex("Employee")]))
        .unwrap();
    assert_eq!(e.source, Some(ExplainSource::Demand));
    assert_eq!(e.commit, ds.snapshot().commit);
    assert!(matches!(e.explained, Explained::NotFound));
}

#[cfg(feature = "reasoning")]
#[test]
fn demand_and_why_not_work_without_materialization_and_enforce_visibility() {
    use sparkles::reasoning::explain::{ExplainMode, explanation_json};
    let ds = Dataset::memory();
    ds.load_str("@prefix ex: <http://ex.org/> . @prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> . ex:hr { ex:alice a ex:Manager . } ex:ontology { ex:Manager rdfs:subClassOf ex:Employee . }",RdfFormat::TriG).unwrap();
    let mut req = ExplainRequest::new([ex("alice"), ty(), ex("Employee")]);
    req.profile = Some("rdfs".into());
    // The default input is the default graph, so use a fresh materialized dataset's
    // recorded graph selection for the visible-view case below.
    let e = ds.reasoning().explain(&req).unwrap();
    assert!(matches!(e.explained, Explained::NotFound));
    let ds = Dataset::memory();
    ds.load_str("@prefix ex: <http://ex.org/> . @prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> . ex:alice a ex:Manager . ex:Manager rdfs:subClassOf ex:Employee .",RdfFormat::Turtle).unwrap();
    let e = ds.reasoning().explain(&req).unwrap();
    assert_eq!(explanation_json(&e)["entailed"], true);
    req.access = only(&[ONTO]);
    let e = ds.reasoning().explain(&req).unwrap();
    assert_eq!(explanation_json(&e)["entailed"], false);
    req.access = None;
    req.mode = ExplainMode::WhyNot;
    req.triple[2] = ex("Director");
    req.max_depth = 2;
    req.max_hypotheses = 3;
    let e = ds.reasoning().explain(&req).unwrap();
    let j = explanation_json(&e);
    assert_eq!(j["mode"], "whyNot");
    assert_eq!(j["entailed"], false);
    assert!(!j["hypotheses"].as_array().unwrap().is_empty());
}

#[cfg(feature = "reasoning")]
#[test]
fn rdfs_on_read_grafts_a_materialized_premise_to_visible_assertions() {
    use sparkles::reasoning::rdfs::NewSchema;
    let (_dir, ds) = materialized();
    ds.reasoning()
        .rdfs()
        .set(NewSchema::Triples(vec![oxrdf::Triple::new(
            NamedNode::new("http://ex.org/Employee").unwrap(),
            oxrdf::vocab::rdfs::SUB_CLASS_OF.into_owned(),
            NamedNode::new("http://ex.org/Person").unwrap(),
        )]))
        .unwrap();
    let e = ds
        .reasoning()
        .explain(&ExplainRequest::new([ex("alice"), ty(), ex("Person")]))
        .unwrap();
    assert_eq!(e.source, Some(ExplainSource::RdfsOnRead));
    let trees = found(e.explained);
    fn leaves(d: &Derivation) {
        assert_ne!(d.graph.as_deref(), Some("urn:x-sparkles:inferred"));
        for p in &d.premises {
            leaves(p);
        }
    }
    for t in trees {
        leaves(&t);
        assert!(t.depth >= 3);
    }
}

#[cfg(feature = "reasoning")]
#[test]
fn explicit_materialized_with_rdfs_can_explain_a_stale_inferred_premise() {
    use sparkles::reasoning::rdfs::NewSchema;
    let (_dir, ds) = materialized();
    ds.reasoning()
        .rdfs()
        .set(NewSchema::Triples(vec![oxrdf::Triple::new(
            NamedNode::new("http://ex.org/Employee").unwrap(),
            oxrdf::vocab::rdfs::SUB_CLASS_OF.into_owned(),
            NamedNode::new("http://ex.org/Person").unwrap(),
        )]))
        .unwrap();
    ds.update("DELETE DATA { GRAPH <http://ex.org/hr> { <http://ex.org/alice> <http://ex.org/manages> <http://ex.org/team7> } }").unwrap();
    let mut req = ExplainRequest::new([ex("alice"), ty(), ex("Person")]);
    req.source = Some(ExplainSource::Materialized);
    let e = ds.reasoning().explain(&req).unwrap();
    assert_eq!(e.source, Some(ExplainSource::RdfsOnRead));
    assert_eq!(e.freshness.unwrap().stale, Some(true));
    found(e.explained);
}
