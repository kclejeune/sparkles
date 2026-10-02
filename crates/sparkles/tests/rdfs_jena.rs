//! RDFS on read compared with Apache Jena's (`ja:DatasetRDFS`, Fuseki `--rdfs`).
//!
//! `tests/rdfs` holds TriG data and suites of a schema and queries. The queries cover
//! every pattern shape Jena's `MatchRDFS` tells apart, and the suites the schemas whose
//! answers differ by Jena's rules: a full schema, one without a class hierarchy, and one
//! of subproperties only. `expected.json` holds Jena 6.2's answers, which
//! `scripts/rdfs-jena-expected.sh` regenerates. Answers are compared as sets: Jena returns
//! some derived triples more than once, Sparkles once per graph.

use oxrdf::Term;
use serde_json::Value;
use sparkles::io::{RdfFormat, Source};
use sparkles::sparql::rdfs::{RdfsOnRead, RdfsSchema};
use sparkles::sparql::{QueryKind, QueryOptions, query};
use sparkles::store::{Store, StoreOptions};
use std::collections::BTreeSet;
use std::path::Path;
use std::sync::Arc;

const PREFIXES: &str = "PREFIX ex: <http://example.org/>
PREFIX rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#>
PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#>
";

fn dir() -> &'static Path {
    Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/rdfs"))
}

fn schema(suite: &str) -> RdfsSchema {
    let text = std::fs::read(dir().join(suite).join("schema.ttl")).unwrap();
    let triples: Vec<oxrdf::Triple> = oxttl::TurtleParser::new()
        .for_slice(&text)
        .collect::<Result<_, _>>()
        .unwrap();
    RdfsSchema::from_triples(&triples)
}

fn store() -> Store {
    let s = Store::in_memory(StoreOptions::default());
    let data = std::fs::read(dir().join("data.trig")).unwrap();
    s.load(&[Source::from_bytes(data, RdfFormat::TriG, None)])
        .unwrap();
    s
}

/// A term of Jena's SPARQL JSON results, as N-Triples.
fn jena_term(v: &Value) -> String {
    let value = v["value"].as_str().unwrap();
    match v["type"].as_str().unwrap() {
        "uri" => format!("<{value}>"),
        "bnode" => format!("_:{value}"),
        _ => {
            let lit = format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""));
            match (v["xml:lang"].as_str(), v["datatype"].as_str()) {
                (Some(l), _) => format!("{lit}@{l}"),
                (None, Some(d)) if d != "http://www.w3.org/2001/XMLSchema#string" => {
                    format!("{lit}^^<{d}>")
                }
                _ => lit,
            }
        }
    }
}

type Rows = BTreeSet<Vec<(String, String)>>;

fn jena_rows(r: &Value) -> Rows {
    r["results"]["bindings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|b| {
            let mut row: Vec<(String, String)> = b
                .as_object()
                .unwrap()
                .iter()
                .map(|(k, v)| (k.clone(), jena_term(v)))
                .collect();
            row.sort();
            row
        })
        .collect()
}

/// The queries of a suite answered as Jena answers them; returns how many ran.
fn suite(name: &str, failures: &mut Vec<String>) -> usize {
    let all: Value =
        serde_json::from_slice(&std::fs::read(dir().join("expected.json")).unwrap()).unwrap();
    let expected = &all[name];
    let store = store();
    let opts = QueryOptions {
        rdfs: Some(Arc::new(RdfsOnRead::fixed(schema(name)))),
        ..Default::default()
    };
    let queries = std::fs::read_to_string(dir().join(name).join("queries.txt")).unwrap();
    let mut n = 0;
    for line in queries.lines() {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (q_name, q) = line.split_once('\t').unwrap();
        let name = format!("{name}/{q_name}");
        n += 1;
        let jena = &expected[q_name];
        assert!(
            !jena.is_null(),
            "{name}: no expected answer (rerun the script)"
        );
        let r = query(store.snapshot(), &format!("{PREFIXES}{q}"), &opts)
            .unwrap_or_else(|e| panic!("{name}: {e}"));
        if r.kind == QueryKind::Ask {
            if Some(r.boolean) != jena["boolean"].as_bool() {
                failures.push(format!("{name}: {} (Jena {})", r.boolean, jena["boolean"]));
            }
            continue;
        }
        let ours: Rows = r
            .rows()
            .into_iter()
            .map(|row| {
                let mut v: Vec<(String, String)> = r
                    .vars
                    .iter()
                    .zip(row)
                    .filter_map(|(k, t)| Some((k.clone(), t?.to_string())))
                    .collect();
                v.sort();
                v
            })
            .collect();
        let theirs = jena_rows(jena);
        if ours != theirs {
            failures.push(format!(
                "{name}:\n  only Sparkles: {:?}\n  only Jena: {:?}",
                ours.difference(&theirs).collect::<Vec<_>>(),
                theirs.difference(&ours).collect::<Vec<_>>()
            ));
        }
    }
    n
}

