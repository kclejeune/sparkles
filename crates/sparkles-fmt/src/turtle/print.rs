//! Turtle and TriG printing: directives in the `directive-style` family, subject blocks in
//! the diff-friendly expanded form (or SPARQL's compact form with `turtle-layout =
//! "conventional"`), object lists, blank node property lists, collections, RDF 1.2 terms
//! and TriG graph blocks.
//!
//! - The document: the file header, the directives, the statements and graph blocks one
//!   after another (with `sort`, ordered between directive blocks and detached comment
//!   blocks, [`sort`]), the comments at the end and one final newline. One blank line
//!   follows a directive block when a statement follows it, and one separates two
//!   statements (or graph blocks) when either prints on more than one line; other blank
//!   lines are kept as written, collapsed to one (not inside a sorted run).
//! - Directives: `PREFIX`/`BASE`/`VERSION`, or `@prefix`/`@base`/`@version` with their
//!   final ` .`, one per line. The leading directive block puts `VERSION` first and
//!   sorts, deduplicates and groups its `PREFIX` runs as a SPARQL prologue does (N5,
//!   `prefix-groups`); directives later in the document are printed where and as they
//!   were written, since a later directive may re-map a label.
//! - Graph blocks: `GRAPH g {` (or `g {` with the Turtle directive style; a default graph
//!   block stays `{`), the statements one level deeper, each ending with `.`, then `}`.
//!
//! The statements, entries, objects and terms are in [`triples`]. The printer works with
//! the SPARQL printer's [`Ctx`] helpers (terms, normalizations, blocks, comments)
//! through [`Tx`], which dispatches nodes to the Turtle printers instead.

pub mod prune;
pub mod triples;

use super::sort::{self, Placed};
use crate::doc::{self, Doc, DocId, Printed};
use crate::lex::TokenKind;
use crate::normalize::{self, PrefixDecl};
use crate::sparql::print::Ctx;
use crate::syntax::NodeKind;
use crate::tree::{Element, NodeId, TokenId, Tree};
use crate::trivia::{self, CommentRules, Comments};
use crate::{DirectiveStyle, FormatError, Options};
use prune::Pruned;
use std::ops::{Deref, DerefMut};
use unicode_width::UnicodeWidthStr;

/// Whether `turtle-layout = "conventional"` is implemented.
pub const CONVENTIONAL_IMPLEMENTED: bool = true;

/// The Turtle printer's context: the SPARQL printer's [`Ctx`] (its helpers for terms,
/// normalizations, blocks and comments), with nodes dispatched by [`node`].
pub struct Tx<'a, 's> {
    pub cx: Ctx<'a, 's>,
    pub trig: bool,
    /// With `sort`: a second printer over the same tree without comments, which builds
    /// the printed forms sorting compares ([`sort::printed`]).
    pub keys: Option<Box<Tx<'a, 's>>>,
}

impl<'a, 's> Deref for Tx<'a, 's> {
    type Target = Ctx<'a, 's>;

    fn deref(&self) -> &Ctx<'a, 's> {
        &self.cx
    }
}

impl DerefMut for Tx<'_, '_> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.cx
    }
}

impl<'a, 's> Tx<'a, 's> {
    pub fn new(
        tree: &'a Tree<'s>,
        comments: &'a Comments,
        opts: &'a Options,
        trig: bool,
    ) -> Tx<'a, 's> {
        Tx {
            cx: Ctx::new(tree, comments, opts),
            trig,
            keys: None,
        }
    }

    /// `doc` with `n`'s comments around it (detached, leading and trailing).
    pub fn wrap(&mut self, n: NodeId, doc: DocId) -> DocId {
        let cx = &mut self.cx;
        trivia::wrap(&mut cx.arena, cx.comments, n, doc)
    }

    /// The width of `d` printed flat, up to its first line break, and whether a line
    /// break follows (inside a long string or a verbatim range); `None` when `d` cannot
    /// print flat (a hard line, a blank line, a forced break).
    pub fn flat_width(&self, d: DocId) -> Option<(usize, bool)> {
        let arena = &self.arena;
        let src = self.tree.src;
        let mut width = 0;
        let mut stack = vec![d];
        while let Some(d) = stack.pop() {
            let text: &str = match arena.get(d) {
                Doc::Nil | Doc::SoftLine | Doc::LineSuffix(_) => continue,
                Doc::HardLine | Doc::EmptyLine | Doc::BreakParent => return None,
                Doc::Line => " ",
                Doc::Text(s) => s,
                Doc::Token { id, printed } => match printed {
                    Some(p) => p,
                    None => arena.tokens()[id.0 as usize].text(src),
                },
                Doc::Verbatim(r) => &src[r.clone()],
                Doc::Indent(x) | Doc::Group { doc: x, .. } => {
                    stack.push(*x);
                    continue;
                }
                Doc::IfBreak { flat, .. } => {
                    stack.push(*flat);
                    continue;
                }
                Doc::Concat(ds) => {
                    stack.extend(ds.iter().rev());
                    continue;
                }
            };
            match text.find(['\n', '\r']) {
                Some(i) => return Some((width + text[..i].width(), true)),
                None => width += text.width(),
            }
        }
        Some((width, false))
    }

