//! CSVW datatypes: the built-in names, their `format` annotations and constraints, and
//! the parsing of one cell value into the lexical form of its literal.

use oxrdf::{Literal, NamedNode};
use oxsdatatypes::{Double, Float};
use serde_json::Value as J;

const XSD: &str = "http://www.w3.org/2001/XMLSchema#";
const RDF: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#";
const CSVW: &str = "http://www.w3.org/ns/csvw#";

/// How a base datatype parses.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// `xsd:string`: kept as is, and the only type that takes `lang`
    String,
    /// `normalizedString`: line breaks become spaces, not trimmed
    Normalized,
    Integer,
    Decimal,
    Double,
    Float,
    Boolean,
    Date,
    DateTime,
    DateTimeStamp,
    Time,
    /// `json`, `xml`, `html`: kept as is
    Markup,
    /// `anyAtomicType`: kept as is, written as `xsd:string`
    Any,
    /// every other XSD type: trimmed and checked against its lexical space
    Other,
}

/// A parsed datatype description.
#[derive(Clone, Debug)]
pub struct Datatype {
    /// the built-in name of the base, e.g. `integer`
    pub name: &'static str,
    pub kind: Kind,
    /// the literal datatype: the base's IRI, or the description's `@id`
    pub iri: NamedNode,
    format: Option<Format>,
    length: Option<usize>,
    min_length: Option<usize>,
    max_length: Option<usize>,
    lower: Option<(f64, bool)>,
    upper: Option<(f64, bool)>,
}

#[derive(Clone, Debug)]
enum Format {
    Regex(regex::Regex),
    Boolean(String, String),
    Number { decimal: char, group: Option<char> },
    Date(Vec<Tok>),
}

/// The built-in datatypes: name, IRI (`x:` is XSD) and kind.
const BUILTINS: &[(&str, &str, Kind)] = &[
    ("anyAtomicType", "x:string", Kind::Any),
    ("any", "x:string", Kind::Any),
    ("anyURI", "x:anyURI", Kind::Other),
    ("base64Binary", "x:base64Binary", Kind::Other),
    ("binary", "x:base64Binary", Kind::Other),
    ("boolean", "x:boolean", Kind::Boolean),
    ("date", "x:date", Kind::Date),
    ("dateTime", "x:dateTime", Kind::DateTime),
    ("datetime", "x:dateTime", Kind::DateTime),
    ("dateTimeStamp", "x:dateTimeStamp", Kind::DateTimeStamp),
    ("time", "x:time", Kind::Time),
    ("decimal", "x:decimal", Kind::Decimal),
    ("integer", "x:integer", Kind::Integer),
    ("long", "x:long", Kind::Integer),
    ("int", "x:int", Kind::Integer),
    ("short", "x:short", Kind::Integer),
    ("byte", "x:byte", Kind::Integer),
    ("nonNegativeInteger", "x:nonNegativeInteger", Kind::Integer),
    ("positiveInteger", "x:positiveInteger", Kind::Integer),
    ("nonPositiveInteger", "x:nonPositiveInteger", Kind::Integer),
    ("negativeInteger", "x:negativeInteger", Kind::Integer),
    ("unsignedLong", "x:unsignedLong", Kind::Integer),
    ("unsignedInt", "x:unsignedInt", Kind::Integer),
    ("unsignedShort", "x:unsignedShort", Kind::Integer),
    ("unsignedByte", "x:unsignedByte", Kind::Integer),
    ("double", "x:double", Kind::Double),
    ("number", "x:double", Kind::Double),
    ("float", "x:float", Kind::Float),
    ("duration", "x:duration", Kind::Other),
    ("dayTimeDuration", "x:dayTimeDuration", Kind::Other),
    ("yearMonthDuration", "x:yearMonthDuration", Kind::Other),
    ("gDay", "x:gDay", Kind::Other),
    ("gMonth", "x:gMonth", Kind::Other),
    ("gMonthDay", "x:gMonthDay", Kind::Other),
    ("gYear", "x:gYear", Kind::Other),
    ("gYearMonth", "x:gYearMonth", Kind::Other),
    ("hexBinary", "x:hexBinary", Kind::Other),
    ("QName", "x:QName", Kind::Other),
    ("string", "x:string", Kind::String),
    ("normalizedString", "x:normalizedString", Kind::Normalized),
    ("token", "x:token", Kind::Other),
    ("language", "x:language", Kind::Other),
    ("Name", "x:Name", Kind::Other),
    ("NMTOKEN", "x:NMTOKEN", Kind::Other),
    ("json", "csvw:JSON", Kind::Markup),
    ("xml", "rdf:XMLLiteral", Kind::Markup),
    ("html", "rdf:HTML", Kind::Markup),
];

