//! Write-time validation (SHACL or ShEx): `/$/validation/{ds}`, per-write options, the
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
        message: super::conditional::commit_message(headers)?,
        // the handler names the caller (see `super::author`)
        author: None,
        precondition: None,
        no_wait: false,
        graphs: None,
        dry_run: super::dry_run::parse(st, params, headers)?,
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

#[cfg(any(feature = "shacl", feature = "shex"))]
mod handlers {
    use super::*;
    use crate::write_validation::none_json;
    use sparkles::guard::GuardLanguage;
    use sparkles::handles::GuardOutcome;

    fn status_json(ds: &Dataset) -> J {
        match ds.dataset.validation().guard().get() {
            Some(g) => g.json(),
            None => none_json(),
        }
    }

    /// The language a `PUT` body asks for: `language`, or SHACL without one.
    fn language_of(j: &J) -> ApiResult<GuardLanguage> {
        match j.get("language") {
            None | Some(J::Null) => Ok(GuardLanguage::Shacl),
            Some(l) => serde_json::from_value(l.clone()).map_err(|_| {
                err(
                    StatusCode::BAD_REQUEST,
                    format!("invalid configuration: unknown language {l} (\"shacl\" or \"shex\")"),
                )
            }),
        }
    }

    fn invalid(e: serde_json::Error) -> ApiError {
        err(
            StatusCode::BAD_REQUEST,
            format!("invalid configuration: {e}"),
        )
    }

    /// Set a SHACL configuration (format 1 or 2; written as format 2).
    #[cfg(feature = "shacl")]
    fn set_shacl(ds: &Dataset, j: J) -> ApiResult<GuardOutcome> {
        let cfg: sparkles_shacl::guard::ValidationConfig =
            serde_json::from_value(j).map_err(invalid)?;
        Ok(ds.dataset.validation().guard().set_shacl(cfg)?)
    }

    /// Set a ShEx configuration (format 2). The schema's imports resolve as for
    /// `/{ds}/shex`: `file:` IRIs under `--load-dir` (and relative IRIs against it),
    /// http(s) through the outbound policy; there are no inline import bodies or
    /// externs, so an EXTERNAL shape is an error.
    #[cfg(feature = "shex")]
    fn set_shex(st: &AppState, ds: &Dataset, mut j: J) -> ApiResult<GuardOutcome> {
        use sparkles_shex::guard::{self, ShexValidationConfig};
        if let Some(o) = j.as_object_mut() {
            o.entry("format").or_insert(json!(guard::CONFIG_FORMAT));
        }
        let cfg: ShexValidationConfig = serde_json::from_value(j).map_err(invalid)?;
        let budget = sparkles::outbound::RequestBudget::new(&st.outbound);
        let resolver = sparkles_shex::FileResolver {
            dirs: match &st.file_loads {
                sparkles::sparql::FileLoads::Under(d) => vec![d.clone()],
                _ => Vec::new(),
            },
            files: st.file_loads.clone(),
            outbound: Some((st.outbound.clone(), budget)),
            limits: st.shex_import_limits,
            ..Default::default()
        };
        Ok(ds.dataset.validation().guard().set_shex(cfg, &resolver)?)
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
        AdminBody(body): AdminBody,
    ) -> ApiResult {
        if st.read_only {
            return Err(err(StatusCode::FORBIDDEN, "server is read-only"));
        }
        let ds = dataset(&st, &name)?;
        let mut j: J = serde_json::from_slice(&body)
            .map_err(|e| err(StatusCode::BAD_REQUEST, format!("invalid JSON: {e}")))?;
        if let Some(o) = j.as_object_mut() {
            o.remove("updated");
        }
        let language = language_of(&j)?;
        blocking(move || {
            let outcome = match language {
                #[cfg(feature = "shacl")]
                GuardLanguage::Shacl => set_shacl(&ds, j)?,
                #[cfg(feature = "shex")]
                GuardLanguage::Shex => set_shex(&st, &ds, j)?,
                #[allow(unreachable_patterns)]
                l => {
                    let _ = (&st, j);
                    return Err(err(
                        StatusCode::NOT_IMPLEMENTED,
                        format!("built without the `{}` feature", l.name()),
                    ));
                }
            };
            match outcome {
                GuardOutcome::NotConforming(s) => Err(ApiError(
                    StatusCode::CONFLICT,
                    json!({
                        "error": "dataset does not conform; fix the data or use mode 'warn' first",
                        "validation": s,
                    }),
                )),
                _ => Ok(Json(status_json(&ds)).into_response()),
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
            // either language's removal takes every validation file away
            ds.dataset.validation().guard().reset()?;
            Ok(StatusCode::NO_CONTENT.into_response())
        })
        .await
    }
}

#[cfg(any(feature = "shacl", feature = "shex"))]
pub(super) use handlers::{
    delete as delete_validation, get as get_validation, put as put_validation,
};

#[cfg(not(any(feature = "shacl", feature = "shex")))]
pub(super) async fn get_validation() -> ApiResult {
    Err(err(
        StatusCode::NOT_IMPLEMENTED,
        "built without the `shacl` and `shex` features",
    ))
}
#[cfg(not(any(feature = "shacl", feature = "shex")))]
pub(super) use get_validation as put_validation;
#[cfg(not(any(feature = "shacl", feature = "shex")))]
pub(super) use get_validation as delete_validation;

#[cfg(all(test, feature = "shex"))]
#[path = "validation_shex_tests.rs"]
mod shex_tests;
