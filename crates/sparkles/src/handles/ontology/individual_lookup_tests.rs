//! Named participants of incoming assertions must open through the entity accessor.
use super::*;
use crate::access::{GraphAccess, GraphRule, Graphs};

const PREFIXES: &str = "PREFIX : <http://ex.org/> PREFIX owl: <http://www.w3.org/2002/07/owl#>";
fn ex(local: &str) -> String {
    format!("http://ex.org/{local}")
}
fn dataset(body: &str) -> Dataset {
    let ds = Dataset::memory();
    ds.update(&format!("{PREFIXES} INSERT DATA {{ {body} }}"))
        .unwrap();
    ds
}

#[test]
fn incoming_individual_lookup_matches_mapped_assertions_and_punning() {
    let ds = dataset(
        ":p a owl:ObjectProperty . :C a owl:Class . :note a owl:AnnotationProperty . :a :p :object, :C ; :note :annotationOnly . :unknownSubject :unknown :unknownObject . :C :undeclaredAnnotation :annotationTarget .",
    );
    let o = ds
        .ontology()
        .open(&Default::default(), &Default::default())
        .unwrap();
    for local in ["object", "C", "unknownObject"] {
        assert!(
            o.named_individual(&ex(local)).is_ok(),
            "incoming assertion participant {local}"
        );
    }
    assert!(o.class(&ex("C")).is_ok());
    for local in ["annotationOnly", "annotationTarget", "p", "unknown"] {
        assert!(
            o.named_individual(&ex(local)).is_err(),
            "not an asserted individual {local}"
        );
    }
    let strict = ds
        .ontology()
        .open(
            &OntologyRequest {
                mode: Mode::Strict,
                ..Default::default()
            },
            &Default::default(),
        )
        .unwrap();
    assert!(strict.named_individual(&ex("object")).is_ok());
    assert!(strict.named_individual(&ex("unknownObject")).is_err());
}

#[test]
fn incoming_individual_lookup_keeps_graph_grants_retained_revisions_and_branches() {
    let ds = dataset(
        ":p a owl:ObjectProperty . :a :p :public . GRAPH :secret { :p a owl:ObjectProperty . :hidden :p :private . }",
    );
    ds.snapshots()
        .create("lookup-before", &At::Head, &Default::default())
        .unwrap();
    let req = OntologyRequest {
        graphs: vec![GraphRef::Default, GraphRef::parse(&ex("secret")).unwrap()],
        ..Default::default()
    };
    let allowed = Graphs::Only(GraphRule::new(["default"], &[]));
    let opts = QueryOptions {
        graphs: Some(Arc::new(GraphAccess::graphs(allowed.clone(), allowed))),
        ..Default::default()
    };
    let scoped = ds.ontology().open(&req, &opts).unwrap();
    assert!(scoped.named_individual(&ex("public")).is_ok());
    assert!(scoped.named_individual(&ex("private")).is_err());
    let full = ds.ontology().open(&req, &Default::default()).unwrap();
    assert!(full.named_individual(&ex("private")).is_ok());
    let hidden_predicate = super::tests::hiding(&ex("p"));
    let hidden = ds.ontology().open(&req, &hidden_predicate).unwrap();
    assert!(hidden.named_individual(&ex("public")).is_err());
    ds.create_branch("lookup-dev", &Default::default()).unwrap();
    let dev = ds.branch("lookup-dev").unwrap();
    dev.update(&format!("{PREFIXES} INSERT DATA {{ :a :p :branchObject }}"))
        .unwrap();
    let branch = dev.ontology().open(&req, &Default::default()).unwrap();
    assert!(branch.named_individual(&ex("branchObject")).is_ok());
    assert!(
        ds.ontology()
            .open(&req, &Default::default())
            .unwrap()
            .named_individual(&ex("branchObject"))
            .is_err()
    );
    ds.update(&format!("{PREFIXES} INSERT DATA {{ :a :p :mainObject }}"))
        .unwrap();
    assert!(
        ds.ontology()
            .open(&req, &Default::default())
            .unwrap()
            .named_individual(&ex("mainObject"))
            .is_ok()
    );
    let retained = OntologyRequest {
        at: Some(At::Snapshot("lookup-before".into())),
        ..req
    };
    assert!(
        ds.ontology()
            .open(&retained, &Default::default())
            .unwrap()
            .named_individual(&ex("mainObject"))
            .is_err()
    );
}
