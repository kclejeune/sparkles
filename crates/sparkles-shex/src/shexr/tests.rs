use super::*;
use oxrdf::dataset::CanonicalizationAlgorithm;
use oxrdf::dataset::CanonicalizationHashAlgorithm;

/// A schema with every construct ShExJ has.
const KITCHEN: &str = r#"{
  "@context": "http://www.w3.org/ns/shex.jsonld",
  "type": "Schema",
  "imports": ["http://ex.org/common"],
  "startActs": [{"type": "SemAct", "name": "http://ex.org/x"}],
  "start": {"type": "ShapeNot", "shapeExpr": "http://ex.org/S"},
  "shapes": [
    {"id": "http://ex.org/S", "type": "Shape", "closed": true,
     "extra": ["http://ex.org/b", "http://ex.org/c"],
     "expression": {"type": "OneOf", "id": "http://ex.org/g", "min": 0, "max": -1, "expressions": [
       {"type": "TripleConstraint", "id": "http://ex.org/e", "predicate": "http://ex.org/a",
        "inverse": true, "min": 2, "max": 5,
        "valueExpr": {"type": "NodeConstraint",
          "datatype": "http://www.w3.org/2001/XMLSchema#decimal", "length": 3,
          "minlength": 1, "maxlength": 9, "pattern": "^a/b\"c$", "flags": "i",
          "mininclusive": 4.5, "minexclusive": -2, "maxinclusive": 1e300,
          "maxexclusive": 10, "totaldigits": 5, "fractiondigits": 2},
        "semActs": [{"type": "SemAct", "name": "http://ex.org/y", "code": " p(o)\n "}],
        "annotations": [{"type": "Annotation", "predicate": "http://ex.org/n",
          "object": "http://ex.org/o"}]},
       {"type": "EachOf", "expressions": [
         {"type": "TripleConstraint", "predicate": "http://ex.org/c"},
         {"type": "TripleConstraint", "predicate": "http://ex.org/d",
          "valueExpr": {"type": "Shape"}},
         "http://ex.org/e"]}
     ]},
     "semActs": [{"type": "SemAct", "name": "http://ex.org/z"}],
     "annotations": [{"type": "Annotation", "predicate": "http://ex.org/note",
       "object": {"value": "hi", "language": "en"}}]},
    {"id": "_:t", "type": "ShapeOr", "shapeExprs": ["http://ex.org/S",
      {"type": "ShapeAnd", "shapeExprs": [
        {"type": "NodeConstraint", "values": []},
        {"type": "NodeConstraint", "nodeKind": "literal", "values": [
          "http://ex.org/v",
          {"value": "1", "type": "http://www.w3.org/2001/XMLSchema#integer"},
          {"value": "x"},
          {"type": "IriStem", "stem": "http://ex.org/s"},
          {"type": "IriStemRange", "stem": {"type": "Wildcard"},
           "exclusions": ["http://ex.org/x", {"type": "IriStem", "stem": "http://ex.org/y"}]},
          {"type": "LiteralStem", "stem": "ab"},
          {"type": "LiteralStemRange", "stem": "a",
           "exclusions": ["ab", {"type": "LiteralStem", "stem": "ac"}]},
          {"type": "Language", "languageTag": "en"},
          {"type": "LanguageStem", "stem": ""},
          {"type": "LanguageStemRange", "stem": "en",
           "exclusions": ["en-us", {"type": "LanguageStem", "stem": "en-gb"}]}
        ]}]}]},
    {"type": "ShapeDecl", "id": "http://ex.org/Alias", "shapeExpr": "_:t"},
    {"id": "http://ex.org/E", "type": "ShapeExternal"},
    {"id": "http://ex.org/R", "type": "Shape", "expression": {"type": "TripleConstraint",
      "predicate": "http://ex.org/p", "valueExpr": "http://ex.org/Imported"}}
  ]
}"#;

fn kitchen() -> Schema {
    crate::shexj::from_shexj(KITCHEN).unwrap()
}

fn canonical(mut g: Graph) -> Graph {
    g.canonicalize(CanonicalizationAlgorithm::Rdfc10 {
        hash_algorithm: CanonicalizationHashAlgorithm::Sha256,
    });
    g
}

fn turtle_graph(text: &str) -> Graph {
    let mut g = Graph::new();
    for t in oxttl::TurtleParser::new().for_slice(text.as_bytes()) {
        g.insert(&t.unwrap_or_else(|e| panic!("{e}\n{text}")));
    }
    g
}

#[test]
fn graph_round_trip() {
    let s = kitchen();
    let g = to_graph(&s);
    let again = from_graph(&g, None).unwrap();
    assert_eq!(again.to_shexj(), s.to_shexj());
}

