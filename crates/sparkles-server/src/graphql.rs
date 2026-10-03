//! GraphQL (C03) in the server and the CLI: what the write-time guard says about
//! non-null fields, drafts of mapping schemas, and `sparkles graphql`.

use crate::state::Validation;
use anyhow::{Context, Result, bail};
use serde_json::{Map, Value as J};
use sparkles::guard::config::DataGraphSel;
use sparkles::history::{At, HistoryOptions};
use sparkles::sparql::QueryOptions;
use sparkles::store::{Store, StoreOptions};
use sparkles_graphql::{Catalog, Change, Code, Config, Request, draft};
use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// The write-time guard's SHACL configuration and its shapes as one graph: its shapes
/// graphs at the head, and its shapes file or inline shapes.
#[cfg(feature = "shacl")]
pub fn guard(
    v: Option<Validation>,
    store: &Store,
) -> Option<(sparkles_shacl::guard::ValidationConfig, oxrdf::Graph)> {
    #[allow(irrefutable_let_patterns)]
    let Validation::Shacl(g) = v? else {
        return None;
    };
    let cfg = g.config().clone();
    let mut graph = oxrdf::Graph::new();
    let snap = store.snapshot();
    for name in cfg.shapes.graphs.iter().flatten() {
        let Ok(n) = oxrdf::NamedNode::new(name.as_str()) else {
            continue;
        };
        let q = format!("CONSTRUCT {{ ?s ?p ?o }} WHERE {{ GRAPH {n} {{ ?s ?p ?o }} }}");
        if let Ok(r) = sparkles::sparql::query(snap.clone(), &q, &QueryOptions::default()) {
            for t in &r.triples {
                graph.insert(t);
            }
        }
    }
    let text = match (&cfg.shapes.inline, &cfg.shapes.file, store.root()) {
        (Some(t), _, _) => Some(t.clone()),
        (None, Some(_), Some(root)) => {
            std::fs::read_to_string(root.join(sparkles::guard::config::SHACL_SHAPES_FILE)).ok()
        }
        _ => None,
    };
    if let Some(text) = text {
        let syntax = cfg
            .shapes
            .format
            .as_deref()
            .and_then(sparkles_shacl::ShapesSyntax::from_media_type)
            .unwrap_or_default();
        if let Ok(g) = sparkles_shacl::shapes::Shapes::read_graph(&text, syntax, None) {
            for t in &g {
                graph.insert(t);
            }
        }
    }
    Some((cfg, graph))
}

/// Whether the guard enforces its shapes on a schema's data graph (§3.3): `reject`
/// mode, a `strict` baseline and the same data graph.
#[cfg(feature = "shacl")]
pub fn enforces(g: &sparkles_shacl::guard::ValidationConfig, data_graph: &DataGraphSel) -> bool {
    g.mode == sparkles::guard::GuardMode::Reject
        && g.baseline.is_strict()
        && g.data_graph == *data_graph
}

/// The facts behind the non-null warnings: the `(class, path, inverse)` triples the
/// guard requires a value for, and the data that says which classes are superclasses.
pub struct Backing {
    required: Vec<(String, String, bool)>,
    snap: Arc<sparkles::store::Snapshot>,
}

impl Backing {
    pub fn of(v: Option<Validation>, store: &Store, data_graph: &DataGraphSel) -> Backing {
        #[cfg(feature = "shacl")]
        let required = match guard(v, store) {
            Some((cfg, graph)) if enforces(&cfg, data_graph) => {
                let (types, _) = draft::read_shapes(&graph);
                types
                    .iter()
                    .flat_map(|t| {
                        t.fields
                            .iter()
                            .filter(|f| f.min_one)
                            .map(|f| (t.class.clone(), f.path.clone(), f.inverse))
                    })
                    .collect()
            }
            _ => Vec::new(),
        };
        #[cfg(not(feature = "shacl"))]
        let required = {
            let _ = (v, data_graph);
            Vec::new()
        };
        Backing {
            required,
            snap: store.snapshot(),
        }
    }

    /// Whether a shape of the guard requires a value of `path` for the class or one of
    /// its superclasses.
    pub fn backs(&self, class: &str, path: &str, inverse: bool) -> bool {
        if self.required.is_empty() {
            return false;
        }
        let Ok(c) = oxrdf::NamedNode::new(class) else {
            return false;
        };
        let q = format!(
            "SELECT DISTINCT ?s WHERE {{ {c} <http://www.w3.org/2000/01/rdf-schema#subClassOf>* ?s }}"
        );
        let mut classes = vec![class.to_string()];
        if let Ok(r) = sparkles::sparql::query(self.snap.clone(), &q, &QueryOptions::default()) {
            for row in r.rows() {
                if let Some(oxrdf::Term::NamedNode(n)) = &row[0] {
                    classes.push(n.as_str().to_string());
                }
            }
        }
        self.required
            .iter()
            .any(|(k, p, i)| classes.contains(k) && p == path && *i == inverse)
    }
}

/// What a draft is made from (§3.5).
pub struct DraftRequest {
    /// `shapes` or `observed`; `None`: shapes when there is a shapes graph or a SHACL
    /// guard, else observed
    pub source: Option<String>,
    pub shapes_graph: Option<String>,
    pub graph: sparkles::schema::GraphSelection,
    pub support: f64,
    pub classes: Vec<String>,
    pub min_instances: u64,
    pub reasoning: bool,
    pub timeout: Duration,
    /// the caller's view through the `info` endpoint
    pub view: Option<Arc<sparkles::access::GraphAccess>>,
    pub max_entries: usize,
    pub inferred_graph: String,
}

