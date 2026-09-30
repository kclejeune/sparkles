//! `sparkles` — Fuseki-compatible server and Jena-style command line tools
//! (`serve` ≈ fuseki-server, `load` ≈ tdb2.tdbloader, `query` ≈ tdb2.tdbquery / arq,
//! `update` ≈ tdb2.tdbupdate, `dump` ≈ tdb2.tdbdump, `compact`, `backup`, `stats`,
//! `infer` ≈ riot --infer, `shacl` ≈ jena `shacl validate`).

mod alloc;
mod clone;
mod http;
mod obs;
mod reasoning;
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
    /// Memory for materialized past states (point-in-time reads), in MiB
    #[arg(long, global = true, default_value_t = 1024)]
    history_cache_mb: u64,
    /// Old index generations named snapshots may keep per dataset
    #[arg(long, global = true, default_value_t = 8)]
    history_max_generations: usize,
    /// Named snapshots per dataset
    #[arg(long, global = true, default_value_t = 256)]
    max_snapshots: usize,
    /// Log format on stderr: text, or json (one object per line)
    #[arg(long, global = true, value_enum, default_value_t = LogFormat::Text)]
    log_format: LogFormat,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum SnapshotCmd {
    /// Pin a commit under a name (default: the head)
    Create {
        #[arg(long)]
        loc: PathBuf,
        name: String,
        /// N, commit:N, time:<RFC 3339>, snapshot:NAME
        #[arg(long)]
        at: Option<String>,
        #[arg(long)]
        note: Option<String>,
    },
    /// List named snapshots
    List {
        #[arg(long)]
        loc: PathBuf,
        /// text or json
        #[arg(long, default_value = "text")]
        format: String,
    },
    /// Remove a named snapshot (and the history only it kept)
    Delete {
        #[arg(long)]
        loc: PathBuf,
        name: String,
    },
    /// Retained generations and readable commits
    History {
        #[arg(long)]
        loc: PathBuf,
        /// text or json
        #[arg(long, default_value = "text")]
        format: String,
    },
    /// Keep the recent past readable: the last N commits and/or a duration (90s, 30m, 12h, 7d)
    Retain {
        #[arg(long)]
        loc: PathBuf,
        #[arg(long)]
        keep_commits: Option<u64>,
        #[arg(long)]
        keep_age: Option<String>,
        /// turn retention off
        #[arg(long, conflicts_with_all = ["keep_commits", "keep_age"])]
        off: bool,
    },
}

/// `90s`, `30m`, `12h`, `7d`, `2w`, or plain seconds, to milliseconds.
fn parse_duration_ms(s: &str) -> Result<u64> {
    let split = s.find(|c: char| !c.is_ascii_digit()).unwrap_or(s.len());
    let (num, unit) = s.split_at(split);
    let n: u64 = num
        .parse()
        .with_context(|| format!("invalid duration {s:?}"))?;
    let secs = match unit {
        "" | "s" => 1,
        "m" => 60,
        "h" => 3600,
        "d" => 86_400,
        "w" => 604_800,
        _ => bail!("invalid duration {s:?}: use s, m, h, d or w"),
    };
    Ok(n * secs * 1000)
}

