//! GeoSPARQL in the server: `/$/geo/{ds}` (status, enable, disable, rebuild), the
//! map endpoints `GET /{ds}/geo` (indexed geometries in a box) and `POST /$/geo/convert`
//! (literals as GeoJSON), the spatial index in dataset listings and statistics, its
//! metrics, `serve --geo` and `sparkles geo-index`.
//!
//! Authorization (the route table in `auth/routes.rs`): `GET /$/geo/{ds}` and
//! `GET /{ds}/geo` need `read` on `{ds}`; `PUT`, `DELETE` and `POST …/rebuild` need
//! `admin`; `POST /$/geo/convert` reads no dataset and is open to any caller.

use crate::http::{
    AdminBody, ApiError, ApiResult, QueryBody, blocking, dataset, err, task_start_check,
};
use crate::state::{AppState, Dataset};
use anyhow::{Context, Result, bail};
use axum::extract::{Path, State};
use axum::http::{StatusCode, Uri};
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
        .route("/$/geo/convert", post(convert))
        .route("/{ds}/geo", get(features))
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

/// `GET /{ds}/geo?bbox=minLon,minLat,maxLon,maxLat&graph=&predicate=&limit=&tolerance=`:
/// the indexed geometries meeting a CRS84 box as a GeoJSON `FeatureCollection`.
async fn features(
    State(st): St,
    Path(name): Path<String>,
    axum::Extension(p): axum::Extension<crate::auth::Principal>,
    uri: Uri,
) -> ApiResult {
    if !cfg!(feature = "geo") {
        return Err(not_built());
    }
    let ds = dataset(&st, &name)?;
    #[cfg(feature = "geo")]
    {
        let q = box_query(&uri)?;
        let graphs = p.view(&ds.name, crate::auth::Endpoint::Query);
        let fc = blocking(move || {
            Ok(sparkles::geo::map::features_in_box_of(
                &ds.store.snapshot(),
                &q,
                graphs.as_deref(),
            )?)
        })
        .await?;
        Ok((
            [(axum::http::header::CONTENT_TYPE, "application/geo+json")],
            Json(fc),
        )
            .into_response())
    }
    #[cfg(not(feature = "geo"))]
    {
        let _ = (ds, uri, p);
        Err(not_built())
    }
}

/// The parameters of `GET /{ds}/geo` (`400` naming the bad one).
#[cfg(feature = "geo")]
fn box_query(uri: &Uri) -> ApiResult<sparkles::geo::map::BoxQuery> {
    use sparkles::geo::map::{BoxQuery, DEFAULT_LIMIT, MAX_LIMIT};
    let bad = |m: String| err(StatusCode::BAD_REQUEST, m);
    let params: Vec<(String, String)> = uri
        .query()
        .map(|q| form_urlencoded::parse(q.as_bytes()).into_owned().collect())
        .unwrap_or_default();
    let get = |k: &str| params.iter().find(|(a, _)| a == k).map(|(_, v)| v.as_str());
    let raw =
        get("bbox").ok_or_else(|| bad("bbox: required (minLon,minLat,maxLon,maxLat)".into()))?;
    let v: Vec<f64> = raw
        .split(',')
        .map(|x| x.trim().parse::<f64>())
        .collect::<Result<_, _>>()
        .map_err(|_| bad(format!("bbox: {raw:?} is not four numbers")))?;
    let [min_lon, min_lat, max_lon, max_lat] = v[..] else {
        return Err(bad(format!("bbox: {raw:?} is not four numbers")));
    };
    if !(v.iter().all(|x| x.is_finite())
        && min_lon <= max_lon
        && min_lat <= max_lat
        && (-90.0..=90.0).contains(&min_lat)
        && (-90.0..=90.0).contains(&max_lat))
    {
        return Err(bad(format!(
            "bbox: {raw:?} is not a box of longitudes and latitudes (min before max)"
        )));
    }
    let limit = match get("limit") {
        None => DEFAULT_LIMIT,
        Some(l) => match l.parse::<usize>() {
            Ok(n) if (1..=MAX_LIMIT).contains(&n) => n,
            _ => return Err(bad(format!("limit: between 1 and {MAX_LIMIT}"))),
        },
    };
    let tolerance = match get("tolerance") {
        None => None,
        Some(t) => match t.parse::<f64>() {
            Ok(x) if x.is_finite() && x >= 0.0 => Some(x),
            _ => return Err(bad("tolerance: a number of degrees, at least 0".into())),
        },
    };
    Ok(BoxQuery {
        bbox: [min_lon, min_lat, max_lon, max_lat],
        graph: get("graph").map(str::to_string),
        predicate: get("predicate").map(str::to_string),
        limit,
        tolerance,
    })
}

