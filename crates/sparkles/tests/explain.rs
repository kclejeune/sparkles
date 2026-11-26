//! `Dataset::reasoning().explain` (spec U10 §3.3).

#[cfg(feature = "reasoning")]
#[path = "../../sparkles-core/tests/support/caller_allocations.rs"]
mod caller_allocations;

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
fn rdfs_on_read_materialized_resolver_does_not_clone_held_premise_over_workspace() {
    use sparkles::reasoning::{
        ReasonRequest,
        rdfs::NewSchema,
        reasoner::{GraphRef, Inputs, Profile},
    };

    // 1. Materialized rule derives `ex:s ex:p "<12 KiB>"` in urn:x-sparkles:inferred while
    //    `ex:p rdfs:domain ex:C` exists only in the on-read schema. Explaining `ex:s a ex:C`
    //    with an 8 KiB answer limit (16,640-byte workspace) misses the materialized search,
    //    decodes and holds the 12 KiB inferred premise inside the workspace, and invokes the
    //    production resolver. The resolver must not clone the held 12 KiB premise over the
    //    16,640-byte workspace cap.
    {
        let (_dir, ds) = open();
        ds.load_str(
            "<http://ex.org/init> <http://ex.org/initp> <http://ex.org/inito> .\n",
            RdfFormat::Turtle,
        )
        .unwrap();
        ds.store().compact().unwrap();
        let lit_12k = "m".repeat(12_288);
        ds.load_str(
            &format!(
                "@prefix ex: <http://ex.org/> .\n\
                 ex:hr {{ ex:s ex:seed \"{lit_12k}\" . }}\n"
            ),
            RdfFormat::TriG,
        )
        .unwrap();
        ds.reasoning()
            .run(&ReasonRequest {
                profile: Profile::Rules(
                    "@prefix ex: <http://ex.org/> . [derive: (?x ex:seed ?y) -> (?x ex:p ?y)]"
                        .into(),
                ),
                inputs: Inputs {
                    data_graphs: vec![GraphRef::Named(HR.into())],
                    ..Default::default()
                },
                ..Default::default()
            })
            .unwrap();
        let snap = ds.store().snapshot();
        let lit_id = snap
            .lookup_term(&Term::Literal(oxrdf::Literal::new_simple_literal(&lit_12k)))
            .expect("derived 12 KiB literal must be stored");
        assert_eq!(lit_id.tag(), sparkles_core::id::Tag::Delta);
        drop(snap);

        ds.reasoning()
            .rdfs()
            .set(NewSchema::Triples(vec![oxrdf::Triple::new(
                NamedNode::new("http://ex.org/p").unwrap(),
                oxrdf::vocab::rdfs::DOMAIN.into_owned(),
                NamedNode::new("http://ex.org/C").unwrap(),
            )]))
            .unwrap();

        let unbounded = ds
            .reasoning()
            .explain(&ExplainRequest::new([ex("s"), ty(), ex("C")]))
            .unwrap();
        assert_eq!(unbounded.source, Some(ExplainSource::RdfsOnRead));
        let trees = found(unbounded.explained);
        assert_eq!(rule(&trees[0]), "rdfs2");
        assert_eq!(rule(&trees[0].premises[0]), "derive");

        let mut req = ExplainRequest::new([ex("s"), ty(), ex("C")]);
        req.max_memory = Some(8192);
        let (res, alloc) = caller_allocations::measure(|| ds.reasoning().explain(&req));
        let err = res.map(|_| ()).unwrap_err();
        assert!(
            matches!(&err, sparkles::Error::Component(c) if c.code == sparkles::reasoning::explain::REASONING_BUDGET),
            "expected reasoning-budget, got {err:?}"
        );
        assert!(
            alloc.peak <= 16_640,
            "materialized resolver must not clone held 12 KiB premise over workspace cap (peak_live={} > 16640, largest={})",
            alloc.peak,
            alloc.largest
        );

        // 2. With remaining budget > 0 (retained ~12.4 KiB < 13,000), the resolver enters the
        //    materialized search and still passes the premise by reference without cloning it.
        let mut req_remaining = ExplainRequest::new([ex("s"), ty(), ex("C")]);
        req_remaining.max_memory = Some(13_000);
        let (res, alloc) = caller_allocations::measure(|| ds.reasoning().explain(&req_remaining));
        let err = res.map(|_| ()).unwrap_err();
        assert!(
            matches!(&err, sparkles::Error::Component(c) if c.code == sparkles::reasoning::explain::REASONING_BUDGET),
            "expected reasoning-budget with small remaining budget, got {err:?}"
        );
        assert!(
            alloc.peak < 24_000,
            "materialized search must pass premise by reference without duplicating 12 KiB literal (peak_live={}, largest={})",
            alloc.peak,
            alloc.largest
        );
    }

    // 3. Generalized materialized rule (using the demand fallback inside materialized search)
    //    grafted under an on-read domain rule: succeeds unbounded and stays within the
    //    16,640-byte workspace cap under an 8 KiB answer limit.
    {
        let (_dir, ds) = open();
        ds.load_str(
            "<http://ex.org/init> <http://ex.org/initp> <http://ex.org/inito> .\n",
            RdfFormat::Turtle,
        )
        .unwrap();
        ds.store().compact().unwrap();
        let lit_12k = "g".repeat(12_288);
        ds.load_str(
            &format!(
                "@prefix ex: <http://ex.org/> .\n\
                 ex:hr {{ ex:s ex:seed \"{lit_12k}\" . }}\n"
            ),
            RdfFormat::TriG,
        )
        .unwrap();
        ds.reasoning()
            .run(&ReasonRequest {
                profile: Profile::Rules(
                    "@prefix ex: <http://ex.org/> . \
                     [reverse: (?x ex:seed ?v) -> (?v ex:of ?x)] \
                     [derive: (?v ex:of ?x) -> (?x ex:p ?v)]"
                        .into(),
                ),
                inputs: Inputs {
                    data_graphs: vec![GraphRef::Named(HR.into())],
                    ..Default::default()
                },
                ..Default::default()
            })
            .unwrap();
        ds.reasoning()
            .rdfs()
            .set(NewSchema::Triples(vec![oxrdf::Triple::new(
                NamedNode::new("http://ex.org/p").unwrap(),
                oxrdf::vocab::rdfs::DOMAIN.into_owned(),
                NamedNode::new("http://ex.org/C").unwrap(),
            )]))
            .unwrap();

        let unbounded = ds
            .reasoning()
            .explain(&ExplainRequest::new([ex("s"), ty(), ex("C")]))
            .unwrap();
        assert_eq!(unbounded.source, Some(ExplainSource::RdfsOnRead));
        let trees = found(unbounded.explained);
        assert_eq!(rule(&trees[0]), "rdfs2");
        assert_eq!(rule(&trees[0].premises[0]), "derive");

        let mut req = ExplainRequest::new([ex("s"), ty(), ex("C")]);
        req.max_memory = Some(8192);
        let (res, alloc) = caller_allocations::measure(|| ds.reasoning().explain(&req));
        let err = res.map(|_| ()).unwrap_err();
        assert!(
            matches!(&err, sparkles::Error::Component(c) if c.code == sparkles::reasoning::explain::REASONING_BUDGET),
            "expected reasoning-budget on generalized mixed proof, got {err:?}"
        );
        assert!(
            alloc.peak <= 16_640,
            "generalized materialized resolver must stay within workspace cap (peak_live={} > 16640, largest={})",
            alloc.peak,
            alloc.largest
        );
    }

    // 4. Generalized mixed fixture with positive remaining headroom large enough for the
    //    materialized pass to return NotFound and enter demand_with_for -> explain_demand:
    //    the demand fallback must not duplicate the workspace-held premise unbudgeted.
    {
        let (_dir, ds) = open();
        ds.load_str(
            "@prefix ex: <http://ex.org/> .\n\
             @prefix rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#> .\n\
             ex:init rdf:first rdf:nil ; rdf:rest rdf:nil ; ex:of ex:init .\n",
            RdfFormat::Turtle,
        )
        .unwrap();
        ds.store().compact().unwrap();
        let lit_64k = "d".repeat(65_536);
        ds.load_str(
            &format!(
                "@prefix ex: <http://ex.org/> .\n\
                 ex:hr {{ ex:s ex:seed \"{lit_64k}\" . }}\n"
            ),
            RdfFormat::TriG,
        )
        .unwrap();
        ds.reasoning()
            .run(&ReasonRequest {
                profile: Profile::Rules(
                    "@prefix ex: <http://ex.org/> . \
                     [reverse: (?x ex:seed ?v) -> (?v ex:of ?x)] \
                     [derive: (?v ex:of ?x) -> (?x ex:p ?v)]"
                        .into(),
                ),
                inputs: Inputs {
                    data_graphs: vec![GraphRef::Named(HR.into())],
                    ..Default::default()
                },
                ..Default::default()
            })
            .unwrap();
        ds.reasoning()
            .rdfs()
            .set(NewSchema::Triples(vec![oxrdf::Triple::new(
                NamedNode::new("http://ex.org/p").unwrap(),
                oxrdf::vocab::rdfs::DOMAIN.into_owned(),
                NamedNode::new("http://ex.org/C").unwrap(),
            )]))
            .unwrap();

        let unbounded = ds
            .reasoning()
            .explain(&ExplainRequest::new([ex("s"), ty(), ex("C")]))
            .unwrap();
        assert_eq!(unbounded.source, Some(ExplainSource::RdfsOnRead));
        let trees = found(unbounded.explained);
        assert_eq!(rule(&trees[0]), "rdfs2");
        assert_eq!(rule(&trees[0].premises[0]), "derive");

        let mut req = ExplainRequest::new([ex("s"), ty(), ex("C")]);
        req.max_memory = Some(200_000);
        let (res, alloc) = caller_allocations::measure(|| ds.reasoning().explain(&req));
        let err = res.map(|_| ()).unwrap_err();
        assert!(
            matches!(&err, sparkles::Error::Component(c) if c.code == sparkles::reasoning::explain::REASONING_BUDGET),
            "expected reasoning-budget in demand fallback, got {err:?}"
        );
        assert!(
            alloc.peak <= 200_000 && alloc.largest <= 65_600,
            "demand fallback must not clone held 64 KiB premise over budget (peak_live={}, largest={})",
            alloc.peak,
            alloc.largest
        );
    }
}

