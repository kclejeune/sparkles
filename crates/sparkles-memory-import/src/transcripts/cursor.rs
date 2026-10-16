//! Cursor's agent transcripts: `~/.cursor/projects/<project>/agent-transcripts/`, where
//! the project directory is the project's absolute path with every character other than a
//! letter or a digit turned into `-`, without the leading one. A transcript is either
//! `<id>/<id>.jsonl` or `<id>.jsonl`, with one entry per line that has a `role` of `user`
//! or `assistant` and a `message` whose `content` is a list of `text` and `tool_use`
//! blocks, or an older flat `<id>.txt`. The person's words are inside `<user_query>` tags,
//! next to context that Cursor attaches, and only the tagged text is kept. In the text
//! form a block starts with a line `user:` or `A:` (or `assistant:`), and tool calls,
//! results and reasoning start with `[Tool call]`, `[Tool result]` and `[Thinking]` and
//! continue on the indented lines under them.
//!
//! Cursor's chat history in its SQLite workspace databases is not read, because that
//! would need an SQLite library for a store that Cursor does not document.

use super::{Opts, Session, TOOL_RESULT_CHARS, cut, text_of};
use crate::{Project, Roots};
use serde_json::Value;
use std::path::PathBuf;

/// The transcripts of a project.
pub fn locate(roots: &Roots, p: &Project) -> Vec<PathBuf> {
    let projects = roots.cursor.join("projects");
    let mut dirs: Vec<PathBuf> = Vec::new();
    for d in [&p.dir, &p.root] {
        let name: String = d
            .to_string_lossy()
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
            .collect();
        let name = name.trim_start_matches('-').to_string();
        let cand = projects.join(name).join("agent-transcripts");
        if cand.is_dir() && !dirs.contains(&cand) {
            dirs.push(cand);
        }
    }
    let mut files = Vec::new();
    for d in dirs {
        let Ok(rd) = std::fs::read_dir(&d) else {
            continue;
        };
        for e in rd.flatten() {
            let path = e.path();
            let ext = path.extension().map(|x| x.to_string_lossy().into_owned());
            if path.is_file() && matches!(ext.as_deref(), Some("jsonl" | "txt")) {
                files.push(path);
            } else if path.is_dir() {
                // `<id>/<id>.jsonl`, and not the subagents' directory
                let id = e.file_name().to_string_lossy().into_owned();
                let f = path.join(format!("{id}.jsonl"));
                if f.is_file() {
                    files.push(f);
                }
            }
        }
    }
    files.sort();
    files
}

/// The text between the `<user_query>` tags of a message, or the whole text without any.
fn user_text(t: &str) -> String {
    let mut out = Vec::new();
    let mut rest = t;
    while let Some(a) = rest.find("<user_query>") {
        let after = &rest[a + "<user_query>".len()..];
        match after.find("</user_query>") {
            Some(b) => {
                out.push(after[..b].trim().to_string());
                rest = &after[b + "</user_query>".len()..];
            }
            None => {
                out.push(after.trim().to_string());
                break;
            }
        }
    }
    if out.is_empty() {
        t.to_string()
    } else {
        out.join("\n\n")
    }
}

/// A Cursor transcript, in either form.
pub fn parse(text: &str, o: &Opts) -> Session {
    let first = text.lines().find(|l| !l.trim().is_empty()).unwrap_or("");
    if serde_json::from_str::<Value>(first).is_ok_and(|j| j.is_object()) {
        jsonl(text, o)
    } else {
        flat(text, o)
    }
}

fn jsonl(text: &str, o: &Opts) -> Session {
    let mut s = Session::default();
    for line in text.lines() {
        let Ok(j) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let role = match j["role"].as_str().unwrap_or("") {
            "user" => "user",
            "assistant" => "assistant",
            _ => continue,
        };
        let mut texts: Vec<String> = Vec::new();
        match &j["message"]["content"] {
            Value::String(t) => texts.push(t.clone()),
            Value::Array(blocks) => {
                for b in blocks {
                    match b["type"].as_str().unwrap_or("") {
                        "text" => texts.push(b["text"].as_str().unwrap_or("").to_string()),
                        "tool_use" if o.tools => texts.push(format!(
                            "Tool call {}: {}",
                            b["name"].as_str().unwrap_or("tool"),
                            cut(&b["input"].to_string(), TOOL_RESULT_CHARS)
                        )),
                        "tool_result" if o.tools => s.push(
                            "tool",
                            None,
                            cut(&text_of(&b["content"]), TOOL_RESULT_CHARS),
                        ),
                        _ => {}
                    }
                }
            }
            _ => {}
        }
        let joined = texts.join("\n\n");
        let joined = if role == "user" {
            user_text(&joined)
        } else {
            joined
        };
        s.push(role, None, joined);
    }
    s
}

#[derive(PartialEq, Clone, Copy)]
enum Mode {
    Text,
    /// reasoning, tool calls and results: kept only for calls and results with tools
    Skip,
    Call,
    Result,
}

fn flat(text: &str, o: &Opts) -> Session {
    let mut s = Session::default();
    let mut role: Option<&str> = None;
    let mut mode = Mode::Text;
    let mut buf: Vec<String> = Vec::new();
    let flush = |s: &mut Session, role: Option<&str>, mode: Mode, buf: &mut Vec<String>| {
        let t = buf.join("\n");
        buf.clear();
        let Some(r) = role else { return };
        match mode {
            Mode::Text if r == "user" => s.push("user", None, user_text(&t)),
            Mode::Text => s.push("assistant", None, t),
            Mode::Call if o.tools => s.push(
                "assistant",
                None,
                format!("Tool call {}", cut(t.trim(), TOOL_RESULT_CHARS)),
            ),
            Mode::Result if o.tools => s.push("tool", None, cut(t.trim(), TOOL_RESULT_CHARS)),
            _ => {}
        }
    };
    for line in text.lines() {
        let t = line.trim_start();
        let lower = t.to_ascii_lowercase();
        let marker = if lower.starts_with("user:") && line.len() == t.len() {
            Some(("user", &t[5..]))
        } else if lower.starts_with("assistant:") && line.len() == t.len() {
            Some(("assistant", &t[10..]))
        } else if t.starts_with("A:") && line.len() == t.len() {
            Some(("assistant", &t[2..]))
        } else {
            None
        };
        if let Some((r, rest)) = marker {
            flush(&mut s, role, mode, &mut buf);
            role = Some(r);
            mode = Mode::Text;
            if !rest.trim().is_empty() {
                buf.push(rest.trim().to_string());
            }
            continue;
        }
        let special = if lower.starts_with("[thinking]") {
            Some(Mode::Skip)
        } else if lower.starts_with("[tool call]") {
            Some(Mode::Call)
        } else if lower.starts_with("[tool result]") {
            Some(Mode::Result)
        } else {
            None
        };
        if let Some(m) = special {
            flush(&mut s, role, mode, &mut buf);
            mode = m;
            let rest = t.split_once(']').map_or("", |x| x.1).trim();
            if !rest.is_empty() {
                buf.push(rest.to_string());
            }
            continue;
        }
        // a block of reasoning, a call or a result ends at the first line that is not
        // indented
        if mode != Mode::Text && !line.is_empty() && line.len() == t.len() {
            flush(&mut s, role, mode, &mut buf);
            mode = Mode::Text;
        }
        buf.push(line.to_string());
    }
    flush(&mut s, role, mode, &mut buf);
    s
}
