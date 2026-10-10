//! Conversion of an ingested document to the text of its rendition (spec C18 §7.1):
//! plain text and Markdown as they are, HTML to Markdown, PDF to Markdown through
//! pdf-inspector (§7.1.1), and the refusals of each.
//!
//! The text that comes out is already normalized as `register_source` normalizes it
//! (NFC, `\n` line ends), so that the page offsets recorded here are offsets in the
//! rendition itself.

use crate::mcp::memory::ingest::normalize;
use serde_json::{Value, json};
use std::time::Instant;

/// The default ceiling of an input document, in bytes (§7.1).
pub const MAX_INPUT_BYTES: usize = 10 << 20;
/// The default ceiling of the text of one source, in bytes (§7.1).
pub const MAX_TEXT_BYTES: usize = crate::mcp::memory::ingest::MAX_TEXT_BYTES;

/// What a document is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    Text,
    Markdown,
    Html,
    Pdf,
    Csv,
    Tsv,
}

impl Format {
    pub fn media_type(self) -> &'static str {
        match self {
            Format::Text => "text/plain",
            Format::Markdown => "text/markdown",
            Format::Html => "text/html",
            Format::Pdf => "application/pdf",
            Format::Csv => "text/csv",
            Format::Tsv => "text/tab-separated-values",
        }
    }

    /// The short name of the format, as results report it.
    pub fn name(self) -> &'static str {
        match self {
            Format::Text => "text",
            Format::Markdown => "markdown",
            Format::Html => "html",
            Format::Pdf => "pdf",
            Format::Csv => "csv",
            Format::Tsv => "tsv",
        }
    }

    /// A media type or a short name, such as `text/html` or `pdf`.
    pub fn parse(s: &str) -> Option<Format> {
        let base = s.split(';').next()?.trim().to_ascii_lowercase();
        Some(match base.as_str() {
            "text/plain" | "text" | "txt" => Format::Text,
            "text/markdown" | "text/x-markdown" | "markdown" | "md" => Format::Markdown,
            "text/html" | "application/xhtml+xml" | "html" | "htm" => Format::Html,
            "application/pdf" | "pdf" => Format::Pdf,
            "text/csv" | "application/csv" | "csv" => Format::Csv,
            "text/tab-separated-values" | "tsv" | "tab" => Format::Tsv,
            _ => return None,
        })
    }

    /// The format a file name's extension names.
    pub fn from_name(name: &str) -> Option<Format> {
        let ext = name.rsplit_once('.')?.1.to_ascii_lowercase();
        Some(match ext.as_str() {
            "txt" | "text" => Format::Text,
            "md" | "markdown" | "mdown" => Format::Markdown,
            "html" | "htm" | "xhtml" => Format::Html,
            "pdf" => Format::Pdf,
            "csv" => Format::Csv,
            "tsv" | "tab" => Format::Tsv,
            _ => return None,
        })
    }

    /// The format of `bytes` from their first bytes, when they say: a PDF header or an
    /// HTML start.
    pub fn sniff(bytes: &[u8]) -> Option<Format> {
        let head = &bytes[..bytes.len().min(1024)];
        if head.starts_with(b"%PDF-") {
            return Some(Format::Pdf);
        }
        let s = String::from_utf8_lossy(head)
            .trim_start()
            .to_ascii_lowercase();
        if s.starts_with("<!doctype html") || s.starts_with("<html") {
            return Some(Format::Html);
        }
        None
    }

    /// The format of a document: the declared one when it is known, else the file
    /// name's, else what the bytes show, else plain text. A declared type that is not
    /// one of the inputs is `unsupported-format`.
    pub fn detect(
        declared: Option<&str>,
        name: Option<&str>,
        bytes: &[u8],
    ) -> Result<Format, Refusal> {
        if let Some(d) = declared.filter(|d| !d.trim().is_empty()) {
            let base = d
                .split(';')
                .next()
                .unwrap_or("")
                .trim()
                .to_ascii_lowercase();
            // a generic type says nothing: look further
            if base != "application/octet-stream" {
                return Format::parse(d).ok_or_else(|| Refusal::unsupported(d));
            }
        }
        if let Some(f) = Format::sniff(bytes) {
            return Ok(f);
        }
        if let Some(f) = name.and_then(Format::from_name) {
            return Ok(f);
        }
        Ok(Format::Text)
    }
}

