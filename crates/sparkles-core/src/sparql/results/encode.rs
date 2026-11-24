//! The engine's own encoders of the SPARQL result formats. Each writes a row straight
//! into the output buffer through the explicit mode of [`RowOut`](super::RowOut), so a
//! row costs its bytes and one mark and no per-write check. Their bytes equal those of
//! sparesults, which stays the oracle of the tests.

use super::rowout::Explicit;
use super::source::RowView;
use crate::sparql::ctx::{Charge, Ctx};
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
pub(super) struct Json<'a> {
    /// each variable's key, `"name":`, escaped once, so that a cell copies its key with
    /// one lookup
    keys: Box<[Box<[u8]>]>,
    /// no row has been written, so the next one starts without a comma
    first: bool,
    /// the reservation for `keys`, declared after them so that they drop first
    _charge: Option<Charge<'a>>,
}

impl<'a> Json<'a> {
    /// The keys of `variables`, charged to `ctx` before they are allocated. Each key and
    /// the table of keys are allocated once at their exact size and never grow.
    pub(super) fn new(ctx: Option<&'a Ctx>, variables: &[String]) -> crate::error::Result<Self> {
        let charge = ctx
            .map(|ctx| ctx.charge(Self::charged(variables)))
            .transpose()?;
        let keys = variables
            .iter()
            .map(|v| {
                // the escaped, quoted name and a colon
                let mut key = Vec::with_capacity(json_escaped_len(v) + 1);
                json_escaped(&mut key, v);
                key.push(b':');
                debug_assert_eq!(key.len(), key.capacity());
                key.into_boxed_slice()
            })
            .collect();
        Ok(Json {
            keys,
            first: true,
            _charge: charge,
        })
    }

