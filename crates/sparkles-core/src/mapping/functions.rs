//! The functions of RML function maps and YARRRML `fn` entries (spec F19 §7.3): a fixed
//! table of GREL and IDLab functions, under the IRIs and parameter predicates of their
//! published FnO descriptions, and Sparkles' own `urn:x-sparkles:fn:` functions, which
//! name SPARQL 1.1 string, hash and numeric functions by their keyword with the
//! parameters `urn:x-sparkles:fn:arg1`, `arg2` and so on. Any other function fails when
//! the mapping is read, with the suggestion to use a CONSTRUCT template, where every
//! SPARQL function is available.

use super::source::{Cell, Dt};
use std::borrow::Cow;

pub const GREL: &str = "http://users.ugent.be/~bjdmeest/function/grel.ttl#";
pub const IDLAB: &str = "https://w3id.org/imec/idlab/function#";
pub const SPK: &str = "urn:x-sparkles:fn:";

/// A function of the table: its IRI, its parameter predicates (required first) and how
/// many are required.
pub struct Function {
    pub iri: String,
    pub params: Vec<String>,
    pub required: usize,
    /// whether the first parameter takes every value of a multi-valued argument
    pub list: bool,
    pub description: &'static str,
}

fn g(s: &str) -> String {
    format!("{GREL}{s}")
}

fn i(s: &str) -> String {
    format!("{IDLAB}{s}")
}

