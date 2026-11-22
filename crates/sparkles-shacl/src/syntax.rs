//! The syntaxes a shapes graph can be read from and written in: the RDF syntaxes, and
//! the SHACL Compact Syntax ([`crate::compact`]).

use crate::compact;
use anyhow::{Context as _, Result};
use oxrdf::{Graph, Triple};
use sparkles_core::codec::Codec;
use sparkles_core::io::{RdfFormat, Source};
use std::fmt;
use std::io::{self, Read};
use std::path::Path;
use std::time::Instant;

/// The syntax of a shapes document.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShapesSyntax {
    /// an RDF syntax (all graphs of a quad format are merged)
    Rdf(RdfFormat),
    /// the SHACL Compact Syntax, `text/shaclc`
    Compact,
}

impl From<RdfFormat> for ShapesSyntax {
    fn from(f: RdfFormat) -> ShapesSyntax {
        ShapesSyntax::Rdf(f)
    }
}

impl Default for ShapesSyntax {
    fn default() -> ShapesSyntax {
        ShapesSyntax::Rdf(RdfFormat::Turtle)
    }
}

impl fmt::Display for ShapesSyntax {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ShapesSyntax::Rdf(r) => write!(f, "{}", r.name()),
            ShapesSyntax::Compact => f.write_str("SHACLC"),
        }
    }
}

impl ShapesSyntax {
    /// The syntax of a media type (parameters ignored): `text/shaclc`, or an RDF syntax.
    pub fn from_media_type(mt: &str) -> Option<ShapesSyntax> {
        let base = mt.split(';').next()?.trim();
        if base.eq_ignore_ascii_case(compact::MEDIA_TYPE) {
            return Some(ShapesSyntax::Compact);
        }
        sparkles_core::io::format_for_media_type(base).map(ShapesSyntax::Rdf)
    }

    /// The syntax (and compression) of a file name: `.shaclc` and `.shc` are SHACLC,
    /// optionally compressed (`shapes.shaclc.gz`); other names as for RDF files.
    pub fn from_path(path: &Path) -> Option<(ShapesSyntax, Option<Codec>)> {
        let codec = Codec::from_extension(path);
        let name = path.file_name()?.to_str()?.to_ascii_lowercase();
        let ext = Codec::strip_extension(&name).rsplit('.').next()?;
        if compact::EXTENSIONS.contains(&ext) {
            return Some((ShapesSyntax::Compact, codec));
        }
        sparkles_core::io::format_for_path(path).map(|(f, c)| (ShapesSyntax::Rdf(f), c))
    }

    /// The syntax named by a short name, as in `--format` flags and MCP arguments:
    /// `shaclc` (or `compact`), or an RDF syntax name such as `turtle`, `ttl` or
    /// `jsonld`.
    pub fn from_name(name: &str) -> Option<ShapesSyntax> {
        let n = name.trim().to_ascii_lowercase();
        match n.as_str() {
            "shaclc" | "shc" | "compact" => Some(ShapesSyntax::Compact),
            _ => match sparkles_core::syntaxes::by_name(&n) {
                // shapes are not read from N3 or streaming JSON-LD
                Some(RdfFormat::N3) => None,
                Some(RdfFormat::JsonLd { .. }) => Some(ShapesSyntax::Rdf(RdfFormat::JsonLd {
                    profile: oxrdfio::JsonLdProfileSet::empty(),
                })),
                Some(f) => Some(ShapesSyntax::Rdf(f)),
                None => ShapesSyntax::from_media_type(&n),
            },
        }
    }

    /// The media type.
    pub fn media_type(self) -> &'static str {
        match self {
            ShapesSyntax::Rdf(f) => f.media_type(),
            ShapesSyntax::Compact => compact::MEDIA_TYPE,
        }
    }
}

/// A shapes document read into a graph, with the prefixes it declares.
pub struct ShapesDocument {
    pub graph: Graph,
    pub prefixes: Vec<(String, String)>,
}

/// Read a shapes document in any syntax into a graph (the blank nodes are fresh).
pub fn read_document(
    text: &str,
    syntax: ShapesSyntax,
    base: Option<&str>,
) -> Result<ShapesDocument> {
    read_document_until(text, syntax, base, None)
}

pub(crate) fn read_document_until(
    text: &str,
    syntax: ShapesSyntax,
    base: Option<&str>,
    deadline: Option<Instant>,
) -> Result<ShapesDocument> {
    crate::data::check_deadline(deadline)?;
    match syntax {
        ShapesSyntax::Compact => {
            let doc = compact::parse(text, base);
            crate::data::check_deadline(deadline)?;
            let doc = doc?;
            Ok(ShapesDocument {
                graph: doc.graph,
                prefixes: doc.prefixes,
            })
        }
        ShapesSyntax::Rdf(RdfFormat::Turtle) if deadline.is_some() => {
            read_turtle_until(text.as_bytes(), base, deadline)
        }
        ShapesSyntax::Rdf(format) => {
            let mut src = Source::from_bytes(text.as_bytes().to_vec(), format, None);
            src.base = base.map(str::to_string);
            src.name = "<shapes>".into();
            let (quads, prefixes) =
                sparkles_core::io::parse_to_vec(&src).context("parsing shapes graph")?;
            crate::data::check_deadline(deadline)?;
            let mut graph = Graph::new();
            for (i, q) in quads.into_iter().enumerate() {
                if i % 256 == 0 {
                    crate::data::check_deadline(deadline)?;
                }
                graph.insert(&Triple::new(q.subject, q.predicate, q.object));
            }
            Ok(ShapesDocument {
                graph,
                prefixes: prefixes.into_iter().collect(),
            })
        }
    }
}

