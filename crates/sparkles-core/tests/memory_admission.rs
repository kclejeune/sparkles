//! Automatic execution streams a plan without spill when eager execution is expected to
//! outgrow the query's memory and the cursor's own state is expected to fit (roadmap item
//! 96). The estimate of that state must count everything the cursor keeps, or a plan that
//! eager execution answers fails as a cursor, or a cursor fails after it has yielded rows
//! where eager execution refuses the query before answering.

use oxrdf::Term;
use oxrdfio::RdfFormat;
use sparkles_core::io::Source;
use sparkles_core::sparql::pool::QueryPool;
use sparkles_core::sparql::{
    CursorOptions, ExecutionMode, QueryExecution, QueryOptions, query, query_execution,
};
use sparkles_core::store::{Store, StoreOptions};

fn store(data: String) -> Store {
    let s = Store::in_memory(StoreOptions::default());
    s.load(&[Source::from_bytes(
        data.into_bytes(),
        RdfFormat::Turtle,
        None,
    )])
    .unwrap();
    s
}

fn budget(bytes: u64) -> QueryOptions {
    QueryOptions {
        max_memory_bytes: Some(bytes),
        ..Default::default()
    }
}

/// A query budget of 64 MiB in a server pool of `bytes`, as the reviewer measured.
fn pooled(bytes: u64) -> QueryOptions {
    QueryOptions {
        max_memory_bytes: Some(64 << 20),
        pool: Some(QueryPool::new(bytes)),
        ..Default::default()
    }
}

/// How automatic execution of `q` went.
#[derive(Debug)]
enum Auto {
    /// It chose eager execution, which answered these rows.
    Eager(Vec<Vec<Option<Term>>>),
    /// It chose a cursor, which answered these rows.
    Streamed(Vec<Vec<Option<Term>>>),
    /// It refused the query before choosing a cursor, as eager execution does.
    Refused,
    /// It chose a cursor, which failed after yielding this many rows.
    CursorFailed(usize),
}

fn automatic(s: &Store, q: &str, opts: &QueryOptions) -> Auto {
    match query_execution(
        s.snapshot(),
        q,
        opts,
        &CursorOptions::default(),
        ExecutionMode::Auto,
    ) {
        Ok(QueryExecution::Select(mut c)) => {
            let mut rows = Vec::new();
            loop {
                match c.next_batch() {
                    Ok(Some(batch)) => {
                        for row in 0..batch.len() {
                            rows.push(batch.row(row).unwrap());
                        }
                    }
                    Ok(None) => return Auto::Streamed(rows),
                    Err(e) => {
                        eprintln!("the cursor failed after {} rows: {e}", rows.len());
                        return Auto::CursorFailed(rows.len());
                    }
                }
            }
        }
        Ok(QueryExecution::Eager(r)) => Auto::Eager(r.into_result().rows()),
        Ok(_) => panic!("{q}: not a SELECT"),
        Err(e) => {
            eprintln!("refused before a cursor: {e}");
            Auto::Refused
        }
    }
}

/// A query that does not fit must be refused, or answered, but never admitted as a
/// cursor that then fails.
fn assert_not_admitted_to_fail(outcome: Auto, rows: usize) {
    match outcome {
        Auto::CursorFailed(n) => panic!("a cursor was admitted and failed after {n} rows"),
        Auto::Eager(r) | Auto::Streamed(r) => assert_eq!(r.len(), rows),
        Auto::Refused => {}
    }
}

/// A hash join keeps its build rows and a hash index over them, whose buckets and row
/// arrays outweigh the rows. A plan that eager execution answers within the budget must
/// not be streamed on an estimate that counts only the rows.
#[test]
fn a_hash_join_whose_index_does_not_fit_is_not_streamed_without_spill() {
    let mut data = String::new();
    // each UNION below has 65,537 rows, four to a key
    for i in 0..65_537 {
        let k = i / 4;
        let p = if i % 2 == 0 { "p" } else { "q" };
        data.push_str(&format!("<urn:a:{i}> <urn:{p}> <urn:k:{k}> .\n"));
    }
    let s = store(data);
    let q = "SELECT * {
        { { ?x <urn:p> ?k } UNION { ?x <urn:q> ?k } }
        { { ?y <urn:p> ?k } UNION { ?y <urn:q> ?k } }
        VALUES ?v { 1 2 3 4 5 6 7 8 9 10 } } LIMIT 1";
    let opts = pooled(6 << 20);
    // the eager result holds its pool memory until it drops
    let eager = query(s.snapshot(), q, &opts)
        .unwrap_or_else(|e| panic!("eager: {e}"))
        .rows()
        .len();
    assert_eq!(eager, 1);
    match automatic(&s, q, &opts) {
        Auto::Eager(rows) | Auto::Streamed(rows) => assert_eq!(rows.len(), 1),
        other => panic!("automatic execution failed a query that eager answers: {other:?}"),
    }
}

