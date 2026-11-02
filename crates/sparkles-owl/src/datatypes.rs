//! The lexical spaces and facet spaces of the OWL 2 datatype map (structural
//! specification §4), which the OWL 2 DL check applies to literals (§5.7), to datatype
//! restrictions (§7.5) and to the datatypes that datatype definitions introduce (§9.4).
//!
//! The lexical spaces are those of XML Schema 1.1 Part 2 for the `xsd:` datatypes, of
//! the `rdf:PlainLiteral` specification and of §4.1 for `owl:real` and `owl:rational`.
//! A lexical form is checked as it is, so a form with surrounding whitespace is outside
//! the lexical space of a numeric datatype, as RDF 1.1 Concepts reads XML Schema.
//!
//! ```
//! use sparkles_owl::datatypes::lexical;
//! let xsd = |l: &str| format!("http://www.w3.org/2001/XMLSchema#{l}");
//! assert_eq!(lexical(&xsd("integer"), "-12"), Some(true));
//! assert_eq!(lexical(&xsd("byte"), "300"), Some(false));
//! assert_eq!(lexical("http://ex.org/dt", "x"), None);
//! ```

use crate::model::*;
use crate::vocab;
use std::str::FromStr;

const XSD: &str = vocab::XSD;

fn local(datatype: &str) -> Option<&str> {
    datatype.strip_prefix(XSD)
}

/// Whether `lex` is in the lexical space of `datatype`: `None` for a datatype outside
/// the OWL 2 datatype map, whose lexical space is not known, and for `rdfs:Literal`.
pub fn lexical(datatype: &str, lex: &str) -> Option<bool> {
    if datatype == vocab::OWL_REAL {
        // §4.1: owl:real provides no lexical forms
        return Some(false);
    }
    if datatype == vocab::OWL_RATIONAL {
        return Some(rational(lex));
    }
    if datatype == vocab::RDF_PLAIN_LITERAL {
        return Some(plain_literal(lex));
    }
    if datatype == vocab::RDF_LANG_STRING {
        return Some(xml_chars(lex));
    }
    if datatype == vocab::RDF_XML_LITERAL {
        return Some(xml_content(lex));
    }
    let l = local(datatype)?;
    Some(match l {
        "string" => xml_chars(lex),
        "normalizedString" => normalized(lex),
        "token" => token(lex),
        "language" => language_tag(lex),
        "Name" => name(lex, true),
        "NCName" => name(lex, false),
        "NMTOKEN" => !lex.is_empty() && lex.chars().all(is_name_char),
        "boolean" => matches!(lex, "true" | "false" | "1" | "0"),
        "decimal" => decimal(lex),
        "integer" | "nonNegativeInteger" | "nonPositiveInteger" | "positiveInteger"
        | "negativeInteger" | "long" | "int" | "short" | "byte" | "unsignedLong"
        | "unsignedInt" | "unsignedShort" | "unsignedByte" => integer_in(l, lex),
        "double" | "float" => floating(lex),
        "hexBinary" => lex.len().is_multiple_of(2) && lex.bytes().all(|b| b.is_ascii_hexdigit()),
        "base64Binary" => base64(lex),
        // XML Schema 1.1: every finite sequence of characters is an anyURI lexical form
        "anyURI" => xml_chars(lex),
        "dateTime" => oxsdatatypes::DateTime::from_str(lex).is_ok(),
        "dateTimeStamp" => {
            oxsdatatypes::DateTime::from_str(lex).is_ok_and(|d| d.timezone_offset().is_some())
        }
        _ => return None,
    })
}

/// The constraining facets of `datatype` that OWL 2 supports (§4.1 to §4.8): `None`
/// for a datatype outside the OWL 2 datatype map, and an empty list for `xsd:boolean`,
/// `rdf:XMLLiteral` and `rdfs:Literal`, which have none.
pub fn facets(datatype: &str) -> Option<&'static [&'static str]> {
    const ORDER: &[&str] = &[
        "http://www.w3.org/2001/XMLSchema#minInclusive",
        "http://www.w3.org/2001/XMLSchema#maxInclusive",
        "http://www.w3.org/2001/XMLSchema#minExclusive",
        "http://www.w3.org/2001/XMLSchema#maxExclusive",
    ];
    const STRING: &[&str] = &[
        "http://www.w3.org/2001/XMLSchema#length",
        "http://www.w3.org/2001/XMLSchema#minLength",
        "http://www.w3.org/2001/XMLSchema#maxLength",
        "http://www.w3.org/2001/XMLSchema#pattern",
    ];
    const PLAIN: &[&str] = &[
        "http://www.w3.org/2001/XMLSchema#length",
        "http://www.w3.org/2001/XMLSchema#minLength",
        "http://www.w3.org/2001/XMLSchema#maxLength",
        "http://www.w3.org/2001/XMLSchema#pattern",
        "http://www.w3.org/1999/02/22-rdf-syntax-ns#langRange",
    ];
    const BINARY: &[&str] = &[
        "http://www.w3.org/2001/XMLSchema#length",
        "http://www.w3.org/2001/XMLSchema#minLength",
        "http://www.w3.org/2001/XMLSchema#maxLength",
    ];
    if datatype == vocab::OWL_REAL || datatype == vocab::OWL_RATIONAL {
        return Some(ORDER);
    }
    if datatype == vocab::RDF_PLAIN_LITERAL || datatype == vocab::RDF_LANG_STRING {
        return Some(PLAIN);
    }
    if datatype == vocab::RDF_XML_LITERAL || datatype == vocab::RDFS_LITERAL {
        return Some(&[]);
    }
    Some(match local(datatype)? {
        "decimal" | "integer" | "nonNegativeInteger" | "nonPositiveInteger" | "positiveInteger"
        | "negativeInteger" | "long" | "int" | "short" | "byte" | "unsignedLong"
        | "unsignedInt" | "unsignedShort" | "unsignedByte" | "double" | "float" | "dateTime"
        | "dateTimeStamp" => ORDER,
        "string" | "normalizedString" | "token" | "language" | "Name" | "NCName" | "NMTOKEN"
        | "anyURI" => STRING,
        "hexBinary" | "base64Binary" => BINARY,
        "boolean" => &[],
        _ => return None,
    })
}

