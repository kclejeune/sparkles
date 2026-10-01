//! What SHACL and ShEx validation share in the server and the CLI: the data graph of
//! Fuseki's `graph=default|union|<iri>` parameter with or without the materialized
//! inferences, the result limit, and the thread pool validations run in.

// (all of it is used by SHACL; ShEx uses it once its endpoint is wired in)
#![cfg_attr(not(feature = "shacl"), allow(dead_code))]

use anyhow::{Context, Result};
use sparkles::sparql::ctx::{DEFAULT_GRAPH_IRI, UNION_GRAPH_IRI};
use sparkles::store::Snapshot;

/// The data graph selected by Fuseki's `graph` parameter.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GraphParam {
    /// the store's default graph (the union of all graphs with `--union-default-graph`)
    Default,
    /// the union of all graphs (`urn:x-arq:UnionGraph`)
    Union,
    Named(String),
}

impl GraphParam {
    /// `default`, `union`, the Jena special graph IRIs, or a graph IRI.
    pub fn parse(s: &str) -> Result<GraphParam> {
        Ok(match s.trim() {
            "" | "default" | DEFAULT_GRAPH_IRI => GraphParam::Default,
            "union" | UNION_GRAPH_IRI => GraphParam::Union,
            iri => {
                let iri = iri
                    .strip_prefix('<')
                    .and_then(|i| i.strip_suffix('>'))
                    .unwrap_or(iri);
                oxrdf::NamedNode::new(iri).with_context(|| format!("invalid graph IRI '{iri}'"))?;
                GraphParam::Named(iri.to_string())
            }
        })
    }
}

/// The graphs a validation reads, in the terms of the validators' options
/// (`data_graph`, `extra_graphs`, `exclude_graphs`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ValidationInputs {
    pub data_graph: Option<String>,
    pub extra_graphs: Vec<String>,
    pub exclude_graphs: Vec<String>,
}

/// The data graph of `graph` in `snap`. `inferred` is the graph of materialized
/// inferences, if any: with `use_inferred` it is merged into the data graph, otherwise
/// it is kept out (even from the union of all graphs, which would include it).
pub fn inputs(
    snap: &Snapshot,
    graph: &GraphParam,
    inferred: Option<&str>,
    use_inferred: bool,
) -> Result<ValidationInputs> {
    let all_graphs = matches!(graph, GraphParam::Union)
        || (*graph == GraphParam::Default && snap.union_default_graph);
    let mut i = ValidationInputs::default();
    match graph {
        GraphParam::Named(iri) => i.data_graph = Some(iri.clone()),
        GraphParam::Union => i.data_graph = Some(UNION_GRAPH_IRI.to_string()),
        GraphParam::Default => {}
    }
    let Some(inferred) = inferred else {
        return Ok(i);
    };
    if use_inferred {
        i.extra_graphs.push(inferred.to_string());
    } else if all_graphs && snap.lookup_iri(inferred).is_some() {
        // The union of all graphs includes the inferred graph; spell it out without it.
        i.data_graph = Some(DEFAULT_GRAPH_IRI.to_string());
        for g in snap.graph_ids()? {
            if let Some(oxrdf::Term::NamedNode(n)) = snap.term(g)
                && n.as_str() != inferred
            {
                i.extra_graphs.push(n.into_string());
            }
        }
    }
    Ok(i)
}

/// Does the named graph exist (hold at least one quad)?
pub fn graph_exists(snap: &Snapshot, iri: &str) -> bool {
    snap.lookup_iri(iri)
        .is_some_and(|g| snap.count(sparkles::index::Perm::Gspo, &[g.0]).unwrap_or(0) > 0)
}

/// The fewest bytes one result takes in any report format (the one-line text form).
pub const MIN_RESULT_BYTES: u64 = 48;

/// Estimated memory of one result while the report is built.
const RESULT_MEMORY_BYTES: u64 = 512;

