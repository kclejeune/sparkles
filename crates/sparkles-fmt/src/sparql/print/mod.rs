//! SPARQL printing: one function per node kind, each building the node's document.
//! [`node`] dispatches to them and wraps the node's comments around the result. A kind
//! whose printer is not written yet prints its source range verbatim.

pub mod expr;
pub mod path;
pub mod pattern;
pub mod prologue;
pub mod query;
pub mod term;
pub mod triples;
pub mod update;
pub mod values;

use crate::doc::{DocArena, DocId, GroupId, Printed};
use crate::lex::TokenKind;
use crate::normalize::{self, PrefixScope};
use crate::sparql::keywords::Kw;
use crate::syntax::NodeKind;
use crate::tree::{Element, NodeId, TokenId, Tree};
use crate::trivia::{self, CommentRules, Comments};
use crate::{FormatError, Options, QuoteStyle};

/// What every node printer works with. Build it with [`Ctx::new`].
pub struct Ctx<'a, 's> {
    pub tree: &'a Tree<'s>,
    pub arena: DocArena<'a>,
    pub opts: &'a Options,
    pub comments: &'a Comments,
    /// the prefixes declared in the document, for IRI compaction and `rdf:type`
    pub scope: PrefixScope,
}

/// Helpers for the node printers. Build each child's document once with [`Ctx::node`]
/// (which adds its comments) and reuse the `DocId` when it appears in two branches of an
/// `if_break`.
impl<'a, 's> Ctx<'a, 's> {
    pub fn new(tree: &'a Tree<'s>, comments: &'a Comments, opts: &'a Options) -> Ctx<'a, 's> {
        Ctx {
            tree,
            arena: DocArena::new(&tree.tokens),
            opts,
            comments,
            scope: PrefixScope::from_tree(tree),
        }
    }

    // ------------------------------------------------------------------ nodes ------

    /// Node `n`'s document, with its comments.
    pub fn node(&mut self, n: NodeId) -> DocId {
        node(self, n)
    }

    /// The node exactly as written, comments inside it included. Comments of nodes
    /// inside it that sit outside its range (the trailing comment of its last element,
    /// say, when `n` itself takes no comments) go around it.
    pub fn verbatim(&mut self, n: NodeId) -> DocId {
        let r = self.tree.range(n);
        self.comments.mark_copied(self.tree, r.clone());
        let mut doc = self.arena.verbatim(r);
        // the nodes inside with comments left (outside the range), outer before inner;
        // the inner ones wrap first
        let mut with_comments = Vec::new();
        let mut inside: Vec<NodeId> = self.tree.child_nodes(n).collect();
        while let Some(d) = inside.pop() {
            if self.comments.has_comments(d) {
                with_comments.push(d);
            }
            inside.extend(self.tree.child_nodes(d));
        }
        for &d in with_comments.iter().rev() {
            doc = trivia::wrap(&mut self.arena, self.comments, d, doc);
        }
        for d in with_comments {
            for c in self.comments.own(d) {
                self.comments.mark_printed(c);
            }
        }
        doc
    }

    /// A child element: a node with [`Ctx::node`], a token with [`Ctx::term`].
    pub fn element(&mut self, e: Element) -> DocId {
        match e {
            Element::Node(c) => self.node(c),
            Element::Token(t) => self.term(t),
        }
    }

    /// Every child of `n`, one space between: `PREFIX ex: <…>`, `LIMIT 10`,
    /// `FROM NAMED <g>`, `LOAD SILENT <x> INTO GRAPH <g>`.
    pub fn words(&mut self, n: NodeId) -> DocId {
        let docs: Vec<DocId> = self
            .children(n)
            .into_iter()
            .map(|e| self.element(e))
            .collect();
        self.spaced(docs)
    }

    // ----------------------------------------------------------------- tokens ------

    /// A token as written.
    pub fn tok(&mut self, t: TokenId) -> DocId {
        self.arena.token(t, None)
    }

    /// A token printed as `printed` (a normalization's result).
    pub fn tok_as(&mut self, t: TokenId, printed: impl Into<Box<str>>) -> DocId {
        let printed: Box<str> = printed.into();
        let same = *printed == *self.tree.token_text(t);
        self.arena.token(t, (!same).then_some(printed))
    }

