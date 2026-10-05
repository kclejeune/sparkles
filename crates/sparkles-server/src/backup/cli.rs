//! `sparkles repo …` and `sparkles backup <subcommand>`: backup repositories without a
//! server. `--repo` takes a repository name from the backup config file
//! (`--backup-config FILE`, `$SPARKLES_BACKUP_CONFIG`, default
//! `$XDG_CONFIG_HOME/sparkles/backup.toml`) or a URL (`file:///abs/dir`,
//! `s3://bucket/prefix?region=…&endpoint=…&path_style=true&allow_http=true`,
//! `memory://`). The manifest cache is `$XDG_CACHE_HOME/sparkles/backup/<repo id>/`.
//! Progress goes to stderr (a line per 5 % or 2 s); exit codes are 0 (ok), 1 (errors)
//! and 2 (warnings, such as orphans in `repo verify`). Ctrl-C cancels the running
//! operation (a second one quits at once).
//!
//! Only `repo add` and `backup create` initialize an empty location; the other
//! commands attach to an existing repository (`memory://` is always new).
//!
//! `sparkles backup --loc DB --out DIR` (no subcommand) keeps writing a compressed
//! N-Quads dump, zstd by default (handled in `main.rs`).

use super::config::{self, ConfigFile, RepoToml};
use crate::state;
use anyhow::{Context, Result, anyhow, bail};
use clap::{Args, Subcommand};
use serde::Serialize;
use serde_json::{Value as J, json};
use sparkles::store::StoreOptions;
use sparkles_backup::policy;
use sparkles_backup::{
    BackupSummary, CheckLevel, Credentials, Ctl, GcOptions, Identity, ListFilter, OpenEnv,
    ProgressFn, RepoConfig, RepoType, Repository, RestoreOptions, Source, VerifyLevel,
    VerifyOptions, VerifyReport, VerifyStatus,
};
use std::collections::{BTreeMap, HashSet};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use uuid::Uuid;

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

/// How a subcommand prints its result.
#[derive(Args, Clone, Debug, Default)]
pub struct OutputArg {
    /// Output: text or json
    #[arg(long, default_value = "text", value_parser = ["text", "json"], global = true)]
    pub format: String,
    /// Same as --format json
    #[arg(long, global = true)]
    pub json: bool,
}

impl OutputArg {
    fn is_json(&self) -> bool {
        self.json || self.format == "json"
    }
}

#[derive(Subcommand, Debug)]
pub enum RepoCmd {
    /// Add a repository to the backup config file, initialize it (or attach to it) and
    /// test it
    Add {
        /// Repository name: a-z, 0-9, '_' and '-'
        name: String,
        /// A local or mounted directory
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
        /// Where S3 credentials come from: default (the AWS provider chain, the
        /// default), env (the AWS_* variables), env:KEY_VAR,SECRET_VAR[,TOKEN_VAR], or
        /// file:PATH (JSON)
        #[arg(long, requires = "s3")]
        credentials: Option<String>,
        /// Keep the credential source in the config file as [credentials.NAME], which the
        /// repository names: with --credentials it defines it, without it uses the one
        /// defined. A server started with this file can then register S3 repositories
        /// through its API with {"source": "named", "name": NAME}
        #[arg(long, value_name = "NAME", requires = "s3")]
        credentials_name: Option<String>,
        /// Never write to it (restore and verify only)
        #[arg(long)]
        readonly: bool,
        /// Attach only: fail unless the location already holds a repository
        #[arg(long)]
        no_init: bool,
        #[command(flatten)]
        config: ConfigArg,
        #[command(flatten)]
        out: OutputArg,
    },
    /// List the repositories of the backup config file
    List {
        #[command(flatten)]
        config: ConfigArg,
        #[command(flatten)]
        out: OutputArg,
    },
    /// Show a repository: settings, id, reachability and totals
    Show {
        /// A name from the backup config file, or a URL
        name: String,
        #[command(flatten)]
        config: ConfigArg,
        #[command(flatten)]
        out: OutputArg,
    },
    /// Run the connection test (create, create again, read, list, delete a probe)
    Test {
        /// A name from the backup config file, or a URL
        repo: String,
        #[command(flatten)]
        config: ConfigArg,
        #[command(flatten)]
        out: OutputArg,
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
        #[command(flatten)]
        out: OutputArg,
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
        #[command(flatten)]
        out: OutputArg,
    },
    /// List the repository's locks, or break one
    Locks {
        repo: String,
        /// Delete the lock with this id
        #[arg(long = "break", value_name = "ID")]
        break_lock: Option<String>,
        #[command(flatten)]
        config: ConfigArg,
        #[command(flatten)]
        out: OutputArg,
    },
}

#[derive(Subcommand, Debug)]
pub enum BackupCmd {
    /// Back up a database directory that no server has open to a repository
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
        /// The dataset name recorded in the backup (default: the directory's name)
        #[arg(long, value_name = "NAME")]
        dataset: Option<String>,
        #[command(flatten)]
        config: ConfigArg,
        #[command(flatten)]
        out: OutputArg,
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
        #[command(flatten)]
        config: ConfigArg,
        #[command(flatten)]
        out: OutputArg,
    },
    /// Show a backup's manifest
    Show {
        #[arg(long)]
        repo: String,
        name: String,
        #[command(flatten)]
        config: ConfigArg,
        #[command(flatten)]
        out: OutputArg,
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
        #[arg(long, requires = "to", conflicts_with = "data")]
        replace: bool,
        /// A stopped server's data directory: restore into databases/<name> and
        /// register it
        #[arg(long)]
        data: Option<PathBuf>,
        /// With --data: the dataset name (default: the backup's)
        #[arg(
            long = "as",
            value_name = "DS",
            requires = "data",
            conflicts_with = "to"
        )]
        as_name: Option<String>,
        /// auto (keep the dataset id unless a dataset has it), new, or keep
        #[arg(long, default_value = "auto", value_parser = ["auto", "new", "keep"])]
        identity: String,
        /// Integrity check of the restored database: quick, full or none
        #[arg(long, default_value = "quick", value_parser = ["quick", "full", "none"])]
        check: String,
        #[command(flatten)]
        config: ConfigArg,
        #[command(flatten)]
        out: OutputArg,
    },
    /// Verify a backup: exists (blobs present), data (blobs hashed) or restore (a full
    /// restore and check in a temporary directory); exits 1 when it fails
    Verify {
        #[arg(long)]
        repo: String,
        name: String,
        #[arg(long, default_value = "exists", value_parser = ["exists", "data", "restore"])]
        level: String,
        #[command(flatten)]
        config: ConfigArg,
        #[command(flatten)]
        out: OutputArg,
    },
    /// Lifecycle policies of the backup config file
    Policy {
        #[command(subcommand)]
        cmd: PolicyCmd,
        #[command(flatten)]
        config: ConfigArg,
        #[command(flatten)]
        out: OutputArg,
    },
}

#[derive(Subcommand, Debug)]
pub enum PolicyCmd {
    /// List the policies
    List,
    /// Show a policy, its next runs and what its retention keeps
    Show { policy: String },
    /// Run a policy now (runs on a server: POST /$/backup-policies/{p}/run)
    Run { policy: String },
    /// A policy's backups in its repository, by run, with the retention verdicts
    History { policy: String },
    /// The next runs of a schedule, and its description
    Preview {
        /// cron (5 or 6 fields) or "every <duration>"
        schedule: String,
        /// IANA time zone
        #[arg(long, default_value = "UTC")]
        tz: String,
        /// How many runs
        #[arg(long, default_value_t = 5)]
        count: usize,
    },
}

