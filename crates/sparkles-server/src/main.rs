//! `sparkles` — Fuseki-compatible server and Jena-style command line tools
//! (`serve` ≈ fuseki-server, `load` ≈ tdb2.tdbloader, `query` ≈ tdb2.tdbquery / arq,
//! `update` ≈ tdb2.tdbupdate, `dump` ≈ tdb2.tdbdump, `compact`, `backup`, `stats`,
//! `infer` ≈ riot --infer, `shacl` ≈ jena `shacl validate`).

mod alloc;
mod http;
#[cfg(feature = "shacl")]
mod shacl;
mod state;
mod ui;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use sparkles::io::Source;
use sparkles::sparql::results::{self, SolutionsFormat};
use sparkles::sparql::{QueryKind, QueryOptions};
use sparkles::store::{Store, StoreOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

#[derive(Parser)]
#[command(
    name = "sparkles",
    version,
    about = "High-performance RDF/SPARQL database (Jena/Fuseki compatible)"
)]
struct Cli {
    /// Block cache size in MiB
    #[arg(long, global = true, default_value_t = 1024)]
    cache_mb: u64,
    /// Query result cache size in MiB (0 disables it)
    #[arg(long, global = true, default_value_t = 512)]
    result_cache_mb: u64,
    /// Treat the default graph as the union of all named graphs
    #[arg(long, global = true)]
    union_default_graph: bool,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Run the SPARQL server with the web UI
    Serve {
        /// Directory holding the dataset registry, databases and backups
        #[arg(long, default_value = "./data")]
        data: PathBuf,
        #[arg(long, default_value = "0.0.0.0")]
        host: String,
        #[arg(long, default_value_t = 3030)]
        port: u16,
        /// Add an in-memory dataset (not persisted), e.g. --mem ds
        #[arg(long)]
        mem: Vec<String>,
        /// Serve an existing database directory, e.g. --loc ds=/path/to/db
        #[arg(long)]
        loc: Vec<String>,
        /// Default query timeout in seconds
        #[arg(long, default_value_t = 60.0)]
        timeout: f64,
        /// Reject updates, uploads and admin changes
        #[arg(long)]
        read_only: bool,
        /// Disable federated SERVICE calls
        #[arg(long)]
        no_service: bool,
        /// Return free heap memory to the OS after this many idle milliseconds (0: never)
        #[arg(long, default_value_t = 1000)]
        idle_release_ms: u64,
    },
    /// Bulk load RDF files into a database (creates it if needed)
    Load {
        #[arg(long)]
        loc: PathBuf,
        /// Load triples into this named graph
        #[arg(long)]
        graph: Option<String>,
        files: Vec<PathBuf>,
    },
    /// Run a SPARQL query against a database or files
    Query {
        /// Database directory
        #[arg(long)]
        loc: Option<PathBuf>,
        /// Data files to query (loaded into memory)
        #[arg(long)]
        data: Vec<PathBuf>,
        /// File containing the query
        #[arg(long)]
        query: Option<PathBuf>,
        /// Output format: text, json, xml, csv, tsv, sparkles (graphs: ttl, nt, nq, trig, jsonld, rdfxml)
        #[arg(long, default_value = "text")]
        results: String,
        /// Print the query plan instead of executing
        #[arg(long)]
        explain: bool,
        /// Print the executed plan and timing after the results
        #[arg(long)]
        time: bool,
        #[arg(long)]
        timeout: Option<f64>,
        /// Query string (if --query is not given)
        text: Option<String>,
    },
    /// Run a SPARQL update against a database
    Update {
        #[arg(long)]
        loc: PathBuf,
        #[arg(long)]
        update: Option<PathBuf>,
        text: Option<String>,
    },
    /// Write the database as N-Quads (or TriG) to stdout
    Dump {
        #[arg(long)]
        loc: PathBuf,
    },
    /// Merge updates into a freshly built index generation
    Compact {
        #[arg(long)]
        loc: PathBuf,
    },
    /// Write a gzipped N-Quads backup
    Backup {
        #[arg(long)]
        loc: PathBuf,
        #[arg(long, default_value = "./backups")]
        out: PathBuf,
    },
    /// Print database statistics
    Stats {
        #[arg(long)]
        loc: PathBuf,
    },
    /// List the database's commits (works while a server holds the database)
    Log {
        #[arg(long)]
        loc: PathBuf,
        #[arg(long, default_value_t = 20)]
        limit: usize,
        /// only commits before this seq (newest first)
        #[arg(long, conflicts_with = "after")]
        before: Option<u64>,
        /// only commits after this seq (oldest first)
        #[arg(long)]
        after: Option<u64>,
        /// one commit: N, commit:N or head
        #[arg(long, conflicts_with_all = ["before", "after"])]
        at: Option<String>,
        /// text or json
        #[arg(long, default_value = "text")]
        format: String,
    },
    /// Materialize inferences (rdfs, rdfs-simple, owl-rl or a Jena rules file)
    Infer {
        #[arg(long)]
        loc: PathBuf,
        #[arg(long, default_value = "rdfs")]
        profile: String,
        #[arg(long)]
        rules: Option<PathBuf>,
        /// Remove materialized inferences instead
        #[arg(long)]
        clear: bool,
    },
    /// Validate a database (or data files) against a SHACL shapes graph; exits with
    /// status 1 when the data does not conform
    Shacl {
        /// Database directory
        #[arg(long)]
        loc: Option<PathBuf>,
        /// Data files to validate (loaded into memory)
        #[arg(long)]
        data: Vec<PathBuf>,
        /// Shapes graph file (Turtle, N-Triples, RDF/XML, JSON-LD, ...; `.gz` allowed)
        #[arg(long)]
        shapes: PathBuf,
        /// Data graph: `default`, `union` (all graphs) or a graph IRI
        #[arg(long, default_value = "default")]
        graph: String,
        /// Report format: ttl, json, text (also nt, jsonld, rdfxml)
        #[arg(long, default_value = "ttl")]
        format: String,
        /// Leave materialized inferences (`urn:x-sparkles:inferred`) out of the data graph
        #[arg(long)]
        no_inferences: bool,
        /// Timeout in seconds
        #[arg(long)]
        timeout: Option<f64>,
    },
}

