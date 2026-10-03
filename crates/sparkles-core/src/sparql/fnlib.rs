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

/// The local system timezone's offset from UTC, in seconds (UTC where it is unknown).
pub fn local_offset() -> i64 {
    #[cfg(unix)]
    {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs() as libc::time_t);
        let mut tm = std::mem::MaybeUninit::<libc::tm>::zeroed();
        // SAFETY: localtime_r writes the broken-down time into `tm`, which outlives the
        // call, and reads only `now`
        let ok = unsafe { !libc::localtime_r(&now, tm.as_mut_ptr()).is_null() };
        if ok {
            // SAFETY: localtime_r succeeded, so it initialized `tm`
            let offset = unsafe { tm.assume_init() }.tm_gmtoff;
            // a c_long, which is narrower than i64 on some targets
            #[allow(clippy::useless_conversion)]
            return i64::from(offset);
        }
    }
    0
}

/// Base64 with MIME line breaks (76 characters a line, CRLF), as Java's
/// `Base64.getMimeEncoder()` writes it.
pub fn base64_mime(bytes: &[u8]) -> String {
    const A: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    let mut line = 0;
    for chunk in bytes.chunks(3) {
        if line == 76 {
            out.push_str("\r\n");
            line = 0;
        }
        let n = chunk.len();
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let v = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        for i in 0..4 {
            if i <= n {
                out.push(char::from(A[((v >> (18 - 6 * i)) & 63) as usize]));
            } else {
                out.push('=');
            }
        }
        line += 4;
    }
    out
}

/// The operators on dates, times and durations that SPARQL's built-in arithmetic leaves
/// out, as ARQ and F&O 3.1 define them: a date or time plus or minus a duration, the
/// difference of two times, a duration times or divided by a number, and the ratio of
/// two durations. A day-time duration stays one, where ARQ gives an `xsd:duration`.
/// `None` when the operands are none of these.
pub fn temporal(x: &Value, y: &Value, op: &super::value::NumOp) -> Option<EvalResult<Value>> {
    use super::value::NumOp;
    let some = |r: Option<Value>| Some(r.ok_or(TypeError));
    let seconds = |n: &Value| {
        Num::of(n).ok().and_then(|n| {
            n.to_decimal()
                .or_else(|| Decimal::try_from(n.to_double()).ok())
        })
    };
    match (x, y, op) {
        (Value::Date(d), Value::YearMonth(e), NumOp::Add) => {
            some(d.checked_add_year_month_duration(*e).map(Value::Date))
        }
        (Value::Date(d), Value::YearMonth(e), NumOp::Sub) => {
            some(d.checked_sub_year_month_duration(*e).map(Value::Date))
        }
        (Value::Date(d), Value::DayTime(e), NumOp::Add) => {
            some(d.checked_add_day_time_duration(*e).map(Value::Date))
        }
        (Value::Date(d), Value::DayTime(e), NumOp::Sub) => {
            some(d.checked_sub_day_time_duration(*e).map(Value::Date))
        }
        (Value::Date(d), Value::Duration(e), NumOp::Add) => {
            some(d.checked_add_duration(*e).map(Value::Date))
        }
        (Value::Date(d), Value::Duration(e), NumOp::Sub) => {
            some(d.checked_sub_duration(*e).map(Value::Date))
        }
        (Value::Time(t), Value::DayTime(e), NumOp::Add) => {
            some(t.checked_add_day_time_duration(*e).map(Value::Time))
        }
        (Value::Time(t), Value::DayTime(e), NumOp::Sub) => {
            some(t.checked_sub_day_time_duration(*e).map(Value::Time))
        }
        (Value::Time(t), Value::Duration(e), NumOp::Add) => {
            some(t.checked_add_duration(*e).map(Value::Time))
        }
        (Value::Time(t), Value::Duration(e), NumOp::Sub) => {
            some(t.checked_sub_duration(*e).map(Value::Time))
        }
        (Value::Time(t), Value::Time(u), NumOp::Sub) => some(t.checked_sub(*u).map(Value::DayTime)),
        // durations of different kinds, as general durations
        (
            Value::Duration(_) | Value::DayTime(_) | Value::YearMonth(_),
            Value::Duration(_) | Value::DayTime(_) | Value::YearMonth(_),
            NumOp::Add | NumOp::Sub,
        ) => {
            let general = |v: &Value| match v {
                Value::Duration(d) => *d,
                Value::DayTime(d) => (*d).into(),
                Value::YearMonth(d) => (*d).into(),
                _ => unreachable!("a duration"),
            };
            let (d, e) = (general(x), general(y));
            some(
                match op {
                    NumOp::Add => d.checked_add(e),
                    _ => d.checked_sub(e),
                }
                .map(Value::Duration),
            )
        }
        (Value::DayTime(d), n, NumOp::Mul) | (n, Value::DayTime(d), NumOp::Mul)
            if n.is_numeric() =>
        {
            some(
                seconds(n)
                    .and_then(|k| d.as_seconds().checked_mul(k))
                    .map(|s| Value::DayTime(DayTimeDuration::new(s))),
            )
        }
        (Value::DayTime(d), n, NumOp::Div) if n.is_numeric() => some(
            seconds(n)
                .filter(|k| *k != Decimal::from(0))
                .and_then(|k| d.as_seconds().checked_div(k))
                .map(|s| Value::DayTime(DayTimeDuration::new(s))),
        ),
        (Value::YearMonth(d), n, NumOp::Mul) | (n, Value::YearMonth(d), NumOp::Mul)
            if n.is_numeric() =>
        {
            // F&O: the months, multiplied and rounded half up
            let months = Decimal::from(d.years() * 12 + d.months());
            some(
                seconds(n)
                    .and_then(|k| months.checked_mul(k))
                    .and_then(round_months)
                    .map(|m| Value::YearMonth(oxsdatatypes::YearMonthDuration::new(m))),
            )
        }
        (Value::YearMonth(d), n, NumOp::Div) if n.is_numeric() => {
            let months = Decimal::from(d.years() * 12 + d.months());
            some(
                seconds(n)
                    .filter(|k| *k != Decimal::from(0))
                    .and_then(|k| months.checked_div(k))
                    .and_then(round_months)
                    .map(|m| Value::YearMonth(oxsdatatypes::YearMonthDuration::new(m))),
            )
        }
        (Value::DayTime(d), Value::DayTime(e), NumOp::Div) => some(
            (e.as_seconds() != Decimal::from(0))
                .then(|| d.as_seconds().checked_div(e.as_seconds()))
                .flatten()
                .map(Value::Decimal),
        ),
        (Value::YearMonth(d), Value::YearMonth(e), NumOp::Div) => {
            let (a, b) = (d.years() * 12 + d.months(), e.years() * 12 + e.months());
            some(
                (b != 0)
                    .then(|| Decimal::from(a).checked_div(Decimal::from(b)))
                    .flatten()
                    .map(Value::Decimal),
            )
        }
        _ => None,
    }
}

