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

/// An atom `name(args)` of a relation that a `.decl` declares (spec C24 §6.2), a
/// relation of the reasoner's rule program (item 839).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct RelationAtom {
    pub name: String,
    pub args: Vec<Node>,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Clause {
    Triple(TriplePattern),
    Builtin(BuiltinCall),
    /// A nested rule (only meaningful in a head).
    Rule(Box<Rule>),
    /// An atom of a declared relation.
    Relation(RelationAtom),
    /// `not` before a triple pattern or a relation atom in a body: stratified negation.
    Not(Box<Clause>),
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
    /// the built-in rule set that an `@include` brought the rule from, if one did
    pub include: Option<String>,
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
            Clause::Relation(a) => write!(f, "{}", Node::Functor(a.name.clone(), a.args.clone())),
            Clause::Not(c) => write!(f, "not {c}"),
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
    /// `.decl`, which declares a relation
    Decl,
    /// `:-`, between the head and the body of a Datalog clause
    Neck,
    /// `?-`, which begins a query
    Query,
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
            Tok::Decl => f.write_str("'.decl'"),
            Tok::Neck => f.write_str("':-'"),
            Tok::Query => f.write_str("'?-'"),
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
                    ".decl" => Tok::Decl,
                    ":-" => Tok::Neck,
                    "?-" => Tok::Query,
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
    /// `@hybrid jena`: nested backward rules become query-time rules (spec F17 §5.2)
    hybrid_jena: bool,
    /// the relations that `.decl` declares, with their arity and where
    decls: HashMap<String, Decl>,
    /// the indices, among the rules parsed, of the Datalog clauses `head :- body.`
    necks: Vec<usize>,
}

