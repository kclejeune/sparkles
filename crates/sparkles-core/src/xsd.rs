//! Lexical well-formedness of XSD literals (`sh:datatype` and ShEx datatype constraints
//! require a valid lexical form).

use oxrdf::Literal;
use oxsdatatypes::*;
use std::str::FromStr;

const XSD: &str = "http://www.w3.org/2001/XMLSchema#";

/// Whether `lex` is in the XSD 1.1 lexical space of `xsd:float` and `xsd:double`:
/// decimal and exponent numerals, `INF`, `+INF`, `-INF` and `NaN`, rejecting Rust
/// float spellings such as `inf`, `infinity`, `nan` and `-NaN`.
pub(crate) fn is_float_lexical(lex: &str) -> bool {
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

fn is_integer_lexical(s: &str) -> bool {
    let d = s.strip_prefix(['+', '-']).unwrap_or(s);
    !d.is_empty() && d.bytes().all(|b| b.is_ascii_digit())
}

fn is_decimal_lexical(s: &str) -> bool {
    let d = s.strip_prefix(['+', '-']).unwrap_or(s);
    let (int, frac) = match d.split_once('.') {
        Some((i, f)) => (i, Some(f)),
        None => (d, None),
    };
    let digits = |x: &str| x.bytes().all(|b| b.is_ascii_digit());
    match frac {
        None => !int.is_empty() && digits(int),
        Some(f) => (!int.is_empty() || !f.is_empty()) && digits(int) && digits(f),
    }
}

/// Is `(lex, [min, max])` an integer in range? Unbounded sides are `None`.
fn int_in(lex: &str, min: Option<i128>, max: Option<i128>) -> bool {
    if !is_integer_lexical(lex) {
        return false;
    }
    match lex.parse::<i128>() {
        Ok(v) => min.is_none_or(|m| v >= m) && max.is_none_or(|m| v <= m),
        // out of i128 range: only acceptable for unbounded types, in the right direction
        Err(_) => {
            let neg = lex.starts_with('-');
            if neg { min.is_none() } else { max.is_none() }
        }
    }
}

/// Is the literal's lexical form valid for its datatype? Unknown datatypes are valid.
pub fn is_valid(l: &Literal) -> bool {
    if l.language().is_some() {
        return true;
    }
    let Some(local) = l.datatype().as_str().strip_prefix(XSD) else {
        return true;
    };
    let lex = l.value();
    // whitespace-collapsing types accept surrounding whitespace
    let t = lex.trim_matches([' ', '\t', '\n', '\r']);
    match local {
        "string" | "normalizedString" | "token" | "anyURI" | "language" | "Name" | "NCName"
        | "NMTOKEN" | "hexBinary" | "base64Binary" | "QName" | "NOTATION" | "ID" | "IDREF"
        | "ENTITY" => true,
        "boolean" => matches!(t, "true" | "false" | "1" | "0"),
        "decimal" => is_decimal_lexical(t),
        "integer" => is_integer_lexical(t),
        "long" => int_in(t, Some(i64::MIN as i128), Some(i64::MAX as i128)),
        "int" => int_in(t, Some(i32::MIN as i128), Some(i32::MAX as i128)),
        "short" => int_in(t, Some(i16::MIN as i128), Some(i16::MAX as i128)),
        "byte" => int_in(t, Some(i8::MIN as i128), Some(i8::MAX as i128)),
        "nonNegativeInteger" => int_in(t, Some(0), None),
        "positiveInteger" => int_in(t, Some(1), None),
        "nonPositiveInteger" => int_in(t, None, Some(0)),
        "negativeInteger" => int_in(t, None, Some(-1)),
        "unsignedLong" => int_in(t, Some(0), Some(u64::MAX as i128)),
        "unsignedInt" => int_in(t, Some(0), Some(u32::MAX as i128)),
        "unsignedShort" => int_in(t, Some(0), Some(u16::MAX as i128)),
        "unsignedByte" => int_in(t, Some(0), Some(u8::MAX as i128)),
        "double" => is_float_lexical(t) && Double::from_str(t).is_ok(),
        "float" => is_float_lexical(t) && Float::from_str(t).is_ok(),
        "dateTime" => DateTime::from_str(t).is_ok(),
        "dateTimeStamp" => DateTime::from_str(t).is_ok_and(|d| d.timezone_offset().is_some()),
        "date" => Date::from_str(t).is_ok(),
        "time" => Time::from_str(t).is_ok(),
        "gYear" => GYear::from_str(t).is_ok(),
        "gYearMonth" => GYearMonth::from_str(t).is_ok(),
        "gMonth" => GMonth::from_str(t).is_ok(),
        "gMonthDay" => GMonthDay::from_str(t).is_ok(),
        "gDay" => GDay::from_str(t).is_ok(),
        "duration" => Duration::from_str(t).is_ok(),
        "yearMonthDuration" => YearMonthDuration::from_str(t).is_ok(),
        "dayTimeDuration" => DayTimeDuration::from_str(t).is_ok(),
        _ => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use oxrdf::NamedNode;

    fn lit(v: &str, dt: &str) -> Literal {
        Literal::new_typed_literal(v, NamedNode::new_unchecked(format!("{XSD}{dt}")))
    }

    #[test]
    fn lexical_forms() {
        assert!(is_valid(&lit("42", "integer")));
        assert!(!is_valid(&lit("4.2", "integer")));
        assert!(is_valid(&lit("-128", "byte")));
        assert!(!is_valid(&lit("128", "byte")));
        assert!(!is_valid(&lit("-1", "nonNegativeInteger")));
        assert!(is_valid(&lit(
            "99999999999999999999999999999999999999999",
            "integer"
        )));
        assert!(is_valid(&lit(".5", "decimal")));
        assert!(!is_valid(&lit("1e3", "decimal")));
        assert!(is_valid(&lit("1e3", "double")));
        assert!(is_valid(&lit("INF", "float")));
        assert!(is_valid(&lit("+INF", "double")));
        assert!(is_valid(&lit("-INF", "float")));
        assert!(is_valid(&lit("NaN", "double")));
        for dt in ["float", "double"] {
            for bad in [
                "inf", "-inf", "+inf", "infinity", "Infinity", "nan", "-NaN", "+NaN", ".", "1e",
                "e3", "1.2.3",
            ] {
                assert!(!is_valid(&lit(bad, dt)), "{bad}^^xsd:{dt}");
            }
        }
        assert!(!is_valid(&lit("yes", "boolean")));
        assert!(is_valid(&lit("2020-02-29", "date")));
        assert!(!is_valid(&lit("2021-02-29", "date")));
        assert!(!is_valid(&lit("2020-01-01T00:00:00", "dateTimeStamp")));
        assert!(is_valid(&lit("anything", "string")));
        assert!(is_valid(&Literal::new_typed_literal(
            "x",
            NamedNode::new_unchecked("http://ex.org/custom")
        )));
    }
}
