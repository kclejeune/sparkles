//! Structured output with degradation (§3.6). A call asks for JSON that matches a
//! schema, at the level the pair supports: the provider's schema-constrained output,
//! JSON mode with the schema in the prompt, or plain text from which a fallback takes
//! what it can. Every answer is validated here, whatever the provider claims, and an
//! answer that fails validation is retried once with the errors.
//!
//! The schemas use a small common subset of JSON Schema that local servers accept:
//! `type`, `properties`, `required`, `additionalProperties: false`, `items`, `enum`,
//! `maxLength`, `minItems` and `maxItems`, with no `$ref`, no `anyOf` or `oneOf`, no
//! `patternProperties` and no numeric bounds.

use super::client::{CallError, ChatResponse, Message};
use super::config::Level;
use serde_json::{Value, json};

/// The schema of one output, and how plain text becomes that output.
pub struct OutputSchema {
    pub name: &'static str,
    pub schema: Value,
    /// what an answer at the `text` level gives, or `None` when the text holds nothing
    /// usable
    pub from_text: fn(&str) -> Option<Value>,
    /// the instruction that replaces the schema at the `text` level
    pub text_instruction: &'static str,
}

/// The errors of `v` against the schema subset, at most 10.
pub fn validate(schema: &Value, v: &Value) -> Vec<String> {
    let mut out = Vec::new();
    check(schema, v, "$", &mut out);
    out.truncate(10);
    out
}

fn check(s: &Value, v: &Value, at: &str, out: &mut Vec<String>) {
    if let Some(e) = s.get("enum").and_then(Value::as_array)
        && !e.contains(v)
    {
        out.push(format!("{at} must be one of {}", Value::Array(e.clone())));
        return;
    }
    let ty = s.get("type").and_then(Value::as_str).unwrap_or("");
    let ok = match ty {
        "object" => v.is_object(),
        "array" => v.is_array(),
        "string" => v.is_string(),
        "integer" => v.is_i64() || v.is_u64(),
        "number" => v.is_number(),
        "boolean" => v.is_boolean(),
        "null" => v.is_null(),
        _ => true,
    };
    if !ok {
        out.push(format!("{at} must be of type {ty}"));
        return;
    }
    match v {
        Value::Object(m) => {
            let props = s.get("properties").and_then(Value::as_object);
            for r in s
                .get("required")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
            {
                if !m.contains_key(r) {
                    out.push(format!("{at} lacks the member {r}"));
                }
            }
            for (k, val) in m {
                match props.and_then(|p| p.get(k)) {
                    Some(ps) => check(ps, val, &format!("{at}.{k}"), out),
                    None if s.get("additionalProperties") == Some(&Value::Bool(false)) => {
                        out.push(format!("{at} has the unexpected member {k}"))
                    }
                    None => {}
                }
            }
        }
        Value::Array(a) => {
            if let Some(n) = s.get("maxItems").and_then(Value::as_u64)
                && a.len() as u64 > n
            {
                out.push(format!("{at} has more than {n} items"));
            }
            if let Some(n) = s.get("minItems").and_then(Value::as_u64)
                && (a.len() as u64) < n
            {
                out.push(format!("{at} has fewer than {n} items"));
            }
            if let Some(items) = s.get("items") {
                for (i, x) in a.iter().enumerate() {
                    check(items, x, &format!("{at}[{i}]"), out);
                }
            }
        }
        Value::String(t) => {
            if let Some(n) = s.get("maxLength").and_then(Value::as_u64)
                && t.chars().count() as u64 > n
            {
                out.push(format!("{at} is longer than {n} characters"));
            }
        }
        _ => {}
    }
}

/// The JSON of an answer at a JSON level: the tool input, or the text, which may sit in
/// a fenced block or after some prose.
pub fn json_of(r: &ChatResponse) -> Option<Value> {
    if let Some(v) = &r.tool_input {
        return Some(v.clone());
    }
    let t = r.text.trim();
    if let Ok(v) = serde_json::from_str(t) {
        return Some(v);
    }
    // a fenced block, or the outermost braces
    let inner = fenced(t, &["json", ""]).map(str::to_string).or_else(|| {
        let (a, b) = (t.find('{')?, t.rfind('}')?);
        (a < b).then(|| t[a..=b].to_string())
    })?;
    serde_json::from_str(&inner).ok()
}

