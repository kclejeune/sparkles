//! `explain_inference`: why a dataset entails a triple, as derivation trees over the
//! materialized inferences or what RDFS on read answers (spec U10 §3.3, F18 §2.4), read
//! through the caller's view.

use super::Outcome;
use super::errors::ToolError;
use super::tools::{Tools, dataset_prefixes, parse};
use serde::Deserialize;
use serde_json::{Map, Value, json};
use sparkles::explain::Explained;
use sparkles::reasoning::explain as ex;

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct InferArgs {
    dataset: Option<String>,
    triple: String,
    mode: Option<String>,
    max_trees: Option<u64>,
    max_depth: Option<u64>,
    timeout_seconds: Option<f64>,
}

impl Tools<'_> {
    pub(super) fn explain_inference(&self, args: Map<String, Value>) -> Result<Outcome, ToolError> {
        let a: InferArgs = parse(args)?;
        match a.mode.as_deref() {
            None | Some("why") => {}
            Some("whyNot") => {
                return Err(ToolError::new(
                    "not-implemented",
                    501,
                    "whyNot explanations are not available: only mode \"why\" is",
                ));
            }
            Some(m) => {
                return Err(ToolError::bad_argument(format!(
                    "mode must be \"why\" or \"whyNot\", not '{m}'"
                )));
            }
        }
        let timeout = self.timeout(a.timeout_seconds)?;
        let ds = self.dataset(a.dataset.as_deref())?;
        let prefix_map = dataset_prefixes(&ds);
        let prefixes: Vec<(String, String)> = prefix_map
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        let triple = ex::triple_from_text(&a.triple, &prefixes)
            .map_err(|e| ToolError::bad_argument(e.to_string()))?;
        let mut req = ex::ExplainRequest::new(triple);
        if let Some(n) = a.max_trees {
            if !(1..=ex::MAX_TREES as u64).contains(&n) {
                return Err(ToolError::bad_argument(format!(
                    "maxTrees must be from 1 to {}",
                    ex::MAX_TREES
                )));
            }
            req.max_trees = n as usize;
        }
        if let Some(n) = a.max_depth {
            if !(1..=ex::MAX_DEPTH as u64).contains(&n) {
                return Err(ToolError::bad_argument(format!(
                    "maxDepth must be from 1 to {}",
                    ex::MAX_DEPTH
                )));
            }
            req.max_depth = n as u32;
        }
        req.timeout = Some(timeout);
        // the search and its answer are held to a query's memory budget
        req.max_memory = self
            .server
            .state
            .limits
            .query_memory_bytes
            .map(|m| usize::try_from(m).unwrap_or(usize::MAX));
        // the caller's view, as a query of the caller reads it
        if !self.call.principal.can_at(
            &ds.name,
            crate::auth::Endpoint::Query,
            crate::auth::Level::Read,
        ) {
            return Err(ToolError::new(
                "not-permitted",
                403,
                format!("the query endpoint of /{} is not allowed", ds.name),
            ));
        }
        req.access = self
            .call
            .principal
            .view(&ds.name, crate::auth::Endpoint::Query);
        req.cancel = Some(self.call.cancel.clone());
        let e = match ds.dataset.reasoning().explain(&req) {
            Ok(e) => e,
            Err(sparkles::Error::Component(c)) if c.code == ex::REASONING_BUDGET => {
                return Err(ToolError::new("reasoning-budget", 422, c.message)
                    .hint("lower maxTrees, or explain a premise of the triple on its own"));
            }
            Err(e) => return Err(ToolError::new("internal", 500, e.to_string())),
        };
        let mut out = ex::explanation_json(&e);
        out["dataset"] = ds.name.clone().into();
        out["summaries"] = match &e.explained {
            Explained::Found { trees, .. } => trees
                .iter()
                .map(|t| Value::String(ex::summary(t, &prefixes)))
                .collect(),
            Explained::NotFound => json!([]),
        };
        if let Explained::NotFound = e.explained {
            out["message"] = "no derivation from facts you can read was found".into();
        }
        out["prefixes"] = json!(prefix_map);
        Ok(Outcome::Structured(out))
    }
}