    /// Whether a node inside `n` (not `n` itself) has comments to print, or `n` has
    /// dangling ones: then `n` cannot print on one line.
    pub fn inner_comments(&self, n: NodeId) -> bool {
        let live = |c: &TokenId| !self.comments.copied(*c);
        if self.comments.dangling(n).iter().any(live) {
            return true;
        }
        let mut stack: Vec<NodeId> = self.tree.child_nodes(n).collect();
        while let Some(d) = stack.pop() {
            if self.comments.has_comments(d) {
                return true;
            }
            stack.extend(self.tree.child_nodes(d));
        }
        false
    }
}

/// Print a whole Turtle (`trig: false`) or TriG tree.
pub fn print(
    tree: &Tree<'_>,
    comments: &Comments,
    opts: &Options,
    trig: bool,
) -> Result<Printed, FormatError> {
    let plain = Comments::default();
    let mut tx = Tx::new(tree, comments, opts, trig);
    if opts.sort {
        tx.keys = Some(Box::new(Tx::new(tree, &plain, opts, trig)));
    }
    let root = node(&mut tx, tree.root());
    doc::print(
        &tx.arena,
        root,
        tree.src,
        opts.line_width,
        opts.indent_width,
        opts.deadline,
    )
}

/// The document of node `n`, with its comments; a node under `# sparkles-fmt: ignore`
/// as written.
pub fn node(tx: &mut Tx<'_, '_>, n: NodeId) -> DocId {
    use NodeKind as K;
    if tx.comments.ignored(n) {
        let doc = tx.verbatim(n);
        return tx.wrap(n, doc);
    }
    let doc = match tx.tree.kind(n) {
        K::TurtleDoc | K::TrigDoc => document(tx, n),
        K::PrefixDecl | K::BaseDecl | K::VersionDecl => directive(tx, n),
        // the graph block, statement and entry printers wrap their own comments
        K::GraphBlock => return graph_block(tx, n).0,
        K::TriplesStmt => return triples::statement(tx, n, 0).0,
        K::PropertyListEntry => {
            let semi = match tx.child_token(n, TokenKind::Semicolon) {
                Some(_) => triples::Semi::Always,
                None => triples::Semi::Never,
            };
            return triples::entry(tx, n, semi);
        }
        K::Object => triples::object(tx, n),
        K::BNodePropertyList => {
            triples::property_block(tx, n, TokenKind::LBracket, TokenKind::RBracket)
        }
        K::AnnotationBlock => {
            triples::property_block(tx, n, TokenKind::LBracePipe, TokenKind::PipeRBrace)
        }
        K::Collection => triples::collection(tx, n),
        K::CollectionItem | K::ReifiedTriple | K::TripleTerm | K::Reifier => triples::spaced(tx, n),
        K::Literal => crate::sparql::print::term::literal(&mut tx.cx, n),
        _ => tx.verbatim(n),
    };
    tx.wrap(n, doc)
}

/// A child element: a node with [`node`], a token as a term (`()` and `[]` without the
/// whitespace inside).
pub fn element(tx: &mut Tx<'_, '_>, e: Element) -> DocId {
    match e {
        Element::Node(c) => node(tx, c),
        Element::Token(t) => crate::sparql::print::term::token(&mut tx.cx, t),
    }
}

/// A top-level part of the document.
#[derive(Clone, Debug)]
enum Part {
    /// consecutive directives
    Directives(Vec<NodeId>),
    /// a statement or a graph block
    Statement(NodeId),
}

/// A top-level part in printing order.
enum Item {
    /// consecutive directives; whether they are the leading block; what
    /// `prune-prefixes` drops
    Directives(Vec<NodeId>, bool, Pruned),
    /// a statement or a graph block
    Statement(Placed),
}

