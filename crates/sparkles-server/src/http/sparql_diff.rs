//! `POST /{ds}/sparql/diff`: the rows a SELECT query returns at one state and not at
//! another, for results too large to compare in the browser (docs/specs/U01-time-slider.md).
//!
//! The query is the request body (`application/sparql-query`, or a form with `query`). `from`
//! and `to` take the selectors of `at`, `to` defaults to the head, and `branch` and
//! `reasoning` are as for `/{ds}/sparql`. Rows are compared as bags, as the UI does and as
//! C18 §6.7.5 describes: a row is its terms in order, and a row that occurs twice at one
//! state and once at the other differs once.
//!
//! The comparison keeps a 128-bit hash for each distinct row of the first result and no
//! row. The second result is read against those hashes, which finds the rows only it has
//! and keeps up to `limit` of them as they go by. The rows only the first result has are
//! found by running its query once more. That makes three executions, and the request
//! counts twice against the `query` rate limit.

use super::*;
use sparkles::history::At;
use sparkles::sparql::{QueryKind, QueryResult};
use std::collections::HashMap;
use std::fmt::Write as _;
use std::hash::{Hash, Hasher};

/// The rows listed for each side without `limit`.
const DEFAULT_LIMIT: usize = 100;
/// The most rows listed for each side.
const MAX_LIMIT: usize = 10_000;

fn bad(msg: impl Into<String>) -> ApiError {
    err(StatusCode::BAD_REQUEST, msg)
}

/// A 128-bit hash of a row: its terms in order, unbound cells apart from any term.
fn row_hash(r: &QueryResult, i: usize, buf: &mut String) -> u128 {
    buf.clear();
    for col in &r.table.cols {
        match r.term(col[i]) {
            Some(t) => {
                let _ = write!(buf, "{t}\u{1}");
            }
            None => buf.push('\u{2}'),
        }
    }
    let mut a = std::collections::hash_map::DefaultHasher::new();
    let mut b = std::collections::hash_map::DefaultHasher::new();
    0xA5u8.hash(&mut a);
    0x5Au8.hash(&mut b);
    buf.hash(&mut a);
    buf.hash(&mut b);
    (u128::from(a.finish()) << 64) | u128::from(b.finish())
}

/// The row as a JSON object of its bound variables, in SPARQL JSON terms.
fn row_json(r: &QueryResult, i: usize) -> J {
    let mut o = serde_json::Map::new();
    for (v, col) in r.vars.iter().zip(&r.table.cols) {
        if let Some(t) = r.term(col[i]) {
            o.insert(v.clone(), sparkles::sparql::results::term_json(&t));
        }
    }
    J::Object(o)
}

/// Run the query at `at` and check that it is a SELECT.
fn run(
    ds: &Dataset,
    at: &At,
    query: &str,
    opts: &sparkles::sparql::QueryOptions,
) -> ApiResult<(QueryResult, sparkles::history::Resolved)> {
    let (snap, resolved) = history::snapshot_for(ds, Some(at), opts)?;
    let r = sparkles::sparql::query_with_plan(snap, query, opts)
        .map_err(|f| ApiError::from(f.error))?;
    if r.kind != QueryKind::Select {
        return Err(bad("sparql/diff compares the rows of a SELECT query"));
    }
    let resolved = resolved.ok_or_else(|| bad("the state could not be resolved"))?;
    Ok((r, resolved))
}

