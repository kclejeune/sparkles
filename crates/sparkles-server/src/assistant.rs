//! A dataset's assistant (spec C18 §3.5, §5.5 and §6.4): its settings in
//! `<db>/assistant.json` (`GET`, `PUT /$/assistant/{ds}`), each principal's ask history
//! in `<db>/asks.json` (`GET`, `DELETE /$/asks/{ds}` and the feedback of
//! `POST /$/asks/{ds}/{id}/feedback`), the daily token counts that the budgets read, and
//! the usage counts of `GET /$/models/usage`.
//!
//! The settings choose among the providers of `--model-config` by name and never hold an
//! endpoint or a key. A principal sees only its own history, whatever its role: a
//! question can hold personal or confidential text. Admins see counts only.

// Without `mcp` there is no `POST /{ds}/ask`, so the parts only it uses go unused, while
// the settings, history and usage routes stay.
#![cfg_attr(not(feature = "mcp"), allow(dead_code))]

use crate::assist::{read_file, write_file};
use crate::auth::Principal;
use crate::http::{AdminBody, ApiResult, blocking, dataset, err, err_code};
use crate::models::{Models, Pair, Role};
use crate::state::{AppState, Dataset};
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Extension, Json, Router};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::sync::Arc;
use std::time::SystemTime;

pub const ASSISTANT_FILE: &str = "assistant.json";
pub const ASKS_FILE: &str = "asks.json";
/// The server's default retention of ask history, in days (`serve --ask-history-days`).
pub const DEFAULT_HISTORY_DAYS: u32 = 30;
/// The default token cap of one ask.
pub const DEFAULT_TOKENS_PER_REQUEST: u64 = 50_000;
/// The default deadline of one ask.
pub const DEFAULT_DEADLINE_SECS: f64 = 120.0;
/// The rows a summary is sent by default.
pub const DEFAULT_ROWS_FOR_SUMMARY: usize = 50;
/// The most rows a summary may be sent.
const MAX_ROWS_FOR_SUMMARY: usize = 500;
/// The most entries a dataset's history keeps, over all principals.
const MAX_HISTORY: usize = 5000;
/// The asks remembered in memory for **Try harder** and feedback, with or without
/// history.
const MAX_RECENT: usize = 10_000;
/// The days of usage counts kept in memory.
const USAGE_DAYS: u64 = 400;

type St = State<Arc<AppState>>;

/// What may leave the server for a model (§3.5). Each level includes the ones before it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Send {
    /// the schema report, prefixes, examples, linked labels, query text and plans
    #[default]
    Schema,
    /// also result rows for the summary
    Rows,
    /// also source text for ingestion
    Documents,
}

/// Token caps of a dataset's asks (§3.5).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct AskBudget {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub per_request: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub per_principal_per_day: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub per_dataset_per_day: Option<u64>,
}

fn yes() -> bool {
    true
}

/// `assistant.json` (§3.5).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct AssistantSettings {
    /// whether the dataset has an assistant at all
    #[serde(default)]
    pub enabled: bool,
    /// overrides of the server's role lists
    #[serde(default)]
    pub roles: BTreeMap<Role, Vec<Pair>>,
    /// whether `POST /{ds}/ask` is enabled
    #[serde(default = "yes")]
    pub ask: bool,
    #[serde(default)]
    pub explain: bool,
    #[serde(default)]
    pub optimize: bool,
    #[serde(default)]
    pub ingest: bool,
    #[serde(default)]
    pub send: Send,
    #[serde(default)]
    pub send_by_provider: BTreeMap<String, Send>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rows_for_summary: Option<usize>,
    #[serde(default)]
    pub budget: AskBudget,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deadline_secs: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub history_days: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub routing: Option<crate::models::Routing>,
}

impl Default for AssistantSettings {
    fn default() -> AssistantSettings {
        AssistantSettings {
            enabled: false,
            roles: BTreeMap::new(),
            ask: true,
            explain: false,
            optimize: false,
            ingest: false,
            send: Send::Schema,
            send_by_provider: BTreeMap::new(),
            rows_for_summary: None,
            budget: AskBudget::default(),
            deadline_secs: None,
            history_days: None,
            routing: None,
        }
    }
}

/// Members that would point a key at an endpoint, refused anywhere in a body.
const FORBIDDEN: &[&str] = &["endpoint", "apiKey"];

