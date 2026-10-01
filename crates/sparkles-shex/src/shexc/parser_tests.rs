use super::*;

const EX: &str = "PREFIX ex: <http://ex.org/>\nPREFIX xsd: <http://www.w3.org/2001/XMLSchema#>\n";

fn ok(text: &str) -> Schema {
    parse(&format!("{EX}{text}"), None).unwrap_or_else(|e| panic!("{text}: {e}"))
}

/// The error of `text` (after the two prefix lines, so line 3 is its first line).
fn err(text: &str) -> String {
    parse(&format!("{EX}{text}"), None)
        .expect_err(text)
        .to_string()
}

fn ex(local: &str) -> String {
    format!("http://ex.org/{local}")
}

fn shape(schema: &Schema, i: usize) -> &Shape {
    match &schema.shapes[i].expr {
        ShapeExpr::Shape(s) => s,
        e => panic!("not a shape: {e:?}"),
    }
}

fn nc(schema: &Schema, i: usize) -> &NodeConstraint {
    match &schema.shapes[i].expr {
        ShapeExpr::Nc(nc) => nc,
        e => panic!("not a node constraint: {e:?}"),
    }
}

fn tc(schema: &Schema) -> &TripleConstraint {
    match &shape(schema, 0).expression {
        Some(TripleExpr::Tc(tc)) => tc,
        e => panic!("not a triple constraint: {e:?}"),
    }
}

#[test]
fn prologue() {
    let s = parse(
        "BASE <http://b.org/x/>\nPREFIX : <rel#>\nIMPORT <other>\nIMPORT :more\n<S> {}",
        None,
    )
    .unwrap();
    assert_eq!(s.base.as_deref(), Some("http://b.org/x/"));
    assert_eq!(
        s.prefixes,
        [(String::new(), "http://b.org/x/rel#".to_string())]
    );
    assert_eq!(
        s.imports,
        ["http://b.org/x/other", "http://b.org/x/rel#more"]
    );
    assert_eq!(s.shapes[0].label, Label::Iri("http://b.org/x/S".into()));
    // without any base, relative IRIs stay relative
    let s = parse("IMPORT <1dot> <S> {}", None).unwrap();
    assert_eq!(s.imports, ["1dot"]);
    assert_eq!(s.shapes[0].label, Label::Iri("S".into()));
    let s = parse("<S> {}", Some("file:///d/s.shex")).unwrap();
    assert_eq!(s.shapes[0].label, Label::Iri("file:///d/S".into()));
    // a redeclared prefix keeps its place
    let s = parse(
        "PREFIX a: <x:1> PREFIX b: <x:2> PREFIX a: <x:3> a:S {}",
        None,
    )
    .unwrap();
    assert_eq!(
        s.prefixes,
        [
            ("a".to_string(), "x:3".to_string()),
            ("b".into(), "x:2".into())
        ]
    );
    assert_eq!(s.shapes[0].label, Label::Iri("x:3S".into()));
}

#[test]
fn declarations() {
    let s = ok("start = @ex:S\nex:S EXTERNAL\n_:b . ex:T { }");
    assert_eq!(s.start, Some(ShapeExpr::Ref(Label::Iri(ex("S")))));
    assert_eq!(s.shapes[0].expr, ShapeExpr::External);
    assert_eq!(s.shapes[1].label, Label::BNode("b".into()));
    assert_eq!(s.shapes[1].expr, ShapeExpr::Shape(Box::default()));
    assert_eq!(s.shapes[2].expr, ShapeExpr::Shape(Box::default()));
    let s = ok("%ex:a{ x \\% \\\\ \\u00e9 \\n %} % ex:b % ex:S {}");
    assert_eq!(
        s.start_acts,
        [
            SemAct {
                name: ex("a"),
                code: Some(" x % \\ é \\n ".into())
            },
            SemAct {
                name: ex("b"),
                code: None
            }
        ]
    );
    // start actions come before the first statement
    assert!(err("start = @ex:S %ex:a{ x %}").contains("expected BASE"));
    assert!(err("%ex:a{ x %} PREFIX p: <x:> %ex:a{ x %}").contains("expected BASE"));
}

