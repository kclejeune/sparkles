//! SPARQL value space (ARQ `NodeValue` equivalent): typed values, promotion,
//! comparison, effective boolean value and ordering.

use super::bignum::{BigDec, BigInt};
use oxrdf::vocab::{rdf, xsd};
use oxrdf::{Literal, NamedNode, Term};
use oxsdatatypes::*;
use std::cmp::Ordering;
use std::fmt;
use std::ops::Deref;
use std::sync::Arc;

/// Canonical arbitrary-precision integer payload of [`Value::BigInteger`].
///
/// Constructible only through [`Value::from_bigint`] (or internal exact-sum helpers) so
/// every `Value::BigInteger` stays strictly outside the `i64` range and within
/// [`super::bignum::MAX_DIGITS`].
#[repr(transparent)]
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BigIntValue(Arc<BigInt>);

impl Deref for BigIntValue {
    type Target = BigInt;
    #[inline]
    fn deref(&self) -> &BigInt {
        &self.0
    }
}

impl AsRef<BigInt> for BigIntValue {
    #[inline]
    fn as_ref(&self) -> &BigInt {
        &self.0
    }
}

impl fmt::Display for BigIntValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

/// Canonical arbitrary-precision decimal payload of [`Value::BigDecimal`].
///
/// Constructible only through [`Value::from_bigdec`] so every `Value::BigDecimal` stays
/// strictly outside `oxsdatatypes::Decimal` and within [`super::bignum::MAX_DIGITS`] and
/// [`super::bignum::MAX_SCALE`].
#[repr(transparent)]
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BigDecValue(Arc<BigDec>);

impl Deref for BigDecValue {
    type Target = BigDec;
    #[inline]
    fn deref(&self) -> &BigDec {
        &self.0
    }
}

impl AsRef<BigDec> for BigDecValue {
    #[inline]
    fn as_ref(&self) -> &BigDec {
        &self.0
    }
}

impl fmt::Display for BigDecValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

#[derive(Clone, Debug)]
pub enum Value {
    Iri(Arc<str>),
    /// blank node, kept by label
    BNode(Arc<str>),
    /// simple literal or xsd:string
    Str(Arc<str>),
    Lang(Arc<str>, Arc<str>),
    /// RDF 1.2 directional language-tagged string (`rdf:dirLangString`)
    LangDir(Arc<str>, Arc<str>, oxrdf::BaseDirection),
    /// RDF 1.2 triple term
    Triple(Arc<oxrdf::Triple>),
    Bool(bool),
    Integer(Integer),
    /// `xsd:integer` (or derived integer) outside the `i64` range
    BigInteger(BigIntValue),
    Decimal(Decimal),
    /// `xsd:decimal` outside `oxsdatatypes::Decimal`
    BigDecimal(BigDecValue),
    Float(Float),
    Double(Double),
    DateTime(DateTime),
    Date(Date),
    Time(Time),
    Duration(Duration),
    YearMonth(YearMonthDuration),
    DayTime(DayTimeDuration),
    /// any other (or ill-typed) literal
    Other {
        lex: Arc<str>,
        dt: Arc<str>,
    },
}

/// Language tag and optional RDF 1.2 base direction of a string literal argument.
pub(crate) type StringLang<'a> = Option<(&'a str, Option<oxrdf::BaseDirection>)>;

/// A SPARQL expression type error (evaluation → unbound / filter false).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TypeError;

pub type EvalResult<T> = Result<T, TypeError>;

const INTEGER_DERIVED: &[&str] = &[
    "http://www.w3.org/2001/XMLSchema#long",
    "http://www.w3.org/2001/XMLSchema#int",
    "http://www.w3.org/2001/XMLSchema#short",
    "http://www.w3.org/2001/XMLSchema#byte",
    "http://www.w3.org/2001/XMLSchema#nonNegativeInteger",
    "http://www.w3.org/2001/XMLSchema#nonPositiveInteger",
    "http://www.w3.org/2001/XMLSchema#positiveInteger",
    "http://www.w3.org/2001/XMLSchema#negativeInteger",
    "http://www.w3.org/2001/XMLSchema#unsignedLong",
    "http://www.w3.org/2001/XMLSchema#unsignedInt",
    "http://www.w3.org/2001/XMLSchema#unsignedShort",
    "http://www.w3.org/2001/XMLSchema#unsignedByte",
];

/// Whether [`Value::from_typed`] gives a number for some lexical form of `dt`.
pub(crate) fn numeric_datatype(dt: &str) -> bool {
    dt == xsd::INTEGER.as_str()
        || dt == xsd::DECIMAL.as_str()
        || dt == xsd::DOUBLE.as_str()
        || dt == xsd::FLOAT.as_str()
        || INTEGER_DERIVED.contains(&dt)
}

/// Parse an `xsd:float` literal lexical form strictly according to XSD 1.1.
#[inline]
pub(crate) fn parse_float(lex: &str) -> Option<Float> {
    crate::xsd::is_float_lexical(lex)
        .then(|| lex.parse().ok())
        .flatten()
}

/// Parse an `xsd:double` literal lexical form strictly according to XSD 1.1.
#[inline]
pub(crate) fn parse_double(lex: &str) -> Option<Double> {
    crate::xsd::is_float_lexical(lex)
        .then(|| lex.parse().ok())
        .flatten()
}

fn format_xpath_sci(sci: &str) -> String {
    let (mantissa, exponent) = sci.split_once('e').expect("scientific format has 'e'");
    if mantissa.contains('.') {
        format!("{mantissa}E{exponent}")
    } else {
        format!("{mantissa}.0E{exponent}")
    }
}

/// Format an `xsd:float` cast to `xsd:string` per XPath F&O 3.1 §19.1.2.1.
pub(crate) fn float_to_xpath_string(f: f32) -> String {
    if f.is_nan() {
        return "NaN".into();
    }
    if f.is_infinite() {
        return if f > 0.0 { "INF" } else { "-INF" }.into();
    }
    if f == 0.0 {
        return if f.is_sign_negative() { "-0" } else { "0" }.into();
    }
    if (1e-6_f64..1e6_f64).contains(&(f.abs() as f64)) {
        format!("{f}")
    } else {
        format_xpath_sci(&format!("{f:e}"))
    }
}

/// Format an `xsd:double` cast to `xsd:string` per XPath F&O 3.1 §19.1.2.1.
pub(crate) fn double_to_xpath_string(d: f64) -> String {
    if d.is_nan() {
        return "NaN".into();
    }
    if d.is_infinite() {
        return if d > 0.0 { "INF" } else { "-INF" }.into();
    }
    if d == 0.0 {
        return if d.is_sign_negative() { "-0" } else { "0" }.into();
    }
    if (1e-6_f64..1e6_f64).contains(&d.abs()) {
        format!("{d}")
    } else {
        format_xpath_sci(&format!("{d:e}"))
    }
}

/// Whether a signed `i64` satisfies the XSD bounds of a derived integer datatype.
#[inline]
fn derived_i64_in_range(v: i64, dt: &str) -> bool {
    match dt.rsplit('#').next().unwrap_or("") {
        "long" => true,
        "int" => i32::try_from(v).is_ok(),
        "short" => i16::try_from(v).is_ok(),
        "byte" => i8::try_from(v).is_ok(),
        "nonNegativeInteger" | "unsignedLong" => v >= 0,
        "positiveInteger" => v >= 1,
        "nonPositiveInteger" => v <= 0,
        "negativeInteger" => v <= -1,
        "unsignedInt" => u32::try_from(v).is_ok(),
        "unsignedShort" => u16::try_from(v).is_ok(),
        "unsignedByte" => u8::try_from(v).is_ok(),
        _ => false,
    }
}

