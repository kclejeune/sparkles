//! Round-trip, edge-case, escaping, lossy-reporting, bound and determinism tests for
//! the OWL/XML (`.owx`), Manchester (`.omn`), OBO 1.4 (`.obo`) and OBO Graphs JSON
//! (`.json`) ontology writers and readers.

use sparkles_owl::manchester;
use sparkles_owl::obo::{
    self, IAO_DEFINITION, IAO_REPLACED_BY, OIO_AUTO_GENERATED_BY, OIO_CONSIDER, OIO_CREATED_BY,
    OIO_CREATION_DATE, OIO_DATE, OIO_DEFAULT_NAMESPACE, OIO_HAS_ALT_ID, OIO_HAS_BROAD_SYNONYM,
    OIO_HAS_DBXREF, OIO_HAS_EXACT_SYNONYM, OIO_HAS_NARROW_SYNONYM, OIO_HAS_OBO_NAMESPACE,
    OIO_HAS_RELATED_SYNONYM, OIO_HAS_SYNONYM_TYPE, OIO_IN_SUBSET, OIO_IS_INFERRED, OIO_SAVED_BY,
};
use sparkles_owl::obographs;
use sparkles_owl::ofn::Renderer;
use sparkles_owl::owx;
use sparkles_owl::transform::Document;
use sparkles_owl::vocab;
use sparkles_owl::*;
use std::collections::BTreeSet;
use std::io::Write;

const EX: &str = "http://ex.org/";
const OBO: &str = "http://purl.obolibrary.org/obo/";

fn ex(s: &str) -> String {
    format!("{EX}{s}")
}
fn obo_iri(s: &str) -> String {
    format!("{OBO}{s}")
}
fn cls(s: &str) -> Class {
    Class::new(ex(s))
}
fn op(s: &str) -> ObjectProperty {
    ObjectProperty::new(ex(s))
}
fn dp(s: &str) -> DataProperty {
    DataProperty::new(ex(s))
}
fn ap(s: &str) -> AnnotationProperty {
    AnnotationProperty::new(ex(s))
}
fn dt(s: &str) -> Datatype {
    Datatype::new(ex(s))
}
fn ind(s: &str) -> Individual {
    Individual::named(ex(s))
}
fn renderer() -> Renderer {
    Renderer::new([("", EX), ("obo", OBO), ("oboInOwl", obo::OBO_IN_OWL)])
}

/// Build an ontology exercising all 37 axiom kinds, all 18 class expressions, all 6
/// data ranges, nested annotations, anonymous individuals, and repeated Multiset operands.
fn full_owl_document() -> Document {
    let mut header = Header {
        iri: Some(iri(ex("full-onto"))),
        version_iri: Some(iri(ex("full-onto/v1"))),
        ..Default::default()
    };
    header.imports.insert(iri(ex("imported-1")));
    header.imports.insert(iri("http://other.example/imp2"));
    let nested_ann = Annotation {
        property: AnnotationProperty::new(vocab::RDFS_COMMENT),
        value: AnnotationValue::Literal(Literal::simple("nested \"note\" <1> & 2")),
        annotations: [Annotation::new(
            ap("meta"),
            AnnotationValue::Literal(Literal::typed("42", vocab::XSD_INTEGER)),
        )]
        .into_iter()
        .collect(),
    };
    header.annotations.insert(Annotation {
        property: AnnotationProperty::new(vocab::RDFS_LABEL),
        value: AnnotationValue::Literal(Literal::lang("Full Ontology", "en").unwrap()),
        annotations: [nested_ann.clone()].into_iter().collect(),
    });

    let dr_restricted = DataRange::DatatypeRestriction(
        Datatype::new(vocab::XSD_INTEGER),
        [
            FacetRestriction {
                facet: iri(format!("{}minInclusive", vocab::XSD)),
                value: Literal::typed("0", vocab::XSD_INTEGER),
            },
            FacetRestriction {
                facet: iri(format!("{}maxExclusive", vocab::XSD)),
                value: Literal::typed("100", vocab::XSD_INTEGER),
            },
        ]
        .into_iter()
        .collect(),
    );
    let dr_union = DataRange::UnionOf(
        [
            DataRange::Datatype(Datatype::new(vocab::XSD_STRING)),
            DataRange::OneOf(
                [
                    Literal::simple("alpha"),
                    Literal::lang("beta", "fr").unwrap(),
                ]
                .into_iter()
                .collect(),
            ),
        ]
        .into_iter()
        .collect(),
    );
    let dr_complex = DataRange::IntersectionOf(
        [
            dr_restricted.clone(),
            DataRange::ComplementOf(Box::new(DataRange::OneOf(
                [Literal::typed("13", vocab::XSD_INTEGER)]
                    .into_iter()
                    .collect(),
            ))),
        ]
        .into_iter()
        .collect(),
    );

    let anon = AnonymousIndividual::new("bnode_1");
    let inv_p = ObjectPropertyExpression::InverseOf(op("p"));

    let axioms = vec![
        // 1. Declarations (all 6 kinds)
        Axiom::declaration(Entity::Class(cls("A"))),
        Axiom::declaration(Entity::Class(cls("B"))),
        Axiom::declaration(Entity::Class(cls("C"))),
        Axiom::declaration(Entity::Class(cls("D"))),
        Axiom::declaration(Entity::Datatype(dt("Age"))),
        Axiom::declaration(Entity::ObjectProperty(op("p"))),
        Axiom::declaration(Entity::ObjectProperty(op("q"))),
        Axiom::declaration(Entity::ObjectProperty(op("r"))),
        Axiom::declaration(Entity::DataProperty(dp("age"))),
        Axiom::declaration(Entity::DataProperty(dp("score"))),
        Axiom::declaration(Entity::AnnotationProperty(ap("note"))),
        Axiom::declaration(Entity::AnnotationProperty(ap("meta"))),
        Axiom::declaration(Entity::NamedIndividual(NamedIndividual::new(ex("i1")))),
        Axiom::declaration(Entity::NamedIndividual(NamedIndividual::new(ex("i2")))),
        Axiom::declaration(Entity::NamedIndividual(NamedIndividual::new(ex("i3")))),
        // 2-5. Class axioms covering all 18 class expressions
        Axiom::sub_class_of(
            cls("A"),
            ClassExpression::and([
                ClassExpression::Class(cls("B")),
                ClassExpression::or([
                    ClassExpression::Class(cls("C")),
                    ClassExpression::not(cls("D")),
                ]),
                ClassExpression::ObjectOneOf(
                    [ind("i1"), Individual::Anonymous(anon.clone())]
                        .into_iter()
                        .collect(),
                ),
            ]),
        )
        .annotated(nested_ann.clone()),
        Axiom::sub_class_of(
            cls("B"),
            ClassExpression::and([
                ClassExpression::some(op("p"), cls("C")),
                ClassExpression::all(inv_p.clone(), cls("D")),
                ClassExpression::ObjectHasValue(op("p").into(), ind("i1")),
                ClassExpression::ObjectHasSelf(op("q").into()),
                ClassExpression::ObjectMinCardinality(
                    1,
                    op("p").into(),
                    Some(Box::new(cls("C").into())),
                ),
                ClassExpression::ObjectMaxCardinality(3, inv_p.clone(), None),
                ClassExpression::ObjectExactCardinality(
                    2,
                    op("q").into(),
                    Some(Box::new(cls("D").into())),
                ),
            ]),
        ),
        Axiom::sub_class_of(
            cls("C"),
            ClassExpression::and([
                ClassExpression::DataSomeValuesFrom(vec![dp("age")], dr_restricted.clone()),
                ClassExpression::DataAllValuesFrom(vec![dp("age"), dp("score")], dr_union.clone()),
                ClassExpression::DataHasValue(dp("age"), Literal::typed("21", vocab::XSD_INTEGER)),
                ClassExpression::DataMinCardinality(1, dp("age"), Some(dr_restricted.clone())),
                ClassExpression::DataMaxCardinality(2, dp("score"), None),
                ClassExpression::DataExactCardinality(
                    1,
                    dp("age"),
                    Some(DataRange::Datatype(Datatype::new(vocab::XSD_INTEGER))),
                ),
            ]),
        ),
        Axiom::equivalent_classes([cls("A").into(), cls("B").into(), cls("C").into()]),
        Axiom::disjoint_classes([cls("B").into(), cls("C").into(), cls("C").into()]), // Multiset dup!
        Axiom::disjoint_union(
            cls("D"),
            [cls("A").into(), cls("B").into(), cls("B").into()],
        ),
        // 6-18. Object property axioms
        Axiom::new(AxiomKind::SubObjectPropertyOf(
            SubObjectPropertyExpression::Expression(op("p").into()),
            op("q").into(),
        )),
        Axiom::new(AxiomKind::SubObjectPropertyOf(
            SubObjectPropertyExpression::Chain(vec![op("p").into(), inv_p.clone()]),
            op("r").into(),
        )),
        Axiom::new(AxiomKind::EquivalentObjectProperties(
            [op("p").into(), op("q").into()].into_iter().collect(),
        )),
        Axiom::disjoint_object_properties([op("p").into(), op("r").into(), op("r").into()]),
        Axiom::new(AxiomKind::InverseObjectProperties(
            op("p").into(),
            op("q").into(),
        )),
        Axiom::new(AxiomKind::ObjectPropertyDomain(
            op("p").into(),
            cls("A").into(),
        )),
        Axiom::new(AxiomKind::ObjectPropertyRange(
            op("p").into(),
            cls("B").into(),
        )),
        Axiom::new(AxiomKind::FunctionalObjectProperty(op("p").into())),
        Axiom::new(AxiomKind::InverseFunctionalObjectProperty(op("p").into())),
        Axiom::new(AxiomKind::ReflexiveObjectProperty(op("q").into())),
        Axiom::new(AxiomKind::IrreflexiveObjectProperty(op("r").into())),
        Axiom::new(AxiomKind::SymmetricObjectProperty(op("q").into())),
        Axiom::new(AxiomKind::AsymmetricObjectProperty(op("r").into())),
        Axiom::new(AxiomKind::TransitiveObjectProperty(op("p").into())),
        // 19-24. Data property axioms
        Axiom::new(AxiomKind::SubDataPropertyOf(dp("age"), dp("score"))),
        Axiom::new(AxiomKind::EquivalentDataProperties(
            [dp("age"), dp("score")].into_iter().collect(),
        )),
        Axiom::disjoint_data_properties([dp("age"), dp("score"), dp("score")]),
        Axiom::new(AxiomKind::DataPropertyDomain(dp("age"), cls("A").into())),
        Axiom::new(AxiomKind::DataPropertyRange(dp("age"), dr_complex.clone())),
        Axiom::new(AxiomKind::FunctionalDataProperty(dp("age"))),
        // 25-26. DatatypeDefinition & HasKey
        Axiom::new(AxiomKind::DatatypeDefinition(dt("Age"), dr_complex)),
        Axiom::new(AxiomKind::HasKey(
            cls("A").into(),
            [op("p").into()].into_iter().collect(),
            [dp("age")].into_iter().collect(),
        )),
        // 27-33. Assertions
        Axiom::new(AxiomKind::SameIndividual(
            [ind("i1"), ind("i2")].into_iter().collect(),
        )),
        Axiom::different_individuals([ind("i1"), ind("i2"), ind("i2")]),
        Axiom::class_assertion(cls("A"), ind("i1")),
        Axiom::new(AxiomKind::ObjectPropertyAssertion(
            op("p").into(),
            ind("i1"),
            Individual::Anonymous(anon.clone()),
        )),
        Axiom::new(AxiomKind::NegativeObjectPropertyAssertion(
            op("q").into(),
            ind("i1"),
            ind("i3"),
        )),
        Axiom::new(AxiomKind::DataPropertyAssertion(
            dp("age"),
            ind("i1"),
            Literal::typed("30", vocab::XSD_INTEGER),
        )),
        Axiom::new(AxiomKind::NegativeDataPropertyAssertion(
            dp("age"),
            ind("i2"),
            Literal::simple("bad\r\n<tag>&\"quote\"'apos'\t"),
        )),
        // 34-37. Annotation axioms
        Axiom::new(AxiomKind::AnnotationAssertion(
            ap("note"),
            AnnotationSubject::Iri(iri(ex("A"))),
            AnnotationValue::Literal(Literal::simple("class A note")),
        )),
        Axiom::new(AxiomKind::AnnotationAssertion(
            ap("note"),
            AnnotationSubject::Anonymous(anon.clone()),
            AnnotationValue::Iri(iri("http://unprefixed.example/ref?a=1&b=2")),
        )),
        Axiom::new(AxiomKind::AnnotationAssertion(
            ap("note"),
            AnnotationSubject::Iri(iri(ex("B"))),
            AnnotationValue::Anonymous(anon),
        )),
        Axiom::new(AxiomKind::SubAnnotationPropertyOf(ap("note"), ap("meta"))),
        Axiom::new(AxiomKind::AnnotationPropertyDomain(
            ap("note"),
            iri(ex("A")),
        )),
        Axiom::new(AxiomKind::AnnotationPropertyRange(
            ap("note"),
            iri(vocab::XSD_STRING),
        )),
    ];

    Document::new(header, axioms)
}

// ===================================================================== OWL/XML ===

#[test]
fn owx_full_round_trip_all_37_axioms_and_determinism() {
    let doc = full_owl_document();
    let r = renderer();
    let xml1 = owx::document(&r, &doc.header, doc.axioms.iter()).unwrap();
    let xml2 = owx::document(&r, &doc.header, doc.axioms.iter()).unwrap();
    assert_eq!(xml1, xml2, "OWL/XML output must be deterministic");

    let back = owx::parse(&xml1).unwrap();
    let back_doc = Document::new(back.header, back.axioms);
    assert_eq!(
        back_doc, doc,
        "OWL/XML must round-trip all 37 axiom kinds losslessly"
    );
}

