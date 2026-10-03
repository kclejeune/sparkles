//! The rules over terms, the same in SPARQL and Turtle: `language-tag-case`,
//! `deprecated-language-tag`, `suspicious-datatype`, `redundant-datatype`, `iri-space`
//! and `deprecated-syntax`.

use super::{Cst, Edit, Fix, Out, unescape_iri};
use crate::FormatError;
use crate::lex::TokenKind;
use crate::sparql::keywords::Kw;
use crate::syntax::NodeKind;
use crate::tree::TokenId;

const XSD: &str = "http://www.w3.org/2001/XMLSchema#";
const RDF: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#";
const OWL_DATA_RANGE: &str = "http://www.w3.org/2002/07/owl#DataRange";

pub(super) fn run(c: &Cst<'_, '_>, out: &mut Out<'_>) {
    for n in c.nodes() {
        match c.kind(n) {
            NodeKind::Literal => literal(c, n, out),
            NodeKind::Call => call(c, n, out),
            _ => {}
        }
    }
    for i in 0..c.tree.tokens.len() {
        let t = TokenId(i as u32);
        match c.token_kind(t) {
            TokenKind::IriRef => iri_ref(c, t, out),
            TokenKind::PnameLn if c.expand(t).as_deref() == Some(OWL_DATA_RANGE) => {
                let (s, e) = c.span(t);
                out.report("deprecated-syntax", s, e, data_range_message());
            }
            _ => {}
        }
    }
}

fn data_range_message() -> String {
    "owl:DataRange is deprecated in OWL 2; use rdfs:Datatype".to_string()
}

/// An IRI written in full: whitespace in it, Jena's old namespaces, `owl:DataRange`.
fn iri_ref(c: &Cst<'_, '_>, t: TokenId, out: &mut Out<'_>) {
    let text = c.text(t);
    let iri = unescape_iri(&text[1..text.len() - 1]);
    let (s, e) = c.span(t);
    if iri.chars().any(char::is_whitespace) {
        out.report(
            "iri-space",
            s,
            e,
            "the IRI holds whitespace; percent-encode it (a space is %20)".to_string(),
        );
    }
    if let Some(rest) = iri.strip_prefix("http://jena.hpl.hp.com/ARQ/") {
        out.report(
            "deprecated-syntax",
            s,
            e,
            format!(
                "http://jena.hpl.hp.com/ARQ/ is Jena's old namespace, which Jena warns about \
                 and Sparkles does not recognize; use http://jena.apache.org/ARQ/{rest}"
            ),
        );
    }
    if iri == OWL_DATA_RANGE {
        out.report("deprecated-syntax", s, e, data_range_message());
    }
}

/// `IRI("…")` and `URI("…")` of a string with whitespace.
fn call(c: &Cst<'_, '_>, n: crate::tree::NodeId, out: &mut Out<'_>) {
    let Some(first) = c.own_tokens(n).first().copied() else {
        return;
    };
    if !matches!(c.token_kind(first), TokenKind::Kw(Kw::Iri | Kw::Uri)) {
        return;
    }
    let tokens = c.tokens_where(n, &|_| false);
    let strings: Vec<TokenId> = tokens
        .into_iter()
        .filter(|&t| is_string(c.token_kind(t)))
        .collect();
    if let [s] = strings.as_slice()
        && string_value(c.text(*s)).chars().any(char::is_whitespace)
    {
        let (a, b) = c.span(*s);
        out.report(
            "iri-space",
            a,
            b,
            "this string becomes an IRI but holds whitespace; percent-encode it (a space is %20)"
                .to_string(),
        );
    }
}

fn is_string(k: TokenKind) -> bool {
    matches!(
        k,
        TokenKind::String1 | TokenKind::String2 | TokenKind::StringLong1 | TokenKind::StringLong2
    )
}

/// A literal: its language tag, or its datatype and lexical form.
fn literal(c: &Cst<'_, '_>, n: crate::tree::NodeId, out: &mut Out<'_>) {
    let tokens = c.own_tokens(n);
    let Some(&string) = tokens.first() else {
        return;
    };
    if !is_string(c.token_kind(string)) {
        return;
    }
    match tokens.as_slice() {
        [_, tag] if c.token_kind(*tag) == TokenKind::LangDir => language_tag(c, *tag, out),
        [_, hat, dt] if c.token_kind(*hat) == TokenKind::HatHat => {
            let Some(iri) = c.iri(*dt) else {
                return;
            };
            datatype(c, string, *hat, *dt, &iri, out);
        }
        _ => {}
    }
}

