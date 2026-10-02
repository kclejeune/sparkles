//! `GET /{ds}/diff`: the net quads added and removed between two readable commits.
//!
//! `from` and `to` take the selectors of `at` (`head`, `N`, `commit:N`,
//! `time:<RFC 3339>`, `snapshot:NAME`). `to` defaults to the head and `from` to the
//! commit before `to`, so `?to=commit:42` shows what commit 42 changed. `graph=IRI` or
//! `default` limits the diff to one graph.
//!
//! The body is JSON (`application/json`, counts, and the quads with `quads=true`) or
//! N-Quads lines marked `+ ` or `- ` (`text/x-sparkles-diff`), removals first. Either is
//! streamed once it is large. A diff between two `commit:` selectors never changes, so
//! it gets a weak entity tag.

use super::*;
use sparkles::history::At;
use sparkles::store::{Diff, DiffOp, DiffOptions};
use std::io::Write;

/// The diff line format.
pub(super) const DIFF_MEDIA_TYPE: &str = "text/x-sparkles-diff";

fn code_err(status: StatusCode, code: &str, msg: impl Into<String>) -> ApiError {
    ApiError(status, json!({ "error": msg.into(), "code": code }))
}

/// One selector parameter (`None` when absent); given twice with different values is an
/// error.
fn selector(params: &Params, key: &str) -> ApiResult<Option<At>> {
    let vals = params.all(key);
    let Some(first) = vals.first() else {
        return Ok(None);
    };
    if vals.iter().any(|v| v != first) {
        return Err(code_err(
            StatusCode::BAD_REQUEST,
            "invalid-at",
            format!("{key} is given more than once with different values"),
        ));
    }
    first
        .parse::<At>()
        .map(Some)
        .map_err(|e| code_err(StatusCode::BAD_REQUEST, "invalid-at", format!("{key}: {e}")))
}

/// The graph a diff is limited to: `graph=IRI`, or `default` (also `graph=default`).
fn graph_param(params: &Params) -> ApiResult<Option<oxrdf::GraphName>> {
    if params.has("default") || params.get("graph") == Some("default") {
        return Ok(Some(oxrdf::GraphName::DefaultGraph));
    }
    match params.get("graph") {
        None => Ok(None),
        Some(iri) => oxrdf::NamedNode::new(iri)
            .map(|n| Some(oxrdf::GraphName::NamedNode(n)))
            .map_err(|e| err(StatusCode::BAD_REQUEST, format!("invalid graph IRI: {e}"))),
    }
}

fn side(r: &sparkles::history::Resolved) -> J {
    json!({
        "selector": r.at.to_string(),
        "commit": r.commit,
    })
}

fn quad_json(op: DiffOp, q: &oxrdf::Quad) -> J {
    let graph = match &q.graph_name {
        oxrdf::GraphName::DefaultGraph => J::Null,
        g => json!(g.to_string()),
    };
    json!({
        "op": op.sign().to_string(),
        "subject": q.subject.to_string(),
        "predicate": q.predicate.to_string(),
        "object": q.object.to_string(),
        "graph": graph,
    })
}

/// The JSON members of a diff besides its quads.
fn summary(name: &str, ds: &Dataset, d: &Diff) -> J {
    json!({
        "dataset": name,
        "datasetId": ds.store.dataset_id(),
        "from": side(&d.from),
        "to": side(&d.to),
        "added": d.added,
        "removed": d.removed,
        "method": d.method.as_str(),
        "logChanges": d.log_changes,
        "compared": d.compared,
    })
}

