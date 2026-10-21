//! Write workloads for the durable-commit measurements of docs/BENCHMARKS.md: serial
//! single-triple commits, concurrent writers, and concurrent writers with readers.
//!
//! Build and run (std only, one file):
//!
//!     rustc --edition 2024 -O -o write-bench scripts/write-bench.rs
//!     write-bench --addr 127.0.0.1:3030 --ds bench serial --op literal --changes 5000
//!     write-bench --addr 127.0.0.1:3030 --ds bench writers --clients 4 --changes 50000
//!     write-bench --addr 127.0.0.1:3030 --ds bench mixed --clients 4 --readers 2 --changes 5000
//!     write-bench --addr 127.0.0.1:3030 --ds bench writers --clients 4 --message "a commit"
//!     write-bench --addr 127.0.0.1:3030 --ds bench mixed --clients 4 --write-rate 200
//!
//! Every update is one HTTP request that inserts or deletes one triple, sent on a kept-alive
//! connection, so a latency is the server's whole answer. The receipts (the
//! `Sparkles-Commit` header) of all successful workload writes, including warmups, are
//! required, and the run fails unless they are distinct and contiguous. Untimed delete
//! seeds also require exactly one valid receipt. Serial throughput excludes warmups.
//! A request is sent again on a new connection only when writing it failed, or when the
//! server had already closed the kept-alive connection before it was sent. A request whose
//! answer cannot be read counts as an error and fails the run, because the server may
//! have committed it. The result is one JSON object on stdout.
//!
//! Scenarios
//!   serial   one client, `--changes` requests after `--warmup` untimed ones
//!   writers  `--clients` clients share `--changes` requests
//!   mixed    as writers, with `--readers` readers that each ask for a count at `--rate`
//!            requests per second until the writers are done
//! `--write-rate N` paces the writers of writers and mixed to N requests per second in
//! total. Each client sends on its own schedule of N / clients requests per second, as the
//! readers do, so two servers can be compared at the same commit rate. Without it, every
//! client sends its next request as soon as the last one is answered.
//! Operations (`--op`, writers and mixed always use `literal`)
//!   literal  a new subject and a new literal, so the commit adds a term to the vocabulary
//!   iri      a new edge between two people of the generated dataset (scripts/gen-data.py)
//!   delete   the same edges, inserted untimed first and then deleted
use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

struct Conn {
    addr: String,
    s: TcpStream,
    buf: Vec<u8>,
}

struct Resp {
    status: u16,
    commit: Option<u64>,
    body: Vec<u8>,
}

