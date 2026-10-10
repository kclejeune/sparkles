//! `sparkles models`: the model store (spec F12 §4). `pull` downloads pinned snapshots
//! from the Hugging Face Hub with per-file verification into
//! `DIR/<owner>/<name>/<revision>/`, `list` and `verify` read the store, and `rm` deletes
//! a snapshot. The server reads the same store (`serve --models-dir`).

use anyhow::{Context, Result, bail};
use clap::{Args, Subcommand};
use serde::Deserialize;
use serde_json::json;
use sparkles_modelstore::{HubSource, ModelStore, SnapshotId, is_pinned};
use std::path::PathBuf;

use crate::model_store::{OutboundClient, download_policy};
use crate::outbound::OutboundArgs;

#[derive(Args, Debug)]
pub struct ModelsArgs {
    #[command(subcommand)]
    cmd: ModelsCmd,
}

/// Where the store is: `--dir`, or `<--data>/models`, or `$SPARKLES_MODELS_DIR`, or
/// `./data/models` (the default of `serve --data`).
#[derive(Args, Debug, Clone)]
struct DirArgs {
    /// The model store
    #[arg(long, value_name = "DIR", conflicts_with = "data")]
    dir: Option<PathBuf>,
    /// A server's data directory; the store is DIR/models
    #[arg(long, value_name = "DIR")]
    data: Option<PathBuf>,
}

impl DirArgs {
    fn store(&self) -> ModelStore {
        let dir = match (&self.dir, &self.data) {
            (Some(d), _) => d.clone(),
            (None, Some(d)) => d.join("models"),
            (None, None) => std::env::var_os("SPARKLES_MODELS_DIR")
                .filter(|v| !v.is_empty())
                .map_or_else(|| PathBuf::from("data/models"), PathBuf::from),
        };
        ModelStore::new(dir)
    }
}

#[derive(Subcommand, Debug)]
enum ModelsCmd {
    /// Download snapshots into the store and verify each file's hash; a snapshot already
    /// there is left alone without any network request
    Pull {
        /// REPO@REVISION, with a full 40-character commit (REPO alone, or a branch or
        /// tag, needs --allow-unpinned)
        models: Vec<String>,
        /// A JSON file: {"models": [{"repo": …, "revision": …, "files": [optional
        /// subset]}]}
        #[arg(long, value_name = "FILE")]
        manifest: Option<PathBuf>,
        #[command(flatten)]
        dir: DirArgs,
        /// Accept a branch, a tag or no revision (main), resolved to the commit the Hub
        /// names, which the store records
        #[arg(long)]
        allow_unpinned: bool,
        /// The Hub's address, for a mirror
        #[arg(
            long,
            value_name = "URL",
            env = "SPARKLES_HUB_ENDPOINT",
            default_value = sparkles_modelstore::DEFAULT_ENDPOINT
        )]
        hub_endpoint: String,
        /// A file holding a Hub token, for gated or private repositories (also
        /// $HF_TOKEN)
        #[arg(long, value_name = "FILE")]
        hub_token_file: Option<PathBuf>,
        /// Print the pulled snapshots as JSON
        #[arg(long)]
        json: bool,
        #[command(flatten)]
        outbound: OutboundArgs,
    },
    /// List the snapshots in the store
    List {
        #[command(flatten)]
        dir: DirArgs,
        /// Print JSON with each file's size and SHA-256
        #[arg(long)]
        json: bool,
    },
    /// Read every file of every snapshot again and compare it with its manifest
    Verify {
        #[command(flatten)]
        dir: DirArgs,
        #[arg(long)]
        json: bool,
    },
    /// Delete a snapshot from the store
    Rm {
        /// REPO@REVISION
        model: String,
        #[command(flatten)]
        dir: DirArgs,
    },
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    models: Vec<Entry>,
}

#[derive(Deserialize, Clone)]
#[serde(deny_unknown_fields)]
struct Entry {
    repo: String,
    #[serde(default)]
    revision: Option<String>,
    #[serde(default)]
    files: Option<Vec<String>>,
}

fn parse_ref(s: &str) -> Entry {
    let (repo, revision) = match s.split_once('@') {
        Some((r, v)) => (r.to_string(), Some(v.to_string())),
        None => (s.to_string(), None),
    };
    Entry {
        repo,
        revision,
        files: None,
    }
}

fn pinned_ref(s: &str) -> Result<SnapshotId> {
    let e = parse_ref(s);
    match e.revision {
        Some(r) if is_pinned(&r) => Ok(SnapshotId {
            repo: e.repo,
            revision: r.to_ascii_lowercase(),
        }),
        _ => bail!("{s}: give REPO@REVISION with a full 40-character commit"),
    }
}