/// `TurtleDoc`, `TrigDoc`: the file header, the directive blocks, statements and graph
/// blocks (sorted between the directive blocks with `sort`), the comments at the end,
/// one final newline.
fn document(tx: &mut Tx<'_, '_>, n: NodeId) -> DocId {
    let mut parts_of: Vec<Part> = Vec::new();
    for c in tx.child_nodes(n) {
        let directive = matches!(
            tx.tree.kind(c),
            NodeKind::PrefixDecl | NodeKind::BaseDecl | NodeKind::VersionDecl
        );
        match (directive, parts_of.last_mut()) {
            (true, Some(Part::Directives(ds))) => ds.push(c),
            (true, _) => parts_of.push(Part::Directives(vec![c])),
            (false, _) => parts_of.push(Part::Statement(c)),
        }
    }
    // the statements between directive blocks, in printing order; a block that prints
    // nothing (every declaration pruned) separates nothing
    let mut items = Vec::new();
    let mut statements = Vec::new();
    for (i, part) in parts_of.iter().enumerate() {
        match part {
            Part::Statement(s) => statements.push(*s),
            Part::Directives(ds) => {
                let pruned = prune::pruned(tx, ds, i == 0);
                if pruned.prints_nothing(ds) {
                    continue;
                }
                let order = statement_order(tx, &std::mem::take(&mut statements));
                items.extend(order.into_iter().map(Item::Statement));
                items.push(Item::Directives(ds.clone(), i == 0, pruned));
            }
        }
    }
    let order = statement_order(tx, &statements);
    items.extend(order.into_iter().map(Item::Statement));
    let mut parts = Vec::new();
    parts.extend(tx.header());
    // (a directive block, prints on several lines)
    let mut prev: Option<(bool, bool)> = None;
    for item in &items {
        let (doc, blank_before, directives, multi) = match item {
            Item::Directives(ds, leading, pruned) => {
                let blank = tx.comments.blank_before(ds[0]);
                (
                    directive_block(tx, ds, *leading, pruned),
                    blank,
                    true,
                    false,
                )
            }
            Item::Statement(p) => {
                let opening = run_opening(tx, p);
                let (doc, multi) = match tx.tree.kind(p.node) {
                    NodeKind::GraphBlock => graph_block(tx, p.node),
                    _ => triples::statement(tx, p.node, 0),
                };
                let doc = match opening {
                    Some(o) => tx.concat([o, doc]),
                    None => doc,
                };
                (doc, p.blank, false, multi)
            }
        };
        if let Some((prev_directives, prev_multi)) = prev {
            let blank = blank_before
                || (prev_directives && !directives)
                || (!prev_directives && !directives && (prev_multi || multi));
            parts.push(match blank {
                true => tx.empty_line(),
                false => tx.hard_line(),
            });
        }
        parts.push(doc);
        prev = Some((directives, multi));
    }
    parts.extend(tx.dangling(n, prev.is_some()));
    parts.push(tx.hard_line());
    tx.concat(parts)
}

/// Statements and graph blocks in printing order: as written, or with `sort` by printed
/// subject, TriG's default graph blocks after the triples outside any block, then the
/// named graph blocks by printed name, within the runs between detached comment blocks.
fn statement_order(tx: &mut Tx<'_, '_>, statements: &[NodeId]) -> Vec<Placed> {
    let keys = match tx.opts.sort {
        false => None,
        true => Some(
            statements
                .iter()
                .map(|&s| statement_key(tx, s))
                .collect::<Vec<_>>(),
        ),
    };
    sort::order(
        tx.comments,
        statements,
        keys.as_deref(),
        tx.opts.sort,
        false,
    )
}

/// What a statement or graph block sorts by: the default graph first, then its printed
/// subject or name (its source text under the ignore pragma).
fn statement_key(tx: &mut Tx<'_, '_>, s: NodeId) -> (u8, String) {
    let ignored = tx.comments.ignored(s);
    if tx.tree.kind(s) == NodeKind::GraphBlock {
        let label = tx.children(s).into_iter().find(|e| {
            matches!(*e, Element::Token(t) if !matches!(
                tx.tree.token_kind(t),
                TokenKind::Kw(_) | TokenKind::LBrace | TokenKind::RBrace
            ))
        });
        return match (label, ignored) {
            (None, _) => (1, sort::printed(tx, Element::Node(s))),
            (Some(_), true) => (2, sort::printed(tx, Element::Node(s))),
            (Some(l), false) => (2, sort::printed(tx, l)),
        };
    }
    match (tx.children(s).first().copied(), ignored) {
        (Some(subject), false) => (0, sort::printed(tx, subject)),
        _ => (0, sort::printed(tx, Element::Node(s))),
    }
}

/// The detached comment blocks that open a run, when the node written first in the run
/// moved further down: each block on its lines, a blank line after it.
pub fn run_opening(tx: &mut Tx<'_, '_>, p: &Placed) -> Option<DocId> {
    let head = p.run_head.filter(|&h| h != p.node)?;
    let mut parts = Vec::new();
    for block in tx.comments.detached_before(head).to_vec() {
        for &c in &block {
            tx.comments.mark_printed(c);
        }
        let cx = &mut tx.cx;
        parts.push(trivia::comment_lines(&mut cx.arena, cx.comments, &block));
        parts.push(tx.empty_line());
    }
    match parts.is_empty() {
        true => None,
        false => Some(tx.concat(parts)),
    }
}

