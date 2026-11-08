//! The axiom model, the mappings and the profile checks on small ontologies: one test
//! or more for each rule, with an ontology that keeps it and one that breaks it.

use oxrdf::Triple;
use oxrdfio::{RdfFormat, RdfParser};
use sparkles_owl::profiles::{self, CheckOptions, Profile};
use sparkles_owl::read::{self, Mode, ReadOptions, Severity};
use sparkles_owl::*;
use std::collections::BTreeSet;

const PREFIXES: &str = "@prefix : <http://example.org/> .
@prefix owl: <http://www.w3.org/2002/07/owl#> .
@prefix rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#> .
@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
@prefix xsd: <http://www.w3.org/2001/XMLSchema#> .
";

fn ttl(body: &str) -> Vec<Triple> {
    let src = format!("{PREFIXES}{body}");
    RdfParser::from_format(RdfFormat::Turtle)
        .for_reader(src.as_bytes())
        .map(|q| q.expect("turtle").into())
        .collect()
}

fn read_mode(body: &str, mode: Mode) -> read::Ontology {
    read::read_graph(
        ttl(body),
        ReadOptions {
            mode,
            ..Default::default()
        },
    )
}

fn strict(body: &str) -> read::Ontology {
    read_mode(body, Mode::Strict)
}

fn lenient(body: &str) -> read::Ontology {
    read_mode(body, Mode::Lenient)
}

fn ex(local: &str) -> String {
    format!("http://example.org/{local}")
}

fn class(local: &str) -> ClassExpression {
    ClassExpression::Class(Class::new(ex(local)))
}

fn op(local: &str) -> ObjectProperty {
    ObjectProperty::new(ex(local))
}

fn dp(local: &str) -> DataProperty {
    DataProperty::new(ex(local))
}

fn ind(local: &str) -> Individual {
    Individual::named(ex(local))
}

fn ofn(o: &read::Ontology) -> Vec<String> {
    o.axiom_set()
        .filter(|a| a.axiom_type() != AxiomType::Declaration)
        .map(|a| a.to_string())
        .collect()
}

/// The report of one profile.
fn report(o: &read::Ontology, p: Profile) -> profiles::Report {
    profiles::check(o, &[p], CheckOptions::default()).remove(0)
}

fn rules(r: &profiles::Report) -> Vec<String> {
    r.violations
        .iter()
        .map(|v| format!("{}: {}", v.rule, v.message))
        .collect()
}

/// Assert that an ontology conforms to `yes` and not to `no`, with a violation of `no`
/// that names `rule`.
fn assert_profiles(o: &read::Ontology, yes: &[Profile], no: &[(Profile, &str)]) {
    for p in yes {
        let r = report(o, *p);
        assert!(r.conforms, "{p} should conform: {:?}", rules(&r));
    }
    for (p, rule) in no {
        let r = report(o, *p);
        assert!(!r.conforms, "{p} should not conform");
        assert!(
            rules(&r).iter().any(|v| v.contains(rule)),
            "{p}: no violation names {rule:?}: {:?}",
            rules(&r)
        );
    }
}

const HEADER: &str = "<http://example.org/onto> a owl:Ontology .\n";

fn decl(body: &str) -> String {
    format!(
        "{HEADER}
:A a owl:Class . :B a owl:Class . :C a owl:Class .
:p a owl:ObjectProperty . :q a owl:ObjectProperty . :r a owl:ObjectProperty .
:d a owl:DatatypeProperty . :e a owl:DatatypeProperty .
:a a owl:NamedIndividual . :b a owl:NamedIndividual .
{body}"
    )
}

// ---- the reverse mapping -------------------------------------------------------

#[test]
fn reads_a_restriction_subclass() {
    // spec F21 A1
    let o = strict(&decl(
        ":A rdfs:subClassOf [ a owl:Restriction ; owl:onProperty :p ; owl:someValuesFrom :B ] .",
    ));
    assert!(o.errors().next().is_none(), "{:?}", o.diagnostics);
    assert!(o.unmapped.is_empty());
    assert_eq!(
        ofn(&o),
        [
            "SubClassOf(<http://example.org/A> ObjectSomeValuesFrom(<http://example.org/p> <http://example.org/B>))"
        ]
    );
    assert!(o.contains(&Axiom::sub_class_of(
        class("A"),
        ClassExpression::some(op("p"), class("B"))
    )));
}

#[test]
fn reads_the_header() {
    let o = strict(
        "<http://example.org/onto> a owl:Ontology ;
            owl:versionIRI <http://example.org/onto/1> ;
            owl:imports <http://example.org/other> ;
            rdfs:label \"Example\" .",
    );
    assert_eq!(o.header.iri.as_ref().unwrap().as_str(), ex("onto"));
    assert_eq!(
        o.header.version_iri.as_ref().unwrap().as_str(),
        ex("onto/1")
    );
    assert_eq!(o.header.imports.len(), 1);
    assert_eq!(o.header.annotations.len(), 1);
    assert!(o.unmapped.is_empty());
}

#[test]
fn reads_annotated_axioms() {
    let o = strict(&decl(
        ":A rdfs:subClassOf :B .
         [] a owl:Axiom ; owl:annotatedSource :A ; owl:annotatedProperty rdfs:subClassOf ;
            owl:annotatedTarget :B ; rdfs:comment \"why\" .",
    ));
    assert!(o.errors().next().is_none(), "{:?}", o.diagnostics);
    let ax = Axiom::sub_class_of(class("A"), class("B")).annotated(Annotation::comment("why"));
    assert!(o.contains(&ax), "{:?}", ofn(&o));
    assert!(!o.contains(&ax.without_annotations()));
}

#[test]
fn reads_n_ary_axioms() {
    let o = strict(&decl(
        "[] a owl:AllDisjointClasses ; owl:members ( :A :B :C ) .
         [] a owl:AllDifferent ; owl:members ( :a :b ) .
         :p owl:propertyChainAxiom ( :q :r ) .
         :A owl:hasKey ( :p :d ) .
         :C owl:disjointUnionOf ( :A :B ) .
         [] a owl:NegativePropertyAssertion ; owl:sourceIndividual :a ;
            owl:assertionProperty :p ; owl:targetIndividual :b .",
    ));
    assert!(o.errors().next().is_none(), "{:?}", o.diagnostics);
    assert!(o.unmapped.is_empty());
    let types: BTreeSet<AxiomType> = o.axiom_set().map(|a| a.axiom_type()).collect();
    for t in [
        AxiomType::DisjointClasses,
        AxiomType::DifferentIndividuals,
        AxiomType::SubObjectPropertyOf,
        AxiomType::HasKey,
        AxiomType::DisjointUnion,
        AxiomType::NegativeObjectPropertyAssertion,
    ] {
        assert!(types.contains(&t), "{t:?} missing from {:?}", ofn(&o));
    }
}

