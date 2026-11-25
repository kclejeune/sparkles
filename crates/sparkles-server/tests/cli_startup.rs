//! `serve` with a dataset that does not open (spec C01 §4.1): the server binds, serves
//! the other datasets and reports itself degraded, `--ready-ignore-failed` reports it
//! ready, `--require-all-datasets` stops it, and SIGTERM stops it cleanly.

#![cfg(unix)]

mod common;

use serde_json::Value as J;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const BIN: &str = env!("CARGO_BIN_EXE_sparkles");

struct Server {
    child: common::ServerProcess,
    base: String,
}

fn start(data: &Path, extra: &[&str], log: &Path) -> Server {
    let (child, port) = common::ServerProcess::start(
        common::serve()
            .arg("--data")
            .arg(data)
            .args(extra)
            .env_remove("SPARKLES_AUTH_CONFIG")
            .env_remove("SPARKLES_BACKUP_CONFIG")
            .env("RUST_LOG", "info"),
        log,
    );
    Server {
        child,
        base: format!("http://127.0.0.1:{port}"),
    }
}

fn get(url: &str) -> Option<(u16, J)> {
    let r = reqwest::blocking::get(url).ok()?;
    let status = r.status().as_u16();
    Some((
        status,
        serde_json::from_slice(&r.bytes().ok()?).unwrap_or(J::Null),
    ))
}

/// Wait until `/$/ready` reports a status other than `starting`.
fn settled(s: &mut Server) -> (u16, J) {
    let t0 = Instant::now();
    loop {
        assert!(s.child.try_wait().unwrap().is_none(), "the server exited");
        if let Some((code, r)) = get(&format!("{}/$/ready", s.base))
            && r["status"] != "starting"
        {
            return (code, r);
        }
        assert!(t0.elapsed() < Duration::from_secs(60), "never settled");
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// A data directory with the datasets `a`, which holds a triple, and `bad`, whose
/// `dataset.json` is not JSON.
fn data_with_a_bad_dataset(dir: &Path) -> std::path::PathBuf {
    let data = dir.join("data");
    let mut s = start(&data, &[], &dir.join("setup.log"));
    let (code, _) = settled(&mut s);
    assert_eq!(code, 200);
    let c = reqwest::blocking::Client::new();
    for n in ["a", "bad"] {
        let r = c
            .post(format!("{}/$/datasets?dbName={n}&dbType=tdb2", s.base))
            .send()
            .unwrap();
        assert!(r.status().is_success(), "{}", r.text().unwrap());
    }
    let r = c
        .post(format!("{}/a/update", s.base))
        .header("content-type", "application/sparql-update")
        .body("INSERT DATA { <urn:a> <urn:p> 1 }")
        .send()
        .unwrap();
    assert!(r.status().is_success());
    drop(s);
    std::fs::write(data.join("databases/bad/dataset.json"), b"not json").unwrap();
    data
}

fn sigterm(s: &mut Server) -> std::process::ExitStatus {
    let pid = s.child.id().to_string();
    assert!(
        Command::new("kill")
            .args(["-TERM", &pid])
            .status()
            .unwrap()
            .success()
    );
    let t0 = Instant::now();
    loop {
        if let Some(st) = s.child.try_wait().unwrap() {
            return st;
        }
        assert!(
            t0.elapsed() < Duration::from_secs(60),
            "no exit after SIGTERM"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn a_dataset_that_does_not_open_leaves_the_others_serving() {
    let dir = tempfile::tempdir().unwrap();
    let data = data_with_a_bad_dataset(dir.path());

    let mut s = start(&data, &[], &dir.path().join("degraded.log"));
    let (code, r) = settled(&mut s);
    assert_eq!(code, 503, "{r}");
    assert_eq!(r["status"], "degraded", "{r}");
    let (code, r) = get(&format!(
        "{}/a/sparql?query=ASK%7B%3Fs%20%3Fp%20%3Fo%7D",
        s.base
    ))
    .unwrap();
    assert_eq!((code, &r["boolean"]), (200, &J::Bool(true)), "{r}");
    let (code, r) = get(&format!("{}/bad/sparql?query=ASK%7B%7D", s.base)).unwrap();
    assert_eq!(code, 503, "{r}");
    assert_eq!(r["code"], "dataset-failed", "{r}");
    assert!(sigterm(&mut s).success());

    let mut s = start(
        &data,
        &["--ready-ignore-failed"],
        &dir.path().join("ignore.log"),
    );
    let (code, r) = settled(&mut s);
    assert_eq!((code, r["status"].as_str()), (200, Some("degraded")), "{r}");
    drop(s);

    let log = dir.path().join("require.log");
    let mut child = common::serve()
        .arg("--data")
        .arg(&data)
        .arg("--require-all-datasets")
        .env_remove("SPARKLES_AUTH_CONFIG")
        .env_remove("SPARKLES_BACKUP_CONFIG")
        .stdout(Stdio::null())
        .stderr(std::fs::File::create(&log).unwrap())
        .spawn()
        .unwrap();
    let t0 = Instant::now();
    let status = loop {
        if let Some(st) = child.try_wait().unwrap() {
            break st;
        }
        if t0.elapsed() >= Duration::from_secs(60) {
            let _ = child.kill();
            let _ = child.wait();
            panic!("the server kept running");
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    assert!(!status.success());
    let text = std::fs::read_to_string(&log).unwrap();
    assert!(text.contains("--require-all-datasets"), "{text}");

    // the registry still names both datasets
    let reg = std::fs::read_to_string(data.join("config.json")).unwrap();
    assert!(reg.contains("\"a\"") && reg.contains("\"bad\""), "{reg}");
}

#[test]
fn port_zero_reports_each_servers_own_listener() {
    let dir = tempfile::tempdir().unwrap();
    let holder = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let occupied = holder.local_addr().unwrap();
    let first_log = dir.path().join("first.log");
    // A previous launch in an appended log must not supply the new child's address.
    std::fs::write(
        &first_log,
        format!("Sparkles test listening on http://{occupied}/ (UI at /ui/), opening 0 datasets\n"),
    )
    .unwrap();
    let mut first = start(&dir.path().join("first"), &["--mem", "first"], &first_log);
    let mut second = start(
        &dir.path().join("second"),
        &["--mem", "second"],
        &dir.path().join("second.log"),
    );

    assert_ne!(first.base, format!("http://{occupied}"));
    assert_ne!(second.base, format!("http://{occupied}"));
    assert_ne!(first.base, second.base);
    assert_eq!(settled(&mut first).0, 200);
    assert_eq!(settled(&mut second).0, 200);
    assert_eq!(
        get(&format!("{}/$/datasets/first", first.base)).unwrap().0,
        200
    );
    assert_eq!(
        get(&format!("{}/$/datasets/first", second.base)).unwrap().0,
        404
    );
    assert_eq!(
        get(&format!("{}/$/datasets/second", second.base))
            .unwrap()
            .0,
        200
    );
}

#[test]
fn an_explicit_port_in_use_fails_to_bind() {
    let dir = tempfile::tempdir().unwrap();
    let holder = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = holder.local_addr().unwrap();
    let out = Command::new(BIN)
        .args([
            "serve",
            "--host",
            "127.0.0.1",
            "--port",
            &addr.port().to_string(),
        ])
        .arg("--data")
        .arg(dir.path().join("data"))
        .env_remove("SPARKLES_AUTH_CONFIG")
        .env_remove("SPARKLES_BACKUP_CONFIG")
        .output()
        .unwrap();
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "{err}");
    assert!(err.contains(&format!("binding {addr}")), "{err}");
}