/// Every function, for `sparkles map check --functions`.
pub fn table() -> Vec<Function> {
    let f = |iri: String, params: &[String], required: usize, description: &'static str| Function {
        iri,
        params: params.to_vec(),
        required,
        list: false,
        description,
    };
    let v = g("valueParameter");
    let mut out = vec![
        f(
            g("toUpperCase"),
            std::slice::from_ref(&v),
            1,
            "the string in upper case",
        ),
        f(
            g("toLowerCase"),
            std::slice::from_ref(&v),
            1,
            "the string in lower case",
        ),
        f(
            g("string_toTitlecase"),
            std::slice::from_ref(&v),
            1,
            "each word with an upper-case first letter",
        ),
        f(
            g("string_trim"),
            std::slice::from_ref(&v),
            1,
            "the string without leading and trailing white space",
        ),
        f(
            g("string_length"),
            std::slice::from_ref(&v),
            1,
            "the number of characters, as xsd:integer",
        ),
        f(
            g("string_startsWith"),
            &[v.clone(), g("string_sub")],
            2,
            "whether the string starts with the other, as xsd:boolean",
        ),
        f(
            g("string_endsWith"),
            &[v.clone(), g("string_sub")],
            2,
            "whether the string ends with the other, as xsd:boolean",
        ),
        f(
            g("string_contains"),
            &[v.clone(), g("string_sub")],
            2,
            "whether the string contains the other, as xsd:boolean",
        ),
        f(
            g("string_indexOf"),
            &[v.clone(), g("string_sub")],
            2,
            "the first character index of the other string, or -1",
        ),
        f(
            g("string_lastIndexOf"),
            &[v.clone(), g("string_sub")],
            2,
            "the last character index of the other string, or -1",
        ),
        f(
            g("string_substring"),
            &[v.clone(), g("param_int_i_from"), g("param_int_i_opt_to")],
            2,
            "the characters from an index up to an optional end index",
        ),
        f(
            g("string_replace"),
            &[v.clone(), g("p_string_find"), g("p_string_replace")],
            3,
            "the string with every occurrence of a substring replaced",
        ),
        f(
            g("string_chomp"),
            &[v.clone(), g("p_string_sep")],
            2,
            "the string without the given suffix",
        ),
        f(
            g("string_split"),
            &[v.clone(), g("p_string_sep")],
            2,
            "the parts between separators, one value each",
        ),
        f(
            g("string_md5"),
            std::slice::from_ref(&v),
            1,
            "the MD5 digest in hexadecimal",
        ),
        f(
            g("string_sha1"),
            std::slice::from_ref(&v),
            1,
            "the SHA-1 digest in hexadecimal",
        ),
        f(
            g("escape"),
            &[v.clone(), g("modeParameter")],
            2,
            "the string escaped for html, xml, csv, url or javascript",
        ),
        f(
            g("controls_if"),
            &[g("bool_b"), g("any_true"), g("any_false")],
            2,
            "the second argument when the first is true, else the third",
        ),
        f(
            g("boolean_not"),
            &[g("bool_b")],
            1,
            "the negation of a boolean",
        ),
        f(
            i("equal"),
            &[v.clone(), g("valueParameter2")],
            2,
            "whether the two strings are equal, as xsd:boolean",
        ),
        f(
            i("notEqual"),
            &[v.clone(), g("valueParameter2")],
            2,
            "whether the two strings differ, as xsd:boolean",
        ),
        f(
            i("trueCondition"),
            &[i("strBoolean"), i("str")],
            2,
            "the string when the condition is true, else no value",
        ),
        f(
            i("stringContainsOtherString"),
            &[i("str"), i("otherStr"), i("delimiter")],
            3,
            "whether the delimited list contains the other string",
        ),
        f(
            i("concat"),
            &[i("str"), i("otherStr"), i("delimiter")],
            2,
            "the two strings joined with an optional delimiter",
        ),
        f(
            i("isNull"),
            &[i("str")],
            0,
            "whether the value is missing, as xsd:boolean",
        ),
        f(
            i("slugify"),
            &[i("str")],
            1,
            "the string as a lower-case slug of letters, digits and hyphens",
        ),
    ];
    out.push(Function {
        iri: g("array_join"),
        params: vec![g("p_array_a"), g("p_string_sep")],
        required: 1,
        list: true,
        description: "every value of the argument joined with a separator",
    });
    out.push(Function {
        iri: g("other_coalesce"),
        params: vec![g("p_any_rep_e")],
        required: 0,
        list: true,
        description: "the first value of the argument",
    });
    for (k, n, d) in SPK_FUNCTIONS {
        let params: Vec<String> = (1..=*n).map(|j| format!("{SPK}arg{j}")).collect();
        out.push(Function {
            iri: format!("{SPK}{k}"),
            required: match *k {
                "SUBSTR" => 2,
                "REPLACE" => 3,
                "CONCAT" => 0,
                "ROUND" | "ABS" | "CEIL" | "FLOOR" => 1,
                _ => *n,
            },
            params,
            list: false,
            description: d,
        });
    }
    out
}

/// The SPARQL functions of `urn:x-sparkles:fn:`, with their arity.
const SPK_FUNCTIONS: &[(&str, usize, &str)] = &[
    ("UCASE", 1, "SPARQL UCASE"),
    ("LCASE", 1, "SPARQL LCASE"),
    ("STRLEN", 1, "SPARQL STRLEN"),
    (
        "SUBSTR",
        3,
        "SPARQL SUBSTR (1-based start, optional length)",
    ),
    ("CONCAT", 8, "SPARQL CONCAT of up to eight arguments"),
    (
        "REPLACE",
        4,
        "SPARQL REPLACE with a regular expression and optional flags",
    ),
    ("STRBEFORE", 2, "SPARQL STRBEFORE"),
    ("STRAFTER", 2, "SPARQL STRAFTER"),
    ("CONTAINS", 2, "SPARQL CONTAINS"),
    ("STRSTARTS", 2, "SPARQL STRSTARTS"),
    ("STRENDS", 2, "SPARQL STRENDS"),
    ("ENCODE_FOR_URI", 1, "SPARQL ENCODE_FOR_URI"),
    ("MD5", 1, "SPARQL MD5"),
    ("SHA1", 1, "SPARQL SHA1"),
    ("SHA256", 1, "SPARQL SHA256"),
    ("SHA512", 1, "SPARQL SHA512"),
    ("ABS", 1, "SPARQL ABS"),
    ("CEIL", 1, "SPARQL CEIL"),
    ("FLOOR", 1, "SPARQL FLOOR"),
    ("ROUND", 1, "SPARQL ROUND"),
];

