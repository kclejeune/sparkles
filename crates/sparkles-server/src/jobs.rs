//! Durable jobs over HTTP (spec C26 §6): `GET` and `PUT /$/jobs/{ds}/settings` read
//! and record a dataset's jobs settings, `POST /$/jobs/{ds}` submits a run with an
//! idempotency key, `GET /$/jobs/{ds}` and `GET /$/jobs/{ds}/{id}` inspect visible runs,
//! `POST /$/jobs/{ds}/claim` leases one eligible run to the calling worker, and the
//! `renew`, `publish`, `fail`, `cancel` and `retry` routes under `/$/jobs/{ds}/{id}` move
//! a run through its states. `POST /$/jobs/{ds}/recover` resumes selected work of a
//! restored dataset that its jobs recovery paused (spec C26 §7.3).
//!
//! Authorization (the route table in `auth/routes.rs`): the settings and the recovery
//! route need `admin` on `{ds}`, inspection needs `read`, and submission, claims and lease operations need
//! `write` through the update endpoint. The lease operations also need the bearer lease
//! token in `Sparkles-Job-Lease`, which the claim returned once. Neither that header nor
//! `Authorization` is written to a log, and no response echoes a token.
//!
//! A worker's publication writes its quads under the caller's update view, into the
//! definition's graphs only. Request bodies are JSON with an optional `format` member,
//! which is 1.

use crate::auth::{Endpoint, Level, Principal};
use crate::http::{AdminBody, ApiResult, blocking, err, err_code, main_dataset};
use crate::state::AppState;
use axum::Extension;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{Value as J, json};
use sparkles::jobs::{
    Caller, Failure, ListFilter, Publication, Settings, State as RunState, Submission,
};
use std::sync::Arc;
use uuid::Uuid;

type St = State<Arc<AppState>>;

/// The header that carries the bearer lease token.
pub const LEASE_HEADER: &str = "sparkles-job-lease";

/// The largest request body of a job operation.
const MAX_BODY_BYTES: usize = 16 << 20;

pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/$/jobs/{ds}/settings", get(get_settings).put(put_settings))
        .route("/$/jobs/{ds}", get(list).post(submit))
        .route("/$/jobs/{ds}/claim", post(claim))
        .route("/$/jobs/{ds}/recover", post(recover))
        .route("/$/jobs/{ds}/{id}", get(get_run))
        .route("/$/jobs/{ds}/{id}/renew", post(renew))
        .route("/$/jobs/{ds}/{id}/publish", post(publish))
        .route("/$/jobs/{ds}/{id}/fail", post(fail))
        .route("/$/jobs/{ds}/{id}/cancel", post(cancel))
        .route("/$/jobs/{ds}/{id}/retry", post(retry))
}

fn caller(st: &AppState, name: &str, p: &Principal) -> Caller {
    let _ = st;
    Caller {
        principal: crate::evidence::principal_iri(p),
        admin: p.can(name, Level::Admin),
        write: p.view(name, Endpoint::Update),
    }
}

fn parse<T: for<'de> Deserialize<'de>>(bytes: &[u8], what: &str) -> ApiResult<T> {
    if bytes.len() > MAX_BODY_BYTES {
        return Err(err(
            StatusCode::PAYLOAD_TOO_LARGE,
            format!("a job request holds at most {MAX_BODY_BYTES} bytes"),
        ));
    }
    let value: J = serde_json::from_slice(bytes).map_err(|e| {
        err_code(
            StatusCode::BAD_REQUEST,
            "invalid-request",
            format!("invalid {what}: {e}"),
        )
    })?;
    if let Some(f) = value.get("format")
        && !f.is_null()
        && f.as_u64() != Some(1)
    {
        return Err(err_code(
            StatusCode::BAD_REQUEST,
            "unsupported-format",
            format!("a job request has format 1, not {f}"),
        ));
    }
    serde_json::from_value(value).map_err(|e| {
        err_code(
            StatusCode::BAD_REQUEST,
            "invalid-request",
            format!("invalid {what}: {e}"),
        )
    })
}

fn run_id(id: &str) -> ApiResult<Uuid> {
    Uuid::parse_str(id).map_err(|_| err(StatusCode::NOT_FOUND, "no such run"))
}

