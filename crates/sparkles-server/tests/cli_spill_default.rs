//! `serve` keeps spill opt-in until every X08 Phase 2b gate passes. Enabling spill
//! explicitly requires a usable directory; an ordinary start leaves it unopened.

use serde_json::Value as J;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const BIN: &str = env!("CARGO_BIN_EXE_sparkles");
const PORTS: std::ops::Range<u16> = 47900..47920;

/// The `sparkles serve` command on a free port, with its log in `log`.
fn serve(data: &Path, extra: &[&str], log: &Path) -> (Command, u16) {
    let port = PORTS
        .clone()
        .find(|p| TcpListener::bind(("127.0.0.1", *p)).is_ok())
        .unwrap_or_else(|| panic!("a free port in {PORTS:?}"));
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log)
        .unwrap();
    let mut command = Command::new(BIN);
    command
        .args(["serve", "--host", "127.0.0.1", "--port", &port.to_string()])
        .arg("--data")
        .arg(data)
        .args(extra)
        .env_remove("SPARKLES_BACKUP_CONFIG")
        .env("RUST_LOG", "info")
        .stdout(Stdio::null())
        .stderr(log);
    (command, port)
}

/// A server started by the test, stopped when dropped.
struct Server {
    child: Child,
    port: u16,
}

impl Server {
    fn start(data: &Path, extra: &[&str], log: &Path) -> Server {
        let (mut command, port) = serve(data, extra, log);
        let s = Server {
            child: command.spawn().unwrap(),
            port,
        };
        let t0 = Instant::now();
        while s.get("/$/ready").0 != 200 {
            assert!(
                t0.elapsed() < Duration::from_secs(30),
                "server did not start"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
        s
    }

    fn get(&self, path: &str) -> (u16, J) {
        let Ok(mut c) = std::net::TcpStream::connect(("127.0.0.1", self.port)) else {
            return (0, J::Null);
        };
        let _ = write!(c, "GET {path} HTTP/1.0\r\nHost: 127.0.0.1\r\n\r\n");
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

    fn spill(&self) -> J {
        let (status, memory) = self.get("/$/server/memory");
        assert_eq!(status, 200, "{memory}");
        memory["spill"].clone()
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
fn spill_is_opt_in_and_an_unusable_directory_fails_when_requested() {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("data");
    let log = dir.path().join("server.log");
    // A plain file where the spill directory should go cannot be used.
    let unusable = dir.path().join("not-a-directory");
    std::fs::write(&unusable, b"").unwrap();
    let unusable = unusable.to_str().unwrap();

    {
        let s = Server::start(&data, &[], &log);
        assert_eq!(s.spill()["enabled"], false, "{}", s.spill());
        assert!(!data.join(".spill").exists());
    }
    {
        let s = Server::start(&data, &["--spill-dir", unusable], &log);
        assert_eq!(s.spill()["enabled"], false, "{}", s.spill());
        assert!(!data.join(".spill").exists());
    }
    {
        let s = Server::start(&data, &["--spill", "off"], &log);
        assert_eq!(s.spill()["enabled"], false, "{}", s.spill());
    }
    for mode in ["on", "encrypted"] {
        let (mut command, _) = serve(&data, &["--spill", mode, "--spill-dir", unusable], &log);
        let mut child = command.spawn().unwrap();
        let t0 = Instant::now();
        let status = loop {
            if let Some(status) = child.try_wait().unwrap() {
                break status;
            }
            if t0.elapsed() > Duration::from_secs(30) {
                let _ = child.kill();
                panic!("--spill {mode} with an unusable directory did not fail the start");
            }
            std::thread::sleep(Duration::from_millis(50));
        };
        assert!(!status.success(), "--spill {mode}");
    }
    for mode in ["on", "encrypted"] {
        let s = Server::start(&data, &["--spill", mode], &log);
        assert_eq!(s.spill()["enabled"], true, "{}", s.spill());
    }
}
