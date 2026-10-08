//! RDF formats, inputs and outputs: `RdfFormat`, `parse`, `serialize`, and the sources
//! and sinks that `Dataset.load` and `Dataset.dump` share with them.
//!
//! Besides oxrdfio's syntaxes, `RdfFormat` names Jena's TriX, RDF Thrift, RDF Protobuf
//! and RDF/JSON ([`sparkles::jena_formats`]). An input in one of them is read by the
//! engine's reader on a thread of its own as it streams: `parse` and file objects take
//! the N-Quads it writes through a pipe, and `Dataset.load` spools them to a temporary
//! file for the bulk loader. An output is written by the engine's writer.

use crate::errors::{EngineResult, invalid};
use crate::results::PyQuadIterator;
use crate::terms::{PyQuad, PyTriple};
use oxrdf::{GraphName, NamedNode, Quad, TripleRef};
use oxrdfio::{JsonLdProfileSet, RdfFormat, RdfSerializer};
use pyo3::exceptions::{PyTypeError, PyValueError};
use pyo3::prelude::*;
use pyo3::types::{PyBytes, PyString};
use sparkles::codec::Codec;
use sparkles::io::{Source, SourceData};
use sparkles::jena_formats::{JenaFormat, RdfWriter};
use std::collections::BTreeMap;
use std::hash::{Hash, Hasher};
use std::io::{BufWriter, Read, Write};
use std::path::{Path, PathBuf};

// ----------------------------------------------------------------------- formats ----

/// A syntax: one of oxrdfio's, or one of Jena's.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Fmt {
    Rdf(RdfFormat),
    Jena(JenaFormat),
}

impl Fmt {
    pub fn name(self) -> &'static str {
        match self {
            Fmt::Rdf(f) => f.name(),
            Fmt::Jena(j) => j.name(),
        }
    }

    pub fn supports_datasets(self) -> bool {
        match self {
            Fmt::Rdf(f) => f.supports_datasets(),
            Fmt::Jena(j) => j.quads(),
        }
    }

    /// The format of a file path, past a compression extension.
    pub fn of_path(p: &Path) -> Option<Fmt> {
        JenaFormat::from_path(p)
            .map(Fmt::Jena)
            .or_else(|| sparkles::io::format_for_path(p).map(|(f, _)| Fmt::Rdf(f)))
    }
}

/// The format where only oxrdfio's syntaxes are read (shapes, schemas).
pub fn rdf_only(f: Option<Fmt>, what: &str) -> PyResult<Option<RdfFormat>> {
    match f {
        None => Ok(None),
        Some(Fmt::Rdf(f)) => Ok(Some(f)),
        Some(Fmt::Jena(j)) => Err(PyValueError::new_err(format!(
            "{what} are not read in {}",
            j.name()
        ))),
    }
}

/// An RDF serialization format.
#[pyclass(
    frozen,
    eq,
    hash,
    module = "sparkles",
    name = "RdfFormat",
    skip_from_py_object
)]
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct PyRdfFormat {
    pub inner: Fmt,
}

impl Hash for PyRdfFormat {
    fn hash<H: Hasher>(&self, h: &mut H) {
        self.inner.name().hash(h)
    }
}

const JSON_LD: RdfFormat = RdfFormat::JsonLd {
    profile: JsonLdProfileSet::empty(),
};

#[pymethods]
impl PyRdfFormat {
    #[classattr]
    const TURTLE: PyRdfFormat = PyRdfFormat {
        inner: Fmt::Rdf(RdfFormat::Turtle),
    };
    #[classattr]
    const N_TRIPLES: PyRdfFormat = PyRdfFormat {
        inner: Fmt::Rdf(RdfFormat::NTriples),
    };
    #[classattr]
    const N_QUADS: PyRdfFormat = PyRdfFormat {
        inner: Fmt::Rdf(RdfFormat::NQuads),
    };
    #[classattr]
    const TRIG: PyRdfFormat = PyRdfFormat {
        inner: Fmt::Rdf(RdfFormat::TriG),
    };
    #[classattr]
    const N3: PyRdfFormat = PyRdfFormat {
        inner: Fmt::Rdf(RdfFormat::N3),
    };
    #[classattr]
    const RDF_XML: PyRdfFormat = PyRdfFormat {
        inner: Fmt::Rdf(RdfFormat::RdfXml),
    };
    #[classattr]
    const JSON_LD: PyRdfFormat = PyRdfFormat {
        inner: Fmt::Rdf(JSON_LD),
    };
    #[classattr]
    const TRIX: PyRdfFormat = PyRdfFormat {
        inner: Fmt::Jena(JenaFormat::TriX),
    };
    #[classattr]
    const RDF_THRIFT: PyRdfFormat = PyRdfFormat {
        inner: Fmt::Jena(JenaFormat::Thrift),
    };
    #[classattr]
    const RDF_PROTOBUF: PyRdfFormat = PyRdfFormat {
        inner: Fmt::Jena(JenaFormat::Protobuf),
    };
    #[classattr]
    const RDF_JSON: PyRdfFormat = PyRdfFormat {
        inner: Fmt::Jena(JenaFormat::RdfJson),
    };

