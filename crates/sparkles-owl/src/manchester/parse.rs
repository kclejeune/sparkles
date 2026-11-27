//! A recursive-descent reader of the Manchester syntax over the tokens of [`super::lex`].

use super::lex::{self, Name, Tok};
use super::{BoundedError, ByteBudget, Error, ErrorKind, Found, Span, Vocabulary};
use crate::model::*;
use crate::vocab;
use std::collections::BTreeSet;

type Ce = ClassExpression;
type Ope = ObjectPropertyExpression;
type K = EntityKind;

/// How deeply expressions may nest, as the RDF reader allows.
const MAX_DEPTH: usize = crate::read::MAX_NESTING;
/// Syntax-level cap on nested grouping parentheses so pathological `((...))` inputs are
/// bounded without penalizing semantic depth for parenthesized complements or sub-expressions.
const MAX_SYNTAX_DEPTH: usize = MAX_DEPTH * 2;

/// The words that are part of the grammar of class expressions and data ranges.
pub const KEYWORDS: [&str; 14] = [
    "and", "or", "not", "that", "some", "only", "value", "Self", "min", "max", "exactly",
    "inverse", "true", "false",
];

/// The words that follow a property in a restriction.
const RESTRICTIONS: [&str; 7] = ["some", "only", "value", "Self", "min", "max", "exactly"];

/// The words that separate the two sides of an axiom.
pub const AXIOM_KEYWORDS: [&str; 12] = [
    "SubClassOf",
    "EquivalentTo",
    "DisjointWith",
    "SubPropertyOf",
    "InverseOf",
    "Domain",
    "Range",
    "Type",
    "Types",
    "SameAs",
    "DifferentFrom",
    "SubPropertyChain",
];

/// The characteristics an axiom can open with, and the axiom each makes.
pub const CHARACTERISTICS: [&str; 7] = [
    "Functional",
    "InverseFunctional",
    "Reflexive",
    "Irreflexive",
    "Symmetric",
    "Asymmetric",
    "Transitive",
];

/// A property in a restriction, which can be an object property, a data property or,
/// for an IRI the vocabulary does not know or a punned name, either.
struct Prop {
    object: Option<Ope>,
    data: Option<DataProperty>,
    span: Span,
}

/// Maximum diagnostic message length built when the parser runs in bounded mode.
const BOUNDED_DIAGNOSTIC_MAX_BYTES: usize = 512;
/// Maximum bytes copied from a single name, IRI, or ontology label in bounded diagnostics.
const BOUNDED_SNIPPET_BYTES: usize = 64;

pub struct Parser<'v> {
    toks: Vec<(Tok, Span)>,
    pos: usize,
    /// the end of the tokens this parser reads, which an axiom sets to read one side
    end: usize,
    len: usize,
    v: &'v Vocabulary,
    depth: usize,
    paren_depth: usize,
    budget: Option<ByteBudget>,
    max_error_bytes: Option<usize>,
}

