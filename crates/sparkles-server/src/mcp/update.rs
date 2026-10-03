//! `sparql_update`: the one write tool. It is offered only when the operator turns it on
//! (`sparkles mcp --allow-update`, `serve --mcp-allow-update`), never on a read-only
//! server, and only to callers that may write to the dataset. LOAD is refused, the write
//! passes the dataset's write-time validation, and the commit carries a message. With
//! `dryRun` the update is previewed instead: it runs up to its commit, and the result
//! says what the commit would be (see `docs/specs/C15-write-previews.md`).
//!
//! The tool also applies RDF Patch in its text form (`patch` instead of `update`), as
//! `/{ds}/patch` does, with the caller's write grant on the `patch` endpoint. `ifHead`
//! makes either write conditional on the dataset's head, checked with the writer lock
//! held.

use super::Outcome;
use super::errors::ToolError;
use super::render::{Prefixes, Terms};
use super::tools::{Tools, dataset_prefixes, parse};
use crate::auth::{Endpoint, Level};
use serde::Deserialize;
use serde_json::{Map, Value, json};
use spargebra::{GraphUpdateOperation, SparqlParser};
use sparkles::commit::CommitKind;
use sparkles::guard::Precondition;
use sparkles::store::{PatchOptions, Snapshot};
use std::sync::Arc;
use std::time::Instant;

/// The longest update text, in characters.
const MAX_UPDATE_CHARS: usize = 1 << 20;

/// The most changed quads a dry run lists.
const MAX_CHANGES: usize = 100;

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct UpdateArgs {
    dataset: Option<String>,
    update: Option<String>,
    message: Option<String>,
    timeout_seconds: Option<f64>,
    dry_run: Option<bool>,
    changes: Option<usize>,
    if_head: Option<u64>,
    patch: Option<String>,
}

/// `ifHead`: the write goes ahead only while commit `expected` is the dataset's head.
/// The store checks it with the writer lock held, so that no other commit can come
/// between the check and the write, as for an HTTP `If-Match`.
fn if_head(expected: u64) -> Precondition {
    Precondition::new(move |head: &Snapshot| {
        if head.commit == expected {
            Ok(())
        } else {
            Err(sparkles::Error::PreconditionFailed(format!(
                "ifHead: the head of the dataset is commit {}, not {expected}",
                head.commit
            )))
        }
    })
}

/// What the update parses to, as far as this tool cares.
enum Parsed {
    Update,
    /// at least one `LOAD` operation
    Load,
    /// a query (`sparql_query` runs those)
    Query,
    /// not parsed here: the engine reports the error with its own parser
    Unknown,
}

fn classify(u: &str, prefixes: &[(String, String)]) -> Parsed {
    let parser = || {
        let mut p = SparqlParser::new();
        for (k, v) in prefixes {
            p = p.with_prefix(k, v).ok()?;
        }
        Some(p)
    };
    let Some(p) = parser() else {
        return Parsed::Unknown;
    };
    match p.parse_update(u) {
        Ok(up) => {
            if up
                .operations
                .iter()
                .any(|op| matches!(op, GraphUpdateOperation::Load { .. }))
            {
                Parsed::Load
            } else {
                Parsed::Update
            }
        }
        Err(_) => match parser().map(|p| p.parse_query(u)) {
            Some(Ok(_)) => Parsed::Query,
            _ => Parsed::Unknown,
        },
    }
}