#[test]
fn reads_data_ranges() {
    let o = strict(&decl(
        ":d rdfs:range [ a rdfs:Datatype ; owl:onDatatype xsd:integer ;
                         owl:withRestrictions ( [ xsd:minInclusive 0 ] [ xsd:maxExclusive 10 ] ) ] .
         :e rdfs:range [ a rdfs:Datatype ; owl:oneOf ( \"x\" \"y\" ) ] .",
    ));
    assert!(o.errors().next().is_none(), "{:?}", o.diagnostics);
    assert_eq!(
        o.axiom_set()
            .filter(|a| a.axiom_type() == AxiomType::DataPropertyRange)
            .count(),
        2
    );
}

#[test]
fn self_equivalence_has_one_operand() {
    let o = strict(&decl(":A owl:equivalentClass :A . :a owl:sameAs :a ."));
    assert!(o.unmapped.is_empty());
    assert!(o.contains(&Axiom::equivalent_classes([class("A")])));
    // a disjointness of one operand cannot be expressed as a set
    let o = strict(&decl(":A owl:disjointWith :A ."));
    assert_eq!(o.unmapped.len(), 1);
}

// ---- strict and lenient reads --------------------------------------------------

#[test]
fn strict_refuses_undeclared_properties() {
    let body = format!(
        "{HEADER}:A a owl:Class . :B a owl:Class .
        :A rdfs:subClassOf [ a owl:Restriction ; owl:onProperty :p ; owl:someValuesFrom :B ] ."
    );
    let s = strict(&body);
    assert!(!s.unmapped.is_empty());
    assert!(
        s.errors()
            .any(|d| d.severity == Severity::Error && d.code == "unmapped")
    );
    assert!(ofn(&s).is_empty());
    // the lenient read types :p from its use and says so
    let l = lenient(&body);
    assert_eq!(ofn(&l).len(), 1, "{:?}", ofn(&l));
    assert!(l.errors().any(|d| d.code == "undeclared"));
    assert!(l.errors().all(|d| d.severity == Severity::Warning));
}

#[test]
fn strict_refuses_untyped_expressions() {
    let body = decl(":A rdfs:subClassOf [ owl:onProperty :p ; owl:someValuesFrom :B ] .");
    let s = strict(&body);
    assert!(!s.unmapped.is_empty());
    let l = lenient(&body);
    assert_eq!(ofn(&l).len(), 1);
    assert!(l.errors().any(|d| d.code == "untyped"));
}

#[test]
fn strict_refuses_shared_list_cells() {
    // two unions share their list
    let body = decl(
        "_:l rdf:first :A ; rdf:rest _:m . _:m rdf:first :B ; rdf:rest rdf:nil .
         :C rdfs:subClassOf [ a owl:Class ; owl:unionOf _:l ] .
         :C owl:disjointWith [ a owl:Class ; owl:intersectionOf _:l ] .",
    );
    let s = strict(&body);
    assert!(!s.unmapped.is_empty(), "{:?}", ofn(&s));
    let l = lenient(&body);
    assert_eq!(ofn(&l).len(), 2, "{:?}", ofn(&l));
    assert!(l.errors().any(|d| d.code == "shared"));
}

#[test]
fn shared_expressions_read_in_both_modes() {
    // the W3C test case Restriction-002 shares a restriction node and is in OWL 2 DL
    let body = decl(
        "_:r a owl:Restriction ; owl:onProperty :p ; owl:someValuesFrom :B .
         :A rdfs:subClassOf _:r . :C rdfs:subClassOf _:r .",
    );
    for o in [strict(&body), lenient(&body)] {
        assert!(o.unmapped.is_empty());
        assert_eq!(ofn(&o).len(), 2);
        assert!(
            o.errors()
                .any(|d| d.code == "shared" && d.severity == Severity::Warning)
        );
    }
}

#[test]
fn annotation_without_main_triple() {
    let body = decl(
        "[] a owl:Axiom ; owl:annotatedSource :A ; owl:annotatedProperty rdfs:subClassOf ;
            owl:annotatedTarget :B ; rdfs:comment \"orphan\" .",
    );
    let s = strict(&body);
    assert!(s.errors().any(|d| d.code == "missing-main"));
    assert!(!s.unmapped.is_empty());
}

#[test]
fn owl1_compatibility() {
    // Table 5 removes rdfs:Class beside owl:Class; Table 6 types owl:TransitiveProperty
    let o = strict(&format!(
        "{HEADER}:A a owl:Class , rdfs:Class . :t a owl:TransitiveProperty ."
    ));
    assert!(
        o.unmapped.is_empty(),
        "{:?}",
        o.unmapped
            .iter()
            .map(|i| o.triples[*i].to_string())
            .collect::<Vec<_>>()
    );
    assert!(o.contains(&Axiom::new(AxiomKind::TransitiveObjectProperty(
        op("t").into()
    ))));
    assert!(o.contains(&Axiom::declaration(Entity::ObjectProperty(op("t")))));
}

#[test]
fn assertions_map_one_triple_at_a_time() {
    let o = strict(&decl(":a a :A . :a :p :b . :a :d \"1\"^^xsd:integer ."));
    for t in &o.triples {
        if t.subject.to_string() == format!("<{}>", ex("a"))
            && t.predicate.as_str() != read_rdf_type()
        {
            let ax = o.map_assertion(t).expect("an assertion");
            assert!(o.contains(&ax), "{ax}");
        }
    }
    let t = Triple::new(
        oxrdf::NamedNode::new_unchecked(ex("a")),
        oxrdf::NamedNode::new_unchecked(ex("p")),
        oxrdf::NamedNode::new_unchecked(ex("b")),
    );
    assert_eq!(
        o.map_assertion(&t),
        Some(Axiom::new(AxiomKind::ObjectPropertyAssertion(
            op("p").into(),
            ind("a"),
            ind("b")
        )))
    );
}

fn read_rdf_type() -> &'static str {
    "http://www.w3.org/1999/02/22-rdf-syntax-ns#type"
}

// ---- the forward mapping -------------------------------------------------------

fn round_trip(axioms: &[Axiom]) {
    let header = Header {
        iri: Some(iri(ex("onto"))),
        ..Default::default()
    };
    let mut fresh = write::counter("t");
    let mut all: BTreeSet<Axiom> = axioms.iter().cloned().collect();
    for ax in axioms {
        for e in ax.signature() {
            if !e.is_builtin() {
                all.insert(Axiom::declaration(e));
            }
        }
    }
    let triples = write::ontology_triples(&header, all.iter(), &mut fresh);
    let o = read::read_graph(
        triples,
        ReadOptions {
            mode: Mode::Strict,
            ..Default::default()
        },
    );
    let back: BTreeSet<Axiom> = o.axiom_set().cloned().collect();
    assert_eq!(back, all, "diagnostics {:?}", o.diagnostics);
    assert!(o.unmapped.is_empty());
    assert_eq!(o.header, header);
}

#[test]
fn writes_and_reads_every_axiom_type() {
    let lit = Literal::typed("5", "http://www.w3.org/2001/XMLSchema#integer");
    let inv = ObjectPropertyExpression::InverseOf(op("q"));
    let ann = Annotation::label("note");
    let axioms = vec![
        Axiom::sub_class_of(class("A"), ClassExpression::some(op("p"), class("B")))
            .annotated(ann.clone()),
        Axiom::equivalent_classes([class("A"), ClassExpression::and([class("B"), class("C")])]),
        Axiom::disjoint_classes([class("A"), class("B"), class("C")]).annotated(ann.clone()),
        Axiom::new(AxiomKind::DisjointUnion(
            Class::new(ex("U")),
            [class("A"), class("B")].into(),
        )),
        Axiom::new(AxiomKind::SubObjectPropertyOf(
            SubObjectPropertyExpression::Chain(vec![op("p").into(), inv.clone()]),
            op("r").into(),
        )),
        Axiom::new(AxiomKind::SubObjectPropertyOf(
            SubObjectPropertyExpression::Expression(op("p").into()),
            op("q").into(),
        )),
        Axiom::new(AxiomKind::EquivalentObjectProperties(
            [op("p").into(), inv.clone()].into(),
        )),
        Axiom::new(AxiomKind::DisjointObjectProperties(
            [op("p").into(), op("q").into(), op("r").into()].into(),
        )),
        Axiom::new(AxiomKind::InverseObjectProperties(
            op("p").into(),
            op("q").into(),
        )),
        Axiom::new(AxiomKind::ObjectPropertyDomain(
            op("p").into(),
            ClassExpression::not(class("A")),
        )),
        Axiom::new(AxiomKind::ObjectPropertyRange(
            op("p").into(),
            ClassExpression::ObjectMaxCardinality(2, op("q").into(), Some(Box::new(class("B")))),
        )),
        Axiom::new(AxiomKind::FunctionalObjectProperty(op("p").into())),
        Axiom::new(AxiomKind::InverseFunctionalObjectProperty(op("p").into())),
        Axiom::new(AxiomKind::ReflexiveObjectProperty(op("p").into())),
        Axiom::new(AxiomKind::IrreflexiveObjectProperty(op("q").into())),
        Axiom::new(AxiomKind::SymmetricObjectProperty(op("p").into())),
        Axiom::new(AxiomKind::AsymmetricObjectProperty(op("q").into())),
        Axiom::new(AxiomKind::TransitiveObjectProperty(op("r").into())),
        Axiom::new(AxiomKind::SubDataPropertyOf(dp("d"), dp("e"))),
        Axiom::new(AxiomKind::EquivalentDataProperties(
            [dp("d"), dp("e")].into(),
        )),
        Axiom::new(AxiomKind::DisjointDataProperties(
            [dp("d"), dp("e"), dp("f")].into(),
        )),
        Axiom::new(AxiomKind::DataPropertyDomain(
            dp("d"),
            ClassExpression::DataSomeValuesFrom(
                vec![dp("e")],
                DataRange::Datatype(Datatype::literal()),
            ),
        )),
        Axiom::new(AxiomKind::DataPropertyRange(
            dp("d"),
            DataRange::DatatypeRestriction(
                Datatype::new("http://www.w3.org/2001/XMLSchema#integer"),
                [FacetRestriction {
                    facet: iri("http://www.w3.org/2001/XMLSchema#minInclusive"),
                    value: lit.clone(),
                }]
                .into(),
            ),
        )),
        Axiom::new(AxiomKind::FunctionalDataProperty(dp("d"))),
        Axiom::new(AxiomKind::DatatypeDefinition(
            Datatype::new(ex("small")),
            DataRange::OneOf([lit.clone(), Literal::simple("x")].into()),
        )),
        Axiom::new(AxiomKind::HasKey(
            class("A"),
            [op("p").into()].into(),
            [dp("d")].into(),
        )),
        Axiom::new(AxiomKind::SameIndividual([ind("a"), ind("b")].into())),
        Axiom::new(AxiomKind::DifferentIndividuals(
            [ind("a"), ind("b"), ind("c")].into(),
        )),
        Axiom::class_assertion(
            ClassExpression::ObjectHasValue(op("p").into(), ind("b")),
            ind("a"),
        ),
        Axiom::class_assertion(ClassExpression::ObjectHasSelf(op("p").into()), ind("a")),
        Axiom::class_assertion(
            ClassExpression::ObjectOneOf([ind("a"), ind("b")].into()),
            ind("c"),
        ),
        Axiom::new(AxiomKind::ObjectPropertyAssertion(
            op("p").into(),
            ind("a"),
            ind("b"),
        ))
        .annotated(ann.clone()),
        Axiom::new(AxiomKind::NegativeObjectPropertyAssertion(
            op("p").into(),
            ind("a"),
            ind("c"),
        )),
        Axiom::new(AxiomKind::DataPropertyAssertion(
            dp("d"),
            ind("a"),
            lit.clone(),
        )),
        Axiom::new(AxiomKind::NegativeDataPropertyAssertion(
            dp("d"),
            ind("b"),
            lit.clone(),
        ))
        .annotated(ann.clone()),
        Axiom::new(AxiomKind::AnnotationAssertion(
            AnnotationProperty::new(ex("note")),
            AnnotationSubject::Iri(iri(ex("A"))),
            AnnotationValue::Literal(Literal::lang("hello", "en").unwrap()),
        )),
        Axiom::new(AxiomKind::SubAnnotationPropertyOf(
            AnnotationProperty::new(ex("note")),
            AnnotationProperty::new("http://www.w3.org/2000/01/rdf-schema#comment"),
        )),
        Axiom::new(AxiomKind::AnnotationPropertyDomain(
            AnnotationProperty::new(ex("note")),
            iri(ex("A")),
        )),
        Axiom::new(AxiomKind::AnnotationPropertyRange(
            AnnotationProperty::new(ex("note")),
            iri(ex("B")),
        )),
    ];
    let types: BTreeSet<AxiomType> = axioms.iter().map(|a| a.axiom_type()).collect();
    assert_eq!(
        types.len() + 1,
        AxiomType::ALL.len(),
        "every type but Declaration"
    );
    round_trip(&axioms);
}

