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

mod cli;
pub mod complexity;
pub mod http;
#[cfg(test)]
mod http_tests;
mod prompts;
#[cfg(test)]
mod routing_tests;
#[cfg(test)]
mod tests;

pub use cli::{AskArgs, run_cli};

use crate::auth::Principal;
use crate::mcp::errors::ToolError;
use crate::mcp::{Call, McpServer, Outcome};
use crate::models::{Level, Models, Pair, Role, StepError, StepRecord};
use serde_json::{Map, Value, json};
use std::collections::{BTreeMap, BTreeSet};
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
/// replaces the server's list, an empty list turns the role off, and `repair` without a
/// list uses `draft`'s.
pub fn resolve_lists(models: &Models, overrides: &RoleLists) -> RoleLists {
    let mut out = RoleLists::new();
    for r in Role::ALL {
        if let Some(l) = overrides.get(&r).or_else(|| models.config.roles.get(&r)) {
            out.insert(r, l.clone());
        }
    }
    if !out.contains_key(&Role::Repair)
        && let Some(d) = out.get(&Role::Draft).cloned()
    {
        out.insert(Role::Repair, d);
    }
    out
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
    };
    let mut out = a.run();
    out["dataset"] = o.dataset.clone().into();
    out["question"] = o.question.clone().into();
    out["attempts"] = Value::Array(std::mem::take(&mut a.attempts));
    out["notes"] = json!(a.notes);
    let usage = a.usage(started);
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
}

/// The grounding of §4.1 step 1.
struct Ground {
    context: String,
    /// a mention that links `ambiguous`, with its candidates
    ambiguous: Option<(String, Vec<Value>)>,
    /// the best stored example's query, when its score reaches `example_score`
    best_example: Option<String>,
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

