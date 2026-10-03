//! The GeoSPARQL Query Rewrite Extension and Jena's `spatial:equals` in the planner.
//!
//! With `queryRewrite` on in the dataset's `geo.json` (and not switched off for the
//! server), a triple pattern whose predicate is one of the 24 topological properties
//! matches the asserted triples and the derived ones: `so1 geo:R so2` holds when some
//! geometry literal of `so1` and some of `so2` satisfy `geof:R`, where a feature's
//! literals are those of its `geo:hasDefaultGeometry`, a geometry's are its
//! serializations, and a literal is itself.
//!
//! ```sparql
//! ?x geo:sfContains ex:g1        # asserted ∪ derived, one solution per ?x
//! ex:A spatial:equals ?y         # sfEquals between features or geometries
//! ```
//!
//! `spatial:equals` is never data: it is always taken, with or without query rewrite,
//! and matches the derived `sfEquals` triples only. A predicate variable matches the
//! asserted triples only, as before.

use super::ctx::Ctx;
use super::plan::{ActiveGraph, GraphFilter, Node, Planner};
use super::table::VarId;
use crate::error::Result;
use crate::geo::{GeomRef, Relation};
use crate::id::Id;
use spargebra::term::{NamedNodePattern, TermPattern, TriplePattern};

/// A topological triple (or `spatial:equals`) taken out of a basic graph pattern.
#[derive(Clone, Debug)]
pub struct RewriteCall {
    pub rel: Relation,
    pub subject: TermPattern,
    pub object: TermPattern,
    /// the predicate is a data property whose asserted triples count too (false for
    /// `spatial:equals`, which is never data)
    pub asserted: bool,
}

/// One end of a [`SpatialRelateSpec`].
#[derive(Clone)]
pub enum RelateEnd {
    Var(VarId),
    /// an IRI or blank node of the data (a feature or a geometry); also a literal that
    /// is not a geometry, which only asserted triples can hold
    Node(Id),
    /// a geometry literal written in the query
    Geometry(Id, GeomRef),
    /// a constant the data does not hold: it relates to nothing
    Absent,
}

/// A topological property (or `spatial:equals`) planned as a leaf: the asserted triples
/// of `property` and the derived ones, as a set.
#[derive(Clone)]
pub struct SpatialRelateSpec {
    pub rel: Relation,
    pub subject: RelateEnd,
    pub object: RelateEnd,
    /// the property's id when asserted triples count and the data holds it
    pub property: Option<Id>,
    /// graph scope of the active graph
    pub graph: GraphFilter,
    /// `GRAPH ?g { … }` around the pattern: bound from each match's graph
    pub graph_var: Option<VarId>,
    /// one solution per pair across graphs (merged default graph)
    pub dedup: bool,
    /// result-cache key of the relation and the constant ends
    pub key: u64,
}

/// `spatial:equals`.
pub const SPATIAL_EQUALS: &str = "http://jena.apache.org/spatial#equals";

/// Whether the snapshot's dataset rewrites topological properties.
pub fn rewrite_enabled(ctx: &Ctx) -> bool {
    cfg!(feature = "geo")
        && ctx
            .snap
            .geo
            .as_ref()
            .is_some_and(|v| v.config.query_rewrite)
}

/// Take the topological triples (when the dataset rewrites them) and `spatial:equals`
/// triples out of `patterns`; returns them and the remaining patterns.
pub fn take_rewrite_triples(
    patterns: Vec<TriplePattern>,
    ctx: &Ctx,
) -> Result<(Vec<RewriteCall>, Vec<TriplePattern>)> {
    let rewrite = rewrite_enabled(ctx);
    let mut calls = Vec::new();
    let mut rest = Vec::with_capacity(patterns.len());
    for tp in patterns {
        let NamedNodePattern::NamedNode(p) = &tp.predicate else {
            rest.push(tp);
            continue;
        };
        let (rel, asserted) = if p.as_str() == SPATIAL_EQUALS {
            (Relation::SfEquals, false)
        } else {
            match Relation::from_property(p.as_str()) {
                Some(r) if rewrite => (r, true),
                _ => {
                    rest.push(tp);
                    continue;
                }
            }
        };
        calls.push(RewriteCall {
            rel,
            subject: tp.subject,
            object: tp.object,
            asserted,
        });
    }
    Ok((calls, rest))
}

/// A taken triple as a leaf.
pub(super) fn rewrite_leaf(p: &Planner<'_>, c: RewriteCall, g: &ActiveGraph) -> Result<Node> {
    #[cfg(not(feature = "geo"))]
    {
        let _ = (p, g, &c);
        Err(crate::geo::not_built())
    }
    #[cfg(feature = "geo")]
    on::rewrite_leaf(p, c, g)
}

#[cfg(feature = "geo")]
mod on {
    use super::super::plan::{Kind, PT, short};
    use super::super::value::Value;
    use super::*;
    use crate::geo::exec::{config, summary};
    use crate::geo::{Fnv, IndexState, vocab};
    use crate::id::Tag;
    use crate::index::Perm;
    use std::sync::Arc;

    /// The end of a triple: a variable, a node of the data, or a geometry literal.
    fn end(p: &Planner<'_>, t: &TermPattern) -> RelateEnd {
        let id = match p.term_pattern(t) {
            PT::V(v) => return RelateEnd::Var(v),
            PT::C(id) => id,
        };
        let cfg = config(p.ctx);
        if let Some(Value::Other { lex, dt }) = p.ctx.value(id)
            && vocab::is_geometry_datatype(&dt)
            && let Ok(g) = crate::geo::parse_limited(&lex, &dt, cfg.max_vertices)
        {
            return RelateEnd::Geometry(id, Arc::new(g));
        }
        // an ill-typed geometry literal is a term like any other (it relates to nothing)
        if id.tag() == Tag::Local {
            RelateEnd::Absent
        } else {
            RelateEnd::Node(id)
        }
    }