#[test]
fn json_round_trip() {
    let axioms = [
        Axiom::sub_class_of(
            class("A"),
            ClassExpression::all(ObjectPropertyExpression::InverseOf(op("p")), class("B")),
        )
        .annotated(Annotation::comment("c")),
        Axiom::new(AxiomKind::DataPropertyAssertion(
            dp("d"),
            Individual::anonymous("x"),
            Literal::lang("v", "fr").unwrap(),
        )),
        Axiom::new(AxiomKind::HasKey(
            class("A"),
            BTreeSet::new(),
            [dp("d")].into(),
        )),
        Axiom::declaration(Entity::Datatype(Datatype::new(ex("dt")))),
    ];
    for ax in axioms {
        let v = json::axiom(&ax);
        assert_eq!(json::read_axiom(&v).unwrap(), ax, "{v}");
    }
    // structural conditions are checked on the way in
    let bad = serde_json::json!({"type": "EquivalentClasses", "classes": []});
    assert!(json::read_axiom(&bad).is_err());
}

// ---- OWL 2 EL ------------------------------------------------------------------

#[test]
fn el_allows_existentials_and_intersections() {
    let o = strict(&decl(
        ":A rdfs:subClassOf [ a owl:Class ; owl:intersectionOf ( :B
            [ a owl:Restriction ; owl:onProperty :p ; owl:someValuesFrom :C ] ) ] .
         :p rdfs:subPropertyOf :q . :r owl:propertyChainAxiom ( :p :q ) . :q a owl:TransitiveProperty .",
    ));
    assert_profiles(&o, &[Profile::El, Profile::Dl], &[]);
}

#[test]
fn el_forbids_universals_and_negation() {
    let o = strict(&decl(
        ":A rdfs:subClassOf [ a owl:Restriction ; owl:onProperty :p ; owl:allValuesFrom :B ] .",
    ));
    assert_profiles(&o, &[Profile::Dl], &[(Profile::El, "§2.2.3")]);
    let o = strict(&decl(
        ":A rdfs:subClassOf [ a owl:Class ; owl:complementOf :B ] .",
    ));
    assert_profiles(&o, &[Profile::Dl], &[(Profile::El, "§2.2.3")]);
}

#[test]
fn el_forbids_inverse_properties_and_some_axioms() {
    let o = strict(&decl(
        ":A rdfs:subClassOf [ a owl:Restriction ; owl:onProperty [ owl:inverseOf :p ] ; owl:someValuesFrom :B ] .",
    ));
    assert_profiles(&o, &[Profile::Dl], &[(Profile::El, "§2.2.2")]);
    let o = strict(&decl(":p a owl:SymmetricProperty ."));
    assert_profiles(&o, &[Profile::Dl], &[(Profile::El, "§2.2.5")]);
    let o = strict(&decl(":p a owl:FunctionalProperty ."));
    assert_profiles(&o, &[Profile::Dl], &[(Profile::El, "§2.2.5")]);
}

#[test]
fn el_forbids_datatypes_and_anonymous_individuals() {
    let o = strict(&decl(":d rdfs:range xsd:double ."));
    assert_profiles(&o, &[Profile::Dl], &[(Profile::El, "§2.2.1")]);
    let o = strict(&decl("_:x a :A ."));
    assert_profiles(&o, &[Profile::Dl, Profile::Rl], &[(Profile::El, "§2.2.1")]);
}

#[test]
fn el_restricts_ranges_of_chains() {
    // §2.2.6: a range on the super property of a chain must hold on the chain's last
    let o = strict(&decl(
        ":r owl:propertyChainAxiom ( :p :q ) . :r rdfs:range :A .",
    ));
    assert_profiles(&o, &[Profile::Dl], &[(Profile::El, "§2.2.6")]);
    let o = strict(&decl(
        ":r owl:propertyChainAxiom ( :p :q ) . :r rdfs:range :A . :q rdfs:range :A .",
    ));
    assert_profiles(&o, &[Profile::El], &[]);
}

// ---- OWL 2 QL ------------------------------------------------------------------

#[test]
fn ql_allows_its_grammar() {
    let o = strict(&decl(
        "[ a owl:Restriction ; owl:onProperty :p ; owl:someValuesFrom owl:Thing ] rdfs:subClassOf :A .
         :A rdfs:subClassOf [ a owl:Restriction ; owl:onProperty [ owl:inverseOf :q ] ; owl:someValuesFrom :B ] .
         :B rdfs:subClassOf [ a owl:Class ; owl:complementOf :C ] .
         :p rdfs:subPropertyOf :q . :p a owl:ReflexiveProperty . :a :p :b .",
    ));
    assert_profiles(&o, &[Profile::Ql, Profile::Dl], &[]);
}

#[test]
fn ql_restricts_subclass_expressions() {
    let o = strict(&decl(
        "[ a owl:Restriction ; owl:onProperty :p ; owl:someValuesFrom :B ] rdfs:subClassOf :A .",
    ));
    assert_profiles(&o, &[Profile::Dl, Profile::El], &[(Profile::Ql, "§3.2.3")]);
    let o = strict(&decl(
        "[ a owl:Class ; owl:unionOf ( :A :B ) ] rdfs:subClassOf :C .",
    ));
    assert_profiles(&o, &[Profile::Dl], &[(Profile::Ql, "§3.2.3")]);
}

#[test]
fn ql_forbids_functional_transitive_and_equality() {
    for body in [
        ":p a owl:FunctionalProperty .",
        ":p a owl:TransitiveProperty .",
        ":p a owl:IrreflexiveProperty .",
        ":a owl:sameAs :b .",
        ":A owl:hasKey ( :p ) .",
    ] {
        let o = strict(&decl(body));
        assert_profiles(&o, &[Profile::Dl], &[(Profile::Ql, "§3.2.5")]);
    }
}

#[test]
fn ql_restricts_class_assertions() {
    let o = strict(&decl(
        ":a a [ a owl:Restriction ; owl:onProperty :p ; owl:someValuesFrom :B ] .",
    ));
    assert_profiles(&o, &[Profile::Dl], &[(Profile::Ql, "§3.2.5")]);
}

// ---- OWL 2 RL ------------------------------------------------------------------

