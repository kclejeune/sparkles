//! The heap audit of every cursor operator (items 1167 and 1183). Each fixture runs one
//! streamed query on the counted thread of [`common::heap`], compares the heap the query
//! allocated and holds with the bytes its ledger has admitted, inside every pull and
//! after it, and records the excess on each side. An excess beyond a fixture's allowance
//! is a finding: heap above the ledger is an undercharge and ledger above the heap an
//! overcharge.
//!
//! The allowances are the measured excess of each fixture rounded up, so a change that
//! drops a charge or charges past an allocation's free fails the fixture. A fixture
//! whose finding is known and owned by a roadmap item names the item beside its
//! allowance. `AUDIT_PRINT=1` prints every reading.

mod common;

use common::heap::{self, Allowance, Audit};
use oxrdfio::RdfFormat;
use sparkles_core::io::Source;
use sparkles_core::sparql::pool::QueryPool;
use sparkles_core::sparql::update::update;
use sparkles_core::sparql::{CursorOptions, QueryOptions, select_cursor};
use sparkles_core::spill::{Spill, SpillOptions};
use sparkles_core::store::{Store, StoreOptions};
use std::alloc::Layout;
use std::sync::Arc;
use std::sync::atomic::Ordering;

#[global_allocator]
static A: heap::Counting = heap::Counting;

const XSD_INT: &str = "http://www.w3.org/2001/XMLSchema#integer";

/// `n` subjects, each with an integer `p`, one to four `q` objects of which a third have
/// `r`, and a `name` string. A chain of `c` nodes on `n`, and a quad in a named graph
/// for every tenth subject.
fn store(n: usize) -> Store {
    let s = Store::in_memory(StoreOptions::default());
    let mut data = String::new();
    for i in 0..n {
        data.push_str(&format!(
            "<urn:s:{i}> <urn:p> \"{}\"^^<{XSD_INT}> .\n",
            i % 97
        ));
        data.push_str(&format!(
            "<urn:s:{i}> <urn:name> \"name {:08}\" .\n",
            (i * 7919) % n
        ));
        for j in 0..(1 + i % 4) {
            data.push_str(&format!(
                "<urn:s:{i}> <urn:q> <urn:o:{}> .\n",
                (i * 7 + j * 331) % 3000
            ));
        }
        if i % 10 == 0 {
            data.push_str(&format!(
                "<urn:s:{i}> <urn:p> \"{i}\"^^<{XSD_INT}> <urn:g:{}> .\n",
                i % 7
            ));
        }
    }
    for o in 0..3000 {
        if o % 3 == 0 {
            data.push_str(&format!("<urn:o:{o}> <urn:r> \"kept\" .\n"));
        }
    }
    for i in 1..2000 {
        data.push_str(&format!("<urn:c:{}> <urn:n> <urn:c:{i}> .\n", i - 1));
    }
    s.load(&[Source::from_bytes(
        data.into_bytes(),
        RdfFormat::NQuads,
        None,
    )])
    .unwrap();
    s
}

