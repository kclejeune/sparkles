//! A SERVICE without SILENT under the strict cursor policy reads its endpoint's answer a
//! batch at a time (item 829, group 1). The endpoint here is a scripted HTTP server that
//! can hold the rest of a response back, cut it short or fail it, so the tests can see
//! when the cursor reads.

use oxrdf::Term;
use oxrdfio::RdfFormat;
use sparkles_core::io::Source;
use sparkles_core::outbound::OutboundPolicy;
use sparkles_core::sparql::{
    CursorOptions, CursorPlan, ExecutionMode, FallbackPolicy, LocalEndpoints, LocalTarget,
    QueryExecution, QueryOptions, query, query_execution, select_cursor,
};
use sparkles_core::store::{Snapshot, Store, StoreOptions};
use sparkles_core::{BudgetKind, Error, Result};
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

/// A gate the endpoint waits at in the middle of a response, for at most ten seconds.
#[derive(Default)]
struct Gate {
    open: Mutex<bool>,
    cv: Condvar,
}

impl Gate {
    fn open(&self) {
        *self.open.lock().unwrap() = true;
        self.cv.notify_all();
    }

    fn wait(&self) {
        let open = self.open.lock().unwrap();
        let _ = self
            .cv
            .wait_timeout_while(open, Duration::from_secs(10), |open| !*open)
            .unwrap();
    }
}

/// One piece of a response: bytes to write, or a gate to wait at first.
enum Part {
    Bytes(Vec<u8>),
    Wait(Arc<Gate>),
}

type Script = Arc<dyn Fn() -> (u16, Vec<Part>) + Send + Sync>;

/// An endpoint that answers every request with what `script` gives, without a content
/// length, so the client reads the body until the connection closes.
struct Endpoint {
    url: String,
    calls: Arc<AtomicUsize>,
}

impl Endpoint {
    fn start(script: Script) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/sparql", listener.local_addr().unwrap());
        let calls = Arc::new(AtomicUsize::new(0));
        let counted = calls.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { break };
                counted.fetch_add(1, Ordering::SeqCst);
                let script = script.clone();
                std::thread::spawn(move || {
                    let _ = stream.set_read_timeout(Some(Duration::from_secs(10)));
                    let mut request = Vec::new();
                    let mut chunk = [0u8; 8192];
                    // the head and the form body, as its content length gives it
                    let complete = |r: &[u8]| {
                        let Some(end) = r.windows(4).position(|w| w == b"\r\n\r\n") else {
                            return false;
                        };
                        let head = String::from_utf8_lossy(&r[..end]).to_lowercase();
                        let len: usize = head
                            .lines()
                            .find_map(|l| l.strip_prefix("content-length:"))
                            .and_then(|v| v.trim().parse().ok())
                            .unwrap_or(0);
                        r.len() >= end + 4 + len
                    };
                    while !complete(&request) {
                        match stream.read(&mut chunk) {
                            Ok(0) | Err(_) => return,
                            Ok(n) => request.extend_from_slice(&chunk[..n]),
                        }
                    }
                    let (status, parts) = script();
                    let head = format!(
                        "HTTP/1.1 {status} X\r\ncontent-type: application/sparql-results+json\r\nconnection: close\r\n\r\n"
                    );
                    if stream.write_all(head.as_bytes()).is_err() {
                        return;
                    }
                    for part in parts {
                        match part {
                            Part::Bytes(b) => {
                                if stream.write_all(&b).and_then(|()| stream.flush()).is_err() {
                                    return;
                                }
                            }
                            Part::Wait(gate) => gate.wait(),
                        }
                    }
                });
            }
        });
        Self { url, calls }
    }

    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

/// The head of a response binding ?s and ?o.
const HEAD: &str = r#"{"head":{"vars":["s","o"]},"results":{"bindings":["#;
const TAIL: &str = "]}}";

