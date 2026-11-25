//! Integration tests for the native streaming leaf ARQ property-function cursor
//! (`Kind::PropertyFn` with no plan inputs, item 829).
//!
//! Verifies:
//! - Zero eager materialization under `FallbackPolicy::RejectMaterialization`
//! - Exact multiset parity and work-count (`rows_produced`) parity against eager
//!   execution at batch sizes 1, 2, 3, and 4096 across all 15 ARQ `PfKind`s
//! - Duplicate-variable slots, 0-variable constant checks, cyclic RDF lists, and EXISTS
//! - Early stop under `LIMIT` without scanning the full collection
//! - Cooperative cancellation, deadline timeout, row budget, and memory budget.

use oxrdf::Term;
use oxrdfio::RdfFormat;
use sparkles_core::io::Source;
use sparkles_core::sparql::{CursorOptions, FallbackPolicy, QueryOptions, query, select_cursor};
use sparkles_core::store::{Snapshot, Store, StoreOptions};
use sparkles_core::{BudgetKind, Error};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

const P: &str = "PREFIX ex: <http://example.org/> \
    PREFIX rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#> \
    PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#> \
    PREFIX list: <http://jena.apache.org/ARQ/list#> \
    PREFIX apf: <http://jena.apache.org/ARQ/property#> ";

fn fixture_store() -> Store {
    let s = Store::in_memory(StoreOptions::default());
    let ttl = r#"
        @prefix ex: <http://example.org/> .
        @prefix rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#> .
        @prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .

        ex:list1 rdf:first 10 ; rdf:rest _:c2 .
        _:c2 rdf:first 20 ; rdf:rest _:c3 .
        _:c3 rdf:first 10 ; rdf:rest rdf:nil .

        ex:list2 rdf:first "alpha" ; rdf:rest _:d2 .
        _:d2 rdf:first "beta" ; rdf:rest rdf:nil .

        ex:emptyHolder ex:hasList rdf:nil .

        # Self-referencing member for duplicate-variable slot test
        ex:selfList rdf:first ex:selfList ; rdf:rest rdf:nil .

        # Cyclic list
        ex:cyc1 rdf:first 1 ; rdf:rest ex:cyc2 .
        ex:cyc2 rdf:first 2 ; rdf:rest ex:cyc1 .

        # Containers
        ex:bag1 a rdf:Bag ; rdf:_1 "b1" ; rdf:_3 "b3" ; rdf:_2 "b2" ; rdf:_4 "b1" .
        ex:seq1 a rdf:Seq ; rdf:_1 "s1" ; rdf:_2 "s2" .
        ex:alt1 a rdf:Alt ; rdf:_1 "a1" ; rdf:_2 "a2" .
        ex:dual a rdf:Bag, rdf:Seq ; rdf:_1 "d1" ; rdf:_2 ex:dual .

        # Direct rdfs:member triple
        ex:direct rdfs:member "direct-val" .
    "#;
    s.load(&[Source::from_bytes(
        ttl.as_bytes().to_vec(),
        RdfFormat::Turtle,
        None,
    )])
    .unwrap();
    s
}

fn bag(rows: Vec<Vec<Option<Term>>>) -> Vec<String> {
    let mut out: Vec<String> = rows.into_iter().map(|r| format!("{r:?}")).collect();
    out.sort();
    out
}

fn assert_strict_parity(snap: &Arc<Snapshot>, body: &str, expect_non_empty: bool) {
    let q = format!("{P}{body}");
    let eager_work = Arc::new(AtomicU64::new(0));
    let eager_opts = QueryOptions {
        no_cache: true,
        work: Some(eager_work.clone()),
        ..Default::default()
    };
    let eager = query(snap.clone(), &q, &eager_opts)
        .unwrap_or_else(|e| panic!("eager failed for `{body}`: {e}"));
    let expected = bag(eager
        .rows()
        .unwrap()
        .iter()
        .map(|r| r.values().to_vec())
        .collect());
    if expect_non_empty {
        assert!(!expected.is_empty(), "`{body}` answered 0 rows");
    }
    let expected_work = eager_work.load(Ordering::Relaxed);

    for batch_rows in [1, 2, 3, 4096] {
        let work = Arc::new(AtomicU64::new(0));
        let opts = QueryOptions {
            no_cache: true,
            work: Some(work.clone()),
            ..Default::default()
        };
        let cursor_opts = CursorOptions {
            batch_rows,
            fallback: FallbackPolicy::RejectMaterialization,
            ..Default::default()
        };
        let mut c = select_cursor(snap.clone(), &q, &opts, &cursor_opts)
            .unwrap_or_else(|e| panic!("strict cursor failed for `{body}`: {e}"));
        assert!(
            !c.plan().has_materialization(),
            "unexpected materialization before pull for `{body}`: {:#?}",
            c.plan()
        );
        let mut rows = Vec::new();
        while let Some(batch) = c.next_batch().unwrap() {
            for i in 0..batch.len() {
                rows.push(batch.row(i).unwrap().values().to_vec());
            }
        }
        assert!(
            !c.plan().has_materialization(),
            "unexpected materialization after pull for `{body}`: {:#?}",
            c.plan()
        );
        assert_eq!(bag(rows), expected, "`{body}` batch_rows={batch_rows}");
        assert_eq!(
            work.load(Ordering::Relaxed),
            expected_work,
            "`{body}` work mismatch batch_rows={batch_rows}"
        );
    }
}