/// The queries of the sweep, by the operator each one exercises.
const FIXTURES: &[(&str, &str)] = &[
    ("leaf scan", "SELECT ?s ?o { ?s <urn:q> ?o }"),
    ("filter", "SELECT ?s ?v { ?s <urn:p> ?v FILTER(?v > 50) }"),
    ("bind", "SELECT ?s ?w { ?s <urn:p> ?v BIND(?v * 2 AS ?w) }"),
    (
        "merge join",
        "SELECT ?s ?v ?o { ?s <urn:p> ?v . ?s <urn:q> ?o }",
    ),
    (
        "merge optional",
        "SELECT ?s ?v ?o { ?s <urn:p> ?v OPTIONAL { ?s <urn:q> ?o } }",
    ),
    (
        "conditional optional",
        "SELECT ?s ?v ?o { ?s <urn:p> ?v OPTIONAL { ?s <urn:q> ?o FILTER(?o = <urn:o:7>) } }",
    ),
    (
        "hash join",
        "SELECT ?s ?o ?k { ?s <urn:q> ?o . ?o <urn:r> ?k }",
    ),
    (
        "union",
        "SELECT ?s ?x { { ?s <urn:p> ?x } UNION { ?s <urn:q> ?x } }",
    ),
    (
        "minus",
        "SELECT ?s ?v { ?s <urn:p> ?v MINUS { ?s <urn:q> <urn:o:7> } }",
    ),
    (
        "exists",
        "SELECT ?s ?o { ?s <urn:q> ?o FILTER EXISTS { ?o <urn:r> ?k } }",
    ),
    (
        "distinct one column",
        "SELECT DISTINCT ?o { ?s <urn:q> ?o }",
    ),
    (
        "distinct two columns",
        "SELECT DISTINCT ?v ?o { ?s <urn:p> ?v . ?s <urn:q> ?o }",
    ),
    ("distinct ordered", "SELECT DISTINCT ?s { ?s <urn:q> ?o }"),
    (
        "group count",
        "SELECT ?o (COUNT(*) AS ?c) { ?s <urn:q> ?o } GROUP BY ?o",
    ),
    (
        "group concat",
        "SELECT ?v (GROUP_CONCAT(?n) AS ?c) { ?s <urn:p> ?v . ?s <urn:name> ?n } GROUP BY ?v",
    ),
    (
        "ordered group",
        "SELECT ?s (COUNT(?o) AS ?c) { ?s <urn:q> ?o } GROUP BY ?s",
    ),
    (
        "top k",
        "SELECT ?s ?n { ?s <urn:name> ?n } ORDER BY ?n LIMIT 20",
    ),
    (
        "sort",
        "SELECT ?s ?n { ?s <urn:name> ?n } ORDER BY DESC(?n)",
    ),
    ("graph", "SELECT ?g ?s ?v { GRAPH ?g { ?s <urn:p> ?v } }"),
    ("sequence path", "SELECT ?s ?k { ?s <urn:q>/<urn:r> ?k }"),
    ("transitive path", "SELECT ?x { <urn:c:0> <urn:n>+ ?x }"),
    (
        "slice",
        "SELECT ?s ?o { ?s <urn:q> ?o } LIMIT 500 OFFSET 3000",
    ),
    (
        "path search",
        "PREFIX path: <urn:x-sparkles:path#> SELECT ?b ?p { SERVICE path:search { [] path:source <urn:c:0> ; path:target ?b ; path:predicate <urn:n> ; path:algorithm path:all ; path:maxLength 3000 ; path:pathIndex ?p } }",
    ),
];

/// The allowance of each fixture. An operator whose charges follow its allocations
/// gets [`Allowance::EXACT`]. A fixture with a known finding gets the measured excess
/// rounded up, under the roadmap item that owns it, so a change that makes the finding
/// worse fails and one that fixes it shows as a lower reading with `AUDIT_PRINT=1`.
fn allowance(name: &str) -> Allowance {
    const K: i64 = 1 << 10;
    let exact = Allowance::EXACT;
    match name {
        // item 1184: a leaf scan charges about 1 MiB that an in-memory store never
        // allocates
        "leaf scan" => Allowance {
            over: 1100 * K,
            peak: 1100 * K,
            ..exact
        },
        // item 1185: the grouping's state is charged after it grows, and a string
        // aggregate's charge stays above what it holds
        "group count" => Allowance {
            under: 128 * K,
            ..exact
        },
        "ordered group" => Allowance {
            under: 800 * K,
            ..exact
        },
        "group concat" => Allowance {
            under: 20 * K,
            over: 1100 * K,
            peak: 420 * K,
        },
        // item 1186: top-k keeps a charge for slots it no longer holds
        "top k" => Allowance {
            over: 72 * K,
            ..exact
        },
        // item 1187: the in-memory sort's decode of its keys charges each distinct value
        // 128 bytes and its term eight times the key's length and 128 bytes more,
        // several times what it allocates for these keys
        "sort" => Allowance {
            peak: 8000 * K,
            ..exact
        },
        // item 1188: a transitive path holds state outside its ledger inside a pull
        "transitive path" => Allowance {
            under: 52 * K,
            ..exact
        },
        // item 1189: a path search under a cursor holds its search outside its ledger
        "path search" => Allowance {
            under: 360 * K,
            ..exact
        },
        // item 1190: the change log's reader holds what it decodes outside its ledger
        "history" => Allowance {
            under: 430 * K,
            over: 72 * K,
            ..exact
        },
        // item 1191: the text reader holds its index searcher's state outside its ledger
        "text" => Allowance {
            under: 470 * K,
            over: 72 * K,
            ..exact
        },
        // item 1193: the external sort holds its pieces' sorts outside its ledger inside a
        // pull and charges its merge a formula above what it holds, and its runs are
        // written under the in-memory sort's charge of item 1187
        "sort spilled" => Allowance {
            under: 100 * K,
            over: 300 * K,
            peak: 1300 * K,
        },
        // item 1194: DISTINCT's passes hold their spill frames outside its ledger
        "distinct spilled" => Allowance {
            under: 64 * K,
            ..exact
        },
        _ => exact,
    }
}

