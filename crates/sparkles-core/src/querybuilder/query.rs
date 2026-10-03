//! Query builders: SELECT, ASK, CONSTRUCT and DESCRIBE.

use std::fmt;

use oxrdf::Triple;

use super::expr::{Expr, IntoExpr};
use super::pattern::WhereBuilder;
use super::render::{Pos, Renderer};
use super::term::{IntoNode, Node};
use crate::dataset::{Dataset, Solutions};
use crate::{Error, Result};

/// Prologue and parameter bindings shared by every builder.
#[derive(Clone, Debug, Default)]
pub(crate) struct Common {
    pub(crate) base: Option<String>,
    pub(crate) prefixes: Vec<(String, String)>,
    pub(crate) bindings: Vec<(String, Node)>,
    /// Problems found while building (reported by `build()`).
    pub(crate) errors: Vec<String>,
}

impl Common {
    pub(crate) fn prefix(&mut self, prefix: &str, iri: &str) {
        let prefix = prefix.strip_suffix(':').unwrap_or(prefix);
        let valid = prefix.is_empty()
            || (prefix
                .chars()
                .next()
                .is_some_and(super::term::is_pn_chars_base)
                && !prefix.ends_with('.')
                && prefix
                    .chars()
                    .all(|c| super::term::is_pn_chars(c) || c == '.'));
        if !valid {
            self.errors.push(format!("invalid prefix name {prefix:?}"));
            return;
        }
        let iri = iri
            .strip_prefix('<')
            .and_then(|i| i.strip_suffix('>'))
            .unwrap_or(iri);
        match self.prefixes.iter_mut().find(|(p, _)| p == prefix) {
            Some(entry) => entry.1 = iri.to_string(),
            None => self.prefixes.push((prefix.to_string(), iri.to_string())),
        }
    }

    pub(crate) fn set_var(&mut self, v: Node, value: Node) {
        match v.var_name() {
            Some(name) => {
                let name = name.to_string();
                self.bindings.retain(|(n, _)| *n != name);
                self.bindings.push((name, value));
            }
            None => self
                .errors
                .push(format!("set_var: expected a variable, got {v}")),
        }
    }
}

/// `ORDER BY`, `LIMIT`, `OFFSET`.
#[derive(Clone, Debug, Default)]
pub(crate) struct Modifiers {
    pub(crate) order_by: Vec<(Expr, bool)>,
    pub(crate) limit: Option<u64>,
    pub(crate) offset: Option<u64>,
}

/// Checks the rendered text: collected builder errors first (`Error::Invalid`), then a
/// full parse (`Error::SparqlSyntax`).
pub(crate) fn finish(text: String, errors: Vec<String>, update: bool) -> Result<String> {
    if !errors.is_empty() {
        return Err(Error::invalid(format!(
            "query builder: {}",
            errors.join("; ")
        )));
    }
    if update {
        spargebra::SparqlParser::new().parse_update(&text)?;
    } else {
        crate::sparql::parse_query(&text, None, &[])?;
    }
    Ok(text)
}

// ------------------------------------------------------------------------ SELECT ----

