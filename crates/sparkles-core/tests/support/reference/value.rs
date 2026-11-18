//! Typed values of RDF terms: the numeric tower, booleans, strings and the orders of
//! SPARQL 1.1 §15.1 and §17. Everything here reads `oxrdf` terms and `oxsdatatypes`
//! values and nothing of the engine.

use oxrdf::vocab::{rdf, xsd};
use oxrdf::{Literal, NamedNode, Term};
use oxsdatatypes::{Boolean, DateTime, Decimal, Double, Float};
use std::cmp::Ordering;
use std::str::FromStr;

/// A numeric value after type promotion: `integer` (all derived integer types),
/// `decimal`, `float` or `double`.
#[derive(Debug, Clone, Copy)]
pub enum Num {
    Int(i128),
    Dec(Decimal),
    Flt(f32),
    Dbl(f64),
}

/// The bounds of an integer datatype, if `dt` is one.
fn integer_bounds(dt: &str) -> Option<(i128, i128)> {
    let local = dt.strip_prefix(xsd::INTEGER.as_str().trim_end_matches("integer"))?;
    Some(match local {
        "integer" => (i128::MIN, i128::MAX),
        "nonPositiveInteger" => (i128::MIN, 0),
        "negativeInteger" => (i128::MIN, -1),
        "long" => (i64::MIN as i128, i64::MAX as i128),
        "int" => (i32::MIN as i128, i32::MAX as i128),
        "short" => (i16::MIN as i128, i16::MAX as i128),
        "byte" => (i8::MIN as i128, i8::MAX as i128),
        "nonNegativeInteger" => (0, i128::MAX),
        "unsignedLong" => (0, u64::MAX as i128),
        "unsignedInt" => (0, u32::MAX as i128),
        "unsignedShort" => (0, u16::MAX as i128),
        "unsignedByte" => (0, u8::MAX as i128),
        "positiveInteger" => (1, i128::MAX),
        _ => return None,
    })
}

/// Whether the datatype is one of the numeric datatypes, whatever the lexical form.
pub fn is_numeric_datatype(dt: &str) -> bool {
    integer_bounds(dt).is_some()
        || dt == xsd::DECIMAL.as_str()
        || dt == xsd::FLOAT.as_str()
        || dt == xsd::DOUBLE.as_str()
}

fn parse_integer(lex: &str) -> Option<i128> {
    let digits = lex.strip_prefix(['+', '-']).unwrap_or(lex);
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    lex.parse().ok()
}