/// The name a repository given by URL has (in `restore.json` and listings).
const URL_REPO_NAME: &str = "cli";

/// Exit status when only warnings were found.
const WARNINGS: i32 = 2;

// ------------------------------------------------------------------ plumbing ------

/// The runtime driving the engine, with Ctrl-C wired to a cancel flag.
struct Cli {
    rt: tokio::runtime::Runtime,
    cancel: Arc<AtomicBool>,
}

impl Cli {
    fn new() -> Result<Cli> {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        let cancel = Arc::new(AtomicBool::new(false));
        let c = cancel.clone();
        rt.spawn(async move {
            while tokio::signal::ctrl_c().await.is_ok() {
                if c.swap(true, Ordering::Relaxed) {
                    std::process::exit(130);
                }
                eprintln!("cancelling (Ctrl-C again to quit)");
            }
        });
        Ok(Cli { rt, cancel })
    }

    fn block_on<F: std::future::Future>(&self, f: F) -> F::Output {
        self.rt.block_on(f)
    }

    /// Cancellation, and progress lines on stderr.
    fn ctl(&self) -> Ctl {
        Ctl {
            cancel: self.cancel.clone(),
            progress: Some(progress_lines()),
        }
    }

    /// Open (attach to) a repository; `init` also initializes an empty location.
    fn open(&self, cfg: &RepoConfig, init: bool) -> Result<Repository> {
        let env = OpenEnv {
            cache_dir: cache_dir(),
            init: init || cfg.kind == RepoType::Memory,
            ..Default::default()
        };
        self.block_on(Repository::open(cfg, &env))
            .with_context(|| format!("repository {}", describe_repo(cfg)))
    }
}

/// A progress callback printing a line per 5 % or per 2 s.
fn progress_lines() -> ProgressFn {
    let last = Mutex::new((-1.0f32, Instant::now()));
    Arc::new(move |f: f32, msg: &str| {
        let mut l = last.lock().unwrap_or_else(|e| e.into_inner());
        if f - l.0 >= 0.05 || l.1.elapsed() >= Duration::from_secs(2) {
            *l = (f, Instant::now());
            eprintln!("{:>3.0}% {msg}", f * 100.0);
        }
    })
}

/// `$XDG_CONFIG_HOME`/`$XDG_CACHE_HOME` style directory: the variable if it is an
/// absolute path, else `$HOME/<fallback>`.
fn xdg_dir(var: &str, fallback: &str) -> Option<PathBuf> {
    if let Some(v) = std::env::var_os(var).map(PathBuf::from)
        && v.is_absolute()
    {
        return Some(v);
    }
    std::env::var_os("HOME")
        .filter(|h| !h.is_empty())
        .map(|h| PathBuf::from(h).join(fallback))
}

/// The backup config file: `--backup-config`, `$SPARKLES_BACKUP_CONFIG`, else
/// `$XDG_CONFIG_HOME/sparkles/backup.toml`.
fn config_path(c: &ConfigArg) -> Result<PathBuf> {
    match &c.backup_config {
        Some(p) => Ok(p.clone()),
        None => xdg_dir("XDG_CONFIG_HOME", ".config")
            .map(|d| d.join("sparkles").join("backup.toml"))
            .ok_or_else(|| {
                anyhow!("no backup config file: pass --backup-config (HOME is not set)")
            }),
    }
}

/// The manifest cache root (`None` without a home directory).
fn cache_dir() -> Option<PathBuf> {
    xdg_dir("XDG_CACHE_HOME", ".cache").map(|d| d.join("sparkles").join("backup"))
}

/// The config file at `path`, or an empty one if it does not exist.
fn load_or_empty(path: &Path) -> Result<ConfigFile> {
    if path.exists() {
        config::load(path)
    } else {
        Ok(ConfigFile {
            version: 1,
            ..Default::default()
        })
    }
}

/// The config file at `path`, which must exist.
fn load_existing(path: &Path) -> Result<ConfigFile> {
    if !path.exists() {
        bail!(
            "backup config {} does not exist (add a repository with `sparkles repo add`, or pass --backup-config)",
            path.display()
        );
    }
    config::load(path)
}

/// Rewrite the config file atomically (a temporary file with mode 0600, synced, then
/// renamed over it).
fn save_config(path: &Path, f: &ConfigFile) -> Result<()> {
    let text = f.to_text()?;
    let dir = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(format!(".tmp-{}", std::process::id()));
    let tmp = PathBuf::from(tmp);
    let r = (|| -> Result<()> {
        let mut o = std::fs::OpenOptions::new();
        o.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            o.mode(0o600);
        }
        let mut file = o.open(&tmp)?;
        #[cfg(unix)]
        {
            // an existing temporary file keeps its mode
            use std::os::unix::fs::PermissionsExt;
            file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
        }
        file.write_all(text.as_bytes())?;
        file.sync_all()?;
        std::fs::rename(&tmp, path)?;
        state::sync_dir(dir)
    })();
    if r.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    r.with_context(|| format!("writing backup config {}", path.display()))
}

/// The configuration of `--repo`: a URL, or a name from the config file.
fn resolve(repo: &str, c: &ConfigArg) -> Result<RepoConfig> {
    if repo.contains("://") {
        return Ok(RepoConfig::from_url(URL_REPO_NAME, repo)?);
    }
    let path = config_path(c)?;
    let f = load_existing(&path)?;
    match f.repositories.get(repo) {
        Some(r) => f.resolve_credentials(r.to_config(repo)),
        None => bail!(
            "no repository {repo:?} in {} (known: {}); a URL (file:///dir, s3://bucket/prefix) works too",
            path.display(),
            if f.repositories.is_empty() {
                "none".to_string()
            } else {
                f.repositories
                    .keys()
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(", ")
            }
        ),
    }
}

/// `name (type location)` of a configuration, for messages.
fn describe_repo(c: &RepoConfig) -> String {
    if c.name == URL_REPO_NAME {
        c.location()
    } else {
        format!("{} ({} {})", c.name, c.kind.as_str(), c.location())
    }
}

fn print_json<T: Serialize + ?Sized>(v: &T) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(v)?);
    Ok(())
}

/// Leave with `code` after flushing stdout.
fn exit_with(code: i32) -> ! {
    let _ = std::io::stdout().flush();
    std::process::exit(code)
}

/// A byte count with decimal units (`286.1 MB`).
fn bytes(n: u64) -> String {
    const UNITS: [&str; 5] = ["KB", "MB", "GB", "TB", "PB"];
    if n < 1000 {
        return format!("{n} B");
    }
    let mut v = n as f64 / 1000.0;
    let mut u = 0;
    while v >= 1000.0 && u < UNITS.len() - 1 {
        v /= 1000.0;
        u += 1;
    }
    format!("{v:.1} {}", UNITS[u])
}

fn short_id(id: &Uuid) -> String {
    format!("{}…", &id.simple().to_string()[..8])
}

fn yes_no(b: bool) -> &'static str {
    if b { "yes" } else { "no" }
}

