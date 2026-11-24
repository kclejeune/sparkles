//! Integration tests for native streaming `SERVICE view:read` cursors over immutable
//! shared view versions (item 1121).

use oxrdf::Term;
use sparkles_core::access::{GraphAccess, Preparation};
use sparkles_core::error::BudgetKind;
use sparkles_core::incr::{OpenHost, Runtime, RuntimeOptions, ViewDef};
use sparkles_core::sparql::update::update;
use sparkles_core::sparql::{
    AggregateErrors, CursorOptions, FallbackPolicy, QueryOptions, query, select_cursor,
    select_source_cursor,
};
use sparkles_core::store::{Store, StoreOptions};
use sparkles_core::task::Control;
use sparkles_core::{Error, Result};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

const P: &str = "PREFIX view: <urn:x-sparkles:view#> PREFIX ex: <http://example.org/> ";

fn upd(s: &Store, sparql: &str) {
    update(s, &format!("{P}{sparql}"), &QueryOptions::default()).unwrap();
}

fn runtime(s: &Store) -> Arc<Runtime> {
    let rt = Runtime::open(None, s.dataset_id(), RuntimeOptions::default());
    rt.set_host(Arc::new(OpenHost));
    rt
}

fn strict(batch_rows: usize) -> CursorOptions {
    CursorOptions {
        batch_rows,
        fallback: FallbackPolicy::RejectMaterialization,
        ..Default::default()
    }
}

fn bag(rows: Vec<Vec<Option<Term>>>) -> Vec<String> {
    let mut out: Vec<String> = rows.into_iter().map(|r| format!("{r:?}")).collect();
    out.sort();
    out
}

fn ordered(rows: Vec<Vec<Option<Term>>>) -> Vec<String> {
    rows.into_iter().map(|r| format!("{r:?}")).collect()
}

fn drain(
    snap: &Arc<sparkles_core::store::Snapshot>,
    q: &str,
    opts: &QueryOptions,
    batch_rows: usize,
) -> Result<(Vec<Vec<Option<Term>>>, bool)> {
    let mut c = select_cursor(snap.clone(), q, opts, &strict(batch_rows))?;
    let has_mat = c.plan().has_materialization();
    let mut out = Vec::new();
    while let Some(b) = c.next_batch()? {
        for i in 0..b.len() {
            out.push(b.row(i)?.values().to_vec());
        }
    }
    Ok((out, has_mat))
}

fn eager_rows(
    snap: &Arc<sparkles_core::store::Snapshot>,
    q: &str,
    opts: &QueryOptions,
) -> Vec<Vec<Option<Term>>> {
    query(snap.clone(), q, opts)
        .unwrap_or_else(|e| panic!("eager failed for {q}: {e}"))
        .rows()
        .unwrap()
        .iter()
        .map(|r| r.values().to_vec())
        .collect()
}

fn assert_parity_all_batches(
    snap: &Arc<sparkles_core::store::Snapshot>,
    q: &str,
    opts: &QueryOptions,
    keep_order: bool,
) {
    let eager = eager_rows(snap, q, opts);
    for batch in [1, 2, 3, 4096] {
        let (streamed, has_mat) = drain(snap, q, opts, batch)
            .unwrap_or_else(|e| panic!("cursor(batch={batch}) failed for {q}: {e}"));
        assert!(!has_mat, "expected native ViewRead plan for {q}");
        if keep_order {
            assert_eq!(
                ordered(streamed),
                ordered(eager.clone()),
                "batch={batch} {q}"
            );
        } else {
            assert_eq!(bag(streamed), bag(eager.clone()), "batch={batch} {q}");
        }
    }
}

