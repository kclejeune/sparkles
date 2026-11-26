//! Integration tests for native streaming `PathSearch` (group 5), `VectorSearch` ORDER BY
//! fallback (group 6 / 8b), `IndexTopK` without heap (group 6 / 8a), and `SILENT SERVICE`
//! parity / strict-refusal invariants (decision Q-SVC1).

use oxrdf::Term;
use oxrdfio::RdfFormat;
use sparkles_core::io::Source;
use sparkles_core::sparql::{
    CursorOptions, FallbackPolicy, LocalEndpoints, LocalTarget, Optimizations, QueryOptions, query,
    select_cursor,
};
use sparkles_core::store::{Snapshot, Store, StoreOptions};
use sparkles_core::{BudgetKind, Error, Result};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

const P: &str = "PREFIX path: <urn:x-sparkles:path#> \
    PREFIX ex: <http://example.org/> \
    PREFIX spk: <urn:x-sparkles:> ";

fn load(s: &Store, data: &str, format: RdfFormat) {
    s.load(&[Source::from_bytes(data.as_bytes().to_vec(), format, None)])
        .unwrap();
}

fn graph_store() -> Store {
    let s = Store::in_memory(StoreOptions::default());
    let mut data = String::from("@prefix ex: <http://example.org/> .\n");
    let n = 25;
    for i in 0..n {
        data.push_str(&format!("<urn:s:{i}> <urn:p> {i} .\n"));
        data.push_str(&format!("<urn:s:{i}> <urn:q> <urn:s:{}> .\n", (i + 1) % n));
        data.push_str(&format!(
            "<urn:s:{i}> <urn:q> <urn:s:{}> .\n",
            (i * 3 + 2) % n
        ));
    }
    for (i, v) in ["[1, 0, 0]", "[0.8, 0.6, 0]", "[0, 1, 0]", "[-1, 0, 0]"]
        .iter()
        .enumerate()
    {
        data.push_str(&format!(
            "ex:v{i} ex:emb \"{v}\"^^<urn:x-sparkles:vector> .\n"
        ));
    }
    // Add one non-vector literal to exercise generic fallback when limit > scored rows
    data.push_str("ex:vBad ex:emb \"not-a-vector\" .\n");
    load(&s, &data, RdfFormat::Turtle);
    s
}

fn bag(rows: Vec<Vec<Option<Term>>>) -> Vec<String> {
    let mut out: Vec<String> = rows.into_iter().map(|r| format!("{r:?}")).collect();
    out.sort();
    out
}

fn drain_strict(
    snap: &Arc<Snapshot>,
    q: &str,
    opts: &QueryOptions,
    batch_rows: usize,
) -> (Vec<String>, sparkles_core::sparql::CursorPlan, u64) {
    let work = Arc::new(AtomicU64::new(0));
    let opts = QueryOptions {
        no_cache: true,
        work: Some(work.clone()),
        ..opts.clone()
    };
    let cursor_opts = CursorOptions {
        batch_rows,
        fallback: FallbackPolicy::RejectMaterialization,
        ..Default::default()
    };
    let mut c = select_cursor(snap.clone(), q, &opts, &cursor_opts)
        .unwrap_or_else(|e| panic!("strict cursor failed for `{q}`: {e}"));
    let mut rows = Vec::new();
    while let Some(batch) = c.next_batch().unwrap() {
        for i in 0..batch.len() {
            rows.push(batch.row(i).unwrap().values().to_vec());
        }
    }
    let plan = c.plan().clone();
    assert!(
        !plan.has_materialization(),
        "unexpected materialization for `{q}`: {plan:#?}"
    );
    (bag(rows), plan, work.load(Ordering::Relaxed))
}

fn assert_parity(snap: &Arc<Snapshot>, body: &str, opts: &QueryOptions, non_empty: bool) {
    let q = format!("{P}{body}");
    let work_eager = Arc::new(AtomicU64::new(0));
    let eager_opts = QueryOptions {
        no_cache: true,
        work: Some(work_eager.clone()),
        ..opts.clone()
    };
    let eager = query(snap.clone(), &q, &eager_opts).unwrap();
    let expected = bag(eager
        .rows()
        .unwrap()
        .iter()
        .map(|r| r.values().to_vec())
        .collect());
    if non_empty {
        assert!(!expected.is_empty(), "fixture returned 0 rows for `{body}`");
    }
    for batch_rows in [1, 2, 3, 4096] {
        let (got, _, work_stream) = drain_strict(snap, &q, opts, batch_rows);
        assert_eq!(
            got, expected,
            "mismatch at batch_rows={batch_rows} for `{body}`"
        );
        if batch_rows == 2 {
            assert_eq!(
                work_stream,
                work_eager.load(Ordering::Relaxed),
                "work mismatch for `{body}`"
            );
        }
    }
}

#[test]
fn path_search_native_streaming_parity() {
    let store = graph_store();
    let snap = store.snapshot();
    let opts = QueryOptions::default();

    // Shortest path between constants (per-edge and per-path)
    assert_parity(
        &snap,
        "SELECT ?i ?es ?eo ?len { SERVICE path:search { [] path:source <urn:s:0> ; path:target <urn:s:7> ; path:predicate <urn:q> ; path:edgeIndex ?i ; path:edgeSubject ?es ; path:edgeObject ?eo ; path:length ?len } }",
        &opts,
        true,
    );
    // Shortest path with minLength 0 to self
    assert_parity(
        &snap,
        "SELECT ?len { SERVICE path:search { [] path:source <urn:s:0> ; path:target <urn:s:0> ; path:predicate <urn:q> ; path:minLength 0 ; path:length ?len } }",
        &opts,
        true,
    );
    // Shortest path to unreachable node (empty result)
    assert_parity(
        &snap,
        "SELECT ?i { SERVICE path:search { [] path:source <urn:s:0> ; path:target <urn:missing> ; path:predicate <urn:q> ; path:edgeIndex ?i } }",
        &opts,
        false,
    );
    // kShortest between constants
    assert_parity(
        &snap,
        "SELECT ?p ?i ?es ?eo { SERVICE path:search { [] path:source <urn:s:0> ; path:target <urn:s:7> ; path:predicate <urn:q> ; path:algorithm path:kShortest ; path:k 3 ; path:pathIndex ?p ; path:edgeIndex ?i ; path:edgeSubject ?es ; path:edgeObject ?eo } }",
        &opts,
        true,
    );
    // All paths between constants with edge triple term
    assert_parity(
        &snap,
        "SELECT ?p ?i ?e ?len { SERVICE path:search { [] path:source <urn:s:0> ; path:target <urn:s:5> ; path:predicate <urn:q> ; path:algorithm path:all ; path:maxLength 4 ; path:pathIndex ?p ; path:edgeIndex ?i ; path:edge ?e ; path:length ?len } }",
        &opts,
        true,
    );
    // All paths from constant source to variable target
    assert_parity(
        &snap,
        "SELECT ?t ?p ?len { SERVICE path:search { [] path:source <urn:s:0> ; path:target ?t ; path:predicate <urn:q> ; path:algorithm path:all ; path:maxLength 3 ; path:pathIndex ?p ; path:length ?len } }",
        &opts,
        true,
    );
    // All paths from variable source into constant target (backward DFS)
    assert_parity(
        &snap,
        "SELECT ?s ?p ?len { SERVICE path:search { [] path:source ?s ; path:target <urn:s:5> ; path:predicate <urn:q> ; path:algorithm path:all ; path:maxLength 3 ; path:pathIndex ?p ; path:length ?len } }",
        &opts,
        true,
    );
    // All paths joined with an outer pattern
    assert_parity(
        &snap,
        "SELECT ?t ?v { { SELECT ?t { SERVICE path:search { [] path:source <urn:s:0> ; path:target ?t ; path:predicate <urn:q> ; path:algorithm path:all ; path:maxLength 2 } } } ?t <urn:p> ?v }",
        &opts,
        true,
    );
    // Early stop via LIMIT on `path:all`
    let q = format!(
        "{P}SELECT ?t {{ SERVICE path:search {{ [] path:source <urn:s:0> ; path:target ?t ; path:predicate <urn:q> ; path:algorithm path:all ; path:maxLength 4 }} }} LIMIT 3 OFFSET 1"
    );
    let (rows, plan, _) = drain_strict(&snap, &q, &opts, 1);
    assert_eq!(rows.len(), 3);
    assert!(!plan.has_materialization());
}