/// A COUNT(DISTINCT *) keeps every distinct input row although it answers one row, so a
/// million-row input does not fit a small pool whether it streams or not. Under a UNION,
/// where the rule for groupings does not stream it, only the memory estimate could admit
/// it as a cursor, which would fail at its first pull.
#[test]
fn a_distinct_aggregate_over_a_large_input_is_not_streamed_without_spill() {
    let mut data = String::new();
    for i in 0..1_000 {
        data.push_str(&format!("<urn:s:{i}> <urn:p> {i} .\n"));
    }
    let s = store(data);
    let q = "SELECT ?n {
        { SELECT (COUNT(DISTINCT *) AS ?n) { ?a <urn:p> ?b . ?c <urn:p> ?d } }
        UNION { VALUES ?n { 0 } } }";
    assert_not_admitted_to_fail(automatic(&s, q, &pooled(16 << 20)), 2);
}

/// GROUP_CONCAT keeps text whose length the row estimates do not tell, so its plans are
/// not admitted on the estimate.
#[test]
fn a_group_concat_over_a_large_input_is_not_streamed_without_spill() {
    let mut data = String::new();
    for i in 0..1_000 {
        data.push_str(&format!(
            "<urn:s:{i}> <urn:p> \"a value long enough to outgrow the pool, number {i}\" .\n"
        ));
    }
    let s = store(data);
    let q = "SELECT ?all {
        { SELECT (GROUP_CONCAT(?b) AS ?all) { ?a <urn:p> ?b . ?c <urn:p> ?d } }
        UNION { VALUES ?all { 0 } } }";
    assert_not_admitted_to_fail(automatic(&s, q, &pooled(16 << 20)), 2);
}

/// A BIND that generates a new term for every row grows the query-local vocabulary by a
/// term per row, which the query keeps until it ends. A million such rows do not fit a
/// small budget, so the query must fail before it answers instead of after 100,000 rows.
#[test]
fn a_bind_that_generates_a_term_per_row_is_not_streamed_without_spill() {
    let mut data = String::new();
    for i in 0..1_000 {
        data.push_str(&format!("<urn:s:{i}> <urn:p> <urn:o:{i}> .\n"));
    }
    let s = store(data);
    let q = "SELECT (STRUUID() AS ?u) { ?a <urn:p> ?b . ?c <urn:p> ?d }";
    assert_not_admitted_to_fail(automatic(&s, q, &pooled(16 << 20)), 1_000_000);
}

/// A BIND of a variable generates no term, so the product it extends still streams
/// within a budget that its eager form outgrows.
#[test]
fn a_bind_of_a_variable_over_a_large_product_still_streams() {
    let mut data = String::new();
    for i in 0..1_000 {
        data.push_str(&format!("<urn:s:{i}> <urn:p> <urn:o:{i}> .\n"));
    }
    let s = store(data);
    let q = "SELECT (COUNT(*) AS ?n) {
        { ?a <urn:p> ?b . ?c <urn:p> ?d BIND(?b AS ?e) } UNION { ?a <urn:p> ?b } }";
    match automatic(&s, q, &budget(16 << 20)) {
        Auto::Streamed(rows) | Auto::Eager(rows) => {
            assert_eq!(rows.len(), 1);
            assert_eq!(
                rows[0][0],
                Some(Term::Literal(oxrdf::Literal::from(1_001_000i64)))
            );
        }
        other => panic!("{other:?}"),
    }
}

/// A Cartesian product has no key, so it builds no hash index, and counting one would
/// keep a count over the product eager, where it does not fit.
#[test]
fn a_count_over_a_keyless_product_still_streams() {
    let mut data = String::new();
    for i in 0..1_000 {
        data.push_str(&format!("<urn:a:{i}> <urn:p> {i} .\n"));
    }
    for i in 0..80_000 {
        data.push_str(&format!("<urn:c:{i}> <urn:q> {i} .\n"));
    }
    let s = store(data);
    let q = "SELECT ?n {
        { SELECT (COUNT(*) AS ?n) { ?a <urn:p> ?b . ?c <urn:q> ?d } }
        UNION { VALUES ?n { 0 } } }";
    match automatic(&s, q, &budget(8 << 20)) {
        Auto::Streamed(rows) => {
            assert_eq!(rows.len(), 2);
            assert!(
                rows.iter()
                    .any(|r| r[0] == Some(Term::Literal(oxrdf::Literal::from(80_000_000i64))))
            );
        }
        other => panic!("not streamed: {other:?}"),
    }
}
