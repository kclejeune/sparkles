//! Fluent SPARQL query builder — the equivalent of Jena's `jena-querybuilder`
//! (`SelectBuilder`, `AskBuilder`, `ConstructBuilder`, `DescribeBuilder`,
//! `UpdateBuilder`, `WhereBuilder`, `ExprFactory`).
//!
//! Builders are owned values (`fn x(self, ..) -> Self`) that chain and are `Clone`, so
//! a partially built query can serve as a template. `build()` renders SPARQL text and
//! validates it with the SPARQL parser; `Display` shows the text without validating;
//! `execute(&Dataset)` builds and runs the query.
//!
//! * **Terms** ([`IntoNode`]): strings are SPARQL term syntax (`"?x"`, `"<http://…>"`,
//!   `"foaf:name"`, `"a"`, `"\"chat\"@fr"`, `"42"`, or a property path such as
//!   `"foaf:knows+"` in predicate position). Data values should go through [`lit`],
//!   [`lit_lang`], [`lit_typed`], [`iri`] or native Rust / `oxrdf` values: those are
//!   always escaped, so user input can never break out of a literal or IRI.
//! * **Prefixes**: prefixed names are checked against the builder's declared prefixes
//!   when building. The prefixes in [`WELL_KNOWN_PREFIXES`] (rdf, rdfs, xsd, owl, dc,
//!   dcterms, foaf, skos) are declared automatically when used; any other undeclared
//!   prefix is an error.
//! * **Expressions** ([`IntoExpr`]): SPARQL expression strings (`"?age > 30"`) or the
//!   typed functions in [`expr`] (`expr::gt(var("age"), 30)`), rendered with the
//!   parentheses that operator precedence requires.
//! * **Parameters**: `set_var("?x", value)` replaces a variable everywhere in patterns,
//!   templates and expressions (Jena's `setVar`), which makes a builder a prepared
//!   query.
//!
//! ```
//! use sparkles::Dataset;
//! use sparkles::io::RdfFormat;
//! use sparkles::querybuilder::{SelectBuilder, expr, lit, var};
//!
//! let ds = Dataset::memory();
//! ds.load_str(
//!     r#"@prefix foaf: <http://xmlns.com/foaf/0.1/> .
//!        <http://ex/alice> foaf:name "Alice" ; foaf:age 34 .
//!        <http://ex/bob>   foaf:name "Bob"   ; foaf:age 25 ."#,
//!     RdfFormat::Turtle,
//! )?;
//!
//! let q = SelectBuilder::new()
//!     .select("?name")
//!     .where_("?p", "foaf:name", "?name")
//!     .where_("?p", "foaf:age", "?age")
//!     .filter(expr::gt(var("age"), 30))
//!     .order_by("?name");
//! assert_eq!(
//!     q.build()?,
//!     "PREFIX foaf: <http://xmlns.com/foaf/0.1/>\n\
//!      SELECT ?name\n\
//!      WHERE {\n  ?p foaf:name ?name .\n  ?p foaf:age ?age .\n  FILTER(?age > 30)\n}\n\
//!      ORDER BY ?name"
//! );
//! let rows = q.execute(&ds)?;
//! assert_eq!(rows.len(), 1);
//! assert_eq!(rows.iter().next().unwrap().get("name").unwrap().to_string(), "\"Alice\"");
//!
//! // A prepared query: bind ?name to untrusted input, safely escaped.
//! let by_name = SelectBuilder::new()
//!     .select("?age")
//!     .where_("?p", "foaf:name", "?name")
//!     .where_("?p", "foaf:age", "?age");
//! let bob = by_name.clone().set_var("?name", lit("Bob")).execute(&ds)?;
//! assert_eq!(bob.iter().next().unwrap().get("age").unwrap().to_string(),
//!            "\"25\"^^<http://www.w3.org/2001/XMLSchema#integer>");
//! # Ok::<(), sparkles::Error>(())
//! ```

pub mod expr;
mod pattern;
mod query;
mod render;
mod term;
mod update;

pub use expr::{Expr, IntoExpr};
pub use pattern::WhereBuilder;
pub use query::{AskBuilder, ConstructBuilder, DescribeBuilder, SelectBuilder};
pub use render::WELL_KNOWN_PREFIXES;
pub use term::{IntoNode, Node, iri, lit, lit_lang, lit_typed, node, triple_term, undef, var};
pub use update::{GraphTarget, UpdateBuilder};

