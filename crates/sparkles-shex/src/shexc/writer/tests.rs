use super::*;
use crate::shexc::parser::parse;

/// Parse, write, parse again: the same schema.
fn round_trip(text: &str) -> String {
    let a = parse(text, None).unwrap_or_else(|e| panic!("{e}\n{text}"));
    let written = write(&a);
    let b = parse(&written, None).unwrap_or_else(|e| panic!("{e}\n{written}"));
    assert_eq!(a, b, "\n{written}");
    written
}

#[test]
fn round_trips() {
    round_trip(
        r#"PREFIX ex: <http://ex.org/> PREFIX xsd: <http://www.w3.org/2001/XMLSchema#>
        PREFIX : <http://d.org/>
        IMPORT <http://ex.org/common>
        %ex:init{ start %}
        start = @ex:Person
        ex:Person EXTRA a CLOSED {
          $ex:tc a [ex:Person ex:Agent~] ;
          ex:name xsd:string MINLENGTH 1 MAXLENGTH 100 /^[A-Z]\/.*$/i // ex:note "named"@en ;
          ex:age xsd:integer MININCLUSIVE 0 MAXEXCLUSIVE 150.5 TOTALDIGITS 3 ? %ex:a{ x \% y \\ z %} ;
          ( ex:knows @ex:Person * | ^ex:knownBy @ex:Person {2,5} ) {1,} ;
          $ex:g ( ex:p . ; ex:q LITERAL LENGTH 3 ) ? // ex:x ex:y ;
          ex:r @<http://ex.org/S> OR (NOT @ex:T AND IRI) ;
          ex:s { ex:t . } ;
          ex:u ({ ex:v . } // ex:w "1"^^xsd:integer %ex:z%)
        } // ex:label "Person" %ex:shape{ code %}
        ex:S [ . - <http://ex.org/x> - ex:y~ "a"@en "b"^^xsd:string 5 1.5 1e3 true ]
        ex:T [ "ab"~ - "abc" - "abd"~ @en~ - @en-us @~ ] OR [ . - "x" ] OR [ . - @fr ] OR []
        :U IRI AND { :p . } AND @ex:S
        :V ((@ex:S AND @ex:T) AND @:U) OR (NOT (NOT @ex:S))
        _:b BNODE /a\/b/ AND CLOSED { &ex:g }
        :W EXTERNAL
        <http://ex.org/odd%20name> { ex:p [<http://ex.org/a#b>] }
        "#,
    );
}

#[test]
fn layout() {
    let w = round_trip(
        "PREFIX ex: <http://ex.org/>\nex:S { ex:a . ; ( ex:b . | ex:c . ; ex:d . ) ; ex:e @ex:S * }",
    );
    assert_eq!(
        w,
        "PREFIX ex: <http://ex.org/>\n\nex:S {\n  ex:a . ;\n  (\n    ex:b . |\n    ex:c . ;\n    \
         ex:d .\n  ) ;\n  ex:e @ex:S*\n}\n"
    );
}

#[test]
fn escapes() {
    assert_eq!(regexp("a/b", Some("i")), "/a\\/b/i");
    assert_eq!(regexp("a\\/b", None), "/a\\u005C\\/b/");
    assert_eq!(regexp("a\\d\nb", None), "/a\\d\\nb/");
    assert_eq!(quoted("a\"b\\c\n"), "\"a\\\"b\\\\c\\n\"");
    assert_eq!(
        iriref("http://ex.org/a b>"),
        "<http://ex.org/a\\u0020b\\u003E>"
    );
    assert_eq!(code("50% \\ ok"), "50\\% \\\\ ok");
    for (min, max, c) in [
        (None, None, ""),
        (Some(0), Some(1), "?"),
        (Some(0), Some(-1), "*"),
        (Some(1), Some(-1), "+"),
        (Some(2), Some(-1), "{2,}"),
        (Some(3), Some(3), "{3}"),
        (Some(2), Some(5), "{2,5}"),
        (Some(0), None, "?"),
        (None, Some(-1), "+"),
    ] {
        assert_eq!(card(min, max), c, "{min:?} {max:?}");
    }
    // a backslash before a slash survives the round trip through the parser
    let s = Schema {
        shapes: vec![ShapeDecl {
            label: Label::Iri("http://ex.org/S".into()),
            expr: ShapeExpr::Nc(Box::new(NodeConstraint {
                pattern: Some("a\\/b/c".into()),
                ..Default::default()
            })),
        }],
        ..Default::default()
    };
    assert_eq!(parse(&write(&s), None).unwrap(), s);
}

/// Node constraints only ShExJ can write: an equivalent conjunction of their parts.
#[test]
fn split_node_constraints() {
    let xsd = |l: &str| format!("http://www.w3.org/2001/XMLSchema#{l}");
    let nc = |n: NodeConstraint| {
        let s = Schema {
            shapes: vec![ShapeDecl {
                label: Label::Iri("http://ex.org/S".into()),
                expr: ShapeExpr::Nc(Box::new(n)),
            }],
            ..Default::default()
        };
        let w = write(&s);
        parse(&w, None).unwrap_or_else(|e| panic!("{e}\n{w}"));
        w.trim()
            .trim_start_matches("<http://ex.org/S> ")
            .to_string()
    };
    assert_eq!(
        nc(NodeConstraint {
            node_kind: Some(NodeKind::Literal),
            datatype: Some(xsd("integer")),
            min_inclusive: Some(NumericLiteral::Integer("1".into())),
            ..Default::default()
        }),
        "(LITERAL AND <http://www.w3.org/2001/XMLSchema#integer> AND MININCLUSIVE 1)"
    );
    assert_eq!(
        nc(NodeConstraint {
            node_kind: Some(NodeKind::Iri),
            max_exclusive: Some(NumericLiteral::Integer("1".into())),
            min_length: Some(2),
            ..Default::default()
        }),
        "(IRI AND MINLENGTH 2 AND MAXEXCLUSIVE 1)"
    );
    assert_eq!(
        nc(NodeConstraint {
            length: Some(2),
            total_digits: Some(3),
            ..Default::default()
        }),
        "(LENGTH 2 AND TOTALDIGITS 3)"
    );
    assert_eq!(nc(NodeConstraint::default()), "(NONLITERAL OR LITERAL)");
    assert_eq!(
        nc(NodeConstraint {
            node_kind: Some(NodeKind::Literal),
            min_length: Some(1),
            max_inclusive: Some(NumericLiteral::Decimal("2.5".into())),
            ..Default::default()
        }),
        "LITERAL MINLENGTH 1 MAXINCLUSIVE 2.5"
    );
}