/// Whether `datatype` is a numeric datatype whose value space is in that of `owl:real`.
fn is_real(datatype: &str) -> bool {
    datatype == vocab::OWL_REAL
        || datatype == vocab::OWL_RATIONAL
        || local(datatype).is_some_and(|l| l == "decimal" || integer_bounds(l).is_some())
}

/// Why the constraining value `value` of `facet` is outside the facet space of
/// `datatype` (§7.5), or `None` when the pair is in it. The facet must be one that
/// `datatype` supports, and the value a literal of the datatype the facet ranges over:
/// a number of `owl:real` for the order facets of the numeric datatypes, a value of
/// the datatype itself for the order facets of `xsd:double`, `xsd:float` and the time
/// instants, a nonnegative integer for the length facets, and a string for
/// `xsd:pattern` and `rdf:langRange`. A value whose lexical form is outside its own
/// datatype is reported as a literal, not here.
pub fn facet_problem(datatype: &str, facet: &str, value: &Literal) -> Option<String> {
    let supported = facets(datatype)?;
    if !supported.contains(&facet) {
        return Some(if supported.is_empty() {
            format!(
                "the datatype <{datatype}> supports no constraining facets, so <{facet}> cannot restrict it"
            )
        } else {
            format!("the constraining facet <{facet}> does not apply to the datatype <{datatype}>")
        });
    }
    let vt = value.datatype();
    let facet_local = facet.rsplit(['#', '/']).next().unwrap_or(facet);
    let ok = match facet_local {
        "minInclusive" | "maxInclusive" | "minExclusive" | "maxExclusive" => {
            if is_real(datatype) {
                is_real(vt) && vt != vocab::OWL_REAL
            } else if datatype.ends_with("#dateTime") || datatype.ends_with("#dateTimeStamp") {
                let stamp = datatype.ends_with("#dateTimeStamp");
                (vt.ends_with("#dateTime") || vt.ends_with("#dateTimeStamp"))
                    && local(vt).is_some()
                    && (!stamp
                        || oxsdatatypes::DateTime::from_str(value.value())
                            .is_ok_and(|d| d.timezone_offset().is_some()))
            } else {
                vt == datatype
            }
        }
        "length" | "minLength" | "maxLength" => nonnegative_integer(vt, value.value()),
        "pattern" => string_value(vt, value.value()).is_some_and(pattern),
        "langRange" => string_value(vt, value.value()).is_some_and(language_range),
        _ => true,
    };
    (!ok).then(|| {
        format!(
            "the value {} of <{facet}> is outside the facet space of <{datatype}>",
            crate::ofn::Renderer::new(Vec::<(String, String)>::new()).literal(value)
        )
    })
}

/// Whether the literal `lex`^^`datatype` is a nonnegative integer: a value of an
/// integer datatype, an `xsd:decimal` without a fractional part or an `owl:rational`
/// whose denominator divides its numerator (§4.1 nests these value spaces in
/// `owl:real`).
fn nonnegative_integer(datatype: &str, lex: &str) -> bool {
    if lexical(datatype, lex) != Some(true) {
        return false;
    }
    // a sign is a minus only on a zero
    let unsigned = |s: &str| {
        s.strip_prefix('-')
            .is_none_or(|d| d.bytes().all(|b| b == b'0' || b == b'.'))
    };
    if datatype == vocab::OWL_RATIONAL {
        let Some((n, d)) = lex.split_once('/') else {
            return false;
        };
        let digits = |s: &str| s.trim_start_matches(['+', '-']).as_bytes().to_vec();
        return divides(&digits(d), &digits(n)) && unsigned(n);
    }
    match local(datatype) {
        Some("decimal") => {
            let (whole, frac) = lex.split_once('.').unwrap_or((lex, ""));
            frac.bytes().all(|b| b == b'0') && unsigned(whole)
        }
        Some(l) if integer_bounds(l).is_some() => unsigned(lex),
        _ => false,
    }
}

/// The string that the literal `lex`^^`datatype` denotes, when its value is a string:
/// a literal of `xsd:string` or a datatype derived from it, or an `rdf:PlainLiteral`
/// without a language tag.
fn string_value<'a>(datatype: &str, lex: &'a str) -> Option<&'a str> {
    if lexical(datatype, lex) != Some(true) {
        return None;
    }
    if datatype == vocab::RDF_PLAIN_LITERAL {
        return lex.strip_suffix('@');
    }
    matches!(
        local(datatype)?,
        "string" | "normalizedString" | "token" | "language" | "Name" | "NCName" | "NMTOKEN"
    )
    .then_some(lex)
}

/// Whether the decimal digits `d`, not all zero, divide the decimal digits `n`, of any
/// length, by schoolbook long division.
fn divides(d: &[u8], n: &[u8]) -> bool {
    let trim = |x: &[u8]| -> Vec<u8> {
        let start = x.iter().position(|&b| b != b'0').unwrap_or(x.len());
        x[start..].to_vec()
    };
    let d = trim(d);
    if d.is_empty() {
        return false;
    }
    // `a >= b` for digit strings without leading zeros
    let ge = |a: &[u8], b: &[u8]| a.len() > b.len() || a.len() == b.len() && a >= b;
    let mut rem: Vec<u8> = Vec::with_capacity(d.len() + 1);
    for &digit in n {
        rem.push(digit);
        rem = trim(&rem);
        while ge(&rem, &d) {
            // rem -= d
            let mut borrow = 0i8;
            let offset = rem.len() - d.len();
            for i in (0..rem.len()).rev() {
                let sub = if i >= offset {
                    (d[i - offset] - b'0') as i8
                } else {
                    0
                };
                let mut v = (rem[i] - b'0') as i8 - sub - borrow;
                borrow = i8::from(v < 0);
                if v < 0 {
                    v += 10;
                }
                rem[i] = b'0' + v as u8;
            }
            rem = trim(&rem);
        }
    }
    rem.is_empty()
}

