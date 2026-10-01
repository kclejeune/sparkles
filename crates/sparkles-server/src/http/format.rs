//! `POST /$/format`: format a document. A JSON body (`{text, language?, cursorOffset?,
//! options?}`, the UI) gets a JSON answer with the cursor mapped (in UTF-16 code units);
//! a raw body (curl) gets the formatted text in its own media type, with
//! `Sparkles-Format-Changed`. The style options are camelCase keys, in the JSON body or
//! the query string (the body wins). Formatting runs on a blocking thread behind one
//! slot per core, within `--format-timeout`.

use super::{ApiError, ApiResult, Params, St, blocking, content_type, err, read_body};
use crate::auth::Principal;
use crate::state::{AppState, FormatEndpoint};
use axum::Router;
use axum::extract::{DefaultBodyLimit, Extension};
use axum::http::{HeaderMap, StatusCode, Uri, header};
use axum::response::IntoResponse;
use axum::routing::post;
use serde_json::{Value as J, json};
use sparkles_fmt::options::{self, OptionError, Value};
use sparkles_fmt::{Detection, FormatError, Language, Options};
use std::sync::Arc;

/// `true` or `false`: whether a raw body's formatted text differs from it.
pub const SPARKLES_FORMAT_CHANGED: &str = "sparkles-format-changed";

pub fn routes(st: &AppState) -> Router<Arc<AppState>> {
    let limit = st.format.max_bytes.map_or(usize::MAX, |b| b as usize);
    Router::new().route(
        "/$/format",
        post(format).layer(DefaultBodyLimit::max(limit)),
    )
}

/// What to format, from either body form.
struct Job {
    text: String,
    language: Language,
    opts: Options,
    /// the media type of a raw body (`None`: JSON)
    raw: Option<String>,
}

async fn format(
    axum::extract::State(st): St,
    Extension(p): Extension<Principal>,
    uri: Uri,
    headers: HeaderMap,
    body: axum::body::Body,
) -> ApiResult {
    match st.format.endpoint {
        FormatEndpoint::Off => {
            return Err(err(
                StatusCode::NOT_FOUND,
                "formatting is turned off on this server (--format-endpoint off)",
            ));
        }
        FormatEndpoint::Authenticated if p.is_anonymous() => {
            return Err(err(
                StatusCode::UNAUTHORIZED,
                "formatting needs a signed-in caller on this server (--format-endpoint authenticated)",
            ));
        }
        _ => {}
    }
    let bytes = read_body(body, st.format.max_bytes, "--format-max-mb").await?;
    let params = Params::from_query(&uri);
    let opts = query_options(&params)?;
    let mt = content_type(&headers);
    let mut job = if mt == "application/json" {
        json_job(&bytes, &params, opts)?
    } else {
        raw_job(&bytes, &mt, &params, opts)?
    };
    // a slot, then the work, all within the deadline
    let deadline = std::time::Instant::now() + st.format.timeout;
    let permit = tokio::time::timeout_at(
        tokio::time::Instant::from_std(deadline),
        st.format.permits.clone().acquire_owned(),
    )
    .await
    .map_err(|_| timed_out())?
    .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    job.opts.deadline = Some(deadline);
    let (job, result) = blocking(move || {
        let _permit = permit;
        let r = sparkles_fmt::format(&job.text, job.language, &job.opts);
        Ok((job, r))
    })
    .await?;
    let f = result.map_err(|e| format_error(&e, &job))?;
    Ok(match job.raw {
        Some(mt) => (
            StatusCode::OK,
            [
                (header::CONTENT_TYPE, mt),
                (
                    header::HeaderName::from_static(SPARKLES_FORMAT_CHANGED),
                    f.changed.to_string(),
                ),
            ],
            f.text,
        )
            .into_response(),
        None => {
            let warnings: Vec<J> = f
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
            axum::Json(json!({
                "cursorOffset": f.cursor.map(|c| byte_to_utf16(&f.text, c)),
                "text": f.text,
                "changed": f.changed,
                "language": f.language.name(),
                "warnings": warnings,
            }))
            .into_response()
        }
    })
}

fn timed_out() -> ApiError {
    err(
        StatusCode::REQUEST_TIMEOUT,
        "formatting did not finish within --format-timeout",
    )
}

fn bad_request(msg: impl Into<String>) -> ApiError {
    ApiError(
        StatusCode::BAD_REQUEST,
        json!({ "error": msg.into(), "code": "bad-request" }),
    )
}

/// A bad option, named in its HTTP spelling.
fn bad_option(e: OptionError) -> ApiError {
    let name = options::camel(&e.key).unwrap_or(&e.key).to_string();
    ApiError(
        StatusCode::BAD_REQUEST,
        json!({
            "error": format!("{name}: {}", e.message),
            "code": "bad-request",
            "option": name,
        }),
    )
}

