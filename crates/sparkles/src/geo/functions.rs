//! The `geof:` functions in SPARQL expressions (the `spatialF:` ones come later).
//!
//! Every argument error is a SPARQL type error: an argument that is not a geometry
//! literal (or is ill-typed), an unknown unit, CRSs without a transform between them, an
//! operation over `maxOpVertices`. A geometry result has the datatype and CRS of the
//! first geometry argument; its literal is charged to the query's memory budget.

use super::crs::{self, CrsRef};
use super::geom::Geom;
use super::ops::accessors::{self, Bound};
use super::ops::overlay::{Overlay, overlay};
use super::ops::{self, OpError, construct, distance, measure, relate};
use super::units::{Unit, UnitKind, unit};
use super::vocab::{GEOF, GEOJSON_LITERAL, Relation, WKT_LITERAL};
use super::{DistanceModel, GeomRef, memo, write};
use crate::sparql::ctx::Ctx;
use crate::sparql::expr::{Expr, Row, Val, arg};
use crate::sparql::value::{EvalResult, Num, TypeError, Value};

/// `xsd:anyURI`: unit and CRS arguments may be literals of it; IRI results are.
const XSD_ANY_URI: &str = "http://www.w3.org/2001/XMLSchema#anyURI";

const METRE: Unit = Unit {
    kind: UnitKind::Length,
    factor: 1.0,
};
const SQUARE_METRE: Unit = Unit {
    kind: UnitKind::Area,
    factor: 1.0,
};

/// Evaluate the GeoSPARQL function `iri`; `None` when `iri` is not one.
pub fn call(iri: &str, args: &[Expr], row: &Row<'_>, ctx: &Ctx) -> Option<EvalResult<Val>> {
    if iri.starts_with(super::vocab::SPATIALF) {
        return super::spatialf::call(iri, args, row, ctx);
    }
    let local = iri.strip_prefix(GEOF)?;
    let f = Call { args, row, ctx };
    if let Some(r) = Relation::from_local(local) {
        return Some(f.relation(r));
    }
    Some(match local {
        "relate" => f.relate(),
        "distance" => f.distance(Some(2)),
        "metricDistance" => f.distance(None),
        "buffer" => f.buffer(Some(2)),
        "metricBuffer" => f.buffer(None),
        "convexHull" => f.construct(construct::convex_hull),
        "envelope" => f.construct(construct::envelope),
        "boundary" => f.construct(construct::boundary),
        "centroid" => f.construct(construct::centroid),
        "intersection" => f.overlay(Overlay::Intersection),
        "union" => f.overlay(Overlay::Union),
        "difference" => f.overlay(Overlay::Difference),
        "symDifference" => f.overlay(Overlay::SymDifference),
        "getSRID" => f.get_srid(),
        "transform" => f.transform(),
        "asWKT" => f.convert(WKT_LITERAL),
        "asGeoJSON" => f.convert(GEOJSON_LITERAL),
        "area" => f.measure(measure::area, Some(1), SQUARE_METRE),
        "metricArea" => f.measure(measure::area, None, SQUARE_METRE),
        "length" => f.measure(measure::length, Some(1), METRE),
        "metricLength" => f.measure(measure::length, None, METRE),
        "perimeter" => f.measure(measure::perimeter, Some(1), METRE),
        "metricPerimeter" => f.measure(measure::perimeter, None, METRE),
        "dimension" => f.accessor(|g| integer(i64::from(g.dim()))),
        "coordinateDimension" => f.accessor(|g| integer(accessors::coordinate_dimension(g))),
        "spatialDimension" => f.accessor(|g| integer(accessors::spatial_dimension(g))),
        "is3D" => f.accessor(|g| boolean(g.layout.has_z())),
        "isMeasured" => f.accessor(|g| boolean(g.layout.has_m())),
        "isEmpty" => f.accessor(|g| boolean(g.empty)),
        "geometryType" => f.accessor(|g| any_uri(accessors::geometry_type(g))),
        "numGeometries" => f.accessor(|g| integer(accessors::num_geometries(g))),
        "geometryN" => f.geometry_n(),
        "minX" => f.accessor(|g| double(accessors::bound(g, Bound::MinX).map_err(op)?)),
        "minY" => f.accessor(|g| double(accessors::bound(g, Bound::MinY).map_err(op)?)),
        "maxX" => f.accessor(|g| double(accessors::bound(g, Bound::MaxX).map_err(op)?)),
        "maxY" => f.accessor(|g| double(accessors::bound(g, Bound::MaxY).map_err(op)?)),
        "minZ" => f.accessor(|g| double(accessors::z_bound(g, false).map_err(op)?)),
        "maxZ" => f.accessor(|g| double(accessors::z_bound(g, true).map_err(op)?)),
        _ => return None,
    })
}

