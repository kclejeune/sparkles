//! CSV and TSV files on the command line (spec C05): the mapping options of
//! `sparkles load`, which turn each table into a temporary N-Triples file before the
//! load, and `sparkles csv`, which converts tables without loading them. A `--mapping`
//! that holds R2RML, RML or YARRRML runs through the mapping engine instead (spec F19),
//! with the files on the command line as its sources.

use anyhow::{Context, Result, bail};
use clap::{Args, Subcommand};
use sparkles::mapping::{self, MappingKind};
use sparkles::tabular::{self, Mapping, Options, Stats};
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// How CSV and TSV files map to triples (`sparkles load`, `sparkles csv`).
#[derive(Args, Debug, Clone, Default)]
pub struct CsvArgs {
    /// CSV and TSV files: a CSVW metadata file (JSON) that maps them; with no file
    /// given, its tables are loaded from their `url`
    #[arg(long, value_name = "FILE")]
    pub mapping: Option<PathBuf>,
    /// CSV and TSV files: a SPARQL CONSTRUCT query run for each row, whose column
    /// variables are bound to the row's cells (Tarql style)
    #[arg(long, value_name = "FILE.rq")]
    pub template: Option<PathBuf>,
    /// CSV and TSV files: the namespace of the default mapping's subjects and
    /// predicates, and the URL of a mapped table that has none
    #[arg(long, value_name = "IRI")]
    pub base: Option<String>,
    /// CSV and TSV files: the column that names each row's subject in the default
    /// mapping (without it, a row is named by its row number in the file)
    #[arg(long, value_name = "COLUMN", conflicts_with_all = ["mapping", "template"])]
    pub key: Option<String>,
    /// The kind of the --mapping file when its content does not tell: rml (R2RML and
    /// RML), yarrrml, csvw or template
    #[arg(long, value_name = "KIND")]
    pub mapping_kind: Option<String>,
    /// R2RML, RML and YARRRML: a file that a logical source names, NAME=PATH
    /// (repeatable); files on the command line count by their file names
    #[arg(long, value_name = "NAME=PATH")]
    pub source: Vec<String>,
    /// R2RML, RML and YARRRML: a database connection, NAME=URL with sqlite:PATH
    /// (repeatable); an R2RML mapping reads the only one, or the one named default
    #[arg(long, value_name = "NAME=URL")]
    pub connection: Vec<String>,
    /// CSVW metadata: minimal (the default) or standard, which adds the table group,
    /// table and row descriptions
    #[arg(long, value_name = "MODE", value_parser = ["minimal", "standard"])]
    pub csvw_mode: Option<String>,
    /// CSVW metadata: check primary and foreign keys (keys, the default in standard
    /// mode) or not (none)
    #[arg(long, value_name = "CHECK", value_parser = ["keys", "none"])]
    pub csvw_check: Option<String>,
    /// CONSTRUCT templates: read the target database (--loc) while mapping
    #[arg(long)]
    pub lookup: bool,
    /// JSON sources of a mapping: the largest document, in MiB (default 512)
    #[arg(long, value_name = "MB")]
    pub max_json_mb: Option<u64>,
    /// The graph of a load (`sparkles load --graph`), which an R2RML, RML or YARRRML
    /// mapping writes every quad into
    #[arg(skip)]
    pub graph: Option<String>,
}

/// `NAME=VALUE`
fn pair(s: &str, what: &str) -> Result<(String, String)> {
    let (n, v) = s
        .split_once('=')
        .with_context(|| format!("{what} {s}: expected NAME=VALUE"))?;
    Ok((n.to_string(), v.to_string()))
}

/// The connections of `--connection NAME=URL`.
pub fn connections(args: &[String]) -> Result<Vec<mapping::NamedConnection>> {
    args.iter()
        .map(|c| {
            let (name, url) = pair(c, "--connection")?;
            Ok(mapping::NamedConnection {
                name,
                connection: mapping::Connection::from_url(&url)?,
                jdbc_dsn: None,
            })
        })
        .collect()
}

