//! The reviewer's side of C18 §7.10 and §8.9 as `Tools` methods that MCP does not offer
//! as tools: the inbox, the review of one branch, and the actions **Reject**,
//! **Promote**, **Use existing** and **Edit value**. The HTTP routes of
//! `mcp::review_routes` run them as the request's principal, so each write goes through
//! `assert_facts` or the guard with the caller's grants.

use super::inbox::{FactRef, live_reifiers_of, promotion_args};
use super::iri;
use crate::mcp::errors::ToolError;
use crate::mcp::tools::{Tools, parse};
use crate::mcp::{Call, Outcome};
use crate::state::Dataset;
use serde::Deserialize;
use serde_json::{Map, Value, json};
use sparkles::branch::MAIN;
use std::sync::Arc;

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct InboxArgs {
    dataset: Option<String>,
    limit: Option<u64>,
    timeout_seconds: Option<f64>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct ReviewArgs {
    dataset: Option<String>,
    branch: String,
    limit: Option<u64>,
    timeout_seconds: Option<f64>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct RejectArgs {
    dataset: Option<String>,
    /// the review branch the facts are on (default: main, for session facts)
    branch: Option<String>,
    facts: Vec<FactRef>,
    reason: Option<String>,
    timeout_seconds: Option<f64>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct PromoteArgs {
    dataset: Option<String>,
    facts: Vec<FactRef>,
    /// the graph the facts go to (default: `consolidatedGraph` of memory.json)
    target: Option<String>,
    /// an open review branch to add them to (default: a new `review.{person}.{date}-{n}`)
    branch: Option<String>,
    message: Option<String>,
    timeout_seconds: Option<f64>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct RelinkArgs {
    dataset: Option<String>,
    branch: String,
    from: String,
    to: String,
    timeout_seconds: Option<f64>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct EditArgs {
    dataset: Option<String>,
    branch: Option<String>,
    fact: FactRef,
    /// the new object, as `assert_facts` takes it
    o: String,
    timeout_seconds: Option<f64>,
}

/// The most facts one action takes.
const MAX_ACTION_FACTS: usize = 500;

/// The reviewer's name for messages and branch names.
fn person(t: &Tools) -> String {
    t.call
        .principal
        .caller()
        .user
        .unwrap_or_else(|| "local".to_string())
}

/// A branch-name-safe form of a name.
fn slug(s: &str) -> String {
    let mut out: String = s
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '-'
            }
        })
        .take(32)
        .collect();
    if !out.starts_with(|c: char| c.is_ascii_alphanumeric()) {
        out.insert(0, 'p');
    }
    out
}

fn check_count(n: usize) -> Result<(), ToolError> {
    if n == 0 {
        return Err(ToolError::bad_argument("name at least one fact"));
    }
    if n > MAX_ACTION_FACTS {
        return Err(ToolError::bad_argument(format!(
            "at most {MAX_ACTION_FACTS} facts per call"
        )));
    }
    Ok(())
}

impl Tools<'_> {
    /// The main dataset the call names, as the caller on main.
    fn review_dataset(&self, name: Option<&str>) -> Result<Arc<Dataset>, ToolError> {
        let p = self.call.principal.clone().on_branch(None);
        self.server.main_dataset(&p, name)
    }

    /// This call as the same principal on `branch` (main when `None`).
    fn on(&self, branch: Option<&str>) -> Result<Call, ToolError> {
        let b = branch
            .map(str::trim)
            .filter(|b| !b.is_empty() && *b != MAIN);
        if let Some(b) = b
            && !sparkles::branch::valid_name(b)
        {
            return Err(ToolError::new(
                "invalid-branch",
                400,
                format!("invalid branch name '{b}'"),
            ));
        }
        Ok(Call {
            arrived: self.call.arrived,
            cancel: self.call.cancel.clone(),
            request_id: self.call.request_id.clone(),
            principal: self.call.principal.clone().on_branch(b),
            headers: self.call.headers.clone(),
            held: self.call.held.clone(),
        })
    }

    /// `assert_facts` as the caller on `branch`.
    fn assert_on(
        &self,
        branch: Option<&str>,
        args: Map<String, Value>,
    ) -> Result<Value, ToolError> {
        let call = self.on(branch)?;
        let t = Tools {
            server: self.server,
            call: &call,
        };
        match t.assert_facts(args)? {
            Outcome::Structured(mut v) => {
                if let Some(b) = branch.filter(|b| *b != MAIN) {
                    v["branch"] = b.into();
                }
                Ok(v)
            }
            Outcome::Text(t) => Ok(serde_json::from_str(&t).unwrap_or(Value::String(t))),
        }
    }

    pub(crate) fn memory_inbox(&self, args: Map<String, Value>) -> Result<Outcome, ToolError> {
        let a: InboxArgs = parse(args)?;
        let limit = crate::mcp::tools::bounded(
            "limit",
            a.limit,
            200,
            1,
            super::inbox::MAX_INBOX_FACTS as u64,
        )? as usize;
        let timeout = self.timeout(a.timeout_seconds)?;
        let ds = self.review_dataset(a.dataset.as_deref())?;
        Ok(Outcome::Structured(self.inbox(&ds, limit, timeout)?))
    }

    pub(crate) fn memory_review(&self, args: Map<String, Value>) -> Result<Outcome, ToolError> {
        let a: ReviewArgs = parse(args)?;
        let limit = crate::mcp::tools::bounded("limit", a.limit, 500, 1, 2000)? as usize;
        let timeout = self.timeout(a.timeout_seconds)?;
        let ds = self.review_dataset(a.dataset.as_deref())?;
        let b = a.branch.trim();
        if b == MAIN || !sparkles::branch::valid_name(b) {
            return Err(ToolError::new(
                "invalid-branch",
                400,
                format!("'{b}' is not a branch to review"),
            ));
        }
        Ok(Outcome::Structured(
            self.review_branch(&ds, b, limit, timeout)?,
        ))
    }

    /// **Reject**: retract the facts on main or on a review branch, keeping their
    /// reifiers with `prov:wasInvalidatedBy`.
    pub(crate) fn memory_reject(&self, args: Map<String, Value>) -> Result<Outcome, ToolError> {
        let a: RejectArgs = parse(args)?;
        check_count(a.facts.len())?;
        let ds = self.review_dataset(a.dataset.as_deref())?;
        let who = person(self);
        let mut message = format!("Rejected by {who}");
        if let Some(r) = a.reason.as_deref().map(str::trim).filter(|r| !r.is_empty()) {
            message.push_str(": ");
            message.extend(r.chars().take(500));
        }
        // one call per graph, since a call writes one graph
        let mut by_graph: Vec<(String, Vec<Value>)> = Vec::new();
        for f in &a.facts {
            match by_graph.iter_mut().find(|(g, _)| *g == f.graph) {
                Some((_, v)) => v.push(f.retraction()),
                None => by_graph.push((f.graph.clone(), vec![f.retraction()])),
            }
        }
        let mut results = Vec::new();
        for (g, retract) in by_graph {
            let mut m = Map::new();
            m.insert("dataset".into(), ds.name.clone().into());
            m.insert("graph".into(), g.into());
            m.insert("retract".into(), retract.into());
            m.insert("message".into(), message.clone().into());
            if let Some(t) = a.timeout_seconds {
                m.insert("timeoutSeconds".into(), t.into());
            }
            results.push(self.assert_on(a.branch.as_deref(), m)?);
        }
        Ok(Outcome::Structured(json!({
            "dataset": ds.name,
            "branch": a.branch.as_deref().unwrap_or(MAIN),
            "rejected": a.facts.len(),
            "commits": results,
        })))
    }

    /// **Promote**: on a review branch, assert the facts in the target graph with
    /// reifiers derived from the facts' own, for a person to merge (§8.8).
    pub(crate) fn memory_promote(&self, args: Map<String, Value>) -> Result<Outcome, ToolError> {
        let a: PromoteArgs = parse(args)?;
        check_count(a.facts.len())?;
        let timeout = self.timeout(a.timeout_seconds)?;
        let ds = self.review_dataset(a.dataset.as_deref())?;
        let memory = crate::assist::memory_settings(&self.server.state, &ds);
        let target = a
            .target
            .as_deref()
            .map(|t| t.trim().trim_start_matches('<').trim_end_matches('>').to_string())
            .or_else(|| memory.consolidated_graph.clone())
            .ok_or_else(|| {
                ToolError::bad_argument(
                    "name a target graph, or set consolidatedGraph in the dataset's memory settings",
                )
            })?;
        if memory.is_agent_graph(&target) {
            return Err(ToolError::bad_argument(format!(
                "{target} is an agent graph, where facts stay unreviewed: promote into a reviewed graph"
            )));
        }
        // the facts' reifiers on main
        let prefix_map = crate::mcp::tools::dataset_prefixes(&ds);
        let prefixes = crate::mcp::render::Prefixes::new(&prefix_map);
        let names = prefixes.names();
        let ctx = self.ctx(&names, timeout.as_secs_f64());
        let deadline = self.call.arrived + timeout;
        let r = self.reader(&ds, None, None, Some(false), deadline, &ctx)?;
        let derived = live_reifiers_of(&r, &a.facts).map_err(|e| ctx.engine(e))?;
        drop(r);
        let who = person(self);
        let branch = match a.branch.as_deref().map(str::trim).filter(|b| !b.is_empty()) {
            Some(b) => b.to_string(),
            None => {
                let date: String = super::assert::date_time(super::assert::now_ms())
                    .chars()
                    .take(10)
                    .filter(char::is_ascii_digit)
                    .collect();
                let base = format!("review.{}.{date}", slug(&who));
                let taken: Vec<String> = ds
                    .store
                    .branches()
                    .map(|l| l.into_iter().map(|b| b.name).collect())
                    .unwrap_or_default();
                let name = (1..)
                    .map(|n| format!("{base}-{n}"))
                    .find(|n| !taken.contains(n))
                    .expect("a free name");
                let mut c = Map::new();
                c.insert("dataset".into(), ds.name.clone().into());
                c.insert("name".into(), name.clone().into());
                c.insert(
                    "note".into(),
                    format!("Facts promoted by {who} into {target}").into(),
                );
                self.create_branch(c)?;
                name
            }
        };
        let message = a
            .message
            .clone()
            .unwrap_or_else(|| format!("Promoted by {who}"));
        let args = promotion_args(&ds.name, &target, &a.facts, &derived, &message);
        let out = self.assert_on(Some(&branch), args)?;
        Ok(Outcome::Structured(json!({
            "dataset": ds.name,
            "branch": branch,
            "target": target,
            "promoted": a.facts.len(),
            "commit": out.get("commit").cloned().unwrap_or(Value::Null),
            "committed": out.get("committed").cloned().unwrap_or(false.into()),
        })))
    }

    /// **Use existing**: on a review branch, the facts of a new entity name an existing
    /// one instead, in one commit.
    pub(crate) fn memory_relink(&self, args: Map<String, Value>) -> Result<Outcome, ToolError> {
        let a: RelinkArgs = parse(args)?;
        let timeout = self.timeout(a.timeout_seconds)?;
        let ds = self.review_dataset(a.dataset.as_deref())?;
        let b = a.branch.trim();
        if b == MAIN {
            return Err(ToolError::new(
                "invalid-branch",
                400,
                "Use existing rewrites a review branch, not main",
            ));
        }
        let strip = |s: &str| {
            s.trim()
                .trim_start_matches('<')
                .trim_end_matches('>')
                .to_string()
        };
        let (from, to) = (strip(&a.from), strip(&a.to));
        if from == to
            || oxrdf::NamedNode::new(&from).is_err()
            || oxrdf::NamedNode::new(&to).is_err()
        {
            return Err(ToolError::bad_argument(
                "from and to must be two different IRIs",
            ));
        }
        let call = self.on(Some(b))?;
        let t = Tools {
            server: self.server,
            call: &call,
        };
        let bds =
            self.server.state.branch_dataset(&ds, b).map_err(|_| {
                ToolError::new("no-such-branch", 404, format!("no such branch: {b}"))
            })?;
        let message = format!("Use existing <{to}> for <{from}>, by {}", person(self));
        let mut out = t.relink(&bds, &iri(&from), &iri(&to), Some(message.into()), timeout)?;
        out["branch"] = b.into();
        Ok(Outcome::Structured(out))
    }

    /// **Edit value**: replace a fact's object, deriving the new reifier from the old
    /// one.
    pub(crate) fn memory_edit(&self, args: Map<String, Value>) -> Result<Outcome, ToolError> {
        let a: EditArgs = parse(args)?;
        let ds = self.review_dataset(a.dataset.as_deref())?;
        let timeout = self.timeout(a.timeout_seconds)?;
        let prefix_map = crate::mcp::tools::dataset_prefixes(&ds);
        let prefixes = crate::mcp::render::Prefixes::new(&prefix_map);
        let names = prefixes.names();
        let ctx = self.ctx(&names, timeout.as_secs_f64());
        let deadline = self.call.arrived + timeout;
        let call = self.on(a.branch.as_deref())?;
        let t = Tools {
            server: self.server,
            call: &call,
        };
        let on = match a.branch.as_deref().filter(|b| *b != MAIN) {
            Some(b) => self.server.state.branch_dataset(&ds, b).map_err(|_| {
                ToolError::new("no-such-branch", 404, format!("no such branch: {b}"))
            })?,
            None => ds.clone(),
        };
        let r = t.reader(&on, None, None, Some(false), deadline, &ctx)?;
        let derived = live_reifiers_of(&r, std::slice::from_ref(&a.fact))
            .map_err(|e| ctx.engine(e))?
            .remove(&0)
            .unwrap_or_default();
        drop(r);
        let mut fact = json!({"s": a.fact.s, "p": a.fact.p, "o": a.o});
        if !derived.is_empty() {
            fact["derivedFrom"] = derived
                .iter()
                .map(|r| format!("<{r}>"))
                .collect::<Vec<_>>()
                .into();
        }
        let mut m = Map::new();
        m.insert("dataset".into(), ds.name.clone().into());
        m.insert("graph".into(), a.fact.graph.clone().into());
        m.insert("facts".into(), vec![fact].into());
        m.insert("retract".into(), vec![a.fact.retraction()].into());
        m.insert("allowUnknownIris".into(), true.into());
        m.insert(
            "message".into(),
            format!("Edited by {}", person(self)).into(),
        );
        Ok(Outcome::Structured(self.assert_on(a.branch.as_deref(), m)?))
    }
}
