//! Parser for the Jena rule language (`org.apache.jena.reasoner.rulesys.Rule`).
//!
//! Supported syntax:
//!
//! ```text
//! @prefix ex: <http://example.org/> .
//! @include <rdfs> .                       # built-in rule sets: rdfs, rdfs-simple, owl, owl-rl, owlmini, owlmicro
//! [name: (?a ex:p ?b) (?b ex:q ?c) notEqual(?a, ?c) -> (?a ex:r ?c)]
//! [(?a ex:p ?b) -> (?b ex:p ?a)]           # unnamed
//! (?a ex:p ?b) -> (?a ex:s ?b) .           # bare form, terminated by '.'
//! -> (ex:A rdf:type rdfs:Class) .          # axiom
//! [r: (?x ex:p ?y) -> [(?y ex:q ?x) <- (?x ex:r ?y)]]   # nested rule in a head
//! [b: (?a ex:q ?b) <- (?b ex:p ?a)]        # backward rule (parsed; not materialized)
//! ```
//!
//! Terms: `?var`, `<iri>`, `prefix:local` (default prefixes `rdf`, `rdfs`, `owl`, `xsd`,
//! `rb`), `'lit'` / `"lit"`, `'x'@en`, `'1'^^xsd:int`, bare numbers (`1` → `xsd:integer`,
//! `1.5` / `1e3` → `xsd:double`), `_` / `*` wildcards, `_:label` blank nodes and functor
//! terms `name(args…)`. Comments start with `#` or `//` and run to the end of the line.

use crate::{OWL_RL_RULES, RDFS_RULES, RDFS_SIMPLE_RULES};
use oxrdf::vocab::xsd;
use oxrdf::{BlankNode, Literal, NamedNode, Term};
use std::collections::HashMap;
use std::fmt;

pub const RDF_NS: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#";
pub const RDFS_NS: &str = "http://www.w3.org/2000/01/rdf-schema#";
pub const OWL_NS: &str = "http://www.w3.org/2002/07/owl#";
pub const XSD_NS: &str = "http://www.w3.org/2001/XMLSchema#";
/// Jena's reasoner-internal namespace (`rb:`), predefined for compatibility.
pub const RB_NS: &str = "http://jena.hpl.hp.com/reasoner#";

/// A node in a rule clause.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Node {
    /// `?name` (stored without the `?`)
    Var(String),
    /// IRI, literal or blank node constant
    Const(Term),
    /// `_` / `*`: matches anything, binds nothing
    Any,
    /// Jena functor term `name(args…)` (structured literal; not supported by the
    /// forward engine)
    Functor(String, Vec<Node>),
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct TriplePattern {
    pub subject: Node,
    pub predicate: Node,
    pub object: Node,
}

/// A builtin call such as `notEqual(?a, ?b)`.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct BuiltinCall {
    pub name: String,
    pub args: Vec<Node>,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Clause {
    Triple(TriplePattern),
    Builtin(BuiltinCall),
    /// A nested rule (only meaningful in a head).
    Rule(Box<Rule>),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Direction {
    /// `body -> head`
    Forward,
    /// `head <- body`
    Backward,
}

/// A parsed rule. `body` and `head` are always stored semantically (for a backward
/// rule, `head` is the part left of `<-`).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Rule {
    pub name: Option<String>,
    pub body: Vec<Clause>,
    pub head: Vec<Clause>,
    pub direction: Direction,
    /// 1-based line of the rule's first token
    pub line: usize,
}

impl Rule {
    /// `name` or `rule@line N`.
    pub fn label(&self) -> String {
        match &self.name {
            Some(n) => n.clone(),
            None => format!("rule@line {}", self.line),
        }
    }
}

/// Rule syntax error with a 1-based position.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("rule syntax error at line {line}, column {column}: {message}")]
pub struct RuleParseError {
    pub line: usize,
    pub column: usize,
    pub message: String,
}

// ------------------------------------------------------------------ display ----

