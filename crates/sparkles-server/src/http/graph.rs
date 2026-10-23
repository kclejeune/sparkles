//! Graph overview (spec U06 Phase 2): `POST /$/graph/{ds}/groups` groups the instances of
//! a class, or of a group from an earlier answer, by community, by a property's values or
//! into the members themselves, and counts the edges between the groups, to the sibling
//! groups and to the other classes.
//!
//! Every count comes from SPARQL queries run under the caller's view through the query
//! endpoint, with the request's memory grant, its deadline and the cancellation of a
//! client that goes away, so an answer never holds a class, a count or a member that a
//! SPARQL query of the same caller could not read. A group id encodes the class and the
//! partitions that lead to the group, so a later request can name it without server
//! state. Answers are cached per branch, commit, view and request in a small bounded
//! cache.
//!
//! Authorization (the route table in `auth/routes.rs`): `read` on `{ds}` through the
//! query endpoint.

use super::{ApiError, ApiResult, Params, QueryBody, St, blocking, dataset, err};
use crate::auth::{Endpoint, Principal};
use axum::Extension;
use axum::Json;
use axum::extract::Path;
use axum::http::{StatusCode, Uri};
use axum::response::IntoResponse;
use serde_json::Value as J;
use sparkles::history::At;
use std::sync::Arc;
use std::time::Instant;

pub(super) mod cache;
mod groups;
mod ids;

use groups::{DEFAULT_BUCKETS, MAX_BUCKETS, MAX_LIMIT};

/// What a request groups and how.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct GroupsRequest {
    pub within: Within,
    pub by: By,
    pub limit: usize,
    pub at: Option<At>,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Within {
    Class(String),
    Group(String),
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum By {
    /// communities if F14 wrote some for the set, else the best property, else members
    Auto,
    Members,
    Community(String),
    Property {
        predicate: String,
        buckets: usize,
    },
}

fn bad(msg: impl Into<String>) -> ApiError {
    err(StatusCode::BAD_REQUEST, msg)
}

fn iri_field(v: &J, what: &str) -> ApiResult<String> {
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
    pub(crate) fn from_json(j: &J) -> ApiResult<GroupsRequest> {
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
    fn key(&self) -> String {
        format!("{self:?}")
    }
}

/// `POST /$/graph/{ds}/groups`
pub(super) async fn groups(
    st: St,
    Path(name): Path<String>,
    Extension(p): Extension<Principal>,
    uri: Uri,
    QueryBody(body): QueryBody,
) -> ApiResult {
    let ds = dataset(&st, &name)?;
    let j: J = serde_json::from_slice(&body).map_err(|e| bad(format!("invalid JSON: {e}")))?;
    let req = GroupsRequest::from_json(&j)?;
    let params = Params::from_query(&uri);
    let mut opts = super::query_options(&st, &ds, &params);
    crate::auth::restrict(&mut opts, &p, &ds.name, Endpoint::Query);
    // one deadline for every query of the answer
    let timeout = opts.timeout.unwrap_or(st.default_timeout);
    opts.deadline = Some(Instant::now() + timeout);
    // a client that disconnects drops this future: the flag stops the query running
    let (cancel, _cancel_on_drop) = super::cancel_on_drop();
    opts.cancel = Some(cancel);
    blocking(move || {
        let started = Instant::now();
        let (snap, _) = super::history::snapshot_for(&ds, req.at.as_ref(), &opts)?;
        let key = cache::Key {
            branch: ds.store.branch_id(),
            commit: snap.commit,
            view: cache::view_key(opts.graphs.as_ref()),
            // the dataset a request names with default-graph-uri and named-graph-uri, and
            // the inferred overlay the dataset adds to the default graph
            extra: format!(
                "{:?}|{:?}|{:?}",
                opts.default_graph_uris, opts.named_graph_uris, opts.default_graph_extra
            ),
            request: req.key(),
        };
        if let Some(hit) = cache::get(&key) {
            return Ok(Json((*hit).clone()).into_response());
        }
        let run = groups::Run {
            snap: snap.clone(),
            opts: &opts,
        };
        let mut answer = run
            .answer(&req)
            .map_err(|e| super::with_timeout(e, Some(timeout)))?;
        if let Some(o) = answer.as_object_mut() {
            o.insert("commit".into(), snap.commit.into());
        }
        tracing::debug!("/{}: groups answered in {:?}", ds.name, started.elapsed());
        let answer = Arc::new(answer);
        cache::put(key, answer.clone());
        Ok(Json((*answer).clone()).into_response())
    })
    .await
}

#[cfg(test)]
mod tests;
