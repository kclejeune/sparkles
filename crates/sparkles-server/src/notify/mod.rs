//! Outbound notifications (spec C21): events that a person should hear about, sent to
//! webhooks, ntfy topics and mail addresses.
//!
//! [`raise`] routes an event to the channels of the `notifications` settings kind,
//! skips it while a lasting condition with the same key was notified less than its
//! repeat interval ago, and queues one delivery per channel. Each channel has its own
//! queue, its share of [`MAX_QUEUE`], and its own worker thread, which sends the
//! channel's deliveries through the server's outbound policy and retries the transient
//! failures with backoff, so a channel that hangs or fails holds only its own
//! deliveries. Raising an event never waits for a delivery. The keys of lasting conditions are kept in
//! `<dataDir>/notifications-state.json`, and the queue, the counters and the recent
//! deliveries live in the process.
//!
//! The events are raised in [`events`]: the memory review and model budget conditions
//! once a minute, and a backup failure when a policy run ends.

pub mod config;
pub mod events;
pub mod http;
mod sign;
mod smtp;
#[cfg(test)]
mod tests;

use crate::state::AppState;
use chrono::{DateTime, TimeDelta, Utc};
use config::{Channel, Delivery, NotifySettings};
use parking_lot::{Condvar, Mutex};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeMap, VecDeque};
use std::path::PathBuf;
use std::sync::{Arc, Weak};
use std::time::{Duration, Instant};

/// The deliveries that the channels' queues share (§6.5). Each channel holds its share
/// of them, at least [`MIN_SHARE`], counting the one in flight.
const MAX_QUEUE: usize = 1000;
/// The smallest share of a channel's queue.
const MIN_SHARE: usize = 50;
/// The recent deliveries that `GET /$/notifications` lists.
const RECENT: usize = 50;
/// The file of the keys of lasting conditions, in the data directory.
pub const STATE_FILE: &str = "notifications-state.json";
/// The longest a worker waits before it looks at the server again.
const IDLE: Duration = Duration::from_millis(500);
/// How long a channel's worker stays with an empty queue before it ends. The next
/// delivery to the channel starts a new one. The worker of a channel that the settings
/// no longer have ends as soon as its queue is empty.
const LINGER: Duration = Duration::from_secs(30);
/// The most channel queues, and so delivery threads, at once: the channels of the
/// settings and those that were removed or renamed while their deliveries wait.
const MAX_LANES: usize = 2 * config::MAX_CHANNELS;

/// How serious an event is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Severity {
    Info,
    Warning,
    Critical,
}

impl Severity {
    pub fn as_str(self) -> &'static str {
        match self {
            Severity::Info => "info",
            Severity::Warning => "warning",
            Severity::Critical => "critical",
        }
    }

    /// ntfy's priority and tag for the severity.
    fn ntfy(self) -> (u8, &'static str) {
        match self {
            Severity::Info => (3, "information_source"),
            Severity::Warning => (4, "warning"),
            Severity::Critical => (5, "rotating_light"),
        }
    }
}

/// An event (§3).
#[derive(Clone, Debug)]
pub struct Event {
    /// the event type, such as `memory.review.pending`
    pub kind: &'static str,
    pub dataset: Option<String>,
    pub severity: Severity,
    pub title: String,
    pub summary: String,
    /// a path under `/ui`, made absolute with `baseUrl`
    pub link: Option<String>,
    pub data: Value,
}

/// A lasting condition: its key, and how long a notification about it holds back the
/// next (`None`: until the condition clears).
#[derive(Clone, Debug)]
pub struct Condition {
    pub key: String,
    pub repeat: Option<TimeDelta>,
}

/// What [`raise`] did with an event.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Raised {
    /// notifications are off
    Disabled,
    /// no route sends the event anywhere
    Unrouted,
    /// a notification about the condition was sent less than its interval ago
    Suppressed,
    /// queued for these channels
    Queued(Vec<String>),
}

/// The notifier's state (`AppState::notifier`).
#[derive(Default)]
pub struct Notifier {
    shared: Arc<Shared>,
}

#[derive(Default)]
struct Shared {
    inner: Mutex<Inner>,
}

/// The queue of one channel and the state of its worker thread.
struct Lane {
    queue: Vec<Job>,
    /// the deliveries the queue holds, the one in flight included
    capacity: usize,
    /// whether the worker is sending a delivery
    in_flight: bool,
    /// whether a worker thread serves the lane
    running: bool,
    wake: Arc<Condvar>,
}