/// The kebab-case key of an HTTP option name (camelCase only).
fn http_key(name: &str) -> Result<&'static str, ApiError> {
    options::KEYS
        .iter()
        .find(|(_, camel)| *camel == name)
        .map(|(kebab, _)| *kebab)
        .ok_or_else(|| {
            ApiError(
                StatusCode::BAD_REQUEST,
                json!({
                    "error": format!("{name}: unknown option"),
                    "code": "bad-request",
                    "option": name,
                }),
            )
        })
}

/// The options of the query string: every key in camelCase, `prefixGroup=a,b` once per
/// group, and `language`.
fn query_options(params: &Params) -> ApiResult<Options> {
    let mut o = Options::default();
    let mut groups = Vec::new();
    for (k, v) in &params.0 {
        match k.as_str() {
            "language" => {}
            "prefixGroup" => groups.push(options::group_from_list(v).map_err(bad_option)?),
            name => {
                let key = http_key(name)?;
                let value = match key {
                    "line-width" | "indent-width" => v
                        .parse::<i64>()
                        .map_or_else(|_| Value::Str(v.clone()), Value::Int),
                    "sort" | "prune-prefixes" | "type-shorthand" | "compact-iris"
                    | "align-values" => match v.as_str() {
                        "true" => Value::Bool(true),
                        "false" => Value::Bool(false),
                        _ => Value::Str(v.clone()),
                    },
                    "prefix-groups" => {
                        return Err(bad_option(OptionError {
                            key: key.into(),
                            message: "in a query string, give each group as prefixGroup=a,b".into(),
                        }));
                    }
                    _ => Value::Str(v.clone()),
                };
                options::set(&mut o, key, value).map_err(bad_option)?;
            }
        }
    }
    if !groups.is_empty() {
        options::set(&mut o, "prefix-groups", Value::Groups(groups)).map_err(bad_option)?;
    }
    Ok(o)
}

/// The `options` object of a JSON body, over the query string's.
fn json_options(v: &J, o: &mut Options) -> ApiResult<()> {
    let Some(obj) = v.as_object() else {
        return Err(bad_request("`options` must be an object"));
    };
    for (name, v) in obj {
        let key = http_key(name)?;
        let value = match v {
            J::Null => continue,
            J::Bool(b) => Value::Bool(*b),
            J::Number(n) => match n.as_i64() {
                Some(i) => Value::Int(i),
                None => Value::Str(n.to_string()),
            },
            J::String(s) => Value::Str(s.clone()),
            J::Array(groups) => {
                let groups: Option<Vec<Vec<String>>> = groups
                    .iter()
                    .map(|g| {
                        g.as_array()?
                            .iter()
                            .map(|l| l.as_str().map(str::to_string))
                            .collect()
                    })
                    .collect();
                match groups {
                    Some(g) => Value::Groups(g),
                    None => {
                        return Err(bad_option(OptionError {
                            key: key.into(),
                            message: "expected an array of arrays of prefix labels".into(),
                        }));
                    }
                }
            }
            J::Object(_) => Value::Str(v.to_string()),
        };
        options::set(o, key, value).map_err(bad_option)?;
    }
    Ok(())
}

fn json_job(bytes: &[u8], params: &Params, mut opts: Options) -> ApiResult<Job> {
    let body: J =
        serde_json::from_slice(bytes).map_err(|e| bad_request(format!("invalid JSON: {e}")))?;
    let Some(obj) = body.as_object() else {
        return Err(bad_request("expected a JSON object"));
    };
    let Some(text) = obj.get("text").and_then(J::as_str) else {
        return Err(bad_request("expected `text`, the document to format"));
    };
    if let Some(o) = obj.get("options") {
        json_options(o, &mut opts)?;
    }
    let name = match obj.get("language") {
        None | Some(J::Null) => params.get("language"),
        Some(J::String(l)) => Some(l.as_str()),
        Some(_) => return Err(bad_request("`language` must be a string")),
    };
    let language = language(name, None, text)?;
    opts.cursor = match obj.get("cursorOffset") {
        None | Some(J::Null) => None,
        Some(v) => {
            let units = v
                .as_u64()
                .ok_or_else(|| bad_request("`cursorOffset` must be a non-negative integer"))?;
            Some(
                utf16_to_byte(text, units)
                    .ok_or_else(|| bad_request("`cursorOffset` is beyond the end of the text"))?,
            )
        }
    };
    Ok(Job {
        text: text.to_string(),
        language,
        opts,
        raw: None,
    })
}

