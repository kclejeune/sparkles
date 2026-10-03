//! URI templates (RFC 6570, levels 1 to 4), as CSVW's `aboutUrl`, `propertyUrl` and
//! `valueUrl` use them. Variable names are read leniently: anything up to `,`, `:`, `*`
//! or `}` is a name, so CSVW column names such as `first-name` can be referenced.

use std::borrow::Cow;
use std::fmt::Write as _;

/// A parsed template. Its variables are numbered in [`vars`](Self::vars) order, and
/// [`expand`](Self::expand) asks for their values by that number.
#[derive(Clone, Debug, PartialEq)]
pub struct UriTemplate {
    parts: Vec<Part>,
    vars: Vec<String>,
    text: String,
}

#[derive(Clone, Debug, PartialEq)]
enum Part {
    Lit(String),
    Expr(Op, Vec<VarSpec>),
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Op {
    Simple,
    Reserved,
    Fragment,
    Label,
    Path,
    Param,
    Query,
    Continuation,
}

#[derive(Clone, Debug, PartialEq)]
struct VarSpec {
    var: usize,
    prefix: Option<usize>,
    explode: bool,
}

/// The value of a template variable.
#[derive(Clone, Debug)]
pub enum Value<'a> {
    Undef,
    Str(Cow<'a, str>),
    List(&'a [String]),
}

impl Op {
    /// (first, separator, named, if-empty, reserved characters allowed)
    fn rules(self) -> (&'static str, &'static str, bool, &'static str, bool) {
        match self {
            Op::Simple => ("", ",", false, "", false),
            Op::Reserved => ("", ",", false, "", true),
            Op::Fragment => ("#", ",", false, "", true),
            Op::Label => (".", ".", false, "", false),
            Op::Path => ("/", "/", false, "", false),
            Op::Param => (";", ";", true, "", false),
            Op::Query => ("?", "&", true, "=", false),
            Op::Continuation => ("&", "&", true, "=", false),
        }
    }
}

impl UriTemplate {
    pub fn parse(text: &str) -> Result<UriTemplate, String> {
        let mut parts = Vec::new();
        let mut vars: Vec<String> = Vec::new();
        let mut rest = text;
        while !rest.is_empty() {
            match rest.find('{') {
                None => {
                    if rest.contains('}') {
                        return Err(format!("unmatched '}}' in URI template {text:?}"));
                    }
                    parts.push(Part::Lit(rest.to_string()));
                    break;
                }
                Some(i) => {
                    if rest[..i].contains('}') {
                        return Err(format!("unmatched '}}' in URI template {text:?}"));
                    }
                    if i > 0 {
                        parts.push(Part::Lit(rest[..i].to_string()));
                    }
                    let end = rest[i..]
                        .find('}')
                        .ok_or_else(|| format!("unclosed '{{' in URI template {text:?}"))?;
                    let body = &rest[i + 1..i + end];
                    rest = &rest[i + end + 1..];
                    let (op, list) = match body.chars().next() {
                        Some('+') => (Op::Reserved, &body[1..]),
                        Some('#') => (Op::Fragment, &body[1..]),
                        Some('.') => (Op::Label, &body[1..]),
                        Some('/') => (Op::Path, &body[1..]),
                        Some(';') => (Op::Param, &body[1..]),
                        Some('?') => (Op::Query, &body[1..]),
                        Some('&') => (Op::Continuation, &body[1..]),
                        Some('=' | ',' | '!' | '@' | '|') => {
                            return Err(format!(
                                "reserved operator in URI template expression {{{body}}}"
                            ));
                        }
                        _ => (Op::Simple, body),
                    };
                    let mut specs = Vec::new();
                    for spec in list.split(',') {
                        let (name, prefix, explode) = if let Some(n) = spec.strip_suffix('*') {
                            (n, None, true)
                        } else if let Some((n, len)) = spec.split_once(':') {
                            let len: usize = len
                                .parse()
                                .ok()
                                .filter(|l| (1..10_000).contains(l))
                                .ok_or_else(|| {
                                format!("bad prefix length in URI template expression {{{body}}}")
                            })?;
                            (n, Some(len), false)
                        } else {
                            (spec, None, false)
                        };
                        if name.is_empty() {
                            return Err(format!(
                                "empty variable name in URI template expression {{{body}}}"
                            ));
                        }
                        let var = match vars.iter().position(|v| v == name) {
                            Some(p) => p,
                            None => {
                                vars.push(name.to_string());
                                vars.len() - 1
                            }
                        };
                        specs.push(VarSpec {
                            var,
                            prefix,
                            explode,
                        });
                    }
                    parts.push(Part::Expr(op, specs));
                }
            }
        }
        Ok(UriTemplate {
            parts,
            vars,
            text: text.to_string(),
        })
    }

    /// The template as written.
    pub fn as_str(&self) -> &str {
        &self.text
    }

    /// The distinct variable names, in order of first use.
    pub fn vars(&self) -> &[String] {
        &self.vars
    }

    /// Expand with `value(i)` giving the value of variable `i` of [`vars`](Self::vars).
    pub fn expand<'a>(&self, value: &dyn Fn(usize) -> Value<'a>) -> String {
        let mut out = String::new();
        for part in &self.parts {
            match part {
                Part::Lit(s) => encode_literal(s, &mut out),
                Part::Expr(op, specs) => {
                    let (first, sep, named, ifemp, reserved) = op.rules();
                    let mut started = false;
                    for spec in specs {
                        let v = value(spec.var);
                        let defined = match &v {
                            Value::Undef => false,
                            Value::List(l) => !l.is_empty(),
                            Value::Str(_) => true,
                        };
                        if !defined {
                            continue;
                        }
                        out.push_str(if started { sep } else { first });
                        started = true;
                        let name = &self.vars[spec.var];
                        match v {
                            Value::Undef => {}
                            Value::Str(s) => {
                                if named {
                                    encode(name, true, &mut out);
                                    out.push_str(if s.is_empty() { ifemp } else { "=" });
                                }
                                let s = match spec.prefix {
                                    Some(n) => match s.char_indices().nth(n) {
                                        Some((i, _)) => &s[..i],
                                        None => &s[..],
                                    },
                                    None => &s[..],
                                };
                                encode(s, reserved, &mut out);
                            }
                            Value::List(items) => {
                                if spec.explode {
                                    for (k, item) in items.iter().enumerate() {
                                        if k > 0 {
                                            out.push_str(sep);
                                        }
                                        if named {
                                            encode(name, true, &mut out);
                                            out.push_str(if item.is_empty() { ifemp } else { "=" });
                                        }
                                        encode(item, reserved, &mut out);
                                    }
                                } else {
                                    if named {
                                        encode(name, true, &mut out);
                                        out.push('=');
                                    }
                                    for (k, item) in items.iter().enumerate() {
                                        if k > 0 {
                                            out.push(',');
                                        }
                                        encode(item, reserved, &mut out);
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
        out
    }
}

fn unreserved(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '-' | '.' | '_' | '~')
}

fn reserved_char(c: char) -> bool {
    matches!(
        c,
        ':' | '/'
            | '?'
            | '#'
            | '['
            | ']'
            | '@'
            | '!'
            | '$'
            | '&'
            | '\''
            | '('
            | ')'
            | '*'
            | '+'
            | ','
            | ';'
            | '='
    )
}

fn pct(c: char, out: &mut String) {
    let mut buf = [0u8; 4];
    for b in c.encode_utf8(&mut buf).bytes() {
        let _ = write!(out, "%{b:02X}");
    }
}

/// Percent-encode a value: everything but unreserved characters, and with `reserved`
/// also reserved characters and existing `%XX` triplets, is encoded.
fn encode(s: &str, reserved: bool, out: &mut String) {
    let b = s.as_bytes();
    for (i, c) in s.char_indices() {
        if unreserved(c)
            || (reserved
                && (reserved_char(c)
                    || (c == '%'
                        && b.len() > i + 2
                        && b[i + 1].is_ascii_hexdigit()
                        && b[i + 2].is_ascii_hexdigit())))
        {
            out.push(c);
        } else {
            pct(c, out);
        }
    }
}

/// Literal text is copied, except characters an IRI cannot hold.
fn encode_literal(s: &str, out: &mut String) {
    for c in s.chars() {
        if c.is_control()
            || matches!(
                c,
                ' ' | '"' | '<' | '>' | '\\' | '^' | '`' | '{' | '|' | '}'
            )
        {
            pct(c, out);
        } else {
            out.push(c);
        }
    }
}

/// Percent-decode `s`; invalid escapes and non-UTF-8 results are kept as written.
pub fn decode(s: &str) -> String {
    if !s.contains('%') {
        return s.to_string();
    }
    percent_encoding::percent_decode_str(s)
        .decode_utf8()
        .map(Cow::into_owned)
        .unwrap_or_else(|_| s.to_string())
}

/// Percent-encode everything but unreserved characters (how CSVW makes a column name
/// from a title).
pub fn encode_name(s: &str) -> String {
    let mut out = String::new();
    encode(s, false, &mut out);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The examples of RFC 6570 §3.2.
    #[test]
    fn rfc_6570_examples() {
        let list = vec!["red".to_string(), "green".into(), "blue".into()];
        let vals = |name: &str| -> Value<'_> {
            match name {
                "var" => Value::Str("value".into()),
                "hello" => Value::Str("Hello World!".into()),
                "path" => Value::Str("/foo/bar".into()),
                "empty" => Value::Str("".into()),
                "x" => Value::Str("1024".into()),
                "y" => Value::Str("768".into()),
                "list" => Value::List(&list),
                _ => Value::Undef,
            }
        };
        let cases = [
            ("{var}", "value"),
            ("{hello}", "Hello%20World%21"),
            ("{+hello}", "Hello%20World!"),
            ("{+path}/here", "/foo/bar/here"),
            ("{#hello}", "#Hello%20World!"),
            ("X{.var}", "X.value"),
            ("X{.x,y}", "X.1024.768"),
            ("{/var,x}/here", "/value/1024/here"),
            ("{;x,y,empty}", ";x=1024;y=768;empty"),
            ("{?x,y,empty}", "?x=1024&y=768&empty="),
            ("?fixed=yes{&x}", "?fixed=yes&x=1024"),
            ("{var:3}", "val"),
            ("{list}", "red,green,blue"),
            ("{list*}", "red,green,blue"),
            ("{/list*,path:4}", "/red/green/blue/%2Ffoo"),
            ("{?list*}", "?list=red&list=green&list=blue"),
            ("{;list}", ";list=red,green,blue"),
            ("{undef}x{?undef}", "x"),
            ("{x,undef,y}", "1024,768"),
        ];
        for (t, want) in cases {
            let tpl = UriTemplate::parse(t).unwrap();
            let got = tpl.expand(&|i| vals(&tpl.vars()[i]));
            assert_eq!(got, want, "{t}");
        }
    }

    #[test]
    fn bad_templates() {
        for t in ["{x", "x}", "{}", "{=x}", "{x:0}", "{x:a}"] {
            assert!(UriTemplate::parse(t).is_err(), "{t}");
        }
        // lenient names: CSVW column names need not be RFC 6570 varnames
        let t = UriTemplate::parse("http://e/{first-name}").unwrap();
        assert_eq!(t.vars(), ["first-name"]);
        assert_eq!(
            t.expand(&|_| Value::Str("Zoë A".into())),
            "http://e/Zo%C3%AB%20A"
        );
    }

    #[test]
    fn names() {
        assert_eq!(encode_name("Person ID"), "Person%20ID");
        assert_eq!(decode("Person%20ID"), "Person ID");
        assert_eq!(decode("100%"), "100%");
    }
}