fn builtin(name: &str) -> Option<(&'static str, NamedNode, Kind)> {
    let &(n, iri, kind) = BUILTINS.iter().find(|(n, _, _)| *n == name)?;
    let iri = if let Some(l) = iri.strip_prefix("x:") {
        format!("{XSD}{l}")
    } else if let Some(l) = iri.strip_prefix("rdf:") {
        format!("{RDF}{l}")
    } else {
        format!("{CSVW}{}", &iri[5..])
    };
    Some((n, NamedNode::new_unchecked(iri), kind))
}

impl Default for Datatype {
    fn default() -> Datatype {
        Datatype::named("string").expect("a built-in")
    }
}

impl Datatype {
    /// A built-in datatype without format or constraints.
    pub fn named(name: &str) -> Result<Datatype, String> {
        let (name, iri, kind) = builtin(name).ok_or_else(|| {
            format!("unknown datatype {name:?}: use a CSVW built-in name such as \"integer\" or a description with \"base\"")
        })?;
        Ok(Datatype {
            name,
            kind,
            iri,
            format: None,
            length: None,
            min_length: None,
            max_length: None,
            lower: None,
            upper: None,
        })
    }

    /// A datatype from a metadata value: a built-in name or a description object.
    /// `warn` receives the parts that are accepted and not checked.
    pub fn from_json(v: &J, warn: &mut dyn FnMut(String)) -> Result<Datatype, String> {
        let o = match v {
            J::String(s) => return Datatype::named(s),
            J::Object(o) => o,
            _ => return Err("datatype must be a string or an object".into()),
        };
        let base = match o.get("base") {
            None => "string",
            Some(J::String(s)) => s.as_str(),
            Some(_) => return Err("datatype base must be a string".into()),
        };
        let mut dt = Datatype::named(base)?;
        if let Some(id) = o.get("@id") {
            let id = id.as_str().ok_or("datatype @id must be a string")?;
            dt.iri = NamedNode::new(id).map_err(|e| format!("datatype @id {id:?}: {e}"))?;
        }
        if let Some(f) = o.get("format") {
            dt.format = Some(dt.parse_format(f, warn)?);
        }
        let len = |k: &str| -> Result<Option<usize>, String> {
            match o.get(k) {
                None => Ok(None),
                Some(v) => v
                    .as_u64()
                    .map(|n| Some(n as usize))
                    .ok_or_else(|| format!("datatype {k} must be a non-negative integer")),
            }
        };
        dt.length = len("length")?;
        dt.min_length = len("minLength")?;
        dt.max_length = len("maxLength")?;
        let numeric = matches!(
            dt.kind,
            Kind::Integer | Kind::Decimal | Kind::Double | Kind::Float
        );
        for (k, upper, inclusive) in [
            ("minimum", false, true),
            ("minInclusive", false, true),
            ("minExclusive", false, false),
            ("maximum", true, true),
            ("maxInclusive", true, true),
            ("maxExclusive", true, false),
        ] {
            let Some(v) = o.get(k) else { continue };
            if !numeric {
                return Err(format!(
                    "datatype {k} on {:?} is not supported: Sparkles checks bounds on numeric types only",
                    dt.name
                ));
            }
            let n = match v {
                J::Number(n) => n.as_f64(),
                J::String(s) => s.trim().parse::<f64>().ok(),
                _ => None,
            }
            .ok_or_else(|| format!("datatype {k} must be a number"))?;
            let slot = if upper { &mut dt.upper } else { &mut dt.lower };
            *slot = Some((n, inclusive));
        }
        for k in o.keys() {
            if !matches!(
                k.as_str(),
                "base"
                    | "@id"
                    | "@type"
                    | "format"
                    | "length"
                    | "minLength"
                    | "maxLength"
                    | "minimum"
                    | "minInclusive"
                    | "minExclusive"
                    | "maximum"
                    | "maxInclusive"
                    | "maxExclusive"
            ) {
                warn(format!("datatype property {k:?} is ignored"));
            }
        }
        Ok(dt)
    }

