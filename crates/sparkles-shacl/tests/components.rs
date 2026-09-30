//! Unit tests for every constraint component, targets, paths and report output.

use oxrdf::Term;
use sparkles::io::{RdfFormat, Source};
use sparkles::store::{Store, StoreOptions};
use sparkles_shacl::{PropertyPath, Shapes, ValidateOptions, ValidationReport, validate};

const PREFIXES: &str = "@prefix ex: <http://ex.org/> .
@prefix sh: <http://www.w3.org/ns/shacl#> .
@prefix rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#> .
@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
@prefix xsd: <http://www.w3.org/2001/XMLSchema#> .
";

fn store(data: &str) -> Store {
    let s = Store::in_memory(StoreOptions::default());
    s.load(&[Source::from_bytes(
        format!("{PREFIXES}{data}").into_bytes(),
        RdfFormat::Turtle,
        None,
    )])
    .unwrap();
    s
}

fn shapes(ttl: &str) -> Shapes {
    Shapes::parse(&format!("{PREFIXES}{ttl}"), RdfFormat::Turtle, None)
        .unwrap_or_else(|e| panic!("{e:#}"))
}

fn run(data: &str, shapes_ttl: &str) -> ValidationReport {
    let s = store(data);
    validate(
        &s.snapshot(),
        &shapes(shapes_ttl),
        &ValidateOptions::default(),
    )
    .unwrap_or_else(|e| panic!("{e:#}"))
}

fn short(t: &Term) -> String {
    match t {
        Term::NamedNode(n) => n.as_str().rsplit(['/', '#']).next().unwrap().to_string(),
        Term::Literal(l) => l.value().to_string(),
        Term::BlankNode(_) => "_".into(),
    }
}

/// Sorted `Component focus value` strings (component without namespace/suffix).
fn summary(r: &ValidationReport) -> Vec<String> {
    let mut v: Vec<String> = r
        .results
        .iter()
        .map(|x| {
            let c = x
                .source_constraint_component
                .as_str()
                .rsplit(['#', '/'])
                .next()
                .unwrap()
                .trim_end_matches("ConstraintComponent")
                .to_string();
            match &x.value {
                Some(val) => format!("{c} {} {}", short(&x.focus_node), short(val)),
                None => format!("{c} {}", short(&x.focus_node)),
            }
        })
        .collect();
    v.sort();
    v
}

fn assert_results(data: &str, shapes_ttl: &str, expected: &[&str]) {
    let r = run(data, shapes_ttl);
    let mut exp: Vec<String> = expected.iter().map(|s| s.to_string()).collect();
    exp.sort();
    assert_eq!(summary(&r), exp, "\n{r}");
    assert_eq!(r.conforms, expected.is_empty());
}

#[test]
fn class_with_subclasses() {
    assert_results(
        "ex:a ex:p ex:x . ex:x a ex:Sub . ex:Sub rdfs:subClassOf ex:C .
         ex:b ex:p ex:y . ex:y a ex:Other . ex:c ex:p \"lit\" .",
        "ex:S sh:targetSubjectsOf ex:p ; sh:property [ sh:path ex:p ; sh:class ex:C ] .",
        &["Class b y", "Class c lit"],
    );
}

#[test]
fn datatype_and_ill_formed() {
    assert_results(
        "ex:a ex:p 1 . ex:b ex:p \"x\" . ex:c ex:p \"12x\"^^xsd:integer . ex:d ex:p \"300\"^^xsd:byte .",
        "ex:S sh:targetSubjectsOf ex:p ; sh:property [ sh:path ex:p ; sh:datatype xsd:integer ] .",
        &["Datatype b x", "Datatype c 12x", "Datatype d 300"],
    );
}

#[test]
fn node_kind() {
    assert_results(
        "ex:a ex:p ex:x . ex:b ex:p [] . ex:c ex:p \"l\" .",
        "ex:S sh:targetSubjectsOf ex:p ; sh:property [ sh:path ex:p ; sh:nodeKind sh:BlankNodeOrIRI ] .",
        &["NodeKind c l"],
    );
}

