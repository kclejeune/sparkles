//! The binary form of RDF Patch: `RDF_Patch_Row` structs of Jena's RDF Thrift schema
//! (`BinaryRDF.thrift`), one per row, in the Thrift compact protocol, with no framing.
//!
//! The decoder reads the structs it knows field by field and skips unknown fields by
//! their wire type, as a Thrift reader of a newer schema version must. Strings and
//! containers are read in pieces, so a length that the body cannot hold fails at the
//! end of the body instead of allocating it.

use super::read::{
    MAX_TERM_BYTES, MAX_TRIPLE_DEPTH, PatchError, PatchErrorKind, PatchRow, bnode, named, quad,
    term_error, triple, typed,
};
use oxrdf::{BaseDirection, Literal, NamedNode, Term};
use std::io::{BufReader, Read};

// compact protocol wire types
const T_TRUE: u8 = 1;
const T_FALSE: u8 = 2;
const T_BYTE: u8 = 3;
const T_I16: u8 = 4;
const T_I32: u8 = 5;
const T_I64: u8 = 6;
const T_DOUBLE: u8 = 7;
const T_BINARY: u8 = 8;
const T_LIST: u8 = 9;
const T_SET: u8 = 10;
const T_MAP: u8 = 11;
const T_STRUCT: u8 = 12;

/// The deepest nesting of structs and containers a skipped field may have.
const MAX_SKIP_DEPTH: usize = 64;

pub(super) struct ThriftRows<R: Read> {
    r: BufReader<R>,
    offset: u64,
    rows: u64,
}

/// A field header: its id and wire type (`None` at the end of a struct).
type Field = Option<(i16, u8)>;

impl<R: Read> ThriftRows<R> {
    pub(super) fn new(r: R) -> ThriftRows<R> {
        ThriftRows {
            r: BufReader::with_capacity(64 << 10, r),
            offset: 0,
            rows: 0,
        }
    }

    pub(super) fn rows(&self) -> u64 {
        self.rows
    }

    pub(super) fn next_row(&mut self) -> crate::Result<Option<PatchRow>> {
        let start = self.offset;
        // the end of the body between rows is the end of the patch
        let mut b = [0u8; 1];
        loop {
            match self.r.read(&mut b) {
                Ok(0) => return Ok(None),
                Ok(_) => break,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(e.into()),
            }
        }
        self.offset += 1;
        self.rows += 1;
        let row = self.rows;
        self.row(b[0])
            .map(Some)
            .map_err(|e| crate::Error::Patch(Box::new(e.at_binary(row, start))))
    }

    fn syntax(&self, msg: impl Into<String>) -> PatchError {
        PatchError::new(PatchErrorKind::Syntax, msg)
    }

    fn byte(&mut self) -> Result<u8, PatchError> {
        let mut b = [0u8; 1];
        self.r
            .read_exact(&mut b)
            .map_err(|e| self.syntax(eof_message(&e)))?;
        self.offset += 1;
        Ok(b[0])
    }

    fn varint(&mut self) -> Result<u64, PatchError> {
        let mut v = 0u64;
        for shift in (0..70).step_by(7) {
            let b = self.byte()?;
            if shift == 63 && b > 1 {
                break;
            }
            v |= ((b & 0x7f) as u64) << shift;
            if b & 0x80 == 0 {
                return Ok(v);
            }
        }
        Err(self.syntax("a varint longer than 64 bits"))
    }

    fn zigzag(&mut self) -> Result<i64, PatchError> {
        let v = self.varint()?;
        Ok(((v >> 1) as i64) ^ -((v & 1) as i64))
    }

    fn bytes(&mut self) -> Result<Vec<u8>, PatchError> {
        let n = self.varint()?;
        if n > MAX_TERM_BYTES as u64 {
            return Err(self.syntax(format!("a string of {n} bytes is too long")));
        }
        let mut buf = Vec::new();
        // read in pieces: the length is not trusted before the bytes arrive
        (&mut self.r)
            .take(n)
            .read_to_end(&mut buf)
            .map_err(|e| self.syntax(format!("reading the patch: {e}")))?;
        if buf.len() as u64 != n {
            return Err(self.syntax("the patch ends inside a string"));
        }
        self.offset += n;
        Ok(buf)
    }

    fn string(&mut self) -> Result<String, PatchError> {
        String::from_utf8(self.bytes()?).map_err(|_| self.syntax("a string is not UTF-8"))
    }