impl fmt::Display for Node {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Node::Var(v) => write!(f, "?{v}"),
            Node::Any => f.write_str("_"),
            Node::Const(Term::NamedNode(n)) => {
                for (p, ns) in [
                    ("rdf", RDF_NS),
                    ("rdfs", RDFS_NS),
                    ("owl", OWL_NS),
                    ("xsd", XSD_NS),
                ] {
                    if let Some(local) = n.as_str().strip_prefix(ns)
                        && !local.is_empty()
                        && local
                            .chars()
                            .all(|c| c.is_alphanumeric() || c == '_' || c == '-')
                    {
                        return write!(f, "{p}:{local}");
                    }
                }
                write!(f, "{n}")
            }
            Node::Const(Term::Literal(l)) => {
                let lex = l.value().replace('\\', "\\\\").replace('\'', "\\'");
                if let Some(lang) = l.language() {
                    write!(f, "'{lex}'@{lang}")
                } else if l.datatype() == xsd::STRING {
                    write!(f, "'{lex}'")
                } else {
                    write!(
                        f,
                        "'{lex}'^^{}",
                        Node::Const(Term::NamedNode(l.datatype().into_owned()))
                    )
                }
            }
            Node::Const(t) => write!(f, "{t}"),
            Node::Functor(name, args) => {
                write!(f, "{name}(")?;
                for (i, a) in args.iter().enumerate() {
                    if i > 0 {
                        f.write_str(", ")?;
                    }
                    write!(f, "{a}")?;
                }
                f.write_str(")")
            }
        }
    }
}

impl fmt::Display for Clause {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Clause::Triple(t) => write!(f, "({} {} {})", t.subject, t.predicate, t.object),
            Clause::Builtin(b) => write!(f, "{}", Node::Functor(b.name.clone(), b.args.clone())),
            Clause::Rule(r) => write!(f, "{r}"),
        }
    }
}

impl fmt::Display for Rule {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("[")?;
        if let Some(n) = &self.name {
            write!(f, "{n}: ")?;
        }
        let join = |cs: &[Clause]| {
            cs.iter()
                .map(|c| c.to_string())
                .collect::<Vec<_>>()
                .join(" ")
        };
        match self.direction {
            Direction::Forward => write!(f, "{} -> {}", join(&self.body), join(&self.head))?,
            Direction::Backward => write!(f, "{} <- {}", join(&self.head), join(&self.body))?,
        }
        f.write_str("]")
    }
}

// ------------------------------------------------------------------ lexer ------

#[derive(Clone, Debug, PartialEq)]
enum Tok {
    LParen,
    RParen,
    LBrack,
    RBrack,
    Comma,
    Arrow,
    BackArrow,
    Dot,
    Iri(String),
    Word(String),
    Directive(String),
    Lit {
        lex: String,
        lang: Option<String>,
        /// raw datatype token (`<iri>` content or a prefixed name) and whether it was an IRI
        dt: Option<(String, bool)>,
    },
}

impl fmt::Display for Tok {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Tok::LParen => f.write_str("'('"),
            Tok::RParen => f.write_str("')'"),
            Tok::LBrack => f.write_str("'['"),
            Tok::RBrack => f.write_str("']'"),
            Tok::Comma => f.write_str("','"),
            Tok::Arrow => f.write_str("'->'"),
            Tok::BackArrow => f.write_str("'<-'"),
            Tok::Dot => f.write_str("'.'"),
            Tok::Iri(i) => write!(f, "<{i}>"),
            Tok::Word(w) => write!(f, "'{w}'"),
            Tok::Directive(d) => write!(f, "'{d}'"),
            Tok::Lit { lex, .. } => write!(f, "literal '{lex}'"),
        }
    }
}

#[derive(Clone, Debug)]
struct Token {
    tok: Tok,
    line: usize,
    col: usize,
}

fn err(line: usize, column: usize, message: impl Into<String>) -> RuleParseError {
    RuleParseError {
        line,
        column,
        message: message.into(),
    }
}

fn is_word_end(c: char) -> bool {
    c.is_whitespace() || matches!(c, '(' | ')' | '[' | ']' | ',' | '\'' | '"')
}

