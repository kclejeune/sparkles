//! Comment classification and attachment, and blank-line accounting: which node owns
//! each comment (header, leading, trailing, dangling, detached, displaced), and which
//! nodes had a blank line before them.
//!
//! Let `P` be the last significant token before a comment and `N` the first after it.
//! - Comments before the first significant token are the file **header**, except a
//!   block ending with `# sparkles-fmt: ignore` right before the first node: it leads
//!   that node, so a node sorted to the top keeps its pragma.
//! - A comment on `P`'s line **trails** the outermost attachment node that ends at `P`;
//!   a separator (`,` `;` `.` `&&` `||`) counts as part of the item before it, on
//!   either side of the comment. A comment after separators whose item has a trailing
//!   comment already does not trail (both would end one printed line).
//! - The other comments form blocks, split at blank lines. Before a closing bracket (or
//!   the end of the input) they **dangle** in the innermost container that `N` closes.
//!   Otherwise the last block, when no blank line follows it, **leads** the outermost
//!   attachment node starting at `N` (within the innermost node holding both `P` and
//!   `N`), and the blocks before it are **detached**: printed before that node with a
//!   blank line after each.
//! - A comment where no attachment node starts or ends is **displaced**: it leads the
//!   nearest attachment node around it, and printing it there gives a `comment-moved`
//!   warning.
//!
//! A node printed verbatim (`# sparkles-fmt: ignore`, or a printer not written yet)
//! copies the comments inside its range; [`Comments::copied`] records them so that
//! [`wrap`] does not print them a second time.

use crate::Warning;
use crate::doc::{DocArena, DocId};
use crate::lex::TokenKind;
use crate::syntax::NodeKind;
use crate::tree::{Element, NodeId, TokenId, Tree};
use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::ops::Range;

/// What a language says about its tree, for attaching comments.
pub trait CommentRules {
    /// Nodes that own comments (prologue declarations, clauses, group elements, entries,
    /// objects, arguments, chain operands …).
    fn is_attachment(&self, kind: NodeKind) -> bool;
    /// Nodes whose closing bracket takes dangling comments (`{ }`, `[ ]`, `( )` …).
    fn is_container(&self, kind: NodeKind) -> bool;
    /// Tokens that count as part of the item before them (`,` `;` `.`, `&&` `||`).
    fn is_separator(&self, kind: TokenKind) -> bool;
    /// Closing brackets (`}` `]` `)` `|}` `>>` `)>>`).
    fn is_closer(&self, kind: TokenKind) -> bool;
}

/// The comments of a tree, by owner.
#[derive(Clone, Debug, Default)]
pub struct Comments {
    header: Vec<TokenId>,
    leading: HashMap<NodeId, Vec<TokenId>>,
    trailing: HashMap<NodeId, Vec<TokenId>>,
    dangling: HashMap<NodeId, Vec<TokenId>>,
    detached: HashMap<NodeId, Vec<Vec<TokenId>>>,
    /// each node's first significant token
    first: Vec<Option<TokenId>>,
    /// significant tokens with a blank line before them (after the previous token and
    /// its trailing comment, before any comment block of their own)
    blank_tokens: HashSet<TokenId>,
    /// comments with a blank line right before them
    blank_comments: HashSet<TokenId>,
    /// significant tokens a trailing comment follows
    trailed: HashSet<TokenId>,
    ignored: HashSet<NodeId>,
    ignore_file: bool,
    /// displaced comments and the warning printing them gives
    displaced: HashMap<TokenId, Warning>,
    /// displaced comments printed away from where they were
    moved: RefCell<BTreeMap<TokenId, Warning>>,
    /// comments a verbatim range printed already
    copied: RefCell<HashSet<TokenId>>,
}

/// Line breaks in a whitespace run (`\r\n` counts once).
fn line_breaks(s: &str) -> usize {
    let b = s.as_bytes();
    b.iter()
        .enumerate()
        .filter(|&(i, &c)| c == b'\n' || (c == b'\r' && b.get(i + 1) != Some(&b'\n')))
        .count()
}

/// Per-tree facts the attachment needs.
struct Shape<'a, 's> {
    tree: &'a Tree<'s>,
    rules: &'a dyn CommentRules,
    first: Vec<Option<TokenId>>,
    last: Vec<Option<TokenId>>,
    /// the node a significant token is a direct child of
    owner: Vec<Option<NodeId>>,
}