fn forbidden_member(v: &Value) -> Option<&'static str> {
    match v {
        Value::Object(m) => m.iter().find_map(|(k, v)| {
            FORBIDDEN
                .iter()
                .find(|f| **f == k)
                .copied()
                .or_else(|| forbidden_member(v))
        }),
        Value::Array(a) => a.iter().find_map(forbidden_member),
        _ => None,
    }
}

impl AssistantSettings {
    /// Parse a `PUT` body: refuse `endpoint` and `apiKey` anywhere, unknown members and
    /// roles, and providers or models the server does not allow (§3.5). A `status`
    /// member, which `GET` adds, is ignored.
    pub fn parse(body: &[u8], models: Option<&Models>) -> Result<AssistantSettings, String> {
        let mut v: Value = serde_json::from_slice(body).map_err(|e| e.to_string())?;
        if let Some(f) = forbidden_member(&v) {
            return Err(format!(
                "{f} is not allowed: providers are defined in the server's model configuration only"
            ));
        }
        if let Some(m) = v.as_object_mut() {
            m.remove("status");
        }
        let s: AssistantSettings = serde_json::from_value(v).map_err(|e| e.to_string())?;
        s.validate(models)?;
        Ok(s)
    }

    fn validate(&self, models: Option<&Models>) -> Result<(), String> {
        for (role, list) in &self.roles {
            for p in list {
                let Some(m) = models else {
                    return Err(format!(
                        "roles.{}: this server defines no model provider",
                        role.as_str()
                    ));
                };
                let Some(provider) = m.provider(&p.provider) else {
                    return Err(format!(
                        "roles.{}: the server defines no provider named {:?}",
                        role.as_str(),
                        p.provider
                    ));
                };
                if p.model.is_empty() || !provider.allows(&p.model) {
                    return Err(format!(
                        "roles.{}: provider {} does not allow the model {:?}",
                        role.as_str(),
                        p.provider,
                        p.model
                    ));
                }
                if p.max_output_tokens == Some(0) {
                    return Err(format!(
                        "roles.{}: maxOutputTokens must be at least 1",
                        role.as_str()
                    ));
                }
            }
        }
        for name in self.send_by_provider.keys() {
            if models.and_then(|m| m.provider(name)).is_none() {
                return Err(format!(
                    "sendByProvider: the server defines no provider named {name:?}"
                ));
            }
        }
        if let Some(n) = self.rows_for_summary
            && !(1..=MAX_ROWS_FOR_SUMMARY).contains(&n)
        {
            return Err(format!(
                "rowsForSummary must be from 1 to {MAX_ROWS_FOR_SUMMARY}"
            ));
        }
        if let Some(d) = self.deadline_secs
            && !(d.is_finite() && d > 0.0 && d <= 3600.0)
        {
            return Err("deadlineSecs must be > 0 and ≤ 3600".into());
        }
        if self.budget.per_request == Some(0) {
            return Err("budget.perRequest must be at least 1".into());
        }
        if let Some(h) = self.history_days
            && h > 3650
        {
            return Err("historyDays must be at most 3650".into());
        }
        Ok(())
    }

    /// What may be sent to `provider`: the dataset's level, lowered by
    /// `sendByProvider`.
    pub fn send_to(&self, provider: &str) -> Send {
        self.send_by_provider
            .get(provider)
            .map_or(self.send, |s| (*s).min(self.send))
    }

    pub fn rows_for_summary(&self) -> usize {
        self.rows_for_summary.unwrap_or(DEFAULT_ROWS_FOR_SUMMARY)
    }

    /// The retention of history, with the server's default.
    pub fn history_days(&self, st: &AppState) -> u32 {
        self.history_days.unwrap_or(st.asks.history_days)
    }
}

/// The settings of a dataset (the defaults without a file, or with one that cannot be
/// read, which is logged).
pub fn settings(st: &AppState, ds: &Dataset) -> AssistantSettings {
    match read_file(st, ds, ASSISTANT_FILE) {
        Ok(Some(v)) => serde_json::from_value(v).unwrap_or_else(|e| {
            tracing::warn!(dataset = %ds.name, "{ASSISTANT_FILE}: {e}");
            AssistantSettings::default()
        }),
        Ok(None) => AssistantSettings::default(),
        Err(e) => {
            tracing::warn!(dataset = %ds.name, "{e:#}");
            AssistantSettings::default()
        }
    }
}

