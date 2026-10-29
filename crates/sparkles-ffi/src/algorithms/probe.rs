//! Private, opt-in acknowledgement for an isolated JVM cancellation test.
use sparkles::task::{Control, Progress};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[derive(Default)]
struct State {
    entered: bool,
    outcome: &'static str,
    checked_close: bool,
}
pub(super) struct Probe {
    dir: PathBuf,
    state: Arc<Mutex<State>>,
    deadline_forwarded: bool,
}

impl Probe {
    /// The one-shot arm must match the exact request and author. An unrelated call cannot
    /// consume it. Only a feature-enabled isolated process can supply the directory.
    pub(super) fn install(
        original: &Control,
        request: &serde_json::Value,
        author: Option<&str>,
    ) -> io::Result<Option<(Self, Control)>> {
        let Some(dir) = std::env::var_os("SPARKLES_TEST_JVM_ALGORITHM_DIR").map(PathBuf::from)
        else {
            return Ok(None);
        };
        let arm_path = dir.join("armed.json");
        let bytes = match std::fs::read(&arm_path) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e),
        };
        let arm: serde_json::Value = serde_json::from_slice(&bytes)?;
        if author != Some("__sparkles_jvm_probe__")
            || arm.get("request") != Some(request)
            || arm.get("author").and_then(|v| v.as_str()) != author
        {
            return Ok(None);
        }
        let mode = match arm.get("mode").and_then(|v| v.as_str()) {
            Some("batch2") => "batch2",
            Some("done") => "done",
            _ => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "invalid private algorithm probe mode",
                ));
            }
        };
        let claimed = dir.join("claimed.json");
        if claimed.exists() {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "private algorithm probe already claimed",
            ));
        }
        std::fs::rename(&arm_path, &claimed)?;
        let state = Arc::new(Mutex::new(State {
            outcome: "not-reached",
            ..Default::default()
        }));
        let state_for_callback = state.clone();
        let dir_for_callback = dir.clone();
        let flag = original.cancel.flag();
        let forward = original.progress.clone();
        let deadline = original.deadline;
        let mut control = original.clone();
        let deadline_forwarded = control.deadline == deadline && deadline.is_some();
        control.progress = Progress::new(move |fraction, message| {
            forward.report(fraction, message);
            let at_boundary = if mode == "batch2" {
                message.starts_with("writing batch 2 of ")
            } else {
                message == "done"
            };
            if !at_boundary {
                return;
            }
            {
                let mut current = state_for_callback.lock().unwrap_or_else(|e| e.into_inner());
                if current.entered {
                    return;
                }
                current.entered = true;
            }
            if publish(&dir_for_callback, "entered", &serde_json::json!({"mode": mode, "deadlineForwarded": deadline_forwarded, "checkedClose": true})).is_err() {
                state_for_callback.lock().unwrap_or_else(|e| e.into_inner()).outcome = "file-error";
                flag.store(true, Ordering::SeqCst);
                return;
            }
            state_for_callback
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .checked_close = true;
            let until = Instant::now() + Duration::from_secs(10);
            let outcome = loop {
                match std::fs::metadata(dir_for_callback.join("release")) {
                    Ok(_) => break "released-by-test",
                    Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                    Err(_) => break "file-error",
                }
                if Instant::now() >= until {
                    break "safety-timeout";
                }
                std::thread::park_timeout(Duration::from_millis(2));
            };
            state_for_callback
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .outcome = outcome;
            if outcome != "released-by-test" {
                flag.store(true, Ordering::SeqCst);
            }
        });
        Ok(Some((
            Self {
                dir,
                state,
                deadline_forwarded,
            },
            control,
        )))
    }
    pub(super) fn finish(
        &self,
        result: &sparkles::Result<sparkles::sparql::algorun::RunReport>,
    ) -> io::Result<()> {
        let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let kind = match result {
            Ok(_) => "Ok",
            Err(sparkles::Error::Cancelled) => "Cancelled",
            Err(sparkles::Error::Timeout) => "Timeout",
            Err(_) => "OtherError",
        };
        publish(
            &self.dir,
            "finished",
            &serde_json::json!({"result": kind, "entered": state.entered, "outcome": state.outcome, "checkedClose": state.checked_close, "deadlineForwarded": self.deadline_forwarded}),
        )
    }
}
impl Drop for Probe {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(self.dir.join("claimed.json"));
    }
}

fn publish(dir: &Path, name: &str, value: &serde_json::Value) -> io::Result<()> {
    if !matches!(name, "entered" | "finished") {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid ACK name",
        ));
    }
    let destination = dir.join(format!("{name}.json"));
    if destination.exists() {
        return Err(io::Error::new(io::ErrorKind::AlreadyExists, "stale ACK"));
    }
    let temp = dir.join(format!(".{name}.json.tmp"));
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temp)?;
    let written = file.write_all(&serde_json::to_vec(value)?);
    let closed = nix::unistd::close(file).map_err(io::Error::from);
    let result = written.and(closed).and_then(|()| {
        if destination.exists() {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "duplicate ACK",
            ));
        }
        std::fs::rename(&temp, &destination)
    });
    if result.is_err() {
        let _ = std::fs::remove_file(temp);
    }
    result
}
