//! 64-bit term identifiers.
//!
//! Layout (QLever `ValueId`-style): the top 4 bits are a [`Tag`], the low 60 bits are
//! a payload whose meaning depends on the tag. Small values (integers, doubles,
//! booleans) are stored *inline* in the id, so they never touch the vocabulary and can
//! be compared/aggregated without a dictionary lookup (like Jena TDB2's inline NodeIds).
//!
//! Inlining only happens when the literal's lexical form is already canonical, which
//! keeps the mapping term ⇄ id bijective: `"1"^^xsd:integer` is inline, `"01"^^xsd:integer`
//! lives in the vocabulary. Two ids are equal iff the RDF terms are identical
//! (`sameTerm`).

use oxrdf::vocab::xsd;
use oxrdf::{BlankNode, Literal, NamedNode, Term};
use std::fmt;

pub const TAG_BITS: u32 = 4;
pub const PAYLOAD_BITS: u32 = 64 - TAG_BITS;
pub const PAYLOAD_MASK: u64 = (1 << PAYLOAD_BITS) - 1;

#[derive(Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
#[repr(u8)]
pub enum Tag {
    /// Unbound variable (id 0).
    Undef = 0,
    /// Special markers (e.g. the default graph).
    Special = 1,
    Bool = 2,
    /// Inline `xsd:integer` in 60-bit two's complement.
    Int = 3,
    /// Inline `xsd:double` whose low 4 mantissa bits are zero.
    Double = 4,
    /// Blank node. Payloads with bit 59 set are query-local (`BNODE()`).
    BNode = 5,
    /// Index into the sorted, immutable base vocabulary.
    Vocab = 6,
    /// Index into the append-only delta vocabulary (terms added by updates).
    Delta = 7,
    /// Index into a per-query local vocabulary (computed terms not in the store).
    Local = 8,
}

impl Tag {
    #[inline]
    fn from_u8(v: u8) -> Tag {
        match v {
            0 => Tag::Undef,
            1 => Tag::Special,
            2 => Tag::Bool,
            3 => Tag::Int,
            4 => Tag::Double,
            5 => Tag::BNode,
            6 => Tag::Vocab,
            7 => Tag::Delta,
            8 => Tag::Local,
            _ => Tag::Undef,
        }
    }
}

#[derive(Copy, Clone, Eq, PartialEq, Ord, PartialOrd, Hash, Default)]
#[repr(transparent)]
pub struct Id(pub u64);

impl Id {
    pub const UNDEF: Id = Id(0);
    pub const DEFAULT_GRAPH: Id = Id::new(Tag::Special, 0);
    /// Marker used by the GSPO scan to mean "any graph" in bound-prefix positions.
    pub const MAX: Id = Id(u64::MAX);

    pub const LOCAL_BNODE_BIT: u64 = 1 << (PAYLOAD_BITS - 1);

    #[inline]
    pub const fn new(tag: Tag, payload: u64) -> Id {
        Id(((tag as u64) << PAYLOAD_BITS) | (payload & PAYLOAD_MASK))
    }
    #[inline]
    pub fn tag(self) -> Tag {
        Tag::from_u8((self.0 >> PAYLOAD_BITS) as u8)
    }
    #[inline]
    pub fn payload(self) -> u64 {
        self.0 & PAYLOAD_MASK
    }
    #[inline]
    pub fn is_undef(self) -> bool {
        self.0 == 0
    }
    #[inline]
    pub fn is_inline(self) -> bool {
        matches!(self.tag(), Tag::Bool | Tag::Int | Tag::Double)
    }
    #[inline]
    pub fn vocab(i: u64) -> Id {
        Id::new(Tag::Vocab, i)
    }
    #[inline]
    pub fn delta(i: u64) -> Id {
        Id::new(Tag::Delta, i)
    }
    #[inline]
    pub fn local(i: u64) -> Id {
        Id::new(Tag::Local, i)
    }
    #[inline]
    pub fn bnode(i: u64) -> Id {
        Id::new(Tag::BNode, i)
    }
    #[inline]
    pub fn from_bool(b: bool) -> Id {
        Id::new(Tag::Bool, b as u64)
    }
    /// Inline integer, if it fits into 60 bits.
    #[inline]
    pub fn from_i64(i: i64) -> Option<Id> {
        const MIN: i64 = -(1 << (PAYLOAD_BITS - 1));
        const MAX: i64 = (1 << (PAYLOAD_BITS - 1)) - 1;
        (MIN..=MAX)
            .contains(&i)
            .then(|| Id::new(Tag::Int, i as u64))
    }
    /// Inline double, if it is representable without loss.
    #[inline]
    pub fn from_f64(f: f64) -> Option<Id> {
        let bits = f.to_bits();
        (bits & 0xF == 0).then(|| Id::new(Tag::Double, bits >> TAG_BITS))
    }
    #[inline]
    pub fn as_i64(self) -> i64 {
        ((self.payload() << TAG_BITS) as i64) >> TAG_BITS
    }
    #[inline]
    pub fn as_f64(self) -> f64 {
        f64::from_bits(self.payload() << TAG_BITS)
    }
    #[inline]
    pub fn as_bool(self) -> bool {
        self.payload() != 0
    }
}

