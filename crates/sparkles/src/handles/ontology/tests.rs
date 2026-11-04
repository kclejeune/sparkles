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
pub(super) fn hiding(p: &str) -> QueryOptions {
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

/// The warnings check the state the run reads under the writer lock: an axiom that a
/// commit adds after the run starts, before it takes the lock, is checked too.
#[test]
fn owl_rl_warnings_check_the_state_the_run_reads() {
    let ds = dataset(":o a owl:Ontology . :A a owl:Class . :B a owl:Class . :C a owl:Class .");
    let writer = ds.clone();
    let wrote = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let w = wrote.clone();
    let ctl = crate::task::Control {
        progress: crate::task::Progress::new(move |_, msg| {
            if msg == "loading triples" && !w.swap(true, std::sync::atomic::Ordering::SeqCst) {
                writer
                    .update(&format!(
                        "{PREFIXES} INSERT DATA {{ :A owl:disjointUnionOf ( :B :C ) . }}"
                    ))
                    .unwrap();
            }
        }),
        ..crate::task::Control::none()
    };
    let out = ds
        .reasoning()
        .run_with(
            &crate::reasoning::ReasonRequest {
                profile: sparkles_reasoner::Profile::OwlRl,
                ..Default::default()
            },
            &ctl,
        )
        .unwrap();
    assert!(wrote.load(std::sync::atomic::Ordering::SeqCst));
    assert!(
        out.report
            .warnings
            .iter()
            .any(|w| w.starts_with("1 axiom is outside OWL 2 RL")),
        "{:?}",
        out.report.warnings
    );
}

fn rl(ds: &Dataset) -> Vec<String> {
    let store = ds.store();
    rl_warnings(
        &store.snapshot(),
        &store.prefixes(),
        &Default::default(),
        &opts(),
    )
    .unwrap()
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
    let w = rl(&ds);
    assert_eq!(w.len(), 1, "{w:?}");
    assert!(
        w[0].starts_with("1 axiom in OWL 2 RL needs rule prp-spo2 for more than 3"),
        "{}",
        w[0]
    );
    let clean =
        dataset(":o a owl:Ontology . :A a owl:Class . :B a owl:Class . :A rdfs:subClassOf :B .");
    assert!(rl(&clean).is_empty());
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

// ---------------------------------------------- roadmap item 540: accessors ------

const ENTITIES: &str = ":o a owl:Ontology .
    :A a owl:Class . :B a owl:Class . :C a owl:Class . :D a owl:Class .
    :p a owl:ObjectProperty . :q a owl:ObjectProperty . :d a owl:DatatypeProperty .
    :A rdfs:subClassOf :B , [ a owl:Restriction ; owl:onProperty :p ; owl:someValuesFrom :C ] .
    :A owl:equivalentClass :D . :B owl:disjointWith :C .
    :A rdfs:label \"A\" , \"Ah\"@fr .
    :p rdfs:domain :A ; rdfs:range :B ; rdfs:subPropertyOf :q ; a owl:TransitiveProperty .
    :p owl:inverseOf :q .
    :d rdfs:domain :A ; rdfs:range xsd:integer ; a owl:FunctionalProperty .
    :a a :A , :B . :a :p :b . :a :d 3 . :c a :A .
    :e a owl:NamedIndividual , [ a owl:Restriction ; owl:onProperty :p ; owl:hasValue :a ] .";

#[test]
fn entity_accessors_answer_from_the_terminology() {
    let ds = dataset(ENTITIES);
    let o = ds.ontology().graph("default").open().unwrap();
    let onto = &o.ontology;
    let names = |cs: Vec<Class>| {
        cs.iter()
            .map(|c| c.as_str().to_string())
            .collect::<Vec<_>>()
    };
    assert_eq!(names(onto.classes()), [ex("A"), ex("B"), ex("C"), ex("D")]);
    assert_eq!(onto.object_properties().len(), 2);
    let a = o.class(&ex("A")).unwrap();
    assert_eq!(
        onto.super_classes(&a),
        vec![
            class("B"),
            ClassExpression::some(ObjectProperty::new(ex("p")), class("C"))
        ]
    );
    assert_eq!(onto.sub_classes(class("B")), vec![class("A")]);
    assert_eq!(onto.equivalent_classes(&a), vec![class("D")]);
    assert_eq!(onto.disjoint_classes(class("B")), vec![class("C")]);
    assert_eq!(onto.labels(&ex("A")).len(), 2);
    let p = o.object_property(&ex("p")).unwrap();
    assert_eq!(onto.object_property_domains(&p), vec![class("A")]);
    assert_eq!(onto.object_property_ranges(&p), vec![class("B")]);
    assert_eq!(
        onto.super_object_properties(&p),
        vec![ObjectProperty::new(ex("q")).into()]
    );
    assert_eq!(
        onto.inverse_object_properties(ObjectProperty::new(ex("q"))),
        vec![p.clone().into()]
    );
    assert_eq!(
        onto.object_property_characteristics(&p),
        vec![AxiomType::TransitiveObjectProperty]
    );
    let d = o.data_property(&ex("d")).unwrap();
    assert_eq!(onto.data_property_domains(&d), vec![class("A")]);
    assert!(onto.is_functional_data_property(&d));
    assert_eq!(onto.data_property_ranges(&d).len(), 1);
    // §2.5: an IRI not used as the asked kind is NotFound
    assert!(matches!(o.class(&ex("p")), Err(Error::NotFound(_))));
    assert!(matches!(o.class(&ex("nothing")), Err(Error::NotFound(_))));
    // the class assertion of a class expression is read with the terminology
    assert_eq!(onto.types(&Individual::named(ex("e"))).len(), 1);
}

#[test]
fn instances_types_and_property_assertions_stream_from_the_store() {
    let ds = dataset(ENTITIES);
    let o = ds.ontology().graph("default").open().unwrap();
    // assertions of named classes stay in the store
    assert!(o.ontology.instances(class("A")).is_empty());
    let mut instances = Vec::new();
    o.instances(&Class::new(ex("A")), &opts(), |i, _| {
        instances.push(i);
        Ok(true)
    })
    .unwrap();
    instances.sort();
    assert_eq!(
        instances,
        vec![Individual::named(ex("a")), Individual::named(ex("c"))]
    );
    // a stopped stream stops
    let mut first = 0;
    o.instances(&Class::new(ex("A")), &opts(), |_, _| {
        first += 1;
        Ok(false)
    })
    .unwrap();
    assert_eq!(first, 1);
    let a = Individual::named(ex("a"));
    let mut types = Vec::new();
    o.types_of(&a, &opts(), |c, _| {
        types.push(c);
        Ok(true)
    })
    .unwrap();
    types.sort();
    assert_eq!(types, vec![class("A"), class("B")]);
    let mut asserted = Vec::new();
    o.property_assertions(&a, &opts(), |ax, _| {
        asserted.push(ax.axiom_type());
        Ok(true)
    })
    .unwrap();
    asserted.sort();
    assert_eq!(
        asserted,
        vec![
            AxiomType::ObjectPropertyAssertion,
            AxiomType::DataPropertyAssertion
        ]
    );
    // an individual the store does not hold has none
    let mut none = 0;
    o.types_of(&Individual::named(ex("zz")), &opts(), |_, _| {
        none += 1;
        Ok(true)
    })
    .unwrap();
    assert_eq!(none, 0);
}

#[test]
fn entities_used_only_by_stored_assertions_are_found() {
    let ds = dataset(
        ":o a owl:Ontology . :alice a :Person ; :knows :bob ; :age 3 ; rdfs:seeAlso :x .
         :carol owl:sameAs :dave . :erin owl:differentFrom :carol .",
    );
    let o = ds
        .ontology()
        .graph("default")
        .mode(Mode::Lenient)
        .open()
        .unwrap();
    let kinds = |iri: &str| o.kinds(&ex(iri), &opts()).unwrap();
    assert!(o.class(&ex("Person")).is_ok());
    assert_eq!(kinds("Person"), vec![EntityKind::Class]);
    assert_eq!(kinds("knows"), vec![EntityKind::ObjectProperty]);
    assert_eq!(kinds("age"), vec![EntityKind::DataProperty]);
    for i in ["alice", "carol", "dave", "erin"] {
        assert!(o.named_individual(&ex(i)).is_ok(), "{i}");
    }
    assert!(o.class(&ex("alice")).is_err());
    assert!(o.object_property(&ex("age")).is_err());
    assert!(matches!(
        o.entity(EntityKind::Class, &ex("nothing"), &opts()),
        Err(Error::NotFound(_))
    ));
    // the equalities of an individual stream in both directions
    let mut seen = Vec::new();
    o.equalities(&Individual::named(ex("carol")), &opts(), |ax, _| {
        seen.push(ax.axiom_type());
        Ok(true)
    })
    .unwrap();
    seen.sort();
    assert_eq!(
        seen,
        vec![AxiomType::SameIndividual, AxiomType::DifferentIndividuals]
    );
    let body = wire::call(
        &ds,
        "entity",
        &serde_json::json!({"iri": ex("carol"), "assertions": true, "mode": "lenient"}),
        &opts(),
    )
    .unwrap();
    let carol = &body["namedIndividual"];
    assert_eq!(
        carol["sameIndividuals"].as_array().unwrap().len(),
        1,
        "{body}"
    );
    assert_eq!(carol["differentIndividuals"].as_array().unwrap().len(), 1);
}

#[test]
fn equalities_stream_once_and_fill_each_list_on_its_own() {
    let ds = dataset(
        ":o a owl:Ontology . :a owl:sameAs :b , :c . :b owl:sameAs :a .
         :a owl:differentFrom :d .",
    );
    let o = ds
        .ontology()
        .graph("default")
        .mode(Mode::Lenient)
        .open()
        .unwrap();
    let mut n = 0;
    o.equalities(&Individual::named(ex("a")), &opts(), |_, _| {
        n += 1;
        Ok(true)
    })
    .unwrap();
    // a sameAs b and its reverse are one axiom in one graph
    assert_eq!(n, 3);
    let body = wire::call(
        &ds,
        "entity",
        &serde_json::json!({"iri": ex("a"), "assertions": true, "mode": "lenient", "limit": 1}),
        &opts(),
    )
    .unwrap();
    let a = &body["namedIndividual"];
    assert_eq!(a["sameIndividuals"].as_array().unwrap().len(), 1, "{body}");
    assert_eq!(
        a["differentIndividuals"].as_array().unwrap().len(),
        1,
        "{body}"
    );
    assert_eq!(body["truncated"], true);
}

#[test]
fn streams_give_every_graph_of_an_assertion() {
    let held = ":o a owl:Ontology . :A a owl:Class . :p a owl:ObjectProperty .
        :a a owl:NamedIndividual , [ a owl:Restriction ; owl:onProperty :p ; owl:someValuesFrom :A ] .
        [] a owl:NegativePropertyAssertion ; owl:sourceIndividual :a ;
           owl:assertionProperty :p ; owl:targetIndividual :a .";
    let ds = dataset(&format!("GRAPH :g1 {{ {held} }} GRAPH :g2 {{ {held} }}"));
    let o = ds.ontology().graphs([ex("g1"), ex("g2")]).open().unwrap();
    let a = Individual::named(ex("a"));
    let mut graphs = Vec::new();
    o.types_of(&a, &opts(), |_, g| {
        graphs.push(g);
        Ok(true)
    })
    .unwrap();
    graphs.sort();
    assert_eq!(graphs, vec![0, 1]);
    let mut graphs = Vec::new();
    o.property_assertions(&a, &opts(), |_, g| {
        graphs.push(g);
        Ok(true)
    })
    .unwrap();
    graphs.sort();
    assert_eq!(graphs, vec![0, 1]);
}

#[test]
fn streams_read_only_the_graphs_of_the_ontology() {
    let ds = dataset(&format!(
        "GRAPH :g {{ {ENTITIES} }} GRAPH :other {{ :x a :A . }}"
    ));
    let o = ds.ontology().graph(&ex("g")).open().unwrap();
    let mut instances = Vec::new();
    o.instances(&Class::new(ex("A")), &opts(), |i, g| {
        assert_eq!(o.graph_name(g), ex("g"));
        instances.push(i);
        Ok(true)
    })
    .unwrap();
    assert_eq!(instances.len(), 2, "{instances:?}");
}

#[test]
fn the_scope_builds_reads_checks_and_edits() {
    let ds = dataset(BASE);
    let scope = ds.ontology().graph("default").imports(false);
    assert_eq!(scope.request().imports, ImportMode::None);
    let reports = scope.check(&[Profile::El]).unwrap();
    assert_eq!(reports.len(), 1);
    let done = scope
        .edit(|e| {
            e.remove(&some_ab())?
                .add(Axiom::sub_class_of(class("B"), class("C")))?
                .message("builder");
            Ok(())
        })
        .unwrap();
    assert_eq!(done.removed, 1);
    assert_eq!(done.added, 1);
    let o = scope.open().unwrap();
    assert_eq!(o.ontology.super_classes(class("B")), vec![class("C")]);
    // an axiom the forward mapping cannot write is refused before anything is written
    let head = ds.head_commit().seq;
    let bad = scope.edit(|e| {
        e.add(Axiom::disjoint_classes([class("A")]))?;
        Ok(())
    });
    assert!(matches!(bad, Err(Error::Invalid(_))), "{:?}", bad.err());
    assert_eq!(ds.head_commit().seq, head);
    // an invalid graph IRI is reported by the call that ends the scope
    assert!(matches!(
        ds.ontology().graph("not an iri").open(),
        Err(Error::Invalid(_))
    ));
    assert!(matches!(
        ds.ontology()
            .graph("default")
            .at(At::Commit(1))
            .edit(|_| Ok(())),
        Err(Error::Invalid(_))
    ));
}

#[test]
fn entity_operations_answer_in_the_route_forms() {
    let ds = dataset(ENTITIES);
    let call = |op: &str, a: serde_json::Value| wire::call(&ds, op, &a, &opts());
    let page = call(
        "entities",
        serde_json::json!({"kind": "Class,ObjectProperty", "limit": 3}),
    )
    .unwrap();
    assert_eq!(page["total"], 6, "{page}");
    assert_eq!(page["entities"].as_array().unwrap().len(), 3);
    assert_eq!(page["next"], 3);
    let a = call("entity", serde_json::json!({"iri": ex("A")})).unwrap();
    assert_eq!(a["kinds"], serde_json::json!(["Class"]));
    assert_eq!(
        a["class"]["superClasses"].as_array().unwrap().len(),
        2,
        "{a}"
    );
    assert_eq!(a["class"]["instances"], serde_json::json!([]));
    assert_eq!(a["labels"].as_array().unwrap().len(), 2);
    let streamed = call(
        "entity",
        serde_json::json!({"iri": ex("A"), "assertions": true, "limit": 1}),
    )
    .unwrap();
    assert_eq!(streamed["class"]["instances"].as_array().unwrap().len(), 1);
    assert_eq!(streamed["truncated"], true);
    let p = call(
        "entity",
        serde_json::json!({"iri": ex("p"), "kind": "ObjectProperty"}),
    )
    .unwrap();
    assert_eq!(
        p["objectProperty"]["characteristics"],
        serde_json::json!(["TransitiveObjectProperty"])
    );
    assert_eq!(p["objectProperty"]["domains"][0]["expression"], ex("A"));
    let i = call(
        "entity",
        serde_json::json!({"iri": ex("e"), "assertions": true}),
    )
    .unwrap();
    assert_eq!(
        i["namedIndividual"]["types"][0]["expression"]["type"], "ObjectHasValue",
        "{i}"
    );
    for (a, not_found) in [
        (serde_json::json!({"iri": ex("nothing")}), true),
        (serde_json::json!({"iri": ex("p"), "kind": "Class"}), true),
        (serde_json::json!({"iri": ex("A"), "kind": "Thing"}), false),
        (serde_json::json!({}), false),
    ] {
        let r = call("entity", a.clone());
        if not_found {
            assert!(matches!(r, Err(Error::NotFound(_))), "{a}");
        } else {
            assert!(matches!(r, Err(Error::Invalid(_))), "{a}");
        }
    }
    assert!(matches!(
        call("entities", serde_json::json!({"kind": "Thing"})),
        Err(Error::Invalid(_))
    ));
}

// OWL 2 EL classification (spec F24)

mod classification {
    use super::*;
    use crate::handles::ontology::classify::ClassifyRequest;
    use crate::handles::ontology::wire;
    use serde_json::json;
    use std::collections::BTreeSet;

    const ONTO: &str = ":partOf a owl:ObjectProperty .
        :Nucleus rdfs:subClassOf [ a owl:Restriction ; owl:onProperty :partOf ;
                                   owl:someValuesFrom :Cell ] .
        :CellPart owl:equivalentClass [ a owl:Restriction ; owl:onProperty :partOf ;
                                        owl:someValuesFrom :Cell ] .
        :Nucleus rdfs:subClassOf :Organelle .
        :Cell a owl:Class . :Organelle a owl:Class .";

    fn c(local: &str) -> Class {
        Class::new(ex(local))
    }

    fn named(g: &str) -> GraphRef {
        GraphRef::Named(g.to_string())
    }

    #[test]
    fn classifies_and_keeps_the_classification_between_commits() {
        let ds = dataset(ONTO);
        let h = ds.ontology();
        let req = OntologyRequest::default();
        let cr = ClassifyRequest::default();
        let a = h.classify(&req, &cr, &opts()).unwrap();
        let t = &a.classification.taxonomy;
        assert!(a.classification.consistent);
        assert!(a.complete());
        assert!(!a.reused);
        assert!(t.is_subclass(&c("Nucleus"), &c("CellPart")));
        // the same state answers from the kept classification
        let b = h.classify(&req, &cr, &opts()).unwrap();
        assert!(b.reused);
        assert!(Arc::ptr_eq(&a.classification, &b.classification));
        // a commit gives the kept classifier the changed axioms
        ds.update(&format!(
            "{PREFIXES} INSERT DATA {{ :CellPart rdfs:subClassOf :Part . :Cell a owl:Class . }}"
        ))
        .unwrap();
        let d = h.classify(&req, &cr, &opts()).unwrap();
        assert!(!d.reused);
        assert!(d.classification.stats.incremental);
        assert!(
            d.classification
                .taxonomy
                .is_subclass(&c("Nucleus"), &c("Part"))
        );
        // a restricted view never shares the kept classification
        let mut view = opts();
        view.graphs = Some(only(&["default"]));
        let e = h.classify(&req, &cr, &view).unwrap();
        assert!(!e.reused);
        assert!(!e.classification.stats.incremental);
        assert_eq!(e.classification.taxonomy, d.classification.taxonomy);
    }

    #[test]
    fn a_run_stopped_before_it_starts_keeps_the_classification_it_had() {
        let ds = dataset(ONTO);
        let h = ds.ontology();
        let req = OntologyRequest::default();
        let cr = ClassifyRequest::default();
        let a = h.classify(&req, &cr, &opts()).unwrap();
        assert!(
            a.classification
                .taxonomy
                .is_subclass(&c("Nucleus"), &c("Organelle"))
        );
        ds.update(&format!(
            "{PREFIXES} DELETE DATA {{ :Nucleus rdfs:subClassOf :Organelle . }}"
        ))
        .unwrap();
        // a run whose wait for threads ends at the deadline gives the saturation nothing
        crate::handles::ontology::classify::REFUSE_ADMISSION.with(|r| r.set(true));
        assert!(matches!(
            h.classify(&req, &cr, &opts()),
            Err(Error::Timeout)
        ));
        let b = h.classify(&req, &cr, &opts()).unwrap();
        assert!(!b.reused);
        assert!(
            !b.classification
                .taxonomy
                .is_subclass(&c("Nucleus"), &c("Organelle"))
        );
        assert!(
            b.classification
                .taxonomy
                .is_subclass(&c("Nucleus"), &c("CellPart"))
        );
    }

    #[test]
    fn refuses_inferences_and_a_target_it_reads() {
        let ds = dataset(ONTO);
        let h = ds.ontology();
        let req = OntologyRequest {
            inferences: true,
            ..Default::default()
        };
        assert!(matches!(
            h.classify(&req, &ClassifyRequest::default(), &opts()),
            Err(Error::Invalid(_))
        ));
        let read = OntologyRequest {
            graphs: vec![named("http://ex.org/onto")],
            ..Default::default()
        };
        let into_read = ClassifyRequest {
            target: Some(named("http://ex.org/onto")),
            ..Default::default()
        };
        assert!(matches!(
            h.classify(&read, &into_read, &opts()),
            Err(Error::Invalid(_))
        ));
        let into_default = ClassifyRequest {
            target: Some(GraphRef::Default),
            ..Default::default()
        };
        assert!(matches!(
            h.classify(&read, &into_default, &opts()),
            Err(Error::Invalid(_))
        ));
    }

    #[test]
    fn writes_the_taxonomy_with_provenance_and_replaces_it() {
        let ds = dataset(ONTO);
        let h = ds.ontology();
        let req = OntologyRequest::default();
        let target = "http://ex.org/inferred";
        let cr = ClassifyRequest {
            target: Some(named(target)),
            message: Some("classify".into()),
            ..Default::default()
        };
        let a = h.classify(&req, &cr, &opts()).unwrap();
        let w = a.written.as_ref().unwrap();
        assert!(w.receipt.committed);
        let tq = OntologyRequest {
            graphs: vec![named(target)],
            imports: ImportMode::None,
            ..Default::default()
        };
        let o = h.open(&tq, &opts()).unwrap();
        let sub = Axiom::sub_class_of(c("Nucleus"), c("CellPart"));
        assert!(o.ontology.contains(&sub));
        assert_eq!(
            o.ontology.header.iri.as_ref().map(|i| i.as_str()),
            Some(target)
        );
        let prov = |p: &str| {
            o.ontology
                .header
                .annotations
                .iter()
                .filter(|a| a.property.as_str() == format!("http://www.w3.org/ns/prov#{p}"))
                .count()
        };
        assert_eq!(prov("wasGeneratedBy"), 1);
        assert_eq!(prov("generatedAtTime"), 1);
        // a change and a second write remove what is no longer entailed
        ds.update(&format!(
            "{PREFIXES} DELETE DATA {{ :Nucleus rdfs:subClassOf :Organelle . }}"
        ))
        .unwrap();
        h.classify(&req, &cr, &opts()).unwrap();
        let o = h.open(&tq, &opts()).unwrap();
        assert!(
            !o.ontology
                .contains(&Axiom::sub_class_of(c("Nucleus"), c("Organelle")))
        );
        assert!(o.ontology.contains(&sub));
        assert_eq!(
            o.ontology
                .header
                .annotations
                .iter()
                .filter(|a| a.property.as_str().ends_with("wasGeneratedBy"))
                .count(),
            1
        );
        // the read graphs are unchanged
        let src = h.open(&req, &opts()).unwrap();
        assert!(!src.ontology.contains(&sub));
    }

    #[test]
    fn a_write_keeps_the_target_header_and_its_comments() {
        let ds = dataset(&format!(
            "{ONTO} GRAPH <http://ex.org/tax> {{ <http://ex.org/taxonomy> a owl:Ontology ; rdfs:comment \"mine\" . }}"
        ));
        let h = ds.ontology();
        let req = OntologyRequest::default();
        let tq = OntologyRequest {
            graphs: vec![named("http://ex.org/tax")],
            imports: ImportMode::None,
            ..Default::default()
        };
        let before = total(&ds);
        // a class the taxonomy lacks refuses the request before anything is written
        let missing = ClassifyRequest {
            target: Some(named("http://ex.org/tax")),
            about: Some("http://ex.org/Nobody".into()),
            ..Default::default()
        };
        assert!(matches!(
            h.classify(&req, &missing, &opts()),
            Err(Error::NotFound(_))
        ));
        assert_eq!(total(&ds), before);
        let cr = ClassifyRequest {
            target: Some(named("http://ex.org/tax")),
            ..Default::default()
        };
        for _ in 0..2 {
            h.classify(&req, &cr, &opts()).unwrap();
            let o = h.open(&tq, &opts()).unwrap();
            let header = &o.ontology.header;
            assert_eq!(
                header.iri.as_ref().map(|i| i.as_str()),
                Some("http://ex.org/taxonomy")
            );
            assert!(
                o.ontology.other_headers.is_empty(),
                "{:?}",
                o.ontology.other_headers
            );
            let values = |p: &str| -> Vec<String> {
                header
                    .annotations
                    .iter()
                    .filter(|a| a.property.as_str() == p)
                    .map(|a| format!("{:?}", a.value))
                    .collect()
            };
            assert_eq!(values("http://www.w3.org/ns/prov#wasGeneratedBy").len(), 1);
            let comments = values(sparkles_owl::vocab::RDFS_COMMENT);
            assert_eq!(comments.len(), 2, "{comments:?}");
            assert!(comments.iter().any(|c| c.contains("mine")), "{comments:?}");
        }
    }

    #[test]
    fn budgets_bound_a_run() {
        let ds = dataset(ONTO);
        let h = ds.ontology();
        let mut tight = opts();
        tight.max_memory_bytes = Some(100);
        let e = h
            .classify(
                &OntologyRequest::default(),
                &ClassifyRequest::default(),
                &tight,
            )
            .err()
            .unwrap();
        assert!(
            matches!(&e, Error::BudgetExceeded(b) if b.kind == crate::BudgetKind::Memory),
            "{e:?}"
        );
        // the kept classifier starts again after the failure
        let ok = h
            .classify(
                &OntologyRequest::default(),
                &ClassifyRequest::default(),
                &opts(),
            )
            .unwrap();
        assert!(
            ok.classification
                .taxonomy
                .is_subclass(&c("Nucleus"), &c("CellPart"))
        );
        let mut late = opts();
        late.deadline = Some(Instant::now());
        // a kept classification answers without work, so a past deadline does not stop it
        let kept = h
            .classify(
                &OntologyRequest::default(),
                &ClassifyRequest::default(),
                &late,
            )
            .unwrap();
        assert!(kept.reused);
        let fresh = dataset(ONTO);
        let r = fresh.ontology().classify(
            &OntologyRequest::default(),
            &ClassifyRequest::default(),
            &late,
        );
        assert!(matches!(r, Err(Error::Timeout)), "{:?}", r.err());
    }

    #[test]
    fn assertions_can_make_the_ontology_inconsistent() {
        let ds = dataset(&format!(
            "{ONTO} :Cell owl:disjointWith :Organelle . :x a :Cell , :Organelle ."
        ));
        let h = ds.ontology();
        let req = OntologyRequest::default();
        let tbox = h
            .classify(&req, &ClassifyRequest::default(), &opts())
            .unwrap();
        assert!(tbox.classification.consistent);
        let abox = ClassifyRequest {
            assertions: true,
            ..Default::default()
        };
        let all = h.classify(&req, &abox, &opts()).unwrap();
        assert!(!all.classification.consistent);
        assert!(!all.reused);
    }

    #[test]
    fn the_json_operation_answers_counts_nodes_and_a_class() {
        // a maximum of two needs counting, which the classifier leaves out
        let ds = dataset(&format!(
            "{ONTO} :X rdfs:subClassOf [ a owl:Restriction ; owl:onProperty :partOf ;
                                         owl:maxCardinality 2 ] ."
        ));
        let j = wire::call(
            &ds,
            "classify",
            &json!({"about": ex("Nucleus"), "nodes": true, "limit": 2}),
            &opts(),
        )
        .unwrap();
        assert_eq!(j["consistent"], json!(true));
        assert_eq!(j["complete"], json!(false));
        assert_eq!(j["excludedCount"], json!(1));
        assert!(
            j["excluded"][0]["rule"]
                .as_str()
                .unwrap()
                .contains("Profiles")
        );
        assert_eq!(j["nodes"].as_array().unwrap().len(), 2);
        assert_eq!(j["next"], json!(2));
        let parents: Vec<&str> = j["class"]["parentClasses"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|v| v.as_str())
            .collect();
        assert_eq!(parents, vec![ex("CellPart"), ex("Organelle")]);
        assert!(matches!(
            wire::call(&ds, "classify", &json!({"about": ex("Nope")}), &opts()),
            Err(Error::NotFound(_))
        ));
        assert!(matches!(
            wire::call(&ds, "classify", &json!({"profiles": []}), &opts()),
            Err(Error::Invalid(_))
        ));
    }

    /// The ontology of item 603's partial answers in Turtle: chains of cases whose common
    /// subsumers hold, over existential and universal restrictions and an inverse role,
    /// large enough that a deadline stops the disjunctive rules part way. The inconsistent
    /// one gives every element a successor in a union of two empty classes.
    pub(crate) fn cases(n: usize, inconsistent: bool) -> String {
        let mut s = String::from(":r a owl:ObjectProperty . :s a owl:ObjectProperty .\n");
        let some = |p: &str, c: &str| {
            format!("[ a owl:Restriction ; owl:onProperty {p} ; owl:someValuesFrom {c} ]")
        };
        for i in 0..n {
            let (b, d) = ((7 * i + 3) % n, (13 * i + 5) % n);
            s.push_str(&format!(
                ":C{i} a owl:Class ; rdfs:subClassOf [ owl:unionOf ( :B{i} :E{i} ) ] .
                 :B{i} rdfs:subClassOf :K{i} , :C{b} . :E{i} rdfs:subClassOf :K{i} .
                 :K{i} rdfs:subClassOf {} .
                 :E{i} rdfs:subClassOf [ a owl:Restriction ;
                     owl:onProperty [ owl:inverseOf :r ] ; owl:allValuesFrom :F{d} ] .
                 {} rdfs:subClassOf :G{b} .\n",
                some(":r", &format!(":C{d}")),
                some(":r", &format!(":F{i}")),
            ));
        }
        if inconsistent {
            s.push_str(&format!(
                "owl:Thing rdfs:subClassOf {} .
                 :N1 rdfs:subClassOf owl:Nothing . :N2 rdfs:subClassOf :N3 .
                 :N3 owl:disjointWith :N2 .\n",
                some(":s", "[ owl:unionOf ( :N1 :N2 ) ]"),
            ));
        }
        s
    }

    fn partial_request() -> ClassifyRequest {
        ClassifyRequest {
            partial: true,
            threads: 1,
            ..Default::default()
        }
    }

    /// The pairs of named classes the taxonomy puts one below the other.
    fn pairs(t: &sparkles_owl::classify::Taxonomy) -> BTreeSet<(String, String)> {
        let mut out = BTreeSet::new();
        for x in t.classes() {
            let Some(n) = t.node_of(x) else { continue };
            for a in t.ancestors(n) {
                for y in &t.node(a).classes {
                    out.insert((x.as_str().to_string(), y.as_str().to_string()));
                }
            }
        }
        out
    }

    /// Checks one answer under a deadline against the full one, in Rust and in its JSON,
    /// and answers whether it was partial and its consistency.
    fn check_answer(
        ds: &Dataset,
        part: &classify::Classified,
        full: &classify::Classified,
    ) -> (bool, &'static str) {
        let read = part.opened.view();
        let j = wire::classification_body(
            "ds",
            &wire::ClassificationView {
                read: &read,
                classification: &part.classification,
                excluded: &part.excluded,
                reused: part.reused,
                written: None,
            },
            &OntologyRequest::default(),
            &crate::owl::ofn::Renderer::new(ds.prefixes()),
            &wire::ClassifyQuery {
                limit: wire::MAX_LIMIT,
                ..Default::default()
            },
        )
        .unwrap();
        let cl = &part.classification;
        let got = pairs(&cl.taxonomy);
        let want = pairs(&full.classification.taxonomy);
        // every subsumption holds in an inconsistent ontology
        let everything = !full.classification.consistent;
        assert!(
            everything || got.is_subset(&want),
            "a partial answer has more"
        );
        assert_eq!(j["tier"], json!("disjunctive"));
        assert_eq!(j["partial"], json!(!cl.complete));
        assert_eq!(j["complete"], json!(part.complete()));
        assert_eq!(j["incompleteCount"], json!(cl.incomplete.len()));
        let consistency = j["consistency"].as_str().unwrap();
        if cl.complete {
            assert!(everything || got == want);
            assert!(part.complete());
            assert_eq!(j["incomplete"], json!([]));
            assert_ne!(consistency, "unknown");
            return (
                false,
                if full.classification.consistent {
                    "consistent"
                } else {
                    "inconsistent"
                },
            );
        }
        assert!(
            !part.complete(),
            "a partial answer is presented as complete"
        );
        let listed: Vec<&str> = j["incomplete"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        let open: BTreeSet<&str> = cl.incomplete.iter().map(|c| c.as_str()).collect();
        assert_eq!(listed.iter().copied().collect::<BTreeSet<_>>(), open);
        let contradiction = if full.classification.consistent {
            "inconsistent"
        } else {
            "consistent"
        };
        assert_ne!(consistency, contradiction);
        for x in full.classification.taxonomy.classes() {
            if *x == Class::thing() || *x == Class::nothing() {
                continue;
            }
            if consistency == "unknown" {
                assert!(
                    open.contains(x.as_str()),
                    "{x:?} is complete while the consistency is unknown"
                );
            }
            if open.contains(x.as_str()) {
                continue;
            }
            if everything {
                assert!(
                    cl.taxonomy.is_unsatisfiable(x),
                    "{x:?} is reported complete"
                );
                continue;
            }
            let above = |a: &BTreeSet<(String, String)>| -> Vec<String> {
                a.iter()
                    .filter(|(p, _)| p == x.as_str())
                    .map(|(_, q)| q.clone())
                    .collect()
            };
            assert_eq!(above(&got), above(&want), "{x:?} is reported complete");
        }
        let c = match consistency {
            "consistent" => "consistent",
            "inconsistent" => "inconsistent",
            _ => "unknown",
        };
        (true, c)
    }

    /// Finds the saturation of a classification by its deadline. The deadlines rise by 8
    /// percent from 2 ms until one of them gives an answer instead of a timeout, which
    /// has run out in the saturation, since that is the first part of a run that can stop
    /// with an answer. The step may be longer than the saturation, so the milliseconds
    /// between the last timeout and that answer are halved until they are one apart. The
    /// deadlines go on until twenty have answered or `limit` is reached. A ladder fixed
    /// from the time of a full run misses a saturation of some milliseconds on a loaded
    /// machine. `answers` runs a classification under a deadline and says whether it
    /// answered.
    fn deadline_ladder(
        limit: std::time::Duration,
        mut answers: impl FnMut(std::time::Duration) -> bool,
    ) {
        let limit = limit.as_millis().max(2000) as u64;
        let after = |ms: u64| ms + (ms * 8 / 100).max(1);
        let answered = std::cell::Cell::new(0);
        let mut run = |ms: u64| {
            let ok = answers(std::time::Duration::from_millis(ms));
            answered.set(answered.get() + ok as usize);
            ok
        };
        let (mut lo, mut ms) = (0, 2);
        while ms < limit && !run(ms) {
            lo = ms;
            ms = after(ms);
        }
        let mut hi = ms;
        while hi - lo > 1 {
            let mid = (lo + hi) / 2;
            if run(mid) {
                hi = mid;
            } else {
                lo = mid;
            }
        }
        while answered.get() < 20 && ms < limit {
            ms = after(ms);
            run(ms);
        }
    }

    /// Classifies the cases under deadlines that [`deadline_ladder`] finds, each on a
    /// dataset of its own, and answers the consistencies of the partial answers.
    fn under_deadlines(inconsistent: bool) -> Vec<&'static str> {
        let n = if cfg!(debug_assertions) { 120 } else { 400 };
        let body = cases(n, inconsistent);
        let req = OntologyRequest::default();
        let ds = dataset(&body);
        let t = std::time::Instant::now();
        let full = ds
            .ontology()
            .classify(
                &req,
                &ClassifyRequest {
                    threads: 1,
                    ..Default::default()
                },
                &opts(),
            )
            .unwrap();
        let took = t.elapsed();
        assert!(full.classification.complete);
        assert_eq!(full.classification.consistent, !inconsistent);
        assert_eq!(full.classification.stats.tier.as_str(), "disjunctive");
        let mut partial = Vec::new();
        deadline_ladder(took * 20, |timeout| {
            let ds = dataset(&body);
            let o = QueryOptions {
                timeout: Some(timeout),
                ..opts()
            };
            let Ok(part) = ds.ontology().classify(&req, &partial_request(), &o) else {
                return false;
            };
            let (p, c) = check_answer(&ds, &part, &full);
            if p {
                partial.push(c);
            }
            true
        });
        partial
    }

    /// Classifies the cases on one thread under memory budgets from a twentieth of the
    /// full run's peak to nearly all of it, each on a dataset of its own, and answers the
    /// consistencies of the partial answers. A run stops at the same place under the
    /// same budget on every machine, where a deadline stops it at a place that depends on
    /// the speed of the machine and its load, so this ladder is the proof that a stop
    /// can leave the consistency unknown.
    fn under_budgets(inconsistent: bool) -> Vec<&'static str> {
        let n = if cfg!(debug_assertions) { 120 } else { 400 };
        let body = cases(n, inconsistent);
        let req = OntologyRequest::default();
        let full = dataset(&body)
            .ontology()
            .classify(&req, &partial_request(), &opts())
            .unwrap();
        assert!(full.classification.complete);
        assert_eq!(full.classification.consistent, !inconsistent);
        assert_eq!(full.classification.stats.tier.as_str(), "disjunctive");
        let peak = full.classification.stats.peak_memory as u64;
        let mut partial = Vec::new();
        for k in 1..20 {
            let ds = dataset(&body);
            let o = QueryOptions {
                max_memory_bytes: Some(peak * k / 20),
                ..opts()
            };
            // a budget under the size of the ontology and its index refuses the run
            let Ok(part) = ds.ontology().classify(&req, &partial_request(), &o) else {
                continue;
            };
            // a budget past the one of the disjunctive rules runs the EL rules instead,
            // whose answer leaves the disjunctive axioms out and is no stop
            if !part.excluded.is_empty() {
                continue;
            }
            let (p, c) = check_answer(&ds, &part, &full);
            if p {
                partial.push(c);
            }
        }
        partial
    }

    #[test]
    fn a_deadline_answers_in_part_with_the_classes_it_leaves_open() {
        let seen = under_deadlines(false);
        assert!(!seen.is_empty(), "no deadline gave a partial answer");
        assert!(!seen.contains(&"inconsistent"));
        let seen = under_budgets(false);
        assert!(!seen.is_empty(), "no budget gave a partial answer");
        assert!(!seen.contains(&"inconsistent"));
    }

    #[test]
    fn a_stop_before_the_inconsistency_is_found_leaves_it_unknown() {
        let seen = under_budgets(true);
        assert!(
            seen.contains(&"unknown"),
            "no budget left the consistency unknown: {seen:?}"
        );
        // the deadlines check every answer, and a stop of the clock may or may not come
        // before the inconsistency is found
        under_deadlines(true);
    }

    #[test]
    fn the_json_operation_reports_a_complete_run_and_an_inconsistent_one() {
        let ds = dataset(ONTO);
        for partial in [false, true] {
            let j = wire::call(
                &ds,
                "classify",
                &json!({"partial": partial, "expansion": "eager"}),
                &opts(),
            )
            .unwrap();
            assert_eq!(j["complete"], json!(true));
            assert_eq!(j["partial"], json!(false));
            assert_eq!(j["consistency"], json!("consistent"));
            assert_eq!(j["incompleteCount"], json!(0));
            assert_eq!(j["incomplete"], json!([]));
            assert_eq!(j["tier"], json!("el"));
        }
        let bad = wire::call(&ds, "classify", &json!({"expansion": "greedy"}), &opts());
        assert!(matches!(bad, Err(Error::Invalid(_))), "{:?}", bad.err());
        let ds = dataset(&format!(
            "{ONTO} owl:Thing rdfs:subClassOf [ owl:unionOf ( :Cell :Organelle ) ] .
             :Cell rdfs:subClassOf owl:Nothing . :Organelle rdfs:subClassOf owl:Nothing ."
        ));
        let j = wire::call(&ds, "classify", &json!({"partial": true}), &opts()).unwrap();
        assert_eq!(j["consistent"], json!(false));
        assert_eq!(j["consistency"], json!("inconsistent"));
        assert_eq!(j["partial"], json!(false));
        assert_eq!(j["incompleteCount"], json!(0));
        assert_eq!(j["tier"], json!("disjunctive"));
    }

    #[test]
    fn a_cancellation_stops_a_classification_waiting_for_threads() {
        // Every slot of the algorithm pool is taken, so admission waits for one. A
        // cancellation source beside the options' flag, as the bindings give a partial
        // classification, is set before the call, so only admission can see it, and it
        // ends the wait at once instead of blocking until a slot frees.
        use sparkles_core::algo::{Control, pool};
        let held: Vec<pool::Par> = (0..pool::capacity())
            .map(|_| pool::Par::acquire(1, &Control::unlimited()).unwrap())
            .collect();
        let o = QueryOptions {
            cancel: Some(Arc::new(std::sync::atomic::AtomicBool::new(false))),
            cancellation: Some(crate::task::Cancellation::from_flags([Arc::new(
                std::sync::atomic::AtomicBool::new(true),
            )])),
            ..opts()
        };
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(classify::admit(1, &o).err());
        });
        let r = rx.recv_timeout(std::time::Duration::from_secs(10));
        drop(held);
        assert!(
            matches!(r, Ok(Some(Error::Cancelled))),
            "the cancellation did not end the wait for threads: {r:?}"
        );
    }

    #[test]
    fn a_partial_answer_is_not_kept_and_does_not_write() {
        let body = cases(if cfg!(debug_assertions) { 120 } else { 400 }, false);
        let req = OntologyRequest::default();
        let ds = dataset(&body);
        let writes = ClassifyRequest {
            target: Some(named("http://ex.org/taxonomy")),
            ..partial_request()
        };
        assert!(matches!(
            ds.ontology().classify(&req, &writes, &opts()),
            Err(Error::Invalid(_))
        ));
        // the first memory budget that answers in part, each on a dataset of its own, since
        // a budget stops the run at the same place on every machine
        let peak = dataset(&body)
            .ontology()
            .classify(&req, &partial_request(), &opts())
            .unwrap()
            .classification
            .stats
            .peak_memory as u64;
        let (ds, part) = (1..20)
            .find_map(|k| {
                let ds = dataset(&body);
                let o = QueryOptions {
                    max_memory_bytes: Some(peak * k / 20),
                    ..opts()
                };
                match ds.ontology().classify(&req, &partial_request(), &o) {
                    Ok(c) if !c.classification.complete => Some((ds, c)),
                    _ => None,
                }
            })
            .expect("no budget answered in part");
        let h = ds.ontology();
        assert!(!part.complete());
        let next = h
            .classify(&req, &ClassifyRequest::default(), &opts())
            .unwrap();
        assert!(!next.reused, "a partial answer was kept");
        assert!(next.complete());
        let again = h.classify(&req, &partial_request(), &opts()).unwrap();
        assert!(again.reused && again.complete());
    }
}
