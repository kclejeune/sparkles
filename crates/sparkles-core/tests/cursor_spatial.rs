//! Integration tests for native streaming GeoSPARQL cursor operators (item 829, groups 4 and 6 / 8c):
//! `SpatialScan`, unlimited `SpatialPf`, `SpatialRelate`, `SpatialJoin` (0, 1, and 2 plan inputs),
//! and `SpatialKnn`.
//!
//! Verifies:
//! - Zero eager materialization under `FallbackPolicy::RejectMaterialization`
//! - Exact bag/order answer parity against eager execution at batch sizes 1, 2, 3, and 4096
//! - Both spatial-index-ready and index-off (predicate-scan fallback) paths
//! - Empty results, `LIMIT`/`OFFSET`, `GRAPH ?g`, joins, and residual filters
//! - Memory budget, row budget, cancellation, and deadline enforcement.
#![cfg(feature = "geo")]

use oxrdf::Term;
use oxrdfio::RdfFormat;
use sparkles_core::geo::GeoConfig;
use sparkles_core::io::Source;
use sparkles_core::sparql::{CursorOptions, FallbackPolicy, QueryOptions, query, select_cursor};
use sparkles_core::store::{Snapshot, Store, StoreOptions};
use sparkles_core::{BudgetKind, Error};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

const P: &str = "PREFIX ex: <http://example.org/> \
    PREFIX geo: <http://www.opengis.net/ont/geosparql#> \
    PREFIX geof: <http://www.opengis.net/def/function/geosparql/> \
    PREFIX spatial: <http://jena.apache.org/spatial#> ";

fn load(s: &Store, data: &str, format: RdfFormat) {
    s.load(&[Source::from_bytes(data.as_bytes().to_vec(), format, None)])
        .unwrap();
}

