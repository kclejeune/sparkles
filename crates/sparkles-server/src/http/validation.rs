//! Write-time SHACL validation: `/$/validation/{ds}`, per-write options, the
//! `Sparkles-Validation` header and 422 rejections.

use super::*;
use sparkles::guard::{ValidationSummary, WriteOptions};

pub(super) const SPARKLES_VALIDATION: &str = "sparkles-validation";

/// The Turtle report of a rejection, served instead of JSON when the client asks for
/// `text/turtle` (see `error_request_id`).
#[derive(Clone)]
pub(super) struct RejectionTurtle(pub String);

/// Guard options of a write: its deadline, a bypass (`validate=false`, only on servers
/// started with `--allow-unvalidated-writes`), and `validationLimit`.
pub(super) fn write_options(
    st: &AppState,
    params: &Params,
    headers: &HeaderMap,
    deadline: Option<Duration>,
) -> ApiResult<WriteOptions> {
    let bypass = params.get("validate").is_some_and(|v| v == "false")
        || headers
            .get("sparkles-validate")
            .is_some_and(|v| v.as_bytes().eq_ignore_ascii_case(b"off"));
    if bypass && !st.allow_unvalidated_writes {
        return Err(err(
            StatusCode::FORBIDDEN,
            "validation bypass is disabled on this server",
        ));
    }
    let report_limit = match params.get("validationLimit") {
        Some(v) => Some(
            v.parse::<usize>()
                .ok()
                .filter(|n| (1..=10_000).contains(n))
                .ok_or_else(|| {
                    err(
                        StatusCode::BAD_REQUEST,
                        "validationLimit must be between 1 and 10000",
                    )
                })?,
        ),
        None => None,
    };
    Ok(WriteOptions {
        bypass_validation: bypass,
        deadline: deadline.map(|d| std::time::Instant::now() + d),
        cancel: None,
        report_limit,
    })
}

/// Add `Sparkles-Validation` to a write response.
pub(super) fn with_validation(mut resp: Response, v: Option<&ValidationSummary>) -> Response {
    if let Some(v) = v
        && let Ok(h) = header::HeaderValue::from_str(&v.header())
    {
        resp.headers_mut().insert(SPARKLES_VALIDATION, h);
    }
    resp
}

/// The 422 body of a rejected write, with the header and Turtle report kept for
/// `ApiError::into_response`.
pub(super) fn rejection(r: &sparkles::guard::Rejection) -> ApiError {
    let mut v = serde_json::to_value(&r.summary).unwrap_or(J::Null);
    v["head"] = json!(r.head);
    v["kind"] = json!(r.kind.name());
    let mut body = json!({ "error": r.to_string(), "validation": v });
    body["_validationHeader"] = json!(r.summary.header());
    if let Some(t) = &r.summary.report_turtle {
        body["_turtle"] = json!(t);
    }
    ApiError(StatusCode::UNPROCESSABLE_ENTITY, body)
}

#[cfg(feature = "shacl")]
mod handlers {
    use super::*;
    use sparkles_shacl::guard::{self, SetOutcome, ValidationConfig};

    fn status_json(ds: &Dataset) -> J {
        match ds.validation.read().as_ref() {
            Some(g) => json!({ "config": g.config(), "status": g.status() }),
            None => json!({ "config": null }),
        }
    }

    pub(in crate::http) async fn get(
        State(st): St,
        Path(name): Path<String>,
    ) -> ApiResult<Json<J>> {
        let ds = dataset(&st, &name)?;
        Ok(Json(status_json(&ds)))
    }

    pub(in crate::http) async fn put(
        State(st): St,
        Path(name): Path<String>,
        body: Bytes,
    ) -> ApiResult {
        if st.read_only {
            return Err(err(StatusCode::FORBIDDEN, "server is read-only"));
        }
        let ds = dataset(&st, &name)?;
        let mut j: J = serde_json::from_slice(&body)
            .map_err(|e| err(StatusCode::BAD_REQUEST, format!("invalid JSON: {e}")))?;
        if let Some(o) = j.as_object_mut() {
            o.entry("format").or_insert(json!(1));
            o.remove("updated");
        }
        let cfg: ValidationConfig = serde_json::from_value(j).map_err(|e| {
            err(
                StatusCode::BAD_REQUEST,
                format!("invalid configuration: {e}"),
            )
        })?;
        blocking(move || {
            let outcome = guard::set_config(&ds.store, Some(cfg)).map_err(config_error)?;
            match outcome {
                SetOutcome::Installed(g, _) => {
                    *ds.validation.write() = Some(g);
                    Ok(Json(status_json(&ds)).into_response())
                }
                SetOutcome::NotConforming(s) => Err(ApiError(
                    StatusCode::CONFLICT,
                    json!({
                        "error": "dataset does not conform; fix the data or use mode 'warn' first",
                        "validation": s,
                    }),
                )),
                SetOutcome::Removed => {
                    *ds.validation.write() = None;
                    Ok(Json(status_json(&ds)).into_response())
                }
            }
        })
        .await
    }

    pub(in crate::http) async fn delete(State(st): St, Path(name): Path<String>) -> ApiResult {
        if st.read_only {
            return Err(err(StatusCode::FORBIDDEN, "server is read-only"));
        }
        let ds = dataset(&st, &name)?;
        blocking(move || {
            guard::set_config(&ds.store, None).map_err(config_error)?;
            *ds.validation.write() = None;
            Ok(StatusCode::NO_CONTENT.into_response())
        })
        .await
    }

    /// Errors of setting a configuration: engine errors keep their status, anything
    /// else (a bad field, shapes that do not parse) is 400.
    fn config_error(e: anyhow::Error) -> ApiError {
        match e.downcast::<Error>() {
            Ok(e) => ApiError::from(e),
            Err(e) => err(StatusCode::BAD_REQUEST, format!("{e:#}")),
        }
    }
}

#[cfg(feature = "shacl")]
pub(super) use handlers::{
    delete as delete_validation, get as get_validation, put as put_validation,
};

#[cfg(not(feature = "shacl"))]
pub(super) async fn get_validation() -> ApiResult {
    Err(err(
        StatusCode::NOT_IMPLEMENTED,
        "built without the `shacl` feature",
    ))
}
#[cfg(not(feature = "shacl"))]
pub(super) use get_validation as put_validation;
#[cfg(not(feature = "shacl"))]
pub(super) use get_validation as delete_validation;
