//! The formatter for the browser (`wasm32-unknown-unknown`, wasm-bindgen): the UI formats
//! without a round trip and falls back to `POST /$/format` when this module does not load.
//! [`format`] takes the endpoint's JSON request body and returns its JSON answer, so the
//! two are interchangeable: `{ text, language?, cursorOffset?, options? }` in (the cursor
//! in UTF-16 code units, the options in camelCase), and out either
//! `{ text, changed, language, cursorOffset, warnings }` or an error object with the
//! endpoint's `status`, `error`, `code` and, for syntax errors, `line` and `column`.

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
            J::Object(_) => {
                return Err(bad_request(
                    &format!("{name}: unexpected object"),
                    Some(name),
                ));
            }
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
            let mut j = error(
                400,
                &format!(
                    "{} syntax error at line {line}, column {column}: {message}",
                    lang.display_name()
                ),
                Some("syntax"),
            );
            j["line"] = json!(line);
            j["column"] = json!(column);
            j["language"] = json!(lang.name());
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
    fn errors_carry_the_endpoint_status() {
        let r = call(json!({ "text": "select * {", "language": "sparql" }));
        assert_eq!(
            (r["status"].as_u64(), r["code"].as_str()),
            (Some(400), Some("syntax"))
        );
        assert_eq!(r["line"], 1);
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
