//! Expressions and functions, SPARQL 1.1 section 17 and the SPARQL 1.2 additions.
//! An expression error is [`Fail::Type`], which the algebra turns into an unbound
//! variable or a dropped solution where it says so.

use super::value::*;
use super::{Cx, Evaluator, Fail, R, Solution, merge, row_variable, unsupported};
use oxrdf::vocab::{rdf, xsd};
use oxrdf::{BlankNode, Literal, NamedNode, Term, Triple};
use oxsdatatypes::{Boolean, DateTime, Decimal, Double, Float};
use regex::RegexBuilder;
use spargebra::algebra::{Expression, Function};
use std::cmp::Ordering;
use std::collections::HashMap;
use std::str::FromStr;

fn type_error<T>() -> R<T> {
    Err(Fail::Type)
}

fn lit(t: &Term) -> R<&Literal> {
    match t {
        Term::Literal(l) => Ok(l),
        _ => type_error(),
    }
}

/// A string literal: `xsd:string` or language-tagged.
fn string_arg(t: &Term) -> R<&Literal> {
    let l = lit(t)?;
    if is_string_literal(l) {
        Ok(l)
    } else {
        type_error()
    }
}

/// An `xsd:string` literal without a language.
fn simple_arg(t: &Term) -> R<&Literal> {
    let l = lit(t)?;
    if is_simple(l) { Ok(l) } else { type_error() }
}

/// A literal with the language and direction of `src` and the value `s`.
fn like(src: &Literal, s: String) -> Term {
    Term::Literal(match (src.language(), src.direction()) {
        (Some(l), Some(d)) => Literal::new_directional_language_tagged_literal_unchecked(s, l, d),
        (Some(l), None) => Literal::new_language_tagged_literal_unchecked(s, l),
        _ => Literal::new_simple_literal(s),
    })
}

