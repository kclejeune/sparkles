//! SHACLC printing.
//!
//! - The document: the file header, then the directives and the shapes one after another.
//!   Directives stay in the order written, one per line, with a blank line between two of
//!   them only where the source had one (sorting them could change which declaration of a
//!   label wins). A blank line separates a directive block from a shape and every two
//!   shapes. Keywords print as `BASE`, `PREFIX`, `IMPORTS`, `shape` and `shapeClass`, and
//!   the booleans as `true` and `false`.
//! - A shape: `shape iri -> class class {` on one line, then its constraints one per line
//!   at one indentation level, then `}`. An empty shape is `{}`. Blank lines between
//!   constraints are kept as written, collapsed to one.
//! - A constraint ends with ` .` and prints on one line while it fits in `line-width`.
//!   Otherwise its first part stays on the first line (the path of a property shape, the
//!   first parameter of a node constraint) and every other part goes on a line of its own
//!   one level deeper. A nested shape is a block that closes with `}` on its own line, and
//!   the parts around it wrap in separate runs.
//! - Parameters are `name=value` with no spaces, a `|` chain has none either, paths print
//!   without spaces, and an array is `[a b]` or, when it does not fit, one value per line.
//!   A chain that does not fit breaks before its `|`.
//! - `align-values` pads the cells of consecutive one-line property shapes into columns.
//! - `prune-prefixes` drops the declarations nothing uses in their scope.
//!
//! `compact-iris` and `quote-style` apply to IRIs and strings, but not to the IRI after
//! `@` (it is a different token from `@ex:Shape`) nor to `BASE` and `PREFIX`. Sorting
//! options, `directive-style` and `prefix-groups` do not apply.

use crate::doc::{self, DocId, Printed};
use crate::lex::TokenKind as T;
use crate::normalize::prune;
use crate::sparql::print::Ctx;
use crate::syntax::NodeKind as K;
use crate::tree::{Element, NodeId, TokenId, Tree};
use crate::trivia::{self, CommentRules, Comments};
use crate::{FormatError, Options};
use std::collections::HashSet;
use std::ops::{Deref, DerefMut};

/// The SHACLC printer's context: the shared [`Ctx`] helpers and the prefix declarations
/// `prune-prefixes` drops.
pub struct Sx<'a, 's> {
    pub cx: Ctx<'a, 's>,
    dropped: HashSet<NodeId>,
}

impl<'a, 's> Deref for Sx<'a, 's> {
    type Target = Ctx<'a, 's>;

    fn deref(&self) -> &Ctx<'a, 's> {
        &self.cx
    }
}

impl DerefMut for Sx<'_, '_> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.cx
    }
}

/// Print a whole SHACLC tree.
pub fn print(tree: &Tree<'_>, comments: &Comments, opts: &Options) -> Result<Printed, FormatError> {
    let cx = Ctx::new(tree, comments, opts);
    let dropped = dropped_prefixes(&cx);
    let mut sx = Sx { cx, dropped };
    let root = node(&mut sx, tree.root());
    doc::print(
        &sx.arena,
        root,
        tree.src,
        opts.line_width,
        opts.indent_width,
        opts.deadline,
    )
}

fn is_directive(kind: K) -> bool {
    matches!(kind, K::BaseDecl | K::PrefixDecl | K::ShaclcImports)
}

