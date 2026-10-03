//! The GeoSPARQL aggregates: `geof:aggBoundingBox`, `aggBoundingCircle`, `aggCentroid`,
//! `aggConvexHull`, `aggUnion` and `aggConcaveHull` over one geometry expression.
//!
//! * An input that is an error or not a geometry makes the aggregate an error (unbound),
//!   as for the numeric aggregates; so does an empty group (there is no first input to
//!   take a CRS from).
//! * The result has the datatype and CRS of the first input; the others are transformed
//!   into that CRS (no transform: an error). Computations are planar in that CRS, like
//!   the functions they aggregate.
//! * All inputs together count against `maxOpVertices`.
//! * `aggCentroid` is the centroid of the union of the inputs; `aggConcaveHull` uses the
//!   default concavity of `geof:concaveHull` (an aggregate takes one expression).

use super::geom::Geom;
use super::ops::overlay::union_many;
use super::ops::{OpError, construct, hull, in_crs};
use super::vocab::{AGGREGATES, GEOJSON_LITERAL};
use super::{GeomRef, memo, write};
use crate::id::Id;
use crate::sparql::ctx::Ctx;
use crate::sparql::value::Value;
use georust::{Geometry, GeometryCollection};

/// The value of the aggregate `geof:<local>` over one group's evaluated values (after
/// DISTINCT; `Err` for a row whose expression was an error); `None` when `local` is not
/// a GeoSPARQL aggregate.
pub fn evaluate(ctx: &Ctx, local: &str, vals: &[std::result::Result<Id, ()>]) -> Option<Id> {
    if !AGGREGATES.contains(&local) {
        return None;
    }
    Some(compute(ctx, local, vals).unwrap_or(Id::UNDEF))
}

fn compute(ctx: &Ctx, local: &str, vals: &[std::result::Result<Id, ()>]) -> Option<Id> {
    let mut gs: Vec<GeomRef> = Vec::with_capacity(vals.len());
    for v in vals {
        gs.push(memo::by_id(ctx, (*v).ok()?, None).ok()?);
    }
    let first = gs.first()?;
    let refs: Vec<&Geom> = gs.iter().map(|g| &**g).collect();
    memo::check_op_vertices(ctx, &refs).ok()?;
    let out = match local {
        "aggUnion" => union_many(&refs).ok()??,
        "aggCentroid" => construct::centroid(&union_many(&refs).ok()??).ok()?,
        _ => {
            let all = collect(first, &refs).ok()?;
            match local {
                "aggBoundingBox" => construct::envelope(&all),
                "aggBoundingCircle" => hull::bounding_circle(&all),
                "aggConvexHull" => construct::convex_hull(&all),
                "aggConcaveHull" => hull::concave_hull(&all, hull::DEFAULT_CONCAVITY),
                _ => return None,
            }
            .ok()?
        }
    };
    let dt = match ctx.value(vals[0].ok()?) {
        Some(Value::Other { dt, .. }) if &*dt == GEOJSON_LITERAL => GEOJSON_LITERAL,
        _ => super::vocab::WKT_LITERAL,
    };
    let lex = if dt == GEOJSON_LITERAL {
        out.crs.known()?;
        write::to_geojson(&out)
    } else {
        write::to_wkt(&out)
    };
    // held until the query ends, like the local vocabulary the literal lands in
    std::mem::forget(ctx.charge(lex.len() as u64 + 64).ok()?);
    Some(ctx.intern_value(&Value::Other {
        lex: lex.into(),
        dt: dt.into(),
    }))
}

/// Every input as one collection in the CRS of `first`.
fn collect(first: &Geom, gs: &[&Geom]) -> Result<Geom, OpError> {
    let mut members = Vec::with_capacity(gs.len());
    for g in gs {
        if !g.empty {
            members.push(in_crs(g, &first.crs)?.into_owned().g);
        }
    }
    Ok(Geom::from_geometry(
        first.crs.clone(),
        Geometry::GeometryCollection(GeometryCollection(members)),
    ))
}

#[cfg(test)]
mod tests {
    use crate::io::{RdfFormat, Source};
    use crate::sparql::{QueryOptions, query};
    use crate::store::{Store, StoreOptions};
    use oxrdf::Term;

    const DATA: &str = r#"
@prefix ex: <http://example.org/> .
@prefix geo: <http://www.opengis.net/ont/geosparql#> .
ex:gA geo:asWKT "POLYGON((0 0, 10 0, 10 10, 0 10, 0 0))"^^geo:wktLiteral ; ex:group 1 .
ex:gC geo:asWKT "POLYGON((10 0, 20 0, 20 10, 10 10, 10 0))"^^geo:wktLiteral ; ex:group 1 .
ex:g1 geo:asWKT "POINT(2 2)"^^geo:wktLiteral ; ex:group 2 .
ex:g2 geo:asWKT "<http://www.opengis.net/def/crs/EPSG/0/4326> POINT(4 6)"^^geo:wktLiteral ; ex:group 2 .
ex:g3 geo:asGeoJSON "{\"type\":\"Point\",\"coordinates\":[30,30]}"^^geo:geoJSONLiteral ; ex:group 3 .
ex:gX geo:asWKT "POINT(1)"^^geo:wktLiteral ; ex:group 4 .
ex:g4 geo:asWKT "POINT(3 3)"^^geo:wktLiteral ; ex:group 4 .
"#;

    const P: &str = "PREFIX ex: <http://example.org/>
PREFIX geo: <http://www.opengis.net/ont/geosparql#>
PREFIX geof: <http://www.opengis.net/def/function/geosparql/>
";

    fn store() -> Store {
        let s = Store::in_memory(StoreOptions::default());
        s.load(&[Source::from_bytes(
            DATA.as_bytes().to_vec(),
            RdfFormat::Turtle,
            None,
        )])
        .unwrap();
        s
    }