/// The settings of a database directory for `sparkles ask --loc`.
#[cfg(feature = "mcp")]
pub fn settings_at(dir: &std::path::Path) -> anyhow::Result<Option<AssistantSettings>> {
    use anyhow::Context;
    match std::fs::read(dir.join(ASSISTANT_FILE)) {
        Ok(b) => Ok(Some(serde_json::from_slice(&b).with_context(|| {
            format!("{ASSISTANT_FILE} of {}", dir.display())
        })?)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).with_context(|| format!("reading {ASSISTANT_FILE}")),
    }
}

/// The role lists of a dataset (§3.5, §3.7): its overrides over the server's lists, with
/// `summarize` limited to the providers that may receive rows.
#[cfg(feature = "mcp")]
pub fn lists(models: &Models, s: &AssistantSettings) -> crate::ask::RoleLists {
    let mut l = crate::ask::resolve_lists(models, &s.roles);
    if let Some(sum) = l.get_mut(&Role::Summarize) {
        sum.retain(|p| s.send_to(&p.provider) >= Send::Rows);
    }
    l
}

/// Whether the dataset answers `POST /{ds}/ask`, and why not.
pub fn ask_status(st: &AppState, s: &AssistantSettings) -> Result<(), &'static str> {
    if !cfg!(feature = "mcp") {
        return Err("this server is built without the mcp feature, which asking needs");
    }
    let Some(m) = &st.models else {
        return Err("this server has no model providers (serve --model-config)");
    };
    if !s.enabled {
        return Err("the dataset has no assistant (PUT /$/assistant/{ds} with enabled)");
    }
    if !s.ask {
        return Err("asking is turned off for the dataset (ask: false)");
    }
    let draft = s
        .roles
        .get(&Role::Draft)
        .cloned()
        .unwrap_or_else(|| m.pairs(Role::Draft));
    if draft.is_empty() {
        return Err("no provider and model answers the draft role");
    }
    Ok(())
}

// ------------------------------------------------------------- the runtime ------

fn today() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() / 86_400)
}

fn now_rfc3339() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

/// An ask the server remembers in memory, for **Try harder** and for feedback when
/// history is off.
#[derive(Clone, Debug)]
pub struct Recent {
    pub dataset: String,
    pub principal: String,
    /// the draft pair's index in the draft list
    pub draft_pair: usize,
    /// the role and pair that wrote the final query
    pub answered_by: Option<(String, String, String)>,
    pub bucket: Option<String>,
    pub outcome: String,
    pub day: u64,
}

/// Usage counts of one dataset on one day.
#[derive(Clone, Debug, Default)]
struct Counts {
    /// (role, provider, model, outcome) → answers
    answers: BTreeMap<(String, String, String, String), u64>,
    /// (role, signal) → escalations
    escalations: BTreeMap<(String, String), u64>,
    /// (bucket, outcome) → answers
    outcomes: BTreeMap<(String, String), u64>,
    /// (provider, model) → (input tokens, output tokens, estimated cost)
    tokens: BTreeMap<(String, String), (u64, u64, f64)>,
    asks: u64,
}

/// The in-memory state of asking: recent asks, usage counts and daily token counts.
pub struct Runtime {
    /// the server's default retention of history, in days
    pub history_days: u32,
    recent: Mutex<(HashMap<String, Recent>, VecDeque<String>)>,
    /// (day, dataset) → counts
    usage: Mutex<BTreeMap<(u64, String), Counts>>,
    /// (day, dataset, principal or "" for the dataset) → tokens
    tokens: Mutex<HashMap<(u64, String, String), u64>>,
    /// totals since the start by dataset, for `/$/metrics`
    totals: Mutex<BTreeMap<String, Counts>>,
}

impl Default for Runtime {
    fn default() -> Runtime {
        Runtime {
            history_days: DEFAULT_HISTORY_DAYS,
            recent: Mutex::default(),
            usage: Mutex::default(),
            tokens: Mutex::default(),
            totals: Mutex::default(),
        }
    }
}

impl Runtime {
    /// The tokens counted today for a dataset, or for one principal in it.
    pub fn tokens_today(&self, dataset: &str, principal: Option<&str>) -> u64 {
        self.tokens
            .lock()
            .get(&(
                today(),
                dataset.to_string(),
                principal.unwrap_or("").to_string(),
            ))
            .copied()
            .unwrap_or(0)
    }