fn language_tag(c: &Cst<'_, '_>, t: TokenId, out: &mut Out<'_>) {
    let written = &c.text(t)[1..];
    let (s, e) = c.span(t);
    let canonical = canonical_case(written);
    if canonical != written {
        out.report_fix(
            "language-tag-case",
            s,
            e,
            format!("write the language tag {written} as {canonical}"),
            Some(Fix {
                title: format!("Write @{canonical}"),
                edits: vec![Edit {
                    start: s,
                    end: e,
                    insert: format!("@{canonical}"),
                }],
            }),
        );
    }
    if let Some(m) = deprecated_tag(written) {
        out.report("deprecated-language-tag", s, e, m);
    }
}

/// A language tag (with an optional `--ltr` or `--rtl`) in the case BCP 47 recommends:
/// the language lowercase, a script in title case, a region uppercase, the rest
/// lowercase, and everything after a singleton lowercase.
pub(crate) fn canonical_case(tag: &str) -> String {
    let (lang, dir) = match tag.split_once("--") {
        Some((l, d)) => (l, Some(d)),
        None => (tag, None),
    };
    let mut out = String::with_capacity(tag.len());
    let mut after_singleton = false;
    for (i, sub) in lang.split('-').enumerate() {
        if i > 0 {
            out.push('-');
        }
        let alpha = sub.bytes().all(|b| b.is_ascii_alphabetic());
        if i == 0 || after_singleton {
            out.push_str(&sub.to_ascii_lowercase());
            after_singleton |= sub.len() == 1;
        } else if sub.len() == 1 {
            after_singleton = true;
            out.push_str(&sub.to_ascii_lowercase());
        } else if sub.len() == 4 && alpha {
            out.push_str(&sub[..1].to_ascii_uppercase());
            out.push_str(&sub[1..].to_ascii_lowercase());
        } else if sub.len() == 2 && alpha {
            out.push_str(&sub.to_ascii_uppercase());
        } else {
            out.push_str(&sub.to_ascii_lowercase());
        }
    }
    if let Some(d) = dir {
        out.push_str("--");
        out.push_str(&d.to_ascii_lowercase());
    }
    out
}

/// Grandfathered and irregular tags of the IANA registry with their preferred values.
const DEPRECATED_TAGS: &[(&str, &str)] = &[
    ("art-lojban", "jbo"),
    ("i-ami", "ami"),
    ("i-bnn", "bnn"),
    ("i-hak", "hak"),
    ("i-klingon", "tlh"),
    ("i-lux", "lb"),
    ("i-navajo", "nv"),
    ("i-pwn", "pwn"),
    ("i-tao", "tao"),
    ("i-tay", "tay"),
    ("i-tsu", "tsu"),
    ("no-bok", "nb"),
    ("no-nyn", "nn"),
    ("sgn-be-fr", "sfb"),
    ("sgn-be-nl", "vgt"),
    ("sgn-ch-de", "sgg"),
    ("zh-guoyu", "cmn"),
    ("zh-hakka", "hak"),
    ("zh-min-nan", "nan"),
    ("zh-xiang", "hsn"),
];

/// Deprecated language subtags with their preferred values (ISO 639 codes withdrawn
/// in favour of others).
const DEPRECATED_LANGUAGES: &[(&str, &str)] = &[
    ("in", "id"),
    ("iw", "he"),
    ("ji", "yi"),
    ("jw", "jv"),
    ("mo", "ro"),
];

/// Deprecated region subtags with their preferred values.
const DEPRECATED_REGIONS: &[(&str, &str)] = &[
    ("BU", "MM"),
    ("DD", "DE"),
    ("FX", "FR"),
    ("TP", "TL"),
    ("YD", "YE"),
    ("ZR", "CD"),
];

/// Why `tag` is deprecated, with the tag to use instead.
fn deprecated_tag(tag: &str) -> Option<String> {
    let lang = tag.split_once("--").map_or(tag, |(l, _)| l);
    let lower = lang.to_ascii_lowercase();
    if let Some((_, to)) = DEPRECATED_TAGS.iter().find(|(from, _)| *from == lower) {
        return Some(format!("the language tag {lang} is deprecated; use {to}"));
    }
    let mut subs = lower.split('-');
    let first = subs.next()?;
    if let Some((from, to)) = DEPRECATED_LANGUAGES.iter().find(|(f, _)| *f == first) {
        return Some(format!(
            "the language subtag {from} of {lang} is deprecated; use {to}"
        ));
    }
    for sub in subs {
        if sub.len() == 1 {
            break;
        }
        let upper = sub.to_ascii_uppercase();
        if let Some((from, to)) = DEPRECATED_REGIONS.iter().find(|(f, _)| *f == upper) {
            return Some(format!(
                "the region subtag {from} of {lang} is deprecated; use {to}"
            ));
        }
    }
    None
}