#[test]
fn triple_constraints() {
    let s = ok("ex:S { ^ex:p xsd:string {2,5} // ex:a \"x\" // a ex:b %ex:t{ print(o) %} }");
    let t = tc(&s);
    assert_eq!(t.inverse, Some(true));
    assert_eq!(t.predicate, ex("p"));
    assert_eq!((t.min, t.max), (Some(2), Some(5)));
    assert_eq!(
        t.value_expr.as_deref(),
        Some(&ShapeExpr::Nc(Box::new(NodeConstraint {
            datatype: Some(format!("{XSD}string")),
            ..Default::default()
        })))
    );
    assert_eq!(
        t.annotations,
        [
            Annotation {
                predicate: ex("a"),
                object: ObjectValue::Literal(ObjectLiteral {
                    value: "x".into(),
                    language: None,
                    datatype: None
                })
            },
            Annotation {
                predicate: RDF_TYPE.into(),
                object: ObjectValue::Iri(ex("b"))
            }
        ]
    );
    assert_eq!(t.sem_acts[0].code.as_deref(), Some(" print(o) "));
    for (card, want) in [
        ("*", (0, -1)),
        ("+", (1, -1)),
        ("?", (0, 1)),
        ("{3}", (3, 3)),
        ("{3,}", (3, -1)),
        ("{3,*}", (3, -1)),
        ("{0,4}", (0, 4)),
    ] {
        let s = ok(&format!("ex:S {{ a .{card} }}"));
        let t = tc(&s);
        assert_eq!(t.predicate, RDF_TYPE);
        assert_eq!(t.value_expr, None);
        assert_eq!((t.min, t.max), (Some(want.0), Some(want.1)), "{card}");
    }
    let s = ok("ex:S { ex:p NOT . }");
    assert_eq!(
        tc(&s).value_expr.as_deref(),
        Some(&ShapeExpr::Not(Box::new(ShapeExpr::Shape(Box::default()))))
    );
    let s = ok("ex:S { ex:p { ex:q . } // ex:a ex:b }");
    assert_eq!(tc(&s).annotations.len(), 1);
    assert!(matches!(
        tc(&s).value_expr.as_deref(),
        Some(ShapeExpr::Shape(_))
    ));
    assert_eq!(
        err("ex:S { ex:p . {99999999999} }"),
        "line 3, column 15: cardinality {99999999999} is too large"
    );
}

#[test]
fn groups() {
    let s = ok("ex:S CLOSED EXTRA ex:a a { $ex:g (ex:a . ; ex:b . | ex:c .)+ ; &ex:h ; }");
    let sh = shape(&s, 0);
    assert!(sh.is_closed());
    assert_eq!(sh.extra, [ex("a"), RDF_TYPE.to_string()]);
    let Some(TripleExpr::EachOf(outer)) = &sh.expression else {
        panic!("{:?}", sh.expression)
    };
    assert_eq!(outer.exprs.len(), 2);
    assert_eq!(outer.exprs[1], TripleExpr::Include(Label::Iri(ex("h"))));
    let TripleExpr::OneOf(one) = &outer.exprs[0] else {
        panic!("{:?}", outer.exprs[0])
    };
    assert_eq!(one.id, Some(Label::Iri(ex("g"))));
    assert_eq!((one.min, one.max), (Some(1), Some(-1)));
    assert!(matches!(&one.exprs[0], TripleExpr::EachOf(g) if g.exprs.len() == 2));
    // a bracketed constraint takes the cardinality, or is wrapped when it has one
    let s = ok("ex:S { (ex:a .)? }");
    assert_eq!((tc(&s).min, tc(&s).max), (Some(0), Some(1)));
    let s = ok("ex:S { $_:t (ex:a .{2}){3} }");
    assert!(matches!(
        &shape(&s, 0).expression,
        Some(TripleExpr::EachOf(g)) if g.exprs.len() == 1 && g.min == Some(3)
            && g.id == Some(Label::BNode("t".into()))
    ));
    let s = ok("ex:S { $_:t ex:a . }");
    assert_eq!(tc(&s).id, Some(Label::BNode("t".into())));
    assert!(matches!(
        &shape(&ok("ex:S { (&ex:a)* }"), 0).expression,
        Some(TripleExpr::EachOf(g)) if g.exprs == [TripleExpr::Include(Label::Iri(ex("a")))]
    ));
    let s = ok("ex:S { ex:a . | ex:b . ; } // ex:x 1 %ex:y%");
    let sh = shape(&s, 0);
    assert!(matches!(&sh.expression, Some(TripleExpr::OneOf(g)) if g.exprs.len() == 2));
    assert_eq!(sh.annotations.len(), 1);
    assert_eq!(sh.sem_acts.len(), 1);
}

