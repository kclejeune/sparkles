//! `sparkles` — Fuseki-compatible server and Jena-style command line tools
//! (`serve` ≈ fuseki-server, `load` ≈ tdb2.tdbloader, `query` ≈ tdb2.tdbquery / arq,
//! `update` ≈ tdb2.tdbupdate, `dump` ≈ tdb2.tdbdump, `compact`, `backup`, `stats`,
//! `infer` ≈ riot --infer, `shacl` ≈ jena `shacl validate`, `shex` ≈ jena `shex
//! validate|parse`).

mod alloc;
mod auth;
#[cfg(feature = "backup")]
mod backup;
mod check_cmd;
mod clone;
mod compress;
mod exposure;
#[cfg(feature = "fmt")]
mod fmt;
mod geo;
mod http;
#[cfg(feature = "fmt")]
mod lsp;
#[cfg(feature = "mcp")]
mod mcp;
mod obs;
mod otel;
mod outbound;
mod quota_cmd;
mod ratelimit;
mod reasoning;
#[cfg(feature = "auth")]
mod remote;
#[cfg(feature = "shacl")]
mod shacl;
mod shex_cmd;
mod shutdown;
mod state;
mod ui;
#[cfg(any(feature = "shacl", feature = "shex"))]
mod validation_cmd;
mod validation_common;
mod vector;
mod write_validation;

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
    /// Memory for packed vectors and HNSW graphs (`spk:vectorSearch`, vector indexes),
    /// per index generation, in MiB
    #[arg(long, global = true, default_value_t = 4096)]
    vector_memory_mb: u64,
    /// Old index generations named snapshots may keep per dataset
    #[arg(long, global = true, default_value_t = 8)]
    history_max_generations: usize,
    /// Named snapshots per dataset
    #[arg(long, global = true, default_value_t = 256)]
    max_snapshots: usize,
    /// Prefixes per dataset (0: unlimited); a new one past it is refused, and loaded
    /// data stops adding its prefixes
    #[arg(long, global = true, default_value_t = sparkles::store::DEFAULT_MAX_PREFIXES)]
    max_prefixes: usize,
    /// Write without write-time validation (load, update, infer)
    #[arg(long, global = true)]
    no_validate: bool,
    /// Record a change digest with every commit of the databases this command opens
    /// (a database keeps the setting once it is on)
    #[arg(long, global = true)]
    commit_digests: bool,
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

/// Compression of a dump or backup.
#[derive(clap::Args, Clone, Debug)]
struct CompressArgs {
    /// none, gzip, zstd, brotli or lz4 (default: from the file extension, or the
    /// command's default)
    #[arg(long)]
    compress: Option<String>,
    /// Level of the codec: gzip 0-9, zstd 1-22, brotli 0-11
    #[arg(long)]
    level: Option<i32>,
    /// zstd worker threads (default: up to 8)
    #[arg(long)]
    threads: Option<usize>,
}

impl CompressArgs {
    fn codec(&self, default: sparkles::codec::Codec) -> Result<sparkles::codec::Codec> {
        let c = match &self.compress {
            Some(c) => sparkles::codec::Codec::parse(c)?,
            None => default,
        };
        if !c.supported() {
            bail!("built without {c}");
        }
        Ok(c)
    }
    fn level(&self) -> Option<sparkles::codec::Level> {
        self.level.map(sparkles::codec::Level)
    }
    fn threads(&self) -> usize {
        self.threads.unwrap_or_else(|| {
            std::thread::available_parallelism()
                .map_or(1, |n| n.get())
                .min(8)
        })
    }
}

