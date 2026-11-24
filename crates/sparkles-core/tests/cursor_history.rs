//! A history query without a limit or input under the strict cursor policy reads the
//! change log a page of whole commits at a time (item 829, history group).

use oxrdf::Term;
use sparkles_core::sparql::update::update;
use sparkles_core::sparql::{
    CursorOptions, CursorPlan, FallbackPolicy, QueryOptions, query, select_cursor,
};
use sparkles_core::store::{ChangeLogSettings, Snapshot, Store, StoreOptions};
use sparkles_core::{BudgetKind, Error, Result};
use std::sync::Arc;

const P: &str = "PREFIX hist: <urn:x-sparkles:history#> ";

fn upd(s: &Store, u: &str) {
    update(s, u, &QueryOptions::default()).unwrap_or_else(|e| panic!("{u}: {e}"));
}

/// 41 commits over many small segments: forty insert 150 quads each, every fourth is
/// followed by a removal of 50, the twentieth is followed by one commit of 5,000 quads,
/// larger than a page, and the last adds three quads whose subject is their object.
fn store(dir: &std::path::Path) -> Store {
    let s = Store::open(
        dir,
        StoreOptions {
            change_log_segment_bytes: 4096,
            ..Default::default()
        },
    )
    .unwrap();
    for i in 0..40 {
        let body: String = (0..150)
            .map(|j| format!("<urn:s{i}x{j}> <urn:p{}> \"{i}-{j}\" . ", j % 3))
            .collect();
        upd(&s, &format!("INSERT DATA {{ {body} }}"));
        if i % 4 == 3 {
            let body: String = (0..50)
                .map(|j| format!("<urn:s{i}x{j}> <urn:p{}> \"{i}-{j}\" . ", j % 3))
                .collect();
            upd(&s, &format!("DELETE DATA {{ {body} }}"));
        }
        if i == 19 {
            let body: String = (0..5000)
                .map(|j| format!("<urn:big{j}> <urn:p0> {j} . "))
                .collect();
            upd(&s, &format!("INSERT DATA {{ {body} }}"));
        }
    }
    upd(
        &s,
        "INSERT DATA { <urn:a> <urn:p0> <urn:a> . <urn:b> <urn:p0> <urn:b> . <urn:c> <urn:p1> <urn:c> }",
    );
    s.flush_change_log().unwrap();
    s
}

fn opts() -> QueryOptions {
    QueryOptions {
        no_cache: true,
        ..Default::default()
    }
}

fn strict(rows: usize) -> CursorOptions {
    CursorOptions {
        batch_rows: rows,
        fallback: FallbackPolicy::RejectMaterialization,
        ..Default::default()
    }
}

type Rows = Vec<Vec<Option<Term>>>;

fn eager(snap: &Arc<Snapshot>, q: &str, o: &QueryOptions) -> Result<Rows> {
    Ok(query(snap.clone(), &format!("{P}{q}"), o)?
        .rows()?
        .iter()
        .map(|r| r.values().to_vec())
        .collect())
}

/// The rows of a cursor in order, the size of each batch, and its plan once drained.
fn streamed(
    snap: &Arc<Snapshot>,
    q: &str,
    o: &QueryOptions,
    c: &CursorOptions,
) -> Result<(Rows, Vec<usize>, CursorPlan)> {
    let mut cursor = select_cursor(snap.clone(), &format!("{P}{q}"), o, c)?;
    let (mut out, mut sizes) = (Vec::new(), Vec::new());
    while let Some(b) = cursor.next_batch()? {
        sizes.push(b.len());
        for r in 0..b.len() {
            out.push(b.row(r)?.values().to_vec());
        }
    }
    Ok((out, sizes, cursor.plan().clone()))
}

fn materializes(plan: &CursorPlan) -> bool {
    plan.materializes || plan.children.iter().any(materializes)
}

fn find<'a>(plan: &'a CursorPlan, op: &str) -> Option<&'a CursorPlan> {
    if plan.operator.operator == op {
        return Some(plan);
    }
    plan.children.iter().find_map(|c| find(c, op))
}

const ALL: &str = "SELECT ?s ?p ?o ?op ?c { SERVICE hist:changes { << ?s ?p ?o >> hist:op ?op ; hist:commit ?c } }";

fn turn_off(s: &Store) -> Result<()> {
    s.set_change_log_settings(ChangeLogSettings {
        enabled: Some(false),
        ..Default::default()
    })
    .map(|_| ())
}

