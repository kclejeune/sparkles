use super::*;
use crate::access::{GraphAccess, GraphRule, Graphs};
use sparkles_owl::{AxiomKind, Class, ClassExpression, Individual, ObjectProperty};

const PREFIXES: &str = "PREFIX : <http://ex.org/>
PREFIX owl: <http://www.w3.org/2002/07/owl#>
PREFIX rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#>
PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#>
PREFIX xsd: <http://www.w3.org/2001/XMLSchema#>
";

fn ex(local: &str) -> String {
    format!("http://ex.org/{local}")
}

fn class(local: &str) -> ClassExpression {
    ClassExpression::Class(Class::new(ex(local)))
}

fn dataset(body: &str) -> Dataset {
    let ds = Dataset::memory();
    ds.update(&format!("{PREFIXES} INSERT DATA {{ {body} }}"))
        .unwrap();
    ds
}

const BASE: &str = ":o a owl:Ontology .
    :A a owl:Class . :B a owl:Class . :C a owl:Class .
    :p a owl:ObjectProperty . :d a owl:DatatypeProperty .
    :A rdfs:subClassOf [ a owl:Restriction ; owl:onProperty :p ; owl:someValuesFrom :B ] .
    :a a :A . :a :p :b . :a :d 3 .";

fn some_ab() -> Axiom {
    Axiom::sub_class_of(
        class("A"),
        ClassExpression::some(ObjectProperty::new(ex("p")), class("B")),
    )
}

fn only(names: &[&str]) -> Arc<GraphAccess> {
    let g = Graphs::Only(GraphRule::new(names.iter().copied(), &[]));
    Arc::new(GraphAccess::graphs(g.clone(), g))
}

fn total(ds: &Dataset) -> u64 {
    ds.store().snapshot().count(Perm::Spo, &[]).unwrap()
}

fn opts() -> QueryOptions {
    QueryOptions::default()
}

#[test]
fn opens_the_terminology_and_streams_assertions() {
    let ds = dataset(BASE);
    let o = ds
        .ontology()
        .open(&OntologyRequest::default(), &opts())
        .unwrap();
    assert!(o.ontology.contains(&some_ab()));
    assert_eq!(o.ontology.header.iri.as_ref().unwrap().as_str(), ex("o"));
    // assertions stay in the store
    assert!(
        !o.ontology
            .axiom_set()
            .any(|a| a.axiom_type() == AxiomType::ClassAssertion)
    );
    let mut found = Vec::new();
    o.assertions(&opts(), |ax, _, _| {
        found.push(ax);
        Ok(true)
    })
    .unwrap();
    assert_eq!(found.len(), 3, "{found:?}");
    assert!(found.iter().all(Option::is_some));
    let s = o.summary();
    assert_eq!(s.axioms[&AxiomType::SubClassOf], 1);
    assert_eq!(s.entities[&EntityKind::Class], 3);
    assert_eq!(s.unmapped, 0);
}

#[test]
fn keeps_open_ontologies_until_a_commit() {
    let ds = dataset(BASE);
    let req = OntologyRequest::default();
    assert!(!ds.ontology().open(&req, &opts()).unwrap().cached);
    assert!(ds.ontology().open(&req, &opts()).unwrap().cached);
    ds.update(&format!("{PREFIXES} INSERT DATA {{ :D a owl:Class }}"))
        .unwrap();
    let o = ds.ontology().open(&req, &opts()).unwrap();
    assert!(!o.cached);
    assert!(
        o.ontology
            .contains(&Axiom::declaration(Entity::Class(Class::new(ex("D")))))
    );
}

#[test]
fn follows_imports_to_graphs() {
    let ds = Dataset::memory();
    ds.update(&format!(
        "{PREFIXES} INSERT DATA {{
            :o a owl:Ontology ; owl:imports <http://ex.org/g2> . :A a owl:Class .
            GRAPH <http://ex.org/g2> {{ <http://ex.org/g2> a owl:Ontology . :B a owl:Class . :A rdfs:subClassOf :B . }}
        }}"
    ))
    .unwrap();
    let o = ds
        .ontology()
        .open(&OntologyRequest::default(), &opts())
        .unwrap();
    assert_eq!(o.graphs.len(), 2);
    assert!(
        o.ontology
            .contains(&Axiom::sub_class_of(class("A"), class("B")))
    );
    let o = ds
        .ontology()
        .open(
            &OntologyRequest {
                imports: ImportMode::None,
                ..Default::default()
            },
            &opts(),
        )
        .unwrap();
    assert_eq!(o.graphs.len(), 1);
    assert!(
        !o.ontology
            .contains(&Axiom::sub_class_of(class("A"), class("B")))
    );
}

