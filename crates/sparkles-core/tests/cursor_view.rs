//! Integration tests for native streaming `SERVICE view:read` cursors over immutable
//! shared view versions (item 1121).

use oxrdf::Term;
use sparkles_core::access::{
    Caller, GraphAccess, Graphs, Preparation, Protection, Rule, TripleRules,
};
use sparkles_core::error::BudgetKind;
use sparkles_core::incr::{Host, OpenHost, Runtime, RuntimeOptions, ViewDef};
use sparkles_core::sparql::update::update;
use sparkles_core::sparql::{
    AggregateErrors, CursorOptions, FallbackPolicy, QueryOptions, query, select_cursor,
    select_source_cursor,
};
use sparkles_core::store::{Store, StoreOptions};
use sparkles_core::task::{Control, Progress};
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

#[test]
fn rebuilt_reduced_is_specific_to_its_graph_occurrence() {
    let s = Store::in_memory(StoreOptions::default());
    upd(
        &s,
        "INSERT DATA { GRAPH <urn:outer> { ex:a ex:p 1 } GRAPH <urn:inner> { ex:a ex:p 1 } }",
    );
    let rt = runtime(&s);
    rt.set_options(RuntimeOptions {
        history_commits: 1,
        ..rt.options()
    });
    rt.put(
        &s,
        "nested",
        ViewDef::table(
            "SELECT ?x WHERE {
            { SELECT REDUCED ?x WHERE { VALUES ?x { 1 1 } } }
            UNION
            { GRAPH <urn:outer> { GRAPH <urn:inner> {
                { SELECT REDUCED ?x WHERE { VALUES ?x { 1 1 } } }
            } } }
            UNION { <http://example.org/delta> <http://example.org/p> ?x }
        }",
        ),
    )
    .unwrap();
    rt.round(&s);
    let snap = s.snapshot();
    let opts = QueryOptions {
        views: Some(rt.clone()),
        ..Default::default()
    };
    let q = format!(
        "{P}SELECT ?x ?c {{ SERVICE view:read {{ [] view:name 'nested'; view:bind ?x; view:commit ?c }} }}"
    );
    let maintained = drain(&snap, &q, &opts, 1).unwrap().0;
    assert_eq!(maintained.len(), 3);
    let alternate = QueryOptions {
        aggregate_errors: Some(AggregateErrors::Sparkles),
        ..opts.clone()
    };
    assert_eq!(drain(&snap, &q, &alternate, 1).unwrap().0, maintained);
    for value in [2, 3] {
        upd(&s, &format!("INSERT DATA {{ ex:delta ex:p {value} }}"));
        rt.round(&s);
    }
    assert_eq!(drain(&snap, &q, &opts, 1).unwrap().0, maintained);
}

#[test]
fn rebuilt_construct_hashes_the_native_values_terms() {
    let s = Store::in_memory(StoreOptions::default());
    let rt = runtime(&s);
    let values = r#"VALUES ?x { "[_:a]"^^<http://w3id.org/awslabs/neptune/SPARQL-CDTs/List> }"#;
    rt.put(
        &s,
        "cdt",
        ViewDef::table(format!(
            "{P}CONSTRUCT {{ _:b ex:q ?x }} WHERE {{
            {{ {values} }} UNION {{ SELECT ?x WHERE {{ {values} }} LIMIT 1 }}
        }}",
        )),
    )
    .unwrap();
    rt.round(&s);
    let q = format!(
        "{P}SELECT ?subject ?object {{ SERVICE view:read {{ [] view:name 'cdt'; view:bind ?subject, ?object }} }}"
    );
    let opts = QueryOptions {
        views: Some(rt),
        ..Default::default()
    };
    let snap = s.snapshot();
    let maintained = drain(&snap, &q, &opts, 1).unwrap().0;
    assert_eq!(maintained.len(), 2);
    assert_eq!(
        maintained
            .iter()
            .filter(|row| { matches!(&row[1], Some(Term::Literal(l)) if l.value() == "[_:a]") })
            .count(),
        1
    );
    let alternate = QueryOptions {
        aggregate_errors: Some(AggregateErrors::Sparkles),
        ..opts
    };
    assert_eq!(
        bag(drain(&snap, &q, &alternate, 1).unwrap().0),
        bag(maintained)
    );
}

struct ProtectedHost(Arc<GraphAccess>);

