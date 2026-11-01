//! `serve --prewarm` (spec X07 §7): a server records its prewarm list at shutdown, the
//! next start reads it in the background once ready, and a list of an unknown version is
//! refused with a warning while the dataset serves as usual.

#![cfg(unix)]

use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const BIN: &str = env!("CARGO_BIN_EXE_sparkles");

struct Server {
    child: Child,
    base: String,
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn free_port() -> u16 {
    (39400..39500)
        .find(|p| std::net::TcpListener::bind(("127.0.0.1", *p)).is_ok())
        .expect("a free port in 39400..39500")
}

fn start(dir: &Path, log: &Path) -> Server {
    let port = free_port();
    let child = Command::new(BIN)
        .args(["serve", "--host", "127.0.0.1", "--port", &port.to_string()])
        .arg("--data")
        .arg(dir.join("data"))
        .arg("--loc")
        .arg(format!("ds={}", dir.join("db").display()))
        .arg("--prewarm")
        .env_remove("SPARKLES_AUTH_CONFIG")
        .env_remove("SPARKLES_BACKUP_CONFIG")
        .env("RUST_LOG", "info")
        .stdout(Stdio::null())
        .stderr(std::fs::File::create(log).unwrap())
        .spawn()
        .unwrap();
    let s = Server {
        child,
        base: format!("http://127.0.0.1:{port}"),
    };
    let t0 = Instant::now();
    while reqwest::blocking::get(format!("{}/$/ready", s.base))
        .map(|r| r.status().as_u16() != 200)
        .unwrap_or(true)
    {
        assert!(t0.elapsed() < Duration::from_secs(60), "never ready");
        std::thread::sleep(Duration::from_millis(50));
    }
    s
}

fn query(s: &Server) -> String {
    reqwest::blocking::Client::new()
        .post(format!("{}/ds/sparql", s.base))
        .header("content-type", "application/sparql-query")
        .header("accept", "text/tab-separated-values")
        .body("SELECT ?v WHERE { <http://e/s777> <http://e/p> ?v }")
        .send()
        .unwrap()
        .text()
        .unwrap()
}

fn sigterm(s: &mut Server) {
    let pid = s.child.id().to_string();
    assert!(
        Command::new("kill")
            .args(["-TERM", &pid])
            .status()
            .unwrap()
            .success()
    );
    let t0 = Instant::now();
    while s.child.try_wait().unwrap().is_none() {
        assert!(
            t0.elapsed() < Duration::from_secs(60),
            "no exit after SIGTERM"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Wait until the log holds `what`, and return the log.
fn wait_log(log: &Path, what: &str) -> String {
    let t0 = Instant::now();
    loop {
        let text = std::fs::read_to_string(log).unwrap_or_default();
        if text.contains(what) {
            return text;
        }
        assert!(
            t0.elapsed() < Duration::from_secs(60),
            "{what:?} not in:\n{text}"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn a_restart_reads_the_list_the_last_run_recorded() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    let mut nt = String::new();
    for i in 0..3000 {
        nt.push_str(&format!("<http://e/s{i}> <http://e/p> \"v{i}\" .\n"));
    }
    std::fs::write(dir.join("data.nt"), nt).unwrap();
    let out = Command::new(BIN)
        .arg("load")
        .arg("--loc")
        .arg(dir.join("db"))
        .arg(dir.join("data.nt"))
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    // the first run has no list yet, and its shutdown writes one
    let log1 = dir.join("first.log");
    let mut s = start(dir, &log1);
    wait_log(&log1, "no prewarm list was recorded");
    assert!(query(&s).contains("\"v777\""));
    sigterm(&mut s);
    let list: serde_json::Value =
        serde_json::from_slice(&std::fs::read(dir.join("db/prewarm.json")).unwrap()).unwrap();
    assert_eq!(list["version"], 1);
    assert!(
        list["columns"].as_object().is_some_and(|c| !c.is_empty()),
        "{list}"
    );

    // the second run reads it after it is ready
    let log2 = dir.join("second.log");
    let mut s = start(dir, &log2);
    let text = wait_log(&log2, "prewarm finished in");
    assert!(text.contains("prewarm of /ds finished"), "{text}");
    assert!(!text.contains("finished: 0 block parts"), "{text}");
    assert!(query(&s).contains("\"v777\""));
    sigterm(&mut s);

    // a list of a version this build does not read is refused, and the dataset serves
    let mut list = list;
    list["version"] = 99.into();
    std::fs::write(dir.join("db/prewarm.json"), list.to_string()).unwrap();
    let log3 = dir.join("third.log");
    let s = start(dir, &log3);
    let text = wait_log(&log3, "prewarm finished in");
    assert!(
        text.contains("prewarm list version 99 is not one this build reads"),
        "{text}"
    );
    assert!(query(&s).contains("\"v777\""));
}
