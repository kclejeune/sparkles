//! Cursor groupings and DISTINCT that pass the rows that do not fit to later passes
//! through spill files (X08 §6.5).
use oxrdf::Term;
use oxrdfio::RdfFormat;
use sparkles_core::io::Source;
use sparkles_core::sparql::{CursorOptions, FallbackPolicy, QueryOptions, query, select_cursor};
use sparkles_core::spill::{Spill, SpillOptions};
use sparkles_core::store::{Store, StoreOptions};
use sparkles_core::{BudgetKind, Error};
use std::sync::Arc;

/// `groups` subjects with three values each, written in an order unrelated to the
/// subjects, and a label on every other subject.
fn store(groups: usize) -> Store {
    let s = Store::in_memory(StoreOptions::default());
    let n = groups * 3;
    let data = (0..n)
        .map(|i| {
            let k = (i * 7919) % n;
            let mut line = format!("<urn:s:{}> <urn:p> {} .\n", k % groups, k % 97);
            if k.is_multiple_of(2) {
                line.push_str(&format!(
                    "<urn:s:{}> <urn:t> \"t{}\" .\n",
                    k % groups,
                    k % 5
                ));
            }
            line
        })
        .collect::<String>();
    s.load(&[Source::from_bytes(
        data.into_bytes(),
        RdfFormat::Turtle,
        None,
    )])
    .unwrap();
    s
}

fn spill(dir: &std::path::Path) -> Arc<Spill> {
    Spill::open(&SpillOptions {
        dir: dir.to_path_buf(),
        max_bytes: 4 << 30,
        query_bytes: 4 << 30,
        min_free_bytes: 0,
        encrypted: false,
    })
    .unwrap()
}

/// The rows of `q` as a sorted bag, and the cursor's plan.
fn run(
    s: &Store,
    q: &str,
    memory: u64,
    spill: Option<Arc<Spill>>,
) -> Result<(Vec<String>, String), Error> {
    let opts = QueryOptions {
        max_memory_bytes: Some(memory),
        spill,
        ..Default::default()
    };
    let cursor = CursorOptions {
        batch_rows: 1000,
        fallback: FallbackPolicy::RejectMaterialization,
        ..Default::default()
    };
    let mut c = select_cursor(s.snapshot(), q, &opts, &cursor)?;
    let mut rows = Vec::new();
    while let Some(b) = c.next_batch()? {
        for i in 0..b.len() {
            rows.push(format!("{:?}", b.row(i).unwrap()));
        }
    }
    rows.sort();
    Ok((rows, c.plan_json()?))
}

fn eager(s: &Store, q: &str) -> Vec<String> {
    let mut rows: Vec<String> = query(s.snapshot(), q, &Default::default())
        .unwrap()
        .rows()
        .into_iter()
        .map(|r: Vec<Option<Term>>| format!("{r:?}"))
        .collect();
    rows.sort();
    rows
}

const MEMORY: u64 = 4 << 20;

#[test]
fn spilled_groupings_match_eager_aggregates() {
    let s = store(20_000);
    let tmp = tempfile::tempdir().unwrap();
    let spill = spill(tmp.path());
    for q in [
        "SELECT ?s (COUNT(*) AS ?n) (SUM(?v) AS ?sum) (MIN(?v) AS ?lo) (MAX(?v) AS ?hi) \
         (AVG(?v * 2) AS ?avg) (SAMPLE(?v) AS ?any) { ?s <urn:p> ?v } GROUP BY ?s",
        "SELECT ?s (GROUP_CONCAT(STR(?v); separator=\",\") AS ?all) \
         (COUNT(DISTINCT ?v) AS ?d) (COUNT(DISTINCT *) AS ?rows) { ?s <urn:p> ?v } GROUP BY ?s",
        "SELECT ?s ?t (COUNT(?v) AS ?n) { ?s <urn:p> ?v OPTIONAL { ?s <urn:t> ?t } } \
         GROUP BY ?s ?t",
    ] {
        let expected = eager(&s, q);
        match run(&s, q, MEMORY, None) {
            Err(Error::BudgetExceeded(b)) => assert_eq!(b.kind, BudgetKind::Memory, "{q}"),
            other => panic!(
                "{q}: expected a memory budget error, got {:?}",
                other.map(|r| r.0.len())
            ),
        }
        let (rows, plan) = run(&s, q, MEMORY, Some(spill.clone())).unwrap();
        assert_eq!(rows.len(), expected.len(), "{q}");
        assert_eq!(rows, expected, "{q}");
        assert!(plan.contains("\"spillPasses\""), "{q}: {plan}");
        assert_eq!(spill.reserved_bytes(), 0, "{q}");
    }
    // Nothing but the marker is left.
    for process in std::fs::read_dir(tmp.path()).unwrap() {
        let names: Vec<_> = std::fs::read_dir(process.unwrap().path())
            .unwrap()
            .map(|f| f.unwrap().file_name())
            .collect();
        assert_eq!(names, ["SPARKLES-SPILL"]);
    }
}

