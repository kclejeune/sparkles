//! Scratch-branch expiry through the real startup of `sparkles serve --mcp` (roadmap
//! item 57). The sweeper deletes branches, so it must run only when the operator allowed
//! MCP updates, whether the duration comes from `--mcp-scratch-branch-ttl` or from a
//! dataset's `memory.scratchBranches.ttl`.
#![cfg(all(feature = "mcp", unix))]

use serde_json::{Value, json};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const BIN: &str = env!("CARGO_BIN_EXE_sparkles");
const BRANCH: &str = "stale-branch";

/// Run `sparkles ARGS` in `dir` and require success.
fn run(dir: &Path, args: &[&str]) {
    let o = Command::new(BIN)
        .args(args)
        .current_dir(dir)
        .env_remove("SPARKLES_SERVER")
        .output()
        .unwrap();
    assert!(
        o.status.success(),
        "{args:?}: {}",
        String::from_utf8_lossy(&o.stderr)
    );
}

/// A database `dir/db` with the scratch branch [`BRANCH`] created in 2020, so that it is
/// idle for any duration, and the dataset setting `ttl` when given.
fn database(dir: &Path, ttl: Option<&str>) {
    run(
        dir,
        &["update", "--loc", "db", "INSERT DATA { <urn:a> <urn:p> 1 }"],
    );
    run(dir, &["branch", "create", "--loc", "db", BRANCH]);
    let table = dir.join("db/branches.json");
    let mut v: Value = serde_json::from_slice(&std::fs::read(&table).unwrap()).unwrap();
    fn mark(v: &mut Value) -> bool {
        match v {
            Value::Object(o) if o.get("name") == Some(&json!(BRANCH)) => {
                o.insert("created".into(), json!("2020-01-01T00:00:00Z"));
                o.insert("scratch".into(), json!({"creator": "agent"}));
                true
            }
            Value::Object(o) => o.values_mut().any(mark),
            Value::Array(a) => a.iter_mut().any(mark),
            _ => false,
        }
    }
    assert!(mark(&mut v), "{v}");
    std::fs::write(&table, serde_json::to_vec_pretty(&v).unwrap()).unwrap();
    if let Some(ttl) = ttl {
        std::fs::write(
            dir.join("db/memory.json"),
            json!({"format": 2, "scratchBranches": {"ttl": ttl}}).to_string(),
        )
        .unwrap();
    }
}

/// Whether the branch table still lists the branch. A deleted branch stays in the
/// table's `deleted` list, so only `branches` counts.
fn exists(dir: &Path) -> bool {
    let table: Value =
        serde_json::from_slice(&std::fs::read(dir.join("db/branches.json")).unwrap()).unwrap();
    table["branches"]
        .as_array()
        .unwrap()
        .iter()
        .any(|b| b["name"] == BRANCH)
}

struct Server(Child);

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// A port that was free a moment ago, as the other serve tests pick one.
fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

/// The running server and its log.
struct Running {
    server: Server,
    port: u16,
    log: PathBuf,
}

impl Running {
    fn log(&self) -> String {
        std::fs::read_to_string(&self.log).unwrap_or_default()
    }

    fn alive(&mut self) -> bool {
        self.server.0.try_wait().unwrap().is_none()
    }