fn lex(text: &str) -> Result<Vec<Token>, RuleParseError> {
    let chars: Vec<char> = text.chars().collect();
    let mut toks = Vec::new();
    let (mut i, mut line, mut col) = (0usize, 1usize, 1usize);
    // advance helper
    macro_rules! bump {
        () => {{
            let c = chars[i];
            i += 1;
            if c == '\n' {
                line += 1;
                col = 1;
            } else {
                col += 1;
            }
            c
        }};
    }
    while i < chars.len() {
        let c = chars[i];
        if c.is_whitespace() {
            bump!();
            continue;
        }
        let (tl, tc) = (line, col);
        let push = |toks: &mut Vec<Token>, tok: Tok| {
            toks.push(Token {
                tok,
                line: tl,
                col: tc,
            })
        };
        // comments
        if c == '#' || (c == '/' && chars.get(i + 1) == Some(&'/')) {
            while i < chars.len() && chars[i] != '\n' {
                bump!();
            }
            continue;
        }
        match c {
            '(' => {
                bump!();
                push(&mut toks, Tok::LParen);
            }
            ')' => {
                bump!();
                push(&mut toks, Tok::RParen);
            }
            '[' => {
                bump!();
                push(&mut toks, Tok::LBrack);
            }
            ']' => {
                bump!();
                push(&mut toks, Tok::RBrack);
            }
            ',' => {
                bump!();
                push(&mut toks, Tok::Comma);
            }
            '<' if chars.get(i + 1) == Some(&'-') => {
                bump!();
                bump!();
                push(&mut toks, Tok::BackArrow);
            }
            '<' => {
                bump!();
                let mut iri = String::new();
                loop {
                    match chars.get(i) {
                        None => return Err(err(tl, tc, "unterminated IRI (missing '>')")),
                        Some('>') => {
                            bump!();
                            break;
                        }
                        Some(ch) if ch.is_whitespace() => {
                            return Err(err(tl, tc, "whitespace inside IRI (missing '>'?)"));
                        }
                        Some(_) => iri.push(bump!()),
                    }
                }
                push(&mut toks, Tok::Iri(iri));
            }
            '\'' | '"' => {
                let q = bump!();
                let mut lex = String::new();
                loop {
                    match chars.get(i) {
                        None => {
                            return Err(err(
                                tl,
                                tc,
                                format!("unterminated string literal (missing {q})"),
                            ));
                        }
                        Some(&ch) if ch == q => {
                            bump!();
                            break;
                        }
                        Some('\\') => {
                            bump!();
                            let Some(&e) = chars.get(i) else {
                                return Err(err(tl, tc, "unterminated string literal"));
                            };
                            bump!();
                            lex.push(match e {
                                'n' => '\n',
                                't' => '\t',
                                'r' => '\r',
                                'b' => '\u{8}',
                                'f' => '\u{c}',
                                'u' | 'U' => {
                                    let n = if e == 'u' { 4 } else { 8 };
                                    let mut hex = String::new();
                                    for _ in 0..n {
                                        match chars.get(i) {
                                            Some(h) if h.is_ascii_hexdigit() => hex.push(bump!()),
                                            _ => return Err(err(line, col, "invalid \\u escape")),
                                        }
                                    }
                                    u32::from_str_radix(&hex, 16)
                                        .ok()
                                        .and_then(char::from_u32)
                                        .ok_or_else(|| err(line, col, "invalid \\u escape"))?
                                }
                                other => other,
                            });
                        }
                        Some(_) => lex.push(bump!()),
                    }
                }
                let mut lang = None;
                let mut dt = None;
                if chars.get(i) == Some(&'@') {
                    bump!();
                    let mut l = String::new();
                    while let Some(&ch) = chars.get(i) {
                        if ch.is_ascii_alphanumeric() || ch == '-' {
                            l.push(bump!());
                        } else {
                            break;
                        }
                    }
                    if l.is_empty() {
                        return Err(err(line, col, "empty language tag after '@'"));
                    }
                    lang = Some(l);
                } else if chars.get(i) == Some(&'^') && chars.get(i + 1) == Some(&'^') {
                    bump!();
                    bump!();
                    if chars.get(i) == Some(&'<') {
                        bump!();
                        let mut iri = String::new();
                        loop {
                            match chars.get(i) {
                                Some('>') => {
                                    bump!();
                                    break;
                                }
                                Some(ch) if !ch.is_whitespace() => iri.push(bump!()),
                                _ => return Err(err(line, col, "unterminated datatype IRI")),
                            }
                        }
                        dt = Some((iri, true));
                    } else {
                        let mut w = String::new();
                        while let Some(&ch) = chars.get(i) {
                            if is_word_end(ch) {
                                break;
                            }
                            w.push(bump!());
                        }
                        if w.is_empty() {
                            return Err(err(line, col, "missing datatype after '^^'"));
                        }
                        dt = Some((w, false));
                    }
                }
                push(&mut toks, Tok::Lit { lex, lang, dt });
            }
            _ => {
                let mut w = String::new();
                while let Some(&ch) = chars.get(i) {
                    if is_word_end(ch) {
                        break;
                    }
                    w.push(bump!());
                }
                let tok = match w.as_str() {
                    "->" => Tok::Arrow,
                    "." => Tok::Dot,
                    _ if w.starts_with('@') => Tok::Directive(w),
                    _ => Tok::Word(w),
                };
                push(&mut toks, tok);
            }
        }
    }
    Ok(toks)
}

// ------------------------------------------------------------------ parser -----

