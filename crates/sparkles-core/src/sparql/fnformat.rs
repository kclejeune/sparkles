//! Text formatting functions of ARQ's library: `afn:sprintf`, which formats as Java's
//! `String.format` does, and `fn:format-number`, which ARQ implements with Java's
//! `DecimalFormat` in the root locale.

use super::value::{EvalResult, TypeError};
use oxsdatatypes::{DateTime, Decimal};

/// An argument of `afn:sprintf`, as ARQ hands it to `String.format`: numbers as
/// `BigInteger`, `BigDecimal`, `Double` or `Float`, dates and dateTimes as a
/// `java.util.Date`, booleans, and strings (every other term as its string).
#[derive(Clone, Debug)]
pub enum Arg {
    Integer(i128),
    Decimal(Decimal),
    Double(f64),
    Float(f32),
    Date(DateTime),
    Bool(bool),
    Str(String),
}

/// A decimal number as digits and the position of the decimal point: the value is
/// `0.d₁d₂…dₙ × 10^point`, without leading zeros (`digits` empty for zero).
#[derive(Clone, Debug)]
struct Digits {
    neg: bool,
    digits: Vec<u8>,
    point: i64,
}

impl Digits {
    /// From a decimal numeral (`-?\d+(\.\d+)?`) or `{:e}` output (`-?d(.ddd)?e±x`).
    fn parse(s: &str) -> Digits {
        let (neg, s) = match s.strip_prefix('-') {
            Some(r) => (true, r),
            None => (false, s),
        };
        let (mant, exp) = match s.split_once(['e', 'E']) {
            Some((m, e)) => (m, e.parse::<i64>().unwrap_or(0)),
            None => (s, 0),
        };
        let (int, frac) = mant.split_once('.').unwrap_or((mant, ""));
        let mut digits: Vec<u8> = int.bytes().chain(frac.bytes()).map(|b| b - b'0').collect();
        let mut point = int.len() as i64 + exp;
        while digits.first() == Some(&0) {
            digits.remove(0);
            point -= 1;
        }
        while digits.last() == Some(&0) {
            digits.pop();
        }
        if digits.is_empty() {
            point = 0;
        }
        Digits { neg, digits, point }
    }

    /// The shortest digits that read back as `x` (as Java's `Double.toString` finds).
    fn of_f64(x: f64) -> Digits {
        Digits::parse(&format!("{x:e}"))
    }

    fn of_f32(x: f32) -> Digits {
        Digits::parse(&format!("{x:e}"))
    }

    fn is_zero(&self) -> bool {
        self.digits.is_empty()
    }

    /// Rounded half up (away from zero on ties) to `frac` digits after the point.
    fn round_frac(&self, frac: i64) -> Digits {
        self.round_at(self.point + frac)
    }

    /// Rounded half up to `n` significant digits.
    fn round_sig(&self, n: i64) -> Digits {
        self.round_at(n)
    }

