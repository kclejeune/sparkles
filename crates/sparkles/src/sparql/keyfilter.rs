//! Filters evaluated on vocabulary keys.
//!
//! String and language-tag tests over a single variable (`CONTAINS(?v, "x")`,
//! `REGEX(STR(?v), "…")`, `LANGMATCHES(LANG(?v), "en")`, …) can be answered from the
//! stored key of a term (`"lexical 0xFF @lang`, `<iri`) without building a [`Value`] —
//! and its heap-allocated strings — for every term. Only shapes whose outcome can be read
//! straight off the key are compiled; ids that are not stored terms (inline numbers and
//! dates, blank nodes, unbound) are left to the general evaluator.

use super::expr::{Expr, Func, compile_regex, lang_matches};
use super::table::VarId;
use super::value::Value;
use crate::id::KEY_SEP;
use spargebra::algebra::Function;
use std::sync::Arc;

/// What a test reads from the term.
#[derive(Clone, Copy)]
enum Arg {
    /// `?v` as a string-literal argument (simple, `xsd:string` or language-tagged)
    Term,
    /// `STR(?v)`: the IRI or the lexical form of any literal
    Str,
}

#[derive(Clone)]
enum Test {
    /// `CONTAINS` / `STRSTARTS` / `STRENDS` with a constant second argument
    Str {
        f: Function,
        arg: Arg,
        needle: Arc<str>,
        /// substring search for `CONTAINS`, built once for the needle
        finder: Box<memchr::memmem::Finder<'static>>,
        lang: Option<Arc<str>>,
    },
    Regex {
        arg: Arg,
        re: regex::Regex,
        /// the text every match starts with, for a pattern anchored at the start
        prefix: Option<Arc<str>>,
    },
    /// `LANGMATCHES(LANG(?v), "range")`
    LangMatches(Arc<str>),
}

/// A conjunction of key tests over one variable.
#[derive(Clone)]
pub struct KeyFilter(Vec<Test>);

/// A stored term key split into its parts, borrowed. The parts are not checked to be
/// UTF-8 up front: the byte tests give the answer of the string tests on UTF-8, and a
/// key that passes them all is checked before it counts as a match.
enum Key<'a> {
    Iri(&'a [u8]),
    Literal {
        lex: &'a [u8],
        /// language tag without the base direction
        lang: Option<&'a [u8]>,
        /// datatype other than `xsd:string` / `rdf:langString`
        typed: bool,
    },
    /// blank node or triple term
    Other,
}

fn split(key: &[u8]) -> Key<'_> {
    match key.first() {
        Some(b'<') => Key::Iri(&key[1..]),
        Some(b'"') => {
            // the separator byte never occurs in UTF-8, so the last one ends the lexical form
            let sep = memchr::memrchr(KEY_SEP, key).unwrap_or(key.len());
            let lex = &key[1..sep];
            let suffix = key.get(sep + 1..).unwrap_or(&[]);
            match suffix.first() {
                None => Key::Literal {
                    lex,
                    lang: None,
                    typed: false,
                },
                Some(b'@') => {
                    let tag = &suffix[1..];
                    let lang = match tag.len().checked_sub(5) {
                        Some(i) if tag[i..] == *b"--ltr" || tag[i..] == *b"--rtl" => &tag[..i],
                        _ => tag,
                    };
                    Key::Literal {
                        lex,
                        lang: Some(lang),
                        typed: false,
                    }
                }
                Some(_) => Key::Literal {
                    lex,
                    lang: None,
                    typed: true,
                },
            }
        }
        _ => Key::Other,
    }
}

impl Key<'_> {
    /// Whether the parts the tests read are UTF-8 (a key that is not is no string).
    fn valid(&self) -> bool {
        let ok = |b: &[u8]| std::str::from_utf8(b).is_ok();
        match self {
            Key::Iri(i) => ok(i),
            Key::Literal { lex, lang, .. } => ok(lex) && lang.is_none_or(ok),
            Key::Other => false,
        }
    }
}