#[test]
fn vector_order_fallback_and_index_topk_without_heap_parity() {
    let store = graph_store();
    let snap = store.snapshot();
    let opts = QueryOptions::default();

    // Vector ORDER BY answered by index (k=2 <= 4 scored vectors)
    assert_parity(
        &snap,
        "SELECT ?s { ?s ex:emb ?v } ORDER BY DESC(spk:cosine(?v, \"[1,0,0]\"^^spk:vector)) LIMIT 2",
        &opts,
        true,
    );
    // Vector ORDER BY falling back to generic scan (k=6 > 4 scored vectors)
    let q_fb = format!(
        "{P}SELECT ?s {{ ?s ex:emb ?v }} ORDER BY DESC(spk:cosine(?v, \"[1,0,0]\"^^spk:vector)) LIMIT 6"
    );
    let (got, plan, _) = drain_strict(&snap, &q_fb, &opts, 2);
    assert_eq!(got.len(), 5);
    assert!(
        format!("{plan:?}").contains("ran the generic plan"),
        "expected generic fallback note in plan: {plan:#?}"
    );
    assert_parity(
        &snap,
        "SELECT ?s { ?s ex:emb ?v } ORDER BY DESC(spk:cosine(?v, \"[1,0,0]\"^^spk:vector)) LIMIT 6",
        &opts,
        true,
    );

    // IndexTopK with `topk_heap = false`
    let decimals = Store::in_memory(StoreOptions::default());
    let data: String = (0..50_000)
        .map(|i| {
            format!(
                "<urn:s:{i}> <urn:v> \"{}.{:02}\"^^<http://www.w3.org/2001/XMLSchema#decimal> .\n",
                (i * 7919) % 5000,
                i % 100
            )
        })
        .collect();
    load(&decimals, &data, RdfFormat::Turtle);
    let mut no_heap = Optimizations::ALL;
    no_heap.topk_heap = false;
    let no_heap_opts = QueryOptions {
        optimizations: Some(no_heap),
        ..Default::default()
    };
    assert_parity(
        &decimals.snapshot(),
        "SELECT ?s ?v { ?s <urn:v> ?v } ORDER BY DESC(?v) OFFSET 2 LIMIT 5",
        &no_heap_opts,
        true,
    );
}

#[test]
fn path_and_vector_cursors_enforce_memory_cancellation_and_timeout() {
    let store = graph_store();
    let snap = store.snapshot();
    let queries = [
        format!(
            "{P}SELECT ?t ?p {{ SERVICE path:search {{ [] path:source <urn:s:0> ; path:target ?t ; path:predicate <urn:q> ; path:algorithm path:all ; path:maxLength 5 ; path:pathIndex ?p }} }}"
        ),
        format!(
            "{P}SELECT ?i {{ SERVICE path:search {{ [] path:source <urn:s:0> ; path:target <urn:s:7> ; path:predicate <urn:q> ; path:edgeIndex ?i }} }}"
        ),
        format!(
            "{P}SELECT ?s {{ ?s ex:emb ?v }} ORDER BY DESC(spk:cosine(?v, \"[1,0,0]\"^^spk:vector)) LIMIT 2"
        ),
    ];
    let strict = CursorOptions {
        batch_rows: 4,
        fallback: FallbackPolicy::RejectMaterialization,
        ..Default::default()
    };
    for q in &queries {
        let cancel = Arc::new(AtomicBool::new(true));
        let opts = QueryOptions {
            no_cache: true,
            cancel: Some(cancel),
            ..Default::default()
        };
        let res = select_cursor(snap.clone(), q, &opts, &strict)
            .and_then(|mut c| c.next_batch().map(|_| ()));
        assert!(matches!(res, Err(Error::Cancelled)), "`{q}`: {res:?}");

        let opts = QueryOptions {
            no_cache: true,
            deadline: Some(Instant::now() - Duration::from_secs(1)),
            ..Default::default()
        };
        let res = select_cursor(snap.clone(), q, &opts, &strict)
            .and_then(|mut c| c.next_batch().map(|_| ()));
        assert!(matches!(res, Err(Error::Timeout)), "`{q}`: {res:?}");

        let opts = QueryOptions {
            no_cache: true,
            max_memory_bytes: Some(1),
            ..Default::default()
        };
        let res = select_cursor(snap.clone(), q, &opts, &strict).and_then(|mut c| {
            while c.next_batch()?.is_some() {}
            Ok(())
        });
        assert!(
            matches!(res, Err(Error::BudgetExceeded(b)) if b.kind == BudgetKind::Memory),
            "`{q}`: {res:?}"
        );
    }
}

/// Native `SERVICE SILENT` buffers its answer under the memory budget so any remote or
/// local endpoint error (including mid-stream failures) is suppressed into a single empty
/// binding, matching eager execution with zero materialization under `RejectMaterialization`.
#[test]
fn silent_service_runs_natively_and_preserves_error_suppression_parity() {
    #[derive(Debug)]
    struct FailingLocal;
    impl LocalEndpoints for FailingLocal {
        fn resolve(&self, iri: &str) -> Option<Result<LocalTarget>> {
            (iri == "http://broken/sparql").then(|| Err(Error::Service("endpoint down".into())))
        }
    }

    let store = graph_store();
    let snap = store.snapshot();
    let opts = QueryOptions {
        no_cache: true,
        allow_service: true,
        local_endpoints: Some(Arc::new(FailingLocal)),
        ..Default::default()
    };
    let q = "SELECT ?s ?o { SERVICE SILENT <http://broken/sparql> { ?s <urn:p> ?o } }";

    let eager = query(snap.clone(), q, &opts).unwrap();
    let expected = bag(eager
        .rows()
        .unwrap()
        .iter()
        .map(|r| r.values().to_vec())
        .collect());
    assert_eq!(expected.len(), 1);

    // Strict policy runs SILENT SERVICE natively without eager fallback.
    for batch_rows in [1, 2, 4096] {
        let strict = CursorOptions {
            batch_rows,
            fallback: FallbackPolicy::RejectMaterialization,
            ..Default::default()
        };
        let mut c = select_cursor(snap.clone(), q, &opts, &strict)
            .expect("SILENT SERVICE must run natively under RejectMaterialization");
        assert!(!c.plan().has_materialization());
        let mut rows = Vec::new();
        while let Some(batch) = c.next_batch().unwrap() {
            for i in 0..batch.len() {
                rows.push(batch.row(i).unwrap().values().to_vec());
            }
        }
        assert!(!c.plan().has_materialization());
        assert_eq!(bag(rows), expected, "batch_rows={batch_rows}");
    }
}

/// Regression for S4 & S5:
/// - Backward BFS in `path:all` pre-admits a high fan-in frontier before allocation.
/// - Per-edge path longer than `batch_rows` maintains parity and charges `terms`/`current`
///   across pulls.
/// - DFS pruned-neighbour loop polls cancellation/timeout even when every neighbour is
///   pruned by the backward distance map.
#[test]
fn path_search_pre_admission_batch_boundary_and_pruned_cancel() {
    // 1. High fan-in backward BFS under tight memory budget fails with Memory budget.
    let fanin = Store::in_memory(StoreOptions::default());
    let mut data = String::new();
    for i in 0..400 {
        data.push_str(&format!("<urn:in:{i}> <urn:e> <urn:hub> .\n"));
    }
    data.push_str("<urn:start> <urn:e> <urn:in:0> .\n");
    load(&fanin, &data, RdfFormat::Turtle);
    let strict = CursorOptions {
        batch_rows: 1,
        fallback: FallbackPolicy::RejectMaterialization,
        ..Default::default()
    };
    let tight = QueryOptions {
        no_cache: true,
        max_memory_bytes: Some(2048),
        ..Default::default()
    };
    let q_bfs = format!(
        "{P}SELECT ?p {{ SERVICE path:search {{ [] path:source <urn:start> ; path:target <urn:hub> ; path:predicate <urn:e> ; path:algorithm path:all ; path:maxLength 2 ; path:pathIndex ?p }} }}"
    );
    let res = select_cursor(fanin.snapshot(), &q_bfs, &tight, &strict).and_then(|mut c| {
        while c.next_batch()?.is_some() {}
        Ok(())
    });
    assert!(
        matches!(res, Err(Error::BudgetExceeded(b)) if b.kind == BudgetKind::Memory),
        "BFS fan-in pre-admission: {res:?}"
    );

    // 2. Per-edge path longer than batch_rows=1 with edge triple terms (`path:edge ?e`).
    let chain = Store::in_memory(StoreOptions::default());
    let mut chain_data = String::new();
    for i in 0..12 {
        chain_data.push_str(&format!("<urn:c:{i}> <urn:next> <urn:c:{}> .\n", i + 1));
    }
    load(&chain, &chain_data, RdfFormat::Turtle);
    assert_parity(
        &chain.snapshot(),
        "SELECT ?i ?e ?len { SERVICE path:search { [] path:source <urn:c:0> ; path:target <urn:c:12> ; path:predicate <urn:next> ; path:algorithm path:all ; path:maxLength 12 ; path:edgeIndex ?i ; path:edge ?e ; path:length ?len } }",
        &QueryOptions::default(),
        true,
    );

    // 3. DFS pruned-neighbour cancellation: <urn:src> has 3,000 dead-end neighbours that
    //    cannot reach <urn:dst> (all pruned by backward distance map `d`), so `steps` never
    //    advances; expired deadline triggered after open must still be caught promptly.
    let dead = Store::in_memory(StoreOptions::default());
    let mut dead_data = String::new();
    for i in 0..3000 {
        dead_data.push_str(&format!("<urn:src> <urn:p> <urn:dead:{i}> .\n"));
    }
    dead_data.push_str("<urn:other> <urn:p> <urn:dst> .\n");
    load(&dead, &dead_data, RdfFormat::Turtle);
    let q_pruned = format!(
        "{P}SELECT ?p {{ SERVICE path:search {{ [] path:source <urn:src> ; path:target <urn:dst> ; path:predicate <urn:p> ; path:algorithm path:all ; path:maxLength 2 ; path:pathIndex ?p }} }}"
    );
    let cancel = Arc::new(AtomicBool::new(true));
    let cancel_opts = QueryOptions {
        no_cache: true,
        cancel: Some(cancel),
        ..Default::default()
    };
    let res = select_cursor(dead.snapshot(), &q_pruned, &cancel_opts, &strict)
        .and_then(|mut c| c.next_batch().map(|_| ()));
    assert!(
        matches!(res, Err(Error::Cancelled)),
        "pruned DFS cancel: {res:?}"
    );
}

