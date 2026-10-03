//! Request bodies: RDF for the Graph Store Protocol, and the parts of an upload.

use crate::error::{Error, Result};
use bytes::Bytes;
use oxrdf::{Quad, Triple};
use oxrdfio::RdfFormat;
use std::path::{Path, PathBuf};

/// Where a body's bytes come from. Files are opened again for each attempt, so a file
/// body can be retried.
#[derive(Clone, Debug)]
pub(crate) enum Source {
    Bytes(Bytes),
    File(PathBuf),
}

impl Source {
    pub(crate) async fn into_body(self) -> Result<reqwest::Body> {
        match self {
            Source::Bytes(b) => Ok(reqwest::Body::from(b)),
            Source::File(p) => {
                let f = tokio::fs::File::open(&p).await.map_err(|e| Error::Io {
                    path: p.display().to_string(),
                    source: e,
                })?;
                Ok(reqwest::Body::wrap_stream(
                    tokio_util::io::ReaderStream::new(f),
                ))
            }
        }
    }
}

/// An RDF document to send: its bytes or a file, its syntax, and a content coding when
/// the bytes are compressed.
#[derive(Clone, Debug)]
pub struct RdfBody {
    pub(crate) source: Source,
    pub(crate) format: RdfFormat,
    pub(crate) encoding: Option<&'static str>,
}

/// The content coding of a compression extension.
fn coding_of(ext: &str) -> Option<&'static str> {
    match ext.to_ascii_lowercase().as_str() {
        "gz" | "gzip" => Some("gzip"),
        "zst" | "zstd" => Some("zstd"),
        "br" => Some("br"),
        _ => None,
    }
}

/// The syntax and content coding of a file name such as `data.ttl.gz`.
pub(crate) fn format_of_path(path: &Path) -> Option<(RdfFormat, Option<&'static str>)> {
    let name = path.file_name()?.to_str()?;
    let mut parts = name.rsplit('.');
    let last = parts.next()?;
    match coding_of(last) {
        Some(c) => Some((RdfFormat::from_extension(parts.next()?)?, Some(c))),
        None => Some((RdfFormat::from_extension(last)?, None)),
    }
}

impl RdfBody {
    /// A document in memory.
    pub fn bytes(data: impl Into<Bytes>, format: RdfFormat) -> RdfBody {
        RdfBody {
            source: Source::Bytes(data.into()),
            format,
            encoding: None,
        }
    }

    /// A file, streamed. The syntax comes from the extension (`.ttl`, `.nt`, `.nq`,
    /// `.trig`, `.rdf`, `.jsonld`, …); a final `.gz`, `.zst` or `.br` sends the file as it
    /// is with the matching `Content-Encoding`, which Sparkles decodes.
    pub fn file(path: impl AsRef<Path>) -> Result<RdfBody> {
        let path = path.as_ref();
        let (format, encoding) = format_of_path(path).ok_or_else(|| {
            Error::config(format!(
                "{}: cannot tell the RDF syntax from the file name",
                path.display()
            ))
        })?;
        Ok(RdfBody {
            source: Source::File(path.to_path_buf()),
            format,
            encoding,
        })
    }

    /// A file in a given syntax, uncompressed.
    pub fn file_with_format(path: impl AsRef<Path>, format: RdfFormat) -> RdfBody {
        RdfBody {
            source: Source::File(path.as_ref().to_path_buf()),
            format,
            encoding: None,
        }
    }

    /// Triples, written as N-Triples.
    pub fn triples<'a>(triples: impl IntoIterator<Item = &'a Triple>) -> RdfBody {
        let mut s = String::new();
        for t in triples {
            s.push_str(&t.to_string());
            s.push_str(" .\n");
        }
        RdfBody::bytes(s, RdfFormat::NTriples)
    }

    /// Quads, written as N-Quads.
    pub fn quads<'a>(quads: impl IntoIterator<Item = &'a Quad>) -> RdfBody {
        let mut s = String::new();
        for q in quads {
            s.push_str(&q.to_string());
            s.push_str(" .\n");
        }
        RdfBody::bytes(s, RdfFormat::NQuads)
    }

    /// The media type the body is sent as.
    pub fn media_type(&self) -> &'static str {
        self.format.media_type()
    }
}

/// One file of [`Dataset::upload`](crate::Dataset::upload): RDF, or a CSV or TSV table.
/// The server reads the syntax from the file name.
#[derive(Clone, Debug)]
pub struct UploadPart {
    pub(crate) field: String,
    pub(crate) file_name: String,
    pub(crate) source: Source,
}

impl UploadPart {
    /// A local file, sent under its own name.
    pub fn file(path: impl AsRef<Path>) -> Result<UploadPart> {
        let path = path.as_ref();
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or_else(|| Error::config(format!("{}: no file name", path.display())))?;
        Ok(UploadPart {
            field: "file".into(),
            file_name: name.to_string(),
            source: Source::File(path.to_path_buf()),
        })
    }

    /// Bytes under a file name, such as `people.csv` or `data.ttl`.
    pub fn bytes(file_name: impl Into<String>, data: impl Into<Bytes>) -> UploadPart {
        UploadPart {
            field: "file".into(),
            file_name: file_name.into(),
            source: Source::Bytes(data.into()),
        }
    }

    /// A CSVW metadata document that maps every table of the upload.
    pub fn csvw_mapping(json: impl Into<Bytes>) -> UploadPart {
        UploadPart {
            field: "mapping".into(),
            file_name: "mapping.json".into(),
            source: Source::Bytes(json.into()),
        }
    }

    /// A SPARQL CONSTRUCT template that maps every table of the upload.
    pub fn template(construct: impl Into<String>) -> UploadPart {
        UploadPart {
            field: "template".into(),
            file_name: "template.rq".into(),
            source: Source::Bytes(Bytes::from(construct.into())),
        }
    }

    pub(crate) async fn into_part(self) -> Result<reqwest::multipart::Part> {
        let len = match &self.source {
            Source::Bytes(b) => Some(b.len() as u64),
            Source::File(p) => tokio::fs::metadata(p).await.ok().map(|m| m.len()),
        };
        let body = self.source.into_body().await?;
        let part = match len {
            Some(n) => reqwest::multipart::Part::stream_with_length(body, n),
            None => reqwest::multipart::Part::stream(body),
        };
        Ok(part.file_name(self.file_name))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_from_names() {
        assert_eq!(
            format_of_path(Path::new("a/data.ttl")),
            Some((RdfFormat::Turtle, None))
        );
        assert_eq!(
            format_of_path(Path::new("data.nq.gz")),
            Some((RdfFormat::NQuads, Some("gzip")))
        );
        assert_eq!(
            format_of_path(Path::new("x.nt.zst")),
            Some((RdfFormat::NTriples, Some("zstd")))
        );
        assert_eq!(format_of_path(Path::new("x.gz")), None);
        assert_eq!(format_of_path(Path::new("x.csv")), None);
    }

    #[test]
    fn terms_serialize_as_ntriples() {
        let t = Triple::new(
            oxrdf::NamedNode::new("http://e/s").unwrap(),
            oxrdf::NamedNode::new("http://e/p").unwrap(),
            oxrdf::Literal::new_language_tagged_literal("a \"b\"\n", "en").unwrap(),
        );
        let b = RdfBody::triples([&t]);
        let Source::Bytes(data) = b.source else {
            unreachable!()
        };
        assert_eq!(
            std::str::from_utf8(&data).unwrap(),
            "<http://e/s> <http://e/p> \"a \\\"b\\\"\\n\"@en .\n"
        );
    }
}
