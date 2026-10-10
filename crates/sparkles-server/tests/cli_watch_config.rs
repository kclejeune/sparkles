//! `serve --watch-config` with a server process: a model configuration mounted the way
//! Kubernetes mounts a ConfigMap is reloaded when the volume's `..data` symlink moves to
//! a new version, and a version that does not load keeps the running configuration.

#![cfg(unix)]

use serde_json::{Value as J, json};
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const BIN: &str = env!("CARGO_BIN_EXE_sparkles");

struct Server {
    child: Child,
    port: u16,
}

impl Server {
    /// Start on the first free port of `ports`.
    fn start(
        ports: std::ops::Range<u16>,
        data: &Path,
        extra: &[&str],
        log: &Path,
        env: &[(&str, &str)],
    ) -> Server {
        let port = ports
            .clone()
            .find(|p| TcpListener::bind(("127.0.0.1", *p)).is_ok())
            .unwrap_or_else(|| panic!("a free port in {ports:?}"));
        let log = std::fs::File::create(log).unwrap();
        let child = Command::new(BIN)
            .args(["serve", "--host", "127.0.0.1", "--port", &port.to_string()])
            .arg("--data")
            .arg(data)
            .args(extra)
            .envs(env.iter().copied())
            .env_remove("SPARKLES_BACKUP_CONFIG")
            .env("RUST_LOG", "info")
            .stdout(Stdio::null())
            .stderr(log)
            .spawn()
            .unwrap();
        let s = Server { child, port };
        let t0 = Instant::now();
        while s.get("/$/ping").0 != 200 {
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

    fn providers(&self) -> Vec<String> {
        let (_, v) = self.get("/$/models");
        v["providers"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|p| p["name"].as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default()
    }

    fn running(&mut self) -> bool {
        self.child.try_wait().unwrap().is_none()
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn eventually(what: &str, f: impl Fn() -> bool) {
    let t0 = Instant::now();
    while !f() {
        assert!(t0.elapsed() < Duration::from_secs(20), "timed out: {what}");
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn models(names: &[&str]) -> String {
    let providers: serde_json::Map<String, J> = names
        .iter()
        .map(|n| {
            (
                n.to_string(),
                json!({ "kind": "ollama", "endpoint": "http://127.0.0.1:9" }),
            )
        })
        .collect();
    json!({ "models": { "providers": providers } }).to_string()
}

/// A directory laid out as kubelet lays out a ConfigMap volume, with `models.json`.
struct ConfigMap<'a> {
    root: &'a Path,
    version: usize,
}

impl ConfigMap<'_> {
    fn publish(&mut self, body: &str) {
        self.version += 1;
        let dir = format!("..v{}", self.version);
        std::fs::create_dir(self.root.join(&dir)).unwrap();
        std::fs::write(self.root.join(&dir).join("models.json"), body).unwrap();
        let tmp = self.root.join("..data_tmp");
        std::os::unix::fs::symlink(&dir, &tmp).unwrap();
        std::fs::rename(&tmp, self.root.join("..data")).unwrap();
        if self.version == 1 {
            std::os::unix::fs::symlink("..data/models.json", self.root.join("models.json"))
                .unwrap();
        }
    }
}

#[test]
fn watch_config_reloads_a_configmap_and_keeps_a_bad_version_out() {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("data");
    let cm_dir = dir.path().join("config");
    std::fs::create_dir(&cm_dir).unwrap();
    let mut cm = ConfigMap {
        root: &cm_dir,
        version: 0,
    };
    cm.publish(&models(&["first"]));
    let log = dir.path().join("server.log");
    let cfg = cm_dir.join("models.json");
    let mut s = Server::start(
        48050..48060,
        &data,
        &["--model-config", cfg.to_str().unwrap()],
        &log,
        &[("SPARKLES_WATCH_CONFIG", "1")],
    );
    assert_eq!(s.providers(), ["first"]);

    // a new version of the volume
    cm.publish(&models(&["first", "second"]));
    eventually("the new configuration", || {
        s.providers().contains(&"second".to_string())
    });

    // a version that does not parse is logged, and the configuration stays
    cm.publish("{ not json");
    let text = || std::fs::read_to_string(&log).unwrap();
    eventually("the reload error", || {
        text().contains("model configuration not reloaded")
    });
    assert!(s.running());
    assert_eq!(s.providers(), ["first", "second"]);

    // a fixed version loads again
    cm.publish(&models(&["third"]));
    eventually("the fixed configuration", || s.providers() == ["third"]);
    let text = text();
    assert!(text.contains("watching"), "{text}");
    assert!(
        text.contains("configuration changed") && text.contains("models.json"),
        "{text}"
    );
}

#[test]
fn without_watch_config_a_change_is_not_read() {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("data");
    let cfg = dir.path().join("models.json");
    std::fs::write(&cfg, models(&["first"])).unwrap();
    let log = dir.path().join("server.log");
    let s = Server::start(
        48060..48070,
        &data,
        &["--model-config", cfg.to_str().unwrap()],
        &log,
        &[("SPARKLES_WATCH_CONFIG", "0")],
    );
    std::fs::write(&cfg, models(&["second"])).unwrap();
    std::thread::sleep(Duration::from_secs(5));
    assert_eq!(s.providers(), ["first"]);
}
