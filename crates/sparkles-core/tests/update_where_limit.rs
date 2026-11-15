//! The WHERE clause of an update pulls a LIMIT without ORDER BY and an EXISTS pattern
//! only until it has the rows it needs, as a query does (items 827 and 980). Each case
//! reads a pattern whose whole answer is a product of a million rows, under a budget of
//! produced rows that the whole answer exceeds and the pulled prefix fits.
use oxrdfio::RdfFormat;
use sparkles_core::io::Source;
use sparkles_core::sparql::QueryOptions;
use sparkles_core::sparql::update::update;
use sparkles_core::store::{Store, StoreOptions};

const SIDE: usize = 1_000;
const BUDGET: u64 = 300_000;

fn fixture() -> Store {
    let store = Store::in_memory(StoreOptions::default());
    let mut data = String::new();
    for i in 0..SIDE {
        data.push_str(&format!("<urn:a:{i}> <urn:p> <urn:b:{i}> .\n"));
        data.push_str(&format!("<urn:c:{i}> <urn:q> <urn:d:{i}> .\n"));
    }
    store
        .load(&[Source::from_bytes(
            data.into_bytes(),
            RdfFormat::Turtle,
            None,
        )])
        .unwrap();
    store
}

fn options() -> QueryOptions {
    QueryOptions {
        max_rows_produced: Some(BUDGET),
        ..Default::default()
    }
}

fn marked(store: &Store) -> bool {
    sparkles_core::sparql::query(
        store.snapshot(),
        "ASK { <urn:x> <urn:marked> ?v }",
        &QueryOptions::default(),
    )
    .unwrap()
    .boolean
}

#[test]
fn an_update_pulls_a_limited_subquery_only_to_its_limit() {
    let store = fixture();
    let stats = update(
        &store,
        "INSERT { <urn:x> <urn:marked> ?a } WHERE {
           { SELECT ?a WHERE { ?a <urn:p> ?b . ?c <urn:q> ?d } LIMIT 1 }
         }",
        &options(),
    )
    .unwrap();
    assert_eq!(stats.inserted, 1);
    assert!(stats.rows_produced < BUDGET, "{}", stats.rows_produced);
    assert!(marked(&store));
}

#[test]
fn an_update_pulls_a_limited_union_only_to_its_limit() {
    let store = fixture();
    let stats = update(
        &store,
        "INSERT { <urn:x> <urn:marked> ?a } WHERE {
           { SELECT ?a WHERE {
               { ?a <urn:p> ?b . ?c <urn:q> ?d } UNION { ?a <urn:q> ?b . ?c <urn:p> ?d }
             } LIMIT 1 }
         }",
        &options(),
    )
    .unwrap();
    assert_eq!(stats.inserted, 1);
    assert!(stats.rows_produced < BUDGET, "{}", stats.rows_produced);
}

#[test]
fn an_update_asks_its_exists_pattern_for_one_solution() {
    let store = fixture();
    // The pattern shares ?a with the outer row, so it is evaluated for each of the
    // single outer row's bindings, and its first solution answers it.
    let stats = update(
        &store,
        "INSERT { <urn:x> <urn:marked> ?a } WHERE {
           VALUES ?a { <urn:a:7> }
           FILTER EXISTS { ?a <urn:p> ?b . ?c <urn:q> ?d . ?e <urn:p> ?f }
         }",
        &options(),
    )
    .unwrap();
    assert_eq!(stats.inserted, 1);
    assert!(stats.rows_produced < BUDGET, "{}", stats.rows_produced);
    assert!(marked(&store));
}
