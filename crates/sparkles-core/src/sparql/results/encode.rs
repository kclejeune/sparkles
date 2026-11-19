//! The engine's own encoders of the SPARQL result formats. Each writes a row straight
//! into the output buffer through the explicit mode of [`RowOut`](super::RowOut), so a
//! row costs its bytes and one mark and no per-write check. Their bytes equal those of
//! sparesults, which stays the oracle of the tests.

use super::rowout::Explicit;
use super::source::RowView;
use oxrdf::vocab::xsd;
use oxrdf::{BaseDirection, TermRef};
use std::io::Result;

/// The tabs between the cells of a TSV row, written a slice at a time.
const TABS: &[u8; 32] = b"\t\t\t\t\t\t\t\t\t\t\t\t\t\t\t\t\t\t\t\t\t\t\t\t\t\t\t\t\t\t\t\t";

/// The commas between the cells of a CSV row.
const COMMAS: &[u8; 32] = b",,,,,,,,,,,,,,,,,,,,,,,,,,,,,,,,";

/// SPARQL 1.1 Query Results TSV, as sparesults writes it with SPARQL 1.2 terms.
pub(super) struct Tsv;

impl Tsv {
    /// The header line, `?` and each name separated by tabs.
    pub(super) fn header(out: &mut Explicit<'_, '_>, variables: &[String]) -> Result<()> {
        out.begin_piece();
        for (i, v) in variables.iter().enumerate() {
            if i > 0 {
                out.put(b"\t")?;
            }
            out.put(b"?")?;
            out.put(v.as_bytes())?;
        }
        out.put(b"\n")?;
        out.end_piece()
    }

    /// One row of `width` cells, an unbound cell empty. The row's start and end are the
    /// caller's to mark.
    pub(super) fn row(out: &mut Explicit<'_, '_>, row: RowView<'_>, width: usize) -> Result<()> {
        match row {
            RowView::Dense(terms) => {
                for (c, term) in terms.iter().enumerate() {
                    if c > 0 {
                        out.put(b"\t")?;
                    }
                    if let Some(term) = term {
                        tsv_term(out, term.as_ref())?;
                    }
                }
            }
            RowView::Sparse(terms) => {
                // the entries ascend by column, so the tabs before a cell are its column
                // less the tabs already written
                let mut tabs = 0;
                for (c, term) in terms {
                    separators_to(out, TABS, &mut tabs, *c)?;
                    tsv_term(out, term.as_ref().as_ref())?;
                }
                separators_to(out, TABS, &mut tabs, width.saturating_sub(1))?;
            }
        }
        out.put(b"\n")
    }
}

/// SPARQL 1.1 Query Results CSV, as sparesults writes it with SPARQL 1.2 terms: IRIs and
/// lexical forms without their datatypes or languages, a lexical form quoted when it holds
/// a quote, comma, line feed or carriage return, and lines ended by CRLF.
pub(super) struct Csv;

impl Csv {
    /// The header line, the names separated by commas.
    pub(super) fn header(out: &mut Explicit<'_, '_>, variables: &[String]) -> Result<()> {
        out.begin_piece();
        for (i, v) in variables.iter().enumerate() {
            if i > 0 {
                out.put(b",")?;
            }
            out.put(v.as_bytes())?;
        }
        out.put(b"\r\n")?;
        out.end_piece()
    }

    /// One row of `width` cells, an unbound cell empty.
    pub(super) fn row(out: &mut Explicit<'_, '_>, row: RowView<'_>, width: usize) -> Result<()> {
        match row {
            RowView::Dense(terms) => {
                for (c, term) in terms.iter().enumerate() {
                    if c > 0 {
                        out.put(b",")?;
                    }
                    if let Some(term) = term {
                        csv_term(out, term.as_ref())?;
                    }
                }
            }
            RowView::Sparse(terms) => {
                let mut commas = 0;
                for (c, term) in terms {
                    separators_to(out, COMMAS, &mut commas, *c)?;
                    csv_term(out, term.as_ref().as_ref())?;
                }
                separators_to(out, COMMAS, &mut commas, width.saturating_sub(1))?;
            }
        }
        out.put(b"\r\n")
    }
}

