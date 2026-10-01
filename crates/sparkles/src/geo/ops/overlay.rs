//! Overlay: intersection, union, difference and symmetric difference.
//!
//! Each geometry is split into its points, curves and areas. Areas combine by `geo`'s
//! boolean operations, curves against areas by clipping, curves against curves by
//! segment intersection, points by point location. The parts are then assembled
//! without overlaps between dimensions (curves outside the areas, points outside both),
//! as the simplest geometry that holds them. An empty result has the OGC result
//! dimension: the lower one for an intersection, the first geometry's for a
//! difference, the higher one otherwise.

use super::measure::polygons;
use super::{OpError, guarded, in_crs, made};
use crate::geo::geom::{Geom, GeomType};
use georust::line_intersection::{LineIntersection, line_intersection};
use georust::{
    BooleanOps, BoundingRect, Coord, Geometry, GeometryCollection, Intersects, Line, LineString,
    MultiLineString, MultiPoint, MultiPolygon, Point, Polygon, unary_union,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Overlay {
    Intersection,
    Union,
    Difference,
    SymDifference,
}

impl Overlay {
    pub fn name(self) -> &'static str {
        match self {
            Overlay::Intersection => "intersection",
            Overlay::Union => "union",
            Overlay::Difference => "difference",
            Overlay::SymDifference => "symDifference",
        }
    }
}

/// `op(a, b)`, in `a`'s CRS (`b` is transformed into it).
pub fn overlay(a: &Geom, b: &Geom, op: Overlay) -> Result<Geom, OpError> {
    let b = in_crs(b, &a.crs)?;
    let (pa, pb) = (Parts::of(&a.g), Parts::of(&b.g));
    let out = guarded(op.name(), || match op {
        Overlay::Intersection => pa.intersection(&pb),
        Overlay::Union => pa.union(pb.clone()),
        Overlay::Difference => pa.difference(&pb),
        Overlay::SymDifference => pa.difference(&pb).union(pb.difference(&pa)),
    })?;
    let (da, db) = (a.dim().max(0), b.dim().max(0));
    let empty_dim = match op {
        Overlay::Intersection => da.min(db),
        Overlay::Difference => da,
        Overlay::Union | Overlay::SymDifference => da.max(db),
    };
    Ok(match out.assemble() {
        Some(g) => made(a, g),
        None => Geom::empty(
            a.crs.clone(),
            match empty_dim {
                0 => GeomType::Point,
                1 => GeomType::LineString,
                _ => GeomType::Polygon,
            },
        ),
    })
}

/// A geometry as its points, curves (as segments) and areas.
#[derive(Clone, Debug)]
struct Parts {
    points: Vec<Coord<f64>>,
    lines: Vec<Line<f64>>,
    areas: MultiPolygon<f64>,
}

impl Default for Parts {
    fn default() -> Parts {
        Parts {
            points: Vec::new(),
            lines: Vec::new(),
            areas: MultiPolygon(Vec::new()),
        }
    }
}

impl Parts {
    fn of(g: &Geometry<f64>) -> Parts {
        let mut p = Parts::default();
        let mut polys = Vec::new();
        p.add(g, &mut polys);
        p.areas = union_all(polys);
        p
    }

    fn add(&mut self, g: &Geometry<f64>, polys: &mut Vec<Polygon<f64>>) {
        use Geometry as G;
        match g {
            G::Point(p) => self.points.push(p.0),
            G::MultiPoint(m) => self.points.extend(m.0.iter().map(|p| p.0)),
            G::Line(l) => self.lines.push(*l),
            G::LineString(l) => self.lines.extend(l.lines()),
            G::MultiLineString(m) => m.0.iter().for_each(|l| self.lines.extend(l.lines())),
            G::GeometryCollection(c) => c.0.iter().for_each(|m| self.add(m, polys)),
            G::Polygon(_) | G::MultiPolygon(_) | G::Rect(_) | G::Triangle(_) => polygons(g, polys),
        }
    }

