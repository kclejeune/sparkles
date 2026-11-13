//! Mail to the owners of watches (spec U12, C21's `entity.changed`).
//!
//! The follower hands each new match that the owner did not make to [`Batches::add`].
//! The first match of an owner in a dataset opens a window of `watches.batchSecs`, and
//! at its close [`flush`] queues one delivery per email channel that a server
//! administrator marked `watchers`. A delivery holds only references: the owner, the
//! dataset and the matched watches and commits. Its message is built by [`build`] at
//! each attempt, from the watches as they are then, so a watch that was removed, a
//! match that was dropped, a graph the owner can no longer read, an owner who is gone
//! or whose identity was not confirmed within `watches.identityMaxAgeDays`, and an
//! address that is no longer verified all end or shrink the delivery before anything
//! is sent. The ordinary routes never carry these deliveries.

use super::follow::{Place, locate, owner_reads};
use super::{Match, Owner, Watch};
use crate::notify::config::NotifySettings;
use crate::state::AppState;
use chrono::{DateTime, Utc};
use parking_lot::Mutex;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// The event type of a delivery to a watch's owner.
pub const EVENT: &str = crate::notify::config::WATCH_EVENT;
/// The matched commits one batch holds; later ones are not mailed.
const MAX_REFS: usize = 1_000;
/// The commits one watch lists in a message; the rest are counted.
const MAX_COMMITS: usize = 20;
/// The most batches that wait at once; a match beyond them is not mailed.
const MAX_BATCHES: usize = 10_000;

/// The matches of one owner in one dataset that wait to be mailed.
#[derive(Clone, Debug)]
pub struct Batch {
    pub owner: Owner,
    /// (watch id, commit), in the order they were matched
    pub refs: Vec<(String, u64)>,
    due: Instant,
}

/// The open batches, by owner key and dataset.
#[derive(Default)]
pub struct Batches {
    open: Mutex<BTreeMap<(String, String), Batch>>,
}

/// Whether the settings can mail anything to watchers: notifications are on and an
/// email channel carries watches.
pub(crate) fn channels(cfg: &NotifySettings) -> Vec<String> {
    if !cfg.enabled {
        return Vec::new();
    }
    cfg.channels
        .iter()
        .filter(|(_, c)| c.carries_watches())
        .map(|(n, _)| n.clone())
        .collect()
}

impl Batches {
    /// Add new matches, each (owner, dataset, watch id, commit). Nothing is kept while
    /// no channel carries watches or for an owner that cannot have an address.
    pub(crate) fn add(&self, st: &AppState, told: Vec<(Owner, String, String, u64)>) {
        if told.is_empty() {
            return;
        }
        let cfg = crate::notify::settings(st);
        if channels(&cfg).is_empty() {
            return;
        }
        let window = Duration::from_secs(cfg.watches.batch_secs.into());
        let mut open = self.open.lock();
        for (owner, dataset, id, commit) in told {
            if !matches!(
                owner.kind,
                crate::auth::Kind::User | crate::auth::Kind::Oidc | crate::auth::Kind::Proxy
            ) {
                continue;
            }
            let key = (owner.key(), dataset);
            if !open.contains_key(&key) && open.len() >= MAX_BATCHES {
                tracing::warn!(target: "sparkles::watches", "too many mail batches wait, a match is not mailed");
                continue;
            }
            let b = open.entry(key).or_insert_with(|| Batch {
                owner,
                refs: Vec::new(),
                due: Instant::now() + window,
            });
            // a match beyond the bound is shown in the UI and not mailed, since a count
            // without its watch could not be authorized again
            if b.refs.len() < MAX_REFS {
                b.refs.push((id, commit));
            }
        }
    }

    /// Take the batches whose window has closed.
    fn due(&self) -> Vec<Batch> {
        let now = Instant::now();
        let mut open = self.open.lock();
        let keys: Vec<_> = open
            .iter()
            .filter(|(_, b)| b.due <= now)
            .map(|(k, _)| k.clone())
            .collect();
        keys.into_iter().filter_map(|k| open.remove(&k)).collect()
    }

    /// Close every window now (tests).
    #[cfg(test)]
    pub(crate) fn close_all(&self) {
        let now = Instant::now();
        for b in self.open.lock().values_mut() {
            b.due = now;
        }
    }

    /// The batches that wait (tests).
    #[cfg(test)]
    pub(crate) fn waiting(&self) -> Vec<Batch> {
        self.open.lock().values().cloned().collect()
    }
}

/// Queue a delivery for each batch whose window closed, on each channel that carries
/// watches.
pub fn flush(st: &Arc<AppState>) {
    let due = st.watches.batches.due();
    if due.is_empty() {
        return;
    }
    let cfg = crate::notify::settings(st);
    let names = channels(&cfg);
    if names.is_empty() {
        return;
    }
    for b in due {
        crate::notify::queue_watcher(st, &cfg, &names, Arc::new(b));
    }
}

/// The address to mail `owner` at now, or why there is none.
fn address(
    st: &AppState,
    owner: &Owner,
    cfg: &NotifySettings,
    now: DateTime<Utc>,
) -> Result<String, String> {
    use crate::auth::Kind;
    let days = cfg.watches.identity_max_age_days;
    let a = match owner.kind {
        Kind::User => {
            user_email(st, &owner.name).ok_or("the user has no email in the auth configuration")?
        }
        Kind::Oidc | Kind::Proxy => {
            if !owner.current(days, now) {
                return Err(format!(
                    "the identity provider has not confirmed the owner within {days} days"
                ));
            }
            owner
                .email
                .clone()
                .ok_or("the identity provider gave no verified address")?
        }
        _ => return Err("the owner has no address".into()),
    };
    crate::notify::config::check_address(&a)?;
    Ok(a)
}