struct Parser {
    toks: Vec<Token>,
    pos: usize,
    prefixes: HashMap<String, String>,
    /// position of the end of input (for EOF errors)
    eof: (usize, usize),
    depth: usize,
}

const MAX_INCLUDE_DEPTH: usize = 8;

impl Parser {
    fn peek(&self) -> Option<&Token> {
        self.toks.get(self.pos)
    }

    fn peek_tok(&self) -> Option<&Tok> {
        self.peek().map(|t| &t.tok)
    }

    fn next(&mut self) -> Result<Token, RuleParseError> {
        let t = self
            .toks
            .get(self.pos)
            .cloned()
            .ok_or_else(|| err(self.eof.0, self.eof.1, "unexpected end of input"))?;
        self.pos += 1;
        Ok(t)
    }

    fn skip_commas(&mut self) {
        while self.peek_tok() == Some(&Tok::Comma) {
            self.pos += 1;
        }
    }

    fn here(&self) -> (usize, usize) {
        self.peek().map_or(self.eof, |t| (t.line, t.col))
    }

    fn parse_all(&mut self, out: &mut Vec<Rule>) -> Result<(), RuleParseError> {
        loop {
            self.skip_commas();
            let Some(t) = self.peek().cloned() else {
                return Ok(());
            };
            match &t.tok {
                Tok::Directive(d) => {
                    self.pos += 1;
                    match d.as_str() {
                        "@prefix" => self.parse_prefix(&t)?,
                        "@include" => self.parse_include(&t, out)?,
                        other => {
                            return Err(err(
                                t.line,
                                t.col,
                                format!(
                                    "unknown directive '{other}' (expected @prefix or @include)"
                                ),
                            ));
                        }
                    }
                }
                Tok::Dot => {
                    // stray terminator, e.g. `[rule] .`
                    self.pos += 1;
                }
                _ => {
                    let r = self.parse_rule(false)?;
                    out.push(r);
                }
            }
        }
    }

    fn parse_prefix(&mut self, at: &Token) -> Result<(), RuleParseError> {
        let t = self.next()?;
        let Tok::Word(w) = &t.tok else {
            return Err(err(
                t.line,
                t.col,
                format!("expected a prefix name after @prefix, found {}", t.tok),
            ));
        };
        let name = w.strip_suffix(':').unwrap_or(w).to_string();
        if name.contains(':') {
            return Err(err(t.line, t.col, format!("invalid prefix name '{w}'")));
        }
        let t = self.next()?;
        let Tok::Iri(iri) = &t.tok else {
            return Err(err(
                t.line,
                t.col,
                format!("expected <namespace IRI> in @prefix, found {}", t.tok),
            ));
        };
        self.prefixes.insert(name, iri.clone());
        if self.peek_tok() == Some(&Tok::Dot) {
            self.pos += 1;
        }
        let _ = at;
        Ok(())
    }

    fn parse_include(&mut self, at: &Token, out: &mut Vec<Rule>) -> Result<(), RuleParseError> {
        let t = self.next()?;
        let name = match &t.tok {
            Tok::Iri(i) | Tok::Word(i) => i.clone(),
            _ => {
                return Err(err(
                    t.line,
                    t.col,
                    format!("expected <name> after @include, found {}", t.tok),
                ));
            }
        };
        if self.peek_tok() == Some(&Tok::Dot) {
            self.pos += 1;
        }
        let text = match name.to_ascii_lowercase().as_str() {
            "rdfs" => RDFS_RULES,
            "rdfs-simple" | "rdfssimple" => RDFS_SIMPLE_RULES,
            "owl" | "owl-rl" | "owlrl" | "owlmini" | "owlmicro" | "owl-mini" | "owl-micro" => {
                OWL_RL_RULES
            }
            _ => {
                return Err(err(
                    at.line,
                    at.col,
                    format!(
                        "cannot @include <{name}>: only the built-in rule sets rdfs, rdfs-simple, owl, owl-rl, owlmini and owlmicro can be included"
                    ),
                ));
            }
        };
        if self.depth >= MAX_INCLUDE_DEPTH {
            return Err(err(at.line, at.col, "@include nested too deeply"));
        }
        let mut sub = Parser {
            toks: lex(text).expect("built-in rules lex"),
            pos: 0,
            prefixes: default_prefixes(),
            eof: (0, 0),
            depth: self.depth + 1,
        };
        sub.parse_all(out).map_err(|e| {
            err(
                at.line,
                at.col,
                format!("in included rule set <{name}>: {e}"),
            )
        })
    }

