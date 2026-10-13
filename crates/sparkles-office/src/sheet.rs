//! Rows of cells collected from a sheet, and their CSV.

use crate::{Error, Sheet};

/// The widest sheet read: Excel's own limit of 16,384 columns.
pub const MAX_COLUMNS: usize = 16_384;

pub struct SheetBuilder {
    rows: Vec<Vec<String>>,
    cells: usize,
    max_cells: usize,
}

impl SheetBuilder {
    pub fn new(max_cells: usize) -> SheetBuilder {
        SheetBuilder {
            rows: Vec::new(),
            cells: 0,
            max_cells,
        }
    }

    /// Add a row. Trailing empty cells are dropped, and a row with no cell is skipped.
    pub fn row(&mut self, mut cells: Vec<String>) -> Result<(), Error> {
        while cells.last().is_some_and(String::is_empty) {
            cells.pop();
        }
        if cells.is_empty() {
            return Ok(());
        }
        self.cells += cells.iter().filter(|c| !c.is_empty()).count();
        if self.cells > self.max_cells {
            return Err(Error::TooLarge(format!(
                "a sheet has more than {} cells",
                self.max_cells
            )));
        }
        self.rows.push(cells);
        Ok(())
    }

    /// The sheet as CSV, without the columns that are empty at its left, or `None` when it
    /// holds no cell.
    pub fn finish(self, name: &str, index: usize, max_csv: usize) -> Result<Option<Sheet>, Error> {
        if self.rows.is_empty() {
            return Ok(None);
        }
        let first = self
            .rows
            .iter()
            .filter_map(|r| r.iter().position(|c| !c.is_empty()))
            .min()
            .unwrap_or(0);
        let width = self.rows.iter().map(Vec::len).max().unwrap_or(0) - first;
        let mut csv = String::new();
        for r in &self.rows {
            for c in 0..width {
                if c > 0 {
                    csv.push(',');
                }
                field(r.get(first + c).map_or("", String::as_str), &mut csv);
            }
            csv.push('\n');
            if csv.len() > max_csv {
                return Err(Error::TooLarge(format!(
                    "the sheet {name:?} is longer than {max_csv} bytes as CSV"
                )));
            }
        }
        Ok(Some(Sheet {
            name: name.to_string(),
            index,
            csv,
            rows: self.rows.len(),
            columns: width,
        }))
    }
}

fn field(s: &str, out: &mut String) {
    if s.contains([',', '"', '\n', '\r']) || s.starts_with(' ') || s.ends_with(' ') {
        out.push('"');
        for c in s.chars() {
            if c == '"' {
                out.push('"');
            }
            out.push(c);
        }
        out.push('"');
    } else {
        out.push_str(s);
    }
}

/// Put `value` in column `col` of a row, growing it.
pub fn set(row: &mut Vec<String>, col: usize, value: String) {
    if col >= MAX_COLUMNS {
        return;
    }
    if row.len() <= col {
        row.resize(col + 1, String::new());
    }
    row[col] = value;
}

/// The 0-based column of an A1 reference such as `AB12`.
pub fn column_of(reference: &str) -> Option<usize> {
    let mut col = 0usize;
    let mut any = false;
    for c in reference.chars() {
        if c.is_ascii_alphabetic() {
            col = col
                .checked_mul(26)?
                .checked_add(c.to_ascii_uppercase() as usize - 'A' as usize + 1)?;
            any = true;
        } else {
            break;
        }
    }
    (any && col > 0).then(|| col - 1)
}

/// The date or date and time of an Excel serial number, in ISO 8601, or `None` when the
/// number is out of range.
pub fn excel_date(serial: f64, date1904: bool) -> Option<String> {
    if !serial.is_finite() || !(0.0..2_958_466.0).contains(&serial) {
        return None;
    }
    let mut days = serial.floor() as i64;
    let mut secs = ((serial - serial.floor()) * 86_400.0).round() as i64;
    if secs >= 86_400 {
        days += 1;
        secs -= 86_400;
    }
    let unix = if date1904 {
        days - 24_107
    } else if days >= 61 {
        days - 25_569
    } else {
        days - 25_568
    };
    let time = format!("{:02}:{:02}:{:02}", secs / 3600, secs / 60 % 60, secs % 60);
    if serial < 1.0 {
        return Some(time);
    }
    let (y, m, d) = civil(unix);
    let date = format!("{y:04}-{m:02}-{d:02}");
    Some(if secs == 0 {
        date
    } else {
        format!("{date}T{time}")
    })
}

/// The proleptic Gregorian date of a day count from 1970-01-01.
fn civil(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}
