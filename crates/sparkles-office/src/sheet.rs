//! Rows of cells collected from a sheet, and their CSV.

use crate::budget::{Budget, SLOT};
use crate::{Error, Sheet};
use std::borrow::Cow;

/// The widest sheet read: Excel's own limit of 16,384 columns.
pub const MAX_COLUMNS: usize = 16_384;

/// The non-empty cells of a row as (column, value), sorted by column.
pub type Cells = Vec<(u32, String)>;

struct Row {
    cells: Cells,
    /// how many times the row occurs in a row, for `number-rows-repeated`
    repeat: u32,
}

pub struct SheetBuilder {
    rows: Vec<Row>,
    cells: usize,
    max_cells: usize,
    /// the first and the last column that holds a cell
    min_col: usize,
    max_col: usize,
    /// bytes charged to the budget for the stored cells
    charged: usize,
}

/// Put `value` in column `col` of a row. Columns past [`MAX_COLUMNS`] are ignored. The
/// value is charged to the budget before it is copied, and a value already in that
/// column (an XLSX row may name a column twice) is replaced and its charge given back.
pub fn set(
    row: &mut Cells,
    col: usize,
    value: Cow<'_, str>,
    budget: &mut Budget,
) -> Result<(), Error> {
    if col >= MAX_COLUMNS {
        return Ok(());
    }
    let col = col as u32;
    budget.charge(value.len() + SLOT)?;
    let value = value.into_owned();
    match row.last() {
        Some((c, _)) if *c < col => row.push((col, value)),
        None => row.push((col, value)),
        _ => match row.binary_search_by_key(&col, |(c, _)| *c) {
            Ok(i) => {
                let old = std::mem::replace(&mut row[i].1, value);
                budget.release(old.len() + SLOT);
            }
            Err(i) => row.insert(i, (col, value)),
        },
    }
    Ok(())
}

impl SheetBuilder {
    pub fn new(max_cells: usize) -> SheetBuilder {
        SheetBuilder {
            rows: Vec::new(),
            cells: 0,
            max_cells,
            min_col: usize::MAX,
            max_col: 0,
            charged: 0,
        }
    }

    /// Add a row that occurs `repeat` times. A row with no cell is skipped. The cells
    /// were charged to the budget as they were stored, and the row is kept once, not once
    /// per repeat.
    pub fn row(&mut self, mut cells: Cells, repeat: usize) -> Result<(), Error> {
        cells.retain(|(_, v)| !v.is_empty());
        let (Some(first), Some(last)) = (cells.first(), cells.last()) else {
            return Ok(());
        };
        let repeat = repeat.clamp(1, 1000);
        self.cells = self
            .cells
            .saturating_add(cells.len().saturating_mul(repeat));
        if self.cells > self.max_cells {
            return Err(Error::TooLarge(format!(
                "a sheet has more than {} cells",
                self.max_cells
            )));
        }
        self.min_col = self.min_col.min(first.0 as usize);
        self.max_col = self.max_col.max(last.0 as usize);
        self.charged += cells.iter().map(|(_, v)| v.len() + SLOT).sum::<usize>();
        self.rows.push(Row {
            cells,
            repeat: repeat as u32,
        });
        Ok(())
    }

    /// Drop the rows of a sheet that is not wanted and give their charge back.
    pub fn discard(self, budget: &mut Budget) {
        budget.release(self.charged);
    }

    /// The sheet as CSV, without the columns that are empty at its left, or `None` when it
    /// holds no cell. The CSV is charged to the budget as it is written and stays charged,
    /// because the caller keeps it. The rows are dropped here and their charge is given
    /// back.
    pub fn finish(
        self,
        name: &str,
        index: usize,
        max_csv: usize,
        budget: &mut Budget,
    ) -> Result<Option<Sheet>, Error> {
        if self.rows.is_empty() {
            return Ok(None);
        }
        let first = self.min_col;
        let width = self.max_col + 1 - first;
        let mut csv = String::new();
        let mut rows = 0usize;
        for r in &self.rows {
            let before = csv.len();
            let mut cells = r.cells.iter().peekable();
            for c in 0..width {
                if c > 0 {
                    csv.push(',');
                }
                if let Some((_, v)) = cells.next_if(|(col, _)| *col as usize == first + c) {
                    field(v, &mut csv);
                }
            }
            csv.push('\n');
            let line = csv.len() - before;
            for _ in 1..r.repeat {
                budget.charge(line)?;
                if csv.len() + line > max_csv {
                    return Err(too_long(name, max_csv));
                }
                csv.extend_from_within(before..before + line);
            }
            budget.charge(line)?;
            if csv.len() > max_csv {
                return Err(too_long(name, max_csv));
            }
            rows += r.repeat as usize;
        }
        budget.release(self.charged);
        // the doubling of the buffer would otherwise hold up to twice what was charged
        csv.shrink_to_fit();
        Ok(Some(Sheet {
            name: name.to_string(),
            index,
            csv,
            rows,
            columns: width,
        }))
    }
}

fn too_long(name: &str, max_csv: usize) -> Error {
    Error::TooLarge(format!(
        "the sheet {name:?} is longer than {max_csv} bytes as CSV"
    ))
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
