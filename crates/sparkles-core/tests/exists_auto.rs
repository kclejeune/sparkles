//! Automatic execution of plans that evaluate EXISTS (roadmap item 7). A grouping or
//! count over a large input whose EXISTS patterns have cursor plans streams, and its
//! answers match eager execution exactly.

include!("support/retained_rows_eq.rs");

use oxrdf::Term;
use oxrdfio::RdfFormat;
use sparkles_core::io::Source;
use sparkles_core::sparql::{
    CursorOptions, ExecutionMode, QueryExecution, QueryOptions, query, query_execution,
};
use sparkles_core::store::{Store, StoreOptions};
use sparkles_core::{BudgetKind, Error};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// 60,000 subjects with `p`, two thirds of them with `q` to an object that has `r` for
/// a third of the objects, and half of the subjects also in a named graph.
fn store() -> Store {
    let s = Store::in_memory(StoreOptions::default());
    let mut data = String::new();
    for i in 0..60_000 {
        data.push_str(&format!("<urn:s:{i}> <urn:p> {} .\n", i % 97));
        if i % 3 != 0 {
            data.push_str(&format!("<urn:s:{i}> <urn:q> <urn:o:{}> .\n", i % 1000));
        }
        if i % 2 == 0 {
            data.push_str(&format!(
                "<urn:g:{}> {{ <urn:s:{i}> <urn:p> {} . }}\n",
                i % 4,
                i % 7
            ));
        }
    }
    for o in 0..1000 {
        if o % 3 == 0 {
            data.push_str(&format!("<urn:o:{o}> <urn:r> \"kept\" .\n"));
        }
    }
    s.load(&[Source::from_bytes(data.into_bytes(), RdfFormat::TriG, None)])
        .unwrap();
    s
}

fn bag(rows: impl IntoIterator<Item = impl std::fmt::Debug>) -> Vec<String> {
    let mut rows = rows
        .into_iter()
        .map(|r| format!("{r:?}"))
        .collect::<Vec<_>>();
    rows.sort();
    rows
}

fn ordered(rows: impl IntoIterator<Item = impl std::fmt::Debug>) -> Vec<String> {
    rows.into_iter().map(|r| format!("{r:?}")).collect()
}

/// The rows of `q` under automatic execution, and whether it streamed.
fn automatic(
    s: &Store,
    q: &str,
    opts: &QueryOptions,
    cursor: &CursorOptions,
) -> sparkles_core::Result<(Vec<Vec<Option<Term>>>, bool)> {
    match query_execution(s.snapshot(), q, opts, cursor, ExecutionMode::Auto)? {
        QueryExecution::Select(mut c) => {
            let mut rows = Vec::new();
            while let Some(batch) = c.next_batch()? {
                for row in 0..batch.len() {
                    rows.push(batch.row(row)?.values().to_vec());
                }
            }
            Ok((rows, true))
        }
        QueryExecution::Eager(r) => Ok((
            r.into_result()
                .rows()?
                .iter()
                // Independent assertion rows, matching the cursor fixture branch.
                .map(|row| row.values().to_vec())
                .collect(),
            false,
        )),
        _ => panic!("{q}: not a SELECT"),
    }
}

/// Groupings and counts over 20,000 input rows or more whose EXISTS are FILTER
/// conjuncts that a key set answers: under OPTIONAL, MINUS and GRAPH, nested, and over
/// empty patterns.
const STREAMED: &[&str] = &[
    "SELECT (COUNT(*) AS ?n) { ?s <urn:p> ?v FILTER NOT EXISTS { ?s <urn:q> ?o } }",
    "SELECT (COUNT(*) AS ?n) { ?s <urn:p> ?v FILTER EXISTS { ?s <urn:q> ?o . ?o <urn:r> ?k } }",
    "SELECT ?v (COUNT(*) AS ?n) { ?s <urn:p> ?v FILTER EXISTS { ?s <urn:q> ?o } } GROUP BY ?v ORDER BY ?v",
    "SELECT (COUNT(*) AS ?n) { ?s <urn:p> ?v OPTIONAL { ?s <urn:q> ?o } FILTER NOT EXISTS { ?o <urn:r> ?k } }",
    "SELECT (COUNT(*) AS ?n) { ?s <urn:p> ?v MINUS { ?s <urn:q> ?o FILTER EXISTS { ?o <urn:r> ?k } } }",
    "SELECT (COUNT(*) AS ?n) { ?s <urn:p> ?v FILTER NOT EXISTS { ?s <urn:q> ?o FILTER NOT EXISTS { ?o <urn:r> ?k } } }",
    "SELECT ?g (COUNT(*) AS ?n) { GRAPH ?g { ?s <urn:p> ?v FILTER EXISTS { ?s <urn:p> ?w FILTER(?w > 2) } } } GROUP BY ?g ORDER BY ?g",
    "SELECT (COUNT(*) AS ?n) { ?s <urn:p> ?v FILTER EXISTS { GRAPH ?g { ?s <urn:p> ?w } } }",
    "SELECT (COUNT(*) AS ?n) { ?s <urn:p> ?v FILTER EXISTS { ?s <urn:absent> ?o } }",
    "SELECT (COUNT(*) AS ?n) { ?s <urn:p> ?v FILTER NOT EXISTS { ?s <urn:absent> ?o } }",
];

