//! Amplification inputs: small files whose conversion would allocate far more than the
//! file's size. A counting allocator caps the process at `CAP` bytes, so a converter
//! that is not budgeted aborts here at once instead of exhausting the machine.
//! The allocator counts every thread, so each test takes a lock for its whole body,
//! building its input included, and the tests run one at a time.

use sparkles_office::{Error, Format, Limits, sheets, to_markdown};
use std::alloc::{GlobalAlloc, Layout, System};
use std::io::Write;
use std::sync::atomic::{AtomicUsize, Ordering};
use zip::CompressionMethod;
use zip::write::{SimpleFileOptions, ZipWriter};

const CAP: usize = 1 << 30;

struct Capped;
static LIVE: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);

unsafe impl GlobalAlloc for Capped {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        let now = LIVE.fetch_add(l.size(), Ordering::Relaxed) + l.size();
        if now > CAP {
            LIVE.fetch_sub(l.size(), Ordering::Relaxed);
            return std::ptr::null_mut();
        }
        PEAK.fetch_max(now, Ordering::Relaxed);
        unsafe { System.alloc(l) }
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        LIVE.fetch_sub(l.size(), Ordering::Relaxed);
        unsafe { System.dealloc(p, l) }
    }
}

#[global_allocator]
static A: Capped = Capped;

/// Held by a test from its first line, so that another test's input never counts here.
fn serial() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

/// Peak bytes allocated while `f` runs, above what was live before it.
fn peak<T>(f: impl FnOnce() -> T) -> (T, usize) {
    let base = LIVE.load(Ordering::Relaxed);
    PEAK.store(base, Ordering::Relaxed);
    let r = f();
    (r, PEAK.load(Ordering::Relaxed) - base)
}

fn zip(parts: &[(&str, &str)]) -> Vec<u8> {
    let mut z = ZipWriter::new(std::io::Cursor::new(Vec::new()));
    let o = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
    for (name, body) in parts {
        z.start_file(*name, o).unwrap();
        z.write_all(body.as_bytes()).unwrap();
    }
    z.finish().unwrap().into_inner()
}

const W: &str = r#"xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main""#;
const X: &str = r#"xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships""#;
const O: &str = r#"xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0" xmlns:text="urn:oasis:names:tc:opendocument:xmlns:text:1.0" xmlns:table="urn:oasis:names:tc:opendocument:xmlns:table:1.0""#;

/// Converted bytes stay within this many bytes of working memory for every case below.
const BOUND: usize = 256 << 20;

fn refused_or_ok<T>(r: &Result<T, Error>) -> bool {
    matches!(r, Ok(_) | Err(Error::TooLarge(_)))
}

// F1: a list level of 4e10 asked `Vec::resize` for 320 GB.
#[test]
fn f1_huge_list_level_in_a_paragraph() {
    let _g = serial();
    let doc = format!(
        r#"<w:document {W}><w:body><w:p><w:pPr><w:numPr><w:ilvl w:val="40000000000"/><w:numId w:val="1"/></w:numPr></w:pPr><w:r><w:t>deep</w:t></w:r></w:p></w:body></w:document>"#
    );
    let f = zip(&[("word/document.xml", &doc)]);
    let (r, used) = peak(|| to_markdown(&f, Format::Docx, &Limits::default()));
    assert!(used < BOUND, "peak {used} bytes");
    assert!(r.unwrap().markdown.contains("deep"));
}

#[test]
fn f1_huge_list_level_in_a_style() {
    let _g = serial();
    let doc = format!(
        r#"<w:document {W}><w:body><w:p><w:pPr><w:pStyle w:val="L"/></w:pPr><w:r><w:t>deep</w:t></w:r></w:p></w:body></w:document>"#
    );
    let styles = format!(
        r#"<w:styles {W}><w:style w:type="paragraph" w:styleId="L"><w:name w:val="x"/><w:pPr><w:numPr><w:ilvl w:val="40000000000"/><w:numId w:val="1"/></w:numPr></w:pPr></w:style></w:styles>"#
    );
    let f = zip(&[("word/document.xml", &doc), ("word/styles.xml", &styles)]);
    let (r, used) = peak(|| to_markdown(&f, Format::Docx, &Limits::default()));
    assert!(used < BOUND, "peak {used} bytes");
    assert!(r.unwrap().markdown.contains("deep"));
}

fn workbook(sheet_count: usize, shared: &str, sheet: &str) -> Vec<u8> {
    let mut wb = format!("<workbook {X}><sheets>");
    for i in 0..sheet_count {
        wb.push_str(&format!(
            r#"<sheet name="S{i}" sheetId="{}" r:id="rId1"/>"#,
            i + 1
        ));
    }
    wb.push_str("</sheets></workbook>");
    let rels = r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/></Relationships>"#;
    zip(&[
        ("xl/workbook.xml", &wb),
        ("xl/_rels/workbook.xml.rels", rels),
        ("xl/sharedStrings.xml", shared),
        ("xl/worksheets/sheet1.xml", sheet),
    ])
}

