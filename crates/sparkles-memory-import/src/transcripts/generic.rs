//! The fallback for a transcript that no specific adapter claims, such as an export or a
//! log of another tool. It recognizes three shapes and reads nothing else.
//!
//! 1. JSON lines, or one JSON value, whose messages have a speaker in `role`, `type`,
//!    `author`, `speaker` or `sender` and the words in `content`, `text`, `message` or
//!    `parts`. A value holds its messages in the list itself or in a member named
//!    `messages`, `conversation`, `history`, `turns` or `chat`.
//! 2. Markdown with headings such as `## User` and `## Assistant`.
//! 3. Text with lines that start with `User:` or `Human:` and `Assistant:`, `AI:` or
//!    `Model:`.
//!
//! The speakers `user` and `human` are the person, and `assistant`, `ai`, `model`, `bot`,
//! `gemini`, `claude`, `gpt`, `copilot` and `agent` are the assistant. Any other speaker,
//! such as `system` and `tool`, is left out. A file that matches none of the shapes gives
//! a session without turns, which the import skips.

use super::{Opts, Session, set, text_of};
use serde_json::Value;

fn speaker(s: &str) -> Option<&'static str> {
    match s.trim().to_ascii_lowercase().as_str() {
        "user" | "human" | "you" | "me" => Some("user"),
        "assistant" | "ai" | "model" | "bot" | "gemini" | "claude" | "gpt" | "chatgpt"
        | "copilot" | "agent" => Some("assistant"),
        _ => None,
    }
}

/// The text of a message object: `content`, `text`, `message` or `parts`.
fn words(m: &Value) -> String {
    for k in ["content", "text", "message", "parts"] {
        let v = &m[k];
        let t = match v {
            Value::Object(o) if o.contains_key("content") => text_of(&o["content"]),
            v => text_of(v),
        };
        if !t.trim().is_empty() {
            return t;
        }
    }
    String::new()
}

fn message(s: &mut Session, m: &Value) -> bool {
    let who = ["role", "type", "author", "speaker", "sender"]
        .iter()
        .find_map(|k| m[*k].as_str().and_then(speaker));
    let Some(who) = who else { return false };
    let time = ["timestamp", "time", "created_at", "createdAt", "ts"]
        .iter()
        .find_map(|k| m[*k].as_str());
    s.saw(time);
    set(&mut s.model, m["model"].as_str());
    s.push(who, time, words(m));
    true
}

fn messages(s: &mut Session, v: &Value) {
    match v {
        Value::Array(a) => {
            for m in a {
                message(s, m);
            }
        }
        Value::Object(_) => {
            set(&mut s.id, v["id"].as_str().or(v["sessionId"].as_str()));
            for k in ["messages", "conversation", "history", "turns", "chat"] {
                if v[k].is_array() {
                    messages(s, &v[k]);
                    return;
                }
            }
            message(s, v);
        }
        _ => {}
    }
}

/// A transcript of an unknown format.
pub fn parse(text: &str, _o: &Opts) -> Session {
    let mut s = Session::default();
    let t = text.trim_start();
    if t.starts_with('{') || t.starts_with('[') {
        if let Ok(v) = serde_json::from_str::<Value>(text) {
            messages(&mut s, &v);
            if !s.turns.is_empty() {
                return s;
            }
        }
        // JSON lines
        for line in text.lines() {
            if let Ok(v) = serde_json::from_str::<Value>(line) {
                messages(&mut s, &v);
            }
        }
        if !s.turns.is_empty() {
            return s;
        }
    }
    marked(text)
}

/// Markdown headings or `Speaker:` lines.
fn marked(text: &str) -> Session {
    let mut s = Session::default();
    let mut who: Option<&'static str> = None;
    let mut buf: Vec<&str> = Vec::new();
    let flush = |s: &mut Session, who: Option<&'static str>, buf: &mut Vec<&str>| {
        if let Some(w) = who {
            s.push(w, None, buf.join("\n"));
        }
        buf.clear();
    };
    for line in text.lines() {
        let l = line.trim();
        let heading = l
            .trim_start_matches('#')
            .trim_matches(['*', ' '])
            .trim_end_matches(':')
            .trim();
        if l.starts_with('#')
            && let Some(w) = speaker(heading)
        {
            flush(&mut s, who, &mut buf);
            who = Some(w);
            continue;
        }
        if let Some((head, rest)) = l.trim_start_matches('*').split_once(':')
            && let Some(w) = speaker(head.trim_end_matches('*'))
        {
            flush(&mut s, who, &mut buf);
            who = Some(w);
            let rest = rest.trim_start_matches('*').trim();
            if !rest.is_empty() {
                buf.push(rest);
            }
            continue;
        }
        if who.is_some() {
            buf.push(line);
        }
    }
    flush(&mut s, who, &mut buf);
    s
}
