//! SPARQL Update builder.

use std::borrow::Borrow;
use std::fmt;

use oxrdf::{GraphName, Quad, Triple};

use super::pattern::WhereBuilder;
use super::query::{Common, finish};
use super::render::{Pos, Renderer, pad};
use super::term::{IntoNode, Node, parse_term};
use crate::Result;
use crate::dataset::Dataset;
use crate::sparql::update::UpdateStats;

/// The target of `CLEAR` / `DROP` (and the source / destination of `ADD` / `COPY` /
/// `MOVE`, where only `Default` and `Graph` are allowed).
///
/// Converts from a [`Node`] / `oxrdf::NamedNode` (a graph) and from strings: `"DEFAULT"`,
/// `"NAMED"` and `"ALL"` (any case) are the keywords, anything else is parsed as a term.
#[derive(Clone, Debug, PartialEq)]
pub enum GraphTarget {
    Default,
    Named,
    All,
    Graph(Node),
}

impl GraphTarget {
    /// A single named graph.
    pub fn graph(g: impl IntoNode) -> GraphTarget {
        GraphTarget::Graph(g.into_node())
    }
}
impl From<Node> for GraphTarget {
    fn from(n: Node) -> Self {
        GraphTarget::Graph(n)
    }
}
impl From<oxrdf::NamedNode> for GraphTarget {
    fn from(n: oxrdf::NamedNode) -> Self {
        GraphTarget::Graph(n.into_node())
    }
}
impl From<&oxrdf::NamedNode> for GraphTarget {
    fn from(n: &oxrdf::NamedNode) -> Self {
        GraphTarget::Graph(n.into_node())
    }
}
impl From<&str> for GraphTarget {
    fn from(s: &str) -> Self {
        match s.trim().to_ascii_uppercase().as_str() {
            "DEFAULT" => GraphTarget::Default,
            "NAMED" => GraphTarget::Named,
            "ALL" => GraphTarget::All,
            _ => GraphTarget::Graph(parse_term(s)),
        }
    }
}

/// `(graph, s, p, o)`; `None` is the default graph.
type QuadPattern = (Option<Node>, Node, Node, Node);

#[derive(Clone, Debug)]
enum Op {
    InsertData(Vec<QuadPattern>),
    DeleteData(Vec<QuadPattern>),
    DeleteWhere(Vec<QuadPattern>),
    Modify {
        with: Option<Node>,
        delete: Vec<QuadPattern>,
        insert: Vec<QuadPattern>,
        using: Vec<Node>,
        using_named: Vec<Node>,
        where_: WhereBuilder,
    },
    Load {
        silent: bool,
        source: Node,
        into: Option<Node>,
    },
    Clear(bool, GraphTarget),
    Drop(bool, GraphTarget),
    Create(bool, Node),
    Transfer(&'static str, bool, GraphTarget, GraphTarget),
}

/// Builds a SPARQL Update request (Jena's `UpdateBuilder`, plus the graph-management
/// operations). Operations are separated by `;`.
///
/// Consecutive `insert_data` calls share one `INSERT DATA`, and `delete` / `insert` /
/// `with` / `using` / the WHERE methods all extend the current `DELETE … INSERT … WHERE`
/// operation; call [`then`](Self::then) to start a new operation of the same kind.
///
/// ```
/// use sparkles::querybuilder::{UpdateBuilder, lit};
/// let u = UpdateBuilder::new()
///     .prefix("ex", "http://example.org/")
///     .delete("?p", "ex:status", "?old")
///     .insert("?p", "ex:status", lit("active"))
///     .where_("?p", "ex:status", "?old");
/// assert_eq!(
///     u.build().unwrap(),
///     "PREFIX ex: <http://example.org/>\n\
///      DELETE {\n  ?p ex:status ?old .\n}\n\
///      INSERT {\n  ?p ex:status \"active\" .\n}\n\
///      WHERE {\n  ?p ex:status ?old .\n}"
/// );
/// ```
#[derive(Clone, Debug, Default)]
pub struct UpdateBuilder {
    common: Common,
    ops: Vec<Op>,
    /// Set by `then()`: the next data / modify call starts a new operation.
    sealed: bool,
}

fn quad(g: Option<Node>, s: impl IntoNode, p: impl IntoNode, o: impl IntoNode) -> QuadPattern {
    (g, s.into_node(), p.into_node(), o.into_node())
}

fn from_triple(t: &Triple) -> QuadPattern {
    quad(None, &t.subject, &t.predicate, &t.object)
}

fn from_quad(q: &Quad) -> QuadPattern {
    let g = match &q.graph_name {
        GraphName::DefaultGraph => None,
        g => Some(g.into_node()),
    };
    quad(g, &q.subject, &q.predicate, &q.object)
}

impl UpdateBuilder {
    pub fn new() -> UpdateBuilder {
        UpdateBuilder::default()
    }