// F2, shared strings: one 1 MiB string named by 2,000 cells is 2 GiB of rows.
#[test]
fn f2_one_shared_string_in_many_cells() {
    let _g = serial();
    let shared = format!("<sst {X}><si><t>{}</t></si></sst>", "a".repeat(1 << 20));
    let mut sheet = format!("<worksheet {X}><sheetData>");
    for r in 1..=2000 {
        sheet.push_str(&format!(
            r#"<row r="{r}"><c r="A{r}" t="s"><v>0</v></c></row>"#
        ));
    }
    sheet.push_str("</sheetData></worksheet>");
    let f = workbook(1, &shared, &sheet);
    assert!(f.len() < 64 << 10, "the input is {} bytes", f.len());
    let (r, used) = peak(|| sheets(&f, Format::Xlsx, &Limits::default()));
    assert!(used < BOUND, "peak {used} bytes");
    assert!(matches!(r, Err(Error::TooLarge(_))), "{r:?}");
}

// F2, sparse rows: a row whose one cell is in column XFD is a 16,384-slot vector.
#[test]
fn f2_sparse_rows() {
    let _g = serial();
    let mut sheet = format!("<worksheet {X}><sheetData>");
    for r in 1..=20_000 {
        sheet.push_str(&format!(r#"<row r="{r}"><c r="XFD{r}"><v>1</v></c></row>"#));
    }
    sheet.push_str("</sheetData></worksheet>");
    let f = workbook(1, &format!("<sst {X}/>"), &sheet);
    let (r, used) = peak(|| sheets(&f, Format::Xlsx, &Limits::default()));
    assert!(used < BOUND, "peak {used} bytes");
    assert!(refused_or_ok(&r), "{r:?}");
}

// Missed by the review: each sheet stays under `max_csv`, but all of them together do
// not, and a workbook may name one small sheet part many times.
#[test]
fn many_sheets_each_under_the_csv_limit() {
    let _g = serial();
    let shared = format!("<sst {X}><si><t>{}</t></si></sst>", "a".repeat(1 << 20));
    let mut sheet = format!("<worksheet {X}><sheetData>");
    for r in 1..=9 {
        sheet.push_str(&format!(
            r#"<row r="{r}"><c r="A{r}" t="s"><v>0</v></c></row>"#
        ));
    }
    sheet.push_str("</sheetData></worksheet>");
    let f = workbook(200, &shared, &sheet);
    let (r, used) = peak(|| sheets(&f, Format::Xlsx, &Limits::default()));
    assert!(used < BOUND, "peak {used} bytes");
    assert!(matches!(r, Err(Error::TooLarge(_))), "{r:?}");
}

fn ods(table: &str) -> Vec<u8> {
    let c = format!(
        r#"<office:document-content {O}><office:body><office:spreadsheet>{table}</office:spreadsheet></office:body></office:document-content>"#
    );
    zip(&[
        ("mimetype", "application/vnd.oasis.opendocument.spreadsheet"),
        ("content.xml", &c),
    ])
}

// F3 A: an empty cell repeated 16,000 times puts the next value at column 16,000, and the
// row repeated 1,000 times clones that 390 KB vector.
#[test]
fn f3_repeated_wide_rows() {
    let _g = serial();
    let row = r#"<table:table-row table:number-rows-repeated="1000"><table:table-cell table:number-columns-repeated="16000"/><table:table-cell office:value-type="float" office:value="1"/></table:table-row>"#;
    let f = ods(&format!(
        r#"<table:table table:name="T">{}</table:table>"#,
        row.repeat(20)
    ));
    let (r, used) = peak(|| sheets(&f, Format::Ods, &Limits::default()));
    assert!(used < BOUND, "peak {used} bytes");
    assert!(refused_or_ok(&r), "{r:?}");
}

// F3 B: one 1 MiB value repeated over 1,000 columns and 1,000 rows is 10^6 cells of 1 MiB.
#[test]
fn f3_repeated_large_value() {
    let _g = serial();
    let row = format!(
        r#"<table:table-row table:number-rows-repeated="1000"><table:table-cell table:number-columns-repeated="1000" office:value-type="string"><text:p>{}</text:p></table:table-cell></table:table-row>"#,
        "a".repeat(1 << 20)
    );
    let f = ods(&format!(
        r#"<table:table table:name="T">{row}</table:table>"#
    ));
    let (r, used) = peak(|| sheets(&f, Format::Ods, &Limits::default()));
    assert!(used < BOUND, "peak {used} bytes");
    assert!(matches!(r, Err(Error::TooLarge(_))), "{r:?}");
}

// Missed by the review: a Markdown table pads every row to the widest one, so one wide row
// (gridSpan 64 on 2,000 cells) and 20,000 narrow rows render as gigabytes before the text
// limit is checked once, after the whole table.
#[test]
fn a_wide_table_with_many_rows() {
    let _g = serial();
    let mut t = String::from("<w:tbl><w:tr>");
    for _ in 0..2000 {
        t.push_str(r#"<w:tc><w:tcPr><w:gridSpan w:val="64"/></w:tcPr><w:p/></w:tc>"#);
    }
    t.push_str("</w:tr>");
    for _ in 0..20_000 {
        t.push_str("<w:tr><w:tc><w:p><w:r><w:t>x</w:t></w:r></w:p></w:tc></w:tr>");
    }
    t.push_str("</w:tbl>");
    let doc = format!(r#"<w:document {W}><w:body>{t}</w:body></w:document>"#);
    let f = zip(&[("word/document.xml", &doc)]);
    let (r, used) = peak(|| to_markdown(&f, Format::Docx, &Limits::default()));
    assert!(used < BOUND, "peak {used} bytes");
    assert!(matches!(r, Err(Error::TooLarge(_))), "{r:?}");
}

// F8: a real deflate bomb. 100 MB of XML compresses to about 100 KB and stops at `max_part`
// while it inflates, whatever the entry's header says.
#[test]
fn a_deflate_bomb_stops_at_the_part_limit() {
    let _g = serial();
    let doc = format!(
        "<w:document {W}><w:body><w:p><w:r><w:t>{}</w:t></w:r></w:p></w:body></w:document>",
        "a".repeat(100 << 20)
    );
    let f = zip(&[("word/document.xml", &doc)]);
    drop(doc);
    assert!(f.len() < 1 << 20, "the input is {} bytes", f.len());
    let (r, used) = peak(|| to_markdown(&f, Format::Docx, &Limits::default()));
    assert!(used < 64 << 20, "peak {used} bytes");
    assert!(matches!(r, Err(Error::TooLarge(_))), "{r:?}");
}

const P: &str = r#"xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main" xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships""#;

/// `inner` inside `depth` levels of `open` and `close`.
fn nest(open: &str, close: &str, depth: usize, inner: &str) -> String {
    format!("{}{inner}{}", open.repeat(depth), close.repeat(depth))
}

// Re-review N2: text collected with `descendants` where an element contains another of its
// kind was copied once for every level. A 2 MiB paragraph under 190 levels is 380 MiB.
const LEAF: usize = 2 << 20;

#[test]
fn n2_nested_paragraphs_in_footnotes() {
    let _g = serial();
    let leaf = format!("<w:r><w:t>{}</w:t></w:r>", "a".repeat(LEAF));
    let foot = format!(
        r#"<w:footnotes {W}><w:footnote w:id="2">{}</w:footnote></w:footnotes>"#,
        nest("<w:p>", "</w:p>", 190, &leaf)
    );
    let doc = format!(
        r#"<w:document {W}><w:body><w:p><w:r><w:t>x</w:t></w:r><w:r><w:footnoteReference w:id="2"/></w:r></w:p></w:body></w:document>"#
    );
    let f = zip(&[("word/document.xml", &doc), ("word/footnotes.xml", &foot)]);
    let (r, used) = peak(|| to_markdown(&f, Format::Docx, &Limits::default()));
    assert!(used < BOUND, "peak {used} bytes");
    assert!(refused_or_ok(&r), "{r:?}");
}

#[test]
fn n2_nested_text_boxes() {
    let _g = serial();
    let leaf = format!("<w:p><w:r><w:t>{}</w:t></w:r></w:p>", "a".repeat(LEAF));
    let doc = format!(
        r#"<w:document {W}><w:body><w:p><w:r><w:drawing>{}</w:drawing></w:r></w:p></w:body></w:document>"#,
        nest("<w:txbxContent>", "</w:txbxContent>", 190, &leaf)
    );
    let f = zip(&[("word/document.xml", &doc)]);
    let (r, used) = peak(|| to_markdown(&f, Format::Docx, &Limits::default()));
    assert!(used < BOUND, "peak {used} bytes");
    assert!(refused_or_ok(&r), "{r:?}");
}

#[test]
fn n2_nested_paragraphs_in_odp_notes() {
    let _g = serial();
    let c = format!(
        r#"<office:document-content {O}><office:body><office:presentation><draw:page><presentation:notes><draw:frame presentation:class="notes"><draw:text-box>{}</draw:text-box></draw:frame></presentation:notes></draw:page></office:presentation></office:body></office:document-content>"#,
        nest("<text:p>", "</text:p>", 190, &"a".repeat(LEAF))
    );
    let f = zip(&[
        (
            "mimetype",
            "application/vnd.oasis.opendocument.presentation",
        ),
        ("content.xml", &c),
    ]);
    let (r, used) = peak(|| to_markdown(&f, Format::Odp, &Limits::default()));
    assert!(used < BOUND, "peak {used} bytes");
    assert!(refused_or_ok(&r), "{r:?}");
}

#[test]
fn n2_nested_shapes_in_pptx_notes() {
    let _g = serial();
    const REL: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";
    let pres = format!(
        r#"<p:presentation {P}><p:sldIdLst><p:sldId id="256" r:id="rId2"/></p:sldIdLst></p:presentation>"#
    );
    let prels = format!(
        r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId2" Type="{REL}/slide" Target="slides/slide1.xml"/></Relationships>"#
    );
    let slide = format!(r#"<p:sld {P}><p:cSld><p:spTree/></p:cSld></p:sld>"#);
    let srels = format!(
        r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="{REL}/notesSlide" Target="../notesSlides/notesSlide1.xml"/></Relationships>"#
    );
    let ph = r#"<p:nvSpPr><p:nvPr><p:ph type="body"/></p:nvPr></p:nvSpPr>"#;
    let leaf = format!(
        "<p:txBody><a:p><a:r><a:t>{}</a:t></a:r></a:p></p:txBody>",
        "a".repeat(LEAF)
    );
    let shapes = nest(&format!("<p:sp>{ph}"), "</p:sp>", 190, &leaf);
    let notes = format!(r#"<p:notes {P}><p:cSld><p:spTree>{shapes}</p:spTree></p:cSld></p:notes>"#);
    let f = zip(&[
        ("ppt/presentation.xml", &pres),
        ("ppt/_rels/presentation.xml.rels", &prels),
        ("ppt/slides/slide1.xml", &slide),
        ("ppt/slides/_rels/slide1.xml.rels", &srels),
        ("ppt/notesSlides/notesSlide1.xml", &notes),
    ]);
    let (r, used) = peak(|| to_markdown(&f, Format::Pptx, &Limits::default()));
    assert!(used < BOUND, "peak {used} bytes");
    assert!(refused_or_ok(&r), "{r:?}");
}

// Re-review N1: a note inside a note body was walked once by the outer note and once more by
// the paragraph that holds it, so the work doubled with every level and no deadline check
// ended it. The conversion runs on a thread so that a hang fails the test.
#[test]
fn n1_nested_footnotes_end() {
    let _g = serial();
    let mut inner = String::from("leaf");
    for _ in 0..40 {
        inner = format!(
            "<text:p>x<text:note><text:note-body>{inner}</text:note-body></text:note></text:p>"
        );
    }
    let c = format!(
        r#"<office:document-content {O}><office:body><office:text>{inner}</office:text></office:body></office:document-content>"#
    );
    let f = zip(&[
        ("mimetype", "application/vnd.oasis.opendocument.text"),
        ("content.xml", &c),
    ]);
    let limits = Limits {
        deadline: Some(std::time::Instant::now() + std::time::Duration::from_secs(5)),
        ..Limits::default()
    };
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(to_markdown(&f, Format::Odt, &limits));
    });
    let r = rx
        .recv_timeout(std::time::Duration::from_secs(15))
        .expect("the conversion did not end");
    assert!(
        matches!(r, Ok(_) | Err(Error::TooLarge(_) | Error::Timeout)),
        "{r:?}"
    );
}

// Re-review N5: a deflate bomb whose header says the part is 1 KiB, so that only the
// bounded read (`take(cap + 1)`) stops it.
#[test]
fn a_deflate_bomb_with_a_lying_header_stops_at_the_part_limit() {
    let _g = serial();
    let doc = format!(
        "<w:document {W}><w:body><w:p><w:r><w:t>{}</w:t></w:r></w:p></w:body></w:document>",
        "a".repeat(100 << 20)
    );
    let mut f = zip(&[("word/document.xml", &doc)]);
    drop(doc);
    // the uncompressed size is at offset 22 of a local header and 24 of a directory entry
    let mut patched = 0;
    for i in 0..f.len().saturating_sub(30) {
        let at = match &f[i..i + 4] {
            b"PK\x03\x04" => 22,
            b"PK\x01\x02" => 24,
            _ => continue,
        };
        f[i + at..i + at + 4].copy_from_slice(&1024u32.to_le_bytes());
        patched += 1;
    }
    assert_eq!(patched, 2);
    let (r, used) = peak(|| to_markdown(&f, Format::Docx, &Limits::default()));
    assert!(used < 64 << 20, "peak {used} bytes");
    assert!(matches!(r, Err(Error::TooLarge(_))), "{r:?}");
}
