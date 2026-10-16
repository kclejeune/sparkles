use super::*;
use crate::fixtures::*;

fn lim() -> Limits {
    Limits::default()
}

#[test]
fn a_docx() {
    let d = to_markdown(&docx(), Format::Docx, &lim()).unwrap();
    assert_eq!(d.title.as_deref(), Some("Q3 report"));
    let m = &d.markdown;
    assert!(
        m.starts_with(
            "# Quarterly report\n\n# Teams\n\nAna Lima moved to the payments team.[^fn2]\n\n"
        ),
        "{m}"
    );
    assert!(
        m.contains("- first bullet\n    - nested bullet\n\n1. step one\n2. step two\n"),
        "{m}"
    );
    assert!(
        m.contains("## Table\n\n| Name | Team \\| unit |\n| --- | --- |\n| Ana | payments |\n"),
        "{m}"
    );
    assert!(
        m.contains("See [the site](https://example.org/site)\n"),
        "{m}"
    );
    assert!(!m.contains("deleted words"), "{m}");
    // a forged marker is escaped
    assert!(m.contains("\n\\<!-- Page 9 -->\n"), "{m}");
    assert!(!m.lines().any(|l| l.starts_with("<!--")), "{m}");
    // no images asked for: the alt text stays
    assert!(m.contains("[Image: A chart of hires]"), "{m}");
    assert!(
        m.trim_end().ends_with("[^fn2]: Effective 1 October."),
        "{m}"
    );
    assert!(d.images.is_empty());
}

#[test]
fn a_docx_with_pictures() {
    let l = Limits {
        max_images: 4,
        ..lim()
    };
    let d = to_markdown(&docx(), Format::Docx, &l).unwrap();
    assert!(d.markdown.contains("<!-- image:0 -->"), "{}", d.markdown);
    assert_eq!(d.images.len(), 1);
    assert_eq!(d.images[0].media_type, "image/png");
    assert_eq!(d.images[0].alt.as_deref(), Some("A chart of hires"));
}

#[test]
fn a_pptx() {
    let d = to_markdown(&pptx(), Format::Pptx, &lim()).unwrap();
    let m = &d.markdown;
    assert_eq!(d.slides, 3);
    assert!(m.starts_with("<!-- Page 1 -->\n\n# Slide 1: Hiring plan\n\n- Five engineers join payments\n    - Two start in October\n\nNotes:\n\nMention the Berlin office.\n\n<!-- Page 2 -->\n\n# Slide 2\n\n| Team | Size |\n| --- | --- |\n| Payments | 5 |\n\nPlain text box\n"), "{m}");
    assert!(!m.contains("Hidden slide text"), "{m}");
    assert!(!m.contains("<!-- Page 3"), "{m}");
}

#[test]
fn an_xlsx() {
    let s = sheets(&xlsx(), Format::Xlsx, &lim()).unwrap();
    assert_eq!(s.len(), 1, "{s:?}");
    assert_eq!(s[0].name, "People");
    assert_eq!(s[0].index, 1);
    assert_eq!(
        s[0].csv,
        "id,name,start,active\n1,\"Ana, \"\"AL\"\" Lima\",2025-10-01,TRUE\n2,Kai Ito,2025-10-02T12:00:00,FALSE\ntail,,,\n"
    );
    assert_eq!((s[0].rows, s[0].columns), (4, 4));
    assert!(to_markdown(&xlsx(), Format::Xlsx, &lim()).is_err());
}

#[test]
fn an_odt() {
    let d = to_markdown(&odt(), Format::Odt, &lim()).unwrap();
    let m = &d.markdown;
    assert_eq!(d.title.as_deref(), Some("Handbook title"));
    assert!(m.starts_with("# Handbook\n\nAna Lima  moved to [payments](https://example.org/p).[^fn1]\n\n- apples\n- pears\n\n1. one\n2. two\n\n| Team | Size |\n| --- | --- |\n| Payments | 5 |\n\nPic[Image: Org chart]") || m.contains("[Image: Org chart]"), "{m}");
    assert!(m.contains("- apples\n- pears\n\n1. one\n2. two\n"), "{m}");
    assert!(m.contains("[^fn1]: Since October."), "{m}");
    assert!(m.contains("[Image: Org chart]"), "{m}");
}

