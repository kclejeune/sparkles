//! The `notifications` settings kind (spec C21 §5): whether notifications are on, the
//! channels, the routes from event types to channels, the repeat interval of lasting
//! conditions and the delivery's retries.
//!
//! A channel names each credential with a secret reference, `{"secret": NAME}`, and
//! never holds one inline. The checks of this module refuse an inline value without
//! quoting it, since serde's messages would.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeMap;

/// The runtime layer, in the data directory.
pub const NOTIFICATIONS_FILE: &str = "notifications.json";

/// The kind's top-level members.
pub const MEMBERS: &[&str] = &[
    "enabled",
    "server",
    "baseUrl",
    "repeatEvery",
    "channels",
    "routes",
    "delivery",
    "watches",
];

pub const WATCH_MEMBERS: &[&str] = &[
    "maxAncestors",
    "largeCommit",
    "identityMaxAgeDays",
    "batchSecs",
];

pub const DELIVERY_MEMBERS: &[&str] = &["attempts", "backoffSecs", "maxBackoffSecs", "timeoutSecs"];

/// The members of a channel that name a secret.
const SECRET_MEMBERS: &[&str] = &["token", "urlSecret", "signingSecret", "password"];

/// The most recipients of an email channel.
const MAX_RECIPIENTS: usize = 20;

/// The ntfy server a channel uses without `server`.
pub const NTFY_SH: &str = "https://ntfy.sh";

/// The most channels the kind holds. Each channel has a delivery thread of its own,
/// so the cap bounds the threads.
pub const MAX_CHANNELS: usize = 32;
/// The event that tells the owner of a watch about changes; no route carries it.
pub const WATCH_EVENT: &str = "entity.changed";

/// The shortest repeat interval, in days (one minute).
const MIN_REPEAT_DAYS: f64 = 1.0 / 1440.0;

fn default_repeat() -> String {
    "1d".into()
}

fn ntfy_sh() -> String {
    NTFY_SH.into()
}

/// The effective `notifications` object as its type.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct NotifySettings {
    /// whether events are delivered (a test send works either way)
    #[serde(default)]
    pub enabled: bool,
    /// the server's name in the envelope (default: the host name)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server: Option<String>,
    /// the URL the UI is reached at, which makes links absolute
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    /// the shortest time between two notifications about one lasting condition
    #[serde(default = "default_repeat")]
    pub repeat_every: String,
    #[serde(default)]
    pub channels: BTreeMap<String, Channel>,
    /// event type, `prefix.*` or `*` → channel names
    #[serde(default)]
    pub routes: BTreeMap<String, Vec<String>>,
    #[serde(default)]
    pub delivery: Delivery,
    /// the bounds of watches on entities (spec U12, item 619)
    #[serde(default)]
    pub watches: WatchSettings,
}

impl Default for NotifySettings {
    fn default() -> NotifySettings {
        NotifySettings {
            enabled: false,
            server: None,
            base_url: None,
            repeat_every: default_repeat(),
            channels: BTreeMap::new(),
            routes: BTreeMap::new(),
            delivery: Delivery::default(),
            watches: WatchSettings::default(),
        }
    }
}

/// A channel (§4).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Channel {
    Webhook(Webhook),
    Ntfy(Ntfy),
    Email(Email),
}

impl Channel {
    pub fn type_name(&self) -> &'static str {
        match self {
            Channel::Webhook(_) => "webhook",
            Channel::Ntfy(_) => "ntfy",
            Channel::Email(_) => "email",
        }
    }

    /// Where the channel delivers, without credentials.
    pub fn target(&self) -> String {
        match self {
            Channel::Webhook(w) => match (&w.url, &w.url_secret) {
                (Some(u), _) => u.clone(),
                (None, Some(s)) => format!("the URL in secret {}", s.secret),
                (None, None) => String::new(),
            },
            Channel::Ntfy(n) => format!("{}/{}", n.server.trim_end_matches('/'), n.topic),
            Channel::Email(e) => {
                let mut to = e.to.join(", ");
                if e.watchers {
                    if !to.is_empty() {
                        to.push_str(" and ");
                    }
                    to.push_str("the owners of watches");
                }
                format!("{} port {} ({}) to {to}", e.host, e.port(), e.tls.as_str())
            }
        }
    }

    /// Whether the channel mails the owners of watches.
    pub fn carries_watches(&self) -> bool {
        matches!(self, Channel::Email(e) if e.watchers)
    }

    /// The secrets the channel names.
    pub fn secrets(&self) -> Vec<&str> {
        match self {
            Channel::Webhook(w) => [&w.url_secret, &w.signing_secret]
                .into_iter()
                .flatten()
                .map(|s| s.secret.as_str())
                .collect(),
            Channel::Ntfy(n) => n.token.iter().map(|s| s.secret.as_str()).collect(),
            Channel::Email(e) => e.password.iter().map(|s| s.secret.as_str()).collect(),
        }
    }
}

