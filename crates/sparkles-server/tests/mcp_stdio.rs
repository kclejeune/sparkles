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
        names[..5],
        [
            "list_datasets",
            "describe_schema",
            "draft_shapes",
            "diff_schema",
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

/// A database `db` under `dir` with a scratch branch `old` that was created in 2020, so
/// that it counts as idle for any TTL without the test waiting. `ttl` is the dataset's
/// `memory.scratchBranches.ttl`.
fn db_with_idle_scratch_branch(dir: &std::path::Path, ttl: Option<&str>) {
    let run = |args: &[&str]| {
        let o = Command::new(env!("CARGO_BIN_EXE_sparkles"))
            .args(args)
            .current_dir(dir)
            .env_remove("SPARKLES_SERVER")
            .output()
            .unwrap();
        assert!(
            o.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&o.stderr)
        );
    };
    run(&["update", "--loc", "db", "INSERT DATA { <urn:a> <urn:p> 1 }"]);
    run(&["branch", "create", "--loc", "db", "old"]);
    let table = dir.join("db/branches.json");
    let mut v: Value = serde_json::from_slice(&std::fs::read(&table).unwrap()).unwrap();
    fn mark(v: &mut Value) -> bool {
        match v {
            Value::Object(o) if o.get("name") == Some(&json!("old")) => {
                o.insert("created".into(), json!("2020-01-01T00:00:00Z"));
                o.insert("scratch".into(), json!({"creator": "agent"}));
                true
            }
            Value::Object(o) => o.values_mut().any(mark),
            Value::Array(a) => a.iter_mut().any(mark),
            _ => false,
        }
    }
    assert!(mark(&mut v), "{v}");
    std::fs::write(&table, serde_json::to_vec_pretty(&v).unwrap()).unwrap();
    if let Some(ttl) = ttl {
        std::fs::write(
            dir.join("db/memory.json"),
            json!({"format": 2, "scratchBranches": {"ttl": ttl}}).to_string(),
        )
        .unwrap();
    }
}

/// The branch names `list_branches` returns in a stdio session over `dir/db`.
fn session_branches(dir: &std::path::Path, extra: &[&str]) -> Vec<String> {
    let mut child = Command::new(env!("CARGO_BIN_EXE_sparkles"))
        .args(["mcp", "--loc", "db"])
        .args(extra)
        .current_dir(dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let stdout = child.stdout.take().unwrap();
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
            .recv_timeout(Duration::from_secs(60))
            .expect("no response");
        Some(serde_json::from_str(&line).expect("stdout carries only JSON-RPC lines"))
    };
    exchange(
        json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
        "protocolVersion": "2025-11-25", "capabilities": {},
        "clientInfo": {"name": "e2e", "version": "1"}}}),
    )
    .unwrap();
    exchange(json!({"jsonrpc": "2.0", "method": "notifications/initialized"}));
    let r = exchange(
        json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": {
        "name": "list_branches", "arguments": {}}}),
    )
    .unwrap();
    assert_eq!(r["result"]["isError"], false, "{r}");
    let names = r["result"]["structuredContent"]["branches"]
        .as_array()
        .unwrap()
        .iter()
        .map(|b| b["name"].as_str().unwrap().to_string())
        .collect();
    drop(stdin);
    assert!(child.wait().unwrap().success());
    names
}

/// A stdio session with `--allow-update` deletes the scratch branch that the dataset's
/// `memory.scratchBranches.ttl` calls idle before its first call. A read-only session,
/// `--no-scratch-expiry` and a dataset without the setting keep it.
#[test]
fn stdio_deletes_idle_scratch_branches_of_a_dataset_setting() {
    let kept = |ttl: Option<&str>, extra: &[&str]| {
        let d = tempfile::tempdir().unwrap();
        db_with_idle_scratch_branch(d.path(), ttl);
        session_branches(d.path(), extra)
    };
    assert_eq!(kept(Some("1h"), &["--allow-update"]), ["main"]);
    assert_eq!(kept(Some("1h"), &[]), ["main", "old"]);
    assert_eq!(
        kept(Some("1h"), &["--allow-update", "--no-scratch-expiry"]),
        ["main", "old"]
    );
    assert_eq!(kept(None, &["--allow-update"]), ["main", "old"]);
    assert_eq!(kept(Some("off"), &["--allow-update"]), ["main", "old"]);
}

