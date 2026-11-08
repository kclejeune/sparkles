//! Query-local state is charged to the query in both accounting modes: the terms that
//! expressions compute are retained until the query ends, so a budget that cannot hold
//! them fails the query instead of leaving the values unbound (item 453 stage 4).

use oxrdfio::RdfFormat;
use sparkles_core::io::Source;
use sparkles_core::sparql::{CursorOptions, QueryOptions, query, select_cursor, update::update};
use sparkles_core::store::{Store, StoreOptions};
use sparkles_core::{BudgetKind, Error};

const ROWS: usize = 50_000;
const Q: &str = "SELECT ?s ?u WHERE { ?s <urn:p> ?o BIND(STRUUID() AS ?u) }";

fn store() -> Store {
    let s = Store::in_memory(StoreOptions::default());
    let data: String = (0..ROWS)
        .map(|i| format!("<urn:s{i}> <urn:p> \"{i}\" .\n"))
        .collect();
    s.load(&[Source::from_bytes(
        data.into_bytes(),
        RdfFormat::NTriples,
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

fn is_memory(error: &Error) -> bool {
    matches!(error, Error::BudgetExceeded(b) if b.kind == BudgetKind::Memory)
}

/// Each row's new UUID string is a query-local term of about 130 bytes, which 50,000
/// rows cannot keep within 3 MiB, while the solutions themselves fit.
#[test]
fn computed_terms_beyond_the_budget_fail_the_query_in_both_modes() {
    let s = store();
    let all = query(s.snapshot(), Q, &Default::default()).unwrap();
    let rows = all.rows().unwrap();
    assert_eq!(rows.len(), ROWS);
    assert!(rows.iter().all(|r| r.values().iter().all(Option::is_some)));

    let eager = query(s.snapshot(), Q, &budget(3 << 20));
    match eager {
        Err(error) => assert!(is_memory(&error), "{error:?}"),
        Ok(r) => panic!("eager answered {} rows within the budget", r.len()),
    }

    let cursor = (|| -> sparkles_core::Result<usize> {
        let mut c = select_cursor(s.snapshot(), Q, &budget(3 << 20), &CursorOptions::default())?;
        let mut n = 0;
        while let Some(b) = c.next_batch()? {
            for i in 0..b.len() {
                assert!(b.row(i)?.values().iter().all(Option::is_some));
            }
            n += b.len();
        }
        Ok(n)
    })();
    match cursor {
        Err(error) => assert!(is_memory(&error), "{error:?}"),
        Ok(n) => panic!("the cursor answered {n} rows within the budget"),
    }
}

fn turtle(data: &str) -> Store {
    let s = Store::in_memory(StoreOptions::default());
    s.load(&[Source::from_bytes(
        data.as_bytes().to_vec(),
        RdfFormat::Turtle,
        None,
    )])
    .unwrap();
    s
}

/// A finished eager result decodes without refusals: its readers cannot report one, so
/// a value the budget cannot hold the decode envelope of must still come back bound.
#[test]
fn a_finished_result_decodes_its_values_within_a_small_budget() {
    let long = "x".repeat(4096);
    let s = turtle(&format!("<urn:s> <urn:p> \"{long}\" ."));
    let r = query(
        s.snapshot(),
        "SELECT ?o WHERE { ?s <urn:p> ?o }",
        &budget(8192),
    )
    .unwrap();
    let rows = r.rows().unwrap();
    assert_eq!(rows.len(), 1);
    match &rows.row(0).unwrap().values()[0] {
        Some(oxrdf::Term::Literal(l)) => assert_eq!(l.value(), long),
        other => panic!("{other:?}"),
    }
}

/// An update whose template term cannot be decoded within its budget fails before it
/// applies anything, instead of applying its deletions and skipping the insertion.
#[test]
fn a_refused_template_term_leaves_the_store_unchanged() {
    let s = turtle("<urn:s> <urn:p> \"old\" .");
    let long = "x".repeat(4096);
    let text = format!(
        "DELETE {{ <urn:s> <urn:p> ?old }} INSERT {{ <urn:s> <urn:p> ?new }} \
         WHERE {{ <urn:s> <urn:p> ?old VALUES ?new {{ \"{long}\" }} }}"
    );
    match update(&s, &text, &budget(8192)) {
        Err(error) => assert!(is_memory(&error), "{error:?}"),
        Ok(stats) => {
            // An update that fits must have applied both halves.
            assert_eq!((stats.deleted, stats.inserted), (1, 1));
            return;
        }
    }
    let r = query(
        s.snapshot(),
        "SELECT ?o WHERE { <urn:s> <urn:p> ?o }",
        &Default::default(),
    )
    .unwrap();
    let rows = r.rows().unwrap();
    assert_eq!(rows.len(), 1, "the deletion was not applied");
    match &rows.row(0).unwrap().values()[0] {
        Some(oxrdf::Term::Literal(l)) => assert_eq!(l.value(), "old"),
        other => panic!("{other:?}"),
    }
}

/// A regular expression's program is charged in both accounting modes, so one that
/// cannot fit a small budget fails the query instead of compiling outside it.
#[test]
fn a_regular_expression_beyond_the_budget_fails_the_query_in_both_modes() {
    let s = store();
    let q = r#"SELECT (REGEX("α", "\\w{20}") AS ?x) WHERE {}"#;
    // the budgeted queries run first, since the result cache would answer them
    match query(s.snapshot(), q, &budget(32 << 10)) {
        Err(error) => assert!(is_memory(&error), "{error:?}"),
        Ok(r) => panic!("eager answered {:?} within the budget", r.rows().unwrap()),
    }
    let mut c = select_cursor(
        s.snapshot(),
        q,
        &budget(32 << 10),
        &CursorOptions::default(),
    )
    .unwrap();
    match c.next_batch() {
        Err(error) => assert!(is_memory(&error), "{error:?}"),
        Ok(b) => panic!(
            "the cursor answered {:?} within the budget",
            b.map(|b| b.len())
        ),
    }
    let rows = query(s.snapshot(), q, &Default::default())
        .unwrap()
        .rows()
        .unwrap();
    assert_eq!(rows.len(), 1);
    assert!(rows.row(0).unwrap().values()[0].is_some());
}

fn count(row: &[Option<oxrdf::Term>]) -> u64 {
    match &row[0] {
        Some(oxrdf::Term::Literal(l)) => l.value().parse().unwrap(),
        other => panic!("not a count: {other:?}"),
    }
}

/// Regular expressions on Rayon workers, each with its own search cache, give the
/// counts that follow from the data, through the key filters on vocabulary keys and
/// through `REGEX` and `REPLACE` on computed strings, in both accounting modes.
#[test]
fn regular_expressions_on_rayon_workers_count_what_the_data_holds() {
    let s = store();
    // the literals are "0" to "49999" and the subjects <urn:s0> to <urn:s49999>
    let cases = [
        (r#"FILTER(REGEX(STR(?s), "s1[0-9]{3}$"))"#, 1000),
        (r#"FILTER(REGEX(?o, "^4[0-9]{4}$"))"#, 10_000),
        (r#"FILTER(REGEX(?o, "^ab", "i") || REGEX(?o, "^7"))"#, 1111),
        (r#"FILTER(REGEX(CONCAT("x", ?o), "^x7"))"#, 1111),
        (r#"FILTER(REPLACE(?o, "([0-9])9$", "$1x") != ?o)"#, 4999),
    ];
    rayon::ThreadPoolBuilder::new()
        .num_threads(4)
        .build()
        .unwrap()
        .install(|| {
            for (filter, expected) in cases {
                let q = format!("SELECT (COUNT(*) AS ?c) WHERE {{ ?s <urn:p> ?o {filter} }}");
                let eager = query(s.snapshot(), &q, &Default::default())
                    .unwrap()
                    .rows()
                    .unwrap();
                assert_eq!(
                    count({
                        assert_eq!(eager.len(), 1);
                        eager.row(0).unwrap().values()
                    }),
                    expected,
                    "eager {filter}"
                );
                let mut c = select_cursor(
                    s.snapshot(),
                    &q,
                    &Default::default(),
                    &CursorOptions::default(),
                )
                .unwrap();
                let mut rows = Vec::new();
                while let Some(b) = c.next_batch().unwrap() {
                    for i in 0..b.len() {
                        rows.push(b.row(i).unwrap().values().to_vec());
                    }
                }
                assert_eq!(
                    count({
                        assert_eq!(rows.len(), 1);
                        &rows[0]
                    }),
                    expected,
                    "cursor {filter}"
                );
            }
        });
}

/// A REPLACE reserves its output by the length of each match and the references in the
/// replacement, so one match of a 10,000-byte literal replacement in a 10,000-byte
/// string fits a 16 MiB budget in both modes. A bound of the whole input per byte of
/// the replacement asked for about 200 MB.
#[test]
fn a_long_literal_replacement_reserves_what_it_writes() {
    let s = store();
    let long = "b".repeat(10_000);
    let q = format!(
        r#"SELECT (STRLEN(REPLACE(?x, "^a", "{long}")) AS ?n) WHERE {{ VALUES ?x {{ "a{long}" }} }}"#
    );
    let eager = query(s.snapshot(), &q, &budget(16 << 20))
        .unwrap()
        .rows()
        .unwrap();
    assert_eq!(
        count({
            assert_eq!(eager.len(), 1);
            eager.row(0).unwrap().values()
        }),
        20_000
    );
    let mut c = select_cursor(
        s.snapshot(),
        &q,
        &budget(16 << 20),
        &CursorOptions::default(),
    )
    .unwrap();
    let mut rows = Vec::new();
    while let Some(b) = c.next_batch().unwrap() {
        for i in 0..b.len() {
            rows.push(b.row(i).unwrap().values().to_vec());
        }
    }
    assert_eq!(
        count({
            assert_eq!(rows.len(), 1);
            &rows[0]
        }),
        20_000
    );
}

fn ntriples(lines: impl Iterator<Item = String>) -> Store {
    let s = Store::in_memory(StoreOptions::default());
    let data: String = lines.collect();
    s.load(&[Source::from_bytes(
        data.into_bytes(),
        RdfFormat::NTriples,
        None,
    )])
    .unwrap();
    s
}

/// The rows of `q` under `options`, eagerly and from a cursor whose batches hold
/// `batch_rows` rows, or the first error of each.
fn both_modes(
    s: &Store,
    q: &str,
    options: &QueryOptions,
    batch_rows: usize,
) -> [sparkles_core::Result<usize>; 2] {
    let eager = query(s.snapshot(), q, options).map(|r| r.len());
    let cursor = (|| {
        let cursor_options = CursorOptions {
            batch_rows,
            ..Default::default()
        };
        let mut c = select_cursor(s.snapshot(), q, options, &cursor_options)?;
        let mut n = 0;
        while let Some(b) = c.next_batch()? {
            n += b.len();
        }
        Ok(n)
    })();
    [eager, cursor]
}

fn without(names: &str, max_memory_bytes: Option<u64>) -> QueryOptions {
    QueryOptions {
        max_memory_bytes,
        optimizations: Some(
            sparkles_core::sparql::Optimizations::ALL
                .disable(names)
                .unwrap(),
        ),
        no_cache: true,
        ..Default::default()
    }
}

/// A top k over more than 16,384 rows decodes its first key in parallel and keeps the
/// decoded strings in its column until the heap is done, whatever the value cache
/// keeps, so 18,000 distinct strings of 3 KiB do not fit in 24 MiB.
#[test]
fn a_large_heap_key_column_is_charged_for_its_strings() {
    let pad = "x".repeat(3000);
    let s = ntriples((0..18_000).map(|i| format!("<urn:s{i}> <urn:q> \"{i:05}{pad}\" .\n")));
    let q = "SELECT ?s WHERE { ?s <urn:q> ?v } ORDER BY ?v LIMIT 3";
    for result in both_modes(&s, q, &without("ordered_topk", None), 32_768) {
        assert_eq!(result.unwrap(), 3);
    }
    for result in both_modes(&s, q, &without("ordered_topk", Some(24 << 20)), 32_768) {
        match result {
            Err(error) => assert!(is_memory(&error), "{error:?}"),
            Ok(n) => panic!("answered {n} rows within the budget"),
        }
    }
}

/// A sort of more than 16,384 rows evaluates a key once per distinct value, and the
/// computed keys stay charged until the sort is done: 512 keys of 32 KiB do not fit in
/// 8 MiB.
#[test]
fn per_value_sort_keys_are_charged_for_their_strings() {
    let pad = "y".repeat(16_000);
    let s =
        ntriples((0..32_768).map(|i| format!("<urn:s{i}> <urn:r> \"{:03}{pad}\" .\n", i % 512)));
    let q = "SELECT ?s WHERE { ?s <urn:r> ?v } ORDER BY CONCAT(?v, ?v) ?s";
    for result in both_modes(&s, q, &without("ordered_topk", None), 32_768) {
        assert_eq!(result.unwrap(), 32_768);
    }
    for result in both_modes(&s, q, &without("ordered_topk", Some(8 << 20)), 32_768) {
        match result {
            Err(error) => assert!(is_memory(&error), "{error:?}"),
            Ok(n) => panic!("answered {n} rows within the budget"),
        }
    }
}
