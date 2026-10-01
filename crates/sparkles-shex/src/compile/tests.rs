use super::*;
use crate::ShapeLabel;
use crate::ast::*;
use crate::check::check;
use crate::check::tests::*;

fn compiled(s: &Schema) -> CompiledSchema {
    let c = check(s).unwrap_or_else(|e| panic!("{e}"));
    compile(s, &c).unwrap_or_else(|e| panic!("{e}"))
}

fn kind(c: &CompiledSchema, label: &str) -> PairKind {
    c.label(&format!("{EX}{label}")).expect("declared")
}

/// The shape of a declaration whose expression is a shape.
fn shape_of<'c>(c: &'c CompiledSchema, label: &str) -> &'c ShapeIr {
    let se = c.ir().pairs[kind(c, label).index()].se;
    match c.ir().ses[se.index()] {
        Se::Shape(id) => &c.ir().shapes[id.index()],
        ref other => panic!("{other:?}"),
    }
}

#[test]
fn reference_chains_collapse() {
    let s = schema(vec![
        ("A", r("B")),
        ("B", r("C")),
        ("C", shape(Some(t("p", Some(r("A")))))),
    ]);
    let c = compiled(&s);
    let k = kind(&c, "C");
    assert_eq!(kind(&c, "A"), k);
    assert_eq!(kind(&c, "B"), k);
    // one pair kind for the three labels; the value `@A` reads C's pairs directly
    assert_eq!(c.ir().pairs.len(), 1);
    let sh = shape_of(&c, "A");
    assert_eq!(sh.tcs[0].pair, Some(k));
    assert!(matches!(c.ir().ses[sh.tcs[0].value.unwrap().index()], Se::Ref(x) if x == k));
    assert_eq!(c.ir().pairs[k.index()].label, Some(l("C")));
}

#[test]
fn value_expressions_are_pair_kinds() {
    let s = schema(vec![
        (
            "S",
            shape(Some(each(vec![
                t("a", Some(values(&["x"]))),
                t("b", Some(not(r("T")))),
                t("c", None),
            ]))),
        ),
        ("T", shape(None)),
    ]);
    let c = compiled(&s);
    let sh = shape_of(&c, "S");
    let (a, b, cc) = (&sh.tcs[0], &sh.tcs[1], &sh.tcs[2]);
    assert!(matches!(c.ir().ses[a.value.unwrap().index()], Se::Nc(_)));
    assert!(matches!(c.ir().ses[b.value.unwrap().index()], Se::Not(_)));
    assert_eq!(cc.value, None);
    assert_eq!(cc.pair, None);
    let (ka, kb) = (a.pair.unwrap(), b.pair.unwrap());
    assert_ne!(ka, kb);
    let info = &c.ir().pairs[kb.index()];
    assert_eq!(info.se, b.value.unwrap());
    assert_eq!(info.label, None);
    // NOT @T is decided after T
    assert!(info.stratum > c.ir().pairs[kind(&c, "T").index()].stratum);
    assert_eq!(c.ir().strata, 2);
}

#[test]
fn includes_expand_per_occurrence() {
    // S { &e ; &e } with e = (p @T ; q .)
    let e = TripleExpr::EachOf(Group {
        id: Some(l("e")),
        ..group(vec![t("p", Some(r("T"))), t("q", None)])
    });
    let s = schema(vec![
        ("D", shape(Some(e))),
        ("S", shape(Some(each(vec![inc("e"), inc("e")])))),
        ("T", shape(None)),
    ]);
    let c = compiled(&s);
    let sh = shape_of(&c, "S");
    // spliced into one EachOf of four triple constraints
    assert_eq!(sh.tcs.len(), 4);
    assert_eq!(sh.te.len(), 5);
    let Te::EachOf { kids, .. } = &sh.te[sh.root.unwrap().index()] else {
        panic!()
    };
    assert_eq!(kids.len(), 4);
    assert_eq!(sh.preds.len(), 2);
    assert_eq!(sh.preds[0].2.as_slice(), &[TcId(0), TcId(2)]);
    assert_eq!(sh.class, ShapeClass::Ambiguous);
    // the instances share their value's pair kind
    assert_eq!(sh.tcs[0].pair, sh.tcs[2].pair);
    assert_eq!(shape_of(&c, "D").class, ShapeClass::Flat);
}