/// `{"secret": NAME}`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SecretRef {
    pub secret: String,
}

/// A webhook channel (§4.1).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Webhook {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// a URL that carries a credential, read from a secret at each delivery
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url_secret: Option<SecretRef>,
    /// the key of the Standard Webhooks signature
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signing_secret: Option<SecretRef>,
}

/// An ntfy channel (§4.2).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Ntfy {
    #[serde(default = "ntfy_sh")]
    pub server: String,
    pub topic: String,
    /// an ntfy access token, sent as a bearer token
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token: Option<SecretRef>,
    /// 1 to 5 (default: from the event's severity)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub priority: Option<u8>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
}

/// How an email channel reaches its server (§4.3). There is no plain mode.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EmailTls {
    /// STARTTLS on the submission port (RFC 3207, RFC 6409)
    #[default]
    Starttls,
    /// TLS from the first byte (RFC 8314)
    Tls,
}

impl EmailTls {
    pub fn as_str(self) -> &'static str {
        match self {
            EmailTls::Starttls => "starttls",
            EmailTls::Tls => "tls",
        }
    }
}

/// An email channel (§4.3).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Email {
    pub host: String,
    /// 587 with STARTTLS and 465 with TLS by default
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
    #[serde(default)]
    pub tls: EmailTls,
    /// the SASL PLAIN user name; without it the channel does not authenticate
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub password: Option<SecretRef>,
    pub from: String,
    /// the recipients of the events the routes send; a channel that only carries
    /// watches has none
    #[serde(default)]
    pub to: Vec<String>,
    /// the channel also mails the owners of watches on entities, each at its own
    /// verified address (spec U12)
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub watchers: bool,
}

impl Email {
    pub fn port(&self) -> u16 {
        self.port.unwrap_or(match self.tls {
            EmailTls::Starttls => 587,
            EmailTls::Tls => 465,
        })
    }
}

/// Whether `s` is a mailbox address that SMTP carries without SMTPUTF8: printable
/// ASCII with one `@`, a local part and a domain, and none of the characters that
/// would end the address in a command or a header.
pub fn check_address(s: &str) -> Result<(), String> {
    let ok = s.len() <= 254
        && s.chars().all(|c| {
            c.is_ascii_graphic()
                && !matches!(
                    c,
                    '<' | '>' | ',' | '"' | '(' | ')' | ';' | ':' | '\\' | '[' | ']'
                )
        })
        && match s.split_once('@') {
            Some((l, d)) => {
                !l.is_empty()
                    && l.len() <= 64
                    && !d.contains('@')
                    && d.split('.').count() >= 1
                    && d.split('.').all(|p| {
                        !p.is_empty() && p.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
                    })
            }
            None => false,
        };
    if ok {
        Ok(())
    } else {
        Err(format!(
            "{s:?} is not a mail address such as ops@example.org"
        ))
    }
}

/// `watches`: the bounds of matching commits against watches on entities, and of
/// telling their owners.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct WatchSettings {
    /// the most ancestors one upward walk of a subtree watch visits
    #[serde(default = "WatchSettings::max_ancestors")]
    pub max_ancestors: u32,
    /// the most members of one watch a commit names before its match becomes a summary
    #[serde(default = "WatchSettings::large_commit")]
    pub large_commit: u32,
    /// an OIDC or proxy owner whose groups are older than this many days is not told
    /// outside the UI
    #[serde(default = "WatchSettings::identity_max_age_days")]
    pub identity_max_age_days: u32,
    /// the window that gathers one owner's matches in one dataset into one message
    #[serde(default = "WatchSettings::batch_secs")]
    pub batch_secs: u32,
}

impl WatchSettings {
    fn max_ancestors() -> u32 {
        10_000
    }
    fn large_commit() -> u32 {
        1_000
    }
    fn identity_max_age_days() -> u32 {
        30
    }
    fn batch_secs() -> u32 {
        300
    }

