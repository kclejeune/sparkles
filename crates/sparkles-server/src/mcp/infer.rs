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
    max_missing: Option<u64>,
    max_hypotheses: Option<u64>,
    source: Option<String>,
    profile: Option<String>,
    timeout_seconds: Option<f64>,
}

impl Tools<'_> {
    pub(super) fn explain_inference(&self, args: Map<String, Value>) -> Result<Outcome, ToolError> {
        let a: InferArgs = parse(args)?;
        let timeout = self.timeout(a.timeout_seconds)?;
        let ds = self.dataset(a.dataset.as_deref())?;
        let prefix_map = dataset_prefixes(&ds);
        let prefixes: Vec<(String, String)> = prefix_map
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        let triple = ex::triple_from_text(&a.triple, &prefixes)
            .map_err(|e| ToolError::bad_argument(e.to_string()))?;
        let term = ex::term_json;
        let request = json!({"triple":{"s":term(&triple[0]),"p":term(&triple[1]),"o":term(&triple[2])},
            "mode":a.mode.as_deref().unwrap_or("why"), "source":a.source,"profile":a.profile,
            "maxTrees":a.max_trees,"maxDepth":a.max_depth,"maxMissing":a.max_missing,"maxHypotheses":a.max_hypotheses});
        let (mut req, _, _) =
            ex::request_from_json(&request).map_err(|e| ToolError::bad_argument(e.to_string()))?;
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
            Explained::Partial { tree, .. } => json!([ex::summary(tree, &prefixes)]),
            Explained::NotFound => json!([]),
        };
        if let Some(sparkles_reasoner::whynot::WhyNot::NotEntailed { hypotheses, .. }) = &e.why_not
        {
            out["summaries"] = hypotheses
                .iter()
                .map(|h| Value::String(ex::summary(&h.tree, &prefixes)))
                .collect();
        }
        if req.mode == ex::ExplainMode::Why && matches!(e.explained, Explained::NotFound) {
            out["message"] = "no derivation from facts you can read was found".into();
        }
        out["prefixes"] = json!(prefix_map);
        Ok(Outcome::Structured(out))
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct DiffArgs {
    dataset: Option<String>,
    from: String,
    to: Option<String>,
    profile: Option<String>,
    rules: Option<String>,
    inputs: Option<Value>,
    sample_limit: Option<usize>,
    timeout_seconds: Option<f64>,
}

impl Tools<'_> {
    pub(super) fn entailment_diff(&self, args: Map<String, Value>) -> Result<Outcome, ToolError> {
        let args: DiffArgs = parse(args)?;
        if args
            .sample_limit
            .is_some_and(|limit| limit == 0 || limit > 100)
        {
            return Err(ToolError::bad_argument("sampleLimit must be from 1 to 100"));
        }
        let dataset = self.dataset(args.dataset.as_deref())?;
        let value = json!({"from":args.from,"to":args.to,"profile":args.profile,"rules":args.rules,"inputs":args.inputs,"sampleLimit":args.sample_limit});
        let mut request = sparkles::reasoning::diff::DiffRequest::from_json(&value)
            .map_err(|error| ToolError::bad_argument(error.to_string()))?;
        request.analysis.timeout = Some(self.timeout(args.timeout_seconds)?);
        request.analysis.max_memory = self
            .cfg()
            .query_memory_bytes
            .map(|limit| usize::try_from(limit).unwrap_or(usize::MAX));
        request.analysis.max_inferred = Some(self.server.state.reason_max_inferred);
        request.analysis.cancel = Some(self.call.cancel.clone());
        let view = |selector: &str| {
            let branch = if selector.parse::<sparkles::history::At>().is_ok() {
                dataset.store.branch_name()
            } else {
                selector
                    .rsplit_once('@')
                    .map_or(selector, |(branch, _)| branch)
                    .to_string()
            };
            let name = crate::auth::on_branch(&dataset.name, &branch);
            if !self.call.principal.can_at(
                &name,
                crate::auth::Endpoint::Query,
                crate::auth::Level::Read,
            ) {
                return Err(ToolError::new(
                    "not-permitted",
                    403,
                    format!("query access to branch {branch} is required"),
                ));
            }
            Ok(self
                .call
                .principal
                .view(&name, crate::auth::Endpoint::Query))
        };
        request.from_access = view(&request.from)?;
        request.to_access = view(&request.to)?;
        let result = dataset
            .dataset
            .reasoning()
            .diff(&request)
            .map_err(|error| {
                self.ctx(&[], request.analysis.timeout.unwrap().as_secs_f64())
                    .engine(error)
            })?;
        let mut value = result.to_json();
        value["dataset"] = dataset.name.clone().into();
        Ok(Outcome::Structured(value))
    }
}