/// The prefix declarations `prune-prefixes` drops: those nothing in the output uses in
/// their scope, that have no comment of their own, and that do not re-map a label
/// to another namespace than an earlier declaration that is printed did (dropping such a declaration
/// would extend the earlier binding over its scope, and the next run would compact IRIs
/// there with it). A document of directives alone keeps them all, and so does a first
/// declaration when the document has a file header. The comments above the first
/// declaration printed are a file header the next time, and a declaration that kept its
/// comments would otherwise lose them and its place the second time.
fn dropped_prefixes(cx: &Ctx<'_, '_>) -> HashSet<NodeId> {
    if !cx.opts.prune_prefixes {
        return HashSet::new();
    }
    let tree = cx.tree;
    let items: Vec<NodeId> = tree.child_nodes(tree.root()).collect();
    if items.iter().all(|&n| is_directive(tree.kind(n))) {
        return HashSet::new();
    }
    // the IRI after `@` is never compacted, and a node under an ignore pragma is printed
    // as written
    let as_written = |n: NodeId| {
        cx.comments.ignored(n)
            || (tree.kind(n) == K::ShaclcAtom
                && tree
                    .first_token(n)
                    .is_some_and(|t| matches!(tree.token_kind(t), T::At)))
    };
    let opts = Options {
        type_shorthand: false,
        ..cx.opts.clone()
    };
    let unused = prune::unused_declarations_with(tree, &cx.scope, &opts, as_written);
    // with a file header, the first item stays: comments printed above the first
    // declaration are the file header the next time
    let under_header = !cx.comments.header().is_empty();
    let mut bound: Vec<(&str, &str)> = Vec::new();
    let mut out = HashSet::new();
    for (i, &n) in items.iter().enumerate() {
        if tree.kind(n) != K::PrefixDecl {
            continue;
        }
        let tokens: Vec<&str> = tree
            .children(n)
            .iter()
            .filter_map(|e| match *e {
                Element::Token(t) => Some(tree.token_text(t)),
                Element::Node(_) => None,
            })
            .collect();
        let [_, label, iri] = tokens[..] else {
            continue;
        };
        let remaps = bound.iter().any(|&(l, i)| l == label && i != iri);
        let kept = (i == 0 && under_header) || cx.comments.has_comments(n) || remaps;
        if unused.contains(&n) && !kept {
            out.insert(n);
        } else {
            bound.push((label, iri));
        }
    }
    out
}

/// The document of node `n`, with its comments; a node under `# sparkles-fmt: ignore`
/// as written.
pub fn node(sx: &mut Sx<'_, '_>, n: NodeId) -> DocId {
    if sx.comments.ignored(n) {
        let doc = sx.verbatim(n);
        return wrap(sx, n, doc);
    }
    let doc = match sx.tree.kind(n) {
        K::ShaclcDoc => return document(sx, n),
        K::BaseDecl => base_decl(sx, n),
        K::PrefixDecl => prefix_decl(sx, n),
        K::ShaclcImports => imports(sx, n),
        K::ShaclcShape => shape(sx, n),
        K::ShaclcBody => body(sx, n),
        K::ShaclcNodeConstraint | K::ShaclcPropertyShape => constraint(sx, n),
        K::ShaclcShapeRef => shape_ref_constraint(sx, n),
        K::ShaclcPath | K::ShaclcCount | K::ShaclcLiteral => adjacent(sx, n),
        K::ShaclcOr => or_chain(sx, n),
        K::ShaclcAtom => atom(sx, n),
        K::ShaclcArray => array(sx, n),
        K::ShaclcArrayItem => adjacent(sx, n),
        _ => sx.verbatim(n),
    };
    wrap(sx, n, doc)
}

fn wrap(sx: &mut Sx<'_, '_>, n: NodeId, doc: DocId) -> DocId {
    let cx = &mut sx.cx;
    trivia::wrap(&mut cx.arena, cx.comments, n, doc)
}

