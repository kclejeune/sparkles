//! The actual dataset CLI against a stopped catalog and a running server.
use serde_json::Value as J;
use std::path::Path;
use std::process::{Command, Output};
const BIN: &str = env!("CARGO_BIN_EXE_sparkles");
fn run(dir: &Path, args: &[&str]) -> Output {
    Command::new(BIN)
        .current_dir(dir)
        .args(args)
        .env_remove("SPARKLES_SERVER")
        .env_remove("SPARKLES_TOKEN")
        .env("XDG_CONFIG_HOME", dir.join("config"))
        .output()
        .unwrap()
}
fn ok(dir: &Path, args: &[&str]) -> J {
    let out = run(dir, args);
    assert!(
        out.status.success(),
        "{args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).unwrap()
}
#[test]
fn catalog_dataset_cli_crud_clone_and_lock() {
    let dir = tempfile::tempdir().unwrap();
    let dir = dir.path();
    let create = ok(
        dir,
        &["dataset", "create", "wiki", "--data-dir", "data", "--json"],
    );
    let id = create["id"].clone();
    assert_eq!(create["name"], "wiki");
    let updated = run(
        dir,
        &[
            "update",
            "--loc",
            "data/databases/wiki",
            "INSERT DATA {<urn:s> <urn:p> 1}",
        ],
    );
    assert!(
        updated.status.success(),
        "{}",
        String::from_utf8_lossy(&updated.stderr)
    );
    let cloned = ok(
        dir,
        &[
            "dataset",
            "clone",
            "wiki",
            "copy",
            "--data-dir",
            "data",
            "--at",
            "1",
            "--json",
        ],
    );
    assert_ne!(cloned["id"], id);
    let renamed = ok(
        dir,
        &[
            "dataset",
            "rename",
            "wiki",
            "renamed",
            "--data-dir",
            "data",
            "--json",
        ],
    );
    assert_eq!(renamed["id"], id);
    assert_eq!(
        ok(dir, &["dataset", "list", "--data-dir", "data", "--json"])["datasets"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    ok(
        dir,
        &["dataset", "delete", "copy", "--data-dir", "data", "--json"],
    );
    assert!(
        !run(dir, &["dataset", "create", "../bad", "--data-dir", "data"])
            .status
            .success()
    );
    assert!(
        !run(dir, &["dataset", "delete", "missing", "--data-dir", "data"])
            .status
            .success()
    );
    let _lock = sparkles::Catalog::open(dir.join("data"), Default::default()).unwrap();
    let locked = run(dir, &["dataset", "list", "--data-dir", "data"]);
    assert!(!locked.status.success());
    assert!(String::from_utf8_lossy(&locked.stderr).contains("in use by another process"));
}

#[test]
fn item62_audit_dataset_commands_log_local_identity_and_actual_clone_commit() {
    let temp = tempfile::tempdir().unwrap();
    let dir = temp.path();
    let audited = |args: &[&str]| {
        let output = Command::new(BIN)
            .current_dir(dir)
            .args(["--log-format", "json"])
            .args(args)
            .env_remove("SPARKLES_SERVER")
            .env_remove("SPARKLES_TOKEN")
            .env("XDG_CONFIG_HOME", dir.join("config"))
            .env("RUST_LOG", "sparkles::audit=info")
            .env("USER", "audit-local")
            .output()
            .unwrap();
        let events: Vec<J> = String::from_utf8_lossy(&output.stderr)
            .lines()
            .filter_map(|line| serde_json::from_str::<J>(line).ok())
            .filter(|record| record["target"] == "sparkles::audit")
            .collect();
        (output, events)
    };
    let (created, create_events) = audited(&[
        "dataset",
        "create",
        "audit-source",
        "--data-dir",
        "data",
        "--json",
    ]);
    assert!(
        created.status.success(),
        "{}",
        String::from_utf8_lossy(&created.stderr)
    );
    let source: J = serde_json::from_slice(&created.stdout).unwrap();
    for number in [1, 2] {
        let updated = run(
            dir,
            &[
                "update",
                "--loc",
                "data/databases/audit-source",
                &format!("INSERT DATA {{ <urn:local:{number}> <urn:p> {number} }}"),
            ],
        );
        assert!(
            updated.status.success(),
            "{}",
            String::from_utf8_lossy(&updated.stderr)
        );
    }
    let (cloned, clone_events) = audited(&[
        "dataset",
        "clone",
        "audit-source",
        "audit-copy",
        "--at",
        "1",
        "--data-dir",
        "data",
        "--json",
    ]);
    assert!(
        cloned.status.success(),
        "{}",
        String::from_utf8_lossy(&cloned.stderr)
    );
    let target: J = serde_json::from_slice(&cloned.stdout).unwrap();
    let (deleted, delete_events) = audited(&[
        "dataset",
        "delete",
        "audit-source",
        "--data-dir",
        "data",
        "--json",
    ]);
    assert!(
        deleted.status.success(),
        "{}",
        String::from_utf8_lossy(&deleted.stderr)
    );
    let value: J = serde_json::from_slice(&deleted.stdout).unwrap();
    assert_eq!(value["deleted"], "audit-source");
    for (name, events) in [
        ("dataset_created", &create_events),
        ("dataset_cloned", &clone_events),
        ("dataset_deleted", &delete_events),
    ] {
        assert_eq!(
            events.len(),
            1,
            "missing or duplicate local {name} audit event: {events:?}"
        );
        assert_eq!(events[0]["fields"]["event"], name);
        assert_eq!(events[0]["fields"]["principal"], "local:audit-local");
    }
    assert_eq!(create_events[0]["fields"]["id"], source["id"]);
    assert_eq!(clone_events[0]["fields"]["id"], target["id"]);
    assert_eq!(clone_events[0]["fields"]["source_commit"], 1);
    assert_eq!(delete_events[0]["fields"]["id"], source["id"]);
    assert!(delete_events[0]["fields"].get("quads").is_none());
    let (failed, events) = audited(&[
        "dataset",
        "delete",
        "missing",
        "--data-dir",
        "data",
        "--json",
    ]);
    assert!(!failed.status.success());
    assert_eq!(
        events.len(),
        1,
        "missing local failure audit event: {events:?}"
    );
    assert_eq!(events[0]["fields"]["event"], "dataset_admin_failed");
    assert!(events[0]["fields"]["code"].is_string());
}

#[cfg(feature = "auth")]
#[test]
fn online_dataset_cli_crud_clone() {
    use std::process::{Child, Stdio};
    use std::time::{Duration, Instant};
    struct Server(Child);
    impl Drop for Server {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let temp = tempfile::tempdir().unwrap();
    let dir = temp.path();
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let mut server = Server(
        Command::new(BIN)
            .current_dir(dir)
            .args([
                "serve",
                "--host",
                "127.0.0.1",
                "--port",
                &port.to_string(),
                "--data",
                "data",
            ])
            .env_remove("SPARKLES_AUTH_CONFIG")
            .env_remove("SPARKLES_BACKUP_CONFIG")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let start = Instant::now();
    while std::net::TcpStream::connect(("127.0.0.1", port)).is_err() {
        assert!(server.0.try_wait().unwrap().is_none(), "server exited");
        assert!(
            start.elapsed() < Duration::from_secs(30),
            "server did not start"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    wait_ready(port);
    let url = format!("http://127.0.0.1:{port}");
    let created = ok(
        dir,
        &["dataset", "create", "wiki", "--server", &url, "--json"],
    );
    let id = created["id"].clone();
    assert!(id.is_string());
    let copied = ok(
        dir,
        &[
            "dataset", "clone", "wiki", "copy", "--server", &url, "--json",
        ],
    );
    assert_ne!(copied["id"], id);
    let renamed = ok(
        dir,
        &[
            "dataset", "rename", "wiki", "renamed", "--server", &url, "--json",
        ],
    );
    assert_eq!(renamed["id"], id);
    assert_eq!(
        ok(dir, &["dataset", "list", "--server", &url, "--json"])["datasets"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    ok(
        dir,
        &["dataset", "delete", "copy", "--server", &url, "--json"],
    );
}

/// Wait until the server on `port` answers `GET /$/ready` with `200`: it listens before
/// its datasets open.
fn wait_ready(port: u16) {
    use std::io::{Read as _, Write as _};
    let t0 = std::time::Instant::now();
    loop {
        if let Ok(mut c) = std::net::TcpStream::connect(("127.0.0.1", port)) {
            let _ = write!(c, "GET /$/ready HTTP/1.0\r\nHost: 127.0.0.1\r\n\r\n");
            let mut buf = String::new();
            let _ = c.read_to_string(&mut buf);
            if buf.starts_with("HTTP/1.0 200") || buf.starts_with("HTTP/1.1 200") {
                return;
            }
        }
        assert!(
            t0.elapsed() < std::time::Duration::from_secs(60),
            "the server never became ready"
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}
