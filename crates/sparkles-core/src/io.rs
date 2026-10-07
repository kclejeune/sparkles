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
use std::io::{BufReader, Cursor, Read};
use std::path::{Path, PathBuf};

/// Minimum bytes per chunk before splitting a document for parallel parsing.
const PARALLEL_MIN_CHUNK: usize = 8 << 20;

/// Input parsing policy. Streaming avoids a whole decompressed document buffer;
/// individual terms and syntax-specific parser state still occupy memory.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ParseMode {
    /// Keep the parallel mapped-file path for uncompressed line formats and
    /// Turtle. Compressed line formats use bounded parallel blocks. Other inputs
    /// buffer at most 128 MiB for Turtle/TriG or 8 MiB for other syntaxes
    /// before selecting a reader parser; sources can override this allowance.
    #[default]
    Auto,
    /// Parse directly from a reader, one quad at a time, in every RDF syntax.
    Streaming,
    /// Use the whole-document slice parser, decompressing into memory if needed.
    Buffered,
}

impl std::str::FromStr for ParseMode {
    type Err = Error;

    fn from_str(value: &str) -> Result<Self> {
        match value {
            "auto" => Ok(Self::Auto),
            "streaming" => Ok(Self::Streaming),
            "buffered" => Ok(Self::Buffered),
            _ => Err(Error::invalid(
                "parse mode must be auto, streaming or buffered",
            )),
        }
    }
}

