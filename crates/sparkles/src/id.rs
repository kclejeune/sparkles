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
    /// Inline `xsd:decimal`: 4-bit scale + 56-bit two's complement mantissa (TDB2 `DecimalNode56`).
    Decimal = 9,
    /// Inline `xsd:dateTime` (packed calendar fields, see [`pack_date_time`]).
    DateTime = 10,
    /// Inline `xsd:date`.
    Date = 11,
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
            9 => Tag::Decimal,
            10 => Tag::DateTime,
            11 => Tag::Date,
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
        matches!(
            self.tag(),
            Tag::Bool | Tag::Int | Tag::Double | Tag::Decimal | Tag::DateTime | Tag::Date
        )
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
            Tag::Decimal => write!(f, "Dec:{}", unpack_decimal(self.payload())),
            Tag::DateTime | Tag::Date => write!(f, "T:{:x}", self.payload()),
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
    } else if datatype == xsd::DECIMAL.as_str() {
        let d: oxsdatatypes::Decimal = lex.parse().ok()?;
        (d.to_string() == lex).then_some(())?;
        pack_decimal(d).map(|p| Id::new(Tag::Decimal, p))
    } else if datatype == xsd::DATE_TIME.as_str() {
        let d: oxsdatatypes::DateTime = lex.parse().ok()?;
        let p = pack_date_time(
            d.year(),
            d.month(),
            d.day(),
            d.hour(),
            d.minute(),
            d.second(),
            tz_minutes(d.timezone_offset()),
        )?;
        (unpack_date_time(p, false) == lex).then(|| Id::new(Tag::DateTime, p))
    } else if datatype == xsd::DATE.as_str() {
        let d: oxsdatatypes::Date = lex.parse().ok()?;
        let p = pack_date_time(
            d.year(),
            d.month(),
            d.day(),
            0,
            0,
            0.into(),
            tz_minutes(d.timezone_offset()),
        )?;
        (unpack_date_time(p, true) == lex).then(|| Id::new(Tag::Date, p))
    } else {
        None
    }
}

fn tz_minutes(tz: Option<oxsdatatypes::TimezoneOffset>) -> Option<i16> {
    tz.map(|t| i16::from_be_bytes(t.to_be_bytes()))
}

const DEC_MANTISSA_BITS: u32 = 56;

/// Pack a decimal as `scale (4 bits) | mantissa (56 bits)`, value = mantissa / 10^scale.
pub fn pack_decimal(d: oxsdatatypes::Decimal) -> Option<u64> {
    // oxsdatatypes stores decimals as i128 with 18 fractional digits
    let v = i128::from_be_bytes(d.to_be_bytes());
    for scale in 0..16u32 {
        let div = 10i128.pow(18 - scale);
        if v % div == 0 {
            let m = v / div;
            let lim = 1i128 << (DEC_MANTISSA_BITS - 1);
            if m < -lim || m >= lim {
                return None;
            }
            return Some(
                ((scale as u64) << DEC_MANTISSA_BITS) | (m as u64 & ((1 << DEC_MANTISSA_BITS) - 1)),
            );
        }
    }
    None
}

pub fn unpack_decimal(p: u64) -> oxsdatatypes::Decimal {
    let scale = (p >> DEC_MANTISSA_BITS) as u32 & 0xF;
    let m = (((p << (64 - DEC_MANTISSA_BITS)) as i64) >> (64 - DEC_MANTISSA_BITS)) as i128;
    let v = m * 10i128.pow(18 - scale);
    oxsdatatypes::Decimal::from_be_bytes(v.to_be_bytes())
}