fn snapshot_cmd(cmd: SnapshotCmd, opts: StoreOptions) -> Result<()> {
    use sparkles::history::{At, Retention};
    match cmd {
        SnapshotCmd::Create {
            loc,
            name,
            at,
            note,
        } => {
            let store = Store::open(&loc, opts)?;
            let at: At = at.as_deref().unwrap_or("head").parse()?;
            let (s, created) = store.create_snapshot(&name, &at, note)?;
            println!(
                "{} → commit {}{}",
                s.name,
                s.seq,
                if created { "" } else { " (already pinned)" }
            );
        }
        SnapshotCmd::List { loc, format } => {
            let store = Store::open(&loc, opts)?;
            let snaps = store.snapshots();
            if format == "json" {
                let j: Vec<serde_json::Value> = snaps
                    .iter()
                    .map(|s| {
                        serde_json::json!({
                            "name": s.name, "seq": s.seq, "commit": s.commit,
                            "created": sparkles::commit::rfc3339_ms(s.created_ms),
                            "note": s.note, "generation": s.generation,
                            "reconstructable": s.reconstructable,
                        })
                    })
                    .collect();
                println!("{}", serde_json::to_string_pretty(&j)?);
            } else {
                println!(
                    "{:<16} {:>8}  {:<24}  {:<10}  note",
                    "snapshot", "commit", "timestamp", "generation"
                );
                for s in snaps {
                    println!(
                        "{:<16} {:>8}  {:<24}  {:<10}  {}{}",
                        s.name,
                        s.seq,
                        s.commit.map(|c| c.timestamp()).unwrap_or_default(),
                        s.generation.unwrap_or_else(|| "-".into()),
                        s.note.unwrap_or_default(),
                        if s.reconstructable {
                            ""
                        } else {
                            "  (not reconstructable)"
                        }
                    );
                }
            }
        }
        SnapshotCmd::Delete { loc, name } => {
            let store = Store::open(&loc, opts)?;
            if !store.delete_snapshot(&name)? {
                bail!("no snapshot '{name}'");
            }
            println!("deleted {name}");
        }
        SnapshotCmd::History { loc, format } => {
            let store = Store::open(&loc, opts)?;
            print_history(&store.history(), &format)?;
        }
        SnapshotCmd::Retain {
            loc,
            keep_commits,
            keep_age,
            off,
        } => {
            let store = Store::open(&loc, opts)?;
            let r = if off {
                Retention::default()
            } else {
                Retention {
                    keep_commits,
                    keep_age_ms: keep_age.as_deref().map(parse_duration_ms).transpose()?,
                }
            };
            print_history(&store.set_retention(r)?, "text")?;
        }
    }
    Ok(())
}

fn print_history(h: &sparkles::history::HistoryStatus, format: &str) -> Result<()> {
    if format == "json" {
        let j = serde_json::json!({
            "head": h.head,
            "reconstructable": h.reconstructable.iter().map(|(a, b)| serde_json::json!({"from": a, "to": b})).collect::<Vec<_>>(),
            "bytes": h.bytes,
            "generations": h.generations.iter().map(|g| serde_json::json!({
                "name": g.name, "baseSeq": g.base_seq, "endSeq": g.end_seq, "bytes": g.bytes,
                "current": g.current, "heldBy": g.held_by.iter().map(|x| x.to_string()).collect::<Vec<_>>(),
            })).collect::<Vec<_>>(),
            "retention": h.retention,
            "snapshots": h.snapshots,
        });
        println!("{}", serde_json::to_string_pretty(&j)?);
        return Ok(());
    }
    let ranges: Vec<String> = h
        .reconstructable
        .iter()
        .map(|(a, b)| {
            if a == b {
                a.to_string()
            } else {
                format!("{a}..{b}")
            }
        })
        .collect();
    println!("head {}   readable commits: {}", h.head, ranges.join(", "));
    for g in &h.generations {
        let held: Vec<String> = g.held_by.iter().map(|x| x.to_string()).collect();
        println!(
            "  {}  commits {}..{}  {}  {}",
            g.name,
            g.base_seq,
            g.end_seq,
            if g.current {
                "current".to_string()
            } else {
                format!("{} MiB", g.bytes >> 20)
            },
            held.join(", ")
        );
    }
    let r = &h.retention;
    println!(
        "retention: {}",
        match (r.keep_commits, r.keep_age_ms) {
            (None, None) => "off".to_string(),
            (c, a) => format!(
                "{}{}",
                c.map(|c| format!("last {c} commits ")).unwrap_or_default(),
                a.map(|a| format!("last {}s", a / 1000)).unwrap_or_default()
            ),
        }
    );
    Ok(())
}

