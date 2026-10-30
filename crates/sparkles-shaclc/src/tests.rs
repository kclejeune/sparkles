use super::*;
use oxrdf::dataset::{CanonicalizationAlgorithm, CanonicalizationHashAlgorithm};

fn canonical(mut g: Graph) -> Graph {
    g.canonicalize(CanonicalizationAlgorithm::Rdfc10 {
        hash_algorithm: CanonicalizationHashAlgorithm::Sha256,
    });
    g
}

fn turtle(text: &str, base: Option<&str>) -> Graph {
    let mut p = oxrdfio::RdfParser::from_format(oxrdfio::RdfFormat::Turtle);
    if let Some(b) = base {
        p = p.with_base_iri(b).unwrap();
    }
    let mut g = Graph::new();
    for q in p.for_slice(text.as_bytes()) {
        let q = q.unwrap();
        g.insert(&Triple::new(q.subject, q.predicate, q.object));
    }
    g
}

fn assert_iso(a: Graph, b: Graph) {
    let (a, b) = (canonical(a), canonical(b));
    assert!(a == b, "graphs differ\n--- left\n{a}\n--- right\n{b}");
}

const TTL_PREFIXES: &str = "@prefix sh: <http://www.w3.org/ns/shacl#> .
@prefix rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#> .
@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
@prefix owl: <http://www.w3.org/2002/07/owl#> .
@prefix xsd: <http://www.w3.org/2001/XMLSchema#> .
@prefix ex: <http://ex.org/> .
";

/// Parse SHACLC and compare with Turtle (both with the `ex:` prefix).
fn parses_to(shaclc: &str, ttl: &str) {
    let doc = parse(&format!("PREFIX ex: <http://ex.org/>\n{shaclc}"), None)
        .unwrap_or_else(|e| panic!("{e}"));
    assert_iso(doc.graph, turtle(&format!("{TTL_PREFIXES}{ttl}"), None));
}

/// Write the Turtle as SHACLC, check the text, and read it back.
fn writes_as(ttl: &str, expected: &str) {
    let g = turtle(&format!("{TTL_PREFIXES}{ttl}"), None);
    let prefixes = vec![("ex".to_string(), "http://ex.org/".to_string())];
    let text = write(&g, &prefixes).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(text, expected);
    let back = parse(&text, None).unwrap_or_else(|e| panic!("{e}\n{text}"));
    assert_iso(back.graph, g);
}

#[test]
fn the_note_example() {
    let doc = parse(
        r#"BASE <http://example.com/ns>
IMPORTS <http://example.com/person-ontology>
PREFIX ex: <http://example.com/ns#>

shape ex:PersonShape -> ex:Person {
    closed=true ignoredProperties=[rdf:type] .
    ex:ssn       xsd:string [0..1] pattern="^\\d{3}-\\d{2}-\\d{4}$" .
    ex:worksFor  IRI ex:Company [0..*] .
    ex:address   BlankNode [0..1] {
        ex:city xsd:string [1..1] .
        ex:postalCode xsd:integer|xsd:string [1..1] maxLength=5 .
    } .
}"#,
        None,
    )
    .unwrap();
    let expected = turtle(
        r#"@base <http://example.com/ns> .
@prefix ex: <http://example.com/ns#> .
@prefix owl: <http://www.w3.org/2002/07/owl#> .
@prefix rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#> .
@prefix sh: <http://www.w3.org/ns/shacl#> .
@prefix xsd: <http://www.w3.org/2001/XMLSchema#> .
<http://example.com/ns> rdf:type owl:Ontology ;
    owl:imports <http://example.com/person-ontology> .
ex:PersonShape a sh:NodeShape ; sh:targetClass ex:Person ; sh:closed true ;
    sh:ignoredProperties ( rdf:type ) ;
    sh:property [ sh:path ex:ssn ; sh:maxCount 1 ; sh:datatype xsd:string ;
                  sh:pattern "^\\d{3}-\\d{2}-\\d{4}$" ] ;
    sh:property [ sh:path ex:worksFor ; sh:class ex:Company ; sh:nodeKind sh:IRI ] ;
    sh:property [ sh:path ex:address ; sh:maxCount 1 ; sh:nodeKind sh:BlankNode ;
        sh:node [
            sh:property [ sh:path ex:city ; sh:datatype xsd:string ; sh:minCount 1 ; sh:maxCount 1 ] ;
            sh:property [ sh:path ex:postalCode ;
                sh:or ( [ sh:datatype xsd:integer ] [ sh:datatype xsd:string ] ) ;
                sh:minCount 1 ; sh:maxCount 1 ; sh:maxLength 5 ] ] ] ."#,
        None,
    );
    assert_iso(doc.graph.clone(), expected);
    assert_eq!(doc.base.as_deref(), Some("http://example.com/ns"));
    assert_eq!(
        doc.prefixes,
        [("ex".to_string(), "http://example.com/ns#".to_string())]
    );
    // and it writes back to a document with the same graph
    let text = write(&doc.graph, &doc.prefixes).unwrap();
    assert_iso(parse(&text, None).unwrap().graph, doc.graph);
}

