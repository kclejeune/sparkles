//! XPath functions (F&O 3.1) and Jena ARQ `afn:` functions that need more than a
//! SPARQL built-in: rounding to a precision, `numeric-mod` and `numeric-integer-divide`,
//! duration components, timezone adjustment, `fn:dateTime`, Unicode normalization and
//! ARQ's zero-based substring.

use super::value::{EvalResult, Num, TypeError, Value};
use oxsdatatypes::{
    Date, DateTime, DayTimeDuration, Decimal, Double, Float, Integer, Time, TimezoneOffset,
};
use std::str::FromStr;

/// `fn:round($x, $precision)` (half towards positive infinity) and
/// `fn:round-half-to-even($x, $precision)`, as ARQ computes them: the exact value is
/// rounded to `precision` digits after the decimal point (before it when negative), and
/// the result has the argument's type.
pub fn round(n: Num, precision: i64, half_even: bool) -> EvalResult<Value> {
    Ok(match n {
        Num::Integer(i) => {
            let s = round_digits(&i.to_string(), precision, half_even);
            Value::Integer(Integer::from_str(&s).map_err(|_| TypeError)?)
        }
        Num::Decimal(d) => {
            let s = round_digits(&d.to_string(), precision, half_even);
            Value::Decimal(Decimal::from_str(&s).map_err(|_| TypeError)?)
        }
        Num::Float(f) => {
            let x = f32::from(f);
            if !x.is_finite() {
                return Ok(Value::Float(f));
            }
            let s = round_digits(&format!("{:.160}", x), precision, half_even);
            Value::Float(Float::from(s.parse::<f32>().map_err(|_| TypeError)?))
        }
        Num::Double(d) => {
            let x = f64::from(d);
            if !x.is_finite() {
                return Ok(Value::Double(d));
            }
            let s = round_digits(&format!("{:.1100}", x), precision, half_even);
            Value::Double(Double::from(s.parse::<f64>().map_err(|_| TypeError)?))
        }
    })
}

/// Round a decimal numeral (`-?digits(.digits)?`, exact) to `precision` digits after
/// the point. Ties go to the even digit, or towards positive infinity.
fn round_digits(s: &str, precision: i64, half_even: bool) -> String {
    let (neg, body) = match s.strip_prefix('-') {
        Some(b) => (true, b),
        None => (false, s),
    };
    let (int, frac) = body.split_once('.').unwrap_or((body, ""));
    let mut digits: Vec<u8> = int.bytes().chain(frac.bytes()).map(|c| c - b'0').collect();
    let mut point = int.len() as i64;
    let keep = point + precision;
    if keep >= digits.len() as i64 {
        return s.to_string();
    }
    if keep < 0 {
        return "0".to_string();
    }
    let keep = keep as usize;
    let tail = digits.split_off(keep);
    let rest_zero = tail[1..].iter().all(|&d| d == 0);
    let up = match tail[0].cmp(&5) {
        std::cmp::Ordering::Greater => true,
        std::cmp::Ordering::Less => false,
        std::cmp::Ordering::Equal if !rest_zero => true,
        std::cmp::Ordering::Equal => {
            if half_even {
                digits.last().is_some_and(|d| d % 2 == 1)
            } else {
                !neg
            }
        }
    };
    if up {
        let mut i = digits.len();
        loop {
            if i == 0 {
                digits.insert(0, 1);
                point += 1;
                break;
            }
            i -= 1;
            if digits[i] == 9 {
                digits[i] = 0;
            } else {
                digits[i] += 1;
                break;
            }
        }
    }
    // the digits kept, padded with zeros up to the point
    while (digits.len() as i64) < point {
        digits.push(0);
    }
    let point = point.max(0) as usize;
    let mut out = String::new();
    let all_zero = digits.iter().all(|&d| d == 0);
    if neg && !all_zero {
        out.push('-');
    }
    let int: String = digits[..point]
        .iter()
        .map(|&d| char::from(b'0' + d))
        .collect();
    match int.trim_start_matches('0') {
        "" => out.push('0'),
        t => out.push_str(t),
    }
    if digits.len() > point {
        out.push('.');
        out.extend(digits[point..].iter().map(|&d| char::from(b'0' + d)));
    }
    out
}

/// `fn:numeric-mod`: the remainder of a truncating division, in the promoted type of the
/// operands; division by zero is an error (also for floats and doubles, as in ARQ).
pub fn numeric_mod(a: Num, b: Num) -> EvalResult<Value> {
    Ok(match a.rank().max(b.rank()) {
        0 => {
            let (Num::Integer(x), Num::Integer(y)) = (a, b) else {
                return Err(TypeError);
            };
            Value::Integer(x.checked_rem(y).ok_or(TypeError)?)
        }
        1 => {
            let (x, y) = (
                a.to_decimal().ok_or(TypeError)?,
                b.to_decimal().ok_or(TypeError)?,
            );
            Value::Decimal(x.checked_rem(y).ok_or(TypeError)?)
        }
        2 => {
            let (x, y) = (f32::from(a.to_float()), f32::from(b.to_float()));
            if y == 0.0 {
                return Err(TypeError);
            }
            Value::Float((x % y).into())
        }
        _ => {
            let (x, y) = (f64::from(a.to_double()), f64::from(b.to_double()));
            if y == 0.0 {
                return Err(TypeError);
            }
            Value::Double((x % y).into())
        }
    })
}

