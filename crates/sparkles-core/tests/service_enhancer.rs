//! Jena's service enhancer (spec G10): `loop`, `bulk` and `cache` written in front of a
//! SERVICE IRI, and `urn:x-arq:self`, against a Sparkles endpoint that the test runs in
//! process.
//!
//! The data and most queries come from Jena's tests of `jena-serviceenhancer`
//! (`TestServiceEnhancerMisc`, `AbstractTestServiceEnhancerResultSetLimits`): department
//! `d` of `n` employs persons 1 to `n - d + 1`. The endpoint can cut each response short
//! after a number of rows, as Jena's tests do to imitate the result limits of public
//! endpoints.

use oxrdf::Term;
use sparkles_core::Error;
use sparkles_core::io::{RdfFormat, Source};
use sparkles_core::outbound::OutboundPolicy;
use sparkles_core::sparql::results::{SolutionsFormat, write_solutions};
use sparkles_core::sparql::{QueryOptions, query};
use sparkles_core::store::{Store, StoreOptions};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Jena's `createModel(n)` as N-Triples.
fn departments(n: usize) -> String {
    let mut s = String::new();
    let label = "<http://www.w3.org/2000/01/rdf-schema#label>";
    let a = "<http://www.w3.org/1999/02/22-rdf-syntax-ns#type>";
    for d in 1..=n {
        s.push_str(&format!("<urn:dept{d}> {a} <urn:Department> .\n"));
        s.push_str(&format!("<urn:dept{d}> {label} \"Department {d}\" .\n"));
        for e in 1..=(n - d + 1) {
            s.push_str(&format!("<urn:person{e}> {a} <urn:Person> .\n"));
            s.push_str(&format!("<urn:person{e}> {label} \"Person {e}\" .\n"));
            s.push_str(&format!(
                "<urn:dept{d}> <urn:hasEmployee> <urn:person{e}> .\n"
            ));
        }
    }
    s
}

fn store(nt: &str) -> Arc<Store> {
    let s = Store::in_memory(StoreOptions::default());
    s.load(&[Source::from_bytes(
        nt.as_bytes().to_vec(),
        RdfFormat::NTriples,
        None,
    )])
    .unwrap();
    Arc::new(s)
}

/// A SPARQL endpoint over a store, answering in SPARQL JSON results and recording the
/// queries it was sent.
struct Endpoint {
    port: u16,
    queries: Arc<Mutex<Vec<String>>>,
    /// rows per response, after which the endpoint stops (`usize::MAX`: no limit)
    limit: Arc<AtomicUsize>,
    /// answer every request with an error
    failing: Arc<AtomicBool>,
    /// the most requests answered at once
    peak: Arc<AtomicUsize>,
}

impl Endpoint {
    fn start(data: Arc<Store>, delay: Duration) -> Endpoint {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        let queries = Arc::new(Mutex::new(Vec::new()));
        let limit = Arc::new(AtomicUsize::new(usize::MAX));
        let failing = Arc::new(AtomicBool::new(false));
        let inflight = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));
        let (q, lim, fail) = (queries.clone(), limit.clone(), failing.clone());
        let (now, most) = (inflight.clone(), peak.clone());
        std::thread::spawn(move || {
            for c in l.incoming() {
                let Ok(c) = c else { continue };
                let (data, q, lim, fail) = (data.clone(), q.clone(), lim.clone(), fail.clone());
                let (now, most) = (now.clone(), most.clone());
                std::thread::spawn(move || {
                    let n = now.fetch_add(1, Ordering::SeqCst) + 1;
                    most.fetch_max(n, Ordering::SeqCst);
                    answer(c, &data, &q, &lim, &fail, delay);
                    now.fetch_sub(1, Ordering::SeqCst);
                });
            }
        });
        Endpoint {
            port,
            queries,
            limit,
            failing,
            peak,
        }
    }

    fn url(&self) -> String {
        format!("http://127.0.0.1:{}/sparql", self.port)
    }

    fn take(&self) -> Vec<String> {
        std::mem::take(&mut *self.queries.lock().unwrap())
    }

    /// The most requests answered at once since the last call.
    fn take_peak(&self) -> usize {
        self.peak.swap(0, Ordering::SeqCst)
    }
}

fn answer(
    mut c: TcpStream,
    data: &Store,
    queries: &Mutex<Vec<String>>,
    limit: &AtomicUsize,
    failing: &AtomicBool,
    delay: Duration,
) {
    let _ = c.set_read_timeout(Some(Duration::from_secs(10)));
    let mut buf = Vec::new();
    let mut chunk = [0u8; 8192];
    let head_end = loop {
        if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break i + 4;
        }
        match c.read(&mut chunk) {
            Ok(0) | Err(_) => return,
            Ok(k) => buf.extend_from_slice(&chunk[..k]),
        }
    };
    let head = String::from_utf8_lossy(&buf[..head_end]).to_string();
    let len: usize = head
        .lines()
        .find_map(|l| {
            let (k, v) = l.split_once(':')?;
            k.eq_ignore_ascii_case("content-length")
                .then(|| v.trim().parse().ok())?
        })
        .unwrap_or(0);
    while buf.len() < head_end + len {
        match c.read(&mut chunk) {
            Ok(0) | Err(_) => break,
            Ok(k) => buf.extend_from_slice(&chunk[..k]),
        }
    }
    let body = String::from_utf8_lossy(&buf[head_end..]).to_string();
    let form = reqwest::Url::parse(&format!("http://x/?{body}")).unwrap();
    let q = form
        .query_pairs()
        .find(|(k, _)| k == "query")
        .map(|(_, v)| v.to_string())
        .unwrap_or_default();
    queries.lock().unwrap().push(q.clone());
    std::thread::sleep(delay);
    let (status, body) = if failing.load(Ordering::SeqCst) {
        ("500 Internal Server Error", b"no".to_vec())
    } else {
        match query(data.snapshot(), &q, &QueryOptions::default()) {
            Ok(r) => {
                let n = limit.load(Ordering::SeqCst);
                let mut out = Vec::new();
                write_solutions(&r, SolutionsFormat::Json, &mut out, Some(n)).unwrap();
                ("200 OK", out)
            }
            Err(e) => ("400 Bad Request", e.to_string().into_bytes()),
        }
    };
    let head = format!(
        "HTTP/1.1 {status}\r\nconnection: close\r\ncontent-type: application/sparql-results+json\r\ncontent-length: {}\r\n\r\n",
        body.len()
    );
    let _ = c.write_all(head.as_bytes());
    let _ = c.write_all(&body);
}

fn opts() -> QueryOptions {
    QueryOptions {
        allow_service: true,
        outbound: OutboundPolicy {
            allow_private: true,
            ..Default::default()
        },
        ..Default::default()
    }
}

fn short(t: &Option<Term>) -> String {
    match t {
        None => "-".into(),
        Some(Term::NamedNode(n)) => n.as_str().trim_start_matches("urn:").to_string(),
        Some(Term::Literal(l)) => l.value().to_string(),
        Some(t) => t.to_string(),
    }
}

fn run(local: &Store, q: &str, o: &QueryOptions) -> sparkles_core::Result<Vec<String>> {
    let r = query(local.snapshot(), q, o)?;
    let mut rows: Vec<String> = r
        .rows()?
        .iter()
        .map(|row| row.values().iter().map(short).collect::<Vec<_>>().join(" "))
        .collect();
    rows.sort();
    Ok(rows)
}

fn rows(local: &Store, q: &str) -> Vec<String> {
    run(local, q, &opts()).unwrap_or_else(|e| panic!("{q}: {e}"))
}

/// `loop:` substitutes each department into the sub-select, so each gets its own top
/// employee; a plain SERVICE runs the sub-select once (`testLoopJoinWithScope`,
/// `testStdJoinWithScope`). With `bulk` the departments go five or three to a request,
/// with the same answer.
#[test]
fn loop_runs_the_pattern_per_input() {
    let data = store(&departments(9));
    let ep = Endpoint::start(data.clone(), Duration::ZERO);
    let q = |mode: &str| {
        format!(
            "SELECT ?s ?o {{ {{ SELECT DISTINCT ?s {{ ?s a <urn:Department> ; ?p ?o }} ORDER BY ?s }} \
             SERVICE <{mode}> {{ SELECT ?o {{ ?s <urn:hasEmployee> ?o }} ORDER BY DESC(?o) LIMIT 1 }} }}"
        )
    };
    let expected: Vec<String> = (1..=9)
        .map(|d| format!("dept{d} person{}", 10 - d))
        .collect();
    let url = ep.url();
    assert_eq!(rows(&data, &q(&format!("loop:{url}"))), expected);
    assert_eq!(ep.take().len(), 9);
    assert_eq!(rows(&data, &q(&format!("loop:bulk+5:{url}"))), expected);
    let sent = ep.take();
    assert_eq!(sent.len(), 2, "{sent:?}");
    // a sub-select with a slice is sent as Jena's UNION of substituted patterns
    assert!(
        sent[0].contains("UNION") && sent[0].contains("<urn:dept1>"),
        "{}",
        sent[0]
    );
    assert_eq!(
        rows(&data, &q(&format!("loop:bulk+3:cache:{url}"))),
        expected
    );
    assert_eq!(ep.take().len(), 3);
    // options alone over a SERVICE directly inside apply to that SERVICE
    let nested = format!(
        "SELECT ?s ?o {{ {{ SELECT DISTINCT ?s {{ ?s a <urn:Department> }} }} \
         SERVICE <loop:bulk+5:> {{ SERVICE <{url}> {{ SELECT ?o {{ ?s <urn:hasEmployee> ?o }} ORDER BY DESC(?o) LIMIT 1 }} }} }}"
    );
    assert_eq!(rows(&data, &nested), expected);
    assert_eq!(ep.take().len(), 2);
    // the dataset itself, without a request
    assert_eq!(rows(&data, &q("loop:")), expected);
    assert_eq!(rows(&data, &q("loop:urn:x-arq:self")), expected);
    assert!(ep.take().is_empty());
    // without loop, every department gets the overall top employee
    let all: Vec<String> = (1..=9).map(|d| format!("dept{d} person9")).collect();
    assert_eq!(rows(&data, &q(&url)), all);
    assert_eq!(rows(&data, &q("urn:x-arq:self")), all);
}