impl Arg {
    fn of(e: &Expr, v: VarId) -> Option<Arg> {
        match e {
            Expr::Var(x) if *x == v => Some(Arg::Term),
            Expr::Call(Func::Builtin(Function::Str), a) if matches!(a.as_slice(), [Expr::Var(x)] if *x == v) => {
                Some(Arg::Str)
            }
            _ => None,
        }
    }

    /// The string and language tag the function sees; `None` on a type error.
    fn read<'a>(self, key: &Key<'a>) -> Option<(&'a [u8], Option<&'a [u8]>)> {
        match (self, key) {
            (
                Arg::Term,
                Key::Literal {
                    lex,
                    lang,
                    typed: false,
                },
            ) => Some((lex, *lang)),
            (Arg::Str, Key::Iri(i)) => Some((i, None)),
            (Arg::Str, Key::Literal { lex, .. }) => Some((lex, None)),
            _ => None,
        }
    }
}

/// A constant simple literal / `xsd:string`.
fn const_str(e: &Expr) -> Option<&str> {
    match e {
        Expr::Lit(_, Value::Str(s)) => Some(s),
        _ => None,
    }
}

impl KeyFilter {
    /// Compile filter conjuncts over `v`; `None` unless every conjunct is a supported test.
    pub fn new(exprs: &[Expr], v: VarId) -> Option<KeyFilter> {
        fn add(e: &Expr, v: VarId, out: &mut Vec<Test>) -> Option<()> {
            let Expr::Call(Func::Builtin(f), args) = e else {
                // an error in either side of `&&` makes the filter false, as does `false`
                if let Expr::And(a, b) = e {
                    add(a, v, out)?;
                    return add(b, v, out);
                }
                return None;
            };
            out.push(match (f, args.as_slice()) {
                (Function::Contains | Function::StrStarts | Function::StrEnds, [a, n]) => {
                    let (needle, lang) = match n {
                        Expr::Lit(_, Value::Str(s)) => (s.clone(), None),
                        Expr::Lit(_, Value::Lang(s, l) | Value::LangDir(s, l, _)) => {
                            (s.clone(), Some(l.clone()))
                        }
                        _ => return None,
                    };
                    Test::Str {
                        f: f.clone(),
                        arg: Arg::of(a, v)?,
                        finder: Box::new(memchr::memmem::Finder::new(needle.as_bytes()).into_owned()),
                        needle,
                        lang,
                    }
                }
                (Function::Regex, [a, p, flags @ ..]) if flags.len() <= 1 => {
                    let flags = match flags {
                        [f] => const_str(f)?,
                        _ => "",
                    };
                    let pattern = const_str(p)?;
                    Test::Regex {
                        arg: Arg::of(a, v)?,
                        re: compile_regex(pattern, flags).ok()?,
                        prefix: regex_prefix(pattern, flags).map(Into::into),
                    }
                }
                (Function::LangMatches, [Expr::Call(Func::Builtin(Function::Lang), l), r])
                    if matches!(l.as_slice(), [Expr::Var(x)] if *x == v) =>
                {
                    Test::LangMatches(const_str(r)?.into())
                }
                _ => return None,
            });
            Some(())
        }
        let mut tests = Vec::new();
        for e in exprs {
            add(e, v, &mut tests)?;
        }
        (!tests.is_empty()).then_some(KeyFilter(tests))
    }

    /// Key prefixes that every stored term passing the filter starts with, in key
    /// order: those of the strings a conjunct's `STRSTARTS` or start-anchored `REGEX`
    /// requires (literals for `?v`, literals and IRIs for `STR(?v)`). `None` when no
    /// conjunct fixes a start.
    pub fn key_prefixes(&self) -> Option<Vec<Vec<u8>>> {
        self.0.iter().find_map(|t| {
            let (arg, start): (Arg, &str) = match t {
                Test::Str {
                    f: Function::StrStarts,
                    arg,
                    needle,
                    ..
                } => (*arg, needle),
                Test::Regex {
                    arg,
                    prefix: Some(p),
                    ..
                } => (*arg, p),
                _ => return None,
            };
            if start.is_empty() {
                return None;
            }
            let key = |first: u8| [&[first], start.as_bytes()].concat();
            Some(match arg {
                Arg::Term => vec![key(b'"')],
                Arg::Str => vec![key(b'"'), key(b'<')],
            })
        })
    }

