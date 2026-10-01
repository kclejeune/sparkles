//! The `geof:` functions in SPARQL expressions (the `spatialF:` ones come later).
//!
//! Every argument error is a SPARQL type error: an argument that is not a geometry
//! literal (or is ill-typed), an unknown unit, CRSs without a transform between them, an
//! operation over `maxOpVertices`. A geometry result has the datatype and CRS of the
//! first geometry argument; its literal is charged to the query's memory budget.

use super::crs::{CRS84, CrsRef};
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
        "is3D" => f.accessor(|g| boolean(accessors::is_3d(g))),
        "isMeasured" => f.accessor(|g| boolean(accessors::is_measured(g))),
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
        ops::check_vertices(ops::DEFAULT_OP_VERTICES, &gs).map_err(op)
    }

    /// A constructed geometry as a literal of datatype `dt`, charged to the query.
    fn geometry(&self, g: &Geom, dt: &'static str) -> EvalResult<Val> {
        let lex = if dt == GEOJSON_LITERAL {
            let g = ops::transform(g, &CrsRef::Known(CRS84)).map_err(op)?;
            write::to_geojson(&g)
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
        any_uri(ops::crs_iri(&self.geom(0)?.crs).to_owned())
    }

    fn transform(&self) -> EvalResult<Val> {
        self.arity(2)?;
        let g = self.geom(0)?;
        let to = ops::known_crs(&self.iri(1)?).map_err(op)?;
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
