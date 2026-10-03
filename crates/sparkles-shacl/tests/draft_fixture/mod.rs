//! The data the drafted-shapes tests of the SHACL and ShEx crates share.

use sparkles_core::io::{RdfFormat, Source};
use sparkles_core::store::{Store, StoreOptions};

const PREFIXES: &str = "@prefix ex: <http://ex.org/> .
@prefix rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#> .
@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
@prefix xsd: <http://www.w3.org/2001/XMLSchema#> .
";

pub const EX: &str = "http://ex.org/";

/// People, employees (a subclass), organisations and documents, with several types per
/// node, mixed datatypes and kinds, an ill-formed integer, language tags, blank nodes,
/// a status enumeration with one misspelt value, and a triple in a named graph.
pub fn fixture() -> String {
    let mut t = String::from(PREFIXES);
    t.push_str(
        "ex:Employee rdfs:subClassOf ex:Person .\nex:Manager rdfs:subClassOf ex:Employee .\n",
    );
    for i in 0..20 {
        let status = match i {
            7 => "actve",
            i if i % 3 == 0 => "inactive",
            _ => "active",
        };
        t.push_str(&format!(
            "ex:p{i} a ex:Person ; ex:name \"Person {i}\" ; ex:status \"{status}\" ; ex:knows ex:p{} ;\n  rdfs:label \"P{i}\"@en, \"P{i}\"@de .\n",
            (i + 1) % 20
        ));
        if i % 4 != 0 {
            t.push_str(&format!("ex:p{i} ex:age {} .\n", 20 + i));
        }
    }
    // an ill-formed integer, a second name, a homepage as an IRI and as a string
    t.push_str("ex:p4 ex:age \"forty\"^^xsd:integer .\nex:p5 ex:name \"P. Five\" .\n");
    t.push_str("ex:p6 ex:homepage <http://p6.example/> .\nex:p8 ex:homepage \"p8.example\" .\n");
    // two labels in one language
    t.push_str("ex:p9 rdfs:label \"Nine\"@en .\n");
    // several types on one node
    t.push_str("ex:p3 a ex:Agent ; ex:email \"p3@example.org\" .\n");
    for i in 0..6 {
        t.push_str(&format!(
            "ex:e{i} a ex:Employee ; ex:name \"Employee {i}\" ; ex:salary {i}000.50 ; ex:status \"active\" ;\n  ex:knows [ a ex:Person ; ex:name \"Anon {i}\" ; ex:status \"active\" ] .\n"
        ));
    }
    t.push_str("ex:m1 a ex:Manager ; ex:name \"Boss\" ; ex:status \"active\" ; ex:reports ex:e1, ex:e2 .\n");
    for i in 0..5 {
        t.push_str(&format!(
            "ex:o{i} a ex:Org ; rdfs:label \"Org {i}\"@en ; ex:member ex:p{i}, ex:e{i} ; ex:founded \"20{i:02}-01-01\"^^xsd:date .\n"
        ));
    }
    t.push_str("ex:o4 ex:member ex:m1 ; ex:founded \"2004-06-01\"^^xsd:date .\n");
    t.push_str("ex:d1 a ex:Doc ; ex:about <<( ex:p1 ex:knows ex:p2 )>> ; ex:flag true .\n");
    t.push_str("ex:d2 a ex:Doc ; ex:about ex:p2 ; ex:flag false .\n");
    t.push_str("ex:d3 a ex:Doc ; ex:flag true .\n");
    t.push_str("ex:d4 a ex:Doc ; ex:flag true .\n");
    t.push_str("GRAPH ex:g { ex:p1 ex:secret \"hidden\" } \n");
    t
}

pub fn store(trig: &str) -> Store {
    let s = Store::in_memory(StoreOptions::default());
    s.load(&[Source::from_bytes(
        trig.as_bytes().to_vec(),
        RdfFormat::TriG,
        None,
    )])
    .unwrap();
    s
}

/// A small random dataset: typed nodes, a random subclass graph, and values of every
/// kind, including blank nodes, triple terms, ill-formed and language-tagged literals.
pub fn random_data(seed: u64) -> String {
    let mut x = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
    let mut next = move |n: u64| {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        x % n
    };
    let mut t = String::from(
        "@prefix ex: <http://ex.org/> .\n@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .\n@prefix xsd: <http://www.w3.org/2001/XMLSchema#> .\n",
    );
    for c in 0..4 {
        if next(3) == 0 {
            t.push_str(&format!("ex:C{c} rdfs:subClassOf ex:C{} .\n", next(4)));
        }
    }
    for n in 0..30 {
        for _ in 0..=next(2) {
            t.push_str(&format!("ex:n{n} a ex:C{} .\n", next(4)));
        }
        for p in 0..5 {
            for _ in 0..next(3) {
                let v = match next(11) {
                    0 => format!("ex:n{}", next(30)),
                    1 => "[ a ex:C1 ]".to_string(),
                    2 => format!("{}", next(5)),
                    3 => format!("\"s{}\"", next(3)),
                    4 => format!("\"l{}\"@{}", next(3), ["en", "de", "fr"][next(3) as usize]),
                    5 => "\"bad\"^^xsd:integer".to_string(),
                    6 => ["true", "false"][next(2) as usize].to_string(),
                    7 => format!("\"2020-01-0{}\"^^xsd:date", 1 + next(9)),
                    8 => format!("<<( ex:n{} ex:p0 ex:n1 )>>", next(30)),
                    9 => format!("{}.5", next(4)),
                    _ => format!("ex:v{}", next(4)),
                };
                t.push_str(&format!("ex:n{n} ex:p{p} {v} .\n"));
            }
        }
    }
    t
}