/// The file header, the directives and the shapes, the comments at the end and one final
/// newline.
fn document(sx: &mut Sx<'_, '_>, n: NodeId) -> DocId {
    let mut parts = Vec::new();
    parts.extend(sx.header());
    // (a directive is the previous item printed, a dropped declaration had a blank line
    // before it that passes to the next item)
    let mut prev: Option<bool> = None;
    let mut carried = false;
    for c in sx.child_nodes(n) {
        if sx.dropped.contains(&c) {
            carried |= sx.comments.blank_before(c);
            continue;
        }
        let directive = is_directive(sx.tree.kind(c));
        if let Some(prev_directive) = prev {
            let blank = match prev_directive && directive {
                true => sx.comments.blank_before(c) || carried,
                false => true,
            };
            parts.push(match blank {
                true => sx.empty_line(),
                false => sx.hard_line(),
            });
        }
        carried = false;
        parts.push(node(sx, c));
        prev = Some(directive);
    }
    parts.extend(sx.dangling(n, prev.is_some()));
    parts.push(sx.hard_line());
    sx.concat(parts)
}

/// `BASE <iri>`
fn base_decl(sx: &mut Sx<'_, '_>, n: NodeId) -> DocId {
    directive(sx, n)
}

/// `PREFIX label: <iri>`
fn prefix_decl(sx: &mut Sx<'_, '_>, n: NodeId) -> DocId {
    directive(sx, n)
}

/// A `BASE` or `PREFIX` declaration: its tokens one space apart, the IRIs as written (a
/// declaration's IRI is never compacted).
fn directive(sx: &mut Sx<'_, '_>, n: NodeId) -> DocId {
    let mut docs = Vec::new();
    for e in sx.children(n) {
        if let Element::Token(t) = e {
            docs.push(sx.kw(t));
        }
    }
    sx.spaced(docs)
}

/// `IMPORTS iri`
fn imports(sx: &mut Sx<'_, '_>, n: NodeId) -> DocId {
    let mut docs = Vec::new();
    for e in sx.children(n) {
        if let Element::Token(t) = e {
            docs.push(match sx.tree.token_kind(t) {
                T::Word => sx.tok_as(t, "IMPORTS"),
                _ => sx.term(t),
            });
        }
    }
    sx.spaced(docs)
}

/// `shape iri -> class class {`, the body and `}`.
fn shape(sx: &mut Sx<'_, '_>, n: NodeId) -> DocId {
    let mut docs = Vec::new();
    for e in sx.children(n) {
        docs.push(match e {
            Element::Token(t) => match sx.tree.token_kind(t) {
                T::Word => {
                    let canonical = match sx.tree.token_text(t).eq_ignore_ascii_case("shapeClass") {
                        true => "shapeClass",
                        false => "shape",
                    };
                    sx.tok_as(t, canonical)
                }
                T::Arrow => sx.tok(t),
                _ => sx.term(t),
            },
            Element::Node(c) => node(sx, c),
        });
    }
    sx.spaced(docs)
}

/// `{`, the constraints one per line, `}`.
fn body(sx: &mut Sx<'_, '_>, n: NodeId) -> DocId {
    let (Some(open), Some(close)) = (sx.child_token(n, T::LBrace), sx.child_token(n, T::RBrace))
    else {
        return sx.verbatim(n);
    };
    let nodes = sx.child_nodes(n);
    let mut aligned = match sx.opts.align_values {
        true => align(sx, n, &nodes),
        false => vec![None; nodes.len()],
    };
    let items: Vec<(NodeId, DocId)> = nodes
        .into_iter()
        .zip(aligned.drain(..))
        .map(|(c, a)| (c, a.unwrap_or_else(|| node(sx, c))))
        .collect();
    let (open, close) = (sx.tok(open), sx.tok(close));
    sx.block(n, open, &items, close)
}

/// Whether `n` or a node inside it has comments of its own to print.
fn any_comments(sx: &Sx<'_, '_>, n: NodeId) -> bool {
    sx.comments.has_comments(n) || sx.tree.child_nodes(n).any(|c| any_comments(sx, c))
}

/// Whether property shape `c` can be a row of aligned columns: it is not ignored, has
/// no comment and no nested shape.
fn aligned_row(sx: &Sx<'_, '_>, c: NodeId) -> bool {
    sx.tree.kind(c) == K::ShaclcPropertyShape
        && !sx.comments.ignored(c)
        && !any_comments(sx, c)
        && !has_body(sx.tree, c)
}

