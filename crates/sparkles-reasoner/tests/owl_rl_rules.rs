//! One test per rule of the OWL 2 RL/RDF rule tables (OWL 2 Profiles §4.3, Tables 4 to
//! 9): the `owl-rl` profile entails the rule's conclusion from its premises, or, for a
//! rule whose conclusion is `false`, an inconsistency check reports it. The few rules the
//! profile leaves out are listed as gaps, and the test fails when the table names a rule
//! that none of the three lists.

use sparkles_core::io::{RdfFormat, Source};
use sparkles_core::sparql::{QueryOptions, query};
use sparkles_core::store::{Store, StoreOptions};
use sparkles_reasoner::diagnostics::{DiagnoseOptions, diagnose};
use sparkles_reasoner::{INFERRED_GRAPH, Profile, ReasonOptions, materialize};

const PREFIXES: &str = "@prefix ex: <http://ex.org/> .
@prefix rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#> .
@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
@prefix owl: <http://www.w3.org/2002/07/owl#> .
@prefix xsd: <http://www.w3.org/2001/XMLSchema#> .
";

const SPARQL_PREFIXES: &str = "PREFIX ex: <http://ex.org/>
PREFIX rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#>
PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#>
PREFIX owl: <http://www.w3.org/2002/07/owl#>
PREFIX xsd: <http://www.w3.org/2001/XMLSchema#>
";

/// The rules of Tables 4 to 9, in table order.
const TABLES: &[&str] = &[
    // Table 4
    "eq-ref",
    "eq-sym",
    "eq-trans",
    "eq-rep-s",
    "eq-rep-p",
    "eq-rep-o",
    "eq-diff1",
    "eq-diff2",
    "eq-diff3", // Table 5
    "prp-ap",
    "prp-dom",
    "prp-rng",
    "prp-fp",
    "prp-ifp",
    "prp-irp",
    "prp-symp",
    "prp-asyp",
    "prp-trp",
    "prp-spo1",
    "prp-spo2",
    "prp-eqp1",
    "prp-eqp2",
    "prp-pdw",
    "prp-adp",
    "prp-inv1",
    "prp-inv2",
    "prp-key",
    "prp-npa1",
    "prp-npa2", // Table 6
    "cls-thing",
    "cls-nothing1",
    "cls-nothing2",
    "cls-int1",
    "cls-int2",
    "cls-uni",
    "cls-com",
    "cls-svf1",
    "cls-svf2",
    "cls-avf",
    "cls-hv1",
    "cls-hv2",
    "cls-maxc1",
    "cls-maxc2",
    "cls-maxqc1",
    "cls-maxqc2",
    "cls-maxqc3",
    "cls-maxqc4",
    "cls-oo", // Table 7
    "cax-sco",
    "cax-eqc1",
    "cax-eqc2",
    "cax-dw",
    "cax-adc", // Table 8
    "dt-type1",
    "dt-type2",
    "dt-eq",
    "dt-diff",
    "dt-not-type", // Table 9
    "scm-cls",
    "scm-sco",
    "scm-eqc1",
    "scm-eqc2",
    "scm-op",
    "scm-dp",
    "scm-spo",
    "scm-eqp1",
    "scm-eqp2",
    "scm-dom1",
    "scm-dom2",
    "scm-rng1",
    "scm-rng2",
    "scm-hv",
    "scm-svf1",
    "scm-svf2",
    "scm-avf1",
    "scm-avf2",
    "scm-int",
    "scm-uni",
];