#[test]
fn parity_across_batch_sizes_and_query_shapes() {
    let s = Store::in_memory(StoreOptions::default());
    upd(
        &s,
        "INSERT DATA {
            ex:a ex:team ex:t1 ; ex:label \"alpha\" ; ex:age 40 .
            ex:b ex:team ex:t1 ; ex:age 30 .
            ex:c ex:team ex:t2 ; ex:label \"gamma\" ; ex:age 20 .
            ex:d ex:team ex:t3 ; ex:label \"alpha\" ; ex:age 10 .
            ex:t1 ex:rank 1 .
            ex:t2 ex:rank 2 .
        }",
    );
    let rt = runtime(&s);
    // View with GROUP BY + BIND (produces Tag::Local computed strings).
    rt.put(
        &s,
        "team-sizes",
        ViewDef::table(format!(
            "{P}SELECT ?team (COUNT(?p) AS ?n) (CONCAT(\"team-\", STR(?n)) AS ?tag) \
             WHERE {{ ?p ex:team ?team }} GROUP BY ?team"
        )),
    )
    .unwrap();
    // View with OPTIONAL (produces UNDEF cells).
    rt.put(
        &s,
        "people-labels",
        ViewDef::table(format!(
            "{P}SELECT ?p ?label WHERE {{ ?p ex:team ?t OPTIONAL {{ ?p ex:label ?label }} }}"
        )),
    )
    .unwrap();
    // Ordered window view with duplicate projected values.
    rt.put(
        &s,
        "oldest-labels",
        ViewDef::table(format!(
            "{P}SELECT ?label WHERE {{ ?p ex:label ?label ; ex:age ?age }} ORDER BY DESC(?age) LIMIT 3"
        )),
    )
    .unwrap();
    // CONSTRUCT view.
    rt.put(
        &s,
        "teams-graph",
        ViewDef::table(format!(
            "{P}CONSTRUCT {{ ?p ex:inTeam ?t }} WHERE {{ ?p ex:team ?t }}"
        )),
    )
    .unwrap();
    rt.round(&s);

    let snap = s.snapshot();
    let opts = QueryOptions {
        views: Some(rt.clone()),
        no_cache: true,
        ..Default::default()
    };

    for q in [
        format!(
            "{P}SELECT ?team ?n ?tag ?c WHERE {{ SERVICE view:read {{ [] view:name \"team-sizes\" ; view:bind ?team, ?n, ?tag ; view:commit ?c }} }}"
        ),
        format!(
            "{P}SELECT ?p ?label WHERE {{ SERVICE view:read {{ [] view:name \"people-labels\" ; view:bind ?p, ?label }} }}"
        ),
        format!(
            "{P}SELECT ?team ?rank ?n WHERE {{ ?team ex:rank ?rank . SERVICE view:read {{ [] view:name \"team-sizes\" ; view:bind ?team, ?n }} }}"
        ),
        format!(
            "{P}SELECT ?team ?rank WHERE {{ ?team ex:rank ?rank FILTER EXISTS {{ SERVICE view:read {{ [] view:name \"team-sizes\" ; view:bind ?team, ?n }} FILTER(?n > 1) }} }}"
        ),
        format!(
            "{P}SELECT ?team ?rank WHERE {{ ?team ex:rank ?rank FILTER NOT EXISTS {{ SERVICE view:read {{ [] view:name \"team-sizes\" ; view:bind ?team, ?n }} FILTER(?n > 1) }} }}"
        ),
        format!(
            "{P}SELECT ?subject ?predicate ?object WHERE {{ SERVICE view:read {{ [] view:name \"teams-graph\" ; view:bind ?subject, ?predicate, ?object }} }}"
        ),
    ] {
        assert_parity_all_batches(&snap, &q, &opts, false);
    }

    // Ordered window preserves exact row order (including duplicate "alpha" at positions 0 and 2).
    let win_q = format!(
        "{P}SELECT ?label WHERE {{ SERVICE view:read {{ [] view:name \"oldest-labels\" ; view:bind ?label }} }}"
    );
    assert_parity_all_batches(&snap, &win_q, &opts, true);

    // Lazy open: missing view and unknown column fail on first pull, not at cursor open.
    let missing = format!(
        "{P}SELECT ?x WHERE {{ SERVICE view:read {{ [] view:name \"nope\" ; view:bind ?x }} }}"
    );
    let mut c = select_cursor(snap.clone(), &missing, &opts, &strict(2)).unwrap();
    assert_eq!(c.stats().rows_produced, 0);
    assert!(c.next_batch().is_err());

    let bad_col = format!(
        "{P}SELECT ?nope WHERE {{ SERVICE view:read {{ [] view:name \"team-sizes\" ; view:bind ?nope }} }}"
    );
    let mut c = select_cursor(snap.clone(), &bad_col, &opts, &strict(2)).unwrap();
    assert_eq!(c.stats().rows_produced, 0);
    assert!(c.next_batch().is_err());
}

