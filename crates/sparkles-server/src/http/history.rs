//! Point-in-time reads (`?at=`) and the named snapshot and history endpoints.

use super::*;
use sparkles::history::{At, HistoryOptions, NamedSnapshot, Resolved, Retention};
use sparkles::store::Snapshot;

pub(super) const SPARKLES_AT: &str = "sparkles-at";
pub(super) const SPARKLES_HEAD: &str = "sparkles-head";

fn code_err(status: StatusCode, code: &str, msg: impl Into<String>) -> ApiError {
    ApiError(status, json!({ "error": msg.into(), "code": code }))
}

/// The `at` selector of a read (`None`: the head). Repeated with different values
/// (query string and form body) is an error.
pub(super) fn at_param(params: &Params) -> ApiResult<Option<At>> {
    let vals: Vec<&str> = params
        .0
        .iter()
        .filter(|(k, _)| k == "at")
        .map(|(_, v)| v.as_str())
        .collect();
    let Some(first) = vals.first() else {
        return Ok(None);
    };
    if vals.iter().any(|v| v != first) {
        return Err(code_err(
            StatusCode::BAD_REQUEST,
            "invalid-at",
            "at is given more than once with different values",
        ));
    }
    first
        .parse::<At>()
        .map(Some)
        .map_err(|e| code_err(StatusCode::BAD_REQUEST, "invalid-at", e.to_string()))
}

/// Writes always apply to the head; `at` on one is a mistake worth reporting.
pub(super) fn reject_at(params: &Params) -> ApiResult<()> {
    match params.get("at") {
        Some(a) => Err(code_err(
            StatusCode::BAD_REQUEST,
            "at-on-write",
            format!("writes cannot target a past state (at={a})"),
        )),
        None => Ok(()),
    }
}

/// The snapshot a read uses: the live one, or the state `at` selects.
pub(super) fn snapshot_for(
    ds: &Dataset,
    at: Option<&At>,
    opts: &QueryOptions,
) -> sparkles::Result<(Arc<Snapshot>, Option<Resolved>)> {
    let Some(at) = at else {
        return Ok((ds.store.snapshot(), None));
    };
    let o = HistoryOptions {
        cancel: opts.cancel.clone(),
        deadline: opts.timeout.map(|t| std::time::Instant::now() + t),
    };
    let (snap, r) = ds.store.snapshot_at(at, &o)?;
    Ok((snap, Some(r)))
}

/// `Sparkles-At`, `Sparkles-Head`, and for a past state the Memento headers:
/// `Memento-Datetime` and `Link: <original>; rel="original"` (RFC 7089).
pub(super) fn history_headers(mut resp: Response, r: Option<&Resolved>, uri: &Uri) -> Response {
    let Some(r) = r else { return resp };
    let h = resp.headers_mut();
    if let Ok(v) = header::HeaderValue::from_str(&r.at.to_string()) {
        h.insert(SPARKLES_AT, v);
    }
    h.insert(SPARKLES_HEAD, r.head.into());
    if r.historical {
        let t = std::time::UNIX_EPOCH
            + std::time::Duration::from_millis(r.commit.timestamp_ms.max(0) as u64);
        if let Ok(v) = header::HeaderValue::from_str(&httpdate::fmt_http_date(t)) {
            h.insert("memento-datetime", v);
        }
        let query: Vec<String> = uri
            .query()
            .unwrap_or("")
            .split('&')
            .filter(|kv| !kv.is_empty() && kv.split('=').next() != Some("at"))
            .map(str::to_string)
            .collect();
        let original = if query.is_empty() {
            uri.path().to_string()
        } else {
            format!("{}?{}", uri.path(), query.join("&"))
        };
        if let Ok(v) = header::HeaderValue::from_str(&format!("<{original}>; rel=\"original\"")) {
            h.insert(header::LINK, v);
        }
    }
    resp
}

/// The 410 body of a commit whose data is gone.
pub(super) fn gone_body(g: &sparkles::history::HistoryGone) -> J {
    let mut j = json!({
        "error": g.message,
        "code": "history-gone",
        "commit": g.seq,
        "head": g.head,
        "oldestReconstructable": g.reconstructable.first().map(|r| r.0),
        "reconstructable": ranges(&g.reconstructable),
        "metadata": g.metadata,
    });
    if let Some(s) = &g.snapshot {
        j["snapshot"] = json!(s);
    }
    j
}