    crate::querybuilder::common_methods!();
    crate::querybuilder::where_methods!();

    /// Ends the current operation: the next call starts a new one.
    pub fn then(mut self) -> Self {
        self.sealed = true;
        self
    }

    fn push(&mut self, op: Op) {
        self.ops.push(op);
        self.sealed = false;
    }

    fn modify(&mut self) -> &mut Op {
        if self.sealed || !matches!(self.ops.last(), Some(Op::Modify { .. })) {
            self.push(Op::Modify {
                with: None,
                delete: Vec::new(),
                insert: Vec::new(),
                using: Vec::new(),
                using_named: Vec::new(),
                where_: WhereBuilder::new(),
            });
        }
        self.ops.last_mut().expect("just pushed")
    }

    fn where_mut(&mut self) -> &mut WhereBuilder {
        match self.modify() {
            Op::Modify { where_, .. } => where_,
            _ => unreachable!(),
        }
    }

    fn data(&mut self, delete: bool, quads: impl IntoIterator<Item = QuadPattern>) {
        let extend = !self.sealed
            && match self.ops.last() {
                Some(Op::InsertData(_)) => !delete,
                Some(Op::DeleteData(_)) => delete,
                _ => false,
            };
        if !extend {
            self.push(if delete {
                Op::DeleteData(Vec::new())
            } else {
                Op::InsertData(Vec::new())
            });
        }
        match self.ops.last_mut() {
            Some(Op::InsertData(v) | Op::DeleteData(v)) => v.extend(quads),
            _ => unreachable!(),
        }
    }

    // ---------------------------------------------------------------- DATA ----

    /// `INSERT DATA { s p o }`
    pub fn insert_data(mut self, s: impl IntoNode, p: impl IntoNode, o: impl IntoNode) -> Self {
        self.data(false, [quad(None, s, p, o)]);
        self
    }

    /// `INSERT DATA { GRAPH g { s p o } }`
    pub fn insert_data_graph(
        mut self,
        g: impl IntoNode,
        s: impl IntoNode,
        p: impl IntoNode,
        o: impl IntoNode,
    ) -> Self {
        self.data(false, [quad(Some(g.into_node()), s, p, o)]);
        self
    }

    /// `INSERT DATA` for `oxrdf` triples (default graph).
    pub fn insert_data_triples<T: Borrow<Triple>>(
        mut self,
        ts: impl IntoIterator<Item = T>,
    ) -> Self {
        self.data(false, ts.into_iter().map(|t| from_triple(t.borrow())));
        self
    }

    /// `INSERT DATA` for `oxrdf` quads.
    pub fn insert_data_quads<Q: Borrow<Quad>>(mut self, qs: impl IntoIterator<Item = Q>) -> Self {
        self.data(false, qs.into_iter().map(|q| from_quad(q.borrow())));
        self
    }

    /// `DELETE DATA { s p o }`
    pub fn delete_data(mut self, s: impl IntoNode, p: impl IntoNode, o: impl IntoNode) -> Self {
        self.data(true, [quad(None, s, p, o)]);
        self
    }

