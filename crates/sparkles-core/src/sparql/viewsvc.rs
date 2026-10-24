//! `SERVICE view:read`: the rows of a `table` view (spec F13 §2.2).
//!
//! ```sparql
//! PREFIX view: <urn:x-sparkles:view#>
//! SELECT ?team ?members WHERE {
//!   SERVICE view:read { [] view:name "team-sizes" ; view:bind ?team, ?members . }
//! }
//! ```
//!
//! `view:bind` lists variables, each a column of the view by name. The rows come with
//! their multiplicities. `view:commit ?c` binds the commit the rows reflect, and
//! `view:allowStale true` reads the latest state without waiting for the view to reach
//! the query's commit. The views themselves come from the query options
//! ([`QueryOptions::views`](super::QueryOptions::views)).

use super::ctx::Ctx;
use super::plan::{Kind, Node, Planner};
use super::table::{Table, VarId};
use crate::access::GraphAccess;
use crate::error::{Error, Result};
use crate::id::Id;
use oxrdf::Term;
use spargebra::algebra::GraphPattern;
use spargebra::term::{NamedNodePattern, TermPattern};
use std::sync::Arc;
use std::time::Instant;

/// The view vocabulary.
pub const NS: &str = "urn:x-sparkles:view#";
/// The IRI of the service.
pub const READ: &str = "urn:x-sparkles:view#read";

pub fn is_read(name: &NamedNodePattern) -> bool {
    matches!(name, NamedNodePattern::NamedNode(n) if n.as_str() == READ)
}

/// A read of a view.
pub struct ViewRead<'a> {
    pub name: &'a str,
    /// the commit of the query's snapshot
    pub at: u64,
    /// the query's snapshot, to evaluate the view at when it has moved past `at` and
    /// its history does not reach back
    pub snap: Option<&'a Arc<crate::store::Snapshot>>,
    pub allow_stale: bool,
    /// the reader's view of the dataset (`None`: every graph)
    pub caller: Option<&'a Arc<GraphAccess>>,
    pub deadline: Option<Instant>,
}

/// The rows of a view.
pub struct ViewRows {
    pub columns: Vec<String>,
    /// each row's terms in the order of `columns`, with its multiplicity
    pub rows: Vec<(Vec<Option<Term>>, i64)>,
    /// the commit the rows reflect
    pub commit: u64,
}

/// What answers `SERVICE view:read`: a dataset's views.
pub trait ViewSource: Send + Sync + std::fmt::Debug {
    fn read_view(&self, r: &ViewRead<'_>) -> Result<ViewRows>;
}

#[derive(Clone, Debug)]
pub struct ViewSpec {
    pub name: String,
    /// each bound variable and the view column it reads
    pub bind: Vec<(String, VarId)>,
    pub commit: Option<VarId>,
    pub allow_stale: bool,
}

fn bad(msg: impl Into<String>) -> Error {
    Error::invalid(format!("SERVICE view:read: {}", msg.into()))
}

/// Plan `SERVICE view:read { … }`.
pub(super) fn view_leaf(p: &Planner<'_>, inner: &GraphPattern) -> Result<Node> {
    let GraphPattern::Bgp { patterns } = inner else {
        return Err(bad("the block holds view: properties of one subject"));
    };
    let mut spec = ViewSpec {
        name: String::new(),
        bind: Vec::new(),
        commit: None,
        allow_stale: false,
    };
    let mut subject: Option<&TermPattern> = None;
    for t in patterns {
        if subject.is_some_and(|s| s != &t.subject) {
            return Err(bad("every property belongs to one subject"));
        }
        subject = Some(&t.subject);
        let NamedNodePattern::NamedNode(pred) = &t.predicate else {
            return Err(bad("a property cannot be a variable"));
        };
        let Some(local) = pred.as_str().strip_prefix(NS) else {
            return Err(bad(format!("unknown property {pred}")));
        };
        match local {
            "name" => match &t.object {
                TermPattern::Literal(l) => spec.name = l.value().to_string(),
                _ => return Err(bad("view:name takes a string")),
            },
            "bind" => match &t.object {
                TermPattern::Variable(v) => spec
                    .bind
                    .push((v.as_str().to_string(), p.ctx.var(v.as_str()))),
                _ => return Err(bad("view:bind takes variables")),
            },
            "commit" => match &t.object {
                TermPattern::Variable(v) => spec.commit = Some(p.ctx.var(v.as_str())),
                _ => return Err(bad("view:commit takes a variable")),
            },
            "allowStale" => match &t.object {
                TermPattern::Literal(l) => {
                    spec.allow_stale = l.value() == "true" || l.value() == "1"
                }
                _ => return Err(bad("view:allowStale takes a boolean")),
            },
            other => return Err(bad(format!("unknown property view:{other}"))),
        }
    }
    if spec.name.is_empty() {
        return Err(bad("view:name is required"));
    }
    let mut vars: Vec<VarId> = spec.bind.iter().map(|b| b.1).collect();
    if let Some(c) = spec.commit {
        vars.push(c);
    }
    vars.dedup();
    let desc = format!("view {:?}", spec.name);
    let mut n = Node::leaf(Kind::ViewRead(Box::new(spec)), vars, 1000.0, desc);
    // a view's columns can be unbound
    n.certain.clear();
    Ok(n)
}

/// Read the view's rows into a table of `vars`.
pub(super) fn run(ctx: &Ctx, spec: &ViewSpec, vars: &[VarId]) -> Result<Table> {
    let Some(source) = ctx.views.as_ref() else {
        return Err(crate::incr::error(
            "view-not-found",
            format!("no view {:?}: views are not available here", spec.name),
        ));
    };
    let rows = source.read_view(&ViewRead {
        name: &spec.name,
        at: ctx.snap.commit,
        snap: Some(&ctx.snap),
        allow_stale: spec.allow_stale,
        caller: ctx.graphs.as_ref(),
        deadline: ctx.current_deadline(),
    })?;
    let cols: Vec<Option<usize>> = vars
        .iter()
        .map(|v| {
            spec.bind
                .iter()
                .find(|b| b.1 == *v)
                .map(|b| rows.columns.iter().position(|c| *c == b.0))
                .unwrap_or(None)
        })
        .collect();
    for (name, _) in &spec.bind {
        if !rows.columns.iter().any(|c| c == name) {
            return Err(bad(format!(
                "the view {:?} has no column ?{name} (its columns: {})",
                spec.name,
                rows.columns
                    .iter()
                    .map(|c| format!("?{c}"))
                    .collect::<Vec<_>>()
                    .join(" ")
            )));
        }
    }
    let commit = Id::from_i64(rows.commit as i64).unwrap_or(Id::UNDEF);
    let mut t = Table::new(vars.to_vec());
    for (row, n) in &rows.rows {
        ctx.check()?;
        let ids: Vec<Id> = vars
            .iter()
            .zip(&cols)
            .map(|(v, c)| match c {
                Some(i) => row[*i].as_ref().map_or(Id::UNDEF, |t| ctx.intern_term(t)),
                None if Some(*v) == spec.commit => commit,
                None => Id::UNDEF,
            })
            .collect();
        for _ in 0..(*n).max(0) {
            t.push_row(&ids);
        }
    }
    ctx.produced(t.len())?;
    Ok(t)
}