/// Consecutive directives, one per line, without those `pruned` drops (their detached
/// comment blocks and blank lines pass on to the next item). The leading block
/// (`sorted`) puts `VERSION` first and sorts, deduplicates and groups each run of
/// `PREFIX` declarations as `prefix-groups` says (a run that binds a label twice stays as
/// written); a `BASE`, a `VERSION` or a detached comment block ends a run, and without
/// groups a blank line does too. Later blocks are printed as written, their blank lines
/// kept.
fn directive_block(tx: &mut Tx<'_, '_>, decls: &[NodeId], sorted: bool, pruned: &Pruned) -> DocId {
    // (node, its document, whether a blank line goes before it)
    let mut items: Vec<(NodeId, DocId, bool)> = Vec::new();
    // an item after dropped declarations: their detached blocks first
    let passed = |tx: &mut Tx<'_, '_>, d: NodeId| -> (Vec<DocId>, bool) {
        let blocks = pruned.blocks.get(&d).map_or(&[][..], Vec::as_slice);
        let parts = prune::comment_blocks(tx, blocks);
        (
            parts,
            tx.comments.blank_before(d) || pruned.blank.contains(&d),
        )
    };
    if !sorted {
        for &d in decls {
            if pruned.nodes.contains(&d) {
                continue;
            }
            let (mut parts, blank) = passed(tx, d);
            parts.push(node(tx, d));
            let doc = tx.concat(parts);
            items.push((d, doc, blank));
        }
        end_blocks(tx, pruned, &mut items);
        return lines(tx, &items);
    }
    let (versions, rest): (Vec<NodeId>, Vec<NodeId>) = decls
        .iter()
        .partition(|&&d| tx.tree.kind(d) == NodeKind::VersionDecl);

    // the prefix declarations that are printed, for the run planning
    let mut prefixes: Vec<PrefixDecl> = Vec::new();
    let mut after_other = false;
    for (i, &d) in decls.iter().enumerate() {
        if tx.tree.kind(d) != NodeKind::PrefixDecl {
            after_other = i > 0;
            continue;
        }
        if pruned.nodes.contains(&d) {
            continue;
        }
        let (label, iri) = prune::prefix_parts(tx, d);
        prefixes.push(PrefixDecl {
            node: d,
            label,
            iri,
            // a detached block stays at the start of the run, so it does not count
            has_comments: !tx.comments.leading(d).is_empty() || !tx.comments.trailing(d).is_empty(),
            barrier_before: after_other
                || !tx.comments.detached_before(d).is_empty()
                || pruned.blocks.contains_key(&d),
            blank_before: tx.comments.blank_before(d) || pruned.blank.contains(&d),
        });
        after_other = false;
    }
    let runs = normalize::plan_runs(&prefixes, &tx.opts.prefix_groups);

    for &v in &versions {
        let d = node(tx, v);
        items.push((v, d, tx.comments.blank_before(v)));
    }
    let mut next_prefix = 0;
    let mut runs = runs.into_iter();
    for &d in &rest {
        if tx.tree.kind(d) != NodeKind::PrefixDecl {
            let (mut parts, blank) = passed(tx, d);
            parts.push(node(tx, d));
            let doc = tx.concat(parts);
            items.push((d, doc, blank));
            continue;
        }
        if prefixes.get(next_prefix).map(|p| p.node) != Some(d) {
            continue;
        }
        let Some(run) = runs.next() else { continue };
        let len = run.order.len() + run.drop.len();
        // the run starts where its first declaration was written, and the detached
        // comment blocks before it (a section header, and those of the dropped
        // declarations before it) stay there, whatever moves first
        let lead_blank = prefixes[next_prefix].blank_before;
        let mut blocks = pruned.blocks.get(&d).cloned().unwrap_or_default();
        blocks.extend(tx.comments.detached_before(d).iter().cloned());
        let mut detached = prune::comment_blocks(tx, &blocks);
        for (k, &i) in run.order.iter().enumerate() {
            let decl = prefixes[i].node;
            let mut doc = node(tx, decl);
            if k == 0 && !detached.is_empty() {
                detached.push(doc);
                doc = tx.concat(std::mem::take(&mut detached));
            }
            let blank = match (k, run.sorted) {
                (0, _) => lead_blank,
                (_, true) => run.group_breaks.contains(&k),
                (_, false) => prefixes[i].blank_before,
            };
            items.push((decl, doc, blank));
        }
        next_prefix += len;
    }
    end_blocks(tx, pruned, &mut items);
    lines(tx, &items)
}

/// The detached blocks of dropped declarations with no item after them, as the block's
/// last item.
fn end_blocks(tx: &mut Tx<'_, '_>, pruned: &Pruned, items: &mut Vec<(NodeId, DocId, bool)>) {
    if pruned.end_blocks.is_empty() {
        return;
    }
    let mut parts = prune::comment_blocks(tx, &pruned.end_blocks);
    parts.pop();
    let doc = tx.concat(parts);
    let root = tx.tree.root();
    items.push((root, doc, true));
}

