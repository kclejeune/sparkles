//! Codex's session logs: `~/.codex/sessions/YYYY/MM/DD/rollout-<time>-<id>.jsonl`, one
//! object per line with `timestamp`, `type` and `payload`. The types include
//! `session_meta` with the id, `cwd` and git state, `turn_context`, `response_item` for
//! messages, reasoning and tool calls, and `event_msg`.

use super::{Opts, Session, TOOL_RESULT_CHARS, cut, set};
use serde_json::Value;

/// A Codex session log.
pub fn parse(text: &str, o: &Opts) -> Session {
    let mut s = Session::default();
    for line in text.lines() {
        let Ok(j) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let time = j["timestamp"].as_str();
        let p = &j["payload"];
        match j["type"].as_str().unwrap_or("") {
            "session_meta" => {
                set(&mut s.id, p["id"].as_str());
                set(&mut s.cwd, p["cwd"].as_str());
                set(&mut s.branch, p["git"]["branch"].as_str());
                set(&mut s.version, p["cli_version"].as_str());
                set(&mut s.started, p["timestamp"].as_str().or(time));
            }
            "turn_context" => set(&mut s.model, p["model"].as_str()),
            "response_item" => {
                s.saw(time);
                match p["type"].as_str().unwrap_or("") {
                    "message" => {
                        let role = p["role"].as_str().unwrap_or("");
                        if role != "user" && role != "assistant" {
                            continue;
                        }
                        let t: Vec<&str> = p["content"]
                            .as_array()
                            .into_iter()
                            .flatten()
                            .filter_map(|c| c["text"].as_str())
                            .collect();
                        let t = t.join("\n\n");
                        // the harness's own context, not the person's words
                        let lead = t.trim_start();
                        if lead.starts_with("<environment_context>")
                            || lead.starts_with("<user_instructions>")
                        {
                            continue;
                        }
                        s.push(role, time, t);
                    }
                    "function_call" | "custom_tool_call" if o.tools => {
                        let name = p["name"].as_str().unwrap_or("tool");
                        let args = p["arguments"]
                            .as_str()
                            .or(p["input"].as_str())
                            .unwrap_or("");
                        s.push(
                            "assistant",
                            time,
                            format!("Tool call {name}: {}", cut(args, TOOL_RESULT_CHARS)),
                        );
                    }
                    "function_call_output" | "custom_tool_call_output" if o.tools => {
                        let out = match &p["output"] {
                            Value::String(o) => o.clone(),
                            o => o["content"].as_str().unwrap_or("").to_string(),
                        };
                        s.push("tool", time, cut(&out, TOOL_RESULT_CHARS));
                    }
                    // reasoning, encrypted or not, is never kept
                    _ => {}
                }
            }
            _ => {}
        }
    }
    s
}