impl Tools<'_> {
    pub(super) fn sparql_update(&self, args: Map<String, Value>) -> Result<Outcome, ToolError> {
        let a: UpdateArgs = parse(args)?;
        let (text, is_patch) = match (&a.update, &a.patch) {
            (Some(u), None) => (u.as_str(), false),
            (None, Some(p)) => (p.as_str(), true),
            (Some(_), Some(_)) => {
                return Err(ToolError::bad_argument(
                    "give update (SPARQL Update) or patch (RDF Patch), not both",
                ));
            }
            (None, None) => {
                return Err(ToolError::bad_argument(
                    "update (SPARQL Update) or patch (RDF Patch) is required",
                ));
            }
        };
        let what = if is_patch { "patch" } else { "update" };
        if text.trim().is_empty() {
            return Err(ToolError::bad_argument(format!("{what} must not be empty")));
        }
        if text.chars().count() > MAX_UPDATE_CHARS {
            return Err(ToolError::bad_argument(format!(
                "{what} must be at most {MAX_UPDATE_CHARS} characters"
            )));
        }
        let timeout = self.timeout(a.timeout_seconds)?;
        let dry_run = a.dry_run.unwrap_or(false);
        let changes = a.changes.unwrap_or(0);
        if changes > MAX_CHANGES {
            return Err(ToolError::bad_argument(format!(
                "changes must be at most {MAX_CHANGES}"
            )));
        }
        if changes > 0 && !dry_run {
            return Err(ToolError::bad_argument("changes needs dryRun"));
        }
        let ds = self.dataset(a.dataset.as_deref())?;
        let p = &self.call.principal;
        let endpoint = if is_patch {
            Endpoint::Patch
        } else {
            Endpoint::Update
        };
        if !p.can(&ds.name, Level::Write) || !p.can_at(&ds.name, endpoint, Level::Write) {
            return Err(ToolError::new(
                "forbidden",
                403,
                format!(
                    "write access to the {} endpoint of dataset {} required",
                    endpoint.as_str(),
                    ds.name
                ),
            )
            .hint("this caller may only read the dataset"));
        }
        let message = self.commit_message(a.message.as_deref())?;
        let prefix_map = dataset_prefixes(&ds);
        let prefixes = Prefixes::new(&prefix_map);
        let names = prefixes.names();
        let ctx = self.ctx(&names, timeout.as_secs_f64());
        let pv: Vec<(String, String)> = prefix_map
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        if !is_patch {
            match classify(text, &pv) {
                Parsed::Load => {
                    return Err(ToolError::new(
                        "load-disabled",
                        403,
                        "LOAD is disabled for MCP updates",
                    )
                    .hint("insert the data with INSERT DATA, or load files with the HTTP API"));
                }
                Parsed::Query => {
                    return Err(
                        ToolError::new("not-an-update", 400, "this is a SPARQL query")
                            .hint("use sparql_query"),
                    );
                }
                Parsed::Update | Parsed::Unknown => {}
            }
        }
        let t0 = Instant::now();
        let deadline = self.call.arrived + timeout;
        let mut opts = self
            .query_options(&ds.name, endpoint, false, deadline, &prefix_map)
            .map_err(|e| ctx.engine(e))?;
        // the engine refuses LOAD too, should the check above not have parsed the update
        opts.forbid_remote_load = true;
        opts.forbid_file_load = true;
        opts.write = sparkles::guard::WriteOptions {
            bypass_validation: false,
            deadline: Some(deadline),
            cancel: Some(self.call.cancel.clone()),
            report_limit: None,
            message: message.clone(),
            precondition: a.if_head.map(if_head),
            no_wait: false,
            graphs: opts.graphs.clone(),
            dry_run: dry_run.then_some(sparkles::preview::DryRun {
                changes,
                all_changes: false,
                max_changes: 0,
            }),
        };
        // a caller limited to some graphs learns that the guard refused, not its results
        let restricted = opts.graphs.is_some();
        let refused = |e: sparkles::Error| match e {
            sparkles::Error::Rejected(_) if restricted => ToolError::new(
                "validation-failed",
                422,
                format!(
                    "the {what} does not conform to the dataset's validation guard; nothing was written"
                ),
            ),
            e => ctx.engine(e),
        };
        let elapsed = || (t0.elapsed().as_secs_f64() * 1000.0 * 1000.0).round() / 1000.0;
        // (receipt, inserted, deleted, the patch's own counts)
        let (receipt, inserted, deleted, patch) = if is_patch {
            let po = PatchOptions {
                write: opts.write.clone(),
                binary: false,
            };
            match ds.store.apply_patch(text.as_bytes(), &po) {
                Err(sparkles::Error::DryRun(p)) => {
                    return Ok(Outcome::Structured(preview_json(
                        &ds.name,
                        &p,
                        &prefixes,
                        restricted,
                        changes,
                        elapsed(),
                    )));
                }
                Err(e) => return Err(refused(e)),
                Ok(o) => {
                    let counts = json!({
                        "rows": o.rows,
                        "aborted": o.aborted,
                        "prevChecked": o.prev_checked,
                        "prefixesSet": o.prefixes_set,
                        "prefixesRemoved": o.prefixes_removed,
                    });
                    (o.receipt, o.inserted, o.deleted, Some(counts))
                }
            }
        } else {
            match sparkles::sparql::update::update_as(&ds.store, text, &opts, CommitKind::Update) {
                Err(sparkles::Error::DryRun(p)) => {
                    return Ok(Outcome::Structured(preview_json(
                        &ds.name,
                        &p,
                        &prefixes,
                        restricted,
                        changes,
                        elapsed(),
                    )));
                }
                Err(e) => return Err(refused(e)),
                Ok(stats) => {
                    let Some(receipt) = stats.commit else {
                        return Err(ToolError::internal(&self.call.request_id));
                    };
                    (receipt, stats.inserted, stats.deleted, None)
                }
            }
        };
        let mut out = json!({
            "dataset": ds.name,
            "committed": receipt.committed,
            "commit": receipt.commit.seq,
            "inserted": inserted,
            "deleted": deleted,
            "elapsedMs": elapsed(),
        });
        if let Some(counts) = patch {
            out["patch"] = counts;
        }
        if let Some(m) = &message {
            out["message"] = json!(m.as_ref());
        }
        if let Some(v) = receipt
            .validation
            .as_deref()
            .filter(|_| !restricted)
            .and_then(|v| serde_json::to_value(v).ok())
        {
            out["validation"] = v;
        }
        Ok(Outcome::Structured(out))
    }

    /// The commit message: the `message` argument, else the HTTP request's
    /// `Sparkles-Commit-Message` header, checked like the HTTP API's.
    fn commit_message(&self, arg: Option<&str>) -> Result<Option<Arc<str>>, ToolError> {
        match arg {
            Some(m) => sparkles::annotations::validate_message(m)
                .map_err(|e| ToolError::bad_argument(format!("message: {e}"))),
            None => match &self.call.headers {
                Some(h) => crate::http::commit_message_header(h).map_err(ToolError::bad_argument),
                None => Ok(None),
            },
        }
    }
}

