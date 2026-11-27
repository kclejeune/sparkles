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
            // A strict cursor runs the merge or the hash OPTIONAL, as the switch
            // selects, at several batch sizes. The merge's unit tests cover a left row
            // whose pairs span several batches.
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

/// 75,000 subjects connected through a 16-level chain (`<urn:e0>` .. `<urn:e16>`) with
/// selective breaks, named graphs, and optional/union side edges so deep EXISTS queries
/// trigger `AUTO_GROUP_ROWS` (even after 0.3 default filter selectivity) and exercise
/// both empty and non-empty branches at every depth.
fn deep_chain_store() -> Store {
    let s = Store::in_memory(StoreOptions::default());
    let mut data = String::new();
    for i in 0..75_000 {
        data.push_str(&format!("<urn:s:{i}> <urn:e0> <urn:n0:{}> .\n", i % 64));
    }
    for level in 0..16 {
        for j in 0..64 {
            // Drop a distinct subset at depths 3, 5, 8, 12, and 15 so every level changes truth values.
            let keep = match level {
                3 => j % 5 != 0,
                5 => j % 7 != 0,
                8 => j % 3 != 0,
                12 => j % 4 != 0,
                15 => j % 2 == 0,
                _ => true,
            };
            if keep {
                data.push_str(&format!(
                    "<urn:n{level}:{j}> <urn:e{}> <urn:n{}:{j}> .\n",
                    level + 1,
                    level + 1
                ));
            }
            if level == 3 && j % 5 == 0 && j % 2 == 0 {
                data.push_str(&format!("<urn:n3:{j}> <urn:alt4> <urn:n4:{j}> .\n"));
            }
            if level == 3 && j % 2 == 0 {
                data.push_str(&format!(
                    "<urn:g:{}> {{ <urn:n3:{j}> <urn:e4> <urn:n4:{j}> . }}\n",
                    j % 2
                ));
            }
        }
    }
    s.load(&[Source::from_bytes(data.into_bytes(), RdfFormat::TriG, None)])
        .unwrap();
    s
}

fn deep_exists_chain(depth: usize, alternating: bool, inner_tail: &str) -> String {
    let mut q = String::new();
    for level in 0..depth {
        let kw = if alternating && level % 2 == 1 {
            "FILTER NOT EXISTS"
        } else {
            "FILTER EXISTS"
        };
        q.push_str(&format!(
            " {kw} {{ ?v{level} <urn:e{}> ?v{}",
            level + 1,
            level + 1
        ));
    }
    if !inner_tail.is_empty() {
        q.push(' ');
        q.push_str(inner_tail);
    }
    for _ in 0..depth {
        q.push_str(" }");
    }
    q
}

