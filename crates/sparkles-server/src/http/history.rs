//! Point-in-time reads (`?at=`) and the named snapshot and history endpoints.

use super::*;
use sparkles::history::{
    At, CatalogHorizon, HistoryOptions, NamedSnapshot, Resolved, Retention, Schedule,
};
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
    let (snap, r) = match ds.store.snapshot_at(at, &o) {
        // a commit a branch shares with its upstream is read there
        Err(e) if sparkles::branch::inherited_commit(&e).is_some() => match ds.main() {
            Some(main) => main.store.branch_snapshot_at(&ds.branch_name(), at, &o)?,
            None => return Err(e),
        },
        r => r?,
    };
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

/// [`snapshot_json`] without the pinned commit's counts, which cover every graph, for a
/// caller whose grants cover only some graphs.
fn snapshot_json_for(s: &NamedSnapshot, restricted: bool) -> J {
    let mut j = snapshot_json(s);
    if restricted && let Some(c) = j.get_mut("commit") {
        super::redact_commit_json(c);
    }
    j
}

fn snapshot_json(s: &NamedSnapshot) -> J {
    json!({
        "name": s.name,
        "ref": format!("snapshot:{}", s.name),
        "seq": s.seq,
        "commit": s.commit,
        "created": sparkles::commit::rfc3339_ms(s.created_ms),
        "expires": s.expires_ms.map(sparkles::commit::rfc3339_ms),
        "note": s.note,
        "generation": s.generation,
        "reconstructable": s.reconstructable,
        "warm": s.warm,
    })
}

fn history_json(name: &str, ds: &Dataset, h: &sparkles::history::HistoryStatus) -> J {
    json!({
        "dataset": name,
        "datasetId": ds.store.owner_dataset_id(),
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
        "schedules": ds.dataset.settings().retention().get().schedules.iter().map(schedule_json).collect::<Vec<_>>(),
        "catalog": {
            "keepCommits": h.catalog.keep_commits,
            "keepAge": h.catalog.keep_age_ms.map(|ms| format!("{}s", ms / 1000)),
            "firstRetained": h.first_commit,
        },
        "snapshots": h.snapshots,
        "changeLog": ds.dataset.settings().change_log().status().map(|s| change_log_json(&s)),
        "cache": {
            "entries": h.cache_entries,
            "bytes": h.cache_bytes,
            "hits": h.hits,
            "misses": h.misses,
            "materializations": h.materializations,
        },
    })
}

/// The change log's state and settings.
pub(super) fn change_log_json(s: &sparkles::store::ChangeLogStatus) -> J {
    json!({
        "enabled": s.enabled,
        "first": s.first,
        "last": s.last,
        "segments": s.segments,
        "bytes": s.bytes,
        "pending": s.pending,
        "maxBytes": s.max_bytes,
        "settings": {
            "enabled": s.settings.enabled,
            "keepCommits": s.settings.keep_commits,
            "keepAge": s.settings.keep_age_ms.map(|ms| format!("{}s", ms / 1000)),
            "maxBytes": s.settings.max_bytes,
        },
        "error": s.error,
    })
}

/// The `changeLog` member of a `PUT /$/history/{ds}` body: every field it leaves out
/// takes the server's default.
fn change_log_param(v: &J) -> ApiResult<sparkles::store::ChangeLogSettings> {
    let bad = |m: &str| err(StatusCode::BAD_REQUEST, format!("changeLog.{m}"));
    let J::Object(o) = v else {
        return Err(err(
            StatusCode::BAD_REQUEST,
            "changeLog must be an object or null",
        ));
    };
    let mut s = sparkles::store::ChangeLogSettings::default();
    for (k, v) in o {
        if v.is_null() {
            continue;
        }
        match k.as_str() {
            "enabled" => {
                s.enabled = Some(
                    v.as_bool()
                        .ok_or_else(|| bad("enabled must be true or false"))?,
                )
            }
            "keepCommits" => {
                s.keep_commits = Some(
                    v.as_u64()
                        .ok_or_else(|| bad("keepCommits must be a number"))?,
                )
            }
            "keepAge" => {
                s.keep_age_ms = Some(parse_age(v).ok_or_else(|| {
                    bad("keepAge must be seconds or a duration like 90s, 30m, 12h, 7d, 2w")
                })?)
            }
            "maxBytes" => {
                s.max_bytes = Some(parse_size(v).ok_or_else(|| {
                    bad("maxBytes must be a number of bytes or a size like 512MiB, 10GiB")
                })?)
            }
            other => return Err(bad(&format!("{other} is not a setting"))),
        }
    }
    Ok(s)
}

