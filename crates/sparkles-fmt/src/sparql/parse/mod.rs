//! The lossless SPARQL 1.2 parser: recursive descent over the significant tokens, one
//! function per grammar rule, emitting events (`Start(kind)`, `Token`, `Finish`) that
//! [`build`] turns into a [`Tree`], as rust-analyzer's parser does. It does not recover:
//! the first error ends the parse with [`FormatError::Unsupported`] (the reference
//! parser accepted the input, so any error here is the formatter's).
//!
//! After an error the parser answers [`TokenKind::Eof`] to every lookahead, so every
//! loop ends.

pub mod expr;
pub mod path;
pub mod pattern;
pub mod prologue;
pub mod query;
pub mod term;
pub mod triples;
pub mod update;

use super::Unit;
use super::keywords::Kw;
use crate::FormatError;
use crate::lex::{Token, TokenKind};
use crate::syntax::NodeKind;
use crate::tree::{Element, NodeData, NodeId, TokenId, Tree};

/// Parse a whole query or update request.
pub fn parse<'s>(src: &'s str, tokens: Vec<Token>, unit: Unit) -> Result<Tree<'s>, FormatError> {
    let events = {
        let mut p = Parser::new(src, &tokens);
        match unit {
            Unit::Query => query::query_unit(&mut p),
            Unit::Update => update::update_unit(&mut p),
        }
        if !p.at(TokenKind::Eof) {
            p.error("expected the end of the input");
        }
        p.finish()?
    };
    Ok(build(src, tokens, events))
}

#[derive(Clone, Debug)]
pub enum Event {
    /// a node starts; `forward_parent` is the distance to the `Start` of a node that
    /// [`Completed::precede`] opened around this one
    Start {
        kind: NodeKind,
        forward_parent: Option<u32>,
    },
    /// the next significant token, with its (possibly re-kinded) kind
    Token {
        kind: TokenKind,
    },
    Finish,
    /// an abandoned or forwarded `Start`
    Tombstone,
}

pub struct Parser<'t> {
    src: &'t str,
    tokens: &'t [Token],
    /// indexes of the significant tokens (the final `Eof` included)
    sig: Vec<u32>,
    pos: usize,
    events: Vec<Event>,
    /// the first error and the byte offset it is at
    error: Option<(String, usize)>,
}

