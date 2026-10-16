//! GitHub Copilot CLI's session logs: `~/.copilot/session-state/<session>/events.jsonl`
//! next to a `workspace.yaml` with the session's `cwd` and git state, and `COPILOT_HOME`
//! moves `~/.copilot`. GitHub does not document the log. Each line is an event with
//! `type`, `timestamp` and a `data` object, and the reader also accepts the members
//! inline. `session.start` carries the session id, the CLI's version, the start time and
//! a context with `cwd`, `gitRoot` and `branch`. `user.message` has the person's text in
//! `content`, `assistant.message` has `content`, `reasoningText` (never kept) and
//! `toolRequests`, and `tool.execution_complete` has the result. Copilot Chat in an editor
//! keeps its history in the editor's workspace storage and is not read.

use super::{Opts, Session, TOOL_RESULT_CHARS, cut, set, text_of};
use crate::{Project, Roots};
use serde_json::Value;
use std::path::{Path, PathBuf};

/// The `events.jsonl` files of the sessions whose working directory lies in the project.
pub fn locate(roots: &Roots, p: &Project) -> Vec<PathBuf> {
    let Ok(rd) = std::fs::read_dir(roots.copilot.join("session-state")) else {
        return Vec::new();
    };
    let mut files: Vec<PathBuf> = rd
        .flatten()
        .map(|e| e.path().join("events.jsonl"))
        .filter(|f| f.is_file())
        .filter(|f| session_cwd(f).is_some_and(|c| Path::new(&c).starts_with(&p.root)))
        .collect();
    files.sort();
    files
}

/// The working directory of a session: `workspace.yaml`'s `cwd`, else the context of the
/// first lines of the log.
pub fn session_cwd(events: &Path) -> Option<String> {
    if let Ok(y) = std::fs::read_to_string(events.with_file_name("workspace.yaml")) {
        for line in y.lines() {
            if let Some(v) = line.strip_prefix("cwd:") {
                let v = v.trim().trim_matches(['"', '\'']);
                if !v.is_empty() {
                    return Some(v.to_string());
                }
            }
        }
    }
    use std::io::BufRead;
    let f = std::fs::File::open(events).ok()?;
    for line in std::io::BufReader::new(f).lines().take(5) {
        let j: Value = serde_json::from_str(&line.ok()?).ok()?;
        let d = data(&j);
        if let Some(c) = d["context"]["cwd"].as_str().or(d["cwd"].as_str()) {
            return Some(c.to_string());
        }
    }
    None
}

/// The event's payload: `data`, or the event itself.
fn data(j: &Value) -> &Value {
    if j["data"].is_object() { &j["data"] } else { j }
}

/// A Copilot CLI event log.
pub fn parse(text: &str, o: &Opts) -> Session {
    let mut s = Session::default();
    for line in text.lines() {
        let Ok(j) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let d = data(&j);
        let time = j["timestamp"].as_str().or(d["timestamp"].as_str());
        match j["type"].as_str().unwrap_or("") {
            "session.start" => {
                set(&mut s.id, d["sessionId"].as_str());
                set(&mut s.version, d["copilotVersion"].as_str());
                set(&mut s.model, d["selectedModel"].as_str());
                set(&mut s.started, d["startTime"].as_str().or(time));
                set(&mut s.cwd, d["context"]["cwd"].as_str());
                set(&mut s.branch, d["context"]["branch"].as_str());
            }
            "session.model_change" => set(&mut s.model, d["newModel"].as_str()),
            "user.message" => {
                s.saw(time);
                s.push("user", time, text_of(&d["content"]));
            }
            "assistant.message" => {
                s.saw(time);
                s.push("assistant", time, text_of(&d["content"]));
                if o.tools {
                    for r in d["toolRequests"].as_array().into_iter().flatten() {
                        let args = r["arguments"]
                            .as_str()
                            .map(str::to_string)
                            .unwrap_or_else(|| r["arguments"].to_string());
                        s.push(
                            "assistant",
                            time,
                            format!(
                                "Tool call {}: {}",
                                r["name"].as_str().unwrap_or("tool"),
                                cut(&args, TOOL_RESULT_CHARS)
                            ),
                        );
                    }
                }
            }
            "tool.execution_complete" if o.tools => {
                let out = text_of(&d["result"]["content"]);
                s.push("tool", time, cut(&out, TOOL_RESULT_CHARS));
            }
            _ => {}
        }
    }
    s
}