/// The most results a validation report may hold: what fits in the response budget
/// (`--max-result-mb`) at [`MIN_RESULT_BYTES`] each, and in the memory budget
/// (`--query-memory-mb`) at an estimated 512 bytes each.
pub fn max_results(limits: &crate::state::Limits) -> Option<usize> {
    let by_bytes = limits.max_result_bytes.map(|b| b / MIN_RESULT_BYTES);
    let by_memory = limits.query_memory_bytes.map(|b| b / RESULT_MEMORY_BYTES);
    let n = match (by_bytes, by_memory) {
        (Some(a), Some(b)) => a.min(b),
        (a, b) => a.or(b)?,
    };
    Some(usize::try_from(n).unwrap_or(usize::MAX).max(1))
}

/// The thread pool of `/{ds}/shacl` and `/{ds}/shex` validations: half the cores,
/// shared by all requests, so validations never take every core from queries.
pub fn validation_pool() -> Option<std::sync::Arc<rayon::ThreadPool>> {
    static POOL: std::sync::OnceLock<Option<std::sync::Arc<rayon::ThreadPool>>> =
        std::sync::OnceLock::new();
    POOL.get_or_init(|| {
        let n = std::thread::available_parallelism().map_or(2, |n| n.get());
        rayon::ThreadPoolBuilder::new()
            .num_threads((n / 2).max(1))
            .thread_name(|i| format!("validate-{i}"))
            .build()
            .map(std::sync::Arc::new)
            .ok()
    })
    .clone()
}

#[cfg(test)]
mod tests {
    use super::*;
    use sparkles::io::{RdfFormat, Source};
    use sparkles::store::{Store, StoreOptions};

    const INFERRED: &str = "urn:x-sparkles:inferred";

    #[test]
    fn graph_params() {
        assert_eq!(GraphParam::parse("").unwrap(), GraphParam::Default);
        assert_eq!(
            GraphParam::parse(DEFAULT_GRAPH_IRI).unwrap(),
            GraphParam::Default
        );
        assert_eq!(GraphParam::parse("union").unwrap(), GraphParam::Union);
        assert_eq!(
            GraphParam::parse("<http://ex.org/g>").unwrap(),
            GraphParam::Named("http://ex.org/g".into())
        );
        assert!(GraphParam::parse("not an iri").is_err());
    }

    #[test]
    fn inputs_with_and_without_inferences() {
        let store = Store::in_memory(StoreOptions::default());
        let trig = format!(
            "<urn:a> <urn:p> <urn:b> . <urn:g> {{ <urn:a> <urn:p> <urn:c> }} <{INFERRED}> {{ <urn:a> <urn:q> <urn:d> }}"
        );
        store
            .load(&[Source::from_bytes(trig.into_bytes(), RdfFormat::TriG, None)])
            .unwrap();
        let snap = store.snapshot();
        let i = inputs(&snap, &GraphParam::Default, Some(INFERRED), true).unwrap();
        assert_eq!(i.data_graph, None);
        assert_eq!(i.extra_graphs, vec![INFERRED.to_string()]);
        let i = inputs(&snap, &GraphParam::Union, Some(INFERRED), false).unwrap();
        assert_eq!(i.data_graph.as_deref(), Some(DEFAULT_GRAPH_IRI));
        assert_eq!(i.extra_graphs, vec!["urn:g".to_string()]);
        let i = inputs(&snap, &GraphParam::Union, None, false).unwrap();
        assert_eq!(i.data_graph.as_deref(), Some(UNION_GRAPH_IRI));
        assert!(graph_exists(&snap, "urn:g"));
        assert!(!graph_exists(&snap, "urn:none"));
    }

    #[test]
    fn result_limits() {
        let limits = |bytes, memory| crate::state::Limits {
            max_result_bytes: bytes,
            query_memory_bytes: memory,
            ..Default::default()
        };
        assert_eq!(max_results(&limits(Some(48 * 10), None)), Some(10));
        assert_eq!(max_results(&limits(Some(48 * 10), Some(512 * 3))), Some(3));
        assert_eq!(max_results(&limits(None, None)), None);
    }
}
