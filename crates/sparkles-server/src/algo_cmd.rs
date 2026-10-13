//! `sparkles algo`: graph algorithms on a database directory (F14). `run` computes an
//! algorithm and prints its rows or summary, or writes the results back in batched
//! commits. `projections` manages the named projections of `algo.json`.

use anyhow::{Context, Result, bail};
use serde_json::{Map, Value};
use sparkles::algo::catalog::{Catalog, PredicateDef, ProjectionDef};
use sparkles::sparql::QueryOptions;
use sparkles::sparql::algorun::{AlgoRequest, ProjectionRef, WriteMode, WriteSpec};
use sparkles::sparql::results::{self, SolutionsFormat};
use sparkles::store::StoreOptions;
use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

#[derive(clap::Subcommand)]
pub enum AlgoCmd {
    /// Run an algorithm and print its rows, or write them back to a graph
    Run(RunArgs),
    /// The named projections of a database
    Projections {
        #[command(subcommand)]
        cmd: ProjectionsCmd,
    },
}

#[derive(clap::Args)]
pub struct RunArgs {
    #[arg(long)]
    loc: PathBuf,
    /// The algorithm: pageRank, betweenness, wcc, scc, labelPropagation, louvain, leiden,
    /// nodeSimilarity, triangleCount, clustering, kCore, bfs, sssp, fastRP or node2vec
    #[arg(required_unless_present = "request")]
    algorithm: Option<String>,
    /// The whole request as JSON (as `POST /$/algo/{ds}/run`), instead of the options below
    #[arg(long, conflicts_with_all = ["algorithm", "projection", "predicate", "direction", "param"])]
    request: Option<PathBuf>,
    /// A named projection
    #[arg(long, conflicts_with_all = ["predicate", "direction"])]
    projection: Option<String>,
    /// A predicate whose triples are edges (repeatable); every predicate but rdf:type by default
    #[arg(long)]
    predicate: Vec<String>,
    /// forward, backward or undirected
    #[arg(long)]
    direction: Option<String>,
    /// A parameter, NAME=VALUE (repeatable), such as maxIterations=30
    #[arg(long, value_name = "NAME=VALUE")]
    param: Vec<String>,
    /// Print the statistics of the run instead of its rows
    #[arg(long, conflicts_with = "write_graph")]
    summary: bool,
    /// Output format: text, json, xml, csv or tsv
    #[arg(long, default_value = "text")]
    format: String,
    #[arg(long)]
    timeout: Option<f64>,
    /// Write the results to this graph in batched commits
    #[arg(long, value_name = "IRI")]
    write_graph: Option<String>,
    /// An output and the predicate it is written with, OUTPUT=PREDICATE (repeatable)
    #[arg(long, value_name = "OUTPUT=PREDICATE", requires = "write_graph")]
    write: Vec<String>,
    /// replace (the default) or merge
    #[arg(long, default_value = "replace", requires = "write_graph")]
    mode: String,
    /// Triples in each commit
    #[arg(long, requires = "write_graph")]
    batch_size: Option<usize>,
    /// One commit for the whole write
    #[arg(long, requires = "write_graph")]
    atomic: bool,
}

#[derive(clap::Subcommand)]
pub enum ProjectionsCmd {
    /// List the named projections
    List {
        #[arg(long)]
        loc: PathBuf,
    },
    /// Print a definition with its revision
    Get {
        #[arg(long)]
        loc: PathBuf,
        name: String,
    },
    /// Create or replace a definition
    Put {
        #[arg(long)]
        loc: PathBuf,
        name: String,
        /// The definition as JSON (as `PUT /$/algo/{ds}/projections/{name}`)
        #[arg(long, conflicts_with_all = ["predicate", "direction", "node_class", "weight"])]
        json: Option<PathBuf>,
        /// A predicate whose triples are edges (repeatable)
        #[arg(long)]
        predicate: Vec<String>,
        /// forward, backward or undirected
        #[arg(long)]
        direction: Option<String>,
        /// A class whose instances are the nodes (repeatable)
        #[arg(long)]
        node_class: Vec<String>,
        /// The property of an edge's reifier that holds its weight
        #[arg(long)]
        weight: Option<String>,
    },
    /// Delete a definition
    Delete {
        #[arg(long)]
        loc: PathBuf,
        name: String,
    },
}

/// A parameter value: a JSON number, boolean or list when it reads as one, else a string.
fn param_value(v: &str) -> Value {
    serde_json::from_str::<Value>(v)
        .ok()
        .filter(|j| !j.is_object())
        .unwrap_or_else(|| Value::String(v.to_string()))
}

fn pairs(items: &[String], what: &str) -> Result<BTreeMap<String, String>> {
    items
        .iter()
        .map(|kv| {
            kv.split_once('=')
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .with_context(|| format!("--{what} {kv}: expected NAME=VALUE"))
        })
        .collect()
}

