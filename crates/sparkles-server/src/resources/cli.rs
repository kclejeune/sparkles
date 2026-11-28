//! Resource-catalog commands use the running server's authorization and load policy.

use anyhow::{Context, Result, bail};
use clap::{Args, Subcommand};
use reqwest::Method;
use serde_json::json;
use std::path::PathBuf;

#[derive(Args, Debug)]
pub(crate) struct SourceArgs {
    /// The running Sparkles server
    #[arg(long, global = true, env = "SPARKLES_SERVER")]
    server: Option<String>,
    #[arg(long, global = true)]
    insecure_http: bool,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// List source records and versions
    List {
        /// Include vocabulary and dataset records
        #[arg(long)]
        all: bool,
    },
    /// Register a resource and its versions from a catalog creation document
    Add { document: PathBuf },
    /// Remove a source that the catalog does not reference
    Delete { resource: String },
    /// Resolve a recipe's graph targets and checksum-pinned files
    Plan { recipe: PathBuf },
    /// Preview entries from a public metadata document or Qleverfile
    Harvest { kind: String, document: PathBuf },
    /// Resolve the graph source pins of a dataset version into a recipe
    Recipe { resource: String, version: String },
    /// Replace a main dataset's named graphs using a verified recipe
    Load { dataset: String, recipe: PathBuf },
    /// Read the source manifest of a commit
    Manifest { dataset: String, commit: u64 },
    /// Replace the graphs of a previous load from the same pinned files
    Reload { dataset: String, commit: u64 },
    /// Read the pinned import lock for a version
    Lock { resource: String, version: String },
    /// Verify an import lock against the catalog
    CheckLock {
        resource: String,
        version: String,
        lock: PathBuf,
    },
    /// Install a pinned vocabulary and its imported graphs and prefixes
    Bundle {
        dataset: String,
        resource: String,
        version: String,
    },
    /// Reserve a vocabulary IRI according to its policy
    Mint {
        dataset: String,
        resource: String,
        #[arg(long)]
        label: Option<String>,
    },
}

fn version_path(resource: &str, version: &str) -> Result<String> {
    resource_id(resource)?;
    if !super::valid_version(version) {
        bail!("invalid resource version {version:?}");
    }
    Ok(format!("/$/resources/{resource}/versions/{version}"))
}

fn resource_id(s: &str) -> Result<&str> {
    if !super::valid_id(s) {
        bail!("invalid resource id {s:?}");
    }
    Ok(s)
}

fn dataset_name(s: &str) -> Result<&str> {
    if !sparkles::catalog::valid_name(s) {
        bail!("invalid dataset name {s:?}");
    }
    Ok(s)
}

pub(crate) fn run(args: SourceArgs) -> Result<()> {
    use crate::remote::{JsonBody, Remote};
    if crate::branch_cmd::branch().is_some() {
        bail!("source commands use the main branch");
    }
    let remote = Remote::open(args.server.as_deref(), args.insecure_http)?;
    let (method, path, body) = match args.command {
        Command::List { all } => (
            Method::GET,
            if all { "/$/resources" } else { "/$/sources" }.into(),
            None,
        ),
        Command::Add { document } => {
            let bytes =
                std::fs::read(&document).with_context(|| format!("read {}", document.display()))?;
            (Method::POST, "/$/resources".into(), Some(bytes))
        }
        Command::Delete { resource } => {
            let path = format!("/$/resources/{}", resource_id(&resource)?);
            let existing = remote.check(remote.req(Method::GET, &path).send(), None)?;
            let etag = existing
                .headers()
                .get(reqwest::header::ETAG)
                .cloned()
                .context("the resource has no ETag")?;
            if existing.json_value()?["kind"] != "source" {
                bail!("the catalog record is not a source");
            }
            remote.check(
                remote
                    .req(Method::DELETE, &path)
                    .header(reqwest::header::IF_MATCH, etag)
                    .send(),
                None,
            )?;
            return Ok(());
        }
        Command::Plan { recipe } => (
            Method::POST,
            "/$/sources/plan".into(),
            Some(std::fs::read(recipe)?),
        ),
        Command::Harvest { kind, document } => (
            Method::POST,
            "/$/sources/harvest".into(),
            Some(serde_json::to_vec(
                &json!({"kind": kind, "document": std::fs::read_to_string(document)?}),
            )?),
        ),
        Command::Recipe { resource, version } => (
            Method::GET,
            format!("{}/recipe", version_path(&resource, &version)?),
            None,
        ),
        Command::Load { dataset, recipe } => (
            Method::POST,
            format!("/$/sources/{}/load", dataset_name(&dataset)?),
            Some(std::fs::read(recipe)?),
        ),
        Command::Manifest { dataset, commit } => (
            Method::GET,
            format!("/$/sources/{}/manifests/{commit}", dataset_name(&dataset)?),
            None,
        ),
        Command::Reload { dataset, commit } => (
            Method::POST,
            format!(
                "/$/sources/{}/manifests/{commit}/reload",
                dataset_name(&dataset)?
            ),
            None,
        ),
        Command::Lock { resource, version } => (
            Method::GET,
            format!("{}/lock", version_path(&resource, &version)?),
            None,
        ),
        Command::CheckLock {
            resource,
            version,
            lock,
        } => (
            Method::POST,
            format!("{}/lock", version_path(&resource, &version)?),
            Some(std::fs::read(lock)?),
        ),
        Command::Bundle {
            dataset,
            resource,
            version,
        } => (
            Method::POST,
            format!(
                "{}/bundle/{}",
                version_path(&resource, &version)?,
                dataset_name(&dataset)?
            ),
            None,
        ),
        Command::Mint {
            dataset,
            resource,
            label,
        } => (
            Method::POST,
            format!(
                "/$/resources/{}/mint/{}",
                resource_id(&resource)?,
                dataset_name(&dataset)?
            ),
            Some(serde_json::to_vec(&super::mint::Request { label })?),
        ),
    };
    let mut request = remote.req(method, &path);
    if let Some(body) = body {
        request = request
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(body);
    }
    let value = remote.check(request.send(), None)?.json_value()?;
    println!("{}", serde_json::to_string_pretty(&value)?);
    Ok(())
}
