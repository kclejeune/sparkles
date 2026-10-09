//! `similar_queries` (C17 §5.3): the stored queries the caller may run, ranked by their
//! similarity to a question.

use super::text::{Bm25, cosine, ranks, words};
use super::{RRF_K, local_name, score_json};
use crate::auth::{Endpoint, Level};
use crate::mcp::Outcome;
use crate::mcp::errors::ToolError;
use crate::mcp::tools::{Tools, bounded, dataset_prefixes, parse};
use serde::Deserialize;
use serde_json::{Map, Value, json};
use sparkles::stored::{Definition, ParamType};
use std::collections::BTreeMap;

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct SimilarQueriesArgs {
    dataset: Option<String>,
    question: String,
    k: Option<u64>,
    with_text: Option<bool>,
    embedding_index: Option<String>,
    timeout_seconds: Option<f64>,
}

/// The text a stored query is ranked by: its description, its parameters' names and
/// descriptions, the local names of the IRIs of its query, and its example questions.
fn document(def: &Definition, prefixes: &BTreeMap<String, String>) -> String {
    let mut d = String::new();
    if let Some(desc) = &def.description {
        d.push_str(desc);
        d.push('\n');
    }
    for (name, p) in &def.parameters {
        d.push_str(name);
        d.push(' ');
        if let Some(desc) = &p.description {
            d.push_str(desc);
        }
        d.push('\n');
    }
    let pv: Vec<(String, String)> = prefixes
        .iter()
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    if let Ok(q) = sparkles::sparql::parse_query(&def.query, None, &pv) {
        let mut consts = Vec::new();
        crate::mcp::tools::constant_terms(crate::mcp::tools::pattern(&q), &mut consts);
        let mut names: Vec<String> = consts
            .iter()
            .filter_map(|t| match t {
                oxrdf::Term::NamedNode(n) => Some(words(local_name(n.as_str())).join(" ")),
                _ => None,
            })
            .collect();
        names.sort();
        names.dedup();
        d.push_str(&names.join(" "));
        d.push('\n');
    }
    for q in &def.questions {
        d.push_str(q);
        d.push('\n');
    }
    d
}

