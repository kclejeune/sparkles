//! `GET /{ds}/changes`: a change feed, the commits after a given one with their net
//! changes.
//!
//! `after` takes the selectors of `at` and defaults to the head. A page lists at most
//! `limit` commits (default 100, at most 1,000), oldest first, and
//! `Sparkles-Changes-Next` names the commit to ask after next. With `wait=N` (at most 60
//! seconds) a request that finds no new commit waits for one (long polling). The body
//! is JSON, or the commits' RDF Patches one after another (`application/rdf-patch`, or
//! `application/rdf-patch+thrift`).
//!
//! `Accept: text/event-stream` streams the commits as server-sent events instead, one
//! `commit` event per commit with the commit's number as its id, so a client that
//! reconnects with `Last-Event-ID` resumes where it stopped. A stream ends after five
//! minutes, and when the server shuts down.

use super::diff::{DiffFormat, selector};
use super::*;
use axum::response::sse::{Event, KeepAlive, Sse};
use sparkles::history::At;
use sparkles::store::{ChangePage, ChangesOptions, CommitChanges, DiffOp};
use std::io::Write;
use std::time::Instant;

/// The longest a request waits for a commit.
const MAX_WAIT: Duration = Duration::from_secs(60);
/// The longest an event stream stays open.
const MAX_STREAM: Duration = Duration::from_secs(300);
/// The most commits a page lists.
const MAX_LIMIT: usize = 1_000;

fn bad(msg: impl Into<String>) -> ApiError {
    err(StatusCode::BAD_REQUEST, msg)
}

/// The JSON of one commit of a page. For a caller whose grants cover only some graphs
/// the commit has no quad counts, and a commit too large to list has no change counts,
/// since both would count every graph.
fn commit_json(ds: &Dataset, c: &CommitChanges, restricted: bool) -> J {
    let note = ds.dataset.history().annotation(c.commit.seq);
    let commit = sparkles::commit::AnnotatedCommit {
        commit: &c.commit,
        annotation: note.as_ref(),
    };
    let mut commit = json!(commit);
    if restricted {
        super::redact_commit_json(&mut commit);
    }
    let mut j = json!({
        "commit": commit,
        "added": c.added,
        "removed": c.removed,
        "complete": c.complete(),
    });
    if restricted
        && !c.complete()
        && let Some(m) = j.as_object_mut()
    {
        m.remove("added");
        m.remove("removed");
    }
    if c.complete() {
        j["changes"] = J::Array(
            c.iter()
                .map(|(op, q)| super::diff::quad_json(op, &q))
                .collect(),
        );
    }
    j
}

/// Write one commit as an RDF Patch: it leads from the parent's state to the commit's.
fn write_patch<W: Write>(
    w: &mut sparkles::patch::PatchWriter<W>,
    ds: &Dataset,
    c: &CommitChanges,
) -> std::io::Result<()> {
    use sparkles::patch::commit_iri;
    let id = ds.store.dataset_id();
    let changes: Vec<(DiffOp, oxrdf::Quad)> = c.iter().collect();
    sparkles::patch::write_patch(
        w,
        &commit_iri(id, c.commit.seq),
        Some(&commit_iri(id, c.commit.seq.saturating_sub(1))),
        changes.iter().map(|(op, q)| (*op, q)),
    )
}

/// What a request asks of the feed.
#[derive(Clone)]
struct Ask {
    after: u64,
    limit: usize,
    max_quads: u64,
    /// the caller's graph view, when it does not cover every graph
    graphs: Option<Arc<sparkles::access::GraphAccess>>,
}

/// Read a page off the request's thread.
async fn page(ds: &Arc<Dataset>, ask: &Ask) -> ApiResult<ChangePage> {
    let ds = ds.clone();
    let o = ChangesOptions {
        max_commits: ask.limit,
        max_quads: ask.max_quads,
        cancel: None,
        deadline: None,
        graphs: ask.graphs.clone(),
    };
    let after = ask.after;
    match tokio::task::spawn_blocking(move || ds.dataset.history().changes(after, &o)).await {
        Ok(r) => r.map_err(ApiError::from),
        Err(e) => Err(err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string())),
    }
}

/// Wait until the published state passes `after`, `until` passes or the server begins
/// to drain. The live snapshot is read without a lock, so a long write holding the
/// writer lock never blocks the runtime here.
async fn wait_for_commit(
    st: &AppState,
    ds: &Dataset,
    rx: &mut tokio::sync::watch::Receiver<u64>,
    after: u64,
    until: Instant,
) {
    loop {
        if ds.store.snapshot().commit > after
            || Instant::now() >= until
            || st.phase() == crate::obs::Phase::Draining
        {
            return;
        }
        let tick = (until - Instant::now()).min(Duration::from_secs(1));
        tokio::select! {
            r = rx.changed() => {
                if r.is_err() {
                    return;
                }
            }
            _ = tokio::time::sleep(tick) => {}
        }
    }
}