    #[getter]
    fn name(&self) -> &'static str {
        self.inner.name()
    }

    #[getter]
    fn media_type(&self) -> &'static str {
        match self.inner {
            Fmt::Rdf(f) => f.media_type(),
            Fmt::Jena(j) => j.media_type(),
        }
    }

    #[getter]
    fn file_extension(&self) -> &'static str {
        match self.inner {
            Fmt::Rdf(f) => f.file_extension(),
            Fmt::Jena(j) => j.file_extension(),
        }
    }

    /// The IRI of the format's specification.
    #[getter]
    fn iri(&self) -> &'static str {
        match self.inner {
            Fmt::Rdf(f) => f.iri(),
            Fmt::Jena(JenaFormat::TriX) => "http://www.w3.org/2004/03/trix/",
            Fmt::Jena(JenaFormat::RdfJson) => "https://www.w3.org/TR/rdf-json/",
            Fmt::Jena(_) => "https://jena.apache.org/documentation/io/rdf-binary.html",
        }
    }

    #[getter]
    fn supports_datasets(&self) -> bool {
        self.inner.supports_datasets()
    }

    /// The format of a file extension such as `ttl` or `.nq`, or `None`.
    #[staticmethod]
    fn from_extension(extension: &str) -> Option<PyRdfFormat> {
        let ext = extension.trim_start_matches('.');
        Fmt::of_path(Path::new(&format!("x.{ext}"))).map(|inner| PyRdfFormat { inner })
    }

    /// The format of a media type such as `text/turtle`, or `None`.
    #[staticmethod]
    fn from_media_type(media_type: &str) -> Option<PyRdfFormat> {
        sparkles::io::format_for_media_type(media_type)
            .map(Fmt::Rdf)
            .or_else(|| JenaFormat::from_media_type(media_type).map(Fmt::Jena))
            .map(|inner| PyRdfFormat { inner })
    }

    fn __str__(&self) -> &'static str {
        self.inner.name()
    }

    fn __repr__(&self) -> String {
        let attr = match self.inner {
            Fmt::Rdf(RdfFormat::Turtle) => "TURTLE",
            Fmt::Rdf(RdfFormat::NTriples) => "N_TRIPLES",
            Fmt::Rdf(RdfFormat::NQuads) => "N_QUADS",
            Fmt::Rdf(RdfFormat::TriG) => "TRIG",
            Fmt::Rdf(RdfFormat::N3) => "N3",
            Fmt::Rdf(RdfFormat::RdfXml) => "RDF_XML",
            Fmt::Rdf(_) => "JSON_LD",
            Fmt::Jena(JenaFormat::TriX) => "TRIX",
            Fmt::Jena(JenaFormat::Thrift) => "RDF_THRIFT",
            Fmt::Jena(JenaFormat::Protobuf) => "RDF_PROTOBUF",
            Fmt::Jena(JenaFormat::RdfJson) => "RDF_JSON",
        };
        format!("RdfFormat.{attr}")
    }
}

/// A `format` argument: an `RdfFormat`, or a name, extension or media type.
pub fn format_from_py(ob: Option<&Bound<'_, PyAny>>) -> PyResult<Option<Fmt>> {
    let Some(ob) = ob.filter(|o| !o.is_none()) else {
        return Ok(None);
    };
    if let Ok(f) = ob.cast::<PyRdfFormat>() {
        return Ok(Some(f.get().inner));
    }
    let Ok(s) = ob.cast::<PyString>() else {
        return Err(PyTypeError::new_err("format must be an RdfFormat or a str"));
    };
    let s = s.to_str()?;
    let found = sparkles::sparql::results::rdf_format_from_name(s)
        .or_else(|| sparkles::io::format_for_media_type(s))
        .map(Fmt::Rdf)
        .or_else(|| JenaFormat::from_name(s).map(Fmt::Jena))
        .or_else(|| PyRdfFormat::from_extension(s).map(|f| f.inner))
        .or_else(|| match s.to_ascii_lowercase().as_str() {
            "n-triples" => Some(Fmt::Rdf(RdfFormat::NTriples)),
            "n-quads" => Some(Fmt::Rdf(RdfFormat::NQuads)),
            "rdf/xml" | "rdf-xml" => Some(Fmt::Rdf(RdfFormat::RdfXml)),
            "n3" => Some(Fmt::Rdf(RdfFormat::N3)),
            _ => None,
        });
    match found {
        Some(f) => Ok(Some(f)),
        None => Err(PyValueError::new_err(format!("unknown RDF format {s:?}"))),
    }
}

pub fn codec_from_py(py: Python<'_>, s: Option<&str>) -> PyResult<Option<Codec>> {
    s.map(|s| Codec::parse(s).py(py)).transpose()
}

// ------------------------------------------------------------------------ inputs ----

