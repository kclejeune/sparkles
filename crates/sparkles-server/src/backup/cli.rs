//! `sparkles repo …` and `sparkles backup <subcommand>`: backup repositories without a
//! server. `--repo` takes a repository name from the backup config file
//! (`--backup-config FILE`, `$SPARKLES_BACKUP_CONFIG`, default
//! `$XDG_CONFIG_HOME/sparkles/backup.toml`) or a URL (`file:///abs/dir`,
//! `s3://bucket/prefix?region=…&endpoint=…&path_style=true&allow_http=true`,
//! `memory://`). The manifest cache is `$XDG_CACHE_HOME/sparkles/backup/<repo id>/`.
//! Progress goes to stderr (a line per 5 % or 2 s); exit codes are 0 (ok), 1 (errors)
//! and 2 (warnings, such as orphans in `repo verify`).
//!
//! `sparkles backup --loc DB --out DIR` (no subcommand) keeps writing a gzipped N-Quads
//! dump (handled in `main.rs`).

use anyhow::Result;
use clap::{Args, Subcommand};
use sparkles::store::StoreOptions;
use std::path::PathBuf;

/// The backup config file of a subcommand.
#[derive(Args, Clone, Debug, Default)]
pub struct ConfigArg {
    /// Backup config file (TOML) naming repositories and policies (default
    /// $XDG_CONFIG_HOME/sparkles/backup.toml)
    #[arg(
        long,
        value_name = "FILE",
        env = "SPARKLES_BACKUP_CONFIG",
        global = true
    )]
    pub backup_config: Option<PathBuf>,
}

#[derive(Subcommand, Debug)]
pub enum RepoCmd {
    /// Add a repository to the backup config file, initialize it (or attach to it) and
    /// test it
    Add {
        /// Repository name: a-z, 0-9, '_' and '-'
        name: String,
        /// A local or mounted directory (absolute)
        #[arg(long, required_unless_present = "s3", conflicts_with = "s3")]
        path: Option<PathBuf>,
        /// An S3 bucket
        #[arg(long, value_name = "BUCKET")]
        s3: Option<String>,
        /// Key prefix inside the bucket
        #[arg(long, requires = "s3")]
        prefix: Option<String>,
        #[arg(long, requires = "s3")]
        region: Option<String>,
        /// The service URL of MinIO, Cloudflare R2, Ceph RGW, …
        #[arg(long, requires = "s3")]
        endpoint: Option<String>,
        /// Path-style addressing (MinIO)
        #[arg(long, requires = "s3")]
        path_style: bool,
        /// Allow an http:// endpoint
        #[arg(long, requires = "s3")]
        allow_http: bool,
        /// Where S3 credentials come from: default, env, or file:PATH
        #[arg(long, default_value = "default", requires = "s3")]
        credentials: String,
        /// Never write to it (restore and verify only)
        #[arg(long)]
        readonly: bool,
        /// Attach only: fail unless the location already holds a repository
        #[arg(long)]
        no_init: bool,
        #[command(flatten)]
        config: ConfigArg,
    },
    /// List the repositories of the backup config file
    List {
        /// text or json
        #[arg(long, default_value = "text")]
        format: String,
        #[command(flatten)]
        config: ConfigArg,
    },
    /// Show a repository: settings, id, reachability and totals
    Show {
        name: String,
        #[command(flatten)]
        config: ConfigArg,
    },
    /// Run the connection test (create, create again, read, list, delete a probe)
    Test {
        /// A name from the backup config file, or a URL
        repo: String,
        #[command(flatten)]
        config: ConfigArg,
    },
    /// Verify every backup of a repository and count orphaned blobs; exits 0 when
    /// clean, 1 on errors, 2 on warnings only (orphans)
    Verify {
        repo: String,
        /// exists or data
        #[arg(long, default_value = "exists", value_parser = ["exists", "data"])]
        level: String,
        #[command(flatten)]
        config: ConfigArg,
    },
    /// Remove a repository from the backup config file (its contents stay)
    Remove {
        name: String,
        #[command(flatten)]
        config: ConfigArg,
    },
    /// Delete blobs no backup references (two-phase, under a repository lock)
    Gc {
        repo: String,
        /// Report what would be deleted, delete nothing
        #[arg(long)]
        dry_run: bool,
        /// Keep unreferenced blobs younger than this (e.g. 24h, 7d)
        #[arg(long, default_value = "24h")]
        grace: String,
        #[command(flatten)]
        config: ConfigArg,
    },
    /// List the repository's locks, or break one
    Locks {
        repo: String,
        /// Delete the lock with this id
        #[arg(long = "break", value_name = "ID")]
        break_lock: Option<String>,
        #[command(flatten)]
        config: ConfigArg,
    },
}

