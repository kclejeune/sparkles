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
            let (answer, sizes, plan) = streamed(&snap, q, &opts(), &strict(rows)).unwrap();
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
