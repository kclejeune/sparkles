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

use crate::doc::{DocArena, DocId, Printed};
use crate::lex::TokenKind;
use crate::syntax::NodeKind;
use crate::tree::{NodeId, Tree};
use crate::trivia::{CommentRules, Comments};
use crate::{FormatError, Options};

/// What every node printer works with.
pub struct Ctx<'a, 's> {
    pub tree: &'a Tree<'s>,
    pub arena: DocArena<'a>,
    pub opts: &'a Options,
    pub comments: &'a Comments,
}

impl Ctx<'_, '_> {
    /// The node exactly as written.
    pub fn verbatim(&mut self, n: NodeId) -> DocId {
        let r = self.tree.range(n);
        self.arena.verbatim(r)
    }
}

/// Print a whole tree.
pub fn print(tree: &Tree<'_>, comments: &Comments, opts: &Options) -> Result<Printed, FormatError> {
    let mut cx = Ctx {
        tree,
        arena: DocArena::new(&tree.tokens),
        opts,
        comments,
    };
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
