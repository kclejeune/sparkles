//! `GET /{ds}/history`: the recorded changes of a range of commits, from the change log.
//!
//! `subject`, `predicate` and `object` take N-Triples terms (`<iri>`, `_:b1`,
//! `"text"@en`; a bare IRI is read as `<iri>`) and may repeat; `graph` takes an IRI or
//! `default`. `from` and `to` take the selectors of `at` and default to the first
//! commit and the head. `op=add|remove` keeps one kind of change, `order=desc` lists the
//! newest commits first, and `limit` (default 1,000, at most `--max-rows`) caps the
//! changes listed. The body is JSON, with the commits whose changes are not recorded in
//! `unrecorded`.

use super::*;
use sparkles::store::{DiffOp, HistoryBound, HistoryQuery, UnrecordedReason};

/// The changes listed without `limit`.
const DEFAULT_LIMIT: usize = 1_000;

fn bad(msg: impl Into<String>) -> ApiError {
    err(StatusCode::BAD_REQUEST, msg)
}

/// A term parameter: N-Triples syntax, or a bare IRI.
fn term_param(key: &str, v: &str) -> ApiResult<oxrdf::Term> {
    let v = v.trim();
    let parsed = if v.starts_with('<') || v.starts_with('"') || v.starts_with("_:") {
        v.parse::<oxrdf::Term>().map_err(|e| e.to_string())
    } else {
        oxrdf::NamedNode::new(v)
            .map(oxrdf::Term::NamedNode)
            .map_err(|e| e.to_string())
    };
    parsed.map_err(|e| bad(format!("{key}: invalid term {v:?}: {e}")))
}

fn reason(r: UnrecordedReason) -> &'static str {
    match r {
        UnrecordedReason::BeforeLog => "before-log",
        UnrecordedReason::Bulk => "bulk",
        UnrecordedReason::Gap => "gap",
    }
}

pub(super) async fn history(
    State(st): St,
    Path(name): Path<String>,
    Extension(p): Extension<Principal>,
    uri: Uri,
) -> ApiResult<Json<J>> {
    let ds = dataset(&st, &name)?;
    // the changes of the graphs and triples the caller may read
    let view = p.view(&ds.name, crate::auth::Endpoint::Diff);
    let params = Params::from_query(&uri);
    let terms = |key: &str| -> ApiResult<Vec<oxrdf::Term>> {
        params.all(key).iter().map(|v| term_param(key, v)).collect()
    };
    let subjects = terms("subject")?;
    let objects = terms("object")?;
    let predicates = terms("predicate")?
        .into_iter()
        .map(|t| match t {
            oxrdf::Term::NamedNode(n) => Ok(n),
            t => Err(bad(format!("predicate: {t} is not an IRI"))),
        })
        .collect::<ApiResult<Vec<_>>>()?;
    let graphs = params
        .all("graph")
        .iter()
        .map(|g| {
            if g == "default" {
                Ok(oxrdf::GraphName::DefaultGraph)
            } else {
                match term_param("graph", g)? {
                    oxrdf::Term::NamedNode(n) => Ok(oxrdf::GraphName::NamedNode(n)),
                    oxrdf::Term::BlankNode(b) => Ok(oxrdf::GraphName::BlankNode(b)),
                    t => Err(bad(format!("graph: {t} is not a graph name"))),
                }
            }
        })
        .collect::<ApiResult<Vec<_>>>()?;
    let from = super::diff::selector(&params, "from")?.map(HistoryBound::At);
    let to = super::diff::selector(&params, "to")?.map(HistoryBound::At);
    let op = match params.get("op") {
        None => None,
        Some("add" | "+") => Some(DiffOp::Add),
        Some("remove" | "-") => Some(DiffOp::Remove),
        Some(o) => return Err(bad(format!("op must be add or remove, not {o:?}"))),
    };
    let descending = match params.get("order") {
        None | Some("asc") => false,
        Some("desc") => true,
        Some(o) => return Err(bad(format!("order must be asc or desc, not {o:?}"))),
    };
    let max = st.limits.max_rows;
    let limit = match params.get("limit") {
        None => DEFAULT_LIMIT.min(max),
        Some(v) => v
            .parse::<usize>()
            .ok()
            .filter(|&n| n >= 1 && n <= max)
            .ok_or_else(|| bad(format!("limit must be between 1 and {max}")))?,
    };
    let opts = query_options(&st, &ds, &params);
    let q = HistoryQuery {
        subjects,
        predicates,
        objects,
        graphs,
        from,
        to,
        op,
        limit,
        descending,
        access: view,
        cancel: opts.cancel.clone(),
        deadline: opts.timeout.map(|t| std::time::Instant::now() + t),
    };
    let r = blocking({
        let ds = ds.clone();
        move || Ok(ds.dataset.history().query(&q)?)
    })
    .await?;
    let changes: Vec<J> = r
        .changes
        .iter()
        .map(|c| {
            let mut j = super::diff::quad_json(c.op, &c.quad);
            j["op"] = json!(match c.op {
                DiffOp::Add => "add",
                DiffOp::Remove => "remove",
            });
            j["commit"] = json!(c.commit.seq);
            j["timestamp"] = json!(c.commit.timestamp());
            j["kind"] = json!(c.commit.kind.name());
            if let Some(a) = &c.commit.author {
                j["author"] = json!(&**a);
            }
            if let Some(m) = &c.commit.message {
                j["message"] = json!(&**m);
            }
            j
        })
        .collect();
    Ok(Json(json!({
        "dataset": name,
        "datasetId": ds.store.owner_dataset_id(),
        "head": r.head,
        "from": r.from,
        "to": r.to,
        "truncated": r.truncated,
        "changes": changes,
        "unrecorded": r.unrecorded.iter().map(|u| json!({
            "from": u.from,
            "to": u.to,
            "reason": reason(u.reason),
        })).collect::<Vec<_>>(),
    })))
}
