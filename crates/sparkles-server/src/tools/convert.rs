//! `sparkles convert` (alias `riot`): parse, validate, count and convert RDF files,
//! streaming (spec G05 §3.1). Also the term checks of `load --check`.
//!
//! Jena's syntaxes that oxrdfio does not read (TriX, RDF Thrift, RDF Protobuf and
//! RDF/JSON) are read into N-Quads on a second thread as the input streams in, and
//! written by [`crate::http::jena_formats::RdfWriter`]. CSV and TSV tables go through
//! the mapping of `sparkles load` ([`crate::csv_cmd::Tables`]).
//!
//! Inputs are files, directories (with `--recursive`) or standard input. Their syntax is
//! `--syntax`, else the file extension, else what [`super::sniff`] makes of the first
//! bytes. The output is one stream (standard output or `--output-file`), or with
//! `--out-dir` one file per input, converted in parallel.

use super::sniff::{self, Kind, Sniffed};
use super::terms::TermChecker;
use crate::csv_cmd::{CsvArgs, Tables};
use crate::http::jena_formats::{JenaFormat, RdfWriter};
use anyhow::{Context, Result, bail};
use oxrdf::{GraphName, Quad};
use oxrdfio::{RdfFormat, RdfParser, RdfSerializer};
use parking_lot::Mutex;
use sparkles::codec::{Codec, FinishWrite};
use sparkles::io::{QuadSink, Source, SourceData};
use sparkles::tabular::TabularKind;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Instant;

/// Warnings kept for printing per input.
const WARNINGS_SHOWN: usize = 100;
/// Syntax errors reported per input when a file is parsed again for them.
const ERRORS_SHOWN: usize = 20;

#[derive(clap::Args)]
pub struct ConvertArgs {
    /// Input files and directories (`-` or none: standard input)
    files: Vec<PathBuf>,
    /// Input syntax for every input: Turtle, N-Triples, N-Quads, TriG, RDF/XML, JSON-LD,
    /// N3, TriX, RDF Thrift, RDF Protobuf, RDF/JSON, CSV, TSV, short names (ttl, nt, nq,
    /// trix, rt, rpb, rj, …) or a media type (default: from the file extension, else
    /// from the content; N-Quads for standard input that is neither)
    #[arg(long, value_name = "LANG")]
    syntax: Option<String>,
    /// Output syntax (default: from --output-file's extension, else N-Quads, which is
    /// N-Triples for the default graph)
    #[arg(long, visible_aliases = ["out", "format"], value_name = "LANG")]
    output: Option<String>,
    /// Write the output to this file instead of standard output (its extension picks the
    /// syntax and the compression)
    #[arg(long, short = 'o', value_name = "FILE", conflicts_with_all = ["count", "sink", "validate", "out_dir"])]
    output_file: Option<PathBuf>,
    /// Write one file per input into this directory, at the input's path relative to
    /// the directory it was found in, with the output syntax's extension
    #[arg(long, value_name = "DIR", conflicts_with_all = ["count", "sink", "validate"])]
    out_dir: Option<PathBuf>,
    /// Replace files that already exist in --out-dir
    #[arg(long, requires = "out_dir")]
    overwrite: bool,
    /// Files converted at once with --out-dir (default: the number of CPUs, at most 8)
    #[arg(long, short = 'j', value_name = "N", requires = "out_dir")]
    jobs: Option<usize>,
    /// Read the files of directories, and of their subdirectories
    #[arg(long, short = 'r')]
    recursive: bool,
    /// In directories, only the files whose name (or, for a pattern with `/`, whose
    /// path below the directory) matches this glob (repeatable; `*`, `**` and `?`)
    #[arg(long, value_name = "GLOB")]
    include: Vec<String>,
    /// In directories, leave out the files whose name or path matches this glob
    /// (repeatable)
    #[arg(long, value_name = "GLOB")]
    exclude: Vec<String>,
    /// Count the triples (or quads) of each input instead of writing them
    #[arg(long, conflicts_with_all = ["output", "sink", "validate"])]
    count: bool,
    /// Parse and write nothing
    #[arg(long, visible_alias = "null", conflicts_with = "output")]
    sink: bool,
    /// --sink --check --strict
    #[arg(long, conflicts_with = "output")]
    validate: bool,
    /// Warn about suspicious IRIs and language tags (scheme rules, case, extlang, …)
    #[arg(long)]
    check: bool,
    /// Treat warnings as errors (exit status 1)
    #[arg(long)]
    strict: bool,
    /// Skip the parsers' validation of IRIs and language tags (syntax errors still fail)
    #[arg(long, conflicts_with = "validate")]
    lenient: bool,
    /// Base IRI for relative IRIs (default: the file's own `file://` IRI); for CSV and
    /// TSV files, the namespace of the default mapping's subjects and predicates
    #[arg(long, value_name = "IRI")]
    base: Option<String>,
    /// CSV and TSV files: a CSVW metadata file (JSON) that maps them
    #[arg(long, value_name = "FILE")]
    mapping: Option<PathBuf>,
    /// CSV and TSV files: a SPARQL CONSTRUCT query run for each row (Tarql style)
    #[arg(long, value_name = "FILE.rq")]
    template: Option<PathBuf>,
    /// CSV and TSV files: the column that names each row's subject in the default
    /// mapping
    #[arg(long, value_name = "COLUMN", conflicts_with_all = ["mapping", "template"])]
    key: Option<String>,
    /// Write quads of named graphs into the default graph
    #[arg(long, visible_alias = "union")]
    merge: bool,
    /// Compression of the inputs: auto (magic bytes, then the extension), none, gzip,
    /// zstd, brotli or lz4
    #[arg(long, default_value = "auto", value_name = "CODEC")]
    compression: String,
    /// Compress the output: gzip (the default when no codec is named), zstd, brotli or
    /// lz4 (default: from --output-file's extension)
    #[arg(long, num_args = 0..=1, default_missing_value = "gzip", value_name = "CODEC")]
    compress: Option<String>,
    /// Print the time and rate of each input on standard error
    #[arg(long)]
    time: bool,
}

