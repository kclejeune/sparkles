//! The projection a build reads (spec C22 §3.1 step 1, §6): the edges between resources
//! in the configured graphs, read as the settings' principal through its view, and the
//! check of §6 that every provider of the `community` role may receive facts.
//!
//! The reader runs ordinary SPARQL with the principal's `QueryOptions`, so the
//! principal's graph grants and triple protections apply as they do to its queries. It
//! records the concrete graphs it read, which the disclosure footprint of §6.2 needs.
//! Guarded graphs are never read.

use super::settings::{GraphragSettings, is_guarded};
use crate::assistant::{AssistantSettings, Send};
use crate::models::Pair;
use oxrdf::Term;
use sparkles::algo::{Aggregate, Control, Graph, Par, RawEdges};
use sparkles::sparql::{self, QueryOptions};
use sparkles::store::Snapshot;
use std::collections::BTreeSet;
use std::sync::Arc;

/// The projection of a build.
#[derive(Debug)]
pub struct Projection {
    /// canonical N-Triples terms of the nodes, sorted, in node index order
    pub terms: Vec<String>,
    pub graph: Graph,
    /// the graphs read, as IRIs, sorted
    pub graphs: Vec<String>,
    /// the edges read, before parallel edges were merged
    pub edges: usize,
}

/// Every provider of the role's pairs may receive facts (§6, A10): its `send` level,
/// the lower of `sendByProvider` and the dataset's `send`, must be at least `rows`.
pub fn check_send(assistant: &AssistantSettings, pairs: &[Pair]) -> Result<(), String> {
    if pairs.is_empty() {
        return Err(
            "the community role has no provider and model, so no report can be written".into(),
        );
    }
    for p in pairs {
        if assistant.send_to(&p.provider) < Send::Rows {
            let which = if assistant.send < Send::Rows {
                "send"
            } else {
                "sendByProvider"
            };
            return Err(format!(
                "{which} allows provider {} the schema only, and community reports send facts; allow it rows or remove it from the community role",
                p.provider
            ));
        }
    }
    Ok(())
}

/// Whether a graph IRI is one the settings' projection names.
pub(crate) fn named(settings: &GraphragSettings, iri: &str) -> bool {
    settings
        .projection
        .graphs
        .iter()
        .any(|g| match g.strip_suffix('*') {
            Some(stem) => iri.starts_with(stem),
            None => g == iri,
        })
}

fn esc_iri(iri: &str) -> Option<String> {
    oxrdf::NamedNode::new(iri).ok().map(|n| n.to_string())
}