/// The function of the table with `iri`.
pub fn lookup(iri: &str) -> Option<Function> {
    table().into_iter().find(|f| f.iri == iri)
}

fn boolean(b: bool) -> Cell<'static> {
    Cell {
        lex: Cow::Borrowed(if b { "true" } else { "false" }),
        dt: Dt::Boolean,
    }
}

fn int(n: i64) -> Cell<'static> {
    Cell {
        lex: Cow::Owned(n.to_string()),
        dt: Dt::Integer,
    }
}

fn text(s: String) -> Cell<'static> {
    Cell {
        lex: Cow::Owned(s),
        dt: Dt::Str,
    }
}

fn hexdigest<D: sha2::Digest>(s: &str) -> String {
    let mut h = D::new();
    h.update(s.as_bytes());
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

fn char_index(hay: &str, byte: Option<usize>) -> i64 {
    byte.map_or(-1, |b| hay[..b].chars().count() as i64)
}

fn number(s: &str) -> Result<f64, String> {
    s.trim()
        .parse::<f64>()
        .map_err(|_| format!("{s:?} is not a number"))
}

fn num_cell(x: f64, integer_input: bool) -> Cell<'static> {
    if integer_input && x.fract() == 0.0 && x.abs() < 9.0e15 {
        int(x as i64)
    } else {
        Cell {
            lex: Cow::Owned(super::source::canonical_double(x)),
            dt: Dt::Double,
        }
    }
}