// parsed once; `serve` has most of the flags
#[allow(clippy::large_enum_variant)]
#[derive(Subcommand)]
enum Cmd {
    /// Run the SPARQL server with the web UI
    Serve {
        /// Directory holding the dataset registry, databases and backups
        #[arg(long, default_value = "./data")]
        data: PathBuf,
        /// Address to listen on; a non-loopback address needs --auth-config or
        /// --allow-open-network
        #[arg(long, default_value = "127.0.0.1")]
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
        /// Largest `timeout` a query or update may ask for, in seconds (0: unlimited);
        /// never below --timeout or --update-timeout
        #[arg(long, default_value_t = 1800.0)]
        max_timeout: f64,
        /// Reject updates, uploads and admin changes
        #[arg(long)]
        read_only: bool,
        /// Disable federated SERVICE calls
        #[arg(long)]
        no_service: bool,
        #[command(flatten)]
        outbound: outbound::OutboundArgs,
        /// Let `LOAD <file:…>` read the files under this directory (and nothing else);
        /// without it, the server refuses file loads
        #[arg(long, value_name = "DIR")]
        load_dir: Option<PathBuf>,
        /// Return free heap memory to the OS after this many idle milliseconds (0: never)
        #[arg(long, default_value_t = 1000)]
        idle_release_ms: u64,
        /// Enable full-text search for a dataset: NAME, or NAME=CONFIG.json
        #[arg(long)]
        text: Vec<String>,
        /// Enable the spatial index (GeoSPARQL) for a dataset: NAME, or NAME=geo.json
        #[arg(long)]
        geo: Vec<String>,
        /// Memory for each dataset's spatial index, in MiB; a build that would exceed it
        /// is refused and queries run without the index
        #[arg(long, default_value_t = 4096)]
        geo_mb: u64,
        /// Largest sum of input vertices of one geometry operation (overlay, buffer,
        /// hull, relate); larger ones are a type error
        #[arg(long, default_value_t = 2_000_000)]
        geo_op_vertices: u64,
        /// Never rewrite GeoSPARQL topological properties, whatever a dataset's geo.json
        /// says
        #[arg(long)]
        no_geo_rewrite: bool,
        /// A MapLibre style JSON for the UI's maps (an http(s) URL whose origin the UI's
        /// Content Security Policy allows; the style's tiles, glyphs and sprites must come
        /// from that origin too); without it the UI draws its bundled basemap
        #[arg(long, value_name = "URL")]
        map_style_url: Option<String>,
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
        /// Also expose Fuseki's metric names (fuseki_requests, fuseki_requests_good,
        /// fuseki_requests_bad, ...) on /$/metrics, for dashboards built for Fuseki
        #[arg(long, conflicts_with = "no_metrics")]
        metrics_fuseki_names: bool,
        /// Also serve /$/metrics on this address (HOST:PORT), under the same
        /// authentication; a non-loopback address without --auth-config needs
        /// --allow-open-network
        #[arg(long, value_name = "HOST:PORT", conflicts_with = "no_metrics")]
        metrics_addr: Option<String>,
        /// Budget for the estimated memory of a query's intermediate results, in MiB
        /// (0: unlimited)
        #[arg(long, default_value_t = 8192)]
        query_memory_mb: u64,
        /// Budget for the serialized body of a SPARQL query response, in MiB (0: unlimited)
        #[arg(long, default_value_t = 1024)]
        max_result_mb: u64,
        /// Budget for the serialized body of a Graph Store GET (a graph or whole-dataset
        /// export), in MiB (0: unlimited)
        #[arg(long, default_value_t = 0)]
        max_export_mb: u64,
        /// Maximum number of rows of any intermediate result
        #[arg(long, default_value_t = 200_000_000)]
        max_rows: usize,
        /// Budget for the rows all the operators of one query produce together (0:
        /// unlimited); `max-rows-produced=` lowers it per request
        #[arg(long, default_value_t = 0)]
        max_rows_produced: u64,
        /// Honor `validate=false` on writes, which skips write-time validation
        #[arg(long)]
        allow_unvalidated_writes: bool,
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
        /// Compress responses for clients that accept it: auto or off
        #[arg(long, default_value = "auto", value_name = "MODE")]
        http_compression: String,
        /// Response compression level: fastest, default (zstd 3, brotli 4, gzip 6), best
        /// or a number for the chosen algorithm
        #[arg(long, default_value = "default", value_name = "LEVEL")]
        http_compression_level: String,
        /// Response encodings offered, comma-separated
        #[arg(long, default_value = "zstd,br,gzip,deflate", value_name = "LIST")]
        http_compression_algorithms: String,
        /// Largest decompressed size of a compressed request body or uploaded file, in MiB
        /// (0: unlimited)
        #[arg(long, default_value_t = 65536)]
        max_decompressed_mb: u64,
        /// Largest request body of a SPARQL query (also explain and a /shacl shapes
        /// graph), in MiB (0: unlimited)
        #[arg(long, default_value_t = 16)]
        max_query_body_mb: u64,
        /// Largest request body of a SPARQL update, in MiB (0: unlimited); bulk data
        /// goes through the Graph Store or upload endpoints
        #[arg(long, default_value_t = 256)]
        max_update_body_mb: u64,
        /// Largest request body of an admin (/$/…) request or a prefix change, in MiB
        /// (0: unlimited)
        #[arg(long, default_value_t = 16)]
        max_admin_body_mb: u64,
        /// Largest body of a Graph Store write or upload, in MiB, counted after HTTP
        /// decompression (0: unlimited)
        #[arg(long, default_value_t = 4096)]
        max_upload_mb: u64,
        /// Refuse (507) to spool a request body to the temporary directory, and to commit
        /// to a persistent dataset or write an N-Quads backup in the data directory,
        /// once that would leave less than this much free disk space, in MiB (0: no
        /// check)
        #[arg(long, default_value_t = 1024)]
        min_free_disk_mb: u64,
        /// Largest size of an in-memory dataset, in MiB: a commit that would grow one
        /// past it is refused with 507 (0: unlimited)
        #[arg(long, default_value_t = 4096)]
        max_mem_dataset_mb: u64,
        /// Default storage quota of a persistent dataset, in MiB of its directory on
        /// disk: a write that would take a dataset past it is refused with 507 (0:
        /// unlimited); PUT /$/quota/{ds} or `sparkles quota` sets one per dataset
        #[arg(long, default_value_t = 0)]
        max_dataset_mb: u64,
        /// On SIGTERM or SIGINT, seconds to let requests in flight finish before they
        /// are cancelled (a cancelled write commits nothing)
        #[arg(long, default_value_t = 20.0, value_name = "SECS")]
        shutdown_grace: f64,
        /// Background tasks (compaction, clones, reasoning, full-text builds, N-Quads
        /// backups) that run at once; more wait, queued (0: no limit). Backup repository
        /// tasks have their own --backup-max-tasks
        #[arg(long, default_value_t = state::DEFAULT_MAX_TASKS)]
        max_tasks: usize,
        /// Limit a request class per client: CLASS[@DATASET]=RATE[,burst=N]
        /// [,concurrency=N][,client-concurrency=N][,failure-cost=N] or CLASS=off; classes
        /// auth, query, update, admin (e.g. query=100/s,burst=200)
        #[arg(long, value_name = "SPEC")]
        rate_limit: Vec<String>,
        /// JSON file of rate limits (re-read on SIGHUP); --rate-limit flags apply on top
        #[arg(long, value_name = "FILE")]
        rate_limit_config: Option<PathBuf>,
        /// Proxy (address, CIDR, or `unix` for the Unix socket) whose forwarding header
        /// names the client for rate limiting and the auth layer's limits
        #[arg(long, value_name = "CIDR")]
        rate_limit_trusted_proxy: Vec<String>,
        /// The header trusted proxies name the client in: x-forwarded-for (the default)
        /// or forwarded; the other one is ignored
        #[arg(long, value_name = "HEADER")]
        rate_limit_trusted_proxy_header: Option<String>,
        /// Export traces and metrics over OTLP (also enabled by OTEL_EXPORTER_OTLP_ENDPOINT
        /// and the other OTEL_* variables)
        #[arg(long)]
        otel: bool,
        /// Record query and update text (db.query.text) and plan operator descriptions in
        /// spans; they may hold data
        #[arg(long)]
        otel_query_text: bool,
        /// One span per executed plan operator, synthesized from the recorded timings
        #[arg(long)]
        otel_plan_spans: bool,
        /// Export log events over OTLP too (also OTEL_LOGS_EXPORTER=otlp)
        #[arg(long)]
        otel_logs: bool,
        /// Enable authentication and per-dataset authorization from this TOML file
        /// (re-read on SIGHUP); without it the server is open
        #[arg(long, value_name = "FILE")]
        auth_config: Option<PathBuf>,
        /// Backup repositories and policies from this TOML file (read-only through the
        /// API; re-read on SIGHUP)
        #[cfg(feature = "backup")]
        #[arg(long, value_name = "FILE", env = "SPARKLES_BACKUP_CONFIG")]
        backup_config: Option<PathBuf>,
        /// Backup, restore, verify and GC tasks that run at once; more wait, queued
        #[cfg(feature = "backup")]
        #[arg(long, default_value_t = 2)]
        backup_max_tasks: usize,
        /// Listen on this Unix socket (mode 0660) instead of TCP; with auth, trusted
        /// proxy headers can then be limited to the socket (`proxy.trusted = ["unix"]`)
        #[arg(long, value_name = "PATH")]
        unix_socket: Option<PathBuf>,
        /// Serve without --auth-config on a non-loopback --host, which is refused
        /// otherwise: every client that can reach the port may then read, write and
        /// administer every dataset
        #[arg(
            long,
            env = exposure::ALLOW_OPEN_NETWORK_ENV,
            value_parser = clap::builder::BoolishValueParser::new()
        )]
        allow_open_network: bool,
        /// A browser origin (scheme://host[:port]) whose pages may call the API
        /// cross-origin, without credentials (repeatable; with --auth-config, added to
        /// `cors.origins`). Without auth, such a page may do everything the server
        /// allows. By default no other origin gets CORS headers
        #[arg(long, value_name = "ORIGIN")]
        cors_origin: Vec<String>,
        /// A host name clients reach the server by without --auth-config, such as a
        /// reverse proxy's (repeatable). An open server answers only IP addresses,
        /// localhost, --host and these names, and refuses any other `Host` with 421,
        /// which stops web pages that rebind their DNS name to it; with auth, the same
        /// holds for requests carrying trusted proxy headers from loopback or the Unix
        /// socket
        #[arg(long, value_name = "NAME")]
        public_host: Vec<String>,
        /// Who may use POST /$/format: on (every caller the server admits), authenticated
        /// (not the anonymous principal) or off
        #[cfg(feature = "fmt")]
        #[arg(long, value_enum, default_value_t = state::FormatEndpoint::On, value_name = "MODE")]
        format_endpoint: state::FormatEndpoint,
        /// Largest request body of POST /$/format, in MiB (0: unlimited)
        #[cfg(feature = "fmt")]
        #[arg(long, default_value_t = 16)]
        format_max_mb: u64,
        /// Seconds a POST /$/format request may take, waiting for a slot included
        #[cfg(feature = "fmt")]
        #[arg(long, default_value_t = 10.0, value_name = "SECS")]
        format_timeout: f64,
        #[cfg(feature = "mcp")]
        #[command(flatten)]
        mcp: mcp::http::ServeArgs,
    },
    /// Authentication: hashes, tokens, configuration checks
    #[cfg(feature = "auth")]
    Auth {
        #[command(subcommand)]
        cmd: auth::cli::AuthCmd,
    },
    /// Serve databases or files to LLM agents over the Model Context Protocol (JSON-RPC
    /// on stdin/stdout): read-only tools for schema discovery and bounded queries
    #[cfg(feature = "mcp")]
    Mcp(mcp::McpArgs),
    /// Format SPARQL, Turtle, TriG, N-Triples, N-Quads and JSON-LD: print, check (--check,
    /// -l) or rewrite (--write)
    #[cfg(feature = "fmt")]
    Fmt(fmt::FmtArgs),
    /// A language server for editors (stdio): formatting and syntax diagnostics for the
    /// languages `sparkles fmt` formats
    #[cfg(feature = "fmt")]
    Lsp(lsp::LspArgs),
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
    /// Vector indexes for spk:vectorSearch: create, drop, rebuild, list, status (locally
    /// with --loc, or with --server)
    Vector(vector::VectorArgs),
    /// Build, rebuild or inspect a database's spatial index (GeoSPARQL)
    GeoIndex {
        #[arg(long)]
        loc: PathBuf,
        /// index the geometry literals of these predicates (default: geo:asWKT,
        /// geo:asGeoJSON, geo:hasSerialization)
        #[arg(long)]
        predicate: Vec<String>,
        /// feature → geometry links of the spatial: functions (default:
        /// geo:hasDefaultGeometry, geo:hasGeometry)
        #[arg(long)]
        feature_link: Vec<String>,
        /// do not index these graphs (IRIs; urn:x-arq:DefaultGraph for the default graph)
        #[arg(long)]
        exclude_graph: Vec<String>,
        /// distances on geographic coordinates: geodesic (default) or haversine
        #[arg(long)]
        distance: Option<String>,
        /// also index W3C Basic Geo (wgs84_pos:lat / wgs84_pos:long) pairs as points
        #[arg(long)]
        wgs84: bool,
        /// rebuild even if the index is current
        #[arg(long, conflicts_with_all = ["status", "disable"])]
        rebuild: bool,
        /// print the status as JSON and change nothing
        #[arg(long, conflicts_with = "disable")]
        status: bool,
        /// turn the spatial index off
        #[arg(long)]
        disable: bool,
    },
    /// Bulk load RDF files into a database (creates it if needed)
    Load {
        #[arg(long, required_unless_present = "server")]
        loc: Option<PathBuf>,
        /// Load triples into this named graph
        #[arg(long)]
        graph: Option<String>,
        files: Vec<PathBuf>,
        /// Compression of the files: auto (magic bytes, then the extension), none, gzip,
        /// zstd, brotli or lz4
        #[arg(long, default_value = "auto")]
        compression: String,
        /// Skip the validation of IRIs and language tags, for data whose IRIs are not all
        /// valid (DBpedia's, for one); syntax errors still fail the load
        #[arg(long)]
        lenient: bool,
        /// A message recorded with the commit (shown by `log` and in /$/commits)
        #[arg(long)]
        message: Option<String>,
        /// A server to send this to instead of a local database (with --dataset)
        #[arg(long, env = "SPARKLES_SERVER")]
        server: Option<String>,
        /// The dataset on --server
        #[arg(long)]
        dataset: Option<String>,
        /// Allow plain http to a --server other than localhost
        #[arg(long)]
        insecure_http: bool,
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
        /// A server to send this to instead of a local database (with --dataset)
        #[arg(long, env = "SPARKLES_SERVER")]
        server: Option<String>,
        /// The dataset on --server
        #[arg(long)]
        dataset: Option<String>,
        /// Allow plain http to a --server other than localhost
        #[arg(long)]
        insecure_http: bool,
        // where SERVICE may connect in a local run (a --server applies its own policy)
        #[command(flatten)]
        outbound: outbound::OutboundArgs,
    },
    /// Run a SPARQL update against a database
    Update {
        #[arg(long, required_unless_present = "server")]
        loc: Option<PathBuf>,
        #[arg(long)]
        update: Option<PathBuf>,
        text: Option<String>,
        /// A message recorded with the commit (shown by `log` and in /$/commits)
        #[arg(long)]
        message: Option<String>,
        /// A server to send this to instead of a local database (with --dataset)
        #[arg(long, env = "SPARKLES_SERVER")]
        server: Option<String>,
        /// The dataset on --server
        #[arg(long)]
        dataset: Option<String>,
        /// Allow plain http to a --server other than localhost
        #[arg(long)]
        insecure_http: bool,
        // where SERVICE and LOAD may connect in a local run (a --server applies its own)
        #[command(flatten)]
        outbound: outbound::OutboundArgs,
    },
    /// Write the database as N-Quads, to stdout or a file
    Dump {
        #[arg(long)]
        loc: PathBuf,
        /// a past state: N, commit:N, time:<RFC 3339>, snapshot:NAME
        #[arg(long)]
        at: Option<String>,
        /// Write to this file instead of stdout (its extension picks the compression)
        #[arg(long)]
        out: Option<PathBuf>,
        #[command(flatten)]
        compress: CompressArgs,
    },
    /// Write-time validation of a database (SHACL, or ShEx with --lang shex): status,
    /// set, or turn off
    #[cfg(any(feature = "shacl", feature = "shex"))]
    Validation(validation_cmd::ValidationArgs),
    /// The storage quota of a persistent dataset: print it, set it (--max-mb), or go
    /// back to the default (--default)
    Quota(quota_cmd::QuotaArgs),
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
    /// Back up to a backup repository (create, list, show, delete, restore, verify,
    /// policy); without a subcommand, write a compressed N-Quads dump of --loc to --out
    /// (zstd unless --compress says otherwise)
    #[command(args_conflicts_with_subcommands = true, subcommand_negates_reqs = true)]
    Backup {
        #[arg(long, required = true)]
        loc: Option<PathBuf>,
        #[arg(long, default_value = "./backups")]
        out: PathBuf,
        #[command(flatten)]
        compress: CompressArgs,
        #[cfg(feature = "backup")]
        #[command(subcommand)]
        cmd: Option<backup::cli::BackupCmd>,
    },
    /// Backup repositories: add, list, show, test, verify, remove, gc, locks
    #[cfg(feature = "backup")]
    Repo {
        #[command(subcommand)]
        cmd: backup::cli::RepoCmd,
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
    /// Verify a database without modifying it (safe next to a running server); exits
    /// 0 when clean, 1 on errors, 2 on warnings only
    Check {
        /// Database directory
        #[arg(long, required_unless_present = "data", conflicts_with = "data")]
        loc: Option<PathBuf>,
        /// Server data directory: check every database in its `databases/`
        #[arg(long)]
        data: Option<PathBuf>,
        /// text or json
        #[arg(long, default_value = "text")]
        format: String,
        /// Metadata only: no block decoding, no per-key vocabulary or checksum pass
        #[arg(long)]
        quick: bool,
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
        /// Add a built-in vocabulary's axioms to the rules: geosparql (repeatable)
        #[arg(long = "vocab", value_name = "NAME")]
        vocab: Vec<String>,
        /// Also materialize geo:hasDefaultGeometry for features with exactly one
        /// geo:hasGeometry
        #[arg(long)]
        geo_default_geometry: bool,
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
        /// text or json (with --check also turtle)
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
        /// Output format: text, json, void (the VoID description in Turtle) or turtle
        /// (the VoID description and the declarations in Turtle)
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
    /// ShEx: validate a database (or data files) against a schema and a shape map
    /// (exits with status 1 when an association does not conform), or print schemas
    Shex(shex_cmd::ShexArgs),
}

fn store_opts(cli: &Cli) -> StoreOptions {
    sparkles::vector::set_budget(cli.vector_memory_mb << 20);
    StoreOptions {
        cache_bytes: cli.cache_mb << 20,
        result_cache_bytes: cli.result_cache_mb << 20,
        union_default_graph: cli.union_default_graph,
        history_cache_bytes: cli.history_cache_mb << 20,
        history_max_generations: cli.history_max_generations,
        max_snapshots: cli.max_snapshots,
        max_prefixes: cli.max_prefixes,
        commit_digests: cli.commit_digests,
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
    let notes = sparkles::annotations::read(loc)?;
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
            "commits": pick
                .iter()
                .map(|c| sparkles::commit::AnnotatedCommit {
                    commit: c,
                    annotation: notes.get(&c.seq),
                })
                .collect::<Vec<_>>(),
        });
        println!("{}", serde_json::to_string_pretty(&doc)?);
        return Ok(());
    }
    println!("dataset {id}  head {head}");
    println!(
        "{:>6}  {:<24}  {:<12} {:>10} {:>10} {:>12}  generation  message",
        "seq", "timestamp", "kind", "+inserted", "-deleted", "quads"
    );
    for c in pick {
        let approx = if c.exact { "" } else { "~" };
        let message = notes
            .get(&c.seq)
            .and_then(|a| a.message.as_deref())
            .map(|m| format!("  {m}"))
            .unwrap_or_default();
        let line = format!(
            "{:>6}  {:<24}  {:<12} {:>10} {:>10} {:>12}  {:<10}{message}",
            c.seq,
            c.timestamp(),
            c.kind.name(),
            format!("{}{approx}", c.inserted),
            format!("{}{approx}", c.deleted),
            c.quads,
            c.generation_name()
        );
        println!("{}", line.trim_end());
    }
    Ok(())
}

