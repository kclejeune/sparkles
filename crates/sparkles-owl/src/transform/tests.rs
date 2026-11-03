//! The transformations against the behaviour of ROBOT 1.9.11's operations, option by
//! option.

use super::*;
use crate::classify::{Error as ClassifyError, Saturation};

fn c(s: &str) -> Class {
    Class::new(format!("http://ex.org/{s}"))
}

fn p(s: &str) -> ObjectProperty {
    ObjectProperty::new(format!("http://ex.org/{s}"))
}

fn i(s: &str) -> Individual {
    Individual::named(format!("http://ex.org/{s}"))
}

fn sub(a: impl Into<ClassExpression>, b: impl Into<ClassExpression>) -> Axiom {
    Axiom::sub_class_of(a, b)
}

fn eq(cs: impl IntoIterator<Item = ClassExpression>) -> Axiom {
    Axiom::equivalent_classes(cs)
}

fn decl(x: &Class) -> Axiom {
    Axiom::declaration(Entity::Class(x.clone()))
}

fn doc(axioms: impl IntoIterator<Item = Axiom>) -> Document {
    Document::new(Header::default(), axioms)
}

fn classify(kinds: &[&AxiomKind]) -> Result<(Classification, Vec<Excluded>), ClassifyError> {
    let mut s = Saturation::new();
    let out = s.classify(kinds, None, &Default::default())?;
    Ok((out, s.excluded().to_vec()))
}

fn run_reason(d: &mut Document, o: &ReasonOptions) -> Result<Report, Error<ClassifyError>> {
    reason(d, o, &mut classify)
}

fn run_reduce(d: &mut Document, o: &ReduceOptions) -> Report {
    reduce(d, o, &mut classify).unwrap()
}

fn has(d: &Document, a: &Axiom) -> bool {
    d.axioms.contains(a)
}

fn inferred() -> Annotation {
    Annotation::new(
        AnnotationProperty::new(reason::IS_INFERRED),
        AnnotationValue::Literal(Literal::simple("true")),
    )
}

/// A ⊑ B ⊑ C with the redundant A ⊑ C, a declared-only class D and the asserted
/// equivalence E ≡ F.
fn chain() -> Document {
    doc([
        sub(c("A"), c("B")),
        sub(c("B"), c("C")),
        sub(c("A"), c("C")),
        decl(&c("D")),
        eq([c("E").into(), c("F").into()]),
    ])
}

#[test]
fn reason_adds_direct_superclasses_and_removes_redundant_ones() {
    let mut d = chain();
    let r = run_reason(&mut d, &ReasonOptions::default()).unwrap();
    assert!(has(&d, &sub(c("A"), c("B"))));
    assert!(has(&d, &sub(c("B"), c("C"))));
    assert!(!has(&d, &sub(c("A"), c("C"))), "A ⊑ C is not direct");
    // owl:Thing is the direct superclass of the roots, and of a declared-only class
    for x in ["C", "D", "E", "F"] {
        assert!(has(&d, &sub(c(x), Class::thing())), "{x}");
    }
    assert!(has(&d, &eq([c("E").into(), c("F").into()])));
    assert_eq!(r.removed, 1);
}

#[test]
fn reason_keeps_redundant_axioms_when_asked() {
    let mut d = chain();
    let o = ReasonOptions {
        remove_redundant_subclass_axioms: false,
        ..Default::default()
    };
    run_reason(&mut d, &o).unwrap();
    assert!(has(&d, &sub(c("A"), c("C"))));
}

#[test]
fn reason_with_indirect_superclasses() {
    let o = ReasonOptions {
        include_indirect: true,
        remove_redundant_subclass_axioms: false,
        ..Default::default()
    };
    let mut d = chain();
    run_reason(&mut d, &o).unwrap();
    assert!(has(&d, &sub(c("A"), c("C"))));
    assert!(has(&d, &sub(c("A"), Class::thing())));
    // the removal of redundant axioms takes the indirect ones out again, as in ROBOT
    let mut d = chain();
    let o = ReasonOptions {
        include_indirect: true,
        ..Default::default()
    };
    run_reason(&mut d, &o).unwrap();
    assert!(!has(&d, &sub(c("A"), Class::thing())));
}

#[test]
fn reason_excludes_owl_thing_and_tautologies() {
    for o in [
        ReasonOptions {
            exclude_owl_thing: true,
            ..Default::default()
        },
        ReasonOptions {
            exclude_duplicate_axioms: true,
            ..Default::default()
        },
        ReasonOptions {
            tautologies: Tautologies::Structural,
            ..Default::default()
        },
        ReasonOptions {
            tautologies: Tautologies::parse("all"),
            ..Default::default()
        },
    ] {
        let mut d = chain();
        run_reason(&mut d, &o).unwrap();
        assert!(!has(&d, &sub(c("D"), Class::thing())), "{o:?}");
        assert!(has(&d, &sub(c("A"), c("B"))), "{o:?}");
    }
    assert_eq!(Tautologies::parse("FALSE"), Tautologies::Keep);
    assert_eq!(Tautologies::parse("Structural"), Tautologies::Structural);
    assert_eq!(Tautologies::parse("true"), Tautologies::All);
}

