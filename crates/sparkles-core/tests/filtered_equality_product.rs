//! SP2Bench v1.01 q5a must not reserve nonmatching equality candidates.
//! The fixture retains its six triple patterns and its exact pinned query text.
use oxrdf::{Literal, NamedNode, Term};
use oxrdfio::RdfFormat;
use sparkles_core::io::Source;
use sparkles_core::sparql::{QueryOptions, query};
use sparkles_core::store::{Store, StoreOptions};

const Q5A: &str = include_str!("fixtures/sp2b-q5a.rq");

fn fixture() -> Store {
    fixture_with_suffix("")
}

fn fixture_with_suffix(suffix: &str) -> Store {
    let store = Store::in_memory(StoreOptions {
        result_cache_min_ms: 0.0,
        ..Default::default()
    });
    let mut data = String::from(
        "@prefix rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#> .\n@prefix foaf: <http://xmlns.com/foaf/0.1/> .\n@prefix bench: <http://localhost/vocabulary/bench/> .\n@prefix dc: <http://purl.org/dc/elements/1.1/> .\n",
    );
    for key in 0..32 {
        data.push_str(&format!(
            "<urn:left:{key}> foaf:name \"name:{key}{suffix}\" .\n<urn:right:{key}> foaf:name \"name:{key}{suffix}\" .\n"
        ));
        for duplicate in 0..2 {
            data.push_str(&format!(
                "<urn:article:{key}:{duplicate}> rdf:type bench:Article ; dc:creator <urn:left:{key}> .\n"
            ));
        }
        for duplicate in 0..3 {
            data.push_str(&format!(
                "<urn:inproc:{key}:{duplicate}> rdf:type bench:Inproceedings ; dc:creator <urn:right:{key}> .\n"
            ));
        }
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

fn bag(rows: Vec<Vec<Option<Term>>>) -> Vec<String> {
    let mut result = rows
        .into_iter()
        .map(|r| format!("{r:?}"))
        .collect::<Vec<_>>();
    result.sort();
    result
}

fn expected(copies: usize) -> Vec<String> {
    let mut rows = Vec::new();
    for key in 0..32 {
        let row = vec![
            Some(Term::NamedNode(
                NamedNode::new(format!("urn:left:{key}")).unwrap(),
            )),
            Some(Term::Literal(Literal::new_simple_literal(format!(
                "name:{key}"
            )))),
        ];
        rows.extend(std::iter::repeat_n(row, copies));
    }
    bag(rows)
}

#[test]
fn q5a_fixture_has_an_independent_distinct_and_duplicate_bag_oracle() {
    let store = fixture();
    let options = QueryOptions {
        max_rows: Some(10_000),
        no_cache: true,
        ..Default::default()
    };
    let distinct = query(store.snapshot(), Q5A, &options).unwrap();
    assert_eq!(bag(distinct.rows()), expected(1));
    let query_text = Q5A.replace("SELECT DISTINCT", "SELECT");
    let duplicate_bag = query(store.snapshot(), &query_text, &options).unwrap();
    assert_eq!(bag(duplicate_bag.rows()), expected(6));
}

#[test]
fn q5a_string_equality_avoids_reserving_nonmatching_product_rows() {
    let store = fixture();
    let options = QueryOptions {
        max_rows: Some(512),
        no_cache: true,
        ..Default::default()
    };
    // Both source relations and all 192 matching bag rows fit. The 6,144-row
    // unfiltered product does not fit and must not be reserved before equality.
    let result = query(store.snapshot(), Q5A, &options).unwrap();
    assert_eq!(bag(result.rows()), expected(1));
}

fn equality_candidates(p: &sparkles_core::sparql::PlanInfo) -> Vec<i64> {
    let mut todo = vec![p];
    let mut rows = Vec::new();
    while let Some(p) = todo.pop() {
        if p.description.contains("string equality candidates") {
            rows.push(p.actual_rows);
        }
        todo.extend(&p.children);
    }
    rows
}

#[test]
fn q5a_candidate_index_keeps_bags_orientation_and_the_original_residual() {
    let store = fixture();
    let options = QueryOptions {
        max_rows: Some(512),
        no_cache: true,
        ..Default::default()
    };
    for reversed in [false, true] {
        let text = Q5A.replace("SELECT DISTINCT", "SELECT");
        let text = if reversed {
            text.replace("?name=?name2", "?name2=?name")
        } else {
            text
        };
        let r = query(store.snapshot(), &text, &options).unwrap();
        assert_eq!(bag(r.rows()), expected(6));
        assert_eq!(equality_candidates(&r.plan), [192]);
        let ordinary = query(
            store.snapshot(),
            &text,
            &QueryOptions {
                max_rows: Some(10_000),
                no_cache: true,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(bag(ordinary.rows()), expected(6));
        assert_eq!(ordinary.rows_produced - r.rows_produced, 6_144 - 192);
    }
    let ordinary = query(
        store.snapshot(),
        Q5A,
        &QueryOptions {
            max_rows: Some(10_000),
            no_cache: true,
            ..Default::default()
        },
    )
    .unwrap();
    assert!(
        equality_candidates(&ordinary.plan).is_empty(),
        "cheap products keep their ordinary kernel"
    );
}

#[test]
fn filtered_candidates_never_poison_the_unfiltered_cross_cache_key() {
    let store = fixture();
    let options = QueryOptions {
        max_rows: Some(512),
        ..Default::default()
    };
    for _ in 0..2 {
        assert_eq!(
            bag(query(store.snapshot(), Q5A, &options).unwrap().rows()),
            expected(1)
        );
    }
    assert!(store.result_cache().entries() > 0);
    assert!(store.result_cache().hits() > 0);
    let unfiltered = Q5A
        .replace("FILTER (?name=?name2)", "")
        .replace("SELECT DISTINCT ?person ?name", "SELECT *");
    let result = query(
        store.snapshot(),
        &unfiltered,
        &QueryOptions {
            max_rows: Some(10_000),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(result.len(), 6_144);
    // A cached full product must not prevent the fused filtered query either.
    store.result_cache().clear();
    query(
        store.snapshot(),
        &unfiltered,
        &QueryOptions {
            max_rows: Some(10_000),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(
        bag(query(store.snapshot(), Q5A, &options).unwrap().rows()),
        expected(1)
    );
}

fn values_query(left: &str, right: &str, expression: &str) -> String {
    format!(
        "PREFIX xsd: <http://www.w3.org/2001/XMLSchema#> SELECT ?x ?y WHERE {{ VALUES ?x {{ {left} }} VALUES ?y {{ {right} }} FILTER ({expression}) }}"
    )
}

#[test]
fn numeric_language_error_and_unbound_domains_keep_value_equality() {
    let store = Store::in_memory(StoreOptions::default());
    // The independently known counts include numeric promotion across distinct
    // terms, case-insensitive language tags, and expression errors as filter false.
    for (left, right, count) in [
        ("1 1.0 1e0", "1 1.0 1e0", 9),
        ("9007199254740993", "9007199254740992e0", 1),
        ("\"same\"@en", "\"same\"@EN", 1),
        ("\"a\"^^<urn:unknown>", "\"b\"^^<urn:unknown>", 0),
        ("UNDEF \"same\"", "\"same\"", 1),
        ("\"NaN\"^^xsd:double", "\"NaN\"^^xsd:double", 1),
    ] {
        let text = values_query(left, right, "?x = ?y");
        let result = query(
            store.snapshot(),
            &text,
            &QueryOptions {
                no_cache: true,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(result.len(), count, "{text}");
        assert!(equality_candidates(&result.plan).is_empty());
    }
    // The entire domain must be proved. A numeric outlier must not get discarded
    // by an index built from the strings before it; the original budget still holds.
    let left = std::iter::repeat_n("\"s\"", 16)
        .chain(["1"])
        .collect::<Vec<_>>()
        .join(" ");
    let right = std::iter::repeat_n("\"s\"", 16)
        .chain(["1.0"])
        .collect::<Vec<_>>()
        .join(" ");
    let text = values_query(&left, &right, "?x = ?y");
    let result = query(
        store.snapshot(),
        &text,
        &QueryOptions {
            no_cache: true,
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(result.len(), 257);
    assert!(
        matches!(query(store.snapshot(), &text, &QueryOptions { max_rows: Some(64), no_cache: true, ..Default::default() }), Err(sparkles_core::Error::BudgetExceeded(b)) if b.kind == sparkles_core::BudgetKind::Rows)
    );
}

#[test]
fn matching_output_and_produced_row_limits_are_still_real() {
    let store = Store::in_memory(StoreOptions::default());
    let text = values_query(
        &std::iter::repeat_n("\"s\"", 32)
            .collect::<Vec<_>>()
            .join(" "),
        &std::iter::repeat_n("\"s\"", 32)
            .collect::<Vec<_>>()
            .join(" "),
        "?x = ?y",
    );
    assert!(
        matches!(query(store.snapshot(), &text, &QueryOptions { max_rows: Some(512), no_cache: true, ..Default::default() }), Err(sparkles_core::Error::BudgetExceeded(b)) if b.kind == sparkles_core::BudgetKind::Rows)
    );
    let store = fixture();
    let failed = sparkles_core::sparql::query_with_plan(
        store.snapshot(),
        Q5A,
        &QueryOptions {
            max_rows: Some(512),
            max_rows_produced: Some(1_200),
            no_cache: true,
            ..Default::default()
        },
    )
    .err()
    .expect("matching candidates must retain their produced-row limit");
    assert!(
        matches!(failed.error, sparkles_core::Error::BudgetExceeded(b) if b.kind == sparkles_core::BudgetKind::RowsProduced)
    );
    let plan = failed.plan.unwrap();
    let mut todo = vec![&plan];
    let cross = loop {
        let p = todo
            .pop()
            .expect("failure must retain its Cartesian product plan");
        if p.operator == "CartesianProduct" {
            break p;
        }
        todo.extend(&p.children);
    };
    assert!(cross.fidelity.as_ref().unwrap().incomplete);
    assert_eq!(
        cross
            .children
            .iter()
            .map(|p| p.actual_rows)
            .collect::<Vec<_>>(),
        [64, 96]
    );
}

#[test]
fn expressions_in_inputs_or_the_filter_do_not_move_evaluation_boundaries() {
    let store = Store::in_memory(StoreOptions::default());
    let values = (0..32)
        .map(|i| format!("\"v{i}\""))
        .collect::<Vec<_>>()
        .join(" ");
    for text in [
        values_query(&values, &values, "?x = ?y || RAND() < 0"),
        format!(
            "SELECT ?x ?y WHERE {{ {{ VALUES ?v {{ {values} }} BIND(STR(?v) AS ?x) }} VALUES ?y {{ {values} }} FILTER (?x = ?y) }}"
        ),
    ] {
        assert!(
            matches!(query(store.snapshot(), &text, &QueryOptions { max_rows: Some(512), no_cache: true, ..Default::default() }), Err(sparkles_core::Error::BudgetExceeded(b)) if b.kind == sparkles_core::BudgetKind::Rows),
            "{text}"
        );
    }
}

#[test]
fn equality_index_is_charged_and_empty_products_and_cancellation_stay_safe() {
    let store = fixture();
    let memory = 32 << 10;
    let result = query(
        store.snapshot(),
        Q5A,
        &QueryOptions {
            max_rows: Some(10_000),
            max_memory_bytes: Some(memory),
            no_cache: true,
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(bag(result.rows()), expected(1));
    assert_eq!(equality_candidates(&result.plan), [192]);
    assert!(result.mem_peak_bytes <= memory);
    assert!(matches!(query(store.snapshot(), Q5A, &QueryOptions {
        max_rows: Some(512), max_memory_bytes: Some(1_024), no_cache: true, ..Default::default()
    }), Err(sparkles_core::Error::BudgetExceeded(b)) if b.kind == sparkles_core::BudgetKind::Memory));
    let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
    assert!(matches!(
        query(
            store.snapshot(),
            Q5A,
            &QueryOptions {
                max_rows: Some(512),
                cancel: Some(cancel),
                no_cache: true,
                ..Default::default()
            }
        ),
        Err(sparkles_core::Error::Cancelled)
    ));
    let empty = Store::in_memory(StoreOptions::default());
    for text in [
        values_query("", "\"s\"", "?x=?y"),
        values_query("\"s\"", "", "?x=?y"),
    ] {
        assert!(
            query(
                empty.snapshot(),
                &text,
                &QueryOptions {
                    max_rows: Some(1),
                    no_cache: true,
                    ..Default::default()
                }
            )
            .unwrap()
            .is_empty()
        );
    }
}

#[test]
fn automatic_execution_uses_the_same_result_without_changing_explicit_streaming() {
    use sparkles_core::sparql::{
        CursorOptions, ExecutionMode, FallbackPolicy, QueryExecution, query_execution,
        select_cursor,
    };
    let store = fixture();
    let opts = QueryOptions {
        max_rows: Some(512),
        no_cache: true,
        ..Default::default()
    };
    let auto = query_execution(
        store.snapshot(),
        Q5A,
        &opts,
        &Default::default(),
        ExecutionMode::Auto,
    )
    .unwrap();
    let QueryExecution::Eager(result) = auto else {
        panic!("this SELECT keeps ordinary automatic policy")
    };
    assert_eq!(bag(result.result().rows()), expected(1));
    for batch_rows in [1, 7, 4096] {
        let mut cursor = select_cursor(
            store.snapshot(),
            Q5A,
            &QueryOptions {
                max_rows: Some(10_000),
                no_cache: true,
                ..Default::default()
            },
            &CursorOptions {
                batch_rows,
                fallback: FallbackPolicy::RejectMaterialization,
                ..Default::default()
            },
        )
        .unwrap();
        let mut rows = Vec::new();
        while let Some(batch) = cursor.next_batch().unwrap() {
            rows.extend((0..batch.len()).map(|i| batch.row(i).unwrap()));
        }
        assert_eq!(bag(rows), expected(1));
    }
}

#[test]
fn retained_string_payloads_must_fit_the_same_query_memory_budget() {
    // The row tables and index slots fit 32 KiB. The independently known string
    // payloads exceed it, even when the eager value cache shares duplicate IDs.
    let store = fixture_with_suffix(&"x".repeat(32 << 10));
    let result = query(
        store.snapshot(),
        Q5A,
        &QueryOptions {
            max_rows: Some(512),
            max_memory_bytes: Some(32 << 10),
            no_cache: true,
            ..Default::default()
        },
    );
    let observed = result
        .as_ref()
        .map(|r| (r.len(), r.mem_peak_bytes))
        .map_err(ToString::to_string);
    assert!(
        matches!(result, Err(sparkles_core::Error::BudgetExceeded(b)) if b.kind == sparkles_core::BudgetKind::Memory),
        "{observed:?}"
    );
}
