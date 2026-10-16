//! Aider's chat history: `.aider.chat.history.md` in the project, or the file that
//! `--chat-history-file` and `AIDER_CHAT_HISTORY_FILE` name. Aider appends to it for every
//! run. A run starts with a line `# aider chat started at YYYY-MM-DD HH:MM:SS`. The
//! person's input follows as lines that start with `#### `, Aider's notices (files added,
//! edits applied, costs) are lines that start with `> `, and every other line is the
//! model's reply in Markdown. The times have no zone, so they are kept as written. The
//! separate `.aider.input.history` repeats the person's prompts and is not read.
//!
//! A file is one session, because Aider keeps one file for the project, and a turn's time
//! is the start of its run.

use super::{Opts, Session, TOOL_RESULT_CHARS, cut};
use crate::{Project, Roots};
use std::path::PathBuf;

const STARTED: &str = "# aider chat started at ";

/// The history file of a project.
pub fn locate(_roots: &Roots, p: &Project) -> Vec<PathBuf> {
    let mut names: Vec<PathBuf> = Vec::new();
    if let Some(n) = std::env::var_os("AIDER_CHAT_HISTORY_FILE").filter(|n| !n.is_empty()) {
        for d in [&p.dir, &p.root] {
            names.push(d.join(&n));
        }
    }
    for d in [&p.dir, &p.root] {
        names.push(d.join(".aider.chat.history.md"));
    }
    let mut out: Vec<PathBuf> = Vec::new();
    for n in names {
        if n.is_file() && !out.contains(&n) {
            out.push(n);
        }
    }
    out
}

/// `2026-10-01 10:00:00` as `2026-10-01T10:00:00`.
fn iso(t: &str) -> String {
    t.trim().replacen(' ', "T", 1)
}

/// An Aider chat history.
pub fn parse(text: &str, o: &Opts) -> Session {
    #[derive(PartialEq, Clone, Copy)]
    enum Kind {
        User,
        Reply,
        Notice,
    }
    let mut s = Session::default();
    let mut time: Option<String> = None;
    let mut kind: Option<Kind> = None;
    let mut buf: Vec<String> = Vec::new();
    let flush =
        |s: &mut Session, kind: Option<Kind>, buf: &mut Vec<String>, time: &Option<String>| {
            let t = buf.join("\n");
            buf.clear();
            let t = t.trim();
            match kind {
                Some(Kind::User) => s.push("user", time.as_deref(), t.to_string()),
                Some(Kind::Reply) => s.push("assistant", time.as_deref(), t.to_string()),
                Some(Kind::Notice) if o.tools => {
                    s.push("tool", time.as_deref(), cut(t, TOOL_RESULT_CHARS))
                }
                _ => {}
            }
        };
    for line in text.lines() {
        if let Some(t) = line.strip_prefix(STARTED) {
            flush(&mut s, kind, &mut buf, &time);
            kind = None;
            let t = iso(t);
            if s.started.is_none() {
                s.started = Some(t.clone());
            }
            s.ended = Some(t.clone());
            time = Some(t);
            continue;
        }
        let (k, body) = if let Some(b) = line.strip_prefix("#### ") {
            (Kind::User, b.trim_end())
        } else if line.trim_end() == "####" {
            (Kind::User, "")
        } else if let Some(b) = line.strip_prefix("> ") {
            (Kind::Notice, b)
        } else if line.trim_end() == ">" {
            (Kind::Notice, "")
        } else if line.trim().is_empty() {
            // a blank line belongs to the block it is in
            if kind.is_some() {
                buf.push(String::new());
            }
            continue;
        } else {
            (Kind::Reply, line)
        };
        // a notice in the middle of a reply is dropped, or is a turn of its own with
        // tools, and the reply's text on both sides of it is one turn
        if kind != Some(k) {
            flush(&mut s, kind, &mut buf, &time);
            kind = Some(k);
        }
        buf.push(body.to_string());
    }
    flush(&mut s, kind, &mut buf, &time);
    s
}