/// `POST /$/geo/convert` `{"literals": [{"value", "datatype"}]}`: each literal as a
/// CRS84 GeoJSON geometry, or the reason it has none, in order:
/// `{"results": [{"geometry": {…}} | {"error": "…"}]}`.
async fn convert(QueryBody(body): QueryBody) -> ApiResult<Json<J>> {
    if !cfg!(feature = "geo") {
        return Err(not_built());
    }
    #[cfg(feature = "geo")]
    {
        use sparkles::geo::convert::{ConvertItem, MAX_ITEMS};
        #[derive(serde::Deserialize)]
        struct Req {
            literals: Vec<ConvertItem>,
        }
        let req: Req = serde_json::from_slice(&body).map_err(|e| {
            err(
                StatusCode::BAD_REQUEST,
                format!("expected {{\"literals\": [{{\"value\", \"datatype\"}}]}}: {e}"),
            )
        })?;
        if req.literals.len() > MAX_ITEMS {
            return Err(err(
                StatusCode::BAD_REQUEST,
                format!("literals: at most {MAX_ITEMS} per request"),
            ));
        }
        let results = blocking(move || Ok(sparkles::geo::convert::convert(&req.literals)?)).await?;
        Ok(Json(json!({ "results": results })))
    }
    #[cfg(not(feature = "geo"))]
    {
        let _ = body;
        Err(not_built())
    }
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

/// The explain counters of spatial operators that the metrics sum up.
pub const WORK: [&str; 4] = ["candidates", "refined", "matched", "rechecked"];

/// Sums of the [`WORK`] counters.
pub type Work = [u64; WORK.len()];

/// The work of a query's spatial operators, from their explain counters: candidates,
/// exact tests, matches and candidates the index could not place (`None` when the plan
/// has no spatial operator).
pub fn plan_work(plan: &sparkles::sparql::PlanInfo) -> Option<Work> {
    fn walk(p: &sparkles::sparql::PlanInfo, sum: &mut Option<Work>) {
        if let Some(c) = &p.counters
            && c.contains_key("candidates")
        {
            let s = sum.get_or_insert([0; WORK.len()]);
            for (x, k) in s.iter_mut().zip(WORK) {
                *x += c.get(k).and_then(J::as_u64).unwrap_or(0);
            }
        }
        for child in &p.children {
            walk(child, sum);
        }
    }
    let mut sum = None;
    walk(plan, &mut sum);
    sum
}

/// The spatial index series of one dataset label (datasets past `--metrics-max-datasets`
/// share `$other`).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct GeoSeries {
    /// whether a dataset of the label has the index enabled
    pub enabled: bool,
    /// rows in the base, the overlay and the tail
    pub rows: [u64; 3],
    /// the longest last build of the label's datasets
    pub build_seconds: Option<f64>,
    /// the spatial operators' candidates, exact tests and matches
    pub work: Work,
}