    fn parse_rule(&mut self, nested: bool) -> Result<Rule, RuleParseError> {
        let (line, col) = self.here();
        let bracketed = if self.peek_tok() == Some(&Tok::LBrack) {
            self.pos += 1;
            true
        } else {
            false
        };
        let mut name = None;
        if let Some(Tok::Word(w)) = self.peek_tok()
            && w.len() > 1
            && w.ends_with(':')
        {
            name = Some(w[..w.len() - 1].to_string());
            self.pos += 1;
        }
        // left side
        let mut left = Vec::new();
        let arrow = loop {
            self.skip_commas();
            match self.peek_tok() {
                Some(Tok::Arrow) => break Direction::Forward,
                Some(Tok::BackArrow) => break Direction::Backward,
                Some(Tok::RBrack) | Some(Tok::Dot) | None => {
                    let (l, c) = self.here();
                    let what = match &name {
                        Some(n) => format!("rule '{n}'"),
                        None => "rule".to_string(),
                    };
                    return Err(err(
                        l,
                        c,
                        format!("{what} starting at line {line} has no '->' or '<-'"),
                    ));
                }
                _ => left.push(self.parse_clause()?),
            }
        };
        self.pos += 1; // arrow
        let mut right = Vec::new();
        loop {
            self.skip_commas();
            match self.peek_tok() {
                Some(Tok::RBrack) if bracketed => {
                    self.pos += 1;
                    break;
                }
                Some(Tok::RBrack) => {
                    let (l, c) = self.here();
                    return Err(err(l, c, "unexpected ']' (rule was not opened with '[')"));
                }
                Some(Tok::Dot) if !bracketed => {
                    self.pos += 1;
                    break;
                }
                Some(Tok::Dot) => {
                    let (l, c) = self.here();
                    return Err(err(
                        l,
                        c,
                        format!(
                            "expected ']' to close the rule opened at line {line}, column {col}"
                        ),
                    ));
                }
                None if !bracketed && !nested => break,
                None => {
                    return Err(err(line, col, "unterminated rule (missing ']')"));
                }
                Some(Tok::Arrow) | Some(Tok::BackArrow) => {
                    let (l, c) = self.here();
                    return Err(err(
                        l,
                        c,
                        "a rule may contain only one '->' or '<-' (did you forget ']' or '.'?)",
                    ));
                }
                _ => right.push(self.parse_clause()?),
            }
        }
        let (body, head) = match arrow {
            Direction::Forward => (left, right),
            Direction::Backward => (right, left),
        };
        Ok(Rule {
            name,
            body,
            head,
            direction: arrow,
            line,
        })
    }

    fn parse_clause(&mut self) -> Result<Clause, RuleParseError> {
        let t = self
            .peek()
            .cloned()
            .ok_or_else(|| err(self.eof.0, self.eof.1, "unexpected end of input"))?;
        match &t.tok {
            Tok::LParen => {
                let nodes = self.parse_node_list()?;
                if nodes.len() != 3 {
                    return Err(err(
                        t.line,
                        t.col,
                        format!("triple pattern with {} nodes (expected 3)", nodes.len()),
                    ));
                }
                let mut it = nodes.into_iter();
                let (s, p, o) = (it.next().unwrap(), it.next().unwrap(), it.next().unwrap());
                if matches!(s, Node::Functor(..)) {
                    return Err(err(
                        t.line,
                        t.col,
                        "functors are not allowed in the subject position of a pattern",
                    ));
                }
                if matches!(p, Node::Functor(..)) {
                    return Err(err(
                        t.line,
                        t.col,
                        "functors are not allowed in the predicate position of a pattern",
                    ));
                }
                if let Node::Const(Term::Literal(_)) = p {
                    return Err(err(t.line, t.col, "a literal cannot be a predicate"));
                }
                Ok(Clause::Triple(TriplePattern {
                    subject: s,
                    predicate: p,
                    object: o,
                }))
            }
            Tok::LBrack => Ok(Clause::Rule(Box::new(self.parse_rule(true)?))),
            Tok::Word(w) if !w.starts_with('?') => {
                self.pos += 1;
                if self.peek_tok() != Some(&Tok::LParen) {
                    return Err(err(
                        t.line,
                        t.col,
                        format!(
                            "expected '(' after builtin name '{w}' (clauses are triple patterns '(s p o)', builtins 'name(args)' or nested rules '[...]')"
                        ),
                    ));
                }
                let args = self.parse_node_list()?;
                Ok(Clause::Builtin(BuiltinCall {
                    name: w.clone(),
                    args,
                }))
            }
            other => Err(err(
                t.line,
                t.col,
                format!(
                    "expected a triple pattern '(s p o)', a builtin call or a nested rule, found {other}"
                ),
            )),
        }
    }

