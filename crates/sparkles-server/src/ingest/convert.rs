//! Conversion of an ingested document to the text of its rendition (spec C18 §7.1):
//! plain text and Markdown as they are, HTML to Markdown, PDF to Markdown through
//! pdf-inspector (§7.1.1), Word, PowerPoint and OpenDocument files to Markdown through
//! sparkles-office, and the refusals of each.
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

/// The Office formats: short name, media type, format.
const OFFICE: [(&str, &str, Format); 6] = [
    (
        "docx",
        "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        Format::Docx,
    ),
    (
        "pptx",
        "application/vnd.openxmlformats-officedocument.presentationml.presentation",
        Format::Pptx,
    ),
    (
        "xlsx",
        "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        Format::Xlsx,
    ),
    (
        "odt",
        "application/vnd.oasis.opendocument.text",
        Format::Odt,
    ),
    (
        "odp",
        "application/vnd.oasis.opendocument.presentation",
        Format::Odp,
    ),
    (
        "ods",
        "application/vnd.oasis.opendocument.spreadsheet",
        Format::Ods,
    ),
];

/// What a document is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    Text,
    Markdown,
    Html,
    Pdf,
    Csv,
    Tsv,
    Docx,
    Pptx,
    Xlsx,
    Odt,
    Odp,
    Ods,
}

/// The first bytes of an OLE compound file.
const OLE_SIGNATURE: &[u8] = &[0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1];