/// `fn:numeric-integer-divide` (`idiv`): the quotient truncated towards zero, as an
/// `xsd:integer`. Division by zero, NaN and infinite operands are errors.
pub fn numeric_integer_divide(a: Num, b: Num) -> EvalResult<Value> {
    let q = match a.rank().max(b.rank()) {
        0 => {
            let (Num::Integer(x), Num::Integer(y)) = (a, b) else {
                return Err(TypeError);
            };
            x.checked_div(y).ok_or(TypeError)?
        }
        1 => {
            let (x, y) = (
                a.to_decimal().ok_or(TypeError)?,
                b.to_decimal().ok_or(TypeError)?,
            );
            if y == Decimal::from(0) {
                return Err(TypeError);
            }
            // x = q * y + r, |r| < |y|, r with the sign of x
            let r = x.checked_rem(y).ok_or(TypeError)?;
            let q = x
                .checked_sub(r)
                .and_then(|d| d.checked_div(y))
                .ok_or(TypeError)?;
            Integer::try_from(q).map_err(|_| TypeError)?
        }
        _ => {
            let (x, y) = (f64::from(a.to_double()), f64::from(b.to_double()));
            if y == 0.0 || x.is_nan() || y.is_nan() || x.is_infinite() {
                return Err(TypeError);
            }
            let q = (x / y).trunc();
            if q.abs() >= 9.2e18 {
                return Err(TypeError);
            }
            Integer::from(q as i64)
        }
    };
    Ok(Value::Integer(q))
}

/// The components of a duration, normalized and signed as F&O has them
/// (`fn:years-from-duration` … `fn:seconds-from-duration`).
pub fn duration_part(v: &Value, part: &str) -> EvalResult<Value> {
    let d: oxsdatatypes::Duration = match v {
        Value::Duration(d) => *d,
        Value::DayTime(d) => (*d).into(),
        Value::YearMonth(d) => (*d).into(),
        _ => return Err(TypeError),
    };
    let int = |i: i64| Ok(Value::Integer(i.into()));
    match part {
        "years" => int(d.years()),
        "months" => int(d.months()),
        "days" => int(d.days()),
        "hours" => int(d.hours()),
        "minutes" => int(d.minutes()),
        "seconds" => Ok(Value::Decimal(d.seconds())),
        _ => Err(TypeError),
    }
}

/// The timezone argument of `fn:adjust-*-to-timezone` and `afn:adjust-to-timezone`:
/// `None` (no argument) is the implicit timezone, UTC; an `xsd:dayTimeDuration` is an
/// offset; the empty string removes the timezone.
pub enum TzArg {
    Implicit,
    Offset(DayTimeDuration),
    Remove,
}

/// Adjust an `xsd:dateTime`, `xsd:date` or `xsd:time` to a timezone (F&O 3.1 §10.7).
/// `kind` restricts the argument's type: `Some("dateTime")` and so on for the `fn:`
/// functions, `None` for `afn:adjust-to-timezone`.
pub fn adjust(v: &Value, tz: TzArg, kind: Option<&str>) -> EvalResult<Value> {
    let offset = match tz {
        TzArg::Implicit => Some(TimezoneOffset::UTC),
        TzArg::Offset(d) => Some(TimezoneOffset::try_from(d).map_err(|_| TypeError)?),
        TzArg::Remove => None,
    };
    let ok = |k: &str| kind.is_none_or(|want| want == k);
    match v {
        Value::DateTime(d) if ok("dateTime") => {
            d.adjust(offset).map(Value::DateTime).ok_or(TypeError)
        }
        Value::Date(d) if ok("date") => d.adjust(offset).map(Value::Date).ok_or(TypeError),
        Value::Time(t) if ok("time") => t.adjust(offset).map(Value::Time).ok_or(TypeError),
        _ => Err(TypeError),
    }
}

/// `fn:dateTime($date, $time)`: the date and the time as one `xsd:dateTime`, with the
/// timezone of either; two different timezones are an error.
pub fn date_time(date: &Value, time: &Value) -> EvalResult<Value> {
    let (Value::Date(d), Value::Time(t)) = (date, time) else {
        return Err(TypeError);
    };
    let tz = match (d.timezone_offset(), t.timezone_offset()) {
        (Some(a), Some(b)) if a != b => return Err(TypeError),
        (a, b) => a.or(b),
    };
    let d = Date::adjust(*d, None).ok_or(TypeError)?;
    let t = Time::adjust(*t, None).ok_or(TypeError)?;
    let lex = format!("{d}T{t}{}", tz.map(|z| z.to_string()).unwrap_or_default());
    DateTime::from_str(&lex)
        .map(Value::DateTime)
        .map_err(|_| TypeError)
}