/// Argument compatibility of SPARQL 1.1 section 17.4.3, with the base direction that
/// SPARQL 1.2 adds to it.
fn compatible_strings(a: &Literal, b: &Literal) -> bool {
    match (a.language(), b.language()) {
        (None, None) => true,
        (Some(x), Some(y)) => x == y && a.direction() == b.direction(),
        (Some(_), None) => true,
        (None, Some(_)) => false,
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn half_up(x: f64) -> f64 {
    if !x.is_finite() {
        return x;
    }
    let f = x.floor();
    if x - f >= 0.5 { f + 1.0 } else { f }
}

impl Evaluator<'_> {
    /// [`compatible_strings`], noting where only the base direction makes two arguments
    /// incompatible, and taking them as compatible when the reference is set to ignore
    /// the direction, as [`super::Notes::direction_in_compatibility`] describes.
    fn compatible_arguments(&self, a: &Literal, b: &Literal) -> bool {
        if compatible_strings(a, b) {
            return true;
        }
        let same_language = a.language().is_some() && a.language() == b.language();
        if !same_language {
            return false;
        }
        let mut notes = self.notes.get();
        notes.direction_in_compatibility = true;
        self.notes.set(notes);
        self.data.ignore_direction_in_compatibility.get()
    }

    fn rand_f64(&self) -> f64 {
        let mut x = self.rng.get();
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.rng.set(x);
        (x >> 11) as f64 / (1u64 << 53) as f64
    }

    pub(crate) fn expr(&self, e: &Expression, cx: &Cx, s: &Solution) -> R<Term> {
        use Expression as E;
        match e {
            E::NamedNode(n) => Ok(Term::NamedNode(n.clone())),
            E::Literal(l) => Ok(Term::Literal(l.clone())),
            E::Variable(v) => {
                if self.open_vars.borrow().contains(v) {
                    self.read_open.set(true);
                }
                s.get(v)
                    .or_else(|| cx.env.get(v))
                    .cloned()
                    .ok_or(Fail::Type)
            }
            E::Or(a, b) => {
                let x = self.soft(self.expr(a, cx, s))?.and_then(|t| ebv(&t));
                let y = self.soft(self.expr(b, cx, s))?.and_then(|t| ebv(&t));
                match (x, y) {
                    (Some(true), _) | (_, Some(true)) => Ok(bool_term(true)),
                    (Some(false), Some(false)) => Ok(bool_term(false)),
                    _ => type_error(),
                }
            }
            E::And(a, b) => {
                let x = self.soft(self.expr(a, cx, s))?.and_then(|t| ebv(&t));
                let y = self.soft(self.expr(b, cx, s))?.and_then(|t| ebv(&t));
                match (x, y) {
                    (Some(false), _) | (_, Some(false)) => Ok(bool_term(false)),
                    (Some(true), Some(true)) => Ok(bool_term(true)),
                    _ => type_error(),
                }
            }
            E::Equal(a, b) => {
                let (a, b) = (self.expr(a, cx, s)?, self.expr(b, cx, s)?);
                Ok(bool_term(rdf_equal(&a, &b)?))
            }
            E::SameTerm(a, b) => {
                let (a, b) = (self.expr(a, cx, s)?, self.expr(b, cx, s)?);
                Ok(bool_term(same_term(&a, &b)))
            }
            E::Greater(a, b) => self.relation(a, b, cx, s, Ordering::is_gt),
            E::GreaterOrEqual(a, b) => self.relation(a, b, cx, s, Ordering::is_ge),
            E::Less(a, b) => self.relation(a, b, cx, s, Ordering::is_lt),
            E::LessOrEqual(a, b) => self.relation(a, b, cx, s, Ordering::is_le),
            E::In(x, list) => {
                let x = self.expr(x, cx, s)?;
                let mut error = false;
                for item in list {
                    match self.soft(self.expr(item, cx, s))? {
                        None => error = true,
                        Some(v) => match rdf_equal(&x, &v) {
                            Ok(true) => return Ok(bool_term(true)),
                            Ok(false) => {}
                            Err(Fail::Type) => error = true,
                            Err(f) => return Err(f),
                        },
                    }
                }
                if error {
                    type_error()
                } else {
                    Ok(bool_term(false))
                }
            }
            E::Add(a, b) => self.arithmetic(a, b, cx, s, Arith::Add),
            E::Subtract(a, b) => self.arithmetic(a, b, cx, s, Arith::Sub),
            E::Multiply(a, b) => self.arithmetic(a, b, cx, s, Arith::Mul),
            E::Divide(a, b) => self.arithmetic(a, b, cx, s, Arith::Div),
            E::UnaryPlus(a) => {
                let t = self.expr(a, cx, s)?;
                Ok(numeric_term(&t).ok_or(Fail::Type)?.to_term())
            }
            E::UnaryMinus(a) => {
                let t = self.expr(a, cx, s)?;
                numeric_term(&t)
                    .and_then(Num::neg)
                    .map(Num::to_term)
                    .ok_or(Fail::Type)
            }
            E::Not(a) => {
                let t = self.expr(a, cx, s)?;
                Ok(bool_term(!ebv(&t).ok_or(Fail::Type)?))
            }
            E::Exists(p) => {
                let env = merge(cx.env, s);
                // the pattern may read the open values that the solution binds
                if env.keys().any(|k| self.open_vars.borrow().contains(k)) {
                    self.read_open.set(true);
                }
                let found = self.eval(
                    p,
                    &Cx {
                        graph: cx.graph,
                        env: &env,
                    },
                )?;
                Ok(bool_term(!found.is_empty()))
            }
            E::Bound(v) => Ok(bool_term(s.contains_key(v) || cx.env.contains_key(v))),
            E::If(c, a, b) => {
                let c = self.expr(c, cx, s)?;
                if ebv(&c).ok_or(Fail::Type)? {
                    self.expr(a, cx, s)
                } else {
                    self.expr(b, cx, s)
                }
            }
            E::Coalesce(list) => {
                for item in list {
                    if let Some(v) = self.soft(self.expr(item, cx, s))? {
                        return Ok(v);
                    }
                }
                type_error()
            }
            E::FunctionCall(f, args) => {
                // values that another evaluation makes otherwise
                if matches!(
                    f,
                    Function::BNode
                        | Function::Rand
                        | Function::Uuid
                        | Function::StrUuid
                        | Function::Now
                ) {
                    self.read_open.set(true);
                }
                if matches!(f, Function::BNode) {
                    self.scope.set(match s.get(&row_variable()) {
                        Some(Term::Literal(l)) => l.value().parse().unwrap_or(0),
                        _ => 0,
                    });
                }
                let mut values = Vec::with_capacity(args.len());
                for a in args {
                    values.push(self.expr(a, cx, s)?);
                }
                self.function(f, &values)
            }
        }
    }

    fn relation(
        &self,
        a: &Expression,
        b: &Expression,
        cx: &Cx,
        s: &Solution,
        test: fn(Ordering) -> bool,
    ) -> R<Term> {
        let (a, b) = (self.expr(a, cx, s)?, self.expr(b, cx, s)?);
        Ok(bool_term(order_compare(&a, &b)?.is_some_and(test)))
    }

    fn arithmetic(
        &self,
        a: &Expression,
        b: &Expression,
        cx: &Cx,
        s: &Solution,
        op: Arith,
    ) -> R<Term> {
        let (a, b) = (self.expr(a, cx, s)?, self.expr(b, cx, s)?);
        let (x, y) = (
            numeric_term(&a).ok_or(Fail::Type)?,
            numeric_term(&b).ok_or(Fail::Type)?,
        );
        x.arith(op, y).map(Num::to_term).ok_or(Fail::Type)
    }

    fn function(&self, f: &Function, a: &[Term]) -> R<Term> {
        use Function as F;
        let arg = |i: usize| a.get(i).ok_or(Fail::Type);
        match f {
            F::Str => match arg(0)? {
                Term::NamedNode(n) => Ok(string_term(n.as_str())),
                Term::Literal(l) => Ok(string_term(l.value())),
                _ => type_error(),
            },
            F::Lang => Ok(string_term(lit(arg(0)?)?.language().unwrap_or(""))),
            F::LangMatches => {
                let (tag, range) = (simple_arg(arg(0)?)?, simple_arg(arg(1)?)?);
                let (tag, range) = (tag.value().to_lowercase(), range.value().to_lowercase());
                let ok = if range == "*" {
                    !tag.is_empty()
                } else {
                    tag == range || tag.starts_with(&format!("{range}-"))
                };
                Ok(bool_term(ok))
            }
            F::Datatype => Ok(Term::NamedNode(lit(arg(0)?)?.datatype().into_owned())),
            F::Iri => match arg(0)? {
                Term::NamedNode(n) => Ok(Term::NamedNode(n.clone())),
                Term::Literal(l) if is_simple(l) => {
                    let resolved = match &self.base {
                        Some(b) => b.resolve(l.value()).map(|i| i.into_inner()),
                        None => oxiri::Iri::parse(l.value().to_string()).map(|i| i.into_inner()),
                    };
                    let iri = resolved.map_err(|_| Fail::Type)?;
                    Ok(Term::NamedNode(
                        NamedNode::new(iri).map_err(|_| Fail::Type)?,
                    ))
                }
                _ => type_error(),
            },
            F::BNode => {
                if a.is_empty() {
                    let n = self.fresh_id();
                    return Ok(Term::BlankNode(BlankNode::new_unchecked(format!(
                        "{}b{n}",
                        self.data.fresh_prefix
                    ))));
                }
                let label = simple_arg(arg(0)?)?.value().to_string();
                let scope = self.scope.get();
                let n = self.bnodes.borrow().len();
                let b = self
                    .bnodes
                    .borrow_mut()
                    .entry((scope, label))
                    .or_insert_with(|| {
                        BlankNode::new_unchecked(format!("{}l{n}", self.data.fresh_prefix))
                    })
                    .clone();
                Ok(Term::BlankNode(b))
            }
            F::Rand => Ok(Num::Dbl(self.rand_f64()).to_term()),
            F::Abs => self.numeric_unary(arg(0)?, Num::abs),
            F::Ceil => self.numeric_unary(arg(0)?, Num::ceil),
            F::Floor => self.numeric_unary(arg(0)?, Num::floor),
            F::Round => self.numeric_unary(arg(0)?, Num::round),
            F::Concat => {
                let mut out = String::new();
                let mut common: Option<Option<(String, Option<oxrdf::BaseDirection>)>> = None;
                for t in a {
                    let l = string_arg(t)?;
                    out.push_str(l.value());
                    let this = l.language().map(|x| (x.to_string(), l.direction()));
                    common = match common {
                        None => Some(this),
                        Some(prev) if prev == this => Some(prev),
                        Some(_) => Some(None),
                    };
                }
                Ok(match common.flatten() {
                    Some((lang, Some(d))) => Term::Literal(
                        Literal::new_directional_language_tagged_literal_unchecked(out, lang, d),
                    ),
                    Some((lang, None)) => {
                        Term::Literal(Literal::new_language_tagged_literal_unchecked(out, lang))
                    }
                    None => string_term(out),
                })
            }
            F::SubStr => {
                let src = string_arg(arg(0)?)?;
                let start = numeric_term(arg(1)?).ok_or(Fail::Type)?.to_f64();
                let length = match a.get(2) {
                    Some(t) => Some(numeric_term(t).ok_or(Fail::Type)?.to_f64()),
                    None => None,
                };
                let first = half_up(start);
                let end = length.map(|l| first + half_up(l));
                let out: String = src
                    .value()
                    .chars()
                    .enumerate()
                    .filter(|(i, _)| {
                        let p = (*i + 1) as f64;
                        p >= first && end.is_none_or(|e| p < e)
                    })
                    .map(|(_, c)| c)
                    .collect();
                Ok(like(src, out))
            }
            F::StrLen => Ok(int_term(
                string_arg(arg(0)?)?.value().chars().count() as i128
            )),
            F::Replace => {
                let src = string_arg(arg(0)?)?;
                let pattern = simple_arg(arg(1)?)?;
                let replacement = simple_arg(arg(2)?)?;
                let flags = match a.get(3) {
                    Some(f) => simple_arg(f)?.value().to_string(),
                    None => String::new(),
                };
                let re = build_regex(pattern.value(), &flags)?;
                if re.is_match("") && re.find("").is_some_and(|m| m.is_empty()) {
                    // fn:replace raises an error for a pattern that matches the empty string
                    return type_error();
                }
                let mut out = String::new();
                let mut last = 0;
                let text = src.value();
                for caps in re.captures_iter(text) {
                    let m = caps.get(0).ok_or(Fail::Type)?;
                    out.push_str(&text[last..m.start()]);
                    if flags.contains('q') {
                        // with the q flag the replacement is literal too
                        out.push_str(replacement.value());
                    } else {
                        expand(replacement.value(), &caps, &mut out)?;
                    }
                    last = m.end();
                }
                out.push_str(&text[last..]);
                Ok(like(src, out))
            }
            F::UCase => {
                let l = string_arg(arg(0)?)?;
                Ok(like(l, l.value().to_uppercase()))
            }
            F::LCase => {
                let l = string_arg(arg(0)?)?;
                Ok(like(l, l.value().to_lowercase()))
            }
            F::EncodeForUri => {
                let l = string_arg(arg(0)?)?;
                let mut out = String::new();
                for b in l.value().bytes() {
                    if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
                        out.push(b as char);
                    } else {
                        out.push_str(&format!("%{b:02X}"));
                    }
                }
                Ok(string_term(out))
            }
            F::Contains | F::StrStarts | F::StrEnds => {
                let (x, y) = (string_arg(arg(0)?)?, string_arg(arg(1)?)?);
                if !self.compatible_arguments(x, y) {
                    return type_error();
                }
                let (x, y) = (x.value(), y.value());
                Ok(bool_term(match f {
                    F::Contains => x.contains(y),
                    F::StrStarts => x.starts_with(y),
                    _ => x.ends_with(y),
                }))
            }
            F::StrBefore | F::StrAfter => {
                let (x, y) = (string_arg(arg(0)?)?, string_arg(arg(1)?)?);
                if !self.compatible_arguments(x, y) {
                    return type_error();
                }
                let (text, needle) = (x.value(), y.value());
                Ok(match text.find(needle) {
                    None => string_term(""),
                    Some(i) if matches!(f, F::StrBefore) => like(x, text[..i].to_string()),
                    Some(i) => like(x, text[i + needle.len()..].to_string()),
                })
            }
            F::Year | F::Month | F::Day | F::Hours | F::Minutes | F::Seconds => {
                let d = date_time(lit(arg(0)?)?).ok_or(Fail::Type)?;
                Ok(match f {
                    F::Year => int_term(d.year() as i128),
                    F::Month => int_term(d.month() as i128),
                    F::Day => int_term(d.day() as i128),
                    F::Hours => int_term(d.hour() as i128),
                    F::Minutes => int_term(d.minute() as i128),
                    _ => Num::Dec(d.second()).to_term(),
                })
            }
            F::Timezone => {
                let d = date_time(lit(arg(0)?)?).ok_or(Fail::Type)?;
                let tz = d.timezone().ok_or(Fail::Type)?;
                Ok(Term::Literal(Literal::new_typed_literal(
                    tz.to_string(),
                    xsd::DAY_TIME_DURATION,
                )))
            }
            F::Tz => {
                let d = date_time(lit(arg(0)?)?).ok_or(Fail::Type)?;
                Ok(string_term(
                    d.timezone_offset()
                        .map(|o| o.to_string())
                        .unwrap_or_default(),
                ))
            }
            F::Now => Ok(Term::Literal(Literal::new_typed_literal(
                self.now.to_string(),
                xsd::DATE_TIME,
            ))),
            F::Uuid => Ok(Term::NamedNode(NamedNode::new_unchecked(format!(
                "urn:uuid:{}",
                uuid::Uuid::new_v4()
            )))),
            F::StrUuid => Ok(string_term(uuid::Uuid::new_v4().to_string())),
            F::Md5 | F::Sha1 | F::Sha256 | F::Sha384 | F::Sha512 => {
                use md5::Digest;
                let text = simple_arg(arg(0)?)?.value().as_bytes();
                Ok(string_term(match f {
                    F::Md5 => hex(&md5::Md5::digest(text)),
                    F::Sha1 => hex(&sha1::Sha1::digest(text)),
                    F::Sha256 => hex(&sha2::Sha256::digest(text)),
                    F::Sha384 => hex(&sha2::Sha384::digest(text)),
                    _ => hex(&sha2::Sha512::digest(text)),
                }))
            }
            F::StrLang => {
                let (s, l) = (simple_arg(arg(0)?)?, simple_arg(arg(1)?)?);
                Literal::new_language_tagged_literal(s.value(), l.value())
                    .map(Term::Literal)
                    .map_err(|_| Fail::Type)
            }
            F::StrDt => {
                let s = simple_arg(arg(0)?)?;
                match arg(1)? {
                    Term::NamedNode(d) => Ok(Term::Literal(Literal::new_typed_literal(
                        s.value(),
                        d.clone(),
                    ))),
                    _ => type_error(),
                }
            }
            F::IsIri => Ok(bool_term(matches!(arg(0)?, Term::NamedNode(_)))),
            F::IsBlank => Ok(bool_term(matches!(arg(0)?, Term::BlankNode(_)))),
            F::IsLiteral => Ok(bool_term(matches!(arg(0)?, Term::Literal(_)))),
            F::IsNumeric => Ok(bool_term(numeric_term(arg(0)?).is_some())),
            F::Regex => {
                let text = string_arg(arg(0)?)?;
                let pattern = simple_arg(arg(1)?)?;
                let flags = match a.get(2) {
                    Some(f) => simple_arg(f)?.value().to_string(),
                    None => String::new(),
                };
                let re = build_regex(pattern.value(), &flags)?;
                Ok(bool_term(re.is_match(text.value())))
            }
            F::Triple => {
                let subject = match arg(0)? {
                    Term::NamedNode(n) => oxrdf::NamedOrBlankNode::from(n.clone()),
                    Term::BlankNode(b) => oxrdf::NamedOrBlankNode::from(b.clone()),
                    _ => return type_error(),
                };
                let predicate = match arg(1)? {
                    Term::NamedNode(n) => n.clone(),
                    _ => return type_error(),
                };
                Ok(Term::Triple(Box::new(Triple::new(
                    subject,
                    predicate,
                    arg(2)?.clone(),
                ))))
            }
            F::Subject | F::Predicate | F::Object => match arg(0)? {
                Term::Triple(t) => Ok(match f {
                    F::Subject => Term::from(t.subject.clone()),
                    F::Predicate => Term::NamedNode(t.predicate.clone()),
                    _ => t.object.clone(),
                }),
                _ => type_error(),
            },
            F::IsTriple => Ok(bool_term(matches!(arg(0)?, Term::Triple(_)))),
            F::LangDir => {
                let l = lit(arg(0)?)?;
                Ok(string_term(match l.direction() {
                    Some(oxrdf::BaseDirection::Ltr) => "ltr",
                    Some(oxrdf::BaseDirection::Rtl) => "rtl",
                    None => "",
                }))
            }
            F::HasLang => Ok(bool_term(
                matches!(arg(0)?, Term::Literal(l) if l.language().is_some()),
            )),
            F::HasLangDir => Ok(bool_term(
                matches!(arg(0)?, Term::Literal(l) if l.direction().is_some()),
            )),
            F::StrLangDir => {
                let (s, l, d) = (
                    simple_arg(arg(0)?)?,
                    simple_arg(arg(1)?)?,
                    simple_arg(arg(2)?)?,
                );
                let dir = match d.value() {
                    "ltr" => oxrdf::BaseDirection::Ltr,
                    "rtl" => oxrdf::BaseDirection::Rtl,
                    _ => return type_error(),
                };
                Literal::new_directional_language_tagged_literal(s.value(), l.value(), dir)
                    .map(Term::Literal)
                    .map_err(|_| Fail::Type)
            }
            F::Custom(name) => self.cast(name, a),
        }
    }

    fn numeric_unary(&self, t: &Term, f: fn(Num) -> Option<Num>) -> R<Term> {
        numeric_term(t)
            .and_then(f)
            .map(Num::to_term)
            .ok_or(Fail::Type)
    }

    /// The `xsd` constructor functions of SPARQL 1.1 section 17.5.
    fn cast(&self, name: &NamedNode, a: &[Term]) -> R<Term> {
        let target = name.as_str();
        let known = [
            xsd::STRING,
            xsd::BOOLEAN,
            xsd::INTEGER,
            xsd::DECIMAL,
            xsd::FLOAT,
            xsd::DOUBLE,
            xsd::DATE_TIME,
        ];
        if !known.iter().any(|k| k.as_str() == target) || a.len() != 1 {
            return unsupported(format!("the function {name}"));
        }
        let t = &a[0];
        // the lexical form that a cast reads, and the numeric value if it has one
        let (lexical, num, boolean_value): (String, Option<Num>, Option<bool>) = match t {
            Term::Literal(l) => (l.value().to_string(), numeric(l), boolean(l)),
            Term::NamedNode(n) if target == xsd::STRING.as_str() => {
                return Ok(string_term(n.as_str()));
            }
            _ => return type_error(),
        };
        let l = lit(t)?;
        let well_typed_source = is_string_literal(l)
            || num.is_some()
            || boolean_value.is_some()
            || date_time(l).is_some();
        if target == xsd::STRING.as_str() {
            return Ok(string_term(match (num, boolean_value) {
                (Some(n), _) => n.xpath_string(),
                (_, Some(b)) => b.to_string(),
                _ => lexical,
            }));
        }
        if !well_typed_source {
            return type_error();
        }
        let from_string = is_simple(l);
        if target == xsd::BOOLEAN.as_str() {
            return Ok(bool_term(match (boolean_value, num, from_string) {
                (Some(b), _, _) => b,
                (_, Some(n), _) => n.truthy(),
                (_, _, true) => {
                    bool::from(Boolean::from_str(lexical.trim()).map_err(|_| Fail::Type)?)
                }
                _ => return type_error(),
            }));
        }
        if target == xsd::DATE_TIME.as_str() {
            return match (date_time(l), from_string) {
                (Some(d), _) => Ok(Term::Literal(Literal::new_typed_literal(
                    d.to_string(),
                    xsd::DATE_TIME,
                ))),
                (_, true) => DateTime::from_str(lexical.trim())
                    .map(|d| {
                        Term::Literal(Literal::new_typed_literal(d.to_string(), xsd::DATE_TIME))
                    })
                    .map_err(|_| Fail::Type),
                _ => type_error(),
            };
        }
        // numeric targets
        let source = match (num, boolean_value, from_string) {
            (Some(n), _, _) => n,
            (_, Some(b), _) => Num::Int(b as i128),
            (_, _, true) => parse_untyped_number(lexical.trim(), target)?,
            _ => return type_error(),
        };
        if target == xsd::INTEGER.as_str() {
            return match source {
                Num::Int(i) => Ok(int_term(i)),
                Num::Dec(d) => d
                    .to_string()
                    .split('.')
                    .next()
                    .and_then(|s| s.parse::<i128>().ok())
                    .map(int_term)
                    .ok_or(Fail::Type),
                other => {
                    let v = other.to_f64();
                    if v.is_finite() && v.abs() < 1e30 {
                        Ok(int_term(v.trunc() as i128))
                    } else {
                        type_error()
                    }
                }
            };
        }
        if target == xsd::DECIMAL.as_str() {
            return match source {
                Num::Dec(d) => Ok(Num::Dec(d).to_term()),
                Num::Int(i) => Decimal::try_from(i)
                    .map(|d| Num::Dec(d).to_term())
                    .map_err(|_| Fail::Type),
                other => Decimal::try_from(Double::from(other.to_f64()))
                    .map(|d| Num::Dec(d).to_term())
                    .map_err(|_| Fail::Type),
            };
        }
        if target == xsd::FLOAT.as_str() {
            return Ok(Num::Flt(match source {
                Num::Flt(f) => f,
                other => other.to_f64() as f32,
            })
            .to_term());
        }
        Ok(Num::Dbl(source.to_f64()).to_term())
    }
}