#[test]
fn all_leaf_property_functions_stream_natively_with_parity() {
    let s = fixture_store();
    let snap = s.snapshot();

    let queries = [
        // list:member (all 4 binding modes + duplicate var + 0-var constant)
        ("SELECT ?m { ex:list1 list:member ?m }", true),
        (
            "SELECT ?m { ex:list1 list:member 10 . BIND(10 AS ?m) }",
            true,
        ),
        ("SELECT * { ex:list1 list:member 10 }", true),
        ("SELECT ?l ?m { ?l list:member ?m }", true),
        ("SELECT ?l { ?l list:member 10 }", true),
        ("SELECT ?x { ?x list:member ?x }", true),
        ("SELECT ?m { ex:cyc1 list:member ?m }", true),
        // list:length (all modes)
        ("SELECT ?len { ex:list1 list:length ?len }", true),
        ("SELECT * { ex:list1 list:length 3 }", true),
        ("SELECT * { ex:list1 list:length 99 }", false),
        ("SELECT ?l ?len { ?l list:length ?len }", true),
        ("SELECT ?l { ?l list:length 3 }", false),
        // list:index (all modes)
        ("SELECT ?i ?m { ex:list1 list:index (?i ?m) }", true),
        ("SELECT ?i { ex:list1 list:index (?i 10) }", true),
        ("SELECT ?m { ex:list1 list:index (1 ?m) }", true),
        ("SELECT * { ex:list1 list:index (1 20) }", true),
        ("SELECT ?l ?i ?m { ?l list:index (?i ?m) }", true),
        ("SELECT ?l ?i { ?l list:index (?i 10) }", true),
        ("SELECT ?l ?m { ?l list:index (0 ?m) }", true),
        // apf:strSplit (bound & unbound subject, edge delimiters)
        ("SELECT ?t { ?t apf:strSplit (\"a b c\" \" \") }", true),
        ("SELECT ?t { ?t apf:strSplit (\",a,,b,,\" \",\") }", true),
        ("SELECT ?t { ?t apf:strSplit (\"\" \",\") }", true),
        ("SELECT * { \"b\" apf:strSplit (\"a,b,c\" \",\") }", true),
        ("SELECT * { \"z\" apf:strSplit (\"a,b,c\" \",\") }", false),
        // apf:concat, apf:str, apf:splitIRI, apf:assign, apf:versionARQ
        (
            "SELECT ?s { ?s apf:concat (\"hello\" \" \" \"world\") }",
            true,
        ),
        ("SELECT * { \"ab\" apf:concat (\"a\" \"b\") }", true),
        ("SELECT ?s { ?s apf:str ex:list1 }", true),
        (
            "SELECT ?ns ?local { ex:list1 apf:splitIRI (?ns ?local) }",
            true,
        ),
        ("SELECT ?x { ?x apf:assign 42 }", true),
        ("SELECT ?x { 42 apf:assign ?x }", true),
        ("SELECT * { 42 apf:assign 42 }", true),
        ("SELECT ?s ?v { ?s apf:versionARQ ?v }", true),
        // apf:container / bag / seq / alt
        ("SELECT ?m { ex:bag1 apf:container ?m }", true),
        ("SELECT * { ex:bag1 apf:container \"b1\" }", true),
        ("SELECT ?c ?m { ?c apf:container ?m }", true),
        ("SELECT ?c { ?c apf:container \"b1\" }", true),
        ("SELECT ?c ?m { ?c apf:bag ?m }", true),
        ("SELECT ?c ?m { ?c apf:seq ?m }", true),
        ("SELECT ?c ?m { ?c apf:alt ?m }", true),
        ("SELECT ?x { ?x apf:container ?x }", true),
        // rdfs:member (stored triples + containers across all 4 binding modes)
        ("SELECT ?s ?m { ?s rdfs:member ?m }", true),
        ("SELECT ?m { ex:direct rdfs:member ?m }", true),
        ("SELECT ?m { ex:bag1 rdfs:member ?m }", true),
        ("SELECT ?s { ?s rdfs:member \"direct-val\" }", true),
        ("SELECT ?s { ?s rdfs:member \"b1\" }", true),
        ("SELECT * { ex:direct rdfs:member \"direct-val\" }", true),
        // EXISTS containing leaf PropertyFn
        (
            "SELECT ?c { ?c a rdf:Bag FILTER EXISTS { ?t apf:strSplit (\"x y\" \" \") } }",
            true,
        ),
        (
            "SELECT ?c { ?c a rdf:Bag FILTER NOT EXISTS { ex:list1 list:member 999 } }",
            true,
        ),
    ];

    for (body, non_empty) in queries {
        assert_strict_parity(&snap, body, non_empty);
    }
}