/// Input buffering for reader parsing. The syntax parser retains its own token limits.
const INPUT_BUFFER_BYTES: usize = 128 << 10;
const AUTO_BUFFER_BYTES: usize = 8 << 20;
const TURTLE_AUTO_BUFFER_BYTES: usize = 128 << 20;

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
    /// How the input is parsed. Automatic selection preserves parallel parsing
    /// without allocating an arbitrarily large decompressed document.
    pub parse_mode: ParseMode,
    /// Automatic whole-document buffering allowance in decoded bytes. None selects
    /// 128 MiB for Turtle/TriG or 8 MiB for other syntaxes; zero skips probing.
    /// Applies to bulk parsing of compressed structured documents and plain
    /// non-splittable files. Mapped parallel input and compressed line blocks keep
    /// their existing paths. Transaction callbacks use a reader in automatic mode.
    /// This is per source, not a shared memory budget for concurrent loads.
    pub auto_buffer_bytes: Option<usize>,
    /// Fail past this many decompressed bytes (input bytes for plain data).
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
    fn buffer_limit(&self) -> usize {
        self.auto_buffer_bytes.unwrap_or(match self.format {
            RdfFormat::Turtle | RdfFormat::TriG => TURTLE_AUTO_BUFFER_BYTES,
            _ => AUTO_BUFFER_BYTES,
        })
    }

    pub fn from_path(path: &Path, graph: Option<NamedNode>) -> Result<Source> {
        let (format, _) = format_for_path(path).ok_or_else(|| {
            Error::invalid(format!("cannot determine RDF format of {}", path.display()))
        })?;
        let abs = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        Ok(Source {
            data: SourceData::File(path.to_path_buf()),
            format,
            compression: None,
            parse_mode: ParseMode::Auto,
            auto_buffer_bytes: None,
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
            parse_mode: ParseMode::Auto,
            auto_buffer_bytes: None,
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
        let mut prefix = [0u8; Codec::MAGIC_LEN];
        let (n, path) = match &self.data {
            SourceData::Bytes(b) => {
                let n = b.len().min(prefix.len());
                prefix[..n].copy_from_slice(&b[..n]);
                (n, None)
            }
            SourceData::File(p) => {
                let mut f = File::open(p)?;
                let mut n = 0;
                while n < prefix.len() {
                    match f.read(&mut prefix[n..]) {
                        Ok(0) => break,
                        Ok(k) => n += k,
                        Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                        Err(e) => return Err(e.into()),
                    }
                }
                (n, Some(p.as_path()))
            }
        };
        let (codec, warning) = Codec::detect(self.compression, &prefix[..n], path)?;
        if let Some(w) = warning {
            tracing::warn!(target: "sparkles::io", "{w}");
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
    let mut head = [0u8; Codec::MAGIC_LEN];
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
        tracing::warn!(target: "sparkles::io", "{w}");
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

/// Parse a media type into an RDF format. JSON-LD profile parameters are retained,
/// including `http://www.w3.org/ns/json-ld#streaming`.
pub fn format_for_media_type(mt: &str) -> Option<RdfFormat> {
    let base = mt.split(';').next()?.trim().to_ascii_lowercase();
    match base.as_str() {
        // Jena reads N3 as Turtle (`text/rdf+n3` is its N3 media type)
        "text/turtle" | "application/x-turtle" | "text/rdf+n3" | "text/n3" | "application/n3" => {
            Some(RdfFormat::Turtle)
        }
        "application/n-triples" | "text/plain" => Some(RdfFormat::NTriples),
        "application/n-quads" | "text/x-nquads" => Some(RdfFormat::NQuads),
        "application/trig" => Some(RdfFormat::TriG),
        "application/rdf+xml" | "application/xml" | "text/xml" => Some(RdfFormat::RdfXml),
        "application/ld+json" | "application/json" => {
            let mut profile = oxrdfio::JsonLdProfileSet::empty();
            for param in mt.split(';').skip(1) {
                if let Some((key, value)) = param.split_once('=')
                    && key.trim().eq_ignore_ascii_case("profile")
                {
                    for iri in value.trim().trim_matches('"').split_ascii_whitespace() {
                        if let Some(p) = oxrdfio::JsonLdProfile::from_iri(iri) {
                            profile |= p;
                        }
                    }
                }
            }
            Some(RdfFormat::JsonLd { profile })
        }
        _ => RdfFormat::from_media_type(&base),
    }
}

enum Loaded<'a> {
    Map(Mmap),
    Vec(Vec<u8>),
    Slice(&'a [u8]),
}
impl AsRef<[u8]> for Loaded<'_> {
    fn as_ref(&self) -> &[u8] {
        match self {
            Loaded::Map(m) => m,
            Loaded::Vec(v) => v,
            Loaded::Slice(s) => s,
        }
    }
}

fn load_bytes(src: &Source) -> Result<Loaded<'_>> {
    let codec = src.codec()?;
    if codec == Codec::None
        && let Some(limit) = src.max_decompressed
    {
        let requested = match &src.data {
            SourceData::Bytes(b) => b.len() as u64,
            SourceData::File(p) => std::fs::metadata(p)?.len(),
        };
        if requested > limit {
            return Err(Error::BudgetExceeded(crate::error::Budget {
                kind: crate::error::BudgetKind::DecompressedBytes,
                limit,
                requested,
            }));
        }
    }
    let inflate = |r: &mut dyn Read| -> Result<Loaded<'_>> {
        let mut out = Vec::new();
        codec
            .reader(r, src.max_decompressed)?
            .read_to_end(&mut out)
            .map_err(crate::codec::io_error)?;
        Ok(Loaded::Vec(out))
    };
    match &src.data {
        SourceData::Bytes(b) if codec == Codec::None => Ok(Loaded::Slice(b)),
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
/// Every syntax supports reader parsing. Automatic selection keeps the parallel
/// mapped-file path for uncompressed line formats and Turtle, and bounded blocks
/// for compressed line formats. Other compressed documents are buffered only up to
/// the source's buffering allowance, then parsed incrementally. Ordinary JSON-LD may retain an
/// object to resolve late contexts; its streaming profile avoids that reordering.
pub fn parse_source<S: QuadSink, F: Fn() -> S + Sync>(
    src: &Source,
    parallelism: usize,
    make_sink: F,
) -> Result<BTreeMap<String, String>> {
    let parser = source_parser(src)?;
    let codec = src.codec()?;
    let buffer_limit = src.buffer_limit();
    let line_based = matches!(src.format, RdfFormat::NTriples | RdfFormat::NQuads);
    if src.parse_mode == ParseMode::Streaming {
        let mut sink = make_sink();
        let prefixes = parse_reader(src, parser, source_reader(src, codec)?, |q| sink.quad(q))?;
        sink.finish()?;
        return Ok(prefixes);
    }
    if src.parse_mode == ParseMode::Auto && line_based && codec != Codec::None {
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
    let splittable = matches!(
        src.format,
        RdfFormat::NTriples | RdfFormat::NQuads | RdfFormat::Turtle
    );
    let mapped_size = match &src.data {
        SourceData::File(p) if codec == Codec::None => Some(std::fs::metadata(p)?.len()),
        _ => None,
    };
    let bytes = if src.parse_mode == ParseMode::Buffered
        || (codec == Codec::None && (splittable || matches!(src.data, SourceData::Bytes(_))))
        || mapped_size.is_some_and(|n| n < buffer_limit as u64)
    {
        load_bytes(src)?
    } else if codec == Codec::None || buffer_limit == 0 {
        let mut sink = make_sink();
        let prefixes = parse_reader(src, parser, source_reader(src, codec)?, |q| sink.quad(q))?;
        sink.finish()?;
        return Ok(prefixes);
    } else {
        // Probe the *decompressed* size, never an estimated compression ratio.
        // Replay the prefix into the same decoder when the input is large.
        let mut reader = source_reader(src, codec)?;
        let mut head = Vec::with_capacity(INPUT_BUFFER_BYTES.min(buffer_limit));
        let mut block = vec![0u8; INPUT_BUFFER_BYTES];
        while head.len() < buffer_limit {
            let remaining = (buffer_limit - head.len()).min(block.len());
            match reader.read(&mut block[..remaining]) {
                Ok(0) => break,
                Ok(n) => {
                    if head.len() + n > head.capacity() {
                        let capacity = head
                            .capacity()
                            .saturating_mul(2)
                            .max(head.len() + n)
                            .min(buffer_limit);
                        head.reserve_exact(capacity - head.len());
                    }
                    head.extend_from_slice(&block[..n]);
                }
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                Err(e) => return Err(crate::codec::io_error(e)),
            }
        }
        if head.len() == buffer_limit {
            let mut sink = make_sink();
            let prefixes = parse_reader(
                src,
                parser,
                PrefixReader {
                    prefix: Some(Cursor::new(head)),
                    reader,
                },
                |q| sink.quad(q),
            )?;
            sink.finish()?;
            return Ok(prefixes);
        }
        Loaded::Vec(head)
    };
    parse_slice(src, parser, bytes.as_ref(), parallelism, make_sink)
}

fn source_parser(src: &Source) -> Result<RdfParser> {
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
    Ok(parser)
}

fn source_reader(src: &Source, codec: Codec) -> Result<Box<dyn Read + '_>> {
    let raw: Box<dyn Read + '_> = match &src.data {
        SourceData::File(p) => Box::new(File::open(p)?),
        SourceData::Bytes(b) => Box::new(b.as_slice()),
    };
    // A smaller byte budget must still allow early quads to reach the sink.
    let capacity = src.max_decompressed.map_or(INPUT_BUFFER_BYTES, |limit| {
        limit.min(INPUT_BUFFER_BYTES as u64).max(1) as usize
    });
    Ok(Box::new(BufReader::with_capacity(
        capacity,
        codec.reader(raw, src.max_decompressed)?,
    )))
}

/// Release the decoded probe as soon as replay is complete, instead of retaining
/// a potentially 128 MiB prefix throughout the remaining parse.
struct PrefixReader<R> {
    prefix: Option<Cursor<Vec<u8>>>,
    reader: R,
}

impl<R: Read> Read for PrefixReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        if let Some(prefix) = &mut self.prefix {
            let n = prefix.read(buf)?;
            if prefix.position() == prefix.get_ref().len() as u64 {
                self.prefix = None;
            }
            if n > 0 {
                return Ok(n);
            }
        }
        self.reader.read(buf)
    }
}

