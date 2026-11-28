use oxrdf::{BlankNode, NamedNode, NamedOrBlankNode, Term, Triple};
use sparkles_owl::classify::{Classifier, Consistency, Limits, Tier};
use sparkles_owl::{
    Annotation, AnonymousIndividual, Axiom, AxiomKind, Class, ClassExpression, DataProperty,
    Entity, Individual, Multiset, ObjectProperty, ObjectPropertyExpression, json, manchester, ofn,
    ofn_read, read, validate, vocab, write,
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
    let AxiomKind::DisjointUnion(_, ref orig_ds) = orig else {
        panic!("expected DisjointUnion");
    };
    assert_eq!(orig_ds.len(), 2);
    let mut k = Classifier::of(&ax);
    let out = k
        .classify(&Limits {
            el_only: true,
            ..Limits::default()
        })
        .unwrap();
    assert_eq!(out.stats.outside_el.horn, 1);
    assert_eq!(out.stats.outside_el.disjunctive, 0);
    assert_eq!(out.stats.outside_el.taken, 0);
    let left = k.excluded();
    assert_eq!(left.len(), 1);
    assert_eq!(left[0].axiom, orig);
    let AxiomKind::DisjointUnion(_, ref left_ds) = left[0].axiom else {
        panic!("expected DisjointUnion in excluded");
    };
    assert_eq!(left_ds.len(), 2);

    // Rooted dl_reduce diagnostic restoration: a repeated-operand axiom over
    // owl:topObjectProperty whose rewritten form is excluded (counting=false refuses
    // ObjectMaxCardinality(2 ...)) must be restored with all original repeated operands.
    // The complemented member makes the axiom need case analysis, so that the rooted run
    // keeps it rather than giving the axioms to the Horn rules (roadmap item 1286).
    let rooted_ax = parse_axioms(
        "SubClassOf(:B ObjectUnionOf(:C :D))\n\
         ClassAssertion(:B :b)\n\
         DisjointClasses(ObjectSomeValuesFrom(owl:topObjectProperty :A) \
                         ObjectSomeValuesFrom(owl:topObjectProperty :A) \
                         ObjectMaxCardinality(2 :r :E) \
                         ObjectComplementOf(:F))",
    );
    let expected_orig = rooted_ax
        .iter()
        .find(|a| matches!(a.kind, AxiomKind::DisjointClasses(_)))
        .unwrap()
        .kind
        .clone();
    let AxiomKind::DisjointClasses(ref expected_cs) = expected_orig else {
        panic!("expected DisjointClasses");
    };
    assert_eq!(expected_cs.len(), 4);
    let mut k = Classifier::of(&rooted_ax);
    let out = k
        .classify(&Limits {
            rooted: true,
            counting: false,
            ..Limits::default()
        })
        .unwrap();
    assert!(!out.complete);
    // the rooted run took the union, which the Horn rules leave out, and reports only the
    // restored original axiom
    assert_eq!(out.stats.tier, Tier::Disjunctive);
    assert!(out.stats.clauses > 0, "the rooted run took no clause");
    let left = k.excluded();
    assert_eq!(left.len(), 1, "{left:?}");
    assert!(
        left[0].axiom == expected_orig,
        "expected restored original repeated axiom {expected_orig:?}, got {left:?}"
    );
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
         EquivalentDataProperties(:d :d)\n\
         HasKey(:C (:p :p) (:d :d))",
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
    assert!(doc.axioms.contains(&Axiom::new(AxiomKind::HasKey(
        ce("C"),
        [op("p")].into(),
        [dp("d")].into(),
    ))));
}