/// The documents one per line, a blank line before those that ask for one (never before
/// the first).
fn lines(tx: &mut Tx<'_, '_>, items: &[(NodeId, DocId, bool)]) -> DocId {
    let mut parts = Vec::new();
    for (k, &(_, doc, blank)) in items.iter().enumerate() {
        if k > 0 {
            parts.push(match blank {
                true => tx.empty_line(),
                false => tx.hard_line(),
            });
        }
        parts.push(doc);
    }
    tx.concat(parts)
}

/// `PrefixDecl`, `BaseDecl`, `VersionDecl` in the `directive-style` family: `PREFIX ex:
/// <…>`, or `@prefix ex: <…> .`. IRIs as written (never compacted); the version string
/// in the configured quote style.
fn directive(tx: &mut Tx<'_, '_>, n: NodeId) -> DocId {
    let turtle = tx.opts.directive_style == DirectiveStyle::Turtle;
    let keyword = match (tx.tree.kind(n), turtle) {
        (NodeKind::PrefixDecl, false) => "PREFIX",
        (NodeKind::PrefixDecl, true) => "@prefix",
        (NodeKind::BaseDecl, false) => "BASE",
        (NodeKind::BaseDecl, true) => "@base",
        (_, false) => "VERSION",
        (_, true) => "@version",
    };
    let mut words = Vec::new();
    let mut dot = None;
    for (i, e) in tx.children(n).into_iter().enumerate() {
        let Element::Token(t) = e else { continue };
        let d = match tx.tree.token_kind(t) {
            _ if i == 0 => tx.tok_as(t, keyword),
            TokenKind::Dot => {
                dot = Some(t);
                continue;
            }
            k if k.is_string() => tx.term(t),
            _ => tx.tok(t),
        };
        words.push(d);
    }
    if turtle {
        words.push(match dot {
            Some(t) => tx.tok(t),
            None => tx.text("."),
        });
    }
    tx.spaced(words)
}

/// `GraphBlock`: `GRAPH g {` (`g {` with the Turtle directive style, `{` for the default
/// graph), the statements one level deeper, `}`; `{}` when empty. Whether it prints on
/// several lines.
fn graph_block(tx: &mut Tx<'_, '_>, n: NodeId) -> (DocId, bool) {
    if tx.comments.ignored(n) {
        let doc = tx.verbatim(n);
        let multi = tx.tree.text(n).contains(['\n', '\r']);
        return (tx.wrap(n, doc), multi);
    }
    let turtle = tx.opts.directive_style == DirectiveStyle::Turtle;
    let mut head = Vec::new();
    let mut keyword = None;
    let mut open = None;
    let mut close = None;
    let mut statements = Vec::new();
    for e in tx.children(n) {
        match e {
            Element::Token(t) => match tx.tree.token_kind(t) {
                TokenKind::Kw(_) => keyword = Some(t),
                TokenKind::LBrace => open = Some(t),
                TokenKind::RBrace => close = Some(t),
                _ => {
                    // the label
                    if !turtle {
                        head.push(match keyword {
                            Some(k) => tx.tok_as(k, "GRAPH"),
                            None => tx.text("GRAPH"),
                        });
                    }
                    head.push(crate::sparql::print::term::token(&mut tx.cx, t));
                }
            },
            Element::Node(s) => statements.push(s),
        }
    }
    let open = match open {
        Some(t) => tx.tok(t),
        None => tx.text("{"),
    };
    let close = match close {
        Some(t) => tx.tok(t),
        None => tx.text("}"),
    };
    let indent = usize::from(tx.opts.indent_width);
    let body = statement_list(tx, &statements, indent);
    let dangling = tx.dangling(n, !statements.is_empty());
    let multi = body.is_some() || dangling.is_some();
    let block = match (body, dangling) {
        (None, None) => tx.concat([open, close]),
        (body, dangling) => {
            let mut inner = Vec::new();
            if let Some(b) = body {
                inner.push(tx.hard_line());
                inner.push(b);
            }
            inner.extend(dangling);
            let inner = tx.concat(inner);
            let inner = tx.indent(inner);
            let hl = tx.hard_line();
            tx.concat([open, inner, hl, close])
        }
    };
    head.push(block);
    let doc = tx.spaced(head);
    (tx.wrap(n, doc), multi)
}

/// Statements one per line at `indent` columns (sorted with `sort`), with a blank line
/// between two of them where the source had one or where either prints on several
/// lines; `None` when there are none.
fn statement_list(tx: &mut Tx<'_, '_>, statements: &[NodeId], indent: usize) -> Option<DocId> {
    let mut parts = Vec::new();
    let mut prev_multi = None;
    for p in statement_order(tx, statements) {
        let opening = run_opening(tx, &p);
        let (doc, multi) = triples::statement(tx, p.node, indent);
        let doc = match opening {
            Some(o) => tx.concat([o, doc]),
            None => doc,
        };
        if let Some(prev_multi) = prev_multi {
            let blank = prev_multi || multi || p.blank;
            parts.push(match blank {
                true => tx.empty_line(),
                false => tx.hard_line(),
            });
        }
        parts.push(doc);
        prev_multi = Some(multi);
    }
    prev_multi.map(|_| tx.concat(parts))
}