impl Host for ProtectedHost {
    fn access(&self, principal: Option<&str>) -> Result<Option<Arc<GraphAccess>>> {
        Ok(principal.map(|_| self.0.clone()))
    }

    fn stored_query(&self, name: &str) -> Result<String> {
        OpenHost.stored_query(name)
    }
}

#[test]
fn a_historical_construct_admits_the_principals_cold_mask() {
    let s = Store::in_memory(StoreOptions::default());
    for chunk in 0..4 {
        let mut data = String::from("INSERT DATA { ");
        for i in 0..1000 {
            data.push_str(&format!("ex:s{chunk}-{i} ex:hidden 1 . "));
        }
        data.push('}');
        upd(&s, &data);
    }
    upd(&s, "INSERT DATA { ex:public ex:visible 42 }");
    let historical = s.snapshot();
    upd(&s, "INSERT DATA { ex:next ex:visible 43 }");
    let rt = runtime(&s);
    rt.set_host(Arc::new(ProtectedHost(Arc::new(
        GraphAccess::with_triples(
            Graphs::All,
            Graphs::none(),
            TripleRules {
                rules: vec![Rule {
                    protection: Arc::new(Protection {
                        name: "private".into(),
                        predicates: Some(vec!["http://example.org/hidden".into()]),
                        classes: None,
                        subclasses: false,
                        graphs: None,
                        pattern: None,
                        prefixes: Default::default(),
                        hide_inferences: false,
                    }),
                    read: Graphs::none(),
                    write: Graphs::none(),
                }],
                caller: Caller::default(),
                limits: Default::default(),
            },
        ),
    ))));
    rt.put(
        &s,
        "protected",
        ViewDef {
            principal: Some("reader".into()),
            ..ViewDef::table(format!("{P}CONSTRUCT {{ ?s ?p ?o }} WHERE {{ ?s ?p ?o }}"))
        },
    )
    .unwrap();
    rt.round(&s);
    let q = format!(
        "{P}SELECT ?subject ?object ?c {{ SERVICE view:read {{ [] view:name 'protected'; view:bind ?subject, ?object; view:commit ?c }} }} LIMIT 1"
    );
    let opts = QueryOptions {
        views: Some(rt),
        ..Default::default()
    };
    let tiny = QueryOptions {
        max_memory_bytes: Some(64 << 10),
        ..opts.clone()
    };
    let err = drain(&historical, &q, &tiny, 1).unwrap_err();
    assert!(matches!(err, Error::BudgetExceeded(b) if b.kind == BudgetKind::Memory));
    let rows = drain(&historical, &q, &opts, 1).unwrap().0;
    assert_eq!(
        rows,
        vec![vec![
            Some(Term::NamedNode(
                oxrdf::NamedNode::new("http://example.org/public").unwrap()
            )),
            Some(Term::Literal(oxrdf::Literal::from(42))),
            Some(Term::Literal(oxrdf::Literal::from(
                historical.commit as i64
            ))),
        ]]
    );

    let flag = Arc::new(AtomicBool::new(false));
    let cancel = flag.clone();
    let prep = Preparation::new(
        Control {
            progress: Progress::new(move |_, phase| {
                if phase == "applying permission mask" {
                    cancel.store(true, Ordering::Relaxed);
                }
            }),
            ..Control::with_cancel(flag.clone())
        },
        32 << 20,
    )
    .unwrap();
    let caller = Arc::new(GraphAccess::all());
    let view = s.prepare_read(caller.clone(), &prep).unwrap();
    let mut cursor = select_source_cursor(
        view.read_source(),
        &s,
        &q,
        &QueryOptions {
            graphs: Some(caller),
            aggregate_errors: Some(AggregateErrors::Sparkles),
            ..opts
        },
        &strict(1),
    )
    .unwrap();
    assert!(matches!(cursor.next_batch(), Err(Error::Cancelled)));
    assert!(flag.load(Ordering::Relaxed));
}

#[test]
fn rebuild_releases_planning_staging_before_streaming() {
    let s = Store::in_memory(StoreOptions::default());
    let rt = runtime(&s);
    rt.put(
        &s,
        "v",
        ViewDef::table(format!(
            "SELECT ?x {{ VALUES ?x {{ 1 2 }} }} # {}",
            "a".repeat(32 << 10)
        )),
    )
    .unwrap();
    rt.round(&s);
    let q = format!("{P}SELECT ?x {{ SERVICE view:read {{ [] view:name 'v'; view:bind ?x }} }}");
    let opts = QueryOptions {
        views: Some(rt),
        aggregate_errors: Some(AggregateErrors::Sparkles),
        ..Default::default()
    };
    let mut cursor = select_cursor(s.snapshot(), &q, &opts, &strict(1)).unwrap();
    let batch = cursor.next_batch().unwrap().unwrap();
    assert_eq!(batch.len(), 1);
    assert!(cursor.memory_meter().live_bytes() < 512 << 10);
    assert!(cursor.stats().mem_peak_bytes > 512 << 10);
}

