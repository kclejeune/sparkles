//! `sparkles` — Fuseki-compatible server and Jena-style command line tools
//! (`serve` ≈ fuseki-server, `load` ≈ tdb2.tdbloader, `query` ≈ tdb2.tdbquery / arq,
//! `update` ≈ tdb2.tdbupdate, `dump` ≈ tdb2.tdbdump, `compact`, `backup`, `stats`,
//! `infer` ≈ riot --infer, `shacl` ≈ jena `shacl validate`, `shex` ≈ jena `shex
//! validate|parse`).

mod alloc;
mod auth;
#[cfg(feature = "backup")]
mod backup;
mod branch_cmd;
mod check_cmd;
mod cli_docs;
mod clone;
mod compaction;
mod compaction_cmd;
mod compress;
mod config_cmd;
mod csv_cmd;
mod describe_cmd;
mod dump_cmd;
mod exposure;
#[cfg(feature = "fmt")]
mod fmt;
mod fuseki_config;
mod geo;
mod geo_index_cmd;
#[cfg(feature = "graphql")]
mod graphql;
mod http;
#[cfg(feature = "fmt")]
mod lsp;
#[cfg(feature = "mcp")]
mod mcp;
mod obs;
mod openapi;
mod otel;
mod outbound;
mod patch_cmd;
#[cfg(feature = "auth")]
mod ping_cmd;
mod queries_cmd;
mod quota_cmd;
mod ratelimit;
mod rdfs;
mod reasoning;
#[cfg(feature = "auth")]
mod remote;
#[cfg(feature = "shacl")]
mod shacl;
mod shex_cmd;
mod shutdown;
mod state;
#[cfg(feature = "tls")]
mod tls;
mod tools;
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
    /// Cache of remote SERVICE results (`SERVICE <cache:…>`) per dataset, in MiB (0
    /// disables it)
    #[arg(long, global = true, default_value_t = 64)]
    service_cache_mb: u64,
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
    /// Do not record the change log that history queries read, unless a dataset's
    /// settings turn it on
    #[arg(long, global = true)]
    no_change_log: bool,
    /// The most disk a dataset's change log may use, in MiB (0: unlimited); a dataset's
    /// settings override it
    #[arg(long, global = true, default_value_t = 1024)]
    change_log_mb: u64,
    /// Named snapshots per dataset
    #[arg(long, global = true, default_value_t = 256)]
    max_snapshots: usize,
    /// Branches per dataset, main included
    #[arg(long, global = true, default_value_t = sparkles::branch::DEFAULT_MAX_BRANCHES)]
    max_branches: usize,
    /// The most upstream log segments a new branch links before it is built as its own
    /// index instead
    #[arg(long, global = true, default_value_t = sparkles::branch::DEFAULT_MAX_BRANCH_DEPTH)]
    max_branch_depth: usize,
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
    /// The most a write-ahead log grows ahead of its commits at once, in KiB, as zero
    /// bytes written and synced in advance (0: no preallocation; each commit appends).
    /// A commit that overwrites preallocated bytes syncs its data without a file-system
    /// journal commit, which on ext4 and XFS takes a fraction of the time
    #[arg(long, global = true, default_value_t = sparkles::store::DEFAULT_WAL_PREALLOC_BYTES >> 10)]
    wal_prealloc_kb: u64,
    /// Log format on stderr: text, or json (one object per line)
    #[arg(long, global = true, value_enum, default_value_t = LogFormat::Text)]
    log_format: LogFormat,
    /// GeoSPARQL: projected CRSs to support beyond the built-in ones, as a JSON file
    /// mapping CRS IRIs to proj4 definitions:
    /// {"<IRI>": {"proj4": "+proj=…", "axis": "en" | "ne"}}
    #[arg(long, global = true, env = "SPARKLES_GEO_CRS", value_name = "FILE")]
    geo_crs: Option<PathBuf>,
    /// The branch to work on, for query, update, load, dump, log, diff, snapshot,
    /// compact, stats and clone (default: main)
    #[arg(long, global = true, value_name = "NAME")]
    branch: Option<String>,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum ShaclCmd {
    /// Read shapes files, check that they are well-formed SHACL, and print them in
    /// another syntax (Jena's `shacl parse`); several files are printed one after
    /// another, each after a `# FILE` header
    #[command(visible_aliases = ["p", "print"])]
    Parse {
        /// Shapes files (`-` for stdin; `.gz` allowed)
        #[arg(required = true, value_name = "FILE")]
        files: Vec<PathBuf>,
        /// Output syntax: shaclc (or compact), turtle, nt, jsonld or rdfxml
        #[arg(long, default_value = "shaclc", value_name = "SYNTAX")]
        out: String,
        /// Input syntax, when the file name does not say (stdin): shaclc, turtle, nt,
        /// jsonld, rdfxml, trig or nquads; default: by extension, else Turtle
        #[arg(long = "in", value_name = "SYNTAX")]
        input: Option<String>,
        /// Base IRI for relative IRIs (default: the file's location)
        #[arg(long, value_name = "IRI")]
        base: Option<String>,
    },
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
        /// Remove the pin after this duration (90s, 30m, 12h, 7d, 2w) or at this RFC 3339
        /// time (a running server's history upkeep, or the next `snapshot gc`)
        #[arg(long)]
        expires: Option<String>,
        /// keep the pinned state materialized in a server's history cache
        #[arg(long)]
        warm: bool,
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
        /// At most this much disk for generations only the window keeps (512MiB, 10GiB)
        #[arg(long)]
        max_bytes: Option<String>,
        /// turn retention off
        #[arg(long, conflicts_with_all = ["keep_commits", "keep_age", "max_bytes"])]
        off: bool,
    },
    /// Pin the head on a schedule as PREFIX<UTC time> and keep the newest few; without
    /// --every, list the schedules (or remove one with --remove)
    Schedule {
        #[arg(long)]
        loc: PathBuf,
        /// the names' prefix, such as daily-
        #[arg(long, required_unless_present = "remove", conflicts_with = "remove")]
        prefix: Option<String>,
        /// how often (30m, 12h, 1d, 1w)
        #[arg(long, requires = "prefix")]
        every: Option<String>,
        /// how many of the schedule's snapshots to keep
        #[arg(long, default_value_t = 7)]
        keep_last: u32,
        /// remove the schedule with this prefix (its snapshots stay)
        #[arg(long)]
        remove: Option<String>,
    },
    /// Drop expired pins, make the pins schedules call for, collect the history nothing
    /// keeps any more and prune the commit catalog (a running server does this every
    /// minute)
    Gc {
        #[arg(long)]
        loc: PathBuf,
    },
    /// Prune the metadata of commits that can no longer be read: keep the last N
    /// commits and/or those of a duration (90s, 30m, 12h, 7d), beyond the readable ones
    Catalog {
        #[arg(long)]
        loc: PathBuf,
        #[arg(long)]
        keep_commits: Option<u64>,
        #[arg(long)]
        keep_age: Option<String>,
        /// keep every commit's metadata (the default)
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
            expires,
            warm,
        } => {
            let store = branch_cmd::open_db(&loc, opts)?;
            let at: At = at.as_deref().unwrap_or("head").parse()?;
            let expires = match expires {
                None => None,
                Some(e) => Some(match format!("time:{e}").parse::<At>() {
                    Ok(At::Time(ms)) => ms,
                    _ => {
                        let now = std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)?
                            .as_millis() as i64;
                        now + parse_duration_ms(&e)? as i64
                    }
                }),
            };
            let (s, created) = store.create_snapshot_opts(
                &name,
                &at,
                &sparkles::history::SnapshotOptions {
                    note,
                    expires_ms: expires,
                    warm,
                },
            )?;
            println!(
                "{} → commit {}{}",
                s.name,
                s.seq,
                if created { "" } else { " (already pinned)" }
            );
        }
        SnapshotCmd::List { loc, format } => {
            let store = branch_cmd::open_db(&loc, opts)?;
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
            let store = branch_cmd::open_db(&loc, opts)?;
            if !store.delete_snapshot(&name)? {
                bail!("no snapshot '{name}'");
            }
            println!("deleted {name}");
        }
        SnapshotCmd::History { loc, format } => {
            let store = branch_cmd::open_db(&loc, opts)?;
            print_history(&store.history(), &format)?;
        }
        SnapshotCmd::Retain {
            loc,
            keep_commits,
            keep_age,
            max_bytes,
            off,
        } => {
            let store = branch_cmd::open_db(&loc, opts)?;
            let r = if off {
                Retention::default()
            } else {
                Retention {
                    keep_commits,
                    keep_age_ms: keep_age.as_deref().map(parse_duration_ms).transpose()?,
                    max_bytes: max_bytes
                        .map(|b| {
                            http::history::parse_size(&serde_json::Value::String(b.clone()))
                                .with_context(|| format!("invalid size {b:?}: use 512MiB, 10GiB"))
                        })
                        .transpose()?,
                }
            };
            print_history(&store.set_retention(r)?, "text")?;
        }
        SnapshotCmd::Schedule {
            loc,
            prefix,
            every,
            keep_last,
            remove,
        } => {
            let store = branch_cmd::open_db(&loc, opts)?;
            let mut all = store.schedules();
            if let Some(p) = remove {
                let before = all.len();
                all.retain(|s| s.prefix != p);
                if all.len() == before {
                    bail!("no schedule with the prefix {p:?}");
                }
                store.set_schedules(all.clone())?;
            } else if let (Some(prefix), Some(every)) = (prefix, every) {
                all.retain(|s| s.prefix != prefix);
                all.push(sparkles::history::Schedule {
                    prefix,
                    every_ms: parse_duration_ms(&every)?,
                    keep_last,
                });
                store.set_schedules(all.clone())?;
            }
            for s in &all {
                println!(
                    "{}<time>  every {}s  keep {}",
                    s.prefix,
                    s.every_ms / 1000,
                    s.keep_last
                );
            }
        }
        SnapshotCmd::Gc { loc } => {
            let store = branch_cmd::open_db(&loc, opts)?;
            let t = store.history_tick()?;
            for n in &t.created {
                println!("created {n}");
            }
            for n in t.expired.iter().chain(&t.rotated) {
                println!("removed {n}");
            }
            let pruned = t.pruned + store.prune_commits()?;
            if pruned > 0 {
                println!("pruned {pruned} commit records");
            }
            print_history(&store.history(), "text")?;
        }
        SnapshotCmd::Catalog {
            loc,
            keep_commits,
            keep_age,
            off,
        } => {
            let store = branch_cmd::open_db(&loc, opts)?;
            let c = if off {
                sparkles::history::CatalogHorizon::default()
            } else {
                sparkles::history::CatalogHorizon {
                    keep_commits,
                    keep_age_ms: keep_age.as_deref().map(parse_duration_ms).transpose()?,
                }
            };
            print_history(&store.set_catalog_horizon(c)?, "text")?;
        }
    }
    Ok(())
}

