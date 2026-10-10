//! `sparkles memory` (spec C18 §10.1, Phase 3m-a): the commands a person, a hook or a
//! skill uses for agent memory.
//!
//! Every subcommand calls an operation the server already has over HTTP, with the same
//! access control (§10.3): Graph Store `PUT` and `DELETE`, `/$/memory/{ds}`,
//! `/$/validation/{ds}`, `/{ds}/sparql`, `POST /{ds}/facts`, `POST /{ds}/recall` and
//! `POST /{ds}/memory/brief`. The commands talk to a server by default and take `--loc`
//! for a database no server holds, which they serve in the process through the same
//! router (§10.2).

mod conn;
mod ops;
mod sync;

use anyhow::Result;
use clap::{Args, Subcommand};
use serde_json::{Value, json};
use sparkles::store::StoreOptions;
use std::path::PathBuf;

/// Exit statuses of §10.1.
pub const EXIT_ERROR: i32 = 1;
pub const EXIT_USAGE: i32 = 2;
pub const EXIT_PARTIAL: i32 = 3;
pub const EXIT_UNREACHABLE: i32 = 75;

#[derive(Args, Debug, Clone)]
pub struct Common {
    /// The server (else SPARKLES_SERVER, the configuration's `server`, or the saved
    /// default of `sparkles auth login`)
    #[arg(long, global = true, env = "SPARKLES_SERVER")]
    pub server: Option<String>,
    /// The dataset (else SPARKLES_MEMORY_DATASET or the configuration's `dataset`)
    #[arg(long, global = true, env = "SPARKLES_MEMORY_DATASET")]
    pub dataset: Option<String>,
    /// A database directory to open in this process instead of a server; refused with
    /// `locked` while a server holds it
    #[arg(long, global = true)]
    pub loc: Option<PathBuf>,
    /// The branch the facts are written to (default main)
    #[arg(long, global = true)]
    pub branch: Option<String>,
    /// Allow plain http to a server other than localhost
    #[arg(long, global = true)]
    pub insecure_http: bool,
    /// Exit 0 without output when the server cannot be reached (one attempt, 2 seconds)
    #[arg(long, global = true)]
    pub if_reachable: bool,
    /// One JSON document per run (one object per line for `sync --watch`)
    #[arg(long, global = true)]
    pub json: bool,
}

#[derive(Args, Debug)]
pub struct MemoryArgs {
    #[command(flatten)]
    pub common: Common,
    #[command(subcommand)]
    pub cmd: MemoryCmd,
}

/// The import flags of `import` and `sync`.
#[derive(Args, Debug, Clone, Default)]
pub struct ImportFlags {
    /// The adapters: claude-code, codex, generic (default: every adapter whose files
    /// exist)
    pub harnesses: Vec<String>,
    /// The project directory (default: the current directory)
    #[arg(long, value_name = "DIR")]
    pub project: Option<PathBuf>,
    /// A Markdown file for the generic adapter (repeatable)
    #[arg(long = "path", value_name = "FILE")]
    pub paths: Vec<PathBuf>,
    /// Also the user-scope files: ~/.claude/CLAUDE.md and rules, ~/.codex/AGENTS.md and
    /// Codex's memories, ~/.gemini/GEMINI.md and the managed CLAUDE.md
    #[arg(long)]
    pub user_scope: bool,
    /// Import transcripts (needs Phase 3m-b; refused here with the reason)
    #[arg(long)]
    pub transcripts: bool,
    /// Who extracts facts from prose: agent, server or none (Phase 3m-a writes structure
    /// only and records the choice)
    #[arg(long, value_parser = ["agent", "server", "none"])]
    pub extract: Option<String>,
    /// Extra redaction patterns: one `name regex` per line
    #[arg(long, value_name = "FILE")]
    pub redact_patterns: Option<PathBuf>,
    /// Show what would be written without writing
    #[arg(long)]
    pub dry_run: bool,
    /// Instruction files only (for a hook that runs when a turn stops)
    #[arg(long)]
    pub instructions_only: bool,
    /// Only this file (set by `--from-hook` and `--detach`)
    #[arg(long, hide = true, value_name = "FILE")]
    pub only: Option<PathBuf>,
}

