//! Views and live queries on a dataset whose queries read more than the stored quads
//! (spec F13 §4.5): RDFS on read, or the graph of materialized inferences added to the
//! default graph.
//!
//! Until circuits read the inferred graph themselves (ROADMAP item 221), such a query is
//! one `recompute` node that the query engine evaluates with the dataset's inference
//! options, exactly as a one-off query with the same options runs. Its footprint still
//! lets unrelated commits skip it. With RDFS on read, the footprint is that of the query
//! rewritten over stored triples for the current schema, and a change of a schema
//! predicate in the schema's graph touches every such circuit. With materialized
//! inferences, the entries of the default graph also match the inferred graph.

use super::compile::pattern_footprint;
use super::footprint::{Entry, FGraph, Footprint, Resolved};
use super::pass::Vis;
use crate::error::Result;
use crate::id::Id;
use crate::sparql::rdfs::{RdfsOnRead, SchemaSource, rewrite};
use crate::store::Snapshot;
use oxrdf::vocab::rdfs;
use oxrdf::{NamedNode, Term};
use sha2::{Digest, Sha256};
use spargebra::algebra::GraphPattern;
use std::borrow::Cow;
use std::sync::Arc;

/// What a dataset's queries read beyond the stored quads.
#[derive(Clone, Debug)]
pub struct Inference {
    /// RDFS on read
    pub rdfs: Option<Arc<RdfsOnRead>>,
    /// graphs merged into the default graph, such as the graph of materialized inferences
    pub default_graph_extra: Vec<String>,
}

impl Inference {
    /// The inference of a dataset with these settings, or `None` when its queries read
    /// the stored quads alone.
    pub fn new(rdfs: Option<Arc<RdfsOnRead>>, default_graph_extra: Vec<String>) -> Option<Self> {
        (rdfs.is_some() || !default_graph_extra.is_empty()).then_some(Inference {
            rdfs,
            default_graph_extra,
        })
    }

    /// Why a circuit evaluates the whole query again, for EXPLAIN and the view's state.
    pub fn reason(&self) -> &'static str {
        match (self.rdfs.is_some(), !self.default_graph_extra.is_empty()) {
            (true, true) => "RDFS on read and materialized inferences",
            (true, false) => "RDFS on read",
            _ => "materialized inferences",
        }
    }

    /// Whether `other` holds the same settings. RDFS on read compares by identity: a
    /// setting replaced by an equal one counts as a change, which costs one evaluation.
    pub fn same(a: Option<&Inference>, b: Option<&Inference>) -> bool {
        match (a, b) {
            (None, None) => true,
            (Some(a), Some(b)) => {
                let rdfs = match (&a.rdfs, &b.rdfs) {
                    (None, None) => true,
                    (Some(x), Some(y)) => Arc::ptr_eq(x, y),
                    _ => false,
                };
                rdfs && a.default_graph_extra == b.default_graph_extra
            }
            _ => false,
        }
    }

    /// A stable text naming the settings, which a checkpoint's fingerprint includes: a
    /// fixed schema by a hash of its triples, a schema graph by its name.
    pub fn key(&self) -> String {
        let mut h = Sha256::new();
        match self.rdfs.as_ref().map(|r| &r.source) {
            None => h.update(b"rdfs:none"),
            Some(SchemaSource::Graph(g)) => h.update(format!("rdfs:graph:{g:?}").as_bytes()),
            Some(SchemaSource::Fixed(s)) => {
                let mut lines: Vec<String> = s.triples().iter().map(|t| t.to_string()).collect();
                lines.sort_unstable();
                h.update(b"rdfs:fixed:");
                for l in lines {
                    h.update(l.as_bytes());
                    h.update([0]);
                }
            }
        }
        h.update([1]);
        for g in &self.default_graph_extra {
            h.update(g.as_bytes());
            h.update([0]);
        }
        let d = h.finalize();
        d[..16].iter().map(|b| format!("{b:02x}")).collect()
    }

    /// The entries a change of the schema matches: the schema predicates in the graph a
    /// schema graph source reads. A fixed schema changes only with the setting.
    fn schema_footprint(&self) -> Footprint {
        let mut fp = Footprint::default();
        let Some(SchemaSource::Graph(g)) = self.rdfs.as_ref().map(|r| &r.source) else {
            return fp;
        };
        let graph = match g {
            None => FGraph::Default,
            Some(iri) => FGraph::Graph(Term::NamedNode(NamedNode::new_unchecked(iri.clone()))),
        };
        for p in [
            rdfs::SUB_CLASS_OF,
            rdfs::SUB_PROPERTY_OF,
            rdfs::DOMAIN,
            rdfs::RANGE,
        ] {
            fp.add(Entry {
                g: graph.clone(),
                s: None,
                p: Some(Term::NamedNode(p.into_owned())),
                o: None,
            });
        }
        fp
    }

    /// The footprint of `pattern` read with these settings at `snap`, whose schema must
    /// be the one before the changes it is checked against.
    fn footprint(&self, pattern: &GraphPattern, snap: &Snapshot) -> Result<Footprint> {
        let gp = match &self.rdfs {
            Some(r) => Cow::Owned(rewrite(pattern, &*r.schema(snap)?)),
            None => Cow::Borrowed(pattern),
        };
        let mut fp = pattern_footprint(&gp);
        let defaults: Vec<Entry> = fp
            .entries
            .iter()
            .filter(|e| e.g == FGraph::Default)
            .cloned()
            .collect();
        for g in &self.default_graph_extra {
            let t = Term::NamedNode(NamedNode::new_unchecked(g.clone()));
            for e in &defaults {
                fp.add(Entry {
                    g: FGraph::Graph(t.clone()),
                    ..e.clone()
                });
            }
        }
        Ok(fp)
    }
}

/// A circuit's footprint over inferred data, resolved against the latest snapshot.
#[derive(Clone, Debug, Default)]
pub(crate) struct InferredFootprint {
    schema: Option<Resolved>,
    data: Option<Resolved>,
}

impl InferredFootprint {
    /// Whether the changes from the circuit's snapshot to `snap` can change the result
    /// of `pattern` read with `inf`. A schema change always can, and makes the footprint
    /// of the data be read again for the new schema. `whole` is set for a query whose
    /// footprint is every quad (a dataset clause).
    #[allow(clippy::too_many_arguments)]
    pub fn touches(
        &mut self,
        inf: &Inference,
        pattern: &GraphPattern,
        whole: bool,
        snap: &Snapshot,
        added: &[[Id; 4]],
        removed: &[[Id; 4]],
        vis: &Vis,
    ) -> Result<bool> {
        if added.is_empty() && removed.is_empty() {
            return Ok(false);
        }
        if !self.schema.as_ref().is_some_and(|r| r.current(snap)) {
            self.schema = Some(inf.schema_footprint().resolve(snap));
        }
        let schema = self.schema.as_ref().expect("resolved above");
        if schema.touches(added.iter().chain(removed.iter())) {
            // the schema the data's footprint was read with is gone
            self.data = None;
            return Ok(true);
        }
        if whole {
            return Ok(true);
        }
        if !self.data.as_ref().is_some_and(|r| r.current(snap)) {
            // no schema change: the schema at `snap` is the one before the changes
            self.data = Some(inf.footprint(pattern, snap)?.resolve(snap));
        }
        let data = self.data.as_ref().expect("resolved above");
        Ok(data.touches(
            added
                .iter()
                .chain(removed.iter())
                .filter(|q| vis.visible(snap, q)),
        ))
    }
}