/// A typed literal's datatype against its lexical form.
fn datatype(
    c: &Cst<'_, '_>,
    string: TokenId,
    hat: TokenId,
    dt: TokenId,
    iri: &str,
    out: &mut Out<'_>,
) {
    let (ds, de) = c.span(dt);
    let (ls, _) = c.span(string);
    let lexical = string_value(c.text(string));
    let dt_text = c.text(dt);
    if let Some(local) = iri.strip_prefix(XSD) {
        if local == "string" {
            let (hs, _) = c.span(hat);
            out.report_fix(
                "redundant-datatype",
                hs,
                de,
                format!("{dt_text} is the datatype of every plain string; it can be left out"),
                Some(Fix {
                    title: format!("Remove ^^{dt_text}"),
                    edits: vec![Edit {
                        start: hs,
                        end: de,
                        insert: String::new(),
                    }],
                }),
            );
            return;
        }
        if !XSD_TYPES.contains(&local) {
            out.report(
                "suspicious-datatype",
                ds,
                de,
                format!("{dt_text} is not an XML Schema datatype"),
            );
            return;
        }
        if local == "anyURI" && lexical.chars().any(char::is_whitespace) {
            out.report(
                "iri-space",
                ls,
                de,
                "this xsd:anyURI holds whitespace; percent-encode it (a space is %20)".to_string(),
            );
            return;
        }
        if let Some(problem) = lexical_problem(local, &lexical) {
            out.report(
                "suspicious-datatype",
                ls,
                de,
                format!("\"{lexical}\" is not a valid xsd:{local}: {problem}"),
            );
        }
        return;
    }
    if iri == format!("{RDF}langString") || iri == format!("{RDF}dirLangString") {
        out.report(
            "suspicious-datatype",
            ds,
            de,
            format!("{dt_text} cannot be written as a datatype; give the string a language tag"),
        );
        return;
    }
    let misspelled = [
        "http://www.w3.org/2001/XMLSchema",
        "https://www.w3.org/2001/XMLSchema",
    ]
    .iter()
    .any(|ns| iri.starts_with(ns))
        && !iri.starts_with(XSD);
    if misspelled {
        let local = iri.rsplit(['#', '/', ':']).next().unwrap_or("");
        out.report(
            "suspicious-datatype",
            ds,
            de,
            format!("{dt_text} is not in the XML Schema namespace; did you mean <{XSD}{local}>?"),
        );
    }
}

/// The XML Schema datatypes, as RDF 1.1 lists them and the ones it leaves out.
const XSD_TYPES: &[&str] = &[
    "string",
    "boolean",
    "decimal",
    "integer",
    "double",
    "float",
    "date",
    "time",
    "dateTime",
    "dateTimeStamp",
    "gYear",
    "gMonth",
    "gDay",
    "gYearMonth",
    "gMonthDay",
    "duration",
    "yearMonthDuration",
    "dayTimeDuration",
    "byte",
    "short",
    "int",
    "long",
    "unsignedByte",
    "unsignedShort",
    "unsignedInt",
    "unsignedLong",
    "positiveInteger",
    "nonNegativeInteger",
    "negativeInteger",
    "nonPositiveInteger",
    "hexBinary",
    "base64Binary",
    "anyURI",
    "language",
    "normalizedString",
    "token",
    "NMTOKEN",
    "Name",
    "NCName",
    "NMTOKENS",
    "ID",
    "IDREF",
    "IDREFS",
    "ENTITY",
    "ENTITIES",
    "QName",
    "NOTATION",
    "anySimpleType",
    "anyAtomicType",
];