#[test]
fn turtle_round_trip() {
    let mut s = kitchen();
    s.prefixes = vec![("ex".into(), "http://ex.org/".into())];
    let text = to_text(&s, RdfFormat::Turtle);
    assert!(
        text.starts_with("PREFIX ex: <http://ex.org/>\nPREFIX sx: <"),
        "{text}"
    );
    assert!(text.contains("[] a sx:Schema ;"), "{text}");
    assert!(text.contains("ex:S a sx:Shape ;"), "{text}");
    assert!(text.contains("sx:extra ex:b , ex:c"), "{text}");
    assert!(text.contains("sx:mininclusive 4.5 ;"), "{text}");
    assert!(text.contains("sx:max -1"), "{text}");
    assert!(text.contains("sx:values ()"), "{text}");
    // the same graph as the triples
    assert_eq!(
        canonical(turtle_graph(&text)),
        canonical(to_graph(&s)),
        "{text}"
    );
    let again = from_text(&text, RdfFormat::Turtle, None).unwrap();
    assert_eq!(again.to_shexj(), s.to_shexj(), "{text}");
    assert_eq!(again.prefixes, s.prefixes);
    // and through the ShExC writer
    let c = again.to_shexc();
    assert!(c.contains("ex:S"), "{c}");
}

#[test]
fn other_syntaxes() {
    let s = kitchen();
    for f in [RdfFormat::NTriples, RdfFormat::RdfXml, RdfFormat::TriG] {
        let text = to_text(&s, f);
        let again = from_text(&text, f, None).unwrap_or_else(|e| panic!("{f}: {e}\n{text}"));
        assert_eq!(again.to_shexj(), s.to_shexj(), "{f}");
    }
}

#[test]
fn reads_the_suite_form() {
    // as shexTest writes it: the declarations nested, a triple expression labelled by
    // an IRI and included by another shape, a blank node label of an import
    let ttl = r#"
BASE <http://all.example/>
PREFIX sx: <http://www.w3.org/ns/shex#>
PREFIX xsd: <http://www.w3.org/2001/XMLSchema#>
[] a sx:Schema ;
  sx:imports (<other>) ;
  sx:start <S1> ;
  sx:shapes (<S1> <S2> _:S3) .
<S1> a sx:Shape ;
  sx:expression <S2e> .
<S2> a sx:Shape ;
  sx:extra <q>, <p> ;
  sx:expression [ a sx:EachOf ; sx:expressions ( <S2e> _:imported ) ;
    sx:annotation ( [ a sx:Annotation ; sx:predicate <n> ; sx:object "x"@EN ] ) ] .
<S2e> a sx:TripleConstraint ; sx:predicate <p1> ; sx:min 0 ; sx:max 1 ;
  sx:valueExpr <S2> .
_:S3 a sx:NodeConstraint ; sx:datatype xsd:integer ; sx:mininclusive "04.50"^^xsd:decimal ;
  sx:maxexclusive "5E0"^^xsd:double ; sx:totaldigits 3 ;
  sx:values ( 1 "a" [ a sx:LiteralStemRange ; sx:stem "@fr" ; sx:exclusion ("@fr-be") ] ) .
"#;
    let s = from_text(ttl, RdfFormat::Turtle, Some("http://all.example/x.ttl")).unwrap();
    assert_eq!(s.base.as_deref(), Some("http://all.example/x.ttl"));
    assert_eq!(s.imports, ["http://all.example/other"]);
    assert_eq!(
        s.start,
        Some(ShapeExpr::Ref(Label::Iri("http://all.example/S1".into())))
    );
    assert_eq!(
        s.prefixes,
        [(
            "xsd".to_string(),
            "http://www.w3.org/2001/XMLSchema#".to_string()
        )]
    );
    let ShapeExpr::Shape(s1) = &s.shapes[0].expr else {
        panic!()
    };
    // the first use defines the labelled triple expression
    let Some(TripleExpr::Tc(tc)) = &s1.expression else {
        panic!("{:?}", s1.expression)
    };
    assert_eq!(tc.id, Some(Label::Iri("http://all.example/S2e".into())));
    assert_eq!((tc.min, tc.max), (Some(0), Some(1)));
    assert_eq!(
        tc.value_expr.as_deref(),
        Some(&ShapeExpr::Ref(Label::Iri("http://all.example/S2".into())))
    );
    let ShapeExpr::Shape(s2) = &s.shapes[1].expr else {
        panic!()
    };
    assert_eq!(s2.extra, ["http://all.example/p", "http://all.example/q"]);
    let Some(TripleExpr::EachOf(g)) = &s2.expression else {
        panic!()
    };
    assert_eq!(
        g.exprs,
        [
            TripleExpr::Include(Label::Iri("http://all.example/S2e".into())),
            TripleExpr::Include(Label::BNode("imported".into()))
        ]
    );
    assert_eq!(
        g.annotations[0].object,
        ObjectValue::Literal(ObjectLiteral {
            value: "x".into(),
            language: Some("en".into()),
            datatype: None
        })
    );
    assert_eq!(s.shapes[2].label, Label::BNode("S3".into()));
    let ShapeExpr::Nc(nc) = &s.shapes[2].expr else {
        panic!()
    };
    assert_eq!(
        nc.min_inclusive,
        Some(NumericLiteral::Decimal("04.50".into()))
    );
    assert_eq!(nc.max_exclusive, Some(NumericLiteral::Double("5E0".into())));
    assert_eq!(nc.total_digits, Some(3));
    let values = nc.values.as_ref().unwrap();
    assert_eq!(
        values[0],
        ValueSetValue::Object(ObjectValue::Literal(ObjectLiteral {
            value: "1".into(),
            language: None,
            datatype: Some(xsd::INTEGER.as_str().into())
        }))
    );
    assert_eq!(
        values[1],
        ValueSetValue::Object(ObjectValue::Literal(ObjectLiteral {
            value: "a".into(),
            language: None,
            datatype: None
        }))
    );
    assert_eq!(
        values[2],
        ValueSetValue::LiteralStemRange {
            stem: Stem::Value("@fr".into()),
            exclusions: vec![Exclusion::Value("@fr-be".into())]
        }
    );
    // it compiles, but for the import
    let e = crate::compile(&s, &crate::NoImports).unwrap_err();
    assert!(e.message.contains("other"), "{}", e.message);
}

