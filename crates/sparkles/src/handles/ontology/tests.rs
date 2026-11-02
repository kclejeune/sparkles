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

// ---- review regressions ----------------------------------------------------------

/// A caller from whom triples with predicate `p` are hidden.
fn hiding(p: &str) -> QueryOptions {
    use crate::access::triples::{Caller, Limits, Protection, Rule, TripleRules};
    QueryOptions {
        graphs: Some(Arc::new(GraphAccess::with_triples(
            Graphs::All,
            Graphs::All,
            TripleRules {
                rules: vec![Rule {
                    protection: Arc::new(Protection {
                        name: "hidden".into(),
                        predicates: Some(vec![p.into()]),
                        classes: None,
                        subclasses: false,
                        graphs: None,
                        pattern: None,
                        prefixes: Default::default(),
                        hide_inferences: false,
                    }),
                    read: Graphs::none(),
                    write: Graphs::none(),
                }],
                caller: Caller::default(),
                limits: Limits::default(),
            },
        ))),
        ..Default::default()
    }
}

fn assertion(p: &str, a: &str, b: &str) -> Axiom {
    AxiomKind::ObjectPropertyAssertion(
        sparkles_owl::ObjectPropertyExpression::Property(ObjectProperty::new(ex(p))),
        Individual::named(ex(a)),
        Individual::named(ex(b)),
    )
    .into()
}

#[test]
fn hidden_assertions_are_neither_present_nor_written() {
    let ds = dataset(BASE);
    let caller = hiding(&ex("p"));
    let req = OntologyRequest::default();
    let edit = |ax: Axiom| {
        ds.ontology().edit(
            &req,
            &EditRequest {
                add: vec![ax],
                ..Default::default()
            },
            &caller,
        )
    };
    // the stored :a :p :b and an absent :a :p :c answer alike
    let existing = edit(assertion("p", "a", "b"));
    let absent = edit(assertion("p", "a", "c"));
    assert!(existing.is_err(), "{existing:?}");
    assert!(absent.is_err(), "{absent:?}");
    assert_eq!(
        std::mem::discriminant(&existing.unwrap_err()),
        std::mem::discriminant(&absent.unwrap_err())
    );
}

#[test]
fn hidden_imports_are_not_followed() {
    let ds = Dataset::memory();
    ds.update(&format!(
        "{PREFIXES} INSERT DATA {{
            GRAPH :open {{ :o a owl:Ontology ; owl:imports :other . :A a owl:Class . }}
            GRAPH :other {{ :other a owl:Ontology . :B a owl:Class . :A rdfs:subClassOf :B . }}
        }}"
    ))
    .unwrap();
    let req = OntologyRequest {
        graphs: vec![GraphRef::Named(ex("open"))],
        ..Default::default()
    };
    let o = ds
        .ontology()
        .open(&req, &hiding("http://www.w3.org/2002/07/owl#imports"))
        .unwrap();
    assert_eq!(o.graphs.len(), 1, "{:?}", o.graphs);
    assert!(o.imports.is_empty(), "{:?}", o.imports);
    assert!(
        !o.ontology
            .contains(&Axiom::sub_class_of(class("A"), class("B")))
    );
}

#[test]
fn unreadable_references_do_not_keep_structures() {
    let ds = Dataset::memory();
    ds.update(&format!(
        "{PREFIXES} INSERT DATA {{
            GRAPH :open {{ :o a owl:Ontology . :A a owl:Class . :B a owl:Class .
                :p a owl:ObjectProperty .
                :A rdfs:subClassOf _:r . _:r a owl:Restriction ; owl:onProperty :p ;
                    owl:someValuesFrom :B . }}
            GRAPH :secret {{ :x :points _:r . }}
        }}"
    ))
    .unwrap();
    let req = OntologyRequest {
        graphs: vec![GraphRef::Named(ex("open"))],
        ..Default::default()
    };
    let mut limited = opts();
    limited.graphs = Some(only(&["http://ex.org/open"]));
    let e = ds
        .ontology()
        .edit(
            &req,
            &EditRequest {
                remove: vec![some_ab()],
                ..Default::default()
            },
            &limited,
        )
        .unwrap();
    assert_eq!((e.removed, e.kept, e.deleted), (1, 0, 4), "{e:?}");
}