/// `prefix`, `prefixes`, `base` and `set_var` (every builder).
macro_rules! common_methods {
    () => {
        /// Declares `PREFIX prefix: <iri>` (re-declaring a prefix replaces it).
        pub fn prefix(mut self, prefix: impl AsRef<str>, iri: impl AsRef<str>) -> Self {
            self.common.prefix(prefix.as_ref(), iri.as_ref());
            self
        }

        /// Declares several prefixes, e.g. from `Dataset::prefixes()`.
        pub fn prefixes<P: AsRef<str>, I: AsRef<str>>(
            mut self,
            prefixes: impl IntoIterator<Item = (P, I)>,
        ) -> Self {
            for (p, i) in prefixes {
                self.common.prefix(p.as_ref(), i.as_ref());
            }
            self
        }

        /// `BASE <iri>`
        pub fn base(mut self, iri: impl AsRef<str>) -> Self {
            let iri = iri.as_ref();
            let iri = iri
                .strip_prefix('<')
                .and_then(|i| i.strip_suffix('>'))
                .unwrap_or(iri);
            self.common.base = Some(iri.to_string());
            self
        }

        /// Replaces variable `v` by `value` wherever it is used as a value (patterns,
        /// templates, expressions, `VALUES` cells) — Jena's `setVar`. Projections and
        /// `AS` targets keep the variable.
        pub fn set_var(
            mut self,
            v: impl $crate::querybuilder::IntoNode,
            value: impl $crate::querybuilder::IntoNode,
        ) -> Self {
            self.common.set_var(v.into_node(), value.into_node());
            self
        }
    };
}
pub(crate) use common_methods;