    /// A token, a keyword in the grammar's spelling (`select` → `SELECT`, `SAMETERM` →
    /// `sameTerm`, `TRUE` → `true`).
    pub fn kw(&mut self, t: TokenId) -> DocId {
        match self.tree.token_kind(t) {
            TokenKind::Kw(k) => self.tok_as(t, k.canonical()),
            _ => self.tok(t),
        }
    }

    /// A token with every normalization that depends on the token alone: a keyword in
    /// the grammar's spelling, a full IRI as a prefixed name when a prefix in scope covers
    /// it (`compact-iris`), a single-quoted string in double quotes (`quote-style`).
    /// Variables keep their sigil; language tags and everything else stay as written.
    /// Not for the IRI of a `PREFIX` or `BASE` (use [`Ctx::kw`]).
    pub fn term(&mut self, t: TokenId) -> DocId {
        let text = self.tree.token_text(t);
        let kind = self.tree.token_kind(t);
        let printed = match kind {
            TokenKind::Kw(k) => Some(k.canonical().to_string()),
            TokenKind::IriRef if self.opts.compact_iris => {
                normalize::compact_iri(text, &self.scope, t)
            }
            TokenKind::String1 | TokenKind::StringLong1
                if self.opts.quote_style == QuoteStyle::Double =>
            {
                normalize::requote(text, kind)
            }
            _ => None,
        };
        match printed {
            Some(p) => self.tok_as(t, p),
            None => self.tok(t),
        }
    }

    /// A verb token: `a` for `rdf:type` (`type-shorthand`), else [`Ctx::term`]. Only for a
    /// simple verb, never inside a longer path or in subject or object position.
    pub fn verb(&mut self, t: TokenId) -> DocId {
        if self.opts.type_shorthand && normalize::is_rdf_type(self.tree, t, &self.scope) {
            return self.tok_as(t, "a");
        }
        self.term(t)
    }

    /// A typed literal `lexical ^^ datatype` as its numeric or boolean shorthand when its
    /// lexical form is that token (`"1"^^xsd:integer` → `1`): the string token printed
    /// as the shorthand, and the caller prints neither `^^` nor the datatype. `None`
    /// when the literal stays as written.
    pub fn literal_shorthand(&mut self, lexical: TokenId, datatype: TokenId) -> Option<DocId> {
        let dt_text = self.tree.token_text(datatype);
        let dt: String = match self.tree.token_kind(datatype) {
            TokenKind::IriRef => dt_text.to_string(),
            TokenKind::PnameLn => {
                let (label, local) = dt_text.split_once(':')?;
                format!("{}{local}", self.scope.resolve(label, datatype)?)
            }
            _ => return None,
        };
        let lexical_text = self.tree.token_text(lexical);
        let short = normalize::literal_shorthand(lexical_text, &dt)?.to_string();
        Some(self.tok_as(lexical, short))
    }

    // ------------------------------------------------------------- documents ------

    pub fn text(&mut self, s: &str) -> DocId {
        self.arena.text(s)
    }

    pub fn space(&mut self) -> DocId {
        self.arena.text(" ")
    }

    pub fn nil(&mut self) -> DocId {
        self.arena.nil()
    }

    pub fn line(&mut self) -> DocId {
        self.arena.line()
    }

    pub fn soft_line(&mut self) -> DocId {
        self.arena.soft_line()
    }

    pub fn hard_line(&mut self) -> DocId {
        self.arena.hard_line()
    }

    pub fn empty_line(&mut self) -> DocId {
        self.arena.empty_line()
    }

    pub fn concat(&mut self, ds: impl IntoIterator<Item = DocId>) -> DocId {
        self.arena.concat(ds)
    }

    pub fn indent(&mut self, d: DocId) -> DocId {
        self.arena.indent(d)
    }

    /// A group, when nothing refers to its id.
    pub fn group(&mut self, d: DocId) -> DocId {
        self.arena.group(d).0
    }

    pub fn group_with_id(&mut self, d: DocId) -> (DocId, GroupId) {
        self.arena.group(d)
    }

    pub fn if_break(&mut self, broken: DocId, flat: DocId, group: Option<GroupId>) -> DocId {
        self.arena.if_break(broken, flat, group)
    }

    /// `ds` with one space between each two.
    pub fn spaced(&mut self, ds: impl IntoIterator<Item = DocId>) -> DocId {
        let mut parts = Vec::new();
        for d in ds {
            if !parts.is_empty() {
                parts.push(self.space());
            }
            parts.push(d);
        }
        self.concat(parts)
    }

