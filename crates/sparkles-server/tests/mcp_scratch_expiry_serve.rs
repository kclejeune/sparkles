//! Scratch-branch expiry through the real startup of `sparkles serve --mcp` (roadmap
//! item 57). The sweeper deletes branches, so it must run only when the operator allowed
//! MCP updates, whether the duration comes from `--mcp-scratch-branch-ttl` or from a
//! dataset's `memory.scratchBranches.ttl`.
#![cfg(all(feature = "mcp", unix))]

use serde_json::{Value, json};
use std::path::Path;
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

/// Start `sparkles serve` over `dir/db` and wait until this child reports that it is
/// listening. Another process may take the port between the probe and the child's bind,
/// and a ping could then reach that process. So readiness is the child's own log line
/// about the address it bound, and a child that exits first, as a failed bind does, is
/// started again on a new port. The sweeper's first pass starts before the line.
fn serve(dir: &Path, extra: &[&str]) -> Server {
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
        let mut s = Server(child);
        let listening = format!("listening on http://127.0.0.1:{port}/");
        let t0 = Instant::now();
        loop {
            if std::fs::read_to_string(&log).is_ok_and(|l| l.contains(&listening)) {
                assert!(s.0.try_wait().unwrap().is_none(), "server exited");
                return s;
            }
            if s.0.try_wait().unwrap().is_some() {
                break;
            }
            assert!(
                t0.elapsed() < Duration::from_secs(60),
                "server did not start"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
    }
    panic!("no server started in 5 attempts");
}

/// Whether the idle scratch branch is deleted by a server started with `extra` over a
/// database with the dataset setting `ttl`. Deletion is awaited for up to 30 s. When it
/// does not come, the branch is watched for 2 s after the server answered, which covers
/// the immediate first pass of the sweeper.
fn expires(ttl: Option<&str>, extra: &[&str], expect: bool) {
    let d = tempfile::tempdir().unwrap();
    database(d.path(), ttl);
    let _server = serve(d.path(), extra);
    let wait = Duration::from_secs(if expect { 30 } else { 2 });
    let t0 = Instant::now();
    while exists(d.path()) && t0.elapsed() < wait {
        std::thread::sleep(Duration::from_millis(50));
    }
    assert_eq!(
        !exists(d.path()),
        expect,
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
    expires(None, &flag, false);
    expires(Some("1h"), &[], false);
    // and with it
    expires(None, &with_update(flag.to_vec()), true);
    expires(Some("1h"), &with_update(vec![]), true);
    // a dataset's off keeps its branch although the flag and the permission are there
    expires(Some("off"), &with_update(flag.to_vec()), false);
}