#[test]
fn node_parameters_or_and_not() {
    parses_to(
        "shapeClass ex:S { nodeKind=sh:IRI|!in=[ex:a ex:b] . severity=sh:Warning . }",
        "ex:S a sh:NodeShape, rdfs:Class ;
            sh:or ( [ sh:nodeKind sh:IRI ] [ sh:not [ sh:in ( ex:a ex:b ) ] ] ) ;
            sh:severity sh:Warning .",
    );
    parses_to(
        "shape ex:S { !hasValue=1 . @ex:T . targetClass=ex:C . }",
        "ex:S a sh:NodeShape ; sh:not [ sh:hasValue 1 ] ; sh:node ex:T ;
            sh:targetClass ex:C .",
    );
}

#[test]
fn property_atoms() {
    parses_to(
        "shape ex:S { ex:p !IRI|Literal @ex:T rdf:langString ex:C uniqueLang=true
                       name=\"n\"@en order=2.5 defaultValue=1e0 . }",
        "ex:S a sh:NodeShape ; sh:property [ sh:path ex:p ;
            sh:or ( [ sh:not [ sh:nodeKind sh:IRI ] ] [ sh:nodeKind sh:Literal ] ) ;
            sh:node ex:T ; sh:datatype rdf:langString ; sh:class ex:C ;
            sh:uniqueLang true ; sh:name \"n\"@en ; sh:order 2.5 ; sh:defaultValue 1e0 ] .",
    );
    // counts: [0..*] produces nothing, [0..0] a maximum
    parses_to(
        "shape ex:S { ex:p [0..*] . ex:q [0..0] . ex:r [2..*] . }",
        "ex:S a sh:NodeShape ;
            sh:property [ sh:path ex:p ] ;
            sh:property [ sh:path ex:q ; sh:maxCount 0 ] ;
            sh:property [ sh:path ex:r ; sh:minCount 2 ] .",
    );
}

#[test]
fn paths() {
    parses_to(
        "shape ex:S { ^ex:a/(ex:b|ex:c)*/ex:d? . (ex:e/ex:f)+ . ex:g|^ex:h . }",
        "ex:S a sh:NodeShape ;
            sh:property [ sh:path ( [ sh:inversePath ex:a ]
                [ sh:zeroOrMorePath [ sh:alternativePath ( ex:b ex:c ) ] ]
                [ sh:zeroOrOnePath ex:d ] ) ] ;
            sh:property [ sh:path [ sh:oneOrMorePath ( ex:e ex:f ) ] ] ;
            sh:property [ sh:path [ sh:alternativePath ( ex:g [ sh:inversePath ex:h ] ) ] ] .",
    );
}

#[test]
fn list_parameters() {
    parses_to(
        "shape ex:S { memberShape=ex:M . ex:speakers IRI [1..1] memberShape=ex:Speaker
                      minListLength=1 maxListLength=10 uniqueMembers=true . }",
        "ex:S a sh:NodeShape ; sh:memberShape ex:M ;
            sh:property [ sh:path ex:speakers ; sh:nodeKind sh:IRI ; sh:minCount 1 ;
                sh:maxCount 1 ; sh:memberShape ex:Speaker ; sh:minListLength 1 ;
                sh:maxListLength 10 ; sh:uniqueMembers true ] .",
    );
    writes_as(
        "ex:S a sh:NodeShape ; sh:property [ sh:path ex:speakers ; sh:nodeKind sh:IRI ;
            sh:minCount 1 ; sh:maxCount 1 ; sh:memberShape ex:Speaker ; sh:maxListLength 10 ] .",
        "PREFIX ex: <http://ex.org/>\n\nshape ex:S {\n    ex:speakers IRI [1..1] memberShape=ex:Speaker maxListLength=10 .\n}\n",
    );
}

#[test]
fn ontology_and_base() {
    // a base given to the parser names the ontology; relative IRIs resolve against it
    let doc = parse("shape <#S> { }", Some("http://ex.org/doc")).unwrap();
    assert_iso(
        doc.graph,
        turtle(
            "<http://ex.org/doc> a <http://www.w3.org/2002/07/owl#Ontology> .
             <http://ex.org/doc#S> a <http://www.w3.org/ns/shacl#NodeShape> .",
            None,
        ),
    );
    // no base: no ontology triple, and IMPORTS is an error
    assert_eq!(parse("", None).unwrap().graph.len(), 0);
    assert!(parse("IMPORTS <http://ex.org/o>", None).is_err());
    // keywords ignore case, as in Jena
    assert!(
        parse(
            "prefix ex: <http://ex.org/> Shape ex:S { closed=TRUE . }",
            None
        )
        .is_ok()
    );
}

