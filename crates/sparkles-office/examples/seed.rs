//! Write the seed corpus of the fuzz target: one document of every format, each after the
//! byte that names its format, and the amplification shapes of `tests/budget.rs`.
//! Usage: `cargo run --example seed --features fixtures DIR`.

use sparkles_office::Format;
use sparkles_office::fixtures::*;

/// The shapes of `tests/budget.rs` at the fuzz target's size: small files that ask for
/// much more memory than they are. Mutations of these start the fuzzer near the budgets.
fn amplifiers() -> Vec<(&'static str, Format, Vec<u8>)> {
    const X: &str = r#"xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships""#;
    let rels = r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/></Relationships>"#;
    let ilvl = format!(
        r#"<w:document {W}><w:body><w:p><w:pPr><w:numPr><w:ilvl w:val="40000000000"/><w:numId w:val="1"/></w:numPr></w:pPr><w:r><w:t>deep</w:t></w:r></w:p></w:body></w:document>"#
    );
    let mut wide = String::from("<w:tbl><w:tr>");
    for _ in 0..200 {
        wide.push_str(r#"<w:tc><w:tcPr><w:gridSpan w:val="64"/></w:tcPr><w:p/></w:tc>"#);
    }
    wide.push_str("</w:tr>");
    for _ in 0..200 {
        wide.push_str("<w:tr><w:tc><w:p><w:r><w:t>x</w:t></w:r></w:p></w:tc></w:tr>");
    }
    wide.push_str("</w:tbl>");
    let wide = format!("<w:document {W}><w:body>{wide}</w:body></w:document>");
    let book = format!(
        r#"<workbook {X}><sheets><sheet name="S" sheetId="1" r:id="rId1"/><sheet name="T" sheetId="2" r:id="rId1"/></sheets></workbook>"#
    );
    let shared = format!("<sst {X}><si><t>{}</t></si><si/></sst>", "a".repeat(4096));
    let mut sparse = format!("<worksheet {X}><sheetData>");
    for r in 1..=300 {
        sparse.push_str(&format!(
            r#"<row r="{r}"><c r="XFD{r}"><v>1</v></c><c r="A{r}" t="s"><v>0</v></c></row>"#
        ));
    }
    sparse.push_str("</sheetData></worksheet>");
    let xlsx = container(&[
        ("xl/workbook.xml", &book),
        ("xl/_rels/workbook.xml.rels", rels),
        ("xl/sharedStrings.xml", &shared),
        ("xl/worksheets/sheet1.xml", &sparse),
    ]);
    let rows = r#"<table:table-row table:number-rows-repeated="1000"><table:table-cell table:number-columns-repeated="16000"/><table:table-cell office:value-type="string" table:number-columns-repeated="1000"><office:annotation><text:p>note</text:p></office:annotation><text:p>cell<text:s text:c="100"/></text:p></table:table-cell></table:table-row>"#;
    let content = format!(
        r#"<office:document-content {O}><office:body><office:spreadsheet><table:table table:name="T">{rows}{rows}</table:table></office:spreadsheet></office:body></office:document-content>"#
    );
    let ods = container(&[
        ("mimetype", "application/vnd.oasis.opendocument.spreadsheet"),
        ("content.xml", &content),
    ]);
    vec![
        (
            "docx-list-level",
            Format::Docx,
            container(&[("word/document.xml", &ilvl)]),
        ),
        (
            "docx-wide-table",
            Format::Docx,
            container(&[("word/document.xml", &wide)]),
        ),
        ("xlsx-sparse-rows", Format::Xlsx, xlsx),
        ("ods-repeats", Format::Ods, ods),
    ]
}

fn main() -> std::io::Result<()> {
    let dir = std::env::args().nth(1).expect("usage: seed DIR");
    std::fs::create_dir_all(&dir)?;
    let docs = [
        (Format::Docx, docx()),
        (Format::Pptx, pptx()),
        (Format::Xlsx, xlsx()),
        (Format::Odt, odt()),
        (Format::Odp, odp()),
        (Format::Ods, ods()),
    ];
    for (i, (f, bytes)) in docs.into_iter().enumerate() {
        let mut out = vec![i as u8];
        out.extend_from_slice(&bytes);
        std::fs::write(format!("{dir}/{}", f.name()), out)?;
    }
    for (name, f, bytes) in amplifiers() {
        let i = Format::ALL.iter().position(|a| *a == f).expect("a format");
        let mut out = vec![i as u8];
        out.extend_from_slice(&bytes);
        std::fs::write(format!("{dir}/{name}"), out)?;
    }
    Ok(())
}
