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
                    tabs_to(out, &mut tabs, *c)?;
                    tsv_term(out, term.as_ref().as_ref())?;
                }
                tabs_to(out, &mut tabs, width.saturating_sub(1))?;
            }
        }
        out.put(b"\n")
    }
}

#[inline]
fn tabs_to(out: &mut Explicit<'_, '_>, written: &mut usize, to: usize) -> Result<()> {
    while *written < to {
        let n = (to - *written).min(TABS.len());
        out.put(&TABS[..n])?;
        *written += n;
    }
    Ok(())
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
