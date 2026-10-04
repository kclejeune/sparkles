//! The search tools: `search_text` (BM25 over the full-text index, `text:query`) and
//! `similar_entities` (exact nearest neighbours over stored `spk:vector` literals,
//! `spk:vectorSearch`). Neither creates anything: they read what the dataset holds.

use super::Outcome;
use super::errors::ToolError;
use super::render::{Prefixes, Terms};
use super::tools::{
    Tools, bounded, dataset_prefixes, label_of, labels_of, parse, parse_iri, remaining,
};
use oxrdf::{Literal, NamedNode, Term};
use serde::Deserialize;
use serde_json::{Map, Value, json};
use sparkles::error::Error;
use sparkles::sparql;
use std::collections::BTreeMap;
#[cfg(feature = "text")]
use std::collections::HashMap;
#[cfg(feature = "text")]
use std::time::Instant;

/// Matched text is cut to this many characters.
#[cfg(feature = "text")]
const TEXT_CHARS: usize = 300;

#[cfg(feature = "text")]
#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct SearchTextArgs {
    dataset: Option<String>,
    query: String,
    predicates: Option<Vec<String>>,
    lang: Option<String>,
    limit: Option<u64>,
    with_types: Option<bool>,
    reasoning: Option<bool>,
    at_commit: Option<u64>,
    at: Option<Value>,
}

#[derive(Deserialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
enum Metric {
    Cosine,
    Dot,
    Euclidean,
}