#[test]
fn rebuilt_exists_preserves_native_reduced_and_values_cdts() {
    let s = Store::in_memory(StoreOptions::default());
    let rt = runtime(&s);
    let cdt = r#"VALUES ?x { "[_:a]"^^<http://w3id.org/awslabs/neptune/SPARQL-CDTs/List> }"#;

    // 1. Ordered window with REDUCED inside a native EXISTS (Sol r3 #1).
    rt.put(
        &s,
        "exists_window_reduced",
        ViewDef::table(
            "SELECT ?outer WHERE {
                VALUES ?outer { 1 }
                FILTER EXISTS {
                    { SELECT REDUCED ?x WHERE { VALUES ?x { 1 1 } }
                      ORDER BY ?x LIMIT 1 OFFSET 1 }
                }
            }",
        ),
    )
    .unwrap();

    // 2. Subquery aggregate over REDUCED inside a native EXISTS (Neon r3 #1b).
    rt.put(
        &s,
        "exists_agg_reduced",
        ViewDef::table(
            "SELECT ?x WHERE {
                VALUES ?x { 1 }
                FILTER EXISTS {
                    SELECT (COUNT(*) AS ?n) WHERE {
                        { SELECT REDUCED ?y WHERE { VALUES ?y { 1 1 } } }
                    }
                    HAVING (COUNT(*) = 1)
                }
            }",
        ),
    )
    .unwrap();

    // 3. Native VALUES CDT blank-node literal inside FILTER EXISTS and NOT EXISTS (Neon r3 #1a).
    rt.put(
        &s,
        "exists_cdt",
        ViewDef::table(format!(
            "SELECT ?x WHERE {{
                {cdt}
                FILTER EXISTS {{ {cdt} }}
            }}"
        )),
    )
    .unwrap();
    rt.put(
        &s,
        "not_exists_cdt",
        ViewDef::table(format!(
            "SELECT ?x WHERE {{
                {cdt}
                FILTER NOT EXISTS {{ {cdt} }}
            }}"
        )),
    )
    .unwrap();

    // 4. Nested EXISTS inside EXISTS with CDT VALUES and REDUCED.
    rt.put(
        &s,
        "nested_exists",
        ViewDef::table(format!(
            "SELECT ?x WHERE {{
                {cdt}
                FILTER EXISTS {{
                    {cdt}
                    FILTER EXISTS {{
                        SELECT (COUNT(*) AS ?n) WHERE {{
                            {{ SELECT REDUCED ?y WHERE {{ VALUES ?y {{ 1 1 }} }} }}
                        }}
                        HAVING (COUNT(*) = 1)
                    }}
                }}
            }}"
        )),
    )
    .unwrap();

    // 5. Correlated / non-native EXISTS inside OR keeps ordinary non-native REDUCED.
    rt.put(
        &s,
        "correlated_exists_or",
        ViewDef::table(
            "SELECT ?outer WHERE {
                VALUES ?outer { 1 }
                OPTIONAL { <http://example.org/none> <http://example.org/p> ?opt }
                FILTER (BOUND(?opt) || EXISTS {
                    { SELECT REDUCED ?x WHERE { VALUES ?x { 1 1 } }
                      ORDER BY ?x LIMIT 1 OFFSET 1 }
                })
            }",
        ),
    )
    .unwrap();

    // 6. CONSTRUCT view with CDT blank-node VALUES in both WHERE and native EXISTS.
    rt.put(
        &s,
        "construct_exists_cdt",
        ViewDef::table(format!(
            "{P}CONSTRUCT {{ _:b ex:q ?x }} WHERE {{
                {cdt}
                FILTER EXISTS {{ {cdt} }}
            }}"
        )),
    )
    .unwrap();

    rt.round(&s);
    let snap = s.snapshot();
    let opts = QueryOptions {
        views: Some(rt),
        ..Default::default()
    };
    let alternate = QueryOptions {
        aggregate_errors: Some(AggregateErrors::Sparkles),
        ..opts.clone()
    };

    for (name, col, expected_len) in [
        ("exists_window_reduced", "?outer", 0usize),
        ("exists_agg_reduced", "?x", 1),
        ("exists_cdt", "?x", 1),
        ("not_exists_cdt", "?x", 0),
        ("nested_exists", "?x", 1),
        ("correlated_exists_or", "?outer", 1),
    ] {
        let q = format!(
            "{P}SELECT {col} {{ SERVICE view:read {{ [] view:name '{name}'; view:bind {col} }} }}"
        );
        let maintained = drain(&snap, &q, &opts, 1).unwrap().0;
        assert_eq!(maintained.len(), expected_len, "maintained view {name}");
        let rebuilt = drain(&snap, &q, &alternate, 1).unwrap().0;
        assert_eq!(bag(rebuilt), bag(maintained), "rebuilt view {name}");
    }

    let cq = format!(
        "{P}SELECT ?subject ?object {{ SERVICE view:read {{ [] view:name 'construct_exists_cdt'; view:bind ?subject, ?object }} }}"
    );
    let maintained = drain(&snap, &cq, &opts, 1).unwrap().0;
    assert_eq!(maintained.len(), 1);
    let rebuilt = drain(&snap, &cq, &alternate, 1).unwrap().0;
    assert_eq!(bag(rebuilt), bag(maintained));
}

