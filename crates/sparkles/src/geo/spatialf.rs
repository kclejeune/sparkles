//! Jena's filter functions (`spatialF:`, `http://jena.apache.org/function/spatial#`),
//! with Jena's argument forms: a unit, a datatype or a CRS may be an IRI, an
//! `xsd:anyURI` literal or a plain string.
//!
//! | Function | Result |
//! |---|---|
//! | `convertLatLon(lat, lon)` | an EPSG:4326 `POINT` (numbers or numeric strings; latitude within ±90, longitude within ±180) |
//! | `convertLatLonBox(latMin, lonMin, latMax, lonMax)` | an EPSG:4326 `POLYGON` |
//! | `equals(g1, g2)` | `geof:sfEquals` |
//! | `nearby(g1, g2, radius, unit)`, `withinCircle` | distance `<` radius |
//! | `distance(g1, g2, unit)` | `geof:distance` |
//! | `greatCircle(lat1, lon1, lat2, lon2, unit)` | the distance between two points under the dataset's distance model (geodesic or haversine); a length unit |
//! | `greatCircleGeom(g1, g2, unit)` | as `greatCircle` between the closest points (projected geometries are transformed to WGS 84 first) |
//! | `angle(x1, y1, x2, y2)` / `angleDeg` | the direction from the first point to the second, clockwise from the y axis, in [0, 2π) radians / degrees (rounded to 6 decimals, as Jena) |
//! | `azimuth(lat1, lon1, lat2, lon2)` / `azimuthDeg` | the initial great-circle bearing, clockwise from north, in [0, 2π) radians / degrees (rounded to 6 decimals) |
//! | `transform(g, datatype, srs)`, `transformDatatype(g, datatype)`, `transformSRS(g, srs)` | `g` in another datatype and/or CRS |
//!
//! Every argument error is a type error, as for the `geof:` functions.

use super::crs::{self, CRS84, CrsRef, EPSG_4326};
use super::geom::Geom;
use super::ops::{self, distance, relate};
use super::units::{Unit, UnitKind, unit};
use super::vocab::{GEOJSON_LITERAL, Relation, SPATIALF, WKT_LITERAL};
use super::{DistanceModel, GeomRef, memo, write};
use crate::sparql::ctx::Ctx;
use crate::sparql::expr::{Expr, Row, Val, arg};
use crate::sparql::value::{EvalResult, Num, TypeError, Value};
use georust::{Coord, Geometry, LineString, Point, Polygon};

const XSD_ANY_URI: &str = "http://www.w3.org/2001/XMLSchema#anyURI";

/// Jena's `spatialF:` filter functions, by local name.
pub const FUNCTIONS: &[&str] = &[
    "convertLatLon",
    "convertLatLonBox",
    "equals",
    "nearby",
    "withinCircle",
    "distance",
    "greatCircle",
    "greatCircleGeom",
    "angle",
    "angleDeg",
    "azimuth",
    "azimuthDeg",
    "transform",
    "transformDatatype",
    "transformSRS",
];

