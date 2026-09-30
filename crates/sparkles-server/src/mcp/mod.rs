//! Model Context Protocol server: tools that let LLM agents discover datasets, learn
//! their schema and query them with bounded, compact results (`sparkles mcp`, stdio).
//!
//! The tool logic here is transport-neutral ([`McpServer::call`]); `adapter.rs` is the
//! only file that knows the MCP SDK. Every call runs under the engine's budgets
//! (timeout, memory, intermediate rows), reads exactly one snapshot, and names the
//! commit it read so later calls can pin it with `atCommit`. SERVICE is off unless
//! allowed, and the default tool set cannot write.

mod adapter;
mod errors;
mod pins;
mod render;
mod schemas;
mod search;
mod tools;

#[cfg(test)]
mod tests;

use crate::state::{AppState, Dataset, DbType};
use anyhow::{Context, Result, bail};
use errors::ToolError;
use serde_json::{Map, Value};
use sparkles::io::Source;
use sparkles::store::StoreOptions;
use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};
use tokio::sync::Semaphore;

/// `sparkles mcp` arguments.
#[derive(clap::Args, Debug)]
pub struct McpArgs {
    /// Database directory to serve, as [NAME=]PATH (repeatable; the name defaults to the
    /// directory's name). A database held by `sparkles serve` is refused: use that
    /// server instead.
    #[arg(
        long,
        value_name = "[NAME=]PATH",
        required_unless_present = "data",
        conflicts_with = "data"
    )]
    pub loc: Vec<String>,
    /// RDF files loaded into one in-memory dataset
    #[arg(long, value_name = "FILE", num_args = 1..)]
    pub data: Vec<PathBuf>,
    /// Name of the in-memory dataset of --data
    #[arg(long, default_value = "data")]
    pub name: String,
    /// Index the --data dataset for full-text search (`search_text`). Databases given
    /// with --loc keep the index they have (`sparkles text-index`).
    #[cfg(feature = "text")]
    #[arg(long, conflicts_with = "loc")]
    pub text: bool,
    /// Allow federated SERVICE calls in queries (off: a prompt-injected model could send
    /// data anywhere)
    #[arg(long)]
    pub allow_service: bool,
    /// Largest query timeout a call may request, in seconds (calls default to 30)
    #[arg(long, value_name = "SECS", default_value_t = 60.0)]
    pub timeout: f64,
    /// Budget for the estimated memory of a call's intermediate results, in MiB (0:
    /// unlimited)
    #[arg(long, value_name = "N", default_value_t = 2048)]
    pub query_memory_mb: u64,
    /// Maximum number of rows of any intermediate result
    #[arg(long, value_name = "N", default_value_t = 200_000_000)]
    pub max_rows: usize,
    /// Largest `maxRows` a sparql_query call may request
    #[arg(long, value_name = "N", default_value_t = 1000)]
    pub mcp_max_rows: usize,
    /// Largest `maxBytes` a sparql_query call may request
    #[arg(long, value_name = "N", default_value_t = 1 << 20)]
    pub mcp_max_bytes: usize,
    /// Tool calls that run at once; further calls wait for a slot
    #[arg(long, value_name = "N", default_value_t = 4)]
    pub max_concurrent: usize,
    /// Do not offer this tool (repeatable)
    #[arg(long, value_name = "NAME")]
    pub disable_tool: Vec<String>,
    /// Largest number of classes, and of predicates, a schema report may have
    #[arg(long, value_name = "N", default_value_t = sparkles::schema::DEFAULT_MAX_ENTRIES)]
    pub schema_max_entries: usize,
}

/// Limits and switches of the MCP tools.
#[derive(Clone, Debug)]
pub struct McpConfig {
    /// largest `maxRows` of sparql_query
    pub max_rows: usize,
    /// largest `maxBytes` of sparql_query
    pub max_bytes: usize,
    /// largest `timeoutSeconds`
    pub max_timeout: Duration,
    /// memory budget of every call's queries (`None`: unlimited)
    pub query_memory_bytes: Option<u64>,
    pub allow_service: bool,
    pub max_concurrent: usize,
    /// tools removed with `--disable-tool`
    pub disabled: BTreeSet<String>,
}