fn retention_json(r: Retention) -> J {
    json!({
        "keepCommits": r.keep_commits,
        "keepAge": r.keep_age_ms.map(|ms| format!("{}s", ms / 1000)),
        "maxBytes": r.max_bytes,
    })
}

fn schedule_json(s: &Schedule) -> J {
    json!({
        "prefix": s.prefix,
        "every": format!("{}s", s.every_ms / 1000),
        "keepLast": s.keep_last,
    })
}

/// A size in bytes: a number, or a string with a binary suffix (`512MiB`, `10GiB`,
/// `K`, `M`, `G`, `T`).
pub(crate) fn parse_size(v: &J) -> Option<u64> {
    match v {
        J::Number(n) => n.as_u64(),
        J::String(s) => {
            let s = s.trim();
            let digits = s.find(|c: char| !c.is_ascii_digit()).unwrap_or(s.len());
            let n: u64 = s[..digits].parse().ok()?;
            let shift = match s[digits..].trim().to_ascii_lowercase().as_str() {
                "" | "b" => 0,
                "k" | "kib" => 10,
                "m" | "mib" => 20,
                "g" | "gib" => 30,
                "t" | "tib" => 40,
                _ => return None,
            };
            n.checked_mul(1 << shift)
        }
        _ => None,
    }
}