/// EXISTS in BIND, which the cursor answers from a key set as it does a FILTER EXISTS,
/// so automatic execution streams them.
const KEYED: &[&str] = &[
    "SELECT ?b (COUNT(*) AS ?n) { ?s <urn:p> ?v BIND(EXISTS { ?s <urn:q> ?o } AS ?b) } GROUP BY ?b ORDER BY ?b",
    "SELECT ?b (COUNT(*) AS ?n) { ?s <urn:p> ?v BIND(NOT EXISTS { ?s <urn:q> ?o . ?o <urn:r> ?k } AS ?b) } GROUP BY ?b ORDER BY ?b",
];

/// EXISTS inside a larger expression, an aggregate argument or an ORDER BY key, which
/// both executors answer from the key set of the pattern where it can and otherwise
/// evaluate once per distinct outer key, so automatic execution streams them.
const NESTED: &[&str] = &[
    "SELECT (COUNT(*) AS ?n) { ?s <urn:p> ?v FILTER (?v < 10 || EXISTS { ?s <urn:q> <urn:o:5> }) }",
    "SELECT (COUNT(*) AS ?n) { ?s <urn:p> ?v FILTER (?v < 10 || EXISTS { ?s <urn:q> ?o . ?o <urn:r> ?k }) }",
    "SELECT (SUM(IF(EXISTS { ?s <urn:q> ?o }, 1, 0)) AS ?n) { ?s <urn:p> ?v }",
    "SELECT ?v (SUM(IF(NOT EXISTS { ?s <urn:q> ?o . ?o <urn:r> ?k }, 1, 0)) AS ?n) { ?s <urn:p> ?v } GROUP BY ?v ORDER BY ?v",
    "SELECT ?e (COUNT(*) AS ?n) { ?s <urn:p> ?v BIND(IF(EXISTS { ?s <urn:q> ?o }, \"w\", \"n\") AS ?e) } GROUP BY ?e ORDER BY ?e",
    "SELECT ?e (COUNT(*) AS ?n) { ?s <urn:p> ?v OPTIONAL { ?s <urn:q> ?o } BIND(COALESCE(?v > 50 && EXISTS { ?o <urn:r> ?k }, false) AS ?e) } GROUP BY ?e ORDER BY ?e",
    "SELECT ?e (COUNT(*) AS ?n) { ?s <urn:p> ?v BIND(IF(?s = <urn:s:7>, EXISTS { ?s <urn:q> ?o }, false) AS ?e) } GROUP BY ?e ORDER BY ?e",
    "SELECT ?v (COUNT(?q) AS ?n) { ?s <urn:p> ?v OPTIONAL { ?s <urn:q> ?q } } GROUP BY ?v ORDER BY DESC(EXISTS { ?x <urn:p> ?v . ?x <urn:q> ?o . ?o <urn:r> ?k }) ?v",
    "SELECT (COUNT(*) AS ?n) { ?s <urn:p> ?v FILTER EXISTS { ?s <urn:q> ?o FILTER(?v >= 0) } }",
    "SELECT (COUNT(*) AS ?n) { ?s <urn:p> ?v FILTER NOT EXISTS { ?o <urn:r> ?k } }",
];

/// OPTIONALs whose condition holds an EXISTS. The cursor merges them and tests the
/// condition on each batch's pairs, but automatic execution keeps them eager, because
/// that merge is slower than eager execution's at higher fan-out. The second and third
/// read the condition's right variable nowhere else, and the third's condition also
/// reads the left side.
const OPTIONAL_CONDITION: &[&str] = &[
    "SELECT ?v (COUNT(?o) AS ?n) { ?s <urn:p> ?v OPTIONAL { ?s <urn:q> ?o FILTER EXISTS { ?o <urn:r> ?k } } } GROUP BY ?v ORDER BY ?v",
    "SELECT (COUNT(*) AS ?n) { ?s <urn:p> ?v OPTIONAL { ?s <urn:q> ?o FILTER NOT EXISTS { ?o <urn:r> ?k } } }",
    "SELECT ?s (COUNT(*) AS ?n) { ?s <urn:p> ?v OPTIONAL { ?s <urn:q> ?o FILTER (?v > 50 || EXISTS { ?o <urn:r> ?k }) } } GROUP BY ?s ORDER BY ?s LIMIT 50",
];

