//! Read-only entailment diffs and minimal inconsistency supports.
use super::*;

#[cfg(feature = "reasoning")]
pub(super) fn options(
    st: &AppState,
    principal: &Principal,
    dataset: &str,
    options: &mut sparkles::reasoning::analysis::AnalysisOptions,
) {
    options.access = principal.view(dataset, crate::auth::Endpoint::Query);
    options.timeout = Some(
        st.limits.cap_timeout(
            options
                .timeout
                .unwrap_or(std::time::Duration::from_secs(30)),
            Some(st.default_timeout),
        ),
    );
    options.max_memory = match (
        options.max_memory,
        st.limits
            .query_memory_bytes
            .map(|limit| usize::try_from(limit).unwrap_or(usize::MAX)),
    ) {
        (Some(asked), Some(limit)) => Some(asked.min(limit)),
        (asked, limit) => asked.or(limit),
    };
    // The configured reasoning pool supplies the server's concurrency ceiling.
    options.threads = None;
    options.max_inferred = Some(
        options
            .max_inferred
            .unwrap_or(st.reason_max_inferred)
            .min(st.reason_max_inferred),
    );
}

#[cfg(feature = "reasoning")]
fn selector_access(
    principal: &Principal,
    dataset: &Dataset,
    selector: &str,
) -> ApiResult<Option<Arc<sparkles::access::GraphAccess>>> {
    let branch = if selector.parse::<sparkles::history::At>().is_ok() {
        dataset.store.branch_name()
    } else {
        selector
            .rsplit_once('@')
            .map_or(selector, |(branch, _)| branch)
            .to_string()
    };
    branches::check(
        principal,
        &dataset.name,
        &branch,
        crate::auth::Level::Read,
        Some(crate::auth::Endpoint::Query),
    )?;
    Ok(principal.view(
        &crate::auth::on_branch(&dataset.name, &branch),
        crate::auth::Endpoint::Query,
    ))
}

#[cfg(feature = "reasoning")]
pub(super) fn output(body: Vec<u8>, media: &str, limit: Option<u64>) -> ApiResult {
    if let Some(limit) = limit
        && body.len() as u64 > limit
    {
        return Err(sparkles::Error::BudgetExceeded(sparkles::Budget {
            kind: sparkles::BudgetKind::ResultBytes,
            limit,
            requested: body.len() as u64,
        })
        .into());
    }
    Ok(([(header::CONTENT_TYPE, media.to_string())], body).into_response())
}

pub(super) async fn diff(
    State(st): St,
    Path(name): Path<String>,
    Extension(principal): Extension<Principal>,
    uri: Uri,
    headers: HeaderMap,
) -> ApiResult {
    #[cfg(not(feature = "reasoning"))]
    {
        let _ = (st, name, principal, uri, headers);
        Err(err(
            StatusCode::NOT_IMPLEMENTED,
            "built without the reasoning feature",
        ))
    }
    #[cfg(feature = "reasoning")]
    {
        use sparkles::reasoning::diff::DiffRequest;
        let dataset = dataset(&st, &name)?;
        let params = Params::from_query(&uri);
        let mut value = serde_json::Map::new();
        for key in ["from", "to", "profile", "rules"] {
            if let Some(text) = params.get(key) {
                value.insert(key.to_string(), text.into());
            }
        }
        for key in [
            "inputs",
            "vocabularies",
            "geoDefaultGeometry",
            "timeout",
            "maxMemory",
            "maxInferred",
            "threads",
            "sampleLimit",
        ] {
            if let Some(text) = params.get(key) {
                value.insert(
                    key.to_string(),
                    serde_json::from_str(text).map_err(|error| {
                        err(StatusCode::BAD_REQUEST, format!("invalid {key}: {error}"))
                    })?,
                );
            }
        }
        let mut request = DiffRequest::from_json(&J::Object(value))
            .map_err(|error| err(StatusCode::BAD_REQUEST, error.to_string()))?;
        options(&st, &principal, &dataset.name, &mut request.analysis);
        request.analysis.access = None;
        request.from_access = selector_access(&principal, &dataset, &request.from)?;
        request.to_access = selector_access(&principal, &dataset, &request.to)?;
        let offers = [
            "application/json",
            "application/trig",
            sparkles::patch::MEDIA_TYPE,
            "text/rdf-patch",
        ];
        let format = headers
            .get(header::ACCEPT)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| negotiate(value, &offers))
            .unwrap_or(0);
        request.all_changes = format != 0;
        request.sample_limit = request.sample_limit.min(st.limits.max_rows);
        if request.all_changes {
            request.sample_limit = st.limits.max_rows;
        }
        let (cancel, _guard) = cancel_on_drop();
        request.analysis.cancel = Some(cancel);
        let limit = st.limits.max_result_bytes;
        blocking(move || {
            let result = dataset.dataset.reasoning().diff(&request)?;
            let text = match format {
                1 => result.to_trig()?,
                2 | 3 => result.to_patch()?,
                _ => serde_json::to_string(&result.to_json())
                    .map_err(|error| err(StatusCode::INTERNAL_SERVER_ERROR, error.to_string()))?,
            };
            output(text.into_bytes(), offers[format], limit)
        })
        .await
    }
}

pub(super) async fn justify(
    State(st): St,
    Path(name): Path<String>,
    Extension(principal): Extension<Principal>,
    QueryBody(body): QueryBody,
) -> ApiResult {
    #[cfg(not(feature = "reasoning"))]
    {
        let _ = (st, name, principal, body);
        Err(err(
            StatusCode::NOT_IMPLEMENTED,
            "built without the reasoning feature",
        ))
    }
    #[cfg(feature = "reasoning")]
    {
        let dataset = dataset(&st, &name)?;
        let value: J = serde_json::from_slice(&body)
            .map_err(|error| err(StatusCode::BAD_REQUEST, error.to_string()))?;
        let mut request = sparkles::reasoning::justify::JustifyRequest::from_json(&value)
            .map_err(|error| err(StatusCode::BAD_REQUEST, error.to_string()))?;
        options(&st, &principal, &dataset.name, &mut request.analysis);
        request.analysis.access = selector_access(&principal, &dataset, &request.at)?;
        let (cancel, _guard) = cancel_on_drop();
        request.analysis.cancel = Some(cancel);
        let limit = st.limits.max_result_bytes;
        blocking(move || {
            let result = dataset.dataset.reasoning().justify(&request)?;
            let body = serde_json::to_vec(&result.to_json())
                .map_err(|error| err(StatusCode::INTERNAL_SERVER_ERROR, error.to_string()))?;
            output(body, "application/json", limit)
        })
        .await
    }
}
