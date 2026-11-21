//! The summary's answer on fixtures, pinned in `head_fixtures.json` (roadmap item 735).
//!
//! `SPARKLES_BLESS_HEAD=1` prints the current answers as the fixture file's content
//! between two marker lines instead of comparing them.

use crate::Dataset;
use crate::sparql::QueryOptions;
use serde_json::{Value as J, json};

const PREFIXES: &str = "PREFIX : <http://ex.org/>
PREFIX owl: <http://www.w3.org/2002/07/owl#>
PREFIX rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#>
PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#>
PREFIX xsd: <http://www.w3.org/2001/XMLSchema#>
PREFIX dcterms: <http://purl.org/dc/terms/>
";

/// One fixture: its name, the data inserted and the request's members.
pub(super) struct Fixture {
    pub name: &'static str,
    pub data: &'static str,
    pub args: fn() -> J,
}

fn default_args() -> J {
    json!({})
}

fn strict() -> J {
    json!({ "mode": "strict" })
}

fn g1() -> J {
    json!({ "graph": "http://ex.org/g1" })
}

fn g1_no_imports() -> J {
    json!({ "graph": "http://ex.org/g1", "imports": false })
}

fn g1_g2() -> J {
    json!({ "graph": ["http://ex.org/g1", "http://ex.org/g2"], "imports": false })
}

const BASIC: &str = ":o a owl:Ontology .
    :A a owl:Class . :B a owl:Class . :C a owl:Class .
    :p a owl:ObjectProperty . :d a owl:DatatypeProperty . :n a owl:AnnotationProperty .
    :dt a rdfs:Datatype . :i a owl:NamedIndividual .
    :A rdfs:subClassOf [ a owl:Restriction ; owl:onProperty :p ; owl:someValuesFrom :B ] .
    :a a :A . :a :p :b . :a :d 3 . _:x a owl:Class .";

const OWL1: &str = ":o a owl:Ontology .
    :X a rdfs:Class . :Y a rdfs:Class, rdfs:Datatype . :W a rdfs:Class, owl:Class .
    :V a rdfs:Class, owl:Restriction . :R a owl:Class, owl:Restriction .
    :t a owl:TransitiveProperty . :s a owl:SymmetricProperty . :f a owl:InverseFunctionalProperty .
    :t2 a owl:TransitiveProperty, owl:ObjectProperty . :fp a owl:FunctionalProperty .
    :op a owl:OntologyProperty . :dr a owl:DataRange . :rp a rdf:Property .
    :u rdfs:subClassOf :X .";

const HEADER: &str = ":o a owl:Ontology ; owl:versionIRI :v1 ; owl:imports :missing ;
        rdfs:label \"Onto\"@en ; rdfs:comment \"About\" ; dcterms:license \"MIT\" ;
        :ann \"declared\" ; :undeclared \"x\" ; owl:priorVersion :o0 ;
        rdfs:seeAlso [ rdfs:label \"nested\" ] .
    :o0 a owl:Ontology ; rdfs:label \"old\" .
    :ann a owl:AnnotationProperty .
    [] a owl:Axiom ; owl:annotatedSource :o ; owl:annotatedProperty rdfs:comment ;
        owl:annotatedTarget \"About\" ; rdfs:label \"why\" .
    :A a owl:Class .";

const TWO_NODES: &str =
    ":z a owl:Ontology ; rdfs:label \"z\" . :b a owl:Ontology ; rdfs:label \"b\" .
    :A a owl:Class .";

const BLANK: &str = "[] a owl:Ontology ; rdfs:label \"anonymous\" ; owl:versionIRI :v .
    :A a owl:Class .";

const NO_HEADER: &str = ":A a owl:Class . :B a owl:Class .";

const ANNOTATED: &str = ":o a owl:Ontology .
    :A a owl:Class .
    [] a owl:Axiom ; owl:annotatedSource :A ; owl:annotatedProperty rdf:type ;
        owl:annotatedTarget owl:Class ; rdfs:comment \"decl\" .
    [] a owl:Axiom ; owl:annotatedSource :Z ; owl:annotatedProperty rdf:type ;
        owl:annotatedTarget owl:Class ; rdfs:comment \"reified only\" .
    :B a owl:Class .
    [] a owl:Axiom ; owl:annotatedSource :B ; owl:annotatedProperty rdf:type ;
        owl:annotatedTarget owl:Class ; rdfs:comment \"one\" .
    [] a owl:Axiom ; owl:annotatedSource :B ; owl:annotatedProperty rdf:type ;
        owl:annotatedTarget owl:Class ; rdfs:comment \"two\" .";

const PUNNING: &str = ":o a owl:Ontology .
    :P a owl:Class, owl:NamedIndividual, owl:ObjectProperty .
    :Q a owl:ObjectProperty, owl:DatatypeProperty .
    :N a owl:AnnotationProperty, owl:ObjectProperty .
    owl:Thing a owl:Class . owl:Nothing a owl:Class . rdfs:label a owl:AnnotationProperty .
    xsd:string a rdfs:Datatype . owl:topObjectProperty a owl:ObjectProperty .";

const GRAPHS: &str = "GRAPH :g1 {
        :g1 a owl:Ontology ; owl:imports :g2 ; rdfs:label \"one\" .
        :A a owl:Class . :S a owl:Class .
        [] a owl:Axiom ; owl:annotatedSource :S ; owl:annotatedProperty rdf:type ;
            owl:annotatedTarget owl:Class ; rdfs:comment \"in g1\" .
    }
    GRAPH :g2 {
        :g2 a owl:Ontology ; rdfs:label \"two\" .
        :A a owl:Class . :B a owl:Class . :S a owl:Class . :q a owl:ObjectProperty .
    }
    GRAPH :g3 { :C a owl:Class . }
    :D a owl:Class .";