/// The number that a string names in the lexical space of the target type.
fn parse_untyped_number(lex: &str, target: &str) -> R<Num> {
    if target == xsd::INTEGER.as_str() {
        let digits = lex.strip_prefix(['+', '-']).unwrap_or(lex);
        if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
            return type_error();
        }
        return lex.parse().map(Num::Int).map_err(|_| Fail::Type);
    }
    if target == xsd::DECIMAL.as_str() {
        if lex.contains(['e', 'E']) {
            return type_error();
        }
        return Decimal::from_str(lex).map(Num::Dec).map_err(|_| Fail::Type);
    }
    if !is_float_lexical(lex) {
        return type_error();
    }
    if target == xsd::FLOAT.as_str() {
        return Float::from_str(lex)
            .map(|f| Num::Flt(f32::from(f)))
            .map_err(|_| Fail::Type);
    }
    Double::from_str(lex)
        .map(|d| Num::Dbl(f64::from(d)))
        .map_err(|_| Fail::Type)
}

/// Build the regular expression of the XPath flags `s`, `m`, `i`, `x` and `q`, once per
/// thread for each pattern and flags, since a filter builds the same one for every row.
/// An invalid pattern is a type error, and a valid one that the evaluator cannot build,
/// such as one with a back-reference, is unsupported.
fn build_regex(pattern: &str, flags: &str) -> R<regex::Regex> {
    thread_local! {
        static BUILT: std::cell::RefCell<HashMap<(String, String), R<regex::Regex>>> =
            std::cell::RefCell::new(HashMap::new());
    }
    let key = (pattern.to_owned(), flags.to_owned());
    if let Some(built) = BUILT.with(|b| b.borrow().get(&key).cloned()) {
        return built;
    }
    let built = compile_regex(pattern, flags);
    BUILT.with(|b| b.borrow_mut().insert(key, built.clone()));
    built
}