// dateTime layout (57 bits, most significant first):
//   year+8192 (14) | month (4) | day (5) | hour (5) | minute (6) | millis of minute (16) | tz (7)
// tz = 0: no timezone, else offset/15min + 64. Payload order is chronological for
// values in the same timezone.
fn pack_date_time(
    year: i64,
    month: u8,
    day: u8,
    hour: u8,
    minute: u8,
    second: oxsdatatypes::Decimal,
    tz: Option<i16>,
) -> Option<u64> {
    let y = year + 8192;
    if !(0..16384).contains(&y) {
        return None;
    }
    // milliseconds, only if the seconds have at most 3 fractional digits
    let sv = i128::from_be_bytes(second.to_be_bytes());
    let unit = 10i128.pow(15);
    if sv % unit != 0 {
        return None;
    }
    let ms = (sv / unit) as u64;
    if ms >= 60_000 {
        return None;
    }
    let tz = match tz {
        None => 0u64,
        Some(m) if m % 15 == 0 && (-56 * 15..=56 * 15).contains(&m) => (m / 15 + 64) as u64,
        Some(_) => return None,
    };
    Some(
        (y as u64) << 43
            | (month as u64) << 39
            | (day as u64) << 34
            | (hour as u64) << 29
            | (minute as u64) << 23
            | ms << 7
            | tz,
    )
}

/// Canonical lexical form of a packed dateTime (or date).
pub fn unpack_date_time(p: u64, date_only: bool) -> String {
    let year = ((p >> 43) & 0x3FFF) as i64 - 8192;
    let month = (p >> 39) & 0xF;
    let day = (p >> 34) & 0x1F;
    let hour = (p >> 29) & 0x1F;
    let minute = (p >> 23) & 0x3F;
    let ms = (p >> 7) & 0xFFFF;
    let tz = (p & 0x7F) as i64;
    let mut s = if year < 0 {
        format!("-{:04}", -year)
    } else {
        format!("{year:04}")
    };
    let _ = std::fmt::Write::write_fmt(&mut s, format_args!("-{month:02}-{day:02}"));
    if !date_only {
        let _ = std::fmt::Write::write_fmt(
            &mut s,
            format_args!("T{hour:02}:{minute:02}:{:02}", ms / 1000),
        );
        if !ms.is_multiple_of(1000) {
            let frac = format!("{:03}", ms % 1000);
            s.push('.');
            s.push_str(frac.trim_end_matches('0'));
        }
    }
    if tz != 0 {
        let off = (tz - 64) * 15;
        if off == 0 {
            s.push('Z');
        } else {
            let a = off.abs();
            let _ = std::fmt::Write::write_fmt(
                &mut s,
                format_args!(
                    "{}{:02}:{:02}",
                    if off < 0 { '-' } else { '+' },
                    a / 60,
                    a % 60
                ),
            );
        }
    }
    s
}

