use super::*;
use proptest::prelude::*;

const EX: &str = "http://ex.org/";
const OTHER: &str = "http://other.org/";

fn c(s: &str) -> Class {
    Class::new(format!("{EX}{s}"))
}
fn op(s: &str) -> ObjectProperty {
    ObjectProperty::new(format!("{EX}{s}"))
}
fn dp(s: &str) -> DataProperty {
    DataProperty::new(format!("{EX}{s}"))
}
fn ind(s: &str) -> Individual {
    Individual::named(format!("{EX}{s}"))
}
fn xsd(s: &str) -> Datatype {
    Datatype::new(format!("{}{s}", vocab::XSD))
}

/// A small university vocabulary with labels, a label shared by two classes and a
/// name that is both a class and an object property.
fn uni() -> Vocabulary {
    let mut v = Vocabulary::new([("ex", EX), ("other", OTHER)]);
    for (s, label) in [
        ("Person", None),
        ("Professor", Some("Professor")),
        ("GraduateStudent", Some("Graduate student")),
        ("Course", Some("Course")),
        ("Department", Some("Department")),
        ("Advised", None),
    ] {
        v.add_entity(Entity::Class(c(s)));
        if let Some(l) = label {
            v.add_label(&format!("{EX}{s}"), l);
        }
    }
    v.add_entity(Entity::Class(Class::new(format!("{OTHER}Lecture"))));
    v.add_label(&format!("{OTHER}Lecture"), "Course");
    for (s, label) in [
        ("teaches", "teaches"),
        ("partOf", "part of"),
        ("advisor", "has advisor"),
    ] {
        v.add_entity(Entity::ObjectProperty(op(s)));
        v.add_label(&format!("{EX}{s}"), label);
    }
    // punned: a class and an object property
    v.add_entity(Entity::ObjectProperty(op("Advised")));
    for s in ["age", "name"] {
        v.add_entity(Entity::DataProperty(dp(s)));
    }
    for (s, label) in [("ada", "Ada Lovelace"), ("bob", "Bob")] {
        v.add_entity(Entity::NamedIndividual(NamedIndividual::new(format!(
            "{EX}{s}"
        ))));
        v.add_label(&format!("{EX}{s}"), label);
    }
    v
}

fn ce(text: &str) -> ClassExpression {
    class_expression(text, &uni()).unwrap_or_else(|e| panic!("{text}: {e}"))
}

fn err(text: &str) -> Error {
    class_expression(text, &uni()).expect_err(text)
}

#[test]
fn names() {
    assert_eq!(ce("Person"), c("Person").into());
    assert_eq!(ce("ex:Person"), c("Person").into());
    assert_eq!(ce("<http://ex.org/Person>"), c("Person").into());
    assert_eq!(ce("'Graduate student'"), c("GraduateStudent").into());
    assert_eq!(ce("GraduateStudent"), c("GraduateStudent").into());
    // a name typed in another case
    assert_eq!(ce("person"), c("Person").into());
    assert_eq!(ce("Thing"), Class::thing().into());
    assert_eq!(ce("owl:Nothing"), Class::nothing().into());
    // an IRI the vocabulary does not know is taken as the kind the place wants
    assert_eq!(ce("ex:New"), c("New").into());
}

#[test]
fn precedence() {
    let p = || ClassExpression::Class(c("Person"));
    let q = || ClassExpression::Class(c("Professor"));
    let d = || ClassExpression::Class(c("Department"));
    assert_eq!(
        ce("Person and Professor or Department"),
        ClassExpression::or([ClassExpression::and([p(), q()]), d()])
    );
    assert_eq!(
        ce("Person and (Professor or Department)"),
        ClassExpression::and([p(), ClassExpression::or([q(), d()])])
    );
    // a restriction's filler is a primary
    assert_eq!(
        ce("teaches some ex:Course and Person"),
        ClassExpression::and([ClassExpression::some(op("teaches"), c("Course")), p()])
    );
    assert_eq!(
        ce("not Person and Professor"),
        ClassExpression::and([ClassExpression::not(p()), q()])
    );
    assert_eq!(
        ce("Person that teaches some ex:Course"),
        ce("Person and teaches some ex:Course")
    );
    assert_eq!(ce("((Person))"), p());
    assert_eq!(ce("Person and Person"), p());
}

