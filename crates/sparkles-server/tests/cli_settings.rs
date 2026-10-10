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
        let port = (47130..47150)
            .find(|p| std::net::TcpListener::bind(("127.0.0.1", *p)).is_ok())
            .expect("a free port in 47130-47149");
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