#[test]
fn repeated_disjoint_union_stays_horn_in_strict_and_rooted_runs() {
    // Sol #1: DisjointUnion(:C :A :A) with Horn property disjointness must stay in Horn
    // and succeed in strict mode (no false Tier D escalation when counting is off).
    let mixed_horn = "Declaration(Class(:A))\n\
        Declaration(Class(:C))\n\
        Declaration(Class(:D))\n\
        DisjointUnion(:C :A :A)\n\
        DisjointObjectProperties(:p :q)\n\
        SubObjectPropertyOf(:q :p)\n\
        SubClassOf(:D ObjectSomeValuesFrom(:q owl:Thing))";
    for (label, base) in limits_pair() {
        let strict = Limits {
            strict: true,
            ..base
        };
        let (_, out) = classify_ofn(mixed_horn, &strict);
        assert!(out.complete, "{label}");
        assert_eq!(out.stats.tier, Tier::Horn, "{label}");
        assert_eq!(out.stats.outside_el.disjunctive, 0, "{label}");
        assert_eq!(out.consistency, Consistency::Consistent, "{label}");
        assert!(out.taxonomy.is_unsatisfiable(&c("A")), "{label}");
        assert!(out.taxonomy.is_unsatisfiable(&c("C")), "{label}");
        assert!(out.taxonomy.is_unsatisfiable(&c("D")), "{label}");
    }

    // Neon #1: DisjointUnion(:C :A :A) + IrreflexiveObjectProperty + self-assertion
    // must remain complete and inconsistent under rooted=true as well as rooted=false.
    let irrefl_horn = "DisjointUnion(:C :A :A)\n\
        IrreflexiveObjectProperty(:r)\n\
        ObjectPropertyAssertion(:r :a :a)";
    for (label, base) in limits_pair() {
        let strict = Limits {
            strict: true,
            ..base
        };
        let (_, out) = classify_ofn(irrefl_horn, &strict);
        assert!(out.complete, "irrefl {label}");
        assert_eq!(out.stats.tier, Tier::Horn, "irrefl {label}");
        assert_eq!(out.stats.outside_el.disjunctive, 0, "irrefl {label}");
        assert_eq!(out.consistency, Consistency::Inconsistent, "irrefl {label}");
    }

    // DisjointUnion(:C :A :A) alone reports Tier::Horn and outside_el.disjunctive == 0.
    let (_, out) = classify_ofn("DisjointUnion(:C :A :A)", &Limits::default());
    assert_eq!(out.stats.tier, Tier::Horn);
    assert_eq!(out.stats.outside_el.horn, 1);
    assert_eq!(out.stats.outside_el.disjunctive, 0);
    assert_eq!(out.stats.outside_el.taken, 1);
}

#[test]
fn complemented_disjoint_classes_stays_horn_in_strict_and_rooted_runs() {
    let body = "DisjointClasses(ObjectComplementOf(:A) ObjectComplementOf(:A))\n\
        IrreflexiveObjectProperty(:r)\n\
        ObjectPropertyAssertion(:r :a :a)";
    for (label, base) in limits_pair() {
        let strict = Limits {
            strict: true,
            ..base
        };
        let (_, out) = classify_ofn(body, &strict);
        assert!(out.complete, "{label}");
        assert_eq!(out.stats.tier, Tier::Horn, "{label}");
        assert_eq!(out.stats.outside_el.disjunctive, 0, "{label}");
        assert_eq!(out.consistency, Consistency::Inconsistent, "{label}");
    }

    // DisjointClasses(¬A, ¬A, ¬B) reduces to ¬A ⊑ ⊥ (i.e. ⊤ ⊑ A), which is Horn.
    let triple_comp = "Declaration(Class(:A))\n\
        Declaration(Class(:B))\n\
        Declaration(Class(:X))\n\
        DisjointClasses(ObjectComplementOf(:A) ObjectComplementOf(:A) ObjectComplementOf(:B))\n\
        SubClassOf(:X ObjectComplementOf(:A))";
    for (label, base) in limits_pair() {
        let strict = Limits {
            strict: true,
            ..base
        };
        let (_, out) = classify_ofn(triple_comp, &strict);
        assert!(out.complete, "triple {label}");
        assert_eq!(out.stats.tier, Tier::Horn, "triple {label}");
        assert_eq!(out.stats.outside_el.disjunctive, 0, "triple {label}");
        assert!(out.taxonomy.is_unsatisfiable(&c("X")), "triple {label}");
    }
}

