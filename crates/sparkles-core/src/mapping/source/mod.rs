//! Logical sources as streams of rows (spec F19 §4). A source is opened with the
//! references its triples maps read, and hands each row to a callback, so no source is
//! held in memory except a JSON document, which a JSONPath iterator needs whole.

mod csv;
mod json;
#[cfg(feature = "mapping-parquet")]
mod parquet;
#[cfg(feature = "mapping-sqlite")]
pub(crate) mod sqlite;

use super::model::{SourceDecl, SourceKind};
use super::{Inputs, RunOptions};
use crate::error::{Error, Result};
use oxrdf::NamedNode;
use std::borrow::Cow;

/// The natural datatype of a value (R2RML §10.2), or another datatype a CSVW
/// description gave.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Dt {
    /// a plain string
    Str,
    Integer,
    Decimal,
    Double,
    Boolean,
    Date,
    Time,
    DateTime,
    HexBinary,
    /// a language-tagged string from a CSVW description
    Lang(String),
    Other(NamedNode),
}

impl Dt {
    pub fn xsd(&self) -> Option<&str> {
        Some(match self {
            Dt::Str | Dt::Lang(_) => return None,
            Dt::Integer => "http://www.w3.org/2001/XMLSchema#integer",
            Dt::Decimal => "http://www.w3.org/2001/XMLSchema#decimal",
            Dt::Double => "http://www.w3.org/2001/XMLSchema#double",
            Dt::Boolean => "http://www.w3.org/2001/XMLSchema#boolean",
            Dt::Date => "http://www.w3.org/2001/XMLSchema#date",
            Dt::Time => "http://www.w3.org/2001/XMLSchema#time",
            Dt::DateTime => "http://www.w3.org/2001/XMLSchema#dateTime",
            Dt::HexBinary => "http://www.w3.org/2001/XMLSchema#hexBinary",
            Dt::Other(n) => n.as_str(),
        })
    }

    /// The datatype of a literal from a CSVW description.
    pub fn of_literal(l: &oxrdf::Literal) -> Dt {
        if let Some(lang) = l.language() {
            return Dt::Lang(lang.to_string());
        }
        match l.datatype().as_str() {
            "http://www.w3.org/2001/XMLSchema#string" => Dt::Str,
            "http://www.w3.org/2001/XMLSchema#integer" => Dt::Integer,
            "http://www.w3.org/2001/XMLSchema#decimal" => Dt::Decimal,
            "http://www.w3.org/2001/XMLSchema#double" => Dt::Double,
            "http://www.w3.org/2001/XMLSchema#boolean" => Dt::Boolean,
            "http://www.w3.org/2001/XMLSchema#date" => Dt::Date,
            "http://www.w3.org/2001/XMLSchema#time" => Dt::Time,
            "http://www.w3.org/2001/XMLSchema#dateTime" => Dt::DateTime,
            "http://www.w3.org/2001/XMLSchema#hexBinary" => Dt::HexBinary,
            _ => Dt::Other(l.datatype().into_owned()),
        }
    }
}

/// One value of a reference in a row.
#[derive(Clone, Debug)]
pub struct Cell<'a> {
    /// the natural RDF lexical form
    pub lex: Cow<'a, str>,
    pub dt: Dt,
}