/// `sparkles mcp --profile` and `--tools` narrow the tools and the initialize result
/// names the active set, and an unknown profile or tool stops the command with the
/// valid names (item 489).
#[test]
fn stdio_tool_profiles() {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("fixture.ttl");
    std::fs::write(&data, FIXTURE).unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_sparkles"))
        .args([
            "mcp",
            "--name",
            "t",
            "--profile",
            "memory",
            "--tools",
            "find_paths,list_commits",
            "--data",
        ])
        .arg(&data)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    for msg in [
        json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
            "protocolVersion": "2025-11-25", "capabilities": {},
            "clientInfo": {"name": "e2e", "version": "1"}}}),
        json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
        json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}),
    ] {
        writeln!(stdin, "{msg}").unwrap();
    }
    drop(stdin);
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success());
    let lines: Vec<Value> = String::from_utf8(out.stdout)
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    let set = &lines[0]["result"]["capabilities"]["experimental"]["io.sparkles/toolSet"];
    assert_eq!(set["profiles"], json!(["memory"]), "{set}");
    let listed: Vec<&str> = lines[1]["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    let offered = set["tools"].as_array().unwrap();
    for t in &listed {
        assert!(offered.contains(&json!(t)), "{t}: {set}");
    }
    assert!(!offered.contains(&json!("graph_algorithm")), "{set}");
    for t in ["recall", "link_entities", "find_paths", "list_commits"] {
        assert!(listed.contains(&t), "{t}: {listed:?}");
    }
    // without --allow-update the write tools of the profile stay out
    for t in ["explain_query", "graph_algorithm", "assert_facts"] {
        assert!(!listed.contains(&t), "{t}: {listed:?}");
    }

    let refused = |flags: &[&str]| {
        let out = Command::new(env!("CARGO_BIN_EXE_sparkles"))
            .args(["mcp", "--data"])
            .arg(&data)
            .args(flags)
            .stdin(Stdio::null())
            .output()
            .unwrap();
        assert!(!out.status.success(), "{flags:?}");
        String::from_utf8(out.stderr).unwrap()
    };
    let e = refused(&["--profile", "nope"]);
    assert!(
        e.contains("--profile: unknown profile 'nope' (profiles: all, explore, memory, admin)"),
        "{e}"
    );
    let e = refused(&["--mcp-tools", "nope"]);
    assert!(
        e.contains("--tools: unknown tool 'nope' (tools: list_datasets, "),
        "{e}"
    );
    // a proxy to a remote server offers that server's tools, so it takes no profile
    #[cfg(feature = "auth")]
    for flags in [["--profile", "explore"], ["--tools", "find_paths"]] {
        let out = Command::new(env!("CARGO_BIN_EXE_sparkles"))
            .args(["mcp", "--url", "http://127.0.0.1:9/mcp"])
            .args(flags)
            .stdin(Stdio::null())
            .output()
            .unwrap();
        assert!(!out.status.success(), "{flags:?}");
        let e = String::from_utf8(out.stderr).unwrap();
        assert!(e.contains("cannot be used with"), "{flags:?}: {e}");
    }
}