#[cfg(feature = "reasoning")]
#[test]
fn materialized_fast_miss_refuses_oversized_key_before_allocating() {
    use sparkles::reasoning::{
        ReasonRequest,
        reasoner::{GraphRef, Inputs, Profile},
    };

    let (_dir, ds) = open();
    ds.load_str(
        "@prefix ex: <http://ex.org/> .\n\
         ex:hr { ex:s ex:p ex:o . }\n",
        RdfFormat::TriG,
    )
    .unwrap();
    ds.reasoning()
        .run(&ReasonRequest {
            profile: Profile::Rules(
                "@prefix ex: <http://ex.org/> . [derive: (?x ex:p ?y) -> (?x ex:q ?y)]".into(),
            ),
            inputs: Inputs {
                data_graphs: vec![GraphRef::Named(HR.into())],
                ..Default::default()
            },
            ..Default::default()
        })
        .unwrap();

    // 1. Oversized absent simple literal must refuse before allocating a 64 KiB vocabulary key.
    let mut req_lit = ExplainRequest::new([
        ex("s"),
        ex("q"),
        Term::Literal(oxrdf::Literal::new_simple_literal("k".repeat(65_536))),
    ]);
    req_lit.max_memory = Some(8192);
    let (res, alloc) = caller_allocations::measure(|| ds.reasoning().explain(&req_lit));
    let err = res.map(|_| ()).unwrap_err();
    assert!(
        matches!(&err, sparkles::Error::Component(c) if c.code == sparkles::reasoning::explain::REASONING_BUDGET),
        "expected reasoning-budget on oversized literal fast miss, got {err:?}"
    );
    assert!(
        alloc.peak <= 8192,
        "fast miss must refuse before allocating 64 KiB literal key (peak_live={}, largest={})",
        alloc.peak,
        alloc.largest
    );

    // 2. Oversized RDF 1.2 triple term must refuse before allocating recursive key buffers.
    let mut req_triple = ExplainRequest::new([
        ex("s"),
        ex("q"),
        Term::Triple(Box::new(oxrdf::Triple::new(
            NamedNode::new("http://ex.org/a").unwrap(),
            NamedNode::new("http://ex.org/b").unwrap(),
            oxrdf::Literal::new_simple_literal("t".repeat(65_536)),
        ))),
    ]);
    req_triple.max_memory = Some(8192);
    let (res, alloc) = caller_allocations::measure(|| ds.reasoning().explain(&req_triple));
    let err = res.map(|_| ()).unwrap_err();
    assert!(
        matches!(&err, sparkles::Error::Component(c) if c.code == sparkles::reasoning::explain::REASONING_BUDGET),
        "expected reasoning-budget on oversized triple-term fast miss, got {err:?}"
    );
    assert!(
        alloc.peak <= 8192,
        "fast miss must refuse before allocating 64 KiB triple-term key (peak_live={}, largest={})",
        alloc.peak,
        alloc.largest
    );

    // 3. Tiny headroom smaller than even a short IRI key must refuse before allocating the key.
    let mut req_tiny = ExplainRequest::new([ex("absent"), ex("q"), ex("o")]);
    req_tiny.max_memory = Some(8);
    let (res, alloc) = caller_allocations::measure(|| ds.reasoning().explain(&req_tiny));
    let err = res.map(|_| ()).unwrap_err();
    assert!(
        matches!(&err, sparkles::Error::Component(c) if c.code == sparkles::reasoning::explain::REASONING_BUDGET),
        "expected reasoning-budget on tiny-headroom fast miss, got {err:?}"
    );
    assert!(
        alloc.peak <= 4096,
        "fast miss with tiny headroom must not spike allocations (peak_live={}, largest={})",
        alloc.peak,
        alloc.largest
    );
}