impl Shape<'_, '_> {
    fn new<'a, 's>(tree: &'a Tree<'s>, rules: &'a dyn CommentRules) -> Shape<'a, 's> {
        let n = tree.len();
        let mut first = vec![None; n];
        let mut last = vec![None; n];
        let mut owner = vec![None; tree.tokens.len()];
        // pre-order, then children before parents
        let mut order = Vec::with_capacity(n);
        let mut stack = if n == 0 { vec![] } else { vec![tree.root()] };
        while let Some(x) = stack.pop() {
            order.push(x);
            stack.extend(tree.child_nodes(x));
        }
        for &x in order.iter().rev() {
            let i = x.0 as usize;
            for e in tree.children(x) {
                let (f, l) = match *e {
                    Element::Token(t) => {
                        owner[t.0 as usize] = Some(x);
                        (Some(t), Some(t))
                    }
                    Element::Node(c) => (first[c.0 as usize], last[c.0 as usize]),
                };
                first[i] = first[i].or(f);
                last[i] = l.or(last[i]);
            }
        }
        Shape {
            tree,
            rules,
            first,
            last,
            owner,
        }
    }

    fn first(&self, n: NodeId) -> Option<TokenId> {
        self.first[n.0 as usize]
    }

    fn last(&self, n: NodeId) -> Option<TokenId> {
        self.last[n.0 as usize]
    }

    fn attachment(&self, n: NodeId) -> bool {
        self.rules.is_attachment(self.tree.kind(n))
    }

    /// The outermost attachment node whose last token is `t`.
    fn ending_at(&self, t: TokenId) -> Option<NodeId> {
        let mut best = None;
        let mut at = self.owner[t.0 as usize];
        while let Some(x) = at {
            if self.last(x) != Some(t) {
                break;
            }
            if self.attachment(x) {
                best = Some(x);
            }
            at = self.tree.parent(x);
        }
        best
    }

    /// The node a comment on `p`'s line trails, `n` being the token after the comment.
    /// A separator belongs to the item before it, on either side of the comment
    /// (`?a, # c` and `?a # c` before `, ?b`): before a separator, the comment trails
    /// the outermost node ending with the separator, as it does after it, since the
    /// separator is printed before the comment (the last operand of a nested chain
    /// would otherwise take it inside that chain, and the next time, printed after the
    /// outer operator, it would trail the outer operand).
    fn trailing_owner(&self, p: TokenId, n: TokenId) -> Option<NodeId> {
        let separator = |t: TokenId| self.rules.is_separator(self.tree.token_kind(t));
        let with_separator = match separator(n) {
            true => self
                .ending_at(n)
                .filter(|&x| self.first(x).is_some_and(|f| f <= p)),
            false => None,
        };
        with_separator
            .or_else(|| self.ending_at(p))
            .or_else(|| match separator(p) {
                true => self.ending_at(self.tree.prev_significant(p)?),
                false => None,
            })
    }

    /// The outermost attachment node whose first token is `t`.
    fn starting_at(&self, t: TokenId) -> Option<NodeId> {
        let mut best = None;
        let mut at = self.owner[t.0 as usize];
        while let Some(x) = at {
            if self.first(x) != Some(t) {
                break;
            }
            if self.attachment(x) {
                best = Some(x);
            }
            at = self.tree.parent(x);
        }
        best
    }

    /// The innermost container whose closing bracket is `t`.
    fn closed_by(&self, t: TokenId) -> Option<NodeId> {
        let mut at = self.owner[t.0 as usize];
        while let Some(x) = at {
            if self.last(x) != Some(t) {
                return None;
            }
            if self.rules.is_container(self.tree.kind(x)) {
                return Some(x);
            }
            at = self.tree.parent(x);
        }
        None
    }

