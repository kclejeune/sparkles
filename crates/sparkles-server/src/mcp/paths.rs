//! `find_paths`: the path search of `SERVICE path:search` (F07) as a tool. The tool
//! writes the search's configuration block from its arguments, runs it as a query of
//! the caller, and returns the paths with their edges as compact terms. IRIs in the
//! arguments are validated and written as `<…>`, so nothing the caller sends is spliced
//! into the query text as syntax.

use super::Outcome;
use super::errors::ToolError;
use super::render::{Prefixes, Terms};
use super::tools::{Tools, bounded, dataset_prefixes, parse, parse_iri};
use oxrdf::Term;
use serde::Deserialize;
use serde_json::{Map, Value, json};
use sparkles::sparql;
use std::collections::BTreeMap;
use std::fmt::Write;

/// The most paths a call returns.
const MAX_PATHS: u64 = 100;

/// The most edges a call returns over all its paths.
const MAX_EDGES: usize = 2000;

/// The most predicates a search may name.
const MAX_PREDICATES: usize = 20;

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct PathArgs {
    dataset: Option<String>,
    source: Option<String>,
    target: Option<String>,
    predicates: Option<Vec<String>>,
    algorithm: Option<Algorithm>,
    direction: Option<Direction>,
    min_length: Option<u64>,
    max_length: Option<u64>,
    k: Option<u64>,
    limit: Option<u64>,
    max_visited: Option<u64>,
    weight: Option<String>,
    default_weight: Option<f64>,
    graph: Option<String>,
    reasoning: Option<bool>,
    at_commit: Option<u64>,
    at: Option<Value>,
    timeout_seconds: Option<f64>,
}

#[derive(Deserialize, Clone, Copy)]
#[serde(rename_all = "camelCase")]
enum Algorithm {
    Shortest,
    AllShortest,
    KShortest,
    All,
}

impl Algorithm {
    fn name(self) -> &'static str {
        match self {
            Algorithm::Shortest => "shortest",
            Algorithm::AllShortest => "allShortest",
            Algorithm::KShortest => "kShortest",
            Algorithm::All => "all",
        }
    }
}

#[derive(Deserialize, Clone, Copy)]
#[serde(rename_all = "lowercase")]
enum Direction {
    Forward,
    Backward,
    Both,
}

impl Direction {
    fn name(self) -> &'static str {
        match self {
            Direction::Forward => "forward",
            Direction::Backward => "backward",
            Direction::Both => "both",
        }
    }
}

/// One path of the result.
struct Path {
    source: Option<Term>,
    target: Option<Term>,
    length: Option<Term>,
    cost: Option<Term>,
    /// (subject, predicate, object)
    edges: Vec<[Option<Term>; 3]>,
}

