use oxrdf::BlankNode;
use sparkles_owl::classify::{Classifier, Consistency, Limits};
use sparkles_owl::{
    Annotation, Axiom, AxiomKind, Class, ClassExpression, DataProperty, Entity, Individual,
    ObjectProperty, ObjectPropertyExpression, json, manchester, ofn, ofn_read, read, validate,
    write,
};

const PREFIX: &str = "Prefix(:=<http://ex.org/>)\n\
    Prefix(owl:=<http://www.w3.org/2002/07/owl#>)\n\
    Prefix(rdfs:=<http://www.w3.org/2000/01/rdf-schema#>)\n\
    Prefix(xsd:=<http://www.w3.org/2001/XMLSchema#>)\n";

fn ex(local: &str) -> String {
    format!("http://ex.org/{local}")
}

fn c(local: &str) -> Class {
    Class::new(ex(local))
}

fn ce(local: &str) -> ClassExpression {
    ClassExpression::Class(c(local))
}

fn op(local: &str) -> ObjectPropertyExpression {
    ObjectPropertyExpression::Property(ObjectProperty::new(ex(local)))
}

fn dp(local: &str) -> DataProperty {
    DataProperty::new(ex(local))
}

fn ind(local: &str) -> Individual {
    Individual::named(ex(local))
}

fn parse_ofn(body: &str) -> ofn_read::Document {
    let text = format!("{PREFIX}Ontology(\n{body}\n)\n");
    ofn_read::parse(&text).unwrap_or_else(|e| panic!("failed to parse OFN:\n{text}\nerror: {e}"))
}

fn parse_axioms(body: &str) -> Vec<Axiom> {
    parse_ofn(body).axioms.into_iter().collect()
}