#[test]
fn restrictions() {
    let t = || ObjectPropertyExpression::Property(op("teaches"));
    let course = || Box::new(ClassExpression::Class(c("Course")));
    assert_eq!(
        ce("'part of' only Department"),
        ClassExpression::all(op("partOf"), c("Department"))
    );
    assert_eq!(
        ce("inverse teaches some Professor"),
        ClassExpression::some(
            ObjectPropertyExpression::InverseOf(op("teaches")),
            c("Professor")
        )
    );
    assert_eq!(
        ce("inverse (teaches) some Professor"),
        ce("inverse teaches some Professor")
    );
    assert_eq!(
        ce("teaches value 'Ada Lovelace'"),
        ClassExpression::ObjectHasValue(t(), ind("ada"))
    );
    assert_eq!(
        ce("teaches value _:x"),
        ClassExpression::ObjectHasValue(t(), Individual::anonymous("x"))
    );
    assert_eq!(ce("teaches Self"), ClassExpression::ObjectHasSelf(t()));
    assert_eq!(
        ce("teaches min 2 ex:Course"),
        ClassExpression::ObjectMinCardinality(2, t(), Some(course()))
    );
    assert_eq!(
        ce("teaches max 1"),
        ClassExpression::ObjectMaxCardinality(1, t(), None)
    );
    assert_eq!(
        ce("teaches exactly 3 ex:Course and Person"),
        ClassExpression::and([
            ClassExpression::ObjectExactCardinality(3, t(), Some(course())),
            c("Person").into()
        ])
    );
    assert_eq!(
        ce("{'Ada Lovelace', Bob}"),
        ClassExpression::ObjectOneOf([ind("ada"), ind("bob")].into_iter().collect())
    );
    assert_eq!(
        ce("teaches some (teaches some ex:Course)"),
        ClassExpression::some(
            op("teaches"),
            ClassExpression::some(op("teaches"), c("Course"))
        )
    );
}

#[test]
fn data() {
    let age = || vec![dp("age")];
    let facet = |f: &str, v: &str| FacetRestriction {
        facet: crate::model::iri(format!("{}{f}", vocab::XSD)),
        value: Literal::typed(v, vocab::XSD_INTEGER),
    };
    assert_eq!(
        ce("age some xsd:integer"),
        ClassExpression::DataSomeValuesFrom(age(), DataRange::Datatype(xsd("integer")))
    );
    assert_eq!(ce("age some integer"), ce("age some xsd:integer"));
    assert_eq!(
        ce("age some integer[>= 18, < 65]"),
        ClassExpression::DataSomeValuesFrom(
            age(),
            DataRange::DatatypeRestriction(
                xsd("integer"),
                [facet("minInclusive", "18"), facet("maxExclusive", "65")]
                    .into_iter()
                    .collect()
            )
        )
    );
    assert_eq!(
        ce("age some integer[<5,>2]"),
        ClassExpression::DataSomeValuesFrom(
            age(),
            DataRange::DatatypeRestriction(
                xsd("integer"),
                [facet("maxExclusive", "5"), facet("minExclusive", "2")]
                    .into_iter()
                    .collect()
            )
        )
    );
    assert_eq!(
        ce("name value \"Ada\"@en"),
        ClassExpression::DataHasValue(dp("name"), Literal::lang("Ada", "en").unwrap())
    );
    assert_eq!(
        ce("name only {\"a\", \"b\"}"),
        ClassExpression::DataAllValuesFrom(
            vec![dp("name")],
            DataRange::OneOf(
                [Literal::simple("a"), Literal::simple("b")]
                    .into_iter()
                    .collect()
            )
        )
    );
    assert_eq!(
        ce("age value 1.5"),
        ClassExpression::DataHasValue(dp("age"), Literal::typed("1.5", vocab::XSD_DECIMAL))
    );
    assert_eq!(
        ce("age value 2.5e3f"),
        ClassExpression::DataHasValue(dp("age"), Literal::typed("2.5e3", vocab::XSD_FLOAT))
    );
    assert_eq!(
        ce("age value \"7\"^^xsd:short"),
        ClassExpression::DataHasValue(
            dp("age"),
            Literal::typed("7", &format!("{}short", vocab::XSD))
        )
    );
    assert_eq!(
        ce("age value true"),
        ClassExpression::DataHasValue(dp("age"), Literal::typed("true", vocab::XSD_BOOLEAN))
    );
    assert_eq!(
        ce("age min 1 not (integer or string)"),
        ClassExpression::DataMinCardinality(
            1,
            dp("age"),
            Some(DataRange::ComplementOf(Box::new(DataRange::UnionOf(
                [
                    DataRange::Datatype(xsd("integer")),
                    DataRange::Datatype(xsd("string"))
                ]
                .into_iter()
                .collect()
            ))))
        )
    );
    assert_eq!(
        ce("name value \"line\\nbreak \\\"q\\\" \\u00e9 \\ud83d\\ude00\""),
        ClassExpression::DataHasValue(dp("name"), Literal::simple("line\nbreak \"q\" é 😀"))
    );
    // a property the vocabulary does not know takes the kind its filler has
    assert_eq!(
        ce("ex:height some xsd:decimal"),
        ClassExpression::DataSomeValuesFrom(
            vec![dp("height")],
            DataRange::Datatype(xsd("decimal"))
        )
    );
    assert_eq!(
        ce("ex:knows some Person"),
        ClassExpression::some(op("knows"), c("Person"))
    );
}

