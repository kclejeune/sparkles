//! The graph overview (spec U06): the instances of a class, or of a group from an
//! earlier answer, grouped by the communities F14 wrote back, by a property's values or
//! ranges, or listed as members, with the counted edges between the groups, to their
//! sibling groups and to the other classes.
//!
//! Every count comes from SPARQL queries run with the caller's [`QueryOptions`], so the
//! view, the budgets, the deadline and the cancellation of `opts` apply to all of them
//! and an answer never holds what a query with the same options could not read. A group
//! id encodes the class and the partitions that lead to the group, so a later request
//! can name it without state. [`Dataset::graph`](crate::Dataset::graph) is the entry
//! point, and [`groups_at`] answers at a snapshot the caller already holds.

use crate::error::{Error, Result};
use crate::history::At;
use crate::sparql::QueryOptions;
use crate::store::Snapshot;
use serde_json::Value as J;
use std::sync::Arc;

mod groups;
mod ids;

pub use groups::{DEFAULT_BUCKETS, MAX_BUCKETS, MAX_EDGES, MAX_LIMIT};
pub use ids::MAX_DEPTH;

/// What a request groups and how.
#[derive(Clone, Debug, PartialEq)]
pub struct GroupsRequest {
    pub within: Within,
    pub by: By,
    pub limit: usize,
    pub at: Option<At>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Within {
    Class(String),
    Group(String),
}

#[derive(Clone, Debug, PartialEq)]
pub enum By {
    /// communities if F14 wrote some for the set, else the best property, else members
    Auto,
    Members,
    Community(String),
    Property {
        predicate: String,
        buckets: usize,
    },
}

fn bad(msg: impl Into<String>) -> Error {
    Error::invalid(msg)
}

fn iri_field(v: &J, what: &str) -> Result<String> {
    let s = v
        .as_str()
        .ok_or_else(|| bad(format!("{what} must be an IRI string")))?;
    let s = s
        .strip_prefix('<')
        .and_then(|i| i.strip_suffix('>'))
        .unwrap_or(s);
    oxrdf::NamedNode::new(s).map_err(|e| bad(format!("{what}: invalid IRI '{s}': {e}")))?;
    Ok(s.to_string())
}

impl GroupsRequest {
    /// Parse and check a request body.
    pub fn from_json(j: &J) -> Result<GroupsRequest> {
        let o = j
            .as_object()
            .ok_or_else(|| bad("the request must be a JSON object"))?;
        for k in o.keys() {
            if !matches!(k.as_str(), "within" | "by" | "limit" | "at") {
                return Err(bad(if k == "branch" {
                    "name the branch with ?branch=, as on the other routes".to_string()
                } else {
                    format!("unknown member '{k}': within, by, limit or at")
                }));
            }
        }
        let within = match o.get("within") {
            Some(J::Object(w)) if w.len() == 1 && w.contains_key("class") => {
                Within::Class(iri_field(&w["class"], "within.class")?)
            }
            Some(J::Object(w)) if w.len() == 1 && w.contains_key("group") => {
                let g = w["group"]
                    .as_str()
                    .ok_or_else(|| bad("within.group must be a group id string"))?;
                Within::Group(g.to_string())
            }
            _ => {
                return Err(bad(
                    "within must be {\"class\": IRI} or {\"group\": id from an earlier answer}",
                ));
            }
        };
        let by = match o.get("by") {
            None | Some(J::Null) => By::Auto,
            Some(J::String(s)) if s == "auto" => By::Auto,
            Some(J::String(s)) if s == "members" => By::Members,
            Some(J::Object(b)) if b.len() == 1 && b.contains_key("community") => {
                By::Community(iri_field(&b["community"], "by.community")?)
            }
            Some(J::Object(b))
                if b.contains_key("property")
                    && b.keys().all(|k| k == "property" || k == "buckets") =>
            {
                let buckets = match b.get("buckets") {
                    None | Some(J::Null) => DEFAULT_BUCKETS,
                    Some(v) => v
                        .as_u64()
                        .filter(|n| (2..=MAX_BUCKETS as u64).contains(n))
                        .ok_or_else(|| {
                            bad(format!(
                                "by.buckets must be an integer from 2 to {MAX_BUCKETS}"
                            ))
                        })? as usize,
                };
                By::Property {
                    predicate: iri_field(&b["property"], "by.property")?,
                    buckets,
                }
            }
            _ => {
                return Err(bad(
                    "by must be \"auto\", \"members\", {\"community\": IRI} or {\"property\": IRI, \"buckets\": n}",
                ));
            }
        };
        let limit = match o.get("limit") {
            None | Some(J::Null) => MAX_LIMIT,
            Some(v) => v
                .as_u64()
                .filter(|n| (1..=MAX_LIMIT as u64).contains(n))
                .ok_or_else(|| bad(format!("limit must be an integer from 1 to {MAX_LIMIT}")))?
                as usize,
        };
        let at = match o.get("at") {
            None | Some(J::Null) => None,
            Some(J::String(s)) => Some(s.parse::<At>().map_err(|e| bad(format!("at: {e}")))?)
                .filter(|a| *a != At::Head),
            Some(_) => return Err(bad("at must be a string: a commit, a time or a snapshot")),
        };
        Ok(GroupsRequest {
            within,
            by,
            limit,
            at,
        })
    }

    /// The request as a cache key: every member, in a fixed order.
    pub fn key(&self) -> String {
        format!("{self:?}")
    }
}

/// The answer to `req` at snapshot `snap`, as the JSON object that API.md describes
/// under "Graph overview", with the snapshot's `commit`. `req.at` is not read: the
/// caller chose the snapshot.
pub fn groups_at(snap: Arc<Snapshot>, req: &GroupsRequest, opts: &QueryOptions) -> Result<J> {
    let commit = snap.commit;
    let mut answer = groups::Run { snap, opts }.answer(req)?;
    if let Some(o) = answer.as_object_mut() {
        o.insert("commit".into(), commit.into());
    }
    Ok(answer)
}