#[test]
fn updates_between_pulls_stay_pinned_to_opened_version_and_do_not_deadlock() {
    let s = Store::in_memory(StoreOptions::default());
    let mut insert = String::from("INSERT DATA { ");
    for i in 0..8 {
        insert.push_str(&format!("ex:s{i} ex:val \"v{i}\" . "));
    }
    insert.push('}');
    upd(&s, &insert);

    let rt = runtime(&s);
    rt.put(
        &s,
        "vals",
        ViewDef::table(format!(
            "{P}SELECT ?s ?v (CONCAT(\"k-\", ?v) AS ?k) WHERE {{ ?s ex:val ?v }}"
        )),
    )
    .unwrap();
    rt.round(&s);

    let snap1 = s.snapshot();
    let commit1 = snap1.commit;
    let opts = QueryOptions {
        views: Some(rt.clone()),
        no_cache: true,
        ..Default::default()
    };

    for allow_stale in [false, true] {
        let stale_clause = if allow_stale {
            "; view:allowStale true"
        } else {
            ""
        };
        let q = format!(
            "{P}SELECT ?s ?v ?k ?c WHERE {{ SERVICE view:read {{ [] view:name \"vals\" ; view:bind ?s, ?v, ?k ; view:commit ?c {stale_clause} }} }}"
        );
        let expected = bag(eager_rows(&snap1, &q, &opts));

        let mut c = select_cursor(snap1.clone(), &q, &opts, &strict(2)).unwrap();
        let mut got = Vec::new();
        // Pull first batch (2 rows).
        let first = c.next_batch().unwrap().unwrap();
        assert_eq!(first.len(), 2);
        for i in 0..first.len() {
            got.push(first.row(i).unwrap().values().to_vec());
        }

        // Mutate the store and run maintenance passes WHILE the cursor is open mid-stream.
        // This must neither deadlock (no circuit lock held across pulls) nor alter the cursor.
        upd(
            &s,
            "DELETE DATA { ex:s0 ex:val \"v0\" . ex:s1 ex:val \"v1\" . ex:s2 ex:val \"v2\" } ; \
             INSERT DATA { ex:s99 ex:val \"v99\" . ex:s100 ex:val \"v100\" }",
        );
        rt.round(&s);
        assert!(s.snapshot().commit > commit1);

        // Drain the rest of the cursor.
        while let Some(b) = c.next_batch().unwrap() {
            for i in 0..b.len() {
                got.push(b.row(i).unwrap().values().to_vec());
            }
        }
        assert_eq!(bag(got.clone()), expected, "allow_stale={allow_stale}");
        // Every row's commit column must stay pinned to commit1 (or the commit opened at).
        let expected_commit = if allow_stale {
            // On the second iteration the view is already ahead when opened with allowStale.
            got[0][3].clone()
        } else {
            Some(Term::Literal(oxrdf::Literal::from(commit1 as i64)))
        };
        for row in &got {
            assert_eq!(row[3], expected_commit);
        }
        // Restore original state before the second iteration.
        upd(
            &s,
            "INSERT DATA { ex:s0 ex:val \"v0\" . ex:s1 ex:val \"v1\" . ex:s2 ex:val \"v2\" } ; \
             DELETE DATA { ex:s99 ex:val \"v99\" . ex:s100 ex:val \"v100\" }",
        );
        rt.round(&s);
    }
}

#[test]
fn stale_reads_behind_and_ahead_with_new_delta_terms() {
    let s = Store::in_memory(StoreOptions::default());
    upd(&s, "INSERT DATA { ex:a ex:p \"one\" . ex:b ex:p \"two\" }");
    let rt = runtime(&s);
    rt.put(
        &s,
        "items",
        ViewDef::table(format!("{P}SELECT ?s ?o WHERE {{ ?s ex:p ?o }}")),
    )
    .unwrap();
    rt.round(&s);
    let snap1 = s.snapshot();
    let commit1 = snap1.commit;

    // Commit a new triple with a brand-new delta term WITHOUT running `rt.round(&s)`.
    upd(&s, "INSERT DATA { ex:c ex:p \"brand-new-delta-term\" }");
    let snap2 = s.snapshot();
    let commit2 = snap2.commit;
    assert!(commit2 > commit1);

    let opts = QueryOptions {
        views: Some(rt.clone()),
        no_cache: true,
        ..Default::default()
    };
    let stale_q = format!(
        "{P}SELECT ?s ?o ?c WHERE {{ SERVICE view:read {{ [] view:name \"items\" ; view:bind ?s, ?o ; view:commit ?c ; view:allowStale true }} }}"
    );

    // 1) View BEHIND snap2: `allowStale true` returns commit1's rows without waiting.
    assert_parity_all_batches(&snap2, &stale_q, &opts, false);
    let (rows, _) = drain(&snap2, &stale_q, &opts, 1).unwrap();
    assert_eq!(rows.len(), 2);
    for row in &rows {
        assert_eq!(
            row[2],
            Some(Term::Literal(oxrdf::Literal::from(commit1 as i64)))
        );
    }

    // 2) Advance the view to commit2, then read at older snap1 with `allowStale true`.
    // The view now holds `ex:c` and `"brand-new-delta-term"` whose Delta ids are >= snap1.dvocab_len!
    rt.round(&s);
    assert_parity_all_batches(&snap1, &stale_q, &opts, false);
    let (rows, _) = drain(&snap1, &stale_q, &opts, 1).unwrap();
    assert_eq!(rows.len(), 3);
    for row in &rows {
        assert_eq!(
            row[2],
            Some(Term::Literal(oxrdf::Literal::from(commit2 as i64)))
        );
    }
    assert!(rows.iter().any(|r| {
        r[1].as_ref()
            .is_some_and(|t| t.to_string().contains("brand-new-delta-term"))
    }));
}