impl<'v> Parser<'v> {
    pub fn new(text: &str, v: &'v Vocabulary) -> Result<Parser<'v>, Error> {
        let toks = lex::tokens(text)?;
        Ok(Self::from_tokens(toks, text.len(), v))
    }

    pub(super) fn from_tokens(toks: Vec<(Tok, Span)>, len: usize, v: &'v Vocabulary) -> Parser<'v> {
        Parser {
            end: toks.len(),
            toks,
            pos: 0,
            len,
            v,
            depth: 0,
            paren_depth: 0,
            budget: None,
            max_error_bytes: None,
        }
    }

    pub fn with_budget(mut self, max_bytes: usize) -> Self {
        self.budget = Some(ByteBudget::new(max_bytes));
        self.max_error_bytes = Some(BOUNDED_DIAGNOSTIC_MAX_BYTES);
        self
    }

    /// Reads with `f` and checks that nothing is left.
    pub fn whole<T>(mut self, f: impl FnOnce(&mut Self) -> Result<T, Error>) -> Result<T, Error> {
        if self.end == 0 {
            return Err(Error::syntax(Span::new(0, self.len), "the text is empty"));
        }
        let out = f(&mut self)?;
        self.done()?;
        Ok(out)
    }

    pub fn whole_bounded<T>(
        mut self,
        f: impl FnOnce(&mut Self) -> Result<T, Error>,
    ) -> Result<(T, usize), BoundedError> {
        if self.end == 0 {
            return Err(BoundedError::Parse {
                error: Error::syntax(Span::new(0, self.len), "the text is empty"),
                used: 0,
            });
        }
        match f(&mut self).and_then(|out| self.done().map(|()| out)) {
            Ok(out) => Ok((out, self.budget.map_or(0, |b| b.used))),
            Err(error) => {
                let used = self.budget.map_or(0, |b| b.used);
                if let Some(b) = self.budget
                    && b.exceeded
                {
                    Err(BoundedError::Budget {
                        used,
                        attempted: b.attempted,
                    })
                } else {
                    Err(BoundedError::Parse { error, used })
                }
            }
        }
    }

    fn done(&self) -> Result<(), Error> {
        if self.pos < self.end {
            let (t, s) = &self.toks[self.pos];
            if let Some(max_bytes) = self.max_error_bytes {
                let mut msg = String::with_capacity(max_bytes.min(160));
                push_tok_described_bounded(&mut msg, t, BOUNDED_SNIPPET_BYTES, max_bytes);
                push_capped(
                    &mut msg,
                    " does not continue the expression; join expressions with and, or or parentheses",
                    max_bytes,
                );
                return Err(Error::syntax(*s, msg));
            }
            let what = match t.word() {
                Some(w) if KEYWORDS.contains(&w) => format!("the keyword '{w}'"),
                _ => format!("'{}'", t.shown()),
            };
            return Err(Error::syntax(
                *s,
                format!(
                    "{what} does not continue the expression; join expressions with and, or or parentheses"
                ),
            ));
        }
        Ok(())
    }

    // ------------------------------------------------------------------- tokens ------

    fn tok(&self, k: usize) -> Option<&Tok> {
        if k < self.end {
            self.toks.get(k).map(|(t, _)| t)
        } else {
            None
        }
    }

    fn peek(&self) -> Option<&Tok> {
        self.tok(self.pos)
    }

    /// The span of the next token, or the empty span at the end of the text.
    fn here(&self) -> Span {
        if self.pos < self.end {
            self.toks[self.pos].1
        } else if self.end < self.toks.len() {
            // the end of one side of an axiom: point at the keyword after it
            self.toks[self.end].1
        } else {
            Span::new(self.len, self.len)
        }
    }

    fn bump(&mut self) -> Option<(Tok, Span)> {
        if self.pos < self.end {
            self.pos += 1;
            Some(self.toks[self.pos - 1].clone())
        } else {
            None
        }
    }

    fn is_word(&self, k: usize, w: &str) -> bool {
        self.tok(k).and_then(Tok::word) == Some(w)
    }

    fn eat_word(&mut self, w: &str) -> bool {
        if self.is_word(self.pos, w) {
            self.pos += 1;
            true
        } else {
            false
        }
    }

    fn expected(&self, what: &str) -> Error {
        if let Some(max_bytes) = self.max_error_bytes {
            let mut msg = String::with_capacity(max_bytes.min(160));
            push_capped(&mut msg, "expected ", max_bytes);
            push_capped(&mut msg, what, max_bytes);
            match self.peek() {
                None => {
                    push_capped(&mut msg, ", but the expression ends", max_bytes);
                }
                Some(t) => {
                    push_capped(&mut msg, ", found ", max_bytes);
                    push_tok_described_bounded(&mut msg, t, BOUNDED_SNIPPET_BYTES, max_bytes);
                }
            }
            return Error::syntax(self.here(), msg);
        }
        match self.peek() {
            None => Error::syntax(
                self.here(),
                format!("expected {what}, but the expression ends"),
            ),
            Some(t) => {
                let found = match t.word() {
                    Some(w) if KEYWORDS.contains(&w) => format!("the keyword '{w}'"),
                    _ => format!("'{}'", t.shown()),
                };
                Error::syntax(self.here(), format!("expected {what}, found {found}"))
            }
        }
    }

    fn parse_iri_checked(&self, span: Span, iri: &str) -> Result<Iri, Error> {
        if let Some(max_bytes) = self.max_error_bytes {
            oxrdf::NamedNode::new(iri).map_err(|e| {
                use std::fmt::Write as _;
                let mut msg = String::with_capacity(max_bytes.min(160));
                push_capped(&mut msg, "invalid IRI <", max_bytes);
                push_snippet(&mut msg, iri, BOUNDED_SNIPPET_BYTES, max_bytes);
                push_capped(&mut msg, ">: ", max_bytes);
                let mut reason = String::new();
                let _ = write!(&mut reason, "{e}");
                push_capped(&mut msg, &reason, max_bytes);
                Error::syntax(span, msg)
            })
        } else {
            crate::model::parse_iri(iri).map_err(|e| Error::syntax(span, e.0))
        }
    }

    fn parse_lang_literal_checked(
        &self,
        span: Span,
        value: String,
        lang: &str,
    ) -> Result<Literal, Error> {
        if let Some(max_bytes) = self.max_error_bytes {
            oxrdf::Literal::new_language_tagged_literal(value, lang)
                .map(Literal)
                .map_err(|e| {
                    use std::fmt::Write as _;
                    let mut msg = String::with_capacity(max_bytes.min(160));
                    push_capped(&mut msg, "invalid language tag '", max_bytes);
                    push_snippet(&mut msg, lang, BOUNDED_SNIPPET_BYTES, max_bytes);
                    push_capped(&mut msg, "': ", max_bytes);
                    let mut reason = String::new();
                    let _ = write!(&mut reason, "{e}");
                    push_capped(&mut msg, &reason, max_bytes);
                    Error::syntax(span, msg)
                })
        } else {
            Literal::lang(value, lang).map_err(|e| Error::syntax(span, e.0))
        }
    }

    fn expect(&mut self, t: Tok, what: &str) -> Result<Span, Error> {
        if self.peek() == Some(&t) {
            Ok(self.bump().expect("peeked").1)
        } else {
            Err(self.expected(what))
        }
    }

    fn nest(&mut self) -> Result<(), Error> {
        self.depth += 1;
        if self.depth > MAX_DEPTH {
            return Err(Error::syntax(
                self.here(),
                format!("the expression nests more than {MAX_DEPTH} deep"),
            ));
        }
        Ok(())
    }

    fn nest_paren(&mut self) -> Result<(), Error> {
        self.paren_depth += 1;
        if self.paren_depth > MAX_SYNTAX_DEPTH {
            return Err(Error::syntax(
                self.here(),
                format!("the expression nests more than {MAX_SYNTAX_DEPTH} parentheses deep"),
            ));
        }
        Ok(())
    }

    /// Whether token `k` is a name that is not a keyword.
    fn is_name(&self, k: usize) -> bool {
        match self.tok(k) {
            Some(Tok::Name(Name::Word(w))) => !KEYWORDS.contains(&w.as_str()),
            Some(Tok::Name(_)) => true,
            _ => false,
        }
    }

    // ------------------------------------------------------------------ entities -----

    /// The entity of one of the kinds `want` that the next token names; `what` says
    /// what the place wants, for messages.
    fn entity(&mut self, want: &[K], what: &str) -> Result<(Entity, Span), Error> {
        if !self.is_name(self.pos) {
            return Err(self.expected(what));
        }
        let (Tok::Name(n), span) = self.bump().expect("a name") else {
            unreachable!("checked")
        };
        let es = self.resolve(&n, span, want, what)?;
        Ok((es.into_iter().next().expect("one at least"), span))
    }

    /// The entities of the kinds `want` that `n` names, all with one IRI, in the order
    /// of `want`. An IRI the vocabulary does not know is taken as the first kind.
    fn resolve(
        &mut self,
        n: &Name,
        span: Span,
        want: &[K],
        what: &str,
    ) -> Result<Vec<Entity>, Error> {
        let found = match self.v.lookup_bounded(n, want, &mut self.budget) {
            Ok(f) => f,
            Err(()) => {
                return Err(Error::syntax(
                    span,
                    "resolved term bytes exceeded the expression budget",
                ));
            }
        };
        match found {
            Found::Entities(es) => {
                let iris: BTreeSet<&str> = es.iter().map(|e| e.iri().as_str()).collect();
                if iris.len() > 1 {
                    if let Some(max_bytes) = self.max_error_bytes {
                        let message =
                            format_ambiguous_bounded(n, &es, iris.len(), self.v, max_bytes);
                        return Err(Error {
                            kind: ErrorKind::Ambiguous,
                            message,
                            span,
                            candidates: Vec::new(),
                        });
                    }
                    let mut cands = es.clone();
                    cands.dedup_by(|a, b| a.iri() == b.iri());
                    let r = crate::ofn::Renderer::new(self.v.prefixes().iter().cloned());
                    let list: Vec<String> = cands
                        .iter()
                        .map(|e| {
                            let mut s =
                                format!("{} ({}", r.iri(e.iri().as_str()), kind_word(e.kind()));
                            if let Some(l) = self.v.label(e.iri().as_str()) {
                                s.push_str(&format!(", labelled '{l}'"));
                            }
                            s.push(')');
                            s
                        })
                        .collect();
                    return Err(Error {
                        kind: ErrorKind::Ambiguous,
                        message: format!(
                            "'{}' names {} entities: {}; write a prefixed name or an IRI",
                            name_text(n),
                            cands.len(),
                            list.join(", ")
                        ),
                        span,
                        candidates: cands.iter().map(|e| self.v.candidate(e)).collect(),
                    });
                }
                Ok(es)
            }
            Found::Unknown { iri, others } => {
                if !others.is_empty() {
                    if let Some(max_bytes) = self.max_error_bytes {
                        let mut kinds: Vec<&str> = Vec::new();
                        for e in &others {
                            let kw = kind_word(e.kind());
                            if !kinds.contains(&kw) {
                                kinds.push(kw);
                            }
                        }
                        let mut msg = String::with_capacity(max_bytes.min(160));
                        push_capped(&mut msg, "'", max_bytes);
                        push_name_bounded(&mut msg, n, BOUNDED_SNIPPET_BYTES, max_bytes);
                        push_capped(&mut msg, "' is ", max_bytes);
                        push_capped(&mut msg, article(kinds[0]), max_bytes);
                        push_capped(&mut msg, " ", max_bytes);
                        push_capped(&mut msg, &join_or(&kinds), max_bytes);
                        push_capped(&mut msg, ", but ", max_bytes);
                        push_capped(&mut msg, what, max_bytes);
                        push_capped(&mut msg, " belongs here", max_bytes);
                        return Err(Error {
                            kind: ErrorKind::Kind,
                            message: msg,
                            span,
                            candidates: Vec::new(),
                        });
                    }
                    let kinds: Vec<&str> = others.iter().map(|e| kind_word(e.kind())).collect();
                    return Err(Error {
                        kind: ErrorKind::Kind,
                        message: format!(
                            "'{}' is {} {}, but {what} belongs here",
                            name_text(n),
                            article(kinds[0]),
                            join_or(&kinds),
                        ),
                        span,
                        candidates: others.iter().map(|e| self.v.candidate(e)).collect(),
                    });
                }
                match iri {
                    Some(iri) => self
                        .parse_iri_checked(span, &iri)
                        .map(|i| vec![Entity::of_kind(want[0], i)]),
                    None => {
                        let message = if let Some(max_bytes) = self.max_error_bytes {
                            let mut msg = String::with_capacity(max_bytes.min(160));
                            push_capped(&mut msg, "no ", max_bytes);
                            push_capped(
                                &mut msg,
                                &join_or(&want.iter().map(|k| kind_word(*k)).collect::<Vec<_>>()),
                                max_bytes,
                            );
                            push_capped(&mut msg, " is named '", max_bytes);
                            push_name_bounded(&mut msg, n, BOUNDED_SNIPPET_BYTES, max_bytes);
                            push_capped(&mut msg, "'", max_bytes);
                            msg
                        } else {
                            format!(
                                "no {} is named '{}'",
                                join_or(&want.iter().map(|k| kind_word(*k)).collect::<Vec<_>>()),
                                name_text(n)
                            )
                        };
                        Err(Error {
                            kind: ErrorKind::Unknown,
                            message,
                            span,
                            candidates: Vec::new(),
                        })
                    }
                }
            }
        }
    }

    pub(super) fn class(&mut self) -> Result<Class, Error> {
        match self.entity(&[K::Class], "a class")? {
            (Entity::Class(c), _) => Ok(c),
            _ => unreachable!("asked for a class"),
        }
    }

    pub(super) fn datatype(&mut self) -> Result<Datatype, Error> {
        match self.entity(&[K::Datatype], "a datatype")? {
            (Entity::Datatype(d), _) => Ok(d),
            _ => unreachable!("asked for a datatype"),
        }
    }

    pub(super) fn object_property(&mut self) -> Result<ObjectProperty, Error> {
        match self.entity(&[K::ObjectProperty], "an object property")? {
            (Entity::ObjectProperty(p), _) => Ok(p),
            _ => unreachable!("asked for an object property"),
        }
    }

    /// An object property expression: a property, `inverse p` or `inverse (p)`.
    pub(super) fn ope(&mut self) -> Result<Ope, Error> {
        if self.eat_word("inverse") {
            let paren = self.peek() == Some(&Tok::LParen);
            if paren {
                self.bump();
            }
            let p = self.object_property()?;
            if paren {
                self.expect(Tok::RParen, "')'")?;
            }
            return Ok(Ope::InverseOf(p));
        }
        Ok(Ope::Property(self.object_property()?))
    }

    /// The property of a restriction, which may be an object or a data property.
    fn prop(&mut self) -> Result<Prop, Error> {
        let start = self.here();
        if self.is_word(self.pos, "inverse") {
            let p = self.ope()?;
            return Ok(Prop {
                object: Some(p),
                data: None,
                span: Span::new(start.start, self.toks[self.pos - 1].1.end),
            });
        }
        if !self.is_name(self.pos) {
            return Err(self.expected("a property"));
        }
        let (Tok::Name(n), span) = self.bump().expect("a name") else {
            unreachable!("checked")
        };
        let es = self.resolve(
            &n,
            span,
            &[K::ObjectProperty, K::DataProperty],
            "a property",
        )?;
        let mut p = Prop {
            object: None,
            data: None,
            span,
        };
        let known = !self.v.kinds(es[0].iri().as_str()).is_empty();
        for e in es {
            match e {
                Entity::ObjectProperty(o) => p.object = Some(Ope::Property(o)),
                Entity::DataProperty(d) => p.data = Some(d),
                _ => {}
            }
        }
        if !known {
            // an IRI the vocabulary does not know: its filler decides
            let iri = p
                .object
                .as_ref()
                .expect("the first kind")
                .named()
                .iri()
                .clone();
            p.data = Some(DataProperty(iri));
        }
        Ok(p)
    }

    // ---------------------------------------------------------- class expressions ----

    /// `conjunction or conjunction …`
    pub fn description(&mut self) -> Result<Ce, Error> {
        self.description_depth().map(|(ce, _)| ce)
    }

    fn description_depth(&mut self) -> Result<(Ce, usize), Error> {
        let (first, first_h) = self.conjunction_depth()?;
        if !self.eat_word("or") {
            return Ok((first, first_h));
        }
        // Do not charge `self.depth` provisionally for booleans: recursion back into
        // nested booleans is already bounded early by `nest_paren` (`MAX_SYNTAX_DEPTH`)
        // and non-collapsing primaries (`nest` at `MAX_DEPTH`), while the exact semantic
        // limit is enforced below after deduplication so `E or E` at height `MAX_DEPTH`
        // still collapses cleanly.
        let mut all = vec![first];
        let mut max_h = first_h;
        loop {
            let (next, h) = self.conjunction_depth()?;
            max_h = max_h.max(h);
            all.push(next);
            if !self.eat_word("or") {
                break;
            }
        }
        let set: BTreeSet<Ce> = all.into_iter().collect();
        let collapsed = set.len() == 1;
        let ce = collapse(set, Ce::ObjectUnionOf);
        let h = if collapsed { max_h } else { max_h + 1 };
        if self.depth + h > MAX_DEPTH {
            return Err(Error::syntax(
                self.here(),
                format!("the expression nests more than {MAX_DEPTH} deep"),
            ));
        }
        Ok((ce, h))
    }

    /// `primary and primary …`, with `that` read as `and`.
    fn conjunction_depth(&mut self) -> Result<(Ce, usize), Error> {
        let (first, first_h) = self.primary_depth()?;
        if !(self.eat_word("and") || self.eat_word("that")) {
            return Ok((first, first_h));
        }
        let mut all = vec![first];
        let mut max_h = first_h;
        loop {
            let (next, h) = self.primary_depth()?;
            max_h = max_h.max(h);
            all.push(next);
            if !(self.eat_word("and") || self.eat_word("that")) {
                break;
            }
        }
        let set: BTreeSet<Ce> = all.into_iter().collect();
        let collapsed = set.len() == 1;
        let ce = collapse(set, Ce::ObjectIntersectionOf);
        let h = if collapsed { max_h } else { max_h + 1 };
        if self.depth + h > MAX_DEPTH {
            return Err(Error::syntax(
                self.here(),
                format!("the expression nests more than {MAX_DEPTH} deep"),
            ));
        }
        Ok((ce, h))
    }

    /// Whether a restriction starts at token `pos`: a property, then a restriction
    /// keyword.
    fn restriction_ahead_at(&self, pos: usize) -> bool {
        let k = if self.is_word(pos, "inverse") {
            if self.tok(pos + 1) == Some(&Tok::LParen) {
                pos + 4
            } else {
                pos + 2
            }
        } else if self.is_name(pos) {
            pos + 1
        } else {
            return false;
        };
        self.tok(k)
            .and_then(Tok::word)
            .is_some_and(|w| RESTRICTIONS.contains(&w))
    }

    /// Whether a restriction starts at the next token: a property, then a restriction
    /// keyword.
    fn restriction_ahead(&self) -> bool {
        self.restriction_ahead_at(self.pos)
    }

    fn primary_depth(&mut self) -> Result<(Ce, usize), Error> {
        if self.peek() == Some(&Tok::LParen) {
            self.nest_paren()?;
            self.bump();
            let res = self.description_depth().and_then(|d| {
                self.expect(Tok::RParen, "')' to close the parenthesis")?;
                Ok(d)
            });
            self.paren_depth -= 1;
            return res;
        }
        self.nest()?;
        let res = (|| {
            if self.eat_word("not") {
                let (inner, h) = self.primary_depth()?;
                Ok((Ce::ObjectComplementOf(Box::new(inner)), h + 1))
            } else if self.peek() == Some(&Tok::LBrace) {
                self.bump();
                let mut is = BTreeSet::new();
                loop {
                    is.insert(self.individual()?);
                    if self.peek() == Some(&Tok::Comma) {
                        self.bump();
                        continue;
                    }
                    self.expect(Tok::RBrace, "',' or '}'")?;
                    break;
                }
                Ok((Ce::ObjectOneOf(is), 1))
            } else if self.restriction_ahead() {
                self.restriction_depth()
            } else if self.is_name(self.pos) {
                Ok((Ce::Class(self.class()?), 1))
            } else {
                Err(self.expected("a class, a restriction, 'not', '(' or '{'"))
            }
        })();
        self.depth -= 1;
        res
    }

    /// Whether a data range rather than a class expression starts at token `k`.
    /// It skips the `not` and `(` that open either kind in a loop, so a long run of them
    /// costs no stack.
    fn data_ahead(&self, mut k: usize) -> bool {
        loop {
            match self.tok(k) {
                Some(Tok::Name(Name::Word(w))) if w == "not" => k += 1,
                Some(Tok::LParen) => k += 1,
                _ => break,
            }
        }
        if self.restriction_ahead_at(k) {
            return false;
        }
        match self.tok(k) {
            Some(Tok::Str { .. } | Tok::Number(_)) => true,
            Some(Tok::Name(Name::Word(w))) if w == "true" || w == "false" => true,
            Some(Tok::LBrace) => self.literal_ahead(k + 1),
            Some(Tok::Name(n)) => {
                self.tok(k + 1) == Some(&Tok::LBracket)
                    || self.v.lookup_kinds(n, &[K::Datatype]).contains(K::Datatype)
                        && !self.v.lookup_kinds(n, &[K::Class]).contains(K::Class)
            }
            _ => false,
        }
    }

    fn literal_ahead(&self, k: usize) -> bool {
        match self.tok(k) {
            Some(Tok::Str { .. } | Tok::Number(_)) => true,
            Some(Tok::Name(Name::Word(w))) => w == "true" || w == "false",
            _ => false,
        }
    }

    /// Whether a filler can start at token `k`.
    fn filler_ahead(&self, k: usize) -> bool {
        match self.tok(k) {
            Some(Tok::LParen | Tok::LBrace) => true,
            Some(Tok::Name(Name::Word(w))) if w == "not" || w == "inverse" => true,
            Some(Tok::Name(_)) => self.is_name(k),
            _ => false,
        }
    }

    fn restriction_depth(&mut self) -> Result<(Ce, usize), Error> {
        let p = self.prop()?;
        let (Tok::Name(Name::Word(kw)), kw_span) = self.bump().expect("a restriction keyword")
        else {
            unreachable!("restriction_ahead checked the keyword")
        };
        // a property of one kind decides; one of both kinds is decided by its filler
        let data_filler = match (&p.object, &p.data) {
            (Some(_), None) => false,
            (None, Some(_)) => true,
            _ => match kw.as_str() {
                "value" => self.literal_ahead(self.pos),
                "Self" => false,
                "min" | "max" | "exactly" => self.data_ahead(self.pos + 1),
                _ => self.data_ahead(self.pos),
            },
        };
        let need_object = |p: &Prop| -> Result<Ope, Error> {
            p.object.clone().ok_or_else(|| Error {
                kind: ErrorKind::Kind,
                message: format!("'{kw}' needs an object property, and this is a data property"),
                span: p.span,
                candidates: Vec::new(),
            })
        };
        if data_filler {
            let d = p.data.clone().expect("a data property");
            return Ok(match kw.as_str() {
                "some" => {
                    let (r, rh) = self.data_primary_depth()?;
                    (Ce::DataSomeValuesFrom(vec![d], r), rh + 1)
                }
                "only" => {
                    let (r, rh) = self.data_primary_depth()?;
                    (Ce::DataAllValuesFrom(vec![d], r), rh + 1)
                }
                "value" => (Ce::DataHasValue(d, self.literal()?), 1),
                "Self" => {
                    return Err(Error {
                        kind: ErrorKind::Kind,
                        message: "Self needs an object property, and this is a data property"
                            .into(),
                        span: kw_span,
                        candidates: Vec::new(),
                    });
                }
                _ => {
                    let n = self.cardinality()?;
                    let (f, h) = if self.filler_ahead(self.pos) {
                        let (r, rh) = self.data_primary_depth()?;
                        (Some(r), rh + 1)
                    } else {
                        (None, 1)
                    };
                    let ce = match kw.as_str() {
                        "min" => Ce::DataMinCardinality(n, d, f),
                        "max" => Ce::DataMaxCardinality(n, d, f),
                        _ => Ce::DataExactCardinality(n, d, f),
                    };
                    (ce, h)
                }
            });
        }
        let o = need_object(&p)?;
        Ok(match kw.as_str() {
            "some" => {
                let (f, fh) = self.primary_depth()?;
                (Ce::ObjectSomeValuesFrom(o, Box::new(f)), fh + 1)
            }
            "only" => {
                let (f, fh) = self.primary_depth()?;
                (Ce::ObjectAllValuesFrom(o, Box::new(f)), fh + 1)
            }
            "value" => (Ce::ObjectHasValue(o, self.individual()?), 1),
            "Self" => (Ce::ObjectHasSelf(o), 1),
            _ => {
                let n = self.cardinality()?;
                let (f, h) = if self.filler_ahead(self.pos) {
                    let (ce, fh) = self.primary_depth()?;
                    (Some(Box::new(ce)), fh + 1)
                } else {
                    (None, 1)
                };
                let ce = match kw.as_str() {
                    "min" => Ce::ObjectMinCardinality(n, o, f),
                    "max" => Ce::ObjectMaxCardinality(n, o, f),
                    _ => Ce::ObjectExactCardinality(n, o, f),
                };
                (ce, h)
            }
        })
    }

    fn cardinality(&mut self) -> Result<u32, Error> {
        match self.peek() {
            Some(Tok::Number(n)) if n.bytes().all(|b| b.is_ascii_digit()) => {
                let n = n.clone();
                let span = self.here();
                self.bump();
                n.parse::<u32>()
                    .map_err(|_| Error::syntax(span, "the cardinality is too large"))
            }
            _ => Err(self.expected("a cardinality, a whole number such as 1")),
        }
    }

    // ---------------------------------------------------------------- data ranges ----

    /// `dataConjunction or dataConjunction …`
    pub fn data_range(&mut self) -> Result<DataRange, Error> {
        self.data_range_depth().map(|(dr, _)| dr)
    }

    fn data_range_depth(&mut self) -> Result<(DataRange, usize), Error> {
        let (first, first_h) = self.data_conjunction_depth()?;
        if !self.eat_word("or") {
            return Ok((first, first_h));
        }
        let mut all = vec![first];
        let mut max_h = first_h;
        loop {
            let (next, h) = self.data_conjunction_depth()?;
            max_h = max_h.max(h);
            all.push(next);
            if !self.eat_word("or") {
                break;
            }
        }
        let set: BTreeSet<DataRange> = all.into_iter().collect();
        let collapsed = set.len() == 1;
        let dr = collapse(set, DataRange::UnionOf);
        let h = if collapsed { max_h } else { max_h + 1 };
        if self.depth + h > MAX_DEPTH {
            return Err(Error::syntax(
                self.here(),
                format!("the expression nests more than {MAX_DEPTH} deep"),
            ));
        }
        Ok((dr, h))
    }

    fn data_conjunction_depth(&mut self) -> Result<(DataRange, usize), Error> {
        let (first, first_h) = self.data_primary_depth()?;
        if !self.eat_word("and") {
            return Ok((first, first_h));
        }
        let mut all = vec![first];
        let mut max_h = first_h;
        loop {
            let (next, h) = self.data_primary_depth()?;
            max_h = max_h.max(h);
            all.push(next);
            if !self.eat_word("and") {
                break;
            }
        }
        let set: BTreeSet<DataRange> = all.into_iter().collect();
        let collapsed = set.len() == 1;
        let dr = collapse(set, DataRange::IntersectionOf);
        let h = if collapsed { max_h } else { max_h + 1 };
        if self.depth + h > MAX_DEPTH {
            return Err(Error::syntax(
                self.here(),
                format!("the expression nests more than {MAX_DEPTH} deep"),
            ));
        }
        Ok((dr, h))
    }

    fn data_primary_depth(&mut self) -> Result<(DataRange, usize), Error> {
        if self.peek() == Some(&Tok::LParen) {
            self.nest_paren()?;
            self.bump();
            let res = self.data_range_depth().and_then(|r| {
                self.expect(Tok::RParen, "')' to close the parenthesis")?;
                Ok(r)
            });
            self.paren_depth -= 1;
            return res;
        }
        self.nest()?;
        let res = (|| {
            if self.eat_word("not") {
                let (inner, h) = self.data_primary_depth()?;
                Ok((DataRange::ComplementOf(Box::new(inner)), h + 1))
            } else if self.peek() == Some(&Tok::LBrace) {
                self.bump();
                let mut ls = BTreeSet::new();
                loop {
                    ls.insert(self.literal()?);
                    if self.peek() == Some(&Tok::Comma) {
                        self.bump();
                        continue;
                    }
                    self.expect(Tok::RBrace, "',' or '}'")?;
                    break;
                }
                Ok((DataRange::OneOf(ls), 1))
            } else if self.is_name(self.pos) {
                let d = self.datatype()?;
                if self.peek() == Some(&Tok::LBracket) {
                    self.bump();
                    let mut fs = BTreeSet::new();
                    loop {
                        fs.insert(self.facet()?);
                        if self.peek() == Some(&Tok::Comma) {
                            self.bump();
                            continue;
                        }
                        self.expect(Tok::RBracket, "',' or ']'")?;
                        break;
                    }
                    Ok((DataRange::DatatypeRestriction(d, fs), 1))
                } else {
                    Ok((DataRange::Datatype(d), 1))
                }
            } else {
                Err(self.expected("a datatype, 'not', '(' or '{'"))
            }
        })();
        self.depth -= 1;
        res
    }

    fn facet(&mut self) -> Result<FacetRestriction, Error> {
        let iri = match self.peek() {
            Some(Tok::Op(op)) => {
                let local = match *op {
                    "<=" => "maxInclusive",
                    "<" => "maxExclusive",
                    ">=" => "minInclusive",
                    _ => "minExclusive",
                };
                format!("{}{local}", vocab::XSD)
            }
            Some(Tok::Name(Name::Word(w))) => {
                let found = vocab::FACETS
                    .iter()
                    .find(|f| super::local_name(f) == w.as_str())
                    .map(|f| f.to_string())
                    .or_else(|| {
                        self.v.expand_parts(w).and_then(|(ns, local)| {
                            vocab::FACETS
                                .iter()
                                .find(|f| {
                                    f.len() == ns.len().saturating_add(local.len())
                                        && f.starts_with(ns)
                                        && &f[ns.len()..] == local
                                })
                                .map(|f| (*f).to_string())
                        })
                    });
                match found {
                    Some(f) => f,
                    None => return Err(self.expected("a facet such as >=, <, length or pattern")),
                }
            }
            Some(Tok::Name(Name::Iri(i))) if vocab::FACETS.contains(&i.as_str()) => i.clone(),
            _ => return Err(self.expected("a facet such as >=, <, length or pattern")),
        };
        self.bump();
        Ok(FacetRestriction {
            facet: crate::model::iri(iri),
            value: self.literal()?,
        })
    }

    // ---------------------------------------------------- individuals and literals ---

    pub fn individual(&mut self) -> Result<Individual, Error> {
        if let Some(Tok::Name(Name::Word(w))) = self.peek()
            && let Some(label) = w.strip_prefix("_:")
        {
            if label.is_empty() {
                return Err(Error::syntax(
                    self.here(),
                    "an anonymous individual needs a label after _:",
                ));
            }
            let i = Individual::anonymous(label);
            self.bump();
            return Ok(i);
        }
        match self.entity(&[K::NamedIndividual], "an individual")? {
            (Entity::NamedIndividual(i), _) => Ok(Individual::Named(i)),
            _ => unreachable!("asked for an individual"),
        }
    }

    pub fn literal(&mut self) -> Result<Literal, Error> {
        let span = self.here();
        match self.peek().cloned() {
            Some(Tok::Str {
                value,
                lang,
                datatype,
            }) => {
                self.bump();
                if let Some(lang) = lang {
                    return self.parse_lang_literal_checked(span, value, &lang);
                }
                if let Some((n, dspan)) = datatype {
                    let d = self.resolve(&n, dspan, &[K::Datatype], "a datatype")?;
                    return Ok(Literal::typed(value, d[0].iri().as_str()));
                }
                Ok(Literal::simple(value))
            }
            Some(Tok::Number(n)) => {
                self.bump();
                number(&n).ok_or_else(|| {
                    if let Some(max_bytes) = self.max_error_bytes {
                        let mut msg = String::with_capacity(max_bytes.min(160));
                        push_capped(&mut msg, "'", max_bytes);
                        push_snippet(&mut msg, &n, BOUNDED_SNIPPET_BYTES, max_bytes);
                        push_capped(
                            &mut msg,
                            "' is not an integer, a decimal or a floating-point number with an f suffix",
                            max_bytes,
                        );
                        Error::syntax(span, msg)
                    } else {
                        Error::syntax(
                            span,
                            format!("'{n}' is not an integer, a decimal or a floating-point number with an f suffix"),
                        )
                    }
                })
            }
            Some(Tok::Name(Name::Word(w))) if w == "true" || w == "false" => {
                self.bump();
                Ok(Literal::typed(w, vocab::XSD_BOOLEAN))
            }
            _ => Err(self.expected("a literal such as \"text\", \"text\"@en, 12 or 1.5")),
        }
    }

    // ------------------------------------------------------------------- axioms ------

    /// One axiom, split at its top-level keyword.
    pub fn axiom(&mut self) -> Result<Axiom, Error> {
        let mut depth = 0i32;
        let mut split = None;
        for (k, (t, _)) in self.toks.iter().enumerate() {
            match t {
                Tok::LParen | Tok::LBrace | Tok::LBracket => depth += 1,
                Tok::RParen | Tok::RBrace | Tok::RBracket => depth -= 1,
                Tok::Name(Name::Word(w)) if depth == 0 && AXIOM_KEYWORDS.contains(&w.as_str()) => {
                    split = Some((k, w.clone()));
                    break;
                }
                _ => {}
            }
        }
        let Some((k, kw)) = split else {
            return self.fact_or_characteristic();
        };
        if k == 0 {
            return Err(Error::syntax(
                self.toks[0].1,
                format!("{kw} needs something on its left"),
            ));
        }
        let total = self.end;
        self.end = k;
        let kw_span = self.toks[k].1;
        let left = self.left_side(&kw)?;
        self.done()?;
        self.end = total;
        self.pos = k + 1;
        if self.pos >= self.end {
            return Err(Error::syntax(
                kw_span,
                format!("{kw} needs something on its right"),
            ));
        }
        let left = match left {
            Left::Punned(es) => self.choose(&kw, es),
            left => left,
        };
        let kind = match (kw.as_str(), left) {
            ("SubClassOf", Left::Class(a)) => AxiomKind::SubClassOf(a, self.description()?),
            ("EquivalentTo", Left::Class(a)) => {
                AxiomKind::EquivalentClasses([a, self.description()?].into_iter().collect())
            }
            ("DisjointWith", Left::Class(a)) => {
                AxiomKind::DisjointClasses([a, self.description()?].into_iter().collect())
            }
            ("Type" | "Types", Left::Individual(i)) => {
                AxiomKind::ClassAssertion(self.description()?, i)
            }
            ("SameAs", Left::Individual(i)) => {
                AxiomKind::SameIndividual([i, self.individual()?].into_iter().collect())
            }
            ("DifferentFrom", Left::Individual(i)) => {
                AxiomKind::DifferentIndividuals([i, self.individual()?].into_iter().collect())
            }
            ("SubPropertyOf", Left::Chain(ps)) => {
                AxiomKind::SubObjectPropertyOf(SubObjectPropertyExpression::Chain(ps), self.ope()?)
            }
            ("SubPropertyChain", Left::Object(sup)) => {
                let mut ps = vec![self.ope()?];
                while self.eat_word("o") {
                    ps.push(self.ope()?);
                }
                if ps.len() < 2 {
                    return Err(self.expected("'o' and the next property of the chain"));
                }
                AxiomKind::SubObjectPropertyOf(SubObjectPropertyExpression::Chain(ps), sup)
            }
            ("SubPropertyOf", Left::Object(p)) => AxiomKind::SubObjectPropertyOf(
                SubObjectPropertyExpression::Expression(p),
                self.ope()?,
            ),
            ("EquivalentTo", Left::Object(p)) => {
                AxiomKind::EquivalentObjectProperties([p, self.ope()?].into_iter().collect())
            }
            ("DisjointWith", Left::Object(p)) => {
                AxiomKind::DisjointObjectProperties([p, self.ope()?].into_iter().collect())
            }
            ("InverseOf", Left::Object(p)) => AxiomKind::InverseObjectProperties(p, self.ope()?),
            ("Domain", Left::Object(p)) => AxiomKind::ObjectPropertyDomain(p, self.description()?),
            ("Range", Left::Object(p)) => AxiomKind::ObjectPropertyRange(p, self.description()?),
            ("SubPropertyOf", Left::Data(p)) => {
                AxiomKind::SubDataPropertyOf(p, self.data_property()?)
            }
            ("EquivalentTo", Left::Data(p)) => AxiomKind::EquivalentDataProperties(
                [p, self.data_property()?].into_iter().collect(),
            ),
            ("DisjointWith", Left::Data(p)) => {
                AxiomKind::DisjointDataProperties([p, self.data_property()?].into_iter().collect())
            }
            ("Domain", Left::Data(p)) => AxiomKind::DataPropertyDomain(p, self.description()?),
            ("Range", Left::Data(p)) => AxiomKind::DataPropertyRange(p, self.data_range()?),
            ("SubPropertyOf", Left::Annotation(p)) => {
                AxiomKind::SubAnnotationPropertyOf(p, self.annotation_property()?)
            }
            ("Domain", Left::Annotation(p)) => AxiomKind::AnnotationPropertyDomain(p, self.iri()?),
            ("Range", Left::Annotation(p)) => AxiomKind::AnnotationPropertyRange(p, self.iri()?),
            (kw, left) => {
                return Err(Error::syntax(
                    kw_span,
                    format!("{kw} does not follow {}", left.what()),
                ));
            }
        };
        Ok(Axiom::new(kind))
    }

    /// The left side of an axiom with the keyword `kw`, which says what it can be.
    fn left_side(&mut self, kw: &str) -> Result<Left, Error> {
        match kw {
            "Type" | "Types" | "SameAs" | "DifferentFrom" => {
                return Ok(Left::Individual(self.individual()?));
            }
            "SubClassOf" => return Ok(Left::Class(self.description()?)),
            _ => {}
        }
        // a chain `p o q …`
        let mut depth = 0i32;
        let chain = (self.pos..self.end).any(|k| match self.tok(k) {
            Some(Tok::LParen) => {
                depth += 1;
                false
            }
            Some(Tok::RParen) => {
                depth -= 1;
                false
            }
            Some(t) => depth == 0 && t.word() == Some("o"),
            None => false,
        });
        if chain {
            if kw != "SubPropertyOf" {
                return Err(Error::syntax(
                    self.here(),
                    format!("a property chain takes SubPropertyOf, not {kw}"),
                ));
            }
            let mut ps = vec![self.ope()?];
            while self.eat_word("o") {
                ps.push(self.ope()?);
            }
            return Ok(Left::Chain(ps));
        }
        // `inverse p` alone is a property; followed by more it starts a restriction
        let inverse_end = if self.tok(self.pos + 1) == Some(&Tok::LParen) {
            self.pos + 4
        } else {
            self.pos + 2
        };
        if self.is_word(self.pos, "inverse") && self.end == inverse_end {
            return Ok(Left::Object(self.ope()?));
        }
        let property_only = matches!(
            kw,
            "SubPropertyOf" | "InverseOf" | "Domain" | "Range" | "SubPropertyChain"
        );
        // one name: a class or a property; anything longer is a class expression
        if self.is_name(self.pos) && self.end == self.pos + 1 {
            let (Tok::Name(n), span) = self.toks[self.pos].clone() else {
                unreachable!("a name")
            };
            let mut want = vec![K::ObjectProperty, K::DataProperty, K::AnnotationProperty];
            if !property_only {
                want.insert(0, K::Class);
            }
            let what = if property_only {
                "a property"
            } else {
                "a class or a property"
            };
            let es = self.resolve(&n, span, &want, what)?;
            self.pos += 1;
            return Ok(match es.len() {
                1 => Left::of(es[0].clone()),
                _ => Left::Punned(es),
            });
        }
        if property_only {
            return Err(self.expected("a property"));
        }
        Ok(Left::Class(self.description()?))
    }

    /// The kind of a punned left side, which the axiom's keyword and its right side
    /// decide: a class where it can be one, a data property for a range that is a data
    /// range, and otherwise the kind that a name on the right has.
    fn choose(&self, kw: &str, es: Vec<Entity>) -> Left {
        if matches!(es[0], Entity::Class(_)) {
            return Left::of(es[0].clone());
        }
        if kw == "Range" {
            let data = self.data_ahead(self.pos);
            if let Some(e) = es
                .iter()
                .find(|e| matches!(e, Entity::DataProperty(_)) == data)
            {
                return Left::of(e.clone());
            }
        }
        if let Some(Tok::Name(n)) = self.tok(self.pos) {
            let right_kinds = self.v.lookup_kinds(
                n,
                &[K::ObjectProperty, K::DataProperty, K::AnnotationProperty],
            );
            if let Some(e) = es.iter().find(|e| right_kinds.contains(e.kind())) {
                return Left::of(e.clone());
            }
        }
        Left::of(es[0].clone())
    }

    pub(super) fn data_property(&mut self) -> Result<DataProperty, Error> {
        match self.entity(&[K::DataProperty], "a data property")? {
            (Entity::DataProperty(p), _) => Ok(p),
            _ => unreachable!("asked for a data property"),
        }
    }

    pub(super) fn annotation_property(&mut self) -> Result<AnnotationProperty, Error> {
        match self.entity(&[K::AnnotationProperty], "an annotation property")? {
            (Entity::AnnotationProperty(p), _) => Ok(p),
            _ => unreachable!("asked for an annotation property"),
        }
    }

    /// An IRI, as the domain or range of an annotation property names one.
    fn iri(&mut self) -> Result<Iri, Error> {
        let span = self.here();
        let iri = match self.peek() {
            Some(Tok::Name(Name::Iri(i))) => {
                let len = i.len();
                let owned = i.clone();
                if let Some(b) = self.budget.as_mut()
                    && b.charge(len).is_err()
                {
                    return Err(Error::syntax(
                        span,
                        "resolved term bytes exceeded the expression budget",
                    ));
                }
                owned
            }
            Some(Tok::Name(Name::Word(w))) if self.v.expanded_len(w).is_some() => {
                let exp_len = self.v.expanded_len(w).expect("checked");
                let w = w.clone();
                if let Some(b) = self.budget.as_mut()
                    && b.charge(exp_len).is_err()
                {
                    return Err(Error::syntax(
                        span,
                        "resolved term bytes exceeded the expression budget",
                    ));
                }
                self.v.expand(&w).expect("checked")
            }
            Some(Tok::Name(n)) => {
                let n = n.clone();
                let es = self.resolve(&n, span, &K::ALL, "an IRI")?;
                es[0].iri().as_str().to_string()
            }
            _ => return Err(self.expected("an IRI")),
        };
        self.bump();
        self.parse_iri_checked(span, &iri)
    }

    /// An axiom without a keyword between its sides: a characteristic such as
    /// `Transitive p`, or a fact `a p b` or `a not p b`.
    fn fact_or_characteristic(&mut self) -> Result<Axiom, Error> {
        if let Some(w) = self.peek().and_then(Tok::word).map(str::to_string)
            && CHARACTERISTICS.contains(&w.as_str())
            && self.end >= 2
        {
            self.bump();
            let data = if w == "Functional" && self.is_name(self.pos) {
                let (Tok::Name(n), span) = self.toks[self.pos].clone() else {
                    unreachable!("a name")
                };
                let es = self.resolve(
                    &n,
                    span,
                    &[K::ObjectProperty, K::DataProperty],
                    "a property",
                )?;
                match &es[..] {
                    [Entity::DataProperty(d)] => Some(d.clone()),
                    _ => None,
                }
            } else {
                None
            };
            let kind = match data {
                Some(d) => {
                    self.bump();
                    AxiomKind::FunctionalDataProperty(d)
                }
                None => {
                    let o = self.ope()?;
                    match w.as_str() {
                        "Functional" => AxiomKind::FunctionalObjectProperty(o),
                        "InverseFunctional" => AxiomKind::InverseFunctionalObjectProperty(o),
                        "Reflexive" => AxiomKind::ReflexiveObjectProperty(o),
                        "Irreflexive" => AxiomKind::IrreflexiveObjectProperty(o),
                        "Symmetric" => AxiomKind::SymmetricObjectProperty(o),
                        "Asymmetric" => AxiomKind::AsymmetricObjectProperty(o),
                        _ => AxiomKind::TransitiveObjectProperty(o),
                    }
                }
            };
            return Ok(Axiom::new(kind));
        }
        let subject = self.individual()?;
        let negative = self.eat_word("not");
        let p = self.prop()?;
        let data = match (&p.object, &p.data) {
            (Some(_), None) => false,
            (None, Some(_)) => true,
            _ => self.literal_ahead(self.pos),
        };
        let kind = if data {
            let d = p.data.expect("a data property");
            let l = self.literal()?;
            if negative {
                AxiomKind::NegativeDataPropertyAssertion(d, subject, l)
            } else {
                AxiomKind::DataPropertyAssertion(d, subject, l)
            }
        } else {
            let o = p.object.expect("an object property");
            let i = self.individual()?;
            if negative {
                AxiomKind::NegativeObjectPropertyAssertion(o, subject, i)
            } else {
                AxiomKind::ObjectPropertyAssertion(o, subject, i)
            }
        };
        Ok(Axiom::new(kind))
    }
}

/// The left side of an axiom.
enum Left {
    Class(Ce),
    Individual(Individual),
    Object(Ope),
    Chain(Vec<Ope>),
    Data(DataProperty),
    Annotation(AnnotationProperty),
    /// a name of several kinds, which the right side decides
    Punned(Vec<Entity>),
}

impl Left {
    fn of(e: Entity) -> Left {
        match e {
            Entity::Class(c) => Left::Class(Ce::Class(c)),
            Entity::ObjectProperty(p) => Left::Object(Ope::Property(p)),
            Entity::DataProperty(p) => Left::Data(p),
            Entity::AnnotationProperty(p) => Left::Annotation(p),
            Entity::NamedIndividual(i) => Left::Individual(Individual::Named(i)),
            Entity::Datatype(d) => Left::Class(Ce::Class(Class(d.0))),
        }
    }

    fn what(&self) -> &'static str {
        match self {
            Left::Class(_) => "a class expression",
            Left::Individual(_) => "an individual",
            Left::Object(_) => "an object property",
            Left::Chain(_) => "a property chain",
            Left::Data(_) => "a data property",
            Left::Annotation(_) => "an annotation property",
            Left::Punned(_) => "a property",
        }
    }
}

/// An n-ary expression of a set, or its one member when the set holds one.
fn collapse<T: Ord>(mut set: BTreeSet<T>, make: impl FnOnce(BTreeSet<T>) -> T) -> T {
    if set.len() == 1 {
        set.pop_first().expect("one")
    } else {
        make(set)
    }
}

/// A number literal: an integer, a decimal, or a floating-point number with an `f`
/// suffix, whose value is the lexical form without the suffix.
pub fn number(n: &str) -> Option<Literal> {
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    let unsigned = n.strip_prefix(['+', '-']).unwrap_or(n);
    if digits(unsigned) {
        return Some(Literal::typed(n, vocab::XSD_INTEGER));
    }
    if let Some((a, b)) = unsigned.split_once('.')
        && (digits(a) || a.is_empty())
        && digits(b)
    {
        return Some(Literal::typed(n, vocab::XSD_DECIMAL));
    }
    let body = unsigned.strip_suffix(['f', 'F'])?;
    let (mantissa, exp) = match body.split_once(['e', 'E']) {
        Some((m, e)) => (m, Some(e)),
        None => (body, None),
    };
    let m_ok = digits(mantissa)
        || mantissa.split_once('.').is_some_and(|(a, b)| {
            (digits(a) || a.is_empty()) && (digits(b) || (b.is_empty() && digits(a)))
        });
    let e_ok = exp.is_none_or(|e| digits(e.strip_prefix(['+', '-']).unwrap_or(e)));
    (m_ok && e_ok).then(|| Literal::typed(&n[..n.len() - 1], vocab::XSD_FLOAT))
}

fn kind_word(k: K) -> &'static str {
    match k {
        K::Class => "class",
        K::Datatype => "datatype",
        K::ObjectProperty => "object property",
        K::DataProperty => "data property",
        K::AnnotationProperty => "annotation property",
        K::NamedIndividual => "individual",
    }
}