/// The arguments of one call.
struct Call<'a, 'r> {
    args: &'a [Expr],
    row: &'a Row<'r>,
    ctx: &'a Ctx,
}

impl Call<'_, '_> {
    fn arity(&self, n: usize) -> EvalResult<()> {
        if self.args.len() == n {
            Ok(())
        } else {
            Err(TypeError)
        }
    }

    fn geom(&self, i: usize) -> EvalResult<GeomRef> {
        memo::geom_arg(self.args, i, self.row, self.ctx)
    }

    fn value(&self, i: usize) -> EvalResult<Value> {
        Ok(arg(self.args, i, self.row, self.ctx)?.into_owned())
    }

    /// An IRI argument: an IRI or an `xsd:anyURI` literal.
    fn iri(&self, i: usize) -> EvalResult<std::sync::Arc<str>> {
        match self.value(i)? {
            Value::Iri(iri) => Ok(iri),
            Value::Other { lex, dt } if &*dt == XSD_ANY_URI => Ok(lex),
            _ => Err(TypeError),
        }
    }

    /// A unit argument naming a known unit.
    fn unit(&self, i: usize) -> EvalResult<Unit> {
        unit(&self.iri(i)?).ok_or(TypeError)
    }

    fn number(&self, i: usize) -> EvalResult<f64> {
        Ok(Num::of(&self.value(i)?)?.to_double().into())
    }

