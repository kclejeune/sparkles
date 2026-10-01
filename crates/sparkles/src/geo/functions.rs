//! The `geof:` functions in SPARQL expressions (the `spatialF:` ones come later).
//!
//! Every argument error is a SPARQL type error: an argument that is not a geometry
//! literal (or is ill-typed), an unknown unit, CRSs without a transform between them, an
//! operation over `maxOpVertices`.

use super::DistanceModel;
use super::ops::{self, OpError, distance, relate};
use super::units::{Unit, unit};
use super::vocab::{GEOF, Relation};
use super::{GeomRef, memo};
use crate::sparql::ctx::Ctx;
use crate::sparql::expr::{Expr, Row, Val, arg};
use crate::sparql::value::{EvalResult, TypeError, Value};

/// `xsd:anyURI`, the datatype of unit arguments given as literals.
const XSD_ANY_URI: &str = "http://www.w3.org/2001/XMLSchema#anyURI";

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

    /// A unit: an IRI or an `xsd:anyURI` literal naming a known unit.
    fn unit(&self, i: usize) -> EvalResult<Unit> {
        match self.value(i)? {
            Value::Iri(iri) => unit(&iri),
            Value::Other { lex, dt } if &*dt == XSD_ANY_URI => unit(&lex),
            _ => None,
        }
        .ok_or(TypeError)
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
        let gs: Vec<&super::Geom> = gs.iter().map(|g| &***g).collect();
        ops::check_vertices(ops::DEFAULT_OP_VERTICES, &gs).map_err(op)
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
        let d = match unit_arg {
            Some(i) => distance::distance(&a, &b, &self.unit(i)?, self.model()),
            None => distance::distance_m(&a, &b, self.model()),
        };
        double(d.map_err(op)?)
    }
}

fn op(_: OpError) -> TypeError {
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