pub(super) async fn sparql_diff(
    State(st): St,
    Path(name): Path<String>,
    Extension(p): Extension<Principal>,
    uri: Uri,
    headers: HeaderMap,
    QueryBody(body): QueryBody,
) -> ApiResult {
    let ds = dataset(&st, &name)?;
    let mut params = Params::from_query(&uri);
    let ct = content_type(&headers);
    let query = match ct.as_str() {
        "application/x-www-form-urlencoded" => {
            params.extend_form(&body);
            params.get("query").unwrap_or_default().to_string()
        }
        "application/json" => {
            let j: J = serde_json::from_slice(&body).map_err(|e| bad(format!("body: {e}")))?;
            for k in ["from", "to", "reasoning", "limit"] {
                if let Some(v) = j.get(k).filter(|v| !v.is_null()) {
                    let s = v
                        .as_str()
                        .map(str::to_string)
                        .unwrap_or_else(|| v.to_string());
                    params.0.push((k.to_string(), s));
                }
            }
            j.get("query")
                .and_then(J::as_str)
                .unwrap_or_default()
                .to_string()
        }
        _ => String::from_utf8_lossy(&body).into_owned(),
    };
    if query.trim().is_empty() {
        return Err(bad("missing query: send it as application/sparql-query"));
    }
    let from = super::diff::selector(&params, "from")?
        .ok_or_else(|| bad("from: required, a commit, time or snapshot as for at"))?;
    let to = super::diff::selector(&params, "to")?.unwrap_or(At::Head);
    let limit = match params.get("limit") {
        None => DEFAULT_LIMIT,
        Some(v) => match v.parse::<usize>() {
            Ok(n) if (1..=MAX_LIMIT).contains(&n) => n,
            _ => return Err(bad(format!("limit must be from 1 to {MAX_LIMIT}"))),
        },
    };
    let mut opts = query_options(&st, &ds, &params);
    crate::auth::restrict(&mut opts, &p, &ds.name, crate::auth::Endpoint::Query);
    let (cancel, _cancel_on_drop) = cancel_on_drop();
    opts.cancel = Some(cancel);
    let max_distinct = st.limits.max_rows;
    let restricted = p.restricted(&ds.name);
    let out = blocking({
        let ds = ds.clone();
        move || {
            let mut buf = String::new();
            // the earlier state: a hash for each distinct row, and how many of it there are
            let (left, from_at) = run(&ds, &from, &query, &opts)?;
            let vars = left.vars.clone();
            let left_rows = left.len() as u64;
            let mut seen: HashMap<u128, u32> = HashMap::new();
            for i in 0..left.len() {
                *seen.entry(row_hash(&left, i, &mut buf)).or_default() += 1;
                if max_distinct > 0 && seen.len() > max_distinct {
                    return Err(sparkles::Error::BudgetExceeded(sparkles::Budget {
                        kind: sparkles::BudgetKind::Rows,
                        limit: max_distinct as u64,
                        requested: seen.len() as u64,
                    })
                    .into());
                }
            }
            drop(left);
            // the later state: a row without a partner is only there, and kept as it goes by
            let (right, to_at) = run(&ds, &to, &query, &opts)?;
            let right_rows = right.len() as u64;
            let (mut same, mut added) = (0u64, 0u64);
            let mut added_rows = Vec::new();
            for i in 0..right.len() {
                match seen.get_mut(&row_hash(&right, i, &mut buf)) {
                    Some(n) if *n > 0 => {
                        *n -= 1;
                        same += 1;
                    }
                    _ => {
                        added += 1;
                        if added_rows.len() < limit {
                            added_rows.push(row_json(&right, i));
                        }
                    }
                }
            }
            drop(right);
            // what is left of the earlier hashes is only there: read that state again for the rows
            let removed: u64 = seen.values().map(|n| u64::from(*n)).sum();
            let mut removed_rows = Vec::new();
            if removed > 0 {
                let again = At::Commit(from_at.commit.seq);
                let (left, _) = run(&ds, &again, &query, &opts)?;
                for i in 0..left.len() {
                    if removed_rows.len() >= limit {
                        break;
                    }
                    if let Some(n) = seen.get_mut(&row_hash(&left, i, &mut buf))
                        && *n > 0
                    {
                        *n -= 1;
                        removed_rows.push(row_json(&left, i));
                    }
                }
            }
            let side = |r: &sparkles::history::Resolved| {
                let mut commit = json!(r.commit);
                if restricted {
                    redact_commit_json(&mut commit);
                }
                json!({ "selector": r.at.to_string(), "commit": commit })
            };
            Ok(json!({
                "dataset": ds.name,
                "datasetId": ds.store.owner_dataset_id(),
                "from": side(&from_at),
                "to": side(&to_at),
                "vars": vars,
                "fromRows": left_rows,
                "toRows": right_rows,
                "same": same,
                "added": added,
                "removed": removed,
                "addedRows": added_rows,
                "removedRows": removed_rows,
                "truncated": { "added": added > added_rows.len() as u64, "removed": removed > removed_rows.len() as u64 },
            }))
        }
    })
    .await?;
    Ok(Json(out).into_response())
}