fn article(word: &str) -> &'static str {
    if word.starts_with(['a', 'e', 'i', 'o', 'u']) {
        "an"
    } else {
        "a"
    }
}

fn join_or(words: &[&str]) -> String {
    match words {
        [] => String::new(),
        [a] => a.to_string(),
        [rest @ .., last] => format!("{} or {last}", rest.join(", ")),
    }
}

fn name_text(n: &Name) -> String {
    match n {
        Name::Iri(i) => format!("<{i}>"),
        Name::Word(w) => w.clone(),
        Name::Quoted(q) => q.clone(),
    }
}

fn push_capped(out: &mut String, s: &str, max_bytes: usize) -> bool {
    if out.len() >= max_bytes {
        return false;
    }
    let avail = max_bytes - out.len();
    if s.len() <= avail {
        out.push_str(s);
        true
    } else {
        let mut end = avail;
        while end > 0 && !s.is_char_boundary(end) {
            end -= 1;
        }
        out.push_str(&s[..end]);
        false
    }
}

fn push_snippet(out: &mut String, s: &str, snippet_cap: usize, max_bytes: usize) {
    if s.len() <= snippet_cap {
        push_capped(out, s, max_bytes);
        return;
    }
    let ellipsis = "...";
    let body_cap = snippet_cap.saturating_sub(ellipsis.len());
    let mut end = body_cap.min(s.len());
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    push_capped(out, &s[..end], max_bytes);
    push_capped(out, ellipsis, max_bytes);
}