/// Regression for S6 & N4:
/// - Extended path search forms (weighted, maxDegree, direction backward/both,
///   excludePredicate, minLength 0 / maxLength 0 with path:all, kShortest budgets).
/// - `VectorOrder` with small `k` and large pending delta (pre-admission & parity).
/// - `IndexTopK` with `topk_heap = false` under cancel, timeout, and tight memory.
#[test]
fn path_search_extended_forms_and_native_topk_pre_admission() {
    let store = Store::in_memory(StoreOptions::default());
    let mut data = String::from("@prefix ex: <http://example.org/> .\n");
    data.push_str(
        r#"
        <urn:a> <urn:p> <urn:b> .
        <urn:b> <urn:p> <urn:c> .
        <urn:a> <urn:p> <urn:c> .
        <urn:c> <urn:q> <urn:d> .
        <urn:b> <urn:skip> <urn:d> .
        << <urn:a> <urn:p> <urn:b> >> <urn:w> 1.5 .
        << <urn:b> <urn:p> <urn:c> >> <urn:w> 1.0 .
        << <urn:a> <urn:p> <urn:c> >> <urn:w> 10.0 .
        "#,
    );
    load(&store, &data, RdfFormat::Turtle);
    let snap = store.snapshot();
    let opts = QueryOptions::default();

    // Weighted shortest / kShortest / all
    assert_parity(
        &snap,
        "SELECT ?i ?es ?eo ?c { SERVICE path:search { [] path:source <urn:a> ; path:target <urn:c> ; path:predicate <urn:p> ; path:weight <urn:w> ; path:edgeIndex ?i ; path:edgeSubject ?es ; path:edgeObject ?eo ; path:cost ?c } }",
        &opts,
        true,
    );
    assert_parity(
        &snap,
        "SELECT ?p ?c { SERVICE path:search { [] path:source <urn:a> ; path:target <urn:c> ; path:predicate <urn:p> ; path:algorithm path:kShortest ; path:k 2 ; path:weight <urn:w> ; path:pathIndex ?p ; path:cost ?c } }",
        &opts,
        true,
    );
    assert_parity(
        &snap,
        "SELECT ?p ?c { SERVICE path:search { [] path:source <urn:a> ; path:target <urn:c> ; path:predicate <urn:p> ; path:algorithm path:all ; path:maxLength 3 ; path:weight <urn:w> ; path:pathIndex ?p ; path:cost ?c } }",
        &opts,
        true,
    );
    // Direction backward & both, maxDegree, excludePredicate, minLength 0 / maxLength 0
    assert_parity(
        &snap,
        "SELECT ?t ?len { SERVICE path:search { [] path:source <urn:c> ; path:target ?t ; path:predicate <urn:p> ; path:algorithm path:all ; path:direction path:backward ; path:maxLength 2 ; path:length ?len } }",
        &opts,
        true,
    );
    assert_parity(
        &snap,
        "SELECT ?t ?len { SERVICE path:search { [] path:source <urn:b> ; path:target ?t ; path:predicate <urn:p> ; path:algorithm path:all ; path:direction path:both ; path:maxLength 2 ; path:length ?len } }",
        &opts,
        true,
    );
    assert_parity(
        &snap,
        "SELECT ?t ?len { SERVICE path:search { [] path:source <urn:a> ; path:target ?t ; path:predicate <urn:p> ; path:algorithm path:all ; path:maxLength 3 ; path:maxDegree 1 ; path:length ?len } }",
        &opts,
        true,
    );
    assert_parity(
        &snap,
        "SELECT ?t ?len { SERVICE path:search { [] path:source <urn:a> ; path:target ?t ; path:algorithm path:all ; path:maxLength 3 ; path:excludePredicate <urn:skip> ; path:length ?len } }",
        &opts,
        true,
    );
    assert_parity(
        &snap,
        "SELECT ?t ?len { SERVICE path:search { [] path:source <urn:a> ; path:target ?t ; path:predicate <urn:p> ; path:algorithm path:all ; path:minLength 0 ; path:maxLength 0 ; path:length ?len } }",
        &opts,
        true,
    );

    // S6: VectorOrder with small k=2 and large pending delta (500 inserted vector triples).
    let vstore = Store::in_memory(StoreOptions::default());
    load(
        &vstore,
        r#"
        @prefix ex: <http://example.org/> .
        ex:base0 ex:emb "[1, 0, 0]"^^<urn:x-sparkles:vector> .
        ex:base1 ex:emb "[0, 1, 0]"^^<urn:x-sparkles:vector> .
        "#,
        RdfFormat::Turtle,
    );
    // Insert 500 delta vectors into the live delta overlay.
    let mut delta_ttl = String::from("@prefix ex: <http://example.org/> .\n");
    for i in 0..500 {
        let a = ((i % 100) as f64) / 100.0;
        let b = (1.0 - a * a).max(0.0).sqrt();
        delta_ttl.push_str(&format!(
            "ex:d{i} ex:emb \"[{a:.3}, {b:.3}, 0]\"^^<urn:x-sparkles:vector> .\n"
        ));
    }
    sparkles_core::sparql::update::update(
        &vstore,
        &format!(
            "PREFIX ex: <http://example.org/> INSERT DATA {{ {} }}",
            delta_ttl.trim_start_matches("@prefix ex: <http://example.org/> .\n")
        ),
        &QueryOptions::default(),
    )
    .unwrap();
    let vsnap = vstore.snapshot();
    assert_parity(
        &vsnap,
        "SELECT ?s { ?s ex:emb ?v } ORDER BY DESC(spk:cosine(?v, \"[1,0,0]\"^^spk:vector)) LIMIT 2",
        &opts,
        true,
    );
    let strict = CursorOptions {
        batch_rows: 4,
        fallback: FallbackPolicy::RejectMaterialization,
        ..Default::default()
    };
    let tight = QueryOptions {
        no_cache: true,
        max_memory_bytes: Some(2048),
        ..Default::default()
    };
    let vq = format!(
        "{P}SELECT ?s {{ ?s ex:emb ?v }} ORDER BY DESC(spk:cosine(?v, \"[1,0,0]\"^^spk:vector)) LIMIT 2"
    );
    let res = select_cursor(vsnap, &vq, &tight, &strict).and_then(|mut c| {
        while c.next_batch()?.is_some() {}
        Ok(())
    });
    assert!(
        matches!(res, Err(Error::BudgetExceeded(b)) if b.kind == BudgetKind::Memory),
        "VectorOrder delta pre-admission: {res:?}"
    );

    // S6 + N4: IndexTopK (`topk_heap = false`) and `path:kShortest` cancel, timeout, memory.
    let decimals = Store::in_memory(StoreOptions::default());
    let dec_data: String = (0..10_000)
        .map(|i| {
            format!(
                "<urn:s:{i}> <urn:v> \"{}.{:02}\"^^<http://www.w3.org/2001/XMLSchema#decimal> .\n",
                (i * 7919) % 5000,
                i % 100
            )
        })
        .collect();
    load(&decimals, &dec_data, RdfFormat::Turtle);
    let mut no_heap = Optimizations::ALL;
    no_heap.topk_heap = false;
    let q_topk = "SELECT ?s ?v { ?s <urn:v> ?v } ORDER BY DESC(?v) LIMIT 5";
    for (snap_i, q_i, opt_base) in [
        (
            decimals.snapshot(),
            q_topk.to_string(),
            QueryOptions {
                no_cache: true,
                optimizations: Some(no_heap),
                ..Default::default()
            },
        ),
        (
            snap.clone(),
            format!(
                "{P}SELECT ?p ?c {{ SERVICE path:search {{ [] path:source <urn:a> ; path:target <urn:c> ; path:predicate <urn:p> ; path:algorithm path:kShortest ; path:k 2 ; path:weight <urn:w> ; path:pathIndex ?p ; path:cost ?c }} }}"
            ),
            QueryOptions {
                no_cache: true,
                ..Default::default()
            },
        ),
    ] {
        let cancel_opts = QueryOptions {
            cancel: Some(Arc::new(AtomicBool::new(true))),
            ..opt_base.clone()
        };
        let res = select_cursor(snap_i.clone(), &q_i, &cancel_opts, &strict)
            .and_then(|mut c| c.next_batch().map(|_| ()));
        assert!(matches!(res, Err(Error::Cancelled)), "`{q_i}`: {res:?}");

        let timeout_opts = QueryOptions {
            deadline: Some(Instant::now() - Duration::from_secs(1)),
            ..opt_base.clone()
        };
        let res = select_cursor(snap_i.clone(), &q_i, &timeout_opts, &strict)
            .and_then(|mut c| c.next_batch().map(|_| ()));
        assert!(matches!(res, Err(Error::Timeout)), "`{q_i}`: {res:?}");

        let mem_opts = QueryOptions {
            max_memory_bytes: Some(128),
            ..opt_base
        };
        let res = select_cursor(snap_i, &q_i, &mem_opts, &strict).and_then(|mut c| {
            while c.next_batch()?.is_some() {}
            Ok(())
        });
        assert!(
            matches!(res, Err(Error::BudgetExceeded(b)) if b.kind == BudgetKind::Memory),
            "`{q_i}`: {res:?}"
        );
    }
}