/// Whether `p` is a regular expression of XML Schema 1.1 Part 2, Appendix G.
fn pattern(p: &str) -> bool {
    let mut r = Regex {
        c: p.chars().collect(),
        i: 0,
    };
    r.reg_exp() && r.i == r.c.len()
}

/// A recursive-descent recognizer of the XML Schema regular expression grammar.
struct Regex {
    c: Vec<char>,
    i: usize,
}

impl Regex {
    fn peek(&self) -> Option<char> {
        self.c.get(self.i).copied()
    }

    fn eat(&mut self, c: char) -> bool {
        let hit = self.peek() == Some(c);
        self.i += usize::from(hit);
        hit
    }

    /// `regExp ::= branch ( '|' branch )*`
    fn reg_exp(&mut self) -> bool {
        loop {
            if !self.branch() {
                return false;
            }
            if !self.eat('|') {
                return true;
            }
        }
    }

    /// `branch ::= piece*`
    fn branch(&mut self) -> bool {
        while let Some(c) = self.peek() {
            if c == '|' || c == ')' {
                return true;
            }
            if !self.atom() || !self.quantifier() {
                return false;
            }
        }
        true
    }

    /// `quantifier ::= [?*+] | '{' quantity '}'`, optional
    fn quantifier(&mut self) -> bool {
        if self.eat('?') || self.eat('*') || self.eat('+') {
            return true;
        }
        if !self.eat('{') {
            return true;
        }
        let Some(min) = self.number() else {
            return false;
        };
        if self.eat(',') {
            if let Some(max) = self.number()
                && max < min
            {
                return false;
            }
        }
        self.eat('}')
    }

    fn number(&mut self) -> Option<u128> {
        let start = self.i;
        while self.peek().is_some_and(|c| c.is_ascii_digit()) {
            self.i += 1;
        }
        let digits: String = self.c[start..self.i].iter().collect();
        // a quantity too large for any machine is still a quantity
        (!digits.is_empty()).then(|| digits.parse().unwrap_or(u128::MAX))
    }

    /// `atom ::= NormalChar | charClass | '(' regExp ')'`
    fn atom(&mut self) -> bool {
        match self.peek() {
            Some('(') => {
                self.i += 1;
                self.reg_exp() && self.eat(')')
            }
            Some('[') => self.char_class_expr(),
            Some('\\') => self.escape().is_some(),
            Some('.') => {
                self.i += 1;
                true
            }
            Some('?' | '*' | '+' | '{' | '}' | ')' | ']' | '|') | None => false,
            Some(_) => {
                self.i += 1;
                true
            }
        }
    }

    /// An escape after its backslash's position: `Some(Some(c))` for a single-character
    /// escape of `c`, `Some(None)` for a multi-character or category escape.
    fn escape(&mut self) -> Option<Option<char>> {
        self.i += 1;
        let c = self.peek()?;
        self.i += 1;
        match c {
            'n' => Some(Some('\n')),
            'r' => Some(Some('\r')),
            't' => Some(Some('\t')),
            '\\' | '|' | '.' | '?' | '*' | '+' | '(' | ')' | '{' | '}' | '-' | '[' | ']' | '^' => {
                Some(Some(c))
            }
            's' | 'S' | 'i' | 'I' | 'c' | 'C' | 'd' | 'D' | 'w' | 'W' => Some(None),
            'p' | 'P' => {
                if !self.eat('{') {
                    return None;
                }
                let start = self.i;
                while self.peek().is_some_and(|c| c != '}') {
                    self.i += 1;
                }
                let prop: String = self.c[start..self.i].iter().collect();
                if !self.eat('}') {
                    return None;
                }
                char_prop(&prop).then_some(None)
            }
            _ => None,
        }
    }

    /// `charClassExpr ::= '[' charGroup ']'`, with
    /// `charGroup ::= ( posCharGroup | negCharGroup ) ( '-' charClassExpr )?`
    fn char_class_expr(&mut self) -> bool {
        if !self.eat('[') {
            return false;
        }
        self.eat('^');
        let mut parts = 0;
        loop {
            match self.peek() {
                None => return false,
                Some(']') => {
                    self.i += 1;
                    return parts > 0;
                }
                Some('-') if self.c.get(self.i + 1) == Some(&'[') => {
                    self.i += 1;
                    return parts > 0 && self.char_class_expr() && self.eat(']');
                }
                Some('[') => return false,
                Some(_) => {
                    let Some(start) = self.single_or_class() else {
                        return false;
                    };
                    parts += 1;
                    // a range, unless the hyphen ends the group or starts a subtraction
                    if self.peek() == Some('-')
                        && !matches!(self.c.get(self.i + 1), Some(']' | '[') | None)
                    {
                        self.i += 1;
                        let (Some(lo), Some(Some(hi))) = (start, self.single_or_class()) else {
                            return false;
                        };
                        if hi < lo {
                            return false;
                        }
                    }
                }
            }
        }
    }

    /// One part of a group: `Some(Some(c))` for a single character, `Some(None)` for
    /// an escape that stands for several.
    fn single_or_class(&mut self) -> Option<Option<char>> {
        match self.peek()? {
            '\\' => self.escape(),
            '[' | ']' => None,
            c => {
                self.i += 1;
                Some(Some(c))
            }
        }
    }
}

/// `charProp ::= IsCategory | IsBlock`
fn char_prop(p: &str) -> bool {
    const CATEGORIES: [&str; 37] = [
        "L", "Lu", "Ll", "Lt", "Lm", "Lo", "M", "Mn", "Mc", "Me", "N", "Nd", "Nl", "No", "P", "Pc",
        "Pd", "Ps", "Pe", "Pi", "Pf", "Po", "Z", "Zs", "Zl", "Zp", "S", "Sm", "Sc", "Sk", "So",
        "C", "Cc", "Cf", "Co", "Cn", "Cs",
    ];
    CATEGORIES.contains(&p)
        || p.strip_prefix("Is").is_some_and(|b| {
            !b.is_empty() && b.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
        })
}