#[test]
fn min_max_count() {
    assert_results(
        "ex:a a ex:T . ex:b a ex:T ; ex:p 1 . ex:c a ex:T ; ex:p 1, 2, 3 .",
        "ex:S sh:targetClass ex:T ; sh:property [ sh:path ex:p ; sh:minCount 1 ; sh:maxCount 2 ] .",
        &["MinCount a", "MaxCount c"],
    );
}

#[test]
fn value_ranges() {
    assert_results(
        "ex:a ex:age 5 . ex:b ex:age 17 . ex:c ex:age 18.5 . ex:d ex:age 120 . ex:e ex:age \"x\" .",
        "ex:S sh:targetSubjectsOf ex:age ; sh:property [ sh:path ex:age ;
            sh:minInclusive 18 ; sh:maxExclusive 120 ] .",
        &[
            "MaxExclusive d 120",
            "MaxExclusive e x",
            "MinInclusive a 5",
            "MinInclusive b 17",
            "MinInclusive e x",
        ],
    );
    assert_results(
        "ex:a ex:d \"2020-01-01\"^^xsd:date . ex:b ex:d \"2019-12-31\"^^xsd:date .",
        "ex:S sh:targetSubjectsOf ex:d ; sh:property [ sh:path ex:d ;
            sh:minExclusive \"2019-12-31\"^^xsd:date ; sh:maxInclusive \"2020-01-01\"^^xsd:date ] .",
        &["MinExclusive b 2019-12-31"],
    );
}

#[test]
fn string_lengths_and_patterns() {
    assert_results(
        "ex:a ex:n \"ab\" . ex:b ex:n \"abcdef\" . ex:c ex:n [] . ex:d ex:n \"ABC\" .",
        "ex:S sh:targetSubjectsOf ex:n ; sh:property [ sh:path ex:n ;
            sh:minLength 3 ; sh:maxLength 5 ; sh:pattern \"^a\" ; sh:flags \"i\" ] .",
        &[
            "MaxLength b abcdef",
            "MinLength a ab",
            "MinLength c _",
            "MaxLength c _",
            "Pattern c _",
        ],
    );
}

#[test]
fn language_in_and_unique_lang() {
    assert_results(
        "ex:a ex:l \"x\"@en-US, \"y\"@en-GB . ex:b ex:l \"z\"@de . ex:c ex:l \"plain\" .",
        "ex:S sh:targetSubjectsOf ex:l ; sh:property [ sh:path ex:l ; sh:languageIn ( \"en\" \"fr\" ) ] .",
        &["LanguageIn b z", "LanguageIn c plain"],
    );
    assert_results(
        "ex:a ex:l \"x\"@en, \"y\"@EN, \"z\"@fr . ex:b ex:l \"x\"@en, \"x\" .",
        "ex:S sh:targetSubjectsOf ex:l ; sh:property [ sh:path ex:l ; sh:uniqueLang true ] .",
        &["UniqueLang a"],
    );
}

#[test]
fn property_pair_constraints() {
    assert_results(
        "ex:a ex:p 1, 2 ; ex:q 1, 2 . ex:b ex:p 1 ; ex:q 2 .",
        "ex:S sh:targetSubjectsOf ex:p ; sh:property [ sh:path ex:p ; sh:equals ex:q ] .",
        &["Equals b 1", "Equals b 2"],
    );
    assert_results(
        "ex:a ex:p 1 ; ex:q 2 . ex:b ex:p 1 ; ex:q 1 .",
        "ex:S sh:targetSubjectsOf ex:p ; sh:property [ sh:path ex:p ; sh:disjoint ex:q ] .",
        &["Disjoint b 1"],
    );
    assert_results(
        "ex:a ex:start 1 ; ex:end 2 . ex:b ex:start 2 ; ex:end 2 . ex:c ex:start 3 ; ex:end 2 .",
        "ex:S sh:targetSubjectsOf ex:start ; sh:property [ sh:path ex:start ; sh:lessThan ex:end ] .",
        &["LessThan b 2", "LessThan c 3"],
    );
    assert_results(
        "ex:a ex:start 1 ; ex:end 2 . ex:b ex:start 2 ; ex:end 2 . ex:c ex:start 3 ; ex:end 2 .",
        "ex:S sh:targetSubjectsOf ex:start ; sh:property [ sh:path ex:start ; sh:lessThanOrEquals ex:end ] .",
        &["LessThanOrEquals c 3"],
    );
}