/// Call function `iri` with its arguments: for each parameter of the table, its values
/// (none when the argument is missing or NULL). Returns the values of the result, none
/// for NULL.
pub fn call(iri: &str, args: &[Vec<String>]) -> Result<Vec<Cell<'static>>, String> {
    let a = |k: usize| args.get(k).and_then(|v| v.first()).map(String::as_str);
    let need = |k: usize| a(k).ok_or_else(|| "a required argument has no value".to_string());
    if let Some(name) = iri.strip_prefix(GREL) {
        if name != "controls_if"
            && name != "other_coalesce"
            && name != "array_join"
            && a(0).is_none()
        {
            return Ok(Vec::new());
        }
        return Ok(match name {
            "toUpperCase" => vec![text(need(0)?.to_uppercase())],
            "toLowerCase" => vec![text(need(0)?.to_lowercase())],
            "string_toTitlecase" => {
                let mut out = String::new();
                let mut start = true;
                for c in need(0)?.chars() {
                    if c.is_whitespace() {
                        start = true;
                        out.push(c);
                    } else if start {
                        out.extend(c.to_uppercase());
                        start = false;
                    } else {
                        out.extend(c.to_lowercase());
                    }
                }
                vec![text(out)]
            }
            "string_trim" => vec![text(need(0)?.trim().to_string())],
            "string_length" => vec![int(need(0)?.chars().count() as i64)],
            "string_startsWith" => vec![boolean(need(0)?.starts_with(need(1)?))],
            "string_endsWith" => vec![boolean(need(0)?.ends_with(need(1)?))],
            "string_contains" => vec![boolean(need(0)?.contains(need(1)?))],
            "string_indexOf" => {
                let s = need(0)?;
                vec![int(char_index(s, s.find(need(1)?)))]
            }
            "string_lastIndexOf" => {
                let s = need(0)?;
                vec![int(char_index(s, s.rfind(need(1)?)))]
            }
            "string_substring" => {
                let chars: Vec<char> = need(0)?.chars().collect();
                let len = chars.len() as i64;
                let idx = |s: &str| -> Result<i64, String> {
                    s.trim()
                        .parse::<i64>()
                        .map_err(|_| format!("{s:?} is not an integer"))
                };
                let norm = |v: i64| if v < 0 { (len + v).max(0) } else { v.min(len) };
                let from = norm(idx(need(1)?)?);
                let to = match a(2) {
                    Some(t) => norm(idx(t)?),
                    None => len,
                };
                vec![text(
                    chars[from as usize..to.max(from) as usize].iter().collect(),
                )]
            }
            "string_replace" => vec![text(need(0)?.replace(need(1)?, need(2)?))],
            "string_chomp" => {
                let s = need(0)?;
                vec![text(s.strip_suffix(need(1)?).unwrap_or(s).to_string())]
            }
            "string_split" => {
                let sep = need(1)?;
                if sep.is_empty() {
                    return Err("the separator is empty".into());
                }
                need(0)?.split(sep).map(|p| text(p.to_string())).collect()
            }
            "string_md5" => vec![text(hexdigest::<md5::Md5>(need(0)?))],
            "string_sha1" => vec![text(hexdigest::<sha1::Sha1>(need(0)?))],
            "escape" => {
                let s = need(0)?;
                vec![text(match need(1)? {
                    "html" | "xml" => s
                        .replace('&', "&amp;")
                        .replace('<', "&lt;")
                        .replace('>', "&gt;")
                        .replace('"', "&quot;")
                        .replace('\'', "&#39;"),
                    "csv" => {
                        if s.contains([',', '"', '\n', '\r']) {
                            format!("\"{}\"", s.replace('"', "\"\""))
                        } else {
                            s.to_string()
                        }
                    }
                    "url" => {
                        percent_encoding::utf8_percent_encode(s, percent_encoding::NON_ALPHANUMERIC)
                            .to_string()
                    }
                    "javascript" => s
                        .replace('\\', "\\\\")
                        .replace('"', "\\\"")
                        .replace('\'', "\\'")
                        .replace('\n', "\\n")
                        .replace('\r', "\\r")
                        .replace('\t', "\\t"),
                    m => {
                        return Err(format!(
                            "escape mode {m:?} is not html, xml, csv, url or javascript"
                        ));
                    }
                })]
            }
            "controls_if" => {
                let cond = a(0).is_some_and(|b| b == "true" || b == "1");
                let pick = if cond { 1 } else { 2 };
                args.get(pick)
                    .cloned()
                    .unwrap_or_default()
                    .into_iter()
                    .map(text)
                    .collect()
            }
            "boolean_not" => vec![boolean(!(need(0)? == "true" || need(0)? == "1"))],
            "array_join" => {
                let sep = a(1).unwrap_or("");
                let items = args.first().cloned().unwrap_or_default();
                if items.is_empty() {
                    Vec::new()
                } else {
                    vec![text(items.join(sep))]
                }
            }
            "other_coalesce" => a(0).map(|s| vec![text(s.to_string())]).unwrap_or_default(),
            _ => return Err(format!("unknown function <{iri}>")),
        });
    }
    if let Some(name) = iri.strip_prefix(IDLAB) {
        return Ok(match name {
            "equal" => vec![boolean(a(0) == a(1))],
            "notEqual" => vec![boolean(a(0) != a(1))],
            "trueCondition" => {
                if a(0) == Some("true") {
                    a(1).map(|s| vec![text(s.to_string())]).unwrap_or_default()
                } else {
                    Vec::new()
                }
            }
            "stringContainsOtherString" => {
                let (Some(s), Some(o)) = (a(0), a(1)) else {
                    return Ok(vec![boolean(false)]);
                };
                let d = a(2).unwrap_or(" ");
                vec![boolean(s.split(d).any(|p| p == o))]
            }
            "concat" => {
                let (Some(s), Some(o)) = (a(0), a(1)) else {
                    return Ok(Vec::new());
                };
                vec![text(format!("{s}{}{o}", a(2).unwrap_or("")))]
            }
            "isNull" => vec![boolean(a(0).is_none())],
            "slugify" => {
                let Some(s) = a(0) else { return Ok(Vec::new()) };
                let mut out = String::new();
                let mut dash = false;
                for c in s.to_lowercase().chars() {
                    if c.is_alphanumeric() {
                        out.push(c);
                        dash = false;
                    } else if !dash && !out.is_empty() {
                        out.push('-');
                        dash = true;
                    }
                }
                vec![text(out.trim_end_matches('-').to_string())]
            }
            _ => return Err(format!("unknown function <{iri}>")),
        });
    }
    if let Some(name) = iri.strip_prefix(SPK) {
        if name != "CONCAT" && a(0).is_none() {
            return Ok(Vec::new());
        }
        let s0 = || need(0);
        return Ok(match name {
            "UCASE" => vec![text(s0()?.to_uppercase())],
            "LCASE" => vec![text(s0()?.to_lowercase())],
            "STRLEN" => vec![int(s0()?.chars().count() as i64)],
            "SUBSTR" => {
                let chars: Vec<char> = s0()?.chars().collect();
                let start = number(need(1)?)?.round() as i64;
                let end = match a(2) {
                    Some(l) => start + number(l)?.round() as i64,
                    None => i64::MAX,
                };
                let out: String = chars
                    .iter()
                    .enumerate()
                    .filter(|(k, _)| {
                        let pos = *k as i64 + 1;
                        pos >= start && pos < end
                    })
                    .map(|(_, c)| *c)
                    .collect();
                vec![text(out)]
            }
            "CONCAT" => vec![text(
                args.iter()
                    .filter_map(|v| v.first().cloned())
                    .collect::<Vec<_>>()
                    .concat(),
            )],
            "REPLACE" => {
                let mut b = regex::RegexBuilder::new(need(1)?);
                for f in a(3).unwrap_or("").chars() {
                    match f {
                        'i' => b.case_insensitive(true),
                        's' => b.dot_matches_new_line(true),
                        'm' => b.multi_line(true),
                        'x' => b.ignore_whitespace(true),
                        _ => return Err(format!("REPLACE flag {f:?} is not i, s, m or x")),
                    };
                }
                let re = b.build().map_err(|e| format!("REPLACE pattern: {e}"))?;
                let rep = xpath_replacement(need(2)?)?;
                vec![text(re.replace_all(s0()?, rep.as_str()).into_owned())]
            }
            "STRBEFORE" => {
                let s = s0()?;
                vec![text(
                    s.find(need(1)?)
                        .map_or(String::new(), |k| s[..k].to_string()),
                )]
            }
            "STRAFTER" => {
                let (s, t) = (s0()?, need(1)?);
                vec![text(
                    s.find(t)
                        .map_or(String::new(), |k| s[k + t.len()..].to_string()),
                )]
            }
            "CONTAINS" => vec![boolean(s0()?.contains(need(1)?))],
            "STRSTARTS" => vec![boolean(s0()?.starts_with(need(1)?))],
            "STRENDS" => vec![boolean(s0()?.ends_with(need(1)?))],
            "ENCODE_FOR_URI" => {
                let mut out = String::new();
                for b in s0()?.bytes() {
                    if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
                        out.push(char::from(b));
                    } else {
                        out.push_str(&format!("%{b:02X}"));
                    }
                }
                vec![text(out)]
            }
            "MD5" => vec![text(hexdigest::<md5::Md5>(s0()?))],
            "SHA1" => vec![text(hexdigest::<sha1::Sha1>(s0()?))],
            "SHA256" => vec![text(hexdigest::<sha2::Sha256>(s0()?))],
            "SHA512" => vec![text(hexdigest::<sha2::Sha512>(s0()?))],
            "ABS" | "CEIL" | "FLOOR" | "ROUND" => {
                let s = s0()?;
                let x = number(s)?;
                let y = match name {
                    "ABS" => x.abs(),
                    "CEIL" => x.ceil(),
                    "FLOOR" => x.floor(),
                    _ => (x + 0.5).floor(),
                };
                vec![num_cell(y, !s.contains(['.', 'e', 'E']))]
            }
            _ => return Err(format!("unknown function <{iri}>")),
        });
    }
    Err(format!("unknown function <{iri}>"))
}