    /// Keep the first `keep` digits, rounding half up.
    fn round_at(&self, keep: i64) -> Digits {
        if keep >= self.digits.len() as i64 {
            return self.clone();
        }
        let mut point = self.point;
        if keep < 0 {
            return Digits {
                neg: self.neg,
                digits: Vec::new(),
                point: 0,
            };
        }
        let keep = keep as usize;
        let mut digits = self.digits[..keep].to_vec();
        if self.digits[keep] >= 5 {
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
        while digits.last() == Some(&0) {
            digits.pop();
        }
        if digits.is_empty() {
            point = 0;
        }
        Digits {
            neg: self.neg,
            digits,
            point,
        }
    }

    fn digit(&self, i: i64) -> u8 {
        if i < 0 {
            return 0;
        }
        self.digits.get(i as usize).copied().unwrap_or(0)
    }

    /// The integer part's digits (at least `0`) and `frac` digits after the point,
    /// without the sign.
    fn fixed(&self, frac: usize) -> (String, String) {
        let int: String = if self.point <= 0 {
            "0".into()
        } else {
            (0..self.point)
                .map(|i| char::from(b'0' + self.digit(i)))
                .collect()
        };
        let f: String = (0..frac as i64)
            .map(|i| char::from(b'0' + self.digit(self.point + i)))
            .collect();
        (int, f)
    }
}

/// Insert `sep` between groups of `size` digits, from the right.
fn group(int: &str, sep: char, size: usize) -> String {
    if size == 0 {
        return int.to_string();
    }
    let n = int.len();
    let mut out = String::with_capacity(n + n / size);
    for (i, c) in int.chars().enumerate() {
        if i > 0 && (n - i).is_multiple_of(size) {
            out.push(sep);
        }
        out.push(c);
    }
    out
}

// ------------------------------------------------------------------ afn:sprintf ----

#[derive(Default, Clone, Copy)]
struct Flags {
    left: bool,
    alt: bool,
    plus: bool,
    space: bool,
    zero: bool,
    group: bool,
    paren: bool,
}

/// `afn:sprintf(format, args…)`: Java's `String.format` with ARQ's arguments. A
/// conversion that does not fit its argument, or a missing argument, is an error.
/// Dates are formatted in UTC.
pub fn sprintf(format: &str, args: &[Arg]) -> EvalResult<String> {
    let mut out = String::new();
    let mut chars = format.char_indices().peekable();
    let mut next = 0usize;
    let mut last: Option<usize> = None;
    while let Some((_, c)) = chars.next() {
        if c != '%' {
            out.push(c);
            continue;
        }
        // %[index$][flags][width][.precision]conversion
        let mut spec = String::new();
        let conv = loop {
            let (_, d) = chars.next().ok_or(TypeError)?;
            if d.is_ascii_alphabetic() || d == '%' {
                break d;
            }
            spec.push(d);
        };
        let (conv, tconv) = if conv == 't' || conv == 'T' {
            let (_, t) = chars.next().ok_or(TypeError)?;
            (conv, Some(t))
        } else {
            (conv, None)
        };
        let mut rest = spec.as_str();
        let mut index: Option<usize> = None;
        if let Some(r) = rest.strip_prefix('<') {
            index = Some(last.ok_or(TypeError)?);
            rest = r;
        } else if let Some(p) = rest.find('$') {
            let n: usize = rest[..p].parse().map_err(|_| TypeError)?;
            if n == 0 {
                return Err(TypeError);
            }
            index = Some(n - 1);
            rest = &rest[p + 1..];
        }
        let mut flags = Flags::default();
        let mut iter = rest.chars().peekable();
        while let Some(&f) = iter.peek() {
            match f {
                '-' => flags.left = true,
                '#' => flags.alt = true,
                '+' => flags.plus = true,
                ' ' => flags.space = true,
                '0' => flags.zero = true,
                ',' => flags.group = true,
                '(' => flags.paren = true,
                _ => break,
            }
            iter.next();
        }
        let rest: String = iter.collect();
        let (width, precision) = match rest.split_once('.') {
            Some((w, p)) => (w, Some(p.parse::<usize>().map_err(|_| TypeError)?)),
            None => (rest.as_str(), None),
        };
        let width: Option<usize> = if width.is_empty() {
            None
        } else {
            Some(width.parse().map_err(|_| TypeError)?)
        };
        if (flags.left || flags.zero) && width.is_none() {
            return Err(TypeError);
        }
        if conv == '%' {
            out.push_str(&pad("%".into(), width, flags.left, ' '));
            continue;
        }
        if conv == 'n' {
            out.push('\n');
            continue;
        }
        let i = match index {
            Some(i) => i,
            None => {
                next += 1;
                next - 1
            }
        };
        last = Some(i);
        let arg = args.get(i).ok_or(TypeError)?;
        let text = match conv {
            'b' | 'B' => {
                let s = match arg {
                    Arg::Bool(b) => b.to_string(),
                    _ => "true".into(),
                };
                truncate(s, precision)
            }
            's' | 'S' => truncate(to_java_string(arg), precision),
            // Java formats no BigInteger or String with %c
            'c' | 'C' => return Err(TypeError),
            'd' => {
                let Arg::Integer(n) = arg else {
                    return Err(TypeError);
                };
                if precision.is_some() {
                    return Err(TypeError);
                }
                let digits = n.unsigned_abs().to_string();
                let digits = if flags.group {
                    group(&digits, ',', 3)
                } else {
                    digits
                };
                return_signed(&mut out, digits, *n < 0, flags, width);
                continue;
            }
            'o' | 'x' | 'X' => {
                let Arg::Integer(n) = arg else {
                    return Err(TypeError);
                };
                let mag = n.unsigned_abs();
                let mut digits = match conv {
                    'o' => format!("{mag:o}"),
                    _ => format!("{mag:x}"),
                };
                if flags.alt {
                    digits.insert_str(0, if conv == 'o' { "0" } else { "0x" });
                }
                if conv == 'X' {
                    digits = digits.to_uppercase();
                }
                return_signed(&mut out, digits, *n < 0, flags, width);
                continue;
            }
            'e' | 'E' | 'f' | 'g' | 'G' => {
                let d = match arg {
                    a if !finite(a) => {
                        let x = match a {
                            Arg::Float(f) => f64::from(*f),
                            Arg::Double(x) => *x,
                            _ => return Err(TypeError),
                        };
                        let s = if x.is_nan() {
                            "NaN".to_string()
                        } else if x > 0.0 {
                            if flags.plus { "+Infinity" } else { "Infinity" }.to_string()
                        } else if flags.paren {
                            "(Infinity)".to_string()
                        } else {
                            "-Infinity".to_string()
                        };
                        out.push_str(&pad(s, width, flags.left, ' '));
                        continue;
                    }
                    Arg::Double(x) => Digits::of_f64(*x),
                    Arg::Float(x) => Digits::of_f32(*x),
                    Arg::Decimal(x) => Digits::parse(&x.to_string()),
                    _ => return Err(TypeError),
                };
                let neg = d.neg;
                let p = precision.unwrap_or(6);
                let body = match conv {
                    'f' => fixed(&d, p, flags.group),
                    'e' | 'E' => scientific(&d, p, conv == 'E'),
                    _ => {
                        // %g: `p` significant digits, in decimal notation from 10⁻⁴ up
                        // to 10^p
                        let p = p.max(1);
                        let r = d.round_sig(p as i64);
                        let m = if r.is_zero() { 0 } else { r.point - 1 };
                        let s = if (-4..p as i64).contains(&m) {
                            fixed(&d, (p as i64 - 1 - m) as usize, flags.group)
                        } else {
                            scientific(&d, p - 1, conv == 'G')
                        };
                        if conv == 'G' { s.to_uppercase() } else { s }
                    }
                };
                return_signed(&mut out, body, neg, flags, width);
                continue;
            }
            't' | 'T' => {
                let Arg::Date(dt) = arg else {
                    return Err(TypeError);
                };
                let s = date_conversion(dt, tconv.ok_or(TypeError)?)?;
                if conv == 'T' { s.to_uppercase() } else { s }
            }
            _ => return Err(TypeError),
        };
        let text = if conv.is_ascii_uppercase() && conv != 'T' {
            text.to_uppercase()
        } else {
            text
        };
        out.push_str(&pad(text, width, flags.left, ' '));
    }
    Ok(out)
}

fn finite(a: &Arg) -> bool {
    match a {
        Arg::Double(x) => x.is_finite(),
        Arg::Float(x) => x.is_finite(),
        _ => true,
    }
}

fn truncate(s: String, precision: Option<usize>) -> String {
    match precision {
        Some(p) => s.chars().take(p).collect(),
        None => s,
    }
}

fn pad(s: String, width: Option<usize>, left: bool, fill: char) -> String {
    let n = s.chars().count();
    match width {
        Some(w) if w > n => {
            let f: String = std::iter::repeat_n(fill, w - n).collect();
            if left { s + &f } else { f + &s }
        }
        _ => s,
    }
}

/// Write a number's `digits` with its sign and the flags' padding.
fn return_signed(out: &mut String, digits: String, neg: bool, flags: Flags, width: Option<usize>) {
    let (pre, post) = if neg {
        if flags.paren { ("(", ")") } else { ("-", "") }
    } else if flags.plus {
        ("+", "")
    } else if flags.space {
        (" ", "")
    } else {
        ("", "")
    };
    let len = pre.len() + digits.chars().count() + post.len();
    let s = match width {
        Some(w) if flags.zero && w > len => {
            format!("{pre}{}{digits}{post}", "0".repeat(w - len))
        }
        _ => pad(format!("{pre}{digits}{post}"), width, flags.left, ' '),
    };
    out.push_str(&s);
}

/// `%f`: `p` digits after the point, rounded half up.
fn fixed(d: &Digits, p: usize, grouping: bool) -> String {
    let r = d.round_frac(p as i64);
    let (int, frac) = r.fixed(p);
    let int = if grouping { group(&int, ',', 3) } else { int };
    if p == 0 { int } else { format!("{int}.{frac}") }
}

/// `%e`: one digit, `p` after the point, and an exponent of at least two digits.
fn scientific(d: &Digits, p: usize, upper: bool) -> String {
    let r = d.round_sig(p as i64 + 1);
    let exp = if r.is_zero() { 0 } else { r.point - 1 };
    let mant: String = (0..=p as i64)
        .map(|i| char::from(b'0' + r.digit(i)))
        .collect();
    let (first, rest) = mant.split_at(1);
    let e = if upper { 'E' } else { 'e' };
    let sign = if exp < 0 { '-' } else { '+' };
    if p == 0 {
        format!("{first}{e}{sign}{:02}", exp.abs())
    } else {
        format!("{first}.{rest}{e}{sign}{:02}", exp.abs())
    }
}

/// Java's `toString` of an argument (`%s`).
fn to_java_string(a: &Arg) -> String {
    match a {
        Arg::Integer(n) => n.to_string(),
        Arg::Decimal(d) => d.to_string(),
        Arg::Double(x) => java_double(*x, false),
        Arg::Float(x) => java_double(f64::from(*x), true),
        Arg::Bool(b) => b.to_string(),
        Arg::Str(s) => s.clone(),
        Arg::Date(dt) => date_conversion(dt, 'c').unwrap_or_default(),
    }
}

/// Java's `Double.toString`: decimal notation between 10⁻³ and 10⁷, else `d.dddE±n`.
fn java_double(x: f64, float: bool) -> String {
    if x.is_nan() {
        return "NaN".into();
    }
    if x.is_infinite() {
        return if x > 0.0 { "Infinity" } else { "-Infinity" }.into();
    }
    let d = if float {
        Digits::of_f32(x as f32)
    } else {
        Digits::of_f64(x)
    };
    let sign = if d.neg { "-" } else { "" };
    if d.is_zero() {
        return format!("{sign}0.0");
    }
    let m = d.point - 1;
    if (-3..7).contains(&m) {
        let frac = (d.digits.len() as i64 - d.point).max(1) as usize;
        let (int, f) = d.fixed(frac);
        format!("{sign}{int}.{f}")
    } else {
        let digits: String = d.digits.iter().map(|&b| char::from(b'0' + b)).collect();
        let (first, rest) = digits.split_at(1);
        let rest = if rest.is_empty() { "0" } else { rest };
        format!("{sign}{first}.{rest}E{m}")
    }
}

/// A `%t` conversion of a date-time, in UTC.
fn date_conversion(dt: &DateTime, c: char) -> EvalResult<String> {
    let utc = dt
        .adjust(Some(oxsdatatypes::TimezoneOffset::UTC))
        .ok_or(TypeError)?;
    let (y, mo, d) = (utc.year(), utc.month(), utc.day());
    let (h, mi) = (utc.hour(), utc.minute());
    let sec = utc.second();
    let whole = |d: Decimal| -> EvalResult<i64> {
        let i = oxsdatatypes::Integer::try_from(d.checked_floor().ok_or(TypeError)?)
            .map_err(|_| TypeError)?;
        Ok(i64::from(i))
    };
    let s = whole(sec)?;
    let millis = whole(
        sec.checked_sub(Decimal::from(s))
            .and_then(|f| f.checked_mul(Decimal::from(1000)))
            .ok_or(TypeError)?,
    )?;
    const MONTHS: [&str; 12] = [
        "January",
        "February",
        "March",
        "April",
        "May",
        "June",
        "July",
        "August",
        "September",
        "October",
        "November",
        "December",
    ];
    const DAYS: [&str; 7] = [
        "Monday",
        "Tuesday",
        "Wednesday",
        "Thursday",
        "Friday",
        "Saturday",
        "Sunday",
    ];
    // days since 1970-01-01, and the weekday (1970-01-01 was a Thursday)
    let days = days_from_civil(y, mo, d);
    let weekday = DAYS[((days % 7 + 7 + 3) % 7) as usize];
    let month = MONTHS[(mo - 1) as usize];
    let h12 = if h % 12 == 0 { 12 } else { h % 12 };
    let epoch = days * 86_400 + i64::from(h) * 3600 + i64::from(mi) * 60 + s;
    Ok(match c {
        'Y' => format!("{y:04}"),
        'y' => format!("{:02}", y.rem_euclid(100)),
        'C' => format!("{:02}", y.div_euclid(100)),
        'm' => format!("{mo:02}"),
        'd' => format!("{d:02}"),
        'e' => d.to_string(),
        'j' => format!("{:03}", days - days_from_civil(y, 1, 1) + 1),
        'B' => month.into(),
        'b' | 'h' => month[..3].into(),
        'A' => weekday.into(),
        'a' => weekday[..3].into(),
        'H' => format!("{h:02}"),
        'k' => h.to_string(),
        'I' => format!("{h12:02}"),
        'l' => h12.to_string(),
        'M' => format!("{mi:02}"),
        'S' => format!("{s:02}"),
        'L' => format!("{millis:03}"),
        'N' => format!("{:09}", millis * 1_000_000),
        'p' => if h < 12 { "am" } else { "pm" }.into(),
        'Z' => "UTC".into(),
        'z' => "+0000".into(),
        's' => epoch.to_string(),
        'Q' => (epoch * 1000 + millis).to_string(),
        'R' => format!("{h:02}:{mi:02}"),
        'T' => format!("{h:02}:{mi:02}:{s:02}"),
        'r' => format!(
            "{h12:02}:{mi:02}:{s:02} {}",
            if h < 12 { "AM" } else { "PM" }
        ),
        'D' => format!("{mo:02}/{d:02}/{:02}", y.rem_euclid(100)),
        'F' => format!("{y:04}-{mo:02}-{d:02}"),
        'c' => format!(
            "{} {} {d:02} {h:02}:{mi:02}:{s:02} UTC {y}",
            &weekday[..3],
            &month[..3]
        ),
        _ => return Err(TypeError),
    })
}

/// Days from 1970-01-01 to a proleptic Gregorian date.
fn days_from_civil(y: i64, m: u8, d: u8) -> i64 {
    let (m, d) = (i64::from(m), i64::from(d));
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

// ------------------------------------------------------------- fn:format-number ----

/// The parts of a `DecimalFormat` pattern.
struct Pattern {
    prefix: String,
    suffix: String,
    neg: Option<(String, String)>,
    min_int: usize,
    group: usize,
    min_frac: usize,
    max_frac: usize,
    /// minimum exponent digits, for scientific notation
    exp: Option<usize>,
    /// the largest number of integer digits, for scientific notation
    max_int: usize,
    multiplier: i32,
}

/// The literal text a pattern character starts: a quoted text, `''` for a quote, or the
/// character itself.
fn literal_text(c: char, chars: &mut std::iter::Peekable<std::str::Chars<'_>>) -> String {
    if c != '\'' {
        return c.to_string();
    }
    if chars.peek() == Some(&'\'') {
        chars.next();
        return "'".to_string();
    }
    let mut s = String::new();
    for q in chars.by_ref() {
        if q == '\'' {
            break;
        }
        s.push(q);
    }
    s
}

/// Split off the prefix, the number part and the suffix of one subpattern.
fn subpattern(p: &str) -> EvalResult<(String, String, String, i32)> {
    let mut prefix = String::new();
    let mut number = String::new();
    let mut suffix = String::new();
    let mut multiplier = 1;
    let mut stage = 0;
    let mut chars = p.chars().peekable();
    while let Some(c) = chars.next() {
        let special = matches!(c, '#' | '0' | ',' | '.' | 'E');
        if stage == 0 && special && c != 'E' {
            stage = 1;
        } else if stage == 1 && !(special || (number.contains('E') && c.is_ascii_digit())) {
            stage = 2;
        }
        match stage {
            1 => number.push(c),
            _ => {
                if c == '%' {
                    multiplier = 100;
                } else if c == '\u{2030}' {
                    multiplier = 1000;
                }
                let t = literal_text(c, &mut chars);
                if stage == 0 {
                    prefix.push_str(&t);
                } else {
                    suffix.push_str(&t);
                }
            }
        }
    }
    if number.is_empty() {
        return Err(TypeError);
    }
    Ok((prefix, number, suffix, multiplier))
}

fn parse_pattern(picture: &str) -> EvalResult<Pattern> {
    let (pos, neg) = match picture.split_once(';') {
        Some((p, n)) => (p, Some(n)),
        None => (picture, None),
    };
    let (prefix, number, suffix, multiplier) = subpattern(pos)?;
    let neg = match neg {
        Some(n) => {
            let (np, _, ns, _) = subpattern(n)?;
            Some((np, ns))
        }
        None => None,
    };
    let (mantissa, exp) = match number.split_once('E') {
        Some((m, e)) => {
            if e.is_empty() || !e.bytes().all(|b| b == b'0') {
                return Err(TypeError);
            }
            (m, Some(e.len()))
        }
        None => (number.as_str(), None),
    };
    let (int, frac) = mantissa.split_once('.').unwrap_or((mantissa, ""));
    if frac.contains(',') {
        return Err(TypeError);
    }
    let group = match int.rfind(',') {
        Some(p) => int.len() - p - 1,
        None => 0,
    };
    let int_digits: String = int.chars().filter(|&c| c != ',').collect();
    let mut min_int = int_digits.chars().filter(|&c| c == '0').count();
    // `#.##` is `0.##` (DecimalFormat's applyPattern), and `.##` stays as it is
    if min_int == 0 && !frac.contains('0') && !int_digits.is_empty() && mantissa.contains('.') {
        min_int = 1;
    }
    let max_int = int_digits.len();
    let min_frac = frac.chars().take_while(|&c| c == '0').count();
    let max_frac = frac.len();
    Ok(Pattern {
        prefix,
        suffix,
        neg,
        min_int,
        group,
        min_frac,
        max_frac,
        exp,
        max_int,
        multiplier,
    })
}

/// A number for `fn:format-number`: integers exactly, other numbers as doubles (as
/// ARQ passes them to `DecimalFormat`).
pub enum FormatValue {
    Integer(i128),
    Double(f64),
}

/// The decimal separator, grouping separator and minus sign of a language tag, as
/// Java 21's CLDR data has them for the common languages; the root locale's for others.
fn symbols(locale: Option<&str>) -> (char, char, char) {
    let Some(tag) = locale else {
        return ('.', ',', '-');
    };
    let tag = tag.to_ascii_lowercase().replace('_', "-");
    if tag == "de-ch" || tag == "it-ch" {
        return ('.', '\u{2019}', '-');
    }
    match tag.split('-').next().unwrap_or("") {
        "de" | "es" | "it" | "nl" | "pt" | "id" | "tr" | "da" | "el" | "ro" | "hr" | "sl" => {
            (',', '.', '-')
        }
        "fr" => (',', '\u{202f}', '-'),
        "sv" | "fi" | "nb" | "no" | "nn" => (',', '\u{a0}', '\u{2212}'),
        "ru" | "uk" | "pl" | "cs" | "sk" | "hu" | "bg" | "et" | "lv" | "lt" => (',', '\u{a0}', '-'),
        _ => ('.', ',', '-'),
    }
}

/// `fn:format-number(value, picture[, locale])` as ARQ computes it: Java's
/// `DecimalFormat` with the picture as its pattern and the symbols of the language tag
/// (the root locale's without one), rounding half to even.
pub fn format_number(v: FormatValue, picture: &str, locale: Option<&str>) -> EvalResult<String> {
    let p = parse_pattern(picture)?;
    let (dec, sep, minus) = symbols(locale);
    let (neg, digits) = match v {
        FormatValue::Integer(n) => {
            let n = n.checked_mul(i128::from(p.multiplier)).ok_or(TypeError)?;
            (n < 0, Digits::parse(&n.unsigned_abs().to_string()))
        }
        FormatValue::Double(x) => {
            if x.is_nan() {
                return Ok("NaN".into());
            }
            let x = x * f64::from(p.multiplier);
            if x.is_infinite() {
                let (pre, suf) = match (&p.neg, x < 0.0) {
                    (Some((np, ns)), true) => (np.clone(), ns.clone()),
                    (None, true) => (format!("{minus}{}", p.prefix), p.suffix.clone()),
                    _ => (p.prefix.clone(), p.suffix.clone()),
                };
                return Ok(format!("{pre}\u{221e}{suf}"));
            }
            let neg = x.is_sign_negative();
            // the exact binary value, rounded half to even below
            (neg, Digits::parse(&format!("{:.1100}", x.abs())))
        }
    };
    let body = match p.exp {
        None => {
            let r = round_half_even(&digits, digits.point + p.max_frac as i64);
            let (int, frac) = r.fixed(p.max_frac);
            let int = int.trim_start_matches('0');
            let int = format!("{}{int}", "0".repeat(p.min_int.saturating_sub(int.len())));
            let mut frac = frac.trim_end_matches('0').to_string();
            while frac.len() < p.min_frac {
                frac.push('0');
            }
            let int = group(&int, sep, p.group);
            if frac.is_empty() {
                int
            } else {
                format!("{int}{dec}{frac}")
            }
        }
        Some(min_exp) => {
            // the exponent is a multiple of the integer digits when there are optional
            // ones (engineering notation), else it leaves `min_int` digits before the point
            let sig = p.min_int + p.max_frac;
            let (int_digits, exp) = if digits.is_zero() {
                (p.min_int.max(1), 0)
            } else if p.max_int > p.min_int && p.max_int > 1 {
                let e = digits.point - 1;
                let step = p.max_int as i64;
                let e = e.div_euclid(step) * step;
                ((digits.point - e) as usize, e)
            } else {
                let lead = p.min_int.max(1) as i64;
                (lead as usize, digits.point - lead)
            };
            let keep = if p.max_int > p.min_int && p.max_int > 1 {
                (p.min_int.max(1) + p.max_frac) as i64
            } else {
                sig.max(1) as i64
            };
            let r = round_half_even(&digits, keep);
            let shifted = Digits {
                neg: false,
                digits: r.digits.clone(),
                point: r.point - exp,
            };
            let int_digits = int_digits.max(1);
            let (int, frac) = shifted.fixed(p.max_frac);
            let int = format!(
                "{}{}",
                "0".repeat(int_digits.saturating_sub(int.len())),
                int.trim_start_matches('0')
            );
            let int = if int.is_empty() { "0".to_string() } else { int };
            let mut frac = frac.trim_end_matches('0').to_string();
            while frac.len() < p.min_frac {
                frac.push('0');
            }
            let e = format!("{:0w$}", exp.abs(), w = min_exp);
            let e = if exp < 0 { format!("{minus}{e}") } else { e };
            if frac.is_empty() {
                format!("{int}E{e}")
            } else {
                format!("{int}{dec}{frac}E{e}")
            }
        }
    };
    Ok(match (neg, &p.neg) {
        (true, Some((np, ns))) => format!("{np}{body}{ns}"),
        (true, None) => format!("{minus}{}{body}{}", p.prefix, p.suffix),
        (false, _) => format!("{}{body}{}", p.prefix, p.suffix),
    })
}

/// Keep the first `keep` digits, rounding half to even.
fn round_half_even(d: &Digits, keep: i64) -> Digits {
    if keep >= d.digits.len() as i64 {
        return d.clone();
    }
    if keep < 0 {
        return Digits {
            neg: d.neg,
            digits: Vec::new(),
            point: 0,
        };
    }
    let k = keep as usize;
    let tail = &d.digits[k..];
    let up = match tail[0].cmp(&5) {
        std::cmp::Ordering::Greater => true,
        std::cmp::Ordering::Less => false,
        std::cmp::Ordering::Equal => {
            tail[1..].iter().any(|&x| x != 0) || (k > 0 && d.digits[k - 1] % 2 == 1)
        }
    };
    if up {
        // half up from the truncation is the same as rounding up here
        let mut digits = d.digits[..k].to_vec();
        digits.push(9);
        let r = Digits {
            neg: d.neg,
            digits,
            point: d.point,
        };
        r.round_at(k as i64)
    } else {
        let mut digits = d.digits[..k].to_vec();
        while digits.last() == Some(&0) {
            digits.pop();
        }
        Digits {
            neg: d.neg,
            point: if digits.is_empty() { 0 } else { d.point },
            digits,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    fn f(fmt: &str, args: &[Arg]) -> String {
        sprintf(fmt, args).unwrap_or_else(|_| "<error>".into())
    }

    /// Java 21's `String.format` gives the same strings.
    #[test]
    fn sprintf_matches_java() {
        assert_eq!(
            f(
                "%5.2f|%d|%s|%x|%-4s|%e",
                &[
                    Arg::Decimal(Decimal::from_str("3.14159").unwrap()),
                    Arg::Integer(42),
                    Arg::Str("s".into()),
                    Arg::Integer(255),
                    Arg::Str("ab".into()),
                    Arg::Decimal(Decimal::from_str("12345.678").unwrap()),
                ]
            ),
            " 3.14|42|s|ff|ab  |1.234568e+04"
        );
        assert_eq!(f("%.2f", &[Arg::Double(0.125)]), "0.13");
        assert_eq!(f("%.2f", &[Arg::Double(1.005)]), "1.01");
        assert_eq!(f("%08.3f", &[Arg::Double(-3.5)]), "-003.500");
        assert_eq!(f("%,d", &[Arg::Integer(1234567)]), "1,234,567");
        assert_eq!(f("%+d %(d", &[Arg::Integer(5), Arg::Integer(-5)]), "+5 (5)");
        assert_eq!(
            f("%s %S", &[Arg::Double(1e10), Arg::Str("ab".into())]),
            "1.0E10 AB"
        );
        assert_eq!(f("%s", &[Arg::Double(3.0)]), "3.0");
        assert_eq!(
            f("%2$s %1$s", &[Arg::Str("a".into()), Arg::Str("b".into())]),
            "b a"
        );
        assert_eq!(f("%b %%", &[Arg::Bool(false)]), "false %");
        assert_eq!(f("%c", &[Arg::Integer(65)]), "<error>");
        assert_eq!(
            f(
                "%10.4s|%-6b|",
                &[Arg::Str("abcdef".into()), Arg::Bool(true)]
            ),
            "      abcd|true  |"
        );
        assert_eq!(
            f(
                "%x %X %#x %o",
                &[
                    Arg::Integer(255),
                    Arg::Integer(255),
                    Arg::Integer(255),
                    Arg::Integer(8)
                ]
            ),
            "ff FF 0xff 10"
        );
        assert_eq!(
            f(
                "%.1f|%e",
                &[
                    Arg::Decimal(Decimal::from_str("2.25").unwrap()),
                    Arg::Double(0.0)
                ]
            ),
            "2.3|0.000000e+00"
        );
        assert_eq!(f("%.3s", &[Arg::Str("abcdef".into())]), "abc");
        assert_eq!(f("%g", &[Arg::Double(0.0001234)]), "0.000123400");
        assert_eq!(f("%g", &[Arg::Double(123456789.0)]), "1.23457e+08");
        assert_eq!(f("%d", &[Arg::Double(1.5)]), "<error>");
        assert_eq!(f("%f", &[Arg::Integer(1)]), "<error>");
        assert_eq!(f("%s %s", &[Arg::Integer(1)]), "<error>");
        let dt = DateTime::from_str("2020-01-31T10:05:06.789Z").unwrap();
        assert_eq!(
            f("%tF %<tT %<tB %<tA %<tL", &[Arg::Date(dt)]),
            "2020-01-31 10:05:06 January Friday 789"
        );
    }

    fn n(picture: &str, v: FormatValue) -> String {
        format_number(v, picture, None).unwrap_or_else(|_| "<error>".into())
    }

    /// Java 21's `DecimalFormat` with `Locale.ROOT` gives the same strings.
    #[test]
    fn format_number_matches_java() {
        use FormatValue::*;
        assert_eq!(n("#,##0.00", Double(1234.5678)), "1,234.57");
        assert_eq!(n("0.00", Double(0.125)), "0.12");
        assert_eq!(n("0.00", Double(0.375)), "0.38");
        assert_eq!(n("#.##", Double(0.5)), "0.5");
        assert_eq!(n(".##", Double(0.5)), ".5");
        assert_eq!(n("#.00", Double(0.5)), ".50");
        assert_eq!(n("0.0", Double(f64::INFINITY * 2.0)), "\u{221e}");
        let de = format_number(Double(1234.5), "#,##0.00", Some("de"));
        assert_eq!(de.as_deref(), Ok("1.234,50"));
        let sv = format_number(Double(-12345.5), "#,##0.00", Some("sv"));
        assert_eq!(sv.as_deref(), Ok("\u{2212}12\u{a0}345,50"));
        assert_eq!(n("000", Integer(7)), "007");
        assert_eq!(n("#,###", Integer(1234567)), "1,234,567");
        assert_eq!(n("0.#", Double(-2.0)), "-2");
        assert_eq!(n("0.0%", Double(0.256)), "25.6%");
        assert_eq!(n("$#,##0.00;($#,##0.00)", Double(-1234.5)), "($1,234.50)");
        assert_eq!(n("0.###E0", Double(1234.0)), "1.234E3");
        assert_eq!(n("00.###E0", Double(0.00123)), "12.3E-4");
        assert_eq!(n("##0.#####E0", Double(12345.0)), "12.345E3");
        assert_eq!(n("'#'0", Integer(5)), "#5");
    }
}