#[test]
fn logical_components() {
    let shapes_ttl = "
        ex:HasP sh:property [ sh:path ex:p ; sh:minCount 1 ] .
        ex:HasQ sh:property [ sh:path ex:q ; sh:minCount 1 ] .
        ex:Not a sh:NodeShape ; sh:targetClass ex:T ; sh:not ex:HasP .
        ex:And a sh:NodeShape ; sh:targetClass ex:T ; sh:and ( ex:HasP ex:HasQ ) .
        ex:Or a sh:NodeShape ; sh:targetClass ex:T ; sh:or ( ex:HasP ex:HasQ ) .
        ex:Xone a sh:NodeShape ; sh:targetClass ex:T ; sh:xone ( ex:HasP ex:HasQ ) .
        ex:Node a sh:NodeShape ; sh:targetClass ex:T ; sh:node ex:HasQ .";
    assert_results(
        "ex:none a ex:T . ex:p a ex:T ; ex:p 1 . ex:pq a ex:T ; ex:p 1 ; ex:q 1 .",
        shapes_ttl,
        &[
            "And none none",
            "And p p",
            "Node none none",
            "Node p p",
            "Not p p",
            "Not pq pq",
            "Or none none",
            "Xone none none",
            "Xone pq pq",
        ],
    );
}

#[test]
fn nested_property_shape_results_keep_their_source() {
    let r = run(
        "ex:a a ex:T ; ex:knows ex:b . ex:b ex:name 1 .",
        "ex:S a sh:NodeShape ; sh:targetClass ex:T ;
           sh:property [ sh:path ex:knows ; sh:node ex:Named ] .
         ex:Named a sh:NodeShape ; sh:property ex:NameProp .
         ex:NameProp sh:path ex:name ; sh:datatype xsd:string .",
    );
    assert_eq!(summary(&r), vec!["Node a b"]);
    // validating ex:b directly against ex:Named reports the nested property shape
    let r2 = run(
        "ex:b ex:name 1 .",
        "ex:Named a sh:NodeShape ; sh:targetNode ex:b ; sh:property ex:NameProp .
         ex:NameProp sh:path ex:name ; sh:datatype xsd:string .",
    );
    assert_eq!(summary(&r2), vec!["Datatype b 1"]);
    assert_eq!(short(&r2.results[0].source_shape), "NameProp");
    assert_eq!(
        r2.results[0].result_path,
        Some(PropertyPath::Predicate(oxrdf::NamedNode::new_unchecked(
            "http://ex.org/name"
        )))
    );
}

#[test]
fn qualified_value_shapes() {
    let data = "ex:hand a ex:Hand ; ex:digit ex:t1, ex:f1, ex:f2, ex:f3 .
        ex:t1 a ex:Thumb . ex:f1 a ex:Finger . ex:f2 a ex:Finger . ex:f3 a ex:Finger .";
    assert_results(
        data,
        "ex:S a sh:NodeShape ; sh:targetClass ex:Hand ;
           sh:property [ sh:path ex:digit ; sh:qualifiedValueShape [ sh:class ex:Thumb ] ;
                         sh:qualifiedMinCount 1 ; sh:qualifiedMaxCount 1 ;
                         sh:qualifiedValueShapesDisjoint true ] ;
           sh:property [ sh:path ex:digit ; sh:qualifiedValueShape [ sh:class ex:Finger ] ;
                         sh:qualifiedMinCount 4 ; sh:qualifiedMaxCount 4 ;
                         sh:qualifiedValueShapesDisjoint true ] .",
        &["QualifiedMinCount hand"],
    );
    assert_results(
        data,
        "ex:S a sh:NodeShape ; sh:targetClass ex:Hand ;
           sh:property [ sh:path ex:digit ; sh:qualifiedValueShape [ sh:class ex:Finger ] ;
                         sh:qualifiedMaxCount 2 ] .",
        &["QualifiedMaxCount hand"],
    );
}

