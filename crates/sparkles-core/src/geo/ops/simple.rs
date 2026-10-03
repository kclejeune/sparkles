//! OGC simplicity (`geof:isSimple`): a geometry without anomalous points such as
//! self-intersections. `geo`'s `Validation` answers validity, a different question, so
//! this is a sweep of our own over the segments (an R-tree of their boxes).
//!
//! * points: always simple; multipoints when no two points are equal;
//! * curves: no two segments meet except consecutive ones at their shared vertex, and
//!   the first and last segment of a closed curve at the closing vertex (repeated
//!   consecutive vertices are ignored, as in JTS);
//! * multicurves: every member simple, and two members meet only at points that end
//!   both of them (closed members have no ends);
//! * polygons: every ring a simple closed curve;
//! * collections: every member simple. Empty geometries are simple.

use geo_index::rtree::sort::HilbertSort;
use geo_index::rtree::{RTreeBuilder, RTreeIndex};
use georust::line_intersection::{LineIntersection, line_intersection};
use georust::{Coord, Geometry, Line, LineString};

/// Below this many segments every pair is tested directly.
const BRUTE_FORCE: usize = 32;

/// Is `g` simple in the OGC sense?
pub fn is_simple(g: &Geometry<f64>) -> bool {
    use Geometry as G;
    match g {
        G::Point(_) => true,
        G::MultiPoint(m) => {
            let mut pts: Vec<Coord<f64>> = m.0.iter().map(|p| p.0).collect();
            pts.sort_by(|a, b| a.x.total_cmp(&b.x).then(a.y.total_cmp(&b.y)));
            pts.windows(2).all(|w| w[0] != w[1])
        }
        G::Line(_) => true,
        G::LineString(l) => curves_simple(std::slice::from_ref(l)),
        G::MultiLineString(m) => curves_simple(&m.0),
        G::Polygon(p) => polygon_simple(p),
        G::MultiPolygon(m) => m.0.iter().all(polygon_simple),
        G::Rect(_) | G::Triangle(_) => true,
        G::GeometryCollection(c) => c.0.iter().all(is_simple),
    }
}

/// Every ring of `p` is a simple closed curve.
fn polygon_simple(p: &georust::Polygon<f64>) -> bool {
    std::iter::once(p.exterior())
        .chain(p.interiors())
        .all(|r| r.0.is_empty() || curves_simple(std::slice::from_ref(r)))
}

/// One segment of a curve: the curve and the position in it.
#[derive(Clone, Copy)]
struct Seg {
    line: Line<f64>,
    curve: usize,
    index: usize,
}

/// A curve's vertices without repeated consecutive points.
fn vertices(l: &LineString<f64>) -> Vec<Coord<f64>> {
    let mut v = l.0.clone();
    v.dedup();
    v
}