/// An RDF syntax by Jena's or Sparkles' name, short name, file extension or media type.
pub fn rdf_syntax(name: &str) -> Option<RdfFormat> {
    let jsonld = RdfFormat::JsonLd {
        profile: oxrdfio::JsonLdProfileSet::empty(),
    };
    Some(match name.to_ascii_lowercase().as_str() {
        "turtle" | "ttl" => RdfFormat::Turtle,
        "n-triples" | "ntriples" | "n-triple" | "nt" => RdfFormat::NTriples,
        "n-quads" | "nquads" | "n-quad" | "nq" => RdfFormat::NQuads,
        "trig" => RdfFormat::TriG,
        "rdf/xml" | "rdfxml" | "rdf" | "xml" | "owl" => RdfFormat::RdfXml,
        "json-ld" | "jsonld" | "json" => jsonld,
        "n3" => RdfFormat::N3,
        other => {
            return sparkles::io::format_for_media_type(other)
                .or_else(|| RdfFormat::from_extension(other));
        }
    })
}

fn is_quad_syntax(f: RdfFormat) -> bool {
    matches!(
        f,
        RdfFormat::NQuads | RdfFormat::TriG | RdfFormat::JsonLd { .. }
    )
}

/// A syntax `convert` reads or writes: one of oxrdfio's, or one of Jena's.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Syntax {
    Rdf(RdfFormat),
    Jena(JenaFormat),
}

impl Syntax {
    /// A syntax by name, short name, file extension or media type.
    pub fn named(name: &str) -> Option<Syntax> {
        JenaFormat::from_name(name)
            .map(Syntax::Jena)
            .or_else(|| rdf_syntax(name).map(Syntax::Rdf))
    }

    /// The syntax of a file path (past a compression extension).
    pub fn of_path(path: &Path) -> Option<Syntax> {
        JenaFormat::from_path(path)
            .map(Syntax::Jena)
            .or_else(|| sparkles::io::format_for_path(path).map(|(f, _)| Syntax::Rdf(f)))
    }

    /// Whether the syntax holds named graphs.
    pub fn quads(self) -> bool {
        match self {
            Syntax::Rdf(f) => is_quad_syntax(f),
            Syntax::Jena(j) => j.quads(),
        }
    }

    /// The media type of the syntax, as the server names it.
    #[cfg_attr(not(feature = "auth"), allow(dead_code))]
    pub fn media_type(self) -> &'static str {
        match self {
            Syntax::Rdf(f) => sparkles::sparql::results::rdf_media_type(f),
            Syntax::Jena(j) => j.media_type(),
        }
    }

    /// The file extension of the syntax, without the dot.
    pub fn file_extension(self) -> &'static str {
        match self {
            Syntax::Rdf(f) => f.file_extension(),
            Syntax::Jena(j) => j.file_extension(),
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Syntax::Rdf(f) => f.name(),
            Syntax::Jena(j) => j.name(),
        }
    }
}

/// What `--syntax` names: an RDF syntax or a table.
fn named_kind(name: &str) -> Option<Kind> {
    match name.to_ascii_lowercase().as_str() {
        "csv" | "text/csv" => Some(Kind::Table(TabularKind::Csv)),
        "tsv" | "tab" | "text/tab-separated-values" => Some(Kind::Table(TabularKind::Tsv)),
        _ => Syntax::named(name).map(Kind::Rdf),
    }
}

/// The kind of a file by its extension.
fn kind_of_path(path: &Path) -> Option<Kind> {
    sparkles::tabular::tabular_kind(path)
        .map(Kind::Table)
        .or_else(|| Syntax::of_path(path).map(Kind::Rdf))
}

/// One input: a file or standard input.
pub struct Input {
    pub name: String,
    pub path: Option<PathBuf>,
    /// The file's path below the directory it was found in (its name for a file given
    /// by itself), for `--out-dir`.
    pub rel: Option<PathBuf>,
    /// The syntax oxrdfio parses: N-Quads for an input in one of Jena's syntaxes, which
    /// [`Input::reader`] transcodes.
    pub format: RdfFormat,
    /// The input's syntax when it is one of Jena's.
    pub jena: Option<JenaFormat>,
    /// The input is a table.
    pub table: Option<TabularKind>,
    pub base: Option<String>,
    /// Standard input, decompressed, when its first bytes were read to tell its syntax.
    prefetched: Mutex<Option<Box<dyn Read + Send>>>,
}

/// How [`Input::find`] finds the inputs.
#[derive(Default)]
pub struct Find<'a> {
    pub syntax: Option<&'a str>,
    pub base: Option<&'a str>,
    pub compression: Option<Codec>,
    pub recursive: bool,
    pub include: &'a [String],
    pub exclude: &'a [String],
    /// A directory whose files are never inputs (the output directory).
    pub skip_dir: Option<PathBuf>,
}

/// The inputs, and whether any came from a directory.
pub struct Found {
    pub inputs: Vec<Input>,
    pub walked: bool,
}

impl Input {
    /// The inputs of `files` (none, or `-`: standard input).
    pub fn all(files: &[PathBuf], syntax: Option<&str>, base: Option<&str>) -> Result<Vec<Input>> {
        Ok(Input::find(
            files,
            &Find {
                syntax,
                base,
                ..Default::default()
            },
        )?
        .inputs)
    }

    /// The inputs of `files`, with the files of the directories among them.
    pub fn find(files: &[PathBuf], how: &Find) -> Result<Found> {
        let forced = match how.syntax {
            Some(s) => Some(named_kind(s).with_context(|| format!("unknown syntax '{s}'"))?),
            None => None,
        };
        if let Some(b) = how.base {
            oxiri::Iri::parse(b).map_err(|e| anyhow::anyhow!("--base {b}: {e}"))?;
        }
        if files.is_empty() {
            return Ok(Found {
                inputs: vec![Input::stdin(forced, how)?],
                walked: false,
            });
        }
        let mut inputs = Vec::new();
        let mut walked = false;
        for f in files {
            if f.as_os_str() == "-" {
                inputs.push(Input::stdin(forced, how)?);
            } else if f.is_dir() {
                if !how.recursive {
                    bail!("{} is a directory (use --recursive)", f.display());
                }
                walked = true;
                let mut found = Vec::new();
                walk(f, f, how, &mut found)?;
                for (path, rel) in found {
                    match Input::file(&path, Some(rel), forced, how) {
                        Ok(i) => inputs.push(i),
                        Err(Skip::Unknown(why)) => {
                            eprintln!("warning: skipping {}: {why}", path.display());
                        }
                        Err(Skip::Error(e)) => return Err(e),
                    }
                }
            } else if f.is_file() {
                let rel = PathBuf::from(f.file_name().unwrap_or(f.as_os_str()));
                match Input::file(f, Some(rel), forced, how) {
                    Ok(i) => inputs.push(i),
                    Err(Skip::Unknown(why)) => bail!("{}: {why} (use --syntax)", f.display()),
                    Err(Skip::Error(e)) => return Err(e),
                }
            } else {
                bail!("{}: no such file", f.display());
            }
        }
        Ok(Found { inputs, walked })
    }