pub(super) const FIXTURES: &[Fixture] = &[
    Fixture {
        name: "basic",
        data: BASIC,
        args: default_args,
    },
    Fixture {
        name: "basic-strict",
        data: BASIC,
        args: strict,
    },
    Fixture {
        name: "owl1",
        data: OWL1,
        args: default_args,
    },
    Fixture {
        name: "owl1-strict",
        data: OWL1,
        args: strict,
    },
    Fixture {
        name: "header",
        data: HEADER,
        args: default_args,
    },
    Fixture {
        name: "header-strict",
        data: HEADER,
        args: strict,
    },
    Fixture {
        name: "two-nodes",
        data: TWO_NODES,
        args: default_args,
    },
    Fixture {
        name: "blank-node",
        data: BLANK,
        args: default_args,
    },
    Fixture {
        name: "no-header",
        data: NO_HEADER,
        args: default_args,
    },
    Fixture {
        name: "annotated",
        data: ANNOTATED,
        args: default_args,
    },
    Fixture {
        name: "annotated-strict",
        data: ANNOTATED,
        args: strict,
    },
    Fixture {
        name: "punning",
        data: PUNNING,
        args: default_args,
    },
    Fixture {
        name: "punning-strict",
        data: PUNNING,
        args: strict,
    },
    Fixture {
        name: "graphs-imports",
        data: GRAPHS,
        args: g1,
    },
    Fixture {
        name: "graphs-no-imports",
        data: GRAPHS,
        args: g1_no_imports,
    },
    Fixture {
        name: "graphs-two",
        data: GRAPHS,
        args: g1_g2,
    },
];

pub(super) fn dataset(data: &str) -> Dataset {
    let ds = Dataset::memory();
    ds.update(&format!("{PREFIXES} INSERT DATA {{ {data} }}"))
        .unwrap();
    ds
}

/// The summary's answer for `f`, without `cached`, which says only whether the kept copy
/// answered.
pub(super) fn full(f: &Fixture) -> J {
    let ds = dataset(f.data);
    let mut j = super::wire::call(&ds, "summary", &(f.args)(), &QueryOptions::default()).unwrap();
    j.as_object_mut().unwrap().remove("cached");
    j
}

#[test]
fn the_summary_answers_as_pinned() {
    let got: serde_json::Map<String, J> = FIXTURES
        .iter()
        .map(|f| (f.name.to_string(), full(f)))
        .collect();
    let got = J::Object(got);
    if std::env::var_os("SPARKLES_BLESS_HEAD").is_some() {
        println!("----- head_fixtures.json -----");
        println!("{}", serde_json::to_string_pretty(&got).unwrap());
        println!("----- end -----");
        return;
    }
    let want: J = serde_json::from_str(include_str!("head_fixtures.json")).unwrap();
    for f in FIXTURES {
        assert_eq!(got[f.name], want[f.name], "fixture {}", f.name);
    }
    assert_eq!(got, want);
}