/// Rules with a conclusion: (rule, premises, entailed patterns).
const ENTAILS: &[(&str, &str, &str)] = &[
    ("eq-sym", "ex:a owl:sameAs ex:b .", "ex:b owl:sameAs ex:a"),
    ("eq-trans", "ex:a owl:sameAs ex:b . ex:b owl:sameAs ex:c .", "ex:a owl:sameAs ex:c"),
    ("eq-rep-s", "ex:a owl:sameAs ex:b . ex:a ex:p ex:c .", "ex:b ex:p ex:c"),
    ("eq-rep-p", "ex:p owl:sameAs ex:q . ex:a ex:p ex:c .", "ex:a ex:q ex:c"),
    ("eq-rep-o", "ex:c owl:sameAs ex:d . ex:a ex:p ex:c .", "ex:a ex:p ex:d"),
    (
        "prp-ap",
        "ex:a ex:p ex:b .",
        "rdfs:label a owl:AnnotationProperty . rdfs:seeAlso a owl:AnnotationProperty .
         owl:deprecated a owl:AnnotationProperty . owl:incompatibleWith a owl:AnnotationProperty",
    ),
    ("prp-dom", "ex:p rdfs:domain ex:C . ex:a ex:p ex:b .", "ex:a a ex:C"),
    ("prp-rng", "ex:p rdfs:range ex:C . ex:a ex:p ex:b .", "ex:b a ex:C"),
    (
        "prp-fp",
        "ex:p a owl:FunctionalProperty . ex:a ex:p ex:b , ex:c .",
        "ex:b owl:sameAs ex:c",
    ),
    (
        "prp-ifp",
        "ex:p a owl:InverseFunctionalProperty . ex:a ex:p ex:c . ex:b ex:p ex:c .",
        "ex:a owl:sameAs ex:b",
    ),
    ("prp-symp", "ex:p a owl:SymmetricProperty . ex:a ex:p ex:b .", "ex:b ex:p ex:a"),
    (
        "prp-trp",
        "ex:p a owl:TransitiveProperty . ex:a ex:p ex:b . ex:b ex:p ex:c . ex:c ex:p ex:d .",
        "ex:a ex:p ex:d",
    ),
    ("prp-spo1", "ex:p rdfs:subPropertyOf ex:q . ex:a ex:p ex:b .", "ex:a ex:q ex:b"),
    (
        "prp-spo2",
        "ex:p owl:propertyChainAxiom ( ex:p1 ex:p2 ) .
         ex:q owl:propertyChainAxiom ( ex:p1 ex:p2 ex:p3 ) .
         ex:u0 ex:p1 ex:u1 . ex:u1 ex:p2 ex:u2 . ex:u2 ex:p3 ex:u3 . ex:u3 ex:p4 ex:u4 .
         ex:u4 ex:p5 ex:u5 . ex:u5 ex:p6 ex:u6 . ex:u6 ex:p7 ex:u7 . ex:u7 ex:p8 ex:u8 .",
        "ex:u0 ex:p ex:u2 . ex:u0 ex:q ex:u3",
    ),
    (
        "prp-eqp1",
        "ex:p owl:equivalentProperty ex:q . ex:a ex:p ex:b .",
        "ex:a ex:q ex:b",
    ),
    (
        "prp-eqp2",
        "ex:p owl:equivalentProperty ex:q . ex:a ex:q ex:b .",
        "ex:a ex:p ex:b",
    ),
    ("prp-inv1", "ex:p owl:inverseOf ex:q . ex:a ex:p ex:b .", "ex:b ex:q ex:a"),
    ("prp-inv2", "ex:p owl:inverseOf ex:q . ex:a ex:q ex:b .", "ex:b ex:p ex:a"),
    (
        "prp-key",
        "ex:C owl:hasKey ( ex:id ) . ex:a a ex:C ; ex:id \"7\" . ex:b a ex:C ; ex:id \"7\" .
         ex:D owl:hasKey ( ex:k1 ex:k2 ) . ex:c a ex:D ; ex:k1 ex:v1 ; ex:k2 ex:v2 .
         ex:d a ex:D ; ex:k1 ex:v1 ; ex:k2 ex:v2 . ex:e a ex:D ; ex:k1 ex:v1 ; ex:k2 ex:v3 .",
        "ex:a owl:sameAs ex:b . ex:c owl:sameAs ex:d . FILTER NOT EXISTS { ex:c owl:sameAs ex:e }",
    ),
    ("cls-thing", "ex:a ex:p ex:b .", "owl:Thing a owl:Class"),
    ("cls-nothing1", "ex:a ex:p ex:b .", "owl:Nothing a owl:Class"),
    (
        "cls-int1",
        "ex:C owl:intersectionOf ( ex:A ex:B ) . ex:x a ex:A , ex:B .",
        "ex:x a ex:C",
    ),
    (
        "cls-int2",
        "ex:C owl:intersectionOf ( ex:A ex:B ) . ex:x a ex:C .",
        "ex:x a ex:A , ex:B",
    ),
    ("cls-uni", "ex:C owl:unionOf ( ex:A ex:B ) . ex:x a ex:B .", "ex:x a ex:C"),
    (
        "cls-svf1",
        "ex:R owl:someValuesFrom ex:C ; owl:onProperty ex:p . ex:u ex:p ex:v . ex:v a ex:C .
         ex:S owl:someValuesFrom xsd:integer ; owl:onProperty ex:q . ex:w ex:q 3 .",
        "ex:u a ex:R . ex:w a ex:S",
    ),
    (
        "cls-svf2",
        "ex:R owl:someValuesFrom owl:Thing ; owl:onProperty ex:p . ex:u ex:p ex:v .",
        "ex:u a ex:R",
    ),
    (
        "cls-avf",
        "ex:R owl:allValuesFrom ex:C ; owl:onProperty ex:p . ex:u a ex:R ; ex:p ex:v .",
        "ex:v a ex:C",
    ),
    (
        "cls-hv1",
        "ex:R owl:hasValue ex:v ; owl:onProperty ex:p . ex:u a ex:R .",
        "ex:u ex:p ex:v",
    ),
    (
        "cls-hv2",
        "ex:R owl:hasValue ex:v ; owl:onProperty ex:p . ex:u ex:p ex:v .",
        "ex:u a ex:R",
    ),
    (
        "cls-maxc2",
        "ex:R owl:maxCardinality 1 ; owl:onProperty ex:p . ex:u a ex:R ; ex:p ex:a , ex:b .",
        "ex:a owl:sameAs ex:b",
    ),
    (
        "cls-maxqc3",
        "ex:R owl:maxQualifiedCardinality 1 ; owl:onProperty ex:p ; owl:onClass ex:C .
         ex:u a ex:R ; ex:p ex:a , ex:b . ex:a a ex:C . ex:b a ex:C .",
        "ex:a owl:sameAs ex:b",
    ),
    (
        "cls-maxqc4",
        "ex:R owl:maxQualifiedCardinality 1 ; owl:onProperty ex:p ; owl:onClass owl:Thing .
         ex:u a ex:R ; ex:p ex:a , ex:b .",
        "ex:a owl:sameAs ex:b",
    ),
    ("cls-oo", "ex:C owl:oneOf ( ex:a ex:b ) .", "ex:a a ex:C . ex:b a ex:C"),
    ("cax-sco", "ex:A rdfs:subClassOf ex:B . ex:x a ex:A .", "ex:x a ex:B"),
    ("cax-eqc1", "ex:A owl:equivalentClass ex:B . ex:x a ex:A .", "ex:x a ex:B"),
    ("cax-eqc2", "ex:A owl:equivalentClass ex:B . ex:x a ex:B .", "ex:x a ex:A"),
    (
        "dt-type1",
        "ex:a ex:p ex:b .",
        "xsd:integer a rdfs:Datatype . xsd:string a rdfs:Datatype . rdf:PlainLiteral a rdfs:Datatype .
         xsd:dateTimeStamp a rdfs:Datatype . rdfs:Literal a rdfs:Datatype",
    ),
    (
        "scm-cls",
        "ex:C a owl:Class .",
        "ex:C rdfs:subClassOf ex:C , owl:Thing . ex:C owl:equivalentClass ex:C .
         owl:Nothing rdfs:subClassOf ex:C",
    ),
    (
        "scm-sco",
        "ex:A rdfs:subClassOf ex:B . ex:B rdfs:subClassOf ex:C .",
        "ex:A rdfs:subClassOf ex:C",
    ),
    (
        "scm-eqc1",
        "ex:A owl:equivalentClass ex:B .",
        "ex:A rdfs:subClassOf ex:B . ex:B rdfs:subClassOf ex:A",
    ),
    (
        "scm-eqc2",
        "ex:A rdfs:subClassOf ex:B . ex:B rdfs:subClassOf ex:A .",
        "ex:A owl:equivalentClass ex:B",
    ),
    (
        "scm-op",
        "ex:p a owl:ObjectProperty .",
        "ex:p rdfs:subPropertyOf ex:p . ex:p owl:equivalentProperty ex:p",
    ),
    (
        "scm-dp",
        "ex:p a owl:DatatypeProperty .",
        "ex:p rdfs:subPropertyOf ex:p . ex:p owl:equivalentProperty ex:p",
    ),
    (
        "scm-spo",
        "ex:p rdfs:subPropertyOf ex:q . ex:q rdfs:subPropertyOf ex:r .",
        "ex:p rdfs:subPropertyOf ex:r",
    ),
    (
        "scm-eqp1",
        "ex:p owl:equivalentProperty ex:q .",
        "ex:p rdfs:subPropertyOf ex:q . ex:q rdfs:subPropertyOf ex:p",
    ),
    (
        "scm-eqp2",
        "ex:p rdfs:subPropertyOf ex:q . ex:q rdfs:subPropertyOf ex:p .",
        "ex:p owl:equivalentProperty ex:q",
    ),
    (
        "scm-dom1",
        "ex:p rdfs:domain ex:A . ex:A rdfs:subClassOf ex:B .",
        "ex:p rdfs:domain ex:B",
    ),
    (
        "scm-dom2",
        "ex:q rdfs:domain ex:A . ex:p rdfs:subPropertyOf ex:q .",
        "ex:p rdfs:domain ex:A",
    ),
    (
        "scm-rng1",
        "ex:p rdfs:range ex:A . ex:A rdfs:subClassOf ex:B .",
        "ex:p rdfs:range ex:B",
    ),
    (
        "scm-rng2",
        "ex:q rdfs:range ex:A . ex:p rdfs:subPropertyOf ex:q .",
        "ex:p rdfs:range ex:A",
    ),
    (
        "scm-hv",
        "ex:R1 owl:hasValue ex:i ; owl:onProperty ex:p1 . ex:R2 owl:hasValue ex:i ; owl:onProperty ex:p2 .
         ex:p1 rdfs:subPropertyOf ex:p2 .",
        "ex:R1 rdfs:subClassOf ex:R2",
    ),
    (
        "scm-svf1",
        "ex:R1 owl:someValuesFrom ex:A ; owl:onProperty ex:p . ex:R2 owl:someValuesFrom ex:B ; owl:onProperty ex:p .
         ex:A rdfs:subClassOf ex:B .",
        "ex:R1 rdfs:subClassOf ex:R2",
    ),
    (
        "scm-svf2",
        "ex:R1 owl:someValuesFrom ex:A ; owl:onProperty ex:p1 . ex:R2 owl:someValuesFrom ex:A ; owl:onProperty ex:p2 .
         ex:p1 rdfs:subPropertyOf ex:p2 .",
        "ex:R1 rdfs:subClassOf ex:R2",
    ),
    (
        "scm-avf1",
        "ex:R1 owl:allValuesFrom ex:A ; owl:onProperty ex:p . ex:R2 owl:allValuesFrom ex:B ; owl:onProperty ex:p .
         ex:A rdfs:subClassOf ex:B .",
        "ex:R1 rdfs:subClassOf ex:R2",
    ),
    (
        "scm-avf2",
        "ex:R1 owl:allValuesFrom ex:A ; owl:onProperty ex:p1 . ex:R2 owl:allValuesFrom ex:A ; owl:onProperty ex:p2 .
         ex:p1 rdfs:subPropertyOf ex:p2 .",
        "ex:R2 rdfs:subClassOf ex:R1",
    ),
    (
        "scm-int",
        "ex:C owl:intersectionOf ( ex:A ex:B ) .",
        "ex:C rdfs:subClassOf ex:A , ex:B",
    ),
    (
        "scm-uni",
        "ex:C owl:unionOf ( ex:A ex:B ) .",
        "ex:A rdfs:subClassOf ex:C . ex:B rdfs:subClassOf ex:C",
    ),
];