    /// `DELETE DATA { GRAPH g { s p o } }`
    pub fn delete_data_graph(
        mut self,
        g: impl IntoNode,
        s: impl IntoNode,
        p: impl IntoNode,
        o: impl IntoNode,
    ) -> Self {
        self.data(true, [quad(Some(g.into_node()), s, p, o)]);
        self
    }

    /// `DELETE DATA` for `oxrdf` triples (default graph).
    pub fn delete_data_triples<T: Borrow<Triple>>(
        mut self,
        ts: impl IntoIterator<Item = T>,
    ) -> Self {
        self.data(true, ts.into_iter().map(|t| from_triple(t.borrow())));
        self
    }

    /// `DELETE DATA` for `oxrdf` quads.
    pub fn delete_data_quads<Q: Borrow<Quad>>(mut self, qs: impl IntoIterator<Item = Q>) -> Self {
        self.data(true, qs.into_iter().map(|q| from_quad(q.borrow())));
        self
    }

    // -------------------------------------------------------- DELETE WHERE ----

    /// `DELETE WHERE { s p o }` (consecutive calls share the operation).
    pub fn delete_where(self, s: impl IntoNode, p: impl IntoNode, o: impl IntoNode) -> Self {
        self.delete_where_quad(quad(None, s, p, o))
    }

    /// `DELETE WHERE { GRAPH g { s p o } }`
    pub fn delete_where_graph(
        self,
        g: impl IntoNode,
        s: impl IntoNode,
        p: impl IntoNode,
        o: impl IntoNode,
    ) -> Self {
        self.delete_where_quad(quad(Some(g.into_node()), s, p, o))
    }

    fn delete_where_quad(mut self, q: QuadPattern) -> Self {
        if self.sealed || !matches!(self.ops.last(), Some(Op::DeleteWhere(_))) {
            self.push(Op::DeleteWhere(Vec::new()));
        }
        if let Some(Op::DeleteWhere(v)) = self.ops.last_mut() {
            v.push(q);
        }
        self
    }

    // ---------------------------------------------------- DELETE/INSERT WHERE ----

    fn with_modify(mut self, f: impl FnOnce(&mut Op)) -> Self {
        f(self.modify());
        self
    }

    /// Adds `s p o` to the `DELETE { }` template.
    pub fn delete(self, s: impl IntoNode, p: impl IntoNode, o: impl IntoNode) -> Self {
        let q = quad(None, s, p, o);
        self.with_modify(|op| {
            if let Op::Modify { delete, .. } = op {
                delete.push(q);
            }
        })
    }

    /// Adds `GRAPH g { s p o }` to the `DELETE { }` template.
    pub fn delete_graph(
        self,
        g: impl IntoNode,
        s: impl IntoNode,
        p: impl IntoNode,
        o: impl IntoNode,
    ) -> Self {
        let q = quad(Some(g.into_node()), s, p, o);
        self.with_modify(|op| {
            if let Op::Modify { delete, .. } = op {
                delete.push(q);
            }
        })
    }

    /// Adds `s p o` to the `INSERT { }` template.
    pub fn insert(self, s: impl IntoNode, p: impl IntoNode, o: impl IntoNode) -> Self {
        let q = quad(None, s, p, o);
        self.with_modify(|op| {
            if let Op::Modify { insert, .. } = op {
                insert.push(q);
            }
        })
    }

    /// Adds `GRAPH g { s p o }` to the `INSERT { }` template.
    pub fn insert_graph(
        self,
        g: impl IntoNode,
        s: impl IntoNode,
        p: impl IntoNode,
        o: impl IntoNode,
    ) -> Self {
        let q = quad(Some(g.into_node()), s, p, o);
        self.with_modify(|op| {
            if let Op::Modify { insert, .. } = op {
                insert.push(q);
            }
        })
    }

    /// `WITH <g>`
    pub fn with(self, g: impl IntoNode) -> Self {
        let g = g.into_node();
        self.with_modify(|op| {
            if let Op::Modify { with, .. } = op {
                *with = Some(g);
            }
        })
    }

