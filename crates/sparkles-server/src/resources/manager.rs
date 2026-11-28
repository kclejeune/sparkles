//! Source manager routes. Shared operations live in recipe, load, harvest and mint.

use super::{Kind, Pin, load, mint, recipe};
use crate::auth::{Endpoint, Level, Principal, ServerPerm};
use crate::http::{ApiResult, Params, blocking, err_code};
use crate::state::{AppState, Dataset};
use axum::body::Bytes;
use axum::extract::{Path, Request, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, Uri, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Extension, Json, Router};
use serde::de::DeserializeOwned;
use serde_json::json;
use sparkles::guard::WriteOptions;
use std::sync::Arc;

type St = State<Arc<AppState>>;

pub(crate) fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/$/sources", get(sources))
        .route("/$/sources/plan", post(plan))
        .route("/$/sources/harvest", post(harvest))
        .route("/$/sources/{ds}/load", post(execute))
        .route("/$/sources/{ds}/manifests/{commit}", get(manifest))
        .route("/$/sources/{ds}/manifests/{commit}/reload", post(reload))
        .route(
            "/$/resources/{id}/versions/{version}/recipe",
            get(get_recipe),
        )
        .route(
            "/$/resources/{id}/versions/{version}/lock",
            get(lock).post(check_lock),
        )
        .route(
            "/$/resources/{id}/versions/{version}/bundle/{ds}",
            post(bundle),
        )
        .route("/$/resources/{id}/mint/{ds}", post(mint_iri))
}

pub(crate) async fn no_store(req: Request, next: Next) -> Response {
    let path = req.uri().path();
    let private = path == "/$/mcp"
        || ["/$/sources", "/$/resources"].into_iter().any(|root| {
            path.strip_prefix(root)
                .is_some_and(|tail| tail.is_empty() || tail.starts_with('/'))
        });
    let mut response = next.run(req).await;
    if private {
        response
            .headers_mut()
            .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    }
    response
}

fn parse<T: DeserializeOwned>(body: &[u8]) -> ApiResult<T> {
    serde_json::from_slice(body).map_err(|e| {
        err_code(
            StatusCode::BAD_REQUEST,
            "bad-resource-document",
            e.to_string(),
        )
    })
}

pub(crate) fn require_admin(p: &Principal) -> ApiResult<()> {
    if p.has(ServerPerm::ServerAdmin) {
        Ok(())
    } else {
        Err(err_code(
            StatusCode::FORBIDDEN,
            "forbidden",
            "source catalog access needs server admin",
        ))
    }
}

pub(crate) fn require_manifest_reader(p: &Principal, ds: &Dataset) -> ApiResult<()> {
    require_admin(p)?;
    if ds.branch.is_some() {
        return Err(super::invalid("source manifests belong to the main branch").into());
    }
    if !p.can_at(&ds.name, Endpoint::Query, Level::Read)
        || p.view(&ds.name, Endpoint::Query)
            .is_some_and(|v| !v.reads_everything())
    {
        return Err(err_code(
            StatusCode::FORBIDDEN,
            "forbidden",
            "manifest reads need Query access to the whole dataset",
        ));
    }
    Ok(())
}

pub(crate) fn source_list(st: &AppState) -> ApiResult<serde_json::Value> {
    let mut catalog = st.resources.read()?;
    catalog.resources.retain(|r| r.input.kind == Kind::Source);
    Ok(json!(catalog))
}

async fn sources(State(st): St) -> ApiResult {
    blocking(move || Ok(Json(source_list(&st)?).into_response())).await
}

async fn plan(State(st): St, body: Bytes) -> ApiResult {
    let recipe = recipe::decode(&body)?;
    blocking(move || Ok(Json(recipe::plan(&st.resources.read()?, &recipe)?).into_response())).await
}

async fn harvest(body: Bytes) -> ApiResult {
    let request = parse(&body)?;
    blocking(move || Ok(Json(super::harvest::preview(request)?).into_response())).await
}

async fn get_recipe(State(st): St, Path((resource, version)): Path<(String, String)>) -> ApiResult {
    blocking(move || {
        let pin = Pin { resource, version };
        Ok(Json(recipe::from_version(&st.resources.read()?, &pin)?).into_response())
    })
    .await
}