#[test]
fn vector_order_budget_refusal_runs_the_generic_child_to_completion() {
    let store = Store::in_memory(StoreOptions::default());
    load(
        &store,
        "<http://example.org/base> <http://example.org/emb> \"[1,0,0]\"^^<urn:x-sparkles:vector> .",
        RdfFormat::Turtle,
    );
    let inserts = (0..12_000)
        .map(|i| format!("ex:d{i:05} ex:emb \"[0,1,0]\"^^spk:vector .\n"))
        .collect::<String>();
    sparkles_core::sparql::update::update(
        &store,
        &format!("{P}INSERT DATA {{ {inserts} }}"),
        &QueryOptions::default(),
    )
    .unwrap();
    let query = format!(
        "{P}SELECT ?s {{ ?s ex:emb ?v }} ORDER BY DESC(spk:cosine(?v, \"[1,0,0]\"^^spk:vector)) LIMIT 2"
    );
    let snap = store.snapshot();
    let (expected, native_plan, _) = drain_strict(&snap, &query, &QueryOptions::default(), 4);
    assert_eq!(expected.len(), 2);
    assert!(format!("{native_plan:?}").contains("the vector index answered"));
    let budget = QueryOptions {
        max_memory_bytes: Some(256 << 10),
        ..QueryOptions::default()
    };
    let (rows, generic_plan, _) = drain_strict(&snap, &query, &budget, 4);
    assert_eq!(rows, expected);
    assert!(format!("{generic_plan:?}").contains("ran the generic plan"));
    assert!(!format!("{generic_plan:?}").contains("the vector index answered"));
}

fn assert_parity_batches(
    snap: &Arc<Snapshot>,
    body: &str,
    opts: &QueryOptions,
    non_empty: bool,
    expect_full_input: bool,
) {
    let q = format!("{P}{body}");
    let work_eager = Arc::new(AtomicU64::new(0));
    let eager_opts = QueryOptions {
        no_cache: true,
        work: Some(work_eager.clone()),
        ..opts.clone()
    };
    let eager = query(snap.clone(), &q, &eager_opts).unwrap();
    let expected = bag(eager
        .rows()
        .unwrap()
        .iter()
        .map(|r| r.values().to_vec())
        .collect());
    if non_empty {
        assert!(!expected.is_empty(), "fixture returned 0 rows for `{body}`");
    }
    fn find_ps_info(
        p: &sparkles_core::sparql::PlanInfo,
    ) -> Option<&sparkles_core::sparql::PlanInfo> {
        if p.operator == "PathSearch" {
            return Some(p);
        }
        p.children.iter().find_map(find_ps_info)
    }
    let eager_ps_counters = find_ps_info(eager.plan()).and_then(|p| p.counters.clone());
    for batch_rows in [1, 2, 7, 64, 4096] {
        let (got, plan, work_stream) = drain_strict(snap, &q, opts, batch_rows);
        assert_eq!(
            got, expected,
            "mismatch at batch_rows={batch_rows} for `{body}`"
        );
        assert_eq!(
            work_stream,
            work_eager.load(Ordering::Relaxed),
            "work mismatch at batch_rows={batch_rows} for `{body}`"
        );
        fn find_ps(
            p: &sparkles_core::sparql::CursorPlan,
        ) -> Option<&sparkles_core::sparql::CursorPlan> {
            if p.operator.operator == "PathSearch" {
                return Some(p);
            }
            p.children.iter().find_map(find_ps)
        }
        let ps = find_ps(&plan).expect("PathSearch node in plan");
        assert_eq!(
            ps.full_input_before_output, expect_full_input,
            "full_input_before_output mismatch for `{body}`: {ps:#?}"
        );
        assert_eq!(
            ps.operator.counters, eager_ps_counters,
            "PathSearch counters mismatch at batch_rows={batch_rows} for `{body}`"
        );
    }
}