impl Tools<'_> {
    pub(crate) fn similar_queries(&self, args: Map<String, Value>) -> Result<Outcome, ToolError> {
        let a: SimilarQueriesArgs = parse(args)?;
        if a.question.trim().is_empty() {
            return Err(ToolError::bad_argument("question must not be empty"));
        }
        if a.question.chars().count() > 2000 {
            return Err(ToolError::bad_argument(
                "question must be at most 2000 characters",
            ));
        }
        let k = bounded("k", a.k, 5, 1, 20)? as usize;
        let with_text = a.with_text.unwrap_or(true);
        let timeout = self.timeout(a.timeout_seconds)?;
        let ds = self.dataset(a.dataset.as_deref())?;
        let prefixes = dataset_prefixes(&ds);
        let p = &self.call.principal;
        // the stored queries the caller may run, as the stored-query tools offer them
        let runnable = p.can_at(&ds.name, Endpoint::Query, Level::Read);
        let mut candidates: Vec<(String, sparkles::stored::Stored)> = if runnable {
            ds.dataset
                .queries()
                .list()
                .into_iter()
                .filter(|(_, s)| s.definition.mcp && s.definition.check().is_ok())
                .collect()
        } else {
            Vec::new()
        };
        candidates.sort_by(|a, b| a.0.cmp(&b.0));
        // the tool names this caller sees (none with --no-stored-queries)
        let tools: BTreeMap<String, String> = self
            .server
            .stored_tools(p)
            .into_iter()
            .filter(|t| t.dataset.name == ds.name)
            .map(|t| (t.query, t.name))
            .collect();
        let docs: Vec<String> = candidates
            .iter()
            .map(|(_, s)| document(&s.definition, &prefixes))
            .collect();
        let text = Bm25::new(&docs).scores(&a.question);
        let text_ranks = ranks(&text);
        // the vector list, through the embedding index's provider
        let index = self.embedding_index(&ds, a.embedding_index.as_deref())?;
        let mut vector_ranks: Vec<Option<usize>> = vec![None; docs.len()];
        let mut hybrid = false;
        if let Some(predicate) = &index
            && !docs.is_empty()
        {
            let deadline = self.call.arrived + timeout;
            let snap = self.snapshot(&ds, None, None, deadline)?;
            let question = sparkles::store::embed_texts(&snap, predicate, &[&a.question], true);
            let doc_refs: Vec<&str> = docs.iter().map(String::as_str).collect();
            let stored = sparkles::store::embed_texts(&snap, predicate, &doc_refs, false);
            match (question, stored) {
                (Ok(q), Ok(d)) if q.len() == 1 && d.len() == docs.len() => {
                    let sims: Vec<f64> = d.iter().map(|v| cosine(&q[0], v)).collect();
                    vector_ranks = ranks(&sims);
                    hybrid = true;
                }
                (Err(e), _) | (_, Err(e)) => {
                    tracing::info!(
                        dataset = ds.name,
                        "similar_queries ranks by text alone: the embedding failed: {e}"
                    );
                }
                _ => {}
            }
        }
        let mut ranked: Vec<(usize, f64)> = (0..docs.len())
            .filter_map(|i| {
                let score = text_ranks[i].map_or(0.0, |r| 1.0 / (RRF_K + r as f64))
                    + vector_ranks[i].map_or(0.0, |r| 1.0 / (RRF_K + r as f64));
                (score > 0.0).then_some((i, score))
            })
            .collect();
        ranked.sort_by(|a, b| {
            b.1.total_cmp(&a.1)
                .then_with(|| candidates[a.0].0.cmp(&candidates[b.0].0))
        });
        let queries: Vec<Value> = ranked
            .iter()
            .take(k)
            .map(|&(i, score)| {
                let (name, stored) = &candidates[i];
                let def = &stored.definition;
                let mut matched = Vec::new();
                if text_ranks[i].is_some() {
                    matched.push("text");
                }
                if vector_ranks[i].is_some() {
                    matched.push("vector");
                }
                let parameters: Vec<Value> = def
                    .parameters
                    .iter()
                    .map(|(n, p)| {
                        let mut j = json!({
                            "name": n,
                            "type": param_type(p.kind),
                            "required": p.is_required(),
                        });
                        if let Some(d) = &p.description {
                            j["description"] = d.clone().into();
                        }
                        j
                    })
                    .collect();
                let mut j = json!({
                    "name": name,
                    "score": score_json(score),
                    "matchedBy": matched,
                    "parameters": parameters,
                });
                if let Some(t) = tools.get(name) {
                    j["tool"] = t.clone().into();
                }
                if let Some(d) = &def.description {
                    j["description"] = d.clone().into();
                }
                if !def.questions.is_empty() {
                    j["questions"] = def.questions.clone().into();
                }
                if with_text {
                    j["query"] = def.query.clone().into();
                }
                j
            })
            .collect();
        // the dataset prefixes the returned query texts use
        let used: BTreeMap<String, String> = prefixes
            .iter()
            .filter(|(name, _)| {
                with_text
                    && ranked.iter().take(k).any(|&(i, _)| {
                        candidates[i]
                            .1
                            .definition
                            .query
                            .contains(&format!("{name}:"))
                    })
            })
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        Ok(Outcome::Structured(json!({
            "dataset": ds.name,
            "queries": queries,
            "ranking": if hybrid { "hybrid" } else { "text" },
            "prefixes": used,
        })))
    }

    /// The predicate of the vector index whose provider embeds texts: the one named, or
    /// the dataset's only index with an `embedding` configuration. `None` when there is
    /// no such index, or several and none is named.
    pub(crate) fn embedding_index(
        &self,
        ds: &crate::state::Dataset,
        name: Option<&str>,
    ) -> Result<Option<String>, ToolError> {
        let indexes = ds.dataset.indexes().vector().list();
        let with: Vec<_> = indexes
            .iter()
            .filter(|s| s.embedding.as_ref().is_some_and(|e| e.config.query_text))
            .collect();
        match name {
            Some(n) => {
                let Some(s) = indexes.iter().find(|s| s.name == n) else {
                    return Err(ToolError::bad_argument(format!(
                        "dataset {} has no vector index '{n}'",
                        ds.name
                    )));
                };
                if !with.iter().any(|w| w.name == s.name) {
                    return Err(ToolError::bad_argument(format!(
                        "the vector index '{n}' has no embedding provider that embeds query text"
                    )));
                }
                Ok(Some(s.predicate.clone()))
            }
            None if with.len() == 1 => Ok(Some(with[0].predicate.clone())),
            None => Ok(None),
        }
    }
}

fn param_type(t: ParamType) -> &'static str {
    t.name()
}
