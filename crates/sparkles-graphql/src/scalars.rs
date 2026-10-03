//! The scalars of the adapter (§4.3): which RDF values each one reads, how a value is
//! written in a response, and how an argument becomes an RDF term.

use oxrdf::{Literal, NamedNode, Term};
use serde_json::{Value as J, json};
use std::cmp::Ordering;

pub const XSD: &str = "http://www.w3.org/2001/XMLSchema#";
pub const RDF: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#";
pub const RDFS: &str = "http://www.w3.org/2000/01/rdf-schema#";
pub const RDF_TYPE: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#type";
pub const RDFS_SUBCLASS_OF: &str = "http://www.w3.org/2000/01/rdf-schema#subClassOf";

/// A scalar a value field may have, or one of the two object types that carry a whole
/// literal or term.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Scalar {
    String,
    Boolean,
    Int,
    Integer,
    Decimal,
    Float,
    DateTime,
    Date,
    Time,
    Duration,
    Iri,
    Id,
    LangString,
    RdfTerm,
}

/// The scalars the server declares when the SDL does not, with the definition each one
/// names in `@specifiedBy`.
pub const CUSTOM: [(&str, &str); 7] = [
    ("Integer", "https://www.w3.org/TR/xmlschema11-2/#integer"),
    ("Decimal", "https://www.w3.org/TR/xmlschema11-2/#decimal"),
    ("DateTime", "https://www.w3.org/TR/xmlschema11-2/#dateTime"),
    ("Date", "https://www.w3.org/TR/xmlschema11-2/#date"),
    ("Time", "https://www.w3.org/TR/xmlschema11-2/#time"),
    ("Duration", "https://www.w3.org/TR/xmlschema11-2/#duration"),
    ("IRI", "https://www.rfc-editor.org/rfc/rfc3987"),
];

impl Scalar {
    pub fn from_name(n: &str) -> Option<Scalar> {
        Some(match n {
            "String" => Scalar::String,
            "Boolean" => Scalar::Boolean,
            "Int" => Scalar::Int,
            "Integer" => Scalar::Integer,
            "Decimal" => Scalar::Decimal,
            "Float" => Scalar::Float,
            "DateTime" => Scalar::DateTime,
            "Date" => Scalar::Date,
            "Time" => Scalar::Time,
            "Duration" => Scalar::Duration,
            "IRI" => Scalar::Iri,
            "ID" => Scalar::Id,
            "LangString" => Scalar::LangString,
            "RDFTerm" => Scalar::RdfTerm,
            _ => return None,
        })
    }

    pub fn name(self) -> &'static str {
        match self {
            Scalar::String => "String",
            Scalar::Boolean => "Boolean",
            Scalar::Int => "Int",
            Scalar::Integer => "Integer",
            Scalar::Decimal => "Decimal",
            Scalar::Float => "Float",
            Scalar::DateTime => "DateTime",
            Scalar::Date => "Date",
            Scalar::Time => "Time",
            Scalar::Duration => "Duration",
            Scalar::Iri => "IRI",
            Scalar::Id => "ID",
            Scalar::LangString => "LangString",
            Scalar::RdfTerm => "RDFTerm",
        }
    }

    /// Whether the field returns an object (`LangString`, `RDFTerm`).
    pub fn is_object(self) -> bool {
        matches!(self, Scalar::LangString | Scalar::RdfTerm)
    }

    /// Whether string operators and a `lang` argument apply.
    pub fn is_text(self) -> bool {
        matches!(self, Scalar::String | Scalar::LangString)
    }

    /// The input type of filters on fields of this scalar (`None`: not filterable).
    pub fn filter_type(self) -> Option<&'static str> {
        Some(match self {
            Scalar::String | Scalar::LangString => "StringFilter",
            Scalar::Boolean => "BooleanFilter",
            Scalar::Int => "IntFilter",
            Scalar::Integer => "IntegerFilter",
            Scalar::Decimal => "DecimalFilter",
            Scalar::Float => "FloatFilter",
            Scalar::DateTime => "DateTimeFilter",
            Scalar::Date => "DateFilter",
            Scalar::Time => "TimeFilter",
            Scalar::Duration => "DurationFilter",
            Scalar::Iri => "IRIFilter",
            Scalar::Id => "IDFilter",
            Scalar::RdfTerm => return None,
        })
    }

    /// Whether `orderBy` may name a single-valued field of this scalar.
    pub fn orderable(self) -> bool {
        !matches!(self, Scalar::RdfTerm)
    }
}