    /// The next field header of a struct whose previous field id was `last`.
    fn field(&mut self, last: &mut i16) -> Result<Field, PatchError> {
        self.field_from(None, last)
    }

    fn field_from(&mut self, first: Option<u8>, last: &mut i16) -> Result<Field, PatchError> {
        let b = match first {
            Some(b) => b,
            None => self.byte()?,
        };
        if b == 0 {
            return Ok(None);
        }
        let ty = b & 0x0f;
        let delta = (b >> 4) as i16;
        let id = if delta == 0 {
            i16::try_from(self.zigzag()?).map_err(|_| self.syntax("a field id out of range"))?
        } else {
            last.checked_add(delta)
                .ok_or_else(|| self.syntax("a field id out of range"))?
        };
        *last = id;
        Ok(Some((id, ty)))
    }

    /// Skip a value of wire type `ty`.
    fn skip(&mut self, ty: u8, depth: usize) -> Result<(), PatchError> {
        if depth > MAX_SKIP_DEPTH {
            return Err(self.syntax("values nested too deeply"));
        }
        match ty {
            T_TRUE | T_FALSE => {}
            T_BYTE => {
                self.byte()?;
            }
            T_I16 | T_I32 | T_I64 => {
                self.varint()?;
            }
            T_DOUBLE => {
                for _ in 0..8 {
                    self.byte()?;
                }
            }
            T_BINARY => {
                self.bytes()?;
            }
            T_LIST | T_SET => {
                let h = self.byte()?;
                let elem = h & 0x0f;
                let n = match h >> 4 {
                    15 => self.varint()?,
                    n => n as u64,
                };
                for _ in 0..n {
                    // a boolean element is one byte in a list
                    if elem == T_TRUE || elem == T_FALSE {
                        self.byte()?;
                    } else {
                        self.skip(elem, depth + 1)?;
                    }
                }
            }
            T_MAP => {
                let n = self.varint()?;
                if n > 0 {
                    let kv = self.byte()?;
                    for _ in 0..n {
                        self.skip_elem(kv >> 4, depth + 1)?;
                        self.skip_elem(kv & 0x0f, depth + 1)?;
                    }
                }
            }
            T_STRUCT => {
                let mut last = 0;
                while let Some((_, t)) = self.field(&mut last)? {
                    self.skip(t, depth + 1)?;
                }
            }
            t => return Err(self.syntax(format!("an unknown Thrift type {t}"))),
        }
        Ok(())
    }

    fn skip_elem(&mut self, ty: u8, depth: usize) -> Result<(), PatchError> {
        if ty == T_TRUE || ty == T_FALSE {
            self.byte().map(|_| ())
        } else {
            self.skip(ty, depth)
        }
    }

    fn expect(&self, ty: u8, want: u8, what: &str) -> Result<(), PatchError> {
        if ty == want {
            Ok(())
        } else {
            Err(self.syntax(format!("{what} has the wrong Thrift type {ty}")))
        }
    }

    /// An `RDF_Patch_Row` (a union) whose first byte was read.
    fn row(&mut self, first: u8) -> Result<PatchRow, PatchError> {
        let mut last = 0;
        let mut row = None;
        let mut next = Some(first);
        while let Some((id, ty)) = self.field_from(next.take(), &mut last)? {
            if row.is_some() {
                return Err(self.syntax("a row with more than one member"));
            }
            row = Some(match id {
                1 => {
                    self.expect(ty, T_STRUCT, "a header")?;
                    self.header()?
                }
                2 | 3 => {
                    self.expect(ty, T_STRUCT, "a data row")?;
                    let q = self.data()?;
                    if id == 2 {
                        PatchRow::Add(q)
                    } else {
                        PatchRow::Delete(q)
                    }
                }
                4 | 5 => {
                    self.expect(ty, T_STRUCT, "a prefix row")?;
                    self.prefix(id == 4)?
                }
                6 => {
                    self.expect(ty, T_I32, "a transaction row")?;
                    match self.zigzag()? {
                        0 => PatchRow::Begin,
                        1 => PatchRow::Commit,
                        2 => PatchRow::Abort,
                        3 => PatchRow::Segment,
                        n => return Err(self.syntax(format!("an unknown transaction code {n}"))),
                    }
                }
                _ => {
                    self.skip(ty, 0)?;
                    continue;
                }
            });
        }
        row.ok_or_else(|| self.syntax("an empty row"))
    }