#[test]
fn reason_excludes_axioms_about_undeclared_classes() {
    let o = ReasonOptions {
        exclude_external_entities: true,
        ..Default::default()
    };
    let mut d = doc([sub(c("A"), c("B")), decl(&c("D")), sub(c("X"), c("Y"))]);
    d.axioms.insert(decl(&c("A")));
    run_reason(&mut d, &o).unwrap();
    assert!(has(&d, &sub(c("D"), Class::thing())));
    // Y ⊑ owl:Thing mentions no declared class
    assert!(!has(&d, &sub(c("Y"), Class::thing())));
}

#[test]
fn reason_annotates_inferred_axioms_and_refuses_other_types() {
    let o = ReasonOptions {
        annotate_inferred_axioms: true,
        ..Default::default()
    };
    let mut d = chain();
    run_reason(&mut d, &o).unwrap();
    // the asserted direct axiom is replaced by the annotated one
    assert!(!has(&d, &sub(c("A"), c("B"))));
    assert!(has(&d, &sub(c("A"), c("B")).annotated(inferred())));
    assert!(has(&d, &sub(c("D"), Class::thing()).annotated(inferred())));
    let o = ReasonOptions {
        annotate_inferred_axioms: true,
        generators: vec![Generator::SubClass, Generator::EquivalentClass],
        ..Default::default()
    };
    let e = run_reason(&mut chain(), &o).unwrap_err();
    assert!(e.to_string().contains("AXIOM TYPE ERROR"), "{e}");
}

#[test]
fn reason_into_a_new_ontology() {
    let label = Axiom::from(AxiomKind::AnnotationAssertion(
        AnnotationProperty::new(crate::vocab::RDFS_LABEL),
        AnnotationSubject::Iri(Iri::new_unchecked("http://ex.org/A")),
        AnnotationValue::Literal(Literal::simple("a")),
    ));
    let mut d = chain();
    d.axioms.insert(label.clone());
    d.header
        .imports
        .insert(Iri::new_unchecked("http://ex.org/other"));
    let o = ReasonOptions {
        new_ontology: NewOntology::Empty,
        ..Default::default()
    };
    let mut n = d.clone();
    run_reason(&mut n, &o).unwrap();
    assert!(n.header.imports.is_empty());
    assert!(!has(&n, &label));
    assert!(!has(&n, &decl(&c("D"))));
    assert!(has(&n, &sub(c("A"), c("B"))));
    assert!(!has(&n, &eq([c("E").into(), c("F").into()])));
    let o = ReasonOptions {
        new_ontology: NewOntology::WithAnnotations,
        ..Default::default()
    };
    let mut n = d.clone();
    run_reason(&mut n, &o).unwrap();
    assert!(has(&n, &label));
    assert!(has(&n, &sub(c("A"), c("B"))));
}

#[test]
fn reason_checks_equivalent_classes() {
    let none = ReasonOptions {
        equivalent_classes: EquivalentClasses::parse("none").unwrap(),
        ..Default::default()
    };
    let only = ReasonOptions {
        equivalent_classes: EquivalentClasses::parse("asserted-only").unwrap(),
        ..Default::default()
    };
    let e = run_reason(&mut chain(), &none).unwrap_err();
    assert!(
        e.to_string().contains("EQUIVALENT CLASS AXIOM ERROR"),
        "{e}"
    );
    run_reason(&mut chain(), &only).unwrap();
    // G ≡ H inferred from two subclass axioms is not asserted
    let mut d = doc([sub(c("G"), c("H")), sub(c("H"), c("G"))]);
    assert!(matches!(
        run_reason(&mut d, &only),
        Err(Error::EquivalentClasses { .. })
    ));
    assert_eq!(EquivalentClasses::parse("true"), Ok(EquivalentClasses::All));
    assert_eq!(
        EquivalentClasses::parse("false"),
        Ok(EquivalentClasses::None)
    );
    assert!(EquivalentClasses::parse("some").is_err());
}

#[test]
fn reason_generates_equivalences_and_class_assertions() {
    let mut d = doc([
        sub(c("G"), c("H")),
        sub(c("H"), c("G")),
        sub(c("A"), c("B")),
        Axiom::class_assertion(c("A"), i("a")),
        Axiom::declaration(Entity::NamedIndividual(NamedIndividual::new(
            "http://ex.org/b",
        ))),
    ]);
    let o = ReasonOptions {
        generators: Generator::parse_list("SubClass EquivalentClass ClassAssertion").unwrap(),
        ..Default::default()
    };
    run_reason(&mut d, &o).unwrap();
    assert!(has(&d, &eq([c("G").into(), c("H").into()])));
    assert!(has(&d, &Axiom::class_assertion(c("A"), i("a"))));
    assert!(!has(&d, &Axiom::class_assertion(c("B"), i("a"))));
    assert!(has(&d, &Axiom::class_assertion(Class::thing(), i("b"))));
    let mut d = doc([sub(c("A"), c("B")), Axiom::class_assertion(c("A"), i("a"))]);
    let o = ReasonOptions {
        generators: vec![Generator::ClassAssertion],
        include_indirect: true,
        ..Default::default()
    };
    run_reason(&mut d, &o).unwrap();
    for t in [c("A"), c("B"), Class::thing()] {
        assert!(has(&d, &Axiom::class_assertion(t.clone(), i("a"))), "{t:?}");
    }
}

