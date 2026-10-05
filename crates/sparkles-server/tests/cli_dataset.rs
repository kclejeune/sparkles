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
