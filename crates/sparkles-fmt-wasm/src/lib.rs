//! The formatter for the browser (`wasm32-unknown-unknown`, wasm-bindgen): the UI formats
//! without a round trip and falls back to `POST /$/format` when this module does not load.
//! [`format`] takes the endpoint's JSON request body and returns its JSON answer, so the
//! two are interchangeable: `{ text, language?, cursorOffset?, options? }` in (the cursor
//! in UTF-16 code units, the options in camelCase), and out either
//! `{ text, changed, language, cursorOffset, warnings }` or an error object with the
//! endpoint's `status`, `error`, `code` and, for syntax errors, `line`, `column`,
//! `language` and (when `error` holds only the head of the parser's message) `detail`.
//!
//! [`lint`] does the same for `POST /$/lint`: `{ text, language?, rules?, fix? }` in, and
//! `{ language, diagnostics, text?, applied? }` out, with each diagnostic's range also in
//! UTF-16 code units (`from`, `to`) for the editor.
//!
//! `mise run ui:wasm` builds it into the UI (`scripts/build-fmt-wasm.sh`). The module has
//! no deadline (`std` reads no clock on this target): the UI formats an editor's text.

use serde_json::{Map, Value as J, json};
use sparkles_fmt::lint::{self, LintError, LintOptions};
use sparkles_fmt::options::{self, Value};
use sparkles_fmt::pattern::{Group, Item, Locked, Path, Pattern, PatternError, Term};
use sparkles_fmt::{Detection, FormatError, Language, Options};
use std::ops::Range;
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

/// Lint the document of a `POST /$/lint` JSON body; the answer is the endpoint's JSON.
#[wasm_bindgen]
pub fn lint(request: &str) -> String {
    match run_lint(request) {
        Ok(v) | Err(v) => v.to_string(),
    }
}

fn run_lint(request: &str) -> Result<J, J> {
    let body: J = serde_json::from_str(request)
        .map_err(|e| bad_request(&format!("invalid JSON: {e}"), None))?;
    let obj = body
        .as_object()
        .ok_or_else(|| bad_request("expected a JSON object", None))?;
    let text = obj
        .get("text")
        .and_then(J::as_str)
        .ok_or_else(|| bad_request("expected `text`, the document to lint", None))?;
    let lang = language(obj.get("language"), text)?;
    let mut opts = LintOptions::default();
    match obj.get("rules") {
        None | Some(J::Null) => {}
        Some(J::Object(rules)) => {
            for (rule, level) in rules {
                let level = level.as_str().ok_or_else(|| {
                    bad_request(&format!("{rule}: expected a severity or \"off\""), None)
                })?;
                opts.set(rule, level).map_err(|e| bad_request(&e, None))?;
            }
        }
        Some(_) => return Err(bad_request("`rules` must be an object", None)),
    }
    let fix = match obj.get("fix") {
        None | Some(J::Null) => false,
        Some(J::Bool(b)) => *b,
        Some(_) => return Err(bad_request("`fix` must be true or false", None)),
    };
    if fix {
        let f = lint::fix(text, lang, &opts).map_err(|e| lint_error(&e))?;
        Ok(json!({
            "language": lang.name(),
            "text": f.text,
            "applied": f.applied,
            "diagnostics": f.diagnostics.iter().map(|d| diagnostic(&f.text, d)).collect::<Vec<J>>(),
        }))
    } else {
        let l = lint::lint(text, lang, &opts).map_err(|e| lint_error(&e))?;
        Ok(json!({
            "language": lang.name(),
            "diagnostics": l.diagnostics.iter().map(|d| diagnostic(text, d)).collect::<Vec<J>>(),
        }))
    }
}