    fn intersection(&self, o: &Parts) -> Parts {
        let mut out = Parts {
            areas: self.areas.intersection(&o.areas),
            ..Parts::default()
        };
        // curves against the other's areas and curves
        out.lines.extend(clip(&self.lines, &o.areas, false));
        out.lines.extend(clip(&o.lines, &self.areas, false));
        for s in &self.lines {
            let sb = s.bounding_rect();
            for t in &o.lines {
                if sb.intersects(&t.bounding_rect()) {
                    match line_intersection(*s, *t) {
                        Some(LineIntersection::SinglePoint { intersection, .. }) => {
                            out.points.push(intersection)
                        }
                        Some(LineIntersection::Collinear { intersection }) => {
                            out.lines.push(intersection)
                        }
                        None => {}
                    }
                }
            }
        }
        // points on the other geometry
        let (gs, go) = (self.geometry(), o.geometry());
        out.points
            .extend(self.points.iter().filter(|p| go.intersects(&Point(**p))));
        out.points
            .extend(o.points.iter().filter(|p| gs.intersects(&Point(**p))));
        out
    }

    fn union(mut self, o: Parts) -> Parts {
        self.areas = union_all(self.areas.0.into_iter().chain(o.areas.0).collect());
        // shared stretches once
        let more = subtract_lines(&o.lines, &self.lines);
        self.lines.extend(more);
        self.points.extend(o.points);
        self
    }

    /// `self` minus `o`: removing a lower-dimension part changes nothing (the result is
    /// closed), so only parts of the same or a higher dimension subtract.
    fn difference(&self, o: &Parts) -> Parts {
        let areas = if o.areas.0.is_empty() {
            self.areas.clone()
        } else {
            self.areas.difference(&o.areas)
        };
        let lines = subtract_lines(&clip(&self.lines, &o.areas, true), &o.lines);
        let go = o.geometry();
        Parts {
            points: self
                .points
                .iter()
                .copied()
                .filter(|p| !go.intersects(&Point(*p)))
                .collect(),
            lines,
            areas,
        }
    }

    fn geometry(&self) -> Geometry<f64> {
        let mut gs: Vec<Geometry<f64>> = Vec::new();
        if !self.areas.0.is_empty() {
            gs.push(Geometry::MultiPolygon(self.areas.clone()));
        }
        gs.extend(self.lines.iter().map(|l| Geometry::Line(*l)));
        gs.extend(self.points.iter().map(|p| Geometry::Point(Point(*p))));
        Geometry::GeometryCollection(GeometryCollection(gs))
    }

    /// The simplest geometry of the parts: curves outside the areas, points outside
    /// both, no repeated points (`None`: nothing).
    fn assemble(self) -> Option<Geometry<f64>> {
        let areas = self.areas;
        let lines = merge(clip(&self.lines, &areas, true));
        let mut kept = Parts {
            areas: areas.clone(),
            lines: lines.iter().flat_map(|l| l.lines()).collect(),
            ..Parts::default()
        };
        let g = kept.geometry();
        let mut points: Vec<Coord<f64>> = Vec::new();
        for p in self.points {
            if !points.contains(&p) && !g.intersects(&Point(p)) {
                points.push(p);
            }
        }
        kept.points = points;
        let mut members: Vec<Geometry<f64>> = Vec::new();
        match areas.0.len() {
            0 => {}
            1 => members.push(Geometry::Polygon(areas.0[0].clone())),
            _ => members.push(Geometry::MultiPolygon(areas)),
        }
        match lines.len() {
            0 => {}
            1 => members.push(Geometry::LineString(lines[0].clone())),
            _ => members.push(Geometry::MultiLineString(MultiLineString(lines))),
        }
        match kept.points.len() {
            0 => {}
            1 => members.push(Geometry::Point(Point(kept.points[0]))),
            _ => members.push(Geometry::MultiPoint(MultiPoint(
                kept.points.into_iter().map(Point).collect(),
            ))),
        }
        match members.len() {
            0 => None,
            1 => members.pop(),
            _ => Some(Geometry::GeometryCollection(GeometryCollection(members))),
        }
    }
}

fn union_all(polys: Vec<Polygon<f64>>) -> MultiPolygon<f64> {
    match polys.len() {
        0 => MultiPolygon(vec![]),
        1 => MultiPolygon(polys),
        _ => unary_union(&polys),
    }
}

