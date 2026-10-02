//! The query builder from Python: `SelectBuilder`, `AskBuilder`, `ConstructBuilder`,
//! `DescribeBuilder`, `UpdateBuilder` and `WhereBuilder` over `sparkles::querybuilder`.
//! Each method returns a new builder, so a partly built query can serve as a template.
//!
//! A `str` is SPARQL term syntax (`"?x"`, `"<http://…>"`, `"foaf:name"`, `"a"`, a property
//! path in predicate position), as in the Rust builder. Terms (`NamedNode`, `Literal`,
//! `Variable`, rdflib terms) and Python numbers and booleans are values, always escaped.
//! Expressions are SPARQL expression text, or a term or number.

// `from_named` adds `FROM NAMED`, as in the Rust builder
#![allow(clippy::wrong_self_convention)]

use crate::errors::EngineResult;
use crate::terms::{PyVariable, term_from_py};
use pyo3::exceptions::PyTypeError;
use pyo3::prelude::*;
use pyo3::types::{PyBool, PyDict, PyFloat, PyInt, PyString};
use sparkles::querybuilder as qb;
use sparkles::querybuilder::{Expr, IntoExpr, IntoNode, Node};

/// A node from Python: `str` is SPARQL term syntax; terms and numbers are values.
fn node(ob: &Bound<'_, PyAny>) -> PyResult<Node> {
    if let Ok(v) = ob.cast::<PyVariable>() {
        return Ok(qb::var(&v.get().name));
    }
    if let Ok(b) = ob.cast::<PyBool>() {
        return Ok(b.is_true().into_node());
    }
    if ob.cast::<PyInt>().is_ok() {
        return Ok(match ob.extract::<i128>() {
            Ok(i) => i.into_node(),
            Err(_) => qb::lit_typed(
                ob.str()?.to_str()?,
                qb::iri("http://www.w3.org/2001/XMLSchema#integer"),
            ),
        });
    }
    if let Ok(f) = ob.cast::<PyFloat>() {
        return Ok(f.value().into_node());
    }
    if let Ok(s) = ob.cast::<PyString>()
        && !is_rdflib(ob)
    {
        return Ok(s.to_str()?.into_node());
    }
    match term_from_py(ob) {
        Ok(t) => Ok(t.into_node()),
        Err(_) => Err(PyTypeError::new_err(format!(
            "expected a str of SPARQL term syntax, a term or a number, got {}",
            ob.get_type().name()?
        ))),
    }
}

/// Whether `ob` is an rdflib term (a `str` subclass that is a value, not syntax).
fn is_rdflib(ob: &Bound<'_, PyAny>) -> bool {
    ob.get_type()
        .getattr("__module__")
        .and_then(|m| m.extract::<String>())
        .is_ok_and(|m| m.starts_with("rdflib"))
}

/// An expression from Python: `str` is SPARQL expression text; otherwise a node.
fn expr(ob: &Bound<'_, PyAny>) -> PyResult<Expr> {
    if let Ok(s) = ob.cast::<PyString>()
        && !is_rdflib(ob)
    {
        return Ok(s.to_str()?.into_expr());
    }
    Ok(node(ob)?.into_expr())
}

fn nodes(ob: &Bound<'_, PyAny>) -> PyResult<Vec<Node>> {
    ob.try_iter()?.map(|x| node(&x?)).collect()
}

/// The target of `CLEAR`, `DROP`, `ADD`, `COPY` and `MOVE`: `"DEFAULT"`, `"NAMED"`,
/// `"ALL"`, `DefaultGraph()`, or a graph.
fn target(ob: &Bound<'_, PyAny>) -> PyResult<qb::GraphTarget> {
    if ob.cast::<crate::terms::PyDefaultGraph>().is_ok() {
        return Ok(qb::GraphTarget::Default);
    }
    if let Ok(s) = ob.cast::<PyString>()
        && !is_rdflib(ob)
    {
        match s.to_str()?.to_ascii_uppercase().as_str() {
            "DEFAULT" => return Ok(qb::GraphTarget::Default),
            "NAMED" => return Ok(qb::GraphTarget::Named),
            "ALL" => return Ok(qb::GraphTarget::All),
            _ => {}
        }
    }
    Ok(qb::GraphTarget::graph(node(ob)?))
}

/// Builds a SELECT query.
#[pyclass(
    frozen,
    module = "sparkles.querybuilder",
    name = "SelectBuilder",
    skip_from_py_object
)]
pub struct PySelectBuilder {
    b: qb::SelectBuilder,
}

#[pymethods]
impl PySelectBuilder {
    #[new]
    fn new() -> Self {
        Self {
            b: qb::SelectBuilder::new(),
        }
    }

    /// Declare a prefix.
    fn prefix(&self, prefix: &str, iri: &str) -> Self {
        Self {
            b: self.b.clone().prefix(prefix, iri),
        }
    }

    /// Declare prefixes from a `{prefix: iri}` mapping.
    fn prefixes(&self, prefixes: &Bound<'_, PyDict>) -> PyResult<Self> {
        let mut b = self.b.clone();
        for (k, v) in prefixes.iter() {
            b = b.prefix(k.extract::<String>()?, v.str()?.to_str()?);
        }
        Ok(Self { b })
    }

    /// Set the base IRI.
    fn base(&self, iri: &str) -> Self {
        Self {
            b: self.b.clone().base(iri),
        }
    }

    /// Replace a variable everywhere with a value (a prepared query).
    fn set_var(&self, var: &Bound<'_, PyAny>, value: &Bound<'_, PyAny>) -> PyResult<Self> {
        Ok(Self {
            b: self.b.clone().set_var(node(var)?, node(value)?),
        })
    }

    /// The SPARQL text, checked with the parser.
    fn build(&self, py: Python<'_>) -> PyResult<String> {
        self.b.build().py(py)
    }

    /// The SPARQL text, unchecked.
    fn __str__(&self) -> String {
        self.b.to_string()
    }

    fn __repr__(&self) -> String {
        format!("<{} {:?}>", "SelectBuilder", self.b.to_string())
    }

    /// Add the triple pattern `s p o`.
    fn where_(
        &self,
        s: &Bound<'_, PyAny>,
        p: &Bound<'_, PyAny>,
        o: &Bound<'_, PyAny>,
    ) -> PyResult<Self> {
        Ok(Self {
            b: self.b.clone().where_(node(s)?, node(p)?, node(o)?),
        })
    }

    /// Add the patterns of a `WhereBuilder` to this group.
    fn add_where(&self, w: PyRef<'_, PyWhereBuilder>) -> Self {
        Self {
            b: self.b.clone().add_where(w.b.clone()),
        }
    }

    /// `{ … }`
    fn group(&self, w: PyRef<'_, PyWhereBuilder>) -> Self {
        let w = w.b.clone();
        Self {
            b: self.b.clone().group(move |_| w),
        }
    }

    /// `OPTIONAL { … }`
    fn optional(&self, w: PyRef<'_, PyWhereBuilder>) -> Self {
        let w = w.b.clone();
        Self {
            b: self.b.clone().optional(move |_| w),
        }
    }

