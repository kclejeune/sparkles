//! `POST /$/reason/{ds}/explain`: why a triple is inferred (spec U10 §3.3, the `why`
//! mode of F18 §2.1).

use super::{ApiResult, QueryBody, St, blocking, dataset, err, err_code};
use crate::auth::{Endpoint, Principal};
use axum::Extension;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use sparkles::reasoning::explain::{
    self, DEFAULT_TIMEOUT, ExplainMode, NO_VISIBLE_DERIVATION, REASONING_BUDGET,
};

/// `POST /$/reason/{ds}/explain`
pub(super) async fn explain(
    State(st): St,
    Path(name): Path<String>,
    Extension(p): Extension<Principal>,
    QueryBody(b): QueryBody,
) -> ApiResult<Response> {
    let ds = dataset(&st, &name)?;
    let j: serde_json::Value = serde_json::from_slice(&b)
        .map_err(|e| err(StatusCode::BAD_REQUEST, format!("invalid JSON: {e}")))?;
    let (mut req, mode, timeout) =
        explain::request_from_json(&j).map_err(|e| err(StatusCode::BAD_REQUEST, e.to_string()))?;
    if mode == ExplainMode::WhyNot {
        return Err(err_code(
            StatusCode::NOT_IMPLEMENTED,
            "not-implemented",
            "whyNot explanations are not available: only mode \"why\" is",
        ));
    }
    let asked = timeout.unwrap_or(DEFAULT_TIMEOUT);
    req.timeout = Some(st.limits.cap_timeout(asked, Some(DEFAULT_TIMEOUT)));
    req.access = p.view(&ds.name, Endpoint::Query);
    // the search and the answer it builds are held to a query's memory budget
    req.max_memory = st
        .limits
        .query_memory_bytes
        .map(|m| usize::try_from(m).unwrap_or(usize::MAX));
    let max_bytes = st.limits.max_result_bytes;
    let (cancel, _guard) = super::cancel_on_drop();
    req.cancel = Some(cancel);
    blocking(move || {
        let e = match ds.dataset.reasoning().explain(&req) {
            Ok(e) => e,
            Err(sparkles::Error::Component(c)) if c.code == REASONING_BUDGET => {
                return Err(err_code(
                    StatusCode::UNPROCESSABLE_ENTITY,
                    REASONING_BUDGET,
                    c.message,
                ));
            }
            Err(e) => return Err(e.into()),
        };
        if matches!(e.explained, sparkles::explain::Explained::NotFound) {
            return Err(err_code(
                StatusCode::NOT_FOUND,
                NO_VISIBLE_DERIVATION,
                "no derivation from facts you can read was found",
            ));
        }
        // the answer is held to the result-byte limit of a query's
        let body = serde_json::to_vec(&explain::explanation_json(&e))
            .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
        if let Some(limit) = max_bytes
            && body.len() as u64 > limit
        {
            return Err(sparkles::Error::BudgetExceeded(sparkles::Budget {
                kind: sparkles::BudgetKind::ResultBytes,
                limit,
                requested: body.len() as u64,
            })
            .into());
        }
        Ok((
            [(axum::http::header::CONTENT_TYPE, "application/json")],
            body,
        )
            .into_response())
    })
    .await
}
