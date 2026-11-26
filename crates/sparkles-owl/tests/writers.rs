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

    // Carriage return in tag text refused cleanly
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
        WriteOptions::default(),
    )
    .unwrap_err();
    assert!(matches!(err, WriteError::Unsupported(_)), "{err:?}");
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