    fn new(name: String, path: Option<PathBuf>, kind: Kind, base: Option<String>) -> Input {
        let (format, jena, table) = match kind {
            Kind::Rdf(Syntax::Rdf(f)) => (f, None, None),
            Kind::Rdf(Syntax::Jena(j)) => (RdfFormat::NQuads, Some(j), None),
            Kind::Table(t) => (RdfFormat::NTriples, None, Some(t)),
        };
        Input {
            name,
            path,
            rel: None,
            format,
            jena,
            table,
            base,
            prefetched: Mutex::new(None),
        }
    }

    /// Standard input: of the syntax `--syntax` names, else of the syntax its first
    /// bytes show, else N-Quads.
    fn stdin(forced: Option<Kind>, how: &Find) -> Result<Input> {
        let base = how.base.map(str::to_string);
        if let Some(k) = forced {
            return Ok(Input::new("stdin".into(), None, k, base));
        }
        let mut r = stdin_reader(how.compression)?;
        let (head, complete) = read_head(&mut r)?;
        let nquads = Kind::Rdf(Syntax::Rdf(RdfFormat::NQuads));
        let kind = match sniff::sniff(&head, complete) {
            // N-Triples is N-Quads without graphs: read as before, as N-Quads
            Sniffed::Found(Kind::Rdf(Syntax::Rdf(RdfFormat::NTriples))) => nquads,
            Sniffed::Found(k) => k,
            Sniffed::Unknown => nquads,
            Sniffed::Ambiguous(c) => bail!("stdin: {} (use --syntax)", ambiguous(&c)),
        };
        let input = Input::new("stdin".into(), None, kind, base);
        *input.prefetched.lock() = Some(Box::new(std::io::Cursor::new(head).chain(r)));
        Ok(input)
    }

    /// A file: of the syntax `--syntax` names, else its extension, else its content.
    fn file(
        f: &Path,
        rel: Option<PathBuf>,
        forced: Option<Kind>,
        how: &Find,
    ) -> std::result::Result<Input, Skip> {
        let kind = match forced.or_else(|| kind_of_path(f)) {
            Some(k) => k,
            None => {
                let mut r = file_reader(f, &f.display().to_string(), how.compression)
                    .map_err(Skip::Error)?;
                let (head, complete) = read_head(&mut r).map_err(Skip::Error)?;
                match sniff::sniff(&head, complete) {
                    Sniffed::Found(k) => k,
                    Sniffed::Unknown => {
                        return Err(Skip::Unknown("unknown RDF syntax".into()));
                    }
                    Sniffed::Ambiguous(c) => return Err(Skip::Unknown(ambiguous(&c))),
                }
            }
        };
        let abs = std::fs::canonicalize(f).unwrap_or_else(|_| f.to_path_buf());
        let base = how
            .base
            .map(str::to_string)
            .unwrap_or_else(|| format!("file://{}", abs.display()));
        let mut input = Input::new(
            f.display().to_string(),
            Some(f.to_path_buf()),
            kind,
            Some(base),
        );
        input.rel = rel;
        Ok(input)
    }

    /// Whether the input's syntax holds named graphs.
    fn quads(&self) -> bool {
        match (self.jena, self.table) {
            (_, Some(_)) => false,
            (Some(j), _) => j.quads(),
            (None, None) => is_quad_syntax(self.format),
        }
    }

    fn unit(&self) -> &'static str {
        if self.quads() { "quads" } else { "triples" }
    }

    /// A [`Source`] of a file input in one of oxrdfio's syntaxes, for the parallel
    /// parser.
    fn source(&self, compression: Option<Codec>, lenient: bool) -> Option<Source> {
        if self.jena.is_some() || self.table.is_some() {
            return None;
        }
        Some(Source {
            data: SourceData::File(self.path.clone()?),
            format: self.format,
            compression,
            max_decompressed: None,
            graph: None,
            base: self.base.clone(),
            name: self.name.clone(),
            lenient,
        })
    }

    /// The decompressed bytes of the input, as N-Quads for an input in one of Jena's
    /// syntaxes.
    pub fn reader(&self, compression: Option<Codec>) -> Result<Box<dyn Read>> {
        let raw = self.raw_reader(compression)?;
        let Some(j) = self.jena else {
            return Ok(raw);
        };
        let (pipe, w) = std::io::pipe()?;
        let base = self.base.clone();
        let worker = std::thread::spawn(move || -> std::result::Result<(), String> {
            let mut w = std::io::BufWriter::with_capacity(1 << 16, w);
            crate::http::jena_formats::transcode_with_base(j, raw, true, &mut w, base.as_deref())
                .map_err(|e| e.to_string())?;
            std::io::Write::flush(&mut w).map_err(|e| e.to_string())
        });
        Ok(Box::new(Transcoded {
            pipe,
            worker: Some(worker),
        }))
    }

    /// The decompressed bytes of the input as they are.
    fn raw_reader(&self, compression: Option<Codec>) -> Result<Box<dyn Read + Send>> {
        match &self.path {
            Some(p) => file_reader(p, &self.name, compression),
            None => match self.prefetched.lock().take() {
                Some(r) => Ok(r),
                None => stdin_reader(compression),
            },
        }
    }

    /// A parser for the input's syntax and base.
    pub fn parser(&self, lenient: bool) -> Result<RdfParser> {
        let mut p = RdfParser::from_format(self.format);
        if let Some(b) = &self.base {
            p = p.with_base_iri(b.clone())?;
        }
        if lenient {
            p = p.lenient();
        }
        Ok(p)
    }
}