fn ranges(r: &[(u64, u64)]) -> J {
    json!(
        r.iter()
            .map(|(a, b)| json!({ "from": a, "to": b }))
            .collect::<Vec<_>>()
    )
}

fn snapshot_json(s: &NamedSnapshot) -> J {
    json!({
        "name": s.name,
        "ref": format!("snapshot:{}", s.name),
        "seq": s.seq,
        "commit": s.commit,
        "created": sparkles::commit::rfc3339_ms(s.created_ms),
        "note": s.note,
        "generation": s.generation,
        "reconstructable": s.reconstructable,
    })
}

fn history_json(name: &str, ds: &Dataset, h: &sparkles::history::HistoryStatus) -> J {
    json!({
        "dataset": name,
        "datasetId": ds.store.dataset_id(),
        "head": h.head,
        "oldestReconstructable": h.oldest_reconstructable(),
        "reconstructable": ranges(&h.reconstructable),
        "bytes": h.bytes,
        "generations": h.generations.iter().map(|g| json!({
            "name": g.name,
            "baseSeq": g.base_seq,
            "endSeq": g.end_seq,
            "bytes": g.bytes,
            "current": g.current,
            "heldBy": g.held_by.iter().map(|h| h.to_string()).collect::<Vec<_>>(),
        })).collect::<Vec<_>>(),
        "retention": retention_json(h.retention),
        "snapshots": h.snapshots,
        "cache": {
            "entries": h.cache_entries,
            "bytes": h.cache_bytes,
            "hits": h.hits,
            "misses": h.misses,
            "materializations": h.materializations,
        },
    })
}

fn retention_json(r: Retention) -> J {
    json!({
        "keepCommits": r.keep_commits,
        "keepAge": r.keep_age_ms.map(|ms| format!("{}s", ms / 1000)),
    })
}

/// `"7d"`, `"12h"`, `"30m"`, `"90s"`, `"2w"`, or a number of seconds.
fn parse_age(v: &J) -> Option<u64> {
    match v {
        J::Number(n) => n.as_u64().map(|s| s * 1000),
        J::String(s) => {
            let (num, unit) = s.split_at(s.find(|c: char| !c.is_ascii_digit())?);
            let n: u64 = num.parse().ok()?;
            let secs = match unit {
                "s" => 1,
                "m" => 60,
                "h" => 3600,
                "d" => 86_400,
                "w" => 604_800,
                _ => return None,
            };
            Some(n * secs * 1000)
        }
        _ => None,
    }
}

pub(super) async fn list_snapshots(State(st): St, Path(name): Path<String>) -> ApiResult<Json<J>> {
    let ds = dataset(&st, &name)?;
    let snaps: Vec<J> = ds.store.snapshots().iter().map(snapshot_json).collect();
    Ok(Json(json!({
        "dataset": name,
        "datasetId": ds.store.dataset_id(),
        "head": ds.store.head_commit().seq,
        "snapshots": snaps,
    })))
}