/// `fn:normalize-unicode($s, $form)`: NFC (the default), NFD, NFKC or NFKD, named
/// case-insensitively; the empty name leaves the string as it is.
pub fn normalize_unicode(s: &str, form: Option<&str>) -> EvalResult<String> {
    use icu_normalizer::{ComposingNormalizerBorrowed, DecomposingNormalizerBorrowed};
    let form = form.unwrap_or("NFC").trim().to_ascii_uppercase();
    Ok(match form.as_str() {
        "" => s.to_string(),
        "NFC" => ComposingNormalizerBorrowed::new_nfc()
            .normalize(s)
            .into_owned(),
        "NFKC" => ComposingNormalizerBorrowed::new_nfkc()
            .normalize(s)
            .into_owned(),
        "NFD" => DecomposingNormalizerBorrowed::new_nfd()
            .normalize(s)
            .into_owned(),
        "NFKD" => DecomposingNormalizerBorrowed::new_nfkd()
            .normalize(s)
            .into_owned(),
        _ => return Err(TypeError),
    })
}

/// `afn:substr($s, $start[, $end])` and `afn:substring`: Java's zero-based
/// `String.substring`, counted in characters; out of range is an error.
pub fn java_substring(s: &str, start: i64, end: Option<i64>) -> EvalResult<String> {
    let n = s.chars().count() as i64;
    let end = end.unwrap_or(n);
    if start < 0 || end > n || start > end {
        return Err(TypeError);
    }
    Ok(s.chars()
        .skip(start as usize)
        .take((end - start) as usize)
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rounds_decimal_numerals() {
        let r = |s: &str, p: i64, e: bool| round_digits(s, p, e);
        assert_eq!(r("2.5", 0, true), "2");
        assert_eq!(r("3.5", 0, true), "4");
        assert_eq!(r("2.5", 0, false), "3");
        assert_eq!(r("-2.5", 0, false), "-2");
        assert_eq!(r("-2.5", 0, true), "-2");
        assert_eq!(r("-2.51", 0, false), "-3");
        assert_eq!(r("3.14159", 2, true), "3.14");
        assert_eq!(r("3.145", 2, true), "3.14");
        assert_eq!(r("3.145", 2, false), "3.15");
        assert_eq!(r("9.99", 1, true), "10.0");
        assert_eq!(r("12345", -2, true), "12300");
        assert_eq!(r("12350", -2, true), "12400");
        assert_eq!(r("12250", -2, true), "12200");
        assert_eq!(r("-0.4", 0, true), "0");
        assert_eq!(r("49", -2, true), "0");
        assert_eq!(r("51", -2, true), "100");
        assert_eq!(r("5", -3, true), "0");
        assert_eq!(r("1.5", 3, true), "1.5");
    }

    #[test]
    fn rounds_with_the_argument_type() {
        let d = |x: f64| Num::Double(x.into());
        assert!(matches!(round(d(0.125), 2, true), Ok(Value::Double(x)) if f64::from(x) == 0.12));
        // 0.15 is a little less than 0.15 as a double
        assert!(matches!(round(d(0.15), 1, false), Ok(Value::Double(x)) if f64::from(x) == 0.1));
        let i = Num::Integer(1250.into());
        assert!(matches!(round(i, -2, true), Ok(Value::Integer(x)) if x == Integer::from(1200)));
    }

    #[test]
    fn idiv_and_mod() {
        let i = |x: i64| Num::Integer(x.into());
        let int = |v: EvalResult<Value>| match v {
            Ok(Value::Integer(x)) => i64::from(x),
            v => panic!("{v:?}"),
        };
        assert_eq!(int(numeric_integer_divide(i(7), i(-2))), -3);
        assert_eq!(int(numeric_mod(i(-7), i(2))), -1);
        assert!(numeric_mod(i(1), i(0)).is_err());
        let dec = |s: &str| Num::Decimal(Decimal::from_str(s).unwrap());
        assert_eq!(int(numeric_integer_divide(dec("-7.5"), dec("2"))), -3);
        assert_eq!(
            int(numeric_integer_divide(Num::Double(10.5.into()), i(3))),
            3
        );
    }

    #[test]
    fn substrings_and_normal_forms() {
        assert_eq!(java_substring("héllo", 1, Some(3)).unwrap(), "él");
        assert_eq!(java_substring("héllo", 2, None).unwrap(), "llo");
        assert!(java_substring("abc", 2, Some(5)).is_err());
        assert_eq!(normalize_unicode("e\u{301}", None).unwrap(), "\u{e9}");
        assert_eq!(
            normalize_unicode("\u{e9}", Some("nfd")).unwrap(),
            "e\u{301}"
        );
        assert!(normalize_unicode("x", Some("fully-normalized")).is_err());
    }
}