/// Why a file found in a directory is not an input.
enum Skip {
    /// Its syntax is unknown: skipped with a warning.
    Unknown(String),
    Error(anyhow::Error),
}

fn ambiguous(candidates: &[Kind]) -> String {
    let names: Vec<&str> = candidates.iter().map(|k| k.name()).collect();
    format!("the content could be {}", names.join(" or "))
}

/// The files below `dir`, sorted by path, with their paths relative to `root`.
fn walk(root: &Path, dir: &Path, how: &Find, out: &mut Vec<(PathBuf, PathBuf)>) -> Result<()> {
    let mut entries: Vec<std::fs::DirEntry> = std::fs::read_dir(dir)
        .with_context(|| format!("reading {}", dir.display()))?
        .collect::<std::io::Result<_>>()
        .with_context(|| format!("reading {}", dir.display()))?;
    entries.sort_by_key(|e| e.file_name());
    for e in entries {
        let path = e.path();
        let ty = e.file_type()?;
        if ty.is_dir() {
            if how.recursive && !skipped_dir(&path, how) {
                walk(root, &path, how, out)?;
            }
            continue;
        }
        // a symbolic link to a file is read; one to a directory is not followed
        if ty.is_symlink() && !path.is_file() {
            continue;
        }
        let rel = path.strip_prefix(root).unwrap_or(&path).to_path_buf();
        let rel_str = rel.to_string_lossy().replace('\\', "/");
        let name = e.file_name().to_string_lossy().into_owned();
        let matches = |g: &String| {
            if g.contains('/') {
                glob(g, &rel_str)
            } else {
                glob(g, &name)
            }
        };
        if !how.include.is_empty() && !how.include.iter().any(matches) {
            continue;
        }
        if how.exclude.iter().any(matches) {
            continue;
        }
        // the CSVW metadata of a table next to it is read with the table
        if let Some(table) = name.strip_suffix("-metadata.json")
            && dir.join(table).is_file()
        {
            continue;
        }
        out.push((path, rel));
    }
    Ok(())
}

fn skipped_dir(dir: &Path, how: &Find) -> bool {
    match (&how.skip_dir, std::fs::canonicalize(dir)) {
        (Some(skip), Ok(d)) => &d == skip,
        _ => false,
    }
}

/// Whether `name` matches `pattern`: `*` matches within a path segment, `**` across
/// segments, `?` one character.
fn glob(pattern: &str, name: &str) -> bool {
    fn go(p: &[u8], n: &[u8]) -> bool {
        match p.first() {
            None => n.is_empty(),
            Some(b'*') if p.get(1) == Some(&b'*') => {
                let rest = p[2..].strip_prefix(b"/").unwrap_or(&p[2..]);
                (0..=n.len()).any(|i| go(rest, &n[i..]))
            }
            Some(b'*') => {
                let rest = &p[1..];
                for i in 0..=n.len() {
                    if go(rest, &n[i..]) {
                        return true;
                    }
                    if n.get(i) == Some(&b'/') {
                        break;
                    }
                }
                false
            }
            Some(b'?') => n.first().is_some_and(|&c| c != b'/') && go(&p[1..], &n[1..]),
            Some(&c) => n.first() == Some(&c) && go(&p[1..], &n[1..]),
        }
    }
    go(pattern.as_bytes(), name.as_bytes())
}

/// The first [`sniff::HEAD_BYTES`] of a reader, and whether that is all of it.
fn read_head(r: &mut impl Read) -> Result<(Vec<u8>, bool)> {
    let mut head = Vec::with_capacity(sniff::HEAD_BYTES);
    let n = r
        .by_ref()
        .take(sniff::HEAD_BYTES as u64)
        .read_to_end(&mut head)?;
    Ok((head, n < sniff::HEAD_BYTES))
}

/// A file, decompressed.
fn file_reader(p: &Path, name: &str, compression: Option<Codec>) -> Result<Box<dyn Read + Send>> {
    let src = Source {
        data: SourceData::File(p.to_path_buf()),
        format: RdfFormat::NQuads,
        compression,
        max_decompressed: None,
        graph: None,
        base: None,
        name: name.to_string(),
        lenient: false,
    };
    let codec = src.codec()?;
    let f = std::fs::File::open(p).with_context(|| format!("opening {}", p.display()))?;
    Ok(codec.reader_send(std::io::BufReader::with_capacity(1 << 16, f), None)?)
}

/// Standard input, decompressed.
fn stdin_reader(compression: Option<Codec>) -> Result<Box<dyn Read + Send>> {
    let mut stdin = std::io::stdin();
    let mut head = [0u8; 4];
    let mut n = 0;
    while n < head.len() {
        match stdin.read(&mut head[n..])? {
            0 => break,
            k => n += k,
        }
    }
    let (codec, warning) = Codec::detect(compression, &head[..n], None)?;
    if let Some(w) = warning {
        tracing::warn!("{w}");
    }
    if !codec.supported() {
        bail!("stdin: built without {codec}");
    }
    let r = std::io::Cursor::new(head[..n].to_vec()).chain(stdin);
    Ok(codec.reader_send(std::io::BufReader::with_capacity(1 << 16, r), None)?)
}

/// N-Quads from a thread that transcodes an input in one of Jena's syntaxes. The
/// transcoder's error ends the stream as an I/O error.
struct Transcoded {
    pipe: std::io::PipeReader,
    worker: Option<std::thread::JoinHandle<std::result::Result<(), String>>>,
}

impl Read for Transcoded {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.pipe.read(buf)?;
        if n == 0
            && !buf.is_empty()
            && let Some(worker) = self.worker.take()
        {
            match worker.join() {
                Ok(Ok(())) => {}
                Ok(Err(e)) => return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, e)),
                Err(_) => return Err(std::io::Error::other("the transcoding thread panicked")),
            }
        }
        Ok(n)
    }
}

fn parse_compression(s: &str) -> Result<Option<Codec>> {
    Ok(match s {
        "auto" => None,
        c => Some(Codec::parse(c)?),
    })
}