    fn validate(&self) -> Result<(), String> {
        if !(1..=1_000_000).contains(&self.max_ancestors) {
            return Err("watches.maxAncestors: from 1 to 1000000".into());
        }
        if !(1..=1_000_000).contains(&self.large_commit) {
            return Err("watches.largeCommit: from 1 to 1000000".into());
        }
        if !(1..=3650).contains(&self.identity_max_age_days) {
            return Err("watches.identityMaxAgeDays: from 1 to 3650".into());
        }
        if !(10..=86_400).contains(&self.batch_secs) {
            return Err("watches.batchSecs: from 10 to 86400".into());
        }
        Ok(())
    }
}

impl Default for WatchSettings {
    fn default() -> WatchSettings {
        WatchSettings {
            max_ancestors: Self::max_ancestors(),
            large_commit: Self::large_commit(),
            identity_max_age_days: Self::identity_max_age_days(),
            batch_secs: Self::batch_secs(),
        }
    }
}

/// `delivery` (§5.1, §6.2).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Delivery {
    /// attempts, the first included
    #[serde(default = "Delivery::attempts")]
    pub attempts: u32,
    /// the wait before the second attempt, which doubles for each later one
    #[serde(default = "Delivery::backoff")]
    pub backoff_secs: f64,
    #[serde(default = "Delivery::max_backoff")]
    pub max_backoff_secs: f64,
    /// one attempt's timeout, at most the outbound policy's
    #[serde(default = "Delivery::timeout")]
    pub timeout_secs: f64,
}

impl Delivery {
    fn attempts() -> u32 {
        5
    }
    fn backoff() -> f64 {
        30.0
    }
    fn max_backoff() -> f64 {
        1800.0
    }
    fn timeout() -> f64 {
        10.0
    }

    /// The wait before attempt `next` (2 or more), or the `Retry-After` of the last
    /// answer when that is longer and within `maxBackoffSecs`.
    pub fn wait(&self, next: u32, retry_after: Option<f64>) -> std::time::Duration {
        let exp = 2f64.powi(next.saturating_sub(2).min(30) as i32);
        let mut w = (self.backoff_secs * exp).min(self.max_backoff_secs);
        if let Some(r) = retry_after
            && r > w
        {
            w = r.min(self.max_backoff_secs);
        }
        std::time::Duration::from_secs_f64(w.max(0.0))
    }
}

impl Default for Delivery {
    fn default() -> Delivery {
        Delivery {
            attempts: Delivery::attempts(),
            backoff_secs: Delivery::backoff(),
            max_backoff_secs: Delivery::max_backoff(),
            timeout_secs: Delivery::timeout(),
        }
    }
}

// --------------------------------------------------------------- the settings kind ------

/// The built-in defaults: off, no channel and no route.
pub fn defaults() -> Value {
    json!({
        "enabled": false,
        "repeatEvery": default_repeat(),
        "channels": {},
        "routes": {},
        "delivery": {
            "attempts": Delivery::attempts(),
            "backoffSecs": Delivery::backoff(),
            "maxBackoffSecs": Delivery::max_backoff(),
            "timeoutSecs": Delivery::timeout(),
        },
    })
}

/// The effective object is kept as JSON, as for `models`: serde's form would add every
/// member a channel leaves out.
pub fn normalize(v: &Value) -> Result<Value, String> {
    parse(v).map(|_| v.clone())
}

pub fn check(v: &Value, _: crate::settings::Providers) -> Result<(), String> {
    parse(v).and_then(|s| s.validate())
}

/// The object as its type, with the secret members checked first so that no message
/// quotes an inline credential.
pub fn parse(v: &Value) -> Result<NotifySettings, String> {
    if let Some(Value::Object(channels)) = v.get("channels") {
        for (name, c) in channels {
            let Value::Object(c) = c else { continue };
            for m in SECRET_MEMBERS {
                match c.get(*m) {
                    None | Some(Value::Null) => {}
                    Some(Value::Object(r))
                        if r.len() == 1 && r.get("secret").is_some_and(Value::is_string) => {}
                    Some(_) => {
                        return Err(format!(
                            "channels.{name}.{m}: a secret reference such as {{\"secret\": \"{}\"}}; the settings never hold a credential",
                            if *m == "token" {
                                "ntfy-token"
                            } else {
                                "hook-signing"
                            }
                        ));
                    }
                }
            }
        }
    }
    if let Some(Value::Object(channels)) = v.get("channels") {
        for (name, c) in channels {
            if c.get("type").and_then(Value::as_str) == Some("email")
                && let Some(t) = c.get("tls")
                && !matches!(t.as_str(), Some("starttls" | "tls"))
            {
                return Err(format!(
                    "channels.{name}.tls: starttls or tls; an email channel never connects without TLS"
                ));
            }
        }
    }
    serde_json::from_value(v.clone()).map_err(|e| e.to_string())
}