/// The result of a dry run: what the commit would be, its counts by graph, the first
/// `changes` changed quads as rendered terms, and the checks the write would meet. A
/// rejected or refused update is a result here, not a tool error, so that an agent can
/// read the findings and revise the update.
fn preview_json(
    dataset: &str,
    p: &sparkles::preview::Preview,
    prefixes: &Prefixes,
    restricted: bool,
    changes: usize,
    elapsed_ms: f64,
) -> Value {
    let c = p.receipt_commit();
    let mut terms = Terms::new(prefixes, 500);
    let mut out = json!({
        "dataset": dataset,
        "dryRun": true,
        "committed": false,
        "wouldCommit": p.commit.is_some(),
        "outcome": p.outcome().name(),
        "head": p.head.seq,
        "commit": c.seq,
        "inserted": if p.commit.is_some() { c.inserted } else { 0 },
        "deleted": if p.commit.is_some() { c.deleted } else { 0 },
        "graphs": p.graphs.iter().map(|g| {
            let name = match &g.graph {
                oxrdf::GraphName::DefaultGraph => Value::Null,
                oxrdf::GraphName::NamedNode(n) => json!(terms.term(&oxrdf::Term::NamedNode(n.clone()))),
                oxrdf::GraphName::BlankNode(b) => json!(b.to_string()),
            };
            json!({ "graph": name, "inserted": g.inserted, "deleted": g.deleted })
        }).collect::<Vec<_>>(),
        "storage": if p.storage.refused.is_some() { "refused" } else { "fits" },
        "elapsedMs": elapsed_ms,
    });
    if let Some(m) = &p.message {
        out["message"] = json!(m.as_ref());
    }
    if let Some(total) = p.changes_total.filter(|_| changes > 0) {
        let rows: Vec<Value> = p
            .changes
            .iter()
            .take(changes)
            .map(|(op, q)| {
                let mut line = format!(
                    "{} {} {} {}",
                    op.sign(),
                    terms.term(&q.subject.clone().into()),
                    terms.term(&oxrdf::Term::NamedNode(q.predicate.clone())),
                    terms.term(&q.object),
                );
                match &q.graph_name {
                    oxrdf::GraphName::DefaultGraph => {}
                    oxrdf::GraphName::NamedNode(n) => {
                        line.push(' ');
                        line.push_str(&terms.term(&oxrdf::Term::NamedNode(n.clone())));
                    }
                    oxrdf::GraphName::BlankNode(b) => {
                        line.push(' ');
                        line.push_str(&b.to_string());
                    }
                }
                json!(line)
            })
            .collect();
        out["changes"] = json!({
            "total": total,
            "truncated": (rows.len() as u64) < total,
            "quads": rows,
        });
    }
    if let Some(v) = p
        .validation
        .as_deref()
        .filter(|_| !restricted)
        .and_then(|v| serde_json::to_value(v).ok())
    {
        out["validation"] = v;
    }
    match p.outcome() {
        sparkles::preview::Outcome::Rejected if restricted => {
            out["error"] = json!(
                "the update does not conform to the dataset's validation guard; it would not be committed"
            );
        }
        _ => {
            if let Some(e) = p.error() {
                out["error"] = json!(e);
            }
        }
    }
    out["prefixes"] = json!(terms.used());
    out
}
