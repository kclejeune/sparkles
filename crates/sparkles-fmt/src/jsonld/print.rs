//! JSON-LD printing: Prettier's JSON layout (`"key": value`, `{ "a": 1 }`, no trailing
//! commas; an object with two or more members expanded, one-member objects and arrays of
//! scalars inline when they fit, arrays holding an object or an array one element per
//! line), and the key order: keywords first in their fixed order (node and value
//! objects, contexts, expanded term definitions), unknown `@` keys after them in source
//! order, then the terms in source order (by codepoint with `sort`), `@graph` last.
//!
//! Which order applies is decided by position ([`Table`]): the document's objects are
//! node objects; a map under `@context` is a context; a map value of a context's entry is
//! an expanded term definition, whose `@context` is a context again; the value of
//! `@value` is data (a JSON literal), whose members keep their order. Arrays pass their
//! position on to their elements and are never reordered. Strings and numbers print as
//! written.

use crate::doc::{DocArena, DocId, Printed};
use crate::lex::TokenKind;
use crate::syntax::NodeKind;
use crate::tree::{Element, NodeId, TokenId, Tree};
use crate::trivia::{CommentRules, Comments};
use crate::{FormatError, Options};
use std::borrow::Cow;

/// Print a JSON-LD tree.
pub fn print(tree: &Tree<'_>, comments: &Comments, opts: &Options) -> Result<Printed, FormatError> {
    let _ = comments;
    let mut cx = Cx {
        tree,
        arena: DocArena::new(&tree.tokens),
        sort: opts.sort,
    };
    let mut docs = Vec::new();
    if let Some(v) = tree.child_nodes(tree.root()).next() {
        docs.push(cx.value(v, Table::Node));
        docs.push(cx.arena.hard_line());
    }
    let root = cx.arena.concat(docs);
    crate::doc::print(
        &cx.arena,
        root,
        tree.src,
        opts.line_width,
        opts.indent_width,
        opts.deadline,
    )
}

/// Where an object is, which decides its key order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Table {
    /// a node object or value object (the document's objects, `@graph`, property values)
    Node,
    /// a context: a map under `@context`
    Context,
    /// an expanded term definition: a map value of a context's entry
    TermDefinition,
    /// data under `@value`: members as written
    Literal,
}

/// The keywords of node and value objects, in their order (`@graph` comes last, after
/// the terms).
const NODE_KEYWORDS: &[&str] = &[
    "@context",
    "@id",
    "@type",
    "@value",
    "@language",
    "@direction",
    "@index",
    "@reverse",
    "@included",
    "@nest",
    "@list",
    "@set",
];

/// The keywords of a context, in their order.
const CONTEXT_KEYWORDS: &[&str] = &[
    "@version",
    "@import",
    "@base",
    "@vocab",
    "@language",
    "@direction",
    "@propagate",
    "@protected",
];

/// The keywords of an expanded term definition, in their order.
const TERM_DEFINITION_KEYWORDS: &[&str] = &[
    "@id",
    "@reverse",
    "@type",
    "@language",
    "@direction",
    "@container",
    "@context",
    "@index",
    "@nest",
    "@prefix",
    "@propagate",
    "@protected",
];

impl Table {
    /// The table of a member's value: `key` is the member's (unescaped) key.
    pub fn child(self, key: &str) -> Table {
        match (self, key) {
            (Table::Literal, _) => Table::Literal,
            (_, "@context") => Table::Context,
            (Table::Node, "@value") => Table::Literal,
            (Table::Node, _) => Table::Node,
            (Table::Context, _) => Table::TermDefinition,
            (Table::TermDefinition, _) => Table::Literal,
        }
    }

    /// Where `key` goes: `(group, position)`, compared before the source order. Known
    /// keywords (group 0) by their place in the table, unknown `@` keys (1) and terms (2)
    /// in source order (terms by codepoint with `sort`), `@graph` of a node object last
    /// (3). A literal keeps every member in place.
    pub fn rank(self, key: &str) -> (u8, usize) {
        let keywords = match self {
            Table::Literal => return (0, 0),
            Table::Node if key == "@graph" => return (3, 0),
            Table::Node => NODE_KEYWORDS,
            Table::Context => CONTEXT_KEYWORDS,
            Table::TermDefinition => TERM_DEFINITION_KEYWORDS,
        };
        match keywords.iter().position(|k| *k == key) {
            Some(i) => (0, i),
            None if key.starts_with('@') => (1, 0),
            None => (2, 0),
        }
    }
}