/// SPARQL 1.1 Query Results JSON, as sparesults writes it with SPARQL 1.2 terms through
/// json-event-parser: no whitespace, each bound cell an object of `type` and `value` with
/// `xml:lang` and `its:dir` or a `datatype` other than xsd:string, a triple term as an
/// object of its three parts, and strings escaped as json-event-parser escapes them.
pub(super) struct Json {
    /// each variable's key, `"name":`, escaped once
    keys: Vec<Box<[u8]>>,
    /// no row has been written, so the next one starts without a comma
    first: bool,
}

impl Json {
    pub(super) fn new(variables: &[String]) -> Json {
        let keys = variables
            .iter()
            .map(|v| {
                let mut key = Vec::with_capacity(v.len() + 3);
                json_escaped(&mut key, v);
                key.push(b':');
                key.into_boxed_slice()
            })
            .collect();
        Json { keys, first: true }
    }

    /// The document up to the opening of the bindings array, as one piece.
    pub(super) fn header(out: &mut Explicit<'_, '_>, variables: &[String]) -> Result<()> {
        out.begin_piece();
        out.put(b"{\"head\":{\"vars\":[")?;
        for (i, v) in variables.iter().enumerate() {
            if i > 0 {
                out.put(b",")?;
            }
            json_string(out, v)?;
        }
        out.put(b"]},\"results\":{\"bindings\":[")?;
        out.end_piece()
    }

    /// One solution, the bound cells in column order, with the comma before it when it is
    /// not the first, as json-event-parser writes the comma with the value it precedes.
    pub(super) fn row(
        &mut self,
        out: &mut Explicit<'_, '_>,
        row: RowView<'_>,
        _width: usize,
    ) -> Result<()> {
        out.put(if std::mem::take(&mut self.first) {
            b"{"
        } else {
            b",{"
        })?;
        let mut comma = false;
        match row {
            RowView::Dense(terms) => {
                for (c, term) in terms.iter().enumerate() {
                    if let Some(term) = term {
                        self.cell(out, &mut comma, c, term.as_ref())?;
                    }
                }
            }
            RowView::Sparse(terms) => {
                for (c, term) in terms {
                    self.cell(out, &mut comma, *c, term.as_ref().as_ref())?;
                }
            }
        }
        out.put(b"}")
    }

    #[inline]
    fn cell(
        &self,
        out: &mut Explicit<'_, '_>,
        comma: &mut bool,
        column: usize,
        term: TermRef<'_>,
    ) -> Result<()> {
        if std::mem::replace(comma, true) {
            out.put(b",")?;
        }
        out.put(&self.keys[column])?;
        json_term(out, term)
    }

    /// The end of the bindings array and of the document.
    pub(super) fn footer(out: &mut Explicit<'_, '_>) -> Result<()> {
        out.put(b"]}}")
    }
}

fn json_term(out: &mut Explicit<'_, '_>, term: TermRef<'_>) -> Result<()> {
    match term {
        TermRef::NamedNode(node) => {
            out.put(b"{\"type\":\"uri\",\"value\":")?;
            json_string(out, node.as_str())?;
            out.put(b"}")
        }
        TermRef::BlankNode(node) => {
            out.put(b"{\"type\":\"bnode\",\"value\":")?;
            json_string(out, node.as_str())?;
            out.put(b"}")
        }
        TermRef::Literal(literal) => {
            out.put(b"{\"type\":\"literal\",\"value\":")?;
            json_string(out, literal.value())?;
            if let Some(language) = literal.language() {
                out.put(b",\"xml:lang\":")?;
                json_string(out, language)?;
                match literal.direction() {
                    Some(BaseDirection::Ltr) => out.put(b",\"its:dir\":\"ltr\"")?,
                    Some(BaseDirection::Rtl) => out.put(b",\"its:dir\":\"rtl\"")?,
                    None => {}
                }
            } else if literal.datatype() != xsd::STRING {
                out.put(b",\"datatype\":")?;
                json_string(out, literal.datatype().as_str())?;
            }
            out.put(b"}")
        }
        TermRef::Triple(triple) => {
            out.put(b"{\"type\":\"triple\",\"value\":{\"subject\":")?;
            json_term(out, triple.subject.as_ref().into())?;
            out.put(b",\"predicate\":")?;
            json_term(out, triple.predicate.as_ref().into())?;
            out.put(b",\"object\":")?;
            json_term(out, triple.object.as_ref())?;
            out.put(b"}}")
        }
    }
}