fn lease(headers: &HeaderMap) -> ApiResult<String> {
    let value = headers.get(LEASE_HEADER).ok_or_else(|| {
        err_code(
            StatusCode::UNAUTHORIZED,
            "lease-missing",
            "the Sparkles-Job-Lease header carries the lease token",
        )
    })?;
    let token = value
        .to_str()
        .map_err(|_| err(StatusCode::BAD_REQUEST, "the lease token is not ASCII"))?;
    if token.is_empty() || token.len() > 256 {
        return Err(err(
            StatusCode::BAD_REQUEST,
            "the lease token is empty or too long",
        ));
    }
    Ok(token.to_string())
}

fn writable(st: &AppState) -> ApiResult<()> {
    if st.read_only {
        return Err(err(StatusCode::FORBIDDEN, "server is read-only"));
    }
    Ok(())
}

fn settings_json(status: sparkles::jobs::SettingsStatus) -> J {
    json!({
        "settings": status.settings,
        "digest": status.digest,
        "revision": status.revision,
        "control": status.control,
        "revisions": status.revisions,
    })
}

async fn get_settings(State(st): St, Path(name): Path<String>) -> ApiResult<Json<J>> {
    let ds = main_dataset(&st, &name)?;
    let status = blocking(move || Ok(ds.dataset.jobs().settings()?)).await?;
    Ok(Json(settings_json(status)))
}

async fn put_settings(
    State(st): St,
    Path(name): Path<String>,
    AdminBody(bytes): AdminBody,
) -> ApiResult<Json<J>> {
    writable(&st)?;
    let settings: Settings = parse(&bytes, "jobs settings")?;
    let ds = main_dataset(&st, &name)?;
    let status = blocking(move || Ok(ds.dataset.jobs().configure(&settings)?)).await?;
    Ok(Json(settings_json(status)))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SubmitRequest {
    #[serde(default, rename = "format")]
    _format: Option<u32>,
    definition: String,
    #[serde(default)]
    payload: J,
    idempotency_key: String,
    #[serde(default)]
    checkpoint: Option<String>,
    #[serde(default)]
    inputs: Vec<InputRef>,
    #[serde(default)]
    message: Option<String>,
}

/// A commit a run reads, by its branch id and sequence number.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct InputRef {
    branch_id: Uuid,
    seq: u64,
}