fn run(s: &Store, q: &str, granted: bool, batch: usize) -> Audit {
    run_under(s, q, granted, batch, 256 << 20, None)
}

/// [`run`] under a budget of `budget` bytes, spilling to `spill` when it is given.
fn run_under(
    s: &Store,
    q: &str,
    granted: bool,
    batch: usize,
    budget: u64,
    spill: Option<Arc<Spill>>,
) -> Audit {
    heap::audit(|| {
        let pool = QueryPool::new(1 << 40);
        let opts = QueryOptions {
            no_cache: true,
            max_memory_bytes: Some(budget),
            grant: granted.then(|| Arc::new(pool.try_grant(0).unwrap())),
            spill,
            ..Default::default()
        };
        let cursor = CursorOptions {
            batch_rows: batch,
            ..Default::default()
        };
        select_cursor(s.snapshot(), q, &opts, &cursor).unwrap()
    })
}

fn print(name: &str, mode: &str, a: &Audit) {
    if std::env::var_os("AUDIT_PRINT").is_some() {
        println!(
            "AUDIT {name:24} {mode:7} rows={:7} batches={:4} within={:9} after={:9} over={:9} peak={:9} ledger={:9}",
            a.rows, a.batches, a.within, a.after, a.over, a.peak, a.ledger
        );
        if std::env::var_os("AUDIT_PLAN").is_some() {
            println!("PLAN {name} {}", a.plan);
        }
    }
}

/// Every fixture's heap stays within its ledger and the ledger within its heap, under
/// the query's own account and under a pool grant, at a small batch and the default.
#[test]
fn every_cursor_operator_holds_its_ledger_and_no_more() {
    let _serial = heap::serial();
    let s = store(20_000);
    let mut found = Vec::new();
    for (name, q) in FIXTURES {
        for (granted, mode) in [(false, "local"), (true, "granted")] {
            for batch in [256, 4096] {
                // Two runs warm what the store keeps between queries.
                let mut a = run(&s, q, granted, batch);
                for _ in 0..2 {
                    a = run(&s, q, granted, batch);
                }
                print(name, &format!("{mode}/{batch}"), &a);
                assert!(a.rows > 0, "{name}: no rows");
                found.extend(a.check(&format!("{name} ({mode}, batch {batch})"), &allowance(name)));
            }
        }
    }
    assert!(found.is_empty(), "{}", found.join("\n"));
}

/// The operators that spill, each under a budget that its state outgrows, so that the
/// sort writes and merges runs and DISTINCT passes rows on to later passes. Their
/// spill files are outside the heap and outside the ledger.
const SPILLED: &[(&str, &str, u64)] = &[
    (
        "sort spilled",
        "SELECT ?s ?n { ?s <urn:name> ?n } ORDER BY DESC(?n)",
        2 << 20,
    ),
    (
        "distinct spilled",
        "SELECT DISTINCT ?v ?o { ?s <urn:p> ?v . ?s <urn:q> ?o }",
        512 << 10,
    ),
];

/// Every spilling fixture holds its ledger while it spills, under both accounts and at
/// both batch sizes.
#[test]
fn every_spilling_operator_holds_its_ledger_and_no_more() {
    let _serial = heap::serial();
    let s = store(20_000);
    let tmp = tempfile::tempdir().unwrap();
    let spill = Spill::open(&SpillOptions {
        dir: tmp.path().to_path_buf(),
        max_bytes: 1 << 30,
        query_bytes: 1 << 30,
        min_free_bytes: 0,
        encrypted: false,
    })
    .unwrap();
    let mut found = Vec::new();
    for &(name, q, budget) in SPILLED {
        for (granted, mode) in [(false, "local"), (true, "granted")] {
            for batch in [256, 4096] {
                let go = || run_under(&s, q, granted, batch, budget, Some(spill.clone()));
                let mut a = go();
                for _ in 0..2 {
                    a = go();
                }
                print(name, &format!("{mode}/{batch}"), &a);
                assert!(a.rows > 0, "{name}: no rows");
                assert!(
                    a.plan.contains("spilledBytes"),
                    "{name}: no spill in {}",
                    a.plan
                );
                found.extend(a.check(&format!("{name} ({mode}, batch {batch})"), &allowance(name)));
            }
        }
    }
    assert!(found.is_empty(), "{}", found.join("\n"));
}