/// The pattern of a query for the visual query builder (spec U08 §3.2): `{ text, strict? }`
/// in (`strict` adds the server parser's checks, for the text of a canvas edit),
/// and out either `{ ok: true, form, prologue, root, projection, limit, offset,
/// modifiers, locked }` or `{ ok: false, error, line, column }` (`code: "update"` for an
/// update request, `"bad-request"` for a request that is not `{ text }`). Every range is
/// `{ from, to }` in UTF-16 code units, as `lint` gives them.
#[wasm_bindgen]
pub fn pattern(request: &str) -> String {
    match run_pattern(request) {
        Ok(v) | Err(v) => v.to_string(),
    }
}

fn run_pattern(request: &str) -> Result<J, J> {
    let fail = |code: &str, error: &str, line: u32, column: u32| json!({ "ok": false, "code": code, "error": error, "line": line, "column": column });
    let body: J = serde_json::from_str(request)
        .map_err(|e| fail("bad-request", &format!("invalid JSON: {e}"), 0, 0))?;
    let text = body
        .get("text")
        .and_then(J::as_str)
        .ok_or_else(|| fail("bad-request", "expected `text`, the query", 0, 0))?;
    let strict = match body.get("strict") {
        None | Some(J::Null) => false,
        Some(J::Bool(b)) => *b,
        Some(_) => return Err(fail("bad-request", "`strict` must be true or false", 0, 0)),
    };
    let answer = sparkles_fmt::pattern::pattern(text);
    // the server's own checks too, for the text of a canvas edit
    let answer = match answer {
        Ok(p) if strict => sparkles_fmt::pattern::check(text).map(|()| p),
        other => other,
    };
    match answer {
        Ok(p) => Ok(pattern_json(text, &p)),
        Err(PatternError::Update) => Err(fail(
            "update",
            "Updates are not drawn. The builder edits queries.",
            1,
            1,
        )),
        Err(PatternError::Syntax {
            message,
            line,
            column,
        }) => Err(fail(
            "syntax",
            &format!(
                "SPARQL syntax error at line {line}, column {column}: {}",
                short_message(&message)
            ),
            line,
            column,
        )),
    }
}

/// Byte offsets to UTF-16 offsets, through a table built once per text (the identity for
/// ASCII text).
struct Units {
    table: Option<Vec<u32>>,
}

impl Units {
    fn new(text: &str) -> Units {
        if text.is_ascii() {
            return Units { table: None };
        }
        let mut table = vec![0u32; text.len() + 1];
        let mut at = 0u32;
        for (i, c) in text.char_indices() {
            // inside a character: its start
            table[i..i + c.len_utf8()].fill(at);
            at += c.len_utf16() as u32;
        }
        table[text.len()] = at;
        Units { table: Some(table) }
    }

    fn at(&self, byte: usize) -> usize {
        match &self.table {
            None => byte,
            Some(t) => t[byte.min(t.len() - 1)] as usize,
        }
    }

    fn range(&self, r: &Range<usize>) -> J {
        json!({ "from": self.at(r.start), "to": self.at(r.end) })
    }
}

fn pattern_json(text: &str, p: &Pattern) -> J {
    let u = Units::new(text);
    let mut j = json!({
        "ok": true,
        "form": p.form.name(),
        "prologue": {
            "base": p.base,
            "prefixes": p.prefixes.iter().map(|x| json!({
                "prefix": x.prefix, "iri": x.iri, "range": u.range(&x.range),
            })).collect::<Vec<J>>(),
        },
        "root": p.root.as_ref().map(|g| group_json(&u, g)),
        "projection": p.projection.as_ref().map(|x| json!({
            "vars": x.vars, "star": x.star, "distinct": x.distinct, "locked": x.locked,
            "range": u.range(&x.range),
            "keyword": u.range(&x.keyword),
            "distinctRange": x.distinct_range.as_ref().map(|r| u.range(r)),
            "starRange": x.star_range.as_ref().map(|r| u.range(r)),
            "items": x.items.iter().map(|i| json!({
                "var": i.var, "expression": i.expression, "range": u.range(&i.range),
                "varRefs": var_refs_json(&u, &i.var_refs),
            })).collect::<Vec<J>>(),
        })),
        "modifiers": p.modifiers.iter().map(|l| locked_json(&u, l)).collect::<Vec<J>>(),
        "locked": p.locked.iter().map(|l| locked_json(&u, l)).collect::<Vec<J>>(),
        "queryStart": u.at(p.query_start),
    });
    for (key, n) in [("limit", &p.limit), ("offset", &p.offset)] {
        if let Some(n) = n {
            j[key] = json!({ "value": n.value, "range": u.range(&n.range) });
        }
    }
    j
}

