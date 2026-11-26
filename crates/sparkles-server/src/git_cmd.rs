//! `sparkles git`: canonical git serialization and branch export (F25 Phases 1 & 2).

use anyhow::{Context, Result, bail};
use clap::{Subcommand, ValueEnum};
use serde_json::json;
use sparkles::Dataset;
use sparkles::git::{
    CanonicalSyntax, GitExportMode, GitExportOptions, ReadOptions, WriteOptions,
    read_git_tree_with, write_snapshot_git_tree,
};
use sparkles::history::{At, HistoryOptions};
use sparkles::store::{Store, StoreOptions};
use std::path::PathBuf;

#[derive(Clone, Copy, Debug, Default, ValueEnum)]
pub enum OutputFormat {
    #[default]
    Text,
    Json,
}

#[derive(Clone, Copy, Debug, Default, ValueEnum)]
pub enum SyntaxArg {
    #[default]
    Turtle,
    Nquads,
}

impl From<SyntaxArg> for CanonicalSyntax {
    fn from(s: SyntaxArg) -> Self {
        match s {
            SyntaxArg::Turtle => CanonicalSyntax::Turtle,
            SyntaxArg::Nquads => CanonicalSyntax::NQuads,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, ValueEnum)]
pub enum ExportModeArg {
    #[default]
    Commits,
    Squash,
}

impl From<ExportModeArg> for GitExportMode {
    fn from(m: ExportModeArg) -> Self {
        match m {
            ExportModeArg::Commits => GitExportMode::Commits,
            ExportModeArg::Squash => GitExportMode::Squash,
        }
    }
}

#[derive(Subcommand, Debug)]
pub enum GitCmd {
    /// Export a dataset branch to a Git repository in canonical layout
    Export {
        /// Database directory
        #[arg(long)]
        loc: PathBuf,
        /// Target Git repository directory (initialized if not yet a Git repository)
        #[arg(long)]
        out: PathBuf,
        /// Canonical RDF syntax for per-graph files: turtle or nquads
        #[arg(long, value_enum, default_value_t = SyntaxArg::Turtle)]
        syntax: SyntaxArg,
        /// Export mode: commits (one Git commit per Sparkles commit) or squash
        #[arg(long, value_enum, default_value_t = ExportModeArg::Commits)]
        mode: ExportModeArg,
        /// Target Git branch name (defaults to the Sparkles branch name)
        #[arg(long)]
        git_branch: Option<String>,
        /// Lowest Sparkles commit number (inclusive) to export
        #[arg(long)]
        from_commit: Option<u64>,
        /// Highest Sparkles commit number (inclusive) to export (default: branch head)
        #[arg(long)]
        to_commit: Option<u64>,
        /// Fallback Git author name when a commit has no recorded author
        #[arg(long, default_value = "Sparkles")]
        author_name: String,
        /// Fallback Git author email when a commit has no recorded email
        #[arg(long, default_value = "sparkles@localhost")]
        author_email: String,
        /// Commit message override for --mode squash
        #[arg(long)]
        message: Option<String>,
        /// Custom path for the versioned bidirectional commit mapping JSON
        #[arg(long)]
        mapping: Option<PathBuf>,
        /// Output format: text or json
        #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
        format: OutputFormat,
    },
    /// Write a single dataset snapshot as a canonical git serialization directory
    WriteTree {
        /// Database directory
        #[arg(long)]
        loc: PathBuf,
        /// Output directory for `.sparkles-git/manifest.json` and `graphs/`
        #[arg(long)]
        out: PathBuf,
        /// Past state to write: N, commit:N, time:<RFC 3339>, snapshot:NAME (default: head)
        #[arg(long)]
        at: Option<String>,
        /// Canonical RDF syntax for per-graph files: turtle or nquads
        #[arg(long, value_enum, default_value_t = SyntaxArg::Turtle)]
        syntax: SyntaxArg,
        /// Output format: text or json
        #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
        format: OutputFormat,
    },
    /// Verify a canonical git serialization directory (manifest, paths, checksums, and RDF)
    Verify {
        /// Directory containing `.sparkles-git/manifest.json` and `graphs/`
        #[arg(long)]
        dir: PathBuf,
        /// Skip SHA-256 and quad-count checks (still validates version, paths, and RDF syntax)
        #[arg(long)]
        no_checksums: bool,
        /// Output format: text or json
        #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
        format: OutputFormat,
    },
}

pub fn run(cmd: GitCmd, opts: StoreOptions) -> Result<()> {
    match cmd {
        GitCmd::Export {
            loc,
            out,
            syntax,
            mode,
            git_branch,
            from_commit,
            to_commit,
            author_name,
            author_email,
            message,
            mapping,
            format,
        } => {
            if !loc.join("CURRENT").is_file() {
                bail!("{} is not a Sparkles database (no CURRENT)", loc.display());
            }
            let main = Store::open(&loc, opts)?;
            let ds = Dataset::from_store(main);
            let export_opts = GitExportOptions {
                branch: crate::branch_cmd::branch().map(str::to_string),
                git_branch,
                mode: mode.into(),
                syntax: syntax.into(),
                from_seq: from_commit,
                to_seq: to_commit,
                default_author_name: author_name,
                default_author_email: author_email,
                squash_message: message,
                mapping_path: mapping,
                ..GitExportOptions::default()
            };
            let report = ds.export_git(&out, &export_opts)?;
            match format {
                OutputFormat::Json => println!("{}", serde_json::to_string_pretty(&report)?),
                OutputFormat::Text => {
                    println!(
                        "exported branch {} to {} (git branch {}, mode {}, syntax {}, {} new / {} skipped, head {})",
                        report.branch,
                        report.repo_dir.display(),
                        report.git_branch,
                        report.mode.as_str(),
                        report.syntax.as_str(),
                        report.commits_exported,
                        report.commits_skipped,
                        report.head_git_sha
                    );
                }
            }
            Ok(())
        }
        GitCmd::WriteTree {
            loc,
            out,
            at,
            syntax,
            format,
        } => {
            if !loc.join("CURRENT").is_file() {
                bail!("{} is not a Sparkles database (no CURRENT)", loc.display());
            }
            let main = Store::open(&loc, opts)?;
            let branch_name = crate::branch_cmd::branch().unwrap_or(sparkles::branch::MAIN);
            let selector = match at {
                Some(spec) => spec
                    .parse::<At>()
                    .with_context(|| format!("invalid --at {spec:?}"))?,
                None => At::Head,
            };
            let (snap, resolved) =
                main.branch_snapshot_at(branch_name, &selector, &HistoryOptions::default())?;
            let prefixes = main.branch(branch_name)?.prefixes();
            let manifest = write_snapshot_git_tree(
                snap.as_ref(),
                &prefixes,
                &out,
                &WriteOptions {
                    syntax: syntax.into(),
                    ..WriteOptions::default()
                },
            )?;
            let total_quads: u64 = manifest.graphs.iter().map(|g| g.quads).sum();
            match format {
                OutputFormat::Json => println!(
                    "{}",
                    serde_json::to_string_pretty(&json!({
                        "branch": branch_name,
                        "commit": resolved.commit.seq,
                        "manifest": manifest,
                    }))?
                ),
                OutputFormat::Text => {
                    println!(
                        "wrote canonical tree (format_version {}, {}, {} graphs, {} quads) to {}",
                        manifest.format_version,
                        manifest.syntax.as_str(),
                        manifest.graphs.len(),
                        total_quads,
                        out.display()
                    );
                }
            }
            Ok(())
        }
        GitCmd::Verify {
            dir,
            no_checksums,
            format,
        } => {
            let tree = read_git_tree_with(
                &dir,
                &ReadOptions {
                    verify_checksums: !no_checksums,
                },
            )?;
            match format {
                OutputFormat::Json => println!(
                    "{}",
                    serde_json::to_string_pretty(&json!({
                        "valid": true,
                        "formatVersion": tree.manifest.format_version,
                        "profile": tree.manifest.profile,
                        "syntax": tree.manifest.syntax,
                        "graphs": tree.manifest.graphs.len(),
                        "quads": tree.quads.len(),
                        "prefixes": tree.prefixes.len(),
                    }))?
                ),
                OutputFormat::Text => {
                    println!(
                        "valid canonical git tree (format_version {}, {}, {} graphs, {} quads, {} prefixes)",
                        tree.manifest.format_version,
                        tree.manifest.syntax.as_str(),
                        tree.manifest.graphs.len(),
                        tree.quads.len(),
                        tree.prefixes.len()
                    );
                }
            }
            Ok(())
        }
    }
}
