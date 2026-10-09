//! The RDF syntaxes [`Dataset::dump`](crate::Dataset::dump) writes and
//! [`Dataset::load_file`](crate::Dataset::load_file) reads: the W3C syntaxes of
//! [`RdfFormat`], and Jena's TriX, RDF Thrift, RDF Protobuf and RDF/JSON of
//! [`JenaFormat`].

use crate::error::{Error, Result};
use crate::io::{RdfFormat, Source, SourceData};
use crate::jena_formats::JenaFormat;
use oxrdf::NamedNode;
use std::io::Write;
use std::path::Path;

/// A W3C syntax or one of Jena's. Both [`RdfFormat`] and [`JenaFormat`] convert into it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RdfSyntax {
    Rdf(RdfFormat),
    Jena(JenaFormat),
}

impl RdfSyntax {
    /// The syntax of a file path by its extension, looking past a compression extension
    /// (`data.trix.gz`, `data.ttl.zst`).
    pub fn from_path(path: &Path) -> Option<RdfSyntax> {
        JenaFormat::from_path(path)
            .map(RdfSyntax::Jena)
            .or_else(|| crate::io::format_for_path(path).map(|(f, _)| RdfSyntax::Rdf(f)))
    }

    pub fn name(self) -> &'static str {
        match self {
            RdfSyntax::Rdf(f) => f.name(),
            RdfSyntax::Jena(j) => j.name(),
        }
    }

    /// Whether the syntax holds named graphs.
    pub fn supports_datasets(self) -> bool {
        match self {
            RdfSyntax::Rdf(f) => f.supports_datasets(),
            RdfSyntax::Jena(j) => j.quads(),
        }
    }
}

impl From<RdfFormat> for RdfSyntax {
    fn from(f: RdfFormat) -> RdfSyntax {
        RdfSyntax::Rdf(f)
    }
}

impl From<JenaFormat> for RdfSyntax {
    fn from(j: JenaFormat) -> RdfSyntax {
        RdfSyntax::Jena(j)
    }
}

/// The load sources of files, and the temporary N-Quads files they read. The loader
/// reads the W3C syntaxes directly. A file in one of Jena's syntaxes is decompressed and
/// rewritten as N-Quads in a temporary file first, which must outlive the load.
pub(crate) fn file_sources(
    paths: impl IntoIterator<Item = impl AsRef<Path>>,
    graph: Option<NamedNode>,
) -> Result<(Vec<Source>, Vec<tempfile::TempPath>)> {
    let mut sources = Vec::new();
    let mut spooled = Vec::new();
    for path in paths {
        let path = path.as_ref();
        let Some(format) = JenaFormat::from_path(path) else {
            sources.push(Source::from_path(path, graph.clone())?);
            continue;
        };
        let abs = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        let base = format!("file://{}", abs.display());
        let input = crate::tabular::open(path, None)?;
        let spool = tempfile::NamedTempFile::new()?;
        let mut w = std::io::BufWriter::with_capacity(1 << 16, spool.as_file());
        crate::jena_formats::transcode_with_base(format, input, true, &mut w, Some(&base))
            .map_err(|e| Error::RdfParse(format!("{}: {}: {e}", path.display(), format.name())))?;
        w.flush()?;
        drop(w);
        let spool = spool.into_temp_path();
        let mut source = Source::from_bytes(Vec::new(), RdfFormat::NQuads, graph.clone());
        source.data = SourceData::File(spool.to_path_buf());
        source.compression = Some(crate::codec::Codec::None);
        source.base = Some(base);
        source.name = path.display().to_string();
        sources.push(source);
        spooled.push(spool);
    }
    Ok((sources, spooled))
}
