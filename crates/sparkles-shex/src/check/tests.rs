//! Schemas built by hand (the negative-structure cases of shexTest among them).

use super::*;
use crate::ast::*;

pub(crate) const EX: &str = "http://example.org/";

pub(crate) fn l(s: &str) -> Label {
    Label::Iri(format!("{EX}{s}"))
}

pub(crate) fn r(s: &str) -> ShapeExpr {
    ShapeExpr::Ref(l(s))
}

pub(crate) fn not(e: ShapeExpr) -> ShapeExpr {
    ShapeExpr::Not(Box::new(e))
}

pub(crate) fn and(v: Vec<ShapeExpr>) -> ShapeExpr {
    ShapeExpr::And(v)
}

pub(crate) fn or(v: Vec<ShapeExpr>) -> ShapeExpr {
    ShapeExpr::Or(v)
}

pub(crate) fn nc(nc: NodeConstraint) -> ShapeExpr {
    ShapeExpr::Nc(Box::new(nc))
}

/// `[<v1> <v2> …]`
pub(crate) fn values(vs: &[&str]) -> ShapeExpr {
    nc(NodeConstraint {
        values: Some(
            vs.iter()
                .map(|v| ValueSetValue::Object(ObjectValue::Iri(format!("{EX}{v}"))))
                .collect(),
        ),
        ..Default::default()
    })
}

pub(crate) fn shape(te: Option<TripleExpr>) -> ShapeExpr {
    shape_x(&[], te)
}

/// A shape with EXTRA predicates.
pub(crate) fn shape_x(extra: &[&str], te: Option<TripleExpr>) -> ShapeExpr {
    ShapeExpr::Shape(Box::new(Shape {
        extra: extra.iter().map(|p| format!("{EX}{p}")).collect(),
        expression: te,
        ..Default::default()
    }))
}

pub(crate) fn tc(p: &str, v: Option<ShapeExpr>) -> TripleConstraint {
    TripleConstraint {
        id: None,
        inverse: None,
        predicate: format!("{EX}{p}"),
        value_expr: v.map(Box::new),
        min: None,
        max: None,
        sem_acts: vec![],
        annotations: vec![],
    }
}

pub(crate) fn t(p: &str, v: Option<ShapeExpr>) -> TripleExpr {
    TripleExpr::Tc(tc(p, v))
}

/// A triple constraint with a cardinality (`max` -1: unbounded).
pub(crate) fn tn(p: &str, v: Option<ShapeExpr>, min: u32, max: i64) -> TripleExpr {
    TripleExpr::Tc(TripleConstraint {
        min: Some(min),
        max: Some(max),
        ..tc(p, v)
    })
}

/// A labelled triple constraint (`$label`).
pub(crate) fn tl(label: &str, p: &str, v: Option<ShapeExpr>) -> TripleExpr {
    TripleExpr::Tc(TripleConstraint {
        id: Some(l(label)),
        ..tc(p, v)
    })
}

pub(crate) fn group(exprs: Vec<TripleExpr>) -> Group {
    Group {
        id: None,
        exprs,
        min: None,
        max: None,
        sem_acts: vec![],
        annotations: vec![],
    }
}

pub(crate) fn each(exprs: Vec<TripleExpr>) -> TripleExpr {
    TripleExpr::EachOf(group(exprs))
}

pub(crate) fn one(exprs: Vec<TripleExpr>) -> TripleExpr {
    TripleExpr::OneOf(group(exprs))
}

pub(crate) fn inc(s: &str) -> TripleExpr {
    TripleExpr::Include(l(s))
}

pub(crate) fn schema(decls: Vec<(&str, ShapeExpr)>) -> Schema {
    Schema {
        prefixes: vec![("ex".into(), EX.into())],
        shapes: decls
            .into_iter()
            .map(|(label, expr)| ShapeDecl {
                label: l(label),
                expr,
            })
            .collect(),
        ..Default::default()
    }
}

fn err(s: &Schema) -> String {
    check(s).expect_err("a structure error").message
}

