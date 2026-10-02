//! RDF input: format detection and (parallel) parsing of sources (RIOT equivalent).

use crate::codec::Codec;
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
    /// The compression of the data: `None` detects it (magic bytes, then the file
    /// extension); a codec is checked against the magic bytes.
    pub compression: Option<Codec>,
    /// Fail past this many decompressed bytes (compressed data only).
    pub max_decompressed: Option<u64>,
    /// Load triples into this graph (default graph if `None`). Named graphs from quad
    /// formats are kept.
    pub graph: Option<NamedNode>,
    pub base: Option<String>,
    /// Human-readable name for errors.
    pub name: String,
    /// Skip the validation of IRIs and language tags (oxttl's lenient mode), for data
    /// such as DBpedia's whose IRIs are not all valid RFC 3987 IRIs. Syntax errors still
    /// fail.
    pub lenient: bool,
}

impl Source {
    pub fn from_path(path: &Path, graph: Option<NamedNode>) -> Result<Source> {
        let (format, _) = format_for_path(path).ok_or_else(|| {
            Error::invalid(format!("cannot determine RDF format of {}", path.display()))
        })?;
        let abs = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        Ok(Source {
            data: SourceData::File(path.to_path_buf()),
            format,
            compression: None,
            max_decompressed: None,
            graph,
            base: Some(format!("file://{}", abs.display())),
            name: path.display().to_string(),
            lenient: false,
        })
    }

    pub fn from_bytes(bytes: Vec<u8>, format: RdfFormat, graph: Option<NamedNode>) -> Source {
        Source {
            data: SourceData::Bytes(bytes),
            format,
            compression: None,
            max_decompressed: None,
            graph,
            base: None,
            name: "<request body>".into(),
            lenient: false,
        }
    }

    /// The data's compression: the explicit choice (checked against the magic bytes),
    /// else the magic bytes, else the file extension. Logs a warning when the file
    /// name and the data disagree.
    pub fn codec(&self) -> Result<Codec> {
        let mut prefix = [0u8; 4];
        let (n, path) = match &self.data {
            SourceData::Bytes(b) => {
                let n = b.len().min(4);
                prefix[..n].copy_from_slice(&b[..n]);
                (n, None)
            }
            SourceData::File(p) => {
                let mut f = File::open(p)?;
                let mut n = 0;
                while n < 4 {
                    match f.read(&mut prefix[n..])? {
                        0 => break,
                        k => n += k,
                    }
                }
                (n, Some(p.as_path()))
            }
        };
        let (codec, warning) = Codec::detect(self.compression, &prefix[..n], path)?;
        if let Some(w) = warning {
            tracing::warn!("{w}");
        }
        if !codec.supported() {
            return Err(Error::Unsupported(format!(
                "{}: built without {}",
                self.name,
                codec.name()
            )));
        }
        Ok(codec)
    }
}

/// The compression of a stream, detected like [`Source::codec`] (magic bytes, then the
/// name `path`, which may be a URL path), and the bytes read from `r` to tell: they come
/// before the rest of `r`.
pub fn sniff_codec(r: &mut impl Read, path: Option<&Path>, name: &str) -> Result<(Codec, Vec<u8>)> {
    let mut head = [0u8; 4];
    let mut n = 0;
    while n < head.len() {
        match r.read(&mut head[n..]) {
            Ok(0) => break,
            Ok(k) => n += k,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => return Err(crate::codec::io_error(e)),
        }
    }
    let (codec, warning) = Codec::detect(None, &head[..n], path)?;
    if let Some(w) = warning {
        tracing::warn!("{w}");
    }
    if !codec.supported() {
        return Err(Error::Unsupported(format!(
            "{name}: built without {}",
            codec.name()
        )));
    }
    Ok((codec, head[..n].to_vec()))
}