#[test]
fn keyed_bind_and_optional_exists_stream_and_match_eager() {
    let s = store();
    for q in KEYED {
        let expected = query(s.snapshot(), q, &Default::default())
            .unwrap()
            .rows()
            .unwrap();
        for rows in [4096, 10_000] {
            let cursor = CursorOptions {
                batch_rows: rows,
                ..Default::default()
            };
            let (got, streamed) = automatic(&s, q, &Default::default(), &cursor)
                .unwrap_or_else(|e| panic!("{q}: {e}"));
            assert!(streamed, "{q}: automatic execution kept the plan eager");
            assert_eq!(ordered(got), ordered(expected.clone()), "{q}; {rows}");
        }
    }
}

#[test]
fn large_groupings_with_exists_stream_and_match_eager() {
    let s = store();
    for q in STREAMED {
        let expected = query(s.snapshot(), q, &Default::default())
            .unwrap_or_else(|e| panic!("{q}: {e}"))
            .rows()
            .unwrap();
        for rows in [4096, 10_000] {
            let cursor = CursorOptions {
                batch_rows: rows,
                ..Default::default()
            };
            let (got, streamed) = automatic(&s, q, &Default::default(), &cursor)
                .unwrap_or_else(|e| panic!("{q}: {e}"));
            assert!(streamed, "{q}: automatic execution kept the plan eager");
            if q.contains("ORDER BY") {
                assert_eq!(ordered(got), ordered(expected.clone()), "{q}; {rows}");
            } else {
                assert_eq!(bag(got), bag(expected.clone()), "{q}; {rows}");
            }
        }
    }
}

/// Automatic execution keeps an OPTIONAL whose condition holds an EXISTS eager, and a
/// strict cursor over it matches eager execution at several batch sizes, with the merge
/// OPTIONAL on and with it off, when both modes hash.
#[test]
fn optional_exists_stays_eager_and_a_strict_cursor_matches() {
    use sparkles_core::sparql::{FallbackPolicy, select_cursor};
    let s = store();
    for q in OPTIONAL_CONDITION {
        for merge in [true, false] {
            let opts = QueryOptions {
                optimizations: Some(sparkles_core::sparql::Optimizations {
                    merge_left_join: merge,
                    ..sparkles_core::sparql::Optimizations::ALL
                }),
                ..Default::default()
            };
            let expected = query(s.snapshot(), q, &opts).unwrap().rows().unwrap();
            for rows in [4096, 10_000] {
                let cursor = CursorOptions {
                    batch_rows: rows,
                    ..Default::default()
                };
                let (got, streamed) =
                    automatic(&s, q, &opts, &cursor).unwrap_or_else(|e| panic!("{q}: {e}"));
                assert!(!streamed, "{q}; merge {merge}: streamed");
                assert_eq!(
                    ordered(got),
                    ordered(expected.clone()),
                    "{q}; merge {merge}; {rows}"
                );
            }
            // A strict cursor runs the conditional merge, at batches small enough
            // that a left row's pairs span several of them.
            for batch_rows in [1, 3, 1000] {
                let cursor = CursorOptions {
                    batch_rows,
                    fallback: FallbackPolicy::RejectMaterialization,
                    ..Default::default()
                };
                let mut c = select_cursor(s.snapshot(), q, &opts, &cursor)
                    .unwrap_or_else(|e| panic!("{q}: {e}"));
                let mut got = Vec::new();
                while let Some(b) = c.next_batch().unwrap() {
                    assert!(b.len() <= batch_rows);
                    for i in 0..b.len() {
                        got.push(b.row(i).unwrap().values().to_vec());
                    }
                }
                assert_eq!(
                    ordered(got),
                    ordered(expected.clone()),
                    "{q}; merge {merge}; strict {batch_rows}"
                );
            }
        }
    }
}