#[test]
fn large_list_limit_stops_early_and_respects_controls() {
    let s = Store::in_memory(StoreOptions::default());
    let n = 4_000usize;
    let mut nt = String::new();
    for i in 0..n {
        let next = if i + 1 < n {
            format!("<urn:cell:{}>", i + 1)
        } else {
            "<http://www.w3.org/1999/02/22-rdf-syntax-ns#nil>".to_string()
        };
        nt.push_str(&format!(
            "<urn:cell:{i}> <http://www.w3.org/1999/02/22-rdf-syntax-ns#first> {i} .\n\
             <urn:cell:{i}> <http://www.w3.org/1999/02/22-rdf-syntax-ns#rest> {next} .\n"
        ));
    }
    s.load(&[Source::from_bytes(nt.into_bytes(), RdfFormat::Turtle, None)])
        .unwrap();
    let snap = s.snapshot();
    let strict = CursorOptions {
        batch_rows: 16,
        fallback: FallbackPolicy::RejectMaterialization,
        ..Default::default()
    };

    // LIMIT 3 over a 4,000-element list stops early with work <= 32.
    let work = Arc::new(AtomicU64::new(0));
    let opts = QueryOptions {
        no_cache: true,
        work: Some(work.clone()),
        ..Default::default()
    };
    let q_limit = format!("{P}SELECT ?m {{ <urn:cell:0> list:member ?m }} LIMIT 3");
    let mut c = select_cursor(snap.clone(), &q_limit, &opts, &strict).unwrap();
    let mut got = Vec::new();
    while let Some(b) = c.next_batch().unwrap() {
        for i in 0..b.len() {
            got.push(b.row(i).unwrap().values().to_vec());
        }
    }
    assert_eq!(got.len(), 3);
    assert!(!c.plan().has_materialization());
    assert!(
        work.load(Ordering::Relaxed) <= 32,
        "work={}",
        work.load(Ordering::Relaxed)
    );

    let q_all = format!("{P}SELECT ?m {{ <urn:cell:0> list:member ?m }}");

    // Pre-cancelled query stops immediately.
    let cancel = Arc::new(AtomicBool::new(true));
    let res = select_cursor(
        snap.clone(),
        &q_all,
        &QueryOptions {
            cancel: Some(cancel),
            ..Default::default()
        },
        &strict,
    )
    .and_then(|mut c| {
        while c.next_batch()?.is_some() {}
        Ok(())
    });
    assert!(matches!(res, Err(Error::Cancelled)), "{res:?}");

    // Expired deadline stops immediately.
    let res = select_cursor(
        snap.clone(),
        &q_all,
        &QueryOptions {
            deadline: Some(Instant::now() - Duration::from_secs(1)),
            ..Default::default()
        },
        &strict,
    )
    .and_then(|mut c| {
        while c.next_batch()?.is_some() {}
        Ok(())
    });
    assert!(matches!(res, Err(Error::Timeout)), "{res:?}");

    // Tight max_rows budget fails with BudgetKind::Rows.
    let res = select_cursor(
        snap.clone(),
        &q_all,
        &QueryOptions {
            max_rows: Some(50),
            ..Default::default()
        },
        &strict,
    )
    .and_then(|mut c| {
        while c.next_batch()?.is_some() {}
        Ok(())
    });
    assert!(
        matches!(res, Err(Error::BudgetExceeded(b)) if b.kind == BudgetKind::Rows),
        "{res:?}"
    );

    // Tight max_memory_bytes budget fails with BudgetKind::Memory when seen set grows.
    let res = select_cursor(
        snap,
        &q_all,
        &QueryOptions {
            max_memory_bytes: Some(24 << 10),
            ..Default::default()
        },
        &strict,
    )
    .and_then(|mut c| {
        while c.next_batch()?.is_some() {}
        Ok(())
    });
    assert!(
        matches!(res, Err(Error::BudgetExceeded(b)) if b.kind == BudgetKind::Memory),
        "{res:?}"
    );
}