/// Whether `r` is a language range of RFC 4647, in the extended form that contains the
/// basic one: `*` or up to eight letters, then subtags of up to eight letters and
/// digits or `*`, each after a hyphen.
fn language_range(r: &str) -> bool {
    let mut subtags = r.split('-');
    let first = subtags.next().unwrap_or("");
    let tag = |s: &str, alnum: bool| {
        s == "*"
            || (1..=8).contains(&s.len())
                && s.bytes()
                    .all(|b| b.is_ascii_alphabetic() || alnum && b.is_ascii_digit())
    };
    tag(first, false) && subtags.all(|s| tag(s, true))
}

// ------------------------------------------------------------- lexical forms ------

/// The characters of XML 1.0 (`Char`), which every string datatype's values consist of.
fn is_xml_char(c: char) -> bool {
    matches!(c, '\u{9}' | '\u{A}' | '\u{D}' | '\u{20}'..='\u{D7FF}' | '\u{E000}'..='\u{FFFD}' | '\u{10000}'..='\u{10FFFF}')
}

fn xml_chars(s: &str) -> bool {
    s.chars().all(is_xml_char)
}

fn normalized(s: &str) -> bool {
    xml_chars(s) && !s.contains(['\r', '\n', '\t'])
}

fn token(s: &str) -> bool {
    normalized(s) && !s.starts_with(' ') && !s.ends_with(' ') && !s.contains("  ")
}

/// The pattern of `xsd:language`: `[a-zA-Z]{1,8}(-[a-zA-Z0-9]{1,8})*`.
fn language_tag(s: &str) -> bool {
    let mut parts = s.split('-');
    let first = parts.next().unwrap_or("");
    (1..=8).contains(&first.len())
        && first.bytes().all(|b| b.is_ascii_alphabetic())
        && parts.all(|p| (1..=8).contains(&p.len()) && p.bytes().all(|b| b.is_ascii_alphanumeric()))
}

/// `NameStartChar` of XML 1.0 fifth edition.
fn is_name_start(c: char) -> bool {
    matches!(c, ':' | 'A'..='Z' | '_' | 'a'..='z' | '\u{C0}'..='\u{D6}' | '\u{D8}'..='\u{F6}'
        | '\u{F8}'..='\u{2FF}' | '\u{370}'..='\u{37D}' | '\u{37F}'..='\u{1FFF}'
        | '\u{200C}'..='\u{200D}' | '\u{2070}'..='\u{218F}' | '\u{2C00}'..='\u{2FEF}'
        | '\u{3001}'..='\u{D7FF}' | '\u{F900}'..='\u{FDCF}' | '\u{FDF0}'..='\u{FFFD}'
        | '\u{10000}'..='\u{EFFFF}')
}

/// `NameChar` of XML 1.0 fifth edition.
fn is_name_char(c: char) -> bool {
    is_name_start(c)
        || matches!(c, '-' | '.' | '0'..='9' | '\u{B7}' | '\u{300}'..='\u{36F}' | '\u{203F}'..='\u{2040}')
}

/// An XML `Name`, or with `colon` false an `NCName`.
fn name(s: &str, colon: bool) -> bool {
    let mut cs = s.chars();
    cs.next()
        .is_some_and(|c| is_name_start(c) && (colon || c != ':'))
        && cs.all(|c| is_name_char(c) && (colon || c != ':'))
}

fn digits(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit())
}

fn unsigned_part(s: &str) -> &str {
    s.strip_prefix(['+', '-']).unwrap_or(s)
}

/// `(\+|-)?([0-9]+(\.[0-9]*)?|\.[0-9]+)`
fn decimal(s: &str) -> bool {
    let d = unsigned_part(s);
    match d.split_once('.') {
        None => digits(d),
        Some((i, f)) => (digits(i) && (f.is_empty() || digits(f))) || (i.is_empty() && digits(f)),
    }
}

/// The value range of an integer datatype, by local name; `None` bounds are unbounded.
fn integer_bounds(l: &str) -> Option<(Option<i128>, Option<i128>)> {
    Some(match l {
        "integer" => (None, None),
        "nonNegativeInteger" => (Some(0), None),
        "nonPositiveInteger" => (None, Some(0)),
        "positiveInteger" => (Some(1), None),
        "negativeInteger" => (None, Some(-1)),
        "long" => (Some(i64::MIN as i128), Some(i64::MAX as i128)),
        "int" => (Some(i32::MIN as i128), Some(i32::MAX as i128)),
        "short" => (Some(i16::MIN as i128), Some(i16::MAX as i128)),
        "byte" => (Some(i8::MIN as i128), Some(i8::MAX as i128)),
        "unsignedLong" => (Some(0), Some(u64::MAX as i128)),
        "unsignedInt" => (Some(0), Some(u32::MAX as i128)),
        "unsignedShort" => (Some(0), Some(u16::MAX as i128)),
        "unsignedByte" => (Some(0), Some(u8::MAX as i128)),
        _ => return None,
    })
}

/// `(\+|-)?[0-9]+` within the range of the integer datatype `l`.
fn integer_in(l: &str, s: &str) -> bool {
    let Some((min, max)) = integer_bounds(l) else {
        return false;
    };
    if !digits(unsigned_part(s)) {
        return false;
    }
    let negative = s.starts_with('-');
    let magnitude = unsigned_part(s).trim_start_matches('0');
    match magnitude.parse::<i128>() {
        Ok(m) => {
            let v = if negative { -m } else { m };
            min.is_none_or(|x| v >= x) && max.is_none_or(|x| v <= x)
        }
        // "0…0" parses as empty: zero
        Err(_) if magnitude.is_empty() => min.is_none_or(|x| x <= 0) && max.is_none_or(|x| x >= 0),
        // beyond i128: only an unbounded side takes it
        Err(_) => {
            if negative {
                min.is_none()
            } else {
                max.is_none()
            }
        }
    }
}