    fn header(&mut self) -> Result<PatchRow, PatchError> {
        let (mut last, mut name, mut value) = (0, None, None);
        while let Some((id, ty)) = self.field(&mut last)? {
            match (id, ty) {
                (1, T_BINARY) => name = Some(self.string()?),
                (2, T_STRUCT) => value = Some(self.term(0)?),
                (_, ty) => self.skip(ty, 0)?,
            }
        }
        match (name, value) {
            (Some(n), Some(v)) => Ok(PatchRow::Header(n, v)),
            _ => Err(self.syntax("a header without a name or a value")),
        }
    }

    fn data(&mut self) -> Result<oxrdf::Quad, PatchError> {
        let mut last = 0;
        let mut t: [Option<Term>; 4] = Default::default();
        while let Some((id, ty)) = self.field(&mut last)? {
            match (id, ty) {
                (1..=4, T_STRUCT) => t[id as usize - 1] = Some(self.term(0)?),
                (_, ty) => self.skip(ty, 0)?,
            }
        }
        let [Some(s), Some(p), Some(o), g] = t else {
            return Err(self.syntax("a data row without a subject, predicate or object"));
        };
        quad(s, p, o, g)
    }

    fn prefix(&mut self, add: bool) -> Result<PatchRow, PatchError> {
        let (mut last, mut prefix, mut iri) = (0, None, None);
        while let Some((id, ty)) = self.field(&mut last)? {
            match (id, ty) {
                // the graph node is read and dropped
                (1, T_STRUCT) => {
                    self.term(0)?;
                }
                (2, T_BINARY) => prefix = Some(self.string()?),
                (3, T_BINARY) if add => iri = Some(self.string()?),
                (_, ty) => self.skip(ty, 0)?,
            }
        }
        match (prefix, iri, add) {
            (Some(p), Some(i), true) => Ok(PatchRow::PrefixSet(p, i)),
            (Some(p), _, false) => Ok(PatchRow::PrefixRemove(p)),
            _ => Err(self.syntax("a prefix row without a prefix or an IRI")),
        }
    }

    /// An `RDF_Term` (a union), whose field header was read.
    fn term(&mut self, depth: usize) -> Result<Term, PatchError> {
        if depth > MAX_TRIPLE_DEPTH {
            return Err(self.syntax("triple terms nested too deeply"));
        }
        let mut last = 0;
        let mut out = None;
        while let Some((id, ty)) = self.field(&mut last)? {
            if out.is_some() {
                return Err(self.syntax("a term with more than one member"));
            }
            out = Some(match (id, ty) {
                (1, T_STRUCT) => Term::NamedNode(named(&self.one_string()?)?),
                (2, T_STRUCT) => Term::BlankNode(bnode(&self.one_string()?)?),
                (3, T_STRUCT) => Term::Literal(self.literal()?),
                (9, T_STRUCT) => {
                    let mut l = 0;
                    let mut t: [Option<Term>; 3] = Default::default();
                    while let Some((id, ty)) = self.field(&mut l)? {
                        match (id, ty) {
                            (1..=3, T_STRUCT) => t[id as usize - 1] = Some(self.term(depth + 1)?),
                            (_, ty) => self.skip(ty, 0)?,
                        }
                    }
                    let [Some(s), Some(p), Some(o)] = t else {
                        return Err(self.syntax("a triple term without all three parts"));
                    };
                    Term::Triple(Box::new(triple(s, p, o)?))
                }
                (10, T_I64) => {
                    let v = self.zigzag()?;
                    Term::Literal(xsd_literal(v.to_string(), "integer"))
                }
                (11, T_DOUBLE) => {
                    let mut b = [0u8; 8];
                    for x in &mut b {
                        *x = self.byte()?;
                    }
                    let v = f64::from_le_bytes(b);
                    Term::Literal(Literal::from(v))
                }
                (12, T_STRUCT) => {
                    let (mut l, mut value, mut scale) = (0, None, None);
                    while let Some((id, ty)) = self.field(&mut l)? {
                        match (id, ty) {
                            (1, T_I64) => value = Some(self.zigzag()?),
                            (2, T_I32) => scale = Some(self.zigzag()?),
                            (_, ty) => self.skip(ty, 0)?,
                        }
                    }
                    let (Some(v), Some(s)) = (value, scale) else {
                        return Err(self.syntax("a decimal without a value or a scale"));
                    };
                    Term::Literal(xsd_literal(decimal(v, s)?, "decimal"))
                }
                (4, _) => {
                    return Err(term_error(
                        "a prefixed name cannot be used in a patch; write the full IRI",
                    ));
                }
                (5..=8, _) => {
                    return Err(term_error(
                        "a variable or pattern term cannot be used in a patch",
                    ));
                }
                (_, ty) => {
                    self.skip(ty, 0)?;
                    continue;
                }
            });
        }
        out.ok_or_else(|| self.syntax("an empty term"))
    }

