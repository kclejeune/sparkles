//! Vector indexes in the server: `/$/vector/{ds}` (status) and
//! `/$/vector/{ds}/{name}` (create, replace, drop, rebuild, recall, reembed), the
//! embedding workers of indexes that compute their vectors, and `sparkles vector`
//! against a local store.
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
use anyhow::{Context, Result, bail};
use axum::extract::{Path, State};
use axum::http::{StatusCode, Uri};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::{Value as J, json};
use sparkles::id::Id;
use sparkles::store::{Store, StoreOptions};
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

/// What `sparkles vector` does.
#[allow(clippy::large_enum_variant)]
pub enum Action {
    Create {
        name: String,
        config: VectorIndexConfig,
    },
    Drop {
        name: String,
    },
    Rebuild {
        name: String,
    },
    /// the status of every index, or of one
    Status {
        name: Option<String>,
    },
    List,
    /// embed every selected text of an index again
    Reembed {
        name: String,
        run: EmbedRun,
    },
    /// embed what is waiting (local only)
    Embed {
        name: Option<String>,
        run: EmbedRun,
    },
}

/// How a local run of the embedding worker reaches the provider.
#[derive(clap::Args, Clone)]
pub struct EmbedRun {
    /// A secret the index's apiKey names: NAME=env:VARIABLE or NAME=file:PATH
    /// (repeatable)
    #[arg(long, value_name = "NAME=SOURCE")]
    embedding_secret: Vec<String>,
    /// Stop after this many seconds if work is left
    #[arg(long, value_name = "SECS", default_value_t = 3600.0)]
    embed_timeout: f64,
    #[command(flatten)]
    outbound: crate::outbound::OutboundArgs,
}

/// Parse `--embedding-secret NAME=env:VAR|file:PATH` flags.
pub fn parse_secrets(
    flags: &[String],
) -> Result<std::collections::BTreeMap<String, sparkles::vector::embed::SecretSource>> {
    let mut out = std::collections::BTreeMap::new();
    for f in flags {
        let (name, src) = f.split_once('=').with_context(|| {
            format!("--embedding-secret {f}: expected NAME=env:VAR or NAME=file:PATH")
        })?;
        if name.is_empty() {
            bail!("--embedding-secret {f}: the name is empty");
        }
        let src = src
            .parse()
            .map_err(|e: String| anyhow::anyhow!("--embedding-secret {e}"))?;
        out.insert(name.to_string(), src);
    }
    Ok(out)
}

impl EmbedRun {
    /// The environment of a local run: the local outbound policy (private destinations
    /// allowed unless --outbound-block-private) and the secrets of the flags.
    fn environment(&self) -> Result<sparkles::vector::embed::Environment> {
        Ok(sparkles::vector::embed::Environment {
            enabled: true,
            outbound: self.outbound.local_policy()?,
            secrets: parse_secrets(&self.embedding_secret)?,
        })
    }

    /// Embed what is waiting in `store`, reporting the status of `names` after.
    fn run(&self, store: &Store, names: &[String]) -> Result<()> {
        if !(self.embed_timeout.is_finite() && self.embed_timeout > 0.0) {
            bail!("--embed-timeout must be a positive number of seconds");
        }
        store.set_embedding_environment(Some(self.environment()?));
        let t = std::time::Instant::now();
        let r = store.embed_until_idle(std::time::Duration::from_secs_f64(self.embed_timeout));
        for n in names {
            if let Some(s) = store.embedding_status(n) {
                eprintln!(
                    "vector index {n}: {} vectors written, {} failed, {} waiting, {} requests in {:.1} s{}",
                    s.embedded,
                    s.failed,
                    s.backlog,
                    s.requests,
                    t.elapsed().as_secs_f64(),
                    s.last_error
                        .map(|e| format!("; last error: {}", e.message))
                        .unwrap_or_default()
                );
            }
        }
        match r {
            Ok(()) => Ok(()),
            Err(sparkles::Error::Timeout) => {
                bail!("embedding is not finished: the provider failed or --embed-timeout passed")
            }
            Err(e) => Err(e.into()),
        }
    }
}

