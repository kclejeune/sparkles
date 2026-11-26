//! `serve` spills by default (X08, item 93). With the default, a spill directory that
//! cannot be used is passed over with a warning and the server runs without spill. With
//! `--spill on` or `--spill encrypted` given, the same directory fails the start, and
//! `--spill off` turns spill off and leaves the directory unopened.

mod common;

use serde_json::Value as J;
use std::io::{Read, Write};
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

fn serve(data: &Path, extra: &[&str]) -> Command {
    let mut command = common::serve();
    command
        .arg("--data")
        .arg(data)
        .args(extra)
        .env_remove("SPARKLES_BACKUP_CONFIG")
        .env("RUST_LOG", "info");
    command
}

/// A server started by the test, stopped when dropped.
struct Server {
    _child: common::ServerProcess,
    port: u16,
}

impl Server {
    fn start(data: &Path, extra: &[&str], log: &Path) -> Server {
        let (child, port) = common::ServerProcess::start(&mut serve(data, extra), log);
        let s = Server {
            _child: child,
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

#[test]
fn spill_is_on_by_default_and_an_unusable_directory_only_fails_an_explicit_request() {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("data");
    let log = dir.path().join("server.log");
    // A plain file where the spill directory should go cannot be used.
    let unusable = dir.path().join("not-a-directory");
    std::fs::write(&unusable, b"").unwrap();
    let unusable = unusable.to_str().unwrap();

    {
        let s = Server::start(&data, &["--spill", "off"], &log);
        assert_eq!(s.spill()["enabled"], false, "{}", s.spill());
        assert!(!data.join(".spill").exists());
    }
    {
        let s = Server::start(&data, &[], &log);
        assert_eq!(s.spill()["enabled"], true, "{}", s.spill());
        assert!(data.join(".spill").is_dir());
    }
    {
        let s = Server::start(&data, &["--spill-dir", unusable], &log);
        assert_eq!(s.spill()["enabled"], false, "{}", s.spill());
        let text = std::fs::read_to_string(&log).unwrap();
        assert!(text.contains("running without spill"), "{text}");
    }
    for mode in ["on", "encrypted"] {
        let status = common::ServerProcess::spawn(
            &mut serve(&data, &["--spill", mode, "--spill-dir", unusable]),
            std::fs::File::create(&log).unwrap(),
        )
        .wait_for_exit(Duration::from_secs(30), &log);
        assert!(!status.success(), "--spill {mode}");
    }
    for mode in ["on", "encrypted"] {
        let s = Server::start(&data, &["--spill", mode], &log);
        assert_eq!(s.spill()["enabled"], true, "{}", s.spill());
    }
}