#[derive(Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
enum LogFormat {
    Text,
    Json,
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
        /// Enable full-text search for a dataset: NAME, or NAME=CONFIG.json
        #[arg(long)]
        text: Vec<String>,
        /// Largest number of classes, and of predicates, a schema report may have
        #[arg(long, default_value_t = sparkles::schema::DEFAULT_MAX_ENTRIES)]
        schema_max_entries: usize,
        /// Do not log one line per request (target `sparkles::access`)
        #[arg(long)]
        no_access_log: bool,
        /// Disable /$/metrics and stop recording request metrics
        #[arg(long)]
        no_metrics: bool,
        /// Datasets with their own metric labels; the others share `$other`
        #[arg(long, default_value_t = 100)]
        metrics_max_datasets: usize,
        /// Budget for the estimated memory of a query's intermediate results, in MiB
        /// (0: unlimited)
        #[arg(long, default_value_t = 8192)]
        query_memory_mb: u64,
        /// Budget for the serialized body of query and Graph Store GET responses, in MiB
        /// (0: unlimited)
        #[arg(long, default_value_t = 1024)]
        max_result_mb: u64,
        /// Maximum number of rows of any intermediate result
        #[arg(long, default_value_t = 200_000_000)]
        max_rows: usize,
        /// Memory for the packed vectors of `spk:vectorSearch`, per index generation, in MiB
        #[arg(long, default_value_t = 4096)]
        vector_memory_mb: u64,
        /// Timeout of SPARQL updates without a `timeout` parameter, in seconds (0: none)
        #[arg(long, default_value_t = 0.0)]
        update_timeout: f64,
        /// Re-materialize stale inferences automatically once a dataset has had no
        /// commit for this many seconds (off by default; each run holds the writer lock)
        #[arg(long, value_name = "SECS")]
        auto_reason: Option<f64>,
        /// With --auto-reason: run at the latest this many seconds after the inferences
        /// became stale, even while writes continue (default: 12 x the debounce)
        #[arg(long, value_name = "SECS", requires = "auto_reason")]
        auto_reason_max_delay: Option<f64>,
    },
    /// Build, rebuild or inspect a database's full-text index
    TextIndex {
        #[arg(long)]
        loc: PathBuf,
        /// index only these predicates (default: every predicate)
        #[arg(long)]
        predicate: Vec<String>,
        /// do not index these graphs (IRIs; urn:x-arq:DefaultGraph for the default graph)
        #[arg(long)]
        exclude_graph: Vec<String>,
        /// rebuild even if the index is current
        #[arg(long)]
        rebuild: bool,
        /// print the status as JSON and change nothing
        #[arg(long)]
        status: bool,
        /// turn full-text search off and delete the index
        #[arg(long)]
        disable: bool,
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
        /// Query a past state: N, commit:N, time:<RFC 3339>, snapshot:NAME (needs --loc)
        #[arg(long)]
        at: Option<String>,
        /// Budget for the estimated memory of intermediate results, in MiB (0: unlimited)
        #[arg(long, default_value_t = 0)]
        memory_mb: u64,
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
        /// a past state: N, commit:N, time:<RFC 3339>, snapshot:NAME
        #[arg(long)]
        at: Option<String>,
    },
    /// Named snapshots (pins that keep a commit readable) and history retention
    Snapshot {
        #[command(subcommand)]
        cmd: SnapshotCmd,
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
    /// Copy a database into a new, independent one (same data and blank nodes, new
    /// dataset id)
    Clone {
        /// Source database directory
        #[arg(long)]
        loc: PathBuf,
        /// Destination directory (must not exist, or be empty)
        #[arg(long)]
        to: PathBuf,
        /// `copy` the materialized inferences and reasoning status, or `drop` them
        #[arg(long, default_value = "copy")]
        inferences: String,
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
    /// Materialize inferences (rdfs, rdfs-simple, owl-rl or a Jena rules file), show
    /// their status, or check the data for OWL 2 RL inconsistencies
    Infer {
        #[arg(long)]
        loc: PathBuf,
        /// rdfs (the default), rdfs-simple or owl-rl
        #[arg(long)]
        profile: Option<String>,
        #[arg(long)]
        rules: Option<PathBuf>,
        /// Remove materialized inferences instead
        #[arg(long)]
        clear: bool,
        /// Print the reasoning status (are the inferences up to date?)
        #[arg(long, conflicts_with_all = ["clear", "check", "profile", "rules"])]
        status: bool,
        /// Run the inconsistency checks (after materializing, when --profile or --rules
        /// is given); exits with 1 when violations are found, 2 when incomplete
        #[arg(long, conflicts_with = "clear")]
        check: bool,
        /// Comma-separated check ids (default: all)
        #[arg(long, value_delimiter = ',', requires = "check")]
        checks: Vec<String>,
        /// Findings per check
        #[arg(long, default_value_t = 100, requires = "check")]
        limit: usize,
        /// Check the asserted data only, without the materialized inferences
        #[arg(long, requires = "check")]
        no_inferences: bool,
        /// `subclass` (type tests follow rdfs:subClassOf*) or `none`
        #[arg(long, default_value = "subclass", requires = "check")]
        closure: String,
        /// text or json
        #[arg(long, default_value = "text")]
        format: String,
        /// Timeout of the checks in seconds
        #[arg(long, requires = "check")]
        timeout: Option<f64>,
    },
    /// Print the schema of a database (or data files): classes and predicates with exact
    /// counts and their RDFS/OWL declarations; exits with status 2 when a budget is exceeded
    Schema {
        /// Database directory
        #[arg(long)]
        loc: Option<PathBuf>,
        /// Data files (loaded into memory)
        #[arg(long)]
        data: Vec<PathBuf>,
        /// Graph whose triples are counted: `default`, `union` (all graphs) or a graph IRI
        #[arg(long, default_value = "default")]
        graph: String,
        /// Graph read for declarations (default: the same as --graph)
        #[arg(long)]
        declared_graph: Option<String>,
        /// Leave materialized inferences (`urn:x-sparkles:inferred`) out of the counts
        #[arg(long)]
        no_inferences: bool,
        /// Declarations to read: `asserted`, or `all` (including inferred ones)
        #[arg(long, default_value = "asserted")]
        declared: String,
        /// Output format: text or json
        #[arg(long, default_value = "text")]
        format: String,
        /// Timeout in seconds
        #[arg(long)]
        timeout: Option<f64>,
        /// Largest number of classes, and of predicates
        #[arg(long, default_value_t = sparkles::schema::DEFAULT_MAX_ENTRIES)]
        max_entries: usize,
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
        history_cache_bytes: cli.history_cache_mb << 20,
        history_max_generations: cli.history_max_generations,
        max_snapshots: cli.max_snapshots,
        ..Default::default()
    }
}

/// `serve --text NAME[=CONFIG]`
#[cfg(feature = "text")]
fn enable_text_for(st: &state::AppState, spec: &str) -> Result<()> {
    let (name, cfg) = match spec.split_once('=') {
        Some((n, path)) => (
            n,
            serde_json::from_slice(
                &std::fs::read(path).with_context(|| format!("reading {path}"))?,
            )
            .with_context(|| format!("{path}: invalid full-text configuration"))?,
        ),
        None => (spec, sparkles::text::TextConfig::default()),
    };
    let ds = st
        .get(name.trim_start_matches('/'))
        .with_context(|| format!("--text: no dataset {name}"))?;
    let s = ds.store.enable_text(cfg)?;
    tracing::info!("full-text search on /{name}: {} documents", s.docs);
    Ok(())
}

#[cfg(not(feature = "text"))]
fn enable_text_for(_: &state::AppState, _: &str) -> Result<()> {
    bail!("built without full-text search (cargo feature \"text\")")
}

/// `sparkles text-index`
#[cfg(feature = "text")]
fn text_index(
    loc: &std::path::Path,
    opts: StoreOptions,
    predicates: Vec<String>,
    exclude_graph: Vec<String>,
    rebuild: bool,
    status: bool,
    disable: bool,
) -> Result<()> {
    use sparkles::text::{PredicateSet, TextConfig};
    let store = Store::open(loc, opts)?;
    if disable {
        store.disable_text()?;
        eprintln!("full-text search disabled");
        return Ok(());
    }
    if status {
        let s = store.text_status();
        println!(
            "{}",
            match s {
                Some(s) => serde_json::to_string_pretty(&s)?,
                None => r#"{ "enabled": false }"#.to_string(),
            }
        );
        return Ok(());
    }
    let t = Instant::now();
    let configured = !predicates.is_empty() || !exclude_graph.is_empty();
    let s = match store.text_status() {
        Some(_) if !configured && rebuild => store.rebuild_text()?,
        Some(s) if !configured => s,
        _ => {
            let mut cfg = TextConfig::default();
            if !predicates.is_empty() {
                cfg.predicates = PredicateSet::Only(predicates);
            }
            cfg.graphs.exclude = exclude_graph;
            store.enable_text(cfg)?
        }
    };
    let preds = match &s.config.predicates {
        PredicateSet::All => "all predicates".to_string(),
        PredicateSet::Only(v) => format!("{} predicates", v.len()),
    };
    eprintln!(
        "text index: {} docs ({preds}), seq {}, {} in {:.0} ms",
        s.docs,
        s.seq,
        s.state,
        t.elapsed().as_secs_f64() * 1000.0
    );
    Ok(())
}

#[cfg(not(feature = "text"))]
fn text_index(
    _: &std::path::Path,
    _: StoreOptions,
    _: Vec<String>,
    _: Vec<String>,
    _: bool,
    _: bool,
    _: bool,
) -> Result<()> {
    eprintln!("built without full-text search (cargo feature \"text\")");
    std::process::exit(2)
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
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| default_filter.into());
    let fmt = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr);
    match cli.log_format {
        LogFormat::Text => fmt.init(),
        LogFormat::Json => fmt
            .json()
            .with_current_span(true)
            .with_span_list(false)
            .init(),
    }
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
            text,
            schema_max_entries,
            no_access_log,
            no_metrics,
            metrics_max_datasets,
            query_memory_mb,
            max_result_mb,
            max_rows,
            update_timeout,
            vector_memory_mb,
            auto_reason,
            auto_reason_max_delay,
        } => {
            let mut st = state::AppState::new(&data, opts, Duration::from_secs_f64(timeout))?;
            st.read_only = read_only;
            st.allow_service = !no_service;
            st.schema_max_entries = schema_max_entries;
            sparkles::vector::set_budget(vector_memory_mb << 20);
            st.access_log = !no_access_log;
            st.metrics = obs::Metrics::new(!no_metrics, metrics_max_datasets);
            let mib = |m: u64| (m > 0).then_some(m << 20);
            st.limits = state::Limits {
                query_memory_bytes: mib(query_memory_mb),
                max_result_bytes: mib(max_result_mb),
                max_rows,
                update_timeout: (update_timeout.is_finite() && update_timeout > 0.0)
                    .then(|| Duration::from_secs_f64(update_timeout)),
            };
            if let Some(secs) = auto_reason {
                if !cfg!(feature = "reasoning") {
                    bail!("--auto-reason: built without the `reasoning` feature");
                }
                if !(secs.is_finite() && secs >= 0.0) {
                    bail!("--auto-reason expects a number of seconds");
                }
                let max = auto_reason_max_delay
                    .filter(|m| m.is_finite() && *m >= 0.0)
                    .map(Duration::from_secs_f64);
                st.auto_reason = Some(reasoning::AutoReason::new(
                    Duration::from_secs_f64(secs),
                    max,
                ));
            }
            let st = Arc::new(st);
            #[cfg(feature = "reasoning")]
            if st.auto_reason.is_some() {
                if st.read_only {
                    tracing::warn!("--auto-reason has no effect on a read-only server");
                } else {
                    reasoning::spawn_auto_reason(st.clone());
                }
            }
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
            for t in text {
                enable_text_for(&st, &t)?;
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
                // every dataset was opened before the listener was bound
                st.set_phase(obs::Phase::Ready);
                let st2 = st.clone();
                axum::serve(listener, http::router(st))
                    .with_graceful_shutdown(async move {
                        shutdown_signal().await;
                        st2.set_phase(obs::Phase::Draining);
                        tracing::info!("shutting down: finishing requests in flight");
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
            at,
            memory_mb,
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
                max_memory_bytes: (memory_mb > 0).then_some(memory_mb << 20),
                allow_service: true,
                prefixes: store.prefixes().into_iter().collect(),
                ..Default::default()
            };
            let snap = match at {
                Some(a) => {
                    let a: sparkles::history::At = a.parse()?;
                    let (snap, r) = store.snapshot_at(&a, &Default::default())?;
                    eprintln!("at commit {} ({})", r.commit.seq, r.commit.timestamp());
                    snap
                }
                None => store.snapshot(),
            };
            if explain {
                let (sse, plan) = sparkles::sparql::explain(snap, &q, &qopts)?;
                println!("{sse}\n");
                print_plan(&plan, 0);
                return Ok(());
            }
            let r = sparkles::sparql::query(snap, &q, &qopts)?;
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
        Cmd::TextIndex {
            loc,
            predicate,
            exclude_graph,
            rebuild,
            status,
            disable,
        } => text_index(
            &loc,
            opts,
            predicate,
            exclude_graph,
            rebuild,
            status,
            disable,
        ),
        Cmd::Log {
            loc,
            limit,
            before,
            after,
            at,
            format,
        } => print_log(&loc, limit, before, after, at.as_deref(), &format),
        Cmd::Dump { loc, at } => {
            let store = Store::open(&loc, opts)?;
            let out = std::io::BufWriter::new(std::io::stdout().lock());
            match at {
                Some(a) => {
                    let a: sparkles::history::At = a.parse()?;
                    let r = store.resolve(&a)?;
                    eprintln!("at commit {} ({})", r.commit.seq, r.commit.timestamp());
                    store.dump_nquads_at(&a, out)?;
                }
                None => {
                    store.dump_nquads(out)?;
                }
            }
            Ok(())
        }
        Cmd::Snapshot { cmd } => snapshot_cmd(cmd, opts),
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
        Cmd::Clone {
            loc,
            to,
            inferences,
        } => {
            let inferences = clone::Inferences::parse(&inferences).with_context(|| {
                format!("--inferences must be copy or drop, not '{inferences}'")
            })?;
            if !loc.join("CURRENT").exists() {
                bail!("{} is not a Sparkles database (no CURRENT)", loc.display());
            }
            if to.exists() && std::fs::read_dir(&to)?.next().is_some() {
                bail!("{} exists and is not empty", to.display());
            }
            let store = Store::open(&loc, opts)?;
            let t = Instant::now();
            let mut tmp = to.as_os_str().to_owned();
            tmp.push(format!(".clone-tmp-{}", std::process::id()));
            let r = clone::clone_into(
                &store,
                &loc.display().to_string(),
                state::read_reasoning_file(&loc),
                std::path::Path::new(&tmp),
                &to,
                inferences,
                None,
            )?;
            eprintln!(
                "cloned {} (commit {}, {} quads, {} graph{}) to {} in {:.2}s",
                loc.display(),
                r.forked_from.seq,
                r.quads,
                r.graphs,
                if r.graphs == 1 { "" } else { "s" },
                to.display(),
                t.elapsed().as_secs_f64()
            );
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
            if let Some(info) = state::read_reasoning_file(&loc) {
                println!("reasoning       {}", reasoning::status_line(&info, &store));
            }
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
            status,
            check,
            checks,
            limit,
            no_inferences,
            closure,
            format,
            timeout,
        } => {
            use sparkles_reasoner::diagnostics::{self, Closure, DiagnoseOptions};
            // bad check options exit with 2, before any work
            let closure = match Closure::parse(&closure) {
                Some(c) => c,
                None => {
                    eprintln!("error: unknown closure '{closure}' (expected subclass or none)");
                    std::process::exit(2);
                }
            };
            if let Err(bad) = diagnostics::select_checks(&checks) {
                eprintln!("error: unknown diagnostics check '{bad}'");
                std::process::exit(2);
            }
            if check && !(1..=diagnostics::MAX_LIMIT).contains(&limit) {
                eprintln!(
                    "error: --limit must be between 1 and {}",
                    diagnostics::MAX_LIMIT
                );
                std::process::exit(2);
            }
            if !loc.join("CURRENT").exists() {
                bail!("{} is not a Sparkles database (no CURRENT)", loc.display());
            }
            let store = Store::open(&loc, opts)?;
            if status {
                return print_reasoning_status(&loc, &store, &format);
            }
            if clear {
                let n = sparkles_reasoner::clear(&store)?;
                state::write_reasoning_file(&loc, None)?;
                eprintln!("removed {n} inferred triples");
                return Ok(());
            }
            if !check || profile.is_some() || rules.is_some() {
                let profile = match rules {
                    Some(f) => sparkles_reasoner::Profile::Rules(std::fs::read_to_string(f)?),
                    None => {
                        let p = profile.as_deref().unwrap_or("rdfs");
                        p.parse()
                            .map_err(|_| anyhow::anyhow!("unknown profile '{p}'"))?
                    }
                };
                let r = sparkles_reasoner::materialize(&store, &profile, &Default::default())?;
                // lets `sparkles serve` pick the inferences up for this database
                state::write_reasoning_file(
                    &loc,
                    Some(&reasoning::recorded(&profile, &r, &store)),
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
            }
            if !check {
                return Ok(());
            }
            let info = state::read_reasoning_file(&loc);
            let has_inferred = info.is_some()
                || store
                    .snapshot()
                    .lookup_iri(sparkles_reasoner::INFERRED_GRAPH)
                    .is_some();
            let dopts = DiagnoseOptions {
                checks,
                limit,
                inferences: has_inferred && !no_inferences,
                closure,
                timeout: timeout.map(Duration::from_secs_f64),
                prefixes: store.prefixes().into_iter().collect(),
            };
            let name = loc
                .file_name()
                .map(|f| f.to_string_lossy().into_owned())
                .unwrap_or_else(|| "db".into());
            let (report, j) =
                match reasoning::diagnostics_json(&name, &store, info.as_ref(), &dopts) {
                    Ok(r) => r,
                    Err(e) => {
                        eprintln!("error: {e:#}");
                        std::process::exit(2);
                    }
                };
            if format == "json" {
                println!("{}", serde_json::to_string_pretty(&j)?);
            } else {
                print_diagnostics(&report, &j);
            }
            std::process::exit(match report.status {
                diagnostics::ReportStatus::NoneFound => 0,
                diagnostics::ReportStatus::ViolationsFound => 1,
                diagnostics::ReportStatus::Incomplete => 2,
            });
        }
        Cmd::Schema {
            loc,
            data,
            graph,
            declared_graph,
            no_inferences,
            declared,
            format,
            timeout,
            max_entries,
        } => {
            use sparkles::index::Perm;
            use sparkles::schema::{GraphSelection, Page, SchemaError, SchemaOptions};
            let json = match format.as_str() {
                "json" => true,
                "text" => false,
                f => bail!("unknown format '{f}' (text or json)"),
            };
            let declared_from_inferred = match declared.as_str() {
                "asserted" => false,
                "all" => true,
                d => bail!("--declared must be asserted or all, not '{d}'"),
            };
            let name = loc
                .as_deref()
                .and_then(|l| l.file_name())
                .map_or_else(|| "data".to_string(), |f| f.to_string_lossy().into_owned());
            let store = open_or_load(loc, &data, opts)?;
            let snap = store.snapshot();
            let sopts = SchemaOptions {
                graph: GraphSelection::parse(&graph).map_err(anyhow::Error::msg)?,
                declared_graph: declared_graph
                    .as_deref()
                    .map(GraphSelection::parse)
                    .transpose()
                    .map_err(anyhow::Error::msg)?,
                inferred_graph: Some(http::INFERRED_GRAPH.to_string()),
                include_inferred: !no_inferences
                    && snap
                        .lookup_iri(http::INFERRED_GRAPH)
                        .is_some_and(|g| snap.count(Perm::Gspo, &[g.0]).unwrap_or(0) > 0),
                declared_from_inferred,
                deadline: timeout.map(|t| Instant::now() + Duration::from_secs_f64(t)),
                cancel: None,
                max_entries,
            };
            let report = match sparkles::schema::discover(&snap, &sopts) {
                Ok(r) => r,
                Err(e @ (SchemaError::Timeout { .. } | SchemaError::TooManyEntries { .. })) => {
                    eprintln!("error: {e}");
                    std::process::exit(2);
                }
                Err(e) => return Err(e.into()),
            };
            let mut out = std::io::stdout().lock();
            if json {
                // every item on one page
                let summary = report.summary(
                    &name,
                    Page {
                        items: &report.classes,
                        total: report.classes.len(),
                        next: None,
                    },
                    Page {
                        items: &report.predicates,
                        total: report.predicates.len(),
                        next: None,
                    },
                );
                serde_json::to_writer_pretty(&mut out, &summary)?;
                writeln!(out)?;
            } else {
                print_schema(&mut out, &report)?;
            }
            out.flush()?;
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

/// `sparkles schema --format text`: one line per class and per predicate.
fn print_schema(out: &mut impl Write, r: &sparkles::schema::SchemaReport) -> Result<()> {
    let s = &r.selection;
    writeln!(
        out,
        "# version {} · generation {} · computed {}",
        r.snapshot.version, r.snapshot.generation, r.snapshot.computed_at
    )?;
    writeln!(
        out,
        "# graph {} · declarations from {} ({}) · inferences {}",
        s.graph,
        s.declared_graph,
        s.declared,
        if s.reasoning { "included" } else { "excluded" }
    )?;
    let t = &r.totals;
    writeln!(
        out,
        "# {} triples · {} classes · {} predicates",
        t.triples, t.classes, t.predicates
    )?;
    for o in &r.ontology {
        let version = o.version_info.first().map(|v| v.value.as_str());
        writeln!(out, "# ontology <{}> {}", o.iri, version.unwrap_or(""))?;
    }
    writeln!(out, "\nclasses ({}):", r.classes.len())?;
    for c in &r.classes {
        write!(out, "  <{}>  instances {}", c.iri, c.observed.instances)?;
        if !c.declared.super_classes.is_empty() {
            write!(out, "  ⊑ {}", c.declared.super_classes.join(", "))?;
        }
        if c.declared.types.is_empty() {
            write!(out, "  (undeclared)")?;
        }
        writeln!(out)?;
    }
    for cycle in &r.hierarchy.cycles {
        writeln!(out, "  cycle: {}", cycle.join(" ⊑ "))?;
    }
    writeln!(out, "\npredicates ({}):", r.predicates.len())?;
    for p in &r.predicates {
        let o = &p.observed;
        let mut kinds: Vec<String> = Vec::new();
        for (name, k) in [
            ("iri", o.objects.iri),
            ("blank", o.objects.blank),
            ("triple", o.objects.triple_term),
        ] {
            if let Some(k) = k {
                kinds.push(format!("{name} {}", k.triples));
            }
        }
        for l in &o.objects.literals {
            let dt = l.datatype.rsplit(['#', '/']).next().unwrap_or(&l.datatype);
            kinds.push(format!("{dt} {}", l.triples));
        }
        writeln!(
            out,
            "  <{}>  triples {}  S {}  O {}  max/subject {}{}",
            p.iri,
            o.triples,
            o.distinct_subjects,
            o.distinct_objects,
            o.max_per_subject,
            if kinds.is_empty() {
                String::new()
            } else {
                format!("  [{}]", kinds.join(", "))
            }
        )?;
    }
    Ok(())
}

/// `sparkles infer --status`.
#[cfg(feature = "reasoning")]
fn print_reasoning_status(loc: &std::path::Path, store: &Store, format: &str) -> Result<()> {
    let info = state::read_reasoning_file(loc);
    if format == "json" {
        let j = match &info {
            Some(i) => reasoning::status_value(i, store, serde_json::json!({ "enabled": false })),
            None => serde_json::json!({ "reasoning": null, "head": store.head_commit().seq }),
        };
        println!("{}", serde_json::to_string_pretty(&j)?);
        return Ok(());
    }
    let head = store.head_commit().seq;
    let Some(info) = info else {
        println!("no materialized inferences (head commit {head})");
        return Ok(());
    };
    let f = reasoning::freshness(&info, store, head);
    println!("profile         {}", info.profile);
    println!("inferred        {}", info.inferred);
    println!("at              {}", info.at);
    println!(
        "commit          {}",
        info.commit.map_or("unknown".to_string(), |c| c.to_string())
    );
    println!("head            {head}");
    let state = match (f.stale, f.commits_since) {
        (Some(false), _) => "up to date".to_string(),
        (Some(true), Some(n)) => {
            format!("STALE ({n} commit{} since)", if n == 1 { "" } else { "s" })
        }
        (Some(true), None) => format!("STALE ({})", f.reason.unwrap_or_default()),
        (None, _) => format!("freshness unknown ({})", f.reason.unwrap_or_default()),
    };
    println!("status          {state}");
    for w in &info.warnings {
        println!("warning         {w}");
    }
    Ok(())
}

/// SIGINT (Ctrl-C) or, on Unix, SIGTERM.
async fn shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        match signal(SignalKind::terminate()) {
            Ok(mut term) => {
                tokio::select! {
                    _ = tokio::signal::ctrl_c() => {}
                    _ = term.recv() => {}
                }
            }
            Err(_) => {
                let _ = tokio::signal::ctrl_c().await;
            }
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}

/// Text form of a diagnostics report.
#[cfg(feature = "reasoning")]
fn print_diagnostics(r: &sparkles_reasoner::diagnostics::DiagnosticsReport, j: &serde_json::Value) {
    use sparkles_reasoner::diagnostics::{Basis, CheckStatus, Severity};
    for f in &r.findings {
        let sev = if f.severity == Severity::Warning {
            " [warning]"
        } else {
            ""
        };
        let basis = if f.basis == Basis::UsesInferences {
            " (uses inferences)"
        } else {
            ""
        };
        println!("{:<14} {}{sev}{basis}", f.rule, f.message);
    }
    for c in &r.checks {
        match c.status {
            CheckStatus::Truncated => println!(
                "{}: the first {} findings are shown; raise --limit for more",
                c.id, c.findings
            ),
            CheckStatus::Timeout => println!("{}: timed out", c.id),
            CheckStatus::Error => println!(
                "{}: error: {}",
                c.id,
                c.error.as_deref().unwrap_or_default()
            ),
            _ => {}
        }
    }
    let inf = &j["scope"]["inferences"];
    let fresh = inf["stale"].as_bool() == Some(false);
    if inf["included"] == true
        && !fresh
        && r.findings.iter().any(|f| f.basis == Basis::UsesInferences)
    {
        println!(
            "note: the inferences are not known to be up to date; findings marked (uses inferences) may be outdated"
        );
    }
    let n = r.findings.len();
    let checks = r.checks.len();
    println!(
        "{}{} ({checks} check{}; this is not a full OWL consistency check)",
        r.status.name(),
        if n > 0 {
            format!(": {n} finding{}", if n == 1 { "" } else { "s" })
        } else {
            String::new()
        },
        if checks == 1 { "" } else { "s" }
    );
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