fn compile_regex(pattern: &str, flags: &str) -> R<regex::Regex> {
    for c in flags.chars() {
        if !"smixq".contains(c) {
            return type_error();
        }
    }
    let translated = if flags.contains('q') {
        regex::escape(pattern)
    } else {
        xpath_regex(pattern, flags.contains('s'), flags.contains('x'))?
    };
    let mut b = RegexBuilder::new(&translated);
    b.case_insensitive(flags.contains('i'))
        .multi_line(flags.contains('m'));
    b.build()
        .map_err(|e| Fail::Unsupported(format!("the regular expression {pattern:?}: {e}")))
}

/// The general categories that `\p{..}` names in XML Schema 1.1 regular expressions.
const CATEGORIES: &[&str] = &[
    "L", "Lu", "Ll", "Lt", "Lm", "Lo", "M", "Mn", "Mc", "Me", "N", "Nd", "Nl", "No", "P", "Pc",
    "Pd", "Ps", "Pe", "Pi", "Pf", "Po", "Z", "Zs", "Zl", "Zp", "S", "Sm", "Sc", "Sk", "So", "C",
    "Cc", "Cf", "Co", "Cn",
];

/// An XPath regular expression, XPath and XQuery Functions and Operators 3.1 section
/// 5.6.1, written in the syntax of the `regex` crate with the same meaning. The two
/// differ in `.`, which in XPath matches neither a newline nor a carriage return
/// without the `s` flag, in `\s`, `\w` and `\d`, which XPath defines by its own
/// character sets, in the escapes each accepts, in `&`, `~` and `[` inside a class, and
/// in the `x` flag, which in XPath removes whitespace outside classes and has no comments.
fn xpath_regex(pattern: &str, dot_all: bool, extended: bool) -> R<String> {
    let chars: Vec<char> = pattern.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    let mut groups = 0usize;
    while i < chars.len() {
        let c = chars[i];
        i += 1;
        match c {
            c if extended && matches!(c, ' ' | '\t' | '\n' | '\r') => {}
            '.' => out.push_str(if dot_all { "(?s:.)" } else { "[^\n\r]" }),
            '\\' => {
                let Some(&e) = chars.get(i) else {
                    return type_error();
                };
                i += 1;
                if e.is_ascii_digit() && e != '0' {
                    if (e as usize - '0' as usize) > groups {
                        return type_error();
                    }
                    return unsupported("a back-reference in a regular expression");
                }
                out.push_str(&escape(e, &chars, &mut i)?);
            }
            '(' => {
                if chars.get(i) == Some(&'?') {
                    if chars.get(i + 1) != Some(&':') {
                        return type_error();
                    }
                    i += 2;
                    out.push_str("(?:");
                } else {
                    groups += 1;
                    out.push('(');
                }
            }
            '[' => out.push_str(&class(&chars, &mut i)?),
            ']' => return type_error(),
            '{' => {
                // a quantifier: {n}, {n,} or {n,m}
                let start = i;
                while i < chars.len() && chars[i] != '}' {
                    i += 1;
                }
                if i == chars.len() {
                    return type_error();
                }
                let body: String = chars[start..i].iter().collect();
                i += 1;
                let (n, m) = match body.split_once(',') {
                    Some((n, m)) => (n, Some(m)),
                    None => (body.as_str(), None),
                };
                let number = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
                if !number(n) || m.is_some_and(|m| !m.is_empty() && !number(m)) {
                    return type_error();
                }
                out.push('{');
                out.push_str(&body);
                out.push('}');
            }
            '}' => return type_error(),
            '&' | '~' | '-' | '#' => {
                out.push('\\');
                out.push(c);
            }
            c => out.push(c),
        }
    }
    Ok(out)
}

