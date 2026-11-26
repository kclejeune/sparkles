//! `serve` with a dataset that does not open (spec C01 §4.1): the server binds, serves
//! the other datasets and reports itself degraded, `--ready-ignore-failed` reports it
//! ready, `--require-all-datasets` stops it, and SIGTERM stops it cleanly.

#![cfg(unix)]

mod common;

use serde_json::Value as J;
use std::path::Path;
use std::process::Command;
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
    let r = reqwest::blocking::Client::builder()
        .no_proxy()
        .build()
        .ok()?
        .get(url)
        .send()
        .ok()?;
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
            assert!(s.child.try_wait().unwrap().is_none(), "the server exited");
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
    let c = reqwest::blocking::Client::builder()
        .no_proxy()
        .build()
        .unwrap();
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
    let status = common::ServerProcess::spawn(
        common::serve()
            .arg("--data")
            .arg(&data)
            .arg("--require-all-datasets")
            .env_remove("SPARKLES_AUTH_CONFIG")
            .env_remove("SPARKLES_BACKUP_CONFIG"),
        std::fs::File::create(&log).unwrap(),
    )
    .wait_for_exit(Duration::from_secs(60), &log);
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
        format!(
            "2026-01-01T00:00:00Z  INFO sparkles: Sparkles test listening on http://{occupied}/ (UI at /ui/), opening 0 datasets\n"
        ),
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
    let log = dir.path().join("collision.log");
    let status = common::ServerProcess::spawn(
        Command::new(BIN)
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
            .env_remove("SPARKLES_BACKUP_CONFIG"),
        std::fs::File::create(&log).unwrap(),
    )
    .wait_for_exit(Duration::from_secs(30), &log);
    let err = std::fs::read_to_string(&log).unwrap();
    assert!(!status.success(), "{err}");
    assert!(err.contains(&format!("binding {addr}")), "{err}");
}

#[test]
fn ignores_unrelated_startup_diagnostics() {
    let dir = tempfile::tempdir().unwrap();
    let holder = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = holder.local_addr().unwrap();
    let (child, port) = common::ServerProcess::start(
        Command::new("sh")
            .args(["-c", "printf '%s\\n' \"$1\" >&2; shift; exec \"$@\"", "sh"])
            .arg(format!("2026-01-01T00:00:00Z  INFO wrapper: Sparkles test listening on http://{addr}/ (UI at /ui/), opening 0 datasets"))
            .args([BIN, "serve", "--host", "127.0.0.1", "--port", "0"])
            .arg("--data")
            .arg(dir.path().join("data"))
            .env("RUST_LOG", "info")
            .env_remove("SPARKLES_AUTH_CONFIG")
            .env_remove("SPARKLES_BACKUP_CONFIG"),
        &dir.path().join("server.log"),
    );
    assert_ne!(port, addr.port());
    let mut server = Server {
        child,
        base: format!("http://127.0.0.1:{port}"),
    };
    assert_eq!(settled(&mut server).0, 200);
}

#[test]
fn port_zero_works_on_ipv6_loopback() {
    match std::net::TcpListener::bind("[::1]:0") {
        Ok(_) => {}
        Err(e)
            if matches!(
                e.kind(),
                std::io::ErrorKind::AddrNotAvailable | std::io::ErrorKind::Unsupported
            ) =>
        {
            return;
        }
        Err(e) => panic!("cannot probe IPv6 loopback support: {e}"),
    }
    let dir = tempfile::tempdir().unwrap();
    for (index, host) in ["::1", "[::1]"].into_iter().enumerate() {
        let (child, port) = common::ServerProcess::start(
            Command::new(BIN)
                .args(["serve", "--host", host, "--port", "0"])
                .arg("--data")
                .arg(dir.path().join(index.to_string()))
                .env("RUST_LOG", "info")
                .env_remove("SPARKLES_AUTH_CONFIG")
                .env_remove("SPARKLES_BACKUP_CONFIG"),
            &dir.path().join("server.log"),
        );
        let mut server = Server {
            child,
            base: format!("http://[::1]:{port}"),
        };
        assert_eq!(settled(&mut server).0, 200);
    }
}

#[test]
fn metrics_logs_the_assigned_port() {
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("server.log");
    let mut server = start(
        &dir.path().join("data"),
        &["--metrics-addr", "127.0.0.1:0"],
        &log,
    );
    assert_eq!(settled(&mut server).0, 200);
    let text = std::fs::read_to_string(log).unwrap();
    let metrics = text
        .lines()
        .find_map(|line| line.split_once("metrics at ").map(|(_, url)| url))
        .expect("expected a metrics URL in the server log");
    assert!(
        !metrics.contains(":0/"),
        "metrics reported an unassigned port: {metrics}"
    );
    assert_eq!(get(metrics).unwrap().0, 200);
}

#[test]
fn waiting_for_an_expected_exit_times_out_and_reaps_a_live_child() {
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("server.log");
    let child = common::ServerProcess::spawn(
        Command::new("sleep").arg("30"),
        std::fs::File::create(&log).unwrap(),
    );
    let pid = child.id();
    let timeout = std::panic::catch_unwind(|| child.wait_for_exit(Duration::from_millis(50), &log));
    assert!(timeout.is_err());
    assert!(
        !Command::new("kill")
            .args(["-0", &pid.to_string()])
            .output()
            .unwrap()
            .status
            .success(),
        "the child survived the timeout"
    );
}

#[test]
fn listener_startup_failure_includes_a_diagnostic_written_just_before_exit() {
    use std::io::Write;
    use std::os::unix::process::ExitStatusExt;
    use std::process::ExitStatus;

    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("server.log");
    let mut writer = std::fs::File::create(&log).unwrap();
    let reader = std::fs::File::open(&log).unwrap();

    let failure = std::panic::catch_unwind(move || {
        common::wait_for_listener(reader, || {
            // The first read has reached EOF, and the child then logs its error and exits.
            writeln!(writer, "last diagnostic").unwrap();
            Ok(Some(ExitStatus::from_raw(23 << 8)))
        })
    })
    .expect_err("the failed child should stop listener discovery");
    let message = failure.downcast_ref::<String>().unwrap();
    assert!(message.contains("23") && message.contains("last diagnostic"));
}