#[test]
fn closed_shapes() {
    let r = run(
        "ex:a a ex:T ; ex:p 1 ; ex:q 2 ; ex:r 3 .",
        "ex:S a sh:NodeShape ; sh:targetClass ex:T ; sh:closed true ;
           sh:ignoredProperties ( rdf:type ) ;
           sh:property [ sh:path ex:p ] ; sh:property [ sh:path ex:q ] .",
    );
    assert_eq!(summary(&r), vec!["Closed a 3"]);
    assert_eq!(
        r.results[0].result_path.as_ref().unwrap().to_string(),
        "<http://ex.org/r>"
    );
}

#[test]
fn has_value_and_in() {
    assert_results(
        "ex:a ex:c ex:red . ex:b ex:c ex:blue . ex:d ex:c ex:red, ex:green .",
        "ex:S sh:targetSubjectsOf ex:c ; sh:property [ sh:path ex:c ; sh:hasValue ex:red ;
            sh:in ( ex:red ex:blue ) ] .",
        &["HasValue b", "In d green"],
    );
}

#[test]
fn targets() {
    assert_results(
        "ex:a a ex:C . ex:b a ex:Sub . ex:Sub rdfs:subClassOf ex:C . ex:x ex:p ex:y .",
        "ex:ByClass a sh:NodeShape ; sh:targetClass ex:C ; sh:nodeKind sh:Literal .
         ex:ByNode a sh:NodeShape ; sh:targetNode ex:missing, 42 ; sh:nodeKind sh:BlankNode .
         ex:BySubj a sh:NodeShape ; sh:targetSubjectsOf ex:p ; sh:nodeKind sh:Literal .
         ex:ByObj a sh:NodeShape ; sh:targetObjectsOf ex:p ; sh:nodeKind sh:Literal .
         ex:C2 a rdfs:Class, sh:NodeShape ; sh:nodeKind sh:Literal .",
        &[
            "NodeKind 42 42",
            "NodeKind a a",
            "NodeKind b b",
            "NodeKind missing missing",
            "NodeKind x x",
            "NodeKind y y",
        ],
    );
    // implicit class target
    assert_results(
        "ex:i a ex:C2 .",
        "ex:C2 a rdfs:Class, sh:NodeShape ; sh:nodeKind sh:Literal .",
        &["NodeKind i i"],
    );
}

#[test]
fn deactivated_severity_message() {
    let r = run(
        "ex:a a ex:T .",
        "ex:S1 a sh:NodeShape ; sh:targetClass ex:T ; sh:deactivated true ; sh:nodeKind sh:Literal .
         ex:S2 a sh:NodeShape ; sh:targetClass ex:T ; sh:severity sh:Warning ;
           sh:message \"custom\"@en ; sh:nodeKind sh:Literal .
         ex:S3 a sh:NodeShape ; sh:targetClass ex:T ; sh:node ex:Off .
         ex:Off sh:deactivated true ; sh:nodeKind sh:Literal .",
    );
    assert_eq!(r.results.len(), 1, "{r}");
    assert!(!r.conforms);
    assert_eq!(
        r.results[0].severity.as_str(),
        "http://www.w3.org/ns/shacl#Warning"
    );
    assert_eq!(r.results[0].message(), Some("custom"));
    assert_eq!(r.violations(), 0);
}