/// Rules with the conclusion `false`: (rule, premises, the check that reports them).
const CHECKS: &[(&str, &str, &str)] = &[
    (
        "eq-diff1",
        "ex:a owl:sameAs ex:b . ex:a owl:differentFrom ex:b .",
        "same-different",
    ),
    (
        "eq-diff2",
        "[] a owl:AllDifferent ; owl:members ( ex:a ex:b ) . ex:a owl:sameAs ex:b .",
        "all-different",
    ),
    (
        "eq-diff3",
        "[] a owl:AllDifferent ; owl:distinctMembers ( ex:a ex:b ) . ex:a owl:sameAs ex:b .",
        "all-different",
    ),
    (
        "prp-irp",
        "ex:p a owl:IrreflexiveProperty . ex:a ex:p ex:a .",
        "irreflexive-property",
    ),
    (
        "prp-asyp",
        "ex:p a owl:AsymmetricProperty . ex:a ex:p ex:b . ex:b ex:p ex:a .",
        "asymmetric-property",
    ),
    (
        "prp-pdw",
        "ex:p owl:propertyDisjointWith ex:q . ex:a ex:p ex:b ; ex:q ex:b .",
        "disjoint-properties",
    ),
    (
        "prp-adp",
        "[] a owl:AllDisjointProperties ; owl:members ( ex:p ex:q ) . ex:a ex:p ex:b ; ex:q ex:b .",
        "all-disjoint-properties",
    ),
    (
        "prp-npa1",
        "[] owl:sourceIndividual ex:a ; owl:assertionProperty ex:p ; owl:targetIndividual ex:b .
         ex:a ex:p ex:b .",
        "negative-property-assertion",
    ),
    (
        "prp-npa2",
        "[] owl:sourceIndividual ex:a ; owl:assertionProperty ex:p ; owl:targetValue 3 .
         ex:a ex:p 3 .",
        "negative-property-assertion",
    ),
    ("cls-nothing2", "ex:a a owl:Nothing .", "nothing-member"),
    (
        "cls-com",
        "ex:A owl:complementOf ex:B . ex:x a ex:A , ex:B .",
        "complement-classes",
    ),
    (
        "cls-maxc1",
        "ex:R owl:maxCardinality 0 ; owl:onProperty ex:p . ex:u a ex:R ; ex:p ex:v .",
        "max-cardinality-zero",
    ),
    (
        "cls-maxqc1",
        "ex:R owl:maxQualifiedCardinality 0 ; owl:onProperty ex:p ; owl:onClass ex:C .
         ex:u a ex:R ; ex:p ex:v . ex:v a ex:C .",
        "max-qualified-cardinality-zero",
    ),
    (
        "cls-maxqc2",
        "ex:R owl:maxQualifiedCardinality 0 ; owl:onProperty ex:p ; owl:onClass owl:Thing .
         ex:u a ex:R ; ex:p ex:v .",
        "max-qualified-cardinality-zero",
    ),
    (
        "cax-dw",
        "ex:A owl:disjointWith ex:B . ex:x a ex:A , ex:B .",
        "disjoint-classes",
    ),
    (
        "cax-adc",
        "[] a owl:AllDisjointClasses ; owl:members ( ex:A ex:B ) . ex:x a ex:A , ex:B .",
        "all-disjoint-classes",
    ),
    (
        "dt-not-type",
        "ex:a ex:p \"abc\"^^xsd:integer .",
        "literal-not-in-datatype",
    ),
];

