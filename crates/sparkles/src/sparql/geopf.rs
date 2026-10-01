//! GeoSPARQL in the planner: the Jena `spatial:` property functions as search leaves,
//! and spatial FILTERs on an indexed predicate's object pushed into a `SpatialScan`.
//!
//! ```sparql
//! ?f spatial:nearby (51.5 -0.12 5 uom:kilometre 10)
//! ?x geo:asWKT ?w FILTER(geof:sfWithin(?w, "POLYGON((…))"^^geo:wktLiteral))
//! ```
//!
//! Compiled without the `geo` feature too: the property functions are then recognized
//! and refused with [`crate::geo::not_built`], and nothing is pushed down.
//!
//! Stub: the property functions are recognized and refused; nothing is pushed down.

use super::ctx::Ctx;
pub use super::ctx::PlanWarning;
use super::expr::Expr;
use super::plan::{ActiveGraph, GraphFilter, Node, PathEnd, Planner, ScanSpec};
use super::table::VarId;
use crate::error::Result;
use crate::geo::{GeomRef, Relation, SpatialPfKind};
use crate::id::Id;
use spargebra::term::{TermPattern, TriplePattern};
use std::sync::Arc;

/// A scan of the indexed predicate `pred` whose object `geom_var` passes spatial tests.
#[derive(Clone)]
pub struct SpatialScanSpec {
    /// the scan it replaces (prefix, columns, graph)
    pub scan: ScanSpec,
    pub pred: Id,
    pub geom_var: VarId,
    pub subj_var: Option<VarId>,
    /// CRS84 boxes that contain every match
    pub windows: Vec<[f64; 4]>,
    /// the pushed conjuncts as exact tests (all must hold)
    pub tests: Vec<SpatialTest>,
    /// conjuncts evaluated on the operator's rows as an ordinary filter
    pub filter: Vec<Expr>,
    /// result-cache key of the constant geometries, tests, windows and radius
    pub key: u64,
}

/// An exact test of a row's geometry `?w` against a constant geometry.
#[derive(Clone)]
pub enum SpatialTest {
    /// `geof:<relation>(?w, q)`
    Relation(Relation, GeomRef),
    /// `geof:relate(?w, q, pattern)`
    Relate(Arc<str>, GeomRef),
    /// distance to `q` below `metres` (`<=` when inclusive)
    Distance {
        q: GeomRef,
        metres: f64,
        inclusive: bool,
    },
}

/// A `spatial:` property function planned as a leaf.
#[derive(Clone)]
pub struct SpatialPfSpec {
    pub func: SpatialPfKind,
    /// the query geometry (latitude/longitude arguments as an EPSG:4326 point)
    pub query: GeomRef,
    pub radius_m: Option<f64>,
    /// keep the `limit` matches nearest to the query
    pub limit: Option<usize>,
    /// the feature slot: a variable, or a constant restricting the answer
    pub subject: PathEnd,
    /// graph scope of the active graph
    pub graph: GraphFilter,
    /// `GRAPH ?g { … }` around the call: bound from each match's graph
    pub graph_var: Option<VarId>,
    /// one solution per feature across graphs (merged default graph)
    pub dedup: bool,
    /// result-cache key of the call's arguments
    pub key: u64,
}

/// A `spatial:` triple taken out of a basic graph pattern, with its list elements.
#[derive(Clone, Debug)]
pub struct SpatialCall {
    pub func: SpatialPfKind,
    pub subjects: Vec<TermPattern>,
    pub args: Vec<TermPattern>,
}

/// Take the `spatial:` property function calls out of `patterns`; returns the calls and
/// the remaining triple patterns.
pub fn take_spatial_calls(
    patterns: &[TriplePattern],
) -> Result<(Vec<SpatialCall>, Vec<TriplePattern>)> {
    let (calls, rest) = super::textpf::take_calls_where(
        patterns,
        |iri| SpatialPfKind::from_iri(iri).is_some(),
        |iri| SpatialPfKind::from_iri(iri).map_or_else(|| iri.to_string(), |k| k.name()),
    )?;
    let calls = calls
        .into_iter()
        .map(|(iri, subjects, args)| SpatialCall {
            func: SpatialPfKind::from_iri(iri.as_str()).expect("a spatial: call"),
            subjects,
            args,
        })
        .collect();
    Ok((calls, rest))
}

/// A `spatial:` call as a search leaf.
pub(super) fn spatial_leaf(p: &Planner<'_>, c: SpatialCall, g: &ActiveGraph) -> Result<Node> {
    let _ = (p, g, &c);
    #[cfg(not(feature = "geo"))]
    return Err(crate::geo::not_built());
    #[cfg(feature = "geo")]
    Err(crate::error::Error::Unsupported(format!(
        "{}: not supported yet",
        c.func.name()
    )))
}

/// Move spatial conjuncts over the object of an indexed predicate's scan into a
/// `SpatialScan`; returns the plan and the conjuncts left for an ordinary filter.
pub fn push_spatial(n: Node, exprs: Vec<Expr>, ctx: &Ctx) -> (Node, Vec<Expr>) {
    let _ = ctx;
    (n, exprs)
}

#[cfg(test)]
mod tests {
    use super::*;
    use spargebra::SparqlParser;
    use spargebra::algebra::GraphPattern;

    fn bgp(q: &str) -> Vec<TriplePattern> {
        let q = SparqlParser::new()
            .with_prefix("spatial", crate::geo::vocab::SPATIAL)
            .unwrap()
            .parse_query(q)
            .unwrap();
        let spargebra::Query::Select { pattern, .. } = q else {
            panic!()
        };
        let mut gp = &pattern;
        loop {
            match gp {
                GraphPattern::Project { inner, .. } => gp = inner,
                GraphPattern::Bgp { patterns } => return patterns.clone(),
                other => panic!("{other:?}"),
            }
        }
    }

    #[test]
    fn calls_are_taken_with_their_lists() {
        let ps = bgp("SELECT * { ?f spatial:nearby (51.5 -0.12 5) . ?f ?p ?o . \
             ?g spatial:withinBoxGeom (\"POINT(1 2)\") }");
        let (calls, rest) = take_spatial_calls(&ps).unwrap();
        assert_eq!(calls.len(), 2);
        assert_eq!(rest.len(), 1);
        assert_eq!(calls[0].func, SpatialPfKind::Nearby);
        assert_eq!(calls[0].args.len(), 3);
        assert_eq!(calls[1].func, SpatialPfKind::WithinBoxGeom);
        // unknown names in the namespace are ordinary triples
        let ps = bgp("SELECT * { ?f spatial:nearbyish ?x }");
        assert!(take_spatial_calls(&ps).unwrap().0.is_empty());
    }
}
