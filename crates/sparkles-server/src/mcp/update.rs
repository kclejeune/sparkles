//! `sparql_update`: the one write tool. It is offered only when the operator turns it on
//! (`sparkles mcp --allow-update`, `serve --mcp-allow-update`), never on a read-only
//! server, and only to callers that may write to the dataset. LOAD is refused, the write
//! passes the dataset's write-time validation, and the commit carries a message.

use super::Outcome;
use super::errors::ToolError;
use super::render::Prefixes;
use super::tools::{Tools, dataset_prefixes, parse};
use crate::auth::Level;
use serde::Deserialize;
use serde_json::{Map, Value, json};
use spargebra::{GraphUpdateOperation, SparqlParser};
use sparkles::commit::CommitKind;
use std::sync::Arc;
use std::time::Instant;

/// The longest update text, in characters.
const MAX_UPDATE_CHARS: usize = 1 << 20;

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct UpdateArgs {
    dataset: Option<String>,
    update: String,
    message: Option<String>,
    timeout_seconds: Option<f64>,
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
        if a.update.trim().is_empty() {
            return Err(ToolError::bad_argument("update must not be empty"));
        }
        if a.update.chars().count() > MAX_UPDATE_CHARS {
            return Err(ToolError::bad_argument(format!(
                "update must be at most {MAX_UPDATE_CHARS} characters"
            )));
        }
        let timeout = self.timeout(a.timeout_seconds)?;
        let ds = self.dataset(a.dataset.as_deref())?;
        let p = &self.call.principal;
        if !p.can(&ds.name, Level::Write) {
            return Err(ToolError::new(
                "forbidden",
                403,
                format!("write access to dataset {} required", ds.name),
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
        match classify(&a.update, &pv) {
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
        let t0 = Instant::now();
        let deadline = self.call.arrived + timeout;
        let mut opts = self
            .query_options(
                &ds.name,
                crate::auth::Endpoint::Update,
                false,
                deadline,
                &prefix_map,
            )
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
            precondition: None,
            no_wait: false,
            graphs: opts.graphs.clone(),
        };
        // a caller limited to some graphs learns that the guard refused, not its results
        let restricted = opts.graphs.is_some();
        let stats =
            sparkles::sparql::update::update_as(&ds.store, &a.update, &opts, CommitKind::Update)
                .map_err(|e| match e {
                    sparkles::Error::Rejected(_) if restricted => ToolError::new(
                        "validation-failed",
                        422,
                        "the update does not conform to the dataset's validation guard; nothing was written",
                    ),
                    e => ctx.engine(e),
                })?;
        let Some(receipt) = stats.commit else {
            return Err(ToolError::internal(&self.call.request_id));
        };
        let elapsed_ms = (t0.elapsed().as_secs_f64() * 1000.0 * 1000.0).round() / 1000.0;
        let mut out = json!({
            "dataset": ds.name,
            "committed": receipt.committed,
            "commit": receipt.commit.seq,
            "inserted": stats.inserted,
            "deleted": stats.deleted,
            "elapsedMs": elapsed_ms,
        });
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
