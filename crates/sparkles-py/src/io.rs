//! RDF formats, inputs and outputs: `RdfFormat`, `parse`, `serialize`, and the sources
//! and sinks that `Dataset.load` and `Dataset.dump` share with them.

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
use std::collections::BTreeMap;
use std::hash::{Hash, Hasher};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};

// ----------------------------------------------------------------------- formats ----

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
    pub inner: RdfFormat,
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
        inner: RdfFormat::Turtle,
    };
    #[classattr]
    const N_TRIPLES: PyRdfFormat = PyRdfFormat {
        inner: RdfFormat::NTriples,
    };
    #[classattr]
    const N_QUADS: PyRdfFormat = PyRdfFormat {
        inner: RdfFormat::NQuads,
    };
    #[classattr]
    const TRIG: PyRdfFormat = PyRdfFormat {
        inner: RdfFormat::TriG,
    };
    #[classattr]
    const N3: PyRdfFormat = PyRdfFormat {
        inner: RdfFormat::N3,
    };
    #[classattr]
    const RDF_XML: PyRdfFormat = PyRdfFormat {
        inner: RdfFormat::RdfXml,
    };
    #[classattr]
    const JSON_LD: PyRdfFormat = PyRdfFormat { inner: JSON_LD };

    #[getter]
    fn name(&self) -> &'static str {
        self.inner.name()
    }

    #[getter]
    fn media_type(&self) -> &'static str {
        self.inner.media_type()
    }

    #[getter]
    fn file_extension(&self) -> &'static str {
        self.inner.file_extension()
    }

    #[getter]
    fn iri(&self) -> &'static str {
        self.inner.iri()
    }

    #[getter]
    fn supports_datasets(&self) -> bool {
        self.inner.supports_datasets()
    }

    /// The format of a file extension such as `ttl` or `.nq`, or `None`.
    #[staticmethod]
    fn from_extension(extension: &str) -> Option<PyRdfFormat> {
        let ext = extension.trim_start_matches('.');
        sparkles::io::format_for_path(Path::new(&format!("x.{ext}")))
            .map(|(inner, _)| PyRdfFormat { inner })
    }

    /// The format of a media type such as `text/turtle`, or `None`.
    #[staticmethod]
    fn from_media_type(media_type: &str) -> Option<PyRdfFormat> {
        sparkles::io::format_for_media_type(media_type).map(|inner| PyRdfFormat { inner })
    }

    fn __str__(&self) -> &'static str {
        self.inner.name()
    }

    fn __repr__(&self) -> String {
        let attr = match self.inner {
            RdfFormat::Turtle => "TURTLE",
            RdfFormat::NTriples => "N_TRIPLES",
            RdfFormat::NQuads => "N_QUADS",
            RdfFormat::TriG => "TRIG",
            RdfFormat::N3 => "N3",
            RdfFormat::RdfXml => "RDF_XML",
            _ => "JSON_LD",
        };
        format!("RdfFormat.{attr}")
    }
}

/// A `format` argument: an `RdfFormat`, or a name, extension or media type.
pub fn format_from_py(ob: Option<&Bound<'_, PyAny>>) -> PyResult<Option<RdfFormat>> {
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
        .or_else(|| PyRdfFormat::from_extension(s).map(|f| f.inner))
        .or_else(|| match s.to_ascii_lowercase().as_str() {
            "n-triples" => Some(RdfFormat::NTriples),
            "n-quads" => Some(RdfFormat::NQuads),
            "rdf/xml" | "rdf-xml" => Some(RdfFormat::RdfXml),
            "n3" => Some(RdfFormat::N3),
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
) -> PyResult<Source> {
    let format = format_from_py(format)?;
    let input = input.filter(|i| !i.is_none());
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
    Ok(src)
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
    match out {
        Output::Bytes => {
            let codec = codec.unwrap_or(Codec::None);
            let buf = py
                .detach(|| {
                    let mut buf = Vec::new();
                    write_through(&mut buf, codec, f).map(|_| buf)
                })
                .py(py)?;
            Ok(Some(PyBytes::new(py, &buf)))
        }
        Output::Path(p) => {
            let codec = codec
                .or_else(|| Codec::from_extension(&p))
                .unwrap_or(Codec::None);
            py.detach(|| {
                let file = std::fs::File::create(&p)?;
                write_through(file, codec, f)
            })
            .py(py)?;
            Ok(None)
        }
        Output::File(obj) => {
            let codec = codec.unwrap_or(Codec::None);
            py.detach(|| write_through(PyFileWriter(obj), codec, f))
                .py(py)?;
            Ok(None)
        }
    }
}

/// The format of an output path (`out.nq.zst` is N-Quads), if any.
pub fn format_of_output(out: &Output) -> Option<RdfFormat> {
    match out {
        Output::Path(p) => sparkles::io::format_for_path(p).map(|(f, _)| f),
        _ => None,
    }
}

/// Serialize quads: with their graphs in a quad format, as triples in a triple format.
/// With `strict`, a quad of a named graph in a triple format is an error.
pub fn serialize_quads(
    w: &mut dyn Write,
    format: RdfFormat,
    prefixes: BTreeMap<String, String>,
    quads: impl Iterator<Item = sparkles::Result<Quad>>,
    strict: bool,
) -> sparkles::Result<u64> {
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

// ----------------------------------------------------------------- parse/serialize ----

/// Parse RDF without a dataset: an iterator of `Quad` (triples are in the default
/// graph).
#[pyfunction]
#[pyo3(signature = (input = None, format = None, *, path = None, base_iri = None, lenient = false))]
pub fn parse(
    py: Python<'_>,
    input: Option<&Bound<'_, PyAny>>,
    format: Option<&Bound<'_, PyAny>>,
    path: Option<PathBuf>,
    base_iri: Option<String>,
    lenient: bool,
) -> PyResult<PyQuadIterator> {
    let src = source_from_py(py, input, format, path, base_iri, None, None, lenient)?;
    let (quads, _) = py.detach(|| sparkles::io::parse_to_vec(&src)).py(py)?;
    Ok(PyQuadIterator::from_vec(quads))
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