    fn parse_format(&self, f: &J, warn: &mut dyn FnMut(String)) -> Result<Format, String> {
        match self.kind {
            Kind::Integer | Kind::Decimal | Kind::Double | Kind::Float => {
                let (pattern, decimal, group) = match f {
                    J::String(p) => (Some(p.as_str()), None, None),
                    J::Object(o) => {
                        let ch = |k: &str| -> Result<Option<char>, String> {
                            match o.get(k) {
                                None | Some(J::Null) => Ok(None),
                                Some(J::String(s)) if s.chars().count() == 1 => {
                                    Ok(s.chars().next())
                                }
                                Some(_) => Err(format!("format {k} must be one character")),
                            }
                        };
                        (
                            o.get("pattern").and_then(J::as_str),
                            ch("decimalChar")?,
                            ch("groupChar")?,
                        )
                    }
                    _ => return Err("a numeric format must be a string or an object".into()),
                };
                if let Some(p) = pattern {
                    warn(format!("the number pattern {p:?} is not checked"));
                }
                Ok(Format::Number {
                    decimal: decimal.unwrap_or('.'),
                    group,
                })
            }
            Kind::Boolean => {
                let s = f.as_str().ok_or("a boolean format must be a string")?;
                let (t, fl) = s
                    .split_once('|')
                    .ok_or("a boolean format must be two values separated by '|'")?;
                Ok(Format::Boolean(t.to_string(), fl.to_string()))
            }
            Kind::Date | Kind::DateTime | Kind::DateTimeStamp | Kind::Time => {
                let s = f.as_str().ok_or("a date or time format must be a string")?;
                Ok(Format::Date(date_pattern(s, self.kind)?))
            }
            _ => {
                let s = f
                    .as_str()
                    .ok_or("a format must be a regular expression string")?;
                let re = regex::Regex::new(&format!("^(?:{s})$"))
                    .map_err(|e| format!("format {s:?} is not a regular expression: {e}"))?;
                Ok(Format::Regex(re))
            }
        }
    }

    /// Whether line breaks and tabs are kept in the value.
    pub fn keeps_line_breaks(&self) -> bool {
        matches!(self.kind, Kind::String | Kind::Markup | Kind::Any)
    }

    /// Whether leading, trailing and repeated spaces are kept.
    pub fn keeps_spaces(&self) -> bool {
        self.keeps_line_breaks() || self.kind == Kind::Normalized
    }

