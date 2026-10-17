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
