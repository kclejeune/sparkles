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
use sparkles::Error;
use sparkles::io::{RdfFormat, Source};
use sparkles::outbound::OutboundPolicy;
use sparkles::sparql::results::{SolutionsFormat, write_solutions};
use sparkles::sparql::{QueryOptions, query};
use sparkles::store::{Store, StoreOptions};
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
}

impl Endpoint {
    fn start(data: Arc<Store>, delay: Duration) -> Endpoint {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        let queries = Arc::new(Mutex::new(Vec::new()));
        let limit = Arc::new(AtomicUsize::new(usize::MAX));
        let failing = Arc::new(AtomicBool::new(false));
        let (q, lim, fail) = (queries.clone(), limit.clone(), failing.clone());
        std::thread::spawn(move || {
            for c in l.incoming() {
                let Ok(c) = c else { continue };
                let (data, q, lim, fail) = (data.clone(), q.clone(), lim.clone(), fail.clone());
                std::thread::spawn(move || answer(c, &data, &q, &lim, &fail, delay));
            }
        });
        Endpoint {
            port,
            queries,
            limit,
            failing,
        }
    }

    fn url(&self) -> String {
        format!("http://127.0.0.1:{}/sparql", self.port)
    }

    fn take(&self) -> Vec<String> {
        std::mem::take(&mut *self.queries.lock().unwrap())
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
            Ok(mut r) => {
                let n = limit.load(Ordering::SeqCst);
                if r.table.len() > n {
                    r.table.truncate(n);
                }
                let mut out = Vec::new();
                write_solutions(&r, SolutionsFormat::Json, &mut out, None).unwrap();
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

fn run(local: &Store, q: &str, o: &QueryOptions) -> sparkles::Result<Vec<String>> {
    let r = query(local.snapshot(), q, o)?;
    let mut rows: Vec<String> = r
        .rows()
        .iter()
        .map(|row| row.iter().map(short).collect::<Vec<_>>().join(" "))
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
    let (_, plan) = sparkles::sparql::explain(
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
            data.result_cache().service.clear();
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
    data.result_cache().service.clear();
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
/// `cargo test --release -p sparkles --test service_enhancer -- --ignored --nocapture`.
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
                local.result_cache().service.clear();
            }
            ep.take();
            let t = Instant::now();
            let r = query(local.snapshot(), q, &opts()).unwrap();
            best = best.min(t.elapsed().as_secs_f64() * 1000.0);
            rows = r.table.len();
            reqs = ep.take().len();
        }
        println!("{name:<40} {rows:>7} rows {reqs:>6} requests {best:>9.1} ms");
    };
    println!("{n} departments, {} ms per request", delay.as_millis());
    time("top label, loop, 1 per request", &q("loop:"), true);
    time("top label, loop:bulk+10", &q("loop:bulk+10:"), true);
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
