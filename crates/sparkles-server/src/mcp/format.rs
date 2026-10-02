//! The `format` tool: format a SPARQL query or update, Turtle, TriG, N-Triples, N-Quads or
//! JSON-LD with the engine of `sparkles fmt` and `POST /$/format`. It reads no dataset and
//! writes nothing. The input is capped at 1 MiB of text and the output at the server's
//! `--mcp-max-bytes`, and the call runs within its timeout.

use super::Outcome;
use super::errors::{ToolError, secs};
use super::tools::{Tools, parse};
use serde::Deserialize;
use serde_json::{Map, Value, json};
use sparkles_fmt::{Detection, FormatError, Language, Options};

/// Largest document, in characters.
pub(super) const MAX_TEXT_CHARS: usize = 1 << 20;

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct FormatArgs {
    text: String,
    language: Option<String>,
    options: Option<Value>,
    timeout_seconds: Option<f64>,
}

/// The language a call names, else the one the text looks like.
fn language(name: Option<&str>, text: &str) -> Result<Language, ToolError> {
    let rdf_xml = || {
        ToolError::new("unsupported-language", 415, sparkles_fmt::RDF_XML_MESSAGE)
            .hint("convert the document to Turtle first")
    };
    if let Some(name) = name {
        if name.eq_ignore_ascii_case("rdfxml") {
            return Err(rdf_xml());
        }
        return Language::from_name(name).ok_or_else(|| {
            ToolError::bad_argument(format!(
                "unknown language '{name}': sparql, turtle, trig, ntriples, nquads or jsonld"
            ))
        });
    }
    match sparkles_fmt::detect(None, text) {
        Detection::Lang(l) => Ok(l),
        Detection::RdfXml => Err(rdf_xml()),
        _ => Err(
            ToolError::bad_argument("cannot tell the language of the text")
                .hint("set language: sparql, turtle, trig, ntriples, nquads or jsonld"),
        ),
    }
}

impl Tools<'_> {
    pub(super) fn format(&self, args: Map<String, Value>) -> Result<Outcome, ToolError> {
        let a: FormatArgs = parse(args)?;
        if a.text.trim().is_empty() {
            return Err(ToolError::bad_argument("text must not be empty"));
        }
        if a.text.chars().count() > MAX_TEXT_CHARS {
            return Err(ToolError::bad_argument(format!(
                "text must be at most {MAX_TEXT_CHARS} characters"
            ))
            .hint("format the document in parts, or use `sparkles fmt` on the file"));
        }
        let timeout = self.timeout(a.timeout_seconds)?;
        let lang = language(a.language.as_deref(), &a.text)?;
        let mut opts = Options::default();
        if let Some(o) = &a.options {
            crate::fmt::config::json_options(o, &mut opts).map_err(|e| match e.option {
                Some(name) => ToolError::bad_argument(format!("options.{name}: {}", e.message)),
                None => ToolError::bad_argument(e.message),
            })?;
        }
        // queued time counts against the timeout, as for the other tools
        opts.deadline = Some(self.call.arrived + timeout);
        let f = sparkles_fmt::format(&a.text, lang, &opts).map_err(|e| match e {
            FormatError::Syntax {
                message,
                line,
                column,
                ..
            } => ToolError::new(
                "syntax",
                400,
                format!(
                    "{} syntax error at line {line}, column {column}: {}",
                    lang.display_name(),
                    crate::fmt::report::short_message(&message)
                ),
            )
            .hint(if a.language.is_none() {
                format!(
                    "the text was read as {}; set language if that is wrong",
                    lang.name()
                )
            } else {
                "fix the syntax error; the formatter only reformats valid documents".into()
            }),
            FormatError::Timeout => ToolError::new(
                "timeout",
                408,
                format!(
                    "formatting exceeded the {} s timeout",
                    secs(timeout.as_secs_f64())
                ),
            )
            .hint(format!(
                "format a smaller document, or raise timeoutSeconds (max {})",
                secs(self.cfg().max_timeout.as_secs_f64())
            )),
            FormatError::TooLarge => ToolError::new("too-large", 413, e.to_string()),
            e @ FormatError::UnsupportedLanguage { .. } => {
                ToolError::new("unsupported-language", 415, e.to_string())
            }
            // the formatter refused its own output: the text stays as it is
            e => ToolError::new(e.code(), 422, e.to_string())
                .hint("keep the text as it is; this is a formatter bug"),
        })?;
        if f.text.len() > self.cfg().max_bytes {
            return Err(ToolError::new(
                "too-large",
                413,
                format!(
                    "the formatted text has {} bytes, more than the {} bytes a result may have",
                    f.text.len(),
                    self.cfg().max_bytes
                ),
            )
            .hint("format the document in parts, or use `sparkles fmt` on the file"));
        }
        let warnings: Vec<Value> = f
            .warnings
            .iter()
            .map(|w| {
                json!({
                    "code": w.code,
                    "message": w.message,
                    "line": w.line,
                    "column": w.column,
                })
            })
            .collect();
        Ok(Outcome::Structured(json!({
            "language": f.language.name(),
            "changed": f.changed,
            "text": f.text,
            "warnings": warnings,
        })))
    }
}
