//! Applying RDF Patch over HTTP, as Fuseki's `patch` operation (`PatchApply`):
//! `POST` or `PATCH /{ds}/patch`, and a `POST /{ds}` whose content type is a patch's.
//!
//! The text form is `application/rdf-patch`, which is also assumed for a missing
//! content type and for `application/x-www-form-urlencoded` (what `curl --data` sends).
//! The binary form is `application/rdf-patch+thrift`, which Fuseki refuses. A charset
//! other than UTF-8 is `415`. The body is limited by `--max-upload-mb`, as a Graph Store
//! write is. The patch is one write transaction (`Store::apply_patch`).

use super::{
    ApiResult, BodyBudget, Params, St, body_write_options, dataset, err, receipt_wanted, spool,
    with_timeout, write_error, write_report, write_response,
};
use crate::auth::{Endpoint, Level, Principal};
use crate::obs::Op;
use axum::extract::{Extension, Path};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, Uri, header};
use axum::response::IntoResponse;
use serde_json::json;
use sparkles::Error;
use sparkles::patch::{MEDIA_TYPE, MEDIA_TYPE_BINARY, PatchErrorKind};
use sparkles::store::PatchOptions;

/// `Allow` of the patch endpoint, as Fuseki answers `OPTIONS`.
const ALLOW: &str = "OPTIONS,POST,PATCH";

/// Whether a request's content type is a patch's (for the dispatch of `POST /{ds}`).
pub(super) fn is_patch_type(media_type: &str) -> bool {
    media_type == MEDIA_TYPE || media_type == MEDIA_TYPE_BINARY
}

/// The form of a patch body: `Ok(true)` binary, `Ok(false)` text, or the `415`.
fn body_form(headers: &HeaderMap) -> ApiResult<bool> {
    let raw = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let mut parts = raw.split(';');
    let media = parts.next().unwrap_or("").trim().to_ascii_lowercase();
    for p in parts {
        if let Some((k, v)) = p.split_once('=')
            && k.trim().eq_ignore_ascii_case("charset")
        {
            let v = v.trim().trim_matches('"');
            if !v.eq_ignore_ascii_case("utf-8") && !v.eq_ignore_ascii_case("utf8") {
                return Err(err(
                    StatusCode::UNSUPPORTED_MEDIA_TYPE,
                    format!("a patch's charset must be omitted or UTF-8, not {v}"),
                ));
            }
        }
    }
    match media.as_str() {
        "" | "application/x-www-form-urlencoded" | MEDIA_TYPE => Ok(false),
        MEDIA_TYPE_BINARY => Ok(true),
        other => Err(err(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            format!(
                "a patch's content type must be {MEDIA_TYPE} or {MEDIA_TYPE_BINARY}, not {other}"
            ),
        )),
    }
}

/// `/{ds}/patch`: `POST` and `PATCH` apply a patch, and every other method is `405`
/// with an `Allow` of Fuseki's. The CORS layer answers `OPTIONS`.
pub(super) async fn patch(
    st: St,
    name: Path<String>,
    p: Extension<Principal>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: axum::body::Body,
) -> ApiResult {
    match method {
        Method::POST | Method::PATCH => apply(st, name, p, uri, headers, body).await,
        m => {
            let mut r = err(
                StatusCode::METHOD_NOT_ALLOWED,
                format!("{m}: a patch must use POST or PATCH"),
            )
            .into_response();
            r.headers_mut()
                .insert(header::ALLOW, HeaderValue::from_static(ALLOW));
            Ok(r)
        }
    }
}

/// Apply the request's patch to dataset `name`.
pub(super) async fn apply(
    st: St,
    axum::extract::Path(name): Path<String>,
    Extension(p): Extension<Principal>,
    uri: Uri,
    headers: HeaderMap,
    body: axum::body::Body,
) -> ApiResult {
    let st = st.0;
    // every route that leads here needs write access, with the endpoint name `patch`
    if let Some(denied) =
        crate::auth::dataset_denial(&st, &p, &headers, &name, Level::Write, Endpoint::Patch)
    {
        return Ok(denied);
    }
    if st.read_only {
        return Err(err(StatusCode::FORBIDDEN, "server is read-only"));
    }
    let ds = dataset(&st, &name)?;
    let params = Params::from_query(&uri);
    super::history::reject_at(&params)?;
    let binary = body_form(&headers)?;
    let view = p.view(&name, Endpoint::Patch);
    let restricted = view.is_some();
    let wanted = receipt_wanted(&params, &headers);
    let (mut wopts, _cancel_on_drop) = body_write_options(&st, &params, &headers, Some(&p))?;
    wopts.opts.graphs = view;
    let dry = wopts
        .opts
        .dry_run
        .as_ref()
        .map(|d| super::dry_run::Request::new(&st, &headers, d, restricted));
    let body = spool(body, &mut BodyBudget::new(&st.limits)).await?;
    let (wopts, timeout) = wopts.start();
    super::blocking(move || {
        let t0 = std::time::Instant::now();
        let opts = PatchOptions {
            write: wopts,
            binary,
        };
        let r = match &body {
            super::Spooled::Memory(b) => ds.store.apply_patch(&b[..], &opts),
            super::Spooled::File(f) => {
                let file = std::fs::File::open(f.path())
                    .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
                ds.store.apply_patch(file, &opts)
            }
        };
        let o = match (r, dry) {
            (Err(Error::DryRun(p)), Some(d)) => return Ok(d.respond(&ds, &p)),
            (Err(e @ Error::Patch(_)), _) => return Err(patch_error(e, &name)),
            (r, _) => r.map_err(|e| write_error(e, restricted))?,
        };
        let body = json!({
            "committed": o.receipt.committed,
            "inserted": o.inserted,
            "deleted": o.deleted,
            "prefixesSet": o.prefixes_set,
            "prefixesRemoved": o.prefixes_removed,
            "rows": o.rows,
            "aborted": o.aborted,
            "prevChecked": o.prev_checked,
            "timing": { "totalMs": t0.elapsed().as_secs_f64() * 1000.0 },
        });
        Ok(
            write_report(Op::Patch, o.inserted + o.deleted).attach(write_response(
                &ds,
                StatusCode::OK,
                Some(body),
                &o.receipt,
                wanted,
                restricted,
            )),
        )
    })
    .await
    .map_err(|e| with_timeout(e, timeout))
}

/// The answer to a patch that cannot be read or applied: `400` with where the error is,
/// or `412` for a `prev` that names a commit other than the head.
fn patch_error(e: Error, name: &str) -> super::ApiError {
    let Error::Patch(p) = e else {
        return e.into();
    };
    let code = p.kind.code();
    if let (PatchErrorKind::PrevMismatch, Some(m)) = (p.kind, &p.mismatch) {
        return super::ApiError(
            StatusCode::PRECONDITION_FAILED,
            json!({
                "error": format!(
                    "the patch expects commit {} as the head of {name}; the head is {}",
                    m.expected, m.head
                ),
                "code": code,
                "prev": m.prev,
                "head": m.head,
            }),
        );
    }
    let mut body = json!({ "error": p.to_string(), "code": code });
    for (k, v) in [
        ("row", p.row),
        ("line", p.line),
        ("column", p.column),
        ("offset", p.offset),
    ] {
        if let Some(v) = v {
            body[k] = v.into();
        }
    }
    super::ApiError(StatusCode::BAD_REQUEST, body)
}