#[test]
fn groupings_that_fit_never_spill() {
    let s = store(500);
    let tmp = tempfile::tempdir().unwrap();
    let spill = spill(tmp.path());
    let q = "SELECT ?s (COUNT(*) AS ?n) { ?s <urn:p> ?v } GROUP BY ?s";
    let (rows, plan) = run(&s, q, 64 << 20, Some(spill.clone())).unwrap();
    assert_eq!(rows, eager(&s, q));
    assert!(!plan.contains("spillPasses"), "{plan}");
    assert_eq!(spill.written_bytes(), 0);
}

#[test]
fn a_grouping_without_keys_never_spills() {
    let s = store(20_000);
    let tmp = tempfile::tempdir().unwrap();
    let spill = spill(tmp.path());
    let q = "SELECT (COUNT(*) AS ?n) (SUM(?v) AS ?sum) { ?s <urn:p> ?v }";
    let (rows, _) = run(&s, q, MEMORY, Some(spill.clone())).unwrap();
    assert_eq!(rows, eager(&s, q));
    assert_eq!(spill.written_bytes(), 0);
}

#[test]
fn spilled_distinct_emits_each_row_once() {
    let s = store(20_000);
    let tmp = tempfile::tempdir().unwrap();
    let spill = spill(tmp.path());
    for q in [
        // UNION concatenates its arms, so the input has no order to keep
        "SELECT DISTINCT ?s { { ?s <urn:p> ?v } UNION { ?s <urn:t> ?t } }",
        "SELECT DISTINCT ?s ?v { { ?s <urn:p> ?v } UNION { ?s <urn:p> ?v } }",
        "SELECT DISTINCT ?s ?x { { ?s <urn:p> ?x } UNION { ?s <urn:t> ?x } }",
    ] {
        let expected = eager(&s, q);
        let memory = 256 << 10;
        if let Ok((rows, plan)) = run(&s, q, memory, None) {
            panic!(
                "{q}: expected a memory budget error, got {} rows: {plan}",
                rows.len()
            );
        }
        let (rows, plan) = run(&s, q, memory, Some(spill.clone())).unwrap();
        assert_eq!(rows, expected, "{q}");
        assert!(plan.contains("\"spillPasses\""), "{q}: {plan}");
        assert_eq!(spill.reserved_bytes(), 0, "{q}");
    }
}

/// The rows of `q` in the order the cursor gives them.
fn run_in_order(
    s: &Store,
    q: &str,
    memory: u64,
    spill: Option<Arc<Spill>>,
) -> Result<Vec<String>, Error> {
    let opts = QueryOptions {
        max_memory_bytes: Some(memory),
        spill,
        ..Default::default()
    };
    let cursor = CursorOptions {
        batch_rows: 1000,
        fallback: FallbackPolicy::RejectMaterialization,
        ..Default::default()
    };
    let mut c = select_cursor(s.snapshot(), q, &opts, &cursor)?;
    let mut rows = Vec::new();
    while let Some(b) = c.next_batch()? {
        for i in 0..b.len() {
            rows.push(format!("{:?}", b.row(i).unwrap()));
        }
    }
    Ok(rows)
}

