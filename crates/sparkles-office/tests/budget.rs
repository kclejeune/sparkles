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

/// Run `f` on a thread of its own and wait at most `secs` seconds for it, so that a
/// conversion that ignores its deadline fails the test instead of hanging it.
fn within<T: Send + 'static>(secs: u64, f: impl FnOnce() -> T + Send + 'static) -> T {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::Builder::new()
        .stack_size(16 << 20)
        .spawn(move || {
            let _ = tx.send(f());
        })
        .unwrap();
    rx.recv_timeout(std::time::Duration::from_secs(secs))
        .expect("the conversion did not end")
}

fn deadline_in(secs: u64) -> Limits {
    Limits {
        deadline: Some(std::time::Instant::now() + std::time::Duration::from_secs(secs)),
        ..Limits::default()
    }
}

const REL_NS: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";

fn rels(items: &[(&str, &str, &str, bool)]) -> String {
    let mut s = String::from(
        r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">"#,
    );
    for (id, kind, target, external) in items {
        s.push_str(&format!(
            r#"<Relationship Id="{id}" Type="{REL_NS}/{kind}" Target="{target}"{}/>"#,
            if *external {
                r#" TargetMode="External""#
            } else {
                ""
            }
        ));
    }
    s.push_str("</Relationships>");
    s
}

// Re-review 2, F1: one 20 MiB relationship target named by 1,000 hyperlinks of a
// paragraph was copied once per link, 20 GB from a file of a few tens of KB.
#[test]
fn r2_f1_long_target_shared_by_docx_hyperlinks() {
    let _g = serial();
    let url = format!("https://example.org/{}", "a".repeat(20 << 20));
    let link = r#"<w:hyperlink r:id="rId1"><w:r><w:t>x</w:t></w:r></w:hyperlink>"#;
    let doc = format!(
        r#"<w:document {W} xmlns:r="{REL_NS}"><w:body><w:p>{}</w:p></w:body></w:document>"#,
        link.repeat(1000)
    );
    let r = rels(&[("rId1", "hyperlink", &url, true)]);
    let f = zip(&[
        ("word/document.xml", &doc),
        ("word/_rels/document.xml.rels", &r),
    ]);
    drop((url, r));
    let (r, used) = peak(|| {
        within(30, move || {
            to_markdown(&f, Format::Docx, &Limits::default())
        })
    });
    assert!(used < BOUND, "peak {used} bytes");
    assert!(refused_or_ok(&r), "{r:?}");
}

// F1 under the target limit: a 2,000-byte target named by 150,000 hyperlinks is 300 MB of
// links, which the budget refuses as they are made.
#[test]
fn r2_f1_target_under_the_limit_in_many_hyperlinks() {
    let _g = serial();
    let url = format!("https://example.org/{}", "a".repeat(2000));
    let link = r#"<w:hyperlink r:id="rId1"><w:r><w:t>x</w:t></w:r></w:hyperlink>"#;
    let doc = format!(
        r#"<w:document {W} xmlns:r="{REL_NS}"><w:body><w:p>{}</w:p></w:body></w:document>"#,
        link.repeat(150_000)
    );
    let r = rels(&[("rId1", "hyperlink", &url, true)]);
    let f = zip(&[
        ("word/document.xml", &doc),
        ("word/_rels/document.xml.rels", &r),
    ]);
    drop(doc);
    let (r, used) = peak(|| {
        within(30, move || {
            to_markdown(&f, Format::Docx, &Limits::default())
        })
    });
    assert!(used < BOUND, "peak {used} bytes");
    assert!(matches!(r, Err(Error::TooLarge(_))), "{r:?}");
}