fn push_name_bounded(out: &mut String, n: &Name, snippet_cap: usize, max_bytes: usize) {
    match n {
        Name::Iri(i) => {
            push_capped(out, "<", max_bytes);
            push_snippet(out, i, snippet_cap, max_bytes);
            push_capped(out, ">", max_bytes);
        }
        Name::Word(w) => push_snippet(out, w, snippet_cap, max_bytes),
        Name::Quoted(q) => push_snippet(out, q, snippet_cap, max_bytes),
    }
}

fn push_debug_snippet(out: &mut String, s: &str, snippet_cap: usize, max_bytes: usize) {
    let ellipsis = "...";
    let body_cap = snippet_cap.saturating_sub(ellipsis.len());
    let mut escaped_bytes = 0usize;
    let mut cut_len = out.len();
    for c in s.chars() {
        let mut buf = [0u8; 16];
        let mut n = 0usize;
        for ec in c.escape_debug() {
            n += ec.encode_utf8(&mut buf[n..]).len();
        }
        let piece = std::str::from_utf8(&buf[..n]).expect("valid utf8");
        let next_bytes = escaped_bytes.saturating_add(piece.len());
        if next_bytes > snippet_cap {
            out.truncate(cut_len);
            push_capped(out, ellipsis, max_bytes);
            return;
        }
        if !push_capped(out, piece, max_bytes) {
            return;
        }
        escaped_bytes = next_bytes;
        if escaped_bytes <= body_cap {
            cut_len = out.len();
        }
    }
}