    /// The innermost attachment node holding both `p` and `n`; the root when there is
    /// none.
    fn enclosing(&self, p: TokenId, n: TokenId) -> NodeId {
        let root = self.tree.root();
        let mut around_p = HashSet::new();
        let mut at = self.owner[p.0 as usize];
        while let Some(x) = at {
            around_p.insert(x);
            at = self.tree.parent(x);
        }
        let mut at = self.owner[n.0 as usize];
        while let Some(x) = at {
            if around_p.contains(&x) {
                break;
            }
            at = self.tree.parent(x);
        }
        while let Some(x) = at {
            if self.attachment(x) {
                return x;
            }
            at = self.tree.parent(x);
        }
        root
    }
}

impl Comments {
    /// Classify every comment of `tree`.
    pub fn attach(tree: &Tree<'_>, rules: &dyn CommentRules) -> Comments {
        let shape = Shape::new(tree, rules);
        let mut c = Comments::default();
        let mut prev: Option<TokenId> = None;
        let mut run: Vec<TokenId> = Vec::new();
        for (i, t) in tree.tokens.iter().enumerate() {
            let id = TokenId(i as u32);
            if t.kind.is_trivia() {
                if t.kind == TokenKind::Comment {
                    run.push(id);
                }
                continue;
            }
            c.run(&shape, prev, &run, id);
            run.clear();
            prev = Some(id);
        }
        c.ignore_file = c
            .header
            .iter()
            .any(|&h| crate::pragma::is_ignore_file(tree.token_text(h)));
        c.first = shape.first;
        c
    }

    /// The comments `comments` between significant tokens `p` (none at the start) and
    /// `n`.
    fn run(&mut self, shape: &Shape<'_, '_>, p: Option<TokenId>, comments: &[TokenId], n: TokenId) {
        let tree = shape.tree;
        // the line breaks in the whitespace right before token `t`
        let breaks_before = |t: TokenId| -> usize {
            match t.0.checked_sub(1).map(|i| tree.token(TokenId(i))) {
                Some(ws) if ws.kind == TokenKind::Whitespace => line_breaks(ws.text(tree.src)),
                _ => 0,
            }
        };
        for (i, &cm) in comments.iter().enumerate() {
            // blank lines at the very start do not count
            if breaks_before(cm) >= 2 && (p.is_some() || i > 0) {
                self.blank_comments.insert(cm);
            }
        }
        let Some(p) = p else {
            // the file header, except that a block ending with an ignore pragma right
            // before the first node is that node's leading block (so a node sorted to
            // the top with its pragma keeps it)
            let mut header = comments;
            if breaks_before(n) < 2
                && let Some(&last) = comments.last()
                && crate::pragma::is_ignore(tree.token_text(last))
                && let Some(node) = shape.starting_at(n)
            {
                let start = comments
                    .iter()
                    .rposition(|&c| breaks_before(c) >= 2)
                    .unwrap_or(0);
                let block = &comments[start..];
                if self.blank_comments.contains(&block[0]) {
                    self.blank_tokens.insert(n);
                }
                self.leading.insert(node, block.to_vec());
                self.ignored.insert(node);
                header = &comments[..start];
            } else if !comments.is_empty() && breaks_before(n) >= 2 {
                self.blank_tokens.insert(n);
            }
            self.header.extend_from_slice(header);
            return;
        };
        let mut rest = comments;
        if let Some((&c0, more)) = rest.split_first()
            && breaks_before(c0) == 0
            && !self.line_taken(shape, p)
            && let Some(owner) = shape.trailing_owner(p, n)
            && !self.trailing.contains_key(&owner)
        {
            self.trailing.insert(owner, vec![c0]);
            self.trailed.insert(p);
            rest = more;
        }
        let blank = match rest.first() {
            Some(&c) => breaks_before(c) >= 2,
            None => breaks_before(n) >= 2,
        };
        if blank {
            self.blank_tokens.insert(n);
        }
        if rest.is_empty() {
            return;
        }
        let n_kind = tree.token_kind(n);
        if n_kind == TokenKind::Eof {
            self.dangling
                .entry(tree.root())
                .or_default()
                .extend_from_slice(rest);
            return;
        }
        if shape.rules.is_closer(n_kind) {
            match shape.closed_by(n) {
                Some(container) => self
                    .dangling
                    .entry(container)
                    .or_default()
                    .extend_from_slice(rest),
                None => self.displace(shape, shape.enclosing(p, n), rest),
            }
            return;
        }
        let Some(node) = shape.starting_at(n) else {
            self.displace(shape, shape.enclosing(p, n), rest);
            return;
        };
        // blocks, split at blank lines; a block with a blank line after it is detached
        let mut blocks: Vec<Vec<TokenId>> = Vec::new();
        for &cm in rest {
            if blocks.is_empty() || breaks_before(cm) >= 2 {
                blocks.push(Vec::new());
            }
            blocks.last_mut().expect("a block").push(cm);
        }
        if breaks_before(n) < 2 {
            let lead = blocks.pop().expect("a block");
            if lead
                .last()
                .is_some_and(|&l| crate::pragma::is_ignore(tree.token_text(l)))
            {
                self.ignored.insert(node);
            }
            self.leading.entry(node).or_default().extend(lead);
        }
        if !blocks.is_empty() {
            self.detached.entry(node).or_default().extend(blocks);
        }
    }