/// The kind of a mapping file: `--mapping-kind`, or its content.
pub fn file_kind(path: &Path, explicit: Option<&str>) -> Result<MappingKind> {
    if let Some(k) = explicit {
        return MappingKind::parse(k)
            .with_context(|| format!("--mapping-kind {k}: use rml, yarrrml, csvw or template"));
    }
    let text =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    mapping::detect(&text).with_context(|| {
        format!(
            "{}: not CSVW metadata, R2RML, RML, YARRRML or a CONSTRUCT template (give --mapping-kind)",
            path.display()
        )
    })
}

impl CsvArgs {
    fn explicit(&self) -> bool {
        self.mapping.is_some() || self.template.is_some() || self.key.is_some()
    }

    /// The mapping engine's kind of `--mapping`, when it holds R2RML, RML or YARRRML.
    pub fn engine_kind(&self) -> Result<Option<MappingKind>> {
        let Some(m) = &self.mapping else {
            return Ok(None);
        };
        Ok(match file_kind(m, self.mapping_kind.as_deref())? {
            k @ (MappingKind::Rml | MappingKind::Yarrrml) => Some(k),
            _ => None,
        })
    }

    /// Whether `--mapping` holds a CONSTRUCT template.
    fn mapping_is_template(&self) -> Result<bool> {
        match &self.mapping {
            Some(m) => Ok(file_kind(m, self.mapping_kind.as_deref())? == MappingKind::Template),
            None => Ok(false),
        }
    }

    fn standard(&self) -> bool {
        self.csvw_mode.as_deref() == Some("standard")
    }

    fn keys(&self) -> bool {
        match self.csvw_check.as_deref() {
            Some(c) => c == "keys",
            None => self.standard(),
        }
    }

    /// The run options of the mapping engine.
    pub fn run_options(&self) -> mapping::RunOptions {
        mapping::RunOptions {
            only_graph: self
                .graph
                .as_deref()
                .and_then(|g| oxrdf::NamedNode::new(g).ok()),
            base: self.base.clone(),
            max_json_bytes: self
                .max_json_mb
                .map_or(mapping::DEFAULT_MAX_JSON_BYTES, |m| m << 20),
            ..Default::default()
        }
    }

    /// The mapping for `file`: the explicit one, a `file-metadata.json` next to it, or
    /// the default mapping.
    fn mapping_for(
        &self,
        file: Option<&Path>,
        cache: &mut Cache,
    ) -> Result<(Mapping, Option<String>)> {
        // a CONSTRUCT template given as --mapping is a template
        if self.mapping_is_template()? {
            return Ok((
                Mapping::Template {
                    template: cache.template(self.mapping.as_ref().expect("checked"))?,
                    metadata: None,
                },
                None,
            ));
        }
        let metadata = match &self.mapping {
            Some(m) => Some(cache.metadata(m)?),
            None => None,
        };
        if let Some(t) = &self.template {
            return Ok((
                Mapping::Template {
                    template: cache.template(t)?,
                    metadata,
                },
                None,
            ));
        }
        if let Some(m) = metadata {
            return Ok((Mapping::Csvw(m), None));
        }
        if !self.explicit()
            && let Some(f) = file
        {
            let mut found = f.as_os_str().to_owned();
            found.push("-metadata.json");
            let found = PathBuf::from(found);
            if found.is_file() {
                let note = format!("using the CSVW metadata in {}", found.display());
                return Ok((Mapping::Csvw(cache.metadata(&found)?), Some(note)));
            }
        }
        Ok((
            Mapping::Default {
                key: self.key.clone(),
            },
            None,
        ))
    }
}

/// Mapping and template files, read once for all tables of a command.
#[derive(Default)]
struct Cache {
    metadata: Vec<(PathBuf, Arc<tabular::Metadata>)>,
    template: Option<(PathBuf, Arc<tabular::Template>)>,
}

