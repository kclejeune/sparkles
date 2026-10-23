//! `sparkles graphrag --loc` against a stub OpenAI-compatible provider on a local port:
//! a build without confirmation sends nothing, a confirmed build writes the reports, and
//! `status` and `reports` read them back (spec C22 §2.2).

use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::process::{Command, Output, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

const BIN: &str = env!("CARGO_BIN_EXE_sparkles");
const ORG: &str = "urn:g:org";

/// A provider that answers every chat request with a report whose one finding cites
/// item 1 and the entity that item names.
fn provider(requests: Arc<AtomicUsize>) -> u16 {
    let l = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = l.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for s in l.incoming().flatten() {
            let n = requests.clone();
            std::thread::spawn(move || handle(s, &n));
        }
    });
    port
}

fn handle(s: TcpStream, requests: &AtomicUsize) {
    let mut r = BufReader::new(s.try_clone().unwrap());
    let mut line = String::new();
    if r.read_line(&mut line).is_err() {
        return;
    }
    let mut len = 0usize;
    loop {
        let mut h = String::new();
        if r.read_line(&mut h).is_err() || h == "\r\n" || h.is_empty() {
            break;
        }
        if let Some(v) = h.to_ascii_lowercase().strip_prefix("content-length:") {
            len = v.trim().parse().unwrap_or(0);
        }
    }
    let mut body = vec![0; len];
    let _ = r.read_exact(&mut body);
    requests.fetch_add(1, Ordering::SeqCst);
    let req: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let prompt = req["messages"]
        .as_array()
        .and_then(|m| m.last())
        .and_then(|m| m["content"].as_str())
        .unwrap_or("")
        .to_string();
    let first = prompt
        .lines()
        .find_map(|l| l.strip_prefix("[1] "))
        .unwrap_or("");
    let subject = first.split(' ').next().unwrap_or("");
    let entity = prompt
        .lines()
        .filter_map(|l| l.strip_prefix('E'))
        .find_map(|l| {
            let (n, rest) = l.split_once(' ')?;
            (rest.split(' ').next() == Some(subject)).then(|| n.parse::<u64>().ok())?
        })
        .unwrap_or(1);
    let report = json!({
        "title": "A team",
        "summary": "People who know each other.",
        "findings": [{ "text": "They know each other.", "entities": [entity], "cites": [1] }],
        "rating": 5
    });
    let answer = json!({
        "choices": [{ "message": { "role": "assistant", "content": report.to_string() }, "finish_reason": "stop" }],
        "usage": { "prompt_tokens": 10, "completion_tokens": 5 }
    })
    .to_string();
    let mut w = s;
    let _ = write!(
        w,
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{answer}",
        answer.len()
    );
}

fn data() -> String {
    let mut t = format!("<{ORG}> {{\n");
    for c in ["x", "y"] {
        for i in 0..4 {
            t.push_str(&format!(
                "<http://ex/{c}{i}> <http://www.w3.org/2000/01/rdf-schema#label> \"{c} person {i}\" .\n"
            ));
            for j in (i + 1)..4 {
                t.push_str(&format!(
                    "<http://ex/{c}{i}> <http://ex/knows> <http://ex/{c}{j}> .\n"
                ));
            }
        }
    }
    t.push_str("<http://ex/x0> <http://ex/knows> <http://ex/y0> .\n}\n");
    t
}

fn graphrag(db: &Path, cfg: &Path, args: &[&str]) -> Output {
    Command::new(BIN)
        .arg("graphrag")
        .arg("--model-config")
        .arg(cfg)
        .arg("--loc")
        .arg(db)
        .args(["--dataset", "org", "--json"])
        .args(args)
        .stdin(Stdio::null())
        .output()
        .unwrap()
}

fn json_of(o: &Output) -> Value {
    serde_json::from_slice(&o.stdout).unwrap_or_else(|_| {
        panic!(
            "not JSON: {}\n{}",
            String::from_utf8_lossy(&o.stdout),
            String::from_utf8_lossy(&o.stderr)
        )
    })
}

#[test]
fn a_local_build_writes_reports_that_status_and_reports_read() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("db");
    let trig = dir.path().join("data.trig");
    std::fs::write(&trig, data()).unwrap();
    let o = Command::new(BIN)
        .args(["load", "--loc"])
        .arg(&db)
        .arg(&trig)
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    std::fs::write(
        db.join("graphrag.json"),
        json!({
            "format": 1,
            "enabled": true,
            "principal": "reports",
            "projection": { "graphs": [ORG] },
            "levels": 1,
            "minCommunitySize": 2
        })
        .to_string(),
    )
    .unwrap();
    std::fs::write(
        db.join("assistant.json"),
        json!({ "format": 1, "send": "rows" }).to_string(),
    )
    .unwrap();
    let requests = Arc::new(AtomicUsize::new(0));
    let port = provider(requests.clone());
    let cfg = dir.path().join("models.json");
    std::fs::write(
        &cfg,
        json!({
            "providers": { "p": { "kind": "openai", "endpoint": format!("http://127.0.0.1:{port}/v1"),
                                  "requestTimeoutSecs": 5, "allowedModels": ["m"],
                                  "structuredOutput": "json-schema" } },
            "roles": { "community": [{ "provider": "p", "model": "m" }] }
        })
        .to_string(),
    )
    .unwrap();

    // without a terminal and without --confirm, the estimate is refused
    let o = graphrag(&db, &cfg, &["build"]);
    assert!(!o.status.success());
    let stderr = String::from_utf8_lossy(&o.stderr);
    assert!(stderr.contains("estimate"), "{stderr}");
    assert!(stderr.contains("--confirm"), "{stderr}");
    assert_eq!(requests.load(Ordering::SeqCst), 0);

    let o = graphrag(&db, &cfg, &["build", "--confirm"]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let task = json_of(&o);
    assert_eq!(task["state"], "done", "{task:#}");
    assert_eq!(task["detail"]["written"], 2, "{task:#}");
    assert_eq!(requests.load(Ordering::SeqCst), 2);

    let st = json_of(&graphrag(&db, &cfg, &["status"]));
    assert_eq!(st["reports"]["unreviewed"], 2, "{st:#}");
    assert_eq!(st["levels"][0]["communities"], 2, "{st:#}");

    let r = json_of(&graphrag(&db, &cfg, &["reports", "--status", "unreviewed"]));
    let cs = r["communities"].as_array().unwrap();
    assert_eq!(cs.len(), 2, "{r:#}");
    assert_eq!(cs[0]["report"]["title"], "A team");

    // a refresh with nothing changed sends nothing
    let o = graphrag(&db, &cfg, &["refresh", "--confirm"]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    assert_eq!(json_of(&o)["detail"]["written"], 0);
    assert_eq!(requests.load(Ordering::SeqCst), 2);
}
