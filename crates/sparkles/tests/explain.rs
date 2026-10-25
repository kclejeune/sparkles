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

#[cfg(feature = "reasoning")]
#[test]
fn rdfs_on_read_skips_an_unsupported_inferred_root_and_uses_a_supported_mixed_proof() {
    use sparkles::reasoning::{
        ReasonRequest,
        rdfs::NewSchema,
        reasoner::{GraphRef, Inputs, Profile},
    };
    let ds = Dataset::memory();
    ds.load_str("@prefix ex: <http://ex.org/> . ex:secret { ex:alice ex:secret ex:value . } ex:public { ex:alice ex:works ex:company . }",RdfFormat::TriG).unwrap();
    ds.reasoning().run(&ReasonRequest {
        profile: Profile::Rules("@prefix ex: <http://ex.org/> . [hidden: (?x ex:secret ?y) -> (?x rdf:type ex:Employee)] [public: (?x ex:works ?y) -> (?x rdf:type ex:Member)]".into()),
        inputs: Inputs { data_graphs: vec![GraphRef::Named("http://ex.org/secret".into()),GraphRef::Named("http://ex.org/public".into())], ..Default::default() },
        ..Default::default()
    }).unwrap();
    ds.reasoning()
        .rdfs()
        .set(NewSchema::Triples(vec![oxrdf::Triple::new(
            NamedNode::new("http://ex.org/Member").unwrap(),
            oxrdf::vocab::rdfs::SUB_CLASS_OF.into_owned(),
            NamedNode::new("http://ex.org/Employee").unwrap(),
        )]))
        .unwrap();
    let mut req = ExplainRequest::new([ex("alice"), ty(), ex("Employee")]);
    req.source = Some(ExplainSource::Materialized);
    req.access = only(&["http://ex.org/public", "urn:x-sparkles:inferred"]);
    let e = ds.reasoning().explain(&req).unwrap();
    assert_eq!(e.source, Some(ExplainSource::RdfsOnRead));
    let trees = found(e.explained);
    fn visible(d: &Derivation) {
        assert_ne!(d.graph.as_deref(), Some("http://ex.org/secret"));
        assert_ne!(d.graph.as_deref(), Some("urn:x-sparkles:inferred"));
        for p in &d.premises {
            visible(p);
        }
    }
    assert_eq!(rule(&trees[0]), "rdfs9");
    for t in &trees {
        visible(t);
    }
}

#[cfg(feature = "reasoning")]
#[test]
fn generalized_references_can_be_expanded_through_the_request_contract() {
    use sparkles::reasoning::explain::{request_from_json, term_json};
    use sparkles::reasoning::{ReasonRequest, reasoner::Profile};
    let ds = Dataset::memory();
    ds.load_str(
        "@prefix ex: <http://ex.org/> . ex:a ex:value \"1\" . ex:b ex:value \"1\" .",
        RdfFormat::Turtle,
    )
    .unwrap();
    ds.reasoning().run(&ReasonRequest { profile: Profile::Rules("@prefix ex: <http://ex.org/> . [reverse: (?x ex:value ?v) -> (?v ex:of ?x)] [pair: (?v ex:of ?x), (?v ex:of ?y), notEqual(?x,?y) -> (?x ex:pair ?y)] [blank: (?x ex:value ?v) -> (?x _:predicate ?v)]".into()), ..Default::default() }).unwrap();
    let mut req = ExplainRequest::new([ex("a"), ex("pair"), ex("b")]);
    req.max_depth = 1;
    let trees = found(ds.reasoning().explain(&req).unwrap().explained);
    let refs: Vec<_> = trees[0]
        .premises
        .iter()
        .filter(|d| d.kind == DerivationKind::Ref && d.generalized)
        .collect();
    assert_eq!(refs.len(), 2);
    for reference in refs {
        let triple = &reference.triple;
        let json = serde_json::json!({"mode":"why","triple":{"s":term_json(&triple[0]),"p":term_json(&triple[1]),"o":term_json(&triple[2])}});
        let (request, _, _) =
            request_from_json(&json).expect("a generalized ref is a valid why target");
        let explanation = ds.reasoning().explain(&request).unwrap();
        assert_eq!(explanation.source, Some(ExplainSource::Materialized));
        assert!(found(explanation.explained)[0].generalized);
        let mut why_not = json.clone();
        why_not["mode"] = "whyNot".into();
        assert!(request_from_json(&why_not).is_err());
    }
    let blank = oxrdf::Term::BlankNode(oxrdf::BlankNode::new("predicate").unwrap());
    let json = serde_json::json!({"mode":"why",
        "triple":{"s":term_json(&ex("a")),"p":term_json(&blank),"o":term_json(&oxrdf::Literal::from("1").into())}});
    let (request, _, _) = request_from_json(&json).unwrap();
    let explanation = ds.reasoning().explain(&request).unwrap();
    assert_eq!(explanation.source, Some(ExplainSource::Materialized));
    assert!(found(explanation.explained)[0].generalized);
    let mut why_not = json;
    why_not["mode"] = "whyNot".into();
    assert!(request_from_json(&why_not).is_err());
}