#[test]
fn an_odp() {
    let d = to_markdown(&odp(), Format::Odp, &lim()).unwrap();
    assert_eq!(
        d.markdown,
        "<!-- Page 1 -->\n\n# Slide 1: Roadmap\n\n- Launch in Berlin\n\nNotes:\n\nSay hello\n\n<!-- Page 2 -->\n\n# Slide 2: Budget\n"
    );
}

#[test]
fn an_ods() {
    let s = sheets(&ods(), Format::Ods, &lim()).unwrap();
    assert_eq!(s.len(), 1, "{s:?}");
    assert_eq!(s[0].name, "People");
    assert_eq!(s[0].csv, "name,joined,n\n\"Ana, A.\",2026-10-01,5\n");
}

#[test]
fn formats_are_told_apart() {
    for (b, f) in [
        (docx(), Format::Docx),
        (pptx(), Format::Pptx),
        (xlsx(), Format::Xlsx),
        (odt(), Format::Odt),
        (odp(), Format::Odp),
        (ods(), Format::Ods),
    ] {
        assert_eq!(sniff(&b), Some(f));
    }
    assert_eq!(sniff(&container(&[("a.txt", "x")])), None);
    assert_eq!(sniff(b"hello"), None);
    assert_eq!(Format::from_name("Report.DOCX"), Some(Format::Docx));
    assert_eq!(
        Format::parse("application/vnd.oasis.opendocument.text; x=y"),
        Some(Format::Odt)
    );
}

#[test]
fn dates() {
    use crate::sheet::excel_date;
    assert_eq!(excel_date(45931.0, false).as_deref(), Some("2025-10-01"));
    assert_eq!(excel_date(1.0, false).as_deref(), Some("1900-01-01"));
    assert_eq!(excel_date(61.0, false).as_deref(), Some("1900-03-01"));
    assert_eq!(excel_date(0.25, false).as_deref(), Some("06:00:00"));
    assert_eq!(excel_date(0.0, true).as_deref(), Some("00:00:00"));
    assert_eq!(excel_date(44469.0, true).as_deref(), Some("2025-10-01"));
    assert_eq!(excel_date(-1.0, false), None);
    assert!(crate::xlsx::is_date_format("yyyy\\-mm\\-dd"));
    assert!(crate::xlsx::is_date_format("[h]:mm:ss"));
    assert!(!crate::xlsx::is_date_format("#,##0.00 \"days\""));
    assert!(!crate::xlsx::is_date_format("General"));
}

