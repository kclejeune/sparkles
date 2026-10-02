//! `sparkles convert` (alias `riot`): parse, validate, count and convert RDF files,
//! streaming (spec G05 §3.1). Also the term checks of `load --check`.

use super::terms::TermChecker;
use anyhow::{Context, Result, bail};
use oxrdf::{GraphName, Quad};
use oxrdfio::{RdfFormat, RdfParser, RdfSerializer};
use parking_lot::Mutex;
use sparkles::codec::Codec;
use sparkles::io::{QuadSink, Source, SourceData};
use std::io::Read;
use std::path::PathBuf;
use std::time::Instant;

/// Warnings kept for printing per input.
const WARNINGS_SHOWN: usize = 100;
/// Syntax errors reported per input when a file is parsed again for them.
const ERRORS_SHOWN: usize = 20;

#[derive(clap::Args)]
pub struct ConvertArgs {
    /// Input files (`-` or none: standard input)
    files: Vec<PathBuf>,
    /// Input syntax for every input: Turtle, N-Triples, N-Quads, TriG, RDF/XML, JSON-LD,
    /// N3, short names (ttl, nt, nq, …) or a media type (default: from the file
    /// extension; N-Quads for standard input)
    #[arg(long, value_name = "LANG")]
    syntax: Option<String>,
    /// Output syntax (default: N-Quads, which is N-Triples for the default graph)
    #[arg(long, visible_alias = "out", value_name = "LANG")]
    output: Option<String>,
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
    /// Base IRI for relative IRIs (default: the file's own `file://` IRI)
    #[arg(long, value_name = "IRI")]
    base: Option<String>,
    /// Write quads of named graphs into the default graph
    #[arg(long, visible_alias = "union")]
    merge: bool,
    /// Compression of the inputs: auto (magic bytes, then the extension), none, gzip,
    /// zstd, brotli or lz4
    #[arg(long, default_value = "auto", value_name = "CODEC")]
    compression: String,
    /// Compress the output: gzip (the default when no codec is named), zstd, brotli or
    /// lz4
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

/// One input: a file or standard input.
pub struct Input {
    pub name: String,
    pub path: Option<PathBuf>,
    pub format: RdfFormat,
    pub base: Option<String>,
}

impl Input {
    /// The inputs of `files` (none, or `-`: standard input).
    pub fn all(files: &[PathBuf], syntax: Option<&str>, base: Option<&str>) -> Result<Vec<Input>> {
        let forced = match syntax {
            Some(s) => Some(rdf_syntax(s).with_context(|| format!("unknown syntax '{s}'"))?),
            None => None,
        };
        if let Some(b) = base {
            oxiri::Iri::parse(b).map_err(|e| anyhow::anyhow!("--base {b}: {e}"))?;
        }
        let stdin = || Input {
            name: "stdin".into(),
            path: None,
            format: forced.unwrap_or(RdfFormat::NQuads),
            base: base.map(str::to_string),
        };
        if files.is_empty() {
            return Ok(vec![stdin()]);
        }
        files
            .iter()
            .map(|f| {
                if f.as_os_str() == "-" {
                    return Ok(stdin());
                }
                if !f.is_file() {
                    bail!("{}: no such file", f.display());
                }
                let format = match forced {
                    Some(f) => f,
                    None => sparkles::io::format_for_path(f)
                        .map(|(f, _)| f)
                        .with_context(|| {
                            format!("{}: unknown RDF syntax (use --syntax)", f.display())
                        })?,
                };
                let abs = std::fs::canonicalize(f).unwrap_or_else(|_| f.clone());
                Ok(Input {
                    name: f.display().to_string(),
                    path: Some(f.clone()),
                    format,
                    base: Some(
                        base.map(str::to_string)
                            .unwrap_or_else(|| format!("file://{}", abs.display())),
                    ),
                })
            })
            .collect()
    }

