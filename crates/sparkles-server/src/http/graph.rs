//! Graph overview (spec U06 Phase 2): `POST /$/graph/{ds}/groups` answers with
//! [`sparkles::overview`], which groups the instances of
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

use super::{ApiResult, Params, QueryBody, St, blocking, dataset, err};
use crate::auth::{Endpoint, Principal};
use axum::Extension;
use axum::Json;
use axum::extract::Path;
use axum::http::{StatusCode, Uri};
use axum::response::IntoResponse;
use serde_json::Value as J;
use std::sync::Arc;
use std::time::Instant;

pub(super) mod cache;

use sparkles::overview::GroupsRequest;

/// The RDFS on read the queries use, for the cache key: a setting can change without a
/// commit. A fixed schema is named by a hash of its content, a graph source by its graph,
/// whose content the commit already names.
fn rdfs_key(rdfs: Option<&sparkles::sparql::rdfs::RdfsOnRead>) -> String {
    use sparkles::sparql::rdfs::RdfsSchema;
    use std::hash::{Hash, Hasher};
    match rdfs.map(|r| &r.source) {
        None => String::new(),
        Some(sparkles::sparql::rdfs::SchemaSource::Graph(g)) => format!("rdfs:graph:{g:?}"),
        Some(sparkles::sparql::rdfs::SchemaSource::Fixed(schema)) => {
            // the hash of a schema is computed once: a schema is immutable, and the weak
            // reference keeps its allocation, so an equal pointer is the same schema
            static MEMO: parking_lot::Mutex<Vec<(std::sync::Weak<RdfsSchema>, String)>> =
                parking_lot::const_mutex(Vec::new());
            let mut memo = MEMO.lock();
            memo.retain(|(w, _)| w.strong_count() > 0);
            if let Some((_, k)) = memo
                .iter()
                .find(|(w, _)| std::ptr::eq(w.as_ptr(), Arc::as_ptr(schema)))
            {
                return k.clone();
            }
            let mut h = std::collections::hash_map::DefaultHasher::new();
            format!("{schema:?}").hash(&mut h);
            let key = format!("rdfs:fixed:{:016x}", h.finish());
            if memo.len() >= 16 {
                memo.remove(0);
            }
            memo.push((Arc::downgrade(schema), key.clone()));
            key
        }
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
    let j: J = serde_json::from_slice(&body)
        .map_err(|e| err(StatusCode::BAD_REQUEST, format!("invalid JSON: {e}")))?;
    let mut req = GroupsRequest::from_json(&j)?;
    if req.reads_reports() {
        // the reports belong to main (spec C22 §6.5), and the settings name their graph
        req.check_branch(&ds.store.branch_name())?;
        let settings: crate::graphrag::settings::GraphragSettings =
            crate::settings::effective(&st, &ds, &crate::settings::GRAPHRAG);
        req.default_reports_graph(&settings.reports_graph);
    }
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
                "{:?}|{:?}|{:?}|{}",
                opts.default_graph_uris,
                opts.named_graph_uris,
                opts.default_graph_extra,
                rdfs_key(opts.rdfs.as_deref())
            ),
            request: req.key(),
        };
        if let Some(hit) = cache::get(&key) {
            return Ok(Json((*hit).clone()).into_response());
        }
        let answer = sparkles::overview::groups_at(snap, &req, &opts)
            .map_err(|e| super::with_timeout(e.into(), Some(timeout)))?;
        tracing::debug!("/{}: groups answered in {:?}", ds.name, started.elapsed());
        let answer = Arc::new(answer);
        cache::put(key, answer.clone());
        Ok(Json((*answer).clone()).into_response())
    })
    .await
}

#[cfg(test)]
mod tests;