#[test]
fn annotated_assertions_round_trip() {
    let ds = dataset(BASE);
    let req = OntologyRequest::default();
    let note = sparkles_owl::Annotation::new(
        sparkles_owl::AnnotationProperty::new(vocab::RDFS_COMMENT),
        sparkles_owl::AnnotationValue::Literal(sparkles_owl::Literal(
            oxrdf::Literal::new_simple_literal("seen"),
        )),
    );
    let mut ca: Axiom = AxiomKind::ClassAssertion(class("B"), Individual::named(ex("c"))).into();
    ca.annotations.insert(note.clone());
    let mut pa = assertion("p", "c", "a");
    pa.annotations.insert(note);
    let start = total(&ds);
    ds.ontology()
        .edit(
            &req,
            &EditRequest {
                add: vec![ca.clone(), pa.clone()],
                ..Default::default()
            },
            &opts(),
        )
        .unwrap();
    let o = ds.ontology().open(&req, &opts()).unwrap();
    assert!(o.ontology.contains(&ca), "{:?}", o.ontology.diagnostics);
    assert!(o.ontology.contains(&pa));
    let mut streamed = Vec::new();
    o.assertions(&opts(), |ax, _, _| {
        streamed.push(ax);
        Ok(true)
    })
    .unwrap();
    // the main triples of the annotated assertions are read with their annotations
    assert_eq!(streamed.len(), 3, "{streamed:?}");
    let before = total(&ds);
    let e = ds
        .ontology()
        .edit(
            &req,
            &EditRequest {
                remove: vec![ca, pa],
                ..Default::default()
            },
            &opts(),
        )
        .unwrap();
    assert_eq!(e.removed, 2, "{e:?}");
    assert_eq!(total(&ds), before - e.deleted as u64);
    assert_eq!(total(&ds), start);
}

#[test]
fn removes_the_main_triple_of_a_shared_subject() {
    let ds = dataset(
        ":o a owl:Ontology . :A a owl:Class . :B a owl:Class . :C a owl:Class . :D a owl:Class .
         _:x a owl:Class ; owl:intersectionOf ( :A :B ) ; rdfs:subClassOf :C .
         :D rdfs:subClassOf _:x .",
    );
    let req = OntologyRequest::default();
    let ab = ClassExpression::and([class("A"), class("B")]);
    let e = ds
        .ontology()
        .edit(
            &req,
            &EditRequest {
                remove: vec![Axiom::sub_class_of(ab.clone(), class("C"))],
                ..Default::default()
            },
            &opts(),
        )
        .unwrap();
    assert_eq!(e.removed, 1);
    let o = ds.ontology().open(&req, &opts()).unwrap();
    assert!(
        !o.ontology
            .contains(&Axiom::sub_class_of(ab.clone(), class("C")))
    );
    assert!(o.ontology.contains(&Axiom::sub_class_of(class("D"), ab)));
}

#[test]
fn removals_leave_imported_graphs_unless_named() {
    let ds = Dataset::memory();
    ds.update(&format!(
        "{PREFIXES} INSERT DATA {{
            GRAPH :local {{ :o a owl:Ontology ; owl:imports :lib . :A a owl:Class . }}
            GRAPH :lib {{ :lib a owl:Ontology . :B a owl:Class . :A rdfs:subClassOf :B . }}
        }}"
    ))
    .unwrap();
    let req = OntologyRequest {
        graphs: vec![GraphRef::Named(ex("local"))],
        ..Default::default()
    };
    let ab = Axiom::sub_class_of(class("A"), class("B"));
    let remove = |graph: Option<GraphRef>| {
        ds.ontology().edit(
            &req,
            &EditRequest {
                remove: vec![ab.clone()],
                graph,
                ..Default::default()
            },
            &opts(),
        )
    };
    let r = remove(None);
    assert!(matches!(r, Err(Error::Invalid(_))), "{r:?}");
    assert!(
        ds.ontology()
            .open(&req, &opts())
            .unwrap()
            .ontology
            .contains(&ab)
    );
    let e = remove(Some(GraphRef::Named(ex("lib")))).unwrap();
    assert_eq!(e.removed, 1);
    assert!(
        !ds.ontology()
            .open(&req, &opts())
            .unwrap()
            .ontology
            .contains(&ab)
    );
}