/// The stack of the threads that parse and run requests (the async runtime's workers and
/// blocking pool, the validation and formatting pools): 8 MiB, as a process's main thread
/// has, rather than the 2 MiB tokio and rayon give theirs. The SPARQL parser takes up to
/// about 1.5 MiB at the deepest nesting it accepts; the planner and the evaluator size
/// their own stack (`sparkles::sparql::depth::with_stack`).
pub(crate) const THREAD_STACK: usize = 8 << 20;

fn main() -> Result<()> {
    run().map_err(rejected_exit)
}

/// A write rejected by write-time validation exits with status 3, its findings on stderr.
fn rejected_exit(e: anyhow::Error) -> anyhow::Error {
    if let Some(sparkles::Error::Rejected(r)) = e.downcast_ref::<sparkles::Error>() {
        eprintln!("{r}");
        for res in r.summary.results.iter().take(10) {
            let t = |k: &str| res[k]["value"].as_str().unwrap_or("").to_string();
            if r.summary.language == sparkles::guard::GuardLanguage::Shex {
                // a ShEx result: node, shape (or START) and the first failure
                let shape = match res["shape"]["type"].as_str() {
                    Some("start") => "START".to_string(),
                    _ => t("shape"),
                };
                let reason = res["reason"].as_str().unwrap_or("");
                eprintln!("  {} @ {shape}: {reason}", t("node"));
                continue;
            }
            let msg = res["messages"][0].as_str().unwrap_or("");
            eprintln!(
                "  {} at {}{}",
                t("sourceConstraintComponent")
                    .rsplit('#')
                    .next()
                    .unwrap_or(""),
                t("focusNode"),
                if msg.is_empty() {
                    String::new()
                } else {
                    format!(": {msg}")
                }
            );
        }
        if r.summary.blocking > 10 {
            eprintln!("  … {} more", r.summary.blocking - 10);
        }
        std::process::exit(3);
    }
    e
}