/// The value of a JSON string token: without its quotes, escapes decoded (an invalid
/// escape, which the reference parser has rejected, stays as written).
pub fn unescape(token: &str) -> Cow<'_, str> {
    let inner = token
        .strip_prefix('"')
        .and_then(|s| s.strip_suffix('"'))
        .unwrap_or(token);
    if !inner.contains('\\') {
        return Cow::Borrowed(inner);
    }
    let mut out = String::with_capacity(inner.len());
    let mut chars = inner.chars();
    let hex = |s: &str| u32::from_str_radix(s, 16).ok();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        let rest = chars.as_str();
        let escaped = match rest.chars().next() {
            Some('"') => '"',
            Some('\\') => '\\',
            Some('/') => '/',
            Some('b') => '\u{8}',
            Some('f') => '\u{c}',
            Some('n') => '\n',
            Some('r') => '\r',
            Some('t') => '\t',
            Some('u') => {
                let unit = rest.get(1..5).and_then(hex);
                let low = rest
                    .get(5..7)
                    .filter(|s| *s == "\\u")
                    .and_then(|_| rest.get(7..11))
                    .and_then(hex);
                match (unit, low) {
                    (Some(h @ 0xD800..=0xDBFF), Some(l @ 0xDC00..=0xDFFF)) => {
                        let c = 0x10000 + ((h - 0xD800) << 10) + (l - 0xDC00);
                        out.push(char::from_u32(c).unwrap_or('\u{fffd}'));
                        chars = rest[11..].chars();
                        continue;
                    }
                    (Some(u), _) => {
                        out.push(char::from_u32(u).unwrap_or('\u{fffd}'));
                        chars = rest[5..].chars();
                        continue;
                    }
                    (None, _) => {
                        out.push('\\');
                        continue;
                    }
                }
            }
            _ => {
                out.push('\\');
                continue;
            }
        };
        out.push(escaped);
        chars.next();
    }
    Cow::Owned(out)
}

struct Cx<'a, 's> {
    tree: &'a Tree<'s>,
    arena: DocArena<'a>,
    sort: bool,
}

impl<'s> Cx<'_, 's> {
    fn token(&mut self, t: TokenId) -> DocId {
        self.arena.token(t, None)
    }

    /// The first and last tokens of a container (its brackets).
    fn brackets(&self, n: NodeId) -> (TokenId, TokenId) {
        let toks = self.tree.children(n).iter().filter_map(|e| match e {
            Element::Token(t) => Some(*t),
            Element::Node(_) => None,
        });
        let mut toks = toks.filter(|&t| {
            matches!(
                self.tree.token_kind(t),
                TokenKind::LBrace | TokenKind::RBrace | TokenKind::LBracket | TokenKind::RBracket
            )
        });
        let open = toks.next().expect("a container opens");
        let close = toks.next().expect("a container closes");
        (open, close)
    }

    fn value(&mut self, n: NodeId, table: Table) -> DocId {
        match self.tree.kind(n) {
            NodeKind::JsonObject => self.object(n, table),
            NodeKind::JsonArray => self.array(n, table),
            _ => {
                let t = self.tree.first_token(n).expect("a scalar is a token");
                self.token(t)
            }
        }
    }

    /// An object: `{}`, `{ "k": v }` when one member fits, else one member per line.
    fn object(&mut self, n: NodeId, table: Table) -> DocId {
        let (open, close) = self.brackets(n);
        let mut members: Vec<(NodeId, TokenId, Cow<'s, str>)> = self
            .tree
            .child_nodes(n)
            .map(|m| {
                let key = self.tree.first_token(m).expect("a member has a key");
                (m, key, unescape(self.tree.token_text(key)))
            })
            .collect();
        let sort = self.sort;
        // a stable sort: equal ranks keep the source order
        members.sort_by(|a, b| {
            let (ra, rb) = (table.rank(&a.2), table.rank(&b.2));
            let by_name = sort && ra.0 == 2 && rb.0 == 2;
            ra.cmp(&rb).then_with(|| {
                if by_name {
                    a.2.cmp(&b.2)
                } else {
                    std::cmp::Ordering::Equal
                }
            })
        });
        let open = self.token(open);
        let close = self.token(close);
        if members.is_empty() {
            return self.arena.concat([open, close]);
        }
        let mut body = Vec::new();
        let count = members.len();
        for (i, (m, key, name)) in members.into_iter().enumerate() {
            body.push(match count {
                1 => self.arena.line(),
                _ => self.arena.hard_line(),
            });
            body.push(self.member(m, key, table.child(&name)));
            if i + 1 < count {
                body.push(self.arena.text(","));
            }
        }
        let body = self.arena.concat(body);
        let body = self.arena.indent(body);
        let end = match count {
            1 => self.arena.line(),
            _ => self.arena.hard_line(),
        };
        let doc = self.arena.concat([open, body, end, close]);
        self.arena.group(doc).0
    }

    /// `"key": value`
    fn member(&mut self, m: NodeId, key: TokenId, table: Table) -> DocId {
        let colon = self
            .tree
            .children(m)
            .iter()
            .find_map(|e| match *e {
                Element::Token(t) if self.tree.token_kind(t) == TokenKind::Colon => Some(t),
                _ => None,
            })
            .expect("a member has a colon");
        let value = self
            .tree
            .child_nodes(m)
            .next()
            .expect("a member has a value");
        let key = self.token(key);
        let colon = self.token(colon);
        let space = self.arena.text(" ");
        let value = self.value(value, table);
        self.arena.concat([key, colon, space, value])
    }

    /// An array: `[]`, `[a, b]` when its scalars fit, else one element per line (always
    /// when it holds an object or an array).
    fn array(&mut self, n: NodeId, table: Table) -> DocId {
        let (open, close) = self.brackets(n);
        let items: Vec<NodeId> = self.tree.child_nodes(n).collect();
        let open = self.token(open);
        let close = self.token(close);
        if items.is_empty() {
            return self.arena.concat([open, close]);
        }
        let nested = items.iter().any(|&i| {
            matches!(
                self.tree.kind(i),
                NodeKind::JsonObject | NodeKind::JsonArray
            )
        });
        let mut body = Vec::new();
        let count = items.len();
        for (i, item) in items.into_iter().enumerate() {
            body.push(match (nested, i) {
                (true, _) => self.arena.hard_line(),
                (false, 0) => self.arena.soft_line(),
                (false, _) => self.arena.line(),
            });
            body.push(self.value(item, table));
            if i + 1 < count {
                body.push(self.arena.text(","));
            }
        }
        let body = self.arena.concat(body);
        let body = self.arena.indent(body);
        let end = match nested {
            true => self.arena.hard_line(),
            false => self.arena.soft_line(),
        };
        let doc = self.arena.concat([open, body, end, close]);
        self.arena.group(doc).0
    }
}