/// The parts of the segments inside (`invert`: outside) the areas.
fn clip(lines: &[Line<f64>], areas: &MultiPolygon<f64>, invert: bool) -> Vec<Line<f64>> {
    if lines.is_empty() {
        return Vec::new();
    }
    if areas.0.is_empty() {
        return if invert { lines.to_vec() } else { Vec::new() };
    }
    let ls = MultiLineString(
        lines
            .iter()
            .map(|l| LineString(vec![l.start, l.end]))
            .collect(),
    );
    areas
        .clip(&ls, invert)
        .0
        .iter()
        .flat_map(|l| l.lines())
        .collect()
}

/// The segments `a` minus the parts they share with segments `b` (collinear overlaps;
/// crossings remove single points, which changes nothing).
fn subtract_lines(a: &[Line<f64>], b: &[Line<f64>]) -> Vec<Line<f64>> {
    let mut out = Vec::new();
    for s in a {
        let len2 = (s.end.x - s.start.x).powi(2) + (s.end.y - s.start.y).powi(2);
        if len2 == 0.0 {
            continue;
        }
        let at = |c: Coord<f64>| {
            ((c.x - s.start.x) * (s.end.x - s.start.x) + (c.y - s.start.y) * (s.end.y - s.start.y))
                / len2
        };
        let sb = s.bounding_rect();
        let mut cut: Vec<(f64, f64)> = Vec::new();
        for t in b {
            if !sb.intersects(&t.bounding_rect()) {
                continue;
            }
            if let Some(LineIntersection::Collinear { intersection }) = line_intersection(*s, *t) {
                let (u, v) = (at(intersection.start), at(intersection.end));
                cut.push((u.min(v).clamp(0.0, 1.0), u.max(v).clamp(0.0, 1.0)));
            }
        }
        cut.sort_by(|x, y| x.0.total_cmp(&y.0));
        let point = |t: f64| Coord {
            x: s.start.x + (s.end.x - s.start.x) * t,
            y: s.start.y + (s.end.y - s.start.y) * t,
        };
        let mut from = 0.0;
        for (u, v) in cut {
            if u > from {
                out.push(Line::new(point(from), point(u)));
            }
            from = from.max(v);
        }
        if from < 1.0 {
            out.push(Line::new(
                if from == 0.0 { s.start } else { point(from) },
                s.end,
            ));
        }
    }
    out
}

