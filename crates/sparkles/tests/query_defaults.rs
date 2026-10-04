//! The dataset's query defaults (RDFS on read, the inferred overlay and DESCRIBE) apply
//! to every way the library runs a query: `query`, `query_with`, a stored query, a
//! transaction's query and `explain`. A field the caller sets wins over its default.

use sparkles::Dataset;
use sparkles::io::RdfFormat;
use sparkles::reasoning::rdfs::NewSchema;
use sparkles::reasoning::{INFERRED_GRAPH, ReasoningRecord};
use sparkles::sparql::QueryOptions;
use std::collections::BTreeMap;

const ASK: &str = "ASK { <urn:s> a <urn:parent> }";

fn stored_ask(ds: &Dataset) -> bool {
    let def = serde_json::from_value(serde_json::json!({ "query": ASK })).unwrap();
    ds.queries().put("parent", def, Default::default()).unwrap();
    ds.queries()
        .run("parent", &BTreeMap::new(), &QueryOptions::default())
        .unwrap()
        .boolean
}

fn transaction_ask(ds: &Dataset, opts: &QueryOptions) -> bool {
    ds.transaction(|tx| Ok(tx.query_with(ASK, opts)?.boolean))
        .unwrap()
}

/// Every library path, with the defaults and with options that set no default.
fn assert_every_path(ds: &Dataset, expected: bool) {
    assert_eq!(ds.ask(ASK).unwrap(), expected, "Dataset::ask");
    assert_eq!(
        ds.query_with(ASK, &QueryOptions::default())
            .unwrap()
            .boolean,
        expected,
        "Dataset::query_with"
    );
    assert_eq!(stored_ask(ds), expected, "StoredQueries::run");
    assert_eq!(
        transaction_ask(ds, &QueryOptions::default()),
        expected,
        "Transaction::query_with"
    );
}

#[test]
fn configured_rdfs_applies_to_every_query() {
    let ds = Dataset::memory();
    ds.load_str(
        "<urn:child> <http://www.w3.org/2000/01/rdf-schema#subClassOf> <urn:parent> .
         <urn:s> a <urn:child> .",
        RdfFormat::Turtle,
    )
    .unwrap();
    assert_every_path(&ds, false);
    ds.reasoning()
        .rdfs()
        .set(NewSchema::Graph("default".into()))
        .unwrap();
    assert_every_path(&ds, true);
    assert!(ds.query_options().rdfs.is_some());
    // an explicit field wins: RDFS on read off for one query
    let off = QueryOptions {
        rdfs: None,
        ..ds.query_options()
    };
    assert!(!ds.query_with(ASK, &off).unwrap().boolean);
    assert!(!transaction_ask(&ds, &off));
    ds.reasoning().rdfs().reset().unwrap();
    assert_every_path(&ds, false);
}

#[test]
fn materialized_inferences_are_part_of_the_default_graph() {
    let ds = Dataset::memory();
    ds.load_str("<urn:s> a <urn:child> .", RdfFormat::Turtle)
        .unwrap();
    ds.load_str_into(
        "<urn:s> a <urn:parent> .",
        RdfFormat::Turtle,
        INFERRED_GRAPH,
    )
    .unwrap();
    // without a reasoning record, the inferred graph is a named graph like any other
    assert_every_path(&ds, false);
    ds.state()
        .set_reasoning(Some(ReasoningRecord {
            profile: "rdfs".into(),
            ..Default::default()
        }))
        .unwrap();
    assert_every_path(&ds, true);
    assert_eq!(ds.query_options().default_graph_extra, [INFERRED_GRAPH]);
    // an explicit field wins: the inferences left out of one query
    let mut without = ds.query_options();
    without.default_graph_extra.clear();
    assert!(!ds.query_with(ASK, &without).unwrap().boolean);
    assert!(!transaction_ask(&ds, &without));
    let stored = ds
        .queries()
        .run("parent", &BTreeMap::new(), &without)
        .unwrap();
    assert!(!stored.boolean);
}

#[test]
fn describe_follows_the_setting_unless_the_caller_sets_it() {
    use sparkles::sparql::describe::{DescribeMode, DescribeOptions};
    let ds = Dataset::memory();
    ds.load_str(
        "<urn:a> <urn:p> <urn:b> . <urn:c> <urn:q> <urn:a> .",
        RdfFormat::Turtle,
    )
    .unwrap();
    let q = "DESCRIBE <urn:a>";
    assert_eq!(ds.construct(q).unwrap().len(), 1);
    let symmetric = DescribeOptions {
        mode: DescribeMode::Scbd,
        ..Default::default()
    };
    ds.settings().describe().set(symmetric).unwrap();
    assert_eq!(ds.construct(q).unwrap().len(), 2);
    assert_eq!(
        ds.query_with(q, &QueryOptions::default())
            .unwrap()
            .triples
            .len(),
        2
    );
    // options the caller built on the defaults are used as given
    let given = QueryOptions {
        describe: DescribeOptions::default(),
        ..ds.query_options()
    };
    assert_eq!(ds.query_with(q, &given).unwrap().triples.len(), 1);
}