fn group_json(u: &Units, g: &Group) -> J {
    json!({
        "kind": g.kind.name(),
        "range": u.range(&g.range),
        "inner": u.range(&g.inner),
        "items": g.items.iter().map(|i| item_json(u, i)).collect::<Vec<J>>(),
    })
}

fn item_json(u: &Units, i: &Item) -> J {
    match i {
        Item::Triple(t) => json!({
            "type": "triple",
            "s": term_json(u, &t.s),
            "p": {
                "kind": t.p.kind.name(), "text": t.p.text, "value": t.p.value,
                "path": t.p.path.as_ref().map(path_json),
            },
            "o": term_json(u, &t.o),
            "range": u.range(&t.range),
            "sRange": u.range(&t.s.range),
            "pRange": u.range(&t.p.range),
            "oRange": u.range(&t.o.range),
            "stmt": t.stmt,
            "entry": t.entry,
            "object": t.object,
            "entryRange": u.range(&t.entry_range),
            "objectRange": u.range(&t.object_range),
        }),
        Item::Filter(f) => json!({
            "type": "filter",
            "expr": f.form.as_ref().map(|x| json!({ "op": x.op, "var": x.var, "args": x.args })),
            "vars": f.vars,
            "varRefs": var_refs_json(u, &f.var_refs),
            "text": f.text,
            "range": u.range(&f.range),
            "dot": f.dot.as_ref().map(|d| u.range(d)),
        }),
        Item::Group(g) => {
            let mut j = group_json(u, g);
            j["type"] = json!("group");
            j["dot"] = json!(g.dot.as_ref().map(|d| u.range(d)));
            j
        }
        Item::Locked(l) => {
            let mut j = locked_json(u, l);
            j["type"] = json!("locked");
            j
        }
    }
}

fn term_json(u: &Units, t: &Term) -> J {
    json!({ "kind": t.kind.name(), "text": t.text, "value": t.value, "range": u.range(&t.range) })
}

fn path_json(p: &Path) -> J {
    match p {
        Path::Iri { text, iri } => json!({ "kind": "iri", "text": text, "iri": iri }),
        Path::Seq(v) => {
            json!({ "kind": "seq", "items": v.iter().map(path_json).collect::<Vec<J>>() })
        }
        Path::Alt(v) => {
            json!({ "kind": "alt", "items": v.iter().map(path_json).collect::<Vec<J>>() })
        }
        Path::Inv(i) => json!({ "kind": "inv", "item": path_json(i) }),
        Path::Mod { op, item } => json!({ "kind": "mod", "op": op, "item": path_json(item) }),
    }
}

fn locked_json(u: &Units, l: &Locked) -> J {
    json!({
        "kind": l.kind, "vars": l.vars, "binds": l.binds,
        "varRefs": var_refs_json(u, &l.var_refs),
        "text": l.text, "range": u.range(&l.range),
        "end": u.at(l.end), "dot": l.dot.as_ref().map(|d| u.range(d)),
    })
}

fn var_refs_json(u: &Units, refs: &[sparkles_fmt::pattern::VarRef]) -> J {
    json!(
        refs.iter()
            .map(|v| json!({ "name": v.name, "range": u.range(&v.range) }))
            .collect::<Vec<J>>()
    )
}