fn push_tok_described_bounded(out: &mut String, t: &Tok, snippet_cap: usize, max_bytes: usize) {
    if let Some(w) = t.word()
        && KEYWORDS.contains(&w)
    {
        push_capped(out, "the keyword '", max_bytes);
        push_capped(out, w, max_bytes);
        push_capped(out, "'", max_bytes);
        return;
    }
    push_capped(out, "'", max_bytes);
    match t {
        Tok::LParen => {
            push_capped(out, "(", max_bytes);
        }
        Tok::RParen => {
            push_capped(out, ")", max_bytes);
        }
        Tok::LBrace => {
            push_capped(out, "{", max_bytes);
        }
        Tok::RBrace => {
            push_capped(out, "}", max_bytes);
        }
        Tok::LBracket => {
            push_capped(out, "[", max_bytes);
        }
        Tok::RBracket => {
            push_capped(out, "]", max_bytes);
        }
        Tok::Comma => {
            push_capped(out, ",", max_bytes);
        }
        Tok::Op(o) => {
            push_capped(out, o, max_bytes);
        }
        Tok::Name(Name::Iri(i)) => {
            push_capped(out, "<", max_bytes);
            push_snippet(out, i, snippet_cap, max_bytes);
            push_capped(out, ">", max_bytes);
        }
        Tok::Name(Name::Word(w)) => push_snippet(out, w, snippet_cap, max_bytes),
        Tok::Name(Name::Quoted(q)) => {
            push_capped(out, "'", max_bytes);
            push_snippet(out, q, snippet_cap, max_bytes);
            push_capped(out, "'", max_bytes);
        }
        Tok::Str { value, .. } => {
            push_capped(out, "\"", max_bytes);
            push_debug_snippet(out, value, snippet_cap, max_bytes);
            push_capped(out, "\"", max_bytes);
        }
        Tok::Number(n) => push_snippet(out, n, snippet_cap, max_bytes),
    }
    push_capped(out, "'", max_bytes);
}