/// Item 1122: `path:allShortest` and `path:shortest` with a variable end run natively
/// under strict cursors and match eager answers across every batch size.
#[test]
fn path_search_item_1122_all_shortest_and_variable_end_parity() {
    let store = Store::in_memory(StoreOptions::default());
    let mut data = String::from("@prefix ex: <http://example.org/> .\n");
    // Diamond chain: 3 diamonds -> 8 tied shortest paths from d0 to d3.
    for i in 0..3 {
        data.push_str(&format!(
            "ex:d{i} ex:e ex:da{i} . ex:d{i} ex:e ex:db{i} .\n\
             ex:da{i} ex:e ex:d{} . ex:db{i} ex:e ex:d{} .\n\
             << ex:d{i} ex:e ex:da{i} >> ex:w 1.5 .\n\
             << ex:d{i} ex:e ex:db{i} >> ex:w 1.5 .\n\
             << ex:da{i} ex:e ex:d{} >> ex:w 1.0 .\n\
             << ex:db{i} ex:e ex:d{} >> ex:w 1.0 .\n",
            i + 1,
            i + 1,
            i + 1,
            i + 1
        ));
    }
    // Cycle back to d0 with zero-weight edge and wide-weight fallback edge.
    data.push_str(
        r#"
        ex:d3 ex:e ex:d0 .
        << ex:d3 ex:e ex:d0 >> ex:w 0.0 .
        ex:d0 ex:e ex:d1 .
        ex:w0 ex:wedge ex:w1 . ex:w1 ex:wedge ex:w2 . ex:w0 ex:wedge ex:w3 . ex:w3 ex:wedge ex:w2 .
        << ex:w0 ex:wedge ex:w1 >> ex:w 1.0 .
        << ex:w1 ex:wedge ex:w2 >> ex:w 1e12 .
        << ex:w0 ex:wedge ex:w3 >> ex:w 1e12 .
        << ex:w3 ex:wedge ex:w2 >> ex:w 1.0 .
        ex:h0 ex:e ex:hub . ex:hub ex:e ex:h1 . ex:hub ex:e ex:h2 . ex:hub ex:e ex:h3 .
        ex:h0 ex:e ex:alt . ex:alt ex:e ex:h1 .
        "#,
    );
    load(&store, &data, RdfFormat::Turtle);
    let snap = store.snapshot();
    let opts = QueryOptions::default();

    // Unweighted allShortest between constants (per-edge + edge triple term)
    assert_parity_batches(
        &snap,
        "SELECT ?p ?i ?e ?len { SERVICE path:search { [] path:source ex:d0 ; path:target ex:d3 ; path:predicate ex:e ; path:algorithm path:allShortest ; path:pathIndex ?p ; path:edgeIndex ?i ; path:edge ?e ; path:length ?len } }",
        &opts,
        true,
        false,
    );
    // Unweighted allShortest cycle (minLength 1 and minLength 0)
    assert_parity_batches(
        &snap,
        "SELECT ?p ?i ?es ?eo ?len { SERVICE path:search { [] path:source ex:d0 ; path:target ex:d0 ; path:predicate ex:e ; path:algorithm path:allShortest ; path:pathIndex ?p ; path:edgeIndex ?i ; path:edgeSubject ?es ; path:edgeObject ?eo ; path:length ?len } }",
        &opts,
        true,
        false,
    );
    assert_parity_batches(
        &snap,
        "SELECT ?p ?len { SERVICE path:search { [] path:source ex:d0 ; path:target ex:d0 ; path:predicate ex:e ; path:algorithm path:allShortest ; path:minLength 0 ; path:pathIndex ?p ; path:length ?len } }",
        &opts,
        true,
        false,
    );
    // Weighted allShortest between constants (bidirectional Dijkstra + wide-weight fallback + maxLength Yen)
    assert_parity_batches(
        &snap,
        "SELECT ?p ?i ?es ?eo ?c { SERVICE path:search { [] path:source ex:d0 ; path:target ex:d3 ; path:predicate ex:e ; path:algorithm path:allShortest ; path:weight ex:w ; path:pathIndex ?p ; path:edgeIndex ?i ; path:edgeSubject ?es ; path:edgeObject ?eo ; path:cost ?c } }",
        &opts,
        true,
        false,
    );
    assert_parity_batches(
        &snap,
        "SELECT ?p ?i ?c { SERVICE path:search { [] path:source ex:w0 ; path:target ex:w2 ; path:predicate ex:wedge ; path:algorithm path:allShortest ; path:weight ex:w ; path:pathIndex ?p ; path:edgeIndex ?i ; path:cost ?c } }",
        &opts,
        true,
        false,
    );
    // Weighted shortest / allShortest / kShortest with maxLength (item 275): const pairs,
    // cycle, VALUES, unbound target, unbound source, minLength 0, maxLength 0 and 1.
    assert_parity_batches(
        &snap,
        "SELECT ?i ?es ?eo ?c { SERVICE path:search { [] path:source ex:d0 ; path:target ex:d3 ; path:predicate ex:e ; path:weight ex:w ; path:maxLength 6 ; path:edgeIndex ?i ; path:edgeSubject ?es ; path:edgeObject ?eo ; path:cost ?c } }",
        &opts,
        true,
        false,
    );
    assert_parity_batches(
        &snap,
        "SELECT ?i ?es ?eo ?c { SERVICE path:search { [] path:source ex:d0 ; path:target ex:d0 ; path:predicate ex:e ; path:weight ex:w ; path:maxLength 7 ; path:edgeIndex ?i ; path:edgeSubject ?es ; path:edgeObject ?eo ; path:cost ?c } }",
        &opts,
        true,
        false,
    );
    assert_parity_batches(
        &snap,
        "SELECT ?t ?c ?len { SERVICE path:search { [] path:source ex:d0 ; path:target ?t ; path:predicate ex:e ; path:weight ex:w ; path:maxLength 4 ; path:cost ?c ; path:length ?len } }",
        &opts,
        true,
        false,
    );
    assert_parity_batches(
        &snap,
        "SELECT ?t ?c ?len { SERVICE path:search { [] path:source ex:d0 ; path:target ?t ; path:predicate ex:e ; path:weight ex:w ; path:minLength 0 ; path:maxLength 2 ; path:cost ?c ; path:length ?len } }",
        &opts,
        true,
        false,
    );
    assert_parity_batches(
        &snap,
        "SELECT ?t ?c ?len { SERVICE path:search { [] path:source ex:d0 ; path:target ?t ; path:predicate ex:e ; path:weight ex:w ; path:minLength 0 ; path:maxLength 0 ; path:cost ?c ; path:length ?len } }",
        &opts,
        true,
        false,
    );
    assert_parity_batches(
        &snap,
        "SELECT ?s ?c ?len { SERVICE path:search { [] path:source ?s ; path:target ex:d3 ; path:predicate ex:e ; path:weight ex:w ; path:minLength 0 ; path:maxLength 0 ; path:cost ?c ; path:length ?len } }",
        &opts,
        true,
        false,
    );
    assert_parity_batches(
        &snap,
        "SELECT ?c ?len { SERVICE path:search { [] path:source ex:d0 ; path:target ex:d0 ; path:predicate ex:e ; path:weight ex:w ; path:minLength 0 ; path:maxLength 0 ; path:cost ?c ; path:length ?len } }",
        &opts,
        true,
        false,
    );
    assert_parity_batches(
        &snap,
        "SELECT ?t ?c ?len { SERVICE path:search { [] path:source ex:d0 ; path:target ?t ; path:predicate ex:e ; path:weight ex:w ; path:maxLength 1 ; path:cost ?c ; path:length ?len } }",
        &opts,
        true,
        false,
    );
    assert_parity_batches(
        &snap,
        "SELECT ?s ?c ?len { SERVICE path:search { [] path:source ?s ; path:target ex:d3 ; path:predicate ex:e ; path:weight ex:w ; path:maxLength 4 ; path:cost ?c ; path:length ?len } }",
        &opts,
        true,
        false,
    );
    assert_parity_batches(
        &snap,
        "SELECT ?s ?t ?c ?len { VALUES (?s ?t) { (ex:d0 ex:d2) (ex:d0 UNDEF) (ex:d1 ex:d3) } SERVICE path:search { [] path:source ?s ; path:target ?t ; path:predicate ex:e ; path:weight ex:w ; path:maxLength 3 ; path:cost ?c ; path:length ?len } }",
        &opts,
        true,
        true,
    );
    assert_parity_batches(
        &snap,
        "SELECT ?p ?i ?c { SERVICE path:search { [] path:source ex:d0 ; path:target ex:d3 ; path:predicate ex:e ; path:algorithm path:kShortest ; path:k 4 ; path:weight ex:w ; path:maxLength 6 ; path:pathIndex ?p ; path:edgeIndex ?i ; path:cost ?c } }",
        &opts,
        true,
        false,
    );
    assert_parity_batches(
        &snap,
        "SELECT ?p ?i ?c { SERVICE path:search { [] path:source ex:d0 ; path:target ex:d3 ; path:predicate ex:e ; path:algorithm path:allShortest ; path:weight ex:w ; path:maxLength 6 ; path:pathIndex ?p ; path:edgeIndex ?i ; path:cost ?c } }",
        &opts,
        true,
        false,
    );
    // Weighted allShortest cycle (s == t)
    assert_parity_batches(
        &snap,
        "SELECT ?p ?i ?c { SERVICE path:search { [] path:source ex:d0 ; path:target ex:d0 ; path:predicate ex:e ; path:algorithm path:allShortest ; path:weight ex:w ; path:pathIndex ?p ; path:edgeIndex ?i ; path:cost ?c } }",
        &opts,
        true,
        false,
    );
    // Shortest to variable target (by_level without edge vars, per-edge, weighted, minLength 0)
    assert_parity_batches(
        &snap,
        "SELECT ?t ?len { SERVICE path:search { [] path:source ex:d0 ; path:target ?t ; path:predicate ex:e ; path:length ?len } }",
        &opts,
        true,
        false,
    );
    assert_parity_batches(
        &snap,
        "SELECT ?t ?p ?i ?es ?eo ?len { SERVICE path:search { [] path:source ex:d0 ; path:target ?t ; path:predicate ex:e ; path:pathIndex ?p ; path:edgeIndex ?i ; path:edgeSubject ?es ; path:edgeObject ?eo ; path:length ?len } }",
        &opts,
        true,
        false,
    );
    assert_parity_batches(
        &snap,
        "SELECT ?t ?len { SERVICE path:search { [] path:source ex:d0 ; path:target ?t ; path:predicate ex:e ; path:minLength 0 ; path:length ?len } }",
        &opts,
        true,
        false,
    );
    assert_parity_batches(
        &snap,
        "SELECT ?t ?c ?len { SERVICE path:search { [] path:source ex:d0 ; path:target ?t ; path:predicate ex:e ; path:weight ex:w ; path:cost ?c ; path:length ?len } }",
        &opts,
        true,
        false,
    );
    // Shortest from variable source (backward BFS and backward Dijkstra)
    assert_parity_batches(
        &snap,
        "SELECT ?s ?len { SERVICE path:search { [] path:source ?s ; path:target ex:d3 ; path:predicate ex:e ; path:length ?len } }",
        &opts,
        true,
        false,
    );
    assert_parity_batches(
        &snap,
        "SELECT ?s ?p ?i ?c { SERVICE path:search { [] path:source ?s ; path:target ex:d3 ; path:predicate ex:e ; path:weight ex:w ; path:pathIndex ?p ; path:edgeIndex ?i ; path:cost ?c } }",
        &opts,
        true,
        false,
    );
    // allShortest to variable target and from variable source (unweighted + weighted)
    assert_parity_batches(
        &snap,
        "SELECT ?t ?p ?i ?es ?eo { SERVICE path:search { [] path:source ex:d0 ; path:target ?t ; path:predicate ex:e ; path:algorithm path:allShortest ; path:pathIndex ?p ; path:edgeIndex ?i ; path:edgeSubject ?es ; path:edgeObject ?eo } }",
        &opts,
        true,
        false,
    );
    assert_parity_batches(
        &snap,
        "SELECT ?s ?p ?c { SERVICE path:search { [] path:source ?s ; path:target ex:d3 ; path:predicate ex:e ; path:algorithm path:allShortest ; path:weight ex:w ; path:pathIndex ?p ; path:cost ?c } }",
        &opts,
        true,
        false,
    );
    // maxDegree + direction both/backward with allShortest
    assert_parity_batches(
        &snap,
        "SELECT ?p ?i ?es ?eo { SERVICE path:search { [] path:source ex:h0 ; path:target ex:h1 ; path:predicate ex:e ; path:algorithm path:allShortest ; path:maxDegree 2 ; path:pathIndex ?p ; path:edgeIndex ?i ; path:edgeSubject ?es ; path:edgeObject ?eo } }",
        &opts,
        true,
        false,
    );
    assert_parity_batches(
        &snap,
        "SELECT ?t ?len { SERVICE path:search { [] path:source ex:d3 ; path:target ?t ; path:predicate ex:e ; path:direction path:backward ; path:length ?len } }",
        &opts,
        true,
        false,
    );

    // Early stop via LIMIT on a 20-diamond chain (1,048,576 tied shortest paths!) must
    // stream only the first few paths without materializing all 2^20 paths.
    let diamonds = Store::in_memory(StoreOptions::default());
    let mut ddata = String::from("@prefix ex: <http://example.org/> .\n");
    for i in 0..20 {
        ddata.push_str(&format!(
            "ex:x{i} ex:e ex:xa{i} . ex:x{i} ex:e ex:xb{i} .\n\
             ex:xa{i} ex:e ex:x{} . ex:xb{i} ex:e ex:x{} .\n",
            i + 1,
            i + 1
        ));
    }
    load(&diamonds, &ddata, RdfFormat::Turtle);
    let q_lim = format!(
        "{P}SELECT ?p ?len {{ SERVICE path:search {{ [] path:source ex:x0 ; path:target ex:x20 ; path:predicate ex:e ; path:algorithm path:allShortest ; path:pathIndex ?p ; path:length ?len }} }} LIMIT 5 OFFSET 2"
    );
    let (rows, plan, _) = drain_strict(&diamonds.snapshot(), &q_lim, &opts, 2);
    assert_eq!(rows.len(), 5);
    assert!(!plan.has_materialization());
}

