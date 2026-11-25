//! RFC 4180 CSV/TSV parser with source line tracking and strict resource bounds.

use super::{CancelReason, EvalError, Limits, RowDiagnostic, Severity};

/// Field delimiter for tabular inputs.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Delimiter {
    /// Detect tab vs comma from the header line.
    #[default]
    Auto,
    Comma,
    Tab,
}

impl Delimiter {
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "auto" => Some(Self::Auto),
            "csv" | "comma" | "," => Some(Self::Comma),
            "tsv" | "tab" | "\t" => Some(Self::Tab),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Comma => "csv",
            Self::Tab => "tsv",
        }
    }
}

/// One parsed data row with its 1-based line number in the source document.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TableRow {
    /// 1-based line number where this record started in the source text.
    pub line: usize,
    /// 1-based data row index (first row after the header is 1).
    pub row_number: usize,
    pub cells: Vec<String>,
}

/// A parsed spreadsheet table.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Table {
    pub delimiter: Delimiter,
    pub headers: Vec<String>,
    pub rows: Vec<TableRow>,
}

/// Error returned when a table cannot be parsed or exceeds structural bounds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TableError {
    pub code: &'static str,
    pub message: String,
    pub line: Option<usize>,
}

impl std::fmt::Display for TableError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.line {
            Some(line) => write!(f, "line {line}: {}", self.message),
            None => f.write_str(&self.message),
        }
    }
}

impl std::error::Error for TableError {}

/// Detect whether the first non-blank logical header record uses tabs or commas outside
/// quoted fields.
fn detect_delimiter(input: &str) -> Delimiter {
    let mut chars = input.chars().peekable();
    while chars.peek().is_some() {
        let mut in_quotes = false;
        let mut tabs = 0usize;
        let mut commas = 0usize;
        let mut has_content = false;
        while let Some(c) = chars.next() {
            match c {
                '"' => {
                    if in_quotes && chars.peek() == Some(&'"') {
                        chars.next();
                        has_content = true;
                    } else {
                        in_quotes = !in_quotes;
                    }
                }
                '\r' if !in_quotes => {
                    if chars.peek() == Some(&'\n') {
                        chars.next();
                    }
                    break;
                }
                '\n' if !in_quotes => break,
                '\t' if !in_quotes => tabs += 1,
                ',' if !in_quotes => commas += 1,
                other => {
                    if !other.is_whitespace() {
                        has_content = true;
                    }
                }
            }
        }
        if has_content {
            return if tabs > commas {
                Delimiter::Tab
            } else {
                Delimiter::Comma
            };
        }
    }
    Delimiter::Comma
}

/// Parse CSV or TSV text into a [`Table`], collecting non-fatal warnings in `diagnostics`.
pub fn parse_table(
    raw: &str,
    delimiter: Delimiter,
    limits: &Limits,
    diagnostics: &mut Vec<RowDiagnostic>,
) -> Result<Table, TableError> {
    let mut truncated = false;
    let mut noop = || Ok(());
    match parse_table_with_options(
        raw,
        delimiter,
        limits,
        0,
        diagnostics,
        &mut truncated,
        &mut noop,
    ) {
        Ok(t) => Ok(t),
        Err(EvalError::Table(e)) => Err(e),
        Err(EvalError::Cancelled | EvalError::Timeout) => unreachable!(),
    }
}

/// Internal entry point allowing an optional extra template row (when ROBOT
/// `header_template_row` is true), forwarding warning truncation, and polling cancellation.
pub(crate) fn parse_table_with_options(
    raw: &str,
    delimiter: Delimiter,
    limits: &Limits,
    extra_rows: usize,
    diagnostics: &mut Vec<RowDiagnostic>,
    diagnostics_truncated: &mut bool,
    check_cancel: &mut dyn FnMut() -> Result<(), CancelReason>,
) -> Result<Table, EvalError> {
    check_cancel()?;
    if raw.len() > limits.max_input_bytes {
        return Err(TableError {
            code: "input-too-large",
            message: format!(
                "table input is {} bytes, exceeding the {} byte limit",
                raw.len(),
                limits.max_input_bytes
            ),
            line: None,
        }
        .into());
    }
    let text = raw.strip_prefix('\u{feff}').unwrap_or(raw);
    let resolved = match delimiter {
        Delimiter::Auto => detect_delimiter(text),
        other => other,
    };
    let sep = match resolved {
        Delimiter::Tab => '\t',
        Delimiter::Comma | Delimiter::Auto => ',',
    };

    let records = parse_records(text, sep, limits, check_cancel)?;
    let mut iter = records.into_iter().filter(|(_, cells)| {
        // Ignore completely blank lines.
        cells.iter().any(|c| !c.trim().is_empty())
    });

    let Some((header_line, raw_headers)) = iter.next() else {
        return Err(TableError {
            code: "empty-table",
            message: "table has no header row".to_string(),
            line: None,
        }
        .into());
    };

    if raw_headers.len() > limits.max_cols {
        return Err(TableError {
            code: "too-many-columns",
            message: format!(
                "table has {} columns, exceeding the {} column limit",
                raw_headers.len(),
                limits.max_cols
            ),
            line: Some(header_line),
        }
        .into());
    }

    let headers: Vec<String> = raw_headers
        .into_iter()
        .map(|h| h.trim().to_string())
        .collect();
    if headers.iter().all(|h| h.is_empty()) {
        return Err(TableError {
            code: "empty-header",
            message: "header row contains no column names".to_string(),
            line: Some(header_line),
        }
        .into());
    }

    let mut seen_headers = rustc_hash::FxHashSet::default();
    for h in &headers {
        if !h.is_empty() && !seen_headers.insert(h.as_str()) {
            push_diag(
                diagnostics,
                limits.max_diagnostics,
                diagnostics_truncated,
                RowDiagnostic {
                    row: header_line,
                    column: Some(h.clone()),
                    severity: Severity::Warning,
                    code: "duplicate-header".to_string(),
                    message: format!("duplicate column header '{h}'"),
                },
            );
        }
    }

    let col_count = headers.len();
    let max_table_rows = limits.max_rows.saturating_add(extra_rows);
    let mut rows = Vec::new();
    for (idx, (line, mut cells)) in iter.enumerate() {
        if idx.is_multiple_of(256) {
            check_cancel()?;
        }
        let row_number = idx + 1;
        if row_number > max_table_rows {
            return Err(TableError {
                code: "too-many-rows",
                message: format!("table exceeds the {} row limit", limits.max_rows),
                line: Some(line),
            }
            .into());
        }
        if cells.len() > col_count {
            let extra = cells.len() - col_count;
            push_diag(
                diagnostics,
                limits.max_diagnostics,
                diagnostics_truncated,
                RowDiagnostic {
                    row: line,
                    column: None,
                    severity: Severity::Warning,
                    code: "extra-columns".to_string(),
                    message: format!(
                        "row has {} cells, {extra} more than the {} header columns; extra cells were ignored",
                        cells.len(),
                        col_count
                    ),
                },
            );
            cells.truncate(col_count);
        } else if cells.len() < col_count {
            cells.resize(col_count, String::new());
        }
        rows.push(TableRow {
            line,
            row_number,
            cells,
        });
    }

    Ok(Table {
        delimiter: resolved,
        headers,
        rows,
    })
}