/// `align-values`: runs of two or more consecutive property shapes of a body with no
/// blank line between them print their parts in columns. Each part but the last of its
/// row is followed by spaces up to the width of the widest part of its column, then one
/// space, and the ` .` follows the last part. A run stays as [`constraint`] prints it when
/// a part would not fit on one line, a row with the padding is wider than `line-width`, or
/// the run has fewer than two columns. Returns the aligned document of each item that is
/// in an aligned run.
fn align(sx: &mut Sx<'_, '_>, body: NodeId, items: &[NodeId]) -> Vec<Option<DocId>> {
    let mut out = vec![None; items.len()];
    // the indentation of the rows
    let depth = std::iter::successors(sx.tree.parent(body), |&p| sx.tree.parent(p))
        .filter(|&p| sx.tree.kind(p) == K::ShaclcBody)
        .count()
        + 1;
    let indent = depth * sx.opts.indent_width as usize;
    let mut i = 0;
    while i < items.len() {
        if !aligned_row(sx, items[i]) {
            i += 1;
            continue;
        }
        let mut j = i + 1;
        while j < items.len() && aligned_row(sx, items[j]) && !sx.comments.blank_before(items[j]) {
            j += 1;
        }
        if j - i >= 2
            && let Some(docs) = align_run(sx, &items[i..j], indent)
        {
            for (k, d) in docs.into_iter().enumerate() {
                out[i + k] = Some(d);
            }
        }
        i = j;
    }
    out
}

/// The aligned rows of one run, or `None` when it stays as it is.
fn align_run(sx: &mut Sx<'_, '_>, rows: &[NodeId], indent: usize) -> Option<Vec<DocId>> {
    use unicode_width::UnicodeWidthStr;
    // each part's document and flat width
    let mut cells: Vec<Vec<(DocId, usize)>> = Vec::new();
    for &r in rows {
        let mut row = Vec::new();
        for p in sx.child_nodes(r) {
            let d = node(sx, p);
            let printed = doc::print(
                &sx.arena,
                d,
                sx.tree.src,
                u16::MAX,
                sx.opts.indent_width,
                None,
            )
            .ok()?
            .text;
            if printed.contains(['\n', '\r']) {
                return None;
            }
            row.push((d, printed.width()));
        }
        cells.push(row);
    }
    if cells.iter().map(Vec::len).max()? < 2 {
        return None;
    }
    let mut widths: Vec<usize> = Vec::new();
    for row in &cells {
        for (j, &(_, w)) in row.iter().enumerate().take(row.len().saturating_sub(1)) {
            match widths.get_mut(j) {
                Some(c) => *c = (*c).max(w),
                None => widths.push(w),
            }
        }
    }
    // a row's width with the padding, its ` .` included
    let width = |row: &[(DocId, usize)]| -> usize {
        let Some((&(_, last), init)) = row.split_last() else {
            return 2;
        };
        indent + 2 + last + widths[..init.len()].iter().map(|w| w + 1).sum::<usize>()
    };
    if cells.iter().any(|r| width(r) > sx.opts.line_width as usize) {
        return None;
    }
    let mut out = Vec::new();
    for (&r, row) in rows.iter().zip(&cells) {
        let mut parts = Vec::new();
        for (j, &(d, w)) in row.iter().enumerate() {
            parts.push(d);
            if j + 1 < row.len() {
                parts.push(sx.text(&" ".repeat(widths[j] - w + 1)));
            }
        }
        if let Some(dot) = sx.child_token(r, T::Dot) {
            parts.push(sx.space());
            parts.push(sx.tok(dot));
        }
        out.push(sx.concat(parts));
    }
    Some(out)
}