#[test]
fn shape_expressions() {
    let s = ok("ex:S IRI @ex:T OR NOT { } AND (LITERAL) OR @<http://x/>");
    let ShapeExpr::Or(or) = &s.shapes[0].expr else {
        panic!("{:?}", s.shapes[0].expr)
    };
    assert_eq!(or.len(), 3);
    assert!(
        matches!(&or[0], ShapeExpr::And(a) if matches!(a[..], [ShapeExpr::Nc(_), ShapeExpr::Ref(_)]))
    );
    assert!(
        matches!(&or[1], ShapeExpr::And(a) if matches!(a[..], [ShapeExpr::Not(_), ShapeExpr::Nc(_)]))
    );
    assert_eq!(or[2], ShapeExpr::Ref(Label::Iri("http://x/".into())));
    // a constraint-and-shape pair joins an enclosing conjunction; parentheses keep it
    let s = ok("ex:S IRI @ex:T AND { } BNODE AND NOT IRI @ex:T AND (IRI @ex:T)");
    let ShapeExpr::And(and) = &s.shapes[0].expr else {
        panic!("{:?}", s.shapes[0].expr)
    };
    assert_eq!(and.len(), 6, "{and:?}");
    assert!(matches!(&and[4], ShapeExpr::Not(n) if matches!(**n, ShapeExpr::And(_))));
    assert!(matches!(&and[5], ShapeExpr::And(a) if a.len() == 2));
    let s = ok("ex:S @_:b ex:T @ # comment\n ex:U ex:V @ex: ex:W { } MINLENGTH 2");
    assert_eq!(s.shapes[0].expr, ShapeExpr::Ref(Label::BNode("b".into())));
    assert_eq!(s.shapes[1].expr, ShapeExpr::Ref(Label::Iri(ex("U"))));
    assert_eq!(s.shapes[2].expr, ShapeExpr::Ref(Label::Iri(ex(""))));
    assert!(
        matches!(&s.shapes[3].expr, ShapeExpr::And(a) if matches!(a[..], [ShapeExpr::Shape(_), ShapeExpr::Nc(_)]))
    );
}