/// `ser` with these prefixes (for the formats that use them), leaving out the ones
/// whose IRI does not parse. Each prefix is added in place: the serializer is not copied
/// once per prefix.
pub fn with_prefixes(
    mut ser: oxrdfio::RdfSerializer,
    prefixes: impl IntoIterator<Item = (String, String)>,
) -> oxrdfio::RdfSerializer {
    if !matches!(
        ser.format(),
        RdfFormat::Turtle | RdfFormat::TriG | RdfFormat::RdfXml
    ) {
        return ser;
    }
    for (p, ns) in prefixes {
        if oxiri::Iri::parse(ns.as_str()).is_err() {
            continue;
        }
        ser = ser.with_prefix(p, ns).expect("a checked IRI");
    }
    ser
}

/// Guess the format (and compression) from a file name like `data.ttl.gz` or
/// `data.nq.zst`.
pub fn format_for_path(path: &Path) -> Option<(RdfFormat, Option<Codec>)> {
    let codec = Codec::from_extension(path);
    let name = path.file_name()?.to_str()?.to_ascii_lowercase();
    let name = Codec::strip_extension(&name);
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
    Some((f, codec))
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
    let codec = src.codec()?;
    let inflate = |r: &mut dyn Read| -> Result<Loaded> {
        let mut out = Vec::new();
        codec
            .reader(r, src.max_decompressed)?
            .read_to_end(&mut out)
            .map_err(crate::codec::io_error)?;
        Ok(Loaded::Vec(out))
    };
    match &src.data {
        SourceData::Bytes(b) if codec == Codec::None => Ok(Loaded::Vec(b.clone())),
        SourceData::Bytes(b) => inflate(&mut &b[..]),
        SourceData::File(p) => {
            let mut f = File::open(p)?;
            if codec != Codec::None {
                inflate(&mut f)
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
///
/// Compressed N-Triples and N-Quads are decompressed and parsed as a stream of blocks
/// (see [`STREAM_BLOCK`]), so their decompressed size is not bounded by memory; other
/// compressed documents are decompressed into memory first.
pub fn parse_source<S: QuadSink, F: Fn() -> S + Sync>(
    src: &Source,
    parallelism: usize,
    make_sink: F,
) -> Result<BTreeMap<String, String>> {
    let mut parser = RdfParser::from_format(src.format);
    if let Some(base) = &src.base {
        parser = parser
            .with_base_iri(base.clone())
            .map_err(|e| Error::invalid(e.to_string()))?;
    }
    if let Some(g) = &src.graph {
        parser = parser.with_default_graph(GraphName::NamedNode(g.clone()));
    }
    if src.lenient {
        parser = parser.lenient();
    }
    let line_based = matches!(src.format, RdfFormat::NTriples | RdfFormat::NQuads);
    if line_based && src.codec()? != Codec::None {
        let block = STREAM_BLOCK.max(parallelism.max(1) * PARALLEL_MIN_CHUNK);
        parse_stream(
            src,
            parser,
            parallelism,
            block,
            PARALLEL_MIN_CHUNK,
            make_sink,
        )?;
        return Ok(BTreeMap::new());
    }
    let bytes = load_bytes(src)?;
    let slice = bytes.as_ref();
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
        parsers
            .into_par_iter()
            .try_for_each(|mut p| -> Result<()> {
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

/// Decompressed bytes per block of a streamed source: blocks end at a line break and
/// are split for parallel parsing like a whole document. One block is parsed while the
/// next one is decompressed.
const STREAM_BLOCK: usize = 256 << 20;

/// Parse compressed N-Triples / N-Quads block by block: a thread decompresses the next
/// block while the current one is parsed in parallel. Each of `parallelism` sinks takes
/// one chunk of every block, so sinks (and the batches they write) span blocks.
fn parse_stream<S: QuadSink, F: Fn() -> S + Sync>(
    src: &Source,
    parser: RdfParser,
    parallelism: usize,
    block: usize,
    min_chunk: usize,
    make_sink: F,
) -> Result<()> {
    let codec = src.codec()?;
    let parallelism = parallelism.max(1);
    let mut sinks: Vec<S> = Vec::new();
    std::thread::scope(|scope| -> Result<()> {
        let (tx, rx) = std::sync::mpsc::sync_channel::<Result<(u64, Vec<u8>)>>(1);
        scope.spawn(move || {
            if let Err(e) = read_blocks(src, codec, block, &tx) {
                let _ = tx.send(Err(e));
            }
        });
        for msg in rx {
            let (start, bytes) = msg?;
            let n = (bytes.len() / min_chunk).clamp(1, parallelism);
            let parsers = parser.clone().split_slice_for_parallel_parsing(&bytes, n);
            while sinks.len() < parsers.len() {
                sinks.push(make_sink());
            }
            let err = |e: &dyn std::fmt::Display| {
                Error::RdfParse(format!(
                    "{} (in the decompressed bytes from offset {start}): {e}",
                    src.name
                ))
            };
            sinks
                .par_iter_mut()
                .zip(parsers)
                .try_for_each(|(sink, p)| -> Result<()> {
                    for q in p {
                        sink.quad(q.map_err(|e| err(&e))?)?;
                    }
                    Ok(())
                })?;
        }
        Ok(())
    })?;
    sinks.into_par_iter().try_for_each(QuadSink::finish)
}

/// Send `src` decompressed in blocks of at least `block` bytes that end at a line break
/// (the last one at the end of the data), with their offsets. Stops when the receiver
/// is gone.
fn read_blocks(
    src: &Source,
    codec: Codec,
    block: usize,
    tx: &std::sync::mpsc::SyncSender<Result<(u64, Vec<u8>)>>,
) -> Result<()> {
    let raw: Box<dyn Read + '_> = match &src.data {
        SourceData::File(p) => Box::new(File::open(p)?),
        SourceData::Bytes(b) => Box::new(&b[..]),
    };
    let mut r = codec.reader(raw, src.max_decompressed)?;
    let mut offset = 0u64;
    let mut buf: Vec<u8> = Vec::new();
    let mut target = block;
    let mut eof = false;
    loop {
        while buf.len() < target && !eof {
            let have = buf.len();
            buf.resize(target, 0);
            match r.read(&mut buf[have..]) {
                Ok(0) => {
                    buf.truncate(have);
                    eof = true;
                }
                Ok(k) => buf.truncate(have + k),
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => buf.truncate(have),
                Err(e) => return Err(crate::codec::io_error(e)),
            }
        }
        let cut = match buf.iter().rposition(|&c| c == b'\n') {
            _ if eof => buf.len(),
            Some(i) => i + 1,
            // a line longer than a block: read on
            None => {
                target = buf.len() + block;
                continue;
            }
        };
        let rest = buf.split_off(cut);
        let len = buf.len() as u64;
        if len > 0
            && tx
                .send(Ok((offset, std::mem::replace(&mut buf, rest))))
                .is_err()
        {
            return Ok(());
        }
        offset += len;
        target = block;
        if eof {
            return Ok(());
        }
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serializers_take_every_valid_prefix() {
        let many = (0..5000).map(|i| (format!("p{i}"), format!("http://p{i}.example/")));
        let bad = [("bad".to_string(), "not an IRI".to_string())];
        for format in [RdfFormat::Turtle, RdfFormat::TriG, RdfFormat::RdfXml] {
            let ser = with_prefixes(
                oxrdfio::RdfSerializer::from_format(format),
                many.clone().chain(bad.clone()),
            );
            let mut w = ser.for_writer(Vec::new());
            let s = oxrdf::NamedNodeRef::new_unchecked("http://p4999.example/s");
            w.serialize_triple(oxrdf::TripleRef::new(s, s, s)).unwrap();
            let out = String::from_utf8(w.finish().unwrap()).unwrap();
            assert!(out.contains("p4999"), "{format}");
            assert!(!out.contains("bad"), "{format}");
        }
        // formats without prefixes are left as they are
        let ser = with_prefixes(
            oxrdfio::RdfSerializer::from_format(RdfFormat::NTriples),
            many,
        );
        assert_eq!(ser.format(), RdfFormat::NTriples);
    }

    struct Collect<'a>(&'a Mutex<Vec<String>>, Vec<String>, &'a Mutex<usize>);
    impl QuadSink for Collect<'_> {
        fn quad(&mut self, q: Quad) -> Result<()> {
            self.1.push(q.to_string());
            Ok(())
        }
        fn finish(self) -> Result<()> {
            *self.2.lock() += 1;
            self.0.lock().extend(self.1);
            Ok(())
        }
    }

    fn gzip(data: &[u8]) -> Vec<u8> {
        let mut w = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        std::io::Write::write_all(&mut w, data).unwrap();
        w.finish().unwrap()
    }

    #[test]
    fn compressed_line_formats_parse_in_blocks() {
        let mut nt = String::new();
        for i in 0..2000 {
            nt += &format!("<http://e/s{i}> <http://e/p> \"line {i}\\nmore\" .\n");
            nt += &format!("_:b{} <http://e/q> <http://e/o{i}> .\n", i % 7);
        }
        // a line longer than a block, and no line break at the end
        nt += &format!("<http://e/long> <http://e/p> \"{}\" .\n", "x".repeat(5000));
        nt += "<http://e/last> <http://e/p> <http://e/o> .";
        let mut want: Vec<String> = RdfParser::from_format(RdfFormat::NTriples)
            .for_slice(nt.as_bytes())
            .map(|q| q.unwrap().to_string())
            .collect();
        want.sort();
        assert_eq!(want.len(), 4002);
        let src = Source::from_bytes(gzip(nt.as_bytes()), RdfFormat::NTriples, None);
        for (block, chunk, threads) in [(1000, 300, 3), (1 << 20, 1 << 20, 4), (64, 64, 1)] {
            let (out, finished) = (Mutex::new(Vec::new()), Mutex::new(0));
            let parser = RdfParser::from_format(RdfFormat::NTriples);
            parse_stream(&src, parser, threads, block, chunk, || {
                Collect(&out, Vec::new(), &finished)
            })
            .unwrap();
            let mut got = out.into_inner();
            got.sort();
            assert_eq!(got, want, "block {block}");
            // one sink per parallel slot, kept across blocks
            assert!(*finished.lock() <= threads);
        }
        // the public entry point takes the same path, and reports parse errors
        let (out, finished) = (Mutex::new(Vec::new()), Mutex::new(0));
        parse_source(&src, 4, || Collect(&out, Vec::new(), &finished)).unwrap();
        assert_eq!(out.into_inner().len(), 4002);
        let bad = Source::from_bytes(
            gzip(b"<http://e/s> <http://e/p> .\n"),
            RdfFormat::NTriples,
            None,
        );
        let (out, finished) = (Mutex::new(Vec::new()), Mutex::new(0));
        let e = parse_source(&bad, 4, || Collect(&out, Vec::new(), &finished)).unwrap_err();
        assert!(e.to_string().contains("offset 0"), "{e}");
    }

    #[test]
    fn lenient_sources_take_invalid_iris() {
        let nt = "<http://e/s> <http://e/p> <http://e/a\u{fffd}b> .\n";
        for compressed in [false, true] {
            let data = if compressed {
                gzip(nt.as_bytes())
            } else {
                nt.as_bytes().to_vec()
            };
            let mut src = Source::from_bytes(data, RdfFormat::NTriples, None);
            let (out, finished) = (Mutex::new(Vec::new()), Mutex::new(0));
            assert!(parse_source(&src, 2, || Collect(&out, Vec::new(), &finished)).is_err());
            src.lenient = true;
            let (out, finished) = (Mutex::new(Vec::new()), Mutex::new(0));
            parse_source(&src, 2, || Collect(&out, Vec::new(), &finished)).unwrap();
            assert_eq!(
                out.into_inner(),
                ["<http://e/s> <http://e/p> <http://e/a\u{fffd}b>"]
            );
        }
    }
}
