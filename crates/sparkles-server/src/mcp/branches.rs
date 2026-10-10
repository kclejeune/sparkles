//! Branches over MCP (spec C17 Phase 1c): the `branch` argument of the dataset tools,
//! the tools `list_branches`, `create_branch`, `merge_branch` and `delete_branch`, the
//! scratch-branch rules of C17 §5.7 and the expiry of idle scratch branches.
//!
//! A call with `branch` runs as its caller on that branch: the principal is checked on
//! `dataset@branch`, as the HTTP routes check a request that chose a branch, so a grant
//! limited to some branches covers only those, and graph grants and protections apply
//! on the branch as they do on `main`. The branch tools wrap the store's branch API,
//! which the HTTP routes of F09 use too, with the access checks of F09 §6.1 and the
//! relaxation C17 §5.7 makes for scratch branches.
//!
//! Every branch `create_branch` makes is a scratch branch: its entry in the branch table
//! records that and the principal that created it. A principal whose grants cover only
//! some graphs may create one when it may write some graph on the new branch, and may
//! merge or delete only the scratch branches it created. Its merge writes through its
//! own graph view of the target, so a branch that changes any graph it may not write
//! there is refused with `forbidden`, whoever made that change.

use super::errors::ToolError;
use super::render::Prefixes;
use super::schemas::{ToolDef, ds, prefixes, strings, to};
use super::tools::{Tools, dataset_prefixes, parse, selector};
use super::{Call, McpConfig, McpServer, Outcome};
use crate::auth::{Endpoint, Level, Principal, on_branch};
use crate::state::{AppState, Dataset};
use serde::Deserialize;
use serde_json::{Map, Value, json};
use sparkles::branch::{
    BranchInfo, BranchOptions, DeleteOptions, MAIN, MergeOptions, MergeOutcome, Scratch,
};
use sparkles::error::Error;
use sparkles::history::At;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

/// The tools that write: offered only with `--mcp-allow-update` (`--allow-update` over
/// stdio), never on a read-only server, and listed only for a caller who may write to
/// some dataset.
pub(crate) const WRITE_TOOLS: [&str; 5] = [
    "sparql_update",
    "assert_facts",
    "create_branch",
    "merge_branch",
    "delete_branch",
];

/// The tools listed after `sparql_update`, in `tools/list` order.
pub(super) const TOOLS_AFTER_UPDATE: [&str; 5] = [
    "assert_facts",
    "list_branches",
    "create_branch",
    "merge_branch",
    "delete_branch",
];

/// The branch tools, which name branches themselves and take no `branch`.
const BRANCH_TOOLS: [&str; 4] = [
    "list_branches",
    "create_branch",
    "merge_branch",
    "delete_branch",
];

/// Write tools that a retry with the same arguments does not repeat.
pub(super) const IDEMPOTENT: [&str; 1] = ["assert_facts"];

/// The most changed quads a merge preview lists.
const MAX_CHANGES: usize = 100;

fn branch_schema() -> Value {
    json!({"type":"string","pattern":"^[A-Za-z0-9][A-Za-z0-9._-]{0,63}$","description":"Work on this branch of the dataset instead of main (see list_branches). Your grants apply on the branch as they do on main."})
}

/// `def` with the `branch` argument when it reads or writes a dataset: when it takes
/// `dataset` and is not a branch tool.
pub(super) fn with_branch_argument(mut def: ToolDef) -> ToolDef {
    if BRANCH_TOOLS.contains(&def.name) {
        return def;
    }
    if let Some(props) = def
        .input
        .get_mut("properties")
        .and_then(Value::as_object_mut)
        && props.contains_key("dataset")
    {
        props.insert("branch".into(), branch_schema());
    }
    def
}

/// The `branch` property of a stored query's tool.
pub(super) fn stored_branch_property(props: &mut Map<String, Value>) {
    props.insert("branch".into(), branch_schema());
}

