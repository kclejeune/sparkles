//! An audit of the streamed merge's memory ledger against the allocator, on the shared
//! counting allocator of [`common::heap`]. Each query runs as a cursor on the counted
//! thread, and the audit compares the heap that the query allocated and still holds with
//! the bytes its ledger has admitted, inside every pull and after it, and the ledger's
//! peak with the heap's. A charge that a formula sizes apart from the allocation it
//! stands for can fall short of it without any answer changing, and this comparison is
//! the check that does not read the formula. The merges run with
//! `merge_pair_conditions` on and off, in memory and with a run that spools.

mod common;

use common::heap::{self, Allowance, Audit};
use oxrdfio::RdfFormat;
use sparkles_core::io::Source;
use sparkles_core::sparql::{
    CursorOptions, ExecutionMode, Optimizations, QueryExecution, QueryOptions, query_execution,
};
use sparkles_core::spill::{Spill, SpillOptions};
use sparkles_core::store::{Store, StoreOptions};
use std::sync::Arc;

#[global_allocator]
static A: heap::Counting = heap::Counting;

fn store(subjects: usize) -> Store {
    let s = Store::in_memory(StoreOptions::default());
    let mut data = String::new();
    for i in 0..subjects {
        data.push_str(&format!(
            "<urn:s:{i}> <urn:p> \"{}\"^^<http://www.w3.org/2001/XMLSchema#integer> .\n",
            i % 97
        ));
        for j in 0..(1 + i % 4) {
            data.push_str(&format!(
                "<urn:s:{i}> <urn:q> <urn:o:{}> .\n",
                (i * 7 + j * 331) % 3000
            ));
        }
    }
    for o in 0..3000 {
        if o % 3 == 0 {
            data.push_str(&format!("<urn:o:{o}> <urn:r> \"kept\" .\n"));
        }
    }
    s.load(&[Source::from_bytes(
        data.into_bytes(),
        RdfFormat::NTriples,
        None,
    )])
    .unwrap();
    s
}

/// Merges on `?s` with one to four pairs for each left row: three conditional
/// OPTIONALs, an OPTIONAL without a condition and a join.
const QUERIES: &[&str] = &[
    "SELECT ?s ?v ?o { ?s <urn:p> ?v OPTIONAL { ?s <urn:q> ?o FILTER(?o = <urn:o:7>) } }",
    "SELECT ?s ?v ?o { ?s <urn:p> ?v OPTIONAL { ?s <urn:q> ?o FILTER EXISTS { ?o <urn:r> ?k } } }",
    "SELECT ?s ?v ?o { ?s <urn:p> ?v OPTIONAL { ?s <urn:q> ?o \
     FILTER(?v = 5 || NOT EXISTS { ?o <urn:r> ?k }) } }",
    "SELECT ?s ?v ?o { ?s <urn:p> ?v OPTIONAL { ?s <urn:q> ?o } }",
    "SELECT ?s ?v ?o { ?s <urn:p> ?v . ?s <urn:q> ?o }",
];

/// Run `q` as a streamed cursor on the counted thread of the shared audit.
fn measure(s: &Store, q: &str, opts: &QueryOptions) -> Audit {
    heap::audit(|| {
        let QueryExecution::Select(c) = query_execution(
            s.snapshot(),
            q,
            opts,
            &CursorOptions::default(),
            ExecutionMode::Streaming,
        )
        .unwrap() else {
            panic!("{q} did not stream");
        };
        *c
    })
}

/// The lengths of the batches of a streamed run of `q`, outside the audit.
fn batches(s: &Store, q: &str, opts: &QueryOptions) -> Vec<usize> {
    let QueryExecution::Select(mut c) = query_execution(
        s.snapshot(),
        q,
        opts,
        &CursorOptions::default(),
        ExecutionMode::Streaming,
    )
    .unwrap() else {
        panic!("{q} did not stream");
    };
    let mut lengths = Vec::new();
    while let Some(batch) = c.next_batch().unwrap() {
        lengths.push(batch.len());
    }
    lengths
}