/// Segments joined into line strings where one ends where the next starts.
fn merge(segs: Vec<Line<f64>>) -> Vec<LineString<f64>> {
    let mut out: Vec<LineString<f64>> = Vec::new();
    for s in segs {
        if s.start == s.end {
            continue;
        }
        match out.last_mut() {
            Some(l) if l.0.last() == Some(&s.start) => l.0.push(s.end),
            _ => out.push(LineString(vec![s.start, s.end])),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geo::Relation;
    use crate::geo::ops::relate::relation;

    fn g(s: &str) -> Geom {
        crate::geo::ops::wkt(s)
    }

    fn check(a: &str, b: &str, op: Overlay, want: &str) {
        let r = overlay(&g(a), &g(b), op).unwrap();
        if want.ends_with("EMPTY") {
            assert!(r.empty, "{op:?}({a}, {b}) = {:?}", r.g);
            let t = match want {
                "POINT EMPTY" => GeomType::Point,
                "LINESTRING EMPTY" => GeomType::LineString,
                _ => GeomType::Polygon,
            };
            assert_eq!(r.declared, t, "{op:?}({a}, {b})");
            return;
        }
        assert!(
            relation(&r, &g(want), Relation::SfEquals).unwrap(),
            "{op:?}({a}, {b}) = {:?}, want {want}",
            r.g
        );
    }

    const GA: &str = "POLYGON((0 0, 10 0, 10 10, 0 10, 0 0))";
    const GB: &str = "POLYGON((5 5, 15 5, 15 15, 5 15, 5 5))";
    const GC: &str = "POLYGON((10 0, 20 0, 20 10, 10 10, 10 0))";
    use Overlay::*;

    #[test]
    fn areas() {
        check(GA, GB, Intersection, "POLYGON((5 5,10 5,10 10,5 10,5 5))");
        check(GA, GC, Union, "POLYGON((0 0,20 0,20 10,0 10,0 0))");
        check(
            GA,
            GB,
            Difference,
            "POLYGON((0 0,10 0,10 5,5 5,5 10,0 10,0 0))",
        );
        check(
            GA,
            GB,
            SymDifference,
            "MULTIPOLYGON(((0 0,10 0,10 5,5 5,5 10,0 10,0 0)),((10 5,15 5,15 15,5 15,5 10,10 10,10 5)))",
        );
        check(
            GA,
            "POLYGON((30 30, 31 30, 31 31, 30 30))",
            Intersection,
            "POLYGON EMPTY",
        );
        check(GA, GA, Difference, "POLYGON EMPTY");
    }

    #[test]
    fn mixed_dimensions() {
        check(GA, "POINT(2 2)", Intersection, "POINT(2 2)");
        check(GA, "POINT(20 20)", Intersection, "POINT EMPTY");
        check("POINT(2 2)", GA, Difference, "POINT EMPTY");
        check(GA, "POINT(2 2)", Difference, GA);
        check(
            GA,
            "POINT(20 20)",
            Union,
            &format!("GEOMETRYCOLLECTION({GA}, POINT(20 20))"),
        );
        check(GA, "POINT(2 2)", Union, GA);
        check(
            GA,
            "LINESTRING(-5 5, 5 5)",
            Intersection,
            "LINESTRING(0 5, 5 5)",
        );
        check(
            "LINESTRING(-5 5, 5 5)",
            GA,
            Difference,
            "LINESTRING(-5 5, 0 5)",
        );
        check(
            GA,
            "LINESTRING(-5 5, 5 5)",
            SymDifference,
            &format!("GEOMETRYCOLLECTION({GA}, LINESTRING(-5 5, 0 5))"),
        );
        check(
            "LINESTRING(20 20, 30 30)",
            GA,
            Intersection,
            "LINESTRING EMPTY",
        );
    }

    #[test]
    fn curves() {
        let (x1, x2) = ("LINESTRING(0 0, 2 2)", "LINESTRING(0 2, 2 0)");
        check(x1, x2, Intersection, "POINT(1 1)");
        check(x1, x2, Union, "MULTILINESTRING((0 0, 2 2),(0 2, 2 0))");
        check(x1, x2, Difference, x1);
        let (s1, s2) = ("LINESTRING(0 0, 2 0)", "LINESTRING(1 0, 3 0)");
        check(s1, s2, Intersection, "LINESTRING(1 0, 2 0)");
        check(s1, s2, Difference, "LINESTRING(0 0, 1 0)");
        check(
            s1,
            s2,
            SymDifference,
            "MULTILINESTRING((0 0, 1 0),(2 0, 3 0))",
        );
        check(s1, s2, Union, "LINESTRING(0 0, 3 0)");
        check(s1, s1, Difference, "LINESTRING EMPTY");
    }

    #[test]
    fn points() {
        let (m1, m2) = ("MULTIPOINT((1 1),(2 2))", "MULTIPOINT((2 2),(3 3))");
        check(m1, m2, Intersection, "POINT(2 2)");
        check(m1, m2, Union, "MULTIPOINT((1 1),(2 2),(3 3))");
        check(m1, m2, Difference, "POINT(1 1)");
        check(m1, m2, SymDifference, "MULTIPOINT((1 1),(3 3))");
        check(
            "POINT(1 0)",
            "LINESTRING(0 0, 2 0)",
            Intersection,
            "POINT(1 0)",
        );
    }

    #[test]
    fn crs_and_empties() {
        let mars = crate::geo::ops::wkt(&format!("<http://example.org/crs/mars> {}", GA));
        assert!(overlay(&mars, &g(GB), Intersection).is_err());
        assert!(overlay(&mars, &mars, Intersection).is_ok());
        check(GA, "POLYGON EMPTY", Union, GA);
        check(GA, "POLYGON EMPTY", Intersection, "POLYGON EMPTY");
        check("POLYGON EMPTY", GA, Difference, "POLYGON EMPTY");
    }
}
