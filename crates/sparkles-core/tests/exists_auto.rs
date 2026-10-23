//! Automatic execution of plans that evaluate EXISTS (roadmap item 7). A grouping or
//! count over a large input whose EXISTS patterns have cursor plans streams, and its
//! answers match eager execution exactly.

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

fn bag(rows: Vec<Vec<Option<Term>>>) -> Vec<String> {
    let mut rows = rows
        .into_iter()
        .map(|r| format!("{r:?}"))
        .collect::<Vec<_>>();
    rows.sort();
    rows
}

fn ordered(rows: Vec<Vec<Option<Term>>>) -> Vec<String> {
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
                    rows.push(batch.row(row)?);
                }
            }
            Ok((rows, true))
        }
        QueryExecution::Eager(r) => Ok((r.into_result().rows(), false)),
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

/// EXISTS that the cursor evaluates once per distinct outer key: in an OPTIONAL
/// condition, in BIND, inside a larger expression and in an aggregate argument. These
/// measured slower in the cursor, so automatic execution keeps them eager.
const PER_KEY: &[&str] = &[
    "SELECT ?v (COUNT(?o) AS ?n) { ?s <urn:p> ?v OPTIONAL { ?s <urn:q> ?o FILTER EXISTS { ?o <urn:r> ?k } } } GROUP BY ?v ORDER BY ?v",
    "SELECT ?b (COUNT(*) AS ?n) { ?s <urn:p> ?v BIND(EXISTS { ?s <urn:q> ?o } AS ?b) } GROUP BY ?b ORDER BY ?b",
    "SELECT (COUNT(*) AS ?n) { ?s <urn:p> ?v FILTER (?v < 10 || EXISTS { ?s <urn:q> <urn:o:5> }) }",
    "SELECT (SUM(IF(EXISTS { ?s <urn:q> ?o }, 1, 0)) AS ?n) { ?s <urn:p> ?v }",
];

#[test]
fn large_groupings_with_exists_stream_and_match_eager() {
    let s = store();
    for q in STREAMED {
        let expected = query(s.snapshot(), q, &Default::default())
            .unwrap_or_else(|e| panic!("{q}: {e}"))
            .rows();
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

#[test]
fn exists_evaluated_per_key_stays_eager_and_matches() {
    let s = store();
    for q in PER_KEY {
        let expected = query(s.snapshot(), q, &Default::default()).unwrap().rows();
        let (got, streamed) = automatic(&s, q, &Default::default(), &Default::default())
            .unwrap_or_else(|e| panic!("{q}: {e}"));
        assert!(!streamed, "{q}: streamed");
        assert_eq!(ordered(got), ordered(expected), "{q}");
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
    for q in STREAMED.iter().step_by(2) {
        let expected = query(s.snapshot(), q, &Default::default()).unwrap().rows();
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
                                    rows.push(batch.row(row).unwrap());
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
                        bag(r.into_result().rows()),
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

/// A pattern whose inner FILTER reads a variable that every outer row binds cannot use
/// its key set, so each distinct outer key is evaluated on its own and the plan stays
/// eager, like the other per-key shapes.
#[test]
fn exists_with_an_outer_bound_risky_variable_stays_eager() {
    let s = store();
    for q in [
        "SELECT (COUNT(*) AS ?n) { ?s <urn:p> ?v FILTER EXISTS { ?s <urn:q> ?o FILTER(?v >= 0) } }",
        "SELECT (COUNT(*) AS ?n) { ?s <urn:p> ?v FILTER NOT EXISTS { ?o <urn:r> ?k } }",
    ] {
        let expected = query(s.snapshot(), q, &Default::default()).unwrap().rows();
        let (got, streamed) = automatic(&s, q, &Default::default(), &Default::default())
            .unwrap_or_else(|e| panic!("{q}: {e}"));
        assert!(!streamed, "{q}: streamed");
        assert_eq!(bag(got), bag(expected), "{q}");
    }
}