    /// Parse one value (already normalized and not null) into the lexical form of its
    /// literal, checking the format and constraints.
    pub fn parse(&self, v: &str) -> Result<String, String> {
        let n = v.chars().count();
        if let Some(l) = self.length
            && n != l
        {
            return Err(format!("{v:?} has {n} characters, not {l}"));
        }
        if let Some(l) = self.min_length
            && n < l
        {
            return Err(format!("{v:?} is shorter than {l} characters"));
        }
        if let Some(l) = self.max_length
            && n > l
        {
            return Err(format!("{v:?} is longer than {l} characters"));
        }
        let lex = match self.kind {
            Kind::Integer | Kind::Decimal | Kind::Double | Kind::Float => {
                let (decimal, group) = match &self.format {
                    Some(Format::Number { decimal, group }) => (*decimal, *group),
                    _ => ('.', None),
                };
                let lex = number(v, self.kind, decimal, group)
                    .ok_or_else(|| format!("{v:?} is not a valid {}", self.name))?;
                self.check_bounds(&lex, v)?;
                lex
            }
            Kind::Boolean => match &self.format {
                Some(Format::Boolean(t, f)) if v == t => "true".into(),
                Some(Format::Boolean(t, f)) if v == f => "false".into(),
                Some(Format::Boolean(t, f)) => {
                    return Err(format!("{v:?} is neither {t:?} nor {f:?}"));
                }
                _ => match v {
                    "true" | "1" => "true".into(),
                    "false" | "0" => "false".into(),
                    _ => return Err(format!("{v:?} is not a valid boolean")),
                },
            },
            Kind::Date | Kind::DateTime | Kind::DateTimeStamp | Kind::Time => match &self.format {
                Some(Format::Date(p)) => apply_date_pattern(p, v, self.kind)
                    .ok_or_else(|| format!("{v:?} does not match the {} format", self.name))?,
                _ => v.to_string(),
            },
            _ => {
                if let Some(Format::Regex(re)) = &self.format
                    && !re.is_match(v)
                {
                    return Err(format!("{v:?} does not match the format {}", re.as_str()));
                }
                v.to_string()
            }
        };
        if !matches!(self.kind, Kind::Any | Kind::String) {
            let xsd_iri = builtin(self.name).expect("a built-in").1;
            if !crate::xsd::is_valid(&Literal::new_typed_literal(&lex, xsd_iri)) {
                return Err(format!("{v:?} is not a valid {}", self.name));
            }
        }
        Ok(lex)
    }

    fn check_bounds(&self, lex: &str, v: &str) -> Result<(), String> {
        if self.lower.is_none() && self.upper.is_none() {
            return Ok(());
        }
        let x: f64 = lex.parse().unwrap_or(f64::NAN);
        if let Some((m, inc)) = self.lower
            && !(x > m || (inc && x == m))
        {
            return Err(format!("{v:?} is below the minimum {m}"));
        }
        if let Some((m, inc)) = self.upper
            && !(x < m || (inc && x == m))
        {
            return Err(format!("{v:?} is above the maximum {m}"));
        }
        Ok(())
    }

    /// The literal of a parsed value, tagged with `lang` when the type is `string`.
    pub fn literal(&self, lex: String, lang: Option<&str>) -> Literal {
        match (self.kind, lang) {
            (Kind::String, Some(l))
                if self.name == "string" && self.iri.as_str().starts_with(XSD) =>
            {
                Literal::new_language_tagged_literal_unchecked(lex, l.to_ascii_lowercase())
            }
            (Kind::String | Kind::Any, _)
                if self.iri.as_str() == "http://www.w3.org/2001/XMLSchema#string" =>
            {
                Literal::new_simple_literal(lex)
            }
            _ => Literal::new_typed_literal(lex, self.iri.clone()),
        }
    }
}

/// The canonical lexical form of a number in CSVW's number syntax: an optional group
/// character, a decimal character, and a `%` or `‰` sign that divides the value.
fn number(v: &str, kind: Kind, decimal: char, group: Option<char>) -> Option<String> {
    if matches!(kind, Kind::Double | Kind::Float) && matches!(v, "NaN" | "INF" | "-INF") {
        return Some(v.to_string());
    }
    let mut scale = 0usize;
    let mut t = v.to_string();
    for (sign, s) in [('%', 2usize), ('‰', 3)] {
        if let Some(x) = t.strip_suffix(sign).or_else(|| t.strip_prefix(sign)) {
            t = x.to_string();
            scale = s;
            break;
        }
    }
    if let Some(g) = group {
        t = t.replace(g, "");
    }
    if decimal != '.' {
        if t.contains('.') {
            return None;
        }
        t = t.replace(decimal, ".");
    }
    match kind {
        Kind::Double | Kind::Float => {
            if t.is_empty()
                || !t
                    .bytes()
                    .all(|b| b.is_ascii_digit() || matches!(b, b'+' | b'-' | b'.' | b'e' | b'E'))
            {
                return None;
            }
            let x: f64 = t.parse().ok()?;
            let x = x / 10f64.powi(scale as i32);
            Some(if kind == Kind::Double {
                Double::from(x).to_string()
            } else {
                Float::from(x as f32).to_string()
            })
        }
        _ => {
            let (neg, digits) = match t.as_bytes().first()? {
                b'-' => (true, &t[1..]),
                b'+' => (false, &t[1..]),
                _ => (false, &t[..]),
            };
            let (int, frac) = digits.split_once('.').unwrap_or((digits, ""));
            if (int.is_empty() && frac.is_empty())
                || !int.bytes().all(|b| b.is_ascii_digit())
                || !frac.bytes().all(|b| b.is_ascii_digit())
                || (kind == Kind::Integer && digits.contains('.') && scale == 0)
            {
                return None;
            }
            // divide by moving the decimal point
            let mut int = int.to_string();
            let mut frac = frac.to_string();
            for _ in 0..scale {
                let c = int.pop().unwrap_or('0');
                frac.insert(0, c);
            }
            let int = int.trim_start_matches('0');
            let frac = frac.trim_end_matches('0');
            if kind == Kind::Integer && !frac.is_empty() {
                return None;
            }
            let int = if int.is_empty() { "0" } else { int };
            let zero = int == "0" && frac.is_empty();
            let mut out = String::new();
            if neg && !zero {
                out.push('-');
            }
            out.push_str(int);
            if !frac.is_empty() {
                out.push('.');
                out.push_str(frac);
            }
            Some(out)
        }
    }
}

