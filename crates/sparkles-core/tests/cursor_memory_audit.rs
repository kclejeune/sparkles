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

/// The queries of the sweep: a fixture's name, its query, the plan operator it
/// exercises and whether that operator runs natively rather than as a materialized
/// fallback. A run whose plan has no such node fails.
const FIXTURES: &[(&str, &str, &str, bool)] = &[
    (
        "leaf scan",
        "SELECT ?s ?o { ?s <urn:q> ?o }",
        "IndexScan",
        true,
    ),
    (
        "filter",
        "SELECT ?s ?o { ?s <urn:q> ?o FILTER(?o != <urn:o:7>) }",
        "Filter",
        true,
    ),
    (
        "filter expression",
        "SELECT ?s ?v { ?s <urn:p> ?v FILTER(?v * 2 > 100) }",
        "Filter",
        true,
    ),
    (
        "bind",
        "SELECT ?s ?w { ?s <urn:p> ?v BIND(?v * 2 AS ?w) }",
        "Bind",
        true,
    ),
    (
        "merge join",
        "SELECT ?s ?v ?o { ?s <urn:p> ?v . ?s <urn:q> ?o }",
        "MergeJoin",
        true,
    ),
    (
        "merge optional",
        "SELECT ?s ?v ?o { ?s <urn:p> ?v OPTIONAL { ?s <urn:q> ?o } }",
        "OptionalJoin",
        true,
    ),
    (
        "conditional optional",
        "SELECT ?s ?v ?o { ?s <urn:p> ?v OPTIONAL { ?s <urn:q> ?o FILTER(?o = <urn:o:7>) } }",
        "OptionalJoin",
        true,
    ),
    (
        "hash join",
        "SELECT ?s ?n ?o { ?s <urn:name> ?n . ?s <urn:q> ?o . ?o <urn:r> ?k }",
        "HashJoin",
        true,
    ),
    (
        "union",
        "SELECT ?s ?x { { ?s <urn:p> ?x } UNION { ?s <urn:q> ?x } }",
        "Union",
        true,
    ),
    (
        "minus",
        "SELECT ?s ?v { ?s <urn:p> ?v MINUS { ?s <urn:q> <urn:o:7> } }",
        "Minus",
        true,
    ),
    (
        "exists",
        "SELECT ?s ?o { ?s <urn:q> ?o FILTER EXISTS { ?o <urn:r> ?k } }",
        "Filter",
        true,
    ),
    (
        "distinct one column",
        "SELECT DISTINCT ?o { ?s <urn:q> ?o }",
        "Distinct",
        true,
    ),
    (
        "distinct two columns",
        "SELECT DISTINCT ?v ?o { ?s <urn:p> ?v . ?s <urn:q> ?o }",
        "Distinct",
        true,
    ),
    (
        "distinct ordered",
        "SELECT DISTINCT ?s { ?s <urn:q> ?o }",
        "Distinct",
        true,
    ),
    (
        "group count",
        "SELECT ?o (COUNT(*) AS ?c) { ?s <urn:q> ?o } GROUP BY ?o",
        "GroupCountFromIndex",
        true,
    ),
    (
        "group by",
        "SELECT ?o (COUNT(*) AS ?c) (SUM(?v) AS ?t) { ?s <urn:q> ?o . ?s <urn:p> ?v } GROUP BY ?o",
        "GroupBy",
        true,
    ),
    (
        "group concat",
        "SELECT ?v (GROUP_CONCAT(?n) AS ?c) { ?s <urn:p> ?v . ?s <urn:name> ?n } GROUP BY ?v",
        "GroupBy",
        true,
    ),
    (
        "ordered group",
        "SELECT ?s (COUNT(?o) AS ?c) { ?s <urn:q> ?o } GROUP BY ?s",
        "GroupCountFromIndex",
        true,
    ),
    (
        "top k",
        "SELECT ?s ?n { ?s <urn:name> ?n } ORDER BY ?n LIMIT 20",
        "IndexTopK",
        true,
    ),
    (
        "sort",
        "SELECT ?s ?n { ?s <urn:name> ?n } ORDER BY DESC(?n)",
        "OrderBy",
        true,
    ),
    // GRAPH ?g runs as a hash join of the graph names with the patterns inside it.
    (
        "graph",
        "SELECT ?g ?s ?v { GRAPH ?g { ?s <urn:p> ?v } }",
        "HashJoin",
        true,
    ),
    (
        "sequence path",
        "SELECT ?s ?k { ?s <urn:q>/<urn:r> ?k }",
        "MergeJoin",
        true,
    ),
    (
        "transitive path",
        "SELECT ?x { <urn:c:0> <urn:n>+ ?x }",
        "TransitivePath",
        true,
    ),
    (
        "slice",
        "SELECT ?s ?o { ?s <urn:q> ?o } LIMIT 500 OFFSET 3000",
        "Limit",
        true,
    ),
    (
        "path search",
        "PREFIX path: <urn:x-sparkles:path#> SELECT ?b ?p { SERVICE path:search { [] path:source <urn:c:0> ; path:target ?b ; path:predicate <urn:n> ; path:algorithm path:all ; path:maxLength 3000 ; path:pathIndex ?p } }",
        "PathSearch",
        true,
    ),
];

/// The nodes of a plan that `QueryCursor::plan_json` wrote, parents first.
fn nodes(plan: &serde_json::Value) -> Vec<&serde_json::Value> {
    let mut out = vec![plan];
    let mut at = 0;
    while at < out.len() {
        out.extend(out[at]["children"].as_array().into_iter().flatten());
        at += 1;
    }
    out
}

/// The bytes that the nodes of `plan` running `operator` spilled, read from their own
/// counters and not their children's. A plan without such a node fails, and so does one
/// whose every such node is a materialized fallback when `native` is asked for.
fn target(plan: &str, operator: &str, native: bool) -> Result<u64, String> {
    let plan: serde_json::Value =
        serde_json::from_str(plan).map_err(|e| format!("a plan that does not parse: {e}"))?;
    let found: Vec<_> = nodes(&plan)
        .into_iter()
        .filter(|n| n["operator"]["operator"].as_str() == Some(operator))
        .filter(|n| !native || n["materializes"].as_bool() != Some(true))
        .collect();
    if found.is_empty() {
        return Err(format!(
            "no {}{operator} in {plan}",
            if native { "native " } else { "" }
        ));
    }
    Ok(found
        .iter()
        .map(|n| {
            n["operator"]["counters"]["spilledBytes"]
                .as_u64()
                .unwrap_or(0)
        })
        .sum())
}

