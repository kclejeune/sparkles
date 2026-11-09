//! The asking pipeline of C18 §4 and §5 as a library: ground, clarify, draft, check,
//! run, repair and summarize, with the same MCP tools an agent calls behind each step.
//!
//! [`ask`] runs one question for one principal over that principal's view. Every query
//! it runs has passed `check_query`, runs through `sparql_query` (which never writes)
//! under the MCP limits, and a `SELECT` without a limit gets `LIMIT 1000`. The model has
//! no tools: the pipeline decides each step and passes only the draft's query on.
//!
//! Each role has an ordered list of provider and model pairs (§3.7). A pointer per role
//! starts at the first pair and moves to the next only on a signal the pipeline observes
//! itself (§5.5): a repaired draft that still fails its check or its run
//! (`check-failed`), a repaired query that is empty with the verdict `query`
//! (`empty-query`), a call that times out, is refused or answers invalid output
//! (`provider-failure`), a complex draft, example or earlier turn (`complexity`), and
//! **Try harder** (`try-harder`). One ask moves at most two pairs in each role and makes
//! at most two failed calls.
//!
//! `POST /{ds}/ask` (`crate::ask::http`) streams the events of §5.2, and `sparkles ask`
//! drives the same function from the command line for the evaluation of §11.4.

#[cfg(test)]
mod claims_tests;
mod cli;
pub mod complexity;
mod entity_evidence;
mod global;
mod grounding;
pub mod http;
#[cfg(test)]
mod http_tests;
pub(crate) mod profile;
mod prompts;
mod repair;
#[cfg(test)]
mod routing_tests;
#[cfg(test)]
mod tests;

pub use cli::{AskArgs, run_cli};

use crate::auth::Principal;
use crate::mcp::errors::ToolError;
use crate::mcp::{Call, McpServer, Outcome};
use crate::models::{Level, Models, Pair, Part, Prompt, Role, StepError, StepRecord};
use serde_json::{Map, Value, json};
use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// The most repairs per question (§4.2).
pub const MAX_REPAIRS: u32 = 2;
/// The most failed model calls per question (§5.3).
pub const MAX_FAILED_CALLS: u32 = 2;
/// The most pairs one ask moves along each role's list (§5.5).
pub const MAX_MOVES: u32 = 2;
/// The limit added to a `SELECT` without one (§4.3).
pub const ADDED_LIMIT: u64 = 1000;
/// The complexity score from which the stronger pair starts (§5.5).
pub const DEFAULT_COMPLEXITY_THRESHOLD: u32 = 8;
/// The score from which the best stored example's complexity counts (§5.5).
pub const DEFAULT_EXAMPLE_SCORE: f64 = 0.8;
/// The most communities a local question's grounding names (C22 §7.3).
const MAX_COMMUNITIES: usize = 3;
/// The linked entities whose claims an ask that names a review policy reads.
const MAX_CLAIM_SEEDS: usize = 20;
/// An entity no dataset describes, which checks an ask's policies before it starts.
const PREFLIGHT_SEED: &str = "urn:x-sparkles:evidence:ask-preflight";
/// The linked entities and matched classes whose predicate use grounding reads.
const MAX_USAGE_ENTITIES: usize = 3;
const MAX_USAGE_CLASSES: usize = 2;
/// The predicates listed for an entity as subject and as object, and for a class.
const USAGE_OUT: usize = 60;
const USAGE_IN: usize = 20;
const USAGE_CLASS: usize = 30;
/// The incoming triples of an entity and the instances of a class that are sampled.
const USAGE_SAMPLE_TRIPLES: usize = 5000;
const USAGE_SAMPLE_INSTANCES: usize = 200;
/// The longest one predicate-use query may run.
const USAGE_QUERY_TIMEOUT: Duration = Duration::from_secs(5);
/// The entities an exploration reads, the predicates it lists for each as subject and
/// as object, and the further link candidates it shows for each mention.
const EXPLORE_ENTITIES: usize = 3;
const EXPLORE_OUT: usize = 40;
const EXPLORE_IN: usize = 20;
const EXPLORE_CANDIDATES: usize = 5;
/// The most first drafts that vote on an answer.
pub use crate::assistant::MAX_DRAFTS;
/// The most earlier turns a follow-up question carries (§5.1).
pub const MAX_CONTEXT: usize = 5;
/// Below this context window the grounding is trimmed (§3.6).
const SMALL_CONTEXT: u64 = 16_384;
/// Below this context window the summary is left out (§3.6).
const TINY_CONTEXT: u64 = 8_192;
/// The summary is skipped when less time than this is left.
const SUMMARY_MIN_LEFT: Duration = Duration::from_secs(5);

/// The pairs of each role, in order.
pub type RoleLists = BTreeMap<Role, Vec<Pair>>;

/// The lists of each role after a dataset's overrides (§3.5): a role the overrides name
/// replaces the server's list, an empty list turns the role off, and an absent role
/// uses its fallback after overrides (`repair` uses `draft`, `community` uses
/// `summarize`).
pub fn resolve_lists(models: &Models, overrides: &RoleLists) -> RoleLists {
    let mut out = RoleLists::new();
    for r in Role::ALL {
        if let Some(l) = overrides.get(&r).or_else(|| models.config.roles.get(&r)) {
            out.insert(r, l.clone());
        }
    }
    for role in Role::ALL {
        if !out.contains_key(&role)
            && let Some(fallback) = role.fallback()
            && let Some(pairs) = out.get(&fallback).cloned()
        {
            out.insert(role, pairs);
        }
    }
    out
}

/// Whether a question is answered from the data or from community reports (C22 §2.4).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Deserialize, clap::ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum AskMode {
    /// classify the question (C22 §7.1)
    #[default]
    Auto,
    /// answer from the data with a query
    Local,
    /// answer from the community reports
    Global,
}

impl AskMode {
    pub fn as_str(self) -> &'static str {
        match self {
            AskMode::Auto => "auto",
            AskMode::Local => "local",
            AskMode::Global => "global",
        }
    }
}

/// An earlier turn of the same conversation (§5.1).
#[derive(Clone, Debug, PartialEq)]
pub struct Turn {
    pub question: String,
    pub query: String,
}

/// What one ask is asked to do.
#[derive(Clone, Debug)]
pub struct AskOptions {
    pub dataset: String,
    pub question: String,
    /// the ask's id, reported in `usage` (`POST /{ds}/ask` gives one)
    pub id: Option<String>,
    /// pairs forced for some roles (`--pair`): the role's list is that pair alone
    pub pairs: Vec<(Role, Pair)>,
    /// the lists of each role (a dataset's, from [`resolve_lists`]); the server's
    /// lists when `None`
    pub lists: Option<RoleLists>,
    /// **Try harder**: the draft starts at this pair of its list (§5.5)
    pub draft_start: Option<usize>,
    /// earlier turns, oldest first, at most [`MAX_CONTEXT`]
    pub context: Vec<Turn>,
    /// the answer to a `clarify` of an earlier ask of the same question
    pub clarification: Option<String>,
    /// a query to check, run and summarize instead of drafting one (the UI's
    /// **Summarize again** after an edit)
    pub query: Option<String>,
    /// run the checked query (false: stop after the check)
    pub run: bool,
    /// summarize the rows
    pub summary: bool,
    /// the rows returned in the result
    pub max_rows: usize,
    /// the rows sent to the summary
    pub rows_for_summary: usize,
    /// the deadline of the whole ask
    pub deadline: Duration,
    /// the token budget of the whole ask
    pub max_tokens: u64,
    /// the complexity score from which a stronger pair starts (§5.5)
    pub complexity_threshold: u32,
    /// the score from which the best stored example's complexity counts
    pub example_score: f64,
    /// the state the tools read, as `at` of `/{ds}/sparql`
    pub at: Option<Value>,
    /// the branch the tools read
    pub branch: Option<String>,
    pub reasoning: Option<bool>,
    /// also return the result in the `application/x-sparkles+json` form of
    /// `/{ds}/sparql` as `result.results`, which the UI renders
    pub full_results: bool,
    /// set when the client went away: the pipeline stops at its next step
    pub cancel: Option<Arc<AtomicBool>>,
    /// the query memory grant the HTTP request was admitted with, which the ask's tool
    /// queries charge. The pipeline runs on a blocking thread, where the request's
    /// task-local grant does not reach, so the route captures it here.
    pub grant: Option<Arc<sparkles::sparql::pool::PoolGrant>>,
    /// answer from the data, from community reports, or as the question needs
    pub mode: AskMode,
    /// use only reviewed community reports (`reviewedOnly`, C22 A7)
    pub reviewed_only: bool,
    /// the draft and repair pairs may receive rows, so their grounding may hold the
    /// communities of the linked entities (C22 §7.3), an exploration may show sample
    /// values and a repair of a result's shape may show its first rows
    pub report_context: bool,
    /// explore the data once when a draft gives no query or its query returns no rows,
    /// and draft again with what the exploration found, on by default
    pub explore: bool,
    /// repair once when a result's shape does not fit the form of the question, and
    /// keep the first answer when the repair does not answer
    pub shape_check: bool,
    /// the first drafts asked, whose results vote on the query, 1 to [`MAX_DRAFTS`]
    pub drafts: usize,
    /// link each mention with the classes that the words around it name as type
    /// hints, and show how many triples name each candidate
    pub type_hints: bool,
    /// cite claim occurrences under this review policy in the summary (C25)
    pub review_policy: Option<String>,
    /// resolve the linked entities through this matching policy's map (C25)
    pub matching_policy: Option<String>,
}

impl Default for AskOptions {
    fn default() -> AskOptions {
        AskOptions {
            dataset: String::new(),
            question: String::new(),
            id: None,
            pairs: Vec::new(),
            lists: None,
            draft_start: None,
            context: Vec::new(),
            clarification: None,
            query: None,
            run: true,
            summary: true,
            max_rows: 1000,
            rows_for_summary: 50,
            deadline: Duration::from_secs(120),
            max_tokens: 50_000,
            complexity_threshold: DEFAULT_COMPLEXITY_THRESHOLD,
            example_score: DEFAULT_EXAMPLE_SCORE,
            at: None,
            branch: None,
            reasoning: None,
            full_results: false,
            cancel: None,
            grant: None,
            mode: AskMode::Auto,
            reviewed_only: false,
            report_context: false,
            explore: true,
            shape_check: false,
            drafts: 1,
            type_hints: false,
            review_policy: None,
            matching_policy: None,
        }
    }
}

/// A step's event, as §5.2 names them: `ground`, `clarify`, `draft`, `escalate`,
/// `check`, `run`, `diagnosis`, `result`, `summary`, `usage` and `error`.
pub type OnEvent<'a> = dyn FnMut(&str, &Value) + 'a;

