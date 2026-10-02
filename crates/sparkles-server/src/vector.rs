//! Vector indexes in the server: `/$/vector/{ds}` (status) and
//! `/$/vector/{ds}/{name}` (create, replace, drop, rebuild, recall), and
//! `sparkles vector` against a local store.
//!
//! Authorization (the route table in `auth/routes.rs`): the `GET`s and `POST …/recall`
//! need `read` on `{ds}`; `PUT`, `DELETE` and `POST …/rebuild` need `admin`.

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
        "indexes": ds.store.vector_indexes(),
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
    ds.store
        .vector_index(&name)
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
    task_start_check(&st, None, &ds_name)?;
    let created = {
        let ds = ds.clone();
        let name = name.clone();
        blocking(move || Ok(ds.store.create_vector_index(&name, cfg)?)).await?
    };
    let index = ds.store.vector_index(&name).ok_or_else(|| unknown(&name))?;
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
            .store
            .wait_vector_index(&name)
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
        ds.store.drop_vector_index(&name)?;
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
    ds.store.rebuild_vector_index(&name)?;
    let task = start_wait(&st, &ds_name, ds, name, "rebuilding the vector index");
    Ok((StatusCode::ACCEPTED, Json(task)).into_response())
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
    let r = blocking(move || Ok(ds.store.vector_recall(&name, samples, k, ef)?)).await?;
    Ok(Json(serde_json::to_value(r).unwrap()))
}

/// What `sparkles vector` does.
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
        "{:<20} {:<12} {:>6} {:<9} {:>10} {:<10} PREDICATE",
        "NAME", "STATE", "DIM", "METRIC", "ROWS", "HNSW"
    );
    for s in ix {
        let hnsw = match &s["hnsw"] {
            J::Null => "off".to_string(),
            h => format!("M={} ef={}", h["m"], h["efSearch"]),
        };
        println!(
            "{:<20} {:<12} {:>6} {:<9} {:>10} {:<10} {}",
            s["name"].as_str().unwrap_or(""),
            s["state"].as_str().unwrap_or(""),
            s["dimension"],
            s["metric"].as_str().unwrap_or(""),
            s["rows"],
            hnsw,
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
    /// HNSW candidates while searching (default 64)
    #[arg(long)]
    ef_search: Option<usize>,
    /// Searches over at most this many rows are exact (default 10000)
    #[arg(long)]
    exact_threshold: Option<usize>,
    /// Pack the vectors without an HNSW graph (exact search only)
    #[arg(long)]
    no_hnsw: bool,
}

impl CreateArgs {
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
    };
    match &target.loc {
        Some(loc) => run_local(loc, opts, action),
        None => run_remote(&target, action),
    }
}

#[cfg(not(feature = "auth"))]
fn run_remote(_: &Target, _: Action) -> Result<()> {
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
    }
    Ok(())
}