/// The change log's reader, over a store with 640 commits.
#[test]
fn the_history_reader_holds_its_ledger_and_no_more() {
    let _serial = heap::serial();
    let s = Store::in_memory(StoreOptions::default());
    s.snapshot()
        .change_log
        .as_ref()
        .unwrap()
        .set_background(false);
    for n in 0..640 {
        update(
            &s,
            &format!("INSERT DATA {{ <urn:s{n}> <urn:p> {n} }}"),
            &QueryOptions::default(),
        )
        .unwrap();
    }
    s.flush_change_log().unwrap();
    let q = "PREFIX hist: <urn:x-sparkles:history#> SELECT ?s ?c WHERE { SERVICE hist:changes { << ?s <urn:p> ?o >> hist:commit ?c } }";
    let mut found = Vec::new();
    for granted in [false, true] {
        let mut a = run(&s, q, granted, 64);
        for _ in 0..2 {
            a = run(&s, q, granted, 64);
        }
        print("history", if granted { "granted" } else { "local" }, &a);
        assert_eq!(a.rows, 640);
        found.extend(a.check("history", &allowance("history")));
    }
    assert!(found.is_empty(), "{}", found.join("\n"));
}

/// The text index's reader.
#[cfg(feature = "text")]
#[test]
fn the_text_reader_holds_its_ledger_and_no_more() {
    let _serial = heap::serial();
    let s = Store::in_memory(StoreOptions::default());
    let mut data = String::new();
    for i in 0..5000 {
        data.push_str(&format!(
            "<urn:d:{i}> <urn:label> \"fox number {i} in the brown woods\" .\n"
        ));
    }
    s.load(&[Source::from_bytes(
        data.into_bytes(),
        RdfFormat::NTriples,
        None,
    )])
    .unwrap();
    s.enable_text(sparkles_core::text::TextConfig::default())
        .unwrap();
    let q =
        "PREFIX text: <http://jena.apache.org/text#> SELECT ?s ?sc { (?s ?sc) text:query \"fox\" }";
    let mut found = Vec::new();
    for granted in [false, true] {
        let mut a = run(&s, q, granted, 256);
        for _ in 0..2 {
            a = run(&s, q, granted, 256);
        }
        print("text", if granted { "granted" } else { "local" }, &a);
        assert!(a.rows > 0);
        found.extend(a.check("text", &allowance("text")));
    }
    assert!(found.is_empty(), "{}", found.join("\n"));
}

