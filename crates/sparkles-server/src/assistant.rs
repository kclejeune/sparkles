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
#[cfg(feature = "mcp")]
use crate::models::Models;
use crate::models::{Pair, Role};
use crate::settings::{Providers, Typed};
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

/// The most first drafts that an ask's vote takes.
pub const MAX_DRAFTS: usize = 5;

/// The repair turns of an ask (C18 §4.2), each unset to keep Ask's default. A
/// dataset's `assistant.json` sets them under `repairs`, a request or a command line
/// overrides them, and [`AskRepairs::apply`] puts them into the options.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct AskRepairs {
    /// explore the data once after a draft without a query or with an empty result
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub explore: Option<bool>,
    /// repair once when a result's shape does not fit the question's form
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shape_check: Option<bool>,
    /// the first drafts whose results vote, 1 to `MAX_DRAFTS`
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub drafts: Option<usize>,
    /// link mentions with the classes the words around them name
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub type_hints: Option<bool>,
}

impl AskRepairs {
    fn is_default(&self) -> bool {
        *self == AskRepairs::default()
    }

    /// The values of `over` where it sets them, else these.
    pub fn or(self, over: AskRepairs) -> AskRepairs {
        AskRepairs {
            explore: over.explore.or(self.explore),
            shape_check: over.shape_check.or(self.shape_check),
            drafts: over.drafts.or(self.drafts),
            type_hints: over.type_hints.or(self.type_hints),
        }
    }

    /// Set the options that these name, keeping the others.
    #[cfg(feature = "mcp")]
    pub fn apply(self, o: &mut crate::ask::AskOptions) {
        if let Some(v) = self.explore {
            o.explore = v;
        }
        if let Some(v) = self.shape_check {
            o.shape_check = v;
        }
        if let Some(v) = self.drafts {
            o.drafts = v;
        }
        if let Some(v) = self.type_hints {
            o.type_hints = v;
        }
    }

    /// An error for a draft count out of range, naming the field as `prefix`.
    pub fn check(&self, prefix: &str) -> Result<(), String> {
        match self.drafts {
            Some(n) if !(1..=MAX_DRAFTS).contains(&n) => {
                Err(format!("{prefix}drafts must be from 1 to {MAX_DRAFTS}"))
            }
            _ => Ok(()),
        }
    }
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
    /// the repair turns of the dataset's asks
    #[serde(default, skip_serializing_if = "AskRepairs::is_default")]
    pub repairs: AskRepairs,
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
            repairs: AskRepairs::default(),
        }
    }
}