#[test]
fn reason_parses_generators_as_robot() {
    assert_eq!(
        Generator::parse_list("").unwrap(),
        vec![Generator::SubClass]
    );
    assert_eq!(
        Generator::parse_list("subclass ").unwrap(),
        vec![Generator::SubClass]
    );
    assert!(matches!(
        Generator::parse("DisjointClasses"),
        Ok(Generator::Unanswered(_))
    ));
    let e = Generator::parse_list("subclass nonsense").unwrap_err();
    assert!(e.contains("UNKNOWN AXIOM GENERATOR nonsense"), "{e}");
    let mut d = chain();
    let o = ReasonOptions {
        generators: Generator::parse_list("DisjointClasses").unwrap(),
        ..Default::default()
    };
    let r = run_reason(&mut d, &o).unwrap();
    assert_eq!(r.warnings.len(), 1);
}

#[test]
fn reason_refuses_incoherent_ontologies() {
    let disjoint = Axiom::disjoint_classes([c("B").into(), c("C").into()]);
    let mut d = doc([
        sub(c("A"), c("B")),
        sub(c("A"), c("C")),
        disjoint.clone(),
        sub(c("X"), c("Y")),
    ]);
    let o = ReasonOptions {
        dump_unsatisfiable: true,
        ..Default::default()
    };
    let before = d.clone();
    match run_reason(&mut d, &o) {
        Err(Error::UnsatisfiableClasses { classes, module }) => {
            assert_eq!(classes, vec![c("A")]);
            let module = module.unwrap();
            assert!(module.contains(&disjoint));
            assert!(module.contains(&sub(c("A"), c("B"))));
            assert!(!module.contains(&sub(c("X"), c("Y"))));
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(d, before, "a failed run leaves the document");
    // an object property that relates nothing
    let gci = sub(
        ClassExpression::some(p("p"), Class::thing()),
        Class::nothing(),
    );
    let mut d = doc([gci, sub(c("A"), c("B"))]);
    match run_reason(&mut d, &ReasonOptions::default()) {
        Err(Error::UnsatisfiableProperties(ps)) => assert_eq!(ps, vec![p("p")]),
        other => panic!("{other:?}"),
    }
    let mut d = doc([Axiom::class_assertion(Class::nothing(), i("a"))]);
    assert!(matches!(
        run_reason(&mut d, &ReasonOptions::default()),
        Err(Error::Inconsistent)
    ));
}

#[test]
fn relax_equivalences_by_options() {
    let def = eq([
        c("X").into(),
        ClassExpression::and([
            c("A").into(),
            ClassExpression::some(p("p"), c("B")),
            ClassExpression::some(p("q"), ClassExpression::and([c("C").into(), c("D").into()])),
            ClassExpression::ObjectMinCardinality(2, p("r").into(), Some(Box::new(c("E").into()))),
            ClassExpression::ObjectMaxCardinality(0, p("s").into(), None),
        ]),
    ]);
    let mut d = doc([def.clone(), eq([c("Y").into(), c("Z").into()])]);
    let r = relax(&mut d, &RelaxOptions::default());
    assert_eq!(r.added, 4);
    assert!(has(&d, &sub(c("X"), c("A"))));
    assert!(has(&d, &sub(c("X"), ClassExpression::some(p("p"), c("B")))));
    assert!(has(&d, &sub(c("X"), ClassExpression::some(p("r"), c("E")))));
    assert!(has(&d, &def), "the equivalence stays");
    assert!(!has(&d, &sub(c("Y"), c("Z"))));
    let mut d = doc([def.clone(), eq([c("Y").into(), c("Z").into()])]);
    let o = RelaxOptions {
        enforce_obo_format: true,
        exclude_named_classes: false,
        ..Default::default()
    };
    relax(&mut d, &o);
    assert!(has(&d, &sub(c("Y"), c("Z"))));
    assert!(has(&d, &sub(c("Z"), c("Y"))));
    assert!(!has(
        &d,
        &sub(
            c("X"),
            ClassExpression::some(p("q"), ClassExpression::and([c("C").into(), c("D").into()]))
        )
    ));
    let gci = sub(
        c("W"),
        ClassExpression::and([c("A").into(), ClassExpression::some(p("p"), c("B"))]),
    );
    let mut d = doc([gci.clone()]);
    assert_eq!(relax(&mut d, &RelaxOptions::default()).added, 0);
    let o = RelaxOptions {
        include_subclass_of: true,
        ..Default::default()
    };
    assert_eq!(relax(&mut d, &o).added, 2);
    assert!(has(&d, &sub(c("W"), c("A"))));
}

#[test]
fn reduce_named_and_existential_subclass_axioms() {
    let mut d = doc([
        sub(c("A"), c("B")),
        sub(c("B"), c("C")),
        sub(c("A"), c("C")),
        sub(c("A"), ClassExpression::some(p("p"), c("B"))),
        sub(c("A"), ClassExpression::some(p("p"), c("C"))),
        sub(c("K"), ClassExpression::some(p("t"), c("L"))),
        sub(c("L"), ClassExpression::some(p("t"), c("M"))),
        sub(c("K"), ClassExpression::some(p("t"), c("M"))),
        Axiom::from(AxiomKind::TransitiveObjectProperty(p("t").into())),
    ]);
    let r = run_reduce(&mut d, &ReduceOptions::default());
    assert_eq!(r.removed, 3, "{d:?}");
    assert!(!has(&d, &sub(c("A"), c("C"))));
    assert!(!has(
        &d,
        &sub(c("A"), ClassExpression::some(p("p"), c("C")))
    ));
    assert!(!has(
        &d,
        &sub(c("K"), ClassExpression::some(p("t"), c("M")))
    ));
    assert!(has(&d, &sub(c("A"), ClassExpression::some(p("p"), c("B")))));
}

#[test]
fn reduce_with_subproperties_and_general_inclusions() {
    let base = || {
        doc([
            sub(c("A"), ClassExpression::some(p("p"), c("B"))),
            sub(c("A"), ClassExpression::some(p("q"), c("B"))),
            Axiom::from(AxiomKind::SubObjectPropertyOf(
                SubObjectPropertyExpression::Expression(p("p").into()),
                p("q").into(),
            )),
        ])
    };
    let mut d = base();
    assert_eq!(run_reduce(&mut d, &ReduceOptions::default()).removed, 0);
    let mut d = base();
    let o = ReduceOptions {
        include_subproperties: true,
        ..Default::default()
    };
    assert_eq!(run_reduce(&mut d, &o).removed, 1);
    assert!(!has(
        &d,
        &sub(c("A"), ClassExpression::some(p("q"), c("B")))
    ));
    // a general class inclusion whose superclass another inclusion entails
    let x = ClassExpression::some(p("p"), c("B"));
    let mut d = doc([
        sub(x.clone(), c("C")),
        sub(c("C"), c("D")),
        sub(x.clone(), c("D")),
    ]);
    assert_eq!(run_reduce(&mut d, &ReduceOptions::default()).removed, 1);
    assert!(!has(&d, &sub(x, c("D"))));
}

#[test]
fn reduce_preserves_annotated_axioms_and_named_only() {
    let note = Annotation::comment("kept");
    let mut d = doc([
        sub(c("A"), c("B")),
        sub(c("B"), c("C")),
        sub(c("A"), c("C")).annotated(note.clone()),
    ]);
    let o = ReduceOptions {
        preserve_annotated_axioms: true,
        ..Default::default()
    };
    assert_eq!(run_reduce(&mut d, &o).removed, 0);
    assert_eq!(run_reduce(&mut d, &ReduceOptions::default()).removed, 1);
    // named classes only: the existential stays, A ⊑ C goes, and so does a subclass
    // axiom between equivalent classes
    let mut d = doc([
        sub(c("A"), c("B")),
        sub(c("B"), c("C")),
        sub(c("A"), c("C")),
        sub(c("A"), ClassExpression::some(p("p"), c("B"))),
        sub(c("A"), ClassExpression::some(p("p"), c("C"))),
        sub(c("G"), c("H")),
        sub(c("H"), c("G")),
    ]);
    let o = ReduceOptions {
        named_classes_only: true,
        ..Default::default()
    };
    let r = run_reduce(&mut d, &o);
    assert_eq!(r.removed, 3, "{d:?}");
    assert!(has(&d, &sub(c("A"), ClassExpression::some(p("p"), c("C")))));
    assert!(!has(&d, &sub(c("A"), c("C"))));
    assert!(has(&d, &sub(c("A"), c("B"))));
}

#[test]
fn reduce_leaves_an_inconsistent_ontology() {
    let mut d = doc([
        sub(c("A"), c("B")),
        sub(c("B"), c("C")),
        sub(c("A"), c("C")),
        Axiom::class_assertion(Class::nothing(), i("a")),
    ]);
    let o = ReduceOptions {
        named_classes_only: true,
        ..Default::default()
    };
    let r = run_reduce(&mut d, &o);
    assert_eq!(r.removed, 0);
    assert_eq!(r.warnings, vec!["Ontology is not consistent!".to_string()]);
}

#[test]
fn reason_relax_reduce_in_turn() {
    // ROBOT's documented example: relax and reduce after reason
    let part = p("partOf");
    let mut d = doc([
        eq([
            c("CerebellarNeuron").into(),
            ClassExpression::and([
                c("Neuron").into(),
                ClassExpression::some(part.clone(), c("Cerebellum")),
            ]),
        ]),
        eq([
            c("HindbrainNeuron").into(),
            ClassExpression::and([
                c("Neuron").into(),
                ClassExpression::some(part.clone(), c("Brain")),
            ]),
        ]),
        sub(
            c("Cerebellum"),
            ClassExpression::some(part.clone(), c("Brain")),
        ),
        Axiom::from(AxiomKind::TransitiveObjectProperty(part.clone().into())),
    ]);
    run_reason(&mut d, &ReasonOptions::default()).unwrap();
    assert!(has(&d, &sub(c("CerebellarNeuron"), c("HindbrainNeuron"))));
    relax(&mut d, &RelaxOptions::default());
    assert!(has(&d, &sub(c("CerebellarNeuron"), c("Neuron"))));
    run_reduce(&mut d, &ReduceOptions::default());
    assert!(!has(&d, &sub(c("CerebellarNeuron"), c("Neuron"))));
    assert!(has(
        &d,
        &sub(
            c("CerebellarNeuron"),
            ClassExpression::some(part.clone(), c("Cerebellum"))
        )
    ));
    assert!(!has(
        &d,
        &sub(
            c("CerebellarNeuron"),
            ClassExpression::some(part, c("Brain"))
        )
    ));
}

#[test]
fn the_module_holds_global_axioms() {
    let global = sub(Class::thing(), c("B"));
    let disjoint = Axiom::disjoint_classes([c("A").into(), c("B").into()]);
    let mut d = doc([
        global.clone(),
        disjoint.clone(),
        decl(&c("A")),
        sub(c("X"), c("Y")),
    ]);
    let o = ReasonOptions {
        dump_unsatisfiable: true,
        ..Default::default()
    };
    match run_reason(&mut d, &o) {
        Err(Error::UnsatisfiableClasses { classes, module }) => {
            assert_eq!(classes, vec![c("A")]);
            let module = module.unwrap();
            assert!(module.contains(&global), "{module:?}");
            assert!(module.contains(&disjoint), "{module:?}");
            assert!(module.contains(&decl(&c("A"))));
            assert!(!module.contains(&sub(c("X"), c("Y"))));
        }
        other => panic!("{other:?}"),
    }
}

fn some(q: &str, x: impl Into<ClassExpression>) -> ClassExpression {
    ClassExpression::some(p(q), x)
}

fn and(xs: impl IntoIterator<Item = ClassExpression>) -> ClassExpression {
    ClassExpression::ObjectIntersectionOf(xs.into_iter().collect())
}

fn ax(k: AxiomKind) -> Axiom {
    Axiom::from(k)
}

fn sub_prop(a: &str, b: &str) -> Axiom {
    ax(AxiomKind::SubObjectPropertyOf(
        SubObjectPropertyExpression::Expression(p(a).into()),
        p(b).into(),
    ))
}

fn terms(qs: &[&str]) -> MaterializeOptions {
    MaterializeOptions {
        properties: qs.iter().map(|q| p(q)).collect(),
        ..Default::default()
    }
}

/// `materialize`, with the axioms it added, each checked to be entailed: the document
/// with a fresh class equivalent to the superclass is classified, and the class must be
/// below it.
fn run_materialize(d: &mut Document, o: &MaterializeOptions) -> BTreeSet<Axiom> {
    let before = d.clone();
    let r = materialize(d, o, &mut classify).unwrap();
    let added: BTreeSet<Axiom> = d.axioms.difference(&before.axioms).cloned().collect();
    assert_eq!(r.added, added.len());
    for a in &added {
        let AxiomKind::SubClassOf(ClassExpression::Class(x), sup) = &a.kind else {
            panic!("{a:?}");
        };
        assert!(a.annotations.is_empty());
        let q = Class::new("urn:test:q");
        let mut kinds = before.logical_kinds();
        let def = AxiomKind::EquivalentClasses([q.clone().into(), sup.clone()].into());
        kinds.push(&def);
        let (k, _) = classify(&kinds).unwrap();
        assert!(k.taxonomy.is_subclass(x, &q), "{a:?} is not entailed");
    }
    added
}

fn set(xs: impl IntoIterator<Item = Axiom>) -> BTreeSet<Axiom> {
    xs.into_iter().collect()
}

#[test]
fn materialize_asserts_the_direct_existentials_of_a_definition() {
    // A ≡ B ⊓ ∃p.C and C ⊑ C2: A's direct superclasses are B and ∃p.C, not ∃p.C2
    let mut d = doc([
        eq([c("A").into(), and([c("B").into(), some("p", c("C"))])]),
        sub(c("C"), c("C2")),
    ]);
    let added = run_materialize(&mut d, &terms(&["p"]));
    assert_eq!(
        added,
        set([sub(c("A"), c("B")), sub(c("A"), some("p", c("C")))])
    );
}

#[test]
fn materialize_skips_asserted_and_indirect_existentials() {
    // D ⊑ ∃p.C is asserted, and E ⊑ D gives E ⊑ ∃p.C only through D
    let mut d = doc([
        sub(c("D"), some("p", c("C"))),
        sub(c("E"), c("D")),
        sub(c("C"), c("C2")),
    ]);
    assert!(run_materialize(&mut d, &terms(&["p"])).is_empty());
    // an annotated assertion is not the same axiom, so ROBOT adds the plain one
    let note = Annotation::new(
        AnnotationProperty::new("http://ex.org/note"),
        AnnotationValue::Literal(Literal::simple("x")),
    );
    let mut d = doc([sub(c("D"), some("p", c("C"))).annotated(note)]);
    assert_eq!(
        run_materialize(&mut d, &terms(&["p"])),
        set([sub(c("D"), some("p", c("C")))])
    );
}

#[test]
fn materialize_follows_the_property_hierarchy() {
    // q ⊑ p and H ⊑ ∃q.C: with p alone ∃p.C is direct, with q too it is below ∃q.C
    let base = || doc([sub_prop("q", "p"), sub(c("H"), some("q", c("C")))]);
    let mut d = base();
    assert_eq!(
        run_materialize(&mut d, &terms(&["p"])),
        set([sub(c("H"), some("p", c("C")))])
    );
    let mut d = base();
    assert!(run_materialize(&mut d, &terms(&["p", "q"])).is_empty());
}

#[test]
fn materialize_through_transitivity_and_chains() {
    // q ⊑ p, p transitive, I ⊑ ∃q.J and J ⊑ ∃q.K: I ⊑ ∃p.K holds but ∃p.J is below it
    let mut d = doc([
        sub_prop("q", "p"),
        ax(AxiomKind::TransitiveObjectProperty(p("p").into())),
        sub(c("I"), some("q", c("J"))),
        sub(c("J"), some("q", c("K"))),
    ]);
    assert_eq!(
        run_materialize(&mut d, &terms(&["p"])),
        set([
            sub(c("I"), some("p", c("J"))),
            sub(c("J"), some("p", c("K")))
        ])
    );
    // r ∘ s ⊑ t: L ⊑ ∃r.M and M ⊑ ∃s.N give L ⊑ ∃t.N
    let mut d = doc([
        ax(AxiomKind::SubObjectPropertyOf(
            SubObjectPropertyExpression::Chain(vec![p("r").into(), p("s").into()]),
            p("t").into(),
        )),
        sub(c("L"), some("r", c("M"))),
        sub(c("M"), some("s", c("N"))),
    ]);
    assert_eq!(
        run_materialize(&mut d, &terms(&["t"])),
        set([sub(c("L"), some("t", c("N")))])
    );
}

#[test]
fn materialize_reads_directness_with_the_expressions() {
    // a domain puts ∃p.Q below Dom, so Dom is no longer a direct superclass of P, which
    // reason would assert
    let mut d = doc([
        ax(AxiomKind::ObjectPropertyDomain(
            p("p").into(),
            c("Dom").into(),
        )),
        sub(c("P"), some("p", c("Q"))),
    ]);
    assert!(run_materialize(&mut d, &terms(&["p"])).is_empty());
    let mut r = d.clone();
    run_reason(&mut r, &ReasonOptions::default()).unwrap();
    assert!(has(&r, &sub(c("P"), c("Dom"))));
    // a named class equivalent to the expression shares its node, and both are asserted
    let mut d = doc([
        eq([c("W").into(), some("p", c("V"))]),
        sub(c("Z"), some("p", c("V"))),
    ]);
    assert_eq!(
        run_materialize(&mut d, &terms(&["p"])),
        set([sub(c("Z"), c("W"))])
    );
    // equivalent fillers give equivalent expressions, each asserted
    let mut d = doc([
        eq([c("V1").into(), c("V2").into()]),
        sub(c("Y"), some("p", c("V1"))),
    ]);
    assert_eq!(
        run_materialize(&mut d, &terms(&["p"])),
        set([sub(c("Y"), some("p", c("V2")))])
    );
    // an anonymous filler gives an expression for each of its named superclasses
    let mut d = doc([
        sub(c("U"), some("p", and([c("V").into(), c("W").into()]))),
        sub(c("U"), Class::thing()),
    ]);
    assert_eq!(
        run_materialize(&mut d, &terms(&["p"])),
        set([
            sub(c("U"), some("p", c("V"))),
            sub(c("U"), some("p", c("W")))
        ])
    );
}

#[test]
fn materialize_skips_thing_the_class_itself_and_undescribed_classes() {
    // r is reflexive, so S ⊑ ∃r.S, which names S and is skipped
    let mut d = doc([
        ax(AxiomKind::ReflexiveObjectProperty(p("r").into())),
        sub(c("S"), c("T")),
        sub(c("T"), Class::thing()),
    ]);
    assert!(run_materialize(&mut d, &terms(&["r"])).is_empty());
    // K is below Dk by a general inclusion only, so OWL API gives it no class axiom, and
    // owl:Nothing is passed over
    let base = || {
        doc([
            sub(and([c("K").into(), c("K2").into()]), c("Dk")),
            eq([c("K").into(), c("K2").into()]),
            sub(Class::nothing(), c("Dk")),
        ])
    };
    let mut d = base();
    assert_eq!(
        run_materialize(&mut d, &terms(&["p"])),
        set([sub(c("K"), c("Dk")), sub(c("K2"), c("Dk"))])
    );
    let mut d = doc([
        sub(and([c("K").into(), c("K2").into()]), c("Dk")),
        sub(c("K"), c("K2")),
    ]);
    assert_eq!(
        run_materialize(&mut d, &terms(&["p"])),
        set([sub(c("K"), c("Dk"))]),
        "K2 has no class axiom"
    );
    let mut r = base();
    run_reason(&mut r, &ReasonOptions::default()).unwrap();
    assert!(has(&r, &sub(Class::nothing(), Class::nothing())));
}

#[test]
fn materialize_takes_every_property_without_terms() {
    let base = || {
        doc([
            sub_prop("q", "p"),
            sub(c("H"), some("q", c("C"))),
            eq([c("A").into(), and([c("B").into(), some("r", c("C"))])]),
        ])
    };
    let mut d = base();
    assert_eq!(
        run_materialize(&mut d, &MaterializeOptions::default()),
        set([sub(c("A"), c("B")), sub(c("A"), some("r", c("C")))])
    );
    // a property the ontology does not use adds only the named superclasses
    let mut d = base();
    assert_eq!(
        run_materialize(&mut d, &terms(&["elsewhere"])),
        set([sub(c("A"), c("B"))])
    );
    // with --create-new-ontology ROBOT saves the ontology without them
    let mut d = base();
    let before = d.clone();
    let o = MaterializeOptions {
        create_new_ontology: true,
        ..terms(&["p"])
    };
    let r = materialize(&mut d, &o, &mut classify).unwrap();
    assert_eq!(d, before);
    assert_eq!(r.added, 0);
    assert!(r.warnings.iter().any(|w| w.contains("not saved")), "{r:?}");
}

#[test]
fn materialize_validates_as_robot() {
    let run = |d: &mut Document| materialize(d, &terms(&["p"]), &mut classify);
    let mut d = doc([
        sub(c("A"), c("B")),
        sub(c("A"), c("C")),
        Axiom::disjoint_classes([c("B").into(), c("C").into()]),
    ]);
    let before = d.clone();
    match run(&mut d) {
        Err(Error::UnsatisfiableClasses { classes, module }) => {
            assert_eq!(classes, vec![c("A")]);
            assert!(module.is_none());
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(d, before);
    let mut d = doc([
        sub(some("p", Class::thing()), Class::nothing()),
        sub(c("A"), c("B")),
    ]);
    assert!(matches!(run(&mut d), Err(Error::UnsatisfiableProperties(ps)) if ps == vec![p("p")]));
    let mut d = doc([Axiom::class_assertion(Class::nothing(), i("a"))]);
    assert!(matches!(run(&mut d), Err(Error::Inconsistent)));
    // ∃p.B is unsatisfiable through p's range, though no named class is
    let mut d = doc([
        ax(AxiomKind::ObjectPropertyRange(p("p").into(), c("R").into())),
        Axiom::disjoint_classes([c("R").into(), c("B").into()]),
        sub(c("A"), some("p", c("R"))),
    ]);
    assert!(run_materialize(&mut d, &terms(&["p"])).is_empty());
}

#[test]
fn reason_materialize_reduce_in_turn() {
    // the chain of a release: the reasoned ontology, its existentials, then the
    // redundant ones removed
    let mut d = doc([
        sub_prop("q", "p"),
        eq([c("A").into(), and([c("B").into(), some("p", c("C"))])]),
        sub(c("A2"), c("A")),
        sub(c("A2"), some("q", c("C"))),
        sub(c("C"), c("C2")),
    ]);
    run_reason(&mut d, &ReasonOptions::default()).unwrap();
    let added = run_materialize(&mut d, &terms(&["p"]));
    assert_eq!(added, set([sub(c("A"), some("p", c("C")))]));
    run_reduce(&mut d, &ReduceOptions::default());
    assert!(has(&d, &sub(c("A"), some("p", c("C")))));
    assert!(has(&d, &sub(c("A2"), some("q", c("C")))));
}

mod pruning {
    use super::*;
    use proptest::prelude::*;

    const CLASSES: usize = 6;
    const ROLES: usize = 4;

    fn r(i: usize) -> ObjectPropertyExpression {
        p(&format!("r{i}")).into()
    }

    fn gen_class() -> impl Strategy<Value = ClassExpression> {
        prop_oneof![
            8 => (0..CLASSES).prop_map(|i| ClassExpression::Class(c(&format!("C{i}")))),
            1 => Just(ClassExpression::Class(Class::thing())),
        ]
    }

    fn gen_expr() -> impl Strategy<Value = ClassExpression> {
        gen_class().prop_recursive(2, 8, 2, |inner| {
            prop_oneof![
                3 => (0..ROLES, inner.clone()).prop_map(|(q, f)| ClassExpression::some(r(q), f)),
                3 => (inner.clone(), inner).prop_map(|(a, b)| and([a, b])),
            ]
        })
    }

    /// EL axioms without individuals, reflexive properties or `ObjectHasSelf`, where the
    /// fillers are pruned: ranges on r0 and r1, chains and transitivity on r2 and r3.
    fn gen_axiom() -> impl Strategy<Value = Axiom> {
        let edges = [
            (0, 1),
            (1, 0),
            (0, 2),
            (0, 3),
            (1, 2),
            (1, 3),
            (2, 3),
            (3, 2),
        ];
        prop_oneof![
            10 => (gen_expr(), gen_expr()).prop_map(|(a, b)| sub(a, b)),
            6 => ((0..CLASSES), gen_expr()).prop_map(|(a, b)| sub(c(&format!("C{a}")), b)),
            3 => ((0..CLASSES), gen_expr()).prop_map(|(a, b)| eq([c(&format!("C{a}")).into(), b])),
            1 => ((0..CLASSES), (0..CLASSES)).prop_filter("distinct", |(a, b)| a != b).prop_map(|(a, b)| {
                Axiom::disjoint_classes([c(&format!("C{a}")).into(), c(&format!("C{b}")).into()])
            }),
            2 => (0..edges.len()).prop_map(move |i| {
                let (a, b) = edges[i];
                ax(AxiomKind::SubObjectPropertyOf(SubObjectPropertyExpression::Expression(r(a)), r(b)))
            }),
            1 => ((0..ROLES), (0..ROLES), (2..ROLES)).prop_map(|(a, b, s)| {
                ax(AxiomKind::SubObjectPropertyOf(SubObjectPropertyExpression::Chain(vec![r(a), r(b)]), r(s)))
            }),
            1 => (2..ROLES).prop_map(|s| ax(AxiomKind::TransitiveObjectProperty(r(s)))),
            1 => ((0..ROLES), gen_class()).prop_map(|(s, x)| ax(AxiomKind::ObjectPropertyDomain(r(s), x))),
            2 => ((0..2usize), gen_class()).prop_map(|(s, x)| ax(AxiomKind::ObjectPropertyRange(r(s), x))),
        ]
    }

    fn outcome(d: &Document, o: &MaterializeOptions, prune: bool) -> Option<BTreeSet<Axiom>> {
        let mut d = d.clone();
        materialize::materialize_with(&mut d, o, &mut classify, prune)
            .ok()
            .map(|_| d.axioms)
    }

    proptest! {
        #![proptest_config(ProptestConfig { cases: 400, ..ProptestConfig::default() })]

        /// Naming only the fillers gives what naming every class gives.
        #[test]
        fn pruned_fillers_change_nothing(axioms in prop::collection::vec(gen_axiom(), 0..16), all in any::<bool>()) {
            let mut axioms = axioms;
            for i in 0..CLASSES {
                axioms.push(decl(&c(&format!("C{i}"))));
            }
            let d = doc(axioms);
            assert!(materialize::fillers(&d).is_some());
            let o = if all { MaterializeOptions::default() } else { terms(&["r0", "r2"]) };
            prop_assert_eq!(outcome(&d, &o, true), outcome(&d, &o, false));
        }
    }

    #[test]
    fn individuals_reflexive_properties_and_self_name_every_class() {
        for a in [
            Axiom::class_assertion(c("A"), i("a")),
            ax(AxiomKind::ReflexiveObjectProperty(r(0))),
            sub(c("A"), ClassExpression::ObjectHasSelf(r(0))),
            sub(c("A"), ClassExpression::ObjectHasValue(r(0), i("a"))),
        ] {
            assert!(materialize::fillers(&doc([a.clone()])).is_none(), "{a:?}");
        }
        // only classes that occur where they could grow are fillers
        let d = doc([
            sub(c("A"), and([c("B").into(), some("r0", c("C"))])),
            Axiom::disjoint_classes([c("D").into(), c("E").into()]),
            eq([c("F").into(), c("G").into()]),
        ]);
        let f = materialize::fillers(&d).unwrap();
        let mut got: Vec<&str> = f.iter().map(|x| x.as_str()).collect();
        got.sort();
        assert_eq!(
            got,
            [
                "http://ex.org/B",
                "http://ex.org/C",
                "http://ex.org/F",
                "http://ex.org/G"
            ]
        );
    }
}