    /// Whether the term with this key passes (type errors fail the filter).
    pub fn test(&self, key: &[u8]) -> bool {
        let key = split(key);
        // UTF-8 is self-synchronizing: a byte match of a UTF-8 needle in a UTF-8 string
        // is a match of the strings
        self.0.iter().all(|t| match t {
            Test::Str {
                f,
                arg,
                needle,
                finder,
                lang,
            } => arg.read(&key).is_some_and(|(s, l)| {
                compatible_bytes(l, lang.as_deref())
                    && match f {
                        Function::Contains => finder.find(s).is_some(),
                        Function::StrStarts => s.starts_with(needle.as_bytes()),
                        _ => s.ends_with(needle.as_bytes()),
                    }
            }),
            Test::Regex { arg, re, .. } => arg
                .read(&key)
                .and_then(|(s, _)| std::str::from_utf8(s).ok())
                .is_some_and(|s| re.is_match(s)),
            Test::LangMatches(range) => match &key {
                Key::Literal { lang: None, .. } => lang_matches("", range),
                Key::Literal { lang: Some(l), .. } => {
                    std::str::from_utf8(l).is_ok_and(|l| lang_matches(l, range))
                }
                _ => false,
            },
        }) && key.valid()
    }
}

/// The text every match of an XPath `pattern` starts with, when the pattern is anchored
/// at the start (`^abc…`) and has no alternation. Flags other than `s` give none: `i`
/// folds case, `m` anchors at every line, `x` drops spaces and `q` reads the pattern as
/// text.
fn regex_prefix(pattern: &str, flags: &str) -> Option<String> {
    if !flags.chars().all(|c| c == 's') || pattern.contains('|') {
        return None;
    }
    let mut chars = pattern.strip_prefix('^')?.chars().peekable();
    let mut out = String::new();
    while let Some(&c) = chars.peek() {
        if !(c.is_alphanumeric() || " /:-_#@%=,;'\"<>!~&".contains(c)) {
            break;
        }
        chars.next();
        // a quantifier that allows no occurrence applies to the character before it
        if matches!(chars.peek(), Some('?' | '*' | '{')) {
            break;
        }
        out.push(c);
    }
    (!out.is_empty()).then_some(out)
}

