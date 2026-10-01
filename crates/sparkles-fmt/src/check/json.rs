//! The JSON check of JSON-LD: the reference parse with json-event-parser (strict JSON;
//! its errors are the user's syntax errors) into an order-insensitive model, and the
//! comparison of the input's and the output's models. Objects are maps from keys to
//! values (a duplicate key is an error), arrays are ordered, strings are compared after
//! unescaping and numbers by lexeme. Equal models are the same JSON document, so they
//! denote the same RDF under any context, remote ones included. A difference refuses the
//! output as [`Check::Graph`].
//!
//! The model is built without recursion; documents nested deeper than [`MAX_DEPTH`] are
//! refused (the formatter's own parser and printer recurse).

use crate::{Check, FormatError};
use json_event_parser::{JsonEvent, LowLevelJsonParser, LowLevelJsonParserResult};
use std::collections::HashSet;

/// The deepest nesting of objects and arrays the formatter takes.
pub const MAX_DEPTH: usize = 256;

/// A JSON value with the order of object members forgotten.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum JsonValue {
    Null,
    Bool(bool),
    /// the lexeme
    Number(String),
    /// unescaped
    String(String),
    Array(Vec<JsonValue>),
    /// sorted by key; keys are unique
    Object(Vec<(String, JsonValue)>),
}

/// The reference parse of a JSON-LD document.
#[derive(Clone, Debug)]
pub struct JsonReference {
    pub value: JsonValue,
}

/// Parse `text` (a BOM dropped) as strict JSON.
pub fn json_reference(text: &str) -> Result<JsonReference, FormatError> {
    model(text).map(|value| JsonReference { value })
}

/// `Ok` when `output` is the same JSON document as the input.
pub fn json_equivalent(r: &JsonReference, output: &str) -> Result<(), FormatError> {
    match model(output) {
        Ok(v) if v == r.value => Ok(()),
        _ => Err(FormatError::Unsafe {
            check: Check::Graph,
        }),
    }
}

/// An object or array being read.
enum Frame {
    Array(Vec<JsonValue>),
    Object {
        members: Vec<(String, JsonValue)>,
        keys: HashSet<String>,
        /// the key whose value comes next
        key: Option<String>,
    },
}

/// The order-insensitive model of `text`, or the first syntax error (a duplicate key
/// included).
pub fn model(text: &str) -> Result<JsonValue, FormatError> {
    let bytes = text.as_bytes();
    let mut parser = LowLevelJsonParser::new();
    let mut stack: Vec<Frame> = Vec::new();
    let mut root: Option<JsonValue> = None;
    // the input consumed so far, and where the last event's bytes started
    let mut offset = 0;
    loop {
        let start = offset;
        let LowLevelJsonParserResult {
            event,
            consumed_bytes,
        } = parser.parse_next(&bytes[offset..], true);
        offset += consumed_bytes;
        let event = match event {
            // the whole input is there, so the parser always answers
            None => return Err(syntax(text, offset, "unexpected end of the input")),
            Some(Ok(e)) => e,
            Some(Err(e)) => {
                let at = (e.location().start.offset as usize).min(text.len());
                return Err(syntax(text, at, e.message()));
            }
        };
        let value = match event {
            JsonEvent::Eof => break,
            JsonEvent::StartArray | JsonEvent::StartObject => {
                if stack.len() >= MAX_DEPTH {
                    let (line, column) = crate::line_col(text, offset - 1);
                    return Err(FormatError::Unsupported {
                        message: format!("nesting deeper than {MAX_DEPTH} levels"),
                        line,
                        column,
                    });
                }
                stack.push(match event {
                    JsonEvent::StartArray => Frame::Array(Vec::new()),
                    _ => Frame::Object {
                        members: Vec::new(),
                        keys: HashSet::new(),
                        key: None,
                    },
                });
                continue;
            }
            JsonEvent::ObjectKey(k) => {
                let Some(Frame::Object { keys, key, .. }) = stack.last_mut() else {
                    unreachable!("a key is in an object");
                };
                if !keys.insert(k.to_string()) {
                    // the key string is the first `"` of the bytes this event consumed
                    let at = start
                        + bytes[start..offset]
                            .iter()
                            .position(|&b| b == b'"')
                            .unwrap_or(0);
                    return Err(syntax(text, at, &format!("duplicate key {:?}", k.as_ref())));
                }
                *key = Some(k.into_owned());
                continue;
            }
            JsonEvent::EndArray => match stack.pop() {
                Some(Frame::Array(items)) => JsonValue::Array(items),
                _ => unreachable!("an array ends"),
            },
            JsonEvent::EndObject => match stack.pop() {
                Some(Frame::Object { mut members, .. }) => {
                    members.sort_unstable_by(|a, b| a.0.cmp(&b.0));
                    JsonValue::Object(members)
                }
                _ => unreachable!("an object ends"),
            },
            JsonEvent::String(s) => JsonValue::String(s.into_owned()),
            JsonEvent::Number(n) => JsonValue::Number(n.into_owned()),
            JsonEvent::Boolean(b) => JsonValue::Bool(b),
            JsonEvent::Null => JsonValue::Null,
        };
        match stack.last_mut() {
            None => root = Some(value),
            Some(Frame::Array(items)) => items.push(value),
            Some(Frame::Object { members, key, .. }) => {
                let k = key.take().expect("a value follows its key");
                members.push((k, value));
            }
        }
    }
    Ok(root.expect("the parser reads one value before the end"))
}