fn store_opts(cli: &Cli) -> StoreOptions {
    StoreOptions {
        cache_bytes: cli.cache_mb << 20,
        result_cache_bytes: cli.result_cache_mb << 20,
        union_default_graph: cli.union_default_graph,
        ..Default::default()
    }
}

/// `sparkles log`: read `dataset.json` and `commits.bin` without taking the database lock.
fn print_log(
    loc: &std::path::Path,
    limit: usize,
    before: Option<u64>,
    after: Option<u64>,
    at: Option<&str>,
    format: &str,
) -> Result<()> {
    let Some((id, all)) = sparkles::commit::read_catalog(&loc.join("commits.bin"))? else {
        bail!(
            "{} has no commit catalog (not a database, or not opened by this version yet)",
            loc.display()
        );
    };
    let head = all.last().map_or(0, |c| c.seq);
    let first = all.first().map_or(0, |c| c.seq);
    let pick: Vec<_> = if let Some(at) = at {
        let seq = match at {
            "head" => head,
            s => s
                .strip_prefix("commit:")
                .unwrap_or(s)
                .parse()
                .with_context(|| format!("invalid commit reference '{s}'"))?,
        };
        all.iter().filter(|c| c.seq == seq).copied().collect()
    } else if let Some(a) = after {
        all.iter()
            .filter(|c| c.seq > a)
            .take(limit)
            .copied()
            .collect()
    } else {
        let b = before.unwrap_or(u64::MAX);
        all.iter()
            .rev()
            .filter(|c| c.seq < b)
            .take(limit)
            .copied()
            .collect()
    };
    if format == "json" {
        let doc = serde_json::json!({
            "datasetId": id,
            "head": head,
            "firstRetained": first,
            "complete": true,
            "commits": pick,
        });
        println!("{}", serde_json::to_string_pretty(&doc)?);
        return Ok(());
    }
    println!("dataset {id}  head {head}");
    println!(
        "{:>6}  {:<24}  {:<12} {:>10} {:>10} {:>12}  generation",
        "seq", "timestamp", "kind", "+inserted", "-deleted", "quads"
    );
    for c in pick {
        let approx = if c.exact { "" } else { "~" };
        println!(
            "{:>6}  {:<24}  {:<12} {:>10} {:>10} {:>12}  {}",
            c.seq,
            c.timestamp(),
            c.kind.name(),
            format!("{}{approx}", c.inserted),
            format!("{}{approx}", c.deleted),
            c.quads,
            c.generation_name()
        );
    }
    Ok(())
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    // progress logging for long-running commands, quiet output for query tools
    let default_filter = match cli.cmd {
        Cmd::Serve { .. } | Cmd::Load { .. } | Cmd::Compact { .. } => {
            "sparkles=info,sparkles_server=info,tower_http=warn"
        }
        _ => "warn",
    };
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| default_filter.into()),
        )
        .with_writer(std::io::stderr)
        .init();
    let opts = store_opts(&cli);
    match cli.cmd {
        Cmd::Serve {
            data,
            host,
            port,
            mem,
            loc,
            timeout,
            read_only,
            no_service,
            idle_release_ms,
        } => {
            let mut st = state::AppState::new(&data, opts, Duration::from_secs_f64(timeout))?;
            st.read_only = read_only;
            st.allow_service = !no_service;
            let st = Arc::new(st);
            for m in mem {
                st.attach(m.trim_start_matches('/'), state::DbType::Mem, None)?;
            }
            for l in loc {
                let (name, path) = l.split_once('=').context("--loc expects NAME=PATH")?;
                st.attach(
                    name.trim_start_matches('/'),
                    state::DbType::Persistent,
                    Some(std::path::Path::new(path)),
                )?;
            }
            alloc::start_idle_release(Duration::from_millis(idle_release_ms));
            let rt = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()?;
            rt.block_on(async move {
                let addr = format!("{host}:{port}");
                let listener = tokio::net::TcpListener::bind(&addr)
                    .await
                    .with_context(|| format!("binding {addr}"))?;
                tracing::info!(
                    "Sparkles {} listening on http://{addr}/ (UI at /ui/)",
                    env!("CARGO_PKG_VERSION")
                );
                for name in st.datasets.read().keys() {
                    tracing::info!(
                        "  dataset /{name}  →  /{name}/sparql  /{name}/update  /{name}/data"
                    );
                }
                axum::serve(listener, http::router(st))
                    .with_graceful_shutdown(async {
                        let _ = tokio::signal::ctrl_c().await;
                    })
                    .await?;
                anyhow::Ok(())
            })
        }
        Cmd::Load { loc, graph, files } => {
            if files.is_empty() {
                bail!("no files given");
            }
            let store = Store::open(&loc, opts)?;
            let g = graph.map(oxrdf::NamedNode::new).transpose()?;
            let sources = files
                .iter()
                .map(|f| Source::from_path(f, g.clone()))
                .collect::<Result<Vec<_>, _>>()?;
            let t = Instant::now();
            let before = store.snapshot().len();
            let r = store.load_as(&sources, sparkles::commit::CommitKind::Load)?;
            let after = store.snapshot().len();
            let secs = t.elapsed().as_secs_f64();
            eprintln!(
                "loaded {} quads in {:.2}s ({:.0} quads/s); database now has {after} quads; commit {}",
                after - before,
                secs,
                (after - before) as f64 / secs.max(1e-9),
                r.commit.seq
            );
            Ok(())
        }
        Cmd::Query {
            loc,
            data,
            query,
            results: fmt,
            explain,
            time,
            timeout,
            text,
        } => {
            let q = match (query, text) {
                (Some(f), _) => std::fs::read_to_string(f)?,
                (None, Some(t)) => t,
                _ => bail!("no query given"),
            };
            let store = open_or_load(loc, &data, opts)?;
            let qopts = QueryOptions {
                timeout: timeout.map(Duration::from_secs_f64),
                allow_service: true,
                prefixes: store.prefixes().into_iter().collect(),
                ..Default::default()
            };
            if explain {
                let (sse, plan) = sparkles::sparql::explain(store.snapshot(), &q, &qopts)?;
                println!("{sse}\n");
                print_plan(&plan, 0);
                return Ok(());
            }
            let r = sparkles::sparql::query(store.snapshot(), &q, &qopts)?;
            let out = std::io::stdout();
            let mut out = out.lock();
            match r.kind {
                QueryKind::Select | QueryKind::Ask if fmt == "text" => {
                    print_table(&r, &store, &mut out)?
                }
                QueryKind::Select | QueryKind::Ask => {
                    let f = SolutionsFormat::from_name(&fmt).context("unknown result format")?;
                    results::write_solutions(&r, f, &mut out, None)?;
                    writeln!(out)?;
                }
                _ => {
                    let f = if fmt == "text" {
                        Some(oxrdfio::RdfFormat::Turtle)
                    } else {
                        results::rdf_format_from_name(&fmt)
                    };
                    results::write_graph(
                        &r,
                        f.context("unknown RDF format")?,
                        &store.prefixes(),
                        &mut out,
                    )?;
                }
            }
            if time {
                eprintln!(
                    "\nparse {:.2} ms · plan {:.2} ms · exec {:.2} ms · total {:.2} ms",
                    r.timing.parse_ms, r.timing.plan_ms, r.timing.exec_ms, r.timing.total_ms
                );
                print_plan_stderr(&r.plan, 0);
            }
            Ok(())
        }
        Cmd::Update { loc, update, text } => {
            let u = match (update, text) {
                (Some(f), _) => std::fs::read_to_string(f)?,
                (None, Some(t)) => t,
                _ => bail!("no update given"),
            };
            let store = Store::open(&loc, opts)?;
            let qopts = QueryOptions {
                prefixes: store.prefixes().into_iter().collect(),
                allow_service: true,
                ..Default::default()
            };
            let s = sparkles::sparql::update::update(&store, &u, &qopts)?;
            let commit = match s.commit {
                Some(r) if r.committed => format!("commit {}", r.commit.seq),
                Some(r) => format!("no change · head {}", r.commit.seq),
                None => String::new(),
            };
            eprintln!(
                "inserted {} · deleted {} · {commit} · {:.2} ms",
                s.inserted, s.deleted, s.timing.total_ms
            );
            Ok(())
        }
        Cmd::Log {
            loc,
            limit,
            before,
            after,
            at,
            format,
        } => print_log(&loc, limit, before, after, at.as_deref(), &format),
        Cmd::Dump { loc } => {
            let store = Store::open(&loc, opts)?;
            let out = std::io::BufWriter::new(std::io::stdout().lock());
            store.dump_nquads(out)?;
            Ok(())
        }
        Cmd::Compact { loc } => {
            let store = Store::open(&loc, opts)?;
            let t = Instant::now();
            store.compact()?;
            eprintln!(
                "compacted into {} in {:.2}s",
                store.snapshot().generation.name,
                t.elapsed().as_secs_f64()
            );
            Ok(())
        }
        Cmd::Backup { loc, out } => {
            let store = Store::open(&loc, opts)?;
            let name = loc
                .file_name()
                .map(|f| f.to_string_lossy().into_owned())
                .unwrap_or_else(|| "db".into());
            let p = store.backup(&out, &name)?;
            eprintln!("backup written to {}", p.display());
            Ok(())
        }
        Cmd::Stats { loc } => {
            let store = Store::open(&loc, opts)?;
            let s = store.snapshot();
            let g = &s.generation;
            println!("generation      {}", g.name);
            println!("quads           {}", s.len());
            println!("  base          {}", g.meta.quads);
            println!(
                "  delta +/-     {} / {}",
                s.delta.inserts(),
                s.delta.deletes()
            );
            println!(
                "terms           {} (+{} delta)",
                g.vocab.len(),
                g.dvocab.len()
            );
            println!("subjects        {}", g.stats.distinct_subjects);
            println!("predicates      {}", g.stats.distinct_predicates);
            println!("objects         {}", g.stats.distinct_objects);
            println!("named graphs    {}", s.graph_ids()?.len());
            println!(
                "disk            {:.1} MiB",
                store.disk_bytes() as f64 / (1 << 20) as f64
            );
            let mut preds = g.stats.predicates.clone();
            preds.sort_by_key(|p| std::cmp::Reverse(p.count));
            println!("\ntop predicates:");
            for p in preds.iter().take(20) {
                let name = s
                    .term(sparkles::id::Id(p.p))
                    .map(|t| t.to_string())
                    .unwrap_or_default();
                println!(
                    "  {:>12}  {name}  (S {}, O {})",
                    p.count, p.distinct_subjects, p.distinct_objects
                );
            }
            Ok(())
        }
        #[cfg(not(feature = "reasoning"))]
        Cmd::Infer { .. } => bail!("built without the `reasoning` feature"),
        #[cfg(feature = "reasoning")]
        Cmd::Infer {
            loc,
            profile,
            rules,
            clear,
        } => {
            let store = Store::open(&loc, opts)?;
            if clear {
                let n = sparkles_reasoner::clear(&store)?;
                state::write_reasoning_file(&loc, None)?;
                eprintln!("removed {n} inferred triples");
                return Ok(());
            }
            let profile = match rules {
                Some(f) => sparkles_reasoner::Profile::Rules(std::fs::read_to_string(f)?),
                None => profile
                    .parse()
                    .map_err(|_| anyhow::anyhow!("unknown profile '{profile}'"))?,
            };
            let profile_name = profile.name().to_string();
            let r = sparkles_reasoner::materialize(&store, &profile, &Default::default())?;
            // lets `sparkles serve` pick the inferences up for this database
            state::write_reasoning_file(
                &loc,
                Some(&state::ReasoningInfo {
                    profile: profile_name,
                    inferred: r.inferred,
                    at: state::now(),
                }),
            )?;
            eprintln!(
                "{} inferred triples ({} rules, {} iterations, {} ms) → graph <{}>",
                r.inferred,
                r.rules,
                r.iterations,
                r.millis,
                sparkles_reasoner::INFERRED_GRAPH
            );
            for w in r.warnings {
                eprintln!("warning: {w}");
            }
            Ok(())
        }
        #[cfg(not(feature = "shacl"))]
        Cmd::Shacl { .. } => bail!("built without the `shacl` feature"),
        #[cfg(feature = "shacl")]
        Cmd::Shacl {
            loc,
            data,
            shapes,
            graph,
            format,
            no_inferences,
            timeout,
        } => {
            let fmt = shacl::ReportFormat::from_name(&format)
                .with_context(|| format!("unknown report format '{format}'"))?;
            let shapes = read_shapes(&shapes)?;
            let store = open_or_load(loc, &data, opts)?;
            let graph = shacl::DataGraph::parse(&graph)?;
            let snap = store.snapshot();
            if let shacl::DataGraph::Named(iri) = &graph
                && !shacl::graph_exists(&snap, iri)
            {
                bail!("no such graph: <{iri}>");
            }
            let inferred =
                shacl::graph_exists(&snap, http::INFERRED_GRAPH).then_some(http::INFERRED_GRAPH);
            let mut vopts = shacl::validate_options(&snap, &graph, inferred, !no_inferences)?;
            vopts.timeout = timeout.map(Duration::from_secs_f64);
            let report = sparkles_shacl::validate(&snap, &shapes, &vopts)?;
            let mut out = std::io::stdout().lock();
            out.write_all(&shacl::write_report(&report, fmt)?)?;
            if fmt == shacl::ReportFormat::Json {
                writeln!(out)?;
            }
            out.flush()?;
            if !report.conforms {
                std::process::exit(1);
            }
            Ok(())
        }
    }
}