/// Item 1123: path searches over their group input and/or nested edge pattern without
/// `path:limit` run natively under strict cursors and match eager answers across every
/// batch size.
#[test]
fn path_search_item_1123_group_input_and_edge_pattern_parity() {
    let store = graph_store();
    let snap = store.snapshot();
    let opts = QueryOptions::default();

    // Group input without limit: shortest, allShortest, kShortest, all
    assert_parity_batches(
        &snap,
        "SELECT ?s ?len { ?s <urn:p> ?o FILTER(?o < 5) SERVICE path:search { [] path:source ?s ; path:target <urn:s:9> ; path:predicate <urn:q> ; path:length ?len } }",
        &opts,
        true,
        true,
    );
    assert_parity_batches(
        &snap,
        "SELECT ?s ?p ?i ?es ?eo { VALUES ?s { <urn:s:8> <urn:s:1> <urn:s:8> } SERVICE path:search { [] path:source ?s ; path:target <urn:s:12> ; path:predicate <urn:q> ; path:algorithm path:allShortest ; path:pathIndex ?p ; path:edgeIndex ?i ; path:edgeSubject ?es ; path:edgeObject ?eo } }",
        &opts,
        true,
        true,
    );
    assert_parity_batches(
        &snap,
        "SELECT ?s ?t ?p ?len { VALUES (?s ?t) { (<urn:s:0> <urn:s:5>) (<urn:s:0> <urn:s:7>) (<urn:s:2> <urn:s:9>) } SERVICE path:search { [] path:source ?s ; path:target ?t ; path:predicate <urn:q> ; path:algorithm path:kShortest ; path:k 2 ; path:pathIndex ?p ; path:length ?len } }",
        &opts,
        true,
        true,
    );
    assert_parity_batches(
        &snap,
        "SELECT ?s ?t ?p ?len { ?s <urn:p> ?o FILTER(?o < 3) SERVICE path:search { [] path:source ?s ; path:target ?t ; path:predicate <urn:q> ; path:algorithm path:all ; path:maxLength 2 ; path:pathIndex ?p ; path:length ?len } }",
        &opts,
        true,
        true,
    );
    // Group input binding target only (backward search)
    assert_parity_batches(
        &snap,
        "SELECT ?s ?t ?len { ?t <urn:p> ?o FILTER(?o < 3) SERVICE path:search { [] path:source ?s ; path:target ?t ; path:predicate <urn:q> ; path:length ?len } }",
        &opts,
        true,
        true,
    );
    // Mixed (Some(s), Some(t)) and (Some(s), None) rows from VALUES
    assert_parity_batches(
        &snap,
        "SELECT ?s ?t ?p ?len { VALUES (?s ?t) { (<urn:s:0> <urn:s:4>) (<urn:s:0> UNDEF) (<urn:s:1> <urn:s:6>) } SERVICE path:search { [] path:source ?s ; path:target ?t ; path:predicate <urn:q> ; path:pathIndex ?p ; path:length ?len } }",
        &opts,
        true,
        true,
    );
    // Empty group input
    assert_parity_batches(
        &snap,
        "SELECT ?s ?len { ?s <urn:p> ?o FILTER(?o > 9999) SERVICE path:search { [] path:source ?s ; path:target <urn:s:9> ; path:predicate <urn:q> ; path:length ?len } }",
        &opts,
        false,
        true,
    );
    // Input row binding an output variable (`?len`) filters incompatible paths
    assert_parity_batches(
        &snap,
        "SELECT ?s ?t ?len { VALUES (?s ?len) { (<urn:s:0> 2) (<urn:s:1> 1) } SERVICE path:search { [] path:source ?s ; path:target ?t ; path:predicate <urn:q> ; path:length ?len } }",
        &opts,
        true,
        true,
    );
    // Nested edge pattern without limit (all four algorithms + direction both + FILTER)
    assert_parity_batches(
        &snap,
        "SELECT ?i ?es ?eo ?len { SERVICE path:search { [] path:source <urn:s:1> ; path:target <urn:s:9> ; path:edgeIndex ?i ; path:edgeSubject ?es ; path:edgeObject ?eo ; path:length ?len ; path:start ?x ; path:end ?y . ?x <urn:q> ?y } }",
        &opts,
        true,
        true,
    );
    assert_parity_batches(
        &snap,
        "SELECT ?p ?i ?len { SERVICE path:search { [] path:source <urn:s:1> ; path:target <urn:s:9> ; path:algorithm path:allShortest ; path:pathIndex ?p ; path:edgeIndex ?i ; path:length ?len ; path:start ?x ; path:end ?y . ?x <urn:q> ?y } }",
        &opts,
        true,
        true,
    );
    assert_parity_batches(
        &snap,
        "SELECT ?t ?p ?len { SERVICE path:search { [] path:source <urn:s:1> ; path:target ?t ; path:algorithm path:all ; path:maxLength 2 ; path:direction path:both ; path:pathIndex ?p ; path:length ?len ; path:start ?x ; path:end ?y . ?x <urn:q> ?y . ?x <urn:p> ?v FILTER(?v < 15) } }",
        &opts,
        true,
        true,
    );
    // Both group input AND nested edge pattern without limit
    assert_parity_batches(
        &snap,
        "SELECT ?s ?p ?i ?len { ?s <urn:p> ?o FILTER(?o < 4) SERVICE path:search { [] path:source ?s ; path:target <urn:s:9> ; path:algorithm path:allShortest ; path:pathIndex ?p ; path:edgeIndex ?i ; path:length ?len ; path:start ?x ; path:end ?y . ?x <urn:q> ?y } }",
        &opts,
        true,
        true,
    );
}

/// Items 1122 & 1123: tight memory budgets, cancellation, deadlines, and error parity.
#[test]
fn path_search_1122_1123_budgets_cancellation_and_errors() {
    let store = graph_store();
    let snap = store.snapshot();
    let strict = CursorOptions {
        batch_rows: 2,
        fallback: FallbackPolicy::RejectMaterialization,
        ..Default::default()
    };
    let queries = [
        format!(
            "{P}SELECT ?p ?i {{ SERVICE path:search {{ [] path:source <urn:s:0> ; path:target <urn:s:9> ; path:predicate <urn:q> ; path:algorithm path:allShortest ; path:pathIndex ?p ; path:edgeIndex ?i }} }}"
        ),
        format!(
            "{P}SELECT ?t ?len {{ SERVICE path:search {{ [] path:source <urn:s:0> ; path:target ?t ; path:predicate <urn:q> ; path:length ?len }} }}"
        ),
        format!(
            "{P}SELECT ?s ?len {{ ?s <urn:p> ?o SERVICE path:search {{ [] path:source ?s ; path:target <urn:s:9> ; path:predicate <urn:q> ; path:length ?len }} }}"
        ),
        format!(
            "{P}SELECT ?t ?len {{ SERVICE path:search {{ [] path:source <urn:s:0> ; path:target ?t ; path:length ?len ; path:start ?x ; path:end ?y . ?x <urn:q> ?y }} }}"
        ),
    ];
    for q in &queries {
        let cancel_opts = QueryOptions {
            no_cache: true,
            cancel: Some(Arc::new(AtomicBool::new(true))),
            ..Default::default()
        };
        let res = select_cursor(snap.clone(), q, &cancel_opts, &strict)
            .and_then(|mut c| c.next_batch().map(|_| ()));
        assert!(matches!(res, Err(Error::Cancelled)), "`{q}`: {res:?}");

        let timeout_opts = QueryOptions {
            no_cache: true,
            deadline: Some(Instant::now() - Duration::from_secs(1)),
            ..Default::default()
        };
        let res = select_cursor(snap.clone(), q, &timeout_opts, &strict)
            .and_then(|mut c| c.next_batch().map(|_| ()));
        assert!(matches!(res, Err(Error::Timeout)), "`{q}`: {res:?}");

        let mem_opts = QueryOptions {
            no_cache: true,
            max_memory_bytes: Some(256),
            ..Default::default()
        };
        let res = select_cursor(snap.clone(), q, &mem_opts, &strict).and_then(|mut c| {
            while c.next_batch()?.is_some() {}
            Ok(())
        });
        assert!(
            matches!(res, Err(Error::BudgetExceeded(b)) if b.kind == BudgetKind::Memory),
            "`{q}`: {res:?}"
        );
    }

    // Error parity on invalid runtime bindings: (UNDEF, UNDEF) row, kShortest unbound target/source.
    for bad_q in [
        format!(
            "{P}SELECT ?s ?t {{ VALUES (?s ?t) {{ (UNDEF UNDEF) }} SERVICE path:search {{ [] path:source ?s ; path:target ?t ; path:predicate <urn:q> }} }}"
        ),
        format!(
            "{P}SELECT ?t {{ SERVICE path:search {{ [] path:source <urn:s:0> ; path:target ?t ; path:predicate <urn:q> ; path:algorithm path:kShortest ; path:k 2 }} }}"
        ),
        format!(
            "{P}SELECT ?s {{ SERVICE path:search {{ [] path:source ?s ; path:target <urn:s:0> ; path:predicate <urn:q> ; path:algorithm path:kShortest ; path:k 2 }} }}"
        ),
    ] {
        let opts = QueryOptions {
            no_cache: true,
            ..Default::default()
        };
        let eager_err = query(snap.clone(), &bad_q, &opts)
            .err()
            .expect("expected eager error");
        let stream_err = select_cursor(snap.clone(), &bad_q, &opts, &strict)
            .and_then(|mut c| {
                while c.next_batch()?.is_some() {}
                Ok(())
            })
            .unwrap_err();
        assert_eq!(
            format!("{stream_err}"),
            format!("{eager_err}"),
            "error mismatch for `{bad_q}`"
        );
    }
}