#[test]
fn node_constraints() {
    let s = ok(
        "ex:S LITERAL MinLength 1 MAXLENGTH 5 /a\\/b\\u0063\\./i MININCLUSIVE -1.5e0 TOTALDIGITS 3",
    );
    let n = nc(&s, 0);
    assert_eq!(n.node_kind, Some(NodeKind::Literal));
    assert_eq!((n.min_length, n.max_length), (Some(1), Some(5)));
    assert_eq!(n.pattern.as_deref(), Some("a/bc\\."));
    assert_eq!(n.flags.as_deref(), Some("i"));
    assert_eq!(
        n.min_inclusive,
        Some(NumericLiteral::Double("-1.5e0".into()))
    );
    assert_eq!(n.total_digits, Some(3));
    let s =
        ok("ex:S BNODE LENGTH 2 ex:T [ex:a \"b\"@en-GB 1 true 'c'^^ex:d 2.5] ex:U MAXEXCLUSIVE 5");
    assert_eq!(nc(&s, 0).node_kind, Some(NodeKind::BNode));
    assert_eq!(nc(&s, 0).length, Some(2));
    let lit = |value: &str, language: Option<&str>, datatype: Option<String>| {
        ValueSetValue::Object(ObjectValue::Literal(ObjectLiteral {
            value: value.into(),
            language: language.map(Into::into),
            datatype,
        }))
    };
    assert_eq!(
        nc(&s, 1).values.as_deref().unwrap(),
        [
            ValueSetValue::Object(ObjectValue::Iri(ex("a"))),
            lit("b", Some("en-gb"), None),
            lit("1", None, Some(format!("{XSD}integer"))),
            lit("true", None, Some(format!("{XSD}boolean"))),
            lit("c", None, Some(ex("d"))),
            lit("2.5", None, Some(format!("{XSD}decimal"))),
        ]
    );
    assert_eq!(
        nc(&s, 2).max_exclusive,
        Some(NumericLiteral::Integer("5".into()))
    );
    // the grammar keeps numeric facets off non-literal constraints
    assert_eq!(
        err("ex:S IRI MININCLUSIVE 1"),
        "line 3, column 10: expected BASE, PREFIX, IMPORT, start or a shape label, \
         found 'MININCLUSIVE'"
    );
    assert_eq!(
        err("ex:S LITERAL LENGTH 2 LENGTH 3"),
        "line 3, column 23: LENGTH is given twice in one node constraint"
    );
    assert_eq!(
        err("ex:S LITERAL LENGTH -2"),
        "line 3, column 21: expected a non-negative integer, found '-2'"
    );
    assert_eq!(
        err("ex:S xsd:int MININCLUSIVE \"1\"^^xsd:int"),
        "line 3, column 27: expected a number after MININCLUSIVE, found '\"1\"'"
    );
    assert_eq!(
        err("ex:S LITERAL %ex:a{ %}"),
        "line 3, column 14: annotations and semantic actions on a node constraint are not \
         supported"
    );
}

#[test]
fn value_sets() {
    let vals = |text: &str| {
        let s = ok(&format!("ex:S [{text}]"));
        nc(&s, 0).values.clone().unwrap()
    };
    assert_eq!(vals("ex:a~"), [ValueSetValue::IriStem(ex("a"))]);
    assert_eq!(
        vals("ex:a~ - ex:b - ex:c~"),
        [ValueSetValue::IriStemRange {
            stem: Stem::Value(ex("a")),
            exclusions: vec![Exclusion::Value(ex("b")), Exclusion::Stem(ex("c"))]
        }]
    );
    assert_eq!(
        vals("'v'~ . - \"a\" - 'b'~ . - ex:a"),
        [
            ValueSetValue::LiteralStem("v".into()),
            ValueSetValue::LiteralStemRange {
                stem: Stem::Wildcard,
                exclusions: vec![Exclusion::Value("a".into()), Exclusion::Stem("b".into())]
            },
            ValueSetValue::IriStemRange {
                stem: Stem::Wildcard,
                exclusions: vec![Exclusion::Value(ex("a"))]
            },
        ]
    );
    assert_eq!(
        vals("@en @fr~ @~ @~ - @de . - @en~"),
        [
            ValueSetValue::Language("en".into()),
            ValueSetValue::LanguageStem("fr".into()),
            ValueSetValue::LanguageStem(String::new()),
            ValueSetValue::LanguageStemRange {
                stem: Stem::Value(String::new()),
                exclusions: vec![Exclusion::Value("de".into())]
            },
            ValueSetValue::LanguageStemRange {
                stem: Stem::Wildcard,
                exclusions: vec![Exclusion::Stem("en".into())]
            },
        ]
    );
    assert_eq!(
        vals("'''a\\tb\\u00e9\n'''"),
        [ValueSetValue::Object(ObjectValue::Literal(ObjectLiteral {
            value: "a\tbé\n".into(),
            language: None,
            datatype: None
        }))]
    );
    assert_eq!(vals(""), []);
    assert_eq!(
        err("ex:S [ex:a - ex:b]"),
        "line 3, column 12: expected an IRI, a literal, a language tag, '.' or ']', found '-'"
    );
    assert_eq!(
        err("ex:S [ex:a~ - \"b\"]"),
        "line 3, column 15: expected an IRI to exclude, found '\"b\"'"
    );
    assert_eq!(
        err("ex:S [. ex:a]"),
        "line 3, column 9: expected '-' (an exclusion after '.'), found 'ex:a'"
    );
    assert_eq!(
        err("ex:S ['a\\zb']"),
        "line 3, column 9: invalid escape '\\z' in a string"
    );
    assert_eq!(
        err("ex:S [\"a\"@en^^ex:b]"),
        "line 3, column 13: expected an IRI, a literal, a language tag, '.' or ']', found '^^'"
    );
}

