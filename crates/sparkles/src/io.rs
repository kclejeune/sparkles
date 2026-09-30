//! RDF input: format detection and (parallel) parsing of sources (RIOT equivalent).

use crate::error::{Error, Result};
use memmap2::Mmap;
use oxrdf::{GraphName, NamedNode, Quad};
pub use oxrdfio::RdfFormat;
use oxrdfio::RdfParser;
use parking_lot::Mutex;
use rayon::prelude::*;
use std::collections::BTreeMap;
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};

/// Minimum bytes per chunk before splitting a document for parallel parsing.
const PARALLEL_MIN_CHUNK: usize = 8 << 20;

pub enum SourceData {
    File(PathBuf),
    Bytes(Vec<u8>),
}

pub struct Source {
    pub data: SourceData,
    pub format: RdfFormat,
    pub gzip: bool,
    /// Load triples into this graph (default graph if `None`). Named graphs from quad
    /// formats are kept.
    pub graph: Option<NamedNode>,
    pub base: Option<String>,
    /// Human-readable name for errors.
    pub name: String,
}

impl Source {
    pub fn from_path(path: &Path, graph: Option<NamedNode>) -> Result<Source> {
        let (format, gzip) = format_for_path(path).ok_or_else(|| {
            Error::invalid(format!("cannot determine RDF format of {}", path.display()))
        })?;
        let abs = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        Ok(Source {
            data: SourceData::File(path.to_path_buf()),
            format,
            gzip,
            graph,
            base: Some(format!("file://{}", abs.display())),
            name: path.display().to_string(),
        })
    }

    pub fn from_bytes(bytes: Vec<u8>, format: RdfFormat, graph: Option<NamedNode>) -> Source {
        Source {
            data: SourceData::Bytes(bytes),
            format,
            gzip: false,
            graph,
            base: None,
            name: "<request body>".into(),
        }
    }
}

/// Guess format (and gzip) from a file name like `data.ttl.gz`.
pub fn format_for_path(path: &Path) -> Option<(RdfFormat, bool)> {
    let name = path.file_name()?.to_str()?.to_ascii_lowercase();
    let (name, gz) = match name.strip_suffix(".gz") {
        Some(n) => (n.to_string(), true),
        None => (name, false),
    };
    let ext = name.rsplit('.').next()?;
    let f = match ext {
        "ttl" | "turtle" => RdfFormat::Turtle,
        "nt" | "ntriples" => RdfFormat::NTriples,
        "nq" | "nquads" => RdfFormat::NQuads,
        "trig" => RdfFormat::TriG,
        "rdf" | "owl" | "xml" | "rdfxml" => RdfFormat::RdfXml,
        "jsonld" | "json" => RdfFormat::JsonLd {
            profile: oxrdfio::JsonLdProfileSet::empty(),
        },
        "n3" => RdfFormat::N3,
        _ => RdfFormat::from_extension(ext)?,
    };
    Some((f, gz))
}

/// Parse a media type (ignoring parameters) into an RDF format.
pub fn format_for_media_type(mt: &str) -> Option<RdfFormat> {
    let base = mt.split(';').next()?.trim().to_ascii_lowercase();
    match base.as_str() {
        "text/turtle" | "application/x-turtle" => Some(RdfFormat::Turtle),
        "application/n-triples" | "text/plain" => Some(RdfFormat::NTriples),
        "application/n-quads" | "text/x-nquads" => Some(RdfFormat::NQuads),
        "application/trig" => Some(RdfFormat::TriG),
        "application/rdf+xml" | "application/xml" | "text/xml" => Some(RdfFormat::RdfXml),
        "application/ld+json" | "application/json" => Some(RdfFormat::JsonLd {
            profile: oxrdfio::JsonLdProfileSet::empty(),
        }),
        _ => RdfFormat::from_media_type(&base),
    }
}

enum Loaded {
    Map(Mmap),
    Vec(Vec<u8>),
}
impl AsRef<[u8]> for Loaded {
    fn as_ref(&self) -> &[u8] {
        match self {
            Loaded::Map(m) => m,
            Loaded::Vec(v) => v,
        }
    }
}

fn load_bytes(src: &Source) -> Result<Loaded> {
    match &src.data {
        SourceData::Bytes(b) if !src.gzip => Ok(Loaded::Vec(b.clone())),
        SourceData::Bytes(b) => {
            let mut out = Vec::new();
            flate2::read::MultiGzDecoder::new(&b[..]).read_to_end(&mut out)?;
            Ok(Loaded::Vec(out))
        }
        SourceData::File(p) => {
            let f = File::open(p)?;
            if src.gzip {
                let mut out = Vec::new();
                flate2::read::MultiGzDecoder::new(std::io::BufReader::new(f))
                    .read_to_end(&mut out)?;
                Ok(Loaded::Vec(out))
            } else if f.metadata()?.len() == 0 {
                Ok(Loaded::Vec(Vec::new()))
            } else {
                // SAFETY: read-only mapping of an input file for the duration of the parse.
                Ok(Loaded::Map(unsafe { Mmap::map(&f)? }))
            }
        }
    }
}

