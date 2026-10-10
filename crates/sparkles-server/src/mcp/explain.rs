//! `explain_query` (C11, extended by C18 §9.6) and `POST /{ds}/sparql/explain` (C18
//! §6.6.6), which share the plan, the notes and the checks of the `explain` role.
//!
//! Without `notes`, the tool answers as C11 defined it. With `notes`, it adds the node
//! facts, the notes and the template description of §6.6. The server's `explain` role
//! writes prose only with `useServerModel`, which needs the `serverModels` permission,
//! and its tokens are charged to the calling principal.

use super::errors::ToolError;
use super::tools::{ExplainArgs, Tools, parse, plan_lines};
use super::{Call, McpServer, Outcome};
use crate::explain::{self, model};
use crate::models::Role;
use serde::Deserialize;
use serde_json::{Map, Value, json};
use std::time::Instant;

fn not_allowed() -> ToolError {
    ToolError::new(
        "server-model-not-allowed",
        403,
        "useServerModel needs the serverModels permission on this dataset",
    )
    .hint("call without useServerModel and write the prose from notes and asks")
}

impl Tools<'_> {
    pub(super) fn explain_query(&self, args: Map<String, Value>) -> Result<Outcome, ToolError> {
        let a: ExplainArgs = parse(args)?;
        if !matches!(a.profile.as_deref(), None | Some("estimate" | "run")) {
            return Err(ToolError::bad_argument("profile must be estimate or run"));
        }
        let use_model = a.use_server_model.unwrap_or(false);
        let notes = a.notes.unwrap_or(false) || use_model;
        // refused before anything runs, so the caller never pays for a refused call
        if use_model {
            let ds = self.dataset(a.dataset.as_deref())?;
            if !self.call.principal.server_models(&ds.name) {
                return Err(not_allowed());
            }
        }
        let started = Instant::now();
        let core = self.explain_core(&a, None)?;
        let mut out = core.out;
        let plan = core.plan.as_ref().expect("the engine's plan");
        if !notes {
            let mut lines = String::new();
            plan_lines(plan, 0, &mut lines);
            out["plan"] = lines.trim_end().into();
            return Ok(Outcome::Structured(out));
        }
        let kind = out["queryType"].as_str().unwrap_or("SELECT").to_string();
        let built = explain::build(
            &core.plan_json,
            &a.query,
            &kind,
            &core.prefixes,
            core.stop.as_ref(),
            core.error.is_some(),
        )
        .map_err(|e| ToolError::new("internal", 500, e))?;
        out["plan"] = explain::text_lines(&built.plan).trim_end().into();
        out["nodes"] = built.plan.facts_json();
        out["notes"] = serde_json::to_value(&built.notes).unwrap_or_default();
        out["asks"] = serde_json::to_value(&built.asks).unwrap_or_default();
        // who wrote `asks` and the notes' text: the templates, or the server's model
        out["source"] = "template".into();
        if use_model {
            let ds = self.dataset(a.dataset.as_deref())?;
            let w = self.explain_with_model(&ds, &a.query, &built, core.stop.as_ref(), started);
            let e = model::explanation(&built.asks, &built.notes, w.as_ref());
            if e["source"] == "model" {
                out["asks"] = e["asks"].clone();
                out["notes"] = e["notes"].clone();
                out["source"] = "model".into();
                out["provider"] = e["provider"].clone();
                out["model"] = e["model"].clone();
            } else if !e["fallback"].is_null() {
                out["fallback"] = e["fallback"].clone();
            }
            if let Some(w) = &w {
                out["usage"] = w.usage();
            }
        }
        Ok(Outcome::Structured(out))
    }

    /// The `explain` role's prose for `built`, charged to the calling principal, or
    /// `None` when the server has no model providers.
    pub(crate) fn explain_with_model(
        &self,
        ds: &crate::state::Dataset,
        query: &str,
        built: &explain::Built,
        stop: Option<&explain::Stop>,
        started: Instant,
    ) -> Option<model::Written> {
        let st = &self.server.state;
        let models = st.models.as_ref()?;
        let settings = crate::assistant::settings(st, ds);
        let me = self.call.principal.id();
        // a daily cap that is used up refuses the call before it is made (§3.5)
        let over = |used: u64, cap: Option<u64>| cap.is_some_and(|c| used >= c);
        if over(
            st.asks.tokens_today(&ds.name, None),
            settings.budget.per_dataset_per_day,
        ) || over(
            st.asks.tokens_today(&ds.name, Some(&me)),
            settings.budget.per_principal_per_day,
        ) {
            return Some(model::Written {
                checked: None,
                pair: None,
                steps: Vec::new(),
                failed: Some((
                    "budget-exceeded",
                    "this dataset's model budget for today is used up".into(),
                )),
            });
        }
        let lists = crate::assistant::lists(models, &settings);
        let pairs = lists.get(&Role::Explain).cloned().unwrap_or_default();
        let user = model::prompt(query, &built.plan, &built.notes, &built.asks);
        let deadline = started
            + std::time::Duration::from_secs_f64(
                settings
                    .deadline_secs
                    .unwrap_or(crate::assistant::DEFAULT_DEADLINE_SECS),
            );
        let extra: Vec<f64> = stop.and_then(|s| s.limit).into_iter().collect();
        let w = model::write(
            models,
            &pairs,
            &user,
            &built.plan,
            &built.notes,
            &extra,
            deadline,
        );
        if !w.steps.is_empty() {
            st.asks.charge(&ds.name, &me, "explain", &w.usage());
        }
        Some(w)
    }
}