/// `1 blob`, `2 blobs`.
pub(crate) fn plural(n: u64, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

/// Rows as aligned columns (two spaces apart; the last column is not padded).
fn table(rows: &[Vec<String>]) -> String {
    let cols = rows.iter().map(Vec::len).max().unwrap_or(0);
    let mut width = vec![0; cols];
    for r in rows {
        for (i, c) in r.iter().enumerate() {
            width[i] = width[i].max(c.chars().count());
        }
    }
    let mut out = String::new();
    for r in rows {
        let mut line = String::new();
        for (i, c) in r.iter().enumerate() {
            if i + 1 < r.len() {
                line.push_str(&format!("{c:<w$}  ", w = width[i]));
            } else {
                line.push_str(c);
            }
        }
        out.push_str(line.trim_end());
        out.push('\n');
    }
    out
}

fn verify_level(s: &str) -> VerifyLevel {
    match s {
        "data" => VerifyLevel::Data,
        "restore" => VerifyLevel::Restore,
        _ => VerifyLevel::Exists,
    }
}

fn status_str(s: VerifyStatus) -> &'static str {
    match s {
        VerifyStatus::Ok => "ok",
        VerifyStatus::Warning => "warning",
        VerifyStatus::Error => "error",
    }
}

// ---------------------------------------------------------------- sparkles repo ------

/// Run `sparkles repo <cmd>`.
pub fn run_repo(cmd: RepoCmd) -> Result<()> {
    match cmd {
        RepoCmd::Add {
            name,
            path: dir,
            s3,
            prefix,
            region,
            endpoint,
            path_style,
            allow_http,
            credentials,
            credentials_name,
            readonly,
            no_init,
            config,
            out,
        } => {
            let mut cfg = RepoConfig {
                name: name.clone(),
                conditional_writes: true,
                readonly,
                ..Default::default()
            };
            let s3_only = prefix.is_some()
                || region.is_some()
                || endpoint.is_some()
                || path_style
                || allow_http
                || credentials.is_some()
                || credentials_name.is_some();
            if dir.is_some() && s3_only {
                clap::Error::raw(
                    clap::error::ErrorKind::ArgumentConflict,
                    "--prefix, --region, --endpoint, --path-style, --allow-http, --credentials and --credentials-name only apply to --s3\n",
                )
                .exit();
            }
            let path = config_path(&config)?;
            let mut f = load_or_empty(&path)?;
            // whether a credential source was defined in the config file
            let mut defined = false;
            match (dir, s3) {
                (Some(p), _) => {
                    cfg.kind = RepoType::Fs;
                    let p = std::path::absolute(&p)
                        .with_context(|| format!("resolving {}", p.display()))?;
                    cfg.path = Some(p.to_string_lossy().into_owned());
                }
                (None, Some(bucket)) => {
                    cfg.kind = RepoType::S3;
                    cfg.bucket = Some(bucket);
                    cfg.prefix = prefix.filter(|p| !p.trim_matches('/').is_empty());
                    cfg.region = region;
                    cfg.endpoint = endpoint;
                    cfg.path_style = path_style;
                    cfg.allow_http = allow_http;
                    let source = credentials.as_deref().map(parse_credentials).transpose()?;
                    (cfg.credentials, defined) =
                        credential_source(&mut f, &path, source, credentials_name)?;
                }
                (None, None) => bail!("--path or --s3 is required"),
            }
            cfg.validate(&[])?;
            if f.repositories.contains_key(&name) || f.policies.contains_key(&name) {
                bail!("{} already defines {name:?}", path.display());
            }
            if let Some((other, _)) = f
                .repositories
                .iter()
                .find(|(n, r)| r.to_config(n).same_location(&cfg))
            {
                bail!(
                    "repository {other:?} of {} already is at {}",
                    path.display(),
                    cfg.location()
                );
            }
            let cli = Cli::new()?;
            let repo = cli.open(&f.resolve_credentials(cfg.clone())?, !no_init && !readonly)?;
            let test = cli.block_on(repo.test())?;
            if !test.ok {
                print_test(&test);
                bail!(
                    "the connection test of {} failed; {} is unchanged",
                    describe_repo(&cfg),
                    path.display()
                );
            }
            f.repositories
                .insert(name.clone(), RepoToml::from_config(&cfg));
            save_config(&path, &f)?;
            if out.is_json() {
                print_json(&json!({
                    "repository": cfg,
                    "id": repo.id(),
                    "config": path,
                    "test": test,
                }))?;
            } else {
                println!(
                    "added repository {name} ({} {}) to {} · id {} · conditional writes {}{}{}",
                    cfg.kind.as_str(),
                    cfg.location(),
                    path.display(),
                    repo.id(),
                    yes_no(test.conditional_writes),
                    match &cfg.credentials {
                        Credentials::Named { name } if defined => {
                            format!(" · credentials {name} (defined)")
                        }
                        Credentials::Named { name } => format!(" · credentials {name}"),
                        _ => String::new(),
                    },
                    if readonly { " · read-only" } else { "" }
                );
            }
            Ok(())
        }
        RepoCmd::List { config, out } => {
            let path = config_path(&config)?;
            let f = load_or_empty(&path)?;
            let repos = f.repository_configs();
            if out.is_json() {
                return print_json(&json!({ "config": path, "repositories": repos }));
            }
            if repos.is_empty() {
                println!("no repositories in {}", path.display());
                return Ok(());
            }
            let mut rows = vec![vec![
                "name".to_string(),
                "type".into(),
                "location".into(),
                "readonly".into(),
            ]];
            for r in &repos {
                rows.push(vec![
                    r.name.clone(),
                    r.kind.as_str().into(),
                    r.location(),
                    yes_no(r.readonly).into(),
                ]);
            }
            print!("{}", table(&rows));
            Ok(())
        }
        RepoCmd::Show { name, config, out } => repo_show(&name, &config, &out),
        RepoCmd::Test { repo, config, out } => {
            let cfg = resolve(&repo, &config)?;
            let cli = Cli::new()?;
            let r = cli.open(&cfg, false)?;
            let test = cli.block_on(r.test())?;
            if out.is_json() {
                print_json(&test)?;
            } else {
                print_test(&test);
            }
            if !test.ok {
                exit_with(1);
            }
            Ok(())
        }
        RepoCmd::Verify {
            repo,
            level,
            config,
            out,
        } => {
            let cfg = resolve(&repo, &config)?;
            let cli = Cli::new()?;
            let r = cli.open(&cfg, false)?;
            let o = VerifyOptions {
                level: verify_level(&level),
                ctl: cli.ctl(),
                ..Default::default()
            };
            let report = cli.block_on(r.verify(&[], &o))?;
            if out.is_json() {
                print_json(&report)?;
            } else {
                print_verify(&report);
            }
            match report.status {
                VerifyStatus::Ok => Ok(()),
                VerifyStatus::Warning => exit_with(WARNINGS),
                VerifyStatus::Error => exit_with(1),
            }
        }
        RepoCmd::Remove { name, config } => {
            let path = config_path(&config)?;
            let mut f = load_existing(&path)?;
            if !f.repositories.contains_key(&name) {
                bail!("no repository {name:?} in {}", path.display());
            }
            let users: Vec<&str> = f
                .policies
                .iter()
                .filter(|(_, p)| p.repository == name)
                .map(|(n, _)| n.as_str())
                .collect();
            if !users.is_empty() {
                bail!(
                    "policies {} back up to {name:?}: remove them from {} first",
                    users.join(", "),
                    path.display()
                );
            }
            f.repositories.remove(&name);
            save_config(&path, &f)?;
            println!(
                "removed repository {name} from {} (its contents are untouched)",
                path.display()
            );
            Ok(())
        }
        RepoCmd::Gc {
            repo,
            dry_run,
            grace,
            config,
            out,
        } => {
            let grace_d = policy::parse_duration(&grace).context("--grace")?;
            let cfg = resolve(&repo, &config)?;
            let cli = Cli::new()?;
            let r = cli.open(&cfg, false)?;
            let o = GcOptions {
                dry_run,
                grace: grace_d,
                ctl: cli.ctl(),
            };
            let g = cli.block_on(r.gc(&o))?;
            if out.is_json() {
                return print_json(&g);
            }
            let verb = if g.dry_run { "would delete" } else { "deleted" };
            println!(
                "gc of {}: {} · {} referenced · {} listed · {verb} {} ({}) · {} younger than {grace} kept · stored {} · {:.2} s",
                describe_repo(&cfg),
                plural(g.manifests, "backup", "backups"),
                plural(g.referenced_blobs, "blob", "blobs"),
                g.listed_blobs,
                plural(
                    if g.dry_run { g.candidates } else { g.deleted },
                    "blob",
                    "blobs"
                ),
                bytes(g.deleted_bytes),
                g.kept_young,
                bytes(g.stored_bytes_after),
                g.millis as f64 / 1000.0
            );
            Ok(())
        }
        RepoCmd::Locks {
            repo,
            break_lock,
            config,
            out,
        } => {
            let cfg = resolve(&repo, &config)?;
            let cli = Cli::new()?;
            let r = cli.open(&cfg, false)?;
            if let Some(id) = break_lock {
                if !cli.block_on(r.break_lock(&id))? {
                    bail!("no lock {id:?} in {}", describe_repo(&cfg));
                }
                if out.is_json() {
                    return print_json(&json!({ "broken": id }));
                }
                println!("broke lock {id} of {}", describe_repo(&cfg));
                return Ok(());
            }
            let locks = cli.block_on(r.locks())?;
            if out.is_json() {
                return print_json(&sparkles_backup::LockList { locks });
            }
            if locks.is_empty() {
                println!("no locks");
                return Ok(());
            }
            let mut rows = vec![vec![
                "id".to_string(),
                "kind".into(),
                "operation".into(),
                "holder".into(),
                "created".into(),
                "refreshed".into(),
                "stale".into(),
            ]];
            for l in &locks {
                rows.push(vec![
                    l.id.clone(),
                    json_str(&l.kind),
                    json_str(&l.operation),
                    format!("{} pid {}", l.holder.host, l.holder.pid),
                    l.created.clone(),
                    l.last_modified.clone(),
                    yes_no(l.stale).into(),
                ]);
            }
            print!("{}", table(&rows));
            Ok(())
        }
    }
}

