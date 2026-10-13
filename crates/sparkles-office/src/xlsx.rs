//! Excel workbooks as one CSV text per visible sheet.
//!
//! Sheets are read as a stream, so a large sheet never becomes a tree. A formula cell
//! gives its cached value. A cell whose number format is a date or a time gives an
//! ISO 8601 date or time, with the workbook's 1900 or 1904 date system, because a
//! mapping that sees a float cannot tell a date from a count. Hidden and very hidden
//! sheets and chart sheets are left out, and so are sheets without a cell.

use crate::package::Package;
use crate::sheet::{self, SheetBuilder};
use crate::xml;
use crate::{Error, Limits, Sheet};
use quick_xml::events::Event;
use std::collections::HashMap;

pub fn sheets(pkg: &mut Package, limits: &Limits) -> Result<Vec<Sheet>, Error> {
    let wb = pkg.need("xl/workbook.xml")?;
    let wb = xml::parse(&wb, limits)?;
    let rels = pkg.rels("xl/workbook.xml");
    let date1904 = wb
        .find("workbookPr")
        .and_then(|n| n.attr("date1904"))
        .is_some_and(|v| v == "1" || v == "true");
    let dates = date_styles(pkg, limits);
    let shared = shared_strings(pkg, limits)?;
    let mut out = Vec::new();
    for (i, s) in wb.descendants("sheet").into_iter().enumerate() {
        let name = s.attr("name").unwrap_or("").to_string();
        if matches!(s.attr("state"), Some("hidden" | "veryHidden")) {
            continue;
        }
        let Some(rel) = s.attr("r:id").and_then(|id| rels.get(id)) else {
            continue;
        };
        if rel.kind != "worksheet" || rel.external {
            continue;
        }
        let Some(bytes) = pkg.read(&rel.target)? else {
            continue;
        };
        let b = read_sheet(&bytes, &shared, &dates, date1904, limits)?;
        if let Some(sh) = b.finish(&name, i + 1, limits.max_csv)? {
            out.push(sh);
        }
    }
    Ok(out)
}

/// The text of each shared string.
fn shared_strings(pkg: &mut Package, limits: &Limits) -> Result<Vec<String>, Error> {
    let Some(bytes) = pkg.read("xl/sharedStrings.xml")? else {
        return Ok(Vec::new());
    };
    let mut r = xml::reader(&bytes);
    let mut buf = Vec::new();
    let mut out: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut in_t = false;
    let mut skip = 0usize; // inside rPh, the phonetic runs
    let mut total = 0usize;
    loop {
        match r.read_event_into(&mut buf).map_err(corrupt)? {
            Event::Start(e) => match xml::local(e.name().as_ref()).as_str() {
                "si" => cur.clear(),
                "rPh" => skip += 1,
                "t" => in_t = skip == 0,
                _ => {}
            },
            Event::End(e) => match xml::local(e.name().as_ref()).as_str() {
                "si" => {
                    total += cur.len();
                    if total > limits.max_part {
                        return Err(Error::TooLarge("the shared strings are too long".into()));
                    }
                    out.push(std::mem::take(&mut cur));
                }
                "rPh" => skip = skip.saturating_sub(1),
                "t" => in_t = false,
                _ => {}
            },
            Event::Text(t) if in_t => cur.push_str(&xml::text_of(&t)),
            Event::CData(c) if in_t => cur.push_str(&String::from_utf8_lossy(&c)),
            Event::DocType(_) => return Err(corrupt("a DOCTYPE is not allowed")),
            Event::Eof => break,
            _ => {}
        }
        buf.clear();
    }
    Ok(out)
}

fn corrupt(e: impl std::fmt::Display) -> Error {
    Error::Corrupt(format!("the XML cannot be read: {e}"))
}

/// For each cell format (`xf`) of `styles.xml`, whether it shows a date or a time.
fn date_styles(pkg: &mut Package, limits: &Limits) -> Vec<bool> {
    let Ok(Some(b)) = pkg.read("xl/styles.xml") else {
        return Vec::new();
    };
    let Ok(root) = xml::parse(&b, limits) else {
        return Vec::new();
    };
    let mut custom: HashMap<u32, String> = HashMap::new();
    for f in root.descendants("numFmt") {
        if let (Some(id), Some(code)) = (
            f.attr("numFmtId").and_then(|v| v.parse().ok()),
            f.attr("formatCode"),
        ) {
            custom.insert(id, code.to_string());
        }
    }
    let Some(xfs) = root.first("cellXfs") else {
        return Vec::new();
    };
    xfs.elements()
        .filter(|n| n.name == "xf")
        .map(|xf| {
            let id: u32 = xf
                .attr("numFmtId")
                .and_then(|v| v.parse().ok())
                .unwrap_or(0);
            match custom.get(&id) {
                Some(code) => is_date_format(code),
                None => matches!(id, 14..=22 | 27..=36 | 45..=47 | 50..=58),
            }
        })
        .collect()
}