/// A parse error as `name:line:column: message`, or `name: message` without a position.
fn parse_error(name: &str, e: &oxrdfio::RdfParseError) -> String {
    if let oxrdfio::RdfParseError::Syntax(s) = e
        && let Some(at) = s.location()
    {
        let text = s.to_string();
        // the parsers' own text names the position again
        let msg = match text.strip_prefix("Parser error") {
            Some(rest) => rest.split_once(": ").map_or(text.as_str(), |(_, m)| m),
            None => text.as_str(),
        };
        return format!(
            "{name}:{}:{}: {msg}",
            at.start.line + 1,
            at.start.column + 1
        );
    }
    format!("{name}: {e}")
}

/// What one input gave.
struct Outcome {
    statements: u64,
    /// Each error with the input's name (and the position, when there is one).
    errors: Vec<String>,
    checker: Option<TermChecker>,
}

/// The shared options of every input of a run.
struct Run {
    compression: Option<Codec>,
    lenient: bool,
    check: bool,
    csv: CsvArgs,
}

/// `sparkles convert`: exits with status 1 when an input had errors, or warnings under
/// `--strict`.
pub fn run(a: ConvertArgs) -> Result<()> {
    let (check, strict) = (a.check || a.validate, a.strict || a.validate);
    let compression = parse_compression(&a.compression)?;
    let skip_dir = match &a.out_dir {
        Some(d) => {
            std::fs::create_dir_all(d).with_context(|| format!("creating {}", d.display()))?;
            Some(std::fs::canonicalize(d)?)
        }
        None => None,
    };
    let found = Input::find(
        &a.files,
        &Find {
            syntax: a.syntax.as_deref(),
            base: a.base.as_deref(),
            compression,
            recursive: a.recursive,
            include: &a.include,
            exclude: &a.exclude,
            skip_dir,
        },
    )?;
    let inputs = found.inputs;
    let csv = CsvArgs {
        mapping: a.mapping.clone(),
        template: a.template.clone(),
        base: a.base.clone(),
        key: a.key.clone(),
    };
    if (csv.mapping.is_some() || csv.template.is_some() || csv.key.is_some())
        && !inputs.iter().any(|i| i.table.is_some())
    {
        bail!("--mapping, --template and --key apply to CSV and TSV files, and none was given");
    }
    let r = Run {
        compression,
        lenient: a.lenient,
        check,
        csv,
    };
    if let Some(dir) = &a.out_dir {
        return out_dir(&a, &r, &inputs, dir, strict);
    }
    let started = Instant::now();
    let writes = !(a.count || a.sink || a.validate);
    let mut out = if writes {
        let format = match (&a.output, &a.output_file) {
            (Some(o), _) => {
                Syntax::named(o).with_context(|| format!("unknown output syntax '{o}'"))?
            }
            (None, Some(f)) => Syntax::of_path(f).unwrap_or(Syntax::Rdf(RdfFormat::NQuads)),
            (None, None) => Syntax::Rdf(RdfFormat::NQuads),
        };
        let codec = match (&a.compress, &a.output_file) {
            (Some(c), _) => Codec::parse(c)?,
            (None, Some(f)) => Codec::from_extension(f).unwrap_or(Codec::None),
            (None, None) => Codec::None,
        };
        if !codec.supported() {
            bail!("built without {codec}");
        }
        let sink: Box<dyn std::io::Write> = match &a.output_file {
            Some(f) => Box::new(
                std::fs::File::create(f).with_context(|| format!("creating {}", f.display()))?,
            ),
            None => Box::new(std::io::stdout().lock()),
        };
        let w = codec.writer(std::io::BufWriter::with_capacity(1 << 16, sink), None, 1)?;
        Some(Output::new(format, w, a.merge))
    } else {
        None
    };
    let mut tables = Tables::new(r.csv.clone());
    let mut failed = 0usize;
    let mut total = 0u64;
    for input in &inputs {
        let t = Instant::now();
        let o = match &mut out {
            Some(out) => convert_one(input, &r, &mut tables, out)?,
            None => parse_one(input, &r, &mut tables)?,
        };
        let bad = report_one(input, &o, strict);
        failed += usize::from(bad);
        total += o.statements;
        let unit = input.unit();
        if a.count {
            println!("{}: {} {unit}", input.name, o.statements);
        } else if found.walked && !a.time {
            eprintln!("{}: {} {unit}", input.name, o.statements);
        }
        if a.time {
            let secs = t.elapsed().as_secs_f64();
            eprintln!(
                "{}: {} {unit} in {secs:.2}s ({:.0} per second)",
                input.name,
                o.statements,
                o.statements as f64 / secs.max(1e-9)
            );
        }
    }
    if a.count && inputs.len() > 1 {
        println!("total: {total}");
    }
    if let Some(out) = out {
        out.finish()?;
    }
    if found.walked {
        summary(inputs.len(), total, failed, started);
    }
    if failed > 0 {
        std::process::exit(1);
    }
    Ok(())
}

/// Print an input's errors and warnings; whether it failed.
fn report_one(input: &Input, o: &Outcome, strict: bool) -> bool {
    for e in &o.errors {
        eprintln!("{e}");
    }
    let mut bad = !o.errors.is_empty();
    if let Some(c) = &o.checker {
        c.report(&input.name);
        bad |= strict && c.total > 0;
    }
    bad
}

fn summary(files: usize, statements: u64, failed: usize, started: Instant) {
    let secs = started.elapsed().as_secs_f64();
    let mut line = format!(
        "{files} {}, {statements} statements in {secs:.2}s",
        if files == 1 { "file" } else { "files" }
    );
    if failed > 0 {
        line.push_str(&format!(", {failed} failed"));
    }
    eprintln!("{line}");
}

/// The output file of an input in `--out-dir`: its relative path, with the output
/// syntax's extension in place of the input's syntax and compression extensions.
fn out_path(dir: &Path, input: &Input, syntax: Syntax, codec: Codec) -> PathBuf {
    let rel = input
        .rel
        .clone()
        .unwrap_or_else(|| PathBuf::from(&input.name));
    let name = rel
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mut stem = Codec::strip_extension(&name);
    // the syntax extension, when the file has one
    let known = input
        .path
        .as_deref()
        .is_some_and(|p| kind_of_path(p).is_some());
    if known && let Some((s, _)) = stem.rsplit_once('.') {
        stem = s;
    }
    let mut file = format!("{stem}.{}", syntax.file_extension());
    if codec != Codec::None {
        file.push_str(codec.extension());
    }
    dir.join(rel.parent().unwrap_or(Path::new(""))).join(file)
}

