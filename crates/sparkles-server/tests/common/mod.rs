//! Launch a CLI test server and learn the port from this child's own startup log.

use regex::Regex;
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom};
use std::net::SocketAddr;
use std::ops::{Deref, DerefMut};
use std::path::Path;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::LazyLock;
use std::time::{Duration, Instant};

pub fn serve() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_sparkles"));
    command
        .args(["serve", "--host", "127.0.0.1", "--port", "0"])
        .env(
            "RUST_LOG",
            "sparkles=info,sparkles_server=info,tower_http=warn",
        );
    command
}

pub struct ServerProcess(Child);

impl ServerProcess {
    pub fn spawn(command: &mut Command, stderr: File) -> Self {
        Self(
            command
                .env("NO_COLOR", "1")
                .stdout(Stdio::null())
                .stderr(stderr)
                .spawn()
                .unwrap(),
        )
    }

    #[allow(dead_code)] // Used only by suites that expect the server to exit.
    pub fn wait_for_exit(mut self, timeout: Duration, log: &Path) -> ExitStatus {
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(status) = self.try_wait().unwrap() {
                return status;
            }
            assert!(
                Instant::now() < deadline,
                "sparkles serve did not exit within {timeout:?}; see {log:?}"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// Wait for the bind, which can precede dataset readiness.
    pub fn start(command: &mut Command, log: &Path) -> (Self, u16) {
        let stderr = OpenOptions::new()
            .create(true)
            .append(true)
            .open(log)
            .unwrap();
        let mut reader = File::open(log).unwrap();
        // A restart may reuse the log. Read only what the new process writes.
        reader.seek(SeekFrom::End(0)).unwrap();
        let mut server = Self::spawn(command, stderr);
        let port = wait_for_listener(reader, || server.try_wait());
        (server, port)
    }
}

pub(super) fn wait_for_listener(
    mut reader: impl Read,
    mut try_wait: impl FnMut() -> std::io::Result<Option<ExitStatus>>,
) -> u16 {
    let deadline = Instant::now() + Duration::from_secs(120);
    let mut bytes = Vec::new();
    loop {
        reader.read_to_end(&mut bytes).unwrap();
        if let Some(status) = try_wait().unwrap() {
            reader.read_to_end(&mut bytes).unwrap();
            panic!(
                "sparkles serve exited with {status} before reporting its listener:\n{}",
                String::from_utf8_lossy(&bytes)
            );
        }
        let text = String::from_utf8_lossy(&bytes);
        if let Some(port) = listener_port(&text) {
            return port;
        }
        assert!(
            Instant::now() < deadline,
            "sparkles serve did not report its listener:\n{text}"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn listener_port(output: &str) -> Option<u16> {
    static RECORD: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(concat!(
            r"(?m)^[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9:.]+Z +INFO sparkles: Sparkles \S+",
            r" listening on http://(\S+)/ \(UI at /ui/\), opening [0-9]+ datasets\r?\n"
        ))
        .unwrap()
    });
    RECORD
        .captures_iter(output)
        .filter_map(|record| record[1].parse::<SocketAddr>().ok())
        .find(|addr| addr.ip().is_loopback() && addr.port() != 0)
        .map(|addr| addr.port())
}

impl Deref for ServerProcess {
    type Target = Child;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl DerefMut for ServerProcess {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl Drop for ServerProcess {
    fn drop(&mut self) {
        let _ = self.kill();
        let _ = self.wait();
    }
}