/// Whether matches are mailed to `owner` now, and where (`GET /$/watches`).
pub(crate) fn mail_json(st: &AppState, owner: &Owner) -> Value {
    let cfg = crate::notify::settings(st);
    if channels(&cfg).is_empty() {
        return json!({ "channels": false, "why": "no email channel carries watches" });
    }
    match address(st, owner, &cfg, Utc::now()) {
        Ok(a) => json!({ "channels": true, "address": a }),
        Err(why) => json!({ "channels": true, "why": why }),
    }
}

#[cfg(feature = "auth")]
fn user_email(st: &AppState, name: &str) -> Option<String> {
    st.auth
        .as_ref()?
        .policy()
        .user_email(name)
        .map(str::to_string)
}

#[cfg(not(feature = "auth"))]
fn user_email(_: &AppState, _: &str) -> Option<String> {
    None
}

/// The message of batch `b` as of now, with envelope id `id`, and the address it goes
/// to; `Err` says why nothing is sent.
pub(crate) fn build(
    st: &AppState,
    cfg: &NotifySettings,
    b: &Batch,
    id: &str,
    now: DateTime<Utc>,
) -> Result<(Value, String), String> {
    let (watches, _, _) = st
        .watches
        .snapshot(st)
        .map_err(|_| "the watches file is refused".to_string())?;
    let mine: Vec<&Watch> = watches.iter().filter(|w| w.owner.same(&b.owner)).collect();
    // the owner as its provider or proxy last vouched for it
    let owner = mine
        .first()
        .map(|w| st.watches.vouched_owner(&w.owner))
        .ok_or("the owner has no watches left")?;
    let to = address(st, &owner, cfg, now)?;
    let mut by_watch: Vec<(&str, Vec<u64>)> = Vec::new();
    for (w, c) in &b.refs {
        match by_watch.iter_mut().find(|(x, _)| x == w) {
            Some((_, cs)) => cs.push(*c),
            None => by_watch.push((w, vec![*c])),
        }
    }
    let mut items = Vec::new();
    let mut dataset: Option<String> = None;
    let mut total = 0usize;
    for (wid, commits) in by_watch {
        let Some(w) = mine.iter().find(|w| w.id == wid) else {
            continue;
        };
        let Place::Found(main, branch) = locate(st, &w.dataset, w.branch_id) else {
            continue;
        };
        if !owner_reads(st, &w.owner, &main.name, &branch, w.graph.as_deref()) {
            continue;
        }
        let mut ms: Vec<&Match> = commits
            .iter()
            .filter_map(|c| w.matches.iter().find(|m| m.commit == *c && !m.own))
            .collect();
        if ms.is_empty() {
            continue;
        }
        ms.sort_by_key(|m| std::cmp::Reverse(m.commit));
        let ds = st.branch_dataset(&main, &branch).ok();
        let listed: Vec<Value> = ms
            .iter()
            .take(MAX_COMMITS)
            .map(|m| commit_json(m, ds.as_ref().map(|d| &*d.store)))
            .collect();
        total += ms.len();
        dataset.get_or_insert_with(|| main.name.clone());
        let mut item = json!({
            "id": w.id,
            "entity": w.entity,
            "scope": w.scope,
            "branch": branch,
            "truncated": w.truncated,
            "commits": listed,
            "more": ms.len().saturating_sub(MAX_COMMITS),
        });
        if let Some(g) = &w.graph {
            item["graph"] = json!(g);
        }
        if let Some(d) = w.depth {
            item["depth"] = json!(d);
        }
        items.push(item);
    }
    let Some(dataset) = dataset else {
        return Err("nothing in the batch is still readable by its owner".into());
    };
    let entities: Vec<&str> = items.iter().filter_map(|i| i["entity"].as_str()).collect();
    let title = match entities.as_slice() {
        [one] => format!("Changes to {} in {dataset}", short(one)),
        _ => format!(
            "Changes to {} watched entities in {dataset}",
            entities.len()
        ),
    };
    let shown: Vec<String> = entities.iter().take(5).map(|e| short(e)).collect();
    let rest = entities.len().saturating_sub(shown.len());
    let summary = format!(
        "{total} {} changed {}{} in {dataset}.",
        if total == 1 { "commit" } else { "commits" },
        shown.join(", "),
        if rest > 0 {
            format!(" and {rest} more")
        } else {
            String::new()
        }
    );
    let ev = crate::notify::Event {
        kind: EVENT,
        dataset: Some(dataset),
        severity: crate::notify::Severity::Info,
        title,
        summary,
        link: Some("/ui/watches".into()),
        data: json!({ "watches": items }),
    };
    let mut env = crate::notify::envelope(cfg, &ev, now);
    env["id"] = json!(id);
    Ok((env, to))
}

/// A commit of a message.
fn commit_json(m: &Match, store: Option<&sparkles::store::Store>) -> Value {
    let mut c = json!({
        "commit": m.commit,
        "time": m.time,
        "members": m.members,
        "count": m.count,
        "exact": m.exact,
    });
    if let Some(a) = &m.author {
        c["author"] = json!(a);
    }
    if m.summary {
        c["summary"] = json!(true);
    }
    if m.partial {
        c["partial"] = json!(true);
    }
    if let Some(msg) = store
        .and_then(|s| s.commit_record(m.commit))
        .and_then(|r| r.message)
    {
        c["message"] = json!(msg.as_ref());
    }
    c
}

/// The local name of an IRI, for a title.
fn short(iri: &str) -> String {
    let tail = iri
        .rsplit(['#', '/', ':'])
        .find(|s| !s.is_empty())
        .unwrap_or(iri);
    tail.chars().take(80).collect()
}