    /// `{ … } UNION { … } …`
    #[pyo3(signature = (*branches))]
    fn union(&self, branches: Vec<PyRef<'_, PyWhereBuilder>>) -> Self {
        let bs: Vec<qb::WhereBuilder> = branches.iter().map(|w| w.b.clone()).collect();
        Self {
            b: self.b.clone().union_of(bs),
        }
    }

    /// `MINUS { … }`
    fn minus(&self, w: PyRef<'_, PyWhereBuilder>) -> Self {
        let w = w.b.clone();
        Self {
            b: self.b.clone().minus(move |_| w),
        }
    }

    /// `GRAPH g { … }`
    fn graph(&self, g: &Bound<'_, PyAny>, w: PyRef<'_, PyWhereBuilder>) -> PyResult<Self> {
        let w = w.b.clone();
        Ok(Self {
            b: self.b.clone().graph(node(g)?, move |_| w),
        })
    }

    /// `SERVICE <endpoint> { … }` (`SERVICE SILENT` with `silent`)
    #[pyo3(signature = (endpoint, w, *, silent = false))]
    fn service(
        &self,
        endpoint: &Bound<'_, PyAny>,
        w: PyRef<'_, PyWhereBuilder>,
        silent: bool,
    ) -> PyResult<Self> {
        let w = w.b.clone();
        let e = node(endpoint)?;
        Ok(Self {
            b: if silent {
                self.b.clone().service_silent(e, move |_| w)
            } else {
                self.b.clone().service(e, move |_| w)
            },
        })
    }

    /// `FILTER(expr)`
    fn filter(&self, e: &Bound<'_, PyAny>) -> PyResult<Self> {
        Ok(Self {
            b: self.b.clone().filter(expr(e)?),
        })
    }

    /// `FILTER EXISTS { … }`
    fn filter_exists(&self, w: PyRef<'_, PyWhereBuilder>) -> Self {
        let w = w.b.clone();
        Self {
            b: self.b.clone().filter_exists(move |_| w),
        }
    }

    /// `FILTER NOT EXISTS { … }`
    fn filter_not_exists(&self, w: PyRef<'_, PyWhereBuilder>) -> Self {
        let w = w.b.clone();
        Self {
            b: self.b.clone().filter_not_exists(move |_| w),
        }
    }

    /// `BIND(expr AS ?var)`
    fn bind(&self, e: &Bound<'_, PyAny>, var: &Bound<'_, PyAny>) -> PyResult<Self> {
        Ok(Self {
            b: self.b.clone().bind(expr(e)?, node(var)?),
        })
    }

    /// `VALUES ?var { … }`
    fn values(&self, var: &Bound<'_, PyAny>, values: &Bound<'_, PyAny>) -> PyResult<Self> {
        Ok(Self {
            b: self.b.clone().values(node(var)?, nodes(values)?),
        })
    }

    /// `VALUES (?a ?b …) { (…) … }`; `None` in a row is `UNDEF`.
    fn values_rows(&self, vars: &Bound<'_, PyAny>, rows: &Bound<'_, PyAny>) -> PyResult<Self> {
        let vars = nodes(vars)?;
        let rows = rows
            .try_iter()?
            .map(|r| {
                r?.try_iter()?
                    .map(|x| {
                        let x = x?;
                        if x.is_none() {
                            Ok(qb::undef())
                        } else {
                            node(&x)
                        }
                    })
                    .collect::<PyResult<Vec<Node>>>()
            })
            .collect::<PyResult<Vec<_>>>()?;
        Ok(Self {
            b: self.b.clone().values_rows(vars, rows),
        })
    }

    /// A sub-query `{ SELECT … }`.
    fn sub_select(&self, q: PyRef<'_, PySelectBuilder>) -> Self {
        Self {
            b: self.b.clone().sub_select(q.b.clone()),
        }
    }

    /// `ORDER BY expr`
    fn order_by(&self, e: &Bound<'_, PyAny>) -> PyResult<Self> {
        Ok(Self {
            b: self.b.clone().order_by(expr(e)?),
        })
    }

    /// `ORDER BY DESC(expr)`
    fn order_by_desc(&self, e: &Bound<'_, PyAny>) -> PyResult<Self> {
        Ok(Self {
            b: self.b.clone().order_by_desc(expr(e)?),
        })
    }

    /// `LIMIT n`
    fn limit(&self, n: u64) -> Self {
        Self {
            b: self.b.clone().limit(n),
        }
    }

    /// `OFFSET n`
    fn offset(&self, n: u64) -> Self {
        Self {
            b: self.b.clone().offset(n),
        }
    }

    /// `FROM <g>`
    fn from_(&self, g: &Bound<'_, PyAny>) -> PyResult<Self> {
        Ok(Self {
            b: self.b.clone().from(node(g)?),
        })
    }

    /// `FROM NAMED <g>`
    fn from_named(&self, g: &Bound<'_, PyAny>) -> PyResult<Self> {
        Ok(Self {
            b: self.b.clone().from_named(node(g)?),
        })
    }

    /// Project variables: `select("?a", "?b")`.
    #[pyo3(signature = (*vars))]
    fn select(&self, vars: Vec<Bound<'_, PyAny>>) -> PyResult<Self> {
        let vs = vars.iter().map(node).collect::<PyResult<Vec<_>>>()?;
        Ok(Self {
            b: self.b.clone().select_vars(vs),
        })
    }

    /// Project `(expr AS ?alias)`.
    fn select_expr(&self, e: &Bound<'_, PyAny>, alias: &Bound<'_, PyAny>) -> PyResult<Self> {
        Ok(Self {
            b: self.b.clone().select_expr(expr(e)?, node(alias)?),
        })
    }

    /// `SELECT *`
    fn select_all(&self) -> Self {
        Self {
            b: self.b.clone().select_all(),
        }
    }

    /// `SELECT DISTINCT`
    fn distinct(&self) -> Self {
        Self {
            b: self.b.clone().distinct(),
        }
    }

    /// `SELECT REDUCED`
    fn reduced(&self) -> Self {
        Self {
            b: self.b.clone().reduced(),
        }
    }

    /// `GROUP BY expr`, or `GROUP BY (expr AS ?alias)`.
    #[pyo3(signature = (e, alias = None))]
    fn group_by(&self, e: &Bound<'_, PyAny>, alias: Option<&Bound<'_, PyAny>>) -> PyResult<Self> {
        Ok(Self {
            b: match alias {
                Some(a) => self.b.clone().group_by_as(expr(e)?, node(a)?),
                None => self.b.clone().group_by(expr(e)?),
            },
        })
    }

    /// `HAVING(expr)`
    fn having(&self, e: &Bound<'_, PyAny>) -> PyResult<Self> {
        Ok(Self {
            b: self.b.clone().having(expr(e)?),
        })
    }
}

/// Builds an ASK query.
#[pyclass(
    frozen,
    module = "sparkles.querybuilder",
    name = "AskBuilder",
    skip_from_py_object
)]
pub struct PyAskBuilder {
    b: qb::AskBuilder,
}