#[test]
fn paths() {
    let data = "ex:a ex:p ex:b . ex:b ex:p ex:c . ex:c ex:q ex:d . ex:a ex:r ex:e .";
    let check = |path: &str, expected: &[&str]| {
        // maxCount 0 reports one result; the value nodes are checked with sh:in ()
        let r = run(
            data,
            &format!("ex:S sh:targetNode ex:a ; sh:property [ sh:path {path} ; sh:in ( ) ] ."),
        );
        let mut got: Vec<String> = r
            .results
            .iter()
            .map(|x| short(x.value.as_ref().unwrap()))
            .collect();
        got.sort();
        assert_eq!(got, expected, "path {path}");
    };
    check("ex:p", &["b"]);
    check("( ex:p ex:p )", &["c"]);
    check("( ex:p ex:p ex:q )", &["d"]);
    check("[ sh:alternativePath ( ex:p ex:r ) ]", &["b", "e"]);
    check("[ sh:zeroOrMorePath ex:p ]", &["a", "b", "c"]);
    check("[ sh:oneOrMorePath ex:p ]", &["b", "c"]);
    check("[ sh:zeroOrOnePath ex:p ]", &["a", "b"]);
    check("( [ sh:oneOrMorePath ex:p ] ex:q )", &["d"]);
    let r = run(
        data,
        "ex:S sh:targetNode ex:c ; sh:property [ sh:path [ sh:inversePath [ sh:oneOrMorePath ex:p ] ] ; sh:in ( ) ] .",
    );
    let mut got: Vec<String> = r
        .results
        .iter()
        .map(|x| short(x.value.as_ref().unwrap()))
        .collect();
    got.sort();
    assert_eq!(got, vec!["a", "b"]);
    assert_eq!(
        r.results[0].result_path.as_ref().unwrap().to_string(),
        "^(<http://ex.org/p>+)"
    );
}

#[test]
fn recursive_shapes_terminate() {
    assert_results(
        "ex:a ex:next ex:b . ex:b ex:next ex:a .",
        "ex:L a sh:NodeShape ; sh:targetNode ex:a ;
           sh:property [ sh:path ex:next ; sh:node ex:L ; sh:minCount 1 ] .",
        &[],
    );
}