/// Whether the cursor ran the plan's join or OPTIONAL as a merge on `?s`.
fn merges(plan: &str) -> bool {
    plan.contains("[merge on ?s]")
        || plan.contains(r#""operator":"MergeJoin","description":"on ?s""#)
}

fn options(budget: u64, pairs: bool, spill: Option<Arc<Spill>>) -> QueryOptions {
    QueryOptions {
        max_memory_bytes: Some(budget),
        no_cache: true,
        spill,
        optimizations: Some(Optimizations {
            merge_pair_conditions: pairs,
            ..Default::default()
        }),
        ..Default::default()
    }
}

fn print(what: &str, a: &Audit) {
    println!(
        "AUDIT {what} rows={} batches={} within={} after={} over={} peak={} ledger={}",
        a.rows, a.batches, a.within, a.after, a.over, a.peak, a.ledger
    );
}

/// The heap a streamed merge holds stays within what its ledger admits, inside and
/// after every pull, whichever way the merge tests an OPTIONAL's condition and however
/// many rows pass through it. Two runs warm what the store keeps between queries, such
/// as decoded blocks, and each of the three runs after them must pass. A conditional
/// OPTIONAL that tests its pairs before it copies them also fills its batches, so at
/// most one batch before the last is short.
#[test]
fn a_streamed_merge_holds_no_more_heap_than_its_ledger_reports() {
    let _serial = heap::serial();
    let mut found = Vec::new();
    for subjects in [20_000, 80_000] {
        let s = store(subjects);
        for q in QUERIES {
            for pairs in [true, false] {
                let opts = options(64 << 20, pairs, None);
                for run in 0..5 {
                    let a = measure(&s, q, &opts);
                    assert!(merges(&a.plan), "{q}: {}", a.plan);
                    assert!(a.rows >= subjects);
                    if run < 2 {
                        continue;
                    }
                    let what = format!("subjects={subjects} pairs={pairs} run={run} q={q}");
                    print(&what, &a);
                    found.extend(a.check(&what, &ALLOWED));
                }
                let lengths = batches(&s, q, &opts);
                let short = lengths[..lengths.len() - 1]
                    .iter()
                    .filter(|&&n| n < SHORT)
                    .count();
                if pairs && q.contains("FILTER") && short > 1 {
                    found.push(format!(
                        "{short} of {} batches are short: {subjects} subjects, {q}",
                        lengths.len()
                    ));
                }
            }
        }
    }
    assert!(found.is_empty(), "{}", found.join("\n"));
}

/// A merge whose right run of equal keys outgrows a small budget writes the run to a
/// spill file and joins from it, and the heap it holds then, with the frames it reads
/// back, stays within what the ledger admits.
#[test]
fn a_spooled_merge_run_holds_no_more_heap_than_its_ledger_reports() {
    let _serial = heap::serial();
    let dir = tempfile::tempdir().unwrap();
    let spill = Spill::open(&SpillOptions {
        dir: dir.path().to_path_buf(),
        max_bytes: 1 << 30,
        query_bytes: 1 << 30,
        min_free_bytes: 0,
        encrypted: false,
    })
    .unwrap();
    // 2,000 subjects with a value and an object, and one with 20 values and 40,000
    // objects, whose run of objects no batch holds.
    let s = Store::in_memory(StoreOptions::default());
    let mut data = String::new();
    let integer = "http://www.w3.org/2001/XMLSchema#integer";
    for i in 0..2000 {
        data.push_str(&format!(
            "<urn:s:{i}> <urn:p> \"{}\"^^<{integer}> .\n",
            i % 97
        ));
        data.push_str(&format!("<urn:s:{i}> <urn:q> <urn:o:{}> .\n", i % 500));
    }
    for v in 0..20 {
        data.push_str(&format!("<urn:hub> <urn:p> \"{v}\"^^<{integer}> .\n"));
    }
    for o in 0..40_000 {
        data.push_str(&format!("<urn:hub> <urn:q> <urn:o:{o}> .\n"));
    }
    s.load(&[Source::from_bytes(
        data.into_bytes(),
        RdfFormat::NTriples,
        None,
    )])
    .unwrap();
    let mut found = Vec::new();
    for q in [QUERIES[0], QUERIES[3], QUERIES[4]] {
        for pairs in [true, false] {
            let opts = options(SPOOL_BUDGET, pairs, Some(spill.clone()));
            for run in 0..5 {
                let a = measure(&s, q, &opts);
                assert!(merges(&a.plan), "{q}: {}", a.plan);
                assert!(a.plan.contains("spooledRuns"), "{q}: {}", a.plan);
                assert!(a.rows >= 2000 + 20);
                if run < 2 {
                    continue;
                }
                let what = format!("spooled pairs={pairs} run={run} q={q}");
                print(&what, &a);
                found.extend(a.check(&what, &SPOOLED_ALLOWED));
            }
        }
    }
    assert_eq!(spill.reserved_bytes(), 0);
    assert!(found.is_empty(), "{}", found.join("\n"));
}

/// A batch under this many rows is short, at the default batch size of 4096 rows.
const SHORT: usize = 512;

/// How far the in-memory merges may stand from their ledger. Every in-memory fixture
/// measured within the shared audit's exact allowance.
const ALLOWED: Allowance = Allowance::EXACT;

/// How far a spooled merge run may stand from its ledger (items 1144 and 1192).
/// While `Input::hold` writes the run, `SpillWriter` holds its pending frame outside
/// the ledger (measured up to 49.7 KiB inside a pull). While `SpooledPair` reads it
/// back, its retained formula charges an uncompressed encoded frame and decoder scratch
/// across pulls (measured up to 124.6 KiB above the heap after a pull). Both are
/// identical with `merge_pair_conditions` on and off, since a spooled run never decides
/// pairs.
const SPOOLED_ALLOWED: Allowance = Allowance {
    under: 56 << 10,
    over: 136 << 10,
    ..Allowance::EXACT
};

/// The budget under which the run of 40,000 objects spools.
const SPOOL_BUDGET: u64 = 2 << 20;