/// An EXISTS inside a larger expression streams, and its answers equal eager execution
/// and evaluation per key with the key sets turned off, at two batch sizes.
#[test]
fn nested_exists_streams_and_matches_eager() {
    let s = store();
    let per_key = QueryOptions {
        optimizations: Some(sparkles_core::sparql::Optimizations {
            decorrelate_exists: false,
            ..sparkles_core::sparql::Optimizations::ALL
        }),
        ..Default::default()
    };
    for q in NESTED {
        let expected = query(s.snapshot(), q, &Default::default())
            .unwrap_or_else(|e| panic!("{q}: {e}"))
            .rows()
            .unwrap();
        let reference = query(s.snapshot(), q, &per_key)
            .unwrap_or_else(|e| panic!("{q}: {e}"))
            .rows()
            .unwrap();
        let sort = |rows: Vec<Vec<Option<Term>>>| {
            if q.contains("ORDER BY") {
                ordered(rows)
            } else {
                bag(rows)
            }
        };
        let expected: Vec<Vec<Option<Term>>> =
            expected.iter().map(|row| row.values().to_vec()).collect();
        let reference: Vec<Vec<Option<Term>>> =
            reference.iter().map(|row| row.values().to_vec()).collect();
        assert_eq!(sort(expected.clone()), sort(reference), "{q}: key set");
        for rows in [4096, 10_000] {
            let cursor = CursorOptions {
                batch_rows: rows,
                ..Default::default()
            };
            let (got, streamed) = automatic(&s, q, &Default::default(), &cursor)
                .unwrap_or_else(|e| panic!("{q}: {e}"));
            assert!(streamed, "{q}: automatic execution kept the plan eager");
            assert_eq!(sort(got), sort(expected.clone()), "{q}; {rows}");
        }
    }
}

#[test]
fn exists_over_a_service_or_a_small_input_stays_eager() {
    let s = store();
    for q in [
        "SELECT (COUNT(*) AS ?n) { ?s <urn:p> ?v FILTER EXISTS { SERVICE <http://example.org/sparql> { ?s ?p ?o } } }",
        // a few hundred input rows do not repay the cursor
        "SELECT (COUNT(*) AS ?n) { ?o <urn:r> ?k FILTER EXISTS { ?s <urn:q> ?o } }",
    ] {
        let opts = QueryOptions {
            allow_service: false,
            ..Default::default()
        };
        let execution = query_execution(
            s.snapshot(),
            q,
            &opts,
            &Default::default(),
            ExecutionMode::Auto,
        );
        match execution {
            Ok(QueryExecution::Eager(_)) | Err(_) => {}
            Ok(_) => panic!("{q}: streamed"),
        }
    }
}

#[test]
fn streamed_exists_honors_cancellation() {
    let s = store();
    let q = STREAMED[0];
    let cancel = Arc::new(AtomicBool::new(false));
    let opts = QueryOptions {
        cancel: Some(cancel.clone()),
        ..Default::default()
    };
    let execution = query_execution(
        s.snapshot(),
        q,
        &opts,
        &Default::default(),
        ExecutionMode::Auto,
    )
    .unwrap();
    let QueryExecution::Select(mut c) = execution else {
        panic!("{q}: not streamed")
    };
    cancel.store(true, Ordering::SeqCst);
    assert!(matches!(c.next_batch(), Err(Error::Cancelled)));
}

/// Under a tight budget a streamed EXISTS either declines its key set and evaluates
/// each distinct key, with the same answer, or fails on memory. It never exceeds its
/// budget or answers differently.
#[test]
fn streamed_exists_stays_within_its_memory_budget() {
    let s = store();
    for q in STREAMED
        .iter()
        .step_by(2)
        .chain(NESTED)
        .chain(OPTIONAL_CONDITION)
    {
        let expected = query(s.snapshot(), q, &Default::default())
            .unwrap()
            .rows()
            .unwrap();
        for memory in [512 << 10, 4 << 20, 64 << 20] {
            let opts = QueryOptions {
                max_memory_bytes: Some(memory),
                ..Default::default()
            };
            match query_execution(
                s.snapshot(),
                q,
                &opts,
                &Default::default(),
                ExecutionMode::Auto,
            ) {
                Ok(QueryExecution::Select(mut c)) => {
                    let mut rows = Vec::new();
                    let outcome = loop {
                        match c.next_batch() {
                            Ok(Some(batch)) => {
                                for row in 0..batch.len() {
                                    rows.push(batch.row(row).unwrap().values().to_vec());
                                }
                            }
                            Ok(None) => break Ok(()),
                            Err(e) => break Err(e),
                        }
                    };
                    match outcome {
                        Ok(()) => assert_eq!(bag(rows), bag(expected.clone()), "{q}; {memory}"),
                        Err(Error::BudgetExceeded(sparkles_core::error::Budget {
                            kind: BudgetKind::Memory,
                            ..
                        })) => {}
                        Err(e) => panic!("{q}; {memory}: {e}"),
                    }
                    assert!(c.stats().mem_peak_bytes <= memory, "{q}; {memory}");
                }
                Ok(QueryExecution::Eager(r)) => {
                    assert_eq!(
                        bag(r.into_result().rows().unwrap()),
                        bag(expected.clone()),
                        "{q}; {memory}"
                    )
                }
                Err(Error::BudgetExceeded(sparkles_core::error::Budget {
                    kind: BudgetKind::Memory,
                    ..
                })) => {}
                Err(e) => panic!("{q}; {memory}: {e}"),
                Ok(_) => panic!("{q}: not a SELECT"),
            }
        }
    }
}