    /// Add an ask's tokens to today's counts of its dataset and principal.
    pub fn add_tokens(&self, dataset: &str, principal: &str, tokens: u64) {
        let d = today();
        let mut t = self.tokens.lock();
        t.retain(|k, _| k.0 + 1 >= d);
        *t.entry((d, dataset.to_string(), String::new()))
            .or_default() += tokens;
        *t.entry((d, dataset.to_string(), principal.to_string()))
            .or_default() += tokens;
    }

    pub fn recent(&self, id: &str) -> Option<Recent> {
        self.recent.lock().0.get(id).cloned()
    }

    /// Count a finished ask: its answer, its escalations and its tokens.
    pub fn record(&self, id: &str, r: Recent, usage: &Value) {
        let add = |c: &mut Counts| {
            c.asks += 1;
            if let Some((role, provider, model)) = &r.answered_by {
                *c.answers
                    .entry((
                        role.clone(),
                        provider.clone(),
                        model.clone(),
                        r.outcome.clone(),
                    ))
                    .or_default() += 1;
            }
            if let Some(b) = &r.bucket {
                *c.outcomes
                    .entry((b.clone(), r.outcome.clone()))
                    .or_default() += 1;
            }
            for e in usage["escalations"].as_array().into_iter().flatten() {
                *c.escalations
                    .entry((
                        e["role"].as_str().unwrap_or("").to_string(),
                        e["signal"].as_str().unwrap_or("").to_string(),
                    ))
                    .or_default() += 1;
            }
            for s in usage["steps"].as_array().into_iter().flatten() {
                let t = c
                    .tokens
                    .entry((
                        s["provider"].as_str().unwrap_or("").to_string(),
                        s["model"].as_str().unwrap_or("").to_string(),
                    ))
                    .or_default();
                t.0 += s["inputTokens"].as_u64().unwrap_or(0);
                t.1 += s["outputTokens"].as_u64().unwrap_or(0);
                t.2 += s["estimatedCost"].as_f64().unwrap_or(0.0);
            }
        };
        {
            let mut u = self.usage.lock();
            let d = r.day;
            u.retain(|k, _| k.0 + USAGE_DAYS > d);
            add(u.entry((d, r.dataset.clone())).or_default());
        }
        add(self.totals.lock().entry(r.dataset.clone()).or_default());
        let mut rc = self.recent.lock();
        let (map, order) = &mut *rc;
        if map.insert(id.to_string(), r).is_none() {
            order.push_back(id.to_string());
        }
        while order.len() > MAX_RECENT {
            if let Some(old) = order.pop_front() {
                map.remove(&old);
            }
        }
    }

    /// Move an ask's answer from its outcome to `outcome` in the counts.
    pub fn set_outcome(&self, id: &str, outcome: &str) -> bool {
        let mut rc = self.recent.lock();
        let Some(r) = rc.0.get_mut(id) else {
            return false;
        };
        if r.outcome == outcome {
            return true;
        }
        let old = std::mem::replace(&mut r.outcome, outcome.to_string());
        let r = r.clone();
        drop(rc);
        let mv = |c: &mut Counts| {
            if let Some((role, provider, model)) = &r.answered_by {
                let k = (role.clone(), provider.clone(), model.clone(), old.clone());
                if let Some(n) = c.answers.get_mut(&k) {
                    *n = n.saturating_sub(1);
                    if *n == 0 {
                        c.answers.remove(&k);
                    }
                }
                *c.answers
                    .entry((
                        role.clone(),
                        provider.clone(),
                        model.clone(),
                        outcome.to_string(),
                    ))
                    .or_default() += 1;
            }
            if let Some(b) = &r.bucket {
                let k = (b.clone(), old.clone());
                if let Some(n) = c.outcomes.get_mut(&k) {
                    *n = n.saturating_sub(1);
                    if *n == 0 {
                        c.outcomes.remove(&k);
                    }
                }
                *c.outcomes
                    .entry((b.clone(), outcome.to_string()))
                    .or_default() += 1;
            }
        };
        if let Some(c) = self.usage.lock().get_mut(&(r.day, r.dataset.clone())) {
            mv(c);
        }
        if let Some(c) = self.totals.lock().get_mut(&r.dataset) {
            mv(c);
        }
        true
    }

