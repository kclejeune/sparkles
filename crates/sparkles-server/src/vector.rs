//! Vector indexes in the server: `/$/vector/{ds}` (status) and
//! `/$/vector/{ds}/{name}` (create, replace, drop, rebuild, recall, reembed) and the
//! embedding workers of indexes that compute their vectors.
//!
//! Authorization (the route table in `auth/routes.rs`): the `GET`s and `POST …/recall`
//! need `read` on `{ds}`; `PUT`, `DELETE`, `POST …/rebuild` and `POST …/reembed` need
//! `admin`.
//!
//! Embedding requests leave the server only for an index whose configuration names an
//! endpoint, through the server's outbound policy. Configurations set through the API
//! name their API key by an operator-defined secret (`serve --embedding-secret`), never
//! by an environment variable or file of the server.

use crate::http::{AdminBody, ApiResult, blocking, dataset, err, task_start_check};
use anyhow::bail;
use axum::extract::{Path, State};
use axum::http::{StatusCode, Uri};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::{Value as J, json};
use sparkles::id::Id;
use sparkles::vector::{VectorIndexConfig, VectorIndexStatus};
use std::sync::Arc;

type St = State<Arc<crate::state::AppState>>;

/// The task kind of vector index builds.
const TASK: &str = "vector-index";

pub fn routes() -> Router<Arc<crate::state::AppState>> {
    Router::new()
        .route("/$/vector/{ds}", get(status))
        .route(
            "/$/vector/{ds}/{name}",
            get(index_status).put(create).delete(drop_index),
        )
        .route("/$/vector/{ds}/{name}/rebuild", post(rebuild))
        .route("/$/vector/{ds}/{name}/reembed", post(reembed))
        .route("/$/vector/{ds}/{name}/recall", post(recall))
}

/// `GET /$/vector/{ds}`: the memory budget, the configured indexes, and the predicates
/// packed without a configuration in the current generation (on their first search).
async fn status(State(st): St, Path(name): Path<String>) -> ApiResult<Json<J>> {
    let ds = dataset(&st, &name)?;
    let snap = ds.store.snapshot();
    let vectors = &snap.generation.vectors;
    let predicates: Vec<J> = vectors
        .status()
        .into_iter()
        .map(|p| {
            let iri = match snap.term(Id(p.predicate)) {
                Some(oxrdf::Term::NamedNode(n)) => n.into_string(),
                other => other.map(|t| t.to_string()).unwrap_or_default(),
            };
            json!({
                "predicate": iri,
                "bytes": p.bytes,
                "malformed": p.malformed,
                "dimensions": p.dims.iter().map(|(dim, rows)| json!({ "dimension": dim, "vectors": rows })).collect::<Vec<_>>(),
            })
        })
        .collect();
    Ok(Json(json!({
        "budgetBytes": sparkles::vector::budget(),
        "usedBytes": vectors.used_bytes(),
        "generation": snap.generation.name,
        "indexes": ds.dataset.indexes().vector().list(),
        "predicates": predicates,
    })))
}

fn unknown(name: &str) -> crate::http::ApiError {
    err(StatusCode::NOT_FOUND, format!("no vector index {name}"))
}

/// `GET /$/vector/{ds}/{name}`
async fn index_status(
    State(st): St,
    Path((ds_name, name)): Path<(String, String)>,
) -> ApiResult<Json<VectorIndexStatus>> {
    let ds = dataset(&st, &ds_name)?;
    ds.dataset
        .indexes()
        .vector()
        .get(&name)
        .map(Json)
        .ok_or_else(|| unknown(&name))
}