/// `--out-dir`: each input to its own file, `--jobs` at a time.
fn out_dir(a: &ConvertArgs, r: &Run, inputs: &[Input], dir: &Path, strict: bool) -> Result<()> {
    let started = Instant::now();
    if inputs.iter().any(|i| i.path.is_none()) {
        bail!("--out-dir converts files: standard input has no file name to write to");
    }
    let syntax = match &a.output {
        Some(o) => Syntax::named(o).with_context(|| format!("unknown output syntax '{o}'"))?,
        None => Syntax::Rdf(RdfFormat::NQuads),
    };
    let codec = match &a.compress {
        Some(c) => Codec::parse(c)?,
        None => Codec::None,
    };
    if !codec.supported() {
        bail!("built without {codec}");
    }
    // two inputs that would write the same file are refused before anything is written
    let targets: Vec<PathBuf> = inputs
        .iter()
        .map(|i| out_path(dir, i, syntax, codec))
        .collect();
    let mut seen = std::collections::HashMap::new();
    for (i, t) in targets.iter().enumerate() {
        if let Some(j) = seen.insert(t.clone(), i) {
            bail!(
                "{} and {} would both be written to {}",
                inputs[j].name,
                inputs[i].name,
                t.display()
            );
        }
    }
    let jobs = a
        .jobs
        .unwrap_or_else(|| {
            std::thread::available_parallelism()
                .map_or(1, |n| n.get())
                .min(8)
        })
        .clamp(1, inputs.len().max(1));
    let next = std::sync::atomic::AtomicUsize::new(0);
    let totals = Mutex::new((0u64, 0usize));
    std::thread::scope(|s| {
        for _ in 0..jobs {
            s.spawn(|| {
                let mut tables = Tables::new(r.csv.clone());
                loop {
                    let i = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    let Some(input) = inputs.get(i) else { break };
                    let target = &targets[i];
                    let t = Instant::now();
                    let (n, bad) =
                        match convert_to_file(input, r, &mut tables, target, syntax, codec, a) {
                            Ok(o) => {
                                let bad = report_one(input, &o, strict);
                                (o.statements, bad)
                            }
                            Err(e) => {
                                eprintln!("{}: {e:#}", input.name);
                                (0, true)
                            }
                        };
                    if !bad {
                        eprintln!(
                            "{} -> {}: {n} {} in {:.2}s",
                            input.name,
                            target.display(),
                            input.unit(),
                            t.elapsed().as_secs_f64()
                        );
                    }
                    let mut tot = totals.lock();
                    tot.0 += n;
                    tot.1 += usize::from(bad);
                }
            });
        }
    });
    let (total, failed) = totals.into_inner();
    summary(inputs.len(), total, failed, started);
    if failed > 0 {
        std::process::exit(1);
    }
    Ok(())
}

/// Convert one input into `target`, through a temporary file next to it that replaces
/// the target only when the input converted without errors.
fn convert_to_file(
    input: &Input,
    r: &Run,
    tables: &mut Tables,
    target: &Path,
    syntax: Syntax,
    codec: Codec,
    a: &ConvertArgs,
) -> Result<Outcome> {
    if target.exists() && !a.overwrite {
        bail!("{} exists (use --overwrite)", target.display());
    }
    let parent = target.parent().unwrap_or(Path::new("."));
    std::fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
    let tmp = tempfile::Builder::new()
        .prefix(".sparkles-convert-")
        .tempfile_in(parent)
        .with_context(|| format!("creating a file in {}", parent.display()))?;
    let file = tmp.reopen()?;
    let w = codec.writer(std::io::BufWriter::with_capacity(1 << 16, file), None, 1)?;
    let mut out = Output::new(syntax, w, a.merge);
    out.label = Some(input.name.clone());
    let o = convert_one(input, r, tables, &mut out)?;
    out.finish()?;
    if o.errors.is_empty() {
        tmp.persist(target)
            .with_context(|| format!("writing {}", target.display()))?;
    }
    Ok(o)
}

/// A serializer of the output.
enum Ser {
    Rdf(oxrdfio::WriterQuadSerializer<Box<dyn FinishWrite>>),
    Jena(RdfWriter<Box<dyn FinishWrite>>),
}

/// The output stream: one serializer for every input, created at the first statement so
/// that it can declare the prefixes the input declared before it.
pub struct Output {
    format: Syntax,
    merge: bool,
    writer: Option<Box<dyn FinishWrite>>,
    ser: Option<Ser>,
    dropped: u64,
    /// The name its warnings start with.
    pub label: Option<String>,
}

impl Output {
    pub fn new(format: Syntax, w: Box<dyn FinishWrite>, merge: bool) -> Output {
        Output {
            format,
            merge,
            writer: Some(w),
            ser: None,
            dropped: 0,
            label: None,
        }
    }

    fn triples_only(&self) -> bool {
        !self.format.quads()
    }

    pub fn start(&mut self, prefixes: impl IntoIterator<Item = (String, String)>) {
        if self.ser.is_none() {
            let w = self.writer.take().expect("the writer is taken once");
            self.ser = Some(match self.format {
                Syntax::Rdf(f) => Ser::Rdf(
                    sparkles::io::with_prefixes(RdfSerializer::from_format(f), prefixes)
                        .for_writer(w),
                ),
                Syntax::Jena(j) => Ser::Jena(RdfWriter::new(j, w)),
            });
        }
    }

    pub fn write(&mut self, mut q: Quad) -> Result<()> {
        if !q.graph_name.is_default_graph() {
            if self.merge {
                q.graph_name = GraphName::DefaultGraph;
            } else if self.triples_only() {
                self.dropped += 1;
                return Ok(());
            }
        }
        match self.ser.as_mut().expect("started") {
            Ser::Rdf(s) => s.serialize_quad(&q),
            Ser::Jena(s) => s.quad(&q),
        }
        .context("writing the output")
    }