impl<'t> Parser<'t> {
    pub fn new(src: &'t str, tokens: &'t [Token]) -> Parser<'t> {
        let sig = tokens
            .iter()
            .enumerate()
            .filter(|(_, t)| !t.kind.is_trivia())
            .map(|(i, _)| i as u32)
            .collect();
        Parser {
            src,
            tokens,
            sig,
            pos: 0,
            events: Vec::new(),
            error: None,
        }
    }

    fn nth_token(&self, n: usize) -> Option<&'t Token> {
        if self.error.is_some() {
            return None;
        }
        let i = *self.sig.get(self.pos + n)?;
        Some(&self.tokens[i as usize])
    }

    /// The kind of the `n`th significant token ahead (`Eof` past the end or after an
    /// error).
    pub fn nth(&self, n: usize) -> TokenKind {
        self.nth_token(n).map_or(TokenKind::Eof, |t| t.kind)
    }

    /// The text of the `n`th significant token ahead.
    pub fn nth_text(&self, n: usize) -> &'t str {
        self.nth_token(n).map_or("", |t| t.text(self.src))
    }

    pub fn current(&self) -> TokenKind {
        self.nth(0)
    }

    pub fn at(&self, kind: TokenKind) -> bool {
        self.nth(0) == kind
    }

    /// Whether the `n`th token ahead is a word spelling `kw`.
    pub fn nth_at_kw(&self, n: usize, kw: Kw) -> bool {
        self.nth(n) == TokenKind::Word && Kw::from_word(self.nth_text(n)) == Some(kw)
    }

    pub fn at_kw(&self, kw: Kw) -> bool {
        self.nth_at_kw(0, kw)
    }

    /// The keyword the current token spells, if it is a word.
    pub fn current_kw(&self) -> Option<Kw> {
        match self.current() {
            TokenKind::Word => Kw::from_word(self.nth_text(0)),
            _ => None,
        }
    }

    /// Consume the current token (nothing at the end).
    pub fn bump(&mut self) {
        let kind = self.current();
        self.bump_as(kind);
    }

    /// Consume the current token, re-kinded (a keyword's word as [`TokenKind::Kw`]).
    pub fn bump_as(&mut self, kind: TokenKind) {
        if self.current() == TokenKind::Eof {
            return;
        }
        self.events.push(Event::Token { kind });
        self.pos += 1;
    }

    /// Consume the current token if it is `kind`.
    pub fn eat(&mut self, kind: TokenKind) -> bool {
        let at = self.at(kind);
        if at {
            self.bump();
        }
        at
    }

    /// Consume the current token as `kw` if it spells it.
    pub fn eat_kw(&mut self, kw: Kw) -> bool {
        let at = self.at_kw(kw);
        if at {
            self.bump_as(TokenKind::Kw(kw));
        }
        at
    }

    /// Consume a `kind` token, or fail.
    pub fn expect(&mut self, kind: TokenKind) -> bool {
        self.eat(kind) || {
            self.error(format!("expected {kind:?}"));
            false
        }
    }

    /// Consume the keyword `kw`, or fail.
    pub fn expect_kw(&mut self, kw: Kw) -> bool {
        self.eat_kw(kw) || {
            self.error(format!("expected {}", kw.canonical()));
            false
        }
    }

    /// Record an error at the current token (only the first one counts).
    pub fn error(&mut self, msg: impl Into<String>) {
        if self.error.is_none() {
            let at = self
                .sig
                .get(self.pos)
                .map_or(self.src.len(), |&i| self.tokens[i as usize].start as usize);
            self.error = Some((msg.into(), at));
        }
    }

    pub fn has_error(&self) -> bool {
        self.error.is_some()
    }

    pub fn start(&mut self, kind: NodeKind) -> Marker {
        self.events.push(Event::Start {
            kind,
            forward_parent: None,
        });
        Marker {
            pos: self.events.len() as u32 - 1,
            kind,
        }
    }

    /// Consume one token, and when it opens a bracket everything up to the matching
    /// closing one.
    pub fn bump_balanced(&mut self) {
        let mut depth = 0usize;
        loop {
            let k = self.current();
            if k == TokenKind::Eof {
                return;
            }
            if is_opener(k) {
                depth += 1;
            } else if is_closer(k) {
                depth = depth.saturating_sub(1);
            }
            self.bump();
            if depth == 0 {
                return;
            }
        }
    }

    fn finish(self) -> Result<Vec<Event>, FormatError> {
        match self.error {
            None => Ok(self.events),
            Some((message, at)) => {
                let (line, column) = crate::line_col(self.src, at);
                Err(FormatError::Unsupported {
                    message,
                    line,
                    column,
                })
            }
        }
    }
}

/// Tokens that open a bracket.
pub fn is_opener(k: TokenKind) -> bool {
    use TokenKind::*;
    matches!(
        k,
        LBrace | LParen | LBracket | LBracePipe | LtLt | LtLtParen
    )
}

/// Tokens that close a bracket.
pub fn is_closer(k: TokenKind) -> bool {
    use TokenKind::*;
    matches!(
        k,
        RBrace | RParen | RBracket | PipeRBrace | GtGt | ParenGtGt
    )
}

/// An open node.
#[must_use]
pub struct Marker {
    pos: u32,
    kind: NodeKind,
}

impl Marker {
    pub fn complete(self, p: &mut Parser<'_>) -> Completed {
        p.events.push(Event::Finish);
        Completed {
            pos: self.pos,
            kind: self.kind,
        }
    }