    /// `USING <g>`
    pub fn using(self, g: impl IntoNode) -> Self {
        let g = g.into_node();
        self.with_modify(|op| {
            if let Op::Modify { using, .. } = op {
                using.push(g);
            }
        })
    }

    /// `USING NAMED <g>`
    pub fn using_named(self, g: impl IntoNode) -> Self {
        let g = g.into_node();
        self.with_modify(|op| {
            if let Op::Modify { using_named, .. } = op {
                using_named.push(g);
            }
        })
    }

    // ------------------------------------------------------ graph management ----

    /// `LOAD <source>`
    pub fn load(mut self, source: impl IntoNode) -> Self {
        self.push(Op::Load {
            silent: false,
            source: source.into_node(),
            into: None,
        });
        self
    }

    /// `LOAD <source> INTO GRAPH <g>`
    pub fn load_into(mut self, source: impl IntoNode, g: impl IntoNode) -> Self {
        self.push(Op::Load {
            silent: false,
            source: source.into_node(),
            into: Some(g.into_node()),
        });
        self
    }

    /// `CLEAR (GRAPH <g> | DEFAULT | NAMED | ALL)`
    pub fn clear(mut self, target: impl Into<GraphTarget>) -> Self {
        self.push(Op::Clear(false, target.into()));
        self
    }

    /// `DROP (GRAPH <g> | DEFAULT | NAMED | ALL)`
    pub fn drop(mut self, target: impl Into<GraphTarget>) -> Self {
        self.push(Op::Drop(false, target.into()));
        self
    }

    /// `CREATE GRAPH <g>`
    pub fn create(mut self, g: impl IntoNode) -> Self {
        self.push(Op::Create(false, g.into_node()));
        self
    }

    /// `ADD from TO to`
    pub fn add(mut self, from: impl Into<GraphTarget>, to: impl Into<GraphTarget>) -> Self {
        self.push(Op::Transfer("ADD", false, from.into(), to.into()));
        self
    }

    /// `COPY from TO to`
    pub fn copy(mut self, from: impl Into<GraphTarget>, to: impl Into<GraphTarget>) -> Self {
        self.push(Op::Transfer("COPY", false, from.into(), to.into()));
        self
    }

    /// `MOVE from TO to`
    pub fn move_(mut self, from: impl Into<GraphTarget>, to: impl Into<GraphTarget>) -> Self {
        self.push(Op::Transfer("MOVE", false, from.into(), to.into()));
        self
    }

    /// Makes the last `LOAD` / `CLEAR` / `DROP` / `CREATE` / `ADD` / `COPY` / `MOVE`
    /// operation `SILENT`.
    pub fn silent(mut self) -> Self {
        match self.ops.last_mut() {
            Some(
                Op::Load { silent, .. }
                | Op::Clear(silent, _)
                | Op::Drop(silent, _)
                | Op::Create(silent, _)
                | Op::Transfer(_, silent, _, _),
            ) => *silent = true,
            _ => self
                .common
                .errors
                .push("silent() must follow a graph management operation".into()),
        }
        self
    }

    // ----------------------------------------------------------------- output ----

    fn render(&self) -> (String, Vec<String>) {
        let mut r = Renderer::new(&self.common);
        if self.ops.is_empty() {
            r.errors.push("empty update request".into());
        }
        let ops: Vec<String> = self.ops.iter().map(|op| render_op(&mut r, op)).collect();
        let body = ops.join(" ;\n");
        let pro = r.prologue(self.common.base.as_deref());
        (pro + &body, r.errors)
    }

    /// Renders and validates the update request.
    pub fn build(&self) -> Result<String> {
        let (text, errors) = self.render();
        finish(text, errors, true)
    }

    /// Builds the request and applies it to `ds` (all operations in one transaction).
    pub fn execute(&self, ds: &Dataset) -> Result<UpdateStats> {
        ds.update(&self.build()?)
    }
}

impl fmt::Display for UpdateBuilder {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.render().0)
    }
}

