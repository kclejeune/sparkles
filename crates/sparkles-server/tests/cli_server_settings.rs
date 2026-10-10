//! Server-wide model settings and runtime secrets (spec C19 §11) with a server process:
//! a runtime layer and a runtime key that survive a restart and a SIGHUP that reads a
//! changed `--model-config`, `sparkles settings check` with `server.locked`, and a log
//! at TRACE that carries the audit records and never the key (A12).

use serde_json::{Value as J, json};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const BIN: &str = env!("CARGO_BIN_EXE_sparkles");
const KEY: &str = "runtime-key-c19-a12-77";
const DECLARED_KEY: &str = "declared-key-c19-11";

/// A server started by the test, stopped when dropped.
struct Server {
    child: Child,
    port: u16,
}

impl Server {
    /// Start on the first free port of 47400–47419, with its log in `log`.
    fn start(data: &Path, extra: &[&str], log: &Path) -> Server {
        let port = (47400..47420)
            .find(|p| TcpListener::bind(("127.0.0.1", *p)).is_ok())
            .expect("a free port in 47400-47419");
        let log = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(log)
            .unwrap();
        let child = Command::new(BIN)
            .args(["serve", "--host", "127.0.0.1", "--port", &port.to_string()])
            .arg("--data")
            .arg(data)
            .args(extra)
            .env_remove("SPARKLES_BACKUP_CONFIG")
            .env("RUST_LOG", "trace")
            .stdout(Stdio::null())
            .stderr(log)
            .spawn()
            .unwrap();
        let s = Server { child, port };
        let t0 = Instant::now();
        while s.call("GET", "/$/ping", None).0 != 200 {
            assert!(
                t0.elapsed() < Duration::from_secs(30),
                "server did not start"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
        s
    }

    fn call(&self, method: &str, path: &str, body: Option<J>) -> (u16, J) {
        let Ok(mut c) = std::net::TcpStream::connect(("127.0.0.1", self.port)) else {
            return (0, J::Null);
        };
        let body = body.map(|b| b.to_string()).unwrap_or_default();
        let _ = write!(
            c,
            "{method} {path} HTTP/1.0\r\nHost: 127.0.0.1\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        );
        let mut buf = String::new();
        if c.read_to_string(&mut buf).is_err() {
            return (0, J::Null);
        }
        let Some((head, body)) = buf.split_once("\r\n\r\n") else {
            return (0, J::Null);
        };
        let status = head
            .split(' ')
            .nth(1)
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        (status, serde_json::from_str(body).unwrap_or(J::Null))
    }

    fn hup(&self) {
        assert_eq!(
            unsafe { libc::kill(self.child.id() as i32, libc::SIGHUP) },
            0
        );
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn eventually(f: impl Fn() -> bool) {
    let t0 = Instant::now();
    while !f() {
        assert!(t0.elapsed() < Duration::from_secs(10), "timed out");
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// An OpenAI-protocol endpoint on a loopback port that answers every request and keeps
/// the `Authorization` header of each.
fn mock() -> (u16, Arc<Mutex<Vec<String>>>) {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    let seen: Arc<Mutex<Vec<String>>> = Arc::default();
    let s = seen.clone();
    std::thread::spawn(move || {
        for c in l.incoming().flatten() {
            let mut r = BufReader::new(c.try_clone().unwrap());
            let mut auth = String::new();
            let mut len = 0usize;
            loop {
                let mut line = String::new();
                if r.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
                    break;
                }
                let lower = line.to_ascii_lowercase();
                if let Some(v) = lower.strip_prefix("content-length:") {
                    len = v.trim().parse().unwrap_or(0);
                }
                if lower.starts_with("authorization:") {
                    auth = line["authorization:".len()..].trim().to_string();
                }
            }
            let mut body = vec![0; len];
            let _ = r.read_exact(&mut body);
            s.lock().unwrap().push(auth);
            let out = json!({
                "choices": [{ "message": { "role": "assistant", "content": "{\"answer\": \"ok\"}" }, "finish_reason": "stop" }],
                "usage": { "prompt_tokens": 10, "completion_tokens": 5 }
            })
            .to_string();
            let mut c = c;
            let _ = write!(
                c,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{out}",
                out.len()
            );
        }
    });
    (port, seen)
}

fn models(mock: u16, extra: bool) -> J {
    let mut providers = json!({
        "gw": {
            "kind": "openai", "endpoint": format!("http://127.0.0.1:{mock}/v1"),
            "apiKey": { "secret": "gw" }, "models": { "m": { "contextTokens": 4096 } }
        }
    });
    if extra {
        providers["second"] = json!({ "kind": "ollama", "endpoint": "http://127.0.0.1:9" });
    }
    json!({ "models": { "providers": providers, "roles": { "draft": [{ "provider": "gw", "model": "m" }] } } })
}

#[test]
fn server_settings_restart_reload_and_log() {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("data");
    let cfg = dir.path().join("models.json");
    let settings = dir.path().join("settings.json");
    let key_file = dir.path().join("gw.key");
    let log = dir.path().join("server.log");
    let (mock_port, seen) = mock();
    std::fs::write(&cfg, models(mock_port, false).to_string()).unwrap();
    std::fs::write(&key_file, DECLARED_KEY).unwrap();
    let check = |locked: J| {
        std::fs::write(
            &settings,
            json!({ "server": { "locked": locked } }).to_string(),
        )
        .unwrap();
        Command::new(BIN)
            .args(["settings", "check"])
            .arg(&settings)
            .arg("--model-config")
            .arg(&cfg)
            .output()
            .unwrap()
    };
    // `settings check` with `server.locked`: a field a provider lacks fails, a provider
    // the configuration lacks is a warning
    let out = check(json!(["models.providers.gw.endpont"]));
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("no member"));
    let out = check(json!(["models.providers.nope.endpoint", "secrets.gw"]));
    assert!(out.status.success(), "{out:?}");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("warning") && err.contains("\"nope\""), "{err}");
    let out = check(json!(["models.providers.gw.endpoint"]));
    assert!(out.status.success(), "{out:?}");

    let secret = format!("gw=file:{}", key_file.display());
    let args = [
        "--model-config",
        cfg.to_str().unwrap(),
        "--model-secret",
        &secret,
        "--settings",
        settings.to_str().unwrap(),
        "--outbound-allow-private",
    ];
    let bearer = || seen.lock().unwrap().last().cloned().unwrap_or_default();
    let test = |s: &Server| {
        let (st, v) = s.call("POST", "/$/models/gw/test", Some(json!({ "model": "m" })));
        assert_eq!(st, 200, "{v}");
        assert_eq!(v["ok"], true, "{v}");
    };

    let s = Server::start(&data, &args, &log);
    test(&s);
    assert_eq!(bearer(), format!("Bearer {DECLARED_KEY}"));
    // A13: the locked endpoint, then a role list and a budget
    let (st, v) = s.call(
        "PATCH",
        "/$/server/settings/models",
        Some(json!({ "providers": { "gw": { "endpoint": "http://127.0.0.1:1/v1" } } })),
    );
    assert_eq!(st, 409, "{v}");
    let (st, v) = s.call(
        "PATCH",
        "/$/server/settings/models",
        Some(json!({
            "providers": { "gw": { "budget": { "tokensPerDay": 100000 } } },
            "roles": { "summarize": [{ "provider": "gw", "model": "m" }] }
        })),
    );
    assert_eq!(st, 200, "{v}");
    let (st, _) = s.call("PUT", "/$/server/secrets/gw", Some(json!({ "value": KEY })));
    assert_eq!(st, 204);
    test(&s);
    assert_eq!(bearer(), format!("Bearer {KEY}"));
    drop(s);

    // a restart keeps the runtime layer and the key
    let s = Server::start(&data, &args, &log);
    let (_, v) = s.call("GET", "/$/models", None);
    assert_eq!(v["roles"]["summarize"][0]["provider"], "gw", "{v}");
    assert_eq!(v["providers"][0]["apiKey"]["source"], "runtime", "{v}");
    test(&s);
    assert_eq!(bearer(), format!("Bearer {KEY}"));
    // SIGHUP reads a changed --model-config under the runtime layer
    std::fs::write(&cfg, models(mock_port, true).to_string()).unwrap();
    s.hup();
    eventually(|| {
        let (_, v) = s.call("GET", "/$/models", None);
        v["providers"]
            .as_array()
            .is_some_and(|p| p.iter().any(|p| p["name"] == "second"))
    });
    let (_, v) = s.call("GET", "/$/server/settings/models", None);
    assert_eq!(
        v["effective"]["roles"]["summarize"][0]["provider"], "gw",
        "{v}"
    );
    assert_eq!(v["sources"]["providers.second.kind"], "declared", "{v}");
    assert_eq!(v["sources"]["providers.gw.endpoint"], "locked", "{v}");
    // DELETE brings back the declared file
    let (st, _) = s.call("DELETE", "/$/server/secrets/gw", None);
    assert_eq!(st, 204);
    test(&s);
    assert_eq!(bearer(), format!("Bearer {DECLARED_KEY}"));
    let (_, v) = s.call("GET", "/$/server/secrets", None);
    assert_eq!(v["secrets"][0]["source"], "declared", "{v}");
    drop(s);

    // A12: the log has the audit records and never the key
    let text = std::fs::read_to_string(&log).unwrap();
    assert!(!text.contains(KEY), "the key is in the log");
    assert!(
        !text.contains(DECLARED_KEY),
        "the declared key is in the log"
    );
    for event in ["server_settings_changed", "secret_set", "secret_removed"] {
        assert!(text.contains(event), "no {event} in the log");
    }
    assert!(
        text.contains("roles.summarize"),
        "the changed fields are not logged"
    );
}