/// Decode an inline id back into a literal.
pub fn inline_to_literal(id: Id) -> Option<Literal> {
    Some(match id.tag() {
        Tag::Bool => {
            Literal::new_typed_literal(if id.as_bool() { "true" } else { "false" }, xsd::BOOLEAN)
        }
        Tag::Int => Literal::new_typed_literal(id.as_i64().to_string(), xsd::INTEGER),
        Tag::Double => Literal::new_typed_literal(
            oxsdatatypes::Double::from(id.as_f64()).to_string(),
            xsd::DOUBLE,
        ),
        Tag::Decimal => {
            Literal::new_typed_literal(unpack_decimal(id.payload()).to_string(), xsd::DECIMAL)
        }
        Tag::DateTime => {
            Literal::new_typed_literal(unpack_date_time(id.payload(), false), xsd::DATE_TIME)
        }
        Tag::Date => Literal::new_typed_literal(unpack_date_time(id.payload(), true), xsd::DATE),
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

/// The label of a blank node id's payload. A blank node of the store is `b<hex>`. One that
/// a query minted (`BNODE()`, a CONSTRUCT template, a SERVICE result), whose payload has
/// [`Id::LOCAL_BNODE_BIT`] set, is `q<hex>`, so it never reads as a stored node's label.
/// The hex digits are lowercase without leading zeros.
pub fn bnode_label(payload: u64) -> String {
    if payload & Id::LOCAL_BNODE_BIT != 0 {
        format!("q{:x}", payload & !Id::LOCAL_BNODE_BIT)
    } else {
        format!("b{payload:x}")
    }
}

/// The payload of a label exactly as [`bnode_label`] writes it, and `None` for any other
/// label: other spellings of the same number (`b01f`, `b1F`) and numbers too large for
/// the payload name nothing.
pub fn parse_bnode_payload(label: &str) -> Option<u64> {
    let (minted, hex) = match label.as_bytes().first()? {
        b'b' => (false, &label[1..]),
        b'q' => (true, &label[1..]),
        _ => return None,
    };
    let canonical = !hex.is_empty()
        && hex.len() <= 15
        && (hex == "0" || !hex.starts_with('0'))
        && hex.bytes().all(|c| matches!(c, b'0'..=b'9' | b'a'..=b'f'));
    let v = u64::from_str_radix(hex, 16)
        .ok()
        .filter(|&v| canonical && v < Id::LOCAL_BNODE_BIT)?;
    Some(if minted { v | Id::LOCAL_BNODE_BIT } else { v })
}

/// Blank node → id used inside triple-term keys when no store scope is available:
/// labels from [`bnode_label`] map back to their id, other labels hash into the space
/// of minted blank nodes, which the store never holds.
pub fn default_bnode_id(b: &BlankNode) -> u64 {
    if let Some(id) = parse_bnode_payload(b.as_str()) {
        return id;
    }
    let h = b.as_str().bytes().fold(0xcbf2_9ce4_8422_2325u64, |h, c| {
        (h ^ c as u64).wrapping_mul(0x100_0000_01b3)
    });
    (h & (PAYLOAD_MASK >> 1)) | Id::LOCAL_BNODE_BIT
}

pub fn write_term_key(term: &Term, out: &mut Vec<u8>) {
    write_term_key_with(term, out, &mut default_bnode_id)
}

/// Write the vocabulary key of a term. Triple terms (RDF 1.2) are encoded as
/// `'(' (varint len, key){3}` with blank nodes inside them as `'_' u64-be id`, so the
/// key of a triple term is canonical for the store's blank node identities.
pub fn write_term_key_with(
    term: &Term,
    out: &mut Vec<u8>,
    bnode: &mut dyn FnMut(&BlankNode) -> u64,
) {
    match term {
        Term::NamedNode(n) => {
            out.push(b'<');
            out.extend_from_slice(n.as_str().as_bytes());
        }
        Term::Literal(l) => write_literal_key(l, out),
        Term::BlankNode(b) => {
            out.push(b'_');
            out.extend_from_slice(&bnode(b).to_be_bytes());
        }
        Term::Triple(t) => {
            out.push(b'(');
            let mut part = Vec::new();
            let subject: Term = t.subject.clone().into();
            let predicate: Term = t.predicate.clone().into();
            for c in [&subject, &predicate, &t.object] {
                part.clear();
                write_term_key_with(c, &mut part, bnode);
                crate::vocab::write_varint(out, part.len() as u64);
                out.extend_from_slice(&part);
            }
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
        if let Some(dir) = l.direction() {
            out.extend_from_slice(match dir {
                oxrdf::BaseDirection::Ltr => b"--ltr",
                oxrdf::BaseDirection::Rtl => b"--rtl",
            });
        }
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

/// Language tag plus optional base direction from a key suffix (`en` / `en--ltr`).
fn lang_literal(lex: String, tag: &str) -> Literal {
    match tag.rsplit_once("--") {
        Some((lang, "ltr")) => Literal::new_directional_language_tagged_literal_unchecked(
            lex,
            lang,
            oxrdf::BaseDirection::Ltr,
        ),
        Some((lang, "rtl")) => Literal::new_directional_language_tagged_literal_unchecked(
            lex,
            lang,
            oxrdf::BaseDirection::Rtl,
        ),
        _ => Literal::new_language_tagged_literal_unchecked(lex, tag),
    }
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
                Some(b'@') => Term::Literal(lang_literal(lex, &utf8(&suffix[1..]))),
                Some(_) => Term::Literal(Literal::new_typed_literal(
                    lex,
                    NamedNode::new_unchecked(utf8(&suffix[1..])),
                )),
            }
        }
        Some(b'_') if key.len() == 9 => {
            let id = u64::from_be_bytes(key[1..9].try_into().unwrap());
            Term::BlankNode(BlankNode::new_unchecked(bnode_label(id & PAYLOAD_MASK)))
        }
        Some(b'(') => {
            let mut pos = 1;
            let mut comps = Vec::with_capacity(3);
            for _ in 0..3 {
                let len = crate::vocab::read_varint(key, &mut pos) as usize;
                comps.push(key_to_term(&key[pos..pos + len]));
                pos += len;
            }
            let o = comps.pop().unwrap();
            let p = comps.pop().unwrap();
            let s = comps.pop().unwrap();
            let subject = match s {
                Term::NamedNode(n) => oxrdf::NamedOrBlankNode::NamedNode(n),
                Term::BlankNode(b) => oxrdf::NamedOrBlankNode::BlankNode(b),
                _ => oxrdf::NamedOrBlankNode::BlankNode(BlankNode::new_unchecked("invalid")),
            };
            let Term::NamedNode(p) = p else {
                return Term::NamedNode(NamedNode::new_unchecked("urn:x-sparkles:invalid"));
            };
            Term::Triple(Box::new(oxrdf::Triple::new(subject, p, o)))
        }
        _ => Term::NamedNode(NamedNode::new_unchecked(utf8(key))),
    }
}

#[inline]
fn utf8(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

/// Canonicalize a numeric/boolean literal term: used when *computing* values so that
/// results produced by expressions get the canonical (inline-able) form.
pub fn is_key_triple(key: &[u8]) -> bool {
    key.first() == Some(&b'(')
}

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
    fn inline_decimal_and_dates() {
        for lex in [
            "0",
            "1.5",
            "-199999.02",
            "12345678.123456789",
            "0.000000000000001",
        ] {
            let id = inline_literal(lex, xsd::DECIMAL.as_str()).unwrap_or_else(|| panic!("{lex}"));
            assert_eq!(inline_to_literal(id).unwrap().value(), lex);
        }
        assert!(inline_literal("123456789.123456789", xsd::DECIMAL.as_str()).is_none());
        assert!(inline_literal("1.50", xsd::DECIMAL.as_str()).is_none());
        for lex in [
            "2020-03-04T10:00:00Z",
            "2006-08-23T09:00:00+01:00",
            "1999-12-31T23:59:59.5",
            "-0044-03-15T12:00:00-05:30",
        ] {
            let id =
                inline_literal(lex, xsd::DATE_TIME.as_str()).unwrap_or_else(|| panic!("{lex}"));
            assert_eq!(inline_to_literal(id).unwrap().value(), lex);
        }
        for lex in ["2001-01-01", "2006-08-23Z", "2006-08-23+00:00"] {
            if let Some(id) = inline_literal(lex, xsd::DATE.as_str()) {
                assert_eq!(inline_to_literal(id).unwrap().value(), lex);
            }
        }
        let a = inline_literal("2020-01-01T00:00:00Z", xsd::DATE_TIME.as_str()).unwrap();
        let b = inline_literal("2020-01-02T00:00:00Z", xsd::DATE_TIME.as_str()).unwrap();
        assert!(a < b);
    }

    #[test]
    fn rdf12_keys() {
        let t = Term::Triple(Box::new(oxrdf::Triple::new(
            BlankNode::new_unchecked("b2a"),
            NamedNode::new_unchecked("http://ex.org/p"),
            Term::Triple(Box::new(oxrdf::Triple::new(
                NamedNode::new_unchecked("http://ex.org/s"),
                NamedNode::new_unchecked("http://ex.org/q"),
                Literal::new_directional_language_tagged_literal_unchecked(
                    "hi",
                    "en",
                    oxrdf::BaseDirection::Rtl,
                ),
            ))),
        )));
        let k = term_key(&t);
        assert!(is_key_triple(&k));
        assert_eq!(key_to_term(&k), t);
        // literals < triple terms < IRIs in key order
        let lit = term_key(&Term::Literal(Literal::new_simple_literal("zzz")));
        let iri = term_key(&Term::NamedNode(NamedNode::new_unchecked("a:b")));
        assert!(lit < k && k < iri);
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