/// `sparkles vector …` on a local store (`--loc`).
pub fn run_local(loc: &std::path::Path, opts: StoreOptions, action: Action) -> Result<()> {
    let store = Store::open(loc, opts)?;
    match action {
        Action::Create { name, config } => {
            let t = std::time::Instant::now();
            let created = store.create_vector_index(&name, config)?;
            let s = store
                .wait_vector_index(&name)
                .context("the index was dropped")?;
            eprintln!(
                "vector index {name} {}: {} rows, {} in {:.0} ms",
                if created { "created" } else { "replaced" },
                s.rows,
                s.state,
                t.elapsed().as_secs_f64() * 1000.0
            );
            if s.state != "ready" {
                bail!("{}", s.message.unwrap_or(s.state));
            }
        }
        Action::Drop { name } => {
            store.drop_vector_index(&name)?;
            eprintln!("vector index {name} dropped");
        }
        Action::Rebuild { name } => {
            let t = std::time::Instant::now();
            store.rebuild_vector_index(&name)?;
            let s = store
                .wait_vector_index(&name)
                .context("the index was dropped")?;
            eprintln!(
                "vector index {name} rebuilt: {} rows, {} in {:.0} ms",
                s.rows,
                s.state,
                t.elapsed().as_secs_f64() * 1000.0
            );
        }
        Action::Status { name } => {
            // indexes are read from their files (or built) when the store opens
            let all = store.wait_vector_indexes();
            let v = match name {
                Some(n) => serde_json::to_value(
                    all.into_iter()
                        .find(|s| s.name == n)
                        .with_context(|| format!("no vector index {n}"))?,
                )?,
                None => serde_json::to_value(all)?,
            };
            println!("{}", serde_json::to_string_pretty(&v)?);
        }
        Action::List => print_list(&serde_json::to_value(store.vector_indexes())?),
        Action::Reembed { name, run } => {
            store.reembed(&name)?;
            run.run(&store, std::slice::from_ref(&name))?;
        }
        Action::Embed { name, run } => {
            let names: Vec<String> = match name {
                Some(n) => vec![n],
                None => store
                    .vector_indexes()
                    .into_iter()
                    .filter(|s| s.embedding.is_some())
                    .map(|s| s.name)
                    .collect(),
            };
            if names.is_empty() {
                bail!("no vector index computes embeddings");
            }
            run.run(&store, &names)?;
        }
    }
    Ok(())
}

/// A table of indexes (a JSON array of `VectorIndexStatus`).
pub fn print_list(v: &J) {
    let Some(ix) = v.as_array() else {
        return;
    };
    if ix.is_empty() {
        eprintln!("no vector indexes");
        return;
    }
    println!(
        "{:<20} {:<12} {:>6} {:<9} {:>10} {:<10} {:<18} PREDICATE",
        "NAME", "STATE", "DIM", "METRIC", "ROWS", "HNSW", "EMBEDDING"
    );
    for s in ix {
        let hnsw = match &s["hnsw"] {
            J::Null => "off".to_string(),
            h => format!("M={} ef={}", h["m"], h["efSearch"]),
        };
        let embedding = match &s["embedding"] {
            J::Null => "-".to_string(),
            e => format!(
                "{} ({} waiting)",
                e["state"].as_str().unwrap_or(""),
                e["backlog"]
            ),
        };
        println!(
            "{:<20} {:<12} {:>6} {:<9} {:>10} {:<10} {:<18} {}",
            s["name"].as_str().unwrap_or(""),
            s["state"].as_str().unwrap_or(""),
            s["dimension"],
            s["metric"].as_str().unwrap_or(""),
            s["rows"],
            hnsw,
            embedding,
            s["predicate"].as_str().unwrap_or("")
        );
    }
}

// -------------------------------------------------------------------------- CLI ----