impl Conn {
    fn new(addr: &str) -> Conn {
        Conn {
            addr: addr.to_string(),
            s: Conn::dial(addr),
            buf: Vec::new(),
        }
    }
    fn dial(addr: &str) -> TcpStream {
        let s = TcpStream::connect(addr).unwrap_or_else(|e| panic!("connect {addr}: {e}"));
        s.set_nodelay(true).unwrap();
        s.set_read_timeout(Some(Duration::from_secs(60))).unwrap();
        s
    }
    fn redial(&mut self) {
        self.s = Conn::dial(&self.addr);
        self.buf.clear();
    }
    /// Dial again if the server closed the kept-alive connection while it was idle. Call it
    /// outside the timed span; it sends nothing.
    fn ready(&mut self) {
        if !self.buf.is_empty() {
            return;
        }
        // With nothing sent, anything but "no data yet" means the connection is unusable:
        // the server closed it, reset it, or said something no request asked for.
        let closed = self.s.set_nonblocking(true).is_ok()
            && !matches!(
                self.s.peek(&mut [0u8; 1]),
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock
            );
        self.s.set_nonblocking(false).unwrap();
        if closed {
            self.redial();
        }
    }
    /// Send `req` and read its answer. The request is sent again on a new connection only
    /// when writing it failed. Once it was written the server may have run it, so a failed
    /// read is an error and is never retried.
    fn request(&mut self, req: &[u8]) -> Result<Resp, String> {
        if self.s.write_all(req).is_err() {
            self.redial();
            self.s
                .write_all(req)
                .map_err(|e| format!("send to {}: {e}", self.addr))?;
        }
        match self.read() {
            Some(r) => Ok(r),
            None => {
                // the connection's state is unknown, so the next request gets a new one
                self.redial();
                Err(format!(
                    "no answer from {} after the request was sent",
                    self.addr
                ))
            }
        }
    }
    fn read(&mut self) -> Option<Resp> {
        let mut chunk = [0u8; 16384];
        let head_end = loop {
            if let Some(p) = find(&self.buf, b"\r\n\r\n") {
                break p;
            }
            match self.s.read(&mut chunk) {
                Ok(0) | Err(_) => return None,
                Ok(n) => self.buf.extend_from_slice(&chunk[..n]),
            }
        };
        let head = String::from_utf8_lossy(&self.buf[..head_end]).to_string();
        let mut lines = head.split("\r\n");
        let status: u16 = lines.next()?.split(' ').nth(1)?.parse().ok()?;
        let (mut len, mut chunked, mut commit) = (0usize, false, None);
        let mut receipt_headers = 0usize;
        for l in lines {
            let Some((k, v)) = l.split_once(':') else {
                continue;
            };
            let (k, v) = (k.to_ascii_lowercase(), v.trim());
            match k.as_str() {
                "content-length" => len = v.parse().ok()?,
                "transfer-encoding" => chunked = v.contains("chunked"),
                "sparkles-commit" => {
                    receipt_headers += 1;
                    commit = if receipt_headers == 1 {
                        v.parse().ok()
                    } else {
                        None
                    };
                }
                _ => {}
            }
        }
        self.buf.drain(..head_end + 4);
        let body = if chunked {
            let mut body = Vec::new();
            loop {
                let line_end = loop {
                    if let Some(p) = find(&self.buf, b"\r\n") {
                        break p;
                    }
                    self.fill(&mut chunk)?;
                };
                let n = usize::from_str_radix(
                    String::from_utf8_lossy(&self.buf[..line_end]).trim(),
                    16,
                )
                .ok()?;
                self.buf.drain(..line_end + 2);
                while self.buf.len() < n + 2 {
                    self.fill(&mut chunk)?;
                }
                body.extend_from_slice(&self.buf[..n]);
                self.buf.drain(..n + 2);
                if n == 0 {
                    break;
                }
            }
            body
        } else {
            while self.buf.len() < len {
                self.fill(&mut chunk)?;
            }
            self.buf.drain(..len).collect()
        };
        Some(Resp {
            status,
            commit,
            body,
        })
    }
    fn fill(&mut self, chunk: &mut [u8]) -> Option<()> {
        match self.s.read(chunk) {
            Ok(0) | Err(_) => None,
            Ok(n) => {
                self.buf.extend_from_slice(&chunk[..n]);
                Some(())
            }
        }
    }
}

fn find(h: &[u8], n: &[u8]) -> Option<usize> {
    h.windows(n.len()).position(|w| w == n)
}

/// The commit message of every update (`--message`), sent as `Sparkles-Commit-Message`.
static MESSAGE: std::sync::OnceLock<String> = std::sync::OnceLock::new();

fn update_request(ds: &str, addr: &str, update: &str) -> Vec<u8> {
    let message = MESSAGE
        .get()
        .map(|m| format!("Sparkles-Commit-Message: {m}\r\n"))
        .unwrap_or_default();
    format!(
        "POST /{ds}/update HTTP/1.1\r\nHost: {addr}\r\n{message}Content-Type: application/sparql-update\r\nContent-Length: {}\r\n\r\n{update}",
        update.len()
    )
    .into_bytes()
}

fn query_request(ds: &str, addr: &str, query: &str) -> Vec<u8> {
    let mut q = String::new();
    for b in query.bytes() {
        if b.is_ascii_alphanumeric() {
            q.push(b as char);
        } else {
            q.push_str(&format!("%{b:02X}"));
        }
    }
    format!(
        "GET /{ds}/sparql?query={q} HTTP/1.1\r\nHost: {addr}\r\nAccept: application/sparql-results+json\r\n\r\n"
    )
    .into_bytes()
}

#[derive(Clone)]
struct Args {
    addr: String,
    ds: String,
    scenario: String,
    op: String,
    clients: usize,
    readers: usize,
    rate: f64,
    write_rate: f64,
    changes: usize,
    warmup: usize,
    people: usize,
    run: String,
    literal_predicate: String,
    samples: Option<String>,
}