/// Every batch size gives eager's rows in eager's order, both ways through the log, over
/// a range, with the commit's metadata, and with a variable used twice that refuses
/// pages of changes before the three it matches. No batch is empty.
#[test]
fn a_history_query_without_a_limit_reads_eagers_rows_at_every_batch_size() {
    let dir = tempfile::tempdir().unwrap();
    let s = store(dir.path());
    let snap = s.snapshot();
    let queries = [
        (ALL, 11_503),
        (
            "SELECT ?s ?o ?c { SERVICE hist:changes { << ?s <urn:p1> ?o >> hist:commit ?c ; hist:order hist:descending } }",
            2171,
        ),
        (
            "SELECT ?s ?o ?op ?t ?k ?a ?m { SERVICE hist:changes { << ?s <urn:p0> ?o >> hist:op ?op ; hist:time ?t ; hist:kind ?k ; hist:author ?a ; hist:message ?m ; hist:from 10 ; hist:to 30 } }",
            0,
        ),
        (
            "SELECT ?x ?c { SERVICE hist:changes { << ?x ?p ?x >> hist:commit ?c } }",
            3,
        ),
    ];
    for (q, n) in queries {
        let expected = eager(&snap, q, &opts()).unwrap();
        // the range's count is whatever eager reads, as long as it spans pages
        if n == 0 {
            assert!(expected.len() > 4096, "{q}: {}", expected.len());
        } else {
            assert_eq!(expected.len(), n, "{q}");
        }
        for rows in [1, 7, 5000] {
            let mut cursor =
                select_cursor(snap.clone(), &format!("{P}{q}"), &opts(), &strict(rows)).unwrap();
            let initial = find(cursor.plan(), "HistoryChanges").expect("initial history node");
            assert!(
                initial.growing_state,
                "initial plan {q} at batch {rows}: {initial:#?}"
            );
            let mut answer = Vec::new();
            let mut sizes = Vec::new();
            let mut checked_first = false;
            while let Some(b) = cursor.next_batch().unwrap() {
                sizes.push(b.len());
                for r in 0..b.len() {
                    answer.push(b.row(r).unwrap().values().to_vec());
                }
                drop(b);
                if !checked_first {
                    checked_first = true;
                    let after_first =
                        find(cursor.plan(), "HistoryChanges").expect("first-batch history node");
                    assert!(
                        after_first.growing_state,
                        "first-batch plan {q} at batch {rows}: {after_first:#?}"
                    );
                }
            }
            let plan = cursor.plan().clone();
            assert_eq!(answer, expected, "{q} at batch {rows}");
            assert!(sizes.iter().all(|&b| b > 0 && b <= rows), "{q}: {sizes:?}");
            assert!(!materializes(&plan), "{q}");
            let op = find(&plan, "HistoryChanges").expect("a history node");
            assert!(
                op.reason
                    .as_deref()
                    .is_some_and(|r| r.contains("a page of whole commits")),
                "{op:?}"
            );
            assert!(
                op.growing_state,
                "drained plan {q} at batch {rows}: {op:#?}"
            );
        }
    }
}

/// Retention between pulls keeps what the cursor has still to read, the log cannot be
/// turned off while it reads, and the pin goes when the range ends, while the cursor is
/// still open.
#[test]
fn retention_and_turning_the_log_off_between_pulls() {
    let dir = tempfile::tempdir().unwrap();
    let s = store(dir.path());
    let snap = s.snapshot();
    let expected = eager(&snap, ALL, &opts()).unwrap();
    let mut cursor =
        select_cursor(snap.clone(), &format!("{P}{ALL}"), &opts(), &strict(100)).unwrap();
    let mut answer: Rows = Vec::new();
    let take = |cursor: &mut sparkles_core::sparql::QueryCursor| {
        let b = cursor.next_batch().unwrap()?;
        Some(
            (0..b.len())
                .map(|r| b.row(r).unwrap().values().to_vec())
                .collect::<Rows>(),
        )
    };
    answer.extend(take(&mut cursor).unwrap());
    let segments = s.change_log_status().unwrap().segments;
    s.set_change_log_settings(ChangeLogSettings {
        keep_commits: Some(1),
        ..Default::default()
    })
    .unwrap();
    let st = s.change_log_status().unwrap();
    assert_eq!(
        (st.first, st.segments),
        (Some(1), segments),
        "the pin keeps the log"
    );
    assert!(matches!(turn_off(&s), Err(Error::Conflict(_))));
    while let Some(rows) = take(&mut cursor) {
        answer.extend(rows);
    }
    assert_eq!(answer, expected);
    // the range has ended: the open cursor pins nothing
    turn_off(&s).unwrap();
    drop(cursor);
}

