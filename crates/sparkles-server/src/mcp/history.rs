//! `list_changes`: the history query of `GET /{ds}/history` (F06 §11) as a tool. It
//! lists the recorded changes of a range of commits, each an addition or removal of a
//! quad with its commit, time, kind, author and message, filtered by subject,
//! predicate, object, graph and kind of change. It reads the change log, which reaches
//! further back than the states `at` can read, and it needs the grant of the `diff`
//! endpoint, as over HTTP. Changes in graphs or of triples the caller may not read are
//! left out.

use super::Outcome;
use super::errors::ToolError;
use super::render::{Prefixes, Terms};
use super::tools::{Tools, bounded, dataset_prefixes, parse, parse_iri};
use crate::auth::{Endpoint, Level};
use oxrdf::{GraphName, Term};
use serde::Deserialize;
use serde_json::{Map, Value, json};
use sparkles::history::At;
use sparkles::store::{DiffOp, HistoryBound, HistoryQuery, UnrecordedReason};

/// The most terms of each kind a call may filter by.
const MAX_TERMS: usize = 20;

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct ChangesArgs {
    dataset: Option<String>,
    subjects: Option<Vec<String>>,
    predicates: Option<Vec<String>>,
    objects: Option<Vec<String>>,
    graphs: Option<Vec<String>>,
    from: Option<Value>,
    to: Option<Value>,
    op: Option<Op>,
    order: Option<Order>,
    limit: Option<u64>,
    timeout_seconds: Option<f64>,
}

#[derive(Deserialize, Clone, Copy)]
#[serde(rename_all = "lowercase")]
enum Op {
    Add,
    Remove,
}

#[derive(Deserialize, Clone, Copy, PartialEq)]
#[serde(rename_all = "lowercase")]
enum Order {
    Asc,
    Desc,
}

/// A commit bound: a number or a selector string (`commit:N`, `time:…`, `snapshot:…`,
/// `head`).
fn bound(name: &str, v: &Value) -> Result<HistoryBound, ToolError> {
    let at = match v {
        Value::Number(n) => n.as_u64().map(At::Commit),
        Value::String(s) => s.trim().parse::<At>().ok(),
        _ => None,
    };
    at.map(HistoryBound::At).ok_or_else(|| {
        ToolError::bad_argument(format!(
            "{name} is a commit number, or a string: commit:N, time:<RFC 3339>, snapshot:<name> or head"
        ))
    })
}