/// The curves are simple each and meet one another only at shared ends.
fn curves_simple(curves: &[LineString<f64>]) -> bool {
    let curves: Vec<Vec<Coord<f64>>> = curves.iter().map(vertices).collect();
    let closed: Vec<bool> = curves
        .iter()
        .map(|c| c.len() > 2 && c.first() == c.last())
        .collect();
    let mut segs: Vec<Seg> = Vec::new();
    for (curve, c) in curves.iter().enumerate() {
        for (index, w) in c.windows(2).enumerate() {
            segs.push(Seg {
                line: Line::new(w[0], w[1]),
                curve,
                index,
            });
        }
    }
    let allowed = |a: &Seg, b: &Seg, x: &LineIntersection<f64>| -> bool {
        // the single point where the two segments meet, if they meet in one point
        let at = match *x {
            LineIntersection::SinglePoint { intersection, .. } => intersection,
            LineIntersection::Collinear { intersection }
                if intersection.start == intersection.end =>
            {
                intersection.start
            }
            LineIntersection::Collinear { .. } => return false,
        };
        if a.curve == b.curve {
            let c = &curves[a.curve];
            let n = c.len() - 1;
            let (i, j) = (a.index.min(b.index), a.index.max(b.index));
            // consecutive segments meet at their shared vertex only
            (j == i + 1 && at == c[j]) || (closed[a.curve] && i == 0 && j == n - 1 && at == c[0])
        } else {
            // different curves: only at an end of each (closed curves have none)
            let end = |k: usize| {
                let c = &curves[k];
                !closed[k] && (c.first() == Some(&at) || c.last() == Some(&at))
            };
            end(a.curve) && end(b.curve)
        }
    };
    let test = |a: &Seg, b: &Seg| match line_intersection(a.line, b.line) {
        None => true,
        Some(x) => allowed(a, b, &x),
    };
    if segs.len() < BRUTE_FORCE {
        return segs
            .iter()
            .enumerate()
            .all(|(i, a)| segs[i + 1..].iter().all(|b| test(a, b)));
    }
    let mut builder = RTreeBuilder::<f64>::new(segs.len() as u32);
    for s in &segs {
        let (p, q) = (s.line.start, s.line.end);
        builder.add(p.x.min(q.x), p.y.min(q.y), p.x.max(q.x), p.y.max(q.y));
    }
    let tree = builder.finish::<HilbertSort>();
    segs.iter().enumerate().all(|(i, a)| {
        let (p, q) = (a.line.start, a.line.end);
        tree.search(p.x.min(q.x), p.y.min(q.y), p.x.max(q.x), p.y.max(q.y))
            .into_iter()
            .filter(|&j| j as usize > i)
            .all(|j| test(a, &segs[j as usize]))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geo::ops::wkt;

    fn simple(s: &str) -> bool {
        is_simple(&wkt(s).g)
    }

    #[test]
    fn table() {
        for (g, want) in [
            ("POINT(1 2)", true),
            ("POINT EMPTY", true),
            ("LINESTRING EMPTY", true),
            ("MULTIPOINT((0 0),(1 1))", true),
            ("MULTIPOINT((0 0),(1 1),(0 0))", false),
            ("LINESTRING(0 0, 1 1, 2 0)", true),
            ("LINESTRING(0 0, 1 1, 1 1, 2 0)", true),
            ("LINESTRING(0 0, 1 1, 2 2)", true),
            // crossing itself
            ("LINESTRING(0 0, 2 2, 2 0, 0 2)", false),
            // touching itself in an interior vertex
            ("LINESTRING(0 0, 2 0, 1 1, 1 0)", false),
            // doubling back on itself
            ("LINESTRING(0 0, 2 0, 1 0)", false),
            // a closed curve is simple; one that also passes its start again is not
            ("LINESTRING(0 0, 1 0, 1 1, 0 0)", true),
            ("LINESTRING(0 0, 1 0, 1 1, 0 0, -1 0)", false),
            ("MULTILINESTRING((0 0, 1 1),(1 1, 2 0))", true),
            ("MULTILINESTRING((0 0, 2 2),(0 2, 2 0))", false),
            // an end of one member on the interior of the other
            ("MULTILINESTRING((0 0, 2 0),(1 0, 1 1))", false),
            // a closed member has no ends: touching it is not simple
            ("MULTILINESTRING((0 0, 1 0, 1 1, 0 0),(1 1, 2 2))", false),
            (
                "POLYGON((0 0, 4 0, 4 4, 0 4, 0 0),(1 1, 2 1, 2 2, 1 1))",
                true,
            ),
            // a ring that touches itself in a vertex (inverted)
            ("POLYGON((0 0, 4 0, 2 2, 4 4, 0 4, 2 2, 0 0))", false),
            ("POLYGON((0 0, 2 2, 2 0, 0 2, 0 0))", false),
            (
                "MULTIPOLYGON(((0 0, 1 0, 1 1, 0 0)),((5 5, 6 5, 6 6, 5 5)))",
                true,
            ),
            ("GEOMETRYCOLLECTION(POINT(0 0), LINESTRING(0 0, 1 1))", true),
            ("GEOMETRYCOLLECTION(LINESTRING(0 0, 2 2, 2 0, 0 2))", false),
        ] {
            assert_eq!(simple(g), want, "{g}");
        }
    }

    /// The R-tree sweep agrees with testing every pair.
    #[test]
    fn long_curves() {
        // a spiral (simple), then the same spiral closed back across itself
        let mut pts: Vec<String> = (0..200)
            .map(|i| {
                let a = f64::from(i) * 0.3;
                let r = 1.0 + f64::from(i) * 0.05;
                format!("{} {}", r * a.cos(), r * a.sin())
            })
            .collect();
        assert!(simple(&format!("LINESTRING({})", pts.join(","))));
        pts.push("0 0".into());
        assert!(!simple(&format!("LINESTRING({})", pts.join(","))));
    }
}
