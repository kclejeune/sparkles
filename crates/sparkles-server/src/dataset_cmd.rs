//! Dataset management over a stopped server's catalog or the HTTP API.
use anyhow::{Context, Result, bail};
use clap::{Args, Subcommand};
use serde_json::{Value as J, json};
use sparkles::catalog::{CloneRequest, CreateDataset, DatasetInfo, DatasetKind};
use sparkles::store::StoreOptions;
use std::path::PathBuf;

#[derive(Args, Debug)]
pub struct Target {
    /// A stopped server's data directory (takes its catalog lock)
    #[arg(long, required_unless_present = "server", conflicts_with = "server")]
    data_dir: Option<PathBuf>,
    /// Manage a running server instead
    #[arg(long, env = "SPARKLES_SERVER")]
    server: Option<String>,
    #[arg(long)]
    insecure_http: bool,
    #[arg(long, default_value = "text", value_parser = ["text", "json"])]
    format: String,
    #[arg(long)]
    json: bool,
}

#[derive(Subcommand, Debug)]
pub enum DatasetCmd {
    /// List datasets
    List {
        #[command(flatten)]
        target: Target,
    },
    /// Create a dataset
    Create {
        #[command(flatten)]
        target: Target,
        name: String,
        #[arg(long, default_value = "persistent", value_parser = ["persistent", "mem", "memory"])]
        kind: String,
    },
    /// Delete a dataset and its managed storage
    Delete {
        #[command(flatten)]
        target: Target,
        name: String,
    },
    /// Rename a dataset, preserving its UUID
    Rename {
        #[command(flatten)]
        target: Target,
        name: String,
        new_name: String,
    },
    /// Clone a consistent dataset snapshot
    Clone {
        #[command(flatten)]
        target: Target,
        source: String,
        name: String,
        #[arg(long)]
        at: Option<String>,
        #[arg(long, default_value = "copy", value_parser = ["copy", "drop"])]
        inferences: String,
    },
}
impl DatasetCmd {
    fn target(&self) -> &Target {
        match self {
            Self::List { target }
            | Self::Create { target, .. }
            | Self::Delete { target, .. }
            | Self::Rename { target, .. }
            | Self::Clone { target, .. } => target,
        }
    }
}

fn info(d: DatasetInfo) -> J {
    json!({"name": d.name, "id": d.id, "type": d.kind, "path": d.path,
        "attached": d.attached, "reservedBy": d.reserved_by})
}

pub fn run(cmd: DatasetCmd, opts: StoreOptions) -> Result<()> {
    if crate::branch_cmd::branch().is_some() {
        bail!("dataset commands manage whole datasets; --branch is not supported");
    }
    let target = cmd.target();
    let value = if let Some(dir) = &target.data_dir {
        let catalog = sparkles::Catalog::open(dir, opts.into())?;
        match &cmd {
            DatasetCmd::List { .. } => {
                json!({"datasets": catalog.list().into_iter().map(info).collect::<Vec<_>>()})
            }
            DatasetCmd::Create { name, kind, .. } => {
                drop(catalog.create(
                    name,
                    &CreateDataset {
                        kind: if kind == "persistent" {
                            DatasetKind::Persistent
                        } else {
                            DatasetKind::Memory
                        },
                        ..Default::default()
                    },
                )?);
                info(catalog.info(name).context("created dataset")?)
            }
            DatasetCmd::Delete { name, .. } => {
                if !catalog.delete(name)? {
                    bail!("no dataset /{name}");
                }
                json!({"deleted": name})
            }
            DatasetCmd::Rename { name, new_name, .. } => {
                drop(catalog.rename(name, new_name)?);
                info(catalog.info(new_name).context("renamed dataset")?)
            }
            DatasetCmd::Clone {
                source,
                name,
                at,
                inferences,
                ..
            } => {
                drop(
                    catalog.clone_dataset(
                        source,
                        name,
                        &CloneRequest {
                            spec: sparkles::cloning::Spec {
                                at: at.as_deref().map(str::parse).transpose()?,
                                inferences: sparkles::cloning::Inferences::parse(inferences)
                                    .context("inferences")?,
                                ..Default::default()
                            },
                            ..Default::default()
                        },
                        &Default::default(),
                    )?,
                );
                info(catalog.info(name).context("cloned dataset")?)
            }
        }
    } else {
        remote(&cmd)?
    };
    if target.json || target.format == "json" {
        println!("{}", serde_json::to_string_pretty(&value)?);
    } else if let Some(datasets) = value["datasets"].as_array() {
        println!("name  type  id");
        for d in datasets {
            println!(
                "{}  {}  {}",
                d["name"].as_str().unwrap_or(""),
                d["type"].as_str().unwrap_or(""),
                d["id"].as_str().unwrap_or("")
            );
        }
    } else if let Some(name) = value["deleted"].as_str() {
        println!("deleted /{name}");
    } else {
        println!(
            "/{} {}",
            value["name"].as_str().unwrap_or(""),
            value["id"].as_str().unwrap_or("")
        );
    }
    Ok(())
}

