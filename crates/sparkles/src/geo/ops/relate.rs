//! Topological relations: the 24 GeoSPARQL relations and DE-9IM pattern matching.
//!
//! The matrix comes from `geo`'s `Relate`, planar in the coordinates of the first
//! geometry's CRS (longitude/latitude for geographic CRSs). Each relation is a set of
//! DE-9IM patterns, with these readings of the standard's tables:
//!
//! * `sfEquals` and `ehEquals` are topological equality (`T*F**FFF*`), so two equal
//!   points are equal (the printed `TFFFTFFFT` would need a boundary a point lacks);
//! * `sfDisjoint`/`ehDisjoint` is `FF*FF****`, and `sfIntersects` its negation;
//! * `sfTouches`/`ehMeet` is false for two points or multipoints;
//! * `sfOverlaps` holds between geometries of the same dimension only (`1*T***T**` for
//!   two curves), `sfCrosses` between a lower-dimension and a higher-dimension
//!   geometry (`T*T***T**`) or two curves (`0********`), and is false otherwise;
//! * RCC8 relations hold between regions only: false when either side is not areal;
//! * an empty geometry is disjoint from everything (`sfDisjoint`, `ehDisjoint`) and in no
//!   other relation (`rcc8dc` included, since it is not a region).

use super::{OpError, guarded, in_crs, type_error};
use crate::geo::geom::Geom;
use crate::geo::{GeomRef, Relation};
use georust::dimensions::Dimensions;
use georust::relate::IntersectionMatrix;
use georust::{BoundingRect, Geometry, HasDimensions, Intersects, PreparedGeometry, Rect, Relate};

/// Whether `r(a, b)` holds (`b` is transformed into `a`'s CRS).
pub fn relation(a: &Geom, b: &Geom, r: Relation) -> Result<bool, OpError> {
    let b = in_crs(b, &a.crs)?;
    let (fa, fb) = (Facts::of(a), Facts::of(&b));
    if let Some(v) = decided(&fa, &fb, r) {
        return Ok(v);
    }
    let im = guarded(r.local(), || a.g.relate(&b.g))?;
    Ok(holds(&im, r, fa.dim, fb.dim))
}

/// Whether the DE-9IM matrix of `(a, b)` matches `pattern` (9 characters of `TF*012`,
/// case-insensitive; anything else is a type error).
pub fn relate(a: &Geom, b: &Geom, pattern: &str) -> Result<bool, OpError> {
    check_pattern(pattern)?;
    let b = in_crs(b, &a.crs)?;
    let im = guarded("relate", || a.g.relate(&b.g))?;
    matches(&im, pattern)
}

/// Whether every match of `pattern` needs the geometries to intersect (one of the
/// II, IB, BI, BB entries is `T`, `0`, `1` or `2`), so the index can find candidates.
pub fn pattern_needs_intersection(pattern: &str) -> Result<bool, OpError> {
    check_pattern(pattern)?;
    Ok([0, 1, 3, 4]
        .into_iter()
        .any(|i| !matches!(pattern.as_bytes()[i], b'*' | b'F' | b'f')))
}

/// A geometry prepared for many tests against other geometries (an edge index built
/// once). It is not `Send`: build one per thread.
pub struct Prepared {
    g: GeomRef,
    /// `None` for an empty geometry, or when preparing failed (tests then use the
    /// plain geometry)
    prep: Option<PreparedGeometry<'static, Geometry<f64>>>,
    facts: Facts,
}

impl Prepared {
    pub fn new(g: GeomRef) -> Prepared {
        let prep = if g.empty {
            None
        } else {
            guarded("prepare", || PreparedGeometry::from(g.g.clone())).ok()
        };
        let facts = Facts::of(&g);
        Prepared { g, prep, facts }
    }

    pub fn geom(&self) -> &GeomRef {
        &self.g
    }

    fn matrix(&self, b: &Geom, what: &str) -> Result<IntersectionMatrix, OpError> {
        match &self.prep {
            Some(p) => guarded(what, || p.relate(&b.g)),
            None => guarded(what, || self.g.g.relate(&b.g)),
        }
    }

    /// `r(self, b)`.
    pub fn relation(&self, b: &Geom, r: Relation) -> Result<bool, OpError> {
        let b = in_crs(b, &self.g.crs)?;
        let fb = Facts::of(&b);
        if let Some(v) = decided(&self.facts, &fb, r) {
            return Ok(v);
        }
        let im = self.matrix(&b, r.local())?;
        Ok(holds(&im, r, self.facts.dim, fb.dim))
    }