#[test]
fn sparql_constraint_and_component() {
    assert_results(
        "ex:a ex:label \"x\"@en . ex:b ex:label \"y\"@de .",
        "ex:S a sh:NodeShape ; sh:targetSubjectsOf ex:label ;
           sh:sparql [ sh:select \"SELECT $this ?value WHERE { $this <http://ex.org/label> ?value FILTER(lang(?value) != 'de') }\" ] .",
        &["SPARQL a x"],
    );
    let r = run(
        "ex:a ex:label \"x\"@en . ex:b ex:label \"y\"@de .",
        "ex:LangComp a sh:ConstraintComponent ;
            sh:parameter [ sh:path ex:lang ] ;
            sh:validator [ a sh:SPARQLAskValidator ;
                sh:message \"expected {$lang}\" ;
                sh:ask \"ASK { FILTER(langMatches(lang($value), $lang)) }\" ] .
         ex:S a sh:NodeShape ; sh:targetSubjectsOf ex:label ;
            sh:property [ sh:path ex:label ; ex:lang \"de\" ] .",
    );
    assert_eq!(r.results.len(), 1, "{r}");
    assert_eq!(short(&r.results[0].focus_node), "a");
    assert_eq!(r.results[0].message(), Some("expected de"));
    assert_eq!(
        r.results[0].source_constraint_component.as_str(),
        "http://ex.org/LangComp"
    );
}

#[test]
fn sparql_prebinding_restrictions() {
    let bad = "ex:S sh:targetNode ex:a ; sh:sparql [ sh:select \"SELECT $this WHERE { $this ?p ?o MINUS { $this ?p 1 } }\" ] .";
    assert!(Shapes::parse(&format!("{PREFIXES}{bad}"), RdfFormat::Turtle, None).is_err());
}

#[test]
fn report_rdf_roundtrip() {
    let r = run(
        "ex:a ex:p ex:b . ex:b ex:p ex:c .",
        "ex:S sh:targetNode ex:a ; sh:property [ sh:path ( ex:p [ sh:inversePath ex:q ] ) ; sh:minCount 1 ] ;
           sh:property [ sh:path ex:p ; sh:datatype xsd:string ] .",
    );
    assert_eq!(r.results.len(), 2, "{r}");
    let g = r.to_graph();
    let back = ValidationReport::from_rdf(&g, None).unwrap();
    assert_eq!(back.conforms, r.conforms);
    let mut a: Vec<_> = r
        .results
        .iter()
        .map(|x| (x.result_path.clone(), x.source_constraint_component.clone()))
        .collect();
    let mut b: Vec<_> = back
        .results
        .iter()
        .map(|x| (x.result_path.clone(), x.source_constraint_component.clone()))
        .collect();
    a.sort_by_key(|x| format!("{x:?}"));
    b.sort_by_key(|x| format!("{x:?}"));
    assert_eq!(a, b);
    let ttl = r.to_turtle();
    assert!(ttl.contains("sh:ValidationReport"), "{ttl}");
    assert!(ttl.contains("sh:conforms false"), "{ttl}");
    // the Turtle parses back into an equivalent report
    let parsed = oxrdfio::RdfParser::from_format(RdfFormat::Turtle)
        .for_slice(ttl.as_bytes())
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(parsed.len(), r.to_rdf().len());
}

#[test]
fn named_graph_data_and_shapes_from_store() {
    let s = Store::in_memory(StoreOptions::default());
    s.load(&[
        Source::from_bytes(
            format!("{PREFIXES} ex:a a ex:T .").into_bytes(),
            RdfFormat::Turtle,
            Some(oxrdf::NamedNode::new_unchecked("http://ex.org/data")),
        ),
        Source::from_bytes(
            format!("{PREFIXES} ex:S a sh:NodeShape ; sh:targetClass ex:T ; sh:property [ sh:path ex:p ; sh:minCount 1 ] .")
                .into_bytes(),
            RdfFormat::Turtle,
            Some(oxrdf::NamedNode::new_unchecked("http://ex.org/shapes")),
        ),
    ])
    .unwrap();
    let snap = s.snapshot();
    let shapes = Shapes::from_store(&snap, Some("http://ex.org/shapes")).unwrap();
    assert_eq!(shapes.targeted().count(), 1);
    // default graph is empty → conforms
    let r = validate(&snap, &shapes, &ValidateOptions::default()).unwrap();
    assert!(r.conforms);
    let opts = ValidateOptions {
        data_graph: Some("http://ex.org/data".into()),
        ..Default::default()
    };
    let r = validate(&snap, &shapes, &opts).unwrap();
    assert_eq!(summary(&r), vec!["MinCount a"]);
    let opts = ValidateOptions {
        data_graph: Some("urn:x-arq:UnionGraph".into()),
        ..Default::default()
    };
    assert_eq!(validate(&snap, &shapes, &opts).unwrap().results.len(), 1);
    let r = sparkles_shacl::validate_node(
        &snap,
        &shapes,
        &Term::NamedNode(oxrdf::NamedNode::new_unchecked("http://ex.org/a")),
        &ValidateOptions {
            data_graph: Some("http://ex.org/data".into()),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(r.results.len(), 1);
    assert!(
        validate(
            &snap,
            &shapes,
            &ValidateOptions {
                data_graph: Some("http://ex.org/nope".into()),
                ..Default::default()
            }
        )
        .is_err()
    );
}

#[test]
fn parallel_matches_sequential() {
    let mut data = String::new();
    for i in 0..3000 {
        data.push_str(&format!("ex:n{i} a ex:T ; ex:v {} .\n", i % 7));
    }
    let shapes_ttl =
        "ex:S sh:targetClass ex:T ; sh:property [ sh:path ex:v ; sh:maxInclusive 4 ] .";
    let s = store(&data);
    let sh = shapes(shapes_ttl);
    let par = validate(&s.snapshot(), &sh, &ValidateOptions::default()).unwrap();
    let seq = validate(
        &s.snapshot(),
        &sh,
        &ValidateOptions {
            parallel: false,
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(par, seq);
    assert_eq!(par.results.len(), (0..3000).filter(|i| i % 7 > 4).count());
}