#[pymethods]
impl PyAskBuilder {
    #[new]
    fn new() -> Self {
        Self {
            b: qb::AskBuilder::new(),
        }
    }

    /// Declare a prefix.
    fn prefix(&self, prefix: &str, iri: &str) -> Self {
        Self {
            b: self.b.clone().prefix(prefix, iri),
        }
    }

    /// Declare prefixes from a `{prefix: iri}` mapping.
    fn prefixes(&self, prefixes: &Bound<'_, PyDict>) -> PyResult<Self> {
        let mut b = self.b.clone();
        for (k, v) in prefixes.iter() {
            b = b.prefix(k.extract::<String>()?, v.str()?.to_str()?);
        }
        Ok(Self { b })
    }

    /// Set the base IRI.
    fn base(&self, iri: &str) -> Self {
        Self {
            b: self.b.clone().base(iri),
        }
    }

    /// Replace a variable everywhere with a value (a prepared query).
    fn set_var(&self, var: &Bound<'_, PyAny>, value: &Bound<'_, PyAny>) -> PyResult<Self> {
        Ok(Self {
            b: self.b.clone().set_var(node(var)?, node(value)?),
        })
    }

    /// The SPARQL text, checked with the parser.
    fn build(&self, py: Python<'_>) -> PyResult<String> {
        self.b.build().py(py)
    }

    /// The SPARQL text, unchecked.
    fn __str__(&self) -> String {
        self.b.to_string()
    }

    fn __repr__(&self) -> String {
        format!("<{} {:?}>", "AskBuilder", self.b.to_string())
    }

    /// Add the triple pattern `s p o`.
    fn where_(
        &self,
        s: &Bound<'_, PyAny>,
        p: &Bound<'_, PyAny>,
        o: &Bound<'_, PyAny>,
    ) -> PyResult<Self> {
        Ok(Self {
            b: self.b.clone().where_(node(s)?, node(p)?, node(o)?),
        })
    }

    /// Add the patterns of a `WhereBuilder` to this group.
    fn add_where(&self, w: PyRef<'_, PyWhereBuilder>) -> Self {
        Self {
            b: self.b.clone().add_where(w.b.clone()),
        }
    }

    /// `{ … }`
    fn group(&self, w: PyRef<'_, PyWhereBuilder>) -> Self {
        let w = w.b.clone();
        Self {
            b: self.b.clone().group(move |_| w),
        }
    }

    /// `OPTIONAL { … }`
    fn optional(&self, w: PyRef<'_, PyWhereBuilder>) -> Self {
        let w = w.b.clone();
        Self {
            b: self.b.clone().optional(move |_| w),
        }
    }

    /// `{ … } UNION { … } …`
    #[pyo3(signature = (*branches))]
    fn union(&self, branches: Vec<PyRef<'_, PyWhereBuilder>>) -> Self {
        let bs: Vec<qb::WhereBuilder> = branches.iter().map(|w| w.b.clone()).collect();
        Self {
            b: self.b.clone().union_of(bs),
        }
    }

    /// `MINUS { … }`
    fn minus(&self, w: PyRef<'_, PyWhereBuilder>) -> Self {
        let w = w.b.clone();
        Self {
            b: self.b.clone().minus(move |_| w),
        }
    }

    /// `GRAPH g { … }`
    fn graph(&self, g: &Bound<'_, PyAny>, w: PyRef<'_, PyWhereBuilder>) -> PyResult<Self> {
        let w = w.b.clone();
        Ok(Self {
            b: self.b.clone().graph(node(g)?, move |_| w),
        })
    }

    /// `SERVICE <endpoint> { … }` (`SERVICE SILENT` with `silent`)
    #[pyo3(signature = (endpoint, w, *, silent = false))]
    fn service(
        &self,
        endpoint: &Bound<'_, PyAny>,
        w: PyRef<'_, PyWhereBuilder>,
        silent: bool,
    ) -> PyResult<Self> {
        let w = w.b.clone();
        let e = node(endpoint)?;
        Ok(Self {
            b: if silent {
                self.b.clone().service_silent(e, move |_| w)
            } else {
                self.b.clone().service(e, move |_| w)
            },
        })
    }

    /// `FILTER(expr)`
    fn filter(&self, e: &Bound<'_, PyAny>) -> PyResult<Self> {
        Ok(Self {
            b: self.b.clone().filter(expr(e)?),
        })
    }

    /// `FILTER EXISTS { … }`
    fn filter_exists(&self, w: PyRef<'_, PyWhereBuilder>) -> Self {
        let w = w.b.clone();
        Self {
            b: self.b.clone().filter_exists(move |_| w),
        }
    }

    /// `FILTER NOT EXISTS { … }`
    fn filter_not_exists(&self, w: PyRef<'_, PyWhereBuilder>) -> Self {
        let w = w.b.clone();
        Self {
            b: self.b.clone().filter_not_exists(move |_| w),
        }
    }

    /// `BIND(expr AS ?var)`
    fn bind(&self, e: &Bound<'_, PyAny>, var: &Bound<'_, PyAny>) -> PyResult<Self> {
        Ok(Self {
            b: self.b.clone().bind(expr(e)?, node(var)?),
        })
    }

    /// `VALUES ?var { … }`
    fn values(&self, var: &Bound<'_, PyAny>, values: &Bound<'_, PyAny>) -> PyResult<Self> {
        Ok(Self {
            b: self.b.clone().values(node(var)?, nodes(values)?),
        })
    }

    /// `VALUES (?a ?b …) { (…) … }`; `None` in a row is `UNDEF`.
    fn values_rows(&self, vars: &Bound<'_, PyAny>, rows: &Bound<'_, PyAny>) -> PyResult<Self> {
        let vars = nodes(vars)?;
        let rows = rows
            .try_iter()?
            .map(|r| {
                r?.try_iter()?
                    .map(|x| {
                        let x = x?;
                        if x.is_none() {
                            Ok(qb::undef())
                        } else {
                            node(&x)
                        }
                    })
                    .collect::<PyResult<Vec<Node>>>()
            })
            .collect::<PyResult<Vec<_>>>()?;
        Ok(Self {
            b: self.b.clone().values_rows(vars, rows),
        })
    }

    /// A sub-query `{ SELECT … }`.
    fn sub_select(&self, q: PyRef<'_, PySelectBuilder>) -> Self {
        Self {
            b: self.b.clone().sub_select(q.b.clone()),
        }
    }

    /// `FROM <g>`
    fn from_(&self, g: &Bound<'_, PyAny>) -> PyResult<Self> {
        Ok(Self {
            b: self.b.clone().from(node(g)?),
        })
    }

    /// `FROM NAMED <g>`
    fn from_named(&self, g: &Bound<'_, PyAny>) -> PyResult<Self> {
        Ok(Self {
            b: self.b.clone().from_named(node(g)?),
        })
    }
}