    pub fn finish(mut self) -> Result<()> {
        self.start(std::iter::empty());
        if self.dropped > 0 {
            let who = self
                .label
                .as_deref()
                .map(|l| format!("{l}: "))
                .unwrap_or_default();
            eprintln!(
                "{who}warning: dropped {} {} in named graphs, which {} cannot hold (use \
                 --merge to write them into the default graph, or a quad syntax)",
                self.dropped,
                if self.dropped == 1 { "quad" } else { "quads" },
                self.format.name()
            );
        }
        let w = match self.ser.take().expect("started") {
            Ser::Rdf(s) => s.finish()?,
            Ser::Jena(s) => s.finish()?,
        };
        w.finish()?;
        Ok(())
    }
}

/// Convert one input to the output, streaming. A syntax error ends the input.
fn convert_one(input: &Input, r: &Run, tables: &mut Tables, out: &mut Output) -> Result<Outcome> {
    if input.table.is_some() {
        return table_one(input, r, tables, Some(out));
    }
    let reader = input.reader(r.compression)?;
    let mut parser = input.parser(r.lenient)?.for_reader(reader);
    let mut checker = r.check.then(|| TermChecker::new(WARNINGS_SHOWN));
    let mut n = 0u64;
    let mut errors = Vec::new();
    while let Some(q) = parser.next() {
        match q {
            Ok(q) => {
                if out.ser.is_none() {
                    out.start(
                        parser
                            .prefixes()
                            .map(|(p, ns)| (p.to_string(), ns.to_string()))
                            .collect::<Vec<_>>(),
                    );
                }
                if let Some(c) = &mut checker {
                    c.quad(q.as_ref());
                }
                n += 1;
                out.write(q)?;
            }
            Err(e) => {
                errors.push(parse_error(&input.name, &e));
                break;
            }
        }
    }
    Ok(Outcome {
        statements: n,
        errors,
        checker,
    })
}

/// A table: converted with the mapping options, into the output when there is one.
fn table_one(
    input: &Input,
    r: &Run,
    tables: &mut Tables,
    mut out: Option<&mut Output>,
) -> Result<Outcome> {
    let tsv = input.table == Some(TabularKind::Tsv);
    let (opts, note) = tables.options(input.path.as_deref(), &input.name, tsv)?;
    if let Some(note) = note {
        eprintln!("{}: {note}", input.name);
    }
    if let Some(out) = out.as_deref_mut() {
        out.start(sparkles::tabular::prefixes(&opts.mapping));
    }
    let reader = input.raw_reader(r.compression)?;
    let mut checker = r.check.then(|| TermChecker::new(WARNINGS_SHOWN));
    let mut failed: Option<anyhow::Error> = None;
    let res = sparkles::tabular::convert(reader, &opts, &mut |t| {
        let q = t.in_graph(GraphName::DefaultGraph);
        if let Some(c) = &mut checker {
            c.quad(q.as_ref());
        }
        if let Some(out) = out.as_deref_mut()
            && let Err(e) = out.write(q)
        {
            failed = Some(e);
            return Err(sparkles::Error::invalid("the output failed"));
        }
        Ok(())
    });
    if let Some(e) = failed {
        return Err(e);
    }
    let (statements, errors) = match res {
        Ok(stats) => {
            for w in &stats.warnings {
                eprintln!("{}: warning: {w}", input.name);
            }
            (stats.triples, Vec::new())
        }
        Err(e) => {
            let msg = e.to_string();
            let prefix = format!("{}: ", input.name);
            let msg = msg.strip_prefix(&prefix).unwrap_or(&msg);
            (0, vec![format!("{}: {msg}", input.name)])
        }
    };
    Ok(Outcome {
        statements,
        errors,
        checker,
    })
}

/// Count (and check) the statements of a sink's chunk; folded into the shared total.
struct CountSink<'a> {
    n: u64,
    checker: Option<TermChecker>,
    shared: &'a Mutex<(u64, Option<TermChecker>)>,
}

impl QuadSink for CountSink<'_> {
    fn quad(&mut self, q: Quad) -> sparkles::error::Result<()> {
        self.n += 1;
        if let Some(c) = &mut self.checker {
            c.quad(q.as_ref());
        }
        Ok(())
    }
    fn finish(self) -> sparkles::error::Result<()> {
        let mut s = self.shared.lock();
        s.0 += self.n;
        if let (Some(all), Some(mine)) = (&mut s.1, self.checker) {
            all.merge(mine);
        }
        Ok(())
    }
}

/// Parse one input without output: files in parallel, as `load` parses them, standard
/// input as a stream. When a parse fails, the input is parsed again in order for exact
/// error positions, up to [`ERRORS_SHOWN`] errors.
fn parse_one(input: &Input, r: &Run, tables: &mut Tables) -> Result<Outcome> {
    if input.table.is_some() {
        return table_one(input, r, tables, None);
    }
    let (compression, lenient, check) = (r.compression, r.lenient, r.check);
    if let Some(src) = input.source(compression, lenient) {
        let threads = std::thread::available_parallelism().map_or(1, |n| n.get());
        let shared = Mutex::new((0u64, check.then(|| TermChecker::new(WARNINGS_SHOWN))));
        let res = sparkles::io::parse_source(&src, threads, || CountSink {
            n: 0,
            checker: check.then(|| TermChecker::new(WARNINGS_SHOWN)),
            shared: &shared,
        });
        match res {
            Ok(_) => {
                let (n, checker) = shared.into_inner();
                return Ok(Outcome {
                    statements: n,
                    errors: Vec::new(),
                    checker,
                });
            }
            Err(e) => {
                let mut o = parse_in_order(input, compression, lenient, check)?;
                if o.errors.is_empty() {
                    let msg = e.to_string();
                    let prefix = format!("{}: ", input.name);
                    let msg = msg.strip_prefix(&prefix).unwrap_or(&msg);
                    o.errors.push(format!("{}: {msg}", input.name));
                }
                return Ok(o);
            }
        }
    }
    parse_in_order(input, compression, lenient, check)
}