/// A database directory, or the given files loaded into an in-memory store.
fn open_or_load(loc: Option<PathBuf>, data: &[PathBuf], opts: StoreOptions) -> Result<Store> {
    Ok(match loc {
        Some(l) => Store::open(&l, opts)?,
        None => {
            let s = Store::in_memory(opts);
            let sources = data
                .iter()
                .map(|f| Source::from_path(f, None))
                .collect::<Result<Vec<_>, _>>()?;
            if !sources.is_empty() {
                s.load(&sources)?;
            }
            s
        }
    })
}

#[cfg(feature = "shacl")]
fn read_shapes(path: &std::path::Path) -> Result<sparkles_shacl::Shapes> {
    use std::io::Read;
    let (format, gz) =
        sparkles::io::format_for_path(path).unwrap_or((oxrdfio::RdfFormat::Turtle, false));
    let raw = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    let text = if gz {
        let mut s = String::new();
        flate2::read::MultiGzDecoder::new(&raw[..]).read_to_string(&mut s)?;
        s
    } else {
        String::from_utf8(raw).with_context(|| format!("{} is not UTF-8", path.display()))?
    };
    let base = std::path::absolute(path)
        .ok()
        .map(|p| format!("file://{}", p.display()));
    sparkles_shacl::Shapes::parse(&text, format, base.as_deref())
        .with_context(|| format!("reading shapes from {}", path.display()))
}