fn push_diag(out: &mut Vec<RowDiagnostic>, max: usize, truncated: &mut bool, diag: RowDiagnostic) {
    if out.len() < max {
        out.push(diag);
    } else {
        *truncated = true;
    }
}

/// Low-level RFC 4180 record parser returning `(1-based start line, cells)`.
fn parse_records(
    text: &str,
    sep: char,
    limits: &Limits,
    check_cancel: &mut dyn FnMut() -> Result<(), CancelReason>,
) -> Result<Vec<(usize, Vec<String>)>, EvalError> {
    let mut records = Vec::new();
    let mut chars = text.chars().peekable();
    let mut line = 1usize;
    let mut char_ticks = 0usize;

    while chars.peek().is_some() {
        if records.len().is_multiple_of(256) {
            check_cancel()?;
        }
        let start_line = line;
        let mut row = Vec::new();
        loop {
            let mut field = String::new();
            if chars.peek() == Some(&'"') {
                chars.next();
                let mut closed = false;
                while let Some(c) = chars.next() {
                    char_ticks += 1;
                    if char_ticks.is_multiple_of(8192) {
                        check_cancel()?;
                    }
                    if c == '"' {
                        if chars.peek() == Some(&'"') {
                            chars.next();
                            field.push('"');
                        } else {
                            closed = true;
                            break;
                        }
                    } else {
                        if c == '\n' {
                            line += 1;
                        }
                        field.push(c);
                    }
                }
                if !closed {
                    return Err(TableError {
                        code: "unclosed-quote",
                        message: "unclosed quoted field".to_string(),
                        line: Some(start_line),
                    }
                    .into());
                }
                // Consume trailing spaces after closing quote before separator/EOL.
                while let Some(&c) = chars.peek() {
                    if c == sep || c == '\n' || c == '\r' {
                        break;
                    }
                    if c == ' ' {
                        chars.next();
                    } else {
                        return Err(TableError {
                            code: "invalid-quote",
                            message: format!("unexpected character '{c}' after closing quote"),
                            line: Some(line),
                        }
                        .into());
                    }
                }
            } else {
                while let Some(&c) = chars.peek() {
                    char_ticks += 1;
                    if char_ticks.is_multiple_of(8192) {
                        check_cancel()?;
                    }
                    if c == sep || c == '\n' || c == '\r' {
                        break;
                    }
                    field.push(c);
                    chars.next();
                }
            }

            if row.len() >= limits.max_cols * 2 {
                return Err(TableError {
                    code: "too-many-columns",
                    message: format!("row exceeds column limit ({})", limits.max_cols),
                    line: Some(start_line),
                }
                .into());
            }
            row.push(field);

            match chars.peek() {
                Some(&c) if c == sep => {
                    chars.next();
                }
                Some(&'\r') => {
                    chars.next();
                    if chars.peek() == Some(&'\n') {
                        chars.next();
                    }
                    line += 1;
                    break;
                }
                Some(&'\n') => {
                    chars.next();
                    line += 1;
                    break;
                }
                None => break,
                _ => break,
            }
        }
        records.push((start_line, row));
        // Hard cap on raw records including blank lines to bound work.
        if records.len() > limits.max_rows.saturating_mul(2).saturating_add(1024) {
            return Err(TableError {
                code: "too-many-rows",
                message: format!("table exceeds the {} row limit", limits.max_rows),
                line: Some(line),
            }
            .into());
        }
    }

    Ok(records)
}