/// A LIMIT above the call that is met releases the reader and its pin, though the cursor
/// stays open.
#[test]
fn a_met_limit_releases_the_pin() {
    let dir = tempfile::tempdir().unwrap();
    let s = store(dir.path());
    let snap = s.snapshot();
    let q = format!("{ALL} LIMIT 5");
    let mut cursor = select_cursor(snap.clone(), &format!("{P}{q}"), &opts(), &strict(2)).unwrap();
    let mut n = 0;
    while let Some(b) = cursor.next_batch().unwrap() {
        n += b.len();
    }
    assert_eq!(n, 5);
    turn_off(&s).unwrap();
    drop(cursor);
}

/// The row budget, a log that is off and an unbound range variable fail as they fail
/// an eager run.
#[test]
fn refusals_are_eagers() {
    let dir = tempfile::tempdir().unwrap();
    let s = store(dir.path());
    let snap = s.snapshot();
    let rows = QueryOptions {
        max_rows: Some(1000),
        ..opts()
    };
    let e = eager(&snap, ALL, &rows).unwrap_err();
    let c = streamed(&snap, ALL, &rows, &strict(7)).unwrap_err();
    assert!(
        matches!(&c, Error::BudgetExceeded(b) if b.kind == BudgetKind::Rows),
        "{c}"
    );
    assert_eq!(c.to_string(), e.to_string());
    let unbound = "SELECT ?s { SERVICE hist:changes { << ?s ?p ?o >> hist:from ?f } }";
    let e = eager(&snap, unbound, &opts()).unwrap_err();
    let c = streamed(&snap, unbound, &opts(), &strict(7)).unwrap_err();
    assert_eq!(c.to_string(), e.to_string());
    turn_off(&s).unwrap();
    let snap = s.snapshot();
    let e = eager(&snap, ALL, &opts()).unwrap_err();
    let c = streamed(&snap, ALL, &opts(), &strict(7)).unwrap_err();
    assert!(matches!(&c, Error::HistoryUnsupported(_)), "{c}");
    assert_eq!(c.to_string(), e.to_string());
}

/// A page that reaches the row budget stops there: a record after it that is larger
/// than the memory budget is not read, and the query fails on its rows as eager does.
#[test]
fn the_row_budget_fails_before_a_later_record_is_read() {
    let dir = tempfile::tempdir().unwrap();
    for persistent in [false, true] {
        let o = StoreOptions {
            change_log_segment_bytes: 4096,
            ..Default::default()
        };
        let s = match persistent {
            true => Store::open(dir.path(), o).unwrap(),
            false => Store::in_memory(o),
        };
        upd(&s, "INSERT DATA { <urn:a> <urn:p> 1 }");
        upd(&s, "INSERT DATA { <urn:b> <urn:p> 2 }");
        let big: String = (0..2000)
            .map(|j| format!("<urn:big> <urn:p> \"{}{j}\" . ", "v".repeat(1000)))
            .collect();
        upd(&s, &format!("INSERT DATA {{ {big} }}"));
        s.flush_change_log().unwrap();
        let snap = s.snapshot();
        let o = QueryOptions {
            max_rows: Some(1),
            max_memory_bytes: Some(1 << 20),
            ..opts()
        };
        let e = eager(&snap, ALL, &o).unwrap_err();
        assert!(
            matches!(&e, Error::BudgetExceeded(b) if b.kind == BudgetKind::Rows),
            "{e}"
        );
        let c = streamed(&snap, ALL, &o, &strict(7)).unwrap_err();
        assert!(
            matches!(&c, Error::BudgetExceeded(b) if b.kind == BudgetKind::Rows && b.limit == 1 && b.requested == 2),
            "persistent={persistent}: {c}"
        );
        drop(snap);
        drop(s);
    }
}