/// Rules the profile leaves out, each with what it would add: reflexive `owl:sameAs` for
/// every term (eq-ref), a type for every literal (dt-type2), `owl:sameAs` between
/// literals with equal values (dt-eq), and `owl:differentFrom` between literals with
/// different values (dt-diff). The `owl-rl` profile evaluates literal types where
/// a rule reads them, through `isDType` (cls-svf1 with a datatype above).
const GAPS: &[&str] = &["eq-ref", "dt-type2", "dt-eq", "dt-diff"];

fn store(ttl: &str) -> Store {
    let s = Store::in_memory(StoreOptions::default());
    s.load(&[Source::from_bytes(
        format!("{PREFIXES}{ttl}").into_bytes(),
        RdfFormat::Turtle,
        None,
    )])
    .unwrap_or_else(|e| panic!("{e:#}\n{ttl}"));
    s
}

#[test]
fn every_rule_of_the_tables_has_a_test_or_is_a_listed_gap() {
    let mut named: Vec<&str> = ENTAILS
        .iter()
        .map(|e| e.0)
        .chain(CHECKS.iter().map(|c| c.0))
        .chain(GAPS.iter().copied())
        .collect();
    named.sort_unstable();
    let before = named.len();
    named.dedup();
    assert_eq!(before, named.len(), "a rule is listed twice");
    let mut table: Vec<&str> = TABLES.to_vec();
    table.sort_unstable();
    assert_eq!(named, table);
}