#[test]
fn view_ahead_within_history_skipped_history_and_expired_history_rebuild() {
    let s = Store::in_memory(StoreOptions::default());
    upd(
        &s,
        "INSERT DATA {
            ex:a ex:member ex:t1 .
            ex:b ex:member ex:t1 .
            ex:c ex:member ex:t2 .
            ex:d ex:member ex:t2 .
            ex:e ex:member ex:t3 .
        }",
    );
    let rt = runtime(&s);
    rt.put(
        &s,
        "teams",
        ViewDef::table(format!(
            "{P}SELECT ?t (COUNT(?p) AS ?n) WHERE {{ ?p ex:member ?t }} GROUP BY ?t"
        )),
    )
    .unwrap();
    // Multiplicity view (duplicates of ?t) to exercise multiplicity rewinding.
    rt.put(
        &s,
        "members-bag",
        ViewDef::table(format!("{P}SELECT ?t WHERE {{ ?p ex:member ?t }}")),
    )
    .unwrap();
    // CONSTRUCT view to exercise CONSTRUCT rebuild after expired history.
    rt.put(
        &s,
        "members-graph",
        ViewDef::table(format!(
            "{P}CONSTRUCT {{ ?p ex:in ?t }} WHERE {{ ?p ex:member ?t }}"
        )),
    )
    .unwrap();
    rt.round(&s);

    let snap_base = s.snapshot();
    let opts = QueryOptions {
        views: Some(rt.clone()),
        no_cache: true,
        ..Default::default()
    };

    let q_teams = format!(
        "{P}SELECT ?t ?n ?c WHERE {{ SERVICE view:read {{ [] view:name \"teams\" ; view:bind ?t, ?n ; view:commit ?c }} }}"
    );
    let q_bag = format!(
        "{P}SELECT ?t ?c WHERE {{ SERVICE view:read {{ [] view:name \"members-bag\" ; view:bind ?t ; view:commit ?c }} }}"
    );
    let q_graph = format!(
        "{P}SELECT ?subject ?predicate ?object ?c WHERE {{ SERVICE view:read {{ [] view:name \"members-graph\" ; view:bind ?subject, ?predicate, ?object ; view:commit ?c }} }}"
    );

    // 1) Skipped commit (unrelated predicate): view ahead with empty history diff.
    upd(&s, "INSERT DATA { ex:other ex:unrelated 1 }");
    rt.round(&s);
    assert_parity_all_batches(&snap_base, &q_teams, &opts, false);
    assert_parity_all_batches(&snap_base, &q_bag, &opts, false);

    // 2) Mutating commits within history (additions, deletions, multiplicity up & down).
    upd(
        &s,
        "INSERT DATA { ex:f ex:member ex:t1 . ex:g ex:member ex:t1 . ex:h ex:member ex:t4 } ; \
         DELETE DATA { ex:c ex:member ex:t2 . ex:e ex:member ex:t3 }",
    );
    rt.round(&s);
    assert_parity_all_batches(&snap_base, &q_teams, &opts, false);
    assert_parity_all_batches(&snap_base, &q_bag, &opts, false);
    assert_parity_all_batches(&snap_base, &q_graph, &opts, false);

    // Verify returned commit is snap_base.commit.
    let (rows, _) = drain(&snap_base, &q_teams, &opts, 1).unwrap();
    for row in &rows {
        assert_eq!(
            row[2],
            Some(Term::Literal(oxrdf::Literal::from(snap_base.commit as i64)))
        );
    }

    // 3) Advance 20 commits past VIEW_HISTORY_COMMITS (16) so history expires -> rebuilt reads!
    for i in 0..20 {
        upd(&s, &format!("INSERT DATA {{ ex:n{i} ex:member ex:t5 }}"));
        rt.round(&s);
    }
    assert_parity_all_batches(&snap_base, &q_teams, &opts, false);
    assert_parity_all_batches(&snap_base, &q_bag, &opts, false);
    assert_parity_all_batches(&snap_base, &q_graph, &opts, false);

    let (rows, _) = drain(&snap_base, &q_teams, &opts, 2).unwrap();
    assert_eq!(rows.len(), 3);
    for row in &rows {
        assert_eq!(
            row[2],
            Some(Term::Literal(oxrdf::Literal::from(snap_base.commit as i64)))
        );
    }
}