#[test]
fn n_ary_axioms_match_their_pairwise_triples() {
    let ds = dataset(BASE);
    let req = OntologyRequest::default();
    let before = total(&ds);
    let eq: Axiom =
        AxiomKind::EquivalentClasses([class("A"), class("B"), class("C")].into_iter().collect())
            .into();
    let inv: Axiom = AxiomKind::ObjectPropertyAssertion(
        sparkles_owl::ObjectPropertyExpression::InverseOf(ObjectProperty::new(ex("p"))),
        Individual::named(ex("b")),
        Individual::named(ex("a")),
    )
    .into();
    let e = ds
        .ontology()
        .edit(
            &req,
            &EditRequest {
                add: vec![eq.clone(), inv.clone()],
                ..Default::default()
            },
            &opts(),
        )
        .unwrap();
    // `:a :p :b` is BASE's
    assert_eq!((e.added, e.present), (1, 1), "{e:?}");
    let e = ds
        .ontology()
        .edit(
            &req,
            &EditRequest {
                remove: vec![eq],
                ..Default::default()
            },
            &opts(),
        )
        .unwrap();
    assert_eq!((e.removed, e.absent.len()), (1, 0), "{e:?}");
    assert_eq!(total(&ds), before);
}

#[test]
fn a_stopped_stream_stops() {
    let ds = dataset(BASE);
    let o = ds
        .ontology()
        .open(&OntologyRequest::default(), &opts())
        .unwrap();
    let mut n = 0;
    o.assertions(&opts(), |_, _, _| {
        n += 1;
        Ok(false)
    })
    .unwrap();
    assert_eq!(n, 1);
}

fn local_and_lib(local: &str, lib: &str) -> (Dataset, OntologyRequest) {
    let ds = Dataset::memory();
    ds.update(&format!(
        "{PREFIXES} INSERT DATA {{
            GRAPH :local {{ :o a owl:Ontology ; owl:imports :lib . {local} }}
            GRAPH :lib {{ :lib a owl:Ontology . {lib} }}
        }}"
    ))
    .unwrap();
    let req = OntologyRequest {
        graphs: vec![GraphRef::Named(ex("local"))],
        ..Default::default()
    };
    (ds, req)
}

fn remove_one(ds: &Dataset, req: &OntologyRequest, ax: Axiom) -> Result<EditOutcome> {
    ds.ontology().edit(
        req,
        &EditRequest {
            remove: vec![ax],
            ..Default::default()
        },
        &opts(),
    )
}

#[test]
fn a_repeated_graph_does_not_widen_the_removal_scope() {
    let (ds, mut req) = local_and_lib(
        ":A a owl:Class .",
        ":B a owl:Class . :A rdfs:subClassOf :B .",
    );
    req.graphs = vec![GraphRef::Named(ex("local")), GraphRef::Named(ex("local"))];
    let r = remove_one(&ds, &req, Axiom::sub_class_of(class("A"), class("B")));
    assert!(matches!(r, Err(Error::Invalid(_))), "{r:?}");
}

#[test]
fn a_reification_in_an_imported_graph_is_refused() {
    let (ds, req) = local_and_lib(
        ":p a owl:ObjectProperty . :a :p :b .",
        "[] a owl:Axiom ; owl:annotatedSource :a ; owl:annotatedProperty :p ;
            owl:annotatedTarget :b ; rdfs:comment \"from lib\" .",
    );
    let before = total(&ds);
    let o = ds.ontology().open(&req, &opts()).unwrap();
    let annotated = o
        .ontology
        .axiom_set()
        .find(|a| a.axiom_type() == AxiomType::ObjectPropertyAssertion)
        .cloned()
        .expect("the annotated assertion");
    let r = remove_one(&ds, &req, annotated);
    assert!(matches!(r, Err(Error::Invalid(_))), "{r:?}");
    assert_eq!(total(&ds), before);
}