#[test]
fn rules_with_a_conclusion_entail_it() {
    let opts = QueryOptions {
        default_graph_extra: vec![INFERRED_GRAPH.into()],
        ..Default::default()
    };
    for (rule, premises, entailed) in ENTAILS {
        let s = store(premises);
        materialize(&s, &Profile::OwlRl, &ReasonOptions::default())
            .unwrap_or_else(|e| panic!("{rule}: {e:#}"));
        let q = format!("{SPARQL_PREFIXES}ASK {{ {entailed} }}");
        let r = query(s.snapshot(), &q, &opts).unwrap_or_else(|e| panic!("{rule}: {e}"));
        assert!(r.boolean, "{rule}: {premises} does not entail {entailed}");
    }
}

#[test]
fn rules_with_the_conclusion_false_are_reported() {
    for (rule, premises, check) in CHECKS {
        let s = store(premises);
        materialize(&s, &Profile::OwlRl, &ReasonOptions::default())
            .unwrap_or_else(|e| panic!("{rule}: {e:#}"));
        let report = diagnose(
            s.snapshot(),
            &DiagnoseOptions {
                inferences: true,
                ..Default::default()
            },
        )
        .unwrap_or_else(|e| panic!("{rule}: {e:#}"));
        assert!(
            report.findings.iter().any(|f| f.check == *check),
            "{rule}: {premises} gives no '{check}' finding: {:?}",
            report.findings.iter().map(|f| f.check).collect::<Vec<_>>()
        );
    }
}