fn invalid(msg: impl Into<String>) -> anyhow::Error {
    sparkles::Error::Invalid(msg.into()).into()
}

/// Draft a mapping schema of a dataset. Returns the draft and the commit it read.
pub fn draft(
    name: &str,
    store: &Store,
    v: Option<Validation>,
    data_graph: &DataGraphSel,
    r: DraftRequest,
) -> Result<(draft::Draft, u64)> {
    #[cfg(feature = "shacl")]
    let has_guard = v
        .as_ref()
        .is_some_and(|v| v.language() == sparkles::guard::GuardLanguage::Shacl);
    #[cfg(not(feature = "shacl"))]
    let has_guard = false;
    let source = match r.source.as_deref() {
        Some("shapes") => "shapes",
        Some("observed") => "observed",
        None if r.shapes_graph.is_some() || has_guard => "shapes",
        None => "observed",
        Some(s) => {
            return Err(invalid(format!(
                "source must be shapes or observed, not '{s}'"
            )));
        }
    };
    let qopts = QueryOptions {
        timeout: Some(r.timeout),
        graphs: r.view.clone(),
        ..Default::default()
    };
    let mut prefixes: Vec<(String, String)> =
        sparkles::io::standard_prefixes().into_iter().collect();
    prefixes.extend(store.prefixes());
    prefixes.sort();
    prefixes.dedup_by(|a, b| a.0 == b.0);
    let snap = store.snapshot();
    let commit = snap.commit;
    let from = if source == "shapes" {
        match &r.shapes_graph {
            Some(g) => format!("the SHACL shapes of the graph <{g}>"),
            None => "the SHACL shapes of the write-time guard".to_string(),
        }
    } else {
        "the shapes drafted from the data".to_string()
    };
    let header = format!(
        "A draft of a GraphQL mapping schema for /{name}, from {from} at commit {commit}.\nReview it, then install it with PUT /$/graphql/{name}."
    );
    let d = if source == "shapes" {
        let (graph, enforced) = match &r.shapes_graph {
            Some(g) => {
                let n = oxrdf::NamedNode::new(g.as_str())
                    .map_err(|e| invalid(format!("shapesGraph: {e}")))?;
                let q = format!("CONSTRUCT {{ ?s ?p ?o }} WHERE {{ GRAPH {n} {{ ?s ?p ?o }} }}");
                let res = sparkles::sparql::query(snap.clone(), &q, &qopts)?;
                let mut graph = oxrdf::Graph::new();
                for t in &res.triples {
                    graph.insert(t);
                }
                (graph, false)
            }
            None => {
                #[cfg(feature = "shacl")]
                {
                    match guard(v, store) {
                        Some((cfg, graph)) => {
                            let e = enforces(&cfg, data_graph);
                            (graph, e)
                        }
                        None => {
                            return Err(invalid(
                                "the dataset has no SHACL write-time validation: name a shapes graph, or draft from the data (source observed)",
                            ));
                        }
                    }
                }
                #[cfg(not(feature = "shacl"))]
                {
                    let _ = (v, data_graph);
                    return Err(invalid("name a shapes graph"));
                }
            }
        };
        let (types, skipped) = draft::read_shapes(&graph);
        draft::render(types, skipped, &prefixes, enforced, &header, "shapes")
    } else {
        if !(r.support > 0.0 && r.support <= 1.0) {
            return Err(invalid("support must be a number in (0, 1]"));
        }
        let o = sparkles::schema::draft::DraftOptions {
            schema: sparkles::schema::SchemaOptions {
                graph: r.graph,
                inferred_graph: Some(r.inferred_graph.clone()),
                include_inferred: r.reasoning,
                max_entries: r.max_entries,
                graphs: r.view,
                deadline: Some(Instant::now() + r.timeout),
                ..Default::default()
            },
            dataset: name.to_string(),
            support: r.support,
            classes: r.classes,
            min_instances: r.min_instances,
            max_in: sparkles::schema::draft::DEFAULT_MAX_IN,
            max_count: 1,
            closed: false,
            base: sparkles::schema::draft::default_base(name),
            prefixes: prefixes.clone(),
        };
        let shapes =
            sparkles::schema::draft_shapes(&snap, &o).map_err(|e| invalid(e.to_string()))?;
        let big = |class: &str, pred: &str| {
            sparkles::sparql::query(snap.clone(), &draft::big_integer_query(class, pred), &qopts)
                .is_ok_and(|r| r.boolean)
        };
        let types = draft::read_observed(&shapes, &big);
        draft::render(types, Vec::new(), &prefixes, false, &header, "observed")
    };
    Ok((d, commit))
}

// ------------------------------------------------------------------- serve flags ------

/// The ceilings of GraphQL requests (`serve`); a schema's `limits` may lower them.
#[derive(clap::Args, Clone, Debug)]
pub struct ServeArgs {
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

impl ServeArgs {
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
                let (d, commit) = draft(
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
                        reasoning: false,
                        timeout: Duration::from_secs(600),
                        view: None,
                        max_entries: sparkles::schema::DEFAULT_MAX_ENTRIES,
                        inferred_graph: crate::http::INFERRED_GRAPH.to_string(),
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