pub(super) async fn changes(
    State(st): St,
    Path(name): Path<String>,
    Extension(p): Extension<Principal>,
    uri: Uri,
    headers: HeaderMap,
) -> ApiResult {
    let ds = dataset(&st, &name)?;
    // the changes of the graphs the caller may read
    let graphs = p.view(&ds.name, crate::auth::Endpoint::Diff);
    let params = Params::from_query(&uri);
    let sse = headers
        .get(header::ACCEPT)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|a| a.contains("text/event-stream"));
    // the body format, or each event's data: JSON or a text patch
    let fmt = match (sse, params.get("format")) {
        (true, None | Some("json")) => DiffFormat::Json,
        (true, Some("patch")) => DiffFormat::Patch(sparkles::patch::MEDIA_TYPE),
        (true, Some(_)) => return Err(bad("the events of a change feed carry json or patch")),
        (false, _) => DiffFormat::of(&params, &headers)?,
    };
    if fmt == DiffFormat::Lines {
        return Err(bad(
            "the change feed is json, patch or patch-binary, not diff lines",
        ));
    }
    let limit = match params.get("limit") {
        None => 100,
        Some(v) => match v.parse::<usize>() {
            Ok(n) if (1..=MAX_LIMIT).contains(&n) => n,
            _ => return Err(bad(format!("limit must be from 1 to {MAX_LIMIT}"))),
        },
    };
    let wait = match params.get("wait") {
        None => Duration::ZERO,
        Some(v) => match v.parse::<f64>() {
            Ok(s) if s >= 0.0 && s.is_finite() => Duration::from_secs_f64(s).min(MAX_WAIT),
            _ => return Err(bad("wait must be a number of seconds")),
        },
    };
    // where to start: `Last-Event-ID` (a reconnecting event stream), `after`, the head
    let last_event = headers
        .get("last-event-id")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.trim().parse::<u64>().ok());
    let at = selector(&params, "after")?.unwrap_or(At::Head);
    let after = match (last_event, &at) {
        (Some(n), _) => n,
        // a commit number needs no catalog record: the change log may hold the commit
        (None, At::Commit(n)) => *n,
        (None, _) => {
            let ds = ds.clone();
            blocking(move || Ok(ds.store.resolve(&at)?.commit.seq)).await?
        }
    };
    let ask = Ask {
        after,
        limit,
        max_quads: st.limits.max_rows as u64,
        graphs,
    };
    let restricted = ask.graphs.is_some();
    if sse {
        return Ok(event_stream(st, ds, ask, fmt).into_response());
    }
    // subscribe before reading, so a commit made in between wakes the wait
    let mut rx = ds.store.subscribe_commits();
    let mut p = page(&ds, &ask).await?;
    if p.commits.is_empty() && !wait.is_zero() {
        wait_for_commit(&st, &ds, &mut rx, after, Instant::now() + wait).await;
        p = page(&ds, &ask).await?;
    }
    let id = ds.store.dataset_id();
    if fmt != DiffFormat::Json {
        // a patch lists every change: the page ends before a commit that lists none
        if let Some(i) = p.commits.iter().position(|c| !c.complete()) {
            if i == 0 {
                let c = &p.commits[0];
                let n = if restricted {
                    "more".to_string()
                } else {
                    (c.added + c.removed).to_string()
                };
                return Err(ApiError(
                    StatusCode::INSUFFICIENT_STORAGE,
                    json!({
                        "error": format!(
                            "commit {} changes {n} quads, more than a page may list ({}); read the state at commit:{} instead",
                            c.commit.seq,
                            ask.max_quads,
                            c.commit.seq
                        ),
                        "code": "changes-too-large",
                        "commit": c.commit.seq,
                    }),
                ));
            }
            p.commits.truncate(i);
        }
    }
    let next = p.next();
    let head = p.head.seq;
    let p = Arc::new(p);
    let body_ds = ds.clone();
    let write = move |w: &mut LimitedWriter<stream::SwitchWriter>| -> sparkles::Result<()> {
        if fmt != DiffFormat::Json {
            let mut pw = sparkles::patch::PatchWriter::new(w, fmt == DiffFormat::PatchBinary);
            for c in &p.commits {
                write_patch(&mut pw, &body_ds, c)?;
            }
            return Ok(());
        }
        let head = json!({
            "dataset": body_ds.name,
            "datasetId": id,
            "after": p.after,
            "next": p.next(),
            "head": p.head,
        });
        let mut b = serde_json::to_vec(&head).map_err(std::io::Error::other)?;
        b.pop();
        w.write_all(&b)?;
        w.write_all(b",\"commits\":[")?;
        for (i, c) in p.commits.iter().enumerate() {
            if i > 0 {
                w.write_all(b",")?;
            }
            serde_json::to_writer(&mut *w, &commit_json(&body_ds, c, restricted))
                .map_err(std::io::Error::other)?;
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
        stream_end_log("changes", &end);
    })
    .await?;
    let body = match body {
        stream::Serialized::Whole { body, .. } => axum::body::Body::from(body),
        stream::Serialized::Streamed(b) => b,
    };
    let mut resp = ([(header::CONTENT_TYPE, fmt.content_type())], body).into_response();
    let h = resp.headers_mut();
    h.insert(SPARKLES_CHANGES_NEXT, next.into());
    h.insert(history::SPARKLES_HEAD, head.into());
    if let Ok(v) = header::HeaderValue::from_str(&id.to_string()) {
        h.insert(SPARKLES_DATASET_ID, v);
    }
    h.insert(
        header::CACHE_CONTROL,
        header::HeaderValue::from_static("no-store"),
    );
    if params.get("format").is_none() {
        h.append(header::VARY, header::HeaderValue::from_static("accept"));
    }
    Ok(resp)
}