/// A source from `input` (str, bytes or a file object) or `path`.
#[allow(clippy::too_many_arguments)]
pub fn source_from_py(
    py: Python<'_>,
    input: Option<&Bound<'_, PyAny>>,
    format: Option<&Bound<'_, PyAny>>,
    path: Option<PathBuf>,
    base_iri: Option<String>,
    graph: Option<NamedNode>,
    compression: Option<&str>,
    lenient: bool,
) -> PyResult<(Source, Option<Spool>)> {
    let format = format_from_py(format)?;
    let input = input.filter(|i| !i.is_none());
    // a path in one of Jena's syntaxes, by its extension
    let format = format.or_else(|| {
        path.as_deref()
            .and_then(JenaFormat::from_path)
            .map(Fmt::Jena)
    });
    if let Some(Fmt::Jena(j)) = format {
        let (src, spool) = jena_source(py, j, input, path, base_iri, graph, compression, lenient)?;
        return Ok((src, Some(spool)));
    }
    let format = rdf_only(format, "datasets")?;
    let mut spool = None;
    let mut src = match (input, path) {
        (Some(_), Some(_)) => return Err(PyValueError::new_err("give input or path, not both")),
        (None, None) => return Err(PyValueError::new_err("give input or path")),
        (None, Some(p)) => {
            if !p.exists() {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    format!("No such file: {}", p.display()),
                )
                .into());
            }
            let mut src = match format {
                Some(f) => {
                    let mut s = Source::from_bytes(Vec::new(), f, graph.clone());
                    let abs = std::fs::canonicalize(&p).unwrap_or_else(|_| p.clone());
                    s.data = SourceData::File(p.clone());
                    s.base = Some(format!("file://{}", abs.display()));
                    s.name = p.display().to_string();
                    s
                }
                None => Source::from_path(&p, graph.clone()).py(py)?,
            };
            src.graph = graph;
            src
        }
        (Some(i), None) if is_file_object(i)? => {
            let Some(f) = format else {
                return Err(PyValueError::new_err(
                    "format is required when loading from input",
                ));
            };
            let (tmp, mut out) = Spool::create()?;
            let mut reader = PyFileReader::new(i.clone().unbind());
            py.detach(|| std::io::copy(&mut reader, &mut out))?;
            let mut src = Source::from_bytes(Vec::new(), f, graph);
            src.data = SourceData::File(tmp.0.clone());
            src.name = "<file object>".into();
            spool = Some(tmp);
            src
        }
        (Some(i), None) => {
            let Some(f) = format else {
                return Err(PyValueError::new_err(
                    "format is required when loading from input",
                ));
            };
            Source::from_bytes(input_bytes(i)?, f, graph)
        }
    };
    if base_iri.is_some() {
        src.base = base_iri;
    }
    src.compression = codec_from_py(py, compression)?;
    src.lenient = lenient;
    Ok((src, spool))
}

/// The decompressed bytes of an input in one of Jena's syntaxes, its name for errors, and
/// the base its path gives.
fn jena_raw(
    py: Python<'_>,
    input: Option<&Bound<'_, PyAny>>,
    path: Option<PathBuf>,
    compression: Option<&str>,
) -> PyResult<(Box<dyn Read + Send>, String, Option<String>)> {
    let explicit = codec_from_py(py, compression)?;
    Ok(match (input, path) {
        (Some(_), Some(_)) => return Err(PyValueError::new_err("give input or path, not both")),
        (None, None) => return Err(PyValueError::new_err("give input or path")),
        (None, Some(p)) => {
            if !p.exists() {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    format!("No such file: {}", p.display()),
                )
                .into());
            }
            let mut src = Source::from_bytes(Vec::new(), RdfFormat::NQuads, None);
            src.data = SourceData::File(p.clone());
            src.name = p.display().to_string();
            src.compression = explicit;
            let codec = src.codec().py(py)?;
            let f = std::fs::File::open(&p)?;
            let abs = std::fs::canonicalize(&p).unwrap_or_else(|_| p.clone());
            (
                codec
                    .reader_send(std::io::BufReader::with_capacity(1 << 16, f), None)
                    .py(py)?,
                p.display().to_string(),
                Some(format!("file://{}", abs.display())),
            )
        }
        (Some(i), None) if is_file_object(i)? => {
            let mut r = PyFileReader::new(i.clone().unbind());
            let name = "<file object>".to_string();
            let (codec, head) = py
                .detach(|| -> sparkles::Result<_> {
                    let (sniffed, head) = sparkles::io::sniff_codec(&mut r, None, &name)?;
                    let codec = match explicit {
                        Some(c) => Codec::detect(Some(c), &head, None)?.0,
                        None => sniffed,
                    };
                    Ok((codec, head))
                })
                .py(py)?;
            (
                codec
                    .reader_send(std::io::Cursor::new(head).chain(r), None)
                    .py(py)?,
                name,
                None,
            )
        }
        (Some(i), None) => {
            let bytes = input_bytes(i)?;
            let codec = explicit
                .or_else(|| Codec::sniff(&bytes))
                .unwrap_or(Codec::None);
            (
                codec
                    .reader_send(std::io::Cursor::new(bytes), None)
                    .py(py)?,
                "<input>".to_string(),
                None,
            )
        }
    })
}

