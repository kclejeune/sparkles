//! `search_semantic` (C11, item 490): the query text embedded with the provider of a
//! vector index and ranked by `spk:vectorSearch`, fused by reciprocal rank with the
//! full-text ranking of its words when the dataset has a full-text index, as
//! `spk:hybridSearch` and `recall` fuse them. Both rankings read every graph of the
//! caller's view.

use super::Outcome;
use super::errors::ToolError;
use super::memory::Reader;
use super::render::{Prefixes, Terms};
use super::tools::{
    Tools, bounded, dataset_prefixes, label_of, label_rank, label_values, parse, parse_iri,
};
use oxrdf::{NamedNode, Term};
use serde::Deserialize;
use serde_json::{Map, Value, json};
use sparkles::error::Error;
use std::collections::{BTreeSet, HashMap};

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct SemanticArgs {
    dataset: Option<String>,
    query: String,
    predicate: Option<String>,
    limit: Option<u64>,
    with_labels: Option<bool>,
    reasoning: Option<bool>,
    at_commit: Option<u64>,
    at: Option<Value>,
}

/// Each subject's best score and its rank: one more than the number of subjects with a
/// better score, so ties share a rank, as `spk:hybridSearch` ranks.
fn ranks(hits: impl Iterator<Item = (NamedNode, f64)>) -> HashMap<String, usize> {
    let mut best: HashMap<String, f64> = HashMap::new();
    for (s, score) in hits {
        let e = best.entry(s.as_str().to_string()).or_insert(score);
        if score > *e {
            *e = score;
        }
    }
    let mut scores: Vec<f64> = best.values().copied().collect();
    scores.sort_by(|a, b| b.total_cmp(a));
    best.iter()
        .map(|(s, v)| (s.clone(), 1 + scores.partition_point(|o| o > v)))
        .collect()
}

/// The label candidates of `iris` in every graph of the reader's view.
fn labels_in(r: &Reader, iris: &[NamedNode]) -> Result<super::tools::Labels, Error> {
    let mut out = super::tools::Labels::new();
    let unique: BTreeSet<&str> = iris.iter().map(|n| n.as_str()).collect();
    if unique.is_empty() {
        return Ok(out);
    }
    let values: Vec<String> = unique
        .iter()
        .map(|i| NamedNode::new_unchecked(*i).to_string())
        .collect();
    let q = format!(
        "SELECT ?x ?lp ?l WHERE {{ VALUES ?lp {{ {} }} VALUES ?x {{ {} }} {} }} LIMIT 10000",
        label_values(),
        values.join(" "),
        r.quads("?x ?lp ?l", &[])
    );
    for row in r.rows(&q, Vec::new())? {
        if let [
            Some(Term::NamedNode(x)),
            Some(p),
            Some(Term::Literal(l)),
            ..,
        ] = row.as_slice()
            && let Some(rank) = label_rank(p)
        {
            out.entry(x.as_str().to_string())
                .or_default()
                .push((rank, l.clone()));
        }
    }
    Ok(out)
}