#[test]
fn punned_property_is_decided_by_filler() {
    assert_eq!(
        ce("Advised some Person"),
        ClassExpression::some(op("Advised"), c("Person"))
    );
    assert_eq!(ce("Advised"), c("Advised").into());
}

#[test]
fn errors_have_spans() {
    let e = err("teaches some Cours");
    assert_eq!(e.kind, ErrorKind::Unknown);
    assert_eq!((e.span.start, e.span.end), (13, 18));
    assert!(e.message.contains("'Cours'"), "{}", e.message);

    let e = err("Course");
    assert_eq!(e.kind, ErrorKind::Ambiguous);
    assert_eq!(e.candidates.len(), 2);
    assert!(
        e.message.contains("ex:Course") && e.message.contains("other:Lecture"),
        "{}",
        e.message
    );
    assert_eq!((e.span.start, e.span.end), (0, 6));

    let e = err("Person and teaches");
    assert_eq!(e.kind, ErrorKind::Kind);
    assert_eq!((e.span.start, e.span.end), (11, 18));
    assert_eq!(
        e.candidates[0].entity,
        Entity::ObjectProperty(op("teaches"))
    );

    let e = err("Person and");
    assert_eq!(e.kind, ErrorKind::Syntax);
    assert_eq!((e.span.start, e.span.end), (10, 10));

    let e = err("(Person or Professor");
    assert_eq!((e.span.start, e.span.end), (20, 20));
    assert!(e.message.contains("')'"));

    let e = err("Person Professor");
    assert_eq!((e.span.start, e.span.end), (7, 16));

    assert_eq!(err("age Self").kind, ErrorKind::Kind);
    assert!(err("teaches min x").message.contains("cardinality"));

    let e = err("name value \"open");
    assert_eq!((e.span.start, e.span.end), (11, 16));

    assert_eq!(err("").kind, ErrorKind::Syntax);
    assert_eq!(err("   ").kind, ErrorKind::Syntax);

    // the span is in UTF-16 code units for a browser
    let text = "'Grad é😀' some Person";
    let e = class_expression(text, &uni()).unwrap_err();
    let j = e.json(text);
    assert_eq!(j["reason"], "unknown");
    assert_eq!(
        (j["start"].as_u64(), j["end"].as_u64()),
        (Some(0), Some(10))
    );
}

#[test]
fn deep_nesting_is_refused() {
    let text = format!("{}Person{}", "(".repeat(300), ")".repeat(300));
    assert!(err(&text).message.contains("nests"));
}