/// The bytes the native `operator` of `plan` spilled itself, which must be some.
fn spilled(plan: &str, operator: &str) -> Result<u64, String> {
    match target(plan, operator, true)? {
        0 => Err(format!("the {operator} spilled nothing itself in {plan}")),
        bytes => Ok(bytes),
    }
}

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
        // item 1192: the filter's charge for a batch of 4,096 rows stands several times
        // above what it allocates
        "filter" => Allowance {
            peak: 600 * K,
            ..exact
        },
        // item 1187: the filter's expression charges the decode of its values at the
        // bound of `Ctx::decoded_bytes`, several times what it allocates
        "filter expression" => Allowance {
            peak: 2400 * K,
            ..exact
        },
        // item 1185: the grouping's state is charged after it grows, and its charge and
        // a string aggregate's stay above what they hold
        // and a table that the allocator grows in place, which `AUDIT_REALLOC=exact`
        // counts at its larger size alone, leaves its charge for the old and new tables
        // above five quarters of the heap's peak
        "group by" => Allowance {
            under: 96 * K,
            over: 460 * K,
            peak: if exact_realloc() { 256 * K } else { exact.peak },
        },
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
        // items 1190 and 1192: the streamed change-log reader stays within its ledger
        // except for small uncharged scratch, and overcharges per-change quad_bytes
        // that count term Arcs shared with the record (bounded per page)
        "history" => Allowance {
            under: 16 * K,
            over: 260 * K,
            ..exact
        },
        // items 1191 and 1192: the streamed text reader stays within its ledger except
        // for small Tantivy searcher scratch, and overcharges its page estimate
        // min(hits, 4096) * 192 bytes in text::Reader::open. Because enable_text builds
        // with available_parallelism().min(8) Tantivy writer threads and hit_terms
        // decodes one segment at a time, multi-segment builds hold a smaller peak heap
        // than a single-segment build against the same 958,497-byte (959 kB) ledger peak.
        // Measured granted-run heap peaks range from 313 kB (4-vCPU CI) to 407 kB
        // (406,344 bytes on 1 segment), all rounded upward in decimal kB, while 8 balanced
        // segments are estimated around 290 kB; 640 KiB (655,360 bytes) covers peaks down
        // to 243 kB under the five-quarters check.
        "text" => Allowance {
            under: 24 * K,
            over: 880 * K,
            peak: 640 * K,
        },
        // item 1193: the external sort holds its pieces' sorts outside its ledger inside a
        // pull and charges its merge a formula above what it holds, and its runs are
        // written under the in-memory sort's charge of item 1187
        "sort spilled" => Allowance {
            under: 100 * K,
            over: 300 * K,
            peak: 1300 * K,
        },
        // X19's growth rule charges a block that the allocator grows in place as a
        // move, its old and new bytes together, and `AUDIT_REALLOC=exact` counts such
        // a block once, at its larger size. The row arena of DISTINCT grows in place
        // at its peak here, so only that mode reads the rule's cost.
        "distinct two columns" if exact_realloc() => Allowance {
            peak: 192 * K,
            ..exact
        },
        // item 1185: the spilling grouping holds its partitions' state outside its
        // ledger inside a pull and keeps a charge above what it holds after it
        "group spilled" => Allowance {
            under: 24 * K,
            over: 140 * K,
            ..exact
        },
        // item 1192: the spilling hash join holds a few KiB beyond the exact allowance
        // inside a pull
        "hash join spilled" => Allowance {
            under: 24 * K,
            ..exact
        },
        // item 1194: DISTINCT's passes hold their spill frames outside its ledger
        "distinct spilled" => Allowance {
            under: 64 * K,
            ..exact
        },
        // item 1184: a rebuilt SELECT view streams through a child cursor whose in-memory
        // leaf scan charges for blocks the in-memory store never allocates
        "view rebuilt" => Allowance {
            over: 256 * K,
            peak: 256 * K,
            ..exact
        },
        _ => exact,
    }
}

/// Whether the meter counts a block grown in place at its larger size alone.
fn exact_realloc() -> bool {
    std::env::var("AUDIT_REALLOC").is_ok_and(|v| v == "exact")
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
    for &(name, q, operator, native) in FIXTURES {
        for (granted, mode) in [(false, "local"), (true, "granted")] {
            for batch in [256, 4096] {
                // Two runs warm what the store keeps between queries.
                let mut a = run(&s, q, granted, batch);
                for _ in 0..2 {
                    a = run(&s, q, granted, batch);
                }
                print(name, &format!("{mode}/{batch}"), &a);
                assert!(a.rows > 0, "{name}: no rows");
                if let Err(e) = target(&a.plan, operator, native) {
                    panic!("{name}: {e}");
                }
                found.extend(a.check(&format!("{name} ({mode}, batch {batch})"), &allowance(name)));
            }
        }
    }
    assert!(found.is_empty(), "{}", found.join("\n"));
}

/// The operators that spill, each under a budget that its state outgrows, so that the
/// sort writes and merges runs, DISTINCT passes rows on to later passes, the grouping
/// writes partitions and the hash join partitions both inputs. Each names its plan
/// operator, which must spill itself, and whether it runs over [`join_store`]. The
/// spill files are outside the heap and outside the ledger.
const SPILLED: &[(&str, &str, u64, &str, bool)] = &[
    (
        "sort spilled",
        "SELECT ?s ?n { ?s <urn:name> ?n } ORDER BY DESC(?n)",
        2 << 20,
        "OrderBy",
        false,
    ),
    (
        "distinct spilled",
        "SELECT DISTINCT ?v ?o { ?s <urn:p> ?v . ?s <urn:q> ?o }",
        512 << 10,
        "Distinct",
        false,
    ),
    (
        "group spilled",
        "SELECT ?o (COUNT(*) AS ?c) (SUM(?v) AS ?t) { ?s <urn:q> ?o . ?s <urn:p> ?v } GROUP BY ?o",
        1 << 20,
        "GroupBy",
        false,
    ),
    (
        "hash join spilled",
        "SELECT ?a ?v ?b ?w ?k { ?a <urn:v> ?v . ?a <urn:p> ?k . ?b <urn:q> ?k . ?b <urn:w> ?w . ?b <urn:l> ?l }",
        1 << 20,
        "HashJoin",
        true,
    ),
];

