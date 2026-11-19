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

/// The in-process endpoint `http://local/sparql`, which reads `snap` and may call
/// services itself.
struct Local(Arc<Snapshot>, Arc<()>);

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
                hold: Some(Box::new(self.1.clone())),
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
        let q = format!(
            "SELECT ?a {{ ?a <urn:b> ?v FILTER {filter} {{ SERVICE <http://local/sparql> {{ ?x <urn:p> ?o SERVICE SILENT <{}> {{ ?x ?q ?r }} }} }} }}",
            failing.url
        );
        let expected = eager(&snap, &q, &o).unwrap();
        let r = streamed(&snap, &q, &o, &strict(4));
        assert!(
            matches!(&r, Err(Error::Unsupported(m)) if m.contains("materialization at Service")),
            "{filter}: {:?}",
            r.map(|(rows, _)| rows)
        );
        let allow = CursorOptions {
            fallback: FallbackPolicy::AllowMaterialization,
            ..strict(4)
        };
        assert_eq!(
            streamed(&snap, &q, &o, &allow).unwrap().0,
            expected,
            "{filter}"
        );
        assert_eq!(auto(&snap, &q, &o), expected, "{filter}");
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
    let (answer, _) = streamed(&snap, &q, &opts(), &allow).unwrap();
    assert_eq!(answer, eager(&snap, &q, &opts()).unwrap());
    assert_eq!(answer.len(), 10);
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