    /// The datatype of argument `i` when it is a GeoJSON literal, else WKT (the
    /// datatype of a geometry result).
    fn datatype(&self, i: usize) -> &'static str {
        match self.value(i) {
            Ok(Value::Other { dt, .. }) if &*dt == GEOJSON_LITERAL => GEOJSON_LITERAL,
            _ => WKT_LITERAL,
        }
    }

    /// The distance model of the dataset's geo configuration (geodesic by default).
    fn model(&self) -> DistanceModel {
        self.ctx
            .snap
            .geo
            .as_ref()
            .map_or_else(DistanceModel::default, |v| v.config.distance)
    }

    /// Refuse inputs larger than one operation may take.
    fn sized(&self, gs: &[&GeomRef]) -> EvalResult<()> {
        let gs: Vec<&Geom> = gs.iter().map(|g| &***g).collect();
        memo::check_op_vertices(self.ctx, &gs)
    }

    /// A constructed geometry as a literal of datatype `dt`, charged to the query.
    fn geometry(&self, g: &Geom, dt: &'static str) -> EvalResult<Val> {
        let lex = if dt == GEOJSON_LITERAL {
            // GeoJSON is CRS84: the writer transforms built-in CRSs, others have none
            g.crs.known().ok_or(TypeError)?;
            write::to_geojson(g)
        } else {
            write::to_wkt(g)
        };
        // held until the query ends, like the local vocabulary the literal lands in
        match self.ctx.charge(lex.len() as u64 + 64) {
            Ok(c) => std::mem::forget(c),
            Err(_) => return Err(TypeError),
        }
        Ok(Val::V(Value::Other {
            lex: lex.into(),
            dt: dt.into(),
        }))
    }

    fn relation(&self, r: Relation) -> EvalResult<Val> {
        self.arity(2)?;
        let (a, b) = (self.geom(0)?, self.geom(1)?);
        self.sized(&[&a, &b])?;
        boolean(relate::relation(&a, &b, r).map_err(op)?)
    }

    fn relate(&self) -> EvalResult<Val> {
        self.arity(3)?;
        let (a, b) = (self.geom(0)?, self.geom(1)?);
        let pattern = match self.value(2)? {
            Value::Str(s) => s,
            _ => return Err(TypeError),
        };
        self.sized(&[&a, &b])?;
        boolean(relate::relate(&a, &b, &pattern).map_err(op)?)
    }

    /// `distance(g1, g2, unit)` (`unit_arg` 2) or `metricDistance(g1, g2)`.
    fn distance(&self, unit_arg: Option<usize>) -> EvalResult<Val> {
        self.arity(unit_arg.map_or(2, |i| i + 1))?;
        let (a, b) = (self.geom(0)?, self.geom(1)?);
        self.sized(&[&a, &b])?;
        let u = match unit_arg {
            Some(i) => self.unit(i)?,
            None => METRE,
        };
        double(distance::distance(&a, &b, &u, self.model()).map_err(op)?)
    }

    /// `buffer(g, radius, unit)` or `metricBuffer(g, radius)`.
    fn buffer(&self, unit_arg: Option<usize>) -> EvalResult<Val> {
        self.arity(unit_arg.map_or(2, |i| i + 1))?;
        let g = self.geom(0)?;
        let r = self.number(1)?;
        let u = match unit_arg {
            Some(i) => self.unit(i)?,
            None => METRE,
        };
        self.sized(&[&g])?;
        let out = construct::buffer(&g, r, &u).map_err(op)?;
        self.geometry(&out, self.datatype(0))
    }

    fn construct(&self, f: fn(&Geom) -> Result<Geom, OpError>) -> EvalResult<Val> {
        self.arity(1)?;
        let g = self.geom(0)?;
        self.sized(&[&g])?;
        self.geometry(&f(&g).map_err(op)?, self.datatype(0))
    }

    fn overlay(&self, o: Overlay) -> EvalResult<Val> {
        self.arity(2)?;
        let (a, b) = (self.geom(0)?, self.geom(1)?);
        self.sized(&[&a, &b])?;
        self.geometry(&overlay(&a, &b, o).map_err(op)?, self.datatype(0))
    }

    fn get_srid(&self) -> EvalResult<Val> {
        self.arity(1)?;
        any_uri(self.geom(0)?.crs_iri().to_owned())
    }

    fn transform(&self) -> EvalResult<Val> {
        self.arity(2)?;
        let g = self.geom(0)?;
        let to = CrsRef::Known(crs::lookup(&self.iri(1)?).ok_or(TypeError)?);
        let out = ops::transform(&g, &to).map_err(op)?;
        self.geometry(&out, self.datatype(0))
    }

    /// `asWKT` / `asGeoJSON`.
    fn convert(&self, dt: &'static str) -> EvalResult<Val> {
        self.arity(1)?;
        let g = self.geom(0)?;
        self.geometry(&g, dt)
    }

    /// `area(g, unit)` and the like (`unit_arg` 1), or their `metric…` forms (in
    /// `metric`).
    fn measure(
        &self,
        f: fn(&Geom, &Unit, DistanceModel) -> Result<f64, OpError>,
        unit_arg: Option<usize>,
        metric: Unit,
    ) -> EvalResult<Val> {
        self.arity(unit_arg.map_or(1, |i| i + 1))?;
        let g = self.geom(0)?;
        let u = match unit_arg {
            Some(i) => self.unit(i)?,
            None => metric,
        };
        self.sized(&[&g])?;
        double(f(&g, &u, self.model()).map_err(op)?)
    }

    fn accessor(&self, f: impl FnOnce(&Geom) -> EvalResult<Val>) -> EvalResult<Val> {
        self.arity(1)?;
        let g = self.geom(0)?;
        f(&g)
    }

    fn geometry_n(&self) -> EvalResult<Val> {
        self.arity(2)?;
        let g = self.geom(0)?;
        let n = match self.value(1)? {
            Value::Integer(i) => i64::from(i),
            _ => return Err(TypeError),
        };
        let m = accessors::geometry_n(&g, n).map_err(op)?;
        self.geometry(&m, self.datatype(0))
    }
}

fn op(_: OpError) -> TypeError {
    TypeError
}

fn boolean(v: bool) -> EvalResult<Val> {
    Ok(Val::Id(crate::id::Id::from_bool(v)))
}

fn integer(v: i64) -> EvalResult<Val> {
    Ok(Val::V(Value::Integer(v.into())))
}

fn double(v: f64) -> EvalResult<Val> {
    if v.is_finite() {
        Ok(Val::V(Value::Double(v.into())))
    } else {
        Err(TypeError)
    }
}

fn any_uri(iri: String) -> EvalResult<Val> {
    Ok(Val::V(Value::Other {
        lex: iri.into(),
        dt: XSD_ANY_URI.into(),
    }))
}

#[cfg(test)]
mod tests {
    use crate::io::{RdfFormat, Source};
    use crate::sparql::{QueryOptions, query};
    use crate::store::{Store, StoreOptions};
    use oxrdf::Term;

