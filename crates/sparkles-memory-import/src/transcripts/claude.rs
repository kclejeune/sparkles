//! Claude Code's session transcripts: `~/.claude/projects/<project>/<session>.jsonl` and
//! `<session>/subagents/*.jsonl`, one JSON object per line with `type`, `sessionId`,
//! `timestamp`, `cwd`, `gitBranch`, `isSidechain` and a `message` whose content is text or
//! blocks of `text`, `thinking`, `tool_use` and `tool_result`.

use super::{Opts, Session, TOOL_RESULT_CHARS, cut, set, text_of};
use serde_json::Value;

/// A Claude Code transcript. With `o.sidechain` the file is a subagent's, whose lines
/// are all on its own chain.
pub fn parse(text: &str, o: &Opts) -> Session {
    let mut s = Session::default();
    for line in text.lines() {
        let Ok(j) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let kind = j["type"].as_str().unwrap_or("");
        if kind != "user" && kind != "assistant" {
            continue;
        }
        if (j["isSidechain"] == true && !o.sidechain) || j["isMeta"] == true {
            continue;
        }
        let time = j["timestamp"].as_str();
        s.saw(time);
        set(&mut s.id, j["sessionId"].as_str());
        set(&mut s.cwd, j["cwd"].as_str());
        set(&mut s.branch, j["gitBranch"].as_str());
        set(&mut s.version, j["version"].as_str());
        let msg = &j["message"];
        if kind == "assistant" {
            set(&mut s.model, msg["model"].as_str());
        }
        let mut texts: Vec<String> = Vec::new();
        let mut results: Vec<String> = Vec::new();
        match &msg["content"] {
            Value::String(t) => texts.push(t.clone()),
            Value::Array(blocks) => {
                for b in blocks {
                    match b["type"].as_str().unwrap_or("") {
                        "text" => texts.push(b["text"].as_str().unwrap_or("").to_string()),
                        "tool_use" => {
                            let name = b["name"].as_str().unwrap_or("tool");
                            if matches!(name, "Write" | "Edit" | "MultiEdit" | "NotebookEdit")
                                && let Some(p) = b["input"]["file_path"]
                                    .as_str()
                                    .or(b["input"]["notebook_path"].as_str())
                            {
                                s.writes.push(p.to_string());
                            }
                            if o.tools {
                                texts.push(format!(
                                    "Tool call {name}: {}",
                                    cut(&b["input"].to_string(), TOOL_RESULT_CHARS)
                                ));
                            }
                        }
                        "tool_result" if o.tools => {
                            results.push(cut(&text_of(&b["content"]), TOOL_RESULT_CHARS));
                        }
                        // thinking, tool results without tools, images and documents
                        _ => {}
                    }
                }
            }
            _ => {}
        }
        let role = if kind == "assistant" {
            "assistant"
        } else {
            "user"
        };
        s.push(role, time, texts.join("\n\n"));
        if !results.is_empty() {
            s.push("tool", time, results.join("\n\n"));
        }
    }
    s
}