impl Default for McpConfig {
    fn default() -> McpConfig {
        McpConfig {
            max_rows: 1000,
            max_bytes: 1 << 20,
            max_timeout: Duration::from_secs(60),
            query_memory_bytes: Some(2 << 30),
            allow_service: false,
            max_concurrent: 4,
            disabled: BTreeSet::new(),
        }
    }
}

/// A JSON number without a trailing `.0` for whole values.
pub(crate) fn number(x: f64) -> Value {
    if x.fract() == 0.0 && (0.0..9e15).contains(&x) {
        Value::from(x as u64)
    } else {
        Value::from(x)
    }
}

impl McpConfig {
    pub fn max_timeout_secs(&self) -> Value {
        number(self.max_timeout.as_secs_f64())
    }

    /// The timeout of calls that do not set one: 30 s, or the maximum if lower.
    pub fn default_timeout(&self) -> Duration {
        self.max_timeout.min(Duration::from_secs(30))
    }

    pub fn default_timeout_secs(&self) -> Value {
        number(self.default_timeout().as_secs_f64())
    }
}

/// State shared by every connection (and, later, every transport).
pub struct Shared {
    pub cfg: McpConfig,
    pub pins: pins::Pins,
    /// one permit per concurrent tool call
    pub slots: Arc<Semaphore>,
    tools: Vec<schemas::ToolDef>,
}

/// The transport-neutral MCP server.
#[derive(Clone)]
pub struct McpServer {
    pub state: Arc<AppState>,
    pub shared: Arc<Shared>,
}

/// A successful tool result.
pub enum Outcome {
    /// `structuredContent`, also sent as one text block of compact JSON
    Structured(Value),
    /// one text block and no `structuredContent` (sparql_query)
    Text(String),
}

/// The call named a tool this server does not offer (a protocol error).
#[derive(Debug)]
pub struct UnknownTool(pub String);

/// What a tool call knows about itself.
pub struct Call {
    /// when the call arrived: queued calls count against their own timeout
    pub arrived: Instant,
    /// set when the client cancels the call
    pub cancel: Arc<AtomicBool>,
    /// the JSON-RPC request id, for logs and internal errors
    pub request_id: String,
}

impl McpServer {
    pub fn new(state: Arc<AppState>, cfg: McpConfig) -> McpServer {
        let tools = schemas::tools(&cfg)
            .into_iter()
            .filter(|t| !cfg.disabled.contains(t.name))
            .collect();
        McpServer {
            state,
            shared: Arc::new(Shared {
                slots: Arc::new(Semaphore::new(cfg.max_concurrent.max(1))),
                pins: pins::Pins::default(),
                tools,
                cfg,
            }),
        }
    }

    pub fn cfg(&self) -> &McpConfig {
        &self.shared.cfg
    }

    /// The offered tools, in `tools/list` order.
    pub fn tools(&self) -> &[schemas::ToolDef] {
        &self.shared.tools
    }

    pub fn offers(&self, name: &str) -> bool {
        self.shared.tools.iter().any(|t| t.name == name)
    }

    /// Run one tool call: wait for a slot, then run the tool on a blocking thread.
    pub async fn call(
        &self,
        name: &str,
        args: Map<String, Value>,
        call: Call,
    ) -> Result<Result<Outcome, ToolError>, UnknownTool> {
        if !self.offers(name) {
            return Err(UnknownTool(name.to_string()));
        }
        let Ok(permit) = self.shared.slots.clone().acquire_owned().await else {
            return Ok(Err(ToolError::internal(&call.request_id)));
        };
        let server = self.clone();
        let name = name.to_string();
        let request_id = call.request_id.clone();
        let joined = tokio::task::spawn_blocking(move || {
            // the slot is held until the work itself ends, even if the caller is gone
            let _permit = permit;
            tools::run(&server, &name, args, &call)
        })
        .await;
        Ok(joined.unwrap_or_else(|e| {
            tracing::error!(request_id, "MCP tool call panicked: {e}");
            Err(ToolError::internal(&request_id))
        }))
    }