/// `PUT /$/vector/{ds}/{name}`: create (`201`) or replace (`200`) the index; the body
/// is its configuration. The build runs in a background task:
/// `{"index": VectorIndexStatus, "task": Task}`.
async fn create(
    State(st): St,
    Path((ds_name, name)): Path<(String, String)>,
    AdminBody(body): AdminBody,
) -> ApiResult {
    if st.read_only {
        return Err(err(StatusCode::FORBIDDEN, "server is read-only"));
    }
    let ds = dataset(&st, &ds_name)?;
    let cfg: VectorIndexConfig = serde_json::from_slice(&body).map_err(|e| {
        err(
            StatusCode::BAD_REQUEST,
            format!("invalid vector index configuration: {e}"),
        )
    })?;
    cfg.validate().map_err(|e| {
        err(
            StatusCode::BAD_REQUEST,
            format!("invalid vector index configuration: {e}"),
        )
    })?;
    if let Some(e) = &cfg.embedding {
        check_embedding(&sparkles::vector::embed::environment(), e).map_err(|m| {
            err(
                StatusCode::BAD_REQUEST,
                format!("invalid vector index configuration: embedding.{m}"),
            )
        })?;
    }
    task_start_check(&st, None, &ds_name)?;
    let created = {
        let ds = ds.clone();
        let name = name.clone();
        blocking(move || Ok(ds.dataset.indexes().vector().put(&name, cfg)?)).await?
    };
    let index = ds
        .dataset
        .indexes()
        .vector()
        .get(&name)
        .ok_or_else(|| unknown(&name))?;
    ensure_worker(&st, &ds);
    let task = start_wait(&st, &ds_name, ds, name, "building the vector index");
    let code = if created {
        StatusCode::CREATED
    } else {
        StatusCode::OK
    };
    Ok((code, Json(json!({ "index": index, "task": task }))).into_response())
}

/// A task that follows the build of index `name` until it is over.
fn start_wait(
    st: &Arc<crate::state::AppState>,
    ds_name: &str,
    ds: Arc<crate::state::Dataset>,
    name: String,
    what: &'static str,
) -> crate::state::Task {
    st.start_task(TASK, ds_name, move |h| {
        h.progress(0.0, what);
        let s = ds
            .dataset
            .indexes()
            .vector()
            .wait(&name)
            .ok_or_else(|| anyhow::anyhow!("vector index {name} was dropped"))?;
        if s.state == "failed" || s.state == "over-budget" {
            bail!(
                "vector index {name}: {}",
                s.message.unwrap_or_else(|| s.state.clone())
            );
        }
        Ok(format!("vector index {name}: {} rows, {}", s.rows, s.state))
    })
}

/// `DELETE /$/vector/{ds}/{name}`
async fn drop_index(State(st): St, Path((ds_name, name)): Path<(String, String)>) -> ApiResult {
    if st.read_only {
        return Err(err(StatusCode::FORBIDDEN, "server is read-only"));
    }
    let ds = dataset(&st, &ds_name)?;
    blocking(move || {
        ds.dataset.indexes().vector().drop(&name)?;
        Ok(StatusCode::NO_CONTENT.into_response())
    })
    .await
}

/// `POST /$/vector/{ds}/{name}/rebuild`: build the index again from RDF (`202` and a
/// task).
async fn rebuild(State(st): St, Path((ds_name, name)): Path<(String, String)>) -> ApiResult {
    if st.read_only {
        return Err(err(StatusCode::FORBIDDEN, "server is read-only"));
    }
    let ds = dataset(&st, &ds_name)?;
    task_start_check(&st, None, &ds_name)?;
    ds.dataset.indexes().vector().rebuild(&name)?;
    let task = start_wait(&st, &ds_name, ds, name, "rebuilding the vector index");
    Ok((StatusCode::ACCEPTED, Json(task)).into_response())
}

/// `POST /$/vector/{ds}/{name}/reembed`: embed every selected text of the index again
/// (`202` and the index's status).
async fn reembed(State(st): St, Path((ds_name, name)): Path<(String, String)>) -> ApiResult {
    if st.read_only {
        return Err(err(StatusCode::FORBIDDEN, "server is read-only"));
    }
    let ds = dataset(&st, &ds_name)?;
    let vectors = ds.dataset.indexes().vector();
    let index = vectors.get(&name).ok_or_else(|| unknown(&name))?;
    if index.embedding.is_none() {
        return Err(err(
            StatusCode::BAD_REQUEST,
            format!("vector index {name} has no embedding configuration"),
        ));
    }
    vectors.reembed(&name)?;
    ensure_worker(&st, &ds);
    let index = vectors.get(&name).ok_or_else(|| unknown(&name))?;
    Ok((StatusCode::ACCEPTED, Json(index)).into_response())
}

