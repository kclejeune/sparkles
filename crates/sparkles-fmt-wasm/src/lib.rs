//! The formatter for the browser (`wasm32-unknown-unknown`, wasm-bindgen): the UI formats
//! without a round trip and falls back to `POST /$/format` when this module does not load.
//! [`format`] takes the endpoint's JSON request body and returns its JSON answer, so the
//! two are interchangeable: `{ text, language?, cursorOffset?, options? }` in (the cursor
//! in UTF-16 code units, the options in camelCase), and out either
//! `{ text, changed, language, cursorOffset, warnings }` or an error object with the
//! endpoint's `status`, `error`, `code` and, for syntax errors, `line`, `column`,
//! `language` and (when `error` holds only the head of the parser's message) `detail`.
//!
//! `mise run ui:wasm` builds it into the UI (`scripts/build-fmt-wasm.sh`). The module has
//! no deadline (`std` reads no clock on this target): the UI formats an editor's text.

use serde_json::{Map, Value as J, json};
use sparkles_fmt::options::{self, Value};
use sparkles_fmt::{Detection, FormatError, Language, Options};
use wasm_bindgen::prelude::*;

/// Format the document of a `POST /$/format` JSON body; the answer is the endpoint's JSON
/// (an error object carries the HTTP `status` the endpoint would answer with).
#[wasm_bindgen]
pub fn format(request: &str) -> String {
    match run(request) {
        Ok(v) | Err(v) => v.to_string(),
    }
}

fn run(request: &str) -> Result<J, J> {
    let body: J = serde_json::from_str(request)
        .map_err(|e| bad_request(&format!("invalid JSON: {e}"), None))?;
    let obj = body
        .as_object()
        .ok_or_else(|| bad_request("expected a JSON object", None))?;
    let text = obj
        .get("text")
        .and_then(J::as_str)
        .ok_or_else(|| bad_request("expected `text`, the document to format", None))?;
    let mut opts = Options::default();
    if let Some(o) = obj.get("options") {
        set_options(o, &mut opts)?;
    }
    let lang = language(obj.get("language"), text)?;
    opts.cursor =
        match obj.get("cursorOffset") {
            None | Some(J::Null) => None,
            Some(v) => {
                let units = v.as_u64().ok_or_else(|| {
                    bad_request("`cursorOffset` must be a non-negative integer", None)
                })?;
                Some(sparkles_fmt::utf16_to_byte(text, units).ok_or_else(|| {
                    bad_request("`cursorOffset` is beyond the end of the text", None)
                })?)
            }
        };
    let f = sparkles_fmt::format(text, lang, &opts).map_err(|e| format_error(&e, lang))?;
    let warnings: Vec<J> = f
        .warnings
        .iter()
        .map(
            |w| json!({ "code": w.code, "message": w.message, "line": w.line, "column": w.column }),
        )
        .collect();
    Ok(json!({
        "cursorOffset": f.cursor.map(|c| sparkles_fmt::byte_to_utf16(&f.text, c)),
        "text": f.text,
        "changed": f.changed,
        "language": f.language.name(),
        "warnings": warnings,
    }))
}

/// The language the request names, else what the text looks like.
fn language(name: Option<&J>, text: &str) -> Result<Language, J> {
    let rdf_xml = || error(415, sparkles_fmt::RDF_XML_MESSAGE, None);
    match name {
        None | Some(J::Null) => match sparkles_fmt::detect(None, text) {
            Detection::Lang(l) => Ok(l),
            Detection::RdfXml => Err(rdf_xml()),
            _ => Err(bad_request(
                "cannot tell the language of the text: set `language`",
                None,
            )),
        },
        Some(J::String(n)) if n.eq_ignore_ascii_case("rdfxml") => Err(rdf_xml()),
        Some(J::String(n)) => Language::from_name(n).ok_or_else(|| {
            bad_request(
                &format!(
                    "unknown language '{n}': sparql, turtle, trig, ntriples, nquads or jsonld"
                ),
                None,
            )
        }),
        Some(_) => Err(bad_request("`language` must be a string", None)),
    }
}

/// The camelCase options of a request, through the same checks as every other front end.
fn set_options(v: &J, o: &mut Options) -> Result<(), J> {
    let obj = v
        .as_object()
        .ok_or_else(|| bad_request("`options` must be an object", None))?;
    for (name, v) in obj {
        // camelCase only, as over HTTP
        let key = options::KEYS
            .iter()
            .find(|(_, camel)| camel == name)
            .map(|(kebab, _)| *kebab)
            .ok_or_else(|| bad_request(&format!("{name}: unknown option"), Some(name)))?;
        let value = match v {
            J::Null => continue,
            J::Bool(b) => Value::Bool(*b),
            J::Number(n) => n
                .as_i64()
                .map_or_else(|| Value::Str(n.to_string()), Value::Int),
            J::String(s) => Value::Str(s.clone()),
            J::Array(groups) => groups
                .iter()
                .map(|g| {
                    g.as_array()?
                        .iter()
                        .map(|l| l.as_str().map(str::to_string))
                        .collect::<Option<Vec<String>>>()
                })
                .collect::<Option<Vec<Vec<String>>>>()
                .map(Value::Groups)
                .ok_or_else(|| {
                    bad_request(
                        &format!("{name}: expected an array of arrays of prefix labels"),
                        Some(name),
                    )
                })?,
            // refused by `options::set`, in the endpoint's words
            J::Object(_) => Value::Str(v.to_string()),
        };
        options::set(o, key, value)
            .map_err(|e| bad_request(&format!("{name}: {}", e.message), Some(name)))?;
    }
    Ok(())
}