/// shexTest `negativeStructure`: every schema is rejected, with the reason.
#[test]
fn negative_structure() {
    let some = |e| Some(e);
    let cases: Vec<(&str, Schema, &str)> = vec![
        (
            "1MissingRef",
            schema(vec![("S1", shape(some(t("p1", Some(r("S2"))))))]),
            "reference to undefined shape ex:S2",
        ),
        (
            "1focusMissingRefdot",
            schema(vec![("S1", and(vec![r("S2"), shape(None)]))]),
            "reference to undefined shape ex:S2",
        ),
        (
            "1focusRefANDSelfdot",
            schema(vec![
                ("S1", and(vec![r("S2"), r("S1"), shape(None)])),
                ("S2", shape(None)),
            ]),
            "reference cycle without a shape: ex:S1 -> ex:S1",
        ),
        (
            "includeExpressionNotFound",
            schema(vec![(
                "S",
                shape(some(each(vec![inc("S1"), t("p", None)]))),
            )]),
            "inclusion of undefined triple expression ex:S1",
        ),
        (
            "includeSimpleShape",
            schema(vec![
                ("S", shape(some(each(vec![inc("S1"), t("p1", None)])))),
                ("S1", shape(some(t("p2", None)))),
            ]),
            "&ex:S1 names a shape expression, not a triple expression",
        ),
        (
            "includeNonSimpleShape",
            schema(vec![
                ("S", shape(some(each(vec![inc("S1"), t("p", None)])))),
                ("S1", values(&["s"])),
            ]),
            "&ex:S1 names a shape expression, not a triple expression",
        ),
        (
            "1ShapeProductionCollision",
            schema(vec![
                ("S1", shape(some(tl("S1", "p1", None)))),
                ("S2", values(&["x"])),
            ]),
            "ex:S1 labels both a shape expression and a triple expression",
        ),
        (
            "Cycle1Negation1",
            schema(vec![(
                "S",
                not(shape(some(t("a", Some(shape(some(t("b", Some(r("S")))))))))),
            )]),
            "negated reference cycle: ex:S -[NOT]-> ex:S",
        ),
        (
            "Cycle1Negation2",
            schema(vec![(
                "S",
                shape(some(t("a", Some(shape(some(t("b", Some(not(r("S")))))))))),
            )]),
            "negated reference cycle: ex:S -[NOT]-> ex:S",
        ),
        (
            "Cycle1Negation3",
            schema(vec![(
                "S",
                shape(some(t("a", Some(not(shape(some(t("b", Some(r("S")))))))))),
            )]),
            "negated reference cycle: ex:S -[NOT]-> ex:S",
        ),
        (
            "TwoNegation",
            schema(vec![
                ("S", and(vec![not(r("T")), not(r("U"))])),
                ("T", shape(some(t("a", Some(r("S")))))),
                ("U", shape(some(t("b", None)))),
            ]),
            "negated reference cycle: ex:S -[NOT]-> ex:T -> ex:S",
        ),
        (
            "TwoNegation2",
            schema(vec![
                ("S", and(vec![not(r("T")), r("U")])),
                ("T", shape(some(t("a", Some(not(r("S"))))))),
                ("U", shape(some(t("b", Some(r("S")))))),
            ]),
            "negated reference cycle: ex:S -[NOT]-> ex:T -[NOT]-> ex:S",
        ),
        (
            "Cycle2Negation",
            schema(vec![("S", not(shape(some(t("a", Some(r("S")))))))]),
            "negated reference cycle: ex:S -[NOT]-> ex:S",
        ),
        (
            "Cycle2Extra",
            schema(vec![("S", shape_x(&["a"], some(t("a", Some(r("S"))))))]),
            "negated reference cycle: ex:S -[EXTRA ex:a]-> ex:S",
        ),
    ];
    for (name, s, msg) in cases {
        assert_eq!(err(&s), msg, "{name}");
    }
}

/// The messages of the acceptance examples, byte for byte.
#[test]
fn negation_messages() {
    let ex = |s: &str| format!("http://ex.org/{s}");
    let mk = |extra: Vec<String>, v: ShapeExpr| Schema {
        prefixes: vec![("ex".into(), "http://ex.org/".into())],
        shapes: vec![ShapeDecl {
            label: Label::Iri(ex("S")),
            expr: ShapeExpr::Shape(Box::new(Shape {
                extra,
                expression: Some(TripleExpr::Tc(TripleConstraint {
                    predicate: ex("a"),
                    value_expr: Some(Box::new(v)),
                    ..tc("a", None)
                })),
                ..Default::default()
            })),
        }],
        ..Default::default()
    };
    let s_ref = || ShapeExpr::Ref(Label::Iri(ex("S")));
    assert_eq!(
        err(&mk(vec![], not(s_ref()))),
        "negated reference cycle: ex:S -[NOT]-> ex:S"
    );
    assert_eq!(
        err(&mk(vec![ex("a")], s_ref())),
        "negated reference cycle: ex:S -[EXTRA ex:a]-> ex:S"
    );
    // without a prefix for them, IRIs are written in full
    let mut s = mk(vec![ex("a")], s_ref());
    s.prefixes.clear();
    assert_eq!(
        err(&s),
        "negated reference cycle: <http://ex.org/S> -[EXTRA <http://ex.org/a>]-> <http://ex.org/S>"
    );
}