#[derive(Subcommand, Debug)]
pub enum MemoryCmd {
    /// Write the memory vocabulary, install its shapes in the guard (as an admin), and
    /// set the import base in the dataset's memory settings
    Init {
        /// The prefix of every import graph (default: the current one, else
        /// urn:x-sparkles:import/)
        #[arg(long, value_name = "IRI")]
        import_base: Option<String>,
        /// Leave the guard alone
        #[arg(long)]
        no_shapes: bool,
    },
    /// Import every file the adapters find, once
    Import {
        #[command(flatten)]
        flags: ImportFlags,
    },
    /// Import what changed since the last import
    Sync {
        #[command(flatten)]
        flags: ImportFlags,
        /// Keep watching the files and sync 2 seconds after the last change
        #[arg(long)]
        watch: bool,
        /// Read a hook's JSON from standard input: claude-code or codex
        #[arg(long, value_name = "HARNESS", value_parser = ["claude-code", "codex"])]
        from_hook: Option<String>,
        /// Run the sync as a background process and return at once
        #[arg(long)]
        detach: bool,
        /// No output on success
        #[arg(long)]
        quiet: bool,
    },
    /// List the imported sources
    Sources {
        #[arg(long)]
        harness: Option<String>,
        /// A project directory or key
        #[arg(long)]
        project: Option<String>,
        /// Only sources without facts extracted from their prose
        #[arg(long)]
        needs_extraction: bool,
        /// Only sources whose file was deleted
        #[arg(long)]
        deleted: bool,
    },
    /// Summarize one project: files against sources, unresolved links, unreviewed facts
    /// and the last sync
    Status {
        #[arg(long, value_name = "DIR")]
        project: Option<PathBuf>,
        #[arg(long)]
        harness: Option<String>,
    },
    /// Render the brief of what the graph knows for a project, an entity or a session
    Brief {
        #[arg(long, value_name = "DIR")]
        project: Option<PathBuf>,
        /// A project key such as github.com/acme/shop
        #[arg(long, value_name = "KEY")]
        project_key: Option<String>,
        /// An entity IRI, or a label that must link exactly
        #[arg(long)]
        entity: Option<String>,
        /// The session scope: recall with --query
        #[arg(long, requires = "query")]
        session: bool,
        #[arg(long)]
        query: Option<String>,
        /// Add unreviewed facts, each marked
        #[arg(long)]
        include_unreviewed: bool,
        #[arg(long, default_value_t = 8000)]
        max_chars: u64,
        #[arg(long, default_value_t = 60)]
        max_facts: u64,
        /// Days after which a fact's weight halves
        #[arg(long, value_name = "DAYS", default_value_t = 90.0)]
        half_life: f64,
        /// Read a session start hook's JSON from standard input: claude-code or codex
        #[arg(long, value_name = "HARNESS", value_parser = ["claude-code", "codex"])]
        hook: Option<String>,
        /// Write the brief into FILE with the generated marker
        #[arg(long, value_name = "FILE")]
        write: Option<PathBuf>,
    },
    /// Run recall and print its text
    Recall {
        text: Option<String>,
        #[arg(long = "seed", value_name = "IRI")]
        seeds: Vec<String>,
        #[arg(long = "type", value_name = "IRI")]
        types: Vec<String>,
        #[arg(long = "graph", value_name = "IRI")]
        graphs: Vec<String>,
        #[arg(long)]
        hops: Option<u64>,
        #[arg(long)]
        reviewed_only: bool,
        #[arg(long)]
        include_superseded: bool,
        /// A commit or time: N, commit:N, time:<RFC 3339>
        #[arg(long)]
        at: Option<String>,
    },
    /// Ask a question through the server (POST /{ds}/ask), or run SPARQL with --sparql
    Query {
        question: Option<String>,
        #[arg(long)]
        preview: bool,
        #[arg(long)]
        reviewed_only: bool,
        #[arg(long, value_name = "ID")]
        try_harder: Option<String>,
        /// Run the SPARQL query of FILE (`-` for standard input) instead
        #[arg(long, value_name = "FILE")]
        sparql: Option<String>,
        /// The results format of --sparql: json, csv, tsv or xml
        #[arg(long, value_name = "FORMAT", default_value = "tsv")]
        results: String,
    },
    /// Write facts with assert_facts, from flags or from a JSON file
    Assert {
        #[arg(long, value_name = "IRI")]
        graph: Option<String>,
        #[arg(long, value_name = "IRI")]
        source: Option<String>,
        /// One fact as `S P O` (repeatable)
        #[arg(long = "fact", value_name = "S P O")]
        facts: Vec<String>,
        /// The tool's arguments as JSON
        #[arg(long, value_name = "FILE")]
        file: Option<PathBuf>,
        #[arg(long = "retract", value_name = "REIFIER")]
        retract: Vec<String>,
        #[arg(long)]
        message: Option<String>,
        #[arg(long)]
        dry_run: bool,
    },
    /// Delete import graphs for good, with their reifiers
    Forget {
        #[arg(long = "source", value_name = "IRI")]
        sources: Vec<String>,
        /// A project directory or key
        #[arg(long)]
        project: Option<String>,
        #[arg(long)]
        harness: Option<String>,
        /// Only session graphs
        #[arg(long)]
        sessions: bool,
        /// Do not ask
        #[arg(long)]
        yes: bool,
    },
    /// Print, or merge with --write, the hooks, the skill and the MCP configuration of a
    /// harness
    Setup {
        #[arg(value_parser = ["claude-code", "codex"])]
        harness: String,
        #[arg(long)]
        write: bool,
        #[arg(long, default_value = "user", value_parser = ["user", "project"])]
        scope: String,
        /// Only the session start hook that prints the brief
        #[arg(long)]
        brief: bool,
        /// Import transcripts at the session's end (needs Phase 3m-b)
        #[arg(long)]
        transcripts: bool,
    },
}

/// Print one JSON document.
pub(crate) fn print_json(v: &Value) {
    println!(
        "{}",
        serde_json::to_string_pretty(v).unwrap_or_else(|_| v.to_string())
    );
}

/// `sparkles memory`: run, print, and exit with the status of §10.1.
pub fn run(args: MemoryArgs, opts: StoreOptions) -> Result<()> {
    let json_mode = args.common.json;
    let if_reachable = args.common.if_reachable;
    match ops::dispatch(args, opts) {
        Ok(code) => {
            if code != 0 {
                std::process::exit(code);
            }
            Ok(())
        }
        Err(e) if e.exit == EXIT_UNREACHABLE && if_reachable => Ok(()),
        Err(e) => {
            if json_mode {
                let mut j = json!({ "error": e.message, "code": e.code });
                if let Some(d) = &e.detail {
                    j["detail"] = d.clone();
                }
                print_json(&j);
            } else {
                eprintln!("error: {}", e.message);
                if let Some(d) = &e.detail {
                    eprintln!("{}", serde_json::to_string_pretty(d).unwrap_or_default());
                }
            }
            std::process::exit(e.exit);
        }
    }
}