/// Evaluate the Jena filter function `iri`; `None` when `iri` is not one.
pub fn call(iri: &str, args: &[Expr], row: &Row<'_>, ctx: &Ctx) -> Option<EvalResult<Val>> {
    let local = iri.strip_prefix(SPATIALF)?;
    let f = Call { args, row, ctx };
    Some(match local {
        "convertLatLon" => f.convert_lat_lon(),
        "convertLatLonBox" => f.convert_lat_lon_box(),
        "equals" => f.equals(),
        "nearby" | "withinCircle" => f.nearby(),
        "distance" => f.distance(),
        "greatCircle" => f.great_circle(),
        "greatCircleGeom" => f.great_circle_geom(),
        "angle" => f.four(|v| angle(v[0], v[1], v[2], v[3])),
        "angleDeg" => f.four(|v| six_decimals(angle(v[0], v[1], v[2], v[3]).to_degrees())),
        "azimuth" => f.four(|v| azimuth(v[0], v[1], v[2], v[3])),
        "azimuthDeg" => f.four(|v| six_decimals(azimuth(v[0], v[1], v[2], v[3]).to_degrees())),
        "transform" => f.transform(Some(1), Some(2)),
        "transformDatatype" => f.transform(Some(1), None),
        "transformSRS" => f.transform(None, Some(1)),
        _ => return None,
    })
}

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

    /// A numeric argument.
    fn number(&self, i: usize) -> EvalResult<f64> {
        let v: f64 = Num::of(&self.value(i)?)?.to_double().into();
        if v.is_finite() { Ok(v) } else { Err(TypeError) }
    }

    /// A number, or a string that reads as one (Jena's `convertLatLon`).
    fn lenient_number(&self, i: usize) -> EvalResult<f64> {
        match self.value(i)? {
            Value::Str(s) => s
                .trim()
                .parse::<f64>()
                .ok()
                .filter(|v| v.is_finite())
                .ok_or(TypeError),
            v => {
                let v: f64 = Num::of(&v)?.to_double().into();
                if v.is_finite() { Ok(v) } else { Err(TypeError) }
            }
        }
    }

    /// An IRI argument: an IRI, an `xsd:anyURI` literal or a plain string.
    fn uri(&self, i: usize) -> EvalResult<std::sync::Arc<str>> {
        match self.value(i)? {
            Value::Iri(iri) | Value::Str(iri) => Ok(iri),
            Value::Other { lex, dt } if &*dt == XSD_ANY_URI => Ok(lex),
            _ => Err(TypeError),
        }
    }

    fn unit(&self, i: usize) -> EvalResult<Unit> {
        unit(&self.uri(i)?).ok_or(TypeError)
    }

    fn model(&self) -> DistanceModel {
        self.ctx
            .snap
            .geo
            .as_ref()
            .map_or_else(DistanceModel::default, |v| v.config.distance)
    }

    fn sized(&self, gs: &[&GeomRef]) -> EvalResult<()> {
        let gs: Vec<&Geom> = gs.iter().map(|g| &***g).collect();
        memo::check_op_vertices(self.ctx, &gs)
    }

    /// The datatype of geometry argument `i` (a GeoJSON literal or else WKT).
    fn datatype(&self, i: usize) -> &'static str {
        match self.value(i) {
            Ok(Value::Other { dt, .. }) => write::result_datatype(&dt),
            _ => WKT_LITERAL,
        }
    }

    /// A geometry as a literal of datatype `dt`, charged to the query.
    fn geometry(&self, g: &Geom, dt: &'static str) -> EvalResult<Val> {
        let lex = write::serialize(g, dt).ok_or(TypeError)?;
        match self.ctx.charge(lex.len() as u64 + 64) {
            Ok(c) => std::mem::forget(c),
            Err(_) => return Err(TypeError),
        }
        Ok(Val::V(Value::Other {
            lex: lex.into(),
            dt: dt.into(),
        }))
    }

    /// A latitude and a longitude in range (Jena's bounds check).
    fn lat_lon(&self, lat: usize, lon: usize) -> EvalResult<Coord<f64>> {
        let (lat, lon) = (self.lenient_number(lat)?, self.lenient_number(lon)?);
        if lat.abs() > 90.0 || lon.abs() > 180.0 {
            return Err(TypeError);
        }
        Ok(Coord { x: lon, y: lat })
    }

    fn convert_lat_lon(&self) -> EvalResult<Val> {
        self.arity(2)?;
        let p = self.lat_lon(0, 1)?;
        let g = Geom::from_geometry(CrsRef::Known(EPSG_4326), Geometry::Point(Point(p)));
        self.geometry(&g, WKT_LITERAL)
    }

    /// The box as Jena writes it: from (latMin, lonMin) through (latMax, lonMin).
    fn convert_lat_lon_box(&self) -> EvalResult<Val> {
        self.arity(4)?;
        let lo = self.lat_lon(0, 1)?;
        let hi = self.lat_lon(2, 3)?;
        let ring = vec![
            lo,
            Coord { x: lo.x, y: hi.y },
            hi,
            Coord { x: hi.x, y: lo.y },
            lo,
        ];
        let g = Geom::from_geometry(
            CrsRef::Known(EPSG_4326),
            Geometry::Polygon(Polygon::new(LineString(ring), vec![])),
        );
        self.geometry(&g, WKT_LITERAL)
    }

    fn equals(&self) -> EvalResult<Val> {
        self.arity(2)?;
        let (a, b) = (self.geom(0)?, self.geom(1)?);
        self.sized(&[&a, &b])?;
        boolean(relate::relation(&a, &b, Relation::SfEquals).map_err(op)?)
    }

    fn nearby(&self) -> EvalResult<Val> {
        self.arity(4)?;
        let (a, b) = (self.geom(0)?, self.geom(1)?);
        let radius = self.number(2)?;
        let u = self.unit(3)?;
        self.sized(&[&a, &b])?;
        let d = distance::distance(&a, &b, &u, self.model()).map_err(op)?;
        boolean(d < radius)
    }

    fn distance(&self) -> EvalResult<Val> {
        self.arity(3)?;
        let (a, b) = (self.geom(0)?, self.geom(1)?);
        let u = self.unit(2)?;
        self.sized(&[&a, &b])?;
        double(distance::distance(&a, &b, &u, self.model()).map_err(op)?)
    }

    /// A length unit (great-circle distances are lengths only, as in Jena).
    fn length_unit(&self, i: usize) -> EvalResult<Unit> {
        let u = self.unit(i)?;
        if u.kind == UnitKind::Length {
            Ok(u)
        } else {
            Err(TypeError)
        }
    }

    fn great_circle(&self) -> EvalResult<Val> {
        self.arity(5)?;
        let p = Coord {
            x: self.number(1)?,
            y: self.number(0)?,
        };
        let q = Coord {
            x: self.number(3)?,
            y: self.number(2)?,
        };
        let u = self.length_unit(4)?;
        if p.y.abs() > 90.0 || q.y.abs() > 90.0 {
            return Err(TypeError);
        }
        let m = match self.model() {
            DistanceModel::Geodesic => distance::geodesic(p, q),
            DistanceModel::Haversine => distance::haversine(p, q),
        };
        double(u.from_base(m))
    }

    fn great_circle_geom(&self) -> EvalResult<Val> {
        self.arity(3)?;
        let (a, b) = (self.geom(0)?, self.geom(1)?);
        let u = self.length_unit(2)?;
        self.sized(&[&a, &b])?;
        // a projected first geometry: measure on WGS 84 (Jena transforms to EPSG:4326)
        let a = match ops::is_geographic(&a.crs) {
            Some(true) => std::borrow::Cow::Borrowed(&*a),
            Some(false) => {
                std::borrow::Cow::Owned(ops::transform(&a, &CrsRef::Known(CRS84)).map_err(op)?)
            }
            None => return Err(TypeError),
        };
        double(u.from_base(distance::distance_m(&a, &b, self.model()).map_err(op)?))
    }

    /// A function of four numbers giving a double.
    fn four(&self, f: impl FnOnce([f64; 4]) -> f64) -> EvalResult<Val> {
        self.arity(4)?;
        let v = [
            self.number(0)?,
            self.number(1)?,
            self.number(2)?,
            self.number(3)?,
        ];
        double(f(v))
    }

    /// `transform(g, datatype, srs)` and the forms with one of the two (`None`: keep the
    /// geometry's own).
    fn transform(&self, datatype: Option<usize>, srs: Option<usize>) -> EvalResult<Val> {
        self.arity(1 + usize::from(datatype.is_some()) + usize::from(srs.is_some()))?;
        let g = self.geom(0)?;
        let dt = match datatype {
            None => self.datatype(0),
            Some(i) => match &*self.uri(i)? {
                WKT_LITERAL => WKT_LITERAL,
                GEOJSON_LITERAL => GEOJSON_LITERAL,
                _ => return Err(TypeError),
            },
        };
        let out = match srs {
            None => (*g).clone(),
            Some(i) => {
                let to = CrsRef::Known(crs::lookup(&self.uri(i)?).ok_or(TypeError)?);
                ops::transform(&g, &to).map_err(op)?
            }
        };
        self.geometry(&out, dt)
    }
}