    /// The counts of the last `days` days, per dataset, with no question or query text.
    pub fn usage_json(&self, days: u64) -> Value {
        let from = today().saturating_sub(days.saturating_sub(1));
        let mut by: BTreeMap<String, Counts> = BTreeMap::new();
        for ((d, ds), c) in self.usage.lock().iter() {
            if *d < from {
                continue;
            }
            let t = by.entry(ds.clone()).or_default();
            t.asks += c.asks;
            for (k, n) in &c.answers {
                *t.answers.entry(k.clone()).or_default() += n;
            }
            for (k, n) in &c.escalations {
                *t.escalations.entry(k.clone()).or_default() += n;
            }
            for (k, n) in &c.outcomes {
                *t.outcomes.entry(k.clone()).or_default() += n;
            }
            for (k, v) in &c.tokens {
                let e = t.tokens.entry(k.clone()).or_default();
                e.0 += v.0;
                e.1 += v.1;
                e.2 += v.2;
            }
        }
        let datasets: Vec<Value> = by
            .into_iter()
            .map(|(ds, c)| {
                let mut j = counts_json(&c);
                j["dataset"] = ds.into();
                j
            })
            .collect();
        json!({ "days": days, "datasets": datasets })
    }
}

/// The asking counters of `/$/metrics`: answers by role and pair, with their current
/// outcomes, escalations by role and signal, and tokens and estimated cost by pair, per
/// dataset since the server started.
pub fn metrics(st: &AppState, o: &mut String) {
    use std::fmt::Write;
    let totals = st.asks.totals.lock().clone();
    if totals.is_empty() {
        return;
    }
    let l = |s: &str| {
        s.replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace('\n', "\\n")
    };
    let family = |o: &mut String, name: &str, kind: &str, help: &str| {
        let _ = writeln!(o, "# HELP {name} {help}");
        let _ = writeln!(o, "# TYPE {name} {kind}");
    };
    family(
        o,
        "sparkles_ask_total",
        "counter",
        "Asks by dataset (C18 §5).",
    );
    for (ds, c) in &totals {
        let _ = writeln!(o, "sparkles_ask_total{{dataset=\"{}\"}} {}", l(ds), c.asks);
    }
    family(
        o,
        "sparkles_ask_answers",
        "gauge",
        "Answers by dataset, the role and pair that wrote the final query, and the outcome its feedback gave (none, accepted, edited, rejected).",
    );
    for (ds, c) in &totals {
        for ((r, p, m, oc), n) in &c.answers {
            let _ = writeln!(
                o,
                "sparkles_ask_answers{{dataset=\"{}\",role=\"{}\",provider=\"{}\",model=\"{}\",outcome=\"{}\"}} {n}",
                l(ds),
                l(r),
                l(p),
                l(m),
                l(oc)
            );
        }
    }
    family(
        o,
        "sparkles_ask_escalations_total",
        "counter",
        "Moves to the next pair of a role's list, by dataset, role and signal.",
    );
    for (ds, c) in &totals {
        for ((r, s), n) in &c.escalations {
            let _ = writeln!(
                o,
                "sparkles_ask_escalations_total{{dataset=\"{}\",role=\"{}\",signal=\"{}\"}} {n}",
                l(ds),
                l(r),
                l(s)
            );
        }
    }
    family(
        o,
        "sparkles_ask_tokens_total",
        "counter",
        "Model tokens of asks by dataset, provider, model and direction.",
    );
    for (ds, c) in &totals {
        for ((p, m), (i, out, _)) in &c.tokens {
            for (dir, n) in [("input", i), ("output", out)] {
                let _ = writeln!(
                    o,
                    "sparkles_ask_tokens_total{{dataset=\"{}\",provider=\"{}\",model=\"{}\",direction=\"{dir}\"}} {n}",
                    l(ds),
                    l(p),
                    l(m)
                );
            }
        }
    }
    family(
        o,
        "sparkles_ask_estimated_cost_total",
        "counter",
        "Estimated cost of asks at the pairs' pricing, by dataset, provider and model.",
    );
    for (ds, c) in &totals {
        for ((p, m), (_, _, cost)) in &c.tokens {
            let _ = writeln!(
                o,
                "sparkles_ask_estimated_cost_total{{dataset=\"{}\",provider=\"{}\",model=\"{}\"}} {cost}",
                l(ds),
                l(p),
                l(m)
            );
        }
    }
}

