//! Evidence-profile settings in the server (spec C25 §7): `GET` and `PUT
//! /$/evidence/{ds}/settings` read and record a dataset's evidence settings, and
//! `POST /$/evidence/{ds}/detach` stops protecting one graph of their roles.
//!
//! Authorization (the route table in `auth/routes.rs`): all three need `admin` on
//! `{ds}`, because the settings name the dataset's protected graphs and policies.

use crate::http::{AdminBody, ApiResult, blocking, dataset, err};
use crate::state::AppState;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{Value as J, json};
use sparkles::evidence::SettingsStatus;
use sparkles::task::Control;
use std::sync::Arc;

type St = State<Arc<AppState>>;

pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route(
            "/$/evidence/{ds}/settings",
            get(get_settings).put(put_settings),
        )
        .route("/$/evidence/{ds}/detach", post(detach))
}

/// The response body: the canonical settings, their digest, the registry revision and
/// the role graphs that are protected or detached.
fn body(status: SettingsStatus) -> ApiResult<Json<J>> {
    let settings = status.settings.canonical(&Control::none())?;
    Ok(Json(json!({
        "settings": settings.value(),
        "digest": status.digest,
        "revision": status.revision,
        "managed": status.managed,
        "detached": status.detached,
    })))
}

/// `GET /$/evidence/{ds}/settings`: the recorded settings, or the disabled defaults.
async fn get_settings(State(st): St, Path(name): Path<String>) -> ApiResult<Json<J>> {
    let ds = dataset(&st, &name)?;
    blocking(move || body(sparkles::evidence::settings(&ds.dataset)?)).await
}

/// `PUT /$/evidence/{ds}/settings`: record complete settings. A configuration that
/// would stop protecting a graph, or that races with another registry change, is a
/// `409` that writes nothing.
async fn put_settings(
    State(st): St,
    Path(name): Path<String>,
    AdminBody(bytes): AdminBody,
) -> ApiResult<Json<J>> {
    if st.read_only {
        return Err(err(StatusCode::FORBIDDEN, "server is read-only"));
    }
    let ds = dataset(&st, &name)?;
    blocking(move || {
        let control = Control::none();
        let settings = sparkles::evidence::parse_settings(&bytes, &control)?;
        body(sparkles::evidence::configure(
            &ds.dataset,
            &settings,
            &control,
        )?)
    })
    .await
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Detach {
    graph: String,
}

/// `POST /$/evidence/{ds}/detach` with `{"graph": IRI}`: ordinary writes can change the
/// graph until the settings are recorded again.
async fn detach(
    State(st): St,
    Path(name): Path<String>,
    AdminBody(bytes): AdminBody,
) -> ApiResult<Json<J>> {
    if st.read_only {
        return Err(err(StatusCode::FORBIDDEN, "server is read-only"));
    }
    let request: Detach = serde_json::from_slice(&bytes).map_err(|e| {
        err(
            StatusCode::BAD_REQUEST,
            format!("invalid detach request: {e}"),
        )
    })?;
    let ds = dataset(&st, &name)?;
    blocking(move || body(sparkles::evidence::detach(&ds.dataset, &request.graph)?)).await
}