#[test]
fn raw_cursor_wait_credit_applies_to_protected_view_rebuild() {
    let s = Store::in_memory(StoreOptions::default());
    upd(
        &s,
        "INSERT DATA { ex:h ex:hidden 1 . ex:a ex:visible 1 . ex:b ex:visible 2 }",
    );
    let rt = runtime(&s);
    rt.set_host(Arc::new(ProtectedHost(Arc::new(
        GraphAccess::with_triples(
            Graphs::All,
            Graphs::none(),
            TripleRules {
                rules: vec![Rule {
                    protection: Arc::new(Protection {
                        name: "private".into(),
                        predicates: Some(vec!["http://example.org/hidden".into()]),
                        classes: None,
                        subclasses: false,
                        graphs: None,
                        pattern: None,
                        prefixes: Default::default(),
                        hide_inferences: false,
                    }),
                    read: Graphs::none(),
                    write: Graphs::none(),
                }],
                caller: Caller::default(),
                limits: Default::default(),
            },
        ),
    ))));
    rt.put(
        &s,
        "protected_stream",
        ViewDef {
            principal: Some("reader".into()),
            ..ViewDef::table(format!("{P}SELECT ?s ?o WHERE {{ ?s ex:visible ?o }}"))
        },
    )
    .unwrap();
    rt.round(&s);

    let q = format!(
        "{P}SELECT ?s ?o {{ SERVICE view:read {{ [] view:name 'protected_stream'; view:bind ?s, ?o }} }}"
    );
    let opts = QueryOptions {
        views: Some(rt),
        aggregate_errors: Some(AggregateErrors::Sparkles),
        timeout: Some(Duration::from_millis(120)),
        ..Default::default()
    };
    let mut cursor = select_cursor(s.snapshot(), &q, &opts, &strict(1)).unwrap();
    let first = cursor.next_batch().unwrap().unwrap();
    assert_eq!(first.len(), 1);

    // Pause longer than the initial 120 ms timeout while crediting consumer wait time.
    let wait = Duration::from_millis(180);
    std::thread::sleep(wait);
    cursor.deadline_pause().fetch_add(
        Duration::from_millis(400).as_nanos() as u64,
        Ordering::Relaxed,
    );

    let second = cursor.next_batch().unwrap().unwrap();
    assert_eq!(second.len(), 1);
    assert!(cursor.next_batch().unwrap().is_none());
}