/// What the API accepts of an embedding configuration beyond its own validation: an
/// API key named by a secret the operator defined, and an endpoint the outbound policy
/// does not refuse outright (an address literal; names are checked when resolved).
pub fn check_embedding(
    env: &sparkles::vector::embed::Environment,
    e: &sparkles::vector::embed::EmbeddingConfig,
) -> std::result::Result<(), String> {
    use sparkles::vector::embed::ApiKey;
    match &e.api_key {
        Some(k) if k.is_local() => {
            return Err(
                "apiKey: the API accepts {\"secret\": NAME}, a secret defined with `serve --embedding-secret NAME=env:VAR|file:PATH`; environment variables and files are for the local command line"
                    .into(),
            );
        }
        Some(ApiKey::Secret(n)) if !env.secrets.contains_key(n) => {
            return Err(format!(
                "apiKey: no secret named {n:?} is defined (serve --embedding-secret {n}=env:VAR|file:PATH)"
            ));
        }
        _ => {}
    }
    env.outbound
        .check_url(&e.url)
        .map_err(|f| format!("url: {f}"))?;
    Ok(())
}

/// Start the embedding worker of `ds` if it has an index that embeds and none runs
/// (not on a read-only server, or with `--no-embedding`).
pub fn ensure_worker(st: &Arc<crate::state::AppState>, ds: &Arc<crate::state::Dataset>) {
    if st.read_only || !sparkles::vector::embed::environment().enabled {
        return;
    }
    let shared = ds.store.embedder();
    if !shared.active() || !shared.claim_spawn() {
        return;
    }
    let weak = Arc::downgrade(ds);
    let name = ds.name.clone();
    let spawned = std::thread::Builder::new()
        .name(format!("embed-{name}"))
        .spawn(move || {
            tracing::info!("embedding worker of /{name} started");
            sparkles::vector::embed::run_worker(shared, &|f| match weak.upgrade() {
                Some(ds) => {
                    f(&ds.store);
                    true
                }
                None => false,
            });
            tracing::info!("embedding worker of /{name} stopped");
        });
    if let Err(e) = spawned {
        tracing::error!("cannot start an embedding worker: {e}");
    }
}

/// Every second, start the embedding workers that datasets need: those opened, restored
/// or configured since.
pub fn spawn_embedders(st: Arc<crate::state::AppState>) {
    let spawned = std::thread::Builder::new()
        .name("embed-supervisor".into())
        .spawn(move || {
            loop {
                let all: Vec<Arc<crate::state::Dataset>> =
                    st.datasets.read().values().cloned().collect();
                for ds in &all {
                    ensure_worker(&st, ds);
                }
                drop(all);
                std::thread::sleep(std::time::Duration::from_secs(1));
            }
        });
    if let Err(e) = spawned {
        tracing::error!("cannot start the embedding supervisor: {e}");
    }
}