impl Tools<'_> {
    pub(super) fn find_paths(&self, args: Map<String, Value>) -> Result<Outcome, ToolError> {
        let a: PathArgs = parse(args)?;
        let limit = bounded("limit", a.limit, 10, 1, MAX_PATHS)?;
        let timeout = self.timeout(a.timeout_seconds)?;
        let ds = self.dataset(a.dataset.as_deref())?;
        let prefix_map = dataset_prefixes(&ds);
        let prefixes = Prefixes::new(&prefix_map);
        let names = prefixes.names();
        let ctx = self.ctx(&names, timeout.as_secs_f64());
        let iri = |what: &str, s: &str| -> Result<String, ToolError> {
            match parse_iri(s, &prefix_map, false)? {
                Term::NamedNode(n) => Ok(n.to_string()),
                _ => Err(ToolError::bad_argument(format!("{what} must be an IRI"))),
            }
        };
        let source = a.source.as_deref().map(|s| iri("source", s)).transpose()?;
        let target = a.target.as_deref().map(|s| iri("target", s)).transpose()?;
        if source.is_none() && target.is_none() {
            return Err(ToolError::bad_argument(
                "source or target is required (both for a path between two nodes)",
            ));
        }
        let predicates = a.predicates.unwrap_or_default();
        if predicates.len() > MAX_PREDICATES {
            return Err(ToolError::bad_argument(format!(
                "at most {MAX_PREDICATES} predicates"
            )));
        }
        let algorithm = a.algorithm.unwrap_or(Algorithm::Shortest);
        // the configuration block
        let mut block = String::from("[] ");
        let mut param = |p: &str, v: &str| {
            let _ = write!(block, "path:{p} {v} ; ");
        };
        param("source", source.as_deref().unwrap_or("?source"));
        param("target", target.as_deref().unwrap_or("?target"));
        param("algorithm", &format!("path:{}", algorithm.name()));
        for p in &predicates {
            param("predicate", &iri("predicates", p)?);
        }
        if let Some(d) = a.direction {
            param("direction", &format!("path:{}", d.name()));
        }
        for (name, v) in [
            ("minLength", a.min_length),
            ("maxLength", a.max_length),
            ("k", a.k),
            ("maxVisited", a.max_visited),
        ] {
            if let Some(v) = v {
                param(name, &v.to_string());
            }
        }
        param("limit", &limit.to_string());
        if let Some(w) = &a.weight {
            param("weight", &iri("weight", w)?);
        }
        if let Some(w) = a.default_weight {
            if !(w.is_finite() && w >= 0.0) {
                return Err(ToolError::bad_argument(
                    "defaultWeight must be a non-negative number",
                ));
            }
            param(
                "defaultWeight",
                &format!("\"{w}\"^^<http://www.w3.org/2001/XMLSchema#double>"),
            );
        }
        block.push_str(
            "path:pathIndex ?path ; path:edgeIndex ?edge ; path:edgeSubject ?s ; path:edgePredicate ?p ; path:edgeObject ?o ; path:length ?length ; path:cost ?cost .",
        );
        let service = format!("SERVICE path:search {{ {block} }}");
        let pattern = match a.graph.as_deref().map(str::trim) {
            None | Some("default") => service,
            Some(g) => format!("GRAPH {} {{ {service} }}", iri("graph", g)?),
        };
        let query = format!(
            "PREFIX path: <urn:x-sparkles:path#>\nSELECT ?path ?edge ?s ?p ?o ?length ?cost ?source ?target WHERE {{ {pattern} }} ORDER BY ?path ?edge"
        );
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
        let rows = sparql::query(snap.clone(), &query, &opts)
            .map_err(|e| ctx.engine(e))?
            .rows();
        // rows come per edge, in path order; a path of length 0 has one row without edges
        let source_term = |s: &Option<String>| {
            s.as_ref().and_then(|s| {
                oxrdf::NamedNode::new(s.trim_start_matches('<').trim_end_matches('>'))
                    .ok()
                    .map(Term::NamedNode)
            })
        };
        let mut paths: Vec<Path> = Vec::new();
        let mut last: Option<Term> = None;
        let mut edges = 0usize;
        let mut truncated = false;
        for row in rows {
            let mut row = row.into_iter();
            let mut next = || row.next().flatten();
            let (index, _edge, s, p, o, length, cost, src, tgt) = (
                next(),
                next(),
                next(),
                next(),
                next(),
                next(),
                next(),
                next(),
                next(),
            );
            if paths.is_empty() || index != last {
                paths.push(Path {
                    source: src.or_else(|| source_term(&source)),
                    target: tgt.or_else(|| source_term(&target)),
                    length,
                    cost,
                    edges: Vec::new(),
                });
                last = index;
            }
            if p.is_some() {
                if edges == MAX_EDGES {
                    truncated = true;
                    continue;
                }
                edges += 1;
                if let Some(path) = paths.last_mut() {
                    path.edges.push([s, p, o]);
                }
            }
        }
        let mut terms = Terms::new(&prefixes, 500);
        let mut t = |x: &Option<Term>| x.as_ref().map_or(Value::Null, |x| json!(terms.term(x)));
        let number = |x: &Option<Term>| match x {
            Some(Term::Literal(l)) => l.value().parse::<f64>().map_or(Value::Null, super::number),
            _ => Value::Null,
        };
        let list: Vec<Value> = paths
            .iter()
            .map(|p| {
                json!({
                    "source": t(&p.source),
                    "target": t(&p.target),
                    "length": number(&p.length),
                    "cost": number(&p.cost),
                    "edges": p.edges.iter().map(|[s, pr, o]| {
                        json!(format!(
                            "{} {} {}",
                            t(s).as_str().unwrap_or(""),
                            t(pr).as_str().unwrap_or(""),
                            t(o).as_str().unwrap_or("")
                        ))
                    }).collect::<Vec<_>>(),
                })
            })
            .collect();
        let found = list.len() as u64;
        Ok(Outcome::Structured(json!({
            "dataset": ds.name,
            "commit": snap.commit,
            "algorithm": algorithm.name(),
            "paths": list,
            "limited": found == limit,
            "edgesTruncated": truncated,
            "prefixes": terms.used(),
        })))
    }
}