/// The serde name of a unit enum value (`shared`, `create`).
fn json_str<T: Serialize>(v: &T) -> String {
    match serde_json::to_value(v) {
        Ok(J::String(s)) => s,
        _ => String::new(),
    }
}

/// The credentials of a repository `repo add` writes to config file `f` (at `file`):
/// `source` itself (default: the provider chain), or with `name` the credential source
/// `[credentials.<name>]`, which `source` defines if it does not exist yet (`f` gets it)
/// and must match if it does. Whether it was defined.
fn credential_source(
    f: &mut ConfigFile,
    file: &Path,
    source: Option<Credentials>,
    name: Option<String>,
) -> Result<(Credentials, bool)> {
    let Some(name) = name else {
        return Ok((source.unwrap_or_default(), false));
    };
    if !sparkles_backup::layout::valid_repo_name(&name) {
        bail!("invalid credential source name {name:?}: use a-z, 0-9, '_' and '-' (max 64)");
    }
    let existing = f.credentials.get(&name).cloned().map(Credentials::from);
    let defined = match (source, existing) {
        (Some(s), Some(e)) if s != e => bail!(
            "{} defines [credentials.{name}] otherwise; leave out --credentials to use it",
            file.display()
        ),
        (Some(s), None) => {
            f.credentials.insert(name.clone(), (&s).into());
            true
        }
        (None, None) => bail!(
            "{} has no [credentials.{name}]: define it with --credentials",
            file.display()
        ),
        _ => false,
    };
    Ok((Credentials::Named { name }, defined))
}

/// `--credentials`: default, env, env:KEY,SECRET[,TOKEN] or file:PATH.
fn parse_credentials(s: &str) -> Result<Credentials> {
    let aws = |k: &str| k.to_string();
    Ok(match s {
        "default" => Credentials::Default,
        "env" => Credentials::Env {
            access_key_id_var: aws("AWS_ACCESS_KEY_ID"),
            secret_access_key_var: aws("AWS_SECRET_ACCESS_KEY"),
            session_token_var: Some(aws("AWS_SESSION_TOKEN")),
        },
        _ => {
            if let Some(vars) = s.strip_prefix("env:") {
                let v: Vec<&str> = vars.split(',').map(str::trim).collect();
                match v.as_slice() {
                    [k, s] | [k, s, ""] => Credentials::Env {
                        access_key_id_var: k.to_string(),
                        secret_access_key_var: s.to_string(),
                        session_token_var: None,
                    },
                    [k, s, t] => Credentials::Env {
                        access_key_id_var: k.to_string(),
                        secret_access_key_var: s.to_string(),
                        session_token_var: Some(t.to_string()),
                    },
                    _ => bail!("--credentials env:KEY_VAR,SECRET_VAR[,TOKEN_VAR]"),
                }
            } else if let Some(p) = s.strip_prefix("file:").filter(|p| !p.is_empty()) {
                let p = std::path::absolute(p)?;
                Credentials::File {
                    path: p.to_string_lossy().into_owned(),
                }
            } else {
                bail!(
                    "--credentials takes default, env, env:KEY_VAR,SECRET_VAR[,TOKEN_VAR] or file:PATH"
                )
            }
        }
    })
}

fn print_test(t: &sparkles_backup::TestReport) {
    let mut rows = Vec::new();
    for s in &t.steps {
        rows.push(vec![
            json_str(&s.step),
            if s.ok { "ok".into() } else { "failed".into() },
            format!("{} ms", s.millis),
            s.error.clone().unwrap_or_default(),
        ]);
    }
    print!("{}", table(&rows));
    println!(
        "{} · conditional writes {}",
        if t.ok { "ok" } else { "failed" },
        yes_no(t.conditional_writes)
    );
}