#[test]
fn deep_exists_streams_and_matches_eager_and_per_key() {
    use sparkles_core::sparql::{FallbackPolicy, select_cursor};
    let s = deep_chain_store();
    let per_key = QueryOptions {
        optimizations: Some(sparkles_core::sparql::Optimizations {
            decorrelate_exists: false,
            ..sparkles_core::sparql::Optimizations::ALL
        }),
        ..Default::default()
    };

    let mut queries: Vec<(&str, String)> = Vec::new();
    for depth in [5usize, 8, 16] {
        queries.push((
            "pure FILTER EXISTS",
            format!(
                "SELECT (COUNT(*) AS ?n) {{ ?s <urn:e0> ?v0{} }}",
                deep_exists_chain(depth, false, "")
            ),
        ));
        queries.push((
            "alternating NOT EXISTS",
            format!(
                "SELECT (COUNT(*) AS ?n) {{ ?s <urn:e0> ?v0{} }}",
                deep_exists_chain(depth, true, "")
            ),
        ));
    }
    // Correlated outer variable `?flag` bound at depth 0 and referenced only in a FILTER at
    // depth 8 (forces per-row `eval_exists` across all 8 levels while keeping 64 distinct
    // outer keys so the debug test stays fast).
    queries.push((
        "depth-8 correlated outer variable in inner FILTER",
        format!(
            "SELECT (COUNT(*) AS ?n) {{ ?s <urn:e0> ?v0 BIND(IF(?v0 = <urn:n0:0>, 0, 1) AS ?flag){} }}",
            deep_exists_chain(8, false, "FILTER(?flag = 1)")
        ),
    ));
    // Mixed decorrelated + non-decorrelated (UNION and OPTIONAL at depth 4 inside depth 8).
    let tail4 = deep_exists_chain(4, false, "");
    let tail4_shifted = tail4
        .replace("?v4", "?v8")
        .replace("?v3", "?v7")
        .replace("?v2", "?v6")
        .replace("?v1", "?v5")
        .replace("?v0", "?v4")
        .replace("<urn:e4>", "<urn:e8>")
        .replace("<urn:e3>", "<urn:e7>")
        .replace("<urn:e2>", "<urn:e6>")
        .replace("<urn:e1>", "<urn:e5>");
    queries.push((
        "depth-8 with UNION at depth 4",
        format!(
            "SELECT (COUNT(*) AS ?n) {{ ?s <urn:e0> ?v0 \
             FILTER EXISTS {{ ?v0 <urn:e1> ?v1 \
             FILTER EXISTS {{ ?v1 <urn:e2> ?v2 \
             FILTER EXISTS {{ ?v2 <urn:e3> ?v3 \
             FILTER EXISTS {{ {{ ?v3 <urn:e4> ?v4 }} UNION {{ ?v3 <urn:alt4> ?v4 }}{tail4_shifted} }} }} }} }} }}"
        ),
    ));
    queries.push((
        "depth-8 with OPTIONAL at depth 4",
        format!(
            "SELECT (COUNT(*) AS ?n) {{ ?s <urn:e0> ?v0 \
             FILTER EXISTS {{ ?v0 <urn:e1> ?v1 \
             FILTER EXISTS {{ ?v1 <urn:e2> ?v2 \
             FILTER EXISTS {{ ?v2 <urn:e3> ?v3 \
             FILTER EXISTS {{ ?v3 <urn:e4> ?v4 OPTIONAL {{ ?v3 <urn:alt4> ?opt }}{tail4_shifted} }} }} }} }} }}"
        ),
    ));
    // GRAPH (named and variable) inside depth-8 EXISTS.
    queries.push((
        "depth-8 with constant GRAPH at depth 4",
        format!(
            "SELECT (COUNT(*) AS ?n) {{ ?s <urn:e0> ?v0 \
             FILTER EXISTS {{ ?v0 <urn:e1> ?v1 \
             FILTER EXISTS {{ ?v1 <urn:e2> ?v2 \
             FILTER EXISTS {{ ?v2 <urn:e3> ?v3 \
             FILTER EXISTS {{ GRAPH <urn:g:0> {{ ?v3 <urn:e4> ?v4 }}{tail4_shifted} }} }} }} }} }}"
        ),
    ));
    queries.push((
        "depth-8 with variable GRAPH at depth 4",
        format!(
            "SELECT (COUNT(*) AS ?n) {{ ?s <urn:e0> ?v0 \
             FILTER EXISTS {{ ?v0 <urn:e1> ?v1 \
             FILTER EXISTS {{ ?v1 <urn:e2> ?v2 \
             FILTER EXISTS {{ ?v2 <urn:e3> ?v3 \
             FILTER EXISTS {{ GRAPH ?g {{ ?v3 <urn:e4> ?v4 }}{tail4_shifted} }} }} }} }} }}"
        ),
    ));
    // Empty and non-empty branches at depth 8.
    queries.push((
        "depth-8 empty innermost branch (EXISTS)",
        format!(
            "SELECT (COUNT(*) AS ?n) {{ ?s <urn:e0> ?v0{} }}",
            deep_exists_chain(7, false, "FILTER EXISTS { ?v7 <urn:absent> ?z }")
        ),
    ));
    queries.push((
        "depth-8 empty innermost branch (NOT EXISTS)",
        format!(
            "SELECT (COUNT(*) AS ?n) {{ ?s <urn:e0> ?v0{} }}",
            deep_exists_chain(7, false, "FILTER NOT EXISTS { ?v7 <urn:absent> ?z }")
        ),
    ));
    // Deep EXISTS inside BIND, SUM, and ORDER BY.
    let inner7 = deep_exists_chain(7, false, "");
    let exists8 = format!("EXISTS {{ ?v0 <urn:e1> ?v1{inner7} }}");
    queries.push((
        "depth-8 inside BIND(IF(...))",
        format!(
            "SELECT ?b (COUNT(*) AS ?n) {{ ?s <urn:e0> ?v0 BIND(IF({exists8}, 1, 0) AS ?b) }} GROUP BY ?b ORDER BY ?b"
        ),
    ));
    queries.push((
        "depth-8 inside SUM(IF(...))",
        format!("SELECT (SUM(IF({exists8}, 1, 0)) AS ?n) {{ ?s <urn:e0> ?v0 }}"),
    ));
    queries.push((
        "depth-8 inside ORDER BY DESC(...)",
        format!(
            "SELECT ?v0 (COUNT(?q) AS ?n) {{ ?s <urn:e0> ?v0 OPTIONAL {{ ?v0 <urn:e1> ?q }} }} GROUP BY ?v0 ORDER BY DESC({exists8}) ?v0"
        ),
    ));

    for (label, q) in &queries {
        let eager = query(s.snapshot(), q, &Default::default())
            .unwrap_or_else(|e| panic!("{label}: eager failed: {e}"))
            .rows()
            .unwrap();
        let reference = query(s.snapshot(), q, &per_key)
            .unwrap_or_else(|e| panic!("{label}: per-key failed: {e}"))
            .rows()
            .unwrap();
        let is_ordered = q.contains("ORDER BY");
        let norm = |rows: Vec<Vec<Option<Term>>>| {
            if is_ordered { ordered(rows) } else { bag(rows) }
        };
        let eager_rows: Vec<Vec<Option<Term>>> =
            eager.iter().map(|r| r.values().to_vec()).collect();
        let ref_rows: Vec<Vec<Option<Term>>> =
            reference.iter().map(|r| r.values().to_vec()).collect();
        assert_eq!(
            norm(eager_rows.clone()),
            norm(ref_rows),
            "{label}: eager vs per-key mismatch"
        );

        let (auto_rows, streamed) =
            automatic(&s, q, &Default::default(), &CursorOptions::default())
                .unwrap_or_else(|e| panic!("{label}: auto failed: {e}"));
        assert!(streamed, "{label}: expected automatic execution to stream");
        assert_eq!(
            norm(auto_rows),
            norm(eager_rows.clone()),
            "{label}: auto vs eager mismatch"
        );

        for batch_rows in [1, 64, 4096] {
            let cursor_opts = CursorOptions {
                batch_rows,
                fallback: FallbackPolicy::RejectMaterialization,
                ..Default::default()
            };
            let mut c = select_cursor(s.snapshot(), q, &Default::default(), &cursor_opts)
                .unwrap_or_else(|e| panic!("{label}: strict cursor ({batch_rows}) failed: {e}"));
            assert!(
                !c.plan().has_materialization(),
                "{label}: strict cursor ({batch_rows}) unexpectedly materialized"
            );
            let mut got = Vec::new();
            while let Some(b) = c.next_batch().unwrap() {
                for i in 0..b.len() {
                    got.push(b.row(i).unwrap().values().to_vec());
                }
            }
            assert_eq!(
                norm(got),
                norm(eager_rows.clone()),
                "{label}: strict cursor ({batch_rows}) mismatch"
            );
        }
    }
}

