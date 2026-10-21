//! External sorts that spill sorted runs to disk under a small memory budget (X08 §6.4).
use oxrdf::Term;
use oxrdfio::RdfFormat;
use sparkles_core::io::Source;
use sparkles_core::sparql::{
    CursorOptions, CursorStatus, ExecutionMode, FallbackPolicy, QueryExecution, QueryOptions,
    query, query_execution, select_cursor,
};
use sparkles_core::spill::{Spill, SpillOptions};
use sparkles_core::store::{Store, StoreOptions};
use sparkles_core::{BudgetKind, Error};
use std::sync::Arc;

/// Objects of every kind, with many ties, in an order unrelated to the sort.
fn store(n: usize) -> Store {
    let s = Store::in_memory(StoreOptions::default());
    let data = (0..n)
        .map(|i| {
            let k = (i * 7919) % n;
            let o = match k % 6 {
                0 => format!("{}", k % 1000),
                1 => format!("\"s{}\"", k % 777),
                2 => format!("\"t{}\"@en", k % 333),
                3 => format!("<urn:o:{}>", k % 555),
                4 => format!(
                    "\"{}.5\"^^<http://www.w3.org/2001/XMLSchema#decimal>",
                    k % 100
                ),
                _ => format!(
                    "\"2020-01-{:02}\"^^<http://www.w3.org/2001/XMLSchema#date>",
                    k % 28 + 1
                ),
            };
            let mut line = format!("<urn:s:{i}> <urn:p> {o} .\n");
            if i % 3 == 0 {
                line.push_str(&format!("<urn:s:{i}> <urn:q> {} .\n", i % 50));
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

fn cursor_options() -> CursorOptions {
    CursorOptions {
        batch_rows: 1000,
        fallback: FallbackPolicy::RejectMaterialization,
        ..Default::default()
    }
}

/// The rows of `q` in order, from a cursor under `memory` bytes with `spill`.
fn run(
    s: &Store,
    q: &str,
    memory: u64,
    spill: Option<Arc<Spill>>,
    stop_after: Option<usize>,
) -> Result<Vec<Vec<Option<Term>>>, Error> {
    let opts = QueryOptions {
        max_memory_bytes: Some(memory),
        spill,
        ..Default::default()
    };
    let mut c = select_cursor(s.snapshot(), q, &opts, &cursor_options())?;
    let mut rows = Vec::new();
    while let Some(b) = c.next_batch()? {
        for i in 0..b.len() {
            rows.push(b.row(i).unwrap());
        }
        if stop_after.is_some_and(|n| rows.len() >= n) {
            return Ok(rows);
        }
    }
    assert_eq!(c.status(), CursorStatus::Complete);
    assert!(c.stats().mem_peak_bytes <= memory);
    Ok(rows)
}

/// The names in `dir`'s process directories other than their markers.
fn leftovers(dir: &std::path::Path) -> Vec<String> {
    let mut names = Vec::new();
    for process in std::fs::read_dir(dir).unwrap() {
        let process = process.unwrap().path();
        if process.is_dir() {
            for f in std::fs::read_dir(&process).unwrap() {
                let name = f.unwrap().file_name().to_string_lossy().into_owned();
                if name != "SPARKLES-SPILL" {
                    names.push(name);
                }
            }
        }
    }
    names
}

const MEMORY: u64 = 768 << 10;

#[test]
fn numbered_distinct_can_spill_partially_ordered_scan_input() {
    let s = store(60_000);
    let tmp = tempfile::tempdir().unwrap();
    let spill = spill(tmp.path());
    let q = "SELECT DISTINCT ?s ?v { ?s <urn:p> ?o BIND(STR(?o) AS ?v) } ORDER BY DESC(?v) ?s";
    let expected = query(s.snapshot(), q, &Default::default()).unwrap().rows();
    let actual = run(&s, q, MEMORY, Some(spill.clone()), None).unwrap();
    assert_eq!(actual, expected);
    assert!(spill.written_bytes() > 0);
    assert_eq!(spill.reserved_bytes(), 0);
    assert!(leftovers(tmp.path()).is_empty());
}

#[test]
fn spilled_order_by_matches_eager_order_with_ties_and_expressions() {
    let s = store(60_000);
    let tmp = tempfile::tempdir().unwrap();
    let spill = spill(tmp.path());
    // Each query, and whether its keys order the rows totally.
    for (q, total) in [
        ("SELECT ?s ?o WHERE { ?s <urn:p> ?o } ORDER BY ?o", false),
        (
            "SELECT ?s ?o WHERE { ?s <urn:p> ?o } ORDER BY DESC(?o) ?s",
            true,
        ),
        (
            "SELECT ?s ?o ?x WHERE { ?s <urn:p> ?o OPTIONAL { ?s <urn:q> ?x } } ORDER BY ?x DESC(?o)",
            false,
        ),
        (
            "SELECT ?s ?o WHERE { ?s <urn:p> ?o } ORDER BY (STR(?o)) DESC(?s)",
            true,
        ),
    ] {
        let expected = query(s.snapshot(), q, &Default::default()).unwrap().rows();
        // Without a place to spill the cursor runs out of memory.
        match run(&s, q, MEMORY, None, None) {
            Err(Error::BudgetExceeded(b)) => assert!(
                matches!(b.kind, BudgetKind::Memory | BudgetKind::ServerMemory),
                "{q}: {b:?}"
            ),
            other => panic!(
                "{q}: expected a memory budget error, got {:?}",
                other.map(|r| r.len())
            ),
        }
        let rows = run(&s, q, MEMORY, Some(spill.clone()), None).unwrap();
        // Equal keys may come in another order than eager input order, so only a
        // total order must match row for row. Every order must hold the same bag.
        assert_eq!(rows.len(), expected.len(), "{q}");
        let mut a = rows.iter().map(|r| format!("{r:?}")).collect::<Vec<_>>();
        let mut b = expected
            .iter()
            .map(|r| format!("{r:?}"))
            .collect::<Vec<_>>();
        if total {
            assert_eq!(a, b, "{q}");
        }
        a.sort();
        b.sort();
        assert_eq!(a, b, "{q}");
        assert_eq!(spill.reserved_bytes(), 0, "{q}");
        assert!(leftovers(tmp.path()).is_empty(), "{q}");
    }
    assert!(spill.spilled_queries() > 0);
}

#[test]
fn spilled_order_by_keeps_a_total_order_on_the_sort_keys() {
    let s = store(40_000);
    let tmp = tempfile::tempdir().unwrap();
    let spill = spill(tmp.path());
    let q = "SELECT ?o ?s WHERE { ?s <urn:p> ?o } ORDER BY ?o";
    let expected = query(s.snapshot(), q, &Default::default()).unwrap().rows();
    let rows = run(&s, q, MEMORY, Some(spill.clone()), None).unwrap();
    let keys = |rows: &[Vec<Option<Term>>]| {
        rows.iter()
            .map(|r| format!("{:?}", r[0]))
            .collect::<Vec<_>>()
    };
    assert_eq!(keys(&rows), keys(&expected));
}

/// A batch that cannot be copied into an empty sort buffer becomes a run by itself, so a
/// sort whose input batches are large next to the room left still spills (item 85).
#[test]
fn a_batch_too_large_to_buffer_spills_as_its_own_run() {
    let s = store(60_000);
    let tmp = tempfile::tempdir().unwrap();
    let spill = spill(tmp.path());
    let q = "SELECT ?s ?o WHERE { ?s <urn:p> ?o } ORDER BY DESC(?o) ?s";
    let expected = query(s.snapshot(), q, &Default::default()).unwrap().rows();
    let opts = QueryOptions {
        max_memory_bytes: Some(MEMORY),
        spill: Some(spill.clone()),
        ..Default::default()
    };
    let cursor = CursorOptions {
        batch_rows: 20_000,
        ..cursor_options()
    };
    let mut c = select_cursor(s.snapshot(), q, &opts, &cursor).unwrap();
    let mut rows = Vec::new();
    while let Some(b) = c.next_batch().unwrap() {
        for i in 0..b.len() {
            rows.push(b.row(i).unwrap());
        }
    }
    assert!(c.stats().mem_peak_bytes <= MEMORY);
    drop(c);
    assert_eq!(rows, expected);
    assert_eq!(spill.reserved_bytes(), 0);
    assert!(leftovers(tmp.path()).is_empty());
}

#[test]
fn spilled_sorts_remove_their_files_when_dropped_early() {
    let s = store(60_000);
    let tmp = tempfile::tempdir().unwrap();
    let spill = spill(tmp.path());
    let q = "SELECT ?s ?o WHERE { ?s <urn:p> ?o } ORDER BY DESC(?o) ?s";
    let rows = run(&s, q, MEMORY, Some(spill.clone()), Some(1500)).unwrap();
    assert!(rows.len() >= 1500);
    assert_eq!(spill.reserved_bytes(), 0);
    assert!(leftovers(tmp.path()).is_empty());
    drop(spill);
    // The process directory goes with the last handle.
    assert_eq!(std::fs::read_dir(tmp.path()).unwrap().count(), 0);
}

#[test]
fn spill_limits_fail_the_query_with_a_spill_budget() {
    let s = store(60_000);
    let tmp = tempfile::tempdir().unwrap();
    let spill = Spill::open(&SpillOptions {
        dir: tmp.path().to_path_buf(),
        max_bytes: 4 << 30,
        query_bytes: 4 << 30,
        min_free_bytes: 0,
        encrypted: false,
    })
    .unwrap();
    let q = "SELECT ?s ?o WHERE { ?s <urn:p> ?o } ORDER BY DESC(?o) ?s";
    let opts = QueryOptions {
        max_memory_bytes: Some(MEMORY),
        spill: Some(spill.clone()),
        max_spill_bytes: Some(1024),
        ..Default::default()
    };
    let error = (|| -> Result<(), Error> {
        let mut c = select_cursor(s.snapshot(), q, &opts, &cursor_options())?;
        while c.next_batch()?.is_some() {}
        Ok(())
    })()
    .unwrap_err();
    assert!(
        matches!(&error, Error::BudgetExceeded(b) if b.kind == BudgetKind::Spill),
        "{error:?}"
    );
    assert_eq!(spill.reserved_bytes(), 0);
    assert!(leftovers(tmp.path()).is_empty());
}

#[test]
fn small_sorts_with_spill_never_touch_the_disk() {
    let s = store(2_000);
    let tmp = tempfile::tempdir().unwrap();
    let spill = spill(tmp.path());
    let q = "SELECT ?s ?o WHERE { ?s <urn:p> ?o } ORDER BY DESC(?o) ?s";
    let expected = query(s.snapshot(), q, &Default::default()).unwrap().rows();
    let rows = run(&s, q, 64 << 20, Some(spill.clone()), None).unwrap();
    assert_eq!(
        rows.iter().map(|r| format!("{r:?}")).collect::<Vec<_>>(),
        expected
            .iter()
            .map(|r| format!("{r:?}"))
            .collect::<Vec<_>>()
    );
    assert_eq!(spill.spilled_queries(), 0);
    assert_eq!(spill.written_bytes(), 0);
}

#[test]
fn many_runs_merge_in_passes_and_report_them_in_the_plan() {
    let s = store(150_000);
    let tmp = tempfile::tempdir().unwrap();
    let spill = spill(tmp.path());
    let q = "SELECT ?s ?o WHERE { ?s <urn:p> ?o } ORDER BY DESC(?o) ?s";
    let expected = query(s.snapshot(), q, &Default::default()).unwrap().rows();
    let opts = QueryOptions {
        max_memory_bytes: Some(MEMORY),
        spill: Some(spill.clone()),
        ..Default::default()
    };
    let mut c = select_cursor(s.snapshot(), q, &opts, &cursor_options()).unwrap();
    let mut rows = Vec::new();
    while let Some(b) = c.next_batch().unwrap() {
        for i in 0..b.len() {
            rows.push(b.row(i).unwrap());
        }
    }
    assert_eq!(
        rows.iter().map(|r| format!("{r:?}")).collect::<Vec<_>>(),
        expected
            .iter()
            .map(|r| format!("{r:?}"))
            .collect::<Vec<_>>()
    );
    let text = c.plan_json().unwrap();
    let passes = text
        .split("\"mergePasses\":")
        .nth(1)
        .and_then(|rest| rest.split([',', '}']).next())
        .and_then(|n| n.trim().parse::<u64>().ok())
        .unwrap_or_else(|| panic!("no mergePasses in {text}"));
    assert!(passes >= 2, "{text}");
    assert!(text.contains("\"spilledBytes\""), "{text}");
    drop(c);
    assert_eq!(spill.reserved_bytes(), 0);
    assert!(leftovers(tmp.path()).is_empty());
}

#[test]
fn impure_keys_spill_with_the_values_their_runs_sorted() {
    let s = store(60_000);
    let tmp = tempfile::tempdir().unwrap();
    let spill = spill(tmp.path());
    let q = "SELECT ?s ?o WHERE { ?s <urn:p> ?o } ORDER BY (RAND()) ?s";
    let expected = query(s.snapshot(), q, &Default::default()).unwrap().rows();
    // The random keys are interned for the whole query, so they need memory that the
    // spilled runs do not free.
    let opts = QueryOptions {
        max_memory_bytes: Some(16 << 20),
        spill: Some(spill.clone()),
        ..Default::default()
    };
    let mut c = select_cursor(s.snapshot(), q, &opts, &cursor_options()).unwrap();
    let mut rows = Vec::new();
    while let Some(b) = c.next_batch().unwrap() {
        for i in 0..b.len() {
            rows.push(b.row(i).unwrap());
        }
    }
    drop(c);
    let bag = |rows: &[Vec<Option<Term>>]| {
        let mut rows = rows.iter().map(|r| format!("{r:?}")).collect::<Vec<_>>();
        rows.sort();
        rows
    };
    assert_eq!(bag(&rows), bag(&expected));
    assert!(spill.written_bytes() > 0);
    assert!(leftovers(tmp.path()).is_empty());
}

#[test]
fn cancelled_spilled_sorts_remove_their_files() {
    let s = store(60_000);
    let tmp = tempfile::tempdir().unwrap();
    let spill = spill(tmp.path());
    let cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let opts = QueryOptions {
        max_memory_bytes: Some(MEMORY),
        spill: Some(spill.clone()),
        cancel: Some(cancel.clone()),
        ..Default::default()
    };
    let q = "SELECT ?s ?o WHERE { ?s <urn:p> ?o } ORDER BY DESC(?o) ?s";
    let mut c = select_cursor(s.snapshot(), q, &opts, &cursor_options()).unwrap();
    assert!(c.next_batch().unwrap().is_some());
    assert!(spill.reserved_bytes() > 0);
    cancel.store(true, std::sync::atomic::Ordering::Relaxed);
    assert!(c.next_batch().is_err());
    drop(c);
    assert_eq!(spill.reserved_bytes(), 0);
    assert!(leftovers(tmp.path()).is_empty());
}

type Rows = Vec<Vec<Option<Term>>>;

/// The rows of an automatic execution, and its cursor plan when it ran as a cursor.
fn automatic(s: &Store, q: &str, opts: &QueryOptions) -> Result<(Rows, Option<String>), Error> {
    match query_execution(
        s.snapshot(),
        q,
        opts,
        &CursorOptions::default(),
        ExecutionMode::Auto,
    )? {
        QueryExecution::Select(mut c) => {
            let mut rows = Vec::new();
            while let Some(b) = c.next_batch()? {
                for i in 0..b.len() {
                    rows.push(b.row(i).unwrap());
                }
            }
            Ok((rows, Some(c.plan_json()?)))
        }
        QueryExecution::Eager(r) => Ok((r.into_result().rows(), None)),
        _ => unreachable!("a SELECT"),
    }
}

#[test]
fn automatic_execution_streams_or_retries_sorts_that_outgrow_memory() {
    let s = store(60_000);
    let tmp = tempfile::tempdir().unwrap();
    let spill = spill(tmp.path());
    let q = "SELECT ?s ?o WHERE { ?s <urn:p> ?o } ORDER BY DESC(?o) ?s";
    let expected = query(s.snapshot(), q, &Default::default()).unwrap().rows();
    let opts = QueryOptions {
        max_memory_bytes: Some(MEMORY),
        spill: Some(spill.clone()),
        ..Default::default()
    };
    // Eager execution would hold more than half the budget, so the query streams.
    let (rows, plan) = automatic(&s, q, &opts).unwrap();
    assert_eq!(rows, expected);
    let plan = plan.expect("a cursor");
    assert!(!plan.contains("eagerMemoryRetry"), "{plan}");
    assert!(plan.contains("spilledBytes"), "{plan}");
    // Without spill the query stays eager and fails within its budget.
    let without = QueryOptions {
        spill: None,
        ..opts.clone()
    };
    assert!(matches!(
        automatic(&s, q, &without),
        Err(Error::BudgetExceeded(b)) if b.kind == BudgetKind::Memory
    ));
    // A store with uncompacted changes keeps automatic execution eager, and the eager
    // run that exceeds its budget runs again as a cursor.
    sparkles_core::sparql::update::update(
        &s,
        "INSERT DATA { <urn:x> <urn:q> 1 }",
        &Default::default(),
    )
    .unwrap();
    let (rows, plan) = automatic(&s, q, &opts).unwrap();
    assert_eq!(rows, expected);
    let plan = plan.expect("a retried cursor");
    assert!(plan.contains("\"eagerMemoryRetry\":1"), "{plan}");
    assert_eq!(spill.eager_retries(), 1);
    assert_eq!(spill.reserved_bytes(), 0);
    assert!(leftovers(tmp.path()).is_empty());
    // A small sort stays eager.
    let small = "SELECT ?s ?o WHERE { ?s <urn:p> ?o FILTER(?s = <urn:s:5>) } ORDER BY ?o";
    let (_, plan) = automatic(&s, small, &opts).unwrap();
    assert!(plan.is_none());
}

/// A query that calls a SERVICE anywhere, even inside an EXISTS, is not run a second
/// time after its eager run exceeds the memory budget, since the retry would call the
/// service again.
#[test]
fn automatic_execution_never_retries_a_query_that_calls_a_service() {
    use std::io::{Read, Write};
    use std::sync::atomic::{AtomicUsize, Ordering};
    let s = store(60_000);
    // A store with uncompacted changes keeps automatic execution eager.
    sparkles_core::sparql::update::update(
        &s,
        "INSERT DATA { <urn:x> <urn:q> 1 }",
        &Default::default(),
    )
    .unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let spill = spill(tmp.path());
    // An endpoint that counts its calls and answers each with a server error.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let endpoint = format!("http://{}/sparql", listener.local_addr().unwrap());
    let calls = Arc::new(AtomicUsize::new(0));
    let counted = calls.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { break };
            counted.fetch_add(1, Ordering::SeqCst);
            let mut buf = [0u8; 4096];
            let _ = stream.read(&mut buf);
            let _ = stream.write_all(
                b"HTTP/1.1 500 Internal Server Error\r\ncontent-length: 0\r\nconnection: close\r\n\r\n",
            );
        }
    });
    let opts = QueryOptions {
        max_memory_bytes: Some(3 * MEMORY / 2),
        spill: Some(spill.clone()),
        allow_service: true,
        outbound: sparkles_core::outbound::OutboundPolicy {
            allow_private: true,
            ..Default::default()
        },
        ..Default::default()
    };
    for q in [
        // The EXISTS calls the service for one row only.
        format!(
            "SELECT ?s ?o WHERE {{ ?s <urn:p> ?o \
             FILTER (?s != <urn:s:1> || EXISTS {{ SERVICE SILENT <{endpoint}> {{ ?s <urn:b> ?x }} }}) }} \
             ORDER BY DESC(?o) ?s"
        ),
        format!(
            "SELECT ?s ?o WHERE {{ ?s <urn:p> ?o \
             FILTER (?s != <urn:s:1> || NOT EXISTS {{ SERVICE SILENT <{endpoint}> {{ ?s <urn:b> ?x }} }}) }} \
             ORDER BY DESC(?o) ?s"
        ),
    ] {
        // The calls of the eager run alone, without spill and so without a retry.
        let before = calls.load(Ordering::SeqCst);
        let without = QueryOptions {
            spill: None,
            ..opts.clone()
        };
        assert!(matches!(
            automatic(&s, &q, &without),
            Err(Error::BudgetExceeded(b)) if b.kind == BudgetKind::Memory
        ));
        let eager_calls = calls.load(Ordering::SeqCst) - before;
        assert!(eager_calls >= 1, "{q}: the service was never called");
        let before = calls.load(Ordering::SeqCst);
        match automatic(&s, &q, &opts) {
            Err(Error::BudgetExceeded(b)) => assert_eq!(b.kind, BudgetKind::Memory, "{q}"),
            Ok((rows, plan)) => panic!(
                "{q}: expected the eager memory failure, got {} rows from {plan:?}",
                rows.len()
            ),
            Err(e) => panic!("{q}: {e}"),
        }
        assert_eq!(calls.load(Ordering::SeqCst) - before, eager_calls, "{q}");
    }
    assert_eq!(spill.eager_retries(), 0);
    assert_eq!(spill.reserved_bytes(), 0);
}