    /// A struct of one required string field 1 (`RDF_IRI`, `RDF_BNode`).
    fn one_string(&mut self) -> Result<String, PatchError> {
        let (mut last, mut s) = (0, None);
        while let Some((id, ty)) = self.field(&mut last)? {
            match (id, ty) {
                (1, T_BINARY) => s = Some(self.string()?),
                (_, ty) => self.skip(ty, 0)?,
            }
        }
        s.ok_or_else(|| self.syntax("an IRI or a blank node without its string"))
    }

    fn literal(&mut self) -> Result<Literal, PatchError> {
        let mut last = 0;
        let (mut lex, mut lang, mut dt, mut dir) = (None, None, None, None);
        while let Some((id, ty)) = self.field(&mut last)? {
            match (id, ty) {
                (1, T_BINARY) => lex = Some(self.string()?),
                (2, T_BINARY) => lang = Some(self.string()?),
                (3, T_BINARY) => dt = Some(self.string()?),
                (5, T_BINARY) => dir = Some(self.string()?),
                (4, _) => {
                    return Err(term_error(
                        "a datatype as a prefixed name cannot be used in a patch",
                    ));
                }
                (_, ty) => self.skip(ty, 0)?,
            }
        }
        let lex = lex.ok_or_else(|| self.syntax("a literal without a lexical form"))?;
        match lang.filter(|l| !l.is_empty()) {
            Some(tag) => {
                let l = match dir.as_deref() {
                    None | Some("") => Literal::new_language_tagged_literal(lex, &tag),
                    Some("ltr") => Literal::new_directional_language_tagged_literal(
                        lex,
                        &tag,
                        BaseDirection::Ltr,
                    ),
                    Some("rtl") => Literal::new_directional_language_tagged_literal(
                        lex,
                        &tag,
                        BaseDirection::Rtl,
                    ),
                    Some(d) => return Err(term_error(format!("unknown base direction {d:?}"))),
                };
                l.map_err(|e| term_error(format!("invalid language tag {tag:?}: {e}")))
            }
            None => match dt {
                Some(dt) => typed(lex, &dt),
                None => Ok(Literal::new_simple_literal(lex)),
            },
        }
    }
}

fn xsd_literal(lex: String, dt: &str) -> Literal {
    Literal::new_typed_literal(
        lex,
        NamedNode::new_unchecked(format!("http://www.w3.org/2001/XMLSchema#{dt}")),
    )
}

/// The lexical form of the decimal `value × 10^-scale`.
fn decimal(value: i64, scale: i64) -> Result<String, PatchError> {
    if !(0..=40).contains(&scale) {
        return Err(term_error(format!("a decimal scale of {scale}")));
    }
    let neg = value < 0;
    let digits = value.unsigned_abs().to_string();
    let scale = scale as usize;
    let (int, frac) = if digits.len() > scale {
        let (a, b) = digits.split_at(digits.len() - scale);
        (a.to_string(), b.to_string())
    } else {
        ("0".to_string(), format!("{digits:0>scale$}"))
    };
    let frac = if frac.is_empty() { "0".into() } else { frac };
    Ok(format!("{}{int}.{frac}", if neg { "-" } else { "" }))
}

fn eof_message(e: &std::io::Error) -> String {
    if e.kind() == std::io::ErrorKind::UnexpectedEof {
        "the patch ends inside a row".into()
    } else {
        format!("reading the patch: {e}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decimals() {
        assert_eq!(decimal(12345, 2).unwrap(), "123.45");
        assert_eq!(decimal(-5, 3).unwrap(), "-0.005");
        assert_eq!(decimal(7, 0).unwrap(), "7.0");
    }
}