/// Where `sparkles vector` works: a local database, or a dataset on a server.
#[derive(clap::Args, Clone)]
pub struct Target {
    /// Database directory
    #[arg(long, required_unless_present = "server")]
    loc: Option<std::path::PathBuf>,
    /// A server to send this to instead of a local database (with --dataset)
    #[arg(long, env = "SPARKLES_SERVER")]
    server: Option<String>,
    /// The dataset on --server
    #[arg(long)]
    dataset: Option<String>,
    /// Allow plain http to a --server other than localhost
    #[arg(long)]
    insecure_http: bool,
}

/// The configuration flags of `sparkles vector create`.
#[derive(clap::Args)]
struct CreateArgs {
    /// The index name
    #[arg(long)]
    name: String,
    /// The embedding predicate (an IRI)
    #[arg(long)]
    predicate: String,
    /// Vectors of other dimensions are not indexed
    #[arg(long)]
    dim: usize,
    /// cosine (default), dot or euclidean
    #[arg(long)]
    metric: Option<String>,
    /// A label of the embedding model
    #[arg(long)]
    model: Option<String>,
    /// HNSW links per node (default 16)
    #[arg(long)]
    m: Option<usize>,
    /// HNSW candidates while building (default 128)
    #[arg(long)]
    ef_construction: Option<usize>,
    /// HNSW candidates while searching (default 128)
    #[arg(long)]
    ef_search: Option<usize>,
    /// Searches over at most this many rows are exact (default 10000)
    #[arg(long)]
    exact_threshold: Option<usize>,
    /// Pack the vectors without an HNSW graph (exact search only)
    #[arg(long)]
    no_hnsw: bool,
    /// Compute the vectors with this OpenAI-compatible embeddings endpoint (e.g.
    /// http://127.0.0.1:11434/v1/embeddings for Ollama)
    #[arg(long, value_name = "URL")]
    embed_url: Option<String>,
    /// The model the endpoint embeds with
    #[arg(long, value_name = "MODEL")]
    embed_model: Option<String>,
    /// A predicate whose string literals are embedded (repeatable)
    #[arg(long, value_name = "IRI")]
    embed_from: Vec<String>,
    /// Embed only literals with this language range ("" for untagged; repeatable)
    #[arg(long, value_name = "RANGE")]
    embed_lang: Vec<String>,
    /// Embed only subjects of this class (repeatable)
    #[arg(long, value_name = "IRI")]
    embed_class: Vec<String>,
    /// A SELECT query binding ?s and ?text (and optionally ?g), instead of --embed-from
    #[arg(long, value_name = "SPARQL")]
    embed_query: Option<String>,
    /// The API key: the environment variable holding it (local use)
    #[arg(long, value_name = "VAR", group = "embed_key")]
    embed_api_key_env: Option<String>,
    /// The API key: a file holding it (local use)
    #[arg(long, value_name = "PATH", group = "embed_key")]
    embed_api_key_file: Option<String>,
    /// The API key: a secret the server defines with --embedding-secret
    #[arg(long, value_name = "NAME", group = "embed_key")]
    embed_secret: Option<String>,
    /// The whole embedding object as JSON (a file, or inline JSON starting with '{');
    /// the other --embed-* flags override its fields
    #[arg(long, value_name = "FILE|JSON")]
    embed_config: Option<String>,
}