/// Whether `v` satisfies the XSD value-space bounds of `xsd:integer` or a derived integer
/// datatype.
pub(crate) fn integer_in_derived_range(v: &BigInt, dt: &str) -> bool {
    match dt.rsplit('#').next().unwrap_or("") {
        "integer" => true,
        "long" => v.to_i64().is_some(),
        "int" => v.to_i64().is_some_and(|n| i32::try_from(n).is_ok()),
        "short" => v.to_i64().is_some_and(|n| i16::try_from(n).is_ok()),
        "byte" => v.to_i64().is_some_and(|n| i8::try_from(n).is_ok()),
        "nonNegativeInteger" => !v.is_negative(),
        "positiveInteger" => v.is_positive(),
        "nonPositiveInteger" => !v.is_positive(),
        "negativeInteger" => v.is_negative(),
        "unsignedLong" => v.to_u64().is_some(),
        "unsignedInt" => v.to_i64().is_some_and(|n| u32::try_from(n).is_ok()),
        "unsignedShort" => v.to_i64().is_some_and(|n| u16::try_from(n).is_ok()),
        "unsignedByte" => v.to_i64().is_some_and(|n| u8::try_from(n).is_ok()),
        _ => false,
    }
}

#[cold]
fn parse_big_integer_or_other(lex: &str, dt: &str) -> Value {
    if let Some(bi) = BigInt::parse(lex)
        && integer_in_derived_range(&bi, dt)
    {
        return Value::from_bigint(bi);
    }
    Value::Other {
        lex: lex.into(),
        dt: dt.into(),
    }
}

#[cold]
fn parse_big_decimal_or_other(lex: &str, dt: &str) -> Value {
    if let Some(bd) = BigDec::parse(lex) {
        return Value::from_bigdec(bd);
    }
    Value::Other {
        lex: lex.into(),
        dt: dt.into(),
    }
}

#[cold]
fn numeric_key_big(lex: &str, dt: &str) -> Option<f64> {
    if dt == xsd::DECIMAL.as_str() {
        return BigDec::parse(lex).map(|d| {
            d.to_oxs_decimal()
                .map(decimal_f64)
                .unwrap_or_else(|| d.to_f64())
        });
    }
    let bi = BigInt::parse(lex)?;
    integer_in_derived_range(&bi, dt).then(|| bi.to_f64())
}
/// Numeric subset of from_key without allocating Other/lexical Arc values.
/// Ordinary fixed-width numbers stay on fixed scalar/stack storage; out-of-range
/// integers and decimals fall back to the cold arbitrary-precision parser.
pub(crate) fn numeric_key(key: &[u8]) -> Option<f64> {
    if key.first() != Some(&b'"') {
        return None;
    }
    let sep = key.iter().rposition(|&b| b == crate::id::KEY_SEP)?;
    let lex = std::str::from_utf8(key.get(1..sep)?).ok()?;
    let suffix = key.get(sep + 1..)?;
    if suffix.first().is_none_or(|&b| b == b'@') {
        return None;
    }
    let dt = std::str::from_utf8(suffix.get(1..)?).ok()?;
    if !numeric_datatype(dt) {
        return None;
    }
    if dt == xsd::DECIMAL.as_str() {
        return match lex.parse::<Decimal>() {
            Ok(d) => Some(decimal_f64(d)),
            Err(_) => numeric_key_big(lex, dt),
        };
    }
    if dt == xsd::FLOAT.as_str() {
        let f = parse_float(lex)?;
        let d = f64::from(f32::from(f));
        return (!d.is_nan()).then_some(d);
    }
    if dt == xsd::DOUBLE.as_str() {
        let d = f64::from(parse_double(lex)?);
        return (!d.is_nan()).then_some(d);
    }
    match lex.parse::<Integer>() {
        Ok(i) => {
            if dt == xsd::INTEGER.as_str() || derived_i64_in_range(i64::from(i), dt) {
                Some(i64::from(i) as f64)
            } else {
                None
            }
        }
        Err(_) => numeric_key_big(lex, dt),
    }
}

impl Value {
    pub fn from_term(t: &Term) -> Value {
        match t {
            Term::NamedNode(n) => Value::Iri(n.as_str().into()),
            Term::BlankNode(b) => Value::BNode(b.as_str().into()),
            Term::Literal(l) => Value::from_literal(l),
            Term::Triple(t) => Value::Triple(Arc::new((**t).clone())),
        }
    }

    pub fn from_literal(l: &Literal) -> Value {
        match l.language() {
            Some(lang) => match l.direction() {
                Some(d) => Value::LangDir(l.value().into(), lang.into(), d),
                None => Value::Lang(l.value().into(), lang.into()),
            },
            None => Value::from_typed(l.value(), l.datatype().as_str()),
        }
    }

