//! Writing geometries in the canonical WKT and GeoJSON forms.
//!
//! WKT: `[<IRI> ]TYPE(…)` with the type in upper case, no space before `(`, `, `
//! between points and one space between ordinates; the CRS prefix is omitted for
//! CRS84. Coordinates are written in the literal's own axis order (latitude first for
//! EPSG:4326). Z values are not kept by [`Geom`], so output is 2D.
//!
//! GeoJSON: compact, members in the order `type`, `coordinates` / `geometries`,
//! always CRS84, exterior rings counter-clockwise and holes clockwise (RFC 7946
//! §3.1.6). An empty geometry is an empty GeometryCollection.

use super::crs::{self, CRS84, CrsRef};
use super::geom::{Geom, GeomType};
use super::vocab;
use georust::{Coord, Geometry, LineString, Polygon, Winding};
use std::fmt::Write;

/// A number in the canonical form: Rust's shortest round-trip digits, integral values
/// without `.0`, `-0` as `0`, and an exponent only outside [1e-6, 1e21).
pub fn number(out: &mut String, v: f64) {
    let a = v.abs();
    if a == 0.0 {
        out.push('0');
    } else if (1e-6..1e21).contains(&a) {
        let _ = write!(out, "{v}");
    } else {
        let _ = write!(out, "{v:e}");
    }
}

/// The canonical WKT form (with the CRS IRI unless it is CRS84).
pub fn to_wkt(g: &Geom) -> String {
    let mut out = String::new();
    let swap = match &g.crs {
        CrsRef::Known(id) => {
            if *id != CRS84 {
                let _ = write!(out, "<{}> ", id.iri());
            }
            id.lat_first()
        }
        CrsRef::Unknown(iri) => {
            let _ = write!(out, "<{iri}> ");
            false
        }
    };
    let mut w = WktWriter {
        out: &mut out,
        swap,
    };
    if g.empty {
        w.out.push_str(g.declared.wkt_name());
        w.out.push_str(" EMPTY");
    } else {
        w.write(&g.g, Some(g.declared));
    }
    out
}

struct WktWriter<'a> {
    out: &'a mut String,
    swap: bool,
}

impl WktWriter<'_> {
    fn coord(&mut self, c: Coord<f64>) {
        let (a, b) = if self.swap { (c.y, c.x) } else { (c.x, c.y) };
        number(self.out, a);
        self.out.push(' ');
        number(self.out, b);
    }

    /// `(x y, x y, …)` or ` EMPTY`
    fn coords(&mut self, cs: &[Coord<f64>]) {
        if cs.is_empty() {
            self.out.push_str("EMPTY");
            return;
        }
        self.out.push('(');
        for (i, c) in cs.iter().enumerate() {
            if i > 0 {
                self.out.push_str(", ");
            }
            self.coord(*c);
        }
        self.out.push(')');
    }

    fn polygon(&mut self, p: &Polygon<f64>) {
        if p.exterior().0.is_empty() {
            self.out.push_str("EMPTY");
            return;
        }
        self.out.push('(');
        self.coords(&p.exterior().0);
        for r in p.interiors() {
            self.out.push_str(", ");
            self.coords(&r.0);
        }
        self.out.push(')');
    }

    fn members<T>(&mut self, items: &[T], mut f: impl FnMut(&mut Self, &T)) {
        if items.is_empty() {
            self.out.push_str("EMPTY");
            return;
        }
        self.out.push('(');
        for (i, x) in items.iter().enumerate() {
            if i > 0 {
                self.out.push_str(", ");
            }
            f(self, x);
        }
        self.out.push(')');
    }

    /// A tagged geometry; `declared` names the written type when it fits `g`.
    fn write(&mut self, g: &Geometry<f64>, declared: Option<GeomType>) {
        use Geometry as G;
        let ty = GeomType::of(g);
        let name = match (declared, g) {
            (Some(GeomType::LinearRing), G::LineString(_))
            | (Some(GeomType::Triangle), G::Polygon(_))
            | (Some(GeomType::Tin | GeomType::PolyhedralSurface), G::MultiPolygon(_)) => {
                declared.unwrap_or(ty).wkt_name()
            }
            // `POINT EMPTY` is held as an empty multipoint
            (Some(GeomType::Point), G::MultiPoint(m)) if m.0.is_empty() => "POINT",
            _ => ty.wkt_name(),
        };
        self.out.push_str(name);
        let at = self.out.len();
        let w = self;
        match g {
            G::Point(p) => {
                w.out.push('(');
                w.coord(p.0);
                w.out.push(')');
            }
            G::Line(l) => w.coords(&[l.start, l.end]),
            G::LineString(l) => w.coords(&l.0),
            G::Polygon(p) => w.polygon(p),
            G::Rect(r) => w.polygon(&r.to_polygon()),
            G::Triangle(t) => w.polygon(&t.to_polygon()),
            G::MultiPoint(m) => w.members(&m.0, |w, p| {
                w.out.push('(');
                w.coord(p.0);
                w.out.push(')');
            }),
            G::MultiLineString(m) => w.members(&m.0, |w, l| w.coords(&l.0)),
            G::MultiPolygon(m) => w.members(&m.0, |w, p| w.polygon(p)),
            G::GeometryCollection(c) => w.members(&c.0, |w, g| w.write(g, None)),
        }
        // `TYPE EMPTY` has a space, `TYPE(…)` none
        if w.out[at..].starts_with("EMPTY") {
            w.out.insert(at, ' ');
        }
    }
}

