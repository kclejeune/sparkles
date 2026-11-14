//! A checked decode of a short key from a front-coded vocabulary block charges that
//! key, not the other keys of its block (item 948). A 600,000-byte literal shares a
//! block with the short IRIs that an `ORDER BY` sorts, and the sort runs within a
//! 1 MiB budget, because the literal is never rebuilt. A sort over the literal itself
//! stays refused.

use oxrdfio::RdfFormat;
use sparkles_core::error::{BudgetKind, Error};
use sparkles_core::io::Source;
use sparkles_core::sparql::{CursorOptions, QueryOptions, query, select_cursor};
use sparkles_core::store::{Store, StoreOptions};
use sparkles_core::vocab::FC_BLOCK;

const BUDGET: u64 = 1 << 20;
const LITERAL: usize = 600_000;
/// The most a sort of the five short subjects may hold, far below the literal.
const SMALL_PEAK: u64 = 64 << 10;

fn store() -> Store {
    let store = Store::in_memory(StoreOptions {
        result_cache_min_ms: 0.0,
        ..Default::default()
    });
    let mut data = String::new();
    for i in 0..5 {
        data.push_str(&format!("<urn:s{i}> <urn:p> <urn:o{i}> .\n"));
    }
    data.push_str(&format!(
        "<urn:big> <urn:text> \"{}\" .\n",
        "x".repeat(LITERAL)
    ));
    store
        .load(&[Source::from_bytes(
            data.into_bytes(),
            RdfFormat::Turtle,
            None,
        )])
        .unwrap();
    // The case needs the literal before the subjects in one front-coded block.
    let snap = store.snapshot();
    let vocab = &snap.generation.vocab;
    assert!(!vocab.is_fsst());
    let (mut literal, mut subjects) = (None, Vec::new());
    vocab.for_each(|id, key| {
        if key.len() > LITERAL {
            literal = Some(id);
        } else if key.windows(5).any(|w| w == b"urn:s") {
            subjects.push(id);
        }
    });
    let literal = literal.expect("the literal is in the base vocabulary");
    assert_eq!(subjects.len(), 5);
    for s in subjects {
        assert!(literal < s, "the literal {literal} comes after subject {s}");
        assert_eq!(literal / FC_BLOCK as u64, s / FC_BLOCK as u64, "one block");
    }
    store
}

fn options() -> QueryOptions {
    QueryOptions {
        max_memory_bytes: Some(BUDGET),
        no_cache: true,
        ..Default::default()
    }
}

/// The rows and memory peak of `q` through `query`.
fn eager(store: &Store, q: &str) -> sparkles_core::error::Result<(usize, u64)> {
    let r = query(store.snapshot(), q, &options())?;
    let peak = r.mem_peak_bytes;
    Ok((r.rows()?.len(), peak))
}

/// The rows and memory peak of `q` through a cursor of one row per batch, with every
/// row decoded.
fn cursor(store: &Store, q: &str) -> sparkles_core::error::Result<(usize, u64)> {
    let mut c = select_cursor(
        store.snapshot(),
        q,
        &options(),
        &CursorOptions {
            batch_rows: 1,
            ..Default::default()
        },
    )?;
    let mut rows = 0;
    while let Some(batch) = c.next_batch()? {
        for i in 0..batch.len() {
            batch.row(i)?;
            rows += 1;
        }
    }
    Ok((rows, c.stats().mem_peak_bytes))
}

fn assert_memory_refusal(result: sparkles_core::error::Result<(usize, u64)>, path: &str) {
    match result {
        Err(Error::BudgetExceeded(b)) => assert_eq!(b.kind, BudgetKind::Memory, "{path}"),
        other => panic!("{path}: expected a memory refusal, got {other:?}"),
    }
}

#[test]
fn ordering_short_subjects_does_not_rebuild_a_large_neighbour() {
    let store = store();
    let q = "SELECT ?s WHERE { ?s <urn:p> ?o } ORDER BY DESC(?s)";
    for (path, run) in [
        ("query", eager as fn(&Store, &str) -> _),
        ("cursor", cursor),
    ] {
        let (rows, peak) = run(&store, q).unwrap_or_else(|e| panic!("{path}: {e}"));
        assert_eq!(rows, 5, "{path}");
        assert!(peak < SMALL_PEAK, "{path}: peak {peak} bytes");
    }
}

#[test]
fn ordering_by_the_large_literal_stays_refused() {
    let store = store();
    let q = "SELECT ?s ?o WHERE { ?s ?p ?o } ORDER BY DESC(?o)";
    assert_memory_refusal(eager(&store, q), "query");
    assert_memory_refusal(cursor(&store, q), "cursor");
}