#[test]
fn owx_escaping_control_char_refusal_and_nesting_bound() {
    let r = renderer();
    // Special XML characters including \r, \n, \t, <, >, &, ", ', Unicode
    let special = "line1\r\nline2\t<a & b=\"c\" 'd'> — λ 🦀";
    let ax = Axiom::new(AxiomKind::AnnotationAssertion(
        AnnotationProperty::new(vocab::RDFS_COMMENT),
        AnnotationSubject::Iri(iri(ex("A"))),
        AnnotationValue::Literal(Literal::simple(special)),
    ));
    let xml = owx::document(&r, &Header::default(), [&ax]).unwrap();
    assert!(
        xml.contains("&#xD;"),
        "CR must be escaped to survive XML normalization: {xml}"
    );
    let back = owx::parse(&xml).unwrap();
    assert_eq!(back.axioms, vec![ax]);

    // Illegal XML 1.0 NUL / control char must be refused cleanly
    let bad = Axiom::new(AxiomKind::AnnotationAssertion(
        AnnotationProperty::new(vocab::RDFS_COMMENT),
        AnnotationSubject::Iri(iri(ex("A"))),
        AnnotationValue::Literal(Literal::simple("bad\0nul")),
    ));
    let err = owx::document(&r, &Header::default(), [&bad]).unwrap_err();
    assert!(matches!(err, WriteError::Unsupported(_)), "{err:?}");

    // Deep nesting past MAX_NESTING must be refused
    let mut deep = ClassExpression::Class(cls("A"));
    for _ in 0..140 {
        deep = ClassExpression::not(deep);
    }
    let deep_ax = Axiom::sub_class_of(cls("B"), deep);
    let err = owx::document(&r, &Header::default(), [&deep_ax]).unwrap_err();
    assert!(matches!(err, WriteError::Unsupported(_)), "{err:?}");

    // Streaming sink works without error
    struct Counter(usize);
    impl Write for Counter {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0 += buf.len();
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let doc = full_owl_document();
    let mut sink = Counter(0);
    let rep = owx::write_document(&mut sink, &r, &doc.header, doc.axioms.iter()).unwrap();
    assert_eq!(rep.written_axioms, doc.axioms.len());
    assert!(rep.is_lossless());
    assert!(sink.0 > 0);
}

// ================================================================== Manchester ===

#[test]
fn manchester_full_document_round_trip_and_determinism() {
    // Filter out only the single n-ary DataAllValuesFrom([age, score], ...) axiom from
    // full_owl_document, since Manchester does not support n-ary data restrictions.
    let mut doc = full_owl_document();
    doc.axioms.retain(|ax| {
        !matches!(
            &ax.kind,
            AxiomKind::SubClassOf(_, ClassExpression::ObjectIntersectionOf(cs))
                if cs.iter().any(|c| matches!(c, ClassExpression::DataAllValuesFrom(ps, _) if ps.len() > 1))
        )
    });
    // Add a single-property DataAllValuesFrom so that expression is still tested
    doc.axioms.insert(Axiom::sub_class_of(
        cls("C"),
        ClassExpression::DataAllValuesFrom(
            vec![dp("age")],
            DataRange::Datatype(Datatype::new(vocab::XSD_INTEGER)),
        ),
    ));

    let r = renderer();
    let (omn1, rep1) = manchester::document(
        &r,
        &doc.header,
        doc.axioms.iter(),
        WriteOptions { strict: true },
    )
    .unwrap();
    assert!(rep1.is_lossless(), "{rep1:?}");
    let (omn2, _) = manchester::document(
        &r,
        &doc.header,
        doc.axioms.iter(),
        WriteOptions { strict: true },
    )
    .unwrap();
    assert_eq!(omn1, omn2, "Manchester output must be deterministic");

    let back = manchester::parse_document(&omn1).unwrap();
    let back_doc = Document::new(back.header, back.axioms);
    assert_eq!(back_doc, doc, "Manchester document must round-trip cleanly");
}

#[test]
fn manchester_lossy_reporting_strict_refusal_and_bounds() {
    let r = renderer();
    let gci = Axiom::sub_class_of(
        ClassExpression::and([cls("A").into(), cls("B").into()]),
        cls("C"),
    );
    let inv_domain = Axiom::new(AxiomKind::ObjectPropertyDomain(
        ObjectPropertyExpression::InverseOf(op("p")),
        cls("A").into(),
    ));
    let nary_data = Axiom::sub_class_of(
        cls("A"),
        ClassExpression::DataSomeValuesFrom(
            vec![dp("age"), dp("score")],
            DataRange::Datatype(Datatype::new(vocab::XSD_INTEGER)),
        ),
    );
    let undeclared_ann = Axiom::new(AxiomKind::AnnotationAssertion(
        AnnotationProperty::new(vocab::RDFS_COMMENT),
        AnnotationSubject::Iri(iri(ex("NeverDeclared"))),
        AnnotationValue::Literal(Literal::simple("orphan")),
    ));
    let keep = Axiom::sub_class_of(cls("A"), cls("B"));
    let axioms = [
        Axiom::declaration(Entity::Class(cls("A"))),
        Axiom::declaration(Entity::Class(cls("B"))),
        keep.clone(),
        gci.clone(),
        inv_domain.clone(),
        nary_data.clone(),
        undeclared_ann.clone(),
    ];

    let (text, rep) = manchester::document(
        &r,
        &Header::default(),
        axioms.iter(),
        WriteOptions::default(),
    )
    .unwrap();
    assert_eq!(rep.skipped_count, 4, "{rep:?}");
    assert_eq!(rep.skipped.len(), 4);
    let back = manchester::parse_document(&text).unwrap();
    assert!(back.axioms.contains(&keep));

    // Strict mode refuses each unrepresentable axiom
    for bad in [&gci, &inv_domain, &nary_data, &undeclared_ann] {
        let err =
            manchester::document(&r, &Header::default(), [bad], WriteOptions { strict: true })
                .unwrap_err();
        assert!(matches!(err, WriteError::Unsupported(_)), "{err:?}");
    }

    // Skipped-axiom report stays bounded at MAX_REPORTED_SKIPPED
    let many_gcis: Vec<Axiom> = (0..1_200)
        .map(|i| {
            Axiom::sub_class_of(
                ClassExpression::and([cls(&format!("X{i}")).into(), cls("B").into()]),
                cls("C"),
            )
        })
        .collect();
    let (_, rep) = manchester::document(
        &r,
        &Header::default(),
        many_gcis.iter(),
        WriteOptions::default(),
    )
    .unwrap();
    assert_eq!(rep.skipped_count, 1_200);
    assert_eq!(rep.skipped.len(), MAX_REPORTED_SKIPPED);
}

// ===================================================================== OBO 1.4 ===

fn obo_ontology() -> Document {
    let mut header = Header {
        iri: Some(iri(obo_iri("test.owl"))),
        version_iri: Some(iri(obo_iri("test/releases/2026-11-25/test.owl"))),
        ..Default::default()
    };
    header.imports.insert(iri(obo_iri("ro.owl")));
    header
        .annotations
        .insert(Annotation::comment("OBO test ! {comment} [v1]"));
    header.annotations.insert(Annotation::new(
        AnnotationProperty::new(OIO_DATE),
        AnnotationValue::Literal(Literal::simple("25:11:2026 12:00")),
    ));
    header.annotations.insert(Annotation::new(
        AnnotationProperty::new(OIO_SAVED_BY),
        AnnotationValue::Literal(Literal::simple("tester")),
    ));
    header.annotations.insert(Annotation::new(
        AnnotationProperty::new(OIO_AUTO_GENERATED_BY),
        AnnotationValue::Literal(Literal::simple("sparkles")),
    ));
    header.annotations.insert(Annotation::new(
        AnnotationProperty::new(OIO_DEFAULT_NAMESPACE),
        AnnotationValue::Literal(Literal::simple("biological_process")),
    ));

    let go1 = Class::new(obo_iri("GO_0000001"));
    let go2 = Class::new(obo_iri("GO_0000002"));
    let go3 = Class::new(obo_iri("GO_0000003"));
    let go4 = Class::new(obo_iri("GO_0000004"));
    let apollo = Class::new(obo_iri("APOLLO_SV_0000001"));
    let part_of = ObjectProperty::new(obo_iri("BFO_0000050"));
    let has_part = ObjectProperty::new(obo_iri("BFO_0000051"));
    let reg = ObjectProperty::new(obo_iri("RO_0002211"));
    let meta_prop = AnnotationProperty::new(ex("curatorNote"));
    let inst1 = NamedIndividual::new(ex("sample1"));
    let inst2 = NamedIndividual::new(ex("sample2"));

    let axioms = vec![
        Axiom::declaration(Entity::Class(go1.clone())),
        Axiom::declaration(Entity::Class(go2.clone())),
        Axiom::declaration(Entity::Class(go3.clone())),
        Axiom::declaration(Entity::Class(go4.clone())),
        Axiom::declaration(Entity::Class(apollo.clone())),
        Axiom::declaration(Entity::ObjectProperty(part_of.clone())),
        Axiom::declaration(Entity::ObjectProperty(has_part.clone())),
        Axiom::declaration(Entity::ObjectProperty(reg.clone())),
        Axiom::declaration(Entity::AnnotationProperty(meta_prop.clone())),
        Axiom::declaration(Entity::NamedIndividual(inst1.clone())),
        Axiom::declaration(Entity::NamedIndividual(inst2.clone())),
        // Term annotations with special OBO characters
        Axiom::new(AxiomKind::AnnotationAssertion(
            AnnotationProperty::new(vocab::RDFS_LABEL),
            AnnotationSubject::Iri(go1.0.clone()),
            AnnotationValue::Literal(Literal::simple("mitochondrion ! [inheritance] {test}")),
        )),
        Axiom::new(AxiomKind::AnnotationAssertion(
            AnnotationProperty::new(OIO_HAS_OBO_NAMESPACE),
            AnnotationSubject::Iri(go1.0.clone()),
            AnnotationValue::Literal(Literal::simple("biological_process")),
        )),
        Axiom::new(AxiomKind::AnnotationAssertion(
            AnnotationProperty::new(OIO_HAS_ALT_ID),
            AnnotationSubject::Iri(go1.0.clone()),
            AnnotationValue::Literal(Literal::simple("GO:9999999")),
        )),
        Axiom {
            kind: AxiomKind::AnnotationAssertion(
                AnnotationProperty::new(IAO_DEFINITION),
                AnnotationSubject::Iri(go1.0.clone()),
                AnnotationValue::Literal(Literal::simple("A \"quoted\"\ndefinition\twith\\slash")),
            ),
            annotations: [
                Annotation::new(
                    AnnotationProperty::new(OIO_HAS_DBXREF),
                    AnnotationValue::Literal(Literal::simple("PMID:12345")),
                ),
                Annotation::new(
                    AnnotationProperty::new(OIO_HAS_DBXREF),
                    AnnotationValue::Literal(Literal::simple("GOC:mcc")),
                ),
            ]
            .into_iter()
            .collect(),
        },
        Axiom::new(AxiomKind::AnnotationAssertion(
            AnnotationProperty::new(vocab::RDFS_COMMENT),
            AnnotationSubject::Iri(go1.0.clone()),
            AnnotationValue::Literal(Literal::simple("see also ! note")),
        )),
        Axiom::new(AxiomKind::AnnotationAssertion(
            AnnotationProperty::new(OIO_IN_SUBSET),
            AnnotationSubject::Iri(go1.0.clone()),
            AnnotationValue::Iri(iri(obo_iri("goslim_generic"))),
        )),
        Axiom {
            kind: AxiomKind::AnnotationAssertion(
                AnnotationProperty::new(OIO_HAS_EXACT_SYNONYM),
                AnnotationSubject::Iri(go1.0.clone()),
                AnnotationValue::Literal(Literal::simple("mito inheritance")),
            ),
            annotations: [
                Annotation::new(
                    AnnotationProperty::new(OIO_HAS_DBXREF),
                    AnnotationValue::Literal(Literal::simple("GOC:cur")),
                ),
                Annotation::new(
                    AnnotationProperty::new(OIO_HAS_SYNONYM_TYPE),
                    AnnotationValue::Iri(iri(obo_iri("syngo_official_label"))),
                ),
            ]
            .into_iter()
            .collect(),
        },
        Axiom::new(AxiomKind::AnnotationAssertion(
            AnnotationProperty::new(OIO_HAS_BROAD_SYNONYM),
            AnnotationSubject::Iri(go1.0.clone()),
            AnnotationValue::Literal(Literal::simple("organelle inheritance")),
        )),
        Axiom::new(AxiomKind::AnnotationAssertion(
            AnnotationProperty::new(OIO_HAS_NARROW_SYNONYM),
            AnnotationSubject::Iri(go1.0.clone()),
            AnnotationValue::Literal(Literal::simple("yeast mito inheritance")),
        )),
        Axiom::new(AxiomKind::AnnotationAssertion(
            AnnotationProperty::new(OIO_HAS_RELATED_SYNONYM),
            AnnotationSubject::Iri(go1.0.clone()),
            AnnotationValue::Literal(Literal::simple("mitochondria")),
        )),
        Axiom::new(AxiomKind::AnnotationAssertion(
            AnnotationProperty::new(OIO_HAS_DBXREF),
            AnnotationSubject::Iri(go1.0.clone()),
            AnnotationValue::Literal(Literal::simple("Wikipedia:Mitochondrion")),
        )),
        Axiom::new(AxiomKind::AnnotationAssertion(
            AnnotationProperty::new(OIO_CREATED_BY),
            AnnotationSubject::Iri(go1.0.clone()),
            AnnotationValue::Literal(Literal::simple("alice")),
        )),
        Axiom::new(AxiomKind::AnnotationAssertion(
            AnnotationProperty::new(OIO_CREATION_DATE),
            AnnotationSubject::Iri(go1.0.clone()),
            AnnotationValue::Literal(Literal::simple("2026-01-01T00:00:00Z")),
        )),
        // Obsolete term
        Axiom::new(AxiomKind::AnnotationAssertion(
            AnnotationProperty::new(vocab::OWL_DEPRECATED),
            AnnotationSubject::Iri(go4.0.clone()),
            AnnotationValue::Literal(Literal::typed("true", vocab::XSD_BOOLEAN)),
        )),
        Axiom::new(AxiomKind::AnnotationAssertion(
            AnnotationProperty::new(IAO_REPLACED_BY),
            AnnotationSubject::Iri(go4.0.clone()),
            AnnotationValue::Iri(go1.0.clone()),
        )),
        Axiom::new(AxiomKind::AnnotationAssertion(
            AnnotationProperty::new(OIO_CONSIDER),
            AnnotationSubject::Iri(go4.0.clone()),
            AnnotationValue::Iri(go2.0.clone()),
        )),
        // Logical term axioms with is_inferred qualifier
        Axiom::sub_class_of(go1.clone(), go2.clone()).annotated(Annotation::new(
            AnnotationProperty::new(OIO_IS_INFERRED),
            AnnotationValue::Literal(Literal::simple("true")),
        )),
        Axiom::sub_class_of(
            go1.clone(),
            ClassExpression::some(part_of.clone(), go3.clone()),
        ),
        Axiom::equivalent_classes([
            go2.clone().into(),
            ClassExpression::and([
                go3.clone().into(),
                ClassExpression::some(part_of.clone(), apollo.clone()),
            ]),
        ]),
        Axiom::equivalent_classes([
            go3.clone().into(),
            ClassExpression::or([go1.clone().into(), apollo.clone().into()]),
        ]),
        Axiom::disjoint_classes([go1.clone().into(), go4.clone().into()]),
        // Typedef axioms
        Axiom::new(AxiomKind::TransitiveObjectProperty(part_of.clone().into())),
        Axiom::new(AxiomKind::ReflexiveObjectProperty(part_of.clone().into())),
        Axiom::new(AxiomKind::InverseObjectProperties(
            part_of.clone().into(),
            has_part.clone().into(),
        )),
        Axiom::new(AxiomKind::SubObjectPropertyOf(
            SubObjectPropertyExpression::Chain(vec![reg.clone().into(), part_of.clone().into()]),
            reg.clone().into(),
        )),
        Axiom::new(AxiomKind::ObjectPropertyDomain(
            part_of.clone().into(),
            go2.clone().into(),
        )),
        Axiom::new(AxiomKind::ObjectPropertyRange(
            part_of.clone().into(),
            go3.clone().into(),
        )),
        Axiom::new(AxiomKind::SubObjectPropertyOf(
            SubObjectPropertyExpression::Expression(reg.clone().into()),
            part_of.clone().into(),
        )),
        // Custom annotation property & property_value
        Axiom::new(AxiomKind::AnnotationAssertion(
            meta_prop,
            AnnotationSubject::Iri(go2.0.clone()),
            AnnotationValue::Literal(Literal::typed("7", vocab::XSD_INTEGER)),
        )),
        // Instance axioms
        Axiom::class_assertion(go1, Individual::Named(inst1.clone())),
        Axiom::new(AxiomKind::ObjectPropertyAssertion(
            part_of.into(),
            Individual::Named(inst1),
            Individual::Named(inst2),
        )),
    ];

    Document::new(header, axioms)
}

#[test]
fn obo_round_trip_obo_ids_escaping_and_determinism() {
    let doc = obo_ontology();
    let r = Renderer::new([("ex", EX)]);
    let (obo1, rep1) = obo::document(
        &r,
        &doc.header,
        doc.axioms.iter(),
        WriteOptions { strict: true },
    )
    .unwrap();
    assert!(rep1.is_lossless(), "{rep1:?}");
    let (obo2, _) = obo::document(
        &r,
        &doc.header,
        doc.axioms.iter(),
        WriteOptions { strict: true },
    )
    .unwrap();
    assert_eq!(obo1, obo2, "OBO 1.4 output must be deterministic");

    assert!(obo1.contains("format-version: 1.4\n"), "{obo1}");
    assert!(obo1.contains("id: GO:0000001\n"), "{obo1}");
    assert!(obo1.contains("id: APOLLO_SV:0000001\n"), "{obo1}");
    assert!(obo1.contains("id: BFO:0000050\n"), "{obo1}");
    assert!(obo1.contains("{is_inferred=\"true\"}"), "{obo1}");
    assert!(
        obo1.contains("holds_over_chain: RO:0002211 BFO:0000050"),
        "{obo1}"
    );
    assert!(obo1.contains("is_metadata_tag: true"), "{obo1}");

    let back = obo::parse(&obo1).unwrap();
    let back_doc = Document::new(back.header, back.axioms);
    assert_eq!(back_doc, doc, "OBO 1.4 document must round-trip cleanly");
}

#[test]
fn obo_lossy_reporting_and_strict_refusal() {
    let r = renderer();
    let unsupported = [
        Axiom::declaration(Entity::Class(cls("A"))),
        Axiom::declaration(Entity::Class(cls("B"))),
        Axiom::sub_class_of(cls("A"), ClassExpression::not(cls("B"))),
        Axiom::disjoint_union(cls("A"), [cls("B").into(), cls("C").into()]),
        Axiom::new(AxiomKind::HasKey(
            cls("A").into(),
            [op("p").into()].into_iter().collect(),
            BTreeSet::new(),
        )),
    ];
    let (_, rep) = obo::document(
        &r,
        &Header::default(),
        unsupported.iter(),
        WriteOptions::default(),
    )
    .unwrap();
    assert_eq!(rep.skipped_count, 3, "{rep:?}");

    let err = obo::document(
        &r,
        &Header::default(),
        unsupported.iter(),
        WriteOptions { strict: true },
    )
    .unwrap_err();
    assert!(matches!(err, WriteError::Unsupported(_)), "{err:?}");

    // Carriage return in tag text refused in strict mode and skipped cleanly in lenient mode
    let cr_ax = [
        Axiom::declaration(Entity::Class(cls("A"))),
        Axiom::new(AxiomKind::AnnotationAssertion(
            AnnotationProperty::new(vocab::RDFS_LABEL),
            AnnotationSubject::Iri(iri(ex("A"))),
            AnnotationValue::Literal(Literal::simple("bad\rcr")),
        )),
    ];
    let err = obo::document(
        &r,
        &Header::default(),
        cr_ax.iter(),
        WriteOptions { strict: true },
    )
    .unwrap_err();
    assert!(matches!(err, WriteError::Unsupported(_)), "{err:?}");
    let (cr_obo, cr_rep) = obo::document(
        &r,
        &Header::default(),
        cr_ax.iter(),
        WriteOptions::default(),
    )
    .unwrap();
    assert_eq!(cr_rep.skipped_count, 1, "{cr_rep:?}");
    assert!(obo::parse(&cr_obo).is_ok());
}

// ============================================================ OBO Graphs JSON ===

fn obographs_ontology() -> Document {
    let mut header = Header {
        iri: Some(iri(obo_iri("test.owl"))),
        version_iri: Some(iri(obo_iri("test/releases/2026-11-25/test.owl"))),
        ..Default::default()
    };
    header.imports.insert(iri(obo_iri("ro.owl")));
    header
        .annotations
        .insert(Annotation::comment("OBO Graphs \"test\" \\ header"));

    let c1 = Class::new(obo_iri("GO_0000001"));
    let c2 = Class::new(obo_iri("GO_0000002"));
    let c3 = Class::new(obo_iri("GO_0000003"));
    let c4 = Class::new(obo_iri("GO_0000004"));
    let p1 = ObjectProperty::new(obo_iri("BFO_0000050"));
    let p2 = ObjectProperty::new(obo_iri("BFO_0000051"));
    let p3 = ObjectProperty::new(obo_iri("RO_0002211"));
    let i1 = NamedIndividual::new(ex("ind1"));
    let i2 = NamedIndividual::new(ex("ind2"));

    let axioms = vec![
        Axiom::declaration(Entity::Class(c1.clone())),
        Axiom::declaration(Entity::Class(c2.clone())),
        Axiom::declaration(Entity::Class(c3.clone())),
        Axiom::declaration(Entity::Class(c4.clone())),
        Axiom::declaration(Entity::ObjectProperty(p1.clone())),
        Axiom::declaration(Entity::ObjectProperty(p2.clone())),
        Axiom::declaration(Entity::ObjectProperty(p3.clone())),
        Axiom::declaration(Entity::NamedIndividual(i1.clone())),
        Axiom::declaration(Entity::NamedIndividual(i2.clone())),
        // Node metadata
        Axiom::new(AxiomKind::AnnotationAssertion(
            AnnotationProperty::new(vocab::RDFS_LABEL),
            AnnotationSubject::Iri(c1.0.clone()),
            AnnotationValue::Literal(Literal::simple("cell\t\"name\"")),
        )),
        Axiom {
            kind: AxiomKind::AnnotationAssertion(
                AnnotationProperty::new(IAO_DEFINITION),
                AnnotationSubject::Iri(c1.0.clone()),
                AnnotationValue::Literal(Literal::simple("Basic unit of life")),
            ),
            annotations: [Annotation::new(
                AnnotationProperty::new(OIO_HAS_DBXREF),
                AnnotationValue::Literal(Literal::simple("ISBN:01234")),
            )]
            .into_iter()
            .collect(),
        },
        Axiom::new(AxiomKind::AnnotationAssertion(
            AnnotationProperty::new(vocab::RDFS_COMMENT),
            AnnotationSubject::Iri(c1.0.clone()),
            AnnotationValue::Literal(Literal::simple("comment 1")),
        )),
        Axiom::new(AxiomKind::AnnotationAssertion(
            AnnotationProperty::new(OIO_IN_SUBSET),
            AnnotationSubject::Iri(c1.0.clone()),
            AnnotationValue::Iri(iri(obo_iri("goslim_generic"))),
        )),
        Axiom {
            kind: AxiomKind::AnnotationAssertion(
                AnnotationProperty::new(OIO_HAS_EXACT_SYNONYM),
                AnnotationSubject::Iri(c1.0.clone()),
                AnnotationValue::Literal(Literal::simple("cellular unit")),
            ),
            annotations: [
                Annotation::new(
                    AnnotationProperty::new(OIO_HAS_DBXREF),
                    AnnotationValue::Literal(Literal::simple("GOC:cur")),
                ),
                Annotation::new(
                    AnnotationProperty::new(OIO_HAS_SYNONYM_TYPE),
                    AnnotationValue::Iri(iri(obo_iri("syngo_label"))),
                ),
            ]
            .into_iter()
            .collect(),
        },
        Axiom::new(AxiomKind::AnnotationAssertion(
            AnnotationProperty::new(OIO_HAS_DBXREF),
            AnnotationSubject::Iri(c1.0.clone()),
            AnnotationValue::Literal(Literal::simple("MESH:D002477")),
        )),
        Axiom::new(AxiomKind::AnnotationAssertion(
            AnnotationProperty::new(vocab::OWL_DEPRECATED),
            AnnotationSubject::Iri(c4.0.clone()),
            AnnotationValue::Literal(Literal::typed("true", vocab::XSD_BOOLEAN)),
        )),
        // Edges (is_a with annotation, existential, subPropertyOf, inverseOf, type, fact)
        Axiom::sub_class_of(c1.clone(), c2.clone()).annotated(Annotation::new(
            AnnotationProperty::new(OIO_IS_INFERRED),
            AnnotationValue::Literal(Literal::simple("true")),
        )),
        Axiom::sub_class_of(c1.clone(), ClassExpression::some(p1.clone(), c3.clone())),
        Axiom::new(AxiomKind::SubObjectPropertyOf(
            SubObjectPropertyExpression::Expression(p3.clone().into()),
            p1.clone().into(),
        )),
        Axiom::new(AxiomKind::InverseObjectProperties(
            p1.clone().into(),
            p2.clone().into(),
        )),
        Axiom::class_assertion(c1.clone(), Individual::Named(i1.clone())),
        Axiom::new(AxiomKind::ObjectPropertyAssertion(
            p1.clone().into(),
            Individual::Named(i1),
            Individual::Named(i2),
        )),
        // Equivalent nodes set & logical definition axiom
        Axiom::equivalent_classes([c2.clone().into(), c3.clone().into()]),
        Axiom::equivalent_classes([
            c1.into(),
            ClassExpression::and([
                c2.clone().into(),
                ClassExpression::some(p1.clone(), c3.clone()),
            ]),
        ]),
        // Domain, range, and 3-link property chain
        Axiom::new(AxiomKind::ObjectPropertyDomain(
            p1.clone().into(),
            c2.into(),
        )),
        Axiom::new(AxiomKind::ObjectPropertyRange(p1.clone().into(), c3.into())),
        Axiom::new(AxiomKind::SubObjectPropertyOf(
            SubObjectPropertyExpression::Chain(vec![p3.clone().into(), p1.into(), p2.into()]),
            p3.into(),
        )),
    ];

    Document::new(header, axioms)
}

#[test]
fn obographs_round_trip_schema_and_determinism() {
    let doc = obographs_ontology();
    let r = renderer();
    let (json1, rep1) = obographs::document(
        &r,
        &doc.header,
        doc.axioms.iter(),
        WriteOptions { strict: true },
    )
    .unwrap();
    assert!(rep1.is_lossless(), "{rep1:?}");
    let (json2, _) = obographs::document(
        &r,
        &doc.header,
        doc.axioms.iter(),
        WriteOptions { strict: true },
    )
    .unwrap();
    assert_eq!(json1, json2, "OBO Graphs JSON output must be deterministic");

    // Check top-level OBO Graphs JSON schema structure
    let val: serde_json::Value = serde_json::from_str(&json1).unwrap();
    let g = &val["graphs"][0];
    assert_eq!(g["id"], obo_iri("test.owl"));
    assert!(g["nodes"].is_array());
    assert!(g["edges"].is_array());
    assert!(g["equivalentNodesSets"].is_array());
    assert!(g["logicalDefinitionAxioms"].is_array());
    assert!(g["domainRangeAxioms"].is_array());
    assert!(g["propertyChainAxioms"].is_array());

    let back = obographs::parse(&json1).unwrap();
    let back_doc = Document::new(back.header, back.axioms);
    assert_eq!(back_doc, doc, "OBO Graphs JSON must round-trip cleanly");
}

#[test]
fn obographs_lossy_reporting_and_strict_refusal() {
    let r = renderer();
    let axioms = [
        Axiom::declaration(Entity::Class(cls("A"))),
        Axiom::declaration(Entity::Class(cls("B"))),
        Axiom::disjoint_classes([cls("A").into(), cls("B").into()]),
        Axiom::sub_class_of(cls("A"), ClassExpression::not(cls("B"))),
    ];
    let (json, rep) = obographs::document(
        &r,
        &Header::default(),
        axioms.iter(),
        WriteOptions::default(),
    )
    .unwrap();
    assert_eq!(rep.skipped_count, 2, "{rep:?}");
    let _: serde_json::Value = serde_json::from_str(&json).unwrap();

    let err = obographs::document(
        &r,
        &Header::default(),
        axioms.iter(),
        WriteOptions { strict: true },
    )
    .unwrap_err();
    assert!(matches!(err, WriteError::Unsupported(_)), "{err:?}");
}

// ================================================== Round 1 Regression Tests ===

fn is_valid_xml_ncname(s: &str) -> bool {
    let mut cs = s.chars();
    let Some(first) = cs.next() else {
        return false;
    };
    (first.is_ascii_alphabetic() || first == '_')
        && cs.all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
}

#[test]
fn owx_schema_conformance_blank_nodes_prefixes_single_operand_and_parser_edge_cases() {
    // 1. Non-NCName blank nodes + potential escape collision stay valid NCNames & distinct
    let labels = ["1", "", "a:b", "foo bar", "__owx_31", "normal_bnode"];
    let axioms: Vec<Axiom> = labels
        .iter()
        .enumerate()
        .map(|(i, lbl)| Axiom::class_assertion(cls(&format!("C{i}")), Individual::anonymous(*lbl)))
        .collect();
    // Include digit-prefixed, dash-prefixed, and duplicate prefix names
    let r = Renderer::new([
        ("1ex", "http://bad-digit.example/"),
        ("-ex", "http://bad-dash.example/"),
        ("ex", "http://first.example/"),
        ("ex", EX),
    ]);
    let mut all_axioms = axioms;
    all_axioms.push(Axiom::declaration(Entity::Class(Class::new(
        "http://bad-digit.example/Thing",
    ))));
    all_axioms.push(Axiom::declaration(Entity::Class(Class::new(ex("A")))));

    // Single-operand EquivalentClasses / EquivalentObjectProperties / EquivalentDataProperties / SameIndividual
    all_axioms.push(Axiom::equivalent_classes([cls("A").into()]));
    all_axioms.push(Axiom::new(AxiomKind::EquivalentObjectProperties(
        [op("p").into()].into_iter().collect(),
    )));
    all_axioms.push(Axiom::new(AxiomKind::EquivalentDataProperties(
        [dp("age")].into_iter().collect(),
    )));
    all_axioms.push(Axiom::new(AxiomKind::SameIndividual(
        [ind("i1")].into_iter().collect(),
    )));

    let xml = owx::document(&r, &Header::default(), all_axioms.iter()).unwrap();

    // Verify no invalid <Prefix> names and no duplicate <Prefix name="ex">
    assert!(!xml.contains("name=\"1ex\""), "{xml}");
    assert!(!xml.contains("name=\"-ex\""), "{xml}");
    assert_eq!(xml.matches("<Prefix name=\"ex\"").count(), 1, "{xml}");
    assert!(
        xml.contains("IRI=\"http://bad-digit.example/Thing\""),
        "{xml}"
    );

    // Independently check every emitted nodeID="..." is a valid XML NCName and all 6 are distinct
    let mut seen_ids = BTreeSet::new();
    for part in xml.split("nodeID=\"").skip(1) {
        let id = part.split('"').next().unwrap();
        assert!(is_valid_xml_ncname(id), "invalid NCName nodeID: {id}");
        seen_ids.insert(id.to_string());
    }
    assert_eq!(
        seen_ids.len(),
        labels.len(),
        "collision in nodeIDs: {seen_ids:?}"
    );
    assert!(seen_ids.contains("normal_bnode"));

    // Verify XSD minOccurs="2" on single-operand Equivalent* and SameIndividual
    for tag in [
        "EquivalentClasses",
        "EquivalentObjectProperties",
        "EquivalentDataProperties",
        "SameIndividual",
    ] {
        let open = format!("<{tag}>\n");
        let close = format!("    </{tag}>");
        let start = xml.find(&open).unwrap() + open.len();
        let end = start + xml[start..].find(&close).unwrap();
        let child_count = xml[start..end]
            .lines()
            .filter(|l| l.trim_start().starts_with('<'))
            .count();
        assert_eq!(
            child_count,
            2,
            "{tag} must have 2 operands: {}",
            &xml[start..end]
        );
    }

    // Round-trip parses cleanly
    let back = owx::parse(&xml).unwrap();
    assert!(
        back.axioms
            .contains(&Axiom::equivalent_classes([cls("A").into()]))
    );
    assert!(
        back.axioms
            .contains(&Axiom::declaration(Entity::Class(Class::new(
                "http://bad-digit.example/Thing"
            ))))
    );

    // 2. Parser edge cases: unterminated <!DOCTYPE>, quoted ']'/'>' inside <!DOCTYPE>, trailing garbage
    assert!(owx::parse("<!DOCTYPE Ontology [ <!ENTITY a \"1\"").is_err());
    let with_doctype = r#"<?xml version="1.0"?>
<!DOCTYPE Ontology [ <!ENTITY x "a]b>"> ]>
<Ontology xmlns="http://www.w3.org/2002/07/owl#"/>
<!-- trailing comment ok -->
"#;
    assert!(owx::parse(with_doctype).is_ok());
    let with_trailing = r#"<Ontology xmlns="http://www.w3.org/2002/07/owl#"/> <Extra/>"#;
    assert!(owx::parse(with_trailing).is_err());
    let with_trailing_text = r#"<Ontology xmlns="http://www.w3.org/2002/07/owl#"/> junk"#;
    assert!(owx::parse(with_trailing_text).is_err());
}

#[test]
fn manchester_w3c_syntax_undeclared_data_props_prefixes_and_edge_cases() {
    // 1. Mixed HasKey (whitespace-separated), nested complements, boolean & newline literal
    let r = Renderer::new([
        ("1ex", "http://bad-digit.example/"),
        ("-ex", "http://bad-dash.example/"),
        ("", EX),
    ]);
    let axioms = vec![
        Axiom::declaration(Entity::Class(cls("C"))),
        Axiom::declaration(Entity::ObjectProperty(op("p"))),
        Axiom::declaration(Entity::DataProperty(dp("d"))),
        Axiom::new(AxiomKind::HasKey(
            cls("C").into(),
            [op("p").into()].into_iter().collect(),
            [dp("d")].into_iter().collect(),
        )),
        Axiom::sub_class_of(
            cls("C"),
            ClassExpression::not(ClassExpression::not(cls("C"))),
        ),
        Axiom::new(AxiomKind::DataPropertyRange(
            dp("d"),
            DataRange::ComplementOf(Box::new(DataRange::ComplementOf(Box::new(
                DataRange::Datatype(Datatype::new(vocab::XSD_INTEGER)),
            )))),
        )),
        Axiom::sub_class_of(
            cls("C"),
            ClassExpression::DataHasValue(dp("d"), Literal::typed("true", vocab::XSD_BOOLEAN)),
        ),
        Axiom::new(AxiomKind::AnnotationAssertion(
            AnnotationProperty::new(vocab::RDFS_COMMENT),
            AnnotationSubject::Iri(iri(ex("C"))),
            AnnotationValue::Literal(Literal::simple("line1\nline2\"quote\\slash")),
        )),
        Axiom::declaration(Entity::Class(Class::new("http://bad-digit.example/X"))),
    ];
    let (omn, rep) = manchester::document(
        &r,
        &Header::default(),
        axioms.iter(),
        WriteOptions { strict: true },
    )
    .unwrap();
    assert!(rep.is_lossless(), "{rep:?}");
    assert!(!omn.contains("Prefix: 1ex:"), "{omn}");
    assert!(!omn.contains("Prefix: -ex:"), "{omn}");
    assert!(
        omn.contains("HasKey:\n        :p :d\n"),
        "HasKey must be space-separated: {omn}"
    );
    assert!(!omn.contains(":p, :d"), "{omn}");
    assert!(
        omn.contains("not (not :C)"),
        "double complement must be parenthesized: {omn}"
    );
    assert!(!omn.contains("not not "), "{omn}");
    assert!(
        omn.contains("not (not xsd:integer)"),
        "double data complement must be parenthesized: {omn}"
    );
    assert!(
        omn.contains("\"true\"^^xsd:boolean"),
        "booleans must use typed literals: {omn}"
    );
    assert!(
        omn.contains("\"line1\nline2\\\"quote\\\\slash\""),
        "newlines must stay literal inside Manchester quotedString: {omn}"
    );
    let back = manchester::parse_document(&omn).unwrap();
    assert_eq!(
        Document::new(back.header, back.axioms),
        Document::new(Header::default(), axioms)
    );

    // 2. Undeclared DataProperty in unqualified DataMinCardinality, custom-datatype DataSomeValuesFrom,
    // HasKey, and n-ary EquivalentDataProperties / DisjointDataProperties must never corrupt to ObjectProperty
    let undecl_axioms = vec![
        Axiom::sub_class_of(
            cls("C"),
            ClassExpression::DataMinCardinality(1, dp("d1"), None),
        ),
        Axiom::sub_class_of(
            cls("C"),
            ClassExpression::DataSomeValuesFrom(
                vec![dp("d2")],
                DataRange::Datatype(dt("CustomRange")),
            ),
        ),
        Axiom::new(AxiomKind::HasKey(
            cls("C").into(),
            BTreeSet::new(),
            [dp("d3")].into_iter().collect(),
        )),
        Axiom::new(AxiomKind::EquivalentDataProperties(
            [dp("d1"), dp("d2"), dp("d3")].into_iter().collect(),
        )),
        Axiom::disjoint_data_properties([dp("d1"), dp("d2"), dp("d3")]),
    ];
    let (omn2, rep2) = manchester::document(
        &renderer(),
        &Header::default(),
        undecl_axioms.iter(),
        WriteOptions { strict: true },
    )
    .unwrap();
    assert!(rep2.is_lossless(), "{rep2:?}");
    let back2 = manchester::parse_document(&omn2).unwrap();
    for expected in &undecl_axioms {
        assert!(
            back2.axioms.contains(expected),
            "missing exact axiom {expected:?} after round-trip:\n{omn2}"
        );
    }

    // 3. Punned ObjectProperty + DataProperty on ambiguous positions must be refused/skipped
    let punned = [
        Axiom::declaration(Entity::ObjectProperty(op("pun"))),
        Axiom::declaration(Entity::DataProperty(dp("pun"))),
        Axiom::sub_class_of(
            cls("C"),
            ClassExpression::DataMinCardinality(1, dp("pun"), None),
        ),
    ];
    let err = manchester::document(
        &renderer(),
        &Header::default(),
        punned.iter(),
        WriteOptions { strict: true },
    )
    .unwrap_err();
    assert!(matches!(err, WriteError::Unsupported(_)), "{err:?}");
    let (_, pun_rep) = manchester::document(
        &renderer(),
        &Header::default(),
        punned.iter(),
        WriteOptions::default(),
    )
    .unwrap();
    assert_eq!(pun_rep.skipped_count, 1);

    // 4. Invalid AnonymousIndividual labels refused/skipped
    let bad_anon = [Axiom::class_assertion(
        cls("C"),
        Individual::anonymous("bad label"),
    )];
    assert!(matches!(
        manchester::document(
            &renderer(),
            &Header::default(),
            bad_anon.iter(),
            WriteOptions { strict: true },
        ),
        Err(WriteError::Unsupported(_))
    ));
    let (_, bad_anon_rep) = manchester::document(
        &renderer(),
        &Header::default(),
        bad_anon.iter(),
        WriteOptions::default(),
    )
    .unwrap();
    assert_eq!(bad_anon_rep.skipped_count, 1);

    // 5. Trailing 'o' in SubPropertyChain: rejected by parser
    let trailing_o = "Prefix: : <http://ex.org/>\nOntology:\nObjectProperty: :r\n    SubPropertyChain: :p o :q o\n";
    assert!(manchester::parse_document(trailing_o).is_err());

    // 6. Nesting depth refusal in Manchester
    let mut deep = ClassExpression::Class(cls("A"));
    for _ in 0..140 {
        deep = ClassExpression::not(deep);
    }
    let deep_ax = Axiom::sub_class_of(cls("B"), deep);
    for strict in [false, true] {
        let err = manchester::document(
            &renderer(),
            &Header::default(),
            [&deep_ax],
            WriteOptions { strict },
        )
        .unwrap_err();
        assert!(matches!(err, WriteError::Unsupported(_)), "{err:?}");
    }
}

/// Independent validator for OBO 1.4 stanza single-cardinality, unique stanza IDs across
/// frame types, URL-as-ID scheme rules, and header subsetdef/synonymtypedef declarations.
fn assert_valid_obo_structure(obo_text: &str) {
    let mut header_subsets = BTreeSet::new();
    let mut header_syn_types = BTreeSet::new();
    let mut seen_stanza_ids = BTreeSet::new();
    let mut in_stanza = false;
    let mut single_counts: std::collections::BTreeMap<String, usize> = Default::default();
    for line in obo_text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            in_stanza = true;
            single_counts.clear();
            continue;
        }
        let (tag, val) = trimmed.split_once(':').expect("tag: val");
        let tag = tag.trim();
        let val = val.trim();
        if !in_stanza {
            if tag == "subsetdef" {
                let id = val.split_whitespace().next().unwrap();
                header_subsets.insert(id.to_string());
            } else if tag == "synonymtypedef" {
                let id = val.split_whitespace().next().unwrap();
                header_syn_types.insert(id.to_string());
            }
        } else {
            if tag == "id" {
                assert!(
                    seen_stanza_ids.insert(val.to_string()),
                    "OBO 1.4 stanza ID '{val}' duplicated across stanzas:\n{obo_text}"
                );
            }
            if matches!(
                tag,
                "name"
                    | "namespace"
                    | "def"
                    | "comment"
                    | "created_by"
                    | "creation_date"
                    | "domain"
                    | "range"
                    | "instance_of"
            ) {
                let c = single_counts.entry(tag.to_string()).or_insert(0);
                *c += 1;
                assert!(
                    *c <= 1,
                    "OBO 1.4 single-cardinality tag '{tag}:' repeated in stanza:\n{obo_text}"
                );
            }
            if tag == "subset" {
                assert!(
                    header_subsets.contains(val),
                    "OBO 1.4 subset '{val}' missing header subsetdef:\n{obo_text}"
                );
            }
            if tag == "synonym" {
                // extract optional synonym type between scope and '['
                if let Some(after_q) = val.rfind('"').map(|i| val[i + 1..].trim())
                    && let Some(bracket) = after_q.find('[')
                {
                    let mid: Vec<&str> = after_q[..bracket].split_whitespace().collect();
                    if let Some(st) = mid.get(1) {
                        assert!(
                            header_syn_types.contains(*st),
                            "OBO 1.4 synonym type '{st}' missing header synonymtypedef:\n{obo_text}"
                        );
                    }
                }
            }
        }
    }
}