impl Lane {
    fn new(capacity: usize) -> Lane {
        Lane {
            queue: Vec::new(),
            capacity,
            in_flight: false,
            running: false,
            wake: Arc::new(Condvar::new()),
        }
    }
}

/// A channel's share of the queue when the kind has `channels` channels.
fn share(channels: usize) -> usize {
    (MAX_QUEUE / channels.max(1)).max(MIN_SHARE)
}

/// One delivery: the envelope, the channel as it was when the event was raised, and
/// the attempts so far.
#[derive(Clone)]
struct Job {
    envelope: Arc<Value>,
    event: &'static str,
    channel: String,
    config: Channel,
    delivery: Delivery,
    attempt: u32,
    due: Instant,
}

/// A lasting condition that was notified (§6.3).
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Active {
    event: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    dataset: Option<String>,
    /// RFC 3339
    first: String,
    last: String,
    count: u64,
}

impl Active {
    fn last(&self) -> Option<DateTime<Utc>> {
        DateTime::parse_from_rfc3339(&self.last)
            .ok()
            .map(|t| t.with_timezone(&Utc))
    }
}

#[derive(Default, Serialize, Deserialize)]
struct StateFile {
    #[serde(default)]
    active: BTreeMap<String, Active>,
}

#[derive(Clone, Default)]
struct ChannelStatus {
    last_success: Option<Value>,
    last_failure: Option<Value>,
    sent: u64,
    failed: u64,
}