/// Builds a CONSTRUCT query.
#[pyclass(
    frozen,
    module = "sparkles.querybuilder",
    name = "ConstructBuilder",
    skip_from_py_object
)]
pub struct PyConstructBuilder {
    b: qb::ConstructBuilder,
}

#[pymethods]
impl PyConstructBuilder {
    #[new]
    fn new() -> Self {
        Self {
            b: qb::ConstructBuilder::new(),
        }
    }

    /// Declare a prefix.
    fn prefix(&self, prefix: &str, iri: &str) -> Self {
        Self {
            b: self.b.clone().prefix(prefix, iri),
        }
    }

    /// Declare prefixes from a `{prefix: iri}` mapping.
    fn prefixes(&self, prefixes: &Bound<'_, PyDict>) -> PyResult<Self> {
        let mut b = self.b.clone();
        for (k, v) in prefixes.iter() {
            b = b.prefix(k.extract::<String>()?, v.str()?.to_str()?);
        }
        Ok(Self { b })
    }

    /// Set the base IRI.
    fn base(&self, iri: &str) -> Self {
        Self {
            b: self.b.clone().base(iri),
        }
    }

    /// Replace a variable everywhere with a value (a prepared query).
    fn set_var(&self, var: &Bound<'_, PyAny>, value: &Bound<'_, PyAny>) -> PyResult<Self> {
        Ok(Self {
            b: self.b.clone().set_var(node(var)?, node(value)?),
        })
    }

    /// The SPARQL text, checked with the parser.
    fn build(&self, py: Python<'_>) -> PyResult<String> {
        self.b.build().py(py)
    }

    /// The SPARQL text, unchecked.
    fn __str__(&self) -> String {
        self.b.to_string()
    }

    fn __repr__(&self) -> String {
        format!("<{} {:?}>", "ConstructBuilder", self.b.to_string())
    }

    /// Add the triple pattern `s p o`.
    fn where_(
        &self,
        s: &Bound<'_, PyAny>,
        p: &Bound<'_, PyAny>,
        o: &Bound<'_, PyAny>,
    ) -> PyResult<Self> {
        Ok(Self {
            b: self.b.clone().where_(node(s)?, node(p)?, node(o)?),
        })
    }

    /// Add the patterns of a `WhereBuilder` to this group.
    fn add_where(&self, w: PyRef<'_, PyWhereBuilder>) -> Self {
        Self {
            b: self.b.clone().add_where(w.b.clone()),
        }
    }

    /// `{ … }`
    fn group(&self, w: PyRef<'_, PyWhereBuilder>) -> Self {
        let w = w.b.clone();
        Self {
            b: self.b.clone().group(move |_| w),
        }
    }

    /// `OPTIONAL { … }`
    fn optional(&self, w: PyRef<'_, PyWhereBuilder>) -> Self {
        let w = w.b.clone();
        Self {
            b: self.b.clone().optional(move |_| w),
        }
    }

    /// `{ … } UNION { … } …`
    #[pyo3(signature = (*branches))]
    fn union(&self, branches: Vec<PyRef<'_, PyWhereBuilder>>) -> Self {
        let bs: Vec<qb::WhereBuilder> = branches.iter().map(|w| w.b.clone()).collect();
        Self {
            b: self.b.clone().union_of(bs),
        }
    }

    /// `MINUS { … }`
    fn minus(&self, w: PyRef<'_, PyWhereBuilder>) -> Self {
        let w = w.b.clone();
        Self {
            b: self.b.clone().minus(move |_| w),
        }
    }

    /// `GRAPH g { … }`
    fn graph(&self, g: &Bound<'_, PyAny>, w: PyRef<'_, PyWhereBuilder>) -> PyResult<Self> {
        let w = w.b.clone();
        Ok(Self {
            b: self.b.clone().graph(node(g)?, move |_| w),
        })
    }

    /// `SERVICE <endpoint> { … }` (`SERVICE SILENT` with `silent`)
    #[pyo3(signature = (endpoint, w, *, silent = false))]
    fn service(
        &self,
        endpoint: &Bound<'_, PyAny>,
        w: PyRef<'_, PyWhereBuilder>,
        silent: bool,
    ) -> PyResult<Self> {
        let w = w.b.clone();
        let e = node(endpoint)?;
        Ok(Self {
            b: if silent {
                self.b.clone().service_silent(e, move |_| w)
            } else {
                self.b.clone().service(e, move |_| w)
            },
        })
    }

    /// `FILTER(expr)`
    fn filter(&self, e: &Bound<'_, PyAny>) -> PyResult<Self> {
        Ok(Self {
            b: self.b.clone().filter(expr(e)?),
        })
    }

    /// `FILTER EXISTS { … }`
    fn filter_exists(&self, w: PyRef<'_, PyWhereBuilder>) -> Self {
        let w = w.b.clone();
        Self {
            b: self.b.clone().filter_exists(move |_| w),
        }
    }

    /// `FILTER NOT EXISTS { … }`
    fn filter_not_exists(&self, w: PyRef<'_, PyWhereBuilder>) -> Self {
        let w = w.b.clone();
        Self {
            b: self.b.clone().filter_not_exists(move |_| w),
        }
    }

    /// `BIND(expr AS ?var)`
    fn bind(&self, e: &Bound<'_, PyAny>, var: &Bound<'_, PyAny>) -> PyResult<Self> {
        Ok(Self {
            b: self.b.clone().bind(expr(e)?, node(var)?),
        })
    }

    /// `VALUES ?var { … }`
    fn values(&self, var: &Bound<'_, PyAny>, values: &Bound<'_, PyAny>) -> PyResult<Self> {
        Ok(Self {
            b: self.b.clone().values(node(var)?, nodes(values)?),
        })
    }

    /// `VALUES (?a ?b …) { (…) … }`; `None` in a row is `UNDEF`.
    fn values_rows(&self, vars: &Bound<'_, PyAny>, rows: &Bound<'_, PyAny>) -> PyResult<Self> {
        let vars = nodes(vars)?;
        let rows = rows
            .try_iter()?
            .map(|r| {
                r?.try_iter()?
                    .map(|x| {
                        let x = x?;
                        if x.is_none() {
                            Ok(qb::undef())
                        } else {
                            node(&x)
                        }
                    })
                    .collect::<PyResult<Vec<Node>>>()
            })
            .collect::<PyResult<Vec<_>>>()?;
        Ok(Self {
            b: self.b.clone().values_rows(vars, rows),
        })
    }

    /// A sub-query `{ SELECT … }`.
    fn sub_select(&self, q: PyRef<'_, PySelectBuilder>) -> Self {
        Self {
            b: self.b.clone().sub_select(q.b.clone()),
        }
    }

    /// `ORDER BY expr`
    fn order_by(&self, e: &Bound<'_, PyAny>) -> PyResult<Self> {
        Ok(Self {
            b: self.b.clone().order_by(expr(e)?),
        })
    }

    /// `ORDER BY DESC(expr)`
    fn order_by_desc(&self, e: &Bound<'_, PyAny>) -> PyResult<Self> {
        Ok(Self {
            b: self.b.clone().order_by_desc(expr(e)?),
        })
    }