/// Run one question. The returned object holds `outcome` (`answered`, `empty`,
/// `clarify`, `unanswerable`, `checked`, `not-run`, `failed` or `error`), and the
/// `result`, `summary`, `clarify`, `error`, `attempts`, `usage` and `notes` that apply.
pub fn ask(
    server: &McpServer,
    models: &Models,
    principal: &Principal,
    o: &AskOptions,
    on: &mut OnEvent,
) -> Value {
    let started = Instant::now();
    let profile = profile::pipeline();
    let pipeline = tracing::debug_span!(target: profile::TARGET, "ask.pipeline", pipeline_id = profile.id(), context_tokens = tracing::field::Empty);
    let _entered = pipeline.enter();
    let mut lists = o.lists.clone().unwrap_or_else(|| {
        Role::ALL
            .into_iter()
            .map(|r| (r, models.pairs(r)))
            .filter(|(_, l)| !l.is_empty())
            .collect()
    });
    for (r, p) in &o.pairs {
        lists.insert(*r, vec![p.clone()]);
    }
    let mut a = Asking {
        server,
        models,
        principal,
        o,
        lists,
        pos: BTreeMap::new(),
        moved: BTreeMap::new(),
        pending: BTreeMap::new(),
        deadline: started + o.deadline,
        steps: Vec::new(),
        signals: Vec::new(),
        failed_calls: 0,
        escalations: Vec::new(),
        attempts: Vec::new(),
        notes: Vec::new(),
        on,
        complexity: None,
        level: None,
        trimmed: Value::Null,
        answered_by: None,
        reports: None,
        fallback: None,
        held: Vec::new(),
        claim_seeds: Vec::new(),
        claims: None,
        withheld: false,
    };
    let mut out = a.run();
    // without a summary, the answer still carries the claims, read for the caller now
    if (o.review_policy.is_some() || o.matching_policy.is_some())
        && out["outcome"] == "answered"
        && a.claims.is_none()
        && !a.withheld
    {
        let seeds = a.claim_seeds.clone();
        match a.claims_recall(&seeds) {
            Ok(c) => a.claims = Some(c),
            Err(_) => a.withheld = true,
        }
    }
    if a.withheld {
        // the evidence the summary read changed or became unreadable before it was
        // published, so the answer is withheld without saying which (C25)
        let e = json!({
            "code": "forbidden",
            "message": "the answer is withheld because the evidence it used is no longer the same for you; ask again",
        });
        (a.on)("error", &e);
        out = json!({ "outcome": "error", "error": e });
    } else if let Some(c) = a.claims.take()
        && out["outcome"] == "answered"
    {
        out["claims"] = c["claims"].clone();
        if !c["resolution"].is_null() {
            out["resolution"] = c["resolution"].clone();
        }
    }
    // an event stream learns of the endings without their own event here
    match out["outcome"].as_str() {
        Some("unanswerable") => (a.on)(
            "error",
            &json!({
                "code": "unanswerable",
                "message": "The data does not seem to describe this.",
                "result": out["result"],
            }),
        ),
        Some("failed") => {
            let mut e = out.get("error").cloned().unwrap_or_else(|| {
                json!({
                    "code": "no-valid-query",
                    "message": "Sparkles could not write a valid query for this question.",
                })
            });
            e["result"] = out["result"].clone();
            (a.on)("error", &e);
        }
        Some("not-run") => (a.on)("result", &out["result"]),
        _ => {}
    }
    if out.get("mode").is_none() && out["outcome"] != "error" {
        out["mode"] = AskMode::Local.as_str().into();
    }
    out["dataset"] = o.dataset.clone().into();
    out["question"] = o.question.clone().into();
    out["attempts"] = Value::Array(std::mem::take(&mut a.attempts));
    out["notes"] = json!(a.notes);
    let mut usage = a.usage(started);
    usage["outcome"] = out["outcome"].clone();
    if !a.notes.is_empty() {
        usage["notes"] = json!(a.notes);
    }
    (a.on)("usage", &usage);
    out["usage"] = usage;
    out
}

struct Asking<'a, 'e> {
    server: &'a McpServer,
    models: &'a Models,
    principal: &'a Principal,
    o: &'a AskOptions,
    lists: RoleLists,
    /// the current pair of each role, as an index into its list
    pos: BTreeMap<Role, usize>,
    /// the pairs each role moved in this ask
    moved: BTreeMap<Role, u32>,
    /// the signal that moved a role, reported with its next step
    pending: BTreeMap<Role, &'static str>,
    deadline: Instant,
    steps: Vec<StepRecord>,
    /// the signal behind each step, when one moved its role
    signals: Vec<Option<&'static str>>,
    failed_calls: u32,
    escalations: Vec<Value>,
    attempts: Vec<Value>,
    notes: Vec<String>,
    on: &'a mut OnEvent<'e>,
    complexity: Option<complexity::Complexity>,
    level: Option<Level>,
    trimmed: Value,
    /// the role and pair that wrote the final query
    answered_by: Option<(Role, Pair)>,
    /// the community reports a global answer may use, as `community_reports` returns
    /// them, when the question may be answered from them (C22 §7)
    reports: Option<Value>,
    /// the answer that a repair of its result's shape may replace
    fallback: Option<Answered>,
    /// the terminal events of that repair, which wait while `fallback` stands
    held: Vec<(String, Value)>,
    /// the entities the question links exactly, which the claims grounding reads
    claim_seeds: Vec<String>,
    /// the `recall` result of the claims grounding, with `reviewPolicy`
    claims: Option<Value>,
    /// the claims changed for the caller before the summary was published
    withheld: bool,
}

/// What `run_query` returns: the compact result and, with `full_results`, the full one.
type RunResult = (Value, Option<Value>);

/// A draft with its output level and pair, and the query and result of its run when
/// the vote of several drafts ran it.
type Drafted = (Value, Level, Pair, Option<(String, RunResult)>);

/// An answer with rows, before its summary.
struct Answered {
    result: Value,
    query: String,
    doc: Value,
    rows: u64,
    by: Option<(Role, Pair)>,
}

/// The grounding of §4.1 step 1.
struct Ground {
    /// the context that stays the same for every question of the dataset at this
    /// context size: the common prefixes, classes and predicates
    stable: String,
    /// the context of this question, after `stable`
    context: String,
    /// a mention that links `ambiguous`, with its candidates
    ambiguous: Option<(String, Vec<Value>)>,
    /// the best stored example's query, when its score reaches `example_score`
    best_example: Option<String>,
    /// a mention links `exact`, which makes the question local (C22 §7.1)
    exact: bool,
    /// the question's mentions, the IRIs of the candidates the context shows, the
    /// entities its predicate use reads, how many candidates each mention showed, and
    /// the schema's prefixes, which an exploration reads
    mentions: Vec<String>,
    shown: std::collections::BTreeSet<String>,
    usage_entities: Vec<String>,
    link_k: usize,
    prefixes: Map<String, Value>,
}

/// The graphs that usage queries read.
struct UsageScope {
    named: bool,
    inferred: String,
}

impl UsageScope {
    /// `pattern` over the default graph and, when the caller can read one, every
    /// named graph, binding the graph to `?g`.
    fn view(&self, pattern: &str, g: &str) -> String {
        if !self.named {
            return format!("{{ {pattern} }}");
        }
        format!(
            "{{ {{ {pattern} }} UNION {{ GRAPH ?{g} {{ {pattern} }}{} }} }}",
            self.inferred.replace("{g}", g)
        )
    }
}