/// Builds a `SELECT` query (Jena's `SelectBuilder`).
///
/// ```
/// use sparkles_core::querybuilder::{SelectBuilder, expr, var};
/// let q = SelectBuilder::new()
///     .prefix("ex", "http://example.org/")
///     .select("?dept")
///     .select_expr(expr::avg(var("salary")), "?avg")
///     .where_("?e", "ex:dept", "?dept")
///     .where_("?e", "ex:salary", "?salary")
///     .group_by("?dept")
///     .having(expr::gt(expr::avg(var("salary")), 1000))
///     .order_by_desc("?avg")
///     .limit(5);
/// assert_eq!(
///     q.build().unwrap(),
///     "PREFIX ex: <http://example.org/>\n\
///      SELECT ?dept (AVG(?salary) AS ?avg)\n\
///      WHERE {\n  ?e ex:dept ?dept .\n  ?e ex:salary ?salary .\n}\n\
///      GROUP BY ?dept\n\
///      HAVING (AVG(?salary) > 1000)\n\
///      ORDER BY DESC(?avg)\n\
///      LIMIT 5"
/// );
/// ```
#[derive(Clone, Debug, Default)]
pub struct SelectBuilder {
    pub(crate) common: Common,
    pub(crate) modifier: Option<&'static str>,
    /// `(None, ?v)` projects a variable, `(Some(e), ?v)` is `(e AS ?v)`.
    pub(crate) projection: Vec<(Option<Expr>, Node)>,
    pub(crate) from: Vec<Node>,
    pub(crate) from_named: Vec<Node>,
    pub(crate) where_: WhereBuilder,
    pub(crate) group_by: Vec<(Expr, Option<Node>)>,
    pub(crate) having: Vec<Expr>,
    pub(crate) modifiers: Modifiers,
}

impl SelectBuilder {
    pub fn new() -> SelectBuilder {
        SelectBuilder::default()
    }

    fn where_mut(&mut self) -> &mut WhereBuilder {
        &mut self.where_
    }

    crate::querybuilder::common_methods!();
    crate::querybuilder::where_methods!();
    crate::querybuilder::modifier_methods!();

    /// Adds a projected variable (`"?x"`, `var("x")`).
    pub fn select(mut self, v: impl IntoNode) -> Self {
        self.projection.push((None, v.into_node()));
        self
    }

    /// Adds several projected variables.
    pub fn select_vars<T: IntoNode>(mut self, vs: impl IntoIterator<Item = T>) -> Self {
        self.projection
            .extend(vs.into_iter().map(|v| (None, v.into_node())));
        self
    }

    /// Adds a projected expression `(e AS ?alias)`.
    pub fn select_expr(mut self, e: impl IntoExpr, alias: impl IntoNode) -> Self {
        self.projection
            .push((Some(e.into_expr()), alias.into_node()));
        self
    }

    /// `SELECT *` (clears any projection).
    pub fn select_all(mut self) -> Self {
        self.projection.clear();
        self
    }

    /// `SELECT DISTINCT`
    pub fn distinct(mut self) -> Self {
        self.modifier = Some("DISTINCT");
        self
    }

    /// `SELECT REDUCED`
    pub fn reduced(mut self) -> Self {
        self.modifier = Some("REDUCED");
        self
    }

    /// `FROM <g>`
    pub fn from(mut self, g: impl IntoNode) -> Self {
        self.from.push(g.into_node());
        self
    }

    /// `FROM NAMED <g>`
    pub fn from_named(mut self, g: impl IntoNode) -> Self {
        self.from_named.push(g.into_node());
        self
    }

    /// `GROUP BY e` (a variable or an expression).
    pub fn group_by(mut self, e: impl IntoExpr) -> Self {
        self.group_by.push((e.into_expr(), None));
        self
    }

    /// `GROUP BY (e AS ?v)`
    pub fn group_by_as(mut self, e: impl IntoExpr, v: impl IntoNode) -> Self {
        self.group_by.push((e.into_expr(), Some(v.into_node())));
        self
    }

    /// `HAVING (e)`
    pub fn having(mut self, e: impl IntoExpr) -> Self {
        self.having.push(e.into_expr());
        self
    }

    fn render(&self) -> (String, Vec<String>) {
        let mut r = Renderer::new(&self.common);
        let body = r.select(self, false);
        let pro = r.prologue(self.common.base.as_deref());
        (pro + &body, r.errors)
    }

    /// Renders and validates the query.
    pub fn build(&self) -> Result<String> {
        let (text, errors) = self.render();
        finish(text, errors, false)
    }

    /// Builds the query and runs it against `ds`.
    pub fn execute(&self, ds: &Dataset) -> Result<Solutions> {
        ds.select(&self.build()?)
    }
}

impl fmt::Display for SelectBuilder {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.render().0)
    }
}