impl Cache {
    fn metadata(&mut self, p: &Path) -> Result<Arc<tabular::Metadata>> {
        if let Some((_, m)) = self.metadata.iter().find(|(q, _)| q == p) {
            return Ok(m.clone());
        }
        let m = Arc::new(tabular::read_metadata(p)?);
        self.metadata.push((p.to_path_buf(), m.clone()));
        Ok(m)
    }

    fn template(&mut self, p: &Path) -> Result<Arc<tabular::Template>> {
        if let Some((q, t)) = &self.template
            && q == p
        {
            return Ok(t.clone());
        }
        let t = Arc::new(tabular::read_template(p)?);
        self.template = Some((p.to_path_buf(), t.clone()));
        Ok(t)
    }
}

/// The tables of `sparkles convert`: the mapping options, and the mapping files they
/// name, read once.
pub struct Tables {
    args: CsvArgs,
    cache: Cache,
    part: usize,
}

impl Tables {
    pub fn new(args: CsvArgs) -> Tables {
        Tables {
            args,
            cache: Cache::default(),
            part: 0,
        }
    }

    /// The conversion options of the table at `path` (`None`: standard input), named
    /// `name` in messages and tab-separated when `tsv`, with a note when a CSVW
    /// metadata file next to it gives the mapping.
    pub fn options(
        &mut self,
        path: Option<&Path>,
        name: &str,
        tsv: bool,
    ) -> Result<(Options, Option<String>)> {
        let (mapping, note) = self.args.mapping_for(path, &mut self.cache)?;
        let mut o = match path {
            Some(p) => Options::for_file(mapping, p),
            None => Options::new(mapping, name),
        };
        o.name = name.to_string();
        o.tsv = tsv;
        o.base = self.args.base.clone();
        o.part = self.part;
        self.part += 1;
        Ok((o, note))
    }
}

/// One table to convert: the file, and the table of the mapping it is.
struct Job {
    path: PathBuf,
    mapping: Mapping,
    table: Option<usize>,
    note: Option<String>,
}

/// The tables among `files` (and, with `--mapping` and no table file, the mapping's own
/// tables), each with its mapping. RDF files are left out.
fn jobs(files: &[PathBuf], args: &CsvArgs) -> Result<Vec<Job>> {
    let mut cache = Cache::default();
    let tables: Vec<&PathBuf> = files
        .iter()
        .filter(|f| tabular::tabular_kind(f).is_some())
        .collect();
    if tables.is_empty() {
        if let Some(m) = &args.mapping
            && files.is_empty()
        {
            let meta = cache.metadata(m)?;
            let mut out = Vec::new();
            for (i, t) in meta.tables.iter().enumerate() {
                let url = t
                    .url
                    .as_deref()
                    .with_context(|| format!("{}: table {} has no url", m.display(), i + 1))?;
                let path = file_path(url).with_context(|| {
                    format!(
                        "{}: table {} is at {url}, which is not a local file",
                        m.display(),
                        i + 1
                    )
                })?;
                let (mapping, _) = args.mapping_for(None, &mut cache)?;
                out.push(Job {
                    path,
                    mapping,
                    table: Some(i),
                    note: None,
                });
            }
            return Ok(out);
        }
        if args.explicit() || args.base.is_some() {
            bail!(
                "--mapping, --template, --base and --key apply to CSV and TSV files, and none was given"
            );
        }
        return Ok(Vec::new());
    }
    tables
        .into_iter()
        .map(|f| {
            let (mapping, note) = args.mapping_for(Some(f), &mut cache)?;
            Ok(Job {
                path: f.clone(),
                mapping,
                table: None,
                note,
            })
        })
        .collect()
}

/// A `file:` URL as a path.
fn file_path(url: &str) -> Option<PathBuf> {
    let rest = url.strip_prefix("file://")?;
    let rest = rest.strip_prefix("localhost").unwrap_or(rest);
    let rest = rest.split(['?', '#']).next()?;
    Some(PathBuf::from(
        percent_encoding::percent_decode_str(rest)
            .decode_utf8()
            .ok()?
            .into_owned(),
    ))
}