    /// Whether a comment after `p` would print on the line of an earlier trailing
    /// comment: `p` is a separator and a trailing comment follows it or the item before
    /// it, with only separators in between (`?o # c` before `; # d`). A separator is
    /// printed with the item before it, so both comments would end the same line, and
    /// the second would go to a line of its own, where it no longer trails.
    fn line_taken(&self, shape: &Shape<'_, '_>, p: TokenId) -> bool {
        let separator = |t: TokenId| shape.rules.is_separator(shape.tree.token_kind(t));
        let mut at = p;
        while separator(at) {
            let Some(prev) = shape.tree.prev_significant(at) else {
                return false;
            };
            if self.trailed.contains(&prev) {
                return true;
            }
            at = prev;
        }
        false
    }

    /// Comments where no attachment node starts or ends lead `node`, the nearest one
    /// around them.
    fn displace(&mut self, shape: &Shape<'_, '_>, node: NodeId, comments: &[TokenId]) {
        let tree = shape.tree;
        for &cm in comments {
            let (line, column) = crate::line_col(tree.src, tree.token(cm).start as usize);
            self.displaced.insert(
                cm,
                Warning {
                    code: "comment-moved",
                    message: format!(
                        "comment at {line}:{column} moved before the enclosing {}: it sat \
                         where no element starts or ends",
                        describe(tree.kind(node))
                    ),
                    line,
                    column,
                },
            );
        }
        self.leading
            .entry(node)
            .or_default()
            .extend_from_slice(comments);
    }

    /// The comments before the first significant token, in order.
    pub fn header(&self) -> &[TokenId] {
        &self.header
    }

    /// The comment block directly before `n` (no blank line between), then any
    /// comments displaced into `n`.
    pub fn leading(&self, n: NodeId) -> &[TokenId] {
        self.leading.get(&n).map_or(&[], Vec::as_slice)
    }

    /// The comment on the line where `n` ends.
    pub fn trailing(&self, n: NodeId) -> &[TokenId] {
        self.trailing.get(&n).map_or(&[], Vec::as_slice)
    }

    /// Comments before the closing bracket of container `n` (for the root: at the end
    /// of the input).
    pub fn dangling(&self, n: NodeId) -> &[TokenId] {
        self.dangling.get(&n).map_or(&[], Vec::as_slice)
    }

    /// Comment blocks with blank lines around them, printed as siblings before `n`.
    pub fn detached_before(&self, n: NodeId) -> &[Vec<TokenId>] {
        self.detached.get(&n).map_or(&[], Vec::as_slice)
    }

    /// Whether the source had a blank line before `n`: after the previous token and its
    /// trailing comment, before `n`'s detached and leading comments. Any node starting
    /// at the same token answers the same. After the file header: whether a blank line
    /// separates the header from `n`.
    pub fn blank_before(&self, n: NodeId) -> bool {
        self.first
            .get(n.0 as usize)
            .copied()
            .flatten()
            .is_some_and(|t| self.blank_tokens.contains(&t))
    }

    /// [`Comments::blank_before`] for a significant token.
    pub fn blank_before_token(&self, t: TokenId) -> bool {
        self.blank_tokens.contains(&t)
    }

    /// Whether the source had a blank line right before comment `c` (never for the
    /// file's first token).
    pub fn blank_before_comment(&self, c: TokenId) -> bool {
        self.blank_comments.contains(&c)
    }