/// A node constraint or a property shape: its parts in runs that wrap, and ` .`. The
/// comments above its first part are printed above the constraint, where the next run
/// reads them as the constraint's own (inside the group they would force it to break, and
/// the output would not format to itself).
fn constraint(sx: &mut Sx<'_, '_>, n: NodeId) -> DocId {
    let parts = sx.child_nodes(n);
    let above = parts.first().and_then(|&p| hoist(sx, p));
    let flow = runs(sx, &parts);
    let mut docs: Vec<DocId> = above.into_iter().collect();
    docs.push(flow);
    if let Some(dot) = sx.child_token(n, T::Dot) {
        docs.push(sx.space());
        docs.push(sx.tok(dot));
    }
    sx.concat(docs)
}

/// The detached and leading comments of `n`, and of its first operand when it is a `|`
/// chain, as lines to put above a constraint (and marked as printed, so the parts do not
/// print them again). `None` without any.
fn hoist(sx: &mut Sx<'_, '_>, n: NodeId) -> Option<DocId> {
    let mut chain = vec![n];
    let mut at = n;
    while sx.tree.kind(at) == K::ShaclcOr
        && let Some(first) = sx.tree.child_nodes(at).next()
    {
        chain.push(first);
        at = first;
    }
    let mut parts = Vec::new();
    for x in chain {
        let comments = sx.comments;
        for block in comments.detached_before(x) {
            let live: Vec<TokenId> = block
                .iter()
                .copied()
                .filter(|&c| !comments.copied(c))
                .collect();
            if live.is_empty() {
                continue;
            }
            parts.push(trivia::comment_lines(&mut sx.cx.arena, comments, &live));
            parts.push(sx.empty_line());
            for c in live {
                comments.mark_printed(c);
            }
        }
        for &c in comments.leading(x) {
            if comments.copied(c) {
                continue;
            }
            parts.push(trivia::comment(&mut sx.cx.arena, comments, c));
            parts.push(sx.hard_line());
            comments.mark_printed(c);
        }
    }
    (!parts.is_empty()).then(|| sx.concat(parts))
}

/// `@ex:Shape .`
fn shape_ref_constraint(sx: &mut Sx<'_, '_>, n: NodeId) -> DocId {
    let mut parts = Vec::new();
    for e in sx.children(n) {
        let Element::Token(t) = e else { continue };
        if sx.tree.token_kind(t) == T::Dot {
            parts.push(sx.space());
        }
        parts.push(sx.tok(t));
    }
    sx.concat(parts)
}

/// Whether node `n` holds a nested shape body to print as a block (through atoms and
/// chains only).
fn has_body(tree: &Tree<'_>, n: NodeId) -> bool {
    tree.child_nodes(n).any(|c| match tree.kind(c) {
        K::ShaclcBody => true,
        K::ShaclcAtom | K::ShaclcOr => has_body(tree, c),
        _ => false,
    })
}

/// The parts of a constraint: the runs between nested shapes are groups that stay on one
/// line while they fit, else keep their first part on the line and put every other part on
/// its own line one level deeper. A part with a nested shape follows the run before it
/// after a space.
fn runs(sx: &mut Sx<'_, '_>, parts: &[NodeId]) -> DocId {
    let mut out = Vec::new();
    let mut run: Vec<DocId> = Vec::new();
    // how the run being collected follows the part before it
    let mut run_follows = Follows::Nothing;
    for (i, &p) in parts.iter().enumerate() {
        // the first part of a run, or a part with a nested shape, follows the part
        // before it on its line, unless that part ends its line with a comment or this
        // one has comments above it: a comment printed there would read as another
        // part's (asked before the part is printed, which moves comments)
        let follows = match i {
            0 => Follows::Nothing,
            _ if ends_line(sx, parts[i - 1]) || leads(sx, p) => Follows::NewLine,
            _ => Follows::Space,
        };
        let d = node(sx, p);
        let body = has_body(sx.tree, p);
        if !body && !run.is_empty() {
            run.push(d);
            continue;
        }
        if body {
            flush(sx, &mut run, &mut out, run_follows);
            out.push(follows.then(sx, d));
        } else {
            run_follows = follows;
            run.push(d);
        }
    }
    flush(sx, &mut run, &mut out, run_follows);
    sx.concat(out)
}