    const FIXTURE: &str = r#"
@prefix ex: <http://example.org/> .
@prefix geo: <http://www.opengis.net/ont/geosparql#> .
ex:gA geo:asWKT "POLYGON((0 0, 10 0, 10 10, 0 10, 0 0))"^^geo:wktLiteral .
ex:gB geo:asWKT "POLYGON((5 5, 15 5, 15 15, 5 15, 5 5))"^^geo:wktLiteral .
ex:gC geo:asWKT "POLYGON((10 0, 20 0, 20 10, 10 10, 10 0))"^^geo:wktLiteral .
ex:g1 geo:asWKT "POINT(2 2)"^^geo:wktLiteral .
ex:g2 geo:asWKT "<http://www.opengis.net/def/crs/EPSG/0/4326> POINT(2 12)"^^geo:wktLiteral .
ex:g3 geo:asGeoJSON "{\"type\":\"Point\",\"coordinates\":[30,30]}"^^geo:geoJSONLiteral .
ex:gX geo:asWKT "POINT(1)"^^geo:wktLiteral .
ex:gE geo:asWKT ""^^geo:wktLiteral .
ex:gM geo:asWKT "<http://example.org/crs/mars> POINT(1 1)"^^geo:wktLiteral .
"#;

    const PREFIXES: &str = "PREFIX ex: <http://example.org/>
PREFIX geo: <http://www.opengis.net/ont/geosparql#>
PREFIX geof: <http://www.opengis.net/def/function/geosparql/>
PREFIX uom: <http://www.opengis.net/def/uom/OGC/1.0/>
PREFIX xsd: <http://www.w3.org/2001/XMLSchema#>
";

    fn store() -> Store {
        let s = Store::in_memory(StoreOptions::default());
        s.load(&[Source::from_bytes(
            FIXTURE.as_bytes().to_vec(),
            RdfFormat::Turtle,
            None,
        )])
        .unwrap();
        s
    }

    /// `expr` evaluated with `?a`, `?b`, `?c`, `?p1`, `?p2`, `?p3`, `?x`, `?e`, `?m`
    /// bound to the stored literals of `ex:gA`, `ex:gB`, `ex:gC`, `ex:g1`, `ex:g2`,
    /// `ex:g3`, `ex:gX`, `ex:gE`, `ex:gM`.
    fn eval(s: &Store, expr: &str) -> Option<Term> {
        let mut pattern = String::new();
        for (v, g) in [
            ("a", "gA"),
            ("b", "gB"),
            ("c", "gC"),
            ("p1", "g1"),
            ("p2", "g2"),
            ("p3", "g3"),
            ("x", "gX"),
            ("e", "gE"),
            ("m", "gM"),
        ] {
            let used = expr
                .match_indices(&format!("?{v}"))
                .any(|(i, m)| !expr[i + m.len()..].starts_with(|c: char| c.is_alphanumeric()));
            if used {
                let p = if v == "p3" {
                    "geo:asGeoJSON"
                } else {
                    "geo:asWKT"
                };
                pattern.push_str(&format!("ex:{g} {p} ?{v} . "));
            }
        }
        let text = format!("{PREFIXES}SELECT ?r {{ {pattern} BIND({expr} AS ?r) }}");
        let r = query(s.snapshot(), &text, &QueryOptions::default())
            .unwrap_or_else(|e| panic!("{text}: {e}"));
        let mut rows = r.rows();
        assert_eq!(rows.len(), 1, "{text}");
        rows.pop().unwrap().pop().unwrap()
    }

    fn lit(s: &Store, expr: &str) -> (String, String) {
        match eval(s, expr) {
            Some(Term::Literal(l)) => (l.value().to_string(), l.datatype().as_str().to_string()),
            other => panic!("{expr}: {other:?}"),
        }
    }

    fn num(s: &Store, expr: &str) -> f64 {
        let (v, dt) = lit(s, expr);
        assert!(
            dt.ends_with("#double") || dt.ends_with("#integer"),
            "{expr}: {dt}"
        );
        v.parse().unwrap()
    }

    fn truth(s: &Store, expr: &str) -> bool {
        match lit(s, expr) {
            (v, dt) if dt.ends_with("#boolean") => v == "true",
            other => panic!("{expr}: {other:?}"),
        }
    }

    const ANY_URI: &str = "http://www.w3.org/2001/XMLSchema#anyURI";
    const WKT: &str = "http://www.opengis.net/ont/geosparql#wktLiteral";