/// A syntax error at byte `at`; a comment gets its own message.
fn syntax(text: &str, at: usize, message: &str) -> FormatError {
    let rest = &text.as_bytes()[at.min(text.len())..];
    let message = if rest.starts_with(b"//") || rest.starts_with(b"/*") {
        "comments are not allowed in JSON".to_string()
    } else {
        message.to_string()
    };
    let (line, column) = crate::line_col(text, at);
    FormatError::Syntax {
        message,
        line,
        column,
        offset: at,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn err(text: &str) -> (String, u32, u32, usize) {
        match json_reference(text) {
            Err(FormatError::Syntax {
                message,
                line,
                column,
                offset,
            }) => (message, line, column, offset),
            other => panic!("{text:?}: {other:?}"),
        }
    }

    #[test]
    fn models_ignore_member_order_and_escapes() {
        let a = json_reference("{\"a\": [1, \"x\"], \"b\": {\"c\": null, \"d\": true}}").unwrap();
        let same = "{ \"b\": {\"d\": true, \"\\u0063\": null},\n \"a\": [1, \"\\u0078\"] }";
        assert!(json_equivalent(&a, same).is_ok());
        for other in [
            "{\"a\": [\"x\", 1], \"b\": {\"c\": null, \"d\": true}}",
            "{\"a\": [1.0, \"x\"], \"b\": {\"c\": null, \"d\": true}}",
            "{\"a\": [1, \"x\"], \"b\": {\"c\": null, \"d\": false}}",
            "{\"a\": [1, \"x\"], \"b\": {\"c\": null}}",
            "{\"a\": [1, \"x\"], \"b\": {\"c\": null, \"d\": true}, \"e\": 1}",
            "[{\"a\": [1, \"x\"], \"b\": {\"c\": null, \"d\": true}}]",
            "{\"a\": [1, \"x\"], \"b\": {\"c\": null, \"d\": true}",
        ] {
            assert_eq!(
                json_equivalent(&a, other),
                Err(FormatError::Unsafe {
                    check: Check::Graph
                }),
                "{other}"
            );
        }
        // numbers compare by lexeme
        let n = json_reference("[1e2]").unwrap();
        assert!(json_equivalent(&n, "[100]").is_err());
        assert!(json_equivalent(&n, " [ 1e2 ] ").is_ok());
        // scalars at the top
        assert!(json_equivalent(&json_reference("\u{feff}\"x\"").unwrap(), "\"x\"\n").is_ok());
    }

    #[test]
    fn duplicate_keys_are_errors_at_the_second_key() {
        let (message, line, column, offset) = err("{\"a\": 1,\n  \"b\": {\"a\": 2, \"a\": 3}}");
        assert_eq!(message, "duplicate key \"a\"");
        assert_eq!((line, column, offset), (2, 17, 25));
        // the same key spelled with an escape
        let (message, ..) = err("{\"@id\": \"x\", \"\\u0040id\": \"y\"}");
        assert_eq!(message, "duplicate key \"@id\"");
        // after the BOM, the column counts characters
        let (_, line, column, offset) = err("\u{feff}{\"é\": 1, \"é\": 2}");
        assert_eq!((line, column, offset), (1, 10, 13));
    }

    #[test]
    fn comments_are_errors() {
        let (message, line, column, _) = err("{\n  // a comment\n  \"a\": 1\n}");
        assert_eq!(message, "comments are not allowed in JSON");
        assert_eq!((line, column), (2, 3));
        let (message, ..) = err("/* c */ {}");
        assert_eq!(message, "comments are not allowed in JSON");
        let (message, ..) = err("{} # c");
        assert_ne!(message, "comments are not allowed in JSON");
    }

    #[test]
    fn syntax_errors() {
        for text in [
            "",
            "{",
            "[1,]",
            "{\"a\": 1,}",
            "{a: 1}",
            "'x'",
            "[1] [2]",
            "01",
            "\"\t\"",
        ] {
            err(text);
        }
    }

    #[test]
    fn nesting_is_bounded() {
        let ok = format!("{}{}", "[".repeat(MAX_DEPTH), "]".repeat(MAX_DEPTH));
        assert!(json_reference(&ok).is_ok());
        let deep = format!("{}{}", "[".repeat(MAX_DEPTH + 1), "]".repeat(MAX_DEPTH + 1));
        assert!(matches!(
            json_reference(&deep),
            Err(FormatError::Unsupported { line: 1, column, .. }) if column as usize == MAX_DEPTH + 1
        ));
    }
}