/// The meter reads what the counted thread allocates and holds, whatever is freed or
/// allocated around it.
#[test]
fn the_meter_counts_the_window_of_the_counted_thread_alone() {
    let _serial = heap::serial();
    const MIB: usize = 1 << 20;
    let before = heap::pool().install(|| Vec::<u8>::with_capacity(4 * MIB));
    let (_, peak) = heap::peak(move || {
        drop(before);
        let held = Vec::<u8>::with_capacity(MIB);
        std::thread::spawn(|| drop(std::hint::black_box(Vec::<u8>::with_capacity(8 * MIB))))
            .join()
            .unwrap();
        drop(held);
    });
    assert!((MIB..2 * MIB).contains(&peak), "{peak}");
    // A reallocated block counts its old and its new bytes together, whether it moves
    // or stays where it is, which is the allocator's choice. `AUDIT_REALLOC=exact`
    // counts one that stays at its larger size.
    let exact = std::env::var("AUDIT_REALLOC").is_ok_and(|v| v == "exact");
    for (from, to) in [(MIB, MIB), (64, 2 * MIB)] {
        let (moved, peak) = heap::peak(|| unsafe {
            let layout = Layout::from_size_align(from, 8).unwrap();
            let p = std::alloc::alloc(layout);
            let old = p as usize;
            let q = std::alloc::realloc(p, layout, to);
            assert!(!q.is_null());
            std::alloc::dealloc(q, Layout::from_size_align(to, 8).unwrap());
            q as usize != old
        });
        let expected = if moved || !exact {
            from + to
        } else {
            from.max(to)
        };
        assert!(
            (expected..expected + MIB / 4).contains(&peak),
            "{from} to {to}: {peak}"
        );
    }
    // A second counted thread that frees or allocates while the first is inside the
    // allocator, between its system call and its count, cannot hide a peak, since the
    // meter holds its lock across both. Each case stops the first thread there and has
    // the other thread make the opposite call, which waits for the lock.
    let mut found = Vec::new();
    for site in [1, 2, 3] {
        let (moved, peak) = heap::peak(|| meter_race(site));
        let overlapped = heap::OVERLAPPED.load(Ordering::SeqCst);
        let expected = match site {
            // A 24 MiB block is live while the first thread allocates 16 MiB.
            1 => 40 * MIB,
            // The first thread frees 24 MiB and the other allocates 16 MiB, after the
            // free or, if the call ran inside the site, beside the block the system
            // still held.
            2 if overlapped => 40 * MIB,
            2 => 24 * MIB,
            // A 24 MiB block is live while the first thread grows 4 MiB to 16 MiB.
            _ if exact && !moved => 40 * MIB,
            _ => 44 * MIB,
        };
        if overlapped {
            found.push(format!("site {site}: the other call ran inside the site"));
        }
        if !(expected..expected + MIB / 4).contains(&peak) {
            found.push(format!("site {site}: read {peak}, expected {expected}"));
        }
    }
    assert!(found.is_empty(), "{found:#?}");
    let (_, peak) = heap::peak(|| {
        let block = Vec::<u8>::with_capacity(2 * MIB);
        std::thread::spawn(move || drop(block)).join().unwrap();
        drop(std::hint::black_box(Vec::<u8>::with_capacity(MIB)));
    });
    assert!((2 * MIB..3 * MIB).contains(&peak), "{peak}");
}

/// One case of the meter's race test, run on the counted thread. See its caller.
fn meter_race(site: usize) -> bool {
    const MIB: usize = 1 << 20;
    for flag in [&heap::REACHED, &heap::DONE, &heap::OVERLAPPED] {
        flag.store(false, Ordering::SeqCst);
    }
    let held = (site != 2).then(|| Vec::<u8>::with_capacity(24 * MIB));
    let other = std::thread::spawn(move || {
        heap::count_this_thread();
        let start = std::time::Instant::now();
        while !heap::REACHED.load(Ordering::SeqCst) {
            assert!(start.elapsed().as_secs() < 10, "the stop was never reached");
            std::hint::spin_loop();
        }
        // The opposite call: a free while the first thread allocates or grows, and an
        // allocation while it frees.
        let made = match held {
            Some(block) => {
                drop(block);
                None
            }
            None => Some(std::hint::black_box(Vec::<u8>::with_capacity(16 * MIB))),
        };
        heap::DONE.store(true, Ordering::SeqCst);
        drop(made);
    });
    heap::stop_here(true);
    let moved = unsafe {
        match site {
            1 => {
                let layout = Layout::from_size_align(16 * MIB, 8).unwrap();
                heap::HOOK.store(1, Ordering::SeqCst);
                let p = std::alloc::alloc(layout);
                assert!(!p.is_null());
                std::alloc::dealloc(p, layout);
                false
            }
            2 => {
                let layout = Layout::from_size_align(24 * MIB, 8).unwrap();
                let p = std::alloc::alloc(layout);
                assert!(!p.is_null());
                heap::HOOK.store(2, Ordering::SeqCst);
                std::alloc::dealloc(p, layout);
                false
            }
            _ => {
                let layout = Layout::from_size_align(4 * MIB, 8).unwrap();
                let p = std::alloc::alloc(layout);
                assert!(!p.is_null());
                heap::HOOK.store(3, Ordering::SeqCst);
                let q = std::alloc::realloc(p, layout, 16 * MIB);
                assert!(!q.is_null());
                std::alloc::dealloc(q, Layout::from_size_align(16 * MIB, 8).unwrap());
                q != p
            }
        }
    };
    heap::stop_here(false);
    heap::HOOK.store(0, Ordering::SeqCst);
    other.join().unwrap();
    moved
}