fn xsd(local: &str) -> NamedNode {
    NamedNode::new_unchecked(format!("{XSD}{local}"))
}

const INTEGER_TYPES: [&str; 13] = [
    "integer",
    "long",
    "int",
    "short",
    "byte",
    "nonNegativeInteger",
    "positiveInteger",
    "nonPositiveInteger",
    "negativeInteger",
    "unsignedLong",
    "unsignedInt",
    "unsignedShort",
    "unsignedByte",
];

fn xsd_local(l: &Literal) -> Option<&str> {
    l.datatype().as_str().strip_prefix(XSD)
}

fn is_integer_type(l: &Literal) -> bool {
    xsd_local(l).is_some_and(|d| INTEGER_TYPES.contains(&d))
}

/// The canonical lexical form of an integer: no `+`, no leading zeros, `0` without a
/// sign. `None` when the form is not an integer.
pub fn canonical_integer(lex: &str) -> Option<String> {
    let t = lex.trim();
    let (neg, digits) = match t.as_bytes().first()? {
        b'-' => (true, &t[1..]),
        b'+' => (false, &t[1..]),
        _ => (false, t),
    };
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let d = digits.trim_start_matches('0');
    Some(match (neg, d.is_empty()) {
        (_, true) => "0".into(),
        (true, false) => format!("-{d}"),
        (false, false) => d.to_string(),
    })
}

/// The canonical lexical form of a decimal (`1.50` gives `1.5`, `2` gives `2.0`).
pub fn canonical_decimal(lex: &str) -> Option<String> {
    let t = lex.trim();
    let (neg, rest) = match t.as_bytes().first()? {
        b'-' => (true, &t[1..]),
        b'+' => (false, &t[1..]),
        _ => (false, t),
    };
    let (int, frac) = rest.split_once('.').unwrap_or((rest, ""));
    if (int.is_empty() && frac.is_empty())
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

/// Why a value does not fit its field.
pub struct Unfit(pub String);

fn unfit(t: &Term, scalar: &str) -> Unfit {
    Unfit(format!("{t} is not a value of type {scalar}"))
}

/// Base direction of a literal, when it has one.
fn direction(l: &Literal) -> Option<&'static str> {
    l.direction().map(|d| match d {
        oxrdf::BaseDirection::Ltr => "ltr",
        oxrdf::BaseDirection::Rtl => "rtl",
    })
}

/// The `id` of a node: an IRI, or the label of a blank node as `_:label`.
pub fn id_of(t: &Term) -> Option<String> {
    match t {
        Term::NamedNode(n) => Some(n.as_str().to_string()),
        Term::BlankNode(b) => Some(format!("_:{}", b.as_str())),
        _ => None,
    }
}

/// The `RDFTerm` object of any term.
pub fn rdf_term(t: &Term) -> J {
    match t {
        Term::NamedNode(n) => json!({ "kind": "IRI", "value": n.as_str() }),
        Term::BlankNode(b) => json!({ "kind": "BLANK_NODE", "value": format!("_:{}", b.as_str()) }),
        Term::Literal(l) => json!({
            "kind": "LITERAL",
            "value": l.value(),
            "datatype": l.datatype().as_str(),
            "language": l.language(),
            "direction": direction(l),
        }),
        #[allow(unreachable_patterns)]
        t => json!({ "kind": "TRIPLE", "value": t.to_string() }),
    }
}

