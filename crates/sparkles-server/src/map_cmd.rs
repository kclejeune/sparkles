//! `sparkles map` (spec F19 §2.1): run, check and convert R2RML, RML and YARRRML
//! mappings, draft an R2RML mapping from a SQLite database, and manage the mappings
//! stored with a database (`--loc`) or on a server (`--server` and `--dataset`).

use crate::csv_cmd;
use anyhow::{Context, Result, bail};
use clap::{Args, Subcommand};
use sparkles::mapping::{self, MappingKind};
use sparkles::store::StoreOptions;
use sparkles::stored_mappings::{Catalog, Change, Definition};
use std::io::Write;
use std::path::{Path, PathBuf};

#[derive(Args, Debug)]
pub struct MapArgs {
    #[command(subcommand)]
    pub cmd: MapCmd,
}

/// Where the inputs of a run come from.
#[derive(Args, Debug, Clone, Default)]
pub struct RunInputs {
    /// A file that a logical source names: NAME=PATH (repeatable). Other relative names
    /// resolve against the mapping file's directory
    #[arg(long, value_name = "NAME=PATH")]
    pub source: Vec<String>,
    /// A database connection: NAME=URL with sqlite:PATH (repeatable). An R2RML mapping
    /// reads the only one, or the one named default
    #[arg(long, value_name = "NAME=URL")]
    pub connection: Vec<String>,
    /// The base IRI of generated relative IRIs, over the mapping's own
    #[arg(long, value_name = "IRI")]
    pub base: Option<String>,
    /// The kind of the mapping when its content does not tell: rml or yarrrml
    #[arg(long, value_name = "KIND")]
    pub mapping_kind: Option<String>,
    /// JSON sources: the largest document, in MiB (default 512)
    #[arg(long, value_name = "MB")]
    pub max_json_mb: Option<u64>,
}

/// A database or a server that holds stored mappings.
#[derive(Args, Debug, Clone, Default)]
pub struct Target {
    /// A database directory
    #[arg(long, conflicts_with = "server")]
    pub loc: Option<PathBuf>,
    /// A server (or SPARKLES_SERVER)
    #[arg(long)]
    pub server: Option<String>,
    /// The server's dataset
    #[arg(long, requires = "server")]
    pub dataset: Option<String>,
    /// Allow plain http:// to a server that is not on this machine
    #[arg(long)]
    pub insecure_http: bool,
}

#[derive(Subcommand, Debug)]
pub enum MapCmd {
    /// Run a mapping file and write its quads (or, with --loc or --server and a stored
    /// mapping's NAME, run that mapping into the dataset)
    Run {
        /// The mapping file, or a stored mapping's name with --loc or --server
        mapping: String,
        #[command(flatten)]
        inputs: RunInputs,
        /// Output syntax: nq (default), nt or ttl
        #[arg(long, default_value = "nq", value_parser = ["nq", "nt", "ttl"])]
        format: String,
        /// Write here instead of standard output, through a temporary file
        #[arg(long, short = 'o', value_name = "FILE")]
        output: Option<PathBuf>,
        #[command(flatten)]
        target: Target,
        /// A stored run: add (default) or replace the graphs the mapping writes
        #[arg(long, value_parser = ["add", "replace"])]
        mode: Option<String>,
        /// A stored run: report what would change and commit nothing
        #[arg(long)]
        dry_run: bool,
        /// A stored run: the commit message
        #[arg(long)]
        message: Option<String>,
    },
    /// Parse a mapping and list its triples maps, sources and joins; check the columns
    /// it references in the CSV files it can read
    Check {
        /// The mapping file (none with --functions)
        mapping: Option<PathBuf>,
        #[command(flatten)]
        inputs: RunInputs,
        /// List the functions that function maps may use
        #[arg(long)]
        functions: bool,
    },
    /// Print a YARRRML or RML mapping as RML-Core Turtle, or as R2RML when every source
    /// is a database
    Convert {
        mapping: PathBuf,
        /// rml or r2rml
        #[arg(long, default_value = "rml", value_parser = ["rml", "r2rml"])]
        to: String,
        #[arg(long, value_name = "KIND")]
        mapping_kind: Option<String>,
    },
    /// Print an R2RML mapping in the Direct Mapping's shape for a SQLite database
    Draft {
        /// NAME=URL with sqlite:PATH
        #[arg(long, value_name = "NAME=URL")]
        connection: String,
        /// Only these tables (comma-separated)
        #[arg(long, value_delimiter = ',')]
        tables: Vec<String>,
        /// The base IRI of the subjects, classes and predicates
        #[arg(long, default_value = "http://example.com/base/")]
        base: String,
    },
    /// Store a mapping with a database or on a server
    Put {
        name: String,
        file: PathBuf,
        #[command(flatten)]
        target: Target,
        #[arg(long, value_name = "KIND")]
        mapping_kind: Option<String>,
        #[arg(long)]
        description: Option<String>,
        /// A message recorded with the version
        #[arg(long)]
        message: Option<String>,
    },
    /// Print a stored mapping's text
    Get {
        name: String,
        #[command(flatten)]
        target: Target,
        #[arg(long)]
        version: Option<u64>,
    },
    /// List the stored mappings
    List {
        #[command(flatten)]
        target: Target,
    },
    /// Delete a stored mapping
    Delete {
        name: String,
        #[command(flatten)]
        target: Target,
    },
}