fn print_verify(r: &VerifyReport) {
    for b in &r.backups {
        let mut problems = Vec::new();
        if !b.missing.is_empty() {
            problems.push(plural(
                b.missing.len() as u64,
                "missing blob",
                "missing blobs",
            ));
        }
        if !b.corrupt.is_empty() {
            problems.push(plural(
                b.corrupt.len() as u64,
                "corrupt blob",
                "corrupt blobs",
            ));
        }
        if let Some(e) = b
            .check
            .as_ref()
            .and_then(|c| c.get("error").and_then(J::as_str))
        {
            problems.push(e.to_string());
        } else if b.status == VerifyStatus::Error
            && let Some(c) = &b.check
        {
            problems.push(format!("check: {}", c.get("status").unwrap_or(c)));
        }
        if problems.is_empty() {
            println!("{}  {}", b.name, status_str(b.status));
        } else {
            println!(
                "{}  {}: {}",
                b.name,
                status_str(b.status),
                problems.join(", ")
            );
        }
        for id in b.missing.iter().chain(&b.corrupt).take(5) {
            println!("    {id}");
        }
    }
    if let Some(o) = r.orphans
        && o.blobs > 0
    {
        println!(
            "orphans: {} ({}), removed by `sparkles repo gc`",
            plural(o.blobs, "blob", "blobs"),
            bytes(o.bytes)
        );
    }
    println!(
        "{} · level {} · {} · {} LIST, {} HEAD, {} GET · {:.2} s",
        status_str(r.status),
        json_str(&r.level),
        plural(r.backups.len() as u64, "backup", "backups"),
        r.requests.list,
        r.requests.head,
        r.requests.get,
        r.millis as f64 / 1000.0
    );
}

fn repo_show(name: &str, c: &ConfigArg, out: &OutputArg) -> Result<()> {
    let cfg = resolve(name, c)?;
    let policies: Vec<String> = if name.contains("://") {
        Vec::new()
    } else {
        load_or_empty(&config_path(c)?)?
            .policies
            .iter()
            .filter(|(_, p)| p.repository == name)
            .map(|(n, _)| n.clone())
            .collect()
    };
    let cli = Cli::new()?;
    let opened = cli.open(&cfg, false);
    let mut view = sparkles_backup::types::Repository {
        config: cfg.clone(),
        source: sparkles_backup::ConfigSource::Config,
        id: None,
        status: sparkles_backup::RepoStatus {
            reachable: false,
            checked: sparkles_backup::now_rfc3339(),
            error: None,
            conditional_writes: None,
            single_writer: !cfg.conditional_writes,
        },
        stats: None,
        last_gc: None,
        policies,
        test: None,
    };
    match &opened {
        Ok(r) => {
            view.id = Some(r.id());
            match cli.block_on(r.stats()) {
                Ok(s) => {
                    view.status.reachable = true;
                    view.stats = Some(s);
                    view.last_gc = cli.block_on(r.last_gc()).ok().flatten();
                }
                Err(e) => view.status.error = Some(e.message().to_string()),
            }
            view.status.single_writer = r.single_writer();
        }
        Err(e) => view.status.error = Some(format!("{e:#}")),
    }
    if out.is_json() {
        print_json(&view)?;
    } else {
        let mut rows = vec![
            vec!["name".to_string(), cfg.name.clone()],
            vec!["type".into(), cfg.kind.as_str().into()],
            vec!["location".into(), cfg.location()],
        ];
        if let Some(r) = &cfg.region {
            rows.push(vec!["region".into(), r.clone()]);
        }
        if let Some(e) = &cfg.endpoint {
            rows.push(vec!["endpoint".into(), e.clone()]);
        }
        rows.push(vec!["readonly".into(), yes_no(cfg.readonly).into()]);
        if let Some(id) = view.id {
            rows.push(vec!["id".into(), id.to_string()]);
        }
        rows.push(vec![
            "reachable".into(),
            match &view.status.error {
                None => "yes".to_string(),
                Some(e) => format!("no: {e}"),
            },
        ]);
        if let Some(s) = &view.stats {
            rows.push(vec![
                "backups".into(),
                format!(
                    "{} · {}",
                    s.backups,
                    plural(s.datasets, "dataset", "datasets")
                ),
            ]);
            rows.push(vec![
                "stored".into(),
                format!(
                    "{} · logical {} · dedup {:.2}×",
                    bytes(s.stored_bytes),
                    bytes(s.logical_bytes),
                    s.dedup_ratio
                ),
            ]);
        }
        if let Some(g) = &view.last_gc {
            rows.push(vec![
                "last gc".into(),
                format!(
                    "{}{} · deleted {} ({})",
                    g.finished,
                    if g.report.dry_run { " (dry run)" } else { "" },
                    plural(g.report.deleted, "blob", "blobs"),
                    bytes(g.report.deleted_bytes)
                ),
            ]);
        }
        if !view.policies.is_empty() {
            rows.push(vec!["policies".into(), view.policies.join(", ")]);
        }
        print!("{}", table(&rows));
    }
    if !view.status.reachable {
        exit_with(1);
    }
    Ok(())
}

// -------------------------------------------------------------- sparkles backup ------