#[cfg(feature = "reasoning")]
#[test]
fn materialized_fast_miss_bounds_fsst_probe_memory() {
    use sparkles::reasoning::{
        ReasonRequest,
        reasoner::{GraphRef, Inputs, Profile},
    };
    use sparkles_core::{
        builder::{BuildOptions, VocabCodec},
        store::StoreOptions,
    };

    let dir = tempfile::tempdir().unwrap();
    let ds = Dataset::open_with(
        dir.path().join("db"),
        StoreOptions {
            build: BuildOptions {
                vocab_codec: Some(VocabCodec::Fsst),
                ..Default::default()
            },
            ..Default::default()
        },
    )
    .unwrap();
    // Fill one literal block so the subject and predicate probes use a different block.
    let long = "z".repeat(65_536);
    let mut data = format!(r#"@prefix ex: <http://ex.org/> . ex:hr {{ ex:s ex:p "a", "{long}""#);
    for i in 2..sparkles_core::vocab::FC_BLOCK {
        data.push_str(&format!(r#", "~filler{i}""#));
    }
    data.push_str(" . }");
    ds.load_str(&data, RdfFormat::TriG).unwrap();
    ds.store().compact().unwrap();
    let snap = ds.store().snapshot();
    assert!(snap.generation.vocab.is_fsst());
    assert_eq!(
        snap.generation.vocab.find(&sparkles_core::id::term_key(
            &oxrdf::Literal::new_simple_literal(&long).into()
        )),
        Ok(1)
    );
    ds.reasoning()
        .run(&ReasonRequest {
            profile: Profile::Rules(
                "@prefix ex: <http://ex.org/> . [derive: (?x ex:p ?y) -> (?x ex:q ?y)]".into(),
            ),
            inputs: Inputs {
                data_graphs: vec![GraphRef::Named(HR.into())],
                ..Default::default()
            },
            ..Default::default()
        })
        .unwrap();

    let mut req = ExplainRequest::new([
        ex("s"),
        ex("q"),
        oxrdf::Literal::new_simple_literal("b").into(),
    ]);
    req.source = Some(ExplainSource::Materialized);
    assert!(matches!(
        ds.reasoning().explain(&req).unwrap().explained,
        Explained::NotFound
    ));

    req.max_memory = Some(8192);
    let (result, alloc) = caller_allocations::measure(|| ds.reasoning().explain(&req));
    assert!(
        alloc.largest < 8192,
        "short probe allocated an oversized FSST suffix (peak={}, largest={})",
        alloc.peak,
        alloc.largest
    );
    assert!(matches!(result.unwrap().explained, Explained::NotFound));
    assert!(alloc.peak < 8192, "short probe used {} bytes", alloc.peak);

    let mut admitted = Vec::new();
    let miss = snap
        .lookup_term_controlled(
            &req.triple[2],
            &mut |bytes| {
                admitted.push(bytes);
                Ok(())
            },
            &mut || Ok(()),
        )
        .unwrap();
    assert!(miss.is_none());
    assert_eq!(admitted, [3]);

    req.triple[2] = oxrdf::Literal::new_simple_literal(long).into();
    let (result, alloc) = caller_allocations::measure(|| ds.reasoning().explain(&req));
    let error = result.unwrap_err();
    assert!(
        matches!(&error, sparkles::Error::Component(c) if c.code == sparkles::reasoning::explain::REASONING_BUDGET),
        "{error:?}"
    );
    assert!(
        alloc.largest < 8192,
        "long probe allocated before admission"
    );

    let mut polls = 0;
    let stopped = snap.lookup_term_controlled(&req.triple[2], &mut |_| Ok(()), &mut || {
        polls += 1;
        if polls == 8 {
            Err(sparkles::Error::Cancelled)
        } else {
            Ok(())
        }
    });
    assert!(matches!(stopped, Err(sparkles::Error::Cancelled)));
}

#[cfg(feature = "reasoning")]
#[test]
fn demand_and_why_not_admit_fsst_root_lookup_scratch() {
    use sparkles::reasoning::{
        ReasonRequest,
        explain::ExplainMode,
        reasoner::{GraphRef, Inputs, Profile},
    };
    use sparkles_core::{
        builder::{BuildOptions, VocabCodec},
        store::StoreOptions,
    };

    let dir = tempfile::tempdir().unwrap();
    let ds = Dataset::open_with(
        dir.path().join("db"),
        StoreOptions {
            build: BuildOptions {
                vocab_codec: Some(VocabCodec::Fsst),
                ..Default::default()
            },
            ..Default::default()
        },
    )
    .unwrap();
    let long = "z".repeat(65_536);
    let mut data = format!(r#"@prefix ex: <http://ex.org/> . ex:hr {{ ex:s ex:p "a", "{long}""#);
    for i in 2..sparkles_core::vocab::FC_BLOCK {
        data.push_str(&format!(r#", "~filler{i}""#));
    }
    data.push_str(" . }");
    ds.load_str(&data, RdfFormat::TriG).unwrap();
    ds.store().compact().unwrap();
    ds.reasoning()
        .run(&ReasonRequest {
            profile: Profile::Rules(
                "@prefix ex: <http://ex.org/> . [derive: (?x ex:p ?y) -> (?x ex:q ?y)]".into(),
            ),
            inputs: Inputs {
                data_graphs: vec![GraphRef::Named(HR.into())],
                ..Default::default()
            },
            ..Default::default()
        })
        .unwrap();

    // 48 KiB admits the demand session's term table (~35 KiB) with ~13 KiB left,
    // well under the 64 KiB FSST suffix that precedes "b" and "~filler2".
    for (mode, object) in [
        (ExplainMode::Why, "b"),
        (ExplainMode::Why, "~filler2"),
        (ExplainMode::WhyNot, "b"),
    ] {
        let mut req = ExplainRequest::new([
            ex("s"),
            ex("q"),
            oxrdf::Literal::new_simple_literal(object).into(),
        ]);
        req.source = Some(ExplainSource::Demand);
        req.mode = mode;
        req.max_memory = Some(48 * 1024);
        let (result, alloc) = caller_allocations::measure(|| ds.reasoning().explain(&req));
        assert!(
            alloc.largest < 65_536,
            "{mode:?} on {object:?} allocated an oversized FSST suffix (peak={}, largest={})",
            alloc.peak,
            alloc.largest
        );
        let error = result.map(|_| ()).unwrap_err();
        assert!(
            matches!(&error, sparkles::Error::Component(c) if c.code == sparkles::reasoning::explain::REASONING_BUDGET),
            "{mode:?} on {object:?}: {error:?}"
        );
    }
}

#[cfg(feature = "reasoning")]
#[test]
fn demand_and_why_not_admit_fsst_rule_constant_scratch() {
    use sparkles::reasoning::{
        ReasonRequest,
        explain::ExplainMode,
        reasoner::{GraphRef, Inputs, Profile},
    };
    use sparkles_core::{
        builder::{BuildOptions, VocabCodec},
        store::StoreOptions,
    };

    let dir = tempfile::tempdir().unwrap();
    let ds = Dataset::open_with(
        dir.path().join("db"),
        StoreOptions {
            build: BuildOptions {
                vocab_codec: Some(VocabCodec::Fsst),
                ..Default::default()
            },
            ..Default::default()
        },
    )
    .unwrap();
    let long = "z".repeat(65_536);
    let mut data = format!(r#"@prefix ex: <http://ex.org/> . ex:hr {{ ex:s ex:p "a", "{long}""#);
    for i in 2..sparkles_core::vocab::FC_BLOCK {
        data.push_str(&format!(r#", "~filler{i}""#));
    }
    data.push_str(" . }");
    ds.load_str(&data, RdfFormat::TriG).unwrap();
    ds.store().compact().unwrap();
    ds.reasoning()
        .run(&ReasonRequest {
            profile: Profile::Rules(
                "@prefix ex: <http://ex.org/> . \
                 [derive: (?x ex:p ?y) -> (?x ex:q ?y)] \
                 [fresh: (?x ex:p \"b\") -> (?x ex:derived \"0fresh\")]"
                    .into(),
            ),
            inputs: Inputs {
                data_graphs: vec![GraphRef::Named(HR.into())],
                ..Default::default()
            },
            ..Default::default()
        })
        .unwrap();

    // 48 KiB admits the session's term table (~35 KiB) with ~13 KiB left, well under
    // the 64 KiB FSST suffix that precedes rule constant "b".
    for (source, mode, object) in [
        (ExplainSource::Materialized, ExplainMode::Why, "a"),
        (ExplainSource::Demand, ExplainMode::Why, "b"),
        (ExplainSource::Demand, ExplainMode::WhyNot, "b"),
    ] {
        let mut req = ExplainRequest::new([
            ex("s"),
            ex("q"),
            oxrdf::Literal::new_simple_literal(object).into(),
        ]);
        req.source = Some(source);
        req.mode = mode;
        req.max_memory = Some(48 * 1024);
        let (result, alloc) = caller_allocations::measure(|| ds.reasoning().explain(&req));
        assert!(
            alloc.largest < 65_536,
            "{source:?}/{mode:?} on {object:?} allocated an oversized FSST suffix (peak={}, largest={})",
            alloc.peak,
            alloc.largest
        );
        let error = result.map(|_| ()).unwrap_err();
        assert!(
            matches!(&error, sparkles::Error::Component(c) if c.code == sparkles::reasoning::explain::REASONING_BUDGET),
            "{source:?}/{mode:?} on {object:?}: {error:?}"
        );
    }
}

#[cfg(feature = "reasoning")]
#[test]
fn explanation_graph_name_lookups_bound_fsst_probe_memory() {
    use sparkles::reasoning::{
        ReasonRequest,
        explain::ExplainMode,
        reasoner::{GraphRef, Inputs, Profile},
    };
    use sparkles_core::{
        builder::{BuildOptions, VocabCodec},
        store::StoreOptions,
    };

    let z = |local: &str| Term::NamedNode(NamedNode::new(format!("http://z.org/{local}")).unwrap());
    let long = "z".repeat(65_536);
    let fc = sparkles_core::vocab::FC_BLOCK;

    // 1. Named input graph <http://x.org/hr> sorts after a 64 KiB IRI in block 1
    //    (block 0 holds rdf:* so Terms::new_in_controlled never touches block 1; zero
    //    literals so rank 0 is an IRI and the FSST IRI table trains directly), while
    //    rule constants and target triple IRIs live in <http://z.org/*> (block 2+).
    let named_hr = "http://x.org/hr";
    let long_graph = format!("http://x.org/b{long}");
    let dir = tempfile::tempdir().unwrap();
    let ds = Dataset::open_with(
        dir.path().join("db"),
        StoreOptions {
            build: BuildOptions {
                vocab_codec: Some(VocabCodec::Fsst),
                ..Default::default()
            },
            ..Default::default()
        },
    )
    .unwrap();
    let mut data = String::from(
        "@prefix rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#> .\n\
         @prefix x: <http://x.org/> .\n\
         @prefix z: <http://z.org/> .\n\
         x:hr { z:s z:p z:o, rdf:first, rdf:nil, rdf:rest, rdf:type",
    );
    for i in 0..(fc - 4) {
        data.push_str(&format!(
            ", <http://www.w3.org/1999/02/22-rdf-syntax-ns#z{i:02}>"
        ));
    }
    data.push_str(&format!(", x:a, <http://x.org/b{long}>"));
    for i in 0..(fc - 3) {
        data.push_str(&format!(", <http://x.org/z{i:02}>"));
    }
    data.push_str(&format!(" . }}\n<{long_graph}> {{ z:s z:p z:o . }}"));
    ds.load_str(&data, RdfFormat::TriG).unwrap();
    ds.store().compact().unwrap();
    ds.reasoning()
        .run(&ReasonRequest {
            profile: Profile::Rules(
                "@prefix z: <http://z.org/> . [derive: (?x z:p ?y) -> (?x z:q ?y)]".into(),
            ),
            inputs: Inputs {
                data_graphs: vec![GraphRef::Named(named_hr.into())],
                ..Default::default()
            },
            ..Default::default()
        })
        .unwrap();

    for (source, mode) in [
        (Some(ExplainSource::Materialized), ExplainMode::Why),
        (None, ExplainMode::Why),
        (Some(ExplainSource::Demand), ExplainMode::Why),
        (Some(ExplainSource::Demand), ExplainMode::WhyNot),
    ] {
        // A predicate with no applicable rules keeps demand's work to the graph lookup
        // and the setup needed to identify a miss.
        let demand = source == Some(ExplainSource::Demand);
        let predicate = if demand { "absent" } else { "q" };
        let mut req = ExplainRequest::new([z("s"), z(predicate), z("o")]);
        req.source = source;
        req.mode = mode;
        req.max_memory = Some(48 * 1024);
        let (result, alloc) = std::thread::scope(|scope| {
            scope
                .spawn(|| caller_allocations::measure(|| ds.reasoning().explain(&req)))
                .join()
                .unwrap()
        });
        assert!(
            alloc.largest < 65_536,
            "named graph {source:?}/{mode:?} decoded 64 KiB FSST suffix (peak={}, largest={})",
            alloc.peak,
            alloc.largest
        );
        let answer =
            result.unwrap_or_else(|error| panic!("named graph {source:?}/{mode:?}: {error:?}"));
        match (demand, mode) {
            (false, _) => assert!(matches!(answer.explained, Explained::Found { .. })),
            (true, ExplainMode::Why) => assert!(matches!(answer.explained, Explained::NotFound)),
            (true, ExplainMode::WhyNot) => assert!(answer.why_not.is_some()),
        }
    }

    for source in [ExplainSource::Materialized, ExplainSource::Demand] {
        let mut req = ExplainRequest::new([z("s"), z("q"), z("o")]);
        req.source = Some(source);
        req.cancel = Some(Arc::new(std::sync::atomic::AtomicBool::new(true)));
        let (result, alloc) = std::thread::scope(|scope| {
            scope
                .spawn(|| caller_allocations::measure(|| ds.reasoning().explain(&req)))
                .join()
                .unwrap()
        });
        assert!(
            alloc.largest < 65_536,
            "cancelled named graph {source:?} decoded 64 KiB FSST suffix (peak={}, largest={})",
            alloc.peak,
            alloc.largest
        );
        assert!(
            matches!(result, Err(sparkles::Error::Cancelled)),
            "cancelled named graph {source:?}: {result:?}"
        );
    }

    // The same FSST block has a graph whose own key needs more than the probe budget.
    let snap = ds.store().snapshot();
    let mut admitted = 0;
    let mut polls = 0;
    let stopped = snap.lookup_iri_controlled(
        &long_graph,
        &mut |bytes| {
            admitted = bytes;
            Ok(())
        },
        &mut || {
            polls += 1;
            if polls == 8 {
                Err(sparkles::Error::Cancelled)
            } else {
                Ok(())
            }
        },
    );
    assert_eq!(admitted, long_graph.len() + 1);
    assert!(matches!(stopped, Err(sparkles::Error::Cancelled)));

    ds.reasoning()
        .run(&ReasonRequest {
            profile: Profile::Rules(
                "@prefix z: <http://z.org/> . [derive: (?x z:p ?y) -> (?x z:q ?y)]".into(),
            ),
            inputs: Inputs {
                data_graphs: vec![GraphRef::Named(long_graph)],
                ..Default::default()
            },
            ..Default::default()
        })
        .unwrap();
    let mut req = ExplainRequest::new([z("s"), z("q"), z("o")]);
    req.source = Some(ExplainSource::Materialized);
    req.max_memory = Some(48 * 1024);
    let error = ds.reasoning().explain(&req).unwrap_err();
    assert!(
        matches!(&error, sparkles::Error::Component(c) if c.code == sparkles::reasoning::explain::REASONING_BUDGET && c.message.contains("looking up an explanation graph")),
        "{error:?}"
    );

    // 2. Inferred graph <urn:x-sparkles:inferred> sorts after a 64 KiB IRI in the same
    //    FSST block after compaction, covering explain() source selection and materialized_for.
    let dir2 = tempfile::tempdir().unwrap();
    let ds2 = Dataset::open_with(
        dir2.path().join("db"),
        StoreOptions {
            build: BuildOptions {
                vocab_codec: Some(VocabCodec::Fsst),
                ..Default::default()
            },
            ..Default::default()
        },
    )
    .unwrap();
    let mut data2 = format!(
        "@prefix z: <http://z.org/> .\n\
         z:s z:p z:o, <urn:x-sparkles:a>, <urn:x-sparkles:b{long}>"
    );
    // Block 0: z:o, z:p, z:q (from reasoning), z:s + (fc - 4) fillers => block 1 starts at <urn:x-sparkles:a>.
    for i in 0..(fc - 4) {
        data2.push_str(&format!(", <http://z.org/z{i:02}>"));
    }
    data2.push_str(" .\n");
    ds2.load_str(&data2, RdfFormat::Turtle).unwrap();
    ds2.reasoning()
        .run(&ReasonRequest {
            profile: Profile::Rules(
                "@prefix z: <http://z.org/> . [derive: (?x z:p ?y) -> (?x z:q ?y)]".into(),
            ),
            ..Default::default()
        })
        .unwrap();
    ds2.store().compact().unwrap();

    for source in [None, Some(ExplainSource::Materialized)] {
        let mut req = ExplainRequest::new([z("s"), z("q"), z("o")]);
        req.source = source;
        req.max_memory = Some(48 * 1024);
        let (result, alloc) = std::thread::scope(|scope| {
            scope
                .spawn(|| caller_allocations::measure(|| ds2.reasoning().explain(&req)))
                .join()
                .unwrap()
        });
        assert!(
            alloc.largest < 65_536,
            "inferred graph {source:?} decoded 64 KiB FSST suffix (peak={}, largest={})",
            alloc.peak,
            alloc.largest
        );
        let answer = result.unwrap_or_else(|error| panic!("inferred graph {source:?}: {error:?}"));
        assert!(matches!(answer.explained, Explained::Found { .. }));
    }
}

#[cfg(feature = "reasoning")]
#[test]
fn restricted_explanation_access_avoids_unchecked_graph_decode() {
    use sparkles::reasoning::{
        ReasonRequest,
        explain::ExplainMode,
        rdfs::NewSchema,
        reasoner::{GraphRef, Inputs, Profile},
    };
    use sparkles_core::{
        builder::{BuildOptions, VocabCodec},
        id::Tag,
        store::StoreOptions,
    };

    let z = |local: &str| Term::NamedNode(NamedNode::new(format!("http://z.org/{local}")).unwrap());
    let long = "z".repeat(65_536);

    // 1. Restricted public demand and why-not over a long 64 KiB delta graph name must
    //    check readability by known IRI name instead of decoding the ID via Snapshot::term.
    let long_graph = format!("http://x.org/g_{long}");
    let (_dir, ds) = open();
    ds.update(&format!(
        "INSERT DATA {{ GRAPH <{long_graph}> {{ <http://z.org/s> <http://z.org/p> <http://z.org/o> }} }}"
    ))
    .unwrap();
    assert_eq!(
        ds.store()
            .snapshot()
            .lookup_iri(&long_graph)
            .expect("long delta graph in snapshot")
            .tag(),
        Tag::Delta
    );
    ds.reasoning()
        .run(&ReasonRequest {
            profile: Profile::Rules(
                "@prefix z: <http://z.org/> . [derive: (?x z:p ?y) -> (?x z:q ?y)]".into(),
            ),
            inputs: Inputs {
                data_graphs: vec![GraphRef::Named(long_graph.clone())],
                ..Default::default()
            },
            ..Default::default()
        })
        .unwrap();

    // Denied long delta graph: public explain rejects it by known IRI name before probing
    // or decoding the 65,552-byte vocabulary key (`<` + long_graph).
    let key_len = long_graph.len() + 1;
    for (mode, triple) in [
        (ExplainMode::Why, [z("s"), z("q"), z("o")]),
        (ExplainMode::WhyNot, [z("s"), z("unrelated"), z("o")]),
    ] {
        let mut req = ExplainRequest::new(triple);
        req.source = Some(ExplainSource::Demand);
        req.mode = mode;
        req.access = only(&["default"]);
        let (result, alloc) = std::thread::scope(|scope| {
            scope
                .spawn(|| caller_allocations::measure(|| ds.reasoning().explain(&req)))
                .join()
                .unwrap()
        });
        let ans = result.expect("denied restricted demand should succeed");
        if mode == ExplainMode::Why {
            assert!(matches!(ans.explained, Explained::NotFound));
        }
        assert!(
            alloc.largest < key_len,
            "denied restricted {mode:?} probed or decoded 64 KiB delta graph vocab key (peak={}, largest={}, key_len={key_len})",
            alloc.peak,
            alloc.largest
        );
    }

    // Allowed long delta graph: public restricted explain must not add unchecked 64 KiB
    // Snapshot::term copies on top of unrestricted explain.
    for (mode, triple) in [
        (ExplainMode::Why, [z("s"), z("q"), z("missing")]),
        (ExplainMode::WhyNot, [z("s"), z("unrelated"), z("o")]),
    ] {
        let mut unres_req = ExplainRequest::new(triple.clone());
        unres_req.source = Some(ExplainSource::Demand);
        unres_req.mode = mode;
        let (unres_res, unres_alloc) = std::thread::scope(|scope| {
            scope
                .spawn(|| caller_allocations::measure(|| ds.reasoning().explain(&unres_req)))
                .join()
                .unwrap()
        });
        assert!(
            unres_res.is_ok(),
            "unrestricted {mode:?} failed: {unres_res:?}"
        );

        let mut req = ExplainRequest::new(triple);
        req.source = Some(ExplainSource::Demand);
        req.mode = mode;
        req.access = only(&[&long_graph]);
        let (result, alloc) = std::thread::scope(|scope| {
            scope
                .spawn(|| caller_allocations::measure(|| ds.reasoning().explain(&req)))
                .join()
                .unwrap()
        });
        assert!(
            result.is_ok(),
            "allowed restricted {mode:?} failed: {result:?}"
        );
        assert!(
            alloc.peak <= unres_alloc.peak + 32 * 1024,
            "allowed restricted {mode:?} spiked above unrestricted via readable_id (restricted_peak={}, unrestricted_peak={}, largest={})",
            alloc.peak,
            unres_alloc.peak,
            alloc.largest
        );
    }

    // Direct DemandInput check (excluding Dataset::query_reasoner JSON deserialization):
    // denied long delta graph allocates < 64 KiB; allowed stays under two live 64 KiB copies.
    let reasoner = ds.query_reasoner(None).unwrap();
    let snap = ds.store().snapshot();
    {
        use sparkles::reasoning::reasoner::{
            explain::{DemandInput, ExplainOptions, explain_demand},
            whynot::{WhyNotOptions, why_not},
        };
        let denied_input = DemandInput {
            reasoner: &reasoner,
            snap: snap.clone(),
            readable: only(&["default"]),
        };
        let (res, alloc) = std::thread::scope(|scope| {
            scope
                .spawn(|| {
                    caller_allocations::measure(|| {
                        explain_demand(
                            &denied_input,
                            &[z("s"), z("q"), z("o")],
                            &ExplainOptions::default(),
                        )
                    })
                })
                .join()
                .unwrap()
        });
        assert!(matches!(res, Ok(Explained::NotFound)));
        assert!(
            alloc.largest < 65_536 && alloc.peak < 65_536,
            "direct denied demand decoded 64 KiB delta graph (peak={}, largest={})",
            alloc.peak,
            alloc.largest
        );

        let allowed_input = DemandInput {
            reasoner: &reasoner,
            snap: snap.clone(),
            readable: only(&[&long_graph]),
        };
        let (res, alloc) = std::thread::scope(|scope| {
            scope
                .spawn(|| {
                    caller_allocations::measure(|| {
                        why_not(
                            &allowed_input,
                            &[z("s"), z("unrelated"), z("o")],
                            &WhyNotOptions::default(),
                        )
                    })
                })
                .join()
                .unwrap()
        });
        assert!(res.is_ok(), "direct allowed why_not failed: {res:?}");
        assert!(
            alloc.peak < 112 * 1024,
            "direct allowed why_not spiked to two live 64 KiB copies (peak={}, largest={})",
            alloc.peak,
            alloc.largest
        );
    }

    // Allowed long delta graph still derives the visible triple.
    let mut visible_req = ExplainRequest::new([z("s"), z("q"), z("o")]);
    visible_req.source = Some(ExplainSource::Demand);
    visible_req.access = only(&[&long_graph]);
    let visible_ans = ds.reasoning().explain(&visible_req).unwrap();
    assert_eq!(found(visible_ans.explained).len(), 1);

    // 2. RDFS on read with restricted access over an unrelated 64 KiB delta named graph
    //    must borrow the delta key instead of allocating owned copies via Snapshot::term.
    let (_dir_rdfs_delta, ds_rdfs_delta) = open();
    let unrelated_delta = format!("http://x.org/u_{long}");
    ds_rdfs_delta
        .update(&format!(
            "INSERT DATA {{ \
                <http://z.org/s> <{RDF_TYPE}> <http://z.org/Sub> . \
                GRAPH <{unrelated_delta}> {{ <http://z.org/a> <http://z.org/b> <http://z.org/c> }} \
            }}"
        ))
        .unwrap();
    ds_rdfs_delta
        .reasoning()
        .rdfs()
        .set(NewSchema::Triples(vec![oxrdf::Triple::new(
            NamedNode::new("http://z.org/Sub").unwrap(),
            oxrdf::vocab::rdfs::SUB_CLASS_OF.into_owned(),
            NamedNode::new("http://z.org/Super").unwrap(),
        )]))
        .unwrap();
    let mut rdfs_delta_req = ExplainRequest::new([z("s"), ty(), z("Super")]);
    rdfs_delta_req.access = Some(Arc::new(GraphAccess::graphs(
        Graphs::All.without_pattern("http://x.org/*"),
        Graphs::none(),
    )));
    let (result, alloc) = std::thread::scope(|scope| {
        scope
            .spawn(|| {
                caller_allocations::measure(|| ds_rdfs_delta.reasoning().explain(&rdfs_delta_req))
            })
            .join()
            .unwrap()
    });
    let ans = result.expect("RDFS on read with excluded delta graph should succeed");
    assert_eq!(ans.source, Some(ExplainSource::RdfsOnRead));
    assert_eq!(found(ans.explained).len(), 1);
    assert!(
        alloc.largest < 65_536 && alloc.peak < 65_536,
        "RDFS on read decoded excluded 64 KiB delta graph (peak={}, largest={})",
        alloc.peak,
        alloc.largest
    );

    // 3. RDFS on read with restricted access over a compacted FSST unrelated 64 KiB named graph.
    let fc = sparkles_core::vocab::FC_BLOCK;
    let dir_rdfs_fsst = tempfile::tempdir().unwrap();
    let ds_rdfs_fsst = Dataset::open_with(
        dir_rdfs_fsst.path().join("db"),
        StoreOptions {
            build: BuildOptions {
                vocab_codec: Some(VocabCodec::Fsst),
                ..Default::default()
            },
            ..Default::default()
        },
    )
    .unwrap();
    let mut fsst_data = String::from(
        "@prefix rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#> .\n\
         @prefix x: <http://x.org/> .\n\
         @prefix z: <http://z.org/> .\n\
         {\n  z:s rdf:type z:Sub, x:a",
    );
    for i in 0..(fc - 2) {
        fsst_data.push_str(&format!(", <http://x.org/z{i:02}>"));
    }
    fsst_data.push_str(&format!(
        " .\n}}\n<http://x.org/b{long}> {{ z:a z:b z:c . }}\n"
    ));
    ds_rdfs_fsst.load_str(&fsst_data, RdfFormat::TriG).unwrap();
    ds_rdfs_fsst.store().compact().unwrap();
    ds_rdfs_fsst
        .reasoning()
        .rdfs()
        .set(NewSchema::Triples(vec![oxrdf::Triple::new(
            NamedNode::new("http://z.org/Sub").unwrap(),
            oxrdf::vocab::rdfs::SUB_CLASS_OF.into_owned(),
            NamedNode::new("http://z.org/Super").unwrap(),
        )]))
        .unwrap();

    // Wildcard policy: must check unrelated FSST graph with admission and live controls.
    let wildcard_access = Some(Arc::new(GraphAccess::graphs(
        Graphs::All.without_iri("http://x.org/hidden"),
        Graphs::none(),
    )));
    let mut rdfs_budget_req = ExplainRequest::new([z("s"), ty(), z("Super")]);
    rdfs_budget_req.access = wildcard_access.clone();
    rdfs_budget_req.max_memory = Some(48 * 1024);
    let (result, alloc) = std::thread::scope(|scope| {
        scope
            .spawn(|| {
                caller_allocations::measure(|| ds_rdfs_fsst.reasoning().explain(&rdfs_budget_req))
            })
            .join()
            .unwrap()
    });
    assert!(
        alloc.largest < 65_536,
        "RDFS on read wildcard policy decoded 64 KiB FSST suffix (peak={}, largest={})",
        alloc.peak,
        alloc.largest
    );
    let err = result.map(|_| ()).unwrap_err();
    assert!(
        matches!(&err, sparkles::Error::Component(c) if c.code == sparkles::reasoning::explain::REASONING_BUDGET),
        "RDFS on read wildcard policy expected reasoning-budget, got {err:?}"
    );

    let mut rdfs_cancel_req = ExplainRequest::new([z("s"), ty(), z("Super")]);
    rdfs_cancel_req.access = wildcard_access;
    rdfs_cancel_req.cancel = Some(Arc::new(std::sync::atomic::AtomicBool::new(true)));
    let (result, alloc) = std::thread::scope(|scope| {
        scope
            .spawn(|| {
                caller_allocations::measure(|| ds_rdfs_fsst.reasoning().explain(&rdfs_cancel_req))
            })
            .join()
            .unwrap()
    });
    assert!(
        alloc.largest < 65_536,
        "cancelled RDFS on read decoded 64 KiB FSST suffix (peak={}, largest={})",
        alloc.peak,
        alloc.largest
    );
    assert!(
        matches!(result, Err(sparkles::Error::Cancelled)),
        "cancelled RDFS on read expected Cancelled, got {result:?}"
    );

    // Exact-only policy ("default" only): must skip unrelated FSST graphs completely.
    let mut rdfs_exact_req = ExplainRequest::new([z("s"), ty(), z("Super")]);
    rdfs_exact_req.access = only(&["default"]);
    rdfs_exact_req.max_memory = Some(48 * 1024);
    let (result, alloc) = std::thread::scope(|scope| {
        scope
            .spawn(|| {
                caller_allocations::measure(|| ds_rdfs_fsst.reasoning().explain(&rdfs_exact_req))
            })
            .join()
            .unwrap()
    });
    let ans = result.expect("exact-only RDFS on read should skip unrelated FSST graph");
    assert_eq!(ans.source, Some(ExplainSource::RdfsOnRead));
    assert_eq!(found(ans.explained).len(), 1);
    assert!(
        alloc.largest < 65_536,
        "exact-only RDFS on read decoded unrelated 64 KiB FSST suffix (peak={}, largest={})",
        alloc.peak,
        alloc.largest
    );
}

#[cfg(feature = "reasoning")]
#[test]
fn public_why_not_consistency_respects_budget_across_long_delta_imports() {
    use sparkles::reasoning::{
        QuerySetting,
        explain::ExplainMode,
        reasoner::{GraphRef, ImportMode, Inputs, whynot::WhyNot},
    };
    use sparkles_core::id::Tag;

    let long = "z".repeat(65_536);
    let imp1 = format!("http://imp.org/one_{long}");
    let imp2 = format!("http://imp.org/two_{long}");
    let imp3 = format!("http://imp.org/three_{long}");
    let owl_imports = "http://www.w3.org/2002/07/owl#imports";

    let (_dir, ds) = open();
    ds.update(&format!(
        "INSERT DATA {{ \
            <http://ex.org/ont> <{owl_imports}> <{imp1}>, <{imp2}>, <{imp3}> . \
            GRAPH <{imp1}> {{ <http://ex.org/a1> <http://ex.org/r> <http://ex.org/b1> }} \
            GRAPH <{imp2}> {{ <http://ex.org/a2> <http://ex.org/r> <http://ex.org/b2> }} \
            GRAPH <{imp3}> {{ <http://ex.org/a3> <http://ex.org/r> <http://ex.org/b3> }} \
        }}"
    ))
    .unwrap();
    let snap = ds.store().snapshot();
    for iri in [&imp1, &imp2, &imp3] {
        assert_eq!(snap.lookup_iri(iri).unwrap().tag(), Tag::Delta);
    }
    ds.set_query_reasoning(Some(QuerySetting {
        profile: Some("rules".into()),
        rules: Some("@prefix ex: <http://ex.org/> . [derive: (?x ex:p ?y) -> (?x ex:q ?y)]".into()),
        inputs: Some(
            serde_json::to_value(Inputs {
                data_graphs: vec![GraphRef::Default],
                imports: ImportMode::Dataset,
                ..Default::default()
            })
            .unwrap(),
        ),
        ..Default::default()
    }))
    .unwrap();

    // Headroom fits the outer why-not session plus its inner verification session resolving
    // one 64 KiB import name at a time, while excluding holding all three 64 KiB names live.
    let cap = 176 * 1024;
    assert!(cap < 3 * 65_536);

    for (label, access) in [
        ("unrestricted", None),
        (
            "restricted-all-imports",
            only(&["default", &imp1, &imp2, &imp3]),
        ),
    ] {
        let mut req = ExplainRequest::new([ex("s"), ex("q"), ex("o")]);
        req.mode = ExplainMode::WhyNot;
        req.max_missing = 1;
        req.max_memory = Some(cap);
        req.access = access;
        let (result, alloc) = std::thread::scope(|scope| {
            scope
                .spawn(|| caller_allocations::measure(|| ds.reasoning().explain(&req)))
                .join()
                .unwrap()
        });
        let ans = result.unwrap_or_else(|e| panic!("{label} public why-not failed: {e}"));
        let Some(WhyNot::NotEntailed { hypotheses, .. }) = ans.why_not else {
            panic!("{label} expected WhyNot::NotEntailed");
        };
        assert!(
            !hypotheses.is_empty(),
            "{label} expected a nonempty hypothesis to enter consistency checking"
        );
        assert!(
            hypotheses.iter().all(|h| h.inconsistent.is_none()),
            "{label} expected consistency checking to stop within budget, got {hypotheses:?}"
        );
        assert!(
            alloc.peak < 3 * 65_536,
            "{label} public why-not consistency pass buffered 64 KiB import names outside budget (peak={}, largest={}, cap={cap})",
            alloc.peak,
            alloc.largest
        );
    }

    // When access denies all three 64 KiB imports, the demand session treats them as
    // unresolved and must refuse before accumulating three 64 KiB retained warnings.
    let mut denied_req = ExplainRequest::new([ex("s"), ex("q"), ex("o")]);
    denied_req.mode = ExplainMode::WhyNot;
    denied_req.max_missing = 1;
    denied_req.max_memory = Some(cap);
    denied_req.access = only(&["default"]);
    let (result, alloc) = std::thread::scope(|scope| {
        scope
            .spawn(|| caller_allocations::measure(|| ds.reasoning().explain(&denied_req)))
            .join()
            .unwrap()
    });
    let err = result.map(|_| ()).unwrap_err();
    assert!(
        matches!(&err, sparkles::Error::Component(c) if c.code == sparkles::reasoning::explain::REASONING_BUDGET),
        "restricted-default-only expected reasoning-budget on unresolved 64 KiB warnings, got {err:?}"
    );
    assert!(
        alloc.peak < 3 * 65_536,
        "restricted-default-only exceeded budget before refusing unresolved 64 KiB warnings (peak={}, largest={}, cap={cap})",
        alloc.peak,
        alloc.largest
    );

    // With sufficient budget, consistency checking still completes normally.
    let mut uncapped = ExplainRequest::new([ex("s"), ex("q"), ex("o")]);
    uncapped.mode = ExplainMode::WhyNot;
    uncapped.max_missing = 1;
    let ans = ds.reasoning().explain(&uncapped).unwrap();
    let Some(WhyNot::NotEntailed { hypotheses, .. }) = ans.why_not else {
        panic!("uncapped expected WhyNot::NotEntailed");
    };
    assert!(!hypotheses.is_empty());
    assert!(
        hypotheses.iter().all(|h| h.inconsistent == Some(false)),
        "uncapped consistency checking should complete, got {hypotheses:?}"
    );
}

#[test]
fn exact_only_rdfs_access_includes_stored_default_graph_iri() {
    use sparkles::reasoning::rdfs::NewSchema;

    let (_dir, ds) = open();
    ds.load_str(
        "<http://ex.org/tom> <http://www.w3.org/1999/02/22-rdf-syntax-ns#type> <http://ex.org/Cat> <urn:x-arq:DefaultGraph> .\n",
        RdfFormat::NQuads,
    )
    .unwrap();
    let stored_id = ds
        .store()
        .snapshot()
        .lookup_iri("urn:x-arq:DefaultGraph")
        .expect("stored urn:x-arq:DefaultGraph graph must be interned");
    assert_ne!(stored_id, sparkles_core::id::Id::DEFAULT_GRAPH);

    ds.reasoning()
        .rdfs()
        .set(NewSchema::Triples(vec![oxrdf::Triple::new(
            NamedNode::new("http://ex.org/Cat").unwrap(),
            oxrdf::vocab::rdfs::SUB_CLASS_OF.into_owned(),
            NamedNode::new("http://ex.org/Animal").unwrap(),
        )]))
        .unwrap();

    for grant in ["default", "urn:x-arq:DefaultGraph"] {
        let mut req = ExplainRequest::new([ex("tom"), ty(), ex("Animal")]);
        req.access = only(&[grant]);
        let ans = ds.reasoning().explain(&req).unwrap();
        assert_eq!(
            ans.source,
            Some(ExplainSource::RdfsOnRead),
            "grant {grant} should explain RDFS derivation from stored urn:x-arq:DefaultGraph"
        );
        assert_eq!(found(ans.explained).len(), 1);
    }
}

#[cfg(feature = "reasoning")]
#[test]
fn demand_session_refuses_before_allocating_terms_table() {
    use sparkles::reasoning::{
        ReasonRequest,
        reasoner::{GraphRef, Inputs, Profile},
    };

    let (_dir, ds) = open();
    ds.load_str(
        "@prefix ex: <http://ex.org/> .\n\
         ex:hr { ex:s ex:p ex:o . }\n",
        RdfFormat::TriG,
    )
    .unwrap();
    ds.reasoning()
        .run(&ReasonRequest {
            profile: Profile::Rules(
                "@prefix ex: <http://ex.org/> . [derive: (?x ex:p ?y) -> (?x ex:q ?y)]".into(),
            ),
            inputs: Inputs {
                data_graphs: vec![GraphRef::Named(HR.into())],
                ..Default::default()
            },
            ..Default::default()
        })
        .unwrap();

    let mut req = ExplainRequest::new([ex("s"), ex("q"), ex("o")]);
    req.source = Some(ExplainSource::Demand);
    req.max_memory = Some(2048);
    let (res, alloc) = caller_allocations::measure(|| ds.reasoning().explain(&req));
    let err = res.map(|_| ()).unwrap_err();
    assert!(
        matches!(&err, sparkles::Error::Component(c) if c.code == sparkles::reasoning::explain::REASONING_BUDGET),
        "expected reasoning-budget on tiny demand session, got {err:?}"
    );
    assert!(
        alloc.peak < 16_384,
        "demand Session::new must use Terms::new_in and refuse before allocating LocalTable segment (peak_live={}, largest={})",
        alloc.peak,
        alloc.largest
    );
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

#[test]
fn rdfs_on_read_asserted_answer_succeeds_with_cold_and_warm_large_schema() {
    let (_dir, ds) = open();
    let graph_4k = format!("http://ex.org/g4k/{}", "g".repeat(4070));
    let mut nq = format!("<http://ex.org/s> <http://ex.org/p> <http://ex.org/o> <{graph_4k}> .\n");
    // Unrelated ontology graph whose distinct IRIs exceed 2 * 8192 + 256 = 16640 bytes.
    for i in 0..30 {
        let a = format!("http://ex.org/C{i}/{}", "a".repeat(1024));
        let b = format!("http://ex.org/D{i}/{}", "b".repeat(1024));
        nq.push_str(&format!(
            "<{a}> <http://www.w3.org/2000/01/rdf-schema#subClassOf> <{b}> <{ONTO}> .\n"
        ));
    }
    ds.load_str(&nq, RdfFormat::NQuads).unwrap();
    ds.store().compact().unwrap();
    ds.reasoning()
        .rdfs()
        .set(sparkles::reasoning::rdfs::NewSchema::Graph(ONTO.into()))
        .unwrap();

    // 1. Cold schema cache: fitting asserted answer in 4 KiB named graph must succeed inside 8 KiB.
    let mut req = ExplainRequest::new([ex("s"), ex("p"), ex("o")]);
    req.max_memory = Some(8192);
    let cold = ds
        .reasoning()
        .explain(&req)
        .expect("fitting asserted answer must succeed when schema cache is cold");
    let trees = found(cold.explained);
    assert_eq!(trees.len(), 1);
    assert_eq!(trees[0].graph.as_deref(), Some(graph_4k.as_str()));
    assert!(trees[0].heap_bytes() <= 8192);

    // 2. Warm the schema cache with an unbounded lookup, then verify warm cache succeeds too.
    let warm_probe = ExplainRequest::new([ex("missing"), ty(), ex("Thing")]);
    let _ = ds.reasoning().explain(&warm_probe).unwrap();
    let warm = ds
        .reasoning()
        .explain(&req)
        .expect("fitting asserted answer must succeed when schema cache is warm");
    let trees = found(warm.explained);
    assert_eq!(trees.len(), 1);
    assert_eq!(trees[0].graph.as_deref(), Some(graph_4k.as_str()));
}

#[cfg(feature = "reasoning")]
#[test]
fn rdfs_on_read_resolver_shares_workspace_with_proof_search() {
    use sparkles::reasoning::ReasonRequest;
    use sparkles::reasoning::reasoner::{GraphRef, Inputs};

    let (_dir, ds) = open();
    // 1800-byte schema graph name: a 2-step proof (~5.8 KiB) fits a 6144-byte answer limit,
    // and decoding the graph name alone (~3.6 KiB) fits the 12544-byte workspace.
    // Ten ~450-byte neighbours retained in Cx::chain (~10.4 KiB) also fit alone,
    // but neighbours + coexisting graph-name decode (~14 KiB) exceed 12544 bytes when
    // sharing one ExplainWorkspace across proof search and the dataset resolver.
    let schema_g = format!("http://ex.org/ont/{}", "s".repeat(1780));
    let mut nq =
        format!("<http://ex.org/hr_a> <http://ex.org/hr_p> <http://ex.org/hr_b> <{HR}> .\n");
    for i in 0..10 {
        let pad = format!("http://ex.org/N{i:02}/{}", "n".repeat(450));
        nq.push_str(&format!(
            "<http://ex.org/Start> <http://www.w3.org/2000/01/rdf-schema#subClassOf> <{pad}> <{schema_g}> .\n"
        ));
    }
    nq.push_str(&format!(
        "<http://ex.org/Start> <http://www.w3.org/2000/01/rdf-schema#subClassOf> <http://ex.org/Mid> <{schema_g}> .\n\
         <http://ex.org/Mid> <http://www.w3.org/2000/01/rdf-schema#subClassOf> <http://ex.org/End> <{schema_g}> .\n"
    ));
    ds.load_str(&nq, RdfFormat::NQuads).unwrap();
    ds.store().compact().unwrap();

    // Run materialized reasoning so ds.reasoning_record().is_some() activates the dataset resolver.
    let reason_req = ReasonRequest {
        inputs: Inputs {
            data_graphs: vec![GraphRef::Named(HR.into())],
            ontology_graphs: vec![],
            ..Default::default()
        },
        ..Default::default()
    };
    ds.reasoning().run(&reason_req).unwrap();
    ds.reasoning()
        .rdfs()
        .set(sparkles::reasoning::rdfs::NewSchema::Graph(
            schema_g.clone(),
        ))
        .unwrap();

    let triple = [
        ex("Start"),
        Term::NamedNode(NamedNode::new_unchecked(
            "http://www.w3.org/2000/01/rdf-schema#subClassOf",
        )),
        ex("End"),
    ];

    // Warm schema cache and confirm the 2-step proof fits under 6144 bytes of answer memory.
    let unbounded = ds
        .reasoning()
        .explain(&ExplainRequest::new(triple.clone()))
        .unwrap();
    let trees = found(unbounded.explained);
    assert!(
        trees[0].heap_bytes() <= 6144,
        "2-step proof should fit 6144 answer budget, got {} bytes",
        trees[0].heap_bytes()
    );

    // With max_memory = 6144 (workspace 12544), retained BFS neighbours + shared resolver
    // graph-name decode must exceed the shared workspace cap and return reasoning-budget.
    let mut tight = ExplainRequest::new(triple);
    tight.max_memory = Some(6144);
    let err = ds.reasoning().explain(&tight).map(|_| ()).unwrap_err();
    assert!(
        matches!(&err, sparkles::Error::Component(c) if c.code == sparkles::reasoning::explain::REASONING_BUDGET),
        "shared workspace across proof search and resolver must fail with reasoning-budget, got {err:?}"
    );
}

#[test]
fn rdfs_on_read_explain_accounts_for_nested_triple_terms_and_borrowed_schema() {
    // 1. Domain proof over an uncompacted delta object containing a 12 KiB nested triple
    //    term in a 6 KiB delta graph with an 8 KiB answer limit (16,640-byte workspace):
    //    workspace hold on the object + 6 KiB graph-name decode exceeds 16,640 bytes, and
    //    the final answer also exceeds 8 KiB.
    {
        let (_dir, ds) = open();
        ds.load_str(
            "<http://ex.org/seed> <http://ex.org/seedp> <http://ex.org/seedo> .\n",
            RdfFormat::Turtle,
        )
        .unwrap();
        ds.store().compact().unwrap();
        let g_6k = format!("http://ex.org/g6k/{}", "g".repeat(6144));
        let lit_12k = "t".repeat(12_288);
        let nq = format!(
            "<http://ex.org/s> <http://ex.org/p> <<( <http://ex.org/a> <http://ex.org/b> \"{lit_12k}\" )>> <{g_6k}> .\n"
        );
        ds.load_str(&nq, RdfFormat::NQuads).unwrap();
        let snap = ds.store().snapshot();
        assert_eq!(
            snap.lookup_iri(&g_6k).unwrap().tag(),
            sparkles_core::id::Tag::Delta
        );
        let domain_schema = vec![oxrdf::Triple::new(
            NamedNode::new_unchecked("http://ex.org/p"),
            NamedNode::new_unchecked("http://www.w3.org/2000/01/rdf-schema#domain"),
            NamedNode::new_unchecked("http://ex.org/C"),
        )];
        ds.reasoning()
            .rdfs()
            .set(sparkles::reasoning::rdfs::NewSchema::Triples(domain_schema))
            .unwrap();

        let mut req = ExplainRequest::new([ex("s"), ty(), ex("C")]);
        req.max_memory = Some(8192);
        let err = ds.reasoning().explain(&req).map(|_| ()).unwrap_err();
        assert!(
            matches!(&err, sparkles::Error::Component(c) if c.code == sparkles::reasoning::explain::REASONING_BUDGET),
            "12 KiB nested triple object + 6 KiB delta graph must fail with reasoning-budget, got {err:?}"
        );
    }

    // 2. Same 12 KiB nested triple term in the default graph (short graph name): workspace
    //    passes the decode, and the final Derivation::heap_bytes() check must still refuse
    //    for an 8 KiB answer limit.
    {
        let (_dir, ds) = open();
        ds.load_str(
            "<http://ex.org/seed> <http://ex.org/seedp> <http://ex.org/seedo> .\n",
            RdfFormat::Turtle,
        )
        .unwrap();
        ds.store().compact().unwrap();
        let lit_12k = "u".repeat(12_288);
        let ttl = format!(
            "<http://ex.org/s> <http://ex.org/p> <<( <http://ex.org/a> <http://ex.org/b> \"{lit_12k}\" )>> .\n"
        );
        ds.load_str(&ttl, RdfFormat::Turtle).unwrap();
        let domain_schema = vec![oxrdf::Triple::new(
            NamedNode::new_unchecked("http://ex.org/p"),
            NamedNode::new_unchecked("http://www.w3.org/2000/01/rdf-schema#domain"),
            NamedNode::new_unchecked("http://ex.org/C"),
        )];
        ds.reasoning()
            .rdfs()
            .set(sparkles::reasoning::rdfs::NewSchema::Triples(domain_schema))
            .unwrap();

        let mut req = ExplainRequest::new([ex("s"), ty(), ex("C")]);
        req.max_memory = Some(8192);
        let err = ds.reasoning().explain(&req).map(|_| ()).unwrap_err();
        assert!(
            matches!(&err, sparkles::Error::Component(c) if c.code == sparkles::reasoning::explain::REASONING_BUDGET),
            "final Derivation::heap_bytes() must include nested triple literal payload, got {err:?}"
        );
    }

    // 3. Warm graph-backed schema with 30 distinct 1 KiB subclasses of BigClass and 30
    //    distinct 1 KiB subproperties of bigProp: explaining a missing subject under a 4 KiB
    //    answer limit borrows the cached sets without cloning 30 KiB vectors.
    {
        let (_dir, ds) = open();
        let mut nq = String::new();
        for i in 0..30 {
            let sub_c = format!("http://ex.org/SubC{i:02}/{}", "c".repeat(1024));
            let sub_p = format!("http://ex.org/subP{i:02}/{}", "p".repeat(1024));
            nq.push_str(&format!(
                "<{sub_c}> <http://www.w3.org/2000/01/rdf-schema#subClassOf> <http://ex.org/BigClass> <{ONTO}> .\n\
                 <{sub_p}> <http://www.w3.org/2000/01/rdf-schema#subPropertyOf> <http://ex.org/bigProp> <{ONTO}> .\n"
            ));
        }
        ds.load_str(&nq, RdfFormat::NQuads).unwrap();
        ds.store().compact().unwrap();
        ds.reasoning()
            .rdfs()
            .set(sparkles::reasoning::rdfs::NewSchema::Graph(ONTO.into()))
            .unwrap();

        // Warm the schema cache.
        let _ = ds
            .reasoning()
            .explain(&ExplainRequest::new([ex("missing"), ty(), ex("BigClass")]))
            .unwrap();

        let mut req_type = ExplainRequest::new([ex("missing"), ty(), ex("BigClass")]);
        req_type.max_memory = Some(4096);
        let ans_type = ds.reasoning().explain(&req_type).unwrap();
        assert!(matches!(ans_type.explained, Explained::NotFound));

        let mut req_prop = ExplainRequest::new([ex("missing"), ex("bigProp"), ex("o")]);
        req_prop.max_memory = Some(4096);
        let ans_prop = ds.reasoning().explain(&req_prop).unwrap();
        assert!(matches!(ans_prop.explained, Explained::NotFound));
    }
}