#[cfg(feature = "reasoning")]
#[test]
fn generalized_targets_round_trip_through_text_surfaces() {
    use sparkles::reasoning::explain::triple_from_text;
    let literal = triple_from_text("\"value\" <urn:p> <urn:o>", &[]).unwrap();
    assert!(matches!(literal[0], oxrdf::Term::Literal(_)));
    let blank_predicate = triple_from_text("<urn:s> _:predicate <urn:o>", &[]).unwrap();
    assert!(matches!(blank_predicate[1], oxrdf::Term::BlankNode(_)));
    assert!(triple_from_text("<urn:s> \"predicate\" <urn:o>", &[]).is_err());
    assert!(triple_from_text("?s <urn:p> <urn:o>", &[]).is_err());
}

#[cfg(feature = "reasoning")]
#[test]
fn review_r2_generalized_refs_keep_the_materialized_snapshot() {
    use sparkles::reasoning::{ReasonRequest, reasoner::Profile};
    for stale in [false, true] {
        let (_dir, ds) = open();
        ds.load_str(
            "<urn:a> <urn:value> \"1\" . <urn:b> <urn:value> \"1\" .",
            RdfFormat::Turtle,
        )
        .unwrap();
        ds.reasoning().run(&ReasonRequest { profile: Profile::Rules("[reverse: (?x <urn:value> ?v) -> (?v <urn:of> ?x)] [pair: (?v <urn:of> ?x), (?v <urn:of> ?y), notEqual(?x,?y) -> (?x <urn:pair> ?y)]".into()), ..Default::default() }).unwrap();
        let term = |iri: &str| Term::NamedNode(NamedNode::new(iri).unwrap());
        let mut request = ExplainRequest::new([term("urn:a"), term("urn:pair"), term("urn:b")]);
        request.source = Some(ExplainSource::Materialized);
        request.max_depth = 1;
        let parent = ds.reasoning().explain(&request).unwrap();
        assert_eq!(parent.source, Some(ExplainSource::Materialized));
        let reference = found(parent.explained)[0]
            .premises
            .iter()
            .find(|d| d.generalized && d.kind == DerivationKind::Ref)
            .unwrap()
            .triple
            .clone();
        if stale {
            ds.update("DELETE DATA { <urn:a> <urn:value> \"1\" }")
                .unwrap();
        }
        let mut request = ExplainRequest::new(reference);
        request.source = Some(ExplainSource::Materialized);
        let child = ds.reasoning().explain(&request).unwrap();
        assert_eq!(
            child.commit, parent.commit,
            "expansion must keep the selected historical read"
        );
        assert_eq!(
            child.source, parent.source,
            "the UI graft check requires the same source"
        );
        found(child.explained);
    }
}

#[cfg(feature = "reasoning")]
#[test]
fn review_r2_returned_blank_predicate_ref_round_trips_verbatim() {
    use sparkles::reasoning::explain::{request_from_json, term_json};
    use sparkles::reasoning::{ReasonRequest, reasoner::Profile};
    let ds = Dataset::memory();
    ds.load_str("<urn:a> <urn:p> <urn:b> .", RdfFormat::Turtle)
        .unwrap();
    let labels = ["predicate", "λ", "spkrule_707265646963617465", "r6"];
    let rules: String = labels.iter().enumerate().map(|(i,label)| format!("[blank{i}: (?s <urn:p> ?o) -> (?s _:{label} ?o)] [use{i}: (?s _:{label} ?o) -> (?s <urn:q{i}> ?o)] ")).collect();
    ds.reasoning()
        .run(&ReasonRequest {
            profile: Profile::Rules(rules),
            ..Default::default()
        })
        .unwrap();
    let term = |iri: &str| Term::NamedNode(NamedNode::new(iri).unwrap());
    let mut predicates = std::collections::HashSet::new();
    for i in 0..labels.len() {
        let mut parent =
            ExplainRequest::new([term("urn:a"), term(&format!("urn:q{i}")), term("urn:b")]);
        parent.source = Some(ExplainSource::Demand);
        parent.max_depth = 1;
        let tree = found(ds.reasoning().explain(&parent).unwrap().explained).remove(0);
        let reference = tree
            .premises
            .iter()
            .find(|d| d.generalized && d.kind == DerivationKind::Ref)
            .expect("blank predicate is a generalized depth cut");
        let t = &reference.triple;
        assert_eq!(
            sparkles::reasoning::explain::triple_from_text(
                &format!("{} {} {}", t[0], t[1], t[2]),
                &[],
            )
            .unwrap(),
            *t
        );
        assert!(
            predicates.insert(t[1].clone()),
            "distinct source constants must not alias"
        );
        let json = serde_json::json!({"mode":"why","source":"demand","triple":{"s":term_json(&t[0]),"p":term_json(&t[1]),"o":term_json(&t[2])}});
        let (request, _, _) = request_from_json(&json).unwrap();
        let tree = found(ds.reasoning().explain(&request).unwrap().explained).remove(0);
        assert_eq!(tree.triple, reference.triple);
    }
}