    #[test]
    fn relations() {
        let s = store();
        for e in [
            "geof:sfTouches(?a, ?c)",
            "geof:sfIntersects(?a, ?c)",
            "geof:sfOverlaps(?a, ?b)",
            "geof:sfContains(?a, \"POINT(2 2)\"^^geo:wktLiteral)",
            "geof:rcc8ec(?a, ?c)",
            "geof:rcc8po(?a, ?b)",
            "geof:ehCovers(?a, \"LINESTRING(0 1, 5 1)\"^^geo:wktLiteral)",
            "geof:sfEquals(\"POINT(1 1)\"^^geo:wktLiteral, \"Point (1.0 1.0)\"^^geo:wktLiteral)",
            // EPSG:4326 POINT(2 12) is longitude 12, latitude 2: within C
            "geof:sfWithin(?p2, ?c)",
            "geof:relate(?a, ?b, \"212101212\")",
            "geof:sfEquals(?m, ?m)",
            "geof:sfDisjoint(?e, ?a)",
        ] {
            assert!(truth(&s, e), "{e}");
        }
        for e in [
            "geof:ehCovers(?a, \"LINESTRING(1 1, 5 1)\"^^geo:wktLiteral)",
            "geof:sfOverlaps(?a, ?c)",
            "geof:rcc8ec(?a, \"POINT(10 5)\"^^geo:wktLiteral)",
            "geof:sfEquals(\"POINT(1 1)\"^^geo:wktLiteral, \"POINT(1 2)\"^^geo:wktLiteral)",
            "geof:sfWithin(?p2, ?a)",
            "geof:sfIntersects(?e, ?a)",
        ] {
            assert!(!truth(&s, e), "{e}");
        }
    }

    #[test]
    fn distances() {
        let s = store();
        let p = |a: &str, b: &str| {
            format!("\"POINT({a})\"^^geo:wktLiteral, \"POINT({b})\"^^geo:wktLiteral")
        };
        let d = num(&s, &format!("geof:metricDistance({})", p("0 0", "1 0")));
        assert!((d - 111_319.490_793_273_57).abs() < 1e-6, "{d}");
        for unit in [
            "uom:kilometre",
            "<http://qudt.org/vocab/unit/KiloM>",
            "\"http://www.opengis.net/def/uom/OGC/1.0/kilometre\"^^xsd:anyURI",
        ] {
            let d = num(&s, &format!("geof:distance({}, {unit})", p("0 0", "1 0")));
            assert!((d - 111.319_490_793_273_57).abs() < 1e-9, "{unit}: {d}");
        }
        let d = num(
            &s,
            &format!("geof:distance({}, uom:degree)", p("0 0", "1 0")),
        );
        assert!((d - 1.0).abs() < 1e-9, "{d}");
        let d = num(
            &s,
            "geof:distance(?a, \"POINT(12 5)\"^^geo:wktLiteral, uom:metre)",
        );
        assert!((d - 221_800.0).abs() / 221_800.0 < 0.001, "{d}");
        assert_eq!(num(&s, "geof:metricDistance(?a, ?p1)"), 0.0);
    }

    #[test]
    fn crs_and_accessors() {
        let s = store();
        let epsg = "http://www.opengis.net/def/crs/EPSG/0/4326";
        assert_eq!(lit(&s, "geof:getSRID(?p2)"), (epsg.into(), ANY_URI.into()));
        assert_eq!(
            lit(&s, "geof:getSRID(?p1)"),
            (crate::geo::crs::CRS84_IRI.into(), ANY_URI.into())
        );
        assert_eq!(lit(&s, "geof:getSRID(?m)").0, "http://example.org/crs/mars");
        assert_eq!(
            lit(
                &s,
                "geof:transform(?p2, <http://www.opengis.net/def/crs/OGC/1.3/CRS84>)"
            ),
            ("POINT(12 2)".into(), WKT.into())
        );
        assert_eq!(
            lit(&s, "geof:asWKT(?p2)"),
            (format!("<{epsg}> POINT(2 12)"), WKT.into())
        );
        assert_eq!(
            lit(&s, "geof:asGeoJSON(?p2)").0,
            r#"{"type":"Point","coordinates":[12,2]}"#
        );
        assert_eq!(lit(&s, "geof:asWKT(?p3)").0, "POINT(30 30)");
        assert_eq!(num(&s, "geof:minX(?p2)"), 2.0);
        assert_eq!(num(&s, "geof:maxY(?a)"), 10.0);
        let mp = "\"MULTIPOINT((1 1),(2 2))\"^^geo:wktLiteral";
        assert_eq!(num(&s, &format!("geof:numGeometries({mp})")), 2.0);
        assert_eq!(lit(&s, &format!("geof:geometryN({mp}, 2)")).0, "POINT(2 2)");
        assert_eq!(
            lit(&s, "geof:geometryType(?a)"),
            (
                "http://www.opengis.net/ont/sf#Polygon".into(),
                ANY_URI.into()
            )
        );
        assert_eq!(num(&s, "geof:dimension(?a)"), 2.0);
        assert!(truth(&s, "geof:isEmpty(?e)"));
        assert_eq!(num(&s, "geof:dimension(?e)"), -1.0);
        assert!(!truth(&s, "geof:is3D(?a)"));
        assert_eq!(num(&s, "geof:coordinateDimension(?a)"), 2.0);
    }