impl fmt::Debug for Id {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.tag() {
            Tag::Undef => f.write_str("UNDEF"),
            Tag::Special => write!(f, "S:{}", self.payload()),
            Tag::Bool => write!(f, "B:{}", self.as_bool()),
            Tag::Int => write!(f, "I:{}", self.as_i64()),
            Tag::Double => write!(f, "D:{}", self.as_f64()),
            Tag::BNode => write!(f, "_:{}", self.payload()),
            Tag::Vocab => write!(f, "V:{}", self.payload()),
            Tag::Delta => write!(f, "Δ:{}", self.payload()),
            Tag::Local => write!(f, "L:{}", self.payload()),
        }
    }
}

/// Try to encode a term as an inline id (no vocabulary involved).
pub fn inline_id(term: &Term) -> Option<Id> {
    match term {
        Term::Literal(l) => inline_literal(l.value(), l.datatype().as_str()),
        _ => None,
    }
}

pub fn inline_literal(lex: &str, datatype: &str) -> Option<Id> {
    if datatype == xsd::INTEGER.as_str() {
        let i: i64 = lex.parse().ok()?;
        (i.to_string() == lex).then_some(())?;
        Id::from_i64(i)
    } else if datatype == xsd::DOUBLE.as_str() {
        let d: oxsdatatypes::Double = lex.parse().ok()?;
        (d.to_string() == lex).then_some(())?;
        Id::from_f64(d.into())
    } else if datatype == xsd::BOOLEAN.as_str() {
        match lex {
            "true" => Some(Id::from_bool(true)),
            "false" => Some(Id::from_bool(false)),
            _ => None,
        }
    } else {
        None
    }
}

/// Decode an inline id back into a literal.
pub fn inline_to_literal(id: Id) -> Option<Literal> {
    Some(match id.tag() {
        Tag::Bool => Literal::new_typed_literal(
            if id.as_bool() { "true" } else { "false" },
            xsd::BOOLEAN,
        ),
        Tag::Int => Literal::new_typed_literal(id.as_i64().to_string(), xsd::INTEGER),
        Tag::Double => Literal::new_typed_literal(
            oxsdatatypes::Double::from(id.as_f64()).to_string(),
            xsd::DOUBLE,
        ),
        _ => return None,
    })
}

// ---------------------------------------------------------------------------------
// Vocabulary keys
// ---------------------------------------------------------------------------------
//
// Terms that are not inlined are stored in the vocabulary as byte strings:
//   IRI      : '<' iri
//   literal  : '"' lexical 0xFF [ '@' lang | '^' datatype-iri ]   (xsd:string: no suffix)
// 0xFF never occurs in UTF-8 so it is an unambiguous separator. Byte order groups all
// literals before all IRIs, and literals by lexical form — the base vocabulary is sorted
// by this key.

pub const KEY_SEP: u8 = 0xFF;

pub fn term_key(term: &Term) -> Vec<u8> {
    let mut out = Vec::new();
    write_term_key(term, &mut out);
    out
}