fn counts_json(c: &Counts) -> Value {
    json!({
        "asks": c.asks,
        "answers": c.answers.iter().map(|((r, p, m, o), n)| json!({"role": r, "provider": p, "model": m, "outcome": o, "count": n})).collect::<Vec<_>>(),
        "escalations": c.escalations.iter().map(|((r, s), n)| json!({"role": r, "signal": s, "count": n})).collect::<Vec<_>>(),
        "outcomes": c.outcomes.iter().map(|((b, o), n)| json!({"bucket": b, "outcome": o, "count": n})).collect::<Vec<_>>(),
        "tokens": c.tokens.iter().map(|((p, m), (i, o, cost))| json!({"provider": p, "model": m, "inputTokens": i, "outputTokens": o, "estimatedCost": cost})).collect::<Vec<_>>(),
    })
}

// ------------------------------------------------------------- the history ------

/// One entry of a principal's history: the question, the final query, the commit, the
/// feedback and the routing record of §5.5, but never the rows or the summary.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AskRecord {
    pub id: String,
    pub principal: String,
    pub at: String,
    pub question: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub query: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit: Option<u64>,
    /// the pipeline's outcome: `answered`, `empty`, `failed` …
    pub result: String,
    /// `accepted`, `edited`, `rejected` or `none` (§5.5)
    pub outcome: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    /// the routing record: complexity, steps, escalations and the answering pair
    pub routing: Value,
}

/// Serializes the read-modify-write of history files.
static HISTORY_LOCK: Mutex<()> = Mutex::new(());

fn history_of(st: &AppState, ds: &Dataset) -> Vec<AskRecord> {
    match read_file(st, ds, ASKS_FILE) {
        Ok(Some(v)) => serde_json::from_value(v.get("asks").cloned().unwrap_or_default())
            .unwrap_or_else(|e| {
                tracing::warn!(dataset = %ds.name, "{ASKS_FILE}: {e}");
                Vec::new()
            }),
        Ok(None) => Vec::new(),
        Err(e) => {
            tracing::warn!(dataset = %ds.name, "{e:#}");
            Vec::new()
        }
    }
}

fn store_history(st: &AppState, ds: &Dataset, list: &[AskRecord]) -> anyhow::Result<()> {
    write_file(st, ds, ASKS_FILE, &json!({ "format": 1, "asks": list }))
}

/// Entries older than `days` days.
fn expired(r: &AskRecord, days: u32) -> bool {
    chrono::DateTime::parse_from_rfc3339(&r.at).is_ok_and(|t| {
        chrono::Utc::now().signed_duration_since(t) > chrono::Duration::days(i64::from(days))
    })
}

/// Keep a finished ask in its principal's history, when the dataset keeps history.
pub fn remember(st: &AppState, ds: &Dataset, rec: AskRecord) {
    let days = settings(st, ds).history_days(st);
    if days == 0 {
        return;
    }
    let _g = HISTORY_LOCK.lock();
    let mut list = history_of(st, ds);
    list.retain(|r| !expired(r, days));
    list.push(rec);
    if list.len() > MAX_HISTORY {
        let drop = list.len() - MAX_HISTORY;
        list.drain(..drop);
    }
    if let Err(e) = store_history(st, ds, &list) {
        tracing::warn!(dataset = %ds.name, "{e:#}");
    }
}

/// Delete the entries older than each dataset's retention (hourly, from `serve`).
pub fn prune(st: &AppState) {
    for ds in st.datasets().values() {
        let days = settings(st, ds).history_days(st);
        let _g = HISTORY_LOCK.lock();
        let list = history_of(st, ds);
        if list.is_empty() {
            continue;
        }
        let kept: Vec<AskRecord> = if days == 0 {
            Vec::new()
        } else {
            list.iter().filter(|r| !expired(r, days)).cloned().collect()
        };
        if kept.len() != list.len()
            && let Err(e) = store_history(st, ds, &kept)
        {
            tracing::warn!(dataset = %ds.name, "{e:#}");
        }
    }
}

/// Run [`prune`] once an hour.
pub fn spawn_pruning(st: Arc<AppState>) {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(std::time::Duration::from_secs(3600));
        loop {
            tick.tick().await;
            let st = st.clone();
            let _ = tokio::task::spawn_blocking(move || prune(&st)).await;
        }
    });
}