pub fn run(args: ModelsArgs) -> Result<()> {
    match args.cmd {
        ModelsCmd::Pull {
            models,
            manifest,
            dir,
            allow_unpinned,
            hub_endpoint,
            hub_token_file,
            json,
            outbound,
        } => {
            let mut entries: Vec<Entry> = models.iter().map(|m| parse_ref(m)).collect();
            if let Some(f) = &manifest {
                let text = std::fs::read_to_string(f)
                    .with_context(|| format!("cannot read {}", f.display()))?;
                let m: Manifest = serde_json::from_str(&text)
                    .with_context(|| format!("model manifest {}", f.display()))?;
                entries.extend(m.models);
            }
            if entries.is_empty() {
                bail!("name a model (REPO@REVISION) or give --manifest");
            }
            let token = match &hub_token_file {
                Some(f) => Some(
                    std::fs::read_to_string(f)
                        .with_context(|| format!("cannot read {}", f.display()))?
                        .trim()
                        .to_string(),
                ),
                None => std::env::var("HF_TOKEN").ok().filter(|t| !t.is_empty()),
            };
            let hub = HubSource {
                endpoint: hub_endpoint,
                token,
            };
            let store = dir.store();
            let client = OutboundClient(download_policy(&outbound.local_policy()?));
            let mut out = Vec::new();
            let mut failed = 0;
            for e in entries {
                match pull(&store, &hub, &client, &e, allow_unpinned) {
                    Ok((id, dir, fetched)) => {
                        if !json {
                            println!(
                                "{}@{} {} {}",
                                id.repo,
                                id.revision,
                                if fetched { "pulled" } else { "present" },
                                dir.display()
                            );
                        }
                        out.push(json!({
                            "repo": id.repo,
                            "revision": id.revision,
                            "dir": dir.display().to_string(),
                            "pulled": fetched,
                        }));
                    }
                    Err(err) => {
                        failed += 1;
                        eprintln!(
                            "{}@{}: {err:#}",
                            e.repo,
                            e.revision.as_deref().unwrap_or("main")
                        );
                    }
                }
            }
            if json {
                println!("{}", serde_json::to_string_pretty(&out)?);
            }
            if failed > 0 {
                bail!("{failed} model(s) could not be pulled");
            }
            Ok(())
        }
        ModelsCmd::List { dir, json } => {
            let store = dir.store();
            let list = store.list()?;
            if json {
                let v: Vec<_> = list
                    .iter()
                    .map(|m| {
                        json!({
                            "repo": m.id.repo,
                            "revision": m.id.revision,
                            "dir": store.root().join(&m.id.repo).join(&m.id.revision).display().to_string(),
                            "bytes": m.files.iter().map(|f| f.size).sum::<u64>(),
                            "files": m.files,
                        })
                    })
                    .collect();
                println!("{}", serde_json::to_string_pretty(&v)?);
            } else {
                for m in &list {
                    let bytes: u64 = m.files.iter().map(|f| f.size).sum();
                    println!(
                        "{}@{}  {} files  {:.1} MB",
                        m.id.repo,
                        m.id.revision,
                        m.files.len(),
                        bytes as f64 / 1e6
                    );
                }
            }
            Ok(())
        }
        ModelsCmd::Verify { dir, json } => {
            let store = dir.store();
            let mut out = Vec::new();
            let mut bad = 0;
            for m in store.list()? {
                let r = store.verify(&m.id);
                if r.is_err() {
                    bad += 1;
                }
                if !json {
                    match &r {
                        Ok(()) => println!("{}@{} ok", m.id.repo, m.id.revision),
                        Err(e) => println!("{}@{} FAILED: {e}", m.id.repo, m.id.revision),
                    }
                }
                out.push(json!({
                    "repo": m.id.repo,
                    "revision": m.id.revision,
                    "ok": r.is_ok(),
                    "error": r.err().map(|e| e.to_string()),
                }));
            }
            if json {
                println!("{}", serde_json::to_string_pretty(&out)?);
            }
            if bad > 0 {
                bail!("{bad} snapshot(s) failed verification");
            }
            Ok(())
        }
        ModelsCmd::Rm { model, dir } => {
            let id = pinned_ref(&model)?;
            if !dir.store().remove(&id)? {
                bail!("{model} is not in the store");
            }
            println!("removed {model}");
            Ok(())
        }
    }
}

/// Pull one entry: `(id, dir, whether anything was downloaded)`.
fn pull(
    store: &ModelStore,
    hub: &HubSource,
    client: &OutboundClient,
    e: &Entry,
    allow_unpinned: bool,
) -> Result<(SnapshotId, PathBuf, bool)> {
    sparkles_modelstore::check_repo(&e.repo)?;
    let revision = e.revision.clone().unwrap_or_else(|| "main".into());
    // a pinned snapshot already in the store needs no request
    if is_pinned(&revision) {
        let id = SnapshotId {
            repo: e.repo.clone(),
            revision: revision.to_ascii_lowercase(),
        };
        if let Some(s) = store.get(&id)? {
            if let Some(files) = &e.files {
                let m = sparkles_modelstore::read_manifest(&s.dir)?;
                if let Some(f) = files
                    .iter()
                    .find(|f| !m.files.iter().any(|x| &x.path == *f))
                {
                    bail!(
                        "the snapshot in the store lacks {f}; remove it with `sparkles models rm` and pull again"
                    );
                }
            }
            return Ok((id, s.dir, false));
        }
    }
    let wanted = e.files.clone();
    let select = move |p: &str| match &wanted {
        Some(f) => f.iter().any(|x| x == p),
        None => sparkles_modelstore::sentence_transformers_files(p),
    };
    let plan = hub.plan(client, &e.repo, &revision, allow_unpinned, &select)?;
    if let Some(files) = &e.files
        && let Some(f) = files
            .iter()
            .find(|f| !plan.files.iter().any(|x| &x.path == *f))
    {
        bail!("{f} is not in {} at {}", e.repo, plan.id.revision);
    }
    if plan.files.is_empty() {
        bail!("no file of {} at {} was selected", e.repo, plan.id.revision);
    }
    let fetched = store.get(&plan.id)?.is_none();
    let s = store.fetch(client, &plan, &mut |p| {
        if p.files_total > 0 && p.files_done < p.files_total {
            eprint!(
                "\r{}: {}/{} files, {:.0}/{:.0} MB",
                e.repo,
                p.files_done,
                p.files_total,
                p.bytes_done as f64 / 1e6,
                p.bytes_total as f64 / 1e6
            );
        } else if p.files_total > 0 {
            eprintln!();
        }
    })?;
    Ok((plan.id, s.dir, fetched))
}