#[cfg(feature = "auth")]
fn remote(cmd: &DatasetCmd) -> Result<J> {
    use crate::remote::{JsonBody, Remote};
    use reqwest::Method;
    let target = cmd.target();
    let remote = Remote::open(target.server.as_deref(), target.insecure_http)?;
    let valid = |name: &str| -> Result<()> {
        if !sparkles::catalog::valid_name(name) {
            bail!("invalid dataset name {name:?}");
        }
        Ok(())
    };
    let (method, path, body, dataset) = match cmd {
        DatasetCmd::List { .. } => (Method::GET, "/$/datasets".into(), None, None),
        DatasetCmd::Create { name, kind, .. } => {
            valid(name)?;
            (
                Method::POST,
                "/$/datasets".into(),
                Some(json!({"dbName": name,
                "dbType": if kind == "persistent" {"persistent"} else {"mem"}})),
                None,
            )
        }
        DatasetCmd::Delete { name, .. } => {
            valid(name)?;
            (
                Method::DELETE,
                format!("/$/datasets/{name}"),
                None,
                Some(name.as_str()),
            )
        }
        DatasetCmd::Rename { name, new_name, .. } => {
            valid(name)?;
            valid(new_name)?;
            (
                Method::POST,
                format!("/$/datasets/{name}/rename"),
                Some(json!({"name": new_name})),
                Some(name.as_str()),
            )
        }
        DatasetCmd::Clone {
            source,
            name,
            at,
            inferences,
            ..
        } => {
            valid(source)?;
            valid(name)?;
            (
                Method::POST,
                format!("/$/datasets/{source}/clone"),
                Some(json!({"name": name, "at": at, "inferences": inferences})),
                Some(source.as_str()),
            )
        }
    };
    let mut req = remote.req(method, &path);
    if let Some(body) = body {
        req = req
            .header("content-type", "application/json")
            .body(body.to_string());
    }
    let response = remote.check(req.send(), dataset)?;
    if let DatasetCmd::Delete { name, .. } = cmd {
        return Ok(json!({"deleted": name}));
    }
    let mut value = response.json_value()?;
    if matches!(cmd, DatasetCmd::Clone { .. }) {
        let id = value["id"].as_str().context("clone task id")?.to_string();
        loop {
            value = remote
                .check(
                    remote.req(Method::GET, &format!("/$/tasks/{id}")).send(),
                    dataset,
                )?
                .json_value()?;
            match value["state"].as_str() {
                Some("done") => break,
                Some("failed" | "cancelled" | "superseded") => {
                    bail!("clone task {id}: {}", value["message"])
                }
                _ => std::thread::sleep(std::time::Duration::from_millis(100)),
            }
        }
    }
    if let DatasetCmd::Create { name, .. }
    | DatasetCmd::Clone { name, .. }
    | DatasetCmd::Rename { new_name: name, .. } = cmd
    {
        return remote
            .check(
                remote
                    .req(Method::GET, &format!("/$/datasets/{name}"))
                    .send(),
                Some(name),
            )?
            .json_value();
    }
    Ok(value)
}

#[cfg(not(feature = "auth"))]
fn remote(_: &DatasetCmd) -> Result<J> {
    bail!("--server: built without the remote client (cargo feature \"auth\")")
}