impl Metric {
    fn name(self) -> &'static str {
        match self {
            Metric::Cosine => "cosine",
            Metric::Dot => "dot",
            Metric::Euclidean => "euclidean",
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct SimilarArgs {
    dataset: Option<String>,
    predicate: String,
    entity: Option<String>,
    vector: Option<Vec<f64>>,
    k: Option<u64>,
    metric: Option<Metric>,
    exclude_self: Option<bool>,
    with_labels: Option<bool>,
    reasoning: Option<bool>,
    at_commit: Option<u64>,
    at: Option<Value>,
}

/// `[A-Za-z]{1,8}(-[A-Za-z0-9]{1,8})*`
#[cfg(feature = "text")]
fn valid_lang(s: &str) -> bool {
    let mut parts = s.split('-');
    let first = parts.next().unwrap_or_default();
    (1..=8).contains(&first.len())
        && first.bytes().all(|c| c.is_ascii_alphabetic())
        && parts.all(|p| (1..=8).contains(&p.len()) && p.bytes().all(|c| c.is_ascii_alphanumeric()))
}

fn score_of(t: Option<&Term>) -> f64 {
    match t {
        Some(Term::Literal(l)) => l.value().parse::<f64>().unwrap_or(0.0),
        _ => 0.0,
    }
}

/// A JSON number (never NaN or infinite), rounded to 6 significant decimals.
fn score_json(x: f64) -> Value {
    if !x.is_finite() {
        return Value::Null;
    }
    let r = (x * 1e6).round() / 1e6;
    serde_json::Number::from_f64(r).map_or(Value::Null, Value::Number)
}

impl Tools<'_> {
    #[cfg(feature = "text")]
    pub(super) fn search_text(&self, args: Map<String, Value>) -> Result<Outcome, ToolError> {
        let a: SearchTextArgs = parse(args)?;
        if a.query.trim().is_empty() {
            return Err(ToolError::bad_argument("query must not be empty"));
        }
        if a.query.chars().count() > 1000 {
            return Err(ToolError::bad_argument(
                "query must be at most 1000 characters",
            ));
        }
        let limit = bounded("limit", a.limit, 20, 1, 200)?;
        if let Some(l) = &a.lang
            && !valid_lang(l)
        {
            return Err(ToolError::bad_argument(format!(
                "invalid language tag '{l}'"
            )));
        }
        let predicates = a.predicates.unwrap_or_default();
        if predicates.len() > 20 {
            return Err(ToolError::bad_argument("at most 20 predicates"));
        }
        let ds = self.dataset(a.dataset.as_deref())?;
        let prefix_map = dataset_prefixes(&ds);
        let preds = predicates
            .iter()
            .map(|p| match parse_iri(p, &prefix_map, false)? {
                Term::NamedNode(n) => Ok(n),
                _ => Err(ToolError::bad_argument("predicates must be IRIs")),
            })
            .collect::<Result<Vec<NamedNode>, _>>()?;
        if !ds.dataset.indexes().text().enabled() {
            return Err(ToolError::new(
                "text-disabled",
                400,
                format!("dataset {} has no full-text index", ds.name),
            )
            .hint(format!(
                "use sparql_query with FILTER(CONTAINS(LCASE(?x), \"…\")) on a narrow pattern; an administrator can enable full-text search with `sparkles text-index` or PUT /$/text/{}",
                ds.name
            )));
        }
        let prefixes = Prefixes::new(&prefix_map);
        let names = prefixes.names();
        let timeout = self.cfg().default_timeout();
        let ctx = self.ctx(&names, timeout.as_secs_f64());
        let snap = self.snapshot(&ds, a.at_commit, a.at.as_ref(), self.call.arrived + timeout)?;
        let reasoning = Self::reasoning(&ds, a.reasoning);
        let deadline = self.call.arrived + timeout;
        let opts = self
            .query_options(
                &ds.name,
                crate::auth::Endpoint::Query,
                reasoning,
                deadline,
                &BTreeMap::new(),
            )
            .map_err(|e| ctx.engine(e))?;
        // the query string and options become literals serialized by oxrdf (escaped),
        // the predicates validated IRIs
        let mut call_args: Vec<String> = preds.iter().map(NamedNode::to_string).collect();
        call_args.push(Literal::new_simple_literal(a.query.as_str()).to_string());
        call_args.push(limit.to_string());
        if let Some(l) = &a.lang {
            call_args.push(Literal::new_simple_literal(format!("lang:{l}")).to_string());
        }
        let q = format!(
            "SELECT ?s ?score ?lit ?p WHERE {{ (?s ?score ?lit ?g ?p) <http://jena.apache.org/text#query> ({}) }} ORDER BY DESC(?score)",
            call_args.join(" ")
        );
        let rows = sparql::query(snap.clone(), &q, &opts)
            .map_err(|e| match e {
                Error::Invalid(m) => ToolError::bad_argument(m),
                e => ctx.engine(e),
            })?
            .rows();
        let subjects: Vec<&NamedNode> = rows
            .iter()
            .filter_map(|r| match r.first() {
                Some(Some(Term::NamedNode(n))) => Some(n),
                _ => None,
            })
            .collect();
        let labels = labels_of(&snap, &opts, deadline, &subjects).map_err(|e| ctx.engine(e))?;
        let types = if a.with_types.unwrap_or(true) {
            types_of(&snap, &opts, deadline, &subjects).map_err(|e| ctx.engine(e))?
        } else {
            HashMap::new()
        };
        let mut terms = Terms::new(&prefixes, 500);
        let hits: Vec<Value> = rows
            .iter()
            .filter_map(|r| {
                let s = r.first()?.as_ref()?;
                let mut h = json!({ "s": terms.term(s), "score": score_json(score_of(r.get(1)?.as_ref())) });
                if let Some(Some(Term::Literal(l))) = r.get(2) {
                    h["text"] = cut_text(l.value()).into();
                }
                if let Some(Some(Term::NamedNode(p))) = r.get(3) {
                    h["p"] = terms.iri(p.as_str()).into();
                }
                if let Some(l) = label_of(&labels, s, "en") {
                    h["label"] = l.into();
                }
                if let Term::NamedNode(n) = s
                    && let Some(t) = types.get(n.as_str())
                {
                    h["types"] = t.iter().map(|t| terms.term(t)).collect::<Vec<_>>().into();
                }
                Some(h)
            })
            .collect();
        Ok(Outcome::Structured(json!({
            "dataset": ds.name,
            "commit": snap.commit,
            "limited": hits.len() as u64 == limit,
            "hits": hits,
            "prefixes": terms.used(),
        })))
    }

    pub(super) fn similar_entities(&self, args: Map<String, Value>) -> Result<Outcome, ToolError> {
        let a: SimilarArgs = parse(args)?;
        let k = bounded("k", a.k, 10, 1, 100)? as usize;
        let metric = a.metric.unwrap_or(Metric::Cosine);
        let ds = self.dataset(a.dataset.as_deref())?;
        let prefix_map = dataset_prefixes(&ds);
        let Term::NamedNode(pred) = parse_iri(&a.predicate, &prefix_map, false)? else {
            return Err(ToolError::bad_argument("predicate must be an IRI"));
        };
        let query_term = match (&a.entity, &a.vector) {
            (Some(e), None) => match parse_iri(e, &prefix_map, false)? {
                Term::NamedNode(n) => n.to_string(),
                _ => return Err(ToolError::bad_argument("entity must be an IRI")),
            },
            (None, Some(v)) => {
                if v.is_empty() || v.len() > sparkles::vector::MAX_DIM {
                    return Err(ToolError::bad_argument(format!(
                        "vector must have 1 to {} numbers",
                        sparkles::vector::MAX_DIM
                    )));
                }
                if v.iter().any(|x| !x.is_finite()) {
                    return Err(ToolError::bad_argument("vector numbers must be finite"));
                }
                let lex = serde_json::to_string(v).unwrap_or_default();
                Literal::new_typed_literal(
                    lex,
                    NamedNode::new_unchecked(sparkles::vector::DATATYPE),
                )
                .to_string()
            }
            _ => {
                return Err(ToolError::bad_argument(
                    "give exactly one of entity (an IRI with a stored vector) and vector (numbers)",
                ));
            }
        };
        let entity = a
            .entity
            .as_deref()
            .map(|e| parse_iri(e, &prefix_map, false))
            .transpose()?;
        let exclude_self = a.exclude_self.unwrap_or(true) && entity.is_some();
        let prefixes = Prefixes::new(&prefix_map);
        let names = prefixes.names();
        let timeout = self.cfg().default_timeout();
        let ctx = self.ctx(&names, timeout.as_secs_f64());
        let no_vectors = |m: String| {
            ToolError::new("no-vectors", 400, m)
                .hint("list embedding predicates with describe_schema (vector=true)")
        };
        let snap = self.snapshot(&ds, a.at_commit, a.at.as_ref(), self.call.arrived + timeout)?;
        let reasoning = Self::reasoning(&ds, a.reasoning);
        let deadline = self.call.arrived + timeout;
        let opts = self
            .query_options(
                &ds.name,
                crate::auth::Endpoint::Query,
                reasoning,
                deadline,
                &BTreeMap::new(),
            )
            .map_err(|e| ctx.engine(e))?;
        let fetch = k + usize::from(exclude_self);
        let order = if metric == Metric::Euclidean {
            "ASC"
        } else {
            "DESC"
        };
        let q = format!(
            "SELECT ?s ?score WHERE {{ (?s ?score) <{}> ({pred} {query_term} {fetch} \"metric:{}\") }} ORDER BY {order}(?score)",
            sparkles::vector::VECTOR_SEARCH,
            metric.name()
        );
        let rows = sparql::query(snap.clone(), &q, &opts)
            .map_err(|e| match e {
                Error::Invalid(m) => no_vectors(m),
                e => ctx.engine(e),
            })?
            .rows();
        if rows.is_empty() {
            // say why nothing matched
            let mut opts = opts.clone();
            opts.timeout = Some(remaining(deadline).map_err(|e| ctx.engine(e))?);
            opts.initial_bindings = vec![("pp".to_string(), Term::NamedNode(pred.clone()))];
            let has_vectors = sparql::query(
                snap.clone(),
                &format!(
                    "ASK {{ ?s ?pp ?v FILTER(DATATYPE(?v) = <{}>) }}",
                    sparkles::vector::DATATYPE
                ),
                &opts,
            )
            .map_err(|e| ctx.engine(e))?
            .boolean;
            let mut terms = Terms::new(&prefixes, 200);
            if !has_vectors {
                return Err(no_vectors(format!(
                    "{} has no spk:vector values in dataset {}",
                    terms.iri(pred.as_str()),
                    ds.name
                )));
            }
            if let Some(e) = &entity {
                return Err(no_vectors(format!(
                    "{} has no vector under {}",
                    terms.term(e),
                    terms.iri(pred.as_str())
                )));
            }
        }
        let mut hits: Vec<(Term, f64)> = rows
            .into_iter()
            .filter_map(|r| {
                let mut r = r.into_iter();
                let s = r.next().flatten()?;
                let score = score_of(r.next().flatten().as_ref());
                Some((s, score))
            })
            .filter(|(s, _)| !(exclude_self && Some(s) == entity.as_ref()))
            .collect();
        hits.truncate(k);
        let iris: Vec<&NamedNode> = hits
            .iter()
            .filter_map(|(s, _)| match s {
                Term::NamedNode(n) => Some(n),
                _ => None,
            })
            .collect();
        let labels = if a.with_labels.unwrap_or(true) {
            labels_of(&snap, &opts, deadline, &iris).map_err(|e| ctx.engine(e))?
        } else {
            Default::default()
        };
        let mut terms = Terms::new(&prefixes, 500);
        let hits: Vec<Value> = hits
            .iter()
            .map(|(s, score)| {
                let mut h = json!({ "iri": terms.term(s), "score": score_json(*score) });
                if let Some(l) = label_of(&labels, s, "en") {
                    h["label"] = l.into();
                }
                h
            })
            .collect();
        Ok(Outcome::Structured(json!({
            "dataset": ds.name,
            "commit": snap.commit,
            "metric": metric.name(),
            "higherIsBetter": metric != Metric::Euclidean,
            "hits": hits,
            "prefixes": terms.used(),
        })))
    }
}

/// Matched text: escaped like a label, cut to [`TEXT_CHARS`].
#[cfg(feature = "text")]
fn cut_text(s: &str) -> String {
    let mut out = String::new();
    match s.char_indices().nth(TEXT_CHARS) {
        None => super::render::escape_into(&mut out, s),
        Some((i, _)) => {
            super::render::escape_into(&mut out, &s[..i]);
            out.push('…');
        }
    }
    out
}

/// At most 3 `rdf:type`s of each IRI, in IRI order, in one `VALUES` query.
#[cfg(feature = "text")]
fn types_of(
    snap: &std::sync::Arc<sparkles::store::Snapshot>,
    opts: &sparql::QueryOptions,
    deadline: Instant,
    iris: &[&NamedNode],
) -> Result<HashMap<String, Vec<Term>>, Error> {
    let mut out: HashMap<String, Vec<Term>> = HashMap::new();
    let unique: std::collections::BTreeSet<&str> = iris.iter().map(|n| n.as_str()).collect();
    if unique.is_empty() {
        return Ok(out);
    }
    let values: Vec<String> = unique
        .iter()
        .map(|i| NamedNode::new_unchecked(*i).to_string())
        .collect();
    let q = format!(
        "SELECT ?x ?t WHERE {{ VALUES ?x {{ {} }} ?x a ?t }} ORDER BY ?x ?t LIMIT 10000",
        values.join(" ")
    );
    let mut opts = opts.clone();
    opts.timeout = Some(remaining(deadline)?);
    for row in sparql::query(snap.clone(), &q, &opts)?.rows() {
        if let [Some(Term::NamedNode(x)), Some(t)] = row.as_slice() {
            let v = out.entry(x.as_str().to_string()).or_default();
            if v.len() < 3 {
                v.push(t.clone());
            }
        }
    }
    Ok(out)
}