/// A finding as `POST /$/lint` writes it: lines and columns as `sparkles lint` prints
/// them, and `from`/`to` in UTF-16 code units. The fix is there only for rules whose
/// fixes are safe.
fn diagnostic(text: &str, d: &lint::Diagnostic) -> J {
    let u16 = |b: usize| sparkles_fmt::byte_to_utf16(text, b);
    let mut j = json!({
        "rule": d.rule,
        "severity": d.severity.name(),
        "message": d.message,
        "line": d.line,
        "column": d.column,
        "endLine": d.end_line,
        "endColumn": d.end_column,
        "from": u16(d.start),
        "to": u16(d.end),
    });
    if let Some(f) = d
        .fix
        .as_ref()
        .filter(|_| lint::rule(d.rule).is_some_and(|r| r.safe_fix))
    {
        j["fix"] = json!({
            "title": f.title,
            "edits": f.edits.iter().map(|e| json!({
                "from": u16(e.start),
                "to": u16(e.end),
                "insert": e.insert,
            })).collect::<Vec<J>>(),
        });
    }
    j
}

/// The endpoint's answer to a linting error.
fn lint_error(e: &LintError) -> J {
    let status = match e {
        LintError::UnsupportedLanguage(_) => 415,
        LintError::Timeout => 408,
        LintError::TooLarge => 413,
        LintError::UnsafeFix => 422,
    };
    error(status, &e.to_string(), Some(e.code()))
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
                    "unknown language '{n}': sparql, turtle, trig, ntriples, nquads, jsonld or shaclc"
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
            (
                "shaclc",
                "PREFIX ex: <http://example.org/>\nshape ex:S -> ex:C { ex:p IRI [1..1] . }",
            ),
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

    fn call_lint(req: J) -> J {
        serde_json::from_str(&lint(&req.to_string())).unwrap()
    }

    #[test]
    fn lints_like_the_endpoint() {
        let text = "PREFIX ex: <http://x/>\nSELECT ?s { ?s ?p \"\u{1F600}\"@en-us }";
        let r = call_lint(json!({ "text": text, "rules": { "single-use-variable": "off" } }));
        assert_eq!(r["language"], "sparql");
        let d = r["diagnostics"].as_array().unwrap();
        assert_eq!(d.len(), 2, "{r}");
        assert_eq!(d[0]["rule"], "unused-prefix");
        assert_eq!(d[0]["severity"], "warning");
        assert_eq!(
            (d[0]["from"].as_u64(), d[0]["to"].as_u64()),
            (Some(0), Some(22))
        );
        assert_eq!(d[0]["fix"]["edits"][0]["insert"], "");
        // UTF-16 offsets past the emoji (two code units)
        let tag = text[..text.find("@en-us").unwrap()].encode_utf16().count() as u64;
        assert_eq!(d[1]["from"].as_u64(), Some(tag));
        assert_eq!(d[1]["fix"]["edits"][0]["insert"], "@en-US");
        let r = call_lint(json!({ "text": text, "fix": true }));
        assert_eq!(r["applied"], 2);
        assert_eq!(r["text"], "SELECT ?s { ?s ?p \"\u{1F600}\"@en-US }");
        // errors
        let r = call_lint(json!({ "text": "<a> <b> <c> .", "language": "rdfxml" }));
        assert_eq!(r["status"], 415);
        let r = call_lint(
            json!({ "text": "<http://a> <http://b> <http://c> .", "language": "ntriples" }),
        );
        assert_eq!(r["language"], "ntriples");
        assert_eq!(r["diagnostics"], json!([]));
        let r = call_lint(json!({ "text": "x", "language": "sparql", "rules": { "nope": "off" } }));
        assert_eq!(r["status"], 400);
        let r = call_lint(json!({ "text": "SELECT * {", "language": "sparql" }));
        assert_eq!(r["diagnostics"][0]["rule"], "syntax");
    }

    fn call_pattern(req: J) -> J {
        serde_json::from_str(&pattern(&req.to_string())).unwrap()
    }

    #[test]
    fn patterns_with_utf16_ranges() {
        let text = "PREFIX ex: <http://example.org/>\n\
                    SELECT ?s WHERE { ?s ex:label \"\u{1F600}\" ; ex:p ?o . BIND(1 AS ?y) # one\n} LIMIT 3";
        let r = call_pattern(json!({ "text": text }));
        assert_eq!(r["ok"], true, "{r}");
        assert_eq!(r["form"], "select");
        assert_eq!(r["prologue"]["prefixes"][0]["prefix"], "ex");
        assert_eq!(r["projection"]["vars"], json!(["s"]));
        assert_eq!(r["limit"]["value"], 3);
        assert!(r.get("offset").is_none());
        let items = r["root"]["items"].as_array().unwrap();
        assert_eq!(items.len(), 3);
        assert_eq!(items[0]["type"], "triple");
        assert_eq!(items[0]["p"]["value"], "http://example.org/label");
        assert_eq!(items[1]["o"]["value"], "o");
        let units = |s: &str| s.encode_utf16().count() as u64;
        // after the emoji, offsets count two code units for it
        let o = &items[1]["oRange"];
        let from = units(&text[..text.find("?o").unwrap()]);
        assert_eq!(
            (o["from"].as_u64(), o["to"].as_u64()),
            (Some(from), Some(from + 2))
        );
        assert_eq!(items[2]["type"], "locked");
        assert_eq!(items[2]["kind"], "BIND");
        assert_eq!(items[2]["text"], "BIND(1 AS ?y) # one");
        let s: Vec<u16> = text.encode_utf16().collect();
        let l = &items[2]["range"];
        let (a, b) = (
            l["from"].as_u64().unwrap() as usize,
            l["to"].as_u64().unwrap() as usize,
        );
        assert_eq!(String::from_utf16(&s[a..b]).unwrap(), "BIND(1 AS ?y) # one");
        let path = call_pattern(json!({ "text": "SELECT * { ?a <http://x/p>+ ?b }" }));
        assert_eq!(
            path["root"]["items"][0]["p"]["path"],
            json!({ "kind": "mod", "op": "+", "item": { "kind": "iri", "text": "<http://x/p>", "iri": "http://x/p" } })
        );
    }

    #[test]
    fn pattern_errors() {
        let r = call_pattern(json!({ "text": "SELECT * {" }));
        assert_eq!(
            (
                r["ok"].as_bool(),
                r["code"].as_str(),
                r["line"].as_u64(),
                r["column"].as_u64()
            ),
            (Some(false), Some("syntax"), Some(1), Some(11))
        );
        assert!(
            r["error"]
                .as_str()
                .unwrap()
                .starts_with("SPARQL syntax error at line 1")
        );
        let r = call_pattern(json!({ "text": "DELETE WHERE { ?s ?p ?o }" }));
        assert_eq!(r["code"], "update");
        // the lossless parser takes a scope error; the strict check does not
        let scope = "SELECT * { ?s ?p ?o BIND(1 AS ?o) }";
        assert_eq!(call_pattern(json!({ "text": scope }))["ok"], true);
        let r = call_pattern(json!({ "text": scope, "strict": true }));
        assert_eq!(
            (r["ok"].as_bool(), r["code"].as_str()),
            (Some(false), Some("syntax"))
        );
        // an undeclared prefix passes, as Run declares it
        let r = call_pattern(json!({ "text": "SELECT * { ?s ex:p ?o }", "strict": true }));
        assert_eq!(r["ok"], true, "{r}");
        assert_eq!(
            call_pattern(json!({ "text": "ASK {}", "strict": 1 }))["code"],
            "bad-request"
        );
        assert_eq!(call_pattern(json!({ "nope": 1 }))["code"], "bad-request");
        assert_eq!(pattern("{"), pattern("{"));
        assert!(pattern("{").contains("bad-request"));
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