/// A value of a field of `scalar` as JSON.
pub fn to_json(t: &Term, scalar: Scalar) -> Result<J, Unfit> {
    if scalar == Scalar::RdfTerm {
        return Ok(rdf_term(t));
    }
    if scalar == Scalar::Iri {
        return match t {
            Term::NamedNode(n) => Ok(n.as_str().into()),
            Term::Literal(l) if l.datatype().as_str() == format!("{XSD}anyURI") => {
                Ok(l.value().into())
            }
            _ => Err(unfit(t, "IRI")),
        };
    }
    if scalar == Scalar::Id {
        return id_of(t).map(J::String).ok_or_else(|| unfit(t, "ID"));
    }
    let Term::Literal(l) = t else {
        return Err(unfit(t, scalar.name()));
    };
    if !sparkles_core::xsd::is_valid(l) {
        return Err(Unfit(format!("{t} is ill-formed for its datatype")));
    }
    let local = xsd_local(l);
    match scalar {
        Scalar::String => Ok(l.value().into()),
        Scalar::LangString => Ok(json!({
            "value": l.value(),
            "language": l.language(),
            "direction": direction(l),
        })),
        Scalar::Boolean => match (local, l.value().trim()) {
            (Some("boolean"), "true" | "1") => Ok(true.into()),
            (Some("boolean"), "false" | "0") => Ok(false.into()),
            _ => Err(unfit(t, "Boolean")),
        },
        Scalar::Int => {
            if !is_integer_type(l) {
                return Err(unfit(t, "Int"));
            }
            match canonical_integer(l.value()).and_then(|c| c.parse::<i64>().ok()) {
                Some(v) if i32::try_from(v).is_ok() => Ok(v.into()),
                _ => Err(Unfit(format!("{t} is outside the 32-bit range of Int"))),
            }
        }
        Scalar::Integer => {
            if !is_integer_type(l) {
                return Err(unfit(t, "Integer"));
            }
            canonical_integer(l.value())
                .map(J::String)
                .ok_or_else(|| unfit(t, "Integer"))
        }
        Scalar::Decimal => {
            if !(local == Some("decimal") || is_integer_type(l)) {
                return Err(unfit(t, "Decimal"));
            }
            canonical_decimal(l.value())
                .map(J::String)
                .ok_or_else(|| unfit(t, "Decimal"))
        }
        Scalar::Float => {
            if !(matches!(local, Some("double" | "float" | "decimal")) || is_integer_type(l)) {
                return Err(unfit(t, "Float"));
            }
            let v: f64 = match l.value().trim() {
                "INF" | "+INF" | "-INF" | "NaN" => {
                    return Err(Unfit(format!("{t} is not a finite number")));
                }
                s => s.parse().map_err(|_| unfit(t, "Float"))?,
            };
            serde_json::Number::from_f64(v)
                .map(J::Number)
                .ok_or_else(|| Unfit(format!("{t} is not a finite number")))
        }
        Scalar::DateTime if matches!(local, Some("dateTime" | "dateTimeStamp")) => {
            Ok(l.value().trim().into())
        }
        Scalar::Date if local == Some("date") => Ok(l.value().trim().into()),
        Scalar::Time if local == Some("time") => Ok(l.value().trim().into()),
        Scalar::Duration
            if matches!(
                local,
                Some("duration" | "dayTimeDuration" | "yearMonthDuration")
            ) =>
        {
            Ok(l.value().trim().into())
        }
        _ => Err(unfit(t, scalar.name())),
    }
}