#[test]
fn recursive_inclusion_through_a_value_is_finite() {
    // S { $e ex:p { &e } ? }
    let mut tc = tc("p", Some(shape(Some(inc("e")))));
    tc.id = Some(l("e"));
    tc.min = Some(0);
    let s = schema(vec![("S", shape(Some(TripleExpr::Tc(tc))))]);
    let c = compiled(&s);
    // S and the anonymous shape; the anonymous shape's value is itself
    assert_eq!(c.ir().shapes.len(), 2);
    let inner = &c.ir().shapes[0];
    let v = inner.tcs[0].value.unwrap();
    assert!(matches!(c.ir().ses[v.index()], Se::Shape(ShapeId(0))));
    assert_eq!(shape_of(&c, "S").tcs[0].value, Some(v));
}

#[test]
fn max_occurrences() {
    // S { (a ; b* ; (c{2} | d){3}){2} ; e{0} }
    let inner = TripleExpr::OneOf(Group {
        min: Some(3),
        max: Some(3),
        ..group(vec![tn("c", None, 2, 2), t("d", None)])
    });
    let mid = TripleExpr::EachOf(Group {
        min: Some(2),
        max: Some(2),
        ..group(vec![t("a", None), tn("b", None, 0, -1), inner])
    });
    let s = schema(vec![(
        "S",
        shape(Some(each(vec![mid, tn("e", None, 0, 0)]))),
    )]);
    let c = compiled(&s);
    let sh = shape_of(&c, "S");
    let occ: Vec<(String, Option<u64>)> = sh
        .tcs
        .iter()
        .zip(&sh.max_occ)
        .map(|(t, o)| (t.pred.trim_start_matches(EX).to_string(), *o))
        .collect();
    assert_eq!(
        occ,
        [
            ("a".into(), Some(2)),
            ("b".into(), None),
            ("c".into(), Some(12)),
            ("d".into(), Some(6)),
            ("e".into(), Some(0)),
        ]
    );
    assert_eq!(sh.class, ShapeClass::Deterministic);
}

#[test]
fn classification() {
    let class = |te: Option<TripleExpr>| {
        let c = compiled(&schema(vec![("S", shape(te))]));
        shape_of(&c, "S").class
    };
    assert_eq!(class(None), ShapeClass::Flat);
    assert_eq!(class(Some(tn("a", None, 0, -1))), ShapeClass::Flat);
    assert_eq!(
        class(Some(each(vec![t("a", None), tn("b", None, 1, 3)]))),
        ShapeClass::Flat
    );
    // nested plain groups are spliced
    assert_eq!(
        class(Some(each(vec![
            t("a", None),
            each(vec![t("b", None), t("c", None)])
        ]))),
        ShapeClass::Flat
    );
    // a group cardinality or a OneOf: deterministic
    let g = TripleExpr::EachOf(Group {
        max: Some(-1),
        ..group(vec![t("a", None), t("b", None)])
    });
    assert_eq!(class(Some(g)), ShapeClass::Deterministic);
    assert_eq!(
        class(Some(one(vec![t("a", None), t("b", None)]))),
        ShapeClass::Deterministic
    );
    // the same predicate twice: ambiguous; an inverse one is another (predicate, direction)
    assert_eq!(
        class(Some(each(vec![t("a", None), t("a", Some(values(&["x"])))]))),
        ShapeClass::Ambiguous
    );
    let inv = TripleExpr::Tc(TripleConstraint {
        inverse: Some(true),
        ..tc("a", None)
    });
    assert_eq!(class(Some(each(vec![t("a", None), inv]))), ShapeClass::Flat);
    // a group with semantic actions is kept
    let acts = TripleExpr::EachOf(Group {
        sem_acts: vec![SemAct {
            name: "http://shex.io/extensions/Test/".into(),
            code: Some(" print(o) ".into()),
        }],
        ..group(vec![t("a", None), t("b", None)])
    });
    assert_eq!(class(Some(acts)), ShapeClass::Deterministic);
}