/// shexTest schemas with recursion but no negated cycle.
#[test]
fn positive_recursion() {
    let some = |e| Some(e);
    let ok = [
        // Cycle2NoNegation
        schema(vec![("S", shape(some(t("a", Some(r("S"))))))]),
        // CycleNoNegation
        schema(vec![(
            "S",
            shape(some(t("a", Some(shape(some(t("b", Some(r("S"))))))))),
        )]),
        // NoNegation
        schema(vec![
            ("S", and(vec![r("T"), r("U")])),
            (
                "T",
                shape(some(each(vec![t("a", Some(r("S"))), t("c", None)]))),
            ),
            ("U", shape(some(t("b", None)))),
        ]),
        // NoNegation2
        schema(vec![
            ("S", shape(some(t("a", Some(and(vec![r("T"), r("U")])))))),
            ("T", shape(some(t("b", Some(r("S")))))),
            ("U", shape(some(t("c", Some(r("T")))))),
        ]),
        // OneNegation
        schema(vec![
            ("S", and(vec![r("T"), not(r("U"))])),
            ("T", shape(some(t("a", Some(r("S")))))),
            ("U", shape(some(t("b", None)))),
        ]),
        // a double negation is positive
        schema(vec![("S", shape(some(t("a", Some(not(not(r("S"))))))))]),
        // EXTRA on another predicate
        schema(vec![(
            "S",
            shape_x(&["b"], some(each(vec![t("a", Some(r("S"))), t("b", None)]))),
        )]),
    ];
    for (i, s) in ok.iter().enumerate() {
        if let Err(e) = check(s) {
            panic!("schema {i}: {e}");
        }
    }
}

#[test]
fn strata_order() {
    let some = |e| Some(e);
    // S reads T negatively; T and U are mutually recursive; V is read by U positively
    let s = schema(vec![
        ("S", and(vec![not(r("T")), shape(None)])),
        ("T", shape(some(t("a", Some(r("U")))))),
        ("U", shape(some(t("b", Some(or(vec![r("T"), r("V")])))))),
        ("V", shape(some(t("c", None)))),
        ("W", shape_x(&["d"], some(t("d", Some(r("S")))))),
    ]);
    let c = check(&s).unwrap();
    let [s_, t_, u_, v_, w_] = c.strata[..] else {
        panic!()
    };
    assert_eq!(t_, u_, "one component, one stratum");
    assert!(v_ <= u_, "a positive edge points no higher");
    assert!(s_ > t_, "a negative edge points strictly lower");
    assert!(w_ > s_, "an EXTRA value is read negatively");
    assert_eq!(c.num_strata, w_ + 1);
    assert_eq!((t_, v_), (0, 0));
    // a value expression is in the stratum of what it reads
    let ShapeExpr::Shape(w) = &s.shapes[4].expr else {
        panic!()
    };
    let Some(TripleExpr::Tc(tcw)) = &w.expression else {
        panic!()
    };
    assert_eq!(c.value_stratum(tcw.value_expr.as_deref().unwrap()), s_);
}

#[test]
fn labels_and_inclusions() {
    let some = |e| Some(e);
    // duplicates
    let s = schema(vec![("S", shape(None)), ("S", shape(None))]);
    assert_eq!(err(&s), "duplicate shape label ex:S");
    let s = schema(vec![
        ("S", shape(some(tl("e", "p", None)))),
        ("T", shape(some(tl("e", "q", None)))),
    ]);
    assert_eq!(err(&s), "duplicate triple expression label ex:e");
    // an inclusion that includes itself
    let s = schema(vec![(
        "S",
        shape(some(TripleExpr::EachOf(Group {
            id: Some(l("e")),
            ..group(vec![t("p", None), inc("e")])
        }))),
    )]);
    assert_eq!(err(&s), "inclusion cycle: ex:e -> ex:e");
    // through a value expression it is fine: a recursive anonymous shape
    let s = schema(vec![(
        "S",
        shape(some(tl("e", "p", Some(shape(some(inc("e"))))))),
    )]);
    check(&s).unwrap();
    // a reference through an inclusion counts for the including shape
    let s = schema(vec![
        ("S", shape_x(&["a"], some(inc("e")))),
        ("T", shape(some(tl("e", "a", Some(r("S")))))),
    ]);
    assert_eq!(
        err(&s),
        "negated reference cycle: ex:S -[EXTRA ex:a]-> ex:S"
    );
}