impl<'a> Cell<'a> {
    pub fn str(s: &'a str) -> Cell<'a> {
        Cell {
            lex: Cow::Borrowed(s),
            dt: Dt::Str,
        }
    }
}

/// A row of a source.
pub trait Row {
    /// Push the values of reference `r` (an index into the references the source was
    /// opened with). None is NULL; a multi-valued JSON reference gives several.
    fn get<'a>(&'a self, r: usize, out: &mut Vec<Cell<'a>>) -> Result<()>;
    /// Where the row is, for messages: the source row of a CSV file, the iteration of
    /// a JSON source, the row group and row of a Parquet file.
    fn position(&self) -> String;
}

/// A source opened with its references.
pub trait RowSource {
    /// Hand every row to `f`, in order.
    fn scan(&mut self, f: &mut dyn FnMut(&dyn Row) -> Result<()>) -> Result<()>;
}

/// Open a source for the references `refs`.
pub fn open(
    decl: &SourceDecl,
    refs: &[String],
    inputs: &Inputs,
    opts: &RunOptions,
) -> Result<Box<dyn RowSource>> {
    match &decl.kind {
        SourceKind::Csv {
            file,
            tsv,
            dialect,
            null,
        } => {
            let path = inputs.file(file)?;
            Ok(Box::new(csv::CsvSource::new(
                path,
                file,
                *tsv,
                dialect.clone(),
                null.clone(),
                refs,
                inputs,
                opts,
            )?))
        }
        SourceKind::Json { file, lines } => {
            let path = inputs.file(file)?;
            Ok(Box::new(json::JsonSource::new(
                path,
                file,
                *lines,
                decl.iterator.as_deref(),
                refs,
                opts,
            )?))
        }
        SourceKind::Parquet { file } => {
            let path = inputs.file(file)?;
            #[cfg(feature = "mapping-parquet")]
            {
                Ok(Box::new(parquet::ParquetSource::new(
                    path, file, refs, opts,
                )?))
            }
            #[cfg(not(feature = "mapping-parquet"))]
            {
                let _ = path;
                Err(Error::Unsupported(format!(
                    "Parquet file {file}: this build has no Parquet sources (cargo feature mapping-parquet)"
                )))
            }
        }
        SourceKind::Sql { connection, query } => {
            let conn = inputs.connection(connection)?;
            #[cfg(feature = "mapping-sqlite")]
            {
                sqlite::open(conn, query, refs, opts)
            }
            #[cfg(not(feature = "mapping-sqlite"))]
            {
                let _ = (conn, query, opts);
                Err(Error::Unsupported(
                    "this build has no SQL sources (cargo feature mapping-sqlite)".into(),
                ))
            }
        }
    }
}

/// A file source's path, checked to be a file.
pub(super) fn check_file(p: &std::path::Path, name: &str) -> Result<()> {
    if !p.is_file() {
        return Err(Error::invalid(format!(
            "mapping-source-missing: the source {name} is not a readable file ({})",
            p.display()
        )));
    }
    Ok(())
}

/// The canonical lexical form of an `xsd:double` (R2RML's natural mapping): a mantissa
/// with one digit before the point and an exponent, such as `8.025E1`.
pub fn canonical_double(v: f64) -> String {
    if v.is_nan() {
        return "NaN".into();
    }
    if v.is_infinite() {
        return if v > 0.0 { "INF".into() } else { "-INF".into() };
    }
    if v == 0.0 {
        return if v.is_sign_negative() {
            "-0.0E0".into()
        } else {
            "0.0E0".into()
        };
    }
    let s = format!("{v:E}");
    // Rust prints `8.025E1` or `1E0`: the mantissa needs a fractional digit
    let (m, e) = s.split_once('E').unwrap_or((&s, "0"));
    let m = if m.contains('.') {
        m.to_string()
    } else {
        format!("{m}.0")
    };
    format!("{m}E{e}")
}

/// The canonical lexical form of an `xsd:decimal`, with at least one digit after the
/// point (`5.0`, `99.5`), as the acceptance examples of spec F19 write it.
pub fn canonical_decimal(s: &str) -> Option<String> {
    let s = s.trim();
    let (neg, body) = match s.strip_prefix('-') {
        Some(b) => (true, b),
        None => (false, s.strip_prefix('+').unwrap_or(s)),
    };
    let (int, frac) = body.split_once('.').unwrap_or((body, ""));
    if int.is_empty() && frac.is_empty()
        || !int.bytes().all(|b| b.is_ascii_digit())
        || !frac.bytes().all(|b| b.is_ascii_digit())
    {
        return None;
    }
    let int = int.trim_start_matches('0');
    let frac = frac.trim_end_matches('0');
    let int = if int.is_empty() { "0" } else { int };
    let frac = if frac.is_empty() { "0" } else { frac };
    let zero = int == "0" && frac == "0";
    Some(format!(
        "{}{int}.{frac}",
        if neg && !zero { "-" } else { "" }
    ))
}

/// The canonical lexical form of an `xsd:integer`.
pub fn canonical_integer(s: &str) -> Option<String> {
    let s = s.trim();
    let (neg, body) = match s.strip_prefix('-') {
        Some(b) => (true, b),
        None => (false, s.strip_prefix('+').unwrap_or(s)),
    };
    if body.is_empty() || !body.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let body = body.trim_start_matches('0');
    if body.is_empty() {
        return Some("0".into());
    }
    Some(format!("{}{body}", if neg { "-" } else { "" }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_forms() {
        assert_eq!(canonical_double(80.25), "8.025E1");
        assert_eq!(canonical_double(1.0), "1.0E0");
        assert_eq!(canonical_double(-0.5), "-5.0E-1");
        assert_eq!(canonical_decimal("99.50").unwrap(), "99.5");
        assert_eq!(canonical_decimal("5").unwrap(), "5.0");
        assert_eq!(canonical_decimal("-0.00").unwrap(), "0.0");
        assert_eq!(canonical_decimal("007.10").unwrap(), "7.1");
        assert!(canonical_decimal("1e3").is_none());
        assert_eq!(canonical_integer("+007").unwrap(), "7");
        assert_eq!(canonical_integer("-0").unwrap(), "0");
    }
}