    /// `LIMIT n`
    fn limit(&self, n: u64) -> Self {
        Self {
            b: self.b.clone().limit(n),
        }
    }

    /// `OFFSET n`
    fn offset(&self, n: u64) -> Self {
        Self {
            b: self.b.clone().offset(n),
        }
    }

    /// `FROM <g>`
    fn from_(&self, g: &Bound<'_, PyAny>) -> PyResult<Self> {
        Ok(Self {
            b: self.b.clone().from(node(g)?),
        })
    }

    /// `FROM NAMED <g>`
    fn from_named(&self, g: &Bound<'_, PyAny>) -> PyResult<Self> {
        Ok(Self {
            b: self.b.clone().from_named(node(g)?),
        })
    }

    /// Add `s p o` to the template.
    fn construct(
        &self,
        s: &Bound<'_, PyAny>,
        p: &Bound<'_, PyAny>,
        o: &Bound<'_, PyAny>,
    ) -> PyResult<Self> {
        Ok(Self {
            b: self.b.clone().construct(node(s)?, node(p)?, node(o)?),
        })
    }
}

/// Builds a DESCRIBE query.
#[pyclass(
    frozen,
    module = "sparkles.querybuilder",
    name = "DescribeBuilder",
    skip_from_py_object
)]
pub struct PyDescribeBuilder {
    b: qb::DescribeBuilder,
}

#[pymethods]
impl PyDescribeBuilder {
    #[new]
    fn new() -> Self {
        Self {
            b: qb::DescribeBuilder::new(),
        }
    }

    /// Declare a prefix.
    fn prefix(&self, prefix: &str, iri: &str) -> Self {
        Self {
            b: self.b.clone().prefix(prefix, iri),
        }
    }

    /// Declare prefixes from a `{prefix: iri}` mapping.
    fn prefixes(&self, prefixes: &Bound<'_, PyDict>) -> PyResult<Self> {
        let mut b = self.b.clone();
        for (k, v) in prefixes.iter() {
            b = b.prefix(k.extract::<String>()?, v.str()?.to_str()?);
        }
        Ok(Self { b })
    }

    /// Set the base IRI.
    fn base(&self, iri: &str) -> Self {
        Self {
            b: self.b.clone().base(iri),
        }
    }

    /// Replace a variable everywhere with a value (a prepared query).
    fn set_var(&self, var: &Bound<'_, PyAny>, value: &Bound<'_, PyAny>) -> PyResult<Self> {
        Ok(Self {
            b: self.b.clone().set_var(node(var)?, node(value)?),
        })
    }

    /// The SPARQL text, checked with the parser.
    fn build(&self, py: Python<'_>) -> PyResult<String> {
        self.b.build().py(py)
    }

    /// The SPARQL text, unchecked.
    fn __str__(&self) -> String {
        self.b.to_string()
    }

    fn __repr__(&self) -> String {
        format!("<{} {:?}>", "DescribeBuilder", self.b.to_string())
    }

    /// Add the triple pattern `s p o`.
    fn where_(
        &self,
        s: &Bound<'_, PyAny>,
        p: &Bound<'_, PyAny>,
        o: &Bound<'_, PyAny>,
    ) -> PyResult<Self> {
        Ok(Self {
            b: self.b.clone().where_(node(s)?, node(p)?, node(o)?),
        })
    }

    /// Add the patterns of a `WhereBuilder` to this group.
    fn add_where(&self, w: PyRef<'_, PyWhereBuilder>) -> Self {
        Self {
            b: self.b.clone().add_where(w.b.clone()),
        }
    }

    /// `{ … }`
    fn group(&self, w: PyRef<'_, PyWhereBuilder>) -> Self {
        let w = w.b.clone();
        Self {
            b: self.b.clone().group(move |_| w),
        }
    }

    /// `OPTIONAL { … }`
    fn optional(&self, w: PyRef<'_, PyWhereBuilder>) -> Self {
        let w = w.b.clone();
        Self {
            b: self.b.clone().optional(move |_| w),
        }
    }

    /// `{ … } UNION { … } …`
    #[pyo3(signature = (*branches))]
    fn union(&self, branches: Vec<PyRef<'_, PyWhereBuilder>>) -> Self {
        let bs: Vec<qb::WhereBuilder> = branches.iter().map(|w| w.b.clone()).collect();
        Self {
            b: self.b.clone().union_of(bs),
        }
    }

    /// `MINUS { … }`
    fn minus(&self, w: PyRef<'_, PyWhereBuilder>) -> Self {
        let w = w.b.clone();
        Self {
            b: self.b.clone().minus(move |_| w),
        }
    }

    /// `GRAPH g { … }`
    fn graph(&self, g: &Bound<'_, PyAny>, w: PyRef<'_, PyWhereBuilder>) -> PyResult<Self> {
        let w = w.b.clone();
        Ok(Self {
            b: self.b.clone().graph(node(g)?, move |_| w),
        })
    }

    /// `SERVICE <endpoint> { … }` (`SERVICE SILENT` with `silent`)
    #[pyo3(signature = (endpoint, w, *, silent = false))]
    fn service(
        &self,
        endpoint: &Bound<'_, PyAny>,
        w: PyRef<'_, PyWhereBuilder>,
        silent: bool,
    ) -> PyResult<Self> {
        let w = w.b.clone();
        let e = node(endpoint)?;
        Ok(Self {
            b: if silent {
                self.b.clone().service_silent(e, move |_| w)
            } else {
                self.b.clone().service(e, move |_| w)
            },
        })
    }

    /// `FILTER(expr)`
    fn filter(&self, e: &Bound<'_, PyAny>) -> PyResult<Self> {
        Ok(Self {
            b: self.b.clone().filter(expr(e)?),
        })
    }

    /// `FILTER EXISTS { … }`
    fn filter_exists(&self, w: PyRef<'_, PyWhereBuilder>) -> Self {
        let w = w.b.clone();
        Self {
            b: self.b.clone().filter_exists(move |_| w),
        }
    }

    /// `FILTER NOT EXISTS { … }`
    fn filter_not_exists(&self, w: PyRef<'_, PyWhereBuilder>) -> Self {
        let w = w.b.clone();
        Self {
            b: self.b.clone().filter_not_exists(move |_| w),
        }
    }

    /// `BIND(expr AS ?var)`
    fn bind(&self, e: &Bound<'_, PyAny>, var: &Bound<'_, PyAny>) -> PyResult<Self> {
        Ok(Self {
            b: self.b.clone().bind(expr(e)?, node(var)?),
        })
    }

    /// `VALUES ?var { … }`
    fn values(&self, var: &Bound<'_, PyAny>, values: &Bound<'_, PyAny>) -> PyResult<Self> {
        Ok(Self {
            b: self.b.clone().values(node(var)?, nodes(values)?),
        })
    }