/// The spatial index series by dataset label: datasets with the index, and labels whose
/// queries ran spatial operators.
pub fn series(st: &AppState) -> std::collections::BTreeMap<String, GeoSeries> {
    let mut out: std::collections::BTreeMap<String, GeoSeries> = Default::default();
    let datasets: Vec<Arc<Dataset>> = st.datasets.read().values().cloned().collect();
    for ds in datasets {
        let Some(s) = ds.store.geo_status() else {
            continue;
        };
        let e = out
            .entry(st.metrics.dataset_label(Some(&ds.name)))
            .or_default();
        e.enabled = true;
        for (x, n) in e
            .rows
            .iter_mut()
            .zip([s.rows.base, s.rows.overlay, s.rows.tail])
        {
            *x += n;
        }
        if let Some(b) = &s.last_build {
            let secs = b.ms / 1000.0;
            e.build_seconds = Some(e.build_seconds.map_or(secs, |x| x.max(secs)));
        }
    }
    for (label, w) in st.metrics.geo_work() {
        if w.iter().any(|&n| n > 0) || out.contains_key(&label) {
            out.entry(label).or_default().work = w;
        }
    }
    out
}

/// The `geo` member of a dataset in the JSON metrics snapshot, or null.
pub fn series_json(s: Option<&GeoSeries>) -> J {
    match s {
        None => J::Null,
        Some(s) => json!({
            "enabled": s.enabled,
            "rows": { "base": s.rows[0], "overlay": s.rows[1], "tail": s.rows[2] },
            "buildSeconds": s.build_seconds,
            "candidates": s.work[0],
            "refined": s.work[1],
            "matches": s.work[2],
            "rechecked": s.work[3],
        }),
    }
}

/// Prometheus series of the spatial indexes: rows per part, the last build's duration,
/// and the work of spatial operators (`sparkles_geo_*`).
pub fn metrics(st: &AppState, out: &mut String) {
    use std::fmt::Write;
    let family = |o: &mut String, name: &str, kind: &str, help: &str| {
        let _ = writeln!(o, "# HELP {name} {help}");
        let _ = writeln!(o, "# TYPE {name} {kind}");
    };
    let label = |s: &str| {
        s.replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace('\n', "\\n")
    };
    let all = series(st);
    if all.is_empty() {
        return;
    }
    family(
        out,
        "sparkles_geo_rows",
        "gauge",
        "Rows of the spatial index by part (base, overlay, tail).",
    );
    for (ds, s) in all.iter().filter(|(_, s)| s.enabled) {
        for (part, n) in ["base", "overlay", "tail"].iter().zip(s.rows) {
            let _ = writeln!(
                out,
                "sparkles_geo_rows{{dataset=\"{}\",part=\"{part}\"}} {n}",
                label(ds)
            );
        }
    }
    family(
        out,
        "sparkles_geo_build_seconds",
        "gauge",
        "Duration of the last build of the spatial index's base, in seconds.",
    );
    for (ds, s) in &all {
        if let Some(b) = s.build_seconds {
            let _ = writeln!(
                out,
                "sparkles_geo_build_seconds{{dataset=\"{}\"}} {b:.3}",
                label(ds)
            );
        }
    }
    for (i, (name, help)) in [
        (
            "sparkles_geo_candidates_total",
            "Index candidates of spatial operators (rows whose envelope matched).",
        ),
        (
            "sparkles_geo_refined_total",
            "Exact geometry tests run by spatial operators.",
        ),
        (
            "sparkles_geo_matches_total",
            "Rows that passed the exact test of spatial operators.",
        ),
        (
            "sparkles_geo_rechecked_total",
            "Candidates of spatial operators whose literal the index skipped (too long to index, or with no place in longitude and latitude), tested whatever the search window.",
        ),
    ]
    .into_iter()
    .enumerate()
    {
        family(out, name, "counter", help);
        for (ds, s) in &all {
            let _ = writeln!(out, "{name}{{dataset=\"{}\"}} {}", label(ds), s.work[i]);
        }
    }
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
    pub wgs84: bool,
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
    // the index is built in memory when the store opens: report it once built
    let current = store.wait_geo();
    if a.status {
        println!(
            "{}",
            match current {
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
        || a.distance.is_some()
        || a.wgs84;
    let s = match current {
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
            if a.wgs84 {
                cfg.wgs84 = true;
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
