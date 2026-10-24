//! `sparkles query` and the execution mode: without `--execution`, a local query with a
//! machine-readable format runs in automatic mode and prints what an eager query prints
//! when the plan stays eager, the text table stays eager, a streamed local query prints
//! what an eager one prints down to the final newline, and a remote query that the server
//! cuts off after its response has started fails instead of looking complete.

#[cfg(feature = "auth")]
use std::io::{Read, Write};
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_sparkles");
const SELECT: &str = "SELECT ?s ?o WHERE { ?s <urn:p> ?o } ORDER BY ?s";

#[track_caller]
fn run(args: &[&str]) -> String {
    let o = Command::new(BIN).args(args).output().unwrap();
    assert!(
        o.status.success(),
        "{args:?}\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    );
    String::from_utf8_lossy(&o.stdout).into_owned()
}

#[test]
fn a_default_local_query_prints_what_an_eager_one_prints() {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("data.ttl");
    std::fs::write(&data, "<urn:a> <urn:p> 1 .\n<urn:b> <urn:p> 2 .\n").unwrap();
    let db = dir.path().join("db").to_str().unwrap().to_string();
    run(&["load", "--loc", &db, data.to_str().unwrap()]);
    for format in ["json", "tsv", "csv", "xml", "text"] {
        let default = run(&["query", "--loc", &db, "--results", format, SELECT]);
        let eager = run(&[
            "query",
            "--loc",
            &db,
            "--results",
            format,
            "--execution",
            "eager",
            SELECT,
        ]);
        assert_eq!(default, eager, "{format}");
    }
}

#[test]
fn a_streamed_local_query_prints_what_an_eager_one_prints() {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("data.ttl");
    std::fs::write(&data, "<urn:a> <urn:p> 1 .\n<urn:b> <urn:p> 2 .\n").unwrap();
    let db = dir.path().join("db").to_str().unwrap().to_string();
    run(&["load", "--loc", &db, data.to_str().unwrap()]);
    for query in [SELECT, "ASK { ?s <urn:p> ?o }"] {
        for format in ["json", "tsv", "csv", "xml"] {
            let output = |execution: &str| {
                run(&[
                    "query",
                    "--loc",
                    &db,
                    "--results",
                    format,
                    "--execution",
                    execution,
                    query,
                ])
            };
            assert_eq!(output("streaming"), output("eager"), "{format} {query}");
        }
    }
}

// a remote query needs the client that the auth feature brings in; without it the command
// refuses at once and the stub server below would wait for a connection that never comes
#[cfg(feature = "auth")]
#[test]
fn a_remote_answer_cut_off_after_it_started_fails_the_command() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = std::thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        let mut request = Vec::new();
        let mut buf = [0u8; 4096];
        while !request.windows(4).any(|w| w == b"\r\n\r\n") {
            let n = socket.read(&mut buf).unwrap();
            if n == 0 {
                break;
            }
            request.extend_from_slice(&buf[..n]);
        }
        let body = "?s\t?o\n<urn:a>\t1\n";
        let response = format!(
            "HTTP/1.1 200 OK\r\ncontent-type: text/tab-separated-values\r\n\
             transfer-encoding: chunked\r\n\r\n{:x}\r\n{body}\r\n",
            body.len()
        );
        socket.write_all(response.as_bytes()).unwrap();
        socket.flush().unwrap();
        // the connection closes without the terminating chunk
        String::from_utf8_lossy(&request).into_owned()
    });
    let o = Command::new(BIN)
        .args([
            "query",
            "--server",
            &format!("http://127.0.0.1:{port}"),
            "--dataset",
            "ds",
            "--results",
            "tsv",
            SELECT,
        ])
        .output()
        .unwrap();
    let request = server.join().unwrap();
    assert!(
        !o.status.success(),
        "a truncated answer exited successfully\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    );
    // without --execution the server's default applies
    assert!(!request.contains("execution="), "{request}");
}