impl Target {
    fn is_set(&self) -> bool {
        self.loc.is_some() || self.server.is_some()
    }
}

fn kind_of(path: &Path, explicit: Option<&str>) -> Result<MappingKind> {
    csv_cmd::file_kind(path, explicit)
}

fn engine_kind(path: &Path, explicit: Option<&str>) -> Result<MappingKind> {
    match kind_of(path, explicit)? {
        k @ (MappingKind::Rml | MappingKind::Yarrrml) => Ok(k),
        _ => bail!(
            "{}: CSVW metadata and CONSTRUCT templates map CSV files: use sparkles csv convert or sparkles load",
            path.display()
        ),
    }
}

fn inputs_for(m: &mapping::Mapping, file: Option<&Path>, a: &RunInputs) -> Result<mapping::Inputs> {
    let mut inputs = mapping::Inputs {
        files: Vec::new(),
        dir: file.and_then(Path::parent).map(|d| {
            if d.as_os_str().is_empty() {
                PathBuf::from(".")
            } else {
                d.to_path_buf()
            }
        }),
        connections: csv_cmd::connections(&a.connection)?,
    };
    for s in &a.source {
        let (n, p) = s
            .split_once('=')
            .with_context(|| format!("--source {s}: expected NAME=PATH"))?;
        inputs.files.push((n.to_string(), PathBuf::from(p)));
    }
    let _ = m;
    Ok(inputs)
}

fn run_options(a: &RunInputs) -> mapping::RunOptions {
    mapping::RunOptions {
        base: a.base.clone(),
        max_json_bytes: a
            .max_json_mb
            .map_or(mapping::DEFAULT_MAX_JSON_BYTES, |m| m << 20),
        ..Default::default()
    }
}