fn read_turtle_until(
    input: impl Read,
    base: Option<&str>,
    deadline: Option<Instant>,
) -> Result<ShapesDocument> {
    struct CheckedRead<R> {
        input: R,
        deadline: Option<Instant>,
    }
    impl<R: Read> Read for CheckedRead<R> {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            // Limit each read so that a long comment or token polls before a
            // quad reaches the parser's sink.
            let cap = buf.len().min(16 * 1024);
            crate::data::check_deadline(self.deadline).map_err(io::Error::other)?;
            let n = self.input.read(&mut buf[..cap])?;
            crate::data::check_deadline(self.deadline).map_err(io::Error::other)?;
            Ok(n)
        }
    }

    let mut graph = Graph::new();
    let prefixes = sparkles_core::io::parse_rdf_reader(
        CheckedRead { input, deadline },
        RdfFormat::Turtle,
        base,
        "<shapes>",
        |q| {
            crate::data::check_deadline(deadline)?;
            graph.insert(&Triple::new(q.subject, q.predicate, q.object));
            Ok(())
        },
    )
    .context("parsing shapes graph")?;
    crate::data::check_deadline(deadline)?;
    Ok(ShapesDocument {
        graph,
        prefixes: prefixes.into_iter().collect(),
    })
}

#[cfg(test)]
#[test]
fn gate_turtle_polls_before_the_first_quad() {
    use sparkles_core::Error;
    // One line and no quad. Three setup/reader checks can pass before expiry.
    let text = format!("#{}\n<invalid", "x".repeat(1 << 20));
    crate::data::set_deadline_checks_left(Some(3));
    let result = read_document_until(
        &text,
        ShapesSyntax::default(),
        None,
        Some(Instant::now() + std::time::Duration::from_secs(30)),
    );
    crate::data::set_deadline_checks_left(None);
    assert!(matches!(
        result.err().and_then(|e| e.downcast::<Error>().ok()),
        Some(Error::Timeout)
    ));
}

#[cfg(test)]
#[test]
fn gate_turtle_preserves_a_read_error_at_expiry() {
    let input = |_: &mut [u8]| -> io::Result<usize> {
        crate::data::set_deadline_checks_left(Some(0));
        Err(io::Error::other("read failed"))
    };
    struct Reader<F>(F);
    impl<F: FnMut(&mut [u8]) -> io::Result<usize>> Read for Reader<F> {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            (self.0)(buf)
        }
    }
    let result = read_turtle_until(Reader(input), None, None);
    crate::data::set_deadline_checks_left(None);
    assert!(matches!(
        result.err().and_then(|e| e.downcast::<sparkles_core::Error>().ok()),
        Some(sparkles_core::Error::Io(e)) if e.to_string() == "read failed"
    ));
}

#[cfg(test)]
#[test]
fn gate_turtle_preserves_base_prefixes_and_blank_node_scope() {
    let text = b"@prefix ex: <urn:examples:> . _:shape ex:p <focus> . _:shape ex:q <target> .";
    let doc = read_turtle_until(text.as_slice(), Some("https://example.com/"), None).unwrap();
    assert_eq!(
        doc.prefixes,
        vec![("ex".to_owned(), "urn:examples:".to_owned())]
    );
    let mut rows = doc.graph.iter();
    let first = rows.next().unwrap();
    let second = rows.next().unwrap();
    assert_eq!(first.subject, second.subject);
    assert!(matches!(
        first.subject,
        oxrdf::NamedOrBlankNodeRef::BlankNode(_)
    ));
    let mut targets = [first.object.to_string(), second.object.to_string()];
    targets.sort();
    assert_eq!(
        targets,
        [
            "<https://example.com/focus>",
            "<https://example.com/target>"
        ]
    );
}

/// Write a shapes graph in Turtle with these prefixes (those it uses).
pub fn to_turtle(graph: &Graph, prefixes: &[(String, String)]) -> Result<String> {
    let mut ser = oxttl::TurtleSerializer::new();
    for (p, ns) in prefixes {
        if graph.iter().any(|t| {
            t.predicate.as_str().starts_with(ns.as_str())
                || matches!(t.subject, oxrdf::NamedOrBlankNodeRef::NamedNode(n) if n.as_str().starts_with(ns.as_str()))
                || matches!(t.object, oxrdf::TermRef::NamedNode(n) if n.as_str().starts_with(ns.as_str()))
                || matches!(t.object, oxrdf::TermRef::Literal(l) if l.datatype().as_str().starts_with(ns.as_str()))
        }) {
            ser = ser.with_prefix(p.as_str(), ns.as_str())?;
        }
    }
    let mut w = ser.for_writer(Vec::new());
    for t in graph.iter() {
        w.serialize_triple(t)?;
    }
    Ok(String::from_utf8(w.finish()?)?)
}

/// Convert a shapes document from one syntax to another. Writing SHACLC fails when
/// the graph has triples SHACLC cannot express (see [`compact::write`]).
pub fn convert(
    text: &str,
    from: ShapesSyntax,
    to: ShapesSyntax,
    base: Option<&str>,
) -> Result<String> {
    let doc = read_document(text, from, base)?;
    match to {
        ShapesSyntax::Compact => Ok(compact::write(&doc.graph, &doc.prefixes)?),
        ShapesSyntax::Rdf(RdfFormat::Turtle) => to_turtle(&doc.graph, &doc.prefixes),
        ShapesSyntax::Rdf(f) => {
            let mut w = oxrdfio::RdfSerializer::from_format(f).for_writer(Vec::new());
            for t in doc.graph.iter() {
                w.serialize_triple(t)?;
            }
            Ok(String::from_utf8(w.finish()?)?)
        }
    }
}