    /// A [`Source`] of a file input, for the parallel parser.
    fn source(&self, compression: Option<Codec>, lenient: bool) -> Option<Source> {
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

    /// The decompressed bytes of the input.
    pub fn reader(&self, compression: Option<Codec>) -> Result<Box<dyn Read>> {
        match &self.path {
            Some(p) => {
                let src = self.source(compression, false).expect("a file");
                let codec = src.codec()?;
                let f =
                    std::fs::File::open(p).with_context(|| format!("opening {}", p.display()))?;
                Ok(codec.reader(std::io::BufReader::with_capacity(1 << 16, f), None)?)
            }
            None => {
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
                Ok(codec.reader(std::io::BufReader::with_capacity(1 << 16, r), None)?)
            }
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

fn parse_compression(s: &str) -> Result<Option<Codec>> {
    Ok(match s {
        "auto" => None,
        c => Some(Codec::parse(c)?),
    })
}

/// What one input gave.
struct Outcome {
    statements: u64,
    errors: Vec<String>,
    checker: Option<TermChecker>,
}

/// `sparkles convert`: exits with status 1 when an input had errors, or warnings under
/// `--strict`.
pub fn run(a: ConvertArgs) -> Result<()> {
    let (check, strict) = (a.check || a.validate, a.strict || a.validate);
    let compression = parse_compression(&a.compression)?;
    let inputs = Input::all(&a.files, a.syntax.as_deref(), a.base.as_deref())?;
    let writes = !(a.count || a.sink || a.validate);
    let mut out = if writes {
        let format = match &a.output {
            Some(o) => rdf_syntax(o).with_context(|| format!("unknown output syntax '{o}'"))?,
            None => RdfFormat::NQuads,
        };
        let codec = match &a.compress {
            Some(c) => Codec::parse(c)?,
            None => Codec::None,
        };
        if !codec.supported() {
            bail!("built without {codec}");
        }
        let w = codec.writer(
            std::io::BufWriter::with_capacity(1 << 16, std::io::stdout().lock()),
            None,
            1,
        )?;
        Some(Output::new(format, w, a.merge))
    } else {
        None
    };
    let mut failed = false;
    let mut total = 0u64;
    for input in &inputs {
        let t = Instant::now();
        let o = match &mut out {
            Some(out) => convert_one(input, compression, a.lenient, check, out)?,
            None => parse_one(input, compression, a.lenient, check)?,
        };
        for e in &o.errors {
            eprintln!("{}: {e}", input.name);
        }
        if let Some(c) = &o.checker {
            c.report(&input.name);
            failed |= strict && c.total > 0;
        }
        failed |= !o.errors.is_empty();
        total += o.statements;
        let unit = if is_quad_syntax(input.format) {
            "quads"
        } else {
            "triples"
        };
        if a.count {
            println!("{}: {} {unit}", input.name, o.statements);
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
    if failed {
        std::process::exit(1);
    }
    Ok(())
}

/// The output stream: one serializer for every input, created at the first statement so
/// that it can declare the prefixes the input declared before it.
struct Output {
    format: RdfFormat,
    merge: bool,
    writer: Option<Box<dyn sparkles::codec::FinishWrite>>,
    ser: Option<oxrdfio::WriterQuadSerializer<Box<dyn sparkles::codec::FinishWrite>>>,
    dropped: u64,
}

impl Output {
    fn new(format: RdfFormat, w: Box<dyn sparkles::codec::FinishWrite>, merge: bool) -> Output {
        Output {
            format,
            merge,
            writer: Some(w),
            ser: None,
            dropped: 0,
        }
    }

    fn triples_only(&self) -> bool {
        !is_quad_syntax(self.format)
    }

    fn start(&mut self, prefixes: impl IntoIterator<Item = (String, String)>) {
        if self.ser.is_none() {
            let ser =
                sparkles::io::with_prefixes(RdfSerializer::from_format(self.format), prefixes);
            let w = self.writer.take().expect("the writer is taken once");
            self.ser = Some(ser.for_writer(w));
        }
    }

    fn write(&mut self, mut q: Quad) -> Result<()> {
        if !q.graph_name.is_default_graph() {
            if self.merge {
                q.graph_name = GraphName::DefaultGraph;
            } else if self.triples_only() {
                self.dropped += 1;
                return Ok(());
            }
        }
        self.ser
            .as_mut()
            .expect("started")
            .serialize_quad(&q)
            .context("writing the output")
    }

    fn finish(mut self) -> Result<()> {
        self.start(std::iter::empty());
        if self.dropped > 0 {
            eprintln!(
                "warning: dropped {} {} in named graphs, which {} cannot hold (use --merge \
                 to write them into the default graph, or a quad syntax)",
                self.dropped,
                if self.dropped == 1 { "quad" } else { "quads" },
                self.format.name()
            );
        }
        let w = self.ser.take().expect("started").finish()?;
        w.finish()?;
        Ok(())
    }
}

/// Convert one input to the output, streaming. A syntax error ends the input.
fn convert_one(
    input: &Input,
    compression: Option<Codec>,
    lenient: bool,
    check: bool,
    out: &mut Output,
) -> Result<Outcome> {
    let reader = input.reader(compression)?;
    let mut parser = input.parser(lenient)?.for_reader(reader);
    let mut checker = check.then(|| TermChecker::new(WARNINGS_SHOWN));
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
                errors.push(e.to_string());
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
fn parse_one(
    input: &Input,
    compression: Option<Codec>,
    lenient: bool,
    check: bool,
) -> Result<Outcome> {
    if let Some(src) = input.source(compression, lenient) {
        let threads = std::thread::available_parallelism().map_or(1, |n| n.get());
        let shared = Mutex::new((0u64, check.then(|| TermChecker::new(WARNINGS_SHOWN))));
        let r = sparkles::io::parse_source(&src, threads, || CountSink {
            n: 0,
            checker: check.then(|| TermChecker::new(WARNINGS_SHOWN)),
            shared: &shared,
        });
        match r {
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
                    o.errors
                        .push(msg.strip_prefix(&prefix).unwrap_or(&msg).to_string());
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
            Err(oxrdfio::RdfParseError::Io(e)) => {
                errors.push(e.to_string());
                break;
            }
            Err(e) => {
                errors.push(e.to_string());
                if errors.len() >= ERRORS_SHOWN {
                    errors.push(format!("stopped after {ERRORS_SHOWN} errors"));
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
    for f in files {
        let input = Input::all(std::slice::from_ref(f), None, None)?
            .pop()
            .expect("one input");
        let o = parse_one(&input, compression, lenient, true)?;
        if let Some(e) = o.errors.first() {
            bail!("{}: {e}", input.name);
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
    let reader = input.reader(compression)?;
    input
        .parser(false)?
        .for_reader(reader)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| anyhow::anyhow!("{}: {e}", input.name))
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
    }
}