    fn parse_node_list(&mut self) -> Result<Vec<Node>, RuleParseError> {
        let open = self.next()?;
        if open.tok != Tok::LParen {
            return Err(err(
                open.line,
                open.col,
                format!("expected '(', found {}", open.tok),
            ));
        }
        let mut nodes = Vec::new();
        loop {
            self.skip_commas();
            let t = self
                .next()
                .map_err(|_| err(open.line, open.col, "unterminated '(' (missing ')')"))?;
            match t.tok {
                Tok::RParen => return Ok(nodes),
                Tok::LParen
                | Tok::LBrack
                | Tok::RBrack
                | Tok::Arrow
                | Tok::BackArrow
                | Tok::Dot => {
                    return Err(err(
                        t.line,
                        t.col,
                        format!(
                            "unexpected {} inside '(…)' opened at line {}, column {} (missing ')'?)",
                            t.tok, open.line, open.col
                        ),
                    ));
                }
                _ => nodes.push(self.parse_node(t)?),
            }
        }
    }

    fn expand(&self, word: &str, t: &Token) -> Result<Option<String>, RuleParseError> {
        let Some((prefix, local)) = word.split_once(':') else {
            return Ok(None);
        };
        if let Some(ns) = self.prefixes.get(prefix) {
            return Ok(Some(format!("{ns}{local}")));
        }
        if matches!(
            prefix,
            "http" | "https" | "urn" | "file" | "ftp" | "mailto" | "tag"
        ) {
            return Ok(Some(word.to_string()));
        }
        Err(err(
            t.line,
            t.col,
            format!(
                "unknown prefix '{prefix}:' in '{word}' (declare it with @prefix {prefix}: <…> .)"
            ),
        ))
    }

    fn iri(&self, iri: &str, t: &Token) -> Result<NamedNode, RuleParseError> {
        NamedNode::new(iri).map_err(|e| err(t.line, t.col, format!("invalid IRI <{iri}>: {e}")))
    }

    fn parse_node(&mut self, t: Token) -> Result<Node, RuleParseError> {
        match &t.tok {
            Tok::Iri(i) => Ok(Node::Const(Term::NamedNode(self.iri(i, &t)?))),
            Tok::Lit { lex, lang, dt } => {
                let lit = if let Some(l) = lang {
                    Literal::new_language_tagged_literal(lex.clone(), l.clone()).map_err(|e| {
                        err(t.line, t.col, format!("invalid language tag '{l}': {e}"))
                    })?
                } else if let Some((d, is_iri)) = dt {
                    let iri = if *is_iri {
                        d.clone()
                    } else {
                        self.expand(d, &t)?.ok_or_else(|| {
                            err(
                                t.line,
                                t.col,
                                format!("datatype '{d}' must be a prefixed name or <IRI>"),
                            )
                        })?
                    };
                    Literal::new_typed_literal(lex.clone(), self.iri(&iri, &t)?)
                } else {
                    Literal::new_simple_literal(lex.clone())
                };
                Ok(Node::Const(Term::Literal(lit)))
            }
            Tok::Word(w) => {
                if let Some(v) = w.strip_prefix('?') {
                    if v.is_empty() {
                        return Err(err(t.line, t.col, "empty variable name '?'"));
                    }
                    return Ok(Node::Var(v.to_string()));
                }
                if w == "_" || w == "*" {
                    return Ok(Node::Any);
                }
                if let Some(label) = w.strip_prefix("_:") {
                    let b = BlankNode::new(label).map_err(|e| {
                        err(t.line, t.col, format!("invalid blank node '{w}': {e}"))
                    })?;
                    return Ok(Node::Const(Term::BlankNode(b)));
                }
                let first = w.chars().next().unwrap_or(' ');
                let second = w.chars().nth(1);
                if first.is_ascii_digit()
                    || (matches!(first, '-' | '+') && second.is_some_and(|c| c.is_ascii_digit()))
                {
                    return parse_number(w)
                        .ok_or_else(|| err(t.line, t.col, format!("invalid number '{w}'")));
                }
                if self.peek_tok() == Some(&Tok::LParen) && !w.contains(':') {
                    let args = self.parse_node_list()?;
                    return Ok(Node::Functor(w.clone(), args));
                }
                match self.expand(w, &t)? {
                    Some(iri) => Ok(Node::Const(Term::NamedNode(self.iri(&iri, &t)?))),
                    None => Err(err(
                        t.line,
                        t.col,
                        format!(
                            "unexpected '{w}': expected ?variable, <IRI>, prefix:name, 'literal', number or _"
                        ),
                    )),
                }
            }
            other => Err(err(t.line, t.col, format!("unexpected {other}"))),
        }
    }
}