/// Receives parsed quads; one sink per parallel chunk.
pub trait QuadSink: Send {
    fn quad(&mut self, q: Quad) -> Result<()>;
    fn finish(self) -> Result<()>;
}

/// Parse a source, splitting line-based and Turtle documents into chunks parsed in
/// parallel (QLever-style parallel parsing). `make_sink` is called once per chunk.
/// Returns the prefixes declared in the document.
pub fn parse_source<S: QuadSink, F: Fn() -> S + Sync>(
    src: &Source,
    parallelism: usize,
    make_sink: F,
) -> Result<BTreeMap<String, String>> {
    let bytes = load_bytes(src)?;
    let slice = bytes.as_ref();
    let mut parser = RdfParser::from_format(src.format);
    if let Some(base) = &src.base {
        parser = parser
            .with_base_iri(base.clone())
            .map_err(|e| Error::invalid(e.to_string()))?;
    }
    if let Some(g) = &src.graph {
        parser = parser.with_default_graph(GraphName::NamedNode(g.clone()));
    }
    let prefixes = Mutex::new(BTreeMap::new());
    let splittable = matches!(
        src.format,
        RdfFormat::NTriples | RdfFormat::NQuads | RdfFormat::Turtle
    );
    let n = if splittable {
        (slice.len() / PARALLEL_MIN_CHUNK).clamp(1, parallelism.max(1))
    } else {
        1
    };
    let err = |e: &dyn std::fmt::Display| Error::RdfParse(format!("{}: {e}", src.name));
    if n > 1 {
        let parsers = parser.split_slice_for_parallel_parsing(slice, n);
        parsers.into_par_iter().try_for_each(|mut p| -> Result<()> {
            let mut sink = make_sink();
            for q in p.by_ref() {
                sink.quad(q.map_err(|e| err(&e))?)?;
            }
            let mut pm = prefixes.lock();
            for (k, v) in p.prefixes() {
                pm.insert(k.to_string(), v.to_string());
            }
            sink.finish()
        })?;
    } else {
        let mut sink = make_sink();
        let mut p = parser.for_slice(slice);
        for q in p.by_ref() {
            sink.quad(q.map_err(|e| err(&e))?)?;
        }
        let mut pm = prefixes.lock();
        for (k, v) in p.prefixes() {
            pm.insert(k.to_string(), v.to_string());
        }
        sink.finish()?;
    }
    Ok(prefixes.into_inner())
}

/// Parse a source fully into memory (small inputs: updates, uploads, rule files).
pub fn parse_to_vec(src: &Source) -> Result<(Vec<Quad>, BTreeMap<String, String>)> {
    struct VecSink<'a>(&'a Mutex<Vec<Quad>>, Vec<Quad>);
    impl QuadSink for VecSink<'_> {
        fn quad(&mut self, q: Quad) -> Result<()> {
            self.1.push(q);
            Ok(())
        }
        fn finish(self) -> Result<()> {
            self.0.lock().extend(self.1);
            Ok(())
        }
    }
    let out = Mutex::new(Vec::new());
    let prefixes = parse_source(src, 1, || VecSink(&out, Vec::new()))?;
    Ok((out.into_inner(), prefixes))
}

/// Well-known prefixes (Jena's `PrefixMapping.Standard` + common vocabularies).
pub fn standard_prefixes() -> BTreeMap<String, String> {
    [
        ("rdf", "http://www.w3.org/1999/02/22-rdf-syntax-ns#"),
        ("rdfs", "http://www.w3.org/2000/01/rdf-schema#"),
        ("owl", "http://www.w3.org/2002/07/owl#"),
        ("xsd", "http://www.w3.org/2001/XMLSchema#"),
        ("dc", "http://purl.org/dc/elements/1.1/"),
        ("dcterms", "http://purl.org/dc/terms/"),
        ("foaf", "http://xmlns.com/foaf/0.1/"),
        ("skos", "http://www.w3.org/2004/02/skos/core#"),
        ("schema", "http://schema.org/"),
        ("sh", "http://www.w3.org/ns/shacl#"),
        ("prov", "http://www.w3.org/ns/prov#"),
    ]
    .into_iter()
    .map(|(a, b)| (a.to_string(), b.to_string()))
    .collect()
}