    /// The DE-9IM matrix of `(self, b)` against `pattern`.
    pub fn relate(&self, b: &Geom, pattern: &str) -> Result<bool, OpError> {
        check_pattern(pattern)?;
        let b = in_crs(b, &self.g.crs)?;
        let im = self.matrix(&b, "relate")?;
        matches(&im, pattern)
    }
}

fn check_pattern(p: &str) -> Result<(), OpError> {
    if p.len() == 9
        && p.bytes()
            .all(|c| matches!(c, b'T' | b't' | b'F' | b'f' | b'*' | b'0' | b'1' | b'2'))
    {
        Ok(())
    } else {
        Err(type_error(format!(
            "relate: {p:?} is not a DE-9IM pattern (9 characters of T, F, *, 0, 1, 2)"
        )))
    }
}

fn matches(im: &IntersectionMatrix, pattern: &str) -> Result<bool, OpError> {
    im.matches(pattern).map_err(|e| type_error(e.to_string()))
}

/// Topological dimension (the highest of a collection's members); -1 when empty.
fn dim(g: &Geometry<f64>) -> i8 {
    match g.dimensions() {
        Dimensions::Empty => -1,
        Dimensions::ZeroDimensional => 0,
        Dimensions::OneDimensional => 1,
        Dimensions::TwoDimensional => 2,
    }
}

/// A region: polygons, or a collection of nothing but polygons.
fn areal(g: &Geometry<f64>) -> bool {
    match g {
        Geometry::Polygon(_) | Geometry::MultiPolygon(_) | Geometry::Rect(_) => true,
        Geometry::Triangle(_) => true,
        Geometry::GeometryCollection(c) => c.0.iter().all(|m| m.is_empty() || areal(m)),
        _ => false,
    }
}

/// What the relations need to know of a geometry besides its matrix.
#[derive(Clone, Copy, Debug)]
struct Facts {
    empty: bool,
    areal: bool,
    dim: i8,
    rect: Option<Rect<f64>>,
}

impl Facts {
    fn of(g: &Geom) -> Facts {
        Facts {
            empty: g.empty,
            areal: areal(&g.g),
            dim: dim(&g.g),
            rect: g.g.bounding_rect(),
        }
    }
}

/// The answer when it does not need the matrix: empty geometries, RCC8 relations of
/// non-regions, dimension pairs a relation excludes, disjoint envelopes.
fn decided(a: &Facts, b: &Facts, r: Relation) -> Option<bool> {
    use Relation::*;
    let disjoint = matches!(r, SfDisjoint | EhDisjoint | Rcc8Dc);
    if a.empty || b.empty {
        return Some(matches!(r, SfDisjoint | EhDisjoint));
    }
    if r.areal_only() && !(a.areal && b.areal) {
        return Some(false);
    }
    let (da, db) = (a.dim, b.dim);
    let excluded = match r {
        SfTouches | EhMeet => da == 0 && db == 0,
        SfOverlaps => da != db || da == -1,
        SfCrosses => !(da < db || (da == 1 && db == 1)),
        _ => false,
    };
    if excluded {
        return Some(false);
    }
    match (a.rect, b.rect) {
        (Some(ra), Some(rb)) if !ra.intersects(&rb) => Some(disjoint),
        _ => None,
    }
}