/// JSON has no comments: nothing attaches.
pub struct JsonRules;

pub static RULES: JsonRules = JsonRules;

impl CommentRules for JsonRules {
    fn is_attachment(&self, kind: NodeKind) -> bool {
        matches!(kind, NodeKind::JsonMember | NodeKind::JsonScalar)
    }

    fn is_container(&self, kind: NodeKind) -> bool {
        matches!(kind, NodeKind::JsonObject | NodeKind::JsonArray)
    }

    fn is_separator(&self, kind: TokenKind) -> bool {
        kind == TokenKind::Comma
    }

    fn is_closer(&self, kind: TokenKind) -> bool {
        matches!(kind, TokenKind::RBrace | TokenKind::RBracket)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unescapes() {
        assert_eq!(unescape("\"plain\""), "plain");
        assert!(matches!(unescape("\"plain\""), Cow::Borrowed(_)));
        assert_eq!(unescape(r#""a\"b\\c\/d\n\t""#), "a\"b\\c/d\n\t");
        assert_eq!(unescape(r#""@id""#), "@id");
        assert_eq!(unescape(r#""😀!""#), "\u{1F600}!");
        assert_eq!(unescape(r#""éé""#), "éé");
    }

    #[test]
    fn ranks() {
        use Table::*;
        assert!(Node.rank("@context") < Node.rank("@id"));
        assert!(Node.rank("@set") < Node.rank("@unknown"));
        assert!(Node.rank("@unknown") < Node.rank("name"));
        assert!(Node.rank("name") < Node.rank("@graph"));
        assert!(Context.rank("@protected") < Context.rank("@type"));
        assert!(Context.rank("@type") < Context.rank("name"));
        assert_eq!(Context.rank("@graph"), (1, 0));
        assert!(TermDefinition.rank("@id") < TermDefinition.rank("@type"));
        assert_eq!(Literal.rank("@id"), Literal.rank("z"));
        assert_eq!(Node.child("@context"), Context);
        assert_eq!(Context.child("knows"), TermDefinition);
        assert_eq!(Context.child("@type"), TermDefinition);
        assert_eq!(TermDefinition.child("@context"), Context);
        assert_eq!(Node.child("@value"), Literal);
        assert_eq!(Literal.child("@context"), Literal);
        assert_eq!(Node.child("@graph"), Node);
    }
}
