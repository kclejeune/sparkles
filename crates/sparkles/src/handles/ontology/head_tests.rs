//! The summary's answer on fixtures, pinned in `head_fixtures.json`, and the head's
//! agreement with it (roadmap item 735).
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

const REIFIED_HEADER: &str = ":o a owl:Ontology ; :p \"title\" ; rdfs:label \"o\" .
    [] a owl:Annotation ; owl:annotatedSource :o ; owl:annotatedProperty :p ;
        owl:annotatedTarget \"title\" ; rdfs:comment \"why\" .
    _:r1 a owl:Axiom ; owl:annotatedSource :o ; owl:annotatedProperty rdfs:label ;
        owl:annotatedTarget \"o\" ; rdfs:comment \"first\" .
    [] a owl:Annotation ; owl:annotatedSource _:r1 ; owl:annotatedProperty rdfs:comment ;
        owl:annotatedTarget \"first\" ; rdfs:label \"nested\" .
    :A a owl:Class .";

const NESTED_DECLARATIONS: &str = ":o a owl:Ontology .
    :A a owl:Class .
    _:d1 a owl:Axiom ; owl:annotatedSource :A ; owl:annotatedProperty rdf:type ;
        owl:annotatedTarget owl:Class ; rdfs:comment \"same\" .
    _:d2 a owl:Axiom ; owl:annotatedSource :A ; owl:annotatedProperty rdf:type ;
        owl:annotatedTarget owl:Class ; rdfs:comment \"same\" .
    [] a owl:Annotation ; owl:annotatedSource _:d2 ; owl:annotatedProperty rdfs:comment ;
        owl:annotatedTarget \"same\" ; rdfs:label \"inner\" .
    :B a owl:Class .
    [] a owl:Axiom ; owl:annotatedSource :B ; owl:annotatedProperty rdfs:subClassOf ;
        owl:annotatedTarget :A ; rdfs:comment \"sub\" .
    :B rdfs:subClassOf :A .";

const TYPE_AS_ANNOTATION: &str = ":o a owl:Ontology ; rdfs:label \"o\" .
    rdf:type a owl:AnnotationProperty . :A a owl:Class . :p a owl:ObjectProperty .";

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
    Fixture {
        name: "reified-header",
        data: REIFIED_HEADER,
        args: default_args,
    },
    Fixture {
        name: "reified-header-strict",
        data: REIFIED_HEADER,
        args: strict,
    },
    Fixture {
        name: "nested-declarations",
        data: NESTED_DECLARATIONS,
        args: default_args,
    },
    Fixture {
        name: "type-as-annotation",
        data: TYPE_AS_ANNOTATION,
        args: default_args,
    },
    Fixture {
        name: "type-as-annotation-strict",
        data: TYPE_AS_ANNOTATION,
        args: strict,
    },
    Fixture {
        name: "graphs-repeated",
        data: GRAPHS,
        args: g1_g1,
    },
];

/// The fixtures whose reading needs the whole mapping, so that the head opens the ontology.
const OPENED: &[&str] = &["type-as-annotation", "type-as-annotation-strict"];

fn g1_g1() -> J {
    json!({ "graph": ["http://ex.org/g1", "http://ex.org/g2", "http://ex.org/g1"] })
}

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

/// The head's answer for `f`.
fn head(f: &Fixture) -> J {
    let ds = dataset(f.data);
    let mut args = (f.args)();
    args["view"] = json!("head");
    super::wire::call(&ds, "summary", &args, &QueryOptions::default()).unwrap()
}

/// The members of `full` that the head answers, with `view`.
pub(super) fn head_of(full: &J) -> J {
    let mut j = serde_json::Map::new();
    for k in [
        "dataset",
        "commit",
        "mode",
        "graphs",
        "imports",
        "warnings",
        "inferences",
        "header",
        "entities",
        "closure",
    ] {
        if let Some(v) = full.get(k) {
            j.insert(k.to_string(), v.clone());
        }
    }
    j.insert("view".into(), json!("head"));
    J::Object(j)
}

#[test]
fn the_head_answers_what_the_summary_does() {
    for f in FIXTURES {
        assert_eq!(head(f), head_of(&full(f)), "fixture {}", f.name);
    }
}

#[test]
fn the_head_reads_without_opening_the_ontology() {
    for f in FIXTURES {
        let ds = dataset(f.data);
        let req = super::wire::request_of((f.args)().as_object().unwrap()).unwrap();
        let h = ds.ontology().head(&req, &QueryOptions::default()).unwrap();
        assert_eq!(h.opened, OPENED.contains(&f.name), "fixture {}", f.name);
    }
}

#[test]
fn the_head_refuses_a_bad_view_and_metrics() {
    let ds = dataset(BASIC);
    let o = QueryOptions::default();
    assert!(super::wire::call(&ds, "summary", &json!({"view": "light"}), &o).is_err());
    let both = json!({"view": "head", "metrics": true});
    assert!(super::wire::call(&ds, "summary", &both, &o).is_err());
    let full = super::wire::call(&ds, "summary", &json!({"view": "full"}), &o).unwrap();
    assert!(full.get("axioms").is_some());
}

#[test]
fn the_head_opens_the_ontology_past_its_bounds() {
    // more ontology nodes than the header fragment reads
    let mut data = String::new();
    for n in 0..=super::head::MAX_NODES {
        data.push_str(&format!("<http://ex.org/o{n}> a owl:Ontology . "));
    }
    data.push_str(":A a owl:Class .");
    let ds = Dataset::memory();
    ds.update(&format!("{PREFIXES} INSERT DATA {{ {data} }}"))
        .unwrap();
    let req = super::OntologyRequest::default();
    let o = QueryOptions::default();
    let h = ds.ontology().head(&req, &o).unwrap();
    assert!(h.opened);
    let s = ds.ontology().open(&req, &o).unwrap().summary();
    assert_eq!(h.header, s.header);
    assert_eq!(h.entities, s.entities);
}