/// A link candidate as a line of the context: its IRI, label and types, and with
/// `uses` how many triples name it as their object.
fn candidate_line(c: &Value, uses: bool) -> String {
    let types: Vec<&str> = c["types"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect();
    let mut line = format!(
        "  {} {} types: {}",
        c["iri"].as_str().unwrap_or(""),
        label(c),
        types.join(" ")
    );
    if uses && let Some(n) = c["uses"].as_u64() {
        line.push_str(&format!(" uses: {n}"));
    }
    line.push('\n');
    line
}

/// A normalized draft.
struct Draft {
    query: String,
    explanation: String,
    assumptions: Vec<String>,
    clarify: Option<(String, Vec<String>)>,
    graph: Option<Value>,
}

impl Asking<'_, '_> {
    fn emit(&mut self, event: &str, v: &Value) {
        // while a repair of a result's shape may still give way to the first answer,
        // its terminal events wait, so a stream never reports both
        if self.fallback.is_some() && matches!(event, "error" | "result" | "clarify") {
            self.held.push((event.to_string(), v.clone()));
            return;
        }
        (self.on)(event, v);
    }

    fn error(&mut self, code: &str, message: impl Into<String>) -> Value {
        let e = json!({ "code": code, "message": message.into() });
        self.emit("error", &e);
        json!({ "outcome": "error", "error": e })
    }

    fn cancelled(&self) -> bool {
        self.o
            .cancel
            .as_ref()
            .is_some_and(|c| c.load(Ordering::Relaxed))
    }

    fn list(&self, role: Role) -> &[Pair] {
        self.lists.get(&role).map_or(&[], Vec::as_slice)
    }

    /// The current pair of `role`.
    fn pair(&self, role: Role) -> Option<Pair> {
        self.list(role)
            .get(self.pos.get(&role).copied().unwrap_or(0))
            .cloned()
    }

    /// Move `role` to its next pair on `signal`, unless the list ends there or the
    /// role moved twice already (§5.5).
    fn escalate(&mut self, role: Role, signal: &'static str) -> bool {
        let i = self.pos.get(&role).copied().unwrap_or(0);
        let list = self.list(role);
        if self.moved.get(&role).copied().unwrap_or(0) >= MAX_MOVES || i + 1 >= list.len() {
            return false;
        }
        let pair_json = |p: &Pair| json!({ "provider": p.provider, "model": p.model });
        let ev = json!({
            "role": role.as_str(),
            "from": pair_json(&list[i]),
            "to": pair_json(&list[i + 1]),
            "signal": signal,
        });
        self.pos.insert(role, i + 1);
        *self.moved.entry(role).or_default() += 1;
        self.pending.insert(role, signal);
        self.emit("escalate", &ev);
        self.escalations.push(ev);
        true
    }

    /// The tokens the ask counts against its budget, each class of input at its price.
    fn tokens(&self) -> u64 {
        self.steps.iter().map(StepRecord::budget_tokens).sum()
    }

    fn push_step(&mut self, role: Role, rec: StepRecord) {
        self.signals.push(self.pending.remove(&role));
        self.steps.push(rec);
    }

    /// One answer for `role` from its current pair. A provider failure runs the same
    /// step on the next pair (`provider-failure`), within the two failed calls of an
    /// ask. The error is the code and message that end the step.
    fn call(
        &mut self,
        role: Role,
        prompt: &Prompt,
        out: &crate::models::OutputSchema,
    ) -> Result<(Value, Level, Pair), (&'static str, String)> {
        loop {
            if self.cancelled() {
                return Err(("cancelled", "the ask was cancelled".into()));
            }
            let Some(pair) = self.pair(role) else {
                return Err((
                    "no-model",
                    format!("no provider and model answers the {} role", role.as_str()),
                ));
            };
            match self
                .models
                .call_prompt(Some(role), &pair, prompt, out, self.deadline, None)
            {
                Ok(a) => {
                    self.push_step(role, a.record);
                    return Ok((a.value, a.level, pair));
                }
                Err(f) => {
                    self.push_step(role, *f.record);
                    let message = f.error.message();
                    match f.error {
                        StepError::Budget(_) => return Err(("budget-exceeded", message)),
                        StepError::Deadline => return Err(("timeout", message)),
                        _ => {
                            self.failed_calls += 1;
                            if self.failed_calls >= MAX_FAILED_CALLS
                                || !self.escalate(role, "provider-failure")
                            {
                                return Err(("provider-unavailable", message));
                            }
                        }
                    }
                }
            }
        }
    }

    /// Whether the offered tool `name` takes the argument `arg`.
    fn takes(&self, name: &str, arg: &str) -> bool {
        self.server
            .tools()
            .iter()
            .find(|t| t.name == name)
            .is_some_and(|t| t.input["properties"].get(arg).is_some())
    }

    /// A tool's arguments with the dataset, the ask's state and the deadline, and the
    /// call to run it as the principal.
    fn tool_call(
        &self,
        name: &str,
        args: Value,
        timed: bool,
    ) -> Result<(Map<String, Value>, Call), ToolError> {
        let mut m = match args {
            Value::Object(m) => m,
            _ => Map::new(),
        };
        m.insert("dataset".into(), self.o.dataset.clone().into());
        if let Some(b) = &self.o.branch
            && self.takes(name, "branch")
        {
            m.insert("branch".into(), b.clone().into());
        }
        if let Some(at) = &self.o.at
            && self.takes(name, "at")
        {
            m.insert("at".into(), at.clone());
        }
        if let Some(r) = self.o.reasoning
            && self.takes(name, "reasoning")
        {
            m.insert("reasoning".into(), r.into());
        }
        if timed {
            let left = self.deadline.saturating_duration_since(Instant::now());
            let secs = left.min(self.server.cfg().max_timeout).as_secs_f64();
            if secs < 0.05 {
                return Err(ToolError::new("timeout", 504, "the ask's deadline passed"));
            }
            m.insert("timeoutSeconds".into(), json!(secs));
        }
        let call = Call {
            arrived: Instant::now(),
            cancel: self.o.cancel.clone().unwrap_or_default(),
            request_id: "ask".into(),
            principal: self.principal.clone(),
            headers: None,
            held: None,
            grant: self.o.grant.clone(),
        };
        Ok((m, call))
    }

    /// Call a tool as the principal, on this thread, under the ask's deadline.
    fn tool(&self, name: &str, args: Value, timed: bool) -> Result<Value, ToolError> {
        if name == "check_query" {
            return profile::tool(profile::Stage::QueryCheck, || {
                self.tool_unprofiled(name, args, timed)
            });
        }
        self.tool_unprofiled(name, args, timed)
    }

    fn tool_unprofiled(&self, name: &str, args: Value, timed: bool) -> Result<Value, ToolError> {
        let (m, call) = self.tool_call(name, args, timed)?;
        Ok(match self.server.run_now(name, m, &call)? {
            Outcome::Structured(v) => v,
            Outcome::Text(t) => serde_json::from_str(&t).unwrap_or(Value::String(t)),
        })
    }

    /// Run a query with `sparql_query`: its compact result, and with `full_results` the
    /// same result in the form of `/{ds}/sparql`.
    fn run_query(&self, query: &str) -> Result<RunResult, ToolError> {
        profile::execution(|| self.run_query_unprofiled(query))
    }

    fn run_query_unprofiled(&self, query: &str) -> Result<(Value, Option<Value>), ToolError> {
        let args = json!({
            "query": query,
            "format": "json",
            "maxRows": self.o.max_rows.min(self.server.cfg().max_rows),
            "maxBytes": self.server.cfg().max_bytes,
        });
        let text = |o: Outcome| match o {
            Outcome::Structured(v) => v,
            Outcome::Text(t) => serde_json::from_str(&t).unwrap_or(Value::String(t)),
        };
        let (m, call) = self.tool_call("sparql_query", args, true)?;
        if self.o.full_results {
            let (o, full) = crate::mcp::tools::sparql_query_full(self.server, m, &call)?;
            Ok((text(o), Some(full)))
        } else {
            Ok((text(self.server.run_now("sparql_query", m, &call)?), None))
        }
    }

    /// Parse a query with the dataset's prefixes.
    fn parse(&self, q: &str) -> Option<spargebra::Query> {
        let ds = self
            .server
            .dataset(self.principal, Some(&self.o.dataset))
            .ok()?;
        let pv: Vec<(String, String)> = crate::mcp::tools::dataset_prefixes(&ds)
            .into_iter()
            .collect();
        sparkles::sparql::parse_query(q, None, &pv).ok()
    }

    /// `q` with a declaration of each well-known prefix that it uses without declaring
    /// and that the dataset does not define ([`prompts::complete_prefixes`]).
    fn declare_prefixes(&self, q: &str) -> String {
        let Ok(ds) = self.server.dataset(self.principal, Some(&self.o.dataset)) else {
            return q.to_string();
        };
        let pv: Vec<(String, String)> = crate::mcp::tools::dataset_prefixes(&ds)
            .into_iter()
            .collect();
        prompts::complete_prefixes(q, &pv)
    }

    /// The complexity score of a query, when it parses.
    fn score(&self, q: &str) -> Option<u32> {
        self.parse(q).map(|p| complexity::of(&p).score())
    }

    fn run(&mut self) -> Value {
        if self.o.question.trim().is_empty() || self.o.question.chars().count() > 2000 {
            return self.error(
                "bad-argument",
                "the question must have 1 to 2000 characters",
            );
        }
        if let Err(e) = self.server.dataset(self.principal, Some(&self.o.dataset)) {
            return self.error(&e.code, e.message);
        }
        if self.o.review_policy.is_some() || self.o.matching_policy.is_some() {
            if self.o.mode == AskMode::Global {
                return self.error(
                    "bad-argument",
                    "reviewPolicy and matchingPolicy apply to answers from the data, not to mode global",
                );
            }
            // the policies and the store's configuration history are checked before
            // any model call, so a refused policy costs nothing
            if let Err(e) = self.claims_recall(&[PREFLIGHT_SEED.to_string()]) {
                return self.error(&e.code, e.message);
            }
        }
        if let Some(q) = self.o.query.clone() {
            return self.given(&q);
        }
        // C22 §7: the reports a global answer may use, and the explicit global mode.
        // An ask that cites claims answers from the data.
        if self.o.mode != AskMode::Local
            && self.o.review_policy.is_none()
            && self.o.matching_policy.is_none()
        {
            self.reports = self.report_selection();
            if self.o.mode == AskMode::Global
                && let Some(sel) = self.reports.take()
            {
                return self.global(sel);
            }
        }
        if self.pair(Role::Draft).is_none() {
            return self.error("no-model", "no provider and model answers the draft role");
        }
        // Try harder: the draft starts at the pair after the one that answered before
        if let Some(k) = self.o.draft_start
            && k > 0
        {
            let len = self.list(Role::Draft).len();
            if k >= len {
                return self.error(
                    "no-later-pair",
                    "the draft role has no pair after the one that answered",
                );
            }
            let list = self.list(Role::Draft);
            let ev = json!({
                "role": "draft",
                "from": { "provider": list[k - 1].provider, "model": list[k - 1].model },
                "to": { "provider": list[k].provider, "model": list[k].model },
                "signal": "try-harder",
            });
            self.pos.insert(Role::Draft, k);
            self.moved.insert(Role::Draft, 1);
            self.pending.insert(Role::Draft, "try-harder");
            self.emit("escalate", &ev);
            self.escalations.push(ev);
        }
        let draft_pair = self.pair(Role::Draft).unwrap_or_else(|| Pair::new("", ""));
        let context_tokens = self
            .models
            .resolved(&draft_pair)
            .map_or(crate::models::DEFAULT_CONTEXT_TOKENS, |r| r.context_tokens);
        profile::context_tokens(context_tokens);
        tracing::Span::current().record("context_tokens", context_tokens);
        let ground = self.ground(context_tokens);
        // C22 §7.1: a question that names an entity linked exactly is local
        if ground.exact {
            self.reports = None;
        }
        // §4.4: an ambiguous mention is asked about before drafting, once
        if self.o.clarification.is_none()
            && let Some((text, candidates)) = &ground.ambiguous
        {
            let choices: Vec<Value> = candidates
                .iter()
                .map(|c| {
                    let types: Vec<&str> = c["types"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(Value::as_str)
                        .collect();
                    let label = c["label"]
                        .as_str()
                        .unwrap_or(c["iri"].as_str().unwrap_or(""));
                    let label = if types.is_empty() {
                        label.to_string()
                    } else {
                        format!("{label} ({})", types.join(", "))
                    };
                    json!({ "label": label, "value": c["iri"] })
                })
                .collect();
            let c = json!({
                "id": "mention",
                "question": format!("Which {text:?} do you mean?"),
                "choices": choices,
            });
            self.emit("clarify", &c);
            return json!({ "outcome": "clarify", "clarify": c });
        }
        // §5.5: a complex nearest example or earlier turn starts the draft one pair later
        if self.o.draft_start.is_none() {
            let threshold = self.o.complexity_threshold;
            let complex = ground
                .best_example
                .as_deref()
                .and_then(|q| self.score(q))
                .is_some_and(|s| s >= threshold)
                || self
                    .o
                    .context
                    .last()
                    .and_then(|t| self.score(&t.query))
                    .is_some_and(|s| s >= threshold);
            if complex {
                self.escalate(Role::Draft, "complexity");
            }
        }
        let mut notes = Vec::new();
        let turns: Vec<&Turn> = self.o.context.iter().rev().take(MAX_CONTEXT).collect();
        if !turns.is_empty() {
            let mut t = String::from(
                "The question follows earlier questions of the same conversation, oldest first. It may refer to them, such as \"and their managers?\".\n",
            );
            for turn in turns.into_iter().rev() {
                t.push_str(&format!(
                    "Earlier question: {}\nIts query:\n```sparql\n{}\n```\n",
                    prompts::data_block(&turn.question),
                    turn.query.trim()
                ));
            }
            notes.push(t);
        }
        if let Some(c) = &self.o.clarification {
            notes.push(format!(
                "The person answered a clarification with: {}. Use that reading and do not ask again.",
                prompts::data_block(c)
            ));
        }
        self.draft_loop(&ground, &notes, context_tokens)
    }

    /// Step 1: the schema terms, the examples and the linked mentions, trimmed for a
    /// small context window.
    fn ground(&mut self, context_tokens: u64) -> Ground {
        profile::ground(context_tokens, || self.ground_unprofiled(context_tokens))
    }

    fn ground_tool(
        &self,
        stage: profile::Stage,
        name: &str,
        args: Value,
        timed: bool,
    ) -> Result<Value, ToolError> {
        profile::tool(stage, || self.tool(name, args, timed))
    }

    fn ground_unprofiled(&mut self, context_tokens: u64) -> Ground {
        let small = context_tokens < SMALL_CONTEXT;
        let (max_classes, max_predicates, examples_k, candidates_k) = if small {
            (30, 60, 2, 3)
        } else {
            (100, 150, 5, 5)
        };
        let mut ctx = String::new();
        let mut ground_event = json!({});
        // Keep common vocabulary for joins and labels alongside question matches.
        // Both calls read the caller-visible cached report, never evaluation gold.
        let common = self
            .ground_tool(
                profile::Stage::SchemaCommon,
                "describe_schema",
                json!({"section":"summary","limit":max_predicates / 2}),
                false,
            )
            .unwrap_or(Value::Null);
        let mut schema = self
            .ground_tool(
                profile::Stage::SchemaQuestion,
                "describe_schema",
                json!({"section":"summary","limit":max_predicates,"query":self.o.question}),
                false,
            )
            .unwrap_or(Value::Null);
        // The common vocabulary leads every question's context, so that it can stay in
        // a provider's prompt cache (C18 §3.6): the common prefixes, then the common
        // classes and predicates that `balanced_entries` puts first. The rest of each
        // list follows in the question's part, under the same heading.
        let common_prefixes = common["prefixes"].as_object().cloned().unwrap_or_default();
        let extra_prefixes: serde_json::Map<String, Value> = schema["prefixes"]
            .as_object()
            .into_iter()
            .flatten()
            .filter(|(k, _)| !common_prefixes.contains_key(*k))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        let mut prefixes = schema["prefixes"].as_object().cloned().unwrap_or_default();
        prefixes.extend(common_prefixes.clone());
        if schema.is_null() {
            schema = json!({});
        }
        schema["prefixes"] = Value::Object(prefixes);
        let mut stable = String::new();
        let prefix_lines = |p: &serde_json::Map<String, Value>| -> String {
            p.iter()
                .map(|(k, v)| format!("{k}: <{}>\n", v.as_str().unwrap_or("")))
                .collect()
        };
        if !common_prefixes.is_empty() {
            stable.push_str("Prefixes:\n");
            stable.push_str(&prefix_lines(&common_prefixes));
        }
        if !extra_prefixes.is_empty() {
            ctx.push_str(if stable.is_empty() {
                "Prefixes:\n"
            } else {
                "\nPrefixes:\n"
            });
            ctx.push_str(&prefix_lines(&extra_prefixes));
        }
        let classes =
            grounding::balanced_entries(&common["classes"], &schema["classes"], max_classes);
        let predicates = grounding::balanced_entries(
            &common["predicates"],
            &schema["predicates"],
            max_predicates,
        );
        let class_head = grounding::common_head(&common["classes"], &classes, max_classes);
        let predicate_head =
            grounding::common_head(&common["predicates"], &predicates, max_predicates);
        let class_line = |c: &Value| {
            format!(
                "{} {} {}\n",
                c["iri"].as_str().unwrap_or(""),
                label(c),
                c["instances"]
            )
        };
        let predicate_line = |p: &Value| {
            let objects: Vec<&str> = p["objects"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .collect();
            format!(
                "{} {} {} {}\n",
                p["iri"].as_str().unwrap_or(""),
                label(p),
                p["triples"],
                objects.join(" ")
            )
        };
        const CLASSES: &str = "\nClasses (IRI, label, instances):\n";
        const PREDICATES: &str =
            "\nPredicates (IRI, label, triples, kinds or classes of the values):\n";
        // the stable part ends with every class and predicate of the head, and the
        // question's part holds the rest of each list under its heading
        let (stable_classes, rest_classes) = classes.split_at(class_head);
        let (stable_predicates, rest_predicates) = predicates.split_at(predicate_head);
        if !stable_classes.is_empty() {
            stable.push_str(CLASSES);
            stable.extend(stable_classes.iter().map(class_line));
        }
        if !stable_predicates.is_empty() {
            stable.push_str(PREDICATES);
            stable.extend(stable_predicates.iter().map(predicate_line));
        }
        if !rest_classes.is_empty() {
            ctx.push_str(CLASSES);
            ctx.extend(rest_classes.iter().map(class_line));
        }
        if !rest_predicates.is_empty() {
            ctx.push_str(PREDICATES);
            ctx.extend(rest_predicates.iter().map(predicate_line));
        }
        ground_event["classes"] = classes.iter().map(|c| c["iri"].clone()).collect();
        ground_event["predicates"] = predicates.iter().map(|c| c["iri"].clone()).collect();
        // stored examples
        let examples = self
            .ground_tool(
                profile::Stage::Examples,
                "similar_queries",
                json!({"question": self.o.question, "k": examples_k, "withText": true}),
                true,
            )
            .map(|v| v["queries"].as_array().cloned().unwrap_or_default())
            .unwrap_or_default();
        if !examples.is_empty() {
            ctx.push_str("\nStored example queries that people accepted:\n");
            for e in &examples {
                ctx.push_str(&format!(
                    "# {}: {}\n",
                    e["name"].as_str().unwrap_or(""),
                    e["description"].as_str().unwrap_or("")
                ));
                if let Some(qs) = e["questions"].as_array()
                    && !qs.is_empty()
                {
                    let qs: Vec<String> = qs.iter().map(Value::to_string).collect();
                    ctx.push_str(&format!("# questions: {}\n", qs.join(", ")));
                }
                ctx.push_str(e["query"].as_str().unwrap_or("").trim());
                ctx.push_str("\n\n");
            }
        }
        let best_example = examples
            .first()
            .filter(|e| {
                e["score"]
                    .as_f64()
                    .is_some_and(|s| s >= self.o.example_score)
            })
            .and_then(|e| e["query"].as_str())
            .map(str::to_string);
        ground_event["examples"] = examples.iter().map(|e| e["name"].clone()).collect();
        // linked mentions
        let mut ambiguous = None;
        let mut exact = false;
        let mentions = mentions(&self.o.question);
        let mut entities = Vec::new();
        let mut link_prefixes = Value::Null;
        let mut shown = std::collections::BTreeSet::new();
        if !mentions.is_empty() {
            let list: Vec<Value> = mentions
                .iter()
                .map(|m| {
                    let mut v = json!({ "text": m });
                    if self.o.type_hints {
                        let around = crate::mcp::relevance::words(&repair::mention_context(
                            &self.o.question,
                            m,
                        ));
                        let types =
                            grounding::matched_classes(&classes, &around, &schema["prefixes"], 2);
                        if !types.is_empty() {
                            v["types"] = json!(types);
                        }
                    }
                    v
                })
                .collect();
            if let Ok(v) = self
                .ground_tool(
                    profile::Stage::Linker,
                    "link_entities",
                    json!({"mentions": list, "k": candidates_k, "evidence": true, "popularity": true}),
                    true,
                )
                .or_else(|error| {
                    if error.code.as_ref() == "budget-result-bytes" {
                        self.ground_tool(
                            profile::Stage::Linker,
                            "link_entities",
                            json!({"mentions": list, "k": candidates_k, "popularity": true}),
                            true,
                        )
                    } else {
                        Err(error)
                    }
                })
            {
                link_prefixes = v["prefixes"].clone();
                let ms = v["mentions"].as_array().cloned().unwrap_or_default();
                let mut lines = String::new();
                for m in &ms {
                    let text = m["text"].as_str().unwrap_or("");
                    let verdict = m["verdict"].as_str().unwrap_or("none");
                    let mut cands = m["candidates"].as_array().cloned().unwrap_or_default();
                    for candidate in &mut cands {
                        entity_evidence::restore_identity(candidate);
                    }
                    if verdict == "ambiguous"
                        && let Some(index) = entity_evidence::preferred_display_candidate(
                            text,
                            &self.o.question,
                            &m["evidence"],
                            &cands,
                            &v["prefixes"],
                        )
                    {
                        // The bounded display is not proof of uniqueness. Reorder
                        // offered choices without changing the linker's verdict.
                        let chosen = cands.remove(index);
                        cands.insert(0, chosen);
                    }
                    exact |= verdict == "exact";
                    if verdict == "ambiguous" && ambiguous.is_none() {
                        ambiguous = Some((text.to_string(), cands.clone()));
                    }
                    entities.push(json!({
                        "text": text,
                        "verdict": verdict,
                        "candidates": cands.iter().map(|c| json!({"iri": c["iri"], "label": c["label"]})).collect::<Vec<_>>(),
                    }));
                    if verdict == "none" {
                        continue;
                    }
                    lines.push_str(&format!("{} ({verdict}):\n", Value::from(text)));
                    for c in &cands {
                        if let Some(i) = c["iri"].as_str() {
                            shown.insert(grounding::expanded(i, &v["prefixes"]));
                        }
                        lines.push_str(&candidate_line(c, self.o.type_hints));
                    }
                }
                if !lines.is_empty() {
                    ctx.push_str("\nEntities that the question may name:\n");
                    ctx.push_str(&lines);
                }
            }
        }
        // The predicates that the linked entities and the instances of the question's
        // classes use, from the data itself, so that a relation the question names in
        // other words than the schema's labels still reaches the context.
        // the first candidate of each mention that links, which an exploration reads
        // whether or not the grounding read their predicate use
        let mut usage_entities: Vec<String> = entities
            .iter()
            .filter(|e| matches!(e["verdict"].as_str(), Some("exact" | "ambiguous")))
            .filter_map(|e| e["candidates"][0]["iri"].as_str())
            .map(|i| grounding::expanded(i, &link_prefixes))
            .collect();
        if !small {
            let words = crate::mcp::relevance::words(&self.o.question);
            let mut iris: Vec<String> = Vec::new();
            if let Some(a) = self
                .o
                .clarification
                .as_deref()
                .and_then(|a| grounding::answered_iri(a, &link_prefixes))
            {
                iris.push(a);
            }
            for e in &entities {
                if matches!(e["verdict"].as_str(), Some("exact" | "ambiguous"))
                    && let Some(i) = e["candidates"][0]["iri"].as_str()
                {
                    iris.push(grounding::expanded(i, &link_prefixes));
                }
            }
            let mut seen = std::collections::BTreeSet::new();
            iris.retain(|i| seen.insert(i.clone()));
            iris.truncate(MAX_USAGE_ENTITIES);
            let classes = grounding::matched_classes(
                &classes,
                &words,
                &schema["prefixes"],
                MAX_USAGE_CLASSES,
            );
            let prefixes = schema["prefixes"].as_object().cloned().unwrap_or_default();
            let section = self.predicate_use(&iris, &classes, &words, &prefixes);
            if !section.is_empty() {
                ctx.push_str(&section);
                ground_event["predicateUse"] = json!({ "entities": &iris, "classes": classes });
            }
            iris.extend(usage_entities);
            usage_entities = iris;
        }
        // the entities the question links exactly, which the claims grounding reads
        self.claim_seeds = entities
            .iter()
            .filter(|e| matches!(e["verdict"].as_str(), Some("exact")))
            .filter_map(|e| e["candidates"][0]["iri"].as_str().map(str::to_string))
            .take(MAX_CLAIM_SEEDS)
            .collect();
        // C22 §7.3: the communities of the linked entities, at most three
        let linked: Vec<Value> = entities
            .iter()
            .filter(|e| matches!(e["verdict"].as_str(), Some("exact")))
            .filter_map(|e| e["candidates"][0]["iri"].as_str().map(Value::from))
            .take(MAX_COMMUNITIES)
            .collect();
        if !linked.is_empty()
            && self.o.report_context
            && let Ok(v) = self.ground_tool(
                profile::Stage::CommunityRecall,
                "recall",
                json!({ "seeds": linked, "hops": 0, "maxTriples": 1, "communities": true, "format": "json" }),
                true,
            )
        {
            let v = match v {
                Value::String(t) => serde_json::from_str(&t).unwrap_or_default(),
                v => v,
            };
            let reports: Vec<&Value> = v["communities"]["reports"]
                .as_array()
                .into_iter()
                .flatten()
                .take(MAX_COMMUNITIES)
                .collect();
            if !reports.is_empty() {
                ctx.push_str("\nCommunities of the entities the question names, from the dataset's community reports:\n");
                for r in &reports {
                    ctx.push_str(&format!(
                        "{} {}: {}\n",
                        r["community"].as_str().unwrap_or(""),
                        Value::from(r["title"].as_str().unwrap_or("")),
                        Value::from(prompts::cut(r["summary"].as_str().unwrap_or(""), 600))
                    ));
                }
                ground_event["communities"] = reports.iter().map(|r| r["iri"].clone()).collect();
            }
        }
        ground_event["entities"] = Value::Array(entities);
        self.trimmed = json!({
            "contextTokens": context_tokens,
            "trimmed": small,
            "classes": classes.len(),
            "predicates": predicates.len(),
            "examples": examples.len(),
        });
        ground_event["trimmed"] = small.into();
        self.emit("ground", &ground_event);
        Ground {
            stable,
            context: ctx,
            ambiguous,
            best_example,
            exact,
            mentions,
            shown,
            usage_entities,
            link_k: candidates_k,
            prefixes: schema["prefixes"].as_object().cloned().unwrap_or_default(),
        }
    }

    /// One bounded query of the grounding or an exploration: at most 200 rows, within
    /// the ask's deadline and five seconds. A query that fails or times out gives none.
    fn usage_query(&self, stage: profile::Stage, q: String) -> Option<Value> {
        let cfg = self.server.cfg();
        let left = self.deadline.saturating_duration_since(Instant::now());
        let secs = left
            .min(USAGE_QUERY_TIMEOUT)
            .min(cfg.max_timeout)
            .as_secs_f64();
        if secs < 0.5 {
            return None;
        }
        self.ground_tool(
            stage,
            "sparql_query",
            json!({"query": q, "format": "json", "maxRows": cfg.max_rows.min(200), "timeoutSeconds": secs}),
            false,
        )
        .ok()
    }

    /// The graphs a usage query reads: the default graph and every named graph the
    /// caller may read, as the linker searches them, without materialized inferences
    /// unless the ask reads them.
    fn usage_scope(&self) -> UsageScope {
        let inferred = if self.o.reasoning == Some(true) {
            String::new()
        } else {
            format!(" FILTER(?{{g}} != <{}>)", crate::http::INFERRED_GRAPH)
        };
        // A union with GRAPH patterns costs a scan of every named graph for each pattern,
        // so it is used only when the caller can read a named graph at all.
        let named = self
            .usage_query(
                profile::Stage::PredicateUse,
                format!(
                    "SELECT ?g WHERE {{ GRAPH ?g {{ ?s ?p ?o }}{} }} LIMIT 1",
                    inferred.replace("{g}", "g")
                ),
            )
            .is_none_or(|v| v["rows"].as_array().is_none_or(|rows| !rows.is_empty()));
        UsageScope { named, inferred }
    }

    /// One exploration of the data around a question whose draft found nothing: the
    /// further link candidates of each mention that the context did not show, and the
    /// predicates that the failed query's entities and the linked entities use as
    /// subject and as object, with counts and a sample value. Every query is bounded
    /// like the predicate-use grounding, and the text is empty when nothing was found.
    fn explore(&self, ground: &Ground, entities: &[String]) -> String {
        let mut out = String::new();
        if !ground.mentions.is_empty() && ground.link_k > 0 {
            let list: Vec<Value> = ground
                .mentions
                .iter()
                .map(|m| json!({ "text": m }))
                .collect();
            let k = (ground.link_k * 2).min(20);
            if let Ok(v) = self.tool(
                "link_entities",
                json!({"mentions": list, "k": k, "popularity": true}),
                true,
            ) {
                for m in v["mentions"].as_array().into_iter().flatten() {
                    let more: Vec<&Value> = m["candidates"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter(|c| {
                            c["iri"].as_str().is_some_and(|i| {
                                !ground
                                    .shown
                                    .contains(&grounding::expanded(i, &v["prefixes"]))
                            })
                        })
                        .take(EXPLORE_CANDIDATES)
                        .collect();
                    if more.is_empty() {
                        continue;
                    }
                    out.push_str(&format!(
                        "More entities that {} may name:\n",
                        Value::from(m["text"].as_str().unwrap_or(""))
                    ));
                    for c in more {
                        out.push_str(&candidate_line(c, true));
                    }
                }
            }
        }
        let mut iris: Vec<String> = entities.to_vec();
        iris.extend(ground.usage_entities.iter().cloned());
        let mut seen = std::collections::BTreeSet::new();
        iris.retain(|i| grounding::safe_iri(i) && seen.insert(i.clone()));
        iris.truncate(EXPLORE_ENTITIES);
        let scope = self.usage_scope();
        let max_rows = self.server.cfg().max_rows.min(200);
        for e in &iris {
            let name = grounding::compact(e, &ground.prefixes);
            let outgoing = format!(
                "SELECT ?p (COUNT(*) AS ?n) (SAMPLE(?o) AS ?x) WHERE {{ {} }} GROUP BY ?p ORDER BY DESC(?n) LIMIT {max_rows}",
                scope.view(&format!("<{e}> ?p ?o"), "g")
            );
            let incoming = format!(
                "SELECT ?p (COUNT(*) AS ?n) (SAMPLE(?s) AS ?x) WHERE {{ {{ SELECT ?p ?s WHERE {{ {} }} LIMIT {USAGE_SAMPLE_TRIPLES} }} }} GROUP BY ?p ORDER BY DESC(?n) LIMIT {max_rows}",
                scope.view(&format!("?s ?p <{e}>"), "g")
            );
            for (q, side, max) in [
                (outgoing, "subject", EXPLORE_OUT),
                (incoming, "object", EXPLORE_IN),
            ] {
                if let Some(l) = self
                    .usage_query(profile::Stage::PredicateUse, q)
                    .and_then(|v| {
                        // a sample value is data, which only a pair that may receive
                        // rows sees
                        repair::usage_line(
                            &format!("{name} as {side}"),
                            &v,
                            &ground.prefixes,
                            max,
                            self.o.report_context,
                        )
                    })
                {
                    out.push_str(&l);
                }
            }
        }
        out
    }

    /// Lines naming the predicates each of `entities` uses as subject and as object, and
    /// that a sample of the instances of each of `classes` uses, with their counts.
    /// Each query is bounded, and one that fails or times out only leaves its line out.
    fn predicate_use(
        &self,
        entities: &[String],
        classes: &[String],
        words: &std::collections::BTreeSet<String>,
        prefixes: &Map<String, Value>,
    ) -> String {
        const SKIP: [&str; 5] = [
            "http://www.w3.org/1999/02/22-rdf-syntax-ns#type",
            "http://www.w3.org/2000/01/rdf-schema#label",
            "http://www.w3.org/2002/07/owl#sameAs",
            "http://www.w3.org/2004/02/skos/core#prefLabel",
            "http://www.w3.org/2004/02/skos/core#altLabel",
        ];
        let max_rows = self.server.cfg().max_rows.min(200);
        let run = |q: String| self.usage_query(profile::Stage::PredicateUse, q);
        let line = |subject: String, rows: &Value, max: usize| -> Option<String> {
            let ranked = grounding::ranked_usage(rows, words, &SKIP, max);
            (!ranked.is_empty())
                .then(|| format!("{subject}: {}\n", grounding::grouped(&ranked, prefixes)))
        };
        let scope = self.usage_scope();
        let view = |pattern: &str, g: &str| scope.view(pattern, g);
        let mut lines = String::new();
        for e in entities.iter().filter(|e| grounding::safe_iri(e)) {
            let name = grounding::compact(e, prefixes);
            let outgoing = format!(
                "SELECT ?p (COUNT(*) AS ?n) WHERE {{ {} }} GROUP BY ?p ORDER BY DESC(?n) LIMIT {max_rows}",
                view(&format!("<{e}> ?p ?o"), "g")
            );
            let incoming = format!(
                "SELECT ?p (COUNT(*) AS ?n) WHERE {{ {{ SELECT ?p WHERE {{ {} }} LIMIT {USAGE_SAMPLE_TRIPLES} }} }} GROUP BY ?p ORDER BY DESC(?n) LIMIT {max_rows}",
                view(&format!("?s ?p <{e}>"), "g")
            );
            if let Some(l) =
                run(outgoing).and_then(|v| line(format!("{name} as subject"), &v, USAGE_OUT))
            {
                lines.push_str(&l);
            }
            if let Some(l) =
                run(incoming).and_then(|v| line(format!("{name} as object"), &v, USAGE_IN))
            {
                lines.push_str(&l);
            }
        }
        for c in classes.iter().filter(|c| grounding::safe_iri(c)) {
            let name = grounding::compact(c, prefixes);
            let q = format!(
                "SELECT ?p (COUNT(DISTINCT ?s) AS ?n) WHERE {{ {{ SELECT DISTINCT ?s WHERE {{ {} }} LIMIT {USAGE_SAMPLE_INSTANCES} }} {} }} GROUP BY ?p ORDER BY DESC(?n) LIMIT {max_rows}",
                view(&format!("?s a <{c}>"), "g1"),
                view("?s ?p ?o", "g2")
            );
            if let Some(l) = run(q).and_then(|v| {
                line(
                    format!("Instances of {name}, a sample of up to {USAGE_SAMPLE_INSTANCES}, as subject"),
                    &v,
                    USAGE_CLASS,
                )
            }) {
                lines.push_str(&l);
            }
        }
        if lines.is_empty() {
            return lines;
        }
        format!(
            "\nPredicates that these entities and a sample of these classes' instances use, grouped by namespace, with how many triples or instances use each:\n{lines}"
        )
    }

    /// Steps 3 to 7: draft, check, run, repair at most twice, then summarize, moving
    /// along the role lists on the signals of §5.5. When a repair of a result's shape
    /// does not answer, the first answer stands.
    fn draft_loop(&mut self, ground: &Ground, notes: &[String], context_tokens: u64) -> Value {
        let out = self.draft_turns(ground, notes, context_tokens);
        if out["outcome"] == "answered" {
            return out;
        }
        match self.fallback.take() {
            Some(a) => {
                // the repair's own terminal events give way to the first answer's
                self.held.clear();
                self.notes.push(
                    "the repair of the result's shape did not answer, so the first answer stands"
                        .into(),
                );
                self.answered_by = a.by.clone();
                self.answer(a, context_tokens)
            }
            None => out,
        }
    }

    /// The answered outcome of a result with rows, with its summary.
    fn answer(&mut self, a: Answered, context_tokens: u64) -> Value {
        self.emit("result", &a.result);
        let mut out = json!({ "outcome": "answered", "result": a.result });
        if let Some(s) = self.summarize(&a.query, &a.doc, a.rows, context_tokens) {
            out["summary"] = s;
        }
        out
    }

    /// Whether the ask has the time and tokens for one more model call.
    fn room(&self) -> bool {
        self.tokens() < self.o.max_tokens
            && self.deadline.saturating_duration_since(Instant::now()) > Duration::from_secs(10)
    }

    /// The first draft of several that their results vote for (`drafts`). Each
    /// further draft is asked with the same prompt while the ask has room, and each
    /// query that passes the check runs once. A first draft that asks for a
    /// clarification stands alone. The
    /// chosen draft keeps its own output level and pair, and its query's result, so
    /// the pipeline does not run it again.
    fn vote_drafts(
        &mut self,
        first: Drafted,
        prompt: &Prompt,
        schema: &crate::models::OutputSchema,
    ) -> Drafted {
        if normalize(&first.0).clarify.is_some() && self.o.clarification.is_none() {
            return first;
        }
        let mut drafted = vec![first];
        while drafted.len() < self.o.drafts.min(MAX_DRAFTS) && self.room() {
            match self.call(Role::Draft, prompt, schema) {
                Ok(x) => drafted.push((x.0, x.1, x.2, None)),
                Err(_) => break,
            }
        }
        let mut signatures = Vec::with_capacity(drafted.len());
        for x in &mut drafted {
            let mut d = normalize(&x.0);
            d.query = self.declare_prefixes(&d.query);
            let parsed = (!d.query.is_empty())
                .then(|| self.parse(&d.query))
                .flatten();
            if parsed.is_none() || has_service(parsed.as_ref()) {
                signatures.push(None);
                continue;
            }
            // a draft that fails the check, such as one with an unknown class, does not
            // vote, since its result says nothing about the question
            let checked = self
                .tool(
                    "check_query",
                    json!({"query": d.query, "explain": true, "terms": true}),
                    true,
                )
                .is_ok_and(|c| c["ok"] == Value::Bool(true));
            if !checked {
                signatures.push(None);
                continue;
            }
            let (q, limited) = self.with_limit(&d.query, parsed.as_ref());
            match self.run_query(&q) {
                Ok(r) => {
                    // a result that reaches the limit Ask added may have more rows
                    let capped = limited && r.0["returned"].as_u64().unwrap_or(0) >= ADDED_LIMIT;
                    signatures.push((!capped).then(|| repair::signature(&r.0)).flatten());
                    x.3 = Some((q, r));
                }
                Err(_) => signatures.push(None),
            }
        }
        let chosen = repair::vote(&signatures);
        let agree = signatures
            .iter()
            .filter(|s| s.is_some() && **s == signatures[chosen])
            .count();
        self.emit(
            "vote",
            &json!({ "drafts": drafted.len(), "withRows": signatures.iter().filter(|s| s.is_some()).count(), "agree": agree, "chosen": chosen }),
        );
        drafted.swap_remove(chosen)
    }

    fn draft_turns(&mut self, ground: &Ground, notes: &[String], context_tokens: u64) -> Value {
        let mut repair: Option<String> = None;
        let mut repairs = 0;
        let mut attempt = 0;
        let mut explored = !self.o.explore;
        let mut shaped = !self.o.shape_check;
        let mut entity_terms: Vec<String> = Vec::new();
        loop {
            attempt += 1;
            let role = if attempt == 1 {
                Role::Draft
            } else {
                Role::Repair
            };
            if self.tokens() >= self.o.max_tokens {
                return self.error(
                    "budget-exceeded",
                    format!("the ask used its {} tokens", self.o.max_tokens),
                );
            }
            // the first draft also classifies the question when reports could answer it
            let scoped = attempt == 1 && self.reports.is_some();
            let mut user = prompts::draft_user(
                &self.o.question,
                &ground.stable,
                &ground.context,
                notes,
                repair.as_deref(),
            );
            if scoped {
                // the scoped schema differs from the repairs' schema, which changes the
                // whole cached prompt, so no repair could read the question's part
                user[1].cache = false;
                user[2].text.push_str(prompts::SCOPE_FORMAT);
            }
            let schema = if scoped {
                prompts::draft_scoped()
            } else {
                prompts::draft()
            };
            // a repair also offers its follow-up form, which an OpenAI provider with
            // prompt caching receives so that it reads the first draft's cached prefix
            let follow = repair.as_deref().filter(|_| !scoped).map(|r| {
                prompts::draft_follow(&self.o.question, &ground.stable, &ground.context, notes, r)
            });
            let prompt = Prompt {
                system: vec![Part::cached(prompts::SYSTEM)],
                user,
                follow,
            };
            let (value, level, pair) = match self.call(role, &prompt, &schema) {
                Ok(x) => x,
                Err((code, message)) => return self.error(code, message),
            };
            let mut ran: Option<(String, RunResult)> = None;
            if scoped && value["scope"].as_str() == Some("global") {
                let sel = self.reports.take().unwrap_or(Value::Null);
                self.emit(
                    "scope",
                    &json!({ "scope": "global", "provider": pair.provider, "model": pair.model }),
                );
                return self.global(sel);
            }
            // the vote runs each draft's query, so a preview that runs nothing asks once
            let (value, level, pair) = if attempt == 1 && self.o.drafts > 1 && self.o.run {
                let (value, level, pair, result) =
                    self.vote_drafts((value, level, pair, None), &prompt, &schema);
                ran = result;
                (value, level, pair)
            } else {
                (value, level, pair)
            };
            if self.level.is_none() {
                self.level = Some(level);
                if level == Level::Text {
                    self.notes.push(format!(
                        "{} answered in plain text, so the answer has no assumptions and cannot ask for a clarification",
                        pair.label()
                    ));
                }
            }
            let mut d = normalize(&value);
            d.query = self.declare_prefixes(&d.query);
            self.answered_by = Some((role, pair.clone()));
            let mut record = json!({
                "attempt": attempt,
                "role": role.as_str(),
                "provider": pair.provider,
                "model": pair.model,
                "level": level.as_str(),
                "query": d.query,
                "explanation": d.explanation,
                "assumptions": d.assumptions,
                "graph": d.graph.clone().unwrap_or(Value::Null),
            });
            self.emit("draft", &record);
            if let Some((question, choices)) = &d.clarify
                && attempt == 1
                && self.o.clarification.is_none()
                && level != Level::Text
            {
                let c = json!({
                    "id": "draft",
                    "question": question,
                    "choices": choices.iter().map(|c| json!({"label": c, "value": c})).collect::<Vec<_>>(),
                    "query": d.query,
                });
                self.attempts.push(record);
                self.emit("clarify", &c);
                return json!({ "outcome": "clarify", "clarify": c });
            }
            if d.query.is_empty() {
                self.attempts.push(record);
                if !explored && self.room() {
                    explored = true;
                    let found = self.explore(ground, &entity_terms);
                    if !found.is_empty() {
                        self.emit(
                            "diagnosis",
                            &json!({ "attempt": attempt, "kind": "explore", "text": found }),
                        );
                        repair = Some(repair::explore_text(None, &found));
                        continue;
                    }
                }
                return json!({
                    "outcome": "unanswerable",
                    "result": { "explanation": d.explanation, "assumptions": d.assumptions },
                });
            }
            let parsed = self.parse(&d.query);
            if let Some(p) = &parsed {
                let c = complexity::of(p);
                // §5.5: a complex first draft is repaired by the stronger pair
                if attempt == 1 && c.score() >= self.o.complexity_threshold {
                    self.escalate(Role::Repair, "complexity");
                }
                self.complexity = Some(c);
            }
            // step 4: the check
            let check = self
                .tool(
                    "check_query",
                    json!({"query": d.query, "explain": true, "terms": true}),
                    true,
                )
                .unwrap_or_else(|e| {
                    json!({"ok": false, "issues": [{"code": e.code, "severity": "error", "message": e.message}]})
                });
            self.emit("check", &check);
            record["check"] = json!({ "ok": check["ok"], "issues": check["issues"] });
            entity_terms = check["terms"]
                .as_array()
                .into_iter()
                .flatten()
                .filter(|t| t["kind"] == "entity")
                .filter_map(|t| t["iri"].as_str().map(str::to_string))
                .collect();
            if check["ok"] != Value::Bool(true) {
                let diagnosis = issues_text(&check["issues"]);
                self.emit(
                    "diagnosis",
                    &json!({ "attempt": attempt, "kind": "check", "text": diagnosis }),
                );
                record["diagnosis"] = diagnosis.clone().into();
                self.attempts.push(record);
                if repairs < MAX_REPAIRS {
                    self.failed_again(attempt, "check-failed");
                    repairs += 1;
                    repair = Some(repair_text(&d.query, &diagnosis));
                    continue;
                }
                return json!({
                    "outcome": "failed",
                    "result": { "query": d.query, "explanation": d.explanation, "issues": check["issues"], "attempt": attempt },
                });
            }
            if has_service(parsed.as_ref()) {
                self.notes
                    .push("the query calls SERVICE, so it was not run automatically".into());
                self.attempts.push(record);
                return json!({
                    "outcome": "not-run",
                    "result": result(&d, &d.query, &check, None, false, attempt),
                });
            }
            let (run_query, limit_added) = self.with_limit(&d.query, parsed.as_ref());
            if !self.o.run {
                self.attempts.push(record);
                let r = result(&d, &run_query, &check, None, limit_added, attempt);
                self.emit("result", &r);
                return json!({ "outcome": "checked", "result": r });
            }
            if self.cancelled() {
                return self.error("cancelled", "the ask was cancelled");
            }
            // step 5: the run, unless the vote of the drafts ran this query already
            let outcome = match ran.take() {
                Some((q, r)) if q == run_query => Ok(r),
                _ => self.run_query(&run_query),
            };
            let (doc, full) = match outcome {
                Err(e) => {
                    let ev = json!({"attempt": attempt, "error": {"code": e.code, "message": e.message}});
                    self.emit("run", &ev);
                    let diagnosis = format!(
                        "The query stopped with {}: {}.{} The check estimated {} rows. Add selective patterns or a LIMIT.",
                        e.code,
                        e.message,
                        e.hint
                            .as_ref()
                            .map(|h| format!(" {h}."))
                            .unwrap_or_default(),
                        check["estimatedRows"]
                    );
                    self.emit(
                        "diagnosis",
                        &json!({ "attempt": attempt, "kind": "run", "text": diagnosis }),
                    );
                    record["run"] = ev["error"].clone();
                    record["diagnosis"] = diagnosis.clone().into();
                    self.attempts.push(record);
                    if repairs < MAX_REPAIRS && e.code.as_ref() != "cancelled" {
                        self.failed_again(attempt, "check-failed");
                        repairs += 1;
                        repair = Some(repair_text(&run_query, &diagnosis));
                        continue;
                    }
                    return json!({
                        "outcome": "failed",
                        "result": result(&d, &run_query, &check, None, limit_added, attempt),
                        "error": ev["error"],
                    });
                }
                Ok(x) => x,
            };
            let rows = doc["total"]
                .as_u64()
                .or_else(|| doc["returned"].as_u64())
                .unwrap_or(0);
            let truncated = !doc["truncated"].is_null();
            let is_ask = doc["queryType"] == "ASK";
            self.emit(
                "run",
                &json!({"attempt": attempt, "commit": doc["commit"], "rows": rows, "truncated": truncated, "elapsedMs": doc["elapsedMs"]}),
            );
            record["run"] = json!({ "rows": rows, "truncated": truncated });
            if rows == 0 && !is_ask {
                // §4.2: diagnose the empty result
                let why = self
                    .tool("why_empty", json!({ "query": run_query }), true)
                    .unwrap_or(Value::Null);
                let verdict = why["verdict"].as_str().unwrap_or("unknown").to_string();
                let diagnosis = why_text(&why);
                self.emit("diagnosis", &json!({ "attempt": attempt, "kind": "empty", "verdict": verdict, "text": diagnosis, "whyEmpty": why }));
                record["diagnosis"] = diagnosis.clone().into();
                record["verdict"] = verdict.clone().into();
                self.attempts.push(record);
                // `data`: the data holds no match, and repair stops at once
                if repairs < MAX_REPAIRS && verdict != "data" {
                    if verdict == "query" {
                        self.failed_again(attempt, "empty-query");
                    }
                    repairs += 1;
                    repair = Some(repair_text(&run_query, &diagnosis));
                    continue;
                }
                if !explored && self.room() {
                    explored = true;
                    let found = self.explore(ground, &entity_terms);
                    if !found.is_empty() {
                        self.emit(
                            "diagnosis",
                            &json!({ "attempt": attempt, "kind": "explore", "text": found }),
                        );
                        repair = Some(repair::explore_text(Some(&run_query), &found));
                        continue;
                    }
                }
                let mut r = result(&d, &run_query, &check, Some(&doc), limit_added, attempt);
                if let Some(f) = full {
                    r["results"] = f;
                }
                r["empty"] = true.into();
                r["verdict"] = verdict.into();
                r["diagnosis"] = diagnosis.into();
                self.emit("result", &r);
                return json!({ "outcome": "empty", "result": r });
            }
            let mut r = result(&d, &run_query, &check, Some(&doc), limit_added, attempt);
            if let Some(f) = full {
                r["results"] = f;
            }
            if !shaped {
                shaped = true;
                if let Some(hint) = repair::shape_hint(&self.o.question, &run_query, &doc)
                    && self.room()
                {
                    self.emit(
                        "diagnosis",
                        &json!({ "attempt": attempt, "kind": "shape", "text": hint }),
                    );
                    record["diagnosis"] = hint.clone().into();
                    self.attempts.push(record);
                    // the rows go to the repair pair only when it may receive rows
                    let rows_seen = self.o.report_context.then(|| rows_text(&doc, 10).0);
                    repair = Some(repair::shape_text(&run_query, &hint, rows_seen.as_deref()));
                    self.fallback = Some(Answered {
                        result: r,
                        query: run_query,
                        doc,
                        rows,
                        by: self.answered_by.clone(),
                    });
                    // the shape's repair is the last: a failure keeps the first answer
                    repairs = MAX_REPAIRS;
                    explored = true;
                    continue;
                }
            }
            self.attempts.push(record);
            self.fallback = None;
            self.held.clear();
            return self.answer(
                Answered {
                    result: r,
                    query: run_query,
                    doc,
                    rows,
                    by: None,
                },
                context_tokens,
            );
        }
    }

    /// A query given by the caller: check, run and summarize it, with no draft (the
    /// UI's **Summarize again** after an edit).
    fn given(&mut self, query: &str) -> Value {
        let d = Draft {
            query: query.to_string(),
            explanation: String::new(),
            assumptions: Vec::new(),
            clarify: None,
            graph: None,
        };
        let check = self
            .tool(
                "check_query",
                json!({"query": query, "explain": true, "terms": true}),
                true,
            )
            .unwrap_or_else(|e| {
                json!({"ok": false, "issues": [{"code": e.code, "severity": "error", "message": e.message}]})
            });
        self.emit("check", &check);
        if check["ok"] != Value::Bool(true) {
            return json!({
                "outcome": "failed",
                "result": { "query": query, "issues": check["issues"], "attempt": 0 },
            });
        }
        let parsed = self.parse(query);
        if let Some(p) = &parsed {
            self.complexity = Some(complexity::of(p));
        }
        if has_service(parsed.as_ref()) {
            self.notes
                .push("the query calls SERVICE, so it was not run automatically".into());
            return json!({
                "outcome": "not-run",
                "result": result(&d, query, &check, None, false, 0),
            });
        }
        if !self.o.run {
            let r = result(&d, query, &check, None, false, 0);
            self.emit("result", &r);
            return json!({ "outcome": "checked", "result": r });
        }
        let (doc, full) = match self.run_query(query) {
            Err(e) => {
                let ev = json!({"attempt": 0, "error": {"code": e.code, "message": e.message}});
                self.emit("run", &ev);
                return json!({
                    "outcome": "failed",
                    "result": result(&d, query, &check, None, false, 0),
                    "error": ev["error"],
                });
            }
            Ok(x) => x,
        };
        let rows = doc["total"]
            .as_u64()
            .or_else(|| doc["returned"].as_u64())
            .unwrap_or(0);
        let truncated = !doc["truncated"].is_null();
        self.emit(
            "run",
            &json!({"attempt": 0, "commit": doc["commit"], "rows": rows, "truncated": truncated, "elapsedMs": doc["elapsedMs"]}),
        );
        let mut r = result(&d, query, &check, Some(&doc), false, 0);
        if let Some(f) = full {
            r["results"] = f;
        }
        self.emit("result", &r);
        if rows == 0 && doc["queryType"] != "ASK" {
            r["empty"] = true.into();
            return json!({ "outcome": "empty", "result": r });
        }
        let mut out = json!({ "outcome": "answered", "result": r });
        let context_tokens = self
            .pair(Role::Summarize)
            .and_then(|p| self.models.resolved(&p))
            .map_or(crate::models::DEFAULT_CONTEXT_TOKENS, |r| r.context_tokens);
        if let Some(s) = self.summarize(query, &doc, rows, context_tokens) {
            out["summary"] = s;
        }
        out
    }

    /// A repaired draft that fails again moves the repair role on (§5.5). The callers
    /// call it only when another repair follows.
    fn failed_again(&mut self, attempt: u32, signal: &'static str) {
        if attempt > 1 {
            self.escalate(Role::Repair, signal);
        }
    }

    /// `query` with `LIMIT 1000` when it is a `SELECT` without a limit (§4.3).
    fn with_limit(&self, query: &str, parsed: Option<&spargebra::Query>) -> (String, bool) {
        let Some(spargebra::Query::Select { pattern, .. }) = parsed else {
            return (query.to_string(), false);
        };
        if matches!(
            pattern,
            spargebra::algebra::GraphPattern::Slice {
                length: Some(_),
                ..
            }
        ) {
            return (query.to_string(), false);
        }
        let limited = format!("{}\nLIMIT {ADDED_LIMIT}", query.trim_end());
        if self.parse(&limited).is_some() {
            (limited, true)
        } else {
            (query.to_string(), false)
        }
    }

    /// Step 7, when the role, the context window, the budget and the deadline allow it.
    fn summarize(
        &mut self,
        query: &str,
        doc: &Value,
        rows: u64,
        context_tokens: u64,
    ) -> Option<Value> {
        if !self.o.summary {
            return None;
        }
        if context_tokens < TINY_CONTEXT {
            self.notes.push(
                "the model's context window is small, so the rows are shown without a summary"
                    .into(),
            );
            return None;
        }
        self.pair(Role::Summarize)?;
        if self.deadline.saturating_duration_since(Instant::now()) < SUMMARY_MIN_LEFT {
            self.notes
                .push("too little time was left for a summary".into());
            return None;
        }
        if self.tokens() >= self.o.max_tokens {
            self.notes
                .push("the token budget was used before the summary".into());
            return None;
        }
        let (text, sent) = rows_text(doc, self.o.rows_for_summary);
        let total = if doc["queryType"] == "ASK" {
            "The query is an ASK query.".to_string()
        } else if sent < rows {
            format!("The result has {rows} rows. These are the first {sent}, numbered from 1.")
        } else {
            format!("The result has {rows} rows, numbered from 1.")
        };
        let mut user = prompts::summary_user(&self.o.question, query, &text, &total);
        // the claims of the linked entities, read for the caller now (C25)
        let claims = if self.o.review_policy.is_some() || self.o.matching_policy.is_some() {
            let seeds = self.claim_seeds.clone();
            match self.claims_recall(&seeds) {
                Ok(c) => Some(c),
                Err(e) => {
                    self.notes
                        .push(format!("the claims were not read: {}", e.message));
                    self.withheld = true;
                    return None;
                }
            }
        } else {
            None
        };
        let listed: Vec<String> = claims
            .as_ref()
            .map(|c| prompts::claim_ids(&c["claims"]))
            .unwrap_or_default();
        if let Some(c) = &claims
            && !c["claims"].is_null()
        {
            user.push_str(&prompts::claims_context(&c["claims"]));
        }
        let schema = if claims.is_some() {
            prompts::summary_with_claims()
        } else {
            prompts::summary()
        };
        match self.call(
            Role::Summarize,
            &Prompt::plain(prompts::SUMMARY_SYSTEM, &user),
            &schema,
        ) {
            Err((_, message)) => {
                self.notes.push(format!("the summary failed: {message}"));
                None
            }
            Ok((value, _, pair)) => {
                let raw = value["text"].as_str().unwrap_or("");
                let text = prompts::drop_markers(raw, sent.max(1));
                let mut citations: Vec<u64> = prompts::markers(&text);
                for c in value["citations"].as_array().into_iter().flatten() {
                    if let Some(n) = c.as_u64()
                        && (1..=sent.max(1)).contains(&n)
                        && !citations.contains(&n)
                        && text.contains(&format!("[{n}]"))
                    {
                        citations.push(n);
                    }
                }
                let mut s = json!({
                    "text": text,
                    "citations": citations,
                    "rowsSent": sent,
                    "provider": pair.provider,
                    "model": pair.model,
                });
                if let Some(c) = &claims {
                    let mut cited: Vec<String> = prompts::claim_markers(&text)
                        .into_iter()
                        .filter(|id| listed.contains(id))
                        .collect();
                    for id in value["claimCitations"].as_array().into_iter().flatten() {
                        if let Some(id) = id.as_str()
                            && listed.iter().any(|l| l == id)
                            && !cited.iter().any(|x| x == id)
                            && text.contains(&format!("[{id}]"))
                        {
                            cited.push(id.to_string());
                        }
                    }
                    s["claimCitations"] = json!(cited);
                    // the same view, read again with the caller's grants of now: any
                    // difference withholds the answer before it is published
                    let seeds = self.claim_seeds.clone();
                    match self.claims_recall(&seeds) {
                        Ok(again) if again == *c => {}
                        _ => {
                            self.withheld = true;
                            return None;
                        }
                    }
                    s["claims"] = c["claims"].clone();
                    self.claims = Some(c.clone());
                }
                if citations.is_empty() {
                    s["uncited"] = true.into();
                    self.notes.push("the summary cites no row".into());
                }
                self.emit("summary", &s);
                if claims.is_some()
                    && let Some(m) = s.as_object_mut()
                {
                    m.remove("claims");
                }
                Some(s)
            }
        }
    }

    /// `recall` of `seeds` at hop 0 with the ask's policies, in JSON. A refused
    /// policy keeps its code. Without seeds, the policies are still checked and the
    /// claims are empty.
    fn claims_recall(&self, seeds: &[String]) -> Result<Value, ToolError> {
        let seeds: Vec<String> = if seeds.is_empty() {
            vec![PREFLIGHT_SEED.to_string()]
        } else {
            seeds.to_vec()
        };
        let mut args = json!({ "seeds": seeds, "hops": 0, "maxTriples": 50, "format": "json" });
        if let Some(p) = &self.o.review_policy {
            args["reviewPolicy"] = p.clone().into();
        }
        if let Some(p) = &self.o.matching_policy {
            args["matchingPolicy"] = p.clone().into();
        }
        let mut v = self.tool("recall", args, true)?;
        // the parts of the result that the summary's context and answer depend on
        let mut kept = json!({ "commit": v["commit"].take() });
        for k in ["claims", "resolution"] {
            if !v[k].is_null() {
                kept[k] = v[k].take();
            }
        }
        Ok(kept)
    }

    fn usage(&self, started: Instant) -> Value {
        let (i, o) = self.steps.iter().fold((0, 0), |(i, o), s| {
            (i + s.input_tokens, o + s.output_tokens)
        });
        let costs: Vec<f64> = self.steps.iter().filter_map(|s| s.estimated_cost).collect();
        let steps: Vec<Value> = self
            .steps
            .iter()
            .zip(&self.signals)
            .map(|(s, sig)| {
                let mut j = s.json();
                if let Some(sig) = sig {
                    j["signal"] = (*sig).into();
                }
                j
            })
            .collect();
        let draft_pos = self.pos.get(&Role::Draft).copied().unwrap_or(0);
        let mut u = json!({
            "inputTokens": i,
            "outputTokens": o,
            "modelCalls": self.steps.len(),
            "failedCalls": self.steps.iter().filter(|s| s.outcome != "ok").count(),
            "elapsedMs": started.elapsed().as_millis() as u64,
            "modelMs": self.steps.iter().map(|s| s.latency.as_millis() as u64).sum::<u64>(),
            "steps": steps,
            "escalations": self.escalations,
            "grounding": self.trimmed,
            "draftPair": draft_pos,
            "tryHarder": self.o.query.is_none() && draft_pos + 1 < self.list(Role::Draft).len(),
        });
        crate::models::steps_json(&mut u, &self.steps);
        if let Some(id) = &self.o.id {
            u["askId"] = id.clone().into();
        }
        if !costs.is_empty() {
            u["estimatedCost"] = json!(costs.iter().sum::<f64>());
        }
        if let Some(c) = &self.complexity {
            u["complexity"] = c.json();
        }
        if let Some(l) = self.level {
            u["level"] = l.as_str().into();
        }
        if let Some((role, p)) = &self.answered_by {
            u["answeredBy"] =
                json!({ "role": role.as_str(), "provider": p.provider, "model": p.model });
            if let Some(r) = self.models.resolved(p)
                && r.context_tokens < SMALL_CONTEXT
            {
                u["smallModel"] = true.into();
            }
        }
        u
    }
}

/// The result object of §5.2.
fn result(
    d: &Draft,
    query: &str,
    check: &Value,
    doc: Option<&Value>,
    limit_added: bool,
    attempt: u32,
) -> Value {
    let mut r = json!({
        "query": query,
        "explanation": d.explanation,
        "assumptions": d.assumptions,
        "terms": check.get("terms").cloned().unwrap_or(json!([])),
        "issues": check["issues"],
        "graph": d.graph.clone().unwrap_or(Value::Null),
        "limitAdded": limit_added,
        "attempt": attempt,
    });
    if let Some(doc) = doc {
        r["commit"] = doc["commit"].clone();
        r["results"] = doc.clone();
        r["truncated"] = (!doc["truncated"].is_null()).into();
    } else if let Some(c) = check.get("commit") {
        r["commit"] = c.clone();
    }
    r
}

/// A draft with its lengths enforced and its empty members removed.
fn normalize(v: &Value) -> Draft {
    let s = |v: &Value| v.as_str().unwrap_or("").trim().to_string();
    let assumptions = v["assumptions"]
        .as_array()
        .into_iter()
        .flatten()
        .map(s)
        .filter(|a| !a.is_empty())
        .take(5)
        .map(|a| prompts::cut(&a, 400))
        .collect();
    let question = s(&v["clarify"]["question"]);
    let choices: Vec<String> = v["clarify"]["choices"]
        .as_array()
        .into_iter()
        .flatten()
        .map(s)
        .filter(|c| !c.is_empty())
        .collect();
    let clarify = (!question.is_empty() && (2..=4).contains(&choices.len()))
        .then(|| (prompts::cut(&question, 400), choices));
    let var = |k: &str| s(&v["graph"][k]).trim_start_matches(['?', '$']).to_string();
    let (gs, gp, go) = (var("subject"), var("predicate"), var("object"));
    let graph = (!gs.is_empty() && !go.is_empty()).then(|| {
        let mut g = json!({ "subject": gs, "object": go });
        if !gp.is_empty() {
            g["predicate"] = gp.into();
        }
        g
    });
    Draft {
        query: s(&v["query"]),
        explanation: prompts::cut(&s(&v["explanation"]), 400),
        assumptions,
        clarify,
        graph,
    }
}

/// The repair section of a draft prompt.
fn repair_text(query: &str, diagnosis: &str) -> String {
    format!(
        "Your previous query was:\n```sparql\n{}\n```\nIt failed. The diagnosis:\n{}\nWrite a corrected query.",
        query.trim(),
        prompts::data_block(diagnosis)
    )
}

/// `check_query` issues as diagnosis lines, with their suggestions.
fn issues_text(issues: &Value) -> String {
    let mut out = String::new();
    for i in issues.as_array().into_iter().flatten() {
        out.push_str(&format!(
            "- {} {}: {}",
            i["severity"].as_str().unwrap_or("error"),
            i["code"].as_str().unwrap_or(""),
            i["message"].as_str().unwrap_or("")
        ));
        if let (Some(l), Some(c)) = (i["line"].as_u64(), i["column"].as_u64()) {
            out.push_str(&format!(" (line {l}, column {c})"));
        }
        let sugg: Vec<String> = i["suggestions"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|s| {
                format!(
                    "{} {} ({} triples)",
                    s["term"].as_str().unwrap_or(""),
                    s.get("label").map(Value::to_string).unwrap_or_default(),
                    s["count"]
                )
            })
            .collect();
        if !sugg.is_empty() {
            out.push_str(&format!(". Suggested: {}", sugg.join("; ")));
        }
        out.push('\n');
    }
    out
}

/// The `why_empty` result as diagnosis text.
fn why_text(w: &Value) -> String {
    if w.is_null() {
        return "The query returned no rows, and the diagnosis did not finish.".into();
    }
    let mut out = format!(
        "The query returned no rows. {}\n",
        w["message"].as_str().unwrap_or("")
    );
    let f = &w["first"];
    if f.is_object() {
        out.push_str(&format!(
            "The first {} without solutions: {}\n",
            f["kind"].as_str().unwrap_or("pattern"),
            f["text"].as_str().unwrap_or("")
        ));
        for c in f["constants"].as_array().into_iter().flatten() {
            if c["occurs"] == false {
                out.push_str(&format!(
                    "The term {} does not occur in the data.\n",
                    c["term"].as_str().unwrap_or("")
                ));
            }
        }
        let issues = issues_text(&f["issues"]);
        if !issues.is_empty() {
            out.push_str("Issues:\n");
            out.push_str(&issues);
        }
    }
    out
}

/// The rows of a result as numbered lines, at most `n`, and how many were sent.
fn rows_text(doc: &Value, n: usize) -> (String, u64) {
    if let Some(b) = doc["boolean"].as_bool() {
        return (format!("[1] {b}"), 1);
    }
    let vars: Vec<&str> = doc["vars"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect();
    let mut out = String::new();
    let mut sent = 0;
    for (i, row) in doc["rows"]
        .as_array()
        .into_iter()
        .flatten()
        .take(n)
        .enumerate()
    {
        let cells: Vec<String> = row
            .as_array()
            .into_iter()
            .flatten()
            .enumerate()
            .map(|(j, c)| {
                let v = vars.get(j).copied().unwrap_or("?");
                match c.as_str() {
                    Some(t) => format!("?{v} = {t}"),
                    None => format!("?{v} unbound"),
                }
            })
            .collect();
        out.push_str(&format!("[{}] {}\n", i + 1, cells.join(" ; ")));
        sent += 1;
    }
    (out, sent)
}

/// A label as a JSON string, or nothing.
fn label(v: &Value) -> String {
    v.get("label")
        .and_then(Value::as_str)
        .map(|l| Value::from(l).to_string())
        .unwrap_or_default()
}

/// Whether a query calls a remote `SERVICE`, read from its parsed algebra.
///
/// Every pattern is visited, including the patterns of `EXISTS` and `NOT EXISTS` in
/// filters, binds, orderings and aggregates, and every subquery. A path search runs
/// locally and does not count. A query that does not parse counts as calling one,
/// because nothing about it can be known. This only decides whether `sparkles ask`
/// runs a draft by itself. The query engine refuses SERVICE on its own unless the
/// server allows it.
fn has_service(q: Option<&spargebra::Query>) -> bool {
    use spargebra::Query;
    match q {
        None => true,
        Some(
            Query::Select { pattern, .. }
            | Query::Ask { pattern, .. }
            | Query::Construct { pattern, .. }
            | Query::Describe { pattern, .. },
        ) => pattern_calls_service(pattern),
    }
}

fn pattern_calls_service(p: &spargebra::algebra::GraphPattern) -> bool {
    use spargebra::algebra::GraphPattern as G;
    match p {
        G::Service { name, inner, .. } => {
            !sparkles::sparql::pathsearch::is_search(name) || pattern_calls_service(inner)
        }
        G::Bgp { .. } | G::Path { .. } | G::Values { .. } => false,
        G::Join { left, right }
        | G::Lateral { left, right }
        | G::Union { left, right }
        | G::Minus { left, right }
        | G::SemiJoin { left, right }
        | G::AntiJoin { left, right } => {
            pattern_calls_service(left) || pattern_calls_service(right)
        }
        G::LeftJoin {
            left,
            right,
            expression,
        } => {
            pattern_calls_service(left)
                || pattern_calls_service(right)
                || expression.as_ref().is_some_and(expression_calls_service)
        }
        G::Filter { expr, inner } => expression_calls_service(expr) || pattern_calls_service(inner),
        G::Extend {
            inner, expression, ..
        }
        | G::Assign {
            inner, expression, ..
        }
        | G::Unfold {
            inner, expression, ..
        } => expression_calls_service(expression) || pattern_calls_service(inner),
        G::OrderBy { inner, expression } => {
            pattern_calls_service(inner) || expression.iter().any(order_calls_service)
        }
        G::Group {
            inner, aggregates, ..
        } => {
            pattern_calls_service(inner)
                || aggregates.iter().any(|(_, a)| aggregate_calls_service(a))
        }
        G::Graph { inner, .. }
        | G::Project { inner, .. }
        | G::Distinct { inner }
        | G::Reduced { inner }
        | G::Slice { inner, .. } => pattern_calls_service(inner),
    }
}

fn order_calls_service(o: &spargebra::algebra::OrderExpression) -> bool {
    use spargebra::algebra::OrderExpression as O;
    match o {
        O::Asc(e) | O::Desc(e) => expression_calls_service(e),
    }
}

fn aggregate_calls_service(a: &spargebra::algebra::AggregateExpression) -> bool {
    use spargebra::algebra::AggregateExpression as A;
    match a {
        A::CountSolutions { .. } => false,
        A::FunctionCall { expr, .. } => expression_calls_service(expr),
        A::Fold {
            expr, value, order, ..
        } => {
            expression_calls_service(expr)
                || value.as_ref().is_some_and(expression_calls_service)
                || order.iter().any(order_calls_service)
        }
    }
}

fn expression_calls_service(e: &spargebra::algebra::Expression) -> bool {
    use spargebra::algebra::Expression as E;
    match e {
        E::NamedNode(_) | E::Literal(_) | E::Variable(_) | E::Bound(_) => false,
        E::Exists(p) => pattern_calls_service(p),
        E::Or(a, b)
        | E::And(a, b)
        | E::Equal(a, b)
        | E::SameTerm(a, b)
        | E::Greater(a, b)
        | E::GreaterOrEqual(a, b)
        | E::Less(a, b)
        | E::LessOrEqual(a, b)
        | E::Add(a, b)
        | E::Subtract(a, b)
        | E::Multiply(a, b)
        | E::Divide(a, b) => expression_calls_service(a) || expression_calls_service(b),
        E::UnaryPlus(a) | E::UnaryMinus(a) | E::Not(a) => expression_calls_service(a),
        E::In(a, list) => expression_calls_service(a) || list.iter().any(expression_calls_service),
        E::If(a, b, c) => {
            expression_calls_service(a)
                || expression_calls_service(b)
                || expression_calls_service(c)
        }
        E::Coalesce(list) | E::FunctionCall(_, list) => list.iter().any(expression_calls_service),
    }
}

/// The names a question mentions: quoted strings and runs of capitalized words, with
/// the question words that open a sentence left out.
fn mentions(q: &str) -> Vec<String> {
    const OPENERS: &[&str] = &[
        "who", "what", "which", "whose", "whom", "where", "when", "why", "how", "list", "show",
        "give", "find", "name", "count", "does", "do", "did", "is", "are", "was", "were", "in",
        "the", "tell", "return", "get", "please", "i", "has", "have", "can", "could", "for", "of",
        "a", "an", "and", "or", "all", "any", "each", "every", "many", "much", "me", "my", "on",
        "at", "by", "to", "from", "with", "between", "among", "average", "total", "sum", "number",
        "top", "most", "least", "sparql",
    ];
    const LINKS: &[&str] = &[
        "of", "the", "van", "von", "de", "da", "du", "del", "la", "&", "and",
    ];
    let mut out: Vec<String> = Vec::new();
    // quoted strings
    for (open, close) in [('"', '"'), ('“', '”'), ('\'', '\''), ('‘', '’')] {
        let mut rest = q;
        while let Some(i) = rest.find(open) {
            let after = &rest[i + open.len_utf8()..];
            let Some(j) = after.find(close) else { break };
            let m = after[..j].trim();
            // an apostrophe inside a word is not a quote
            let word_before = rest[..i].chars().last().is_some_and(char::is_alphanumeric);
            if !m.is_empty() && m.len() <= 200 && !(open == '\'' && word_before) {
                out.push(m.to_string());
            }
            rest = &after[j + close.len_utf8()..];
        }
    }
    let tokens: Vec<&str> = q
        .split(|c: char| {
            c.is_whitespace()
                || matches!(c, ',' | '?' | '!' | ';' | ':' | '(' | ')' | '"' | '“' | '”')
        })
        .filter(|t| !t.is_empty())
        .collect();
    let clean = |t: &str| -> String {
        let t = t.trim_end_matches(['.', '\'', '’']);
        let t = t
            .strip_suffix("'s")
            .or_else(|| t.strip_suffix("’s"))
            .unwrap_or(t);
        t.to_string()
    };
    let capital = |t: &str| t.chars().next().is_some_and(char::is_uppercase);
    let mut i = 0;
    while i < tokens.len() {
        let t = clean(tokens[i]);
        // A capitalized article inside the question starts a title, as in "Which
        // museum exhibits The Scream?", so the mention keeps it.
        let article = i > 0
            && t == "The"
            && tokens.get(i + 1).is_some_and(|n| {
                let n = clean(n);
                capital(&n) && !OPENERS.contains(&n.to_lowercase().as_str())
            });
        if !article && (!capital(&t) || OPENERS.contains(&t.to_lowercase().as_str())) {
            i += 1;
            continue;
        }
        let mut run = vec![t];
        let mut j = i + 1;
        let ends = |t: &str| t.ends_with("'s") || t.ends_with("’s") || t.ends_with('.');
        while j < tokens.len() && !ends(tokens[j - 1]) {
            let n = clean(tokens[j]);
            let lower = n.to_lowercase();
            // a number continues a name, as in "Apollo 11"
            let name = (capital(&n) && !OPENERS.contains(&lower.as_str()))
                || (!n.is_empty() && n.chars().all(|c| c.is_ascii_digit()));
            // a linking word joins two capitalized words, as in "Guido van Rossum"
            let link = LINKS.contains(&lower.as_str())
                && j + 1 < tokens.len()
                && capital(&clean(tokens[j + 1]));
            if !(name || link) {
                break;
            }
            run.push(n);
            j += 1;
        }
        let m = run.join(" ");
        if !out.contains(&m) {
            out.push(m);
        }
        i = j;
    }
    out.truncate(20);
    out
}
