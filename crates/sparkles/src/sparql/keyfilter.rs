//! Filters evaluated on vocabulary keys.
//!
//! String and language-tag tests over a single variable (`CONTAINS(?v, "x")`,
//! `REGEX(STR(?v), "…")`, `LANGMATCHES(LANG(?v), "en")`, …) can be answered from the
//! stored key of a term (`"lexical 0xFF @lang`, `<iri`) without building a [`Value`] —
//! and its heap-allocated strings — for every term. Only shapes whose outcome can be read
//! straight off the key are compiled; ids that are not stored terms (inline numbers and
//! dates, blank nodes, unbound) are left to the general evaluator.

use super::expr::{Expr, Func, compatible, compile_regex, lang_matches};
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

enum Test {
    /// `CONTAINS` / `STRSTARTS` / `STRENDS` with a constant second argument
    Str {
        f: Function,
        arg: Arg,
        needle: Arc<str>,
        lang: Option<Arc<str>>,
    },
    Regex {
        arg: Arg,
        re: regex::Regex,
    },
    /// `LANGMATCHES(LANG(?v), "range")`
    LangMatches(Arc<str>),
}

/// A conjunction of key tests over one variable.
pub struct KeyFilter(Vec<Test>);

/// A stored term key, borrowed.
enum Key<'a> {
    Iri(&'a str),
    Literal {
        lex: &'a str,
        /// language tag without the base direction
        lang: Option<&'a str>,
        /// datatype other than `xsd:string` / `rdf:langString`
        typed: bool,
    },
    /// blank node, triple term or undecodable bytes
    Other,
}

fn parse(key: &[u8]) -> Key<'_> {
    let utf8 = |b| std::str::from_utf8(b).ok();
    match key.first() {
        Some(b'<') => utf8(&key[1..]).map_or(Key::Other, Key::Iri),
        Some(b'"') => {
            let sep = key.iter().rposition(|&b| b == KEY_SEP).unwrap_or(key.len());
            let Some(lex) = utf8(&key[1..sep]) else {
                return Key::Other;
            };
            let suffix = key.get(sep + 1..).unwrap_or(&[]);
            match suffix.first() {
                None => Key::Literal {
                    lex,
                    lang: None,
                    typed: false,
                },
                Some(b'@') => {
                    let Some(tag) = utf8(&suffix[1..]) else {
                        return Key::Other;
                    };
                    let lang = match tag.rsplit_once("--") {
                        Some((l, "ltr" | "rtl")) => l,
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
    fn read<'a>(self, key: &Key<'a>) -> Option<(&'a str, Option<&'a str>)> {
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
                        needle,
                        lang,
                    }
                }
                (Function::Regex, [a, p, flags @ ..]) if flags.len() <= 1 => {
                    let flags = match flags {
                        [f] => const_str(f)?,
                        _ => "",
                    };
                    Test::Regex {
                        arg: Arg::of(a, v)?,
                        re: compile_regex(const_str(p)?, flags).ok()?,
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

    /// Whether the term with this key passes (type errors fail the filter).
    pub fn test(&self, key: &[u8]) -> bool {
        let key = parse(key);
        self.0.iter().all(|t| match t {
            Test::Str {
                f,
                arg,
                needle,
                lang,
            } => arg.read(&key).is_some_and(|(s, l)| {
                compatible(l, lang.as_deref())
                    && match f {
                        Function::Contains => s.contains(&**needle),
                        Function::StrStarts => s.starts_with(&**needle),
                        _ => s.ends_with(&**needle),
                    }
            }),
            Test::Regex { arg, re } => arg.read(&key).is_some_and(|(s, _)| re.is_match(s)),
            Test::LangMatches(range) => match &key {
                Key::Literal { lang, .. } => lang_matches(lang.unwrap_or(""), range),
                _ => false,
            },
        })
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