fn options(job: &Job, args: &CsvArgs, part: usize) -> Options {
    let mut o = Options::for_file(job.mapping.clone(), &job.path);
    o.base = args.base.clone();
    o.table = job.table;
    o.part = part;
    o.standard = args.standard();
    o
}

/// With key checks, the jobs of a CSVW mapping in the order of their foreign keys, each
/// with the table it is, and the registry that shares the referenced values.
fn order_for_keys(
    jobs: Vec<Job>,
    args: &CsvArgs,
) -> Result<(Vec<Job>, Option<Arc<tabular::KeyRegistry>>)> {
    if !args.keys() {
        return Ok((jobs, None));
    }
    let Some(Mapping::Csvw(meta)) = jobs.first().map(|j| j.mapping.clone()) else {
        if args.csvw_check.is_some() {
            bail!("--csvw-check keys needs CSVW metadata (--mapping)");
        }
        return Ok((jobs, None));
    };
    let mut indexed = Vec::new();
    for (i, mut j) in jobs.into_iter().enumerate() {
        let t = match j.table {
            Some(t) => t,
            None => tabular::table_index(&meta, &options(&j, args, i))?,
        };
        j.table = Some(t);
        indexed.push((t, j));
    }
    let tables: Vec<usize> = indexed.iter().map(|(t, _)| *t).collect();
    let order = tabular::key_order(&meta, &tables)?;
    let mut out = Vec::new();
    for t in order {
        if let Some(pos) = indexed.iter().position(|(x, _)| *x == t) {
            out.push(indexed.remove(pos).1);
        }
    }
    Ok((out, Some(Arc::new(tabular::KeyRegistry::new(None)))))
}

/// The inputs of the mapping engine: `--source`, `--connection`, the files on the
/// command line that a logical source names, and the mapping file's directory. Returns
/// the inputs and the files that are not sources (RDF files of the same load).
fn engine_inputs(
    m: &mapping::Mapping,
    mapping_file: &Path,
    files: &[PathBuf],
    args: &CsvArgs,
) -> Result<(mapping::Inputs, Vec<PathBuf>)> {
    let mut inputs = mapping::Inputs {
        files: Vec::new(),
        dir: mapping_file.parent().map(|d| {
            if d.as_os_str().is_empty() {
                PathBuf::from(".")
            } else {
                d.to_path_buf()
            }
        }),
        connections: connections(&args.connection)?,
    };
    for s in &args.source {
        let (n, p) = pair(s, "--source")?;
        inputs.files.push((n, PathBuf::from(p)));
    }
    let last = |s: &str| s.rsplit(['/', '\\']).next().unwrap_or(s).to_string();
    let names: Vec<String> = m
        .sources
        .iter()
        .filter_map(|s| s.kind.file().map(last))
        .collect();
    let mut rest = Vec::new();
    for f in files {
        let name = f
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        if names.contains(&name) {
            inputs.files.push((name, f.clone()));
        } else {
            rest.push(f.clone());
        }
    }
    Ok((inputs, rest))
}

/// Read and parse a mapping file of the engine's kinds.
pub fn read_mapping(path: &Path, kind: MappingKind) -> Result<mapping::Mapping> {
    let text =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    mapping::parse(&text, kind, Some(&tabular::file_url(path)))
        .map_err(|e| anyhow::anyhow!("{}: {e}", path.display()))
}

fn report_mapping(name: &str, s: &mapping::Stats) {
    for w in &s.warnings {
        eprintln!("{name}: warning: {w}");
    }
    eprintln!("{name}: {} rows, {} quads", s.rows, s.quads);
}

fn report(name: &str, s: &Stats, note: Option<&str>) {
    if let Some(n) = note {
        eprintln!("{name}: {n}");
    }
    for w in &s.warnings {
        eprintln!("{name}: warning: {w}");
    }
    eprintln!("{name}: {} rows, {} triples", s.rows, s.triples);
}