/// A DISTINCT above an ORDER BY keeps the order. With spill on it is moved below the sort
/// and may spill there. A DISTINCT that stays above the sort never sends rows to a later
/// pass, since rows read back from disk would come after rows that sort after them, and
/// it fails on memory when its set does not fit rather than answer out of order.
#[test]
fn distinct_over_order_by_keeps_the_order_or_fails() {
    let s = store(20_000);
    let tmp = tempfile::tempdir().unwrap();
    let spill = spill(tmp.path());
    for q in [
        "SELECT DISTINCT ?s { { ?s <urn:p> ?v } UNION { ?s <urn:t> ?t } } ORDER BY ?s",
        "SELECT DISTINCT ?s ?v { { ?s <urn:p> ?v } UNION { ?s <urn:p> ?v } } \
         ORDER BY DESC(?v) ?s",
        "SELECT DISTINCT ?s ?v { { ?s <urn:p> ?v } UNION { ?s <urn:p> ?v } } \
         ORDER BY DESC(?v) ?s LIMIT 5 OFFSET 30000",
        "SELECT ?s { { SELECT DISTINCT ?s { { ?s <urn:p> ?v } UNION { ?s <urn:t> ?t } } \
         ORDER BY ?s LIMIT 10 OFFSET 15000 } }",
    ] {
        let expected: Vec<String> = query(s.snapshot(), q, &Default::default())
            .unwrap()
            .rows()
            .into_iter()
            .map(|r: Vec<Option<Term>>| format!("{r:?}"))
            .collect();
        for memory in [256 << 10, 64 << 20] {
            match run_in_order(&s, q, memory, Some(spill.clone())) {
                Ok(rows) => {
                    let first = rows.iter().zip(&expected).position(|(a, b)| a != b);
                    assert_eq!(
                        (rows.len(), first),
                        (expected.len(), None),
                        "{q} under {memory} bytes"
                    );
                }
                Err(Error::BudgetExceeded(b)) => {
                    assert_eq!(b.kind, BudgetKind::Memory, "{q}");
                    assert!(memory < 64 << 20, "{q} fails with room to spare");
                }
                Err(e) => panic!("{q}: {e}"),
            }
        }
        assert_eq!(spill.reserved_bytes(), 0, "{q}");
    }
}

/// `SELECT DISTINCT … ORDER BY` whose sort keys are projected deduplicates below the
/// sort when the query may spill, so the DISTINCT spills and the rows keep the order an
/// in-memory run gives, ties included. A key that is not projected keeps the DISTINCT
/// above the sort, which still keeps the order or fails on memory.
#[test]
fn distinct_below_order_by_spills_and_keeps_the_order() {
    let s = store(20_000);
    let tmp = tempfile::tempdir().unwrap();
    let spill = spill(tmp.path());
    let opts = |memory: u64, spill: Option<Arc<Spill>>| QueryOptions {
        max_memory_bytes: Some(memory),
        spill,
        ..Default::default()
    };
    let ordered = |q: &str, memory: u64, spill: Option<Arc<Spill>>| {
        let cursor = CursorOptions {
            batch_rows: 1000,
            fallback: FallbackPolicy::RejectMaterialization,
            ..Default::default()
        };
        let mut c = select_cursor(s.snapshot(), q, &opts(memory, spill), &cursor)?;
        let mut rows = Vec::new();
        while let Some(b) = c.next_batch()? {
            for i in 0..b.len() {
                rows.push(format!("{:?}", b.row(i).unwrap()));
            }
        }
        Ok::<_, Error>((rows, c.plan_json()?))
    };
    for q in [
        "SELECT DISTINCT ?s { { ?s <urn:p> ?v } UNION { ?s <urn:t> ?t } } ORDER BY ?s",
        // Many ties on the first key, decided by the second or by input order.
        "SELECT DISTINCT ?s ?v { { ?s <urn:p> ?v } UNION { ?s <urn:p> ?v } } \
         ORDER BY DESC(?v)",
        "SELECT DISTINCT ?s ?v { { ?s <urn:p> ?v } UNION { ?s <urn:p> ?v } } \
         ORDER BY (?v * 2) STR(?s) LIMIT 50 OFFSET 30000",
        "SELECT ?s { { SELECT DISTINCT ?s { { ?s <urn:p> ?v } UNION { ?s <urn:t> ?t } } \
         ORDER BY ?s LIMIT 10 OFFSET 15000 } }",
    ] {
        let (expected, _) = ordered(q, 256 << 20, None).unwrap();
        let eager: Vec<String> = query(s.snapshot(), q, &Default::default())
            .unwrap()
            .rows()
            .into_iter()
            .map(|r: Vec<Option<Term>>| format!("{r:?}"))
            .collect();
        assert_eq!(expected, eager, "{q}");
        let (rows, plan) =
            ordered(q, 512 << 10, Some(spill.clone())).unwrap_or_else(|e| panic!("{q}: {e}"));
        let first = rows.iter().zip(&expected).position(|(a, b)| a != b);
        assert_eq!((rows.len(), first), (expected.len(), None), "{q}");
        assert!(plan.contains("below the ORDER BY"), "{q}: {plan}");
        assert!(plan.contains("\"spillPasses\""), "{q}: {plan}");
        assert_eq!(spill.reserved_bytes(), 0, "{q}");
    }
    // A key that is not projected: the DISTINCT stays above the sort.
    let q = "SELECT DISTINCT ?s { ?s <urn:p> ?v } ORDER BY ?v ?s";
    let (expected, _) = ordered(q, 256 << 20, None).unwrap();
    match ordered(q, 512 << 10, Some(spill.clone())) {
        Ok((rows, plan)) => {
            assert_eq!(rows, expected, "{q}");
            assert!(!plan.contains("below the ORDER BY"), "{q}: {plan}");
        }
        Err(Error::BudgetExceeded(b)) => assert_eq!(b.kind, BudgetKind::Memory),
        Err(e) => panic!("{q}: {e}"),
    }
}

