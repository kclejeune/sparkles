//! The formats this crate reads, and how to tell them apart.

/// An Office document format.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Format {
    Docx,
    Pptx,
    Xlsx,
    Odt,
    Odp,
    Ods,
}

impl Format {
    pub const ALL: [Format; 6] = [
        Format::Docx,
        Format::Pptx,
        Format::Xlsx,
        Format::Odt,
        Format::Odp,
        Format::Ods,
    ];

    pub fn media_type(self) -> &'static str {
        match self {
            Format::Docx => {
                "application/vnd.openxmlformats-officedocument.wordprocessingml.document"
            }
            Format::Pptx => {
                "application/vnd.openxmlformats-officedocument.presentationml.presentation"
            }
            Format::Xlsx => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
            Format::Odt => "application/vnd.oasis.opendocument.text",
            Format::Odp => "application/vnd.oasis.opendocument.presentation",
            Format::Ods => "application/vnd.oasis.opendocument.spreadsheet",
        }
    }

    /// The short name, which is also the file extension.
    pub fn name(self) -> &'static str {
        match self {
            Format::Docx => "docx",
            Format::Pptx => "pptx",
            Format::Xlsx => "xlsx",
            Format::Odt => "odt",
            Format::Odp => "odp",
            Format::Ods => "ods",
        }
    }

    /// Whether the format is a spreadsheet, which is mapped sheet by sheet.
    pub fn is_spreadsheet(self) -> bool {
        matches!(self, Format::Xlsx | Format::Ods)
    }

    /// A media type or a short name, such as `docx`.
    pub fn parse(s: &str) -> Option<Format> {
        let base = s.split(';').next()?.trim().to_ascii_lowercase();
        Format::ALL
            .into_iter()
            .find(|f| f.name() == base || f.media_type() == base)
    }

    /// The format a file name's extension names.
    pub fn from_name(name: &str) -> Option<Format> {
        let ext = name.rsplit_once('.')?.1.to_ascii_lowercase();
        Format::ALL.into_iter().find(|f| f.name() == ext)
    }
}

/// The format of a ZIP container from the names of its entries and, for OpenDocument, the
/// text of its `mimetype` entry. `None` for anything else, including a plain ZIP file.
pub fn sniff(bytes: &[u8]) -> Option<Format> {
    if !bytes.starts_with(b"PK\x03\x04") {
        return None;
    }
    let limits = crate::Limits::default();
    let mut pkg = crate::package::Package::open(bytes, &limits).ok()?;
    if let Some(m) = pkg.small_text("mimetype", 128) {
        return Format::parse(m.trim());
    }
    if pkg.has("word/document.xml") {
        Some(Format::Docx)
    } else if pkg.has("ppt/presentation.xml") {
        Some(Format::Pptx)
    } else if pkg.has("xl/workbook.xml") {
        Some(Format::Xlsx)
    } else {
        None
    }
}
