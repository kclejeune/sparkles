//! `GET /$/schema/{ds}/profiles` (per-class property profiles,
//! `sparkles::schema::profiles`) and `GET /$/schema/{ds}/diff` (what changed between the
//! reports of two states, `sparkles::schema::compare`).

use super::super::{ApiResult, Params, St, blocking, dataset};
use super::{bad, parse, schema_error};
use crate::auth::Principal;
use axum::Extension;
use axum::Json;
use axum::extract::Path;
use axum::http::{HeaderMap, Uri, header};
use axum::response::IntoResponse;
use sparkles::history::At;
use sparkles::schema::{self, ProfileOptions};
use std::time::Instant;

/// The class IRIs of `class=` parameters (repeatable; angle brackets optional).
fn classes(params: &Params) -> ApiResult<Vec<String>> {
    let mut out = Vec::new();
    for c in params.all("class") {
        let c = c.trim();
        let iri = c
            .strip_prefix('<')
            .and_then(|i| i.strip_suffix('>'))
            .unwrap_or(c);
        oxrdf::NamedNode::new(iri).map_err(|e| bad(format!("class: invalid IRI '{iri}': {e}")))?;
        out.push(iri.to_string());
    }
    Ok(out)
}

/// `GET /$/schema/{ds}/profiles`: for each class (or each `class=`), the predicates its
/// instances use and the predicates that point at them, with counts.
pub(in crate::http) async fn profiles(
    st: St,
    Path(name): Path<String>,
    Extension(p): Extension<Principal>,
    uri: Uri,
) -> ApiResult {
    let ds = dataset(&st, &name)?;
    let req = parse(&st, &ds, &uri, &p)?;
    let params = Params::from_query(&uri);
    let classes = classes(&params)?;
    blocking(move || {
        let mut opts = ProfileOptions {
            schema: req.report.options,
            classes,
        };
        opts.schema.deadline = Some(Instant::now() + req.timeout);
        let profiles = ds
            .dataset
            .schema()
            .profiles_at(&opts, req.report.at.as_ref())
            .map_err(|e| schema_error(e, req.timeout))?;
        Ok(Json(profiles).into_response())
    })
    .await
}

/// `from=` or `to=`: a commit, time or named snapshot, as `at=` takes them.
fn at(params: &Params, k: &str) -> ApiResult<Option<At>> {
    params
        .get(k)
        .map(|v| v.parse::<At>().map_err(|e| bad(format!("{k}: {e}"))))
        .transpose()
}

/// `GET /$/schema/{ds}/diff?from=…[&to=…]`: the classes and predicates added, removed
/// and changed from the report of state `from` to that of `to` (default: the head), with
/// the selection parameters of `/$/schema/{ds}`. The head's report comes from the
/// dataset's cache as a summary's would.
pub(in crate::http) async fn diff(
    st: St,
    Path(name): Path<String>,
    Extension(p): Extension<Principal>,
    uri: Uri,
    headers: HeaderMap,
) -> ApiResult {
    let ds = dataset(&st, &name)?;
    let params = Params::from_query(&uri);
    if params.get("at").is_some() {
        return Err(bad("a schema diff takes from= and to=, not at="));
    }
    if params.get("cursor").is_some() {
        return Err(bad("a schema diff is not paginated (no cursor)"));
    }
    let from = at(&params, "from")?.ok_or_else(|| bad("from= is required"))?;
    let to = at(&params, "to")?.filter(|t| *t != At::Head);
    let text = match params.get("format") {
        None => {
            super::super::negotiate(
                headers
                    .get(header::ACCEPT)
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or("*/*"),
                &["application/json", "text/plain"],
            ) == Some(1)
        }
        Some("json") => false,
        Some("text") => true,
        Some(f) => return Err(bad(format!("unknown format '{f}': json or text"))),
    };
    let mut req = parse(&st, &ds, &uri, &p)?;
    blocking(move || {
        // the older state is computed for this request and not cached
        req.report.at = to;
        let (d, how) = ds
            .dataset
            .schema()
            .diff(&from, &req.report)
            .map_err(|e| schema_error(e, req.timeout))?;
        let mut resp = if text {
            (
                [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
                schema::diff_text(&d),
            )
                .into_response()
        } else {
            Json(d).into_response()
        };
        if let Ok(v) = header::HeaderValue::from_str(&how.to_string()) {
            resp.headers_mut().insert(super::COMPUTED_HEADER, v);
        }
        Ok(resp)
    })
    .await
}