/// Run `sparkles backup <cmd>`; `opts` opens restored databases to check them.
pub fn run_backup(cmd: BackupCmd, opts: StoreOptions) -> Result<()> {
    match cmd {
        BackupCmd::Create {
            loc,
            repo,
            name,
            note,
            dataset,
            config,
            out,
        } => create(&loc, &repo, name, note, dataset, &config, &out),
        BackupCmd::List {
            repo,
            dataset,
            dataset_id,
            policy,
            config,
            out,
        } => {
            let dataset_id = dataset_id
                .map(|s| Uuid::parse_str(&s).with_context(|| format!("--dataset-id {s:?}")))
                .transpose()?;
            let cfg = resolve(&repo, &config)?;
            let cli = Cli::new()?;
            let r = cli.open(&cfg, false)?;
            let filter = ListFilter {
                dataset,
                dataset_id,
                policy,
                ..Default::default()
            };
            let list = cli.block_on(r.list(&filter))?;
            if out.is_json() {
                return print_json(&sparkles_backup::BackupPage {
                    backups: list,
                    next: None,
                });
            }
            print_backups(&list);
            let s = cli.block_on(r.stats())?;
            println!(
                "repository {}  {} · {} · stored {} · dedup {:.2}×",
                short_id(&r.id()),
                plural(s.backups, "backup", "backups"),
                plural(s.datasets, "dataset", "datasets"),
                bytes(s.stored_bytes),
                s.dedup_ratio
            );
            Ok(())
        }
        BackupCmd::Show {
            repo,
            name,
            config,
            out,
        } => {
            let cfg = resolve(&repo, &config)?;
            let cli = Cli::new()?;
            let r = cli.open(&cfg, false)?;
            let m = cli.block_on(r.manifest(&name))?;
            if out.is_json() {
                return print_json(&m.view(&cfg.name));
            }
            let mut rows = vec![
                vec!["name".to_string(), m.name.clone()],
                vec![
                    "dataset".into(),
                    format!("{} ({})", m.dataset.name, m.dataset.id),
                ],
                vec![
                    "commit".into(),
                    format!(
                        "{} at {} · {} quads",
                        m.commit.seq, m.commit.timestamp, m.commit.quads
                    ),
                ],
                vec!["generation".into(), m.generation.clone()],
                vec!["created".into(), m.created.clone()],
                vec![
                    "completed".into(),
                    format!("{} ({:.2} s)", m.completed, m.millis as f64 / 1000.0),
                ],
                vec![
                    "size".into(),
                    format!(
                        "{} logical · {} added · {} ({} new, {} reused)",
                        bytes(m.stats.logical_bytes),
                        bytes(m.stats.added_bytes),
                        plural(m.stats.blobs, "blob", "blobs"),
                        m.stats.new_blobs,
                        m.stats.reused_blobs
                    ),
                ],
            ];
            if let Some(p) = &m.parent {
                rows.push(vec!["parent".into(), p.clone()]);
            }
            if let Some(p) = &m.policy {
                rows.push(vec![
                    "policy".into(),
                    format!("{p} (run {})", m.run.as_deref().unwrap_or("-")),
                ]);
            }
            if let Some(n) = &m.note {
                rows.push(vec!["note".into(), n.clone()]);
            }
            rows.push(vec!["server".into(), m.server.version.clone()]);
            print!("{}", table(&rows));
            println!();
            let mut files = vec![vec![
                "path".to_string(),
                "kind".into(),
                "size".into(),
                "blobs".into(),
            ]];
            for f in &m.files {
                files.push(vec![
                    f.path.clone(),
                    json_str(&f.kind),
                    bytes(f.size),
                    f.blobs.len().to_string(),
                ]);
            }
            print!("{}", table(&files));
            Ok(())
        }
        BackupCmd::Delete { repo, name, config } => {
            let cfg = resolve(&repo, &config)?;
            let cli = Cli::new()?;
            let r = cli.open(&cfg, false)?;
            if !cli.block_on(r.delete(&name))? {
                bail!("no backup {name:?} in {}", describe_repo(&cfg));
            }
            println!(
                "deleted backup {name} (its blobs stay until `sparkles repo gc` finds them unreferenced)"
            );
            Ok(())
        }
        BackupCmd::Restore {
            repo,
            name,
            to,
            replace,
            data,
            as_name,
            identity,
            check,
            config,
            out,
        } => {
            let identity = match identity.as_str() {
                "new" => Identity::New,
                "keep" => Identity::Keep,
                _ => Identity::Auto,
            };
            let check = match check.as_str() {
                "full" => CheckLevel::Full,
                "none" => CheckLevel::None,
                _ => CheckLevel::Quick,
            };
            let cfg = resolve(&repo, &config)?;
            let a = RestoreArgs {
                name,
                identity,
                check,
                opts,
            };
            match (to, data) {
                (Some(to), _) => restore_to(&cfg, a, &to, replace, &out),
                (None, Some(data)) => restore_data(&cfg, a, &data, as_name, &out),
                (None, None) => bail!("--to or --data is required"),
            }
        }
        BackupCmd::Verify {
            repo,
            name,
            level,
            config,
            out,
        } => {
            let cfg = resolve(&repo, &config)?;
            let cli = Cli::new()?;
            let r = cli.open(&cfg, false)?;
            let o = VerifyOptions {
                level: verify_level(&level),
                store_opts: opts,
                ctl: cli.ctl(),
                ..Default::default()
            };
            let report = cli.block_on(r.verify(std::slice::from_ref(&name), &o))?;
            if out.is_json() {
                print_json(&report)?;
            } else {
                print_verify(&report);
            }
            if report.status == VerifyStatus::Error {
                exit_with(1);
            }
            Ok(())
        }
        BackupCmd::Policy { cmd, config, out } => run_policy(cmd, &config, &out),
    }
}

fn print_backups(list: &[BackupSummary]) {
    if list.is_empty() {
        println!("no backups");
        return;
    }
    let mut rows = vec![vec![
        "name".to_string(),
        "dataset".into(),
        "commit".into(),
        "completed".into(),
        "logical".into(),
        "added".into(),
        "policy".into(),
    ]];
    for b in list {
        rows.push(vec![
            b.name.clone(),
            b.dataset.name.clone(),
            b.commit.seq.to_string(),
            b.completed.clone(),
            bytes(b.logical_bytes),
            bytes(b.added_bytes),
            b.policy.clone().unwrap_or_else(|| "-".into()),
        ]);
    }
    print!("{}", table(&rows));
}

/// `reasoning.json` of a closed database, as the server adds it to a backup: only when
/// its inferences are not from a later commit than `seq`.
fn reasoning_file(root: &Path, seq: u64) -> Option<(String, Vec<u8>)> {
    let mut info = state::read_reasoning_file(root)?;
    if info.commit.is_some_and(|c| c > seq) {
        return None;
    }
    info.reasoning_format = 2;
    Some((
        "reasoning.json".into(),
        serde_json::to_vec_pretty(&info).ok()?,
    ))
}

fn create(
    loc: &Path,
    repo: &str,
    name: Option<String>,
    note: Option<String>,
    dataset: Option<String>,
    config: &ConfigArg,
    out: &OutputArg,
) -> Result<()> {
    let dataset = match dataset {
        Some(d) => d,
        None => std::path::absolute(loc)?
            .file_name()
            .map(|f| f.to_string_lossy().into_owned())
            .unwrap_or_else(|| "db".into()),
    };
    if !state::valid_name(&dataset) {
        bail!("{dataset:?} is not a valid dataset name: pass --dataset NAME");
    }
    let name = name.unwrap_or_else(|| {
        sparkles_backup::layout::default_backup_name(&dataset, chrono::Utc::now())
    });
    if !sparkles_backup::layout::valid_backup_name(&name) {
        bail!(
            "invalid backup name {name:?}: up to 64 of A-Z, a-z, 0-9, '.', '_' and '-', starting with a letter or digit"
        );
    }
    let cfg = resolve(repo, config)?;
    let cli = Cli::new()?;
    let r = cli.open(&cfg, true)?;
    // opens the database (the "in use by another process" error while a server has it)
    let src = Source::from_closed_branch(
        loc,
        crate::branch_cmd::branch().unwrap_or("main"),
        &sparkles::store::MemoryCaptureOptions {
            cancel: Some(cli.ctl().cancel.clone()),
            ..Default::default()
        },
    )?;
    let branch_root = src
        .branch
        .as_ref()
        .map(|b| loc.join("branches").join(b.id.to_string()));
    let extra = reasoning_file(branch_root.as_deref().unwrap_or(loc), src.commit.seq)
        .into_iter()
        .collect();
    let o = sparkles_backup::CreateOptions {
        name,
        note,
        policy: None,
        dataset_name: dataset,
        extra,
        min_free_disk_bytes: None,
        ctl: cli.ctl(),
    };
    let s = cli.block_on(r.create(src, &o))?;
    // the manifest (cached by the create) has the file and blob counts
    let stats = cli
        .block_on(r.manifest(&s.name))
        .map(|m| m.stats)
        .unwrap_or_default();
    if out.is_json() {
        let mut v = serde_json::to_value(&s)?;
        v["stats"] = serde_json::to_value(&stats)?;
        return print_json(&v);
    }
    println!(
        "backup {} of {} ({}) at commit {} · {} · {} logical · {} added · {} · {:.2} s",
        s.name,
        s.dataset.name,
        short_id(&s.dataset.id),
        s.commit.seq,
        plural(stats.files, "file", "files"),
        bytes(s.logical_bytes),
        bytes(s.added_bytes),
        plural(stats.new_blobs, "new blob", "new blobs"),
        s.millis as f64 / 1000.0
    );
    Ok(())
}

struct RestoreArgs {
    name: String,
    identity: Identity,
    check: CheckLevel,
    opts: StoreOptions,
}