#[derive(Subcommand, Debug)]
pub enum BackupCmd {
    /// Back up a database directory to a repository
    Create {
        #[arg(long)]
        loc: PathBuf,
        /// A repository name from the backup config file, or a URL
        #[arg(long)]
        repo: String,
        /// Backup name (default {dataset}-{time})
        #[arg(long)]
        name: Option<String>,
        #[arg(long)]
        note: Option<String>,
        #[command(flatten)]
        config: ConfigArg,
    },
    /// List a repository's backups, newest first
    List {
        #[arg(long)]
        repo: String,
        /// Only backups of this dataset name
        #[arg(long, conflicts_with = "dataset_id")]
        dataset: Option<String>,
        /// Only backups of this dataset id
        #[arg(long)]
        dataset_id: Option<String>,
        /// Only backups made by this policy
        #[arg(long)]
        policy: Option<String>,
        /// text or json
        #[arg(long, default_value = "text")]
        format: String,
        #[command(flatten)]
        config: ConfigArg,
    },
    /// Show a backup's manifest
    Show {
        #[arg(long)]
        repo: String,
        name: String,
        /// text or json
        #[arg(long, default_value = "text")]
        format: String,
        #[command(flatten)]
        config: ConfigArg,
    },
    /// Delete a backup (its manifest; blobs go at the next GC)
    Delete {
        #[arg(long)]
        repo: String,
        name: String,
        #[command(flatten)]
        config: ConfigArg,
    },
    /// Restore a backup to a directory, or into a stopped server's data directory
    Restore {
        #[arg(long)]
        repo: String,
        name: String,
        /// Restore into this directory (absent or empty)
        #[arg(long, required_unless_present = "data", conflicts_with = "data")]
        to: Option<PathBuf>,
        /// With --to: replace the database in DIR (no process may hold it)
        #[arg(long, requires = "to")]
        replace: bool,
        /// A stopped server's data directory: restore into databases/<name> and
        /// register it
        #[arg(long)]
        data: Option<PathBuf>,
        /// With --data: the dataset name (default: the backup's)
        #[arg(long = "as", value_name = "DS", requires = "data")]
        as_name: Option<String>,
        /// auto (keep the dataset id unless a dataset has it), new, or keep
        #[arg(long, default_value = "auto", value_parser = ["auto", "new", "keep"])]
        identity: String,
        /// Integrity check of the restored database: quick, full or none
        #[arg(long, default_value = "quick", value_parser = ["quick", "full", "none"])]
        check: String,
        #[command(flatten)]
        config: ConfigArg,
    },
    /// Verify a backup: exists (blobs present), data (blobs hashed) or restore (a full
    /// restore and check in a temporary directory)
    Verify {
        #[arg(long)]
        repo: String,
        name: String,
        #[arg(long, default_value = "exists", value_parser = ["exists", "data", "restore"])]
        level: String,
        #[command(flatten)]
        config: ConfigArg,
    },
    /// Lifecycle policies of the backup config file
    Policy {
        #[command(subcommand)]
        cmd: PolicyCmd,
        #[command(flatten)]
        config: ConfigArg,
    },
}

#[derive(Subcommand, Debug)]
pub enum PolicyCmd {
    /// List the policies
    List,
    /// Show a policy
    Show { policy: String },
    /// Run a policy now
    Run { policy: String },
    /// A policy's run history
    History { policy: String },
    /// The next runs of a schedule, and its description
    Preview {
        /// cron (5 or 6 fields) or "every <duration>"
        schedule: String,
        /// IANA time zone
        #[arg(long, default_value = "UTC")]
        tz: String,
    },
}

/// Exit status of a subcommand that is not built yet.
const NOT_IMPLEMENTED: i32 = 2;

fn not_implemented(what: &str) -> ! {
    eprintln!("sparkles {what}: not implemented yet");
    std::process::exit(NOT_IMPLEMENTED)
}

/// Run `sparkles repo <cmd>`.
pub fn run_repo(cmd: RepoCmd) -> Result<()> {
    let what = match cmd {
        RepoCmd::Add { .. } => "repo add",
        RepoCmd::List { .. } => "repo list",
        RepoCmd::Show { .. } => "repo show",
        RepoCmd::Test { .. } => "repo test",
        RepoCmd::Verify { .. } => "repo verify",
        RepoCmd::Remove { .. } => "repo remove",
        RepoCmd::Gc { .. } => "repo gc",
        RepoCmd::Locks { .. } => "repo locks",
    };
    not_implemented(what)
}

/// Run `sparkles backup <cmd>`; `opts` opens databases (`create --loc`) and checks
/// restores.
pub fn run_backup(cmd: BackupCmd, opts: StoreOptions) -> Result<()> {
    let _ = opts;
    let what = match cmd {
        BackupCmd::Create { .. } => "backup create",
        BackupCmd::List { .. } => "backup list",
        BackupCmd::Show { .. } => "backup show",
        BackupCmd::Delete { .. } => "backup delete",
        BackupCmd::Restore { .. } => "backup restore",
        BackupCmd::Verify { .. } => "backup verify",
        BackupCmd::Policy { .. } => "backup policy",
    };
    not_implemented(what)
}
