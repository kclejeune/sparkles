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
    let first = cursor.next_batch().unwrap().expect("a first row");
    assert_eq!(first.len(), 1);
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

/// A response that breaks off after some rows fails the cursor after it has handed
/// those rows out, and fails the eager run. A failed request fails both, and a SILENT
/// SERVICE stays an eager leaf: refused by the strict policy, and one empty solution
/// under the permissive one, as in an eager run.
#[test]
fn failures_end_the_stream_and_silent_stays_eager() {
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

    let failing = Endpoint::start(Arc::new(|| (500, vec![Part::Bytes(b"no".to_vec())])));
    let q = format!(
        "SELECT ?s ?o {{ SERVICE <{}> {{ ?s ?p ?o }} }}",
        failing.url
    );
    assert!(matches!(eager(&snap, &q, &opts()), Err(Error::Service(_))));
    let r = streamed(&snap, &q, &opts(), &strict(1));
    assert!(matches!(r, Err(Error::Service(_))), "{:?}", r.map(|r| r.0));

    let silent = q.replace("SERVICE <", "SERVICE SILENT <");
    let unit = vec![vec![None, None]];
    assert_eq!(eager(&snap, &silent, &opts()).unwrap(), unit);
    let allow = CursorOptions {
        fallback: FallbackPolicy::AllowMaterialization,
        ..strict(1)
    };
    assert_eq!(streamed(&snap, &silent, &opts(), &allow).unwrap().0, unit);
    // the empty solution has the SERVICE's variables, unbound, also below a join
    let joined = format!(
        "SELECT ?a ?s ?o {{ ?a <urn:b> ?v SERVICE SILENT <{}> {{ ?s ?p ?o }} }}",
        failing.url
    );
    let expected = eager(&snap, &joined, &opts()).unwrap();
    assert_eq!(expected.len(), 1);
    assert_eq!(
        streamed(&snap, &joined, &opts(), &allow).unwrap().0,
        expected
    );
    let r = streamed(&snap, &silent, &opts(), &strict(1));
    assert!(
        matches!(&r, Err(Error::Unsupported(m)) if m.contains("materialization at Service")),
        "{:?}",
        r.map(|r| r.0)
    );

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
        let allow = CursorOptions {
            fallback: FallbackPolicy::AllowMaterialization,
            ..strict(1)
        };
        let r = streamed(&snap, &q, &denied, &allow);
        assert!(
            matches!(r, Err(Error::NotPermitted(_))),
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
}

/// A remote transfer's timeout counts from the start of its request, while the cursor's
/// deadline leaves out the time its consumer keeps it waiting. A consumer slower than
/// the outbound timeout therefore fails a transfer that an eager run finishes, with the
/// timeout as a SERVICE error.
#[test]
fn a_slow_consumer_can_expire_a_remote_transfer() {
    let s = store();
    let snap = s.snapshot();
    let ep = Endpoint::start(answer(20_000));
    let q = format!("SELECT ?s ?o {{ SERVICE <{}> {{ ?s ?p ?o }} }}", ep.url);
    let short = QueryOptions {
        outbound: OutboundPolicy {
            allow_private: true,
            timeout: Duration::from_millis(500),
            ..Default::default()
        },
        ..opts()
    };
    assert_eq!(eager(&snap, &q, &short).unwrap().len(), 20_000);
    let mut cursor = select_cursor(snap.clone(), &q, &short, &strict(1)).unwrap();
    assert!(cursor.next_batch().unwrap().is_some());
    std::thread::sleep(Duration::from_millis(800));
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

/// The in-process endpoint `http://local/sparql`, which reads `snap` and may call
/// services itself.
struct Local(Arc<Snapshot>);

impl std::fmt::Debug for Local {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Local")
    }
}

impl LocalEndpoints for Local {
    fn resolve(&self, iri: &str) -> Option<Result<LocalTarget>> {
        (iri == "http://local/sparql").then(|| {
            Ok(LocalTarget {
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
                hold: None,
            })
        })
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
    let o = QueryOptions {
        local_endpoints: Some(Arc::new(Local(remote.snapshot()))),
        ..opts()
    };
    let plain = "SELECT ?s ?o { SERVICE <http://local/sparql> { ?s <urn:p> ?o } }";
    let expected = eager(&snap, plain, &o).unwrap();
    assert_eq!(expected.len(), 100);
    for rows in [1, 9, 4096] {
        let (answer, plan) = streamed(&snap, plain, &o, &strict(rows)).unwrap();
        assert_eq!(answer, expected, "{rows}");
        assert!(!materializes(&plan));
    }
    // the callee holds a SILENT SERVICE, which the strict policy refuses
    let failing = Endpoint::start(Arc::new(|| (500, vec![Part::Bytes(b"no".to_vec())])));
    let nested = format!(
        "SELECT ?s ?o {{ SERVICE <http://local/sparql> {{ ?s <urn:p> ?o SERVICE SILENT <{}> {{ ?s ?q ?r }} }} }}",
        failing.url
    );
    let expected = eager(&snap, &nested, &o).unwrap();
    assert_eq!(expected.len(), 100);
    let mut cursor = select_cursor(snap.clone(), &nested, &o, &strict(4)).unwrap();
    let r = cursor.next_batch();
    assert!(
        matches!(&r, Err(Error::Unsupported(m)) if m.contains("materialization at Service")),
        "{:?}",
        r.map(|b| b.map(|b| b.len()))
    );
    let allow = CursorOptions {
        fallback: FallbackPolicy::AllowMaterialization,
        ..strict(4)
    };
    assert_eq!(streamed(&snap, &nested, &o, &allow).unwrap().0, expected);
    // a pause of the caller's consumer past the deadline
    let timed = QueryOptions {
        timeout: Some(Duration::from_millis(300)),
        ..o.clone()
    };
    let mut cursor = select_cursor(snap.clone(), plain, &timed, &strict(1)).unwrap();
    let mut n = cursor.next_batch().unwrap().unwrap().len();
    cursor
        .deadline_pause()
        .store(3_600_000_000_000, Ordering::Relaxed);
    std::thread::sleep(Duration::from_millis(500));
    while let Some(b) = cursor.next_batch().unwrap() {
        n += b.len();
    }
    assert_eq!(n, 100);
}
