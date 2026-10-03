//! Plan-time checks of the GeoSPARQL parts of a query: malformed geometry constants are
//! a `400` (a constant that can never evaluate is reported, not silently false), and a
//! build without the `geo` feature warns about `geof:` calls.
//!
//! Only literals that are arguments of `geof:` (or `spatialF:`) calls are checked:
//! geometry literals in triple patterns match data, and ill-typed data never errors.

use crate::error::Result;
use crate::sparql::ctx::PlanWarning;
use spargebra::algebra::{AggregateExpression, Expression, GraphPattern, OrderExpression};

/// Check the query (or update WHERE clause) `gp`; warnings go to `warn`.
pub fn validate_query(gp: &GraphPattern, warn: &mut dyn FnMut(PlanWarning)) -> Result<()> {
    let mut v = Visitor {
        warn,
        warned: false,
    };
    v.pattern(gp)
}

struct Visitor<'a> {
    warn: &'a mut dyn FnMut(PlanWarning),
    warned: bool,
}

impl Visitor<'_> {
    fn pattern(&mut self, gp: &GraphPattern) -> Result<()> {
        use GraphPattern as GP;
        match gp {
            GP::Bgp { .. } | GP::Path { .. } | GP::Values { .. } => Ok(()),
            GP::Join { left, right }
            | GP::Lateral { left, right }
            | GP::Union { left, right }
            | GP::Minus { left, right } => {
                self.pattern(left)?;
                self.pattern(right)
            }
            GP::LeftJoin {
                left,
                right,
                expression,
            } => {
                self.pattern(left)?;
                self.pattern(right)?;
                expression.iter().try_for_each(|e| self.expr(e, false))
            }
            GP::Filter { expr, inner } => {
                self.expr(expr, false)?;
                self.pattern(inner)
            }
            GP::Extend {
                inner, expression, ..
            }
            | GP::Assign {
                inner, expression, ..
            }
            | GP::Unfold {
                inner, expression, ..
            } => {
                self.expr(expression, false)?;
                self.pattern(inner)
            }
            GP::OrderBy { inner, expression } => {
                for o in expression {
                    let (OrderExpression::Asc(e) | OrderExpression::Desc(e)) = o;
                    self.expr(e, false)?;
                }
                self.pattern(inner)
            }
            GP::Group {
                inner, aggregates, ..
            } => {
                for (_, a) in aggregates {
                    if let AggregateExpression::FunctionCall { name, expr, .. } = a {
                        // the GeoSPARQL aggregates take a geometry
                        let geo = match name {
                            spargebra::algebra::AggregateFunction::Custom(iri) => {
                                self.geo_call(iri.as_str())
                            }
                            _ => false,
                        };
                        self.expr(expr, geo)?;
                    }
                }
                self.pattern(inner)
            }
            GP::Graph { inner, .. }
            | GP::Project { inner, .. }
            | GP::Distinct { inner }
            | GP::Reduced { inner }
            | GP::Slice { inner, .. }
            | GP::Service { inner, .. } => self.pattern(inner),
            #[allow(unreachable_patterns)]
            _ => Ok(()),
        }
    }

    /// `e`, an argument of a GeoSPARQL function when `geo_arg`.
    fn expr(&mut self, e: &Expression, geo_arg: bool) -> Result<()> {
        use Expression as E;
        match e {
            E::Literal(l) if geo_arg => check_literal(l),
            E::Literal(_) | E::NamedNode(_) | E::Variable(_) | E::Bound(_) => Ok(()),
            E::Or(a, b)
            | E::And(a, b)
            | E::Equal(a, b)
            | E::SameTerm(a, b)
            | E::Greater(a, b)
            | E::GreaterOrEqual(a, b)
            | E::Less(a, b)
            | E::LessOrEqual(a, b)
            | E::Add(a, b)
            | E::Subtract(a, b)
            | E::Multiply(a, b)
            | E::Divide(a, b) => {
                self.expr(a, false)?;
                self.expr(b, false)
            }
            E::UnaryPlus(a) | E::UnaryMinus(a) | E::Not(a) => self.expr(a, false),
            E::In(a, l) => {
                self.expr(a, false)?;
                l.iter().try_for_each(|x| self.expr(x, false))
            }
            E::If(a, b, c) => {
                self.expr(a, false)?;
                self.expr(b, false)?;
                self.expr(c, false)
            }
            E::Coalesce(l) => l.iter().try_for_each(|x| self.expr(x, false)),
            E::Exists(p) => self.pattern(p),
            E::FunctionCall(f, args) => {
                let geo = match f {
                    spargebra::algebra::Function::Custom(iri) => self.geo_call(iri.as_str()),
                    _ => false,
                };
                args.iter().try_for_each(|x| self.expr(x, geo))
            }
            #[allow(unreachable_patterns)]
            _ => Ok(()),
        }
    }

    /// Whether `iri` is a GeoSPARQL (or Jena spatial) function or aggregate; the first
    /// one in a build without the feature adds the warning.
    fn geo_call(&mut self, iri: &str) -> bool {
        let geo = iri.starts_with(super::vocab::GEOF) || iri.starts_with(super::vocab::SPATIALF);
        if geo && !cfg!(feature = "geo") && !self.warned {
            self.warned = true;
            (self.warn)(PlanWarning {
                code: "geo-not-built",
                message: "geof:* needs cargo feature \"geo\": the calls are unknown \
                          functions (errors) in this build"
                    .into(),
            });
        }
        geo
    }
}

/// A geometry literal argument must parse.
fn check_literal(l: &oxrdf::Literal) -> Result<()> {
    let dt = l.datatype().as_str();
    if !super::vocab::is_geometry_datatype(dt) {
        return Ok(());
    }
    #[cfg(feature = "geo")]
    if let Err(e) = super::parse(l.value(), dt) {
        return Err(crate::error::Error::invalid(super::exec::malformed(dt, &e)));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check(q: &str) -> (Result<()>, Vec<PlanWarning>) {
        let q = spargebra::SparqlParser::new()
            .with_prefix("geo", super::super::vocab::GEO)
            .unwrap()
            .with_prefix("geof", super::super::vocab::GEOF)
            .unwrap()
            .parse_query(q)
            .unwrap();
        let spargebra::Query::Select { pattern, .. } = q else {
            panic!()
        };
        let mut ws = Vec::new();
        let r = validate_query(&pattern, &mut |w| ws.push(w));
        (r, ws)
    }

    #[test]
    fn literals_outside_geof_calls_are_data() {
        // a pattern constant matches data; an equality compares terms
        let (r, ws) = check(
            "SELECT * { ?g geo:asWKT \"POINT(1\"^^geo:wktLiteral \
             FILTER(?w = \"POINT(1\"^^geo:wktLiteral) }",
        );
        assert!(r.is_ok());
        assert!(ws.is_empty());
    }

    #[test]
    #[cfg(not(feature = "geo"))]
    fn geof_calls_warn_once_without_the_feature() {
        let (r, ws) = check(
            "SELECT * { ?g geo:asWKT ?w FILTER(geof:sfWithin(?w, ?v) && geof:sfContains(?v, ?w)) \
             BIND(geof:area(?w) AS ?a) }",
        );
        assert!(r.is_ok());
        assert_eq!(ws.len(), 1);
        assert_eq!(ws[0].code, "geo-not-built");
    }
}
