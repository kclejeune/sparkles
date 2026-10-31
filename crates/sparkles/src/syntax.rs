//! The RDF syntaxes [`Dataset::dump`](crate::Dataset::dump) writes and
//! [`Dataset::load_file`](crate::Dataset::load_file) reads: the W3C syntaxes of
//! [`RdfFormat`](crate::io::RdfFormat), and Jena's TriX, RDF Thrift, RDF Protobuf and
//! RDF/JSON of [`JenaFormat`](crate::jena_formats::JenaFormat).

use crate::error::Result;
use crate::io::Source;
use oxrdf::NamedNode;
use std::path::Path;

pub use crate::io::RdfSyntax;

/// The load sources of files. Every syntax is read straight from the file, Jena's as
/// well as the W3C syntaxes, so no file is rewritten first.
pub(crate) fn file_sources(
    paths: impl IntoIterator<Item = impl AsRef<Path>>,
    graph: Option<NamedNode>,
) -> Result<Vec<Source>> {
    paths
        .into_iter()
        .map(|path| Source::from_path(path.as_ref(), graph.clone()))
        .collect()
}