/// `n` rows on each side of a join on `?k` whose inputs come in no order of the key, as
/// in `spill_join.rs`, so that the cursor joins them by hash.
fn join_store(n: usize) -> Store {
    let s = Store::in_memory(StoreOptions::default());
    let key = |i: usize| (i * 7919) % (n / 3).max(1);
    let mut data = String::new();
    for i in 0..n {
        let a = (i * 104_729) % n;
        data.push_str(&format!("<urn:a:{a}> <urn:v> {} .\n", a % 13));
        if !a.is_multiple_of(20) {
            data.push_str(&format!("<urn:a:{a}> <urn:p> <urn:k:{}> .\n", key(a)));
        }
        let b = (i * 15_485_863) % n;
        data.push_str(&format!("<urn:b:{b}> <urn:w> {} .\n", b % 11));
        data.push_str(&format!("<urn:b:{b}> <urn:l> \"label {b}\" .\n"));
        if b % 20 != 3 {
            data.push_str(&format!("<urn:b:{b}> <urn:q> <urn:k:{}> .\n", key(b * 7)));
        }
    }
    s.load(&[Source::from_bytes(
        data.into_bytes(),
        RdfFormat::Turtle,
        None,
    )])
    .unwrap();
    s
}

/// Every spilling fixture holds its ledger while it spills, under both accounts and at
/// both batch sizes.
#[test]
fn every_spilling_operator_holds_its_ledger_and_no_more() {
    let _serial = heap::serial();
    let s = store(20_000);
    let joins = join_store(30_000);
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
    for &(name, q, budget, operator, join) in SPILLED {
        let s = if join { &joins } else { &s };
        for (granted, mode) in [(false, "local"), (true, "granted")] {
            for batch in [256, 4096] {
                let go = || run_under(s, q, granted, batch, budget, Some(spill.clone()));
                let mut a = go();
                for _ in 0..2 {
                    a = go();
                }
                print(name, &format!("{mode}/{batch}"), &a);
                assert!(a.rows > 0, "{name}: no rows");
                if let Err(e) = spilled(&a.plan, operator) {
                    panic!("{name} ({mode}, batch {batch}): {e}");
                }
                found.extend(a.check(&format!("{name} ({mode}, batch {batch})"), &allowance(name)));
            }
        }
    }
    assert!(found.is_empty(), "{}", found.join("\n"));
}

