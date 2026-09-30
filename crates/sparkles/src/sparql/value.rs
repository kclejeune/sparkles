//! SPARQL value space (ARQ `NodeValue` equivalent): typed values, promotion,
//! comparison, effective boolean value and ordering.

use oxrdf::vocab::{rdf, xsd};
use oxrdf::{Literal, NamedNode, Term};
use oxsdatatypes::*;
use std::cmp::Ordering;
use std::sync::Arc;

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
    Decimal(Decimal),
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
            s if s == xsd::INTEGER.as_str() => parse!(Integer),
            s if s == xsd::DECIMAL.as_str() => parse!(Decimal),
            s if s == xsd::DOUBLE.as_str() => parse!(Double),
            s if s == xsd::FLOAT.as_str() => parse!(Float),
            s if s == xsd::DATE_TIME.as_str() || s == xsd::DATE_TIME_STAMP.as_str() => {
                parse!(DateTime)
            }
            s if s == xsd::DATE.as_str() => parse!(Date),
            s if s == xsd::TIME.as_str() => parse!(Time),
            s if s == xsd::DURATION.as_str() => parse!(Duration),
            s if s == xsd::YEAR_MONTH_DURATION.as_str() => parse!(YearMonth),
            s if s == xsd::DAY_TIME_DURATION.as_str() => parse!(DayTime),
            s if INTEGER_DERIVED.contains(&s) => parse!(Integer),
            _ => other(),
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
            Value::Decimal(d) => typed(d.to_string(), xsd::DECIMAL.into()),
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
            Value::Integer(_) | Value::Decimal(_) | Value::Float(_) | Value::Double(_)
        )
    }
    /// simple literal or xsd:string
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::Str(s) => Some(s),
            _ => None,
        }
    }
    /// "string literal" argument per SPARQL 17.4.3 (simple, xsd:string or lang-tagged)
    pub fn string_arg(&self) -> EvalResult<(&str, Option<&str>)> {
        match self {
            Value::Str(s) => Ok((s, None)),
            Value::Lang(s, l) | Value::LangDir(s, l, _) => Ok((s, Some(l))),
            _ => Err(TypeError),
        }
    }
    /// Lexical form (STR()).
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
            Value::Decimal(d) => Ok(*d != Decimal::from(0)),
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
pub enum Num {
    Integer(Integer),
    Decimal(Decimal),
    Float(Float),
    Double(Double),
}

impl Num {
    pub fn of(v: &Value) -> EvalResult<Num> {
        Ok(match v {
            Value::Integer(i) => Num::Integer(*i),
            Value::Decimal(d) => Num::Decimal(*d),
            Value::Float(f) => Num::Float(*f),
            Value::Double(d) => Num::Double(*d),
            _ => return Err(TypeError),
        })
    }
    fn rank(self) -> u8 {
        match self {
            Num::Integer(_) => 0,
            Num::Decimal(_) => 1,
            Num::Float(_) => 2,
            Num::Double(_) => 3,
        }
    }
    fn to_decimal(self) -> Option<Decimal> {
        match self {
            Num::Integer(i) => Some(i.into()),
            Num::Decimal(d) => Some(d),
            _ => None,
        }
    }
    fn to_float(self) -> Float {
        match self {
            Num::Integer(i) => i.into(),
            Num::Decimal(d) => d.into(),
            Num::Float(f) => f,
            Num::Double(d) => d.into(),
        }
    }
    pub fn to_double(self) -> Double {
        match self {
            Num::Integer(i) => i.into(),
            Num::Decimal(d) => d.into(),
            Num::Float(f) => f.into(),
            Num::Double(d) => d,
        }
    }
    pub fn into_value(self) -> Value {
        match self {
            Num::Integer(i) => Value::Integer(i),
            Num::Decimal(d) => Value::Decimal(d),
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
                let (Num::Integer(x), Num::Integer(y)) = (a, b) else {
                    unreachable!()
                };
                let r = match op {
                    NumOp::Add => x.checked_add(y),
                    NumOp::Sub => x.checked_sub(y),
                    NumOp::Mul => x.checked_mul(y),
                    NumOp::Div => unreachable!(),
                };
                return r.map(Value::Integer).ok_or(TypeError);
            }
            let x = a.to_decimal().ok_or(TypeError)?;
            let y = b.to_decimal().ok_or(TypeError)?;
            let r = match op {
                NumOp::Add => x.checked_add(y),
                NumOp::Sub => x.checked_sub(y),
                NumOp::Mul => x.checked_mul(y),
                NumOp::Div => {
                    if y == Decimal::from(0) {
                        return Err(TypeError);
                    }
                    x.checked_div(y)
                }
            };
            Value::Decimal(r.ok_or(TypeError)?)
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

fn num_cmp(a: Num, b: Num) -> Option<Ordering> {
    match a.rank().max(b.rank()) {
        0 => match (a, b) {
            (Num::Integer(x), Num::Integer(y)) => x.partial_cmp(&y),
            _ => None,
        },
        1 => a.to_decimal()?.partial_cmp(&b.to_decimal()?),
        2 => a.to_float().partial_cmp(&b.to_float()),
        _ => a.to_double().partial_cmp(&b.to_double()),
    }
}

// --------------------------------------------------------------- comparison ----

/// Value comparison for `<`, `>`, `<=`, `>=`. `Ok(None)` = incomparable (e.g. NaN).
pub fn compare(a: &Value, b: &Value) -> EvalResult<Option<Ordering>> {
    use Value::*;
    Ok(match (a, b) {
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

/// Total order for ORDER BY (SPARQL 15.1 + ARQ's fallback ordering):
/// unbound < blank nodes < IRIs < literals; comparable literals by value, the rest by
/// (kind, lexical form, datatype/lang).
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
        _ => {}
    }
    if let Ok(Some(o)) = compare(a, b)
        && o != Ordering::Equal
    {
        return o;
    }
    // fallback: group by value-space kind, then lexical form, then datatype / lang
    fn lit_rank(v: &Value) -> u8 {
        match v {
            Value::Str(_) => 0,
            Value::Lang(..) => 1,
            Value::Integer(_) | Value::Decimal(_) | Value::Float(_) | Value::Double(_) => 2,
            Value::Bool(_) => 3,
            Value::DateTime(_) => 4,
            Value::Date(_) => 5,
            Value::Time(_) => 6,
            Value::Duration(_) | Value::YearMonth(_) | Value::DayTime(_) => 7,
            _ => 8,
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
}