/// N-Quads from a thread that reads an input in one of Jena's syntaxes with the
/// engine's reader, through a pipe, so the input is never held whole: TriX, RDF Thrift
/// and RDF Protobuf are read as they arrive (RDF/JSON's reader reads its document
/// first). The reader's error ends the stream as an I/O error.
struct Transcoded {
    pipe: std::io::PipeReader,
    worker: Option<std::thread::JoinHandle<Result<(), String>>>,
}

impl Transcoded {
    fn start(
        fmt: JenaFormat,
        raw: Box<dyn Read + Send>,
        base: Option<String>,
        name: String,
    ) -> std::io::Result<Transcoded> {
        let (pipe, w) = std::io::pipe()?;
        let worker = std::thread::spawn(move || -> Result<(), String> {
            let mut w = BufWriter::with_capacity(1 << 16, w);
            sparkles::jena_formats::transcode_with_base(fmt, raw, true, &mut w, base.as_deref())
                .map_err(|e| format!("{name}: {}: {e}", fmt.name()))?;
            w.flush().map_err(|e| e.to_string())
        });
        Ok(Transcoded {
            pipe,
            worker: Some(worker),
        })
    }
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
                // a syntax error, as the parsers report theirs
                Ok(Err(e)) => {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        sparkles::Error::RdfParse(e),
                    ));
                }
                Err(_) => return Err(std::io::Error::other("the reading thread panicked")),
            }
        }
        Ok(n)
    }
}

/// A temporary file that holds a transcoded input until the load that reads it ends;
/// it is removed when dropped.
pub struct Spool(PathBuf);

impl Spool {
    fn create() -> std::io::Result<(Spool, std::fs::File)> {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let dir = std::env::temp_dir();
        loop {
            let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let p = dir.join(format!("sparkles-load-{}-{n}.nq", std::process::id()));
            let mut options = std::fs::OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            match options.open(&p) {
                Ok(f) => return Ok((Spool(p), f)),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(e),
            }
        }
    }
}

impl Drop for Spool {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// A source of an input in one of Jena's syntaxes for the bulk loader: read by the
/// engine's reader as it streams and written as N-Quads to a temporary file in the
/// system's temporary directory, so neither the input nor its N-Quads are held in
/// memory. The source reads the file until the [`Spool`] is dropped.
#[allow(clippy::too_many_arguments)]
fn jena_source(
    py: Python<'_>,
    fmt: JenaFormat,
    input: Option<&Bound<'_, PyAny>>,
    path: Option<PathBuf>,
    base_iri: Option<String>,
    graph: Option<NamedNode>,
    compression: Option<&str>,
    lenient: bool,
) -> PyResult<(Source, Spool)> {
    let (raw, name, base) = jena_raw(py, input, path, compression)?;
    let base = base_iri.or(base);
    let (spool, file) = Spool::create()?;
    py.detach(|| -> Result<(), String> {
        let mut w = BufWriter::with_capacity(1 << 16, file);
        sparkles::jena_formats::transcode_with_base(fmt, raw, true, &mut w, base.as_deref())
            .map_err(|e| e.to_string())?;
        w.flush().map_err(|e| e.to_string())
    })
    .map_err(|e| invalid(py, format!("{name}: {}: {e}", fmt.name())))?;
    let mut src = Source::from_bytes(Vec::new(), RdfFormat::NQuads, graph);
    src.data = SourceData::File(spool.0.clone());
    src.compression = Some(Codec::None);
    src.name = name;
    src.lenient = lenient;
    Ok((src, spool))
}

/// Whether `input` is a file object rather than `str` or bytes.
pub fn is_file_object(input: &Bound<'_, PyAny>) -> PyResult<bool> {
    Ok(input.cast::<PyString>().is_err()
        && input.cast::<PyBytes>().is_err()
        && input.hasattr("read")?)
}

/// A Python file object as a `Read`: each read takes the GIL to call `read`. A text-mode
/// file's `str` is encoded as UTF-8.
pub struct PyFileReader {
    obj: Py<PyAny>,
    pending: Vec<u8>,
    pos: usize,
}

impl PyFileReader {
    pub fn new(obj: Py<PyAny>) -> Self {
        PyFileReader {
            obj,
            pending: Vec::new(),
            pos: 0,
        }
    }
}

impl Read for PyFileReader {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if self.pos >= self.pending.len() {
            let chunk = Python::attach(|py| -> PyResult<Vec<u8>> {
                let data = self
                    .obj
                    .bind(py)
                    .call_method1("read", (buf.len().max(1 << 16),))?;
                if let Ok(s) = data.cast::<PyString>() {
                    return Ok(s.to_str()?.as_bytes().to_vec());
                }
                if let Ok(b) = data.cast::<PyBytes>() {
                    return Ok(b.as_bytes().to_vec());
                }
                if data.is_none() {
                    return Ok(Vec::new());
                }
                data.extract::<Vec<u8>>()
            })
            .map_err(std::io::Error::other)?;
            self.pending = chunk;
            self.pos = 0;
            if self.pending.is_empty() {
                return Ok(0);
            }
        }
        let n = buf.len().min(self.pending.len() - self.pos);
        buf[..n].copy_from_slice(&self.pending[self.pos..self.pos + n]);
        self.pos += n;
        Ok(n)
    }
}

