//! `sparkles fuseki-config convert` and `serve --fuseki-config` (spec G08): a Fuseki
//! configuration (`config.ttl`, `run/configuration/*.ttl`, `shiro.ini` or the password
//! file) converted into `serve` flags, settings files and an auth configuration, with a
//! report of every element.

pub mod access;
pub mod convert;
pub mod endpoints;
pub mod graph;
mod parts;
pub mod report;
mod service;
pub mod users;
pub mod write;

#[cfg(test)]
mod tests;

use anyhow::{Context, Result, bail};
use convert::{Inputs, Mode, Plan};
use graph::{ConfigGraph, FUSEKI, format_of};
use std::path::{Path, PathBuf};

#[derive(clap::Args)]
pub struct FusekiConfigArgs {
    #[command(subcommand)]
    cmd: FusekiConfigCmd,
}

#[derive(clap::Subcommand)]
enum FusekiConfigCmd {
    /// Convert a Fuseki configuration into a `serve` script, settings files and an auth
    /// configuration, and report what was converted, approximated or unsupported (exit
    /// status 1 when something important could not be converted, 2 on an error)
    Convert {
        /// Fuseki configuration files (config.ttl and service files), or Fuseki base
        /// directories holding config.ttl, configuration/ and shiro.ini
        #[arg(required = true, value_name = "PATH")]
        inputs: Vec<PathBuf>,
        /// Directory to write serve.sh, load.sh, auth.toml, the dataset settings and
        /// report.txt into
        #[arg(long, default_value = "sparkles-config")]
        out: PathBuf,
        /// Only print the report: write nothing and hash no password
        #[arg(long)]
        check: bool,
        /// Write into --out even when it is not empty
        #[arg(long)]
        force: bool,
        /// Shiro's shiro.ini with the users and URL rules (default: a shiro.ini next to
        /// the configuration)
        #[arg(long, value_name = "FILE", conflicts_with = "passwd")]
        shiro: Option<PathBuf>,
        /// The password file of fuseki:passwd (default: the file the configuration names)
        #[arg(long, value_name = "FILE")]
        passwd: Option<PathBuf>,
        /// Report format: text or json
        #[arg(long, default_value = "text", value_parser = ["text", "json"])]
        format: String,
    },
}

/// Where the users come from.
#[derive(Default)]
pub struct UserSource {
    pub shiro: Option<PathBuf>,
    pub passwd: Option<PathBuf>,
}

/// Read the configuration files and the user file.
pub fn read_inputs(paths: &[PathBuf], users: &UserSource) -> Result<(Inputs, Vec<String>)> {
    let mut files: Vec<PathBuf> = Vec::new();
    let mut shiro_found: Option<PathBuf> = None;
    let mut notes: Vec<String> = Vec::new();
    let rdf_files = |dir: &Path| -> Result<Vec<PathBuf>> {
        let mut v: Vec<PathBuf> = std::fs::read_dir(dir)
            .with_context(|| format!("reading {}", dir.display()))?
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| {
                p.is_file()
                    && format_of(p).is_some()
                    && !p
                        .file_name()
                        .is_some_and(|n| n.to_string_lossy().starts_with('.'))
            })
            .collect();
        v.sort();
        Ok(v)
    };
    for p in paths {
        if p.is_dir() {
            let cfg = p.join("config.ttl");
            if cfg.is_file() {
                files.push(cfg);
            }
            let conf = p.join("configuration");
            if conf.is_dir() {
                files.extend(rdf_files(&conf)?);
            }
            let s = p.join("shiro.ini");
            if s.is_file() && shiro_found.is_none() {
                shiro_found = Some(s);
            }
        } else if p.is_file() {
            files.push(p.clone());
            let dir = p.parent().map(Path::to_path_buf).unwrap_or_default();
            let dir = if dir.as_os_str().is_empty() {
                PathBuf::from(".")
            } else {
                dir
            };
            let conf = dir.join("configuration");
            if p.file_name().is_some_and(|n| n == "config.ttl") && conf.is_dir() {
                for f in rdf_files(&conf)? {
                    if !paths.contains(&f) && !files.contains(&f) {
                        notes.push(format!(
                            "{} (next to {})",
                            graph::shown(&f),
                            graph::shown(p)
                        ));
                        files.push(f);
                    }
                }
            }
            let s = dir.join("shiro.ini");
            if s.is_file() && shiro_found.is_none() {
                shiro_found = Some(s);
            }
        } else {
            bail!("{}: no such file or directory", p.display());
        }
    }
    if files.is_empty() {
        bail!("no configuration file found (a directory needs config.ttl or configuration/)");
    }
    let mut graphs = Vec::new();
    for f in &files {
        graphs.push(ConfigGraph::read(f)?);
    }
    let mut read: Vec<String> = files.iter().map(|f| graph::shown(f)).collect();
    for n in notes {
        if let Some(r) = read.iter_mut().find(|r| n.starts_with(r.as_str())) {
            *r = n;
        }
    }

    // the user file: --shiro, --passwd, fuseki:passwd, then a shiro.ini nearby
    let server_passwd = graphs.iter().find_map(|g| {
        g.of_type(&format!("{FUSEKI}Server"))
            .into_iter()
            .find_map(|s| {
                g.one(&s, &format!("{FUSEKI}passwd"))
                    .ok()
                    .flatten()
                    .and_then(|t| g.file_ref(t))
            })
    });
    let (path, shiro) = if let Some(s) = &users.shiro {
        (Some(s.clone()), true)
    } else if let Some(p) = &users.passwd {
        (Some(p.clone()), false)
    } else if let Some(p) = &server_passwd {
        (Some(p.clone()), false)
    } else if let Some(s) = shiro_found {
        (Some(s), true)
    } else {
        (None, false)
    };
    let user_file = match path {
        Some(p) => {
            let f = if shiro {
                users::read_shiro(&p)
            } else {
                users::read_passwd(&p)
            };
            match f {
                Ok(f) => {
                    read.push(graph::shown(&p));
                    Some((p, f))
                }
                Err(_) if users.shiro.is_none() && users.passwd.is_none() => {
                    // the file fuseki:passwd names, relative to Fuseki's directory
                    read.push(format!(
                        "{} (not readable; pass --passwd)",
                        graph::shown(&p)
                    ));
                    None
                }
                Err(e) => return Err(e),
            }
        }
        None => None,
    };
    Ok((
        Inputs {
            graphs,
            users: user_file,
            shiro,
        },
        read,
    ))
}