/// Print what a restore did.
fn print_restore(
    r: &sparkles_backup::RestoreReport,
    target: &Path,
    dataset: Option<&str>,
    out: &OutputArg,
) -> Result<()> {
    if out.is_json() {
        let mut v = json!({
            "backup": r.backup,
            "target": target,
            "datasetId": r.dataset_id,
            "identity": r.identity,
            "check": r.check,
            "millis": r.millis,
        });
        if let Some(d) = dataset {
            v["dataset"] = json!(d);
        }
        if let Some(f) = &r.forked_from {
            v["forkedFrom"] = json!(f);
        }
        return print_json(&v);
    }
    let check = match &r.check {
        Some(c) => format!(
            "check {}",
            c.get("status").and_then(J::as_str).unwrap_or("done")
        ),
        None => "no check".into(),
    };
    println!(
        "restored {} of {} (commit {}, {} quads) into {}{} · dataset id {} ({}) · {check} · {:.2} s",
        r.backup.name,
        r.backup.dataset.name,
        r.backup.commit.seq,
        r.backup.commit.quads,
        target.display(),
        dataset.map(|d| format!(" as /{d}")).unwrap_or_default(),
        r.dataset_id,
        r.identity,
        r.millis as f64 / 1000.0
    );
    Ok(())
}

/// The "in use by another process" error if a process holds `dir`'s database lock.
fn refuse_if_open(dir: &Path) -> Result<()> {
    let path = dir.join("sparkles.lock");
    let Ok(f) = std::fs::File::open(&path) else {
        return Ok(());
    };
    if let Err(std::fs::TryLockError::WouldBlock) = f.try_lock() {
        let pid = std::fs::read_to_string(&path).unwrap_or_default();
        bail!(
            "database {} is in use by another process (pid {}); stop it or talk to it over HTTP",
            dir.display(),
            pid.trim()
        );
    }
    Ok(())
}

/// `--to DIR [--replace]`: restore next to DIR, then rename (or swap) it into place.
/// `auto` identity keeps the id here (no registry to compare with).
fn restore_to(
    cfg: &RepoConfig,
    a: RestoreArgs,
    to: &Path,
    replace: bool,
    out: &OutputArg,
) -> Result<()> {
    let to = std::path::absolute(to)?;
    let empty = |d: &Path| std::fs::read_dir(d).is_ok_and(|mut e| e.next().is_none());
    if to.exists() {
        if !to.is_dir() {
            bail!("{} exists and is not a directory", to.display());
        }
        if !empty(&to) {
            if !replace {
                bail!(
                    "{} is not empty: pass --replace to replace the database in it",
                    to.display()
                );
            }
            if !to.join("CURRENT").is_file() {
                bail!(
                    "{} is not a database directory; refusing to replace it",
                    to.display()
                );
            }
            refuse_if_open(&to)?;
        }
    }
    let parent = to
        .parent()
        .ok_or_else(|| anyhow!("{} has no parent directory", to.display()))?;
    std::fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
    let file_name = to
        .file_name()
        .ok_or_else(|| anyhow!("{} has no name", to.display()))?
        .to_string_lossy();
    let tmp = parent.join(format!(".{file_name}.restore-{}", std::process::id()));
    if tmp.exists() {
        std::fs::remove_dir_all(&tmp)?;
    }
    let cli = Cli::new()?;
    let r = cli.open(cfg, false)?;
    let o = RestoreOptions {
        identity: a.identity,
        check: a.check,
        id_in_use: Arc::new(|_| false),
        in_place_head: None,
        store_opts: a.opts,
        ctl: cli.ctl(),
    };
    let report = cli.block_on(r.restore(&a.name, &tmp, &o))?;
    let published = (|| -> Result<()> {
        if to.exists() && empty(&to) {
            std::fs::remove_dir(&to)?;
        }
        sparkles_backup::restore::swap_dir(&to, &tmp, false)?;
        Ok(())
    })();
    if let Err(e) = published {
        let _ = std::fs::remove_dir_all(&tmp);
        return Err(e);
    }
    print_restore(&report, &to, None, out)
}

/// Restore into the catalog of a stopped server, using its registry and identity rules.
fn restore_data(
    cfg: &RepoConfig,
    a: RestoreArgs,
    data: &Path,
    as_name: Option<String>,
    out: &OutputArg,
) -> Result<()> {
    let catalog = sparkles::Catalog::open(data, a.opts.into())?;
    let cli = Cli::new()?;
    let repo = cli.open(cfg, false)?;
    let req = sparkles_backup::RestoreRequest {
        target: as_name,
        identity: a.identity,
        check: a.check,
        ..Default::default()
    };
    let (dataset, report) = catalog.restore_report(&repo, &a.name, &req, &cli.ctl().control())?;
    let root = dataset.store().root().expect("persistent restore");
    print_restore(&report, root, dataset.name(), out)
}

// ------------------------------------------------------- sparkles backup policy ------