    /// Decode a vocabulary key (see [`crate::id::term_key`]) directly into a value,
    /// without building an intermediate `Term`.
    pub fn from_key(key: &[u8]) -> Value {
        fn s(b: &[u8]) -> std::borrow::Cow<'_, str> {
            String::from_utf8_lossy(b)
        }
        match key.first() {
            Some(b'<') => Value::Iri(s(&key[1..]).into()),
            Some(b'"') => {
                let sep = key
                    .iter()
                    .rposition(|&b| b == crate::id::KEY_SEP)
                    .unwrap_or(key.len());
                let lex = s(&key[1..sep]);
                let suffix = key.get(sep + 1..).unwrap_or(&[]);
                match suffix.first() {
                    None => Value::Str(lex.into()),
                    Some(b'@') => {
                        let tag = s(&suffix[1..]);
                        match tag.rsplit_once("--") {
                            Some((l, "ltr")) => {
                                Value::LangDir(lex.into(), l.into(), oxrdf::BaseDirection::Ltr)
                            }
                            Some((l, "rtl")) => {
                                Value::LangDir(lex.into(), l.into(), oxrdf::BaseDirection::Rtl)
                            }
                            _ => Value::Lang(lex.into(), tag.into()),
                        }
                    }
                    Some(_) => Value::from_typed(&lex, &s(&suffix[1..])),
                }
            }
            _ => Value::from_term(&crate::id::key_to_term(key)),
        }
    }

    /// Value of a typed literal given its lexical form and datatype IRI.
    pub fn from_typed(lex: &str, dt: &str) -> Value {
        let other = || Value::Other {
            lex: lex.into(),
            dt: dt.into(),
        };
        macro_rules! parse {
            ($variant:ident) => {
                lex.parse().map(Value::$variant).unwrap_or_else(|_| other())
            };
        }
        match dt {
            s if s == xsd::STRING.as_str() => Value::Str(lex.into()),
            s if s == xsd::BOOLEAN.as_str() => match lex {
                "true" | "1" => Value::Bool(true),
                "false" | "0" => Value::Bool(false),
                _ => other(),
            },
            s if s == xsd::INTEGER.as_str() => match lex.parse() {
                Ok(i) => Value::Integer(i),
                Err(_) => parse_big_integer_or_other(lex, dt),
            },
            s if s == xsd::DECIMAL.as_str() => match lex.parse() {
                Ok(d) => Value::Decimal(d),
                Err(_) => parse_big_decimal_or_other(lex, dt),
            },
            s if s == xsd::DOUBLE.as_str() => {
                parse_double(lex).map(Value::Double).unwrap_or_else(other)
            }
            s if s == xsd::FLOAT.as_str() => {
                parse_float(lex).map(Value::Float).unwrap_or_else(other)
            }
            s if s == xsd::DATE_TIME.as_str() || s == xsd::DATE_TIME_STAMP.as_str() => {
                parse!(DateTime)
            }
            s if s == xsd::DATE.as_str() => parse!(Date),
            s if s == xsd::TIME.as_str() => parse!(Time),
            s if s == xsd::DURATION.as_str() => parse!(Duration),
            s if s == xsd::YEAR_MONTH_DURATION.as_str() => parse!(YearMonth),
            s if s == xsd::DAY_TIME_DURATION.as_str() => parse!(DayTime),
            s if INTEGER_DERIVED.contains(&s) => match lex.parse::<Integer>() {
                Ok(i) if derived_i64_in_range(i64::from(i), dt) => Value::Integer(i),
                Ok(_) => other(),
                Err(_) => parse_big_integer_or_other(lex, dt),
            },
            _ => other(),
        }
    }

    pub fn from_bigint(i: BigInt) -> Value {
        match i.to_i64() {
            Some(n) => Value::Integer(n.into()),
            None => Value::BigInteger(BigIntValue(Arc::new(i))),
        }
    }

    pub fn from_bigdec(d: BigDec) -> Value {
        match d.to_oxs_decimal() {
            Some(dec) => Value::Decimal(dec),
            None => Value::BigDecimal(BigDecValue(Arc::new(d))),
        }
    }

    pub fn to_term(&self) -> Term {
        let typed = |lex: String, dt: NamedNode| Term::Literal(Literal::new_typed_literal(lex, dt));
        match self {
            Value::Iri(i) => Term::NamedNode(NamedNode::new_unchecked(&**i)),
            Value::BNode(b) => Term::BlankNode(oxrdf::BlankNode::new_unchecked(&**b)),
            Value::Str(s) => Term::Literal(Literal::new_simple_literal(&**s)),
            Value::LangDir(s, l, d) => Term::Literal(
                Literal::new_directional_language_tagged_literal_unchecked(&**s, &**l, *d),
            ),
            Value::Triple(t) => Term::Triple(Box::new((**t).clone())),
            Value::Lang(s, l) => {
                Term::Literal(Literal::new_language_tagged_literal_unchecked(&**s, &**l))
            }
            Value::Bool(b) => typed(b.to_string(), xsd::BOOLEAN.into()),
            Value::Integer(i) => typed(i.to_string(), xsd::INTEGER.into()),
            Value::BigInteger(i) => typed(i.0.to_lexical_string(), xsd::INTEGER.into()),
            Value::Decimal(d) => typed(d.to_string(), xsd::DECIMAL.into()),
            Value::BigDecimal(d) => typed(d.0.to_lexical_string(), xsd::DECIMAL.into()),
            Value::Float(f) => typed(f.to_string(), xsd::FLOAT.into()),
            Value::Double(d) => typed(d.to_string(), xsd::DOUBLE.into()),
            Value::DateTime(d) => typed(d.to_string(), xsd::DATE_TIME.into()),
            Value::Date(d) => typed(d.to_string(), xsd::DATE.into()),
            Value::Time(d) => typed(d.to_string(), xsd::TIME.into()),
            Value::Duration(d) => typed(d.to_string(), xsd::DURATION.into()),
            Value::YearMonth(d) => typed(d.to_string(), xsd::YEAR_MONTH_DURATION.into()),
            Value::DayTime(d) => typed(d.to_string(), xsd::DAY_TIME_DURATION.into()),
            Value::Other { lex, dt } => typed(lex.to_string(), NamedNode::new_unchecked(&**dt)),
        }
    }

    pub fn is_literal(&self) -> bool {
        !matches!(self, Value::Iri(_) | Value::BNode(_) | Value::Triple(_))
    }
    pub fn is_numeric(&self) -> bool {
        matches!(
            self,
            Value::Integer(_)
                | Value::BigInteger(_)
                | Value::Decimal(_)
                | Value::BigDecimal(_)
                | Value::Float(_)
                | Value::Double(_)
        )
    }
    /// simple literal or xsd:string
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::Str(s) => Some(s),
            _ => None,
        }
    }
    /// "string literal" argument per SPARQL 17.4.3 (simple, `xsd:string` or lang-tagged).
    pub fn string_arg(&self) -> EvalResult<(&str, Option<&str>)> {
        match self {
            Value::Str(s) => Ok((s, None)),
            Value::Lang(s, l) | Value::LangDir(s, l, _) => Ok((s, Some(l))),
            _ => Err(TypeError),
        }
    }
    /// "string literal" argument with optional RDF 1.2 base direction for
    /// SPARQL 1.2 compatibility and result-kind preservation.
    pub(crate) fn string_arg_dir(&self) -> EvalResult<(&str, StringLang<'_>)> {
        match self {
            Value::Str(s) => Ok((s, None)),
            Value::Lang(s, l) => Ok((s, Some((l, None)))),
            Value::LangDir(s, l, d) => Ok((s, Some((l, Some(*d))))),
            _ => Err(TypeError),
        }
    }
    /// Lexical form (STR()).
    /// Whether the value is a string literal as SPARQL's string functions take one: a
    /// simple literal, an `xsd:string` or a language-tagged string.
    pub fn is_string(&self) -> bool {
        matches!(self, Value::Str(_) | Value::Lang(..) | Value::LangDir(..))
    }

    pub fn lexical(&self) -> EvalResult<Arc<str>> {
        Ok(match self {
            Value::Iri(i) => i.clone(),
            Value::BNode(_) => return Err(TypeError),
            Value::Str(s) | Value::Lang(s, _) | Value::LangDir(s, _, _) => s.clone(),
            Value::Triple(_) => return Err(TypeError),
            Value::Other { lex, .. } => lex.clone(),
            v => match v.to_term() {
                Term::Literal(l) => l.value().into(),
                _ => return Err(TypeError),
            },
        })
    }

    pub fn datatype(&self) -> EvalResult<NamedNode> {
        Ok(match self {
            Value::Iri(_) | Value::BNode(_) => return Err(TypeError),
            Value::Str(_) => xsd::STRING.into(),
            Value::Lang(..) => rdf::LANG_STRING.into(),
            Value::LangDir(..) => rdf::DIR_LANG_STRING.into(),
            Value::Triple(_) => return Err(TypeError),
            Value::Other { dt, .. } => NamedNode::new_unchecked(&**dt),
            v => match v.to_term() {
                Term::Literal(l) => l.datatype().into_owned(),
                _ => return Err(TypeError),
            },
        })
    }

    /// Effective boolean value (SPARQL 17.2.2).
    pub fn ebv(&self) -> EvalResult<bool> {
        match self {
            Value::Bool(b) => Ok(*b),
            Value::Str(s) => Ok(!s.is_empty()),
            Value::Integer(i) => Ok(i64::from(*i) != 0),
            Value::BigInteger(i) => Ok(!i.is_zero()),
            Value::Decimal(d) => Ok(*d != Decimal::from(0)),
            Value::BigDecimal(d) => Ok(!d.is_zero()),
            Value::Float(f) => Ok(!(f32::from(*f) == 0.0 || f.is_nan())),
            Value::Double(d) => Ok(!(f64::from(*d) == 0.0 || d.is_nan())),
            // SPARQL 1.2 (§17.2.2): ill-typed boolean / numeric literals have no
            // effective boolean value (SPARQL 1.1 said `false`)
            _ => Err(TypeError),
        }
    }
}

// ------------------------------------------------------------------ numerics ----