    /// `VALUES (?a ?b …) { (…) … }`; `None` in a row is `UNDEF`.
    fn values_rows(&self, vars: &Bound<'_, PyAny>, rows: &Bound<'_, PyAny>) -> PyResult<Self> {
        let vars = nodes(vars)?;
        let rows = rows
            .try_iter()?
            .map(|r| {
                r?.try_iter()?
                    .map(|x| {
                        let x = x?;
                        if x.is_none() {
                            Ok(qb::undef())
                        } else {
                            node(&x)
                        }
                    })
                    .collect::<PyResult<Vec<Node>>>()
            })
            .collect::<PyResult<Vec<_>>>()?;
        Ok(Self {
            b: self.b.clone().values_rows(vars, rows),
        })
    }

    /// A sub-query `{ SELECT … }`.
    fn sub_select(&self, q: PyRef<'_, PySelectBuilder>) -> Self {
        Self {
            b: self.b.clone().sub_select(q.b.clone()),
        }
    }

    /// `ORDER BY expr`
    fn order_by(&self, e: &Bound<'_, PyAny>) -> PyResult<Self> {
        Ok(Self {
            b: self.b.clone().order_by(expr(e)?),
        })
    }

    /// `ORDER BY DESC(expr)`
    fn order_by_desc(&self, e: &Bound<'_, PyAny>) -> PyResult<Self> {
        Ok(Self {
            b: self.b.clone().order_by_desc(expr(e)?),
        })
    }

    /// `LIMIT n`
    fn limit(&self, n: u64) -> Self {
        Self {
            b: self.b.clone().limit(n),
        }
    }

    /// `OFFSET n`
    fn offset(&self, n: u64) -> Self {
        Self {
            b: self.b.clone().offset(n),
        }
    }

    /// `FROM <g>`
    fn from_(&self, g: &Bound<'_, PyAny>) -> PyResult<Self> {
        Ok(Self {
            b: self.b.clone().from(node(g)?),
        })
    }

    /// `FROM NAMED <g>`
    fn from_named(&self, g: &Bound<'_, PyAny>) -> PyResult<Self> {
        Ok(Self {
            b: self.b.clone().from_named(node(g)?),
        })
    }

    /// Describe resources or variables.
    #[pyo3(signature = (*resources))]
    fn describe(&self, resources: Vec<Bound<'_, PyAny>>) -> PyResult<Self> {
        let mut b = self.b.clone();
        for r in &resources {
            b = b.describe(node(r)?);
        }
        Ok(Self { b })
    }
}

/// Builds a SPARQL Update request.
#[pyclass(
    frozen,
    module = "sparkles.querybuilder",
    name = "UpdateBuilder",
    skip_from_py_object
)]
pub struct PyUpdateBuilder {
    b: qb::UpdateBuilder,
}

#[pymethods]
impl PyUpdateBuilder {
    #[new]
    fn new() -> Self {
        Self {
            b: qb::UpdateBuilder::new(),
        }
    }

    /// Declare a prefix.
    fn prefix(&self, prefix: &str, iri: &str) -> Self {
        Self {
            b: self.b.clone().prefix(prefix, iri),
        }
    }

    /// Declare prefixes from a `{prefix: iri}` mapping.
    fn prefixes(&self, prefixes: &Bound<'_, PyDict>) -> PyResult<Self> {
        let mut b = self.b.clone();
        for (k, v) in prefixes.iter() {
            b = b.prefix(k.extract::<String>()?, v.str()?.to_str()?);
        }
        Ok(Self { b })
    }

    /// Set the base IRI.
    fn base(&self, iri: &str) -> Self {
        Self {
            b: self.b.clone().base(iri),
        }
    }

    /// Replace a variable everywhere with a value (a prepared query).
    fn set_var(&self, var: &Bound<'_, PyAny>, value: &Bound<'_, PyAny>) -> PyResult<Self> {
        Ok(Self {
            b: self.b.clone().set_var(node(var)?, node(value)?),
        })
    }

    /// The SPARQL text, checked with the parser.
    fn build(&self, py: Python<'_>) -> PyResult<String> {
        self.b.build().py(py)
    }

    /// The SPARQL text, unchecked.
    fn __str__(&self) -> String {
        self.b.to_string()
    }

    fn __repr__(&self) -> String {
        format!("<{} {:?}>", "UpdateBuilder", self.b.to_string())
    }

    /// Add the triple pattern `s p o`.
    fn where_(
        &self,
        s: &Bound<'_, PyAny>,
        p: &Bound<'_, PyAny>,
        o: &Bound<'_, PyAny>,
    ) -> PyResult<Self> {
        Ok(Self {
            b: self.b.clone().where_(node(s)?, node(p)?, node(o)?),
        })
    }

    /// Add the patterns of a `WhereBuilder` to this group.
    fn add_where(&self, w: PyRef<'_, PyWhereBuilder>) -> Self {
        Self {
            b: self.b.clone().add_where(w.b.clone()),
        }
    }

    /// `{ … }`
    fn group(&self, w: PyRef<'_, PyWhereBuilder>) -> Self {
        let w = w.b.clone();
        Self {
            b: self.b.clone().group(move |_| w),
        }
    }

    /// `OPTIONAL { … }`
    fn optional(&self, w: PyRef<'_, PyWhereBuilder>) -> Self {
        let w = w.b.clone();
        Self {
            b: self.b.clone().optional(move |_| w),
        }
    }

    /// `{ … } UNION { … } …`
    #[pyo3(signature = (*branches))]
    fn union(&self, branches: Vec<PyRef<'_, PyWhereBuilder>>) -> Self {
        let bs: Vec<qb::WhereBuilder> = branches.iter().map(|w| w.b.clone()).collect();
        Self {
            b: self.b.clone().union_of(bs),
        }
    }

    /// `MINUS { … }`
    fn minus(&self, w: PyRef<'_, PyWhereBuilder>) -> Self {
        let w = w.b.clone();
        Self {
            b: self.b.clone().minus(move |_| w),
        }
    }

    /// `GRAPH g { … }`
    fn graph(&self, g: &Bound<'_, PyAny>, w: PyRef<'_, PyWhereBuilder>) -> PyResult<Self> {
        let w = w.b.clone();
        Ok(Self {
            b: self.b.clone().graph(node(g)?, move |_| w),
        })
    }

    /// `SERVICE <endpoint> { … }` (`SERVICE SILENT` with `silent`)
    #[pyo3(signature = (endpoint, w, *, silent = false))]
    fn service(
        &self,
        endpoint: &Bound<'_, PyAny>,
        w: PyRef<'_, PyWhereBuilder>,
        silent: bool,
    ) -> PyResult<Self> {
        let w = w.b.clone();
        let e = node(endpoint)?;
        Ok(Self {
            b: if silent {
                self.b.clone().service_silent(e, move |_| w)
            } else {
                self.b.clone().service(e, move |_| w)
            },
        })
    }

    /// `FILTER(expr)`
    fn filter(&self, e: &Bound<'_, PyAny>) -> PyResult<Self> {
        Ok(Self {
            b: self.b.clone().filter(expr(e)?),
        })
    }

