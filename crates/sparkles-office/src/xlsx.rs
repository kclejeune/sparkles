//! Excel workbooks as one CSV text per visible sheet.
//!
//! Sheets are read as a stream, so a large sheet never becomes a tree. A formula cell
//! gives its cached value. A cell whose number format is a date or a time gives an
//! ISO 8601 date or time, with the workbook's 1900 or 1904 date system, because a
//! mapping that sees a float cannot tell a date from a count. Hidden and very hidden
//! sheets and chart sheets are left out, and so are sheets without a cell.

use crate::budget::{Budget, SLOT};
use crate::package::Package;
use crate::sheet::{self, SheetBuilder};
use crate::xml;
use crate::{Error, Limits, Sheet};
use quick_xml::events::Event;
use std::borrow::Cow;
use std::collections::HashMap;

pub fn sheets(pkg: &mut Package, limits: &Limits) -> Result<Vec<Sheet>, Error> {
    let wb = pkg.need("xl/workbook.xml")?;
    let wb = xml::parse(&wb, limits)?;
    let rels = pkg.rels("xl/workbook.xml");
    let date1904 = wb
        .find("workbookPr")
        .and_then(|n| n.attr("date1904"))
        .is_some_and(|v| v == "1" || v == "true");
    let mut budget = Budget::new(limits);
    let dates = date_styles(pkg, limits, &mut budget)?;
    let shared = shared_strings(pkg, limits, &mut budget)?;
    let mut out = Vec::new();
    for (i, s) in wb.descendants("sheet").into_iter().enumerate() {
        let name = s.attr("name").unwrap_or("").to_string();
        if matches!(s.attr("state"), Some("hidden" | "veryHidden")) {
            continue;
        }
        // the relationship id is `r:id` by habit, but any prefix may be bound to the namespace
        let rid = s.attr("r:id").or_else(|| {
            s.attrs
                .iter()
                .find(|(k, _)| k.rsplit_once(':').is_some_and(|(_, l)| l == "id"))
                .map(|(_, v)| v.as_str())
        });
        let Some(rel) = rid.and_then(|id| rels.get(id)) else {
            continue;
        };
        if rel.kind != "worksheet" || rel.external {
            continue;
        }
        let Some(bytes) = pkg.read(&rel.target)? else {
            continue;
        };
        let b = read_sheet(&bytes, &shared, &dates, date1904, limits, &mut budget)?;
        if let Some(sh) = b.finish(&name, i + 1, limits.max_csv, &mut budget)? {
            out.push(sh);
        }
    }
    Ok(out)
}

/// The text of each shared string.
fn shared_strings(
    pkg: &mut Package,
    limits: &Limits,
    budget: &mut Budget,
) -> Result<Vec<String>, Error> {
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
        budget.tick()?;
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
                    // the strings stay for the whole conversion, so they are charged
                    budget.charge(cur.len() + SLOT)?;
                    out.push(std::mem::take(&mut cur));
                }
                "rPh" => skip = skip.saturating_sub(1),
                "t" => in_t = false,
                _ => {}
            },
            // an empty `<si/>` is a string like any other, and the ones after it keep their index
            Event::Empty(e) if xml::local(e.name().as_ref()) == "si" => {
                budget.charge(SLOT)?;
                out.push(String::new());
            }
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

/// The longest number format code read. Real codes are a few dozen characters, and a
/// longer one is taken as a format that is not a date.
const MAX_FORMAT_CODE: usize = 512;