#[test]
fn syntax_errors() {
    for (text, line, what) in [
        ("shape ex:S { }", 1, "undeclared prefix"),
        (
            "PREFIX ex: <http://ex.org/>\nshape ex:S {\n  IRI .\n}",
            3,
            "not a node parameter",
        ),
        (
            "PREFIX ex: <http://ex.org/>\nshape ex:S {\n  ex:p foo=1 .\n}",
            3,
            "not a property parameter",
        ),
        (
            "PREFIX ex: <http://ex.org/>\nshape ex:S {\n  ex:p [1..x] .\n}",
            3,
            "maximum count",
        ),
        (
            "PREFIX ex: <http://ex.org/>\nshape ex:S {\n  ex:p IRI\n}",
            4,
            "'.'",
        ),
        (
            "PREFIX ex: <http://ex.org/>\nshape ex:S { ex:p Iri . }",
            2,
            "node kind",
        ),
    ] {
        let e = parse(text, None).unwrap_err();
        assert_eq!(e.line, line, "{text}: {e}");
        assert!(e.message.contains(what), "{text}: {e}");
    }
}

#[test]
fn writer_output() {
    writes_as(
        "ex:S a sh:NodeShape ; sh:targetClass ex:C ; sh:closed true ;
            sh:ignoredProperties ( rdf:type ) ;
            sh:or ( [ sh:nodeKind sh:IRI ] [ sh:not [ sh:hasValue \"x\" ] ] ) ;
            sh:property [ sh:path [ sh:inversePath [ sh:zeroOrMorePath ex:p ] ] ;
                          sh:class xsd:string ; sh:datatype ex:D ; sh:minCount 2 ] ;
            sh:property [ sh:path ( ex:a [ sh:alternativePath ( ex:b ex:c ) ] ) ;
                          sh:node [ sh:property [ sh:path ex:d ; sh:in ( 1 2.5 \"t\"@en ) ] ] ] .
         ex:T a sh:NodeShape, rdfs:Class ; sh:targetClass ex:D ; sh:node ex:S .",
        "PREFIX ex: <http://ex.org/>

shape ex:S -> ex:C {
    closed=true .
    ignoredProperties=[rdf:type] .
    nodeKind=sh:IRI|!hasValue=\"x\" .
    ^(ex:p*) [2..*] class=xsd:string datatype=ex:D .
    ex:a/(ex:b|ex:c) {
        ex:d in=[1 2.5 \"t\"@en] .
    } .
}

shapeClass ex:T {
    targetClass=ex:D .
    @ex:S .
}
",
    );
}

#[test]
fn writer_refuses_what_compact_cannot_express() {
    let refuse = |ttl: &str, culprit: &str| {
        let g = turtle(&format!("{TTL_PREFIXES}{ttl}"), None);
        let e = write(&g, &[]).unwrap_err();
        assert!(
            e.triples.iter().any(|t| t.to_string().contains(culprit)),
            "{ttl}: {e}"
        );
    };
    // sh:minCount 0 has no count form
    refuse(
        "ex:S a sh:NodeShape ; sh:property [ sh:path ex:p ; sh:minCount 0 ] .",
        "minCount",
    );
    // a named property shape
    refuse(
        "ex:S a sh:NodeShape ; sh:property ex:P . ex:P sh:path ex:p .",
        "property",
    );
    // a shared blank node
    refuse(
        "ex:S a sh:NodeShape ; sh:property _:p . ex:T a sh:NodeShape ; sh:property _:p .
         _:p sh:path ex:p .",
        "property",
    );
    // a triple outside the shapes
    refuse("ex:S a sh:NodeShape ; rdfs:label \"S\" .", "label");
    // a parameter SHACLC does not have, and a property parameter in a node shape
    refuse(
        "ex:S a sh:NodeShape ; sh:property [ sh:path ex:p ; sh:qualifiedValueShape [ sh:class ex:C ] ;
             sh:qualifiedMinCount 1 ] .",
        "qualifiedValueShape",
    );
    refuse("ex:S a sh:NodeShape ; sh:uniqueLang true .", "uniqueLang");
    // a count that is not a plain xsd:integer
    refuse(
        "ex:S a sh:NodeShape ; sh:property [ sh:path ex:p ; sh:maxCount \"1\"^^xsd:int ] .",
        "maxCount",
    );
}