/// The escape json-event-parser writes for `b`, a byte below 0x20, a quote or a
/// backslash, with the bytes of `\u00XX` in lowercase hex for a control byte that has no
/// short form.
#[inline]
fn json_escape(b: u8, long: &mut [u8; 6]) -> Option<&[u8]> {
    Some(match b {
        b'\\' => b"\\\\",
        b'"' => b"\\\"",
        0x08 => b"\\b",
        0x0c => b"\\f",
        b'\n' => b"\\n",
        b'\r' => b"\\r",
        b'\t' => b"\\t",
        0..0x20 => {
            const HEX: &[u8; 16] = b"0123456789abcdef";
            long[4] = HEX[usize::from(b >> 4)];
            long[5] = HEX[usize::from(b & 15)];
            &long[..]
        }
        _ => return None,
    })
}

/// A string between double quotes, escaped, the runs between escapes copied whole. Only
/// ASCII bytes are escaped, so a run never splits a character.
fn json_string(out: &mut Explicit<'_, '_>, s: &str) -> Result<()> {
    out.put(b"\"")?;
    let bytes = s.as_bytes();
    let mut long = *b"\\u0000";
    let mut start = 0;
    for (i, &b) in bytes.iter().enumerate() {
        if let Some(escaped) = json_escape(b, &mut long) {
            out.put(&bytes[start..i])?;
            out.put(escaped)?;
            start = i + 1;
        }
    }
    out.put(&bytes[start..])?;
    out.put(b"\"")
}

/// [`json_string`] into a vector, for the keys escaped once per answer.
fn json_escaped(to: &mut Vec<u8>, s: &str) {
    to.push(b'"');
    let mut long = *b"\\u0000";
    for &b in s.as_bytes() {
        match json_escape(b, &mut long) {
            Some(escaped) => to.extend_from_slice(escaped),
            None => to.push(b),
        }
    }
    to.push(b'"');
}

/// Write separators until `written` reaches `to`, which the entries of a sparse row give
/// as the column of the next cell.
#[inline]
fn separators_to(
    out: &mut Explicit<'_, '_>,
    run: &[u8; 32],
    written: &mut usize,
    to: usize,
) -> Result<()> {
    while *written < to {
        let n = (to - *written).min(run.len());
        out.put(&run[..n])?;
        *written += n;
    }
    Ok(())
}

fn csv_term(out: &mut Explicit<'_, '_>, term: TermRef<'_>) -> Result<()> {
    match term {
        TermRef::NamedNode(node) => out.put(node.as_str().as_bytes()),
        TermRef::BlankNode(node) => {
            out.put(b"_:")?;
            out.put(node.as_str().as_bytes())
        }
        TermRef::Literal(literal) => csv_string(out, literal.value()),
        TermRef::Triple(triple) => {
            csv_term(out, triple.subject.as_ref().into())?;
            out.put(b" ")?;
            csv_term(out, triple.predicate.as_ref().into())?;
            out.put(b" ")?;
            csv_term(out, triple.object.as_ref())
        }
    }
}

/// A lexical form, between double quotes with each quote doubled when it holds a quote,
/// comma, line feed or carriage return.
fn csv_string(out: &mut Explicit<'_, '_>, s: &str) -> Result<()> {
    let bytes = s.as_bytes();
    if !bytes
        .iter()
        .any(|&b| matches!(b, b'"' | b',' | b'\n' | b'\r'))
    {
        return out.put(bytes);
    }
    out.put(b"\"")?;
    let mut start = 0;
    for (i, &b) in bytes.iter().enumerate() {
        if b == b'"' {
            // the quote ends this run, and the next run starts with it again
            out.put(&bytes[start..=i])?;
            start = i;
        }
    }
    out.put(&bytes[start..])?;
    out.put(b"\"")
}