#[test]
fn imported_assertions_are_refused_not_absent() {
    let (ds, req) = local_and_lib(
        ":p a owl:ObjectProperty . :A a owl:Class .",
        ":a :p :b . :a a :A .",
    );
    for ax in [
        assertion("p", "a", "b"),
        AxiomKind::ClassAssertion(class("A"), Individual::named(ex("a"))).into(),
    ] {
        let r = remove_one(&ds, &req, ax);
        assert!(matches!(r, Err(Error::Invalid(_))), "{r:?}");
    }
}

#[test]
fn additions_see_the_removals_of_the_same_edit() {
    let ds = dataset(
        ":o a owl:Ontology . :A a owl:Class . :B a owl:Class . :C a owl:Class .
         :A owl:equivalentClass :B . :B owl:equivalentClass :C .",
    );
    let req = OntologyRequest::default();
    let abc: Axiom =
        AxiomKind::EquivalentClasses([class("A"), class("B"), class("C")].into_iter().collect())
            .into();
    let ab: Axiom =
        AxiomKind::EquivalentClasses([class("A"), class("B")].into_iter().collect()).into();
    let e = ds
        .ontology()
        .edit(
            &req,
            &EditRequest {
                remove: vec![abc],
                add: vec![ab.clone()],
                ..Default::default()
            },
            &opts(),
        )
        .unwrap();
    assert_eq!((e.removed, e.added, e.present), (1, 1, 0), "{e:?}");
    let o = ds.ontology().open(&req, &opts()).unwrap();
    assert!(o.ontology.contains(&ab));
}

#[test]
fn shared_forms_keep_their_own_results() {
    let ds = dataset(BASE);
    let req = OntologyRequest::default();
    let ab: Axiom =
        AxiomKind::EquivalentClasses([class("A"), class("B")].into_iter().collect()).into();
    let abc: Axiom =
        AxiomKind::EquivalentClasses([class("A"), class("B"), class("C")].into_iter().collect())
            .into();
    let e = ds
        .ontology()
        .edit(
            &req,
            &EditRequest {
                remove: vec![ab, abc],
                ..Default::default()
            },
            &opts(),
        )
        .unwrap();
    assert_eq!((e.removed, e.absent.len()), (0, 2), "{e:?}");
}

#[test]
fn an_imported_copy_survives_a_local_removal() {
    let (ds, req) = local_and_lib(
        ":A a owl:Class . :B a owl:Class . :C a owl:Class .
         :A owl:equivalentClass :B . :B owl:equivalentClass :C .",
        ":A owl:equivalentClass :B .",
    );
    let abc: Axiom =
        AxiomKind::EquivalentClasses([class("A"), class("B"), class("C")].into_iter().collect())
            .into();
    let ab: Axiom =
        AxiomKind::EquivalentClasses([class("A"), class("B")].into_iter().collect()).into();
    let e = ds
        .ontology()
        .edit(
            &req,
            &EditRequest {
                remove: vec![abc],
                add: vec![ab],
                ..Default::default()
            },
            &opts(),
        )
        .unwrap();
    assert_eq!((e.removed, e.added, e.present), (1, 0, 1), "{e:?}");
}

/// A dataset on disk, whose history the store's diff reads, with `body` inserted.
fn on_disk(body: &str) -> (tempfile::TempDir, Dataset) {
    let dir = tempfile::tempdir().unwrap();
    let ds = Dataset::open(dir.path()).unwrap();
    if !body.is_empty() {
        ds.update(&format!("{PREFIXES} INSERT DATA {{ {body} }}"))
            .unwrap();
    }
    (dir, ds)
}

/// The head, with its state held: an in-memory dataset keeps the states its snapshots
/// hold.
fn head(ds: &Dataset) -> (At, Arc<Snapshot>) {
    let s = ds.store().snapshot();
    (At::Commit(s.commit), s)
}

