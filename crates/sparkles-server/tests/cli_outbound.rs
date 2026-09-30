//! The local `query` and `update` reach loopback and private endpoints by default (the
//! operator runs them on their own machine), unlike `serve`; `--outbound-block-private`
//! restores the strict policy, and link-local addresses stay refused either way.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::process::{Command, Output};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

const BIN: &str = env!("CARGO_BIN_EXE_sparkles");

/// An endpoint on 127.0.0.1 answering SPARQL results on `/sparql` and Turtle elsewhere;
/// its port and the number of connections it accepted.
fn endpoint() -> (u16, Arc<AtomicUsize>) {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    let conns = Arc::new(AtomicUsize::new(0));
    let n = conns.clone();
    std::thread::spawn(move || {
        for c in l.incoming() {
            let Ok(mut c) = c else { continue };
            n.fetch_add(1, Ordering::SeqCst);
            let _ = c.set_read_timeout(Some(Duration::from_secs(5)));
            let head = read_request(&mut c);
            let (ctype, body) = if head.contains(" /sparql ") {
                (
                    "application/sparql-results+json",
                    r#"{"head":{"vars":["o"]},"results":{"bindings":[{"o":{"type":"literal","value":"from the endpoint"}}]}}"#,
                )
            } else {
                ("text/turtle", "<urn:a> <urn:p> \"loaded\" .")
            };
            let _ = write!(
                c,
                "HTTP/1.1 200 OK\r\ncontent-type: {ctype}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );
        }
    });
    (port, conns)
}

/// Read a request (head and `Content-Length` body); its head.
fn read_request(c: &mut std::net::TcpStream) -> String {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    while let Ok(k) = c.read(&mut chunk) {
        if k == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..k]);
        let text = String::from_utf8_lossy(&buf);
        if let Some(end) = text.find("\r\n\r\n") {
            let head = text[..end].to_string();
            let len = head
                .lines()
                .find_map(|l| {
                    let (k, v) = l.split_once(':')?;
                    k.eq_ignore_ascii_case("content-length")
                        .then(|| v.trim().parse::<usize>().ok())?
                })
                .unwrap_or(0);
            if buf.len() >= end + 4 + len {
                return head;
            }
        }
    }
    String::from_utf8_lossy(&buf).into_owned()
}

fn sparkles(args: &[&str]) -> Output {
    Command::new(BIN)
        .args(args)
        .env_remove("SPARKLES_SERVER")
        .output()
        .unwrap()
}

fn text(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

#[test]
fn local_service_reaches_loopback_by_default() {
    let (port, conns) = endpoint();
    let q = format!("SELECT ?o {{ SERVICE <http://127.0.0.1:{port}/sparql> {{ ?s ?p ?o }} }}");
    let o = sparkles(&["query", "--results", "tsv", &q]);
    assert!(o.status.success(), "{}", text(&o.stderr));
    assert!(
        text(&o.stdout).contains("\"from the endpoint\""),
        "{}",
        text(&o.stdout)
    );
    assert_eq!(conns.load(Ordering::SeqCst), 1);

    // the strict policy of a server, on request: refused before connecting
    let o = sparkles(&["query", "--outbound-block-private", &q]);
    assert!(!o.status.success());
    assert!(
        text(&o.stderr).contains("127.0.0.1 is a loopback address"),
        "{}",
        text(&o.stderr)
    );
    assert_eq!(conns.load(Ordering::SeqCst), 1);
    let o = sparkles(&[
        "query",
        "--outbound-allow-private",
        "--outbound-block-private",
        &q,
    ]);
    assert!(!o.status.success());
}

#[test]
fn local_service_refuses_link_local() {
    let q = "SELECT * { SERVICE <http://169.254.169.254/latest/meta-data/> { ?s ?p ?o } }";
    let o = sparkles(&["query", "--outbound-timeout", "2", q]);
    assert!(!o.status.success());
    assert!(
        text(&o.stderr).contains("169.254.169.254 is a link-local (cloud metadata) address"),
        "{}",
        text(&o.stderr)
    );
    // --outbound-allow-private does not open it either
    let o = sparkles(&[
        "query",
        "--outbound-allow-private",
        "--outbound-timeout",
        "2",
        q,
    ]);
    assert!(!o.status.success());
    assert!(
        text(&o.stderr).contains("link-local"),
        "{}",
        text(&o.stderr)
    );
}

#[test]
fn local_load_reaches_loopback_by_default() {
    let (port, conns) = endpoint();
    let dir = tempfile::tempdir().unwrap();
    let loc = dir.path().join("db");
    let loc = loc.to_str().unwrap();
    let load = format!("LOAD <http://127.0.0.1:{port}/d.ttl>");
    let o = sparkles(&["update", "--loc", loc, "--outbound-block-private", &load]);
    assert!(!o.status.success());
    assert!(text(&o.stderr).contains("loopback"), "{}", text(&o.stderr));
    assert_eq!(conns.load(Ordering::SeqCst), 0);
    let o = sparkles(&["update", "--loc", loc, &load]);
    assert!(o.status.success(), "{}", text(&o.stderr));
    assert_eq!(conns.load(Ordering::SeqCst), 1);
    let o = sparkles(&[
        "query",
        "--loc",
        loc,
        "--results",
        "tsv",
        "SELECT ?o { <urn:a> <urn:p> ?o }",
    ]);
    assert!(o.status.success(), "{}", text(&o.stderr));
    assert!(
        text(&o.stdout).contains("\"loaded\""),
        "{}",
        text(&o.stdout)
    );
}