type StreamParser = oxrdfio::ReaderQuadParser<sparkles::nesting::Guarded<Box<dyn Read + Send>>>;

/// Quads parsed as they are read: from a file, bytes or a Python file object, through
/// its codec.
pub struct QuadStream {
    parser: StreamParser,
    name: String,
}

impl QuadStream {
    /// The prefixes the document has declared so far.
    pub fn prefixes(&self) -> BTreeMap<String, String> {
        self.parser
            .prefixes()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }
}

impl Iterator for QuadStream {
    type Item = sparkles::Result<Quad>;

    fn next(&mut self) -> Option<Self::Item> {
        let r = self.parser.next()?;
        Some(r.map_err(|e| match e {
            oxrdfio::RdfParseError::Io(io) => sparkles::codec::io_error(io),
            e => sparkles::Error::RdfParse(format!("{}: {e}", self.name)),
        }))
    }
}

/// A streaming parse of `input` (str, bytes or a file object) or `path`, with the
/// arguments of `parse` and `Dataset.load`.
#[allow(clippy::too_many_arguments)]
pub fn quad_stream(
    py: Python<'_>,
    input: Option<&Bound<'_, PyAny>>,
    format: Option<&Bound<'_, PyAny>>,
    path: Option<PathBuf>,
    base_iri: Option<String>,
    graph: Option<NamedNode>,
    compression: Option<&str>,
    lenient: bool,
) -> PyResult<QuadStream> {
    let input = input.filter(|i| !i.is_none());
    let explicit = codec_from_py(py, compression)?;
    let (reader, format, base, graph, name): (Box<dyn Read + Send>, _, _, _, String) = match input {
        Some(i) if path.is_none() && is_file_object(i)? => {
            let f = match format_from_py(format)? {
                None => {
                    return Err(PyValueError::new_err(
                        "format is required when reading from input",
                    ));
                }
                Some(Fmt::Jena(j)) => {
                    return jena_stream(
                        py,
                        j,
                        Some(i),
                        None,
                        base_iri,
                        graph,
                        compression,
                        lenient,
                    );
                }
                Some(Fmt::Rdf(f)) => f,
            };
            let mut r = PyFileReader::new(i.clone().unbind());
            let name = "<file object>".to_string();
            let (codec, head) = py
                .detach(|| -> sparkles::Result<_> {
                    let (sniffed, head) = sparkles::io::sniff_codec(&mut r, None, &name)?;
                    let codec = match explicit {
                        Some(c) => Codec::detect(Some(c), &head, None)?.0,
                        None => sniffed,
                    };
                    Ok((codec, head))
                })
                .py(py)?;
            let r = codec
                .reader_send(std::io::Cursor::new(head).chain(r), None)
                .py(py)?;
            (r, f, base_iri, graph, name)
        }
        _ => {
            // Jena's syntaxes, by name or by the path's extension, stream through the
            // engine's reader
            let jena = match format_from_py(format)? {
                Some(Fmt::Jena(j)) => Some(j),
                Some(Fmt::Rdf(_)) => None,
                None => path.as_deref().and_then(JenaFormat::from_path),
            };
            if let Some(j) = jena {
                return jena_stream(py, j, input, path, base_iri, graph, compression, lenient);
            }
            let (src, _) = source_from_py(
                py,
                input,
                format,
                path,
                base_iri,
                graph,
                compression,
                lenient,
            )?;
            let codec = src.codec().py(py)?;
            let raw: Box<dyn Read + Send> = match src.data {
                SourceData::File(p) => Box::new(std::fs::File::open(p)?),
                SourceData::Bytes(b) => Box::new(std::io::Cursor::new(b)),
            };
            let r = codec.reader_send(raw, None).py(py)?;
            (r, src.format, src.base, src.graph, src.name)
        }
    };
    stream_of(py, reader, format, base, graph, lenient, name)
}

/// A streaming parse of an input in one of Jena's syntaxes: the engine's reader runs on
/// its own thread and hands N-Quads to the parser through a pipe.
#[allow(clippy::too_many_arguments)]
fn jena_stream(
    py: Python<'_>,
    fmt: JenaFormat,
    input: Option<&Bound<'_, PyAny>>,
    path: Option<PathBuf>,
    base_iri: Option<String>,
    graph: Option<NamedNode>,
    compression: Option<&str>,
    lenient: bool,
) -> PyResult<QuadStream> {
    let (raw, name, base) = jena_raw(py, input, path, compression)?;
    let r = Transcoded::start(fmt, raw, base_iri.or(base), name.clone())?;
    stream_of(
        py,
        Box::new(r),
        RdfFormat::NQuads,
        None,
        graph,
        lenient,
        name,
    )
}

