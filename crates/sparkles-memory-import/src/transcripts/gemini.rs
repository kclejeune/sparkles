//! Gemini CLI's chat recordings: `~/.gemini/tmp/<project hash>/chats/session-<time>-<id>.jsonl`.
//! The project hash is the SHA-256 of the project's root path in hexadecimal, and
//! `GEMINI_CLI_HOME` moves `~/.gemini`. A recording is JSONL. Its first record holds
//! `sessionId`, `projectHash`, `startTime`, `lastUpdated` and, for newer versions,
//! `directories`. Each message record has `id`, `timestamp`, `type` (`user` or `gemini`,
//! and `info`, `error` or `warning` for the client's own notices), `content` (a string, a
//! part with `text`, or a list of parts), `model`, `thoughts` and `toolCalls`. Records with
//! a leading `$` edit the log: `$rewindTo` drops the message with that id and the ones
//! after it, and `$patch` and `$set` change metadata. Older versions wrote one JSON object
//! with a `messages` array to a `.json` file, and `/chat save` writes a checkpoint, a
//! list of `{role, parts}` contents. All three are read.

use super::{Opts, Session, TOOL_RESULT_CHARS, cut, set, text_of};
use crate::{Project, Roots};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::path::PathBuf;

/// The transcripts of a project: the files of its `chats` directory.
pub fn locate(roots: &Roots, p: &Project) -> Vec<PathBuf> {
    let tmp = roots.gemini.join("tmp");
    let mut dirs: Vec<PathBuf> = Vec::new();
    for d in [&p.root, &p.dir] {
        let h = Sha256::digest(d.to_string_lossy().as_bytes());
        let hex: String = h.iter().map(|b| format!("{b:02x}")).collect();
        let c = tmp.join(hex).join("chats");
        if c.is_dir() && !dirs.contains(&c) {
            dirs.push(c);
        }
    }
    // newer versions may name the directory after the project and record its root
    if let Ok(rd) = std::fs::read_dir(&tmp) {
        for e in rd.flatten() {
            let root_file = e.path().join(".project_root");
            let chats = e.path().join("chats");
            if chats.is_dir()
                && !dirs.contains(&chats)
                && std::fs::read_to_string(&root_file).is_ok_and(|r| {
                    [&p.root, &p.dir]
                        .iter()
                        .any(|d| r.trim() == d.to_string_lossy())
                })
            {
                dirs.push(chats);
            }
        }
    }
    let mut files = Vec::new();
    for d in dirs {
        if let Ok(rd) = std::fs::read_dir(d) {
            files.extend(rd.flatten().map(|e| e.path()).filter(|f| {
                f.is_file() && f.extension().is_some_and(|x| x == "jsonl" || x == "json")
            }));
        }
    }
    files.sort();
    files
}

/// A Gemini CLI recording.
pub fn parse(text: &str, o: &Opts) -> Session {
    let mut s = Session::default();
    let trimmed = text.trim_start();
    let mut messages: Vec<Value> = Vec::new();
    // a whole-file JSON value: the older recording, or a checkpoint
    let whole = if trimmed.starts_with('[') || trimmed.starts_with('{') {
        serde_json::from_str::<Value>(text).ok()
    } else {
        None
    };
    match whole {
        Some(Value::Array(contents)) => return checkpoint(&contents, o),
        Some(j) if j["messages"].is_array() => {
            meta(&mut s, &j);
            messages = j["messages"].as_array().cloned().unwrap_or_default();
        }
        _ => {
            for line in text.lines() {
                let Ok(j) = serde_json::from_str::<Value>(line) else {
                    continue;
                };
                if let Some(id) = j["$rewindTo"].as_str() {
                    if let Some(i) = messages.iter().position(|m| m["id"] == id) {
                        messages.truncate(i);
                    }
                } else if j.get("$patch").is_some() || j.get("$set").is_some() {
                    continue;
                } else if j.get("type").is_none() && j.get("sessionId").is_some() {
                    meta(&mut s, &j);
                } else if j.get("type").is_some() {
                    messages.push(j);
                }
            }
        }
    }
    for m in &messages {
        let time = m["timestamp"].as_str();
        let role = match m["type"].as_str().unwrap_or("") {
            "user" => "user",
            "gemini" => "assistant",
            _ => continue,
        };
        s.saw(time);
        if role == "assistant" {
            set(&mut s.model, m["model"].as_str());
        }
        s.push(role, time, text_of(&m["content"]));
        if o.tools {
            for t in m["toolCalls"].as_array().into_iter().flatten() {
                let name = t["name"].as_str().unwrap_or("tool");
                let args = if t["args"].is_null() {
                    String::new()
                } else {
                    format!(" {}", t["args"])
                };
                s.push(
                    "assistant",
                    time,
                    format!("Tool call {name}:{}", cut(&args, TOOL_RESULT_CHARS)),
                );
                let r = tool_result(&t["result"]);
                if !r.is_empty() {
                    s.push("tool", time, cut(&r, TOOL_RESULT_CHARS));
                }
            }
        }
    }
    s
}

fn meta(s: &mut Session, j: &Value) {
    set(&mut s.id, j["sessionId"].as_str());
    set(&mut s.started, j["startTime"].as_str());
    set(&mut s.ended, j["lastUpdated"].as_str());
    set(
        &mut s.cwd,
        j["directories"]
            .as_array()
            .and_then(|d| d.first())
            .and_then(Value::as_str),
    );
}

/// The text of a tool call's result: parts with text, or function responses.
fn tool_result(r: &Value) -> String {
    match r {
        Value::Array(a) => a
            .iter()
            .map(|p| {
                let o = &p["functionResponse"]["response"]["output"];
                match o {
                    Value::String(s) => s.clone(),
                    _ => text_of(p),
                }
            })
            .filter(|t| !t.is_empty())
            .collect::<Vec<_>>()
            .join("\n"),
        v => text_of(v),
    }
}

/// A `/chat save` checkpoint: contents with a `role` of `user` or `model` and `parts`.
fn checkpoint(contents: &[Value], o: &Opts) -> Session {
    let mut s = Session::default();
    for c in contents {
        let role = match c["role"].as_str().unwrap_or("") {
            "user" => "user",
            "model" => "assistant",
            _ => continue,
        };
        let mut texts = Vec::new();
        for p in c["parts"].as_array().into_iter().flatten() {
            if let Some(t) = p["text"].as_str() {
                texts.push(t.to_string());
            } else if o.tools && p.get("functionCall").is_some() {
                texts.push(format!(
                    "Tool call {}: {}",
                    p["functionCall"]["name"].as_str().unwrap_or("tool"),
                    cut(&p["functionCall"]["args"].to_string(), TOOL_RESULT_CHARS)
                ));
            }
        }
        s.push(role, None, texts.join("\n\n"));
    }
    s
}