impl AssistantSettings {
    /// The checks of §3.5: providers and models the server allows, and the ranges of the
    /// numbers. Without the model configuration (`Providers::Unchecked`) the providers
    /// are not checked.
    pub fn validate(&self, providers: Providers) -> Result<(), String> {
        let check = match providers {
            Providers::Unchecked => None,
            Providers::Checked(m) => Some(m),
        };
        for (role, list) in &self.roles {
            for p in list {
                if p.max_output_tokens == Some(0) {
                    return Err(format!(
                        "roles.{}: maxOutputTokens must be at least 1",
                        role.as_str()
                    ));
                }
                let Some(models) = check else {
                    continue;
                };
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
            }
        }
        for name in self.send_by_provider.keys() {
            if let Some(models) = check
                && models.and_then(|m| m.provider(name)).is_none()
            {
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
        self.repairs.check("repairs.")?;
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

impl Typed for AssistantSettings {
    fn validate(&self, providers: Providers) -> Result<(), String> {
        AssistantSettings::validate(self, providers)
    }
}

/// The effective settings of a dataset (spec C19 §4).
pub fn settings(st: &AppState, ds: &Dataset) -> AssistantSettings {
    crate::settings::effective(st, ds, &crate::settings::ASSISTANT)
}

/// The settings of a database directory for `sparkles ask --loc`.
#[cfg(feature = "mcp")]
pub fn settings_at(dir: &std::path::Path) -> anyhow::Result<Option<AssistantSettings>> {
    crate::settings::effective_at(dir, &crate::settings::ASSISTANT)
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

#[cfg(feature = "mcp")]
/// Whether every pair of `roles` may receive rows, taking a role's forced pairs in place of
/// its list. Report prose is data, so only such pairs see it (C22 §6, §7.3).
pub fn rows_allowed(
    s: &AssistantSettings,
    lists: &crate::ask::RoleLists,
    forced: &[(Role, crate::models::Pair)],
    roles: &[Role],
) -> bool {
    let ok = |p: &crate::models::Pair| s.send_to(&p.provider) >= Send::Rows;
    roles.iter().all(|r| {
        let mut own = forced.iter().filter(|(f, _)| f == r).peekable();
        if own.peek().is_some() {
            // a forced pair is the role's whole list
            own.all(|(_, p)| ok(p))
        } else {
            lists.get(r).is_none_or(|l| l.iter().all(ok))
        }
    })
}

/// Whether the dataset answers `POST /{ds}/ask`, and why not.
pub fn ask_status(st: &AppState, s: &AssistantSettings) -> Result<(), &'static str> {
    if !cfg!(feature = "mcp") {
        return Err("this server is built without the mcp feature, which asking needs");
    }
    let Some(m) = st.models() else {
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
    /// (provider, model) → (tokens, estimated cost, budget tokens, each step's rounded
    /// on its own)
    tokens: BTreeMap<(String, String), (crate::models::admission::Usage, f64, u64)>,
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

    /// Count the model calls of a step outside an ask, such as an explanation (C18
    /// §6.6): its tokens against the dataset and `principal`, and its answer and tokens
    /// in the usage counts under `role`. It is not counted as an ask.
    pub fn charge(&self, dataset: &str, principal: &str, role: &str, usage: &Value) {
        let steps = usage["steps"].as_array().cloned().unwrap_or_default();
        let tokens: u64 = steps.iter().map(crate::models::budget_of).sum();
        self.add_tokens(dataset, principal, tokens);
        tracing::info!(
            target: "sparkles::models",
            dataset,
            principal,
            role,
            tokens,
            calls = steps.len(),
            "model calls charged"
        );
        let add = |c: &mut Counts| {
            for s in &steps {
                let pair = (
                    s["provider"].as_str().unwrap_or("").to_string(),
                    s["model"].as_str().unwrap_or("").to_string(),
                );
                if s["outcome"] == "ok" {
                    *c.answers
                        .entry((
                            role.to_string(),
                            pair.0.clone(),
                            pair.1.clone(),
                            "none".into(),
                        ))
                        .or_default() += 1;
                }
                let t = c.tokens.entry(pair).or_default();
                t.0 = t.0.plus(crate::models::usage_json(s));
                t.1 += s["estimatedCost"].as_f64().unwrap_or(0.0);
                t.2 += crate::models::budget_of(s);
            }
        };
        {
            let mut u = self.usage.lock();
            let d = today();
            u.retain(|k, _| k.0 + USAGE_DAYS > d);
            add(u.entry((d, dataset.to_string())).or_default());
        }
        add(self.totals.lock().entry(dataset.to_string()).or_default());
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
                t.0 = t.0.plus(crate::models::usage_json(s));
                t.1 += s["estimatedCost"].as_f64().unwrap_or(0.0);
                t.2 += crate::models::budget_of(s);
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
                e.0 = e.0.plus(v.0);
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
        "Model tokens of asks by dataset, provider, model and direction: uncached input, output, and input written to or read from the provider's prompt cache.",
    );
    for (ds, c) in &totals {
        for ((p, m), (u, _, _)) in &c.tokens {
            let written = u.cache_write_5m + u.cache_write_1h;
            for (dir, n) in [
                ("input", u.input),
                ("output", u.output),
                ("cache_write", written),
                ("cache_read", u.cache_read),
            ] {
                if n == 0 && dir.starts_with("cache") {
                    continue;
                }
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
        for ((p, m), (_, cost, _)) in &c.tokens {
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
        "tokens": c.tokens.iter().map(|((p, m), (u, cost, budget))| {
            let mut j = json!({"provider": p, "model": m, "inputTokens": u.input, "outputTokens": u.output, "estimatedCost": cost});
            crate::models::cache_json(&mut j, u, *budget);
            j
        }).collect::<Vec<_>>(),
    })
}

// ------------------------------------------------------------- the history ------

/// One entry of a principal's history: the question, the final query, the commit, the
/// feedback, the routing record of §5.5, the thread it belongs to and, from format 2,
/// the answer's text, citations and steps, but never the rows.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AskRecord {
    pub id: String,
    pub principal: String,
    pub at: String,
    /// the question as the person wrote it, without the rendered `about`
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
    /// `global` for an answer from community reports (C22 §7)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
    /// the reports a global answer used, shown only while their graph is proven for the
    /// reader (C22 §6.6)
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reports: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reports_graph: Option<String>,
    /// the thread of the ask (format 2); a record without one is a thread of its own
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread: Option<String>,
    /// the resource the question was asked about (format 2)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub about: Option<AskAbout>,
    /// the answer's text, citations, explanation, diagnosis and notes (format 2), shown
    /// only to a reader whose view covers [`AskRecord::access`]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answer: Option<Value>,
    /// what the answer read, for deciding who may see it again
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub access: Option<AnswerAccess>,
}

/// The resource a question is about: the page's context in the UI (C18 §6.2).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct AskAbout {
    pub iri: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

impl AskAbout {
    /// The checks of a request's `about`.
    pub fn check(&self) -> Result<(), String> {
        if self.iri.is_empty()
            || self.iri.chars().count() > 2000
            || oxrdf::NamedNode::new(&self.iri).is_err()
        {
            return Err("about.iri must be an absolute IRI of at most 2000 characters".into());
        }
        if self
            .kind
            .as_ref()
            .is_some_and(|k| k.is_empty() || k.chars().count() > 32)
        {
            return Err("about.kind must hold 1 to 32 characters".into());
        }
        if self.label.as_ref().is_some_and(|l| l.chars().count() > 200) {
            return Err("about.label must be at most 200 characters".into());
        }
        Ok(())
    }

    /// The question as the pipeline reads it: the person's words and the resource.
    pub fn render(&self, question: &str) -> String {
        format!("{question} (about <{}>)", self.iri)
    }
}

/// The most graphs an answer's access record lists, of each kind. A view over more
/// graphs keeps no footprint, and only unrestricted readers see that answer again.
pub const MAX_ACCESS_GRAPHS: usize = 512;

/// What an ask could read, so that its stored answer is shown again only to a reader
/// who can read all of it under the same protections (C18 §6.4). The footprint is the
/// disclosure guard's (C22 §6.2) over every graph the asker's view read; the guarded
/// graphs, such as reports graphs, are checked on their own.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AnswerAccess {
    /// `None` when the graphs could not be described: only unrestricted readers pass
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub footprint: Option<crate::graphrag::guard::Footprint>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub guarded: Vec<String>,
    /// the protections in force on `guarded` for the asker, which a reader must match
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub guarded_footprint: Option<crate::graphrag::guard::Footprint>,
    /// the branch the ask read, `main` when absent
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
}

/// The access record of an ask over `snap`, the state its answer read (`None` when
/// that state cannot be opened, which keeps no footprint), read through `view`
/// (`None`: unrestricted) by `principal`.
pub fn answer_access(
    snap: Option<&Arc<sparkles::store::Snapshot>>,
    view: Option<&Arc<sparkles::access::GraphAccess>>,
    caller: &sparkles::access::Caller,
    dataset: &str,
    branch: Option<&str>,
    principal: &str,
) -> AnswerAccess {
    use crate::graphrag::guard::{CaptureInput, capture, capture_guarded};
    use crate::graphrag::settings::is_guarded;
    let Some(snap) = snap else {
        return AnswerAccess {
            footprint: None,
            guarded: Vec::new(),
            guarded_footprint: None,
            branch: branch.map(str::to_string),
        };
    };
    let bound = view.map(|v| v.bound(snap));
    let mut graphs = vec!["default".to_string()];
    let mut guarded = Vec::new();
    let mut describable = true;
    let ids = snap.graph_ids().unwrap_or_default();
    for g in ids {
        let Some(t) = snap.term(g) else { continue };
        let oxrdf::Term::NamedNode(n) = &t else {
            // a blank-node graph has no name a footprint can keep
            if bound.as_ref().is_none_or(|v| v.read.allows(Some(&t))) {
                describable = false;
            }
            continue;
        };
        if bound.as_ref().is_some_and(|v| !v.read.allows(Some(&t))) {
            continue;
        }
        if is_guarded(n.as_str()) {
            guarded.push(n.as_str().to_string());
        } else {
            graphs.push(n.as_str().to_string());
        }
    }
    if bound.as_ref().is_some_and(|v| !v.read.allows(None)) {
        graphs.retain(|g| g != "default");
    }
    let cap = |graphs: &[String], of_guarded: bool| {
        let i = CaptureInput {
            dataset,
            branch: branch.unwrap_or(sparkles::branch::MAIN),
            source_commit: snap.commit,
            principal,
            endpoint: "query",
            view: bound.as_deref(),
            caller,
            graphs,
            reports: &[],
        };
        if of_guarded {
            capture_guarded(&i).ok()
        } else {
            capture(&i).ok()
        }
    };
    // past either bound, or with protections a footprint cannot name, the record keeps
    // no footprint: dropping a graph would let a narrower reader see what it informed
    let fits =
        describable && graphs.len() <= MAX_ACCESS_GRAPHS && guarded.len() <= MAX_ACCESS_GRAPHS;
    let footprint = if fits { cap(&graphs, false) } else { None };
    let guarded_footprint = if fits && !guarded.is_empty() {
        cap(&guarded, true)
    } else {
        None
    };
    let complete = guarded.is_empty() || guarded_footprint.is_some();
    AnswerAccess {
        footprint: footprint.filter(|_| complete),
        guarded: if fits { guarded } else { Vec::new() },
        guarded_footprint,
        branch: branch.map(str::to_string),
    }
}

/// Whether a reader with `view` (`None`: unrestricted) may see a stored answer.
pub fn answer_visible(
    access: Option<&AnswerAccess>,
    view: Option<&Arc<sparkles::access::GraphAccess>>,
    snap: &Arc<sparkles::store::Snapshot>,
) -> bool {
    use crate::graphrag::guard::{Decision, covers, unrestricted};
    let bound = view.map(|v| v.bound(snap));
    if unrestricted(bound.as_deref()) {
        return true;
    }
    // an answer stored without what it read is seen only by unrestricted readers
    let Some(a) = access else { return false };
    let Some(fp) = &a.footprint else { return false };
    if covers(bound.as_deref(), fp) != Decision::Proven {
        return false;
    }
    // the guarded graphs it read: each readable, under the protections the asker had
    if a.guarded.is_empty() {
        return true;
    }
    let readable = a.guarded.iter().all(|g| {
        bound.as_ref().is_none_or(|v| {
            v.read.allows(Some(&oxrdf::Term::NamedNode(
                oxrdf::NamedNode::new_unchecked(g),
            )))
        })
    });
    readable
        && a.guarded_footprint
            .as_ref()
            .is_some_and(|g| covers(bound.as_deref(), g) == Decision::Proven)
}

/// The most characters of a stored text, and the most items of a stored list.
const MAX_STORED_TEXT: usize = 8000;
const MAX_STORED_ITEMS: usize = 50;

fn cut(v: &Value) -> Option<Value> {
    v.as_str()
        .filter(|s| !s.is_empty())
        .map(|s| Value::from(s.chars().take(MAX_STORED_TEXT).collect::<String>()))
}

fn cut_list(v: &Value, each: impl Fn(&Value) -> Option<Value>) -> Option<Value> {
    let a: Vec<Value> = v
        .as_array()?
        .iter()
        .filter_map(each)
        .take(MAX_STORED_ITEMS)
        .collect();
    (!a.is_empty()).then_some(Value::Array(a))
}

/// The answer a record stores from the pipeline's output and the lists of its
/// `ground` event: never the rows.
pub fn stored_answer(out: &Value, ground: &Value) -> Option<Value> {
    let mut a = serde_json::Map::new();
    let mut claims_kept: Option<Value> = None;
    let r = &out["result"];
    let mut put = |k: &str, v: Option<Value>| {
        if let Some(v) = v {
            a.insert(k.into(), v);
        }
    };
    put("explanation", cut(&r["explanation"]));
    put("assumptions", cut_list(&r["assumptions"], cut));
    put(
        "graph",
        r["graph"].is_object().then(|| {
            json!({
                "subject": cut(&r["graph"]["subject"]),
                "predicate": cut(&r["graph"]["predicate"]),
                "object": cut(&r["graph"]["object"]),
            })
        }),
    );
    let s = &out["summary"];
    if s["text"].is_string() {
        let mut sum = json!({
            "text": cut(&s["text"]),
            "citations": cut_list(&s["citations"], |c| c.as_u64().map(Value::from)).unwrap_or(json!([])),
        });
        for k in ["rowsSent", "provider", "model", "uncited"] {
            if !s[k].is_null() {
                sum[k] = s[k].clone();
            }
        }
        // the claim occurrences the summary cites, with the policy they were read
        // under (C25); the answer's footprint covers the graphs they came from
        if let Some(cited) = cut_list(&s["claimCitations"], |c| c.as_str().map(Value::from)) {
            let ids: Vec<&str> = cited
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .collect();
            let occurrences: Vec<Value> = out["claims"]["occurrences"]
                .as_array()
                .into_iter()
                .flatten()
                .filter(|o| o["id"].as_str().is_some_and(|id| ids.contains(&id)))
                .cloned()
                .collect();
            sum["claimCitations"] = cited.clone();
            claims_kept = Some(json!({
                "version": out["claims"]["version"],
                "policy": out["claims"]["policy"],
                "occurrences": occurrences,
            }));
        }
        put("summary", Some(sum));
    }
    // a global answer's points and reports sit beside its `global` selection
    if out["global"].is_object() || out["points"].is_array() {
        let points = cut_list(&out["points"], |p| {
            Some(json!({
                "n": p["n"].as_u64()?,
                "text": cut(&p["text"])?,
                "reports": cut_list(&p["reports"], cut).unwrap_or(json!([])),
            }))
        });
        let mut gl = json!({ "points": points.unwrap_or(json!([])) });
        if let Some(r) = cut_list(&out["reports"], |r| {
            Some(json!({ "iri": cut(&r["iri"])?, "title": cut(&r["title"]) }))
        }) {
            gl["reports"] = r;
        }
        if let Some(u) = cut_list(&s["uncitedSentences"], cut) {
            gl["uncitedSentences"] = u;
        }
        put("global", Some(gl));
    }
    put("claims", claims_kept.take());
    put("verdict", cut(&r["verdict"]));
    let diag = out["attempts"]
        .as_array()
        .into_iter()
        .flatten()
        .rev()
        .find_map(|t| {
            t["diagnosis"]
                .as_str()
                .or(t["whyEmpty"]["message"].as_str())
        });
    put("diagnosis", diag.and_then(|d| cut(&Value::from(d))));
    put("notes", cut_list(&out["notes"], cut));
    if out["clarify"].is_object() {
        let c = &out["clarify"];
        put(
            "clarify",
            Some(json!({
                "question": cut(&c["question"]),
                "choices": cut_list(&c["choices"], |ch| Some(json!({
                    "label": cut(&ch["label"])?, "value": cut(&ch["value"])?,
                }))).unwrap_or(json!([])),
            })),
        );
    }
    if out["error"].is_object() {
        put(
            "error",
            Some(
                json!({ "code": cut(&out["error"]["code"]), "message": cut(&out["error"]["message"]) }),
            ),
        );
    }
    put(
        "entities",
        cut_list(&ground["entities"], |e| {
            e["candidates"][0]["iri"]
                .as_str()
                .filter(|_| e["verdict"] == "exact")
                .and_then(|i| cut(&Value::from(i)))
        }),
    );
    put("communities", cut_list(&ground["communities"], cut));
    // nulls left by optional members
    for v in a.values_mut() {
        if let Some(o) = v.as_object_mut() {
            o.retain(|_, x| !x.is_null());
        }
    }
    (!a.is_empty()).then_some(Value::Object(a))
}

/// Serializes the read-modify-write of history files.
static HISTORY_LOCK: Mutex<()> = Mutex::new(());

/// Why a history file cannot be read: a newer format, or a damaged file. Either way it
/// is left as it is.
#[derive(Debug)]
pub enum HistoryError {
    Unsupported(String),
    Unreadable(String),
}

impl HistoryError {
    fn api(&self) -> crate::http::ApiError {
        match self {
            HistoryError::Unsupported(m) => {
                err_code(StatusCode::CONFLICT, "unsupported-format", m.clone())
            }
            HistoryError::Unreadable(m) => {
                err_code(StatusCode::CONFLICT, "unreadable-history", m.clone())
            }
        }
    }
}

impl std::fmt::Display for HistoryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            HistoryError::Unsupported(m) | HistoryError::Unreadable(m) => f.write_str(m),
        }
    }
}

/// A dataset's history, oldest first. A file this release cannot read is refused, never
/// read as empty, so that no write replaces it.
pub(crate) fn history_of(st: &AppState, ds: &Dataset) -> Result<Vec<AskRecord>, HistoryError> {
    match read_file(st, ds, ASKS_FILE) {
        Ok(Some(v)) => {
            // read_file checks the files on disk; an in-memory dataset's is checked here
            match sparkles::formats::check_settings(ASKS_FILE, &v) {
                Err(sparkles::Error::Unsupported(m)) => {
                    return Err(HistoryError::Unsupported(format!("{m} ({})", ds.name)));
                }
                Err(e) => return Err(HistoryError::Unreadable(format!("{e} ({})", ds.name))),
                Ok(_) => {}
            }
            serde_json::from_value(v.get("asks").cloned().unwrap_or(json!([])))
                .map_err(|e| HistoryError::Unreadable(format!("{ASKS_FILE} of {}: {e}", ds.name)))
        }
        Ok(None) => Ok(Vec::new()),
        Err(e) => {
            let unsupported = e.chain().any(|c| {
                matches!(
                    c.downcast_ref::<sparkles::Error>(),
                    Some(sparkles::Error::Unsupported(_))
                )
            });
            let m = format!("{e:#}");
            Err(if unsupported {
                HistoryError::Unsupported(m)
            } else {
                HistoryError::Unreadable(m)
            })
        }
    }
}

pub(crate) fn store_history(st: &AppState, ds: &Dataset, list: &[AskRecord]) -> anyhow::Result<()> {
    write_file(
        st,
        ds,
        ASKS_FILE,
        &json!({ "format": HISTORY_FORMAT, "asks": list }),
    )
}

/// The format of `asks.json` this release writes; it reads 1 and 2.
pub const HISTORY_FORMAT: u32 = 2;

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
    let mut list = match history_of(st, ds) {
        Ok(l) => l,
        Err(e) => {
            tracing::warn!(dataset = %ds.name, "the ask is not kept: {e}");
            return;
        }
    };
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
        let list = match history_of(st, ds) {
            Ok(l) => l,
            Err(e) => {
                tracing::warn!(dataset = %ds.name, "the history is not pruned: {e}");
                continue;
            }
        };
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

/// The record of a finished ask, from the pipeline's output. It is a thread of its own
/// and stores no answer until [`AskRecord::with_answer`].
pub fn record_of(id: &str, principal: &str, out: &Value) -> AskRecord {
    let u = &out["usage"];
    let mut routing = json!({
        "complexity": u["complexity"],
        "steps": u["steps"],
        "escalations": u["escalations"],
        "answeredBy": u["answeredBy"],
        "draftPair": u["draftPair"],
        "inputTokens": u["inputTokens"],
        "outputTokens": u["outputTokens"],
        "estimatedCost": u["estimatedCost"],
    });
    for k in [
        "cacheWriteInputTokens",
        "cacheWrite1hInputTokens",
        "cacheReadInputTokens",
        "budgetTokens",
    ] {
        if !u[k].is_null() {
            routing[k] = u[k].clone();
        }
    }
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
        mode: out["mode"].as_str().map(str::to_string),
        reports: out["reports"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|r| r["iri"].as_str().map(str::to_string))
            .collect(),
        reports_graph: out["global"]["reportsGraph"].as_str().map(str::to_string),
        thread: Some(id.to_string()),
        about: None,
        answer: None,
        access: None,
    }
}

impl AskRecord {
    /// The record with its thread, its `about`, the question as written, and the stored
    /// answer with what it read.
    pub fn with_answer(
        mut self,
        thread: &str,
        question: &str,
        about: Option<AskAbout>,
        answer: Option<Value>,
        access: AnswerAccess,
    ) -> AskRecord {
        self.thread = Some(thread.to_string());
        self.question = question.to_string();
        self.about = about;
        self.answer = answer;
        self.access = Some(access);
        self
    }