fn tsv_term(out: &mut Explicit<'_, '_>, term: TermRef<'_>) -> Result<()> {
    match term {
        TermRef::NamedNode(node) => {
            out.put(b"<")?;
            out.put(node.as_str().as_bytes())?;
            out.put(b">")
        }
        TermRef::BlankNode(node) => {
            out.put(b"_:")?;
            out.put(node.as_str().as_bytes())
        }
        TermRef::Literal(literal) => {
            let value = literal.value();
            if let Some(language) = literal.language() {
                tsv_quoted(out, value)?;
                out.put(b"@")?;
                out.put(language.as_bytes())?;
                match literal.direction() {
                    Some(BaseDirection::Ltr) => out.put(b"--ltr"),
                    Some(BaseDirection::Rtl) => out.put(b"--rtl"),
                    None => Ok(()),
                }
            } else {
                match literal.datatype() {
                    xsd::BOOLEAN if matches!(value, "true" | "false") => out.put(value.as_bytes()),
                    xsd::INTEGER if is_turtle_integer(value) => out.put(value.as_bytes()),
                    xsd::DECIMAL if is_turtle_decimal(value) => out.put(value.as_bytes()),
                    xsd::DOUBLE if is_turtle_double(value) => out.put(value.as_bytes()),
                    xsd::STRING => tsv_quoted(out, value),
                    datatype => {
                        tsv_quoted(out, value)?;
                        out.put(b"^^<")?;
                        out.put(datatype.as_str().as_bytes())?;
                        out.put(b">")
                    }
                }
            }
        }
        TermRef::Triple(triple) => {
            out.put(b"<<( ")?;
            tsv_term(out, triple.subject.as_ref().into())?;
            out.put(b" ")?;
            tsv_term(out, triple.predicate.as_ref().into())?;
            out.put(b" ")?;
            tsv_term(out, triple.object.as_ref())?;
            out.put(b" )>>")
        }
    }
}

/// A string between double quotes with tab, line feed, carriage return, quote and
/// backslash escaped, the runs between them copied whole.
fn tsv_quoted(out: &mut Explicit<'_, '_>, s: &str) -> Result<()> {
    out.put(b"\"")?;
    let bytes = s.as_bytes();
    let mut start = 0;
    for (i, &b) in bytes.iter().enumerate() {
        let escaped: &[u8] = match b {
            b'\t' => b"\\t",
            b'\n' => b"\\n",
            b'\r' => b"\\r",
            b'"' => b"\\\"",
            b'\\' => b"\\\\",
            _ => continue,
        };
        out.put(&bytes[start..i])?;
        out.put(escaped)?;
        start = i + 1;
    }
    out.put(&bytes[start..])?;
    out.put(b"\"")
}

fn strip_sign(value: &[u8]) -> &[u8] {
    match value {
        [b'+' | b'-', rest @ ..] => rest,
        _ => value,
    }
}

/// `[+-]? [0-9]+`
fn is_turtle_integer(value: &str) -> bool {
    let value = strip_sign(value.as_bytes());
    !value.is_empty() && value.iter().all(u8::is_ascii_digit)
}

/// `[+-]? [0-9]* '.' [0-9]+`
fn is_turtle_decimal(value: &str) -> bool {
    let value = strip_sign(value.as_bytes());
    let digits = value.iter().take_while(|b| b.is_ascii_digit()).count();
    let Some(after) = value[digits..].strip_prefix(b".") else {
        return false;
    };
    !after.is_empty() && after.iter().all(u8::is_ascii_digit)
}

/// `[+-]? ([0-9]+ '.' [0-9]* EXPONENT | '.' [0-9]+ EXPONENT | [0-9]+ EXPONENT)` with
/// `EXPONENT ::= [eE] [+-]? [0-9]+`
fn is_turtle_double(value: &str) -> bool {
    let mut value = strip_sign(value.as_bytes());
    let before = value.iter().take_while(|b| b.is_ascii_digit()).count();
    value = &value[before..];
    let mut after = 0;
    if let Some(v) = value.strip_prefix(b".") {
        after = v.iter().take_while(|b| b.is_ascii_digit()).count();
        value = &v[after..];
    }
    let Some(exponent) = value
        .strip_prefix(b"e")
        .or_else(|| value.strip_prefix(b"E"))
    else {
        return false;
    };
    let exponent = strip_sign(exponent);
    (before > 0 || after > 0) && !exponent.is_empty() && exponent.iter().all(u8::is_ascii_digit)
}

#[cfg(test)]
mod tests;
