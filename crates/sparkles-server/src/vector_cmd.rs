//! `sparkles vector`: the vector indexes of a database directory, through the dataset's
//! vector index handle, or of a dataset on a server: create, drop, rebuild, list,
//! status, and the embedding of a local database's text.

use anyhow::{Context, Result, bail};
use serde_json::Value as J;
use sparkles::handles::VectorIndexes;
use sparkles::store::StoreOptions;
use sparkles::vector::VectorIndexConfig;

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
            providers: None,
        })
    }

    /// Embed what is waiting in the indexes `vector`, reporting the status of `names`
    /// after.
    fn run(&self, vector: &VectorIndexes, names: &[String]) -> Result<()> {
        if !(self.embed_timeout.is_finite() && self.embed_timeout > 0.0) {
            bail!("--embed-timeout must be a positive number of seconds");
        }
        vector.set_embedding_environment(Some(self.environment()?));
        let t = std::time::Instant::now();
        let r = vector.embed_until_idle(std::time::Duration::from_secs_f64(self.embed_timeout));
        for n in names {
            if let Some(s) = vector.embedding_status(n) {
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

/// `sparkles vector …` on a local database (`--loc`).
fn run_local(loc: &std::path::Path, opts: StoreOptions, action: Action) -> Result<()> {
    let vector = sparkles::Dataset::open_with(loc, opts)?.indexes().vector();
    match action {
        Action::Create { name, config } => {
            let t = std::time::Instant::now();
            let created = vector.put(&name, config)?;
            let s = vector.wait(&name).context("the index was dropped")?;
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
            vector.drop(&name)?;
            eprintln!("vector index {name} dropped");
        }
        Action::Rebuild { name } => {
            let t = std::time::Instant::now();
            vector.rebuild(&name)?;
            let s = vector.wait(&name).context("the index was dropped")?;
            eprintln!(
                "vector index {name} rebuilt: {} rows, {} in {:.0} ms",
                s.rows,
                s.state,
                t.elapsed().as_secs_f64() * 1000.0
            );
        }
        Action::Status { name } => {
            // indexes are read from their files (or built) when the store opens
            let all = vector.wait_all();
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
        Action::List => print_list(&serde_json::to_value(vector.list())?),
        Action::Reembed { name, run } => {
            vector.reembed(&name)?;
            run.run(&vector, std::slice::from_ref(&name))?;
        }
        Action::Embed { name, run } => {
            let names: Vec<String> = match name {
                Some(n) => vec![n],
                None => vector
                    .list()
                    .into_iter()
                    .filter(|s| s.embedding.is_some())
                    .map(|s| s.name)
                    .collect(),
            };
            if names.is_empty() {
                bail!("no vector index computes embeddings");
            }
            run.run(&vector, &names)?;
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
            let what = if resp.status().as_u16() == reqwest::StatusCode::CREATED.as_u16() {
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