/// Forwards the [`WhereBuilder`] methods to `self.where_mut()`.
macro_rules! where_methods {
    () => {
        fn map_where(
            mut self,
            f: impl FnOnce($crate::querybuilder::WhereBuilder) -> $crate::querybuilder::WhereBuilder,
        ) -> Self {
            let w = self.where_mut();
            *w = f(std::mem::take(w));
            self
        }

        /// Adds the triple pattern `s p o` to the WHERE clause (the predicate may be a
        /// property path such as `"foaf:knows+"`).
        pub fn where_(
            self,
            s: impl $crate::querybuilder::IntoNode,
            p: impl $crate::querybuilder::IntoNode,
            o: impl $crate::querybuilder::IntoNode,
        ) -> Self {
            self.map_where(|w| w.where_(s, p, o))
        }

        /// Appends all elements of a [`WhereBuilder`](crate::querybuilder::WhereBuilder).
        pub fn add_where(self, other: $crate::querybuilder::WhereBuilder) -> Self {
            self.map_where(|w| w.add_where(other))
        }

        /// A nested group `{ … }`.
        pub fn group(
            self,
            f: impl FnOnce($crate::querybuilder::WhereBuilder) -> $crate::querybuilder::WhereBuilder,
        ) -> Self {
            self.map_where(|w| w.group(f))
        }

        /// `OPTIONAL { … }`
        pub fn optional(
            self,
            f: impl FnOnce($crate::querybuilder::WhereBuilder) -> $crate::querybuilder::WhereBuilder,
        ) -> Self {
            self.map_where(|w| w.optional(f))
        }

        /// `{ … } UNION { … }`
        pub fn union(
            self,
            left: impl FnOnce($crate::querybuilder::WhereBuilder) -> $crate::querybuilder::WhereBuilder,
            right: impl FnOnce($crate::querybuilder::WhereBuilder) -> $crate::querybuilder::WhereBuilder,
        ) -> Self {
            self.map_where(|w| w.union(left, right))
        }

        /// `{ … } UNION { … } UNION …`
        pub fn union_of(
            self,
            branches: impl IntoIterator<Item = $crate::querybuilder::WhereBuilder>,
        ) -> Self {
            self.map_where(|w| w.union_of(branches))
        }

        /// `MINUS { … }`
        pub fn minus(
            self,
            f: impl FnOnce($crate::querybuilder::WhereBuilder) -> $crate::querybuilder::WhereBuilder,
        ) -> Self {
            self.map_where(|w| w.minus(f))
        }

        /// `GRAPH g { … }`
        pub fn graph(
            self,
            g: impl $crate::querybuilder::IntoNode,
            f: impl FnOnce($crate::querybuilder::WhereBuilder) -> $crate::querybuilder::WhereBuilder,
        ) -> Self {
            self.map_where(|w| w.graph(g, f))
        }

        /// `SERVICE <endpoint> { … }`
        pub fn service(
            self,
            endpoint: impl $crate::querybuilder::IntoNode,
            f: impl FnOnce($crate::querybuilder::WhereBuilder) -> $crate::querybuilder::WhereBuilder,
        ) -> Self {
            self.map_where(|w| w.service(endpoint, f))
        }

        /// `SERVICE SILENT <endpoint> { … }`
        pub fn service_silent(
            self,
            endpoint: impl $crate::querybuilder::IntoNode,
            f: impl FnOnce($crate::querybuilder::WhereBuilder) -> $crate::querybuilder::WhereBuilder,
        ) -> Self {
            self.map_where(|w| w.service_silent(endpoint, f))
        }

        /// `FILTER(e)`
        pub fn filter(self, e: impl $crate::querybuilder::IntoExpr) -> Self {
            self.map_where(|w| w.filter(e))
        }

        /// `FILTER EXISTS { … }`
        pub fn filter_exists(
            self,
            f: impl FnOnce($crate::querybuilder::WhereBuilder) -> $crate::querybuilder::WhereBuilder,
        ) -> Self {
            self.map_where(|w| w.filter_exists(f))
        }

        /// `FILTER NOT EXISTS { … }`
        pub fn filter_not_exists(
            self,
            f: impl FnOnce($crate::querybuilder::WhereBuilder) -> $crate::querybuilder::WhereBuilder,
        ) -> Self {
            self.map_where(|w| w.filter_not_exists(f))
        }

        /// `BIND(e AS ?v)`
        pub fn bind(
            self,
            e: impl $crate::querybuilder::IntoExpr,
            v: impl $crate::querybuilder::IntoNode,
        ) -> Self {
            self.map_where(|w| w.bind(e, v))
        }

        /// `VALUES ?v { … }`
        pub fn values<T: $crate::querybuilder::IntoNode>(
            self,
            v: impl $crate::querybuilder::IntoNode,
            vals: impl IntoIterator<Item = T>,
        ) -> Self {
            self.map_where(|w| w.values(v, vals))
        }

        /// `VALUES (?a ?b …) { (…) … }` (see [`WhereBuilder::values_rows`](crate::querybuilder::WhereBuilder::values_rows)).
        pub fn values_rows<V, R, T>(
            self,
            vars: impl IntoIterator<Item = V>,
            rows: impl IntoIterator<Item = R>,
        ) -> Self
        where
            V: $crate::querybuilder::IntoNode,
            R: IntoIterator<Item = T>,
            T: $crate::querybuilder::IntoNode,
        {
            self.map_where(|w| w.values_rows(vars, rows))
        }

        /// A sub-query `{ SELECT … }`.
        pub fn sub_select(self, q: $crate::querybuilder::SelectBuilder) -> Self {
            self.map_where(|w| w.sub_select(q))
        }
    };
}
pub(crate) use where_methods;

/// `order_by`, `order_by_desc`, `limit`, `offset`.
macro_rules! modifier_methods {
    () => {
        /// `ORDER BY e` (ascending; a variable or an expression).
        pub fn order_by(mut self, e: impl $crate::querybuilder::IntoExpr) -> Self {
            self.modifiers.order_by.push((e.into_expr(), false));
            self
        }

        /// `ORDER BY DESC(e)`
        pub fn order_by_desc(mut self, e: impl $crate::querybuilder::IntoExpr) -> Self {
            self.modifiers.order_by.push((e.into_expr(), true));
            self
        }

        /// `LIMIT n`
        pub fn limit(mut self, n: u64) -> Self {
            self.modifiers.limit = Some(n);
            self
        }

        /// `OFFSET n`
        pub fn offset(mut self, n: u64) -> Self {
            self.modifiers.offset = Some(n);
            self
        }
    };
}
pub(crate) use modifier_methods;
