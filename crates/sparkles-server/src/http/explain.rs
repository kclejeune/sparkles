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
    req.timeout = Some(timeout_of(&st.limits, st.default_timeout, timeout));
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

/// The timeout of an explanation: the one asked for or [`DEFAULT_TIMEOUT`], within the
/// server's ceiling as a query's is, whose floor is the server's query timeout.
fn timeout_of(
    limits: &crate::state::Limits,
    query_timeout: std::time::Duration,
    asked: Option<std::time::Duration>,
) -> std::time::Duration {
    limits.cap_timeout(asked.unwrap_or(DEFAULT_TIMEOUT), Some(query_timeout))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn a_ceiling_below_the_default_holds() {
        // serve --timeout 1 --max-timeout 2
        let limits = crate::state::Limits {
            max_timeout: Some(Duration::from_secs(2)),
            ..Default::default()
        };
        let one = Duration::from_secs(1);
        assert_eq!(timeout_of(&limits, one, None), Duration::from_secs(2));
        assert_eq!(
            timeout_of(&limits, one, Some(Duration::from_secs(30))),
            Duration::from_secs(2)
        );
        assert_eq!(timeout_of(&limits, one, Some(one)), one);
        // without a ceiling the default stands
        let open = crate::state::Limits::default();
        assert_eq!(timeout_of(&open, one, None), DEFAULT_TIMEOUT);
    }
}