fn run_policy(cmd: PolicyCmd, c: &ConfigArg, out: &OutputArg) -> Result<()> {
    let now = chrono::Utc::now();
    let rfc =
        |t: chrono::DateTime<chrono::Utc>| t.to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    match cmd {
        PolicyCmd::Preview {
            schedule,
            tz,
            count,
        } => {
            let s = policy::parse_schedule(&schedule)?;
            let zone = policy::parse_timezone(&tz)?;
            let next: Vec<String> = policy::next_runs(&s, zone, now, count.clamp(1, 100))
                .into_iter()
                .map(rfc)
                .collect();
            let p = sparkles_backup::PreviewResponse {
                next,
                description: policy::describe(&s, zone),
                sample: None,
            };
            if out.is_json() {
                return print_json(&p);
            }
            println!("{}", p.description);
            for n in &p.next {
                println!("  {n}");
            }
            Ok(())
        }
        PolicyCmd::Run { policy } => bail!(
            "policies run on a server: POST /$/backup-policies/{policy}/run (running one offline is not supported yet)"
        ),
        PolicyCmd::List => {
            let path = config_path(c)?;
            let f = load_or_empty(&path)?;
            let list: Vec<J> = f
                .policy_configs()
                .iter()
                .map(|p| policy_json(p, now, 1))
                .collect();
            if out.is_json() {
                return print_json(&json!({ "config": path, "policies": list }));
            }
            if list.is_empty() {
                println!("no policies in {}", path.display());
                return Ok(());
            }
            let mut rows = vec![vec![
                "name".to_string(),
                "repository".into(),
                "datasets".into(),
                "schedule".into(),
                "next run".into(),
                "enabled".into(),
            ]];
            for (p, j) in f.policy_configs().iter().zip(&list) {
                rows.push(vec![
                    p.name.clone(),
                    p.repository.clone(),
                    p.datasets.join(","),
                    j["description"].as_str().unwrap_or("").to_string(),
                    j["next"][0].as_str().unwrap_or("-").to_string(),
                    yes_no(p.enabled).into(),
                ]);
            }
            print!("{}", table(&rows));
            Ok(())
        }
        PolicyCmd::Show { policy: name } => {
            let path = config_path(c)?;
            let f = load_existing(&path)?;
            let p = f
                .policies
                .get(&name)
                .map(|p| p.to_config(&name))
                .ok_or_else(|| anyhow!("no policy {name:?} in {}", path.display()))?;
            let j = policy_json(&p, now, 5);
            if out.is_json() {
                return print_json(&j);
            }
            let r = &p.retention;
            let mut rows = vec![
                vec!["name".to_string(), p.name.clone()],
                vec!["repository".into(), p.repository.clone()],
                vec!["datasets".into(), p.datasets.join(", ")],
                vec![
                    "schedule".into(),
                    format!(
                        "{} ({})",
                        p.schedule,
                        j["description"].as_str().unwrap_or("")
                    ),
                ],
                vec!["names".into(), p.name_template.clone()],
                vec![
                    "retention".into(),
                    format!(
                        "keep the newest {}{}{}",
                        r.min_count,
                        r.max_count
                            .map(|m| format!(", at most {m}"))
                            .unwrap_or_default(),
                        r.expire_after
                            .as_deref()
                            .map(|e| format!(", delete others older than {e}"))
                            .unwrap_or_default()
                    ),
                ],
                vec!["skip unchanged".into(), yes_no(p.skip_unchanged).into()],
                vec![
                    "gc after retention".into(),
                    yes_no(p.gc_after_retention).into(),
                ],
                vec!["catch up".into(), json_str(&p.catch_up)],
                vec!["enabled".into(), yes_no(p.enabled).into()],
            ];
            if let Some(e) = j["error"].as_str() {
                rows.push(vec!["error".into(), e.to_string()]);
            }
            print!("{}", table(&rows));
            if let Some(next) = j["next"].as_array().filter(|n| !n.is_empty()) {
                println!("next runs:");
                for n in next {
                    println!("  {}", n.as_str().unwrap_or(""));
                }
            }
            Ok(())
        }
        PolicyCmd::History { policy: name } => {
            let path = config_path(c)?;
            let f = load_existing(&path)?;
            let p = f
                .policies
                .get(&name)
                .map(|p| p.to_config(&name))
                .ok_or_else(|| anyhow!("no policy {name:?} in {}", path.display()))?;
            let repo = f
                .repositories
                .get(&p.repository)
                .map(|r| r.to_config(&p.repository))
                .ok_or_else(|| {
                    anyhow!(
                        "repository {:?} of policy {name} is not in {}",
                        p.repository,
                        path.display()
                    )
                })?;
            let cli = Cli::new()?;
            let r = cli.open(&repo, false)?;
            let list = cli.block_on(r.list(&ListFilter {
                policy: Some(name.clone()),
                ..Default::default()
            }))?;
            let plan = policy::retention(&list, &name, &p.retention, now, &HashSet::new());
            // by run, newest first (the listing is newest first)
            let mut runs: Vec<(String, Vec<&BackupSummary>)> = Vec::new();
            let mut at: BTreeMap<String, usize> = BTreeMap::new();
            for b in &list {
                let run = b.run.clone().unwrap_or_else(|| "-".into());
                let i = *at.entry(run.clone()).or_insert_with(|| {
                    runs.push((run, Vec::new()));
                    runs.len() - 1
                });
                runs[i].1.push(b);
            }
            let verdict = |b: &BackupSummary| {
                plan.reasons
                    .get(&b.name)
                    .map(|r| r.describe())
                    .unwrap_or("")
                    .to_string()
            };
            if out.is_json() {
                let runs: Vec<J> = runs
                    .iter()
                    .map(|(run, bs)| {
                        json!({
                            "run": run,
                            "backups": bs.iter().map(|b| {
                                let mut v = serde_json::to_value(b).unwrap_or(J::Null);
                                v["retention"] = json!(verdict(b));
                                v
                            }).collect::<Vec<_>>(),
                        })
                    })
                    .collect();
                return print_json(&json!({
                    "policy": name,
                    "repository": p.repository,
                    "runs": runs,
                }));
            }
            if runs.is_empty() {
                println!("no backups of policy {name} in {}", describe_repo(&repo));
                return Ok(());
            }
            for (run, bs) in &runs {
                println!("run {run}");
                let rows: Vec<Vec<String>> = bs
                    .iter()
                    .map(|b| {
                        vec![
                            format!("  {}", b.name),
                            b.dataset.name.clone(),
                            b.commit.seq.to_string(),
                            b.completed.clone(),
                            bytes(b.added_bytes),
                            verdict(b),
                        ]
                    })
                    .collect();
                print!("{}", table(&rows));
            }
            Ok(())
        }
    }
}

/// A policy with its schedule's description and next `n` runs (or the error that
/// makes it invalid).
fn policy_json(
    p: &sparkles_backup::PolicyConfig,
    now: chrono::DateTime<chrono::Utc>,
    n: usize,
) -> J {
    let mut j = serde_json::to_value(p).unwrap_or(J::Null);
    match policy::check_policy(p) {
        Ok((s, tz)) => {
            j["description"] = json!(policy::describe(&s, tz));
            j["next"] = json!(
                policy::next_runs(&s, tz, now, n)
                    .into_iter()
                    .map(|t| t.to_rfc3339_opts(chrono::SecondsFormat::Secs, true))
                    .collect::<Vec<_>>()
            );
        }
        Err(e) => {
            j["error"] = json!(e.message());
            j["next"] = json!([]);
        }
    }
    j
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn named_credential_sources_are_defined_once_and_then_named() {
        let file = Path::new("/etc/sparkles/backup.toml");
        let mut f = ConfigFile {
            version: 1,
            ..Default::default()
        };
        let env = parse_credentials("env:MINIO_KEY,MINIO_SECRET").unwrap();
        // without a name: the source itself, the provider chain by default
        assert_eq!(
            credential_source(&mut f, file, None, None).unwrap(),
            (Credentials::Default, false)
        );
        assert_eq!(
            credential_source(&mut f, file, Some(env.clone()), None).unwrap(),
            (env.clone(), false)
        );
        assert!(f.credentials.is_empty());
        // a name not defined yet needs a source
        let e = credential_source(&mut f, file, None, Some("minio".into())).unwrap_err();
        assert!(e.to_string().contains("no [credentials.minio]"), "{e}");
        // defined with one, then named by itself or with the same source
        let named = Credentials::Named {
            name: "minio".into(),
        };
        assert_eq!(
            credential_source(&mut f, file, Some(env.clone()), Some("minio".into())).unwrap(),
            (named.clone(), true)
        );
        assert_eq!(
            Credentials::from(f.credentials["minio"].clone()),
            env.clone()
        );
        assert_eq!(
            credential_source(&mut f, file, None, Some("minio".into())).unwrap(),
            (named.clone(), false)
        );
        assert_eq!(
            credential_source(&mut f, file, Some(env), Some("minio".into())).unwrap(),
            (named.clone(), false)
        );
        // another source under the same name is refused
        let e = credential_source(
            &mut f,
            file,
            Some(Credentials::Default),
            Some("minio".into()),
        )
        .unwrap_err();
        assert!(e.to_string().contains("otherwise"), "{e}");
        assert!(credential_source(&mut f, file, None, Some("Bad Name".into())).is_err());
        // the file a server reads: the repository names the source
        let cfg = RepoConfig {
            name: "s3-main".into(),
            kind: RepoType::S3,
            bucket: Some("kg".into()),
            credentials: named,
            ..Default::default()
        };
        f.repositories
            .insert(cfg.name.clone(), RepoToml::from_config(&cfg));
        let text = f.to_text().unwrap();
        assert!(text.contains("[credentials.minio]"), "{text}");
        let back = ConfigFile::parse(&text).unwrap();
        assert_eq!(back, f);
        assert_eq!(
            back.resolve_credentials(cfg).unwrap().credentials,
            Credentials::from(f.credentials["minio"].clone())
        );
    }
}