/// A streaming parse of `reader` in `format`.
fn stream_of(
    py: Python<'_>,
    reader: Box<dyn Read + Send>,
    format: RdfFormat,
    base: Option<String>,
    graph: Option<NamedNode>,
    lenient: bool,
    name: String,
) -> PyResult<QuadStream> {
    let mut parser = oxrdfio::RdfParser::from_format(format);
    if let Some(b) = base {
        parser = parser
            .with_base_iri(b)
            .map_err(|e| invalid(py, e.to_string()))?;
    }
    if let Some(g) = graph {
        parser = parser.with_default_graph(GraphName::NamedNode(g));
    }
    if lenient {
        parser = parser.lenient();
    }
    let guarded = sparkles::nesting::Guarded::rdf(reader, format, &name);
    Ok(QuadStream {
        parser: parser.for_reader(guarded),
        name,
    })
}

/// The bytes of a `str`, `bytes`-like or file object (read whole).
fn input_bytes(i: &Bound<'_, PyAny>) -> PyResult<Vec<u8>> {
    if let Ok(s) = i.cast::<PyString>() {
        return Ok(s.to_str()?.as_bytes().to_vec());
    }
    if let Ok(b) = i.cast::<PyBytes>() {
        return Ok(b.as_bytes().to_vec());
    }
    if i.hasattr("read")? {
        let data = i.call_method0("read")?;
        return input_bytes(&data);
    }
    if let Ok(v) = i.extract::<Vec<u8>>() {
        return Ok(v);
    }
    Err(PyTypeError::new_err(
        "input must be str, bytes or a file object with read()",
    ))
}

// ------------------------------------------------------------------------ tables ----

/// Whether a load reads a CSV or TSV table: `format` names one (`"csv"`, `"tsv"` or a
/// media type), or there is no format and the path's extension is `.csv`, `.tsv` or
/// `.tab` (before a compression extension).
pub fn table_kind(
    format: Option<&Bound<'_, PyAny>>,
    path: Option<&Path>,
) -> PyResult<Option<sparkles::tabular::TabularKind>> {
    use sparkles::tabular::TabularKind;
    match format.filter(|f| !f.is_none()) {
        Some(f) => {
            let Ok(s) = f.cast::<PyString>() else {
                return Ok(None);
            };
            let s = s.to_str()?;
            Ok(match s.to_ascii_lowercase().as_str() {
                "csv" => Some(TabularKind::Csv),
                "tsv" | "tab" => Some(TabularKind::Tsv),
                other => sparkles::tabular::tabular_media_type(other),
            })
        }
        None => Ok(path.and_then(sparkles::tabular::tabular_kind)),
    }
}

/// How a table maps to triples (spec C05).
pub struct TableArgs {
    pub kind: sparkles::tabular::TabularKind,
    /// a CSVW metadata file
    pub mapping: Option<PathBuf>,
    /// a SPARQL CONSTRUCT template run for each row
    pub template: Option<PathBuf>,
    /// the column that names each row's subject in the default mapping
    pub key: Option<String>,
    /// the default mapping's namespace
    pub base: Option<String>,
    pub compression: Option<Codec>,
}