/// Jena's `testScopeSimple`: `loop:` reaches a variable a sub-select does not project.
#[test]
fn loop_substitutes_through_sub_selects() {
    let data = store(&departments(9));
    let q = |mode: &str| {
        format!(
            "SELECT ?p ?c {{ BIND(<http://www.w3.org/1999/02/22-rdf-syntax-ns#type> AS ?p) \
             SERVICE <{mode}> {{ SELECT (COUNT(*) AS ?c) {{ ?s ?p ?o }} }} }}"
        )
    };
    let ep = Endpoint::start(data.clone(), Duration::ZERO);
    let rdf_type = "http://www.w3.org/1999/02/22-rdf-syntax-ns#type";
    assert_eq!(rows(&data, &q("loop:")), [format!("{rdf_type} 18")]);
    assert_eq!(
        rows(&data, &q(&format!("loop:{}", ep.url()))),
        [format!("{rdf_type} 18")]
    );
    // LATERAL keeps the sub-select's scope: the count of every triple
    let total = data.snapshot().len();
    assert_eq!(
        rows(
            &data,
            &format!(
                "SELECT ?p ?c {{ BIND(<{rdf_type}> AS ?p) LATERAL {{ SELECT (COUNT(*) AS ?c) {{ ?s ?p ?o }} }} }}"
            )
        ),
        [format!("{rdf_type} {total}")]
    );
}

/// A pattern of triples and filters goes out as one VALUES block per request.
#[test]
fn bulk_requests_use_values_blocks() {
    let data = store(&departments(9));
    let ep = Endpoint::start(data.clone(), Duration::ZERO);
    let q = |mode: &str| {
        format!(
            "SELECT ?d ?p {{ ?d a <urn:Department> SERVICE <{mode}> {{ ?d <urn:hasEmployee> ?p FILTER(?p != <urn:person1>) }} }}"
        )
    };
    let plain = rows(&data, &q(&ep.url()));
    assert_eq!(plain.len(), 45 - 9);
    ep.take();
    assert_eq!(rows(&data, &q(&format!("loop:bulk+4:{}", ep.url()))), plain);
    let sent = ep.take();
    assert_eq!(sent.len(), 3);
    assert!(sent[0].contains("VALUES ( ?d ?__idx__ )"), "{}", sent[0]);
    // `bulk` alone takes the configured size, capped at the maximum
    let mut o = opts();
    o.outbound.service_bulk_size = 5;
    assert_eq!(
        run(&data, &q(&format!("loop:bulk:{}", ep.url())), &o).unwrap(),
        plain
    );
    assert_eq!(ep.take().len(), 2);
    o.outbound.service_bulk_max = 3;
    assert_eq!(
        run(&data, &q(&format!("loop:bulk+100:{}", ep.url())), &o).unwrap(),
        plain
    );
    assert_eq!(ep.take().len(), 3);
    // EXPLAIN reports the requests
    let (_, plan) = sparkles_core::sparql::explain(
        data.snapshot(),
        &q(&format!("loop:bulk+4:{}", ep.url())),
        &opts(),
    )
    .unwrap();
    let text = serde_json::to_string(&plan).unwrap();
    assert!(text.contains("4 per request as VALUES"), "{text}");
}

/// Jena's `AbstractTestServiceEnhancerResultSetLimits`: an endpoint that stops after a
/// number of rows gives each input what a request of its own would get.
#[test]
fn bulk_requests_cut_short_are_sent_again_per_input() {
    let data = store(&departments(4));
    let ep = Endpoint::start(data.clone(), Duration::ZERO);
    for mode in ["loop:", "loop:bulk+5:", "loop:bulk+5:cache:"] {
        for (dir, limit, expected) in [
            ("ASC", 1, 4),
            ("ASC", 2, 7),
            ("DESC", 1, 4),
            ("DESC", 2, 7),
            ("ASC", 10, 10),
        ] {
            data.result_cache().clear();
            data.result_cache().service.clear().unwrap();
            ep.limit.store(limit, Ordering::SeqCst);
            let q = format!(
                "SELECT * {{ {{ SELECT ?d {{ ?d a <urn:Department> }} ORDER BY {dir}(?d) }} \
                 SERVICE <{mode}{}> {{ ?d <urn:hasEmployee> ?p }} }}",
                ep.url()
            );
            assert_eq!(rows(&data, &q).len(), expected, "{mode} {dir} {limit}");
        }
    }
}

/// The cache keeps each input's solutions: a repeated query sends nothing, a new input
/// only its own request, and `cache+clear`, `cache+off`, `no_cache`, another caller's
/// scope and a cleared cache all fetch again.
#[test]
fn cache_per_input_and_per_caller() {
    let data = store(&departments(9));
    let ep = Endpoint::start(data.clone(), Duration::ZERO);
    let q = |values: &str, cache: &str| {
        format!(
            "SELECT ?d ?p {{ VALUES ?d {{ {values} }} SERVICE <loop:bulk+5:{cache}:{}> {{ ?d <urn:hasEmployee> ?p }} }}",
            ep.url()
        )
    };
    let three = "<urn:dept1> <urn:dept2> <urn:dept3>";
    let first = rows(&data, &q(three, "cache"));
    assert_eq!(first.len(), 9 + 8 + 7);
    assert_eq!(ep.take().len(), 1);
    assert_eq!(rows(&data, &q(three, "cache")), first);
    assert!(ep.take().is_empty());
    // fewer inputs: all from the cache; one more: only its request
    assert_eq!(rows(&data, &q("<urn:dept2>", "cache")).len(), 8);
    assert!(ep.take().is_empty());
    assert_eq!(
        rows(&data, &q(&format!("{three} <urn:dept4>"), "cache")).len(),
        24 + 6
    );
    let sent = ep.take();
    assert_eq!(sent.len(), 1);
    assert!(
        sent[0].contains("<urn:dept4>") && !sent[0].contains("<urn:dept1>"),
        "{}",
        sent[0]
    );
    // the controls
    assert_eq!(rows(&data, &q(three, "cache+clear")), first);
    assert_eq!(ep.take().len(), 1);
    assert_eq!(rows(&data, &q(three, "cache+off")), first);
    assert_eq!(ep.take().len(), 1);
    let mut o = opts();
    o.no_cache = true;
    assert_eq!(run(&data, &q(three, "cache"), &o).unwrap(), first);
    assert_eq!(ep.take().len(), 1);
    // another caller has its own entries
    let mut bob = opts();
    bob.service_scope = Some("bob".into());
    assert_eq!(run(&data, &q(three, "cache"), &bob).unwrap(), first);
    assert_eq!(ep.take().len(), 1);
    assert_eq!(run(&data, &q(three, "cache"), &bob).unwrap(), first);
    assert!(ep.take().is_empty());
    // a caller without the federate permission is refused, cached or not
    let mut denied = opts();
    denied.forbid_service = true;
    assert!(matches!(
        run(&data, &q(three, "cache"), &denied),
        Err(Error::NotPermitted(_))
    ));
    let mut off = opts();
    off.allow_service = false;
    assert!(run(&data, &q(three, "cache"), &off).is_err());
    assert!(ep.take().is_empty());
    let svc = &data.result_cache().service;
    assert!(svc.entries() > 0 && svc.bytes() > 0 && svc.hits() > 0);
    data.result_cache().service.clear().unwrap();
    assert_eq!(rows(&data, &q(three, "cache")), first);
    assert_eq!(ep.take().len(), 1);
    // without loop, the whole result is one entry
    let whole = format!(
        "SELECT ?d ?p {{ SERVICE <cache:{}> {{ ?d <urn:hasEmployee> ?p }} }}",
        ep.url()
    );
    assert_eq!(rows(&data, &whole).len(), 45);
    assert_eq!(rows(&data, &whole).len(), 45);
    assert_eq!(ep.take().len(), 1);
}

/// `OPTIONAL { SERVICE <loop:…> }` keeps the inputs without solutions, and SILENT keeps
/// the inputs of a failed request.
#[test]
fn optional_and_silent_loops() {
    let data = store(&departments(9));
    let ep = Endpoint::start(data.clone(), Duration::ZERO);
    let q = format!(
        "SELECT ?d ?p {{ ?d a <urn:Department> OPTIONAL {{ SERVICE <loop:bulk+4:{}> \
         {{ ?d <urn:hasEmployee> ?p FILTER(?p = <urn:person5>) }} }} }}",
        ep.url()
    );
    let r = rows(&data, &q);
    assert_eq!(r.len(), 9);
    assert_eq!(r.iter().filter(|r| r.ends_with("person5")).count(), 5);
    ep.failing.store(true, Ordering::SeqCst);
    let loud = format!(
        "SELECT ?d ?p {{ ?d a <urn:Department> SERVICE <loop:bulk+4:{}> {{ ?d <urn:hasEmployee> ?p }} }}",
        ep.url()
    );
    assert!(matches!(run(&data, &loud, &opts()), Err(Error::Service(_))));
    let silent = loud.replace("SERVICE <", "SERVICE SILENT <");
    assert_eq!(rows(&data, &silent).len(), 9);
}

