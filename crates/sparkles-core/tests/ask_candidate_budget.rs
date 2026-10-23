//! ASK must not reserve an entire filtered Cartesian product to find one binding.
use oxrdfio::RdfFormat;
use sparkles_core::io::Source;
use sparkles_core::sparql::{ExecutionMode, QueryExecution, QueryOptions, query, query_execution};
use sparkles_core::store::{Store, StoreOptions};
use sparkles_core::{BudgetKind, Error};

fn fixture(n: usize, left: &str, right: &str) -> Store {
    fixture_sizes(n, n, left, right)
}

fn fixture_sizes(left_rows: usize, right_rows: usize, left: &str, right: &str) -> Store {
    let store = Store::in_memory(StoreOptions::default());
    let mut data = String::new();
    for i in 0..left_rows {
        data.push_str(&format!("<urn:left:{i}> <urn:left> {left} .\n"));
    }
    for i in 0..right_rows {
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

#[test]
fn automatic_ask_keeps_successful_cheap_asymmetric_prefixes_eager() {
    for (left, right) in [(1, 2048), (2048, 1)] {
        let store = fixture_sizes(left, right, "\"same\"", "\"same\"");
        let options = QueryOptions {
            max_rows: Some(1024),
            no_cache: true,
            ..Default::default()
        };
        assert!(query(store.snapshot(), ASK, &options).unwrap().boolean);
        let execution = query_execution(
            store.snapshot(),
            ASK,
            &options,
            &Default::default(),
            ExecutionMode::Auto,
        )
        .unwrap();
        let QueryExecution::Eager(answer) = execution else {
            panic!("a bounded cheap eager prefix must stay ordinary")
        };
        assert!(answer.result().boolean);
    }
}

#[test]
fn automatic_ask_budget_failure_keeps_its_partial_execution_plan() {
    use sparkles_core::sparql::query_execution_with_plan;
    let store = fixture(64, "\"left\"", "\"right\"");
    let options = QueryOptions {
        max_rows: Some(1024),
        no_cache: true,
        ..Default::default()
    };
    let failed = query_execution_with_plan(
        store.snapshot(),
        ASK,
        &options,
        &Default::default(),
        ExecutionMode::Auto,
    )
    .err()
    .expect("actual candidates exceed the row budget");
    assert!(matches!(failed.error, Error::BudgetExceeded(b) if b.kind == BudgetKind::Rows));
    let plan = failed
        .plan
        .expect("the automatic ASK keeps the plan as far as it ran");
    assert_eq!(plan.operator, "Limit");
    assert!(!plan.children.is_empty());
    assert!(plan.fidelity().incomplete);
}

#[test]
fn admitted_ask_builds_the_smaller_pure_input_without_renaming_values() {
    let store = fixture_sizes(1, 2048, "\"same\"", "\"same\"");
    // The subquery hides ?left, so the pure left input keeps a projection. The
    // ordinary direct-scan prefix screen does not apply, and native admission
    // must avoid collecting the whole 2048-row right side.
    let ask =
        "ASK { {SELECT ?x WHERE {?left <urn:left> ?x}} ?right <urn:right> ?y FILTER(?x = ?y)}";
    let options = QueryOptions {
        max_rows: Some(1024),
        no_cache: true,
        ..Default::default()
    };
    assert!(query(store.snapshot(), ask, &options).unwrap().boolean);
    let execution = query_execution(
        store.snapshot(),
        ask,
        &options,
        &Default::default(),
        ExecutionMode::Auto,
    )
    .unwrap();
    let QueryExecution::Ask(answer) = execution else {
        panic!("the admitted pure projected input uses bounded ASK")
    };
    assert!(answer.result().boolean);
}

#[test]
fn repeated_variable_scan_estimates_do_not_claim_a_cheap_eager_prefix() {
    let store = Store::in_memory(StoreOptions::default());
    let mut data = String::from(
        "<urn:0shared> <urn:left> <urn:0shared> .\n<urn:0shared> <urn:right> <urn:0shared> .\n",
    );
    for i in 1..160 {
        data.push_str(&format!("<urn:left:{i}> <urn:left> <urn:left:{i}> .\n"));
    }
    for i in 1..800 {
        data.push_str(&format!("<urn:right:{i}> <urn:right> <urn:right:{i}> .\n"));
    }
    store
        .load(&[Source::from_bytes(
            data.into_bytes(),
            RdfFormat::Turtle,
            None,
        )])
        .unwrap();
    let ask = "ASK {?x <urn:left> ?x . ?y <urn:right> ?y FILTER(?x = ?y)}";
    let options = QueryOptions {
        max_rows: Some(1024),
        no_cache: true,
        ..Default::default()
    };
    assert!(
        matches!(query(store.snapshot(), ask, &options), Err(Error::BudgetExceeded(b)) if b.kind == BudgetKind::Rows)
    );
    let execution = query_execution(
        store.snapshot(),
        ask,
        &options,
        &Default::default(),
        ExecutionMode::Auto,
    )
    .unwrap();
    let QueryExecution::Ask(answer) = execution else {
        panic!("a discounted scan estimate cannot prove that the eager prefix fits")
    };
    assert!(answer.result().boolean);
}

#[test]
fn graph_ratio_estimates_do_not_claim_a_cheap_eager_prefix() {
    let store = Store::in_memory(StoreOptions::default());
    let mut data = String::from(
        "<urn:0shared> <urn:left> <urn:0shared> .\n<urn:0shared> <urn:right> <urn:0shared> .\n",
    );
    for i in 1..160 {
        data.push_str(&format!("<urn:left:{i}> <urn:left> <urn:left:{i}> .\n"));
    }
    for i in 1..800 {
        data.push_str(&format!("<urn:right:{i}> <urn:right> <urn:right:{i}> .\n"));
    }
    data.push_str("<urn:named> {\n");
    for i in 0..8640 {
        data.push_str(&format!("<urn:noise:{i}> <urn:noise> <urn:noise:{i}> .\n"));
    }
    data.push_str("}\n");
    store
        .load(&[Source::from_bytes(data.into_bytes(), RdfFormat::TriG, None)])
        .unwrap();
    // Graph-independent predicate counts are discounted by the global default
    // graph ratio. The two predicates here exist entirely in the default graph.
    let ask = "ASK {?x <urn:left> ?lx . ?y <urn:right> ?ry FILTER(?x = ?y)}";
    let options = QueryOptions {
        max_rows: Some(1024),
        no_cache: true,
        ..Default::default()
    };
    assert!(
        matches!(query(store.snapshot(), ask, &options), Err(Error::BudgetExceeded(b)) if b.kind == BudgetKind::Rows)
    );
    let execution = query_execution(
        store.snapshot(),
        ask,
        &options,
        &Default::default(),
        ExecutionMode::Auto,
    )
    .unwrap();
    let QueryExecution::Ask(answer) = execution else {
        panic!("a graph ratio estimate cannot prove that the eager prefix fits")
    };
    assert!(answer.result().boolean);
}

#[test]
fn exhausted_scan_with_failed_row_accounting_remains_incomplete() {
    use sparkles_core::sparql::query_execution_with_plan;
    let store = fixture(64, "\"left\"", "\"right\"");
    let options = QueryOptions {
        max_rows: Some(32),
        no_cache: true,
        ..Default::default()
    };
    let failed = query_execution_with_plan(
        store.snapshot(),
        ASK,
        &options,
        &Default::default(),
        ExecutionMode::Auto,
    )
    .err()
    .expect("build scan exceeds its row cap");
    assert!(matches!(failed.error, Error::BudgetExceeded(b) if b.kind == BudgetKind::Rows));
    let plan = failed.plan.expect("partial execution is preserved");
    let mut todo = vec![&plan];
    let mut failing_scan = None;
    while let Some(node) = todo.pop() {
        if node.operator == "IndexScan" && node.actual_rows == 0 && node.time_ms > 0.0 {
            failing_scan = Some(node);
        }
        todo.extend(&node.children);
    }
    assert!(
        failing_scan
            .expect("the scan attempted an output before its budget failed")
            .fidelity()
            .incomplete
    );
}
