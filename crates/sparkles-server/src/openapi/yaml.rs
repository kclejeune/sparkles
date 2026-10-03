//! A YAML 1.2 emitter for JSON values: block mappings and sequences, plain scalars where
//! they can only read as strings, JSON-escaped double quotes otherwise, and literal
//! blocks for multi-line text.

use serde_json::Value as J;
use std::fmt::Write;

pub fn to_string(v: &J) -> String {
    let mut out = String::new();
    match v {
        J::Object(m) if !m.is_empty() => mapping(&mut out, v, 0),
        J::Array(a) if !a.is_empty() => sequence(&mut out, v, 0),
        _ => {
            out.push_str(&inline(v));
            out.push('\n');
        }
    }
    out
}

fn pad(out: &mut String, indent: usize) {
    out.extend(std::iter::repeat_n(' ', indent));
}

fn mapping(out: &mut String, v: &J, indent: usize) {
    for (k, val) in v.as_object().unwrap() {
        pad(out, indent);
        out.push_str(&scalar_str(k));
        out.push(':');
        value_after_key(out, val, indent);
    }
}

fn sequence(out: &mut String, v: &J, indent: usize) {
    for item in v.as_array().unwrap() {
        pad(out, indent);
        out.push('-');
        match item {
            // a mapping in a sequence starts on the dash's line
            J::Object(m) if !m.is_empty() => {
                let mut first = true;
                for (k, val) in m {
                    if first {
                        out.push(' ');
                        first = false;
                    } else {
                        pad(out, indent + 2);
                    }
                    out.push_str(&scalar_str(k));
                    out.push(':');
                    value_after_key(out, val, indent + 2);
                }
            }
            J::Array(a) if !a.is_empty() => {
                out.push('\n');
                sequence(out, item, indent + 2);
            }
            J::String(s) if block_ok(s) => block(out, s, indent + 2),
            _ => {
                out.push(' ');
                out.push_str(&inline(item));
                out.push('\n');
            }
        }
    }
}

/// The value of a mapping entry, after `key:`.
fn value_after_key(out: &mut String, val: &J, indent: usize) {
    match val {
        J::Object(m) if !m.is_empty() => {
            out.push('\n');
            mapping(out, val, indent + 2);
        }
        J::Array(a) if !a.is_empty() => {
            out.push('\n');
            sequence(out, val, indent + 2);
        }
        J::String(s) if block_ok(s) => block(out, s, indent + 2),
        _ => {
            out.push(' ');
            out.push_str(&inline(val));
            out.push('\n');
        }
    }
}

/// A literal block scalar (`|-`) for `s`, indented by `indent`.
fn block(out: &mut String, s: &str, indent: usize) {
    out.push_str(" |-\n");
    for line in s.split('\n') {
        if !line.is_empty() {
            pad(out, indent);
            out.push_str(line);
        }
        out.push('\n');
    }
}

/// Whether `s` reads back exactly from a `|-` block: several lines, no final line break,
/// no leading space on the first line (it would set the indentation), no trailing
/// whitespace and no control characters but line breaks.
fn block_ok(s: &str) -> bool {
    s.contains('\n')
        && !s.ends_with('\n')
        && !s.starts_with([' ', '\t'])
        && !s.split('\n').any(|l| l.ends_with([' ', '\t']))
        && !s.chars().any(|c| c.is_control() && c != '\n')
        && !s.contains('\u{feff}')
}

fn inline(v: &J) -> String {
    match v {
        J::Null => "null".into(),
        J::Bool(b) => b.to_string(),
        J::Number(n) => n.to_string(),
        J::String(s) => scalar_str(s),
        J::Array(_) => "[]".into(),
        J::Object(_) => "{}".into(),
    }
}

/// A string scalar: plain when that can only read as this string, else double-quoted
/// with JSON's escapes (which YAML's double-quoted style reads the same way).
fn scalar_str(s: &str) -> String {
    if plain_ok(s) {
        s.to_string()
    } else {
        let mut q = String::with_capacity(s.len() + 2);
        q.push('"');
        for c in s.chars() {
            match c {
                '"' => q.push_str("\\\""),
                '\\' => q.push_str("\\\\"),
                '\n' => q.push_str("\\n"),
                '\t' => q.push_str("\\t"),
                '\r' => q.push_str("\\r"),
                c if c.is_control() || c == '\u{feff}' || c == '\u{2028}' || c == '\u{2029}' => {
                    let _ = write!(q, "\\u{:04x}", c as u32);
                }
                c => q.push(c),
            }
        }
        q.push('"');
        q
    }
}

/// Plain scalars are limited to a safe alphabet, start with a letter, `/`, `_` or `$`, and are
/// never one of YAML's other scalars (booleans, nulls, numbers).
fn plain_ok(s: &str) -> bool {
    let Some(first) = s.chars().next() else {
        return false;
    };
    if !(first.is_ascii_alphabetic() || matches!(first, '/' | '_' | '$')) {
        return false;
    }
    if !s.chars().all(|c| {
        c.is_ascii_alphanumeric()
            || matches!(
                c,
                ' ' | '_' | '-' | '.' | '/' | '$' | '{' | '}' | '+' | '(' | ')' | ','
            )
    }) {
        return false;
    }
    if s.ends_with(' ') || s.contains("  ") {
        return false;
    }
    // `{` and `}` only inside a path; `- ` and `: ` cannot occur (no colon allowed)
    if (s.contains('{') || s.contains('}')) && !first.eq(&'/') {
        return false;
    }
    if s.contains(" #") {
        return false;
    }
    let lower = s.to_ascii_lowercase();
    !matches!(
        lower.as_str(),
        "true" | "false" | "null" | "yes" | "no" | "on" | "off" | "y" | "n" | "nan" | "inf"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn scalars() {
        assert_eq!(scalar_str("getDataset"), "getDataset");
        assert_eq!(scalar_str("/$/datasets/{ds}"), "/$/datasets/{ds}");
        assert_eq!(scalar_str("true"), "\"true\"");
        assert_eq!(scalar_str("Null"), "\"Null\"");
        assert_eq!(scalar_str("200"), "\"200\"");
        assert_eq!(scalar_str(""), "\"\"");
        assert_eq!(scalar_str("a: b"), "\"a: b\"");
        assert_eq!(scalar_str("#/components"), "\"#/components\"");
        assert_eq!(scalar_str("x # y"), "\"x # y\"");
        assert_eq!(scalar_str("{ds}"), "\"{ds}\"");
        assert_eq!(scalar_str("say \"hi\""), "\"say \\\"hi\\\"\"");
        assert_eq!(scalar_str("a\u{1}"), "\"a\\u0001\"");
    }

    #[test]
    fn document() {
        let v = json!({
            "a": { "b": [1, "x", { "c": true, "d": [] }], "e": {} },
            "f": "line one\nline two",
            "g": [["n"]],
            "h": "ends\n",
        });
        assert_eq!(
            to_string(&v),
            "a:\n  b:\n    - 1\n    - x\n    - c: true\n      d: []\n  e: {}\n\
             f: |-\n  line one\n  line two\n\
             g:\n  -\n    - \"n\"\n\
             h: \"ends\\n\"\n"
        );
    }
}