    // ------------------------------------------------------------------- tree ------

    /// `n`'s children (nodes and significant tokens), copied out of the tree.
    pub fn children(&self, n: NodeId) -> Vec<Element> {
        self.tree.children(n).to_vec()
    }

    /// `n`'s child nodes.
    pub fn child_nodes(&self, n: NodeId) -> Vec<NodeId> {
        self.tree.child_nodes(n).collect()
    }

    /// `n`'s first child token of `kind`.
    pub fn child_token(&self, n: NodeId, kind: TokenKind) -> Option<TokenId> {
        self.tree.children(n).iter().find_map(|e| match *e {
            Element::Token(t) if self.tree.token_kind(t) == kind => Some(t),
            _ => None,
        })
    }

    /// `n`'s child keyword `kw`.
    pub fn child_kw(&self, n: NodeId, kw: Kw) -> Option<TokenId> {
        self.child_token(n, TokenKind::Kw(kw))
    }

    // --------------------------------------------------------------- comments ------

    /// Whether `n` has comments of its own still to print (detached, leading,
    /// trailing or dangling): a list that must break, a `VALUES` that cannot stay
    /// inline.
    pub fn has_comments(&self, n: NodeId) -> bool {
        self.comments.has_comments(n)
    }

    /// Whether `n` will print a leading comment, so it must start its own line.
    pub fn has_leading(&self, n: NodeId) -> bool {
        let live = |c: &TokenId| !self.comments.copied(*c);
        self.comments.leading(n).iter().any(live)
            || self.comments.detached_before(n).iter().flatten().any(live)
    }

    /// Container `n`'s dangling comments, each on its own line: starts with a line
    /// break, so it goes inside the container's indentation right before the closing
    /// bracket. `after_items`: elements come before it, so a blank line before the
    /// first comment is kept.
    pub fn dangling(&mut self, n: NodeId, after_items: bool) -> Option<DocId> {
        let cs: Vec<TokenId> = self
            .comments
            .dangling(n)
            .iter()
            .copied()
            .filter(|&c| !self.comments.copied(c))
            .collect();
        let first = *cs.first()?;
        let lead = match after_items && self.comments.blank_before_comment(first) {
            true => self.empty_line(),
            false => self.hard_line(),
        };
        let lines = trivia::comment_lines(&mut self.arena, self.comments, &cs);
        Some(self.concat([lead, lines]))
    }

    /// The file header: its comments with their blank lines, then the line break (or
    /// blank line, as written) before the first node. `None` without a header.
    pub fn header(&mut self) -> Option<DocId> {
        let header = self.comments.header();
        if header.is_empty() {
            return None;
        }
        let lines = trivia::comment_lines(&mut self.arena, self.comments, header);
        let first = self.tree.next_significant(header[header.len() - 1]);
        let gap = match first.is_some_and(|t| self.comments.blank_before_token(t)) {
            true => self.empty_line(),
            false => self.hard_line(),
        };
        Some(self.concat([lines, gap]))
    }

    // ------------------------------------------------------- lists and blocks ------

    /// Items one per line; a blank line before an item where the source had one
    /// (collapsed to one), never before the first.
    pub fn stack(&mut self, items: &[(NodeId, DocId)]) -> DocId {
        let mut parts = Vec::with_capacity(items.len() * 2);
        for (i, &(n, d)) in items.iter().enumerate() {
            if i > 0 {
                parts.push(match self.comments.blank_before(n) {
                    true => self.empty_line(),
                    false => self.hard_line(),
                });
            }
            parts.push(d);
        }
        self.concat(parts)
    }

    /// The child nodes of `n` one per line, as [`Ctx::stack`].
    pub fn lines(&mut self, items: &[NodeId]) -> DocId {
        let docs: Vec<(NodeId, DocId)> = items.iter().map(|&n| (n, self.node(n))).collect();
        self.stack(&docs)
    }