#[test]
fn rebuilt_construct_keeps_retained_template_charged_between_batches() {
    let s = Store::in_memory(StoreOptions::default());
    let rt = runtime(&s);
    // Three 300 KiB literals in unbound template triples total 900 KiB of retained
    // template storage while staying under the 1 MiB view definition limit and keeping
    // any single per-triple transient decode well below the retained total.
    let part_len = 300 << 10;
    let total_payload = part_len * 3;
    let payload = "x".repeat(part_len);
    rt.put(
        &s,
        "big_template",
        ViewDef::table(format!(
            "{P}CONSTRUCT {{ ex:s ex:p ?x . ?unbound ex:h1 \"{payload}\" . ?unbound ex:h2 \"{payload}\" . ?unbound ex:h3 \"{payload}\" }} WHERE {{ VALUES ?x {{ 1 2 }} }}"
        )),
    )
    .unwrap();
    rt.round(&s);

    let q = format!(
        "{P}SELECT ?subject ?predicate ?object {{ SERVICE view:read {{ [] view:name 'big_template'; view:bind ?subject, ?predicate, ?object }} }}"
    );
    let opts = QueryOptions {
        views: Some(rt),
        aggregate_errors: Some(AggregateErrors::Sparkles),
        ..Default::default()
    };
    let mut cursor = select_cursor(s.snapshot(), &q, &opts, &strict(1)).unwrap();
    let first = cursor.next_batch().unwrap().unwrap();
    assert_eq!(first.len(), 1);
    let live_between = cursor.memory_meter().live_bytes();
    assert!(
        live_between >= total_payload as u64,
        "expected retained CONSTRUCT template ({total_payload} bytes) to stay charged between batches, got {live_between}"
    );
    assert!(
        live_between < 3 << 20,
        "expected transient planning staging to be released before streaming, got {live_between}"
    );
    let second = cursor.next_batch().unwrap().unwrap();
    assert_eq!(second.len(), 1);
    assert!(cursor.next_batch().unwrap().is_none());
    cursor.close();
    assert!(cursor.memory_meter().live_bytes() < total_payload as u64);
}

#[test]
fn rebuilt_exists_ordered_window_on_shared_variable_matches_maintained_and_direct() {
    let s = Store::in_memory(StoreOptions::default());
    let rt = runtime(&s);
    let cases = [
        (
            "shared_reduced_exists",
            "SELECT ?x WHERE {
                VALUES ?x { 2 }
                FILTER EXISTS {
                    { SELECT REDUCED ?x WHERE { VALUES ?x { 1 2 2 } }
                      ORDER BY ?x LIMIT 1 OFFSET 1 }
                }
            }",
            1usize,
        ),
        (
            "shared_reduced_not_exists",
            "SELECT ?x WHERE {
                VALUES ?x { 2 }
                FILTER NOT EXISTS {
                    { SELECT REDUCED ?x WHERE { VALUES ?x { 1 2 2 } }
                      ORDER BY ?x LIMIT 1 OFFSET 1 }
                }
            }",
            0usize,
        ),
        (
            "shared_distinct_exists",
            "SELECT ?x WHERE {
                VALUES ?x { 2 }
                FILTER EXISTS {
                    { SELECT DISTINCT ?x WHERE { VALUES ?x { 1 2 2 } }
                      ORDER BY ?x LIMIT 1 OFFSET 1 }
                }
            }",
            0usize,
        ),
        (
            "shared_distinct_not_exists",
            "SELECT ?x WHERE {
                VALUES ?x { 2 }
                FILTER NOT EXISTS {
                    { SELECT DISTINCT ?x WHERE { VALUES ?x { 1 2 2 } }
                      ORDER BY ?x LIMIT 1 OFFSET 1 }
                }
            }",
            1usize,
        ),
        (
            "shared_plain_window_exists",
            "SELECT ?x WHERE {
                VALUES ?x { 2 }
                FILTER EXISTS {
                    { SELECT ?x WHERE { VALUES ?x { 1 2 } }
                      ORDER BY ?x LIMIT 1 OFFSET 1 }
                }
            }",
            0usize,
        ),
    ];
    for (name, def, _) in cases {
        rt.put(&s, name, ViewDef::table(def)).unwrap();
    }
    rt.round(&s);
    let snap = s.snapshot();
    let opts = QueryOptions {
        views: Some(rt),
        ..Default::default()
    };
    let alternate = QueryOptions {
        aggregate_errors: Some(AggregateErrors::Sparkles),
        ..opts.clone()
    };
    for (name, def, expected_len) in cases {
        let direct = drain(&snap, def, &QueryOptions::default(), 1).unwrap().0;
        assert_eq!(direct.len(), expected_len, "direct query {name}");
        let q = format!(
            "{P}SELECT ?x {{ SERVICE view:read {{ [] view:name '{name}'; view:bind ?x }} }}"
        );
        let maintained = drain(&snap, &q, &opts, 1).unwrap().0;
        assert_eq!(bag(maintained.clone()), bag(direct), "maintained {name}");
        let rebuilt = drain(&snap, &q, &alternate, 1).unwrap().0;
        assert_eq!(bag(rebuilt), bag(maintained), "rebuilt {name}");
    }
}