pub fn write_term_key(term: &Term, out: &mut Vec<u8>) {
    match term {
        Term::NamedNode(n) => {
            out.push(b'<');
            out.extend_from_slice(n.as_str().as_bytes());
        }
        Term::Literal(l) => write_literal_key(l, out),
        Term::BlankNode(b) => {
            // Blank nodes are normally mapped to BNode ids; this path is only used for
            // hashing/diagnostics.
            out.extend_from_slice(b"_:");
            out.extend_from_slice(b.as_str().as_bytes());
        }
    }
}

pub fn write_literal_key(l: &Literal, out: &mut Vec<u8>) {
    out.push(b'"');
    out.extend_from_slice(l.value().as_bytes());
    out.push(KEY_SEP);
    if let Some(lang) = l.language() {
        out.push(b'@');
        out.extend_from_slice(lang.as_bytes());
    } else if l.datatype() != xsd::STRING {
        out.push(b'^');
        out.extend_from_slice(l.datatype().as_str().as_bytes());
    }
}

pub fn iri_key(iri: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(iri.len() + 1);
    out.push(b'<');
    out.extend_from_slice(iri.as_bytes());
    out
}

/// Parse a vocabulary key back into a term.
pub fn key_to_term(key: &[u8]) -> Term {
    match key.first() {
        Some(b'<') => Term::NamedNode(NamedNode::new_unchecked(utf8(&key[1..]))),
        Some(b'"') => {
            let sep = key.iter().rposition(|&b| b == KEY_SEP).unwrap_or(key.len());
            let lex = utf8(&key[1..sep]);
            let suffix = key.get(sep + 1..).unwrap_or(&[]);
            match suffix.first() {
                None => Term::Literal(Literal::new_simple_literal(lex)),
                Some(b'@') => Term::Literal(Literal::new_language_tagged_literal_unchecked(
                    lex,
                    utf8(&suffix[1..]),
                )),
                Some(_) => Term::Literal(Literal::new_typed_literal(
                    lex,
                    NamedNode::new_unchecked(utf8(&suffix[1..])),
                )),
            }
        }
        Some(b'_') => Term::BlankNode(BlankNode::new_unchecked(utf8(&key[2..]))),
        _ => Term::NamedNode(NamedNode::new_unchecked(utf8(key))),
    }
}

#[inline]
fn utf8(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

/// Canonicalize a numeric/boolean literal term: used when *computing* values so that
/// results produced by expressions get the canonical (inline-able) form.
pub fn is_key_iri(key: &[u8]) -> bool {
    key.first() == Some(&b'<')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn int_roundtrip() {
        for i in [0i64, 1, -1, 42, -(1 << 59), (1 << 59) - 1] {
            assert_eq!(Id::from_i64(i).unwrap().as_i64(), i);
        }
        assert!(Id::from_i64(1 << 59).is_none());
    }

    #[test]
    fn inline_only_canonical() {
        assert!(inline_literal("1", xsd::INTEGER.as_str()).is_some());
        assert!(inline_literal("01", xsd::INTEGER.as_str()).is_none());
        assert!(inline_literal("+1", xsd::INTEGER.as_str()).is_none());
        assert!(inline_literal("true", xsd::BOOLEAN.as_str()).is_some());
        assert!(inline_literal("1", xsd::BOOLEAN.as_str()).is_none());
        let d = inline_literal("1.5", xsd::DOUBLE.as_str());
        assert!(d.is_some(), "{}", oxsdatatypes::Double::from(1.5));
        assert_eq!(d.unwrap().as_f64(), 1.5);
    }

    #[test]
    fn key_roundtrip() {
        let terms = [
            Term::NamedNode(NamedNode::new_unchecked("http://ex.org/a")),
            Term::Literal(Literal::new_simple_literal("hi \"there\"")),
            Term::Literal(Literal::new_language_tagged_literal_unchecked("chat", "fr")),
            Term::Literal(Literal::new_typed_literal("2020-01-01", xsd::DATE)),
        ];
        for t in terms {
            assert_eq!(key_to_term(&term_key(&t)), t);
        }
    }
}
