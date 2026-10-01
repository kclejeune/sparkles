//! The ShExC 2.1 parser: recursive descent over the lexer's tokens, with errors that
//! give the 1-based line and column and the tokens that were expected. ShEx 2.2 syntax
//! (`EXTENDS`, `ABSTRACT`, `RESTRICTS`) is an error that names the feature.
//!
//! Prefixed names and relative IRIs are resolved here (against `BASE`, then the base the
//! text was given with; without either a relative IRI stays relative), escapes in
//! strings, IRIs, regular expressions and semantic-action code are decoded, and each
//! node constraint goes through [`check_facets`].

use super::lexer::{Token, TokenKind, is_local_escape, lex, uchar};
use crate::ast::*;
use crate::check::check_facets;
use crate::{ParseError, PrefixMap};
use oxiri::IriRef;

const RDF_TYPE: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#type";
const XSD: &str = "http://www.w3.org/2001/XMLSchema#";

/// Parse a ShExC schema (see [`Schema::parse_shexc`]).
pub fn parse(text: &str, base: Option<&str>) -> std::result::Result<Schema, ParseError> {
    let base = match base {
        Some(b) => Some(
            IriRef::parse(b.to_string())
                .map_err(|e| ParseError::new(format!("invalid base IRI <{b}>: {e}"), 1, 1))?,
        ),
        None => None,
    };
    let toks = lex(text)
        .into_iter()
        .filter(|t| !t.kind.is_trivia())
        .collect();
    let mut p = Parser {
        src: text,
        toks,
        pos: 0,
        base,
        prefixes: Vec::new(),
        nest: 0,
    };
    p.schema()
}

/// The 1-based line and column (in characters) of byte `offset` of `src`.
pub fn line_col(src: &str, offset: usize) -> (usize, usize) {
    let from = if src.starts_with('\u{feff}') { 3 } else { 0 };
    let before = &src[from.min(offset)..offset];
    let line = 1 + before.matches('\n').count();
    let line_start = before.rfind('\n').map_or(0, |i| i + 1);
    (line, 1 + before[line_start..].chars().count())
}

/// The ShEx 2.2 keywords, with the feature each one belongs to.
const SHEX_2_2: &[(&str, &str)] = &[
    ("EXTENDS", "extending shapes"),
    ("ABSTRACT", "abstract shapes"),
    ("RESTRICTS", "restricting shapes"),
];

struct Parser<'a> {
    src: &'a str,
    /// the tokens without trivia; the last one is `Eof`
    toks: Vec<Token>,
    pos: usize,
    base: Option<IriRef<String>>,
    prefixes: PrefixMap,
    /// how many bracketed triple expressions enclose the current token
    nest: usize,
}

type Result<T> = std::result::Result<T, ParseError>;

impl<'a> Parser<'a> {
    // ------------------------------------------------------------ tokens ------

    fn peek(&self) -> Token {
        self.peek_at(0)
    }

    fn peek_at(&self, n: usize) -> Token {
        self.toks[(self.pos + n).min(self.toks.len() - 1)]
    }

    fn kind(&self) -> TokenKind {
        self.peek().kind
    }

    fn bump(&mut self) -> Token {
        let t = self.peek();
        if self.pos < self.toks.len() - 1 {
            self.pos += 1;
        }
        t
    }