/// The record of a finished ask, from the pipeline's output.
pub fn record_of(id: &str, principal: &str, out: &Value) -> AskRecord {
    let u = &out["usage"];
    let routing = json!({
        "complexity": u["complexity"],
        "steps": u["steps"],
        "escalations": u["escalations"],
        "answeredBy": u["answeredBy"],
        "draftPair": u["draftPair"],
        "inputTokens": u["inputTokens"],
        "outputTokens": u["outputTokens"],
        "estimatedCost": u["estimatedCost"],
    });
    AskRecord {
        id: id.to_string(),
        principal: principal.to_string(),
        at: now_rfc3339(),
        question: out["question"].as_str().unwrap_or("").to_string(),
        query: out["result"]["query"].as_str().map(str::to_string),
        commit: out["result"]["commit"].as_u64(),
        result: out["outcome"].as_str().unwrap_or("").to_string(),
        outcome: "none".into(),
        note: None,
        routing,
    }
}

/// The in-memory record of a finished ask.
pub fn recent_of(dataset: &str, principal: &str, out: &Value) -> Recent {
    let u = &out["usage"];
    let ab = &u["answeredBy"];
    Recent {
        dataset: dataset.to_string(),
        principal: principal.to_string(),
        draft_pair: u["draftPair"].as_u64().unwrap_or(0) as usize,
        answered_by: ab.is_object().then(|| {
            (
                ab["role"].as_str().unwrap_or("").to_string(),
                ab["provider"].as_str().unwrap_or("").to_string(),
                ab["model"].as_str().unwrap_or("").to_string(),
            )
        }),
        bucket: u["complexity"]["bucket"].as_str().map(str::to_string),
        outcome: "none".into(),
        day: today(),
    }
}

/// Record feedback on an ask of `principal`: in the counts and, when history is kept,
/// in its entry. `false` when the principal has no such ask.
pub fn feedback(
    st: &AppState,
    ds: &Dataset,
    principal: &str,
    id: &str,
    outcome: &str,
    note: Option<String>,
) -> bool {
    let mine = st
        .asks
        .recent(id)
        .is_some_and(|r| r.principal == principal && r.dataset == ds.name);
    let mut in_history = false;
    {
        let _g = HISTORY_LOCK.lock();
        let mut list = history_of(st, ds);
        if let Some(r) = list
            .iter_mut()
            .find(|r| r.id == id && r.principal == principal)
        {
            r.outcome = outcome.to_string();
            if note.is_some() {
                r.note = note;
            }
            in_history = true;
            if let Err(e) = store_history(st, ds, &list) {
                tracing::warn!(dataset = %ds.name, "{e:#}");
            }
        }
    }
    if mine {
        st.asks.set_outcome(id, outcome);
    }
    mine || in_history
}

// --------------------------------------------------------------- the routes ------

pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/$/assistant/{ds}", get(get_settings).put(put_settings))
        .route("/$/asks/{ds}", get(list_asks).delete(delete_asks))
        .route("/$/asks/{ds}/{id}/feedback", post(post_feedback))
        .route("/$/models/usage", get(usage))
}

/// The settings, with a `status` that says whether asking works and why not.
fn settings_json(st: &AppState, s: &AssistantSettings) -> Value {
    let mut v = serde_json::to_value(s).unwrap_or_default();
    let mut status = json!({
        "models": st.models.is_some(),
        "historyDays": s.history_days(st),
    });
    match ask_status(st, s) {
        Ok(()) => {
            status["ask"] = true.into();
            if let Some(m) = &st.models {
                let draft = s
                    .roles
                    .get(&Role::Draft)
                    .cloned()
                    .unwrap_or_else(|| m.pairs(Role::Draft));
                status["draft"] = serde_json::to_value(&draft).unwrap_or_default();
                let sum = s
                    .roles
                    .get(&Role::Summarize)
                    .cloned()
                    .unwrap_or_else(|| m.pairs(Role::Summarize));
                status["summary"] = (s.send >= Send::Rows
                    && sum.iter().any(|p| s.send_to(&p.provider) >= Send::Rows))
                .into();
            }
        }
        Err(why) => {
            status["ask"] = false.into();
            status["reason"] = why.into();
        }
    }
    v["status"] = status;
    v
}

async fn get_settings(State(st): St, Path(name): Path<String>) -> ApiResult<Json<Value>> {
    let ds = dataset(&st, &name)?;
    Ok(Json(settings_json(&st, &settings(&st, &ds))))
}

