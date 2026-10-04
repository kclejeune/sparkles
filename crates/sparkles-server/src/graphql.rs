//! GraphQL (C03) in the server and the CLI: the server's ceilings and `sparkles
//! graphql`. The library's [`sparkles::handles::GraphQl`] installs, drafts and runs
//! mapping schemas.

use anyhow::{Context, Result, bail};
use serde_json::{Map, Value as J};
use sparkles::handles::graphql::{Backing, DraftRequest, draft_of};
use sparkles::history::{At, HistoryOptions};
use sparkles::sparql::QueryOptions;
use sparkles::store::{Store, StoreOptions};
use sparkles_graphql::{Catalog, Change, Code, Config, Request};
use std::io::{Read, Write};
use std::path::PathBuf;
use std::time::Duration;

// ------------------------------------------------------------------- serve flags ------

/// The ceilings of GraphQL requests (`serve`); a schema's `limits` may lower them.
#[derive(clap::Args, Clone, Debug)]
pub struct GraphqlServeArgs {
    /// Deepest selection of a GraphQL request, outside introspection
    #[arg(long, default_value_t = 12)]
    graphql_max_depth: u32,
    /// Most nodes a GraphQL response may hold (estimated before it runs, counted while it
    /// runs)
    #[arg(long, default_value_t = 100_000)]
    graphql_max_nodes: u64,
    /// Page size of GraphQL lists without first or last
    #[arg(long, default_value_t = 100)]
    graphql_default_first: u32,
    /// Largest first or last of a GraphQL list
    #[arg(long, default_value_t = 1000)]
    graphql_max_first: u32,
}

impl GraphqlServeArgs {
    pub fn limits(&self) -> sparkles_graphql::plan::Limits {
        sparkles_graphql::plan::Limits {
            max_depth: self.graphql_max_depth,
            max_nodes: self.graphql_max_nodes,
            default_first: self.graphql_default_first.min(self.graphql_max_first),
            max_first: self.graphql_max_first,
            ..Default::default()
        }
    }
}

// ------------------------------------------------------------------------ CLI ------

#[derive(clap::Args)]
pub struct GraphqlArgs {
    /// Database directory
    #[arg(long)]
    loc: PathBuf,
    #[command(subcommand)]
    cmd: GraphqlCmd,
}

#[derive(clap::Subcommand)]
enum GraphqlCmd {
    /// Run a GraphQL document and print the JSON response (status 2 on a budget, 1 on
    /// any other error)
    Run {
        /// File with the document (standard input without it)
        #[arg(long)]
        query: Option<PathBuf>,
        /// The variables as a JSON object
        #[arg(long)]
        variables: Option<String>,
        /// The operation to run, when the document has several
        #[arg(long)]
        operation: Option<String>,
        /// Read a past state: N, commit:N, time:<RFC 3339>, snapshot:NAME
        #[arg(long)]
        at: Option<String>,
        /// Add each fetch group's SPARQL, rows and time to the response
        #[arg(long)]
        explain: bool,
        #[arg(long)]
        timeout: Option<f64>,
    },
    /// The installed schema: print, install, delete or draft one
    Schema {
        #[command(subcommand)]
        cmd: SchemaCmd,
    },
}

#[derive(clap::Subcommand)]
enum SchemaCmd {
    /// Print the mapping schema (the SDL), or with --api the API schema clients see
    Get {
        #[arg(long)]
        api: bool,
        /// Print the configuration with its version as JSON
        #[arg(long, conflicts_with = "api")]
        json: bool,
    },
    /// Install a mapping schema: an SDL file, or a JSON configuration (a .json file)
    Put {
        file: PathBuf,
        /// A message recorded with the version
        #[arg(long)]
        message: Option<String>,
        /// Accepted for the stored GraphQL queries of a later version; nothing depends
        /// on the schema yet
        #[arg(long)]
        force: bool,
    },
    /// Remove the configuration and its versions
    Delete,
    /// List the kept versions
    Versions,
    /// Draft a mapping schema from SHACL shapes or from the data
    Draft {
        /// shapes (the write-time guard's, or --shapes-graph) or observed (the data)
        #[arg(long)]
        source: Option<String>,
        /// observed: the share of instances a constraint must hold for
        #[arg(long, default_value_t = 1.0)]
        support: f64,
        /// shapes: a named graph of SHACL shapes
        #[arg(long)]
        shapes_graph: Option<String>,
        /// observed: default, union or a graph IRI
        #[arg(long)]
        graph: Option<String>,
        /// observed: only these classes (repeatable)
        #[arg(long)]
        class: Vec<String>,
        /// observed: skip classes with fewer instances
        #[arg(long, default_value_t = 1)]
        min_instances: u64,
        /// Print the decisions as JSON instead of the SDL
        #[arg(long)]
        json: bool,
    },
}

fn open_catalog(loc: &std::path::Path) -> Result<Catalog> {
    if !loc.is_dir() {
        bail!("{} is not a database directory", loc.display());
    }
    Ok(Catalog::open(Some(loc))?)
}