fn parse() -> Args {
    let mut a = Args {
        addr: "127.0.0.1:3030".into(),
        ds: "bench".into(),
        scenario: String::new(),
        op: "literal".into(),
        clients: 4,
        readers: 2,
        rate: 25.0,
        write_rate: 0.0,
        changes: 5000,
        warmup: 200,
        people: 100_000,
        run: format!(
            "{:x}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_millis()
        ),
        literal_predicate: "urn:wb:p".into(),
        samples: None,
    };
    let mut it = std::env::args().skip(1);
    while let Some(k) = it.next() {
        let mut val = || it.next().unwrap_or_else(|| panic!("{k} needs a value"));
        match k.as_str() {
            "--addr" => a.addr = val(),
            "--ds" => a.ds = val(),
            "--op" => a.op = val(),
            "--clients" => a.clients = val().parse().unwrap(),
            "--readers" => a.readers = val().parse().unwrap(),
            "--rate" => a.rate = val().parse().unwrap(),
            "--write-rate" => a.write_rate = val().parse().unwrap(),
            "--changes" => a.changes = val().parse().unwrap(),
            "--warmup" => a.warmup = val().parse().unwrap(),
            "--people" => a.people = val().parse().unwrap(),
            "--run" => a.run = val(),
            "--literal-predicate" => a.literal_predicate = val(),
            "--samples" => a.samples = Some(val()),
            "--message" => {
                let _ = MESSAGE.set(val());
            }
            s if !s.starts_with('-') && a.scenario.is_empty() => a.scenario = s.to_string(),
            s => panic!("unknown argument {s}"),
        }
    }
    assert!(
        ["serial", "writers", "mixed"].contains(&a.scenario.as_str()),
        "scenario: serial, writers or mixed"
    );
    assert!(
        a.write_rate >= 0.0 && a.write_rate.is_finite(),
        "--write-rate must be a number of requests per second"
    );
    assert!(
        a.write_rate == 0.0 || a.scenario != "serial",
        "--write-rate paces writers and mixed only"
    );
    assert!(
        !a.literal_predicate.is_empty()
            && !a
                .literal_predicate
                .chars()
                .any(|c| c.is_whitespace() || "<>\"{}|^`\\".contains(c)),
        "--literal-predicate must be an absolute IRI without SPARQL delimiters"
    );
    assert!(
        a.literal_predicate.contains(':'),
        "--literal-predicate must be absolute"
    );
    assert!(
        a.changes > 0 && a.clients > 0,
        "positive changes and clients required"
    );
    assert!(
        matches!(a.op.as_str(), "literal" | "iri" | "delete" | "geo"),
        "operation: literal, iri, delete or geo"
    );
    a
}

/// The update for request number `i` of this run.
fn update(a: &Args, op: &str, i: usize) -> String {
    match op {
        "literal" => format!(
            "INSERT DATA {{ <urn:wb:{}:s{i}> <{}> \"value {} {i}\" }}",
            a.run, a.literal_predicate, a.run
        ),
        "geo" => format!(
            "INSERT DATA {{ <urn:wb:{}:s{i}> <http://www.opengis.net/ont/geosparql#asWKT> \"POINT({} {})\"^^<http://www.opengis.net/ont/geosparql#wktLiteral> }}",
            a.run,
            100 + i % 50,
            40 + i % 30
        ),
        "iri" | "delete" => {
            let (s, o) = edge(a, i);
            let verb = if op == "iri" { "INSERT" } else { "DELETE" };
            format!(
                "{verb} DATA {{ <http://example.org/person/{s}> <urn:wb:link:{}> <http://example.org/person/{o}> }}",
                a.run
            )
        }
        _ => panic!("operation: literal, iri, delete or geo"),
    }
}

fn edge(a: &Args, i: usize) -> (usize, usize) {
    let n = a.people.max(2);
    ((i * 7919 + 13) % n, (i * 104_729 + 7) % n)
}

fn pct(v: &[f64], p: f64) -> f64 {
    if v.is_empty() {
        return 0.0;
    }
    v[((v.len() as f64 - 1.0) * p).round() as usize]
}

fn stats(mut v: Vec<f64>) -> String {
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let mean = v.iter().sum::<f64>() / v.len().max(1) as f64;
    format!(
        "{{\"n\":{},\"p50_ms\":{:.3},\"p90_ms\":{:.3},\"p99_ms\":{:.3},\"mean_ms\":{:.3},\"max_ms\":{:.3}}}",
        v.len(),
        pct(&v, 0.5),
        pct(&v, 0.9),
        pct(&v, 0.99),
        mean,
        v.last().copied().unwrap_or(0.0)
    )
}

