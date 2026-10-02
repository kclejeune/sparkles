//! `sparkles queries`: the stored queries of a database directory (C16): list, show,
//! store, delete and run them.

use anyhow::{Context, Result, bail};
use serde_json::Value;
use sparkles::sparql::QueryKind;
use sparkles::sparql::results::{self, SolutionsFormat};
use sparkles::store::{Store, StoreOptions};
use sparkles::stored::{Catalog, Change, Definition, ParamType, Parameter};
use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

#[derive(clap::Subcommand)]
pub enum QueriesCmd {
    /// List the stored queries
    List {
        #[arg(long)]
        loc: PathBuf,
        /// text or json
        #[arg(long, default_value = "text")]
        format: String,
    },
    /// Print a stored query's definition
    Get {
        #[arg(long)]
        loc: PathBuf,
        name: String,
        /// An older version
        #[arg(long)]
        version: Option<u64>,
        /// text or json
        #[arg(long, default_value = "text")]
        format: String,
    },
    /// List the kept versions of a stored query
    Versions {
        #[arg(long)]
        loc: PathBuf,
        name: String,
    },
    /// Store a query (a new version when it changes)
    Put {
        #[arg(long)]
        loc: PathBuf,
        name: String,
        /// File with the SPARQL query
        #[arg(long, conflicts_with = "json", required_unless_present = "json")]
        query: Option<PathBuf>,
        /// File with the whole definition as JSON (as `PUT /$/queries/{ds}/{name}`)
        #[arg(long)]
        json: Option<PathBuf>,
        /// A parameter: NAME:TYPE or NAME:TYPE=DEFAULT (repeatable). Types: iri, string,
        /// integer, decimal, double, boolean, date, dateTime, literal, term
        #[arg(long, value_name = "NAME:TYPE[=DEFAULT]", conflicts_with = "json")]
        param: Vec<String>,
        #[arg(long, conflicts_with = "json")]
        description: Option<String>,
        /// The result format of runs that ask for none (json, xml, csv, tsv; turtle,
        /// ntriples, jsonld, rdfxml for graphs)
        #[arg(long, conflicts_with = "json")]
        results: Option<String>,
        /// Do not offer the query as an MCP tool
        #[arg(long, conflicts_with = "json")]
        no_mcp: bool,
        /// A message recorded with the version
        #[arg(long)]
        message: Option<String>,
    },
    /// Delete a stored query and its versions
    Delete {
        #[arg(long)]
        loc: PathBuf,
        name: String,
    },
    /// Run a stored query
    Run {
        #[arg(long)]
        loc: PathBuf,
        name: String,
        /// A parameter value: NAME=VALUE (repeatable)
        #[arg(long = "set", value_name = "NAME=VALUE")]
        set: Vec<String>,
        /// Run an older version
        #[arg(long)]
        version: Option<u64>,
        /// Output format: text, json, xml, csv, tsv (graphs: ttl, nt, jsonld, rdfxml);
        /// the definition's format by default
        #[arg(long)]
        results: Option<String>,
        #[arg(long)]
        timeout: Option<f64>,
    },
}

fn param_type(t: &str) -> Result<ParamType> {
    serde_json::from_value(Value::String(t.to_string())).map_err(|_| {
        anyhow::anyhow!(
            "unknown parameter type '{t}' (iri, string, integer, decimal, double, boolean, date, dateTime, literal, term)"
        )
    })
}

/// `NAME:TYPE[=DEFAULT]`
fn parse_param(s: &str) -> Result<(String, Parameter)> {
    let (name, rest) = s
        .split_once(':')
        .with_context(|| format!("--param {s}: expected NAME:TYPE[=DEFAULT]"))?;
    let (ty, default) = match rest.split_once('=') {
        Some((t, d)) => (t, Some(Value::String(d.to_string()))),
        None => (rest, None),
    };
    Ok((
        name.trim_start_matches(['?', '$']).to_string(),
        Parameter {
            kind: param_type(ty)?,
            description: None,
            default,
            required: None,
            datatype: None,
            language: None,
            allowed: None,
        },
    ))
}

fn print_params(out: &mut impl Write, def: &Definition) -> Result<()> {
    for (n, p) in &def.parameters {
        let default = p
            .default
            .as_ref()
            .map(|d| match d {
                Value::String(s) => format!(" = {s}"),
                v => format!(" = {v}"),
            })
            .unwrap_or_default();
        let req = if p.is_required() { " (required)" } else { "" };
        writeln!(out, "  ?{n}: {}{default}{req}", p.kind.name())?;
    }
    Ok(())
}