/// A BIND of EXISTS or NOT EXISTS takes its values from the pattern's key set in a
/// strict cursor as well as eagerly, and both give the answers of evaluation per key,
/// including rows that leave a key unbound.
#[test]
fn bind_exists_streams_from_the_key_set() {
    use sparkles_core::sparql::{FallbackPolicy, select_cursor};
    let s = store();
    let per_key = QueryOptions {
        optimizations: Some(sparkles_core::sparql::Optimizations {
            decorrelate_exists: false,
            ..sparkles_core::sparql::Optimizations::ALL
        }),
        ..Default::default()
    };
    for q in [
        KEYED[0],
        KEYED[1],
        "SELECT ?b (COUNT(*) AS ?n) { ?s <urn:p> ?v OPTIONAL { ?s <urn:q> ?o } BIND(EXISTS { ?o <urn:r> ?k } AS ?b) } GROUP BY ?b ORDER BY ?b",
    ] {
        let expected = query(s.snapshot(), q, &per_key).unwrap().rows().unwrap();
        assert_retained_rows_eq!(
            query(s.snapshot(), q, &Default::default())
                .unwrap()
                .rows()
                .unwrap(),
            expected,
            "{q}"
        );
        for batch_rows in [1000, 4096] {
            let cursor = CursorOptions {
                batch_rows,
                fallback: FallbackPolicy::RejectMaterialization,
                ..Default::default()
            };
            let mut c = select_cursor(s.snapshot(), q, &Default::default(), &cursor)
                .unwrap_or_else(|e| panic!("{q}: {e}"));
            let mut rows = Vec::new();
            while let Some(b) = c.next_batch().unwrap() {
                for i in 0..b.len() {
                    rows.push(b.row(i).unwrap().values().to_vec());
                }
            }
            assert_eq!(
                ordered(rows),
                ordered(expected.clone()),
                "{q}; {batch_rows}"
            );
        }
    }
}

/// An ORDER BY key or a top-k key that holds an EXISTS runs in a strict cursor, sorted or
/// spilled, with the order of evaluation per key.
#[test]
fn order_keys_with_exists_stream_from_the_key_set() {
    use sparkles_core::sparql::{FallbackPolicy, select_cursor};
    let s = store();
    let per_key = QueryOptions {
        optimizations: Some(sparkles_core::sparql::Optimizations {
            decorrelate_exists: false,
            ..sparkles_core::sparql::Optimizations::ALL
        }),
        ..Default::default()
    };
    for q in [
        "SELECT ?s { ?s <urn:p> ?v } ORDER BY DESC(EXISTS { ?s <urn:q> ?o . ?o <urn:r> ?k }) ?s LIMIT 20",
        "SELECT ?s ?v { ?s <urn:p> ?v FILTER(?v = 3) } ORDER BY ?v DESC(NOT EXISTS { ?s <urn:q> ?o }) ?s",
        "SELECT ?s { ?s <urn:p> ?v } ORDER BY DESC(IF(EXISTS { ?s <urn:q> ?o }, ?v, -1)) ?s LIMIT 30",
    ] {
        let expected = query(s.snapshot(), q, &per_key).unwrap().rows().unwrap();
        assert_retained_rows_eq!(
            query(s.snapshot(), q, &Default::default())
                .unwrap()
                .rows()
                .unwrap(),
            expected,
            "{q}"
        );
        for batch_rows in [1000, 4096] {
            let cursor = CursorOptions {
                batch_rows,
                fallback: FallbackPolicy::RejectMaterialization,
                ..Default::default()
            };
            let mut c = select_cursor(s.snapshot(), q, &Default::default(), &cursor)
                .unwrap_or_else(|e| panic!("{q}: {e}"));
            let mut rows = Vec::new();
            while let Some(b) = c.next_batch().unwrap() {
                for i in 0..b.len() {
                    rows.push(b.row(i).unwrap().values().to_vec());
                }
            }
            assert_eq!(
                ordered(rows),
                ordered(expected.iter().map(|r| r.values().to_vec())),
                "{q}; {batch_rows}"
            );
        }
    }
}