/// Convert a table from `input` (str, bytes or a file object) or `path` into a
/// temporary N-Triples file, and hand it to `load` as a source into `graph`. Warnings of
/// the conversion become Python warnings.
pub fn load_table(
    py: Python<'_>,
    input: Option<&Bound<'_, PyAny>>,
    path: Option<&Path>,
    args: &TableArgs,
    graph: Option<NamedNode>,
    load: impl FnOnce(Source) -> sparkles::Result<u64> + Send,
) -> PyResult<u64> {
    use sparkles::tabular::{self, Mapping, Options};
    use std::sync::Arc;
    let input = input.filter(|i| !i.is_none());
    let metadata = args
        .mapping
        .as_deref()
        .map(|m| tabular::read_metadata(m).map(Arc::new))
        .transpose()
        .py(py)?;
    let mapping = match (&args.template, metadata) {
        (Some(t), metadata) => Mapping::Template {
            template: Arc::new(tabular::read_template(t).py(py)?),
            metadata,
        },
        (None, Some(m)) => Mapping::Csvw(m),
        (None, None) => {
            // a CSVW metadata file next to the table, as `sparkles load` finds it
            let beside = path.map(|p| {
                let mut s = p.as_os_str().to_owned();
                s.push("-metadata.json");
                PathBuf::from(s)
            });
            match beside.filter(|b| b.is_file() && args.key.is_none()) {
                Some(b) => Mapping::Csvw(Arc::new(tabular::read_metadata(&b).py(py)?)),
                None => Mapping::Default {
                    key: args.key.clone(),
                },
            }
        }
    };
    let (reader, mut opts): (Box<dyn Read + Send>, Options) = match (input, path) {
        (Some(_), Some(_)) => return Err(PyValueError::new_err("give input or path, not both")),
        (None, None) => return Err(PyValueError::new_err("give input or path")),
        (None, Some(p)) => {
            if !p.exists() {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    format!("No such file: {}", p.display()),
                )
                .into());
            }
            let f = std::fs::File::open(p)?;
            let mut src = Source::from_bytes(Vec::new(), RdfFormat::NTriples, None);
            src.data = SourceData::File(p.to_path_buf());
            src.name = p.display().to_string();
            src.compression = args.compression;
            let codec = src.codec().py(py)?;
            let r = codec
                .reader_send(std::io::BufReader::with_capacity(1 << 16, f), None)
                .py(py)?;
            (r, Options::for_file(mapping, p))
        }
        (Some(i), None) => {
            let (r, _, _) = jena_raw(py, Some(i), None, args.compression.map(Codec::name))?;
            (r, Options::new(mapping, "<input>"))
        }
    };
    opts.tsv = args.kind == tabular::TabularKind::Tsv;
    opts.base = args.base.clone();
    let name = opts.name.clone();
    let (n, warnings) = py
        .detach(|| -> sparkles::Result<_> {
            let (tmp, stats) = tabular::to_ntriples_file(reader, &opts, None)?;
            let n = load(tabular::source(&tmp, graph, &name))?;
            Ok((n, stats.warnings))
        })
        .py(py)?;
    for w in &warnings {
        PyErr::warn(
            py,
            &py.get_type::<pyo3::exceptions::PyUserWarning>(),
            &std::ffi::CString::new(format!("{}: {w}", opts.name)).unwrap_or_default(),
            1,
        )?;
    }
    Ok(n)
}

// ----------------------------------------------------------------------- outputs ----

/// Where `dump` and `serialize` write.
pub enum Output {
    /// returned as `bytes`
    Bytes,
    Path(PathBuf),
    /// a Python object with `write`
    File(Py<PyAny>),
}

pub fn output_from_py(ob: Option<&Bound<'_, PyAny>>) -> PyResult<Output> {
    let Some(ob) = ob.filter(|o| !o.is_none()) else {
        return Ok(Output::Bytes);
    };
    if ob.hasattr("write")? {
        return Ok(Output::File(ob.clone().unbind()));
    }
    ob.extract::<PathBuf>()
        .map(Output::Path)
        .map_err(|_| PyTypeError::new_err("output must be a path or a binary file object"))
}

/// A Python file object as a `Write`: each call takes the GIL to call `write`.
struct PyFileWriter(Py<PyAny>);

impl Write for PyFileWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        Python::attach(|py| {
            self.0
                .bind(py)
                .call_method1("write", (PyBytes::new(py, buf),))
                .map_err(std::io::Error::other)?;
            Ok(buf.len())
        })
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Run `f` on a buffered, compressed writer to `sink`, then finish the compression.
fn write_through<W: Write>(
    sink: W,
    codec: Codec,
    f: impl FnOnce(&mut dyn Write) -> sparkles::Result<u64>,
) -> sparkles::Result<u64> {
    let enc = codec.writer(sink, None, 1)?;
    let mut w = BufWriter::with_capacity(1 << 16, enc);
    let n = f(&mut w)?;
    let enc = w
        .into_inner()
        .map_err(|e| sparkles::Error::Io(e.into_error()))?;
    enc.finish()?;
    Ok(n)
}

/// Write to `out`, without the GIL unless `out` is a Python file object: `f` gets a
/// writer and returns a count. Returns the bytes for `Output::Bytes`.
pub fn write_output<'py>(
    py: Python<'py>,
    out: Output,
    codec: Option<Codec>,
    f: impl FnOnce(&mut dyn Write) -> sparkles::Result<u64> + Send,
) -> PyResult<Option<Bound<'py, PyBytes>>> {
    let buf = py.detach(|| write_to(out, codec, f)).py(py)?;
    Ok(buf.map(|buf| PyBytes::new(py, &buf)))
}

/// Write to `out` on the calling thread, which must not hold the GIL. A Python file
/// object takes the GIL for each write. Returns the bytes for `Output::Bytes`.
pub fn write_to(
    out: Output,
    codec: Option<Codec>,
    f: impl FnOnce(&mut dyn Write) -> sparkles::Result<u64>,
) -> sparkles::Result<Option<Vec<u8>>> {
    match out {
        Output::Bytes => {
            let mut buf = Vec::new();
            write_through(&mut buf, codec.unwrap_or(Codec::None), f)?;
            Ok(Some(buf))
        }
        Output::Path(p) => {
            let codec = codec
                .or_else(|| Codec::from_extension(&p))
                .unwrap_or(Codec::None);
            let file = std::fs::File::create(&p)?;
            write_through(file, codec, f)?;
            Ok(None)
        }
        Output::File(obj) => {
            write_through(PyFileWriter(obj), codec.unwrap_or(Codec::None), f)?;
            Ok(None)
        }
    }
}

