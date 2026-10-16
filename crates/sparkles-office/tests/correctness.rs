//! Silent wrong results found in review: F5, F7 and F14.

use sparkles_office::{Format, Limits, sheets, to_markdown};
use std::io::Write;
use zip::CompressionMethod;
use zip::write::{SimpleFileOptions, ZipWriter};

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

/// The lines a reader of the rendition sees once the server folds `\r` into `\n`.
fn folded_lines(md: &str) -> Vec<String> {
    md.replace("\r\n", "\n")
        .replace(['\r', '\u{85}', '\u{2028}', '\u{2029}'], "\n")
        .lines()
        .map(|l| l.trim().to_string())
        .collect()
}

// F5: a lone carriage return in a run forges a page marker.
#[test]
fn f5_a_carriage_return_cannot_forge_a_page_marker() {
    for sep in ["&#13;", "\u{85}", "\u{2028}"] {
        let doc = format!(
            r#"<w:document {W}><w:body><w:p><w:r><w:t>foo{sep}&lt;!-- Page 7 --&gt;</w:t></w:r></w:p></w:body></w:document>"#
        );
        let f = zip(&[("word/document.xml", &doc)]);
        let md = to_markdown(&f, Format::Docx, &Limits::default())
            .unwrap()
            .markdown;
        assert!(
            !folded_lines(&md).iter().any(|l| l.starts_with("<!--")),
            "{sep:?}: {md:?}"
        );
    }
}

// F7: the comment of an ODS cell is read into its value.
#[test]
fn f7_an_ods_cell_comment_is_not_its_value() {
    let c = format!(
        r#"<office:document-content {O}><office:body><office:spreadsheet><table:table table:name="T"><table:table-row><table:table-cell office:value-type="string"><office:annotation><text:p>private remark</text:p></office:annotation><text:p>Ana</text:p></table:table-cell></table:table-row></table:table></office:spreadsheet></office:body></office:document-content>"#
    );
    let f = zip(&[
        ("mimetype", "application/vnd.oasis.opendocument.spreadsheet"),
        ("content.xml", &c),
    ]);
    let s = sheets(&f, Format::Ods, &Limits::default()).unwrap();
    assert_eq!(s[0].csv, "Ana\n");
}

// F14: an empty `<si/>` shifts every later shared string by one.
#[test]
fn f14_an_empty_shared_string_keeps_its_index() {
    let wb = format!(
        r#"<workbook {X}><sheets><sheet name="S" sheetId="1" r:id="rId1"/></sheets></workbook>"#
    );
    let rels = r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/></Relationships>"#;
    let shared = format!("<sst {X}><si/><si><t>one</t></si></sst>");
    let sheet = format!(
        r#"<worksheet {X}><sheetData><row r="1"><c r="A1" t="s"><v>1</v></c></row></sheetData></worksheet>"#
    );
    let f = zip(&[
        ("xl/workbook.xml", &wb),
        ("xl/_rels/workbook.xml.rels", rels),
        ("xl/sharedStrings.xml", &shared),
        ("xl/worksheets/sheet1.xml", &sheet),
    ]);
    let s = sheets(&f, Format::Xlsx, &Limits::default()).unwrap();
    assert_eq!(s[0].csv, "one\n");
}