#[test]
fn multiplicities_spanning_batches_with_slice_and_joins() {
    let s = Store::in_memory(StoreOptions::default());
    let mut data = String::from("INSERT DATA { ");
    for i in 0..10 {
        data.push_str(&format!("ex:a{i} ex:group ex:g1 . "));
    }
    for i in 0..7 {
        data.push_str(&format!("ex:b{i} ex:group ex:g2 . "));
    }
    for i in 0..5 {
        data.push_str(&format!("ex:c{i} ex:group ex:g3 . "));
    }
    data.push_str("ex:g1 ex:weight 100 . ex:g2 ex:weight 200 . ex:g3 ex:weight 300 . }");
    upd(&s, &data);

    let rt = runtime(&s);
    // Multiplicity per group: g1=10, g2=7, g3=5 (22 rows total, 3 distinct view rows).
    rt.put(
        &s,
        "groups-bag",
        ViewDef::table(format!("{P}SELECT ?g WHERE {{ ?p ex:group ?g }}")),
    )
    .unwrap();
    rt.round(&s);

    let snap = s.snapshot();
    let opts = QueryOptions {
        views: Some(rt.clone()),
        no_cache: true,
        ..Default::default()
    };

    for q in [
        format!(
            "{P}SELECT ?g WHERE {{ SERVICE view:read {{ [] view:name \"groups-bag\" ; view:bind ?g }} }}"
        ),
        format!(
            "{P}SELECT ?g WHERE {{ SERVICE view:read {{ [] view:name \"groups-bag\" ; view:bind ?g }} }} ORDER BY ?g LIMIT 8 OFFSET 5"
        ),
        format!(
            "{P}SELECT ?g ?w WHERE {{ SERVICE view:read {{ [] view:name \"groups-bag\" ; view:bind ?g }} ?g ex:weight ?w }}"
        ),
        format!(
            "{P}SELECT ?g ?w WHERE {{ ?g ex:weight ?w OPTIONAL {{ SERVICE view:read {{ [] view:name \"groups-bag\" ; view:bind ?g }} }} }}"
        ),
    ] {
        assert_parity_all_batches(&snap, &q, &opts, q.contains("ORDER BY"));
    }

    // Unordered LIMIT stops early without expanding all 22 rows.
    let lim_q = format!(
        "{P}SELECT ?g WHERE {{ SERVICE view:read {{ [] view:name \"groups-bag\" ; view:bind ?g }} }} LIMIT 4"
    );
    let mut c = select_cursor(snap.clone(), &lim_q, &opts, &strict(2)).unwrap();
    let mut count = 0;
    while let Some(b) = c.next_batch().unwrap() {
        count += b.len();
    }
    assert_eq!(count, 4);
    assert!(
        c.stats().rows_produced < 22,
        "LIMIT 4 must stop early without expanding all multiplicities: {}",
        c.stats().rows_produced
    );
}

#[test]
fn alternate_aggregate_errors_reading_streams_with_warning() {
    let s = Store::in_memory(StoreOptions::default());
    upd(
        &s,
        "INSERT DATA { ex:a ex:g \"x\" ; ex:v 1 . ex:b ex:g \"x\" }",
    );
    let rt = runtime(&s);
    rt.put(
        &s,
        "mins",
        ViewDef::table(format!(
            "{P}SELECT (MIN(?v) AS ?m) WHERE {{ ?s ex:g ?g OPTIONAL {{ ?s ex:v ?v }} }}"
        )),
    )
    .unwrap();
    rt.round(&s);

    let snap = s.snapshot();
    let opts = QueryOptions {
        views: Some(rt.clone()),
        no_cache: true,
        aggregate_errors: Some(AggregateErrors::Sparkles),
        ..Default::default()
    };
    let q = format!(
        "{P}SELECT ?m ?c WHERE {{ SERVICE view:read {{ [] view:name \"mins\" ; view:bind ?m ; view:commit ?c }} }}"
    );
    assert_parity_all_batches(&snap, &q, &opts, false);
    let mut c = select_cursor(snap.clone(), &q, &opts, &strict(1)).unwrap();
    let b = c.next_batch().unwrap().unwrap();
    assert_eq!(b.len(), 1);
    assert_eq!(
        b.row(0).unwrap().values()[0],
        Some(Term::Literal(oxrdf::Literal::from(1)))
    );
    assert!(
        c.plan()
            .operator
            .warnings
            .iter()
            .any(|w| w.code == "view-evaluated")
    );
}