    /// Whether `n`'s leading block ends with `# sparkles-fmt: ignore`.
    pub fn ignored(&self, n: NodeId) -> bool {
        self.ignored.contains(&n)
    }

    /// Whether the header holds `# sparkles-fmt: ignore-file`.
    pub fn ignore_file(&self) -> bool {
        self.ignore_file
    }

    /// `comment-moved` warnings of the displaced comments that were printed away from
    /// where they were, in source order.
    pub fn warnings(&self) -> Vec<Warning> {
        self.moved.borrow().values().cloned().collect()
    }

    /// Whether comment `c` was printed inside a verbatim range already.
    pub fn copied(&self, c: TokenId) -> bool {
        self.copied.borrow().contains(&c)
    }

    /// Record that a verbatim copy of `range` printed the comments inside it.
    pub fn mark_copied(&self, tree: &Tree<'_>, range: Range<usize>) {
        let first = tree
            .tokens
            .partition_point(|t| (t.start as usize) < range.start);
        let mut copied = self.copied.borrow_mut();
        for (i, t) in tree.tokens.iter().enumerate().skip(first) {
            if t.end() > range.end {
                break;
            }
            if t.kind == TokenKind::Comment {
                copied.insert(TokenId(i as u32));
            }
        }
    }

    /// Record that comment `c` is printed (by a verbatim node's surroundings).
    pub fn mark_printed(&self, c: TokenId) {
        self.copied.borrow_mut().insert(c);
    }

    /// Note that displaced comment `c` is printed away from where it was.
    fn note_moved(&self, c: TokenId) {
        if let Some(w) = self.displaced.get(&c) {
            self.moved.borrow_mut().insert(c, w.clone());
        }
    }

    /// Every comment attached to `n` itself: detached, leading, trailing, dangling.
    pub fn own(&self, n: NodeId) -> impl Iterator<Item = TokenId> + '_ {
        self.detached_before(n)
            .iter()
            .flatten()
            .chain(self.leading(n))
            .chain(self.trailing(n))
            .chain(self.dangling(n))
            .copied()
    }

    /// Whether `n` has comments of its own that are still to print.
    pub fn has_comments(&self, n: NodeId) -> bool {
        self.own(n).any(|c| !self.copied(c))
    }
}

fn describe(kind: NodeKind) -> String {
    let name = format!("{kind:?}");
    let mut out = String::new();
    for (i, ch) in name.chars().enumerate() {
        if ch.is_uppercase() && i > 0 {
            out.push(' ');
        }
        out.push(ch.to_ascii_lowercase());
    }
    out
}

/// A comment as a document: its text without trailing whitespace. Displaced comments
/// count as moved.
pub fn comment(arena: &mut DocArena<'_>, comments: &Comments, c: TokenId) -> DocId {
    comments.note_moved(c);
    arena.token(c, None)
}

/// Comment lines: each comment on its own line, a blank line kept (collapsed to one)
/// where the source had one between two of them. The document starts with the first
/// comment and ends after the last one, without a line break.
pub fn comment_lines(arena: &mut DocArena<'_>, comments: &Comments, cs: &[TokenId]) -> DocId {
    let mut parts = Vec::with_capacity(cs.len() * 2);
    for (i, &c) in cs.iter().enumerate() {
        if i > 0 {
            parts.push(match comments.blank_before_comment(c) {
                true => arena.empty_line(),
                false => arena.hard_line(),
            });
        }
        parts.push(comment(arena, comments, c));
    }
    arena.concat(parts)
}

/// A trailing comment: `LineSuffix(" " + c)` with a `BreakParent`, since a line comment
/// ends its line.
pub fn trailing_comment(arena: &mut DocArena<'_>, comments: &Comments, c: TokenId) -> DocId {
    let sp = arena.text(" ");
    let cm = comment(arena, comments, c);
    let both = arena.concat([sp, cm]);
    let suffix = arena.line_suffix(both);
    let bp = arena.break_parent();
    arena.concat([suffix, bp])
}