#[test]
fn single_surviving_disjoint_class_with_non_horn_neg_stays_horn() {
    let single = "Declaration(Class(:A))\n\
        Declaration(Class(:B))\n\
        Declaration(Class(:C))\n\
        DisjointClasses(:A :A ObjectIntersectionOf(ObjectComplementOf(:B) ObjectComplementOf(:C)))";
    let (_, out) = classify_ofn(single, &Limits::default());
    assert!(out.complete);
    assert_eq!(out.stats.tier, Tier::Horn);
    assert_eq!(out.stats.outside_el.horn, 1);
    assert_eq!(out.stats.outside_el.disjunctive, 0);
    assert_eq!(out.stats.outside_el.taken, 1);
    assert_eq!(out.consistency, Consistency::Consistent);
    assert!(out.taxonomy.is_unsatisfiable(&c("A")));
    assert!(!out.taxonomy.is_unsatisfiable(&c("B")));
    assert!(!out.taxonomy.is_unsatisfiable(&c("C")));

    let mixed_horn = "Declaration(Class(:A))\n\
        Declaration(Class(:B))\n\
        Declaration(Class(:C))\n\
        Declaration(Class(:D))\n\
        DisjointClasses(:A :A ObjectIntersectionOf(ObjectComplementOf(:B) ObjectComplementOf(:C)))\n\
        DisjointObjectProperties(:p :q)\n\
        SubObjectPropertyOf(:q :p)\n\
        SubClassOf(:D ObjectSomeValuesFrom(:q owl:Thing))";
    for (label, base) in limits_pair() {
        let strict = Limits {
            strict: true,
            ..base
        };
        let (_, out) = classify_ofn(mixed_horn, &strict);
        assert!(out.complete, "mixed {label}");
        assert_eq!(out.stats.tier, Tier::Horn, "mixed {label}");
        assert_eq!(out.stats.outside_el.disjunctive, 0, "mixed {label}");
        assert_eq!(out.consistency, Consistency::Consistent, "mixed {label}");
        assert!(out.taxonomy.is_unsatisfiable(&c("A")), "mixed {label}");
        assert!(out.taxonomy.is_unsatisfiable(&c("D")), "mixed {label}");
    }

    let irrefl_horn = "DisjointClasses(:A :A ObjectIntersectionOf(ObjectComplementOf(:B) ObjectComplementOf(:C)))\n\
        IrreflexiveObjectProperty(:r)\n\
        ObjectPropertyAssertion(:r :a :a)";
    for (label, base) in limits_pair() {
        let strict = Limits {
            strict: true,
            ..base
        };
        let (_, out) = classify_ofn(irrefl_horn, &strict);
        assert!(out.complete, "irrefl {label}");
        assert_eq!(out.stats.tier, Tier::Horn, "irrefl {label}");
        assert_eq!(out.stats.outside_el.disjunctive, 0, "irrefl {label}");
        assert_eq!(out.consistency, Consistency::Inconsistent, "irrefl {label}");
    }
}

#[test]
fn mixed_repeated_disjoint_union_with_individual_is_horn_and_inconsistent() {
    // Sol #1 second case: DisjointUnion(:C :A :A :B) with ClassAssertion(:A :a) must
    // normalize in Horn under rooted=false and rooted=true (reducing to :A ⊑ ⊥, :C ≡ :B).
    let incons = "DisjointUnion(:C :A :A :B)\nClassAssertion(:A :a)";
    for (label, base) in limits_pair() {
        let strict = Limits {
            strict: true,
            ..base
        };
        let (_, out) = classify_ofn(incons, &strict);
        assert!(out.complete, "incons {label}");
        assert_eq!(out.stats.tier, Tier::Horn, "incons {label}");
        assert_eq!(out.stats.outside_el.disjunctive, 0, "incons {label}");
        assert_eq!(out.consistency, Consistency::Inconsistent, "incons {label}");
    }

    let cons = "Declaration(Class(:A))\n\
        Declaration(Class(:B))\n\
        Declaration(Class(:C))\n\
        DisjointUnion(:C :A :A :B)\n\
        ClassAssertion(:B :b)";
    for (label, base) in limits_pair() {
        let strict = Limits {
            strict: true,
            ..base
        };
        let (_, out) = classify_ofn(cons, &strict);
        assert!(out.complete, "cons {label}");
        assert_eq!(out.stats.tier, Tier::Horn, "cons {label}");
        assert_eq!(out.consistency, Consistency::Consistent, "cons {label}");
        assert!(out.taxonomy.is_unsatisfiable(&c("A")), "cons {label}");
        assert!(out.taxonomy.is_subclass(&c("C"), &c("B")), "cons {label}");
        assert!(out.taxonomy.is_subclass(&c("B"), &c("C")), "cons {label}");
    }
}