/// Parse one source into a synchronous callback, without collecting its quads.
/// Useful for transactions whose mutable writer cannot be sent to parser workers.
/// Automatic mode uses the reader path here; explicit buffered mode is respected.
pub fn parse_source_into(
    src: &Source,
    sink: impl FnMut(Quad) -> Result<()>,
) -> Result<BTreeMap<String, String>> {
    if src.parse_mode == ParseMode::Buffered {
        let bytes = load_bytes(src)?;
        crate::nesting::check(src.format, bytes.as_ref(), &src.name)?;
        let mut parsed = source_parser(src)?.for_slice(bytes.as_ref());
        let mut sink = sink;
        for q in parsed.by_ref() {
            sink(q.map_err(|e| Error::RdfParse(format!("{}: {e}", src.name)))?)?;
        }
        return Ok(parsed
            .prefixes()
            .map(|(k, v)| (k.to_owned(), v.to_owned()))
            .collect());
    }
    parse_reader(
        src,
        source_parser(src)?,
        source_reader(src, src.codec()?)?,
        sink,
    )
}

fn parse_reader(
    src: &Source,
    parser: RdfParser,
    reader: impl Read,
    mut sink: impl FnMut(Quad) -> Result<()>,
) -> Result<BTreeMap<String, String>> {
    let mut parsed = parser.for_reader(crate::nesting::Guarded::rdf(reader, src.format, &src.name));
    for q in parsed.by_ref() {
        let q = q.map_err(|e| match e {
            oxrdfio::RdfParseError::Io(e) => crate::codec::io_error(e),
            e => Error::RdfParse(format!("{}: {e}", src.name)),
        })?;
        sink(q)?;
    }
    Ok(parsed
        .prefixes()
        .map(|(k, v)| (k.to_owned(), v.to_owned()))
        .collect())
}