/// A page that conversion could not read, and why.
#[derive(Clone, Debug, PartialEq)]
pub struct PageReason {
    /// 1-based
    pub page: u32,
    pub reasons: Vec<String>,
}

/// Why a document was not converted.
#[derive(Clone, Debug, PartialEq)]
pub struct Refusal {
    /// `unsupported-format`, `too-large`, `not-utf8`, `empty`, `needs-ocr`,
    /// `conversion-failed`, `conversion-timeout` or `ocr-failed`
    pub code: &'static str,
    pub status: u16,
    pub message: String,
    /// the pages that need OCR, with pdf-inspector's reasons
    pub pages: Vec<PageReason>,
    /// fonts whose codes could not be mapped to text
    pub fonts: Vec<String>,
}

impl Refusal {
    pub fn new(code: &'static str, status: u16, message: impl Into<String>) -> Refusal {
        Refusal {
            code,
            status,
            message: message.into(),
            pages: Vec::new(),
            fonts: Vec::new(),
        }
    }

    pub fn unsupported(what: &str) -> Refusal {
        Refusal::new(
            "unsupported-format",
            415,
            format!(
                "{what} cannot be ingested: send plain text, Markdown, HTML, PDF, CSV or TSV (RDF goes through the upload)"
            ),
        )
    }

    pub fn json(&self) -> Value {
        let mut j = json!({ "code": self.code, "message": self.message });
        if !self.pages.is_empty() {
            j["pages"] = self
                .pages
                .iter()
                .map(|p| json!({ "page": p.page, "reasons": p.reasons }))
                .collect::<Vec<_>>()
                .into();
        }
        if !self.fonts.is_empty() {
            j["fonts"] = self.fonts.clone().into();
        }
        j
    }
}

/// A converted document.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Converted {
    /// the rendition's text, normalized
    pub text: String,
    /// the media type of the original document
    pub media_type: String,
    /// the document's own title (HTML `<title>`, PDF `/Title`)
    pub title: Option<String>,
    /// the code point offset at which each page starts (PDF)
    pub page_starts: Vec<usize>,
    /// pages read by OCR
    pub ocr_pages: Vec<u32>,
    /// pages left out under `allowPartial`
    pub omitted_pages: Vec<u32>,
    /// what the conversion noticed, such as broken font encodings kept under
    /// `allowPartial`
    pub notes: Vec<String>,
}

/// The page whose marker starts the last page start at or before `offset`.
pub fn page_of(text: &str, starts: &[usize], offset: usize) -> Option<u32> {
    let i = starts.partition_point(|s| *s <= offset);
    let start = *starts.get(i.checked_sub(1)?)?;
    let marker: String = text.chars().skip(start).take(24).collect();
    marker_page(&marker)
}

/// The page number of a `<!-- Page N -->` marker at the start of `s`.
pub fn marker_page(s: &str) -> Option<u32> {
    let rest = s.strip_prefix("<!-- Page ")?;
    let end = rest.find(" -->")?;
    rest[..end].parse().ok()
}

/// The offsets of the page markers of a PDF rendition: each `<!-- Page N -->` that
/// starts a line.
pub fn page_starts(text: &str) -> Vec<usize> {
    let mut out = Vec::new();
    let mut offset = 0usize;
    for line in text.split_inclusive('\n') {
        if marker_page(line.trim_end()).is_some() && line.trim_end().ends_with("-->") {
            out.push(offset);
        }
        offset += line.chars().count();
    }
    out
}

