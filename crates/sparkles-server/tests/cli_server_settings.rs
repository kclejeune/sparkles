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
        Server::start_in(data, extra, log, 47400..47420)
    }

    /// Start on the first free port of `ports`, with its log in `log`.
    fn start_in(data: &Path, extra: &[&str], log: &Path, ports: std::ops::Range<u16>) -> Server {
        let port = ports
            .clone()
            .find(|p| TcpListener::bind(("127.0.0.1", *p)).is_ok())
            .unwrap_or_else(|| panic!("a free port in {ports:?}"));
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

/// The output of a CLI run.
struct Out {
    code: Option<i32>,
    stdout: String,
    stderr: String,
}

impl Out {
    fn ok(self) -> Out {
        assert_eq!(self.code, Some(0), "{}\n{}", self.stdout, self.stderr);
        self
    }

    fn json(&self) -> J {
        serde_json::from_str(&self.stdout)
            .unwrap_or_else(|e| panic!("{e}: {}\n{}", self.stdout, self.stderr))
    }
}

/// Run the CLI against `s` with `stdin` piped in, so that it is not a terminal.
fn cli(s: &Server, home: &Path, args: &[&str], stdin: &str) -> Out {
    let mut c = Command::new(BIN);
    c.args(args)
        .env("HOME", home)
        .env("XDG_CONFIG_HOME", home.join("config"))
        .env("XDG_STATE_HOME", home.join("state"))
        .env("SPARKLES_SERVER", format!("http://127.0.0.1:{}", s.port))
        .env_remove("SPARKLES_TOKEN")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = c.spawn().unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(stdin.as_bytes())
        .unwrap();
    let o = child.wait_with_output().unwrap();
    Out {
        code: o.status.code(),
        stdout: String::from_utf8_lossy(&o.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&o.stderr).into_owned(),
    }
}

/// The line of `out` whose first word is `field`.
fn line(out: &str, field: &str) -> String {
    out.lines()
        .find(|l| l.split_whitespace().next() == Some(field))
        .unwrap_or_else(|| panic!("{field} in {out}"))
        .to_string()
}

/// `sparkles settings --global` on the `models` kind (a locked endpoint, a budget, a
/// role list that overrides the declared one, `diff` and `reset`) and `sparkles secrets
/// list|set|unset`, with a check that the key reaches neither the CLI's output nor the
/// server's log.
#[test]
fn global_settings_and_secrets_commands() {
    const CLI_KEY: &str = "cli-runtime-key-c19-4321";
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    let data = dir.path().join("data");
    let cfg = dir.path().join("models.json");
    let settings = dir.path().join("settings.json");
    let key_file = dir.path().join("gw.key");
    let log = dir.path().join("server.log");
    let (mock_port, seen) = mock();
    std::fs::write(&cfg, models(mock_port, false).to_string()).unwrap();
    std::fs::write(&key_file, DECLARED_KEY).unwrap();
    std::fs::write(
        &settings,
        json!({ "server": { "locked": ["models.providers.gw.endpoint"] } }).to_string(),
    )
    .unwrap();
    let secret = format!("gw=file:{}", key_file.display());
    let s = Server::start_in(
        &data,
        &[
            "--model-config",
            cfg.to_str().unwrap(),
            "--model-secret",
            &secret,
            "--settings",
            settings.to_str().unwrap(),
            "--outbound-allow-private",
        ],
        &log,
        47600..47620,
    );
    let mut outputs: Vec<String> = Vec::new();
    let mut run = |args: &[&str], stdin: &str| {
        let o = cli(&s, &home, args, stdin);
        outputs.push(format!("{}\n{}", o.stdout, o.stderr));
        o
    };

    // get --global: every field with its source, the lock among them
    let o = run(&["settings", "get", "--global"], "").ok();
    assert!(o.stdout.starts_with("models on the server"), "{}", o.stdout);
    assert!(line(&o.stdout, "providers.gw.endpoint").contains("locked"));
    assert!(line(&o.stdout, "providers.gw.kind").contains("declared"));
    let j = run(&["settings", "get", "--global", "models", "--json"], "")
        .ok()
        .json();
    assert_eq!(j["scope"], "server");
    assert_eq!(j["overrides"], json!([]));

    // set --global: a budget and a role list; the locked endpoint is refused
    let o = run(
        &[
            "settings",
            "set",
            "--global",
            "models.providers.gw.budget.tokensPerDay=5000",
            r#"models.roles.draft=[{"provider":"gw","model":"m2"}]"#,
        ],
        "",
    )
    .ok();
    assert!(
        o.stdout
            .contains("models.providers.gw.budget.tokensPerDay = 5000  (runtime)"),
        "{}",
        o.stdout
    );
    let o = run(
        &[
            "settings",
            "set",
            "--global",
            "models.providers.gw.endpoint=http://127.0.0.1:1/v1",
        ],
        "",
    );
    assert_eq!(o.code, Some(1));
    assert!(
        o.stderr.contains("locks providers.gw.endpoint"),
        "{}",
        o.stderr
    );
    // the role list now overrides the declared one, and get says so
    let o = run(&["settings", "get", "--global", "models"], "").ok();
    let draft = line(&o.stdout, "roles.draft");
    assert!(
        draft.contains(r#"runtime, overrides declared [{"model":"m","provider":"gw"}]"#),
        "{draft}"
    );
    let o = run(&["settings", "get", "--global", "--layer", "runtime"], "").ok();
    assert!(
        line(&o.stdout, "roles.draft").contains("(overrides declared"),
        "{}",
        o.stdout
    );
    // diff --global lists both changes
    let o = run(&["settings", "diff", "--global"], "").ok();
    assert!(
        o.stdout.contains(
            r#"server models.roles.draft: runtime [{"model":"m2","provider":"gw"}], declared [{"model":"m","provider":"gw"}]"#
        ),
        "{}",
        o.stdout
    );
    assert!(
        o.stdout.contains(
            "server models.providers.gw.budget.tokensPerDay: runtime 5000, default unset"
        ),
        "{}",
        o.stdout
    );
    // reset --global says which value applies now
    let o = run(&["settings", "reset", "--global", "models.roles.draft"], "").ok();
    assert!(
        o.stdout.contains(
            r#"models.roles.draft on the server = [{"model":"m","provider":"gw"}], the declared value applies"#
        ),
        "{}",
        o.stdout
    );
    let o = run(
        &[
            "settings",
            "reset",
            "--global",
            "models.providers.gw.budget.tokensPerDay",
        ],
        "",
    )
    .ok();
    assert!(
        o.stdout.contains("= unset, the default applies"),
        "{}",
        o.stdout
    );
    // the dataset form names --global for a server-wide kind
    let o = run(&["settings", "get", "models"], "");
    assert_eq!(o.code, Some(1));
    assert!(o.stderr.contains("--global"), "{}", o.stderr);

    // secrets: list, set from standard input, unset
    let o = run(&["secrets", "list"], "").ok();
    let gw = line(&o.stdout, "gw");
    assert!(gw.contains("declared"), "{}", o.stdout);
    let o = run(&["secrets", "set", "gw"], &format!("{CLI_KEY}\n")).ok();
    assert!(
        o.stdout.contains("stored a runtime value for secret gw"),
        "{}",
        o.stdout
    );
    assert!(
        o.stdout
            .contains("the runtime value applies, in place of the declared source; used by gw"),
        "{}",
        o.stdout
    );
    let (st, v) = s.call("POST", "/$/models/gw/test", Some(json!({ "model": "m" })));
    assert_eq!(st, 200, "{v}");
    assert_eq!(
        seen.lock().unwrap().last().cloned().unwrap_or_default(),
        format!("Bearer {CLI_KEY}")
    );
    let j = run(&["secrets", "list", "--json"], "").ok().json();
    let gw = &j["secrets"][0];
    assert_eq!(gw["source"], "runtime", "{j}");
    assert!(gw["setAt"].is_string(), "{j}");
    assert_eq!(gw["overridden"], false);
    assert_eq!(gw["providers"], json!(["gw"]));
    let o = run(&["secrets", "list"], "").ok();
    assert!(
        line(&o.stdout, "gw").contains("overrides the declared source"),
        "{}",
        o.stdout
    );
    // an empty value and a bad name are refused before anything is sent
    let o = run(&["secrets", "set", "gw"], "\n");
    assert_eq!(o.code, Some(1));
    assert!(o.stderr.contains("no value"), "{}", o.stderr);
    let o = run(&["secrets", "set", ".bad"], "x\n");
    assert_eq!(o.code, Some(1));
    let o = run(&["secrets", "unset", "gw"], "").ok();
    assert!(
        o.stdout.contains("the declared source applies; used by gw"),
        "{}",
        o.stdout
    );
    let (_, v) = s.call("GET", "/$/server/secrets", None);
    assert_eq!(v["secrets"][0]["source"], "declared", "{v}");
    drop(s);

    // the key is in no output of the CLI and not in the server's log
    for out in &outputs {
        assert!(
            !out.contains(CLI_KEY),
            "the key is in the CLI's output: {out}"
        );
    }
    let text = std::fs::read_to_string(&log).unwrap();
    assert!(!text.contains(CLI_KEY), "the key is in the server's log");
    assert!(text.contains("secret_set"), "no secret_set in the log");
}

/// `sparkles settings set --global --preset NAME PROVIDER [FIELD=VALUE…]` adds a
/// provider from a preset with the assignments in place of the preset's fields, refuses
/// a provider that exists and an unknown preset, and a provider with certificate checks
/// off is reported as unverified and logged with a warning (spec C19 §11.5, §11.6).
#[test]
fn provider_presets_and_insecure_warning() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    let cfg = dir.path().join("models.json");
    let log = dir.path().join("server.log");
    std::fs::write(&cfg, models(9, false).to_string()).unwrap();
    let s = Server::start_in(
        &dir.path().join("data"),
        &["--model-config", cfg.to_str().unwrap()],
        &log,
        47800..47820,
    );
    let run = |args: &[&str]| cli(&s, &home, args, "");
    let provider = |name: &str| {
        let (_, v) = s.call("GET", "/$/server/settings/models", None);
        v["effective"]["providers"][name].clone()
    };

    let o = run(&[
        "settings",
        "set",
        "--global",
        "--preset",
        "anthropic",
        "claude",
    ])
    .ok();
    assert!(
        o.stdout
            .contains("added provider claude from preset anthropic"),
        "{}",
        o.stdout
    );
    assert!(
        o.stdout.contains("sparkles secrets set anthropic"),
        "{}",
        o.stdout
    );
    assert_eq!(
        provider("claude"),
        json!({
            "kind": "anthropic", "endpoint": "https://api.anthropic.com",
            "apiKey": { "secret": "anthropic" }, "models": { "claude-sonnet-5-5": {} }
        })
    );
    // the assignments replace the preset's fields
    run(&[
        "settings",
        "set",
        "--global",
        "--preset",
        "openai",
        "oai",
        "endpoint=https://proxy.example/v1",
        "apiKey.secret=team-key",
        r#"models={"gpt-x":{}}"#,
    ])
    .ok();
    assert_eq!(
        provider("oai"),
        json!({
            "kind": "openai", "endpoint": "https://proxy.example/v1",
            "apiKey": { "secret": "team-key" }, "models": { "gpt-x": {} }
        })
    );
    let j = run(&[
        "settings", "set", "--global", "--preset", "ollama", "local", "--json",
    ])
    .ok()
    .json();
    assert_eq!(
        j["kinds"]["models"]["effective"]["providers"]["local"]["endpoint"],
        "http://127.0.0.1:11434"
    );
    assert!(
        j["kinds"]["models"]["effective"]["providers"]["local"]
            .get("apiKey")
            .is_none()
    );

    // refusals: a provider that exists, an unknown preset, tls on http, no --global
    let o = run(&[
        "settings",
        "set",
        "--global",
        "--preset",
        "anthropic",
        "claude",
    ]);
    assert_eq!(o.code, Some(1));
    assert!(
        o.stderr.contains("a provider named claude exists"),
        "{}",
        o.stderr
    );
    let o = run(&["settings", "set", "--global", "--preset", "gateway", "gw2"]);
    assert_eq!(o.code, Some(1));
    assert!(o.stderr.contains("no preset named"), "{}", o.stderr);
    let o = run(&[
        "settings",
        "set",
        "--global",
        "--preset",
        "ollama",
        "local2",
        "tls.insecureSkipVerify=true",
    ]);
    assert_eq!(o.code, Some(1));
    assert!(o.stderr.contains("https endpoints only"), "{}", o.stderr);
    assert_eq!(provider("local2"), J::Null);
    let o = run(&["settings", "set", "--preset", "ollama", "x"]);
    assert_eq!(o.code, Some(2), "{}", o.stderr);

    // certificate checks off: unverified in GET /$/models, and a warning in the log
    run(&[
        "settings",
        "set",
        "--global",
        "models.providers.oai.tls.insecureSkipVerify=true",
    ])
    .ok();
    let (_, v) = s.call("GET", "/$/models", None);
    let oai = v["providers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["name"] == "oai")
        .cloned()
        .unwrap();
    assert_eq!(oai["unverified"], true, "{v}");
    assert_eq!(oai["tls"]["verification"], "off", "{v}");
    let text = std::fs::read_to_string(&log).unwrap();
    assert!(
        text.contains("provider oai: tls.insecureSkipVerify is on"),
        "no warning in the log"
    );
}