pub(super) async fn create_snapshot(
    State(st): St,
    Path(name): Path<String>,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> ApiResult {
    if st.read_only {
        return Err(err(StatusCode::FORBIDDEN, "server is read-only"));
    }
    let ds = dataset(&st, &name)?;
    let mut params = Params::from_query(&uri);
    match content_type(&headers).as_str() {
        "application/json" => {
            let j: J = serde_json::from_slice(&body)
                .map_err(|e| err(StatusCode::BAD_REQUEST, format!("invalid JSON: {e}")))?;
            for k in ["name", "at", "note"] {
                if let Some(v) = j.get(k).and_then(J::as_str) {
                    params.0.push((k.to_string(), v.to_string()));
                }
            }
        }
        _ => params.extend_form(&body),
    }
    let snap_name = params
        .get("name")
        .ok_or_else(|| err(StatusCode::BAD_REQUEST, "missing 'name'"))?
        .to_string();
    let at = at_param(&params)?.unwrap_or(At::Head);
    let note = params.get("note").map(str::to_string);
    blocking(move || {
        let (snap, created) = ds.store.create_snapshot(&snap_name, &at, note)?;
        let status = if created {
            StatusCode::CREATED
        } else {
            StatusCode::OK
        };
        let mut resp = (status, Json(snapshot_json(&snap))).into_response();
        if created
            && let Ok(v) =
                header::HeaderValue::from_str(&format!("/$/snapshots/{}/{}", ds.name, snap_name))
        {
            resp.headers_mut().insert(header::LOCATION, v);
        }
        Ok(resp)
    })
    .await
}

pub(super) async fn get_snapshot(
    State(st): St,
    Path((name, snap)): Path<(String, String)>,
) -> ApiResult<Json<J>> {
    let ds = dataset(&st, &name)?;
    let s = ds.store.named_snapshot(&snap).ok_or_else(|| {
        code_err(
            StatusCode::NOT_FOUND,
            "no-such-snapshot",
            format!("no snapshot '{snap}' in dataset {name}"),
        )
    })?;
    Ok(Json(snapshot_json(&s)))
}

pub(super) async fn delete_snapshot(
    State(st): St,
    Path((name, snap)): Path<(String, String)>,
) -> ApiResult {
    if st.read_only {
        return Err(err(StatusCode::FORBIDDEN, "server is read-only"));
    }
    let ds = dataset(&st, &name)?;
    blocking(move || {
        if ds.store.delete_snapshot(&snap)? {
            Ok(StatusCode::NO_CONTENT.into_response())
        } else {
            Err(code_err(
                StatusCode::NOT_FOUND,
                "no-such-snapshot",
                format!("no snapshot '{snap}' in dataset {}", ds.name),
            ))
        }
    })
    .await
}

pub(super) async fn get_history(State(st): St, Path(name): Path<String>) -> ApiResult<Json<J>> {
    let ds = dataset(&st, &name)?;
    let h = ds.store.history();
    Ok(Json(history_json(&name, &ds, &h)))
}

pub(super) async fn put_history(
    State(st): St,
    Path(name): Path<String>,
    body: Bytes,
) -> ApiResult<Json<J>> {
    if st.read_only {
        return Err(err(StatusCode::FORBIDDEN, "server is read-only"));
    }
    let ds = dataset(&st, &name)?;
    let j: J = serde_json::from_slice(&body)
        .map_err(|e| err(StatusCode::BAD_REQUEST, format!("invalid JSON: {e}")))?;
    let keep_commits = match j.get("keepCommits") {
        None | Some(J::Null) => None,
        Some(v) => Some(v.as_u64().ok_or_else(|| {
            err(
                StatusCode::BAD_REQUEST,
                "keepCommits must be a number or null",
            )
        })?),
    };
    let keep_age_ms = match j.get("keepAge") {
        None | Some(J::Null) => None,
        Some(v) => Some(parse_age(v).ok_or_else(|| {
            err(
                StatusCode::BAD_REQUEST,
                "keepAge must be seconds or a duration like 90s, 30m, 12h, 7d, 2w",
            )
        })?),
    };
    let r = Retention {
        keep_commits,
        keep_age_ms,
    };
    blocking(move || {
        let h = ds.store.set_retention(r)?;
        Ok(Json(history_json(&ds.name, &ds, &h)))
    })
    .await
}

/// `reconstructable` ranges and per-commit flags for the commit listing.
pub(super) fn commit_list_extras(
    ds: &Dataset,
    commits: &[sparkles::commit::CommitInfo],
) -> (J, J, Vec<J>) {
    let h = ds.store.history();
    let pins = ds.store.snapshots();
    let inside = |s: u64| h.reconstructable.iter().any(|&(a, b)| a <= s && s <= b);
    let list = commits
        .iter()
        .map(|c| {
            let mut j = serde_json::to_value(c).unwrap_or(J::Null);
            j["reconstructable"] = json!(inside(c.seq));
            let names: Vec<&str> = pins
                .iter()
                .filter(|p| p.seq == c.seq)
                .map(|p| p.name.as_str())
                .collect();
            if !names.is_empty() {
                j["snapshots"] = json!(names);
            }
            j
        })
        .collect();
    (
        json!(h.oldest_reconstructable()),
        ranges(&h.reconstructable),
        list,
    )
}