impl CreateArgs {
    /// The embedding object the --embed-* flags describe, if any.
    fn embedding(&self) -> Result<Option<sparkles::vector::embed::EmbeddingConfig>> {
        use sparkles::vector::embed::{ApiKey, EmbeddingConfig};
        let mut e: Option<EmbeddingConfig> = match &self.embed_config {
            Some(c) => {
                let text = if c.trim_start().starts_with('{') {
                    c.clone()
                } else {
                    std::fs::read_to_string(c).with_context(|| format!("--embed-config {c}"))?
                };
                Some(serde_json::from_str(&text).context("--embed-config")?)
            }
            None => None,
        };
        let any = self.embed_url.is_some()
            || self.embed_model.is_some()
            || !self.embed_from.is_empty()
            || self.embed_query.is_some();
        if e.is_none() && !any {
            return Ok(None);
        }
        let e = e.get_or_insert_with(|| EmbeddingConfig::new("", ""));
        if let Some(u) = &self.embed_url {
            e.url = u.clone();
        }
        if let Some(m) = &self.embed_model {
            e.model = m.clone();
        }
        if !self.embed_from.is_empty() {
            e.predicates = self.embed_from.clone();
        }
        if !self.embed_lang.is_empty() {
            e.languages = Some(self.embed_lang.clone());
        }
        if !self.embed_class.is_empty() {
            e.classes = self.embed_class.clone();
        }
        if let Some(q) = &self.embed_query {
            e.query = Some(q.clone());
        }
        if let Some(v) = &self.embed_api_key_env {
            e.api_key = Some(ApiKey::Env(v.clone()));
        }
        if let Some(p) = &self.embed_api_key_file {
            e.api_key = Some(ApiKey::File(p.clone()));
        }
        if let Some(n) = &self.embed_secret {
            e.api_key = Some(ApiKey::Secret(n.clone()));
        }
        if e.url.is_empty() || e.model.is_empty() {
            bail!("embedding needs --embed-url and --embed-model");
        }
        Ok(Some(e.clone()))
    }

    /// The index configuration the flags describe.
    fn config(&self) -> Result<VectorIndexConfig> {
        let mut c = VectorIndexConfig::new(&self.predicate, self.dim);
        if let Some(m) = &self.metric {
            c.metric = sparkles::vector::Metric::parse(m)
                .with_context(|| format!("--metric {m}: cosine, dot or euclidean"))?;
        }
        c.model = self.model.clone();
        if self.no_hnsw {
            c.hnsw = None;
        } else if let Some(h) = c.hnsw.as_mut() {
            h.m = self.m.unwrap_or(h.m);
            h.ef_construction = self.ef_construction.unwrap_or(h.ef_construction);
            h.ef_search = self.ef_search.unwrap_or(h.ef_search);
        }
        if let Some(t) = self.exact_threshold {
            c.exact_threshold = t;
        }
        c.embedding = self.embedding()?;
        c.validate()?;
        Ok(c)
    }
}

#[derive(clap::Args)]
pub struct VectorArgs {
    #[command(subcommand)]
    cmd: VectorCmd,
}

#[derive(clap::Subcommand)]
#[allow(clippy::large_enum_variant)]
enum VectorCmd {
    /// Create (or replace) an index and wait for its build
    Create {
        #[command(flatten)]
        target: Target,
        #[command(flatten)]
        args: CreateArgs,
    },
    /// Drop an index and its files
    Drop {
        #[command(flatten)]
        target: Target,
        #[arg(long)]
        name: String,
    },
    /// Build an index again from RDF and wait for it
    Rebuild {
        #[command(flatten)]
        target: Target,
        #[arg(long)]
        name: String,
    },
    /// The indexes as a table
    List {
        #[command(flatten)]
        target: Target,
    },
    /// The status of every index (or of --name) as JSON
    Status {
        #[command(flatten)]
        target: Target,
        #[arg(long)]
        name: Option<String>,
    },
    /// Embed every selected text of an index again (after its model changed); a local
    /// store embeds right away
    Reembed {
        #[command(flatten)]
        target: Target,
        #[arg(long)]
        name: String,
        #[command(flatten)]
        run: EmbedRun,
    },
    /// Embed the text waiting in a local database now, and exit
    Embed {
        /// Database directory
        #[arg(long)]
        loc: std::path::PathBuf,
        /// Only this index
        #[arg(long)]
        name: Option<String>,
        #[command(flatten)]
        run: EmbedRun,
    },
}