fn err(ttl: &str) -> ParseError {
    let text = format!("PREFIX sx: <{SX}> PREFIX ex: <http://ex.org/>\n{ttl}");
    from_text(&text, RdfFormat::Turtle, None).unwrap_err()
}

#[test]
fn errors() {
    let e = err("ex:a ex:b ex:c .");
    assert_eq!((e.line, e.column), (0, 0));
    assert_eq!(e.message, "ShExR: no node has type sx:Schema");
    assert!(
        err("[] a sx:Schema . [] a sx:Schema .")
            .message
            .contains("more than one")
    );
    // RDF syntax errors have their place
    let e = err("[] a sx:Schema ;\n  sx:shapes ( ex:S .");
    assert_eq!(e.line, 3, "{e}");
    assert!(e.column > 0);
    let e = err(
        "[] a sx:Schema ; sx:shapes (ex:S) . ex:S a sx:Shape ; sx:expression [ a sx:TripleConstraint ] .",
    );
    assert!(e.message.contains("has no sx:predicate"), "{}", e.message);
    assert!(e.message.starts_with("ShExR: _:"), "{}", e.message);
    let e = err("[] a sx:Schema ; sx:shapes (ex:S) .");
    assert!(
        e.message.contains(
            "<http://ex.org/S> is declared in sx:shapes but has no shape expression type"
        ),
        "{}",
        e.message
    );
    let e = err("[] a sx:Schema ; sx:shapes (ex:S) . ex:S a sx:Bogus .");
    assert!(e.message.contains("sx:Bogus"), "{}", e.message);
    let e = err(
        "[] a sx:Schema ; sx:start _:x . _:x a sx:ShapeNot ; sx:shapeExpr _:y . \
         _:y a sx:ShapeNot ; sx:shapeExpr _:x .",
    );
    assert!(e.message.contains("contains itself"), "{}", e.message);
    let e = err(
        "[] a sx:Schema ; sx:shapes (ex:S) . ex:S a sx:Shape ; sx:expression [ a sx:TripleConstraint ; \
         sx:predicate ex:p ; sx:min 3 ; sx:max 2 ] .",
    );
    assert!(e.message.contains("greater than max"), "{}", e.message);
    let e = err(
        "[] a sx:Schema ; sx:shapes (ex:S) . ex:S a sx:NodeConstraint ; \
         sx:datatype ex:dt ; sx:mininclusive 1 .",
    );
    assert!(
        e.message.contains("not an XSD numeric datatype"),
        "{}",
        e.message
    );
    let e = err("[] a sx:Schema ; sx:shapes (ex:S) . ex:S a sx:Shape ; sx:extends (ex:T) .");
    assert!(e.message.contains("ShEx 2.2"), "{}", e.message);
    let e = err(
        "[] a sx:Schema ; sx:shapes [ <http://www.w3.org/1999/02/22-rdf-syntax-ns#first> ex:S ] .",
    );
    assert!(e.message.contains("not an RDF list"), "{}", e.message);
    let e = err("[] a sx:Schema ; sx:shapes (ex:S) . ex:S a sx:Shape ; sx:closed \"maybe\" .");
    assert!(e.message.contains("true or false"), "{}", e.message);
    let e = from_text("", RdfFormat::Turtle, Some("not a base")).unwrap_err();
    assert!(e.message.contains("invalid base"), "{}", e.message);
}

#[test]
fn empty_and_alias_declarations() {
    // a declaration that is a reference is a ShapeDecl; an empty value set stays empty
    let s = crate::Schema::parse_shexc(
        "PREFIX ex: <http://ex.org/> ex:A @ex:B ex:B [] ex:C {}",
        None,
    )
    .unwrap();
    let g = to_graph(&s);
    let again = from_graph(&g, None).unwrap();
    assert_eq!(again.shapes, s.shapes);
    let text = to_text(&s, RdfFormat::Turtle);
    assert!(
        text.contains("ex:A a sx:ShapeDecl ;\n    sx:shapeExpr ex:B ."),
        "{text}"
    );
}