#[test]
fn tier_d_and_rooted_repeated_properties_individuals_and_top_disjoint_union() {
    // Tier D (unrooted, counting=true): DisjointObjectProperties(:p :p) beside a genuine
    // disjunctive axiom forces :A unsatisfiable through dl_norm + dl_rules.
    let counting = Limits {
        counting: true,
        strict: true,
        ..Limits::default()
    };
    let (_, out) = classify_ofn(
        "Declaration(Class(:A))\n\
         Declaration(Class(:B))\n\
         Declaration(Class(:C))\n\
         Declaration(Class(:D))\n\
         SubClassOf(:B ObjectUnionOf(:C :D))\n\
         DisjointObjectProperties(:p :p)\n\
         SubClassOf(:A ObjectSomeValuesFrom(:p owl:Thing))",
        &counting,
    );
    assert!(out.complete);
    assert_eq!(out.stats.tier, Tier::Disjunctive);
    assert_eq!(out.consistency, Consistency::Consistent);
    assert!(out.taxonomy.is_unsatisfiable(&c("A")));

    // Rooted Tier D (rooted=true, counting=true): DisjointObjectProperties(:p :p) with
    // ObjectPropertyAssertion(:p :a :b) beside a disjunctive axiom derives inconsistency.
    let rooted_counting = Limits {
        rooted: true,
        counting: true,
        strict: true,
        ..Limits::default()
    };
    let (_, out) = classify_ofn(
        "SubClassOf(:B ObjectUnionOf(:C :D))\n\
         DisjointObjectProperties(:p :p)\n\
         ObjectPropertyAssertion(:p :a :b)",
        &rooted_counting,
    );
    assert!(out.complete);
    assert_eq!(out.stats.tier, Tier::Disjunctive);
    assert_eq!(out.consistency, Consistency::Inconsistent);

    // Rooted Tier D: DifferentIndividuals(:a :a) beside a disjunctive axiom derives inconsistency.
    let rooted_strict = Limits {
        rooted: true,
        strict: true,
        ..Limits::default()
    };
    let (_, out) = classify_ofn(
        "SubClassOf(:B ObjectUnionOf(:C :D))\n\
         ClassAssertion(:B :b)\n\
         DifferentIndividuals(:a :a)",
        &rooted_strict,
    );
    assert!(out.complete);
    assert_eq!(out.stats.tier, Tier::Disjunctive);
    assert_eq!(out.consistency, Consistency::Inconsistent);

    // Rooted dl_reduce on DisjointUnion with duplicate owl:topObjectProperty operands.
    let (_, out) = classify_ofn(
        "Declaration(Class(:A))\n\
         Declaration(Class(:B))\n\
         Declaration(Class(:C))\n\
         SubClassOf(:B ObjectUnionOf(:D :E))\n\
         DisjointUnion(:C ObjectSomeValuesFrom(owl:topObjectProperty :A) \
                          ObjectSomeValuesFrom(owl:topObjectProperty :A))\n\
         ClassAssertion(:B :b)",
        &rooted_strict,
    );
    assert!(out.complete);
    assert_eq!(out.stats.tier, Tier::Disjunctive);
    assert_eq!(out.consistency, Consistency::Consistent);
    assert!(out.taxonomy.is_unsatisfiable(&c("A")));
    assert!(out.taxonomy.is_unsatisfiable(&c("C")));

    let (_, out) = classify_ofn(
        "SubClassOf(:B ObjectUnionOf(:D :E))\n\
         DisjointUnion(:C ObjectSomeValuesFrom(owl:topObjectProperty :A) \
                          ObjectSomeValuesFrom(owl:topObjectProperty :A))\n\
         ClassAssertion(:C :a)",
        &rooted_strict,
    );
    assert!(out.complete);
    assert_eq!(out.consistency, Consistency::Inconsistent);
}