/// The first fenced block whose info string is one of `langs` (`""` for none).
pub fn fenced<'a>(t: &'a str, langs: &[&str]) -> Option<&'a str> {
    let mut rest = t;
    while let Some(i) = rest.find("```") {
        let after = &rest[i + 3..];
        let nl = after.find('\n')?;
        let info = after[..nl].trim().to_ascii_lowercase();
        let body = &after[nl + 1..];
        let end = body.find("```")?;
        if langs.contains(&info.as_str()) {
            return Some(body[..end].trim());
        }
        rest = &body[end + 3..];
    }
    None
}

/// The messages of a call at `level`: the user's text, with the schema as text at the
/// `json-object` level and the text instruction at the `text` level.
pub fn messages(user: &str, out: &OutputSchema, level: Level) -> Vec<Message> {
    let mut u = user.to_string();
    match level {
        Level::JsonObject => {
            u.push_str("\n\nAnswer with one JSON object that matches this JSON Schema, and nothing else:\n");
            u.push_str(&out.schema.to_string());
        }
        Level::Text => {
            u.push_str("\n\n");
            u.push_str(out.text_instruction);
        }
        _ => {}
    }
    vec![Message::user(u)]
}

/// The value of an answer at `level`, or the validation errors that a retry sends back.
pub fn value_of(r: &ChatResponse, out: &OutputSchema, level: Level) -> Result<Value, Vec<String>> {
    let v = if level == Level::Text {
        (out.from_text)(&r.text)
            .ok_or_else(|| vec!["the answer holds nothing usable".to_string()])?
    } else {
        json_of(r).ok_or_else(|| vec!["the answer is not a JSON object".to_string()])?
    };
    let errors = validate(&out.schema, &v);
    if errors.is_empty() {
        Ok(v)
    } else {
        Err(errors)
    }
}

/// The follow-up message of a retry after `errors`.
pub fn retry_message(errors: &[String]) -> Message {
    Message::user(format!(
        "Your answer did not match the required format: {}. Answer again in the required format.",
        errors.join("; ")
    ))
}

/// Whether a failed call at a JSON level says that the provider does not support it,
/// so that the next level should be tried.
pub fn unsupported(e: &CallError) -> bool {
    matches!(e, CallError::Rejected(..))
}

/// The output schema of the test call of `POST /$/models/{name}/test`.
pub fn ping() -> OutputSchema {
    OutputSchema {
        name: "Ping",
        schema: json!({
            "type": "object",
            "properties": { "answer": { "type": "string", "maxLength": 200 } },
            "required": ["answer"],
            "additionalProperties": false,
        }),
        from_text: |t| {
            let t = t.trim();
            (!t.is_empty()).then(|| json!({ "answer": super::client::cut(t, 200) }))
        },
        text_instruction: "Answer with one short sentence.",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subset_validation() {
        let s = json!({
            "type": "object",
            "properties": {
                "q": {"type": "string", "maxLength": 3},
                "a": {"type": "array", "items": {"type": "string"}, "maxItems": 1},
                "k": {"type": "string", "enum": ["x", "y"]}
            },
            "required": ["q"],
            "additionalProperties": false
        });
        assert!(validate(&s, &json!({"q": "abc", "a": ["x"], "k": "x"})).is_empty());
        let e = validate(&s, &json!({"q": "abcd", "a": ["x", 1], "z": 1, "k": "w"}));
        assert!(e.iter().any(|m| m.contains("longer than 3")), "{e:?}");
        assert!(e.iter().any(|m| m.contains("more than 1")));
        assert!(
            e.iter()
                .any(|m| m.contains("$.a[1] must be of type string"))
        );
        assert!(e.iter().any(|m| m.contains("unexpected member z")));
        assert!(e.iter().any(|m| m.contains("one of")));
        assert_eq!(validate(&s, &json!({})), vec!["$ lacks the member q"]);
    }

    #[test]
    fn json_in_text() {
        let r = |t: &str| ChatResponse {
            text: t.into(),
            ..Default::default()
        };
        assert_eq!(json_of(&r(r#"{"a":1}"#)), Some(json!({"a": 1})));
        assert_eq!(
            json_of(&r("Here:\n```json\n{\"a\":2}\n```\n")),
            Some(json!({"a": 2}))
        );
        assert_eq!(json_of(&r("x {\"a\":3} y")), Some(json!({"a": 3})));
        assert_eq!(json_of(&r("no json")), None);
        assert_eq!(
            fenced(
                "a\n```text\nno\n```\n```sparql\nSELECT * {}\n```",
                &["sparql"]
            ),
            Some("SELECT * {}")
        );
    }
}