#[test]
fn stdio_ask_model_config_secret_read_only_and_url_refusal() {
    use std::io::Read;
    use std::net::TcpListener;
    use std::sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    };

    let dir = tempfile::tempdir().unwrap();
    let ttl = dir.path().join("fixture.ttl");
    let db = dir.path().join("org");
    std::fs::write(&ttl, FIXTURE).unwrap();

    // Load a persistent database directory via `sparkles load`.
    let loaded = Command::new(env!("CARGO_BIN_EXE_sparkles"))
        .args(["load", "--loc"])
        .arg(&db)
        .arg(&ttl)
        .output()
        .unwrap();
    assert!(
        loaded.status.success(),
        "load failed: {}",
        String::from_utf8_lossy(&loaded.stderr)
    );

    // Enable the assistant with two draft pairs and write a sentinel asks.json.
    std::fs::write(
        db.join("assistant.json"),
        serde_json::to_string_pretty(&json!({
            "format": 1,
            "enabled": true,
            "send": "schema",
            "historyDays": 30,
            "roles": {
                "draft": [
                    { "provider": "mock", "model": "m1" },
                    { "provider": "mock", "model": "m2" }
                ]
            }
        }))
        .unwrap(),
    )
    .unwrap();
    let sentinel_history = serde_json::to_string_pretty(&json!({
        "format": 2,
        "asks": [{
            "id": "keep-me",
            "principal": "local",
            "at": "2026-11-26T00:00:00Z",
            "question": "Sentinel?",
            "result": "answered",
            "outcome": "none",
            "routing": {}
        }]
    }))
    .unwrap();
    let asks_path = db.join("asks.json");
    std::fs::write(&asks_path, &sentinel_history).unwrap();

    // Minimal loopback mock OpenAI server capturing Authorization headers and models.
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    listener.set_nonblocking(true).unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let seen = Arc::new(Mutex::new(Vec::<(Option<String>, String)>::new()));
    let stop_srv = stop.clone();
    let seen_srv = seen.clone();
    let handle = std::thread::spawn(move || {
        while !stop_srv.load(Ordering::Relaxed) {
            match listener.accept() {
                Ok((mut stream, _)) => {
                    stream
                        .set_read_timeout(Some(Duration::from_secs(5)))
                        .unwrap();
                    let mut buf = Vec::new();
                    let mut chunk = [0u8; 4096];
                    let mut header_end = None;
                    let mut content_len = 0usize;
                    while let Ok(n) = stream.read(&mut chunk) {
                        if n == 0 {
                            break;
                        }
                        buf.extend_from_slice(&chunk[..n]);
                        if header_end.is_none()
                            && let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n")
                        {
                            header_end = Some(pos + 4);
                            let hdrs = String::from_utf8_lossy(&buf[..pos]);
                            for line in hdrs.lines() {
                                if let Some((k, v)) = line.split_once(':')
                                    && k.eq_ignore_ascii_case("content-length")
                                {
                                    content_len = v.trim().parse().unwrap_or(0);
                                }
                            }
                        }
                        if let Some(end) = header_end
                            && buf.len() >= end + content_len
                        {
                            break;
                        }
                    }
                    let end = header_end.unwrap_or(buf.len());
                    let hdrs = String::from_utf8_lossy(&buf[..end]);
                    let auth = hdrs.lines().find_map(|line| {
                        let (k, v) = line.split_once(':')?;
                        k.eq_ignore_ascii_case("authorization")
                            .then(|| v.trim().to_string())
                    });
                    let req_body: Value = serde_json::from_slice(&buf[end..]).unwrap_or_default();
                    let model = req_body["model"].as_str().unwrap_or("").to_string();
                    seen_srv.lock().unwrap().push((auth, model));

                    let content = json!({
                        "query": "SELECT ?p ?name WHERE { ?p a ex:Person ; rdfs:label ?name } ORDER BY ?p",
                        "explanation": "Lists people and labels.",
                        "assumptions": [],
                        "clarify": { "question": "", "choices": [] },
                        "graph": { "subject": "", "predicate": "", "object": "" }
                    })
                    .to_string();
                    let body = json!({
                        "choices": [{
                            "message": { "content": content },
                            "finish_reason": "stop"
                        }],
                        "usage": { "prompt_tokens": 12, "completion_tokens": 8 }
                    })
                    .to_string();
                    let resp = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        body.len(),
                        body
                    );
                    let _ = stream.write_all(resp.as_bytes());
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(_) => break,
            }
        }
    });

    let secret_file = dir.path().join("api.key");
    std::fs::write(&secret_file, "sk-stdio-secret-488\n").unwrap();
    let model_cfg = dir.path().join("models.json");
    std::fs::write(
        &model_cfg,
        serde_json::to_string_pretty(&json!({
            "providers": {
                "mock": {
                    "kind": "openai",
                    "endpoint": format!("http://{addr}"),
                    "apiKey": { "secret": "mock_key" },
                    "requestTimeoutSecs": 5,
                    "allowedModels": ["m1", "m2"]
                }
            },
            "roles": {
                "draft": [
                    { "provider": "mock", "model": "m1" },
                    { "provider": "mock", "model": "m2" }
                ]
            }
        }))
        .unwrap(),
    )
    .unwrap();

    // 1. `--url` refuses `--model-config` and `--model-secret` before connecting.
    #[cfg(feature = "auth")]
    {
        let out = Command::new(env!("CARGO_BIN_EXE_sparkles"))
            .args(["mcp", "--url", "http://127.0.0.1:9/mcp", "--model-config"])
            .arg(&model_cfg)
            .stdin(Stdio::null())
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(1));
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            stderr.contains("--loc") && stderr.contains("bridged server"),
            "unexpected stderr: {stderr}"
        );
    }

    // 2. Default read-only `sparkles mcp --loc` loads `--model-config` and `--model-secret`,
    //    lists `ask`, answers an ordinary call and `tryHarder`, and never writes `asks.json`.
    let mut child = Command::new(env!("CARGO_BIN_EXE_sparkles"))
        .args(["mcp", "--loc"])
        .arg(&db)
        .arg("--model-config")
        .arg(&model_cfg)
        .arg("--model-secret")
        .arg(format!("mock_key=file:{}", secret_file.display()))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let stdout = child.stdout.take().unwrap();
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
            .expect("no stdio response");
        Some(serde_json::from_str(&line).expect("JSON-RPC line"))
    };
    let meta = json!({
        "io.modelcontextprotocol/protocolVersion": "2026-07-28",
        "io.modelcontextprotocol/clientCapabilities": {}
    });

    let list = exchange(json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/list",
        "params": { "_meta": meta }
    }))
    .unwrap();
    let names: Vec<&str> = list["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|t| t["name"].as_str())
        .collect();
    assert!(
        names.contains(&"ask"),
        "ask missing from tools/list: {names:?}"
    );

    let first = exchange(json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "tools/call",
        "params": {
            "_meta": meta,
            "name": "ask",
            "arguments": { "question": "List people" }
        }
    }))
    .unwrap();
    assert_eq!(first["result"]["isError"], false, "{first}");
    let sc1 = &first["result"]["structuredContent"];
    assert_eq!(sc1["outcome"], "answered", "{sc1}");
    assert_eq!(sc1["usage"]["answeredBy"]["model"], "m1");
    let ask_id = sc1["usage"]["askId"].as_str().unwrap().to_string();

    // `tryHarder` escalates to m2 using in-memory recent state without mutating asks.json.
    let second = exchange(json!({
        "jsonrpc": "2.0",
        "id": 3,
        "method": "tools/call",
        "params": {
            "_meta": meta,
            "name": "ask",
            "arguments": { "question": "List people", "tryHarder": ask_id }
        }
    }))
    .unwrap();
    assert_eq!(second["result"]["isError"], false, "{second}");
    let sc2 = &second["result"]["structuredContent"];
    assert_eq!(sc2["outcome"], "answered", "{sc2}");
    assert_eq!(sc2["usage"]["answeredBy"]["model"], "m2");

    drop(stdin);
    let status = child.wait().unwrap();
    stop.store(true, Ordering::Relaxed);
    let _ = handle.join();
    assert!(status.success(), "{status}");

    let recorded = seen.lock().unwrap();
    assert_eq!(recorded.len(), 2, "{recorded:?}");
    assert_eq!(
        recorded[0],
        (Some("Bearer sk-stdio-secret-488".into()), "m1".into())
    );
    assert_eq!(
        recorded[1],
        (Some("Bearer sk-stdio-secret-488".into()), "m2".into())
    );

    let after_history = std::fs::read_to_string(&asks_path).unwrap();
    assert_eq!(
        after_history, sentinel_history,
        "read-only `sparkles mcp --loc` must not modify asks.json on disk"
    );
}