/// What is wrong with `lexical` as an `xsd:{local}`, if anything.
fn lexical_problem(local: &str, lexical: &str) -> Option<String> {
    use std::str::FromStr;
    let numeric_or_temporal = !matches!(
        local,
        "normalizedString"
            | "token"
            | "language"
            | "NMTOKEN"
            | "NMTOKENS"
            | "Name"
            | "NCName"
            | "ID"
            | "IDREF"
            | "IDREFS"
            | "ENTITY"
            | "ENTITIES"
            | "QName"
            | "NOTATION"
            | "anySimpleType"
            | "anyAtomicType"
    );
    if numeric_or_temporal && lexical.trim() != lexical {
        return Some("it has leading or trailing whitespace".to_string());
    }
    let range = |lo: i128, hi: i128| integer_in(lexical, lo, hi);
    let ok = match local {
        "boolean" => matches!(lexical, "true" | "false" | "1" | "0"),
        "decimal" => is_decimal(lexical),
        "double" | "float" => is_double(lexical),
        "integer" => is_integer(lexical),
        "nonPositiveInteger" => is_integer(lexical) && !positive(lexical),
        "negativeInteger" => is_integer(lexical) && negative(lexical),
        "nonNegativeInteger" => is_integer(lexical) && !negative(lexical),
        "positiveInteger" => is_integer(lexical) && positive(lexical),
        "long" => range(i64::MIN.into(), i64::MAX.into()),
        "int" => range(i32::MIN.into(), i32::MAX.into()),
        "short" => range(i16::MIN.into(), i16::MAX.into()),
        "byte" => range(i8::MIN.into(), i8::MAX.into()),
        "unsignedLong" => range(0, u64::MAX.into()),
        "unsignedInt" => range(0, u32::MAX.into()),
        "unsignedShort" => range(0, u16::MAX.into()),
        "unsignedByte" => range(0, u8::MAX.into()),
        "dateTime" => oxsdatatypes::DateTime::from_str(lexical).is_ok(),
        "dateTimeStamp" => {
            oxsdatatypes::DateTime::from_str(lexical).is_ok_and(|d| d.timezone().is_some())
        }
        "date" => oxsdatatypes::Date::from_str(lexical).is_ok(),
        "time" => oxsdatatypes::Time::from_str(lexical).is_ok(),
        "gYear" => oxsdatatypes::GYear::from_str(lexical).is_ok(),
        "gMonth" => oxsdatatypes::GMonth::from_str(lexical).is_ok(),
        "gDay" => oxsdatatypes::GDay::from_str(lexical).is_ok(),
        "gYearMonth" => oxsdatatypes::GYearMonth::from_str(lexical).is_ok(),
        "gMonthDay" => oxsdatatypes::GMonthDay::from_str(lexical).is_ok(),
        "duration" => oxsdatatypes::Duration::from_str(lexical).is_ok(),
        "yearMonthDuration" => oxsdatatypes::YearMonthDuration::from_str(lexical).is_ok(),
        "dayTimeDuration" => oxsdatatypes::DayTimeDuration::from_str(lexical).is_ok(),
        "hexBinary" => {
            lexical.len().is_multiple_of(2) && lexical.bytes().all(|b| b.is_ascii_hexdigit())
        }
        "base64Binary" => is_base64(lexical),
        "language" => is_language(lexical),
        "normalizedString" => !lexical.contains(['\n', '\r', '\t']),
        "token" => {
            !lexical.contains(['\n', '\r', '\t'])
                && !lexical.contains("  ")
                && lexical.trim() == lexical
        }
        _ => true,
    };
    if ok {
        return None;
    }
    let what = match local {
        "boolean" => "expected true, false, 1 or 0",
        "decimal" => "expected digits with an optional sign and decimal point",
        "double" | "float" => "expected a number such as 1.5e3, INF or NaN",
        "integer" | "nonPositiveInteger" | "negativeInteger" | "nonNegativeInteger"
        | "positiveInteger" | "long" | "int" | "short" | "byte" | "unsignedLong"
        | "unsignedInt" | "unsignedShort" | "unsignedByte" => {
            "expected an integer in the datatype's range"
        }
        "dateTimeStamp" => "expected a date and time with a timezone",
        _ => "the lexical form does not match the datatype",
    };
    Some(what.to_string())
}

fn digits(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit())
}

fn unsigned(s: &str) -> &str {
    s.strip_prefix(['+', '-']).unwrap_or(s)
}

fn is_integer(s: &str) -> bool {
    digits(unsigned(s))
}

fn negative(s: &str) -> bool {
    s.starts_with('-') && unsigned(s).bytes().any(|b| b != b'0')
}

fn positive(s: &str) -> bool {
    !s.starts_with('-') && unsigned(s).bytes().any(|b| b != b'0')
}

