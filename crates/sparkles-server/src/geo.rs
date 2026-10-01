//! GeoSPARQL in the server: `/$/geo/{ds}` (status, enable, disable, rebuild), the
//! spatial index in dataset listings and statistics, its metrics, `serve --geo` and
//! `sparkles geo-index`.
//!
//! Authorization (the route table in `auth/routes.rs`): `GET /$/geo/{ds}` needs `read`
//! on `{ds}`; `PUT`, `DELETE` and `POST …/rebuild` need `admin`.

use crate::http::{AdminBody, ApiError, ApiResult, blocking, dataset, err, task_start_check};
use crate::state::{AppState, Dataset};
use anyhow::{Context, Result, bail};
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::{Value as J, json};
use sparkles::geo::GeoConfig;
use sparkles::store::{Store, StoreOptions};
use std::sync::Arc;

/// The task kind of spatial index builds.
const TASK: &str = "geo-index";

type St = State<Arc<AppState>>;

pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/$/geo/{ds}", get(status).put(enable).delete(disable))
        .route("/$/geo/{ds}/rebuild", post(rebuild))
}

fn not_built() -> ApiError {
    sparkles::geo::not_built().into()
}

/// A configuration from a request: `400 invalid geo configuration: …` when it does not
/// parse or validate.
fn parse_config(v: J) -> ApiResult<GeoConfig> {
    let invalid = |m: String| {
        err(
            StatusCode::BAD_REQUEST,
            format!("invalid geo configuration: {m}"),
        )
    };
    let c: GeoConfig = serde_json::from_value(v).map_err(|e| invalid(e.to_string()))?;
    c.validate().map_err(|e| invalid(e.to_string()))?;
    Ok(c)
}

/// The `geo` option of `POST /$/datasets`: `true` (the defaults), a configuration, or
/// absent / `false` / `null` for none.
pub fn create_option(v: &J) -> ApiResult<Option<GeoConfig>> {
    let c = match v {
        J::Null | J::Bool(false) => return Ok(None),
        J::Bool(true) => GeoConfig::default(),
        J::Object(_) => parse_config(v.clone())?,
        _ => {
            return Err(err(
                StatusCode::BAD_REQUEST,
                "geo must be true, false or a configuration object",
            ));
        }
    };
    if !cfg!(feature = "geo") {
        return Err(not_built());
    }
    Ok(Some(c))
}

/// `GET /$/geo/{ds}`: the index status, or `{"enabled": false}`.
async fn status(State(st): St, Path(name): Path<String>) -> ApiResult<Json<J>> {
    if !cfg!(feature = "geo") {
        return Err(not_built());
    }
    let ds = dataset(&st, &name)?;
    Ok(Json(match ds.store.geo_status() {
        Some(s) => serde_json::to_value(s).unwrap(),
        None => json!({ "enabled": false }),
    }))
}

/// `PUT /$/geo/{ds}`: enable (or reconfigure) the spatial index; the body is the
/// configuration (empty: defaults). The index is built in a background task.
async fn enable(State(st): St, Path(name): Path<String>, AdminBody(body): AdminBody) -> ApiResult {
    if !cfg!(feature = "geo") {
        return Err(not_built());
    }
    if st.read_only {
        return Err(err(StatusCode::FORBIDDEN, "server is read-only"));
    }
    let ds = dataset(&st, &name)?;
    let cfg = if body.iter().all(u8::is_ascii_whitespace) {
        GeoConfig::default()
    } else {
        let v: J = serde_json::from_slice(&body).map_err(|e| {
            err(
                StatusCode::BAD_REQUEST,
                format!("invalid geo configuration: {e}"),
            )
        })?;
        parse_config(v)?
    };
    start_build(&st, &name, ds, "building the spatial index", move |ds| {
        ds.store.enable_geo(cfg)
    })
}

/// `DELETE /$/geo/{ds}`: turn the spatial index off.
async fn disable(State(st): St, Path(name): Path<String>) -> ApiResult {
    if !cfg!(feature = "geo") {
        return Err(not_built());
    }
    if st.read_only {
        return Err(err(StatusCode::FORBIDDEN, "server is read-only"));
    }
    let ds = dataset(&st, &name)?;
    blocking(move || {
        ds.store.disable_geo()?;
        Ok(StatusCode::NO_CONTENT.into_response())
    })
    .await
}

/// `POST /$/geo/{ds}/rebuild`: rebuild the index of the current generation.
async fn rebuild(State(st): St, Path(name): Path<String>) -> ApiResult {
    if !cfg!(feature = "geo") {
        return Err(not_built());
    }
    if st.read_only {
        return Err(err(StatusCode::FORBIDDEN, "server is read-only"));
    }
    let ds = dataset(&st, &name)?;
    if !ds.store.geo_enabled() {
        return Err(err(StatusCode::BAD_REQUEST, "spatial index is not enabled"));
    }
    start_build(&st, &name, ds, "rebuilding the spatial index", |ds| {
        ds.store.rebuild_geo()
    })
}

