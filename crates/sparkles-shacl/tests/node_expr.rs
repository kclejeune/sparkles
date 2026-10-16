//! SHACL 1.2 node expressions in validation: targets, `sh:values`, `sh:defaultValue`,
//! `sh:deactivated`, custom functions and the SHACL-SPARQL expressions. The Working
//! Group's own test suite runs from `w3c_shacl.rs`; these are the draft's examples and
//! the cases that suite does not cover.

use sparkles_core::io::{RdfFormat, Source};
use sparkles_core::store::{Store, StoreOptions};
use sparkles_shacl::{Shapes, ValidateOptions, ValidationReport};

const PREFIXES: &str = r#"
@prefix sh: <http://www.w3.org/ns/shacl#> .
@prefix shnex: <http://www.w3.org/ns/shacl-node-expr#> .
@prefix sparql: <http://www.w3.org/ns/sparql#> .
@prefix ex: <http://ex.org/> .
@prefix rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#> .
@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
@prefix xsd: <http://www.w3.org/2001/XMLSchema#> .
"#;

fn validate(shapes: &str, data: &str) -> ValidationReport {
    let store = Store::in_memory(StoreOptions::default());
    let text = format!("{PREFIXES}{data}");
    store
        .load(&[Source::from_bytes(
            text.into_bytes(),
            RdfFormat::Turtle,
            None,
        )])
        .unwrap();
    let shapes = Shapes::parse(&format!("{PREFIXES}{shapes}"), RdfFormat::Turtle, None).unwrap();
    sparkles_shacl::validate(&store.snapshot(), &shapes, &ValidateOptions::default()).unwrap()
}

fn focus_nodes(r: &ValidationReport) -> Vec<String> {
    let mut v: Vec<String> = r
        .results
        .iter()
        .map(|x| x.focus_node.to_string().replace("<http://ex.org/", "<"))
        .collect();
    v.sort();
    v
}

#[test]
fn target_node_expression_selects_estonian_companies() {
    let shapes = r#"
ex:EstonianCompanyShape a sh:NodeShape ;
    sh:targetNode [
        shnex:nodes [ shnex:instancesOf ex:Company ] ;
        shnex:filterShape [
            sh:property [ sh:path ex:headQuarterCountry ; sh:hasValue ex:Estonia ]
        ]
    ] ;
    sh:property [ sh:path ex:name ; sh:minCount 1 ] .
"#;
    let data = r#"
ex:A a ex:Company ; ex:headQuarterCountry ex:Estonia .
ex:B a ex:Company ; ex:headQuarterCountry ex:Latvia .
ex:C a ex:Company ; ex:headQuarterCountry ex:Estonia ; ex:name "C" .
"#;
    let r = validate(shapes, data);
    // only the Estonian companies are targets, and only A lacks a name
    assert_eq!(focus_nodes(&r), ["<A>"]);
}

#[test]
fn values_and_default_values_feed_the_constraints() {
    let shapes = r#"
ex:CompanyShape a sh:NodeShape ; sh:targetClass ex:Company ;
    sh:property [
        sh:path ex:employeeCount ;
        sh:datatype xsd:integer ;
        sh:hasValue 2 ;
        sh:values [ shnex:count [ shnex:pathValues ex:employee ] ]
    ] ;
    sh:property [
        sh:path ex:nickname ;
        sh:minCount 1 ;
        sh:maxCount 1 ;
        sh:defaultValue "n/a" ;
    ] .
"#;
    let data = r#"
ex:Two a ex:Company ; ex:employee ex:p, ex:q ; ex:nickname "Duo" .
ex:Three a ex:Company ; ex:employee ex:p, ex:q, ex:r .
"#;
    let r = validate(shapes, data);
    // ex:Three has three employees, so hasValue 2 fails there; ex:Three gets the
    // default nickname, ex:Two keeps its own, and both have exactly one
    assert_eq!(focus_nodes(&r), ["<Three>"], "{r}");
}

#[test]
fn deactivated_by_an_expression() {
    let shapes = r#"
ex:S a sh:NodeShape ; sh:targetClass ex:Thing ;
    sh:deactivated [ shnex:exists [ shnex:pathValues ex:legacy ] ] ;
    sh:property [ sh:path ex:label ; sh:minCount 1 ] .
"#;
    let data = r#"
ex:Old a ex:Thing ; ex:legacy true .
ex:New a ex:Thing .
"#;
    let r = validate(shapes, data);
    assert_eq!(focus_nodes(&r), ["<New>"]);
}

