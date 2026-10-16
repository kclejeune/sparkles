//! `graph_algorithm`: the graph algorithms of `SERVICE algo:…` (F14) as a read-only tool.
//! The tool builds the request from its arguments, runs it as a query of the caller with
//! the caller's view and budgets, and returns the best rows of the algorithm's main
//! measure as a compact table with the statistics of the run. Results are never written
//! back by the tool. Every IRI in the arguments is validated and written as `<…>`, so
//! nothing the caller sends becomes query syntax.

use super::Outcome;
use super::errors::ToolError;
use super::render::{Prefixes, Terms};
use super::tools::{Tools, bounded, dataset_prefixes, parse, parse_iri};
use oxrdf::Term;
use serde::Deserialize;
use serde_json::{Map, Value, json};
use sparkles::algo::catalog::{PredicateDef, ProjectionDef};
use sparkles::sparql;
use sparkles::sparql::algorun::{AlgoRequest, ProjectionRef};
use std::collections::BTreeMap;

/// The most rows a call returns.
const MAX_ROWS: u64 = 100;

/// The most predicates or node classes a call may name.
const MAX_NAMES: usize = 20;

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct AlgoArgs {
    dataset: Option<String>,
    algorithm: String,
    projection: Option<String>,
    predicates: Option<Vec<String>>,
    direction: Option<String>,
    node_classes: Option<Vec<String>>,
    weight: Option<String>,
    params: Option<Map<String, Value>>,
    limit: Option<u64>,
    reasoning: Option<bool>,
    at_commit: Option<u64>,
    at: Option<Value>,
    timeout_seconds: Option<f64>,
}

/// A number for a numeric literal, else the compact term.
fn cell(t: &Option<Term>, terms: &mut Terms) -> Value {
    match t {
        None => Value::Null,
        Some(Term::Literal(l))
            if matches!(
                l.datatype().as_str(),
                "http://www.w3.org/2001/XMLSchema#integer"
                    | "http://www.w3.org/2001/XMLSchema#double"
                    | "http://www.w3.org/2001/XMLSchema#decimal"
            ) =>
        {
            l.value().parse::<f64>().map_or_else(
                |_| json!(terms.term(&Term::Literal(l.clone()))),
                super::number,
            )
        }
        Some(t) => json!(terms.term(t)),
    }
}

impl Tools<'_> {
    pub(super) fn graph_algorithm(&self, args: Map<String, Value>) -> Result<Outcome, ToolError> {
        let a: AlgoArgs = parse(args)?;
        let limit = bounded("limit", a.limit, 20, 1, MAX_ROWS)?;
        let timeout = self.timeout(a.timeout_seconds)?;
        let ds = self.dataset(a.dataset.as_deref())?;
        let prefix_map = dataset_prefixes(&ds);
        let prefixes = Prefixes::new(&prefix_map);
        let names = prefixes.names();
        let ctx = self.ctx(&names, timeout.as_secs_f64());
        let iri = |what: &str, s: &str| -> Result<String, ToolError> {
            match parse_iri(s, &prefix_map, false)? {
                Term::NamedNode(n) => Ok(n.as_str().to_string()),
                _ => Err(ToolError::bad_argument(format!("{what} must be an IRI"))),
            }
        };
        let list = |what: &str, v: &Option<Vec<String>>| -> Result<Vec<String>, ToolError> {
            let v = v.as_deref().unwrap_or_default();
            if v.len() > MAX_NAMES {
                return Err(ToolError::bad_argument(format!(
                    "{what}: at most {MAX_NAMES} entries"
                )));
            }
            v.iter().map(|s| iri(what, s)).collect()
        };
        let predicates = list("predicates", &a.predicates)?;
        let node_classes = list("nodeClasses", &a.node_classes)?;
        let projection = match &a.projection {
            Some(name) => {
                if !predicates.is_empty()
                    || !node_classes.is_empty()
                    || a.direction.is_some()
                    || a.weight.is_some()
                {
                    return Err(ToolError::bad_argument(
                        "projection names a stored projection and cannot be combined with predicates, direction, nodeClasses or weight",
                    ));
                }
                Some(ProjectionRef::Named(name.clone()))
            }
            None => Some(ProjectionRef::Inline(ProjectionDef {
                predicates: (!predicates.is_empty()).then(|| {
                    predicates
                        .iter()
                        .map(|p| PredicateDef::new(p, None))
                        .collect()
                }),
                direction: a.direction.clone(),
                node_classes,
                weight: a.weight.as_deref().map(|w| iri("weight", w)).transpose()?,
                ..Default::default()
            })),
        };
        let mut params = a.params.unwrap_or_default();
        if params.contains_key("limit") || params.contains_key("maxMemory") {
            return Err(ToolError::bad_argument(
                "limit is an argument of the tool, and maxMemory is the server's",
            ));
        }
        params.insert("limit".into(), limit.into());
        let req = AlgoRequest {
            algorithm: a.algorithm.clone(),
            projection,
            params,
            write: None,
        };
        let query = ds
            .dataset
            .algorithms()
            .query_text_with_summary(&req)
            .map_err(|e| ToolError::bad_argument(e.to_string()))?;
        let deadline = self.call.arrived + timeout;
        let snap = self.snapshot(&ds, a.at_commit, a.at.as_ref(), deadline)?;
        let reasoning = Self::reasoning(&ds, a.reasoning);
        let opts = self
            .query_options(
                &ds.name,
                crate::auth::Endpoint::Query,
                reasoning,
                deadline,
                &BTreeMap::new(),
            )
            .map_err(|e| ctx.engine(e))?;
        let result = sparql::query(snap.clone(), &query, &opts).map_err(|e| ctx.engine(e))?;
        let mut columns = result.vars.clone();
        let summary_at = columns.iter().position(|v| v == "summary");
        if let Some(i) = summary_at {
            columns.remove(i);
        }
        let rows = result.rows();
        let mut terms = Terms::new(&prefixes, 200);
        let mut summary = Value::Null;
        let mut table: Vec<Value> = Vec::with_capacity(rows.len());
        for row in &rows {
            if summary.is_null()
                && let Some(i) = summary_at
                && let Some(Some(Term::Literal(l))) = row.get(i)
            {
                summary = serde_json::from_str(l.value()).unwrap_or(Value::Null);
            }
            let cells = row
                .iter()
                .enumerate()
                .filter(|(i, _)| Some(*i) != summary_at)
                .map(|(_, t)| cell(t, &mut terms))
                .collect();
            table.push(Value::Array(cells));
        }
        Ok(Outcome::Structured(json!({
            "dataset": ds.name,
            "commit": snap.commit,
            "algorithm": a.algorithm,
            "columns": columns,
            "rows": table,
            "limited": table.len() as u64 == limit,
            "summary": summary,
            "prefixes": terms.used(),
        })))
    }
}