/// Whether a number format code shows dates or times: it has a date or time letter
/// outside quotes, escapes and brackets other than elapsed-time brackets.
pub fn is_date_format(code: &str) -> bool {
    let mut chars = code.chars().peekable();
    let mut date = false;
    while let Some(c) = chars.next() {
        match c {
            '"' => {
                for q in chars.by_ref() {
                    if q == '"' {
                        break;
                    }
                }
            }
            '\\' | '_' | '*' => {
                chars.next();
            }
            '[' => {
                let mut inner = String::new();
                for q in chars.by_ref() {
                    if q == ']' {
                        break;
                    }
                    inner.push(q);
                }
                let l = inner.to_ascii_lowercase();
                if !l.is_empty() && l.chars().all(|c| matches!(c, 'h' | 'm' | 's')) {
                    date = true;
                }
            }
            'y' | 'Y' | 'd' | 'D' | 'h' | 'H' | 's' | 'S' | 'm' | 'M' => date = true,
            _ => {}
        }
    }
    date
}

fn read_sheet(
    bytes: &[u8],
    shared: &[String],
    dates: &[bool],
    date1904: bool,
    limits: &Limits,
) -> Result<SheetBuilder, Error> {
    #[derive(PartialEq)]
    enum Target {
        None,
        Value,
        Inline,
    }
    let mut b = SheetBuilder::new(limits.max_cells);
    let mut r = xml::reader(bytes);
    let mut buf = Vec::new();
    let mut row: Vec<String> = Vec::new();
    let mut next_col = 0usize;
    let mut col = 0usize;
    let mut kind = String::new();
    let mut style = 0usize;
    let mut text = String::new();
    let mut target = Target::None;
    let mut in_cell = false;
    loop {
        let ev = r.read_event_into(&mut buf).map_err(corrupt)?;
        match &ev {
            Event::Start(e) | Event::Empty(e) => {
                let empty = matches!(ev, Event::Empty(_));
                match xml::local(e.name().as_ref()).as_str() {
                    "row" => {
                        row.clear();
                        next_col = 0;
                    }
                    "c" => {
                        let at = xml::attrs(e);
                        let get =
                            |k: &str| at.iter().find(|(n, _)| n == k).map(|(_, v)| v.as_str());
                        col = get("r").and_then(sheet::column_of).unwrap_or(next_col);
                        next_col = col + 1;
                        kind = get("t").unwrap_or("n").to_string();
                        style = get("s").and_then(|v| v.parse().ok()).unwrap_or(0);
                        text.clear();
                        in_cell = true;
                        target = Target::None;
                        if empty {
                            in_cell = false;
                        }
                    }
                    "v" if in_cell && !empty => target = Target::Value,
                    "t" if in_cell && !empty => target = Target::Inline,
                    _ => {}
                }
            }
            Event::Text(t) if target != Target::None => text.push_str(&xml::text_of(t)),
            Event::CData(c) if target != Target::None => text.push_str(&String::from_utf8_lossy(c)),
            Event::End(e) => match xml::local(e.name().as_ref()).as_str() {
                "v" | "t" => target = Target::None,
                "c" if in_cell => {
                    in_cell = false;
                    let v = cell_value(&kind, &text, style, shared, dates, date1904);
                    if !v.is_empty() {
                        sheet::set(&mut row, col, v);
                    }
                }
                "row" => b.row(std::mem::take(&mut row))?,
                _ => {}
            },
            Event::DocType(_) => return Err(corrupt("a DOCTYPE is not allowed")),
            Event::Eof => break,
            _ => {}
        }
        buf.clear();
    }
    Ok(b)
}

fn cell_value(
    kind: &str,
    text: &str,
    style: usize,
    shared: &[String],
    dates: &[bool],
    date1904: bool,
) -> String {
    match kind {
        "s" => text
            .trim()
            .parse::<usize>()
            .ok()
            .and_then(|i| shared.get(i))
            .cloned()
            .unwrap_or_default(),
        "b" => if text.trim() == "1" { "TRUE" } else { "FALSE" }.to_string(),
        "str" | "inlineStr" | "e" | "d" => text.to_string(),
        _ => {
            let t = text.trim();
            if dates.get(style).copied().unwrap_or(false)
                && let Ok(n) = t.parse::<f64>()
                && let Some(d) = sheet::excel_date(n, date1904)
            {
                return d;
            }
            t.to_string()
        }
    }
}
