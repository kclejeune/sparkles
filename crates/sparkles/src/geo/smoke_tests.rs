//! The capabilities of the geometry crates the GeoSPARQL implementation relies on: if an
//! upgrade drops one, these fail first.

use geo_index::rtree::sort::HilbertSort;
use geo_index::rtree::{RTreeBuilder, RTreeIndex, RTreeRef};
use georust::{
    Area, BooleanOps, Buffer, ClosestPoint, ConvexHull, Distance, Geodesic, GeodesicArea, Geometry,
    Haversine, Length, LineString, Point, PreparedGeometry, Relate, polygon, unary_union,
};

fn square(x: f64, y: f64, side: f64) -> georust::Polygon<f64> {
    polygon![
        (x: x, y: y),
        (x: x + side, y: y),
        (x: x + side, y: y + side),
        (x: x, y: y + side),
    ]
}

#[test]
fn relate_and_prepared_geometries() {
    let (a, b) = (square(0.0, 0.0, 4.0), square(1.0, 1.0, 1.0));
    let im = a.relate(&b);
    assert!(im.is_contains() && im.is_intersects() && !im.is_touches());
    assert!(im.matches("T*****FF*").unwrap());
    assert!(im.matches("TT").is_err());
    let prepared = PreparedGeometry::from(Geometry::Polygon(a.clone()));
    assert!(prepared.relate(&Geometry::Polygon(b.clone())).is_contains());
    // equal points: topologically equal, though the boundary pattern is all empty
    let p = Point::new(1.0, 2.0);
    assert!(p.relate(&p).is_equal_topo());
}

#[test]
fn overlay_buffer_and_hull() {
    let (a, b) = (square(0.0, 0.0, 4.0), square(3.0, 3.0, 2.0));
    assert!((a.intersection(&b).unsigned_area() - 1.0_f64).abs() < 1e-9);
    assert!((a.union(&b).unsigned_area() - 19.0_f64).abs() < 1e-9);
    assert!((a.difference(&b).unsigned_area() - 15.0_f64).abs() < 1e-9);
    assert!((a.xor(&b).unsigned_area() - 18.0_f64).abs() < 1e-9);
    assert!((unary_union(&[a.clone(), b.clone()]).unsigned_area() - 19.0_f64).abs() < 1e-9);
    let disc = Point::new(0.0, 0.0).buffer(1.0);
    assert!((disc.unsigned_area() - std::f64::consts::PI).abs() < 0.05);
    let ls = LineString::from(vec![(0., 0.), (1., 1.), (2., 0.), (1., 0.5)]);
    assert!((ls.convex_hull().unsigned_area() - 1.0_f64).abs() < 1e-9);
    let cp = ls.closest_point(&Point::new(1.0, 2.0));
    assert!(matches!(cp, georust::Closest::SinglePoint(p) if p == Point::new(1.0, 1.0)));
}

#[test]
fn geodesic_measures() {
    // London – Paris; geodesic on WGS 84 against the haversine sphere
    let (a, b) = (Point::new(-0.1278, 51.5074), Point::new(2.3522, 48.8566));
    let g = Geodesic.distance(a, b);
    assert!((g - 343_923.12).abs() < 1.0, "{g}");
    let h = Haversine.distance(a, b);
    assert!((g - h).abs() / g < 0.005, "{h}");
    let line = LineString::from(vec![a.0, b.0]);
    assert!((Geodesic.length(&line) - g).abs() < 1e-6);
    // the same through geographiclib-rs (latitude first)
    use geographiclib_rs::InverseGeodesic;
    let s12: f64 = geographiclib_rs::Geodesic::wgs84().inverse(51.5074, -0.1278, 48.8566, 2.3522);
    assert!((s12 - g).abs() < 1e-6);
    // one degree square at the equator: about 12,309 km²
    let area = square(0.0, 0.0, 1.0).geodesic_area_unsigned();
    assert!((area / 1e6 - 12_308.78).abs() < 1.0, "{area}");
}

#[test]
fn packed_hilbert_rtree() {
    let mut builder = RTreeBuilder::<f32>::new_with_node_size(1000, 16);
    for i in 0..1000 {
        let (x, y) = ((i % 40) as f32, (i / 40) as f32);
        builder.add(x, y, x + 0.5, y + 0.5);
    }
    let tree = builder.finish::<HilbertSort>();
    assert_eq!(tree.search(0.0, 0.0, 1.0, 1.0).len(), 4);
    // the node boxes of every level, leaves (the items) first
    assert_eq!(tree.num_levels(), 4);
    let per_level: Vec<usize> = (0..tree.num_levels())
        .map(|l| tree.boxes_at_level(l).unwrap().len() / 4)
        .collect();
    assert_eq!(per_level, [1000, 63, 4, 1]);
    let root = tree.root();
    assert_eq!((root.max_x(), root.max_y()), (39.5, 24.5));
    assert_eq!(root.children().unwrap().count(), 4);
    // usable from bytes without copying
    let bytes = tree.into_inner();
    let r = RTreeRef::<f32>::try_new(&bytes).unwrap();
    assert_eq!(r.search(0.0, 0.0, 1.0, 1.0).len(), 4);
}