/// Parse one input as a stream, going on after syntax errors (the parsers skip to the
/// next statement) up to [`ERRORS_SHOWN`] of them.
fn parse_in_order(
    input: &Input,
    compression: Option<Codec>,
    lenient: bool,
    check: bool,
) -> Result<Outcome> {
    let reader = input.reader(compression)?;
    let mut checker = check.then(|| TermChecker::new(WARNINGS_SHOWN));
    let mut n = 0u64;
    let mut errors = Vec::new();
    for q in input.parser(lenient)?.for_reader(reader) {
        match q {
            Ok(q) => {
                n += 1;
                if let Some(c) = &mut checker {
                    c.quad(q.as_ref());
                }
            }
            Err(e @ oxrdfio::RdfParseError::Io(_)) => {
                errors.push(parse_error(&input.name, &e));
                break;
            }
            Err(e) => {
                errors.push(parse_error(&input.name, &e));
                if errors.len() >= ERRORS_SHOWN {
                    errors.push(format!(
                        "{}: stopped after {ERRORS_SHOWN} errors",
                        input.name
                    ));
                    break;
                }
            }
        }
    }
    Ok(Outcome {
        statements: n,
        errors,
        checker,
    })
}

/// `load --check`: the term checks over the files before they are loaded. With
/// `strict`, an error when any value has warnings.
pub fn precheck(
    files: &[PathBuf],
    compression: Option<Codec>,
    lenient: bool,
    strict: bool,
) -> Result<()> {
    let mut flagged = 0;
    let r = Run {
        compression,
        lenient,
        check: true,
        csv: CsvArgs::default(),
    };
    let mut tables = Tables::new(CsvArgs::default());
    for f in files {
        let input = Input::all(std::slice::from_ref(f), None, None)?
            .pop()
            .expect("one input");
        // tables are checked when they are converted
        if input.table.is_some() {
            continue;
        }
        let o = parse_one(&input, &r, &mut tables)?;
        if let Some(e) = o.errors.first() {
            bail!("{e}");
        }
        if let Some(c) = o.checker {
            c.report(&input.name);
            flagged += c.total;
        }
    }
    if strict && flagged > 0 {
        bail!("--strict: {flagged} IRIs or language tags have warnings; nothing was loaded");
    }
    Ok(())
}

/// Read a whole input into memory (for `compare`).
pub fn read_all(input: &Input, compression: Option<Codec>) -> Result<Vec<Quad>> {
    if input.table.is_some() {
        bail!(
            "{}: a CSV or TSV table (convert it to RDF first)",
            input.name
        );
    }
    let reader = input.reader(compression)?;
    input
        .parser(false)?
        .for_reader(reader)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| anyhow::anyhow!("{}", parse_error(&input.name, &e)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn syntax_names() {
        for (n, f) in [
            ("TTL", RdfFormat::Turtle),
            ("N-Triples", RdfFormat::NTriples),
            ("nq", RdfFormat::NQuads),
            ("RDF/XML", RdfFormat::RdfXml),
            ("text/turtle", RdfFormat::Turtle),
            ("application/n-quads", RdfFormat::NQuads),
            ("TriG", RdfFormat::TriG),
        ] {
            assert_eq!(rdf_syntax(n), Some(f), "{n}");
        }
        assert!(matches!(
            rdf_syntax("JSON-LD"),
            Some(RdfFormat::JsonLd { .. })
        ));
        assert_eq!(rdf_syntax("csv"), None);
        assert_eq!(named_kind("csv"), Some(Kind::Table(TabularKind::Csv)));
    }

    #[test]
    fn jena_syntax_names() {
        for (n, j) in [
            ("TriX", JenaFormat::TriX),
            ("application/trix", JenaFormat::TriX),
            ("application/trix+xml", JenaFormat::TriX),
            ("rt", JenaFormat::Thrift),
            ("rdf-protobuf", JenaFormat::Protobuf),
            ("RDF/JSON", JenaFormat::RdfJson),
        ] {
            assert_eq!(Syntax::named(n), Some(Syntax::Jena(j)), "{n}");
        }
        assert_eq!(
            Syntax::named("json"),
            Some(Syntax::Rdf(rdf_syntax("json").unwrap()))
        );
        for (p, j) in [
            ("a.trix", JenaFormat::TriX),
            ("a.TRIX.gz", JenaFormat::TriX),
            ("a.rj", JenaFormat::RdfJson),
        ] {
            assert_eq!(
                Syntax::of_path(std::path::Path::new(p)),
                Some(Syntax::Jena(j)),
                "{p}"
            );
        }
    }

    #[test]
    fn globs() {
        assert!(glob("*.ttl", "a.ttl"));
        assert!(!glob("*.ttl", "a.ttl.gz"));
        assert!(glob("*.ttl*", "a.ttl.gz"));
        assert!(glob("a?c", "abc"));
        assert!(!glob("*/x.nt", "a/b/x.nt"));
        assert!(glob("**/x.nt", "a/b/x.nt"));
        assert!(glob("**/x.nt", "x.nt"));
        assert!(glob("a/**", "a/b/c"));
        assert!(!glob("a/*", "a/b/c"));
    }

    #[test]
    fn output_paths() {
        let input = |name: &str, rel: &str| {
            let mut i = Input::new(
                name.into(),
                Some(PathBuf::from(name)),
                Kind::Rdf(Syntax::Rdf(RdfFormat::Turtle)),
                None,
            );
            i.rel = Some(PathBuf::from(rel));
            i
        };
        let nt = Syntax::Rdf(RdfFormat::NTriples);
        assert_eq!(
            out_path(
                Path::new("o"),
                &input("in/a/b.ttl.gz", "a/b.ttl.gz"),
                nt,
                Codec::None
            ),
            PathBuf::from("o/a/b.nt")
        );
        assert_eq!(
            out_path(
                Path::new("o"),
                &input("in/x.v1.rdf", "x.v1.rdf"),
                nt,
                Codec::Gzip
            ),
            PathBuf::from("o/x.v1.nt.gz")
        );
        // a file whose syntax came from its content keeps its whole name
        assert_eq!(
            out_path(
                Path::new("o"),
                &input("in/data.v2", "data.v2"),
                nt,
                Codec::None
            ),
            PathBuf::from("o/data.v2.nt")
        );
    }
}