/// A character class from just after its `[` to just after its `]`.
fn class(chars: &[char], i: &mut usize) -> R<String> {
    let mut out = String::from("[");
    if chars.get(*i) == Some(&'^') {
        out.push('^');
        *i += 1;
    }
    let mut first = true;
    while let Some(&c) = chars.get(*i) {
        *i += 1;
        match c {
            ']' if first => return type_error(),
            ']' => {
                out.push(']');
                return Ok(out);
            }
            '-' if chars.get(*i) == Some(&'[') => {
                return unsupported("a character class subtraction in a regular expression");
            }
            '[' => return type_error(),
            '\\' => {
                let Some(&e) = chars.get(*i) else {
                    return type_error();
                };
                *i += 1;
                out.push_str(&escape(e, chars, i)?);
            }
            '&' | '~' => {
                out.push('\\');
                out.push(c);
            }
            c => out.push(c),
        }
        first = false;
    }
    type_error()
}

/// The escape `\e`, after which `i` is just past it. A set it names is a nested class,
/// which the regex crate also takes inside a class.
fn escape(e: char, chars: &[char], i: &mut usize) -> R<String> {
    let set = |s: &str| format!("[{s}]");
    Ok(match e {
        'n' => "\\n".into(),
        'r' => "\\r".into(),
        't' => "\\t".into(),
        '\\' | '|' | '.' | '?' | '*' | '+' | '(' | ')' | '{' | '}' | '-' | '[' | ']' | '^'
        | '$' => format!("\\{e}"),
        's' => set("\\x20\\t\\n\\r"),
        'S' => set("^\\x20\\t\\n\\r"),
        'd' => "\\p{Nd}".into(),
        'D' => "\\P{Nd}".into(),
        'w' => set("^\\p{P}\\p{Z}\\p{C}"),
        'W' => set("\\p{P}\\p{Z}\\p{C}"),
        'i' | 'I' | 'c' | 'C' => {
            return unsupported("the XML name escapes \\i and \\c in a regular expression");
        }
        'p' | 'P' => {
            if chars.get(*i) != Some(&'{') {
                return type_error();
            }
            let start = *i + 1;
            let Some(len) = chars[start..].iter().position(|&c| c == '}') else {
                return type_error();
            };
            let name: String = chars[start..start + len].iter().collect();
            *i = start + len + 1;
            if name.starts_with("Is") {
                return unsupported("a Unicode block escape in a regular expression");
            }
            if !CATEGORIES.contains(&name.as_str()) {
                return type_error();
            }
            format!("\\{e}{{{name}}}")
        }
        _ => return type_error(),
    })
}