fn print_plan(p: &sparkles::sparql::PlanInfo, depth: usize) {
    println!(
        "{}{} {}  [est {} rows, cost {}]",
        "  ".repeat(depth),
        p.operator,
        p.description,
        p.estimated_rows,
        p.estimated_cost
    );
    for c in &p.children {
        print_plan(c, depth + 1);
    }
}

fn print_plan_stderr(p: &sparkles::sparql::PlanInfo, depth: usize) {
    eprintln!(
        "{}{} {}  [{} rows (est {}), {:.2} ms]",
        "  ".repeat(depth),
        p.operator,
        p.description,
        p.actual_rows,
        p.estimated_rows,
        p.time_ms
    );
    for c in &p.children {
        print_plan_stderr(c, depth + 1);
    }
}

/// Jena-style text table (`ResultSetFormatter.out`).
fn print_table(
    r: &sparkles::sparql::QueryResult,
    store: &Store,
    out: &mut impl Write,
) -> Result<()> {
    if r.kind == QueryKind::Ask {
        writeln!(out, "{}", if r.boolean { "yes" } else { "no" })?;
        return Ok(());
    }
    let prefixes = store.prefixes();
    let show = |t: Option<oxrdf::Term>| -> String {
        match t {
            None => String::new(),
            Some(oxrdf::Term::NamedNode(n)) => {
                for (p, ns) in &prefixes {
                    if let Some(l) = n.as_str().strip_prefix(ns.as_str())
                        && l.chars()
                            .all(|c| c.is_alphanumeric() || c == '_' || c == '-')
                    {
                        return format!("{p}:{l}");
                    }
                }
                format!("<{}>", n.as_str())
            }
            Some(t) => t.to_string(),
        }
    };
    let rows: Vec<Vec<String>> = r
        .rows()
        .into_iter()
        .map(|row| row.into_iter().map(show).collect())
        .collect();
    let mut widths: Vec<usize> = r.vars.iter().map(|v| v.chars().count() + 1).collect();
    for row in &rows {
        for (i, c) in row.iter().enumerate() {
            widths[i] = widths[i].max(c.chars().count());
        }
    }
    let line: String = widths
        .iter()
        .map(|w| "-".repeat(w + 2))
        .collect::<Vec<_>>()
        .join("-");
    writeln!(out, "-{line}-")?;
    let hdr: Vec<String> = r
        .vars
        .iter()
        .enumerate()
        .map(|(i, v)| format!(" {:w$} ", format!("?{v}"), w = widths[i]))
        .collect();
    writeln!(out, "|{}|", hdr.join("|"))?;
    writeln!(out, "={}=", "=".repeat(line.chars().count()))?;
    for row in &rows {
        let cells: Vec<String> = row
            .iter()
            .enumerate()
            .map(|(i, c)| format!(" {:w$} ", c, w = widths[i]))
            .collect();
        writeln!(out, "|{}|", cells.join("|"))?;
    }
    writeln!(out, "-{line}-")?;
    Ok(())
}
