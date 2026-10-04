//! The SHACL constraints layer of the schema report: `shapes=` on `GET /$/schema/{ds}`
//! and `GET /$/schema/{ds}/constraints`.
//!
//! Without `shapes=`, the layer holds the shapes of the dataset's write-time SHACL
//! validation, when it has one, and is left out otherwise. `shapes=guard` asks for them
//! explicitly, `shapes=default` and `shapes=<graph IRI>` read shapes graphs of the
//! dataset, and `shapes=none` leaves the layer out. The layer is computed for every
//! request from the shapes alone: it is not part of the cached report and never derived
//! from the counts.

use super::super::{ApiResult, Params, St, blocking, dataset};
use super::bad;
use crate::auth::Principal;
use axum::Extension;
use axum::Json;
use axum::extract::Path;
use axum::http::Uri;
use axum::response::IntoResponse;
use serde_json::json;
pub(crate) use sparkles::handles::ShapesRequest;
use sparkles::store::Snapshot;
use std::sync::Arc;
use std::time::Instant;

/// The `shapes` parameters of a request.
pub(in crate::http) fn shapes_param(params: &Params) -> ApiResult<ShapesRequest> {
    ShapesRequest::from_values(&params.all("shapes")).map_err(bad)
}

/// `GET /$/schema/{ds}/constraints`: the constraints layer alone, without computing the
/// rest of the report.
pub(in crate::http) async fn constraints(
    st: St,
    Path(name): Path<String>,
    Extension(p): Extension<Principal>,
    uri: Uri,
) -> ApiResult {
    let ds = dataset(&st, &name)?;
    let params = Params::from_query(&uri);
    let req = shapes_param(&params)?;
    let at = super::super::history::at_param(&params)?;
    let view = p.view(&ds.name, crate::auth::Endpoint::Info);
    let timeout = st.default_timeout;
    blocking(move || {
        let snap: Arc<Snapshot> = match &at {
            None => ds.store.snapshot(),
            Some(at) => {
                let o = sparkles::history::HistoryOptions {
                    cancel: None,
                    deadline: Some(Instant::now() + timeout),
                };
                ds.store.snapshot_at(at, &o)?.0
            }
        };
        let layer = ds
            .dataset
            .schema()
            .constraints_at(&snap, &req, view.as_deref())?
            .unwrap_or_default();
        Ok(Json(json!({
            "schemaFormat": sparkles::schema::SCHEMA_FORMAT,
            "dataset": ds.name,
            "snapshot": {
                "version": sparkles::schema::snapshot_identity(&snap),
                "generation": snap.generation.name,
            },
            "constraints": layer,
        }))
        .into_response())
    })
    .await
}
