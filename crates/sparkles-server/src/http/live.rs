//! `GET /{ds}/live`: a live query over server-sent events (spec F13 §2.3).
//!
//! The query is a SELECT, ASK or CONSTRUCT query that a view could be. The stream
//! starts with the result in `snapshot` events of at most 10,000 rows, the last with
//! `more: false`, and then sends a `diff` event after each commit that changes the
//! result. Each event's id is the commit it reflects, so a client that reconnects with
//! `Last-Event-ID` (or `after=commit:N`) gets the change since that commit, or a
//! `reset` with the whole result when the server cannot compute it. `coalesceMs`
//! merges the diffs of commits that arrive within the interval.
//!
//! A row is `{"values": [term, …]}` in the order of `vars`, with SPARQL JSON terms and
//! `null` for unbound, plus `"count"` when the row was added or removed more than once.
//! ASK streams `{commit, value}`. The stream ends when the client closes it, when the
//! server drains, and with an `error` event when the subscription fails or the access
//! policy changes.

use super::*;
use axum::response::sse::{Event, KeepAlive, Sse};
use sparkles::incr::{LiveEvent, LiveOptions, LiveQuery, LiveRow};
use std::time::Instant;

/// The most rows a `snapshot` event carries.
const PAGE_ROWS: usize = 10_000;
/// The longest merge window of `coalesceMs`.
const MAX_COALESCE: Duration = Duration::from_secs(60);

fn bad(msg: impl Into<String>) -> ApiError {
    err(StatusCode::BAD_REQUEST, msg)
}

fn row_json(r: &LiveRow) -> J {
    let values: Vec<J> = r
        .terms
        .iter()
        .map(|t| t.as_ref().map_or(J::Null, results::term_json))
        .collect();
    if r.count == 1 {
        json!({ "values": values })
    } else {
        json!({ "values": values, "count": r.count })
    }
}

/// The SSE events of a live event.
fn events(e: &LiveEvent) -> Vec<Event> {
    match e {
        LiveEvent::Snapshot {
            commit,
            vars,
            rows,
            value,
            reset,
        } => {
            let name = if *reset { "reset" } else { "snapshot" };
            let id = commit.to_string();
            if let Some(v) = value {
                return vec![
                    Event::default()
                        .event(name)
                        .id(id)
                        .data(json!({ "commit": commit, "value": v }).to_string()),
                ];
            }
            let pages: Vec<&[LiveRow]> = if rows.is_empty() {
                vec![&[]]
            } else {
                rows.chunks(PAGE_ROWS).collect()
            };
            let n = pages.len();
            pages
                .into_iter()
                .enumerate()
                .map(|(i, page)| {
                    let rows: Vec<J> = page.iter().map(row_json).collect();
                    Event::default().event(name).id(id.clone()).data(
                        json!({
                            "commit": commit,
                            "vars": &**vars,
                            "rows": rows,
                            "more": i + 1 < n,
                        })
                        .to_string(),
                    )
                })
                .collect()
        }
        LiveEvent::Diff {
            commit,
            from,
            removed,
            added,
        } => vec![
            Event::default().event("diff").id(commit.to_string()).data(
                json!({
                    "commit": commit,
                    "from": from,
                    "removed": removed.iter().map(row_json).collect::<Vec<_>>(),
                    "added": added.iter().map(row_json).collect::<Vec<_>>(),
                })
                .to_string(),
            ),
        ],
        LiveEvent::Value {
            commit,
            from,
            value,
        } => vec![
            Event::default()
                .event("diff")
                .id(commit.to_string())
                .data(json!({ "commit": commit, "from": from, "value": value }).to_string()),
        ],
        LiveEvent::Error { code, message } => vec![
            Event::default()
                .event("error")
                .data(json!({ "code": code, "error": message }).to_string()),
        ],
    }
}

/// A commit number from `commit:N` or `N`.
fn commit_param(v: &str) -> Option<u64> {
    v.strip_prefix("commit:").unwrap_or(v).trim().parse().ok()
}