/// The format of an output path (`out.nq.zst` is N-Quads), if any.
pub fn format_of_output(out: &Output) -> Option<Fmt> {
    match out {
        Output::Path(p) => Fmt::of_path(p),
        _ => None,
    }
}

/// Serialize quads: with their graphs in a quad format, as triples in a triple format.
/// With `strict`, a quad of a named graph in a triple format is an error.
pub fn serialize_quads(
    w: &mut dyn Write,
    format: Fmt,
    prefixes: BTreeMap<String, String>,
    quads: impl Iterator<Item = sparkles::Result<Quad>>,
    strict: bool,
) -> sparkles::Result<u64> {
    let format = match format {
        Fmt::Rdf(f) => f,
        Fmt::Jena(j) => return serialize_jena(w, j, quads, strict),
    };
    let ser = sparkles::io::with_prefixes(RdfSerializer::from_format(format), prefixes);
    let mut out = ser.for_writer(w);
    let quads_format = format.supports_datasets();
    let mut n = 0;
    for q in quads {
        let q = q?;
        if quads_format {
            out.serialize_quad(&q)?;
        } else {
            if strict && q.graph_name != GraphName::DefaultGraph {
                return Err(sparkles::Error::invalid(format!(
                    "{} has no named graphs: {q}",
                    format.name()
                )));
            }
            out.serialize_triple(TripleRef::new(&q.subject, &q.predicate, &q.object))?;
        }
        n += 1;
    }
    out.finish()?;
    Ok(n)
}

/// [`serialize_quads`] in one of Jena's syntaxes.
fn serialize_jena(
    w: &mut dyn Write,
    fmt: JenaFormat,
    quads: impl Iterator<Item = sparkles::Result<Quad>>,
    strict: bool,
) -> sparkles::Result<u64> {
    let mut out = RdfWriter::new(fmt, w);
    let mut n = 0;
    for q in quads {
        let q = q?;
        if fmt.quads() {
            out.quad(&q)?;
        } else {
            if strict && q.graph_name != GraphName::DefaultGraph {
                return Err(sparkles::Error::invalid(format!(
                    "{} has no named graphs: {q}",
                    fmt.name()
                )));
            }
            out.triple(&oxrdf::Triple::new(q.subject, q.predicate, q.object))?;
        }
        n += 1;
    }
    out.finish()?;
    Ok(n)
}

// ----------------------------------------------------------------- parse/serialize ----

/// Parse RDF without a dataset: an iterator of `Quad` (triples are in the default
/// graph).
#[pyfunction]
#[pyo3(signature = (input = None, format = None, *, path = None, base_iri = None, compression = None, lenient = false))]
pub fn parse(
    py: Python<'_>,
    input: Option<&Bound<'_, PyAny>>,
    format: Option<&Bound<'_, PyAny>>,
    path: Option<PathBuf>,
    base_iri: Option<String>,
    compression: Option<&str>,
    lenient: bool,
) -> PyResult<PyQuadIterator> {
    let stream = quad_stream(
        py,
        input,
        format,
        path,
        base_iri,
        None,
        compression,
        lenient,
    )?;
    Ok(PyQuadIterator::from_stream(Box::new(stream)))
}

/// Serialize triples or quads; returns bytes when `output` is `None`.
#[pyfunction]
#[pyo3(signature = (input, output = None, format = None, *, prefixes = None))]
pub fn serialize<'py>(
    py: Python<'py>,
    input: &Bound<'py, PyAny>,
    output: Option<&Bound<'py, PyAny>>,
    format: Option<&Bound<'py, PyAny>>,
    prefixes: Option<BTreeMap<String, String>>,
) -> PyResult<Option<Bound<'py, PyBytes>>> {
    let out = output_from_py(output)?;
    let format = match format_from_py(format)?.or_else(|| format_of_output(&out)) {
        Some(f) => f,
        None => {
            return Err(invalid(
                py,
                "give a format (or an output path with an extension)",
            ));
        }
    };
    // the input is Python objects: convert them all with the GIL held
    let mut quads = Vec::new();
    for item in input.try_iter()? {
        let item = item?;
        let q = if let Ok(q) = item.cast::<PyQuad>() {
            q.get().inner.clone()
        } else if let Ok(t) = item.cast::<PyTriple>() {
            t.get().inner.clone().in_graph(GraphName::DefaultGraph)
        } else {
            return Err(PyTypeError::new_err(
                "serialize takes Triple or Quad objects",
            ));
        };
        quads.push(q);
    }
    let prefixes = prefixes.unwrap_or_default();
    write_output(py, out, None, move |w| {
        serialize_quads(w, format, prefixes, quads.into_iter().map(Ok), true)
    })
}

pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<PyRdfFormat>()?;
    m.add_function(wrap_pyfunction!(parse, m)?)?;
    m.add_function(wrap_pyfunction!(serialize, m)?)?;
    Ok(())
}