pub fn run(args: GraphqlArgs, opts: StoreOptions) -> Result<()> {
    let loc = args.loc;
    let mut out = std::io::stdout().lock();
    match args.cmd {
        GraphqlCmd::Run {
            query,
            variables,
            operation,
            at,
            explain,
            timeout,
        } => {
            let catalog = open_catalog(&loc)?;
            let c = catalog
                .compiled()?
                .context("no GraphQL schema is installed: draft one with `sparkles graphql --loc DB schema draft`")?;
            let text = match query {
                Some(f) => std::fs::read_to_string(&f)
                    .with_context(|| format!("reading {}", f.display()))?,
                None => {
                    let mut s = String::new();
                    std::io::stdin().read_to_string(&mut s)?;
                    s
                }
            };
            let variables: Map<String, J> = match variables {
                None => Map::new(),
                Some(v) => match serde_json::from_str(&v).context("--variables is not JSON")? {
                    J::Object(o) => o,
                    _ => bail!("--variables must be a JSON object"),
                },
            };
            let store = Store::open(&loc, opts)?;
            let at: Option<At> = at.map(|a| a.parse()).transpose()?;
            let o = sparkles_graphql::Options {
                query: QueryOptions {
                    timeout: timeout.map(Duration::from_secs_f64),
                    ..Default::default()
                },
                explain,
                at,
                ..Default::default()
            };
            let resolve = |a: Option<&At>| match a {
                None => Ok(store.snapshot()),
                Some(a) => Ok(store.snapshot_at(a, &HistoryOptions::default())?.0),
            };
            let r = sparkles_graphql::execute(
                &c,
                &Request {
                    query: text,
                    operation_name: operation,
                    variables,
                },
                &o,
                &resolve,
            );
            serde_json::to_writer_pretty(&mut out, &r.body)?;
            writeln!(out)?;
            out.flush()?;
            let codes: Vec<&str> = r.body["errors"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|e| e["extensions"]["code"].as_str())
                .collect();
            if codes.contains(&Code::BudgetExceeded.as_str()) {
                std::process::exit(2);
            }
            if !codes.is_empty() {
                std::process::exit(1);
            }
        }
        GraphqlCmd::Schema { cmd } => match cmd {
            SchemaCmd::Get { api, json } => {
                let catalog = open_catalog(&loc)?;
                let s = catalog
                    .get(None)
                    .context("no GraphQL schema is installed")?;
                if json {
                    serde_json::to_writer_pretty(&mut out, &s)?;
                    writeln!(out)?;
                } else if api {
                    let c = catalog
                        .compiled()?
                        .context("no GraphQL schema is installed")?;
                    write!(out, "{}", c.api_sdl)?;
                } else {
                    write!(out, "{}", s.config.sdl)?;
                    if !s.config.sdl.ends_with('\n') {
                        writeln!(out)?;
                    }
                }
            }
            SchemaCmd::Put {
                file,
                message,
                force: _,
            } => {
                let text = std::fs::read_to_string(&file)
                    .with_context(|| format!("reading {}", file.display()))?;
                // the store's lock keeps a server from changing the configuration meanwhile
                let store = Store::open(&loc, opts)?;
                let catalog = open_catalog(&loc)?;
                let config = if file.extension().is_some_and(|e| e == "json") {
                    serde_json::from_str::<Config>(&text).context("invalid configuration")?
                } else {
                    match catalog.get(None) {
                        Some(cur) => Config {
                            sdl: text,
                            ..cur.config
                        },
                        None => Config::new(text),
                    }
                };
                let v = crate::write_validation::install(&store).ok().flatten();
                let backing = Backing::of(v, &store, &config.data_graph);
                let backs = |c: &str, p: &str, i: bool| backing.backs(c, p, i);
                let change = Change {
                    author: std::env::var("USER").ok(),
                    message,
                    dataset_commit: Some(store.snapshot().commit),
                    if_version: None,
                };
                let (saved, warnings) =
                    catalog.put(config, change, &backs).map_err(|e| match e {
                        sparkles_graphql::PutError::Engine(e) => anyhow::Error::from(e),
                        e => anyhow::anyhow!("the mapping schema is invalid: {}", e.message()),
                    })?;
                for w in &warnings {
                    eprintln!("warning: {w}");
                }
                if saved.changed {
                    writeln!(out, "version {}", saved.stored.version.version)?;
                } else {
                    writeln!(out, "unchanged at version {}", saved.stored.version.version)?;
                }
            }
            SchemaCmd::Delete => {
                let _store = Store::open(&loc, opts)?;
                if !open_catalog(&loc)?.delete(None)? {
                    bail!("no GraphQL schema is installed");
                }
            }
            SchemaCmd::Versions => {
                for v in open_catalog(&loc)?.versions() {
                    writeln!(
                        out,
                        "v{}  {}  {}  {}",
                        v.version,
                        v.created,
                        v.author.as_deref().unwrap_or("-"),
                        v.message.as_deref().unwrap_or("")
                    )?;
                }
            }
            SchemaCmd::Draft {
                source,
                support,
                shapes_graph,
                graph,
                class,
                min_instances,
                json,
            } => {
                let store = Store::open(&loc, opts)?;
                let v = crate::write_validation::install(&store).ok().flatten();
                let data_graph = open_catalog(&loc)?
                    .get(None)
                    .map(|s| s.config.data_graph)
                    .unwrap_or_default();
                let graph = match graph {
                    Some(g) => sparkles::schema::GraphSelection::parse(&g)
                        .map_err(|e| anyhow::anyhow!("--graph: {e}"))?,
                    None => sparkles::schema::GraphSelection::Default,
                };
                let name = loc
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "data".into());
                let (d, commit) = draft_of(
                    &name,
                    &store,
                    v,
                    &data_graph,
                    DraftRequest {
                        source,
                        shapes_graph,
                        graph,
                        support,
                        classes: class,
                        min_instances,
                        ..Default::default()
                    },
                )?;
                if json {
                    let mut j = serde_json::to_value(&d)?;
                    j["commit"] = commit.into();
                    serde_json::to_writer_pretty(&mut out, &j)?;
                    writeln!(out)?;
                } else {
                    write!(out, "{}", d.sdl)?;
                }
            }
        },
    }
    out.flush()?;
    Ok(())
}