/// Turtle's and TriG's comment attachment rules. A statement is the container of the
/// comments on their own lines before its `.` (the lone `.` of an expanded statement
/// comes after them).
pub struct TurtleRules;

pub static RULES: TurtleRules = TurtleRules;

impl CommentRules for TurtleRules {
    fn is_attachment(&self, kind: NodeKind) -> bool {
        use NodeKind as K;
        matches!(
            kind,
            K::BaseDecl
                | K::PrefixDecl
                | K::VersionDecl
                | K::TriplesStmt
                | K::GraphBlock
                | K::PropertyListEntry
                | K::Object
                | K::CollectionItem
        )
    }

    fn is_container(&self, kind: NodeKind) -> bool {
        use NodeKind as K;
        matches!(
            kind,
            K::GraphBlock
                | K::BNodePropertyList
                | K::Collection
                | K::AnnotationBlock
                | K::TriplesStmt
        )
    }

    fn is_separator(&self, kind: TokenKind) -> bool {
        use TokenKind as T;
        matches!(kind, T::Comma | T::Semicolon | T::Dot)
    }

    fn is_closer(&self, kind: TokenKind) -> bool {
        kind == TokenKind::Dot || crate::sparql::parse::is_closer(kind)
    }
}

#[cfg(test)]
mod tests {
    use super::super::Turtle;
    use crate::{DirectiveStyle, Options};

    fn fmt_with(src: &str, trig: bool, opts: &Options) -> String {
        let out = crate::check::run(&Turtle { trig }, src, opts)
            .unwrap_or_else(|e| panic!("{src:?}: {e}"))
            .text;
        // a fixpoint
        let again = crate::check::run(&Turtle { trig }, &out, opts)
            .unwrap()
            .text;
        assert_eq!(again, out, "not a fixpoint");
        out
    }

    fn fmt(src: &str) -> String {
        fmt_with(src, false, &Options::default())
    }

    fn trig(src: &str) -> String {
        fmt_with(src, true, &Options::default())
    }

    fn narrow() -> Options {
        Options {
            line_width: 40,
            ..Options::default()
        }
    }

    const P: &str = "PREFIX ex: <http://example.org/>\n\n";

    #[test]
    fn flat_and_expanded_statements() {
        assert_eq!(
            fmt("@prefix ex: <http://example.org/> . ex:s ex:p ex:o."),
            format!("{P}ex:s ex:p ex:o .\n")
        );
        assert_eq!(
            fmt(&format!("{P}ex:s ex:p ex:o ; ex:q ex:r .")),
            format!("{P}ex:s\n  ex:p ex:o ;\n  ex:q ex:r ;\n.\n")
        );
        // two objects are not flat, a long single one breaks the statement
        assert_eq!(
            fmt(&format!("{P}ex:s ex:p 1, 2 .")),
            format!("{P}ex:s\n  ex:p 1, 2 ;\n.\n")
        );
        assert_eq!(
            fmt_with(
                &format!("{P}ex:subject ex:predicate ex:objectThatIsLong ."),
                false,
                &narrow()
            ),
            format!("{P}ex:subject\n  ex:predicate ex:objectThatIsLong ;\n.\n")
        );
        // a blank node property list or a reified triple alone
        assert_eq!(
            fmt(&format!(
                "{P}[ ex:p ex:o ] . [ ex:p 1 ; ex:q 2 ] . << ex:a ex:b ex:c >> ."
            )),
            format!(
                "{P}[ ex:p ex:o ] .\n\n[\n  ex:p 1 ;\n  ex:q 2 ;\n] .\n\n<< ex:a ex:b ex:c >> .\n"
            )
        );
        // one blank line between statements when either is multi-line, as written
        // (collapsed) otherwise
        assert_eq!(
            fmt(&format!(
                "{P}ex:a ex:p 1 .\nex:b ex:p 1 .\n\n\n\nex:c ex:p 1 .\nex:d ex:p 1, 2 .\nex:e ex:p 1 ."
            )),
            format!(
                "{P}ex:a ex:p 1 .\nex:b ex:p 1 .\n\nex:c ex:p 1 .\n\nex:d\n  ex:p 1, 2 ;\n.\n\nex:e ex:p 1 .\n"
            )
        );
    }

