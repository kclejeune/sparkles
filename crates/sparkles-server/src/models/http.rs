//! `GET /$/models` and `POST /$/models/{name}/test` (spec C18 §3.4), for server admins
//! (the route table in `auth/routes.rs`). Neither returns a key, and no route creates a
//! provider or changes its endpoint: providers come from `--model-config` only.

use crate::http::{AdminBody, ApiResult, blocking, err, err_code};
use crate::state::AppState;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::Arc;
use std::time::Duration;

type St = State<Arc<AppState>>;

pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/$/models", get(list))
        .route("/$/models/{name}/test", post(test))
}

/// The providers and role lists; an empty listing without `--model-config`.
async fn list(State(st): St) -> Json<Value> {
    Json(match &st.models {
        Some(m) => {
            let mut v = m.describe();
            v["configured"] = true.into();
            v
        }
        None => json!({ "configured": false, "providers": [], "roles": {} }),
    })
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct TestBody {
    model: Option<String>,
    timeout_seconds: Option<f64>,
}

/// A short prompt to one pair: its latency, its structured-output level and its tokens.
async fn test(
    State(st): St,
    Path(name): Path<String>,
    AdminBody(body): AdminBody,
) -> ApiResult<Json<Value>> {
    let Some(models) = st.models.clone() else {
        return Err(err_code(
            StatusCode::NOT_FOUND,
            "no-models",
            "this server has no model providers (serve --model-config)",
        ));
    };
    let b: TestBody = if body.iter().all(u8::is_ascii_whitespace) {
        TestBody::default()
    } else {
        serde_json::from_slice(&body).map_err(|e| err(StatusCode::BAD_REQUEST, e.to_string()))?
    };
    let secs = b.timeout_seconds.unwrap_or(60.0);
    if !(secs.is_finite() && secs > 0.0 && secs <= 600.0) {
        return Err(err(
            StatusCode::BAD_REQUEST,
            "timeoutSeconds must be > 0 and ≤ 600",
        ));
    }
    if models.provider(&name).is_none() {
        return Err(err_code(
            StatusCode::NOT_FOUND,
            "unknown-provider",
            format!("no provider named {name:?}"),
        ));
    }
    blocking(move || {
        models
            .test(&name, b.model.as_deref(), Duration::from_secs_f64(secs))
            .map(Json)
            .map_err(|m| err(StatusCode::BAD_REQUEST, m))
    })
    .await
}