/// How a part of a constraint follows the one before it.
#[derive(Clone, Copy)]
enum Follows {
    /// it is the first
    Nothing,
    /// after a space
    Space,
    /// on a line of its own one level deeper
    NewLine,
}

impl Follows {
    /// `d` after the part before it.
    fn then(self, sx: &mut Sx<'_, '_>, d: DocId) -> DocId {
        match self {
            Follows::Nothing => d,
            Follows::Space => {
                let space = sx.space();
                sx.concat([space, d])
            }
            Follows::NewLine => {
                let line = sx.hard_line();
                let both = sx.concat([line, d]);
                sx.indent(both)
            }
        }
    }
}

/// Whether `n`, or the first operand of a `|` chain `n`, has comments above it still to
/// print: it starts a line.
fn leads(sx: &Sx<'_, '_>, n: NodeId) -> bool {
    sx.has_leading(n)
        || (sx.tree.kind(n) == K::ShaclcOr
            && sx.tree.child_nodes(n).next().is_some_and(|f| leads(sx, f)))
}

/// Whether `n` has a trailing comment still to print, which ends its line.
fn ends_line(sx: &Sx<'_, '_>, n: NodeId) -> bool {
    sx.comments
        .trailing(n)
        .iter()
        .any(|&c| !sx.comments.copied(c))
}

fn flush(sx: &mut Sx<'_, '_>, run: &mut Vec<DocId>, out: &mut Vec<DocId>, follows: Follows) {
    if run.is_empty() {
        return;
    }
    let docs = std::mem::take(run);
    let flow = flow(sx, docs);
    out.push(follows.then(sx, flow));
}

/// `first` and then the rest, one space apart when they fit, else on lines of their own.
fn flow(sx: &mut Sx<'_, '_>, docs: Vec<DocId>) -> DocId {
    let Some((&first, rest)) = docs.split_first() else {
        return sx.nil();
    };
    if rest.is_empty() {
        return first;
    }
    let mut tail = Vec::new();
    for &d in rest {
        tail.push(sx.line());
        tail.push(d);
    }
    let tail = sx.concat(tail);
    let tail = sx.indent(tail);
    let all = sx.concat([first, tail]);
    sx.group(all)
}

/// `a|b|c`: the operands, the chain breaking before a `|` when it does not fit. The
/// comments above the first operand are printed above the chain (inside its group they
/// would force it to break, and the output would not format to itself).
fn or_chain(sx: &mut Sx<'_, '_>, n: NodeId) -> DocId {
    let block = has_body(sx.tree, n);
    let operands = sx.child_nodes(n);
    let above = operands.first().and_then(|&p| hoist(sx, p));
    let mut first = None;
    let mut tail = Vec::new();
    let mut previous: Option<NodeId> = None;
    for e in sx.children(n) {
        match e {
            Element::Node(c) => {
                let d = node(sx, c);
                match first {
                    None => first = Some(d),
                    Some(_) => tail.push(d),
                }
                previous = Some(c);
            }
            Element::Token(t) => {
                // a chain with a nested shape reads better as `} | ex:C`, and a comment
                // that ends the line before ends it here too
                match block {
                    true => match previous.is_some_and(|p| ends_line(sx, p)) {
                        true => tail.push(sx.hard_line()),
                        false => tail.push(sx.space()),
                    },
                    false => tail.push(sx.soft_line()),
                }
                tail.push(sx.tok(t));
                // a comment above the operand after the bar starts after a space
                let next = sx
                    .tree
                    .child_nodes(n)
                    .find(|&c| previous.is_some_and(|p| c > p));
                if block || next.is_some_and(|c| sx.has_leading(c)) {
                    tail.push(sx.space());
                }
            }
        }
    }
    let first = first.unwrap_or_else(|| sx.nil());
    let tail = sx.concat(tail);
    let chain = match block {
        // the nested shapes of the operands keep the indentation of the constraint
        true => sx.concat([first, tail]),
        false => {
            let tail = sx.indent(tail);
            let all = sx.concat([first, tail]);
            sx.group(all)
        }
    };
    match above {
        Some(a) => sx.concat([a, chain]),
        None => chain,
    }
}