#[test]
fn reads_only_the_graphs_the_caller_may_read() {
    let ds = Dataset::memory();
    ds.update(&format!(
        "{PREFIXES} INSERT DATA {{
            GRAPH :open {{ :o a owl:Ontology ; owl:imports :secret . :A a owl:Class . }}
            GRAPH :secret {{ :secret a owl:Ontology . :B a owl:Class . :A rdfs:subClassOf :B . }}
        }}"
    ))
    .unwrap();
    let req = OntologyRequest {
        graphs: vec![GraphRef::Named(ex("open"))],
        ..Default::default()
    };
    let all = ds.ontology().open(&req, &opts()).unwrap();
    assert!(
        all.ontology
            .contains(&Axiom::sub_class_of(class("A"), class("B")))
    );
    let mut limited = opts();
    limited.graphs = Some(only(&["http://ex.org/open"]));
    let o = ds.ontology().open(&req, &limited).unwrap();
    assert!(
        !o.ontology
            .contains(&Axiom::sub_class_of(class("A"), class("B")))
    );
    assert!(!o.cached);
}

#[test]
fn removes_an_axiom_with_its_structure() {
    // spec F21 A3: add then remove restores the graph
    let ds = dataset(BASE);
    let before = total(&ds);
    let req = OntologyRequest::default();
    let added = Axiom::sub_class_of(
        class("C"),
        ClassExpression::and([
            class("A"),
            ClassExpression::all(ObjectProperty::new(ex("p")), class("B")),
        ]),
    );
    let e = ds
        .ontology()
        .edit(
            &req,
            &EditRequest {
                add: vec![added.clone()],
                ..Default::default()
            },
            &opts(),
        )
        .unwrap();
    assert_eq!((e.added, e.present, e.declared.len()), (1, 0, 0));
    assert!(e.inserted > 4);
    assert!(
        ds.ontology()
            .open(&req, &opts())
            .unwrap()
            .ontology
            .contains(&added)
    );
    let e = ds
        .ontology()
        .edit(
            &req,
            &EditRequest {
                remove: vec![added.clone()],
                ..Default::default()
            },
            &opts(),
        )
        .unwrap();
    assert_eq!((e.removed, e.kept), (1, 0));
    assert_eq!(total(&ds), before);
}

#[test]
fn keeps_shared_structures() {
    let ds = dataset(
        ":o a owl:Ontology . :A a owl:Class . :B a owl:Class . :C a owl:Class . :p a owl:ObjectProperty .
         _:r a owl:Restriction ; owl:onProperty :p ; owl:someValuesFrom :B .
         :A rdfs:subClassOf _:r . :C rdfs:subClassOf _:r .",
    );
    let req = OntologyRequest::default();
    let e = ds
        .ontology()
        .edit(
            &req,
            &EditRequest {
                remove: vec![some_ab()],
                ..Default::default()
            },
            &opts(),
        )
        .unwrap();
    assert_eq!(e.removed, 1);
    assert_eq!(e.deleted, 1);
    let o = ds.ontology().open(&req, &opts()).unwrap();
    assert!(o.ontology.contains(&Axiom::sub_class_of(
        class("C"),
        ClassExpression::some(ObjectProperty::new(ex("p")), class("B"))
    )));
    assert!(o.ontology.unmapped.is_empty());
}