fn integer_in(s: &str, lo: i128, hi: i128) -> bool {
    is_integer(s) && s.parse::<i128>().is_ok_and(|v| lo <= v && v <= hi)
}

fn is_decimal(s: &str) -> bool {
    let u = unsigned(s);
    match u.split_once('.') {
        Some((a, b)) => {
            (a.is_empty() || digits(a))
                && (b.is_empty() || digits(b))
                && !(a.is_empty() && b.is_empty())
        }
        None => digits(u),
    }
}

fn is_double(s: &str) -> bool {
    if matches!(s, "INF" | "+INF" | "-INF" | "NaN") {
        return true;
    }
    match s.split_once(['e', 'E']) {
        Some((m, e)) => is_decimal(m) && is_integer(e),
        None => is_decimal(s),
    }
}

fn is_base64(s: &str) -> bool {
    let compact: Vec<u8> = s.bytes().filter(|b| *b != b' ').collect();
    let pad = compact.iter().rev().take_while(|b| **b == b'=').count();
    compact.len().is_multiple_of(4)
        && pad <= 2
        && compact[..compact.len() - pad]
            .iter()
            .all(|b| b.is_ascii_alphanumeric() || *b == b'+' || *b == b'/')
}

fn is_language(s: &str) -> bool {
    let mut subs = s.split('-');
    let first = subs.next().unwrap_or("");
    (1..=8).contains(&first.len())
        && first.bytes().all(|b| b.is_ascii_alphabetic())
        && subs.all(|p| (1..=8).contains(&p.len()) && p.bytes().all(|b| b.is_ascii_alphanumeric()))
}

/// The value of a string token: its quotes dropped and its escapes decoded.
pub(crate) fn string_value(text: &str) -> String {
    let quote = if text.starts_with("\"\"\"") || text.starts_with("'''") {
        3
    } else {
        1
    };
    let inner = &text[quote.min(text.len())..text.len().saturating_sub(quote).max(quote)];
    let mut out = String::with_capacity(inner.len());
    let mut chars = inner.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch != '\\' {
            out.push(ch);
            continue;
        }
        match chars.next() {
            Some('t') => out.push('\t'),
            Some('b') => out.push('\u{8}'),
            Some('n') => out.push('\n'),
            Some('r') => out.push('\r'),
            Some('f') => out.push('\u{c}'),
            Some(k @ ('u' | 'U')) => {
                let n = if k == 'u' { 4 } else { 8 };
                let hex: String = (0..n).filter_map(|_| chars.next()).collect();
                match u32::from_str_radix(&hex, 16).ok().and_then(char::from_u32) {
                    Some(c) => out.push(c),
                    None => {
                        out.push('\\');
                        out.push(k);
                        out.push_str(&hex);
                    }
                }
            }
            Some(other) => out.push(other),
            None => out.push('\\'),
        }
    }
    out
}

/// When the reference parser stopped inside an IRI written with whitespace
/// (`<http://example.org/a b>`), report that as `iri-space`, an error. Returns whether it
/// did.
pub(super) fn iri_space_in_error(text: &str, e: &FormatError, out: &mut Out<'_>) -> bool {
    let FormatError::Syntax { offset, .. } = e else {
        return false;
    };
    if !out.on("iri-space") {
        return false;
    }
    let bytes = text.as_bytes();
    let mut i = 0;
    while let Some(rel) = text[i..].find('<') {
        let start = i + rel;
        i = start + 1;
        // a scheme right after the `<`
        let rest = &text[start + 1..];
        let scheme = rest
            .bytes()
            .take_while(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'-' | b'.'))
            .count();
        if scheme == 0
            || !bytes[start + 1].is_ascii_alphabetic()
            || rest.as_bytes().get(scheme) != Some(&b':')
        {
            continue;
        }
        let line_end = rest.find('\n').unwrap_or(rest.len());
        let Some(close) = rest[..line_end].find('>') else {
            continue;
        };
        let inner = &rest[..close];
        if inner.contains(['<', '"', '{', '}']) || !inner.contains([' ', '\t']) {
            continue;
        }
        let end = start + 1 + close + 1;
        if (start..=end).contains(offset) {
            out.found.retain(|d| d.rule != "syntax");
            let before = out.found.len();
            out.report(
                "iri-space",
                start,
                end,
                "this IRI holds a space, which an IRI cannot; percent-encode it (a space is %20)"
                    .to_string(),
            );
            if let Some(d) = out.found.get_mut(before) {
                d.severity = super::Severity::Error;
            }
            return true;
        }
    }
    false
}