async fn submit(
    State(st): St,
    Path(name): Path<String>,
    Extension(p): Extension<Principal>,
    AdminBody(bytes): AdminBody,
) -> ApiResult {
    writable(&st)?;
    let r: SubmitRequest = parse(&bytes, "submission")?;
    let ds = main_dataset(&st, &name)?;
    let c = caller(&st, &name, &p);
    let submission = Submission {
        definition: r.definition,
        payload: r.payload,
        idempotency_key: r.idempotency_key,
        checkpoint: r.checkpoint,
        inputs: r
            .inputs
            .into_iter()
            .map(|i| sparkles::branch::CommitRef {
                branch_id: i.branch_id,
                seq: i.seq,
            })
            .collect(),
        message: r.message,
        retry_of: None,
    };
    let s = blocking(move || Ok(ds.dataset.jobs().submit(&submission, &c)?)).await?;
    let location = format!("/$/jobs/{name}/{}", s.run);
    let status = if s.replayed {
        StatusCode::OK
    } else {
        StatusCode::ACCEPTED
    };
    Ok((status, [(header::LOCATION, location)], Json(json!(s))).into_response())
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct ListQuery {
    state: Option<RunState>,
    definition: Option<String>,
    limit: Option<usize>,
}

async fn list(
    State(st): St,
    Path(name): Path<String>,
    Query(q): Query<ListQuery>,
    Extension(p): Extension<Principal>,
) -> ApiResult<Json<J>> {
    let ds = main_dataset(&st, &name)?;
    let c = caller(&st, &name, &p);
    let filter = ListFilter {
        state: q.state,
        definition: q.definition,
        limit: q.limit,
    };
    let runs = blocking(move || Ok(ds.dataset.jobs().list(&filter, &c)?)).await?;
    Ok(Json(json!({ "runs": runs })))
}

async fn get_run(
    State(st): St,
    Path((name, id)): Path<(String, String)>,
    Extension(p): Extension<Principal>,
) -> ApiResult<Json<J>> {
    let id = run_id(&id)?;
    let ds = main_dataset(&st, &name)?;
    let c = caller(&st, &name, &p);
    let run = blocking(move || Ok(ds.dataset.jobs().get(id, &c)?)).await?;
    match run {
        Some(run) => Ok(Json(json!(run))),
        None => Err(err(StatusCode::NOT_FOUND, "no such run")),
    }
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ClaimRequest {
    #[serde(default, rename = "format")]
    _format: Option<u32>,
    #[serde(default)]
    definitions: Vec<String>,
}

async fn claim(
    State(st): St,
    Path(name): Path<String>,
    Extension(p): Extension<Principal>,
    AdminBody(bytes): AdminBody,
) -> ApiResult {
    writable(&st)?;
    let r: ClaimRequest = if bytes.is_empty() {
        ClaimRequest::default()
    } else {
        parse(&bytes, "claim")?
    };
    let ds = main_dataset(&st, &name)?;
    let c = caller(&st, &name, &p);
    let claimed = blocking(move || Ok(ds.dataset.jobs().claim(&r.definitions, &c)?)).await?;
    match claimed {
        Some(claimed) => Ok(Json(json!(claimed)).into_response()),
        None => Ok(StatusCode::NO_CONTENT.into_response()),
    }
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RenewRequest {
    #[serde(default, rename = "format")]
    _format: Option<u32>,
    #[serde(default)]
    extend_ms: Option<u64>,
    #[serde(default)]
    expected_version: Option<u64>,
}

async fn renew(
    State(st): St,
    Path((name, id)): Path<(String, String)>,
    Extension(p): Extension<Principal>,
    headers: HeaderMap,
    AdminBody(bytes): AdminBody,
) -> ApiResult<Json<J>> {
    writable(&st)?;
    let id = run_id(&id)?;
    let token = lease(&headers)?;
    let r: RenewRequest = if bytes.is_empty() {
        RenewRequest::default()
    } else {
        parse(&bytes, "renewal")?
    };
    let ds = main_dataset(&st, &name)?;
    let c = caller(&st, &name, &p);
    let renewed = blocking(move || {
        Ok(ds
            .dataset
            .jobs()
            .renew(id, &token, r.extend_ms, r.expected_version, &c)?)
    })
    .await?;
    Ok(Json(json!(renewed)))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PublishRequest {
    #[serde(default, rename = "format")]
    _format: Option<u32>,
    operation: Uuid,
    #[serde(default)]
    expected_version: Option<u64>,
    /// N-Quads lines, one quad each, in the definition's graphs.
    #[serde(default)]
    insert: Vec<String>,
    #[serde(default)]
    delete: Vec<String>,
    #[serde(default)]
    checkpoint: Option<String>,
    #[serde(default)]
    message: Option<String>,
}

fn parse_quads(field: &str, lines: &[String]) -> ApiResult<Vec<oxrdf::Quad>> {
    let mut out = Vec::with_capacity(lines.len());
    for (i, line) in lines.iter().enumerate() {
        let text = format!("{line}\n");
        let mut quads =
            oxrdfio::RdfParser::from_format(oxrdfio::RdfFormat::NQuads).for_slice(text.as_bytes());
        let quad = match quads.next() {
            Some(Ok(q)) => q,
            Some(Err(e)) => {
                return Err(err_code(
                    StatusCode::BAD_REQUEST,
                    "invalid-request",
                    format!("{field}[{i}]: {e}"),
                ));
            }
            None => {
                return Err(err_code(
                    StatusCode::BAD_REQUEST,
                    "invalid-request",
                    format!("{field}[{i}]: expected one quad"),
                ));
            }
        };
        if quads.next().is_some() {
            return Err(err_code(
                StatusCode::BAD_REQUEST,
                "invalid-request",
                format!("{field}[{i}]: expected one quad"),
            ));
        }
        out.push(quad);
    }
    Ok(out)
}

async fn publish(
    State(st): St,
    Path((name, id)): Path<(String, String)>,
    Extension(p): Extension<Principal>,
    headers: HeaderMap,
    AdminBody(bytes): AdminBody,
) -> ApiResult<Json<J>> {
    writable(&st)?;
    let id = run_id(&id)?;
    let token = lease(&headers)?;
    let r: PublishRequest = parse(&bytes, "publication")?;
    let publication = Publication {
        operation: r.operation,
        expected_version: r.expected_version,
        insert: parse_quads("insert", &r.insert)?,
        delete: parse_quads("delete", &r.delete)?,
        checkpoint: r.checkpoint,
        message: r.message,
    };
    let ds = main_dataset(&st, &name)?;
    let c = caller(&st, &name, &p);
    let published =
        blocking(move || Ok(ds.dataset.jobs().publish(id, &token, &publication, &c)?)).await?;
    Ok(Json(json!(published)))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct FailRequest {
    #[serde(default, rename = "format")]
    _format: Option<u32>,
    #[serde(default)]
    transient: bool,
    #[serde(default)]
    reason: String,
    #[serde(default)]
    retry_after_ms: Option<u64>,
    #[serde(default)]
    expected_version: Option<u64>,
}

async fn fail(
    State(st): St,
    Path((name, id)): Path<(String, String)>,
    Extension(p): Extension<Principal>,
    headers: HeaderMap,
    AdminBody(bytes): AdminBody,
) -> ApiResult<Json<J>> {
    writable(&st)?;
    let id = run_id(&id)?;
    let token = lease(&headers)?;
    let r: FailRequest = parse(&bytes, "failure")?;
    let failure = Failure {
        transient: r.transient,
        reason: r.reason,
        retry_after_ms: r.retry_after_ms,
    };
    let ds = main_dataset(&st, &name)?;
    let c = caller(&st, &name, &p);
    let view = blocking(move || {
        Ok(ds
            .dataset
            .jobs()
            .fail(id, &token, &failure, r.expected_version, &c)?)
    })
    .await?;
    Ok(Json(json!(view)))
}

async fn cancel(
    State(st): St,
    Path((name, id)): Path<(String, String)>,
    Extension(p): Extension<Principal>,
) -> ApiResult<Json<J>> {
    writable(&st)?;
    let id = run_id(&id)?;
    let ds = main_dataset(&st, &name)?;
    let c = caller(&st, &name, &p);
    let view = blocking(move || Ok(ds.dataset.jobs().cancel(id, &c)?)).await?;
    Ok(Json(json!(view)))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RetryRequest {
    #[serde(default, rename = "format")]
    _format: Option<u32>,
    idempotency_key: String,
}

async fn retry(
    State(st): St,
    Path((name, id)): Path<(String, String)>,
    Extension(p): Extension<Principal>,
    AdminBody(bytes): AdminBody,
) -> ApiResult {
    writable(&st)?;
    let id = run_id(&id)?;
    let r: RetryRequest = parse(&bytes, "retry")?;
    let ds = main_dataset(&st, &name)?;
    let c = caller(&st, &name, &p);
    let s = blocking(move || Ok(ds.dataset.jobs().retry(id, &r.idempotency_key, &c)?)).await?;
    let location = format!("/$/jobs/{name}/{}", s.run);
    Ok((
        StatusCode::ACCEPTED,
        [(header::LOCATION, location)],
        Json(json!(s)),
    )
        .into_response())
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RecoverRequest {
    #[serde(default, rename = "format")]
    _format: Option<u32>,
    /// the paused runs of the restored set to resume
    #[serde(default)]
    resume: Vec<Uuid>,
}

async fn recover(
    State(st): St,
    Path(name): Path<String>,
    Extension(p): Extension<Principal>,
    AdminBody(bytes): AdminBody,
) -> ApiResult<Json<J>> {
    writable(&st)?;
    let r: RecoverRequest = parse(&bytes, "recovery")?;
    let ds = main_dataset(&st, &name)?;
    let c = caller(&st, &name, &p);
    let status = blocking(move || Ok(ds.dataset.jobs().resume(&r.resume, &c)?)).await?;
    Ok(Json(json!(status)))
}

#[cfg(test)]
mod tests;