/// ` · validation passed in 14 ms`: the write-time validation of a CLI write, for its
/// summary line (empty when the database is not validated).
fn validation_note(v: Option<&sparkles::guard::ValidationSummary>) -> String {
    use sparkles::guard::{GuardStatus, Strategy};
    match v {
        None => String::new(),
        Some(v) if v.strategy == Strategy::None => format!(" · validation {}", v.status.name()),
        Some(v) if v.status == GuardStatus::Warned => format!(
            " · validation warned: {} blocking of {} results in {} ms",
            v.blocking, v.total, v.millis
        ),
        Some(v) => format!(" · validation {} in {} ms", v.status.name(), v.millis),
    }
}

/// The `sparkles stats` line of a validated database:
/// `reject · 2 shape graphs · 20 shapes · last full 164 ms`.
fn validation_stats(store: &Store) -> Option<String> {
    match sparkles::guard::config::config_language(store.root()?) {
        Ok(None) => return None,
        Ok(Some(_)) => {}
        Err(e) => return Some(format!("cannot be loaded: {e:#}")),
    }
    Some(match write_validation::install(store) {
        Ok(Some(v)) => v.stats_line(),
        Ok(None) => "off".to_string(),
        Err(e) => format!("cannot be loaded: {e:#}"),
    })
}

/// Open a database for a CLI write, with its write-time validation installed (unless
/// `--no-validate`, which skips it and says so).
fn open_for_write(loc: &std::path::Path, opts: StoreOptions, no_validate: bool) -> Result<Store> {
    let mut opts = opts;
    if no_validate {
        opts.unvalidated_writes = true;
    }
    let store = Store::open(loc, opts)?;
    if no_validate {
        if store.guard_required() {
            eprintln!("warning: --no-validate: write-time validation is skipped");
        }
        return Ok(store);
    }
    write_validation::install(&store)?;
    Ok(store)
}