#[test]
fn externals_and_start() {
    let some = |e| Some(e);
    // an EXTERNAL shape nothing references is fine
    let s = schema(vec![("E", ShapeExpr::External), ("S", shape(None))]);
    check(&s).unwrap();
    let s = schema(vec![
        ("E", ShapeExpr::External),
        ("S", shape(some(t("p", Some(r("E")))))),
    ]);
    assert_eq!(err(&s), "external shape ex:E has no definition");
    // START is checked like a declaration
    let mut s = schema(vec![("S", shape(None))]);
    s.start = Some(r("T"));
    assert_eq!(err(&s), "reference to undefined shape ex:T");
    s.start = Some(not(r("S")));
    let c = check(&s).unwrap();
    assert_eq!(c.start_stratum, 1);
}

#[test]
fn facets() {
    let xsd = |l: &str| format!("http://www.w3.org/2001/XMLSchema#{l}");
    let ok = |n: NodeConstraint| check_facets(&n);
    assert_eq!(
        ok(NodeConstraint {
            datatype: Some("http://a.example/dt1".into()),
            max_inclusive: Some(NumericLiteral::Integer("5".into())),
            ..Default::default()
        }),
        Err(
            "numeric facet MAXINCLUSIVE on <http://a.example/dt1>, which is not an XSD numeric \
             datatype"
                .into()
        )
    );
    assert!(
        ok(NodeConstraint {
            datatype: Some(xsd("string")),
            total_digits: Some(3),
            ..Default::default()
        })
        .is_err()
    );
    for dt in ["integer", "decimal", "double", "float", "unsignedByte"] {
        ok(NodeConstraint {
            datatype: Some(xsd(dt)),
            min_inclusive: Some(NumericLiteral::Decimal("4.5".into())),
            total_digits: Some(3),
            ..Default::default()
        })
        .unwrap();
    }
    // numeric facets without a datatype, string facets on any datatype
    ok(NodeConstraint {
        node_kind: Some(NodeKind::Literal),
        min_inclusive: Some(NumericLiteral::Integer("5".into())),
        ..Default::default()
    })
    .unwrap();
    ok(NodeConstraint {
        datatype: Some(xsd("integer")),
        length: Some(3),
        ..Default::default()
    })
    .unwrap();
    // bounds are numbers of their kind
    for (n, good) in [
        (NumericLiteral::Integer("-5".into()), true),
        (NumericLiteral::Decimal("05.0".into()), true),
        (NumericLiteral::Double("1.5E3".into()), true),
        (NumericLiteral::Integer("V".into()), false),
        (NumericLiteral::Integer("4.5".into()), false),
        (NumericLiteral::Decimal("".into()), false),
    ] {
        let r = ok(NodeConstraint {
            min_exclusive: Some(n.clone()),
            ..Default::default()
        });
        assert_eq!(r.is_ok(), good, "{n:?}");
    }
    // the checks run over the whole schema
    let s = schema(vec![(
        "S",
        shape(Some(t(
            "p",
            Some(nc(NodeConstraint {
                datatype: Some(xsd("string")),
                max_exclusive: Some(NumericLiteral::Integer("5".into())),
                ..Default::default()
            })),
        ))),
    )]);
    assert!(err(&s).starts_with("numeric facet MAXEXCLUSIVE"));
}

#[test]
fn prefixed_names() {
    let p: PrefixMap = vec![
        ("ex".into(), "http://ex.org/".into()),
        ("exa".into(), "http://ex.org/a/".into()),
        ("".into(), "http://d.org/".into()),
    ];
    assert_eq!(show_iri("http://ex.org/S", &p), "ex:S");
    assert_eq!(show_iri("http://ex.org/a/b", &p), "exa:b");
    assert_eq!(show_iri("http://d.org/x", &p), ":x");
    assert_eq!(show_iri("http://ex.org/a/b/c", &p), "<http://ex.org/a/b/c>");
    assert_eq!(show_iri("http://ex.org/x.", &p), "<http://ex.org/x.>");
    assert_eq!(show_label(&Label::BNode("b0".into()), &p), "_:b0");
}

#[test]
fn tarjan_components() {
    // 0 -> 1 -> 2 -> 0, 2 -> 3, 4 alone
    let edges = vec![vec![1], vec![2], vec![0, 3], vec![], vec![]];
    let s = tarjan(&edges);
    assert_eq!(s.of[0], s.of[1]);
    assert_eq!(s.of[1], s.of[2]);
    assert_ne!(s.of[2], s.of[3]);
    // sinks first
    assert!(s.of[3] < s.of[0]);
    assert_eq!(find_cycle(&edges), Some(vec![0, 1, 2, 0]));
    assert_eq!(find_cycle(&[vec![1], vec![]]), None);
    assert_eq!(find_cycle(&[vec![0]]), Some(vec![0, 0]));
}