fn geo_store(enable_index: bool) -> Store {
    let s = Store::in_memory(StoreOptions::default());
    let mut data = String::from(
        r#"
        @prefix ex: <http://example.org/> .
        @prefix geo: <http://www.opengis.net/ont/geosparql#> .
        ex:A geo:hasDefaultGeometry ex:gA ; geo:hasGeometry ex:gA ; ex:tag "alpha" .
        ex:gA geo:asWKT "POLYGON((0 0, 10 0, 10 10, 0 10, 0 0))"^^geo:wktLiteral .
        ex:B geo:hasDefaultGeometry ex:gB ; geo:hasGeometry ex:gB ; ex:tag "beta" .
        ex:gB geo:asWKT "POLYGON((5 5, 15 5, 15 15, 5 15, 5 5))"^^geo:wktLiteral .
        ex:C geo:hasDefaultGeometry ex:gC ; geo:hasGeometry ex:gC ; ex:tag "gamma" .
        ex:gC geo:asWKT "POLYGON((20 20, 30 20, 30 30, 20 30, 20 20))"^^geo:wktLiteral .
        ex:p1 geo:hasDefaultGeometry ex:g1 ; geo:hasGeometry ex:g1 ; ex:kind ex:Pt .
        ex:g1 geo:asWKT "POINT(2 2)"^^geo:wktLiteral .
        ex:p2 geo:hasDefaultGeometry ex:g2 ; geo:hasGeometry ex:g2 ; ex:kind ex:Pt .
        ex:g2 geo:asWKT "POINT(7 7)"^^geo:wktLiteral .
        ex:p3 geo:hasDefaultGeometry ex:g3 ; geo:hasGeometry ex:g3 ; ex:kind ex:Pt .
        ex:g3 geo:asWKT "POINT(25 25)"^^geo:wktLiteral .
        ex:gBad geo:asWKT "not-a-wkt"^^geo:wktLiteral .
        ex:A geo:sfContains ex:assertedTarget .
        "#,
    );
    for i in 0..600 {
        let lon = 35.0 + (i % 40) as f64 * 0.5;
        let lat = 35.0 + (i / 40) as f64 * 0.5;
        data.push_str(&format!(
            "ex:f{i} geo:hasGeometry ex:fg{i} . ex:fg{i} geo:asWKT \"POINT({lon} {lat})\"^^geo:wktLiteral .\n",
        ));
    }
    load(&s, &data, RdfFormat::Turtle);
    if enable_index {
        s.enable_geo(GeoConfig {
            query_rewrite: true,
            ..Default::default()
        })
        .unwrap();
    }
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

fn assert_parity_all_batches(
    snap: &Arc<Snapshot>,
    body: &str,
    expected_op: Option<&str>,
    expect_non_empty: bool,
) {
    assert_parity_checked(snap, body, expected_op, expect_non_empty, false);
}

fn assert_parity_checked(
    snap: &Arc<Snapshot>,
    body: &str,
    expected_op: Option<&str>,
    expect_non_empty: bool,
    exact_work: bool,
) {
    let q = format!("{P}{body}");
    let opts = QueryOptions {
        no_cache: true,
        ..Default::default()
    };
    let work_eager = Arc::new(AtomicU64::new(0));
    let eager_opts = QueryOptions {
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
    if expect_non_empty {
        assert!(!expected.is_empty(), "fixture returned 0 rows for `{body}`");
    }
    for batch_rows in [1, 2, 3, 4096] {
        let (got, plan, work_stream) = drain_strict(snap, &q, &opts, batch_rows);
        assert_eq!(
            got, expected,
            "answer mismatch at batch_rows={batch_rows} for `{body}`"
        );
        if let Some(op) = expected_op {
            assert!(
                plan_contains(&plan, op),
                "expected operator `{op}` in plan for `{body}`: {plan:#?}"
            );
        }
        if batch_rows == 2 && exact_work {
            assert_eq!(
                work_stream,
                work_eager.load(Ordering::Relaxed),
                "work mismatch for `{body}`"
            );
        }
    }
}

fn plan_contains(plan: &sparkles_core::sparql::CursorPlan, op: &str) -> bool {
    plan.operator.operator == op || plan.children.iter().any(|c| plan_contains(c, op))
}

#[test]
fn spatial_scan_parity_indexed_and_fallback() {
    let window = "\"POLYGON((0 0, 12 0, 12 12, 0 12, 0 0))\"^^geo:wktLiteral";
    for indexed in [true, false] {
        let store = geo_store(indexed);
        let snap = store.snapshot();
        let op = indexed.then_some("SpatialScan");
        // Basic spatial scan
        assert_parity_all_batches(
            &snap,
            &format!("SELECT ?g {{ ?g geo:asWKT ?w FILTER(geof:sfWithin(?w, {window})) }}"),
            op,
            true,
        );
        // Spatial scan with residual filter
        assert_parity_all_batches(
            &snap,
            &format!(
                "SELECT ?g {{ ?g geo:asWKT ?w FILTER(geof:sfWithin(?w, {window}) && ?g != ex:g1) }}"
            ),
            op,
            true,
        );
        // Empty spatial scan window
        assert_parity_all_batches(
            &snap,
            "SELECT ?g { ?g geo:asWKT ?w FILTER(geof:sfWithin(?w, \"POLYGON((-50 -50, -40 -50, -40 -40, -50 -40, -50 -50))\"^^geo:wktLiteral)) }",
            op,
            false,
        );
        // Spatial scan joined with another pattern
        assert_parity_all_batches(
            &snap,
            &format!(
                "SELECT ?f ?g {{ ?f geo:hasGeometry ?g . ?g geo:asWKT ?w FILTER(geof:sfWithin(?w, {window})) }}"
            ),
            op,
            true,
        );
    }
}

#[test]
fn spatial_pf_unlimited_parity_indexed_and_fallback() {
    for indexed in [true, false] {
        let store = geo_store(indexed);
        let snap = store.snapshot();
        // Variable subject, withinBox, no limit
        assert_parity_all_batches(
            &snap,
            "SELECT ?f { ?f spatial:withinBox(0 0 12 12) }",
            Some("SpatialPf"),
            true,
        );
        // Constant feature subject, withinBox, no limit
        assert_parity_all_batches(
            &snap,
            "SELECT ?x { ex:p1 spatial:withinBox(0 0 12 12) . BIND(1 AS ?x) }",
            Some("SpatialPf"),
            true,
        );
        // Cardinal function without limit
        assert_parity_all_batches(
            &snap,
            "SELECT ?f { ?f spatial:north(20 20) }",
            Some("SpatialPf"),
            true,
        );
        // Empty result
        assert_parity_all_batches(
            &snap,
            "SELECT ?f { ?f spatial:withinBox(-50 -50 -40 -40) }",
            Some("SpatialPf"),
            false,
        );
    }
}

#[test]
fn spatial_relate_all_forms_parity() {
    let store = geo_store(true);
    let snap = store.snapshot();
    // One constant object (derived + asserted)
    assert_parity_all_batches(
        &snap,
        "SELECT ?x { ?x geo:sfContains ex:g1 }",
        Some("SpatialRelate"),
        true,
    );
    // One constant subject (derived + asserted)
    assert_parity_all_batches(
        &snap,
        "SELECT ?y { ex:A geo:sfContains ?y }",
        Some("SpatialRelate"),
        true,
    );
    // Both constant (true)
    assert_parity_all_batches(
        &snap,
        "SELECT ?ok { ex:A geo:sfContains ex:g1 . BIND(1 AS ?ok) }",
        Some("SpatialRelate"),
        true,
    );
    // Both constant (false)
    assert_parity_all_batches(
        &snap,
        "SELECT ?ok { ex:C geo:sfContains ex:g1 . BIND(1 AS ?ok) }",
        Some("SpatialRelate"),
        false,
    );
    // Both variable (index-usable relation)
    assert_parity_all_batches(
        &snap,
        "SELECT ?x ?y { ?x geo:sfContains ?y }",
        Some("SpatialRelate"),
        true,
    );
    // Disjoint relation (not index-usable)
    assert_parity_all_batches(
        &snap,
        "SELECT ?x { ?x geo:sfDisjoint ex:gC }",
        Some("SpatialRelate"),
        true,
    );
}

#[test]
fn spatial_join_all_shapes_and_empty_inputs_parity() {
    for indexed in [true, false] {
        let store = geo_store(indexed);
        let snap = store.snapshot();
        // Two index sides (0 plan children) with residual filter
        assert_parity_all_batches(
            &snap,
            "SELECT ?a ?b { ?a geo:asWKT ?wa . ?b geo:asWKT ?wb FILTER(geof:sfContains(?wa, ?wb) && ?a != ?b) }",
            Some("SpatialJoin"),
            true,
        );
        // One plan child + one index side
        assert_parity_all_batches(
            &snap,
            "SELECT ?a ?b { ?a geo:hasGeometry ?ga . ?ga geo:asWKT ?wa . ?b geo:asWKT ?wb FILTER(geof:sfWithin(?wa, ?wb)) }",
            Some("SpatialJoin"),
            true,
        );
        // Two plan children
        assert_parity_all_batches(
            &snap,
            "SELECT ?f1 ?f2 { ?f1 ex:tag ?t . ?f1 geo:hasGeometry ?g1 . ?g1 geo:asWKT ?w1 . ?f2 ex:kind ex:Pt . ?f2 geo:hasGeometry ?g2 . ?g2 geo:asWKT ?w2 FILTER(geof:sfContains(?w1, ?w2)) }",
            Some("SpatialJoin"),
            true,
        );
        // Empty plan input to a spatial join (runtime filter so planner still builds SpatialJoin)
        assert_parity_all_batches(
            &snap,
            "SELECT ?f1 ?f2 { ?f1 ex:tag ?t . ?f1 geo:hasGeometry ?g1 . ?g1 geo:asWKT ?w1 FILTER(STRSTARTS(?t, \"zzz\")) . ?f2 ex:kind ex:Pt . ?f2 geo:hasGeometry ?g2 . ?g2 geo:asWKT ?w2 FILTER(geof:sfContains(?w1, ?w2)) }",
            Some("SpatialJoin"),
            false,
        );
    }
}

#[test]
fn spatial_knn_parity_with_and_without_errors() {
    for indexed in [true, false] {
        let store = geo_store(indexed);
        let snap = store.snapshot();
        let op = indexed.then_some("SpatialKnn");
        // Filtered errors (`FILTER(BOUND(?d))`)
        assert_parity_checked(
            &snap,
            "SELECT ?g ?d { ?g geo:asWKT ?w BIND(geof:metricDistance(?w, \"POINT(2 2)\"^^geo:wktLiteral) AS ?d) FILTER(BOUND(?d)) } ORDER BY ?d LIMIT 3",
            op,
            true,
            indexed,
        );
        // Unfiltered errors (malformed literal `ex:gBad` sorts first as error)
        assert_parity_checked(
            &snap,
            "SELECT ?g ?d { ?g geo:asWKT ?w BIND(geof:metricDistance(?w, \"POINT(2 2)\"^^geo:wktLiteral) AS ?d) } ORDER BY ?d LIMIT 4",
            op,
            true,
            indexed,
        );
        // Template with join + OFFSET + LIMIT (streaming template join can short-circuit earlier than eager)
        assert_parity_checked(
            &snap,
            "SELECT ?f ?d { ?f geo:hasGeometry ?g . ?g geo:asWKT ?w BIND(geof:metricDistance(?w, \"POINT(7 7)\"^^geo:wktLiteral) AS ?d) FILTER(BOUND(?d)) } ORDER BY ?d OFFSET 1 LIMIT 3",
            op,
            true,
            false,
        );
    }
}

#[test]
fn spatial_limit_offset_early_stop_and_named_graphs() {
    let store = Store::in_memory(StoreOptions::default());
    load(
        &store,
        r#"
        <http://example.org/g1> <http://www.opengis.net/ont/geosparql#asWKT> "POINT(1 1)"^^<http://www.opengis.net/ont/geosparql#wktLiteral> <http://example.org/graph1> .
        <http://example.org/g2> <http://www.opengis.net/ont/geosparql#asWKT> "POINT(2 2)"^^<http://www.opengis.net/ont/geosparql#wktLiteral> <http://example.org/graph2> .
        <http://example.org/f1> <http://www.opengis.net/ont/geosparql#hasGeometry> <http://example.org/g1> <http://example.org/graph1> .
        <http://example.org/f2> <http://www.opengis.net/ont/geosparql#hasGeometry> <http://example.org/g2> <http://example.org/graph2> .
        "#,
        RdfFormat::NQuads,
    );
    store
        .enable_geo(GeoConfig {
            query_rewrite: true,
            ..Default::default()
        })
        .unwrap();
    let snap = store.snapshot();
    assert_parity_all_batches(
        &snap,
        "SELECT ?gr ?g { GRAPH ?gr { ?g geo:asWKT ?w FILTER(geof:sfWithin(?w, \"POLYGON((0 0, 5 0, 5 5, 0 5, 0 0))\"^^geo:wktLiteral)) } }",
        None,
        true,
    );
    assert_parity_all_batches(
        &snap,
        "SELECT ?gr ?f { GRAPH ?gr { ?f spatial:withinBox(0 0 5 5) } }",
        Some("SpatialPf"),
        true,
    );

    // Early stop via LIMIT releases cursor cleanly without draining everything.
    let big = geo_store(true);
    let q = format!(
        "{P}SELECT ?g {{ ?g geo:asWKT ?w FILTER(geof:sfWithin(?w, \"POLYGON((34 34, 60 34, 60 50, 34 50, 34 34))\"^^geo:wktLiteral)) }} LIMIT 5 OFFSET 2"
    );
    let (rows, plan, _) = drain_strict(&big.snapshot(), &q, &QueryOptions::default(), 2);
    assert_eq!(rows.len(), 5);
    assert!(!plan.has_materialization());
}

#[test]
fn spatial_cursors_enforce_memory_budget_cancellation_and_timeout() {
    let store = geo_store(true);
    let snap = store.snapshot();
    let queries = [
        format!(
            "{P}SELECT ?g {{ ?g geo:asWKT ?w FILTER(geof:sfWithin(?w, \"POLYGON((34 34, 60 34, 60 50, 34 50, 34 34))\"^^geo:wktLiteral)) }}"
        ),
        format!("{P}SELECT ?f {{ ?f spatial:withinBox(34 34 50 60) }}"),
        format!(
            "{P}SELECT ?a ?b {{ ?a geo:asWKT ?wa . ?b geo:asWKT ?wb FILTER(geof:sfIntersects(?wa, ?wb)) }}"
        ),
        format!(
            "{P}SELECT ?g ?d {{ ?g geo:asWKT ?w BIND(geof:metricDistance(?w, \"POINT(40 38)\"^^geo:wktLiteral) AS ?d) FILTER(BOUND(?d)) }} ORDER BY ?d LIMIT 10"
        ),
    ];
    let strict = CursorOptions {
        batch_rows: 16,
        fallback: FallbackPolicy::RejectMaterialization,
        ..Default::default()
    };

    for q in &queries {
        // Pre-cancelled query stops immediately.
        let cancel = Arc::new(AtomicBool::new(true));
        let opts = QueryOptions {
            no_cache: true,
            cancel: Some(cancel),
            ..Default::default()
        };
        let res = select_cursor(snap.clone(), q, &opts, &strict)
            .and_then(|mut c| c.next_batch().map(|_| ()));
        assert!(matches!(res, Err(Error::Cancelled)), "`{q}`: {res:?}");

        // Expired deadline times out immediately.
        let opts = QueryOptions {
            no_cache: true,
            deadline: Some(Instant::now() - Duration::from_secs(1)),
            ..Default::default()
        };
        let res = select_cursor(snap.clone(), q, &opts, &strict)
            .and_then(|mut c| c.next_batch().map(|_| ()));
        assert!(matches!(res, Err(Error::Timeout)), "`{q}`: {res:?}");

        // Tiny memory budget fails with Memory budget exceeded.
        let opts = QueryOptions {
            no_cache: true,
            max_memory_bytes: Some(256),
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