/// `!`, then a parameter `name=value`, a type, a node kind, a shape reference or a nested
/// body, with no space between the parts.
fn atom(sx: &mut Sx<'_, '_>, n: NodeId) -> DocId {
    adjacent(sx, n)
}

/// The children of `n` with no space between them: tokens as terms, nodes printed.
fn adjacent(sx: &mut Sx<'_, '_>, n: NodeId) -> DocId {
    let mut parts = Vec::new();
    let mut prev: Option<T> = None;
    for e in sx.children(n) {
        match e {
            Element::Token(t) => {
                let kind = sx.tree.token_kind(t);
                parts.push(token(sx, t, prev));
                prev = Some(kind);
            }
            Element::Node(c) => {
                parts.push(node(sx, c));
                prev = None;
            }
        }
    }
    sx.concat(parts)
}

/// A token of a value, a path or an atom. `prev` is the token before it in the same node.
/// An IRI after `@` stays as written (it is not a prefixed name's place), a boolean is
/// lowercase, a string takes the quote style, an IRI compacts.
fn token(sx: &mut Sx<'_, '_>, t: TokenId, prev: Option<T>) -> DocId {
    let text = sx.tree.token_text(t);
    match sx.tree.token_kind(t) {
        T::IriRef if prev == Some(T::At) => sx.tok(t),
        T::Word if prev == Some(T::Eq) => {
            let lower = text.to_ascii_lowercase();
            match lower == "true" || lower == "false" {
                true => sx.tok_as(t, lower),
                false => sx.tok(t),
            }
        }
        T::IriRef | T::String1 | T::StringLong1 => sx.term(t),
        _ => sx.tok(t),
    }
}

/// `[a b]`, or one value per line when they do not fit.
fn array(sx: &mut Sx<'_, '_>, n: NodeId) -> DocId {
    let (Some(open), Some(close)) = (
        sx.child_token(n, T::LBracket),
        sx.child_token(n, T::RBracket),
    ) else {
        return sx.verbatim(n);
    };
    let items: Vec<DocId> = sx.child_nodes(n).into_iter().map(|c| node(sx, c)).collect();
    let (open, close) = (sx.tok(open), sx.tok(close));
    sx.delimited(n, open, &items, close, false)
}

/// Comments attach to the declarations, the shapes, the constraints and their parts.
pub struct ShaclcRules;

pub static RULES: ShaclcRules = ShaclcRules;

impl CommentRules for ShaclcRules {
    fn is_attachment(&self, kind: crate::syntax::NodeKind) -> bool {
        matches!(
            kind,
            K::BaseDecl
                | K::PrefixDecl
                | K::ShaclcImports
                | K::ShaclcShape
                | K::ShaclcNodeConstraint
                | K::ShaclcPropertyShape
                | K::ShaclcShapeRef
                | K::ShaclcOr
                | K::ShaclcAtom
                | K::ShaclcCount
                | K::ShaclcArrayItem
        )
    }

    fn is_container(&self, kind: crate::syntax::NodeKind) -> bool {
        matches!(kind, K::ShaclcBody | K::ShaclcArray)
    }

    fn is_separator(&self, kind: T) -> bool {
        kind == T::Dot
    }

    fn is_closer(&self, kind: T) -> bool {
        crate::sparql::parse::is_closer(kind)
    }
}