/// Whether the matrix satisfies relation `r` between geometries of dimensions `da`,
/// `db` (after [`decided`] found no answer).
fn holds(im: &IntersectionMatrix, r: Relation, da: i8, db: i8) -> bool {
    use Relation::*;
    let m = |p: &str| im.matches(p).unwrap_or(false);
    match r {
        SfEquals | EhEquals => m("T*F**FFF*"),
        SfDisjoint | EhDisjoint => m("FF*FF****"),
        SfIntersects => !m("FF*FF****"),
        SfTouches | EhMeet => m("FT*******") || m("F**T*****") || m("F***T****"),
        SfWithin => m("T*F**F***"),
        SfContains => m("T*****FF*"),
        SfOverlaps if da == 1 && db == 1 => m("1*T***T**"),
        SfOverlaps | EhOverlap => m("T*T***T**"),
        SfCrosses if da == 1 && db == 1 => m("0********"),
        SfCrosses => m("T*T***T**"),
        EhCovers => m("T*TFT*FF*"),
        EhCoveredBy => m("TFF*TFT**"),
        EhInside => m("TFF*FFT**"),
        EhContains => m("T*TFF*FF*"),
        Rcc8Eq => m("TFFFTFFFT"),
        Rcc8Dc => m("FFTFFTTTT"),
        Rcc8Ec => m("FFTFTTTTT"),
        Rcc8Po => m("TTTTTTTTT"),
        Rcc8Tppi => m("TTTFTTFFT"),
        Rcc8Tpp => m("TFFTTFTTT"),
        Rcc8Ntpp => m("TFFTFFTTT"),
        Rcc8Ntppi => m("TTTFFTFFT"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn g(s: &str) -> Geom {
        crate::geo::ops::wkt(s)
    }

    fn rel(a: &str, b: &str, r: &str) -> bool {
        let r = Relation::from_local(r).unwrap();
        let v = relation(&g(a), &g(b), r).unwrap();
        // the prepared form agrees
        let p = Prepared::new(Arc::new(g(a)));
        assert_eq!(p.relation(&g(b), r).unwrap(), v, "prepared {r:?}({a}, {b})");
        v
    }

    const GA: &str = "POLYGON((0 0, 10 0, 10 10, 0 10, 0 0))";
    const GB: &str = "POLYGON((5 5, 15 5, 15 15, 5 15, 5 5))";
    const GC: &str = "POLYGON((10 0, 20 0, 20 10, 10 10, 10 0))";

    #[test]
    fn acceptance_relations() {
        for (r, b) in [
            ("sfTouches", GC),
            ("sfIntersects", GC),
            ("sfOverlaps", GB),
            ("sfContains", "POINT(2 2)"),
            ("rcc8ec", GC),
            ("rcc8po", GB),
            ("ehCovers", "LINESTRING(0 1, 5 1)"),
        ] {
            assert!(rel(GA, b, r), "{r}(gA, {b})");
        }
        assert!(rel("POINT(1 1)", "POINT(1.0 1.0)", "sfEquals"));
        assert!(!rel(GA, "LINESTRING(1 1, 5 1)", "ehCovers"));
        assert!(!rel(GA, GC, "sfOverlaps"));
        assert!(!rel(GA, "POINT(10 5)", "rcc8ec"));
        assert!(!rel("POINT(1 1)", "POINT(1 2)", "sfEquals"));
    }

    #[test]
    fn relate_patterns() {
        let (a, b) = (g(GA), g(GB));
        assert!(relate(&a, &b, "212101212").unwrap());
        assert!(relate(&a, &b, "t*t***t**").unwrap());
        assert!(!relate(&a, &b, "FF*FF****").unwrap());
        for bad in [
            "TT",
            "",
            "T*T***T**T",
            "T*T***T*X",
            "T*T***T*é",
            "T*T***T* ",
        ] {
            assert!(relate(&a, &b, bad).is_err(), "{bad:?}");
            assert!(pattern_needs_intersection(bad).is_err(), "{bad:?}");
        }
        // empty geometries are related too: everything F but EE
        let e = g("POLYGON EMPTY");
        assert!(relate(&e, &a, "FFFFFF212").unwrap());
        assert!(relate(&e, &e, "FFFFFFFF2").unwrap());
        assert!(pattern_needs_intersection("T********").unwrap());
        assert!(pattern_needs_intersection("***0*****").unwrap());
        assert!(pattern_needs_intersection("F***1****").unwrap());
        assert!(!pattern_needs_intersection("FF*FF****").unwrap());
        assert!(!pattern_needs_intersection("**T***T**").unwrap());
        assert!(!pattern_needs_intersection("*********").unwrap());
    }

    #[test]
    fn empty_geometries() {
        for e in [
            "POINT EMPTY",
            "LINESTRING EMPTY",
            "POLYGON EMPTY",
            "GEOMETRYCOLLECTION EMPTY",
        ] {
            for other in [GA, "POINT(1 1)", e] {
                for r in Relation::ALL {
                    let want = matches!(r, Relation::SfDisjoint | Relation::EhDisjoint);
                    assert_eq!(rel(e, other, r.local()), want, "{r:?}({e}, {other})");
                    assert_eq!(rel(other, e, r.local()), want, "{r:?}({other}, {e})");
                }
            }
        }
    }

    #[test]
    fn equal_points_and_crossing_lines() {
        for r in [
            "sfEquals",
            "ehEquals",
            "sfWithin",
            "sfContains",
            "sfIntersects",
        ] {
            assert!(rel("POINT(3 4)", "POINT(3 4)", r), "{r}");
        }
        assert!(!rel("POINT(3 4)", "POINT(3 4)", "sfTouches"));
        assert!(!rel("POINT(3 4)", "POINT(3 4)", "ehMeet"));
        assert!(rel(
            "MULTIPOINT((1 1),(2 2))",
            "MULTIPOINT((2 2),(1 1))",
            "sfEquals"
        ));
        assert!(!rel("POINT(3 4)", "POINT(3 4)", "rcc8eq"));
        // two lines crossing at a point
        let (x1, x2) = ("LINESTRING(0 0, 2 2)", "LINESTRING(0 2, 2 0)");
        assert!(rel(x1, x2, "sfCrosses"));
        assert!(!rel(x1, x2, "sfOverlaps"));
        // sharing a segment: overlap, not a crossing
        let (s1, s2) = ("LINESTRING(0 0, 2 0)", "LINESTRING(1 0, 3 0)");
        assert!(!rel(s1, s2, "sfCrosses"));
        assert!(rel(s1, s2, "sfOverlaps"));
        // meeting at an end point: touching, not crossing
        assert!(!rel(
            "LINESTRING(0 0, 1 1)",
            "LINESTRING(1 1, 2 0)",
            "sfCrosses"
        ));
        assert!(rel(
            "LINESTRING(0 0, 1 1)",
            "LINESTRING(1 1, 2 0)",
            "sfTouches"
        ));
        // a line through an area crosses it, but not the other way round
        let (l, a) = ("LINESTRING(-5 5, 5 5)", GA);
        assert!(rel(l, a, "sfCrosses"));
        assert!(!rel(a, l, "sfCrosses"));
        assert!(rel("MULTIPOINT((1 1),(20 20))", GA, "sfCrosses"));
        assert!(!rel(GA, GB, "sfCrosses"));
        // overlaps needs equal dimensions
        assert!(rel(
            "MULTIPOINT((1 1),(2 2))",
            "MULTIPOINT((2 2),(3 3))",
            "sfOverlaps"
        ));
        assert!(!rel("LINESTRING(-5 5, 5 5)", GA, "sfOverlaps"));
        assert!(rel("LINESTRING(-5 5, 5 5)", GA, "ehOverlap"));
    }

    #[test]
    fn rcc8_needs_regions() {
        for r in Relation::ALL.into_iter().filter(|r| r.areal_only()) {
            for other in ["POINT(5 5)", "LINESTRING(0 0, 20 20)", "POINT(50 50)"] {
                assert!(!rel(GA, other, r.local()), "{r:?}");
                assert!(!rel(other, GA, r.local()), "{r:?}");
            }
        }
        assert!(rel(GA, "POLYGON((30 30, 31 30, 31 31, 30 30))", "rcc8dc"));
        assert!(rel(GA, GA, "rcc8eq"));
        assert!(rel("POLYGON((1 1, 2 1, 2 2, 1 1))", GA, "rcc8ntpp"));
        assert!(rel(GA, "POLYGON((0 0, 2 0, 2 2, 0 0))", "rcc8tppi"));
        assert!(rel(
            "GEOMETRYCOLLECTION(POLYGON((1 1, 2 1, 2 2, 1 1)))",
            GA,
            "rcc8ntpp"
        ));
        assert!(!rel(
            "GEOMETRYCOLLECTION(POINT(1 1), POLYGON((1 1, 2 1, 2 2, 1 1)))",
            GA,
            "rcc8ntpp"
        ));
    }

    #[test]
    fn unknown_crs() {
        let (m, p) = (
            g("<http://example.org/crs/mars> POINT(1 1)"),
            g("POINT(1 1)"),
        );
        assert!(relation(&m, &m, Relation::SfEquals).unwrap());
        assert!(relation(&m, &p, Relation::SfEquals).is_err());
        assert!(relation(&p, &m, Relation::SfIntersects).is_err());
        assert!(relate(&p, &m, "T********").is_err());
        assert!(
            Prepared::new(Arc::new(m.clone()))
                .relation(&p, Relation::SfDisjoint)
                .is_err()
        );
    }

    /// Every relation over pairs of points, curves and areas, against its definition
    /// as DE-9IM patterns and dimension conditions.
    #[test]
    fn relations_against_their_patterns() {
        let shapes = [
            "POINT(2 2)",
            "POINT(0 5)",
            "POINT(10 10)",
            "POINT(30 30)",
            "MULTIPOINT((2 2),(30 30))",
            "MULTIPOINT((1 1),(2 2))",
            "LINESTRING(1 1, 4 4)",
            "LINESTRING(-5 5, 5 5)",
            "LINESTRING(0 0, 10 0)",
            "LINESTRING(0 0, 5 0)",
            "LINESTRING(2 0, 2 20)",
            "LINESTRING(0 10, 20 10)",
            "LINESTRING(30 30, 40 40)",
            "MULTILINESTRING((1 1, 4 4),(-5 5, 5 5))",
            GA,
            GB,
            GC,
            "POLYGON((1 1, 4 1, 4 4, 1 4, 1 1))",
            "POLYGON((0 0, 5 0, 5 5, 0 5, 0 0))",
            "POLYGON((30 30, 40 30, 40 40, 30 30))",
            "POLYGON((-5 -5, 15 -5, 15 15, -5 15, -5 -5),(1 1, 2 1, 2 2, 1 2, 1 1))",
            "MULTIPOLYGON(((1 1, 2 1, 2 2, 1 1)),((30 30, 31 30, 31 31, 30 30)))",
        ];
        let any = |a: &Geom, b: &Geom, ps: &[&str]| ps.iter().any(|p| relate(a, b, p).unwrap());
        let mut tested = 0;
        for sa in shapes {
            for sb in shapes {
                let (a, b) = (g(sa), g(sb));
                let (da, db) = (dim(&a.g), dim(&b.g));
                let regions = da == 2 && db == 2;
                for r in Relation::ALL {
                    use Relation::*;
                    let want = match r {
                        SfEquals | EhEquals => any(&a, &b, &["T*F**FFF*"]),
                        SfDisjoint | EhDisjoint => any(&a, &b, &["FF*FF****"]),
                        SfIntersects => any(
                            &a,
                            &b,
                            &["T********", "*T*******", "***T*****", "****T****"],
                        ),
                        SfTouches | EhMeet => {
                            !(da == 0 && db == 0)
                                && any(&a, &b, &["FT*******", "F**T*****", "F***T****"])
                        }
                        SfWithin => any(&a, &b, &["T*F**F***"]),
                        SfContains => any(&a, &b, &["T*****FF*"]),
                        SfOverlaps => match (da, db) {
                            (0, 0) | (2, 2) => any(&a, &b, &["T*T***T**"]),
                            (1, 1) => any(&a, &b, &["1*T***T**"]),
                            _ => false,
                        },
                        SfCrosses => match (da, db) {
                            (0, 1) | (0, 2) | (1, 2) => any(&a, &b, &["T*T***T**"]),
                            (1, 1) => any(&a, &b, &["0********"]),
                            _ => false,
                        },
                        EhOverlap => any(&a, &b, &["T*T***T**"]),
                        EhCovers => any(&a, &b, &["T*TFT*FF*"]),
                        EhCoveredBy => any(&a, &b, &["TFF*TFT**"]),
                        EhInside => any(&a, &b, &["TFF*FFT**"]),
                        EhContains => any(&a, &b, &["T*TFF*FF*"]),
                        Rcc8Eq => regions && any(&a, &b, &["TFFFTFFFT"]),
                        Rcc8Dc => regions && any(&a, &b, &["FFTFFTTTT"]),
                        Rcc8Ec => regions && any(&a, &b, &["FFTFTTTTT"]),
                        Rcc8Po => regions && any(&a, &b, &["TTTTTTTTT"]),
                        Rcc8Tppi => regions && any(&a, &b, &["TTTFTTFFT"]),
                        Rcc8Tpp => regions && any(&a, &b, &["TFFTTFTTT"]),
                        Rcc8Ntpp => regions && any(&a, &b, &["TFFTFFTTT"]),
                        Rcc8Ntppi => regions && any(&a, &b, &["TTTFFTFFT"]),
                    };
                    assert_eq!(rel(sa, sb, r.local()), want, "{r:?}({sa}, {sb})");
                    // the converse relation with the arguments swapped
                    if let Some(c) = r.converse() {
                        assert_eq!(rel(sb, sa, c.local()), want, "{c:?}({sb}, {sa})");
                    }
                    tested += 1;
                }
            }
        }
        assert_eq!(tested, 24 * shapes.len() * shapes.len());
    }
}