#[test]
fn expression_constraint_sees_the_value_node() {
    let shapes = r#"
ex:S a sh:NodeShape ; sh:targetClass ex:Thing ;
    sh:property [
        sh:path ex:code ;
        sh:expression [ sparql:strstarts ( [ sparql:str [ shnex:var "value" ] ] "AB" ) ]
    ] .
"#;
    let data = r#"
ex:Good a ex:Thing ; ex:code "ABC" .
ex:Bad a ex:Thing ; ex:code "XYZ" .
"#;
    let r = validate(shapes, data);
    assert_eq!(focus_nodes(&r), ["<Bad>"]);
    assert_eq!(
        r.results[0].source_constraint_component.as_str(),
        "http://www.w3.org/ns/shacl#ExpressionConstraintComponent"
    );
}

#[test]
fn custom_functions_are_called_like_built_in_ones() {
    let shapes = r#"
ex:AverageExpression a sh:NamedParameterExpressionFunction ;
    rdfs:subClassOf sh:NamedParameterExpression ;
    sh:parameter [ a sh:Parameter ; sh:path ex:average ; sh:keyParameter true ] ;
    sh:bodyExpression [
        sparql:divide (
            [ shnex:sum [ shnex:arg ex:average ] ]
            [ shnex:count [ shnex:arg ex:average ] ]
        )
    ] .

ex:spacedConcat a sh:ListParameterExpressionFunction ;
    rdfs:subClassOf sh:ListParameterExpression ;
    sh:bodyExpression [ sparql:concat ( [ shnex:arg 0 ] " " [ shnex:arg 1 ] ) ] .

ex:S a sh:NodeShape ; sh:targetClass ex:Group ;
    sh:property [
        sh:path ex:avg ;
        sh:values [ ex:average [ shnex:pathValues ex:score ] ] ;
        sh:minInclusive 3 ; sh:maxInclusive 3
    ] ;
    sh:property [
        sh:path ex:full ;
        sh:values [ ex:spacedConcat ( [ shnex:pathValues ex:first ] [ shnex:pathValues ex:last ] ) ] ;
        sh:hasValue "Ada Lovelace"
    ] .
"#;
    let data = r#"
ex:G1 a ex:Group ; ex:score 2, 4 ; ex:first "Ada" ; ex:last "Lovelace" .
ex:G2 a ex:Group ; ex:score 1, 2 ; ex:first "Ada" ; ex:last "Byron" .
"#;
    let r = validate(shapes, data);
    // G2: average 1.5 and the wrong name
    assert_eq!(focus_nodes(&r), ["<G2>", "<G2>"], "{r}");
}

#[test]
fn select_and_sparql_expressions() {
    let shapes = r#"
ex:Child a sh:NodeShape ;
    sh:targetNode [
        sh:select """
            PREFIX ex: <http://ex.org/>
            SELECT ?person WHERE { ?person ex:age ?age . FILTER (?age < 18) }
        """
    ] ;
    sh:property [ sh:path ex:license ; sh:maxCount 0 ] .

ex:Len a sh:NodeShape ; sh:targetClass ex:Thing ;
    sh:property [
        sh:path ex:uriLength ;
        sh:values [ sh:sparqlExpr "STRLEN(STR($this))" ] ;
        sh:datatype xsd:integer ;
        sh:minInclusive 20
    ] .
"#;
    let data = r#"
ex:Kid ex:age 12 ; ex:license "x" .
ex:Adult ex:age 40 ; ex:license "y" .
ex:Thing1 a ex:Thing .
ex:T a ex:Thing .
"#;
    let r = validate(shapes, data);
    // the child has a license; <http://ex.org/T> is 16 characters, shorter than 20
    assert_eq!(focus_nodes(&r), ["<Kid>", "<T>"], "{r}");
}

#[test]
fn a_cycle_in_expressions_is_a_shapes_graph_error() {
    let text = format!(
        "{PREFIXES}
ex:S a sh:NodeShape ; sh:targetNode ex:x ; sh:expression _:a .
_:a shnex:exists _:b .
_:b shnex:distinct _:a ."
    );
    let err = Shapes::parse(&text, RdfFormat::Turtle, None).unwrap_err();
    assert!(format!("{err:#}").contains("contains itself"), "{err:#}");
}