fn main() {
    let a = parse();
    let mut sample_file = a.samples.as_ref().map(|path| {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
            .expect("create new latency sample file");
        writeln!(file, "kind\tlatency_ms").unwrap();
        file
    });
    let op = a.op.clone();
    // untimed seeding: deletes need their triples, and every run warms the connection
    if op == "delete" {
        let mut c = Conn::new(&a.addr);
        for i in 0..a.changes + a.warmup {
            c.ready();
            let r = c
                .request(&update_request(&a.ds, &a.addr, &update(&a, "iri", i)))
                .unwrap_or_else(|e| {
                    eprintln!("delete setup request {} failed: {e}", i + 1);
                    std::process::exit(2);
                });
            if r.status != 200 || r.commit.is_none() {
                eprintln!(
                    "delete setup request {} failed: HTTP {}, valid Sparkles-Commit receipt {}, {}",
                    i + 1,
                    r.status,
                    r.commit.is_some(),
                    String::from_utf8_lossy(&r.body)
                );
                std::process::exit(2);
            }
        }
    }
    let total = if a.scenario == "serial" {
        a.changes + a.warmup
    } else {
        a.changes
    };
    let clients = if a.scenario == "serial" { 1 } else { a.clients };
    let next = Arc::new(AtomicUsize::new(0));
    let done = Arc::new(AtomicBool::new(false));
    let receipts = Arc::new(Mutex::new(Vec::<u64>::new()));
    let start_gate = Arc::new(std::sync::Barrier::new(clients + 1));
    let mut writers = Vec::new();
    for k in 0..clients {
        let (a, op, next, receipts, gate) = (
            a.clone(),
            op.clone(),
            next.clone(),
            receipts.clone(),
            start_gate.clone(),
        );
        writers.push(std::thread::spawn(move || {
            let mut c = Conn::new(&a.addr);
            let mut lat = Vec::new();
            let mut mine = Vec::new();
            let mut errors = 0usize;
            let mut missing_receipts = 0usize;
            let (mut timed_start, mut timed_end) = (None, None);
            gate.wait();
            // with --write-rate, client k of n sends every n / rate seconds, offset by k / rate
            let period = (a.write_rate > 0.0)
                .then(|| Duration::from_secs_f64(clients as f64 / a.write_rate));
            let mut due = Instant::now()
                + Duration::from_secs_f64(if a.write_rate > 0.0 {
                    k as f64 / a.write_rate
                } else {
                    0.0
                });
            loop {
                if let Some(period) = period {
                    let now = Instant::now();
                    if now < due {
                        std::thread::sleep(due - now);
                    }
                    due += period;
                }
                let i = next.fetch_add(1, Ordering::Relaxed);
                if i >= total {
                    break;
                }
                let req = update_request(&a.ds, &a.addr, &update(&a, &op, i));
                if a.scenario == "serial" && i == a.warmup {
                    timed_start = Some(Instant::now());
                }
                c.ready();
                let t = Instant::now();
                let r = c.request(&req);
                let ms = t.elapsed().as_secs_f64() * 1e3;
                if a.scenario == "serial" && i >= a.warmup {
                    timed_end = Some(Instant::now());
                }
                let r = match r {
                    Ok(r) => r,
                    Err(e) => {
                        eprintln!("update request {}: {e}", i + 1);
                        errors += 1;
                        continue;
                    }
                };
                if r.status != 200 {
                    errors += 1;
                    continue;
                }
                // the first `warmup` requests of a serial run are not timed
                if a.scenario != "serial" || i >= a.warmup {
                    lat.push(ms);
                }
                if let Some(commit) = r.commit {
                    mine.push(commit);
                } else {
                    missing_receipts += 1;
                }
            }
            receipts.lock().unwrap().extend(mine);
            let timed_span = timed_start
                .zip(timed_end)
                .map(|(start, end)| end.duration_since(start));
            (lat, errors, missing_receipts, timed_span)
        }));
    }
    let mut readers = Vec::new();
    if a.scenario == "mixed" {
        for _ in 0..a.readers {
            let (a, done) = (a.clone(), done.clone());
            readers.push(std::thread::spawn(move || {
                let mut c = Conn::new(&a.addr);
                let req = query_request(
                    &a.ds,
                    &a.addr,
                    "SELECT (COUNT(*) AS ?n) WHERE { ?s <urn:wb:p> ?o }",
                );
                let mut lat = Vec::new();
                let (mut last_count, mut last_commit) = (0u64, 0u64);
                let mut bad = 0usize;
                let period = Duration::from_secs_f64(1.0 / a.rate);
                let mut due = Instant::now();
                while !done.load(Ordering::Relaxed) {
                    let now = Instant::now();
                    if now < due {
                        std::thread::sleep(due - now);
                    }
                    due += period;
                    c.ready();
                    let t = Instant::now();
                    let r = match c.request(&req) {
                        Ok(r) => r,
                        Err(e) => {
                            eprintln!("count request: {e}");
                            bad += 1;
                            continue;
                        }
                    };
                    lat.push(t.elapsed().as_secs_f64() * 1e3);
                    // counts and commits never go back within one reader
                    let text = String::from_utf8_lossy(&r.body);
                    let n = text
                        .split("\"value\":\"")
                        .nth(1)
                        .and_then(|s| s.split('"').next())
                        .and_then(|s| s.parse::<u64>().ok())
                        .unwrap_or(0);
                    let commit = r.commit.unwrap_or(0);
                    if r.status != 200 || n < last_count || commit < last_commit {
                        bad += 1;
                    }
                    last_count = n;
                    last_commit = commit;
                }
                (lat, bad)
            }));
        }
    }
    start_gate.wait();
    let t0 = Instant::now();
    let mut lat = Vec::new();
    let mut per_client = Vec::new();
    let mut errors = 0;
    let mut missing_receipts = 0;
    let mut serial_elapsed = None;
    for w in writers {
        let (l, e, missing, span) = w.join().unwrap();
        serial_elapsed = serial_elapsed.or(span);
        per_client.push(l.len());
        lat.extend(l);
        errors += e;
        missing_receipts += missing;
    }
    let elapsed = if a.scenario == "serial" {
        serial_elapsed.unwrap_or_default().as_secs_f64()
    } else {
        t0.elapsed().as_secs_f64()
    };
    done.store(true, Ordering::Relaxed);
    let (mut rlat, mut rbad) = (Vec::new(), 0);
    for r in readers {
        let (l, b) = r.join().unwrap();
        rlat.extend(l);
        rbad += b;
    }
    let mut rc = receipts.lock().unwrap().clone();
    rc.sort_unstable();
    let contiguous = rc.windows(2).all(|w| w[1] == w[0] + 1);
    let distinct = {
        let mut d = rc.clone();
        d.dedup();
        d.len() == rc.len()
    };
    let timed = lat.len();
    let mean_share = per_client.iter().sum::<usize>() as f64 / per_client.len().max(1) as f64;
    let spread = per_client
        .iter()
        .map(|n| (*n as f64 - mean_share).abs() / mean_share.max(1.0))
        .fold(0.0, f64::max);
    if let Some(file) = &mut sample_file {
        for value in &lat {
            writeln!(file, "writer\t{value:.9}").unwrap();
        }
        for value in &rlat {
            writeln!(file, "reader\t{value:.9}").unwrap();
        }
    }
    println!(
        "{{\"scenario\":\"{}\",\"op\":\"{}\",\"clients\":{},\"write_rate\":{},\"changes\":{},\"errors\":{},\"missing_receipts\":{},\"seconds\":{:.3},\"tps\":{:.1},\"writers\":{},\"receipts_contiguous\":{},\"receipts_distinct\":{},\"first_receipt\":{},\"per_client\":{:?},\"max_share_deviation\":{:.3},\"readers\":{},\"reader_violations\":{}}}",
        a.scenario,
        op,
        clients,
        a.write_rate,
        timed,
        errors,
        missing_receipts,
        elapsed,
        if elapsed > 0.0 {
            timed as f64 / elapsed
        } else {
            0.0
        },
        stats(lat),
        contiguous,
        distinct,
        rc.first().copied().unwrap_or(0),
        per_client,
        spread,
        if rlat.is_empty() {
            "null".to_string()
        } else {
            stats(rlat)
        },
        rbad
    );
    if errors > 0
        || missing_receipts > 0
        || rc.len() != total - errors
        || !contiguous
        || !distinct
        || rbad > 0
    {
        std::process::exit(2);
    }
}