fn rendered(changes: &[AxiomChange]) -> Vec<String> {
    changes.iter().map(|c| c.axiom.to_string()).collect()
}

#[test]
fn diffs_axioms_between_commits() {
    let (_dir, ds) = on_disk(BASE);
    let req = OntologyRequest::default();
    let (c0, _c0) = head(&ds);
    let new = Axiom::sub_class_of(class("C"), class("A"))
        .annotated(sparkles_owl::Annotation::comment("why"));
    ds.ontology()
        .edit(
            &req,
            &EditRequest {
                add: vec![new.clone()],
                remove: vec![some_ab()],
                ..Default::default()
            },
            &opts(),
        )
        .unwrap();
    let (c1, _c1) = head(&ds);
    let d = ds.ontology().diff(&req, &c0, &c1, false, &opts()).unwrap();
    assert_eq!(rendered(&d.added), [new.to_string()]);
    assert_eq!(rendered(&d.removed), [some_ab().to_string()]);
    assert_eq!(d.added[0].graph, "default");
    assert!(d.changed_quads > 0);
    // in the other order the sides swap
    let back = ds.ontology().diff(&req, &c1, &c0, false, &opts()).unwrap();
    assert_eq!(rendered(&back.removed), [new.to_string()]);
    // spec F21 A3: adding then removing leaves no difference
    ds.ontology()
        .edit(
            &req,
            &EditRequest {
                add: vec![some_ab()],
                remove: vec![new],
                ..Default::default()
            },
            &opts(),
        )
        .unwrap();
    let (h0, _h0) = head(&ds);
    let d = ds.ontology().diff(&req, &c0, &h0, true, &opts()).unwrap();
    assert!(d.added.is_empty() && d.removed.is_empty(), "{d:?}");
    // a change in a graph the ontology does not read opens nothing
    let (c2, _c2) = head(&ds);
    ds.update(
        "INSERT DATA { GRAPH <http://ex.org/other> { <http://ex.org/x> <http://ex.org/y> 1 } }",
    )
    .unwrap();
    let (h1, _h1) = head(&ds);
    let d = ds.ontology().diff(&req, &c2, &h1, true, &opts()).unwrap();
    assert_eq!(d.changed_quads, 0);
    assert!(d.added.is_empty() && d.removed.is_empty());
}

#[test]
fn diffs_assertions_by_their_triples() {
    let (_dir, ds) = on_disk(BASE);
    let req = OntologyRequest::default();
    let (c0, _c0) = head(&ds);
    ds.update(&format!(
        "{PREFIXES} DELETE DATA {{ :a :p :b }} ; INSERT DATA {{ :a :p :c }}"
    ))
    .unwrap();
    let (c1, _c1) = head(&ds);
    let d = ds.ontology().diff(&req, &c0, &c1, true, &opts()).unwrap();
    assert_eq!(rendered(&d.added), [assertion("p", "a", "c").to_string()]);
    assert_eq!(rendered(&d.removed), [assertion("p", "a", "b").to_string()]);
    // without assertions only the terminology is compared
    let d = ds.ontology().diff(&req, &c0, &c1, false, &opts()).unwrap();
    assert!(d.added.is_empty() && d.removed.is_empty());
    // an assertion that loses its annotation keeps its triple: the annotated axiom
    // goes and the plain one comes
    let noted = assertion("p", "a", "c").annotated(sparkles_owl::Annotation::comment("seen"));
    ds.ontology()
        .edit(
            &req,
            &EditRequest {
                add: vec![noted.clone()],
                ..Default::default()
            },
            &opts(),
        )
        .unwrap();
    let (c2, _c2) = head(&ds);
    let d = ds.ontology().diff(&req, &c1, &c2, true, &opts()).unwrap();
    assert_eq!(rendered(&d.added), [noted.to_string()]);
    assert_eq!(rendered(&d.removed), [assertion("p", "a", "c").to_string()]);
    let d = ds.ontology().diff(&req, &c2, &c1, true, &opts()).unwrap();
    assert_eq!(rendered(&d.removed), [noted.to_string()]);
    assert_eq!(rendered(&d.added), [assertion("p", "a", "c").to_string()]);
}