/// The direction from `(x1, y1)` to `(x2, y2)`, clockwise from the y axis, in [0, 2π)
/// radians; 0 for two equal points. (Jena's implementation is off by a quarter turn in
/// the south-east and north-west quadrants; its documented meaning is followed here.)
fn angle(x1: f64, y1: f64, x2: f64, y2: f64) -> f64 {
    (x2 - x1).atan2(y2 - y1).rem_euclid(std::f64::consts::TAU)
}

/// The initial bearing of the great circle from the first point to the second,
/// clockwise from north, in [0, 2π) radians (Jena's formula, on the sphere).
fn azimuth(lat1: f64, lon1: f64, lat2: f64, lon2: f64) -> f64 {
    let (p1, p2) = (lat1.to_radians(), lat2.to_radians());
    let dl = (lon2 - lon1).to_radians();
    let x = p1.cos() * p2.sin() - p1.sin() * p2.cos() * dl.cos();
    let y = dl.sin() * p2.cos();
    y.atan2(x).rem_euclid(std::f64::consts::TAU)
}

/// Rounded half up to 6 decimals, as Jena reports degrees.
fn six_decimals(v: f64) -> f64 {
    (v * 1e6).round() / 1e6
}

fn op(_: ops::OpError) -> TypeError {
    TypeError
}

fn boolean(v: bool) -> EvalResult<Val> {
    Ok(Val::Id(crate::id::Id::from_bool(v)))
}

fn double(v: f64) -> EvalResult<Val> {
    if v.is_finite() {
        Ok(Val::V(Value::Double(v.into())))
    } else {
        Err(TypeError)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn angles_and_azimuths() {
        use std::f64::consts::{FRAC_PI_2, PI};
        // Jena's AngleFFTest / AzimuthFFTest values
        assert!((angle(25.0, 45.0, 75.0, 100.0) - 0.737_815_060_120_464_9).abs() < 1e-15);
        assert_eq!(
            six_decimals(angle(25.0, 45.0, 75.0, 100.0).to_degrees()),
            42.273_689
        );
        assert!((azimuth(0.0, 0.0, 0.0, 10.0) - FRAC_PI_2).abs() < 1e-15);
        // the four quadrants, clockwise from north
        for ((x, y), want) in [
            ((0.0, 1.0), 0.0),
            ((1.0, 0.0), FRAC_PI_2),
            ((0.0, -1.0), PI),
            ((-1.0, 0.0), 3.0 * FRAC_PI_2),
            ((1.0, -1.0), 0.75 * PI),
            ((-1.0, 1.0), 1.75 * PI),
        ] {
            assert!((angle(0.0, 0.0, x, y) - want).abs() < 1e-12, "{x} {y}");
        }
        assert_eq!(angle(1.0, 1.0, 1.0, 1.0), 0.0);
        // due south and due west on the sphere
        assert!((azimuth(10.0, 0.0, 0.0, 0.0) - PI).abs() < 1e-12);
        assert!((azimuth(0.0, 10.0, 0.0, 0.0) - 3.0 * FRAC_PI_2).abs() < 1e-12);
    }
}