#[test]
fn adds_declarations_and_reports_present_and_absent() {
    let ds = dataset(BASE);
    let req = OntologyRequest::default();
    let new = Axiom::sub_class_of(class("New"), class("A"));
    let assertion = Axiom::new(AxiomKind::ObjectPropertyAssertion(
        ObjectProperty::new(ex("p")).into(),
        Individual::named(ex("a")),
        Individual::named(ex("b")),
    ));
    let e = ds
        .ontology()
        .edit(
            &req,
            &EditRequest {
                add: vec![new.clone(), some_ab(), assertion.clone()],
                remove: vec![Axiom::sub_class_of(class("B"), class("C"))],
                ..Default::default()
            },
            &opts(),
        )
        .unwrap();
    assert_eq!(e.present, 2);
    assert_eq!(e.declared, vec![Entity::Class(Class::new(ex("New")))]);
    assert_eq!(e.added, 2);
    assert_eq!(e.absent.len(), 1);
    // an assertion the store holds is removed as one triple
    let e = ds
        .ontology()
        .edit(
            &req,
            &EditRequest {
                remove: vec![assertion],
                ..Default::default()
            },
            &opts(),
        )
        .unwrap();
    assert_eq!((e.removed, e.deleted), (1, 1));
    // an added axiom must be well formed
    let bad = Axiom::new(AxiomKind::DisjointClasses([class("A")].into()));
    assert!(
        ds.ontology()
            .edit(
                &req,
                &EditRequest {
                    add: vec![bad],
                    ..Default::default()
                },
                &opts()
            )
            .is_err()
    );
}

#[test]
fn edits_refuse_unwritable_graphs() {
    let ds = dataset(BASE);
    let mut limited = opts();
    limited.graphs = Some(only(&["http://ex.org/elsewhere"]));
    let r = ds.ontology().edit(
        &OntologyRequest::default(),
        &EditRequest {
            add: vec![Axiom::sub_class_of(class("B"), class("C"))],
            ..Default::default()
        },
        &limited,
    );
    assert!(r.is_err());
}

#[test]
fn checks_profiles_with_assertions() {
    let ds = dataset(BASE);
    let req = OntologyRequest::default();
    let r = ds
        .ontology()
        .check(
            &req,
            &CheckRequest {
                profiles: vec![Profile::El, Profile::Ql, Profile::Rl, Profile::Dl],
                ..Default::default()
            },
            &opts(),
        )
        .unwrap();
    let conforms: Vec<bool> = r.iter().map(|r| r.conforms).collect();
    // the existential superclass is outside RL
    assert_eq!(conforms, [true, true, false, true]);
    // an undeclared property in an assertion is outside DL in a strict read
    ds.update(&format!("{PREFIXES} INSERT DATA {{ :a :undeclared :b }}"))
        .unwrap();
    let strict = OntologyRequest {
        mode: Mode::Strict,
        ..Default::default()
    };
    let r = ds
        .ontology()
        .check(&strict, &CheckRequest::default(), &opts())
        .unwrap();
    assert!(!r[0].conforms);
    let r = ds
        .ontology()
        .check(
            &strict,
            &CheckRequest {
                assertions: false,
                ..Default::default()
            },
            &opts(),
        )
        .unwrap();
    assert!(r[0].conforms);
}

#[test]
fn parses_lists() {
    assert_eq!(
        parse_profiles("el, dl").unwrap(),
        [Profile::El, Profile::Dl]
    );
    assert_eq!(parse_profiles("all").unwrap().len(), 4);
    assert!(parse_profiles("xl").is_err());
    assert_eq!(parse_axiom_types("SubClassOf").unwrap().len(), 1);
    assert!(parse_axiom_types("Nope").is_err());
}

#[test]
fn removes_an_entity() {
    let ds = dataset(BASE);
    let req = OntologyRequest::default();
    let e = ds
        .ontology()
        .remove_entity(&req, &ex("B"), &opts())
        .unwrap();
    // the declaration and the restriction axiom
    assert_eq!(e.removed, 2, "{:?}", e.absent);
    let o = ds.ontology().open(&req, &opts()).unwrap();
    assert!(!o.ontology.axiom_set().any(|a| a.mentions(&ex("B"))));
    assert!(o.ontology.unmapped.is_empty());
    // an assertion about an individual goes too
    let e = ds
        .ontology()
        .remove_entity(&req, &ex("a"), &opts())
        .unwrap();
    assert_eq!(e.removed, 3);
    let none = ds
        .ontology()
        .remove_entity(&req, &ex("nobody"), &opts())
        .unwrap();
    assert_eq!(none.removed, 0);
}