/// A plain SERVICE inside a `LATERAL` that runs per row is sent with the row's values,
/// and so are initial bindings.
#[test]
fn lateral_and_initial_bindings_reach_the_service() {
    let data = store(&departments(3));
    let ep = Endpoint::start(data.clone(), Duration::ZERO);
    let q = format!(
        "SELECT ?d ?p {{ ?d a <urn:Department> LATERAL {{ SERVICE <{}> {{ SELECT ?d ?p {{ ?d <urn:hasEmployee> ?p }} LIMIT 1 }} }} }}",
        ep.url()
    );
    assert_eq!(rows(&data, &q).len(), 3);
    let sent = ep.take();
    assert_eq!(sent.len(), 3);
    assert!(sent.iter().all(|s| s.contains("<urn:dept")), "{sent:?}");
    let mut o = opts();
    o.initial_bindings = vec![(
        "d".into(),
        oxrdf::NamedNode::new("urn:dept2").unwrap().into(),
    )];
    let q = format!(
        "SELECT ?d ?p {{ SERVICE <{}> {{ ?d <urn:hasEmployee> ?p }} }}",
        ep.url()
    );
    assert_eq!(run(&data, &q, &o).unwrap().len(), 2);
    assert!(ep.take()[0].contains("<urn:dept2>"));
}