fn raw_job(bytes: &[u8], mt: &str, params: &Params, opts: Options) -> ApiResult<Job> {
    if mt == "application/rdf+xml" {
        return Err(err(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            sparkles_fmt::RDF_XML_MESSAGE,
        ));
    }
    let by_type = Language::from_media_type(mt);
    if by_type.is_none() && mt != "text/plain" {
        return Err(err(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            format!(
                "cannot format a body of type '{mt}': send application/json, or the document with its own media type"
            ),
        ));
    }
    let text = std::str::from_utf8(bytes)
        .map_err(|_| bad_request("the body is not UTF-8"))?
        .to_string();
    let name = params.get("language");
    if name.is_none() && by_type.is_none() {
        return Err(bad_request(
            "a text/plain body needs the language parameter",
        ));
    }
    let language = language(name, by_type, &text)?;
    Ok(Job {
        text,
        language,
        opts,
        raw: Some(mt.to_string()),
    })
}

/// The language a request names, else the body's media type, else what the text looks
/// like.
fn language(name: Option<&str>, by_type: Option<Language>, text: &str) -> ApiResult<Language> {
    let rdf_xml = || {
        err(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            sparkles_fmt::RDF_XML_MESSAGE,
        )
    };
    if let Some(name) = name {
        if name.eq_ignore_ascii_case("rdfxml") {
            return Err(rdf_xml());
        }
        return Language::from_name(name).ok_or_else(|| {
            bad_request(format!(
                "unknown language '{name}': sparql, turtle, trig, ntriples, nquads or jsonld"
            ))
        });
    }
    if let Some(l) = by_type {
        return Ok(l);
    }
    match sparkles_fmt::detect(None, text) {
        Detection::Lang(l) => Ok(l),
        Detection::RdfXml => Err(rdf_xml()),
        _ => Err(bad_request(
            "cannot tell the language of the text: set `language`",
        )),
    }
}

fn format_error(e: &FormatError, job: &Job) -> ApiError {
    match e {
        FormatError::Syntax {
            message,
            line,
            column,
            ..
        } => {
            // the head of the parser's message; the whole of it in `detail`
            let short = crate::fmt::report::short_message(message);
            let mut body = json!({
                "error": format!(
                    "{} syntax error at line {line}, column {column}: {short}",
                    job.language.display_name()
                ),
                "line": line,
                "column": column,
                "code": "syntax",
                "language": job.language.name(),
            });
            if short != message.trim() {
                body["detail"] = message.as_str().into();
            }
            ApiError(StatusCode::BAD_REQUEST, body)
        }
        FormatError::Unsupported { .. } | FormatError::Unsafe { .. } => {
            tracing::warn!(
                code = e.code(),
                sha256 = %sha256_hex(&job.text),
                "the formatter refused its output: {e}"
            );
            ApiError(
                StatusCode::UNPROCESSABLE_ENTITY,
                json!({ "error": e.to_string(), "code": e.code() }),
            )
        }
        FormatError::UnsupportedLanguage { .. } => {
            err(StatusCode::UNSUPPORTED_MEDIA_TYPE, e.to_string())
        }
        FormatError::Timeout => timed_out(),
        FormatError::TooLarge => err(StatusCode::PAYLOAD_TOO_LARGE, e.to_string()),
    }
}

fn sha256_hex(text: &str) -> String {
    use sha2::Digest;
    sha2::Sha256::digest(text.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// The byte offset of a UTF-16 offset (inside a surrogate pair: the next character);
/// `None` past the end.
fn utf16_to_byte(text: &str, units: u64) -> Option<usize> {
    let mut at = 0u64;
    for (i, c) in text.char_indices() {
        if at >= units {
            return Some(i);
        }
        at += c.len_utf16() as u64;
    }
    (at >= units).then_some(text.len())
}

/// The UTF-16 offset of a byte offset.
fn byte_to_utf16(text: &str, byte: usize) -> usize {
    let mut b = byte.min(text.len());
    while !text.is_char_boundary(b) {
        b -= 1;
    }
    text[..b].encode_utf16().count()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utf16_offsets() {
        let t = "a😀b\u{e9}";
        assert_eq!(utf16_to_byte(t, 0), Some(0));
        assert_eq!(utf16_to_byte(t, 1), Some(1));
        // inside the surrogate pair: the next character
        assert_eq!(utf16_to_byte(t, 2), Some(5));
        assert_eq!(utf16_to_byte(t, 3), Some(5));
        assert_eq!(utf16_to_byte(t, 4), Some(6));
        assert_eq!(utf16_to_byte(t, 5), Some(t.len()));
        assert_eq!(utf16_to_byte(t, 6), None);
        for (bytes, units) in [(0, 0), (1, 1), (5, 3), (6, 4), (t.len(), 5)] {
            assert_eq!(byte_to_utf16(t, bytes), units);
        }
        assert_eq!(utf16_to_byte("", 0), Some(0));
    }
}