fn parse_slice<S: QuadSink, F: Fn() -> S + Sync>(
    src: &Source,
    parser: RdfParser,
    slice: &[u8],
    parallelism: usize,
    make_sink: F,
) -> Result<BTreeMap<String, String>> {
    crate::nesting::check(src.format, slice, &src.name)?;
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
            // a block ends at a line break, and no triple term spans lines: its lines
            // are checked in parallel pieces
            line_pieces(&bytes, n)
                .into_par_iter()
                .try_for_each(|p| crate::nesting::check(src.format, p, &src.name))?;
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

/// `bytes` cut into about `n` pieces that end at a line break (the last at the end).
fn line_pieces(bytes: &[u8], n: usize) -> Vec<&[u8]> {
    let step = bytes.len().div_ceil(n.max(1)).max(1);
    let mut out = Vec::with_capacity(n);
    let mut start = 0;
    while start < bytes.len() {
        let end = match memchr::memchr(b'\n', &bytes[(start + step).min(bytes.len())..]) {
            Some(i) => (start + step).min(bytes.len()) + i + 1,
            None => bytes.len(),
        };
        out.push(&bytes[start..end]);
        start = end;
    }
    out
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
    // bytes of `buf` read; the rest is zeroed room for the next reads, zeroed once per
    // block (a decoder fills a few hundred KiB per call)
    let mut filled = 0;
    let mut target = block;
    let mut eof = false;
    loop {
        if buf.len() < target {
            buf.resize(target, 0);
        }
        while filled < target && !eof {
            match r.read(&mut buf[filled..target]) {
                Ok(0) => eof = true,
                Ok(k) => filled += k,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                Err(e) => return Err(crate::codec::io_error(e)),
            }
        }
        let cut = match memchr::memrchr(b'\n', &buf[..filled]) {
            _ if eof => filled,
            Some(i) => i + 1,
            // a line longer than a block: read on
            None => {
                target = filled + block;
                continue;
            }
        };
        let mut next = Vec::with_capacity(block.max(filled - cut));
        next.extend_from_slice(&buf[cut..filled]);
        filled -= cut;
        buf.truncate(cut);
        let len = buf.len() as u64;
        if len > 0
            && tx
                .send(Ok((offset, std::mem::replace(&mut buf, next))))
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

/// A syntax that [`check_data`] reads: one of oxrdfio's, or one of Jena's that
/// [`crate::jena_formats`] reads.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum DataSyntax {
    Rdf(RdfFormat),
    Jena(crate::jena_formats::JenaFormat),
}

impl DataSyntax {
    /// A syntax by Jena's name, in any case: `Turtle`, `TTL`, `N-Triples`, `NT`,
    /// `N-Quads`, `NQ`, `TriG`, `RDF/XML`, `JSON-LD`, `N3`, `RDF/JSON` or `TriX`.
    pub fn from_name(name: &str) -> Option<DataSyntax> {
        use crate::jena_formats::JenaFormat;
        let n = name.to_ascii_lowercase();
        Some(DataSyntax::Rdf(match n.as_str() {
            "turtle" | "ttl" => RdfFormat::Turtle,
            "n-triples" | "ntriples" | "n-triple" | "nt" => RdfFormat::NTriples,
            "n-quads" | "nquads" | "nq" => RdfFormat::NQuads,
            "trig" => RdfFormat::TriG,
            "rdf/xml" | "rdfxml" | "rdf" => RdfFormat::RdfXml,
            "json-ld" | "jsonld" => RdfFormat::JsonLd {
                profile: oxrdfio::JsonLdProfileSet::empty(),
            },
            "jsonld-streaming" | "json-ld-streaming" => RdfFormat::JsonLd {
                profile: oxrdfio::JsonLdProfile::Streaming.into(),
            },
            "n3" => RdfFormat::N3,
            "rdf/json" | "rdfjson" | "rj" => return Some(DataSyntax::Jena(JenaFormat::RdfJson)),
            "trix" => return Some(DataSyntax::Jena(JenaFormat::TriX)),
            _ => return None,
        }))
    }
}

/// A syntax error of RDF data, with its 1-based line and column when the parser gives
/// them.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
#[non_exhaustive]
pub struct DataIssue {
    pub message: String,
    pub line: Option<u64>,
    pub column: Option<u64>,
}

/// The first syntax error of `text` in `syntax`, with relative IRIs resolved against
/// `base`, or `None` when the data parses. Parsing stops at the first error.
pub fn check_data(syntax: DataSyntax, text: &str, base: Option<&str>) -> Option<DataIssue> {
    match syntax {
        DataSyntax::Rdf(f) => {
            let mut parser = RdfParser::from_format(f);
            if let Some(b) = base {
                parser = parser.with_base_iri(b).ok()?;
            }
            for q in parser.for_slice(text.as_bytes()) {
                if let Err(e) = q {
                    let at = e.location().map(|l| l.start);
                    return Some(DataIssue {
                        message: e.to_string(),
                        line: at.map(|p| p.line + 1),
                        column: at.map(|p| p.column + 1),
                    });
                }
            }
            None
        }
        DataSyntax::Jena(j) => {
            crate::jena_formats::transcode(j, text.as_bytes(), true, std::io::sink())
                .err()
                .map(|e| DataIssue {
                    message: e.to_string(),
                    line: None,
                    column: None,
                })
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn replay_releases_probe_and_preserves_empty_reads() {
        use super::PrefixReader;
        use std::io::{Cursor, Read};
        let mut reader = PrefixReader {
            prefix: Some(Cursor::new(b"head".to_vec())),
            reader: Cursor::new(b"tail"),
        };
        assert_eq!(reader.read(&mut []).unwrap(), 0);
        assert!(reader.prefix.is_some());
        let mut block = [0; 4];
        assert_eq!(reader.read(&mut block).unwrap(), 4);
        assert_eq!(&block, b"head");
        assert!(reader.prefix.is_none());
        assert_eq!(reader.read(&mut block).unwrap(), 4);
        assert_eq!(&block, b"tail");
        assert_eq!(reader.read(&mut block).unwrap(), 0);
    }

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
    fn line_pieces_cover_the_block_at_line_breaks() {
        let text = b"aa\nbbbb\n\nc\nddddddd\ne";
        for n in 1..12 {
            let pieces = line_pieces(text, n);
            assert_eq!(pieces.concat(), text, "{n}");
            for p in &pieces[..pieces.len() - 1] {
                assert_eq!(p.last(), Some(&b'\n'), "{n}");
            }
            assert!(pieces.len() <= n, "{n}");
        }
        assert!(line_pieces(b"", 4).is_empty());
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