    #[test]
    fn constructions_and_measures() {
        let s = store();
        let equal = |expr: &str, want: &str| {
            let e = format!("geof:sfEquals({expr}, \"{want}\"^^geo:wktLiteral)");
            assert!(truth(&s, &e), "{e}");
        };
        equal(
            "geof:intersection(?a, ?b)",
            "POLYGON((5 5,10 5,10 10,5 10,5 5))",
        );
        equal("geof:union(?a, ?c)", "POLYGON((0 0,20 0,20 10,0 10,0 0))");
        equal(
            "geof:envelope(\"LINESTRING(0 0, 2 1)\"^^geo:wktLiteral)",
            "POLYGON((0 0,2 0,2 1,0 1,0 0))",
        );
        equal(
            "geof:convexHull(\"MULTIPOINT((0 0),(2 0),(1 1),(1 0.5))\"^^geo:wktLiteral)",
            "POLYGON((0 0,2 0,1 1,0 0))",
        );
        equal("geof:centroid(?a)", "POINT(5 5)");
        assert_eq!(lit(&s, "geof:boundary(?p1)").0, "GEOMETRYCOLLECTION EMPTY");
        assert_eq!(
            lit(
                &s,
                "geof:intersection(?a, \"POINT(50 50)\"^^geo:wktLiteral)"
            )
            .0,
            "POINT EMPTY"
        );
        let square = "\"POLYGON((0 0, 1 0, 1 1, 0 1, 0 0))\"^^geo:wktLiteral";
        let a = num(&s, &format!("geof:metricArea({square})"));
        assert!((a - 12_308_778_361.469).abs() / a < 1e-6, "{a}");
        let km2 = num(
            &s,
            &format!("geof:area({square}, <http://qudt.org/vocab/unit/KiloM2>)"),
        );
        assert!((km2 - 12_308.778).abs() < 0.01, "{km2}");
        let l = num(
            &s,
            "geof:metricLength(\"LINESTRING(0 0, 1 0)\"^^geo:wktLiteral)",
        );
        assert!((l - 111_319.490_793_273_57).abs() < 1e-6);
        assert_eq!(
            num(&s, "geof:metricArea(\"POINT(0 0)\"^^geo:wktLiteral)"),
            0.0
        );
        let disc = num(
            &s,
            "geof:metricArea(geof:metricBuffer(\"POINT(0 0)\"^^geo:wktLiteral, 1000))",
        );
        let circle = std::f64::consts::PI * 1e6;
        assert!(disc > circle * 0.993 && disc < circle * 1.001, "{disc}");
        // a GeoJSON argument gives a GeoJSON result
        let (_, dt) = lit(&s, "geof:envelope(?p3)");
        assert_eq!(dt, crate::geo::vocab::GEOJSON_LITERAL);
    }

    #[test]
    fn type_errors() {
        let s = store();
        for e in [
            "geof:sfIntersects(?x, ?a)",
            "geof:sfIntersects(?m, ?a)",
            "geof:distance(?a, ?b, <http://www.opengis.net/def/uom/OGC/1.0/parsec>)",
            "geof:area(?a, uom:metre)",
            "geof:geometryN(?a, 2)",
            "geof:minZ(?a)",
            "geof:relate(?a, ?b, \"TT\")",
            "geof:metricDistance(?m, ?m)",
            "geof:metricDistance(?e, ?a)",
            "geof:minX(?e)",
            "geof:sfIntersects(?a)",
            "geof:buffer(?p1, -1, uom:metre)",
            "geof:asGeoJSON(?m)",
            "geof:transform(?a, <http://example.org/crs/mars>)",
        ] {
            assert_eq!(eval(&s, e), None, "{e}");
        }
    }
}