#[test]
fn obo_asymmetry_shadowing_cardinality_escaping_typedef_and_strictness() {
    // 1. AsymmetricObjectProperty -> is_asymmetric: true (and external is_anti_symmetric -> annotation)
    let p_asym = ObjectProperty::new(obo_iri("RO_0000001"));
    let asym_axioms = vec![
        Axiom::declaration(Entity::ObjectProperty(p_asym.clone())),
        Axiom::new(AxiomKind::AsymmetricObjectProperty(p_asym.clone().into())),
    ];
    let (obo_asym, rep) = obo::document(
        &renderer(),
        &Header::default(),
        asym_axioms.iter(),
        WriteOptions { strict: true },
    )
    .unwrap();
    assert!(rep.is_lossless());
    assert!(obo_asym.contains("is_asymmetric: true\n"), "{obo_asym}");
    assert!(!obo_asym.contains("is_anti_symmetric: true"), "{obo_asym}");
    let back_asym = obo::parse(&obo_asym).unwrap();
    assert_eq!(
        Document::new(back_asym.header, back_asym.axioms),
        Document::new(Header::default(), asym_axioms)
    );

    let ext_anti = "format-version: 1.4\n\n[Typedef]\nid: RO:0000001\nis_anti_symmetric: true\n";
    let parsed_anti = obo::parse(ext_anti).unwrap();
    assert!(
        !parsed_anti
            .axioms
            .iter()
            .any(|a| matches!(a.kind, AxiomKind::AsymmetricObjectProperty(_)))
    );
    assert!(
        parsed_anti
            .axioms
            .contains(&Axiom::new(AxiomKind::AnnotationAssertion(
                AnnotationProperty::new(format!("{}is_anti_symmetric", obo::OBO_IN_OWL)),
                AnnotationSubject::Iri(p_asym.0.clone()),
                AnnotationValue::Literal(Literal::typed("true", vocab::XSD_BOOLEAN)),
            )))
    );

    // 2. IDSPACE shadowing OBO PURL + standard prefix shadowing + idspace-backed doi + .obo header IRI
    let custom_r = Renderer::new([
        ("GO", "https://example.org/go/"),
        ("DOI", "doi:"),
        ("EXURL", "http://example.org/"),
    ]);
    let mut header = Header {
        iri: Some(iri(obo_iri("test.obo"))),
        ..Default::default()
    };
    // Duplicate saved-by & annotated comment must not overwrite or drop
    header.annotations.insert(Annotation::new(
        AnnotationProperty::new(OIO_SAVED_BY),
        AnnotationValue::Literal(Literal::simple("alice")),
    ));
    header.annotations.insert(Annotation::new(
        AnnotationProperty::new(OIO_SAVED_BY),
        AnnotationValue::Literal(Literal::simple("bob")),
    ));
    header.annotations.insert(Annotation {
        property: AnnotationProperty::new(vocab::RDFS_COMMENT),
        value: AnnotationValue::Literal(Literal::simple("annotated comment")),
        annotations: [Annotation::new(
            AnnotationProperty::new(OIO_IS_INFERRED),
            AnnotationValue::Literal(Literal::simple("true")),
        )]
        .into_iter()
        .collect(),
    });
    let go_obo = Class::new(obo_iri("GO_0000001"));
    let go_custom = Class::new("https://example.org/go/0000001");
    let rdf_obo = Class::new(obo_iri("rdf_1"));
    let doi_obo = Class::new(obo_iri("doi_1"));
    let data_obo = Class::new(obo_iri("data_0006"));
    let geo_obo = Class::new(obo_iri("geo_0001"));
    let doi_cls = Class::new("doi:10.1234/abc");
    let nested_scheme_cls = Class::new("http://example.org/ref/http://other");
    let bang_cls = Class::new("https://example.org/A!B");
    let shadow_axioms = vec![
        Axiom::declaration(Entity::Class(go_obo.clone())),
        Axiom::declaration(Entity::Class(go_custom.clone())),
        Axiom::declaration(Entity::Class(rdf_obo.clone())),
        Axiom::declaration(Entity::Class(doi_obo.clone())),
        Axiom::declaration(Entity::Class(data_obo.clone())),
        Axiom::declaration(Entity::Class(geo_obo.clone())),
        Axiom::declaration(Entity::Class(doi_cls.clone())),
        Axiom::declaration(Entity::Class(nested_scheme_cls.clone())),
        Axiom::declaration(Entity::Class(bang_cls.clone())),
        Axiom::sub_class_of(go_obo.clone(), go_custom.clone()),
        Axiom::sub_class_of(bang_cls.clone(), rdf_obo.clone()),
        // Multi-label, multi-def, multi-comment, boundary spaces, unbalanced quote, escaped xrefs & subset
        Axiom::new(AxiomKind::AnnotationAssertion(
            AnnotationProperty::new(vocab::RDFS_LABEL),
            AnnotationSubject::Iri(go_obo.0.clone()),
            AnnotationValue::Literal(Literal::simple(" cell ")),
        )),
        Axiom::new(AxiomKind::AnnotationAssertion(
            AnnotationProperty::new(vocab::RDFS_LABEL),
            AnnotationSubject::Iri(go_obo.0.clone()),
            AnnotationValue::Literal(Literal::simple("5\" pipe")),
        )),
        Axiom::new(AxiomKind::AnnotationAssertion(
            AnnotationProperty::new(vocab::RDFS_COMMENT),
            AnnotationSubject::Iri(go_obo.0.clone()),
            AnnotationValue::Literal(Literal::simple("first comment")),
        )),
        Axiom::new(AxiomKind::AnnotationAssertion(
            AnnotationProperty::new(vocab::RDFS_COMMENT),
            AnnotationSubject::Iri(go_obo.0.clone()),
            AnnotationValue::Literal(Literal::simple("second comment")),
        )),
        Axiom {
            kind: AxiomKind::AnnotationAssertion(
                AnnotationProperty::new(IAO_DEFINITION),
                AnnotationSubject::Iri(go_obo.0.clone()),
                AnnotationValue::Literal(Literal::simple("def one")),
            ),
            annotations: [
                Annotation::new(
                    AnnotationProperty::new(OIO_HAS_DBXREF),
                    AnnotationValue::Literal(Literal::simple("DOI:10,20!30[40]\\50")),
                ),
                Annotation::new(
                    AnnotationProperty::new(OIO_HAS_DBXREF),
                    AnnotationValue::Literal(Literal::simple("PMID:999")),
                ),
            ]
            .into_iter()
            .collect(),
        },
        Axiom::new(AxiomKind::AnnotationAssertion(
            AnnotationProperty::new(IAO_DEFINITION),
            AnnotationSubject::Iri(go_obo.0.clone()),
            AnnotationValue::Literal(Literal::simple("def two")),
        )),
        Axiom::new(AxiomKind::AnnotationAssertion(
            AnnotationProperty::new(OIO_IN_SUBSET),
            AnnotationSubject::Iri(go_obo.0.clone()),
            AnnotationValue::Iri(iri("https://example.org/sub!set")),
        )),
        // Typedef replaced_by, consider, alt_id, subset, synonym, created_by, creation_date
        Axiom::declaration(Entity::ObjectProperty(p_asym.clone())),
        Axiom::new(AxiomKind::AnnotationAssertion(
            AnnotationProperty::new(IAO_REPLACED_BY),
            AnnotationSubject::Iri(p_asym.0.clone()),
            AnnotationValue::Iri(iri(obo_iri("RO_0000002"))),
        )),
        Axiom::new(AxiomKind::AnnotationAssertion(
            AnnotationProperty::new(OIO_CONSIDER),
            AnnotationSubject::Iri(p_asym.0.clone()),
            AnnotationValue::Iri(iri(obo_iri("RO_0000003"))),
        )),
        Axiom::new(AxiomKind::AnnotationAssertion(
            AnnotationProperty::new(OIO_HAS_ALT_ID),
            AnnotationSubject::Iri(p_asym.0.clone()),
            AnnotationValue::Literal(Literal::simple("RO:9999999")),
        )),
        Axiom::new(AxiomKind::AnnotationAssertion(
            AnnotationProperty::new(OIO_CREATED_BY),
            AnnotationSubject::Iri(p_asym.0.clone()),
            AnnotationValue::Literal(Literal::simple("curator")),
        )),
        Axiom::new(AxiomKind::AnnotationAssertion(
            AnnotationProperty::new(OIO_CREATION_DATE),
            AnnotationSubject::Iri(p_asym.0.clone()),
            AnnotationValue::Literal(Literal::simple("2026-11-26T00:00:00Z")),
        )),
    ];

    let (obo_text, rep_shadow) = obo::document(
        &custom_r,
        &header,
        shadow_axioms.iter(),
        WriteOptions { strict: true },
    )
    .unwrap();
    assert!(rep_shadow.is_lossless(), "{rep_shadow:?}");
    assert_valid_obo_structure(&obo_text);
    // Order-independent determinism check
    let rev: Vec<&Axiom> = shadow_axioms.iter().rev().collect();
    let (obo_rev, _) =
        obo::document(&custom_r, &header, rev, WriteOptions { strict: true }).unwrap();
    assert_eq!(obo_text, obo_rev, "OBO output must be order-independent");

    let back_shadow = obo::parse(&obo_text).unwrap();
    assert_eq!(
        Document::new(back_shadow.header, back_shadow.axioms),
        Document::new(header, shadow_axioms)
    );

    // 3. DataProperty declaration, DataPropertyAssertion, multiple instance_of, typed/IRI qualifiers,
    // duplicate hasSynonymType, and header-only loss must be refused in strict mode & reported in lenient mode
    let lossy_cases: Vec<Vec<Axiom>> = vec![
        vec![Axiom::declaration(Entity::DataProperty(dp("age")))],
        vec![Axiom::new(AxiomKind::DataPropertyAssertion(
            dp("age"),
            ind("i1"),
            Literal::typed("5", vocab::XSD_INTEGER),
        ))],
        vec![
            Axiom::declaration(Entity::NamedIndividual(NamedIndividual::new(ex("i1")))),
            Axiom::class_assertion(cls("A"), ind("i1")),
            Axiom::class_assertion(cls("B"), ind("i1")),
        ],
        vec![
            Axiom::sub_class_of(cls("A"), cls("B")).annotated(Annotation::new(
                ap("flag"),
                AnnotationValue::Literal(Literal::typed("true", vocab::XSD_BOOLEAN)),
            )),
        ],
        vec![
            Axiom::sub_class_of(cls("A"), cls("B")).annotated(Annotation::new(
                ap("ref"),
                AnnotationValue::Iri(iri(ex("Target"))),
            )),
        ],
    ];
    for case in &lossy_cases {
        let err = obo::document(
            &renderer(),
            &Header::default(),
            case.iter(),
            WriteOptions { strict: true },
        )
        .unwrap_err();
        assert!(
            matches!(err, WriteError::Unsupported(_)),
            "{case:?}: {err:?}"
        );
        let (_, rep) = obo::document(
            &renderer(),
            &Header::default(),
            case.iter(),
            WriteOptions::default(),
        )
        .unwrap();
        assert_eq!(rep.skipped_count, 1, "{case:?}: {rep:?}");
        assert!(!rep.is_lossless());
    }

    // Header-only loss (language-tagged ontology annotation)
    let mut lang_hdr = Header::default();
    lang_hdr.annotations.insert(Annotation::new(
        AnnotationProperty::new(vocab::RDFS_LABEL),
        AnnotationValue::Literal(Literal::lang("bonjour", "fr").unwrap()),
    ));
    assert!(matches!(
        obo::document(
            &renderer(),
            &lang_hdr,
            std::iter::empty::<&Axiom>(),
            WriteOptions { strict: true }
        ),
        Err(WriteError::Unsupported(_))
    ));
    let (_, hdr_rep) = obo::document(
        &renderer(),
        &lang_hdr,
        std::iter::empty::<&Axiom>(),
        WriteOptions::default(),
    )
    .unwrap();
    assert_eq!(hdr_rep.skipped_count, 0);
    assert_eq!(hdr_rep.skipped_header_count, 1);
    assert!(!hdr_rep.is_lossless());

    // MAX_REPORTED_SKIPPED bound on OBO
    let many_unsup: Vec<Axiom> = (0..1_200)
        .map(|i| Axiom::sub_class_of(cls(&format!("A{i}")), ClassExpression::not(cls("B"))))
        .collect();
    let (_, rep_many) = obo::document(
        &renderer(),
        &Header::default(),
        many_unsup.iter(),
        WriteOptions::default(),
    )
    .unwrap();
    assert_eq!(rep_many.skipped_count, 1_200);
    assert_eq!(rep_many.skipped.len(), MAX_REPORTED_SKIPPED);

    // Deep unsupported axiom bound in both strict and lenient modes
    let mut deep = ClassExpression::Class(cls("A"));
    for _ in 0..140 {
        deep = ClassExpression::not(deep);
    }
    let deep_ax = Axiom::sub_class_of(cls("B"), deep);
    for strict in [false, true] {
        let err = obo::document(
            &renderer(),
            &Header::default(),
            [&deep_ax],
            WriteOptions { strict },
        )
        .unwrap_err();
        assert!(matches!(err, WriteError::Unsupported(_)), "{err:?}");
    }
}