    fn text(&self, t: Token) -> &'a str {
        t.text(self.src)
    }

    fn is_kw(&self, t: Token, kw: &str) -> bool {
        t.kind == TokenKind::Word && self.text(t).eq_ignore_ascii_case(kw)
    }

    fn at_kw(&self, kw: &str) -> bool {
        self.is_kw(self.peek(), kw)
    }

    fn at_any_kw(&self, kws: &[&str]) -> bool {
        kws.iter().any(|k| self.at_kw(k))
    }

    fn eat_kw(&mut self, kw: &str) -> bool {
        let yes = self.at_kw(kw);
        if yes {
            self.bump();
        }
        yes
    }

    fn eat(&mut self, kind: TokenKind) -> bool {
        let yes = self.kind() == kind;
        if yes {
            self.bump();
        }
        yes
    }

    fn expect(&mut self, kind: TokenKind, what: &str) -> Result<Token> {
        if self.kind() == kind {
            Ok(self.bump())
        } else {
            Err(self.expected(&[what]))
        }
    }

    // ------------------------------------------------------------ errors ------

    fn error_at(&self, offset: usize, message: impl Into<String>) -> ParseError {
        let (line, column) = line_col(self.src, offset);
        ParseError::new(message, line, column)
    }

    fn error_tok(&self, t: Token, message: impl Into<String>) -> ParseError {
        self.error_at(t.start as usize, message)
    }

    /// "expected A, B or C, found X" at the current token; a ShEx 2.2 keyword names its
    /// feature instead.
    fn expected(&self, what: &[&str]) -> ParseError {
        let t = self.peek();
        if t.kind == TokenKind::Word
            && let Some((kw, feature)) = SHEX_2_2
                .iter()
                .find(|(kw, _)| self.text(t).eq_ignore_ascii_case(kw))
        {
            return self.error_tok(
                t,
                format!("{kw} is ShEx 2.2 syntax ({feature}), which is not supported"),
            );
        }
        let list = match what {
            [] => String::new(),
            [one] => (*one).to_string(),
            [init @ .., last] => format!("{} or {last}", init.join(", ")),
        };
        self.error_tok(t, format!("expected {list}, found {}", self.describe(t)))
    }

    fn describe(&self, t: Token) -> String {
        match t.kind {
            TokenKind::Eof => "the end of the input".into(),
            TokenKind::Unknown if matches!(self.text(t), "\"" | "'") => {
                "an unterminated string".into()
            }
            _ => {
                let text = self.text(t);
                match text.char_indices().nth(40) {
                    Some((i, _)) => format!("'{}…'", &text[..i]),
                    None => format!("'{text}'"),
                }
            }
        }
    }

    // ------------------------------------------------------------ schema ------

    fn schema(&mut self) -> Result<Schema> {
        let mut schema = Schema::default();
        // start actions may only come before the first statement, in one run
        let mut acts_open = true;
        loop {
            let t = self.peek();
            if t.kind == TokenKind::Eof {
                break;
            } else if self.at_any_kw(&["BASE", "PREFIX", "IMPORT"]) {
                self.directive(&mut schema)?;
                if !schema.start_acts.is_empty() {
                    acts_open = false;
                }
            } else if self.at_kw("start") {
                self.bump();
                self.expect(TokenKind::Eq, "'='")?;
                let expr = self.shape_expression(true)?;
                if schema.start.is_some() {
                    return Err(self.error_tok(t, "the start shape is declared twice"));
                }
                schema.start = Some(expr);
                acts_open = false;
            } else if t.kind == TokenKind::Code && acts_open {
                schema.start_acts.push(self.sem_act()?);
            } else if self.starts_label() {
                let label = self.label()?;
                let expr = if self.eat_kw("EXTERNAL") {
                    ShapeExpr::External
                } else {
                    self.shape_expression(false)?
                };
                schema.shapes.push(ShapeDecl { label, expr });
                acts_open = false;
            } else {
                let mut what = vec!["BASE", "PREFIX", "IMPORT", "start", "a shape label"];
                if acts_open {
                    what.push("a semantic action");
                }
                return Err(self.expected(&what));
            }
        }
        schema.base = self.base.as_ref().map(|b| b.as_str().to_string());
        schema.prefixes = std::mem::take(&mut self.prefixes);
        Ok(schema)
    }

    fn directive(&mut self, schema: &mut Schema) -> Result<()> {
        let kw = self.bump();
        if self.is_kw(kw, "BASE") {
            let t = self.expect(TokenKind::IriRef, "an IRI in angle brackets")?;
            let iri = self.iriref(t)?;
            self.base = Some(IriRef::parse(iri).map_err(|e| self.error_tok(t, e.to_string()))?);
        } else if self.is_kw(kw, "PREFIX") {
            let ns = self.expect(TokenKind::PnameNs, "a prefix ('ex:')")?;
            let t = self.expect(TokenKind::IriRef, "an IRI in angle brackets")?;
            let iri = self.iriref(t)?;
            let name = self.text(ns).trim_end_matches(':').to_string();
            match self.prefixes.iter_mut().find(|(p, _)| *p == name) {
                Some(entry) => entry.1 = iri,
                None => self.prefixes.push((name, iri)),
            }
        } else {
            let iri = self.iri()?;
            schema.imports.push(iri);
        }
        Ok(())
    }

    // ------------------------------------------------------ IRIs, labels ------

    fn starts_iri(&self) -> bool {
        matches!(
            self.kind(),
            TokenKind::IriRef | TokenKind::PnameLn | TokenKind::PnameNs
        )
    }

    fn starts_label(&self) -> bool {
        self.starts_iri() || self.kind() == TokenKind::BlankNodeLabel
    }

    /// `iri`: an IRI reference or a prefixed name.
    fn iri(&mut self) -> Result<String> {
        let t = self.peek();
        match t.kind {
            TokenKind::IriRef => {
                self.bump();
                self.iriref(t)
            }
            TokenKind::PnameLn | TokenKind::PnameNs => {
                self.bump();
                self.pname(t.start as usize, self.text(t))
            }
            _ => Err(self.expected(&["an IRI"])),
        }
    }

    /// The IRI of an `IRIREF` token, unescaped and resolved against the base.
    fn iriref(&self, t: Token) -> Result<String> {
        let text = self.text(t);
        let raw = &text[1..text.len() - 1];
        let mut iri = String::with_capacity(raw.len());
        let mut rest = raw;
        while let Some(i) = rest.find('\\') {
            iri.push_str(&rest[..i]);
            let (c, n) = self.decode_uchar(
                &rest[i..],
                t.start as usize + 1 + raw.len() - rest.len() + i,
            )?;
            iri.push(c);
            rest = &rest[i + n..];
        }
        iri.push_str(rest);
        let resolved = match &self.base {
            Some(base) => base.resolve(&iri).map(IriRef::into_inner),
            None => IriRef::parse(iri.as_str()).map(|_| iri.clone()),
        };
        resolved.map_err(|e| self.error_tok(t, format!("invalid IRI <{iri}>: {e}")))
    }

    /// `\uXXXX` or `\UXXXXXXXX` at the start of `s` (found at byte `at` of the source):
    /// the character and the escape's length.
    fn decode_uchar(&self, s: &str, at: usize) -> Result<(char, usize)> {
        let bad = || self.error_at(at, "invalid escape: expected \\uXXXX or \\UXXXXXXXX");
        let n = uchar(s.as_bytes()).ok_or_else(bad)?;
        let code = u32::from_str_radix(&s[2..n], 16).map_err(|_| bad())?;
        let c = char::from_u32(code)
            .ok_or_else(|| self.error_at(at, format!("{} is not a Unicode character", &s[..n])))?;
        Ok((c, n))
    }

    /// A prefixed name's IRI: the namespace of its prefix and the local part with its
    /// `\` escapes removed.
    fn pname(&self, at: usize, text: &str) -> Result<String> {
        let colon = text.find(':').expect("a prefixed name has a colon");
        let (prefix, local) = (&text[..colon], &text[colon + 1..]);
        let Some((_, ns)) = self.prefixes.iter().rev().find(|(p, _)| p == prefix) else {
            return Err(self.error_at(at, format!("undeclared prefix '{prefix}:'")));
        };
        let mut iri = String::with_capacity(ns.len() + local.len());
        iri.push_str(ns);
        let mut chars = local.chars().peekable();
        while let Some(c) = chars.next() {
            match chars.peek() {
                Some(&e) if c == '\\' && e.is_ascii() && is_local_escape(e as u8) => {
                    iri.push(e);
                    chars.next();
                }
                _ => iri.push(c),
            }
        }
        Ok(iri)
    }

    /// `shapeExprLabel` / `tripleExprLabel`: an IRI or a blank node.
    fn label(&mut self) -> Result<Label> {
        if self.kind() == TokenKind::BlankNodeLabel {
            let t = self.bump();
            return Ok(Label::BNode(self.text(t)[2..].to_string()));
        }
        if !self.starts_iri() {
            return Err(self.expected(&["an IRI", "a blank node label"]));
        }
        Ok(Label::Iri(self.iri()?))
    }

    /// `predicate`: an IRI or `a`.
    fn predicate(&mut self) -> Result<String> {
        let t = self.peek();
        if t.kind == TokenKind::Word && self.text(t) == "a" {
            self.bump();
            return Ok(RDF_TYPE.to_string());
        }
        if !self.starts_iri() {
            return Err(self.expected(&["a predicate (an IRI or 'a')"]));
        }
        self.iri()
    }

    // ------------------------------------------------- shape expressions ------

    /// `shapeExpression` (outline: shapes and node constraints take annotations and
    /// semantic actions) or `inlineShapeExpression`.
    fn shape_expression(&mut self, inline: bool) -> Result<ShapeExpr> {
        let first = self.shape_and(inline)?;
        if !self.at_kw("OR") {
            return Ok(first);
        }
        let mut exprs = vec![first];
        while self.eat_kw("OR") {
            exprs.push(self.shape_and(inline)?);
        }
        Ok(ShapeExpr::Or(exprs))
    }

    fn shape_and(&mut self, inline: bool) -> Result<ShapeExpr> {
        let first = self.shape_not(inline)?;
        if !self.at_kw("AND") {
            return Ok(first);
        }
        let mut exprs = vec![first];
        while self.eat_kw("AND") {
            exprs.push(self.shape_not(inline)?);
        }
        Ok(ShapeExpr::And(exprs))
    }

    fn shape_not(&mut self, inline: bool) -> Result<ShapeExpr> {
        if self.eat_kw("NOT") {
            Ok(ShapeExpr::Not(Box::new(self.shape_atom(inline)?)))
        } else {
            self.shape_atom(inline)
        }
    }

    fn shape_atom(&mut self, inline: bool) -> Result<ShapeExpr> {
        if self.starts_non_lit() {
            let nc = self.node_constraint(inline, false)?;
            if self.starts_shape_or_ref() {
                let s = self.shape_or_ref(inline)?;
                return Ok(ShapeExpr::And(vec![nc, s]));
            }
            Ok(nc)
        } else if self.starts_lit() {
            self.node_constraint(inline, true)
        } else if self.starts_shape_or_ref() {
            let s = self.shape_or_ref(inline)?;
            if self.starts_non_lit() {
                let nc = self.node_constraint(inline, false)?;
                return Ok(ShapeExpr::And(vec![s, nc]));
            }
            Ok(s)
        } else if self.eat(TokenKind::LParen) {
            let e = self.shape_expression(false)?;
            self.expect(TokenKind::RParen, "')'")?;
            Ok(e)
        } else if self.eat(TokenKind::Dot) {
            Ok(ShapeExpr::Shape(Box::default()))
        } else {
            Err(self.expected(&[
                "a node constraint",
                "a shape ('{')",
                "a shape reference ('@')",
                "'('",
                "'.'",
            ]))
        }
    }

    fn starts_shape_or_ref(&self) -> bool {
        matches!(
            self.kind(),
            TokenKind::LBrace | TokenKind::At | TokenKind::AtPnameLn | TokenKind::AtPnameNs
        ) || self.at_any_kw(&["CLOSED", "EXTRA"])
    }

    fn shape_or_ref(&mut self, inline: bool) -> Result<ShapeExpr> {
        match self.kind() {
            TokenKind::At | TokenKind::AtPnameLn | TokenKind::AtPnameNs => self.shape_ref(),
            _ => self.shape_definition(inline),
        }
    }

    /// `@label`, `@ex:local` or `@ex:`.
    fn shape_ref(&mut self) -> Result<ShapeExpr> {
        let t = self.bump();
        match t.kind {
            TokenKind::At => {
                if self.kind() == TokenKind::LParen || !self.starts_label() {
                    return Err(self.expected(&["a shape label after '@'"]));
                }
                Ok(ShapeExpr::Ref(self.label()?))
            }
            _ => {
                let iri = self.pname(t.start as usize + 1, &self.text(t)[1..])?;
                Ok(ShapeExpr::Ref(Label::Iri(iri)))
            }
        }
    }

    /// `(EXTRA predicate+ | CLOSED)* '{' tripleExpression? '}'`, then annotations and
    /// semantic actions unless inline.
    fn shape_definition(&mut self, inline: bool) -> Result<ShapeExpr> {
        let mut shape = Shape::default();
        loop {
            if self.eat_kw("CLOSED") {
                shape.closed = Some(true);
            } else if self.eat_kw("EXTRA") {
                shape.extra.push(self.predicate()?);
                while self.starts_iri() || self.at_a() {
                    shape.extra.push(self.predicate()?);
                }
            } else if self.kind() == TokenKind::LBrace {
                self.bump();
                break;
            } else {
                return Err(self.expected(&["CLOSED", "EXTRA", "'{'"]));
            }
        }
        if self.kind() != TokenKind::RBrace {
            shape.expression = Some(self.triple_expression()?);
        }
        if self.kind() != TokenKind::RBrace {
            return Err(self.expected(&[
                "a cardinality",
                "an annotation ('//')",
                "a semantic action ('%')",
                "';'",
                "'|'",
                "'}'",
            ]));
        }
        self.bump();
        if !inline {
            shape.annotations = self.annotations()?;
            shape.sem_acts = self.sem_acts()?;
        }
        Ok(ShapeExpr::Shape(Box::new(shape)))
    }

    fn at_a(&self) -> bool {
        let t = self.peek();
        t.kind == TokenKind::Word && self.text(t) == "a"
    }

    // -------------------------------------------------- node constraints ------

    fn starts_non_lit(&self) -> bool {
        self.kind() == TokenKind::Regexp
            || self.at_any_kw(&[
                "IRI",
                "BNODE",
                "NONLITERAL",
                "LENGTH",
                "MINLENGTH",
                "MAXLENGTH",
            ])
    }

    fn starts_lit(&self) -> bool {
        self.starts_iri()
            || self.kind() == TokenKind::LBracket
            || self.at_kw("LITERAL")
            || self.starts_numeric_facet()
    }

    fn starts_string_facet(&self) -> bool {
        self.kind() == TokenKind::Regexp || self.at_any_kw(&["LENGTH", "MINLENGTH", "MAXLENGTH"])
    }

    fn starts_numeric_facet(&self) -> bool {
        self.at_any_kw(&[
            "MININCLUSIVE",
            "MINEXCLUSIVE",
            "MAXINCLUSIVE",
            "MAXEXCLUSIVE",
            "TOTALDIGITS",
            "FRACTIONDIGITS",
        ])
    }

    /// A literal node constraint (`LITERAL`, a datatype or a value set, then any facets;
    /// or numeric facets) or a non-literal one (a node kind then string facets; or
    /// string facets). Outline constraints may not carry annotations or semantic
    /// actions: ShExJ has no place for them.
    fn node_constraint(&mut self, inline: bool, literal: bool) -> Result<ShapeExpr> {
        let start = self.peek();
        let mut nc = NodeConstraint::default();
        // which facets may follow: (string, numeric)
        let facets = if literal {
            if self.eat_kw("LITERAL") {
                nc.node_kind = Some(NodeKind::Literal);
                (true, true)
            } else if self.starts_iri() {
                nc.datatype = Some(self.iri()?);
                (true, true)
            } else if self.kind() == TokenKind::LBracket {
                nc.values = Some(self.value_set()?);
                (true, true)
            } else {
                self.numeric_facet(&mut nc)?;
                (false, true)
            }
        } else {
            let kind = [
                ("IRI", NodeKind::Iri),
                ("BNODE", NodeKind::BNode),
                ("NONLITERAL", NodeKind::NonLiteral),
            ]
            .into_iter()
            .find(|(kw, _)| self.at_kw(kw));
            match kind {
                Some((_, k)) => {
                    self.bump();
                    nc.node_kind = Some(k);
                }
                None => self.string_facet(&mut nc)?,
            }
            (true, false)
        };
        loop {
            if facets.0 && self.starts_string_facet() {
                self.string_facet(&mut nc)?;
            } else if facets.1 && self.starts_numeric_facet() {
                self.numeric_facet(&mut nc)?;
            } else {
                break;
            }
        }
        check_facets(&nc).map_err(|m| self.error_tok(start, m))?;
        if !inline {
            let t = self.peek();
            if !self.annotations()?.is_empty() || !self.sem_acts()?.is_empty() {
                return Err(self.error_tok(
                    t,
                    "annotations and semantic actions on a node constraint are not supported",
                ));
            }
        }
        Ok(ShapeExpr::Nc(Box::new(nc)))
    }

    fn string_facet(&mut self, nc: &mut NodeConstraint) -> Result<()> {
        let t = self.bump();
        if t.kind == TokenKind::Regexp {
            let (pattern, flags) = self.regexp(t)?;
            set_once(&mut nc.pattern, pattern).map_err(|_| self.twice(t, "a pattern"))?;
            nc.flags = flags;
            return Ok(());
        }
        let n = self.unsigned()?;
        let slot = if self.is_kw(t, "LENGTH") {
            &mut nc.length
        } else if self.is_kw(t, "MINLENGTH") {
            &mut nc.min_length
        } else {
            &mut nc.max_length
        };
        let name = self.text(t).to_ascii_uppercase();
        set_once(slot, n).map_err(|_| self.twice(t, &name))
    }

    fn numeric_facet(&mut self, nc: &mut NodeConstraint) -> Result<()> {
        let t = self.bump();
        let name = self.text(t).to_ascii_uppercase();
        let slot = match name.as_str() {
            "TOTALDIGITS" | "FRACTIONDIGITS" => {
                let n = self.unsigned()?;
                let slot = if name == "TOTALDIGITS" {
                    &mut nc.total_digits
                } else {
                    &mut nc.fraction_digits
                };
                return set_once(slot, n).map_err(|_| self.twice(t, &name));
            }
            "MININCLUSIVE" => &mut nc.min_inclusive,
            "MINEXCLUSIVE" => &mut nc.min_exclusive,
            "MAXINCLUSIVE" => &mut nc.max_inclusive,
            _ => &mut nc.max_exclusive,
        };
        let v = self.peek();
        let text = self.text(v).to_string();
        let lit = match v.kind {
            TokenKind::Integer => NumericLiteral::Integer(text),
            TokenKind::Decimal => NumericLiteral::Decimal(text),
            TokenKind::Double => NumericLiteral::Double(text),
            _ => return Err(self.expected(&[&format!("a number after {name}")])),
        };
        self.bump();
        set_once(slot, lit).map_err(|_| self.twice(t, &name))
    }

    fn twice(&self, t: Token, what: &str) -> ParseError {
        self.error_tok(t, format!("{what} is given twice in one node constraint"))
    }

    /// An unsigned `INTEGER` (a length or a digit count).
    fn unsigned(&mut self) -> Result<u64> {
        let t = self.peek();
        let text = self.text(t);
        if t.kind != TokenKind::Integer || text.starts_with(['+', '-']) {
            return Err(self.expected(&["a non-negative integer"]));
        }
        self.bump();
        text.parse()
            .map_err(|_| self.error_tok(t, format!("{text} is too large")))
    }

    /// A `REGEXP` token's pattern (`\/` and `\u` escapes decoded, the others kept for
    /// the regular expression) and flags.
    fn regexp(&self, t: Token) -> Result<(String, Option<String>)> {
        let text = self.text(t);
        let close = text.rfind('/').expect("a regexp ends with '/'");
        let (body, flags) = (&text[1..close], &text[close + 1..]);
        let mut pattern = String::with_capacity(body.len());
        let mut i = 0;
        while i < body.len() {
            let rest = &body[i..];
            let c = rest.chars().next().expect("on a char boundary");
            if c != '\\' {
                pattern.push(c);
                i += c.len_utf8();
                continue;
            }
            match rest.as_bytes()[1] {
                b'/' => {
                    pattern.push('/');
                    i += 2;
                }
                b'u' | b'U' => {
                    let (c, n) = self.decode_uchar(rest, t.start as usize + 1 + i)?;
                    pattern.push(c);
                    i += n;
                }
                _ => {
                    pattern.push_str(&rest[..2]);
                    i += 2;
                }
            }
        }
        Ok((pattern, (!flags.is_empty()).then(|| flags.to_string())))
    }

    // --------------------------------------------------------- value sets ------

    fn value_set(&mut self) -> Result<Vec<ValueSetValue>> {
        self.bump();
        let mut values = Vec::new();
        while !self.eat(TokenKind::RBracket) {
            values.push(self.value_set_value()?);
        }
        Ok(values)
    }

    fn starts_literal(&self) -> bool {
        let t = self.peek();
        t.kind.is_string() || t.kind.is_number() || self.at_any_kw(&["true", "false"])
    }

    fn value_set_value(&mut self) -> Result<ValueSetValue> {
        let t = self.peek();
        if self.starts_iri() {
            let iri = self.iri()?;
            if !self.eat(TokenKind::Tilde) {
                return Ok(ValueSetValue::Object(ObjectValue::Iri(iri)));
            }
            let exclusions = self.exclusions(Kind::Iri)?;
            return Ok(if exclusions.is_empty() {
                ValueSetValue::IriStem(iri)
            } else {
                ValueSetValue::IriStemRange {
                    stem: Stem::Value(iri),
                    exclusions,
                }
            });
        }
        if self.starts_literal() {
            let lit = self.literal()?;
            if !self.eat(TokenKind::Tilde) {
                return Ok(ValueSetValue::Object(ObjectValue::Literal(lit)));
            }
            let exclusions = self.exclusions(Kind::Literal)?;
            return Ok(if exclusions.is_empty() {
                ValueSetValue::LiteralStem(lit.value)
            } else {
                ValueSetValue::LiteralStemRange {
                    stem: Stem::Value(lit.value),
                    exclusions,
                }
            });
        }
        let lang = match t.kind {
            TokenKind::LangTag => Some(self.text(t)[1..].to_string()),
            TokenKind::At if self.peek_at(1).kind == TokenKind::Tilde => Some(String::new()),
            _ => None,
        };
        if let Some(lang) = lang {
            self.bump();
            if !self.eat(TokenKind::Tilde) {
                return Ok(ValueSetValue::Language(lang));
            }
            let exclusions = self.exclusions(Kind::Language)?;
            return Ok(if exclusions.is_empty() {
                ValueSetValue::LanguageStem(lang)
            } else {
                ValueSetValue::LanguageStemRange {
                    stem: Stem::Value(lang),
                    exclusions,
                }
            });
        }
        if t.kind == TokenKind::Dot {
            self.bump();
            if self.kind() != TokenKind::Minus {
                return Err(self.expected(&["'-' (an exclusion after '.')"]));
            }
            // the first exclusion decides the kind of all of them
            self.bump();
            let kind = if self.starts_iri() {
                Kind::Iri
            } else if self.starts_literal() {
                Kind::Literal
            } else if self.kind() == TokenKind::LangTag {
                Kind::Language
            } else {
                return Err(self.expected(&["an IRI", "a literal", "a language tag"]));
            };
            self.pos -= 1;
            let exclusions = self.exclusions(kind)?;
            let stem = Stem::Wildcard;
            return Ok(match kind {
                Kind::Iri => ValueSetValue::IriStemRange { stem, exclusions },
                Kind::Literal => ValueSetValue::LiteralStemRange { stem, exclusions },
                Kind::Language => ValueSetValue::LanguageStemRange { stem, exclusions },
            });
        }
        Err(self.expected(&["an IRI", "a literal", "a language tag", "'.'", "']'"]))
    }

    /// `('-' value '~'?)*` of one kind.
    fn exclusions(&mut self, kind: Kind) -> Result<Vec<Exclusion>> {
        let mut out = Vec::new();
        while self.eat(TokenKind::Minus) {
            let value = match kind {
                Kind::Iri => {
                    if !self.starts_iri() {
                        return Err(self.expected(&["an IRI to exclude"]));
                    }
                    self.iri()?
                }
                Kind::Literal => {
                    if !self.starts_literal() {
                        return Err(self.expected(&["a literal to exclude"]));
                    }
                    self.literal()?.value
                }
                Kind::Language => {
                    let t = self.peek();
                    if t.kind != TokenKind::LangTag {
                        return Err(self.expected(&["a language tag to exclude"]));
                    }
                    self.bump();
                    self.text(t)[1..].to_string()
                }
            };
            out.push(if self.eat(TokenKind::Tilde) {
                Exclusion::Stem(value)
            } else {
                Exclusion::Value(value)
            });
        }
        Ok(out)
    }

    /// `literal`: a string with a language tag or a datatype, a number or a boolean.
    fn literal(&mut self) -> Result<ObjectLiteral> {
        let t = self.bump();
        let text = self.text(t);
        if t.kind.is_number() {
            let dt = match t.kind {
                TokenKind::Integer => "integer",
                TokenKind::Decimal => "decimal",
                _ => "double",
            };
            return Ok(ObjectLiteral {
                value: text.to_string(),
                language: None,
                datatype: Some(format!("{XSD}{dt}")),
            });
        }
        if t.kind == TokenKind::Word {
            return Ok(ObjectLiteral {
                value: text.to_ascii_lowercase(),
                language: None,
                datatype: Some(format!("{XSD}boolean")),
            });
        }
        let value = self.string(t)?;
        let mut lit = ObjectLiteral {
            value,
            language: None,
            datatype: None,
        };
        let next = self.peek();
        if next.kind == TokenKind::LangTag {
            self.bump();
            lit.language = Some(self.text(next)[1..].to_string());
        } else if self.eat(TokenKind::HatHat) {
            if !self.starts_iri() {
                return Err(self.expected(&["a datatype IRI after '^^'"]));
            }
            lit.datatype = Some(self.iri()?);
        }
        Ok(lit)
    }

    /// The value of a string token, escapes decoded.
    fn string(&self, t: Token) -> Result<String> {
        let text = self.text(t);
        let q = match t.kind {
            TokenKind::StringLong1 | TokenKind::StringLong2 => 3,
            _ => 1,
        };
        let body = &text[q..text.len() - q];
        let mut out = String::with_capacity(body.len());
        let mut rest = body;
        while let Some(i) = rest.find('\\') {
            out.push_str(&rest[..i]);
            let at = t.start as usize + q + (body.len() - rest.len()) + i;
            let esc = &rest[i..];
            let (c, n) = match esc.as_bytes().get(1) {
                Some(b't') => ('\t', 2),
                Some(b'b') => ('\u{8}', 2),
                Some(b'n') => ('\n', 2),
                Some(b'r') => ('\r', 2),
                Some(b'f') => ('\u{c}', 2),
                Some(b'"') => ('"', 2),
                Some(b'\'') => ('\'', 2),
                Some(b'\\') => ('\\', 2),
                Some(b'u' | b'U') => self.decode_uchar(esc, at)?,
                _ => {
                    let shown: String = esc.chars().take(2).collect();
                    return Err(self.error_at(at, format!("invalid escape '{shown}' in a string")));
                }
            };
            out.push(c);
            rest = &esc[n..];
        }
        out.push_str(rest);
        Ok(out)
    }

    // -------------------------------------------------- triple expressions ------

    fn starts_unary(&self) -> bool {
        matches!(
            self.kind(),
            TokenKind::Dollar | TokenKind::Amp | TokenKind::LParen | TokenKind::Hat
        ) || self.starts_iri()
            || self.at_a()
    }

    /// `tripleExpression`: groups separated by `|`.
    fn triple_expression(&mut self) -> Result<TripleExpr> {
        let first = self.group()?;
        if self.kind() != TokenKind::Pipe {
            return Ok(first);
        }
        let mut exprs = vec![first];
        while self.eat(TokenKind::Pipe) {
            exprs.push(self.group()?);
        }
        Ok(TripleExpr::OneOf(Group::of(exprs)))
    }

    /// Unary triple expressions separated (and optionally ended) by `;`.
    fn group(&mut self) -> Result<TripleExpr> {
        let first = self.unary()?;
        let mut exprs = vec![first];
        while self.eat(TokenKind::Semicolon) {
            if !self.starts_unary() {
                break;
            }
            exprs.push(self.unary()?);
        }
        Ok(if exprs.len() == 1 {
            exprs.pop().expect("one expression")
        } else {
            TripleExpr::EachOf(Group::of(exprs))
        })
    }

    fn unary(&mut self) -> Result<TripleExpr> {
        if self.eat(TokenKind::Amp) {
            if !self.starts_label() {
                return Err(self.expected(&["a triple expression label after '&'"]));
            }
            return Ok(TripleExpr::Include(self.label()?));
        }
        let id = if self.eat(TokenKind::Dollar) {
            if !self.starts_label() {
                return Err(self.expected(&["a triple expression label after '$'"]));
            }
            Some(self.label()?)
        } else {
            None
        };
        if self.kind() == TokenKind::LParen {
            return self.bracketed(id);
        }
        if !(self.kind() == TokenKind::Hat || self.starts_iri() || self.at_a()) {
            let mut what = vec!["a triple constraint", "'('"];
            if id.is_none() {
                what.extend(["'$'", "'&'"]);
            }
            return Err(self.expected(&what));
        }
        self.triple_constraint(id).map(TripleExpr::Tc)
    }

    fn triple_constraint(&mut self, id: Option<Label>) -> Result<TripleConstraint> {
        let inverse = self.eat(TokenKind::Hat).then_some(true);
        let predicate = self.predicate()?;
        // a lone `.` is "any value"
        let value_expr = if self.kind() == TokenKind::Dot
            && !(self.is_kw(self.peek_at(1), "AND") || self.is_kw(self.peek_at(1), "OR"))
        {
            self.bump();
            None
        } else {
            Some(Box::new(self.shape_expression(true)?))
        };
        let (min, max) = self.cardinality()?.unzip();
        let tc = TripleConstraint {
            id,
            inverse,
            predicate,
            value_expr,
            min,
            max,
            annotations: self.annotations()?,
            sem_acts: self.sem_acts()?,
        };
        self.end_of_unary(min.is_some(), !tc.sem_acts.is_empty())?;
        Ok(tc)
    }

    /// After a unary triple expression: a separator or the end of the enclosing group,
    /// or else an error listing what could still follow (a cardinality unless one was
    /// given, annotations unless semantic actions were).
    fn end_of_unary(&self, card: bool, acts: bool) -> Result<()> {
        if matches!(
            self.kind(),
            TokenKind::Semicolon | TokenKind::Pipe | TokenKind::RParen | TokenKind::RBrace
        ) {
            return Ok(());
        }
        let mut what = Vec::new();
        if !card {
            what.push("a cardinality");
        }
        if !acts {
            what.push("an annotation ('//')");
        }
        what.extend(["a semantic action ('%')", "';'", "'|'"]);
        what.push(if self.nest > 0 { "')'" } else { "'}'" });
        Err(self.expected(&what))
    }

    /// `'(' tripleExpression ')' cardinality? annotation* semanticActions`. The
    /// modifiers go on the inner expression when it has room for them, otherwise the
    /// inner expression is wrapped in a one-element `EachOf`.
    fn bracketed(&mut self, id: Option<Label>) -> Result<TripleExpr> {
        self.bump();
        self.nest += 1;
        let inner = self.triple_expression()?;
        if self.kind() != TokenKind::RParen {
            return Err(self.expected(&["';'", "'|'", "')'"]));
        }
        self.bump();
        self.nest -= 1;
        let card = self.cardinality()?;
        let annotations = self.annotations()?;
        let sem_acts = self.sem_acts()?;
        self.end_of_unary(card.is_some(), !sem_acts.is_empty())?;
        let fits = |eid: &Option<Label>, min: &Option<u32>, max: &Option<i64>, acts: &[SemAct]| {
            (id.is_none() || eid.is_none())
                && (card.is_none() || (min.is_none() && max.is_none() && acts.is_empty()))
        };
        let mut inner = inner;
        let merged = match &mut inner {
            TripleExpr::EachOf(g) | TripleExpr::OneOf(g)
                if fits(&g.id, &g.min, &g.max, &g.sem_acts) =>
            {
                apply(&mut g.id, &mut g.min, &mut g.max, id.clone(), card);
                g.annotations.extend(annotations.iter().cloned());
                g.sem_acts.extend(sem_acts.iter().cloned());
                true
            }
            TripleExpr::Tc(tc) if fits(&tc.id, &tc.min, &tc.max, &[]) => {
                apply(&mut tc.id, &mut tc.min, &mut tc.max, id.clone(), card);
                tc.annotations.extend(annotations.iter().cloned());
                tc.sem_acts.extend(sem_acts.iter().cloned());
                true
            }
            _ => false,
        };
        if merged {
            return Ok(inner);
        }
        let (min, max) = card.unzip();
        Ok(TripleExpr::EachOf(Group {
            id,
            exprs: vec![inner],
            min,
            max,
            sem_acts,
            annotations,
        }))
    }

    /// `*`, `+`, `?` or `{m,n}`: `(min, max)` with `-1` for unbounded.
    fn cardinality(&mut self) -> Result<Option<(u32, i64)>> {
        let t = self.peek();
        let card = match t.kind {
            TokenKind::Star => (0, -1),
            TokenKind::Plus => (1, -1),
            TokenKind::Question => (0, 1),
            TokenKind::RepeatRange => {
                let text = self.text(t);
                let body = &text[1..text.len() - 1];
                let too_large = || self.error_tok(t, format!("cardinality {text} is too large"));
                let (min, max) = match body.split_once(',') {
                    None => (body, Some(body)),
                    Some((min, "" | "*")) => (min, None),
                    Some((min, max)) => (min, Some(max)),
                };
                let min: u32 = min.parse().map_err(|_| too_large())?;
                let max: i64 = match max {
                    None => -1,
                    Some(m) => m.parse::<u32>().map(i64::from).map_err(|_| too_large())?,
                };
                (min, max)
            }
            _ => return Ok(None),
        };
        self.bump();
        Ok(Some(card))
    }

    // --------------------------------------- annotations, semantic actions ------

    fn annotations(&mut self) -> Result<Vec<Annotation>> {
        let mut out = Vec::new();
        while self.eat(TokenKind::SlashSlash) {
            let predicate = self.predicate()?;
            let object = if self.starts_iri() {
                ObjectValue::Iri(self.iri()?)
            } else if self.starts_literal() {
                ObjectValue::Literal(self.literal()?)
            } else {
                return Err(self.expected(&["an IRI", "a literal"]));
            };
            out.push(Annotation { predicate, object });
        }
        Ok(out)
    }

    fn sem_acts(&mut self) -> Result<Vec<SemAct>> {
        let mut out = Vec::new();
        while self.kind() == TokenKind::Code {
            out.push(self.sem_act()?);
        }
        Ok(out)
    }

    /// A `CODE` token: `%` name (`{` code `%}` | `%`). In the code, `\%` and `\\` are
    /// `%` and `\`, `\u` escapes are decoded and other backslashes are kept.
    fn sem_act(&mut self) -> Result<SemAct> {
        let t = self.bump();
        let text = self.text(t);
        let after = text[1..].trim_start();
        let name_at = text.len() - after.len();
        let (kind, n) = lex(after)
            .first()
            .map(|tok| (tok.kind, tok.len as usize))
            .expect("the lexer found a name");
        let name_text = &after[..n];
        let name = match kind {
            TokenKind::IriRef => self.iriref(Token {
                kind,
                start: t.start + name_at as u32,
                len: n as u32,
            })?,
            _ => self.pname(t.start as usize + name_at, name_text)?,
        };
        let rest = after[n..].trim_start();
        if rest == "%" {
            return Ok(SemAct { name, code: None });
        }
        let body = &rest[1..rest.len() - 2];
        let body_at = t.start as usize + text.len() - rest.len() + 1;
        let mut code = String::with_capacity(body.len());
        let mut i = 0;
        while i < body.len() {
            let rest = &body[i..];
            let c = rest.chars().next().expect("on a char boundary");
            if c != '\\' {
                code.push(c);
                i += c.len_utf8();
                continue;
            }
            match rest[1..].chars().next() {
                Some(e @ ('%' | '\\')) => {
                    code.push(e);
                    i += 2;
                }
                Some('u' | 'U') if uchar(rest.as_bytes()).is_some() => {
                    let (c, n) = self.decode_uchar(rest, body_at + i)?;
                    code.push(c);
                    i += n;
                }
                Some(e) => {
                    code.push('\\');
                    code.push(e);
                    i += 1 + e.len_utf8();
                }
                None => {
                    code.push('\\');
                    i += 1;
                }
            }
        }
        Ok(SemAct {
            name,
            code: Some(code),
        })
    }
}

/// The kinds of value-set ranges.
#[derive(Clone, Copy)]
enum Kind {
    Iri,
    Literal,
    Language,
}

impl Group {
    fn of(exprs: Vec<TripleExpr>) -> Group {
        Group {
            id: None,
            exprs,
            min: None,
            max: None,
            sem_acts: Vec::new(),
            annotations: Vec::new(),
        }
    }
}

fn set_once<T>(slot: &mut Option<T>, v: T) -> std::result::Result<(), ()> {
    if slot.is_some() {
        return Err(());
    }
    *slot = Some(v);
    Ok(())
}

fn apply(
    eid: &mut Option<Label>,
    min: &mut Option<u32>,
    max: &mut Option<i64>,
    id: Option<Label>,
    card: Option<(u32, i64)>,
) {
    if id.is_some() {
        *eid = id;
    }
    if let Some((lo, hi)) = card {
        *min = Some(lo);
        *max = Some(hi);
    }
}

#[cfg(test)]
#[path = "parser_tests.rs"]
mod tests;
