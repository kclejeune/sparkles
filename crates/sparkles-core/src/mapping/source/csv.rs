//! CSV and TSV sources (spec F19 §4.1), read with C05's reader and dialects. A
//! reference is a column title of the header. An empty cell is NULL, as are the null
//! values of a CSVW description. A CSVW metadata file next to the table
//! (`orders.csv-metadata.json`) types its columns, as C05 finds one for `sparkles load`.

use super::{Cell, Dt, Row, RowSource, check_file};
use crate::error::{Error, Result};
use crate::mapping::{Inputs, RunOptions};
use crate::tabular::csvw::DialectDesc;
use crate::tabular::{self, RowReader, TypedColumns};
use std::borrow::Cow;
use std::path::PathBuf;

pub(super) struct CsvSource {
    path: PathBuf,
    name: String,
    tsv: bool,
    dialect: DialectDesc,
    null: Vec<String>,
    refs: Vec<String>,
    metadata: Option<tabular::csvw::TableDesc>,
    opts: RunOptions,
}

impl CsvSource {
    #[allow(clippy::too_many_arguments)]
    pub(super) fn new(
        path: PathBuf,
        name: &str,
        tsv: bool,
        dialect: DialectDesc,
        null: Vec<String>,
        refs: &[String],
        inputs: &Inputs,
        opts: &RunOptions,
    ) -> Result<CsvSource> {
        check_file(&path, name)?;
        let metadata = match inputs.optional_file(&format!("{name}-metadata.json")) {
            Some(p) => {
                let m = tabular::read_metadata(&p)?;
                m.tables.into_iter().next()
            }
            None => None,
        };
        // the description's dialect, under the mapping's own
        let dialect = match &metadata {
            Some(t) => DialectDesc {
                delimiter: dialect.delimiter.or(t.dialect.delimiter),
                quote: dialect.quote.or(t.dialect.quote),
                double_quote: dialect.double_quote.or(t.dialect.double_quote),
                header: dialect.header.or(t.dialect.header),
                header_rows: dialect.header_rows.or(t.dialect.header_rows),
                skip_rows: dialect.skip_rows.or(t.dialect.skip_rows),
                skip_columns: dialect.skip_columns.or(t.dialect.skip_columns),
                skip_blank_rows: dialect.skip_blank_rows.or(t.dialect.skip_blank_rows),
                skip_initial_space: dialect.skip_initial_space.or(t.dialect.skip_initial_space),
                comment: dialect.comment.or(t.dialect.comment),
                trim: dialect.trim.or(t.dialect.trim),
            },
            None => dialect,
        };
        Ok(CsvSource {
            path,
            name: name.to_string(),
            tsv,
            dialect,
            null,
            refs: refs.to_vec(),
            metadata,
            opts: opts.clone(),
        })
    }
}

struct CsvRow<'r, R: std::io::Read> {
    reader: &'r RowReader<R>,
    idx: &'r [usize],
    typed: Option<&'r TypedColumns>,
    null: &'r [String],
    row: u64,
    name: &'r str,
}

impl<R: std::io::Read> Row for CsvRow<'_, R> {
    fn get<'a>(&'a self, r: usize, out: &mut Vec<Cell<'a>>) -> Result<()> {
        let i = self.idx[r];
        let Some(raw) = self.reader.field(i) else {
            return Ok(());
        };
        match self.typed {
            Some(t) => {
                let lits = t.cell(i, raw).map_err(|m| {
                    Error::invalid(format!(
                        "{}: row {}, column {}: {m}",
                        self.name,
                        self.row,
                        self.reader.header.get(i).map_or("", String::as_str)
                    ))
                })?;
                for l in lits {
                    let dt = Dt::of_literal(&l);
                    // decimals as R2RML's natural mapping writes them, with a fraction
                    let lex = match dt {
                        Dt::Decimal => super::canonical_decimal(l.value())
                            .unwrap_or_else(|| l.value().to_string()),
                        _ => l.value().to_string(),
                    };
                    out.push(Cell {
                        lex: Cow::Owned(lex),
                        dt,
                    });
                }
            }
            None => {
                if raw.is_empty() || self.null.iter().any(|n| n == raw) {
                    return Ok(());
                }
                out.push(Cell::str(raw));
            }
        }
        Ok(())
    }

    fn position(&self) -> String {
        format!("{}, row {}", self.name, self.row)
    }
}

impl RowSource for CsvSource {
    fn scan(&mut self, f: &mut dyn FnMut(&dyn Row) -> Result<()>) -> Result<()> {
        let input = tabular::open(&self.path, self.opts.max_decompressed)?;
        let mut reader = RowReader::new(
            input,
            self.tsv,
            &self.dialect,
            &self.name,
            &tabular::Limits::default(),
        )?;
        let header = reader.header.clone();
        let mut idx = Vec::with_capacity(self.refs.len());
        for r in &self.refs {
            let i = header
                .iter()
                .position(|h| h == r)
                .or_else(|| {
                    // a CSVW description may name a column differently from its title
                    self.metadata.as_ref().and_then(|m| {
                        m.columns
                            .iter()
                            .filter(|c| !c.is_virtual)
                            .position(|c| c.name.as_deref() == Some(r.as_str()))
                    })
                })
                .ok_or_else(|| {
                    Error::invalid(format!(
                        "{}: there is no column {r:?}; the columns are {}",
                        self.name,
                        header
                            .iter()
                            .map(|h| format!("{h:?}"))
                            .collect::<Vec<_>>()
                            .join(", ")
                    ))
                })?;
            idx.push(i);
        }
        let typed = match &self.metadata {
            Some(m) => Some(
                TypedColumns::new(m, &header)
                    .map_err(|e| Error::invalid(format!("{}-metadata.json: {e}", self.name)))?,
            ),
            None => None,
        };
        let mut n = 0u64;
        while let Some(row) = reader.next()? {
            n += 1;
            if n.is_multiple_of(65_536)
                && let Some(c) = &self.opts.check
            {
                c()?;
            }
            let r = CsvRow {
                reader: &reader,
                idx: &idx,
                typed: typed.as_ref(),
                null: &self.null,
                row,
                name: &self.name,
            };
            f(&r)?;
        }
        Ok(())
    }
}
