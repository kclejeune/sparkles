//! Fuseki's admin API where Sparkles' own routes do not already answer it: dataset
//! descriptions (`ds.name`, `ds.state`, `ds.services`), taking a dataset offline,
//! `/$/stats` with Fuseki's request counters, `/$/backups-list`, the `/$/validate/*`
//! services, assembler bodies on `POST /$/datasets`, and Graph Store direct naming.

use super::{ApiResult, Params, St, err};
use crate::auth::{Level, Principal};
use crate::state::{AppState, Dataset};
use axum::Json;
use axum::Router;
use axum::extract::{Extension, Path, State};
use axum::http::{StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::routing::{any, get};
use serde_json::{Map, Value as J, json};
use std::sync::Arc;
use std::sync::atomic::Ordering;

pub(super) mod assembler;
mod validate;

/// The routes, merged into `http::router` before its layers.
pub(super) fn routes() -> Router<Arc<AppState>> {
    let r = Router::new()
        .route("/$/stats", get(stats_all).post(stats_all))
        .route("/$/backups-list", get(backups_list).post(backups_list))
        .route(
            "/$/validate/query",
            get(validate::query).post(validate::query),
        )
        .route(
            "/$/validate/update",
            get(validate::update).post(validate::update),
        )
        .route("/$/validate/iri", get(validate::iri).post(validate::iri))
        .route("/$/validate/data", get(validate::data).post(validate::data))
        .route(
            "/$/validate/langtag",
            get(validate::langtag).post(validate::langtag),
        )
        .route("/{ds}/{*graph}", any(direct));
    // Fuseki's alias of `/$/backup/{ds}`; with the `backup` feature the repository
    // routes answer it (`backup::http::create_backup_or_dump`)
    #[cfg(not(feature = "backup"))]
    let r = r.route("/$/backups/{ds}", axum::routing::post(super::backup));
    r
}

// ------------------------------------------------------- dataset descriptions ------

/// Fuseki's operations and the endpoint names Sparkles serves them at: (`srv.type`,
/// `srv.description`, endpoints; `""` is the dataset URL itself).
fn services(st: &AppState) -> Vec<(&'static str, &'static str, Vec<&'static str>)> {
    let mut s = vec![
        ("query", "SPARQL Query", vec!["", "sparql", "query"]),
        ("update", "SPARQL Update", vec!["", "update"]),
        ("gsp-rw", "Graph Store Protocol", vec!["", "data"]),
        ("gsp-r", "Graph Store Protocol (Read)", vec!["get"]),
        ("upload", "File Upload", vec!["upload"]),
        ("prefixes-rw", "Read-write prefixes", vec!["prefixes"]),
    ];
    if cfg!(feature = "shacl") {
        s.push(("SHACL", "SHACL Validation", vec!["shacl"]));
    }
    if st.gsp_direct_naming {
        s.push((
            "gsp-direct-rw",
            "Graph Store Protocol (Direct naming)",
            vec![""],
        ));
    }
    s
}

/// The members Fuseki's dataset descriptions have (`/$/datasets`, `/$/server`), added
/// to Sparkles' `DatasetInfo`.
pub(super) fn describe(st: &AppState, ds: &Dataset) -> Map<String, J> {
    let services: Vec<J> = services(st)
        .into_iter()
        .map(|(t, d, e)| json!({ "srv.type": t, "srv.description": d, "srv.endpoints": e }))
        .collect();
    let mut m = Map::new();
    m.insert("ds.name".into(), format!("/{}", ds.name).into());
    m.insert(
        "ds.state".into(),
        (!ds.offline.load(Ordering::Relaxed)).into(),
    );
    m.insert("ds.services".into(), services.into());
    m
}

/// `POST /$/datasets/{ds}?state=offline|active` (Fuseki): an offline dataset's services
/// answer `503` until it is active again. Admin routes keep working. The state is not
/// persisted: a restart brings every dataset back active.
pub(super) async fn set_state(State(st): St, Path(name): Path<String>, uri: Uri) -> ApiResult {
    let ds = super::dataset(&st, &name)?;
    let params = Params::from_query(&uri);
    let offline = match params.get("state") {
        Some("offline") => true,
        Some("active") => false,
        None | Some("") => {
            return Err(err(StatusCode::BAD_REQUEST, "No state change given"));
        }
        Some(s) => {
            return Err(err(
                StatusCode::BAD_REQUEST,
                format!("state must be offline or active, not '{s}'"),
            ));
        }
    };
    ds.offline.store(offline, Ordering::Relaxed);
    tracing::info!(
        dataset = %name,
        "dataset /{name} is {}",
        if offline { "offline" } else { "active" }
    );
    Ok(StatusCode::OK.into_response())
}

/// The `503` of a request to an offline dataset's services.
pub(super) fn offline_response(name: &str) -> Response {
    let body = json!({
        "error": format!("dataset /{name} is offline"),
        "code": "dataset-offline",
    });
    let mut r = (StatusCode::SERVICE_UNAVAILABLE, Json(body.clone())).into_response();
    r.extensions_mut().insert(super::ErrorJson(body));
    r
}

// ------------------------------------------------------------------- stats ------

/// A dataset's entry of Fuseki's `/$/stats`: `Requests`, `RequestsGood`, `RequestsBad`
/// and the same per endpoint that has had a request. Endpoints are keyed by name, the
/// dataset URL itself as `_1`, `_2`, …, and a name with two operations (`data`) once
/// per operation.
pub(super) fn fuseki_stats(st: &AppState, ds: &Dataset) -> J {
    let (mut good, mut bad) = (0u64, 0u64);
    let mut endpoints = Map::new();
    let mut unnamed = 0;
    for (name, operation, description, g, b) in st.metrics.fuseki_endpoint_counts(&ds.name) {
        good += g;
        bad += b;
        let key = if name.is_empty() {
            unnamed += 1;
            format!("_{unnamed}")
        } else if endpoints.contains_key(name) {
            format!("{name}_{operation}")
        } else {
            name.to_string()
        };
        endpoints.insert(
            key,
            json!({
                "Requests": g + b,
                "RequestsGood": g,
                "RequestsBad": b,
                "operation": operation,
                "description": description,
            }),
        );
    }
    json!({
        "Requests": good + bad,
        "RequestsGood": good,
        "RequestsBad": bad,
        "endpoints": endpoints,
    })
}

/// `GET|POST /$/stats` (Fuseki): `{datasets: {"/ds": counters}}` for every dataset the
/// caller may read.
async fn stats_all(State(st): St, Extension(p): Extension<Principal>) -> Json<J> {
    let datasets: Vec<Arc<Dataset>> = st.datasets.read().values().cloned().collect();
    let mut out = Map::new();
    for ds in datasets.iter().filter(|d| p.can(&d.name, Level::Read)) {
        out.insert(format!("/{}", ds.name), fuseki_stats(&st, ds));
    }
    Json(json!({ "datasets": out }))
}

// ----------------------------------------------------------------- backups ------

/// `GET|POST /$/backups-list` (Fuseki): `{backups: [file name]}`, sorted, of the
/// N-Quads backups in `<data>/backups`. A caller without `server-admin` sees the files of
/// the datasets it administers.
async fn backups_list(State(st): St, Extension(p): Extension<Principal>) -> ApiResult<Json<J>> {
    let dir = st.data_dir.join("backups");
    let admin = p.has(crate::auth::ServerPerm::ServerAdmin);
    let names: Vec<String> = st.datasets.read().keys().cloned().collect();
    let mut files: Vec<String> = match std::fs::read_dir(&dir) {
        Ok(rd) => rd
            .filter_map(Result::ok)
            .filter(|e| e.file_type().is_ok_and(|t| t.is_file()))
            .filter_map(|e| e.file_name().into_string().ok())
            .filter(|f| !f.starts_with('.'))
            .collect(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(e) => return Err(err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string())),
    };
    if !admin {
        files.retain(|f| backup_dataset(f, &names).is_some_and(|ds| p.can(ds, Level::Admin)));
    }
    files.sort();
    Ok(Json(json!({ "backups": files })))
}

/// The dataset a backup file `{ds}_{time}.nq…` belongs to: the longest dataset name
/// followed by `_` and a digit.
fn backup_dataset<'a>(file: &str, names: &'a [String]) -> Option<&'a str> {
    names
        .iter()
        .filter(|n| {
            file.strip_prefix(n.as_str())
                .and_then(|r| r.strip_prefix('_'))
                .is_some_and(|r| r.starts_with(|c: char| c.is_ascii_digit()))
        })
        .max_by_key(|n| n.len())
        .map(String::as_str)
}