    /// Wait until the log holds `needle`. A child that exits meanwhile gives `None`.
    fn wait_for(&mut self, needle: &str, limit: Duration) -> Option<()> {
        let t0 = Instant::now();
        loop {
            if self.log().contains(needle) {
                return Some(());
            }
            if !self.alive() {
                return None;
            }
            assert!(t0.elapsed() < limit, "no {needle:?} in\n{}", self.log());
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// Whether `/$/ready` answers `200`. The port is this child's, since the child
    /// logged that it bound it and is still running. Connecting, writing and the one
    /// read each wait at most 2 s, and at most `left`, so a child that accepts the
    /// connection and stalls counts as not ready and leaves the caller's deadline intact.
    fn ready(&self, left: Duration) -> bool {
        let limit = left
            .min(Duration::from_secs(2))
            .max(Duration::from_millis(1));
        let addr = std::net::SocketAddr::from(([127, 0, 0, 1], self.port));
        let Ok(mut c) = std::net::TcpStream::connect_timeout(&addr, limit) else {
            return false;
        };
        if c.set_read_timeout(Some(limit)).is_err() || c.set_write_timeout(Some(limit)).is_err() {
            return false;
        }
        if write!(c, "GET /$/ready HTTP/1.0\r\nHost: 127.0.0.1\r\n\r\n").is_err() {
            return false;
        }
        // the status line comes first, so one read of its start decides
        let mut buf = [0u8; 12];
        c.read_exact(&mut buf).is_ok() && &buf == b"HTTP/1.0 200"
    }
}

/// Start `sparkles serve` over `dir/db`, identify the child by its own log line about
/// the address it bound, wait for the line with the server's decision about scratch
/// branch expiry, and then for HTTP readiness. Another process may take the port between
/// the probe and the child's bind, and the child then exits, so the start is repeated on
/// a new port, up to 5 times. The decision line comes after the sweeper was started or
/// left off, so a case that watches the branch from there sees the whole outcome.
fn serve(dir: &Path, extra: &[&str]) -> Running {
    let log = dir.join("serve.log");
    for _ in 0..5 {
        let port = free_port();
        let child = Command::new(BIN)
            .args(["serve", "--host", "127.0.0.1", "--port", &port.to_string()])
            .arg("--data")
            .arg(dir.join("data"))
            .arg("--loc")
            .arg(format!("db={}", dir.join("db").display()))
            .arg("--mcp")
            .args(extra)
            .env_remove("SPARKLES_BACKUP_CONFIG")
            .env("RUST_LOG", "info")
            .stdout(Stdio::null())
            .stderr(std::fs::File::create(&log).unwrap())
            .spawn()
            .unwrap();
        let mut r = Running {
            server: Server(child),
            port,
            log: log.clone(),
        };
        let listening = format!("listening on http://127.0.0.1:{port}/");
        if r.wait_for(&listening, Duration::from_secs(60)).is_none() {
            continue;
        }
        r.wait_for("scratch-branch expiry: ", Duration::from_secs(30))
            .expect("the server exited before deciding about expiry");
        let t0 = Instant::now();
        let deadline = Duration::from_secs(60);
        while !r.ready(deadline.saturating_sub(t0.elapsed())) {
            assert!(r.alive(), "server exited\n{}", r.log());
            assert!(t0.elapsed() < deadline, "server not ready");
            std::thread::sleep(Duration::from_millis(50));
        }
        return r;
    }
    panic!("no server started in 5 attempts");
}

/// What a server started with `extra` over a database with the dataset setting `ttl`
/// decides and does. The log must say `decision` (`on` or `off`) about expiry. Deletion
/// is awaited for up to 30 s. When none is expected the branch is watched for 2 s, and
/// the child has to stay alive through that window.
fn expires(ttl: Option<&str>, extra: &[&str], decision: &str, deleted: bool) {
    let d = tempfile::tempdir().unwrap();
    database(d.path(), ttl);
    let mut r = serve(d.path(), extra);
    let said = format!("scratch-branch expiry: {decision} (");
    assert!(
        r.log().contains(&said),
        "serve --mcp {extra:?}, dataset ttl {ttl:?}: no {said:?} in\n{}",
        r.log()
    );
    let other = if decision == "on" { "off" } else { "on" };
    assert!(
        !r.log()
            .contains(&format!("scratch-branch expiry: {other} ("))
    );
    let wait = Duration::from_secs(if deleted { 30 } else { 2 });
    let t0 = Instant::now();
    while exists(d.path()) && t0.elapsed() < wait {
        assert!(r.alive(), "server exited\n{}", r.log());
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(r.alive(), "server exited\n{}", r.log());
    assert_eq!(
        !exists(d.path()),
        deleted,
        "serve --mcp {extra:?} with dataset ttl {ttl:?}"
    );
}

#[test]
fn serve_expires_scratch_branches_only_with_mcp_update_permission() {
    let flag = ["--mcp-scratch-branch-ttl", "1h"];
    let with_update = |mut v: Vec<&'static str>| {
        v.push("--mcp-allow-update");
        v
    };
    // the flag alone, and the dataset setting alone, without permission
    expires(None, &flag, "off", false);
    expires(Some("1h"), &[], "off", false);
    // and with it
    expires(None, &with_update(flag.to_vec()), "on", true);
    expires(Some("1h"), &with_update(vec![]), "on", true);
    // a dataset's off keeps its branch although the flag and the permission are there
    expires(Some("off"), &with_update(flag.to_vec()), "on", false);
}