#[test]
fn a_long_lookahead_costs_no_stack() {
    // the filler of a property of unknown kind is looked at before it is read, and a
    // recursive look at 100,000 `not`s overflowed the stack
    // an unknown prefixed name reads as a property of either kind, so its filler
    // decides, and the reader then refuses the nesting
    for opening in ["not ", "("] {
        let text = format!("ex:nobody some {}Person", opening.repeat(100_000));
        let e = err(&text);
        assert!(e.message.contains("nests"), "{opening}: {e}");
    }
}

fn ax(text: &str) -> Axiom {
    axiom(text, &uni()).unwrap_or_else(|e| panic!("{text}: {e}"))
}

#[test]
fn axioms() {
    use AxiomKind as A;
    let t = || ObjectPropertyExpression::Property(op("teaches"));
    assert_eq!(
        ax("Professor SubClassOf teaches some ex:Course"),
        Axiom::sub_class_of(
            c("Professor"),
            ClassExpression::some(op("teaches"), c("Course"))
        )
    );
    assert_eq!(
        ax("Professor and Person SubClassOf Person"),
        Axiom::sub_class_of(
            ClassExpression::and([c("Professor").into(), c("Person").into()]),
            c("Person")
        )
    );
    assert_eq!(
        ax("Professor EquivalentTo teaches some ex:Course"),
        Axiom::equivalent_classes([
            c("Professor").into(),
            ClassExpression::some(op("teaches"), c("Course"))
        ])
    );
    assert_eq!(
        ax("Professor DisjointWith Department"),
        Axiom::disjoint_classes([c("Professor").into(), c("Department").into()])
    );
    assert_eq!(
        ax("'Ada Lovelace' Type Professor"),
        Axiom::class_assertion(c("Professor"), ind("ada"))
    );
    assert_eq!(
        ax("teaches SubPropertyOf 'part of'"),
        Axiom::new(A::SubObjectPropertyOf(
            SubObjectPropertyExpression::Expression(t()),
            ObjectPropertyExpression::Property(op("partOf"))
        ))
    );
    assert_eq!(
        ax("teaches o 'part of' SubPropertyOf teaches"),
        Axiom::new(A::SubObjectPropertyOf(
            SubObjectPropertyExpression::Chain(vec![
                t(),
                ObjectPropertyExpression::Property(op("partOf"))
            ]),
            t()
        ))
    );
    assert_eq!(
        ax("teaches SubPropertyChain teaches o teaches"),
        ax("teaches o teaches SubPropertyOf teaches")
    );
    assert_eq!(
        ax("teaches InverseOf inverse 'part of'"),
        Axiom::new(A::InverseObjectProperties(
            t(),
            ObjectPropertyExpression::InverseOf(op("partOf"))
        ))
    );
    assert_eq!(
        ax("teaches Domain Professor"),
        Axiom::new(A::ObjectPropertyDomain(t(), c("Professor").into()))
    );
    assert_eq!(
        ax("age Range integer[>= 0]"),
        Axiom::new(A::DataPropertyRange(
            dp("age"),
            DataRange::DatatypeRestriction(
                xsd("integer"),
                [FacetRestriction {
                    facet: crate::model::iri(format!("{}minInclusive", vocab::XSD)),
                    value: Literal::typed("0", vocab::XSD_INTEGER),
                }]
                .into_iter()
                .collect()
            )
        ))
    );
    assert_eq!(
        ax("age SubPropertyOf name"),
        Axiom::new(A::SubDataPropertyOf(dp("age"), dp("name")))
    );
    assert_eq!(
        ax("Transitive 'part of'"),
        Axiom::new(A::TransitiveObjectProperty(
            ObjectPropertyExpression::Property(op("partOf"))
        ))
    );
    assert_eq!(
        ax("Functional age"),
        Axiom::new(A::FunctionalDataProperty(dp("age")))
    );
    assert_eq!(
        ax("Functional inverse teaches"),
        Axiom::new(A::FunctionalObjectProperty(
            ObjectPropertyExpression::InverseOf(op("teaches"))
        ))
    );
    assert_eq!(
        ax("Bob teaches 'Ada Lovelace'"),
        Axiom::new(A::ObjectPropertyAssertion(t(), ind("bob"), ind("ada")))
    );
    assert_eq!(
        ax("Bob not age 3"),
        Axiom::new(A::NegativeDataPropertyAssertion(
            dp("age"),
            ind("bob"),
            Literal::typed("3", vocab::XSD_INTEGER)
        ))
    );
    assert_eq!(
        ax("Bob SameAs 'Ada Lovelace'"),
        Axiom::new(A::SameIndividual(
            [ind("bob"), ind("ada")].into_iter().collect()
        ))
    );
    let e = axiom("Professor SubClassOf", &uni()).unwrap_err();
    assert!(e.message.contains("right"), "{}", e.message);
    let e = axiom("teaches o teaches EquivalentTo teaches", &uni()).unwrap_err();
    assert!(e.message.contains("chain"), "{}", e.message);
    let e = axiom("Professor Domain Person", &uni()).unwrap_err();
    assert_eq!(e.kind, ErrorKind::Kind);
}

