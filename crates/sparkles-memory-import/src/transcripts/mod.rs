//! Session transcripts (spec C18 §8.10.7): one module for each harness's log format.
//!
//! Each adapter turns a session log into a [`Session`] of turns, and [`render`] writes
//! the turns as Markdown with one `## user` or `## assistant` heading per turn. The
//! command line redacts the rendering and registers it. By default a session keeps the
//! text of the person's and the assistant's messages on the main chain. Thinking and
//! reasoning, tool calls and results, attachments and bookkeeping are left out, and
//! `Opts::tools` adds tool calls and the first 2,000 characters of each result.
//!
//! | Adapter | Log | Locations |
//! |---|---|---|
//! | [`claude`] | JSONL, one entry per line | `~/.claude/projects/<project>/<session>.jsonl` |
//! | [`codex`] | JSONL with `session_meta` and `response_item` | `~/.codex/sessions/YYYY/MM/DD/rollout-*.jsonl` |
//! | [`gemini`] | JSONL of records, or the older single JSON object | `~/.gemini/tmp/<project hash>/chats/session-*.jsonl` |
//! | [`cursor`] | JSONL of `role` entries, or the older text form | `~/.cursor/projects/<project>/agent-transcripts/` |
//! | [`copilot`] | the CLI's `events.jsonl` | `~/.copilot/session-state/<session>/events.jsonl` |
//! | [`aider`] | Markdown history | `.aider.chat.history.md` in the project |
//! | [`generic`] | any of the shapes above that it can recognize | a file given by name |
//!
//! Only Claude Code's and Codex's formats are documented by their makers. The others
//! follow the vendors' published behavior and the fields that other tools read, and the
//! adapters skip a line or a block they do not understand instead of failing.

pub mod aider;
pub mod claude;
pub mod codex;
pub mod copilot;
pub mod cursor;
pub mod gemini;
pub mod generic;

use serde_json::Value;
use std::path::Path;

/// The characters of a tool result kept with `Opts::tools`.
pub const TOOL_RESULT_CHARS: usize = 2000;

/// What a parse keeps.
#[derive(Clone, Copy, Debug, Default)]
pub struct Opts {
    /// tool calls and the start of each tool result
    pub tools: bool,
    /// the lines of a subagent's file, which are all on its own chain (Claude Code)
    pub sidechain: bool,
}

/// One turn of a session.
#[derive(Clone, Debug, PartialEq)]
pub struct Turn {
    /// `user`, `assistant` or `tool`
    pub role: String,
    pub time: Option<String>,
    pub text: String,
}

/// A parsed session.
#[derive(Clone, Debug, Default)]
pub struct Session {
    pub id: Option<String>,
    pub cwd: Option<String>,
    pub branch: Option<String>,
    pub model: Option<String>,
    pub version: Option<String>,
    pub started: Option<String>,
    pub ended: Option<String>,
    pub turns: Vec<Turn>,
    /// the files the session wrote with `Write`, `Edit`, `MultiEdit` or `NotebookEdit`
    pub writes: Vec<String>,
}

impl Session {
    pub(crate) fn push(&mut self, role: &str, time: Option<&str>, text: String) {
        let text = text.trim().to_string();
        if text.is_empty() {
            return;
        }
        match self.turns.last_mut() {
            Some(t) if t.role == role => {
                t.text.push_str("\n\n");
                t.text.push_str(&text);
            }
            _ => self.turns.push(Turn {
                role: role.to_string(),
                time: time.map(str::to_string),
                text,
            }),
        }
    }

    pub(crate) fn saw(&mut self, time: Option<&str>) {
        if let Some(t) = time {
            if self.started.is_none() {
                self.started = Some(t.to_string());
            }
            self.ended = Some(t.to_string());
        }
    }
}

/// The text cut to `n` characters, with an ellipsis when it was longer.
pub(crate) fn cut(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        let mut t: String = s.chars().take(n).collect();
        t.push_str(" …");
        t
    }
}

/// Fill a slot once, with a non-empty value.
pub(crate) fn set(slot: &mut Option<String>, v: Option<&str>) {
    if slot.is_none()
        && let Some(v) = v.filter(|v| !v.is_empty())
    {
        *slot = Some(v.to_string());
    }
}

/// The text of a content value: a string, a part with `text`, or a list of those.
pub(crate) fn text_of(c: &Value) -> String {
    match c {
        Value::String(s) => s.clone(),
        Value::Array(a) => a
            .iter()
            .map(text_of)
            .filter(|t| !t.is_empty())
            .collect::<Vec<_>>()
            .join("\n"),
        Value::Object(o) => o
            .get("text")
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or_default(),
        _ => String::new(),
    }
}

