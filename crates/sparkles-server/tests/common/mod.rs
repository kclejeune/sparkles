//! Launch a CLI test server and learn the port from this child's own startup log.

use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom};
use std::ops::{Deref, DerefMut};
use std::path::Path;
use std::process::{Child, Command, Stdio};
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
        let mut server = Self(
            command
                .stdout(Stdio::null())
                .stderr(stderr)
                .spawn()
                .unwrap(),
        );
        let deadline = Instant::now() + Duration::from_secs(120);
        let mut bytes = Vec::new();
        loop {
            reader.read_to_end(&mut bytes).unwrap();
            let text = String::from_utf8_lossy(&bytes);
            if let Some(status) = server.try_wait().unwrap() {
                panic!(
                    "sparkles serve exited with {status} before reporting its listener:\n{text}"
                );
            }
            if let Some(port) = text.lines().find_map(|line| {
                let (_, address) = line.split_once(" listening on http://127.0.0.1:")?;
                let (port, _) = address.split_once("/ (UI at /ui/)")?;
                port.parse::<u16>().ok().filter(|&port| port != 0)
            }) {
                return (server, port);
            }
            assert!(
                Instant::now() < deadline,
                "sparkles serve did not report its listener:\n{text}"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }
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