/// The files of a `sparkles load`, with each table converted to a temporary N-Triples
/// file, and each file in one of Jena's syntaxes that the loader does not parse (TriX,
/// RDF Thrift, RDF Protobuf, RDF/JSON) to a temporary N-Quads file. Keep it until the
/// load is done: dropping it removes the files.
pub struct Prepared {
    pub files: Vec<PathBuf>,
    /// the name of each file in messages (a table's own name for its N-Triples file)
    pub names: Vec<String>,
    /// whether each file was converted (so `--compression` does not apply)
    pub converted: Vec<bool>,
    _temps: Vec<tempfile::TempPath>,
}

/// Convert the tables and the files in Jena's syntaxes among `files` for a load. Other
/// RDF files pass through. A template with `--lookup` reads `lookup`.
pub fn prepare(
    files: &[PathBuf],
    args: &CsvArgs,
    lookup: Option<tabular::template::Lookup>,
) -> Result<Prepared> {
    let mut out = Prepared {
        files: Vec::new(),
        names: Vec::new(),
        converted: Vec::new(),
        _temps: Vec::new(),
    };
    if let Some(kind) = args.engine_kind()? {
        let mfile = args.mapping.as_ref().expect("an engine mapping");
        if args.template.is_some() || args.key.is_some() {
            bail!("--template and --key do not apply to an R2RML, RML or YARRRML mapping");
        }
        let m = read_mapping(mfile, kind)?;
        let (inputs, rest) = engine_inputs(&m, mfile, files, args)?;
        let (path, stats) = mapping::to_nquads_file(&m, &inputs, &args.run_options(), None)?;
        let name = mfile.display().to_string();
        report_mapping(&name, &stats);
        out.files.push(path.to_path_buf());
        out.names.push(name);
        out.converted.push(true);
        out._temps.push(path);
        for f in rest {
            let p = prepare(std::slice::from_ref(&f), &CsvArgs::default(), None)?;
            out.files.extend(p.files);
            out.names.extend(p.names);
            out.converted.extend(p.converted);
            out._temps.extend(p._temps);
        }
        return Ok(out);
    }
    if args.lookup && lookup.is_none() {
        bail!("--lookup reads the target database, so it needs --loc");
    }
    let (jobs, keys) = order_for_keys(jobs(files, args)?, args)?;
    let mut converted = std::collections::HashMap::new();
    for (i, job) in jobs.iter().enumerate() {
        let name = job.path.display().to_string();
        let input = tabular::open(&job.path, None)?;
        let mut o = options(job, args, i);
        o.keys = keys.clone();
        o.lookup = lookup.clone();
        let (path, stats) = tabular::to_ntriples_file(input, &o, None)?;
        report(&name, &stats, job.note.as_deref());
        if let Some(c) = stats.lookup_commit {
            eprintln!("{name}: the template read commit {c}");
        }
        converted.insert(job.path.clone(), path.to_path_buf());
        if !files.contains(&job.path) {
            out.files.push(path.to_path_buf());
            out.names.push(name);
            out.converted.push(true);
        }
        out._temps.push(path);
    }
    for f in files {
        if let Some(p) = converted.get(f) {
            out.files.push(p.clone());
            out.converted.push(true);
        } else if let Some(j) = crate::http::jena_formats::JenaFormat::from_path(f) {
            let path = jena_to_nquads(f, j)?;
            out.files.push(path.to_path_buf());
            out.converted.push(true);
            out._temps.push(path);
        } else {
            out.files.push(f.clone());
            out.converted.push(false);
        }
        out.names.push(f.display().to_string());
    }
    Ok(out)
}