thread_local! {
    static BEYOND_MINIMUM_PRECISION: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static SPARQL11_EBV: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Read the effective boolean value of an ill-formed literal as SPARQL 1.1 does, false,
/// or as SPARQL 1.2 does, an error, in the evaluations that follow on this thread.
pub fn set_sparql11_ebv(sparql11: bool) {
    SPARQL11_EBV.with(|c| c.set(sparql11));
}

/// Whether `lex` is in the lexical space of `xsd:float` and `xsd:double`, which has
/// decimal and exponent forms, `INF`, `+INF`, `-INF` and `NaN` and nothing else, where
/// a Rust float parse also takes forms such as `inf` and `infinity`.
pub fn is_float_lexical(lex: &str) -> bool {
    if matches!(lex, "INF" | "+INF" | "-INF" | "NaN") {
        return true;
    }
    let body = lex.strip_prefix(['+', '-']).unwrap_or(lex);
    let (mantissa, exponent) = match body.find(['e', 'E']) {
        Some(i) => (&body[..i], Some(&body[i + 1..])),
        None => (body, None),
    };
    let (whole, fraction) = match mantissa.find('.') {
        Some(i) => (&mantissa[..i], Some(&mantissa[i + 1..])),
        None => (mantissa, None),
    };
    let digits = |s: &str| s.bytes().all(|b| b.is_ascii_digit());
    let mantissa_ok = digits(whole)
        && fraction.is_none_or(digits)
        && !(whole.is_empty() && fraction.is_none_or(str::is_empty));
    let exponent_ok = exponent.is_none_or(|e| {
        let e = e.strip_prefix(['+', '-']).unwrap_or(e);
        !e.is_empty() && digits(e)
    });
    mantissa_ok && exponent_ok
}

/// Whether [`numeric`] read an integer or decimal with more than 16 digits since the last
/// call, which XSD 1.1 section 5.4 lets an implementation limit. The call resets it.
pub fn take_beyond_minimum_precision() -> bool {
    BEYOND_MINIMUM_PRECISION.with(|c| c.replace(false))
}

fn note_digits(lex: &str) {
    let digits = lex
        .trim_start_matches(['+', '-'])
        .trim_start_matches('0')
        .bytes()
        .filter(u8::is_ascii_digit)
        .count();
    if digits > 16 {
        BEYOND_MINIMUM_PRECISION.with(|c| c.set(true));
    }
}

/// The numeric value of a well-formed numeric literal.
pub fn numeric(l: &Literal) -> Option<Num> {
    let dt = l.datatype();
    let dt = dt.as_str();
    if integer_bounds(dt).is_some() || dt == xsd::DECIMAL.as_str() {
        note_digits(l.value());
    }
    if let Some((lo, hi)) = integer_bounds(dt) {
        let v = parse_integer(l.value())?;
        return (lo <= v && v <= hi).then_some(Num::Int(v));
    }
    if dt == xsd::DECIMAL.as_str() {
        let lex = l.value();
        // the lexical space has no exponent and no special values
        if lex.contains(['e', 'E']) || lex.is_empty() {
            return None;
        }
        return Decimal::from_str(lex).ok().map(Num::Dec);
    }
    if dt == xsd::FLOAT.as_str() {
        if !is_float_lexical(l.value()) {
            return None;
        }
        return Float::from_str(l.value())
            .ok()
            .map(|f| Num::Flt(f32::from(f)));
    }
    if dt == xsd::DOUBLE.as_str() {
        if !is_float_lexical(l.value()) {
            return None;
        }
        return Double::from_str(l.value())
            .ok()
            .map(|f| Num::Dbl(f64::from(f)));
    }
    None
}

pub fn numeric_term(t: &Term) -> Option<Num> {
    match t {
        Term::Literal(l) => numeric(l),
        _ => None,
    }
}

impl Num {
    fn rank(self) -> u8 {
        match self {
            Num::Int(_) => 0,
            Num::Dec(_) => 1,
            Num::Flt(_) => 2,
            Num::Dbl(_) => 3,
        }
    }

    pub fn to_f64(self) -> f64 {
        match self {
            Num::Int(i) => i as f64,
            Num::Dec(d) => f64::from(Double::from(d)),
            Num::Flt(f) => f as f64,
            Num::Dbl(d) => d,
        }
    }

    fn to_f32(self) -> f32 {
        match self {
            Num::Flt(f) => f,
            other => other.to_f64() as f32,
        }
    }

    fn to_decimal(self) -> Option<Decimal> {
        match self {
            Num::Int(i) => Decimal::try_from(i).ok(),
            Num::Dec(d) => Some(d),
            _ => None,
        }
    }

    pub fn is_nan(self) -> bool {
        match self {
            Num::Flt(f) => f.is_nan(),
            Num::Dbl(d) => d.is_nan(),
            _ => false,
        }
    }

    pub fn to_literal(self) -> Literal {
        match self {
            Num::Int(i) => Literal::new_typed_literal(i.to_string(), xsd::INTEGER),
            Num::Dec(d) => Literal::new_typed_literal(d.to_string(), xsd::DECIMAL),
            Num::Flt(f) => Literal::new_typed_literal(Float::from(f).to_string(), xsd::FLOAT),
            Num::Dbl(d) => Literal::new_typed_literal(Double::from(d).to_string(), xsd::DOUBLE),
        }
    }

    pub fn to_term(self) -> Term {
        Term::Literal(self.to_literal())
    }

    /// The canonical lexical form of the value in its own type.
    pub fn canonical_lexical(self) -> String {
        match self {
            Num::Int(i) => i.to_string(),
            Num::Dec(d) => d.to_string(),
            Num::Flt(f) => Float::from(f).to_string(),
            Num::Dbl(d) => Double::from(d).to_string(),
        }
    }

    /// The value cast to `xsd:string`, by the rules of XPath and XQuery Functions and
    /// Operators 3.1 section 19.1.2.1. An integer and a decimal with an integral value
    /// print as an integer, a decimal otherwise without trailing zeros. A float or double
    /// with an absolute value from one millionth to below one million prints as a
    /// decimal, and any other as a mantissa with one digit before the point and at least
    /// one after, `E` and the exponent, such as `1.0E6`.
    pub fn xpath_string(self) -> String {
        let float = |v: f64, shortest: String| -> String {
            if v.is_nan() {
                return "NaN".into();
            }
            if v.is_infinite() {
                return if v > 0.0 { "INF" } else { "-INF" }.into();
            }
            if v == 0.0 {
                return if v.is_sign_negative() { "-0" } else { "0" }.into();
            }
            let a = v.abs();
            if (1e-6..1e6).contains(&a) {
                // the shortest decimal that reads back as the value
                return shortest;
            }
            // scientific notation from the shortest digits
            let sci = format!("{v:e}");
            let (mantissa, exponent) = sci.split_once('e').expect("an exponent");
            let mantissa = if mantissa.contains('.') {
                mantissa.to_string()
            } else {
                format!("{mantissa}.0")
            };
            format!("{mantissa}E{exponent}")
        };
        match self {
            Num::Int(i) => i.to_string(),
            Num::Dec(d) => {
                let s = d.to_string();
                if s.contains('.') {
                    let t = s.trim_end_matches('0').trim_end_matches('.');
                    t.to_string()
                } else {
                    s
                }
            }
            Num::Flt(f) => {
                let shortest = format!("{f}");
                // the float's own shortest digits, not the double's
                if (1e-6..1e6).contains(&(f.abs() as f64)) || !f.is_finite() || f == 0.0 {
                    float(f as f64, shortest)
                } else {
                    let sci = format!("{f:e}");
                    let (mantissa, exponent) = sci.split_once('e').expect("an exponent");
                    let mantissa = if mantissa.contains('.') {
                        mantissa.to_string()
                    } else {
                        format!("{mantissa}.0")
                    };
                    format!("{mantissa}E{exponent}")
                }
            }
            Num::Dbl(d) => float(d, format!("{d}")),
        }
    }

    /// The value comparison of the operator mapping: `None` when a NaN takes part.
    pub fn compare(self, other: Num) -> Option<Ordering> {
        match (self, other) {
            (Num::Int(a), Num::Int(b)) => Some(a.cmp(&b)),
            _ => match self.rank().max(other.rank()) {
                1 => match (self.to_decimal(), other.to_decimal()) {
                    (Some(a), Some(b)) => a.partial_cmp(&b),
                    _ => self.to_f64().partial_cmp(&other.to_f64()),
                },
                2 => self.to_f32().partial_cmp(&other.to_f32()),
                _ => self.to_f64().partial_cmp(&other.to_f64()),
            },
        }
    }

    /// An arithmetic result of the two operands after promotion, or `None` on a type
    /// error: an overflow, or a division by zero outside `float` and `double`.
    pub fn arith(self, op: Arith, other: Num) -> Option<Num> {
        let rank = self.rank().max(other.rank());
        match rank {
            0 => {
                let (Num::Int(a), Num::Int(b)) = (self, other) else {
                    unreachable!()
                };
                match op {
                    Arith::Add => a.checked_add(b).map(Num::Int),
                    Arith::Sub => a.checked_sub(b).map(Num::Int),
                    Arith::Mul => a.checked_mul(b).map(Num::Int),
                    // xsd:integer / xsd:integer is an xsd:decimal
                    Arith::Div => Decimal::try_from(a)
                        .ok()?
                        .checked_div(Decimal::try_from(b).ok()?)
                        .map(Num::Dec),
                }
            }
            1 => {
                let (a, b) = (self.to_decimal()?, other.to_decimal()?);
                match op {
                    Arith::Add => a.checked_add(b),
                    Arith::Sub => a.checked_sub(b),
                    Arith::Mul => a.checked_mul(b),
                    Arith::Div => a.checked_div(b),
                }
                .map(Num::Dec)
            }
            2 => {
                let (a, b) = (self.to_f32(), other.to_f32());
                Some(Num::Flt(match op {
                    Arith::Add => a + b,
                    Arith::Sub => a - b,
                    Arith::Mul => a * b,
                    Arith::Div => a / b,
                }))
            }
            _ => {
                let (a, b) = (self.to_f64(), other.to_f64());
                Some(Num::Dbl(match op {
                    Arith::Add => a + b,
                    Arith::Sub => a - b,
                    Arith::Mul => a * b,
                    Arith::Div => a / b,
                }))
            }
        }
    }

    pub fn neg(self) -> Option<Num> {
        match self {
            Num::Int(i) => i.checked_neg().map(Num::Int),
            Num::Dec(d) => d.checked_neg().map(Num::Dec),
            Num::Flt(f) => Some(Num::Flt(-f)),
            Num::Dbl(d) => Some(Num::Dbl(-d)),
        }
    }

    pub fn abs(self) -> Option<Num> {
        match self {
            Num::Int(i) => i.checked_abs().map(Num::Int),
            Num::Dec(d) => d.checked_abs().map(Num::Dec),
            Num::Flt(f) => Some(Num::Flt(f.abs())),
            Num::Dbl(d) => Some(Num::Dbl(d.abs())),
        }
    }

    pub fn ceil(self) -> Option<Num> {
        match self {
            Num::Int(_) => Some(self),
            Num::Dec(d) => d.checked_ceil().map(Num::Dec),
            Num::Flt(f) => Some(Num::Flt(f.ceil())),
            Num::Dbl(d) => Some(Num::Dbl(d.ceil())),
        }
    }

    pub fn floor(self) -> Option<Num> {
        match self {
            Num::Int(_) => Some(self),
            Num::Dec(d) => d.checked_floor().map(Num::Dec),
            Num::Flt(f) => Some(Num::Flt(f.floor())),
            Num::Dbl(d) => Some(Num::Dbl(d.floor())),
        }
    }

    /// `fn:round`: the closest integral value, halves toward positive infinity.
    pub fn round(self) -> Option<Num> {
        fn half_up(x: f64) -> f64 {
            if !x.is_finite() {
                return x;
            }
            let f = x.floor();
            if x - f >= 0.5 { f + 1.0 } else { f }
        }
        match self {
            Num::Int(_) => Some(self),
            Num::Dec(d) => {
                let half = Decimal::from_str("0.5").ok()?;
                d.checked_add(half)?.checked_floor().map(Num::Dec)
            }
            Num::Flt(f) => Some(Num::Flt(half_up(f as f64) as f32)),
            Num::Dbl(d) => Some(Num::Dbl(half_up(d))),
        }
    }

    /// The effective boolean value of a number: false for zero and NaN.
    pub fn truthy(self) -> bool {
        match self {
            Num::Int(i) => i != 0,
            Num::Dec(d) => d != Decimal::from(0),
            Num::Flt(f) => f != 0.0 && !f.is_nan(),
            Num::Dbl(d) => d != 0.0 && !d.is_nan(),
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub enum Arith {
    Add,
    Sub,
    Mul,
    Div,
}

/// `xsd:string` or a language-tagged string: the "string literals" of the string
/// functions.
pub fn is_string_literal(l: &Literal) -> bool {
    let dt = l.datatype();
    dt == xsd::STRING || dt == rdf::LANG_STRING || dt.as_str() == DIR_LANG_STRING
}

pub const DIR_LANG_STRING: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#dirLangString";

/// A literal without a language tag whose datatype is `xsd:string`.
pub fn is_simple(l: &Literal) -> bool {
    l.datatype() == xsd::STRING
}

pub fn boolean(l: &Literal) -> Option<bool> {
    (l.datatype() == xsd::BOOLEAN)
        .then(|| Boolean::from_str(l.value()).ok().map(bool::from))
        .flatten()
}

pub fn date(l: &Literal) -> Option<oxsdatatypes::Date> {
    (l.datatype() == xsd::DATE)
        .then(|| oxsdatatypes::Date::from_str(l.value()).ok())
        .flatten()
}

pub fn date_time(l: &Literal) -> Option<DateTime> {
    (l.datatype() == xsd::DATE_TIME)
        .then(|| DateTime::from_str(l.value()).ok())
        .flatten()
}

pub fn bool_term(b: bool) -> Term {
    Term::Literal(Literal::from(b))
}

pub fn string_term(s: impl Into<String>) -> Term {
    Term::Literal(Literal::new_simple_literal(s))
}

pub fn int_term(i: i128) -> Term {
    Num::Int(i).to_term()
}

/// The effective boolean value of SPARQL 1.2 §17.2.3, or of SPARQL 1.1 §17.2.2 where
/// [`set_sparql11_ebv`] asks for it, `None` for a type error.
pub fn ebv(t: &Term) -> Option<bool> {
    let Term::Literal(l) = t else { return None };
    let dt = l.datatype();
    // An ill-formed boolean or number has no effective boolean value in SPARQL 1.2,
    // and SPARQL 1.1 reads it as false.
    let ill_formed = || SPARQL11_EBV.with(|c| c.get()).then_some(false);
    if dt == xsd::BOOLEAN {
        return boolean(l).or_else(ill_formed);
    }
    if dt == xsd::STRING {
        return Some(!l.value().is_empty());
    }
    if is_numeric_datatype(dt.as_str()) {
        return numeric(l).map(Num::truthy).or_else(ill_formed);
    }
    None
}

/// The class of a term for ORDER BY: unbound, blank node, IRI, literal.
fn class(t: Option<&Term>) -> u8 {
    match t {
        None => 0,
        Some(Term::BlankNode(_)) => 1,
        Some(Term::NamedNode(_)) => 2,
        Some(Term::Literal(_)) => 3,
        Some(Term::Triple(_)) => 4,
    }
}

/// What the specification orders: `Some` where SPARQL 1.1 §15.1 and the operator
/// mapping of §17.3 define the order of the two terms, and `None` for the pairs that
/// it leaves to the implementation, such as an integer against a string.
pub fn spec_order(a: Option<&Term>, b: Option<&Term>) -> Option<Ordering> {
    let (ca, cb) = (class(a), class(b));
    if ca != cb {
        return Some(ca.cmp(&cb));
    }
    match (a, b) {
        (None, None) => Some(Ordering::Equal),
        // blank nodes have no order among themselves
        (Some(Term::BlankNode(_)), Some(Term::BlankNode(_))) => None,
        (Some(Term::NamedNode(x)), Some(Term::NamedNode(y))) => Some(x.as_str().cmp(y.as_str())),
        // SPARQL 1.2 section 15.1 leaves two triple terms unordered
        (Some(Term::Triple(x)), Some(Term::Triple(y))) => (x == y).then_some(Ordering::Equal),
        (Some(Term::Literal(x)), Some(Term::Literal(y))) => literal_order(x, y),
        _ => None,
    }
}

fn literal_order(x: &Literal, y: &Literal) -> Option<Ordering> {
    if let (Some(a), Some(b)) = (numeric(x), numeric(y)) {
        // the operator mapping does not order NaN against any number
        return a.compare(b);
    }
    if is_simple(x) && is_simple(y) {
        return Some(x.value().cmp(y.value()));
    }
    if let (Some(a), Some(b)) = (boolean(x), boolean(y)) {
        return Some(a.cmp(&b));
    }
    if let (Some(a), Some(b)) = (date_time(x), date_time(y)) {
        return a.partial_cmp(&b);
    }
    if x == y {
        return Some(Ordering::Equal);
    }
    None
}

/// A total order that extends [`spec_order`], for sorting. Each literal falls into a
/// group, numbers first, then simple strings, booleans, date-times and everything else.
/// The groups are ordered among themselves, and the terms of a group by value, so the
/// order stays transitive where the specification leaves pairs open. Ties of value fall
/// back to the datatype, the lexical form and the language.
pub fn total_order(a: Option<&Term>, b: Option<&Term>) -> Ordering {
    let (ca, cb) = (class(a), class(b));
    if ca != cb {
        return ca.cmp(&cb);
    }
    match (a, b) {
        (Some(Term::Literal(x)), Some(Term::Literal(y))) => {
            let (gx, gy) = (group(x), group(y));
            gx.cmp(&gy)
                .then_with(|| match gx {
                    0 => {
                        let (p, q) = (numeric(x).unwrap(), numeric(y).unwrap());
                        // NaN first, then by value
                        q.is_nan().cmp(&p.is_nan()).then_with(|| {
                            p.compare(q)
                                .unwrap_or_else(|| p.to_f64().total_cmp(&q.to_f64()))
                        })
                    }
                    1 => x.value().cmp(y.value()),
                    2 => boolean(x).cmp(&boolean(y)),
                    3 => utc_date_time(x)
                        .partial_cmp(&utc_date_time(y))
                        .unwrap_or(Ordering::Equal),
                    _ => Ordering::Equal,
                })
                .then_with(|| x.datatype().as_str().cmp(y.datatype().as_str()))
                .then_with(|| x.value().cmp(y.value()))
                .then_with(|| x.language().cmp(&y.language()))
        }
        (Some(Term::Triple(x)), Some(Term::Triple(y))) => total_order(
            Some(&Term::from(x.subject.clone())),
            Some(&Term::from(y.subject.clone())),
        )
        .then_with(|| x.predicate.as_str().cmp(y.predicate.as_str()))
        .then_with(|| total_order(Some(&x.object), Some(&y.object))),
        (Some(Term::NamedNode(x)), Some(Term::NamedNode(y))) => x.as_str().cmp(y.as_str()),
        (Some(Term::BlankNode(x)), Some(Term::BlankNode(y))) => x.as_str().cmp(y.as_str()),
        _ => Ordering::Equal,
    }
}

/// The group of a literal in [`total_order`].
fn group(l: &Literal) -> u8 {
    if numeric(l).is_some() {
        0
    } else if is_simple(l) {
        1
    } else if boolean(l).is_some() {
        2
    } else if date_time(l).is_some() {
        3
    } else {
        4
    }
}

/// A date-time, read as UTC when it has no timezone. Where the specification orders two
/// date-times, this reading orders them the same way.
fn utc_date_time(l: &Literal) -> Option<DateTime> {
    let d = date_time(l)?;
    if d.timezone_offset().is_some() {
        return Some(d);
    }
    DateTime::from_str(&format!("{}Z", l.value())).ok()
}

/// The same term, as in `sameTerm`.
pub fn same_term(a: &Term, b: &Term) -> bool {
    a == b
}

pub fn named(s: &str) -> NamedNode {
    NamedNode::new_unchecked(s)
}