    /// The thread of the record: its own id when it has none.
    pub fn thread_id(&self) -> &str {
        self.thread.as_deref().unwrap_or(&self.id)
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
        // a file this release cannot read keeps its entries as they are
        let mut list = history_of(st, ds).unwrap_or_else(|e| {
            tracing::warn!(dataset = %ds.name, "the feedback is not kept in the history: {e}");
            Vec::new()
        });
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
        "models": st.models().is_some(),
        "historyDays": s.history_days(st),
    });
    match ask_status(st, s) {
        Ok(()) => {
            status["ask"] = true.into();
            if let Some(m) = st.models() {
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

/// `PUT /$/assistant/{ds}`: the `PUT` of `/$/settings/{ds}/assistant`, with the
/// answer of `GET`.
async fn put_settings(
    State(st): St,
    Path(name): Path<String>,
    headers: axum::http::HeaderMap,
    AdminBody(body): AdminBody,
) -> ApiResult<Json<Value>> {
    let ds = dataset(&st, &name)?;
    let r = crate::settings::http::write(
        st.clone(),
        ds.clone(),
        &crate::settings::ASSISTANT,
        crate::settings::http::Write::Put(body),
        &headers,
    )
    .await?;
    Ok(Json(settings_json(&st, &r.typed(&ds.name))))
}

#[derive(Deserialize, Default)]
struct ListParams {
    limit: Option<usize>,
    id: Option<String>,
    thread: Option<String>,
}

/// Whether `t` is a thread id a request may carry: 1 to 64 of `[A-Za-z0-9_-]`.
pub fn valid_thread(t: &str) -> bool {
    (1..=64).contains(&t.len())
        && t.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

async fn list_asks(
    State(st): St,
    Path(name): Path<String>,
    Extension(p): Extension<Principal>,
    Query(q): Query<ListParams>,
) -> ApiResult<Json<Value>> {
    let ds = dataset(&st, &name)?;
    let limit = q.limit.unwrap_or(100).clamp(1, 1000);
    if q.thread.as_deref().is_some_and(|t| !valid_thread(t)) {
        return Err(err_code(
            StatusCode::BAD_REQUEST,
            "bad-argument",
            "thread must hold 1 to 64 letters, digits, '_' or '-'",
        ));
    }
    let me = p.id();
    blocking(move || {
        let s = settings(&st, &ds);
        let days = s.history_days(&st);
        let mut list: Vec<AskRecord> = if days == 0 {
            Vec::new()
        } else {
            history_of(&st, &ds)
                .map_err(|e| e.api())?
                .into_iter()
                .filter(|r| r.principal == me && !expired(r, days))
                .filter(|r| q.thread.as_deref().is_none_or(|t| r.thread_id() == t))
                .collect()
        };
        // a thread reads oldest first, the whole history newest first
        if q.thread.is_some() {
            let drop = list.len().saturating_sub(limit);
            list.drain(..drop);
        } else {
            list.reverse();
            list.truncate(limit);
        }
        // C22 §6.6: the reports of a global answer only while their graph is proven,
        // in every build, since a history may come from a build with global answers
        let snap = ds.store.snapshot();
        #[cfg_attr(not(feature = "auth"), allow(unused_mut))]
        let mut opts = ds.dataset.query_options();
        #[cfg(feature = "auth")]
        crate::auth::restrict(
            &mut opts,
            &p,
            &ds.name,
            crate::auth::Endpoint::Query,
            &ds.store.hidden_graphs(),
        );
        let out: Vec<Value> = list
            .into_iter()
            .map(|mut r| {
                let reports_hidden = r
                    .reports_graph
                    .as_ref()
                    .is_some_and(|g| !crate::graphrag::guard::readable(&snap, &opts, g));
                if reports_hidden {
                    r.reports.clear();
                    r.reports_graph = None;
                }
                // C18 §6.4: the answer only to a reader whose view covers what it read,
                // and a global answer's text only while its reports are readable
                if r.answer.is_some() {
                    let visible = !reports_hidden
                        && reader_view(&st, &ds, &p, &opts, &snap, r.access.as_ref()).is_some_and(
                            |(view, snap)| answer_visible(r.access.as_ref(), view.as_ref(), &snap),
                        );
                    if !visible {
                        r.answer = None;
                    }
                    let mut v = record_json(&r);
                    if !visible {
                        v["answerWithheld"] = true.into();
                    }
                    v
                } else {
                    record_json(&r)
                }
            })
            .collect();
        Ok(Json(
            json!({ "dataset": ds.name, "historyDays": days, "asks": out }),
        ))
    })
    .await
}

/// A record as `GET /$/asks/{ds}` gives it: always with its thread and the branch it
/// read when that was not main, never with what its answer read.
fn record_json(r: &AskRecord) -> Value {
    let mut v = serde_json::to_value(r).unwrap_or_default();
    if let Some(o) = v.as_object_mut() {
        o.remove("access");
        o.insert("thread".into(), r.thread_id().into());
        if let Some(b) = r
            .access
            .as_ref()
            .and_then(|a| a.branch.as_deref())
            .filter(|b| *b != sparkles::branch::MAIN)
        {
            o.insert("branch".into(), b.into());
        }
    }
    v
}

/// The reader's view of the branch a record's answer read (`None` in it:
/// unrestricted), with that branch's current state, or `None` when the reader may not
/// query the branch or it cannot be opened, which withholds the answer.
#[cfg_attr(not(feature = "auth"), allow(unused_variables))]
#[allow(clippy::type_complexity)]
fn reader_view(
    st: &AppState,
    ds: &Arc<Dataset>,
    p: &Principal,
    opts: &sparkles::sparql::QueryOptions,
    snap: &Arc<sparkles::store::Snapshot>,
    access: Option<&AnswerAccess>,
) -> Option<(
    Option<Arc<sparkles::access::GraphAccess>>,
    Arc<sparkles::store::Snapshot>,
)> {
    let branch = access
        .and_then(|a| a.branch.as_deref())
        .filter(|b| *b != sparkles::branch::MAIN);
    let Some(b) = branch else {
        #[cfg(feature = "auth")]
        if st.auth.is_some()
            && !p.can_at(
                &ds.name,
                crate::auth::Endpoint::Query,
                crate::auth::Level::Read,
            )
        {
            return None;
        }
        return Some((opts.graphs.clone(), snap.clone()));
    };
    let target = st.branch_dataset(ds, b).ok()?;
    let snap = target.store.snapshot();
    #[cfg(feature = "auth")]
    let view = {
        let name = format!("{}@{b}", ds.name);
        // an ungranted branch has no view, which is not an unrestricted one
        if st.auth.is_some()
            && !p.can_at(
                &name,
                crate::auth::Endpoint::Query,
                crate::auth::Level::Read,
            )
        {
            return None;
        }
        p.view(&name, crate::auth::Endpoint::Query)
    };
    #[cfg(not(feature = "auth"))]
    let view = opts.graphs.clone();
    Some((view, snap))
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
        let mut list = history_of(&st, &ds).map_err(|e| e.api())?;
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

#[cfg(test)]
mod usage_tests {
    use super::*;

    /// The usage counters add each step's own budget tokens, as the charges do, so two
    /// steps that each read 513 cached tokens count 52 + 52 for the pair, over two
    /// requests too, and never the 103 of their summed reads.
    #[test]
    fn usage_counters_keep_each_steps_budget() {
        let rt = Runtime::default();
        let step = json!({ "provider": "p", "model": "m", "outcome": "ok", "inputTokens": 0,
            "outputTokens": 0, "cacheWriteInputTokens": 0, "cacheReadInputTokens": 513,
            "budgetTokens": 52 });
        let usage = json!({ "steps": [step.clone(), step.clone()] });
        rt.charge("ds", "ana", "explain", &usage);
        let t = &rt.usage_json(1)["datasets"][0]["tokens"][0];
        assert_eq!(t["budgetTokens"], 104, "{t}");
        assert_eq!(t["cacheReadInputTokens"], 1026);
        rt.charge("ds", "ana", "explain", &json!({ "steps": [step] }));
        assert_eq!(
            rt.usage_json(1)["datasets"][0]["tokens"][0]["budgetTokens"],
            156
        );
    }
}

#[cfg(all(test, feature = "mcp"))]
mod repair_tests {
    use super::*;

    #[test]
    fn repairs_parse_merge_and_apply() {
        let s: AssistantSettings = serde_json::from_str(
            r#"{"repairs": {"explore": false, "shapeCheck": true, "drafts": 3, "typeHints": true}}"#,
        )
        .unwrap();
        assert!(s.validate(crate::settings::Providers::Unchecked).is_ok());
        let mut o = crate::ask::AskOptions::default();
        s.repairs.apply(&mut o);
        assert!(!o.explore && o.shape_check && o.drafts == 3 && o.type_hints);
        let over = AskRepairs {
            drafts: Some(1),
            ..AskRepairs::default()
        };
        assert_eq!(s.repairs.or(over).drafts, Some(1));
        assert_eq!(s.repairs.or(over).shape_check, Some(true));
        let bad: AssistantSettings = serde_json::from_str(r#"{"repairs": {"drafts": 6}}"#).unwrap();
        assert!(bad.validate(crate::settings::Providers::Unchecked).is_err());
        assert!(serde_json::from_str::<AssistantSettings>(r#"{"repairs": {"vote": 2}}"#).is_err());
        // unset repairs are left out of the file
        assert!(
            !serde_json::to_string(&AssistantSettings::default())
                .unwrap()
                .contains("repairs")
        );
    }
}