pub fn run(args: MapArgs, opts: StoreOptions) -> Result<()> {
    match args.cmd {
        MapCmd::Run {
            mapping: m,
            inputs,
            format,
            output,
            target,
            mode,
            dry_run,
            message,
        } => {
            if target.is_set() {
                return run_stored(
                    &m,
                    &inputs,
                    &target,
                    mode.as_deref(),
                    dry_run,
                    message.as_deref(),
                    opts,
                );
            }
            if mode.is_some() || dry_run || message.is_some() {
                bail!(
                    "--mode, --dry-run and --message run a stored mapping: give --loc or --server"
                );
            }
            let path = PathBuf::from(&m);
            let kind = engine_kind(&path, inputs.mapping_kind.as_deref())?;
            let mapping = csv_cmd::read_mapping(&path, kind)?;
            let ins = inputs_for(&mapping, Some(&path), &inputs)?;
            let format = match format.as_str() {
                "nt" => oxrdfio::RdfFormat::NTriples,
                "ttl" => oxrdfio::RdfFormat::Turtle,
                _ => oxrdfio::RdfFormat::NQuads,
            };
            let staged = match &output {
                Some(p) => Some(crate::tools::out_file::OutFile::create(
                    p,
                    [path.as_path()],
                )?),
                None => None,
            };
            let mut out: Box<dyn Write> = match &staged {
                Some(o) => Box::new(std::io::BufWriter::new(o.file()?)),
                None => Box::new(std::io::BufWriter::new(std::io::stdout().lock())),
            };
            let stats = mapping::write(&mapping, &ins, &run_options(&inputs), &mut out, format)?;
            out.flush()?;
            drop(out);
            if let Some(o) = staged {
                o.commit()?;
            }
            for w in &stats.warnings {
                eprintln!("{m}: warning: {w}");
            }
            eprintln!("{m}: {} rows, {} quads", stats.rows, stats.quads);
            Ok(())
        }
        MapCmd::Check {
            mapping: m,
            inputs,
            functions,
        } => {
            let mut out = std::io::stdout().lock();
            if functions {
                for f in sparkles::mapping::functions::table() {
                    writeln!(out, "<{}>  {}", f.iri, f.description)?;
                    for (i, p) in f.params.iter().enumerate() {
                        let req = if i < f.required {
                            "required"
                        } else {
                            "optional"
                        };
                        writeln!(out, "    <{p}> ({req})")?;
                    }
                }
                if m.is_none() {
                    return Ok(());
                }
            }
            let Some(path) = m else {
                bail!("no mapping given");
            };
            let kind = engine_kind(&path, inputs.mapping_kind.as_deref())?;
            let mapping = csv_cmd::read_mapping(&path, kind)?;
            let ins = inputs_for(&mapping, Some(&path), &inputs)?;
            check(&mapping, &ins, &mut out)
        }
        MapCmd::Convert {
            mapping: path,
            to,
            mapping_kind,
        } => {
            let kind = engine_kind(&path, mapping_kind.as_deref())?;
            let mapping = csv_cmd::read_mapping(&path, kind)?;
            print!(
                "{}",
                sparkles::mapping::rdf::to_turtle(&mapping, to == "r2rml")?
            );
            Ok(())
        }
        MapCmd::Draft {
            connection,
            tables,
            base,
        } => draft(&connection, &tables, &base),
        MapCmd::Put {
            name,
            file,
            target,
            mapping_kind,
            description,
            message,
        } => {
            let kind = kind_of(&file, mapping_kind.as_deref())?;
            let text = std::fs::read_to_string(&file)
                .with_context(|| format!("reading {}", file.display()))?;
            if let Some(loc) = &target.loc {
                let ds = sparkles::Dataset::open_with(loc, opts)?;
                let saved = ds.mappings().put(
                    &name,
                    Definition {
                        kind: kind.name().to_string(),
                        text,
                        description,
                    },
                    Change {
                        message,
                        dataset_commit: Some(ds.head_commit().seq),
                        ..Default::default()
                    },
                )?;
                for w in &saved.summary.warnings {
                    eprintln!("{name}: warning: {w}");
                }
                println!(
                    "{name}: version {}{}",
                    saved.stored.version.version,
                    if saved.changed { "" } else { " (unchanged)" }
                );
                return Ok(());
            }
            remote(&target, |r, ds| {
                remote_put(
                    r,
                    ds,
                    &name,
                    kind,
                    &text,
                    description.as_deref(),
                    message.as_deref(),
                )
            })
        }
        MapCmd::Get {
            name,
            target,
            version,
        } => {
            if let Some(loc) = &target.loc {
                let c = open_catalog(loc)?;
                let s = c
                    .get(&name, version)
                    .with_context(|| format!("no stored mapping '{name}'"))?;
                print!("{}", s.definition.text);
                return Ok(());
            }
            remote(&target, |r, ds| remote_get(r, ds, &name, version))
        }
        MapCmd::List { target } => {
            if let Some(loc) = &target.loc {
                let c = open_catalog(loc)?;
                for (n, s, last) in c.list() {
                    let run = match last {
                        Some(l) => format!(
                            "  last run {} ({} quads{})",
                            l.time,
                            l.quads,
                            l.commit
                                .map(|c| format!(", commit {c}"))
                                .unwrap_or_default()
                        ),
                        None => String::new(),
                    };
                    println!(
                        "{n}  v{}  {}  {}{run}",
                        s.version.version,
                        s.definition.kind,
                        s.definition.description.as_deref().unwrap_or("")
                    );
                }
                return Ok(());
            }
            remote(&target, remote_list)
        }
        MapCmd::Delete { name, target } => {
            if let Some(loc) = &target.loc {
                let ds = sparkles::Dataset::open_with(loc, opts)?;
                if !ds.mappings().delete(&name, None)? {
                    bail!("no stored mapping '{name}'");
                }
                println!("deleted {name}");
                return Ok(());
            }
            remote(&target, |r, ds| remote_delete(r, ds, &name))
        }
    }
}