    /// `FILTER EXISTS { … }`
    fn filter_exists(&self, w: PyRef<'_, PyWhereBuilder>) -> Self {
        let w = w.b.clone();
        Self {
            b: self.b.clone().filter_exists(move |_| w),
        }
    }

    /// `FILTER NOT EXISTS { … }`
    fn filter_not_exists(&self, w: PyRef<'_, PyWhereBuilder>) -> Self {
        let w = w.b.clone();
        Self {
            b: self.b.clone().filter_not_exists(move |_| w),
        }
    }

    /// `BIND(expr AS ?var)`
    fn bind(&self, e: &Bound<'_, PyAny>, var: &Bound<'_, PyAny>) -> PyResult<Self> {
        Ok(Self {
            b: self.b.clone().bind(expr(e)?, node(var)?),
        })
    }

    /// `VALUES ?var { … }`
    fn values(&self, var: &Bound<'_, PyAny>, values: &Bound<'_, PyAny>) -> PyResult<Self> {
        Ok(Self {
            b: self.b.clone().values(node(var)?, nodes(values)?),
        })
    }

    /// `VALUES (?a ?b …) { (…) … }`; `None` in a row is `UNDEF`.
    fn values_rows(&self, vars: &Bound<'_, PyAny>, rows: &Bound<'_, PyAny>) -> PyResult<Self> {
        let vars = nodes(vars)?;
        let rows = rows
            .try_iter()?
            .map(|r| {
                r?.try_iter()?
                    .map(|x| {
                        let x = x?;
                        if x.is_none() {
                            Ok(qb::undef())
                        } else {
                            node(&x)
                        }
                    })
                    .collect::<PyResult<Vec<Node>>>()
            })
            .collect::<PyResult<Vec<_>>>()?;
        Ok(Self {
            b: self.b.clone().values_rows(vars, rows),
        })
    }

    /// A sub-query `{ SELECT … }`.
    fn sub_select(&self, q: PyRef<'_, PySelectBuilder>) -> Self {
        Self {
            b: self.b.clone().sub_select(q.b.clone()),
        }
    }

    /// Start a new operation (`;`), so that the next ones do not merge with the last.
    fn then(&self) -> Self {
        Self {
            b: self.b.clone().then(),
        }
    }

    /// `INSERT DATA { s p o }`, or in `graph`
    #[pyo3(signature = (s, p, o, *, graph = None))]
    fn insert_data(
        &self,
        s: &Bound<'_, PyAny>,
        p: &Bound<'_, PyAny>,
        o: &Bound<'_, PyAny>,
        graph: Option<&Bound<'_, PyAny>>,
    ) -> PyResult<Self> {
        let b = self.b.clone();
        Ok(Self {
            b: match graph {
                Some(g) => b.insert_data_graph(node(g)?, node(s)?, node(p)?, node(o)?),
                None => b.insert_data(node(s)?, node(p)?, node(o)?),
            },
        })
    }

    /// `DELETE DATA { s p o }`, or in `graph`
    #[pyo3(signature = (s, p, o, *, graph = None))]
    fn delete_data(
        &self,
        s: &Bound<'_, PyAny>,
        p: &Bound<'_, PyAny>,
        o: &Bound<'_, PyAny>,
        graph: Option<&Bound<'_, PyAny>>,
    ) -> PyResult<Self> {
        let b = self.b.clone();
        Ok(Self {
            b: match graph {
                Some(g) => b.delete_data_graph(node(g)?, node(s)?, node(p)?, node(o)?),
                None => b.delete_data(node(s)?, node(p)?, node(o)?),
            },
        })
    }

    /// `DELETE WHERE { s p o }`, or in `graph`
    #[pyo3(signature = (s, p, o, *, graph = None))]
    fn delete_where(
        &self,
        s: &Bound<'_, PyAny>,
        p: &Bound<'_, PyAny>,
        o: &Bound<'_, PyAny>,
        graph: Option<&Bound<'_, PyAny>>,
    ) -> PyResult<Self> {
        let b = self.b.clone();
        Ok(Self {
            b: match graph {
                Some(g) => b.delete_where_graph(node(g)?, node(s)?, node(p)?, node(o)?),
                None => b.delete_where(node(s)?, node(p)?, node(o)?),
            },
        })
    }

    /// Add `s p o` to the `DELETE { }` template, or in `graph`
    #[pyo3(signature = (s, p, o, *, graph = None))]
    fn delete(
        &self,
        s: &Bound<'_, PyAny>,
        p: &Bound<'_, PyAny>,
        o: &Bound<'_, PyAny>,
        graph: Option<&Bound<'_, PyAny>>,
    ) -> PyResult<Self> {
        let b = self.b.clone();
        Ok(Self {
            b: match graph {
                Some(g) => b.delete_graph(node(g)?, node(s)?, node(p)?, node(o)?),
                None => b.delete(node(s)?, node(p)?, node(o)?),
            },
        })
    }

    /// Add `s p o` to the `INSERT { }` template, or in `graph`
    #[pyo3(signature = (s, p, o, *, graph = None))]
    fn insert(
        &self,
        s: &Bound<'_, PyAny>,
        p: &Bound<'_, PyAny>,
        o: &Bound<'_, PyAny>,
        graph: Option<&Bound<'_, PyAny>>,
    ) -> PyResult<Self> {
        let b = self.b.clone();
        Ok(Self {
            b: match graph {
                Some(g) => b.insert_graph(node(g)?, node(s)?, node(p)?, node(o)?),
                None => b.insert(node(s)?, node(p)?, node(o)?),
            },
        })
    }

    /// `WITH <g>`
    fn with_(&self, g: &Bound<'_, PyAny>) -> PyResult<Self> {
        Ok(Self {
            b: self.b.clone().with(node(g)?),
        })
    }

    /// `USING <g>`
    fn using(&self, g: &Bound<'_, PyAny>) -> PyResult<Self> {
        Ok(Self {
            b: self.b.clone().using(node(g)?),
        })
    }

    /// `USING NAMED <g>`
    fn using_named(&self, g: &Bound<'_, PyAny>) -> PyResult<Self> {
        Ok(Self {
            b: self.b.clone().using_named(node(g)?),
        })
    }

    /// `LOAD <source>`, or `LOAD <source> INTO GRAPH <into>`
    #[pyo3(signature = (source, *, into = None))]
    fn load(&self, source: &Bound<'_, PyAny>, into: Option<&Bound<'_, PyAny>>) -> PyResult<Self> {
        Ok(Self {
            b: match into {
                Some(g) => self.b.clone().load_into(node(source)?, node(g)?),
                None => self.b.clone().load(node(source)?),
            },
        })
    }

    /// `CLEAR` a graph, or `"DEFAULT"`, `"NAMED"` or `"ALL"`
    fn clear(&self, target_: &Bound<'_, PyAny>) -> PyResult<Self> {
        Ok(Self {
            b: self.b.clone().clear(target(target_)?),
        })
    }