    /// The child nodes of `n`, each with the `sep` token that follows it (`,` in an
    /// argument list), ready for [`Ctx::delimited`] or [`Ctx::stack`]. A trailing
    /// comment on an item prints after its separator.
    pub fn separated(&mut self, n: NodeId, sep: TokenKind) -> Vec<(NodeId, DocId)> {
        let children = self.children(n);
        let mut out = Vec::new();
        for (i, e) in children.iter().enumerate() {
            let Element::Node(c) = *e else { continue };
            let mut d = self.node(c);
            if let Some(&Element::Token(t)) = children.get(i + 1)
                && self.tree.token_kind(t) == sep
            {
                let s = self.tok(t);
                d = self.concat([d, s]);
            }
            out.push((c, d));
        }
        out
    }

    /// An always expanded block: `open`, the items one per line at +1 indent (as
    /// [`Ctx::stack`]) with the container's dangling comments after them, then `close`
    /// on its own line. Empty, without comments: `open` and `close` together (`{}`).
    pub fn block(
        &mut self,
        container: NodeId,
        open: DocId,
        items: &[(NodeId, DocId)],
        close: DocId,
    ) -> DocId {
        let dangling = self.dangling(container, !items.is_empty());
        if items.is_empty() && dangling.is_none() {
            return self.concat([open, close]);
        }
        let mut inner = Vec::new();
        if !items.is_empty() {
            inner.push(self.hard_line());
            inner.push(self.stack(items));
        }
        inner.extend(dangling);
        let inner = self.concat(inner);
        let inner = self.indent(inner);
        let hl = self.hard_line();
        self.concat([open, inner, hl, close])
    }

    /// A group that stays on one line when it fits: `open`, the items (separators
    /// included) one space apart, `close`; broken, the items go one per line at +1
    /// indent. `pad` puts spaces inside the brackets when flat (`[ p o ]`, `{ a b }`)
    /// instead of none (`(a, b)`). Dangling comments go before `close` and break it.
    pub fn delimited(
        &mut self,
        container: NodeId,
        open: DocId,
        items: &[DocId],
        close: DocId,
        pad: bool,
    ) -> DocId {
        let dangling = self.dangling(container, !items.is_empty());
        if items.is_empty() && dangling.is_none() {
            return self.concat([open, close]);
        }
        let mut edge = || match pad {
            true => self.arena.line(),
            false => self.arena.soft_line(),
        };
        let mut inner = vec![edge()];
        for (i, &d) in items.iter().enumerate() {
            if i > 0 {
                inner.push(self.arena.line());
            }
            inner.push(d);
        }
        inner.extend(dangling);
        let inner = self.concat(inner);
        let inner = self.indent(inner);
        let end = match pad {
            true => self.line(),
            false => self.soft_line(),
        };
        let all = self.concat([open, inner, end, close]);
        self.group(all)
    }
}

/// Print a whole tree.
pub fn print(tree: &Tree<'_>, comments: &Comments, opts: &Options) -> Result<Printed, FormatError> {
    let mut cx = Ctx::new(tree, comments, opts);
    let root = node(&mut cx, tree.root());
    crate::doc::print(
        &cx.arena,
        root,
        tree.src,
        opts.line_width,
        opts.indent_width,
        opts.deadline,
    )
}