/// An XPath `fn:replace` replacement string (SPARQL's REPLACE) in the regex crate's
/// syntax: `$N` names group N, and `\$` and `\\` are a literal dollar and backslash.
fn xpath_replacement(r: &str) -> Result<String, String> {
    let mut out = String::with_capacity(r.len());
    let mut chars = r.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' => match chars.next() {
                Some('$') => out.push_str("$$"),
                Some('\\') => out.push('\\'),
                _ => {
                    return Err(format!(
                        "REPLACE replacement {r:?}: a backslash escapes $ or \\ only"
                    ));
                }
            },
            '$' => {
                let mut n = String::new();
                while let Some(d) = chars.peek().filter(|d| d.is_ascii_digit()) {
                    n.push(*d);
                    chars.next();
                }
                if n.is_empty() {
                    return Err(format!(
                        "REPLACE replacement {r:?}: $ must be followed by a group number"
                    ));
                }
                out.push_str(&format!("${{{n}}}"));
            }
            c => out.push(c),
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xpath_replacements() {
        let r = |s| super::xpath_replacement(s);
        assert_eq!(r("$1a").unwrap(), "${1}a");
        assert_eq!(r(r"\$x\\").unwrap(), r"$$x\");
        assert!(r("$x").is_err());
        assert!(r(r"\n").is_err());
        let re = regex::Regex::new("(a)(b)").unwrap();
        assert_eq!(re.replace_all("ab", r("$2$1x").unwrap().as_str()), "bax");
    }

    fn one(iri: &str, args: &[&[&str]]) -> Vec<String> {
        let args: Vec<Vec<String>> = args
            .iter()
            .map(|v| v.iter().map(|s| s.to_string()).collect())
            .collect();
        call(iri, &args)
            .unwrap()
            .into_iter()
            .map(|c| c.lex.into_owned())
            .collect()
    }

    #[test]
    fn grel_and_idlab() {
        assert_eq!(one(&g("toUpperCase"), &[&["abc"]]), ["ABC"]);
        assert_eq!(one(&g("string_split"), &[&["a;b"], &[";"]]), ["a", "b"]);
        assert_eq!(
            one(&g("string_substring"), &[&["hello"], &["1"], &["3"]]),
            ["el"]
        );
        assert_eq!(
            one(&g("string_replace"), &[&["a-b-c"], &["-"], &["+"]]),
            ["a+b+c"]
        );
        assert_eq!(one(&g("array_join"), &[&["a", "b"], &[","]]), ["a,b"]);
        assert_eq!(
            one(&g("string_toTitlecase"), &[&["hello WORLD"]]),
            ["Hello World"]
        );
        assert_eq!(one(&g("escape"), &[&["a b"], &["url"]]), ["a%20b"]);
        assert_eq!(one(&i("equal"), &[&["a"], &["a"]]), ["true"]);
        assert_eq!(
            one(&i("trueCondition"), &[&["false"], &["x"]]),
            Vec::<String>::new()
        );
        assert_eq!(one(&i("slugify"), &[&["Hello, World!"]]), ["hello-world"]);
        assert_eq!(
            one(&format!("{SPK}SUBSTR"), &[&["hello"], &["2"], &["3"]]),
            ["ell"]
        );
        assert_eq!(
            one(&format!("{SPK}REPLACE"), &[&["abc"], &["b"], &["X"]]),
            ["aXc"]
        );
        assert_eq!(one(&format!("{SPK}ROUND"), &[&["2.5"]]), ["3.0E0"]);
        assert_eq!(one(&format!("{SPK}ABS"), &[&["-2"]]), ["2"]);
        assert!(lookup(&g("toUpperCase")).is_some());
        assert!(lookup("http://ex.org/nope").is_none());
    }
}