#[test]
fn diffs_only_what_the_caller_reads() {
    let (_dir, ds) = on_disk("");
    ds.update(&format!(
        "{PREFIXES} INSERT DATA {{ GRAPH :pub {{ :o a owl:Ontology . :A a owl:Class }} GRAPH :secret {{ :S a owl:Class }} }}"
    ))
    .unwrap();
    let (c0, _c0) = head(&ds);
    ds.update(&format!(
        "{PREFIXES} INSERT DATA {{ GRAPH :pub {{ :B a owl:Class }} GRAPH :secret {{ :T a owl:Class }} }}"
    ))
    .unwrap();
    let req = OntologyRequest {
        graphs: vec![GraphRef::Named(ex("pub")), GraphRef::Named(ex("secret"))],
        ..Default::default()
    };
    let o = QueryOptions {
        graphs: Some(only(&[&ex("pub")])),
        ..Default::default()
    };
    let (h2, _h2) = head(&ds);
    let d = ds.ontology().diff(&req, &c0, &h2, true, &o).unwrap();
    assert_eq!(
        rendered(&d.added),
        ["Declaration(Class(<http://ex.org/B>))"]
    );
    assert!(d.removed.is_empty());
}

#[test]
fn diffs_annotations_of_reversed_equalities() {
    let (_dir, ds) = on_disk(&format!("{BASE} :b owl:sameAs :a ."));
    let req = OntologyRequest::default();
    let (c0, _k0) = head(&ds);
    let plain: Axiom =
        AxiomKind::SameIndividual([Individual::named(ex("a")), Individual::named(ex("b"))].into())
            .into();
    let noted = plain
        .clone()
        .annotated(sparkles_owl::Annotation::comment("same"));
    ds.ontology()
        .edit(
            &req,
            &EditRequest {
                add: vec![noted.clone()],
                ..Default::default()
            },
            &opts(),
        )
        .unwrap();
    let (c1, _k1) = head(&ds);
    let d = ds.ontology().diff(&req, &c0, &c1, true, &opts()).unwrap();
    assert_eq!(rendered(&d.added), [noted.to_string()], "{d:?}");
    assert_eq!(rendered(&d.removed), [plain.to_string()], "{d:?}");
}

fn materialize(
    ds: &Dataset,
    profile: sparkles_reasoner::Profile,
) -> crate::reasoning::ReasonOutcome {
    ds.reasoning()
        .run(&crate::reasoning::ReasonRequest {
            profile,
            ..Default::default()
        })
        .unwrap()
}

fn inferred() -> OntologyRequest {
    OntologyRequest {
        inferences: true,
        ..Default::default()
    }
}