fn request(a: &RunArgs) -> Result<AlgoRequest> {
    if let Some(f) = &a.request {
        let mut j: Value = serde_json::from_slice(
            &std::fs::read(f).with_context(|| format!("reading {}", f.display()))?,
        )
        .with_context(|| format!("{}: not JSON", f.display()))?;
        return Ok(AlgoRequest::from_json(j)?);
    }
    let projection = match (
        &a.projection,
        a.predicate.is_empty() && a.direction.is_none(),
    ) {
        (Some(n), _) => Some(ProjectionRef::Named(n.clone())),
        (None, true) => None,
        (None, false) => Some(ProjectionRef::Inline(ProjectionDef {
            predicates: (!a.predicate.is_empty()).then(|| {
                a.predicate
                    .iter()
                    .map(|p| PredicateDef::new(p, None))
                    .collect()
            }),
            direction: a.direction.clone(),
            ..Default::default()
        })),
    };
    let params: Map<String, Value> = pairs(&a.param, "param")?
        .into_iter()
        .map(|(k, v)| (k, param_value(&v)))
        .collect();
    let write = match &a.write_graph {
        None => None,
        Some(graph) => Some(WriteSpec {
            graph: graph.clone(),
            mode: match a.mode.as_str() {
                "replace" => WriteMode::Replace,
                "merge" => WriteMode::Merge,
                m => bail!("--mode {m}: replace or merge"),
            },
            predicates: pairs(&a.write, "write")?,
            batch_size: a.batch_size,
            atomic: a.atomic,
        }),
    };
    Ok(AlgoRequest {
        algorithm: a.algorithm.clone().context("an algorithm is required")?,
        projection,
        params,
        write,
    })
}

fn open_dataset(loc: &Path, opts: StoreOptions) -> Result<sparkles::Dataset> {
    if !loc.is_dir() {
        bail!("{} is not a database directory", loc.display());
    }
    let ds = sparkles::Dataset::open_with(loc, opts)?;
    if let Some(e) = ds.algorithms().projections().error() {
        bail!("{e}");
    }
    Ok(ds)
}

fn open_catalog(loc: &Path) -> Result<Catalog> {
    if !loc.is_dir() {
        bail!("{} is not a database directory", loc.display());
    }
    Ok(Catalog::open(Some(loc))?)
}

pub fn run(cmd: AlgoCmd, opts: StoreOptions) -> Result<()> {
    let mut out = std::io::stdout().lock();
    match cmd {
        AlgoCmd::Run(a) => {
            let req = request(&a)?;
            let ds = open_dataset(&a.loc, opts)?;
            let qopts = QueryOptions {
                timeout: a.timeout.map(Duration::from_secs_f64),
                ..Default::default()
            };
            let algo = ds.algorithms();
            if req.write.is_some() {
                let ctl = sparkles::task::Control {
                    progress: sparkles::task::Progress::new(|p, m| {
                        eprintln!("{:>3.0}%  {m}", p * 100.0)
                    }),
                    ..sparkles::task::Control::none()
                };
                let author = std::env::var("USER").ok().map(Into::into);
                let rep = algo.run_write(&req, &qopts, author, &ctl)?;
                serde_json::to_writer_pretty(&mut out, &rep)?;
                writeln!(out)?;
            } else if a.summary {
                writeln!(out, "{}", algo.stats(&req, &qopts)?)?;
            } else {
                let r = algo.run(&req, &qopts)?;
                if a.format == "text" {
                    crate::print_table(&r, ds.store(), &mut out)?;
                } else {
                    let f =
                        SolutionsFormat::from_name(&a.format).context("unknown result format")?;
                    results::write_solutions(&r, f, &mut out, None)?;
                    writeln!(out)?;
                }
            }
        }
        AlgoCmd::Projections { cmd } => match cmd {
            ProjectionsCmd::List { loc } => {
                for (name, s) in open_catalog(&loc)?.list() {
                    writeln!(
                        out,
                        "{name}  r{}  {}",
                        s.revision,
                        serde_json::to_string(&s.definition)?
                    )?;
                }
            }
            ProjectionsCmd::Get { loc, name } => {
                let s = open_catalog(&loc)?
                    .get_stored(&name)
                    .with_context(|| format!("no projection '{name}'"))?;
                serde_json::to_writer_pretty(&mut out, &s)?;
                writeln!(out)?;
            }
            ProjectionsCmd::Put {
                loc,
                name,
                json,
                predicate,
                direction,
                node_class,
                weight,
            } => {
                let def = match json {
                    Some(f) => {
                        let mut j: Value = serde_json::from_slice(
                            &std::fs::read(&f)
                                .with_context(|| format!("reading {}", f.display()))?,
                        )
                        .with_context(|| format!("{}: not JSON", f.display()))?;
                        if let Some(d) = j.get_mut("definition").map(Value::take) {
                            j = d;
                        }
                        ProjectionDef::from_json(j)?
                    }
                    None => ProjectionDef {
                        predicates: (!predicate.is_empty()).then(|| {
                            predicate
                                .iter()
                                .map(|p| PredicateDef::new(p, None))
                                .collect()
                        }),
                        direction,
                        node_classes: node_class,
                        weight,
                        ..Default::default()
                    },
                };
                // the store's lock keeps a server from changing the file meanwhile
                let ds = open_dataset(&loc, opts)?;
                let (s, created) = ds.algorithms().projections().put(
                    &name,
                    def,
                    std::env::var("USER").ok(),
                    None,
                )?;
                writeln!(
                    out,
                    "{name}: {} at revision {}",
                    if created { "created" } else { "replaced" },
                    s.revision
                )?;
            }
            ProjectionsCmd::Delete { loc, name } => {
                if !open_dataset(&loc, opts)?
                    .algorithms()
                    .projections()
                    .delete(&name, None)?
                {
                    bail!("no projection '{name}'");
                }
            }
        },
    }
    out.flush()?;
    Ok(())
}
