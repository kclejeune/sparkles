//! The model store of the server and its local embedding models (spec F12).
//!
//! * `serve --models-dir DIR` (default `<data>/models`) names the store of
//!   [`sparkles_modelstore`]: snapshots under `DIR/<owner>/<name>/<revision>/`, each with
//!   a `sparkles-manifest.json`. The store may be read-only, such as a Nix store path.
//! * `serve --models-download on` lets the server download a pinned snapshot that a
//!   `local` provider names and the store lacks, through the outbound policy. The
//!   default `off` only reads what `sparkles models pull` or the operator put there.
//! * A `local` provider's models run in the process through `sparkles-embed` (cargo
//!   feature `embed-local`). [`LocalModels`] keeps one runtime per provider and model,
//!   created at first use, and replaces it when the model's settings change.
//! * [`ServerProviders`] resolves the `provider` of a vector index's `embedding`: a local
//!   model, or the embeddings endpoint and key of an `openai` or `ollama` provider.

use parking_lot::Mutex;
use serde_json::{Value, json};
use sparkles::outbound::OutboundPolicy;
use sparkles::vector::embed::client::CallError;
use sparkles::vector::embed::{Providers, Target};
use sparkles_modelstore::{HttpClient, HttpResponse, ModelStore, SnapshotId};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock, Weak};
use std::time::Duration;

use crate::models::{Kind, ModelsConfig};
use crate::state::AppState;

/// Whether the server may download snapshots.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, clap::ValueEnum)]
pub enum Download {
    /// only snapshots already in the store or in a model's `path`
    #[default]
    Off,
    /// pinned revisions a local provider names, through the outbound policy
    On,
}

/// `serve --models-dir` and `--models-download`.
#[derive(clap::Args, Clone, Debug, Default)]
pub struct ModelStoreArgs {
    /// The model store: downloaded snapshots under DIR/<owner>/<name>/<revision>/
    /// [default: <data>/models]. It may be read-only
    #[arg(long, value_name = "DIR", env = "SPARKLES_MODELS_DIR")]
    pub models_dir: Option<PathBuf>,
    /// Whether the server downloads a pinned snapshot that a local provider names and
    /// the store lacks, through the outbound policy (off: only snapshots already there)
    #[arg(
        long,
        value_enum,
        value_name = "MODE",
        default_value = "off",
        env = "SPARKLES_MODELS_DOWNLOAD"
    )]
    pub models_download: Download,
}

/// The outbound policy of model downloads: the server's destinations and private
/// address rules, with ceilings and timeouts that fit files of several gigabytes.
pub fn download_policy(base: &OutboundPolicy) -> OutboundPolicy {
    OutboundPolicy {
        max_response_bytes: 64 << 30,
        max_request_bytes: 64 << 30,
        timeout: Duration::from_secs(6 * 3600),
        request_timeout: Duration::from_secs(6 * 3600),
        ..base.clone()
    }
}

/// The store's HTTP client on an outbound policy: every request and every redirect hop
/// is checked against it.
pub struct OutboundClient(pub OutboundPolicy);

impl HttpClient for OutboundClient {
    fn get(&self, url: &str, headers: &[(&str, &str)]) -> Result<HttpResponse, String> {
        let (status, body) = sparkles::outbound::get_stream(&self.0, url, headers, self.0.timeout)
            .map_err(|f| f.to_string())?;
        Ok(HttpResponse { status, body })
    }
}