/// The RDF term of an argument value given for a field of `scalar` (filters): a literal
/// of the scalar's datatype, or an IRI. `None` when the value does not fit.
pub fn input_term(v: &J, scalar: Scalar, prefixes: &[(String, String)]) -> Result<Term, String> {
    let s = || {
        v.as_str()
            .map(str::to_string)
            .ok_or_else(|| format!("{v} is not a string"))
    };
    let typed = |lex: String, local: &str| -> Result<Term, String> {
        let l = Literal::new_typed_literal(lex, xsd(local));
        if sparkles_core::xsd::is_valid(&l) {
            Ok(Term::Literal(l))
        } else {
            Err(format!("'{}' is not a valid xsd:{local}", l.value()))
        }
    };
    match scalar {
        Scalar::String | Scalar::LangString => Ok(Literal::new_simple_literal(s()?).into()),
        Scalar::Boolean => v
            .as_bool()
            .map(|b| Literal::from(b).into())
            .ok_or_else(|| format!("{v} is not a boolean")),
        Scalar::Int => v
            .as_i64()
            .map(|i| Literal::new_typed_literal(i.to_string(), xsd("integer")).into())
            .ok_or_else(|| format!("{v} is not an integer")),
        Scalar::Float => v
            .as_f64()
            .map(|f| Literal::from(f).into())
            .ok_or_else(|| format!("{v} is not a number")),
        Scalar::Integer => {
            let t = match v {
                J::Number(n) if n.is_i64() || n.is_u64() => n.to_string(),
                _ => s()?,
            };
            typed(
                canonical_integer(&t).ok_or_else(|| format!("'{t}' is not an integer"))?,
                "integer",
            )
        }
        Scalar::Decimal => {
            let t = match v {
                J::Number(n) => n.to_string(),
                _ => s()?,
            };
            typed(
                canonical_decimal(&t).ok_or_else(|| format!("'{t}' is not a decimal"))?,
                "decimal",
            )
        }
        Scalar::DateTime => typed(s()?, "dateTime"),
        Scalar::Date => typed(s()?, "date"),
        Scalar::Time => typed(s()?, "time"),
        Scalar::Duration => typed(s()?, "duration"),
        Scalar::Iri | Scalar::Id => parse_id(&s()?, prefixes),
        Scalar::RdfTerm => Err("RDFTerm values cannot be compared".into()),
    }
}

/// An `ID` argument (§4.2): an absolute IRI, a prefixed name of the schema's prefixes,
/// or a stored blank node label (`_:b…`).
pub fn parse_id(s: &str, prefixes: &[(String, String)]) -> Result<Term, String> {
    let t = s.trim();
    if let Some(label) = t.strip_prefix("_:") {
        return oxrdf::BlankNode::new(label)
            .map(Term::BlankNode)
            .map_err(|_| format!("'{s}' is not a blank node label"));
    }
    let t = t
        .strip_prefix('<')
        .and_then(|x| x.strip_suffix('>'))
        .unwrap_or(t);
    if let Some((p, local)) = t.split_once(':')
        && !local.starts_with("//")
        && let Some((_, ns)) = prefixes.iter().find(|(n, _)| n == p)
    {
        return NamedNode::new(format!("{ns}{local}"))
            .map(Term::NamedNode)
            .map_err(|e| format!("'{s}' is not an IRI: {e}"));
    }
    match NamedNode::new(t) {
        Ok(n) if t.contains(':') => Ok(Term::NamedNode(n)),
        _ => Err(format!(
            "'{s}' is not an absolute IRI, a prefixed name of the schema or a blank node label"
        )),
    }
}