async fn put_settings(
    State(st): St,
    Path(name): Path<String>,
    AdminBody(body): AdminBody,
) -> ApiResult<Json<Value>> {
    let ds = dataset(&st, &name)?;
    let s = AssistantSettings::parse(&body, st.models.as_deref())
        .map_err(|m| err_code(StatusCode::BAD_REQUEST, "bad-settings", m))?;
    let v = serde_json::to_value(&s).unwrap_or_default();
    blocking(move || {
        write_file(&st, &ds, ASSISTANT_FILE, &v)
            .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")))?;
        Ok(Json(settings_json(&st, &s)))
    })
    .await
}

#[derive(Deserialize, Default)]
struct ListParams {
    limit: Option<usize>,
    id: Option<String>,
}

async fn list_asks(
    State(st): St,
    Path(name): Path<String>,
    Extension(p): Extension<Principal>,
    Query(q): Query<ListParams>,
) -> ApiResult<Json<Value>> {
    let ds = dataset(&st, &name)?;
    let limit = q.limit.unwrap_or(100).clamp(1, 1000);
    let me = p.id();
    blocking(move || {
        let s = settings(&st, &ds);
        let days = s.history_days(&st);
        let mut list: Vec<AskRecord> = if days == 0 {
            Vec::new()
        } else {
            history_of(&st, &ds)
                .into_iter()
                .filter(|r| r.principal == me && !expired(r, days))
                .collect()
        };
        list.reverse();
        list.truncate(limit);
        Ok(Json(
            json!({ "dataset": ds.name, "historyDays": days, "asks": list }),
        ))
    })
    .await
}

async fn delete_asks(
    State(st): St,
    Path(name): Path<String>,
    Extension(p): Extension<Principal>,
    Query(q): Query<ListParams>,
) -> ApiResult<StatusCode> {
    let ds = dataset(&st, &name)?;
    let me = p.id();
    blocking(move || {
        let _g = HISTORY_LOCK.lock();
        let mut list = history_of(&st, &ds);
        let before = list.len();
        list.retain(|r| r.principal != me || q.id.as_ref().is_some_and(|id| *id != r.id));
        if let Some(id) = &q.id
            && list.len() == before
        {
            return Err(err_code(
                StatusCode::NOT_FOUND,
                "unknown-ask",
                format!("no ask {id:?} in your history"),
            ));
        }
        if list.len() != before {
            store_history(&st, &ds, &list)
                .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")))?;
        }
        Ok(StatusCode::NO_CONTENT)
    })
    .await
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FeedbackBody {
    outcome: String,
    note: Option<String>,
}

async fn post_feedback(
    State(st): St,
    Path((name, id)): Path<(String, String)>,
    Extension(p): Extension<Principal>,
    AdminBody(body): AdminBody,
) -> ApiResult<StatusCode> {
    let ds = dataset(&st, &name)?;
    let b: FeedbackBody =
        serde_json::from_slice(&body).map_err(|e| err(StatusCode::BAD_REQUEST, e.to_string()))?;
    if !matches!(b.outcome.as_str(), "accepted" | "edited" | "rejected") {
        return Err(err_code(
            StatusCode::BAD_REQUEST,
            "bad-argument",
            "outcome must be accepted, edited or rejected",
        ));
    }
    if b.note.as_ref().is_some_and(|n| n.chars().count() > 1000) {
        return Err(err_code(
            StatusCode::BAD_REQUEST,
            "bad-argument",
            "note must be at most 1000 characters",
        ));
    }
    let me = p.id();
    blocking(move || {
        if feedback(&st, &ds, &me, &id, &b.outcome, b.note) {
            Ok(StatusCode::NO_CONTENT)
        } else {
            Err(err_code(
                StatusCode::NOT_FOUND,
                "unknown-ask",
                format!("no ask {id:?} of yours"),
            ))
        }
    })
    .await
}

#[derive(Deserialize, Default)]
struct UsageParams {
    days: Option<u64>,
}

async fn usage(State(st): St, Query(q): Query<UsageParams>) -> ApiResult<Json<Value>> {
    let days = q.days.unwrap_or(30);
    if !(1..=USAGE_DAYS).contains(&days) {
        return Err(err_code(
            StatusCode::BAD_REQUEST,
            "bad-argument",
            format!("days must be from 1 to {USAGE_DAYS}"),
        ));
    }
    Ok(Json(st.asks.usage_json(days)))
}