    fn text(p: &Planner<'_>, e: &RelateEnd) -> String {
        let ctx = p.ctx;
        match e {
            RelateEnd::Var(v) => format!("?{}", ctx.var_name(*v)),
            RelateEnd::Geometry(_, g) => summary(g),
            RelateEnd::Node(id) => ctx.term(*id).map_or_else(|| "?".into(), |t| short(&t)),
            RelateEnd::Absent => "(absent)".into(),
        }
    }

    /// The predicate as written: `geo:sfContains`, `spatial:equals`.
    fn predicate_name(c: &RewriteCall) -> String {
        if c.asserted {
            format!("geo:{}", c.rel.local())
        } else {
            "spatial:equals".into()
        }
    }

    pub(super) fn rewrite_leaf(p: &Planner<'_>, c: RewriteCall, g: &ActiveGraph) -> Result<Node> {
        let ctx = p.ctx;
        let subject = end(p, &c.subject);
        let object = end(p, &c.object);
        let mut vars = Vec::new();
        for e in [&subject, &object] {
            if let RelateEnd::Var(v) = e
                && !vars.contains(v)
            {
                vars.push(*v);
            }
        }
        let Some((graph, graph_var)) = p.graph_filter(g) else {
            return Ok(Node::empty(vars));
        };
        if let Some(gv) = graph_var
            && !vars.contains(&gv)
        {
            vars.push(gv);
        }
        if matches!(subject, RelateEnd::Absent) || matches!(object, RelateEnd::Absent) {
            return Ok(Node::empty(vars));
        }
        let property = if c.asserted {
            ctx.snap
                .lookup_iri(&format!("{}{}", vocab::GEO, c.rel.local()))
        } else {
            None
        };
        let dedup = graph_var.is_none() && graph.multi();
        let cfg = config(ctx);
        // estimate: geometry rows, the asserted triples, features per geometry
        let rows: f64 = cfg
            .predicates
            .iter()
            .filter_map(|p| ctx.snap.lookup_iri(p))
            .map(|p| ctx.snap.estimate(Perm::Pso, &[p.0]) as f64)
            .sum::<f64>()
            .max(1.0);
        let asserted = property.map_or(0.0, |p| ctx.snap.estimate(Perm::Pso, &[p.0]) as f64);
        let is_var = |e: &RelateEnd| matches!(e, RelateEnd::Var(_));
        let ready = ctx
            .snap
            .geo
            .as_ref()
            .is_some_and(|v| v.state() == IndexState::Ready);
        let (est, cost) = match (is_var(&subject), is_var(&object)) {
            (false, false) => (1.0, 16.0),
            (true, true) => {
                let pairs = if c.rel.index_usable() {
                    2.0 * rows
                } else {
                    rows * rows
                };
                (
                    pairs + asserted,
                    rows * rows.log2().max(1.0) * 4.0 + pairs + asserted,
                )
            }
            _ => {
                let found = (rows * 0.01).max(1.0) * 2.0;
                let read = if ready && c.rel.index_usable() {
                    found * 4.0
                } else {
                    rows * 2.0
                };
                (found + asserted.min(rows).sqrt(), read + found)
            }
        };
        let desc = format!(
            "{} {} {} [{}]",
            text(p, &subject),
            predicate_name(&c),
            text(p, &object),
            if c.asserted {
                "asserted ∪ derived"
            } else {
                "derived"
            }
        );
        let mut h = Fnv::new();
        h.field(predicate_name(&c).as_bytes());
        for t in [&c.subject, &c.object] {
            match t {
                TermPattern::Variable(v) => h.field(format!("?{}", v.as_str()).as_bytes()),
                t => h.field(t.to_string().as_bytes()),
            }
        }
        let spec = SpatialRelateSpec {
            rel: c.rel,
            subject,
            object,
            property,
            graph,
            graph_var,
            dedup,
            key: h.finish(),
        };
        let sorted = vars.clone();
        let mut n = Node::leaf(Kind::SpatialRelate(Box::new(spec)), vars, est, desc);
        n.cost = cost;
        n.sorted = sorted;
        Ok(n)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spatial_equals_is_always_taken_and_properties_only_with_rewrite() {
        let q = "PREFIX geo: <http://www.opengis.net/ont/geosparql#>
            PREFIX spatial: <http://jena.apache.org/spatial#>
            SELECT * { ?a geo:sfWithin ?b . ?a spatial:equals ?c . ?a ?p ?b . ?a geo:asWKT ?w }";
        let q = spargebra::SparqlParser::new().parse_query(q).unwrap();
        let spargebra::Query::Select { pattern, .. } = q else {
            unreachable!()
        };
        fn bgp(p: &spargebra::algebra::GraphPattern) -> Vec<TriplePattern> {
            use spargebra::algebra::GraphPattern as GP;
            match p {
                GP::Bgp { patterns } => patterns.clone(),
                GP::Project { inner, .. } => bgp(inner),
                _ => unreachable!(),
            }
        }
        let store = crate::store::Store::in_memory(crate::store::StoreOptions::default());
        let ctx = Ctx::new(store.snapshot());
        assert!(!rewrite_enabled(&ctx));
        let (calls, rest) = take_rewrite_triples(bgp(&pattern), &ctx).unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].rel, Relation::SfEquals);
        assert!(!calls[0].asserted);
        assert_eq!(rest.len(), 3);
    }
}