#[test]
fn refusals() {
    // legacy and protected files are OLE containers
    let mut ole = vec![0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1];
    ole.extend_from_slice(&[0; 64]);
    assert!(matches!(
        to_markdown(&ole, Format::Docx, &lim()),
        Err(Error::Unsupported(_))
    ));
    assert!(matches!(
        to_markdown(b"not a zip", Format::Docx, &lim()),
        Err(Error::Corrupt(_))
    ));
    // a stored part larger than the limit; a deflate bomb, whose header cannot be trusted
    // to name its size, is `a_deflate_bomb_stops_at_the_part_limit` in `tests/budget.rs`
    let big = format!(
        "<w:document {W}><w:body><w:p><w:r><w:t>{}</w:t></w:r></w:p></w:body></w:document>",
        "x".repeat(4096)
    );
    let l = Limits {
        max_part: 1024,
        ..lim()
    };
    let b = container(&[("word/document.xml", &big)]);
    assert!(matches!(
        to_markdown(&b, Format::Docx, &l),
        Err(Error::TooLarge(_))
    ));
    let l = Limits {
        max_text: 100,
        ..lim()
    };
    assert!(matches!(
        to_markdown(&b, Format::Docx, &l),
        Err(Error::TooLarge(_))
    ));
    // entities cannot grow
    let doctype = format!(
        r#"<?xml version="1.0"?><!DOCTYPE d [<!ENTITY a "aaaa">]><w:document {W}><w:body><w:p><w:r><w:t>&a;</w:t></w:r></w:p></w:body></w:document>"#
    );
    let b = container(&[("word/document.xml", &doctype)]);
    assert!(matches!(
        to_markdown(&b, Format::Docx, &lim()),
        Err(Error::Corrupt(_))
    ));
    // too many entries
    let l = Limits {
        max_entries: 2,
        ..lim()
    };
    assert!(matches!(
        to_markdown(&docx(), Format::Docx, &l),
        Err(Error::TooLarge(_))
    ));
    // too deep, too many nodes
    let deep = format!(
        "<w:document {W}><w:body>{}{}</w:body></w:document>",
        "<w:sdt><w:sdtContent>".repeat(300),
        "</w:sdtContent></w:sdt>".repeat(300)
    );
    let b = container(&[("word/document.xml", &deep)]);
    assert!(matches!(
        to_markdown(&b, Format::Docx, &lim()),
        Err(Error::TooLarge(_))
    ));
    // a sheet with too many cells
    let l = Limits {
        max_cells: 3,
        ..lim()
    };
    assert!(matches!(
        sheets(&xlsx(), Format::Xlsx, &l),
        Err(Error::TooLarge(_))
    ));
}

/// A small xorshift generator, so the mutations are the same on every run.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
}

/// Convert every mutation of the fixtures: flipped, overwritten and removed bytes, and
/// truncations. Any answer is fine, a panic is not. The container is stored, so most
/// changes land in the XML.
#[test]
fn damaged_files_never_panic() {
    let l = Limits {
        max_images: 2,
        ..lim()
    };
    let fixtures = [
        (docx(), Format::Docx),
        (pptx(), Format::Pptx),
        (xlsx(), Format::Xlsx),
        (odt(), Format::Odt),
        (odp(), Format::Odp),
        (ods(), Format::Ods),
    ];
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    for (orig, format) in &fixtures {
        for round in 0..400 {
            let mut b = orig.clone();
            match round % 4 {
                0 => {
                    for _ in 0..1 + rng.next() % 4 {
                        let i = (rng.next() as usize) % b.len();
                        b[i] ^= 1 << (rng.next() % 8);
                    }
                }
                1 => {
                    for _ in 0..1 + rng.next() % 8 {
                        let i = (rng.next() as usize) % b.len();
                        b[i] = b"<>/\"&;= \n\0"[(rng.next() % 10) as usize];
                    }
                }
                2 => {
                    let i = (rng.next() as usize) % b.len();
                    let n = 1 + (rng.next() as usize) % 40;
                    b.drain(i..(i + n).min(b.len()));
                }
                _ => b.truncate((rng.next() as usize) % b.len()),
            }
            let _ = sniff(&b);
            if format.is_spreadsheet() {
                let _ = sheets(&b, *format, &l);
            } else {
                let _ = to_markdown(&b, *format, &l);
            }
        }
    }
}

/// The output budget refuses every format, and the fixtures still convert at the default.
#[test]
fn the_output_budget_stops_every_format() {
    let small = Limits {
        max_output: 64,
        ..lim()
    };
    for (b, f) in [
        (docx(), Format::Docx),
        (pptx(), Format::Pptx),
        (odt(), Format::Odt),
        (odp(), Format::Odp),
    ] {
        assert!(
            matches!(to_markdown(&b, f, &small), Err(Error::TooLarge(_))),
            "{f:?}"
        );
        assert!(to_markdown(&b, f, &lim()).is_ok(), "{f:?}");
    }
    for (b, f) in [(xlsx(), Format::Xlsx), (ods(), Format::Ods)] {
        assert!(
            matches!(sheets(&b, f, &small), Err(Error::TooLarge(_))),
            "{f:?}"
        );
        assert!(!sheets(&b, f, &lim()).unwrap().is_empty(), "{f:?}");
    }
}