/// The call on the branch that `args` names, with `branch` taken out of `args`, or
/// `None` for `main` and for tools without the argument (whose argument parsing then
/// refuses it as unknown).
pub(super) fn branch_call(
    server: &McpServer,
    name: &str,
    args: &mut Map<String, Value>,
    call: &Call,
) -> Result<Option<Call>, ToolError> {
    if BRANCH_TOOLS.contains(&name) || !args.contains_key("branch") {
        return Ok(None);
    }
    let takes = match server.tools().iter().find(|t| t.name == name) {
        Some(t) => t.input["properties"].get("branch").is_some(),
        // a stored query's tool (`<dataset>__<query>`), unless `branch` is a parameter
        None => server
            .stored_tool(&call.principal, name)
            .is_some_and(|t| !t.stored.definition.parameters.contains_key("branch")),
    };
    if !takes {
        return Ok(None);
    }
    let b = match args.remove("branch") {
        Some(Value::String(s)) => s.trim().to_string(),
        _ => return Err(ToolError::bad_argument("branch must be a branch name")),
    };
    if b == MAIN {
        return Ok(None);
    }
    if !sparkles::branch::valid_name(&b) {
        return Err(ToolError::new(
            "invalid-branch",
            400,
            format!("invalid branch name '{b}'"),
        ));
    }
    Ok(Some(Call {
        arrived: call.arrived,
        cancel: call.cancel.clone(),
        request_id: call.request_id.clone(),
        principal: call.principal.clone().on_branch(Some(&b)),
        headers: call.headers.clone(),
        held: call.held.clone(),
    }))
}

/// A structured result of a call on a branch other than `main` names the branch.
pub(super) fn name_branch(
    out: Result<Outcome, ToolError>,
    call: &Call,
) -> Result<Outcome, ToolError> {
    match (out, call.principal.branch.as_deref()) {
        (Ok(Outcome::Structured(Value::Object(mut m))), Some(b))
            if b != MAIN && m.contains_key("dataset") && !m.contains_key("branch") =>
        {
            m.insert("branch".into(), b.into());
            Ok(Outcome::Structured(Value::Object(m)))
        }
        (out, _) => out,
    }
}

fn no_such_branch(name: &str) -> ToolError {
    ToolError::new("no-such-branch", 404, format!("no such branch: {name}"))
        .hint("list_branches lists the branches you may see")
}

fn forbidden(msg: impl Into<String>) -> ToolError {
    ToolError::new("forbidden", 403, msg)
}

/// A store error of the branch API as a tool error.
fn store_error(e: Error, request_id: &str) -> ToolError {
    match e {
        Error::Branch(b) => ToolError::new(b.code, b.status(), b.message.clone()),
        Error::NotPermitted(m) => forbidden(m),
        Error::PreconditionFailed(m) => ToolError::new("precondition-failed", 412, m),
        Error::Timeout => ToolError::new("timeout", 408, "the call exceeded its timeout"),
        Error::StorageFull(m) => ToolError::new("storage-full", 507, m),
        Error::Rejected(r) => ToolError::new("validation-failed", 422, r.to_string()),
        e => {
            tracing::error!(request_id, "MCP branch tool failed: {e}");
            ToolError::internal(request_id)
        }
    }
}

impl McpServer {
    /// The datasets `p` may read on some branch, in name order.
    fn visible_any_branch(&self, p: &Principal) -> Vec<Arc<Dataset>> {
        let unbranched = p.clone().on_branch(None);
        self.state
            .datasets()
            .values()
            .filter(|ds| {
                let patterns = &self.shared.cfg.datasets;
                (patterns.is_empty() || patterns.iter().any(|x| crate::auth::glob(x, &ds.name)))
                    && unbranched.level_any_branch(&ds.name).is_some()
            })
            .cloned()
            .collect()
    }

    /// The dataset object (its `main`) a branch tool names: one `p` may read on some
    /// branch.
    pub(super) fn main_dataset(
        &self,
        p: &Principal,
        name: Option<&str>,
    ) -> Result<Arc<Dataset>, ToolError> {
        let all = self.visible_any_branch(p);
        match name {
            None if all.len() == 1 => Ok(all[0].clone()),
            None => Err(ToolError::new(
                "unknown-dataset",
                404,
                "dataset is required: this server has several datasets",
            )),
            Some(n) => all.iter().find(|d| d.name == n).cloned().ok_or_else(|| {
                ToolError::new("unknown-dataset", 404, format!("no dataset '{n}'"))
                    .hint("list_datasets lists the datasets")
            }),
        }
    }