/// A branch deleted while a streamed history query reads its change log: the later
/// pulls either fail because its files are gone or give eager's rows, never fewer rows
/// without an error (row 1173).
#[test]
fn deleting_the_branch_under_a_reading_cursor_fails_or_reads_all() {
    let dir = tempfile::tempdir().unwrap();
    let main = Store::open(
        dir.path(),
        StoreOptions {
            change_log_segment_bytes: 4096,
            ..Default::default()
        },
    )
    .unwrap();
    upd(&main, "INSERT DATA { <urn:main> <urn:p> 1 }");
    main.create_branch("b", &Default::default()).unwrap();
    let (snap, expected) = {
        let b = main.branch("b").unwrap();
        for i in 0..100 {
            let body: String = (0..150)
                .map(|j| format!("<urn:s{i}x{j}> <urn:p{}> \"{i}-{j}\" . ", j % 3))
                .collect();
            upd(&b, &format!("INSERT DATA {{ {body} }}"));
        }
        b.flush_change_log().unwrap();
        let snap = b.snapshot();
        let expected = eager(&snap, ALL, &opts()).unwrap();
        (snap, expected)
    };
    assert!(expected.len() > 3 * 4096, "{}", expected.len());
    let mut cursor =
        select_cursor(snap.clone(), &format!("{P}{ALL}"), &opts(), &strict(100)).unwrap();
    let mut answer: Rows = Vec::new();
    let b = cursor.next_batch().unwrap().unwrap();
    for r in 0..b.len() {
        answer.push(b.row(r).unwrap().values().to_vec());
    }
    drop(b);
    main.delete_branch("b", true).unwrap();
    let mut failed = None;
    loop {
        match cursor.next_batch() {
            Ok(Some(b)) => {
                for r in 0..b.len() {
                    answer.push(b.row(r).unwrap().values().to_vec());
                }
            }
            Ok(None) => break,
            Err(e) => {
                failed = Some(e);
                break;
            }
        }
    }
    match failed {
        // the deleted segment files are gone
        Some(Error::Io(e)) => assert_eq!(e.kind(), std::io::ErrorKind::NotFound, "{e}"),
        Some(e) => panic!("{e}"),
        None => assert_eq!(answer, expected, "rows went missing without an error"),
    }
}

/// Crediting consumer wait time to `deadline_pause` extends the history reader's effective
/// deadline across subsequent page scans in both orders, while omitting the pause still
/// times out with `Error::Timeout`.
#[test]
fn a_paused_history_cursor_does_not_time_out_on_later_pages() {
    use std::sync::atomic::Ordering;
    use std::time::{Duration, Instant};

    let dir = tempfile::tempdir().unwrap();
    let s = store(dir.path());
    let snap = s.snapshot();
    let desc = "SELECT ?s ?p ?o ?op ?c { SERVICE hist:changes { << ?s ?p ?o >> hist:op ?op ; hist:commit ?c ; hist:order hist:descending } }";
    let hour_ns = 3_600_000_000_000u64;

    for (label, q) in [("ascending", ALL), ("descending", desc)] {
        let expected = eager(&snap, q, &opts()).unwrap();
        assert_eq!(expected.len(), 11_503, "{label}");

        let deadline = Instant::now() + Duration::from_secs(2);
        let timed = QueryOptions {
            deadline: Some(deadline),
            ..opts()
        };
        let mut cursor =
            select_cursor(snap.clone(), &format!("{P}{q}"), &timed, &strict(100)).unwrap();
        let first = cursor
            .next_batch()
            .unwrap_or_else(|e| panic!("{label}: first batch before deadline failed: {e}"))
            .expect("first batch");
        assert_eq!(first.len(), 100, "{label}");
        let mut answer = Vec::with_capacity(11_503);
        for r in 0..first.len() {
            answer.push(first.row(r).unwrap().values().to_vec());
        }
        drop(first);

        cursor.deadline_pause().store(hour_ns, Ordering::Relaxed);
        let past = deadline + Duration::from_millis(100);
        if let Some(wait) = past.checked_duration_since(Instant::now()) {
            std::thread::sleep(wait);
        }

        while let Some(b) = cursor
            .next_batch()
            .unwrap_or_else(|e| panic!("{label}: paused cursor failed on later page: {e}"))
        {
            for r in 0..b.len() {
                answer.push(b.row(r).unwrap().values().to_vec());
            }
        }
        assert_eq!(answer.len(), 11_503, "{label}");
        assert_eq!(answer, expected, "{label}");
    }

    // No-pause control: crossing past the original deadline without crediting a pause fails.
    let deadline = Instant::now() + Duration::from_secs(1);
    let timed = QueryOptions {
        deadline: Some(deadline),
        ..opts()
    };
    let mut cursor =
        select_cursor(snap.clone(), &format!("{P}{ALL}"), &timed, &strict(100)).unwrap();
    let first = cursor.next_batch().unwrap().expect("first batch");
    assert_eq!(first.len(), 100);
    drop(first);
    let past = deadline + Duration::from_millis(100);
    if let Some(wait) = past.checked_duration_since(Instant::now()) {
        std::thread::sleep(wait);
    }
    let err = loop {
        match cursor.next_batch() {
            Ok(Some(_)) => {}
            Ok(None) => break None,
            Err(e) => break Some(e),
        }
    };
    assert!(
        matches!(err, Some(Error::Timeout)),
        "expected Timeout without pause credit, got {err:?}"
    );
}