#[cfg(feature = "reasoning")]
#[test]
fn mixed_historical_refs_keep_their_materialized_read_context() {
    use sparkles::reasoning::explain::explanation_json;
    use sparkles::reasoning::{ReasonRequest, rdfs::NewSchema, reasoner::Profile};
    let (_dir, ds) = open();
    ds.load_str(
        "<urn:a> <urn:value> \"1\" . <urn:b> <urn:value> \"1\" .",
        RdfFormat::Turtle,
    )
    .unwrap();
    ds.reasoning().run(&ReasonRequest { profile: Profile::Rules("[reverse: (?x <urn:value> ?v) -> (?v <urn:of> ?x)] [pair: (?v <urn:of> ?x), (?v <urn:of> ?y), notEqual(?x,?y) -> (?x <urn:pair> ?y)] [type: (?x <urn:pair> ?y) -> (?x rdf:type <urn:Employee>)]".into()), ..Default::default() }).unwrap();
    ds.reasoning()
        .rdfs()
        .set(NewSchema::Triples(vec![oxrdf::Triple::new(
            NamedNode::new("urn:Employee").unwrap(),
            oxrdf::vocab::rdfs::SUB_CLASS_OF.into_owned(),
            NamedNode::new("urn:Person").unwrap(),
        )]))
        .unwrap();
    ds.update("DELETE DATA { <urn:a> <urn:value> \"1\" }")
        .unwrap();
    let mut request = ExplainRequest::new([
        Term::NamedNode(NamedNode::new("urn:a").unwrap()),
        ty(),
        Term::NamedNode(NamedNode::new("urn:Person").unwrap()),
    ]);
    request.source = Some(ExplainSource::Materialized);
    request.max_depth = 3;
    let parent = ds.reasoning().explain(&request).unwrap();
    assert_eq!(parent.source, Some(ExplainSource::RdfsOnRead));
    fn reference(node: &Derivation) -> Option<&Derivation> {
        if node.generalized && node.kind == DerivationKind::Ref {
            Some(node)
        } else {
            node.premises.iter().find_map(reference)
        }
    }
    let trees = match &parent.explained {
        Explained::Found { trees, .. } => trees,
        _ => panic!("no mixed tree"),
    };
    let reference = reference(&trees[0]).expect("cut generalized materialized premise");
    let read = reference
        .read
        .expect("mixed subtree keeps its historical context");
    assert_ne!(read.commit, parent.commit);
    assert_eq!(read.source, "materialized");
    let json = explanation_json(&parent);
    assert!(json.to_string().contains("\"read\""));
    let mut request = ExplainRequest::new(reference.triple.clone());
    request.source = Some(ExplainSource::Materialized);
    let child = ds.reasoning().explain(&request).unwrap();
    assert_eq!(child.commit, read.commit);
    assert_eq!(child.source.unwrap().as_str(), read.source);
    assert_eq!(child.read_at_head, read.read_at_head);
    found(child.explained);
}