// ------------------------------------------------------------ direct naming ------

/// `/{ds}/{path}` with `serve --gsp-direct-naming` (Fuseki's direct naming): the Graph
/// Store Protocol on the graph whose IRI is the request URL without its query string.
/// `?graph=` and `?default` do not apply. Other parameters, such as `timeout` and
/// `receipt`, do. Without the flag, and under `/$/`, the path is not found.
#[allow(clippy::too_many_arguments)]
async fn direct(
    st: St,
    Path((name, _)): Path<(String, String)>,
    p: Extension<Principal>,
    method: axum::http::Method,
    uri: Uri,
    headers: axum::http::HeaderMap,
    body: axum::body::Body,
) -> ApiResult {
    if !st.gsp_direct_naming || name == "$" {
        return Ok(StatusCode::NOT_FOUND.into_response());
    }
    let params = Params::from_query(&uri);
    if params.has("graph") || params.has("default") {
        return Err(err(
            StatusCode::BAD_REQUEST,
            "a direct graph name takes no graph or default parameter",
        ));
    }
    let iri = direct_iri(&uri, &headers);
    super::gsp_on(st, Path(name), p, method, uri, headers, body, Some(iri)).await
}

/// The request URL without its query. The scheme and host are those a proxy forwarded
/// (`X-Forwarded-Proto`, `X-Forwarded-Host`), else `http` and the `Host` header.
fn direct_iri(uri: &Uri, headers: &axum::http::HeaderMap) -> String {
    let first = |h: &str| {
        headers
            .get(h)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.split(',').next())
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(str::to_string)
    };
    let scheme = first("x-forwarded-proto")
        .or_else(|| uri.scheme_str().map(str::to_string))
        .unwrap_or_else(|| "http".into());
    let host = first("x-forwarded-host")
        .or_else(|| first("host"))
        .or_else(|| uri.authority().map(|a| a.as_str().to_string()))
        .unwrap_or_else(|| "localhost".into());
    format!("{scheme}://{host}{}", uri.path())
}

#[cfg(test)]
mod tests {
    use super::backup_dataset;

    #[test]
    fn backup_files_belong_to_the_longest_name() {
        let names = vec!["a".to_string(), "a_b".to_string(), "c".to_string()];
        assert_eq!(
            backup_dataset("a_b_2026-01-01T00-00-00Z.nq.zst", &names),
            Some("a_b")
        );
        assert_eq!(
            backup_dataset("a_2026-01-01T00-00-00Z.nq.gz", &names),
            Some("a")
        );
        assert_eq!(backup_dataset("c.nq", &names), None);
        assert_eq!(backup_dataset("d_2026.nq", &names), None);
    }
}
