//! End to end: `sparkles mcp --data FILE` as a child process, driven over stdin/stdout
//! the way an MCP host drives it.
#![cfg(feature = "mcp")]

use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

const FIXTURE: &str = r#"@prefix ex:   <http://ex.org/> .
@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
ex:alice a ex:Person ; rdfs:label "Alice"@en ; ex:knows ex:bob .
ex:bob   a ex:Person ; rdfs:label "Bob" .
"#;

#[test]
fn stdio_session() {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("fixture.ttl");
    std::fs::write(&data, FIXTURE).unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_sparkles"))
        .args(["mcp", "--data"])
        .arg(&data)
        .args(["--name", "t"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let stdout = child.stdout.take().unwrap();
    // read stdout on a thread so a hung server fails the test instead of blocking it
    let (tx, rx) = mpsc::channel::<String>();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            let Ok(line) = line else { break };
            if tx.send(line).is_err() {
                break;
            }
        }
    });
    let mut exchange = |msg: Value| -> Option<Value> {
        writeln!(stdin, "{msg}").unwrap();
        stdin.flush().unwrap();
        msg.get("id")?;
        let line = rx
            .recv_timeout(Duration::from_secs(30))
            .expect("no response");
        Some(serde_json::from_str(&line).expect("stdout carries only JSON-RPC lines"))
    };

    // the legacy handshake, as most hosts still speak it
    let r = exchange(
        json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
        "protocolVersion": "2025-11-25", "capabilities": {},
        "clientInfo": {"name": "e2e", "version": "1"}}}),
    )
    .unwrap();
    assert_eq!(r["result"]["protocolVersion"], "2025-11-25");
    assert_eq!(r["result"]["serverInfo"]["name"], "sparkles");
    assert!(exchange(json!({"jsonrpc": "2.0", "method": "notifications/initialized"})).is_none());

    let r = exchange(json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"})).unwrap();
    let names: Vec<&str> = r["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert_eq!(
        names[..4],
        [
            "list_datasets",
            "describe_schema",
            "draft_shapes",
            "sparql_query"
        ]
    );

    let r = exchange(json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": {
        "name": "sparql_query",
        "arguments": {"query": "SELECT ?p ?name WHERE { ?p a ex:Person ; rdfs:label ?name } ORDER BY ?p"}}}))
    .unwrap();
    assert_eq!(r["result"]["isError"], false);
    assert_eq!(
        r["result"]["content"][0]["text"],
        "# SELECT · rows 1–2 of 2 · commit 1\nPREFIX ex: <http://ex.org/>\n?p\t?name\nex:alice\t\"Alice\"@en\nex:bob\t\"Bob\""
    );

    let r = exchange(
        json!({"jsonrpc": "2.0", "id": 4, "method": "tools/call", "params": {
        "name": "list_datasets", "arguments": {}}}),
    )
    .unwrap();
    assert_eq!(r["result"]["structuredContent"]["datasets"][0]["quads"], 5);

    // closing stdin ends the process with status 0
    drop(stdin);
    let status = child.wait().unwrap();
    assert!(status.success(), "{status}");
}

#[test]
fn stdio_modern_session() {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("fixture.ttl");
    std::fs::write(&data, FIXTURE).unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_sparkles"))
        .args(["mcp", "--name", "t", "--data"])
        .arg(&data)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let meta = json!({"io.modelcontextprotocol/protocolVersion": "2026-07-28",
                      "io.modelcontextprotocol/clientCapabilities": {}});
    let mut stdin = child.stdin.take().unwrap();
    for (id, method, params) in [
        (1, "server/discover", json!({"_meta": meta})),
        (
            2,
            "tools/call",
            json!({"_meta": meta, "name": "describe_resource", "arguments": {"iri": "ex:bob"}}),
        ),
    ] {
        writeln!(
            stdin,
            "{}",
            json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params})
        )
        .unwrap();
    }
    drop(stdin);
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success());
    let lines: Vec<Value> = String::from_utf8(out.stdout)
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(lines.len(), 2);
    let by_id = |id: u64| lines.iter().find(|l| l["id"] == id).unwrap();
    assert_eq!(by_id(1)["result"]["supportedVersions"][0], "2026-07-28");
    let s = &by_id(2)["result"]["structuredContent"];
    assert_eq!(s["label"], "Bob");
    assert_eq!(s["incoming"]["triples"][0]["s"], "ex:alice");
}

#[test]
fn startup_error_exits_1() {
    let out = Command::new(env!("CARGO_BIN_EXE_sparkles"))
        .args(["mcp", "--data", "/nonexistent/file.ttl"])
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert!(out.stdout.is_empty());
}