#[test]
fn errors_have_positions_and_expectations() {
    assert_eq!(
        err("ex:S {\n  ex:p @ }"),
        "line 4, column 10: expected a shape label after '@', found '}'"
    );
    assert_eq!(
        err("ex:S {\n  ex:p . ex:q . }"),
        "line 4, column 10: expected a cardinality, an annotation ('//'), a semantic \
         action ('%'), ';', '|' or '}', found 'ex:q'"
    );
    assert_eq!(
        err("ex:S { (ex:p . %ex:a% ex:q . ) }"),
        "line 3, column 23: expected a cardinality, a semantic action ('%'), ';', '|' or \
         ')', found 'ex:q'"
    );
    assert_eq!(
        err("ex:S { ex:p .+ * }"),
        "line 3, column 16: expected an annotation ('//'), a semantic action ('%'), ';', \
         '|' or '}', found '*'"
    );
    assert_eq!(
        err("ex:S { ex:p ."),
        "line 3, column 14: expected a cardinality, an annotation ('//'), a semantic \
         action ('%'), ';', '|' or '}', found the end of the input"
    );
    assert_eq!(
        err("ex:S { foo:p . }"),
        "line 3, column 8: undeclared prefix 'foo:'"
    );
    assert_eq!(
        err("ex:S [\"abc]"),
        "line 3, column 7: expected an IRI, a literal, a language tag, '.' or ']', \
         found an unterminated string"
    );
    assert_eq!(
        err("é { }"),
        "line 3, column 1: expected BASE, PREFIX, IMPORT, start, a shape label or a \
         semantic action, found 'é'"
    );
    assert_eq!(
        err("ex:S { ex:p (1) }"),
        "line 3, column 14: expected a node constraint, a shape ('{'), a shape reference \
         ('@'), '(' or '.', found '1'"
    );
    assert_eq!(
        parse("<S> [<\\uD800>]", None).unwrap_err().to_string(),
        "line 1, column 7: \\uD800 is not a Unicode character"
    );
    assert_eq!(
        parse("<S> { <p\\u0020> . }", None).unwrap_err().to_string(),
        "line 1, column 7: invalid IRI <p >: Invalid IRI code point ' '"
    );
    assert_eq!(
        parse("<S> { <p> . }\nstart = @<S>\nstart = @<S>", None)
            .unwrap_err()
            .to_string(),
        "line 3, column 1: the start shape is declared twice"
    );
    assert_eq!(
        parse("BASE", None).unwrap_err().to_string(),
        "line 1, column 5: expected an IRI in angle brackets, found the end of the input"
    );
    assert_eq!(
        parse("<S> {}", Some("not an iri")).unwrap_err().to_string(),
        "line 1, column 1: invalid base IRI <not an iri>: Invalid IRI code point ' '"
    );
}

#[test]
fn shex_2_2_is_named() {
    assert_eq!(
        err("ABSTRACT ex:S {}"),
        "line 3, column 1: ABSTRACT is ShEx 2.2 syntax (abstract shapes), which is not supported"
    );
    assert_eq!(
        err("ex:S extends @ex:T {}"),
        "line 3, column 6: EXTENDS is ShEx 2.2 syntax (extending shapes), which is not \
         supported"
    );
    assert_eq!(
        err("ex:S CLOSED RESTRICTS @ex:T {}"),
        "line 3, column 13: RESTRICTS is ShEx 2.2 syntax (restricting shapes), which is not \
         supported"
    );
}

#[test]
fn line_and_column() {
    assert_eq!(line_col("ab\ncé d", 7), (2, 4));
    assert_eq!(line_col("\u{feff}ab", 4), (1, 2));
    assert_eq!(line_col("", 0), (1, 1));
}