/// The document of node `n`, with its comments.
pub fn node(cx: &mut Ctx<'_, '_>, n: NodeId) -> DocId {
    use NodeKind as K;
    let doc = if cx.comments.ignored(n) {
        cx.verbatim(n)
    } else {
        match cx.tree.kind(n) {
            // units and prologue
            K::QueryUnit => query::query_unit(cx, n),
            K::UpdateUnit => update::update_unit(cx, n),
            K::Prologue => prologue::prologue(cx, n),
            K::BaseDecl => prologue::base_decl(cx, n),
            K::PrefixDecl => prologue::prefix_decl(cx, n),
            K::VersionDecl => prologue::version_decl(cx, n),
            // queries
            K::SelectQuery => query::select_query(cx, n),
            K::ConstructQuery => query::construct_query(cx, n),
            K::DescribeQuery => query::describe_query(cx, n),
            K::AskQuery => query::ask_query(cx, n),
            K::SubSelect => query::sub_select(cx, n),
            K::SelectClause => query::select_clause(cx, n),
            K::ProjectionItem => query::projection_item(cx, n),
            K::ConstructTemplate => query::construct_template(cx, n),
            K::DescribeClause => query::describe_clause(cx, n),
            K::DatasetClause => query::dataset_clause(cx, n),
            K::WhereClause => query::where_clause(cx, n),
            K::GroupBy => query::group_by(cx, n),
            K::GroupCondition => query::group_condition(cx, n),
            K::Having => query::having(cx, n),
            K::OrderBy => query::order_by(cx, n),
            K::OrderCondition => query::order_condition(cx, n),
            K::Limit => query::limit(cx, n),
            K::Offset => query::offset(cx, n),
            K::ValuesClause => values::values_clause(cx, n),
            // updates
            K::LoadOp => update::load_op(cx, n),
            K::ClearOp => update::clear_op(cx, n),
            K::DropOp => update::drop_op(cx, n),
            K::CreateOp => update::create_op(cx, n),
            K::AddOp => update::add_op(cx, n),
            K::MoveOp => update::move_op(cx, n),
            K::CopyOp => update::copy_op(cx, n),
            K::InsertDataOp => update::insert_data_op(cx, n),
            K::DeleteDataOp => update::delete_data_op(cx, n),
            K::DeleteWhereOp => update::delete_where_op(cx, n),
            K::ModifyOp => update::modify_op(cx, n),
            K::WithClause => update::with_clause(cx, n),
            K::DeleteClause => update::delete_clause(cx, n),
            K::InsertClause => update::insert_clause(cx, n),
            K::UsingClause => update::using_clause(cx, n),
            K::QuadPattern => update::quad_pattern(cx, n),
            K::QuadsGraph => update::quads_graph(cx, n),
            // group graph patterns
            K::GroupGraphPattern => pattern::group_graph_pattern(cx, n),
            K::Optional => pattern::optional(cx, n),
            K::Minus => pattern::minus(cx, n),
            K::Union => pattern::union(cx, n),
            K::UnionBranch => pattern::union_branch(cx, n),
            K::GraphPattern => pattern::graph_pattern(cx, n),
            K::Service => pattern::service(cx, n),
            K::Filter => pattern::filter(cx, n),
            K::Bind => pattern::bind(cx, n),
            K::InlineValues => values::inline_values(cx, n),
            K::ValuesRow => values::values_row(cx, n),
            K::DataValue => values::data_value(cx, n),
            // triples and terms
            K::TriplesStmt => triples::triples_stmt(cx, n),
            K::PropertyListEntry => triples::property_list_entry(cx, n),
            K::Object => triples::object(cx, n),
            K::BNodePropertyList => triples::bnode_property_list(cx, n),
            K::Collection => triples::collection(cx, n),
            K::CollectionItem => triples::collection_item(cx, n),
            K::ReifiedTriple => triples::reified_triple(cx, n),
            K::TripleTerm => triples::triple_term(cx, n),
            K::Reifier => triples::reifier(cx, n),
            K::AnnotationBlock => triples::annotation_block(cx, n),
            K::Literal => term::literal(cx, n),
            // paths
            K::PathAlternative => path::path_alternative(cx, n),
            K::PathSequence => path::path_sequence(cx, n),
            K::PathElt => path::path_elt(cx, n),
            K::PathInverse => path::path_inverse(cx, n),
            K::PathNegated => path::path_negated(cx, n),
            K::PathBracketed => path::path_bracketed(cx, n),
            // expressions
            K::OrChain => expr::or_chain(cx, n),
            K::AndChain => expr::and_chain(cx, n),
            K::ChainOperand => expr::chain_operand(cx, n),
            K::Binary => expr::binary(cx, n),
            K::Unary => expr::unary(cx, n),
            K::Bracketed => expr::bracketed(cx, n),
            K::Call => expr::call(cx, n),
            K::ArgList => expr::arg_list(cx, n),
            K::Arg => expr::arg(cx, n),
            K::Aggregate => expr::aggregate(cx, n),
            K::InList => expr::in_list(cx, n),
            K::Exists => expr::exists(cx, n),
            K::NotExists => expr::not_exists(cx, n),
            K::Opaque => cx.verbatim(n),
        }
    };
    crate::trivia::wrap(&mut cx.arena, cx.comments, n, doc)
}

/// SPARQL's comment attachment rules.
pub struct SparqlRules;

pub static RULES: SparqlRules = SparqlRules;