/// A file in one of Jena's syntaxes as a temporary N-Quads file. Relative IRIs in TriX
/// resolve against the file's `file://` IRI, as in the other syntaxes.
fn jena_to_nquads(
    path: &Path,
    fmt: crate::http::jena_formats::JenaFormat,
) -> Result<tempfile::TempPath> {
    use std::io::Write;
    let input = tabular::open(path, None)?;
    let tmp = tempfile::Builder::new()
        .prefix("sparkles-load-")
        .suffix(".nq")
        .tempfile()?;
    let abs = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let base = format!("file://{}", abs.display());
    let mut w = std::io::BufWriter::new(tmp.as_file());
    crate::http::jena_formats::transcode_with_base(fmt, input, true, &mut w, Some(&base))
        .map_err(|e| anyhow::anyhow!("{}: {e}", path.display()))?;
    w.flush()?;
    drop(w);
    Ok(tmp.into_temp_path())
}

// ----------------------------------------------------------------- sparkles csv ----

#[derive(Args, Debug)]
pub struct CsvCmdArgs {
    #[command(subcommand)]
    pub cmd: CsvCmd,
}

// one command runs per process, so the size of a variant does not matter
#[allow(clippy::large_enum_variant)]
#[derive(Subcommand, Debug)]
pub enum CsvCmd {
    /// Convert CSV and TSV files to RDF without loading them
    Convert {
        /// CSV and TSV files (compressed or not); none with --mapping converts the
        /// mapping's tables
        files: Vec<PathBuf>,
        #[command(flatten)]
        csv: CsvArgs,
        /// Output syntax: nt, nq or ttl
        #[arg(long, default_value = "nt", value_parser = ["nt", "nq", "ttl"])]
        format: String,
        /// The graph of the triples in N-Quads output
        #[arg(long, value_name = "IRI")]
        graph: Option<String>,
        /// Write here instead of standard output, through a temporary file that
        /// replaces FILE only when the conversion succeeds (an input is refused as FILE)
        #[arg(long, short = 'o', value_name = "FILE")]
        output: Option<PathBuf>,
    },
    /// Print the CSVW metadata of the default mapping of a table, to start a mapping
    /// file from
    Mapping {
        file: PathBuf,
        /// The namespace of subjects and predicates
        #[arg(long, value_name = "IRI")]
        base: Option<String>,
        /// The column that names each row's subject
        #[arg(long, value_name = "COLUMN")]
        key: Option<String>,
    },
}