impl Format {
    pub fn media_type(self) -> &'static str {
        match self {
            Format::Text => "text/plain",
            Format::Markdown => "text/markdown",
            Format::Html => "text/html",
            Format::Pdf => "application/pdf",
            Format::Csv => "text/csv",
            Format::Tsv => "text/tab-separated-values",
            Format::Docx => OFFICE[0].1,
            Format::Pptx => OFFICE[1].1,
            Format::Xlsx => OFFICE[2].1,
            Format::Odt => OFFICE[3].1,
            Format::Odp => OFFICE[4].1,
            Format::Ods => OFFICE[5].1,
        }
    }

    /// Whether the format is a spreadsheet, which is mapped sheet by sheet (§7.8).
    pub fn is_spreadsheet(self) -> bool {
        matches!(self, Format::Xlsx | Format::Ods)
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
            Format::Docx => "docx",
            Format::Pptx => "pptx",
            Format::Xlsx => "xlsx",
            Format::Odt => "odt",
            Format::Odp => "odp",
            Format::Ods => "ods",
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
            other => {
                return OFFICE
                    .iter()
                    .find(|(n, m, _)| *n == other || *m == other)
                    .map(|(_, _, f)| *f);
            }
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
            other => {
                return OFFICE
                    .iter()
                    .find(|(n, _, _)| *n == other)
                    .map(|(_, _, f)| *f);
            }
        })
    }

    /// The format of `bytes` from their first bytes, when they say: a PDF header or an
    /// HTML start.
    pub fn sniff(bytes: &[u8]) -> Option<Format> {
        let head = &bytes[..bytes.len().min(1024)];
        if head.starts_with(b"%PDF-") {
            return Some(Format::Pdf);
        }
        #[cfg(feature = "office")]
        if head.starts_with(b"PK\x03\x04") {
            return sparkles_office::sniff(bytes).and_then(|f| Format::parse(f.name()));
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
        // an OLE compound file is a legacy binary Office document or a password-protected
        // one, whatever its name says, and it is not text
        if bytes.starts_with(OLE_SIGNATURE) {
            return Err(Refusal::new(
                "unsupported-format",
                415,
                "the file is a legacy binary Office document or a password-protected one: save it as docx, pptx or xlsx without a password",
            ));
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
                "{what} cannot be ingested: send plain text, Markdown, HTML, PDF, Word, PowerPoint, Excel, OpenDocument, CSV or TSV (RDF goes through the upload)"
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

/// A picture inside a document that a `describe` model may read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EmbeddedImage {
    pub media_type: &'static str,
    pub bytes: Vec<u8>,
    pub alt: Option<String>,
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
    /// the pictures of an Office document, by the index of the `<!-- image:K -->` line
    /// that stands for each
    pub images: Vec<EmbeddedImage>,
    /// pages read by OCR
    pub ocr_pages: Vec<u32>,
    /// pages left out under `allowPartial`
    pub omitted_pages: Vec<u32>,
    /// what the conversion noticed, such as broken font encodings kept under
    /// `allowPartial`
    pub notes: Vec<String>,
}

/// The page of each page start of a rendition, read once. Reading a start's marker with
/// `chars().skip(start)` scans the text from its beginning, so a lookup for every fact or
/// page of a long PDF took time quadratic in the text.
pub struct PageMap<'a> {
    starts: &'a [usize],
    pages: Vec<Option<u32>>,
}

impl<'a> PageMap<'a> {
    /// The pages that start at `starts`, which are ascending code point offsets in `text`.
    pub fn new(text: &str, starts: &'a [usize]) -> PageMap<'a> {
        debug_assert!(starts.windows(2).all(|w| w[0] <= w[1]));
        let mut chars = text.char_indices();
        let mut at = 0;
        let pages = starts
            .iter()
            .map(|&start| {
                while at < start && chars.next().is_some() {
                    at += 1;
                }
                // a start past the end has no marker
                let (byte, _) = chars.clone().next()?;
                let marker = text[byte..]
                    .char_indices()
                    .nth(24)
                    .map_or(&text[byte..], |(end, _)| &text[byte..byte + end]);
                marker_page(marker)
            })
            .collect();
        PageMap { starts, pages }
    }

    /// The page whose marker starts the last page start at or before `offset`.
    pub fn page_of(&self, offset: usize) -> Option<u32> {
        let i = self.starts.partition_point(|s| *s <= offset);
        *self.pages.get(i.checked_sub(1)?)?
    }
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
// the deadline, the workers and the picture limit serve PDF and Office conversion only
#[cfg_attr(not(any(feature = "pdf", feature = "office")), allow(dead_code))]
pub struct Options {
    /// convert what can be read of a PDF that needs OCR, and record the rest
    #[cfg_attr(not(feature = "pdf"), allow(dead_code))]
    pub allow_partial: bool,
    pub max_input: usize,
    pub max_text: usize,
    /// when the conversion must end
    pub deadline: Instant,
    /// the PDF workers and the OCR configuration of the server
    pub pdf: std::sync::Arc<super::PdfRuntime>,
    /// how many pictures of an Office document to return for a `describe` model; 0 leaves
    /// their alt text in the text
    #[cfg_attr(not(feature = "office"), allow(dead_code))]
    pub max_images: usize,
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
        Format::Csv | Format::Tsv | Format::Xlsx | Format::Ods => {
            return Err(Refusal::new(
                "unsupported-format",
                415,
                "a table is mapped to triples, not converted to text",
            ));
        }
        Format::Docx | Format::Pptx | Format::Odt | Format::Odp => office(bytes, format, o)?,
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
    if matches!(format, Format::Pdf | Format::Pptx | Format::Odp) {
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

#[cfg(not(feature = "office"))]
fn office(_bytes: &[u8], _format: Format, _o: &Options) -> Result<Converted, Refusal> {
    Err(Refusal::new(
        "unsupported-format",
        415,
        "this server was built without Office conversion (the office feature)",
    ))
}

#[cfg(feature = "office")]
fn office(bytes: &[u8], format: Format, o: &Options) -> Result<Converted, Refusal> {
    super::office::convert(bytes, format, o)
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
        let map = PageMap::new(t, &s);
        assert_eq!(map.page_of(18), Some(1));
        assert_eq!(map.page_of(40), Some(3));
        assert_eq!(PageMap::new("x", &[]).page_of(0), None);
    }

    /// The function that `PageMap` replaced, as the oracle of its answers.
    fn page_of_by_scanning(text: &str, starts: &[usize], offset: usize) -> Option<u32> {
        let i = starts.partition_point(|s| *s <= offset);
        let start = *starts.get(i.checked_sub(1)?)?;
        let marker: String = text.chars().skip(start).take(24).collect();
        marker_page(&marker)
    }

    #[test]
    fn page_map_answers_like_a_scan_for_every_offset() {
        let mut t = String::new();
        for n in [1, 2, 10, 123456] {
            t.push_str(&format!(
                "<!-- Page {n} -->\n\nCaf\u{e9} \u{4e2d}\u{6587} \u{1f600} text.\n\n"
            ));
        }
        // a start whose marker is cut short by the end of the text, and one past it
        t.push_str("<!-- Page 9 -->");
        let mut starts = page_starts(&t);
        starts.push(t.chars().count() + 5);
        let map = PageMap::new(&t, &starts);
        for offset in 0..t.chars().count() + 9 {
            assert_eq!(
                map.page_of(offset),
                page_of_by_scanning(&t, &starts, offset),
                "{offset}"
            );
        }
        // starts that are not at a marker read none
        let odd = [0, 3, 3, 40];
        let map = PageMap::new(&t, &odd);
        for offset in 0..60 {
            assert_eq!(map.page_of(offset), page_of_by_scanning(&t, &odd, offset));
        }
    }

    /// Reading the pages of a long rendition takes time in proportion to its length. With
    /// one scan from the start of the text for each page, this text of 40,000 pages took
    /// about ten minutes (15 s for a fortieth of its pages).
    #[test]
    fn page_map_scales_with_the_length_of_the_text() {
        let mut t = String::new();
        for n in 1..=40_000 {
            t.push_str(&format!(
                "<!-- Page {n} -->\n\n{}\n\n",
                "lorem ipsum ".repeat(4)
            ));
        }
        let starts = page_starts(&t);
        assert_eq!(starts.len(), 40_000);
        let began = std::time::Instant::now();
        let map = PageMap::new(&t, &starts);
        let mut sum = 0u64;
        for &s in &starts {
            sum += u64::from(map.page_of(s).unwrap());
        }
        assert_eq!(sum, 40_000 * 40_001 / 2);
        assert!(
            began.elapsed() < std::time::Duration::from_secs(2),
            "{:?}",
            began.elapsed()
        );
    }
}