impl Tools<'_> {
    pub(super) fn search_semantic(&self, args: Map<String, Value>) -> Result<Outcome, ToolError> {
        let a: SemanticArgs = parse(args)?;
        if a.query.trim().is_empty() {
            return Err(ToolError::bad_argument("query must not be empty"));
        }
        if a.query.chars().count() > 1000 {
            return Err(ToolError::bad_argument(
                "query must be at most 1000 characters",
            ));
        }
        let limit = bounded("limit", a.limit, 10, 1, 100)? as usize;
        let ds = self.dataset(a.dataset.as_deref())?;
        let prefix_map = dataset_prefixes(&ds);
        let prefixes = Prefixes::new(&prefix_map);
        let no_embedder = |m: String| {
            ToolError::new("no-embedder", 400, m).hint(
                "use search_text for keyword search, or similar_entities with a vector; an administrator gives a vector index an embedding provider (PUT /$/vector/{ds}/{name} with an embedding object)",
            )
        };
        let wanted = match &a.predicate {
            Some(p) => match parse_iri(p, &prefix_map, false)? {
                Term::NamedNode(n) => Some(n),
                _ => return Err(ToolError::bad_argument("predicate must be an IRI")),
            },
            None => None,
        };
        let names = prefixes.names();
        let timeout = self.cfg().default_timeout();
        let ctx = self.ctx(&names, timeout.as_secs_f64());
        let deadline = self.call.arrived + timeout;
        let r = self.reader(&ds, a.at_commit, a.at.as_ref(), a.reasoning, deadline, &ctx)?;
        // the first index by name whose provider embeds query text and whose vectors
        // the caller can read: an index on a predicate the caller's view hides is
        // treated as absent, so that its name does not leak
        let mut indexes = ds.dataset.indexes().vector().list();
        indexes.sort_by(|a, b| a.name.cmp(&b.name));
        let mut pred = None;
        for s in &indexes {
            if !s.embedding.as_ref().is_some_and(|e| e.config.query_text)
                || wanted.as_ref().is_some_and(|w| w.as_str() != s.predicate)
            {
                continue;
            }
            let p = NamedNode::new_unchecked(s.predicate.as_str());
            let q = format!("ASK {{ {} }}", r.quads(&format!("?s {p} ?v"), &[]));
            if r.ask(&q, Vec::new()).map_err(|e| ctx.engine(e))? {
                pred = Some(p);
                break;
            }
        }
        let Some(pred) = pred else {
            let mut terms = Terms::new(&prefixes, 200);
            return Err(no_embedder(match &wanted {
                Some(w) => format!(
                    "{} has no vector index in dataset {} whose embedding provider embeds query text and whose vectors you can read",
                    terms.iri(w.as_str()),
                    ds.name
                ),
                None => format!(
                    "dataset {} has no vector index whose embedding provider embeds query text and whose vectors you can read, so search_semantic cannot embed the query",
                    ds.name
                ),
            }));
        };
        // the vector ranking: the provider's failure is the call's error
        let vector = self
            .vector_hits(&r, &pred, &a.query, &[])
            .map_err(|e| match e {
                Error::Service(m) => ToolError::new(
                    "embedding-failed",
                    502,
                    format!("the embedding provider failed: {m}"),
                )
                .hint("try again later, or use search_text for keyword search"),
                e => ctx.engine(e),
            })?;
        let vector = ranks(vector.into_iter());
        // the full-text ranking of the query's words, when the dataset has the index
        let text = if cfg!(feature = "text") && ds.dataset.indexes().text().enabled() {
            let preds: Vec<NamedNode> = match ds
                .dataset
                .indexes()
                .text()
                .status()
                .map(|s| s.config.predicates)
            {
                Some(sparkles::text::PredicateSet::Only(p)) => p
                    .iter()
                    .map(|p| NamedNode::new_unchecked(p.as_str()))
                    .collect(),
                _ => Vec::new(),
            };
            match super::memory::text::plain_query(&a.query) {
                Some(words) => match self.text_hits(&r, &preds, &words, &[]) {
                    Ok(hits) => Some(ranks(hits.into_iter().map(|(s, score, _)| (s, score)))),
                    // the index is not ready: the vector ranking alone
                    Err(Error::TextUnavailable(_)) => None,
                    Err(e) => return Err(ctx.engine(e)),
                },
                None => None,
            }
        } else {
            None
        };
        let ranking = if text.is_some() { "hybrid" } else { "vector" };
        let lists = std::iter::once(&vector).chain(text.as_ref());
        let mut fused: Vec<(String, f64)> = sparkles::rank::fuse_rrf(
            lists.map(|l| (1.0, l.iter().map(|(s, rank)| (s.clone(), *rank)))),
            super::memory::RRF_K,
        );
        fused.truncate(limit);
        let iris: Vec<NamedNode> = fused
            .iter()
            .map(|(s, _)| NamedNode::new_unchecked(s.as_str()))
            .collect();
        let labels = if a.with_labels.unwrap_or(true) {
            labels_in(&r, &iris).map_err(|e| ctx.engine(e))?
        } else {
            Default::default()
        };
        let mut terms = Terms::new(&prefixes, 500);
        let predicate = terms.iri(pred.as_str());
        let hits: Vec<Value> = fused
            .iter()
            .zip(&iris)
            .map(|((s, score), iri)| {
                let t = Term::NamedNode(iri.clone());
                let mut h =
                    json!({ "iri": terms.term(&t), "score": super::search::score_json(*score) });
                if let Some(rank) = text.as_ref().and_then(|l| l.get(s)) {
                    h["textRank"] = (*rank as u64).into();
                }
                if let Some(rank) = vector.get(s) {
                    h["vectorRank"] = (*rank as u64).into();
                }
                if let Some(l) = label_of(&labels, &t, "en") {
                    h["label"] = l.into();
                }
                h
            })
            .collect();
        Ok(Outcome::Structured(json!({
            "dataset": ds.name,
            "commit": r.snap.commit,
            "predicate": predicate,
            "ranking": ranking,
            "hits": hits,
            "prefixes": terms.used(),
        })))
    }
}