#[test]
fn recursion_through_a_custom_function_is_a_failure() {
    let shapes = r#"
ex:Loop a sh:ListParameterExpressionFunction ;
    sh:bodyExpression [ ex:Loop ( [ shnex:arg 0 ] ) ] .
ex:S a sh:NodeShape ; sh:targetNode ex:x ; sh:expression [ ex:Loop ( 1 ) ] .
"#;
    let store = Store::in_memory(StoreOptions::default());
    let shapes = Shapes::parse(&format!("{PREFIXES}{shapes}"), RdfFormat::Turtle, None).unwrap();
    let err = sparkles_shacl::validate(&store.snapshot(), &shapes, &ValidateOptions::default())
        .unwrap_err();
    assert!(format!("{err:#}").contains("too deeply"), "{err:#}");
}

/// A shared expression graph in which each `shnex:concat` lists the one
/// before it twice doubles at every level, far below the nesting limit. Evaluation must
/// charge its output to a budget (and check the deadline) instead of expanding 2^24
/// nodes.
#[test]
fn doubling_expressions_are_bounded() {
    let levels = 24;
    let mut shapes = String::from("_:e0 shnex:concat ( ex:a ex:a ) .\n");
    for i in 1..levels {
        shapes.push_str(&format!(
            "_:e{i} shnex:concat ( _:e{} _:e{} ) .\n",
            i - 1,
            i - 1
        ));
    }
    shapes.push_str(&format!(
        "ex:S a sh:NodeShape ; sh:targetNode ex:x ;
           sh:property [ sh:path ex:p ; sh:values _:e{} ; sh:maxCount 1 ] .\n",
        levels - 1
    ));
    let store = Store::in_memory(StoreOptions::default());
    store
        .load(&[Source::from_bytes(
            format!("{PREFIXES}ex:x ex:p 1 .").into_bytes(),
            RdfFormat::Turtle,
            None,
        )])
        .unwrap();
    let shapes = Shapes::parse(&format!("{PREFIXES}{shapes}"), RdfFormat::Turtle, None).unwrap();
    let opts = ValidateOptions {
        timeout: Some(std::time::Duration::from_millis(100)),
        ..ValidateOptions::default()
    };
    let start = std::time::Instant::now();
    let r = sparkles_shacl::validate(&store.snapshot(), &shapes, &opts);
    assert!(
        r.is_err(),
        "expanded 2^{levels} nodes in {:?} and returned a report",
        start.elapsed()
    );
    assert!(
        start.elapsed() < std::time::Duration::from_secs(2),
        "took {:?}",
        start.elapsed()
    );
    // without a timeout the node budget stops it
    let err = sparkles_shacl::validate(&store.snapshot(), &shapes, &ValidateOptions::default())
        .unwrap_err();
    assert!(format!("{err:#}").contains("more than"), "{err:#}");
}

/// A read of the data may return more nodes than the budget of work, because the data
/// bounds the read and a class with millions of instances is an ordinary size. A
/// concatenation that appends the same nodes twice is work that the expression multiplies,
/// so the budget counts it. The budget here is far below the size of the read, so charging
/// the nodes of a read would fail the first case.
#[test]
fn a_large_read_is_not_charged_to_the_budget() {
    let n = 10_000;
    let mut nt = String::from("<http://ex.org/x> <http://ex.org/p> <http://ex.org/y> .\n");
    for i in 0..n {
        nt.push_str(&format!(
            "<http://ex.org/i{i}> <http://www.w3.org/1999/02/22-rdf-syntax-ns#type> <http://ex.org/C> .\n"
        ));
    }
    let store = Store::in_memory(StoreOptions::default());
    store
        .load(&[Source::from_bytes(
            nt.into_bytes(),
            RdfFormat::NTriples,
            None,
        )])
        .unwrap();
    let opts = ValidateOptions {
        parallel: false,
        max_expr_nodes: 1_000,
        ..ValidateOptions::default()
    };
    let shapes_with = |values: &str| {
        Shapes::parse(
            &format!(
                "{PREFIXES}ex:S a sh:NodeShape ; sh:targetNode ex:x ;
                   sh:property [ sh:path ex:p ;
                     sh:values {values} ; sh:minCount 1 ; sh:nodeKind sh:IRI ] ."
            ),
            RdfFormat::Turtle,
            None,
        )
        .unwrap()
    };
    let read = shapes_with("[ shnex:instancesOf ex:C ]");
    let r = sparkles_shacl::validate(&store.snapshot(), &read, &opts).unwrap();
    assert!(r.conforms, "{r}");
    let twice =
        shapes_with("[ shnex:concat ( [ shnex:instancesOf ex:C ] [ shnex:instancesOf ex:C ] ) ]");
    let err = sparkles_shacl::validate(&store.snapshot(), &twice, &opts).unwrap_err();
    assert!(format!("{err:#}").contains("more than"), "{err:#}");
}