/// A token of a CSVW date or time format.
#[derive(Clone, Debug, PartialEq)]
enum Tok {
    Year,
    /// month, day, hour, minute, second: (field, exact two digits)
    Field(Field, bool),
    Frac(usize),
    /// time zone: (`Z` allowed, colon form)
    Zone(bool, Option<bool>),
    Lit(char),
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Field {
    Month,
    Day,
    Hour,
    Minute,
    Second,
}

fn date_pattern(p: &str, kind: Kind) -> Result<Vec<Tok>, String> {
    let chars: Vec<char> = p.chars().collect();
    let mut toks = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        let mut n = 1;
        while i + n < chars.len() && chars[i + n] == c {
            n += 1;
        }
        let bad = || format!("unsupported date or time format {p:?}");
        let tok = match (c, n) {
            ('y', 4) => Tok::Year,
            ('M', 1 | 2) => Tok::Field(Field::Month, n == 2),
            ('d', 1 | 2) => Tok::Field(Field::Day, n == 2),
            ('H', 1 | 2) => Tok::Field(Field::Hour, n == 2),
            ('m', 2) => Tok::Field(Field::Minute, true),
            ('s', 2) => Tok::Field(Field::Second, true),
            ('S', _) => Tok::Frac(n),
            ('X', 1..=3) => Tok::Zone(true, [None, Some(false), Some(true)][n - 1]),
            ('x', 1..=3) => Tok::Zone(false, [None, Some(false), Some(true)][n - 1]),
            ('T', _) => {
                for _ in 0..n {
                    toks.push(Tok::Lit('T'));
                }
                i += n;
                continue;
            }
            (c, _) if c.is_ascii_alphabetic() => return Err(bad()),
            (c, _) => {
                for _ in 0..n {
                    toks.push(Tok::Lit(c));
                }
                i += n;
                continue;
            }
        };
        toks.push(tok);
        i += n;
    }
    let has = |f: Field| {
        toks.iter()
            .any(|t| matches!(t, Tok::Field(g, _) if *g == f))
    };
    let date = toks.contains(&Tok::Year) && has(Field::Month) && has(Field::Day);
    let time = has(Field::Hour) && has(Field::Minute);
    let ok = match kind {
        Kind::Date => date && !time,
        Kind::Time => time && !date,
        _ => date && time,
    };
    if !ok {
        return Err(format!(
            "date or time format {p:?} does not have the fields of a {}",
            match kind {
                Kind::Date => "date",
                Kind::Time => "time",
                _ => "dateTime",
            }
        ));
    }
    Ok(toks)
}

