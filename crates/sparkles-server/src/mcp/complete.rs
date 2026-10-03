//! `completion/complete`: suggestions for the arguments of prompts and resource
//! templates, from what the caller may see.
//!
//! * `dataset`: the names of the datasets the caller may read (for `run_stored_query`
//!   and the stored-query template, those with stored queries it may run).
//! * `query`: the stored queries of the dataset named in the context.
//! * `arguments` (`run_stored_query`): the parameter names of that stored query that the
//!   value does not give yet, as `name=`.
//! * `graph`: the named graphs of the dataset that the caller's view holds, at most
//!   [`MAX_GRAPHS`] read.
//! * `term` (`explain_term`): the dataset's prefixes, as `pfx:`.
//!
//! A value matches when it starts with the typed text. At most 100 values are returned,
//! with the total and `hasMore`.

use super::context::{ContextError, Kind, parse_uri};
use super::tools::{Tools, dataset_prefixes, remaining};
use super::{Call, McpServer};
use crate::auth::Endpoint;
use oxrdf::{Literal, Term};
use std::collections::{BTreeSet, HashMap};
use std::time::Duration;

/// The most named graphs a completion reads.
pub const MAX_GRAPHS: usize = 1000;

/// The most values of one completion (the protocol's limit).
pub const MAX_VALUES: usize = 100;

/// How long the graph listing of a completion may take.
const GRAPH_TIMEOUT: Duration = Duration::from_secs(5);

/// What a completion is for.
#[derive(Clone, Debug)]
pub enum Target {
    Prompt(String),
    /// a resource template's URI, such as `sparkles://{dataset}/schema`
    Template(String),
}

/// A `completion/complete` request.
#[derive(Clone, Debug)]
pub struct Request {
    pub target: Target,
    pub argument: String,
    pub value: String,
    /// the values of the other arguments
    pub context: HashMap<String, String>,
}

/// The values of a completion, at most [`MAX_VALUES`], with the total of matches.
pub struct Completion {
    pub values: Vec<String>,
    pub total: usize,
}

impl McpServer {
    /// `completion/complete` for `call.principal`. It runs on a blocking thread, since
    /// the graph names come from a query.
    pub async fn complete(&self, req: Request, call: Call) -> Result<Completion, ContextError> {
        let server = self.clone();
        let request_id = call.request_id.clone();
        tokio::task::spawn_blocking(move || {
            Tools {
                server: &server,
                call: &call,
            }
            .complete(&req)
        })
        .await
        .unwrap_or_else(|e| {
            tracing::error!(request_id, "MCP completion panicked: {e}");
            Err(ContextError::Tool(super::errors::ToolError::internal(
                &request_id,
            )))
        })
    }
}

/// The values that start with `prefix`, sorted and without duplicates.
fn matching(values: impl IntoIterator<Item = String>, prefix: &str) -> Completion {
    let all: BTreeSet<String> = values
        .into_iter()
        .filter(|v| v.starts_with(prefix))
        .collect();
    let total = all.len();
    Completion {
        values: all.into_iter().take(MAX_VALUES).collect(),
        total,
    }
}