struct Stream {
    st: Arc<AppState>,
    live: Arc<LiveQuery>,
    coalesce: Duration,
    queue: std::collections::VecDeque<Event>,
    epoch: Option<u64>,
    done: bool,
    /// an event read while coalescing that does not merge
    pending: Option<LiveEvent>,
}

impl Stream {
    async fn recv(&mut self) -> Option<LiveEvent> {
        loop {
            if self.st.phase() == crate::obs::Phase::Draining {
                return None;
            }
            if self.epoch.is_some_and(|e| e != self.st.access_epoch()) {
                return Some(LiveEvent::Error {
                    code: "access-changed".into(),
                    message: "the access policy or a disclosure registry changed; reconnect to read under the new rules".into(),
                });
            }
            tokio::select! {
                e = self.live.recv() => return e,
                _ = tokio::time::sleep(Duration::from_secs(1)) => {}
            }
        }
    }

    async fn next(mut self) -> Option<(Result<Event, std::convert::Infallible>, Stream)> {
        loop {
            if let Some(e) = self.queue.pop_front() {
                return Some((Ok(e), self));
            }
            if self.done {
                return None;
            }
            let mut e = match self.pending.take() {
                Some(e) => e,
                None => self.recv().await?,
            };
            if !self.coalesce.is_zero()
                && matches!(e, LiveEvent::Diff { .. } | LiveEvent::Value { .. })
            {
                let until = Instant::now() + self.coalesce;
                while let Some(left) = until.checked_duration_since(Instant::now()) {
                    match tokio::time::timeout(left, self.live.recv()).await {
                        Ok(Some(next)) => match e.merge(&next) {
                            Some(m) => e = m,
                            None => {
                                self.pending = Some(next);
                                break;
                            }
                        },
                        Ok(None) | Err(_) => break,
                    }
                }
                // a merge that cancels out sends nothing
                if let LiveEvent::Diff { removed, added, .. } = &e
                    && removed.is_empty()
                    && added.is_empty()
                {
                    continue;
                }
            }
            if matches!(e, LiveEvent::Error { .. }) {
                self.done = true;
            }
            self.queue.extend(events(&e));
        }
    }
}

pub(super) async fn live(
    State(st): St,
    Path(name): Path<String>,
    Extension(p): Extension<Principal>,
    uri: Uri,
    headers: HeaderMap,
) -> ApiResult {
    let ds = dataset(&st, &name)?;
    let params = Params::from_query(&uri);
    let Some(query) = params.get("query").map(str::to_string) else {
        return Err(bad("a live query needs ?query="));
    };
    let last_event = headers
        .get("last-event-id")
        .and_then(|v| v.to_str().ok())
        .and_then(commit_param);
    let after = match (last_event, params.get("after")) {
        (Some(n), _) => Some(n),
        (None, Some(v)) => Some(commit_param(v).ok_or_else(|| bad("after takes commit:N"))?),
        (None, None) => None,
    };
    let coalesce = match params.get("coalesceMs") {
        None => Duration::ZERO,
        Some(v) => match v.parse::<u64>() {
            Ok(ms) => Duration::from_millis(ms).min(MAX_COALESCE),
            Err(_) => return Err(bad("coalesceMs must be a number of milliseconds")),
        },
    };
    let access = p.view(&ds.name, crate::auth::Endpoint::Query);
    let o = LiveOptions {
        after,
        max_memory_bytes: None,
        principal: (!p.is_local()).then(|| p.rate_key()),
    };
    let epoch = p.access_epoch();
    let ds2 = ds.clone();
    let live = blocking(move || Ok(ds2.dataset.live_as(&query, access, &o)?)).await?;
    let stream = Stream {
        st: st.clone(),
        live: Arc::new(live),
        coalesce,
        queue: Default::default(),
        epoch,
        done: false,
        pending: None,
    };
    let events = futures_util::stream::unfold(stream, Stream::next);
    let mut resp = Sse::new(events)
        .keep_alive(KeepAlive::new().interval(Duration::from_secs(15)))
        .into_response();
    resp.headers_mut().insert(
        header::CACHE_CONTROL,
        header::HeaderValue::from_static("no-store"),
    );
    Ok(resp)
}