/// GROUP_CONCAT joins a group's values in the order its input gives them, so an ordered
/// subquery orders the concatenation, as in Jena. A streamed grouping keeps that order
/// whether it fits in memory or passes groups to later passes.
#[test]
fn group_concat_follows_an_ordered_subquery_in_every_mode() {
    let s = store(20_000);
    let tmp = tempfile::tempdir().unwrap();
    let spill = spill(tmp.path());
    for q in [
        "SELECT ?s (GROUP_CONCAT(STR(?v); separator=\",\") AS ?all) \
         { { SELECT ?s ?v { ?s <urn:p> ?v } ORDER BY DESC(?v) } } GROUP BY ?s",
        "SELECT ?s (GROUP_CONCAT(STR(?v); separator=\",\") AS ?all) \
         { { SELECT ?s ?v { ?s <urn:p> ?v } ORDER BY ?v } } GROUP BY ?s",
    ] {
        let expected = eager(&s, q);
        // Every concatenation of the eager answer is in the subquery's order.
        let descending = q.contains("DESC");
        let mut checked = 0;
        for row in &expected {
            let start =
                row.find("String(\"").unwrap_or_else(|| panic!("{row}")) + "String(\"".len();
            let all = &row[start..start + row[start..].find('"').unwrap()];
            let values: Vec<i64> = all.split(',').map(|v| v.parse().unwrap()).collect();
            let mut sorted = values.clone();
            sorted.sort_unstable();
            if descending {
                sorted.reverse();
            }
            assert_eq!(values, sorted, "{q}: {row}");
            checked += usize::from(values.len() > 1 && values[0] != values[values.len() - 1]);
        }
        assert!(checked > 1000, "{q}");
        for (memory, spill) in [(64 << 20, None), (MEMORY, Some(spill.clone()))] {
            let spills = spill.is_some();
            let (rows, plan) = run(&s, q, memory, spill).unwrap();
            assert_eq!(rows, expected, "{q} under {memory} bytes");
            assert_eq!(plan.contains("\"spillPasses\""), spills, "{q}: {plan}");
        }
    }
}

/// A DISTINCT never spills above an ORDER BY that reaches it through a join, MINUS,
/// OPTIONAL or EXISTS that passes its first input's order on, so a spilled cursor gives
/// the rows in the order the same cursor gives them in memory.
#[test]
fn distinct_over_a_join_of_an_ordered_subquery_keeps_the_order_or_fails() {
    let s = store(20_000);
    let tmp = tempfile::tempdir().unwrap();
    let spill = spill(tmp.path());
    let ordered = "{ SELECT ?s ?v { ?s <urn:p> ?v } ORDER BY DESC(?v) ?s }";
    for q in [
        format!("SELECT DISTINCT ?s ?v ?t {{ {ordered} ?s <urn:t> ?t }}"),
        format!("SELECT DISTINCT ?s ?v ?t {{ {ordered} OPTIONAL {{ ?s <urn:t> ?t }} }}"),
        format!("SELECT DISTINCT ?s ?v {{ {ordered} MINUS {{ ?s <urn:t> \"t1\" }} }}"),
        format!("SELECT DISTINCT ?s ?v {{ {ordered} FILTER EXISTS {{ ?s <urn:t> ?t }} }}"),
        "SELECT DISTINCT ?s ?l { { SELECT ?s { ?s <urn:p> ?v } ORDER BY ?s LIMIT 100 } \
         ?s <urn:t> ?l }"
            .to_string(),
    ] {
        let expected = run_in_order(&s, &q, 256 << 20, None).unwrap();
        for memory in [512 << 10, 2 << 20, 8 << 20] {
            let r = run_in_order(&s, &q, memory, Some(spill.clone()));
            match r {
                Ok(rows) => {
                    let first = rows.iter().zip(&expected).position(|(a, b)| a != b);
                    assert_eq!((rows.len(), first), (expected.len(), None), "{q}");
                }
                Err(Error::BudgetExceeded(b)) => assert_eq!(b.kind, BudgetKind::Memory, "{q}"),
                Err(e) => panic!("{q}: {e}"),
            }
        }
        assert_eq!(spill.reserved_bytes(), 0, "{q}");
    }
}