#[test]
fn memory_budgets_for_current_history_and_rebuilt_reads() {
    let s = Store::in_memory(StoreOptions::default());
    let payload = "x".repeat(256);
    let mut data = String::from("INSERT DATA { ");
    for i in 0..400 {
        data.push_str(&format!("ex:s{i} ex:p \"{payload}-{i}\" . "));
    }
    data.push('}');
    upd(&s, &data);

    let rt = runtime(&s);
    // View with computed Tag::Local strings (each ~280 bytes).
    rt.put(
        &s,
        "big",
        ViewDef::table(format!(
            "{P}SELECT ?s (CONCAT(\"v-\", ?o) AS ?x) WHERE {{ ?s ex:p ?o }}"
        )),
    )
    .unwrap();
    // View with store ids only (no Tag::Local terms).
    rt.put(
        &s,
        "store-ids",
        ViewDef::table(format!("{P}SELECT ?s ?o WHERE {{ ?s ex:p ?o }}")),
    )
    .unwrap();
    rt.round(&s);

    let snap_base = s.snapshot();

    // 1) Current read with tiny memory budget refuses cleanly.
    let q_big = format!(
        "{P}SELECT ?s ?x WHERE {{ SERVICE view:read {{ [] view:name \"big\" ; view:bind ?s, ?x }} }}"
    );
    let tight = QueryOptions {
        views: Some(rt.clone()),
        no_cache: true,
        max_memory_bytes: Some(4 << 10),
        ..Default::default()
    };
    let err = drain(&snap_base, &q_big, &tight, 16).unwrap_err();
    assert!(
        matches!(err, Error::BudgetExceeded(ref b) if b.kind == BudgetKind::Memory),
        "expected Memory budget refusal on current read, got: {err:?}"
    );

    // 2) Current read of store-ids streams under a budget smaller than a full 400-row copy!
    let q_ids = format!(
        "{P}SELECT ?s ?o WHERE {{ SERVICE view:read {{ [] view:name \"store-ids\" ; view:bind ?s, ?o }} }}"
    );
    let small_stream = QueryOptions {
        views: Some(rt.clone()),
        no_cache: true,
        max_memory_bytes: Some(32 << 10),
        ..Default::default()
    };
    let (rows, has_mat) = drain(&snap_base, &q_ids, &small_stream, 16).unwrap();
    assert!(!has_mat);
    assert_eq!(rows.len(), 400);

    // 3) History read: mutate 200 rows so the history delta (~80 KiB) exceeds a 16 KiB budget.
    let mut del = String::from("DELETE DATA { ");
    for i in 0..200 {
        del.push_str(&format!("ex:s{i} ex:p \"{payload}-{i}\" . "));
    }
    del.push('}');
    upd(&s, &del);
    rt.round(&s);
    let tight_hist = QueryOptions {
        views: Some(rt.clone()),
        no_cache: true,
        max_memory_bytes: Some(16 << 10),
        ..Default::default()
    };
    let err = drain(&snap_base, &q_ids, &tight_hist, 16).unwrap_err();
    assert!(
        matches!(err, Error::BudgetExceeded(ref b) if b.kind == BudgetKind::Memory),
        "expected Memory budget refusal on history rewind, got: {err:?}"
    );

    // 4) Expire history (>16 commits) and verify rebuilt read refuses under tight memory budget.
    for i in 0..18 {
        upd(
            &s,
            &format!("INSERT DATA {{ ex:extra{i} ex:p \"extra-{i}\" }}"),
        );
        rt.round(&s);
    }
    let tight_rebuild = QueryOptions {
        views: Some(rt.clone()),
        no_cache: true,
        max_memory_bytes: Some(4 << 10),
        ..Default::default()
    };
    let err = drain(&snap_base, &q_ids, &tight_rebuild, 16).unwrap_err();
    assert!(
        matches!(err, Error::BudgetExceeded(ref b) if b.kind == BudgetKind::Memory),
        "expected Memory budget refusal on rebuilt read, got: {err:?}"
    );
}

#[test]
fn cancellation_timeout_and_checked_source_enforcement() {
    let s = Store::in_memory(StoreOptions::default());
    let mut data = String::from("INSERT DATA { ");
    for i in 0..20 {
        data.push_str(&format!("ex:s{i} ex:p {i} . "));
    }
    data.push('}');
    upd(&s, &data);

    let rt = Runtime::open(
        None,
        s.dataset_id(),
        RuntimeOptions {
            view_wait: Duration::from_secs(5),
            ..Default::default()
        },
    );
    rt.set_host(Arc::new(OpenHost));
    rt.put(
        &s,
        "nums",
        ViewDef::table(format!("{P}SELECT ?s ?o WHERE {{ ?s ex:p ?o }}")),
    )
    .unwrap();
    rt.round(&s);

    let snap = s.snapshot();
    let q = format!(
        "{P}SELECT ?s ?o WHERE {{ SERVICE view:read {{ [] view:name \"nums\" ; view:bind ?s, ?o }} }}"
    );

    // 1) Cancellation between pulls stops immediately.
    let cancel = Arc::new(AtomicBool::new(false));
    let opts = QueryOptions {
        views: Some(rt.clone()),
        no_cache: true,
        cancel: Some(cancel.clone()),
        ..Default::default()
    };
    let mut c = select_cursor(snap.clone(), &q, &opts, &strict(2)).unwrap();
    assert_eq!(c.next_batch().unwrap().unwrap().len(), 2);
    cancel.store(true, Ordering::Relaxed);
    assert!(matches!(c.next_batch(), Err(Error::Cancelled)));

    // 2) Deadline expiring between pulls fails with Timeout.
    let deadline = Instant::now() + Duration::from_millis(40);
    let timed_out = QueryOptions {
        views: Some(rt.clone()),
        no_cache: true,
        deadline: Some(deadline),
        ..Default::default()
    };
    let mut c = select_cursor(snap.clone(), &q, &timed_out, &strict(2)).unwrap();
    assert_eq!(c.next_batch().unwrap().unwrap().len(), 2);
    std::thread::sleep(Duration::from_millis(50));
    assert!(matches!(c.next_batch(), Err(Error::Timeout)));

    // 3) Cancellation while waiting for a behind view wakes promptly (well below 5s view_wait).
    upd(&s, "INSERT DATA { ex:s99 ex:p 99 }");
    let snap_ahead = s.snapshot();
    let wait_cancel = Arc::new(AtomicBool::new(false));
    let waiter = wait_cancel.clone();
    let t = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(60));
        waiter.store(true, Ordering::Relaxed);
    });
    let t0 = Instant::now();
    let opts_wait = QueryOptions {
        views: Some(rt.clone()),
        no_cache: true,
        cancel: Some(wait_cancel),
        ..Default::default()
    };
    let mut c = select_cursor(snap_ahead, &q, &opts_wait, &strict(2)).unwrap();
    assert!(matches!(c.next_batch(), Err(Error::Cancelled)));
    assert!(
        t0.elapsed() < Duration::from_secs(1),
        "cancellation while waiting for behind view took too long: {:?}",
        t0.elapsed()
    );
    t.join().unwrap();

    // 4) Checked BoundReadSource respects source cancellation.
    rt.round(&s);
    let src_cancel = Arc::new(AtomicBool::new(false));
    let prep = Preparation::new(Control::with_cancel(src_cancel.clone()), 64 << 20).unwrap();
    let policy = Arc::new(GraphAccess::all());
    let view = s.prepare_read(policy.clone(), &prep).unwrap();
    let mut c = select_source_cursor(
        view.read_source(),
        &s,
        &q,
        &QueryOptions {
            graphs: Some(policy),
            views: Some(rt.clone()),
            no_cache: true,
            ..Default::default()
        },
        &strict(2),
    )
    .unwrap();
    assert_eq!(c.next_batch().unwrap().unwrap().len(), 2);
    src_cancel.store(true, Ordering::Relaxed);
    assert!(matches!(c.next_batch(), Err(Error::Cancelled)));
}