// --------------------------------------------------------------------------- ASK ----

/// Builds an `ASK` query (Jena's `AskBuilder`).
#[derive(Clone, Debug, Default)]
pub struct AskBuilder {
    common: Common,
    from: Vec<Node>,
    from_named: Vec<Node>,
    where_: WhereBuilder,
}

impl AskBuilder {
    pub fn new() -> AskBuilder {
        AskBuilder::default()
    }

    fn where_mut(&mut self) -> &mut WhereBuilder {
        &mut self.where_
    }

    crate::querybuilder::common_methods!();
    crate::querybuilder::where_methods!();

    /// `FROM <g>`
    pub fn from(mut self, g: impl IntoNode) -> Self {
        self.from.push(g.into_node());
        self
    }

    /// `FROM NAMED <g>`
    pub fn from_named(mut self, g: impl IntoNode) -> Self {
        self.from_named.push(g.into_node());
        self
    }

    fn render(&self) -> (String, Vec<String>) {
        let mut r = Renderer::new(&self.common);
        let mut lines = vec!["ASK".to_string()];
        r.dataset_clause(&mut lines, &self.from, &self.from_named);
        let g = r.group(&self.where_);
        lines.push(format!("WHERE {g}"));
        let body = r.join_lines(lines);
        let pro = r.prologue(self.common.base.as_deref());
        (pro + &body, r.errors)
    }

    /// Renders and validates the query.
    pub fn build(&self) -> Result<String> {
        let (text, errors) = self.render();
        finish(text, errors, false)
    }

    /// Builds the query and runs it against `ds`.
    pub fn execute(&self, ds: &Dataset) -> Result<bool> {
        ds.ask(&self.build()?)
    }
}

impl fmt::Display for AskBuilder {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.render().0)
    }
}

// --------------------------------------------------------------------- CONSTRUCT ----

/// Builds a `CONSTRUCT` query (Jena's `ConstructBuilder`). Without template triples the
/// short form `CONSTRUCT WHERE { … }` is produced.
///
/// ```
/// use sparkles_core::querybuilder::ConstructBuilder;
/// let q = ConstructBuilder::new()
///     .prefix("ex", "http://example.org/")
///     .construct("?b", "ex:childOf", "?a")
///     .where_("?a", "ex:parentOf", "?b");
/// assert_eq!(
///     q.to_string(),
///     "PREFIX ex: <http://example.org/>\n\
///      CONSTRUCT {\n  ?b ex:childOf ?a .\n}\n\
///      WHERE {\n  ?a ex:parentOf ?b .\n}"
/// );
/// ```
#[derive(Clone, Debug, Default)]
pub struct ConstructBuilder {
    common: Common,
    template: Vec<(Node, Node, Node)>,
    from: Vec<Node>,
    from_named: Vec<Node>,
    where_: WhereBuilder,
    modifiers: Modifiers,
}

impl ConstructBuilder {
    pub fn new() -> ConstructBuilder {
        ConstructBuilder::default()
    }

    fn where_mut(&mut self) -> &mut WhereBuilder {
        &mut self.where_
    }

    crate::querybuilder::common_methods!();
    crate::querybuilder::where_methods!();
    crate::querybuilder::modifier_methods!();

    /// Adds the template triple `s p o`.
    pub fn construct(mut self, s: impl IntoNode, p: impl IntoNode, o: impl IntoNode) -> Self {
        self.template
            .push((s.into_node(), p.into_node(), o.into_node()));
        self
    }

    /// `FROM <g>`
    pub fn from(mut self, g: impl IntoNode) -> Self {
        self.from.push(g.into_node());
        self
    }

    /// `FROM NAMED <g>`
    pub fn from_named(mut self, g: impl IntoNode) -> Self {
        self.from_named.push(g.into_node());
        self
    }

