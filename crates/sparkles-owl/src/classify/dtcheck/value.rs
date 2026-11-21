//! Data values compared by identity, and the value of a literal.
//!
//! Identity is what cardinalities and functional data properties count (Structural
//! Specification §9.3.6), so `+0` and `-0` of the floating-point datatypes are two
//! values and two date-times with the same instant and different offsets are two values
//! (§4.2 and §4.7). The order that facets use is kept beside identity in the range
//! normal forms.

use super::sets::Rat;
use crate::datatypes;
use crate::model::Literal;
use crate::vocab;
use num_bigint::BigInt;
use num_traits::Num;

/// Why a literal or a data range is not decided by the checker. Its axioms are reported
/// as used in part.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Unsupported {
    /// a literal whose lexical form is outside its datatype's lexical space (§5.7)
    IllTyped,
    /// an XML literal, whose values the checker keeps opaque
    XmlLiteral,
    /// a datatype outside the OWL 2 datatype map
    Datatype(String),
    /// the `xsd:pattern` facet, which needs an automaton
    Pattern,
    /// a facet pair outside the facet space, or a facet the checker does not take
    Facet(String),
    /// a value beyond the checker's representation, such as a year beyond i64
    Large,
}

/// The key of a floating-point value in the datatype's order, with `-0` just below
/// `+0`. NaN has the key [`NAN`], outside the order.
pub const NAN: u64 = u64::MAX;

pub fn float_key(f: f32) -> u64 {
    if f.is_nan() {
        return NAN;
    }
    let b = f.to_bits();
    u64::from(if b >> 31 == 1 { !b } else { b | 1 << 31 })
}

pub fn double_key(f: f64) -> u64 {
    if f.is_nan() {
        return NAN;
    }
    let b = f.to_bits();
    if b >> 63 == 1 { !b } else { b | 1 << 63 }
}

/// A data value of the OWL 2 datatype map, compared by identity.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum DVal {
    /// a value of `owl:real` that a literal can denote, a rational
    Real(Rat),
    Float(u64),
    Double(u64),
    Str(String),
    /// a string and its language tag in lower case
    Lang(String, String),
    Uri(String),
    Hex(Vec<u8>),
    B64(Vec<u8>),
    Bool(bool),
    /// a date-time: the seconds of its instant on the time line, or of its local time
    /// read as if it were UTC when it has no offset, and its offset in minutes
    Time(Rat, Option<i16>),
}

impl DVal {
    pub fn bytes(&self) -> usize {
        size_of::<DVal>()
            + match self {
                DVal::Real(q) | DVal::Time(q, _) => q.bytes(),
                DVal::Str(s) | DVal::Uri(s) => s.len(),
                DVal::Lang(s, t) => s.len() + t.len(),
                DVal::Hex(b) | DVal::B64(b) => b.len(),
                _ => 0,
            }
    }
}

fn integer(s: &str) -> BigInt {
    let s = s.strip_prefix('+').unwrap_or(s);
    BigInt::from_str_radix(s, 10).expect("checked lexical form")
}

/// `(\+|-)?([0-9]+(\.[0-9]*)?|\.[0-9]+)` as an exact number.
pub fn decimal(s: &str) -> Rat {
    let (neg, u) = match s.as_bytes().first() {
        Some(b'-') => (true, &s[1..]),
        Some(b'+') => (false, &s[1..]),
        _ => (false, s),
    };
    let (i, f) = u.split_once('.').unwrap_or((u, ""));
    let digits = format!("{}{f}", if i.is_empty() { "0" } else { i });
    let mut n = BigInt::from_str_radix(&digits, 10).expect("checked lexical form");
    if neg {
        n = -n;
    }
    Rat::new(n, BigInt::from(10u32).pow(f.len() as u32))
}