/// The plan checks fail a spill counted only on a child, a target whose counter is zero,
/// a plan of another operator and a materialized fallback, and pass the target's own
/// spill.
#[test]
fn the_plan_checks_find_the_operator_that_spilled() {
    let node = |op: &str, spilled: Option<u64>, materializes: bool, children: &str| {
        let counters = spilled.map_or(String::new(), |b| {
            format!(r#","counters":{{"spilledBytes":{b}}}"#)
        });
        format!(
            r#"{{"operator":{{"operator":"{op}"{counters}}},"materializes":{materializes},"children":[{children}]}}"#
        )
    };
    let child_only = node(
        "Distinct",
        None,
        false,
        &node("OrderBy", Some(4096), false, ""),
    );
    assert!(spilled(&child_only, "Distinct").is_err(), "a child's spill");
    assert_eq!(spilled(&child_only, "OrderBy"), Ok(4096));
    let zero = node(
        "Distinct",
        Some(0),
        false,
        &node("IndexScan", None, false, ""),
    );
    assert!(spilled(&zero, "Distinct").is_err(), "a zero counter");
    let other = node(
        "MergeJoin",
        Some(64),
        false,
        &node("IndexScan", None, false, ""),
    );
    assert!(
        target(&other, "HashJoin", true).is_err(),
        "another operator"
    );
    assert!(spilled(&other, "HashJoin").is_err(), "another operator");
    let fallback = node("PathSearch", None, true, "");
    assert!(target(&fallback, "PathSearch", true).is_err(), "a fallback");
    assert_eq!(target(&fallback, "PathSearch", false), Ok(0));
    let own = node(
        "Project",
        None,
        false,
        &node("HashJoin", Some(512), false, ""),
    );
    assert_eq!(spilled(&own, "HashJoin"), Ok(512));
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

/// The view reader (item 1121), over current, stale, history-rewound and rebuilt reads.
#[test]
fn the_view_reader_holds_its_ledger_and_no_more() {
    use sparkles_core::incr::{OpenHost, Runtime, RuntimeOptions, ViewDef};
    use sparkles_core::sparql::FallbackPolicy;

    let _serial = heap::serial();
    let s = Store::in_memory(StoreOptions::default());
    let mut data = String::new();
    for i in 0..2000 {
        data.push_str(&format!("<urn:s:{i}> <urn:p> \"val-{i}\" .\n"));
    }
    s.load(&[Source::from_bytes(
        data.into_bytes(),
        RdfFormat::NTriples,
        None,
    )])
    .unwrap();
    let rt = Runtime::open(None, s.dataset_id(), RuntimeOptions::default());
    rt.set_host(Arc::new(OpenHost));
    rt.put(
        &s,
        "items",
        ViewDef::table("SELECT ?s ?o WHERE { ?s <urn:p> ?o }"),
    )
    .unwrap();
    rt.round(&s);
    let snap_base = s.snapshot();

    let run_view = |snap: &Arc<sparkles_core::store::Snapshot>,
                    q: &str,
                    granted: bool,
                    batch: usize|
     -> Audit {
        heap::audit(|| {
            let pool = QueryPool::new(1 << 40);
            let opts = QueryOptions {
                no_cache: true,
                max_memory_bytes: Some(256 << 20),
                grant: granted.then(|| Arc::new(pool.try_grant(0).unwrap())),
                views: Some(rt.clone()),
                ..Default::default()
            };
            let cursor = CursorOptions {
                batch_rows: batch,
                fallback: FallbackPolicy::RejectMaterialization,
                ..Default::default()
            };
            select_cursor(snap.clone(), q, &opts, &cursor).unwrap()
        })
    };

    let q_cur = "PREFIX view: <urn:x-sparkles:view#> SELECT ?s ?o ?c { SERVICE view:read { [] view:name \"items\" ; view:bind ?s, ?o ; view:commit ?c } }";
    let q_stale = "PREFIX view: <urn:x-sparkles:view#> SELECT ?s ?o ?c { SERVICE view:read { [] view:name \"items\" ; view:bind ?s, ?o ; view:commit ?c ; view:allowStale true } }";

    let mut found = Vec::new();
    // 1) Current shared read.
    for granted in [false, true] {
        for batch in [64, 4096] {
            let mut a = run_view(&snap_base, q_cur, granted, batch);
            for _ in 0..2 {
                a = run_view(&snap_base, q_cur, granted, batch);
            }
            print(
                "view current",
                &format!("{}/{batch}", if granted { "granted" } else { "local" }),
                &a,
            );
            assert_eq!(a.rows, 2000);
            found.extend(a.check("view current", &allowance("view")));
        }
    }
    // 2) History rewind read (view ahead within history).
    let mut del = String::from("DELETE DATA { ");
    for i in 0..200 {
        del.push_str(&format!("<urn:s:{i}> <urn:p> \"val-{i}\" . "));
    }
    del.push('}');
    update(&s, &del, &QueryOptions::default()).unwrap();
    rt.round(&s);
    // Stale read ahead of snap_base before introducing new terms.
    for granted in [false, true] {
        let mut a = run_view(&snap_base, q_stale, granted, 256);
        for _ in 0..2 {
            a = run_view(&snap_base, q_stale, granted, 256);
        }
        print("view stale", if granted { "granted" } else { "local" }, &a);
        assert_eq!(a.rows, 1800);
        found.extend(a.check("view stale", &allowance("view")));
    }
    let snap_before_add = s.snapshot();
    let mut add = String::from("INSERT DATA { ");
    for i in 0..200 {
        add.push_str(&format!(
            "<urn:added:{i}> <urn:p> \"{}-{i}\" . ",
            "added".repeat(20)
        ));
    }
    add.push('}');
    update(&s, &add, &QueryOptions::default()).unwrap();
    rt.round(&s);
    for (label, snap, rows) in [
        ("view history (mixed)", &snap_base, 2000),
        ("view history (inserts)", &snap_before_add, 1800),
    ] {
        for granted in [false, true] {
            let mut a = run_view(snap, q_cur, granted, 256);
            for _ in 0..2 {
                a = run_view(snap, q_cur, granted, 256);
            }
            print(label, if granted { "granted" } else { "local" }, &a);
            assert_eq!(a.rows, rows);
            found.extend(a.check(label, &allowance("view")));
        }
    }
    // 4) Rebuilt read (history expired > 16 commits).
    for i in 0..18 {
        update(
            &s,
            &format!("INSERT DATA {{ <urn:e:{i}> <urn:p> \"e-{i}\" }}"),
            &QueryOptions::default(),
        )
        .unwrap();
        rt.round(&s);
    }
    for granted in [false, true] {
        let mut a = run_view(&snap_base, q_cur, granted, 256);
        for _ in 0..2 {
            a = run_view(&snap_base, q_cur, granted, 256);
        }
        print(
            "view rebuilt",
            if granted { "granted" } else { "local" },
            &a,
        );
        assert_eq!(a.rows, 2000);
        found.extend(a.check("view rebuilt", &allowance("view rebuilt")));
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
    let exact = exact_realloc();
    for (from, to) in [(MIB, MIB), (64, 2 * MIB)] {
        let (moved, peak) = heap::peak(|| unsafe {
            let layout = Layout::from_size_align(from, 8).unwrap();
            let p = std::alloc::alloc(layout);
            assert!(!p.is_null());
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
        } else if !heap::CONTENDED.load(Ordering::SeqCst) {
            found.push(format!(
                "site {site}: the other call never reached the allocator during the stop"
            ));
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
    // A block that the window counted stays counted when a thread that does not count
    // reallocates it.
    let (live, peak) = heap::peak(|| {
        let block = Vec::<u8>::with_capacity(MIB);
        let block = std::thread::spawn(move || {
            let mut block = block;
            block.reserve_exact(2 * MIB);
            block
        })
        .join()
        .unwrap();
        let live = heap::live();
        drop(block);
        live
    });
    assert!((2 * MIB..2 * MIB + MIB / 4).contains(&live), "{live}");
    let least = if exact { 2 * MIB } else { 3 * MIB };
    assert!((least..3 * MIB + MIB / 4).contains(&peak), "{peak}");
}

/// A window that opens or closes while a counted thread calls the allocator counts the
/// whole call. A counted thread decides under the allocator's lock whether a window is
/// open, and the window opens and closes under the lock.
#[test]
fn the_meter_counts_an_allocation_at_the_window_boundaries() {
    let _serial = heap::serial();
    const MIB: usize = 1 << 20;
    // An allocation and a reallocation count their new block, and the reallocated
    // block from before the window counts at its new size alone. A free of a block
    // from before the window counts nothing, and waits like the others.
    let mut found = Vec::new();
    for (call, least) in [(1, MIB), (2, 0), (3, MIB)] {
        let (held, contended, overlapped) = heap::opening_race(call);
        if !contended {
            found.push(format!(
                "call {call} never found the opening window's lock held"
            ));
        }
        if overlapped {
            found.push(format!("call {call} finished while the window was opening"));
        }
        if !(least..least + MIB / 8).contains(&held) {
            found.push(format!(
                "call {call}: the window that opened during it holds {held} bytes"
            ));
        }
    }
    assert!(found.is_empty(), "{found:#?}");
    let (peak, contended) = heap::closing_race();
    assert!(contended, "the closer never found the lock held");
    assert!(
        (MIB..MIB + MIB / 4).contains(&peak),
        "the window that closed during the allocation read {peak}"
    );
}

/// The watch that lets the allocator read the meter waits, as it is dropped, for a
/// reader that has its pointer, so the meter is never read after the watch is gone. It
/// waits on a normal drop and on a drop while a panic unwinds.
#[test]
fn the_watch_waits_for_a_reader_of_the_meter() {
    let _serial = heap::serial();
    let s = Store::in_memory(StoreOptions::default());
    let opts = QueryOptions {
        no_cache: true,
        ..Default::default()
    };
    let c = select_cursor(
        s.snapshot(),
        "SELECT * WHERE { ?s ?p ?o }",
        &opts,
        &CursorOptions::default(),
    )
    .unwrap();
    let meter = c.memory_meter();
    let read = [false, true].map(|unwind| heap::watch_teardown_race(&meter, unwind));
    let expected = [false, true].map(|unwind| heap::Teardown {
        contended: true,
        overlapped: false,
        stale: false,
        unwound: unwind,
    });
    assert_eq!(read, expected);
}

/// A window closes while a panic unwinds out of it, and the next window reads only its
/// own blocks.
#[test]
fn a_window_closes_when_a_panic_leaves_it() {
    let _serial = heap::serial();
    const MIB: usize = 1 << 20;
    let left = std::panic::catch_unwind(|| {
        heap::peak(|| {
            let block = std::hint::black_box(Vec::<u8>::with_capacity(4 * MIB));
            std::panic::resume_unwind(Box::new(block));
        })
    });
    assert!(left.is_err());
    assert!(!heap::is_open(), "the window stayed open after the panic");
    let (_, peak) = heap::peak(|| drop(std::hint::black_box(Vec::<u8>::with_capacity(MIB))));
    assert!((MIB..MIB + MIB / 4).contains(&peak), "{peak}");
}

/// A window whose block table filled voids its reading, even once the blocks are freed,
/// and the next window starts with an empty table.
#[test]
fn a_full_block_table_voids_the_window() {
    let _serial = heap::serial();
    const MIB: usize = 1 << 20;
    let filled = std::panic::catch_unwind(|| {
        heap::peak(|| {
            let blocks: Vec<Box<u8>> = (0..=heap::TABLE).map(|_| Box::new(0)).collect();
            drop(std::hint::black_box(blocks));
        })
    });
    let payload = filled.expect_err("a window whose table filled gave a reading");
    let message = payload
        .downcast_ref::<&str>()
        .map(|m| m.to_string())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_default();
    assert!(message.contains("table filled"), "{message}");
    let (_, peak) = heap::peak(|| drop(std::hint::black_box(Vec::<u8>::with_capacity(MIB))));
    assert!((MIB..MIB + MIB / 4).contains(&peak), "{peak}");
}

/// One case of the meter's race test, run on the counted thread. See its caller.
fn meter_race(site: usize) -> bool {
    const MIB: usize = 1 << 20;
    static READY: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    READY.store(false, Ordering::SeqCst);
    for flag in [
        &heap::REACHED,
        &heap::DONE,
        &heap::CONTENDED,
        &heap::OVERLAPPED,
    ] {
        flag.store(false, Ordering::SeqCst);
    }
    let held = (site != 2).then(|| Vec::<u8>::with_capacity(24 * MIB));
    let other = std::thread::spawn(move || {
        heap::count_this_thread();
        heap::contend_here();
        READY.store(true, Ordering::SeqCst);
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
    // The other thread is started and allocates nothing more before its call, so it
    // cannot wait for the lock that the stop holds on a call of its own start.
    while !READY.load(Ordering::SeqCst) {
        std::hint::spin_loop();
    }
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

#[test]
fn native_backward_search_admits_a_wide_filtered_frontier() {
    let _serial = heap::serial();
    let s = Store::in_memory(StoreOptions::default());
    let mut data = String::from("<urn:root> <urn:p> <urn:mid:00000> .\n");
    for i in 0..4096 {
        data.push_str(&format!("<urn:mid:{i:05}> <urn:p> <urn:target> .\n"));
    }
    data.push_str("<urn:mid:04095> <urn:p> <urn:dead:1>, <urn:dead:2>, <urn:dead:3> .\n");
    s.load(&[Source::from_bytes(
        data.into_bytes(),
        RdfFormat::Turtle,
        None,
    )])
    .unwrap();
    let q = "PREFIX path: <urn:x-sparkles:path#> SELECT ?i { SERVICE path:search { [] path:source <urn:root> ; path:target <urn:target> ; path:predicate <urn:p> ; path:algorithm path:all ; path:maxLength 2 ; path:maxDegree 2 ; path:pathIndex ?i } }";
    let _ = run(&s, q, false, 1);
    let audit = run(&s, q, false, 1);
    assert_eq!(audit.rows, 1);
    target(&audit.plan, "PathSearch", true).unwrap();
    let failures = audit.check("backward path frontier", &Allowance::EXACT);
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn native_vector_order_admits_exact_partitions() {
    let _serial = heap::serial();
    let s = Store::in_memory(StoreOptions::default());
    let data = (0..16_400)
        .map(|i| format!("<urn:s:{i}> <urn:v> \"[1,0,0]\"^^<urn:x-sparkles:vector> .\n"))
        .collect::<String>();
    s.load(&[Source::from_bytes(
        data.into_bytes(),
        RdfFormat::Turtle,
        None,
    )])
    .unwrap();
    let q = "PREFIX spk: <urn:x-sparkles:> SELECT ?s { ?s <urn:v> ?v } ORDER BY DESC(spk:cosine(?v, \"[1,0,0]\"^^spk:vector)) LIMIT 2";
    // Keep the value cache reservation step smaller than the audit tolerance.
    let _ = run_under(&s, q, false, 1, 512 << 10, None);
    let audit = run_under(&s, q, false, 1, 512 << 10, None);
    assert_eq!(audit.rows, 2);
    target(&audit.plan, "VectorSearch", true).unwrap();
    let plan = serde_json::from_str(&audit.plan).unwrap();
    assert!(nodes(&plan).into_iter().any(|node| {
        node["operator"]["operator"] == "VectorSearch"
            && node["operator"]["counters"]["method"] == "exact"
            && node["operator"]["counters"]["scored"] == 16_400
    }));
    let failures = audit.check("exact VectorOrder", &Allowance::EXACT);
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[cfg(feature = "geo")]
#[test]
fn native_constant_relation_keeps_its_input_charged_until_the_first_witness() {
    let _serial = heap::serial();
    let s = Store::in_memory(StoreOptions::default());
    let mut data = String::from("@prefix geo: <http://www.opengis.net/ont/geosparql#> .\n");
    for i in 0..1000 {
        data.push_str(&format!(
            "<urn:a> geo:hasDefaultGeometry <urn:g:{i}> .\n<urn:g:{i}> geo:asWKT \"POLYGON((0 0, {} 0, {} 10, 0 10, 0 0))\"^^geo:wktLiteral .\n\
             <urn:b> geo:asWKT \"POINT(1.{i:04} 1)\"^^geo:wktLiteral .\n",
            i + 3, i + 3
        ));
    }
    s.load(&[Source::from_bytes(
        data.into_bytes(),
        RdfFormat::Turtle,
        None,
    )])
    .unwrap();
    s.enable_geo(sparkles_core::geo::GeoConfig {
        query_rewrite: true,
        ..Default::default()
    })
    .unwrap();
    let q = "PREFIX geo: <http://www.opengis.net/ont/geosparql#> SELECT * { <urn:a> geo:sfIntersects <urn:b> } LIMIT 1";
    // Tighten the ordinary cache reservation steps for this small result.
    let _ = run_under(&s, q, false, 1, 512 << 10, None);
    let audit = run_under(&s, q, false, 1, 512 << 10, None);
    assert_eq!(audit.rows, 1);
    target(&audit.plan, "SpatialRelate", true).unwrap();
    let failures = audit.check("both-constant SpatialRelate", &Allowance::EXACT);
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[cfg(feature = "geo")]
#[test]
fn native_constant_basic_geo_keeps_longitude_scratch_bounded() {
    let _serial = heap::serial();
    let s = Store::in_memory(StoreOptions::default());
    let mut data = String::from(
        "@prefix wgs: <http://www.w3.org/2003/01/geo/wgs84_pos#> .\n<urn:point> wgs:lat 1 .\n",
    );
    for i in 0..6000 {
        data.push_str(&format!("<urn:point> wgs:long 1.{i:04} .\n"));
    }
    s.load(&[Source::from_bytes(
        data.into_bytes(),
        RdfFormat::Turtle,
        None,
    )])
    .unwrap();
    s.enable_geo(sparkles_core::geo::GeoConfig {
        wgs84: true,
        ..Default::default()
    })
    .unwrap();
    let q = "PREFIX spatial: <http://jena.apache.org/spatial#> SELECT * { <urn:point> spatial:withinBox(0 0 10 10) }";
    let _ = run_under(&s, q, false, 1, 512 << 10, None);
    let audit = run_under(&s, q, false, 1, 512 << 10, None);
    assert_eq!(audit.rows, 1);
    target(&audit.plan, "SpatialPf", true).unwrap();
    let failures = audit.check("constant-subject Basic Geo", &Allowance::EXACT);
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[cfg(feature = "geo")]
#[test]
fn native_one_constant_relation_admits_lit_pass_memo() {
    let _serial = heap::serial();
    let s = Store::in_memory(StoreOptions::default());
    let mut data = String::from("@prefix geo: <http://www.opengis.net/ont/geosparql#> .\n");
    for i in 0..4000 {
        data.push_str(&format!(
            "<urn:p:{i}> geo:asWKT \"POINT(5.{i:04} 5)\"^^geo:wktLiteral .\n"
        ));
    }
    s.load(&[Source::from_bytes(
        data.into_bytes(),
        RdfFormat::Turtle,
        None,
    )])
    .unwrap();
    s.enable_geo(sparkles_core::geo::GeoConfig {
        query_rewrite: true,
        ..Default::default()
    })
    .unwrap();
    let q = "PREFIX geo: <http://www.opengis.net/ont/geosparql#> SELECT ?s { ?s geo:sfIntersects \"POLYGON((0 0, 10 0, 10 1, 1 1, 1 10, 0 10, 0 0))\"^^geo:wktLiteral }";
    let _ = run_under(&s, q, false, 1, 512 << 10, None);
    let audit = run_under(&s, q, false, 1, 512 << 10, None);
    assert_eq!(audit.rows, 0);
    target(&audit.plan, "SpatialRelate", true).unwrap();
    let failures = audit.check(
        "one-constant SpatialRelate lit_pass memo",
        &Allowance::EXACT,
    );
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[cfg(feature = "geo")]
#[test]
fn native_one_constant_relation_fallback_admits_key_chunk() {
    let _serial = heap::serial();
    let s = Store::in_memory(StoreOptions::default());
    let mut data = String::from("@prefix geo: <http://www.opengis.net/ont/geosparql#> .\n");
    for i in 0..4096 {
        data.push_str(&format!(
            "<urn:p:{i}> geo:asWKT \"POINT(5.{:04} 5)\"^^geo:wktLiteral .\n",
            i % 64
        ));
    }
    s.load(&[Source::from_bytes(
        data.into_bytes(),
        RdfFormat::Turtle,
        None,
    )])
    .unwrap();
    // `geo:sfDisjoint` is not index-usable, so SpatialRelate takes the `OneConstCand::Scan`
    // fallback arm with a 4,096-key chunk pull while every point lies inside the query polygon.
    s.enable_geo(sparkles_core::geo::GeoConfig {
        query_rewrite: true,
        ..Default::default()
    })
    .unwrap();
    let q = "PREFIX geo: <http://www.opengis.net/ont/geosparql#> SELECT ?s { ?s geo:sfDisjoint \"POLYGON((0 0, 10 0, 10 10, 0 10, 0 0))\"^^geo:wktLiteral }";
    let _ = run_under(&s, q, false, 4096, 512 << 10, None);
    let audit = run_under(&s, q, false, 4096, 512 << 10, None);
    assert_eq!(audit.rows, 0);
    target(&audit.plan, "SpatialRelate", true).unwrap();
    let plan = serde_json::from_str(&audit.plan).unwrap();
    assert!(nodes(&plan).into_iter().any(|node| {
        node["operator"]["operator"] == "SpatialRelate"
            && node["operator"]["counters"]["candidates"] == 4096
            && node["operator"]["counters"]["treeNodesVisited"] == 0
    }));
    let failures = audit.check(
        "one-constant SpatialRelate fallback scan",
        &Allowance::EXACT,
    );
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// Items 1122 & 1123: `path:allShortest`, shortest paths to a variable end, and
/// unlimited path searches over a group input and/or nested edge pattern hold their
/// ledger within `Allowance::EXACT`.
#[test]
fn native_path_search_1122_1123_forms_hold_exact_ledger() {
    let _serial = heap::serial();
    let s = Store::in_memory(StoreOptions::default());
    let mut data = String::new();
    // A chain of 8 diamonds (256 tied shortest paths) plus a 1,500-node fan-out graph
    // and weighted edges.
    for i in 0..8 {
        data.push_str(&format!(
            "<urn:d:{i}> <urn:d> <urn:da:{i}> .\n\
             <urn:d:{i}> <urn:d> <urn:db:{i}> .\n\
             <urn:da:{i}> <urn:d> <urn:d:{}> .\n\
             <urn:db:{i}> <urn:d> <urn:d:{}> .\n\
             << <urn:d:{i}> <urn:d> <urn:da:{i}> >> <urn:w> 1.5 .\n\
             << <urn:d:{i}> <urn:d> <urn:db:{i}> >> <urn:w> 1.5 .\n\
             << <urn:da:{i}> <urn:d> <urn:d:{}> >> <urn:w> 1.0 .\n\
             << <urn:db:{i}> <urn:d> <urn:d:{}> >> <urn:w> 1.0 .\n",
            i + 1,
            i + 1,
            i + 1,
            i + 1
        ));
    }
    for i in 0..1500 {
        data.push_str(&format!("<urn:n:{i}> <urn:k> {i} .\n"));
        data.push_str(&format!(
            "<urn:n:{i}> <urn:e> <urn:n:{}> .\n",
            (i + 1) % 1500
        ));
        data.push_str(&format!(
            "<urn:n:{i}> <urn:e> <urn:n:{}> .\n",
            (i * 7 + 3) % 1500
        ));
    }
    // Wide-weight fallback graph, hub graph for `path:maxDegree`, and a high-reifier edge.
    data.push_str(
        "<urn:w:0> <urn:we> <urn:w:1> . <urn:w:1> <urn:we> <urn:w:2> .\n\
         <urn:w:0> <urn:we> <urn:w:3> . <urn:w:3> <urn:we> <urn:w:2> .\n\
         << <urn:w:0> <urn:we> <urn:w:1> >> <urn:w> 1.0 .\n\
         << <urn:w:1> <urn:we> <urn:w:2> >> <urn:w> 1e12 .\n\
         << <urn:w:0> <urn:we> <urn:w:3> >> <urn:w> 1e12 .\n\
         << <urn:w:3> <urn:we> <urn:w:2> >> <urn:w> 1.0 .\n\
         <urn:h:0> <urn:he> <urn:hub> . <urn:h:0> <urn:he> <urn:alt> . <urn:alt> <urn:he> <urn:h:1> .\n",
    );
    for i in 1..=20 {
        data.push_str(&format!("<urn:hub> <urn:he> <urn:h:{i}> .\n"));
    }
    data.push_str("<urn:mr:0> <urn:mre> <urn:mr:1> .\n");
    for i in 0..2000 {
        data.push_str(&format!(
            "<< <urn:mr:0> <urn:mre> <urn:mr:1> ~ <urn:mrr:{i}> >> <urn:w> 2.0 .\n"
        ));
    }
    s.load(&[Source::from_bytes(
        data.into_bytes(),
        RdfFormat::Turtle,
        None,
    )])
    .unwrap();

    const P: &str = "PREFIX path: <urn:x-sparkles:path#> ";
    let cases = [
        (
            "allShortest const unweighted",
            format!(
                "{P}SELECT ?p ?i ?es ?eo {{ SERVICE path:search {{ [] path:source <urn:d:0> ; path:target <urn:d:8> ; path:predicate <urn:d> ; path:algorithm path:allShortest ; path:pathIndex ?p ; path:edgeIndex ?i ; path:edgeSubject ?es ; path:edgeObject ?eo }} }}"
            ),
        ),
        (
            "allShortest const weighted",
            format!(
                "{P}SELECT ?p ?i ?c {{ SERVICE path:search {{ [] path:source <urn:d:0> ; path:target <urn:d:8> ; path:predicate <urn:d> ; path:algorithm path:allShortest ; path:weight <urn:w> ; path:pathIndex ?p ; path:edgeIndex ?i ; path:cost ?c }} }}"
            ),
        ),
        (
            "allShortest const weighted maxLength Yen",
            format!(
                "{P}SELECT ?p ?i ?c {{ SERVICE path:search {{ [] path:source <urn:d:0> ; path:target <urn:d:3> ; path:predicate <urn:d> ; path:algorithm path:allShortest ; path:weight <urn:w> ; path:maxLength 6 ; path:pathIndex ?p ; path:edgeIndex ?i ; path:cost ?c }} }}"
            ),
        ),
        (
            "shortest const weighted maxLength",
            format!(
                "{P}SELECT ?i ?c {{ SERVICE path:search {{ [] path:source <urn:d:0> ; path:target <urn:d:8> ; path:predicate <urn:d> ; path:weight <urn:w> ; path:maxLength 16 ; path:edgeIndex ?i ; path:cost ?c }} }}"
            ),
        ),
        (
            "shortest variable target weighted maxLength",
            format!(
                "{P}SELECT ?t ?c ?l {{ SERVICE path:search {{ [] path:source <urn:d:0> ; path:target ?t ; path:predicate <urn:d> ; path:weight <urn:w> ; path:maxLength 6 ; path:cost ?c ; path:length ?l }} }}"
            ),
        ),
        (
            "shortest variable source weighted maxLength",
            format!(
                "{P}SELECT ?s ?c ?l {{ SERVICE path:search {{ [] path:source ?s ; path:target <urn:d:8> ; path:predicate <urn:d> ; path:weight <urn:w> ; path:maxLength 6 ; path:cost ?c ; path:length ?l }} }}"
            ),
        ),
        (
            "kShortest const weighted maxLength",
            format!(
                "{P}SELECT ?p ?i ?c {{ SERVICE path:search {{ [] path:source <urn:d:0> ; path:target <urn:d:3> ; path:predicate <urn:d> ; path:algorithm path:kShortest ; path:k 6 ; path:weight <urn:w> ; path:maxLength 6 ; path:pathIndex ?p ; path:edgeIndex ?i ; path:cost ?c }} }}"
            ),
        ),
        (
            "allShortest wide-weight fallback",
            format!(
                "{P}SELECT ?p ?i ?c {{ SERVICE path:search {{ [] path:source <urn:w:0> ; path:target <urn:w:2> ; path:predicate <urn:we> ; path:algorithm path:allShortest ; path:weight <urn:w> ; path:pathIndex ?p ; path:edgeIndex ?i ; path:cost ?c }} }}"
            ),
        ),
        (
            "shortest const weighted maxLength wide-weight fallback",
            format!(
                "{P}SELECT ?i ?c {{ SERVICE path:search {{ [] path:source <urn:w:0> ; path:target <urn:w:2> ; path:predicate <urn:we> ; path:weight <urn:w> ; path:maxLength 3 ; path:edgeIndex ?i ; path:cost ?c }} }}"
            ),
        ),
        (
            "allShortest const weighted maxLength wide-weight fallback",
            format!(
                "{P}SELECT ?p ?i ?c {{ SERVICE path:search {{ [] path:source <urn:w:0> ; path:target <urn:w:2> ; path:predicate <urn:we> ; path:algorithm path:allShortest ; path:weight <urn:w> ; path:maxLength 3 ; path:pathIndex ?p ; path:edgeIndex ?i ; path:cost ?c }} }}"
            ),
        ),
        (
            "shortest variable target",
            format!(
                "{P}SELECT ?t ?l {{ SERVICE path:search {{ [] path:source <urn:n:0> ; path:target ?t ; path:predicate <urn:e> ; path:length ?l }} }}"
            ),
        ),
        (
            "shortest backward variable source",
            format!(
                "{P}SELECT ?s ?l {{ SERVICE path:search {{ [] path:source ?s ; path:target <urn:n:50> ; path:predicate <urn:e> ; path:length ?l }} }}"
            ),
        ),
        (
            "allShortest variable target with edge term",
            format!(
                "{P}SELECT ?t ?p ?i ?e ?l {{ SERVICE path:search {{ [] path:source <urn:d:0> ; path:target ?t ; path:predicate <urn:d> ; path:algorithm path:allShortest ; path:pathIndex ?p ; path:edgeIndex ?i ; path:edge ?e ; path:length ?l }} }}"
            ),
        ),
        (
            "shortest over group no limit",
            format!(
                "{P}SELECT ?s ?l {{ ?s <urn:k> ?k FILTER(?k < 15) SERVICE path:search {{ [] path:source ?s ; path:target <urn:n:50> ; path:predicate <urn:e> ; path:length ?l }} }}"
            ),
        ),
        (
            "mixed bound and unbound targets in group",
            format!(
                "{P}SELECT ?s ?t ?l {{ VALUES (?s ?t) {{ (<urn:n:0> <urn:n:10>) (<urn:n:0> <urn:n:20>) (<urn:n:0> UNDEF) (<urn:n:1> <urn:n:30>) }} SERVICE path:search {{ [] path:source ?s ; path:target ?t ; path:predicate <urn:e> ; path:length ?l }} }}"
            ),
        ),
        (
            "shortest over edge pattern no limit",
            format!(
                "{P}SELECT ?t ?l {{ SERVICE path:search {{ [] path:source <urn:n:0> ; path:target ?t ; path:length ?l ; path:start ?x ; path:end ?y . ?x <urn:e> ?y }} }}"
            ),
        ),
        (
            "allShortest over group and edge pattern",
            format!(
                "{P}SELECT ?s ?p ?l {{ ?s <urn:k> ?k FILTER(?k < 5) SERVICE path:search {{ [] path:source ?s ; path:target <urn:n:20> ; path:algorithm path:allShortest ; path:pathIndex ?p ; path:length ?l ; path:start ?x ; path:end ?y . ?x <urn:e> ?y }} }}"
            ),
        ),
        (
            "path:all over group and edge pattern",
            format!(
                "{P}SELECT ?s ?t ?p ?l {{ ?s <urn:k> ?k FILTER(?k < 3) SERVICE path:search {{ [] path:source ?s ; path:target ?t ; path:algorithm path:all ; path:maxLength 2 ; path:pathIndex ?p ; path:length ?l ; path:start ?x ; path:end ?y . ?x <urn:e> ?y }} }}"
            ),
        ),
        (
            "allShortest with maxDegree hub cache",
            format!(
                "{P}SELECT ?p ?i ?es ?eo {{ SERVICE path:search {{ [] path:source <urn:h:0> ; path:target <urn:h:1> ; path:predicate <urn:he> ; path:algorithm path:allShortest ; path:maxDegree 4 ; path:pathIndex ?p ; path:edgeIndex ?i ; path:edgeSubject ?es ; path:edgeObject ?eo }} }}"
            ),
        ),
        (
            "weighted shortest high-reifier streaming lookup",
            format!(
                "{P}SELECT ?c {{ SERVICE path:search {{ [] path:source <urn:mr:0> ; path:target <urn:mr:1> ; path:predicate <urn:mre> ; path:weight <urn:w> ; path:cost ?c }} }}"
            ),
        ),
        (
            "kShortest const",
            format!(
                "{P}SELECT ?p ?i {{ SERVICE path:search {{ [] path:source <urn:d:0> ; path:target <urn:d:8> ; path:predicate <urn:d> ; path:algorithm path:kShortest ; path:k 8 ; path:pathIndex ?p ; path:edgeIndex ?i }} }}"
            ),
        ),
    ];

    let mut found = Vec::new();
    for (name, q) in &cases {
        for (granted, mode) in [(false, "local"), (true, "granted")] {
            for batch in [16, 256] {
                let mut a = run(&s, q, granted, batch);
                for _ in 0..2 {
                    a = run(&s, q, granted, batch);
                }
                print(name, &format!("{mode}/{batch}"), &a);
                assert!(a.rows > 0, "{name}: no rows");
                target(&a.plan, "PathSearch", true).unwrap();
                found.extend(a.check(
                    &format!("{name} ({mode}, batch {batch})"),
                    &Allowance::EXACT,
                ));
            }
        }
    }
    assert!(found.is_empty(), "{}", found.join("\n"));
}

#[test]
fn a_rebuilt_construct_does_not_build_its_whole_circuit_for_one_row() {
    use sparkles_core::incr::{OpenHost, Runtime, RuntimeOptions, ViewDef};
    use sparkles_core::sparql::{AggregateErrors, FallbackPolicy};
    let _serial = heap::serial();
    let s = store(12_000);
    let rt = Runtime::open(None, s.dataset_id(), RuntimeOptions::default());
    rt.set_host(Arc::new(OpenHost));
    rt.put(
        &s,
        "graph",
        ViewDef::table("CONSTRUCT { ?s <urn:q> ?o } WHERE { ?s <urn:p> ?o }"),
    )
    .unwrap();
    rt.round(&s);
    let snap = s.snapshot();
    let opts = QueryOptions {
        views: Some(rt),
        aggregate_errors: Some(AggregateErrors::Sparkles),
        max_memory_bytes: Some(1 << 20),
        pool: Some(QueryPool::new(1 << 20)),
        no_cache: true,
        ..Default::default()
    };
    let cursor = CursorOptions {
        batch_rows: 1,
        fallback: FallbackPolicy::RejectMaterialization,
        ..Default::default()
    };
    let q = "PREFIX view: <urn:x-sparkles:view#> SELECT ?subject { SERVICE view:read { [] view:name \"graph\" ; view:bind ?subject } } LIMIT 1";
    let (rows, peak) = heap::peak(|| {
        let mut c = select_cursor(snap, q, &opts, &cursor).unwrap();
        c.next_batch().unwrap().unwrap().len()
    });
    assert_eq!(rows, 1);
    assert!(peak < 1 << 20, "the first pull allocated {peak} bytes");
}