#[derive(Clone, Copy, Debug)]
pub enum Num<'a> {
    Integer(Integer),
    BigInteger(&'a BigInt),
    Decimal(Decimal),
    BigDecimal(&'a BigDec),
    Float(Float),
    Double(Double),
}

impl<'a> Num<'a> {
    pub fn of(v: &'a Value) -> EvalResult<Num<'a>> {
        Ok(match v {
            Value::Integer(i) => Num::Integer(*i),
            Value::BigInteger(i) => Num::BigInteger(i),
            Value::Decimal(d) => Num::Decimal(*d),
            Value::BigDecimal(d) => Num::BigDecimal(d),
            Value::Float(f) => Num::Float(*f),
            Value::Double(d) => Num::Double(*d),
            _ => return Err(TypeError),
        })
    }
    pub(crate) fn rank(self) -> u8 {
        match self {
            Num::Integer(_) | Num::BigInteger(_) => 0,
            Num::Decimal(_) | Num::BigDecimal(_) => 1,
            Num::Float(_) => 2,
            Num::Double(_) => 3,
        }
    }
    pub(crate) fn to_decimal(self) -> Option<Decimal> {
        match self {
            Num::Integer(i) => Some(i.into()),
            Num::Decimal(d) => Some(d),
            _ => None,
        }
    }
    pub(crate) fn to_big_int(self) -> Option<BigInt> {
        match self {
            Num::Integer(i) => Some(BigInt::from_i64(i64::from(i))),
            Num::BigInteger(i) => Some(i.clone()),
            _ => None,
        }
    }
    pub(crate) fn to_big_dec(self) -> Option<BigDec> {
        match self {
            Num::Integer(i) => Some(BigDec::from_i64(i64::from(i))),
            Num::BigInteger(i) => Some(BigDec::from_bigint(i.clone())),
            Num::Decimal(d) => Some(BigDec::from_oxs_decimal(d)),
            Num::BigDecimal(d) => Some(d.clone()),
            _ => None,
        }
    }
    pub(crate) fn to_float(self) -> Float {
        match self {
            Num::Integer(i) => i.into(),
            Num::BigInteger(i) => Float::from(i.to_f32()),
            Num::Decimal(d) => d.into(),
            Num::BigDecimal(d) => Float::from(d.to_f32()),
            Num::Float(f) => f,
            Num::Double(d) => d.into(),
        }
    }
    pub fn to_double(self) -> Double {
        match self {
            Num::Integer(i) => i.into(),
            Num::BigInteger(i) => Double::from(i.to_f64()),
            Num::Decimal(d) => d.into(),
            Num::BigDecimal(d) => Double::from(d.to_f64()),
            Num::Float(f) => f.into(),
            Num::Double(d) => d,
        }
    }
    pub fn into_value(self) -> Value {
        match self {
            Num::Integer(i) => Value::Integer(i),
            Num::BigInteger(i) => Value::from_bigint(i.clone()),
            Num::Decimal(d) => Value::Decimal(d),
            Num::BigDecimal(d) => Value::from_bigdec(d.clone()),
            Num::Float(f) => Value::Float(f),
            Num::Double(d) => Value::Double(d),
        }
    }
}

pub enum NumOp {
    Add,
    Sub,
    Mul,
    Div,
}

#[cold]
fn arith_big(op: NumOp, rank: u8, a: Num<'_>, b: Num<'_>) -> EvalResult<Value> {
    if rank == 0 && !matches!(op, NumOp::Div) {
        let x = a.to_big_int().ok_or(TypeError)?;
        let y = b.to_big_int().ok_or(TypeError)?;
        let r = match op {
            NumOp::Add => x.checked_add(&y),
            NumOp::Sub => x.checked_sub(&y),
            NumOp::Mul => x.checked_mul(&y),
            NumOp::Div => unreachable!(),
        };
        return r.map(Value::from_bigint).ok_or(TypeError);
    }
    let x = a.to_big_dec().ok_or(TypeError)?;
    let y = b.to_big_dec().ok_or(TypeError)?;
    let r = match op {
        NumOp::Add => x.checked_add(&y),
        NumOp::Sub => x.checked_sub(&y),
        NumOp::Mul => x.checked_mul(&y),
        NumOp::Div => x.checked_div(&y),
    };
    r.map(Value::from_bigdec).ok_or(TypeError)
}

pub fn arith(op: NumOp, a: &Value, b: &Value) -> EvalResult<Value> {
    let (a, b) = (Num::of(a), Num::of(b));
    let (a, b) = match (a, b) {
        (Ok(a), Ok(b)) => (a, b),
        _ => return Err(TypeError),
    };
    let rank = a.rank().max(b.rank());
    Ok(match rank {
        0 | 1 => {
            if rank == 0 && !matches!(op, NumOp::Div) {
                if let (Num::Integer(x), Num::Integer(y)) = (a, b) {
                    let r = match op {
                        NumOp::Add => x.checked_add(y),
                        NumOp::Sub => x.checked_sub(y),
                        NumOp::Mul => x.checked_mul(y),
                        NumOp::Div => unreachable!(),
                    };
                    if let Some(v) = r {
                        return Ok(Value::Integer(v));
                    }
                }
                return arith_big(op, 0, a, b);
            }
            if let Some(r) = exact_decimal(&op, a, b) {
                if let Some(v) = r {
                    return Ok(Value::Decimal(v));
                }
                return arith_big(op, 1, a, b);
            }
            if let (Some(x), Some(y)) = (a.to_decimal(), b.to_decimal()) {
                let r = match op {
                    NumOp::Add => x.checked_add(y),
                    NumOp::Sub => x.checked_sub(y),
                    NumOp::Mul => x.checked_mul(y),
                    NumOp::Div => {
                        if y == Decimal::from(0) {
                            return Err(TypeError);
                        }
                        if decimal_div_needs_big(x, y) {
                            None
                        } else {
                            x.checked_div(y)
                        }
                    }
                };
                if let Some(v) = r {
                    return Ok(Value::Decimal(v));
                }
            }
            return arith_big(op, 1, a, b);
        }
        2 => {
            let (x, y) = (a.to_float(), b.to_float());
            Value::Float(match op {
                NumOp::Add => x + y,
                NumOp::Sub => x - y,
                NumOp::Mul => x * y,
                NumOp::Div => x / y,
            })
        }
        _ => {
            let (x, y) = (a.to_double(), b.to_double());
            Value::Double(match op {
                NumOp::Add => x + y,
                NumOp::Sub => x - y,
                NumOp::Mul => x * y,
                NumOp::Div => x / y,
            })
        }
    })
}

/// The scale of oxsdatatypes' decimals, which hold `value × 10^-18` in an `i128`.
pub(crate) const DECIMAL_SCALE: i128 = 1_000_000_000_000_000_000;

const DEC_Q_MAX: i128 = i128::MAX.div_euclid(DECIMAL_SCALE);
const DEC_R_MAX: i128 = i128::MAX.rem_euclid(DECIMAL_SCALE);
const DEC_Q_MIN: i128 = i128::MIN.div_euclid(DECIMAL_SCALE);
const DEC_R_MIN: i128 = i128::MIN.rem_euclid(DECIMAL_SCALE);

/// The raw `i128` value (`d × 10^18`) stored inside an `oxsdatatypes::Decimal`.
#[inline]
pub(crate) fn decimal_raw(d: Decimal) -> i128 {
    i128::from_be_bytes(d.to_be_bytes())
}

/// Construct an `oxsdatatypes::Decimal` from its raw `i128` value (`d × 10^18`).
#[inline]
pub(crate) fn decimal_from_raw(raw: i128) -> Decimal {
    Decimal::from_be_bytes(raw.to_be_bytes())
}

/// Exact `dec + int * 10^18` checked over `[i128::MIN, i128::MAX]` without overflowing
/// when `int * 10^18` alone exceeds `i128` while the sum with `dec` stays in range.
pub(crate) fn add_scaled_int(dec: i128, int: i128) -> Option<i128> {
    let q = dec.div_euclid(DECIMAL_SCALE);
    let r = dec.rem_euclid(DECIMAL_SCALE);
    let nq = q.checked_add(int)?;
    if nq > DEC_Q_MAX || (nq == DEC_Q_MAX && r > DEC_R_MAX) {
        return None;
    }
    if nq < DEC_Q_MIN || (nq == DEC_Q_MIN && r < DEC_R_MIN) {
        return None;
    }
    if nq == DEC_Q_MIN {
        Some((nq + 1) * DECIMAL_SCALE - (DECIMAL_SCALE - r))
    } else {
        Some(nq * DECIMAL_SCALE + r)
    }
}

/// Whether `num / den` (`den > 0`) terminates in base 10.
#[inline]
fn u128_terminates_base10(num: u128, mut den: u128) -> bool {
    den >>= den.trailing_zeros();
    while den.is_multiple_of(5) {
        den /= 5;
    }
    num.is_multiple_of(den)
}

/// Whether `x / y` (`y != 0`) is a terminating decimal quotient whose exact fractional
/// scale exceeds 18 digits (so the fixed-width `Decimal` path would truncate it).
pub(crate) fn decimal_div_needs_big(x: Decimal, y: Decimal) -> bool {
    let rx = i128::from_be_bytes(x.to_be_bytes()).unsigned_abs();
    if rx == 0 {
        return false;
    }
    let ry = i128::from_be_bytes(y.to_be_bytes()).unsigned_abs();
    let p2_y = ry.trailing_zeros();
    let mut r = ry >> p2_y;
    let mut p5_y = 0u32;
    while r.is_multiple_of(5) {
        r /= 5;
        p5_y += 1;
    }
    if !rx.is_multiple_of(r) {
        return false;
    }
    let q0 = rx / r;
    let p2_q = q0.trailing_zeros();
    let mut q5 = q0 >> p2_q;
    let mut p5_q = 0u32;
    while q5.is_multiple_of(5) {
        q5 /= 5;
        p5_q += 1;
    }
    p2_y.saturating_sub(p2_q).max(p5_y.saturating_sub(p5_q)) > 18
}

/// An exact integer sum as a value to add further numbers to. It is an `xsd:integer`
/// (`Value::Integer` when it fits `i64`, `Value::BigInteger` otherwise).
pub(crate) fn exact_sum(sum: i128) -> Value {
    match i64::try_from(sum) {
        Ok(sum) => Value::Integer(sum.into()),
        Err(_) => Value::BigInteger(BigIntValue(Arc::new(BigInt::from_i128(sum)))),
    }
}

/// An AVG of a fixed-point subtotal can stay in the fixed-width decimal type
/// exactly when the raw scaled subtotal is divisible by the row count.
pub(crate) fn avg_fixed_raw(raw: i128, rows: u64) -> Option<Decimal> {
    let rows = i128::from(rows);
    (raw.checked_rem(rows)? == 0).then(|| decimal_from_raw(raw / rows))
}

/// Use the same division for every exact subtotal, even one which grew past the
/// digit cap before division but whose final quotient fits.
pub(crate) fn avg_big_decimal(sum: &BigDec, rows: u64) -> Option<Value> {
    sum.checked_avg(rows).map(Value::from_bigdec)
}

/// The decimal product or quotient of `a` and `b` in one `i128` operation, for the
/// operands whose oxsdatatypes result it is known to equal, or `None` to use
/// oxsdatatypes (or the big-decimal fallback for terminating quotients needing >18
/// fractional digits). The general operations cancel scale factors or strip trailing
/// zeros with repeated 128-bit division.
///
/// A product with an integer `j` is the other operand's value times `j`.
/// oxsdatatypes 0.2.4 cancels the scale's factors before multiplying and returns
/// zero for a zero operand. The exact product overflows exactly when it does.
/// The quotient of two integers `i / j` with `j` nonzero is `i × 10^18 / j`
/// truncated toward zero, except when `i / j` terminates in base 10 with more than 18
/// fractional digits, in which case `None` delegates to `BigDec::checked_div`.
fn exact_decimal(op: &NumOp, a: Num<'_>, b: Num<'_>) -> Option<Option<Decimal>> {
    let raw = |d: Decimal| i128::from_be_bytes(d.to_be_bytes());
    let decimal = |v: i128| Decimal::from_be_bytes(v.to_be_bytes());
    match (op, a, b) {
        (NumOp::Mul, Num::Decimal(d), Num::Integer(j))
        | (NumOp::Mul, Num::Integer(j), Num::Decimal(d)) => {
            Some(raw(d).checked_mul(i128::from(i64::from(j))).map(decimal))
        }
        (NumOp::Div, Num::Integer(i), Num::Integer(j)) if i64::from(j) != 0 => {
            let ii = i64::from(i);
            let jj = i64::from(j);
            let num = i128::from(ii) * DECIMAL_SCALE;
            let den = i128::from(jj);
            let q = num / den;
            let rem = num % den;
            if rem != 0
                && u128_terminates_base10(ii.unsigned_abs() as u128, jj.unsigned_abs() as u128)
            {
                return None;
            }
            Some(Some(decimal(q)))
        }
        _ => None,
    }
}

#[derive(Clone, Debug)]
enum NumOwned {
    Integer(Integer),
    BigInteger(Arc<BigInt>),
    Decimal(Decimal),
    BigDecimal(Arc<BigDec>),
    Float(Float),
    Double(Double),
}

impl NumOwned {
    fn as_num(&self) -> Num<'_> {
        match self {
            NumOwned::Integer(i) => Num::Integer(*i),
            NumOwned::BigInteger(i) => Num::BigInteger(i),
            NumOwned::Decimal(d) => Num::Decimal(*d),
            NumOwned::BigDecimal(d) => Num::BigDecimal(d),
            NumOwned::Float(f) => Num::Float(*f),
            NumOwned::Double(d) => Num::Double(*d),
        }
    }
}

/// A numeric constant prepared for many comparisons: its value in each type that
/// [`num_cmp`] promotes it to, converted once. [`NumConst::order`] gives `num_cmp(x, c)`.
#[derive(Clone, Debug)]
pub struct NumConst {
    c: NumOwned,
    rank: u8,
    decimal: Option<Decimal>,
    float: Float,
    double: Double,
}

impl NumConst {
    pub fn new(c: Num<'_>) -> NumConst {
        let owned = match c {
            Num::Integer(i) => NumOwned::Integer(i),
            Num::BigInteger(i) => NumOwned::BigInteger(Arc::new(i.clone())),
            Num::Decimal(d) => NumOwned::Decimal(d),
            Num::BigDecimal(d) => NumOwned::BigDecimal(Arc::new(d.clone())),
            Num::Float(f) => NumOwned::Float(f),
            Num::Double(d) => NumOwned::Double(d),
        };
        NumConst {
            c: owned,
            rank: c.rank(),
            decimal: c.to_decimal(),
            float: c.to_float(),
            double: c.to_double(),
        }
    }

    /// `num_cmp(x, c)`.
    #[inline]
    pub fn order(&self, x: Num<'_>) -> Option<Ordering> {
        match x.rank().max(self.rank) {
            0 => match (x, &self.c) {
                (Num::Integer(x), NumOwned::Integer(y)) => x.partial_cmp(y),
                _ => num_cmp_big(x, self.c.as_num()),
            },
            1 => match (x.to_decimal(), self.decimal) {
                (Some(a), Some(b)) => a.partial_cmp(&b),
                _ => num_cmp_big(x, self.c.as_num()),
            },
            2 => x.to_float().partial_cmp(&self.float),
            _ => x.to_double().partial_cmp(&self.double),
        }
    }
}

#[cold]
fn num_cmp_big(a: Num<'_>, b: Num<'_>) -> Option<Ordering> {
    match (a, b) {
        (Num::BigInteger(x), Num::BigInteger(y)) => Some(x.cmp(y)),
        (Num::BigInteger(x), Num::Integer(y)) => Some(x.cmp(&BigInt::from_i64(i64::from(y)))),
        (Num::Integer(x), Num::BigInteger(y)) => Some(BigInt::from_i64(i64::from(x)).cmp(y)),
        (Num::BigDecimal(x), Num::BigDecimal(y)) => Some(x.cmp(y)),
        (Num::BigDecimal(x), Num::BigInteger(y)) => Some(x.cmp_bigint(y)),
        (Num::BigInteger(x), Num::BigDecimal(y)) => Some(y.cmp_bigint(x).reverse()),
        (Num::BigDecimal(x), Num::Integer(y)) => {
            Some(x.cmp_bigint(&BigInt::from_i64(i64::from(y))))
        }
        (Num::Integer(x), Num::BigDecimal(y)) => {
            Some(y.cmp_bigint(&BigInt::from_i64(i64::from(x))).reverse())
        }
        _ => Some(a.to_big_dec()?.cmp(&b.to_big_dec()?)),
    }
}

fn num_cmp(a: Num<'_>, b: Num<'_>) -> Option<Ordering> {
    match a.rank().max(b.rank()) {
        0 => match (a, b) {
            (Num::Integer(x), Num::Integer(y)) => x.partial_cmp(&y),
            _ => num_cmp_big(a, b),
        },
        1 => match (a.to_decimal(), b.to_decimal()) {
            (Some(x), Some(y)) => x.partial_cmp(&y),
            _ => num_cmp_big(a, b),
        },
        2 => a.to_float().partial_cmp(&b.to_float()),
        _ => a.to_double().partial_cmp(&b.to_double()),
    }
}

// --------------------------------------------------------------- comparison ----

/// Value comparison for `<`, `>`, `<=`, `>=`. `Ok(None)` = incomparable (e.g. NaN).
pub fn compare(a: &Value, b: &Value) -> EvalResult<Option<Ordering>> {
    use Value::*;
    Ok(match (a, b) {
        // ARQ's list-less-than and map-less-than (cdt:List, cdt:Map)
        (Other { lex: l1, dt: d1 }, Other { lex: l2, dt: d2 })
            if d1 == d2 && super::cdt::is_cdt(d1) =>
        {
            Some(super::cdt::compare_cdt(l1, l2, d1, false)?)
        }
        _ if a.is_numeric() && b.is_numeric() => num_cmp(Num::of(a)?, Num::of(b)?),
        (Str(x), Str(y)) => Some(x.cmp(y)),
        (Lang(x, lx), Lang(y, ly)) if lx.eq_ignore_ascii_case(ly) => Some(x.cmp(y)),
        (LangDir(x, lx, dx), LangDir(y, ly, dy)) if lx.eq_ignore_ascii_case(ly) && dx == dy => {
            Some(x.cmp(y))
        }
        (Bool(x), Bool(y)) => Some(x.cmp(y)),
        (DateTime(x), DateTime(y)) => x.partial_cmp(y),
        (Date(x), Date(y)) => x.partial_cmp(y),
        (Time(x), Time(y)) => x.partial_cmp(y),
        (Duration(x), Duration(y)) => x.partial_cmp(y),
        (YearMonth(x), YearMonth(y)) => x.partial_cmp(y),
        (DayTime(x), DayTime(y)) => x.partial_cmp(y),
        (YearMonth(x), DayTime(y)) => {
            oxsdatatypes::Duration::from(*x).partial_cmp(&oxsdatatypes::Duration::from(*y))
        }
        (DayTime(x), YearMonth(y)) => {
            oxsdatatypes::Duration::from(*x).partial_cmp(&oxsdatatypes::Duration::from(*y))
        }
        (Duration(x), YearMonth(y)) => x.partial_cmp(&oxsdatatypes::Duration::from(*y)),
        (Duration(x), DayTime(y)) => x.partial_cmp(&oxsdatatypes::Duration::from(*y)),
        (YearMonth(x), Duration(y)) => oxsdatatypes::Duration::from(*x).partial_cmp(y),
        (DayTime(x), Duration(y)) => oxsdatatypes::Duration::from(*x).partial_cmp(y),
        _ => return Err(TypeError),
    })
}

/// `=` (RDFterm-equal extended with value equality). `a`/`b` also carry whether they
/// are identical terms (checked by the caller via ids).
pub fn equals(a: &Value, b: &Value) -> EvalResult<bool> {
    use Value::*;
    Ok(match (a, b) {
        (Iri(x), Iri(y)) => x == y,
        (BNode(x), BNode(y)) => x == y,
        (Iri(_) | BNode(_), _) | (_, Iri(_) | BNode(_)) => false,
        _ if a.is_numeric() && b.is_numeric() => {
            num_cmp(Num::of(a)?, Num::of(b)?) == Some(Ordering::Equal)
        }
        (Str(x), Str(y)) => x == y,
        (Lang(x, lx), Lang(y, ly)) => x == y && lx.eq_ignore_ascii_case(ly),
        (LangDir(x, lx, dx), LangDir(y, ly, dy)) => {
            x == y && lx.eq_ignore_ascii_case(ly) && dx == dy
        }
        (Triple(x), Triple(y)) => triple_equals(x, y)?,
        (Triple(_), _) | (_, Triple(_)) => false,
        (LangDir(..), _) | (_, LangDir(..)) => false,
        (Bool(x), Bool(y)) => x == y,
        (Str(_), Lang(..)) | (Lang(..), Str(_)) => false,
        (Lang(..), _) | (_, Lang(..)) => false,
        (Other { lex: l1, dt: d1 }, Other { lex: l2, dt: d2 })
            if super::cdt::is_cdt(d1) && super::cdt::is_cdt(d2) =>
        {
            super::cdt::equals_cdt(l1, d1, l2, d2)?
        }
        (Other { lex: l1, dt: d1 }, Other { lex: l2, dt: d2 }) => {
            if l1 == l2 && d1 == d2 {
                true
            } else {
                return Err(TypeError);
            }
        }
        (Other { .. }, _) | (_, Other { .. }) => return Err(TypeError),
        _ => match compare(a, b) {
            Ok(Some(o)) => o == Ordering::Equal,
            // NaN is unequal to everything; other incomparable values (e.g. dates with and
            // without timezone) are indeterminate → error
            Ok(None) => {
                if a.is_numeric() {
                    false
                } else {
                    return Err(TypeError);
                }
            }
            Err(_) => {
                // different known literal types: not equal (per RDFterm-equal they are
                // different terms of known datatypes)
                false
            }
        },
    })
}

/// RDFterm-equal for triple terms: component-wise, literals by value (errors propagate).
fn triple_equals(x: &oxrdf::Triple, y: &oxrdf::Triple) -> EvalResult<bool> {
    if x == y {
        return Ok(true);
    }
    if x.subject != y.subject || x.predicate != y.predicate {
        return Ok(false);
    }
    equals(&Value::from_term(&x.object), &Value::from_term(&y.object))
}

/// A numeric value rounded to `f64`, or `None` for non-numeric values and NaN.
/// A fixed decimal's promotion can differ slightly from correct rounding.
pub fn approx_f64(v: &Value) -> Option<f64> {
    if let Value::Decimal(d) = v {
        return Some(decimal_f64(*d));
    }
    let d = f64::from(Num::of(v).ok()?.to_double());
    (!d.is_nan()).then_some(d)
}

/// `xsd:decimal` to `xsd:double` with the result of oxsdatatypes' conversion, which
/// strips the trailing zeros of the 18 fraction digits with one 128-bit division per
/// digit and then divides by the remaining power of ten. Here one 128-bit division
/// splits off the fraction, its trailing zeros are counted in 64 bits, and the same
/// quotient and power of ten give the same double.
pub fn decimal_f64(d: Decimal) -> f64 {
    const POW: u64 = 1_000_000_000_000_000_000;
    let v = i128::from_be_bytes(d.to_be_bytes());
    let a = v.unsigned_abs();
    let (int, mut frac) = ((a / u128::from(POW)), (a % u128::from(POW)) as u64);
    let f = if frac == 0 {
        // every fraction digit is zero: the shift goes down to 1
        int as f64
    } else {
        let mut shift = POW;
        while frac.is_multiple_of(10) {
            frac /= 10;
            shift /= 10;
        }
        // the value without its trailing zeros, as oxsdatatypes divides it down
        let m = int * u128::from(shift) + u128::from(frac);
        m as f64 / shift as f64
    };
    // IEEE conversion and division round symmetrically, so the sign comes last
    if v < 0 { -f } else { f }
}

/// Compare two exact or floating-point numbers by their exact mathematical value for
/// `ORDER BY` (transitive across all numeric datatypes; returns `None` if either is `NaN`).
///
/// Unlike [`num_cmp`] (which implements XPath numeric promotion for `FILTER` `<` and `=`),
/// this never rounds an exact integer or decimal to `f64` when comparing against a close
/// float or double, keeping `ORDER BY` strictly transitive.
fn order_num_cmp(a: Num<'_>, b: Num<'_>) -> Option<Ordering> {
    let to_f64_opt = |n: Num<'_>| match n {
        Num::Float(f) => Some(f64::from(f32::from(f))),
        Num::Double(d) => Some(f64::from(d)),
        _ => None,
    };
    match (to_f64_opt(a), to_f64_opt(b)) {
        (None, None) => num_cmp(a, b),
        (Some(x), Some(y)) => {
            if x.is_nan() || y.is_nan() {
                None
            } else if x < y {
                Some(Ordering::Less)
            } else if x > y {
                Some(Ordering::Greater)
            } else {
                Some(Ordering::Equal)
            }
        }
        (None, Some(f)) => exact_vs_f64(a, f),
        (Some(f), None) => exact_vs_f64(b, f).map(Ordering::reverse),
    }
}

