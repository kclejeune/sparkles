//! Parquet sources (spec F19 §4.3), behind the `mapping-parquet` feature. A file is
//! read with the `parquet` crate's row reader, one row group at a time, projected to the
//! referenced top-level columns. A reference is a column name, or a dotted path into a
//! struct column. Values map to literals as R2RML's natural mapping maps the matching
//! SQL types, and a list column gives one value per element.

use super::{Cell, Dt, Row, RowSource, canonical_decimal, canonical_double, check_file};
use crate::error::{Error, Result};
use crate::mapping::RunOptions;
use parquet::basic::LogicalType;
use parquet::data_type::Decimal;
use parquet::file::reader::{FileReader, SerializedFileReader};
use parquet::record::{Field, Row as PRow};
use parquet::schema::types::Type;
use std::borrow::Cow;
use std::path::PathBuf;
use std::sync::Arc;

pub(super) struct ParquetSource {
    path: PathBuf,
    name: String,
    refs: Vec<String>,
    opts: RunOptions,
}

impl ParquetSource {
    pub(super) fn new(
        path: PathBuf,
        name: &str,
        refs: &[String],
        opts: &RunOptions,
    ) -> Result<ParquetSource> {
        check_file(&path, name)?;
        Ok(ParquetSource {
            path,
            name: name.to_string(),
            refs: refs.to_vec(),
            opts: opts.clone(),
        })
    }
}

/// A reference resolved against the projected schema: the top-level column, the path
/// inside it, and whether a timestamp is in UTC.
struct Ref {
    top: usize,
    rest: Vec<String>,
    utc: bool,
}

struct PqRow<'r> {
    fields: Vec<&'r Field>,
    refs: &'r [Ref],
    group: usize,
    row: usize,
    name: &'r str,
}

fn days_to_date(days: i64) -> String {
    // Howard Hinnant's civil-from-days
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    if y < 0 {
        format!("-{:04}-{m:02}-{d:02}", -y)
    } else {
        format!("{y:04}-{m:02}-{d:02}")
    }
}

fn time_of_day(micros: i64) -> String {
    let s = micros.div_euclid(1_000_000);
    let frac = micros.rem_euclid(1_000_000);
    let (h, m, s) = (s / 3600, (s / 60) % 60, s % 60);
    if frac == 0 {
        format!("{h:02}:{m:02}:{s:02}")
    } else {
        let f = format!("{frac:06}");
        format!("{h:02}:{m:02}:{s:02}.{}", f.trim_end_matches('0'))
    }
}

fn timestamp(micros: i64, utc: bool) -> String {
    let days = micros.div_euclid(86_400_000_000);
    let rest = micros.rem_euclid(86_400_000_000);
    format!(
        "{}T{}{}",
        days_to_date(days),
        time_of_day(rest),
        if utc { "Z" } else { "" }
    )
}

fn decimal(d: &Decimal) -> String {
    let bytes = d.data();
    let mut v: i128 = if bytes.first().is_some_and(|b| b & 0x80 != 0) {
        -1
    } else {
        0
    };
    for b in bytes {
        v = (v << 8) | i128::from(*b);
    }
    let scale = d.scale().max(0) as usize;
    let neg = v < 0;
    let digits = v.unsigned_abs().to_string();
    let s = if scale == 0 {
        digits
    } else if digits.len() > scale {
        format!(
            "{}.{}",
            &digits[..digits.len() - scale],
            &digits[digits.len() - scale..]
        )
    } else {
        format!("0.{}{digits}", "0".repeat(scale - digits.len()))
    };
    let s = if neg { format!("-{s}") } else { s };
    canonical_decimal(&s).unwrap_or(s)
}

fn push<'a>(f: &'a Field, utc: bool, out: &mut Vec<Cell<'a>>, top: bool) {
    let owned = |s: String, dt: Dt| Cell {
        lex: Cow::Owned(s),
        dt,
    };
    let c = match f {
        Field::Null => return,
        Field::Bool(b) => Cell {
            lex: Cow::Borrowed(if *b { "true" } else { "false" }),
            dt: Dt::Boolean,
        },
        Field::Byte(x) => owned(x.to_string(), Dt::Integer),
        Field::Short(x) => owned(x.to_string(), Dt::Integer),
        Field::Int(x) => owned(x.to_string(), Dt::Integer),
        Field::Long(x) => owned(x.to_string(), Dt::Integer),
        Field::UByte(x) => owned(x.to_string(), Dt::Integer),
        Field::UShort(x) => owned(x.to_string(), Dt::Integer),
        Field::UInt(x) => owned(x.to_string(), Dt::Integer),
        Field::ULong(x) => owned(x.to_string(), Dt::Integer),
        Field::Float16(x) => owned(canonical_double(f64::from(*x)), Dt::Double),
        Field::Float(x) => owned(canonical_double(f64::from(*x)), Dt::Double),
        Field::Double(x) => owned(canonical_double(*x), Dt::Double),
        Field::Decimal(d) => owned(decimal(d), Dt::Decimal),
        Field::Str(s) => Cell::str(s),
        Field::Bytes(b) => owned(
            b.data().iter().map(|x| format!("{x:02X}")).collect(),
            Dt::HexBinary,
        ),
        Field::Date(d) => owned(days_to_date(i64::from(*d)), Dt::Date),
        Field::TimeMillis(t) => owned(time_of_day(i64::from(*t) * 1000), Dt::Time),
        Field::TimeMicros(t) => owned(time_of_day(*t), Dt::Time),
        Field::TimestampMillis(t) => owned(timestamp(t * 1000, utc), Dt::DateTime),
        Field::TimestampMicros(t) => owned(timestamp(*t, utc), Dt::DateTime),
        Field::ListInternal(l) if top => {
            for e in l.elements() {
                push(e, utc, out, false);
            }
            return;
        }
        Field::Group(_) | Field::ListInternal(_) | Field::MapInternal(_) => {
            owned(f.to_string(), Dt::Str)
        }
    };
    out.push(c);
}

