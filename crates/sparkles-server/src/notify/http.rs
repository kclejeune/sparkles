//! The routes of spec C21 §8: `GET /$/notifications` and
//! `POST /$/notifications/test/{channel}`, both for server administrators. The
//! configuration is the server-wide settings kind at
//! `/$/server/settings/notifications`.

use crate::auth::Principal;
use crate::http::{ApiResult, blocking, err_code};
use crate::state::AppState;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Extension, Json, Router};
use serde_json::Value;
use std::sync::Arc;

type St = State<Arc<AppState>>;

pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/$/notifications", get(status))
        .route("/$/notifications/test/{channel}", post(test))
}

async fn status(State(st): St) -> ApiResult<Json<Value>> {
    blocking(move || Ok(Json(super::status_json(&st)))).await
}

async fn test(
    State(st): St,
    Path(channel): Path<String>,
    p: Option<Extension<Principal>>,
) -> ApiResult<Json<Value>> {
    let principal = crate::settings::http::who(p);
    blocking(move || {
        let out = super::test_send(&st, &channel);
        tracing::info!(
            target: "sparkles::audit",
            event = "notification_test",
            channel = channel.as_str(),
            result = if out.is_ok() { "ok" } else { "failed" },
            principal = principal.as_str()
        );
        match out {
            Ok(v) => Ok(Json(v)),
            Err(("unknown-channel", m)) => {
                Err(err_code(StatusCode::NOT_FOUND, "unknown-channel", m))
            }
            Err((code, m)) => Err(err_code(StatusCode::BAD_GATEWAY, code, m)),
        }
    })
    .await
}