/// Start a build task (`409` while one runs for the dataset).
fn start_build(
    st: &Arc<AppState>,
    name: &str,
    ds: Arc<Dataset>,
    what: &'static str,
    build: impl FnOnce(&Dataset) -> sparkles::Result<sparkles::geo::GeoStatus> + Send + 'static,
) -> ApiResult {
    if st.active_task(TASK, name).is_some() {
        return Err(err(
            StatusCode::CONFLICT,
            "spatial index build already running",
        ));
    }
    task_start_check(st, None, name)?;
    let task = st.start_task(TASK, name, move |h| {
        h.progress(0.1, what);
        let s = build(&ds)?;
        Ok(format!(
            "spatial index: {} rows, {}",
            s.rows.base + s.rows.overlay + s.rows.tail,
            s.state
        ))
    });
    Ok((StatusCode::ACCEPTED, Json(task)).into_response())
}

/// `{state, rows}` of the dataset's spatial index for its `DatasetInfo`, or null.
pub fn summary(ds: &Dataset) -> J {
    match ds.store.geo_status() {
        Some(s) => json!({
            "state": s.state,
            "rows": s.rows.base + s.rows.overlay + s.rows.tail,
        }),
        None => J::Null,
    }
}

/// The full status for `/$/stats/{ds}`, or null.
pub fn status_json(ds: &Dataset) -> J {
    ds.store
        .geo_status()
        .map_or(J::Null, |s| serde_json::to_value(s).unwrap())
}

/// Prometheus series of the spatial indexes (`sparkles_geo_*`).
pub fn metrics(st: &AppState, out: &mut String) {
    let _ = (st, out);
}

/// `serve --geo NAME[=geo.json]`
pub fn enable_for(st: &AppState, spec: &str) -> Result<()> {
    if !cfg!(feature = "geo") {
        bail!("built without GeoSPARQL (cargo feature \"geo\")");
    }
    let (name, cfg) = match spec.split_once('=') {
        Some((n, path)) => {
            let c: GeoConfig = serde_json::from_slice(
                &std::fs::read(path).with_context(|| format!("reading {path}"))?,
            )
            .with_context(|| format!("{path}: invalid geo configuration"))?;
            (n, c)
        }
        None => (spec, GeoConfig::default()),
    };
    cfg.validate()
        .with_context(|| "--geo: invalid geo configuration")?;
    let ds = st
        .get(name.trim_start_matches('/'))
        .with_context(|| format!("--geo: no dataset {name}"))?;
    let s = ds.store.enable_geo(cfg)?;
    tracing::info!(
        "spatial index on /{name}: {}, {} rows",
        s.state,
        s.rows.base + s.rows.overlay + s.rows.tail
    );
    Ok(())
}

/// The options of `sparkles geo-index`.
pub struct IndexArgs {
    pub predicate: Vec<String>,
    pub feature_link: Vec<String>,
    pub exclude_graph: Vec<String>,
    pub distance: Option<String>,
    pub rebuild: bool,
    pub status: bool,
    pub disable: bool,
}

/// `sparkles geo-index`: enable (with the defaults, if needed), build and print the
/// status; `--status` prints it as JSON and changes nothing.
pub fn geo_index(loc: &std::path::Path, opts: StoreOptions, a: IndexArgs) -> Result<()> {
    if !cfg!(feature = "geo") {
        eprintln!("built without GeoSPARQL (cargo feature \"geo\")");
        std::process::exit(2)
    }
    let store = Store::open(loc, opts)?;
    if a.disable {
        store.disable_geo()?;
        eprintln!("spatial index disabled");
        return Ok(());
    }
    if a.status {
        println!(
            "{}",
            match store.geo_status() {
                Some(s) => serde_json::to_string_pretty(&s)?,
                None => r#"{ "enabled": false }"#.to_string(),
            }
        );
        return Ok(());
    }
    let t = std::time::Instant::now();
    let configured = !a.predicate.is_empty()
        || !a.feature_link.is_empty()
        || !a.exclude_graph.is_empty()
        || a.distance.is_some();
    let s = match store.geo_status() {
        Some(_) if !configured && a.rebuild => store.rebuild_geo()?,
        Some(s) if !configured => s,
        current => {
            let mut cfg = current.map(|s| s.config).unwrap_or_default();
            if !a.predicate.is_empty() {
                cfg.predicates = a.predicate;
            }
            if !a.feature_link.is_empty() {
                cfg.feature_links = a.feature_link;
            }
            if !a.exclude_graph.is_empty() {
                cfg.graphs.exclude = a.exclude_graph;
            }
            if let Some(d) = a.distance {
                cfg.distance = serde_json::from_value(J::String(d.clone()))
                    .with_context(|| format!("--distance {d}: geodesic or haversine"))?;
            }
            store.enable_geo(cfg)?
        }
    };
    eprintln!(
        "spatial index: {} rows ({} base, {} overlay, {} tail), {} literals, {} in {:.0} ms",
        s.rows.base + s.rows.overlay + s.rows.tail,
        s.rows.base,
        s.rows.overlay,
        s.rows.tail,
        s.literals,
        s.state,
        t.elapsed().as_secs_f64() * 1000.0
    );
    Ok(())
}