/// `(\+|-)?([0-9]+(\.[0-9]*)?|\.[0-9]+)([Ee](\+|-)?[0-9]+)?|(\+|-)?INF|NaN`
fn floating(s: &str) -> bool {
    if matches!(s, "INF" | "+INF" | "-INF" | "NaN") {
        return true;
    }
    let (mantissa, exponent) = match s.find(['e', 'E']) {
        Some(i) => (&s[..i], Some(&s[i + 1..])),
        None => (s, None),
    };
    decimal(mantissa) && exponent.is_none_or(|e| digits(unsigned_part(e)))
}

/// `owl:rational` (§4.1): an integer numerator, `/`, and a positive integer
/// denominator without a sign.
fn rational(s: &str) -> bool {
    let Some((n, d)) = s.split_once('/') else {
        return false;
    };
    digits(unsigned_part(n)) && digits(d) && d.bytes().any(|b| b != b'0')
}

/// `rdf:PlainLiteral`: a string, `@`, and an empty or well-formed language tag.
fn plain_literal(s: &str) -> bool {
    let Some((text, lang)) = s.rsplit_once('@') else {
        return false;
    };
    xml_chars(text) && (lang.is_empty() || language_tag(lang))
}

/// The base64 grammar of XML Schema 1.1: groups of four characters of the alphabet,
/// the last possibly padded with `=`, with single spaces allowed between characters.
fn base64(s: &str) -> bool {
    if s.starts_with(' ') || s.ends_with(' ') || s.contains("  ") {
        return false;
    }
    let cs: Vec<u8> = s.bytes().filter(|b| *b != b' ').collect();
    if !cs.len().is_multiple_of(4) {
        return false;
    }
    let alpha = |b: u8| b.is_ascii_alphanumeric() || b == b'+' || b == b'/';
    let pad = cs.iter().rev().take_while(|b| **b == b'=').count();
    if pad > 2 || !cs[..cs.len() - pad].iter().all(|b| alpha(*b)) {
        return false;
    }
    // the character before the padding carries no bits the padding drops
    match (pad, cs.len().checked_sub(pad + 1).map(|i| cs[i])) {
        (1, Some(b)) => b"AEIMQUYcgkosw048".contains(&b),
        (2, Some(b)) => b"AQgw".contains(&b),
        _ => true,
    }
}

/// `rdf:XMLLiteral`: well-balanced XML content, as RDF 1.1 Concepts defines its
/// lexical space. The check covers balanced and properly nested element tags, quoted
/// attribute values, references, comments, processing instructions and CDATA sections,
/// and characters of XML 1.0. It does not resolve namespace prefixes.
fn xml_content(s: &str) -> bool {
    let mut open: Vec<&str> = Vec::new();
    let mut rest = s;
    while let Some(i) = rest.find(['<', '&']) {
        if !xml_text(&rest[..i]) {
            return false;
        }
        rest = &rest[i..];
        if rest.starts_with('&') {
            let Some(end) = rest.find(';') else {
                return false;
            };
            if !reference(&rest[1..end]) {
                return false;
            }
            rest = &rest[end + 1..];
        } else if let Some(r) = rest.strip_prefix("<!--") {
            let Some(end) = r.find("-->") else {
                return false;
            };
            if r[..end].contains("--") || !xml_chars(&r[..end]) {
                return false;
            }
            rest = &r[end + 3..];
        } else if let Some(r) = rest.strip_prefix("<![CDATA[") {
            let Some(end) = r.find("]]>") else {
                return false;
            };
            if !xml_chars(&r[..end]) {
                return false;
            }
            rest = &r[end + 3..];
        } else if let Some(r) = rest.strip_prefix("<?") {
            let Some(end) = r.find("?>") else {
                return false;
            };
            let target = r[..end].split([' ', '\t', '\r', '\n']).next().unwrap_or("");
            if !name(target, false) || target.eq_ignore_ascii_case("xml") || !xml_chars(&r[..end]) {
                return false;
            }
            rest = &r[end + 2..];
        } else if let Some(r) = rest.strip_prefix("</") {
            let Some(end) = r.find('>') else {
                return false;
            };
            let n = r[..end].trim_end_matches([' ', '\t', '\r', '\n']);
            if open.pop() != Some(n) {
                return false;
            }
            rest = &r[end + 1..];
        } else {
            let r = &rest[1..];
            let Some((n, after, empty)) = start_tag(r) else {
                return false;
            };
            if !empty {
                open.push(n);
            }
            rest = after;
        }
    }
    open.is_empty() && xml_text(rest)
}

/// Character data: XML characters without `]]>`.
fn xml_text(s: &str) -> bool {
    xml_chars(s) && !s.contains("]]>")
}

/// The body of a reference `&…;`: a character reference or one of the five entities
/// XML predefines, since a fragment has no document type to declare others.
fn reference(body: &str) -> bool {
    if let Some(h) = body.strip_prefix("#x") {
        return !h.is_empty()
            && u32::from_str_radix(h, 16)
                .ok()
                .and_then(char::from_u32)
                .is_some_and(is_xml_char);
    }
    if let Some(d) = body.strip_prefix('#') {
        return digits(d)
            && d.parse::<u32>()
                .ok()
                .and_then(char::from_u32)
                .is_some_and(is_xml_char);
    }
    matches!(body, "amp" | "lt" | "gt" | "quot" | "apos")
}