/// A presentation of `slides` (presentation.xml body, its rels) and one slide part.
fn pptx(sld_ids: &str, prels: &str, slide: &str, srels: &str) -> Vec<u8> {
    let pres =
        format!(r#"<p:presentation {P}><p:sldIdLst>{sld_ids}</p:sldIdLst></p:presentation>"#);
    zip(&[
        ("ppt/presentation.xml", &pres),
        ("ppt/_rels/presentation.xml.rels", prels),
        ("ppt/slides/slide1.xml", slide),
        ("ppt/slides/_rels/slide1.xml.rels", srels),
    ])
}

#[test]
fn r2_f1_long_target_shared_by_pptx_runs() {
    let _g = serial();
    let url = format!("https://example.org/{}", "a".repeat(20 << 20));
    let run = r#"<a:r><a:rPr><a:hlinkClick r:id="rId9"/></a:rPr><a:t>x</a:t></a:r>"#;
    let slide = format!(
        r#"<p:sld {P}><p:cSld><p:spTree><p:sp><p:txBody><a:p>{}</a:p></p:txBody></p:sp></p:spTree></p:cSld></p:sld>"#,
        run.repeat(1000)
    );
    let prels = rels(&[("rId2", "slide", "slides/slide1.xml", false)]);
    let srels = rels(&[("rId9", "hyperlink", &url, true)]);
    let f = pptx(r#"<p:sldId id="256" r:id="rId2"/>"#, &prels, &slide, &srels);
    drop((url, srels));
    let (r, used) = peak(|| {
        within(30, move || {
            to_markdown(&f, Format::Pptx, &Limits::default())
        })
    });
    assert!(used < BOUND, "peak {used} bytes");
    assert!(refused_or_ok(&r), "{r:?}");
}

// F1, slide list: 100,000 `sldId` naming one slide whose path is 2,000 bytes long, and
// which is missing, were 200 MB of copied paths and 200 MB of notes about them.
#[test]
fn r2_f1_many_slide_ids_with_one_long_target() {
    let _g = serial();
    let target = format!("slides/{}.xml", "s".repeat(2000));
    let prels = rels(&[("rId2", "slide", &target, false)]);
    let ids = r#"<p:sldId r:id="rId2"/>"#.repeat(100_000);
    let f = pptx(&ids, &prels, "<p:sld/>", &rels(&[]));
    let (r, used) = peak(|| {
        within(30, move || {
            to_markdown(&f, Format::Pptx, &Limits::default())
        })
    });
    assert!(used < BOUND, "peak {used} bytes");
    assert!(
        matches!(r, Ok(_) | Err(Error::TooLarge(_) | Error::Corrupt(_))),
        "{r:?}"
    );
}

// Re-review 2, F2: one 12 MiB number format named by 400,000 cell formats was scanned once
// per format, terabytes of characters before the first cell, with no deadline check.
#[test]
fn r2_f2_one_long_format_code_in_many_cell_formats() {
    let _g = serial();
    let mut styles = format!(
        r#"<styleSheet {X}><numFmts><numFmt numFmtId="164" formatCode="{}"/></numFmts><cellXfs>"#,
        "0".repeat(12 << 20)
    );
    styles.push_str(&r#"<xf numFmtId="164"/>"#.repeat(400_000));
    styles.push_str("</cellXfs></styleSheet>");
    let sheet = format!(
        r#"<worksheet {X}><sheetData><row r="1"><c r="A1"><v>1</v></c></row></sheetData></worksheet>"#
    );
    let wb = format!(
        r#"<workbook {X}><sheets><sheet name="S" sheetId="1" r:id="rId1"/></sheets></workbook>"#
    );
    let wrels = rels(&[("rId1", "worksheet", "worksheets/sheet1.xml", false)]);
    let f = zip(&[
        ("xl/workbook.xml", &wb),
        ("xl/_rels/workbook.xml.rels", &wrels),
        ("xl/styles.xml", &styles),
        ("xl/worksheets/sheet1.xml", &sheet),
    ]);
    drop(styles);
    let r = within(30, move || sheets(&f, Format::Xlsx, &deadline_in(10)));
    assert!(r.is_ok(), "{r:?}");
}

/// The faster of two runs of `f`, which takes out most of the noise of a loaded machine.
fn fastest<T>(mut f: impl FnMut() -> T) -> std::time::Duration {
    (0..2)
        .map(|_| {
            let t = std::time::Instant::now();
            std::hint::black_box(f());
            t.elapsed()
        })
        .min()
        .unwrap()
}

/// A presentation whose notes hold 400,000 empty paragraphs under `depth` nested body
/// placeholders.
fn nested_notes(depth: usize) -> Vec<u8> {
    let ph = r#"<p:nvSpPr><p:nvPr><p:ph type="body"/></p:nvPr></p:nvSpPr>"#;
    let leaf = format!("<p:txBody>{}</p:txBody>", "<a:p/>".repeat(400_000));
    let shapes = nest(&format!("<p:sp>{ph}"), "</p:sp>", depth, &leaf);
    let notes = format!(r#"<p:notes {P}><p:cSld><p:spTree>{shapes}</p:spTree></p:cSld></p:notes>"#);
    let prels = rels(&[("rId2", "slide", "slides/slide1.xml", false)]);
    let srels = rels(&[(
        "rId1",
        "notesSlide",
        "../notesSlides/notesSlide1.xml",
        false,
    )]);
    let pres = format!(
        r#"<p:presentation {P}><p:sldIdLst><p:sldId r:id="rId2"/></p:sldIdLst></p:presentation>"#
    );
    let slide = format!(r#"<p:sld {P}><p:cSld><p:spTree/></p:cSld></p:sld>"#);
    zip(&[
        ("ppt/presentation.xml", &pres),
        ("ppt/_rels/presentation.xml.rels", &prels),
        ("ppt/slides/slide1.xml", &slide),
        ("ppt/slides/_rels/slide1.xml.rels", &srels),
        ("ppt/notesSlides/notesSlide1.xml", &notes),
    ])
}

// Re-review 2, F3: a body placeholder inside 190 others walked its 400,000 paragraphs once
// per level, and the deadline was read only a few times. The walk is linear when 190
// levels cost about what one level costs. The ratio, unlike a wall-clock bound, holds on
// a loaded machine: the per-level walk made it well over 20.
#[test]
fn r2_f3_nested_note_placeholders_over_many_paragraphs() {
    let _g = serial();
    let (one, deep) = (nested_notes(1), nested_notes(190));
    let r = within(60, move || {
        let run = |f: &[u8]| to_markdown(f, Format::Pptx, &deadline_in(30)).unwrap();
        (fastest(|| run(&one)), fastest(|| run(&deep)))
    });
    let (t1, t190) = r;
    assert!(t190 < t1 * 20, "one level {t1:?}, 190 levels {t190:?}");
}

// Re-review 2, F4: a style based on itself with a 7 MiB id cloned and hashed that id nine
// times per paragraph, and the clock was read once in 1,024 paragraphs.
#[test]
fn r2_f4_long_style_id_based_on_itself() {
    let _g = serial();
    let id = "L".repeat(7 << 20);
    let styles = format!(
        r#"<w:styles {W}><w:style w:styleId="a"><w:basedOn w:val="{id}"/></w:style><w:style w:styleId="{id}"><w:basedOn w:val="{id}"/></w:style></w:styles>"#
    );
    let doc = format!(
        r#"<w:document {W}><w:body>{}</w:body></w:document>"#,
        r#"<w:p><w:pPr><w:pStyle w:val="a"/></w:pPr></w:p>"#.repeat(200_000)
    );
    let f = zip(&[("word/document.xml", &doc), ("word/styles.xml", &styles)]);
    drop((id, styles));
    let r = within(30, move || to_markdown(&f, Format::Docx, &deadline_in(10)));
    assert!(r.is_ok(), "{r:?}");
}

// F4, ODS: a 4 MiB table style name followed by 300,000 `table-properties` was cloned
// once per element.
#[test]
fn r2_f4_long_table_style_name() {
    let _g = serial();
    let name = "n".repeat(4 << 20);
    let props = r#"<style:table-properties table:display="false"/>"#.repeat(300_000);
    let c = format!(
        r#"<office:document-content {O} xmlns:style="urn:oasis:names:tc:opendocument:xmlns:style:1.0"><office:automatic-styles><style:style style:name="{name}" style:family="table">{props}</style:style></office:automatic-styles><office:body><office:spreadsheet><table:table table:name="T"><table:table-row><table:table-cell office:value-type="float" office:value="1"/></table:table-row></table:table></office:spreadsheet></office:body></office:document-content>"#
    );
    let f = zip(&[
        ("mimetype", "application/vnd.oasis.opendocument.spreadsheet"),
        ("content.xml", &c),
    ]);
    drop((name, props, c));
    let r = within(30, move || sheets(&f, Format::Ods, &deadline_in(10)));
    assert!(r.is_ok(), "{r:?}");
}

// Re-review 2, P3: cells of a row in descending column order were each inserted at the
// front of the row, a quadratic memmove for every row. The unit test
// `cells_out_of_order_are_appended_then_sorted_once` checks the cost deterministically,
// and this one checks that such rows come out in column order.
#[test]
fn r2_cells_in_descending_column_order() {
    let _g = serial();
    let mut row = String::new();
    for c in (0..16_384usize).rev() {
        row.push_str(&format!(r#"<c r="{}1"><v>{c}</v></c>"#, column_name(c)));
    }
    let mut sheet = format!("<worksheet {X}><sheetData>");
    for _ in 0..20 {
        sheet.push_str(&format!("<row>{row}</row>"));
    }
    sheet.push_str("</sheetData></worksheet>");
    let f = workbook(1, &format!("<sst {X}/>"), &sheet);
    let s = within(60, move || sheets(&f, Format::Xlsx, &Limits::default())).unwrap();
    assert_eq!(s[0].columns, 16_384);
    assert_eq!(s[0].rows, 20);
    let first = s[0].csv.lines().next().unwrap();
    let want: Vec<String> = (0..16_384).map(|c| c.to_string()).collect();
    assert_eq!(first, want.join(","));
}

fn column_name(mut c: usize) -> String {
    let mut s = Vec::new();
    loop {
        s.push(b'A' + (c % 26) as u8);
        if c < 26 {
            break;
        }
        c = c / 26 - 1;
    }
    s.reverse();
    String::from_utf8(s).unwrap()
}

// Re-review 2, P3: text and CDATA children were not counted as nodes, so 2 million empty
// CDATA sections were a tree of 160 MB under the node limit.
#[test]
fn r2_cdata_sections_count_as_nodes() {
    let _g = serial();
    let doc = format!(
        "<w:document {W}><w:body><w:p>{}</w:p></w:body></w:document>",
        "<![CDATA[]]>".repeat(1_900_000)
    );
    let f = zip(&[("word/document.xml", &doc)]);
    drop(doc);
    let r = to_markdown(&f, Format::Docx, &Limits::default());
    assert!(matches!(r, Err(Error::TooLarge(_))), "{r:?}");
}

// Re-review 3: a link inside a link is read as its text. Without that, each of 190 levels
// copied and charged the whole 1 MiB text of the link once more, and the links together
// passed the text limit.
#[test]
fn r3_nested_hyperlinks_are_read_once() {
    let _g = serial();
    let url = "https://example.org/a";
    let leaf = format!("<w:r><w:t>{}</w:t></w:r>", "a".repeat(1 << 20));
    let doc = format!(
        r#"<w:document {W} xmlns:r="{REL_NS}"><w:body><w:p>{}</w:p></w:body></w:document>"#,
        nest(r#"<w:hyperlink r:id="rId1">"#, "</w:hyperlink>", 190, &leaf)
    );
    let r = rels(&[("rId1", "hyperlink", url, true)]);
    let f = zip(&[
        ("word/document.xml", &doc),
        ("word/_rels/document.xml.rels", &r),
    ]);
    let md = within(30, move || {
        to_markdown(&f, Format::Docx, &Limits::default())
    })
    .unwrap()
    .markdown;
    assert_eq!(md.matches(url).count(), 1);
    // the same in an OpenDocument text
    let c = format!(
        r#"<office:document-content {O} xmlns:xlink="http://www.w3.org/1999/xlink"><office:body><office:text><text:p>{}</text:p></office:text></office:body></office:document-content>"#,
        nest(
            &format!(r#"<text:a xlink:href="{url}">"#),
            "</text:a>",
            190,
            &"a".repeat(1 << 20)
        )
    );
    let f = zip(&[
        ("mimetype", "application/vnd.oasis.opendocument.text"),
        ("content.xml", &c),
    ]);
    let md = within(30, move || to_markdown(&f, Format::Odt, &Limits::default()))
        .unwrap()
        .markdown;
    assert_eq!(md.matches(url).count(), 1);
}
