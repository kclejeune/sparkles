//! Configurable DESCRIBE (spec G06 Phase 2): the default description against Jena 6.2.0's
//! answers, and the `cbd`, `scbd` and `outgoing` modes, reifiers, labels, limits, the
//! query's dataset and the dataset's stored setting.
//!
//! "arq 6.2.0" marks the answers of Jena 6.2.0's `arq --data d.trig` for the data below,
//! whose default handler is `DescribeBNodeClosure`. Graphs are compared after blank
//! nodes are canonicalized.

use oxrdf::dataset::CanonicalizationAlgorithm;
use oxrdf::{Graph, Triple};
use sparkles::Error;
use sparkles::io::{RdfFormat, Source};
use sparkles::sparql::describe::{DescribeMode, DescribeOptions};
use sparkles::sparql::{QueryOptions, query};
use sparkles::store::{DESCRIBE_FILE, Store, StoreOptions};

const PREFIXES: &str = "PREFIX ex: <http://example.org/>
PREFIX rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#>
PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#>
PREFIX skos: <http://www.w3.org/2004/02/skos/core#>
";

const DATA: &str = "
ex:a ex:p ex:b ; ex:q [ ex:r [ ex:s \"deep\" ] ; ex:t ex:c ] ; rdfs:label \"A\" .
ex:b rdfs:label \"B\" ; skos:prefLabel \"Bee\"@en ; ex:more \"not a label\" .
ex:c skos:prefLabel \"C\" .
ex:z ex:link ex:a .
_:in ex:link ex:a ; ex:about \"inbound blank\" .
_:in2 ex:x _:in .
ex:a ex:p ex:b {| ex:source ex:wiki |} .
ex:c1 ex:p _:c .
_:c ex:p _:d .
_:d ex:p _:c .
ex:m ex:p2 _:shared .
GRAPH ex:g1 { ex:a ex:inG1 [ ex:n 1 ] . ex:y ex:link ex:a . }
GRAPH ex:g2 { ex:other ex:p ex:a . }
GRAPH ex:g3 { ex:m ex:p _:shared . }
GRAPH ex:g4 { _:shared ex:q \"in g4\" . }
";

/// What arq 6.2.0 answers to `DESCRIBE ex:a`.
const JENA_A: &str = "
ex:a rdfs:label \"A\" ; ex:inG1 [ ex:n 1 ] ; ex:p ex:b ;
     ex:q [ ex:r [ ex:s \"deep\" ] ; ex:t ex:c ] .";

/// The reifier of `ex:a ex:p ex:b`, which CBD adds to Jena's answer.
const REIFIER: &str = "
[] rdf:reifies <<( ex:a ex:p ex:b )>> ; ex:source ex:wiki .";

fn store_with(opts: StoreOptions) -> Store {
    let s = Store::in_memory(opts);
    let data = format!("{PREFIXES}{DATA}");
    s.load(&[Source::from_bytes(data.into_bytes(), RdfFormat::TriG, None)])
        .unwrap();
    s
}

fn store() -> Store {
    store_with(StoreOptions::default())
}

fn canon(triples: impl IntoIterator<Item = Triple>) -> Graph {
    let mut g: Graph = triples.into_iter().collect();
    g.canonicalize(CanonicalizationAlgorithm::Unstable);
    g
}

fn turtle(ttl: &str) -> Graph {
    let text = format!("{PREFIXES}{ttl}");
    let (quads, _) = sparkles::io::parse_to_vec(&Source::from_bytes(
        text.into_bytes(),
        RdfFormat::Turtle,
        None,
    ))
    .unwrap();
    canon(quads.into_iter().map(Triple::from))
}

fn run(s: &Store, q: &str, d: &DescribeOptions) -> sparkles::sparql::QueryResult {
    let opts = QueryOptions {
        describe: d.clone(),
        ..Default::default()
    };
    query(s.snapshot(), &format!("{PREFIXES}{q}"), &opts).unwrap_or_else(|e| panic!("{q}: {e}"))
}

fn describe(s: &Store, q: &str, d: &DescribeOptions) -> Graph {
    canon(run(s, q, d).triples)
}

fn check(s: &Store, q: &str, d: &DescribeOptions, want: &str) {
    let got = describe(s, q, d);
    let want = turtle(want);
    assert!(got == want, "{q} with {d:?}\ngot:\n{got}\nwant:\n{want}");
}

fn mode(m: DescribeMode) -> DescribeOptions {
    DescribeOptions {
        mode: m,
        ..Default::default()
    }
}

/// Jena's handler: CBD without reifiers.
fn jena() -> DescribeOptions {
    DescribeOptions {
        reifiers: false,
        ..Default::default()
    }
}

#[test]
fn the_default_without_reifiers_matches_jena() {
    let s = store();
    let cases: &[(&str, &str)] = &[
        ("DESCRIBE ex:a", JENA_A),
        (
            "DESCRIBE ?x WHERE { ?x ex:link ex:a }",
            "ex:z ex:link ex:a . [ ex:about \"inbound blank\" ; ex:link ex:a ] .",
        ),
        // only in a named graph
        ("DESCRIBE ex:other", "ex:other ex:p ex:a ."),
        // a blank node is followed in the graph it was found in only
        ("DESCRIBE ex:m", "ex:m ex:p _:b0 ; ex:p2 _:b0 ."),
        // a cycle of blank nodes
        (
            "DESCRIBE ex:c1",
            "_:b0 ex:p [ ex:p _:b0 ] . ex:c1 ex:p _:b0 .",
        ),
        ("DESCRIBE ?g WHERE { GRAPH ?g { ex:a ?p ?o } }", ""),
        ("DESCRIBE ex:nothing", ""),
        (
            "DESCRIBE ?o WHERE { ex:a ex:p ?o }",
            "ex:b rdfs:label \"B\" ; ex:more \"not a label\" ; skos:prefLabel \"Bee\"@en .",
        ),
        (
            "DESCRIBE ex:a ex:b",
            &format!(
                "{JENA_A} ex:b rdfs:label \"B\" ; ex:more \"not a label\" ; \
                 skos:prefLabel \"Bee\"@en ."
            ),
        ),
        (
            "DESCRIBE * WHERE { ex:c1 ex:p ?x }",
            "_:b0 ex:p _:b1 . _:b1 ex:p _:b0 .",
        ),
    ];
    for (q, want) in cases {
        check(&s, q, &jena(), want);
    }
}

#[test]
fn cbd_adds_the_reifiers_of_included_triples() {
    let s = store();
    let cbd = DescribeOptions::default();
    assert_eq!(cbd.mode, DescribeMode::Cbd);
    check(&s, "DESCRIBE ex:a", &cbd, &format!("{JENA_A}{REIFIER}"));
    // a description without a reified triple is Jena's
    check(&s, "DESCRIBE ex:other", &cbd, "ex:other ex:p ex:a .");
    // the reifier of a reifier's triple, and RDF 1.1 reification
    let r = Store::in_memory(StoreOptions::default());
    let data = format!(
        "{PREFIXES}
        ex:s ex:p ex:o {{| ex:by ex:bob {{| ex:checked true |}} |}} .
        ex:s ex:q ex:o2 .
        _:st a rdf:Statement ; rdf:subject ex:s ; rdf:predicate ex:q ; rdf:object ex:o2 ;
             ex:said ex:carol .
        _:other rdf:subject ex:s ; rdf:predicate ex:q ; rdf:object ex:elsewhere ."
    );
    r.load(&[Source::from_bytes(
        data.into_bytes(),
        RdfFormat::Turtle,
        None,
    )])
    .unwrap();
    check(
        &r,
        "DESCRIBE ex:s",
        &cbd,
        "ex:s ex:p ex:o ; ex:q ex:o2 .
         _:r1 rdf:reifies <<( ex:s ex:p ex:o )>> ; ex:by ex:bob .
         _:r2 rdf:reifies <<( _:r1 ex:by ex:bob )>> ; ex:checked true .
         _:st a rdf:Statement ; rdf:subject ex:s ; rdf:predicate ex:q ; rdf:object ex:o2 ;
              ex:said ex:carol .",
    );
    check(
        &r,
        "DESCRIBE ex:s",
        &jena(),
        "ex:s ex:p ex:o ; ex:q ex:o2 .",
    );
}

#[test]
fn scbd_adds_the_triples_that_point_at_the_resource() {
    let s = store();
    let scbd = mode(DescribeMode::Scbd);
    // inbound blank-node subjects are followed backwards, not forwards
    let inbound = "ex:z ex:link ex:a . _:in ex:link ex:a . _:in2 ex:x _:in .
                   ex:y ex:link ex:a . ex:other ex:p ex:a .";
    check(
        &s,
        "DESCRIBE ex:a",
        &scbd,
        &format!("{JENA_A}{REIFIER}{inbound}"),
    );
    check(
        &s,
        "DESCRIBE ex:a",
        &DescribeOptions {
            reifiers: false,
            ..scbd.clone()
        },
        &format!("{JENA_A}{inbound}"),
    );
    check(
        &s,
        "DESCRIBE ex:c",
        &scbd,
        "ex:c skos:prefLabel \"C\" . ex:a ex:q [ ex:t ex:c ] .",
    );
}

#[test]
fn outgoing_is_the_resources_own_triples() {
    let s = store();
    check(
        &s,
        "DESCRIBE ex:a",
        &mode(DescribeMode::Outgoing),
        "ex:a rdfs:label \"A\" ; ex:inG1 [] ; ex:p ex:b ; ex:q [] .",
    );
}

#[test]
fn labels_of_linked_iris() {
    let s = store();
    let labels = DescribeOptions {
        labels: true,
        ..Default::default()
    };
    check(
        &s,
        "DESCRIBE ex:a",
        &labels,
        &format!(
            "{JENA_A}{REIFIER} ex:b rdfs:label \"B\" ; skos:prefLabel \"Bee\"@en .
             ex:c skos:prefLabel \"C\" ."
        ),
    );
    check(
        &s,
        "DESCRIBE ex:a",
        &DescribeOptions {
            mode: DescribeMode::Outgoing,
            labels: true,
            ..Default::default()
        },
        "ex:a rdfs:label \"A\" ; ex:inG1 [] ; ex:p ex:b ; ex:q [] .
         ex:b rdfs:label \"B\" ; skos:prefLabel \"Bee\"@en .",
    );
}

#[test]
fn depth_and_size_limits() {
    let s = store();
    let depth = |n| DescribeOptions {
        max_depth: Some(n),
        ..Default::default()
    };
    check(
        &s,
        "DESCRIBE ex:a",
        &depth(1),
        "ex:a rdfs:label \"A\" ; ex:inG1 [] ; ex:p ex:b ; ex:q [] .",
    );
    check(
        &s,
        "DESCRIBE ex:a",
        &depth(2),
        &format!(
            "ex:a rdfs:label \"A\" ; ex:inG1 [ ex:n 1 ] ; ex:p ex:b ;
                  ex:q [ ex:r [] ; ex:t ex:c ] . {REIFIER}"
        ),
    );
    check(
        &s,
        "DESCRIBE ex:a",
        &depth(3),
        &format!("{JENA_A}{REIFIER}"),
    );
    let small = DescribeOptions {
        max_triples: Some(3),
        ..Default::default()
    };
    let r = run(&s, "DESCRIBE ex:a", &small);
    assert_eq!(r.triples.len(), 3);
    assert!(r.describe_truncated);
    assert!(
        r.plan
            .warnings
            .iter()
            .any(|w| w.code == "describe-truncated")
    );
    let exact = DescribeOptions {
        max_triples: Some(10),
        ..Default::default()
    };
    let r = run(&s, "DESCRIBE ex:a", &exact);
    assert_eq!(r.triples.len(), 10);
    assert!(!r.describe_truncated);
}

#[test]
fn the_querys_dataset_is_described() {
    let s = store();
    let d = jena();
    // FROM: the merged default graph, and no named graphs
    check(
        &s,
        "DESCRIBE ex:a FROM ex:g1",
        &d,
        "ex:a ex:inG1 [ ex:n 1 ] .",
    );
    check(
        &s,
        "DESCRIBE ex:m FROM ex:g3 FROM ex:g4",
        &d,
        "ex:m ex:p [ ex:q \"in g4\" ] .",
    );
    // FROM NAMED alone: an empty default graph
    check(
        &s,
        "DESCRIBE ex:a FROM NAMED ex:g1",
        &d,
        "ex:a ex:inG1 [ ex:n 1 ] .",
    );
    check(&s, "DESCRIBE ex:a FROM NAMED ex:g2", &d, "");
    // the protocol's dataset replaces the query's
    let opts = QueryOptions {
        describe: d,
        named_graph_uris: vec!["http://example.org/g2".into()],
        ..Default::default()
    };
    let r = query(
        s.snapshot(),
        &format!("{PREFIXES}DESCRIBE ex:other FROM ex:g1"),
        &opts,
    )
    .unwrap();
    assert_eq!(canon(r.triples), turtle("ex:other ex:p ex:a ."));
}

#[test]
fn a_union_default_graph_reads_the_stored_default_graph_and_each_named_graph() {
    let u = store_with(StoreOptions {
        union_default_graph: true,
        ..Default::default()
    });
    check(&u, "DESCRIBE ex:a", &jena(), JENA_A);
    check(
        &u,
        "DESCRIBE ex:m",
        &jena(),
        "ex:m ex:p _:b0 ; ex:p2 _:b0 .",
    );
}

#[test]
fn materialized_inferences_count_only_through_the_default_graph() {
    let s = store();
    s.load(&[Source::from_bytes(
        b"<http://example.org/other> <http://example.org/inferred> \"x\" <urn:x-sparkles:inferred> .\n"
            .to_vec(),
        RdfFormat::NQuads,
        None,
    )])
    .unwrap();
    check(&s, "DESCRIBE ex:other", &jena(), "ex:other ex:p ex:a .");
    let overlay = QueryOptions {
        describe: jena(),
        default_graph_extra: vec!["urn:x-sparkles:inferred".into()],
        ..Default::default()
    };
    let r = query(
        s.snapshot(),
        &format!("{PREFIXES}DESCRIBE ex:other"),
        &overlay,
    )
    .unwrap();
    assert_eq!(
        canon(r.triples),
        turtle("ex:other ex:p ex:a ; ex:inferred \"x\" .")
    );
}

#[test]
fn options_parse_and_lower() {
    let mut o = DescribeOptions::default();
    o.set("mode", "SCBD").unwrap();
    o.set("labels", "true").unwrap();
    o.set("maxTriples", "100").unwrap();
    o.set("max-depth", "4").unwrap();
    assert_eq!(o.mode, DescribeMode::Scbd);
    assert!(o.labels);
    assert_eq!((o.max_triples, o.max_depth), (Some(100), Some(4)));
    let low = o.clone().lowered(Some(1000), Some(2));
    assert_eq!((low.max_triples, low.max_depth), (Some(100), Some(2)));
    assert!(o.set("mode", "symmetric").is_err());
    assert!(o.set("depth", "1").is_err());
    assert!(o.set("labels", "maybe").is_err());
    o.set("maxTriples", "0").unwrap();
    assert_eq!(o.max_triples, None);
    let j = serde_json::to_value(&o).unwrap();
    assert_eq!(DescribeOptions::from_json(&j).unwrap(), o);
    assert!(DescribeOptions::from_json(&serde_json::json!({"mode": 3})).is_err());
    assert!(DescribeOptions::from_json(&serde_json::json!({"nope": 1})).is_err());
}

#[test]
fn the_dataset_setting_is_kept_in_describe_json() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("db");
    let set = DescribeOptions {
        mode: DescribeMode::Outgoing,
        labels: true,
        max_triples: Some(50),
        ..Default::default()
    };
    {
        let s = Store::open(&root, StoreOptions::default()).unwrap();
        assert!(s.describe_settings().is_default());
        s.set_describe_settings(Some(set.clone())).unwrap();
        assert!(root.join(DESCRIBE_FILE).exists());
    }
    {
        let s = Store::open(&root, StoreOptions::default()).unwrap();
        assert_eq!(s.describe_settings(), set);
        // the defaults remove the file
        s.set_describe_settings(Some(DescribeOptions::default()))
            .unwrap();
        assert!(!root.join(DESCRIBE_FILE).exists());
    }
    std::fs::write(root.join(DESCRIBE_FILE), b"{\"mode\": \"everything\"}").unwrap();
    match Store::open(&root, StoreOptions::default()) {
        Err(Error::Corrupt(m)) => assert!(m.contains(DESCRIBE_FILE), "{m}"),
        Err(e) => panic!("{e}"),
        Ok(_) => panic!("a malformed describe.json opens"),
    }
}

#[test]
fn a_dataset_queries_with_its_setting() {
    let ds = sparkles::Dataset::memory();
    ds.load_str(
        "<http://example.org/a> <http://example.org/p> [ <http://example.org/q> 1 ] .",
        RdfFormat::Turtle,
    )
    .unwrap();
    assert_eq!(
        ds.construct("DESCRIBE <http://example.org/a>")
            .unwrap()
            .len(),
        2
    );
    ds.store()
        .set_describe_settings(Some(mode(DescribeMode::Outgoing)))
        .unwrap();
    assert_eq!(
        ds.construct("DESCRIBE <http://example.org/a>")
            .unwrap()
            .len(),
        1
    );
}