    fn render(&self) -> (String, Vec<String>) {
        let mut r = Renderer::new(&self.common);
        let mut lines = Vec::new();
        if self.template.is_empty() {
            lines.push("CONSTRUCT".to_string());
        } else {
            let ts: Vec<String> = self
                .template
                .iter()
                .map(|(s, p, o)| format!("  {}\n", r.triple(s, p, o, Pos::TemplatePred)))
                .collect();
            lines.push(format!("CONSTRUCT {{\n{}}}", ts.concat()));
        }
        r.dataset_clause(&mut lines, &self.from, &self.from_named);
        let g = r.group(&self.where_);
        lines.push(format!("WHERE {g}"));
        r.modifiers(&mut lines, &self.modifiers);
        let mut body = r.join_lines(lines);
        if self.template.is_empty() && self.from.is_empty() && self.from_named.is_empty() {
            body = body.replacen("CONSTRUCT\nWHERE", "CONSTRUCT WHERE", 1);
        }
        let pro = r.prologue(self.common.base.as_deref());
        (pro + &body, r.errors)
    }

    /// Renders and validates the query.
    pub fn build(&self) -> Result<String> {
        let (text, errors) = self.render();
        finish(text, errors, false)
    }

    /// Builds the query and runs it against `ds`.
    pub fn execute(&self, ds: &Dataset) -> Result<Vec<Triple>> {
        ds.construct(&self.build()?)
    }
}

impl fmt::Display for ConstructBuilder {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.render().0)
    }
}

// ---------------------------------------------------------------------- DESCRIBE ----

/// Builds a `DESCRIBE` query (Jena's `DescribeBuilder`). Without any described resource
/// it renders `DESCRIBE *`.
#[derive(Clone, Debug, Default)]
pub struct DescribeBuilder {
    common: Common,
    resources: Vec<Node>,
    from: Vec<Node>,
    from_named: Vec<Node>,
    where_: WhereBuilder,
    modifiers: Modifiers,
}

impl DescribeBuilder {
    pub fn new() -> DescribeBuilder {
        DescribeBuilder::default()
    }

    fn where_mut(&mut self) -> &mut WhereBuilder {
        &mut self.where_
    }

    crate::querybuilder::common_methods!();
    crate::querybuilder::where_methods!();
    crate::querybuilder::modifier_methods!();

    /// Adds a resource or variable to describe.
    pub fn describe(mut self, r: impl IntoNode) -> Self {
        self.resources.push(r.into_node());
        self
    }

    /// `FROM <g>`
    pub fn from(mut self, g: impl IntoNode) -> Self {
        self.from.push(g.into_node());
        self
    }

    /// `FROM NAMED <g>`
    pub fn from_named(mut self, g: impl IntoNode) -> Self {
        self.from_named.push(g.into_node());
        self
    }

    fn render(&self) -> (String, Vec<String>) {
        let mut r = Renderer::new(&self.common);
        let mut head = String::from("DESCRIBE");
        if self.resources.is_empty() {
            head.push_str(" *");
        }
        for n in &self.resources {
            head.push(' ');
            let n = r.node(n, Pos::Term);
            head.push_str(&n);
        }
        let mut lines = vec![head];
        r.dataset_clause(&mut lines, &self.from, &self.from_named);
        if !self.where_.is_empty() || self.resources.is_empty() {
            let g = r.group(&self.where_);
            lines.push(format!("WHERE {g}"));
        }
        r.modifiers(&mut lines, &self.modifiers);
        let body = r.join_lines(lines);
        let pro = r.prologue(self.common.base.as_deref());
        (pro + &body, r.errors)
    }

    /// Renders and validates the query.
    pub fn build(&self) -> Result<String> {
        let (text, errors) = self.render();
        finish(text, errors, false)
    }

    /// Builds the query and runs it against `ds` (the description triples).
    pub fn execute(&self, ds: &Dataset) -> Result<Vec<Triple>> {
        ds.construct(&self.build()?)
    }
}

impl fmt::Display for DescribeBuilder {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.render().0)
    }
}