/// `{ triples GRAPH g { triples } … }`, grouping quads by graph (first-appearance order).
fn quads_block(r: &mut Renderer, quads: &[QuadPattern]) -> String {
    let mut groups: Vec<(Option<&Node>, Vec<&QuadPattern>)> = Vec::new();
    for q in quads {
        match groups.iter_mut().find(|(g, _)| *g == q.0.as_ref()) {
            Some((_, v)) => v.push(q),
            None => groups.push((q.0.as_ref(), vec![q])),
        }
    }
    let mut s = String::from("{\n");
    for (g, qs) in groups {
        let (indent, close) = match g {
            None => (pad(1), None),
            Some(g) => {
                let g = r.node(g, Pos::Term);
                s.push_str(&format!("  GRAPH {g} {{\n"));
                (pad(2), Some("  }\n"))
            }
        };
        for (_, sub, p, o) in qs {
            let t = r.triple(sub, p, o, Pos::TemplatePred);
            s.push_str(&format!("{indent}{t}\n"));
        }
        if let Some(c) = close {
            s.push_str(c);
        }
    }
    s.push('}');
    s
}

fn target(r: &mut Renderer, t: &GraphTarget, graph_kw: bool) -> String {
    match t {
        GraphTarget::Default => "DEFAULT".into(),
        GraphTarget::Named => "NAMED".into(),
        GraphTarget::All => "ALL".into(),
        GraphTarget::Graph(g) => {
            let g = r.node(g, Pos::Term);
            if graph_kw { format!("GRAPH {g}") } else { g }
        }
    }
}

fn silent_kw(silent: bool) -> &'static str {
    if silent { " SILENT" } else { "" }
}

fn render_op(r: &mut Renderer, op: &Op) -> String {
    match op {
        Op::InsertData(q) => format!("INSERT DATA {}", quads_block(r, q)),
        Op::DeleteData(q) => format!("DELETE DATA {}", quads_block(r, q)),
        Op::DeleteWhere(q) => format!("DELETE WHERE {}", quads_block(r, q)),
        Op::Modify {
            with,
            delete,
            insert,
            using,
            using_named,
            where_,
        } => {
            let mut lines = Vec::new();
            if let Some(g) = with {
                let g = r.node(g, Pos::Term);
                lines.push(format!("WITH {g}"));
            }
            if delete.is_empty() && insert.is_empty() {
                r.errors
                    .push("DELETE/INSERT operation without any template triple".into());
            }
            if !delete.is_empty() {
                lines.push(format!("DELETE {}", quads_block(r, delete)));
            }
            if !insert.is_empty() {
                lines.push(format!("INSERT {}", quads_block(r, insert)));
            }
            for g in using {
                let g = r.node(g, Pos::Term);
                lines.push(format!("USING {g}"));
            }
            for g in using_named {
                let g = r.node(g, Pos::Term);
                lines.push(format!("USING NAMED {g}"));
            }
            let w = r.group(where_);
            lines.push(format!("WHERE {w}"));
            lines.join("\n")
        }
        Op::Load {
            silent,
            source,
            into,
        } => {
            let src = r.node(source, Pos::Term);
            let mut s = format!("LOAD{} {src}", silent_kw(*silent));
            if let Some(g) = into {
                let g = r.node(g, Pos::Term);
                s.push_str(&format!(" INTO GRAPH {g}"));
            }
            s
        }
        Op::Clear(silent, t) => format!("CLEAR{} {}", silent_kw(*silent), target(r, t, true)),
        Op::Drop(silent, t) => format!("DROP{} {}", silent_kw(*silent), target(r, t, true)),
        Op::Create(silent, g) => {
            let g = r.node(g, Pos::Term);
            format!("CREATE{} GRAPH {g}", silent_kw(*silent))
        }
        Op::Transfer(kw, silent, from, to) => {
            for t in [from, to] {
                if matches!(t, GraphTarget::Named | GraphTarget::All) {
                    r.errors
                        .push(format!("{kw} only accepts DEFAULT or a graph IRI"));
                }
            }
            let f = target(r, from, false);
            let t = target(r, to, false);
            format!("{kw}{} {f} TO {t}", silent_kw(*silent))
        }
    }
}