/// The replacement string of `fn:replace`: `$N` is a group and `\$` and `\\` are escapes.
fn expand(replacement: &str, caps: &regex::Captures, out: &mut String) -> R<()> {
    let mut chars = replacement.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' => match chars.next() {
                Some(e @ ('\\' | '$')) => out.push(e),
                _ => return type_error(),
            },
            '$' => {
                let mut n = String::new();
                while let Some(d) = chars.peek().filter(|d| d.is_ascii_digit()) {
                    n.push(*d);
                    chars.next();
                }
                if n.is_empty() {
                    return type_error();
                }
                let i: usize = n.parse().map_err(|_| Fail::Type)?;
                if let Some(m) = caps.get(i) {
                    out.push_str(m.as_str());
                }
            }
            c => out.push(c),
        }
    }
    Ok(())
}

/// The value space of a literal for `=`: the well-formed values of the types whose
/// equality SPARQL defines, language-tagged strings apart, and everything else.
enum Space {
    Number(Num),
    Text,
    Lang,
    Boolean(bool),
    DateTime(DateTime),
    Date(oxsdatatypes::Date),
    Other,
}

fn space(l: &Literal) -> Space {
    if let Some(n) = numeric(l) {
        Space::Number(n)
    } else if is_simple(l) {
        Space::Text
    } else if l.language().is_some() {
        Space::Lang
    } else if let Some(b) = boolean(l) {
        Space::Boolean(b)
    } else if let Some(d) = date_time(l) {
        Space::DateTime(d)
    } else if let Some(d) = date(l) {
        Space::Date(d)
    } else {
        Space::Other
    }
}

