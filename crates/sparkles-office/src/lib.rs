//! Office documents as Markdown and CSV (spec C18 §7.1).
//!
//! Word, PowerPoint and Excel files and the OpenDocument text, presentation and
//! spreadsheet formats are ZIP containers of XML parts. This crate reads the parts that
//! hold the text and writes Markdown with headings, lists, tables, speaker notes and a
//! `<!-- Page N -->` marker before each slide, or one CSV text per sheet of a
//! spreadsheet. It reads nothing else. Macros, external links, embedded objects, comments,
//! headers and footers are not read, and no part of a file is ever executed or fetched.
//!
//! The file comes from outside, so every size is bounded by [`Limits`]: the number of
//! entries in the container, the uncompressed size of a part and of all parts together
//! (a ZIP bomb stops at the limit, whatever its header says), the nodes of an XML tree,
//! the cells of a sheet and the text that comes out. One output budget, [`Limits::max_output`],
//! also bounds everything a conversion holds besides the input and its XML trees: Markdown
//! blocks and table slots, sheet rows, shared strings, the text of notes and the CSV of all
//! sheets together. The paths that can multiply the size of the input, which are repeated
//! and padded cells, table slots, shared strings and the spaces of `text:s`, charge it
//! before they allocate, and text that is copied once from the input is bounded by the
//! limits on the input. A small file therefore cannot ask for gigabytes. The tradeoff is
//! memory for safety. A conversion may hold up to `max_output` bytes, which is 128 MiB by
//! default, beside its input and XML trees. A conversion also stops at
//! [`Limits::deadline`]. A file
//! past a limit is refused with [`Error::TooLarge`]. A damaged file gives [`Error::Corrupt`] and never a panic, which
//! the tests and the cargo-fuzz targets of `fuzz/` check.

mod budget;
mod docx;
mod format;
mod md;
mod odf;
mod package;
mod pptx;
mod sheet;
mod xlsx;
mod xml;

pub use format::{Format, sniff};

/// The bounds of a conversion.
#[derive(Clone, Debug)]
pub struct Limits {
    /// entries in the container
    pub max_entries: usize,
    /// uncompressed bytes of one part
    pub max_part: usize,
    /// uncompressed bytes of all parts read
    pub max_total: usize,
    /// nodes of one XML tree
    pub max_nodes: usize,
    /// nesting depth of an XML tree
    pub max_depth: usize,
    /// bytes of the Markdown
    pub max_text: usize,
    /// bytes of one sheet's CSV
    pub max_csv: usize,
    /// cells of a spreadsheet, empty cells not counted
    pub max_cells: usize,
    /// embedded images to return; 0 returns none and leaves their alt text in the text
    pub max_images: usize,
    /// bytes of one returned image
    pub max_image_bytes: usize,
    /// bytes of converted output a conversion may hold: Markdown blocks and table cells,
    /// sheet rows, and the CSV of all sheets together
    pub max_output: usize,
    /// when to stop: a conversion that is still running at this time fails with
    /// [`Error::Timeout`]. It is checked while the XML is read and while the output is
    /// charged, so a thread that the caller has given up on ends soon after the deadline.
    pub deadline: Option<std::time::Instant>,
}

impl Default for Limits {
    fn default() -> Limits {
        Limits {
            max_entries: 20_000,
            max_part: 24 << 20,
            max_total: 96 << 20,
            max_nodes: 1_000_000,
            max_depth: 200,
            max_text: 2 << 20,
            max_csv: 10 << 20,
            max_cells: 2_000_000,
            max_images: 0,
            max_image_bytes: 5 << 20,
            max_output: 128 << 20,
            deadline: None,
        }
    }
}

/// Why a file was not converted.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    /// the file is not a valid container of its format
    Corrupt(String),
    /// the file is encrypted or protected with a password
    Encrypted,
    /// a limit of [`Limits`] was reached
    TooLarge(String),
    /// the file is an Office format that this crate does not read, such as the binary
    /// `.doc`, `.xls` and `.ppt` formats
    Unsupported(String),
    /// the conversion was still running at [`Limits::deadline`]
    Timeout,
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Corrupt(m) | Error::TooLarge(m) | Error::Unsupported(m) => f.write_str(m),
            Error::Encrypted => f.write_str("the document is encrypted with a password"),
            Error::Timeout => f.write_str("the conversion did not end before its deadline"),
        }
    }
}

impl std::error::Error for Error {}

/// A picture inside a document, in the order it appears. The Markdown holds a line
/// `<!-- image:K -->` where image K belongs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Image {
    /// `image/png`, `image/jpeg` or `image/webp`
    pub media_type: &'static str,
    pub bytes: Vec<u8>,
    /// the alternative text the document gives
    pub alt: Option<String>,
}

/// A text document: Markdown, and the pictures its placeholders name.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Document {
    pub markdown: String,
    /// the document's own title
    pub title: Option<String>,
    /// slides read, for a presentation
    pub slides: usize,
    pub images: Vec<Image>,
    /// what the conversion left out or could not read
    pub notes: Vec<String>,
}

/// One sheet of a spreadsheet as CSV.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Sheet {
    pub name: String,
    /// 1-based, among the workbook's sheets
    pub index: usize,
    /// RFC 4180 CSV with `\n` line ends, the first row being the sheet's first row
    pub csv: String,
    pub rows: usize,
    pub columns: usize,
}

/// A text document or a presentation as Markdown.
pub fn to_markdown(bytes: &[u8], format: Format, limits: &Limits) -> Result<Document, Error> {
    let mut pkg = package::Package::open(bytes, limits)?;
    match format {
        Format::Docx => docx::convert(&mut pkg, limits),
        Format::Pptx => pptx::convert(&mut pkg, limits),
        Format::Odt => odf::text(&mut pkg, limits),
        Format::Odp => odf::presentation(&mut pkg, limits),
        Format::Xlsx | Format::Ods => Err(Error::Unsupported(
            "a spreadsheet is mapped to triples sheet by sheet, not converted to text".into(),
        )),
    }
}

/// The visible, non-empty sheets of a spreadsheet as CSV.
pub fn sheets(bytes: &[u8], format: Format, limits: &Limits) -> Result<Vec<Sheet>, Error> {
    let mut pkg = package::Package::open(bytes, limits)?;
    match format {
        Format::Xlsx => xlsx::sheets(&mut pkg, limits),
        Format::Ods => odf::sheets(&mut pkg, limits),
        _ => Err(Error::Unsupported("the document has no sheets".into())),
    }
}

#[cfg(any(test, feature = "fixtures"))]
#[doc(hidden)]
pub mod fixtures;
#[cfg(test)]
mod tests;