/// The arguments of `sparkles history`.
struct HistoryArgs {
    subject: Vec<String>,
    predicate: Vec<String>,
    object: Vec<String>,
    graph: Vec<String>,
    from: Option<String>,
    to: Option<String>,
    op: Option<String>,
    limit: usize,
    desc: bool,
    format: String,
}

/// A term argument: N-Triples syntax, or a bare IRI.
fn term_arg(v: &str) -> Result<oxrdf::Term> {
    let v = v.trim();
    if v.starts_with('<') || v.starts_with('"') || v.starts_with("_:") {
        v.parse::<oxrdf::Term>()
            .map_err(|e| anyhow::anyhow!("invalid term {v:?}: {e}"))
    } else {
        Ok(oxrdf::Term::NamedNode(
            oxrdf::NamedNode::new(v).with_context(|| format!("invalid IRI {v:?}"))?,
        ))
    }
}

fn history_cmd(loc: &std::path::Path, a: HistoryArgs, opts: StoreOptions) -> Result<()> {
    use sparkles::store::{DiffOp, HistoryBound, HistoryQuery};
    if !matches!(a.format.as_str(), "text" | "json") {
        bail!("unknown format {:?}: use text or json", a.format);
    }
    let store = Store::open(loc, opts)?;
    let terms = |v: &[String]| v.iter().map(|t| term_arg(t)).collect::<Result<Vec<_>>>();
    let predicates = terms(&a.predicate)?
        .into_iter()
        .map(|t| match t {
            oxrdf::Term::NamedNode(n) => Ok(n),
            t => bail!("--predicate {t} is not an IRI"),
        })
        .collect::<Result<Vec<_>>>()?;
    let graphs = a
        .graph
        .iter()
        .map(|g| match g.as_str() {
            "default" => Ok(oxrdf::GraphName::DefaultGraph),
            g => Ok(oxrdf::GraphName::NamedNode(
                oxrdf::NamedNode::new(g).with_context(|| format!("invalid graph IRI {g:?}"))?,
            )),
        })
        .collect::<Result<Vec<_>>>()?;
    let bound = |s: &Option<String>| -> Result<Option<HistoryBound>> {
        Ok(match s {
            Some(s) => Some(HistoryBound::At(s.parse()?)),
            None => None,
        })
    };
    let q = HistoryQuery {
        subjects: terms(&a.subject)?,
        predicates,
        objects: terms(&a.object)?,
        graphs,
        from: bound(&a.from)?,
        to: bound(&a.to)?,
        op: match a.op.as_deref() {
            None => None,
            Some("add") => Some(DiffOp::Add),
            Some("remove") => Some(DiffOp::Remove),
            Some(o) => bail!("--op must be add or remove, not {o:?}"),
        },
        limit: a.limit,
        descending: a.desc,
        ..Default::default()
    };
    let t = Instant::now();
    let r = store.history_changes(&q)?;
    eprintln!(
        "commits {}..{}: {} change(s){} ({:.1} ms)",
        r.from,
        r.to,
        r.changes.len(),
        if r.truncated {
            ", more past --limit"
        } else {
            ""
        },
        t.elapsed().as_secs_f64() * 1000.0
    );
    for u in &r.unrecorded {
        eprintln!(
            "commits {}..{} are not recorded ({})",
            u.from,
            u.to,
            serde_json::to_value(u.reason)?.as_str().unwrap_or("")
        );
    }
    let mut out = std::io::BufWriter::new(std::io::stdout().lock());
    for c in &r.changes {
        if a.format == "json" {
            let j = serde_json::json!({
                "commit": c.commit.seq,
                "timestamp": c.commit.timestamp(),
                "kind": c.commit.kind.name(),
                "author": c.commit.author.as_deref(),
                "message": c.commit.message.as_deref(),
                "op": match c.op { DiffOp::Add => "add", DiffOp::Remove => "remove" },
                "quad": sparkles::annotations::nquads_line(&c.quad),
            });
            writeln!(out, "{j}")?;
        } else {
            let who = match (&c.commit.author, &c.commit.message) {
                (Some(a), Some(m)) => format!("  # {a}: {m}"),
                (Some(a), None) => format!("  # {a}"),
                (None, Some(m)) => format!("  # {m}"),
                (None, None) => String::new(),
            };
            writeln!(
                out,
                "{:>8}  {}  {} {}{who}",
                c.commit.seq,
                c.commit.timestamp(),
                c.op.sign(),
                sparkles::annotations::nquads_line(&c.quad)
            )?;
        }
    }
    Ok(())
}