impl Tools<'_> {
    fn complete(&self, req: &Request) -> Result<Completion, ContextError> {
        let unknown = |what: &str| ContextError::InvalidParams(format!("unknown {what}"));
        // which prompt or template, and whether it is about stored queries
        let (stored_only, args): (bool, &[&str]) = match &req.target {
            Target::Prompt(name) => match name.as_str() {
                "explore_dataset" => (false, &["dataset", "graph"]),
                "answer_question" => (false, &["dataset", "question", "graph"]),
                "run_stored_query" => (true, &["dataset", "query", "arguments"]),
                "explain_term" => (false, &["dataset", "term"]),
                _ => return Err(unknown(&format!("prompt: {name}"))),
            },
            Target::Template(uri) => {
                let template = uri.replace("{dataset}", "d").replace("{query}", "q");
                match parse_uri(&template) {
                    Some((_, Kind::Query, _)) => (true, &["dataset", "query"]),
                    Some(_) => (false, &["dataset"]),
                    None => return Err(unknown(&format!("resource template: {uri}"))),
                }
            }
        };
        let arg = req.argument.as_str();
        if !args.contains(&arg) {
            return Err(ContextError::InvalidParams(format!(
                "no argument {arg} to complete (arguments: {})",
                args.join(", ")
            )));
        }
        let p = &self.call.principal;
        let value = req.value.as_str();
        if arg == "dataset" {
            let names: Vec<String> = if stored_only {
                self.server
                    .stored_tools(p)
                    .into_iter()
                    .map(|t| t.dataset.name.clone())
                    .collect()
            } else {
                self.server
                    .visible(p)
                    .iter()
                    .map(|d| d.name.clone())
                    .collect()
            };
            return Ok(matching(names, value));
        }
        // the other arguments are about the dataset of the context (or the only one)
        let Ok(ds) = self.dataset(req.context.get("dataset").map(String::as_str)) else {
            return Ok(matching(Vec::new(), value));
        };
        match arg {
            "query" => Ok(matching(
                self.server
                    .stored_tools(p)
                    .into_iter()
                    .filter(|t| t.dataset.name == ds.name)
                    .map(|t| t.query),
                value,
            )),
            "arguments" => {
                let Some(t) = req.context.get("query").and_then(|q| {
                    self.server
                        .stored_tools(p)
                        .into_iter()
                        .find(|t| t.dataset.name == ds.name && &t.query == q)
                }) else {
                    return Ok(matching(Vec::new(), value));
                };
                // `a=1, b=2, c` → the names after the last comma, not given yet
                let (done, typing) = match value.rfind(',') {
                    Some(i) => (&value[..=i], value[i + 1..].trim_start()),
                    None => ("", value.trim_start()),
                };
                let given: BTreeSet<&str> = done
                    .split(',')
                    .filter_map(|kv| kv.split_once('=').map(|(k, _)| k.trim()))
                    .collect();
                let lead = if done.is_empty() {
                    String::new()
                } else {
                    format!("{done} ")
                };
                let names = t
                    .stored
                    .definition
                    .parameters
                    .keys()
                    .filter(|k| !given.contains(k.as_str()) && k.starts_with(typing))
                    .map(|k| format!("{lead}{k}="));
                Ok(matching(names.collect::<Vec<_>>(), ""))
            }
            "term" => Ok(matching(
                dataset_prefixes(&ds).into_keys().map(|k| format!("{k}:")),
                value,
            )),
            "graph" => Ok(matching(self.graphs(&ds, value)?, value)),
            // free text
            _ => Ok(matching(Vec::new(), value)),
        }
    }

    /// The named graphs of `ds` in the caller's view whose IRI starts with `prefix`, at
    /// most [`MAX_GRAPHS`].
    fn graphs(
        &self,
        ds: &crate::state::Dataset,
        prefix: &str,
    ) -> Result<Vec<String>, ContextError> {
        let deadline = self.call.arrived + GRAPH_TIMEOUT;
        let failed = |e: sparkles::Error| {
            ContextError::Tool(self.ctx(&[], GRAPH_TIMEOUT.as_secs_f64()).engine(e))
        };
        let mut opts = self
            .query_options(
                &ds.name,
                Endpoint::Query,
                false,
                deadline,
                &Default::default(),
            )
            .map_err(failed)?;
        opts.timeout = Some(remaining(deadline).map_err(failed)?);
        let filter = Literal::new_simple_literal(prefix);
        let q = format!(
            "SELECT DISTINCT ?g WHERE {{ GRAPH ?g {{ }} FILTER(STRSTARTS(STR(?g), {filter})) }} ORDER BY ?g LIMIT {MAX_GRAPHS}"
        );
        let rows = sparkles::sparql::query(ds.store.snapshot(), &q, &opts)
            .map_err(failed)?
            .rows();
        Ok(rows
            .into_iter()
            .filter_map(|r| match r.into_iter().next().flatten() {
                Some(Term::NamedNode(n)) => Some(n.into_string()),
                _ => None,
            })
            .collect())
    }
}