/// A start tag after its `<`: its name, the text after its `>` and whether it is an
/// empty-element tag.
fn start_tag(r: &str) -> Option<(&str, &str, bool)> {
    let end_name = r.find(|c: char| !is_name_char(c)).unwrap_or(r.len());
    let n = &r[..end_name];
    if !name(n, true) {
        return None;
    }
    let mut rest = &r[end_name..];
    let mut seen: Vec<&str> = Vec::new();
    loop {
        let trimmed = rest.trim_start_matches([' ', '\t', '\r', '\n']);
        let spaced = trimmed.len() < rest.len();
        rest = trimmed;
        if let Some(after) = rest.strip_prefix("/>") {
            return Some((n, after, true));
        }
        if let Some(after) = rest.strip_prefix('>') {
            return Some((n, after, false));
        }
        if !spaced {
            return None;
        }
        let end_attr = rest.find(|c: char| !is_name_char(c)).unwrap_or(rest.len());
        let attr = &rest[..end_attr];
        if !name(attr, true) || seen.contains(&attr) {
            return None;
        }
        seen.push(attr);
        rest = rest[end_attr..].trim_start_matches([' ', '\t', '\r', '\n']);
        rest = rest
            .strip_prefix('=')?
            .trim_start_matches([' ', '\t', '\r', '\n']);
        let q = rest.chars().next().filter(|c| *c == '"' || *c == '\'')?;
        let body = &rest[1..];
        let close = body.find(q)?;
        let value = &body[..close];
        if value.contains('<') || !xml_chars(value) {
            return None;
        }
        let mut v = value;
        while let Some(i) = v.find('&') {
            let end = v[i..].find(';')? + i;
            if !reference(&v[i + 1..end]) {
                return None;
            }
            v = &v[end + 1..];
        }
        rest = &body[close + 1..];
    }
}

// ------------------------------------------------------------ the literals ------

/// Every literal of an axiom with whether it is the value of an annotation, and every
/// datatype restriction of its data ranges.
pub(crate) struct Literals<'a> {
    pub literals: Vec<(&'a Literal, bool)>,
    pub restrictions: Vec<(&'a Datatype, &'a FacetRestriction)>,
}

impl<'a> Literals<'a> {
    pub(crate) fn of(ax: &'a Axiom) -> Literals<'a> {
        let mut out = Literals {
            literals: Vec::new(),
            restrictions: Vec::new(),
        };
        out.kind(&ax.kind);
        for a in &ax.annotations {
            out.annotation(a);
        }
        out
    }

    pub(crate) fn of_annotations(
        annotations: impl IntoIterator<Item = &'a Annotation>,
    ) -> Literals<'a> {
        let mut out = Literals {
            literals: Vec::new(),
            restrictions: Vec::new(),
        };
        for a in annotations {
            out.annotation(a);
        }
        out
    }

    fn annotation(&mut self, a: &'a Annotation) {
        if let AnnotationValue::Literal(l) = &a.value {
            self.literals.push((l, true));
        }
        for x in &a.annotations {
            self.annotation(x);
        }
    }

    fn range(&mut self, r: &'a DataRange) {
        match r {
            DataRange::Datatype(_) => {}
            DataRange::IntersectionOf(rs) | DataRange::UnionOf(rs) => {
                rs.iter().for_each(|r| self.range(r))
            }
            DataRange::ComplementOf(r) => self.range(r),
            DataRange::OneOf(ls) => self.literals.extend(ls.iter().map(|l| (l, false))),
            DataRange::DatatypeRestriction(d, fs) => {
                for f in fs {
                    self.literals.push((&f.value, false));
                    self.restrictions.push((d, f));
                }
            }
        }
    }

    fn class(&mut self, c: &'a ClassExpression) {
        use ClassExpression as C;
        match c {
            C::ObjectIntersectionOf(cs) | C::ObjectUnionOf(cs) => {
                cs.iter().for_each(|c| self.class(c))
            }
            C::ObjectComplementOf(c)
            | C::ObjectSomeValuesFrom(_, c)
            | C::ObjectAllValuesFrom(_, c) => self.class(c),
            C::ObjectMinCardinality(_, _, Some(c))
            | C::ObjectMaxCardinality(_, _, Some(c))
            | C::ObjectExactCardinality(_, _, Some(c)) => self.class(c),
            C::DataSomeValuesFrom(_, r) | C::DataAllValuesFrom(_, r) => self.range(r),
            C::DataMinCardinality(_, _, Some(r))
            | C::DataMaxCardinality(_, _, Some(r))
            | C::DataExactCardinality(_, _, Some(r)) => self.range(r),
            C::DataHasValue(_, l) => self.literals.push((l, false)),
            _ => {}
        }
    }