fn real_value(dt: &str, lex: &str) -> Option<Rat> {
    if dt == vocab::OWL_RATIONAL {
        let (n, d) = lex.split_once('/')?;
        return Some(Rat::new(integer(n), integer(d)));
    }
    let l = dt.strip_prefix(vocab::XSD)?;
    match l {
        "decimal" => Some(decimal(lex)),
        "integer" | "nonNegativeInteger" | "nonPositiveInteger" | "positiveInteger"
        | "negativeInteger" | "long" | "int" | "short" | "byte" | "unsignedLong"
        | "unsignedInt" | "unsignedShort" | "unsignedByte" => Some(Rat::int(integer(lex))),
        _ => None,
    }
}

fn special(lex: &str) -> Option<f64> {
    Some(match lex {
        "INF" | "+INF" => f64::INFINITY,
        "-INF" => f64::NEG_INFINITY,
        "NaN" => f64::NAN,
        _ => return None,
    })
}

/// The days from 1970-01-01 to a date of the proleptic Gregorian calendar, with
/// astronomical year numbering, so that year 0 is 1 BCE as in XML Schema 1.1.
fn days_from_civil(y: i64, m: i64, d: i64) -> i128 {
    let y = i128::from(if m <= 2 { y - 1 } else { y });
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let m = i128::from(m);
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + i128::from(d) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

fn two(s: &str) -> Option<i64> {
    (s.len() == 2 && s.bytes().all(|b| b.is_ascii_digit())).then(|| s.parse().ok())?
}

fn days_in_month(y: i64, m: i64) -> i64 {
    match m {
        2 if y % 4 == 0 && (y % 100 != 0 || y % 400 == 0) => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

/// The value of an `xsd:dateTime` lexical form of XML Schema 1.1 §3.3.7,
/// `-?yyyy-mm-ddThh:mm:ss(.s+)?(Z|(+|-)hh:mm)?`, with years of any length and seconds
/// of any precision, or why it has none.
pub fn date_time(lex: &str) -> Result<DVal, Unsupported> {
    let bad = Unsupported::IllTyped;
    let (neg, s) = match lex.strip_prefix('-') {
        Some(r) => (true, r),
        None => (false, lex),
    };
    let (date, rest) = s.split_once('T').ok_or(bad.clone())?;
    let mut dp = date.splitn(3, '-');
    let ys = dp.next().ok_or(bad.clone())?;
    let month = dp.next().and_then(two).ok_or(bad.clone())?;
    let day = dp.next().and_then(two).ok_or(bad.clone())?;
    if ys.len() < 4
        || !ys.bytes().all(|b| b.is_ascii_digit())
        || (ys.len() > 4 && ys.starts_with('0'))
    {
        return Err(bad);
    }
    if ys.len() > 15 {
        return Err(Unsupported::Large);
    }
    let year: i64 = ys.parse().map_err(|_| Unsupported::Large)?;
    let year = if neg { -year } else { year };
    if !(1..=12).contains(&month) || day < 1 || day > days_in_month(year, month) {
        return Err(bad);
    }
    let (time, offset) = if let Some(t) = rest.strip_suffix('Z') {
        (t, Some(0i16))
    } else if rest.len() > 6 && matches!(rest.as_bytes()[rest.len() - 6], b'+' | b'-') {
        let (t, o) = rest.split_at(rest.len() - 6);
        let sign = if o.starts_with('-') { -1 } else { 1 };
        let h = two(&o[1..3]).ok_or(bad.clone())?;
        let m = two(&o[4..6]).ok_or(bad.clone())?;
        if &o[3..4] != ":" || m > 59 || h > 14 || (h == 14 && m != 0) {
            return Err(bad);
        }
        (t, Some(sign * (h * 60 + m) as i16))
    } else {
        (rest, None)
    };
    let tp: Vec<&str> = time.splitn(3, ':').collect();
    let [hs, ms, ss] = tp[..] else {
        return Err(bad);
    };
    let h = two(hs).ok_or(bad.clone())?;
    let mi = two(ms).ok_or(bad.clone())?;
    let (whole, frac) = ss.split_once('.').unwrap_or((ss, ""));
    let sec_whole = two(whole).ok_or(bad.clone())?;
    if ss.contains('.') && (frac.is_empty() || !frac.bytes().all(|b| b.is_ascii_digit())) {
        return Err(bad);
    }
    let zero_frac = frac.bytes().all(|b| b == b'0');
    if mi > 59 || sec_whole > 59 || h > 24 || (h == 24 && (mi != 0 || sec_whole != 0 || !zero_frac))
    {
        return Err(bad);
    }
    let sec = decimal(ss);
    let base = days_from_civil(year, month, day) * 86400 + i128::from(h * 3600 + mi * 60);
    let mut at = Rat::int(base).add(&sec);
    if let Some(o) = offset {
        at = at.plus(-i64::from(o) * 60);
    }
    Ok(DVal::Time(at, offset))
}

fn hex(lex: &str) -> Vec<u8> {
    lex.as_bytes()
        .chunks(2)
        .map(|c| u8::from_str_radix(std::str::from_utf8(c).expect("ascii"), 16).expect("hex"))
        .collect()
}

fn base64(lex: &str) -> Vec<u8> {
    let mut bits = 0u32;
    let mut n = 0;
    let mut out = Vec::new();
    for b in lex.bytes() {
        let v = match b {
            b'A'..=b'Z' => b - b'A',
            b'a'..=b'z' => b - b'a' + 26,
            b'0'..=b'9' => b - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => continue,
        };
        bits = bits << 6 | u32::from(v);
        n += 6;
        if n >= 8 {
            n -= 8;
            out.push((bits >> n) as u8);
            bits &= (1 << n) - 1;
        }
    }
    out
}

/// The value of a literal, or why the checker keeps it opaque.
pub fn of_literal(l: &Literal) -> Result<DVal, Unsupported> {
    let lex = l.value();
    if let Some(tag) = l.language() {
        return Ok(DVal::Lang(lex.to_string(), tag.to_ascii_lowercase()));
    }
    let dt = l.datatype();
    if dt == vocab::RDF_XML_LITERAL {
        return Err(Unsupported::XmlLiteral);
    }
    let local = dt.strip_prefix(vocab::XSD).unwrap_or("");
    if matches!(local, "dateTime" | "dateTimeStamp") {
        // checked here, since `datatypes::lexical` refuses fractions of a second past
        // eighteen digits, which XML Schema 1.1 allows
        return match date_time(lex)? {
            DVal::Time(_, None) if local == "dateTimeStamp" => Err(Unsupported::IllTyped),
            v => Ok(v),
        };
    }
    match datatypes::lexical(dt, lex) {
        None => return Err(Unsupported::Datatype(dt.to_string())),
        Some(false) => return Err(Unsupported::IllTyped),
        Some(true) => {}
    }
    if dt == vocab::RDF_PLAIN_LITERAL {
        let (text, tag) = lex.rsplit_once('@').ok_or(Unsupported::IllTyped)?;
        return Ok(if tag.is_empty() {
            DVal::Str(text.to_string())
        } else {
            DVal::Lang(text.to_string(), tag.to_ascii_lowercase())
        });
    }
    if dt == vocab::RDF_LANG_STRING {
        // a langString without a tag has no value
        return Err(Unsupported::IllTyped);
    }
    if let Some(q) = real_value(dt, lex) {
        return Ok(DVal::Real(q));
    }
    Ok(match local {
        "string" | "normalizedString" | "token" | "language" | "Name" | "NCName" | "NMTOKEN" => {
            DVal::Str(lex.to_string())
        }
        "boolean" => DVal::Bool(matches!(lex, "true" | "1")),
        "double" => DVal::Double(double_key(
            special(lex).unwrap_or_else(|| lex.parse().expect("checked lexical form")),
        )),
        "float" => DVal::Float(float_key(match special(lex) {
            Some(f) => f as f32,
            None => lex.parse().expect("checked lexical form"),
        })),
        "anyURI" => DVal::Uri(lex.to_string()),
        "hexBinary" => DVal::Hex(hex(lex)),
        "base64Binary" => DVal::B64(base64(lex)),
        _ => return Err(Unsupported::Datatype(dt.to_string())),
    })
}