#[test]
fn dtcheck_repeated_data_property_handles_tbox_and_subproperty() {
    for (label, base) in limits_pair() {
        let strict = Limits {
            strict: true,
            ..base
        };
        let (_, out) = classify_ofn(
            "Declaration(Class(:A))\n\
             DisjointDataProperties(:d :d)\n\
             SubClassOf(:A DataSomeValuesFrom(:d xsd:integer))",
            &strict,
        );
        assert!(out.complete, "tbox {label}");
        assert_eq!(out.consistency, Consistency::Consistent, "tbox {label}");
        assert!(out.taxonomy.is_unsatisfiable(&c("A")), "tbox {label}");

        let (_, out) = classify_ofn(
            "Declaration(Class(:A))\n\
             DisjointDataProperties(:d :d)\n\
             SubDataPropertyOf(:dsub :d)\n\
             SubClassOf(:A DataSomeValuesFrom(:dsub xsd:integer))",
            &strict,
        );
        assert!(out.complete, "subprop-tbox {label}");
        assert_eq!(
            out.consistency,
            Consistency::Consistent,
            "subprop-tbox {label}"
        );
        assert!(
            out.taxonomy.is_unsatisfiable(&c("A")),
            "subprop-tbox {label}"
        );

        let (_, out) = classify_ofn(
            "DisjointDataProperties(:d :d)\n\
             SubDataPropertyOf(:dsub :d)\n\
             DataPropertyAssertion(:dsub :a \"1\"^^xsd:integer)",
            &strict,
        );
        assert!(out.complete, "subprop-abox {label}");
        assert_eq!(
            out.consistency,
            Consistency::Inconsistent,
            "subprop-abox {label}"
        );
    }
}

#[test]
fn large_repeated_property_lists_classify_within_tight_memory_budget() {
    let p = op("p");
    let d = dp("d");
    let obj_ax = vec![
        Axiom::declaration(Entity::Class(c("A"))),
        Axiom::new(AxiomKind::DisjointObjectProperties(
            vec![p.clone(); 2000].into(),
        )),
        Axiom::sub_class_of(
            ce("A"),
            ClassExpression::ObjectSomeValuesFrom(
                p,
                Box::new(ClassExpression::Class(Class::thing())),
            ),
        ),
    ];
    let limits = Limits {
        max_memory: Some(16 * 1024 * 1024),
        strict: true,
        ..Limits::default()
    };
    let mut k = Classifier::of(&obj_ax);
    let out = k.classify(&limits).unwrap();
    assert!(out.complete);
    assert!(out.taxonomy.is_unsatisfiable(&c("A")));

    let data_ax = vec![
        Axiom::declaration(Entity::Class(c("A"))),
        Axiom::new(AxiomKind::DisjointDataProperties(
            vec![d.clone(); 2000].into(),
        )),
        Axiom::sub_class_of(
            ce("A"),
            ClassExpression::DataSomeValuesFrom(
                vec![d],
                sparkles_owl::DataRange::Datatype(sparkles_owl::Datatype::new(vocab::XSD_INTEGER)),
            ),
        ),
    ];
    let mut k = Classifier::of(&data_ax);
    let out = k.classify(&limits).unwrap();
    assert!(out.complete);
    assert!(out.taxonomy.is_unsatisfiable(&c("A")));
}

