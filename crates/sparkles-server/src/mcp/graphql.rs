//! `graphql_query`: a read-only GraphQL query against a dataset's GraphQL API (C03). The
//! tool is listed only while a dataset the caller may query through GraphQL has a mapping
//! schema installed. A call runs like `GET /{ds}/graphql` for the caller: with its graph
//! view, the protections of triples, the budgets of MCP calls, and no mutations. Without
//! `query` the tool returns the API schema (SDL) to write queries against.

use super::McpServer;
use super::Outcome;
use super::errors::ToolError;
use super::tools::{Tools, bounded, dataset_prefixes, parse};
use crate::auth::{Endpoint, Level, Principal};
use crate::state::Dataset;
use serde::Deserialize;
use serde_json::{Map, Value, json};
use sparkles::history::At;
use std::sync::Arc;

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct GraphqlArgs {
    dataset: Option<String>,
    query: Option<String>,
    variables: Option<Map<String, Value>>,
    operation_name: Option<String>,
    max_bytes: Option<u64>,
    timeout_seconds: Option<f64>,
    reasoning: Option<bool>,
    at_commit: Option<u64>,
    at: Option<Value>,
}

/// The longest GraphQL document, in characters.
const MAX_QUERY_CHARS: usize = 65536;

/// Whether `p` may run GraphQL on `ds` and `ds` has a mapping schema installed.
pub fn graphql_on(p: &Principal, ds: &Dataset) -> bool {
    p.can_at(&ds.name, Endpoint::Graphql, Level::Read)
        && ds.dataset.graphql().compiled().ok().flatten().is_some()
}

impl McpServer {
    /// Whether `p` may run GraphQL on some dataset it sees that has a mapping schema.
    pub fn graphql_available(&self, p: &Principal) -> bool {
        self.visible(p).iter().any(|ds| graphql_on(p, ds))
    }
}

impl Tools<'_> {
    pub(super) fn graphql_query(&self, args: Map<String, Value>) -> Result<Outcome, ToolError> {
        let a: GraphqlArgs = parse(args)?;
        let cfg = self.cfg();
        let max_bytes = bounded(
            "maxBytes",
            a.max_bytes,
            65536.min(cfg.max_bytes as u64),
            1024,
            cfg.max_bytes as u64,
        )?;
        let timeout = self.timeout(a.timeout_seconds)?;
        let ds = self.dataset(a.dataset.as_deref())?;
        let compiled = ds
            .dataset
            .graphql()
            .compiled()
            .map_err(|e| {
                tracing::error!("GraphQL schema of /{}: {e}", ds.name);
                ToolError::internal(&self.call.request_id)
            })?
            .ok_or_else(|| {
                ToolError::new(
                    "graphql-not-installed",
                    404,
                    format!("dataset {} has no GraphQL schema installed", ds.name),
                )
                .hint("use sparql_query, or a dataset that list_datasets marks with graphql=true")
            })?;
        let prefix_map = dataset_prefixes(&ds);
        let ctx = self.ctx(&[], timeout.as_secs_f64());
        let deadline = self.call.arrived + timeout;
        let reasoning = Self::reasoning(&ds, a.reasoning);
        let opts = self
            .query_options(
                &ds.name,
                Endpoint::Graphql,
                reasoning,
                deadline,
                &prefix_map,
            )
            .map_err(|e| ctx.engine(e))?;
        let Some(query) = a.query else {
            return Ok(Outcome::Text(compiled.api_sdl.clone()));
        };
        if query.trim().is_empty() || query.chars().count() > MAX_QUERY_CHARS {
            return Err(ToolError::bad_argument(format!(
                "query must be a GraphQL document of 1 to {MAX_QUERY_CHARS} characters"
            )));
        }
        let snap = self.snapshot(&ds, a.at_commit, a.at.as_ref(), deadline)?;
        let req = sparkles_graphql::Request {
            query,
            operation_name: a.operation_name,
            variables: a.variables.unwrap_or_default(),
        };
        let gopts = sparkles_graphql::Options {
            limits: self.server.state.graphql_limits,
            admin: self.call.principal.can(&ds.name, Level::Admin),
            explain: false,
            // as a GET: queries only, a mutation is refused
            get: true,
            at: None,
            max_result_bytes: Some(max_bytes),
            query: opts,
        };
        // the call's state, and the commits that cursors name
        let resolve = |at: Option<&At>| -> sparkles::Result<Arc<sparkles::store::Snapshot>> {
            match at {
                None => Ok(snap.clone()),
                Some(at) => Ok(ds
                    .store
                    .snapshot_at(
                        at,
                        &sparkles::history::HistoryOptions {
                            cancel: Some(self.call.cancel.clone()),
                            deadline: Some(deadline),
                        },
                    )?
                    .0),
            }
        };
        let r = ds
            .dataset
            .graphql()
            .execute_with(&req, &gopts, &resolve)
            .map_err(|e| ctx.engine(e))?;
        use sparkles_graphql::Outcome as O;
        let fail = |code: &'static str, status: u16| {
            let msg = r.body["errors"][0]["message"]
                .as_str()
                .unwrap_or("the GraphQL request failed")
                .to_string();
            ToolError::new(code, status, msg)
        };
        match r.outcome {
            O::Executed => {}
            O::Timeout => {
                return Err(ctx.engine(sparkles::Error::Timeout));
            }
            O::Budget => {
                return Err(fail("budget", 507)
                    .hint("ask for fewer fields or a smaller first:, or raise maxBytes"));
            }
            O::Cancelled => return Err(ctx.engine(sparkles::Error::Cancelled)),
            O::MethodNotAllowed => {
                return Err(fail("graphql-error", 405)
                    .hint("mutations are not available through MCP; graphql_query only reads"));
            }
            O::Internal => return Err(ToolError::internal(&self.call.request_id)),
            _ => {
                return Err(fail("graphql-error", 400).hint(
                    "check the document against the API schema (call graphql_query without query)",
                ));
            }
        }
        let mut out = json!({
            "dataset": ds.name,
            "commit": r.commit.unwrap_or(snap.commit),
        });
        if let Some(o) = r.body.as_object() {
            for (k, v) in o {
                out[k.as_str()] = v.clone();
            }
        }
        Ok(Outcome::Text(out.to_string()))
    }
}