/// The resolved settings of one local model, compared to notice a change.
#[derive(Clone, Debug, PartialEq)]
struct LocalSpec {
    source: Source,
    dtype: String,
    threads: usize,
    idle_unload_secs: u64,
    dimensions: Option<usize>,
    max_tokens: Option<usize>,
    query_prefix: Option<String>,
    document_prefix: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
enum Source {
    Store(SnapshotId),
    Path(PathBuf),
}

/// The default threads and idle time of a local model.
pub const DEFAULT_THREADS: usize = 2;
pub const DEFAULT_IDLE_UNLOAD_SECS: u64 = 600;

fn local_spec(cfg: &ModelsConfig, provider: &str, model: &str) -> Result<LocalSpec, String> {
    let p = cfg
        .providers
        .get(provider)
        .ok_or_else(|| format!("no provider named {provider:?} in the model configuration"))?;
    if p.kind != Kind::Local {
        return Err(format!("provider {provider} is not of the local kind"));
    }
    let o = p.models.get(model).ok_or_else(|| {
        format!(
            "provider {provider} defines no model {model:?} (a local provider lists its models)"
        )
    })?;
    // the provider's members are the defaults of its models
    let d = &p.defaults;
    let pick = |a: &Option<String>, b: &Option<String>| a.clone().or_else(|| b.clone());
    let source = match (
        pick(&o.path, &d.path),
        pick(&o.repo, &d.repo),
        pick(&o.revision, &d.revision),
    ) {
        (Some(path), _, _) => Source::Path(PathBuf::from(path)),
        (None, Some(repo), Some(revision)) => Source::Store(SnapshotId { repo, revision }),
        _ => return Err(format!("model {model}: give repo and revision, or path")),
    };
    Ok(LocalSpec {
        source,
        dtype: pick(&o.dtype, &d.dtype).unwrap_or_else(|| "f32".into()),
        threads: o.threads.or(d.threads).unwrap_or(DEFAULT_THREADS),
        idle_unload_secs: o
            .idle_unload_secs
            .or(d.idle_unload_secs)
            .unwrap_or(DEFAULT_IDLE_UNLOAD_SECS),
        dimensions: o.dimensions.or(d.dimensions),
        max_tokens: o.max_tokens.or(d.max_tokens),
        query_prefix: pick(&o.query_prefix, &d.query_prefix),
        document_prefix: pick(&o.document_prefix, &d.document_prefix),
    })
}

#[cfg(feature = "embed-local")]
struct Running {
    spec: LocalSpec,
    embedder: Arc<sparkles_embed::Embedder>,
}

/// The local models of the server.
pub struct LocalModels {
    store: ModelStore,
    #[cfg(feature = "embed-local")]
    download: Download,
    #[cfg(feature = "embed-local")]
    outbound: OutboundPolicy,
    #[cfg(feature = "embed-local")]
    running: Mutex<HashMap<(String, String), Running>>,
    /// snapshots being downloaded, and the last download error of each
    pulls: Mutex<(HashSet<SnapshotId>, HashMap<SnapshotId, String>)>,
}

impl LocalModels {
    pub fn new(dir: PathBuf, download: Download, outbound: OutboundPolicy) -> LocalModels {
        #[cfg(not(feature = "embed-local"))]
        let _ = (download, outbound);
        LocalModels {
            store: ModelStore::new(dir),
            #[cfg(feature = "embed-local")]
            download,
            #[cfg(feature = "embed-local")]
            outbound,
            #[cfg(feature = "embed-local")]
            running: Mutex::new(HashMap::new()),
            pulls: Mutex::new((HashSet::new(), HashMap::new())),
        }
    }

    pub fn dir(&self) -> &Path {
        self.store.root()
    }

    /// The snapshot directory of `spec`, or why there is none yet. A missing snapshot
    /// starts a download in the background when downloads are on.
    #[cfg(feature = "embed-local")]
    fn snapshot(self: &Arc<Self>, source: &Source) -> Result<PathBuf, CallError> {
        let id = match source {
            Source::Path(p) => {
                return if p.is_dir() {
                    Ok(p.clone())
                } else {
                    Err(CallError::Fatal(format!(
                        "the model directory {} does not exist",
                        p.display()
                    )))
                };
            }
            Source::Store(id) => id,
        };
        if let Some(s) = self
            .store
            .get(id)
            .map_err(|e| CallError::Fatal(e.to_string()))?
        {
            return Ok(s.dir);
        }
        let label = format!("{}@{}", id.repo, id.revision);
        if self.download == Download::Off {
            return Err(CallError::Fatal(format!(
                "the model {label} is not in the store {}; run `sparkles models pull {label} --dir {}`, or start the server with --models-download on",
                self.store.root().display(),
                self.store.root().display()
            )));
        }
        let mut pulls = self.pulls.lock();
        if !pulls.0.contains(id) {
            pulls.0.insert(id.clone());
            pulls.1.remove(id);
            let me = self.clone();
            let id = id.clone();
            std::thread::Builder::new()
                .name("sparkles-model-pull".into())
                .spawn(move || me.pull(id))
                .map_err(|e| CallError::Fatal(format!("cannot start a download: {e}")))?;
        }
        Err(CallError::Transient(
            format!("the model {label} is being downloaded"),
            Some(Duration::from_secs(30)),
        ))
    }