    #[test]
    fn objects_blocks_and_collections() {
        assert_eq!(
            fmt_with(
                &format!("{P}ex:s ex:p ex:objectNumberOne, ex:objectNumberTwo ; ex:q 1 ."),
                false,
                &narrow()
            ),
            format!(
                "{P}ex:s\n  ex:p\n    ex:objectNumberOne,\n    ex:objectNumberTwo ;\n  ex:q 1 ;\n.\n"
            )
        );
        // blocks hug, `], [`
        assert_eq!(
            fmt(&format!(
                "{P}ex:s ex:p [ ex:a 1 ; ex:b 2 ], [ ex:c 3 ; ex:d 4 ] ."
            )),
            format!(
                "{P}ex:s\n  ex:p [\n    ex:a 1 ;\n    ex:b 2 ;\n  ], [\n    ex:c 3 ;\n    ex:d 4 ;\n  ] ;\n.\n"
            )
        );
        // a single entry breaks only when it does not fit, and then ends with ` ;`
        assert_eq!(
            fmt(&format!("{P}ex:s ex:p [ ex:a 1 ] .")),
            format!("{P}ex:s ex:p [ ex:a 1 ] .\n")
        );
        assert_eq!(
            fmt_with(
                &format!("{P}ex:s ex:p [ ex:aVeryLongPredicate ex:aVeryLongObject ] ."),
                false,
                &narrow()
            ),
            format!(
                "{P}ex:s\n  ex:p [\n    ex:aVeryLongPredicate ex:aVeryLongObject ;\n  ] ;\n.\n"
            )
        );
        assert_eq!(
            fmt_with(
                &format!("{P}ex:s ex:p ( ex:itemNumberOne ex:itemNumberTwo ) ."),
                false,
                &narrow()
            ),
            format!("{P}ex:s\n  ex:p (\n    ex:itemNumberOne\n    ex:itemNumberTwo\n  ) ;\n.\n")
        );
        assert_eq!(
            fmt(&format!("{P}ex:s ex:p ( ), ( 1 ), [] .")),
            format!("{P}ex:s\n  ex:p (), ( 1 ), [] ;\n.\n")
        );
        // annotation blocks hug the object
        assert_eq!(
            fmt(&format!(
                "{P}ex:s ex:p ex:o ~ ex:r {{| ex:a 1 ; ex:b 2 |}} ."
            )),
            format!("{P}ex:s\n  ex:p ex:o ~ ex:r {{|\n    ex:a 1 ;\n    ex:b 2 ;\n  |}} ;\n.\n")
        );
        assert_eq!(
            fmt(&format!(
                "{P}ex:s ex:p ex:o ~{{|ex:a 1|}}, <<( ex:a ex:b ex:c )>> ."
            )),
            format!("{P}ex:s\n  ex:p ex:o ~ {{| ex:a 1 |}}, <<( ex:a ex:b ex:c )>> ;\n.\n")
        );
    }

    #[test]
    fn type_entries_first() {
        assert_eq!(
            fmt(&format!(
                "{P}ex:s ex:p 1 ; <http://www.w3.org/1999/02/22-rdf-syntax-ns#type> ex:C ; ex:q [ ex:r 2 ; a ex:D ] ."
            )),
            format!(
                "{P}ex:s\n  a ex:C ;\n  ex:p 1 ;\n  ex:q [\n    a ex:D ;\n    ex:r 2 ;\n  ] ;\n.\n"
            )
        );
        // a detached comment block is a barrier
        assert_eq!(
            fmt(&format!(
                "{P}ex:s ex:p 1 ;\n\n  # types\n\n  ex:q 2 ; a ex:C ."
            )),
            format!("{P}ex:s\n  ex:p 1 ;\n\n  # types\n\n  a ex:C ;\n  ex:q 2 ;\n.\n")
        );
        let off = Options {
            type_shorthand: false,
            ..Options::default()
        };
        assert_eq!(
            fmt_with(&format!("{P}ex:s ex:p 1 ; a ex:C ."), false, &off),
            format!("{P}ex:s\n  ex:p 1 ;\n  a ex:C ;\n.\n")
        );
    }

    #[test]
    fn comments() {
        // the last entry keeps its comment; comments before the lone `.` stay there
        assert_eq!(
            fmt(&format!(
                "{P}ex:s\n  ex:p ex:o ; # last\n  # ex:q ex:r ;\n."
            )),
            format!("{P}ex:s\n  ex:p ex:o ; # last\n  # ex:q ex:r ;\n.\n")
        );
        // a statement's trailing comment keeps it flat
        assert_eq!(
            fmt(&format!("{P}ex:s ex:p ex:o # c\n .")),
            format!("{P}ex:s ex:p ex:o . # c\n")
        );
        // comments inside a statement expand it
        assert_eq!(
            fmt(&format!("{P}ex:s # c\n ex:p ex:o .")),
            format!("{P}ex:s\n  # c\n  ex:p ex:o ;\n.\n")
        );
        assert_eq!(
            fmt(&format!("{P}ex:s ex:p [ # c\n ], ( # d\n ) .")),
            format!("{P}ex:s\n  ex:p\n    [\n      # c\n    ],\n    (\n      # d\n    ) ;\n.\n")
        );
        assert_eq!(
            fmt(&format!("{P}ex:s ex:p 1, # one\n 2 .")),
            format!("{P}ex:s\n  ex:p\n    1, # one\n    2 ;\n.\n")
        );
        // the file header, a section header, the end of the file
        assert_eq!(
            fmt(
                "# header\n\n@prefix ex: <http://example.org/> .\n\n# section\n\nex:s ex:p ex:o .\n# end\n"
            ),
            "# header\n\nPREFIX ex: <http://example.org/>\n\n# section\n\nex:s ex:p ex:o .\n# end\n"
        );
    }