    fn tokens(&self) -> u64 {
        self.steps
            .iter()
            .map(|s| s.input_tokens + s.output_tokens)
            .sum()
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
        system: &str,
        user: &str,
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
                .call(Some(role), &pair, system, user, out, self.deadline)
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
        };
        Ok((m, call))
    }

    /// Call a tool as the principal, on this thread, under the ask's deadline.
    fn tool(&self, name: &str, args: Value, timed: bool) -> Result<Value, ToolError> {
        let (m, call) = self.tool_call(name, args, timed)?;
        Ok(match self.server.run_now(name, m, &call)? {
            Outcome::Structured(v) => v,
            Outcome::Text(t) => serde_json::from_str(&t).unwrap_or(Value::String(t)),
        })
    }

    /// Run a query with `sparql_query`: its compact result, and with `full_results` the
    /// same result in the form of `/{ds}/sparql`.
    fn run_query(&self, query: &str) -> Result<(Value, Option<Value>), ToolError> {
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
            return self.error(e.code, e.message);
        }
        if let Some(q) = self.o.query.clone() {
            return self.given(&q);
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
        let ground = self.ground(context_tokens);
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
        let small = context_tokens < SMALL_CONTEXT;
        let (max_classes, max_predicates, examples_k, candidates_k) = if small {
            (30, 60, 2, 3)
        } else {
            (100, 150, 5, 5)
        };
        let mut ctx = String::new();
        let mut ground_event = json!({});
        let q_words = words(&self.o.question);
        let schema = self
            .tool(
                "describe_schema",
                json!({"section": "summary", "limit": 200}),
                false,
            )
            .unwrap_or(Value::Null);
        if let Some(p) = schema["prefixes"].as_object()
            && !p.is_empty()
        {
            ctx.push_str("Prefixes:\n");
            for (k, v) in p {
                ctx.push_str(&format!("{k}: <{}>\n", v.as_str().unwrap_or("")));
            }
        }
        let ranked = |list: &Value, max: usize| -> Vec<Value> {
            let mut v: Vec<(usize, Value)> = list
                .as_array()
                .into_iter()
                .flatten()
                .map(|e| (relevance(e, &q_words), e.clone()))
                .collect();
            if small {
                // stable: the size order breaks ties
                v.sort_by_key(|a| std::cmp::Reverse(a.0));
            }
            v.into_iter().take(max).map(|(_, e)| e).collect()
        };
        let classes = ranked(&schema["classes"], max_classes);
        let predicates = ranked(&schema["predicates"], max_predicates);
        if !classes.is_empty() {
            ctx.push_str("\nClasses (IRI, label, instances):\n");
            for c in &classes {
                ctx.push_str(&format!(
                    "{} {} {}\n",
                    c["iri"].as_str().unwrap_or(""),
                    label(c),
                    c["instances"]
                ));
            }
        }
        if !predicates.is_empty() {
            ctx.push_str("\nPredicates (IRI, label, triples, kinds or classes of the values):\n");
            for p in &predicates {
                let objects: Vec<&str> = p["objects"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                    .collect();
                ctx.push_str(&format!(
                    "{} {} {} {}\n",
                    p["iri"].as_str().unwrap_or(""),
                    label(p),
                    p["triples"],
                    objects.join(" ")
                ));
            }
        }
        ground_event["classes"] = classes.iter().map(|c| c["iri"].clone()).collect();
        ground_event["predicates"] = predicates.iter().map(|c| c["iri"].clone()).collect();
        // stored examples
        let examples = self
            .tool(
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
        let mentions = mentions(&self.o.question);
        let mut entities = Vec::new();
        if !mentions.is_empty() {
            let list: Vec<Value> = mentions.iter().map(|m| json!({ "text": m })).collect();
            if let Ok(v) = self.tool(
                "link_entities",
                json!({"mentions": list, "k": candidates_k}),
                true,
            ) {
                let ms = v["mentions"].as_array().cloned().unwrap_or_default();
                let mut lines = String::new();
                for m in &ms {
                    let text = m["text"].as_str().unwrap_or("");
                    let verdict = m["verdict"].as_str().unwrap_or("none");
                    let cands = m["candidates"].as_array().cloned().unwrap_or_default();
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
                        let types: Vec<&str> = c["types"]
                            .as_array()
                            .into_iter()
                            .flatten()
                            .filter_map(Value::as_str)
                            .collect();
                        lines.push_str(&format!(
                            "  {} {} types: {}\n",
                            c["iri"].as_str().unwrap_or(""),
                            label(c),
                            types.join(" ")
                        ));
                    }
                }
                if !lines.is_empty() {
                    ctx.push_str("\nEntities that the question may name:\n");
                    ctx.push_str(&lines);
                }
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
            context: ctx,
            ambiguous,
            best_example,
        }
    }

    /// Steps 3 to 7: draft, check, run, repair at most twice, then summarize, moving
    /// along the role lists on the signals of §5.5.
    fn draft_loop(&mut self, ground: &Ground, notes: &[String], context_tokens: u64) -> Value {
        let mut repair: Option<String> = None;
        let mut repairs = 0;
        let mut attempt = 0;
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
            let user =
                prompts::draft_user(&self.o.question, &ground.context, notes, repair.as_deref());
            let (value, level, pair) =
                match self.call(role, prompts::SYSTEM, &user, &prompts::draft()) {
                    Ok(x) => x,
                    Err((code, message)) => return self.error(code, message),
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
            let d = normalize(&value);
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
            if has_service(&d.query) {
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
            // step 5: the run
            let (doc, full) = match self.run_query(&run_query) {
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
                    if repairs < MAX_REPAIRS && e.code != "cancelled" {
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
            self.attempts.push(record);
            let mut r = result(&d, &run_query, &check, Some(&doc), limit_added, attempt);
            if let Some(f) = full {
                r["results"] = f;
            }
            self.emit("result", &r);
            let mut out = json!({ "outcome": "answered", "result": r });
            if let Some(s) = self.summarize(&run_query, &doc, rows, context_tokens) {
                out["summary"] = s;
            }
            return out;
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
        if let Some(p) = self.parse(query) {
            self.complexity = Some(complexity::of(&p));
        }
        if has_service(query) {
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
        let user = prompts::summary_user(&self.o.question, query, &text, &total);
        match self.call(
            Role::Summarize,
            prompts::SUMMARY_SYSTEM,
            &user,
            &prompts::summary(),
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
                if citations.is_empty() {
                    s["uncited"] = true.into();
                    self.notes.push("the summary cites no row".into());
                }
                self.emit("summary", &s);
                Some(s)
            }
        }
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

/// Whether the query text calls `SERVICE`, outside strings and IRIs.
fn has_service(q: &str) -> bool {
    let mut in_str: Option<char> = None;
    let mut in_iri = false;
    let mut word = String::new();
    for c in q.chars() {
        match (in_str, in_iri) {
            (Some(d), _) => {
                if c == d {
                    in_str = None;
                }
                continue;
            }
            (None, true) => {
                if c == '>' {
                    in_iri = false;
                }
                continue;
            }
            _ => {}
        }
        if c.is_ascii_alphabetic() {
            word.push(c);
            continue;
        }
        if word.eq_ignore_ascii_case("service") {
            return true;
        }
        word.clear();
        match c {
            '"' | '\'' => in_str = Some(c),
            '<' => in_iri = true,
            '#' => {}
            _ => {}
        }
    }
    word.eq_ignore_ascii_case("service")
}

/// The lowercase words of `s` of three letters or more, split at camel case, with a
/// plural `s` removed.
fn words(s: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    let mut cur = String::new();
    let mut prev_lower = false;
    let mut push = |w: &mut String| {
        if w.chars().count() >= 3 {
            let mut x = w.to_lowercase();
            if x.len() > 3 && x.ends_with('s') && !x.ends_with("ss") {
                x.pop();
            }
            out.insert(x);
        }
        w.clear();
    };
    for c in s.chars() {
        if c.is_alphanumeric() {
            if c.is_uppercase() && prev_lower {
                push(&mut cur);
            }
            prev_lower = c.is_lowercase();
            cur.push(c);
        } else {
            push(&mut cur);
            prev_lower = false;
        }
    }
    push(&mut cur);
    out
}

/// How many words of the question a schema entry's label or local name shares.
fn relevance(e: &Value, q: &BTreeSet<String>) -> usize {
    let iri = e["iri"].as_str().unwrap_or("");
    let local = iri.rsplit(['#', '/', ':']).next().unwrap_or(iri);
    let mut w = words(local);
    w.extend(words(e["label"].as_str().unwrap_or("")));
    w.intersection(q).count()
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
        if !capital(&t) || OPENERS.contains(&t.to_lowercase().as_str()) {
            i += 1;
            continue;
        }
        let mut run = vec![t];
        let mut j = i + 1;
        let ends = |t: &str| t.ends_with("'s") || t.ends_with("’s") || t.ends_with('.');
        while j < tokens.len() && !ends(tokens[j - 1]) {
            let n = clean(tokens[j]);
            let lower = n.to_lowercase();
            let name = capital(&n) && !OPENERS.contains(&lower.as_str());
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