fn run() -> Result<()> {
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
    let otel_settings = match &cli.cmd {
        Cmd::Serve {
            otel,
            otel_query_text,
            otel_plan_spans,
            otel_logs,
            ..
        } => otel::Settings {
            enabled: *otel,
            query_text: *otel_query_text,
            plan_spans: *otel_plan_spans,
            logs: *otel_logs,
        },
        _ => otel::Settings::default(),
    };
    let otel_guard = otel::init(&otel_settings)?;
    {
        use tracing_subscriber::prelude::*;
        let fmt = tracing_subscriber::fmt::layer().with_writer(std::io::stderr);
        let fmt = match cli.log_format {
            LogFormat::Text => fmt.boxed(),
            LogFormat::Json => fmt
                .json()
                .with_current_span(true)
                .with_span_list(false)
                .boxed(),
        };
        let base = tracing_subscriber::registry().with(filter).with(fmt);
        // not `.with(Option)`: a `None` layer reports OFF and would silence the others
        match otel_guard.layers() {
            Some(otel) => base.with(otel).init(),
            None => base.init(),
        }
    }
    if let Some(d) = otel::describe(&otel_guard) {
        tracing::info!("{d}");
    }
    let opts = store_opts(&cli);
    let no_validate = cli.no_validate;
    match cli.cmd {
        Cmd::Serve {
            data,
            host,
            port,
            mem,
            loc,
            timeout,
            max_timeout,
            read_only,
            no_service,
            outbound,
            load_dir,
            idle_release_ms,
            text,
            geo,
            geo_mb,
            geo_op_vertices,
            no_geo_rewrite,
            map_style_url,
            schema_max_entries,
            no_access_log,
            no_metrics,
            metrics_max_datasets,
            metrics_fuseki_names,
            metrics_addr,
            query_memory_mb,
            max_result_mb,
            max_export_mb,
            max_rows,
            max_rows_produced,
            update_timeout,
            allow_unvalidated_writes,
            auto_reason,
            auto_reason_max_delay,
            http_compression,
            http_compression_level,
            http_compression_algorithms,
            max_decompressed_mb,
            max_query_body_mb,
            max_update_body_mb,
            max_admin_body_mb,
            max_upload_mb,
            min_free_disk_mb,
            max_tasks,
            max_mem_dataset_mb,
            max_dataset_mb,
            shutdown_grace,
            auth_config,
            #[cfg(feature = "backup")]
            backup_config,
            #[cfg(feature = "backup")]
            backup_max_tasks,
            unix_socket,
            allow_open_network,
            cors_origin,
            public_host,
            rate_limit,
            rate_limit_config,
            rate_limit_trusted_proxy,
            rate_limit_trusted_proxy_header,
            #[cfg(feature = "fmt")]
            format_endpoint,
            #[cfg(feature = "fmt")]
            format_max_mb,
            #[cfg(feature = "fmt")]
            format_timeout,
            #[cfg(feature = "mcp")]
            mcp,
            ..
        } => {
            // an open server on the network, or a bad auth configuration, stops the
            // server before anything else
            exposure::check(
                &host,
                unix_socket.is_some(),
                auth_config.is_some(),
                allow_open_network,
            )?;
            if let Some(addr) = &metrics_addr {
                exposure::check_metrics_addr(addr, auth_config.is_some(), allow_open_network)?;
            }
            // one server per data directory (held until the process exits)
            #[cfg(feature = "backup")]
            let _data_lock = backup::lock_data_dir(&data)?;
            let bound = if unix_socket.is_some() { "unix" } else { &host };
            let auth = auth::load(auth_config.as_deref(), &data, bound)?;
            // an in-place restore interrupted between its renames is undone before the
            // registry's datasets are opened
            #[cfg(feature = "backup")]
            backup::recover::startup(&data)?;
            if let Some(o) = cors_origin.iter().find(|o| !exposure::valid_origin(o)) {
                bail!("--cors-origin '{o}': expected scheme://host[:port]");
            }
            let hosts = exposure::Hosts::new(bound, &public_host)?;
            // commits keep the disk reserve; in-memory datasets stay within their limit
            let mut opts = opts;
            opts.min_free_disk_bytes = (min_free_disk_mb > 0).then_some(min_free_disk_mb << 20);
            opts.max_memory_bytes = (max_mem_dataset_mb > 0).then_some(max_mem_dataset_mb << 20);
            // persistent datasets without a quota of their own get this one
            opts.max_disk_bytes = (max_dataset_mb > 0).then_some(max_dataset_mb << 20);
            if !(shutdown_grace.is_finite() && shutdown_grace >= 0.0) {
                bail!("--shutdown-grace expects a number of seconds");
            }
            opts.geo_budget_bytes = geo_mb << 20;
            opts.geo_op_vertices = geo_op_vertices;
            opts.geo_query_rewrite = !no_geo_rewrite;
            // a read-only server writes no index files (it still reads good ones)
            opts.geo_files = !read_only;
            if let Some(url) = &map_style_url {
                ui::map_style_origin(url)?;
            }
            let mut st = state::AppState::new(&data, opts, Duration::from_secs_f64(timeout))?;
            st.auth = auth;
            st.cors_origins = cors_origin;
            st.hosts = hosts;
            st.map_style_url = map_style_url;
            if let Some(w) = auth::proxy_host_warning(&st, !public_host.is_empty()) {
                tracing::warn!("{w}");
            }
            #[cfg(feature = "backup")]
            {
                let mut b = backup::BackupState::new(&data, backup_config, backup_max_tasks)?;
                if let Some(f) = &auth_config {
                    b.forbid_config_dir(f);
                }
                st.backup = Some(Arc::new(b));
            }
            st.read_only = read_only;
            st.allow_service = !no_service;
            st.outbound = outbound.policy()?;
            st.file_loads = outbound::file_loads(load_dir.as_deref(), &data)?;
            st.schema_max_entries = schema_max_entries;
            st.allow_unvalidated_writes = allow_unvalidated_writes;
            st.http_compression = compress::HttpCompression::parse(
                &http_compression,
                &http_compression_level,
                &http_compression_algorithms,
            )?;
            st.access_log = !no_access_log;
            st.metrics = obs::Metrics::new(!no_metrics, metrics_max_datasets);
            st.metrics.fuseki_names = metrics_fuseki_names;
            st.task_queue.set_max(max_tasks);
            let mib = |m: u64| (m > 0).then_some(m << 20);
            st.limits = state::Limits {
                query_memory_bytes: mib(query_memory_mb),
                max_result_bytes: mib(max_result_mb),
                max_export_bytes: mib(max_export_mb),
                max_rows,
                max_rows_produced: (max_rows_produced > 0).then_some(max_rows_produced),
                max_dataset_bytes: mib(max_dataset_mb),
                update_timeout: (update_timeout.is_finite() && update_timeout > 0.0)
                    .then(|| Duration::from_secs_f64(update_timeout)),
                max_decompressed_bytes: mib(max_decompressed_mb),
                max_query_body_bytes: mib(max_query_body_mb),
                max_update_body_bytes: mib(max_update_body_mb),
                max_admin_body_bytes: mib(max_admin_body_mb),
                max_upload_bytes: mib(max_upload_mb),
                min_free_disk_bytes: mib(min_free_disk_mb),
                max_timeout: (max_timeout.is_finite() && max_timeout > 0.0)
                    .then(|| Duration::from_secs_f64(max_timeout)),
            };
            #[cfg(feature = "fmt")]
            {
                if !(format_timeout.is_finite() && format_timeout > 0.0) {
                    bail!("--format-timeout expects a positive number of seconds");
                }
                st.format = state::FormatConf {
                    endpoint: format_endpoint,
                    max_bytes: mib(format_max_mb),
                    timeout: Duration::from_secs_f64(format_timeout),
                    ..Default::default()
                };
            }
            // after the limits: MCP calls stay within them
            #[cfg(feature = "mcp")]
            {
                st.mcp = mcp.conf(&st)?.map(Arc::new);
            }
            if let Some(max) = st.limits.max_timeout
                && (st.default_timeout > max || st.limits.update_timeout.is_some_and(|u| u > max))
            {
                tracing::warn!(
                    "--max-timeout {}s is below --timeout or --update-timeout: requests may still ask for those",
                    max.as_secs_f64()
                );
            }
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
            } else if cfg!(feature = "reasoning") {
                st.auto_reason = Some(reasoning::AutoReason::per_dataset());
            }
            let limit_sources = ratelimit::Sources {
                file: rate_limit_config,
                flags: rate_limit,
                trusted_proxies: rate_limit_trusted_proxy,
                trusted_proxy_header: rate_limit_trusted_proxy_header,
                // with auth, the pre-authentication limit is on by default
                auth: st.auth.is_some(),
            };
            let limit_cfg = limit_sources.load()?;
            for w in ratelimit::client_warnings(
                limit_cfg.as_ref(),
                st.auth.is_some(),
                unix_socket.is_some(),
                exposure::loopback(&host),
            ) {
                tracing::warn!("{w}");
            }
            let requests_limited = exposure::requests_limited(limit_cfg.as_ref());
            if let Some(cfg) = limit_cfg {
                // signed-in callers are limited per principal, others per address
                st.rate_limit = Some(Arc::new(
                    ratelimit::RateLimiter::new(&cfg)
                        .map_err(anyhow::Error::msg)?
                        .with_keyer(Arc::new(auth::PrincipalKeyer)),
                ));
            }
            for w in exposure::warnings(
                &host,
                unix_socket.is_some(),
                st.auth.is_some(),
                requests_limited,
            ) {
                tracing::warn!("{w}");
            }
            let st = Arc::new(st);
            otel::register_metrics(&st);
            #[cfg(feature = "reasoning")]
            if st.read_only {
                if st.auto_reason.is_some() {
                    tracing::warn!("--auto-reason has no effect on a read-only server");
                }
            } else {
                // the loop also serves datasets that enable automatic runs themselves
                reasoning::spawn_auto_reason(st.clone());
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
            for g in geo {
                geo::enable_for(&st, &g)?;
            }
            alloc::start_idle_release(Duration::from_millis(idle_release_ms));
            let rt = tokio::runtime::Builder::new_multi_thread()
                .thread_stack_size(THREAD_STACK)
                .enable_all()
                .build()?;
            // backup tasks drive the repository engine on this runtime
            #[cfg(feature = "backup")]
            backup::start(&st, rt.handle());
            let grace = Duration::from_secs_f64(shutdown_grace);
            let st_after = st.clone();
            let served = rt.block_on(async move {
                let addr = format!("{host}:{port}");
                let tcp = match &unix_socket {
                    None => Some(
                        tokio::net::TcpListener::bind(&addr)
                            .await
                            .with_context(|| format!("binding {addr}"))?,
                    ),
                    Some(_) => None,
                };
                // the metrics listener ends with the runtime, after the main one
                if let Some(maddr) = &metrics_addr {
                    let l = tokio::net::TcpListener::bind(maddr)
                        .await
                        .with_context(|| format!("binding --metrics-addr {maddr}"))?;
                    tracing::info!("metrics at http://{maddr}/$/metrics");
                    let service = obs::metrics_router(st.clone())
                        .into_make_service_with_connect_info::<auth::Peer>();
                    tokio::spawn(async move {
                        if let Err(e) = axum::serve(l, service).await {
                            tracing::error!("metrics listener failed: {e}");
                        }
                    });
                }
                #[cfg(unix)]
                let unix = match &unix_socket {
                    Some(path) => Some(bind_unix(path)?),
                    None => None,
                };
                #[cfg(not(unix))]
                if unix_socket.is_some() {
                    bail!("--unix-socket needs a Unix platform");
                }
                let listening = match &unix_socket {
                    Some(p) => format!("unix:{}", p.display()),
                    None => format!("http://{addr}/"),
                };
                tracing::info!(
                    "Sparkles {} listening on {listening} (UI at /ui/)",
                    env!("CARGO_PKG_VERSION")
                );
                for name in st.datasets.read().keys() {
                    tracing::info!(
                        "  dataset /{name}  →  /{name}/sparql  /{name}/update  /{name}/data"
                    );
                }
                if let Some(rl) = &st.rate_limit {
                    ratelimit::spawn_sweeper(rl.clone(), Duration::from_secs(60));
                    #[cfg(unix)]
                    if limit_sources.file.is_some() {
                        ratelimit::spawn_reload_on_sighup(rl.clone(), limit_sources);
                    }
                }
                // every dataset was opened before the listener was bound
                st.set_phase(obs::Phase::Ready);
                auth::spawn_reload_on_sighup(&st);
                let st2 = st.clone();
                let app = http::router(st.clone());
                let (draining_tx, draining) = tokio::sync::oneshot::channel();
                let shutdown = async move {
                    shutdown_signal().await;
                    st2.set_phase(obs::Phase::Draining);
                    // open MCP streams would hold the shutdown up
                    #[cfg(feature = "mcp")]
                    if let Some(m) = &st2.mcp {
                        m.shutdown.cancel();
                    }
                    tracing::info!(
                        "shutting down: finishing requests in flight (up to {grace:?})"
                    );
                    let _ = draining_tx.send(());
                };
                // the peer address feeds trusted-proxy checks
                let service = app.into_make_service_with_connect_info::<auth::Peer>();
                use std::future::IntoFuture;
                #[cfg(unix)]
                let drained = match (unix, tcp) {
                    (Some(l), _) => {
                        let serve = axum::serve(l, service).with_graceful_shutdown(shutdown);
                        shutdown::drain(serve.into_future(), draining, grace).await?
                    }
                    (None, Some(l)) => {
                        let serve = axum::serve(l, service).with_graceful_shutdown(shutdown);
                        shutdown::drain(serve.into_future(), draining, grace).await?
                    }
                    (None, None) => shutdown::Drained::Finished,
                };
                #[cfg(not(unix))]
                let drained = match tcp {
                    Some(l) => {
                        let serve = axum::serve(l, service).with_graceful_shutdown(shutdown);
                        shutdown::drain(serve.into_future(), draining, grace).await?
                    }
                    None => shutdown::Drained::Finished,
                };
                if drained == shutdown::Drained::GraceElapsed {
                    tracing::warn!(
                        "shutdown grace of {grace:?} elapsed: cancelling the requests still in flight"
                    );
                }
                anyhow::Ok(())
            });
            // cancels what still runs (dropping a request's future sets its cancel flag)
            // and waits a little for it to stop; a write stops before its commit or
            // finishes it
            rt.shutdown_timeout(shutdown::CANCEL_WAIT);
            auth::flush(&st_after);
            // flush spans and metrics of the last requests (bounded)
            otel_guard.shutdown();
            served
        }
        #[cfg(feature = "mcp")]
        Cmd::Mcp(args) => mcp::run(args, opts),
        #[cfg(feature = "fmt")]
        Cmd::Fmt(args) => fmt::run(args),
        #[cfg(feature = "fmt")]
        Cmd::Lsp(args) => lsp::run(args),
        Cmd::Shex(args) => shex_cmd::run(args, opts),
        Cmd::Load {
            loc,
            graph,
            files,
            compression,
            lenient,
            message,
            server,
            dataset,
            insecure_http,
        } => {
            let message = message
                .as_deref()
                .map(sparkles::annotations::validate_message)
                .transpose()?
                .flatten();
            let Some(loc) = loc else {
                if lenient {
                    bail!("--lenient applies to a local database (--loc) only");
                }
                let ds = remote_dataset(server.as_deref(), dataset.as_deref())?;
                #[cfg(feature = "auth")]
                return remote::client::load(
                    server.as_deref(),
                    insecure_http,
                    ds,
                    graph.as_deref(),
                    &files,
                    message.as_deref(),
                );
                #[cfg(not(feature = "auth"))]
                return no_remote(ds, insecure_http);
            };
            if files.is_empty() {
                bail!("no files given");
            }
            let store = open_for_write(&loc, opts, no_validate)?;
            let g = graph.map(oxrdf::NamedNode::new).transpose()?;
            let explicit = match compression.as_str() {
                "auto" => None,
                c => Some(sparkles::codec::Codec::parse(c)?),
            };
            let sources = files
                .iter()
                .map(|f| -> Result<Source> {
                    let mut s = Source::from_path(f, g.clone())?;
                    s.compression = explicit;
                    s.lenient = lenient;
                    // fail before loading anything
                    s.codec()?;
                    Ok(s)
                })
                .collect::<Result<Vec<_>>>()?;
            let t = Instant::now();
            let before = store.snapshot().len();
            let wopts = sparkles::guard::WriteOptions {
                message,
                ..Default::default()
            };
            let r = store.load_with(&sources, sparkles::commit::CommitKind::Load, &wopts)?;
            let after = store.snapshot().len();
            let secs = t.elapsed().as_secs_f64();
            eprintln!(
                "loaded {} quads in {:.2}s ({:.0} quads/s); database now has {after} quads; commit {}{}",
                after - before,
                secs,
                (after - before) as f64 / secs.max(1e-9),
                r.commit.seq,
                validation_note(r.validation.as_deref())
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
            server,
            dataset,
            insecure_http,
            outbound,
        } => {
            let q = match (query, text) {
                (Some(f), _) => std::fs::read_to_string(f)?,
                (None, Some(t)) => t,
                _ => bail!("no query given"),
            };
            if loc.is_none() && data.is_empty() && server.is_some() {
                let ds = remote_dataset(server.as_deref(), dataset.as_deref())?;
                #[cfg(feature = "auth")]
                return remote::client::query(
                    server.as_deref(),
                    insecure_http,
                    ds,
                    &q,
                    &fmt,
                    timeout,
                    explain,
                );
                #[cfg(not(feature = "auth"))]
                return no_remote(ds, insecure_http);
            }
            let outbound = outbound.local_policy()?;
            let store = open_or_load(loc, &data, opts)?;
            let qopts = QueryOptions {
                timeout: timeout.map(Duration::from_secs_f64),
                max_memory_bytes: (memory_mb > 0).then_some(memory_mb << 20),
                allow_service: true,
                outbound,
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
        Cmd::Update {
            loc,
            update,
            text,
            message,
            server,
            dataset,
            insecure_http,
            outbound,
        } => {
            let u = match (update, text) {
                (Some(f), _) => std::fs::read_to_string(f)?,
                (None, Some(t)) => t,
                _ => bail!("no update given"),
            };
            let message = message
                .as_deref()
                .map(sparkles::annotations::validate_message)
                .transpose()?
                .flatten();
            let Some(loc) = loc else {
                let ds = remote_dataset(server.as_deref(), dataset.as_deref())?;
                #[cfg(feature = "auth")]
                return remote::client::update(
                    server.as_deref(),
                    insecure_http,
                    ds,
                    &u,
                    message.as_deref(),
                );
                #[cfg(not(feature = "auth"))]
                return no_remote(ds, insecure_http);
            };
            let outbound = outbound.local_policy()?;
            let store = open_for_write(&loc, opts, no_validate)?;
            let qopts = QueryOptions {
                prefixes: store.prefixes().into_iter().collect(),
                allow_service: true,
                outbound,
                write: sparkles::guard::WriteOptions {
                    message,
                    ..Default::default()
                },
                ..Default::default()
            };
            let s = sparkles::sparql::update::update(&store, &u, &qopts)?;
            let commit = match &s.commit {
                Some(r) if r.committed => format!("commit {}", r.commit.seq),
                Some(r) => format!("no change · head {}", r.commit.seq),
                None => String::new(),
            };
            eprintln!(
                "inserted {} · deleted {} · {commit} · {:.2} ms{}",
                s.inserted,
                s.deleted,
                s.timing.total_ms,
                validation_note(s.commit.as_ref().and_then(|r| r.validation.as_deref()))
            );
            Ok(())
        }
        #[cfg(feature = "auth")]
        Cmd::Auth { cmd } => auth::cli::run(cmd),
        Cmd::Vector(a) => vector::cli(a, opts),
        Cmd::GeoIndex {
            loc,
            predicate,
            feature_link,
            exclude_graph,
            distance,
            wgs84,
            rebuild,
            status,
            disable,
        } => geo::geo_index(
            &loc,
            opts,
            geo::IndexArgs {
                predicate,
                feature_link,
                exclude_graph,
                distance,
                wgs84,
                rebuild,
                status,
                disable,
            },
        ),
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
        Cmd::Dump {
            loc,
            at,
            out,
            compress,
        } => {
            let store = Store::open(&loc, opts)?;
            let codec = compress.codec(
                out.as_deref()
                    .and_then(sparkles::codec::Codec::from_extension)
                    .unwrap_or_default(),
            )?;
            let sink: Box<dyn std::io::Write> = match &out {
                Some(p) => Box::new(
                    std::fs::File::create(p)
                        .with_context(|| format!("creating {}", p.display()))?,
                ),
                None => Box::new(std::io::stdout().lock()),
            };
            let mut w = codec.writer(
                std::io::BufWriter::new(sink),
                compress.level(),
                compress.threads(),
            )?;
            match at {
                Some(a) => {
                    let a: sparkles::history::At = a.parse()?;
                    let r = store.resolve(&a)?;
                    eprintln!("at commit {} ({})", r.commit.seq, r.commit.timestamp());
                    store.dump_nquads_at(&a, &mut w)?;
                }
                None => {
                    store.dump_nquads(&mut w)?;
                }
            }
            w.finish()?;
            Ok(())
        }
        Cmd::Snapshot { cmd } => snapshot_cmd(cmd, opts),
        #[cfg(any(feature = "shacl", feature = "shex"))]
        Cmd::Validation(args) => validation_cmd::run(args, opts),
        Cmd::Quota(args) => quota_cmd::run(args, opts),
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
        #[cfg(feature = "backup")]
        Cmd::Repo { cmd } => backup::cli::run_repo(cmd),
        #[cfg(feature = "backup")]
        Cmd::Backup { cmd: Some(cmd), .. } => backup::cli::run_backup(cmd, opts),
        Cmd::Backup {
            loc, out, compress, ..
        } => {
            // clap requires --loc without a subcommand
            let loc = loc.context("--loc is required")?;
            let store = Store::open(&loc, opts)?;
            let name = loc
                .file_name()
                .map(|f| f.to_string_lossy().into_owned())
                .unwrap_or_else(|| "db".into());
            let codec = compress.codec(sparkles::codec::Codec::dump_default())?;
            let t = Instant::now();
            let p = store.backup_with(&out, &name, codec, compress.level(), compress.threads())?;
            let size = std::fs::metadata(&p).map_or(0, |m| m.len());
            eprintln!(
                "backup written to {} ({}, {:.1}s)",
                p.display(),
                sparkles::error::human_bytes(size),
                t.elapsed().as_secs_f64()
            );
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
        Cmd::Check {
            loc,
            data,
            format,
            quick,
        } => check_cmd::run(loc, data, &format, quick),
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
            if let Some(v) = validation_stats(&store) {
                println!("validation      {v}");
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
            vocab,
            geo_default_geometry,
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
            let extras = sparkles_reasoner::Extras::parse(&vocab, geo_default_geometry)?;
            extras.validate()?;
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
            let store = open_for_write(&loc, opts, no_validate)?;
            if status {
                return print_reasoning_status(&loc, &store, &format);
            }
            if clear {
                let n = sparkles_reasoner::clear(&store)?;
                state::write_reasoning_file(&loc, None)?;
                eprintln!("removed {n} inferred triples");
                return Ok(());
            }
            if !check || profile.is_some() || rules.is_some() || !extras.is_empty() {
                let profile = match rules {
                    Some(f) => sparkles_reasoner::Profile::Rules(std::fs::read_to_string(f)?),
                    None => {
                        let p = profile.as_deref().unwrap_or("rdfs");
                        p.parse()
                            .map_err(|_| anyhow::anyhow!("unknown profile '{p}'"))?
                    }
                };
                let r = sparkles_reasoner::materialize_with(
                    &store,
                    &profile,
                    &extras,
                    &Default::default(),
                )?;
                // lets `sparkles serve` pick the inferences up for this database, with
                // the database's automatic re-run setting kept
                let mut info = reasoning::recorded(&profile, &extras, &r, &store);
                info.auto = state::read_reasoning_file(&loc).and_then(|i| i.auto);
                state::write_reasoning_file(&loc, Some(&info))?;
                eprintln!(
                    "{} inferred triples ({} rules, {} iterations, {} ms) → graph <{}>{}",
                    r.inferred,
                    r.rules,
                    r.iterations,
                    r.millis,
                    sparkles_reasoner::INFERRED_GRAPH,
                    validation_note(r.receipt.as_ref().and_then(|r| r.validation.as_deref()))
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
            } else if format == "turtle" {
                let inf = &j["scope"]["inferences"];
                print!(
                    "{}",
                    report.to_turtle(&sparkles_reasoner::diagnostics::ReportContext {
                        dataset: Some(&name),
                        profile: inf["profile"].as_str(),
                        stale: inf["stale"].as_bool(),
                        commits_since: inf["commitsSince"].as_u64(),
                        prefixes: &dopts.prefixes,
                    })
                );
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
            // `void` is the VoID description, `turtle` the description and the declarations
            let (json, void) = match format.as_str() {
                "json" => (true, None),
                "text" => (false, None),
                "void" => (false, Some(false)),
                "turtle" => (false, Some(true)),
                f => bail!("unknown format '{f}' (text, json, void or turtle)"),
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
                term_totals: void.is_some(),
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
            if let Some(declarations) = void {
                let mut prefixes = sparkles::io::standard_prefixes();
                prefixes.extend(store.prefixes());
                let vopts = sparkles::schema::VoidOptions {
                    dataset: &name,
                    declarations,
                    prefixes: prefixes.into_iter().collect(),
                };
                let turtle = oxrdfio::RdfFormat::Turtle;
                let text = sparkles::schema::void_text(&report, &vopts, turtle);
                out.write_all(text.as_bytes())?;
            } else if json {
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
            let graph = validation_common::GraphParam::parse(&graph)?;
            let snap = store.snapshot();
            if let validation_common::GraphParam::Named(iri) = &graph
                && !validation_common::graph_exists(&snap, iri)
            {
                bail!("no such graph: <{iri}>");
            }
            let inferred = validation_common::graph_exists(&snap, http::INFERRED_GRAPH)
                .then_some(http::INFERRED_GRAPH);
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
        (Some(false), n) => reasoning::up_to_date(n),
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
/// The `--dataset` of a remote command (`--server` without `--loc`).
fn remote_dataset<'a>(server: Option<&str>, dataset: Option<&'a str>) -> Result<&'a str> {
    if server.is_none() {
        bail!("give --loc (a local database) or --server URL --dataset NAME");
    }
    dataset.context("--dataset NAME is required with --server")
}

#[cfg(not(feature = "auth"))]
fn no_remote(_: &str, _: bool) -> Result<()> {
    bail!("--server: built without the remote client (cargo feature \"auth\")")
}

/// `serve --unix-socket`: remove a stale socket, bind, and allow owner and group
/// (mode 0660).
#[cfg(unix)]
fn bind_unix(path: &std::path::Path) -> Result<tokio::net::UnixListener> {
    use std::os::unix::fs::{FileTypeExt, PermissionsExt};
    if let Ok(m) = std::fs::symlink_metadata(path) {
        if !m.file_type().is_socket() {
            bail!("{} exists and is not a socket", path.display());
        }
        std::fs::remove_file(path)?;
    }
    let l = tokio::net::UnixListener::bind(path)
        .with_context(|| format!("binding {}", path.display()))?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o660))?;
    Ok(l)
}

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
    let (format, _) =
        sparkles::io::format_for_path(path).unwrap_or((oxrdfio::RdfFormat::Turtle, None));
    let codec = Source::from_path(path, None)
        .and_then(|s| s.codec())
        .unwrap_or_default();
    let raw = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    let mut text = String::new();
    codec
        .reader(&raw[..], None)?
        .read_to_string(&mut text)
        .with_context(|| format!("reading {} ({codec})", path.display()))?;
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