    /// The first column of each solution (`None`: unbound).
    fn column(s: &Store, q: &str) -> Vec<Option<Term>> {
        let text = format!("{P}{q}");
        let r = query(s.snapshot(), &text, &QueryOptions::default())
            .unwrap_or_else(|e| panic!("{text}: {e}"));
        r.rows()
            .into_iter()
            .map(|row| row.into_iter().next().flatten())
            .collect()
    }

    fn ask(s: &Store, q: &str) -> bool {
        let text = format!("{P}{q}");
        query(s.snapshot(), &text, &QueryOptions::default())
            .unwrap_or_else(|e| panic!("{text}: {e}"))
            .boolean
    }

    #[test]
    fn union_of_two_polygons() {
        // the acceptance example: A ∪ C is one rectangle
        let s = store();
        assert!(ask(
            &s,
            "ASK { { SELECT (geof:aggUnion(?w) AS ?u) { VALUES ?w { \
             \"POLYGON((0 0, 10 0, 10 10, 0 10, 0 0))\"^^geo:wktLiteral \
             \"POLYGON((10 0, 20 0, 20 10, 10 10, 10 0))\"^^geo:wktLiteral } } } \
             FILTER(geof:sfEquals(?u, \"POLYGON((0 0,20 0,20 10,0 10,0 0))\"^^geo:wktLiteral)) }"
        ));
    }

    #[test]
    fn grouped_aggregates() {
        let s = store();
        let per_group = |agg: &str| -> Vec<Option<String>> {
            column(
                &s,
                &format!(
                    "SELECT (geof:{agg}(?w) AS ?r) {{ ?g ?p ?w ; ex:group ?k \
                     FILTER(?p IN (geo:asWKT, geo:asGeoJSON)) }} GROUP BY ?k ORDER BY ?k"
                ),
            )
            .into_iter()
            .map(|t| match t {
                Some(Term::Literal(l)) => Some(l.value().to_string()),
                None => None,
                other => panic!("{other:?}"),
            })
            .collect()
        };
        // group 2 mixes CRS84 and EPSG:4326 (lon 6, lat 4): the result is in CRS84,
        // the CRS of the first input; group 4 has a malformed literal: unbound
        let boxes = per_group("aggBoundingBox");
        assert_eq!(boxes.len(), 4);
        assert_eq!(
            boxes[0].as_deref(),
            Some("POLYGON((20 0, 20 10, 0 10, 0 0, 20 0))")
        );
        assert_eq!(boxes[3], None);
        assert!(ask(
            &s,
            "ASK { { SELECT (geof:aggBoundingBox(?w) AS ?b) { ?g geo:asWKT ?w ; ex:group 2 } } \
             FILTER(geof:sfEquals(?b, \"POLYGON((2 2, 6 2, 6 4, 2 4, 2 2))\"^^geo:wktLiteral)) }"
        ));
        // a GeoJSON first input gives a GeoJSON result
        let hulls = per_group("aggConvexHull");
        assert_eq!(
            hulls[2].as_deref(),
            Some(r#"{"type":"Point","coordinates":[30,30]}"#)
        );
        assert_eq!(hulls[3], None);
        for agg in [
            "aggUnion",
            "aggCentroid",
            "aggBoundingCircle",
            "aggConcaveHull",
        ] {
            let v = per_group(agg);
            assert!(
                v[0].is_some() && v[1].is_some() && v[2].is_some(),
                "{agg}: {v:?}"
            );
            assert_eq!(v[3], None, "{agg}");
        }
        assert_eq!(per_group("aggCentroid")[0].as_deref(), Some("POINT(10 5)"));
    }

    #[test]
    fn distinct_errors_and_empty_groups() {
        let s = store();
        // DISTINCT drops the repeated point before the union
        let u = column(
            &s,
            "SELECT (geof:aggUnion(DISTINCT ?w) AS ?u) { VALUES ?w { \
             \"POINT(1 1)\"^^geo:wktLiteral \"POINT(1 1)\"^^geo:wktLiteral \"POINT(2 2)\"^^geo:wktLiteral } }",
        );
        let Some(Term::Literal(l)) = &u[0] else {
            panic!("{u:?}")
        };
        assert_eq!(l.value(), "MULTIPOINT((1 1), (2 2))");
        // a non-geometry input, an error row, an empty group: unbound
        for q in [
            "SELECT (geof:aggUnion(?w) AS ?u) { VALUES ?w { \"POINT(1 1)\"^^geo:wktLiteral 3 } }",
            "SELECT (geof:aggConvexHull(?w) AS ?u) { VALUES ?x { 1 2 } BIND(geof:buffer(?x, 1, ex:nounit) AS ?w) }",
            "SELECT (geof:aggBoundingBox(?w) AS ?u) { ?g geo:asWKT ?w FILTER(false) }",
            // a CRS that has no transform to the first input's
            "SELECT (geof:aggUnion(?w) AS ?u) { VALUES ?w { \"POINT(1 1)\"^^geo:wktLiteral \
             \"<http://example.org/crs/mars> POINT(1 1)\"^^geo:wktLiteral } }",
        ] {
            assert_eq!(column(&s, q), vec![None], "{q}");
        }
        // the aggregate IRIs are not functions
        let text = format!("{P}SELECT (geof:aggUnion(?w) + 1 AS ?u) {{}}");
        assert!(query(s.snapshot(), &text, &QueryOptions::default()).is_ok());
        let text = format!("{P}SELECT * {{ BIND(geof:aggUnion(1) AS ?u) }}");
        assert!(query(s.snapshot(), &text, &QueryOptions::default()).is_err());
    }
}