fn same(ds: &Dataset, args: J, o: &QueryOptions) {
    let mut full = super::wire::call(ds, "summary", &args, o).unwrap();
    full.as_object_mut().unwrap().remove("cached");
    let mut h = args.clone();
    h["view"] = json!("head");
    let head = super::wire::call(ds, "summary", &h, o).unwrap();
    assert_eq!(head, head_of(&full), "{args}");
}

#[test]
fn the_head_reads_through_the_callers_grants() {
    use crate::access::{GraphAccess, GraphRule, Graphs};
    let ds = dataset(GRAPHS);
    let g = Graphs::Only(GraphRule::new(["http://ex.org/g1"], &[]));
    let o = QueryOptions {
        graphs: Some(std::sync::Arc::new(GraphAccess::graphs(g.clone(), g))),
        ..Default::default()
    };
    same(&ds, g1(), &o);
    same(&ds, g1_g2(), &o);
    let mut h = g1_g2();
    h["view"] = json!("head");
    let head = super::wire::call(&ds, "summary", &h, &o).unwrap();
    assert_eq!(head["entities"], json!({ "Class": 2 }));
}

#[test]
fn the_head_reads_the_inferences_with_their_freshness() {
    let ds = dataset(
        ":o a owl:Ontology ; rdfs:label \"o\" . :A a owl:Class . :B a owl:Class .
        :A rdfs:subClassOf :B . :p a owl:TransitiveProperty .",
    );
    let inferred = json!({ "inferences": true });
    same(&ds, inferred.clone(), &QueryOptions::default());
    ds.reasoning()
        .run(&crate::reasoning::ReasonRequest {
            profile: sparkles_reasoner::Profile::OwlRl,
            ..Default::default()
        })
        .unwrap();
    same(&ds, inferred.clone(), &QueryOptions::default());
    let args = json!({ "inferences": true, "view": "head" });
    let h = super::wire::call(&ds, "summary", &args, &QueryOptions::default()).unwrap();
    assert_eq!(h["inferences"]["included"], json!(true), "{h}");
    same(&ds, json!({}), &QueryOptions::default());
    ds.update(&format!("{PREFIXES} INSERT DATA {{ :C a owl:Class }}"))
        .unwrap();
    same(&ds, inferred, &QueryOptions::default());
}

#[test]
fn the_head_refuses_a_cancelled_or_late_read() {
    use std::sync::Arc;
    use std::sync::atomic::AtomicBool;
    let ds = dataset(BASIC);
    let req = super::OntologyRequest::default();
    let cancelled = QueryOptions {
        cancel: Some(Arc::new(AtomicBool::new(true))),
        ..Default::default()
    };
    assert!(ds.ontology().head(&req, &cancelled).is_err());
    let late = QueryOptions {
        deadline: Some(std::time::Instant::now() - std::time::Duration::from_secs(1)),
        ..Default::default()
    };
    assert!(ds.ontology().head(&req, &late).is_err());
    let empty = Dataset::memory();
    assert!(empty.ontology().head(&req, &cancelled).is_err());
}

#[test]
fn the_head_reads_a_past_state() {
    // a dataset on disk keeps its history
    let dir = tempfile::tempdir().unwrap();
    let ds = Dataset::open(dir.path()).unwrap();
    ds.update(&format!("{PREFIXES} INSERT DATA {{ {BASIC} }}"))
        .unwrap();
    let c0 = ds.store().snapshot().commit;
    ds.update(&format!(
        "{PREFIXES} INSERT DATA {{ :E a owl:Class . :o rdfs:label \"later\" }}"
    ))
    .unwrap();
    let at = json!({ "at": format!("commit:{c0}") });
    same(&ds, at, &QueryOptions::default());
    same(&ds, json!({}), &QueryOptions::default());
}

#[test]
fn the_head_opens_the_ontology_past_its_reified_bound() {
    let mut data = String::from(":o a owl:Ontology . ");
    for n in 0..=super::head::MAX_REIFIED {
        data.push_str(&format!(
            "<http://ex.org/C{n}> a owl:Class . [] a owl:Axiom ; \
             owl:annotatedSource <http://ex.org/C{n}> ; owl:annotatedProperty rdf:type ; \
             owl:annotatedTarget owl:Class ; rdfs:comment \"c\" . "
        ));
    }
    let ds = dataset(&data);
    let req = super::OntologyRequest::default();
    let h = ds.ontology().head(&req, &QueryOptions::default()).unwrap();
    assert!(h.opened);
    same(&ds, json!({}), &QueryOptions::default());
}

#[test]
fn the_head_reads_through_a_masked_view() {
    // grants that hide the requested graph's header, or one graph of a declaration
    use crate::access::{GraphAccess, GraphRule, Graphs};
    let ds = dataset(GRAPHS);
    let g = Graphs::Only(GraphRule::new(
        ["http://ex.org/g2", "http://ex.org/g3"],
        &[],
    ));
    let o = QueryOptions {
        graphs: Some(std::sync::Arc::new(GraphAccess::graphs(g.clone(), g))),
        ..Default::default()
    };
    same(&ds, g1(), &o);
    same(
        &ds,
        json!({ "graph": ["http://ex.org/g1", "http://ex.org/g3"] }),
        &o,
    );
}