/// The body of `POST /{ds}/sparql/explain`, with the dataset its path names.
#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub(crate) struct ExplainBody {
    #[serde(default)]
    pub dataset: Option<String>,
    pub query: String,
    pub profile: Option<String>,
    pub plan: Option<Value>,
    pub commit: Option<u64>,
    /// with `profile: "given"`, the error body of the run that stopped, which names
    /// the budget
    pub error: Option<Value>,
    pub describe: Option<bool>,
    pub at: Option<Value>,
    pub branch: Option<String>,
    pub reasoning: Option<bool>,
    pub timeout_seconds: Option<f64>,
}

/// `POST /{ds}/sparql/explain` (C18 §6.6.6) on this thread: `on` receives the `plan`,
/// `notes` and `explanation` events, and `usage` when a model was called. An error
/// before the plan is returned. One after it is sent as an `error` event.
pub(crate) fn sparql_explain(
    server: &McpServer,
    call: &Call,
    b: ExplainBody,
    on: &mut dyn FnMut(&str, Value),
) -> Result<(), ToolError> {
    let t = Tools { server, call };
    let started = Instant::now();
    let profile = b.profile.as_deref().unwrap_or("estimate");
    if !matches!(profile, "estimate" | "run" | "given") {
        return Err(ToolError::bad_argument(
            "profile must be estimate, run or given",
        ));
    }
    let given = match (profile, b.plan) {
        ("given", Some(p)) => Some(p),
        ("given", None) => return Err(ToolError::bad_argument("profile given needs a plan")),
        (_, Some(_)) => {
            return Err(ToolError::bad_argument(
                "plan is read only with profile given",
            ));
        }
        (_, None) => None,
    };
    // a given plan is checked before anything else reads it
    if let Some(p) = &given {
        explain::Plan::read(p).map_err(|e| {
            ToolError::bad_argument(format!("plan is not a PlanNode or CursorPlan: {e}"))
        })?;
    }
    let ea = ExplainArgs {
        dataset: b.dataset,
        query: b.query.clone(),
        reasoning: b.reasoning,
        at_commit: if profile == "given" { b.commit } else { None },
        at: if profile == "given" { None } else { b.at },
        profile: Some(profile.into()),
        timeout_seconds: b.timeout_seconds,
        ..Default::default()
    };
    let core = t.explain_core(&ea, given)?;
    let ds = t.dataset(ea.dataset.as_deref())?;
    let kind = core.out["queryType"]
        .as_str()
        .unwrap_or("SELECT")
        .to_string();
    let (stop, error) = match (&core.stop, &b.error) {
        (Some(s), _) => (Some(s.clone()), core.error.clone()),
        (None, Some(e)) if profile == "given" => (explain::Stop::of_error_body(e), Some(e.clone())),
        _ => (None, core.error.clone()),
    };
    let built = explain::build(
        &core.plan_json,
        &b.query,
        &kind,
        &core.prefixes,
        stop.as_ref(),
        error.is_some(),
    )
    .map_err(ToolError::bad_argument)?;
    let mut plan = core.out;
    plan["profile"] = profile.into();
    plan["executed"] = built.plan.executed.into();
    plan["plan"] = core.plan_json;
    if let Some(s) = &stop {
        plan["stop"] = serde_json::to_value(s).unwrap_or_default();
    }
    if let Some(e) = &error {
        plan["error"] = e.clone();
    }
    if let Some(br) = call.principal.branch.as_deref() {
        plan["branch"] = br.into();
    }
    on("plan", plan);
    on(
        "notes",
        json!({
            "nodes": built.plan.facts_json(),
            "notes": built.notes,
            "shownNotes": explain::SHOWN_NOTES.min(built.notes.len()),
            "hiddenEstimates": built.plan.hidden,
        }),
    );
    let st = &server.state;
    let settings = crate::assistant::settings(st, &ds);
    let describe =
        b.describe.unwrap_or(settings.enabled && settings.explain) && st.models.is_some();
    let w = if describe {
        t.explain_with_model(&ds, &b.query, &built, stop.as_ref(), started)
    } else {
        None
    };
    on(
        "explanation",
        model::explanation(&built.asks, &built.notes, w.as_ref()),
    );
    if let Some(w) = &w {
        on("usage", w.usage());
    }
    Ok(())
}