#[cfg(feature = "reasoning")]
#[test]
fn why_not_marks_hypotheses_that_introduce_a_new_inconsistency() {
    use sparkles::reasoning::explain::{ExplainMode, explanation_json};
    let ds = Dataset::memory();
    ds.load_str("@prefix ex: <http://ex.org/> . @prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> . @prefix owl: <http://www.w3.org/2002/07/owl#> . ex:alice a ex:Cat . ex:Cat owl:disjointWith ex:Dog . ex:Dog rdfs:subClassOf ex:Animal .",RdfFormat::Turtle).unwrap();
    let head = ds.store().snapshot().commit;
    let mut request = ExplainRequest::new([ex("alice"), ty(), ex("Animal")]);
    request.mode = ExplainMode::WhyNot;
    request.profile = Some("rdfs-simple".into());
    request.max_missing = 1;
    request.max_depth = 2;
    let explanation = ds.reasoning().explain(&request).unwrap();
    let value = explanation_json(&explanation);
    let hypotheses = value["hypotheses"].as_array().unwrap();
    let dog = hypotheses
        .iter()
        .find(|hypothesis| {
            hypothesis["missing"]
                .as_array()
                .unwrap()
                .iter()
                .any(|triple| {
                    triple["s"]["value"] == "http://ex.org/alice"
                        && triple["p"]["value"] == oxrdf::vocab::rdf::TYPE.as_str()
                        && triple["o"]["value"] == "http://ex.org/Dog"
                })
        })
        .expect("the direct Dog type is a verified hypothesis");
    assert_eq!(dog["inconsistent"], true, "{value}");
    assert_eq!(dog["consistencyComplete"], true);
    assert!(
        hypotheses
            .iter()
            .any(|hypothesis| hypothesis["inconsistent"] == false),
        "{value}"
    );
    assert_eq!(ds.store().snapshot().commit, head);
}

#[cfg(feature = "reasoning")]
#[test]
fn why_not_consistency_keeps_placeholders_visible_to_named_only_programs() {
    use sparkles::reasoning::explain::{ExplainMode, explanation_json};
    use sparkles::reasoning::reasoner::{GraphRef, ImportMode, Inputs, Profile};
    let ds = Dataset::memory();
    ds.load_str("@prefix ex: <http://ex.org/> . @prefix owl: <http://www.w3.org/2002/07/owl#> . ex:hr {ex:alice a ex:Cat . ex:Cat owl:disjointWith ex:Result .}",RdfFormat::TriG).unwrap();
    ds.reasoning().run(&sparkles::reasoning::ReasonRequest { profile:Profile::Rules("[r: (?x <http://ex.org/p> ?y) -> (?x <http://www.w3.org/1999/02/22-rdf-syntax-ns#type> <http://ex.org/Result>)]".into()),inputs:Inputs {data_graphs:vec![GraphRef::Named("http://ex.org/hr".into())],ontology_graphs:Vec::new(),imports:ImportMode::None,location_mapping:Default::default()},..Default::default() }).unwrap();
    let head = ds.store().snapshot().commit;
    let mut request = ExplainRequest::new([ex("alice"), ty(), ex("Result")]);
    request.mode = ExplainMode::WhyNot;
    request.max_missing = 1;
    let value = explanation_json(&ds.reasoning().explain(&request).unwrap());
    let hypothesis = value["hypotheses"]
        .as_array()
        .unwrap()
        .iter()
        .find(|hypothesis| !hypothesis["placeholders"].as_array().unwrap().is_empty())
        .expect("the missing variable gets a placeholder");
    assert_eq!(hypothesis["inconsistent"], true, "{value}");
    assert_eq!(ds.store().snapshot().commit, head);
}

#[cfg(feature = "reasoning")]
#[test]
fn existing_unrelated_inconsistencies_do_not_mark_every_hypothesis() {
    use sparkles::reasoning::explain::{ExplainMode, explanation_json};
    let ds = Dataset::memory();
    ds.load_str("@prefix ex: <http://ex.org/> . @prefix owl: <http://www.w3.org/2002/07/owl#> . ex:alice a ex:Manager . ex:Cat owl:disjointWith ex:Dog . ex:unrelated a ex:Cat, ex:Dog .",RdfFormat::Turtle).unwrap();
    let mut request = ExplainRequest::new([ex("alice"), ty(), ex("Employee")]);
    request.mode = ExplainMode::WhyNot;
    request.profile = Some("rdfs-simple".into());
    request.max_missing = 1;
    request.max_depth = 1;
    let value = explanation_json(&ds.reasoning().explain(&request).unwrap());
    let hypothesis = value["hypotheses"]
        .as_array()
        .unwrap()
        .iter()
        .find(|hypothesis| {
            hypothesis["missing"]
                .as_array()
                .unwrap()
                .iter()
                .any(|triple| {
                    triple["s"]["value"] == "http://ex.org/Manager"
                        && triple["p"]["value"] == oxrdf::vocab::rdfs::SUB_CLASS_OF.as_str()
                        && triple["o"]["value"] == "http://ex.org/Employee"
                })
        })
        .expect("the subclass axiom is a verified hypothesis");
    assert_eq!(hypothesis["inconsistent"], false, "{value}");
}