pub(super) async fn diff(
    State(st): St,
    Path(name): Path<String>,
    uri: Uri,
    headers: HeaderMap,
) -> ApiResult {
    let ds = dataset(&st, &name)?;
    let params = Params::from_query(&uri);
    let to = selector(&params, "to")?.unwrap_or(At::Head);
    let from = selector(&params, "from")?;
    let graph = graph_param(&params)?;
    let text = match params.get("format") {
        Some("diff" | "text") => true,
        Some("json") => false,
        Some(f) => {
            return Err(err(
                StatusCode::BAD_REQUEST,
                format!("unknown diff format '{f}': use json or diff"),
            ));
        }
        None => headers
            .get(header::ACCEPT)
            .and_then(|v| v.to_str().ok())
            .and_then(|a| negotiate(a, &["application/json", DIFF_MEDIA_TYPE, "text/plain"]))
            .is_some_and(|i| i > 0),
    };
    let with_quads = text || params.get("quads").is_some_and(truthy);
    let limit = match params.get("limit") {
        None => usize::MAX,
        Some(v) => v.parse().map_err(|_| {
            err(
                StatusCode::BAD_REQUEST,
                "limit must be a non-negative integer",
            )
        })?,
    };
    let opts = query_options(&st, &ds, &params);
    let o = DiffOptions {
        graph,
        max_quads: st.limits.max_rows as u64,
        cancel: opts.cancel.clone(),
        deadline: opts.timeout.map(|t| std::time::Instant::now() + t),
    };
    let d = blocking({
        let ds = ds.clone();
        move || {
            // without `from`: the commit before `to`
            let from = match from {
                Some(f) => f,
                None => {
                    let r = ds.store.resolve(&to)?;
                    At::Commit(r.commit.seq.saturating_sub(1))
                }
            };
            Ok(ds.store.diff(&from, &to, &o)?)
        }
    })
    .await?;
    let id = ds.store.dataset_id();
    let fixed = matches!(d.from.at, At::Commit(_)) && matches!(d.to.at, At::Commit(_));
    // everything that shapes the body: the commits, the format, the quads listed and the
    // graph
    let tag = format!(
        "W/\"{id}:{}..{}:{}{}{}\"",
        d.from.commit.seq,
        d.to.commit.seq,
        match (text, with_quads) {
            (true, _) => "diff",
            (false, true) => "json+quads",
            (false, false) => "json",
        },
        match params.get("limit") {
            Some(_) if with_quads => format!(":limit={limit}"),
            _ => String::new(),
        },
        match (&params.get("graph"), params.has("default")) {
            (Some(g), _) => format!(
                ":graph={}",
                form_urlencoded::byte_serialize(g.as_bytes()).collect::<String>()
            ),
            (None, true) => ":graph=default".to_string(),
            _ => String::new(),
        }
    );
    let summary = summary(&name, &ds, &d);
    let counts = [
        ("sparkles-diff-from", d.from.commit.seq),
        ("sparkles-diff-to", d.to.commit.seq),
        ("sparkles-diff-added", d.added),
        ("sparkles-diff-removed", d.removed),
    ];
    if fixed
        && headers
            .get(header::IF_NONE_MATCH)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v.split(',').any(|t| t.trim() == tag || t.trim() == "*"))
    {
        let mut r = StatusCode::NOT_MODIFIED.into_response();
        if let Ok(v) = header::HeaderValue::from_str(&tag) {
            r.headers_mut().insert(header::ETAG, v);
        }
        return Ok(r);
    }
    let d = Arc::new(d);
    let write = move |w: &mut LimitedWriter<stream::SwitchWriter>| -> sparkles::Result<()> {
        if text {
            for (op, q) in d.slice(0, limit) {
                writeln!(
                    w,
                    "{} {}",
                    op.sign(),
                    sparkles::annotations::nquads_line(&q)
                )?;
            }
            return Ok(());
        }
        // the summary, then the quads one by one
        let mut head = serde_json::to_vec(&summary).map_err(std::io::Error::other)?;
        if !with_quads {
            w.write_all(&head)?;
            return Ok(());
        }
        head.pop(); // the closing brace
        w.write_all(&head)?;
        w.write_all(b",\"quads\":[")?;
        for (i, (op, q)) in d.slice(0, limit).enumerate() {
            if i > 0 {
                w.write_all(b",")?;
            }
            serde_json::to_writer(&mut *w, &quad_json(op, &q)).map_err(std::io::Error::other)?;
        }
        w.write_all(b"]}")?;
        Ok(())
    };
    let ds_name = ds.name.clone();
    let st_weak = Arc::downgrade(&st);
    let body = stream::serialize(st.limits.max_export_bytes, write, move |end| {
        if let Some(st) = st_weak.upgrade() {
            st.metrics
                .add_response_bytes(Some(&ds_name), Op::Other, end.bytes);
        }
        stream_end_log("diff", &end);
    })
    .await?;
    let body = match body {
        stream::Serialized::Whole { body, .. } => axum::body::Body::from(body),
        stream::Serialized::Streamed(b) => b,
    };
    let ct = if text {
        "text/x-sparkles-diff; charset=utf-8"
    } else {
        "application/json"
    };
    let mut resp = ([(header::CONTENT_TYPE, ct)], body).into_response();
    let h = resp.headers_mut();
    for (k, v) in counts {
        h.insert(k, v.into());
    }
    if let Ok(v) = header::HeaderValue::from_str(&id.to_string()) {
        h.insert(SPARKLES_DATASET_ID, v);
    }
    if params.get("format").is_none() {
        h.append(header::VARY, header::HeaderValue::from_static("accept"));
    }
    if fixed && let Ok(v) = header::HeaderValue::from_str(&tag) {
        h.insert(header::ETAG, v);
    }
    Ok(resp)
}
