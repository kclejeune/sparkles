//! `GET /$/commit-graph/{ds}`: the commits of several branches with their parents, newest
//! first, one page at a time, for the History panel's graph view
//! ([F09](../../../../docs/specs/F09-branches-and-merges.md) §2.9).

use super::branches::{check, commit_ref, main_dataset};
use super::*;
use crate::auth::{Endpoint, on_branch};
use sparkles::store::{CommitGraphOptions, GraphCursor};

/// Commits per page by default, and at most.
const DEFAULT_LIMIT: usize = 100;
const MAX_LIMIT: usize = 1000;

fn bad(msg: impl Into<String>) -> ApiError {
    ApiError(
        StatusCode::BAD_REQUEST,
        json!({ "error": msg.into(), "code": "bad-request" }),
    )
}

/// `GET /$/commit-graph/{ds}?branches=&before=&limit=`. `branches` names the branches to
/// draw, comma-separated or repeated, and defaults to every branch the caller may read.
/// Each commit names the branch that made it and its parents. A caller whose grants
/// cover some graphs only sees the commits without their counts, as in `/$/commits`.
pub(crate) async fn get(
    State(st): St,
    Path(name): Path<String>,
    Extension(p): Extension<Principal>,
    uri: Uri,
) -> ApiResult {
    let ds = main_dataset(&st, &name)?;
    let params = Params::from_query(&uri);
    let limit = match params.get("limit") {
        None => DEFAULT_LIMIT,
        Some(v) => v
            .parse::<usize>()
            .ok()
            .filter(|n| (1..=MAX_LIMIT).contains(n))
            .ok_or_else(|| bad(format!("limit: 1 to {MAX_LIMIT}, not {v}")))?,
    };
    let before = match params.get("before") {
        None | Some("") => None,
        Some(v) => Some(v.parse::<GraphCursor>()?),
    };
    let asked: Vec<String> = params
        .all("branches")
        .iter()
        .flat_map(|v| v.split(','))
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect();
    let readable = |b: &str| p.level_at(&on_branch(&name, b), Endpoint::Info).is_some();
    let branches = if asked.is_empty() {
        let d = ds.clone();
        let all = blocking(move || Ok(d.store.branches()?)).await?;
        all.into_iter()
            .map(|b| b.name)
            .filter(|b| readable(b))
            .collect::<Vec<_>>()
    } else {
        for b in &asked {
            check(&p, &name, b, Level::Read, Some(Endpoint::Info))?;
        }
        let mut v = asked.clone();
        v.dedup();
        v
    };
    let o = CommitGraphOptions {
        branches: Some(branches),
        before,
        limit,
    };
    let d = ds.clone();
    let g = blocking(move || Ok(d.store.commit_graph(&o)?)).await?;

    // what a point-in-time read can see on each branch, and the snapshots that pin commits
    let mut extras: std::collections::HashMap<String, (Vec<(u64, u64)>, Vec<(u64, String)>)> =
        Default::default();
    for b in &g.branches {
        if let Ok(bd) = st.branch_dataset(&ds, &b.name) {
            let h = bd.store.history();
            let pins = bd
                .store
                .snapshots()
                .into_iter()
                .map(|s| (s.seq, s.name))
                .collect();
            extras.insert(b.name.clone(), (h.reconstructable.clone(), pins));
        }
    }
    let branches: Vec<J> = g
        .branches
        .iter()
        .map(|b| {
            json!({
                "name": b.name,
                "id": b.id,
                "ordinal": b.ordinal,
                "head": b.head.seq,
                "modified": b.head.timestamp(),
                "from": b.from.as_ref().map(commit_ref),
                "upstream": b.upstream,
                "created": sparkles::commit::rfc3339_ms(b.created_ms),
            })
        })
        .collect();
    let commits: Vec<J> = g
        .commits
        .iter()
        .map(|c| {
            let mut j = json!(sparkles::commit::AnnotatedCommit {
                commit: &c.commit,
                annotation: c.annotation.as_ref(),
            });
            j["branch"] = json!(c.branch);
            j["branchId"] = json!(c.branch_id);
            j["parents"] = c.parents.iter().map(commit_ref).collect();
            if let Some(m) = &c.merged_from {
                j["mergedFrom"] = commit_ref(m);
            }
            if let Some((ranges, pins)) = extras.get(&c.branch) {
                let s = c.commit.seq;
                j["reconstructable"] = json!(ranges.iter().any(|&(a, b)| a <= s && s <= b));
                let names: Vec<&str> = pins
                    .iter()
                    .filter(|(q, _)| *q == s)
                    .map(|(_, n)| n.as_str())
                    .collect();
                if !names.is_empty() {
                    j["snapshots"] = json!(names);
                }
            }
            if p.restricted(&on_branch(&name, &c.branch)) {
                redact_commit_json(&mut j);
            }
            j
        })
        .collect();
    let next = g.next.map(|n| {
        let mut q = format!("/$/commit-graph/{name}?before={n}&limit={limit}");
        if !asked.is_empty() {
            q.push_str("&branches=");
            q.push_str(
                &percent_encoding::utf8_percent_encode(&asked.join(","), QUERY_VALUE).to_string(),
            );
        }
        q
    });
    Ok(Json(json!({
        "dataset": name,
        "datasetId": ds.store.dataset_id(),
        "branches": branches,
        "commits": commits,
        "next": next,
    }))
    .into_response())
}

/// Bytes escaped in a query value.
const QUERY_VALUE: &percent_encoding::AsciiSet = &percent_encoding::NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'_')
    .remove(b'.')
    .remove(b',');
