//! The outbound policy on the real request paths (SERVICE, LOAD) against a local HTTP
//! server: refused destinations are never contacted, names connect to the addresses
//! the policy checked, every redirect hop is checked, and slow or oversized responses
//! fail.

use sparkles::Error;
use sparkles::outbound::{Allow, OutboundPolicy, Resolver};
use sparkles::sparql::QueryOptions;
use sparkles::store::{Store, StoreOptions};
use std::collections::HashMap;
use std::io::{self, Read, Write};
use std::net::{IpAddr, TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

const RESULTS: &str =
    r#"{"head":{"vars":["x"]},"results":{"bindings":[{"x":{"type":"literal","value":"remote"}}]}}"#;

/// What the server does for one request.
enum Step {
    Write(Vec<u8>),
    Sleep(Duration),
}

fn reply(status: &str, headers: &[(&str, &str)], body: &[u8]) -> Vec<Step> {
    let mut head = format!("HTTP/1.1 {status}\r\nconnection: close\r\n");
    for (k, v) in headers {
        head.push_str(&format!("{k}: {v}\r\n"));
    }
    head.push_str(&format!("content-length: {}\r\n\r\n", body.len()));
    let mut out = head.into_bytes();
    out.extend_from_slice(body);
    vec![Step::Write(out)]
}

fn results() -> Vec<Step> {
    reply(
        "200 OK",
        &[("content-type", "application/sparql-results+json")],
        RESULTS.as_bytes(),
    )
}

fn redirect(to: &str) -> Vec<Step> {
    reply("302 Found", &[("location", to)], b"")
}

/// A local HTTP/1.1 server answering each request with `handler(request path)`, and
/// counting connections.
struct Server {
    port: u16,
    conns: Arc<AtomicUsize>,
}

impl Server {
    fn start(handler: impl Fn(&str) -> Vec<Step> + Send + Sync + 'static) -> Server {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        let conns = Arc::new(AtomicUsize::new(0));
        let (n, handler) = (conns.clone(), Arc::new(handler));
        std::thread::spawn(move || {
            for c in l.incoming() {
                let Ok(c) = c else { continue };
                n.fetch_add(1, Ordering::SeqCst);
                let handler = handler.clone();
                std::thread::spawn(move || serve(c, &*handler));
            }
        });
        Server { port, conns }
    }

    fn url(&self, path: &str) -> String {
        format!("http://127.0.0.1:{}{path}", self.port)
    }

    fn conns(&self) -> usize {
        self.conns.load(Ordering::SeqCst)
    }
}

fn serve(mut c: TcpStream, handler: &dyn Fn(&str) -> Vec<Step>) {
    let _ = c.set_read_timeout(Some(Duration::from_secs(5)));
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
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
    let path = head.split_whitespace().nth(1).unwrap_or("/").to_string();
    for step in handler(&path) {
        match step {
            Step::Write(b) => {
                if c.write_all(&b).is_err() {
                    return;
                }
            }
            Step::Sleep(d) => std::thread::sleep(d),
        }
    }
}

/// A resolver answering from a table (names the system resolver would not know).
struct Fake(HashMap<&'static str, Vec<IpAddr>>);

impl Resolver for Fake {
    fn resolve(&self, host: &str) -> io::Result<Vec<IpAddr>> {
        self.0
            .get(host)
            .cloned()
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no such host"))
    }
}

fn fake(entries: &[(&'static str, &[&str])]) -> Arc<dyn Resolver> {
    Arc::new(Fake(
        entries
            .iter()
            .map(|(h, a)| (*h, a.iter().map(|s| s.parse().unwrap()).collect()))
            .collect(),
    ))
}

fn private_ok() -> OutboundPolicy {
    OutboundPolicy {
        allow_private: true,
        ..Default::default()
    }
}

fn service(endpoint: &str, silent: bool, policy: OutboundPolicy) -> sparkles::Result<usize> {
    let store = Store::in_memory(StoreOptions::default());
    let silent = if silent { "SILENT " } else { "" };
    let q = format!("SELECT ?x WHERE {{ SERVICE {silent}<{endpoint}> {{ ?x ?p ?o }} }}");
    let opts = QueryOptions {
        allow_service: true,
        outbound: policy,
        ..Default::default()
    };
    let r = sparkles::sparql::query(store.snapshot(), &q, &opts)?;
    Ok(r.len())
}

fn load(url: &str, policy: OutboundPolicy) -> (sparkles::Result<()>, u64) {
    let store = Store::in_memory(StoreOptions::default());
    let opts = QueryOptions {
        outbound: policy,
        ..Default::default()
    };
    let r = sparkles::sparql::update::update(&store, &format!("LOAD <{url}>"), &opts);
    (r.map(|_| ()), store.snapshot().len())
}

fn refusal<T: std::fmt::Debug>(r: sparkles::Result<T>) -> String {
    match r {
        Err(Error::NotPermitted(m)) => m,
        other => panic!("expected a refusal, got {other:?}"),
    }
}

fn failure<T: std::fmt::Debug>(r: sparkles::Result<T>) -> String {
    match r {
        Err(e @ (Error::Service(_) | Error::Invalid(_))) => e.to_string(),
        other => panic!("expected a failure, got {other:?}"),
    }
}

#[test]
fn loopback_is_refused_by_default() {
    let s = Server::start(|_| results());
    let m = refusal(service(&s.url("/sparql"), false, Default::default()));
    assert!(m.contains("127.0.0.1 is a loopback address"), "{m}");
    // SILENT hides failures, not refusals
    refusal(service(&s.url("/sparql"), true, Default::default()));
    let (r, n) = load(&s.url("/d.ttl"), Default::default());
    assert!(refusal(r).contains("loopback"));
    assert_eq!(n, 0);
    for url in [
        "http://[::1]:9/sparql",
        "http://[::ffff:127.0.0.1]:9/sparql",
        "http://10.0.0.1:9/sparql",
        "http://169.254.169.254/latest/meta-data/",
        "http://[fd00::1]:9/sparql",
        "http://[fe80::1]:9/sparql",
    ] {
        refusal(service(url, false, Default::default()));
    }
    assert_eq!(s.conns(), 0);
    // allowed explicitly
    assert_eq!(service(&s.url("/sparql"), false, private_ok()).unwrap(), 1);
    let allow = OutboundPolicy {
        allow: vec!["127.0.0.1".parse().unwrap()],
        ..Default::default()
    };
    assert_eq!(service(&s.url("/sparql"), false, allow).unwrap(), 1);
    assert_eq!(s.conns(), 2);
}

#[test]
fn load_through_the_policy() {
    let s = Server::start(|_| {
        reply(
            "200 OK",
            &[("content-type", "text/turtle")],
            b"<urn:a> <urn:p> <urn:b> .",
        )
    });
    let (r, n) = load(&s.url("/d.ttl"), private_ok());
    r.unwrap();
    assert_eq!(n, 1);
    // an error status is a failure, not data
    let e = Server::start(|_| reply("404 Not Found", &[("content-type", "text/turtle")], b""));
    let (r, _) = load(&e.url("/d.ttl"), private_ok());
    assert!(failure(r).contains("404"));
    // other schemes are not fetched
    let (r, _) = load("ftp://example.org/d.ttl", private_ok());
    assert!(failure(r).contains("only http and https"));
}

#[test]
fn names_connect_to_the_checked_addresses() {
    let s = Server::start(|_| results());
    let resolver = fake(&[
        ("svc.test", &["127.0.0.1"]),
        ("mixed.test", &["127.0.0.1", "93.184.216.34"]),
        ("mixed-public-first.test", &["93.184.216.34", "10.0.0.1"]),
    ]);
    let url = |h: &str| format!("http://{h}:{}/sparql", s.port);
    let policy = |allow_private: bool, allow: &[&str]| OutboundPolicy {
        allow_private,
        allow: allow.iter().map(|a| a.parse::<Allow>().unwrap()).collect(),
        resolver: resolver.clone(),
        ..Default::default()
    };
    // the name exists only in the fake resolver: the connection used its answer
    assert_eq!(
        service(&url("svc.test"), false, policy(true, &[])).unwrap(),
        1
    );
    assert_eq!(s.conns(), 1);
    let m = refusal(service(&url("svc.test"), false, policy(false, &[])));
    assert!(
        m.contains("svc.test resolves to 127.0.0.1, a loopback address"),
        "{m}"
    );
    // one disallowed address refuses the name
    let m = refusal(service(&url("mixed.test"), false, policy(false, &[])));
    assert!(m.contains("127.0.0.1, a loopback address"), "{m}");
    let m = refusal(service(
        &url("mixed-public-first.test"),
        false,
        policy(false, &[]),
    ));
    assert!(m.contains("10.0.0.1, a private address"), "{m}");
    let m = refusal(service(
        &url("mixed.test"),
        false,
        policy(false, &["127.0.0.0/8"]),
    ));
    assert!(m.contains("not in the outbound allowlist"), "{m}");
    // an allowlisted name may resolve to a private address
    assert_eq!(
        service(&url("svc.test"), false, policy(false, &["svc.test"])).unwrap(),
        1
    );
    let m = refusal(service(
        &url("svc.test"),
        false,
        policy(true, &["other.test"]),
    ));
    assert!(
        m.contains("svc.test is not in the outbound allowlist"),
        "{m}"
    );
    assert_eq!(s.conns(), 2);
}

#[test]
fn every_redirect_hop_is_checked() {
    let s = Server::start(|path| match path {
        "/to-loopback" => redirect("http://127.0.0.2:9/sparql"),
        "/to-metadata" => redirect("http://169.254.169.254/latest/meta-data/"),
        "/to-name" => redirect("http://evil.test:9/sparql"),
        "/to-ftp" => redirect("ftp://example.org/x"),
        "/loop" => redirect("/loop"),
        "/hop" => redirect("/sparql"),
        _ => results(),
    });
    let policy = OutboundPolicy {
        allow: vec!["good.test".parse().unwrap()],
        resolver: fake(&[("good.test", &["127.0.0.1"]), ("evil.test", &["127.0.0.1"])]),
        ..Default::default()
    };
    let url = |p: &str| format!("http://good.test:{}{p}", s.port);
    assert_eq!(service(&url("/hop"), false, policy.clone()).unwrap(), 1);
    assert_eq!(s.conns(), 2);
    let m = refusal(service(&url("/to-loopback"), false, policy.clone()));
    assert!(m.contains("redirect to http://127.0.0.2:9/sparql"), "{m}");
    let m = refusal(service(&url("/to-metadata"), false, policy.clone()));
    assert!(m.contains("cloud metadata"), "{m}");
    let m = refusal(service(&url("/to-name"), false, policy.clone()));
    assert!(
        m.contains("evil.test is not in the outbound allowlist"),
        "{m}"
    );
    let (r, n) = load(&url("/to-metadata"), policy.clone());
    assert!(refusal(r).contains("cloud metadata"));
    assert_eq!(n, 0);
    // one connection per refused chain: the refused hop was never contacted
    assert_eq!(s.conns(), 6);
    let m = failure(service(&url("/to-ftp"), false, policy.clone()));
    assert!(m.contains("only http and https"), "{m}");
    let before = s.conns();
    let m = failure(service(&url("/loop"), false, policy.clone()));
    assert!(m.contains("more than 5 redirects"), "{m}");
    assert_eq!(s.conns() - before, 6);
    // SILENT hides a failed chain, not a refused one
    assert_eq!(service(&url("/loop"), true, policy.clone()).unwrap(), 1);
    refusal(service(&url("/to-loopback"), true, policy));
}

#[test]
fn oversized_responses_fail() {
    let big = format!(
        r#"{{"head":{{"vars":["x"]}},"results":{{"bindings":[{}]}}}}"#,
        vec![r#"{"x":{"type":"literal","value":"0123456789"}}"#; 10_000].join(",")
    );
    let body = big.clone();
    let s = Server::start(move |path| {
        match path {
        "/declared" => reply(
            "200 OK",
            &[("content-type", "application/sparql-results+json")],
            body.as_bytes(),
        ),
        // no length: counted as it streams
        _ => vec![
            Step::Write(
                b"HTTP/1.1 200 OK\r\nconnection: close\r\ncontent-type: application/sparql-results+json\r\n\r\n"
                    .to_vec(),
            ),
            Step::Write(body.as_bytes().to_vec()),
        ],
    }
    });
    let small = OutboundPolicy {
        max_response_bytes: 64 << 10,
        ..private_ok()
    };
    for path in ["/declared", "/streamed"] {
        let m = failure(service(&s.url(path), false, small.clone()));
        assert!(
            m.contains("larger than the outbound limit of 65536 bytes"),
            "{path}: {m}"
        );
        assert!(service(&s.url(path), false, private_ok()).unwrap() == 10_000);
    }
    let (r, _) = load(&s.url("/streamed.ttl"), small);
    assert!(failure(r).contains("larger than the outbound limit"));

    // compressed data is held to the ceiling once decompressed
    let nt: String = (0..20_000)
        .map(|i| format!("<urn:s{i}> <urn:p> \"{i}\" .\n"))
        .collect();
    let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::best());
    gz.write_all(nt.as_bytes()).unwrap();
    let gz = gz.finish().unwrap();
    let limit = 256 << 10;
    assert!(gz.len() < limit && nt.len() > limit);
    let z =
        Server::start(move |_| reply("200 OK", &[("content-type", "application/n-triples")], &gz));
    let capped = OutboundPolicy {
        max_response_bytes: limit as u64,
        ..private_ok()
    };
    let (r, n) = load(&z.url("/d.nt.gz"), capped);
    assert!(matches!(r, Err(Error::BudgetExceeded(_))), "{r:?}");
    assert_eq!(n, 0);
    let (r, n) = load(&z.url("/d.nt.gz"), private_ok());
    r.unwrap();
    assert_eq!(n, 20_000);
}

#[test]
fn slow_responses_time_out() {
    let s = Server::start(|path| match path {
        "/slow-headers" => {
            let mut steps = vec![Step::Sleep(Duration::from_secs(4))];
            steps.extend(results());
            steps
        }
        // headers at once, then a byte every 100 ms: no read waits long
        _ => {
            let mut steps = vec![Step::Write(
                b"HTTP/1.1 200 OK\r\nconnection: close\r\ncontent-type: text/turtle\r\n\r\n"
                    .to_vec(),
            )];
            for _ in 0..60 {
                steps.push(Step::Sleep(Duration::from_millis(100)));
                steps.push(Step::Write(b" ".to_vec()));
            }
            steps
        }
    });
    let quick = OutboundPolicy {
        timeout: Duration::from_secs(1),
        ..private_ok()
    };
    let t = Instant::now();
    let m = failure(service(&s.url("/slow-headers"), false, quick.clone()));
    assert!(m.contains("no complete response within 1 s"), "{m}");
    let t2 = Instant::now();
    let m = failure(service(&s.url("/trickle"), false, quick.clone()));
    assert!(m.contains("no complete response within 1 s"), "{m}");
    let t3 = Instant::now();
    let (r, _) = load(&s.url("/trickle"), quick);
    assert!(failure(r).contains("no complete response within 1 s"));
    assert!(t2 - t < Duration::from_secs(3), "{:?}", t2 - t);
    assert!(t3 - t2 < Duration::from_secs(3), "{:?}", t3 - t2);
    assert!(t3.elapsed() < Duration::from_secs(3));
    // the query's own deadline bounds SERVICE too
    let store = Store::in_memory(StoreOptions::default());
    let opts = QueryOptions {
        allow_service: true,
        timeout: Some(Duration::from_secs(1)),
        outbound: private_ok(),
        ..Default::default()
    };
    let t = Instant::now();
    let q = format!(
        "SELECT * WHERE {{ SERVICE <{}> {{ ?s ?p ?o }} }}",
        s.url("/slow-headers")
    );
    assert!(sparkles::sparql::query(store.snapshot(), &q, &opts).is_err());
    assert!(t.elapsed() < Duration::from_secs(3));
}