fn open_catalog(loc: &Path) -> Result<Catalog> {
    if !loc.is_dir() {
        bail!("{} is not a database directory", loc.display());
    }
    Ok(Catalog::open(Some(loc))?)
}

/// `sparkles map check`: the triples maps, sources and joins, and the referenced
/// columns of the CSV files that can be read.
fn check(m: &mapping::Mapping, ins: &mapping::Inputs, out: &mut impl Write) -> Result<()> {
    use sparkles::mapping::model::SourceKind;
    writeln!(
        out,
        "{} mapping with {} triples maps",
        m.vocabulary.name(),
        m.maps.len()
    )?;
    if let Some(b) = &m.base {
        writeln!(out, "base IRI {b}")?;
    }
    writeln!(out, "sources:")?;
    for (i, s) in m.sources.iter().enumerate() {
        let maps: Vec<&str> = m
            .maps
            .iter()
            .filter(|t| t.source == i)
            .map(|t| t.id.as_str())
            .collect();
        let iter = s
            .iterator
            .as_deref()
            .map(|it| format!(" iterating {it}"))
            .unwrap_or_default();
        writeln!(out, "  {}{iter}: {}", s.kind.describe(), maps.join(", "))?;
        // the columns of a CSV file that the maps reference
        if let SourceKind::Csv {
            file, tsv, dialect, ..
        } = &s.kind
            && let Some(p) = ins.optional_file(file)
        {
            let mut refs = Vec::new();
            for t in m.maps.iter().filter(|t| t.source == i) {
                t.subject.references(&mut refs);
                for g in &t.graphs {
                    g.references(&mut refs);
                }
                for po in &t.predicate_objects {
                    for x in po.predicates.iter().chain(&po.graphs) {
                        x.references(&mut refs);
                    }
                    for o in &po.objects {
                        match o {
                            sparkles::mapping::model::ObjectMap::Term(tm) => {
                                tm.references(&mut refs)
                            }
                            sparkles::mapping::model::ObjectMap::Join { conditions, .. } => {
                                refs.extend(conditions.iter().map(|(c, _)| c.clone()))
                            }
                        }
                    }
                }
            }
            let header = sparkles::tabular::header(&p, *tsv, dialect, file)?;
            let missing: Vec<&String> = refs.iter().filter(|r| !header.contains(r)).collect();
            if missing.is_empty() {
                writeln!(out, "    every referenced column is in the header")?;
            } else {
                writeln!(
                    out,
                    "    missing columns: {}",
                    missing
                        .iter()
                        .map(|r| format!("{r:?}"))
                        .collect::<Vec<_>>()
                        .join(", ")
                )?;
            }
        }
    }
    let joins = sparkles::mapping::exec::describe_joins(m);
    if !joins.is_empty() {
        writeln!(out, "joins:")?;
        for j in joins {
            writeln!(out, "  {j}")?;
        }
    }
    for w in &m.warnings {
        writeln!(out, "warning: {w}")?;
    }
    Ok(())
}

fn draft(connection: &str, tables: &[String], base: &str) -> Result<()> {
    let c = csv_cmd::connections(std::slice::from_ref(&connection.to_string()))?;
    #[cfg(feature = "mapping-sqlite")]
    {
        print!(
            "{}",
            sparkles::mapping::draft::r2rml(&c[0].connection, tables, base)?
        );
        Ok(())
    }
    #[cfg(not(feature = "mapping-sqlite"))]
    {
        let _ = (c, tables, base);
        bail!("this build has no SQLite connections (cargo feature mapping-sqlite)")
    }
}