#[test]
fn a_label_two_kinds_share_is_not_written() {
    // a restriction's property reads both property kinds, so a label an object and a
    // data property share would not read back
    let mut v = uni();
    v.add_entity(Entity::ObjectProperty(op("shares")));
    v.add_label(&format!("{EX}shares"), "shared");
    v.add_entity(Entity::DataProperty(dp("sharedValue")));
    v.add_label(&format!("{EX}sharedValue"), "shared");
    let e = ClassExpression::some(op("shares"), c("Person"));
    let text = Renderer::of_vocabulary(&v).class_expression(&e);
    assert!(text.starts_with("ex:shares"), "{text}");
    assert_eq!(class_expression(&text, &v).unwrap(), e);
}

#[test]
fn renders_labels_quoted_and_round_trips() {
    let v = uni();
    let r = Renderer::of_vocabulary(&v);
    let e = ClassExpression::and([
        ClassExpression::some(op("partOf"), c("GraduateStudent")),
        ClassExpression::not(c("Course")),
        ClassExpression::DataHasValue(dp("name"), Literal::lang("x'y", "en").unwrap()),
    ]);
    let text = r.class_expression(&e);
    // the two classes labelled Course are written by their prefixed names
    assert!(text.contains("not ex:Course"), "{text}");
    assert!(
        text.contains("('part of' some 'Graduate student')"),
        "{text}"
    );
    assert_eq!(class_expression(&text, &v).unwrap(), e);
    assert_eq!(quote_name("Ada"), "Ada");
    assert_eq!(quote_name("and"), "'and'");
    assert_eq!(quote_name("it's"), "'it\\'s'");
    assert_eq!(quote_name("12 monkeys"), "'12 monkeys'");
    assert_eq!(quote_name("a:b"), "'a:b'");
    let mut v2 = uni();
    v2.add_entity(Entity::ObjectProperty(op("its")));
    v2.add_label(&format!("{EX}its"), "it's");
    assert_eq!(
        class_expression("'it\\'s' some Person", &v2).unwrap(),
        ClassExpression::some(op("its"), c("Person"))
    );
}

