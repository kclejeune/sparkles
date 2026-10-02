//! The SHACL constraints layer of the schema report: `shapes=` on `GET /$/schema/{ds}`
//! and `GET /$/schema/{ds}/constraints`.
//!
//! Without `shapes=`, the layer holds the shapes of the dataset's write-time SHACL
//! validation, when it has one, and is left out otherwise. `shapes=guard` asks for them
//! explicitly, `shapes=default` and `shapes=<graph IRI>` read shapes graphs of the
//! dataset, and `shapes=none` leaves the layer out. The layer is computed for every
//! request from the shapes alone: it is not part of the cached report and never derived
//! from the counts.

use super::super::{ApiResult, Params, St, blocking, dataset, err};
use super::bad;
use crate::auth::Principal;
use crate::state::Dataset;
use axum::Extension;
use axum::Json;
use axum::extract::Path;
use axum::http::{StatusCode, Uri};
use axum::response::IntoResponse;
use serde_json::json;
use sparkles::access::GraphAccess;
use sparkles::schema::ConstraintsLayer;
use sparkles::store::Snapshot;
use std::sync::Arc;
use std::time::Instant;

/// Which shapes the layer is built from.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct ShapesRequest {
    /// the guard's shapes: `None` when present (no `shapes=`), `Some(true)` when asked
    /// for, `Some(false)` when not
    pub guard: Option<bool>,
    /// shapes graphs: IRIs, or `default`
    pub graphs: Vec<String>,
}

impl ShapesRequest {
    /// The sources named by `shapes=` values (`--shapes` of the CLI): `guard`, `none`,
    /// `default` or a graph IRI. No value means the guard's shapes when there are any.
    pub(crate) fn from_values<S: AsRef<str>>(values: &[S]) -> Result<ShapesRequest, String> {
        if values.is_empty() {
            return Ok(ShapesRequest::default());
        }
        let mut r = ShapesRequest {
            guard: Some(false),
            graphs: Vec::new(),
        };
        let mut none = false;
        for v in values {
            let g = match v.as_ref().trim() {
                "none" => {
                    none = true;
                    continue;
                }
                "guard" => {
                    r.guard = Some(true);
                    continue;
                }
                "default" | sparkles::sparql::ctx::DEFAULT_GRAPH_IRI => "default".to_string(),
                "union" | sparkles::sparql::ctx::UNION_GRAPH_IRI => {
                    return Err(
                        "shapes: name the graphs that hold shapes, not the union graph".into(),
                    );
                }
                iri => {
                    let iri = iri
                        .strip_prefix('<')
                        .and_then(|i| i.strip_suffix('>'))
                        .unwrap_or(iri);
                    oxrdf::NamedNode::new(iri)
                        .map_err(|e| format!("shapes: invalid graph IRI '{iri}': {e}"))?;
                    iri.to_string()
                }
            };
            if !r.graphs.contains(&g) {
                r.graphs.push(g);
            }
        }
        if none && values.len() > 1 {
            return Err("shapes=none cannot be combined with other shapes".into());
        }
        Ok(r)
    }

    /// The `shapes` parameters of a request.
    pub(in crate::http) fn parse(params: &Params) -> ApiResult<ShapesRequest> {
        Self::from_values(&params.all("shapes")).map_err(bad)
    }
}

/// Why a constraints layer could not be built: an HTTP status and a message.
#[derive(Debug)]
pub(crate) struct LayerError {
    pub status: u16,
    pub message: String,
}

fn layer_error(status: u16, message: impl Into<String>) -> LayerError {
    LayerError {
        status,
        message: message.into(),
    }
}

/// The constraints layer of `req` over `snap`, or `None` when it has no source. A
/// caller limited to some graphs (`view`) reads only shapes graphs it may read, and
/// sees the guard's shapes only when it may read all of the guard's shapes graphs.
#[cfg(feature = "shacl")]
pub(crate) fn build(
    ds: &Dataset,
    snap: &Snapshot,
    req: &ShapesRequest,
    view: Option<&GraphAccess>,
) -> Result<Option<ConstraintsLayer>, LayerError> {
    let view = view.filter(|v| !v.reads_all());
    let readable = |g: &str| {
        view.is_none_or(|v| {
            if g == "default" {
                v.read.default_graph()
            } else {
                v.read.allows_iri(g)
            }
        })
    };
    let mut layer = ConstraintsLayer::default();
    if req.guard != Some(false) {
        let guard = match ds.validation.read().as_ref() {
            Some(crate::state::Validation::Shacl(g)) => Some(g.clone()),
            _ => None,
        };
        let guard = guard.filter(|g| {
            g.config()
                .shapes
                .graphs
                .iter()
                .flatten()
                .all(|s| readable(s))
        });
        match guard {
            Some(g) => layer
                .sources
                .push(sparkles_shacl::constraints::guard_source(&g)),
            None if req.guard == Some(true) => {
                return Err(layer_error(
                    404,
                    format!(
                        "dataset {} has no write-time SHACL validation whose shapes this caller may read",
                        ds.name
                    ),
                ));
            }
            None => {}
        }
    }
    if !req.graphs.is_empty() {
        for g in &req.graphs {
            let exists = g == "default" || crate::validation_common::graph_exists(snap, g);
            if !exists || !readable(g) {
                return Err(layer_error(404, format!("no such graph: <{g}>")));
            }
        }
        let source = sparkles_shacl::constraints::graphs_source(snap, &req.graphs)
            .map_err(|e| layer_error(400, format!("shapes: {e:#}")))?;
        layer.sources.push(source);
    }
    Ok((!layer.is_empty()).then_some(layer))
}

#[cfg(not(feature = "shacl"))]
pub(crate) fn build(
    _ds: &Dataset,
    _snap: &Snapshot,
    req: &ShapesRequest,
    _view: Option<&GraphAccess>,
) -> Result<Option<ConstraintsLayer>, LayerError> {
    if req.guard == Some(true) || !req.graphs.is_empty() {
        return Err(layer_error(501, "built without the `shacl` feature"));
    }
    Ok(None)
}

/// [`build`] for the HTTP handlers.
pub(in crate::http) fn layer(
    ds: &Dataset,
    snap: &Snapshot,
    req: &ShapesRequest,
    view: Option<&GraphAccess>,
) -> ApiResult<Option<ConstraintsLayer>> {
    build(ds, snap, req, view).map_err(|e| {
        err(
            StatusCode::from_u16(e.status).unwrap_or(StatusCode::BAD_REQUEST),
            e.message,
        )
    })
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
    let req = ShapesRequest::parse(&params)?;
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
        let layer = layer(&ds, &snap, &req, view.as_deref())?.unwrap_or_default();
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