    /// The bytes [`Json::new`] charges for the keys of `variables`: each key's bytes and
    /// the table of their pointers.
    pub(super) fn charged(variables: &[String]) -> u64 {
        let bytes: usize = variables.iter().map(|v| json_escaped_len(v) + 1).sum();
        (bytes + variables.len() * std::mem::size_of::<Box<[u8]>>()) as u64
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

/// The length of `s` escaped and quoted by [`json_escaped`].
fn json_escaped_len(s: &str) -> usize {
    let mut long = *b"\\u0000";
    2 + s
        .bytes()
        .map(|b| json_escape(b, &mut long).map_or(1, <[u8]>::len))
        .sum::<usize>()
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

/// SPARQL Query Results XML, as sparesults writes it with SPARQL 1.2 terms through
/// quick-xml: no whitespace between elements, attributes in double quotes, text and
/// attribute values with `<`, `>`, `&`, `'` and `"` escaped, a literal's leading and
/// trailing tabs, line feeds, carriage returns and spaces as character references, and the
/// ITS namespace and version on each literal with a direction.
pub(super) struct Xml<'a> {
    /// each variable's opening binding tag, `<binding name="name">`, escaped once, so that
    /// a cell copies its tag with one lookup
    tags: Box<[Box<[u8]>]>,
    /// the reservation for `tags`, declared after them so that they drop first
    _charge: Option<Charge<'a>>,
}

const BINDING_OPEN: &[u8] = b"<binding name=\"";
const BINDING_CLOSE: &[u8] = b"\">";

impl<'a> Xml<'a> {
    /// The binding tags of `variables`, charged to `ctx` before they are allocated. Each
    /// tag and the table of tags are allocated once at their exact size and never grow.
    pub(super) fn new(ctx: Option<&'a Ctx>, variables: &[String]) -> crate::error::Result<Self> {
        let charge = ctx
            .map(|ctx| ctx.charge(Self::charged(variables)))
            .transpose()?;
        let tags = variables
            .iter()
            .map(|v| {
                let mut tag = Vec::with_capacity(Self::tag_len(v));
                tag.extend_from_slice(BINDING_OPEN);
                xml_escaped(&mut tag, v);
                tag.extend_from_slice(BINDING_CLOSE);
                debug_assert_eq!(tag.len(), tag.capacity());
                tag.into_boxed_slice()
            })
            .collect();
        Ok(Xml {
            tags,
            _charge: charge,
        })
    }

    fn tag_len(name: &str) -> usize {
        BINDING_OPEN.len() + xml_escaped_len(name) + BINDING_CLOSE.len()
    }

    /// The bytes [`Xml::new`] charges for the tags of `variables`: each tag's bytes and
    /// the table of their pointers.
    pub(super) fn charged(variables: &[String]) -> u64 {
        let bytes: usize = variables.iter().map(|v| Self::tag_len(v)).sum();
        (bytes + variables.len() * std::mem::size_of::<Box<[u8]>>()) as u64
    }

    /// The document up to the opening of the results element, as one piece.
    pub(super) fn header(out: &mut Explicit<'_, '_>, variables: &[String]) -> Result<()> {
        out.begin_piece();
        out.put(b"<?xml version=\"1.0\"?><sparql xmlns=\"http://www.w3.org/2005/sparql-results#\"><head>")?;
        for v in variables {
            out.put(b"<variable name=\"")?;
            xml_text(out, v)?;
            out.put(b"\"/>")?;
        }
        out.put(b"</head><results>")?;
        out.end_piece()
    }

    /// One solution, the bound cells in column order.
    pub(super) fn row(
        &mut self,
        out: &mut Explicit<'_, '_>,
        row: RowView<'_>,
        _width: usize,
    ) -> Result<()> {
        out.put(b"<result>")?;
        match row {
            RowView::Dense(terms) => {
                for (c, term) in terms.iter().enumerate() {
                    if let Some(term) = term {
                        self.cell(out, c, term.as_ref())?;
                    }
                }
            }
            RowView::Sparse(terms) => {
                for (c, term) in terms {
                    self.cell(out, *c, term.as_ref().as_ref())?;
                }
            }
        }
        out.put(b"</result>")
    }

    #[inline]
    fn cell(&self, out: &mut Explicit<'_, '_>, column: usize, term: TermRef<'_>) -> Result<()> {
        out.put(&self.tags[column])?;
        xml_term(out, term)?;
        out.put(b"</binding>")
    }

    /// The end of the results element and of the document.
    pub(super) fn footer(out: &mut Explicit<'_, '_>) -> Result<()> {
        out.put(b"</results></sparql>")
    }
}

fn xml_term(out: &mut Explicit<'_, '_>, term: TermRef<'_>) -> Result<()> {
    match term {
        TermRef::NamedNode(node) => {
            out.put(b"<uri>")?;
            xml_text(out, node.as_str())?;
            out.put(b"</uri>")
        }
        TermRef::BlankNode(node) => {
            out.put(b"<bnode>")?;
            xml_text(out, node.as_str())?;
            out.put(b"</bnode>")
        }
        TermRef::Literal(literal) => {
            out.put(b"<literal")?;
            if let Some(language) = literal.language() {
                out.put(b" xml:lang=\"")?;
                xml_text(out, language)?;
                out.put(b"\"")?;
                if let Some(direction) = literal.direction() {
                    out.put(match direction {
                        BaseDirection::Ltr => b" its:dir=\"ltr\"",
                        BaseDirection::Rtl => b" its:dir=\"rtl\"",
                    })?;
                    out.put(b" xmlns:its=\"http://www.w3.org/2005/11/its\" its:version=\"2.0\"")?;
                }
            } else if literal.datatype() != xsd::STRING {
                out.put(b" datatype=\"")?;
                xml_text(out, literal.datatype().as_str())?;
                out.put(b"\"")?;
            }
            out.put(b">")?;
            xml_literal_value(out, literal.value())?;
            out.put(b"</literal>")
        }
        TermRef::Triple(triple) => {
            out.put(b"<triple><subject>")?;
            xml_term(out, triple.subject.as_ref().into())?;
            out.put(b"</subject><predicate>")?;
            xml_term(out, triple.predicate.as_ref().into())?;
            out.put(b"</predicate><object>")?;
            xml_term(out, triple.object.as_ref())?;
            out.put(b"</object></triple>")
        }
    }
}

/// The escape quick-xml writes for `b` in text and attribute values.
#[inline]
fn xml_escape(b: u8) -> Option<&'static [u8]> {
    Some(match b {
        b'<' => b"&lt;",
        b'>' => b"&gt;",
        b'&' => b"&amp;",
        b'\'' => b"&apos;",
        b'"' => b"&quot;",
        _ => return None,
    })
}

/// Text or an attribute value, escaped, the runs between escapes copied whole. Only ASCII
/// bytes are escaped, so a run never splits a character.
fn xml_text(out: &mut Explicit<'_, '_>, s: &str) -> Result<()> {
    let bytes = s.as_bytes();
    let mut start = 0;
    for (i, &b) in bytes.iter().enumerate() {
        if let Some(escaped) = xml_escape(b) {
            out.put(&bytes[start..i])?;
            out.put(escaped)?;
            start = i + 1;
        }
    }
    out.put(&bytes[start..])
}

/// The character reference sparesults writes for a tab, line feed, carriage return or
/// space at either end of a literal's value.
#[inline]
fn xml_bound_space(b: u8) -> Option<&'static [u8]> {
    Some(match b {
        b'\t' => b"&#9;",
        b'\n' => b"&#10;",
        b'\r' => b"&#13;",
        b' ' => b"&#32;",
        _ => return None,
    })
}

/// A literal's value, its leading and trailing whitespace as character references so that
/// a reader keeps it, and the rest escaped as text. A value of only whitespace is all
/// references.
fn xml_literal_value(out: &mut Explicit<'_, '_>, s: &str) -> Result<()> {
    let bytes = s.as_bytes();
    let start = bytes
        .iter()
        .position(|&b| xml_bound_space(b).is_none())
        .unwrap_or(bytes.len());
    let end = bytes
        .iter()
        .rposition(|&b| xml_bound_space(b).is_none())
        .map_or(start, |i| i + 1);
    for &b in &bytes[..start] {
        out.put(xml_bound_space(b).unwrap_or_default())?;
    }
    // both ends are ASCII bytes or the ends of the value, so the middle is a whole str
    xml_text(out, &s[start..end])?;
    for &b in &bytes[end..] {
        out.put(xml_bound_space(b).unwrap_or_default())?;
    }
    Ok(())
}

/// The length of `s` escaped by [`xml_escaped`].
fn xml_escaped_len(s: &str) -> usize {
    s.bytes()
        .map(|b| xml_escape(b).map_or(1, <[u8]>::len))
        .sum()
}

/// [`xml_text`] into a vector, for the binding tags escaped once per answer.
fn xml_escaped(to: &mut Vec<u8>, s: &str) {
    for &b in s.as_bytes() {
        match xml_escape(b) {
            Some(escaped) => to.extend_from_slice(escaped),
            None => to.push(b),
        }
    }
}

#[cfg(test)]
mod tests;