/// An order of terms close to SPARQL's `ORDER BY`: unbound, blank nodes, IRIs, then
/// literals; numbers by value, other literals of one datatype by lexical form, then by
/// datatype and language. Used for `@single(onMany: MIN)` and descending value lists.
pub fn term_cmp(a: &Term, b: &Term) -> Ordering {
    fn rank(t: &Term) -> u8 {
        match t {
            Term::BlankNode(_) => 0,
            Term::NamedNode(_) => 1,
            Term::Literal(_) => 2,
            #[allow(unreachable_patterns)]
            _ => 3,
        }
    }
    fn num(l: &Literal) -> Option<f64> {
        let local = xsd_local(l)?;
        if is_integer_type(l) || matches!(local, "decimal" | "double" | "float") {
            l.value().trim().parse::<f64>().ok()
        } else {
            None
        }
    }
    match (a, b) {
        (Term::NamedNode(x), Term::NamedNode(y)) => x.as_str().cmp(y.as_str()),
        (Term::BlankNode(x), Term::BlankNode(y)) => x.as_str().cmp(y.as_str()),
        (Term::Literal(x), Term::Literal(y)) => {
            if let (Some(p), Some(q)) = (num(x), num(y)) {
                return p.partial_cmp(&q).unwrap_or(Ordering::Equal);
            }
            x.value()
                .cmp(y.value())
                .then_with(|| x.datatype().as_str().cmp(y.datatype().as_str()))
                .then_with(|| x.language().cmp(&y.language()))
        }
        _ => rank(a).cmp(&rank(b)),
    }
}

/// Basic language-range matching (RFC 4647 §3.3.1, SPARQL's `langMatches`): `*` matches
/// any tag, `""` matches literals without one.
pub fn lang_matches(tag: Option<&str>, range: &str) -> bool {
    match (tag, range) {
        (_, "*") => tag.is_some(),
        (None, "") => true,
        (None, _) | (Some(_), "") => false,
        (Some(t), r) => {
            let (t, r) = (t.to_ascii_lowercase(), r.to_ascii_lowercase());
            t == r || t.starts_with(&format!("{r}-"))
        }
    }
}

/// The language tag of a value for `lang` matching: a literal's tag, `None` for other
/// literals; IRIs and blank nodes never match a range.
pub fn tag_of(t: &Term) -> Option<Option<&str>> {
    match t {
        Term::Literal(l) => Some(l.language()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_forms() {
        assert_eq!(canonical_integer("+007").as_deref(), Some("7"));
        assert_eq!(canonical_integer("-0").as_deref(), Some("0"));
        assert_eq!(
            canonical_integer("3000000000").as_deref(),
            Some("3000000000")
        );
        assert_eq!(canonical_integer("1.0"), None);
        assert_eq!(canonical_decimal("1.50").as_deref(), Some("1.5"));
        assert_eq!(canonical_decimal("2").as_deref(), Some("2.0"));
        assert_eq!(canonical_decimal("-0.0").as_deref(), Some("0.0"));
        assert_eq!(canonical_decimal(".5").as_deref(), Some("0.5"));
    }

    #[test]
    fn int_range() {
        let big = Term::Literal(Literal::new_typed_literal("3000000000", xsd("integer")));
        assert!(to_json(&big, Scalar::Int).is_err());
        assert_eq!(
            to_json(&big, Scalar::Integer).ok(),
            Some(J::String("3000000000".into()))
        );
        let small = Term::Literal(Literal::new_typed_literal("42", xsd("int")));
        assert_eq!(to_json(&small, Scalar::Int).ok(), Some(J::from(42)));
    }

    #[test]
    fn langs() {
        assert!(lang_matches(Some("en-GB"), "en"));
        assert!(!lang_matches(Some("en"), "en-GB"));
        assert!(lang_matches(None, ""));
        assert!(!lang_matches(None, "*"));
        assert!(lang_matches(Some("fr"), "*"));
    }

    #[test]
    fn ids() {
        let p = vec![("ex".to_string(), "http://example.org/".to_string())];
        assert_eq!(
            parse_id("ex:a", &p).unwrap(),
            Term::NamedNode(NamedNode::new_unchecked("http://example.org/a"))
        );
        assert!(parse_id("nope", &p).is_err());
        assert!(parse_id("_:b12", &p).is_ok());
        assert!(parse_id("http://x.org/a", &p).is_ok());
    }
}
