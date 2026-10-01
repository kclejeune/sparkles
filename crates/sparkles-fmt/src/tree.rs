//! The concrete syntax tree: an arena of nodes over the lexer's tokens. A node's children
//! are nodes and significant tokens in source order; the trivia (whitespace, comments)
//! between two tokens is found by adjacency in [`Tree::tokens`].

use crate::lex::{Token, TokenKind};
use crate::syntax::NodeKind;
use std::ops::Range;

/// An index into [`Tree::tokens`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TokenId(pub u32);

/// A node of a [`Tree`]; the root is `NodeId(0)`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct NodeId(pub u32);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Element {
    Node(NodeId),
    Token(TokenId),
}

#[derive(Clone, Debug)]
pub(crate) struct NodeData {
    pub(crate) kind: NodeKind,
    pub(crate) parent: Option<NodeId>,
    pub(crate) children: Vec<Element>,
    pub(crate) range: Range<u32>,
}

/// A syntax tree over `src`. Tokens are lossless (see [`crate::lex`]); the parser has
/// re-kinded keywords to [`TokenKind::Kw`].
#[derive(Clone, Debug)]
pub struct Tree<'s> {
    pub src: &'s str,
    pub tokens: Vec<Token>,
    pub(crate) nodes: Vec<NodeData>,
}

impl<'s> Tree<'s> {
    pub fn root(&self) -> NodeId {
        NodeId(0)
    }

    pub fn kind(&self, n: NodeId) -> NodeKind {
        self.nodes[n.0 as usize].kind
    }

    pub fn children(&self, n: NodeId) -> &[Element] {
        &self.nodes[n.0 as usize].children
    }

    /// The child nodes of `n`, without its tokens.
    pub fn child_nodes(&self, n: NodeId) -> impl Iterator<Item = NodeId> + '_ {
        self.children(n).iter().filter_map(|e| match e {
            Element::Node(c) => Some(*c),
            Element::Token(_) => None,
        })
    }

    pub fn parent(&self, n: NodeId) -> Option<NodeId> {
        self.nodes[n.0 as usize].parent
    }

    /// From the start of the node's first token to the end of its last; the root spans
    /// the whole input after a BOM, trivia included. A node without tokens is empty, at
    /// the start of the next token.
    pub fn range(&self, n: NodeId) -> Range<usize> {
        let r = &self.nodes[n.0 as usize].range;
        r.start as usize..r.end as usize
    }

    /// The source text of [`Tree::range`].
    pub fn text(&self, n: NodeId) -> &'s str {
        &self.src[self.range(n)]
    }

    /// The first significant token in the node's subtree.
    pub fn first_token(&self, n: NodeId) -> Option<TokenId> {
        self.children(n).iter().find_map(|e| match *e {
            Element::Token(t) => Some(t),
            Element::Node(c) => self.first_token(c),
        })
    }

    /// The last significant token in the node's subtree.
    pub fn last_token(&self, n: NodeId) -> Option<TokenId> {
        self.children(n).iter().rev().find_map(|e| match *e {
            Element::Token(t) => Some(t),
            Element::Node(c) => self.last_token(c),
        })
    }

    /// The tokens strictly between `a` and `b`: the trivia between two adjacent
    /// significant tokens.
    pub fn trivia_between(&self, a: TokenId, b: TokenId) -> &[Token] {
        let (a, b) = (a.0 as usize, b.0 as usize);
        if b <= a + 1 {
            return &[];
        }
        &self.tokens[a + 1..b]
    }

    pub fn token(&self, t: TokenId) -> Token {
        self.tokens[t.0 as usize]
    }

    pub fn token_kind(&self, t: TokenId) -> TokenKind {
        self.tokens[t.0 as usize].kind
    }

    pub fn token_text(&self, t: TokenId) -> &'s str {
        self.tokens[t.0 as usize].text(self.src)
    }

    /// The next significant token after `t` (the [`TokenKind::Eof`] at the end).
    pub fn next_significant(&self, t: TokenId) -> Option<TokenId> {
        (t.0 as usize + 1..self.tokens.len())
            .find(|&i| !self.tokens[i].kind.is_trivia())
            .map(|i| TokenId(i as u32))
    }

    /// The significant token before `t`.
    pub fn prev_significant(&self, t: TokenId) -> Option<TokenId> {
        (0..t.0 as usize)
            .rev()
            .find(|&i| !self.tokens[i].kind.is_trivia())
            .map(|i| TokenId(i as u32))
    }

    /// The number of nodes.
    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    /// An indented outline of the tree for snapshot tests: one node or token per line,
    /// tokens with their kind and text.
    pub fn dump(&self) -> String {
        let mut out = String::new();
        self.dump_node(self.root(), 0, &mut out);
        out
    }

    fn dump_node(&self, n: NodeId, depth: usize, out: &mut String) {
        use std::fmt::Write;
        let _ = writeln!(out, "{:indent$}{:?}", "", self.kind(n), indent = depth * 2);
        for e in self.children(n) {
            match *e {
                Element::Node(c) => self.dump_node(c, depth + 1, out),
                Element::Token(t) => {
                    let _ = writeln!(
                        out,
                        "{:indent$}{:?} {:?}",
                        "",
                        self.token_kind(t),
                        self.token_text(t),
                        indent = (depth + 1) * 2
                    );
                }
            }
        }
    }
}