/// Independent validator for OBO Graphs JSON schema structure.
fn assert_valid_obographs_schema(json_text: &str) {
    let val: serde_json::Value = serde_json::from_str(json_text).expect("valid JSON");
    let graphs = val
        .get("graphs")
        .and_then(|v| v.as_array())
        .expect("graphs array");
    assert_eq!(graphs.len(), 1);
    let g = graphs[0].as_object().expect("graph object");
    for node in g
        .get("nodes")
        .and_then(|v| v.as_array())
        .expect("nodes array")
    {
        let obj = node.as_object().expect("node object");
        assert!(obj.get("id").and_then(|v| v.as_str()).is_some());
        if let Some(ty) = obj.get("type").and_then(|v| v.as_str()) {
            assert!(
                matches!(ty, "CLASS" | "PROPERTY" | "INDIVIDUAL"),
                "bad type: {ty}"
            );
            if let Some(pt) = obj.get("propertyType").and_then(|v| v.as_str()) {
                assert_eq!(ty, "PROPERTY");
                assert!(
                    matches!(pt, "OBJECT" | "DATA" | "ANNOTATION"),
                    "bad propertyType: {pt}"
                );
            }
        }
    }
    for edge in g
        .get("edges")
        .and_then(|v| v.as_array())
        .expect("edges array")
    {
        let obj = edge.as_object().expect("edge object");
        assert!(obj.get("sub").and_then(|v| v.as_str()).is_some());
        assert!(obj.get("pred").and_then(|v| v.as_str()).is_some());
        assert!(obj.get("obj").and_then(|v| v.as_str()).is_some());
    }
}