impl CommentRules for SparqlRules {
    fn is_attachment(&self, kind: NodeKind) -> bool {
        use NodeKind as K;
        matches!(
            kind,
            K::BaseDecl
                | K::PrefixDecl
                | K::VersionDecl
                | K::SelectClause
                | K::ConstructTemplate
                | K::DescribeClause
                | K::DatasetClause
                | K::WhereClause
                | K::GroupBy
                | K::Having
                | K::OrderBy
                | K::Limit
                | K::Offset
                | K::ValuesClause
                | K::LoadOp
                | K::ClearOp
                | K::DropOp
                | K::CreateOp
                | K::AddOp
                | K::MoveOp
                | K::CopyOp
                | K::InsertDataOp
                | K::DeleteDataOp
                | K::DeleteWhereOp
                | K::ModifyOp
                | K::WithClause
                | K::DeleteClause
                | K::InsertClause
                | K::UsingClause
                | K::TriplesStmt
                | K::Optional
                | K::Minus
                | K::UnionBranch
                | K::GraphPattern
                | K::Service
                | K::Filter
                | K::Bind
                | K::InlineValues
                | K::SubSelect
                | K::QuadsGraph
                | K::PropertyListEntry
                | K::Object
                | K::CollectionItem
                | K::ProjectionItem
                | K::ValuesRow
                | K::DataValue
                | K::GroupCondition
                | K::OrderCondition
                | K::Arg
                | K::ChainOperand
        )
    }

    fn is_container(&self, kind: NodeKind) -> bool {
        use NodeKind as K;
        matches!(
            kind,
            K::GroupGraphPattern
                | K::ConstructTemplate
                | K::QuadPattern
                | K::BNodePropertyList
                | K::Collection
                | K::AnnotationBlock
                | K::ArgList
                | K::InlineValues
                | K::ValuesClause
                | K::ValuesRow
                | K::Bracketed
        )
    }

    fn is_separator(&self, kind: TokenKind) -> bool {
        use TokenKind as T;
        matches!(kind, T::Comma | T::Semicolon | T::Dot | T::AndAnd | T::OrOr)
    }