/// spec F21 §7: an inferred view reads the materialized closure while it is fresh,
/// says when it is stale, and reads the asserted axioms only without a materialization.
#[test]
fn inferred_views_read_the_materialized_closure() {
    let ds = dataset(
        ":o a owl:Ontology . :A a owl:Class . :B a owl:Class . :C a owl:Class .
        :A rdfs:subClassOf :B . :B rdfs:subClassOf :C .",
    );
    let a_c = Axiom::sub_class_of(class("A"), class("C"));
    // no materialization: the asserted axioms, and no inferences read
    let o = ds.ontology().open(&inferred(), &opts()).unwrap();
    assert!(o.inferences.is_none());
    assert!(!o.ontology.contains(&a_c));
    materialize(&ds, sparkles_reasoner::Profile::Rdfs);
    // asserted only unless asked
    let o = ds
        .ontology()
        .open(&OntologyRequest::default(), &opts())
        .unwrap();
    assert!(!o.ontology.contains(&a_c));
    assert!(o.inferences.is_none());
    let o = ds.ontology().open(&inferred(), &opts()).unwrap();
    assert!(o.ontology.contains(&a_c), "the closure is read");
    let i = o.inferences.clone().expect("inferences read");
    assert_eq!(i.profile, "rdfs");
    assert_eq!(i.stale, Some(false));
    assert_eq!(o.graph_name(i.graph), INFERRED_GRAPH);
    let origins: Vec<&str> = o
        .ontology
        .axioms
        .iter()
        .find(|(a, _)| **a == a_c)
        .map(|(_, g)| g.iter().map(|x| o.graph_name(x.graph)).collect())
        .unwrap();
    assert_eq!(origins, vec![INFERRED_GRAPH]);
    // a later change to the default graph makes the view stale
    ds.update(&format!("{PREFIXES} INSERT DATA {{ :D a owl:Class }}"))
        .unwrap();
    let o = ds.ontology().open(&inferred(), &opts()).unwrap();
    let i = o.inferences.expect("inferences read");
    assert_eq!(i.stale, Some(true));
    assert_eq!(i.commits_since, Some(1));
    // the JSON form says what was read
    let j = wire::call(
        &ds,
        "summary",
        &serde_json::json!({"inferences": true}),
        &opts(),
    )
    .unwrap();
    assert_eq!(j["inferences"]["included"], true, "{j}");
    assert_eq!(j["inferences"]["stale"], true, "{j}");
    // edits and diffs read the asserted axioms only
    let edit = EditRequest {
        add: vec![a_c.clone()],
        ..Default::default()
    };
    assert!(matches!(
        ds.ontology().edit(&inferred(), &edit, &opts()),
        Err(Error::Invalid(_))
    ));
    assert!(matches!(
        ds.ontology()
            .diff(&inferred(), &At::Commit(1), &At::Head, false, &opts()),
        Err(Error::Invalid(_))
    ));
}

/// An inferred view through a view that hides the inferred graph reads no inferences.
#[test]
fn inferred_views_follow_the_callers_view() {
    let ds = dataset(
        ":A a owl:Class . :B a owl:Class . :C a owl:Class .
        :A rdfs:subClassOf :B . :B rdfs:subClassOf :C .",
    );
    materialize(&ds, sparkles_reasoner::Profile::Rdfs);
    let hidden = QueryOptions {
        graphs: Some(only(&["default"])),
        ..Default::default()
    };
    let o = ds.ontology().open(&inferred(), &hidden).unwrap();
    assert!(o.inferences.is_none());
    assert!(
        !o.ontology
            .contains(&Axiom::sub_class_of(class("A"), class("C")))
    );
}

/// spec F21 A10: an OWL 2 RL run over a `DisjointUnion` completes and warns that one
/// axiom is outside OWL 2 RL; an RDFS run does not check.
#[test]
fn owl_rl_runs_warn_about_axioms_outside_rl() {
    let ds = dataset(
        ":o a owl:Ontology . :A a owl:Class . :B a owl:Class . :C a owl:Class .
        :A owl:disjointUnionOf ( :B :C ) . :x a :B .",
    );
    let out = materialize(&ds, sparkles_reasoner::Profile::OwlRl);
    let w: Vec<&String> = out
        .record
        .warnings
        .iter()
        .filter(|w| w.contains("outside OWL 2 RL"))
        .collect();
    assert_eq!(w.len(), 1, "{:?}", out.record.warnings);
    assert!(w[0].starts_with("1 axiom is outside OWL 2 RL"), "{}", w[0]);
    assert!(w[0].contains("DisjointUnion("), "{}", w[0]);
    assert!(out.report.warnings.contains(w[0]));
    let rdfs = materialize(&ds, sparkles_reasoner::Profile::Rdfs);
    assert!(
        !rdfs.record.warnings.iter().any(|w| w.contains("OWL 2 RL")),
        "{:?}",
        rdfs.record.warnings
    );
}

