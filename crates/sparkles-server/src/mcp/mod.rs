//! Model Context Protocol server: tools that let LLM agents discover datasets, learn
//! their schema and query them with bounded, compact results. `sparkles mcp` serves it
//! over stdio, and `sparkles serve --mcp` over Streamable HTTP at `/$/mcp`.
//!
//! The tool logic here is transport-neutral ([`McpServer::call`]); `adapter.rs` and
//! `http.rs` are the only files that know the MCP SDK. Every call runs as a
//! [`Principal`] and sees only the datasets it may read. Every call runs under the
//! engine's budgets (timeout, memory, intermediate rows), reads exactly one snapshot,
//! and names the commit it read so later calls can pin it with `atCommit`. SERVICE is
//! off unless allowed, and the default tool set cannot write.

mod adapter;
pub(crate) mod branches;
#[cfg(feature = "auth")]
mod bridge;
mod complete;
mod context;
mod draft;
mod errors;
#[cfg(feature = "fmt")]
mod format;
#[cfg(feature = "graphql")]
mod graphql;
#[cfg(feature = "graphql")]
use graphql::graphql_on;

/// Without GraphQL no dataset has a GraphQL API.
#[cfg(not(feature = "graphql"))]
fn graphql_on(_: &Principal, _: &Dataset) -> bool {
    false
}
mod history;
pub mod http;
mod memory;
mod notify;
mod paths;
mod pins;
mod render;
mod schema_history;
mod schemas;
mod search;
mod stored;
mod tasks;
mod tools;
mod update;
#[cfg(any(feature = "shacl", feature = "shex"))]
mod validate;

#[cfg(test)]
mod tests;

use crate::auth::{Level, Principal};
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
    #[arg(long, value_name = "[NAME=]PATH", conflicts_with = "data")]
    #[cfg_attr(feature = "auth", arg(required_unless_present_any = ["data", "url"]))]
    #[cfg_attr(not(feature = "auth"), arg(required_unless_present = "data"))]
    pub loc: Vec<String>,
    /// RDF files loaded into one in-memory dataset
    #[arg(long, value_name = "FILE", num_args = 1..)]
    pub data: Vec<PathBuf>,
    /// Bridge stdio to the MCP endpoint (/$/mcp) of a running `sparkles serve --mcp` at
    /// this URL instead of opening databases. The server's tools, limits and permissions
    /// apply, and the bridge signs in with --token, SPARKLES_TOKEN or the saved
    /// `sparkles auth login` of that server
    #[cfg(feature = "auth")]
    #[arg(long, value_name = "URL", conflicts_with_all = ["loc", "data"])]
    pub url: Option<String>,
    /// The API token the bridge sends (instead of SPARKLES_TOKEN or the saved login)
    #[cfg(feature = "auth")]
    #[arg(long, value_name = "TOKEN", requires = "url")]
    pub token: Option<String>,
    /// Allow --url over plain http to a host other than localhost (the token travels in
    /// clear text)
    #[cfg(feature = "auth")]
    #[arg(long, requires = "url")]
    pub insecure_http: bool,
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
    /// Offer the sparql_update tool, which writes to the datasets (without it the process
    /// never writes)
    #[arg(long)]
    pub allow_update: bool,
    #[command(flatten)]
    pub outbound: crate::outbound::OutboundArgs,
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
    /// Budget for the rows all the operators of a call's query produce together (0:
    /// unlimited)
    #[arg(long, value_name = "N", default_value_t = 0)]
    pub max_rows_produced: u64,
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
    /// Do not offer the datasets' stored queries as tools
    #[arg(long)]
    pub no_stored_queries: bool,
    /// Largest number of classes, and of predicates, a schema report may have
    #[arg(long, value_name = "N", default_value_t = sparkles::schema::DEFAULT_MAX_ENTRIES)]
    pub schema_max_entries: usize,
    /// How long a tool call of a client that supports tasks runs before it becomes a
    /// task that the client polls, in milliseconds
    #[arg(long, value_name = "MS", default_value_t = 2000)]
    pub task_after_ms: u64,
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
    /// offer sparql_update (never on a read-only server)
    pub allow_update: bool,
    pub max_concurrent: usize,
    /// tools removed with `--disable-tool`
    pub disabled: BTreeSet<String>,
    /// the dataset names (`*` patterns) the tools may see; empty: all
    pub datasets: Vec<String>,
    /// offer the stored queries of the datasets as tools (C16)
    pub stored_queries: bool,
    /// how often a subscription looks for changed tool and resource lists and resources
    pub watch_interval: Duration,
    /// how long a tool call of a client that supports tasks may run before it becomes a
    /// task that the client polls
    pub task_after: Duration,
    /// delete scratch branches idle for longer than this (`None`: never)
    pub scratch_ttl: Option<Duration>,
}