/// GeoJSON (always CRS84: a projected geometry is transformed; a geometry of an unknown
/// CRS is written in its own coordinates, so callers check the CRS first).
pub fn to_geojson(g: &Geom) -> String {
    let mut out = String::new();
    if g.empty {
        out.push_str(r#"{"type":"GeometryCollection","geometries":[]}"#);
        return out;
    }
    let to84 = match g.crs.known() {
        Some(id) if !id.is_geographic() => Some(id),
        _ => None,
    };
    let mut w = JsonWriter {
        out: &mut out,
        to84,
    };
    w.write(&g.g);
    out
}

struct JsonWriter<'a> {
    out: &'a mut String,
    /// the projected CRS to transform from
    to84: Option<crs::CrsId>,
}

impl JsonWriter<'_> {
    fn position(&mut self, c: Coord<f64>) {
        let (x, y) = match self.to84 {
            Some(id) => crs::to_lonlat(id, c.x, c.y).unwrap_or((c.x, c.y)),
            None => (c.x, c.y),
        };
        self.out.push('[');
        number(self.out, x);
        self.out.push(',');
        number(self.out, y);
        self.out.push(']');
    }

    fn array<T>(&mut self, items: &[T], mut f: impl FnMut(&mut Self, &T)) {
        self.out.push('[');
        for (i, x) in items.iter().enumerate() {
            if i > 0 {
                self.out.push(',');
            }
            f(self, x);
        }
        self.out.push(']');
    }

    fn line(&mut self, l: &LineString<f64>) {
        self.array(&l.0, |w, c| w.position(*c));
    }

    fn polygon(&mut self, p: &Polygon<f64>) {
        if p.exterior().0.is_empty() {
            self.out.push_str("[]");
            return;
        }
        let mut ext = p.exterior().clone();
        ext.make_ccw_winding();
        let mut rings = vec![ext];
        for r in p.interiors() {
            let mut r = r.clone();
            r.make_cw_winding();
            rings.push(r);
        }
        self.array(&rings, |w, r| w.line(r));
    }

    fn typed(&mut self, ty: &str) {
        let _ = write!(self.out, r#"{{"type":"{ty}","coordinates":"#);
    }

    fn write(&mut self, g: &Geometry<f64>) {
        use Geometry as G;
        match g {
            G::Point(p) => {
                self.typed("Point");
                self.position(p.0);
            }
            G::Line(l) => {
                self.typed("LineString");
                self.line(&LineString::new(vec![l.start, l.end]));
            }
            G::LineString(l) => {
                self.typed("LineString");
                self.line(l);
            }
            G::Polygon(p) => {
                self.typed("Polygon");
                self.polygon(p);
            }
            G::Rect(r) => {
                self.typed("Polygon");
                self.polygon(&r.to_polygon());
            }
            G::Triangle(t) => {
                self.typed("Polygon");
                self.polygon(&t.to_polygon());
            }
            G::MultiPoint(m) => {
                self.typed("MultiPoint");
                self.array(&m.0, |w, p| w.position(p.0));
            }
            G::MultiLineString(m) => {
                self.typed("MultiLineString");
                self.array(&m.0, |w, l| w.line(l));
            }
            G::MultiPolygon(m) => {
                self.typed("MultiPolygon");
                self.array(&m.0, |w, p| w.polygon(p));
            }
            G::GeometryCollection(c) => {
                self.out
                    .push_str(r#"{"type":"GeometryCollection","geometries":"#);
                self.array(&c.0, |w, g| w.write(g));
            }
        }
        self.out.push('}');
    }
}

/// A literal of datatype `dt` (`geo:wktLiteral`, `geo:geoJSONLiteral`,
/// `geo:gmlLiteral` or `geo:kmlLiteral`; WKT for any other). GeoJSON and KML of a
/// geometry in an unknown CRS are written in its own coordinates, so callers check the
/// CRS first.
pub fn literal(g: &Geom, dt: &str) -> oxrdf::Literal {
    let dt = result_datatype(dt);
    let lex = match dt {
        vocab::GEOJSON_LITERAL => to_geojson(g),
        vocab::GML_LITERAL => super::xml::to_gml(g),
        vocab::KML_LITERAL => super::xml::to_kml(g),
        _ => to_wkt(g),
    };
    oxrdf::Literal::new_typed_literal(lex, oxrdf::NamedNode::new_unchecked(dt))
}

/// The datatype of a geometry computed from a literal of datatype `dt`: the same
/// serialization, or WKT for a datatype that is not a geometry's.
pub fn result_datatype(dt: &str) -> &'static str {
    match dt {
        vocab::GEOJSON_LITERAL => vocab::GEOJSON_LITERAL,
        vocab::GML_LITERAL => vocab::GML_LITERAL,
        vocab::KML_LITERAL => vocab::KML_LITERAL,
        _ => vocab::WKT_LITERAL,
    }
}