#[test]
fn obographs_property_types_punning_strict_values_io_error_and_bounds() {
    // 1. All 3 propertyType kinds round-trip distinctly + order-independent lbl/def determinism
    let prop_axioms = vec![
        Axiom::declaration(Entity::Class(cls("C"))),
        Axiom::declaration(Entity::ObjectProperty(op("objp"))),
        Axiom::declaration(Entity::DataProperty(dp("datap"))),
        Axiom::declaration(Entity::AnnotationProperty(ap("annp"))),
        Axiom::new(AxiomKind::AnnotationAssertion(
            AnnotationProperty::new(vocab::RDFS_LABEL),
            AnnotationSubject::Iri(iri(ex("C"))),
            AnnotationValue::Literal(Literal::simple("zeta")),
        )),
        Axiom::new(AxiomKind::AnnotationAssertion(
            AnnotationProperty::new(vocab::RDFS_LABEL),
            AnnotationSubject::Iri(iri(ex("C"))),
            AnnotationValue::Literal(Literal::simple("alpha")),
        )),
    ];
    let (j1, rep1) = obographs::document(
        &renderer(),
        &Header::default(),
        prop_axioms.iter(),
        WriteOptions { strict: true },
    )
    .unwrap();
    assert!(rep1.is_lossless(), "{rep1:?}");
    assert_valid_obographs_schema(&j1);
    assert!(j1.contains("\"propertyType\" : \"OBJECT\""), "{j1}");
    assert!(j1.contains("\"propertyType\" : \"DATA\""), "{j1}");
    assert!(j1.contains("\"propertyType\" : \"ANNOTATION\""), "{j1}");

    // Reverse order produces identical bytes
    let rev: Vec<&Axiom> = prop_axioms.iter().rev().collect();
    let (j2, _) = obographs::document(
        &renderer(),
        &Header::default(),
        rev,
        WriteOptions { strict: true },
    )
    .unwrap();
    assert_eq!(j1, j2, "OBO Graphs JSON must be order-independent");

    let back = obographs::parse(&j1).unwrap();
    assert_eq!(
        Document::new(back.header, back.axioms),
        Document::new(Header::default(), prop_axioms)
    );

    // 2. Class / NamedIndividual punning collision refuses/skips and never turns assertion into SubClassOf
    let punned = [
        Axiom::declaration(Entity::Class(cls("X"))),
        Axiom::declaration(Entity::NamedIndividual(NamedIndividual::new(ex("X")))),
        Axiom::declaration(Entity::NamedIndividual(NamedIndividual::new(ex("Y")))),
        Axiom::declaration(Entity::ObjectProperty(op("p"))),
        Axiom::new(AxiomKind::ObjectPropertyAssertion(
            op("p").into(),
            ind("X"),
            ind("Y"),
        )),
    ];
    assert!(matches!(
        obographs::document(
            &renderer(),
            &Header::default(),
            punned.iter(),
            WriteOptions { strict: true },
        ),
        Err(WriteError::Unsupported(_))
    ));
    let (pun_json, pun_rep) = obographs::document(
        &renderer(),
        &Header::default(),
        punned.iter(),
        WriteOptions::default(),
    )
    .unwrap();
    assert_eq!(pun_rep.skipped_count, 2, "{pun_rep:?}");
    let pun_back = obographs::parse(&pun_json).unwrap();
    assert!(
        !pun_back
            .axioms
            .iter()
            .any(|a| matches!(a.kind, AxiomKind::SubClassOf(..))),
        "punned assertion must never silently become SubClassOf"
    );

    // 3. Typed literal & IRI-valued generic annotations (node, edge, header) refused in strict mode & reported in lenient
    let typed_node_ann = [
        Axiom::declaration(Entity::Class(cls("A"))),
        Axiom::new(AxiomKind::AnnotationAssertion(
            ap("count"),
            AnnotationSubject::Iri(iri(ex("A"))),
            AnnotationValue::Literal(Literal::typed("7", vocab::XSD_INTEGER)),
        )),
    ];
    let iri_node_ann = [
        Axiom::declaration(Entity::Class(cls("A"))),
        Axiom::new(AxiomKind::AnnotationAssertion(
            ap("seeRef"),
            AnnotationSubject::Iri(iri(ex("A"))),
            AnnotationValue::Iri(iri(ex("Other"))),
        )),
    ];
    let typed_edge_ann = [
        Axiom::sub_class_of(cls("A"), cls("B")).annotated(Annotation::new(
            ap("weight"),
            AnnotationValue::Literal(Literal::typed("7", vocab::XSD_INTEGER)),
        )),
    ];
    for case in [&typed_node_ann[..], &iri_node_ann[..], &typed_edge_ann[..]] {
        assert!(matches!(
            obographs::document(
                &renderer(),
                &Header::default(),
                case.iter(),
                WriteOptions { strict: true },
            ),
            Err(WriteError::Unsupported(_))
        ));
        let (_, rep) = obographs::document(
            &renderer(),
            &Header::default(),
            case.iter(),
            WriteOptions::default(),
        )
        .unwrap();
        assert_eq!(rep.skipped_count, 1, "{rep:?}");
        assert!(!rep.is_lossless());
    }

    // Header-only loss (language-tagged ontology annotation)
    let mut lang_hdr = Header::default();
    lang_hdr.annotations.insert(Annotation::new(
        AnnotationProperty::new(vocab::RDFS_LABEL),
        AnnotationValue::Literal(Literal::lang("bonjour", "fr").unwrap()),
    ));
    assert!(matches!(
        obographs::document(
            &renderer(),
            &lang_hdr,
            std::iter::empty::<&Axiom>(),
            WriteOptions { strict: true },
        ),
        Err(WriteError::Unsupported(_))
    ));
    let (_, hdr_rep) = obographs::document(
        &renderer(),
        &lang_hdr,
        std::iter::empty::<&Axiom>(),
        WriteOptions::default(),
    )
    .unwrap();
    assert_eq!(hdr_rep.skipped_count, 0);
    assert_eq!(hdr_rep.skipped_header_count, 1);
    assert!(!hdr_rep.is_lossless());

    // 4. WriteError::Io preserved when failing INSIDE serde_json::to_writer
    struct FailAfter {
        remaining: usize,
        failed: bool,
    }
    impl Write for FailAfter {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            if self.remaining == 0 {
                self.failed = true;
                return Err(std::io::Error::new(
                    std::io::ErrorKind::BrokenPipe,
                    "simulated broken pipe inside serde_json",
                ));
            }
            let take = buf.len().min(self.remaining);
            self.remaining -= take;
            Ok(take)
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    // "{\n  \"graphs\" : [ {\n    \"id\" : " is 30 bytes; failing at byte 34 triggers inside
    // write_json_str's serde_json::to_writer call writing "\"http://ex.org/g\"".
    let mut fw = FailAfter {
        remaining: 34,
        failed: false,
    };
    let err = obographs::write_document(
        &mut fw,
        &renderer(),
        &Header {
            iri: Some(iri(ex("g"))),
            ..Default::default()
        },
        std::iter::empty::<&Axiom>(),
        WriteOptions::default(),
    )
    .unwrap_err();
    assert!(fw.failed);
    assert!(
        matches!(err, WriteError::Io(_)),
        "expected WriteError::Io from serde_json::to_writer, got {err:?}"
    );

    // 5. MAX_REPORTED_SKIPPED bound & deep unsupported axiom bound
    let many_unsup: Vec<Axiom> = (0..1_200)
        .map(|i| Axiom::sub_class_of(cls(&format!("A{i}")), ClassExpression::not(cls("B"))))
        .collect();
    let (_, rep_many) = obographs::document(
        &renderer(),
        &Header::default(),
        many_unsup.iter(),
        WriteOptions::default(),
    )
    .unwrap();
    assert_eq!(rep_many.skipped_count, 1_200);
    assert_eq!(rep_many.skipped.len(), MAX_REPORTED_SKIPPED);

    let mut deep = ClassExpression::Class(cls("A"));
    for _ in 0..140 {
        deep = ClassExpression::not(deep);
    }
    let deep_ax = Axiom::sub_class_of(cls("B"), deep);
    for strict in [false, true] {
        let err = obographs::document(
            &renderer(),
            &Header::default(),
            [&deep_ax],
            WriteOptions { strict },
        )
        .unwrap_err();
        assert!(matches!(err, WriteError::Unsupported(_)), "{err:?}");
    }
}

// ================================================== Round 2 Regression Tests ===

#[test]
fn owx_pn_prefix_unicode_boundary_and_doctype_comments_and_trailing_declaration() {
    // 1. U+00AA ('ª') is Unicode alphabetic but outside W3C PN_CHARS_BASE (starts at U+00C0);
    // U+00C0 ('À') is valid PN_CHARS_BASE.
    let r = Renderer::new([
        ("\u{00AA}", "http://invalid-pn.example/"),
        ("\u{00C0}", "http://valid-pn.example/"),
    ]);
    let axioms = [
        Axiom::declaration(Entity::Class(Class::new("http://invalid-pn.example/Thing"))),
        Axiom::declaration(Entity::Class(Class::new("http://valid-pn.example/Thing"))),
    ];
    let xml = owx::document(&r, &Header::default(), axioms.iter()).unwrap();
    assert!(
        !xml.contains("name=\"\u{00AA}\""),
        "U+00AA must not be emitted as <Prefix>: {xml}"
    );
    assert!(
        xml.contains("IRI=\"http://invalid-pn.example/Thing\""),
        "invalid prefix IRI must fall back to full IRI: {xml}"
    );
    assert!(
        xml.contains("name=\"\u{00C0}\""),
        "U+00C0 is valid PN_PREFIX: {xml}"
    );
    assert!(xml.contains("abbreviatedIRI=\"\u{00C0}:Thing\""), "{xml}");
    let back = owx::parse(&xml).unwrap();
    assert_eq!(back.axioms, axioms);

    // 2. DTD internal subset containing a comment with an apostrophe must parse cleanly.
    let dtd_comment = r#"<!DOCTYPE Ontology [<!-- that's fine -->]><Ontology xmlns="http://www.w3.org/2002/07/owl#"/>"#;
    assert!(
        owx::parse(dtd_comment).is_ok(),
        "apostrophe inside DTD comment must not start a quoted literal"
    );

    // 3. Trailing <!DOCTYPE>, in-element <!DOCTYPE>, and duplicate prolog <!DOCTYPE> must be rejected.
    let trailing_dtd = r#"<Ontology xmlns="http://www.w3.org/2002/07/owl#"/><!DOCTYPE Ontology>"#;
    assert!(owx::parse(trailing_dtd).is_err());
    let inner_dtd =
        r#"<Ontology xmlns="http://www.w3.org/2002/07/owl#"><!DOCTYPE Ontology></Ontology>"#;
    assert!(owx::parse(inner_dtd).is_err());
    let dup_dtd = r#"<!DOCTYPE Ontology><!DOCTYPE Ontology><Ontology xmlns="http://www.w3.org/2002/07/owl#"/>"#;
    assert!(owx::parse(dup_dtd).is_err());
}

#[test]
fn manchester_punned_object_keys_depth_boundary_prefixes_and_blank_nodes() {
    // 1. Undeclared IRI punned as ObjectProperty + DataProperty:
    // (a) Object-only HasKey(:C (:p) ()) + DataSomeValuesFrom(:p xsd:string) must not turn into data key.
    let obj_key_pun = [
        Axiom::new(AxiomKind::HasKey(
            cls("C").into(),
            [op("p").into()].into_iter().collect(),
            BTreeSet::new(),
        )),
        Axiom::sub_class_of(
            cls("D"),
            ClassExpression::DataSomeValuesFrom(
                vec![dp("p")],
                DataRange::Datatype(Datatype::new(vocab::XSD_STRING)),
            ),
        ),
    ];
    assert!(matches!(
        manchester::document(
            &renderer(),
            &Header::default(),
            obj_key_pun.iter(),
            WriteOptions { strict: true },
        ),
        Err(WriteError::Unsupported(_))
    ));
    let (omn_k, rep_k) = manchester::document(
        &renderer(),
        &Header::default(),
        obj_key_pun.iter(),
        WriteOptions::default(),
    )
    .unwrap();
    assert_eq!(rep_k.skipped_count, 1, "{rep_k:?}");
    let back_k = manchester::parse_document(&omn_k).unwrap();
    assert!(
        !back_k
            .axioms
            .iter()
            .any(|a| matches!(a.kind, AxiomKind::HasKey(_, _, ref dps) if !dps.is_empty())),
        "object key must never silently become a data key:\n{omn_k}"
    );

    // (b) Unqualified ObjectMinCardinality(1, :p, None) + DataHasValue(:p "x") must not turn into DataMinCardinality.
    let obj_card_pun = [
        Axiom::sub_class_of(
            cls("C"),
            ClassExpression::ObjectMinCardinality(1, op("p").into(), None),
        ),
        Axiom::sub_class_of(
            cls("C"),
            ClassExpression::DataHasValue(dp("p"), Literal::simple("x")),
        ),
    ];
    assert!(matches!(
        manchester::document(
            &renderer(),
            &Header::default(),
            obj_card_pun.iter(),
            WriteOptions { strict: true },
        ),
        Err(WriteError::Unsupported(_))
    ));
    let (omn_c, rep_c) = manchester::document(
        &renderer(),
        &Header::default(),
        obj_card_pun.iter(),
        WriteOptions::default(),
    )
    .unwrap();
    assert_eq!(rep_c.skipped_count, 1, "{rep_c:?}");
    let back_c = manchester::parse_document(&omn_c).unwrap();
    assert!(
        !back_c.axioms.iter().any(|a| matches!(
            a.kind,
            AxiomKind::SubClassOf(_, ClassExpression::DataMinCardinality(..))
        )),
        "ObjectMinCardinality must never corrupt to DataMinCardinality:\n{omn_c}"
    );

    // (c) N-ary EquivalentObjectProperties + DataHasValue on all members must not corrupt to EquivalentDataProperties.
    let nary_obj_pun = [
        Axiom::new(AxiomKind::EquivalentObjectProperties(
            [op("p1").into(), op("p2").into(), op("p3").into()]
                .into_iter()
                .collect(),
        )),
        Axiom::sub_class_of(
            cls("C"),
            ClassExpression::DataHasValue(dp("p1"), Literal::simple("a")),
        ),
        Axiom::sub_class_of(
            cls("C"),
            ClassExpression::DataHasValue(dp("p2"), Literal::simple("b")),
        ),
        Axiom::sub_class_of(
            cls("C"),
            ClassExpression::DataHasValue(dp("p3"), Literal::simple("c")),
        ),
    ];
    assert!(matches!(
        manchester::document(
            &renderer(),
            &Header::default(),
            nary_obj_pun.iter(),
            WriteOptions { strict: true },
        ),
        Err(WriteError::Unsupported(_))
    ));

    // (d) Unambiguous ObjectSomeValuesFrom(:p :D) + DataHasValue(:p "x") with NO declarations must
    // synthesize both ObjectProperty: :p and DataProperty: :p frames and parse cleanly.
    let unambig_pun = [
        Axiom::sub_class_of(cls("C"), ClassExpression::some(op("p"), cls("D"))),
        Axiom::sub_class_of(
            cls("C"),
            ClassExpression::DataHasValue(dp("p"), Literal::simple("x")),
        ),
    ];
    let (omn_u, rep_u) = manchester::document(
        &renderer(),
        &Header::default(),
        unambig_pun.iter(),
        WriteOptions { strict: true },
    )
    .unwrap();
    assert!(rep_u.is_lossless(), "{rep_u:?}");
    assert!(omn_u.contains("ObjectProperty: :p"), "{omn_u}");
    assert!(omn_u.contains("DataProperty: :p"), "{omn_u}");
    let back_u = manchester::parse_document(&omn_u).unwrap();
    for expected in &unambig_pun {
        assert!(
            back_u.axioms.contains(expected),
            "missing {expected:?} in:\n{omn_u}"
        );
    }

    // 2. 64/65-complement (and 127-complement MAX_NESTING) boundary for class & data expressions
    for n in [64usize, 65, 127] {
        let mut ce = ClassExpression::Class(cls("A"));
        let mut dr = DataRange::Datatype(Datatype::new(vocab::XSD_INTEGER));
        for _ in 0..n {
            ce = ClassExpression::not(ce);
            dr = DataRange::ComplementOf(Box::new(dr));
        }
        let axioms = [
            Axiom::declaration(Entity::Class(cls("A"))),
            Axiom::declaration(Entity::Class(cls("B"))),
            Axiom::declaration(Entity::DataProperty(dp("d"))),
            Axiom::sub_class_of(cls("B"), ce),
            Axiom::new(AxiomKind::DataPropertyRange(dp("d"), dr)),
        ];
        let (omn, rep) = manchester::document(
            &renderer(),
            &Header::default(),
            axioms.iter(),
            WriteOptions { strict: true },
        )
        .unwrap();
        assert!(rep.is_lossless(), "n={n}: {rep:?}");
        let back = manchester::parse_document(&omn)
            .unwrap_or_else(|e| panic!("n={n} failed to parse back: {e}"));
        assert_eq!(
            Document::new(back.header, back.axioms),
            Document::new(Header::default(), axioms.to_vec()),
            "n={n}"
        );
    }
    // 128 complements = model depth 129 > MAX_NESTING must be refused
    let mut over_ce = ClassExpression::Class(cls("A"));
    for _ in 0..128 {
        over_ce = ClassExpression::not(over_ce);
    }
    let over_ax = Axiom::sub_class_of(cls("B"), over_ce);
    assert!(matches!(
        manchester::document(
            &renderer(),
            &Header::default(),
            [&over_ax],
            WriteOptions { strict: true },
        ),
        Err(WriteError::Unsupported(_))
    ));

    // 3. Non-ASCII prefix U+00AA dropped + rebound standard prefix `xsd` retains standard binding
    let bad_pref_r = Renderer::new([
        ("\u{00AA}", "http://invalid-pn.example/"),
        ("xsd", "https://example.org/"),
    ]);
    let pref_axioms = [
        Axiom::declaration(Entity::Class(Class::new("http://invalid-pn.example/Thing"))),
        Axiom::declaration(Entity::Class(Class::new("https://example.org/Thing"))),
        Axiom::new(AxiomKind::DataPropertyRange(
            dp("age"),
            DataRange::Datatype(Datatype::new(vocab::XSD_INTEGER)),
        )),
    ];
    let (omn_p, rep_p) = manchester::document(
        &bad_pref_r,
        &Header::default(),
        pref_axioms.iter(),
        WriteOptions { strict: true },
    )
    .unwrap();
    assert!(rep_p.is_lossless());
    assert!(!omn_p.contains("Prefix: \u{00AA}:"), "{omn_p}");
    assert!(
        omn_p.contains("Prefix: xsd: <http://www.w3.org/2001/XMLSchema#>"),
        "{omn_p}"
    );
    assert!(
        !omn_p.contains("Prefix: xsd: <https://example.org/>"),
        "{omn_p}"
    );
    assert!(omn_p.contains("<https://example.org/Thing>"), "{omn_p}");
    let back_p = manchester::parse_document(&omn_p).unwrap();
    assert!(
        back_p
            .axioms
            .contains(&Axiom::declaration(Entity::Class(Class::new(
                "https://example.org/Thing"
            ))))
    );

    // 4. Blank node labels: `a` vs `a#b` (comment stripping collision), `a/b`, `a.`, and header anon lenient skip
    let bnode_collide = [
        Axiom::class_assertion(cls("C"), Individual::anonymous("a")),
        Axiom::class_assertion(cls("C"), Individual::anonymous("a#b")),
        Axiom::class_assertion(cls("C"), Individual::anonymous("a/b")),
        Axiom::class_assertion(cls("C"), Individual::anonymous("a.")),
    ];
    assert!(matches!(
        manchester::document(
            &renderer(),
            &Header::default(),
            bnode_collide.iter(),
            WriteOptions { strict: true },
        ),
        Err(WriteError::Unsupported(_))
    ));
    let (omn_bn, rep_bn) = manchester::document(
        &renderer(),
        &Header::default(),
        bnode_collide.iter(),
        WriteOptions::default(),
    )
    .unwrap();
    assert_eq!(rep_bn.written_axioms, 1, "{rep_bn:?}");
    assert_eq!(rep_bn.skipped_count, 3, "{rep_bn:?}");
    let back_bn = manchester::parse_document(&omn_bn).unwrap();
    assert!(back_bn.axioms.contains(&Axiom::class_assertion(
        cls("C"),
        Individual::anonymous("a")
    )));
    assert!(!omn_bn.contains("a#b") && !omn_bn.contains("a/b") && !omn_bn.contains("a."));

    let mut bad_hdr = Header::default();
    bad_hdr.annotations.insert(Annotation::new(
        AnnotationProperty::new(vocab::RDFS_SEE_ALSO),
        AnnotationValue::Anonymous(AnonymousIndividual::new("a#b")),
    ));
    bad_hdr.annotations.insert(Annotation::comment("keep me"));
    assert!(matches!(
        manchester::document(
            &renderer(),
            &bad_hdr,
            std::iter::empty::<&Axiom>(),
            WriteOptions { strict: true },
        ),
        Err(WriteError::Unsupported(_))
    ));
    let (omn_hdr, rep_hdr) = manchester::document(
        &renderer(),
        &bad_hdr,
        std::iter::empty::<&Axiom>(),
        WriteOptions::default(),
    )
    .unwrap();
    assert_eq!(rep_hdr.skipped_header_count, 1, "{rep_hdr:?}");
    assert!(!rep_hdr.is_lossless());
    let back_hdr = manchester::parse_document(&omn_hdr).unwrap();
    assert!(
        back_hdr
            .header
            .annotations
            .contains(&Annotation::comment("keep me"))
    );
}

#[test]
fn obo_interoperable_ids_typedef_cardinality_escaping_entity_kinds_and_headers() {
    // 1. Non-HTTP(S) IRIs without a matching idspace must be refused/skipped (OBO 1.4 §5.9.2).
    for raw in [
        "doi:10.1234/abc",
        "ftp://example.org/cls/1",
        "file:///tmp/onto#C1",
    ] {
        let ax = [Axiom::declaration(Entity::Class(Class::new(raw)))];
        assert!(
            matches!(
                obo::document(
                    &renderer(),
                    &Header::default(),
                    ax.iter(),
                    WriteOptions { strict: true },
                ),
                Err(WriteError::Unsupported(_))
            ),
            "{raw} must be refused without idspace"
        );
        let (_, rep) = obo::document(
            &renderer(),
            &Header::default(),
            ax.iter(),
            WriteOptions::default(),
        )
        .unwrap();
        assert_eq!(rep.skipped_count, 1, "{raw}: {rep:?}");
    }

    // 2. [Typedef] single-cardinality domain and range (ObjectProperty & AnnotationProperty)
    let multi_dom_rng_cases = [
        vec![
            Axiom::declaration(Entity::ObjectProperty(op("p"))),
            Axiom::new(AxiomKind::ObjectPropertyDomain(
                op("p").into(),
                cls("A").into(),
            )),
            Axiom::new(AxiomKind::ObjectPropertyDomain(
                op("p").into(),
                cls("B").into(),
            )),
        ],
        vec![
            Axiom::declaration(Entity::ObjectProperty(op("p"))),
            Axiom::new(AxiomKind::ObjectPropertyRange(
                op("p").into(),
                cls("A").into(),
            )),
            Axiom::new(AxiomKind::ObjectPropertyRange(
                op("p").into(),
                cls("B").into(),
            )),
        ],
        vec![
            Axiom::declaration(Entity::AnnotationProperty(ap("a"))),
            Axiom::new(AxiomKind::AnnotationPropertyDomain(ap("a"), iri(ex("A")))),
            Axiom::new(AxiomKind::AnnotationPropertyDomain(ap("a"), iri(ex("B")))),
        ],
        vec![
            Axiom::declaration(Entity::AnnotationProperty(ap("a"))),
            Axiom::new(AxiomKind::AnnotationPropertyRange(ap("a"), iri(ex("A")))),
            Axiom::new(AxiomKind::AnnotationPropertyRange(ap("a"), iri(ex("B")))),
        ],
    ];
    for case in &multi_dom_rng_cases {
        assert!(matches!(
            obo::document(
                &renderer(),
                &Header::default(),
                case.iter(),
                WriteOptions { strict: true },
            ),
            Err(WriteError::Unsupported(_))
        ));
        let (obo_text, rep) = obo::document(
            &renderer(),
            &Header::default(),
            case.iter(),
            WriteOptions::default(),
        )
        .unwrap();
        assert_eq!(rep.skipped_count, 1, "{rep:?}");
        assert_valid_obo_structure(&obo_text);
    }

    // 3. NBSP boundaries, empty nested dbxref, and carriage return (\r) in strict and lenient modes
    let nbsp_and_empty_xref = vec![
        Axiom::declaration(Entity::Class(cls("C"))),
        Axiom::new(AxiomKind::AnnotationAssertion(
            AnnotationProperty::new(vocab::RDFS_LABEL),
            AnnotationSubject::Iri(iri(ex("C"))),
            AnnotationValue::Literal(Literal::simple("\u{00A0}padded\u{00A0}")),
        )),
        Axiom {
            kind: AxiomKind::AnnotationAssertion(
                AnnotationProperty::new(IAO_DEFINITION),
                AnnotationSubject::Iri(iri(ex("C"))),
                AnnotationValue::Literal(Literal::simple("def with empty xref")),
            ),
            annotations: [Annotation::new(
                AnnotationProperty::new(OIO_HAS_DBXREF),
                AnnotationValue::Literal(Literal::simple("")),
            )]
            .into_iter()
            .collect(),
        },
    ];
    let (obo_esc, rep_esc) = obo::document(
        &renderer(),
        &Header::default(),
        nbsp_and_empty_xref.iter(),
        WriteOptions { strict: true },
    )
    .unwrap();
    assert!(rep_esc.is_lossless(), "{rep_esc:?}");
    let back_esc = obo::parse(&obo_esc).unwrap();
    assert_eq!(
        Document::new(back_esc.header, back_esc.axioms),
        Document::new(Header::default(), nbsp_and_empty_xref)
    );

    let cr_axioms = [
        Axiom::declaration(Entity::Class(cls("C"))),
        Axiom::new(AxiomKind::AnnotationAssertion(
            AnnotationProperty::new(vocab::RDFS_LABEL),
            AnnotationSubject::Iri(iri(ex("C"))),
            AnnotationValue::Literal(Literal::simple("bad\rlabel")),
        )),
    ];
    assert!(matches!(
        obo::document(
            &renderer(),
            &Header::default(),
            cr_axioms.iter(),
            WriteOptions { strict: true },
        ),
        Err(WriteError::Unsupported(_))
    ));
    let (obo_cr, rep_cr) = obo::document(
        &renderer(),
        &Header::default(),
        cr_axioms.iter(),
        WriteOptions::default(),
    )
    .unwrap();
    assert_eq!(rep_cr.skipped_count, 1, "{rep_cr:?}");
    assert!(obo::parse(&obo_cr).is_ok());

    // 4. Cross-stanza entity-kind conflicts: Class+NamedIndividual, ObjectProperty+AnnotationProperty,
    // and same-axiom ClassAssertion(:X :X)
    let kind_conflicts: Vec<Vec<Axiom>> = vec![
        vec![
            Axiom::declaration(Entity::Class(cls("X"))),
            Axiom::declaration(Entity::NamedIndividual(NamedIndividual::new(ex("X")))),
        ],
        vec![
            Axiom::declaration(Entity::ObjectProperty(op("p"))),
            Axiom::declaration(Entity::AnnotationProperty(ap("p"))),
        ],
        vec![Axiom::class_assertion(cls("X"), ind("X"))],
        vec![Axiom::new(AxiomKind::ObjectPropertyDomain(
            op("p").into(),
            cls("p").into(),
        ))],
    ];
    for case in &kind_conflicts {
        assert!(matches!(
            obo::document(
                &renderer(),
                &Header::default(),
                case.iter(),
                WriteOptions { strict: true },
            ),
            Err(WriteError::Unsupported(_))
        ));
        let (obo_k, rep_k) = obo::document(
            &renderer(),
            &Header::default(),
            case.iter(),
            WriteOptions::default(),
        )
        .unwrap();
        assert_eq!(rep_k.skipped_count, 1, "{case:?}: {rep_k:?}");
        assert_valid_obo_structure(&obo_k);
    }

    // 5. Reserved header fields: URL-shaped owl:versionInfo, hasOBOFormatVersion "1.4",
    // non-HTTP(S) versionIRI, and non-HTTP(S) ontology IRI (`did:example:demo`)
    let mut hdr_ok = Header {
        iri: Some(iri("https://example.org/onto")),
        ..Default::default()
    };
    hdr_ok.annotations.insert(Annotation::new(
        AnnotationProperty::new(vocab::OWL_VERSION_INFO),
        AnnotationValue::Literal(Literal::simple("https://example.org/releases/v2")),
    ));
    hdr_ok.annotations.insert(Annotation::new(
        AnnotationProperty::new(obo::OIO_HAS_OBO_FORMAT_VERSION),
        AnnotationValue::Literal(Literal::simple("1.4")),
    ));
    let (obo_h, rep_h) = obo::document(
        &renderer(),
        &hdr_ok,
        std::iter::empty::<&Axiom>(),
        WriteOptions { strict: true },
    )
    .unwrap();
    assert!(rep_h.is_lossless(), "{rep_h:?}");
    let back_h = obo::parse(&obo_h).unwrap();
    assert_eq!(back_h.header, hdr_ok, "{obo_h}");

    for bad_hdr in [
        Header {
            iri: Some(iri("did:example:demo")),
            ..Default::default()
        },
        Header {
            iri: Some(iri("https://example.org/onto")),
            version_iri: Some(iri("urn:example:v2")),
            ..Default::default()
        },
    ] {
        assert!(matches!(
            obo::document(
                &renderer(),
                &bad_hdr,
                std::iter::empty::<&Axiom>(),
                WriteOptions { strict: true },
            ),
            Err(WriteError::Unsupported(_))
        ));
        let (_, rep_bad) = obo::document(
            &renderer(),
            &bad_hdr,
            std::iter::empty::<&Axiom>(),
            WriteOptions::default(),
        )
        .unwrap();
        assert_eq!(rep_bad.skipped_header_count, 1, "{rep_bad:?}");
        assert!(!rep_bad.is_lossless());
    }
}

#[test]
fn obographs_same_axiom_punning_stub_nodes_and_reserved_imports_header() {
    // 1. Same-axiom kind collisions: ClassAssertion(:X :X) and ObjectPropertyDomain(:p :p)
    for case in [
        vec![Axiom::class_assertion(cls("X"), ind("X"))],
        vec![Axiom::new(AxiomKind::ObjectPropertyDomain(
            op("p").into(),
            cls("p").into(),
        ))],
    ] {
        assert!(matches!(
            obographs::document(
                &renderer(),
                &Header::default(),
                case.iter(),
                WriteOptions { strict: true },
            ),
            Err(WriteError::Unsupported(_))
        ));
        let (json_text, rep) = obographs::document(
            &renderer(),
            &Header::default(),
            case.iter(),
            WriteOptions::default(),
        )
        .unwrap();
        assert_eq!(rep.skipped_count, 1, "{case:?}: {rep:?}");
        assert_valid_obographs_schema(&json_text);
        let parsed = obographs::parse(&json_text).unwrap();
        assert!(parsed.axioms.is_empty(), "{json_text}");
    }

    // 2. Lenient skipped AnnotationAssertion on undeclared IRI must not leave an empty stub node
    let undecl_typed = [Axiom::new(AxiomKind::AnnotationAssertion(
        ap("count"),
        AnnotationSubject::Iri(iri(ex("Ghost"))),
        AnnotationValue::Literal(Literal::typed("7", vocab::XSD_INTEGER)),
    ))];
    let (json_stub, rep_stub) = obographs::document(
        &renderer(),
        &Header::default(),
        undecl_typed.iter(),
        WriteOptions::default(),
    )
    .unwrap();
    assert_eq!(rep_stub.skipped_count, 1);
    assert!(
        !json_stub.contains("Ghost"),
        "skipped annotation on undeclared IRI must not create a stub node:\n{json_stub}"
    );

    // 3. Header annotation with reserved predicate owl:imports must not silently become an import
    let mut imp_hdr = Header::default();
    imp_hdr.annotations.insert(Annotation::new(
        AnnotationProperty::new(vocab::OWL_IMPORTS),
        AnnotationValue::Literal(Literal::simple("not an iri")),
    ));
    assert!(matches!(
        obographs::document(
            &renderer(),
            &imp_hdr,
            std::iter::empty::<&Axiom>(),
            WriteOptions { strict: true },
        ),
        Err(WriteError::Unsupported(_))
    ));
    let (json_imp, rep_imp) = obographs::document(
        &renderer(),
        &imp_hdr,
        std::iter::empty::<&Axiom>(),
        WriteOptions::default(),
    )
    .unwrap();
    assert_eq!(rep_imp.skipped_header_count, 1, "{rep_imp:?}");
    let back_imp = obographs::parse(&json_imp).unwrap();
    assert!(back_imp.header.imports.is_empty());
}

#[test]
fn manchester_builtin_punning_nested_restriction_lookahead_and_left_boolean_depth() {
    // 1. P2.1(a): DataMinCardinality on builtin owl:topObjectProperty must not silently
    // round-trip as ObjectMinCardinality.
    let builtin_data_card = [Axiom::sub_class_of(
        cls("C"),
        ClassExpression::DataMinCardinality(
            1,
            DataProperty::new(vocab::OWL_TOP_OBJECT_PROPERTY),
            None,
        ),
    )];
    assert!(matches!(
        manchester::document(
            &renderer(),
            &Header::default(),
            builtin_data_card.iter(),
            WriteOptions { strict: true },
        ),
        Err(WriteError::Unsupported(_))
    ));
    let (omn_bd, rep_bd) = manchester::document(
        &renderer(),
        &Header::default(),
        builtin_data_card.iter(),
        WriteOptions::default(),
    )
    .unwrap();
    assert_eq!(rep_bd.skipped_count, 1, "{rep_bd:?}");
    let back_bd = manchester::parse_document(&omn_bd).unwrap();
    assert!(
        !back_bd.axioms.iter().any(|a| matches!(
            a.kind,
            AxiomKind::SubClassOf(_, ClassExpression::ObjectMinCardinality(..))
        )),
        "builtin owl:topObjectProperty DataMinCardinality must not become ObjectMinCardinality:\n{omn_bd}"
    );

    // Symmetric builtin case: ObjectMinCardinality on builtin owl:topDataProperty.
    let builtin_obj_card = [Axiom::sub_class_of(
        cls("C"),
        ClassExpression::ObjectMinCardinality(
            1,
            ObjectProperty::new(vocab::OWL_TOP_DATA_PROPERTY).into(),
            None,
        ),
    )];
    assert!(matches!(
        manchester::document(
            &renderer(),
            &Header::default(),
            builtin_obj_card.iter(),
            WriteOptions { strict: true },
        ),
        Err(WriteError::Unsupported(_))
    ));

    // 2. P2.1(b): Punned :p (ObjectProperty + DataProperty) with qualified nested object
    // restriction whose property :q is also declared as a Datatype.
    let nested_dt_prop = [
        Axiom::declaration(Entity::ObjectProperty(op("p"))),
        Axiom::declaration(Entity::DataProperty(dp("p"))),
        Axiom::declaration(Entity::Datatype(Datatype::new(ex("q")))),
        Axiom::sub_class_of(
            cls("C"),
            ClassExpression::some(op("p"), ClassExpression::some(op("q"), cls("D"))),
        ),
    ];
    let (omn_nq, rep_nq) = manchester::document(
        &renderer(),
        &Header::default(),
        nested_dt_prop.iter(),
        WriteOptions { strict: true },
    )
    .unwrap();
    assert!(rep_nq.is_lossless(), "{rep_nq:?}");
    let back_nq = manchester::parse_document(&omn_nq).unwrap_or_else(|e| {
        panic!("failed to parse nested datatype-punned restriction: {e}\n{omn_nq}")
    });
    for expected in &nested_dt_prop {
        assert!(
            back_nq.axioms.contains(expected),
            "missing {expected:?} in:\n{omn_nq}"
        );
    }

    // 3. P2.1(c): Builtin datatype xsd:string used as Class filler under punned :p must
    // synthesize a Class: xsd:string frame so the reader keeps it as ObjectSomeValuesFrom.
    let builtin_dt_class = [
        Axiom::declaration(Entity::ObjectProperty(op("p"))),
        Axiom::declaration(Entity::DataProperty(dp("p"))),
        Axiom::sub_class_of(
            cls("C"),
            ClassExpression::some(op("p"), Class::new(vocab::XSD_STRING)),
        ),
    ];
    let (omn_bc, rep_bc) = manchester::document(
        &renderer(),
        &Header::default(),
        builtin_dt_class.iter(),
        WriteOptions { strict: true },
    )
    .unwrap();
    assert!(rep_bc.is_lossless(), "{rep_bc:?}");
    let back_bc = manchester::parse_document(&omn_bc).unwrap();
    assert!(
        back_bc.axioms.contains(&builtin_dt_class[2]),
        "missing ObjectSomeValuesFrom(:p xsd:string) in:\n{omn_bc}"
    );

    // 4. P2.2: Left-nested boolean expressions at exact semantic depth 128 (accepted by
    // both writer and parser) and depth 129 (rejected by both writer and parser).
    fn left_nested_ce(n: usize, union: bool) -> ClassExpression {
        let mut ce = ClassExpression::Class(cls("A"));
        for i in 0..n {
            let other = ClassExpression::Class(cls(&format!("B{i}")));
            let set = [ce, other].into_iter().collect();
            ce = if union {
                ClassExpression::ObjectUnionOf(set)
            } else {
                ClassExpression::ObjectIntersectionOf(set)
            };
        }
        ce
    }
    fn left_nested_dr(n: usize, union: bool) -> DataRange {
        let mut dr = DataRange::Datatype(Datatype::new(vocab::XSD_INTEGER));
        for i in 0..n {
            let other = DataRange::Datatype(Datatype::new(ex(&format!("DT{i}"))));
            let set = [dr, other].into_iter().collect();
            dr = if union {
                DataRange::UnionOf(set)
            } else {
                DataRange::IntersectionOf(set)
            };
        }
        dr
    }

    for union in [true, false] {
        // Depth 128 = 127 nested operators over leaf: lossless round-trip
        let ce128 = left_nested_ce(127, union);
        let dr128 = left_nested_dr(127, union);
        let axioms = [
            Axiom::declaration(Entity::Class(cls("C"))),
            Axiom::declaration(Entity::DataProperty(dp("d"))),
            Axiom::sub_class_of(cls("C"), ce128.clone()),
            Axiom::new(AxiomKind::DataPropertyRange(dp("d"), dr128.clone())),
        ];
        let (omn, rep) = manchester::document(
            &renderer(),
            &Header::default(),
            axioms.iter(),
            WriteOptions { strict: true },
        )
        .unwrap();
        assert!(rep.is_lossless(), "union={union}: {rep:?}");
        let back = manchester::parse_document(&omn)
            .unwrap_or_else(|e| panic!("union={union} depth-128 parse failed: {e}"));
        assert!(back.axioms.contains(&axioms[2]));
        assert!(back.axioms.contains(&axioms[3]));

        // Depth 129 = 128 nested operators: writer refuses
        let over_ce_ax = Axiom::sub_class_of(cls("C"), left_nested_ce(128, union));
        assert!(matches!(
            manchester::document(
                &renderer(),
                &Header::default(),
                [&over_ce_ax],
                WriteOptions { strict: true },
            ),
            Err(WriteError::Unsupported(_))
        ));
        let over_dr_ax = Axiom::new(AxiomKind::DataPropertyRange(
            dp("d"),
            left_nested_dr(128, union),
        ));
        assert!(matches!(
            manchester::document(
                &renderer(),
                &Header::default(),
                [&over_dr_ax],
                WriteOptions { strict: true },
            ),
            Err(WriteError::Unsupported(_))
        ));

        // Depth 128 in raw Manchester syntax with left-operand nesting: parser accepts and
        // writer round-trips losslessly; depth 129 in raw Manchester syntax: parser refuses.
        let kw = if union { "or" } else { "and" };
        let mut ce_src_128 = ":A".to_string();
        let mut dr_src_128 = "xsd:integer".to_string();
        for i in 0..127 {
            ce_src_128 = format!("({ce_src_128} {kw} :B{i})");
            dr_src_128 = format!("({dr_src_128} {kw} :DT{i})");
        }
        let ce_src_129 = format!("({ce_src_128} {kw} :B127)");
        let dr_src_129 = format!("({dr_src_128} {kw} :DT127)");

        let mut ce_prefix = String::from("Prefix: : <http://ex.org/>\nOntology:\nClass: :A\n");
        for i in 0..128 {
            ce_prefix.push_str(&format!("Class: :B{i}\n"));
        }
        let ce_doc_128 = format!("{ce_prefix}Class: :C\n  SubClassOf: {ce_src_128}\n");
        let parsed_ce_128 = manchester::parse_document(&ce_doc_128)
            .unwrap_or_else(|e| panic!("parser must accept depth-128 left-nested class {kw}: {e}"));
        let (_, rep_ce_128) = manchester::document(
            &renderer(),
            &parsed_ce_128.header,
            parsed_ce_128.axioms.iter(),
            WriteOptions { strict: true },
        )
        .unwrap();
        assert!(rep_ce_128.is_lossless());

        let ce_doc_129 = format!("{ce_prefix}Class: :C\n  SubClassOf: {ce_src_129}\n");
        assert!(
            manchester::parse_document(&ce_doc_129).is_err(),
            "parser must reject depth-129 left-nested class {kw}"
        );

        let mut dr_prefix = String::from("Prefix: : <http://ex.org/>\nOntology:\n");
        for i in 0..128 {
            dr_prefix.push_str(&format!("Datatype: :DT{i}\n"));
        }
        let dr_doc_128 = format!("{dr_prefix}DataProperty: :d\n  Range: {dr_src_128}\n");
        let parsed_dr_128 = manchester::parse_document(&dr_doc_128)
            .unwrap_or_else(|e| panic!("parser must accept depth-128 left-nested data {kw}: {e}"));
        let (_, rep_dr_128) = manchester::document(
            &renderer(),
            &parsed_dr_128.header,
            parsed_dr_128.axioms.iter(),
            WriteOptions { strict: true },
        )
        .unwrap();
        assert!(rep_dr_128.is_lossless());

        let dr_doc_129 = format!("{dr_prefix}DataProperty: :d\n  Range: {dr_src_129}\n");
        assert!(
            manchester::parse_document(&dr_doc_129).is_err(),
            "parser must reject depth-129 left-nested data {kw}"
        );
    }
}

#[test]
fn obo_nbsp_iris_header_fields_and_wide_logical_definitions() {
    // 1. P2.3: Class IRI ending in U+00A0 (NBSP) or containing internal U+00A0 must not
    // silently trim/split or merge with https://example.org/C.
    let c_plain = Class::new("https://example.org/C");
    let c_trailing_nbsp = Class::new("https://example.org/C\u{00A0}");
    let c_internal_nbsp = Class::new("https://example.org/C\u{00A0}D");
    let axioms = [
        Axiom::declaration(Entity::Class(c_plain.clone())),
        Axiom::declaration(Entity::Class(c_trailing_nbsp.clone())),
        Axiom::declaration(Entity::Class(c_internal_nbsp.clone())),
        Axiom::sub_class_of(
            c_plain.clone(),
            ClassExpression::some(op("part_of"), c_internal_nbsp.clone()),
        ),
    ];
    assert!(matches!(
        obo::document(
            &renderer(),
            &Header::default(),
            axioms.iter(),
            WriteOptions { strict: true },
        ),
        Err(WriteError::Unsupported(_))
    ));
    let (obo_text, rep) = obo::document(
        &renderer(),
        &Header::default(),
        axioms.iter(),
        WriteOptions::default(),
    )
    .unwrap();
    assert_eq!(rep.skipped_count, 3, "{rep:?}\n{obo_text}");
    assert_valid_obo_structure(&obo_text);
    let back = obo::parse(&obo_text).unwrap();
    assert_eq!(
        back.axioms,
        vec![Axiom::declaration(Entity::Class(c_plain))],
        "{obo_text}"
    );

    // 2. P2.3: Reserved OBO header IRIs with NBSP (OBO PURL ontology IRI, HTTP version IRI,
    // and import IRI) must be refused in strict mode and recorded in skipped_header in lenient mode.
    let mut bad_hdr = Header {
        iri: Some(iri("http://purl.obolibrary.org/obo/demo\u{00A0}.owl")),
        version_iri: Some(iri("https://example.org/v1\u{00A0}")),
        ..Default::default()
    };
    bad_hdr
        .imports
        .insert(iri("https://example.org/imp\u{00A0}"));
    assert!(matches!(
        obo::document(
            &renderer(),
            &bad_hdr,
            std::iter::empty::<&Axiom>(),
            WriteOptions { strict: true },
        ),
        Err(WriteError::Unsupported(_))
    ));
    let (hdr_obo, hdr_rep) = obo::document(
        &renderer(),
        &bad_hdr,
        std::iter::empty::<&Axiom>(),
        WriteOptions::default(),
    )
    .unwrap();
    assert_eq!(hdr_rep.skipped_header_count, 3, "{hdr_rep:?}\n{hdr_obo}");
    let hdr_back = obo::parse(&hdr_obo).unwrap();
    assert!(hdr_back.header.iri.is_none());
    assert!(hdr_back.header.version_iri.is_none());
    assert!(hdr_back.header.imports.is_empty());

    // 3. P2.6: Wide logical definitions (5,000 genera) stay linear in both OBO and OBO Graphs
    // JSON, and wide (>4) definitions with an intra-axiom class/property kind conflict are
    // still refused in strict mode and skipped cleanly in lenient mode.
    let mut genera: BTreeSet<ClassExpression> = BTreeSet::new();
    for i in 0..5_000 {
        genera.insert(ClassExpression::Class(Class::new(format!(
            "http://purl.obolibrary.org/obo/GO_{i:07}"
        ))));
    }
    let wide_ax = Axiom::new(AxiomKind::EquivalentClasses(
        [
            ClassExpression::Class(Class::new("http://purl.obolibrary.org/obo/GO_9999999")),
            ClassExpression::ObjectIntersectionOf(genera),
        ]
        .into_iter()
        .collect(),
    ));
    let (wide_obo, wide_obo_rep) = obo::document(
        &renderer(),
        &Header::default(),
        [&wide_ax],
        WriteOptions { strict: true },
    )
    .unwrap();
    assert!(wide_obo_rep.is_lossless());
    assert!(obo::parse(&wide_obo).unwrap().axioms.contains(&wide_ax));
    let (wide_json, wide_json_rep) = obographs::document(
        &renderer(),
        &Header::default(),
        [&wide_ax],
        WriteOptions { strict: true },
    )
    .unwrap();
    assert!(wide_json_rep.is_lossless());
    assert!(
        obographs::parse(&wide_json)
            .unwrap()
            .axioms
            .contains(&wide_ax)
    );

    // Wide (>4) intra-axiom conflict: GO_0000001 used as both genus class and restriction property.
    let conflict_members: BTreeSet<ClassExpression> = [
        ClassExpression::Class(Class::new("http://purl.obolibrary.org/obo/GO_0000001")),
        ClassExpression::Class(Class::new("http://purl.obolibrary.org/obo/GO_0000002")),
        ClassExpression::Class(Class::new("http://purl.obolibrary.org/obo/GO_0000003")),
        ClassExpression::Class(Class::new("http://purl.obolibrary.org/obo/GO_0000004")),
        ClassExpression::some(
            ObjectProperty::new("http://purl.obolibrary.org/obo/GO_0000001"),
            Class::new("http://purl.obolibrary.org/obo/GO_0000005"),
        ),
    ]
    .into_iter()
    .collect();
    let conflict_ax = Axiom::new(AxiomKind::EquivalentClasses(
        [
            ClassExpression::Class(Class::new("http://purl.obolibrary.org/obo/GO_0000010")),
            ClassExpression::ObjectIntersectionOf(conflict_members),
        ]
        .into_iter()
        .collect(),
    ));
    assert!(matches!(
        obo::document(
            &renderer(),
            &Header::default(),
            [&conflict_ax],
            WriteOptions { strict: true },
        ),
        Err(WriteError::Unsupported(_))
    ));
    assert!(matches!(
        obographs::document(
            &renderer(),
            &Header::default(),
            [&conflict_ax],
            WriteOptions { strict: true },
        ),
        Err(WriteError::Unsupported(_))
    ));
    let (c_obo, c_obo_rep) = obo::document(
        &renderer(),
        &Header::default(),
        [&conflict_ax],
        WriteOptions::default(),
    )
    .unwrap();
    assert_eq!(c_obo_rep.skipped_count, 1);
    assert!(obo::parse(&c_obo).unwrap().axioms.is_empty());
    let (c_json, c_json_rep) = obographs::document(
        &renderer(),
        &Header::default(),
        [&conflict_ax],
        WriteOptions::default(),
    )
    .unwrap();
    assert_eq!(c_json_rep.skipped_count, 1);
    assert!(obographs::parse(&c_json).unwrap().axioms.is_empty());
}