/// The measurements of spec G10: a correlated federated query, one input per request,
/// in bulk, and from a cold and a warm cache, against an endpoint that adds `DELAY_MS`
/// to each request. Run with
/// `cargo test --release -p sparkles-core --test service_enhancer -- --ignored --nocapture`.
#[test]
#[ignore]
fn measure_bulk_and_cache() {
    let n: usize = std::env::var("DEPARTMENTS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(200);
    let delay = Duration::from_millis(
        std::env::var("DELAY_MS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(0),
    );
    let data = store(&departments(n));
    let ep = Endpoint::start(data.clone(), delay);
    let local = store(&departments(n));
    let q = |mode: &str| {
        format!(
            "SELECT ?d ?l {{ ?d a <urn:Department> SERVICE <{mode}{}> \
             {{ SELECT ?l {{ ?d <urn:hasEmployee> ?p . ?p <http://www.w3.org/2000/01/rdf-schema#label> ?l }} ORDER BY ?l LIMIT 1 }} }}",
            ep.url()
        )
    };
    let flat = |mode: &str| {
        format!(
            "SELECT ?d ?p {{ ?d a <urn:Department> SERVICE <{mode}{}> {{ ?d <urn:hasEmployee> ?p }} }}",
            ep.url()
        )
    };
    let time = |name: &str, q: &str, clear: bool| {
        let mut best = f64::MAX;
        let mut rows = 0;
        let mut reqs = 0;
        for _ in 0..3 {
            if clear {
                local.result_cache().service.clear().unwrap();
            }
            ep.take();
            let t = Instant::now();
            let r = query(local.snapshot(), q, &opts()).unwrap();
            best = best.min(t.elapsed().as_secs_f64() * 1000.0);
            rows = r.len();
            reqs = ep.take().len();
        }
        println!("{name:<40} {rows:>7} rows {reqs:>6} requests {best:>9.1} ms");
    };
    println!("{n} departments, {} ms per request", delay.as_millis());
    time("top label, loop, 1 per request", &q("loop:"), true);
    time("top label, loop:parallel+4", &q("loop:parallel+4:"), true);
    time("top label, loop:bulk+10", &q("loop:bulk+10:"), true);
    time(
        "top label, loop:bulk+10:parallel+4",
        &q("loop:bulk+10:parallel+4:"),
        true,
    );
    time("top label, loop:bulk+100", &q("loop:bulk+100:"), true);
    time(
        "top label, loop:bulk+100:cache, cold",
        &q("loop:bulk+100:cache:"),
        true,
    );
    time(
        "top label, loop:bulk+100:cache, warm",
        &q("loop:bulk+100:cache:"),
        false,
    );
    time("employees, plain SERVICE", &flat(""), true);
    time("employees, loop, 1 per request", &flat("loop:"), true);
    time(
        "employees, loop:parallel+4",
        &flat("loop:parallel+4:"),
        true,
    );
    time(
        "employees, loop:bulk+10 (VALUES)",
        &flat("loop:bulk+10:"),
        true,
    );
    time(
        "employees, loop:bulk+100 (VALUES)",
        &flat("loop:bulk+100:"),
        true,
    );
    time(
        "employees, loop:bulk+100:cache, warm",
        &flat("loop:bulk+100:cache:"),
        false,
    );
}

/// A LIMIT without OFFSET over a SERVICE that reads no input is sent with the remote
/// query, through a projection and a BIND, in both execution modes, so the endpoint
/// returns only the rows the LIMIT keeps (roadmap item 7). An OFFSET, a FILTER between
/// them or a loop sends the query without it.
#[test]
fn a_limit_reaches_the_remote_query() {
    use sparkles_core::sparql::{CursorOptions, ExecutionMode, QueryExecution, query_execution};
    let data = store(&departments(9));
    let ep = Endpoint::start(data.clone(), Duration::ZERO);
    let url = ep.url();
    let count = |q: &str, mode: ExecutionMode| -> usize {
        match query_execution(data.snapshot(), q, &opts(), &CursorOptions::default(), mode)
            .unwrap_or_else(|e| panic!("{q}: {e}"))
        {
            QueryExecution::Select(mut c) => {
                let mut n = 0;
                while let Some(b) = c.next_batch().unwrap() {
                    n += b.len();
                }
                n
            }
            QueryExecution::Eager(r) => r.into_result().len(),
            _ => unreachable!("a SELECT"),
        }
    };
    for mode in [ExecutionMode::Eager, ExecutionMode::Streaming] {
        let q = format!(
            "SELECT ?p ?n {{ SERVICE <{url}> {{ ?d <urn:hasEmployee> ?p }} BIND(STR(?p) AS ?n) }} LIMIT 4"
        );
        assert_eq!(count(&q, mode), 4, "{mode:?}");
        let sent = ep.take();
        assert_eq!(sent.len(), 1, "{sent:?}");
        assert!(
            sent[0].trim_end().ends_with("LIMIT 4"),
            "{mode:?}: {}",
            sent[0]
        );
        // the remote solutions, not the rows a later operator drops, are limited
        for q in [
            format!(
                "SELECT ?p {{ SERVICE <{url}> {{ ?d <urn:hasEmployee> ?p }} }} OFFSET 2 LIMIT 4"
            ),
            format!(
                "SELECT ?p {{ SERVICE <{url}> {{ ?d <urn:hasEmployee> ?p }} FILTER(?d = <urn:dept1>) }} LIMIT 4"
            ),
        ] {
            assert_eq!(count(&q, mode), 4, "{q}");
            let sent = ep.take();
            assert!(sent.iter().all(|s| !s.contains("LIMIT")), "{q}: {sent:?}");
        }
    }
}

/// A remote loop runs as a cursor operator over its input's batches (item 829). It
/// holds the rows of a partial bulk request until more inputs arrive and keeps the
/// answers of the inputs it has sent, so at every batch size it sends the requests one
/// eager run sends, with the same answer, the same counters and the same note, even
/// when a later batch repeats an input.
#[test]
fn a_streamed_loop_sends_the_requests_of_an_eager_run() {
    use sparkles_core::sparql::{CursorOptions, FallbackPolicy, select_cursor};
    let data = store(&departments(9));
    let ep = Endpoint::start(data.clone(), Duration::ZERO);
    let streamed = |q: &str, rows: usize| {
        let o = CursorOptions {
            batch_rows: rows,
            fallback: FallbackPolicy::RejectMaterialization,
            ..Default::default()
        };
        let mut c =
            select_cursor(data.snapshot(), q, &opts(), &o).unwrap_or_else(|e| panic!("{q}: {e}"));
        let mut out = Vec::new();
        while let Some(b) = c.next_batch().unwrap() {
            for r in 0..b.len() {
                out.push(
                    b.row(r)
                        .unwrap()
                        .values()
                        .iter()
                        .map(short)
                        .collect::<Vec<_>>()
                        .join(" "),
                );
            }
        }
        out.sort();
        (out, format!("{:?}", c.plan()))
    };
    for q in [
        // nine inputs, four to a request
        format!(
            "SELECT ?d ?p {{ ?d a <urn:Department> SERVICE <loop:bulk+4:{}> {{ ?d <urn:hasEmployee> ?p FILTER(?p != <urn:person1>) }} }}",
            ep.url()
        ),
        // each input twice, the second time in a later batch
        format!(
            "SELECT ?d ?x ?p {{ VALUES ?x {{ 1 2 }} ?d a <urn:Department> SERVICE <loop:bulk+4:{}> {{ ?d <urn:hasEmployee> ?p FILTER(?p != <urn:person1>) }} }}",
            ep.url()
        ),
        // an optional loop keeps the rows without solutions
        format!(
            "SELECT ?d ?p {{ ?d a <urn:Department> OPTIONAL {{ SERVICE <loop:bulk+4:{}> {{ ?d <urn:hasEmployee> ?p FILTER(?p = <urn:person5>) }} }} }}",
            ep.url()
        ),
    ] {
        let expected = rows(&data, &q);
        let eager = ep.take().len();
        assert!(eager > 1, "{q}");
        for batch in [1, 2, 3, 4096] {
            let (answer, plan) = streamed(&q, batch);
            assert_eq!(answer, expected, "{q}; {batch}");
            let sent = ep.take().len();
            assert_eq!(sent, eager, "{q}; {batch}: requests");
            let note = format!("9 inputs, {eager} requests, 0 from the cache");
            assert!(plan.contains(&note), "{q}; {batch}: {plan}");
            assert!(
                plan.contains(&format!("\"serviceRequests\": Number({eager})")),
                "{plan}"
            );
        }
    }
    // a failing endpoint fails the stream, and SILENT gives each input one empty
    // solution, as in eager runs
    ep.failing.store(true, Ordering::SeqCst);
    let loud = format!(
        "SELECT ?d ?p {{ ?d a <urn:Department> SERVICE <loop:bulk+4:{}> {{ ?d <urn:hasEmployee> ?p }} }}",
        ep.url()
    );
    let o = CursorOptions {
        batch_rows: 1,
        fallback: FallbackPolicy::RejectMaterialization,
        ..Default::default()
    };
    let mut c = select_cursor(data.snapshot(), &loud, &opts(), &o).unwrap();
    let mut failed = None;
    loop {
        match c.next_batch() {
            Ok(Some(_)) => {}
            Ok(None) => break,
            Err(e) => {
                failed = Some(e);
                break;
            }
        }
    }
    assert!(matches!(failed, Some(Error::Service(_))), "{failed:?}");
    let silent = loud.replace("SERVICE <", "SERVICE SILENT <");
    assert_eq!(streamed(&silent, 1).0.len(), 9);
    assert_eq!(streamed(&silent, 1).0, rows(&data, &silent));
}

/// A streamed loop counts the rows it joins against the work limit as an eager run
/// does: both count the same rows, and a limit one row short of that count stops both
/// (item 829).
#[test]
fn a_streamed_loop_counts_its_rows_as_an_eager_run_does() {
    use sparkles_core::BudgetKind;
    use sparkles_core::sparql::{CursorOptions, FallbackPolicy, select_cursor};
    use std::sync::atomic::AtomicU64;
    let data = store(&departments(9));
    let ep = Endpoint::start(data.clone(), Duration::ZERO);
    let q = format!(
        "SELECT ?d ?p {{ ?d a <urn:Department> SERVICE <loop:bulk+4:{}> {{ ?d <urn:hasEmployee> ?p }} }}",
        ep.url()
    );
    let with = |limit: Option<u64>, work: Option<Arc<AtomicU64>>| QueryOptions {
        no_cache: true,
        max_rows_produced: limit,
        work,
        ..opts()
    };
    let eager = query(data.snapshot(), &q, &with(None, None)).unwrap();
    let produced = eager.rows_produced;
    assert!(eager.len() > 9, "{q}");
    let streamed = |limit: Option<u64>, rows: usize| -> Result<u64, Error> {
        let work = Arc::new(AtomicU64::new(0));
        let o = CursorOptions {
            batch_rows: rows,
            fallback: FallbackPolicy::RejectMaterialization,
            ..Default::default()
        };
        let mut c = select_cursor(data.snapshot(), &q, &with(limit, Some(work.clone())), &o)?;
        while c.next_batch()?.is_some() {}
        Ok(work.load(Ordering::Relaxed))
    };
    for rows in [1, 3, 4096] {
        assert_eq!(streamed(None, rows).unwrap(), produced, "{rows}");
        assert!(streamed(Some(produced), rows).is_ok(), "{rows}");
        assert!(
            matches!(streamed(Some(produced - 1), rows), Err(Error::BudgetExceeded(b)) if b.kind == BudgetKind::RowsProduced),
            "{rows}"
        );
    }
    assert!(matches!(
        query(data.snapshot(), &q, &with(Some(produced - 1), None)),
        Err(Error::BudgetExceeded(b)) if b.kind == BudgetKind::RowsProduced
    ));
}

/// A streamed remote loop holds what it keeps across batches under the memory budget:
/// each distinct key of its input rows, as well as the answers. Blank-node keys all send
/// the same unbound request, and its answer is empty, so only the keys grow. A budget
/// that cannot hold the input refuses the eager run, and it refuses the streamed run with
/// a memory error too, while a larger budget answers (Sol review of item 829, finding 2).
#[test]
fn a_streamed_loop_charges_the_keys_it_keeps() {
    use sparkles_core::BudgetKind;
    use sparkles_core::sparql::{CursorOptions, FallbackPolicy, select_cursor};
    let n = 20_000;
    let data = store(
        &(0..n)
            .map(|i| format!("_:b{i} <urn:q> <urn:x> .\n"))
            .collect::<String>(),
    );
    let ep = Endpoint::start(data.clone(), Duration::ZERO);
    let q = format!(
        "SELECT ?d ?p {{ ?d <urn:q> <urn:x> SERVICE <loop:{}> {{ ?d <urn:none> ?p }} }}",
        ep.url()
    );
    let with = |memory: Option<u64>| QueryOptions {
        no_cache: true,
        max_memory_bytes: memory,
        ..opts()
    };
    let streamed = |memory: Option<u64>| -> Result<usize, Error> {
        let o = CursorOptions {
            batch_rows: 64,
            fallback: FallbackPolicy::RejectMaterialization,
            ..Default::default()
        };
        let mut c = select_cursor(data.snapshot(), &q, &with(memory), &o)?;
        let mut rows = 0;
        while let Some(b) = c.next_batch()? {
            rows += b.len();
        }
        Ok(rows)
    };
    let memory = |r: &Result<usize, Error>| matches!(r, Err(Error::BudgetExceeded(b)) if b.kind == BudgetKind::Memory);
    let tight = Some(100 << 10);
    assert_eq!(query(data.snapshot(), &q, &with(None)).unwrap().len(), 0);
    assert_eq!(streamed(None).unwrap(), 0);
    assert_eq!(ep.take().len(), 2, "one request per run");
    let r = query(data.snapshot(), &q, &with(tight)).map(|r| r.len());
    assert!(memory(&r), "eager: {r:?}");
    let r = streamed(tight);
    assert!(memory(&r), "streamed: {r:?}");
    assert_eq!(streamed(Some(64 << 20)).unwrap(), 0);
}

/// A streamed remote loop admits each answer as it arrives, so a budget that the answers
/// outgrow stops the loop before it sends the rest of its requests (Sol review of item
/// 829, round 2, finding 2).
#[test]
fn a_streamed_loop_admits_each_answer_before_the_next_request() {
    use sparkles_core::BudgetKind;
    use sparkles_core::sparql::{CursorOptions, FallbackPolicy, select_cursor};
    let data = store(&departments(40));
    let ep = Endpoint::start(data.clone(), Duration::ZERO);
    let q = format!(
        "SELECT ?d ?p ?l {{ ?d a <urn:Department> SERVICE <loop:{}> {{ ?d <urn:hasEmployee> ?p . ?p <http://www.w3.org/2000/01/rdf-schema#label> ?l }} }}",
        ep.url()
    );
    let streamed = |memory: Option<u64>| -> Result<usize, Error> {
        let o = CursorOptions {
            batch_rows: 64,
            fallback: FallbackPolicy::RejectMaterialization,
            ..Default::default()
        };
        let opts = QueryOptions {
            no_cache: true,
            max_memory_bytes: memory,
            ..opts()
        };
        let mut c = select_cursor(data.snapshot(), &q, &opts, &o)?;
        let mut rows = 0;
        while let Some(b) = c.next_batch()? {
            rows += b.len();
        }
        Ok(rows)
    };
    assert_eq!(streamed(None).unwrap(), 820);
    assert_eq!(ep.take().len(), 40, "one request per department");
    let r = streamed(Some(48 << 10));
    assert!(
        matches!(&r, Err(Error::BudgetExceeded(b)) if b.kind == BudgetKind::Memory),
        "{r:?}"
    );
    let sent = ep.take().len();
    assert!(sent < 40, "the loop sent {sent} requests past its budget");
}

/// A bulk request cut short is sent again one input at a time, and the streamed loop
/// admits each of those answers before it sends the next (Sol review of item 829, round
/// 3, finding 2).
#[test]
fn a_streamed_loop_admits_each_retried_answer() {
    use sparkles_core::BudgetKind;
    use sparkles_core::sparql::{CursorOptions, FallbackPolicy, select_cursor};
    let data = store(&departments(40));
    let ep = Endpoint::start(data.clone(), Duration::ZERO);
    let q = format!(
        "SELECT ?d ?p ?l {{ ?d a <urn:Department> SERVICE <loop:bulk+40:{}> {{ ?d <urn:hasEmployee> ?p . ?p <http://www.w3.org/2000/01/rdf-schema#label> ?l }} }}",
        ep.url()
    );
    // Every response stops after 30 rows, so the bulk request lacks its end marker.
    ep.limit.store(30, Ordering::SeqCst);
    let streamed = |memory: Option<u64>| -> Result<usize, Error> {
        let o = CursorOptions {
            batch_rows: 64,
            fallback: FallbackPolicy::RejectMaterialization,
            ..Default::default()
        };
        let opts = QueryOptions {
            no_cache: true,
            max_memory_bytes: memory,
            ..opts()
        };
        let mut c = select_cursor(data.snapshot(), &q, &opts, &o)?;
        let mut rows = 0;
        while let Some(b) = c.next_batch()? {
            rows += b.len();
        }
        Ok(rows)
    };
    assert!(streamed(None).unwrap() > 0);
    assert_eq!(
        ep.take().len(),
        41,
        "a bulk request and one retry per department"
    );
    let r = streamed(Some(48 << 10));
    assert!(
        matches!(&r, Err(Error::BudgetExceeded(b)) if b.kind == BudgetKind::Memory),
        "{r:?}"
    );
    let sent = ep.take().len();
    assert!(sent < 41, "the loop sent {sent} requests past its budget");
}

/// The cache keys a request without its outermost slice and bounds the entry by it, so a
/// request whose slice lies inside what an entry holds is answered without a request,
/// and a request for more is sent as written (roadmap item 38).
#[test]
fn slices_are_answered_from_the_entries_that_hold_them() {
    let data = store(&departments(9));
    let ep = Endpoint::start(data.clone(), Duration::ZERO);
    let q = |slice: &str| {
        format!(
            "SELECT ?p {{ SERVICE <cache:{}> {{ SELECT DISTINCT ?p {{ ?d <urn:hasEmployee> ?p }} ORDER BY ?p {slice} }} }}",
            ep.url()
        )
    };
    let all = rows(&data, &q(""));
    assert_eq!(all.len(), 9);
    ep.take();
    data.result_cache().service.clear().unwrap();
    let first5 = rows(&data, &q("LIMIT 5"));
    assert_eq!(first5.len(), 5);
    let sent = ep.take();
    assert_eq!(sent.len(), 1);
    assert!(
        sent[0].contains("LIMIT 5"),
        "the user's slice is sent: {}",
        sent[0]
    );
    // inside the five rows held
    for (slice, n) in [
        ("LIMIT 3", 3),
        ("OFFSET 1 LIMIT 3", 3),
        ("OFFSET 4 LIMIT 1", 1),
    ] {
        assert_eq!(rows(&data, &q(slice)).len(), n, "{slice}");
        assert!(ep.take().is_empty(), "{slice} was sent");
    }
    // OFFSET 4 LIMIT 1 is the fifth row
    let fifth = rows(&data, &q("OFFSET 4 LIMIT 1"));
    let sorted_all: Vec<String> = {
        let mut v = all.clone();
        v.sort_by_key(|p| p.trim_start_matches("person").parse::<u32>().unwrap_or(0));
        v
    };
    assert_eq!(fifth, vec![sorted_all[4].clone()]);
    ep.take();
    // past the rows held, and the whole result, are sent
    assert_eq!(rows(&data, &q("OFFSET 3 LIMIT 5")).len(), 5);
    assert_eq!(ep.take().len(), 1);
    assert_eq!(rows(&data, &q("LIMIT 20")), all);
    assert_eq!(ep.take().len(), 1);
    // the request itself, and the slices inside the 9 rows the LIMIT of 20 got, are
    // answered from the entry
    assert_eq!(rows(&data, &q("LIMIT 20")), all);
    assert_eq!(rows(&data, &q("OFFSET 7 LIMIT 2")).len(), 2);
    assert!(ep.take().is_empty());
    // but 9 rows for a LIMIT of 20 is not taken as the whole result, so slices past them
    // and the request without a slice are sent
    assert_eq!(rows(&data, &q("OFFSET 7 LIMIT 100")).len(), 2);
    assert_eq!(rows(&data, &q("")), all);
    assert_eq!(rows(&data, &q("OFFSET 20")).len(), 0);
    assert_eq!(ep.take().len(), 3);
}

/// An endpoint that caps its responses without saying so: a short response is not the
/// end of the result, so a later slice past it gets what an uncached request gets.
#[test]
fn a_short_response_is_not_the_end_of_the_result() {
    let data = store(&departments(9));
    let ep = Endpoint::start(data.clone(), Duration::ZERO);
    let q = |cache: &str, slice: &str| {
        format!(
            "SELECT ?p {{ SERVICE <{cache}{}> {{ SELECT DISTINCT ?p {{ ?d <urn:hasEmployee> ?p }} ORDER BY ?p {slice} }} }}",
            ep.url()
        )
    };
    ep.limit.store(4, Ordering::SeqCst);
    for first in ["LIMIT 20", ""] {
        data.result_cache().service.clear().unwrap();
        assert_eq!(rows(&data, &q("cache:", first)).len(), 4, "{first}: capped");
        ep.take();
        for later in ["OFFSET 4 LIMIT 3", "OFFSET 6", "OFFSET 2 LIMIT 4"] {
            let uncached = rows(&data, &q("", later));
            assert!(!uncached.is_empty(), "{later}");
            assert_eq!(
                rows(&data, &q("cache:", later)),
                uncached,
                "{first} then {later}"
            );
        }
        // two uncached requests and the cached ones, which the entry could not answer
        assert_eq!(ep.take().len(), 6, "{first}");
        // inside the four rows held, nothing is sent
        assert_eq!(rows(&data, &q("cache:", "OFFSET 1 LIMIT 3")).len(), 3);
        assert!(ep.take().is_empty());
    }
}

/// A LIMIT pushed into the request of a plain cached SERVICE bounds its entry in both
/// execution modes, and a smaller one is answered from it.
#[test]
fn a_pushed_limit_bounds_the_entry() {
    use sparkles_core::sparql::{CursorOptions, ExecutionMode, QueryExecution, query_execution};
    let data = store(&departments(9));
    let ep = Endpoint::start(data.clone(), Duration::ZERO);
    let url = ep.url();
    let count = |q: &str, mode: ExecutionMode| -> usize {
        match query_execution(data.snapshot(), q, &opts(), &CursorOptions::default(), mode)
            .unwrap_or_else(|e| panic!("{q}: {e}"))
        {
            QueryExecution::Select(mut c) => {
                let mut n = 0;
                while let Some(b) = c.next_batch().unwrap() {
                    n += b.len();
                }
                n
            }
            QueryExecution::Eager(r) => r.into_result().len(),
            _ => unreachable!("a SELECT"),
        }
    };
    for mode in [ExecutionMode::Eager, ExecutionMode::Streaming] {
        data.result_cache().service.clear().unwrap();
        let q = |k: usize| {
            format!(
                "SELECT ?p ?n {{ SERVICE <cache:{url}> {{ ?d <urn:hasEmployee> ?p }} BIND(STR(?p) AS ?n) }} LIMIT {k}"
            )
        };
        assert_eq!(count(&q(6), mode), 6, "{mode:?}");
        let sent = ep.take();
        assert_eq!(sent.len(), 1, "{mode:?}");
        assert!(sent[0].trim_end().ends_with("LIMIT 6"), "{}", sent[0]);
        assert_eq!(count(&q(2), mode), 2, "{mode:?}");
        // A non-SILENT query that reaches the LIMIT does not block on the response
        // tail and does not cache the response, so a later request with a narrower
        // LIMIT sends its own request.
        ep.take();
        assert_eq!(count(&q(10), mode), 10, "{mode:?}");
        ep.take();
    }
}

/// `se:cacheLs` lists the caller's own entries, and `se:cacheRm` removes them only for
/// a query that its options let manage the cache. Any other query that calls it fails.
#[test]
fn cache_functions_list_and_remove_the_callers_entries() {
    let data = store(&departments(9));
    let ep = Endpoint::start(data.clone(), Duration::ZERO);
    let url = ep.url();
    let cached = format!(
        "SELECT ?d ?p {{ VALUES ?d {{ <urn:dept1> <urn:dept2> }} SERVICE <loop:cache:{url}> {{ ?d <urn:hasEmployee> ?p }} }}"
    );
    let se = "PREFIX se: <http://jena.apache.org/service-enhancer#>";
    let ls = format!("{se} SELECT ?id ?s ?q ?b ?n {{ ?id se:cacheLs (?s ?q ?b ?n) }}");
    assert_eq!(rows(&data, &cached).len(), 9 + 8);
    assert_eq!(ep.take().len(), 2);
    let listed = query(data.snapshot(), &ls, &opts()).unwrap();
    let listed = listed.rows().unwrap();
    assert_eq!(listed.len(), 2);
    for r in listed.iter() {
        let v = r.values();
        assert_eq!(
            v[1].as_ref().map(ToString::to_string),
            Some(format!("<{url}>"))
        );
        assert!(matches!(&v[2], Some(Term::Literal(l)) if l.value().contains("hasEmployee")));
        assert!(matches!(&v[3], Some(Term::Literal(l)) if l.value().contains("urn:dept")));
    }
    let ns: Vec<String> = listed.iter().map(|r| short(&r.values()[4])).collect();
    assert!(
        ns.contains(&"9".to_string()) && ns.contains(&"8".to_string()),
        "{ns:?}"
    );
    // a filter on the endpoint, and on another one
    let by = |e: &str| format!("{se} SELECT ?id {{ ?id se:cacheLs (<{e}>) }}");
    assert_eq!(rows(&data, &by(&url)).len(), 2);
    assert_eq!(rows(&data, &by("http://elsewhere.example/sparql")).len(), 0);
    // another caller sees none of them
    let mut bob = opts();
    bob.service_scope = Some("bob".into());
    assert_eq!(run(&data, &ls, &bob).unwrap().len(), 0);
    // removal needs the right
    let rm_all = format!("{se} SELECT (se:cacheRm() AS ?n) {{}}");
    assert!(matches!(
        run(&data, &rm_all, &opts()),
        Err(Error::NotPermitted(_))
    ));
    let mut admin_bob = bob.clone();
    admin_bob.service_cache_mgmt = true;
    assert_eq!(
        run(&data, &rm_all, &admin_bob).unwrap(),
        vec!["0".to_string()]
    );
    assert_eq!(
        rows(&data, &ls).len(),
        2,
        "bob removed another caller's entries"
    );
    let mut admin = opts();
    admin.service_cache_mgmt = true;
    let rm_one =
        format!("{se} SELECT (SUM(se:cacheRm(?id)) AS ?n) {{ ?id se:cacheLs (?s ?q ?b 8) }}");
    assert_eq!(run(&data, &rm_one, &admin).unwrap(), vec!["1".to_string()]);
    assert_eq!(rows(&data, &ls).len(), 1);
    assert_eq!(rows(&data, &cached).len(), 17);
    assert_eq!(ep.take().len(), 1, "only the removed input is sent again");
    assert_eq!(run(&data, &rm_all, &admin).unwrap(), vec!["2".to_string()]);
    assert_eq!(rows(&data, &ls).len(), 0);
    // a call of se:cacheRm is never answered by a cache: each query removes again
    assert_eq!(rows(&data, &cached).len(), 17);
    assert_eq!(ep.take().len(), 2);
    assert_eq!(run(&data, &rm_all, &admin).unwrap(), vec!["2".to_string()]);
    assert_eq!(run(&data, &rm_all, &admin).unwrap(), vec!["0".to_string()]);
}

/// `parallel+n` keeps up to n requests in flight and gives the answer and the requests
/// of a run one at a time (roadmap item 38).
#[test]
fn parallel_requests_give_the_answer_of_one_at_a_time() {
    let data = store(&departments(9));
    let ep = Endpoint::start(data.clone(), Duration::from_millis(60));
    let q = |opts: &str| {
        format!(
            "SELECT ?d ?p {{ ?d a <urn:Department> SERVICE <loop:{opts}{}> {{ SELECT ?p {{ ?d <urn:hasEmployee> ?p }} ORDER BY DESC(?p) LIMIT 1 }} }}",
            ep.url()
        )
    };
    let one = rows(&data, &q(""));
    assert_eq!(one.len(), 9);
    let mut sent_one = ep.take();
    assert_eq!(sent_one.len(), 9);
    assert_eq!(ep.take_peak(), 1);
    for (opt, peak) in [
        ("parallel+4:", 4),
        ("parallel:", 4),
        ("parallel+100:", 4),
        ("parallel+3:bulk+2:", 3),
    ] {
        let started = Instant::now();
        assert_eq!(rows(&data, &q(opt)), one, "{opt}");
        let took = started.elapsed();
        let mut sent = ep.take();
        let most = ep.take_peak();
        assert!(most > 1 && most <= peak, "{opt}: {most} requests at once");
        if !opt.contains("bulk") {
            sent.sort();
            sent_one.sort();
            assert_eq!(sent, sent_one, "{opt}");
            // three waves of 60 ms against nine requests one at a time
            assert!(took < Duration::from_millis(9 * 60), "{opt}: {took:?}");
        } else {
            assert_eq!(sent.len(), 5, "{opt}");
        }
    }
    // a failing endpoint fails the query, and with SILENT keeps each input once
    ep.failing.store(true, Ordering::SeqCst);
    assert!(run(&data, &q("parallel+4:"), &opts()).is_err());
    let silent = format!(
        "SELECT ?d ?p {{ ?d a <urn:Department> SERVICE SILENT <loop:parallel+4:{}> {{ ?d <urn:hasEmployee> ?p }} }}",
        ep.url()
    );
    let kept = rows(&data, &silent);
    assert_eq!(kept.len(), 9);
    assert!(kept.iter().all(|r| r.ends_with(" -")), "{kept:?}");
    ep.failing.store(false, Ordering::SeqCst);
    ep.take();
}

/// A query that runs out of time stops sending: the waves after the deadline never go
/// out, so the endpoint sees fewer requests than inputs.
#[test]
fn parallel_requests_stop_at_the_deadline() {
    let data = store(&departments(30));
    let ep = Endpoint::start(data.clone(), Duration::from_millis(150));
    let q = format!(
        "SELECT ?d ?p {{ ?d a <urn:Department> SERVICE <loop:parallel+2:{}> {{ ?d <urn:hasEmployee> ?p }} }}",
        ep.url()
    );
    let mut o = opts();
    o.timeout = Some(Duration::from_millis(400));
    let started = Instant::now();
    let r = run(&data, &q, &o);
    // the deadline ends the query, or the request it cuts short
    assert!(
        matches!(r, Err(Error::Timeout | Error::Service(_))),
        "{r:?}"
    );
    assert!(started.elapsed() < Duration::from_secs(3));
    let sent = ep.take().len();
    assert!((2..=8).contains(&sent), "{sent} of 30 requests were sent");
}

/// Parallel requests admit each solution as it arrives, so a loop over a small memory
/// budget fails in both modes before it holds every answer of every wave.
#[test]
fn parallel_requests_admit_their_answers() {
    use sparkles_core::BudgetKind;
    use sparkles_core::sparql::{CursorOptions, FallbackPolicy, select_cursor};
    let data = store(&departments(40));
    let ep = Endpoint::start(data.clone(), Duration::ZERO);
    let q = format!(
        "SELECT ?d ?p ?l {{ ?d a <urn:Department> SERVICE <loop:parallel+4:{}> {{ ?d <urn:hasEmployee> ?p . ?p <http://www.w3.org/2000/01/rdf-schema#label> ?l }} }}",
        ep.url()
    );
    let o = |memory: Option<u64>| QueryOptions {
        no_cache: true,
        max_memory_bytes: memory,
        ..opts()
    };
    let streamed = |memory: Option<u64>| -> Result<usize, Error> {
        let c = CursorOptions {
            batch_rows: 64,
            fallback: FallbackPolicy::RejectMaterialization,
            ..Default::default()
        };
        let mut c = select_cursor(data.snapshot(), &q, &o(memory), &c)?;
        let mut rows = 0;
        while let Some(b) = c.next_batch()? {
            rows += b.len();
        }
        Ok(rows)
    };
    assert_eq!(run(&data, &q, &o(None)).unwrap().len(), 820);
    assert_eq!(streamed(None).unwrap(), 820);
    assert_eq!(ep.take().len(), 80);
    let eager = run(&data, &q, &o(Some(48 << 10)));
    assert!(
        matches!(&eager, Err(Error::BudgetExceeded(b)) if b.kind == BudgetKind::Memory),
        "{eager:?}"
    );
    let sent = ep.take().len();
    assert!(
        sent < 40,
        "the eager loop sent {sent} requests past its budget"
    );
    let r = streamed(Some(48 << 10));
    assert!(
        matches!(&r, Err(Error::BudgetExceeded(b)) if b.kind == BudgetKind::Memory),
        "{r:?}"
    );
    let sent = ep.take().len();
    assert!(
        sent < 40,
        "the streamed loop sent {sent} requests past its budget"
    );
}

/// The requests of a wave share the outbound time of the SPARQL request: with a total
/// shorter than one response takes, every request of the wave is cut short within its
/// share, the query fails with SILENT too, and no later wave goes out. Without shares
/// each request would wait for the whole total of 2 s.
#[test]
fn parallel_requests_share_the_outbound_time() {
    let data = store(&departments(12));
    let ep = Endpoint::start(data.clone(), Duration::from_secs(4));
    let q = |silent: &str| {
        format!(
            "SELECT ?d ?p {{ ?d a <urn:Department> SERVICE {silent} <loop:parallel+4:{}> {{ ?d <urn:hasEmployee> ?p }} }}",
            ep.url()
        )
    };
    let mut o = opts();
    o.outbound.request_timeout = Duration::from_secs(2);
    for silent in ["", "SILENT"] {
        let started = Instant::now();
        let r = run(&data, &q(silent), &o);
        assert!(r.is_err(), "{silent}: {r:?}");
        // four shares of 500 ms, not four requests of 2 s each
        let took = started.elapsed();
        assert!(took < Duration::from_millis(1700), "{silent}: {took:?}");
        let sent = ep.take().len();
        assert!(sent <= 4, "{silent}: {sent} requests were sent");
    }
}

/// A query cancelled while a wave is in flight sends no later wave. The test cancels
/// once the endpoint holds the first wave's requests, so the wave is known to have
/// started, and the requests in flight end when their responses arrive (roadmap item
/// 1341). The query then waits for one wave and not for the three it would send.
#[test]
fn parallel_requests_stop_when_cancelled() {
    let data = store(&departments(12));
    let delay = Duration::from_millis(800);
    let ep = Endpoint::start(data.clone(), delay);
    let q = format!(
        "SELECT ?d ?p {{ ?d a <urn:Department> SERVICE <loop:parallel+4:{}> {{ ?d <urn:hasEmployee> ?p }} }}",
        ep.url()
    );
    let cancel = Arc::new(AtomicBool::new(false));
    let mut o = opts();
    o.cancel = Some(cancel.clone());
    let (flag, seen) = (cancel.clone(), ep.queries.clone());
    let t = std::thread::spawn(move || {
        // the rendezvous: the first wave's requests have reached the endpoint
        let started = Instant::now();
        while seen.lock().unwrap().is_empty() && started.elapsed() < Duration::from_secs(30) {
            std::thread::sleep(Duration::from_millis(5));
        }
        flag.store(true, Ordering::SeqCst);
    });
    let started = Instant::now();
    let r = run(&data, &q, &o);
    t.join().unwrap();
    assert!(matches!(r, Err(Error::Cancelled)), "{r:?}");
    let sent = ep.take().len();
    assert!((1..=4).contains(&sent), "{sent} requests were sent");
    // one wave, with ample room for a loaded worker, against the three it would send
    let took = started.elapsed();
    assert!(took < delay * 3, "{took:?}");
}

/// `se:cacheRm` called through `fn:apply` or `afn:eval` removes nothing, with the right
/// or without it, and the results of `se:cacheLs` are never kept in the query result
/// cache: a second caller and a later listing each see the entries as they are.
#[test]
fn dynamic_removal_and_listings_in_the_result_cache() {
    // a result cache that keeps every result, however fast
    let data = Store::in_memory(StoreOptions {
        result_cache_min_ms: 0.0,
        ..Default::default()
    });
    data.load(&[Source::from_bytes(
        departments(9).into_bytes(),
        RdfFormat::NTriples,
        None,
    )])
    .unwrap();
    let data = Arc::new(data);
    // the result cache does keep a plain query's result
    let plain = "SELECT ?d { ?d a <urn:Department> }";
    let caching0 = QueryOptions {
        no_cache: false,
        ..opts()
    };
    run(&data, plain, &caching0).unwrap();
    let before = data.result_cache().hits();
    run(&data, plain, &caching0).unwrap();
    assert!(
        data.result_cache().hits() > before,
        "the result cache is on"
    );
    let listed_hits = || data.result_cache().hits();
    let ep = Endpoint::start(data.clone(), Duration::ZERO);
    let url = ep.url();
    let cached = |d: &str| {
        format!(
            "SELECT ?p {{ VALUES ?d {{ <urn:{d}> }} SERVICE <loop:cache:{url}> {{ ?d <urn:hasEmployee> ?p }} }}"
        )
    };
    let se = "PREFIX se: <http://jena.apache.org/service-enhancer#>";
    let ls = format!("{se} SELECT ?id {{ ?id se:cacheLs () }}");
    let caching = QueryOptions {
        no_cache: false,
        ..opts()
    };
    rows(&data, &cached("dept1"));
    let hits = listed_hits();
    assert_eq!(run(&data, &ls, &caching).unwrap().len(), 1);
    assert_eq!(run(&data, &ls, &caching).unwrap().len(), 1);
    assert_eq!(
        listed_hits(),
        hits,
        "a listing was answered from the result cache"
    );
    rows(&data, &cached("dept2"));
    assert_eq!(
        run(&data, &ls, &caching).unwrap().len(),
        2,
        "a stale listing"
    );
    let mut bob = caching.clone();
    bob.service_scope = Some("bob".into());
    assert_eq!(
        run(&data, &ls, &bob).unwrap().len(),
        0,
        "another caller's listing"
    );
    let mut admin = caching.clone();
    admin.service_cache_mgmt = true;
    for q in [
        format!(
            "{se} PREFIX fn: <http://www.w3.org/2005/xpath-functions#> SELECT (fn:apply(se:cacheRm) AS ?n) {{}}"
        ),
        format!(
            "{se} PREFIX afn: <http://jena.apache.org/ARQ/function#> SELECT (afn:eval(se:cacheRm) AS ?n) {{}}"
        ),
    ] {
        for o in [&caching, &admin] {
            // an error of the expression leaves ?n unbound
            assert_eq!(run(&data, &q, o).unwrap(), vec!["-".to_string()], "{q}");
            assert_eq!(run(&data, &ls, &caching).unwrap().len(), 2, "{q}");
        }
    }
}

/// A listing admits its entries as it makes them, so a query under a small memory
/// budget fails on a long listing that it would run without one.
#[test]
fn a_listing_admits_its_entries() {
    use sparkles_core::BudgetKind;
    let data = store(&departments(200));
    let ep = Endpoint::start(data.clone(), Duration::ZERO);
    rows(
        &data,
        &format!(
            "SELECT ?p {{ ?d a <urn:Department> SERVICE <loop:cache:{}> {{ ?d <urn:hasEmployee> ?p }} }}",
            ep.url()
        ),
    );
    let se = "PREFIX se: <http://jena.apache.org/service-enhancer#>";
    let ls = format!("{se} SELECT ?id ?s ?q ?b {{ ?id se:cacheLs (?s ?q ?b) }}");
    assert_eq!(run(&data, &ls, &opts()).unwrap().len(), 200);
    let small = QueryOptions {
        max_memory_bytes: Some(16 << 10),
        ..opts()
    };
    // the same shape without a listing runs under the budget
    assert!(run(&data, "SELECT ?id { VALUES ?id { 1 } }", &small).is_ok());
    let r = run(&data, &ls, &small);
    assert!(
        matches!(&r, Err(Error::BudgetExceeded(b)) if b.kind == BudgetKind::Memory),
        "{r:?}"
    );
}

/// A dataset that keeps its SERVICE cache on disk answers a cached query from it after
/// it is opened again (roadmap item 38).
#[test]
fn a_disk_cache_outlives_the_store() {
    use sparkles_core::store::ServiceCacheSettings;
    let data = store(&departments(9));
    let ep = Endpoint::start(data.clone(), Duration::ZERO);
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("db");
    let q = format!(
        "SELECT ?d ?p {{ VALUES ?d {{ <urn:dept1> <urn:dept5> }} SERVICE <loop:cache:{}> {{ ?d <urn:hasEmployee> ?p }} }}",
        ep.url()
    );
    let first = {
        let local = Store::open(&dir, StoreOptions::default()).unwrap();
        local
            .set_service_cache_settings(Some(ServiceCacheSettings {
                disk: true,
                ..Default::default()
            }))
            .unwrap();
        let first = rows(&local, &q);
        assert_eq!(first.len(), 9 + 5);
        assert_eq!(ep.take().len(), 2);
        first
    };
    let local = Store::open(&dir, StoreOptions::default()).unwrap();
    assert_eq!(
        local.result_cache().service.entries(),
        0,
        "the memory tier is new"
    );
    assert_eq!(rows(&local, &q), first);
    assert!(ep.take().is_empty(), "the entries were not read from disk");
    assert_eq!(local.result_cache().service.entries(), 2);
}

/// A store whose query result cache keeps every result, however fast, over Jena's
/// departments.
fn caching_store(n: usize) -> Arc<Store> {
    let data = Store::in_memory(StoreOptions {
        result_cache_min_ms: 0.0,
        ..Default::default()
    });
    data.load(&[Source::from_bytes(
        departments(n).into_bytes(),
        RdfFormat::NTriples,
        None,
    )])
    .unwrap();
    Arc::new(data)
}

/// `se:cacheLs` and `se:cacheRm` inside a correlated LATERAL or a loop over the dataset
/// itself are never answered by the query result cache: another caller sees its own
/// listing, and each removal removes (Sol and Neon round-1 P1).
#[test]
fn correlated_listings_and_removals_skip_the_result_cache() {
    let data = caching_store(9);
    let ep = Endpoint::start(data.clone(), Duration::ZERO);
    let url = ep.url();
    let cached = |ds: &[&str]| {
        let vs: Vec<String> = ds.iter().map(|d| format!("<urn:{d}>")).collect();
        format!(
            "SELECT ?p {{ VALUES ?d {{ {} }} SERVICE <loop:cache:{url}> {{ ?d <urn:hasEmployee> ?p }} }}",
            vs.join(" ")
        )
    };
    let se = "PREFIX se: <http://jena.apache.org/service-enhancer#>";
    let caching = QueryOptions {
        no_cache: false,
        ..opts()
    };
    run(&data, &cached(&["dept1", "dept2"]), &caching).unwrap();
    let mut bob = caching.clone();
    bob.service_scope = Some("bob".into());
    let count = format!("{se} SELECT (COUNT(?id) AS ?n) {{ ?id se:cacheLs () }}");
    for ls in [
        format!(
            "{se} SELECT (COUNT(?id) AS ?n) {{ VALUES ?x {{ 1 }} LATERAL {{ ?id se:cacheLs () FILTER(?x = 1) }} }}"
        ),
        format!(
            "{se} SELECT (COUNT(?id) AS ?n) {{ VALUES ?x {{ 1 }} SERVICE <loop:> {{ ?id se:cacheLs () FILTER(?x = 1) }} }}"
        ),
    ] {
        assert_eq!(run(&data, &ls, &caching).unwrap(), vec!["2"], "{ls}");
        assert_eq!(
            run(&data, &ls, &bob).unwrap(),
            vec!["0"],
            "{ls}: another caller's listing"
        );
    }
    let mut admin = caching.clone();
    admin.service_cache_mgmt = true;
    for rm in [
        format!(
            "{se} SELECT ?n {{ VALUES ?x {{ 1 }} LATERAL {{ BIND(se:cacheRm() + ?x - ?x AS ?n) }} }}"
        ),
        format!(
            "{se} SELECT ?n {{ VALUES ?x {{ 1 }} SERVICE <loop:> {{ BIND(se:cacheRm() + ?x - ?x AS ?n) }} }}"
        ),
    ] {
        run(&data, &cached(&["dept1", "dept2"]), &caching).unwrap();
        assert_eq!(run(&data, &rm, &admin).unwrap(), vec!["2"], "{rm}");
        run(&data, &cached(&["dept3"]), &caching).unwrap();
        assert_eq!(
            run(&data, &rm, &admin).unwrap(),
            vec!["1"],
            "{rm}: a removal answered from the result cache"
        );
        assert_eq!(run(&data, &count, &caching).unwrap(), vec!["0"], "{rm}");
        assert!(
            matches!(run(&data, &rm, &caching), Err(Error::NotPermitted(_))),
            "{rm}: removal without the right"
        );
    }
}

/// A wave of `parallel+n` whose share of the outbound time rounds to nothing fails the
/// query under SILENT too, as one request out of time does (Neon round-1 P1-3).
#[test]
fn parallel_requests_out_of_time_fail_with_silent() {
    let data = store(&departments(6));
    let ep = Endpoint::start(data.clone(), Duration::ZERO);
    let mut o = opts();
    o.outbound.request_timeout = Duration::from_nanos(2);
    for p in ["parallel+3:", "parallel+4:bulk+2:"] {
        let q = format!(
            "SELECT ?d ?p {{ ?d a <urn:Department> SERVICE SILENT <loop:{p}{}> {{ ?d <urn:hasEmployee> ?p }} }}",
            ep.url()
        );
        let r = run(&data, &q, &o);
        assert!(r.is_err(), "{p}: {r:?}");
    }
    ep.take();
}

/// `se:cacheRm` in the WHERE clause of an update removes the caller's entries when the
/// update's options grant it, and is refused otherwise (Neon round-1 P2-1).
#[test]
fn an_update_removes_with_the_right() {
    let data = store(&departments(4));
    let ep = Endpoint::start(data.clone(), Duration::ZERO);
    let cached = format!(
        "SELECT ?p {{ VALUES ?d {{ <urn:dept1> <urn:dept2> }} SERVICE <loop:cache:{}> {{ ?d <urn:hasEmployee> ?p }} }}",
        ep.url()
    );
    rows(&data, &cached);
    let se = "PREFIX se: <http://jena.apache.org/service-enhancer#>";
    let count = format!("{se} SELECT (COUNT(?id) AS ?n) {{ ?id se:cacheLs () }}");
    assert_eq!(rows(&data, &count), vec!["2"]);
    let u =
        format!("{se} INSERT {{ <urn:removed> <urn:n> ?n }} WHERE {{ BIND(se:cacheRm() AS ?n) }}");
    let r = sparkles_core::sparql::update::update(&data, &u, &opts());
    assert!(matches!(r, Err(Error::NotPermitted(_))), "{r:?}");
    assert_eq!(rows(&data, &count), vec!["2"]);
    let admin = QueryOptions {
        service_cache_mgmt: true,
        ..opts()
    };
    sparkles_core::sparql::update::update(&data, &u, &admin).unwrap();
    assert_eq!(rows(&data, &count), vec!["0"]);
    assert_eq!(
        rows(&data, "SELECT ?n { <urn:removed> <urn:n> ?n }"),
        vec!["2"]
    );
}

/// A removal refused for want of the right inside an EXISTS that no row reaches still
/// fails the query when it is planned (Sol round-1 #9). A call inside a SERVICE to a
/// remote endpoint is the endpoint's and is not refused here.
#[test]
fn a_removal_inside_exists_is_refused_at_planning() {
    let data = store(&departments(2));
    let ep = Endpoint::start(data.clone(), Duration::ZERO);
    let se = "PREFIX se: <http://jena.apache.org/service-enhancer#>";
    for q in [
        format!("{se} SELECT * {{ FILTER(false && EXISTS {{ BIND(se:cacheRm() AS ?n) }}) }}"),
        format!(
            "{se} SELECT * {{ ?d a <urn:Department> FILTER NOT EXISTS {{ FILTER EXISTS {{ BIND(se:cacheRm() AS ?n) }} }} }}"
        ),
        format!(
            "{se} SELECT * {{ ?d a <urn:Department> FILTER EXISTS {{ SERVICE <loop:> {{ BIND(se:cacheRm() AS ?n) }} }} }}"
        ),
    ] {
        let r = run(&data, &q, &opts());
        assert!(matches!(r, Err(Error::NotPermitted(_))), "{q}: {r:?}");
    }
    // sent to the endpoint, which refuses it as an unknown function or answers it
    let remote = format!(
        "{se} SELECT * {{ ?d a <urn:Department> FILTER EXISTS {{ SERVICE SILENT <{}> {{ BIND(se:cacheRm() AS ?n) }} }} }}",
        ep.url()
    );
    let r = run(&data, &remote, &opts());
    assert!(!matches!(r, Err(Error::NotPermitted(_))), "{r:?}");
}

/// A guard whose exemptions change with the data gives the same caller a scope of its
/// own for each set of graphs it lets through, so a listing in one view shows none of
/// the entries cached in another (Sol round-1 #13).
#[test]
fn a_guarded_view_scopes_its_entries_by_its_exemptions() {
    use sparkles_core::access::{Decider, GraphAccess, Graphs, GuardDecision};
    use sparkles_core::store::Snapshot;
    struct Flip(Arc<AtomicBool>);
    impl GuardDecision for Flip {
        fn exempt(&self, _snap: &Arc<Snapshot>) -> Vec<String> {
            if self.0.load(Ordering::SeqCst) {
                vec!["urn:g1".into()]
            } else {
                Vec::new()
            }
        }
    }
    let data = store(&departments(4));
    let ep = Endpoint::start(data.clone(), Duration::ZERO);
    let wide = Arc::new(AtomicBool::new(true));
    let view = Graphs::All.guarded_by("urn:g*", Decider(Arc::new(Flip(wide.clone()))));
    let o = QueryOptions {
        graphs: Some(Arc::new(GraphAccess::graphs(view.clone(), view))),
        ..opts()
    };
    let cached = format!(
        "SELECT ?p {{ VALUES ?d {{ <urn:dept1> }} SERVICE <loop:cache:{}> {{ ?d <urn:hasEmployee> ?p }} }}",
        ep.url()
    );
    let se = "PREFIX se: <http://jena.apache.org/service-enhancer#>";
    let count = format!("{se} SELECT (COUNT(?id) AS ?n) {{ ?id se:cacheLs () }}");
    run(&data, &cached, &o).unwrap();
    assert_eq!(run(&data, &count, &o).unwrap(), vec!["1"]);
    wide.store(false, Ordering::SeqCst);
    assert_eq!(
        run(&data, &count, &o).unwrap(),
        vec!["0"],
        "the narrower view lists the broader one's entry"
    );
    wide.store(true, Ordering::SeqCst);
    assert_eq!(run(&data, &count, &o).unwrap(), vec!["1"]);
}

/// A SERVICE call in an UPDATE WHERE clause does not read from or publish to the cache.
/// Values inserted by one operation and readable by another operation's SERVICE are not
/// published, and a rolled-back UPDATE leaves the cache as it was (Sol round-2 P1-1).
#[test]
fn an_update_does_not_cache_service_results() {
    let data = store(&departments(4));
    let ep = Endpoint::start(data.clone(), Duration::ZERO);
    let se = "PREFIX se: <http://jena.apache.org/service-enhancer#>";
    let count = format!("{se} SELECT (COUNT(?id) AS ?n) {{ ?id se:cacheLs () }}");
    // a SELECT caches the result
    let select = format!(
        "SELECT ?p {{ VALUES ?d {{ <urn:dept1> }} SERVICE <loop:cache:{}> {{ ?d <urn:hasEmployee> ?p }} }}",
        ep.url()
    );
    rows(&data, &select);
    assert_eq!(rows(&data, &count), vec!["1"]);
    // clear for the UPDATE test
    let admin = QueryOptions {
        service_cache_mgmt: true,
        ..opts()
    };
    sparkles_core::sparql::query(
        data.snapshot(),
        &format!("{se} SELECT (se:cacheRm() AS ?r) {{ }}"),
        &admin,
    )
    .unwrap();
    assert_eq!(rows(&data, &count), vec!["0"]);
    // the same loop in an UPDATE WHERE must not publish to the cache
    let u = format!(
        "INSERT {{ <urn:test> <urn:n> ?p }} WHERE {{ VALUES ?d {{ <urn:dept2> }} SERVICE <loop:cache:{}> {{ ?d <urn:hasEmployee> ?p }} }}",
        ep.url()
    );
    sparkles_core::sparql::update::update(&data, &u, &admin).unwrap();
    let sent = ep.take();
    assert!(!sent.is_empty(), "the endpoint received requests");
    assert_eq!(
        rows(&data, &count),
        vec!["0"],
        "UPDATE must not publish SERVICE results to the cache"
    );
    // the inserted data proves the SERVICE ran
    assert!(
        !rows(&data, "SELECT ?n { <urn:test> <urn:n> ?n }").is_empty(),
        "the UPDATE committed its WHERE results"
    );
}

/// A deferred OPTIONAL condition that calls se:cacheRm inside EXISTS is refused at
/// planning time, not deferred to execution (Sol round-2 P2-4).
#[test]
fn a_deferred_optional_refuses_cache_rm() {
    let data = store(&departments(2));
    let ep = Endpoint::start(data.clone(), Duration::ZERO);
    let se = "PREFIX se: <http://jena.apache.org/service-enhancer#>";
    let q = format!(
        "{se} SELECT ?d ?p {{ ?d a <urn:Department> OPTIONAL {{ SERVICE <loop:{}> {{ ?d <urn:hasEmployee> ?p }} FILTER EXISTS {{ BIND(se:cacheRm() AS ?rm) }} }} }}",
        ep.url()
    );
    let r = run(&data, &q, &opts());
    assert!(
        matches!(r, Err(Error::NotPermitted(_))),
        "expected NotPermitted, got {r:?}"
    );
    assert!(ep.take().is_empty(), "no requests should have been sent");
}