/// A relation's declaration: its arity and the position of its name.
#[derive(Clone, Copy)]
struct Decl {
    arity: usize,
    line: usize,
    col: usize,
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
                        "@hybrid" => self.parse_hybrid()?,
                        other => {
                            return Err(err(
                                t.line,
                                t.col,
                                format!(
                                    "unknown directive '{other}' (expected @prefix, @include or \
                                     @hybrid)"
                                ),
                            ));
                        }
                    }
                }
                Tok::Dot => {
                    // stray terminator, e.g. `[rule] .`
                    self.pos += 1;
                }
                Tok::Decl => {
                    self.pos += 1;
                    self.parse_decl(&t)?;
                }
                Tok::Query => {
                    return Err(err(
                        t.line,
                        t.col,
                        "a query '?-' cannot be part of a rule set (queries over rules are \
                         roadmap item 72's Datalog query language)",
                    ));
                }
                _ => {
                    let at = self.pos;
                    let r = self.parse_rule(false)?;
                    if self.toks[at..self.pos].iter().any(|t| t.tok == Tok::Neck) && self.depth == 0
                    {
                        self.necks.push(out.len());
                    }
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

    /// `.decl name(col, …)`: a relation of the given arity (spec C24 §6.2). The column
    /// names only document the relation.
    fn parse_decl(&mut self, at: &Token) -> Result<(), RuleParseError> {
        let t = self.next()?;
        let Tok::Word(name) = &t.tok else {
            return Err(err(
                t.line,
                t.col,
                format!("expected a relation name after .decl, found {}", t.tok),
            ));
        };
        check_relation_name(name, &t)?;
        if self.peek_tok() != Some(&Tok::LParen) {
            let (l, c) = self.here();
            return Err(err(
                l,
                c,
                format!("expected '(' and the columns of relation '{name}'"),
            ));
        }
        self.pos += 1;
        let mut arity = 0usize;
        loop {
            self.skip_commas();
            let c = self
                .next()
                .map_err(|_| err(at.line, at.col, "unterminated .decl (missing ')')"))?;
            match &c.tok {
                Tok::RParen => break,
                Tok::Word(w) if is_identifier(w) => arity += 1,
                other => {
                    return Err(err(
                        c.line,
                        c.col,
                        format!("expected a column name in .decl of '{name}', found {other}"),
                    ));
                }
            }
        }
        if arity >= u16::MAX as usize {
            return Err(err(
                t.line,
                t.col,
                format!("relation '{name}' has {arity} columns, more than a relation can have"),
            ));
        }
        if let Some(d) = self.decls.get(name.as_str())
            && d.arity != arity
        {
            return Err(err(
                t.line,
                t.col,
                format!(
                    "relation '{name}' is declared with {arity} columns here and with {} at line {}, column {}",
                    d.arity, d.line, d.col
                ),
            ));
        }
        self.decls.entry(name.clone()).or_insert(Decl {
            arity,
            line: t.line,
            col: t.col,
        });
        if self.peek_tok() == Some(&Tok::Dot) {
            self.pos += 1;
        }
        Ok(())
    }

    /// `@hybrid jena .` or `@hybrid materialize .`
    fn parse_hybrid(&mut self) -> Result<(), RuleParseError> {
        let t = self.next()?;
        let mode = match &t.tok {
            Tok::Word(w) => w.trim_end_matches('.').to_string(),
            _ => String::new(),
        };
        self.hybrid_jena = match mode.as_str() {
            "jena" => true,
            "materialize" => false,
            _ => {
                return Err(err(
                    t.line,
                    t.col,
                    format!(
                        "expected jena or materialize after @hybrid, found {}",
                        t.tok
                    ),
                ));
            }
        };
        if self.peek_tok() == Some(&Tok::Dot) {
            self.pos += 1;
        }
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
            hybrid_jena: false,
            decls: HashMap::new(),
            necks: Vec::new(),
        };
        let first = out.len();
        sub.parse_all(out).map_err(|e| {
            err(
                at.line,
                at.col,
                format!("in included rule set <{name}>: {e}"),
            )
        })?;
        for r in &mut out[first..] {
            if r.include.is_none() {
                r.include = Some(name.to_ascii_lowercase());
            }
        }
        Ok(())
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
        let mut neck = false;
        let arrow = loop {
            self.skip_commas();
            match self.peek_tok() {
                Some(Tok::Arrow) => break Direction::Forward,
                Some(Tok::BackArrow) => break Direction::Backward,
                Some(Tok::Neck) if !bracketed && !nested => {
                    // `head :- body.`: a Datalog clause, a forward rule (spec C24 §6.2)
                    neck = true;
                    break Direction::Forward;
                }
                Some(Tok::Neck) => {
                    let (l, c) = self.here();
                    return Err(err(
                        l,
                        c,
                        "':-' begins the body of a Datalog clause, which is not written in \
                         brackets (use '->' or '<-' inside '[…]')",
                    ));
                }
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
                None if neck => {
                    return Err(err(
                        line,
                        col,
                        "unterminated Datalog clause (missing '.' after its body)",
                    ));
                }
                None if !bracketed && !nested => break,
                None => {
                    return Err(err(line, col, "unterminated rule (missing ']')"));
                }
                Some(Tok::Arrow) | Some(Tok::BackArrow) | Some(Tok::Neck) => {
                    let (l, c) = self.here();
                    return Err(err(
                        l,
                        c,
                        "a rule may contain only one '->', '<-' or ':-' (did you forget ']' or '.'?)",
                    ));
                }
                _ => right.push(self.parse_clause()?),
            }
        }
        let (body, head) = match (neck, arrow) {
            (true, _) => {
                if left.len() != 1 || matches!(left[0], Clause::Rule(_) | Clause::Not(_)) {
                    return Err(err(
                        line,
                        col,
                        "the head of a Datalog clause ':-' is one relation atom or triple pattern",
                    ));
                }
                (right, left)
            }
            (false, Direction::Forward) => (left, right),
            (false, Direction::Backward) => (right, left),
        };
        if let Some(c) = head.iter().find(|c| matches!(c, Clause::Not(_))) {
            return Err(err(
                line,
                col,
                format!("'not' cannot be part of a rule's head ({c})"),
            ));
        }
        Ok(Rule {
            name,
            body,
            head,
            direction: arrow,
            line,
            include: None,
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
            Tok::Word(w)
                if w == "not"
                    && matches!(
                        self.toks.get(self.pos + 1).map(|t| &t.tok),
                        Some(Tok::LParen | Tok::Word(_))
                    ) =>
            {
                // `not (s p o)` or `not name(args)`; the name is checked against the
                // declarations once the whole text is read
                self.pos += 1;
                let inner = self.parse_clause()?;
                match inner {
                    Clause::Triple(_) | Clause::Builtin(_) => Ok(Clause::Not(Box::new(inner))),
                    other => Err(err(
                        t.line,
                        t.col,
                        format!(
                            "'not' applies to a triple pattern or a relation atom, not {other}"
                        ),
                    )),
                }
            }
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
        hybrid_jena: false,
        decls: HashMap::new(),
        necks: Vec::new(),
    };
    let mut out = Vec::new();
    p.parse_all(&mut out)?;
    for r in &mut out {
        resolve(r, &p.decls)?;
    }
    for &i in &p.necks {
        check_clause(&out[i])?;
    }
    if p.hybrid_jena {
        out = split_hybrid(out)?;
    }
    Ok(out)
}

/// Whether `w` is an identifier, `[A-Za-z_][A-Za-z0-9_]*`.
fn is_identifier(w: &str) -> bool {
    let mut cs = w.chars();
    cs.next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && cs.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Refuse a relation name that is no identifier, or that names a builtin, a head action
/// or a keyword, since an atom `name(args)` must say which it is.
fn check_relation_name(name: &str, t: &Token) -> Result<(), RuleParseError> {
    if !is_identifier(name) {
        return Err(err(
            t.line,
            t.col,
            format!("relation name '{name}' is not an identifier (letters, digits and '_')"),
        ));
    }
    let taken = matches!(name, "not" | "_" | "bound" | "unbound")
        || crate::builtins::BuiltinKind::from_name(name).is_some()
        || crate::builtins::HeadAction::from_name(name).is_some();
    if taken {
        return Err(err(
            t.line,
            t.col,
            format!("relation name '{name}' is the name of a builtin or a keyword"),
        ));
    }
    Ok(())
}

/// `?_` in a relation atom or a negated atom is anonymous, as in C24 §6.2: each occurrence
/// matches anything, as `_` does. Elsewhere `?_` keeps Jena's reading, a variable.
fn anonymous(args: &mut [Node]) {
    for n in args {
        if matches!(n, Node::Var(v) if v == "_") {
            *n = Node::Any;
        }
    }
}

/// Turn each call of a declared relation in `r` into a relation atom and check its
/// arity, and refuse a `not` before anything else (spec C24 §6.2).
fn resolve(r: &mut Rule, decls: &HashMap<String, Decl>) -> Result<(), RuleParseError> {
    fn clause(
        c: &mut Clause,
        decls: &HashMap<String, Decl>,
        line: usize,
        label: &dyn Fn() -> String,
    ) -> Result<(), RuleParseError> {
        match c {
            Clause::Builtin(b) => {
                if let Some(d) = decls.get(&b.name) {
                    if b.args.len() != d.arity {
                        return Err(err(
                            line,
                            1,
                            format!(
                                "rule '{}': relation '{}' has {} columns, and an atom gives it {}",
                                label(),
                                b.name,
                                d.arity,
                                b.args.len()
                            ),
                        ));
                    }
                    let mut args = std::mem::take(&mut b.args);
                    anonymous(&mut args);
                    *c = Clause::Relation(RelationAtom {
                        name: std::mem::take(&mut b.name),
                        args,
                    });
                }
            }
            Clause::Not(inner) => {
                if let Clause::Builtin(b) = &**inner
                    && !decls.contains_key(&b.name)
                {
                    return Err(err(
                        line,
                        1,
                        format!(
                            "rule '{}': 'not' applies to a triple pattern or a declared relation, and '{}' is not declared (.decl {}(…))",
                            label(),
                            b.name,
                            b.name
                        ),
                    ));
                }
                clause(inner, decls, line, label)?;
                if let Clause::Triple(t) = &mut **inner {
                    for n in [&mut t.subject, &mut t.predicate, &mut t.object] {
                        anonymous(std::slice::from_mut(n));
                    }
                }
            }
            Clause::Rule(r) => resolve(r, decls)?,
            Clause::Triple(_) | Clause::Relation(_) => {}
        }
        Ok(())
    }
    let line = r.line;
    let name = r.label();
    let label = || name.clone();
    for c in r.body.iter_mut().chain(r.head.iter_mut()) {
        clause(c, decls, line, &label)?;
    }
    for c in &r.head {
        if let Clause::Relation(a) = c
            && a.args.iter().any(|n| matches!(n, Node::Any))
        {
            return Err(err(
                line,
                1,
                format!(
                    "rule '{}': the head atom {c} has an anonymous argument, and a derived fact needs a value in every column",
                    label()
                ),
            ));
        }
    }
    Ok(())
}

/// Refuse a Datalog clause `head :- body.` whose head is not a triple pattern or a
/// declared relation, or names a variable that no body clause names: an existential head
/// needs the restricted chase, which the reasoner does not have.
fn check_clause(r: &Rule) -> Result<(), RuleParseError> {
    let head = &r.head[0];
    let nodes: Vec<&Node> = match head {
        Clause::Triple(t) => vec![&t.subject, &t.predicate, &t.object],
        Clause::Relation(a) => a.args.iter().collect(),
        Clause::Builtin(b) => {
            return Err(err(
                r.line,
                1,
                format!(
                    "the head of a Datalog clause is a triple pattern or a declared relation, and '{}' is not declared (.decl {}(…))",
                    b.name, b.name
                ),
            ));
        }
        _ => unreachable!("checked by the parser"),
    };
    fn names(c: &Clause, out: &mut std::collections::HashSet<String>) {
        let node = |n: &Node| {
            if let Node::Var(v) = n {
                out.insert(v.clone());
            }
        };
        match c {
            Clause::Triple(t) => [&t.subject, &t.predicate, &t.object]
                .into_iter()
                .for_each(node),
            Clause::Relation(a) => a.args.iter().for_each(node),
            Clause::Builtin(b) => b.args.iter().for_each(node),
            Clause::Not(c) => names(c, out),
            Clause::Rule(_) => {}
        }
    }
    let mut body = std::collections::HashSet::new();
    r.body.iter().for_each(|c| names(c, &mut body));
    for n in nodes {
        if let Node::Var(v) = n
            && !body.contains(v)
        {
            return Err(err(
                r.line,
                1,
                format!(
                    "the head of the Datalog clause at line {} names ?{v}, which its body does not: existential heads need the restricted chase, which the reasoner does not have",
                    r.line
                ),
            ));
        }
    }
    Ok(())
}

/// Whether `rules` use the syntax of a rule program (spec C24 §6.2, item 839): a
/// relation atom or a `not`, in a rule or a nested rule. Rules without either are Jena
/// rules, and every path reads them as before.
pub fn uses_program(rules: &[Rule]) -> bool {
    fn clauses(cs: &[Clause]) -> bool {
        cs.iter().any(|c| match c {
            Clause::Relation(_) | Clause::Not(_) => true,
            Clause::Rule(r) => clauses(&r.body) || clauses(&r.head),
            Clause::Triple(_) | Clause::Builtin(_) => false,
        })
    }
    rules.iter().any(|r| clauses(&r.body) || clauses(&r.head))
}

/// The rule text of `rules`: a `.decl` for each relation their atoms use, with its arity,
/// and then each rule as [`Rule`]'s `Display` writes it, so that [`parse_rules`] reads it
/// back as `rules`, up to the rules' lines and included sets.
pub fn rules_text(rules: &[Rule]) -> String {
    fn collect(cs: &[Clause], out: &mut Vec<(String, usize)>) {
        for c in cs {
            match c {
                Clause::Relation(a) => {
                    if !out.iter().any(|(n, _)| *n == a.name) {
                        out.push((a.name.clone(), a.args.len()));
                    }
                }
                Clause::Not(inner) => collect(std::slice::from_ref(&**inner), out),
                Clause::Rule(r) => {
                    collect(&r.body, out);
                    collect(&r.head, out);
                }
                Clause::Triple(_) | Clause::Builtin(_) => {}
            }
        }
    }
    let mut rels = Vec::new();
    for r in rules {
        collect(&r.body, &mut rels);
        collect(&r.head, &mut rels);
    }
    let mut out = String::new();
    for (name, arity) in rels {
        out.push_str(".decl ");
        out.push_str(&name);
        out.push('(');
        for i in 0..arity {
            if i > 0 {
                out.push_str(", ");
            }
            out.push_str(&format!("c{i}"));
        }
        out.push_str(")\n");
    }
    for r in rules {
        out.push_str(&r.to_string());
        out.push('\n');
    }
    out
}

/// The rules of `@hybrid jena` (spec F17 §5.2): a forward rule `B1 -> [H <- B2]` keeps
/// its other head clauses for materialization, and its nested backward rule becomes the
/// top-level backward rule `H <- B1, B2`, which only query-time reasoning runs. The
/// answers are those of Jena's hybrid engine, which adds `H <- B2` for each match of `B1`,
/// without keeping the instantiated rules. A rule whose split cannot be stated this way is
/// refused: a functor term, a builtin with a side effect, one that makes terms or
/// `noValue`, whose answer Jena's backward engine reads differently, a head variable that
/// no body clause binds, or a backward rule nested deeper than a forward rule's head.
fn split_hybrid(rules: Vec<Rule>) -> Result<Vec<Rule>, RuleParseError> {
    fn refuse(r: &Rule, why: &str) -> RuleParseError {
        err(
            r.line,
            1,
            format!(
                "rule '{}': @hybrid jena cannot make a query-time rule of it, since {why}",
                r.label()
            ),
        )
    }
    fn functor(clauses: &[Clause]) -> bool {
        clauses.iter().any(|c| match c {
            Clause::Triple(t) => [&t.subject, &t.predicate, &t.object]
                .into_iter()
                .any(|n| matches!(n, Node::Functor(..))),
            Clause::Builtin(b) => b.args.iter().any(|n| matches!(n, Node::Functor(..))),
            Clause::Rule(r) => functor(&r.body) || functor(&r.head),
            Clause::Relation(a) => a.args.iter().any(|n| matches!(n, Node::Functor(..))),
            Clause::Not(c) => functor(std::slice::from_ref(&**c)),
        })
    }
    /// A builtin that has a side effect, makes terms, or reads the facts in a way that a
    /// query-time fixpoint evaluates differently from Jena's backward engine.
    fn effect(clauses: &[Clause]) -> Option<String> {
        clauses.iter().find_map(|c| match c {
            Clause::Builtin(b)
                if matches!(
                    b.name.as_str(),
                    "print"
                        | "drop"
                        | "remove"
                        | "hide"
                        | "makeTemp"
                        | "makeSkolem"
                        | "makeInstance"
                        | "now"
                        | "noValue"
                ) =>
            {
                Some(b.name.clone())
            }
            Clause::Rule(r) => effect(&r.body).or_else(|| effect(&r.head)),
            _ => None,
        })
    }
    /// Whether a backward rule is nested below a nested forward rule.
    fn deep_backward(head: &[Clause]) -> bool {
        head.iter().any(|c| match c {
            Clause::Rule(i) if i.direction == Direction::Forward => {
                i.head
                    .iter()
                    .any(|c| matches!(c, Clause::Rule(j) if j.direction == Direction::Backward))
                    || deep_backward(&i.head)
            }
            _ => false,
        })
    }
    fn vars(clauses: &[Clause], out: &mut std::collections::HashSet<String>) {
        fn node(n: &Node, out: &mut std::collections::HashSet<String>) {
            match n {
                Node::Var(v) => {
                    out.insert(v.clone());
                }
                Node::Functor(_, args) => args.iter().for_each(|a| node(a, out)),
                _ => {}
            }
        }
        for c in clauses {
            match c {
                Clause::Triple(t) => {
                    [&t.subject, &t.predicate, &t.object]
                        .into_iter()
                        .for_each(|n| node(n, out));
                }
                Clause::Builtin(b) => b.args.iter().for_each(|n| node(n, out)),
                Clause::Relation(a) => a.args.iter().for_each(|n| node(n, out)),
                Clause::Rule(_) | Clause::Not(_) => {}
            }
        }
    }
    /// The variables a body binds: those of its triples, and the outputs of each builtin
    /// whose inputs are bound, until no more are.
    fn bound_by(clauses: &[Clause], out: &mut std::collections::HashSet<String>) {
        for c in clauses {
            if let Clause::Triple(_) = c {
                vars(std::slice::from_ref(c), out);
            }
        }
        loop {
            let before = out.len();
            for c in clauses {
                let Clause::Builtin(b) = c else { continue };
                let n = b.args.len();
                let Some(kind) = crate::builtins::BuiltinKind::from_name(&b.name) else {
                    continue;
                };
                // an empty call binds nothing, and the compiler reports its arity
                if n == 0 {
                    continue;
                }
                let ready = kind
                    .input_positions(n)
                    .into_iter()
                    .all(|i| match b.args.get(i) {
                        Some(Node::Var(v)) => out.contains(v),
                        _ => true,
                    });
                if !ready {
                    continue;
                }
                for i in kind.output_positions(n) {
                    if let Some(Node::Var(v)) = b.args.get(i) {
                        out.insert(v.clone());
                    }
                }
            }
            if out.len() == before {
                break;
            }
        }
    }
    let mut out = Vec::with_capacity(rules.len());
    for r in rules {
        if r.direction == Direction::Forward && deep_backward(&r.head) {
            return Err(refuse(
                &r,
                "a backward rule is nested inside a nested forward rule",
            ));
        }
        let nested_backward = r
            .head
            .iter()
            .any(|c| matches!(c, Clause::Rule(i) if i.direction == Direction::Backward));
        if r.direction != Direction::Forward || !nested_backward {
            out.push(r);
            continue;
        }
        // the whole rule is checked, the forward head it keeps as well as the split
        if uses_program(std::slice::from_ref(&r)) {
            return Err(refuse(&r, "it holds a relation atom or 'not'"));
        }
        if functor(&r.body) || functor(&r.head) {
            return Err(refuse(&r, "it holds a functor term"));
        }
        if let Some(b) = effect(&r.body).or_else(|| effect(&r.head)) {
            return Err(refuse(&r, &format!("it calls {b}")));
        }
        let outer: Vec<Clause> = r
            .body
            .iter()
            .filter(|c| !matches!(c, Clause::Rule(_)))
            .cloned()
            .collect();
        let mut head = Vec::new();
        let mut made = Vec::new();
        for (n, c) in r.head.iter().enumerate() {
            match c {
                Clause::Rule(inner) if inner.direction == Direction::Backward => {
                    if inner
                        .head
                        .iter()
                        .chain(&inner.body)
                        .any(|c| matches!(c, Clause::Rule(_)))
                    {
                        return Err(refuse(&r, "its backward rule holds a nested rule"));
                    }
                    if inner.head.iter().any(|c| !matches!(c, Clause::Triple(_))) {
                        return Err(refuse(&r, "its backward rule's head is not a triple"));
                    }
                    let mut body = outer.clone();
                    body.extend(inner.body.iter().cloned());
                    // a head variable that no body clause names would need Jena's goal
                    // binding, which a bottom-up rule does not have
                    let mut bound = std::collections::HashSet::new();
                    let mut head_vars = std::collections::HashSet::new();
                    bound_by(&body, &mut bound);
                    vars(&inner.head, &mut head_vars);
                    if let Some(v) = head_vars.iter().find(|v| !bound.contains(*v)) {
                        return Err(refuse(
                            &r,
                            &format!(
                                "its backward rule's head names ?{v}, which no body clause binds"
                            ),
                        ));
                    }
                    made.push(Rule {
                        name: Some(
                            inner
                                .name
                                .clone()
                                .unwrap_or_else(|| format!("{}/{}", r.label(), n + 1)),
                        ),
                        body,
                        head: inner.head.clone(),
                        direction: Direction::Backward,
                        line: inner.line,
                        include: inner.include.clone(),
                    });
                }
                c => head.push(c.clone()),
            }
        }
        if !head.is_empty() {
            out.push(Rule { head, ..r });
        }
        out.extend(made);
    }
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

    const P: &str = "@prefix ex: <http://ex.org/> .\n";

    fn program(text: &str) -> Vec<Rule> {
        parse_rules(&format!("{P}{text}")).unwrap_or_else(|e| panic!("{e}"))
    }

    fn refused(text: &str) -> String {
        parse_rules(&format!("{P}{text}")).unwrap_err().message
    }

    /// `.decl`, relation atoms before and after their declaration, `not` of both kinds,
    /// `:-` clauses and relations of arity 0 and 5 (spec C24 §6.2).
    #[test]
    fn rule_programs() {
        let rs = program(
            "r(?x, ?y) :- (?x ex:p ?y).
             .decl r(a, b)
             .decl flag()
             .decl five(a, b, c, d, e)
             flag() :- (ex:a ex:p ex:b).
             [w: (?x ex:p ?y), not r(?y, ?x), not (?y ex:q ?x), flag() -> five(?x, ?y, ?x, ?y, ?x)]
             (?x ex:s ?y) :- five(?x, ?y, ?_, _, ?x), not r(?x, ?_).",
        );
        assert!(uses_program(&rs));
        assert_eq!(rs.len(), 4);
        assert!(
            matches!(&rs[0].head[0], Clause::Relation(a) if a.name == "r" && a.args.len() == 2)
        );
        assert_eq!(rs[0].direction, Direction::Forward);
        assert!(matches!(&rs[1].head[0], Clause::Relation(a) if a.args.is_empty()));
        let w = &rs[2];
        assert!(matches!(&w.body[1], Clause::Not(c) if matches!(&**c, Clause::Relation(_))));
        assert!(matches!(&w.body[2], Clause::Not(c) if matches!(&**c, Clause::Triple(_))));
        // `?_` is anonymous in relation atoms and negated atoms
        let Clause::Relation(five) = &rs[3].body[0] else {
            panic!()
        };
        assert_eq!(five.args[2], Node::Any);
        assert_eq!(five.args[3], Node::Any);
        let Clause::Not(n) = &rs[3].body[1] else {
            panic!()
        };
        let Clause::Relation(r) = &**n else { panic!() };
        assert_eq!(r.args[1], Node::Any);
        // Jena rules alone are no program, and keep `?_` a variable
        let jena = program("[j: (?x ex:p ?_) -> (?x ex:q ?_)]");
        assert!(!uses_program(&jena));
        let Clause::Triple(t) = &jena[0].body[0] else {
            panic!()
        };
        assert_eq!(t.object, Node::Var("_".into()));
        // the program reads back from its text
        let again = parse_rules(&rules_text(&rs)).unwrap();
        let show = |rs: &[Rule]| rs.iter().map(|r| r.to_string()).collect::<Vec<_>>();
        assert_eq!(show(&rs), show(&again));
    }

    #[test]
    fn rule_program_refusals() {
        for (text, want) in [
            (".decl r(a) .decl r(a, b)", "declared with 2 columns"),
            (".decl noValue(a)", "name of a builtin"),
            (".decl not(a)", "name of a builtin or a keyword"),
            (".decl listMapAsSubject(a)", "name of a builtin"),
            (".decl ex:r(a)", "not an identifier"),
            (
                ".decl r(a) r(?x) :- (?x ex:p ?y), r(?x, ?y).",
                "has 1 columns, and an atom gives it 2",
            ),
            (
                "[n: (?x ex:p ?y), not s(?x) -> (?x ex:q ?y)]",
                "'s' is not declared",
            ),
            ("s(?x) :- (?x ex:p ?y).", "'s' is not declared"),
            (".decl r(a) r(?y) :- (?x ex:p ex:o).", "existential heads"),
            ("(?x ex:q ?z) :- (?x ex:p ?y).", "existential heads"),
            (
                ".decl r(a, b) r(?x, ?_) :- (?x ex:p ?y).",
                "anonymous argument",
            ),
            (
                ".decl r(a, b) [h: (?x ex:p ?y) -> r(?x, _)]",
                "anonymous argument",
            ),
            ("[n: (?x ex:p ?y) -> not (?x ex:q ?y)]", "head"),
            ("?- (?x ex:p ?y).", "item 72"),
            ("[n: (?x ex:p ?y) :- (?x ex:q ?y)]", "':-'"),
            (".decl r(a) r(?x) :- (?x ex:p ?y)", "missing '.'"),
            (
                "@hybrid jena .\n.decl r(a) [h: (?x ex:p ?y) -> [(?x ex:q ?y) <- r(?x)]]",
                "relation atom or 'not'",
            ),
        ] {
            let got = refused(text);
            assert!(got.contains(want), "{text}: {got}");
        }
        // a bracket rule with an unbound head variable stays Jena's, which the compiler
        // skips with a warning
        let rs = program("[j: (?x ex:p ?y) -> (?x ex:q ?z)]");
        assert!(!uses_program(&rs));
    }

    /// An included rule set's rules carry its name, and no built-in rule set uses the
    /// new syntax.
    #[test]
    fn includes_are_named() {
        let rs = program("@include <rdfs> .\n.decl r(a)\nr(?x) :- (?x rdf:type ex:C).");
        assert!(
            rs[..rs.len() - 1]
                .iter()
                .all(|r| r.include.as_deref() == Some("rdfs"))
        );
        assert_eq!(rs.last().unwrap().include, None);
        for text in [
            crate::RDFS_RULES,
            crate::RDFS_SIMPLE_RULES,
            crate::OWL_RL_RULES,
        ] {
            assert!(!uses_program(&parse_rules(text).unwrap()));
        }
    }
}