    /// `DROP` a graph, or `"DEFAULT"`, `"NAMED"` or `"ALL"`
    fn drop(&self, target_: &Bound<'_, PyAny>) -> PyResult<Self> {
        Ok(Self {
            b: self.b.clone().drop(target(target_)?),
        })
    }

    /// `CREATE GRAPH <g>`
    fn create(&self, g: &Bound<'_, PyAny>) -> PyResult<Self> {
        Ok(Self {
            b: self.b.clone().create(node(g)?),
        })
    }

    /// `ADD from TO to`
    fn add(&self, source: &Bound<'_, PyAny>, to: &Bound<'_, PyAny>) -> PyResult<Self> {
        Ok(Self {
            b: self.b.clone().add(target(source)?, target(to)?),
        })
    }

    /// `COPY from TO to`
    fn copy(&self, source: &Bound<'_, PyAny>, to: &Bound<'_, PyAny>) -> PyResult<Self> {
        Ok(Self {
            b: self.b.clone().copy(target(source)?, target(to)?),
        })
    }

    /// `MOVE from TO to`
    fn move_(&self, source: &Bound<'_, PyAny>, to: &Bound<'_, PyAny>) -> PyResult<Self> {
        Ok(Self {
            b: self.b.clone().move_(target(source)?, target(to)?),
        })
    }
}

/// A group of patterns, for `optional`, `union`, `graph`, `minus` and the other nested
/// patterns.
#[pyclass(
    frozen,
    module = "sparkles.querybuilder",
    name = "WhereBuilder",
    skip_from_py_object
)]
pub struct PyWhereBuilder {
    b: qb::WhereBuilder,
}

#[pymethods]
impl PyWhereBuilder {
    #[new]
    fn new() -> Self {
        Self {
            b: qb::WhereBuilder::new(),
        }
    }

    /// Add the triple pattern `s p o`.
    fn where_(
        &self,
        s: &Bound<'_, PyAny>,
        p: &Bound<'_, PyAny>,
        o: &Bound<'_, PyAny>,
    ) -> PyResult<Self> {
        Ok(Self {
            b: self.b.clone().where_(node(s)?, node(p)?, node(o)?),
        })
    }

    /// Add the patterns of a `WhereBuilder` to this group.
    fn add_where(&self, w: PyRef<'_, PyWhereBuilder>) -> Self {
        Self {
            b: self.b.clone().add_where(w.b.clone()),
        }
    }

    /// `{ … }`
    fn group(&self, w: PyRef<'_, PyWhereBuilder>) -> Self {
        let w = w.b.clone();
        Self {
            b: self.b.clone().group(move |_| w),
        }
    }

    /// `OPTIONAL { … }`
    fn optional(&self, w: PyRef<'_, PyWhereBuilder>) -> Self {
        let w = w.b.clone();
        Self {
            b: self.b.clone().optional(move |_| w),
        }
    }

    /// `{ … } UNION { … } …`
    #[pyo3(signature = (*branches))]
    fn union(&self, branches: Vec<PyRef<'_, PyWhereBuilder>>) -> Self {
        let bs: Vec<qb::WhereBuilder> = branches.iter().map(|w| w.b.clone()).collect();
        Self {
            b: self.b.clone().union_of(bs),
        }
    }

    /// `MINUS { … }`
    fn minus(&self, w: PyRef<'_, PyWhereBuilder>) -> Self {
        let w = w.b.clone();
        Self {
            b: self.b.clone().minus(move |_| w),
        }
    }

    /// `GRAPH g { … }`
    fn graph(&self, g: &Bound<'_, PyAny>, w: PyRef<'_, PyWhereBuilder>) -> PyResult<Self> {
        let w = w.b.clone();
        Ok(Self {
            b: self.b.clone().graph(node(g)?, move |_| w),
        })
    }

    /// `SERVICE <endpoint> { … }` (`SERVICE SILENT` with `silent`)
    #[pyo3(signature = (endpoint, w, *, silent = false))]
    fn service(
        &self,
        endpoint: &Bound<'_, PyAny>,
        w: PyRef<'_, PyWhereBuilder>,
        silent: bool,
    ) -> PyResult<Self> {
        let w = w.b.clone();
        let e = node(endpoint)?;
        Ok(Self {
            b: if silent {
                self.b.clone().service_silent(e, move |_| w)
            } else {
                self.b.clone().service(e, move |_| w)
            },
        })
    }

    /// `FILTER(expr)`
    fn filter(&self, e: &Bound<'_, PyAny>) -> PyResult<Self> {
        Ok(Self {
            b: self.b.clone().filter(expr(e)?),
        })
    }

    /// `FILTER EXISTS { … }`
    fn filter_exists(&self, w: PyRef<'_, PyWhereBuilder>) -> Self {
        let w = w.b.clone();
        Self {
            b: self.b.clone().filter_exists(move |_| w),
        }
    }

    /// `FILTER NOT EXISTS { … }`
    fn filter_not_exists(&self, w: PyRef<'_, PyWhereBuilder>) -> Self {
        let w = w.b.clone();
        Self {
            b: self.b.clone().filter_not_exists(move |_| w),
        }
    }

    /// `BIND(expr AS ?var)`
    fn bind(&self, e: &Bound<'_, PyAny>, var: &Bound<'_, PyAny>) -> PyResult<Self> {
        Ok(Self {
            b: self.b.clone().bind(expr(e)?, node(var)?),
        })
    }

    /// `VALUES ?var { … }`
    fn values(&self, var: &Bound<'_, PyAny>, values: &Bound<'_, PyAny>) -> PyResult<Self> {
        Ok(Self {
            b: self.b.clone().values(node(var)?, nodes(values)?),
        })
    }

    /// `VALUES (?a ?b …) { (…) … }`; `None` in a row is `UNDEF`.
    fn values_rows(&self, vars: &Bound<'_, PyAny>, rows: &Bound<'_, PyAny>) -> PyResult<Self> {
        let vars = nodes(vars)?;
        let rows = rows
            .try_iter()?
            .map(|r| {
                r?.try_iter()?
                    .map(|x| {
                        let x = x?;
                        if x.is_none() {
                            Ok(qb::undef())
                        } else {
                            node(&x)
                        }
                    })
                    .collect::<PyResult<Vec<Node>>>()
            })
            .collect::<PyResult<Vec<_>>>()?;
        Ok(Self {
            b: self.b.clone().values_rows(vars, rows),
        })
    }

    /// A sub-query `{ SELECT … }`.
    fn sub_select(&self, q: PyRef<'_, PySelectBuilder>) -> Self {
        Self {
            b: self.b.clone().sub_select(q.b.clone()),
        }
    }

    fn __repr__(&self) -> String {
        "<WhereBuilder>".to_string()
    }
}

pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<PySelectBuilder>()?;
    m.add_class::<PyAskBuilder>()?;
    m.add_class::<PyConstructBuilder>()?;
    m.add_class::<PyDescribeBuilder>()?;
    m.add_class::<PyUpdateBuilder>()?;
    m.add_class::<PyWhereBuilder>()?;
    Ok(())
}