#[test]
fn rl_allows_its_grammar() {
    let o = strict(&decl(
        "[ a owl:Class ; owl:unionOf ( :A [ a owl:Restriction ; owl:onProperty :p ; owl:someValuesFrom :B ] ) ]
             rdfs:subClassOf [ a owl:Restriction ; owl:onProperty :q ; owl:allValuesFrom :C ] .
         :A rdfs:subClassOf [ a owl:Restriction ; owl:onProperty :p ; owl:maxCardinality 1 ] .
         :p a owl:FunctionalProperty . :r a owl:TransitiveProperty . :a owl:sameAs :b . :A owl:hasKey ( :p ) .",
    ));
    assert_profiles(&o, &[Profile::Rl, Profile::Dl], &[]);
}

#[test]
fn rl_restricts_superclass_expressions() {
    let o = strict(&decl(
        ":A rdfs:subClassOf [ a owl:Restriction ; owl:onProperty :p ; owl:someValuesFrom :B ] .",
    ));
    assert_profiles(&o, &[Profile::Dl], &[(Profile::Rl, "§4.2.3")]);
    let o = strict(&decl(
        ":A rdfs:subClassOf [ a owl:Class ; owl:unionOf ( :B :C ) ] .",
    ));
    assert_profiles(&o, &[Profile::Dl], &[(Profile::Rl, "§4.2.3")]);
    let o = strict(&decl(
        ":A rdfs:subClassOf [ a owl:Restriction ; owl:onProperty :p ; owl:maxCardinality 2 ] .",
    ));
    assert_profiles(&o, &[Profile::Dl], &[(Profile::Rl, "§4.2.3")]);
}

#[test]
fn rl_restricts_subclass_expressions() {
    let o = strict(&decl(
        "[ a owl:Restriction ; owl:onProperty :p ; owl:allValuesFrom :B ] rdfs:subClassOf :A .",
    ));
    assert_profiles(&o, &[Profile::Dl], &[(Profile::Rl, "§4.2.3")]);
}

#[test]
fn rl_forbids_reflexive_and_disjoint_union() {
    let o = strict(&decl(":p a owl:ReflexiveProperty ."));
    assert_profiles(&o, &[Profile::Dl, Profile::Ql], &[(Profile::Rl, "§4.2.5")]);
    let o = strict(&decl(":C owl:disjointUnionOf ( :A :B ) ."));
    assert_profiles(&o, &[Profile::Dl], &[(Profile::Rl, "§4.2.5")]);
}

#[test]
fn rl_forbids_owl_real() {
    let o = strict(&decl(":d rdfs:range owl:real ."));
    assert_profiles(&o, &[Profile::Dl], &[(Profile::Rl, "§4.2.1")]);
}

// ---- literals (roadmap item 541) --------------------------------------------------