/// The XSD lexical form of `v` read with a date pattern, or `None` when it does not
/// match. The result is checked against the XSD type afterwards.
fn apply_date_pattern(p: &[Tok], v: &str, kind: Kind) -> Option<String> {
    let b = v.as_bytes();
    let mut i = 0;
    let digits = |i: &mut usize, min: usize, max: usize| -> Option<u32> {
        let start = *i;
        while *i < b.len() && *i - start < max && b[*i].is_ascii_digit() {
            *i += 1;
        }
        if *i - start < min {
            return None;
        }
        v[start..*i].parse().ok()
    };
    let (mut y, mut mo, mut d, mut h, mut mi, mut s) = (0, 0, 0, 0, 0, 0);
    let mut frac = String::new();
    let mut zone: Option<String> = None;
    for t in p {
        match t {
            Tok::Year => y = digits(&mut i, 4, 4)?,
            Tok::Field(f, two) => {
                let x = digits(&mut i, if *two { 2 } else { 1 }, 2)?;
                match f {
                    Field::Month => mo = x,
                    Field::Day => d = x,
                    Field::Hour => h = x,
                    Field::Minute => mi = x,
                    Field::Second => s = x,
                }
            }
            Tok::Frac(n) => {
                let start = i;
                digits(&mut i, 1, *n)?;
                frac = v[start..i].to_string();
            }
            Tok::Zone(z, colon) => {
                if *z && b.get(i) == Some(&b'Z') {
                    i += 1;
                    zone = Some("Z".into());
                    continue;
                }
                let sign = *b.get(i)? as char;
                if sign != '+' && sign != '-' {
                    return None;
                }
                i += 1;
                let hh = digits(&mut i, 2, 2)?;
                let mm = match colon {
                    Some(true) => {
                        if b.get(i) != Some(&b':') {
                            return None;
                        }
                        i += 1;
                        digits(&mut i, 2, 2)?
                    }
                    Some(false) => digits(&mut i, 2, 2)?,
                    None if i < b.len() && b[i].is_ascii_digit() => digits(&mut i, 2, 2)?,
                    None => 0,
                };
                zone = Some(format!("{sign}{hh:02}:{mm:02}"));
            }
            Tok::Lit(c) => {
                let mut buf = [0u8; 4];
                let l = c.encode_utf8(&mut buf).as_bytes();
                if !b[i..].starts_with(l) {
                    return None;
                }
                i += l.len();
            }
        }
    }
    if i != b.len() {
        return None;
    }
    let date = format!("{y:04}-{mo:02}-{d:02}");
    let mut time = format!("{h:02}:{mi:02}:{s:02}");
    if !frac.is_empty() {
        time.push('.');
        time.push_str(&frac);
    }
    let zone = zone.unwrap_or_default();
    Some(match kind {
        Kind::Date => format!("{date}{zone}"),
        Kind::Time => format!("{time}{zone}"),
        _ => format!("{date}T{time}{zone}"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn dt(v: J) -> Datatype {
        Datatype::from_json(&v, &mut |_| {}).unwrap()
    }

    #[test]
    fn numbers() {
        let i = dt(json!("integer"));
        assert_eq!(i.parse("007").unwrap(), "7");
        assert_eq!(i.parse("-0").unwrap(), "0");
        assert_eq!(i.parse("+12").unwrap(), "12");
        assert!(i.parse("1.5").is_err());
        assert!(i.parse("1e3").is_err());
        assert!(i.parse("").is_err());
        assert!(dt(json!("byte")).parse("128").is_err());
        let g = dt(json!({"base": "integer", "format": {"groupChar": ","}}));
        assert_eq!(g.parse("1,234,567").unwrap(), "1234567");
        let d = dt(json!({"base": "decimal", "format": {"decimalChar": ",", "groupChar": "."}}));
        assert_eq!(d.parse("1.234,50").unwrap(), "1234.5");
        assert_eq!(dt(json!("decimal")).parse("12%").unwrap(), "0.12");
        assert_eq!(dt(json!("decimal")).parse("5‰").unwrap(), "0.005");
        assert_eq!(dt(json!("integer")).parse("300%").unwrap(), "3");
        assert!(dt(json!("integer")).parse("30%").is_err());
        assert_eq!(dt(json!("double")).parse("INF").unwrap(), "INF");
        assert_eq!(dt(json!("double")).parse("1.5e2").unwrap(), "150");
        assert!(dt(json!("double")).parse("abc").is_err());
        let b = dt(json!({"base": "integer", "minimum": 1, "maxExclusive": 10}));
        assert!(b.parse("0").is_err());
        assert!(b.parse("1").is_ok());
        assert!(b.parse("10").is_err());
    }

    #[test]
    fn booleans_and_strings() {
        assert_eq!(dt(json!("boolean")).parse("1").unwrap(), "true");
        assert!(dt(json!("boolean")).parse("yes").is_err());
        let yn = dt(json!({"base": "boolean", "format": "Y|N"}));
        assert_eq!(yn.parse("N").unwrap(), "false");
        assert!(yn.parse("y").is_err());
        let re = dt(json!({"base": "string", "format": "[A-Z]{2}\\d+"}));
        assert!(re.parse("AB12").is_ok());
        assert!(re.parse("xAB12").is_err());
        let l = dt(json!({"base": "string", "maxLength": 3}));
        assert!(l.parse("abcd").is_err());
        assert!(dt(json!("gYear")).parse("2020").is_ok());
        assert!(dt(json!("gYear")).parse("20x0").is_err());
        assert!(Datatype::from_json(&json!("nope"), &mut |_| {}).is_err());
        assert!(
            Datatype::from_json(
                &json!({"base": "date", "minimum": "2020-01-01"}),
                &mut |_| {}
            )
            .is_err()
        );
    }

    #[test]
    fn dates() {
        let d = dt(json!({"base": "date", "format": "dd/MM/yyyy"}));
        assert_eq!(d.parse("03/04/1990").unwrap(), "1990-04-03");
        assert!(d.parse("31/02/1990").is_err());
        assert!(d.parse("3/4/1990").is_err());
        let d = dt(json!({"base": "date", "format": "M/d/yyyy"}));
        assert_eq!(d.parse("3/14/2001").unwrap(), "2001-03-14");
        let d = dt(json!({"base": "date", "format": "yyyyMMdd"}));
        assert_eq!(d.parse("20010314").unwrap(), "2001-03-14");
        let t = dt(json!({"base": "time", "format": "HH:mm"}));
        assert_eq!(t.parse("09:30").unwrap(), "09:30:00");
        let dtm = dt(json!({"base": "dateTime", "format": "yyyy-MM-ddTHH:mm:ss.SSSXXX"}));
        assert_eq!(
            dtm.parse("2001-03-14T09:30:01.25+01:00").unwrap(),
            "2001-03-14T09:30:01.25+01:00"
        );
        assert_eq!(
            dtm.parse("2001-03-14T09:30:01.2Z").unwrap(),
            "2001-03-14T09:30:01.2Z"
        );
        let dtm = dt(json!({"base": "dateTime", "format": "dd.MM.yyyy HH:mm x"}));
        assert_eq!(
            dtm.parse("14.03.2001 09:30 -05").unwrap(),
            "2001-03-14T09:30:00-05:00"
        );
        assert!(dt(json!("date")).parse("2001-03-14").is_ok());
        assert!(dt(json!("date")).parse("14/03/2001").is_err());
        for bad in ["dd/MM/yy", "yyyy-MM-dd QQ", "HH:mm"] {
            assert!(
                Datatype::from_json(&json!({"base": "date", "format": bad}), &mut |_| {}).is_err(),
                "{bad}"
            );
        }
    }

    #[test]
    fn literals() {
        let s = Datatype::default();
        assert_eq!(s.literal("a".into(), Some("EN")).to_string(), "\"a\"@en");
        assert_eq!(s.literal("a".into(), None).to_string(), "\"a\"");
        let i = dt(json!("integer"));
        assert_eq!(
            i.literal("1".into(), Some("en")).to_string(),
            "\"1\"^^<http://www.w3.org/2001/XMLSchema#integer>"
        );
        let c = dt(json!({"base": "string", "@id": "http://e/code"}));
        assert_eq!(
            c.literal("x".into(), Some("en")).to_string(),
            "\"x\"^^<http://e/code>"
        );
        assert_eq!(
            dt(json!("json")).literal("{}".into(), None).to_string(),
            "\"{}\"^^<http://www.w3.org/ns/csvw#JSON>"
        );
    }
}