/// Read the projection at `snap` with the principal's options.
pub fn read(
    snap: Arc<Snapshot>,
    opts: &QueryOptions,
    settings: &GraphragSettings,
    ctl: &Control,
) -> Result<Projection, String> {
    if settings.projection.weight.is_some() {
        return Err(
            "projection.weight is not supported by this server's community build; remove it".into(),
        );
    }
    let q = |text: &str| -> Result<Vec<Vec<Option<Term>>>, String> {
        ctl.check().map_err(|e| e.to_string())?;
        sparql::query(snap.clone(), text, opts)
            .map(|r| r.rows())
            .map_err(|e| e.to_string())
    };
    // the graphs the principal sees, limited to the projection's
    let mut graphs: Vec<String> = q("SELECT DISTINCT ?g WHERE { GRAPH ?g { ?s ?p ?o } }")?
        .into_iter()
        .filter_map(|r| match r.into_iter().next().flatten() {
            Some(Term::NamedNode(n)) => Some(n.into_string()),
            _ => None,
        })
        .filter(|g| !is_guarded(g) && named(settings, g))
        .collect();
    graphs.sort();
    graphs.dedup();
    let mut excluded: Vec<String> = Vec::new();
    for p in &settings.projection.exclude {
        excluded
            .push(esc_iri(p).ok_or_else(|| format!("projection.exclude: {p:?} is not an IRI"))?);
    }
    let mut terms: BTreeSet<String> = BTreeSet::new();
    let mut pairs: Vec<(String, String)> = Vec::new();
    if !graphs.is_empty() {
        let values: Vec<String> = graphs.iter().filter_map(|g| esc_iri(g)).collect();
        let filter = if excluded.is_empty() {
            String::new()
        } else {
            format!("FILTER(?p NOT IN ({}))", excluded.join(", "))
        };
        let text = format!(
            "SELECT ?s ?o WHERE {{ VALUES ?g {{ {} }} GRAPH ?g {{ ?s ?p ?o }} FILTER(isIRI(?s) && isIRI(?o) && ?s != ?o) {filter} }}",
            values.join(" ")
        );
        for r in q(&text)? {
            ctl.check().map_err(|e| e.to_string())?;
            if let [Some(s), Some(o)] = &r[..] {
                let (s, o) = (s.to_string(), o.to_string());
                terms.insert(s.clone());
                terms.insert(o.clone());
                pairs.push((s, o));
            }
        }
    }
    let terms: Vec<String> = terms.into_iter().collect();
    let index = |t: &str| {
        terms
            .binary_search_by(|x| x.as_str().cmp(t))
            .expect("collected") as u64
    };
    let mut src = Vec::with_capacity(pairs.len() * 2);
    let mut dst = Vec::with_capacity(pairs.len() * 2);
    for (s, o) in &pairs {
        let (a, b) = (index(s), index(o));
        src.extend([a, b]);
        dst.extend([b, a]);
    }
    let raw = RawEdges {
        nodes: (0..terms.len() as u64).collect(),
        src,
        dst,
        w: None,
        symmetric: true,
    };
    let graph =
        Graph::build(raw, Aggregate::Count, &Par::single(), ctl).map_err(|e| e.to_string())?;
    Ok(Projection {
        terms,
        graph,
        graphs,
        edges: pairs.len(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use sparkles::access::{GraphAccess, GraphRule, Graphs};
    use sparkles::io::{RdfFormat, Source};
    use sparkles::store::{Store, StoreOptions};

    const DATA: &str = r#"
<urn:g:org> {
  <http://ex/ana> <http://ex/memberOf> <http://ex/payments> .
  <http://ex/bob> <http://ex/memberOf> <http://ex/payments> .
  <http://ex/ana> a <http://ex/Person> .
  <http://ex/ana> <http://ex/name> "Ana" .
}
<urn:g:secret> { <http://ex/ana> <http://ex/knows> <http://ex/eve> . }
<urn:x-sparkles:graphrag:reports> { <http://ex/r> <http://ex/about> <http://ex/ana> . }
<urn:other> { <http://ex/x> <http://ex/y> <http://ex/z> . }
"#;

    fn store() -> Arc<Snapshot> {
        let s = Store::in_memory(StoreOptions::default());
        s.load(&[Source::from_bytes(
            DATA.as_bytes().to_vec(),
            RdfFormat::TriG,
            None,
        )])
        .unwrap();
        s.snapshot()
    }

    fn settings(graphs: &[&str]) -> GraphragSettings {
        let mut s = GraphragSettings::default();
        s.projection.graphs = graphs.iter().map(|g| g.to_string()).collect();
        s
    }

    #[test]
    fn edges_between_resources_in_the_projection_graphs() {
        let p = read(
            store(),
            &QueryOptions::default(),
            &settings(&["urn:g:*"]),
            &Control::unlimited(),
        )
        .unwrap();
        assert_eq!(p.graphs, vec!["urn:g:org", "urn:g:secret"]);
        // rdf:type is excluded by default, literals are not nodes
        assert_eq!(
            p.terms,
            vec![
                "<http://ex/ana>",
                "<http://ex/bob>",
                "<http://ex/eve>",
                "<http://ex/payments>"
            ]
        );
        assert_eq!(p.edges, 3);
        // the projection feeds the hierarchy
        assert_eq!(p.graph.n(), 4);
        let h = crate::graphrag::hierarchy::compute(&p.graph, 3, 1.0, 1, &Control::unlimited())
            .unwrap();
        let all: usize = h.depths[0].iter().map(|c| c.members.len()).sum();
        assert_eq!(all, 4);
    }

    #[test]
    fn the_principal_view_limits_what_is_read() {
        let view = GraphAccess::graphs(
            Graphs::Only(GraphRule::new(["urn:g:org"], &[])),
            Graphs::none(),
        );
        let opts = QueryOptions {
            graphs: Some(Arc::new(view)),
            ..Default::default()
        };
        let p = read(
            store(),
            &opts,
            &settings(&["urn:g:*"]),
            &Control::unlimited(),
        )
        .unwrap();
        assert_eq!(p.graphs, vec!["urn:g:org"]);
        assert!(!p.terms.contains(&"<http://ex/eve>".to_string()));
    }

    #[test]
    fn guarded_graphs_are_never_read() {
        let mut s = settings(&["urn:g:org"]);
        // settings validation refuses this; the reader refuses it again
        s.projection
            .graphs
            .push("urn:x-sparkles:graphrag:reports".into());
        let p = read(store(), &QueryOptions::default(), &s, &Control::unlimited()).unwrap();
        assert_eq!(p.graphs, vec!["urn:g:org"]);
        assert!(!p.terms.contains(&"<http://ex/r>".to_string()));
    }

    #[test]
    fn weights_are_refused() {
        let mut s = settings(&["urn:g:org"]);
        s.projection.weight = Some("http://ex/w".into());
        assert!(read(store(), &QueryOptions::default(), &s, &Control::unlimited()).is_err());
    }

    /// A provider limited to the schema cannot serve the role (A10).
    #[test]
    fn send_levels_are_checked() {
        let mut a = AssistantSettings {
            send: Send::Rows,
            ..Default::default()
        };
        let pairs = vec![Pair::new("p", "m"), Pair::new("q", "m")];
        check_send(&a, &pairs).unwrap();
        a.send_by_provider.insert("q".into(), Send::Schema);
        let e = check_send(&a, &pairs).unwrap_err();
        assert!(
            e.contains("sendByProvider") && e.contains("provider q"),
            "{e}"
        );
        // a provider entry never raises the dataset-wide limit
        a.send = Send::Schema;
        a.send_by_provider.insert("p".into(), Send::Rows);
        a.send_by_provider.insert("q".into(), Send::Rows);
        let e = check_send(&a, &pairs).unwrap_err();
        assert!(e.starts_with("send allows provider p"), "{e}");
        a.send_by_provider.clear();
        assert!(check_send(&a, &pairs).is_err());
        assert!(check_send(&a, &[]).is_err());
    }
}