#[test]
fn deep_exists_enforces_tight_memory_budget_and_cancellation() {
    let s = deep_chain_store();
    let depth8_decor = format!(
        "SELECT (COUNT(*) AS ?n) {{ ?s <urn:e0> ?v0{} }}",
        deep_exists_chain(8, false, "")
    );
    let depth16_decor = format!(
        "SELECT (COUNT(*) AS ?n) {{ ?s <urn:e0> ?v0{} }}",
        deep_exists_chain(16, false, "")
    );
    let depth8_corr = format!(
        "SELECT (COUNT(*) AS ?n) {{ ?s <urn:e0> ?v0 BIND(IF(?v0 = <urn:n0:0>, 0, 1) AS ?flag){} }}",
        deep_exists_chain(8, false, "FILTER(?flag = 1)")
    );

    for (label, q) in [
        ("depth-8 decorrelated", &depth8_decor),
        ("depth-16 decorrelated", &depth16_decor),
        ("depth-8 correlated", &depth8_corr),
    ] {
        let expected = query(s.snapshot(), q, &Default::default())
            .unwrap()
            .rows()
            .unwrap();
        for memory in [256 << 10, 512 << 10, 2 << 20, 16 << 20] {
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
                        Ok(()) => assert_eq!(bag(rows), bag(expected.clone()), "{label}; {memory}"),
                        Err(Error::BudgetExceeded(sparkles_core::error::Budget {
                            kind: BudgetKind::Memory,
                            ..
                        })) => {}
                        Err(e) => panic!("{label}; {memory}: unexpected error: {e}"),
                    }
                    assert!(
                        c.stats().mem_peak_bytes <= memory,
                        "{label}; {memory}: peak {} > {memory}",
                        c.stats().mem_peak_bytes
                    );
                }
                Ok(QueryExecution::Eager(r)) => {
                    let r = r.into_result();
                    assert!(
                        r.mem_peak_bytes <= memory,
                        "{label}; {memory}: eager peak {} > {memory}",
                        r.mem_peak_bytes
                    );
                    assert_eq!(
                        bag(r.rows().unwrap()),
                        bag(expected.clone()),
                        "{label}; {memory}"
                    );
                }
                Err(Error::BudgetExceeded(sparkles_core::error::Budget {
                    kind: BudgetKind::Memory,
                    ..
                })) => {}
                Err(e) => panic!("{label}; {memory}: unexpected open error: {e}"),
                Ok(_) => panic!("{label}: not a SELECT"),
            }
        }
    }

    // Pre-cancelled admission at depth 16 returns Cancelled (never falls back to eager).
    let pre_cancel = Arc::new(AtomicBool::new(true));
    let err = query_execution(
        s.snapshot(),
        &depth16_decor,
        &QueryOptions {
            cancel: Some(pre_cancel),
            ..Default::default()
        },
        &Default::default(),
        ExecutionMode::Auto,
    )
    .err()
    .expect("pre-cancelled depth-16 query must fail");
    assert!(matches!(err, Error::Cancelled), "got {err}");

    // Expired deadline at depth 16 returns Timeout (never falls back to eager).
    let err = query_execution(
        s.snapshot(),
        &depth16_decor,
        &QueryOptions {
            deadline: Some(std::time::Instant::now() - std::time::Duration::from_secs(1)),
            ..Default::default()
        },
        &Default::default(),
        ExecutionMode::Auto,
    )
    .err()
    .expect("expired-deadline depth-16 query must fail");
    assert!(matches!(err, Error::Timeout), "got {err}");

    // Cancellation after opening streamed depth-16 cursor stops execution cleanly.
    let mid_cancel = Arc::new(AtomicBool::new(false));
    let exec = query_execution(
        s.snapshot(),
        &depth16_decor,
        &QueryOptions {
            cancel: Some(mid_cancel.clone()),
            ..Default::default()
        },
        &Default::default(),
        ExecutionMode::Auto,
    )
    .unwrap();
    let QueryExecution::Select(mut c) = exec else {
        panic!("depth-16 query should stream under Auto");
    };
    mid_cancel.store(true, Ordering::SeqCst);
    assert!(matches!(c.next_batch(), Err(Error::Cancelled)));
}