    #[cfg(feature = "embed-local")]
    fn pull(&self, id: SnapshotId) {
        let client = OutboundClient(download_policy(&self.outbound));
        let label = format!("{}@{}", id.repo, id.revision);
        tracing::info!(
            "downloading the model {label} into {}",
            self.store.root().display()
        );
        let r = sparkles_modelstore::HubSource::default()
            .plan(
                &client,
                &id.repo,
                &id.revision,
                false,
                &sparkles_modelstore::sentence_transformers_files,
            )
            .and_then(|plan| self.store.fetch(&client, &plan, &mut |_| {}));
        let mut pulls = self.pulls.lock();
        pulls.0.remove(&id);
        match r {
            Ok(s) => tracing::info!("downloaded the model {label} to {}", s.dir.display()),
            Err(e) => {
                tracing::error!("cannot download the model {label}: {e}");
                pulls.1.insert(id, e.to_string());
            }
        }
    }

    /// The runtime of a local model, created or replaced as its settings require.
    #[cfg(feature = "embed-local")]
    fn embedder(
        self: &Arc<Self>,
        cfg: &ModelsConfig,
        provider: &str,
        model: &str,
    ) -> Result<Arc<sparkles_embed::Embedder>, CallError> {
        let spec = local_spec(cfg, provider, model).map_err(CallError::Fatal)?;
        let key = (provider.to_string(), model.to_string());
        if let Some(r) = self.running.lock().get(&key)
            && r.spec == spec
        {
            return Ok(r.embedder.clone());
        }
        let dir = self.snapshot(&spec.source)?;
        let mut ms = sparkles_embed::ModelSpec::new(dir);
        ms.dtype = match spec.dtype.as_str() {
            "bf16" => sparkles_embed::Dtype::Bf16,
            _ => sparkles_embed::Dtype::F32,
        };
        ms.dimension = spec.dimensions;
        ms.max_tokens = spec.max_tokens;
        ms.query_prompt = spec.query_prefix.clone();
        ms.document_prompt = spec.document_prefix.clone();
        let opts = sparkles_embed::Options {
            threads: spec.threads,
            idle_unload: (spec.idle_unload_secs > 0)
                .then(|| Duration::from_secs(spec.idle_unload_secs)),
            ..Default::default()
        };
        let e = Arc::new(
            sparkles_embed::Embedder::new(ms, opts)
                .map_err(|e| CallError::Fatal(format!("{provider}/{model}: {e}")))?,
        );
        self.running.lock().insert(
            key,
            Running {
                spec,
                embedder: e.clone(),
            },
        );
        Ok(e)
    }

    /// The local models of `cfg` with their state, for `GET /$/models`: `absent`,
    /// `downloading`, `present` (in the store, not started), `unloaded`, `loading` or
    /// `loaded`, with the weights' size while loaded.
    pub fn describe(&self, cfg: &ModelsConfig) -> Value {
        let mut out = Vec::new();
        for (pname, p) in &cfg.providers {
            if p.kind != Kind::Local {
                continue;
            }
            for model in p.models.keys() {
                let Ok(spec) = local_spec(cfg, pname, model) else {
                    continue;
                };
                let mut j = json!({
                    "provider": pname,
                    "model": model,
                    "dtype": spec.dtype,
                    "threads": spec.threads,
                    "idleUnloadSecs": spec.idle_unload_secs,
                    "runtime": cfg!(feature = "embed-local"),
                });
                let present = match &spec.source {
                    Source::Path(path) => {
                        j["path"] = path.display().to_string().into();
                        path.is_dir()
                    }
                    Source::Store(id) => {
                        j["repo"] = id.repo.clone().into();
                        j["revision"] = id.revision.clone().into();
                        matches!(self.store.get(id), Ok(Some(_)))
                    }
                };
                let mut state = if present { "present" } else { "absent" };
                if let Source::Store(id) = &spec.source {
                    let pulls = self.pulls.lock();
                    if pulls.0.contains(id) {
                        state = "downloading";
                    }
                    if let Some(e) = pulls.1.get(id) {
                        j["downloadError"] = e.clone().into();
                    }
                }
                #[cfg(feature = "embed-local")]
                if let Some(r) = self
                    .running
                    .lock()
                    .get(&(pname.clone(), model.clone()))
                    .filter(|r| r.spec == spec)
                {
                    let s = r.embedder.status();
                    let i = r.embedder.info();
                    state = match s.state {
                        sparkles_embed::State::Loaded => "loaded",
                        sparkles_embed::State::Loading => "loading",
                        sparkles_embed::State::Unloaded => "unloaded",
                    };
                    j["arch"] = i.arch.into();
                    j["dimension"] = i.dimension.into();
                    j["maxTokens"] = i.max_tokens.into();
                    j["weightBytes"] = s.weight_bytes.into();
                    j["loads"] = s.loads.into();
                    j["unloads"] = s.unloads.into();
                    j["texts"] = s.texts.into();
                    j["queued"] = (s.queued_queries + s.queued_documents).into();
                    if let Some(e) = s.last_error {
                        j["lastError"] = e.into();
                    }
                }
                j["state"] = state.into();
                out.push(j);
            }
        }
        Value::Array(out)
    }