/// A stored mapping's run with a database directory.
fn run_stored(
    name: &str,
    inputs: &RunInputs,
    target: &Target,
    mode: Option<&str>,
    dry_run: bool,
    message: Option<&str>,
    opts: StoreOptions,
) -> Result<()> {
    let Some(loc) = &target.loc else {
        return remote(target, |r, ds| {
            remote_run(r, ds, name, inputs, mode, dry_run, message)
        });
    };
    let ds = sparkles::Dataset::open_with(loc, opts)?;
    let stored = ds
        .mappings()
        .get(name, None)
        .with_context(|| format!("no stored mapping '{name}'"))?;
    let kind = stored.definition.kind()?;
    if !matches!(kind, MappingKind::Rml | MappingKind::Yarrrml) {
        bail!(
            "mapping '{name}' is {} and maps CSV files: load the tables with sparkles load --mapping",
            kind.name()
        );
    }
    let m = mapping::parse(&stored.definition.text, kind, None)?;
    let mut ins = inputs_for(&m, None, inputs)?;
    ins.dir = std::env::current_dir().ok();
    let replace = mode == Some("replace");
    if replace && m.constant_graphs().is_none() {
        bail!(
            "mode replace needs constant graph maps, since the graphs a template graph map clears are not known before the run"
        );
    }
    if dry_run {
        let (_tmp, stats) = mapping::to_nquads_file(&m, &ins, &run_options(inputs), None)?;
        println!(
            "{name}: {} rows, {} quads; nothing committed (dry run)",
            stats.rows, stats.quads
        );
        return Ok(());
    }
    let wo = sparkles::guard::WriteOptions {
        message: message.map(Into::into),
        ..Default::default()
    };
    let run = ds
        .mappings()
        .run(name, None, &ins, &run_options(inputs), replace, &wo)?;
    for w in &run.stats.warnings {
        eprintln!("{name}: warning: {w}");
    }
    println!(
        "{name}: {} rows, {} quads; {}",
        run.stats.rows,
        run.stats.quads,
        if run.receipt.committed {
            format!("commit {}", run.receipt.commit.seq)
        } else {
            "nothing changed".into()
        }
    );
    Ok(())
}

// ------------------------------------------------------------------ the server ----

#[cfg(feature = "auth")]
fn remote(
    target: &Target,
    f: impl FnOnce(&crate::remote::Remote, &str) -> Result<()>,
) -> Result<()> {
    let ds = target
        .dataset
        .as_deref()
        .context("--server needs --dataset")?;
    let r = crate::remote::Remote::open(target.server.as_deref(), target.insecure_http)?;
    f(&r, ds)
}

#[cfg(not(feature = "auth"))]
fn remote(target: &Target, _f: impl FnOnce(&(), &str) -> Result<()>) -> Result<()> {
    let _ = target;
    bail!("give --loc: this build has no remote client (cargo feature auth)")
}

#[cfg(feature = "auth")]
fn enc(s: &str) -> String {
    percent_encoding::utf8_percent_encode(s, percent_encoding::NON_ALPHANUMERIC).to_string()
}

#[cfg(feature = "auth")]
fn remote_put(
    r: &crate::remote::Remote,
    ds: &str,
    name: &str,
    kind: MappingKind,
    text: &str,
    description: Option<&str>,
    message: Option<&str>,
) -> Result<()> {
    use crate::remote::JsonBody;
    let mut path = format!("/$/mappings/{}/{}", enc(ds), enc(name));
    if let Some(d) = description {
        path.push_str(&format!("?description={}", enc(d)));
    }
    let mut req = r
        .req(reqwest::Method::PUT, &path)
        .header("content-type", kind.media_type())
        .header("accept", "application/json");
    if let Some(m) = message {
        req = req.header(
            "sparkles-commit-message",
            crate::remote::client::message_header(m),
        );
    }
    let j = r
        .check(req.body(text.to_string()).send(), Some(ds))?
        .json_value()?;
    println!(
        "{name}: version {}",
        j["version"]["version"].as_u64().unwrap_or(0)
    );
    Ok(())
}

#[cfg(not(feature = "auth"))]
fn remote_put(
    _: &(),
    _: &str,
    _: &str,
    _: MappingKind,
    _: &str,
    _: Option<&str>,
    _: Option<&str>,
) -> Result<()> {
    unreachable!("remote() refuses")
}

#[cfg(feature = "auth")]
fn remote_get(r: &crate::remote::Remote, ds: &str, name: &str, version: Option<u64>) -> Result<()> {
    let mut path = format!("/$/mappings/{}/{}", enc(ds), enc(name));
    if let Some(v) = version {
        path.push_str(&format!("?version={v}"));
    }
    let j = r.get_json(&path)?;
    print!("{}", j["text"].as_str().unwrap_or_default());
    Ok(())
}