#[test]
fn shapes_keep_their_parts() {
    let s = Schema {
        start_acts: vec![SemAct {
            name: "http://ex.org/x".into(),
            code: None,
        }],
        ..schema(vec![(
            "S",
            ShapeExpr::Shape(Box::new(Shape {
                closed: Some(true),
                extra: vec![format!("{EX}b")],
                expression: Some(TripleExpr::Tc(TripleConstraint {
                    inverse: Some(true),
                    ..tc("a", None)
                })),
                ..Default::default()
            })),
        )])
    };
    let c = compiled(&s);
    let sh = shape_of(&c, "S");
    assert!(sh.closed);
    assert_eq!(sh.extra.as_slice(), &[format!("{EX}b")]);
    assert_eq!(sh.tcs[0].dir, Dir::In);
    assert_eq!((sh.tcs[0].min, sh.tcs[0].max), (1, Some(1)));
    assert_eq!(c.ir().start_acts.len(), 1);
    assert_eq!(c.prefixes(), &vec![("ex".to_string(), EX.to_string())]);
}

#[test]
fn start_and_map_labels() {
    let mut s = schema(vec![("S", shape(None))]);
    let c = compiled(&s);
    assert!(!c.has_start());
    assert_eq!(
        shape_label(&c, &ShapeLabel::Start).unwrap_err().message,
        "the schema has no start shape"
    );
    assert_eq!(
        shape_label(&c, &ShapeLabel::Iri(format!("{EX}T")))
            .unwrap_err()
            .message,
        "undefined shape label ex:T"
    );
    assert_eq!(
        shape_label(&c, &ShapeLabel::Iri(format!("{EX}S"))).unwrap(),
        kind(&c, "S")
    );
    // START = @S reads S's pairs
    s.start = Some(r("S"));
    let c = compiled(&s);
    assert_eq!(c.ir().start, Some(kind(&c, "S")));
    assert_eq!(shape_label(&c, &ShapeLabel::Start).unwrap(), kind(&c, "S"));
    // an inline START has its own pair kind
    s.start = Some(not(r("S")));
    let c = compiled(&s);
    let k = c.ir().start.unwrap();
    assert_ne!(k, kind(&c, "S"));
    assert_eq!(c.ir().pairs[k.index()].stratum, 1);
    // an EXTERNAL shape nothing references compiles
    let s = schema(vec![("E", ShapeExpr::External)]);
    let c = compiled(&s);
    assert!(matches!(
        c.ir().ses[c.ir().pairs[kind(&c, "E").index()].se.index()],
        Se::External
    ));
}

#[test]
fn patterns() {
    let pat = |p: &str, f: Option<&str>| {
        let s = schema(vec![(
            "S",
            nc(NodeConstraint {
                pattern: Some(p.into()),
                flags: f.map(Into::into),
                ..Default::default()
            }),
        )]);
        let c = check(&s).unwrap();
        compile(&s, &c).map(|c| c.ir().ncs[0].regex.as_ref().unwrap().is_match("AB"))
    };
    assert!(pat("^ab$", Some("i")).unwrap());
    assert!(!pat("^ab$", None).unwrap());
    assert!(
        pat("(", None)
            .unwrap_err()
            .message
            .starts_with("invalid pattern /(/")
    );
}

#[test]
fn every_pair_kind_has_an_expression() {
    let s = schema(vec![
        (
            "S",
            shape(Some(each(vec![
                t("a", Some(r("T"))),
                t("b", Some(shape(Some(t("c", Some(values(&["x"]))))))),
            ]))),
        ),
        ("T", or(vec![r("S"), values(&["y"])])),
    ]);
    let c = compiled(&s);
    for p in &c.ir().pairs {
        assert!(p.se.index() < c.ir().ses.len());
        assert!(p.stratum < c.ir().strata.max(1));
    }
    assert!(c.ir().ses.iter().all(|se| match se {
        Se::Ref(k) => k.index() < c.ir().pairs.len(),
        Se::Shape(s) => s.index() < c.ir().shapes.len(),
        Se::Nc(n) => n.index() < c.ir().ncs.len(),
        _ => true,
    }));
}