pub fn run(args: CsvCmdArgs) -> Result<()> {
    match args.cmd {
        CsvCmd::Convert {
            files,
            csv,
            format,
            graph,
            output,
        } => {
            if let Some(f) = files.iter().find(|f| tabular::tabular_kind(f).is_none()) {
                bail!("{}: not a .csv or .tsv file", f.display());
            }
            if files.is_empty() && csv.mapping.is_none() {
                bail!("no files given");
            }
            let format = match format.as_str() {
                "nq" => oxrdfio::RdfFormat::NQuads,
                "ttl" => oxrdfio::RdfFormat::Turtle,
                _ => oxrdfio::RdfFormat::NTriples,
            };
            let graph = graph.map(oxrdf::NamedNode::new).transpose()?;
            if graph.is_some() && format != oxrdfio::RdfFormat::NQuads {
                bail!("--graph needs --format nq");
            }
            if csv.engine_kind()?.is_some() {
                bail!("an R2RML, RML or YARRRML mapping runs with `sparkles map run`");
            }
            if csv.lookup {
                bail!("--lookup reads a database; use it with sparkles load --loc");
            }
            let (jobs, keys) = order_for_keys(jobs(&files, &csv)?, &csv)?;
            // written through a temporary file that replaces the output at the end
            let staged = match &output {
                Some(p) => Some(crate::tools::out_file::OutFile::create(
                    p,
                    jobs.iter()
                        .map(|j| j.path.as_path())
                        .chain(csv.mapping.as_deref())
                        .chain(csv.template.as_deref()),
                )?),
                None => None,
            };
            let mut out: Box<dyn std::io::Write> = match &staged {
                Some(o) => Box::new(std::io::BufWriter::new(o.file()?)),
                None => Box::new(std::io::BufWriter::new(std::io::stdout().lock())),
            };
            for (i, job) in jobs.iter().enumerate() {
                let input = tabular::open(&job.path, None)?;
                let mut o = options(job, &csv, i);
                o.keys = keys.clone();
                let stats = tabular::write(input, &o, &mut out, format, graph.as_ref())?;
                report(&job.path.display().to_string(), &stats, job.note.as_deref());
            }
            out.flush()?;
            drop(out);
            if let Some(o) = staged {
                o.commit()?;
            }
            Ok(())
        }
        CsvCmd::Mapping { file, base, key } => {
            if tabular::tabular_kind(&file).is_none() {
                bail!("{}: not a .csv or .tsv file", file.display());
            }
            let mut o = Options::for_file(Mapping::Default { key: key.clone() }, &file);
            o.base = base;
            let m = tabular::default_metadata(tabular::open(&file, None)?, &o, key.as_deref())?;
            println!("{}", serde_json::to_string_pretty(&m)?);
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_urls() {
        assert_eq!(
            file_path("file:///a/b%20c.csv"),
            Some(PathBuf::from("/a/b c.csv"))
        );
        assert_eq!(
            file_path("file://localhost/a.csv"),
            Some(PathBuf::from("/a.csv"))
        );
        assert_eq!(file_path("http://e/a.csv"), None);
    }

    #[test]
    fn prepare_converts_tables_and_passes_rdf_through() {
        let dir = tempfile::tempdir().unwrap();
        let csv = dir.path().join("people.csv");
        std::fs::write(&csv, "id,name\n7,Ann\n").unwrap();
        let ttl = dir.path().join("more.ttl");
        std::fs::write(&ttl, "<http://e/s> <http://e/p> 1 .\n").unwrap();
        let args = CsvArgs {
            base: Some("http://e/".into()),
            key: Some("id".into()),
            ..Default::default()
        };
        let p = prepare(&[csv.clone(), ttl.clone()], &args, None).unwrap();
        assert_eq!(p.files.len(), 2);
        assert_eq!(p.files[1], ttl);
        let nt = std::fs::read_to_string(&p.files[0]).unwrap();
        assert!(
            nt.contains("<http://e/7> <http://e/name> \"Ann\" ."),
            "{nt}"
        );
        assert_eq!(p.names[0], csv.display().to_string());
        // a metadata file next to the table is used when no mapping is given
        std::fs::write(
            dir.path().join("people.csv-metadata.json"),
            r#"{"@context": "http://www.w3.org/ns/csvw", "tableSchema": {
                "aboutUrl": "http://e/p/{id}",
                "columns": [{"name": "id", "datatype": "integer", "propertyUrl": "http://e/id"},
                            {"name": "name", "propertyUrl": "http://e/name"}]}}"#,
        )
        .unwrap();
        let p = prepare(std::slice::from_ref(&csv), &CsvArgs::default(), None).unwrap();
        let nt = std::fs::read_to_string(&p.files[0]).unwrap();
        assert!(
            nt.contains(
                "<http://e/p/7> <http://e/id> \"7\"^^<http://www.w3.org/2001/XMLSchema#integer> ."
            ),
            "{nt}"
        );
        // the mapping's own tables, from their url
        let p = prepare(
            &[],
            &CsvArgs {
                mapping: Some(dir.path().join("people.csv-metadata.json")),
                ..Default::default()
            },
            None,
        );
        assert!(p.is_err(), "the table has no url");
        std::fs::write(
            dir.path().join("m.json"),
            r#"{"@context": "http://www.w3.org/ns/csvw", "url": "people.csv",
                "tableSchema": {"aboutUrl": "http://e/q/{id}"}}"#,
        )
        .unwrap();
        let p = prepare(
            &[],
            &CsvArgs {
                mapping: Some(dir.path().join("m.json")),
                ..Default::default()
            },
            None,
        )
        .unwrap();
        let nt = std::fs::read_to_string(&p.files[0]).unwrap();
        assert!(nt.contains("<http://e/q/7>"), "{nt}");
        // options without a table
        assert!(prepare(&[ttl], &args, None).is_err());
    }
}
