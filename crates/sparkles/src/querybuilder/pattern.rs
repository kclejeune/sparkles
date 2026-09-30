//! Graph patterns: [`WhereBuilder`], shared by every query and update builder.

use super::expr::{self, Expr, IntoExpr};
use super::query::SelectBuilder;
use super::term::{IntoNode, Node};

/// A group graph pattern (`{ … }`) under construction — the equivalent of Jena's
/// `WhereBuilder`.
///
/// Every builder (`SelectBuilder`, `AskBuilder`, `ConstructBuilder`, `DescribeBuilder`,
/// `UpdateBuilder`) exposes the same methods and forwards them to its `WHERE` clause.
/// Nested groups are built with closures that receive a fresh `WhereBuilder`:
///
/// ```
/// use sparkles::querybuilder::{SelectBuilder, WhereBuilder, expr, var};
/// let q = SelectBuilder::new()
///     .prefix("foaf", "http://xmlns.com/foaf/0.1/")
///     .select("?name")
///     .where_("?p", "foaf:name", "?name")
///     .optional(|w| w.where_("?p", "foaf:age", "?age"))
///     .filter(expr::not(expr::bound(var("age"))));
/// assert!(q.build().is_ok());
/// ```
#[derive(Clone, Debug, Default)]
pub struct WhereBuilder {
    pub(crate) elements: Vec<Element>,
}

#[derive(Clone, Debug)]
pub(crate) enum Element {
    Triple(Node, Node, Node),
    Optional(WhereBuilder),
    Union(Vec<WhereBuilder>),
    Minus(WhereBuilder),
    Graph(Node, WhereBuilder),
    Service(Node, bool, WhereBuilder),
    Filter(Expr),
    Bind(Expr, Node),
    Values(Vec<Node>, Vec<Vec<Node>>),
    SubSelect(Box<SelectBuilder>),
    Group(WhereBuilder),
}

fn sub(f: impl FnOnce(WhereBuilder) -> WhereBuilder) -> WhereBuilder {
    f(WhereBuilder::new())
}

impl WhereBuilder {
    pub fn new() -> WhereBuilder {
        WhereBuilder::default()
    }

    /// Does the pattern have no elements?
    pub fn is_empty(&self) -> bool {
        self.elements.is_empty()
    }

    fn push(mut self, e: Element) -> Self {
        self.elements.push(e);
        self
    }

    /// Adds the triple pattern `s p o`. The predicate may be a property path
    /// (`"foaf:knows+"`, `"^ex:parent/ex:name"`).
    pub fn where_(self, s: impl IntoNode, p: impl IntoNode, o: impl IntoNode) -> Self {
        self.push(Element::Triple(s.into_node(), p.into_node(), o.into_node()))
    }

    /// Appends all elements of another pattern (not nested).
    pub fn add_where(mut self, other: WhereBuilder) -> Self {
        self.elements.extend(other.elements);
        self
    }

    /// A nested group `{ … }`.
    pub fn group(self, f: impl FnOnce(WhereBuilder) -> WhereBuilder) -> Self {
        self.push(Element::Group(sub(f)))
    }

    /// `OPTIONAL { … }`
    pub fn optional(self, f: impl FnOnce(WhereBuilder) -> WhereBuilder) -> Self {
        self.push(Element::Optional(sub(f)))
    }

    /// `{ … } UNION { … }`
    pub fn union(
        self,
        left: impl FnOnce(WhereBuilder) -> WhereBuilder,
        right: impl FnOnce(WhereBuilder) -> WhereBuilder,
    ) -> Self {
        self.push(Element::Union(vec![sub(left), sub(right)]))
    }

    /// `{ … } UNION { … } UNION …` over any number of branches.
    pub fn union_of(self, branches: impl IntoIterator<Item = WhereBuilder>) -> Self {
        self.push(Element::Union(branches.into_iter().collect()))
    }

    /// `MINUS { … }`
    pub fn minus(self, f: impl FnOnce(WhereBuilder) -> WhereBuilder) -> Self {
        self.push(Element::Minus(sub(f)))
    }

    /// `GRAPH g { … }` (`g` is an IRI or a variable).
    pub fn graph(self, g: impl IntoNode, f: impl FnOnce(WhereBuilder) -> WhereBuilder) -> Self {
        self.push(Element::Graph(g.into_node(), sub(f)))
    }

    /// `SERVICE <endpoint> { … }`
    pub fn service(
        self,
        endpoint: impl IntoNode,
        f: impl FnOnce(WhereBuilder) -> WhereBuilder,
    ) -> Self {
        self.push(Element::Service(endpoint.into_node(), false, sub(f)))
    }

    /// `SERVICE SILENT <endpoint> { … }`
    pub fn service_silent(
        self,
        endpoint: impl IntoNode,
        f: impl FnOnce(WhereBuilder) -> WhereBuilder,
    ) -> Self {
        self.push(Element::Service(endpoint.into_node(), true, sub(f)))
    }

    /// `FILTER(e)`
    pub fn filter(self, e: impl IntoExpr) -> Self {
        self.push(Element::Filter(e.into_expr()))
    }

    /// `FILTER EXISTS { … }`
    pub fn filter_exists(self, f: impl FnOnce(WhereBuilder) -> WhereBuilder) -> Self {
        self.push(Element::Filter(expr::exists(f)))
    }

    /// `FILTER NOT EXISTS { … }`
    pub fn filter_not_exists(self, f: impl FnOnce(WhereBuilder) -> WhereBuilder) -> Self {
        self.push(Element::Filter(expr::not_exists(f)))
    }

    /// `BIND(e AS ?v)`
    pub fn bind(self, e: impl IntoExpr, v: impl IntoNode) -> Self {
        self.push(Element::Bind(e.into_expr(), v.into_node()))
    }

    /// `VALUES ?v { a b … }` (use [`undef()`](super::undef) / `None` for `UNDEF`).
    pub fn values<T: IntoNode>(self, v: impl IntoNode, vals: impl IntoIterator<Item = T>) -> Self {
        let rows = vals.into_iter().map(|x| vec![x.into_node()]).collect();
        self.push(Element::Values(vec![v.into_node()], rows))
    }

    /// `VALUES (?a ?b …) { (x y …) … }`. Every row must have one value per variable;
    /// use [`undef()`](super::undef) (or `None`) for `UNDEF` and [`node`](super::node)
    /// to mix value types in a row.
    pub fn values_rows<V, R, T>(
        self,
        vars: impl IntoIterator<Item = V>,
        rows: impl IntoIterator<Item = R>,
    ) -> Self
    where
        V: IntoNode,
        R: IntoIterator<Item = T>,
        T: IntoNode,
    {
        let vars = vars.into_iter().map(IntoNode::into_node).collect();
        let rows = rows
            .into_iter()
            .map(|r| r.into_iter().map(IntoNode::into_node).collect())
            .collect();
        self.push(Element::Values(vars, rows))
    }

    /// A sub-query `{ SELECT … }`. Its prefixes are merged into the enclosing query.
    pub fn sub_select(self, q: SelectBuilder) -> Self {
        self.push(Element::SubSelect(Box::new(q)))
    }
}