/// Read and convert.
pub fn plan_for(paths: &[PathBuf], users: &UserSource, mode: Mode) -> Result<Plan> {
    let (inputs, read) = read_inputs(paths, users)?;
    let mut plan = convert::convert(&inputs, mode)?;
    let unreadable_passwd = read.iter().any(|r| r.contains("pass --passwd"));
    plan.report.inputs = read;
    if unreadable_passwd {
        plan.report.unsupported(
            "server",
            "the password file of fuseki:passwd cannot be read, so no user is converted",
        );
    }
    Ok(plan)
}

/// `sparkles fuseki-config …`
pub fn run(args: FusekiConfigArgs) -> Result<()> {
    match args.cmd {
        FusekiConfigCmd::Convert {
            inputs,
            out,
            check,
            force,
            shiro,
            passwd,
            format,
        } => {
            let users = UserSource { shiro, passwd };
            let mut plan = match plan_for(&inputs, &users, Mode::Files) {
                Ok(p) => p,
                Err(e) => {
                    eprintln!("error: {e:#}");
                    std::process::exit(2);
                }
            };
            if !check && let Err(e) = write::write_dir(&mut plan, &out, force) {
                eprintln!("error: {e:#}");
                std::process::exit(2);
            }
            if format == "json" {
                println!("{}", serde_json::to_string_pretty(&plan.report)?);
            } else {
                print!("{}", plan.report.text());
                if !check {
                    println!("wrote {}: run load.sh once, then serve.sh", out.display());
                }
            }
            if plan.report.has_unsupported() {
                std::process::exit(1);
            }
            Ok(())
        }
    }
}

/// What `serve --fuseki-config` adds to the server's own flags.
#[derive(Default, Debug)]
pub struct ServeAdditions {
    pub mem: Vec<String>,
    pub loc: Vec<String>,
    pub text: Vec<String>,
    pub geo: Vec<String>,
    pub rdfs: Vec<String>,
    pub auth_config: Option<PathBuf>,
    pub timeout: Option<f64>,
    pub update_timeout: Option<f64>,
    pub union_default_graph: bool,
    pub read_only: bool,
    pub gsp_direct_naming: bool,
    pub metrics_fuseki_names: bool,
    pub auto_reason: bool,
    /// data files to load into in-memory datasets: dataset, file, graph
    pub data: Vec<(String, PathBuf, Option<String>)>,
    /// datasets whose inferences to materialize at the start
    pub reason: Vec<(String, convert::Reasoning, Option<PathBuf>)>,
}