impl Default for McpConfig {
    fn default() -> McpConfig {
        McpConfig {
            max_rows: 1000,
            max_bytes: 1 << 20,
            max_timeout: Duration::from_secs(60),
            query_memory_bytes: Some(2 << 30),
            allow_service: false,
            allow_update: false,
            max_concurrent: 4,
            disabled: BTreeSet::new(),
            datasets: Vec::new(),
            stored_queries: true,
            watch_interval: Duration::from_secs(2),
            task_after: Duration::from_secs(2),
            scratch_ttl: None,
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
    /// tool calls that became tasks (the tasks extension), with their callers
    pub tasks: tasks::Tasks,
    /// one permit per open `subscriptions/listen` stream or legacy session watcher
    pub subscriptions: Arc<Semaphore>,
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
    /// who calls: the HTTP request's principal, or the local principal on stdio
    pub principal: Principal,
    /// the HTTP request's headers (`Sparkles-Commit-Message`); `None` on stdio
    pub headers: Option<axum::http::HeaderMap>,
    /// the HTTP request's share of its rate-limit permits, held (never read) until the
    /// work ends
    #[allow(dead_code)]
    pub held: Option<http::HeldShare>,
}

impl McpServer {
    pub fn new(state: Arc<AppState>, cfg: McpConfig) -> McpServer {
        let read_only = state.read_only;
        let tools = schemas::tools(&cfg)
            .into_iter()
            .filter(|t| !cfg.disabled.contains(t.name))
            // a read-only server never offers the write tools
            .filter(|t| !(read_only && branches::WRITE_TOOLS.contains(&t.name)))
            .collect();
        McpServer {
            state,
            shared: Arc::new(Shared {
                slots: Arc::new(Semaphore::new(cfg.max_concurrent.max(1))),
                pins: pins::Pins::default(),
                tools,
                tasks: tasks::Tasks::default(),
                subscriptions: Arc::new(Semaphore::new(notify::MAX_SUBSCRIPTIONS)),
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

    /// Whether `p` may call `sparql_update`: the server offers it and `p` may write to a
    /// dataset it can see. Tool listings leave the tool out otherwise.
    pub fn may_update(&self, p: &Principal) -> bool {
        self.offers("sparql_update")
            && self
                .visible(p)
                .iter()
                .any(|ds| p.can(&ds.name, Level::Write))
    }

    /// Whether `p` may write to some branch of a dataset it can see, which the other
    /// write tools (`assert_facts` and the branch tools) need to be listed.
    pub fn may_write_somewhere(&self, p: &Principal) -> bool {
        self.state.datasets().values().any(|ds| {
            self.exposed(&ds.name)
                && p.level_any_branch(&ds.name)
                    .is_some_and(|l| l >= Level::Write)
        })
    }

    /// Whether `p` may call the offered tool `name`: the write tool needs a dataset
    /// `p` may write to, and `graphql_query` one with a GraphQL schema that `p` may
    /// query. Tool listings leave out the others.
    pub fn allows(&self, p: &Principal, name: &str) -> bool {
        match name {
            "sparql_update" => self.may_update(p),
            n if branches::WRITE_TOOLS.contains(&n) => {
                self.offers(n) && self.may_write_somewhere(p)
            }
            #[cfg(feature = "graphql")]
            "graphql_query" => self.graphql_available(p),
            _ => true,
        }
    }

    /// The offered tools `p` may call, in `tools/list` order.
    pub fn tools_for(&self, p: &Principal) -> Vec<&schemas::ToolDef> {
        self.shared
            .tools
            .iter()
            .filter(|t| self.allows(p, t.name))
            .collect()
    }

    /// Whether MCP may show the dataset `name` at all (`serve --mcp-dataset`).
    fn exposed(&self, name: &str) -> bool {
        let patterns = &self.shared.cfg.datasets;
        patterns.is_empty() || patterns.iter().any(|p| crate::auth::glob(p, name))
    }

    /// The datasets `p` may read through MCP, in name order.
    pub fn visible(&self, p: &Principal) -> Vec<Arc<Dataset>> {
        self.state
            .datasets()
            .values()
            .filter(|ds| self.exposed(&ds.name) && p.can(&ds.name, Level::Read))
            .cloned()
            .collect()
    }

    /// Run one tool call: wait for a slot, then run the tool on a blocking thread.
    pub async fn call(
        &self,
        name: &str,
        args: Map<String, Value>,
        call: Call,
    ) -> Result<Result<Outcome, ToolError>, UnknownTool> {
        if !self.known(&call.principal, name) {
            return Err(UnknownTool(name.to_string()));
        }
        Ok(self.run(name, args, call).await)
    }

    /// Whether `p` may call the tool `name`: an offered tool (the write tool checks for
    /// itself which datasets `p` may write to) or one of its stored queries.
    pub fn known(&self, p: &Principal, name: &str) -> bool {
        (self.offers(name) && (branches::WRITE_TOOLS.contains(&name) || self.allows(p, name)))
            || self.stored_tool(p, name).is_some()
    }

    /// [`McpServer::call`] for a tool whether or not it is offered (resources are read
    /// through the tools).
    async fn run(
        &self,
        name: &str,
        args: Map<String, Value>,
        call: Call,
    ) -> Result<Outcome, ToolError> {
        let Ok(permit) = self.shared.slots.clone().acquire_owned().await else {
            return Err(ToolError::internal(&call.request_id));
        };
        let server = self.clone();
        let name = name.to_string();
        let request_id = call.request_id.clone();
        let joined = tokio::task::spawn_blocking(move || {
            // the slot, and the request's rate-limit permits in `call.held`, are held
            // until the work itself ends, even if the caller is gone
            let _permit = permit;
            tools::run(&server, &name, args, &call)
        })
        .await;
        joined.unwrap_or_else(|e| {
            tracing::error!(request_id, "MCP tool call panicked: {e}");
            Err(ToolError::internal(&request_id))
        })
    }

    /// The dataset a call of `p` names; it may be omitted when `p` sees exactly one. A
    /// dataset `p` may not read is reported like one that does not exist.
    pub fn dataset(&self, p: &Principal, name: Option<&str>) -> Result<Arc<Dataset>, ToolError> {
        let datasets = self.visible(p);
        let available = || {
            let names: Vec<&str> = datasets.iter().map(|d| d.name.as_str()).collect();
            format!("available datasets: {}", names.join(", "))
        };
        match name {
            None if datasets.len() == 1 => Ok(datasets[0].clone()),
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
                datasets
                    .iter()
                    .find(|d| d.name == n)
                    .cloned()
                    .ok_or_else(|| {
                        ToolError::new("unknown-dataset", 404, format!("no dataset '{n}'"))
                            .hint(available())
                    })
            }
        }
    }
}

/// `sparkles mcp`: serve the datasets over stdio until stdin closes.
pub fn run(args: McpArgs, store_opts: StoreOptions) -> Result<()> {
    #[cfg(feature = "auth")]
    if let Some(url) = &args.url {
        return bridge::run(url, args.token.clone(), args.insecure_http);
    }
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
    // without --allow-update the process never writes: no write tool is offered
    st.read_only = !args.allow_update;
    st.allow_service = args.allow_service;
    st.outbound = args.outbound.policy()?;
    st.schema_max_entries = args.schema_max_entries;
    let memory = (args.query_memory_mb > 0).then_some(args.query_memory_mb << 20);
    st.limits = crate::state::Limits {
        query_memory_bytes: memory,
        max_result_bytes: None,
        max_rows: args.max_rows,
        max_rows_produced: (args.max_rows_produced > 0).then_some(args.max_rows_produced),
        update_timeout: None,
        ..Default::default()
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
        allow_update: args.allow_update,
        max_concurrent: args.max_concurrent,
        disabled: args.disable_tool.into_iter().collect(),
        datasets: Vec::new(),
        stored_queries: !args.no_stored_queries,
        task_after: Duration::from_millis(args.task_after_ms),
        ..McpConfig::default()
    };
    let server = McpServer::new(st, cfg);
    let rt = tokio::runtime::Builder::new_multi_thread()
        .thread_stack_size(crate::THREAD_STACK)
        .enable_all()
        .build()?;
    rt.block_on(adapter::serve_stdio(server))
}