fn walk<'a>(f: &'a Field, path: &[String]) -> Option<&'a Field> {
    let Some((first, rest)) = path.split_first() else {
        return Some(f);
    };
    match f {
        Field::Group(r) => {
            let (_, child) = r.get_column_iter().find(|(n, _)| *n == first)?;
            walk(child, rest)
        }
        _ => None,
    }
}

impl Row for PqRow<'_> {
    fn get<'a>(&'a self, r: usize, out: &mut Vec<Cell<'a>>) -> Result<()> {
        let rf = &self.refs[r];
        if let Some(f) = self.fields.get(rf.top).and_then(|f| walk(f, &rf.rest)) {
            push(f, rf.utc, out, true);
        }
        Ok(())
    }

    fn position(&self) -> String {
        format!(
            "{}, row group {}, row {}",
            self.name,
            self.group + 1,
            self.row + 1
        )
    }
}

impl RowSource for ParquetSource {
    fn scan(&mut self, f: &mut dyn FnMut(&dyn Row) -> Result<()>) -> Result<()> {
        let pq = |e: parquet::errors::ParquetError| Error::invalid(format!("{}: {e}", self.name));
        let file = std::fs::File::open(&self.path)
            .map_err(|e| Error::invalid(format!("{}: {e}", self.name)))?;
        let reader = SerializedFileReader::new(file).map_err(pq)?;
        let schema = reader.metadata().file_metadata().schema();
        let all = schema.get_fields();
        // the referenced top-level columns, in schema order
        let mut tops: Vec<usize> = Vec::new();
        let mut want: Vec<(usize, Vec<String>)> = Vec::new();
        for r in &self.refs {
            let (top, rest) = match all.iter().position(|t| t.name() == r) {
                Some(i) => (i, Vec::new()),
                None => {
                    let mut parts = r.split('.');
                    let first = parts.next().unwrap_or_default();
                    let i = all.iter().position(|t| t.name() == first).ok_or_else(|| {
                        Error::invalid(format!(
                            "{}: there is no column {r:?}; the columns are {}",
                            self.name,
                            all.iter()
                                .map(|t| format!("{:?}", t.name()))
                                .collect::<Vec<_>>()
                                .join(", ")
                        ))
                    })?;
                    (i, parts.map(str::to_string).collect())
                }
            };
            if !tops.contains(&top) {
                tops.push(top);
            }
            want.push((top, rest));
        }
        tops.sort_unstable();
        let fields: Vec<Arc<Type>> = tops.iter().map(|&i| all[i].clone()).collect();
        let utc_of = |t: &Type| {
            !t.is_group()
                && matches!(
                    t.get_basic_info().logical_type_ref(),
                    Some(LogicalType::Timestamp(ts)) if ts.is_adjusted_to_u_t_c
                )
        };
        let refs: Vec<Ref> = want
            .into_iter()
            .map(|(top, rest)| Ref {
                top: tops.iter().position(|&t| t == top).unwrap_or(0),
                utc: utc_of(&all[top]),
                rest,
            })
            .collect();
        let projection = Type::group_type_builder(schema.name())
            .with_fields(fields)
            .build()
            .map_err(pq)?;
        let mut n = 0u64;
        for g in 0..reader.num_row_groups() {
            let rg = reader.get_row_group(g).map_err(pq)?;
            let rows = rg.get_row_iter(Some(projection.clone())).map_err(pq)?;
            for (k, row) in rows.enumerate() {
                let row: PRow = row.map_err(pq)?;
                n += 1;
                if n.is_multiple_of(65_536)
                    && let Some(c) = &self.opts.check
                {
                    c()?;
                }
                let p = PqRow {
                    fields: row.get_column_iter().map(|(_, f)| f).collect(),
                    refs: &refs,
                    group: g,
                    row: k,
                    name: &self.name,
                };
                f(&p)?;
            }
        }
        Ok(())
    }
}
