//! `POST /$/lint`: lint a SPARQL, Turtle or TriG document (spec X03). The JSON body is
//! `{text, language?, rules?, fix?}`; the answer is `{language, diagnostics}`, and with
//! `fix` also the fixed `text` and the number of fixes `applied`. Each diagnostic has
//! its range as lines and columns and, for the editor, as UTF-16 offsets (`from`, `to`).
//! The endpoint follows `--format-endpoint`, `--format-max-mb` and `--format-timeout`,
//! and shares the formatter's slots. The UI's WebAssembly module answers the same.

use super::{ApiError, ApiResult, St, blocking, content_type, err, read_body};
use crate::auth::Principal;
use crate::state::FormatEndpoint;
use axum::extract::Extension;
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use serde_json::{Value as J, json};
use sparkles_fmt::lint::{self, LintError, LintOptions};
use sparkles_fmt::{Detection, Language, byte_to_utf16};

fn bad_request(msg: impl Into<String>) -> ApiError {
    ApiError(
        StatusCode::BAD_REQUEST,
        json!({ "error": msg.into(), "code": "bad-request" }),
    )
}

pub(super) async fn lint_handler(
    axum::extract::State(st): St,
    Extension(p): Extension<Principal>,
    headers: HeaderMap,
    body: axum::body::Body,
) -> ApiResult {
    match st.format.endpoint {
        FormatEndpoint::Off => {
            return Err(err(
                StatusCode::NOT_FOUND,
                "linting is turned off on this server (--format-endpoint off)",
            ));
        }
        FormatEndpoint::Authenticated if p.is_anonymous() => {
            return Err(err(
                StatusCode::UNAUTHORIZED,
                "linting needs a signed-in caller on this server (--format-endpoint authenticated)",
            ));
        }
        _ => {}
    }
    if content_type(&headers) != "application/json" {
        return Err(err(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "send the document as application/json: {\"text\": …}",
        ));
    }
    let bytes = read_body(body, st.format.max_bytes, "--format-max-mb").await?;
    let req: J =
        serde_json::from_slice(&bytes).map_err(|e| bad_request(format!("invalid JSON: {e}")))?;
    let Some(obj) = req.as_object() else {
        return Err(bad_request("expected a JSON object"));
    };
    let Some(text) = obj.get("text").and_then(J::as_str) else {
        return Err(bad_request("expected `text`, the document to lint"));
    };
    let text = text.to_string();
    let language = match obj.get("language") {
        None | Some(J::Null) => match sparkles_fmt::detect(None, &text) {
            Detection::Lang(l) => l,
            _ => {
                return Err(bad_request(
                    "cannot tell the language of the text: set `language`",
                ));
            }
        },
        Some(J::String(n)) => Language::from_name(n).ok_or_else(|| {
            bad_request(format!("unknown language '{n}': sparql, turtle or trig"))
        })?,
        Some(_) => return Err(bad_request("`language` must be a string")),
    };
    let mut opts = LintOptions::default();
    match obj.get("rules") {
        None | Some(J::Null) => {}
        Some(J::Object(rules)) => {
            for (rule, level) in rules {
                let level = level.as_str().ok_or_else(|| {
                    bad_request(format!("{rule}: expected a severity or \"off\""))
                })?;
                opts.set(rule, level).map_err(bad_request)?;
            }
        }
        Some(_) => return Err(bad_request("`rules` must be an object")),
    }
    let fix = match obj.get("fix") {
        None | Some(J::Null) => false,
        Some(J::Bool(b)) => *b,
        Some(_) => return Err(bad_request("`fix` must be true or false")),
    };
    let deadline = std::time::Instant::now() + st.format.timeout;
    let permit = tokio::time::timeout_at(
        tokio::time::Instant::from_std(deadline),
        st.format.permits.clone().acquire_owned(),
    )
    .await
    .map_err(|_| timed_out())?
    .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    opts.deadline = Some(deadline);
    let answer = blocking(move || {
        let _permit = permit;
        Ok(if fix {
            lint::fix(&text, language, &opts).map(|f| {
                json!({
                    "language": language.name(),
                    "text": f.text,
                    "applied": f.applied,
                    "diagnostics": f.diagnostics.iter().map(|d| diagnostic(&f.text, d)).collect::<Vec<J>>(),
                })
            })
        } else {
            lint::lint(&text, language, &opts).map(|l| {
                json!({
                    "language": language.name(),
                    "diagnostics": l.diagnostics.iter().map(|d| diagnostic(&text, d)).collect::<Vec<J>>(),
                })
            })
        })
    })
    .await?;
    let j = answer.map_err(|e| lint_error(&e))?;
    Ok(axum::Json(j).into_response())
}

fn timed_out() -> ApiError {
    err(
        StatusCode::REQUEST_TIMEOUT,
        "linting did not finish within --format-timeout",
    )
}

/// A finding as JSON: lines and columns, `from` and `to` in UTF-16 code units, and the
/// fix of a rule whose fixes are safe.
pub(crate) fn diagnostic(text: &str, d: &lint::Diagnostic) -> J {
    let u16 = |b: usize| byte_to_utf16(text, b);
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

fn lint_error(e: &LintError) -> ApiError {
    let status = match e {
        LintError::UnsupportedLanguage(_) => StatusCode::UNSUPPORTED_MEDIA_TYPE,
        LintError::Timeout => return timed_out(),
        LintError::TooLarge => StatusCode::PAYLOAD_TOO_LARGE,
        LintError::UnsafeFix => StatusCode::UNPROCESSABLE_ENTITY,
    };
    ApiError(status, json!({ "error": e.to_string(), "code": e.code() }))
}