/// Whether `s` is an `http` or `https` URL with a host and without a user name, a
/// password, a query or a fragment (`inline`), or with a query allowed (a URL read from
/// a secret).
pub fn check_url(s: &str, inline: bool) -> Result<(), String> {
    let iri = oxiri::Iri::parse(s).map_err(|_| "not a URL".to_string())?;
    if !matches!(iri.scheme(), "http" | "https") {
        return Err("not an http or https URL".into());
    }
    let auth = iri.authority().unwrap_or("");
    if auth.is_empty() {
        return Err("the URL has no host".into());
    }
    if auth.contains('@') {
        return Err("the URL carries a user name or password; use urlSecret".into());
    }
    if inline && (iri.query().is_some() || iri.fragment().is_some()) {
        return Err("the URL carries a query or a fragment; use urlSecret".into());
    }
    Ok(())
}

/// Whether `s` is an event type, a prefix ending in `.*`, or `*`.
fn route_key(s: &str) -> bool {
    if s == "*" {
        return true;
    }
    let base = s.strip_suffix(".*").unwrap_or(s);
    !base.is_empty()
        && base.split('.').all(|p| {
            !p.is_empty()
                && p.chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        })
}

fn days(what: &str, s: &str) -> Result<f64, String> {
    match crate::assist::duration_days(s) {
        Some(d) if d >= MIN_REPEAT_DAYS => Ok(d),
        _ => Err(format!(
            "{what}: {s:?} is not a duration of at least 1m, such as 1d or 12h"
        )),
    }
}

impl NotifySettings {
    pub fn validate(&self) -> Result<(), String> {
        if let Some(s) = &self.server
            && (s.trim().is_empty() || s.len() > 200 || s.chars().any(char::is_control))
        {
            return Err("server: a name of 1 to 200 characters".into());
        }
        if let Some(b) = &self.base_url {
            check_url(b, true).map_err(|e| format!("baseUrl: {e}"))?;
        }
        days("repeatEvery", &self.repeat_every)?;
        self.watches.validate()?;
        if self.channels.len() > MAX_CHANNELS {
            return Err(format!(
                "channels: at most {MAX_CHANNELS} channels, not {}",
                self.channels.len()
            ));
        }
        for (name, c) in &self.channels {
            crate::models::check_secret_name(name).map_err(|_| {
                format!("channels: {name:?} is not a channel name of letters, digits, _, - and .")
            })?;
            let at = |e: String| format!("channels.{name}: {e}");
            for s in c.secrets() {
                crate::models::check_secret_name(s).map_err(at)?;
            }
            match c {
                Channel::Email(e) => {
                    let host_ok = !e.host.is_empty()
                        && e.host.len() <= 253
                        && (e.host.parse::<std::net::IpAddr>().is_ok()
                            || e.host.split('.').all(|l| {
                                !l.is_empty()
                                    && l.len() <= 63
                                    && l.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
                            }));
                    if !host_ok {
                        return Err(at("host: a host name or an IP address".into()));
                    }
                    if e.port == Some(0) {
                        return Err(at("port: a port from 1 to 65535".into()));
                    }
                    check_address(&e.from).map_err(|m| at(format!("from: {m}")))?;
                    if (e.to.is_empty() && !e.watchers) || e.to.len() > MAX_RECIPIENTS {
                        return Err(at(format!(
                            "to: 1 to {MAX_RECIPIENTS} mail addresses, or none when the channel carries watches"
                        )));
                    }
                    for t in &e.to {
                        check_address(t).map_err(|m| at(format!("to: {m}")))?;
                    }
                    match (&e.username, &e.password) {
                        (Some(u), _)
                            if u.is_empty()
                                || u.len() > 256
                                || u.chars().any(|c| c.is_control()) =>
                        {
                            return Err(at(
                                "username: 1 to 256 characters without control characters".into(),
                            ));
                        }
                        (None, Some(_)) => {
                            return Err(at("password: a password needs a username".into()));
                        }
                        (Some(_), None) => {
                            return Err(at("username: a username needs a password secret".into()));
                        }
                        _ => {}
                    }
                }
                Channel::Webhook(w) => match (&w.url, &w.url_secret) {
                    (Some(u), None) => check_url(u, true).map_err(|e| at(format!("url: {e}")))?,
                    (None, Some(_)) => {}
                    _ => return Err(at("a webhook has either url or urlSecret".into())),
                },
                Channel::Ntfy(n) => {
                    check_url(&n.server, true).map_err(|e| at(format!("server: {e}")))?;
                    if n.topic.is_empty()
                        || n.topic.len() > 64
                        || !n
                            .topic
                            .chars()
                            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
                    {
                        return Err(at(
                            "topic: 1 to 64 letters, digits, - and _, as ntfy requires".into(),
                        ));
                    }
                    if n.priority.is_some_and(|p| !(1..=5).contains(&p)) {
                        return Err(at("priority: a number from 1 to 5".into()));
                    }
                    if n.tags.len() > 10
                        || n.tags.iter().any(|t| {
                            t.is_empty()
                                || t.len() > 64
                                || t.contains(',')
                                || t.chars().any(char::is_control)
                        })
                    {
                        return Err(at(
                            "tags: at most 10 tags of 1 to 64 characters without commas".into(),
                        ));
                    }
                }
            }
        }
        for (key, chans) in &self.routes {
            if !route_key(key) {
                return Err(format!(
                    "routes: {key:?} is not an event type, a prefix such as backup.* or *"
                ));
            }
            if key == WATCH_EVENT || key == "entity.*" {
                return Err(format!(
                    "routes: {WATCH_EVENT} goes only to the owners of watches, through email channels with watchers"
                ));
            }
            for c in chans {
                match self.channels.get(c) {
                    None => return Err(format!("routes.{key}: no channel named {c:?}")),
                    Some(Channel::Email(e)) if e.to.is_empty() => {
                        return Err(format!(
                            "routes.{key}: channel {c:?} has no recipients of its own; it only carries watches"
                        ));
                    }
                    Some(_) => {}
                }
            }
        }
        let d = &self.delivery;
        if !(1..=10).contains(&d.attempts) {
            return Err("delivery.attempts: a number from 1 to 10".into());
        }
        let secs = |what: &str, v: f64, max: f64| {
            if v.is_finite() && v > 0.0 && v <= max {
                Ok(())
            } else {
                Err(format!(
                    "delivery.{what}: a number of seconds above 0, at most {max}"
                ))
            }
        };
        secs("backoffSecs", d.backoff_secs, 86_400.0)?;
        secs("maxBackoffSecs", d.max_backoff_secs, 86_400.0)?;
        secs("timeoutSecs", d.timeout_secs, 300.0)?;
        if d.max_backoff_secs < d.backoff_secs {
            return Err("delivery.maxBackoffSecs is at least backoffSecs".into());
        }
        Ok(())
    }