/// For each cell format (`xf`) of `styles.xml`, whether it shows a date or a time. Each
/// custom code is read once, however many cell formats name it.
fn date_styles(
    pkg: &mut Package,
    limits: &Limits,
    budget: &mut Budget,
) -> Result<Vec<bool>, Error> {
    let Ok(Some(b)) = pkg.read("xl/styles.xml") else {
        return Ok(Vec::new());
    };
    let root = match xml::parse(&b, limits) {
        Ok(r) => r,
        Err(Error::Timeout) => return Err(Error::Timeout),
        Err(_) => return Ok(Vec::new()),
    };
    drop(b);
    let mut custom: HashMap<u32, bool> = HashMap::new();
    for f in root.descendants("numFmt") {
        budget.tick()?;
        if let (Some(id), Some(code)) = (
            f.attr("numFmtId").and_then(|v| v.parse().ok()),
            f.attr("formatCode"),
        ) {
            custom.insert(id, code.len() <= MAX_FORMAT_CODE && is_date_format(code));
        }
    }
    let Some(xfs) = root.first("cellXfs") else {
        return Ok(Vec::new());
    };
    let mut out = Vec::new();
    for xf in xfs.elements().filter(|n| n.name == "xf") {
        budget.tick()?;
        let id: u32 = xf
            .attr("numFmtId")
            .and_then(|v| v.parse().ok())
            .unwrap_or(0);
        out.push(match custom.get(&id) {
            Some(date) => *date,
            None => matches!(id, 14..=22 | 27..=36 | 45..=47 | 50..=58),
        });
    }
    Ok(out)
}

/// Whether a number format code shows dates or times: it has a date or time letter
/// outside quotes, escapes and brackets other than elapsed-time brackets.
pub fn is_date_format(code: &str) -> bool {
    let mut chars = code.chars().peekable();
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
                    return true;
                }
            }
            'y' | 'Y' | 'd' | 'D' | 'h' | 'H' | 's' | 'S' | 'm' | 'M' => return true,
            _ => {}
        }
    }
    false
}

fn read_sheet(
    bytes: &[u8],
    shared: &[String],
    dates: &[bool],
    date1904: bool,
    limits: &Limits,
    budget: &mut Budget,
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
    let mut row: sheet::Cells = Vec::new();
    let mut next_col = 0usize;
    let mut col = 0usize;
    let mut kind = String::new();
    let mut style = 0usize;
    let mut text = String::new();
    let mut target = Target::None;
    let mut in_cell = false;
    let mut phonetic = 0usize; // inside rPh, the phonetic runs of an inline string
    loop {
        budget.tick()?;
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
                        let at = xml::attrs(e)?;
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
                    "rPh" if !empty => phonetic += 1,
                    "v" if in_cell && !empty => target = Target::Value,
                    "t" if in_cell && !empty && phonetic == 0 => target = Target::Inline,
                    _ => {}
                }
            }
            Event::Text(t) if target != Target::None => text.push_str(&xml::text_of(t)),
            Event::CData(c) if target != Target::None => text.push_str(&String::from_utf8_lossy(c)),
            Event::End(e) => match xml::local(e.name().as_ref()).as_str() {
                "v" | "t" => target = Target::None,
                "rPh" => phonetic = phonetic.saturating_sub(1),
                "c" if in_cell => {
                    in_cell = false;
                    let v = cell_value(&kind, &text, style, shared, dates, date1904);
                    if !v.is_empty() {
                        sheet::set(&mut row, col, v, budget)?;
                    }
                }
                "row" => b.row(std::mem::take(&mut row), 1, budget)?,
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

fn cell_value<'a>(
    kind: &str,
    text: &str,
    style: usize,
    shared: &'a [String],
    dates: &[bool],
    date1904: bool,
) -> Cow<'a, str> {
    // a shared string is borrowed, so that the budget can refuse it before it is copied
    if kind == "s" {
        return text
            .trim()
            .parse::<usize>()
            .ok()
            .and_then(|i| shared.get(i))
            .map_or(Cow::Borrowed(""), |s| Cow::Borrowed(s.as_str()));
    }
    Cow::Owned(match kind {
        "b" => if text.trim() == "1" { "TRUE" } else { "FALSE" }.to_string(),
        "str" | "inlineStr" | "e" | "d" => text.to_string(),
        _ => {
            let t = text.trim();
            if dates.get(style).copied().unwrap_or(false)
                && let Ok(n) = t.parse::<f64>()
                && let Some(d) = sheet::excel_date(n, date1904)
            {
                return Cow::Owned(d);
            }
            t.to_string()
        }
    })
}