    /// The dataset a call names, on the branch its principal works on (`main` when
    /// none): a branch the caller may not read answers as one that does not exist.
    pub(super) fn dataset_on_branch(
        &self,
        p: &Principal,
        name: Option<&str>,
    ) -> Result<Arc<Dataset>, ToolError> {
        let Some(b) = p.branch.as_deref().filter(|b| *b != MAIN) else {
            return self.dataset(p, name);
        };
        let main = self.main_dataset(p, name)?;
        if p.level(&main.name).is_none() {
            return Err(no_such_branch(b));
        }
        self.state
            .branch_dataset(&main, b)
            .map_err(|e| store_error(e, "branch"))
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ListArgs {
    dataset: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CreateArgs {
    dataset: Option<String>,
    name: String,
    from: Option<String>,
    at: Option<Value>,
    note: Option<String>,
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct Expect {
    source: Option<u64>,
    target: Option<u64>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct MergeArgs {
    dataset: Option<String>,
    source: String,
    target: Option<String>,
    dry_run: Option<bool>,
    message: Option<String>,
    squash: Option<bool>,
    expect: Option<Expect>,
    changes: Option<usize>,
    timeout_seconds: Option<f64>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DeleteArgs {
    dataset: Option<String>,
    name: String,
    force: Option<bool>,
}

/// When a branch last changed: its last own commit, or its creation when it has none.
fn last_activity_ms(b: &BranchInfo) -> i64 {
    let own = match (&b.head, &b.from) {
        (Some(h), Some(f)) if h.seq > f.seq => Some(h.timestamp_ms),
        _ => None,
    };
    own.unwrap_or(b.created_ms).max(b.created_ms)
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64)
}

impl Tools<'_> {
    /// The caller without the branch of the call (the branch tools name branches).
    fn caller(&self) -> Principal {
        self.call.principal.clone().on_branch(None)
    }

    pub(super) fn list_branches(&self, args: Map<String, Value>) -> Result<Outcome, ToolError> {
        let a: ListArgs = parse(args)?;
        let p = self.caller();
        let ds = self.server.main_dataset(&p, a.dataset.as_deref())?;
        let infos = ds
            .store
            .branches()
            .map_err(|e| store_error(e, &self.call.request_id))?;
        let ttl = self.cfg().scratch_ttl;
        let list: Vec<Value> = infos
            .iter()
            .filter(|b| p.level(&on_branch(&ds.name, &b.name)).is_some())
            .map(|b| {
                let mut j = json!({
                    "name": b.name,
                    "head": b.head.as_ref().map(|h| h.seq),
                    "created": sparkles::commit::rfc3339_ms(b.created_ms),
                    "lastChange": sparkles::commit::rfc3339_ms(last_activity_ms(b)),
                    "upstream": b.upstream,
                    "ahead": b.ahead,
                    "behind": b.behind,
                    "protected": b.protected,
                    "scratch": b.scratch.is_some(),
                });
                if let Some(f) = &b.from {
                    j["from"] = json!({ "branch": f.branch, "commit": f.seq });
                }
                if let Some(s) = &b.scratch {
                    j["creator"] = s.creator.clone().into();
                    if let Some(t) = ttl {
                        j["expires"] = sparkles::commit::rfc3339_ms(
                            last_activity_ms(b) + t.as_millis() as i64,
                        )
                        .into();
                    }
                }
                if let Some(n) = &b.note {
                    j["note"] = crate::mcp::render::label_text(n).into();
                }
                j
            })
            .collect();
        Ok(Outcome::Structured(json!({
            "dataset": ds.name,
            "branches": list,
        })))
    }

    pub(super) fn create_branch(&self, args: Map<String, Value>) -> Result<Outcome, ToolError> {
        let a: CreateArgs = parse(args)?;
        let p = self.caller();
        let ds = self.server.main_dataset(&p, a.dataset.as_deref())?;
        let name = a.name.trim().to_string();
        if name == MAIN || !sparkles::branch::valid_name(&name) {
            return Err(ToolError::new(
                "invalid-branch",
                400,
                format!(
                    "invalid branch name '{name}': letters, digits, '.', '_' and '-', at most 64, starting with a letter or digit"
                ),
            ));
        }
        let from = a.from.as_deref().unwrap_or(MAIN).trim().to_string();
        if p.level(&on_branch(&ds.name, &from)).is_none() {
            return Err(no_such_branch(&from));
        }
        let new = on_branch(&ds.name, &name);
        if p.level_at(&new, Endpoint::Branches)
            .is_none_or(|l| l < Level::Write)
        {
            return Err(forbidden(format!(
                "write access to branch {name} of dataset {} required",
                ds.name
            )));
        }
        // F09 §6.1 asks for grants without graph restrictions; a scratch branch needs
        // only that the caller may write some graph of the dataset on it (C17 §5.7)
        if (p.restricted(&on_branch(&ds.name, &from)) || p.restricted(&new))
            && !writes_some_graph(&p, &new)
        {
            return Err(forbidden(format!(
                "creating branch {name} of dataset {} needs write access to some graph on it",
                ds.name
            )));
        }
        let at = match selector(None, a.at.as_ref())? {
            Some(at) => at,
            None => At::Head,
        };
        let o = BranchOptions {
            from,
            at,
            protected: false,
            note: a.note,
        };
        let rid = &self.call.request_id;
        ds.store
            .create_branch(&name, &o)
            .map_err(|e| store_error(e, rid))?;
        let marked = ds
            .store
            .set_branch_scratch(&name, Some(Scratch { creator: p.id() }));
        let info = match marked {
            Ok(i) => i,
            Err(e) => {
                // an unmarked branch would keep the rights of F09 alone: remove it
                let _ = ds.dataset.delete_branch_with(
                    &name,
                    &DeleteOptions {
                        force: true,
                        reparent: false,
                    },
                );
                return Err(store_error(e, rid));
            }
        };
        let mut j = json!({
            "dataset": ds.name,
            "name": info.name,
            "head": info.head.as_ref().map(|h| h.seq),
            "created": sparkles::commit::rfc3339_ms(info.created_ms),
            "scratch": true,
            "creator": p.id(),
        });
        if let Some(f) = &info.from {
            j["from"] = json!({ "branch": f.branch, "commit": f.seq });
        }
        if let Some(t) = self.cfg().scratch_ttl {
            j["expires"] =
                sparkles::commit::rfc3339_ms(info.created_ms + t.as_millis() as i64).into();
        }
        Ok(Outcome::Structured(j))
    }

    pub(super) fn merge_branch(&self, args: Map<String, Value>) -> Result<Outcome, ToolError> {
        let a: MergeArgs = parse(args)?;
        let timeout = self.timeout(a.timeout_seconds)?;
        let changes = a.changes.unwrap_or(0);
        if changes > MAX_CHANGES {
            return Err(ToolError::bad_argument(format!(
                "changes must be at most {MAX_CHANGES}"
            )));
        }
        let dry_run = a.dry_run.unwrap_or(true);
        if changes > 0 && !dry_run {
            return Err(ToolError::bad_argument("changes needs dryRun"));
        }
        let p = self.caller();
        let ds = self.server.main_dataset(&p, a.dataset.as_deref())?;
        let source = a.source.trim().to_string();
        let target = a.target.as_deref().unwrap_or(MAIN).trim().to_string();
        let (sq, tq) = (on_branch(&ds.name, &source), on_branch(&ds.name, &target));
        for (b, q) in [(&source, &sq), (&target, &tq)] {
            if p.level(q).is_none() {
                return Err(no_such_branch(b));
            }
        }
        if p.level_at(&sq, Endpoint::Merge).is_none() {
            return Err(forbidden(format!(
                "read access to branch {source} of dataset {} through the merge endpoint required",
                ds.name
            )));
        }
        if p.level_at(&tq, Endpoint::Merge)
            .is_none_or(|l| l < Level::Write)
        {
            return Err(forbidden(format!(
                "write access to branch {target} of dataset {} through the merge endpoint required",
                ds.name
            )));
        }
        let rid = &self.call.request_id;
        let restricted = p.restricted(&sq) || p.restricted(&tq);
        let deadline = self.call.arrived + timeout;
        let mut o = MergeOptions {
            max_quads: self.server.state.limits.max_rows as u64,
            deadline: Some(deadline),
            cancel: Some(self.call.cancel.clone()),
            squash: a.squash.unwrap_or(false),
            ..Default::default()
        };
        if restricted {
            // C17 §5.7: only its own scratch branches, and only into graphs it may write
            let info = ds
                .store
                .branch_info(&source)
                .map_err(|e| store_error(e, rid))?;
            if info.scratch.as_ref().is_none_or(|s| s.creator != p.id()) {
                return Err(forbidden(format!(
                    "your access to dataset {} is limited to some graphs or triples: you may merge only the scratch branches you created",
                    ds.name
                )));
            }
            o.write.graphs = Some(
                p.view(&tq, Endpoint::Merge)
                    .unwrap_or_else(|| Arc::new(sparkles::access::GraphAccess::all())),
            );
        }
        o.write.message = self.commit_message(a.message.as_deref())?;
        o.write.author = crate::http::author(&p);
        o.write.deadline = Some(deadline);
        o.write.cancel = Some(self.call.cancel.clone());
        let expect = a.expect.unwrap_or_default();
        o.expect_source = expect.source;
        o.expect_target = expect.target;
        let refused = |e: Error| match e {
            Error::NotPermitted(_) if restricted => forbidden(format!(
                "branch {source} changes graphs you may not write on {target}; nothing was merged"
            )),
            e => store_error(e, rid),
        };
        let st = &self.server.state;
        let t0 = Instant::now();
        let elapsed = || (t0.elapsed().as_secs_f64() * 1e6).round() / 1000.0;
        if dry_run {
            let mut po = o.clone();
            po.write.dry_run = Some(sparkles::preview::DryRun {
                changes,
                all_changes: false,
                max_changes: st.limits.max_rows as u64,
            });
            let prefix_map = dataset_prefixes(&ds);
            let prefixes = Prefixes::new(&prefix_map);
            let mut out = match ds.store.merge(&source, &target, &po) {
                Err(Error::DryRun(pv)) => {
                    let mut doc = super::update::preview_json(
                        &ds.name,
                        &pv,
                        &prefixes,
                        restricted,
                        changes,
                        elapsed(),
                    );
                    doc["mergeable"] = true.into();
                    doc
                }
                Ok(MergeOutcome::Conflicts(c)) => {
                    let mut doc = json!({
                        "dataset": ds.name,
                        "dryRun": true,
                        "committed": false,
                        "wouldCommit": false,
                        "mergeable": false,
                        "conflicts": c.conflicts,
                        "error": format!("{} conflicts: merge_branch does not resolve conflicts. A person resolves them on the merge page of the UI or with `sparkles merge`", c.conflicts),
                    });
                    if !restricted && let Ok(v) = serde_json::to_value(c.as_ref()) {
                        doc["conflictReport"] = v;
                    }
                    doc
                }
                Ok(MergeOutcome::UpToDate(_)) => json!({
                    "dataset": ds.name,
                    "dryRun": true,
                    "committed": false,
                    "wouldCommit": false,
                    "mergeable": true,
                    "upToDate": true,
                }),
                Ok(MergeOutcome::Merged(_)) => {
                    return Err(ToolError::internal(rid));
                }
                Err(e) => return Err(refused(e)),
            };
            // the heads the merge itself must send as `expect`
            let fields = ds
                .store
                .preview_merge(&source, &target, &o)
                .map_err(&refused)?;
            out["expect"] = json!({ "source": fields.source.seq, "target": fields.target.seq });
            let mut m = crate::http::branches::merge_json(&fields, None);
            if restricted && let Some(obj) = m.as_object_mut() {
                obj.retain(|k, _| !matches!(k.as_str(), "cells" | "graphs"));
            }
            out["merge"] = m;
            return Ok(Outcome::Structured(out));
        }
        if expect.source.is_none() || expect.target.is_none() {
            return Err(ToolError::bad_argument(
                "a merge with dryRun false needs expect with the source and target heads that its preview returned",
            )
            .hint("call merge_branch with dryRun true first"));
        }
        let out = ds.store.merge(&source, &target, &o);
        let secs = t0.elapsed().as_secs_f64();
        match &out {
            Ok(o) => crate::http::branches::count_outcome(st, &ds.name, o, secs),
            Err(_) => crate::http::branches::count_merge(st, &ds.name, "refused", None, secs),
        }
        match out.map_err(refused)? {
            MergeOutcome::Conflicts(c) => {
                let mut e = ToolError::new(
                    "merge-conflict",
                    409,
                    format!(
                        "{} conflicts merging {source} into {target}; nothing was merged",
                        c.conflicts
                    ),
                )
                .hint("merge_branch does not resolve conflicts: a person resolves them on the merge page of the UI or with `sparkles merge`");
                if !restricted && let Ok(v) = serde_json::to_value(c.as_ref()) {
                    e = e.data(v);
                }
                Err(e)
            }
            MergeOutcome::UpToDate(r) | MergeOutcome::Merged(r) => {
                let mut j = crate::http::branches::merge_json(&r, None);
                j["dataset"] = ds.name.clone().into();
                j["committed"] = r.merged.into();
                j["elapsedMs"] = elapsed().into();
                Ok(Outcome::Structured(j))
            }
        }
    }

    pub(super) fn delete_branch(&self, args: Map<String, Value>) -> Result<Outcome, ToolError> {
        let a: DeleteArgs = parse(args)?;
        let p = self.caller();
        let ds = self.server.main_dataset(&p, a.dataset.as_deref())?;
        let name = a.name.trim().to_string();
        if name == MAIN {
            return Err(ToolError::new(
                "invalid-branch",
                400,
                "main cannot be deleted",
            ));
        }
        let q = on_branch(&ds.name, &name);
        if p.level(&q).is_none() {
            return Err(no_such_branch(&name));
        }
        if p.level_at(&q, Endpoint::Branches)
            .is_none_or(|l| l < Level::Write)
        {
            return Err(forbidden(format!(
                "write access to branch {name} of dataset {} required",
                ds.name
            )));
        }
        let rid = &self.call.request_id;
        let info = ds
            .store
            .branch_info(&name)
            .map_err(|e| store_error(e, rid))?;
        if info.protected && p.level(&q).is_none_or(|l| l < Level::Admin) {
            return Err(forbidden(format!(
                "branch {name} is protected: admin access required"
            )));
        }
        if p.restricted(&q) && info.scratch.as_ref().is_none_or(|s| s.creator != p.id()) {
            return Err(forbidden(format!(
                "your access to dataset {} is limited to some graphs or triples: you may delete only the scratch branches you created",
                ds.name
            )));
        }
        ds.dataset
            .delete_branch_with(
                &name,
                &DeleteOptions {
                    force: a.force.unwrap_or(false),
                    reparent: false,
                },
            )
            .map_err(|e| store_error(e, rid))?;
        ds.branches.lock().remove(&name);
        Ok(Outcome::Structured(json!({
            "dataset": ds.name,
            "deleted": name,
        })))
    }
}

/// Whether `p` may write at least one graph of the dataset on branch `q`
/// (`dataset@branch`) through the update endpoint.
fn writes_some_graph(p: &Principal, q: &str) -> bool {
    if p.level_at(q, Endpoint::Update)
        .is_none_or(|l| l < Level::Write)
    {
        return false;
    }
    match p.view(q, Endpoint::Update) {
        None => true,
        Some(v) => v.write != sparkles::access::Graphs::none(),
    }
}

/// The definitions of the branch tools.
pub(super) fn tool_defs(cfg: &McpConfig) -> Vec<ToolDef> {
    let branch_item = json!({"type":"object","required":["name","created","lastChange","ahead","behind","protected","scratch"],"properties":{
        "name":{"type":"string"},"head":{"type":["integer","null"]},
        "created":{"type":"string"},"lastChange":{"type":"string"},
        "upstream":{"type":["string","null"]},
        "from":{"type":"object","properties":{"branch":{"type":["string","null"]},"commit":{"type":"integer"}}},
        "ahead":{"type":"integer"},"behind":{"type":"integer"},
        "protected":{"type":"boolean"},"scratch":{"type":"boolean"},
        "creator":{"type":"string"},"expires":{"type":"string"},"note":{"type":"string"}}});
    vec![
        ToolDef {
            name: "list_branches",
            title: "List branches",
            description: "List the branches of a dataset that you may see, main first: each with its head commit, its creation time and last change, how far it is ahead of and behind its upstream, whether it is protected, and whether it is a scratch branch made by create_branch, with its creator and, when the server expires idle scratch branches, when it will expire. Pass a branch name as `branch` to the other tools to read or write it.",
            input: json!({"type":"object","additionalProperties":false,"properties":{
                "dataset": ds()}}),
            output: Some(
                json!({"type":"object","required":["dataset","branches"],"properties":{
                "dataset":{"type":"string"},
                "branches":{"type":"array","items":branch_item}}}),
            ),
            read_only: true,
            open_world: false,
            destructive: false,
        },
        ToolDef {
            name: "create_branch",
            title: "Create a scratch branch",
            description: "Create a scratch branch of a dataset for writes you are unsure of, so main stays unchanged: write there with assert_facts or sparql_update and `branch`, read it back with `branch`, then merge it with merge_branch or discard it with delete_branch. A branch costs a few kilobytes until it is compacted. The branch starts at the head of `from` (main by default) or at the commit `at` names.",
            input: json!({"type":"object","additionalProperties":false,"required":["name"],"properties":{
                "dataset": ds(),
                "name": {"type":"string","pattern":"^[A-Za-z0-9][A-Za-z0-9._-]{0,63}$","description":"The new branch's name"},
                "from": {"type":"string","description":"The branch to start from (default main)"},
                "at": super::schemas::at_sel(),
                "note": {"type":"string","maxLength":1024,"description":"A note shown in branch listings"}}}),
            output: Some(
                json!({"type":"object","required":["dataset","name","created","scratch","creator"],"properties":{
                "dataset":{"type":"string"},"name":{"type":"string"},"head":{"type":["integer","null"]},
                "created":{"type":"string"},"scratch":{"type":"boolean"},"creator":{"type":"string"},
                "from":{"type":"object"},"expires":{"type":"string"}}}),
            ),
            read_only: false,
            open_world: false,
            destructive: false,
        },
        ToolDef {
            name: "merge_branch",
            title: "Merge a branch",
            description: "Merge a branch into another (main by default). By default this is a preview (dryRun true): it lists the changes, whether they conflict, the outcome of the target's validation guard and the storage check, and returns `expect`, the heads it saw. To merge, call again with dryRun false and that `expect`: if either branch moved in between, the merge fails with head-moved and nothing changes. Conflicting merges are refused: a person resolves conflicts on the merge page of the UI or with `sparkles merge`. With grants limited to some graphs you may merge only scratch branches you created, and only into graphs you may write.",
            input: json!({"type":"object","additionalProperties":false,"required":["source"],"properties":{
                "dataset": ds(),
                "source": {"type":"string","description":"The branch to merge"},
                "target": {"type":"string","description":"The branch to merge into (default main)"},
                "dryRun": {"type":"boolean","default":true,"description":"Preview the merge without writing (the default)"},
                "expect": {"type":"object","additionalProperties":false,"properties":{
                    "source":{"type":"integer","minimum":0},"target":{"type":"integer","minimum":0}},
                    "description":"The heads a preview returned; required with dryRun false"},
                "message": {"type":"string","maxLength":1024,"description":"The merge commit's message"},
                "squash": {"type":"boolean","default":false,"description":"Make one commit without a second parent"},
                "changes": {"type":"integer","minimum":0,"maximum":MAX_CHANGES,"description":"With dryRun, list up to this many changed quads"},
                "timeoutSeconds": to(cfg)}}),
            output: Some(
                json!({"type":"object","required":["dataset","committed"],"properties":{
                "dataset":{"type":"string"},"committed":{"type":"boolean"},
                "dryRun":{"type":"boolean"},"wouldCommit":{"type":"boolean"},"mergeable":{"type":"boolean"},
                "upToDate":{"type":"boolean"},"conflicts":{},
                "expect":{"type":"object","properties":{"source":{"type":"integer"},"target":{"type":"integer"}}},
                "merge":{"type":"object"},"validation":{"type":"object"},
                "graphs":{"type":"array"},"changes":{"type":"object"},
                "storage":{"enum":["fits","refused"]},"error":{"type":"string"},
                "merged":{"type":"boolean"},"fastForward":{"type":"boolean"},
                "source":{"type":"object"},"target":{"type":"object"},"commit":{},
                "elapsedMs":{"type":"number"},"prefixes":prefixes()}}),
            ),
            read_only: false,
            open_world: false,
            destructive: true,
        },
        ToolDef {
            name: "delete_branch",
            title: "Delete a branch",
            description: "Delete a branch and its commits, for instance a scratch branch whose writes you discard. A branch with commits its upstream does not have needs force. With grants limited to some graphs you may delete only scratch branches you created. main cannot be deleted.",
            input: json!({"type":"object","additionalProperties":false,"required":["name"],"properties":{
                "dataset": ds(),
                "name": {"type":"string","description":"The branch to delete"},
                "force": {"type":"boolean","default":false,"description":"Delete it also when it has commits that were never merged"}}}),
            output: Some(
                json!({"type":"object","required":["dataset","deleted"],"properties":{
                "dataset":{"type":"string"},"deleted":{"type":"string"},"note":strings()}}),
            ),
            read_only: false,
            open_world: false,
            destructive: true,
        },
    ]
}

// ------------------------------------------------------------- scratch expiry ------

/// Scratch branches the expiry deleted.
static EXPIRED: AtomicU64 = AtomicU64::new(0);
/// Whether this server expires scratch branches (the metric is exported then).
static EXPIRING: AtomicBool = AtomicBool::new(false);

/// Delete the scratch branches of every dataset whose last change is older than `ttl`
/// at `now_ms`. A branch with a running task or a named snapshot is kept, and so is a
/// branch that other branches were created from. Returns the deleted branches as
/// `dataset@branch`.
pub(crate) fn sweep(st: &AppState, ttl: Duration, now_ms: i64) -> Vec<String> {
    let mut out = Vec::new();
    let cutoff = now_ms - ttl.as_millis() as i64;
    for ds in st.datasets().values() {
        let Ok(infos) = ds.store.branches() else {
            continue;
        };
        for b in infos {
            if b.scratch.is_none() || b.broken || last_activity_ms(&b) >= cutoff {
                continue;
            }
            let key = format!("{}@{}", ds.name, b.name);
            let busy = st.tasks.lock().iter().any(|t| {
                t.finished_at.is_none()
                    && (t.dataset == key
                        || (t.dataset == ds.name && t.target.as_deref() == Some(&b.name)))
            });
            if busy {
                continue;
            }
            let pinned = st
                .branch_dataset(ds, &b.name)
                .map(|bd| bd.dataset.history().status().snapshots > 0)
                .unwrap_or(true);
            if pinned {
                continue;
            }
            match ds.dataset.delete_branch_with(
                &b.name,
                &DeleteOptions {
                    force: true,
                    reparent: false,
                },
            ) {
                Ok(_) => {
                    ds.branches.lock().remove(&b.name);
                    EXPIRED.fetch_add(1, Ordering::Relaxed);
                    tracing::info!(
                        "dataset {}: deleted scratch branch {} of {}, idle since {}",
                        ds.name,
                        b.name,
                        b.scratch.as_ref().map_or("", |s| s.creator.as_str()),
                        sparkles::commit::rfc3339_ms(last_activity_ms(&b))
                    );
                    out.push(key);
                }
                Err(e) => tracing::warn!(
                    "dataset {}: could not delete the expired scratch branch {}: {e}",
                    ds.name,
                    b.name
                ),
            }
        }
    }
    out
}

/// Run [`sweep`] in the background, at most every minute and at least four times per
/// `ttl`.
pub(crate) fn spawn_expiry(st: Arc<AppState>, ttl: Duration) {
    EXPIRING.store(true, Ordering::Relaxed);
    let every = (ttl / 4).clamp(Duration::from_secs(1), Duration::from_secs(60));
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(every);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tick.tick().await;
            let st = st.clone();
            if let Err(e) = tokio::task::spawn_blocking(move || sweep(&st, ttl, now_ms())).await {
                tracing::warn!("scratch branch expiry panicked: {e}");
            }
        }
    });
}

/// The expiry's counter in the Prometheus text format, when this server expires
/// scratch branches.
pub(crate) fn metrics(out: &mut String) {
    use std::fmt::Write;
    if !EXPIRING.load(Ordering::Relaxed) {
        return;
    }
    let _ = writeln!(
        out,
        "# HELP sparkles_mcp_scratch_branches_expired_total Scratch branches deleted because they were idle for longer than --mcp-scratch-branch-ttl."
    );
    let _ = writeln!(
        out,
        "# TYPE sparkles_mcp_scratch_branches_expired_total counter"
    );
    let _ = writeln!(
        out,
        "sparkles_mcp_scratch_branches_expired_total {}",
        EXPIRED.load(Ordering::Relaxed)
    );
}

/// The expiry counter (tests).
#[cfg(test)]
pub(crate) fn expired_total() -> u64 {
    EXPIRED.load(Ordering::Relaxed)
}