/// `doc` with `node`'s detached, leading and trailing comments around it: each
/// detached block on its own lines with a blank line after it, the leading block on
/// its own lines, the trailing comment at the end of the line. Comments a verbatim copy
/// printed already are left out. Dangling comments are the container printer's.
pub fn wrap(arena: &mut DocArena<'_>, comments: &Comments, node: NodeId, doc: DocId) -> DocId {
    let live = |c: &&TokenId| !comments.copied(**c);
    let mut parts = Vec::new();
    for block in comments.detached_before(node) {
        let block: Vec<TokenId> = block.iter().filter(live).copied().collect();
        if !block.is_empty() {
            parts.push(comment_lines(arena, comments, &block));
            parts.push(arena.empty_line());
        }
    }
    for &c in comments.leading(node).iter().filter(live) {
        parts.push(comment(arena, comments, c));
        parts.push(arena.hard_line());
    }
    let trailing: Vec<TokenId> = comments
        .trailing(node)
        .iter()
        .filter(live)
        .copied()
        .collect();
    if parts.is_empty() && trailing.is_empty() {
        return doc;
    }
    parts.push(doc);
    for c in trailing {
        parts.push(trailing_comment(arena, comments, c));
    }
    arena.concat(parts)
}

/// Hand-built trees for tests, without the parser.
#[cfg(test)]
pub(crate) mod test_tree {
    use crate::lex::{LexMode, lex};
    use crate::syntax::NodeKind;
    use crate::tree::{Element, NodeData, NodeId, TokenId, Tree};