/// The endpoint's answer to a formatting error.
fn format_error(e: &FormatError, lang: Language) -> J {
    match e {
        FormatError::Syntax {
            message,
            line,
            column,
            ..
        } => {
            // the head of the parser's message; the whole of it in `detail`
            let short = short_message(message);
            let mut j = error(
                400,
                &format!(
                    "{} syntax error at line {line}, column {column}: {short}",
                    lang.display_name()
                ),
                Some("syntax"),
            );
            j["line"] = json!(line);
            j["column"] = json!(column);
            j["language"] = json!(lang.name());
            if short != message.trim() {
                j["detail"] = json!(message);
            }
            j
        }
        FormatError::UnsupportedLanguage { message, .. } => error(415, message, None),
        FormatError::Timeout => error(408, &e.to_string(), None),
        FormatError::TooLarge => error(413, &e.to_string(), None),
        FormatError::Unsupported { .. } | FormatError::Unsafe { .. } => {
            error(422, &e.to_string(), Some(e.code()))
        }
    }
}

/// A parser message cut to its first line and about 120 characters, at a list separator,
/// as the endpoint and `sparkles fmt` print it (spargebra lists every token it expected).
fn short_message(message: &str) -> String {
    const MAX: usize = 120;
    let message = message.trim();
    let first = message.lines().next().unwrap_or("").trim_end();
    if first.len() == message.len() && first.chars().count() <= MAX {
        return first.to_string();
    }
    let end = first
        .char_indices()
        .nth(MAX)
        .map_or(first.len(), |(i, _)| i);
    let head = &first[..end];
    match head.rfind(", ") {
        Some(i) if i > 0 => format!("{}, …", &head[..i]),
        _ => format!("{}…", head.trim_end()),
    }
}

fn bad_request(message: &str, option: Option<&str>) -> J {
    let mut j = error(400, message, Some("bad-request"));
    if let Some(o) = option {
        j["option"] = json!(o);
    }
    j
}

fn error(status: u16, message: &str, code: Option<&str>) -> J {
    let mut m = Map::new();
    m.insert("status".into(), json!(status));
    m.insert("error".into(), json!(message));
    if let Some(c) = code {
        m.insert("code".into(), json!(c));
    }
    J::Object(m)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call(req: J) -> J {
        serde_json::from_str(&format(&req.to_string())).unwrap()
    }

    #[test]
    fn formats_like_the_endpoint() {
        let r = call(json!({ "text": "ASK {}", "language": "sparql", "cursorOffset": 3 }));
        assert_eq!(r["text"], "ASK {}\n");
        assert_eq!(r["changed"], true);
        assert_eq!(r["language"], "sparql");
        assert_eq!(r["cursorOffset"], 3);
        // the language from the text
        let r = call(json!({ "text": "ASK {}\n" }));
        assert_eq!(
            (r["language"].as_str(), r["changed"].as_bool()),
            (Some("sparql"), Some(false))
        );
    }

    #[test]
    fn formats_every_language() {
        for (language, text) in [
            (
                "turtle",
                "@prefix ex: <http://example.org/> . ex:a ex:b ex:c .",
            ),
            (
                "trig",
                "<http://example.org/g> { <http://example.org/a> <http://example.org/b> 1 }",
            ),
            ("ntriples", "<http://a>   <http://b> <http://c>."),
            ("nquads", "<http://a> <http://b> \"c\"   <http://g>  ."),
            ("jsonld", r#"{"@id":"http://a","http://b":1}"#),
        ] {
            let r = call(json!({ "text": text, "language": language }));
            assert_eq!(
                (r["language"].as_str(), r["changed"].as_bool()),
                (Some(language), Some(true)),
                "{r}"
            );
            let again = call(json!({ "text": r["text"], "language": language }));
            assert_eq!(again["changed"], false, "{language}: {again}");
        }
    }

    #[test]
    fn errors_carry_the_endpoint_status() {
        let r = call(json!({ "text": "select * {", "language": "sparql" }));
        assert_eq!(
            (r["status"].as_u64(), r["code"].as_str()),
            (Some(400), Some("syntax"))
        );
        assert_eq!(
            (r["line"].as_u64(), r["column"].as_u64()),
            (Some(1), Some(11))
        );
        assert_eq!(r["language"], "sparql");
        // the endpoint's words: the head of spargebra's list of expected tokens, all of it
        // in `detail`
        let e = r["error"].as_str().unwrap();
        assert!(
            e.starts_with("SPARQL syntax error at line 1, column 11: expected") && e.ends_with('…'),
            "{e}"
        );
        assert!(r["detail"].as_str().unwrap().len() > e.len(), "{r}");
        let r = call(json!({ "text": "<a> <b> .", "language": "turtle" }));
        assert_eq!(
            (r["code"].as_str(), r["language"].as_str()),
            (Some("syntax"), Some("turtle"))
        );
        assert!(
            r["error"]
                .as_str()
                .unwrap()
                .starts_with("Turtle syntax error at line 1")
        );
        let r = call(json!({ "text": "x", "options": { "lineWidth": {} } }));
        assert_eq!(
            (r["code"].as_str(), r["option"].as_str()),
            (Some("bad-request"), Some("lineWidth"))
        );
        let r = call(json!({ "text": "x", "language": "sparql", "options": { "lineWidth": 7 } }));
        assert_eq!(r["code"], "bad-request");
        assert_eq!(r["option"], "lineWidth");
        let r = call(json!({ "text": "x", "language": "rdfxml" }));
        assert_eq!(r["status"], 415);
        let r = call(json!({ "text": "ASK {}", "cursorOffset": 99 }));
        assert_eq!(r["status"], 400);
        assert_eq!(call(json!([1]))["status"], 400);
    }
}