/// The binding of row `i`: an IRI subject and a literal object, or the blank node
/// `label` as the object.
fn binding(i: usize, label: Option<&str>) -> String {
    let o = match label {
        Some(l) => format!(r#"{{"type":"bnode","value":"{l}"}}"#),
        None => format!(r#"{{"type":"literal","value":"value {i}"}}"#),
    };
    format!(r#"{{"s":{{"type":"uri","value":"urn:s:{i}"}},"o":{o}}}"#)
}

fn rows(n: usize) -> String {
    (0..n)
        .map(|i| binding(i, None))
        .collect::<Vec<_>>()
        .join(",")
}

/// A complete answer of `n` rows.
fn answer(n: usize) -> Script {
    let body = format!("{HEAD}{}{TAIL}", rows(n));
    Arc::new(move || (200, vec![Part::Bytes(body.clone().into_bytes())]))
}

fn store() -> Store {
    let s = Store::in_memory(StoreOptions::default());
    s.load(&[Source::from_bytes(
        b"<urn:a> <urn:b> 1 .\n".to_vec(),
        RdfFormat::Turtle,
        None,
    )])
    .unwrap();
    s
}

fn opts() -> QueryOptions {
    QueryOptions {
        allow_service: true,
        no_cache: true,
        outbound: OutboundPolicy {
            allow_private: true,
            ..Default::default()
        },
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

/// The rows of an eager run, in order.
fn eager(snap: &Arc<Snapshot>, q: &str, o: &QueryOptions) -> Result<Vec<Vec<Option<Term>>>> {
    Ok(query(snap.clone(), q, o)?
        .rows()?
        .iter()
        .map(|r| r.values().to_vec())
        .collect())
}

/// The rows of a cursor, in order, and its plan once drained.
fn streamed(
    snap: &Arc<Snapshot>,
    q: &str,
    o: &QueryOptions,
    c: &CursorOptions,
) -> Result<(Vec<Vec<Option<Term>>>, CursorPlan)> {
    let mut cursor = select_cursor(snap.clone(), q, o, c)?;
    let mut out = Vec::new();
    while let Some(b) = cursor.next_batch()? {
        for r in 0..b.len() {
            out.push(b.row(r)?.values().to_vec());
        }
    }
    Ok((out, cursor.plan().clone()))
}

fn materializes(plan: &CursorPlan) -> bool {
    plan.materializes || plan.children.iter().any(materializes)
}

fn service_node(plan: &CursorPlan) -> &CursorPlan {
    fn find<'a>(plan: &'a CursorPlan, out: &mut Vec<&'a CursorPlan>) {
        if plan.operator.operator == "Service" {
            out.push(plan);
        }
        for child in &plan.children {
            find(child, out);
        }
    }
    let mut found = Vec::new();
    find(plan, &mut found);
    assert_eq!(
        found.len(),
        1,
        "expected one Service node in plan: {plan:#?}"
    );
    found[0]
}

fn assert_service_streaming(plan: &CursorPlan, phase: &str) {
    let svc = service_node(plan);
    assert!(
        !svc.full_input_before_output
            && !svc.materializes
            && svc.growing_state
            && svc.reason.is_none(),
        "{phase}: expected streaming Service node with growingState, got {svc:#?}"
    );
}

fn assert_service_buffered(plan: &CursorPlan, phase: &str) {
    let svc = service_node(plan);
    assert!(
        svc.full_input_before_output
            && !svc.materializes
            && svc.growing_state
            && svc
                .reason
                .as_deref()
                .is_some_and(|r| r.contains("buffered before output")),
        "{phase}: expected buffered-before-output Service node with growingState and reason, got {svc:#?}"
    );
}

/// The strict cursor hands out its first batch while the endpoint still holds the rest
/// of the response back, and the drained answer is eager's, in eager's order, at every
/// batch size, with no node of the plan materializing.
#[test]
fn a_service_streams_its_first_batch_before_the_answer_ends() {
    let s = store();
    let snap = s.snapshot();
    let gate = Arc::new(Gate::default());
    let held = gate.clone();
    let ep = Endpoint::start(Arc::new(move || {
        (
            200,
            vec![
                Part::Bytes(format!("{HEAD}{},", rows(2)).into_bytes()),
                Part::Wait(held.clone()),
                Part::Bytes(
                    format!(
                        "{}{TAIL}",
                        (2..50)
                            .map(|i| binding(i, None))
                            .collect::<Vec<_>>()
                            .join(",")
                    )
                    .into_bytes(),
                ),
            ],
        )
    }));
    let q = format!("SELECT ?s ?o {{ SERVICE <{}> {{ ?s ?p ?o }} }}", ep.url);
    let began = Instant::now();
    let mut cursor = select_cursor(snap.clone(), &q, &opts(), &strict(1)).unwrap();
    assert_service_streaming(cursor.plan(), "head-first initial");
    let first = cursor.next_batch().unwrap().expect("a first row");
    assert_eq!(first.len(), 1);
    drop(first);
    assert_service_streaming(cursor.plan(), "head-first after first batch");
    assert!(
        began.elapsed() < Duration::from_secs(5),
        "the first batch waited for the rest of the response"
    );
    gate.open();
    let mut n = 1;
    while let Some(b) = cursor.next_batch().unwrap() {
        n += b.len();
    }
    assert_eq!(n, 50);
    assert!(!materializes(cursor.plan()), "{:#?}", cursor.plan());
    assert_service_streaming(cursor.plan(), "head-first drained");
    let expected = eager(&snap, &q, &opts()).unwrap();
    for rows in [1, 2, 3, 4096] {
        let (answer, plan) = streamed(&snap, &q, &opts(), &strict(rows)).unwrap();
        assert_eq!(answer, expected, "{rows}");
        assert!(!materializes(&plan));
    }
}

/// One label of the endpoint's answer is one blank node of the query across batches,
/// and different labels are different blank nodes, as in one eager call.
#[test]
fn a_blank_node_label_is_one_node_across_batches() {
    let s = store();
    let snap = s.snapshot();
    let body = format!(
        "{HEAD}{}{TAIL}",
        [
            binding(0, Some("x")),
            binding(1, Some("y")),
            binding(2, Some("x")),
            binding(3, Some("y")),
        ]
        .join(",")
    );
    let ep = Endpoint::start(Arc::new(move || {
        (200, vec![Part::Bytes(body.clone().into_bytes())])
    }));
    let q = format!("SELECT ?s ?o {{ SERVICE <{}> {{ ?s ?p ?o }} }}", ep.url);
    for answer in [
        eager(&snap, &q, &opts()).unwrap(),
        streamed(&snap, &q, &opts(), &strict(1)).unwrap().0,
    ] {
        let o: Vec<&Option<Term>> = answer.iter().map(|r| &r[1]).collect();
        assert!(matches!(o[0], Some(Term::BlankNode(_))));
        assert_eq!(o[0], o[2]);
        assert_eq!(o[1], o[3]);
        assert_ne!(o[0], o[1]);
    }
}

/// A response that breaks off after some rows fails a non-SILENT cursor after it has
/// handed those rows out, and fails the eager run. A SILENT SERVICE runs natively under
/// the strict policy, buffering its answer under the memory budget so both open-time and
/// mid-stream endpoint failures become one empty solution while policy refusals,
/// cancellation, timeouts, and budget failures still propagate.
#[test]
fn failures_end_the_stream_and_silent_runs_natively() {
    let s = store();
    let snap = s.snapshot();
    let cut = format!("{HEAD}{},{{\"s\":", rows(3));
    let broken = Endpoint::start(Arc::new(move || {
        (200, vec![Part::Bytes(cut.clone().into_bytes())])
    }));
    let q = format!("SELECT ?s ?o {{ SERVICE <{}> {{ ?s ?p ?o }} }}", broken.url);
    assert!(matches!(eager(&snap, &q, &opts()), Err(Error::Service(_))));
    let mut cursor = select_cursor(snap.clone(), &q, &opts(), &strict(1)).unwrap();
    let mut emitted = 0;
    let failure = loop {
        match cursor.next_batch() {
            Ok(Some(b)) => emitted += b.len(),
            Ok(None) => break None,
            Err(e) => break Some(e),
        }
    };
    assert_eq!(emitted, 3);
    assert!(matches!(failure, Some(Error::Service(_))), "{failure:?}");

    // Mid-stream truncation under SILENT SERVICE emits ONLY one empty solution (never the
    // 3 partial rows) and runs natively under the strict policy.
    let silent_broken = q.replace("SERVICE <", "SERVICE SILENT <");
    let unit = vec![vec![None, None]];
    assert_eq!(eager(&snap, &silent_broken, &opts()).unwrap(), unit);
    for batch_rows in [1, 2, 4096] {
        let (got_broken, plan) =
            streamed(&snap, &silent_broken, &opts(), &strict(batch_rows)).unwrap();
        assert_eq!(
            got_broken, unit,
            "mid-stream SILENT batch_rows={batch_rows}"
        );
        assert!(!materializes(&plan));
        let svc = service_node(&plan);
        assert!(svc.full_input_before_output && svc.growing_state && svc.reason.is_some());
    }

    // A clean 0-row SILENT SERVICE emits 0 rows (not an empty solution).
    let empty_ep = Endpoint::start(answer(0));
    let silent_empty = format!(
        "SELECT ?s ?o {{ SERVICE SILENT <{}> {{ ?s ?p ?o }} }}",
        empty_ep.url
    );
    assert!(eager(&snap, &silent_empty, &opts()).unwrap().is_empty());
    let (got_empty, plan) = streamed(&snap, &silent_empty, &opts(), &strict(1)).unwrap();
    assert!(got_empty.is_empty());
    assert!(!materializes(&plan));

    // A succeeding SILENT SERVICE (including late-head JSON) matches eager at all batch sizes.
    let ok_ep = Endpoint::start(answer(15));
    let silent_ok = format!(
        "SELECT ?s ?o {{ SERVICE SILENT <{}> {{ ?s ?p ?o }} }}",
        ok_ep.url
    );
    let expected_ok = eager(&snap, &silent_ok, &opts()).unwrap();
    assert_eq!(expected_ok.len(), 15);
    for batch_rows in [1, 2, 3, 4096] {
        let (got, plan) = streamed(&snap, &silent_ok, &opts(), &strict(batch_rows)).unwrap();
        assert_eq!(got, expected_ok, "batch_rows={batch_rows}");
        assert!(!materializes(&plan));
    }
    let late_ep = Endpoint::start(Arc::new(|| {
        (
            200,
            vec![Part::Bytes(
                format!(
                    r#"{{"results":{{"bindings":[{}]}},"head":{{"vars":["s","o"]}}}}"#,
                    rows(7)
                )
                .into_bytes(),
            )],
        )
    }));
    let silent_late = format!(
        "SELECT ?s ?o {{ SERVICE SILENT <{}> {{ ?s ?p ?o }} }}",
        late_ep.url
    );
    let expected_late = eager(&snap, &silent_late, &opts()).unwrap();
    let (got_late, plan_late) = streamed(&snap, &silent_late, &opts(), &strict(2)).unwrap();
    assert_eq!(got_late, expected_late);
    assert!(!materializes(&plan_late));

    // HTTP 500 under SILENT SERVICE suppresses to one empty binding, also below a join.
    let failing = Endpoint::start(Arc::new(|| (500, vec![Part::Bytes(b"no".to_vec())])));
    let q = format!(
        "SELECT ?s ?o {{ SERVICE <{}> {{ ?s ?p ?o }} }}",
        failing.url
    );
    assert!(matches!(eager(&snap, &q, &opts()), Err(Error::Service(_))));
    let r = streamed(&snap, &q, &opts(), &strict(1));
    assert!(matches!(r, Err(Error::Service(_))), "{:?}", r.map(|r| r.0));

    let silent = q.replace("SERVICE <", "SERVICE SILENT <");
    assert_eq!(eager(&snap, &silent, &opts()).unwrap(), unit);
    let (got, plan) = streamed(&snap, &silent, &opts(), &strict(1)).unwrap();
    assert_eq!(got, unit);
    assert!(!materializes(&plan));
    // the empty solution has the SERVICE's variables, unbound, also below a join
    let joined = format!(
        "SELECT ?a ?s ?o {{ ?a <urn:b> ?v SERVICE SILENT <{}> {{ ?s ?p ?o }} }}",
        failing.url
    );
    let expected = eager(&snap, &joined, &opts()).unwrap();
    assert_eq!(expected.len(), 1);
    let (got_joined, plan_joined) = streamed(&snap, &joined, &opts(), &strict(1)).unwrap();
    assert_eq!(got_joined, expected);
    assert!(!materializes(&plan_joined));

    // the policy's refusals come at the first pull, as in an eager run, and SILENT does
    // not hide them
    let ok = Endpoint::start(answer(2));
    let q = format!("SELECT ?s ?o {{ SERVICE <{}> {{ ?s ?p ?o }} }}", ok.url);
    let denied = QueryOptions {
        forbid_service: true,
        ..opts()
    };
    for q in [q.clone(), q.replace("SERVICE <", "SERVICE SILENT <")] {
        assert!(matches!(
            eager(&snap, &q, &denied),
            Err(Error::NotPermitted(_))
        ));
        let r = streamed(&snap, &q, &denied, &strict(1));
        assert!(
            matches!(r, Err(Error::NotPermitted(_))),
            "{:?}",
            r.map(|r| r.0)
        );
    }
    // Cancellation, expired deadline, row budget, and memory budget are never suppressed
    // by SILENT SERVICE.
    let silent_q = q.replace("SERVICE <", "SERVICE SILENT <");
    {
        let cancel = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let o = QueryOptions {
            cancel: Some(cancel),
            ..opts()
        };
        let r = streamed(&snap, &silent_q, &o, &strict(1));
        assert!(matches!(r, Err(Error::Cancelled)), "{:?}", r.map(|r| r.0));
    }
    {
        let o = QueryOptions {
            deadline: Some(Instant::now() - Duration::from_secs(1)),
            ..opts()
        };
        let r = streamed(&snap, &silent_q, &o, &strict(1));
        assert!(matches!(r, Err(Error::Timeout)), "{:?}", r.map(|r| r.0));
    }
    {
        let o = QueryOptions {
            max_rows: Some(1),
            ..opts()
        };
        let r = streamed(&snap, &silent_ok, &o, &strict(1));
        assert!(
            matches!(r, Err(Error::BudgetExceeded(ref b)) if b.kind == BudgetKind::Rows),
            "{:?}",
            r.map(|r| r.0)
        );
    }
    {
        let big_row = r#"{"s":{"type":"uri","value":"urn:s"},"o":{"type":"uri","value":"urn:o"}}"#;
        let big_body = format!("{HEAD}{}{TAIL}", vec![big_row; 50_000].join(","));
        let big_ep = Endpoint::start(Arc::new(move || {
            (200, vec![Part::Bytes(big_body.clone().into_bytes())])
        }));
        let big_silent = format!(
            "SELECT ?s ?o {{ SERVICE SILENT <{}> {{ ?s ?p ?o }} }}",
            big_ep.url
        );
        let tight = QueryOptions {
            max_memory_bytes: Some(256 << 10),
            ..opts()
        };
        let r = streamed(&snap, &big_silent, &tight, &strict(64));
        assert!(
            matches!(r, Err(Error::BudgetExceeded(ref b)) if b.kind == BudgetKind::Memory),
            "{:?}",
            r.map(|r| r.0)
        );
    }
    let off = QueryOptions {
        allow_service: false,
        ..opts()
    };
    assert!(matches!(eager(&snap, &q, &off), Err(Error::Service(_))));
    let r = streamed(&snap, &q, &off, &strict(1));
    assert!(matches!(r, Err(Error::Service(_))), "{:?}", r.map(|r| r.0));
    assert_eq!(ok.calls(), 0, "a refused SERVICE sends nothing");
}

/// The streamed SERVICE counts the rows an eager run counts, and a work limit one row
/// short of that count refuses both.
#[test]
fn a_service_counts_its_rows_as_an_eager_run_does() {
    let s = store();
    let snap = s.snapshot();
    let ep = Endpoint::start(answer(30));
    let q = format!(
        "SELECT ?s ?o {{ SERVICE <{}> {{ ?s ?p ?o }} ?a <urn:b> ?v }}",
        ep.url
    );
    let with = |limit: Option<u64>, work: Option<Arc<AtomicU64>>| QueryOptions {
        max_rows_produced: limit,
        work,
        ..opts()
    };
    let produced = query(snap.clone(), &q, &with(None, None))
        .unwrap()
        .rows_produced;
    assert!(produced >= 30);
    for rows in [1, 7, 4096] {
        let work = Arc::new(AtomicU64::new(0));
        let (answer, _) =
            streamed(&snap, &q, &with(None, Some(work.clone())), &strict(rows)).unwrap();
        assert_eq!(answer.len(), 30);
        assert_eq!(work.load(Ordering::Relaxed), produced, "{rows}");
        assert!(streamed(&snap, &q, &with(Some(produced), None), &strict(rows)).is_ok());
        let r = streamed(&snap, &q, &with(Some(produced - 1), None), &strict(rows));
        assert!(
            matches!(&r, Err(Error::BudgetExceeded(b)) if b.kind == BudgetKind::RowsProduced),
            "{rows}: {:?}",
            r.map(|r| r.0.len())
        );
    }
    let r = query(snap.clone(), &q, &with(Some(produced - 1), None));
    assert!(matches!(r, Err(Error::BudgetExceeded(b)) if b.kind == BudgetKind::RowsProduced));
}

/// The cursor holds one batch of the answer, not all of it: a memory budget that cannot
/// hold the whole answer refuses the eager run, and the streamed run answers under it.
#[test]
fn a_service_answers_under_a_budget_its_whole_answer_outgrows() {
    let s = store();
    let snap = s.snapshot();
    // the same two terms in every row, so the query's own terms do not grow
    let row = r#"{"s":{"type":"uri","value":"urn:s"},"o":{"type":"uri","value":"urn:o"}}"#;
    let n = 200_000;
    let body = format!("{HEAD}{}{TAIL}", vec![row; n].join(","));
    let ep = Endpoint::start(Arc::new(move || {
        (200, vec![Part::Bytes(body.clone().into_bytes())])
    }));
    let q = format!("SELECT ?s ?o {{ SERVICE <{}> {{ ?s ?p ?o }} }}", ep.url);
    let tight = QueryOptions {
        max_memory_bytes: Some(1 << 20),
        ..opts()
    };
    let r = query(snap.clone(), &q, &tight).map(|r| r.len());
    assert!(
        matches!(&r, Err(Error::BudgetExceeded(b)) if b.kind == BudgetKind::Memory),
        "eager: {r:?}"
    );
    let mut cursor = select_cursor(snap.clone(), &q, &tight, &strict(1024)).unwrap();
    let mut rows = 0;
    while let Some(b) = cursor.next_batch().unwrap() {
        rows += b.len();
    }
    assert_eq!(rows, n);
}

/// A cached SERVICE fills the cache only once its answer has been read to the end. A
/// consumer that stops early leaves no entry, and a drained run's entry answers the
/// next run, streamed or eager, without a request.
#[test]
fn a_service_is_cached_only_when_drained() {
    let s = store();
    let snap = s.snapshot();
    let ep = Endpoint::start(answer(20));
    let q = format!(
        "SELECT ?s ?o {{ SERVICE <cache:{}> {{ ?s ?p ?o }} }}",
        ep.url
    );
    let cached = QueryOptions {
        no_cache: false,
        ..opts()
    };
    let expected = eager(&snap, &q, &opts()).unwrap();
    assert_eq!(ep.calls(), 1);
    let service = &s.result_cache().service;
    service.clear();
    // stopped after one batch
    {
        let mut cursor = select_cursor(snap.clone(), &q, &cached, &strict(1)).unwrap();
        assert_eq!(cursor.next_batch().unwrap().unwrap().len(), 1);
        cursor.close();
    }
    assert_eq!(ep.calls(), 2);
    assert_eq!(service.entries(), 0, "an early stop filled the cache");
    let (answer, plan) = streamed(&snap, &q, &cached, &strict(3)).unwrap();
    assert_eq!(answer, expected);
    assert!(!materializes(&plan));
    assert_eq!(ep.calls(), 3);
    assert_eq!(service.entries(), 1);
    {
        let mut cursor = select_cursor(snap.clone(), &q, &cached, &strict(3)).unwrap();
        assert_service_streaming(cursor.plan(), "cache hit initial");
        let first = cursor.next_batch().unwrap().expect("first cached batch");
        assert_eq!(first.len(), 3);
        drop(first);
        assert_service_streaming(cursor.plan(), "cache hit after first batch");
        let mut n = 3;
        while let Some(b) = cursor.next_batch().unwrap() {
            n += b.len();
        }
        assert_eq!(n, expected.len());
        assert_service_streaming(cursor.plan(), "cache hit drained");
        assert_eq!(ep.calls(), 3, "the cache answered");
    }
    for rows in [1, 4096] {
        assert_eq!(
            streamed(&snap, &q, &cached, &strict(rows)).unwrap().0,
            expected
        );
    }
    assert_eq!(eager(&snap, &q, &cached).unwrap(), expected);
    assert_eq!(ep.calls(), 3, "the cache answered");
    // `cache+clear` drops the entry and asks again
    let clear = q.replace("<cache:", "<cache+clear:");
    assert_eq!(
        streamed(&snap, &clear, &cached, &strict(4)).unwrap().0,
        expected
    );
    assert_eq!(ep.calls(), 4);
    // An entry seeded by a permissive late-head run does not mark a later cache hit as
    // full-input: reading the cached entry does not reparse or buffer the response.
    let late = Endpoint::start(Arc::new(|| {
        (
            200,
            vec![Part::Bytes(
                format!(
                    r#"{{"results":{{"bindings":[{}]}},"head":{{"vars":["s","o"]}}}}"#,
                    rows(10)
                )
                .into_bytes(),
            )],
        )
    }));
    let q_late = format!(
        "SELECT ?s ?o {{ SERVICE <cache:{}> {{ ?s ?p ?o }} }}",
        late.url
    );
    let allow = CursorOptions {
        fallback: FallbackPolicy::AllowMaterialization,
        ..strict(3)
    };
    let (seeded, seed_plan) = streamed(&snap, &q_late, &cached, &allow).unwrap();
    assert_eq!(seeded.len(), 10);
    assert_eq!(late.calls(), 1);
    assert_service_buffered(&seed_plan, "late-head cache seed");
    let mut hit_cursor = select_cursor(snap.clone(), &q_late, &cached, &strict(3)).unwrap();
    assert_service_streaming(hit_cursor.plan(), "late-head cache hit initial");
    let first = hit_cursor
        .next_batch()
        .unwrap()
        .expect("first late-head cached batch");
    assert_eq!(first.len(), 3);
    drop(first);
    assert_service_streaming(hit_cursor.plan(), "late-head cache hit after first batch");
    let mut n = 3;
    while let Some(b) = hit_cursor.next_batch().unwrap() {
        n += b.len();
    }
    assert_eq!(n, 10);
    assert_service_streaming(hit_cursor.plan(), "late-head cache hit drained");
    assert_eq!(late.calls(), 1, "strict cache hit sent no new request");
}

/// A remote transfer's timeout counts from the start of its request, while the cursor's
/// deadline leaves out the time its consumer keeps it waiting. A consumer slower than
/// the outbound timeout therefore fails a transfer that an eager run finishes, with the
/// timeout as a SERVICE error.
#[test]
fn a_slow_consumer_can_expire_a_remote_transfer() {
    let s = store();
    let snap = s.snapshot();
    let ep = Endpoint::start(answer(5_000));
    let q = format!("SELECT ?s ?o {{ SERVICE <{}> {{ ?s ?p ?o }} }}", ep.url);
    let short = QueryOptions {
        outbound: OutboundPolicy {
            allow_private: true,
            timeout: Duration::from_secs(3),
            ..Default::default()
        },
        ..opts()
    };
    assert_eq!(eager(&snap, &q, &short).unwrap().len(), 5_000);
    let mut cursor = select_cursor(snap.clone(), &q, &short, &strict(1)).unwrap();
    assert!(cursor.next_batch().unwrap().is_some());
    std::thread::sleep(Duration::from_millis(3500));
    let failure = loop {
        match cursor.next_batch() {
            Ok(Some(_)) => {}
            Ok(None) => break None,
            Err(e) => break Some(e),
        }
    };
    assert!(
        matches!(&failure, Some(Error::Service(m)) if m.contains("no complete response within")),
        "{failure:?}"
    );
}

/// An EXISTS over a SERVICE without SILENT streams, and sends the requests an eager run
/// sends.
#[test]
fn an_exists_over_a_streamed_service_sends_eagers_requests() {
    let s = Store::in_memory(StoreOptions::default());
    let mut nt = String::new();
    for i in 0..2_000 {
        nt.push_str(&format!("<urn:s:{i}> <urn:p> {} .\n", i % 10));
    }
    s.load(&[Source::from_bytes(nt.into_bytes(), RdfFormat::Turtle, None)])
        .unwrap();
    let ep = Endpoint::start(answer(3));
    for q in [
        format!(
            "SELECT ?s ?o WHERE {{ ?s <urn:p> ?o FILTER EXISTS {{ SERVICE <{}> {{ ?x <urn:b> ?y }} }} }}",
            ep.url
        ),
        format!(
            "SELECT ?s ?o WHERE {{ ?s <urn:p> ?o FILTER NOT EXISTS {{ SERVICE <{}> {{ ?x <urn:b> ?y }} }} }}",
            ep.url
        ),
    ] {
        let count = |mode: ExecutionMode, c: &CursorOptions| {
            let before = ep.calls();
            let n = match query_execution(s.snapshot(), &q, &opts(), c, mode).unwrap() {
                QueryExecution::Select(mut c) => {
                    let mut n = 0;
                    while let Some(b) = c.next_batch().unwrap() {
                        n += b.len();
                    }
                    n
                }
                QueryExecution::Eager(r) => r.into_result().len(),
                _ => unreachable!("a SELECT"),
            };
            (n, ep.calls() - before)
        };
        let eager = count(ExecutionMode::Eager, &CursorOptions::default());
        let streamed = count(ExecutionMode::Streaming, &strict(64));
        assert_eq!(eager, streamed, "{q}");
        assert_eq!(eager.1, 1, "{q}");
    }
}

/// A legacy `ViewSource` implementing only eager `read_view` (refused by strict cursors).
#[derive(Debug)]
struct LegacyViews;

impl sparkles_core::sparql::viewsvc::ViewSource for LegacyViews {
    fn read_view(
        &self,
        _: &sparkles_core::sparql::viewsvc::ViewRead<'_>,
    ) -> Result<sparkles_core::sparql::viewsvc::ViewRows> {
        Ok(sparkles_core::sparql::viewsvc::ViewRows {
            columns: vec!["x".into()],
            rows: vec![(vec![Some(Term::Literal(oxrdf::Literal::from(1)))], 1)],
            commit: 0,
            evaluated_under: None,
        })
    }
}

/// The in-process endpoint `http://local/sparql`, which reads `snap` and may call
/// services itself, plus `http://legacy-view/sparql`, which carries a legacy `ViewSource`.
struct Local(Arc<Snapshot>, Arc<()>);

impl std::fmt::Debug for Local {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Local")
    }
}

impl LocalEndpoints for Local {
    fn resolve(&self, iri: &str) -> Option<Result<LocalTarget>> {
        match iri {
            "http://local/sparql" => Some(Ok(LocalTarget {
                snap: self.0.clone(),
                options: QueryOptions {
                    allow_service: true,
                    no_cache: true,
                    outbound: OutboundPolicy {
                        allow_private: true,
                        ..Default::default()
                    },
                    ..Default::default()
                },
                hold: Some(Box::new(self.1.clone())),
            })),
            "http://legacy-view/sparql" => Some(Ok(LocalTarget {
                snap: self.0.clone(),
                options: QueryOptions {
                    no_cache: true,
                    views: Some(Arc::new(LegacyViews)),
                    ..Default::default()
                },
                hold: Some(Box::new(self.1.clone())),
            })),
            _ => None,
        }
    }
}

/// An in-process call runs as a cursor of the caller's policy. A strict caller refuses
/// a callee form that the strict policy refuses, at the first pull, and a permissive
/// caller answers it. The callee shares the time its caller's consumer keeps the cursor
/// waiting, so a pause that the caller's deadline leaves out does not expire the call.
#[test]
fn an_in_process_call_runs_under_the_callers_policy_and_deadline() {
    let s = store();
    let remote = Store::in_memory(StoreOptions::default());
    let mut nt = String::new();
    for i in 0..100 {
        nt.push_str(&format!("<urn:s:{i}> <urn:p> {i} .\n"));
    }
    remote
        .load(&[Source::from_bytes(nt.into_bytes(), RdfFormat::Turtle, None)])
        .unwrap();
    let snap = s.snapshot();
    let hold = Arc::new(());
    let o = QueryOptions {
        local_endpoints: Some(Arc::new(Local(remote.snapshot(), hold.clone()))),
        ..opts()
    };
    let plain = "SELECT ?s ?o { SERVICE <http://local/sparql> { ?s <urn:p> ?o } }";
    let expected = eager(&snap, plain, &o).unwrap();
    assert_eq!(expected.len(), 100);
    for rows in [1, 9, 4096] {
        let (answer, plan) = streamed(&snap, plain, &o, &strict(rows)).unwrap();
        assert_eq!(answer, expected, "{rows}");
        assert!(!materializes(&plan));
        assert_service_streaming(&plan, "local drained");
    }
    {
        let mut cursor = select_cursor(snap.clone(), plain, &o, &strict(9)).unwrap();
        assert_service_streaming(cursor.plan(), "local initial");
        let first = cursor.next_batch().unwrap().expect("first local batch");
        assert_eq!(first.len(), 9);
        drop(first);
        assert_service_streaming(cursor.plan(), "local after first batch");
        let mut n = 9;
        while let Some(b) = cursor.next_batch().unwrap() {
            n += b.len();
        }
        assert_eq!(n, 100);
        assert_service_streaming(cursor.plan(), "local after none");
    }
    // a nested SILENT SERVICE inside an in-process call now runs natively under strict policy
    let failing = Endpoint::start(Arc::new(|| (500, vec![Part::Bytes(b"no".to_vec())])));
    let nested_silent = format!(
        "SELECT ?s ?o {{ SERVICE <http://local/sparql> {{ ?s <urn:p> ?o SERVICE SILENT <{}> {{ ?s ?q ?r }} }} }}",
        failing.url
    );
    let expected = eager(&snap, &nested_silent, &o).unwrap();
    assert_eq!(expected.len(), 100);
    let (got, plan) = streamed(&snap, &nested_silent, &o, &strict(4)).unwrap();
    assert_eq!(got, expected);
    assert!(!materializes(&plan));

    // a callee holding a strict-refused form (legacy eager-only ViewSource) is refused at
    // first pull under strict policy (even when wrapped in SILENT) and answered under permissive.
    let nested_refused = "SELECT ?x { SERVICE <http://legacy-view/sparql> { SERVICE <urn:x-sparkles:view#read> { [] <urn:x-sparkles:view#name> \"v\" ; <urn:x-sparkles:view#bind> ?x } } }";
    let expected_refused = eager(&snap, nested_refused, &o).unwrap();
    assert_eq!(expected_refused.len(), 1);
    for q in [
        nested_refused.to_string(),
        nested_refused.replace(
            "SERVICE <http://legacy-view/sparql>",
            "SERVICE SILENT <http://legacy-view/sparql>",
        ),
    ] {
        let mut cursor = select_cursor(snap.clone(), &q, &o, &strict(4)).unwrap();
        let r = cursor.next_batch();
        assert!(
            matches!(&r, Err(Error::Unsupported(m)) if m.contains("materialization at ViewRead")),
            "{q}: {:?}",
            r.map(|b| b.map(|b| b.len()))
        );
    }
    let allow = CursorOptions {
        fallback: FallbackPolicy::AllowMaterialization,
        ..strict(4)
    };
    assert_eq!(
        streamed(&snap, nested_refused, &o, &allow).unwrap().0,
        expected_refused
    );
    // a pause of the caller's consumer past the deadline, after the call opened and
    // before it opened
    let timed = QueryOptions {
        timeout: Some(Duration::from_secs(2)),
        ..o.clone()
    };
    let hour = 3_600_000_000_000;
    let mut cursor = select_cursor(snap.clone(), plain, &timed, &strict(1)).unwrap();
    let mut n = cursor.next_batch().unwrap().unwrap().len();
    cursor.deadline_pause().store(hour, Ordering::Relaxed);
    std::thread::sleep(Duration::from_millis(2500));
    while let Some(b) = cursor.next_batch().unwrap() {
        n += b.len();
    }
    assert_eq!(n, 100);
    let mut cursor = select_cursor(snap.clone(), plain, &timed, &strict(1)).unwrap();
    cursor.deadline_pause().store(hour, Ordering::Relaxed);
    std::thread::sleep(Duration::from_millis(2500));
    let mut n = 0;
    while let Some(b) = cursor.next_batch().unwrap() {
        n += b.len();
    }
    assert_eq!(n, 100);
    // a slice that has its rows lets go of the call before the next UNION branch runs
    let sliced = "SELECT ?s ?a { { SELECT ?s { SERVICE <http://local/sparql> { ?s <urn:p> ?o } } LIMIT 1 } UNION { ?a <urn:b> ?v } }";
    let mut cursor = select_cursor(snap.clone(), sliced, &o, &strict(1)).unwrap();
    assert_eq!(cursor.next_batch().unwrap().unwrap().len(), 1);
    assert_eq!(
        Arc::strong_count(&hold),
        2,
        "the fixture and the endpoints only"
    );
    let mut n = 1;
    while let Some(b) = cursor.next_batch().unwrap() {
        n += b.len();
    }
    assert_eq!(n, 2);
}

/// A cache hit is charged what its entry keeps allocated, every slot of a wide row of
/// mostly unbound values included, which the cache's own weight of the entry leaves
/// out. A budget between the two refuses the streamed hit.
#[test]
fn a_cache_hit_is_charged_what_its_entry_keeps() {
    let s = store();
    let snap = s.snapshot();
    let width = 100;
    let vars: Vec<String> = (0..width).map(|i| format!("\"v{i}\"")).collect();
    let head = format!(
        r#"{{"head":{{"vars":["s",{}]}},"results":{{"bindings":["#,
        vars.join(",")
    );
    let n = 2_000;
    let body = format!(
        "{head}{}{TAIL}",
        (0..n)
            .map(|i| format!(r#"{{"s":{{"type":"uri","value":"urn:s:{i}"}}}}"#))
            .collect::<Vec<_>>()
            .join(",")
    );
    let ep = Endpoint::start(Arc::new(move || {
        (200, vec![Part::Bytes(body.clone().into_bytes())])
    }));
    let q = format!("SELECT ?s {{ SERVICE <cache:{}> {{ ?s ?p ?o }} }}", ep.url);
    let cached = QueryOptions {
        no_cache: false,
        ..opts()
    };
    assert_eq!(
        streamed(&snap, &q, &cached, &strict(256)).unwrap().0.len(),
        n
    );
    assert_eq!(s.result_cache().service.entries(), 1);
    let calls = ep.calls();
    // the weight counts 16 bytes an unbound slot, and a slot is larger than that
    let slot = std::mem::size_of::<Option<Term>>() as u64;
    assert!(slot > 16);
    let weight = s.result_cache().service.bytes();
    let tight = QueryOptions {
        max_memory_bytes: Some(weight + (n as u64 * width * (slot - 16)) / 2),
        ..cached.clone()
    };
    let r = streamed(&snap, &q, &tight, &strict(256));
    assert!(
        matches!(&r, Err(Error::BudgetExceeded(b)) if b.kind == BudgetKind::Memory),
        "{:?}",
        r.map(|r| r.0.len())
    );
    assert_eq!(ep.calls(), calls, "the cache answered");
}

/// An EXISTS pattern runs an in-process call under the policy of the cursor that holds
/// the filter, not a permissive one of its own. A strict caller refuses a callee form
/// that the strict policy refuses, inside FILTER EXISTS as at the top of the query.
#[test]
fn an_exists_over_an_in_process_call_keeps_the_strict_policy() {
    let s = store();
    let remote = Store::in_memory(StoreOptions::default());
    remote
        .load(&[Source::from_bytes(
            b"<urn:x> <urn:p> 1 .\n".to_vec(),
            RdfFormat::Turtle,
            None,
        )])
        .unwrap();
    let snap = s.snapshot();
    let o = QueryOptions {
        local_endpoints: Some(Arc::new(Local(remote.snapshot(), Arc::new(())))),
        ..opts()
    };
    let failing = Endpoint::start(Arc::new(|| (500, vec![Part::Bytes(b"no".to_vec())])));
    for filter in ["EXISTS", "NOT EXISTS"] {
        // Nested SILENT SERVICE runs natively under strict policy inside EXISTS too.
        let q_silent = format!(
            "SELECT ?a {{ ?a <urn:b> ?v FILTER {filter} {{ SERVICE <http://local/sparql> {{ ?x <urn:p> ?o SERVICE SILENT <{}> {{ ?x ?q ?r }} }} }} }}",
            failing.url
        );
        let expected_silent = eager(&snap, &q_silent, &o).unwrap();
        let (got_silent, plan_silent) = streamed(&snap, &q_silent, &o, &strict(4)).unwrap();
        assert_eq!(got_silent, expected_silent, "{filter}");
        assert!(!materializes(&plan_silent));
        assert_eq!(auto(&snap, &q_silent, &o), expected_silent, "{filter}");

        // A strict-refused callee form (legacy ViewSource) still fails under strict policy
        // inside EXISTS, and succeeds under AllowMaterialization / auto.
        let q_refused = format!(
            "SELECT ?a {{ ?a <urn:b> ?v FILTER {filter} {{ SERVICE <http://legacy-view/sparql> {{ SERVICE <urn:x-sparkles:view#read> {{ [] <urn:x-sparkles:view#name> \"v\" ; <urn:x-sparkles:view#bind> ?x }} }} }} }}"
        );
        let expected = eager(&snap, &q_refused, &o).unwrap();
        let r = streamed(&snap, &q_refused, &o, &strict(4));
        assert!(
            matches!(&r, Err(Error::Unsupported(m)) if m.contains("materialization at ViewRead")),
            "{filter}: {:?}",
            r.map(|(rows, _)| rows)
        );
        let allow = CursorOptions {
            fallback: FallbackPolicy::AllowMaterialization,
            ..strict(4)
        };
        assert_eq!(
            streamed(&snap, &q_refused, &o, &allow).unwrap().0,
            expected,
            "{filter}"
        );
        assert_eq!(auto(&snap, &q_refused, &o), expected, "{filter}");
    }
    // automatic admission under the default policy answers the nested form too
    let nested = format!(
        "SELECT ?x {{ SERVICE <http://local/sparql> {{ ?x <urn:p> ?o SERVICE SILENT <{}> {{ ?x ?q ?r }} }} }}",
        failing.url
    );
    assert_eq!(auto(&snap, &nested, &o), eager(&snap, &nested, &o).unwrap());
}

/// The strict refusal of a response whose head comes late fails the query inside FILTER
/// EXISTS and NOT EXISTS too, over the remote directly and through an in-process call,
/// rather than reading as a false filter. A permissive cursor answers as eager does.
#[test]
fn a_late_head_refusal_inside_exists_fails_the_query() {
    let s = store();
    let snap = s.snapshot();
    let late = Endpoint::start(Arc::new(|| {
        (
            200,
            vec![Part::Bytes(
                format!(
                    r#"{{"results":{{"bindings":[{}]}},"head":{{"vars":["s","o"]}}}}"#,
                    rows(10)
                )
                .into_bytes(),
            )],
        )
    }));
    let remote = Store::in_memory(StoreOptions::default());
    remote
        .load(&[Source::from_bytes(
            b"<urn:x> <urn:p> 1 .\n".to_vec(),
            RdfFormat::Turtle,
            None,
        )])
        .unwrap();
    let o = QueryOptions {
        local_endpoints: Some(Arc::new(Local(remote.snapshot(), Arc::new(())))),
        ..opts()
    };
    let direct = format!("SERVICE <{}> {{ ?s ?p ?o }}", late.url);
    let nested = format!(
        "SERVICE <http://local/sparql> {{ ?x <urn:p> ?v SERVICE <{}> {{ ?s ?p ?o }} }}",
        late.url
    );
    for inner in [&direct, &nested] {
        for filter in ["EXISTS", "NOT EXISTS"] {
            let q = format!("SELECT ?a {{ ?a <urn:b> ?w FILTER {filter} {{ {inner} }} }}");
            let r = streamed(&snap, &q, &o, &strict(4));
            assert!(
                matches!(&r, Err(Error::Service(m)) if m.contains("before the variables of its head")),
                "{filter} {inner}: {:?}",
                r.map(|(rows, _)| rows)
            );
            let allow = CursorOptions {
                fallback: FallbackPolicy::AllowMaterialization,
                ..strict(4)
            };
            assert_eq!(
                streamed(&snap, &q, &o, &allow).unwrap().0,
                eager(&snap, &q, &o).unwrap(),
                "{filter} {inner}"
            );
        }
    }

    // An enclosing `SERVICE SILENT` around an in-process callee that calls a non-SILENT
    // late-head endpoint propagates the strict refusal under `RejectMaterialization` and
    // yields the bindings under `AllowMaterialization`.
    let silent_nested = format!(
        "SELECT ?x ?s ?o {{ SERVICE SILENT <http://local/sparql> {{ ?x <urn:p> ?v SERVICE <{}> {{ ?s ?p ?o }} }} }}",
        late.url
    );
    let r = streamed(&snap, &silent_nested, &o, &strict(4));
    assert!(
        matches!(&r, Err(Error::Service(m)) if m.contains("before the variables of its head")),
        "enclosing SILENT must propagate strict callee LATE_HEAD: {:?}",
        r.map(|(rows, _)| rows)
    );
    let allow = CursorOptions {
        fallback: FallbackPolicy::AllowMaterialization,
        ..strict(4)
    };
    assert_eq!(
        streamed(&snap, &silent_nested, &o, &allow).unwrap().0,
        eager(&snap, &silent_nested, &o).unwrap()
    );
}

/// Truncated or trailing-malformed late-head JSON responses never emit partial rows and
/// never populate the service cache, under both `SILENT SERVICE` and permissive
/// non-`SILENT SERVICE`.
#[test]
fn malformed_late_head_responses_never_emit_partial_rows_or_cache() {
    let s = store();
    let snap = s.snapshot();
    let cached = QueryOptions {
        no_cache: false,
        ..opts()
    };
    let allow = CursorOptions {
        fallback: FallbackPolicy::AllowMaterialization,
        ..strict(2)
    };
    let cases: [(&str, &[u8]); 2] = [
        (
            "truncated after vars",
            br#"{"results":{"bindings":[{"s":{"type":"uri","value":"urn:s"}}]},"head":{"vars":["s"]"#,
        ),
        (
            "trailing garbage",
            br#"{"results":{"bindings":[{"s":{"type":"uri","value":"urn:s"}}]},"head":{"vars":["s"]}} trailing"#,
        ),
    ];
    for (label, body) in cases {
        let payload = body.to_vec();
        let ep = Endpoint::start(Arc::new(move || (200, vec![Part::Bytes(payload.clone())])));
        s.result_cache().service.clear();

        let silent_q = format!(
            "SELECT ?s {{ SERVICE SILENT <cache:{}> {{ ?s ?p ?o }} }}",
            ep.url
        );
        let unit = vec![vec![None]];
        assert_eq!(eager(&snap, &silent_q, &opts()).unwrap(), unit, "{label}");
        assert_eq!(
            streamed(&snap, &silent_q, &cached, &strict(1)).unwrap().0,
            unit,
            "{label}: first silent streamed"
        );
        assert_eq!(
            s.result_cache().service.entries(),
            0,
            "{label}: malformed late-head must not populate cache"
        );
        let calls_after_first = ep.calls();
        assert_eq!(
            streamed(&snap, &silent_q, &cached, &strict(1)).unwrap().0,
            unit,
            "{label}: second silent streamed"
        );
        assert_eq!(
            ep.calls(),
            calls_after_first + 1,
            "{label}: second cache-enabled call must contact endpoint again"
        );

        let non_silent_q = format!("SELECT ?s {{ SERVICE <cache:{}> {{ ?s ?p ?o }} }}", ep.url);
        let r = streamed(&snap, &non_silent_q, &cached, &allow);
        assert!(
            matches!(&r, Err(Error::Service(_))),
            "{label}: permissive non-silent must fail: {:?}",
            r.map(|(rows, _)| rows)
        );
        assert_eq!(
            s.result_cache().service.entries(),
            0,
            "{label}: failed permissive late-head must not populate cache"
        );
    }
}

/// `SILENT SERVICE` enforces `max_rows` and `max_rows_produced` per accepted row before
/// reading malformed trailing content, never caches a failed stream, and serves a
/// succeeding cache hit directly without hitting the endpoint again.
#[test]
fn silent_service_enforces_row_budgets_per_row_and_cache_semantics() {
    let s = store();
    let snap = s.snapshot();

    // 2 valid rows followed by malformed JSON.
    let cut = format!("{HEAD}{},{{\"s\":", rows(2));
    let broken = Endpoint::start(Arc::new(move || {
        (200, vec![Part::Bytes(cut.clone().into_bytes())])
    }));
    let q_broken = format!(
        "SELECT ?s ?o {{ SERVICE SILENT <cache:{}> {{ ?s ?p ?o }} }}",
        broken.url
    );
    let cached = QueryOptions {
        no_cache: false,
        ..opts()
    };

    // max_rows = 1 crosses the limit on row 2 before the malformed JSON arrives.
    let over_rows = QueryOptions {
        max_rows: Some(1),
        ..opts()
    };
    let r = streamed(&snap, &q_broken, &over_rows, &strict(1));
    assert!(
        matches!(&r, Err(Error::BudgetExceeded(b)) if b.kind == BudgetKind::Rows),
        "expected BudgetKind::Rows, got {:?}",
        r.map(|(rows, _)| rows)
    );

    // max_rows = 2 stays within the limit, hits malformed JSON, and suppresses to 1 empty row.
    let within_rows = QueryOptions {
        max_rows: Some(2),
        ..opts()
    };
    let unit = vec![vec![None, None]];
    assert_eq!(
        streamed(&snap, &q_broken, &within_rows, &strict(1))
            .unwrap()
            .0,
        unit
    );

    // max_rows_produced = 1 crosses the produced-row limit before malformed JSON arrives.
    let over_produced = QueryOptions {
        max_rows_produced: Some(1),
        ..opts()
    };
    let r = streamed(&snap, &q_broken, &over_produced, &strict(1));
    assert!(
        matches!(&r, Err(Error::BudgetExceeded(b)) if b.kind == BudgetKind::RowsProduced),
        "expected BudgetKind::RowsProduced, got {:?}",
        r.map(|(rows, _)| rows)
    );

    // Mid-stream failure never populates the service cache.
    s.result_cache().service.clear();
    let before = broken.calls();
    assert_eq!(
        streamed(&snap, &q_broken, &cached, &strict(1)).unwrap().0,
        unit
    );
    assert_eq!(s.result_cache().service.entries(), 0);
    assert_eq!(
        streamed(&snap, &q_broken, &cached, &strict(1)).unwrap().0,
        unit
    );
    assert_eq!(broken.calls(), before + 2);

    // A succeeding SILENT SERVICE populates the cache once and serves later runs from cache.
    let ok = Endpoint::start(answer(9));
    let q_ok = format!(
        "SELECT ?s ?o {{ SERVICE SILENT <cache:{}> {{ ?s ?p ?o }} }}",
        ok.url
    );
    s.result_cache().service.clear();
    let (first_rows, _) = streamed(&snap, &q_ok, &cached, &strict(2)).unwrap();
    assert_eq!(first_rows.len(), 9);
    assert_eq!(ok.calls(), 1);
    assert_eq!(s.result_cache().service.entries(), 1);
    for batch_rows in [1, 4096] {
        let (hit_rows, plan) = streamed(&snap, &q_ok, &cached, &strict(batch_rows)).unwrap();
        assert_eq!(hit_rows, first_rows, "batch_rows={batch_rows}");
        assert!(!materializes(&plan));
    }
    assert_eq!(
        ok.calls(),
        1,
        "SILENT cache hit must not call endpoint again"
    );
}

/// An ordinary failure inside EXISTS stays a false filter under the strict policy even
/// when the endpoint's own words in the error read like the strict refusal of a late
/// head. Here a head-first answer has a term key that sparesults reports verbatim.
#[test]
fn an_endpoints_words_do_not_make_a_failure_a_refusal() {
    let s = store();
    let snap = s.snapshot();
    let key = "a strict cursor cannot stream a SERVICE response whose results come before the variables of its head: <x>: the response gives its results before the variables of its head, which a strict cursor cannot stream";
    let body = format!(
        r#"{{"head":{{"vars":["s"]}},"results":{{"bindings":[{{"s":{{"type":"uri","value":"urn:x","{key}":"1"}}}}]}}}}"#
    );
    let ep = Endpoint::start(Arc::new(move || {
        (200, vec![Part::Bytes(body.clone().into_bytes())])
    }));
    for filter in ["EXISTS", "NOT EXISTS"] {
        let q = format!(
            "SELECT ?a {{ ?a <urn:b> ?w FILTER {filter} {{ SERVICE <{}> {{ ?s ?p ?o }} }} }}",
            ep.url
        );
        let expected = eager(&snap, &q, &opts()).unwrap();
        assert_eq!(
            streamed(&snap, &q, &opts(), &strict(4)).unwrap().0,
            expected,
            "{filter}"
        );
    }
}

/// The rows of a query under automatic admission and the default cursor policy.
fn auto(snap: &Arc<Snapshot>, q: &str, o: &QueryOptions) -> Vec<Vec<Option<Term>>> {
    match query_execution(
        snap.clone(),
        q,
        o,
        &CursorOptions::default(),
        ExecutionMode::Auto,
    )
    .unwrap()
    {
        QueryExecution::Select(mut c) => {
            let mut out = Vec::new();
            while let Some(b) = c.next_batch().unwrap() {
                for r in 0..b.len() {
                    out.push(b.row(r).unwrap().values().to_vec());
                }
            }
            out
        }
        QueryExecution::Eager(r) => r
            .into_result()
            .rows()
            .unwrap()
            .iter()
            .map(|r| r.values().to_vec())
            .collect(),
        _ => unreachable!("a SELECT"),
    }
}

/// A JSON response that gives its results before the variables of its head, which
/// sparesults buffers whole. A strict cursor refuses it by name as soon as it has read
/// the start of the results, while the endpoint still holds the rest back. A permissive
/// one answers it as eager does, under a charge for the buffered answer, so a budget the
/// whole answer outgrows refuses it.
#[test]
fn a_response_whose_head_comes_late_is_refused_or_charged() {
    let s = store();
    let snap = s.snapshot();
    let gate = Arc::new(Gate::default());
    let held = gate.clone();
    let late = Endpoint::start(Arc::new(move || {
        (
            200,
            vec![
                Part::Bytes(format!(r#"{{"results":{{"bindings":[{}"#, rows(10)).into_bytes()),
                Part::Wait(held.clone()),
                Part::Bytes(br#"]},"head":{"vars":["s","o"]}}"#.to_vec()),
            ],
        )
    }));
    let q = format!("SELECT ?s ?o {{ SERVICE <{}> {{ ?s ?p ?o }} }}", late.url);
    let started = Instant::now();
    let mut cursor = select_cursor(snap.clone(), &q, &opts(), &strict(4)).unwrap();
    let r = cursor.next_batch().map(|b| b.map(|b| b.len()));
    assert!(
        matches!(&r, Err(Error::Service(m)) if m.contains("before the variables of its head")),
        "{r:?}"
    );
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "refused before the gate"
    );
    gate.open();
    let allow = CursorOptions {
        fallback: FallbackPolicy::AllowMaterialization,
        ..strict(4)
    };
    let expected = eager(&snap, &q, &opts()).unwrap();
    assert_eq!(expected.len(), 10);
    // Drive the permissive ten-row cursor directly so we inspect the Service plan node
    // initially, immediately after the first batch, after the last rows, and after None.
    let mut cursor = select_cursor(snap.clone(), &q, &opts(), &allow).unwrap();
    assert_service_streaming(cursor.plan(), "late-head initial");
    let mut answer = Vec::new();
    let first = cursor.next_batch().unwrap().expect("first late-head batch");
    assert_eq!(first.len(), 4);
    for r in 0..first.len() {
        answer.push(first.row(r).unwrap().values().to_vec());
    }
    drop(first);
    assert_service_buffered(cursor.plan(), "late-head after first batch");
    let second = cursor
        .next_batch()
        .unwrap()
        .expect("second late-head batch");
    assert_eq!(second.len(), 4);
    for r in 0..second.len() {
        answer.push(second.row(r).unwrap().values().to_vec());
    }
    drop(second);
    let third = cursor.next_batch().unwrap().expect("last late-head batch");
    assert_eq!(third.len(), 2);
    for r in 0..third.len() {
        answer.push(third.row(r).unwrap().values().to_vec());
    }
    drop(third);
    assert_service_buffered(cursor.plan(), "late-head after last rows");
    assert!(cursor.next_batch().unwrap().is_none());
    assert_service_buffered(cursor.plan(), "late-head completed plan");
    assert_eq!(answer, expected);
    // Batch capacity larger than the ten-row answer releases the source during the first
    // pull; the runtime plan still retains the discovered buffering flag.
    let allow_wide = CursorOptions {
        fallback: FallbackPolicy::AllowMaterialization,
        ..strict(64)
    };
    let mut wide = select_cursor(snap.clone(), &q, &opts(), &allow_wide).unwrap();
    assert_service_streaming(wide.plan(), "late-head wide initial");
    let all = wide.next_batch().unwrap().expect("single wide batch");
    assert_eq!(all.len(), 10);
    drop(all);
    assert_service_buffered(wide.plan(), "late-head wide after single batch");
    assert!(wide.next_batch().unwrap().is_none());
    assert_service_buffered(wide.plan(), "late-head wide completed plan");
    // Empty bindings array with late head: no nonempty batch is emitted, yet the
    // completed plan still reports the buffering startup barrier.
    let empty_late = Endpoint::start(Arc::new(|| {
        (
            200,
            vec![Part::Bytes(
                br#"{"results":{"bindings":[]},"head":{"vars":["s","o"]}}"#.to_vec(),
            )],
        )
    }));
    let q_empty = format!(
        "SELECT ?s ?o {{ SERVICE <{}> {{ ?s ?p ?o }} }}",
        empty_late.url
    );
    let mut empty_cursor = select_cursor(snap.clone(), &q_empty, &opts(), &allow).unwrap();
    assert_service_streaming(empty_cursor.plan(), "late-head empty initial");
    assert!(empty_cursor.next_batch().unwrap().is_none());
    assert_service_buffered(empty_cursor.plan(), "late-head empty completed plan");
    // Outer LIMIT 1 releases the Service source via release_finished while preserving
    // the discovered buffering flag on the child Service node.
    let q_limit = format!(
        "SELECT ?s ?o {{ SERVICE <{}> {{ ?s ?p ?o }} }} LIMIT 1",
        late.url
    );
    let mut limited = select_cursor(snap.clone(), &q_limit, &opts(), &allow).unwrap();
    assert_service_streaming(limited.plan(), "late-head limit initial");
    let one = limited.next_batch().unwrap().expect("limited batch");
    assert_eq!(one.len(), 1);
    drop(one);
    assert_service_buffered(limited.plan(), "late-head limit after first batch");
    assert!(limited.next_batch().unwrap().is_none());
    assert_service_buffered(limited.plan(), "late-head limit completed plan");
    // Empty bindings, which a buffered answer keeps at many times their bytes: the
    // budget holds the bytes many times over, but not what sparesults keeps of them.
    let body = format!(
        r#"{{"results":{{"bindings":[{}]}},"head":{{"vars":["s","o"]}}}}"#,
        vec!["{}"; 70_000].join(",")
    );
    let big = Endpoint::start(Arc::new(move || {
        (200, vec![Part::Bytes(body.clone().into_bytes())])
    }));
    let q = format!("SELECT ?s ?o {{ SERVICE <{}> {{ ?s ?p ?o }} }}", big.url);
    let roomy = QueryOptions {
        max_memory_bytes: Some(64 << 20),
        ..opts()
    };
    assert_eq!(streamed(&snap, &q, &roomy, &allow).unwrap().0.len(), 70_000);
    let tight = QueryOptions {
        max_memory_bytes: Some(4 << 20),
        ..opts()
    };
    let r = streamed(&snap, &q, &tight, &allow).map(|(rows, _)| rows.len());
    assert!(
        matches!(&r, Err(Error::BudgetExceeded(b)) if b.kind == BudgetKind::Memory),
        "{r:?}"
    );
}

/// The head of a response is charged before the parser keeps it, so a head of many
/// variables that the budget cannot hold refuses the query, at the head-first order a
/// strict cursor streams.
#[test]
fn an_oversized_head_is_refused_under_a_tight_budget() {
    let s = store();
    let snap = s.snapshot();
    let vars: Vec<String> = (0..40_000).map(|i| format!(r#""v{i}""#)).collect();
    let body = format!(
        r#"{{"head":{{"vars":["s","o",{}]}},"results":{{"bindings":[{}]}}}}"#,
        vars.join(","),
        rows(3)
    );
    let ep = Endpoint::start(Arc::new(move || {
        (200, vec![Part::Bytes(body.clone().into_bytes())])
    }));
    let q = format!("SELECT ?s ?o {{ SERVICE <{}> {{ ?s ?p ?o }} }}", ep.url);
    assert_eq!(streamed(&snap, &q, &opts(), &strict(4)).unwrap().0.len(), 3);
    let tight = QueryOptions {
        max_memory_bytes: Some(1 << 20),
        ..opts()
    };
    let r = streamed(&snap, &q, &tight, &strict(4)).map(|(rows, _)| rows.len());
    assert!(
        matches!(&r, Err(Error::BudgetExceeded(b)) if b.kind == BudgetKind::Memory),
        "{r:?}"
    );
}

/// A literal far larger than the rest of the answer grows the parser's buffer while it
/// arrives. The buffer is charged by the bytes read between two solutions, so a budget
/// that holds the rest of the answer refuses the literal.
#[test]
fn a_large_literal_is_charged_while_the_parser_reads_it() {
    let s = store();
    let snap = s.snapshot();
    let big = "x".repeat(1 << 20);
    let body = format!(
        r#"{HEAD}{},{{"s":{{"type":"uri","value":"urn:s"}},"o":{{"type":"literal","value":"{big}"}}}}{TAIL}"#,
        rows(3)
    );
    let ep = Endpoint::start(Arc::new(move || {
        (200, vec![Part::Bytes(body.clone().into_bytes())])
    }));
    let q = format!("SELECT ?s {{ SERVICE <{}> {{ ?s ?p ?o }} }}", ep.url);
    assert_eq!(streamed(&snap, &q, &opts(), &strict(1)).unwrap().0.len(), 4);
    let tight = QueryOptions {
        max_memory_bytes: Some(5 << 20),
        ..opts()
    };
    let r = streamed(&snap, &q, &tight, &strict(1)).map(|(rows, _)| rows.len());
    assert!(
        matches!(&r, Err(Error::BudgetExceeded(b)) if b.kind == BudgetKind::Memory),
        "{r:?}"
    );
}

/// An in-process call's memory counts against its caller's query. A callee that sorts
/// its answer holds every row of it, and the caller's peak includes that state, as the
/// caller's limit then does.
#[test]
fn an_in_process_call_charges_its_callers_query() {
    let s = store();
    let remote = Store::in_memory(StoreOptions::default());
    let mut nt = String::new();
    for i in 0..20_000 {
        nt.push_str(&format!(
            "<urn:s:{i}> <urn:p> \"a long enough literal value number {i:08}\" .\n"
        ));
    }
    remote
        .load(&[Source::from_bytes(nt.into_bytes(), RdfFormat::Turtle, None)])
        .unwrap();
    let snap = s.snapshot();
    let o = QueryOptions {
        local_endpoints: Some(Arc::new(Local(remote.snapshot(), Arc::new(())))),
        ..opts()
    };
    let inner = "SELECT ?s ?o { ?s <urn:p> ?o } ORDER BY DESC(?o)";
    let mut callee = select_cursor(remote.snapshot(), inner, &o, &strict(64)).unwrap();
    let mut n = 0;
    while let Some(b) = callee.next_batch().unwrap() {
        n += b.len();
    }
    assert_eq!(n, 20_000);
    let alone = callee.stats().mem_peak_bytes;
    drop(callee);
    let q = format!("SELECT ?s ?o {{ SERVICE <http://local/sparql> {{ {inner} }} }}");
    let mut cursor = select_cursor(snap.clone(), &q, &o, &strict(64)).unwrap();
    assert_eq!(cursor.next_batch().unwrap().unwrap().len(), 64);
    let peak = cursor.stats().mem_peak_bytes;
    assert!(peak >= alone / 2, "caller {peak}, callee alone {alone}");
}

/// A cache hit of nested triple terms is charged the box of every triple at every level,
/// which the cache's own weight of the entry leaves out. A budget between the two
/// refuses the streamed hit.
#[test]
fn a_cache_hit_of_nested_triple_terms_is_charged_every_box() {
    let s = store();
    let snap = s.snapshot();
    let depth = 8;
    let iri = |v: &str| format!(r#"{{"type":"uri","value":"{v}"}}"#);
    let term = |i: usize| {
        let mut t = iri(&format!("urn:o:{i}"));
        for _ in 0..depth {
            t = format!(
                r#"{{"type":"triple","value":{{"subject":{},"predicate":{},"object":{t}}}}}"#,
                iri("urn:s"),
                iri("urn:p")
            );
        }
        t
    };
    let n = 4_000;
    let body = format!(
        r#"{{"head":{{"vars":["t"]}},"results":{{"bindings":[{}{TAIL}"#,
        (0..n)
            .map(|i| format!(r#"{{"t":{}}}"#, term(i)))
            .collect::<Vec<_>>()
            .join(",")
    );
    let ep = Endpoint::start(Arc::new(move || {
        (200, vec![Part::Bytes(body.clone().into_bytes())])
    }));
    let q = format!("SELECT ?t {{ SERVICE <cache:{}> {{ ?t ?p ?o }} }}", ep.url);
    let cached = QueryOptions {
        no_cache: false,
        ..opts()
    };
    let answer = streamed(&snap, &q, &cached, &strict(256)).unwrap().0;
    assert_eq!(answer.len(), n);
    assert!(matches!(&answer[0][0], Some(Term::Triple(_))));
    assert_eq!(s.result_cache().service.entries(), 1);
    let calls = ep.calls();
    let weight = s.result_cache().service.bytes();
    let slot = std::mem::size_of::<Option<Term>>() as u64;
    let boxes = n as u64 * depth as u64 * std::mem::size_of::<oxrdf::Triple>() as u64;
    let tight = QueryOptions {
        max_memory_bytes: Some(weight + n as u64 * slot + boxes / 2),
        ..cached.clone()
    };
    let r = streamed(&snap, &q, &tight, &strict(256));
    assert!(
        matches!(&r, Err(Error::BudgetExceeded(b)) if b.kind == BudgetKind::Memory),
        "{:?}",
        r.map(|r| r.0.len())
    );
    assert_eq!(ep.calls(), calls, "the cache answered");
}

/// A cached SERVICE whose copy of the answer the budget cannot hold drops the copy and
/// streams the answer on uncached, as the same query does without the cache.
#[test]
fn a_copy_the_budget_cannot_hold_is_dropped() {
    let s = store();
    let snap = s.snapshot();
    let vars: Vec<String> = (0..100).map(|i| format!("\"v{i}\"")).collect();
    let head = format!(
        r#"{{"head":{{"vars":["s",{}]}},"results":{{"bindings":["#,
        vars.join(",")
    );
    let n = 2_000;
    let body = format!(
        "{head}{}{TAIL}",
        (0..n)
            .map(|i| format!(r#"{{"s":{{"type":"uri","value":"urn:s:{i}"}}}}"#))
            .collect::<Vec<_>>()
            .join(",")
    );
    let ep = Endpoint::start(Arc::new(move || {
        (200, vec![Part::Bytes(body.clone().into_bytes())])
    }));
    let plain = format!("SELECT ?s {{ SERVICE <{}> {{ ?s ?p ?o }} }}", ep.url);
    let cached = plain.replace("SERVICE <", "SERVICE <cache:");
    let tight = QueryOptions {
        no_cache: false,
        max_memory_bytes: Some(4 << 20),
        ..opts()
    };
    let expected = streamed(&snap, &plain, &tight, &strict(256)).unwrap().0;
    assert_eq!(expected.len(), n);
    assert_eq!(
        streamed(&snap, &cached, &tight, &strict(256)).unwrap().0,
        expected
    );
    assert_eq!(s.result_cache().service.entries(), 0);
    // with room for the copy, the answer is cached
    let roomy = QueryOptions {
        max_memory_bytes: Some(256 << 20),
        ..tight.clone()
    };
    assert_eq!(
        streamed(&snap, &cached, &roomy, &strict(256)).unwrap().0,
        expected
    );
    assert_eq!(s.result_cache().service.entries(), 1);
}

/// An unpooled caller keeps its unpooled accounting domain even when the local target's
/// options carry a `QueryPool`, and a pooled caller charges only its own pool grant while
/// leaving the target's pool untouched.
#[test]
fn unpooled_caller_can_call_local_target_with_pool_configured() {
    use sparkles_core::sparql::pool::QueryPool;

    struct PooledLocal(Arc<Snapshot>, Arc<QueryPool>);

    impl std::fmt::Debug for PooledLocal {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("PooledLocal")
        }
    }

    impl LocalEndpoints for PooledLocal {
        fn resolve(&self, iri: &str) -> Option<Result<LocalTarget>> {
            (iri == "http://local/sparql").then(|| {
                Ok(LocalTarget {
                    snap: self.0.clone(),
                    options: QueryOptions {
                        pool: Some(self.1.clone()),
                        ..Default::default()
                    },
                    hold: None,
                })
            })
        }
    }

    let s = store();
    let snap = s.snapshot();
    let remote = Store::in_memory(StoreOptions::default());
    let mut nt = String::new();
    for i in 0..10 {
        nt.push_str(&format!("<urn:s:{i}> <urn:p> \"val-{i:04}\" .\n"));
    }
    remote
        .load(&[Source::from_bytes(nt.into_bytes(), RdfFormat::Turtle, None)])
        .unwrap();

    // 1. Unpooled caller calling a pooled local target.
    let target_pool = QueryPool::new(16 << 20);
    let o = QueryOptions {
        local_endpoints: Some(Arc::new(PooledLocal(
            remote.snapshot(),
            target_pool.clone(),
        ))),
        ..opts()
    };
    let q = "SELECT ?s ?o { SERVICE <http://local/sparql> { ?s <urn:p> ?o } }";
    let expected = eager(&snap, q, &o).unwrap();
    assert_eq!(expected.len(), 10);
    for rows in [1, 4] {
        let (answer, plan) = streamed(&snap, q, &o, &strict(rows)).unwrap();
        assert_eq!(answer, expected, "batch {rows}");
        assert!(!materializes(&plan));
        assert_service_streaming(&plan, "unpooled caller against pooled target");
    }
    assert_eq!(target_pool.granted(), 0);

    // Additional coverage: SELECT ... LIMIT 1 and ASK parity.
    let q_lim = "SELECT ?s ?o { SERVICE <http://local/sparql> { ?s <urn:p> ?o } } LIMIT 1";
    let expected_lim = eager(&snap, q_lim, &o).unwrap();
    assert_eq!(expected_lim.len(), 1);
    for rows in [1, 4] {
        let (answer, _) = streamed(&snap, q_lim, &o, &strict(rows)).unwrap();
        assert_eq!(answer, expected_lim, "LIMIT 1 batch {rows}");
    }
    let q_ask = "ASK { SERVICE <http://local/sparql> { ?s <urn:p> ?o } }";
    let eager_ask = query(snap.clone(), q_ask, &o).unwrap().boolean;
    assert!(eager_ask);
    let streamed_ask = match query_execution(
        snap.clone(),
        q_ask,
        &o,
        &strict(1),
        ExecutionMode::Streaming,
    )
    .unwrap()
    {
        QueryExecution::Ask(r) => r.into_result().boolean,
        other => panic!("expected Ask execution, got {:?}", other.kind()),
    };
    assert_eq!(streamed_ask, eager_ask);
    assert_eq!(target_pool.granted(), 0);

    // 2. Caller with a grant from pool A calling a target whose options carry pool B.
    let pool_a = QueryPool::new(64 << 20);
    let pool_b = QueryPool::new(64 << 20);
    let grant_a = Arc::new(pool_a.empty_grant());
    let o_pooled = QueryOptions {
        grant: Some(grant_a.clone()),
        local_endpoints: Some(Arc::new(PooledLocal(remote.snapshot(), pool_b.clone()))),
        ..opts()
    };
    let q_sorted = "SELECT ?s ?o { SERVICE <http://local/sparql> { SELECT ?s ?o { ?s <urn:p> ?o } ORDER BY DESC(?o) } }";
    let mut cursor = select_cursor(snap.clone(), q_sorted, &o_pooled, &strict(4)).unwrap();
    let first = cursor.next_batch().unwrap().expect("first batch");
    assert_eq!(first.len(), 4);
    assert!(
        grant_a.used() > 0,
        "caller grant from pool A should be charged while cursor is live"
    );
    assert_eq!(pool_b.granted(), 0, "target pool B must remain uncharged");
    drop(first);
    let mut total = 4;
    while let Some(b) = cursor.next_batch().unwrap() {
        total += b.len();
    }
    assert_eq!(total, 10);
    cursor.close();
    drop(cursor);
    assert_eq!(
        grant_a.used(),
        0,
        "dropping the cursor must return its owned contribution to pool A"
    );
    assert_eq!(pool_b.granted(), 0);
}

/// Eager SERVICE compacts a long leading whitespace prefix in bounded memory and charges
/// non-whitespace JSON head and late-head staging buffers against the query memory grant.
#[test]
fn eager_service_charges_json_staging_and_streams_long_whitespace_prefix() {
    let s = store();
    let snap = s.snapshot();
    let tight = QueryOptions {
        max_memory_bytes: Some(128 << 10),
        ..opts()
    };

    // 1. Long leading whitespace prefix before a small head-first response succeeds under
    // a tight eager budget.
    let mut ws_payload = vec![b' '; 256 << 10];
    ws_payload.extend_from_slice(
        br#"{"head":{"vars":["s"]},"results":{"bindings":[{"s":{"type":"uri","value":"urn:ok"}}]}}"#,
    );
    let ep_ws = Endpoint::start(Arc::new(move || {
        (200, vec![Part::Bytes(ws_payload.clone())])
    }));
    let q_ws = format!("SELECT ?s {{ SERVICE <{}> {{ ?s ?p ?o }} }}", ep_ws.url);
    let rows = eager(&snap, &q_ws, &tight).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(
        rows[0][0],
        Some(Term::NamedNode(oxrdf::NamedNode::new("urn:ok").unwrap()))
    );

    // 2. Large non-whitespace head exceeds the tight eager memory budget.
    let big_head = format!(
        r#"{{"head":{{"links":["{}"],"vars":["s"]}},"results":{{"bindings":[]}}}}"#,
        "h".repeat(256 << 10)
    )
    .into_bytes();
    let ep_head = Endpoint::start(Arc::new(move || (200, vec![Part::Bytes(big_head.clone())])));
    let q_head = format!("SELECT ?s {{ SERVICE <{}> {{ ?s ?p ?o }} }}", ep_head.url);
    let err = eager(&snap, &q_head, &tight).unwrap_err();
    assert!(
        matches!(err, Error::BudgetExceeded(b) if b.kind == BudgetKind::Memory),
        "expected Memory BudgetExceeded for large eager head, got {err:?}"
    );

    // 3. Large late-head body exceeds the tight eager memory budget.
    let big_late = format!(
        r#"{{"results":{{"bindings":[{{"s":{{"type":"literal","value":"{}"}}}}]}},"head":{{"vars":["s"]}}}}"#,
        "v".repeat(256 << 10)
    )
    .into_bytes();
    let ep_late = Endpoint::start(Arc::new(move || (200, vec![Part::Bytes(big_late.clone())])));
    let q_late = format!("SELECT ?s {{ SERVICE <{}> {{ ?s ?p ?o }} }}", ep_late.url);
    let err = eager(&snap, &q_late, &tight).unwrap_err();
    assert!(
        matches!(err, Error::BudgetExceeded(b) if b.kind == BudgetKind::Memory),
        "expected Memory BudgetExceeded for large eager late-head body, got {err:?}"
    );
}

/// Eager SERVICE refuses responses whose raw JSON bytes fit in the memory grant when
/// either escaped-token parser scratch or late-head buffered bindings exceed the grant.
#[test]
fn eager_service_refuses_escaped_token_scratch_and_late_buffered_bindings() {
    let s = store();
    let snap = s.snapshot();

    // 1. ~27 KiB escaped string in `head.links` fits in a 32 KiB staging Vec<u8> (48 KiB
    // growth peak), while the staging Vec + parser's decoded String exceed 56 KiB.
    let escaped_head = format!(
        r#"{{"head":{{"links":["\"{}"],"vars":["s"]}},"results":{{"bindings":[]}}}}"#,
        "e".repeat(27 << 10)
    )
    .into_bytes();
    let ep_esc = Endpoint::start(Arc::new(move || {
        (200, vec![Part::Bytes(escaped_head.clone())])
    }));
    let q_esc = format!("SELECT ?s {{ SERVICE <{}> {{ ?s ?p ?o }} }}", ep_esc.url);
    let tight_esc = QueryOptions {
        max_memory_bytes: Some(56 << 10),
        ..opts()
    };
    let err = eager(&snap, &q_esc, &tight_esc).unwrap_err();
    assert!(
        matches!(err, Error::BudgetExceeded(b) if b.kind == BudgetKind::Memory),
        "expected Memory BudgetExceeded for eager escaped-token scratch, got {err:?}"
    );

    // 2. 10,000 empty bindings occupy ~30 KiB on the wire (raw staging fits easily in
    // 256 KiB, and a single 4 KiB read's allowance is ~172 KiB), while `sparesults`
    // buffers ~1 MiB of `(Vec<String>, Vec<Term>)` pairs alongside the replay cursor.
    let bindings = vec!["{}"; 10_000].join(",");
    let late_empty =
        format!(r#"{{"results":{{"bindings":[{bindings}]}},"head":{{"vars":["s"]}}}}"#)
            .into_bytes();
    assert!(late_empty.len() < (32 << 10));
    let ep_late = Endpoint::start(Arc::new(move || {
        (200, vec![Part::Bytes(late_empty.clone())])
    }));
    let q_late = format!("SELECT ?s {{ SERVICE <{}> {{ ?s ?p ?o }} }}", ep_late.url);
    let tight_late = QueryOptions {
        max_memory_bytes: Some(256 << 10),
        ..opts()
    };
    let err = eager(&snap, &q_late, &tight_late).unwrap_err();
    assert!(
        matches!(err, Error::BudgetExceeded(b) if b.kind == BudgetKind::Memory),
        "expected Memory BudgetExceeded for eager late-head buffered bindings, got {err:?}"
    );

    // 3. A tiny (< 100 B) complete late-head response succeeds under a 128 KiB eager
    // budget instead of failing before its EOF check read.
    let tiny_late =
        br#"{"results":{"bindings":[{"s":{"type":"uri","value":"urn:tiny"}}]},"head":{"vars":["s"]}}"#
            .to_vec();
    let ep_tiny = Endpoint::start(Arc::new(move || {
        (200, vec![Part::Bytes(tiny_late.clone())])
    }));
    let q_tiny = format!("SELECT ?s {{ SERVICE <{}> {{ ?s ?p ?o }} }}", ep_tiny.url);
    let tight_tiny = QueryOptions {
        max_memory_bytes: Some(128 << 10),
        ..opts()
    };
    assert_eq!(
        eager(&snap, &q_tiny, &tight_tiny).unwrap(),
        vec![vec![Some(Term::NamedNode(
            oxrdf::NamedNode::new("urn:tiny").unwrap()
        ))]]
    );

    // 4. An escape after an 18,000-byte literal prefix in a comma-preceded second link
    // refuses under an 84 KiB eager budget (covering separator skipping and
    // non-power-of-two initial String growth).
    let prefix_escaped = format!(
        r#"{{"head":{{"links":["ok","{}\""],"vars":["s"]}},"results":{{"bindings":[]}}}}"#,
        "p".repeat(18_000)
    )
    .into_bytes();
    let ep_prefix = Endpoint::start(Arc::new(move || {
        (200, vec![Part::Bytes(prefix_escaped.clone())])
    }));
    let q_prefix = format!("SELECT ?s {{ SERVICE <{}> {{ ?s ?p ?o }} }}", ep_prefix.url);
    let tight_prefix = QueryOptions {
        max_memory_bytes: Some(84 << 10),
        ..opts()
    };
    let err = eager(&snap, &q_prefix, &tight_prefix).unwrap_err();
    assert!(
        matches!(err, Error::BudgetExceeded(b) if b.kind == BudgetKind::Memory),
        "expected Memory BudgetExceeded for comma-preceded long-prefix escaped link, got {err:?}"
    );
}

/// Whitespace compaction never turns a misplaced UTF-8 BOM after leading spaces into a
/// valid response in either eager or strict cursor SERVICE, while a valid UTF-8 BOM at
/// byte zero succeeds in both.
#[test]
fn service_rejects_misplaced_bom_after_whitespace_and_accepts_leading_bom() {
    let s = store();
    let snap = s.snapshot();

    let mut misplaced = vec![b' '; 8192];
    misplaced.extend_from_slice(&[0xEF, 0xBB, 0xBF]);
    misplaced.extend_from_slice(
        br#"{"results":{"bindings":[{"s":{"type":"uri","value":"urn:bad"}}]},"head":{"vars":["s"]}}"#,
    );
    let ep_bad = Endpoint::start(Arc::new(move || {
        (200, vec![Part::Bytes(misplaced.clone())])
    }));
    let q_bad = format!("SELECT ?s {{ SERVICE <{}> {{ ?s ?p ?o }} }}", ep_bad.url);
    assert!(
        matches!(eager(&snap, &q_bad, &opts()), Err(Error::Service(_))),
        "eager SERVICE must reject misplaced BOM after discarded whitespace"
    );
    assert!(
        matches!(
            streamed(&snap, &q_bad, &opts(), &strict(1)),
            Err(Error::Service(_))
        ),
        "strict cursor SERVICE must reject misplaced BOM after discarded whitespace"
    );

    let mut leading_bom = vec![0xEF, 0xBB, 0xBF];
    leading_bom.extend(std::iter::repeat_n(b' ', 8192));
    leading_bom.extend_from_slice(
        br#"{"head":{"vars":["s"]},"results":{"bindings":[{"s":{"type":"uri","value":"urn:ok"}}]}}"#,
    );
    let ep_ok = Endpoint::start(Arc::new(move || {
        (200, vec![Part::Bytes(leading_bom.clone())])
    }));
    let q_ok = format!("SELECT ?s {{ SERVICE <{}> {{ ?s ?p ?o }} }}", ep_ok.url);
    let want = vec![vec![Some(Term::NamedNode(
        oxrdf::NamedNode::new("urn:ok").unwrap(),
    ))]];
    assert_eq!(eager(&snap, &q_ok, &opts()).unwrap(), want);
    assert_eq!(streamed(&snap, &q_ok, &opts(), &strict(1)).unwrap().0, want);
}

/// A head-first response with an incomplete > 4 KiB head token delivered across chunks
/// streams its first solution immediately once the token closes and `"results"` + row 1
/// arrive, without blocking on the rest of the open HTTP body.
#[test]
fn strict_service_streams_first_row_after_chunked_large_head_with_open_body() {
    let s = store();
    let snap = s.snapshot();
    let gate = Arc::new(Gate::default());
    let g = gate.clone();

    let mut chunk1 = br#"{"head":{"links":[""#.to_vec();
    chunk1.extend(std::iter::repeat_n(b'a', 5_000));
    let chunk2 =
        br#"","b"],"vars":["s"]},"results":{"bindings":[{"s":{"type":"uri","value":"urn:first"}}"#
            .to_vec();
    let chunk3 = br#"]}}"#.to_vec();

    let ep = Endpoint::start(Arc::new(move || {
        (
            200,
            vec![
                Part::Bytes(chunk1.clone()),
                Part::Bytes(chunk2.clone()),
                Part::Wait(g.clone()),
                Part::Bytes(chunk3.clone()),
            ],
        )
    }));
    let q = format!("SELECT ?s {{ SERVICE <{}> {{ ?s ?p ?o }} }}", ep.url);
    let qopts = QueryOptions {
        timeout: Some(Duration::from_secs(3)),
        ..opts()
    };
    let mut cursor = select_cursor(snap, &q, &qopts, &strict(1)).unwrap();
    let batch = cursor
        .next_batch()
        .expect("first row must arrive before the endpoint closes or sends more body")
        .expect("first row present");
    assert_eq!(batch.len(), 1);
    assert_eq!(
        batch.row(0).unwrap().values(),
        &[Some(Term::NamedNode(
            oxrdf::NamedNode::new("urn:first").unwrap()
        ))]
    );
    gate.open();
    assert!(cursor.next_batch().unwrap().is_none());
}

/// A head-first response with an incomplete > 4 KiB number in a `head` extension member
/// streams its first solution immediately once the number ends and `"results"` + row 1
/// arrive, without blocking on the rest of the open HTTP body.
#[test]
fn strict_service_streams_first_row_after_chunked_large_head_number_with_open_body() {
    let s = store();
    let snap = s.snapshot();
    let gate = Arc::new(Gate::default());
    let g = gate.clone();

    let mut chunk1 = br#"{"head":{"ext":1"#.to_vec();
    chunk1.extend(std::iter::repeat_n(b'9', 5_000));
    let chunk2 =
        br#","vars":["s"]},"results":{"bindings":[{"s":{"type":"uri","value":"urn:first-num"}}"#
            .to_vec();
    let chunk3 = br#"]}}"#.to_vec();

    let ep = Endpoint::start(Arc::new(move || {
        (
            200,
            vec![
                Part::Bytes(chunk1.clone()),
                Part::Bytes(chunk2.clone()),
                Part::Wait(g.clone()),
                Part::Bytes(chunk3.clone()),
            ],
        )
    }));
    let q = format!("SELECT ?s {{ SERVICE <{}> {{ ?s ?p ?o }} }}", ep.url);
    let qopts = QueryOptions {
        timeout: Some(Duration::from_secs(3)),
        ..opts()
    };
    let mut cursor = select_cursor(snap, &q, &qopts, &strict(1)).unwrap();
    let batch = cursor
        .next_batch()
        .expect("first row must arrive before the endpoint closes or sends more body")
        .expect("first row present");
    assert_eq!(batch.len(), 1);
    assert_eq!(
        batch.row(0).unwrap().values(),
        &[Some(Term::NamedNode(
            oxrdf::NamedNode::new("urn:first-num").unwrap()
        ))]
    );
    gate.open();
    assert!(cursor.next_batch().unwrap().is_none());
}