impl Tools<'_> {
    pub(super) fn list_changes(&self, args: Map<String, Value>) -> Result<Outcome, ToolError> {
        let a: ChangesArgs = parse(args)?;
        let max = self.cfg().max_rows as u64;
        let limit = bounded("limit", a.limit, 100.min(max), 1, max)? as usize;
        let timeout = self.timeout(a.timeout_seconds)?;
        let ds = self.dataset(a.dataset.as_deref())?;
        let p = &self.call.principal;
        if !p.can_at(&ds.name, Endpoint::Diff, Level::Read) {
            return Err(ToolError::new(
                "forbidden",
                403,
                format!("the diff endpoint of /{} is not allowed", ds.name),
            ));
        }
        let prefix_map = dataset_prefixes(&ds);
        let prefixes = Prefixes::new(&prefix_map);
        let names = prefixes.names();
        let ctx = self.ctx(&names, timeout.as_secs_f64());
        let list = |name: &str, v: &Option<Vec<String>>| -> Result<Vec<String>, ToolError> {
            let v = v.clone().unwrap_or_default();
            if v.len() > MAX_TERMS {
                return Err(ToolError::bad_argument(format!(
                    "{name}: at most {MAX_TERMS} terms"
                )));
            }
            Ok(v)
        };
        let subjects = list("subjects", &a.subjects)?
            .iter()
            .map(|s| parse_iri(s, &prefix_map, true))
            .collect::<Result<Vec<_>, _>>()?;
        let predicates = list("predicates", &a.predicates)?
            .iter()
            .map(|s| match parse_iri(s, &prefix_map, false)? {
                Term::NamedNode(n) => Ok(n),
                _ => Err(ToolError::bad_argument("predicates must be IRIs")),
            })
            .collect::<Result<Vec<_>, _>>()?;
        // an object may also be a literal in N-Triples syntax
        let objects = list("objects", &a.objects)?
            .iter()
            .map(|s| {
                let s = s.trim();
                if s.starts_with('"') {
                    s.parse::<Term>().map_err(|e| {
                        ToolError::bad_argument(format!(
                            "invalid literal {s}: {e} (write it in N-Triples syntax, with a full datatype IRI)"
                        ))
                    })
                } else {
                    parse_iri(s, &prefix_map, true)
                }
            })
            .collect::<Result<Vec<_>, _>>()?;
        let graphs = list("graphs", &a.graphs)?
            .iter()
            .map(|g| match g.trim() {
                "default" => Ok(GraphName::DefaultGraph),
                g => match parse_iri(g, &prefix_map, false)? {
                    Term::NamedNode(n) => Ok(GraphName::NamedNode(n)),
                    _ => Err(ToolError::bad_argument(
                        "graphs are `default` or graph IRIs",
                    )),
                },
            })
            .collect::<Result<Vec<_>, _>>()?;
        let q = HistoryQuery {
            subjects,
            predicates,
            objects,
            graphs,
            from: a.from.as_ref().map(|v| bound("from", v)).transpose()?,
            to: a.to.as_ref().map(|v| bound("to", v)).transpose()?,
            op: a.op.map(|o| match o {
                Op::Add => DiffOp::Add,
                Op::Remove => DiffOp::Remove,
            }),
            limit,
            descending: a.order == Some(Order::Desc),
            access: p.view(&ds.name, Endpoint::Diff),
            cancel: Some(self.call.cancel.clone()),
            deadline: Some(self.call.arrived + timeout),
        };
        let r = ds.store.history_changes(&q).map_err(|e| match e {
            sparkles::Error::NotFound(m) => {
                ToolError::new("unknown-commit", 404, format!("{m} in dataset {}", ds.name))
            }
            sparkles::Error::HistoryUnsupported(m) => ToolError::new("unsupported", 501, m),
            e => ctx.engine(e),
        })?;
        let mut terms = Terms::new(&prefixes, 500);
        let changes: Vec<Value> = r
            .changes
            .iter()
            .map(|c| {
                let mut quad = format!(
                    "{} {} {}",
                    terms.term(&c.quad.subject.clone().into()),
                    terms.term(&Term::NamedNode(c.quad.predicate.clone())),
                    terms.term(&c.quad.object),
                );
                match &c.quad.graph_name {
                    GraphName::DefaultGraph => {}
                    GraphName::NamedNode(n) => {
                        quad.push(' ');
                        quad.push_str(&terms.term(&Term::NamedNode(n.clone())));
                    }
                    GraphName::BlankNode(b) => {
                        quad.push(' ');
                        quad.push_str(&terms.term(&Term::BlankNode(b.clone())));
                    }
                }
                let mut j = json!({
                    "commit": c.commit.seq,
                    "timestamp": c.commit.timestamp(),
                    "kind": c.commit.kind.name(),
                    "op": match c.op {
                        DiffOp::Add => "add",
                        DiffOp::Remove => "remove",
                    },
                    "quad": quad,
                });
                if let Some(a) = &c.commit.author {
                    j["author"] = json!(&**a);
                }
                if let Some(m) = &c.commit.message {
                    j["message"] = json!(&**m);
                }
                j
            })
            .collect();
        let unrecorded: Vec<Value> = r
            .unrecorded
            .iter()
            .map(|u| {
                json!({
                    "from": u.from,
                    "to": u.to,
                    "reason": match u.reason {
                        UnrecordedReason::BeforeLog => "before-log",
                        UnrecordedReason::Bulk => "bulk",
                        UnrecordedReason::Gap => "gap",
                    },
                })
            })
            .collect();
        Ok(Outcome::Structured(json!({
            "dataset": ds.name,
            "head": r.head,
            "from": r.from,
            "to": r.to,
            "changes": changes,
            "truncated": r.truncated,
            "unrecorded": unrecorded,
            "prefixes": terms.used(),
        })))
    }
}