fn limits_pair() -> [(&'static str, Limits); 2] {
    [
        ("rooted=false", Limits::default()),
        (
            "rooted=true",
            Limits {
                rooted: true,
                ..Limits::default()
            },
        ),
    ]
}

fn classify_ofn(
    body: &str,
    limits: &Limits,
) -> (Classifier, sparkles_owl::classify::Classification) {
    let ax = parse_axioms(body);
    let mut k = Classifier::of(&ax);
    let out = k.classify(limits).unwrap();
    (k, out)
}

fn fresh_bnodes() -> impl FnMut() -> BlankNode {
    let mut n = 0usize;
    move || {
        n += 1;
        BlankNode::new_unchecked(format!("b{n}"))
    }
}

#[test]
fn ofn_disjoint_classes_repeated_pair_forces_unsatisfiable() {
    for (label, limits) in limits_pair() {
        let (_, out) = classify_ofn("Declaration(Class(:A))\nDisjointClasses(:A :A)", &limits);
        assert_eq!(out.consistency, Consistency::Consistent, "{label}");
        assert!(out.taxonomy.is_unsatisfiable(&c("A")), "{label}");
    }
}

#[test]
fn ofn_disjoint_classes_repeated_pair_with_third_member() {
    for (label, limits) in limits_pair() {
        let (_, out) = classify_ofn(
            "Declaration(Class(:A))\nDeclaration(Class(:B))\nDisjointClasses(:A :A :B)",
            &limits,
        );
        assert_eq!(out.consistency, Consistency::Consistent, "{label}");
        assert!(out.taxonomy.is_unsatisfiable(&c("A")), "{label}");
        assert!(!out.taxonomy.is_unsatisfiable(&c("B")), "{label}");
    }
}

#[test]
fn ofn_disjoint_union_repeated_operands_force_both_empty() {
    for (label, limits) in limits_pair() {
        let (_, out) = classify_ofn(
            "Declaration(Class(:C))\nDeclaration(Class(:A))\nDisjointUnion(:C :A :A)",
            &limits,
        );
        assert_eq!(out.consistency, Consistency::Consistent, "{label}");
        assert!(out.taxonomy.is_unsatisfiable(&c("A")), "{label}");
        assert!(out.taxonomy.is_unsatisfiable(&c("C")), "{label}");
    }
}

#[test]
fn ofn_disjoint_union_repeated_pair_with_third_member() {
    for (label, limits) in limits_pair() {
        let (_, out) = classify_ofn(
            "Declaration(Class(:C))\nDeclaration(Class(:A))\nDeclaration(Class(:B))\n\
             DisjointUnion(:C :A :A :B)",
            &limits,
        );
        assert_eq!(out.consistency, Consistency::Consistent, "{label}");
        assert!(out.taxonomy.is_unsatisfiable(&c("A")), "{label}");
        assert!(!out.taxonomy.is_unsatisfiable(&c("B")), "{label}");
        assert!(!out.taxonomy.is_unsatisfiable(&c("C")), "{label}");
        assert!(out.taxonomy.is_subclass(&c("C"), &c("B")), "{label}");
        assert!(out.taxonomy.is_subclass(&c("B"), &c("C")), "{label}");
    }
}

#[test]
fn named_instances_make_repeated_disjoint_class_inconsistent() {
    for (label, limits) in limits_pair() {
        let (_, out) = classify_ofn("DisjointClasses(:A :A)\nClassAssertion(:A :a)", &limits);
        assert_eq!(out.consistency, Consistency::Inconsistent, "pair {label}");

        let (_, out) = classify_ofn("DisjointClasses(:A :A :B)\nClassAssertion(:A :a)", &limits);
        assert_eq!(out.consistency, Consistency::Inconsistent, "triple {label}");

        let (_, out) = classify_ofn("DisjointClasses(:A :A :B)\nClassAssertion(:B :b)", &limits);
        assert_eq!(out.consistency, Consistency::Consistent, "third {label}");
        assert!(out.taxonomy.is_unsatisfiable(&c("A")), "third {label}");

        let (_, out) = classify_ofn("DisjointUnion(:C :A :A)\nClassAssertion(:A :a)", &limits);
        assert_eq!(
            out.consistency,
            Consistency::Inconsistent,
            "union-A {label}"
        );

        let (_, out) = classify_ofn("DisjointUnion(:C :A :A)\nClassAssertion(:C :a)", &limits);
        assert_eq!(
            out.consistency,
            Consistency::Inconsistent,
            "union-C {label}"
        );
    }
}

#[test]
fn public_constructor_disjoint_classes_preserves_duplicates() {
    let pair = Axiom::disjoint_classes([ce("A"), ce("A")]);
    assert!(validate::axiom(&pair).is_ok(), "{pair:?}");
    let triple_a = Axiom::disjoint_classes([ce("B"), ce("A"), ce("A")]);
    let triple_b = Axiom::disjoint_classes([ce("A"), ce("B"), ce("A")]);
    assert_eq!(triple_a, triple_b);

    let du = Axiom::disjoint_union(c("C"), [ce("A"), ce("A")]);
    assert!(validate::axiom(&du).is_ok(), "{du:?}");
    let dop = Axiom::disjoint_object_properties([op("p"), op("p")]);
    assert!(validate::axiom(&dop).is_ok(), "{dop:?}");
    let ddp = Axiom::disjoint_data_properties([dp("d"), dp("d")]);
    assert!(validate::axiom(&ddp).is_ok(), "{ddp:?}");
    let diff = Axiom::different_individuals([ind("a"), ind("a")]);
    assert!(validate::axiom(&diff).is_ok(), "{diff:?}");

    // Single-operand disjointness axioms are still rejected by validation.
    for bad in [
        Axiom::disjoint_classes([ce("A")]),
        Axiom::disjoint_union(c("C"), [ce("A")]),
        Axiom::disjoint_object_properties([op("p")]),
        Axiom::disjoint_data_properties([dp("d")]),
        Axiom::different_individuals([ind("a")]),
    ] {
        assert!(validate::axiom(&bad).is_err(), "should fail: {bad:?}");
    }

    for (label, limits) in limits_pair() {
        let ax = vec![Axiom::declaration(Entity::Class(c("A"))), pair.clone()];
        let mut k = Classifier::of(&ax);
        let out = k.classify(&limits).unwrap();
        assert_eq!(out.consistency, Consistency::Consistent, "{label}");
        assert!(out.taxonomy.is_unsatisfiable(&c("A")), "{label}");

        let ax = vec![pair.clone(), Axiom::class_assertion(ce("A"), ind("a"))];
        let mut k = Classifier::of(&ax);
        let out = k.classify(&limits).unwrap();
        assert_eq!(out.consistency, Consistency::Inconsistent, "{label}");

        let ax = vec![
            Axiom::declaration(Entity::Class(c("C"))),
            Axiom::declaration(Entity::Class(c("A"))),
            du.clone(),
        ];
        let mut k = Classifier::of(&ax);
        let out = k.classify(&limits).unwrap();
        assert_eq!(out.consistency, Consistency::Consistent, "du {label}");
        assert!(out.taxonomy.is_unsatisfiable(&c("A")), "du-A {label}");
        assert!(out.taxonomy.is_unsatisfiable(&c("C")), "du-C {label}");
    }
}

#[test]
fn disjoint_thing_with_thing_and_rooted_top_property_duplicates() {
    for (label, limits) in limits_pair() {
        let (_, out) = classify_ofn("DisjointClasses(owl:Thing owl:Thing)", &limits);
        assert_eq!(out.consistency, Consistency::Inconsistent, "thing {label}");
    }

    // Rooted rewriting of literal duplicates over owl:topObjectProperty.
    let rooted = Limits {
        rooted: true,
        ..Limits::default()
    };
    let (_, out) = classify_ofn(
        "Declaration(Class(:A))\n\
         Declaration(Class(:B))\n\
         SubClassOf(:B ObjectUnionOf(:A :D))\n\
         DisjointClasses(ObjectSomeValuesFrom(owl:topObjectProperty :A) \
                         ObjectSomeValuesFrom(owl:topObjectProperty :A))\n\
         ClassAssertion(:B :b)",
        &rooted,
    );
    assert_eq!(out.consistency, Consistency::Consistent);
    assert!(out.taxonomy.is_unsatisfiable(&c("A")));

    let (_, out) = classify_ofn(
        "Declaration(Class(:A))\n\
         Declaration(Class(:B))\n\
         SubClassOf(:B ObjectUnionOf(:A :D))\n\
         DisjointClasses(ObjectSomeValuesFrom(owl:topObjectProperty :A) \
                         ObjectSomeValuesFrom(owl:topObjectProperty :A))\n\
         ClassAssertion(:A :a)",
        &rooted,
    );
    assert_eq!(out.consistency, Consistency::Inconsistent);
}

#[test]
fn repeated_disjoint_properties_and_different_individuals_force_emptiness_and_clash() {
    for (label, limits) in limits_pair() {
        let (_, out) = classify_ofn(
            "Declaration(Class(:A))\n\
             DisjointObjectProperties(:p :p)\n\
             SubClassOf(:A ObjectSomeValuesFrom(:p owl:Thing))",
            &limits,
        );
        assert_eq!(out.consistency, Consistency::Consistent, "objprop {label}");
        assert!(out.taxonomy.is_unsatisfiable(&c("A")), "objprop {label}");

        let (_, out) = classify_ofn(
            "DisjointObjectProperties(:p :p)\n\
             ObjectPropertyAssertion(:p :a :b)",
            &limits,
        );
        assert_eq!(
            out.consistency,
            Consistency::Inconsistent,
            "objprop-inst {label}"
        );

        let (_, out) = classify_ofn(
            "DisjointDataProperties(:d :d)\n\
             DataPropertyAssertion(:d :a \"1\"^^xsd:integer)",
            &limits,
        );
        assert_eq!(
            out.consistency,
            Consistency::Inconsistent,
            "dataprop-inst {label}"
        );

        let (_, out) = classify_ofn("DifferentIndividuals(:a :a)", &limits);
        assert_eq!(
            out.consistency,
            Consistency::Inconsistent,
            "diffind {label}"
        );

        let (_, out) = classify_ofn("DifferentIndividuals(:a :a :b)", &limits);
        assert_eq!(
            out.consistency,
            Consistency::Inconsistent,
            "diffind3 {label}"
        );
    }
}

#[test]
fn ofn_rdf_json_and_manchester_roundtrip_preserve_duplicates_and_annotations() {
    let ann = Annotation::comment("keep-dup");
    let ofn_doc = parse_ofn(
        "Declaration(Class(:A))\n\
         Declaration(Class(:B))\n\
         Declaration(Class(:C))\n\
         Declaration(ObjectProperty(:p))\n\
         Declaration(ObjectProperty(:q))\n\
         Declaration(DataProperty(:d))\n\
         Declaration(DataProperty(:e))\n\
         Declaration(NamedIndividual(:a))\n\
         Declaration(NamedIndividual(:b))\n\
         DisjointClasses(Annotation(rdfs:comment \"keep-dup\") :A :A)\n\
         DisjointClasses(Annotation(rdfs:comment \"keep-dup\") :A :A :B)\n\
         DisjointClasses(:A :A :A)\n\
         DisjointClasses(ObjectSomeValuesFrom(:p :A) ObjectSomeValuesFrom(:p :A))\n\
         DisjointUnion(Annotation(rdfs:comment \"keep-dup\") :C :A :A)\n\
         DisjointObjectProperties(Annotation(rdfs:comment \"keep-dup\") :p :p)\n\
         DisjointObjectProperties(:p :p :q)\n\
         DisjointDataProperties(Annotation(rdfs:comment \"keep-dup\") :d :d)\n\
         DisjointDataProperties(:d :d :e)\n\
         DifferentIndividuals(Annotation(rdfs:comment \"keep-dup\") :a :a)\n\
         DifferentIndividuals(:a :a :b)",
    );

    // OFN -> OFN -> OFN
    let rendered = ofn::document(
        &ofn::Renderer::new(ofn_doc.prefixes.iter().cloned()),
        &ofn_doc.header,
        ofn_doc.axioms.iter(),
    );
    let reparsed = ofn_read::parse(&rendered).unwrap();
    assert_eq!(reparsed.header, ofn_doc.header);
    assert_eq!(reparsed.axioms, ofn_doc.axioms);

    // OFN -> RDF -> Ontology (strict)
    let mut fresh = fresh_bnodes();
    let triples = write::ontology_triples(&ofn_doc.header, ofn_doc.axioms.iter(), &mut fresh);
    let onto = read::read_graph(
        triples,
        read::ReadOptions {
            mode: read::Mode::Strict,
            ..Default::default()
        },
    );
    assert!(onto.unmapped.is_empty(), "unmapped: {:?}", onto.unmapped);
    assert_eq!(onto.error_count, 0, "errors: {:?}", onto.diagnostics);
    for ax in &ofn_doc.axioms {
        assert!(onto.contains(ax), "missing after RDF roundtrip: {ax:?}");
    }

    // JSON round-trip
    for ax in &ofn_doc.axioms {
        let v = json::axiom(ax);
        let back = json::read_axiom(&v).unwrap();
        assert_eq!(&back, ax, "JSON roundtrip mismatch for {ax:?}");
    }

    // Manchester binary disjointness / inequality round-trip
    let mut vocab = manchester::Vocabulary::new([("", "http://ex.org/")]);
    for e in [
        Entity::Class(c("A")),
        Entity::ObjectProperty(ObjectProperty::new(ex("p"))),
        Entity::DataProperty(dp("d")),
        Entity::NamedIndividual(sparkles_owl::NamedIndividual::new(ex("a"))),
    ] {
        vocab.add_entity(e);
    }
    let m_renderer = manchester::Renderer::of_vocabulary(&vocab);
    for (text, expected) in [
        (
            "A DisjointWith A",
            Axiom::disjoint_classes([ce("A"), ce("A")]),
        ),
        (
            "p DisjointWith p",
            Axiom::new(AxiomKind::DisjointObjectProperties(
                [op("p"), op("p")].into(),
            )),
        ),
        (
            "d DisjointWith d",
            Axiom::new(AxiomKind::DisjointDataProperties([dp("d"), dp("d")].into())),
        ),
        (
            "a DifferentFrom a",
            Axiom::new(AxiomKind::DifferentIndividuals([ind("a"), ind("a")].into())),
        ),
    ] {
        let parsed = manchester::axiom(text, &vocab).unwrap();
        assert_eq!(parsed, expected, "manchester parse {text}");
        let back_text = m_renderer.axiom(&parsed).unwrap();
        let reparsed = manchester::axiom(&back_text, &vocab).unwrap();
        assert_eq!(reparsed, expected, "manchester roundtrip {back_text}");
    }

    // Verify annotation survived on the repeated binary disjointness
    let annotated_pair = Axiom::disjoint_classes([ce("A"), ce("A")]).annotated(ann);
    assert!(onto.contains(&annotated_pair));

    // Entity accessors surface self-disjointness and keep DisjointUnion multiplicity
    assert!(onto.disjoint_classes(ce("A")).contains(&ce("A")));
    assert!(onto.disjoint_classes(ce("A")).contains(&ce("B")));
    assert_eq!(
        onto.disjoint_unions(&c("C")),
        vec![[ce("A"), ce("A")].into()]
    );
    assert!(onto.disjoint_object_properties(op("p")).contains(&op("p")));
    assert!(onto.disjoint_data_properties(&dp("d")).contains(&dp("d")));
    assert!(onto.different_individuals(&ind("a")).contains(&ind("a")));
}

#[test]
fn original_axiom_diagnostics_and_counts_preserve_repeated_operands() {
    // Under el_only, DisjointUnion(:C :A :A) is excluded and reported verbatim with both :A operands.
    let ax = parse_axioms("DisjointUnion(:C :A :A)");
    let orig = ax[0].kind.clone();
    let mut k = Classifier::of(&ax);
    let out = k
        .classify(&Limits {
            el_only: true,
            ..Limits::default()
        })
        .unwrap();
    assert_eq!(out.stats.outside_el.taken, 0);
    let left = k.excluded();
    assert_eq!(left.len(), 1);
    assert_eq!(left[0].axiom, orig);
}

#[test]
fn equivalences_unions_and_intersections_still_deduplicate() {
    let doc = parse_ofn(
        "EquivalentClasses(:A :A)\n\
         EquivalentClasses(:A :A :B)\n\
         SubClassOf(:C ObjectIntersectionOf(:A :A :B))\n\
         SubClassOf(:D ObjectUnionOf(:A :A :B))\n\
         SameIndividual(:a :a)\n\
         EquivalentObjectProperties(:p :p)\n\
         EquivalentDataProperties(:d :d)",
    );
    assert!(doc.axioms.contains(&Axiom::equivalent_classes([ce("A")])));
    assert!(
        doc.axioms
            .contains(&Axiom::equivalent_classes([ce("A"), ce("B")]))
    );
    assert!(doc.axioms.contains(&Axiom::sub_class_of(
        ce("C"),
        ClassExpression::ObjectIntersectionOf([ce("A"), ce("B")].into()),
    )));
    assert!(doc.axioms.contains(&Axiom::sub_class_of(
        ce("D"),
        ClassExpression::ObjectUnionOf([ce("A"), ce("B")].into()),
    )));
    assert!(
        doc.axioms
            .contains(&Axiom::new(AxiomKind::SameIndividual([ind("a")].into())))
    );
    assert!(
        doc.axioms
            .contains(&Axiom::new(AxiomKind::EquivalentObjectProperties(
                [op("p")].into()
            )))
    );
    assert!(
        doc.axioms
            .contains(&Axiom::new(AxiomKind::EquivalentDataProperties(
                [dp("d")].into()
            )))
    );
}