fn parse_number(w: &str) -> Option<Node> {
    let is_float = w.contains(['.', 'e', 'E']);
    let lit = if is_float {
        let v: f64 = w.parse().ok()?;
        if !v.is_finite() {
            return None;
        }
        Literal::new_typed_literal(oxsdatatypes::Double::from(v).to_string(), xsd::DOUBLE)
    } else {
        let v: i128 = w.parse().ok()?;
        Literal::new_typed_literal(v.to_string(), xsd::INTEGER)
    };
    Some(Node::Const(Term::Literal(lit)))
}

pub(crate) fn default_prefixes() -> HashMap<String, String> {
    [
        ("rdf", RDF_NS),
        ("rdfs", RDFS_NS),
        ("owl", OWL_NS),
        ("xsd", XSD_NS),
        ("rb", RB_NS),
    ]
    .into_iter()
    .map(|(a, b)| (a.to_string(), b.to_string()))
    .collect()
}

/// Parse Jena rule text into rules (including `@include`d built-in rule sets).
pub fn parse_rules(text: &str) -> Result<Vec<Rule>, RuleParseError> {
    let toks = lex(text)?;
    let eof = {
        let lines: Vec<&str> = text.split('\n').collect();
        (
            lines.len().max(1),
            lines.last().map_or(0, |l| l.chars().count()) + 1,
        )
    };
    let mut p = Parser {
        toks,
        pos: 0,
        prefixes: default_prefixes(),
        eof,
        depth: 0,
    };
    let mut out = Vec::new();
    p.parse_all(&mut out)?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn iri(s: &str) -> Node {
        Node::Const(Term::NamedNode(NamedNode::new_unchecked(s)))
    }

    #[test]
    fn named_rule_with_builtin() {
        let rules = parse_rules(
            "@prefix ex: <http://ex.org/> .\n\
             # a comment\n\
             [r1: (?a ex:p ?b), (?b ex:q ?c) notEqual(?a, ?c) -> (?a ex:r ?c)]",
        )
        .unwrap();
        assert_eq!(rules.len(), 1);
        let r = &rules[0];
        assert_eq!(r.name.as_deref(), Some("r1"));
        assert_eq!(r.line, 3);
        assert_eq!(r.direction, Direction::Forward);
        assert_eq!(r.body.len(), 3);
        assert_eq!(r.head.len(), 1);
        match &r.body[0] {
            Clause::Triple(t) => {
                assert_eq!(t.subject, Node::Var("a".into()));
                assert_eq!(t.predicate, iri("http://ex.org/p"));
            }
            other => panic!("{other:?}"),
        }
        match &r.body[2] {
            Clause::Builtin(b) => {
                assert_eq!(b.name, "notEqual");
                assert_eq!(b.args, vec![Node::Var("a".into()), Node::Var("c".into())]);
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn unnamed_bare_axioms_and_terms() {
        let text = r#"
            // line comment
            [ (?x rdf:type owl:Class) -> (?x rdfs:subClassOf owl:Thing) ]
            (?a <http://ex.org/p> ?b) -> (?b <http://ex.org/p> ?a) .
            -> (<http://ex.org/a> rdfs:label 'hi'@en) .
            -> (<http://ex.org/a> <http://ex.org/n> '1'^^xsd:int) .
            -> (<http://ex.org/a> <http://ex.org/n> 42), (<http://ex.org/a> <http://ex.org/n> -1.5) .
            [w: (?a _ ?b) (?b * "x\"y") -> (?a <http://ex.org/r> ?b)]
        "#;
        let rules = parse_rules(text).unwrap();
        assert_eq!(rules.len(), 6);
        assert!(rules[0].name.is_none());
        assert!(rules[2].body.is_empty());
        let Clause::Triple(t) = &rules[2].head[0] else {
            panic!()
        };
        assert_eq!(
            t.object,
            Node::Const(Term::Literal(
                Literal::new_language_tagged_literal_unchecked("hi", "en")
            ))
        );
        let Clause::Triple(t) = &rules[3].head[0] else {
            panic!()
        };
        assert_eq!(
            t.object,
            Node::Const(Term::Literal(Literal::new_typed_literal("1", xsd::INT)))
        );
        let Clause::Triple(t) = &rules[4].head[0] else {
            panic!()
        };
        assert_eq!(
            t.object,
            Node::Const(Term::Literal(Literal::new_typed_literal(
                "42",
                xsd::INTEGER
            )))
        );
        let Clause::Triple(t) = &rules[4].head[1] else {
            panic!()
        };
        assert_eq!(
            t.object,
            Node::Const(Term::Literal(Literal::new_typed_literal(
                "-1.5",
                xsd::DOUBLE
            )))
        );
        let Clause::Triple(t) = &rules[5].body[0] else {
            panic!()
        };
        assert_eq!(t.predicate, Node::Any);
        let Clause::Triple(t) = &rules[5].body[1] else {
            panic!()
        };
        assert_eq!(
            t.object,
            Node::Const(Term::Literal(Literal::new_simple_literal("x\"y")))
        );
    }

    #[test]
    fn backward_and_nested() {
        let rules = parse_rules(
            "[b: (?a rdfs:subClassOf ?c) <- (?a rdfs:subClassOf ?b), (?b rdfs:subClassOf ?c)]\n\
             [n: (?p rdf:type owl:SymmetricProperty) -> [sym: (?x ?p ?y) <- (?y ?p ?x)]]",
        )
        .unwrap();
        assert_eq!(rules[0].direction, Direction::Backward);
        assert_eq!(rules[0].head.len(), 1);
        assert_eq!(rules[0].body.len(), 2);
        let Clause::Rule(n) = &rules[1].head[0] else {
            panic!()
        };
        assert_eq!(n.name.as_deref(), Some("sym"));
        assert_eq!(n.direction, Direction::Backward);
        // display roundtrip
        let again = parse_rules(&rules[1].to_string()).unwrap();
        assert_eq!(again[0].to_string(), rules[1].to_string());
        assert_eq!(again[0].body, rules[1].body);
    }

    #[test]
    fn functor_terms() {
        let rules =
            parse_rules("[(?C owl:onProperty ?P) -> (?C owl:equivalentClass some(?P, ?D))]")
                .unwrap();
        let Clause::Triple(t) = &rules[0].head[0] else {
            panic!()
        };
        assert!(matches!(&t.object, Node::Functor(n, a) if n == "some" && a.len() == 2));
    }

    #[test]
    fn include_builtin_sets() {
        let rules = parse_rules(
            "@include <rdfs>.\n[x: (?a <http://ex.org/p> ?b) -> (?b <http://ex.org/p> ?a)]",
        )
        .unwrap();
        assert!(rules.len() > 10);
        assert_eq!(rules.last().unwrap().name.as_deref(), Some("x"));
    }

    #[test]
    fn errors_have_positions() {
        let e = parse_rules("[r: (?a ex:p ?b) -> (?a ?b)]").unwrap_err();
        assert_eq!(e.line, 1);
        assert!(e.message.contains("unknown prefix 'ex:'"), "{e}");

        let e = parse_rules("\n\n[r: (?a <http://x/p> ?b) -> (?a ?b)]").unwrap_err();
        assert_eq!((e.line, e.column), (3, 29));
        assert!(e.message.contains("2 nodes"), "{e}");

        let e = parse_rules("[r: (?a <http://x/p> ?b) (?b <http://x/p> ?c)]").unwrap_err();
        assert!(e.message.contains("no '->'"), "{e}");

        let e = parse_rules("[r: (?a <http://x/p ?b) -> (?a <http://x/p> ?b)]").unwrap_err();
        assert!(e.message.contains("IRI"), "{e}");

        let e = parse_rules("[r: (?a <http://x/p> 'abc) -> ]").unwrap_err();
        assert!(e.message.contains("unterminated string"), "{e}");

        let e = parse_rules("[r: (?a <http://x/p> ?b) -> (?b <http://x/p> ?a)").unwrap_err();
        assert!(e.message.contains("missing ']'"), "{e}");

        let e = parse_rules("@include <http://example.org/my.rules> .").unwrap_err();
        assert!(e.message.contains("@include"), "{e}");

        let e = parse_rules("[r: (?a <http://x/p> ?b) foo -> (?a <http://x/p> ?b)]").unwrap_err();
        assert!(e.message.contains("builtin name 'foo'"), "{e}");

        let e = parse_rules(
            "[r: (?a <http://x/p> ?b) -> (?a <http://x/p> ?b) -> (?b <http://x/p> ?a)]",
        )
        .unwrap_err();
        assert!(e.message.contains("only one"), "{e}");

        let e = parse_rules("@frobnicate <x> .").unwrap_err();
        assert!(e.message.contains("unknown directive"), "{e}");
    }
}