/// The embedding series of the indexes that embed (`sparkles_embedding_*`), by dataset
/// label and index. Datasets past `--metrics-max-datasets` share `$other`: their counters
/// and backlogs add up, and their lag is the largest.
pub fn metrics(st: &crate::state::AppState, out: &mut String) {
    use sparkles::vector::embed::FailureKind;
    use std::collections::BTreeMap;
    use std::fmt::Write;
    let datasets: Vec<Arc<crate::state::Dataset>> = st.datasets.read().values().cloned().collect();
    let mut all: BTreeMap<(String, String), sparkles::vector::embed::EmbeddingMetrics> =
        BTreeMap::new();
    for d in &datasets {
        for m in d.store.embedding_metrics() {
            let key = (st.metrics.dataset_label(Some(&d.name)), m.index.clone());
            let a = all.entry(key).or_default();
            a.requests += m.requests;
            a.inputs += m.inputs;
            a.vectors += m.vectors;
            for (x, y) in a.failures.iter_mut().zip(m.failures) {
                *x += y;
            }
            a.backlog += m.backlog;
            a.lag = a.lag.max(m.lag);
        }
    }
    if all.is_empty() {
        return;
    }
    let label = |s: &str| {
        s.replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace('\n', "\\n")
    };
    let family = |o: &mut String, name: &str, kind: &str, help: &str| {
        let _ = writeln!(o, "# HELP {name} {help}");
        let _ = writeln!(o, "# TYPE {name} {kind}");
    };
    type Pick = fn(&sparkles::vector::embed::EmbeddingMetrics) -> u64;
    let simple: [(&str, &str, &str, Pick); 5] = [
        (
            "sparkles_embedding_requests_total",
            "counter",
            "Requests to the embeddings endpoint, retries included.",
            |m| m.requests,
        ),
        (
            "sparkles_embedding_inputs_total",
            "counter",
            "Inputs sent to the embeddings endpoint (cached inputs are not sent).",
            |m| m.inputs,
        ),
        (
            "sparkles_embedding_vectors_total",
            "counter",
            "Vectors the embedding worker wrote.",
            |m| m.vectors,
        ),
        (
            "sparkles_embedding_backlog",
            "gauge",
            "Subjects (per graph) waiting to be embedded.",
            |m| m.backlog,
        ),
        (
            "sparkles_embedding_lag_commits",
            "gauge",
            "Commits since the newest one whose text is all embedded.",
            |m| m.lag,
        ),
    ];
    for (name, kind, help, pick) in simple {
        family(out, name, kind, help);
        for ((ds, index), m) in &all {
            let _ = writeln!(
                out,
                "{name}{{dataset=\"{}\",index=\"{}\"}} {}",
                label(ds),
                label(index),
                pick(m)
            );
        }
    }
    family(
        out,
        "sparkles_embedding_failures_total",
        "counter",
        "Embedding failures by kind: failed batches (transient, auth, refused, fatal, write), inputs the provider refused or answered with an unusable vector (rejected), and pairs whose text could not be read (read).",
    );
    for ((ds, index), m) in &all {
        for (k, n) in FailureKind::ALL.iter().zip(m.failures) {
            let _ = writeln!(
                out,
                "sparkles_embedding_failures_total{{dataset=\"{}\",index=\"{}\",kind=\"{}\"}} {n}",
                label(ds),
                label(index),
                k.name()
            );
        }
    }
}

/// `POST /$/vector/{ds}/{name}/recall?samples=100&k=10&ef=`: recall@k of the graph
/// against the exact search, with stored vectors as the queries.
async fn recall(
    State(st): St,
    Path((ds_name, name)): Path<(String, String)>,
    uri: Uri,
) -> ApiResult<Json<J>> {
    let ds = dataset(&st, &ds_name)?;
    let params: Vec<(String, String)> = uri
        .query()
        .map(|q| form_urlencoded::parse(q.as_bytes()).into_owned().collect())
        .unwrap_or_default();
    let num = |k: &str, d: usize, max: usize| -> ApiResult<usize> {
        match params.iter().find(|(a, _)| a == k) {
            None => Ok(d),
            Some((_, v)) => v
                .parse::<usize>()
                .ok()
                .filter(|n| (1..=max).contains(n))
                .ok_or_else(|| err(StatusCode::BAD_REQUEST, format!("{k}: 1 to {max}"))),
        }
    };
    let samples = num("samples", 100, 10_000)?;
    let k = num("k", 10, sparkles::vector::MAX_K)?;
    let ef = match params.iter().any(|(a, _)| a == "ef") {
        true => Some(num("ef", 64, sparkles::vector::config::MAX_EF)?),
        false => None,
    };
    let opts = sparkles::handles::RecallOptions { samples, k, ef };
    let r = blocking(move || Ok(ds.dataset.indexes().vector().recall(&name, &opts)?)).await?;
    Ok(Json(serde_json::to_value(r).unwrap()))
}