fn diff_cmd(
    loc: &std::path::Path,
    from: &str,
    to: &str,
    graph: Option<&str>,
    format: &str,
    opts: StoreOptions,
) -> Result<()> {
    use sparkles::store::{DiffOp, DiffOptions};
    if !matches!(format, "diff" | "json" | "count" | "patch" | "patch-binary") {
        bail!("unknown format {format:?}: use diff, json, count, patch or patch-binary");
    }
    let store = branch_cmd::open_db(loc, opts)?;
    let graph = match graph {
        None => None,
        Some("default") => Some(oxrdf::GraphName::DefaultGraph),
        Some(g) => Some(oxrdf::GraphName::NamedNode(
            oxrdf::NamedNode::new(g).with_context(|| format!("invalid graph IRI {g:?}"))?,
        )),
    };
    let t = Instant::now();
    let d = store.diff(
        &from.parse()?,
        &to.parse()?,
        &DiffOptions {
            graph,
            ..Default::default()
        },
    )?;
    eprintln!(
        "commit {} → commit {}: +{} −{} ({}, {:.1} ms)",
        d.from.commit.seq,
        d.to.commit.seq,
        d.added,
        d.removed,
        d.method.as_str(),
        t.elapsed().as_secs_f64() * 1000.0
    );
    let mut out = std::io::BufWriter::new(std::io::stdout().lock());
    match format {
        "count" => writeln!(out, "+{} -{}", d.added, d.removed)?,
        "json" => {
            let quads: Vec<serde_json::Value> = d
                .iter()
                .map(|(op, q)| {
                    serde_json::json!({
                        "op": op.sign().to_string(),
                        "subject": q.subject.to_string(),
                        "predicate": q.predicate.to_string(),
                        "object": q.object.to_string(),
                        "graph": match &q.graph_name {
                            oxrdf::GraphName::DefaultGraph => serde_json::Value::Null,
                            g => g.to_string().into(),
                        },
                    })
                })
                .collect();
            let j = serde_json::json!({
                "from": { "selector": d.from.at.to_string(), "commit": d.from.commit },
                "to": { "selector": d.to.at.to_string(), "commit": d.to.commit },
                "added": d.added,
                "removed": d.removed,
                "method": d.method.as_str(),
                "quads": quads,
            });
            serde_json::to_writer_pretty(&mut out, &j)?;
            writeln!(out)?;
        }
        "patch" | "patch-binary" => {
            use sparkles::patch::{PatchWriter, commit_iri, write_patch};
            let id = store.dataset_id();
            let quads: Vec<(DiffOp, oxrdf::Quad)> = d.iter().collect();
            let mut w = PatchWriter::new(&mut out, format == "patch-binary");
            write_patch(
                &mut w,
                &commit_iri(id, d.to.commit.seq),
                Some(&commit_iri(id, d.from.commit.seq)),
                quads.iter().map(|(op, q)| (*op, q)),
            )?;
        }
        _ => {
            for (op, q) in d.iter() {
                let sign = if op == DiffOp::Add { '+' } else { '-' };
                writeln!(out, "{sign} {}", sparkles::annotations::nquads_line(&q))?;
            }
        }
    }
    out.flush()?;
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
            "catalog": h.catalog,
            "firstRetained": h.first_commit,
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
                "{}{}{}",
                c.map(|c| format!("last {c} commits ")).unwrap_or_default(),
                a.map(|a| format!("last {}s ", a / 1000))
                    .unwrap_or_default(),
                r.max_bytes
                    .map(|b| format!("at most {} MiB", b >> 20))
                    .unwrap_or_default()
            )
            .trim_end()
            .to_string(),
        }
    );
    let c = &h.catalog;
    println!(
        "commit catalog: from commit {}, {}",
        h.first_commit,
        match (c.keep_commits, c.keep_age_ms) {
            (None, None) => "keeps every commit".to_string(),
            (n, a) => format!(
                "keeps the readable commits{}{}",
                n.map(|n| format!(" and the last {n}")).unwrap_or_default(),
                a.map(|a| format!(" and those of the last {}s", a / 1000))
                    .unwrap_or_default()
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
        /// Start from a Fuseki configuration: a config.ttl, or a Fuseki base directory
        /// with config.ttl, configuration/ and shiro.ini. Its datasets, indexes,
        /// inference, timeouts and access rules are converted at each start, and the
        /// flags given here win; a part with no Sparkles equivalent stops the start
        #[arg(long, value_name = "PATH")]
        fuseki_config: Option<PathBuf>,
        /// Serve an existing database directory, e.g. --loc ds=/path/to/db
        #[arg(long)]
        loc: Vec<String>,
        /// Default query timeout in seconds [default: 60]
        #[arg(long, value_name = "TIMEOUT")]
        timeout: Option<f64>,
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
        /// A secret vector indexes may name as their embedding API key, read from an
        /// environment variable or a file when a request is made: NAME=env:VARIABLE or
        /// NAME=file:PATH (repeatable)
        #[arg(long, value_name = "NAME=SOURCE")]
        embedding_secret: Vec<String>,
        /// Compute no embeddings: no worker sends text to a provider, and searches
        /// cannot pass text (the configurations are kept)
        #[arg(long)]
        no_embedding: bool,
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
        /// Set a dataset's write-time validation at startup from NAME=CONFIG.json (the
        /// body of PUT /$/validation/{ds}), or with NAME alone validate it with the
        /// configuration it has; the data is validated in full and the result logged
        #[arg(long, value_name = "NAME[=CONFIG]")]
        validate: Vec<String>,
        /// Answer a dataset's queries over the RDFS closure of its graphs with respect to
        /// the schema in FILE, as Fuseki's --rdfs does (NAME=FILE, repeatable); the
        /// setting is kept like one made with PUT /$/rdfs/{ds}
        #[arg(long, value_name = "NAME=FILE")]
        rdfs: Vec<String>,
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
        // the ceilings of GraphQL requests
        #[cfg(feature = "graphql")]
        #[command(flatten)]
        graphql: graphql::GraphqlServeArgs,
        /// Fuseki's Graph Store direct naming on every dataset: a request to
        /// /{ds}/{path} that names no endpoint reads or writes the graph whose IRI is
        /// the request URL
        #[arg(long)]
        gsp_direct_naming: bool,
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
        /// [default: 0]
        #[arg(long, value_name = "UPDATE_TIMEOUT")]
        update_timeout: Option<f64>,
        /// Re-materialize stale inferences automatically once a dataset has had no
        /// commit for this many seconds (off by default; each run holds the writer lock)
        #[arg(long, value_name = "SECS")]
        auto_reason: Option<f64>,
        /// With --auto-reason: run at the latest this many seconds after the inferences
        /// became stale, even while writes continue (default: 12 x the debounce)
        #[arg(long, value_name = "SECS", requires = "auto_reason")]
        auto_reason_max_delay: Option<f64>,
        /// Keep the closure of each dataset's last materialization in memory, up to this
        /// many triples, so that the next run updates it incrementally (0: keep none; a
        /// run then reads it back from a persistent dataset)
        #[arg(long, value_name = "N", default_value_t = state::DEFAULT_REASON_CACHE_TRIPLES)]
        reason_cache_triples: usize,
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
        /// Clones that run at once, within --max-tasks; more wait, queued (0: only
        /// --max-tasks limits them)
        #[arg(long, default_value_t = state::DEFAULT_MAX_CLONES)]
        max_clones: usize,
        #[command(flatten)]
        auto_compact: compaction::AutoCompactArgs,
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
        /// Serve HTTPS with this PEM certificate chain (the server's certificate first;
        /// with --tls-key). HTTP/2 and HTTP/1.1 are negotiated through ALPN, and the files
        /// are read again on SIGHUP and when they change. Most deployments terminate TLS
        /// at a reverse proxy instead
        #[arg(long, value_name = "FILE", requires = "tls_key")]
        tls_cert: Option<PathBuf>,
        /// The PEM private key of --tls-cert (PKCS#8, PKCS#1 or SEC1)
        #[arg(long, value_name = "FILE", requires = "tls_cert")]
        tls_key: Option<PathBuf>,
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
    /// Lint SPARQL, Turtle and TriG: unused and undefined prefixes, unused and unbound
    /// variables, cartesian products, FILTER scope, language tags, datatypes and more
    /// (--fix applies the safe fixes)
    #[cfg(feature = "fmt")]
    Lint(fmt::lint::LintArgs),
    /// A language server for editors (stdio): formatting, syntax diagnostics, lint findings
    /// and their quick fixes for the languages `sparkles fmt` formats
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
        /// also index the literals of this language stemmed, for lang: searches (a
        /// primary tag such as en, or all)
        #[arg(long)]
        language: Vec<String>,
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
        /// geo:asGeoJSON, geo:asGML, geo:asKML, geo:hasSerialization)
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
    /// Bulk load RDF files, and CSV and TSV tables, into a database (creates it if
    /// needed)
    Load {
        #[arg(long, required_unless_present = "server")]
        loc: Option<PathBuf>,
        /// Load triples into this named graph
        #[arg(long)]
        graph: Option<String>,
        files: Vec<PathBuf>,
        #[command(flatten)]
        csv: csv_cmd::CsvArgs,
        /// Compression of the files: auto (magic bytes, then the extension), none, gzip,
        /// zstd, brotli or lz4
        #[arg(long, default_value = "auto")]
        compression: String,
        /// Skip the validation of IRIs and language tags, for data whose IRIs are not all
        /// valid (DBpedia's, for one); syntax errors still fail the load
        #[arg(long)]
        lenient: bool,
        /// Warn about suspicious IRIs and language tags before loading (scheme rules,
        /// percent-encoding, extlang, …), at the cost of a second parse of the files
        #[arg(long)]
        check: bool,
        /// With --check: load nothing when any IRI or language tag has a warning
        #[arg(long, requires = "check")]
        strict: bool,
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
        /// Output format: text, json, xml, csv, tsv, sparkles (graphs: ttl, nt, nq, trig, jsonld,
        /// rdfxml, trix, rt, rpb, rj)
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
        /// RDFS on read: match the RDFS closure of each graph with respect to the schema
        /// in this file, as Fuseki's --rdfs does
        #[arg(long, value_name = "FILE", conflicts_with = "rdfs_graph")]
        rdfs: Option<PathBuf>,
        /// RDFS on read with the schema in this graph of the database: `default` or an IRI
        #[arg(long, value_name = "GRAPH")]
        rdfs_graph: Option<String>,
        /// How DESCRIBE describes a resource: cbd, scbd or outgoing (default: the
        /// database's setting, see `describe-settings`)
        #[arg(long, value_name = "MODE")]
        describe: Option<String>,
        /// DESCRIBE adds the rdfs:label and skos:prefLabel of the IRIs it links to
        #[arg(long)]
        describe_labels: bool,
        /// DESCRIBE adds the reifiers of the triples it describes
        #[arg(long)]
        describe_reifiers: bool,
        /// DESCRIBE writes at most this many triples per query (it can only lower the
        /// dataset's limit)
        #[arg(long, value_name = "N", value_parser = clap::value_parser!(u64).range(1..))]
        describe_max_triples: Option<u64>,
        /// DESCRIBE follows blank nodes this many steps deep at most (it can only lower
        /// the dataset's limit)
        #[arg(long, value_name = "N", value_parser = clap::value_parser!(u32).range(1..))]
        describe_max_depth: Option<u32>,
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
    /// Apply RDF Patch files to a database, one commit per file, or send them to a
    /// server's patch endpoint
    Patch(patch_cmd::PatchArgs),
    /// Write the database, or a dataset on a server, in any RDF syntax (N-Quads by
    /// default), to stdout or a file
    Dump(dump_cmd::DumpArgs),
    /// Write-time validation of a database (SHACL, or ShEx with --lang shex): status,
    /// set, or turn off
    #[cfg(any(feature = "shacl", feature = "shex"))]
    Validation(validation_cmd::ValidationArgs),
    /// The storage quota of a persistent dataset: print it, set it (--max-mb), or go
    /// back to the default (--default)
    Quota(quota_cmd::QuotaArgs),
    /// Show or change a dataset's automatic compaction settings, on a local database or
    /// on a server
    Compaction(compaction_cmd::CompactionArgs),
    /// Ask a server whether it is ready (GET /$/ready), over HTTPS or HTTP: exit status 0
    /// when it answers 200. The container image's health check runs it.
    #[cfg(feature = "auth")]
    Ping(ping_cmd::PingArgs),
    /// Show or change how DESCRIBE describes a resource in a dataset (cbd, scbd or
    /// outgoing, labels, reifiers and limits), on a local database or on a server
    DescribeSettings(describe_cmd::DescribeArgs),
    /// Named snapshots (pins that keep a commit readable) and history retention
    Snapshot {
        #[command(subcommand)]
        cmd: SnapshotCmd,
    },
    /// Stored, parameterized queries of a database: list, get, put, delete and run them
    Queries {
        #[command(subcommand)]
        cmd: queries_cmd::QueriesCmd,
    },
    /// GraphQL over a database: run a document, and print, install, delete or draft
    /// the mapping schema
    #[cfg(feature = "graphql")]
    Graphql(graphql::GraphqlArgs),
    /// Merge updates into a freshly built index generation
    Compact {
        #[arg(long)]
        loc: PathBuf,
        /// Compact only when the dataset's compaction policy (its compaction.json and
        /// the defaults) says a compaction is due
        #[arg(long)]
        if_due: bool,
        /// Whether to rewrite only the index blocks the delta touches: auto, off or
        /// always (default: the dataset's partial setting, else auto)
        #[arg(long, value_name = "MODE")]
        partial: Option<String>,
    },
    /// Add the sparse vocabulary index (vocab.idx) to a database whose current index was
    /// built before it existed, so that a cold server looks up a term with one read
    /// instead of one per step of a binary search. A load or compaction writes it too.
    VocabIndex {
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
    /// Print the OpenAPI 3.1 description of the HTTP API (also served at
    /// /$/openapi.json and /$/openapi.yaml)
    Openapi {
        /// json or yaml
        #[arg(long, value_enum, default_value_t = openapi::OutputFormat::Json)]
        format: openapi::OutputFormat,
    },
    /// Print a shell completion script: bash, zsh, fish, elvish or powershell
    Completions(cli_docs::CompletionsArgs),
    /// Write man pages for sparkles and each of its subcommands
    Man(cli_docs::ManArgs),
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
        /// clone a past state: N, commit:N, time:<RFC 3339>, snapshot:NAME
        #[arg(long)]
        at: Option<String>,
        /// Copy only this graph (repeatable): `default`, a graph IRI, or an IRI pattern
        /// with `*`; the inferred graph is copied only when named
        #[arg(long = "graph", value_name = "GRAPH")]
        graphs: Vec<String>,
        /// `auto` shares the source's index files by reflink or copy when it has no
        /// changes since its last compaction, `link` uses hard links first, `rebuild`
        /// always rebuilds
        #[arg(long, default_value = "auto")]
        mode: String,
    },
    /// The branches of a database: list, create, show, delete, protect
    Branch {
        #[command(subcommand)]
        cmd: branch_cmd::BranchCmd,
    },
    /// Merge a branch into another (main by default); exits 2 when conflicts stopped it
    Merge(branch_cmd::MergeArgs),
    /// Undo a commit on the --branch branch (main by default) with a new commit; exits 2
    /// when conflicts stopped it
    Revert(branch_cmd::RevertArgs),
    /// Apply a commit of another branch to the --branch branch (main by default) with a
    /// new commit; exits 2 when conflicts stopped it
    CherryPick(branch_cmd::CherryPickArgs),
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
    /// The quads added and removed between two commits (N, commit:N, time:<RFC 3339>,
    /// snapshot:NAME, head)
    Diff {
        #[arg(long)]
        loc: PathBuf,
        from: String,
        #[arg(default_value = "head")]
        to: String,
        /// only this graph (an IRI, or `default`)
        #[arg(long)]
        graph: Option<String>,
        /// diff (lines marked + and -), json, count, patch (RDF Patch) or
        /// patch-binary (RDF Patch in RDF Thrift)
        #[arg(long, default_value = "diff")]
        format: String,
    },
    /// The recorded changes of a range of commits, from the change log: the quads added
    /// and removed, and the commit, time and author of each change
    History {
        #[arg(long)]
        loc: PathBuf,
        /// a subject (an IRI, or an N-Triples term); repeat for several
        #[arg(long)]
        subject: Vec<String>,
        /// a predicate IRI; repeat for several
        #[arg(long)]
        predicate: Vec<String>,
        /// an object (an IRI, or an N-Triples term such as '"Ann"@en'); repeat for several
        #[arg(long)]
        object: Vec<String>,
        /// a graph IRI, or `default`; repeat for several
        #[arg(long)]
        graph: Vec<String>,
        /// the first commit read (N, commit:N, time:<RFC 3339>, snapshot:NAME, head)
        #[arg(long)]
        from: Option<String>,
        /// the last commit read (by default the head)
        #[arg(long)]
        to: Option<String>,
        /// only additions (add) or only removals (remove)
        #[arg(long)]
        op: Option<String>,
        /// the most changes listed
        #[arg(long, default_value_t = 100)]
        limit: usize,
        /// newest commits first
        #[arg(long)]
        desc: bool,
        /// text or json
        #[arg(long, default_value = "text")]
        format: String,
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
        /// Materialize in full instead of updating the previous materialization
        /// incrementally
        #[arg(long, conflicts_with = "clear")]
        full: bool,
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
        /// Check the merge of these graphs instead of the default graph: `default` or a
        /// graph IRI (repeatable). The inferences are included only with `default`
        #[arg(long = "graph", value_name = "GRAPH", requires = "check")]
        graphs: Vec<String>,
        /// `subclass` (type tests follow rdfs:subClassOf*) or `none`
        #[arg(long, default_value = "subclass", requires = "check")]
        closure: String,
        /// text or json (with --check also turtle)
        #[arg(long, default_value = "text")]
        format: String,
        /// Timeout of the checks in seconds
        #[arg(long, requires = "check")]
        timeout: Option<f64>,
        /// A graph whose triples the rules read: `default` or a graph IRI (repeatable;
        /// default: the default graph). Without input options, a run reads the graphs
        /// the recorded status names
        #[arg(long = "data-graph", value_name = "GRAPH")]
        data_graphs: Vec<String>,
        /// A graph that holds the ontology, read like the data graphs (repeatable)
        #[arg(long = "ontology-graph", value_name = "GRAPH")]
        ontology_graphs: Vec<String>,
        /// What to do with owl:imports: `none`, `dataset` (follow them to graphs of the
        /// database, the default) or `fetch` (also load the missing ones, as LOAD does)
        #[arg(long, value_name = "MODE")]
        imports: Option<String>,
        /// A Jena location-mapping file (lm:name/lm:altName, lm:prefix/lm:altPrefix) for
        /// the imports
        #[arg(long, value_name = "FILE")]
        location_mapping: Option<PathBuf>,
        /// Load again the imports that earlier runs fetched
        #[arg(long)]
        refresh_imports: bool,
        // where fetched imports may come from
        #[command(flatten)]
        outbound: outbound::OutboundArgs,
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
        /// List, for each predicate, the classes of its subjects with their triples
        #[arg(long, conflicts_with = "draft_shapes")]
        subject_classes: bool,
        /// SHACL shapes for the constraints layer: `guard` (the database's write-time
        /// validation), `default`, a graph IRI, or `none` (repeatable; default: the
        /// write-time validation's shapes, if it has SHACL validation)
        #[arg(long, value_name = "SOURCE", conflicts_with = "draft_shapes")]
        shapes: Vec<String>,
        /// Draft SHACL shapes (or, with --format shexc, a ShEx schema) from the data
        /// instead of printing the schema; --format is then turtle, shaclc (the SHACL
        /// Compact Syntax), shexc or json
        #[arg(long)]
        draft_shapes: bool,
        /// Draft a constraint when at least this share of the instances it applies to
        /// satisfy it, in (0, 1]
        #[arg(long, default_value_t = 1.0, requires = "draft_shapes")]
        support: f64,
        /// Draft closed shapes
        #[arg(long, requires = "draft_shapes")]
        closed: bool,
        /// Largest sh:in list (0: none)
        #[arg(long, default_value_t = sparkles::schema::draft::DEFAULT_MAX_IN, requires = "draft_shapes")]
        max_in: usize,
        /// Largest sh:maxCount drafted (0: none)
        #[arg(long, default_value_t = sparkles::schema::draft::DEFAULT_MAX_COUNT, requires = "draft_shapes")]
        max_count: u64,
        /// Draft or profile only this class (IRI; repeatable)
        #[arg(long, value_name = "IRI")]
        class: Vec<String>,
        /// Skip classes with fewer instances
        #[arg(long, default_value_t = 1, requires = "draft_shapes")]
        min_instances: u64,
        /// Draft from the data with its materialized inferences (drafts leave them out
        /// by default, as write-time validation does)
        #[arg(long, requires = "draft_shapes", conflicts_with = "no_inferences")]
        with_inferences: bool,
        /// Print per-class property profiles instead of the schema: the predicates the
        /// instances of each class (or of each --class) use, and the predicates that point
        /// at them; --format is text or json
        #[arg(long, conflicts_with_all = ["draft_shapes", "subject_classes", "diff"])]
        profiles: bool,
        /// Print what changed in the schema since this state instead of the schema: a
        /// commit (`N` or `commit:N`), `time:<RFC 3339>` or `snapshot:<name>`; --format is
        /// text or json
        #[arg(long, value_name = "AT", conflicts_with_all = ["draft_shapes", "subject_classes"])]
        diff: Option<String>,
        /// The later state of --diff (default: the head)
        #[arg(long, value_name = "AT", requires = "diff")]
        to: Option<String>,
    },
    /// Validate a database (or data files) against a SHACL shapes graph; exits with
    /// status 1 when the data does not conform. `sparkles shacl parse` prints shapes
    /// files in another syntax
    #[command(args_conflicts_with_subcommands = true, subcommand_negates_reqs = true)]
    Shacl {
        #[command(subcommand)]
        cmd: Option<ShaclCmd>,
        /// Database directory
        #[arg(long)]
        loc: Option<PathBuf>,
        /// Data files to validate (loaded into memory)
        #[arg(long)]
        data: Vec<PathBuf>,
        /// Shapes graph file (Turtle, N-Triples, RDF/XML, JSON-LD, ..., or SHACLC as
        /// `.shaclc` or `.shc`; `.gz` allowed)
        #[arg(long, required = true)]
        shapes: Option<PathBuf>,
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
    // convert (riot), qparse, uparse, compare (rdfdiff), iri, langtag, rsparql, rupdate,
    // rset
    #[command(flatten)]
    Tools(tools::ToolCmd),
    /// Configurations of other servers: import one as serve flags, dataset settings and
    /// an auth configuration, or check what converts
    ///
    /// Sources: fuseki (Apache Jena Fuseki's config.ttl, the service files of
    /// run/configuration/, and shiro.ini or the password file of fuseki:passwd).
    Config(config_cmd::ConfigArgs),
    /// CSV and TSV tables: convert them to RDF without loading, or print the CSVW
    /// metadata of the default mapping
    Csv(csv_cmd::CsvCmdArgs),
}

fn store_opts(cli: &Cli) -> StoreOptions {
    sparkles::vector::set_budget(cli.vector_memory_mb << 20);
    StoreOptions {
        cache_bytes: cli.cache_mb << 20,
        result_cache_bytes: cli.result_cache_mb << 20,
        service_cache_bytes: cli.service_cache_mb << 20,
        union_default_graph: cli.union_default_graph,
        history_cache_bytes: cli.history_cache_mb << 20,
        history_max_generations: cli.history_max_generations,
        max_snapshots: cli.max_snapshots,
        max_branches: cli.max_branches,
        max_branch_depth: cli.max_branch_depth,
        max_prefixes: cli.max_prefixes,
        commit_digests: cli.commit_digests,
        wal_prealloc_bytes: cli.wal_prealloc_kb << 10,
        change_log: !cli.no_change_log,
        change_log_max_bytes: cli.change_log_mb << 20,
        ..Default::default()
    }
}

/// `serve --validate NAME[=CONFIG]`
#[cfg(any(feature = "shacl", feature = "shex"))]
fn validate_at_startup(st: &state::AppState, spec: &str) -> Result<()> {
    let line = write_validation::validate_at_startup(st, spec)?;
    tracing::info!("{line}");
    Ok(())
}

#[cfg(not(any(feature = "shacl", feature = "shex")))]
fn validate_at_startup(_: &state::AppState, _: &str) -> Result<()> {
    bail!("built without write-time validation (cargo features \"shacl\" and \"shex\")")
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
#[allow(clippy::too_many_arguments)]
fn text_index(
    loc: &std::path::Path,
    opts: StoreOptions,
    predicates: Vec<String>,
    exclude_graph: Vec<String>,
    languages: Vec<String>,
    rebuild: bool,
    status: bool,
    disable: bool,
) -> Result<()> {
    use sparkles::text::{Languages, PredicateSet, TextConfig};
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
    let configured = !predicates.is_empty() || !exclude_graph.is_empty() || !languages.is_empty();
    let s = match store.text_status() {
        Some(_) if !configured && rebuild => store.rebuild_text()?,
        Some(s) if !configured => s,
        _ => {
            let mut cfg = TextConfig::default();
            if !predicates.is_empty() {
                cfg.predicates = PredicateSet::Only(predicates);
            }
            cfg.graphs.exclude = exclude_graph;
            cfg.languages = Languages::from_tags(&languages).map_err(anyhow::Error::msg)?;
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
#[allow(clippy::too_many_arguments)]
fn text_index(
    _: &std::path::Path,
    _: StoreOptions,
    _: Vec<String>,
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
    // commits a point-in-time read can still see
    let readable = sparkles::history::reconstructable_offline(loc, id).unwrap_or_default();
    let readable_at = |s: u64| readable.iter().any(|&(a, b)| a <= s && s <= b);
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
            "reconstructable": readable
                .iter()
                .map(|(a, b)| serde_json::json!({ "from": a, "to": b }))
                .collect::<Vec<_>>(),
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
    println!("dataset {id}  head {head}  (* readable with --at)");
    println!(
        "{:>7}  {:<24}  {:<12} {:>10} {:>10} {:>12}  generation  message",
        "seq", "timestamp", "kind", "+inserted", "-deleted", "quads"
    );
    for c in pick {
        let approx = if c.exact { "" } else { "~" };
        let message = notes
            .get(&c.seq)
            .and_then(|a| a.message.as_deref())
            .map(|m| format!("  {m}"))
            .unwrap_or_default();
        // a write that bypassed the dataset's write-time validation
        let message = if c.unvalidated {
            format!("  [unvalidated]{message}")
        } else {
            message
        };
        let line = format!(
            "{:>6}{}  {:<24}  {:<12} {:>10} {:>10} {:>12}  {:<10}{message}",
            c.seq,
            if readable_at(c.seq) { "*" } else { " " },
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
fn open_for_write(
    loc: &std::path::Path,
    opts: StoreOptions,
    no_validate: bool,
) -> Result<branch_cmd::Db> {
    let mut opts = opts;
    if no_validate {
        opts.unvalidated_writes = true;
    }
    let store = branch_cmd::open_db(loc, opts)?;
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
    branch_cmd::set_branch(cli.branch.clone());
    // the commands that work on a branch of a database
    if branch_cmd::branch().is_some()
        && !matches!(
            cli.cmd,
            Cmd::Query { .. }
                | Cmd::Update { .. }
                | Cmd::Load { .. }
                | Cmd::Dump(_)
                | Cmd::Log { .. }
                | Cmd::Diff { .. }
                | Cmd::Snapshot { .. }
                | Cmd::Compact { .. }
                | Cmd::Stats { .. }
                | Cmd::Clone { .. }
                | Cmd::Patch(_)
                | Cmd::Revert(_)
                | Cmd::CherryPick(_)
        )
    {
        bail!("this command does not take --branch");
    }
    // bulk loads merge a file per batch, and a server holds the files of every dataset
    sparkles::disk::raise_open_file_limit();
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
    if let Some(path) = &cli.geo_crs {
        geo::register_crs_file(path)?;
    }
    let opts = store_opts(&cli);
    let no_validate = cli.no_validate;
    match cli.cmd {
        Cmd::Serve {
            data,
            host,
            port,
            mut mem,
            mut loc,
            fuseki_config,
            timeout,
            max_timeout,
            mut read_only,
            no_service,
            embedding_secret,
            no_embedding,
            outbound,
            load_dir,
            idle_release_ms,
            mut text,
            mut geo,
            validate,
            mut rdfs,
            geo_mb,
            geo_op_vertices,
            no_geo_rewrite,
            map_style_url,
            schema_max_entries,
            #[cfg(feature = "graphql")]
            graphql,
            mut gsp_direct_naming,
            no_access_log,
            no_metrics,
            metrics_max_datasets,
            mut metrics_fuseki_names,
            metrics_addr,
            query_memory_mb,
            max_result_mb,
            max_export_mb,
            max_rows,
            max_rows_produced,
            update_timeout,
            allow_unvalidated_writes,
            mut auto_reason,
            auto_reason_max_delay,
            reason_cache_triples,
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
            max_clones,
            auto_compact,
            max_mem_dataset_mb,
            max_dataset_mb,
            shutdown_grace,
            mut auth_config,
            #[cfg(feature = "backup")]
            backup_config,
            #[cfg(feature = "backup")]
            backup_max_tasks,
            unix_socket,
            tls_cert,
            tls_key,
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
            // a Fuseki configuration adds datasets, settings and access rules; the flags
            // given on the command line win
            let mut fuseki = None;
            let (mut timeout, mut update_timeout) = (timeout, update_timeout);
            if let Some(path) = &fuseki_config {
                let f = fuseki_config::for_serve(path, &data, auth_config.is_some())?;
                mem.extend(f.mem.iter().cloned());
                loc.extend(f.loc.iter().cloned());
                text.extend(f.text.iter().cloned());
                geo.extend(f.geo.iter().cloned());
                rdfs.extend(f.rdfs.iter().cloned());
                timeout = timeout.or(f.timeout);
                update_timeout = update_timeout.or(f.update_timeout);
                read_only |= f.read_only;
                gsp_direct_naming |= f.gsp_direct_naming;
                metrics_fuseki_names |= f.metrics_fuseki_names && !no_metrics;
                if auth_config.is_none() {
                    auth_config = f.auth_config.clone();
                }
                if f.auto_reason && auto_reason.is_none() {
                    auto_reason = Some(5.0);
                }
                fuseki = Some(f);
            }
            let timeout = timeout.unwrap_or(60.0);
            let update_timeout = update_timeout.unwrap_or(0.0);
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
            if tls_cert.is_some() && unix_socket.is_some() {
                bail!("--tls-cert applies to the TCP listener, not to --unix-socket");
            }
            // a certificate that does not load stops the server before it binds
            #[cfg(feature = "tls")]
            let certs = match (&tls_cert, &tls_key) {
                (Some(c), Some(k)) => Some(tls::Certs::open(c, k)?),
                _ => None,
            };
            #[cfg(not(feature = "tls"))]
            if tls_cert.is_some() || tls_key.is_some() {
                bail!("--tls-cert: built without native TLS (cargo feature \"tls\")");
            }
            // one server per data directory (held until the process exits)
            #[cfg(feature = "backup")]
            let _data_lock = backup::lock_data_dir(&data)?;
            let bound = if unix_socket.is_some() { "unix" } else { &host };
            let auth = auth::load(auth_config.as_deref(), &data, bound, tls_cert.is_some())?;
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
            if fuseki.as_ref().is_some_and(|f| f.union_default_graph) {
                opts.union_default_graph = true;
            }
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
            // embedding requests go through the same outbound policy
            sparkles::vector::embed::set_environment(sparkles::vector::embed::Environment {
                enabled: !no_embedding,
                outbound: st.outbound.clone(),
                secrets: vector::parse_secrets(&embedding_secret)?,
            });
            st.file_loads = outbound::file_loads(load_dir.as_deref(), &data)?;
            st.schema_max_entries = schema_max_entries;
            #[cfg(feature = "graphql")]
            {
                st.graphql_limits = graphql.limits();
            }
            st.gsp_direct_naming = gsp_direct_naming;
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
            st.task_queue.set_kind_max("clone", max_clones);
            st.compaction = auto_compact.state()?;
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
            st.reason_cache_triples = reason_cache_triples;
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
            if !st.read_only {
                compaction::spawn(st.clone());
                // the embedding workers of datasets whose vector indexes compute vectors
                vector::spawn_embedders(st.clone());
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
            for v in validate {
                validate_at_startup(&st, &v)?;
            }
            for r in rdfs {
                rdfs::configure(&st, &r)?;
            }
            if let Some(f) = &fuseki {
                fuseki_config::start(&st, f)?;
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
            // The server runs as a task on a worker thread, not on this thread (which
            // `block_on` would use): the worker that sees a connection arrive accepts it
            // and runs its request itself, instead of waking this thread to accept and
            // then another worker to serve it.
            let serve = async move {
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
                let scheme = if tls_cert.is_some() { "https" } else { "http" };
                let listening = match &unix_socket {
                    Some(p) => format!("unix:{}", p.display()),
                    None => format!("{scheme}://{addr}/"),
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
                // pin expiry, scheduled pins, and history that ages out of the window
                if !st.read_only {
                    http::history::spawn_tick(st.clone(), Duration::from_secs(60));
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
                    tracing::info!("shutting down: finishing requests in flight (up to {grace:?})");
                    let _ = draining_tx.send(());
                };
                // over TLS the connection's handshakes run beside the accept loop, and
                // requests say they came over https
                #[cfg(feature = "tls")]
                let (tls, tcp) = match (certs, tcp) {
                    (Some(c), Some(l)) => {
                        tls::spawn_reload(c.clone());
                        (
                            Some(tls::TlsListener::new(l, tls::server_config(c)?)?),
                            None,
                        )
                    }
                    (_, l) => (None, l),
                };
                #[cfg(feature = "tls")]
                if let Some(l) = tls {
                    let app = app.layer(axum::middleware::map_request(tls::mark_https));
                    let service = app.into_make_service_with_connect_info::<auth::Peer>();
                    use std::future::IntoFuture;
                    let serve = axum::serve(l, service).with_graceful_shutdown(shutdown);
                    let drained = shutdown::drain(serve.into_future(), draining, grace).await?;
                    if drained == shutdown::Drained::GraceElapsed {
                        tracing::warn!(
                            "shutdown grace of {grace:?} elapsed: cancelling the requests still in flight"
                        );
                    }
                    return anyhow::Ok(());
                }
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
            };
            let served = rt.block_on(async move {
                tokio::spawn(serve)
                    .await
                    .unwrap_or_else(|e| Err(anyhow::anyhow!("the server task failed: {e}")))
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
        Cmd::Lint(args) => fmt::lint::run(args),
        #[cfg(feature = "fmt")]
        Cmd::Lsp(args) => lsp::run(args),
        Cmd::Openapi { format } => openapi::print(format),
        Cmd::Completions(args) => {
            cli_docs::completions(args, <Cli as clap::CommandFactory>::command())
        }
        Cmd::Man(args) => cli_docs::man(args, <Cli as clap::CommandFactory>::command()),
        Cmd::Shex(args) => shex_cmd::run(args, opts),
        Cmd::Tools(cmd) => tools::run(cmd, opts),
        Cmd::Config(args) => config_cmd::run(args),
        Cmd::Csv(args) => csv_cmd::run(args),
        Cmd::Load {
            loc,
            graph,
            files,
            csv,
            compression,
            lenient,
            check,
            strict,
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
            // tables become temporary N-Triples files, kept until the load is done
            let prepared = csv_cmd::prepare(&files, &csv)?;
            let files = &prepared.files;
            // the term checks read the files once before anything is written
            if check {
                let explicit = match compression.as_str() {
                    "auto" => None,
                    c => Some(sparkles::codec::Codec::parse(c)?),
                };
                tools::convert::precheck(files, explicit, lenient, strict)?;
            }
            let Some(loc) = loc else {
                if lenient {
                    bail!("--lenient applies to a local database (--loc) only");
                }
                let ds = remote_dataset(server.as_deref(), dataset.as_deref())?;
                #[cfg(feature = "auth")]
                return remote::client::load(
                    server.as_deref(),
                    insecure_http,
                    &ds,
                    graph.as_deref(),
                    files,
                    message.as_deref(),
                );
                #[cfg(not(feature = "auth"))]
                return no_remote(&ds, insecure_http);
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
                .zip(&prepared.names)
                .zip(&prepared.converted)
                .map(|((f, name), &converted)| -> Result<Source> {
                    let mut s = Source::from_path(f, g.clone())?;
                    s.name = name.clone();
                    // a converted table is plain N-Triples
                    s.compression = if converted { None } else { explicit };
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
            rdfs,
            rdfs_graph,
            describe,
            describe_labels,
            describe_reifiers,
            describe_max_triples,
            describe_max_depth,
            outbound,
        } => {
            let q = match (query, text) {
                (Some(f), _) => std::fs::read_to_string(f)?,
                (None, Some(t)) => t,
                _ => bail!("no query given"),
            };
            if loc.is_none() && data.is_empty() && server.is_some() {
                let ds = remote_dataset(server.as_deref(), dataset.as_deref())?;
                // the server's DESCRIBE request parameters, over the dataset's setting
                let mut params: Vec<(&str, String)> = Vec::new();
                if let Some(m) = &describe {
                    sparkles::sparql::describe::DescribeMode::parse(m)?;
                    params.push(("describe", m.clone()));
                }
                if describe_labels {
                    params.push(("describe-labels", "true".into()));
                }
                if describe_reifiers {
                    params.push(("describe-reifiers", "true".into()));
                }
                if let Some(n) = describe_max_triples {
                    params.push(("describe-max-triples", n.to_string()));
                }
                if let Some(n) = describe_max_depth {
                    params.push(("describe-max-depth", n.to_string()));
                }
                #[cfg(feature = "auth")]
                return remote::client::query(
                    server.as_deref(),
                    insecure_http,
                    &ds,
                    &q,
                    &fmt,
                    timeout,
                    explain,
                    &params,
                );
                #[cfg(not(feature = "auth"))]
                return no_remote(&ds, insecure_http);
            }
            let outbound = outbound.local_policy()?;
            let store = open_or_load(loc, &data, opts)?;
            use sparkles::sparql::rdfs::{RdfsOnRead, RdfsSchema, SchemaSource};
            let rdfs = match (rdfs, rdfs_graph) {
                (Some(f), _) => Some(RdfsOnRead::fixed(RdfsSchema::from_triples(
                    &rdfs::read_file(&f).with_context(|| format!("--rdfs {}", f.display()))?,
                ))),
                (None, Some(g)) => Some(RdfsOnRead::new(SchemaSource::Graph(
                    (g != "default").then_some(g),
                ))),
                (None, None) => None,
            };
            let qopts = QueryOptions {
                timeout: timeout.map(Duration::from_secs_f64),
                max_memory_bytes: (memory_mb > 0).then_some(memory_mb << 20),
                allow_service: true,
                outbound,
                prefixes: store.prefixes().into_iter().collect(),
                rdfs: rdfs.map(Arc::new),
                describe: {
                    let mut d = store.describe_settings();
                    if let Some(m) = &describe {
                        d.mode = sparkles::sparql::describe::DescribeMode::parse(m)?;
                    }
                    d.labels |= describe_labels;
                    d.reifiers |= describe_reifiers;
                    d.lowered(describe_max_triples, describe_max_depth)
                },
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
                    // Jena's TriX, RDF Thrift, RDF Protobuf and RDF/JSON
                    let jena = f
                        .is_none()
                        .then(|| http::jena_formats::JenaFormat::from_name(&fmt))
                        .flatten();
                    match jena {
                        Some(j) => results::write_jena_graph(&r, j, &mut out)?,
                        None => results::write_graph(
                            &r,
                            f.context("unknown RDF format")?,
                            &store.prefixes(),
                            &mut out,
                        )?,
                    }
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
                    &ds,
                    &u,
                    message.as_deref(),
                );
                #[cfg(not(feature = "auth"))]
                return no_remote(&ds, insecure_http);
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
        Cmd::Patch(a) => patch_cmd::run(a, opts, no_validate),
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
        } => geo_index_cmd::run(
            &loc,
            opts,
            geo_index_cmd::IndexArgs {
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
            language,
            rebuild,
            status,
            disable,
        } => text_index(
            &loc,
            opts,
            predicate,
            exclude_graph,
            language,
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
        } => {
            // a branch's own commits, from its directory's files
            let loc = match branch_cmd::branch() {
                Some(b) => branch_cmd::branch_dir(&loc, b)?,
                None => loc,
            };
            print_log(&loc, limit, before, after, at.as_deref(), &format)
        }
        Cmd::Dump(args) => dump_cmd::run(args, opts),
        Cmd::Snapshot { cmd } => snapshot_cmd(cmd, opts),
        Cmd::Queries { cmd } => queries_cmd::run(cmd, opts),
        #[cfg(feature = "graphql")]
        Cmd::Graphql(args) => graphql::run(args, opts),
        Cmd::Diff {
            loc,
            from,
            to,
            graph,
            format,
        } => diff_cmd(&loc, &from, &to, graph.as_deref(), &format, opts),
        Cmd::History {
            loc,
            subject,
            predicate,
            object,
            graph,
            from,
            to,
            op,
            limit,
            desc,
            format,
        } => history_cmd(
            &loc,
            HistoryArgs {
                subject,
                predicate,
                object,
                graph,
                from,
                to,
                op,
                limit,
                desc,
                format,
            },
            opts,
        ),
        #[cfg(any(feature = "shacl", feature = "shex"))]
        Cmd::Validation(args) => validation_cmd::run(args, opts),
        Cmd::Quota(args) => quota_cmd::run(args, opts),
        Cmd::Compaction(args) => compaction_cmd::run(args, opts),
        #[cfg(feature = "auth")]
        Cmd::Ping(args) => ping_cmd::run(args),
        Cmd::VocabIndex { loc } => {
            let store = Store::open(&loc, opts)?;
            match store.add_vocab_index()? {
                Some(n) => println!("wrote vocab.idx ({n} entries)"),
                None => println!("the current index already has vocab.idx"),
            }
            Ok(())
        }
        Cmd::DescribeSettings(args) => describe_cmd::run(args, opts),
        Cmd::Compact {
            loc,
            if_due,
            partial,
        } => {
            let partial = partial
                .map(|p| {
                    sparkles::store::PartialMode::parse(&p).with_context(|| {
                        format!("--partial expects auto, off or always, not {p:?}")
                    })
                })
                .transpose()?;
            let store = branch_cmd::open_db(&loc, opts)?;
            if if_due {
                let policy =
                    sparkles::store::CompactionPolicy::default().with(&store.compaction_settings());
                match policy.verdict(&store.compaction_measures()) {
                    Some(t) if policy.enabled => eprintln!("due: {}", t.detail),
                    Some(_) => {
                        eprintln!("not compacted: automatic compaction is off for this dataset");
                        return Ok(());
                    }
                    None => {
                        eprintln!("not compacted: the compaction policy says nothing is due");
                        return Ok(());
                    }
                }
            }
            let t = Instant::now();
            let rep = store.compact_with(&sparkles::store::CompactOptions {
                partial,
                ..Default::default()
            })?;
            eprintln!(
                "compacted into {} in {:.2}s{}",
                store.snapshot().generation.name,
                t.elapsed().as_secs_f64(),
                match &rep.full_reason {
                    Some(why) if rep.mode == "full" => format!(" (full: {why})"),
                    _ => compaction::partial_note(&rep),
                }
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
            at,
            graphs,
            mode,
        } => {
            let at: Option<sparkles::history::At> = at.as_deref().map(str::parse).transpose()?;
            let inferences = clone::Inferences::parse(&inferences).with_context(|| {
                format!("--inferences must be copy or drop, not '{inferences}'")
            })?;
            let mode = sparkles::store::CloneMode::parse(&mode)
                .with_context(|| format!("--mode must be auto, link or rebuild, not '{mode}'"))?;
            if let Some(g) = graphs.iter().find(|g| !clone::valid_graph_name(g)) {
                bail!("--graph '{g}' is not default, an absolute IRI or an IRI pattern");
            }
            let spec = clone::Spec {
                inferences,
                graphs: (!graphs.is_empty()).then_some(graphs),
                mode,
                at,
            };
            if !loc.join("CURRENT").exists() {
                bail!("{} is not a Sparkles database (no CURRENT)", loc.display());
            }
            if to.exists() && std::fs::read_dir(&to)?.next().is_some() {
                bail!("{} exists and is not empty", to.display());
            }
            let store = branch_cmd::open_db(&loc, opts)?;
            let t = Instant::now();
            clone::sweep_cli_leftovers(&to);
            let mut tmp = to.as_os_str().to_owned();
            tmp.push(format!(".clone-tmp-{}", std::process::id()));
            let r = clone::clone_into(
                &store,
                &loc.display().to_string(),
                state::read_reasoning_file(&loc),
                std::path::Path::new(&tmp),
                &to,
                &spec,
                None,
                None,
            )?;
            eprintln!(
                "cloned {} (commit {}, {} quads, {} graph{}) to {} by {} in {:.2}s",
                loc.display(),
                r.forked_from.seq,
                r.quads,
                r.graphs,
                if r.graphs == 1 { "" } else { "s" },
                to.display(),
                r.method.name(),
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
        Cmd::Branch { cmd } => branch_cmd::run_branch(cmd, opts),
        Cmd::Merge(a) => {
            let code = branch_cmd::run_merge(a, opts)?;
            if code != 0 {
                std::process::exit(code);
            }
            Ok(())
        }
        Cmd::Revert(a) => {
            let pick = branch_cmd::Pick::Revert { commit: a.commit };
            let code = branch_cmd::run_pick(pick, a.pick, opts)?;
            if code != 0 {
                std::process::exit(code);
            }
            Ok(())
        }
        Cmd::CherryPick(a) => {
            let pick = branch_cmd::Pick::CherryPick {
                source: a.source,
                commit: a.commit,
            };
            let code = branch_cmd::run_pick(pick, a.pick, opts)?;
            if code != 0 {
                std::process::exit(code);
            }
            Ok(())
        }
        Cmd::Stats { loc } => {
            let store = branch_cmd::open_db(&loc, opts)?;
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
            full,
            status,
            check,
            checks,
            limit,
            no_inferences,
            graphs,
            closure,
            format,
            timeout,
            data_graphs,
            ontology_graphs,
            imports,
            location_mapping,
            refresh_imports,
            outbound,
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
            let graphs = match diagnostics::parse_graphs(&graphs) {
                Ok(g) => g,
                Err(bad) => {
                    eprintln!("error: --graph must be default or an absolute IRI, not '{bad}'");
                    std::process::exit(2);
                }
            };
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
                // the previous materialization, updated incrementally when it can be
                let previous = state::read_reasoning_file(&loc);
                let since = if full {
                    None
                } else {
                    reasoning::incremental_since(previous.as_ref(), &store)
                };
                // the input graphs: as given, else as recorded
                let given = !data_graphs.is_empty()
                    || !ontology_graphs.is_empty()
                    || imports.is_some()
                    || location_mapping.is_some();
                let inputs = if given {
                    let graphs = |v: &[String]| {
                        v.iter()
                            .map(|g| sparkles_reasoner::GraphRef::parse(g))
                            .collect::<Result<Vec<_>, _>>()
                            .map_err(|e| anyhow::anyhow!(e))
                    };
                    let mut i = sparkles_reasoner::Inputs::default();
                    if !data_graphs.is_empty() {
                        i.data_graphs = graphs(&data_graphs)?;
                    }
                    i.ontology_graphs = graphs(&ontology_graphs)?;
                    if let Some(m) = &imports {
                        i.imports = m.parse().map_err(|e: String| anyhow::anyhow!(e))?;
                    }
                    if let Some(f) = &location_mapping {
                        let (format, _) = sparkles::io::format_for_path(f)
                            .with_context(|| format!("{}: unknown RDF format", f.display()))?;
                        i.location_mapping = sparkles_reasoner::LocationMapping::from_jena(
                            &std::fs::read(f)?,
                            format,
                        )?;
                    }
                    i
                } else {
                    match &previous {
                        Some(p) => reasoning::recorded_inputs(p)?,
                        None => Default::default(),
                    }
                };
                inputs.validate()?;
                let fetched_before: Vec<String> = previous
                    .as_ref()
                    .map(|p| p.fetched_imports.clone())
                    .unwrap_or_default();
                let mut fetched = fetched_before.clone();
                if inputs.imports == sparkles_reasoner::ImportMode::Fetch || refresh_imports {
                    let qopts = QueryOptions {
                        outbound: outbound.local_policy()?,
                        ..Default::default()
                    };
                    let refresh = if refresh_imports {
                        fetched_before
                    } else {
                        Vec::new()
                    };
                    let f = sparkles_reasoner::fetch_imports(&store, &inputs, &refresh, &qopts)?;
                    for i in &f.fetched {
                        eprintln!("fetched owl:imports <{i}>");
                    }
                    for w in &f.warnings {
                        eprintln!("warning: {w}");
                    }
                    fetched.extend(f.fetched);
                }
                let r = sparkles_reasoner::materialize_incremental(
                    &store,
                    &profile,
                    &extras,
                    sparkles_reasoner::Incremental { since, cache: None },
                    &sparkles_reasoner::ReasonOptions {
                        inputs: inputs.clone(),
                        ..Default::default()
                    },
                )?;
                // lets `sparkles serve` pick the inferences up for this database, with
                // the database's automatic re-run setting kept
                let mut info = reasoning::recorded(&profile, &extras, &r, &store);
                reasoning::record_inputs(&mut info, &inputs, &r, fetched);
                info.auto = previous.and_then(|i| i.auto);
                state::write_reasoning_file(&loc, Some(&info))?;
                eprintln!(
                    "{} inferred triples ({} rules, {} ms; {}) → graph <{}>{}",
                    r.inferred,
                    r.rules,
                    r.millis,
                    reasoning::run_text(&r),
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
            let default_checked =
                graphs.is_empty() || graphs.iter().any(|g| g == diagnostics::DEFAULT_GRAPH);
            let dopts = DiagnoseOptions {
                checks,
                limit,
                inferences: has_inferred && !no_inferences && default_checked,
                graphs,
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
            subject_classes,
            shapes,
            draft_shapes,
            support,
            closed,
            max_in,
            max_count,
            class,
            min_instances,
            with_inferences,
            profiles,
            diff,
            to,
        } => {
            use sparkles::index::Perm;
            if !class.is_empty() && !draft_shapes && !profiles {
                bail!("--class goes with --draft-shapes or --profiles");
            }
            use sparkles::schema::{GraphSelection, Page, SchemaError, SchemaOptions};
            if draft_shapes {
                return schema_draft(
                    loc,
                    &data,
                    opts,
                    SchemaDraftArgs {
                        graph,
                        format,
                        timeout,
                        max_entries,
                        support,
                        closed,
                        max_in,
                        max_count,
                        classes: class,
                        min_instances,
                        with_inferences,
                    },
                );
            }
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
                graphs: None,
                subject_classes,
            };
            if profiles {
                return schema_profiles(&snap, sopts, class, &format);
            }
            if let Some(from) = diff {
                return schema_diff(&store, &sopts, &from, to.as_deref(), &format);
            }
            let report = match sparkles::schema::discover(&snap, &sopts) {
                Ok(r) => r,
                Err(e @ (SchemaError::Timeout { .. } | SchemaError::TooManyEntries { .. })) => {
                    eprintln!("error: {e}");
                    std::process::exit(2);
                }
                Err(e) => return Err(e.into()),
            };
            // the constraints layer, for the JSON and text reports
            let constraints = match void {
                None => schema_constraints(&store, &snap, &shapes)?,
                Some(_) => None,
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
                let summary = summary.with_constraints(constraints.as_ref());
                serde_json::to_writer_pretty(&mut out, &summary)?;
                writeln!(out)?;
            } else {
                print_schema(&mut out, &report)?;
                if let Some(c) = &constraints {
                    print_constraints(&mut out, c)?;
                }
            }
            out.flush()?;
            Ok(())
        }
        #[cfg(not(feature = "shacl"))]
        Cmd::Shacl { .. } => bail!("built without the `shacl` feature"),
        #[cfg(feature = "shacl")]
        Cmd::Shacl {
            cmd:
                Some(ShaclCmd::Parse {
                    files,
                    out,
                    input,
                    base,
                }),
            ..
        } => shacl_parse(&files, &out, input.as_deref(), base.as_deref()),
        #[cfg(feature = "shacl")]
        Cmd::Shacl {
            cmd: None,
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
            let shapes = read_shapes(&shapes.context("--shapes is required")?)?;
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

/// The constraints layer of `sparkles schema`: the sources of `--shapes`, or else the
/// shapes of the database's write-time SHACL validation, if any.
fn schema_constraints(
    store: &sparkles::store::Store,
    snap: &sparkles::store::Snapshot,
    shapes: &[String],
) -> Result<Option<sparkles::schema::ConstraintsLayer>> {
    let req = http::ShapesRequest::from_values(shapes).map_err(anyhow::Error::msg)?;
    #[cfg(feature = "shacl")]
    {
        use sparkles_shacl::constraints::{configured_source, graphs_source};
        let mut layer = sparkles::schema::ConstraintsLayer::default();
        if req.guard != Some(false) {
            match sparkles_shacl::guard::configured_shapes(store)? {
                Some((cfg, s)) => layer.sources.push(configured_source(&cfg, &s)),
                None if req.guard == Some(true) => {
                    bail!("the database has no write-time SHACL validation")
                }
                None => {}
            }
        }
        for g in req.graphs.iter().filter(|g| *g != "default") {
            if !validation_common::graph_exists(snap, g) {
                bail!("no such graph: <{g}>");
            }
        }
        if !req.graphs.is_empty() {
            layer.sources.push(graphs_source(snap, &req.graphs)?);
        }
        Ok((!layer.is_empty()).then_some(layer))
    }
    #[cfg(not(feature = "shacl"))]
    {
        let _ = (store, snap);
        if req.guard == Some(true) || !req.graphs.is_empty() {
            bail!("built without the `shacl` feature");
        }
        Ok(None)
    }
}

/// `sparkles schema --format text`: the constraints layer, one line per property shape.
fn print_constraints(
    out: &mut impl Write,
    layer: &sparkles::schema::ConstraintsLayer,
) -> Result<()> {
    use sparkles::schema::constraints::SourceKind;
    let short = |i: &str| {
        let xsd = "http://www.w3.org/2001/XMLSchema#";
        let sh = "http://www.w3.org/ns/shacl#";
        match (i.strip_prefix(xsd), i.strip_prefix(sh)) {
            (Some(l), _) => format!("xsd:{l}"),
            (_, Some(l)) => format!("sh:{l}"),
            _ => format!("<{i}>"),
        }
    };
    for src in &layer.sources {
        let mut from: Vec<String> = src
            .graphs
            .iter()
            .map(|g| match g.as_str() {
                "default" => "the default graph".to_string(),
                g => format!("<{g}>"),
            })
            .collect();
        if src.file {
            from.push("a shapes file".into());
        }
        let what = match src.kind {
            SourceKind::Guard => format!(
                "write-time validation ({}, threshold {})",
                src.mode.as_deref().unwrap_or("?"),
                src.threshold.as_deref().unwrap_or("?")
            ),
            SourceKind::Graphs => "shapes graphs, validated on request".to_string(),
        };
        writeln!(
            out,
            "
constraints from {what}: {} · {} shapes · {} classes",
            from.join(", "),
            src.shapes,
            src.classes.len()
        )?;
        for c in &src.classes {
            write!(out, "  <{}>", c.class)?;
            if c.closed {
                write!(out, "  closed")?;
            }
            if c.other_paths > 0 {
                write!(out, "  (+{} other paths)", c.other_paths)?;
            }
            writeln!(out)?;
            for p in &c.properties {
                writeln!(
                    out,
                    "    <{}>  {}  [{}]",
                    p.path,
                    p.summary(short),
                    p.enforcement.name()
                )?;
            }
        }
        if src.other_targets > 0 {
            writeln!(
                out,
                "  {} shapes with other targets are not listed",
                src.other_targets
            )?;
        }
    }
    Ok(())
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
        if let Some(classes) = &o.subject_classes {
            let mut parts: Vec<String> = classes
                .iter()
                .map(|c| format!("<{}> {}", c.class, c.triples))
                .collect();
            if let Some(u) = o.untyped_subjects.filter(|u| u.triples > 0) {
                parts.push(format!("untyped {}", u.triples));
            }
            writeln!(out, "    subjects: {}", parts.join(", "))?;
        }
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
/// The dataset of a `--server` command, with the global `--branch` in the path form
/// (`ds@branch`) that every dataset endpoint takes.
fn remote_dataset(server: Option<&str>, dataset: Option<&str>) -> Result<String> {
    if server.is_none() {
        bail!("give --loc (a local database) or --server URL --dataset NAME");
    }
    let ds = dataset.context("--dataset NAME is required with --server")?;
    Ok(branch_cmd::remote_name(ds))
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
    if !r.graphs.is_empty() {
        println!("checked graphs: {}", r.graphs.join(", "));
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

/// The dataset of an existing database directory, set up as the server sets it up
/// (`sparkles::Dataset::open_with`). A directory that is not a database is an error,
/// and nothing is created.
pub(crate) fn open_dataset(loc: &std::path::Path, opts: StoreOptions) -> Result<sparkles::Dataset> {
    if !loc.join("CURRENT").exists() {
        bail!("{} is not a database directory", loc.display());
    }
    Ok(sparkles::Dataset::open_with(loc, opts)?)
}

/// A database directory, or the given files loaded into an in-memory store.
fn open_or_load(
    loc: Option<PathBuf>,
    data: &[PathBuf],
    opts: StoreOptions,
) -> Result<branch_cmd::Db> {
    Ok(match loc {
        Some(l) => branch_cmd::open_db(&l, opts)?,
        None => {
            if branch_cmd::branch().is_some() {
                bail!("--branch needs a database (--loc)");
            }
            let s = Store::in_memory(opts);
            let sources = data
                .iter()
                .map(|f| Source::from_path(f, None))
                .collect::<Result<Vec<_>, _>>()?;
            if !sources.is_empty() {
                s.load(&sources)?;
            }
            s.into()
        }
    })
}

/// `sparkles schema --draft-shapes` arguments.
struct SchemaDraftArgs {
    graph: String,
    format: String,
    timeout: Option<f64>,
    max_entries: usize,
    support: f64,
    closed: bool,
    max_in: usize,
    max_count: u64,
    classes: Vec<String>,
    min_instances: u64,
    with_inferences: bool,
}

/// `sparkles schema --draft-shapes`: SHACL shapes, a ShEx schema or the JSON draft.
/// Exit with status 2 on a budget error of the schema API.
fn schema_budget<T>(r: Result<T, sparkles::schema::SchemaError>) -> Result<T> {
    use sparkles::schema::SchemaError;
    match r {
        Ok(x) => Ok(x),
        Err(e @ (SchemaError::Timeout { .. } | SchemaError::TooManyEntries { .. })) => {
            eprintln!("error: {e}");
            std::process::exit(2);
        }
        Err(e) => Err(e.into()),
    }
}

/// `sparkles schema --profiles`.
fn schema_profiles(
    snap: &Arc<sparkles::store::Snapshot>,
    schema: sparkles::schema::SchemaOptions,
    classes: Vec<String>,
    format: &str,
) -> Result<()> {
    let json = match format {
        "json" => true,
        "text" => false,
        f => bail!("unknown format '{f}' with --profiles (text or json)"),
    };
    let mut iris = Vec::new();
    for c in classes {
        let iri = c.trim().trim_start_matches('<').trim_end_matches('>');
        oxrdf::NamedNode::new(iri).with_context(|| format!("--class {iri}"))?;
        iris.push(iri.to_string());
    }
    let opts = sparkles::schema::ProfileOptions {
        schema,
        classes: iris,
    };
    let p = schema_budget(sparkles::schema::profiles(snap, &opts))?;
    let mut out = std::io::stdout().lock();
    if json {
        serde_json::to_writer_pretty(&mut out, &p)?;
        writeln!(out)?;
    } else {
        let short = |iri: &str| iri.rsplit(['#', '/']).next().unwrap_or(iri).to_string();
        writeln!(
            out,
            "# version {} · commit {} · graph {} · inferences {}",
            p.snapshot.version,
            p.snapshot.commit,
            p.selection.graph,
            if p.selection.reasoning {
                "included"
            } else {
                "excluded"
            }
        )?;
        for c in &p.classes {
            writeln!(out, "\n<{}>  instances {}", c.class, c.instances)?;
            for x in &c.properties {
                let mut kinds: Vec<String> = Vec::new();
                for (name, n) in [
                    ("iri", x.objects.iri),
                    ("blank", x.objects.blank),
                    ("triple", x.objects.triple_term),
                ] {
                    if n > 0 {
                        kinds.push(format!("{name} {n}"));
                    }
                }
                for l in &x.objects.literals {
                    kinds.push(format!("{} {}", short(&l.datatype), l.triples));
                }
                let classes: Vec<String> = x
                    .object_classes
                    .iter()
                    .take(5)
                    .map(|k| format!("{} {}", short(&k.class), k.triples))
                    .collect();
                write!(
                    out,
                    "  <{}>  instances {}/{}  triples {}  values {}..{}  {}",
                    x.predicate,
                    x.instances,
                    c.instances,
                    x.triples,
                    x.min_per_instance,
                    x.max_per_instance,
                    kinds.join(", ")
                )?;
                if !classes.is_empty() {
                    write!(out, "  → {}", classes.join(", "))?;
                }
                writeln!(out)?;
            }
            for i in &c.incoming {
                writeln!(
                    out,
                    "  ← <{}>  triples {}  instances {}",
                    i.predicate, i.triples, i.instances
                )?;
            }
        }
    }
    out.flush()?;
    Ok(())
}

/// `sparkles schema --diff FROM [--to TO]`.
fn schema_diff(
    store: &sparkles::store::Store,
    opts: &sparkles::schema::SchemaOptions,
    from: &str,
    to: Option<&str>,
    format: &str,
) -> Result<()> {
    use sparkles::history::{At, HistoryOptions};
    let json = match format {
        "json" => true,
        "text" => false,
        f => bail!("unknown format '{f}' with --diff (text or json)"),
    };
    let at = |s: &str| s.parse::<At>().map_err(anyhow::Error::from);
    let (from, to) = (at(from)?, to.map(at).transpose()?.unwrap_or(At::Head));
    let ho = HistoryOptions {
        cancel: None,
        deadline: opts.deadline,
    };
    let (a, _) = store.snapshot_at(&from, &ho)?;
    let (b, _) = store.snapshot_at(&to, &ho)?;
    let ra = schema_budget(sparkles::schema::discover(&a, opts))?;
    let rb = schema_budget(sparkles::schema::discover(&b, opts))?;
    let d = sparkles::schema::compare(&ra, &rb);
    let mut out = std::io::stdout().lock();
    if json {
        serde_json::to_writer_pretty(&mut out, &d)?;
        writeln!(out)?;
    } else {
        out.write_all(sparkles::schema::diff_text(&d).as_bytes())?;
    }
    out.flush()?;
    Ok(())
}

fn schema_draft(
    loc: Option<PathBuf>,
    data: &[PathBuf],
    opts: StoreOptions,
    a: SchemaDraftArgs,
) -> Result<()> {
    use sparkles::schema::draft::{DraftOptions, TRACKED_VALUES, default_base};
    use sparkles::schema::{GraphSelection, SchemaError, SchemaOptions};
    #[derive(PartialEq)]
    enum Out {
        Turtle,
        Shaclc,
        ShexC,
        Json,
    }
    let out = match a.format.as_str() {
        "turtle" | "text" => Out::Turtle,
        "shaclc" => Out::Shaclc,
        "shexc" => Out::ShexC,
        "json" => Out::Json,
        f => bail!("unknown format '{f}' with --draft-shapes (turtle, shaclc, shexc or json)"),
    };
    if !(a.support > 0.0 && a.support <= 1.0) {
        bail!("--support must be in (0, 1]");
    }
    if a.max_in > TRACKED_VALUES {
        bail!("--max-in must be at most {TRACKED_VALUES}");
    }
    let name = loc
        .as_deref()
        .and_then(|l| l.file_name())
        .map_or_else(|| "data".to_string(), |f| f.to_string_lossy().into_owned());
    let store = open_or_load(loc, data, opts)?;
    let mut prefixes = sparkles::io::standard_prefixes();
    prefixes.extend(store.prefixes());
    let classes = a
        .classes
        .iter()
        .map(|c| {
            let iri = c.trim().trim_start_matches('<').trim_end_matches('>');
            oxrdf::NamedNode::new(iri)
                .map(|n| n.into_string())
                .map_err(|e| anyhow::anyhow!("--class {c}: {e}"))
        })
        .collect::<Result<Vec<_>>>()?;
    let dopts = DraftOptions {
        schema: SchemaOptions {
            graph: GraphSelection::parse(&a.graph).map_err(anyhow::Error::msg)?,
            inferred_graph: Some(http::INFERRED_GRAPH.to_string()),
            include_inferred: a.with_inferences,
            deadline: a
                .timeout
                .map(|t| Instant::now() + Duration::from_secs_f64(t)),
            max_entries: a.max_entries,
            ..Default::default()
        },
        base: default_base(&name),
        dataset: name,
        support: a.support,
        classes,
        min_instances: a.min_instances,
        max_in: a.max_in,
        max_count: a.max_count,
        closed: a.closed,
        prefixes: prefixes.into_iter().collect(),
    };
    let draft = match sparkles::schema::draft_shapes(&store.snapshot(), &dopts) {
        Ok(d) => d,
        Err(e @ (SchemaError::Timeout { .. } | SchemaError::TooManyEntries { .. })) => {
            eprintln!("error: {e}");
            std::process::exit(2);
        }
        Err(e) => return Err(e.into()),
    };
    let mut w = std::io::stdout().lock();
    match out {
        Out::Turtle => w.write_all(draft.shacl.as_bytes())?,
        Out::Shaclc => w.write_all(draft.shaclc.as_bytes())?,
        Out::ShexC => {
            w.write_all(draft.shex.as_bytes())?;
            writeln!(
                w,
                "\n# Shape map:\n# {}",
                draft.shape_map.replace('\n', "\n# ")
            )?;
        }
        Out::Json => {
            serde_json::to_writer_pretty(&mut w, &draft)?;
            writeln!(w)?;
        }
    }
    w.flush()?;
    Ok(())
}

/// The text of a shapes file (`-` for stdin), decompressed, with its syntax and the
/// base IRI of its location.
#[cfg(feature = "shacl")]
fn read_shapes_text(
    path: &std::path::Path,
) -> Result<(String, sparkles_shacl::ShapesSyntax, Option<String>)> {
    use std::io::Read;
    if path.as_os_str() == "-" {
        let mut text = String::new();
        std::io::stdin()
            .read_to_string(&mut text)
            .context("reading stdin")?;
        return Ok((text, sparkles_shacl::ShapesSyntax::default(), None));
    }
    // `.shaclc` and `.shc` are SHACLC; other names as for RDF files, Turtle by default
    let (format, _) = sparkles_shacl::ShapesSyntax::from_path(path).unwrap_or_default();
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
    Ok((text, format, base))
}

#[cfg(feature = "shacl")]
fn read_shapes(path: &std::path::Path) -> Result<sparkles_shacl::Shapes> {
    let (text, format, base) = read_shapes_text(path)?;
    sparkles_shacl::Shapes::parse(&text, format, base.as_deref())
        .with_context(|| format!("reading shapes from {}", path.display()))
}

/// `sparkles shacl parse`: each file read, checked as SHACL and written in `out`.
#[cfg(feature = "shacl")]
fn shacl_parse(
    files: &[PathBuf],
    out: &str,
    input: Option<&str>,
    base: Option<&str>,
) -> Result<()> {
    use sparkles_shacl::ShapesSyntax;
    use sparkles_shacl::syntax::{read_document, to_turtle};
    let to = ShapesSyntax::from_name(out).with_context(|| {
        format!("unknown output syntax '{out}' (shaclc, turtle, nt, jsonld or rdfxml)")
    })?;
    let from = input
        .map(|i| ShapesSyntax::from_name(i).with_context(|| format!("unknown input syntax '{i}'")))
        .transpose()?;
    let mut w = std::io::stdout().lock();
    for f in files {
        let (text, syntax, file_base) = read_shapes_text(f)?;
        let syntax = from.unwrap_or(syntax);
        // a SHACLC document without BASE would name its base as the ontology
        // (`<base> a owl:Ontology`), so the file's location is not its base
        let file_base = file_base.filter(|_| syntax != ShapesSyntax::Compact);
        let what = || format!("reading shapes from {}", f.display());
        let mut doc =
            read_document(&text, syntax, base.or(file_base.as_deref())).with_context(what)?;
        // SHACLC's predeclared prefixes, for the other syntaxes (only those used are written)
        for (p, ns) in [
            ("rdf", "http://www.w3.org/1999/02/22-rdf-syntax-ns#"),
            ("rdfs", "http://www.w3.org/2000/01/rdf-schema#"),
            ("sh", "http://www.w3.org/ns/shacl#"),
            ("xsd", "http://www.w3.org/2001/XMLSchema#"),
        ] {
            if !doc.prefixes.iter().any(|(q, n)| q == p || n == ns) {
                doc.prefixes.push((p.to_string(), ns.to_string()));
            }
        }
        // well-formed SHACL, as Jena's `shacl parse` checks
        sparkles_shacl::Shapes::from_graph(doc.graph.clone()).with_context(what)?;
        let printed = match to {
            ShapesSyntax::Compact => sparkles_shacl::compact::write(&doc.graph, &doc.prefixes)
                .with_context(|| format!("{} has no SHACLC form", f.display()))?,
            ShapesSyntax::Rdf(sparkles::io::RdfFormat::Turtle) => {
                to_turtle(&doc.graph, &doc.prefixes)?
            }
            ShapesSyntax::Rdf(format) => {
                let mut s = oxrdfio::RdfSerializer::from_format(format).for_writer(Vec::new());
                for t in doc.graph.iter() {
                    s.serialize_triple(t)?;
                }
                String::from_utf8(s.finish()?)?
            }
        };
        if files.len() > 1 {
            writeln!(w, "# {}", f.display())?;
        }
        w.write_all(printed.as_bytes())?;
        if !printed.ends_with('\n') {
            writeln!(w)?;
        }
    }
    w.flush()?;
    Ok(())
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
        tools::table::write_boolean(r.boolean, out)?;
        return Ok(());
    }
    tools::table::write_table(&r.vars, &r.rows(), &store.prefixes(), out)?;
    Ok(())
}