/// The lexical form of `g` as a literal of datatype `dt`; `None` for GeoJSON and KML
/// of a geometry in an unknown CRS, which has no transform to CRS84.
pub fn serialize(g: &Geom, dt: &str) -> Option<String> {
    Some(match result_datatype(dt) {
        vocab::GEOJSON_LITERAL => {
            g.crs.known()?;
            to_geojson(g)
        }
        vocab::KML_LITERAL => {
            g.crs.known()?;
            super::xml::to_kml(g)
        }
        vocab::GML_LITERAL => super::xml::to_gml(g),
        _ => to_wkt(g),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geo::crs::{EPSG_4326, WEB_MERCATOR};
    use crate::geo::parse::parse;
    use crate::geo::vocab::{GEOJSON_LITERAL, WKT_LITERAL};

    fn num(v: f64) -> String {
        let mut s = String::new();
        number(&mut s, v);
        s
    }

    fn wkt(s: &str) -> Geom {
        parse(s, WKT_LITERAL).unwrap_or_else(|e| panic!("{s}: {e}"))
    }

    #[test]
    fn numbers() {
        assert_eq!(num(2.0), "2");
        assert_eq!(num(48.8566), "48.8566");
        assert_eq!(num(-0.0), "0");
        assert_eq!(num(0.0), "0");
        assert_eq!(num(-12.5), "-12.5");
        assert_eq!(num(0.1 + 0.2), "0.30000000000000004");
        assert_eq!(num(1e-6), "0.000001");
        assert_eq!(num(1e-7), "1e-7");
        assert_eq!(num(-2.5e-9), "-2.5e-9");
        assert_eq!(num(1e20), "100000000000000000000");
        assert_eq!(num(1e21), "1e21");
        assert_eq!(num(20037508.342789244), "20037508.342789244");
    }

    #[test]
    fn canonical_wkt() {
        for (input, out) in [
            ("POINT(1 2)", "POINT(1 2)"),
            ("Point ( 1.0  2.0 )", "POINT(1 2)"),
            ("POINT(2.0 48.8566)", "POINT(2 48.8566)"),
            ("linestring(0 0,1 1 , 2 0)", "LINESTRING(0 0, 1 1, 2 0)"),
            (
                "POLYGON((0 0,10 0,10 10,0 10,0 0),(1 1,2 1,2 2,1 1))",
                "POLYGON((0 0, 10 0, 10 10, 0 10, 0 0), (1 1, 2 1, 2 2, 1 1))",
            ),
            ("MULTIPOINT(1 1, 2 2)", "MULTIPOINT((1 1), (2 2))"),
            (
                "MULTILINESTRING((0 0,1 1),EMPTY)",
                "MULTILINESTRING((0 0, 1 1), EMPTY)",
            ),
            (
                "MULTIPOLYGON(((0 0,1 0,1 1,0 0)),EMPTY)",
                "MULTIPOLYGON(((0 0, 1 0, 1 1, 0 0)), EMPTY)",
            ),
            (
                "GEOMETRYCOLLECTION(POINT(1 2),GEOMETRYCOLLECTION(LINESTRING(0 0,1 1)))",
                "GEOMETRYCOLLECTION(POINT(1 2), GEOMETRYCOLLECTION(LINESTRING(0 0, 1 1)))",
            ),
            (
                "LINEARRING(0 0,1 0,1 1,0 0)",
                "LINEARRING(0 0, 1 0, 1 1, 0 0)",
            ),
            (
                "TRIANGLE((0 0,1 0,0 1,0 0))",
                "TRIANGLE((0 0, 1 0, 0 1, 0 0))",
            ),
            ("TIN(((0 0,1 0,0 1,0 0)))", "TIN(((0 0, 1 0, 0 1, 0 0)))"),
            (
                "POLYHEDRALSURFACE(((0 0,1 0,1 1,0 0)))",
                "POLYHEDRALSURFACE(((0 0, 1 0, 1 1, 0 0)))",
            ),
            ("POINT Z (1 2 3)", "POINT(1 2)"),
            ("POINT EMPTY", "POINT EMPTY"),
            ("polygon empty", "POLYGON EMPTY"),
            ("", "GEOMETRYCOLLECTION EMPTY"),
            ("GEOMETRYCOLLECTION EMPTY", "GEOMETRYCOLLECTION EMPTY"),
            (
                "<http://www.opengis.net/def/crs/OGC/1.3/CRS84> POINT(1 2)",
                "POINT(1 2)",
            ),
            (
                "<http://www.opengis.net/def/crs/EPSG/0/4326> POINT(2 12)",
                "<http://www.opengis.net/def/crs/EPSG/0/4326> POINT(2 12)",
            ),
            (
                "<urn:ogc:def:crs:EPSG::4326>\tPOINT(2 12)",
                "<http://www.opengis.net/def/crs/EPSG/0/4326> POINT(2 12)",
            ),
            (
                "<http://www.opengis.net/def/crs/EPSG/0/4326> POINT EMPTY",
                "<http://www.opengis.net/def/crs/EPSG/0/4326> POINT EMPTY",
            ),
            (
                "<http://example.org/crs/mars> POINT(1 1)",
                "<http://example.org/crs/mars> POINT(1 1)",
            ),
        ] {
            let g = wkt(input);
            let w = to_wkt(&g);
            assert_eq!(w, out, "{input}");
            // the canonical form reads back to the same geometry and writes the same
            let again = wkt(&w);
            assert_eq!(
                (again.g.clone(), again.crs.clone()),
                (g.g, g.crs),
                "{input}"
            );
            assert_eq!(to_wkt(&again), w);
        }
    }

    #[test]
    fn transformed_output() {
        // EPSG:4326 POINT(2 12) is longitude 12, latitude 2
        let g = wkt("<http://www.opengis.net/def/crs/EPSG/0/4326> POINT(2 12)");
        assert_eq!(to_wkt(&g.transformed(CRS84).unwrap()), "POINT(12 2)");
        let back = wkt("POINT(12 2)").transformed(EPSG_4326).unwrap();
        assert_eq!(
            to_wkt(&back),
            "<http://www.opengis.net/def/crs/EPSG/0/4326> POINT(2 12)"
        );
        let m = wkt("POINT(180 0)").transformed(WEB_MERCATOR).unwrap();
        assert_eq!(
            to_wkt(&m),
            "<http://www.opengis.net/def/crs/EPSG/0/3857> POINT(20037508.342789244 0)"
        );
        // GeoJSON is CRS84: transformed back (within rounding)
        let j = parse(&to_geojson(&m), GEOJSON_LITERAL).unwrap();
        let b = j.bbox().unwrap();
        assert!((b[0] - 180.0).abs() < 1e-9 && b[1] == 0.0, "{b:?}");
        let l = literal(&g, WKT_LITERAL);
        assert_eq!(l.datatype().as_str(), WKT_LITERAL);
        assert_eq!(
            literal(&g, GEOJSON_LITERAL).value(),
            r#"{"type":"Point","coordinates":[12,2]}"#
        );
    }

    #[test]
    fn canonical_geojson() {
        let j = |s: &str| to_geojson(&wkt(s));
        assert_eq!(
            j("POINT(2.0 48.8566)"),
            r#"{"type":"Point","coordinates":[2,48.8566]}"#
        );
        assert_eq!(
            j("LINESTRING(0 0, 1 1)"),
            r#"{"type":"LineString","coordinates":[[0,0],[1,1]]}"#
        );
        // a clockwise exterior ring and a counter-clockwise hole are reversed
        assert_eq!(
            j("POLYGON((0 0, 0 10, 10 10, 10 0, 0 0), (1 1, 2 1, 2 2, 1 1))"),
            r#"{"type":"Polygon","coordinates":[[[0,0],[10,0],[10,10],[0,10],[0,0]],[[1,1],[2,2],[2,1],[1,1]]]}"#
        );
        assert_eq!(
            j("MULTIPOINT((1 1), (2 2))"),
            r#"{"type":"MultiPoint","coordinates":[[1,1],[2,2]]}"#
        );
        assert_eq!(
            j("TIN(((0 0, 1 0, 0 1, 0 0)))"),
            r#"{"type":"MultiPolygon","coordinates":[[[[0,0],[1,0],[0,1],[0,0]]]]}"#
        );
        assert_eq!(
            j("GEOMETRYCOLLECTION(POINT(1 2), LINESTRING(0 0, 1 1))"),
            r#"{"type":"GeometryCollection","geometries":[{"type":"Point","coordinates":[1,2]},{"type":"LineString","coordinates":[[0,0],[1,1]]}]}"#
        );
        assert_eq!(
            j("POINT EMPTY"),
            r#"{"type":"GeometryCollection","geometries":[]}"#
        );
        // EPSG:4326 input comes out in GeoJSON's longitude, latitude order
        assert_eq!(
            j("<http://www.opengis.net/def/crs/EPSG/0/4326> POINT(48.8606 2.3376)"),
            r#"{"type":"Point","coordinates":[2.3376,48.8606]}"#
        );
        // round trips through the GeoJSON parser
        for s in [
            "POINT(1 2)",
            "MULTILINESTRING((0 0, 1 1), (2 2, 3 3))",
            "MULTIPOLYGON(((0 0, 1 0, 1 1, 0 0)))",
            "GEOMETRYCOLLECTION(POINT(1 2), POINT(3 4))",
        ] {
            let g = wkt(s);
            let back = parse(&to_geojson(&g), GEOJSON_LITERAL).unwrap();
            assert_eq!(back.g, g.g, "{s}");
            assert_eq!(to_wkt(&back), to_wkt(&g));
        }
    }
}
