//! ASK must not reserve an entire filtered Cartesian product to find one binding.
use oxrdfio::RdfFormat;
use sparkles_core::io::Source;
use sparkles_core::sparql::{ExecutionMode, QueryExecution, QueryOptions, query, query_execution};
use sparkles_core::store::{Store, StoreOptions};
use sparkles_core::{BudgetKind, Error};

fn fixture(n: usize, left: &str, right: &str) -> Store {
    let store = Store::in_memory(StoreOptions::default());
    let mut data = String::new();
    for i in 0..n {
        data.push_str(&format!("<urn:left:{i}> <urn:left> {left} .\n"));
        data.push_str(&format!("<urn:right:{i}> <urn:right> {right} .\n"));
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

const ASK: &str = "ASK { ?left <urn:left> ?x . ?right <urn:right> ?y . FILTER (?x = ?y) }";

#[test]
fn automatic_ask_finds_a_binding_without_reserving_filtered_candidates() {
    // Four thousand candidate pairs, but ASK needs one qualifying binding. The
    // independently known answer also checks value equality rather than term IDs.
    for (left, right) in [
        ("\"same\"", "\"same\""),
        (
            "\"01\"^^<http://www.w3.org/2001/XMLSchema#integer>",
            "\"1.0\"^^<http://www.w3.org/2001/XMLSchema#decimal>",
        ),
        ("\"same\"@en", "\"same\"@EN"),
        (
            "\"2026-01-01T00:00:00Z\"^^<http://www.w3.org/2001/XMLSchema#dateTime>",
            "\"2026-01-01T01:00:00+01:00\"^^<http://www.w3.org/2001/XMLSchema#dateTime>",
        ),
    ] {
        let store = fixture(64, left, right);
        assert!(
            query(store.snapshot(), ASK, &Default::default())
                .unwrap()
                .boolean
        );
        let options = QueryOptions {
            max_rows: Some(1024),
            no_cache: true,
            ..Default::default()
        };
        assert!(matches!(
            query(store.snapshot(), ASK, &options),
            Err(Error::BudgetExceeded(b)) if b.kind == BudgetKind::Rows
        ));
        let execution = query_execution(
            store.snapshot(),
            ASK,
            &options,
            &Default::default(),
            ExecutionMode::Auto,
        )
        .unwrap();
        let QueryExecution::Ask(answer) = execution else {
            panic!("over-budget filtered ASK must use its native cursor")
        };
        assert!(answer.result().boolean, "{left} = {right}");
        assert!(answer.result().rows_produced < 1024);
    }
}

#[test]
fn automatic_ask_keeps_actual_candidate_and_produced_row_limits() {
    let store = fixture(64, "\"left\"", "\"right\"");
    assert!(
        !query(store.snapshot(), ASK, &Default::default())
            .unwrap()
            .boolean
    );
    let options = QueryOptions {
        max_rows: Some(1024),
        no_cache: true,
        ..Default::default()
    };
    // No pair qualifies, so the cursor really does consume too many candidates.
    assert!(matches!(
        query_execution(
            store.snapshot(), ASK, &options, &Default::default(), ExecutionMode::Auto
        ),
        Err(Error::BudgetExceeded(b)) if b.kind == BudgetKind::Rows
    ));
    let options = QueryOptions {
        max_rows: Some(1024),
        no_cache: true,
        max_rows_produced: Some(1),
        ..Default::default()
    };
    assert!(matches!(
        query_execution(
            store.snapshot(), ASK, &options, &Default::default(), ExecutionMode::Auto
        ),
        Err(Error::BudgetExceeded(b)) if b.kind == BudgetKind::RowsProduced
    ));
}

#[test]
fn automatic_ask_avoids_candidate_memory_without_lifting_its_budget() {
    let store = fixture(4096, "\"same\"", "\"same\"");
    let options = QueryOptions {
        max_memory_bytes: Some(4 << 20),
        no_cache: true,
        ..Default::default()
    };
    assert!(matches!(
        query(store.snapshot(), ASK, &options),
        Err(Error::BudgetExceeded(b)) if b.kind == BudgetKind::Memory
    ));
    let execution = query_execution(
        store.snapshot(),
        ASK,
        &options,
        &Default::default(),
        ExecutionMode::Auto,
    )
    .unwrap();
    let QueryExecution::Ask(answer) = execution else {
        panic!("filtered ASK must avoid the over-budget eager candidate table")
    };
    assert!(answer.result().boolean);
    assert!(answer.result().mem_peak_bytes <= 4 << 20);
}

#[test]
fn automatic_ask_can_prove_false_within_its_real_candidate_budget() {
    let store = fixture(512, "\"left\"", "\"right\"");
    let options = QueryOptions {
        max_rows: Some(512 * 512),
        max_memory_bytes: Some(1 << 20),
        no_cache: true,
        ..Default::default()
    };
    let execution = query_execution(
        store.snapshot(),
        ASK,
        &options,
        &Default::default(),
        ExecutionMode::Auto,
    )
    .unwrap();
    let QueryExecution::Ask(answer) = execution else {
        panic!("the oversized eager candidate estimate should use the native ASK")
    };
    assert!(!answer.result().boolean);
    assert!(answer.result().mem_peak_bytes <= 1 << 20);
}

#[test]
fn automatic_ask_preserves_errors_and_small_query_policy() {
    use std::sync::{Arc, atomic::AtomicBool};
    use std::time::Instant;
    let store = fixture(64, "\"same\"", "\"same\"");
    let options = QueryOptions {
        max_rows: Some(8192),
        no_cache: true,
        ..Default::default()
    };
    assert!(matches!(
        query_execution(
            store.snapshot(),
            ASK,
            &options,
            &Default::default(),
            ExecutionMode::Auto,
        )
        .unwrap(),
        QueryExecution::Eager(_)
    ));
    let options = QueryOptions {
        max_rows: Some(1024),
        no_cache: true,
        cancel: Some(Arc::new(AtomicBool::new(true))),
        ..Default::default()
    };
    assert!(matches!(
        query_execution(
            store.snapshot(),
            ASK,
            &options,
            &Default::default(),
            ExecutionMode::Auto,
        ),
        Err(Error::Cancelled)
    ));
    let options = QueryOptions {
        max_rows: Some(1024),
        no_cache: true,
        deadline: Some(Instant::now()),
        ..Default::default()
    };
    assert!(matches!(
        query_execution(
            store.snapshot(),
            ASK,
            &options,
            &Default::default(),
            ExecutionMode::Auto,
        ),
        Err(Error::Timeout)
    ));
    let options = QueryOptions {
        max_rows: Some(1024),
        no_cache: true,
        max_memory_bytes: Some(1),
        ..Default::default()
    };
    assert!(matches!(query_execution(
        store.snapshot(), ASK, &options, &Default::default(), ExecutionMode::Auto,
    ), Err(Error::BudgetExceeded(b)) if b.kind == BudgetKind::Memory));
}