    /// Complete the node as another kind than it was started with.
    pub fn complete_as(self, p: &mut Parser<'_>, kind: NodeKind) -> Completed {
        if let Event::Start { kind: k, .. } = &mut p.events[self.pos as usize] {
            *k = kind;
        }
        Marker {
            pos: self.pos,
            kind,
        }
        .complete(p)
    }

    /// Drop the node (its children become its parent's).
    pub fn abandon(self, p: &mut Parser<'_>) {
        if self.pos as usize == p.events.len() - 1 {
            p.events.pop();
        } else {
            p.events[self.pos as usize] = Event::Tombstone;
        }
    }
}

/// A finished node.
#[derive(Clone, Copy, Debug)]
pub struct Completed {
    pos: u32,
    kind: NodeKind,
}

impl Completed {
    pub fn kind(&self) -> NodeKind {
        self.kind
    }

    /// Start a node that will contain this one (`?a` then `+ ?b`: a `Binary` around it).
    pub fn precede(self, p: &mut Parser<'_>, kind: NodeKind) -> Marker {
        let m = p.start(kind);
        if let Event::Start { forward_parent, .. } = &mut p.events[self.pos as usize] {
            *forward_parent = Some(m.pos - self.pos);
        }
        m
    }
}

/// Turn the events into a tree: re-kind the tokens, link the nodes, compute the ranges.
pub fn build<'s>(src: &'s str, mut tokens: Vec<Token>, mut events: Vec<Event>) -> Tree<'s> {
    let sig: Vec<u32> = tokens
        .iter()
        .enumerate()
        .filter(|(_, t)| !t.kind.is_trivia())
        .map(|(i, _)| i as u32)
        .collect();
    let mut nodes: Vec<NodeData> = Vec::new();
    let mut stack: Vec<NodeId> = Vec::new();
    let mut next = 0usize;
    let mut kinds = Vec::new();
    for i in 0..events.len() {
        match std::mem::replace(&mut events[i], Event::Tombstone) {
            Event::Start {
                kind,
                forward_parent,
            } => {
                // the node and the nodes `precede` opened around it, outermost first
                kinds.clear();
                kinds.push(kind);
                let mut at = i;
                let mut fp = forward_parent;
                while let Some(d) = fp {
                    at += d as usize;
                    fp = match std::mem::replace(&mut events[at], Event::Tombstone) {
                        Event::Start {
                            kind,
                            forward_parent,
                        } => {
                            kinds.push(kind);
                            forward_parent
                        }
                        _ => None,
                    };
                }
                for &kind in kinds.iter().rev() {
                    let id = NodeId(nodes.len() as u32);
                    let parent = stack.last().copied();
                    nodes.push(NodeData {
                        kind,
                        parent,
                        children: Vec::new(),
                        range: 0..0,
                    });
                    if let Some(p) = parent {
                        nodes[p.0 as usize].children.push(Element::Node(id));
                    }
                    stack.push(id);
                }
            }
            Event::Token { kind } => {
                let t = sig[next];
                next += 1;
                tokens[t as usize].kind = kind;
                if let Some(&n) = stack.last() {
                    nodes[n.0 as usize]
                        .children
                        .push(Element::Token(TokenId(t)));
                }
            }
            Event::Finish => {
                let n = stack.pop().expect("a Finish closes a Start");
                let range = {
                    let child_range = |e: &Element| match *e {
                        Element::Node(c) => nodes[c.0 as usize].range.clone(),
                        Element::Token(t) => {
                            let t = tokens[t.0 as usize];
                            t.start..t.start + t.len
                        }
                    };
                    let children = &nodes[n.0 as usize].children;
                    match (children.first(), children.last()) {
                        (Some(a), Some(b)) => child_range(a).start..child_range(b).end,
                        _ => {
                            let at = sig
                                .get(next)
                                .map_or(src.len() as u32, |&t| tokens[t as usize].start);
                            at..at
                        }
                    }
                };
                nodes[n.0 as usize].range = range;
            }
            Event::Tombstone => {}
        }
    }
    // the root spans the whole input after a BOM, so its trivia is inside it
    if let Some(root) = nodes.first_mut() {
        let bom = if src.starts_with('\u{feff}') { 3 } else { 0 };
        root.range = bom..src.len() as u32;
    }
    Tree { src, tokens, nodes }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lex::{LexMode, lex};

    #[test]
    fn events_build_a_tree() {
        let src = "  ?a + ?b # c\n";
        let tokens = lex(src, LexMode::Sparql);
        let mut p = Parser::new(src, &tokens);
        let root = p.start(NodeKind::QueryUnit);
        let lhs = p.start(NodeKind::Opaque);
        p.bump();
        let lhs = lhs.complete(&mut p);
        let bin = lhs.precede(&mut p, NodeKind::Binary);
        p.bump();
        let rhs = p.start(NodeKind::Opaque);
        p.bump();
        rhs.complete(&mut p);
        bin.complete(&mut p);
        root.complete(&mut p);
        assert!(p.at(TokenKind::Eof));
        let events = p.finish().unwrap();
        let t = build(src, tokens, events);
        assert_eq!(
            t.dump(),
            "QueryUnit\n  Binary\n    Opaque\n      Var1 \"?a\"\n    Plus \"+\"\n    Opaque\n      Var1 \"?b\"\n"
        );
        assert_eq!(t.range(t.root()), 0..src.len());
        let bin = t.child_nodes(t.root()).next().unwrap();
        assert_eq!(t.text(bin), "?a + ?b");
        assert_eq!(t.parent(bin), Some(t.root()));
        let first = t.first_token(bin).unwrap();
        let last = t.last_token(bin).unwrap();
        assert_eq!(t.token_text(first), "?a");
        assert_eq!(t.token_text(last), "?b");
        assert_eq!(
            t.trivia_between(first, t.next_significant(first).unwrap())
                .len(),
            1
        );
    }

    #[test]
    fn keywords_and_errors() {
        let src = "select ?x";
        let tokens = lex(src, LexMode::Sparql);
        let mut p = Parser::new(src, &tokens);
        let root = p.start(NodeKind::QueryUnit);
        assert!(p.at_kw(Kw::Select));
        assert!(p.expect_kw(Kw::Select));
        assert!(!p.expect_kw(Kw::Where));
        // after an error everything looks like the end
        assert!(p.at(TokenKind::Eof));
        root.complete(&mut p);
        let e = p.finish().unwrap_err();
        assert_eq!(
            e,
            FormatError::Unsupported {
                message: "expected WHERE".into(),
                line: 1,
                column: 8
            }
        );

        let tokens = lex(src, LexMode::Sparql);
        let mut p = Parser::new(src, &tokens);
        let root = p.start(NodeKind::QueryUnit);
        p.eat_kw(Kw::Select);
        p.bump();
        root.complete(&mut p);
        let events = p.finish().unwrap();
        let t = build(src, tokens, events);
        assert_eq!(t.token_kind(TokenId(0)), TokenKind::Kw(Kw::Select));
    }

    #[test]
    fn balanced_and_abandoned() {
        let src = "{ ( ?a ) [ ] } ?b";
        let tokens = lex(src, LexMode::Sparql);
        let mut p = Parser::new(src, &tokens);
        let root = p.start(NodeKind::QueryUnit);
        let gone = p.start(NodeKind::Opaque);
        gone.abandon(&mut p);
        let g = p.start(NodeKind::GroupGraphPattern);
        p.bump_balanced();
        g.complete(&mut p);
        assert_eq!(p.nth_text(0), "?b");
        p.bump();
        root.complete(&mut p);
        let events = p.finish().unwrap();
        let t = build(src, tokens, events);
        let g = t.child_nodes(t.root()).next().unwrap();
        assert_eq!(t.kind(g), NodeKind::GroupGraphPattern);
        assert_eq!(t.text(g), "{ ( ?a ) [ ] }");
    }
}