    /// A tree over `src` from `shape`: `(K child …)` is a node of kind `K` (a letter, see
    /// [`kind`]), `_` the next significant token. The root spans the whole input.
    pub(crate) fn tree<'s>(src: &'s str, shape: &str) -> Tree<'s> {
        let tokens = lex(src, LexMode::Sparql);
        let sig: Vec<u32> = (0..tokens.len() as u32)
            .filter(|&i| !tokens[i as usize].kind.is_trivia())
            .collect();
        let mut nodes: Vec<NodeData> = Vec::new();
        let mut stack: Vec<NodeId> = Vec::new();
        let mut next = 0;
        let spaced = shape.replace('(', " ( ").replace(')', " ) ");
        let mut words = spaced.split_whitespace();
        while let Some(w) = words.next() {
            match w {
                "(" => {
                    let k = kind(words.next().expect("a kind"));
                    let id = NodeId(nodes.len() as u32);
                    let parent = stack.last().copied();
                    nodes.push(NodeData {
                        kind: k,
                        parent,
                        children: Vec::new(),
                        range: 0..0,
                    });
                    if let Some(p) = parent {
                        nodes[p.0 as usize].children.push(Element::Node(id));
                    }
                    stack.push(id);
                }
                ")" => {
                    stack.pop();
                }
                "_" => {
                    let t = TokenId(sig[next]);
                    next += 1;
                    let n = *stack.last().expect("a node");
                    nodes[n.0 as usize].children.push(Element::Token(t));
                }
                other => panic!("shape: {other}"),
            }
        }
        assert_eq!(
            tokens[sig[next] as usize].kind,
            crate::lex::TokenKind::Eof,
            "the shape leaves tokens over"
        );
        let mut t = Tree { src, tokens, nodes };
        for i in (0..t.nodes.len()).rev() {
            let n = NodeId(i as u32);
            let range = match (t.first_token(n), t.last_token(n)) {
                (Some(f), Some(l)) => t.token(f).start..t.token(l).end() as u32,
                _ => 0..0,
            };
            t.nodes[i].range = range;
        }
        t.nodes[0].range = 0..src.len() as u32;
        t
    }

    pub(crate) fn kind(letter: &str) -> NodeKind {
        use NodeKind as K;
        match letter {
            "Q" => K::QueryUnit,
            "P" => K::Prologue,
            "D" => K::PrefixDecl,
            "G" => K::GroupGraphPattern,
            "S" => K::TriplesStmt,
            "E" => K::PropertyListEntry,
            "O" => K::Object,
            "F" => K::Filter,
            "B" => K::Bracketed,
            "U" => K::Union,
            "R" => K::UnionBranch,
            "X" => K::Opaque,
            other => panic!("kind {other}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::test_tree::tree;
    use super::*;
    use crate::sparql::print::RULES;

    /// The comment texts of `ids`.
    fn texts(t: &Tree<'_>, ids: &[TokenId]) -> Vec<String> {
        ids.iter().map(|&c| t.token_text(c).to_string()).collect()
    }

    /// The node of the `i`th `(` in the shape (pre-order).
    fn nth(i: u32) -> NodeId {
        NodeId(i)
    }

    #[test]
    fn header_and_the_blank_line_after_it() {
        let src = "# a\n\n# b\n\n{ ?s ?p ?o }";
        let t = tree(src, "(Q (G _ (S _ (E _ (O _))) _))");
        let c = Comments::attach(&t, &RULES);
        assert_eq!(texts(&t, c.header()), ["# a", "# b"]);
        assert!(c.blank_before(nth(1)));
        assert!(c.blank_before_comment(c.header()[1]));
        assert!(!c.blank_before_comment(c.header()[0]));
        assert!(c.leading(nth(1)).is_empty());

        let t = tree("# a\n{ ?s ?p ?o }", "(Q (G _ (S _ (E _ (O _))) _))");
        let c = Comments::attach(&t, &RULES);
        assert!(!c.blank_before(nth(1)));
        assert!(!c.ignore_file());
        let t = tree("# sparkles-fmt: ignore-file\n{}", "(Q (G _ _))");
        assert!(Comments::attach(&t, &RULES).ignore_file());
    }

    #[test]
    fn trailing_goes_to_the_outermost_node_ending_on_the_line() {
        // `?o .` ends the statement, entry and object: the statement takes it
        let src = "{ ?s ?p ?o . # c\n}";
        let t = tree(src, "(Q (G _ (S _ (E _ (O _)) _) _))");
        let c = Comments::attach(&t, &RULES);
        assert_eq!(texts(&t, c.trailing(nth(2))), ["# c"]);
        assert!(c.dangling(nth(1)).is_empty());

        // after an object's `,`, and before it
        let src = "{ ?s ?p ?a, # c\n ?b # d\n , ?e }";
        let t = tree(src, "(Q (G _ (S _ (E _ (O _ _) (O _ _) (O _))) _))");
        let c = Comments::attach(&t, &RULES);
        assert_eq!(texts(&t, c.trailing(nth(4))), ["# c"]);
        assert_eq!(texts(&t, c.trailing(nth(5))), ["# d"]);

        // a second comment for the same node is not a trailing one: it leads the next
        let src = "{ ?s ?p ?a # c\n , # d\n ?b }";
        let t = tree(src, "(Q (G _ (S _ (E _ (O _ _) (O _))) _))");
        let c = Comments::attach(&t, &RULES);
        assert_eq!(texts(&t, c.trailing(nth(4))), ["# c"]);
        assert_eq!(texts(&t, c.leading(nth(5))), ["# d"]);

        // before a separator, the node ending with the separator takes it (the entry,
        // not its object); a comment after the separator then leads the next entry
        let src = "{ ?s ?p ?a # c\n ; # d\n ?q ?b }";
        let t = tree(src, "(Q (G _ (S _ (E _ (O _) _) (E _ (O _))) _))");
        let c = Comments::attach(&t, &RULES);
        assert_eq!(texts(&t, c.trailing(nth(3))), ["# c"]);
        assert!(c.trailing(nth(4)).is_empty());
        assert_eq!(texts(&t, c.leading(nth(5))), ["# d"]);
        // nor does a comment trail after separators when the item before them ends a
        // line with a trailing comment already, whatever node it went to: both would
        // end the same line
        let src = "{ ?s ?p ?a # c\n ; ; # d\n ?q ?b }";
        let t = tree(src, "(Q (G _ (S _ (E _ (O _) _ _) (E _ (O _))) _))");
        let c = Comments::attach(&t, &RULES);
        assert_eq!(texts(&t, c.trailing(nth(4))), ["# c"]);
        assert!(c.trailing(nth(3)).is_empty());
        assert_eq!(texts(&t, c.leading(nth(5))), ["# d"]);
        // after the next item it trails again
        let src = "{ ?s ?p ?a # c\n ; ?q ?b ; # d\n ?r ?e }";
        let t = tree(
            src,
            "(Q (G _ (S _ (E _ (O _) _) (E _ (O _) _) (E _ (O _))) _))",
        );
        let c = Comments::attach(&t, &RULES);
        assert_eq!(texts(&t, c.trailing(nth(5))), ["# d"]);
    }

    #[test]
    fn leading_and_detached_blocks() {
        let src =
            "{ ?a ?b ?c .\n\n  # one\n  # two\n\n\n  # three\n\n  # four\n  # five\n  ?s ?p ?o . }";
        let t = tree(src, "(Q (G _ (S _ (E _ (O _)) _) (S _ (E _ (O _)) _) _))");
        let c = Comments::attach(&t, &RULES);
        let second = nth(5);
        assert_eq!(t.kind(second), NodeKind::TriplesStmt);
        assert_eq!(texts(&t, c.leading(second)), ["# four", "# five"]);
        let detached: Vec<_> = c
            .detached_before(second)
            .iter()
            .map(|b| texts(&t, b))
            .collect();
        assert_eq!(detached, [vec!["# one", "# two"], vec!["# three"]]);
        assert!(c.blank_before(second));
        assert!(!c.blank_before(nth(2)));
        // only the outermost node starting at `?s` takes them
        assert!(c.leading(nth(6)).is_empty() && c.leading(nth(7)).is_empty());

        // a comment after `{` on its line leads the first element
        let t = tree("{ # c\n ?s ?p ?o }", "(Q (G _ (S _ (E _ (O _))) _))");
        let c = Comments::attach(&t, &RULES);
        assert_eq!(texts(&t, c.leading(nth(2))), ["# c"]);
        assert!(c.warnings().is_empty());
    }

    #[test]
    fn dangling_before_a_closer_and_at_the_end() {
        let src = "{ ?s ?p ?o .\n\n  # a\n  # b\n}\n# end\n";
        let t = tree(src, "(Q (G _ (S _ (E _ (O _)) _) _))");
        let c = Comments::attach(&t, &RULES);
        assert_eq!(texts(&t, c.dangling(nth(1))), ["# a", "# b"]);
        assert!(c.blank_before_comment(c.dangling(nth(1))[0]));
        assert_eq!(texts(&t, c.dangling(nth(0))), ["# end"]);

        let t = tree("{ # only\n}", "(Q (G _ _))");
        let c = Comments::attach(&t, &RULES);
        assert_eq!(texts(&t, c.dangling(nth(1))), ["# only"]);
    }

    #[test]
    fn displaced_comments_lead_the_enclosing_node() {
        let src = "{ FILTER # c\n (?x) }";
        let t = tree(src, "(Q (G _ (F _ (B _ _ _)) _))");
        let c = Comments::attach(&t, &RULES);
        let filter = nth(2);
        assert_eq!(texts(&t, c.leading(filter)), ["# c"]);
        // the warning comes once the comment is printed there
        assert!(c.warnings().is_empty());
        let mut a = DocArena::new(&t.tokens);
        let d = a.nil();
        wrap(&mut a, &c, filter, d);
        let w = c.warnings();
        assert_eq!(w.len(), 1);
        assert_eq!(
            (w[0].code, w[0].line, w[0].column),
            ("comment-moved", 1, 10)
        );
        assert!(w[0].message.contains("filter"), "{}", w[0].message);

        // inside a node with no attachment node around it: the root
        let t = tree("{ ?s # c\n ?p ?o }", "(Q (X _ _ _ _ _))");
        let c = Comments::attach(&t, &RULES);
        assert_eq!(texts(&t, c.leading(nth(0))), ["# c"]);
    }

    #[test]
    fn the_ignore_pragma_ends_a_leading_block() {
        let src = "{\n  # keep\n  # sparkles-fmt: ignore\n  ?s   ?p ?o .\n  # sparkles-fmt: ignore\n\n  ?a ?b ?c . }";
        let t = tree(src, "(Q (G _ (S _ (E _ (O _)) _) (S _ (E _ (O _)) _) _))");
        let c = Comments::attach(&t, &RULES);
        assert!(c.ignored(nth(2)));
        // a blank line between the pragma and the node: detached, so no effect
        assert!(!c.ignored(nth(5)));
        assert_eq!(c.detached_before(nth(5)).len(), 1);
    }

    #[test]
    fn line_breaks_count_crlf_once() {
        assert_eq!(line_breaks(" \r\n\r\n "), 2);
        assert_eq!(line_breaks("\r\r"), 2);
        assert_eq!(line_breaks("  "), 0);
    }
}