#[test]
fn rdf_shared_expression_bnode_and_repeated_anonymous_individual() {
    let nn = |iri: &str| NamedNode::new_unchecked(iri);
    let rdf_type = nn(vocab::RDF_TYPE);
    let rdf_first = nn(vocab::RDF_FIRST);
    let rdf_rest = nn(vocab::RDF_REST);
    let rdf_nil = nn(vocab::RDF_NIL);

    // 1. AllDisjointClasses list referencing the same Restriction blank node twice.
    let expr = BlankNode::new_unchecked("expr");
    let all_disj = BlankNode::new_unchecked("ad");
    let l1 = BlankNode::new_unchecked("l1");
    let l2 = BlankNode::new_unchecked("l2");
    let p_nn = nn(&ex("p"));
    let a_nn = nn(&ex("A"));
    let triples = vec![
        Triple::new(a_nn.clone(), rdf_type.clone(), nn(vocab::OWL_CLASS)),
        Triple::new(
            p_nn.clone(),
            rdf_type.clone(),
            nn(vocab::OWL_OBJECT_PROPERTY),
        ),
        Triple::new(expr.clone(), rdf_type.clone(), nn(vocab::OWL_RESTRICTION)),
        Triple::new(expr.clone(), nn(vocab::OWL_ON_PROPERTY), p_nn.clone()),
        Triple::new(expr.clone(), nn(vocab::OWL_SOME_VALUES_FROM), a_nn.clone()),
        Triple::new(
            all_disj.clone(),
            rdf_type.clone(),
            nn(vocab::OWL_ALL_DISJOINT_CLASSES),
        ),
        Triple::new(all_disj, nn(vocab::OWL_MEMBERS), l1.clone()),
        Triple::new(l1.clone(), rdf_first.clone(), Term::BlankNode(expr.clone())),
        Triple::new(l1, rdf_rest.clone(), l2.clone()),
        Triple::new(l2.clone(), rdf_first.clone(), Term::BlankNode(expr)),
        Triple::new(l2, rdf_rest.clone(), rdf_nil.clone()),
    ];
    let expected_ce = ClassExpression::ObjectSomeValuesFrom(op("p"), Box::new(ce("A")));
    let expected_ax = Axiom::disjoint_classes([expected_ce.clone(), expected_ce]);
    let onto = read::read_graph(
        triples,
        read::ReadOptions {
            mode: read::Mode::Lenient,
            ..Default::default()
        },
    );
    assert!(
        onto.contains(&expected_ax),
        "expected shared bnode restriction duplicate in {:#?}",
        onto.axioms.keys().collect::<Vec<_>>()
    );

    // 2. Repeated anonymous individual in owl:differentFrom and AllDifferent.
    let anon = BlankNode::new_unchecked("anon1");
    let adiff = BlankNode::new_unchecked("adiff");
    let n1 = BlankNode::new_unchecked("n1");
    let n2 = BlankNode::new_unchecked("n2");
    let binary_triples = vec![Triple::new(
        NamedOrBlankNode::BlankNode(anon.clone()),
        nn(vocab::OWL_DIFFERENT_FROM),
        Term::BlankNode(anon.clone()),
    )];
    let onto_bin = read::read_graph(
        binary_triples,
        read::ReadOptions {
            mode: read::Mode::Strict,
            ..Default::default()
        },
    );
    let anon_ind = Individual::Anonymous(AnonymousIndividual::new("anon1"));
    let expected_diff = Axiom::different_individuals([anon_ind.clone(), anon_ind]);
    assert!(onto_bin.contains(&expected_diff));
    let mut k = Classifier::of(onto_bin.axioms.keys());
    let out = k.classify(&Limits::default()).unwrap();
    assert_eq!(out.consistency, Consistency::Inconsistent);

    let list_triples = vec![
        Triple::new(adiff.clone(), rdf_type, nn(vocab::OWL_ALL_DIFFERENT)),
        Triple::new(adiff, nn(vocab::OWL_MEMBERS), n1.clone()),
        Triple::new(n1.clone(), rdf_first.clone(), Term::BlankNode(anon.clone())),
        Triple::new(n1, rdf_rest.clone(), n2.clone()),
        Triple::new(n2.clone(), rdf_first, Term::BlankNode(anon)),
        Triple::new(n2, rdf_rest, rdf_nil),
    ];
    let onto_list = read::read_graph(
        list_triples,
        read::ReadOptions {
            mode: read::Mode::Strict,
            ..Default::default()
        },
    );
    assert!(onto_list.contains(&expected_diff));
    let mut k = Classifier::of(onto_list.axioms.keys());
    let out = k.classify(&Limits::default()).unwrap();
    assert_eq!(out.consistency, Consistency::Inconsistent);

    // Multiset::extend preserves canonical sorted order and duplicates.
    let mut bag: Multiset<ClassExpression> = [ce("B")].into();
    bag.extend([ce("A"), ce("B"), ce("A")]);
    assert_eq!(bag.as_slice(), &[ce("A"), ce("A"), ce("B"), ce("B")]);
}