/// The session as Markdown, one string per turn with its heading and time. A line of a
/// message that starts with `#` is escaped, so only the turn headings are headings.
pub fn render(s: &Session) -> Vec<String> {
    s.turns
        .iter()
        .map(|t| {
            let mut out = format!("## {}", t.role);
            if let Some(time) = &t.time {
                out.push(' ');
                out.push_str(time);
            }
            out.push_str("\n\n");
            for l in t.text.lines() {
                if l.trim_start().starts_with('#') {
                    out.push('\\');
                }
                out.push_str(l);
                out.push('\n');
            }
            out.push('\n');
            out
        })
        .collect()
}

/// The longest prefix of `s` of at most `n` bytes that ends on a character boundary.
fn prefix_bytes(s: &str, n: usize) -> &str {
    if s.len() <= n {
        return s;
    }
    let mut i = n;
    while !s.is_char_boundary(i) {
        i -= 1;
    }
    &s[..i]
}

/// Split rendered turns into parts of at most `max` bytes at turn boundaries, at most
/// `parts` of them. What does not fit is left out with a note at the end of the last
/// part.
pub fn split(turns: &[String], max: usize, parts: usize) -> Vec<String> {
    let mut out: Vec<String> = vec![String::new()];
    let mut left = 0;
    for (i, t) in turns.iter().enumerate() {
        let t = if t.len() > max {
            let mut c = prefix_bytes(t, max - 64).to_string();
            c.push_str("\n[the rest of this turn is left out]\n\n");
            c
        } else {
            t.clone()
        };
        let cur = out.last_mut().expect("one part");
        if cur.len() + t.len() <= max {
            cur.push_str(&t);
            continue;
        }
        if out.len() == parts {
            left = turns.len() - i;
            break;
        }
        out.push(t);
    }
    if left > 0 {
        let last = out.last_mut().expect("one part");
        last.push_str(&format!(
            "[{left} more turns of this session are left out of the import]\n"
        ));
    }
    out.retain(|p| !p.is_empty());
    out
}

/// The log format of a transcript file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    Claude,
    Codex,
    Gemini,
    Cursor,
    Copilot,
    Aider,
    Generic,
}

impl Format {
    /// The harness a format belongs to.
    pub fn harness(self) -> crate::Harness {
        match self {
            Format::Claude => crate::Harness::ClaudeCode,
            Format::Codex => crate::Harness::Codex,
            Format::Gemini => crate::Harness::GeminiCli,
            Format::Cursor => crate::Harness::Cursor,
            Format::Copilot => crate::Harness::Copilot,
            Format::Aider => crate::Harness::Aider,
            Format::Generic => crate::Harness::Generic,
        }
    }

    /// The format of a harness's transcripts.
    pub fn of(h: crate::Harness) -> Format {
        match h {
            crate::Harness::ClaudeCode => Format::Claude,
            crate::Harness::Codex => Format::Codex,
            crate::Harness::GeminiCli => Format::Gemini,
            crate::Harness::Cursor => Format::Cursor,
            crate::Harness::Copilot => Format::Copilot,
            crate::Harness::Aider => Format::Aider,
            crate::Harness::Generic => Format::Generic,
        }
    }

    /// Parse a log.
    pub fn parse(self, text: &str, o: &Opts) -> Session {
        match self {
            Format::Claude => claude::parse(text, o),
            Format::Codex => codex::parse(text, o),
            Format::Gemini => gemini::parse(text, o),
            Format::Cursor => cursor::parse(text, o),
            Format::Copilot => copilot::parse(text, o),
            Format::Aider => aider::parse(text, o),
            Format::Generic => generic::parse(text, o),
        }
    }
}

/// The format of a transcript file the person named, from its path and its first lines.
/// A file that none of the specific formats claims is [`Format::Generic`].
pub fn detect(path: &Path, text: &str) -> Format {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let under = |dir: &str| path.components().any(|c| c.as_os_str() == dir);
    if name.starts_with(".aider.chat.history")
        || text.contains("\n# aider chat started at ")
        || text.starts_with("# aider chat started at ")
    {
        return Format::Aider;
    }
    let first: Vec<Value> = text
        .lines()
        .filter(|l| !l.trim().is_empty())
        .take(5)
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect();
    if first.iter().any(|j| j["type"] == "session_meta") {
        return Format::Codex;
    }
    if name == "events.jsonl"
        || first.iter().any(|j| {
            j["type"]
                .as_str()
                .is_some_and(|t| t.starts_with("session."))
        })
    {
        return Format::Copilot;
    }
    if under(".gemini")
        || first
            .iter()
            .any(|j| j.get("projectHash").is_some() && j.get("sessionId").is_some())
    {
        return Format::Gemini;
    }
    if under("agent-transcripts") || under(".cursor") {
        return Format::Cursor;
    }
    if first.iter().any(|j| {
        j["sessionId"].is_string()
            && matches!(j["type"].as_str(), Some("user" | "assistant" | "summary"))
    }) {
        return Format::Claude;
    }
    Format::Generic
}

#[cfg(test)]
mod tests;