#[cfg(feature = "reasoning")]
#[test]
fn hypothesis_consistency_budget_is_unknown_and_cancellation_propagates() {
    use sparkles::reasoning::analysis::AnalysisOptions;
    use sparkles::reasoning::explain::ExplainMode;
    use std::sync::atomic::AtomicBool;
    let ds = Dataset::memory();
    ds.load_str(
        "@prefix ex: <http://ex.org/> . ex:alice a ex:Manager .",
        RdfFormat::Turtle,
    )
    .unwrap();
    let mut request = ExplainRequest::new([ex("alice"), ty(), ex("Employee")]);
    request.mode = ExplainMode::WhyNot;
    request.profile = Some("rdfs-simple".into());
    request.max_missing = 1;
    let mut answer = ds.reasoning().explain(&request).unwrap().why_not.unwrap();
    let reasoner = ds
        .query_reasoner(Some(sparkles::reasoning::reasoner::Profile::RdfsSimple))
        .unwrap();
    let snapshot = ds.store().snapshot();
    sparkles::reasoning::hypotheses::check_consistency(
        &ds,
        &snapshot,
        &reasoner,
        &AnalysisOptions {
            max_memory: Some(1),
            ..Default::default()
        },
        &mut answer,
    )
    .unwrap();
    let sparkles::reasoning::reasoner::whynot::WhyNot::NotEntailed { hypotheses, .. } = &answer
    else {
        panic!("expected hypotheses")
    };
    assert!(!hypotheses.is_empty());
    assert!(
        hypotheses
            .iter()
            .all(|hypothesis| hypothesis.inconsistent.is_none())
    );
    let error = sparkles::reasoning::hypotheses::check_consistency(
        &ds,
        &snapshot,
        &reasoner,
        &AnalysisOptions {
            cancel: Some(Arc::new(AtomicBool::new(true))),
            ..Default::default()
        },
        &mut answer,
    )
    .unwrap_err();
    assert!(matches!(error, sparkles::Error::Cancelled));
}

#[cfg(feature = "reasoning")]
#[test]
fn hypothesis_consistency_uses_the_verified_geometry_program() {
    use sparkles::reasoning::analysis::AnalysisOptions;
    use sparkles::reasoning::reasoner::explain::DemandInput;
    use sparkles::reasoning::reasoner::whynot::{WhyNot, WhyNotOptions, why_not};
    use sparkles::reasoning::reasoner::{Extras, Inputs, Profile, query::QueryReasoner};
    let ds = Dataset::memory();
    ds.load_str("@prefix ex: <http://ex.org/> . @prefix owl: <http://www.w3.org/2002/07/owl#> . ex:alice a ex:Cat . ex:Cat owl:disjointWith ex:Result .", RdfFormat::Turtle).unwrap();
    let reasoner = QueryReasoner::new(Profile::Rules("@prefix ex: <http://ex.org/> . @prefix rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#> . @prefix geo: <http://www.opengis.net/ont/geosparql#> . [use: (?f geo:hasDefaultGeometry ?g) -> (?f ex:target ?g), (?f rdf:type ex:Result)]".into()), &Extras { geo_default_geometry:true, ..Default::default() }, Inputs::default()).unwrap();
    let snapshot = ds.store().snapshot();
    let missing = [
        ex("alice"),
        Term::NamedNode(NamedNode::new_unchecked(
            "http://www.opengis.net/ont/geosparql#hasGeometry",
        )),
        ex("shape"),
    ];
    let mut answer = why_not(
        &DemandInput {
            reasoner: &reasoner,
            snap: snapshot.clone(),
            readable: None,
        },
        &[ex("alice"), ex("target"), ex("shape")],
        &WhyNotOptions {
            max_missing: 1,
            ..Default::default()
        },
    )
    .unwrap();
    sparkles::reasoning::hypotheses::check_consistency(
        &ds,
        &snapshot,
        &reasoner,
        &AnalysisOptions::default(),
        &mut answer,
    )
    .unwrap();
    let WhyNot::NotEntailed { hypotheses, .. } = answer else {
        panic!("expected a hypothesis")
    };
    let hypothesis = hypotheses
        .iter()
        .find(|hypothesis| hypothesis.missing == [missing.clone()])
        .expect("the implicit default rule supplies a hasGeometry hypothesis");
    assert_eq!(hypothesis.inconsistent, Some(true), "{hypotheses:?}");
}