#[test]
fn wide_shallow_repeated_disjoint_class_keeps_emptiness_in_horn_and_tier_d() {
    let classes: Vec<Class> = (0..4096).map(|i| c(&format!("C{i}"))).collect();
    let inv_exist = ClassExpression::ObjectSomeValuesFrom(
        ObjectPropertyExpression::InverseOf(ObjectProperty::new(ex("p"))),
        Box::new(ClassExpression::Class(Class::thing())),
    );
    let mut conjuncts: std::collections::BTreeSet<ClassExpression> = classes
        .iter()
        .cloned()
        .map(ClassExpression::Class)
        .collect();
    conjuncts.insert(inv_exist.clone());
    let wide = ClassExpression::ObjectIntersectionOf(conjuncts);

    // Put Witness in every conjunct individually (depth 1 in positive position) so the
    // only wide conjunction lives in the negative position of the repeated disjointness.
    let mut witness_axioms = Vec::with_capacity(4098);
    witness_axioms.push(Axiom::declaration(Entity::Class(c("Witness"))));
    for k in &classes {
        witness_axioms.push(Axiom::sub_class_of(
            ce("Witness"),
            ClassExpression::Class(k.clone()),
        ));
    }
    witness_axioms.push(Axiom::sub_class_of(ce("Witness"), inv_exist));

    let strict = Limits {
        strict: true,
        ..Limits::default()
    };

    // Horn DisjointClasses(R, R)
    let mut horn_dc = witness_axioms.clone();
    horn_dc.push(Axiom::disjoint_classes([wide.clone(), wide.clone()]));
    let mut k = Classifier::of(&horn_dc);
    let out = k.classify(&strict).expect("Horn wide DisjointClasses");
    assert!(out.complete);
    assert_eq!(out.stats.tier, Tier::Horn);
    assert!(out.taxonomy.is_unsatisfiable(&c("Witness")));

    // Horn DisjointUnion(:U, R, R)
    let mut horn_du = witness_axioms.clone();
    horn_du.push(Axiom::declaration(Entity::Class(c("U"))));
    horn_du.push(Axiom::new(AxiomKind::DisjointUnion(
        c("U"),
        [wide.clone(), wide.clone()].into(),
    )));
    let mut k = Classifier::of(&horn_du);
    let out = k.classify(&strict).expect("Horn wide DisjointUnion");
    assert!(out.complete);
    assert_eq!(out.stats.tier, Tier::Horn);
    assert!(out.taxonomy.is_unsatisfiable(&c("Witness")));
    assert!(out.taxonomy.is_unsatisfiable(&c("U")));

    // Tier D DisjointClasses(R, R) beside a disjunctive axiom
    let mut dl_dc = witness_axioms;
    dl_dc.push(Axiom::sub_class_of(
        ce("X"),
        ClassExpression::ObjectUnionOf([ce("Y"), ce("Z")].into()),
    ));
    dl_dc.push(Axiom::disjoint_classes([wide.clone(), wide]));
    let mut k = Classifier::of(&dl_dc);
    let out = k.classify(&strict).expect("Tier D wide DisjointClasses");
    assert!(out.complete);
    assert_eq!(out.stats.tier, Tier::Disjunctive);
    assert!(out.taxonomy.is_unsatisfiable(&c("Witness")));
}