/// `serve --fuseki-config PATH`: convert, refuse what is unsupported, and write the
/// settings files under `<data>/fuseki`.
pub fn for_serve(path: &Path, data_dir: &Path, own_auth: bool) -> Result<ServeAdditions> {
    let mut plan = plan_for(&[path.to_path_buf()], &UserSource::default(), Mode::Serve)?;
    let dir = data_dir.join("fuseki");
    std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    let mut report = std::mem::take(&mut plan.report);
    write::write_dataset_files(&plan, &dir, &mut report)?;
    let mut add = ServeAdditions::default();
    if let Some(a) = &plan.auth {
        if own_auth {
            report.approximated(
                "server",
                "--auth-config is given, so the converted access rules are not used",
            );
        } else {
            let toml = write::auth_toml(a, plan.server.realm.as_deref(), &mut report)?;
            let p = dir.join("auth.toml");
            write::write_private(&p, toml.as_bytes())?;
            add.auth_config = Some(p);
        }
    }
    for line in report.text().lines() {
        if !line.is_empty() {
            tracing::info!("fuseki-config: {line}");
        }
    }
    if report.has_unsupported() {
        let bad: Vec<String> = report
            .items
            .iter()
            .filter(|i| i.kind == report::Kind::Unsupported)
            .map(|i| format!("  {}: {}", i.place, i.message))
            .collect();
        bail!(
            "--fuseki-config {}: these parts have no Sparkles equivalent; convert the \
             configuration with `sparkles fuseki-config convert` and edit the result:\n{}",
            path.display(),
            bad.join("\n")
        );
    }
    let s = &plan.server;
    add.timeout = s.timeout;
    add.update_timeout = s.update_timeout;
    add.union_default_graph = s.union_default_graph;
    add.read_only = s.read_only;
    add.gsp_direct_naming = s.gsp_direct_naming;
    add.metrics_fuseki_names = s.metrics_fuseki_names;
    add.auto_reason = s.auto_reason;
    for ds in &plan.datasets {
        let files = write::dataset_files(ds);
        if ds.persistent(Mode::Serve) {
            add.loc.push(format!(
                "{}={}",
                ds.name,
                dir.join(write::db_dir(ds)).display()
            ));
            if let Some(t) = &ds.tdb {
                tracing::warn!(
                    "fuseki-config: /{} starts empty; export Fuseki's database at {} with \
                     tdb2.tdbdump and load it with sparkles load or POST /{}/data",
                    ds.name,
                    t.display(),
                    ds.name
                );
            }
        } else {
            add.mem.push(ds.name.clone());
            for f in &ds.data {
                add.data
                    .push((ds.name.clone(), f.path.clone(), f.graph.clone()));
            }
            for f in ds.reasoning.iter().flat_map(|r| &r.schema) {
                add.data.push((
                    ds.name.clone(),
                    f.clone(),
                    Some(convert::SCHEMA_GRAPH.to_string()),
                ));
            }
        }
        if let Some(p) = files.text {
            add.text
                .push(format!("{}={}", ds.name, dir.join(p).display()));
        }
        if let Some(p) = files.geo {
            add.geo
                .push(format!("{}={}", ds.name, dir.join(p).display()));
        }
        if let Some(p) = files.rdfs {
            add.rdfs
                .push(format!("{}={}", ds.name, dir.join(p).display()));
        }
        if let Some(r) = &ds.reasoning {
            add.reason
                .push((ds.name.clone(), r.clone(), files.rules.map(|p| dir.join(p))));
        }
    }
    Ok(add)
}

/// After the datasets are attached: load the data files of in-memory datasets, and
/// materialize the inferences of datasets that have none recorded yet.
pub fn start(st: &std::sync::Arc<crate::state::AppState>, add: &ServeAdditions) -> Result<()> {
    for (name, path, graph) in &add.data {
        let ds = st
            .get(name)
            .with_context(|| format!("--fuseki-config: no dataset /{name}"))?;
        let g = graph.as_deref().map(oxrdf::NamedNode::new).transpose()?;
        let src = sparkles::io::Source::from_path(path, g)?;
        ds.store
            .load_with(
                &[src],
                sparkles::commit::CommitKind::Load,
                &sparkles::guard::WriteOptions::default(),
            )
            .with_context(|| format!("--fuseki-config: loading {} into /{name}", path.display()))?;
        tracing::info!("fuseki-config: loaded {} into /{name}", path.display());
    }
    #[cfg(feature = "reasoning")]
    for (name, r, rules) in &add.reason {
        let ds = st
            .get(name)
            .with_context(|| format!("--fuseki-config: no dataset /{name}"))?;
        if ds.reasoning.read().is_some() {
            continue;
        }
        let profile = match rules {
            Some(p) => sparkles_reasoner::Profile::Rules(
                std::fs::read_to_string(p).with_context(|| format!("reading {}", p.display()))?,
            ),
            None => r
                .profile
                .unwrap_or("rdfs")
                .parse()
                .map_err(|_| anyhow::anyhow!("unknown profile"))?,
        };
        let vocabs: Vec<&str> = if r.geo_vocab {
            vec!["geosparql"]
        } else {
            vec![]
        };
        let extras = sparkles_reasoner::Extras::parse(&vocabs, r.default_geometry)
            .map_err(|e| anyhow::anyhow!("{e}"))?;
        let mut inputs = sparkles_reasoner::Inputs::default();
        if !r.schema.is_empty() {
            inputs.ontology_graphs = vec![sparkles_reasoner::GraphRef::Named(
                convert::SCHEMA_GRAPH.into(),
            )];
        }
        let task = crate::reasoning::start_reason(
            st,
            ds,
            profile,
            extras,
            inputs,
            crate::reasoning::Trigger::Request,
            false,
            false,
        );
        tracing::info!(
            "fuseki-config: materializing the inferences of /{name} (task {})",
            task.id
        );
    }
    #[cfg(not(feature = "reasoning"))]
    if !add.reason.is_empty() {
        bail!("--fuseki-config: the configuration has inference, and this build has no reasoner");
    }
    Ok(())
}