/// RDFterm-equal and the value equalities of the operator mapping of section 17.3.
/// The two literals that are not the same term are an error where one has a datatype
/// that gives it no value or no equality, unless one of them is language-tagged, which
/// no other literal equals. That is how the W3C `open-world` tests read RDFterm-equal.
/// Two well-formed values of different supported types, such as a string and an
/// integer, are not equal, since the note to section 17.4.1.7 makes the error the
/// answer only where the implementation cannot tell whether the values are equivalent.
pub(crate) fn rdf_equal(a: &Term, b: &Term) -> R<bool> {
    match (a, b) {
        (Term::Literal(x), Term::Literal(y)) => match (space(x), space(y)) {
            (Space::Number(m), Space::Number(n)) => Ok(m.compare(n) == Some(Ordering::Equal)),
            (Space::Text, Space::Text) => Ok(x.value() == y.value()),
            (Space::Boolean(m), Space::Boolean(n)) => Ok(m == n),
            (Space::DateTime(m), Space::DateTime(n)) => match m.partial_cmp(&n) {
                Some(o) => Ok(o == Ordering::Equal),
                None => type_error(),
            },
            (Space::Date(m), Space::Date(n)) => match m.partial_cmp(&n) {
                Some(o) => Ok(o == Ordering::Equal),
                None => type_error(),
            },
            (Space::DateTime(_), Space::Date(_)) | (Space::Date(_), Space::DateTime(_)) => {
                Ok(false)
            }
            _ if x == y => Ok(true),
            (Space::Lang, _) | (_, Space::Lang) => Ok(false),
            (Space::Other, _) | (_, Space::Other) => type_error(),
            _ => Ok(false),
        },
        (Term::Triple(_), Term::Triple(_)) => same_value(a, b),
        _ => Ok(a == b),
    }
}