/// How to convert.
#[derive(Clone)]
pub struct Options {
    /// convert what can be read of a PDF that needs OCR, and record the rest
    pub allow_partial: bool,
    pub max_input: usize,
    pub max_text: usize,
    /// when the conversion must end
    pub deadline: Instant,
    /// the PDF workers and the OCR configuration of the server
    pub pdf: std::sync::Arc<super::PdfRuntime>,
}

/// Convert `bytes` of format `format`. CSV and TSV are not converted to text (§7.8)
/// and are refused here.
pub fn convert(bytes: &[u8], format: Format, o: &Options) -> Result<Converted, Refusal> {
    if bytes.len() > o.max_input {
        return Err(Refusal::new(
            "too-large",
            413,
            format!(
                "the document has {} bytes; ingestion takes at most {}",
                bytes.len(),
                o.max_input
            ),
        ));
    }
    let mut out = match format {
        Format::Text | Format::Markdown => {
            let text = utf8(bytes)?;
            Converted {
                text: normalize(text.strip_prefix('\u{feff}').unwrap_or(text)),
                media_type: format.media_type().into(),
                ..Default::default()
            }
        }
        Format::Html => {
            let page = super::html::to_markdown(utf8(bytes)?);
            Converted {
                text: normalize(&page.markdown),
                media_type: format.media_type().into(),
                title: page.title,
                ..Default::default()
            }
        }
        Format::Pdf => pdf(bytes, o)?,
        Format::Csv | Format::Tsv => {
            return Err(Refusal::new(
                "unsupported-format",
                415,
                "a table is mapped to triples, not converted to text",
            ));
        }
    };
    if out.text.trim().is_empty() {
        return Err(Refusal::new("empty", 422, "the document holds no text"));
    }
    if out.text.len() > o.max_text {
        return Err(Refusal::new(
            "too-large",
            413,
            format!(
                "the document's text has {} bytes; a source holds at most {}: split the document",
                out.text.len(),
                o.max_text
            ),
        ));
    }
    if format == Format::Pdf {
        out.page_starts = page_starts(&out.text);
    }
    Ok(out)
}

fn utf8(bytes: &[u8]) -> Result<&str, Refusal> {
    std::str::from_utf8(bytes).map_err(|e| {
        Refusal::new(
            "not-utf8",
            415,
            format!("the document is not UTF-8 text (byte {})", e.valid_up_to()),
        )
    })
}

#[cfg(not(feature = "pdf"))]
fn pdf(_bytes: &[u8], _o: &Options) -> Result<Converted, Refusal> {
    Err(Refusal::new(
        "unsupported-format",
        415,
        "this server was built without PDF conversion (the pdf feature)",
    ))
}

#[cfg(feature = "pdf")]
fn pdf(bytes: &[u8], o: &Options) -> Result<Converted, Refusal> {
    super::pdf::convert(bytes, o)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats() {
        assert_eq!(
            Format::detect(Some("text/html; charset=utf-8"), None, b""),
            Ok(Format::Html)
        );
        assert_eq!(
            Format::detect(None, Some("a.MD"), b"# x"),
            Ok(Format::Markdown)
        );
        assert_eq!(
            Format::detect(None, Some("x.bin"), b"%PDF-1.7\n"),
            Ok(Format::Pdf)
        );
        assert_eq!(
            Format::detect(
                Some("application/octet-stream"),
                None,
                b"<!DOCTYPE html><p>x"
            ),
            Ok(Format::Html)
        );
        assert_eq!(Format::detect(None, None, b"plain"), Ok(Format::Text));
        assert_eq!(
            Format::detect(Some("text/turtle"), None, b"")
                .unwrap_err()
                .code,
            "unsupported-format"
        );
    }

    #[test]
    fn page_markers() {
        let t = "<!-- Page 1 -->\n\nOne.\n\n<!-- Page 3 -->\n\nThree é.\n";
        let s = page_starts(t);
        assert_eq!(s, vec![0, 23]);
        assert_eq!(page_of(t, &s, 18), Some(1));
        assert_eq!(page_of(t, &s, 40), Some(3));
        assert_eq!(page_of("x", &[], 0), None);
    }
}