#[test]
fn reduced_rebuild_preserves_multiplicity_and_commit() {
    let s = Store::in_memory(StoreOptions::default());
    upd(&s, "INSERT DATA { ex:a ex:p 1 . ex:b ex:p 1 }");
    let rt = runtime(&s);
    rt.put(
        &s,
        "reduced",
        ViewDef::table(format!("{P}SELECT REDUCED ?x {{ ?s ex:p ?x }}")),
    )
    .unwrap();
    rt.put(
        &s,
        "reduced-limit",
        ViewDef::table(format!("{P}SELECT REDUCED ?x {{ ?s ex:p ?x }} LIMIT 2")),
    )
    .unwrap();
    rt.put(
        &s,
        "reduced-mixed",
        ViewDef::table(format!("{P}SELECT ?x {{ {{ SELECT REDUCED ?x {{ ?s ex:p ?x }} }} UNION {{ SELECT REDUCED ?x {{ ?s ex:p ?x }} LIMIT 2 }} }}")),
    )
    .unwrap();
    rt.round(&s);
    let snap = s.snapshot();
    let opts = QueryOptions {
        views: Some(rt.clone()),
        ..Default::default()
    };
    let q = format!(
        "{P}SELECT ?x ?c {{ SERVICE view:read {{ [] view:name \"reduced\" ; view:bind ?x ; view:commit ?c }} }}"
    );
    let expected = drain(&snap, &q, &opts, 1).unwrap().0;
    assert_eq!(expected.len(), 1);
    let alternate = QueryOptions {
        aggregate_errors: Some(AggregateErrors::Sparkles),
        ..opts.clone()
    };
    assert_eq!(drain(&snap, &q, &alternate, 1).unwrap().0, expected);
    let limited = q.replace("\"reduced\"", "\"reduced-limit\"");
    let before = drain(&snap, &limited, &opts, 1).unwrap().0;
    assert_eq!(before.len(), 2, "the maintained recompute keeps both rows");
    assert_eq!(drain(&snap, &limited, &alternate, 1).unwrap().0, before);
    let mixed = q.replace("\"reduced\"", "\"reduced-mixed\"");
    let before = drain(&snap, &mixed, &opts, 1).unwrap().0;
    assert_eq!(before.len(), 3, "only the native branch reduces duplicates");
    assert_eq!(drain(&snap, &mixed, &alternate, 1).unwrap().0, before);
    for i in 0..18 {
        upd(&s, &format!("INSERT DATA {{ ex:new{i} ex:p {i} }}"));
        rt.round(&s);
    }
    assert_eq!(drain(&snap, &q, &opts, 1).unwrap().0, expected);
    assert_eq!(eager_rows(&snap, &q, &opts), expected);
}