/// A number of months rounded half up (F&O's rounding of `yearMonthDuration` products).
fn round_months(m: Decimal) -> Option<i64> {
    let half = Decimal::from_str("0.5").ok()?;
    let r = if m >= Decimal::from(0) {
        m.checked_add(half)?.checked_floor()?
    } else {
        m.checked_sub(half)?.checked_ceil()?
    };
    Integer::try_from(r).ok().map(i64::from)
}

/// Where Jena splits an IRI into a namespace and a local name (`SplitIRI.splitXML`): the
/// local name is the longest XML 1.1 NCName at the end that starts with an NCName start
/// character, never the whole IRI, and does not break a `%` escape. `mailto:` keeps a
/// character after it. An IRI that ends with no such name splits at its end.
pub fn split_xml(iri: &str) -> usize {
    fn start(c: char) -> bool {
        matches!(c,
            'A'..='Z' | '_' | 'a'..='z' | '\u{C0}'..='\u{2FF}' | '\u{370}'..='\u{37D}'
            | '\u{37F}'..='\u{1FFF}' | '\u{200C}'..='\u{200D}' | '\u{2070}'..='\u{218F}'
            | '\u{2C00}'..='\u{2FEF}' | '\u{3001}'..='\u{D7FF}' | '\u{F900}'..='\u{FDCF}'
            | '\u{FDF0}'..='\u{FFFD}' | '\u{10000}'..='\u{EFFFF}')
    }
    fn name(c: char) -> bool {
        start(c)
            || matches!(c, '-' | '.' | '0'..='9' | '\u{B7}' | '\u{300}'..='\u{36F}'
                | '\u{203F}'..='\u{2040}')
    }
    let chars: Vec<(usize, char)> = iri.char_indices().collect();
    let n = chars.len();
    if n == 0 {
        return 0;
    }
    let mut i = n - 1;
    while i >= 1 && name(chars[i].1) {
        i -= 1;
    }
    let mut j = i + 1;
    if j >= n {
        return iri.len();
    }
    if j >= 2 && chars[j - 2].1 == '%' {
        j += 1;
    }
    if chars[j - 1].1 == '%' {
        j += 2;
        if j > n {
            return iri.len();
        }
    }
    while j < n {
        if start(chars[j].1) && !(j == 7 && iri.starts_with("mailto:")) {
            break;
        }
        j += 1;
    }
    chars.get(j).map_or(iri.len(), |c| c.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_iris_as_jena() {
        fn split(s: &str) -> (&str, &str) {
            s.split_at(split_xml(s))
        }
        assert_eq!(
            split("http://www.w3.org/2001/XMLSchema#integer"),
            ("http://www.w3.org/2001/XMLSchema#", "integer")
        );
        assert_eq!(split("http://example/a/b"), ("http://example/a/", "b"));
        assert_eq!(split("http://example/a/"), ("http://example/a/", ""));
        assert_eq!(split("http://example/a/1x"), ("http://example/a/1", "x"));
        assert_eq!(split("urn:x"), ("urn:", "x"));
        assert_eq!(split("mailto:me"), ("mailto:m", "e"));
        assert_eq!(split("http://ex/a%20b"), ("http://ex/a%20", "b"));
    }

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