async fn lock(State(st): St, Path((resource, version)): Path<(String, String)>) -> ApiResult {
    blocking(move || {
        let pin = Pin { resource, version };
        Ok(Json(recipe::lock(&st.resources.read()?, &pin)?).into_response())
    })
    .await
}

async fn check_lock(
    State(st): St,
    Path((resource, version)): Path<(String, String)>,
    body: Bytes,
) -> ApiResult {
    let given: recipe::ImportLock = recipe::decode(&body)?;
    blocking(move || {
        let pin = Pin { resource, version };
        let expected = recipe::lock(&st.resources.read()?, &pin)?;
        if given != expected {
            return Err(err_code(
                StatusCode::CONFLICT,
                "import-lock-mismatch",
                "the import lock differs from the catalog's pinned versions",
            ));
        }
        Ok(Json(expected).into_response())
    })
    .await
}

type Cancel = crate::obs::CancelOnDrop;

fn options(
    st: &AppState,
    ds: &Dataset,
    p: &Principal,
    headers: &HeaderMap,
    uri: &Uri,
) -> ApiResult<(WriteOptions, Cancel)> {
    super::http::writable(st)?;
    let params = Params::from_query(uri);
    let (options, cancel) = crate::http::body_write_options(st, &params, headers, Some(p))?;
    let (mut options, _) = options.start();
    options.precondition = crate::http::dataset_write_precondition(headers, ds.store.dataset_id());
    Ok((options, cancel))
}

async fn execute(
    State(st): St,
    Path(name): Path<String>,
    Extension(p): Extension<Principal>,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> ApiResult {
    require_admin(&p)?;
    let ds = crate::http::dataset(&st, &name)?;
    let (opts, _cancel) = options(&st, &ds, &p, &headers, &uri)?;
    let recipe = recipe::decode(&body)?;
    blocking(move || Ok(Json(load::execute(&st, &ds, &p, recipe, None, opts)?).into_response()))
        .await
}

async fn manifest(
    State(st): St,
    Path((name, seq)): Path<(String, u64)>,
    Extension(p): Extension<Principal>,
) -> ApiResult {
    let ds = crate::http::dataset(&st, &name)?;
    require_manifest_reader(&p, &ds)?;
    blocking(move || {
        let manifest = load::read(&st, &ds, seq)?;
        Ok(Json(manifest).into_response())
    })
    .await
}

async fn reload(
    State(st): St,
    Path((name, seq)): Path<(String, u64)>,
    Extension(p): Extension<Principal>,
    uri: Uri,
    headers: HeaderMap,
) -> ApiResult {
    let ds = crate::http::dataset(&st, &name)?;
    require_manifest_reader(&p, &ds)?;
    let (opts, _cancel) = options(&st, &ds, &p, &headers, &uri)?;
    blocking(move || {
        let old = load::read(&st, &ds, seq)?;
        let result = load::execute(&st, &ds, &p, old.recipe, Some((seq, old.plan)), opts)?;
        Ok(Json(result).into_response())
    })
    .await
}

async fn bundle(
    State(st): St,
    Path((resource, version, name)): Path<(String, String, String)>,
    Extension(p): Extension<Principal>,
    uri: Uri,
    headers: HeaderMap,
) -> ApiResult {
    require_admin(&p)?;
    let ds = crate::http::dataset(&st, &name)?;
    let (opts, _cancel) = options(&st, &ds, &p, &headers, &uri)?;
    blocking(move || {
        let pin = Pin { resource, version };
        let recipe = recipe::bundle(&st.resources.read()?, &pin)?;
        Ok(Json(load::execute(&st, &ds, &p, recipe, None, opts)?).into_response())
    })
    .await
}

async fn mint_iri(
    State(st): St,
    Path((id, name)): Path<(String, String)>,
    Extension(p): Extension<Principal>,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> ApiResult {
    let ds = crate::http::dataset(&st, &name)?;
    let (opts, _cancel) = options(&st, &ds, &p, &headers, &uri)?;
    let request = parse(&body)?;
    blocking(move || Ok(Json(mint::reserve(&st, &ds, &p, &id, request, opts)?).into_response()))
        .await
}