#[test]
fn a_hidden_run_is_left_out() {
    let doc = format!(
        r#"<w:document {W}><w:body><w:p><w:r><w:t>shown </w:t></w:r><w:r><w:rPr><w:vanish/></w:rPr><w:t>secret </w:t></w:r><w:r><w:rPr><w:vanish w:val="0"/></w:rPr><w:t>also shown</w:t></w:r></w:p><w:tbl><w:tr><w:tc><w:p><w:r><w:rPr><w:vanish w:val="true"/></w:rPr><w:t>cell secret</w:t></w:r><w:r><w:t>cell</w:t></w:r></w:p></w:tc></w:tr></w:tbl></w:body></w:document>"#
    );
    let b = container(&[("word/document.xml", &doc)]);
    let m = to_markdown(&b, Format::Docx, &lim()).unwrap().markdown;
    assert!(m.contains("shown also shown"), "{m}");
    assert!(!m.contains("secret"), "{m}");
    assert!(m.contains("| cell |"), "{m}");
}

const X: &str = r#"xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:x="http://schemas.openxmlformats.org/officeDocument/2006/relationships""#;

fn workbook_with(sheet_tag: &str, sheet: &str) -> Vec<u8> {
    let wb = format!(r#"<workbook {X}><sheets>{sheet_tag}</sheets></workbook>"#);
    let rels = r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/></Relationships>"#;
    container(&[
        ("xl/workbook.xml", &wb),
        ("xl/_rels/workbook.xml.rels", rels),
        ("xl/worksheets/sheet1.xml", sheet),
    ])
}

#[test]
fn a_workbook_may_bind_another_prefix_to_the_relationship_namespace() {
    let sheet = format!(
        r#"<worksheet {X}><sheetData><row r="1"><c r="A1" t="inlineStr"><is><t>kept</t></is></c></row></sheetData></worksheet>"#
    );
    let f = workbook_with(r#"<sheet name="S" sheetId="1" x:id="rId1"/>"#, &sheet);
    let s = sheets(&f, Format::Xlsx, &lim()).unwrap();
    assert_eq!(s[0].csv, "kept\n");
}

#[test]
fn the_phonetic_runs_of_an_inline_string_are_left_out() {
    let sheet = format!(
        r#"<worksheet {X}><sheetData><row r="1"><c r="A1" t="inlineStr"><is><r><t>Tokyo</t></r><rPh sb="0" eb="2"><t>toukyou</t></rPh></is></c></row></sheetData></worksheet>"#
    );
    let f = workbook_with(r#"<sheet name="S" sheetId="1" x:id="rId1"/>"#, &sheet);
    let s = sheets(&f, Format::Xlsx, &lim()).unwrap();
    assert_eq!(s[0].csv, "Tokyo\n");
}

#[test]
fn an_element_with_thousands_of_attributes_is_refused() {
    let attrs: String = (0..2000).map(|i| format!(r#" a{i}="1""#)).collect();
    let doc = format!(r#"<w:document {W}><w:body><w:p{attrs}/></w:body></w:document>"#);
    let b = container(&[("word/document.xml", &doc)]);
    assert!(matches!(
        to_markdown(&b, Format::Docx, &lim()),
        Err(Error::TooLarge(_))
    ));
    // the streaming readers refuse it as well
    let sheet = format!(
        r#"<worksheet {X}><sheetData><row r="1"><c r="A1"{attrs}><v>1</v></c></row></sheetData></worksheet>"#
    );
    let f = workbook_with(r#"<sheet name="S" sheetId="1" x:id="rId1"/>"#, &sheet);
    assert!(matches!(
        sheets(&f, Format::Xlsx, &lim()),
        Err(Error::TooLarge(_))
    ));
}

#[test]
fn a_deadline_that_has_passed_stops_every_format() {
    let past = std::time::Instant::now();
    let l = Limits {
        deadline: Some(past),
        ..lim()
    };
    for (b, f) in [
        (docx(), Format::Docx),
        (pptx(), Format::Pptx),
        (odt(), Format::Odt),
        (odp(), Format::Odp),
    ] {
        assert_eq!(to_markdown(&b, f, &l).unwrap_err(), Error::Timeout, "{f:?}");
    }
    for (b, f) in [(xlsx(), Format::Xlsx), (ods(), Format::Ods)] {
        assert_eq!(sheets(&b, f, &l).unwrap_err(), Error::Timeout, "{f:?}");
    }
    // a deadline in the future changes nothing
    let l = Limits {
        deadline: Some(past + std::time::Duration::from_secs(3600)),
        ..lim()
    };
    assert!(to_markdown(&docx(), Format::Docx, &l).is_ok());
}

/// A document of `n` paragraphs, each naming its own footnote when `refs` is set.
fn footnoted(n: usize, refs: bool) -> Vec<u8> {
    let mut body = String::new();
    let mut notes = String::new();
    for i in 1..=n {
        let r = if refs {
            format!(r#"<w:footnoteReference w:id="{i}"/>"#)
        } else {
            String::new()
        };
        body.push_str(&format!(
            r#"<w:p><w:r><w:t>p{i}</w:t></w:r><w:r>{r}</w:r></w:p>"#
        ));
        notes.push_str(&format!(
            r#"<w:footnote w:id="{i}"><w:p><w:r><w:t>note {i}</w:t></w:r></w:p></w:footnote>"#
        ));
    }
    let doc = format!(r#"<w:document {W}><w:body>{body}</w:body></w:document>"#);
    let foot = format!(r#"<w:footnotes {W}>{notes}</w:footnotes>"#);
    container(&[("word/document.xml", &doc), ("word/footnotes.xml", &foot)])
}

// The scan of the used notes that a set replaced made each reference cost as much as all
// the references before it: 100,000 references took about ten times as long as the same
// document without them. The ratio to that baseline, the faster of two runs each, holds
// on a loaded machine where a wall-clock bound does not.
#[test]
fn many_footnote_references_convert_quickly() {
    let l = Limits {
        max_text: 64 << 20,
        ..lim()
    };
    let (with, without) = (footnoted(100_000, true), footnoted(100_000, false));
    let d = to_markdown(&with, Format::Docx, &l).unwrap();
    assert!(d.markdown.contains("[^fn100000]: note 100000"));
    let time = |b: &[u8]| {
        (0..2)
            .map(|_| {
                let t = std::time::Instant::now();
                to_markdown(b, Format::Docx, &l).unwrap();
                t.elapsed()
            })
            .min()
            .unwrap()
    };
    let (tw, t0) = (time(&with), time(&without));
    assert!(tw < t0 * 4, "with references {tw:?}, without {t0:?}");
}

// Cells set out of column order are appended, not inserted, and a row is sorted once
// when it is added. Inserting each cell at the front of the row was a memmove of the
// whole row per cell.
#[test]
fn cells_out_of_order_are_appended_then_sorted_once() {
    use std::borrow::Cow;
    let mut budget = crate::budget::Budget::new(&lim());
    let mut row: crate::sheet::Cells = Vec::new();
    for c in [5usize, 3, 4, 3, 0] {
        crate::sheet::set(&mut row, c, Cow::Owned(format!("v{c}")), &mut budget).unwrap();
    }
    let cols: Vec<u32> = row.iter().map(|(c, _)| *c).collect();
    assert_eq!(cols, [5, 3, 4, 3, 0], "set must append in call order");
    let mut b = crate::sheet::SheetBuilder::new(100);
    b.row(row, 1, &mut budget).unwrap();
    let sh = b.finish("S", 1, 1 << 20, &mut budget).unwrap().unwrap();
    assert_eq!(sh.csv, "v0,,,v3,v4,v5\n");
}