#[test]
fn literals_need_lexical_forms_of_their_datatype() {
    let ok = strict(&decl(
        ":a :d \"12\"^^xsd:integer , \"-INF\"^^xsd:double , \"2000-01-01T00:00:00Z\"^^xsd:dateTimeStamp .
         :A rdfs:subClassOf [ a owl:Restriction ; owl:onProperty :d ; owl:hasValue \"1/3\"^^owl:rational ] .",
    ));
    assert_profiles(&ok, &[Profile::Dl], &[]);
    // §5.7: each report lists the DL violation, whichever datatypes the profile allows
    let bad = strict(&decl(":a :d \"twelve\"^^xsd:integer ."));
    assert_profiles(
        &bad,
        &[],
        &[
            (Profile::Dl, "§5.7"),
            (Profile::El, "§5.7"),
            (Profile::Ql, "§5.7"),
            (Profile::Rl, "§5.7"),
        ],
    );
    for lit in [
        "\"300\"^^xsd:byte",
        "\" 1\"^^xsd:integer",
        "\"1.5\"^^xsd:integer",
        "\"1\"^^owl:real",
        "\"1/0\"^^owl:rational",
        "\"maybe\"^^xsd:boolean",
        "\"2000-01-01T00:00:00\"^^xsd:dateTimeStamp",
        "\"a  b\"^^xsd:token",
        "\"0FB\"^^xsd:hexBinary",
        "\"<a>\"^^rdf:XMLLiteral",
    ] {
        let o = strict(&decl(&format!(":a :d {lit} .")));
        assert_profiles(&o, &[], &[(Profile::Dl, "§5.7")]);
    }
}

#[test]
fn annotation_literals_are_checked() {
    let o = strict(&decl(":A rdfs:comment \"x\"^^xsd:integer ."));
    assert_profiles(&o, &[], &[(Profile::Dl, "§5.7")]);
    let header = strict(&decl(
        "<http://example.org/onto> rdfs:comment \"x\"^^xsd:boolean .",
    ));
    let r = report(&header, Profile::Dl);
    assert!(!r.conforms);
    assert!(
        r.violations
            .iter()
            .any(|v| v.axiom.is_none() && v.rule.ends_with("§5.7"))
    );
    // a datatype outside the map has no known lexical space: the datatype check
    // reports the datatype, and no lexical form is reported
    let other = strict(&decl(":A rdfs:comment \"x\"^^<http://example.org/dt> ."));
    assert!(
        !rules(&report(&other, Profile::Dl))
            .iter()
            .any(|v| v.contains("§5.7"))
    );
}

#[test]
fn imported_header_literals_are_checked() {
    let main = ttl(&decl(
        "<http://example.org/onto> owl:imports <http://example.org/lib> .",
    ));
    let lib = ttl("<http://example.org/lib> a owl:Ontology ; rdfs:comment \"x\"^^xsd:integer .");
    let graph_of: Vec<u32> = std::iter::repeat_n(0, main.len())
        .chain(std::iter::repeat_n(1, lib.len()))
        .collect();
    let triples = main.into_iter().chain(lib).collect();
    let o = read::read(
        triples,
        graph_of,
        &[0],
        ReadOptions {
            mode: Mode::Strict,
            ..Default::default()
        },
    );
    assert_eq!(o.other_headers.len(), 1);
    let r = report(&o, Profile::Dl);
    assert!(
        r.violations
            .iter()
            .any(|v| v.rule.ends_with("§5.7") && v.graph == Some(1)),
        "{:?}",
        r.violations
    );
}

// Namespace validation is a necessary XMLLiteral check. Accepted controls retain
// the existing noncanonical fragment spellings, not a new canonical-value claim.
fn xml_literal_namespaces_turtle(xml: &str) -> String {
    format!(
        "{}^^rdf:XMLLiteral",
        serde_json::to_string(xml).expect("hand-built XML literal string")
    )
}

#[test]
fn xml_literal_namespaces_data_assertions_report_section57() {
    let literal = xml_literal_namespaces_turtle("<p:a/>");
    let o = strict(&decl(&format!(":a :d {literal} .")));
    assert_profiles(
        &o,
        &[],
        &[
            (Profile::Dl, "§5.7"),
            (Profile::El, "§5.7"),
            (Profile::Ql, "§5.7"),
            (Profile::Rl, "§5.7"),
        ],
    );
}

#[test]
fn xml_literal_namespaces_value_restrictions_report_section57() {
    let literal = xml_literal_namespaces_turtle("<a p:x='1'/>");
    let o = strict(&decl(&format!(
        ":A rdfs:subClassOf [ a owl:Restriction ; owl:onProperty :d ; owl:hasValue {literal} ] ."
    )));
    assert_profiles(&o, &[], &[(Profile::Dl, "§5.7")]);
}

#[test]
fn xml_literal_namespaces_annotations_report_section57() {
    let literal =
        xml_literal_namespaces_turtle("<a xmlns:p='urn:u' xmlns:q='urn:u' p:x='1' q:x='2'/>");
    let o = strict(&decl(&format!(":A rdfs:comment {literal} .")));
    assert_profiles(&o, &[], &[(Profile::Dl, "§5.7")]);
}

#[test]
fn xml_literal_namespaces_ontology_headers_report_section57() {
    let literal = xml_literal_namespaces_turtle("<a xmlns:xml='urn:wrong'/>");
    let o = strict(&decl(&format!(
        "<http://example.org/onto> rdfs:comment {literal} ."
    )));
    let r = report(&o, Profile::Dl);
    assert!(
        !r.conforms,
        "invalid XML namespace binding must not conform"
    );
    assert!(
        r.violations
            .iter()
            .any(|v| v.axiom.is_none() && v.rule.ends_with("§5.7")),
        "{:?}",
        r.violations
    );
}

#[test]
fn xml_literal_namespaces_import_headers_report_section57() {
    let main = ttl(&decl(
        "<http://example.org/onto> owl:imports <http://example.org/lib> .",
    ));
    let literal = xml_literal_namespaces_turtle("<a xmlns:p=''/>");
    let lib = ttl(&format!(
        "<http://example.org/lib> a owl:Ontology ; rdfs:comment {literal} ."
    ));
    let graph_of: Vec<u32> = std::iter::repeat_n(0, main.len())
        .chain(std::iter::repeat_n(1, lib.len()))
        .collect();
    let triples = main.into_iter().chain(lib).collect();
    let o = read::read(
        triples,
        graph_of,
        &[0],
        ReadOptions {
            mode: Mode::Strict,
            ..Default::default()
        },
    );
    assert_eq!(o.other_headers.len(), 1);
    let r = report(&o, Profile::Dl);
    assert!(
        r.violations
            .iter()
            .any(|v| v.rule.ends_with("§5.7") && v.graph == Some(1)),
        "{:?}",
        r.violations
    );
}

#[test]
fn xml_literal_namespaces_keep_accepted_legacy_fragments_in_all_literal_positions() {
    let literal = xml_literal_namespaces_turtle("<a xmlns='urn:u' xmlns:p='urn:u' x='1' p:x='2'/>");
    let o = strict(&decl(&format!(
        ":a :d {literal} .\n\
         :A rdfs:comment {literal} .\n\
         <http://example.org/onto> rdfs:comment {literal} ."
    )));
    let r = report(&o, Profile::Dl);
    assert!(r.conforms, "{:?}", rules(&r));
    assert!(
        !r.violations.iter().any(|v| v.rule.ends_with("§5.7")),
        "{:?}",
        r.violations
    );
}

#[test]
fn xml_literal_namespaces_do_not_assign_a_lexical_space_to_an_unknown_datatype() {
    let literal = serde_json::to_string("<p:a/>").expect("hand-built literal string");
    let o = strict(&decl(&format!(
        ":A rdfs:comment {literal}^^<http://example.org/custom-xml> ."
    )));
    let r = report(&o, Profile::Dl);
    assert!(
        !r.violations.iter().any(|v| v.rule.ends_with("§5.7")),
        "{:?}",
        r.violations
    );
}

#[test]
fn defined_datatypes_have_no_literals_or_facets() {
    let body = ":SSN a rdfs:Datatype ; owl:equivalentClass [ a rdfs:Datatype ;
         owl:onDatatype xsd:string ; owl:withRestrictions ( [ xsd:pattern \"[0-9]+\" ] ) ] .";
    assert_profiles(&strict(&decl(body)), &[Profile::Dl], &[]);
    let lit = strict(&decl(&format!("{body} :a :d \"123\"^^:SSN .")));
    assert_profiles(&lit, &[], &[(Profile::Dl, "§9.4")]);
    let restricted = strict(&decl(&format!(
        "{body} :d rdfs:range [ a rdfs:Datatype ; owl:onDatatype :SSN ;
           owl:withRestrictions ( [ xsd:maxLength \"3\"^^xsd:integer ] ) ] ."
    )));
    assert_profiles(&restricted, &[], &[(Profile::Dl, "§9.4")]);
}

#[test]
fn facets_must_be_in_the_facet_space() {
    let range = |dt: &str, facet: &str, value: &str| {
        strict(&decl(&format!(
            ":d rdfs:range [ a rdfs:Datatype ; owl:onDatatype {dt} ;
               owl:withRestrictions ( [ {facet} {value} ] ) ] ."
        )))
    };
    for (dt, facet, value) in [
        ("xsd:integer", "xsd:minInclusive", "\"5\"^^xsd:integer"),
        ("xsd:integer", "xsd:maxExclusive", "\"5.5\"^^xsd:decimal"),
        ("xsd:double", "xsd:minInclusive", "\"0\"^^xsd:double"),
        (
            "xsd:string",
            "xsd:maxLength",
            "\"3\"^^xsd:nonNegativeInteger",
        ),
        ("xsd:string", "xsd:pattern", "\"a+\""),
        ("rdf:PlainLiteral", "rdf:langRange", "\"en\""),
        (
            "xsd:dateTime",
            "xsd:minInclusive",
            "\"2000-01-01T00:00:00\"^^xsd:dateTime",
        ),
    ] {
        let o = range(dt, facet, value);
        assert!(
            report(&o, Profile::Dl).conforms,
            "{dt} {facet} {value}: {:?}",
            rules(&report(&o, Profile::Dl))
        );
    }
    for (dt, facet, value) in [
        ("xsd:integer", "xsd:minInclusive", "\"5\"^^xsd:double"),
        ("xsd:integer", "xsd:length", "\"5\"^^xsd:integer"),
        ("xsd:double", "xsd:minInclusive", "\"0\"^^xsd:integer"),
        ("xsd:string", "xsd:maxLength", "\"-1\"^^xsd:integer"),
        ("xsd:string", "xsd:maxLength", "\"three\""),
        ("xsd:boolean", "xsd:pattern", "\"t.*\""),
        ("xsd:string", "xsd:totalDigits", "\"3\"^^xsd:integer"),
        (
            "xsd:dateTimeStamp",
            "xsd:minInclusive",
            "\"2000-01-01T00:00:00\"^^xsd:dateTime",
        ),
    ] {
        let o = range(dt, facet, value);
        assert_profiles(&o, &[], &[(Profile::Dl, "§7.5")]);
    }
}

// ---- OWL 2 DL ------------------------------------------------------------------

#[test]
fn dl_requires_a_header() {
    let o = strict(":A a owl:Class .");
    assert_profiles(&o, &[], &[(Profile::Dl, "§3.1.2")]);
}

#[test]
fn dl_requires_typed_properties() {
    // §5.8.1: a property used as object and data property at once
    let o = lenient(&decl(":x a owl:ObjectProperty , owl:DatatypeProperty ."));
    assert_profiles(&o, &[], &[(Profile::Dl, "§5.8.1")]);
}

#[test]
fn dl_forbids_reserved_vocabulary() {
    let o = lenient(&decl("rdf:Property rdfs:subClassOf :A ."));
    assert!(!report(&o, Profile::Dl).conforms);
}

#[test]
fn dl_requires_simple_properties_in_cardinalities() {
    // §11.2: a transitive property is not simple
    let o = strict(&decl(
        ":p a owl:TransitiveProperty .
         :A rdfs:subClassOf [ a owl:Restriction ; owl:onProperty :p ; owl:maxCardinality 1 ] .",
    ));
    assert_profiles(&o, &[], &[(Profile::Dl, "§11.2")]);
    // the sub property of a composite property is not simple either
    let o = strict(&decl(
        ":r owl:propertyChainAxiom ( :p :q ) . :r rdfs:subPropertyOf :e2 . :e2 a owl:ObjectProperty .
         :r a owl:FunctionalProperty .",
    ));
    assert_profiles(&o, &[], &[(Profile::Dl, "§11.2")]);
    let o = strict(&decl(
        ":p a owl:FunctionalProperty . :p rdfs:subPropertyOf :q .",
    ));
    assert_profiles(&o, &[Profile::Dl], &[]);
}

#[test]
fn dl_requires_regular_chains() {
    // p ∘ q → p and q ∘ p → q require p < q and q < p
    let o = strict(&decl(
        ":p owl:propertyChainAxiom ( :p :q ) . :q owl:propertyChainAxiom ( :q :p ) .",
    ));
    assert_profiles(&o, &[], &[(Profile::Dl, "§11.2")]);
    let o = strict(&decl(
        ":r owl:propertyChainAxiom ( :p :q ) . :r owl:propertyChainAxiom ( :r :p ) .",
    ));
    assert_profiles(&o, &[Profile::Dl], &[]);
}

#[test]
fn dl_checks_datatype_definitions() {
    let o = strict(&format!(
        "{HEADER}:t1 a rdfs:Datatype . :t2 a rdfs:Datatype .
         :t1 owl:equivalentClass :t2 . :t2 owl:equivalentClass :t1 ."
    ));
    assert_profiles(&o, &[], &[(Profile::Dl, "§11")]);
}

#[test]
fn dl_restricts_anonymous_individuals() {
    // §11.2: anonymous individuals may not form a cycle
    let o = strict(&decl("_:x :p _:y . _:y :p _:x ."));
    assert_profiles(&o, &[], &[(Profile::Dl, "§11")]);
    let o = strict(&decl("_:x :p _:y . _:y :p :a ."));
    assert_profiles(&o, &[Profile::Dl], &[]);
}

#[test]
fn dl_counts_unmapped_triples() {
    let o = lenient(&decl(":A rdfs:subClassOf \"not a class\" ."));
    assert_profiles(&o, &[], &[(Profile::Dl, "§3")]);
}

/// spec F21 A5: a universal and a functional property are two EL violations, each
/// naming its axiom, and both are in RL.
#[test]
fn el_names_the_axioms_it_refuses() {
    let o = strict(&decl(
        ":A rdfs:subClassOf [ a owl:Restriction ; owl:onProperty :p ; owl:allValuesFrom :B ] .
         :p a owl:FunctionalProperty .",
    ));
    let r = report(&o, Profile::El);
    let named: Vec<String> = r
        .violations
        .iter()
        .map(|v| v.axiom.as_ref().expect("an axiom").to_string())
        .collect();
    assert_eq!(r.violations.len(), 2, "{:?}", rules(&r));
    assert!(
        named.iter().any(|a| a.starts_with("SubClassOf(")),
        "{named:?}"
    );
    assert!(
        named
            .iter()
            .any(|a| a.starts_with("FunctionalObjectProperty(")),
        "{named:?}"
    );
    assert_profiles(&o, &[Profile::Rl], &[]);
}

/// spec F21 A6, without F17: a transitive property takes an ontology out of QL with
/// one violation.
#[test]
fn ql_refuses_one_transitive_property() {
    let body =
        ":A rdfs:subClassOf [ a owl:Restriction ; owl:onProperty :p ; owl:someValuesFrom :B ] .
         :B rdfs:subClassOf :C .";
    assert_profiles(&strict(&decl(body)), &[Profile::Ql], &[]);
    let o = strict(&decl(&format!("{body}\n:p a owl:TransitiveProperty .")));
    let r = report(&o, Profile::Ql);
    assert_eq!(r.violations.len(), 1, "{:?}", rules(&r));
}

#[test]
fn profile_reports_include_dl_violations() {
    let o = lenient(&decl(":x a owl:ObjectProperty , owl:DatatypeProperty ."));
    for p in [Profile::El, Profile::Ql, Profile::Rl] {
        assert!(!report(&o, p).conforms, "{p}");
    }
}

#[test]
fn reports_are_bounded() {
    let mut body = String::new();
    for n in 0..50 {
        body.push_str(&format!(":A{n} rdfs:subClassOf [ a owl:Restriction ; owl:onProperty :p ; owl:allValuesFrom :B ] .\n:A{n} a owl:Class .\n"));
    }
    let o = strict(&decl(&body));
    let r = profiles::check(&o, &[Profile::El], CheckOptions { max_violations: 10 }).remove(0);
    assert_eq!(r.violations.len(), 10);
    assert_eq!(r.total, 50);
    assert!(r.truncated);
}

// ---- random axioms -------------------------------------------------------------

mod random {
    use super::*;
    use proptest::prelude::*;

    fn named_class() -> impl Strategy<Value = ClassExpression> {
        (0..4u8).prop_map(|n| class(&format!("C{n}")))
    }

    fn ope() -> impl Strategy<Value = ObjectPropertyExpression> {
        prop_oneof![
            (0..3u8).prop_map(|n| ObjectPropertyExpression::Property(op(&format!("p{n}")))),
            (0..3u8).prop_map(|n| ObjectPropertyExpression::InverseOf(op(&format!("p{n}")))),
        ]
    }

    fn individual() -> impl Strategy<Value = Individual> {
        (0..3u8).prop_map(|n| ind(&format!("i{n}")))
    }

    fn data_range() -> impl Strategy<Value = DataRange> {
        prop_oneof![
            Just(DataRange::Datatype(Datatype::new(
                "http://www.w3.org/2001/XMLSchema#integer"
            ))),
            Just(DataRange::Datatype(Datatype::new(
                "http://www.w3.org/2001/XMLSchema#string"
            ))),
            Just(DataRange::OneOf(
                [Literal::simple("a"), Literal::simple("b")].into()
            )),
        ]
    }

    fn class_expression() -> impl Strategy<Value = ClassExpression> {
        named_class().prop_recursive(2, 8, 3, |inner| {
            prop_oneof![
                prop::collection::btree_set(inner.clone(), 2..4)
                    .prop_filter("two operands", |s| s.len() >= 2)
                    .prop_map(ClassExpression::ObjectIntersectionOf),
                prop::collection::btree_set(inner.clone(), 2..4)
                    .prop_filter("two operands", |s| s.len() >= 2)
                    .prop_map(ClassExpression::ObjectUnionOf),
                inner.clone().prop_map(ClassExpression::not),
                (ope(), inner.clone()).prop_map(|(p, c)| ClassExpression::some(p, c)),
                (ope(), inner.clone()).prop_map(|(p, c)| ClassExpression::all(p, c)),
                (0..3u32, ope(), proptest::option::of(inner.clone())).prop_map(|(n, p, c)| {
                    ClassExpression::ObjectMinCardinality(n, p, c.map(Box::new))
                }),
                (ope(), individual()).prop_map(|(p, i)| ClassExpression::ObjectHasValue(p, i)),
                ope().prop_map(ClassExpression::ObjectHasSelf),
                prop::collection::btree_set(individual(), 1..3)
                    .prop_map(ClassExpression::ObjectOneOf),
                (0..2u8, data_range()).prop_map(|(n, r)| ClassExpression::DataSomeValuesFrom(
                    vec![dp(&format!("d{n}"))],
                    r
                )),
                (0..3u32, 0..2u8, proptest::option::of(data_range())).prop_map(|(k, n, r)| {
                    ClassExpression::DataMaxCardinality(k, dp(&format!("d{n}")), r)
                }),
            ]
        })
    }

    fn axiom() -> impl Strategy<Value = Axiom> {
        let kind = prop_oneof![
            (class_expression(), class_expression()).prop_map(|(a, b)| AxiomKind::SubClassOf(a, b)),
            (class_expression(), class_expression())
                .prop_filter("distinct", |(a, b)| a != b)
                .prop_map(|(a, b)| AxiomKind::EquivalentClasses([a, b].into())),
            prop::collection::btree_set(class_expression(), 2..4)
                .prop_filter("two operands", |s| s.len() >= 2)
                .prop_map(AxiomKind::DisjointClasses),
            (ope(), class_expression()).prop_map(|(p, c)| AxiomKind::ObjectPropertyDomain(p, c)),
            (prop::collection::vec(ope(), 2..4), ope()).prop_map(|(c, p)| {
                AxiomKind::SubObjectPropertyOf(SubObjectPropertyExpression::Chain(c), p)
            }),
            ope().prop_map(AxiomKind::TransitiveObjectProperty),
            (class_expression(), individual()).prop_map(|(c, i)| AxiomKind::ClassAssertion(c, i)),
            (0..3u8, individual(), individual()).prop_map(|(n, a, b)| {
                AxiomKind::ObjectPropertyAssertion(op(&format!("p{n}")).into(), a, b)
            }),
            (ope(), individual(), individual())
                .prop_map(|(p, a, b)| AxiomKind::NegativeObjectPropertyAssertion(p, a, b)),
            (0..2u8, data_range())
                .prop_map(|(n, r)| AxiomKind::DataPropertyRange(dp(&format!("d{n}")), r)),
        ];
        (kind, proptest::option::of("[a-z]{1,6}")).prop_map(|(k, note)| {
            let ax = Axiom::new(k);
            match note {
                Some(n) => ax.annotated(Annotation::comment(&n)),
                None => ax,
            }
        })
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(256))]

        /// spec F21 A2: axioms written to triples and read again are the same axioms.
        /// An axiom and an annotated copy of it share their main triple, which reads
        /// back as the annotated axiom only, so the axioms differ in their operands.
        #[test]
        fn write_then_read(axioms in prop::collection::vec(axiom(), 1..6)) {
            let axioms: Vec<Axiom> = axioms
                .into_iter()
                .map(|a| (a.without_annotations(), a))
                .collect::<std::collections::BTreeMap<_, _>>()
                .into_values()
                .collect();
            round_trip(&axioms);
        }

        #[test]
        fn json_then_read(ax in axiom()) {
            prop_assert_eq!(json::read_axiom(&json::axiom(&ax)).unwrap(), ax);
        }

        /// The functional-style writer's document reads back as the same axioms.
        #[test]
        fn ofn_then_read(axioms in prop::collection::vec(axiom(), 1..6)) {
            let r = sparkles_owl::ofn::Renderer::new([("ex", "http://example.org/")]);
            let header = Header { iri: Some(iri(ex("onto"))), ..Default::default() };
            let doc = sparkles_owl::ofn::document(&r, &header, &axioms);
            let back = sparkles_owl::ofn_read::parse(&doc).unwrap();
            prop_assert_eq!(back.header, header);
            prop_assert_eq!(back.axioms, axioms);
        }
    }
}

// ---- review regressions ----------------------------------------------------------

#[test]
fn deep_nesting_stays_unmapped() {
    // a chain of complements far deeper than the read follows: no stack overflow
    let mut body = decl(":A rdfs:subClassOf _:c0 .\n");
    for i in 0..20_000 {
        body.push_str(&format!(
            "_:c{i} a owl:Class ; owl:complementOf _:c{} .\n",
            i + 1
        ));
    }
    body.push_str("_:c20000 a owl:Class ; owl:complementOf :B .\n");
    let o = std::thread::Builder::new()
        .stack_size(2 << 20)
        .spawn(move || lenient(&body))
        .unwrap()
        .join()
        .expect("the read does not overflow its stack");
    assert!(o.diagnostics.iter().any(|d| d.code == "nesting"));
    assert!(!o.unmapped.is_empty());
    assert!(!report(&o, Profile::Dl).conforms);
}

#[test]
fn mentions_reaches_annotation_values() {
    let see = "http://example.org/E";
    let o = strict(&decl(
        "[] a owl:Axiom ; owl:annotatedSource :A ; owl:annotatedProperty rdfs:subClassOf ;
            owl:annotatedTarget :B ; rdfs:seeAlso :E .
         :A rdfs:subClassOf :B .
         _:x rdfs:seeAlso :E .",
    ));
    let hits: Vec<String> = o
        .axiom_set()
        .filter(|a| a.mentions(see))
        .map(|a| a.to_string())
        .collect();
    assert_eq!(hits.len(), 2, "{hits:?}");
}

#[test]
fn normal_forms_follow_the_reverse_mapping() {
    let eq = Axiom::from(AxiomKind::EquivalentClasses(
        [class("A"), class("B"), class("C")].into_iter().collect(),
    ));
    let read: BTreeSet<Axiom> = {
        let mut fresh = 0;
        let triples = write::axiom_triples(&eq, &mut || {
            fresh += 1;
            oxrdf::BlankNode::new_unchecked(format!("n{fresh}"))
        });
        let mut body = decl("");
        for t in triples {
            body.push_str(&format!("{t} .\n"));
        }
        strict(&body)
            .axiom_set()
            .filter(|a| a.axiom_type() == AxiomType::EquivalentClasses)
            .cloned()
            .collect()
    };
    assert_eq!(eq.normal_forms().into_iter().collect::<BTreeSet<_>>(), read);
    let inv = Axiom::from(AxiomKind::ObjectPropertyAssertion(
        ObjectPropertyExpression::InverseOf(op("p")),
        ind("a"),
        ind("b"),
    ));
    assert_eq!(
        inv.normal_forms(),
        [Axiom::from(AxiomKind::ObjectPropertyAssertion(
            ObjectPropertyExpression::Property(op("p")),
            ind("b"),
            ind("a"),
        ))]
    );
}

#[test]
fn dl_refuses_a_datatype_defined_from_itself() {
    let o = strict(&format!(
        "{HEADER}:t a rdfs:Datatype .
         :t owl:equivalentClass [ a rdfs:Datatype ; owl:datatypeComplementOf :t ] ."
    ));
    assert_profiles(&o, &[], &[(Profile::Dl, "§11")]);
}

#[test]
fn el_ranges_pass_through_equivalent_properties() {
    let o = strict(&decl(
        ":s a owl:ObjectProperty . :r owl:propertyChainAxiom ( :p :q ) . :r owl:equivalentProperty :s . :s rdfs:range :A .",
    ));
    assert_profiles(&o, &[Profile::Dl], &[(Profile::El, "§2.2.6")]);
}

#[test]
fn nesting_is_bounded_bottom_up_and_independent_of_order() {
    // each _:c{i} is the superclass of :A{i} and the complement inside _:c{i+1}, so a
    // read that reaches the shallow ones first builds deeper expressions from memos
    let n = 1000;
    let mut body = decl("_:c0 a owl:Class ; owl:complementOf :B .\n");
    for i in 0..n {
        body.push_str(&format!(":A{i} a owl:Class ; rdfs:subClassOf _:c{i} .\n"));
        if i > 0 {
            body.push_str(&format!(
                "_:c{i} a owl:Class ; owl:complementOf _:c{} .\n",
                i - 1
            ));
        }
    }
    let o = std::thread::Builder::new()
        .stack_size(2 << 20)
        .spawn(move || lenient(&body))
        .unwrap()
        .join()
        .expect("the read does not overflow its stack");
    let sub = |i: usize| {
        o.axiom_set().any(|a| {
            matches!(&a.kind, AxiomKind::SubClassOf(ClassExpression::Class(c), _)
                if c.as_str() == format!("http://example.org/A{i}"))
        })
    };
    assert!(sub(0) && sub(5) && sub(100), "shallow expressions are read");
    assert!(!sub(n - 1), "the deepest expression is not");
    assert!(o.diagnostics.iter().any(|d| d.code == "nesting"));
}

#[test]
fn a_too_deep_root_does_not_spoil_a_shallow_suffix() {
    // datatype definitions: the outermost one is too deep, the innermost two are not
    let n = 200;
    let mut body = format!(
        "{HEADER}:T a rdfs:Datatype . :U a rdfs:Datatype .\n:T owl:equivalentClass _:d{} .\n",
        n - 1
    );
    // the read of :T visits _:d100 before the limit stops it
    body.push_str(":U owl:equivalentClass _:d100 .\n");
    body.push_str("_:d0 a rdfs:Datatype ; owl:datatypeComplementOf xsd:integer .\n");
    for i in 1..n {
        body.push_str(&format!(
            "_:d{i} a rdfs:Datatype ; owl:datatypeComplementOf _:d{} .\n",
            i - 1
        ));
    }
    let o = lenient(&body);
    let defined = |t: &str| {
        o.axiom_set().any(|a| {
            matches!(&a.kind, AxiomKind::DatatypeDefinition(d, _)
                if d.as_str() == format!("http://example.org/{t}"))
        })
    };
    assert!(defined("U"));
    assert!(!defined("T"));
}

#[test]
fn class_and_data_depths_stay_apart() {
    // _:x is a shallow class expression and the root of a data range 128 levels deep:
    // the class read after the data range must not borrow its depth
    let n = 126;
    let mut body = decl(
        ":A rdfs:subClassOf _:x .
         :T a rdfs:Datatype ; owl:equivalentClass _:x .
         :C rdfs:subClassOf _:y . _:y owl:complementOf _:x .
         _:x owl:complementOf :B ; owl:datatypeComplementOf _:d0 .\n",
    );
    for i in 0..n {
        body.push_str(&format!(
            "_:d{i} a rdfs:Datatype ; owl:datatypeComplementOf _:d{} .\n",
            i + 1
        ));
    }
    body.push_str(&format!(
        "_:d{n} a rdfs:Datatype ; owl:datatypeComplementOf xsd:integer .\n"
    ));
    let o = lenient(&body);
    for c in ["A", "C"] {
        assert!(
            o.axiom_set().any(
                |a| matches!(&a.kind, AxiomKind::SubClassOf(ClassExpression::Class(k), _)
                if k.as_str() == format!("http://example.org/{c}"))
            ),
            "{c}: {:?}",
            o.diagnostics
        );
    }
}

#[test]
fn a_failed_class_read_does_not_shorten_a_data_range() {
    // every _:d{i} defines :T{i} and is also used, in vain, as a class: the deep data
    // ranges built from the memo still meet the nesting limit
    let n = 200;
    let mut body =
        format!("{HEADER}_:d0 a rdfs:Datatype ; owl:datatypeComplementOf xsd:integer .\n");
    for i in 0..n {
        body.push_str(&format!(
            ":T{i} a rdfs:Datatype ; owl:equivalentClass _:d{i} . :K{i} a owl:Class ; rdfs:subClassOf _:d{i} .\n"
        ));
        if i > 0 {
            body.push_str(&format!(
                "_:d{i} a rdfs:Datatype ; owl:datatypeComplementOf _:d{} .\n",
                i - 1
            ));
        }
    }
    let o = lenient(&body);
    assert!(
        !o.axiom_set()
            .any(|a| matches!(&a.kind, AxiomKind::DatatypeDefinition(d, _)
            if d.as_str() == format!("http://example.org/T{}", n - 1))),
        "the deepest definition is not read"
    );
}

// ---- the functional-style syntax reader -------------------------------------------

#[test]
fn ofn_reads_a_document() {
    use sparkles_owl::ofn_read;
    let doc = ofn_read::parse(
        r#"Prefix(:=<http://example.org/>)
# a comment
Prefix(dc:=<http://purl.org/dc/elements/1.1/>)
Ontology(<http://example.org/onto> <http://example.org/onto/1>
  Import(<http://example.org/other>)
  Annotation(dc:title "Uni"@en)
  Declaration(Class(:Professor))
  Declaration(ObjectProperty(:teaches))
  SubClassOf(Annotation(rdfs:comment "says \"hi\" \\ there") :Professor ObjectSomeValuesFrom(:teaches :Course))
  DataPropertyAssertion(:age _:x "41"^^xsd:integer)
  AnnotationAssertion(rdfs:label :Professor "Prof@de"^^rdf:PlainLiteral)
  AnnotationAssertion(rdfs:label :Course "Course@"^^rdf:PlainLiteral)
  HasKey(:Professor () (:age))
  DataSomeValuesFrom(:a :b xsd:string)
)"#,
    );
    // the last line is a class expression, not an axiom
    let e = doc.unwrap_err();
    assert_eq!(e.line, 14, "{e}");
    assert!(e.message.contains("not an axiom"), "{e}");
    let doc = ofn_read::parse(
        r#"Prefix(:=<http://example.org/>)
Ontology(<http://example.org/onto> <http://example.org/onto/1>
  Import(<http://example.org/other>)
  Annotation(rdfs:comment "Uni"@en)
  SubClassOf(Annotation(rdfs:comment "says \"hi\" \\ there") :Professor ObjectSomeValuesFrom(:teaches :Course))
  DataPropertyAssertion(:age _:x "41"^^xsd:integer)
  AnnotationAssertion(rdfs:label :Professor "Prof@de"^^rdf:PlainLiteral)
  AnnotationAssertion(rdfs:label :Course "Course@"^^rdf:PlainLiteral)
  SubClassOf(:A DataSomeValuesFrom(:d1 :d2 DatatypeRestriction(xsd:integer xsd:minInclusive "1"^^xsd:integer)))
)"#,
    )
    .unwrap();
    assert_eq!(doc.header.iri.as_ref().unwrap().as_str(), ex("onto"));
    assert_eq!(
        doc.header.version_iri.as_ref().unwrap().as_str(),
        ex("onto/1")
    );
    assert_eq!(doc.header.imports.len(), 1);
    assert_eq!(doc.prefixes, vec![(String::new(), ex(""))]);
    let got: Vec<String> = doc.axioms.iter().map(|a| a.to_string()).collect();
    assert_eq!(
        got,
        [
            "SubClassOf(Annotation(rdfs:comment \"says \\\"hi\\\" \\\\ there\") <http://example.org/Professor> ObjectSomeValuesFrom(<http://example.org/teaches> <http://example.org/Course>))",
            "DataPropertyAssertion(<http://example.org/age> _:x \"41\"^^xsd:integer)",
            "AnnotationAssertion(rdfs:label <http://example.org/Professor> \"Prof\"@de)",
            "AnnotationAssertion(rdfs:label <http://example.org/Course> \"Course\")",
            "SubClassOf(<http://example.org/A> DataSomeValuesFrom(<http://example.org/d1> <http://example.org/d2> DatatypeRestriction(xsd:integer xsd:minInclusive \"1\"^^xsd:integer)))",
        ]
    );
    // to triples, and read strictly with the declarations a strict read needs
    let mut fresh = write::counter("f");
    let o = read::read_graph(ofn_read::triples(&doc, &mut fresh), ReadOptions::default());
    assert!(o.contains(&doc.axioms[0]));
}

#[test]
fn ofn_reports_errors_with_positions() {
    use sparkles_owl::ofn_read::parse;
    let err = |text: &str| parse(text).unwrap_err();
    let e = err("Ontology(\n  SubClassOf(ex:A ex:B))");
    assert_eq!((e.line, e.column), (2, 14), "{e}");
    assert!(e.message.contains("prefix 'ex:'"), "{e}");
    assert!(
        err("Ontology(SubClassOf(owl:Thing))")
            .message
            .contains("expected")
    );
    assert!(err("Ontology(").message.contains("not closed"));
    assert!(err("Ontology() x").message.contains("end of the document"));
    assert!(
        err("Ontology(Declaration(Thing(owl:A)))")
            .message
            .contains("entity kind")
    );
    assert!(
        err("Ontology(DisjointClasses(owl:Thing owl:Thing))")
            .message
            .contains("operands")
    );
    assert!(
        err("Ontology(AnnotationAssertion(rdfs:label owl:Thing \"a\\n\"))")
            .message
            .contains("escape")
    );
    assert!(
        err("Ontology(SubClassOf(owl:Thing ObjectMinCardinality(99999999999 owl:p)))")
            .message
            .contains("too large")
    );
    assert!(
        err("Ontology(SubClassOf(<a b> owl:Thing))")
            .message
            .contains("IRI")
    );
    assert!(
        err("Ontology(SubClassOf(<rel> owl:Thing))")
            .message
            .contains("IRI")
    );
    // a nesting deeper than the reader follows is an error, not a stack overflow
    let mut deep = String::from("Ontology(SubClassOf(owl:Thing ");
    for _ in 0..10_000 {
        deep.push_str("ObjectComplementOf(");
    }
    deep.push_str("owl:Nothing");
    deep.push_str(&")".repeat(10_000));
    deep.push_str("))");
    let e = std::thread::Builder::new()
        .stack_size(2 << 20)
        .spawn(move || parse(&deep).unwrap_err())
        .unwrap()
        .join()
        .unwrap();
    assert!(e.message.contains("nest"), "{e}");
}

#[test]
fn ofn_reader_streams_axioms() {
    let text = "Prefix(:=<http://example.org/>) Ontology(<http://example.org/o> SubClassOf(:A :B) SubClassOf(:B :C) SubClassOf(:C)";
    let mut r = sparkles_owl::ofn_read::Reader::new(text).unwrap();
    assert_eq!(r.header().iri.as_ref().unwrap().as_str(), ex("o"));
    assert!(r.next().unwrap().is_ok());
    assert!(r.next().unwrap().is_ok());
    assert!(r.next().unwrap().is_err());
    assert!(r.next().is_none());
}

#[test]
fn ofn_reads_one_axiom_with_prefixes() {
    use sparkles_owl::ofn_read::parse_axiom;
    let p = vec![("ex".to_string(), ex(""))];
    let ax = parse_axiom("SubClassOf(ex:A ObjectSomeValuesFrom(ex:p owl:Thing))", &p).unwrap();
    assert_eq!(
        ax,
        Axiom::sub_class_of(class("A"), ClassExpression::some(op("p"), Class::thing()))
    );
    assert!(parse_axiom("SubClassOf(ex:A ex:B) SubClassOf(ex:B ex:C)", &p).is_err());
    assert!(parse_axiom("SubClassOf(ex:A)", &p).is_err());
}

/// spec F21 A6 and F17 §7: a partition by the QL grammar keeps the axioms F17's
/// rewriting reads and sets the transitive property aside with its violation.
#[test]
fn ql_partition_sets_aside_the_axioms_outside_ql() {
    let body =
        ":A rdfs:subClassOf [ a owl:Restriction ; owl:onProperty :p ; owl:someValuesFrom :B ] .
         :B rdfs:subClassOf :C .";
    let inside = profiles::partition(&strict(&decl(body)), Profile::Ql);
    assert!(inside.outside.is_empty(), "{:?}", inside.outside);
    let o = strict(&decl(&format!("{body}\n:p a owl:TransitiveProperty .")));
    let part = profiles::partition(&o, Profile::Ql);
    let transitive = Axiom::new(AxiomKind::TransitiveObjectProperty(op("p").into()));
    assert_eq!(
        part.outside_axioms().into_iter().collect::<Vec<_>>(),
        vec![&transitive]
    );
    assert_eq!(part.inside.len(), inside.inside.len());
    assert!(!part.inside.contains(&transitive));
    assert!(
        part.outside[0].rule.starts_with("OWL 2 Profiles"),
        "{:?}",
        part.outside
    );
}

/// A partition applies the profile's own grammar, not the DL global restrictions that a
/// check adds to every report.
#[test]
fn partitions_leave_out_the_dl_restrictions() {
    let o = lenient(&decl(":x a owl:ObjectProperty , owl:DatatypeProperty ."));
    assert!(!report(&o, Profile::Rl).conforms);
    assert!(profiles::partition(&o, Profile::Rl).outside.is_empty());
    let union = strict(&decl(":A owl:disjointUnionOf ( :B :C ) ."));
    let part = profiles::partition(&union, Profile::Rl);
    assert_eq!(part.outside_axioms().len(), 1, "{:?}", part.outside);
}

#[test]
fn xml_literal_namespace_fixes_report_section57() {
    for xml in [
        "<p:a xmlns:p='urn:a b'/>",
        "<a xmlns:unused='urn:a&#xA;b'/>",
        "<a xmlns='urn:%'/>",
        "<p:a xmlns:p='urn:&#x+41;'/>",
        "<a><!--x---></a>",
    ] {
        let literal = xml_literal_namespaces_turtle(xml);
        for body in [
            format!(":a :d {literal} ."),
            format!(":A rdfs:comment {literal} ."),
            format!("<http://example.org/onto> rdfs:comment {literal} ."),
        ] {
            let o = strict(&decl(&body));
            let r = report(&o, Profile::Dl);
            assert!(
                !r.conforms && r.violations.iter().any(|v| v.rule.ends_with("§5.7")),
                "{xml:?} in {body:?}: {:?}",
                r.violations
            );
        }
    }
}