/// [`super::expr::compatible`] on the bytes of a language tag: the needle's tag, if it
/// has one, must equal the string's ignoring ASCII case.
fn compatible_bytes(a: Option<&[u8]>, b: Option<&str>) -> bool {
    match (a, b) {
        (_, None) => true,
        (Some(x), Some(y)) => x.eq_ignore_ascii_case(y.as_bytes()),
        (None, Some(_)) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::id::term_key;
    use oxrdf::{Literal, NamedNode, Term};

    fn lit(s: &str) -> Expr {
        Expr::Lit(crate::id::Id::UNDEF, Value::Str(s.into()))
    }
    fn call(f: Function, args: Vec<Expr>) -> Expr {
        Expr::Call(Func::Builtin(f), args)
    }
    fn key(t: Term) -> Vec<u8> {
        term_key(&t)
    }

    #[test]
    fn string_tests_follow_argument_rules() {
        let v = 0;
        let f = KeyFilter::new(
            &[call(Function::Contains, vec![Expr::Var(v), lit("da")])],
            v,
        )
        .unwrap();
        assert!(f.test(&key(Literal::new_simple_literal("Ada").into())));
        assert!(f.test(&key(
            Literal::new_language_tagged_literal_unchecked("Ada", "en").into()
        )));
        // typed literals and IRIs are not string literals
        assert!(!f.test(&key(
            Literal::new_typed_literal("Ada", NamedNode::new_unchecked("http://x/dt")).into()
        )));
        assert!(!f.test(&key(NamedNode::new_unchecked("http://x/Ada").into())));

        // STR() reads IRIs and the lexical form of typed literals
        let str_v = call(Function::Str, vec![Expr::Var(v)]);
        let f = KeyFilter::new(&[call(Function::StrEnds, vec![str_v, lit("Ada")])], v).unwrap();
        assert!(f.test(&key(NamedNode::new_unchecked("http://x/Ada").into())));
        assert!(f.test(&key(
            Literal::new_typed_literal("Ada", NamedNode::new_unchecked("http://x/dt")).into()
        )));

        // a language-tagged needle requires a compatible tag
        let en = Expr::Lit(crate::id::Id::UNDEF, Value::Lang("A".into(), "en".into()));
        let f = KeyFilter::new(&[call(Function::StrStarts, vec![Expr::Var(v), en])], v).unwrap();
        assert!(f.test(&key(
            Literal::new_language_tagged_literal_unchecked("Ada", "EN").into()
        )));
        assert!(!f.test(&key(Literal::new_simple_literal("Ada").into())));
    }

    #[test]
    fn prefixes_of_anchored_patterns() {
        assert_eq!(
            regex_prefix("^http://x/a1", "").as_deref(),
            Some("http://x/a1")
        );
        assert_eq!(regex_prefix("^ab?c", "").as_deref(), Some("a"));
        assert_eq!(regex_prefix("^ab*", "s").as_deref(), Some("a"));
        assert_eq!(regex_prefix("^ab{0,2}", "").as_deref(), Some("a"));
        assert_eq!(regex_prefix("^ab+", "").as_deref(), Some("ab"));
        assert_eq!(regex_prefix("^a.c", "").as_deref(), Some("a"));
        assert_eq!(regex_prefix("^ünï[0-9]", "").as_deref(), Some("ünï"));
        for (p, f) in [
            ("^abc", "i"),
            ("^abc", "m"),
            ("^a b", "x"),
            ("^abc", "q"),
            ("^ab|cd", ""),
            ("abc", ""),
            ("^a?b", ""),
            ("^[ab]", ""),
            ("^\\d", ""),
        ] {
            assert_eq!(regex_prefix(p, f), None, "{p} {f}");
        }
        let v = 0;
        let str_v = call(Function::Str, vec![Expr::Var(v)]);
        let f = KeyFilter::new(&[call(Function::StrStarts, vec![str_v, lit("ab")])], v).unwrap();
        assert_eq!(
            f.key_prefixes(),
            Some(vec![b"\"ab".to_vec(), b"<ab".to_vec()])
        );
        let f = KeyFilter::new(
            &[call(
                Function::Regex,
                vec![Expr::Var(v), lit("^ab"), lit("s")],
            )],
            v,
        )
        .unwrap();
        assert_eq!(f.key_prefixes(), Some(vec![b"\"ab".to_vec()]));
        let f = KeyFilter::new(
            &[call(Function::Contains, vec![Expr::Var(v), lit("ab")])],
            v,
        )
        .unwrap();
        assert_eq!(f.key_prefixes(), None);
    }

    #[test]
    fn lang_and_regex() {
        let v = 3;
        let lang = call(Function::Lang, vec![Expr::Var(v)]);
        let re = call(Function::Regex, vec![Expr::Var(v), lit("^a"), lit("i")]);
        let f = KeyFilter::new(
            &[Expr::And(
                Box::new(call(Function::LangMatches, vec![lang, lit("en")])),
                Box::new(re),
            )],
            v,
        )
        .unwrap();
        assert!(f.test(&key(
            Literal::new_language_tagged_literal_unchecked("Ada", "en-GB").into()
        )));
        assert!(!f.test(&key(
            Literal::new_language_tagged_literal_unchecked("Bob", "en").into()
        )));
        assert!(!f.test(&key(
            Literal::new_language_tagged_literal_unchecked("Ada", "de").into()
        )));
        assert!(!f.test(&key(Literal::new_simple_literal("Ada").into())));

        // other variables, OR and non-constant arguments are not compiled
        assert!(
            KeyFilter::new(&[call(Function::Contains, vec![Expr::Var(1), lit("a")])], v).is_none()
        );
        assert!(
            KeyFilter::new(
                &[Expr::Or(
                    Box::new(call(Function::Contains, vec![Expr::Var(v), lit("a")])),
                    Box::new(call(Function::Contains, vec![Expr::Var(v), lit("b")])),
                )],
                v
            )
            .is_none()
        );
    }
}