    fn kind(&mut self, k: &'a AxiomKind) {
        use AxiomKind as K;
        match k {
            K::SubClassOf(a, b) => {
                self.class(a);
                self.class(b);
            }
            K::EquivalentClasses(cs) | K::DisjointClasses(cs) | K::DisjointUnion(_, cs) => {
                cs.iter().for_each(|c| self.class(c))
            }
            K::ObjectPropertyDomain(_, c)
            | K::ObjectPropertyRange(_, c)
            | K::DataPropertyDomain(_, c)
            | K::HasKey(c, _, _) => self.class(c),
            K::ClassAssertion(c, _) => self.class(c),
            K::DataPropertyRange(_, r) | K::DatatypeDefinition(_, r) => self.range(r),
            K::DataPropertyAssertion(_, _, l) | K::NegativeDataPropertyAssertion(_, _, l) => {
                self.literals.push((l, false))
            }
            K::AnnotationAssertion(_, _, AnnotationValue::Literal(l)) => {
                self.literals.push((l, true))
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn xsd(l: &str) -> String {
        format!("{XSD}{l}")
    }

    #[test]
    fn numbers() {
        for (dt, ok, bad) in [
            (
                "integer",
                &[
                    "0",
                    "-12",
                    "+7",
                    "0012",
                    "123456789012345678901234567890123456789012",
                ][..],
                &["", "1.0", " 1", "1 ", "+", "1e3", "٣"][..],
            ),
            ("nonNegativeInteger", &["0", "-0", "+5"], &["-1"]),
            ("nonPositiveInteger", &["0", "-0", "-5", "+0"], &["1"]),
            ("positiveInteger", &["1", "+0001"], &["0", "-0", "-1"]),
            ("negativeInteger", &["-1"], &["0", "-0", "1"]),
            ("byte", &["-128", "127"], &["128", "-129"]),
            ("unsignedByte", &["255", "0"], &["256", "-1"]),
            ("short", &["-32768", "32767"], &["32768"]),
            ("int", &["2147483647"], &["2147483648"]),
            ("long", &["-9223372036854775808"], &["9223372036854775808"]),
            (
                "unsignedLong",
                &["18446744073709551615"],
                &["18446744073709551616"],
            ),
            ("unsignedInt", &["4294967295"], &["4294967296"]),
            ("unsignedShort", &["65535"], &["65536"]),
            (
                "decimal",
                &["1", "-1.5", "+.5", "5.", "0.0"],
                &[".", "1e3", "1.2.3", "", "INF"],
            ),
            (
                "double",
                &["1", "1.5e-3", "-INF", "+INF", "INF", "NaN", ".5E10", "5."],
                &["inf", "nan", "1e", "e3", "1.5e3.2", "-NaN"],
            ),
            ("float", &["3.4e38", "1e100"], &["abc"]),
            (
                "boolean",
                &["true", "false", "1", "0"],
                &["True", "yes", " true"],
            ),
        ] {
            for s in ok {
                assert_eq!(lexical(&xsd(dt), s), Some(true), "{dt} {s:?}");
            }
            for s in bad {
                assert_eq!(lexical(&xsd(dt), s), Some(false), "{dt} {s:?}");
            }
        }
        assert_eq!(lexical(vocab::OWL_RATIONAL, "1/3"), Some(true));
        assert_eq!(lexical(vocab::OWL_RATIONAL, "-4/2"), Some(true));
        for s in ["1/0", "1/+3", "1/-3", "1 / 3", "1", "1/3/4", "/3"] {
            assert_eq!(lexical(vocab::OWL_RATIONAL, s), Some(false), "{s}");
        }
        assert_eq!(lexical(vocab::OWL_REAL, "1"), Some(false));
    }

    #[test]
    fn strings() {
        assert_eq!(lexical(&xsd("string"), "a\tb\nc"), Some(true));
        assert_eq!(lexical(&xsd("string"), "a\u{1}b"), Some(false));
        assert_eq!(lexical(&xsd("normalizedString"), "a b"), Some(true));
        assert_eq!(lexical(&xsd("normalizedString"), "a\tb"), Some(false));
        assert_eq!(lexical(&xsd("token"), "a b"), Some(true));
        for s in [" a", "a ", "a  b", "a\nb"] {
            assert_eq!(lexical(&xsd("token"), s), Some(false), "{s:?}");
        }
        assert_eq!(lexical(&xsd("language"), "en-GB"), Some(true));
        assert_eq!(lexical(&xsd("language"), "x-private1"), Some(true));
        for s in ["", "en_GB", "toolonglang", "en-", "1en"] {
            assert_eq!(lexical(&xsd("language"), s), Some(false), "{s:?}");
        }
        assert_eq!(lexical(&xsd("Name"), "a:b"), Some(true));
        assert_eq!(lexical(&xsd("Name"), "1a"), Some(false));
        assert_eq!(lexical(&xsd("NCName"), "a-b.c"), Some(true));
        assert_eq!(lexical(&xsd("NCName"), "a:b"), Some(false));
        assert_eq!(lexical(&xsd("NMTOKEN"), "1a:b"), Some(true));
        assert_eq!(lexical(&xsd("NMTOKEN"), "a b"), Some(false));
        assert_eq!(lexical(&xsd("NMTOKEN"), ""), Some(false));
        assert_eq!(lexical(vocab::RDF_PLAIN_LITERAL, "abc@"), Some(true));
        assert_eq!(lexical(vocab::RDF_PLAIN_LITERAL, "a@b@en-GB"), Some(true));
        assert_eq!(lexical(vocab::RDF_PLAIN_LITERAL, "abc"), Some(false));
        assert_eq!(lexical(vocab::RDF_PLAIN_LITERAL, "abc@e_n"), Some(false));
        assert_eq!(lexical(&xsd("anyURI"), "not a uri, still fine"), Some(true));
    }

    #[test]
    fn binary_and_time() {
        assert_eq!(lexical(&xsd("hexBinary"), "0FB7"), Some(true));
        assert_eq!(lexical(&xsd("hexBinary"), ""), Some(true));
        assert_eq!(lexical(&xsd("hexBinary"), "0FB"), Some(false));
        assert_eq!(lexical(&xsd("hexBinary"), "0G"), Some(false));
        for s in ["", "AQID", "AQI=", "AQ==", "QU Jj", "YWJjZA=="] {
            assert_eq!(lexical(&xsd("base64Binary"), s), Some(true), "{s:?}");
        }
        for s in ["AQI", "A===", "AQJ=", "AR==", " AQID", "AQ  ID", "AQ*D"] {
            assert_eq!(lexical(&xsd("base64Binary"), s), Some(false), "{s:?}");
        }
        assert_eq!(lexical(&xsd("dateTime"), "2004-04-12T13:20:00"), Some(true));
        assert_eq!(
            lexical(&xsd("dateTime"), "2004-04-12T13:20:00-05:00"),
            Some(true)
        );
        assert_eq!(lexical(&xsd("dateTime"), "2004-04-12"), Some(false));
        assert_eq!(
            lexical(&xsd("dateTime"), "2004-13-12T13:20:00"),
            Some(false)
        );
        assert_eq!(
            lexical(&xsd("dateTimeStamp"), "2004-04-12T13:20:00Z"),
            Some(true)
        );
        assert_eq!(
            lexical(&xsd("dateTimeStamp"), "2004-04-12T13:20:00"),
            Some(false)
        );
    }

    #[test]
    fn xml_literals() {
        for s in [
            "",
            "text",
            "<a>b</a>",
            "<a x=\"1\" y='2'/>",
            "<a><b/>c&amp;d&#x41;&#65;</a>",
            "<!-- c --><?pi data?><![CDATA[<x>]]>",
            "a > b",
        ] {
            assert!(xml_content(s), "{s:?}");
        }
        for s in [
            "<a>",
            "</a>",
            "<a></b>",
            "<a><b></a></b>",
            "<a x=1/>",
            "<a x=\"1\" x=\"2\"/>",
            "a & b",
            "&#0;",
            "<!-- a -- b -->",
            "<?xml version=\"1.0\"?>",
            "<1a/>",
            "]]>",
        ] {
            assert!(!xml_content(s), "{s:?}");
        }
    }

    #[test]
    fn facet_spaces() {
        let int = Literal::typed("5", &xsd("integer"));
        let dbl = Literal::typed("5", &xsd("double"));
        let s = Literal::simple("[0-9]+");
        let min = xsd("minInclusive");
        let len = xsd("length");
        assert_eq!(facet_problem(&xsd("integer"), &min, &int), None);
        assert_eq!(facet_problem(&xsd("decimal"), &min, &int), None);
        assert!(facet_problem(&xsd("integer"), &min, &dbl).is_some());
        assert_eq!(facet_problem(&xsd("double"), &min, &dbl), None);
        assert!(facet_problem(&xsd("double"), &min, &int).is_some());
        assert!(facet_problem(&xsd("integer"), &len, &int).is_some());
        assert_eq!(facet_problem(&xsd("string"), &len, &int), None);
        assert!(
            facet_problem(&xsd("string"), &len, &Literal::typed("-1", &xsd("integer"))).is_some()
        );
        assert_eq!(facet_problem(&xsd("string"), &xsd("pattern"), &s), None);
        assert!(facet_problem(&xsd("string"), &xsd("pattern"), &int).is_some());
        assert!(facet_problem(&xsd("boolean"), &xsd("pattern"), &s).is_some());
        assert!(facet_problem(&xsd("string"), &xsd("totalDigits"), &int).is_some());
        assert_eq!(
            facet_problem(
                vocab::RDF_PLAIN_LITERAL,
                vocab::RDF_LANG_RANGE,
                &Literal::simple("en")
            ),
            None
        );
        let stamp = Literal::typed("2000-01-01T00:00:00Z", &xsd("dateTime"));
        let local_time = Literal::typed("2000-01-01T00:00:00", &xsd("dateTime"));
        assert_eq!(facet_problem(&xsd("dateTime"), &min, &local_time), None);
        assert_eq!(facet_problem(&xsd("dateTimeStamp"), &min, &stamp), None);
        assert!(facet_problem(&xsd("dateTimeStamp"), &min, &local_time).is_some());
        assert_eq!(facet_problem("http://ex.org/dt", &min, &int), None);
    }

    #[test]
    fn facet_values_are_judged_by_their_data_values() {
        let (len, pat) = (xsd("length"), xsd("pattern"));
        let string = xsd("string");
        let lit = |v: &str, dt: &str| Literal::typed(v, &xsd(dt));
        for v in [
            lit("3.0", "decimal"),
            lit("-0", "integer"),
            lit("3", "unsignedByte"),
            Literal::typed("6/2", vocab::OWL_RATIONAL),
            Literal::typed(
                "18446744073709551616/18446744073709551616",
                vocab::OWL_RATIONAL,
            ),
            Literal::typed("100000000000000000000000000000/10", vocab::OWL_RATIONAL),
        ] {
            assert_eq!(facet_problem(&string, &len, &v), None, "{v:?}");
        }
        for v in [
            lit("3.5", "decimal"),
            lit("-1.0", "decimal"),
            Literal::typed("7/2", vocab::OWL_RATIONAL),
            Literal::typed(
                "18446744073709551617/18446744073709551616",
                vocab::OWL_RATIONAL,
            ),
            lit("3", "double"),
            lit("x", "decimal"),
        ] {
            assert!(facet_problem(&string, &len, &v).is_some(), "{v:?}");
        }
        assert_eq!(facet_problem(&string, &pat, &lit("a", "token")), None);
        assert_eq!(
            facet_problem(
                &string,
                &pat,
                &Literal::typed("a+@", vocab::RDF_PLAIN_LITERAL)
            ),
            None
        );
        assert!(
            facet_problem(
                &string,
                &pat,
                &Literal::typed("a@en", vocab::RDF_PLAIN_LITERAL)
            )
            .is_some()
        );
    }

    #[test]
    fn patterns_and_language_ranges_follow_their_grammars() {
        for p in [
            "a+",
            r"\i\c*",
            "[a-z-[aeiou]]",
            r"\p{IsBasicLatin}+",
            r"\P{Lu}",
            r"\P{Cs}",
            r"\d{3}-\d{4}",
            "a{2,}b{0,1}",
            "[^-a-z]",
            "^a$",
            "(a|b)*|",
            "",
        ] {
            assert!(pattern(p), "{p:?}");
        }
        for p in [
            "[",
            "a{",
            "(a",
            r"\p{IsBasicLatin",
            "\\",
            "(?i)a",
            "(?:a)",
            r"\bword\b",
            "a{3,2}",
            "[z-a]",
            r"\p{Xx}",
            "a**",
            "[]",
        ] {
            assert!(!pattern(p), "{p:?}");
        }
        for r in ["en", "*", "de-*-DE", "en-US", "zh-Hant-1996"] {
            assert!(language_range(r), "{r:?}");
        }
        for r in ["en_US", "", "en-", "toolongtag", "-en", "1en"] {
            assert!(!language_range(r), "{r:?}");
        }
        let lr = vocab::RDF_LANG_RANGE;
        let plain = vocab::RDF_PLAIN_LITERAL;
        assert!(facet_problem(plain, lr, &Literal::simple("en_US")).is_some());
        assert!(facet_problem(&xsd("string"), &xsd("pattern"), &Literal::simple("[")).is_some());
    }

    #[test]
    fn xml_literals_need_known_entities_and_xml_characters() {
        let xml = |s: &str| lexical(vocab::RDF_XML_LITERAL, s);
        assert_eq!(xml("<a>&amp;&lt;&#65;&#x42;</a>"), Some(true));
        assert_eq!(xml("<a>&unknown;</a>"), Some(false));
        assert_eq!(xml("<a b=\"&nbsp;\"/>"), Some(false));
        assert_eq!(xml("<?pi ok?>"), Some(true));
        assert_eq!(xml("<?pi \u{1}?>"), Some(false));
    }
}