#[test]
fn rebuilt_exists_keeps_retained_syntax_and_patterns_charged() {
    let s = Store::in_memory(StoreOptions::default());
    let rt = runtime(&s);
    rt.set_host(Arc::new(ProtectedHost(Arc::new(
        GraphAccess::with_triples(
            Graphs::All,
            Graphs::none(),
            TripleRules {
                rules: vec![Rule {
                    protection: Arc::new(Protection {
                        name: "private".into(),
                        predicates: Some(vec!["http://example.org/hidden".into()]),
                        classes: None,
                        subclasses: false,
                        graphs: None,
                        pattern: None,
                        prefixes: Default::default(),
                        hide_inferences: false,
                    }),
                    read: Graphs::none(),
                    write: Graphs::none(),
                }],
                caller: Caller::default(),
                limits: Default::default(),
            },
        ),
    ))));

    // Twenty copies of the same 32 KiB literal total 640 KiB in the retained EXISTS
    // syntax while staying under the 1 MiB view definition limit. VALUES planning
    // interns the repeated string once (~32 KiB) and REDUCED deduplicates by ID without
    // decoding the string, so live bytes >= 640 KiB between batches proves the retained
    // EXISTS syntax and projected occurrence set stay charged across batches and drop
    // with the child, while staying far below the ~10 MiB planning staging peak.
    let copy_len = 32 << 10;
    let copies = 20usize;
    let total_payload = copy_len * copies;
    let payload = "y".repeat(copy_len);
    let values = std::iter::repeat_n(format!("\"{payload}\""), copies)
        .collect::<Vec<_>>()
        .join(" ");
    let def = format!(
        "SELECT ?outer WHERE {{
            VALUES ?outer {{ 1 2 }}
            FILTER EXISTS {{
                {{ SELECT REDUCED ?x WHERE {{ VALUES ?x {{ {values} }} }} }}
            }}
        }}"
    );
    rt.put(&s, "big_exists", ViewDef::table(def.clone()))
        .unwrap();
    rt.put(
        &s,
        "big_exists_protected",
        ViewDef {
            principal: Some("reader".into()),
            ..ViewDef::table(def)
        },
    )
    .unwrap();
    rt.round(&s);

    for name in ["big_exists", "big_exists_protected"] {
        let q = format!(
            "{P}SELECT ?outer {{ SERVICE view:read {{ [] view:name '{name}'; view:bind ?outer }} }}"
        );
        let opts = QueryOptions {
            views: Some(rt.clone()),
            aggregate_errors: Some(AggregateErrors::Sparkles),
            ..Default::default()
        };
        let mut cursor = select_cursor(s.snapshot(), &q, &opts, &strict(1)).unwrap();
        let first = cursor.next_batch().unwrap().unwrap();
        assert_eq!(first.len(), 1, "{name}: first batch");
        let live_between = cursor.memory_meter().live_bytes();
        assert!(
            live_between >= total_payload as u64,
            "{name}: expected retained EXISTS syntax ({total_payload} bytes) to stay charged between batches, got {live_between}"
        );
        assert!(
            live_between < 2 << 20,
            "{name}: expected transient planning staging to be released before streaming, got {live_between}"
        );
        let second = cursor.next_batch().unwrap().unwrap();
        assert_eq!(second.len(), 1, "{name}: second batch");
        assert!(cursor.next_batch().unwrap().is_none(), "{name}: eof");
        let live_after_eof = cursor.memory_meter().live_bytes();
        assert!(
            live_after_eof < total_payload as u64,
            "{name}: expected retained EXISTS syntax charge to release with child at EOF, got {live_after_eof}"
        );
        cursor.close();
        assert!(cursor.memory_meter().live_bytes() < total_payload as u64);

        // A tight memory budget refuses before cloning oversized EXISTS syntax.
        let tight = QueryOptions {
            max_memory_bytes: Some(256 << 10),
            ..opts
        };
        let mut tight_cursor = select_cursor(s.snapshot(), &q, &tight, &strict(1)).unwrap();
        let err = tight_cursor
            .next_batch()
            .err()
            .expect("expected memory limit error under tight budget");
        assert!(
            matches!(err, Error::BudgetExceeded(ref b) if b.kind == BudgetKind::Memory),
            "{name}: expected memory BudgetExceeded under tight budget, got {err:?}"
        );
    }
}