fn push_short_iri_bounded(
    out: &mut String,
    iri: &str,
    prefixes: &[(String, String)],
    snippet_cap: usize,
    max_bytes: usize,
) {
    if let Some((prefix, local)) = prefixes
        .iter()
        .filter_map(|(p, ns)| iri.strip_prefix(ns.as_str()).map(|local| (p, ns, local)))
        .max_by_key(|(_, ns, _)| ns.len())
        .map(|(p, _, local)| (p.as_str(), local))
        && !local.is_empty()
        && !local.contains([' ', '<', '>', '\'', '"', '(', ')'])
    {
        push_snippet(out, prefix, 24, max_bytes);
        push_capped(out, ":", max_bytes);
        push_snippet(out, local, snippet_cap, max_bytes);
    } else {
        push_capped(out, "<", max_bytes);
        push_snippet(out, iri, snippet_cap, max_bytes);
        push_capped(out, ">", max_bytes);
    }
}

/// Format an ambiguous-name diagnostic strictly within `max_bytes` without cloning the
/// prefix table, candidate list, or full ontology labels.
fn format_ambiguous_bounded(
    n: &Name,
    es: &[Entity],
    unique_count: usize,
    v: &Vocabulary,
    max_bytes: usize,
) -> String {
    use std::fmt::Write as _;

    let suffix = "; write a prefixed name or an IRI";
    let list_budget = max_bytes.saturating_sub(suffix.len() + 6);
    let mut msg = String::with_capacity(max_bytes.min(256));
    push_capped(&mut msg, "'", max_bytes);
    push_name_bounded(&mut msg, n, BOUNDED_SNIPPET_BYTES, list_budget);
    let mut header = String::new();
    let _ = write!(&mut header, "' names {unique_count} entities: ");
    push_capped(&mut msg, &header, list_budget);

    let mut seen: BTreeSet<&str> = BTreeSet::new();
    let mut written = 0usize;
    let mut omitted = false;
    for e in es {
        let iri = e.iri().as_str();
        if !seen.insert(iri) {
            continue;
        }
        if written > 0 && msg.len().saturating_add(24) >= list_budget {
            omitted = true;
            break;
        }
        if written > 0 {
            push_capped(&mut msg, ", ", list_budget);
        }
        push_short_iri_bounded(
            &mut msg,
            iri,
            v.prefixes(),
            BOUNDED_SNIPPET_BYTES,
            list_budget,
        );
        push_capped(&mut msg, " (", list_budget);
        push_capped(&mut msg, kind_word(e.kind()), list_budget);
        if let Some(label) = v.label(iri) {
            push_capped(&mut msg, ", labelled '", list_budget);
            push_snippet(&mut msg, label, BOUNDED_SNIPPET_BYTES, list_budget);
            push_capped(&mut msg, "'", list_budget);
        }
        push_capped(&mut msg, ")", list_budget);
        written += 1;
    }
    if omitted {
        push_capped(&mut msg, ", ...", max_bytes);
    }
    push_capped(&mut msg, suffix, max_bytes);
    msg
}