/// spec F21 §7: an axiom in OWL 2 RL whose rule the subset covers only up to a size is
/// named, from the gap table beside the rules.
#[test]
fn owl_rl_runs_warn_about_rules_the_subset_lacks() {
    let ds = dataset(
        ":o a owl:Ontology . :p a owl:ObjectProperty . :q a owl:ObjectProperty .
        :r a owl:ObjectProperty . :s a owl:ObjectProperty . :t a owl:ObjectProperty .
        :t owl:propertyChainAxiom ( :p :q :r :s ) .
        :u owl:propertyChainAxiom ( :p :q ) . :u a owl:ObjectProperty .",
    );
    let w = rl_warnings(ds.store(), &Default::default(), &opts()).unwrap();
    assert_eq!(w.len(), 1, "{w:?}");
    assert!(
        w[0].starts_with("1 axiom in OWL 2 RL needs rule prp-spo2 for more than 3"),
        "{}",
        w[0]
    );
    let clean =
        dataset(":o a owl:Ontology . :A a owl:Class . :B a owl:Class . :A rdfs:subClassOf :B .");
    assert!(
        rl_warnings(clean.store(), &Default::default(), &opts())
            .unwrap()
            .is_empty()
    );
}

/// The gap table names rules whose sizes up to its limit, and no larger one, are in the
/// rules file.
#[test]
fn the_gap_table_matches_the_rules() {
    let gaps = rl_gaps();
    assert_eq!(
        gaps.iter().map(|g| g.0).collect::<Vec<_>>(),
        vec!["prp-spo2", "prp-key"]
    );
    for (rule, max, what) in gaps {
        assert!(!what.is_empty());
        let sizes: Vec<usize> = sparkles_reasoner::OWL_RL_RULES
            .lines()
            .filter_map(|l| l.trim_start().strip_prefix(&format!("[{rule}-")))
            .filter_map(|l| l.split(':').next()?.parse().ok())
            .collect();
        assert_eq!(sizes.iter().max(), Some(&max), "{rule}: {sizes:?}");
    }
}

/// The JSON operations the bindings call answer as the HTTP routes do.
#[test]
fn json_operations_answer_in_the_route_forms() {
    let (_dir, ds) = on_disk(BASE);
    let call = |op: &str, a: serde_json::Value| wire::call(&ds, op, &a, &opts());
    let s = call("summary", serde_json::json!({})).unwrap();
    assert_eq!(s["axioms"]["SubClassOf"], 1, "{s}");
    assert!(s.get("inferences").is_none());
    let p = call("axioms", serde_json::json!({"type": "SubClassOf"})).unwrap();
    assert_eq!(p["total"], 1);
    assert_eq!(p["axioms"][0]["type"], "SubClassOf");
    assert_eq!(
        sparkles_owl::json::read_axiom(&p["axioms"][0]["axiom"]).unwrap(),
        some_ab()
    );
    assert!(matches!(
        call("axioms", serde_json::json!({"about": ex("nothing")})),
        Err(Error::NotFound(_))
    ));
    let c = call("check", serde_json::json!({"profiles": ["el", "ql"]})).unwrap();
    assert_eq!(c["reports"].as_array().unwrap().len(), 2);
    // axioms as functional-style strings with the dataset's prefixes
    ds.set_prefix("ex", "http://ex.org/").unwrap();
    // the snapshot keeps the state before the edit for the diff
    let (before, _keep) = head(&ds);
    let e = call(
        "edit",
        serde_json::json!({"add": ["SubClassOf(ex:B ex:C)"], "remove": [sparkles_owl::json::axiom(&some_ab())]}),
    )
    .unwrap();
    assert_eq!(e["removed"], 1, "{e}");
    assert_eq!(e["added"], 1, "{e}");
    let At::Commit(from) = before else {
        unreachable!()
    };
    let d = call("diff", serde_json::json!({"from": from})).unwrap();
    assert_eq!(d["addedCount"], 1, "{d}");
    assert_eq!(d["removedCount"], 1, "{d}");
    let r = call("removeEntity", serde_json::json!({"iri": ex("C")})).unwrap();
    assert!(r["removed"].as_u64().unwrap() >= 2, "{r}");
    for (op, a) in [
        ("summary", serde_json::json!({"nope": 1})),
        ("axioms", serde_json::json!({"limit": 0})),
        ("edit", serde_json::json!({})),
        ("edit", serde_json::json!({"add": ["SubClassOf(ex:A"]})),
        ("diff", serde_json::json!({})),
        ("nothing", serde_json::json!({})),
    ] {
        assert!(matches!(call(op, a), Err(Error::Invalid(_))), "{op}");
    }
}