    /// The state of one local model, for the vector index card.
    pub fn describe_one(&self, cfg: &ModelsConfig, provider: &str, model: &str) -> Option<Value> {
        match self.describe(cfg) {
            Value::Array(a) => a
                .into_iter()
                .find(|j| j["provider"] == provider && j["model"] == model),
            _ => None,
        }
    }
}

#[cfg(feature = "embed-local")]
struct Adapter(Arc<sparkles_embed::Embedder>);

#[cfg(feature = "embed-local")]
impl sparkles::vector::embed::LocalModel for Adapter {
    fn embed(
        &self,
        inputs: &[String],
        query: bool,
        dimension: Option<usize>,
    ) -> Result<Vec<Vec<f32>>, CallError> {
        let refs: Vec<&str> = inputs.iter().map(String::as_str).collect();
        let kind = if query {
            sparkles_embed::Kind::Query
        } else {
            sparkles_embed::Kind::Document
        };
        let mut vs = self.0.embed(&refs, kind).map_err(|e| match e {
            sparkles_embed::Error::Busy => {
                CallError::Transient(e.to_string(), Some(Duration::from_secs(1)))
            }
            e => CallError::Fatal(e.to_string()),
        })?;
        if let Some(d) = dimension {
            let renorm = self.0.info().normalize;
            for v in &mut vs {
                sparkles_embed::pooling::truncate(v, d, renorm);
            }
        }
        Ok(vs)
    }
}

/// The providers of the server's model configuration, for vector indexes (spec F12 §6).
pub struct ServerProviders {
    state: OnceLock<Weak<AppState>>,
    pub local: Arc<LocalModels>,
}

impl std::fmt::Debug for ServerProviders {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ServerProviders")
            .field("models_dir", &self.local.dir())
            .finish_non_exhaustive()
    }
}

impl ServerProviders {
    pub fn new(local: Arc<LocalModels>) -> ServerProviders {
        ServerProviders {
            state: OnceLock::new(),
            local,
        }
    }

    /// Give the providers the server's state, once it is shared.
    pub fn attach(&self, st: &Arc<AppState>) {
        let _ = self.state.set(Arc::downgrade(st));
    }
}

impl Providers for ServerProviders {
    fn resolve(&self, provider: &str, model: &str) -> Result<Target, CallError> {
        let models = self
            .state
            .get()
            .and_then(Weak::upgrade)
            .and_then(|st| st.models())
            .ok_or_else(|| {
                CallError::Fatal(format!(
                    "the server has no model configuration, so the provider {provider:?} is unknown"
                ))
            })?;
        let (kind, url, key) = models
            .embedding_endpoint(provider, model)
            .map_err(CallError::Fatal)?;
        if kind != Kind::Local {
            return Ok(Target::Remote { url, bearer: key });
        }
        #[cfg(feature = "embed-local")]
        {
            let e = self.local.embedder(&models.config, provider, model)?;
            Ok(Target::Local(Arc::new(Adapter(e))))
        }
        #[cfg(not(feature = "embed-local"))]
        {
            let _ = local_spec(&models.config, provider, model).map_err(CallError::Fatal)?;
            Err(CallError::Fatal(
                "this build has no local embedding runtime (cargo feature embed-local)".into(),
            ))
        }
    }
}