    /// The repeat interval of a lasting condition.
    pub fn repeat(&self) -> chrono::TimeDelta {
        let d = crate::assist::duration_days(&self.repeat_every).unwrap_or(1.0);
        chrono::TimeDelta::milliseconds((d * 86_400_000.0) as i64)
    }

    /// The channels that the routes send an event of type `event` to: the union of every
    /// key that matches it, each once, in name order.
    pub fn route(&self, event: &str) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        // only the owners of watches are told of it, never the channels of a route
        if event == WATCH_EVENT {
            return out;
        }
        for (key, chans) in &self.routes {
            let hit = key == "*"
                || key == event
                || key
                    .strip_suffix(".*")
                    .is_some_and(|p| event.strip_prefix(p).is_some_and(|r| r.starts_with('.')));
            if hit {
                for c in chans {
                    if self.channels.contains_key(c) && !out.contains(c) {
                        out.push(c.clone());
                    }
                }
            }
        }
        out.sort();
        out
    }
}

/// The form of a field of `server.locked` after `notifications` (§5.2).
pub fn check_lock(p: &[String]) -> Result<(), String> {
    let top = p[0].as_str();
    if !MEMBERS.contains(&top) {
        return Err(format!(
            "notifications has no member {top:?}; use {}",
            MEMBERS.join(", ")
        ));
    }
    if top == "delivery"
        && let Some(m) = p.get(1)
        && !DELIVERY_MEMBERS.contains(&m.as_str())
    {
        return Err(format!("delivery has no member {m:?}"));
    }
    if top == "watches"
        && let Some(m) = p.get(1)
        && !WATCH_MEMBERS.contains(&m.as_str())
    {
        return Err(format!("watches has no member {m:?}"));
    }
    Ok(())
}

/// The secrets that the channels of a `notifications` object name.
pub fn secret_names(v: &Value) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(Value::Object(channels)) = v.get("channels") {
        for c in channels.values() {
            for m in SECRET_MEMBERS {
                if let Some(s) = c
                    .get(*m)
                    .and_then(|r| r.get("secret"))
                    .and_then(Value::as_str)
                    && !out.iter().any(|o| o == s)
                {
                    out.push(s.to_string());
                }
            }
        }
    }
    out
}