/// Round-1 parity regressions: weighted `allShortest` + `path:maxLength` + `path:maxVisited`
/// on the three-edge graph (`s -> t`, `s -> a`, `a -> t`), plus grouped multiset / `pathIndex`
/// / `ORDER BY` parity, mid-pull cancellation, pre-admission refusal and memory release.
#[test]
fn path_search_r1_parity_cancellation_and_release() {
    // 1. Weighted `allShortest` with `path:maxLength 1` on `s -> t`, `s -> a`, `a -> t`.
    let three = Store::in_memory(StoreOptions::default());
    load(
        &three,
        "@prefix ex: <http://example.org/> .\n\
         ex:s ex:p ex:t .\n\
         ex:s ex:p ex:a .\n\
         ex:a ex:p ex:t .\n",
        RdfFormat::Turtle,
    );
    let snap3 = three.snapshot();
    let opts = QueryOptions::default();
    assert_parity_batches(
        &snap3,
        "SELECT ?p ?i ?es ?eo ?c { SERVICE path:search { [] path:source ex:s ; path:target ex:t ; path:predicate ex:p ; path:algorithm path:allShortest ; path:weight ex:w ; path:maxLength 1 ; path:pathIndex ?p ; path:edgeIndex ?i ; path:edgeSubject ?es ; path:edgeObject ?eo ; path:cost ?c } }",
        &opts,
        true,
        false,
    );
    // `maxVisited 2`: bounded `maxLength 1` visits only `s` in each of the two spurs,
    // so both eager and streaming succeed with identical `visited` counters.
    assert_parity_batches(
        &snap3,
        "SELECT ?p ?c { SERVICE path:search { [] path:source ex:s ; path:target ex:t ; path:predicate ex:p ; path:algorithm path:allShortest ; path:weight ex:w ; path:maxLength 1 ; path:maxVisited 2 ; path:pathIndex ?p ; path:cost ?c } }",
        &opts,
        true,
        false,
    );
    // `maxVisited 1`: both eager and streaming fail with the exact same `path:maxVisited`
    // error on full drain.
    let q_mv1 = format!(
        "{P}SELECT ?p ?c {{ SERVICE path:search {{ [] path:source ex:s ; path:target ex:t ; path:predicate ex:p ; path:algorithm path:allShortest ; path:weight ex:w ; path:maxLength 1 ; path:maxVisited 1 ; path:pathIndex ?p ; path:cost ?c }} }}"
    );
    let strict = CursorOptions {
        batch_rows: 1,
        fallback: FallbackPolicy::RejectMaterialization,
        ..Default::default()
    };
    let eager_mv1 = query(snap3.clone(), &q_mv1, &opts)
        .err()
        .expect("expected eager maxVisited error");
    let stream_mv1 = select_cursor(snap3.clone(), &q_mv1, &opts, &strict)
        .and_then(|mut c| {
            while c.next_batch()?.is_some() {}
            Ok(())
        })
        .unwrap_err();
    assert_eq!(format!("{stream_mv1}"), format!("{eager_mv1}"));

    // 2. Grouped output with two VALUES rows binding the same source and two tied shortest
    //    paths: full multiset + `pathIndex` parity and exact ordered parity under ORDER BY.
    let diamond = Store::in_memory(StoreOptions::default());
    load(
        &diamond,
        "@prefix ex: <http://example.org/> .\n\
         ex:s ex:p ex:a . ex:a ex:p ex:t .\n\
         ex:s ex:p ex:b . ex:b ex:p ex:t .\n",
        RdfFormat::Turtle,
    );
    let dsnap = diamond.snapshot();
    assert_parity_batches(
        &dsnap,
        "SELECT ?tag ?p ?i ?es ?eo { VALUES (?tag ?s) { (\"first\" ex:s) (\"second\" ex:s) } SERVICE path:search { [] path:source ?s ; path:target ex:t ; path:predicate ex:p ; path:algorithm path:allShortest ; path:pathIndex ?p ; path:edgeIndex ?i ; path:edgeSubject ?es ; path:edgeObject ?eo } }",
        &opts,
        true,
        true,
    );
    let q_ord = format!(
        "{P}SELECT ?tag ?p ?i ?es ?eo {{ VALUES (?tag ?s) {{ (\"first\" ex:s) (\"second\" ex:s) }} SERVICE path:search {{ [] path:source ?s ; path:target ex:t ; path:predicate ex:p ; path:algorithm path:allShortest ; path:pathIndex ?p ; path:edgeIndex ?i ; path:edgeSubject ?es ; path:edgeObject ?eo }} }} ORDER BY ?tag ?p ?i"
    );
    let eager_ord: Vec<_> = query(dsnap.clone(), &q_ord, &opts)
        .unwrap()
        .rows()
        .unwrap()
        .iter()
        .map(|r| r.values().to_vec())
        .collect();
    let mut c_ord = select_cursor(dsnap.clone(), &q_ord, &opts, &strict).unwrap();
    let mut stream_ord = Vec::new();
    while let Some(b) = c_ord.next_batch().unwrap() {
        for i in 0..b.len() {
            stream_ord.push(b.row(i).unwrap().values().to_vec());
        }
    }
    assert_eq!(stream_ord, eager_ord);

    // 3. Mid-pull cancellation and memory release after completion, early close, and error.
    let store = graph_store();
    let snap = store.snapshot();
    let q_multi = format!(
        "{P}SELECT ?t ?p ?i {{ SERVICE path:search {{ [] path:source <urn:s:0> ; path:target ?t ; path:predicate <urn:q> ; path:algorithm path:all ; path:maxLength 4 ; path:pathIndex ?p ; path:edgeIndex ?i }} }}"
    );
    // 3a. Cancel between pulls after first batch succeeds.
    let cancel = Arc::new(AtomicBool::new(false));
    let cancel_opts = QueryOptions {
        no_cache: true,
        cancel: Some(cancel.clone()),
        ..Default::default()
    };
    let mut c = select_cursor(snap.clone(), &q_multi, &cancel_opts, &strict).unwrap();
    let meter = c.memory_meter();
    assert!(c.next_batch().unwrap().is_some());
    cancel.store(true, Ordering::Relaxed);
    assert!(matches!(c.next_batch(), Err(Error::Cancelled)));
    drop(c);
    assert_eq!(meter.live_bytes(), 0);

    // 3a-ii. True within-pull cancellation gated deterministically at a PathSearch
    // traversal checkpoint (no VALUES group input, constant endpoints, 0 matching paths on
    // K_12 with minLength=maxLength=12 so an uninterrupted traversal would run millions of
    // DFS steps). Cancellation stays false until traversal poll #2, proving PathSearch
    // itself observes cancellation and aborts mid-traversal rather than completing and
    // failing at `QueryCursor::pull`'s post-operator check.
    let dense = Store::in_memory(StoreOptions::default());
    let mut dense_data = String::new();
    for i in 0..12 {
        for j in 0..12 {
            if i != j {
                dense_data.push_str(&format!("<urn:k:{i}> <urn:e> <urn:k:{j}> .\n"));
            }
        }
    }
    load(&dense, &dense_data, RdfFormat::Turtle);
    let q_in_pull = format!(
        "{P}SELECT ?l {{ SERVICE path:search {{ [] path:source <urn:k:0> ; path:target <urn:k:11> ; path:predicate <urn:e> ; path:algorithm path:all ; path:minLength 12 ; path:maxLength 12 ; path:maxVisited 50000000 ; path:length ?l }} }}"
    );
    let in_pull_cancel = Arc::new(AtomicBool::new(false));
    let in_pull_opts = QueryOptions {
        no_cache: true,
        cancel: Some(in_pull_cancel.clone()),
        ..Default::default()
    };
    let mut c_in_pull =
        select_cursor(dense.snapshot(), &q_in_pull, &in_pull_opts, &strict).unwrap();
    let meter_in_pull = c_in_pull.memory_meter();
    let traversal_polls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let polls_for_hook = traversal_polls.clone();
    let cancel_for_hook = in_pull_cancel.clone();
    let _hook = sparkles_core::sparql::pathsearch::test_hooks::on_traversal_poll(move || {
        let count = polls_for_hook.fetch_add(1, Ordering::Relaxed) + 1;
        if count == 2 {
            cancel_for_hook.store(true, Ordering::Release);
        }
    });
    let res = c_in_pull.next_batch().map(|b| b.is_some());
    drop(_hook);
    assert!(
        matches!(res, Err(Error::Cancelled)),
        "expected mid-traversal cancellation inside next_batch(): {res:?}"
    );
    let observed_polls = traversal_polls.load(Ordering::Relaxed);
    assert_eq!(
        observed_polls, 2,
        "PathSearch must abort immediately on traversal poll #2 instead of completing (saw {observed_polls} polls)"
    );
    fn find_ps_node(
        p: &sparkles_core::sparql::CursorPlan,
    ) -> Option<&sparkles_core::sparql::CursorPlan> {
        if p.operator.operator == "PathSearch" {
            return Some(p);
        }
        p.children.iter().find_map(find_ps_node)
    }
    let ps_node = find_ps_node(c_in_pull.plan()).expect("PathSearch node in cursor plan");
    assert!(
        !ps_node.complete,
        "cancelled PathSearch must remain incomplete in cursor plan"
    );
    let ps_visited = ps_node
        .operator
        .counters
        .as_ref()
        .and_then(|c| c.get("visited"))
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);
    assert!(
        ps_visited > 0 && ps_visited < 50_000_000,
        "expected partial mid-traversal visited count in cursor plan, got {ps_visited}"
    );
    drop(c_in_pull);
    assert_eq!(meter_in_pull.live_bytes(), 0);

    // 3b. Release on early drop after one batch.
    let mut c = select_cursor(snap.clone(), &q_multi, &opts, &strict).unwrap();
    let meter = c.memory_meter();
    assert!(c.next_batch().unwrap().is_some());
    drop(c);
    assert_eq!(meter.live_bytes(), 0);

    // 3c. Release on full drain.
    let q_short = format!(
        "{P}SELECT ?t ?l {{ SERVICE path:search {{ [] path:source <urn:s:0> ; path:target ?t ; path:predicate <urn:q> ; path:length ?l }} }}"
    );
    let mut c = select_cursor(snap.clone(), &q_short, &opts, &strict).unwrap();
    let meter = c.memory_meter();
    while c.next_batch().unwrap().is_some() {}
    drop(c);
    assert_eq!(meter.live_bytes(), 0);

    // 3d. Release on runtime error (`maxVisited` exceeded).
    let q_err = format!(
        "{P}SELECT ?t ?l {{ SERVICE path:search {{ [] path:source <urn:s:0> ; path:target ?t ; path:predicate <urn:q> ; path:maxVisited 3 ; path:length ?l }} }}"
    );
    let mut c = select_cursor(snap.clone(), &q_err, &opts, &strict).unwrap();
    let meter = c.memory_meter();
    assert!(c.next_batch().is_err());
    drop(c);
    assert_eq!(meter.live_bytes(), 0);

    // 4. Tight-budget pre-allocation refusal on long predicate list and wide group input.
    let multi_pred = Store::in_memory(StoreOptions::default());
    let mut mp_data = String::new();
    for i in 0..120 {
        mp_data.push_str(&format!("<urn:a> <urn:mp:{i}> <urn:b> .\n"));
    }
    load(&multi_pred, &mp_data, RdfFormat::Turtle);
    let pred_list: String = (0..120)
        .map(|i| format!("<urn:mp:{i}>"))
        .collect::<Vec<_>>()
        .join(" , ");
    let q_long_preds = format!(
        "{P}SELECT ?t ?l {{ SERVICE path:search {{ [] path:source <urn:a> ; path:target ?t ; path:predicate {pred_list} ; path:length ?l }} }}"
    );
    let probe_preds = select_cursor(multi_pred.snapshot(), &q_long_preds, &opts, &strict).unwrap();
    let pred_open_bytes = probe_preds.memory_meter().live_bytes();
    drop(probe_preds);
    let pred_opts = QueryOptions {
        no_cache: true,
        max_memory_bytes: Some(pred_open_bytes + 512),
        ..Default::default()
    };
    let mut c = select_cursor(multi_pred.snapshot(), &q_long_preds, &pred_opts, &strict).unwrap();
    let meter = c.memory_meter();
    let res = c.next_batch().map(|b| b.is_some());
    assert!(
        matches!(res, Err(Error::BudgetExceeded(b)) if b.kind == BudgetKind::Memory),
        "expected pre-allocation refusal on long predicate list: {res:?}"
    );
    drop(c);
    assert_eq!(meter.live_bytes(), 0);

    let wide_vars: String = (0..40)
        .map(|i| format!("?v{i}"))
        .collect::<Vec<_>>()
        .join(" ");
    let wide_vals: String = (0..40)
        .map(|i| format!("{i}"))
        .collect::<Vec<_>>()
        .join(" ");
    let q_wide_group = format!(
        "{P}SELECT ?s ?t ?l {wide_vars} {{ VALUES (?s {wide_vars}) {{ (<urn:s:0> {wide_vals}) }} SERVICE path:search {{ [] path:source ?s ; path:target ?t ; path:predicate <urn:q> ; path:length ?l }} }}"
    );
    let probe_wide = select_cursor(snap.clone(), &q_wide_group, &opts, &strict).unwrap();
    let wide_open_bytes = probe_wide.memory_meter().live_bytes();
    drop(probe_wide);
    let wide_opts = QueryOptions {
        no_cache: true,
        max_memory_bytes: Some(wide_open_bytes + 512),
        ..Default::default()
    };
    let mut c = select_cursor(snap, &q_wide_group, &wide_opts, &strict).unwrap();
    let meter = c.memory_meter();
    let res = c.next_batch().map(|b| b.is_some());
    assert!(
        matches!(res, Err(Error::BudgetExceeded(b)) if b.kind == BudgetKind::Memory),
        "expected pre-allocation refusal on wide group input: {res:?}"
    );
    drop(c);
    assert_eq!(meter.live_bytes(), 0);

    // 5. Bounded weighted cursor (item 275): early close, mid-pull cancellation, and
    //    admission failure while frontier state is live all release charged memory to 0.
    let star = Store::in_memory(StoreOptions::default());
    let mut star_data = String::from("<urn:z> <urn:p> <urn:t> .\n");
    for i in 0..400 {
        star_data.push_str(&format!(
            "<urn:s> <urn:p> <urn:l:{i}> .\n<urn:l:{i}> <urn:p> <urn:m:{i}> .\n"
        ));
    }
    load(&star, &star_data, RdfFormat::Turtle);
    let star_snap = star.snapshot();
    let q_bounded_unbound = format!(
        "{P}SELECT ?t ?c ?l {{ SERVICE path:search {{ [] path:source <urn:s> ; path:target ?t ; path:predicate <urn:p> ; path:weight <urn:w> ; path:maxLength 2 ; path:cost ?c ; path:length ?l }} }}"
    );
    let q_bounded_pair = format!(
        "{P}SELECT ?c ?l {{ SERVICE path:search {{ [] path:source <urn:s> ; path:target <urn:t> ; path:predicate <urn:p> ; path:weight <urn:w> ; path:maxLength 2 ; path:cost ?c ; path:length ?l }} }}"
    );

    // 5a. Early close after one batch releases all bounded-search memory.
    let mut c = select_cursor(star_snap.clone(), &q_bounded_unbound, &opts, &strict).unwrap();
    let meter = c.memory_meter();
    assert!(c.next_batch().unwrap().is_some());
    drop(c);
    assert_eq!(meter.live_bytes(), 0);

    // 5b. Mid-pull cancellation after first batch releases all bounded-search memory.
    let b_cancel = Arc::new(AtomicBool::new(false));
    let b_cancel_opts = QueryOptions {
        no_cache: true,
        cancel: Some(b_cancel.clone()),
        ..Default::default()
    };
    let mut c = select_cursor(
        star_snap.clone(),
        &q_bounded_unbound,
        &b_cancel_opts,
        &strict,
    )
    .unwrap();
    let meter = c.memory_meter();
    assert!(c.next_batch().unwrap().is_some());
    b_cancel.store(true, Ordering::Relaxed);
    assert!(matches!(c.next_batch(), Err(Error::Cancelled)));
    drop(c);
    assert_eq!(meter.live_bytes(), 0);

    // 5c. Admission failure while bounded Pareto frontier state is live (both pair and
    //     one-ended searches) returns Memory BudgetExceeded and releases every charged byte.
    for (label, q) in [
        ("bounded pair", &q_bounded_pair),
        ("bounded unbound", &q_bounded_unbound),
    ] {
        let probe = select_cursor(star_snap.clone(), q, &opts, &strict).unwrap();
        let open_bytes = probe.memory_meter().live_bytes();
        drop(probe);
        let tight = QueryOptions {
            no_cache: true,
            max_memory_bytes: Some(open_bytes + 4096),
            ..Default::default()
        };
        let mut c = select_cursor(star_snap.clone(), q, &tight, &strict).unwrap();
        let meter = c.memory_meter();
        let res = c.next_batch().map(|b| b.is_some());
        assert!(
            matches!(res, Err(Error::BudgetExceeded(b)) if b.kind == BudgetKind::Memory),
            "{label}: expected memory admission failure while frontier is live, got {res:?}"
        );
        drop(c);
        assert_eq!(
            meter.live_bytes(),
            0,
            "{label}: live bytes not released after admission failure"
        );
    }
}