    #[test]
    fn ignore_pragma() {
        let src = format!(
            "{P}# sparkles-fmt: ignore\nex:s   ex:p   ex:o ;\n  ex:q ex:r .\nex:t ex:p 'x' .\n"
        );
        assert_eq!(
            fmt(&src),
            format!(
                "{P}# sparkles-fmt: ignore\nex:s   ex:p   ex:o ;\n  ex:q ex:r .\n\nex:t ex:p \"x\" .\n"
            )
        );
        let src = format!("{P}ex:s ex:a 1 ;\n  # sparkles-fmt: ignore\n  ex:b   2 ; ex:c 3 .\n");
        assert_eq!(
            fmt(&src),
            format!("{P}ex:s\n  ex:a 1 ;\n  # sparkles-fmt: ignore\n  ex:b   2 ;\n  ex:c 3 ;\n.\n")
        );
        let src = format!("{P}ex:s ex:a 1 ;\n  # sparkles-fmt: ignore\n  ex:b   2 .\n");
        assert_eq!(
            fmt(&src),
            format!("{P}ex:s\n  ex:a 1 ;\n  # sparkles-fmt: ignore\n  ex:b   2 ;\n.\n")
        );
    }

    #[test]
    fn directives() {
        let src = "@prefix z: <http://z/> .\nprefix a: <http://a/>\n@prefix z: <http://z/> .\nversion '1.2'\nbase <http://b/>\n@prefix b: <b/> .\nz:s a:p b:o .\nPREFIX y: <http://y/>\n@prefix x: <http://x/> .\ny:s x:p 1 .\n";
        assert_eq!(
            fmt(src),
            "VERSION \"1.2\"\nPREFIX a: <http://a/>\nPREFIX z: <http://z/>\nBASE <http://b/>\nPREFIX b: <b/>\n\nz:s a:p b:o .\nPREFIX y: <http://y/>\nPREFIX x: <http://x/>\n\ny:s x:p 1 .\n"
        );
        let turtle = Options {
            directive_style: DirectiveStyle::Turtle,
            ..Options::default()
        };
        assert_eq!(
            fmt_with(src, false, &turtle),
            "@version \"1.2\" .\n@prefix a: <http://a/> .\n@prefix z: <http://z/> .\n@base <http://b/> .\n@prefix b: <b/> .\n\nz:s a:p b:o .\n@prefix y: <http://y/> .\n@prefix x: <http://x/> .\n\ny:s x:p 1 .\n"
        );
    }

    #[test]
    fn graph_blocks() {
        let src = "PREFIX ex: <http://example.org/>\nex:g { ex:a ex:b ex:c } graph ex:h { ex:a ex:b ex:c . ex:d ex:e ex:f } { ex:a ex:b 1, 2 } GRAPH [] {} ex:a ex:b ex:c .";
        assert_eq!(
            trig(src),
            "PREFIX ex: <http://example.org/>\n\nGRAPH ex:g {\n  ex:a ex:b ex:c .\n}\n\nGRAPH ex:h {\n  ex:a ex:b ex:c .\n  ex:d ex:e ex:f .\n}\n\n{\n  ex:a\n    ex:b 1, 2 ;\n  .\n}\n\nGRAPH [] {}\nex:a ex:b ex:c .\n"
        );
        let turtle = Options {
            directive_style: DirectiveStyle::Turtle,
            ..Options::default()
        };
        assert_eq!(
            fmt_with(
                "GRAPH <http://g> { <http://a> <http://b> <http://c> . # c\n # d\n }",
                true,
                &turtle
            ),
            "<http://g> {\n  <http://a> <http://b> <http://c> . # c\n  # d\n}\n"
        );
    }

    #[test]
    fn terms_are_normalized() {
        assert_eq!(
            fmt(
                "PREFIX ex: <http://example.org/>\nPREFIX xsd: <http://www.w3.org/2001/XMLSchema#>\n<http://example.org/s> ex:p '1'^^xsd:integer, \"1\"^^xsd:decimal, '''a\nb''', 'it\\'s' ; ex:q <http://example.org/a.>, \"x\"@EN ."
            ),
            "PREFIX ex: <http://example.org/>\nPREFIX xsd: <http://www.w3.org/2001/XMLSchema#>\n\nex:s\n  ex:p 1, \"1\"^^xsd:decimal, \"\"\"a\nb\"\"\", \"it's\" ;\n  ex:q <http://example.org/a.>, \"x\"@EN ;\n.\n"
        );
    }
}