fn exact_vs_f64(exact: Num<'_>, f: f64) -> Option<Ordering> {
    if f.is_nan() {
        return None;
    }
    if f.is_infinite() {
        return Some(if f.is_sign_positive() {
            Ordering::Less
        } else {
            Ordering::Greater
        });
    }
    let approx = match exact {
        Num::Integer(i) => i64::from(i) as f64,
        Num::Decimal(d) => decimal_f64(d),
        Num::BigInteger(i) => i.to_f64(),
        Num::BigDecimal(d) => d.to_f64(),
        _ => unreachable!(),
    };
    if f == 0.0 {
        return Some(match exact {
            Num::Integer(i) => i64::from(i).cmp(&0),
            Num::Decimal(d) => i128::from_be_bytes(d.to_be_bytes()).cmp(&0),
            Num::BigInteger(i) => i.cmp(&BigInt::zero()),
            Num::BigDecimal(d) => d.cmp(&BigDec::zero()),
            _ => unreachable!(),
        });
    }
    if approx.is_infinite() {
        return Some(if approx.is_sign_positive() {
            Ordering::Greater
        } else {
            Ordering::Less
        });
    }
    if (approx - f).abs() > 1e-12 * approx.abs().max(f.abs()) {
        return Some(if approx < f {
            Ordering::Less
        } else {
            Ordering::Greater
        });
    }
    // Close values: compare exact mathematical values without promotion rounding.
    let f_exact = BigDec::from_exact_f64(f)?;
    let exact_dec = exact.to_big_dec()?;
    Some(exact_dec.cmp(&f_exact))
}