pub fn run(cmd: QueriesCmd, opts: StoreOptions) -> Result<()> {
    let mut out = std::io::stdout().lock();
    match cmd {
        QueriesCmd::List { loc, format } => {
            let c = open_catalog(&loc)?;
            let list = c.list();
            if format == "json" {
                let v: Vec<Value> = list
                    .iter()
                    .map(|(n, s)| {
                        serde_json::json!({"name": n, "definition": s.definition, "version": s.version})
                    })
                    .collect();
                serde_json::to_writer_pretty(&mut out, &v)?;
                writeln!(out)?;
            } else {
                for (n, s) in list {
                    let kind = s
                        .definition
                        .check()
                        .map(|k| format!("{k:?}").to_uppercase())
                        .unwrap_or_else(|_| "INVALID".into());
                    writeln!(
                        out,
                        "{n}  v{}  {kind}  {}",
                        s.version.version,
                        s.definition.description.as_deref().unwrap_or("")
                    )?;
                    print_params(&mut out, &s.definition)?;
                }
            }
        }
        QueriesCmd::Get {
            loc,
            name,
            version,
            format,
        } => {
            let c = open_catalog(&loc)?;
            let s = c
                .get(&name, version)
                .with_context(|| format!("no stored query '{name}'"))?;
            if format == "json" {
                serde_json::to_writer_pretty(
                    &mut out,
                    &serde_json::json!({"name": name, "definition": s.definition, "version": s.version}),
                )?;
                writeln!(out)?;
            } else {
                writeln!(
                    out,
                    "# {name}, version {} of {}{}",
                    s.version.version,
                    s.version.created,
                    s.version
                        .author
                        .as_deref()
                        .map(|a| format!(" by {a}"))
                        .unwrap_or_default()
                )?;
                if let Some(d) = &s.definition.description {
                    writeln!(out, "# {d}")?;
                }
                print_params(&mut out, &s.definition)?;
                writeln!(out, "{}", s.definition.query)?;
            }
        }
        QueriesCmd::Versions { loc, name } => {
            let c = open_catalog(&loc)?;
            for v in c
                .versions(&name)
                .with_context(|| format!("no stored query '{name}'"))?
            {
                writeln!(
                    out,
                    "v{}  {}  {}  {}",
                    v.version,
                    v.created,
                    v.author.as_deref().unwrap_or("-"),
                    v.message.as_deref().unwrap_or("")
                )?;
            }
        }
        QueriesCmd::Put {
            loc,
            name,
            query,
            json,
            param,
            description,
            results,
            no_mcp,
            message,
        } => {
            let def: Definition = match json {
                Some(f) => {
                    let mut j: Value = serde_json::from_slice(
                        &std::fs::read(&f).with_context(|| format!("reading {}", f.display()))?,
                    )
                    .with_context(|| format!("{}: not JSON", f.display()))?;
                    if let Some(o) = j.as_object_mut() {
                        for k in ["name", "dataset", "kind", "version", "message"] {
                            o.remove(k);
                        }
                    }
                    serde_json::from_value(j).context("invalid definition")?
                }
                None => {
                    let f = query.context("--query or --json is required")?;
                    let text = std::fs::read_to_string(&f)
                        .with_context(|| format!("reading {}", f.display()))?;
                    Definition {
                        query: text,
                        description,
                        parameters: param
                            .iter()
                            .map(|p| parse_param(p))
                            .collect::<Result<BTreeMap<_, _>>>()?,
                        results,
                        mcp: !no_mcp,
                    }
                }
            };
            // the store's lock keeps a server from changing the catalog meanwhile
            let store = Store::open(&loc, opts)?;
            let c = open_catalog(&loc)?;
            let saved = c.put(
                &name,
                def,
                Change {
                    author: std::env::var("USER").ok(),
                    message,
                    dataset_commit: Some(store.snapshot().commit),
                    if_version: None,
                },
            )?;
            if saved.changed {
                writeln!(out, "{name}: version {}", saved.stored.version.version)?;
            } else {
                writeln!(
                    out,
                    "{name}: unchanged at version {}",
                    saved.stored.version.version
                )?;
            }
        }
        QueriesCmd::Delete { loc, name } => {
            let _store = Store::open(&loc, opts)?;
            if !open_catalog(&loc)?.delete(&name, None)? {
                bail!("no stored query '{name}'");
            }
        }
        QueriesCmd::Run {
            loc,
            name,
            set,
            version,
            results: fmt,
            timeout,
        } => {
            let c = open_catalog(&loc)?;
            let s = c
                .get(&name, version)
                .with_context(|| format!("no stored query '{name}'"))?;
            let mut given = BTreeMap::new();
            for kv in &set {
                let (k, v) = kv
                    .split_once('=')
                    .with_context(|| format!("--set {kv}: expected NAME=VALUE"))?;
                given.insert(
                    k.trim_start_matches(['?', '$']).to_string(),
                    Value::String(v.to_string()),
                );
            }
            let store = Store::open(&loc, opts)?;
            let mut prefixes = sparkles::io::standard_prefixes();
            prefixes.extend(store.prefixes());
            let bindings = s.definition.bind(&given, &prefixes)?;
            let qopts = sparkles::sparql::QueryOptions {
                timeout: timeout.map(Duration::from_secs_f64),
                initial_bindings: bindings,
                ..Default::default()
            };
            let r = sparkles::sparql::query(store.snapshot(), &s.definition.query, &qopts)?;
            let fmt = fmt
                .or_else(|| s.definition.results.clone())
                .unwrap_or_else(|| "text".into());
            match r.kind {
                QueryKind::Select | QueryKind::Ask if fmt == "text" => {
                    crate::print_table(&r, &store, &mut out)?
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
        }
    }
    out.flush()?;
    Ok(())
}

fn open_catalog(loc: &Path) -> Result<Catalog> {
    if !loc.is_dir() {
        bail!("{} is not a database directory", loc.display());
    }
    Ok(Catalog::open(Some(loc))?)
}