    /// The dataset a call names; may be omitted when there is exactly one.
    pub fn dataset(&self, name: Option<&str>) -> Result<Arc<Dataset>, ToolError> {
        let datasets = self.state.datasets.read();
        let available = || {
            let names: Vec<&str> = datasets.keys().map(String::as_str).collect();
            format!("available datasets: {}", names.join(", "))
        };
        match name {
            None if datasets.len() == 1 => Ok(datasets
                .values()
                .next()
                .cloned()
                .unwrap_or_else(|| unreachable!())),
            None => Err(ToolError::new(
                "unknown-dataset",
                404,
                if datasets.is_empty() {
                    "this server has no datasets".to_string()
                } else {
                    "dataset is required: this server has several datasets".to_string()
                },
            )
            .hint(available())),
            Some(n) => {
                if n.is_empty()
                    || !n
                        .bytes()
                        .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'.' | b'-'))
                {
                    return Err(ToolError::bad_argument(format!(
                        "invalid dataset name '{n}': use a name from list_datasets"
                    )));
                }
                datasets.get(n).cloned().ok_or_else(|| {
                    ToolError::new("unknown-dataset", 404, format!("no dataset '{n}'"))
                        .hint(available())
                })
            }
        }
    }
}

/// `sparkles mcp`: serve the datasets over stdio until stdin closes.
pub fn run(args: McpArgs, store_opts: StoreOptions) -> Result<()> {
    if !(args.timeout.is_finite() && args.timeout > 0.0) {
        bail!("--timeout expects a positive number of seconds");
    }
    if args.mcp_max_rows == 0 {
        bail!("--mcp-max-rows must be at least 1");
    }
    if args.mcp_max_bytes < 1024 {
        bail!("--mcp-max-bytes must be at least 1024");
    }
    if args.max_concurrent == 0 {
        bail!("--max-concurrent must be at least 1");
    }
    for t in &args.disable_tool {
        if !schemas::all_tools().contains(&t.as_str()) {
            bail!(
                "--disable-tool: unknown tool '{t}' (tools: {})",
                schemas::all_tools().join(", ")
            );
        }
    }
    let timeout = Duration::from_secs_f64(args.timeout);
    let mut st = AppState::standalone(store_opts, timeout);
    // the process never writes: no write tool is offered
    st.read_only = true;
    st.allow_service = args.allow_service;
    st.schema_max_entries = args.schema_max_entries;
    let memory = (args.query_memory_mb > 0).then_some(args.query_memory_mb << 20);
    st.limits = crate::state::Limits {
        query_memory_bytes: memory,
        max_result_bytes: None,
        max_rows: args.max_rows,
        update_timeout: None,
    };
    let st = Arc::new(st);
    if args.data.is_empty() {
        for l in &args.loc {
            let (name, path) = match l.split_once('=') {
                Some((n, p)) => (n.to_string(), PathBuf::from(p)),
                None => {
                    let p = PathBuf::from(l);
                    let name = std::path::absolute(&p)
                        .ok()
                        .and_then(|a| a.file_name().map(|f| f.to_string_lossy().into_owned()))
                        .with_context(|| {
                            format!("--loc {l}: cannot derive a dataset name; use NAME=PATH")
                        })?;
                    (name, p)
                }
            };
            if !path.is_dir() {
                bail!("--loc {l}: {} is not a database directory", path.display());
            }
            st.attach(&name, DbType::Persistent, Some(&path))?;
        }
    } else {
        let ds = st.attach(&args.name, DbType::Mem, None)?;
        let sources = args
            .data
            .iter()
            .map(|f| Source::from_path(f, None))
            .collect::<Result<Vec<_>, _>>()?;
        ds.store.load(&sources)?;
        #[cfg(feature = "text")]
        if args.text {
            ds.store
                .enable_text(sparkles::text::TextConfig::default())?;
        }
    }
    let cfg = McpConfig {
        max_rows: args.mcp_max_rows,
        max_bytes: args.mcp_max_bytes,
        max_timeout: timeout,
        query_memory_bytes: memory,
        allow_service: args.allow_service,
        max_concurrent: args.max_concurrent,
        disabled: args.disable_tool.into_iter().collect(),
    };
    let server = McpServer::new(st, cfg);
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    rt.block_on(adapter::serve_stdio(server))
}