/// The header that names the commit to ask after next.
pub(super) const SPARKLES_CHANGES_NEXT: &str = "sparkles-changes-next";

/// What an event stream is doing between events.
struct Feed {
    st: Arc<AppState>,
    ds: Arc<Dataset>,
    ask: Ask,
    rx: tokio::sync::watch::Receiver<u64>,
    fmt: DiffFormat,
    queue: std::collections::VecDeque<Event>,
    until: Instant,
    done: bool,
}

impl Feed {
    fn event(&self, c: &CommitChanges) -> Event {
        let e = Event::default()
            .event("commit")
            .id(c.commit.seq.to_string());
        if self.fmt == DiffFormat::Json {
            return e.data(commit_json(&self.ds, c, self.ask.graphs.is_some()).to_string());
        }
        let mut w = sparkles::patch::PatchWriter::new(Vec::new(), false);
        let text = match write_patch(&mut w, &self.ds, c) {
            Ok(()) => String::from_utf8(w.into_inner()).unwrap_or_default(),
            Err(e) => e.to_string(),
        };
        e.data(text.trim_end())
    }

    /// The next event, waiting for commits; `None` when the stream ends.
    async fn next(mut self) -> Option<(Result<Event, std::convert::Infallible>, Feed)> {
        loop {
            if let Some(e) = self.queue.pop_front() {
                return Some((Ok(e), self));
            }
            if self.done
                || Instant::now() >= self.until
                || self.st.phase() == crate::obs::Phase::Draining
            {
                return None;
            }
            match page(&self.ds, &self.ask).await {
                Ok(p) if p.commits.is_empty() => {
                    let (after, until) = (self.ask.after, self.until);
                    wait_for_commit(&self.st, &self.ds, &mut self.rx, after, until).await;
                }
                Ok(p) => {
                    for c in &p.commits {
                        let e = if !c.complete() && self.fmt != DiffFormat::Json {
                            // a text patch cannot leave changes out
                            self.done = true;
                            Event::default().event("error").data(
                                json!({
                                    "error": format!("commit {} changes more quads than a page may list; read the state at commit:{} instead", c.commit.seq, c.commit.seq),
                                    "code": "changes-too-large",
                                    "commit": c.commit.seq,
                                })
                                .to_string(),
                            )
                        } else {
                            self.event(c)
                        };
                        self.queue.push_back(e);
                        if self.done {
                            break;
                        }
                        self.ask.after = c.commit.seq;
                    }
                }
                Err(ApiError(_, body)) => {
                    self.done = true;
                    self.queue
                        .push_back(Event::default().event("error").data(body.to_string()));
                }
            }
        }
    }
}

fn event_stream(
    st: Arc<AppState>,
    ds: Arc<Dataset>,
    ask: Ask,
    fmt: DiffFormat,
) -> impl IntoResponse {
    let feed = Feed {
        rx: ds.store.subscribe_commits(),
        st,
        ds,
        ask,
        fmt,
        queue: Default::default(),
        until: Instant::now() + MAX_STREAM,
        done: false,
    };
    let events = futures_util::stream::unfold(feed, Feed::next);
    Sse::new(events).keep_alive(KeepAlive::default())
}