#[derive(Default)]
struct Inner {
    /// the channels' queues, by channel name
    lanes: BTreeMap<String, Lane>,
    /// the keys of lasting conditions, read from the state file at the first use
    active: Option<BTreeMap<String, Active>>,
    channels: BTreeMap<String, ChannelStatus>,
    recent: VecDeque<Value>,
    /// (channel, event, result)
    sent: BTreeMap<(String, String, &'static str), u64>,
    retries: BTreeMap<String, u64>,
    suppressed: BTreeMap<String, u64>,
}

fn rfc3339(t: DateTime<Utc>) -> String {
    t.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

fn state_path(st: &AppState) -> Option<PathBuf> {
    (!st.data_dir.as_os_str().is_empty()).then(|| st.data_dir.join(STATE_FILE))
}

/// The conditions of the state file's bytes. A file that a newer release wrote is
/// refused, like one that does not parse.
fn parse_state(bytes: &[u8]) -> sparkles::error::Result<BTreeMap<String, Active>> {
    sparkles::formats::settings_from_slice::<StateFile>(STATE_FILE, bytes).map(|s| s.active)
}

impl Inner {
    /// The deliveries waiting in every channel's queue.
    fn queued(&self) -> usize {
        self.lanes.values().map(|l| l.queue.len()).sum()
    }

    fn active(&mut self, st: &AppState) -> &mut BTreeMap<String, Active> {
        self.active.get_or_insert_with(|| {
            let Some(p) = state_path(st) else {
                return BTreeMap::new();
            };
            match std::fs::read(&p) {
                Ok(b) => match parse_state(&b) {
                    Ok(active) => active,
                    Err(e) => {
                        tracing::warn!(target: "sparkles::notify", "{} is not readable, it starts empty: {e}", p.display());
                        BTreeMap::new()
                    }
                },
                Err(_) => BTreeMap::new(),
            }
        })
    }

    fn save(&self, st: &AppState) {
        let (Some(p), Some(active)) = (state_path(st), &self.active) else {
            return;
        };
        let f = StateFile {
            active: active.clone(),
        };
        let mut bytes = sparkles::formats::settings_to_vec(STATE_FILE, &f).unwrap_or_default();
        bytes.push(b'\n');
        if let Err(e) = crate::settings::secrets::write_atomic(&p, &bytes, 0o644) {
            tracing::warn!(target: "sparkles::notify", "cannot write {}: {e}", p.display());
        }
    }

    fn record(&mut self, job: &Job, result: &'static str, detail: Value, attempts: u32) {
        let now = rfc3339(Utc::now());
        *self
            .sent
            .entry((job.channel.clone(), job.event.to_string(), result))
            .or_default() += 1;
        let c = self.channels.entry(job.channel.clone()).or_default();
        let mut entry = json!({
            "at": now,
            "event": job.event,
            "id": job.envelope["id"],
        });
        if let (Value::Object(e), Value::Object(d)) = (&mut entry, &detail) {
            for (k, v) in d {
                e.insert(k.clone(), v.clone());
            }
        }
        if result == "ok" {
            c.sent += 1;
            c.last_success = Some(entry);
        } else {
            c.failed += 1;
            c.last_failure = Some(entry);
        }
        let mut r = json!({
            "at": now,
            "id": job.envelope["id"],
            "event": job.event,
            "channel": job.channel,
            "result": result,
            "attempts": attempts,
        });
        if let (Value::Object(e), Value::Object(d)) = (&mut r, detail) {
            for (k, v) in d {
                e.insert(k, v);
            }
        }
        self.recent.push_front(r);
        self.recent.truncate(RECENT);
    }
}

/// The effective `notifications` settings, or the defaults (off) when they do not
/// read or do not pass the kind's checks, such as a runtime layer that an older release
/// wrote with more channels than the cap.
pub fn settings(st: &AppState) -> NotifySettings {
    effective(st).unwrap_or_else(|e| {
        tracing::warn!(target: "sparkles::notify", "notification settings, notifications are off: {e}");
        NotifySettings::default()
    })
}

/// The effective `notifications` settings, checked.
fn effective(st: &AppState) -> Result<NotifySettings, String> {
    let r = crate::settings::server::resolved_kind(st, &crate::settings::NOTIFICATIONS);
    config::parse(&r.effective).and_then(|s| s.validate().map(|()| s))
}

/// The host name, for the envelope's `server` without the setting.
fn host_name() -> String {
    std::fs::read_to_string("/proc/sys/kernel/hostname")
        .ok()
        .or_else(|| std::env::var("HOSTNAME").ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "sparkles".into())
}

/// The envelope of §3.2.
fn envelope(cfg: &NotifySettings, ev: &Event, now: DateTime<Utc>) -> Value {
    let mut v = json!({
        "version": 1,
        "id": uuid::Uuid::new_v4().to_string(),
        "type": ev.kind,
        "time": rfc3339(now),
        "server": cfg.server.clone().unwrap_or_else(host_name),
        "severity": ev.severity.as_str(),
        "title": ev.title,
        "summary": ev.summary,
        "data": ev.data,
    });
    if let Some(d) = &ev.dataset {
        v["dataset"] = d.clone().into();
    }
    if let Some(l) = &ev.link {
        v["link"] = match &cfg.base_url {
            Some(b) => format!("{}{l}", b.trim_end_matches('/')),
            None => l.clone(),
        }
        .into();
    }
    v
}

impl Notifier {
    /// Queue `jobs`, each in its channel's queue, when the kind has `channels`
    /// channels. A delivery that does not fit its channel's share is dropped (§6.5).
    fn enqueue(&self, st: &Arc<AppState>, jobs: Vec<Job>, channels: usize) {
        let cap = share(channels);
        let mut g = self.shared.inner.lock();
        for j in jobs {
            if !g.lanes.contains_key(&j.channel) && g.lanes.len() >= MAX_LANES {
                tracing::warn!(target: "sparkles::notify", channel = j.channel.as_str(), event = j.event,
                    "the queues of removed channels still hold deliveries, a delivery is dropped");
                g.record(
                    &j,
                    "dropped",
                    json!({ "error": "too many channels have queued deliveries" }),
                    0,
                );
                continue;
            }
            let lane = g
                .lanes
                .entry(j.channel.clone())
                .or_insert_with(|| Lane::new(cap));
            lane.capacity = cap;
            if lane.queue.len() + usize::from(lane.in_flight) >= lane.capacity {
                tracing::warn!(target: "sparkles::notify", channel = j.channel.as_str(), event = j.event,
                    capacity = cap, "the channel's notification queue is full, a delivery is dropped");
                g.record(
                    &j,
                    "dropped",
                    json!({ "error": "the channel's queue is full" }),
                    0,
                );
                continue;
            }
            let name = j.channel.clone();
            lane.queue.push(j);
            lane.wake.notify_one();
            if !lane.running {
                lane.running = true;
                if let Err(e) = spawn_worker(&self.shared, st, &name) {
                    tracing::error!(target: "sparkles::notify", channel = name.as_str(),
                        "cannot start the channel's delivery worker: {e}");
                    let lane = g.lanes.remove(&name).unwrap_or_else(|| Lane::new(cap));
                    for j in lane.queue {
                        g.record(
                            &j,
                            "dropped",
                            json!({ "error": "the channel's delivery worker cannot start" }),
                            0,
                        );
                    }
                }
            }
        }
    }
}

fn spawn_worker(shared: &Arc<Shared>, st: &Arc<AppState>, channel: &str) -> std::io::Result<()> {
    let shared = shared.clone();
    let weak = Arc::downgrade(st);
    let name = channel.to_string();
    std::thread::Builder::new()
        .name(format!(
            "notify-{}",
            channel.chars().take(8).collect::<String>()
        ))
        .spawn(move || worker(shared, weak, name))
        .map(|_| ())
}

/// Raise `ev` at `now`: route it, hold it back while `cond` was notified less than its
/// interval ago, and queue a delivery per channel.
pub fn raise(st: &Arc<AppState>, ev: Event, cond: Option<Condition>, now: DateTime<Utc>) -> Raised {
    let cfg = settings(st);
    if !cfg.enabled {
        return Raised::Disabled;
    }
    let channels = cfg.route(ev.kind);
    if channels.is_empty() {
        return Raised::Unrouted;
    }
    let n = &st.notifier;
    {
        let mut g = n.shared.inner.lock();
        if let Some(c) = &cond {
            let held = g
                .active(st)
                .get(&c.key)
                .is_some_and(|a| match (c.repeat, a.last()) {
                    (None, _) => true,
                    (Some(r), Some(last)) => now - last < r && now >= last,
                    (Some(_), None) => false,
                });
            if held {
                *g.suppressed.entry(ev.kind.to_string()).or_default() += 1;
                return Raised::Suppressed;
            }
            let a = g.active(st).entry(c.key.clone()).or_insert_with(|| Active {
                event: ev.kind.into(),
                dataset: ev.dataset.clone(),
                first: rfc3339(now),
                last: rfc3339(now),
                count: 0,
            });
            a.last = rfc3339(now);
            a.count += 1;
            g.save(st);
        }
    }
    let env = Arc::new(envelope(&cfg, &ev, now));
    let jobs = channels
        .iter()
        .filter_map(|name| {
            Some(Job {
                envelope: env.clone(),
                event: ev.kind,
                channel: name.clone(),
                config: cfg.channels.get(name)?.clone(),
                delivery: cfg.delivery,
                attempt: 1,
                due: Instant::now(),
            })
        })
        .collect();
    n.enqueue(st, jobs, cfg.channels.len());
    Raised::Queued(channels)
}

/// The condition `key` cleared: the next event with it is notified at once.
pub fn clear(st: &AppState, key: &str) {
    clear_where(st, |k| k == key);
}

/// Clear every condition whose key `cleared` accepts.
pub fn clear_where(st: &AppState, cleared: impl Fn(&str) -> bool) {
    let mut g = st.notifier.shared.inner.lock();
    let active = g.active(st);
    let before = active.len();
    active.retain(|k, _| !cleared(k));
    if active.len() != before {
        g.save(st);
    }
}

/// The keys of the active conditions that start with `prefix`.
pub fn keys_with_prefix(st: &AppState, prefix: &str) -> Vec<String> {
    let mut g = st.notifier.shared.inner.lock();
    g.active(st)
        .range(prefix.to_string()..)
        .take_while(|(k, _)| k.starts_with(prefix))
        .map(|(k, _)| k.clone())
        .collect()
}

/// Record the condition `key` of event type `event` as present without sending
/// anything, so that a later check can tell when it clears. Notifications are about
/// the event that names the condition's group.
pub fn mark(st: &AppState, key: &str, event: &str, now: DateTime<Utc>) {
    let mut g = st.notifier.shared.inner.lock();
    let active = g.active(st);
    if active.contains_key(key) {
        return;
    }
    active.insert(
        key.to_string(),
        Active {
            event: event.into(),
            dataset: None,
            first: rfc3339(now),
            last: rfc3339(now),
            count: 0,
        },
    );
    g.save(st);
}

/// Whether the condition `key` is active.
#[cfg(test)]
pub fn is_active(st: &AppState, key: &str) -> bool {
    st.notifier.shared.inner.lock().active(st).contains_key(key)
}

/// The keys of the active conditions.
#[cfg(test)]
pub fn active_keys(st: &AppState) -> Vec<String> {
    st.notifier
        .shared
        .inner
        .lock()
        .active(st)
        .keys()
        .cloned()
        .collect()
}

/// Turn notifications on for the tests of other modules: a settings file in `dir`
/// routes every event to a webhook on loopback, which the outbound policy refuses, so
/// an event is raised and recorded without any network traffic.
#[cfg(test)]
pub fn enable_for_tests(st: &mut AppState, dir: &std::path::Path) {
    st.outbound.allow_private = false;
    let f = dir.join("notify-settings.json");
    std::fs::write(
        &f,
        json!({"server": {"notifications": {
            "enabled": true,
            "channels": {"hook": {"type": "webhook", "url": "http://127.0.0.1:9/x"}},
            "routes": {"*": ["hook"]}
        }}})
        .to_string(),
    )
    .unwrap();
    crate::settings::server::start(st, Some(&f), &Default::default()).unwrap();
}

// --------------------------------------------------------------------- delivery ------

/// The outcome of one attempt.
#[derive(Debug)]
enum Attempt {
    Ok(u16),
    /// a transient failure, with the `Retry-After` of the answer in seconds
    Retry(String, Option<f64>),
    Failed(String),
    Refused(String),
}

/// The worker of channel `name`: it sends the deliveries of the channel's queue in
/// the order they are due, and ends when the server is gone or the queue stayed empty
/// for [`LINGER`].
fn worker(shared: Arc<Shared>, weak: Weak<AppState>, name: String) {
    let mut idle_since = Instant::now();
    // whether the settings no longer have the channel, checked while the queue is empty
    let mut removed = false;
    loop {
        let job = {
            let mut g = shared.inner.lock();
            let Some(lane) = g.lanes.get_mut(&name) else {
                return;
            };
            let now = Instant::now();
            let next = lane
                .queue
                .iter()
                .enumerate()
                .min_by_key(|(_, j)| j.due)
                .map(|(i, j)| (i, j.due));
            match next {
                Some((i, due)) if due <= now => {
                    lane.in_flight = true;
                    Some(lane.queue.swap_remove(i))
                }
                Some((_, due)) => {
                    idle_since = now;
                    let wake = lane.wake.clone();
                    wake.wait_for(&mut g, (due - now).min(IDLE));
                    None
                }
                None if now.duration_since(idle_since) >= LINGER || removed => {
                    g.lanes.remove(&name);
                    return;
                }
                None => {
                    let wake = lane.wake.clone();
                    wake.wait_for(&mut g, IDLE);
                    None
                }
            }
        };
        let Some(st) = weak.upgrade() else {
            if let Some(lane) = shared.inner.lock().lanes.get_mut(&name) {
                lane.running = false;
                lane.in_flight = false;
            }
            return;
        };
        let Some(mut job) = job else {
            let empty = shared
                .inner
                .lock()
                .lanes
                .get(&name)
                .is_some_and(|l| l.queue.is_empty());
            removed = empty && !effective(&st).is_ok_and(|c| c.channels.contains_key(&name));
            continue;
        };
        let outcome = attempt(&st, &job.channel, &job.config, &job.envelope, &job.delivery);
        drop(st);
        idle_since = Instant::now();
        let mut g = shared.inner.lock();
        let mut retry = None;
        match outcome {
            Attempt::Ok(status) => {
                tracing::info!(target: "sparkles::notify", channel = job.channel.as_str(), event = job.event, status, "notification delivered");
                g.record(&job, "ok", json!({ "status": status }), job.attempt);
            }
            Attempt::Retry(e, after) if job.attempt < job.delivery.attempts => {
                *g.retries.entry(job.channel.clone()).or_default() += 1;
                job.attempt += 1;
                let wait = job.delivery.wait(job.attempt, after);
                tracing::info!(target: "sparkles::notify", channel = job.channel.as_str(), event = job.event,
                    attempt = job.attempt, wait_secs = wait.as_secs_f64(), "notification retried: {e}");
                job.due = Instant::now() + wait;
                retry = Some(job);
            }
            Attempt::Retry(e, _) | Attempt::Failed(e) => {
                tracing::warn!(target: "sparkles::notify", channel = job.channel.as_str(), event = job.event,
                    attempts = job.attempt, "notification failed: {e}");
                g.record(&job, "failed", json!({ "error": e }), job.attempt);
            }
            Attempt::Refused(e) => {
                tracing::warn!(target: "sparkles::notify", channel = job.channel.as_str(), event = job.event,
                    "notification refused by the outbound policy: {e}");
                g.record(&job, "refused", json!({ "error": e }), job.attempt);
            }
        }
        // a retry waits in the channel's own queue, in the place its attempt held
        if let Some(lane) = g.lanes.get_mut(&name) {
            lane.in_flight = false;
            if let Some(j) = retry {
                lane.queue.push(j);
            }
        }
    }
}

/// The value of the secret `name`: its runtime value unless `server.locked` locks it,
/// else its declared source (spec C21 §5.3). The error never holds the value.
pub fn read_secret(st: &AppState, name: &str) -> Result<String, String> {
    let layers = &st.settings.server;
    let d = st.settings.declared();
    let runtime = (!d.secret_locked(name))
        .then(|| layers.secrets_dir())
        .flatten()
        .map(|dir| dir.join(name))
        .filter(|p| p.is_file());
    let read = match runtime {
        Some(p) => layers.sealing.read(&p, name)?,
        None => match layers.secret_sources().get(name) {
            None => return Err(format!("no secret named {name:?} is defined")),
            Some(sparkles::vector::embed::SecretSource::Env(v)) => std::env::var(v)
                .map_err(|_| format!("the environment variable of secret {name:?} is not set"))?,
            Some(sparkles::vector::embed::SecretSource::File(p)) => std::fs::read_to_string(p)
                .map_err(|_| format!("the file of secret {name:?} cannot be read"))?,
        },
    };
    let v = read.trim().to_string();
    if v.is_empty() {
        return Err(format!("secret {name:?} is empty"));
    }
    Ok(v)
}

/// `s` with each of `secrets` replaced, and a URL that reqwest quotes in its errors
/// left out.
fn scrub(mut s: String, secrets: &[&str]) -> String {
    for k in secrets {
        if k.len() >= 4 {
            s = s.replace(k, "[redacted]");
        }
    }
    if let Some(i) = s.find("for url (")
        && let Some(j) = s[i..].find(')')
    {
        s.replace_range(i..i + j + 1, "for the channel's URL");
    }
    s
}

/// The text of an answer's body, at most 200 characters.
fn excerpt(body: &[u8]) -> String {
    let t = String::from_utf8_lossy(body);
    let t = t.trim();
    let mut out: String = t.chars().take(200).collect();
    if t.chars().count() > 200 {
        out.push('…');
    }
    out
}

/// One attempt to deliver `env` to `channel`.
fn attempt(
    st: &AppState,
    name: &str,
    channel: &Channel,
    env: &Value,
    delivery: &Delivery,
) -> Attempt {
    let timeout = Duration::from_secs_f64(delivery.timeout_secs).min(st.outbound.timeout);
    let mut secrets: Vec<String> = Vec::new();
    let read = |s: &str, secrets: &mut Vec<String>| -> Result<String, Attempt> {
        let v = read_secret(st, s).map_err(|e| Attempt::Failed(format!("channel {name}: {e}")))?;
        secrets.push(v.clone());
        Ok(v)
    };
    let (url, headers, body) = match channel {
        Channel::Webhook(w) => {
            let url = match (&w.url, &w.url_secret) {
                (Some(u), _) => u.clone(),
                (None, Some(s)) => match read(&s.secret, &mut secrets) {
                    Ok(u) => {
                        if let Err(e) = config::check_url(&u, false) {
                            return Attempt::Failed(format!(
                                "channel {name}: the URL in secret {:?}: {e}",
                                s.secret
                            ));
                        }
                        u
                    }
                    Err(a) => return a,
                },
                (None, None) => return Attempt::Failed(format!("channel {name} has no URL")),
            };
            let body = serde_json::to_vec(env).unwrap_or_default();
            let mut headers = Vec::new();
            if let Some(s) = &w.signing_secret {
                let key = match read(&s.secret, &mut secrets) {
                    Ok(k) => sign::key_of(&k),
                    Err(a) => return a,
                };
                let id = env["id"].as_str().unwrap_or_default().to_string();
                let ts = Utc::now().timestamp();
                headers.push((
                    "webhook-signature".to_string(),
                    sign::standard(&key, &id, ts, &body),
                ));
                headers.push(("webhook-id".to_string(), id));
                headers.push(("webhook-timestamp".to_string(), ts.to_string()));
            }
            (url, headers, body)
        }
        Channel::Email(e) => {
            let password = match &e.password {
                Some(p) => match read(&p.secret, &mut secrets) {
                    Ok(v) => Some(v),
                    Err(a) => return a,
                },
                None => None,
            };
            let refs: Vec<&str> = secrets.iter().map(String::as_str).collect();
            return match smtp::send(&st.outbound, name, e, password.as_deref(), env, timeout) {
                Attempt::Ok(c) => Attempt::Ok(c),
                Attempt::Retry(m, a) => Attempt::Retry(scrub(m, &refs), a),
                Attempt::Failed(m) => Attempt::Failed(scrub(m, &refs)),
                Attempt::Refused(m) => Attempt::Refused(scrub(m, &refs)),
            };
        }
        Channel::Ntfy(n) => {
            let severity = match env["severity"].as_str() {
                Some("critical") => Severity::Critical,
                Some("warning") => Severity::Warning,
                _ => Severity::Info,
            };
            let (prio, tag) = severity.ntfy();
            let mut tags = vec![tag.to_string()];
            tags.extend(n.tags.iter().cloned());
            let mut msg = json!({
                "topic": n.topic,
                "title": env["title"],
                "message": env["summary"],
                "priority": n.priority.unwrap_or(prio),
                "tags": tags,
            });
            if let Some(l) = env["link"].as_str()
                && (l.starts_with("http://") || l.starts_with("https://"))
            {
                msg["click"] = l.into();
            }
            let mut headers = Vec::new();
            if let Some(t) = &n.token {
                match read(&t.secret, &mut secrets) {
                    Ok(v) => headers.push(("Authorization".to_string(), format!("Bearer {v}"))),
                    Err(a) => return a,
                }
            }
            let url = format!("{}/", n.server.trim_end_matches('/'));
            (url, headers, serde_json::to_vec(&msg).unwrap_or_default())
        }
    };
    let refs: Vec<&str> = secrets.iter().map(String::as_str).collect();
    let hdrs: Vec<(&str, &str)> = headers
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();
    match sparkles::outbound::post_json(&st.outbound, &url, &hdrs, body, timeout) {
        Ok(p) => {
            let status = p.status.as_u16();
            if (200..300).contains(&status) {
                return Attempt::Ok(status);
            }
            let text = scrub(excerpt(&p.body), &refs);
            let msg = if text.is_empty() {
                format!("HTTP {status}")
            } else {
                format!("HTTP {status}: {text}")
            };
            if status == 408 || status == 429 || status >= 500 {
                let after = p
                    .retry_after
                    .as_deref()
                    .and_then(|s| s.trim().parse::<f64>().ok());
                Attempt::Retry(msg, after)
            } else {
                Attempt::Failed(msg)
            }
        }
        Err(sparkles::outbound::Failure::Refused(m)) => Attempt::Refused(scrub(m, &refs)),
        Err(sparkles::outbound::Failure::Failed(m)) => Attempt::Retry(scrub(m, &refs), None),
        Err(sparkles::outbound::Failure::Budget(b)) => {
            Attempt::Failed(scrub(format!("{b:?}"), &refs))
        }
    }
}

/// Send a `notification.test` envelope to `channel` now, in one attempt, whatever
/// `enabled` and the routes say (§6.6). `Err` is (code, message).
pub fn test_send(st: &AppState, channel: &str) -> Result<Value, (&'static str, String)> {
    let cfg = settings(st);
    let Some(c) = cfg.channels.get(channel) else {
        return Err((
            "unknown-channel",
            format!("no notification channel named {channel:?}"),
        ));
    };
    let ev = Event {
        kind: "notification.test",
        dataset: None,
        severity: Severity::Info,
        title: "Test notification".into(),
        summary: format!("A test of the notification channel {channel} of this Sparkles server."),
        link: Some("/ui/server".into()),
        data: json!({ "channel": channel }),
    };
    let env = Arc::new(envelope(&cfg, &ev, Utc::now()));
    let job = Job {
        envelope: env.clone(),
        event: ev.kind,
        channel: channel.to_string(),
        config: c.clone(),
        delivery: cfg.delivery,
        attempt: 1,
        due: Instant::now(),
    };
    let started = Instant::now();
    let outcome = attempt(st, channel, c, &env, &cfg.delivery);
    let ms = started.elapsed().as_millis() as u64;
    let mut g = st.notifier.shared.inner.lock();
    match outcome {
        Attempt::Ok(status) => {
            g.record(&job, "ok", json!({ "status": status }), 1);
            Ok(json!({
                "channel": channel,
                "type": c.type_name(),
                "result": "ok",
                "status": status,
                "id": env["id"],
                "latencyMs": ms,
            }))
        }
        Attempt::Refused(e) => {
            g.record(&job, "refused", json!({ "error": e }), 1);
            Err(("outbound-refused", e))
        }
        Attempt::Retry(e, _) | Attempt::Failed(e) => {
            g.record(&job, "failed", json!({ "error": e }), 1);
            Err(("delivery-failed", e))
        }
    }
}

/// `GET /$/notifications` (§7).
pub fn status_json(st: &AppState) -> Value {
    let cfg = settings(st);
    let mut g = st.notifier.shared.inner.lock();
    let active: Vec<Value> = g
        .active(st)
        .iter()
        .map(|(k, a)| {
            json!({
                "key": k,
                "event": a.event,
                "dataset": a.dataset,
                "first": a.first,
                "last": a.last,
                "count": a.count,
            })
        })
        .collect();
    let channels: Vec<Value> = cfg
        .channels
        .iter()
        .map(|(name, c)| {
            let s = g.channels.get(name).cloned().unwrap_or_default();
            let lane = g.lanes.get(name);
            json!({
                "name": name,
                "type": c.type_name(),
                "target": c.target(),
                "secrets": c.secrets(),
                "lastSuccess": s.last_success,
                "lastFailure": s.last_failure,
                "sent": s.sent,
                "failed": s.failed,
                "queued": lane.map_or(0, |l| l.queue.len()),
                "inFlight": lane.is_some_and(|l| l.in_flight),
                "queueCapacity": lane.map_or_else(|| share(cfg.channels.len()), |l| l.capacity),
            })
        })
        .collect();
    json!({
        "enabled": cfg.enabled,
        "channels": channels,
        "routes": cfg.routes,
        "queued": g.queued(),
        "active": active,
        "recent": g.recent.iter().cloned().collect::<Vec<_>>(),
    })
}

/// The channels that name each secret, for `GET /$/server/secrets`.
pub fn secret_users(st: &AppState) -> BTreeMap<String, Vec<String>> {
    let mut out: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (name, c) in settings(st).channels {
        for s in c.secrets() {
            out.entry(s.to_string()).or_default().push(name.clone());
        }
    }
    out
}

/// The metrics of §7.
pub fn metrics(st: &AppState, out: &mut String) {
    use std::fmt::Write;
    let label = |s: &str| s.replace('\\', "\\\\").replace('"', "\\\"");
    let family = |o: &mut String, name: &str, kind: &str, help: &str| {
        let _ = writeln!(o, "# HELP {name} {help}");
        let _ = writeln!(o, "# TYPE {name} {kind}");
    };
    let g = st.notifier.shared.inner.lock();
    family(
        out,
        "sparkles_notifications_sent_total",
        "counter",
        "Notification deliveries by channel, event type and final result (ok, failed, refused or dropped).",
    );
    for ((c, e, r), n) in &g.sent {
        let _ = writeln!(
            out,
            "sparkles_notifications_sent_total{{channel=\"{}\",event=\"{}\",result=\"{r}\"}} {n}",
            label(c),
            label(e)
        );
    }
    family(
        out,
        "sparkles_notifications_retries_total",
        "counter",
        "Notification attempts that were retried, by channel.",
    );
    for (c, n) in &g.retries {
        let _ = writeln!(
            out,
            "sparkles_notifications_retries_total{{channel=\"{}\"}} {n}",
            label(c)
        );
    }
    family(
        out,
        "sparkles_notifications_suppressed_total",
        "counter",
        "Events held back because the same condition was notified within its repeat interval.",
    );
    for (e, n) in &g.suppressed {
        let _ = writeln!(
            out,
            "sparkles_notifications_suppressed_total{{event=\"{}\"}} {n}",
            label(e)
        );
    }
    family(
        out,
        "sparkles_notifications_queued",
        "gauge",
        "Notification deliveries waiting in the channels' queues.",
    );
    let _ = writeln!(out, "sparkles_notifications_queued {}", g.queued());
    family(
        out,
        "sparkles_notifications_channel_queued",
        "gauge",
        "Notification deliveries waiting in a channel's queue, by channel.",
    );
    for (c, l) in &g.lanes {
        let _ = writeln!(
            out,
            "sparkles_notifications_channel_queued{{channel=\"{}\"}} {}",
            label(c),
            l.queue.len()
        );
    }
    family(
        out,
        "sparkles_notifications_channel_queue_capacity",
        "gauge",
        "The deliveries a channel's queue holds, the one in flight included; a delivery beyond it ends dropped.",
    );
    for (c, l) in &g.lanes {
        let _ = writeln!(
            out,
            "sparkles_notifications_channel_queue_capacity{{channel=\"{}\"}} {}",
            label(c),
            l.capacity
        );
    }
}
