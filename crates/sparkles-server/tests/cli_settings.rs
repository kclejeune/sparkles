//! Layered dataset settings (spec C19) with a server process: `serve --settings`, a
//! reload on SIGHUP, a dataset of `--loc`, and `sparkles settings check`.

use serde_json::{Value as J, json};
use std::io::{Read, Write};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const BIN: &str = env!("CARGO_BIN_EXE_sparkles");

/// A server started by the test, stopped when dropped.
struct Server {
    child: Child,
    port: u16,
}

impl Server {
    /// Start on the first free port of 47130–47149.
    fn start(data: &Path, extra: &[&str]) -> Server {
        Server::start_in(data, extra, 47130..47150)
    }

    /// Start on the first free port of `ports`.
    fn start_in(data: &Path, extra: &[&str], ports: std::ops::Range<u16>) -> Server {
        let port = ports
            .clone()
            .find(|p| std::net::TcpListener::bind(("127.0.0.1", *p)).is_ok())
            .unwrap_or_else(|| panic!("a free port in {ports:?}"));
        let child = Command::new(BIN)
            .args(["serve", "--host", "127.0.0.1", "--port", &port.to_string()])
            .arg("--data")
            .arg(data)
            .args(extra)
            .env_remove("SPARKLES_BACKUP_CONFIG")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
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

/// Wait until `f` holds, for a reload that runs after the signal.
fn eventually(f: impl Fn() -> bool) {
    let t0 = Instant::now();
    while !f() {
        assert!(t0.elapsed() < Duration::from_secs(10), "timed out");
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// A2 with a restart and SIGHUP, A4 at the start and on a reload, and A9.
#[test]
fn settings_file_restart_reload_and_declared_datasets() {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("data");
    let loc = dir.path().join("demo");
    let f = dir.path().join("settings.json");
    let f_arg = f.to_str().unwrap().to_string();
    let loc_arg = format!("demo={}", loc.display());

    // A4: a file that does not validate fails the start and `settings check`
    std::fs::write(
        &f,
        json!({"datasets": {"demo": {"assistant": {"send": "everything"}}}}).to_string(),
    )
    .unwrap();
    let out = Command::new(BIN)
        .args(["settings", "check", &f_arg])
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("everything"));
    let out = Command::new(BIN)
        .args([
            "serve",
            "--host",
            "127.0.0.1",
            "--port",
            "47149",
            "--settings",
            &f_arg,
        ])
        .arg("--data")
        .arg(&data)
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("everything"));

    std::fs::write(
        &f,
        json!({"datasets": {"demo": {"assistant": {"historyDays": 30}}}}).to_string(),
    )
    .unwrap();
    let out = Command::new(BIN)
        .args(["settings", "check", &f_arg])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let args = ["--settings", f_arg.as_str(), "--loc", loc_arg.as_str()];
    {
        let s = Server::start(&data, &args);
        let (st, v) = s.call(
            "PATCH",
            "/$/settings/demo/assistant",
            Some(json!({"historyDays": 7})),
        );
        assert_eq!(st, 200, "{v}");
        // A9
        let (st, v) = s.call("DELETE", "/$/datasets/demo", None);
        assert_eq!((st, v["code"].as_str()), (409, Some("declared-dataset")));
        let (_, v) = s.call("GET", "/$/datasets/demo", None);
        assert_eq!(v["declared"], true);
    }
    assert!(loc.join("assistant.json").exists());
    // a restart keeps the runtime value
    let s = Server::start(&data, &args);
    let (_, v) = s.call("GET", "/$/settings/demo/assistant", None);
    assert_eq!(v["effective"]["historyDays"], 7);
    assert_eq!(v["sources"]["historyDays"], "runtime");
    // SIGHUP reads a changed file, and the runtime value stays
    std::fs::write(
        &f,
        json!({"datasets": {"demo": {"assistant": {"historyDays": 40, "explain": true}}}})
            .to_string(),
    )
    .unwrap();
    s.hup();
    eventually(|| {
        s.call("GET", "/$/settings/demo/assistant", None).1["effective"]["explain"] == true
    });
    let (_, v) = s.call("GET", "/$/settings/demo/assistant", None);
    assert_eq!(v["effective"]["historyDays"], 7);
    let (st, v) = s.call(
        "DELETE",
        "/$/settings/demo/assistant?field=historyDays",
        None,
    );
    assert_eq!(st, 200);
    assert_eq!(v["effective"]["historyDays"], 40);
    assert_eq!(v["sources"]["historyDays"], "declared");
    // A4: a file that no longer validates is logged and the previous one stays
    std::fs::write(
        &f,
        json!({"datasets": {"demo": {"assistant": {"send": "everything"}}}}).to_string(),
    )
    .unwrap();
    s.hup();
    eventually(|| s.call("GET", "/$/settings", None).1["error"].is_string());
    let (_, v) = s.call("GET", "/$/settings/demo/assistant", None);
    assert_eq!(v["effective"]["historyDays"], 40);
    assert_eq!(v["effective"]["explain"], true);
}

/// The output of a command run against `s` with an empty home, so that no saved login
/// or memory configuration applies.
struct Out {
    code: Option<i32>,
    stdout: String,
    stderr: String,
}

impl Out {
    fn json(&self) -> J {
        serde_json::from_str(&self.stdout)
            .unwrap_or_else(|e| panic!("{e}: {}\n{}", self.stdout, self.stderr))
    }

    fn ok(self) -> Out {
        assert_eq!(self.code, Some(0), "{}\n{}", self.stdout, self.stderr);
        self
    }
}

fn cli(s: &Server, home: &Path, args: &[&str], stdin: &str, env: &[(&str, &str)]) -> Out {
    let mut c = Command::new(BIN);
    c.args(args)
        .env("HOME", home)
        .env("XDG_CONFIG_HOME", home.join("config"))
        .env("XDG_STATE_HOME", home.join("state"))
        .env("SPARKLES_SERVER", format!("http://127.0.0.1:{}", s.port))
        .env_remove("SPARKLES_TOKEN")
        .env_remove("SPARKLES_MEMORY_DATASET")
        .env_remove("VISUAL")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (k, v) in env {
        c.env(k, v);
    }
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

/// An editor that writes `$EDIT_CONTENT` into the file. On its first run with
/// `$EDIT_RACE` set, it first sets `historyDays` with another command (to the digits of
/// the file name followed by 1), so that the
/// edit is sent against a stale ETag.
fn editor(dir: &Path) -> String {
    let p = dir.join("editor.sh");
    std::fs::write(
        &p,
        "#!/bin/sh\n\
         if [ -n \"$EDIT_RACE\" ] && [ ! -e \"$EDIT_RACE\" ]; then\n\
           touch \"$EDIT_RACE\"\n\
           \"$SPARKLES_BIN\" settings set demo assistant.historyDays=$(basename \"$EDIT_RACE\" | tr -dc 0-9)1 >/dev/null || exit 1\n\
         fi\n\
         printf '%s' \"$EDIT_CONTENT\" > \"$1\"\n",
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
    p.to_str().unwrap().to_string()
}

/// `sparkles settings get`, `set`, `edit`, `reset`, `diff` and `apply` against a server
/// whose settings file declares values and locks `assistant.send`.
#[test]
fn settings_commands() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    let f = dir.path().join("settings.json");
    std::fs::write(
        &f,
        json!({
            "defaults": { "locked": ["assistant.send"] },
            "datasets": { "demo": { "assistant": { "historyDays": 30 } } }
        })
        .to_string(),
    )
    .unwrap();
    let s = Server::start_in(
        &dir.path().join("data"),
        &["--settings", f.to_str().unwrap(), "--mem", "demo"],
        47200..47220,
    );
    let (st, v) = s.call(
        "POST",
        "/$/datasets",
        Some(json!({"dbName": "other", "dbType": "mem"})),
    );
    assert!(st == 200 || st == 201, "{st} {v}");
    let run = |args: &[&str]| cli(&s, &home, args, "", &[]);

    // get: each field with its source
    let o = run(&["settings", "get", "demo", "assistant"]).ok();
    let line = |field: &str| {
        o.stdout
            .lines()
            .find(|l| l.split_whitespace().next() == Some(field))
            .unwrap_or_else(|| panic!("{field} in {}", o.stdout))
            .to_string()
    };
    assert!(line("send").contains("locked"), "{}", o.stdout);
    assert!(line("historyDays").contains("declared"), "{}", o.stdout);
    assert!(line("ask").contains("default"), "{}", o.stdout);
    let j = run(&["settings", "get", "demo", "--json"]).ok().json();
    assert_eq!(j["kinds"]["assistant"]["effective"]["historyDays"], 30);
    assert!(j["kinds"]["memory"].is_object());

    // set: values read as JSON or as strings, one PATCH per kind
    let o = run(&[
        "settings",
        "set",
        "demo",
        "assistant.historyDays=7",
        "memory.consolidatedGraph=urn:x:consolidated",
        "assistant.explain=true",
    ])
    .ok();
    assert!(
        o.stdout.contains("assistant.historyDays = 7  (runtime)"),
        "{}",
        o.stdout
    );
    let j = run(&[
        "settings",
        "get",
        "demo",
        "assistant",
        "--layer",
        "runtime",
        "--json",
    ])
    .ok()
    .json();
    assert_eq!(j, json!({"historyDays": 7, "explain": true}));
    let j = run(&["settings", "get", "demo", "memory", "--json"])
        .ok()
        .json();
    assert_eq!(j["effective"]["consolidatedGraph"], "urn:x:consolidated");
    assert_eq!(j["sources"]["consolidatedGraph"], "runtime");
    // a locked field is refused, and the message says why
    let o = run(&["settings", "set", "demo", "assistant.send=documents"]);
    assert_eq!(o.code, Some(1));
    assert!(o.stderr.contains("locks send"), "{}", o.stderr);
    assert!(o.stderr.contains("settings file"), "{}", o.stderr);
    let o = run(&["settings", "set", "demo", "nosuch.x=1"]);
    assert_eq!(o.code, Some(1));
    assert!(o.stderr.contains("no settings kind"), "{}", o.stderr);

    // diff: the runtime values against the declared values and defaults
    let o = run(&["settings", "diff"]).ok();
    assert!(
        o.stdout
            .contains("/demo assistant.historyDays: runtime 7, declared 30"),
        "{}",
        o.stdout
    );
    assert!(
        o.stdout
            .contains("/demo assistant.explain: runtime true, default false"),
        "{}",
        o.stdout
    );
    let j = run(&["settings", "diff", "other", "--json"]).ok().json();
    assert_eq!(j["differences"], json!([]));

    // edit: the runtime layer in the editor, sent with If-Match
    let ed = editor(dir.path());
    let edit = |stdin: &str, content: &J, race: Option<&Path>| {
        let c = content.to_string();
        let r = race.map(|p| p.to_str().unwrap().to_string());
        let mut env = vec![
            ("EDITOR", ed.as_str()),
            ("SPARKLES_BIN", BIN),
            ("EDIT_CONTENT", c.as_str()),
        ];
        if let Some(r) = &r {
            env.push(("EDIT_RACE", r.as_str()));
        }
        cli(
            &s,
            &home,
            &["settings", "edit", "demo", "assistant"],
            stdin,
            &env,
        )
    };
    let o = edit("", &json!({"historyDays": 9}), None).ok();
    assert!(o.stdout.contains("2 fields changed"), "{}", o.stdout);
    let (_, v) = s.call("GET", "/$/settings/demo/assistant", None);
    assert_eq!(v["runtime"], json!({"historyDays": 9}));
    // a change made meanwhile is a 412: declining keeps the server's value
    let race = dir.path().join("race1");
    let o = edit("n\n", &json!({"historyDays": 12}), Some(&race));
    assert_eq!(o.code, Some(1));
    assert!(o.stderr.contains("since they were read"), "{}", o.stderr);
    let (_, v) = s.call("GET", "/$/settings/demo/assistant", None);
    assert_eq!(v["effective"]["historyDays"], 11);
    // accepting opens the editor again on the current settings
    let race = dir.path().join("race2");
    let o = edit("y\n", &json!({"historyDays": 12}), Some(&race)).ok();
    assert!(o.stderr.contains("since they were read"), "{}", o.stderr);
    let (_, v) = s.call("GET", "/$/settings/demo/assistant", None);
    assert_eq!(v["effective"]["historyDays"], 12);
    // a locked field in the editor is refused, and the user may decline to edit again
    let o = edit("n\n", &json!({"historyDays": 12, "send": "rows"}), None);
    assert_eq!(o.code, Some(1));
    assert!(o.stderr.contains("locks send"), "{}", o.stderr);

    // get names the declared value that a runtime value overrides
    let o = run(&["settings", "get", "demo", "assistant"]).ok();
    let history = o
        .stdout
        .lines()
        .find(|l| l.split_whitespace().next() == Some("historyDays"))
        .unwrap_or_default();
    assert!(
        history.contains("runtime, overrides declared 30") && history.ends_with("12"),
        "{}",
        o.stdout
    );
    let j = run(&["settings", "get", "demo", "assistant", "--json"])
        .ok()
        .json();
    assert_eq!(
        j["overrides"],
        json!([{"path": "historyDays", "declared": 30, "runtime": 12}])
    );

    // reset: one field, then a whole kind, each saying which value applies now
    let o = run(&["settings", "reset", "demo", "assistant.historyDays"]).ok();
    assert!(
        o.stdout
            .contains("assistant.historyDays on /demo = 30, the declared value applies"),
        "{}",
        o.stdout
    );
    let o = run(&["settings", "reset", "demo", "memory"]).ok();
    assert!(
        o.stdout.contains("memory on /demo: runtime layer cleared"),
        "{}",
        o.stdout
    );
    assert!(
        o.stdout.contains("consolidatedGraph") && o.stdout.contains("the default applies"),
        "{}",
        o.stdout
    );
    let (_, v) = s.call("GET", "/$/settings/demo/memory", None);
    assert_eq!(v["runtime"], json!({}));

    // apply: defaults to every dataset, entries to theirs, locks reported
    let file = dir.path().join("apply.json");
    std::fs::write(
        &file,
        json!({
            "defaults": { "assistant": { "explain": true }, "locked": ["assistant.explain"] },
            "datasets": {
                "other": { "assistant": { "historyDays": 3 } },
                "ghost": { "assistant": { "historyDays": 4 } }
            }
        })
        .to_string(),
    )
    .unwrap();
    let o = run(&["settings", "apply", file.to_str().unwrap()]).ok();
    assert!(
        o.stdout.contains("/other assistant: explain, historyDays"),
        "{}",
        o.stdout
    );
    assert!(o.stdout.contains("/ghost: skipped"), "{}", o.stdout);
    assert!(
        o.stdout.contains("assistant.explain (defaults)"),
        "{}",
        o.stdout
    );
    let (_, v) = s.call("GET", "/$/settings/other/assistant", None);
    assert_eq!(v["runtime"], json!({"explain": true, "historyDays": 3}));
    assert_eq!(v["locked"], json!(["send"]));
    // a null in an entry resets that field, over a value the defaults give it too
    std::fs::write(
        &file,
        json!({
            "defaults": { "assistant": { "historyDays": 9 } },
            "datasets": { "other": { "assistant": { "historyDays": null } } }
        })
        .to_string(),
    )
    .unwrap();
    run(&["settings", "apply", file.to_str().unwrap()]).ok();
    let (_, v) = s.call("GET", "/$/settings/other/assistant", None);
    assert_eq!(v["runtime"], json!({"explain": true}));
    // a patch that a lock refuses fails that dataset only
    std::fs::write(
        &file,
        json!({ "datasets": {
            "demo": { "assistant": { "send": "documents" } },
            "other": { "assistant": { "historyDays": 5 } }
        } })
        .to_string(),
    )
    .unwrap();
    let o = run(&["settings", "apply", file.to_str().unwrap(), "--json"]);
    assert_eq!(o.code, Some(1));
    let j = o.json();
    assert_eq!(j["failed"][0]["dataset"], "demo");
    assert_eq!(j["failed"][0]["code"], "locked-by-config");
    assert_eq!(j["failed"][0]["fields"], json!(["send"]));
    assert_eq!(j["applied"][0]["dataset"], "other");
    // a file that does not validate patches nothing
    std::fs::write(
        &file,
        json!({"defaults": {"assistant": {"send": "x"}}}).to_string(),
    )
    .unwrap();
    assert_eq!(
        run(&["settings", "apply", file.to_str().unwrap()]).code,
        Some(1)
    );
}

/// A8: `sparkles memory init` turns on server-side ingestion for the fields that have
/// their default, lists the extract providers, leaves a locked `send` alone and says
/// so, and `--no-ingest` skips it. Then the maintenance commands of C18 §8.3 and §8.4.
#[cfg(feature = "memory")]
#[test]
fn memory_init_and_maintenance() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    let models = dir.path().join("models.json");
    let pair = json!([{"provider": "local", "model": "qwen3:8b"}]);
    std::fs::write(
        &models,
        json!({
            "providers": { "local": { "kind": "ollama", "endpoint": "http://127.0.0.1:9" } },
            "roles": { "draft": pair, "extract": pair }
        })
        .to_string(),
    )
    .unwrap();
    let f = dir.path().join("settings.json");
    std::fs::write(
        &f,
        json!({ "datasets": { "locked": { "locked": ["assistant.send"] } } }).to_string(),
    )
    .unwrap();
    let s = Server::start_in(
        &dir.path().join("data"),
        &[
            "--settings",
            f.to_str().unwrap(),
            "--model-config",
            models.to_str().unwrap(),
            "--mem",
            "fresh",
            "--mem",
            "locked",
            "--mem",
            "plain",
        ],
        47220..47240,
    );
    let mem = |ds: &str, args: &[&str]| {
        let mut a = vec!["memory", "--dataset", ds];
        a.extend_from_slice(args);
        cli(&s, &home, &a, "", &[])
    };

    let j = mem("fresh", &["init", "--json"]).ok().json();
    let a = &j["assistant"];
    assert_eq!(a["status"], "changed", "{j:#}");
    let fields: Vec<&str> = a["changed"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["field"].as_str().unwrap())
        .collect();
    assert_eq!(fields, ["enabled", "ingest", "send"]);
    assert_eq!(a["extract"]["providers"][0]["provider"], "local", "{j:#}");
    assert_eq!(a["extract"]["providers"][0]["send"], "documents");
    assert!(a["note"].as_str().unwrap().contains("rows"));
    let (_, v) = s.call("GET", "/$/settings/fresh/assistant", None);
    assert_eq!(v["effective"]["send"], "documents");
    assert_eq!(v["sources"]["ingest"], "runtime");
    // a second init leaves the runtime values alone
    let j = mem("fresh", &["init", "--json"]).ok().json();
    assert_eq!(j["assistant"]["status"], "unchanged");
    assert_eq!(j["assistant"]["unchanged"][2]["source"], "runtime");

    // send locked at schema: left alone and reported
    let o = mem("locked", &["init"]).ok();
    assert!(
        o.stdout
            .contains("assistant: set enabled = true, ingest = true"),
        "{}",
        o.stdout
    );
    assert!(
        o.stdout
            .contains("assistant: left send at \"schema\" (locked by the server's settings file)"),
        "{}",
        o.stdout
    );
    assert!(
        o.stdout.contains("local/qwen3:8b (send \"schema\""),
        "{}",
        o.stdout
    );
    assert!(!o.stdout.contains("note:"), "{}", o.stdout);
    let (_, v) = s.call("GET", "/$/settings/locked/assistant", None);
    assert_eq!(v["effective"]["send"], "schema");

    // --no-ingest
    let j = mem("plain", &["init", "--no-ingest", "--json"]).ok().json();
    assert_eq!(j["assistant"]["status"], "skipped");
    let (_, v) = s.call("GET", "/$/settings/plain/assistant", None);
    assert_eq!(v["runtime"], json!({}));

    // maintenance: two sessions assert the same fact about known entities
    cli(
        &s,
        &home,
        &[
            "update",
            "--dataset",
            "plain",
            "INSERT DATA { GRAPH <urn:x:curated> { <http://example.org/kai> \
             <http://example.org/memberOf> <http://example.org/payments> . \
             <http://example.org/ana> a <http://example.org/Person> } }",
        ],
        "",
        &[],
    )
    .ok();
    for n in ["s1", "s2"] {
        let (st, v) = s.call(
            "POST",
            "/plain/facts",
            Some(json!({
                "graph": format!("<urn:x-sparkles:import/kc/sessions/{n}>"),
                "facts": [{ "s": "<http://example.org/ana>", "p": "<http://example.org/memberOf>",
                            "o": "<http://example.org/payments>" }],
            })),
        );
        assert_eq!(st, 200, "{v}");
    }
    let o = mem("plain", &["consolidate", "--dry-run"]);
    assert_eq!(o.code, Some(1), "{}", o.stdout);
    assert!(o.stderr.contains("consolidatedGraph"), "{}", o.stderr);
    cli(
        &s,
        &home,
        &[
            "settings",
            "set",
            "plain",
            "memory.consolidatedGraph=urn:x:consolidated",
            "memory.consolidation.every=1h",
        ],
        "",
        &[],
    )
    .ok();
    let j = mem("plain", &["consolidate", "--dry-run", "--json"])
        .ok()
        .json();
    assert_eq!(j["status"], "done", "{j:#}");
    assert_eq!(j["result"]["outcome"], "dry-run", "{j:#}");
    assert_eq!(j["result"]["repeated"], 1, "{j:#}");
    let o = mem("plain", &["consolidate"]).ok();
    assert!(
        o.stdout.contains("proposed on the branch consolidation."),
        "{}",
        o.stdout
    );
    let j = mem(
        "plain",
        &["consolidate", "--dry-run", "--no-wait", "--json"],
    )
    .ok()
    .json();
    assert!(j["id"].is_string(), "{j:#}");
    // retention needs `after` without a setting
    let o = mem("plain", &["retention", "--dry-run"]);
    assert_eq!(o.code, Some(1));
    assert!(o.stderr.contains("after"), "{}", o.stderr);
    let o = mem("plain", &["retention", "--after", "1d", "--dry-run"]).ok();
    assert!(
        o.stdout.contains("retention of /plain after 1d"),
        "{}",
        o.stdout
    );
    // the sessions written a moment ago are kept, and the output says why
    assert!(o.stdout.contains("  kept: recent"), "{}", o.stdout);
    let o = mem("plain", &["maintenance"]).ok();
    assert!(o.stdout.contains("consolidation: every 1h"), "{}", o.stdout);
    assert!(o.stdout.contains("next run due"), "{}", o.stdout);
    assert!(
        o.stdout.contains("retention: not configured"),
        "{}",
        o.stdout
    );
    // the consolidation branch waits for review
    assert!(o.stdout.contains("\nreview: "), "{}", o.stdout);
    assert!(
        o.stdout.contains("\n  consolidation: 1 since "),
        "{}",
        o.stdout
    );
    let j = mem("plain", &["maintenance", "--json"]).ok().json();
    assert_eq!(j["consolidation"]["nextRun"], "due", "{j:#}");
    assert_eq!(j["review"]["kinds"]["consolidation"]["open"], 1, "{j:#}");
}