/// `"7d"`, `"12h"`, `"30m"`, `"90s"`, `"2w"`, or a number of seconds.
pub(crate) fn parse_age(v: &J) -> Option<u64> {
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

pub(super) async fn list_snapshots(
    State(st): St,
    Path(name): Path<String>,
    Extension(p): Extension<Principal>,
) -> ApiResult<Json<J>> {
    let ds = dataset(&st, &name)?;
    let restricted = p.restricted(&ds.name);
    let snaps: Vec<J> = ds
        .dataset
        .snapshots()
        .list()
        .iter()
        .map(|s| snapshot_json_for(s, restricted))
        .collect();
    Ok(Json(json!({
        "dataset": name,
        "datasetId": ds.store.owner_dataset_id(),
        "head": ds.store.head_commit().seq,
        "snapshots": snaps,
    })))
}

pub(super) async fn create_snapshot(
    State(st): St,
    Path(name): Path<String>,
    uri: Uri,
    headers: HeaderMap,
    AdminBody(body): AdminBody,
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
            for k in ["name", "at", "note", "expires", "warm"] {
                match j.get(k) {
                    Some(J::String(v)) => params.0.push((k.to_string(), v.clone())),
                    Some(J::Number(n)) => params.0.push((k.to_string(), n.to_string())),
                    Some(J::Bool(b)) => params.0.push((k.to_string(), b.to_string())),
                    _ => {}
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
    // an RFC 3339 instant, or a duration from now
    let expires = match params.get("expires") {
        None => None,
        Some(e) => Some(
            match format!("time:{e}").parse::<At>() {
                Ok(At::Time(ms)) => Some(ms),
                _ => None,
            }
            .or_else(|| {
                parse_age(&J::String(e.to_string()))
                    .or_else(|| e.parse::<u64>().ok().map(|s| s * 1000))
                    .map(|ms| now_ms() + ms as i64)
            })
            .ok_or_else(|| {
                err(
                    StatusCode::BAD_REQUEST,
                    "expires must be an RFC 3339 time or a duration like 90s, 30m, 12h, 7d, 2w",
                )
            })?,
        ),
    };
    let warm = params.get("warm").is_some_and(truthy);
    blocking(move || {
        let (snap, created) = ds.dataset.snapshots().create(
            &snap_name,
            &at,
            &sparkles::history::SnapshotOptions {
                note,
                expires_ms: expires,
                warm,
            },
        )?;
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
    Extension(p): Extension<Principal>,
) -> ApiResult<Json<J>> {
    let ds = dataset(&st, &name)?;
    let s = ds.dataset.snapshots().get(&snap).ok_or_else(|| {
        code_err(
            StatusCode::NOT_FOUND,
            "no-such-snapshot",
            format!("no snapshot '{snap}' in dataset {name}"),
        )
    })?;
    Ok(Json(snapshot_json_for(&s, p.restricted(&ds.name))))
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
        if ds.dataset.snapshots().delete(&snap)? {
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
    let h = ds.dataset.history().status();
    Ok(Json(history_json(&name, &ds, &h)))
}

pub(super) async fn put_history(
    State(st): St,
    Path(name): Path<String>,
    AdminBody(body): AdminBody,
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
    let max_bytes = match j.get("maxBytes") {
        None | Some(J::Null) => None,
        Some(v) => Some(parse_size(v).ok_or_else(|| {
            err(
                StatusCode::BAD_REQUEST,
                "maxBytes must be a number of bytes or a size like 512MiB, 10GiB",
            )
        })?),
    };
    let r = Retention {
        keep_commits,
        keep_age_ms,
        max_bytes,
    };
    // `schedules` replaces the pin schedules when present
    let schedules = match j.get("schedules") {
        None => None,
        Some(J::Null) => Some(Vec::new()),
        Some(J::Array(a)) => Some(
            a.iter()
                .map(schedule_param)
                .collect::<ApiResult<Vec<_>>>()?,
        ),
        Some(_) => {
            return Err(err(
                StatusCode::BAD_REQUEST,
                "schedules must be an array or null",
            ));
        }
    };
    // `catalog` replaces the catalog horizon when present
    let catalog = match j.get("catalog") {
        None => None,
        Some(J::Null) => Some(CatalogHorizon::default()),
        Some(J::Object(o)) => {
            let bad = || {
                err(
                    StatusCode::BAD_REQUEST,
                    "catalog is {\"keepCommits\": number, \"keepAge\": duration}",
                )
            };
            let keep_commits = match o.get("keepCommits") {
                None | Some(J::Null) => None,
                Some(v) => Some(v.as_u64().ok_or_else(bad)?),
            };
            let keep_age_ms = match o.get("keepAge") {
                None | Some(J::Null) => None,
                Some(v) => Some(parse_age(v).ok_or_else(bad)?),
            };
            Some(CatalogHorizon {
                keep_commits,
                keep_age_ms,
            })
        }
        Some(_) => {
            return Err(err(
                StatusCode::BAD_REQUEST,
                "catalog must be an object or null",
            ));
        }
    };
    // `changeLog` replaces the change log settings when present
    let change_log = match j.get("changeLog") {
        None => None,
        Some(J::Null) => Some(sparkles::store::ChangeLogSettings::default()),
        Some(v) => Some(change_log_param(v)?),
    };
    blocking(move || {
        let settings = ds.dataset.settings();
        if let Some(c) = change_log {
            settings.change_log().set(c)?;
        }
        let h = settings.retention().set(sparkles::handles::HistoryUpdate {
            retention: Some(r),
            schedules,
            catalog,
        })?;
        Ok(Json(history_json(&ds.name, &ds, &h)))
    })
    .await
}

/// The wall clock in milliseconds since the epoch.
fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64)
}

/// A schedule of a `PUT /$/history/{ds}` body.
fn schedule_param(s: &J) -> ApiResult<Schedule> {
    let bad = || {
        err(
            StatusCode::BAD_REQUEST,
            r#"a schedule is {"prefix": string, "every": duration, "keepLast": number}"#,
        )
    };
    Ok(Schedule {
        prefix: s
            .get("prefix")
            .and_then(J::as_str)
            .ok_or_else(bad)?
            .to_string(),
        every_ms: s.get("every").and_then(parse_age).ok_or_else(bad)?,
        keep_last: s
            .get("keepLast")
            .and_then(J::as_u64)
            .and_then(|n| u32::try_from(n).ok())
            .ok_or_else(bad)?,
    })
}

/// `reconstructable` ranges and per-commit flags for the commit listing.
pub(super) fn commit_list_extras(
    ds: &Dataset,
    commits: &[sparkles::commit::CommitInfo],
) -> (J, J, Vec<J>) {
    let h = ds.dataset.history().status();
    let pins = ds.dataset.snapshots().list();
    let inside = |s: u64| h.reconstructable.iter().any(|&(a, b)| a <= s && s <= b);
    let list = commits
        .iter()
        .map(|c| {
            let note = ds.dataset.history().annotation(c.seq);
            let commit = sparkles::commit::AnnotatedCommit {
                commit: c,
                annotation: note.as_ref(),
            };
            let mut j = serde_json::to_value(commit).unwrap_or(J::Null);
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

/// Run [`Store::history_tick`](sparkles::store::Store::history_tick) on every dataset
/// every `every`: pin expiry, scheduled pins, and collection of history that aged out.
pub(crate) fn spawn_tick(st: Arc<AppState>, every: std::time::Duration) {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(every);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        tick.tick().await;
        loop {
            tick.tick().await;
            let all: Vec<Arc<Dataset>> = st.datasets_and_branches();
            for ds in all {
                let name = ds.key();
                match tokio::task::spawn_blocking(move || ds.dataset.history().tick()).await {
                    Ok(Ok(r)) => {
                        for n in &r.created {
                            tracing::info!("dataset {name}: scheduled snapshot {n}");
                        }
                        for n in r.expired.iter().chain(&r.rotated) {
                            tracing::info!("dataset {name}: removed snapshot {n}");
                        }
                    }
                    Ok(Err(e)) => tracing::warn!("dataset {name}: history upkeep failed: {e}"),
                    Err(e) => tracing::warn!("dataset {name}: history upkeep panicked: {e}"),
                }
            }
        }
    });
}

/// History metrics in the Prometheus text format: retained bytes, named snapshots,
/// cache hits and misses, and materializations with their time.
pub(crate) fn metrics(st: &AppState, out: &mut String) {
    use std::fmt::Write;
    let datasets: Vec<Arc<Dataset>> = st.datasets.read().values().cloned().collect();
    if datasets.is_empty() {
        return;
    }
    // by dataset label: datasets beyond the label cap are summed under one
    let mut all: std::collections::BTreeMap<String, [f64; 7]> = Default::default();
    for ds in datasets {
        let h = ds.dataset.history().status();
        let e = all
            .entry(st.metrics.dataset_label(Some(&ds.name)))
            .or_default();
        let v = [
            h.bytes as f64,
            h.snapshots as f64,
            h.cache_entries as f64,
            h.hits as f64,
            h.misses as f64,
            h.materializations as f64,
            h.materialize_seconds,
        ];
        for (x, y) in e.iter_mut().zip(v) {
            *x += y;
        }
    }
    let label = |s: &str| {
        s.replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace('\n', "\\n")
    };
    let families: [(&str, &str, &str); 7] = [
        (
            "sparkles_history_bytes",
            "gauge",
            "Disk used by retained past generations.",
        ),
        ("sparkles_history_snapshots", "gauge", "Named snapshots."),
        (
            "sparkles_history_cache_entries",
            "gauge",
            "Past states held in memory.",
        ),
        (
            "sparkles_history_cache_hits_total",
            "counter",
            "Past-state reads served from memory.",
        ),
        (
            "sparkles_history_cache_misses_total",
            "counter",
            "Past-state reads that had to be materialized.",
        ),
        (
            "sparkles_history_materialize_seconds_count",
            "counter",
            "Past states materialized by replaying a write-ahead log.",
        ),
        (
            "sparkles_history_materialize_seconds_sum",
            "counter",
            "Time spent materializing past states.",
        ),
    ];
    for (i, (name, kind, help)) in families.into_iter().enumerate() {
        let _ = writeln!(out, "# HELP {name} {help}");
        let _ = writeln!(out, "# TYPE {name} {kind}");
        for (ds, v) in &all {
            let _ = writeln!(out, "{name}{{dataset=\"{}\"}} {}", label(ds), v[i]);
        }
    }
}

/// The instant of an `Accept-Datetime` request header (RFC 7089 §2.1.1), in
/// milliseconds. `None` without the header; `400` for a malformed one.
pub(super) fn accept_datetime(headers: &HeaderMap) -> ApiResult<Option<i64>> {
    let Some(v) = headers.get("accept-datetime") else {
        return Ok(None);
    };
    let t = v
        .to_str()
        .ok()
        .and_then(|s| httpdate::parse_http_date(s.trim()).ok())
        .ok_or_else(|| {
            code_err(
                StatusCode::BAD_REQUEST,
                "invalid-accept-datetime",
                "Accept-Datetime must be an HTTP date, like Wed, 30 Sep 2026 14:03:11 GMT",
            )
        })?;
    let ms = t
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64);
    // an HTTP date has whole seconds: the state as of the end of that second
    Ok(Some(ms + 999))
}

/// TimeGate negotiation (RFC 7089 §4.1.2, a 200-style one where the original resource is
/// its own TimeGate): the readable commit that best matches instant `ms`, the last one
/// at or before it, clamped to the oldest readable commit.
pub(super) fn negotiate_datetime(ds: &Dataset, ms: i64) -> sparkles::Result<At> {
    let h = ds.dataset.history().status();
    let wanted = match ds.store.resolve(&At::Time(ms)) {
        Ok(r) => r.commit.seq,
        // before the first commit the catalog has
        Err(sparkles::Error::NotFound(_)) => 0,
        Err(e) => return Err(e),
    };
    let readable = &h.reconstructable;
    let seq = readable
        .iter()
        .rev()
        .find_map(|&(a, b)| (a <= wanted).then_some(wanted.min(b)))
        .or_else(|| readable.first().map(|r| r.0))
        .unwrap_or(h.head);
    Ok(At::Commit(seq))
}

/// The headers of a memento chosen by `Accept-Datetime`: `Memento-Datetime`,
/// `Content-Location` (the URI of the memento itself, with `at`), `Vary` and a `Link` to
/// the original resource, which is also the TimeGate.
pub(super) fn memento_headers(mut resp: Response, r: &Resolved, uri: &Uri) -> Response {
    let h = resp.headers_mut();
    let t = std::time::UNIX_EPOCH
        + std::time::Duration::from_millis(r.commit.timestamp_ms.max(0) as u64);
    if let Ok(v) = header::HeaderValue::from_str(&httpdate::fmt_http_date(t)) {
        h.insert("memento-datetime", v);
    }
    if let Ok(v) = header::HeaderValue::from_str(&r.at.to_string()) {
        h.insert(SPARKLES_AT, v);
    }
    h.insert(SPARKLES_HEAD, r.head.into());
    let path = uri.path();
    let query = uri.query().unwrap_or("");
    let sep = if query.is_empty() { "" } else { "&" };
    let memento = format!("{path}?{query}{sep}at=commit:{}", r.commit.seq);
    if let Ok(v) = header::HeaderValue::from_str(&memento) {
        h.insert(header::CONTENT_LOCATION, v);
    }
    let original = if query.is_empty() {
        path.to_string()
    } else {
        format!("{path}?{query}")
    };
    if let Ok(v) =
        header::HeaderValue::from_str(&format!("<{original}>; rel=\"original timegate\""))
    {
        h.insert(header::LINK, v);
    }
    h.append(
        header::VARY,
        header::HeaderValue::from_static("accept-datetime"),
    );
    resp
}