#[test]
fn rdfs_on_read_matches_jena() {
    let mut failures = Vec::new();
    assert!(suite("full", &mut failures) >= 40);
    assert!(suite("properties", &mut failures) >= 8);
    assert!(suite("subproperties", &mut failures) >= 7);
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// Without a schema, and with an empty one, every answer is the stored triples.
#[test]
fn no_schema_changes_nothing() {
    let store = store();
    let empty = QueryOptions {
        rdfs: Some(Arc::new(RdfsOnRead::fixed(RdfsSchema::default()))),
        ..Default::default()
    };
    for opts in [QueryOptions::default(), empty] {
        let r = query(
            store.snapshot(),
            &format!("{PREFIXES}SELECT ?x {{ ?x a ex:Animal }}"),
            &opts,
        )
        .unwrap();
        assert!(r.rows().is_empty());
    }
}

/// A schema of blank-node classes is left out, and counted.
#[test]
fn blank_node_terms_are_skipped() {
    let s = schema("full");
    assert_eq!(s.skipped, 1);
    assert_eq!(s.triples, 15);
}

/// Update WHERE clauses see the derived triples; the update writes stored triples.
#[test]
fn update_where_sees_derived_triples() {
    let store = store();
    let opts = QueryOptions {
        rdfs: Some(Arc::new(RdfsOnRead::fixed(schema("full")))),
        ..Default::default()
    };
    sparkles::sparql::update::update(
        &store,
        &format!(
            "{PREFIXES}INSERT {{ GRAPH ex:out {{ ?x a ex:Animal }} }} WHERE {{ ?x a ex:Animal }}"
        ),
        &opts,
    )
    .unwrap();
    let r = query(
        store.snapshot(),
        &format!("{PREFIXES}SELECT ?x {{ GRAPH ex:out {{ ?x a ex:Animal }} }}"),
        &QueryOptions::default(),
    )
    .unwrap();
    let mut xs: Vec<String> = r
        .rows()
        .into_iter()
        .map(|row| match &row[0] {
            Some(Term::NamedNode(n)) => n.as_str().to_string(),
            other => panic!("{other:?}"),
        })
        .collect();
    xs.sort();
    assert_eq!(xs, ["http://example.org/rex", "http://example.org/tom"]);
}

/// A schema graph of the dataset is read in the state each query sees.
#[test]
fn schema_graph_follows_commits() {
    use sparkles::sparql::rdfs::SchemaSource;
    let store = store();
    let opts = QueryOptions {
        rdfs: Some(Arc::new(RdfsOnRead::new(SchemaSource::Graph(Some(
            "http://example.org/schema".into(),
        ))))),
        ..Default::default()
    };
    let animals = |store: &Store| {
        let q = format!("{PREFIXES}SELECT ?x {{ ?x a ex:Animal }}");
        query(store.snapshot(), &q, &opts).unwrap().rows().len()
    };
    let insert = |triple: &str| {
        let u = format!("{PREFIXES}INSERT DATA {{ GRAPH ex:schema {{ {triple} }} }}");
        sparkles::sparql::update::update(&store, &u, &QueryOptions::default()).unwrap();
    };
    assert_eq!(animals(&store), 0);
    insert("ex:Dog rdfs:subClassOf ex:Animal");
    assert_eq!(animals(&store), 1);
    // ex:tom is a Kitten, which this schema does not place under ex:Cat yet
    insert("ex:Cat rdfs:subClassOf ex:Animal");
    assert_eq!(animals(&store), 1);
    insert("ex:Kitten rdfs:subClassOf ex:Cat");
    assert_eq!(animals(&store), 2);
}