    fn is_closer(&self, kind: TokenKind) -> bool {
        crate::sparql::parse::is_closer(kind)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::trivia::test_tree::tree;

    /// Attach `tree`'s comments, print the group graph pattern `NodeId(1)` with
    /// [`Ctx::block`] (its elements through [`Ctx::node`]), and return the text and the
    /// `comment-moved` warnings.
    fn group(src: &str, shape: &str) -> (String, usize) {
        let t = tree(src, shape);
        let comments = Comments::attach(&t, &RULES);
        let opts = Options::default();
        let mut cx = Ctx::new(&t, &comments, &opts);
        let g = NodeId(1);
        let items: Vec<(NodeId, DocId)> = cx
            .child_nodes(g)
            .into_iter()
            .map(|n| (n, cx.node(n)))
            .collect();
        let open = cx.tok(t.first_token(g).unwrap());
        let close = cx.tok(t.last_token(g).unwrap());
        let body = cx.block(g, open, &items, close);
        let dangling = cx.dangling(t.root(), true);
        let hl = cx.hard_line();
        let root = cx.concat([body].into_iter().chain(dangling).chain([hl]));
        let p = crate::doc::print(&cx.arena, root, t.src, 100, 2, None).unwrap();
        (p.text, comments.warnings().len())
    }

    const STMTS: &str = "(Q (G _ (S _ (E _ (O _)) _) (S _ (E _ (O _)) _) _))";

    #[test]
    fn blocks_print_every_kind_of_comment() {
        let src = "{ # lead\n  ?s   ?p ?o . # trailing\n\n\n  # detached\n\n  # leading\n ?a ?b ?c .\n\n # dangling\n}\n\n# end";
        let (out, moved) = group(src, STMTS);
        assert_eq!(
            out,
            "{\n  # lead\n  ?s ?p ?o . # trailing\n\n  # detached\n\n  # leading\n  ?a ?b ?c .\n\n  # dangling\n}\n\n# end\n"
        );
        assert_eq!(moved, 0);
        // blank lines just inside the brackets go
        let src = "{\n\n  ?s ?p ?o .\n\n  ?a ?b ?c .\n\n}";
        assert_eq!(group(src, STMTS).0, "{\n  ?s ?p ?o .\n\n  ?a ?b ?c .\n}\n");
        assert_eq!(group("{  }", "(Q (G _ _))").0, "{}\n");
        assert_eq!(group("{ # c\n}", "(Q (G _ _))").0, "{\n  # c\n}\n");
    }

    #[test]
    fn verbatim_nodes_print_their_comments_once() {
        // a displaced comment inside a node printed as written stays where it was
        let (out, moved) = group("{ FILTER # c\n (?x) }", "(Q (G _ (F (X _ _ _ _)) _))");
        assert_eq!(out, "{\n  FILTER # c\n (?x)\n}\n");
        assert_eq!(moved, 0);
        // a trailing comment of the last branch of a union printed as written
        let shape = "(Q (G _ (U (R (G _ (S _ (E _ (O _))) _)) _ (R (X _ _ _ _ _))) _))";
        let (out, _) = group("{ {?a ?b ?c} UNION {?d ?e ?f} # t\n}", shape);
        assert_eq!(out, "{\n  {\n    ?a ?b ?c .\n  } UNION {?d ?e ?f} # t\n}\n");
    }

    #[test]
    fn helpers_for_lists() {
        let src = "{ ?s ?p ?a, ?b, # c\n ?d }";
        let t = tree(src, "(Q (G _ (S _ (E _ (O _ _) (O _ _) (O _))) _))");
        let comments = Comments::attach(&t, &RULES);
        let opts = Options::default();
        let mut cx = Ctx::new(&t, &comments, &opts);
        // objects, their commas inside them, through `delimited` (a group that broke
        // because of the trailing comment)
        let entry = NodeId(3);
        let objects: Vec<DocId> = cx
            .child_nodes(entry)
            .into_iter()
            .map(|o| {
                let parts: Vec<DocId> = cx.children(o).into_iter().map(|e| cx.element(e)).collect();
                let d = cx.concat(parts);
                trivia::wrap(&mut cx.arena, cx.comments, o, d)
            })
            .collect();
        let open = cx.text("(");
        let close = cx.text(")");
        let list = cx.delimited(entry, open, &objects, close, false);
        let p = crate::doc::print(&cx.arena, list, t.src, 100, 2, None).unwrap();
        assert_eq!(p.text, "(\n  ?a,\n  ?b, # c\n  ?d\n)");

        let none = Comments::default();
        let mut cx = Ctx::new(&t, &none, &opts);
        let objects: Vec<DocId> = cx
            .child_nodes(entry)
            .into_iter()
            .map(|o| cx.words(o))
            .collect();
        let open = cx.text("[");
        let close = cx.text("]");
        let list = cx.delimited(entry, open, &objects, close, true);
        let p = crate::doc::print(&cx.arena, list, t.src, 100, 2, None).unwrap();
        assert_eq!(p.text, "[ ?a , ?b , ?d ]");
    }

    #[test]
    fn moved_comments_warn_and_the_cursor_follows() {
        let src = "PREFIX # c\n ex: <http://e/>\nASK {}";
        let opts = Options {
            // on `ex:`
            cursor: Some(13),
            ..Options::default()
        };
        let f = crate::format(src, crate::Language::Sparql, &opts).unwrap();
        assert_eq!(f.text, "# c\nPREFIX ex: <http://e/>\n\nASK {}\n");
        assert_eq!(f.cursor, Some(12));
        let w: Vec<_> = f
            .warnings
            .iter()
            .map(|w| (w.code, w.line, w.column))
            .collect();
        assert_eq!(w, [("comment-moved", 1, 8)]);

        // a comment that stays where it was gives no warning
        let f = crate::format(
            "PREFIX ex: <http://e/> # c\nASK {}\n",
            crate::Language::Sparql,
            &Options::default(),
        )
        .unwrap();
        assert!(f.warnings.is_empty());
    }

    #[test]
    fn keywords_print_in_the_grammar_spelling() {
        let src = "select ?x";
        let mut tokens = crate::lex::lex(src, crate::lex::LexMode::Sparql);
        tokens[0].kind = TokenKind::Kw(Kw::Select);
        let t = Tree {
            src,
            tokens,
            nodes: Vec::new(),
        };
        let comments = Comments::default();
        let opts = Options::default();
        let mut cx = Ctx::new(&t, &comments, &opts);
        let a = cx.kw(TokenId(0));
        let b = cx.kw(TokenId(2));
        let both = cx.spaced([a, b]);
        let p = crate::doc::print(&cx.arena, both, t.src, 100, 2, None).unwrap();
        assert_eq!(p.text, "SELECT ?x");
    }
}