#[test]
fn of_ontology_reads_labels_and_kinds() {
    let doc = crate::ofn_read::parse(
        "Prefix(:=<http://ex.org/>)
         Ontology(
           Declaration(Class(:Cell))
           Declaration(ObjectProperty(:partOf))
           AnnotationAssertion(rdfs:label :partOf \"part of\"@en)
           AnnotationAssertion(<http://www.w3.org/2004/02/skos/core#prefLabel> :Cell \"living cell\")
         )",
    )
    .unwrap();
    let triples = crate::ofn_read::triples(&doc, &mut crate::write::counter("b"));
    let o = crate::read::read_graph(triples, Default::default());
    let v = Vocabulary::of_ontology(&o, doc.prefixes.clone());
    assert_eq!(
        class_expression("'part of' some 'living cell'", &v).unwrap(),
        ClassExpression::some(op("partOf"), c("Cell"))
    );
    assert_eq!(class_expression(":Cell", &v).unwrap(), c("Cell").into());
}

// ------------------------------------------------------------------ round trips ------

fn st_ope() -> BoxedStrategy<ObjectPropertyExpression> {
    (
        prop::sample::select(vec!["teaches", "partOf", "advisor", "Advised"]),
        any::<bool>(),
    )
        .prop_map(|(p, inv)| {
            if inv {
                ObjectPropertyExpression::InverseOf(op(p))
            } else {
                ObjectPropertyExpression::Property(op(p))
            }
        })
        .boxed()
}

fn st_literal() -> BoxedStrategy<Literal> {
    prop_oneof![
        "[a-z '\"\\\\é\n]{0,6}".prop_map(Literal::simple),
        ("[a-z ]{0,4}", prop::sample::select(vec!["en", "fr-CA"]))
            .prop_map(|(s, l)| Literal::lang(s, l).unwrap()),
        (-50i64..50).prop_map(|n| Literal::typed(n.to_string(), vocab::XSD_INTEGER)),
        (0u32..100, 0u32..100)
            .prop_map(|(a, b)| Literal::typed(format!("{a}.{b}"), vocab::XSD_DECIMAL)),
        any::<bool>().prop_map(|b| Literal::typed(b.to_string(), vocab::XSD_BOOLEAN)),
        "[0-9]{1,3}".prop_map(|s| Literal::typed(s, &format!("{}short", vocab::XSD))),
    ]
    .boxed()
}

fn st_data_range() -> BoxedStrategy<DataRange> {
    let base: BoxedStrategy<DataRange> = prop_oneof![
        prop::sample::select(vec!["integer", "string", "dateTime"])
            .prop_map(|s| DataRange::Datatype(xsd(s))),
        prop::collection::btree_set(st_literal(), 1..3).prop_map(DataRange::OneOf),
        (
            prop::sample::select(vec!["minInclusive", "maxExclusive", "length", "pattern"]),
            -5i64..5
        )
            .prop_map(|(f, n)| DataRange::DatatypeRestriction(
                xsd("integer"),
                [FacetRestriction {
                    facet: crate::model::iri(format!("{}{f}", vocab::XSD)),
                    value: Literal::typed(n.to_string(), vocab::XSD_INTEGER),
                }]
                .into_iter()
                .collect()
            )),
    ]
    .boxed();
    base.prop_recursive(2, 6, 2, |inner| {
        prop_oneof![
            inner
                .clone()
                .prop_map(|r| DataRange::ComplementOf(Box::new(r))),
            prop::collection::btree_set(inner.clone(), 2..3).prop_map(DataRange::UnionOf),
            prop::collection::btree_set(inner, 2..3).prop_map(DataRange::IntersectionOf),
        ]
        .boxed()
    })
    .prop_filter("n-ary ranges of two", |r| match r {
        DataRange::UnionOf(s) | DataRange::IntersectionOf(s) => s.len() >= 2,
        _ => true,
    })
    .boxed()
}

fn leaf() -> BoxedStrategy<ClassExpression> {
    prop_oneof![
        prop::sample::select(vec![
            "Person",
            "Professor",
            "GraduateStudent",
            "Department",
            "New"
        ])
        .prop_map(|s| ClassExpression::Class(c(s))),
        Just(ClassExpression::Class(Class::thing())),
        Just(ClassExpression::Class(Class::new(format!(
            "{OTHER}Lecture"
        )))),
        prop::collection::btree_set(prop::sample::select(vec!["ada", "bob"]).prop_map(ind), 1..3)
            .prop_map(ClassExpression::ObjectOneOf),
        (st_ope(), prop::sample::select(vec!["ada", "bob"]))
            .prop_map(|(p, i)| ClassExpression::ObjectHasValue(p, ind(i))),
        st_ope().prop_map(ClassExpression::ObjectHasSelf),
        (prop::sample::select(vec!["age", "name"]), st_data_range())
            .prop_map(|(p, r)| ClassExpression::DataSomeValuesFrom(vec![dp(p)], r)),
        (prop::sample::select(vec!["age", "name"]), st_literal())
            .prop_map(|(p, l)| ClassExpression::DataHasValue(dp(p), l)),
        (0u32..4, prop::option::of(st_data_range()))
            .prop_map(|(n, r)| ClassExpression::DataMaxCardinality(n, dp("age"), r)),
    ]
    .boxed()
}

fn expression() -> BoxedStrategy<ClassExpression> {
    leaf()
        .prop_recursive(4, 24, 3, |inner| {
            prop_oneof![
                prop::collection::btree_set(inner.clone(), 2..4)
                    .prop_map(ClassExpression::ObjectIntersectionOf),
                prop::collection::btree_set(inner.clone(), 2..4)
                    .prop_map(ClassExpression::ObjectUnionOf),
                inner.clone().prop_map(ClassExpression::not),
                (st_ope(), inner.clone()).prop_map(|(p, c)| ClassExpression::some(p, c)),
                (st_ope(), inner.clone()).prop_map(|(p, c)| ClassExpression::all(p, c)),
                (0u32..5, st_ope(), prop::option::of(inner)).prop_map(|(n, p, c)| {
                    ClassExpression::ObjectMinCardinality(n, p, c.map(Box::new))
                }),
            ]
            .boxed()
        })
        .prop_filter("n-ary expressions of two", |e| match e {
            ClassExpression::ObjectIntersectionOf(s) | ClassExpression::ObjectUnionOf(s) => {
                s.len() >= 2
            }
            _ => true,
        })
        .boxed()
}

fn axiom_strategy() -> BoxedStrategy<Axiom> {
    prop_oneof![
        (expression(), expression()).prop_map(|(a, b)| Axiom::sub_class_of(a, b)),
        (expression(), expression())
            .prop_filter("two classes", |(a, b)| a != b)
            .prop_map(|(a, b)| Axiom::equivalent_classes([a, b])),
        (expression(), prop::sample::select(vec!["ada", "bob"]))
            .prop_map(|(a, i)| Axiom::class_assertion(a, ind(i))),
        (st_ope(), expression())
            .prop_map(|(p, c)| Axiom::new(AxiomKind::ObjectPropertyRange(p, c))),
        st_data_range().prop_map(|r| Axiom::new(AxiomKind::DataPropertyRange(dp("age"), r))),
        (st_ope(), st_ope()).prop_map(|(a, b)| Axiom::new(AxiomKind::SubObjectPropertyOf(
            SubObjectPropertyExpression::Chain(vec![a, b]),
            ObjectPropertyExpression::Property(op("teaches"))
        ))),
        (st_literal(), prop::sample::select(vec!["age", "name"]))
            .prop_map(|(l, p)| Axiom::new(AxiomKind::DataPropertyAssertion(dp(p), ind("bob"), l))),
    ]
    .boxed()
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(400))]

    #[test]
    fn class_expressions_round_trip(e in expression(), labels in any::<bool>()) {
        let v = uni();
        let r = if labels {
            Renderer::of_vocabulary(&v)
        } else {
            Renderer::new(v.prefixes().iter().cloned())
        };
        let text = r.class_expression(&e);
        let back = class_expression(&text, &v);
        prop_assert_eq!(back.as_ref().ok(), Some(&e), "text: {} error: {:?}", text, back.clone().err());
    }

    #[test]
    fn axioms_round_trip(a in axiom_strategy(), labels in any::<bool>()) {
        let v = uni();
        let r = if labels {
            Renderer::of_vocabulary(&v)
        } else {
            Renderer::new(v.prefixes().iter().cloned())
        };
        let text = r.axiom(&a).expect("a form");
        let back = axiom(&text, &v);
        prop_assert_eq!(back.as_ref().ok(), Some(&a), "text: {} error: {:?}", text, back.clone().err());
    }

    #[test]
    fn arbitrary_text_never_panics(s in "[ -~é]{0,40}") {
        let v = uni();
        let _ = class_expression(&s, &v);
        let _ = axiom(&s, &v);
        let _ = data_range(&s, &v);
    }
}