/// Total order for ORDER BY (SPARQL 15.1 + ARQ's fallback ordering).
/// Unbound precedes blank nodes, IRIs and literals. Comparable literals sort by
/// value. Other literals sort by kind, lexical form and datatype or language.
pub fn order_cmp(a: Option<&Value>, b: Option<&Value>) -> Ordering {
    let (a, b) = match (a, b) {
        (None, None) => return Ordering::Equal,
        (None, _) => return Ordering::Less,
        (_, None) => return Ordering::Greater,
        (Some(a), Some(b)) => (a, b),
    };
    fn kind(v: &Value) -> u8 {
        match v {
            Value::BNode(_) => 0,
            Value::Iri(_) => 1,
            Value::Triple(_) => 3,
            _ => 2,
        }
    }
    let (ka, kb) = (kind(a), kind(b));
    if ka != kb {
        return ka.cmp(&kb);
    }
    match (a, b) {
        (Value::BNode(x), Value::BNode(y)) | (Value::Iri(x), Value::Iri(y)) => return x.cmp(y),
        (Value::Triple(x), Value::Triple(y)) => {
            // component-wise (subject, predicate, object) in term order
            let c = |t: &oxrdf::Triple| {
                [
                    Value::from_term(&t.subject.clone().into()),
                    Value::from_term(&t.predicate.clone().into()),
                    Value::from_term(&t.object),
                ]
            };
            let (cx, cy) = (c(x), c(y));
            for (p, q) in cx.iter().zip(cy.iter()) {
                let o = order_cmp(Some(p), Some(q));
                if o != Ordering::Equal {
                    return o;
                }
            }
            return Ordering::Equal;
        }
        // ARQ orders composite literals after the other value spaces it knows
        (Value::Other { lex: l1, dt: d1 }, Value::Other { lex: l2, dt: d2 })
            if d1 == d2 && super::cdt::is_cdt(d1) =>
        {
            if let Ok(o) = super::cdt::compare_cdt(l1, l2, d1, true) {
                return o;
            }
        }
        _ => {}
    }
    if a.is_numeric() && b.is_numeric() {
        if let (Ok(na), Ok(nb)) = (Num::of(a), Num::of(b))
            && let Some(o) = order_num_cmp(na, nb)
            && o != Ordering::Equal
        {
            return o;
        }
    } else if let Ok(Some(o)) = compare(a, b)
        && o != Ordering::Equal
    {
        return o;
    }
    // fallback: group by value-space kind, then lexical form, then datatype / lang
    fn lit_rank(v: &Value) -> u8 {
        match v {
            Value::Str(_) => 0,
            Value::Lang(..) => 1,
            Value::Integer(_)
            | Value::BigInteger(_)
            | Value::Decimal(_)
            | Value::BigDecimal(_)
            | Value::Float(_)
            | Value::Double(_) => 2,
            Value::Bool(_) => 3,
            Value::DateTime(_) => 4,
            Value::Date(_) => 5,
            Value::Time(_) => 6,
            Value::Duration(_) | Value::YearMonth(_) | Value::DayTime(_) => 7,
            Value::Other { dt, .. } if &**dt == super::cdt::LIST => 8,
            Value::Other { dt, .. } if &**dt == super::cdt::MAP => 9,
            _ => 10,
        }
    }
    let (ra, rb) = (lit_rank(a), lit_rank(b));
    if ra != rb {
        return ra.cmp(&rb);
    }
    let la = a.lexical().unwrap_or_default();
    let lb = b.lexical().unwrap_or_default();
    la.cmp(&lb).then_with(|| {
        let da = match a {
            Value::Lang(_, l) => l.to_string(),
            _ => a.datatype().map(|d| d.into_string()).unwrap_or_default(),
        };
        let db = match b {
            Value::Lang(_, l) => l.to_string(),
            _ => b.datatype().map(|d| d.into_string()).unwrap_or_default(),
        };
        da.cmp(&db)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lit(lex: &str, dt: NamedNodeRef<'_>) -> Value {
        Value::from_literal(&Literal::new_typed_literal(lex, dt))
    }
    use oxrdf::NamedNodeRef;

    #[test]
    fn decimal_f64_matches_the_decimal_conversion() {
        use rand::{RngExt, SeedableRng};
        let mut rng = rand::rngs::StdRng::seed_from_u64(7);
        let same = |d: Decimal| {
            let want = f64::from(Double::from(d));
            assert_eq!(decimal_f64(d).to_bits(), want.to_bits(), "{d}");
        };
        for s in [
            "0",
            "-0.0",
            "1",
            "-1",
            "0.1",
            "-175000.5",
            "1e-18",
            "9.5",
            "100",
        ] {
            let s = s.replace("1e-18", "0.000000000000000001");
            same(s.parse().unwrap());
        }
        same(Decimal::MAX);
        same(Decimal::MIN);
        for _ in 0..100_000 {
            let bits: i128 = match rng.random_range(0..4) {
                0 => rng.random_range(-1_000_000i128..1_000_000),
                1 => i128::from(rng.random::<i64>()),
                2 => rng.random::<i64>() as i128 * 10i128.pow(rng.random_range(0..18)),
                _ => rng.random::<i128>() >> rng.random_range(0..100),
            };
            same(Decimal::from_be_bytes(bits.to_be_bytes()));
        }
    }

    /// The one-operation decimal products and quotients give oxsdatatypes' results,
    /// errors and overflows included.
    #[test]
    fn exact_decimal_arithmetic_matches_oxsdatatypes() {
        use rand::{RngExt, SeedableRng};
        let mut rng = rand::rngs::StdRng::seed_from_u64(11);
        let reference = |op: &NumOp, a: Num, b: Num| -> Option<Decimal> {
            let (x, y) = (a.to_decimal()?, b.to_decimal()?);
            match op {
                NumOp::Mul => x.checked_mul(y),
                NumOp::Div if y == Decimal::from(0) => None,
                NumOp::Div => x.checked_div(y),
                _ => unreachable!(),
            }
        };
        let decimal = |v: i128| Decimal::from_be_bytes(v.to_be_bytes());
        let integers = |rng: &mut rand::rngs::StdRng| -> i64 {
            match rng.random_range(0..5) {
                0 => rng.random_range(-20i64..20),
                1 => rng.random_range(-3000i64..3000),
                2 => rng.random::<i64>(),
                3 => rng.random::<i64>() >> rng.random_range(0..63),
                _ => [0, 1, -1, 10, i64::MAX, i64::MIN, 1_000_000_000_000_000_000]
                    [rng.random_range(0..7)],
            }
        };
        let decimals = |rng: &mut rand::rngs::StdRng| -> i128 {
            match rng.random_range(0..5) {
                0 => rng.random_range(-1_000_000i128..1_000_000),
                1 => i128::from(rng.random::<i32>())
                    .saturating_mul(10i128.pow(rng.random_range(0..30))),
                2 => rng.random::<i128>() >> rng.random_range(0..127),
                3 => rng.random::<i128>(),
                _ => [
                    0,
                    1,
                    -1,
                    i128::MAX,
                    i128::MIN,
                    DECIMAL_SCALE,
                    -DECIMAL_SCALE,
                ][rng.random_range(0..7)],
            }
        };
        let mut checked = 0;
        for _ in 0..200_000 {
            let i = Num::Integer(integers(&mut rng).into());
            let j = Num::Integer(integers(&mut rng).into());
            let d = Num::Decimal(decimal(decimals(&mut rng)));
            for (op, a, b) in [
                (NumOp::Mul, d, j),
                (NumOp::Mul, j, d),
                (NumOp::Div, i, j),
                (NumOp::Div, d, j),
                (NumOp::Mul, d, Num::Decimal(decimal(decimals(&mut rng)))),
            ] {
                if let Some(fast) = exact_decimal(&op, a, b) {
                    checked += 1;
                    assert_eq!(fast, reference(&op, a, b), "{a:?} {b:?}");
                }
                let want = reference(&op, a, b);
                let is_div = matches!(op, NumOp::Div);
                let div_by_zero = is_div && b.to_decimal() == Some(Decimal::from(0));
                match (want, arith(op, &a.into_value(), &b.into_value())) {
                    (Some(d), Ok(Value::Decimal(got))) => assert_eq!(got, d, "{a:?} {b:?}"),
                    // Exact terminating quotients needing >18 fractional digits now preserve
                    // full precision in BigDecimal instead of truncating to 18 digits.
                    (Some(_), Ok(Value::BigDecimal(_))) if is_div && !div_by_zero => {}
                    (None, Err(_)) if div_by_zero => {}
                    // Overflows, >18-fractional-digit results, and intermediate i128
                    // overflows (such as Decimal::MIN / -10) fall back to BigDec (item 1065).
                    (None, Ok(Value::Decimal(_) | Value::BigDecimal(_))) if !div_by_zero => {}
                    (want, got) => panic!("{a:?} {b:?}: want {want:?}, got {got:?}"),
                }
            }
        }
        assert!(checked > 500_000, "the fast paths are exercised: {checked}");
        // Zero products are exact even for the smallest fractional decimal.
        let tiny = Num::Decimal(decimal(1));
        assert_eq!(
            exact_decimal(&NumOp::Mul, tiny, Num::Integer(0.into())),
            Some(Some(Decimal::from(0)))
        );
        // Fractional quotients beyond 18 digits fall back to BigDecimal.
        assert!(exact_decimal(&NumOp::Div, tiny, Num::Integer(10.into())).is_none());
        let q = arith(NumOp::Div, &tiny.into_value(), &Value::Integer(10.into())).unwrap();
        assert_eq!(q.lexical().unwrap().as_ref(), "0.0000000000000000001");
        assert!(
            arith(
                NumOp::Div,
                &Value::Integer(7.into()),
                &Value::Integer(0.into())
            )
            .is_err()
        );
    }

    #[test]
    fn decimal_zero_product_is_zero() {
        for sign in [-1i128, 1] {
            let tiny = Value::Decimal(Decimal::from_be_bytes(sign.to_be_bytes()));
            for zero in [Value::Integer(0.into()), Value::Decimal(Decimal::from(0))] {
                for (a, b) in [(&tiny, &zero), (&zero, &tiny)] {
                    assert!(
                        matches!(arith(NumOp::Mul, a, b), Ok(Value::Decimal(d)) if d == Decimal::from(0))
                    );
                }
            }
        }
    }

    #[test]
    fn promotion() {
        let a = lit("1", xsd::INTEGER);
        let b = lit("2.5", xsd::DECIMAL);
        let c = lit("1.0E0", xsd::DOUBLE);
        assert!(matches!(
            arith(NumOp::Add, &a, &b).unwrap(),
            Value::Decimal(_)
        ));
        assert!(matches!(
            arith(NumOp::Add, &a, &c).unwrap(),
            Value::Double(_)
        ));
        assert!(matches!(
            arith(NumOp::Div, &a, &a).unwrap(),
            Value::Decimal(_)
        ));
        assert!(equals(&a, &c).unwrap());
        assert!(equals(&lit("01", xsd::INTEGER), &a).unwrap());
        assert_eq!(compare(&a, &b).unwrap(), Some(Ordering::Less));
    }

    #[test]
    fn ebv() {
        assert!(!lit("0", xsd::INTEGER).ebv().unwrap());
        assert!(lit("abc", xsd::STRING).ebv().unwrap());
        // SPARQL 1.2: ill-typed numeric literals have no EBV
        assert!(lit("abc", xsd::INTEGER).ebv().is_err());
        assert!(Value::Iri("http://x".into()).ebv().is_err());
    }

    #[test]
    fn ordering() {
        let vals = [
            Value::Str("b".into()),
            Value::Iri("http://a".into()),
            Value::BNode("x".into()),
            lit("3", xsd::INTEGER),
            Value::Str("a".into()),
        ];
        let mut v: Vec<_> = vals.iter().collect();
        v.sort_by(|a, b| order_cmp(Some(a), Some(b)));
        assert!(matches!(v[0], Value::BNode(_)));
        assert!(matches!(v[1], Value::Iri(_)));
    }
    #[test]
    fn string_arg_preserves_public_return_type_and_dir_helper() {
        let plain = Value::Str("Ada".into());
        let lang = Value::Lang("Ada".into(), "en".into());
        let ltr = Value::LangDir("Ada".into(), "en".into(), oxrdf::BaseDirection::Ltr);
        let rtl = Value::LangDir("Ada".into(), "ar".into(), oxrdf::BaseDirection::Rtl);
        let iri = Value::Iri("urn:x".into());

        let pub_plain: EvalResult<(&str, Option<&str>)> = plain.string_arg();
        let pub_lang: EvalResult<(&str, Option<&str>)> = lang.string_arg();
        let pub_ltr: EvalResult<(&str, Option<&str>)> = ltr.string_arg();
        assert_eq!(pub_plain, Ok(("Ada", None)));
        assert_eq!(pub_lang, Ok(("Ada", Some("en"))));
        assert_eq!(pub_ltr, Ok(("Ada", Some("en"))));
        assert_eq!(iri.string_arg(), Err(TypeError));

        assert_eq!(plain.string_arg_dir(), Ok(("Ada", None)));
        assert_eq!(lang.string_arg_dir(), Ok(("Ada", Some(("en", None)))));
        assert_eq!(
            ltr.string_arg_dir(),
            Ok(("Ada", Some(("en", Some(oxrdf::BaseDirection::Ltr)))))
        );
        assert_eq!(
            rtl.string_arg_dir(),
            Ok(("Ada", Some(("ar", Some(oxrdf::BaseDirection::Rtl)))))
        );
        assert_eq!(iri.string_arg_dir(), Err(TypeError));
    }

    #[test]
    fn order_cmp_is_transitive_across_big_exact_and_double() {
        let a = lit("9999999999999999999999", xsd::INTEGER);
        let b = lit("10000000000000000000000.0", xsd::DECIMAL);
        let c = lit("1e22", xsd::DOUBLE);
        assert_eq!(order_cmp(Some(&a), Some(&b)), Ordering::Less);
        assert_eq!(order_cmp(Some(&b), Some(&c)), Ordering::Less);
        assert_eq!(order_cmp(Some(&a), Some(&c)), Ordering::Less);
        // FILTER promotion semantics stay separate from ORDER BY exact ordering.
        assert!(equals(&a, &c).unwrap());
        assert_eq!(compare(&a, &c).unwrap(), Some(Ordering::Equal));
    }
}

#[cfg(test)]
mod projection_numeric_tests {
    use super::*;
    #[test]
    fn borrowed_numeric_subset_matches_existing_key_decode_bits() {
        let datatypes = [
            xsd::INTEGER.as_str(),
            xsd::DECIMAL.as_str(),
            xsd::FLOAT.as_str(),
            xsd::DOUBLE.as_str(),
            xsd::STRING.as_str(),
        ];
        for dt in datatypes.into_iter().chain(INTEGER_DERIVED.iter().copied()) {
            for lex in [
                "0",
                "-0",
                "01",
                "-3",
                "1.50",
                "+1",
                "1e20",
                "INF",
                "+INF",
                "-INF",
                "NaN",
                "inf",
                "-inf",
                "infinity",
                "nan",
                "-NaN",
                "heavy",
                "99999999999999999999999999999",
                "123456789012345678.00000000000000000000",
            ] {
                let key = crate::id::term_key(&Term::Literal(Literal::new_typed_literal(
                    lex,
                    NamedNode::new_unchecked(dt),
                )));
                assert_eq!(
                    numeric_key(&key).map(f64::to_bits),
                    approx_f64(&Value::from_key(&key)).map(f64::to_bits),
                    "{lex} {dt}"
                );
            }
        }
        for key in [
            b"\"\xff\x1f^http://www.w3.org/2001/XMLSchema#double".as_slice(),
            b"\"1\x1f@en",
            b"<iri",
        ] {
            assert_eq!(numeric_key(key), None);
        }
    }
}