/// sameValue of SPARQL 1.2 section 17.4.2.2, which `=` applies to two triple terms and
/// sameValue applies to their components in turn. Equal terms are the same value, an
/// IRI or a blank node is no other value, and two triple terms are the same value where
/// each pair of components is, with an error in a pair before a false one. Literals are
/// an error where one is ill-typed, two NaN are the same value, and otherwise the value
/// equality of `=` decides.
fn same_value(a: &Term, b: &Term) -> R<bool> {
    if a == b {
        return Ok(true);
    }
    match (a, b) {
        (Term::NamedNode(_) | Term::BlankNode(_), _)
        | (_, Term::NamedNode(_) | Term::BlankNode(_)) => Ok(false),
        (Term::Triple(x), Term::Triple(y)) => {
            let parts = [
                (Term::from(x.subject.clone()), Term::from(y.subject.clone())),
                (
                    Term::NamedNode(x.predicate.clone()),
                    Term::NamedNode(y.predicate.clone()),
                ),
                (x.object.clone(), y.object.clone()),
            ];
            let mut error = false;
            let mut all = true;
            for (p, q) in &parts {
                match same_value(p, q) {
                    Ok(true) => {}
                    Ok(false) => all = false,
                    Err(Fail::Type) => error = true,
                    Err(f) => return Err(f),
                }
            }
            if error { type_error() } else { Ok(all) }
        }
        (Term::Triple(_), _) | (_, Term::Triple(_)) => Ok(false),
        (Term::Literal(x), Term::Literal(y)) => {
            if ill_typed(x) || ill_typed(y) {
                return type_error();
            }
            if numeric(x).is_some_and(Num::is_nan) && numeric(y).is_some_and(Num::is_nan) {
                return Ok(true);
            }
            rdf_equal(a, b)
        }
    }
}

/// Whether a literal has a datatype that the evaluator reads and a lexical form outside
/// it.
fn ill_typed(l: &Literal) -> bool {
    let dt = l.datatype();
    (is_numeric_datatype(dt.as_str()) && numeric(l).is_none())
        || (dt == xsd::BOOLEAN && boolean(l).is_none())
        || (dt == xsd::DATE_TIME && date_time(l).is_none())
        || (dt == xsd::DATE && date(l).is_none())
}

/// The order of the operator mapping for `<`, `>`, `<=` and `>=`. `Ok(None)` is an
/// unordered pair, which a NaN makes, and the comparisons are false for it.
fn order_compare(a: &Term, b: &Term) -> R<Option<Ordering>> {
    let (Term::Literal(x), Term::Literal(y)) = (a, b) else {
        return type_error();
    };
    if let (Some(m), Some(n)) = (numeric(x), numeric(y)) {
        return Ok(m.compare(n));
    }
    if is_simple(x) && is_simple(y) {
        return Ok(Some(x.value().cmp(y.value())));
    }
    if let (Some(m), Some(n)) = (boolean(x), boolean(y)) {
        return Ok(Some(m.cmp(&n)));
    }
    if let (Some(m), Some(n)) = (date_time(x), date_time(y)) {
        return m.partial_cmp(&n).map(Some).ok_or(Fail::Type);
    }
    if let (Some(m), Some(n)) = (date(x), date(y)) {
        return m.partial_cmp(&n).map(Some).ok_or(Fail::Type);
    }
    type_error()
}
