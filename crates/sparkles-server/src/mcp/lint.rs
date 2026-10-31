//! The `lint` tool: lint a SPARQL query or update, Turtle, TriG, N-Triples, N-Quads,
//! JSON-LD or SHACLC document with the rules of `sparkles lint` and `POST /$/lint`,
//! including the SPARQL inside SHACL literals. It reads no dataset and writes nothing. The
//! input is capped at 1 MiB of text, and the result at the server's `--mcp-max-bytes`,
//! measured as the structured content is serialized: a fixed text whose escaped form does
//! not fit is refused, and the findings fill what is left. The call runs within its
//! timeout.

use super::Outcome;
use super::errors::{ToolError, secs};
use super::format::{MAX_TEXT_CHARS, language};
use super::tools::{Tools, parse};
use serde::Deserialize;
use serde_json::{Map, Value, json};
use sparkles_fmt::lint::{self, LintError, LintOptions};

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct LintArgs {
    text: String,
    language: Option<String>,
    options: Option<Value>,
    fix: Option<bool>,
    timeout_seconds: Option<f64>,
}

/// The kebab-case rule name of a camelCase one (`unusedPrefix` is `unused-prefix`). A name
/// that is kebab-case already stays as it is.
fn kebab(name: &str) -> String {
    let mut out = String::with_capacity(name.len() + 4);
    for c in name.chars() {
        if c.is_ascii_uppercase() {
            out.push('-');
            out.push(c.to_ascii_lowercase());
        } else {
            out.push(c);
        }
    }
    out
}

/// The rule levels of an `options` object whose keys are camelCase rule names.
fn levels(options: &Value) -> Result<LintOptions, ToolError> {
    let mut opts = LintOptions::default();
    match options {
        Value::Null => {}
        Value::Object(rules) => {
            for (name, level) in rules {
                let level = level.as_str().ok_or_else(|| {
                    ToolError::bad_argument(format!(
                        "options.{name}: expected error, warning, info, hint or off"
                    ))
                })?;
                opts.set(&kebab(name), level)
                    .map_err(|e| ToolError::bad_argument(format!("options.{name}: {e}")))?;
            }
        }
        _ => return Err(ToolError::bad_argument("`options` must be an object")),
    }
    Ok(opts)
}

impl Tools<'_> {
    pub(super) fn lint(&self, args: Map<String, Value>) -> Result<Outcome, ToolError> {
        let a: LintArgs = parse(args)?;
        if a.text.trim().is_empty() {
            return Err(ToolError::bad_argument("text must not be empty"));
        }
        if a.text.chars().count() > MAX_TEXT_CHARS {
            return Err(ToolError::bad_argument(format!(
                "text must be at most {MAX_TEXT_CHARS} characters"
            ))
            .hint("lint the document in parts, or use `sparkles lint` on the file"));
        }
        let timeout = self.timeout(a.timeout_seconds)?;
        let lang = language(a.language.as_deref(), &a.text)?;
        let mut opts = match &a.options {
            Some(o) => levels(o)?,
            None => LintOptions::default(),
        };
        // queued time counts against the timeout, as for the other tools
        opts.deadline = Some(self.call.arrived + timeout);
        let fix = a.fix.unwrap_or(false);
        let (fixed, applied, diagnostics) = if fix {
            let f = lint::fix(&a.text, lang, &opts).map_err(|e| self.error(&e, timeout))?;
            (Some(f.text), f.applied, f.diagnostics)
        } else {
            let l = lint::lint(&a.text, lang, &opts).map_err(|e| self.error(&e, timeout))?;
            (None, 0, l.diagnostics)
        };
        // the findings describe the fixed text when there is one
        let text = fixed.as_deref().unwrap_or(&a.text);
        let max = self.cfg().max_bytes;
        let mut counts = json!({"error": 0, "warning": 0, "info": 0, "hint": 0});
        for d in &diagnostics {
            let n = &mut counts[d.severity.name()];
            *n = json!(n.as_u64().unwrap_or(0) + 1);
        }
        // the result without findings, as it is serialized, escapes included, and with
        // `truncated` counted whether or not it is set
        let mut out = json!({
            "language": lang.name(),
            "diagnostics": [],
            "total": diagnostics.len(),
            "counts": counts,
            "truncated": true,
        });
        if let Some(f) = &fixed {
            out["fixed"] = json!(f);
            out["applied"] = json!(applied);
        }
        let frame = serialized_len(&out);
        if frame > max {
            return Err(ToolError::new(
                "too-large",
                413,
                format!(
                    "the result with the fixed text has {frame} bytes, more than the {max} bytes a result may have"
                ),
            )
            .hint("lint the document in parts, or use `sparkles lint --fix` on the file"));
        }
        // the findings fit what is left, each with the comma before it
        let mut room = max - frame;
        let mut shown: Vec<Value> = Vec::new();
        let utf16 = crate::http::lint::Utf16::new(text, &diagnostics);
        for d in &diagnostics {
            let j = crate::http::lint::diagnostic(&utf16, d);
            let size = serialized_len(&j) + usize::from(!shown.is_empty());
            if size > room {
                break;
            }
            room -= size;
            shown.push(j);
        }
        let truncated = shown.len() < diagnostics.len();
        out["diagnostics"] = Value::Array(shown);
        if !truncated && let Some(o) = out.as_object_mut() {
            o.remove("truncated");
        }
        Ok(Outcome::Structured(out))
    }

    fn error(&self, e: &LintError, timeout: std::time::Duration) -> ToolError {
        match e {
            LintError::Timeout => ToolError::new(
                "timeout",
                408,
                format!(
                    "linting exceeded the {} s timeout",
                    secs(timeout.as_secs_f64())
                ),
            )
            .hint(format!(
                "lint a smaller document, or raise timeoutSeconds (max {})",
                secs(self.cfg().max_timeout.as_secs_f64())
            )),
            LintError::TooLarge => ToolError::new("too-large", 413, e.to_string()),
            LintError::UnsupportedLanguage(_) => {
                ToolError::new("unsupported-language", 415, e.to_string())
            }
            LintError::UnsafeFix => ToolError::new(e.code(), 422, e.to_string())
                .hint("keep the text as it is; this is a linter bug"),
        }
    }
}

/// The length of `v` as compact JSON, counted without building the string.
fn serialized_len(v: &Value) -> usize {
    struct Count(usize);
    impl std::io::Write for Count {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0 += bytes.len();
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut c = Count(0);
    serde_json::to_writer(&mut c, v).expect("a JSON value serializes");
    c.0
}