/// `sparkles vector …`
pub fn cli(a: VectorArgs, opts: StoreOptions) -> Result<()> {
    let (target, action) = match a.cmd {
        VectorCmd::Create { target, args } => {
            let config = args.config()?;
            (
                target,
                Action::Create {
                    name: args.name,
                    config,
                },
            )
        }
        VectorCmd::Drop { target, name } => (target, Action::Drop { name }),
        VectorCmd::Rebuild { target, name } => (target, Action::Rebuild { name }),
        VectorCmd::List { target } => (target, Action::List),
        VectorCmd::Status { target, name } => (target, Action::Status { name }),
        VectorCmd::Reembed { target, name, run } => (target, Action::Reembed { name, run }),
        VectorCmd::Embed { loc, name, run } => {
            return run_local(&loc, opts, Action::Embed { name, run });
        }
    };
    match &target.loc {
        Some(loc) => run_local(loc, opts, action),
        None => run_remote(&target, action),
    }
}

#[cfg(not(feature = "auth"))]
fn run_remote(t: &Target, _: Action) -> Result<()> {
    let _ = (&t.server, &t.dataset, t.insecure_http);
    bail!("--server: built without the remote client (cargo feature \"auth\")")
}

/// `sparkles vector … --server URL --dataset DS`
#[cfg(feature = "auth")]
fn run_remote(t: &Target, action: Action) -> Result<()> {
    use reqwest::Method;
    let ds = t
        .dataset
        .as_deref()
        .context("--dataset NAME is required with --server")?;
    let r = crate::remote::Remote::open(t.server.as_deref(), t.insecure_http)?;
    let enc = |s: &str| {
        percent_encoding::utf8_percent_encode(s, percent_encoding::NON_ALPHANUMERIC).to_string()
    };
    let base = format!("/$/vector/{}", enc(ds));
    // follow a build until it is over
    let wait = |name: &str| -> Result<J> {
        loop {
            let s = r.get_json(&format!("{base}/{}", enc(name)))?;
            if s["state"] != "building" {
                return Ok(s);
            }
            std::thread::sleep(std::time::Duration::from_millis(500));
        }
    };
    let report = |name: &str, what: &str, s: &J| -> Result<()> {
        eprintln!(
            "vector index {name} {what}: {} rows, {}",
            s["rows"],
            s["state"].as_str().unwrap_or("")
        );
        if s["state"] != "ready" {
            bail!("{}", s["message"].as_str().unwrap_or("not ready"));
        }
        Ok(())
    };
    match action {
        Action::Create { name, config } => {
            let resp = r.check(
                r.req(Method::PUT, &format!("{base}/{}", enc(&name)))
                    .header("content-type", "application/json")
                    .body(serde_json::to_vec(&config)?)
                    .send(),
                Some(ds),
            )?;
            let what = if resp.status().as_u16() == StatusCode::CREATED.as_u16() {
                "created"
            } else {
                "replaced"
            };
            report(&name, what, &wait(&name)?)?;
        }
        Action::Drop { name } => {
            r.check(
                r.req(Method::DELETE, &format!("{base}/{}", enc(&name)))
                    .send(),
                Some(ds),
            )?;
            eprintln!("vector index {name} dropped");
        }
        Action::Rebuild { name } => {
            r.check(
                r.req(Method::POST, &format!("{base}/{}/rebuild", enc(&name)))
                    .send(),
                Some(ds),
            )?;
            report(&name, "rebuilt", &wait(&name)?)?;
        }
        Action::Status { name: Some(name) } => {
            println!("{}", serde_json::to_string_pretty(&wait(&name)?)?);
        }
        Action::Status { name: None } => {
            let j = r.get_json(&base)?;
            println!("{}", serde_json::to_string_pretty(&j["indexes"])?);
        }
        Action::List => print_list(&r.get_json(&base)?["indexes"]),
        Action::Reembed { name, .. } => {
            let resp = r.check(
                r.req(Method::POST, &format!("{base}/{}/reembed", enc(&name)))
                    .send(),
                Some(ds),
            )?;
            let s: J = serde_json::from_slice(&resp.bytes()?)?;
            eprintln!(
                "vector index {name}: re-embedding on the server ({} waiting, state {})",
                s["embedding"]["backlog"],
                s["embedding"]["state"].as_str().unwrap_or("")
            );
        }
        Action::Embed { .. } => bail!("vector embed works on a local database (--loc)"),
    }
    Ok(())
}