#[test]
fn rebuilt_view_inherits_strict_fallback_and_reports_permissive_child() {
    let s = Store::in_memory(StoreOptions::default());
    let rt = runtime(&s);
    rt.put(&s, "split", ViewDef::table(
        "PREFIX apf: <http://jena.apache.org/ARQ/property#> SELECT ?x { ?x apf:strSplit ('one,two' ',') }",
    )).unwrap();
    rt.round(&s);
    let q =
        format!("{P}SELECT ?x {{ SERVICE view:read {{ [] view:name \"split\" ; view:bind ?x }} }}");
    let opts = QueryOptions {
        aggregate_errors: Some(AggregateErrors::Sparkles),
        views: Some(rt),
        ..Default::default()
    };
    let snap = s.snapshot();
    let mut c = select_cursor(snap.clone(), &q, &opts, &strict(1)).unwrap();
    assert!(matches!(c.next_batch(), Err(Error::Unsupported(s)) if s.contains("materialization")));
    let permissive = CursorOptions {
        fallback: FallbackPolicy::AllowMaterialization,
        ..strict(1)
    };
    let mut c = select_cursor(snap, &q, &opts, &permissive).unwrap();
    assert_eq!(c.next_batch().unwrap().unwrap().len(), 1);
    assert!(c.plan().has_materialization());
}

#[test]
fn rebuilt_view_preserves_template_blank_nodes_and_occurrences() {
    let s = Store::in_memory(StoreOptions::default());
    let rt = runtime(&s);
    rt.put(
        &s,
        "blank",
        ViewDef::table(format!(
            "{P}CONSTRUCT {{ _:b ex:q ?x ; ex:r ?x }} WHERE {{ VALUES ?x {{ 1 1 2 }} }}"
        )),
    )
    .unwrap();
    rt.round(&s);
    let q = format!(
        "{P}SELECT ?subject ?predicate ?object ?c {{ SERVICE view:read {{ [] view:name \"blank\" ; view:bind ?subject, ?predicate, ?object ; view:commit ?c }} }}"
    );
    let opts = QueryOptions {
        views: Some(rt),
        ..Default::default()
    };
    let snap = s.snapshot();
    let now = drain(&snap, &q, &opts, 1).unwrap().0;
    assert_eq!(now.len(), 6);
    assert!(
        now.iter()
            .all(|row| matches!(row[0], Some(Term::BlankNode(_))))
    );
    let alternate = QueryOptions {
        aggregate_errors: Some(AggregateErrors::Sparkles),
        ..opts
    };
    assert_eq!(bag(drain(&snap, &q, &alternate, 1).unwrap().0), bag(now));
}

#[test]
fn stale_view_translates_across_compaction() {
    let dir = tempfile::tempdir().unwrap();
    let s = Store::open(dir.path(), StoreOptions::default()).unwrap();
    upd(&s, "INSERT DATA { _:b ex:p \"from-before-compaction\" }");
    let rt = runtime(&s);
    rt.put(
        &s,
        "items",
        ViewDef::table(format!("{P}SELECT ?s ?o {{ ?s ex:p ?o }}")),
    )
    .unwrap();
    rt.round(&s);
    let before = s.snapshot();
    let opts = QueryOptions {
        views: Some(rt),
        ..Default::default()
    };
    let q = format!(
        "{P}SELECT ?s ?o {{ SERVICE view:read {{ [] view:name \"items\" ; view:bind ?s, ?o ; view:allowStale true }} }}"
    );
    let expected = drain(&before, &q, &opts, 1).unwrap().0;
    s.compact().unwrap();
    let after = s.snapshot();
    assert!(!Arc::ptr_eq(&before.generation, &after.generation));
    assert_eq!(drain(&after, &q, &opts, 1).unwrap().0, expected);
}

#[test]
fn rebuilt_views_share_a_work_budget_in_both_execution_modes() {
    let s = Store::in_memory(StoreOptions::default());
    let rt = runtime(&s);
    for name in ["left", "right"] {
        rt.put(
            &s,
            name,
            ViewDef::table("SELECT ?x { VALUES ?x { 1 2 3 4 5 6 7 8 } FILTER(?x = 1) }"),
        )
        .unwrap();
    }
    rt.round(&s);
    let opts = QueryOptions {
        views: Some(rt),
        aggregate_errors: Some(AggregateErrors::Sparkles),
        no_cache: true,
        ..Default::default()
    };
    let snap = s.snapshot();
    let left = "SERVICE view:read { [] view:name \"left\" ; view:bind ?x }";
    let right = "SERVICE view:read { [] view:name \"right\" ; view:bind ?x }";
    let one = format!("{P}SELECT ?x {{ {left} }}");
    let both = format!("{P}SELECT ?x {{ {{ {left} }} UNION {{ {right} }} }}");
    let mut c = select_cursor(snap.clone(), &one, &opts, &strict(1)).unwrap();
    while c.next_batch().unwrap().is_some() {}
    let one_work = c.stats().rows_produced;
    assert!(one_work > 2, "the child scan contributes work");
    let limited = QueryOptions {
        max_rows_produced: Some(one_work + 2),
        ..opts
    };
    assert_eq!(drain(&snap, &one, &limited, 1).unwrap().0.len(), 1);
    assert_eq!(eager_rows(&snap, &one, &limited).len(), 1);
    assert!(matches!(
        drain(&snap, &both, &limited, 1),
        Err(Error::BudgetExceeded(_))
    ));
    assert!(matches!(
        query(snap, &both, &limited),
        Err(Error::BudgetExceeded(_))
    ));
}