#[cfg(not(feature = "auth"))]
fn remote_get(_: &(), _: &str, _: &str, _: Option<u64>) -> Result<()> {
    unreachable!("remote() refuses")
}

#[cfg(feature = "auth")]
fn remote_list(r: &crate::remote::Remote, ds: &str) -> Result<()> {
    let j = r.get_json(&format!("/$/mappings/{}", enc(ds)))?;
    for m in j["mappings"].as_array().cloned().unwrap_or_default() {
        println!(
            "{}  v{}  {}  {}",
            m["name"].as_str().unwrap_or_default(),
            m["version"]["version"].as_u64().unwrap_or(0),
            m["kind"].as_str().unwrap_or_default(),
            m["description"].as_str().unwrap_or_default()
        );
    }
    Ok(())
}

#[cfg(not(feature = "auth"))]
fn remote_list(_: &(), _: &str) -> Result<()> {
    unreachable!("remote() refuses")
}

#[cfg(feature = "auth")]
fn remote_delete(r: &crate::remote::Remote, ds: &str, name: &str) -> Result<()> {
    r.check(
        r.req(
            reqwest::Method::DELETE,
            &format!("/$/mappings/{}/{}", enc(ds), enc(name)),
        )
        .send(),
        Some(ds),
    )?;
    println!("deleted {name}");
    Ok(())
}

#[cfg(not(feature = "auth"))]
fn remote_delete(_: &(), _: &str, _: &str) -> Result<()> {
    unreachable!("remote() refuses")
}

/// A multipart body of the files of `--source`, each as a part named `file` whose file
/// name is the source's name.
#[cfg(feature = "auth")]
fn multipart(sources: &[String]) -> Result<(String, std::fs::File)> {
    use std::io::Seek;
    let boundary = format!("sparkles-{}", uuid::Uuid::new_v4().simple());
    let mut body = tempfile::tempfile()?;
    for s in sources {
        let (n, p) = s
            .split_once('=')
            .with_context(|| format!("--source {s}: expected NAME=PATH"))?;
        write!(
            body,
            "--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"{}\"\r\nContent-Type: application/octet-stream\r\n\r\n",
            n.replace('"', "")
        )?;
        let mut f = std::fs::File::open(p).with_context(|| format!("opening {p}"))?;
        std::io::copy(&mut f, &mut body)?;
        write!(body, "\r\n")?;
    }
    write!(body, "--{boundary}--\r\n")?;
    body.rewind()?;
    Ok((boundary, body))
}

#[cfg(feature = "auth")]
fn remote_run(
    r: &crate::remote::Remote,
    ds: &str,
    name: &str,
    inputs: &RunInputs,
    mode: Option<&str>,
    dry_run: bool,
    message: Option<&str>,
) -> Result<()> {
    use crate::remote::JsonBody;
    if !inputs.connection.is_empty() {
        bail!(
            "a run on a server reads the server's connections; --connection applies to local runs"
        );
    }
    let mut q = vec![format!("mode={}", mode.unwrap_or("add"))];
    if dry_run {
        q.push("dryRun=true".into());
    }
    if let Some(b) = &inputs.base {
        q.push(format!("base={}", enc(b)));
    }
    let path = format!("/{}/mappings/{}/run?{}", enc(ds), enc(name), q.join("&"));
    let mut req = r
        .req(reqwest::Method::POST, &path)
        .header("accept", "application/json");
    if let Some(m) = message {
        req = req.header(
            "sparkles-commit-message",
            crate::remote::client::message_header(m),
        );
    }
    let req = if inputs.source.is_empty() {
        req
    } else {
        let (boundary, body) = multipart(&inputs.source)?;
        req.header(
            "content-type",
            format!("multipart/form-data; boundary={boundary}"),
        )
        .body(reqwest::blocking::Body::new(body))
    };
    let j = r.check(req.send(), Some(ds))?.json_value()?;
    println!("{}", serde_json::to_string_pretty(&j)?);
    Ok(())
}

#[cfg(not(feature = "auth"))]
fn remote_run(
    _: &(),
    _: &str,
    _: &str,
    _: &RunInputs,
    _: Option<&str>,
    _: bool,
    _: Option<&str>,
) -> Result<()> {
    unreachable!("remote() refuses")
}
