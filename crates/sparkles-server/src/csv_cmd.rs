//! CSV and TSV files on the command line (spec C05): the mapping options of
//! `sparkles load`, which turn each table into a temporary N-Triples file before the
//! load, and `sparkles csv`, which converts tables without loading them.

use anyhow::{Context, Result, bail};
use clap::{Args, Subcommand};
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
}

impl CsvArgs {
    fn explicit(&self) -> bool {
        self.mapping.is_some() || self.template.is_some() || self.key.is_some()
    }

    /// The mapping for `file`: the explicit one, a `file-metadata.json` next to it, or
    /// the default mapping.
    fn mapping_for(
        &self,
        file: Option<&Path>,
        cache: &mut Cache,
    ) -> Result<(Mapping, Option<String>)> {
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
    o
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
/// RDF files pass through.
pub fn prepare(files: &[PathBuf], args: &CsvArgs) -> Result<Prepared> {
    let jobs = jobs(files, args)?;
    let mut out = Prepared {
        files: Vec::new(),
        names: Vec::new(),
        converted: Vec::new(),
        _temps: Vec::new(),
    };
    let mut converted = std::collections::HashMap::new();
    for (i, job) in jobs.iter().enumerate() {
        let name = job.path.display().to_string();
        let input = tabular::open(&job.path, None)?;
        let (path, stats) = tabular::to_ntriples_file(input, &options(job, args, i), None)?;
        report(&name, &stats, job.note.as_deref());
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
        /// Write here instead of standard output
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
            let mut out: Box<dyn std::io::Write> = match &output {
                Some(p) => Box::new(std::io::BufWriter::new(
                    std::fs::File::create(p)
                        .with_context(|| format!("creating {}", p.display()))?,
                )),
                None => Box::new(std::io::BufWriter::new(std::io::stdout().lock())),
            };
            for (i, job) in jobs(&files, &csv)?.iter().enumerate() {
                let input = tabular::open(&job.path, None)?;
                let stats = tabular::write(
                    input,
                    &options(job, &csv, i),
                    &mut out,
                    format,
                    graph.as_ref(),
                )?;
                report(&job.path.display().to_string(), &stats, job.note.as_deref());
            }
            out.flush()?;
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
        let p = prepare(&[csv.clone(), ttl.clone()], &args).unwrap();
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
        let p = prepare(std::slice::from_ref(&csv), &CsvArgs::default()).unwrap();
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
        )
        .unwrap();
        let nt = std::fs::read_to_string(&p.files[0]).unwrap();
        assert!(nt.contains("<http://e/q/7>"), "{nt}");
        // options without a table
        assert!(prepare(&[ttl], &args).is_err());
    }
}
