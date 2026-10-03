//! Parsing `geo:wktLiteral` and `geo:geoJSONLiteral` lexical forms (GML and KML are in
//! [`super::xml`]).
//!
//! WKT has its own small parser rather than the `wkt` crate's: errors need a byte
//! offset, and GeoSPARQL literals also use LINEARRING, TRIANGLE, TIN and
//! POLYHEDRALSURFACE, untagged 3D coordinates and a CRS IRI prefix. GeoJSON is read
//! through `serde_json`, accepting Geometry objects only.

use super::crs::{CRS84, CrsRef};
use super::geom::{Geom, GeomError, GeomType, Layout};
use super::vocab::{GEOJSON_LITERAL, GML_LITERAL, KML_LITERAL, WKT_LITERAL};
use georust::{
    Coord, Geometry, GeometryCollection, LineString, MultiLineString, MultiPoint, MultiPolygon,
    Point, Polygon,
};

/// Deepest nesting of geometry collections.
pub const MAX_DEPTH: usize = 32;

/// Parse a geometry literal of datatype `dt`.
pub fn parse(lex: &str, dt: &str) -> Result<Geom, GeomError> {
    parse_limited(lex, dt, u32::MAX)
}

/// [`parse`], refusing geometries with more than `max_vertices` vertices.
pub fn parse_limited(lex: &str, dt: &str, max_vertices: u32) -> Result<Geom, GeomError> {
    if dt == WKT_LITERAL {
        parse_wkt(lex, max_vertices)
    } else if dt == GEOJSON_LITERAL {
        parse_geojson(lex, max_vertices)
    } else if dt == GML_LITERAL {
        super::xml::parse_gml(lex, max_vertices)
    } else if dt == KML_LITERAL {
        super::xml::parse_kml(lex, max_vertices)
    } else {
        Err(GeomError::new(format!(
            "unsupported geometry datatype <{dt}>"
        )))
    }
}

/// Is `dt` a geometry datatype this build can parse?
pub fn is_geometry_datatype(dt: &str) -> bool {
    super::vocab::is_geometry_datatype(dt)
}

/// The shared state of both parsers: layout, Z range and vertex count of the geometry.
pub(super) struct Acc {
    layout: Option<Layout>,
    z: Option<(f64, f64)>,
    vertices: u32,
    max_vertices: u32,
    /// the literal is latitude first: swap into internal (east, north) order
    pub(super) swap: bool,
}

impl Acc {
    pub(super) fn new(max_vertices: u32) -> Acc {
        Acc {
            layout: None,
            z: None,
            vertices: 0,
            max_vertices,
            swap: false,
        }
    }

    /// Fix the layout, or check that `l` agrees with it.
    fn layout(&mut self, l: Layout) -> Result<(), String> {
        match self.layout {
            None => {
                self.layout = Some(l);
                Ok(())
            }
            Some(have) if have == l => Ok(()),
            Some(_) => Err("mixed coordinate dimensions in one geometry".into()),
        }
    }

    /// A coordinate from its ordinates.
    pub(super) fn coord(&mut self, ords: &[f64]) -> Result<Coord<f64>, String> {
        let l = match self.layout {
            Some(l) => l,
            None => match ords.len() {
                2 => Layout::Xy,
                3 => Layout::Xyz,
                4 => Layout::Xyzm,
                n => return Err(format!("a coordinate has {n} ordinates")),
            },
        };
        if ords.len() != l.ordinates() {
            return Err(
                if self.layout.is_some() && ords.len() >= 2 && ords.len() <= 4 {
                    "mixed coordinate dimensions in one geometry".into()
                } else {
                    format!(
                        "a coordinate has {} ordinates, expected {}",
                        ords.len(),
                        l.ordinates()
                    )
                },
            );
        }
        self.layout = Some(l);
        if l.has_z() {
            let z = ords[2];
            self.z = Some(match self.z {
                None => (z, z),
                Some((lo, hi)) => (lo.min(z), hi.max(z)),
            });
        }
        self.vertices += 1;
        if self.vertices > self.max_vertices {
            return Err(format!(
                "geometry too complex (more than {} vertices)",
                self.max_vertices
            ));
        }
        let (x, y) = if self.swap {
            (ords[1], ords[0])
        } else {
            (ords[0], ords[1])
        };
        Ok(Coord { x, y })
    }

    pub(super) fn finish(self, crs: CrsRef, declared: GeomType, g: Geometry<f64>) -> Geom {
        Geom {
            crs,
            declared,
            layout: self.layout.unwrap_or_default(),
            // a collection of empty members is empty too
            empty: self.vertices == 0,
            g,
            z: self.z,
            vertices: self.vertices,
        }
    }
}

/// A ring needs 4 points and must be closed.
pub(super) fn check_ring(ring: &[Coord<f64>]) -> Result<(), String> {
    if ring.len() < 4 {
        return Err(format!(
            "a ring has {} points, at least 4 needed",
            ring.len()
        ));
    }
    if ring.first() != ring.last() {
        return Err("a ring is not closed".into());
    }
    Ok(())
}

// ---------------------------------------------------------------------------------------
// WKT

fn parse_wkt(lex: &str, max_vertices: u32) -> Result<Geom, GeomError> {
    let mut p = Wkt {
        s: lex.as_bytes(),
        pos: 0,
        acc: Acc::new(max_vertices),
    };
    p.ws();
    let mut crs = CrsRef::Known(CRS84);
    if p.peek() == Some(b'<') {
        let start = p.pos + 1;
        let Some(len) = lex[start..].find('>') else {
            return Err(GeomError::at(p.pos, "unterminated CRS IRI"));
        };
        let iri = &lex[start..start + len];
        if oxiri::Iri::parse(iri).is_err() {
            return Err(GeomError::at(start, "the CRS IRI is not an absolute IRI"));
        }
        crs = CrsRef::from_iri(Some(iri));
        p.acc.swap = crs.known().is_some_and(|id| id.lat_first());
        p.pos = start + len + 1;
        p.ws();
    }
    if p.pos == p.s.len() {
        // the empty literal
        return Ok(Geom::empty(crs, GeomType::GeometryCollection));
    }
    let (declared, g) = p.geometry(0)?;
    p.ws();
    if p.pos != p.s.len() {
        return Err(p.err("unexpected text after the geometry"));
    }
    Ok(p.acc.finish(crs, declared, g))
}

struct Wkt<'a> {
    s: &'a [u8],
    pos: usize,
    acc: Acc,
}

impl Wkt<'_> {
    fn peek(&self) -> Option<u8> {
        self.s.get(self.pos).copied()
    }

    fn ws(&mut self) {
        while self.peek().is_some_and(|b| b.is_ascii_whitespace()) {
            self.pos += 1;
        }
    }

    fn err(&self, msg: impl Into<String>) -> GeomError {
        GeomError::at(self.pos, msg)
    }

    fn word(&mut self) -> &str {
        let start = self.pos;
        while self.peek().is_some_and(|b| b.is_ascii_alphabetic()) {
            self.pos += 1;
        }
        // ASCII letters only: always a char boundary
        std::str::from_utf8(&self.s[start..self.pos]).unwrap_or_default()
    }

    fn expect(&mut self, b: u8) -> Result<(), GeomError> {
        self.ws();
        if self.peek() == Some(b) {
            self.pos += 1;
            Ok(())
        } else {
            Err(self.err(format!("expected '{}'", b as char)))
        }
    }

    /// `(` or `EMPTY`: true when a body follows.
    fn open_or_empty(&mut self) -> Result<bool, GeomError> {
        self.ws();
        match self.peek() {
            Some(b'(') => {
                self.pos += 1;
                Ok(true)
            }
            Some(b) if b.is_ascii_alphabetic() => {
                let at = self.pos;
                if self.word().eq_ignore_ascii_case("EMPTY") {
                    Ok(false)
                } else {
                    Err(GeomError::at(at, "expected '(' or EMPTY"))
                }
            }
            _ => Err(self.err("expected '(' or EMPTY")),
        }
    }

    /// After a list member: `,` (true: another member) or `)` (false).
    fn next_member(&mut self) -> Result<bool, GeomError> {
        self.ws();
        match self.peek() {
            Some(b',') => {
                self.pos += 1;
                Ok(true)
            }
            Some(b')') => {
                self.pos += 1;
                Ok(false)
            }
            _ => Err(self.err("expected ',' or ')'")),
        }
    }

    fn number(&mut self) -> Result<f64, GeomError> {
        let start = self.pos;
        if matches!(self.peek(), Some(b'+' | b'-')) {
            self.pos += 1;
        }
        let mut digits = 0;
        while self.peek().is_some_and(|b| b.is_ascii_digit()) {
            self.pos += 1;
            digits += 1;
        }
        if self.peek() == Some(b'.') {
            self.pos += 1;
            while self.peek().is_some_and(|b| b.is_ascii_digit()) {
                self.pos += 1;
                digits += 1;
            }
        }
        if digits == 0 {
            return Err(GeomError::at(start, "expected a number"));
        }
        if matches!(self.peek(), Some(b'e' | b'E')) {
            self.pos += 1;
            if matches!(self.peek(), Some(b'+' | b'-')) {
                self.pos += 1;
            }
            let exp = self.pos;
            while self.peek().is_some_and(|b| b.is_ascii_digit()) {
                self.pos += 1;
            }
            if self.pos == exp {
                return Err(GeomError::at(start, "expected a number"));
            }
        }
        if self
            .peek()
            .is_some_and(|b| b.is_ascii_alphanumeric() || b == b'.')
        {
            return Err(GeomError::at(start, "invalid number"));
        }
        let text = std::str::from_utf8(&self.s[start..self.pos]).unwrap_or_default();
        match text.parse::<f64>() {
            Ok(v) if v.is_finite() => Ok(v),
            _ => Err(GeomError::at(start, "number out of range")),
        }
    }

    fn coord(&mut self) -> Result<Coord<f64>, GeomError> {
        self.ws();
        let start = self.pos;
        let mut ords = [0.0; 4];
        let mut n = 0;
        loop {
            if n == 4 {
                return Err(GeomError::at(
                    start,
                    "a coordinate has more than 4 ordinates",
                ));
            }
            ords[n] = self.number()?;
            n += 1;
            let before = self.pos;
            self.ws();
            if !matches!(self.peek(), Some(b'0'..=b'9' | b'+' | b'-' | b'.')) {
                if n < 2 {
                    return Err(self.err("expected a number"));
                }
                break;
            }
            if self.pos == before {
                return Err(self.err("expected whitespace between ordinates"));
            }
        }
        self.acc
            .coord(&ords[..n])
            .map_err(|m| GeomError::at(start, m))
    }

    /// `( coord, … )` after the opening parenthesis was read.
    fn coords(&mut self) -> Result<Vec<Coord<f64>>, GeomError> {
        let mut v = vec![self.coord()?];
        while self.next_member()? {
            v.push(self.coord()?);
        }
        Ok(v)
    }

    fn line(&mut self, min: usize) -> Result<LineString<f64>, GeomError> {
        if !self.open_or_empty()? {
            return Ok(LineString::new(vec![]));
        }
        let start = self.pos;
        let v = self.coords()?;
        if v.len() < min {
            return Err(GeomError::at(
                start,
                format!("a line has {} points, at least {min} needed", v.len()),
            ));
        }
        Ok(LineString::new(v))
    }

    fn ring(&mut self) -> Result<LineString<f64>, GeomError> {
        self.expect(b'(')?;
        let start = self.pos;
        let v = self.coords()?;
        check_ring(&v).map_err(|m| GeomError::at(start, m))?;
        Ok(LineString::new(v))
    }

    /// A polygon; `triangle`: one ring of exactly 4 points.
    fn polygon(&mut self, triangle: bool) -> Result<Polygon<f64>, GeomError> {
        if !self.open_or_empty()? {
            return Ok(Polygon::new(LineString::new(vec![]), vec![]));
        }
        let start = self.pos;
        let exterior = self.ring()?;
        let mut holes = vec![];
        while self.next_member()? {
            holes.push(self.ring()?);
        }
        if triangle && (!holes.is_empty() || exterior.0.len() != 4) {
            return Err(GeomError::at(
                start,
                "a triangle needs one ring of 4 points",
            ));
        }
        Ok(Polygon::new(exterior, holes))
    }

    /// `( member, … )` or `EMPTY`.
    fn list<T>(
        &mut self,
        mut member: impl FnMut(&mut Self) -> Result<T, GeomError>,
    ) -> Result<Vec<T>, GeomError> {
        let mut v = vec![];
        if self.open_or_empty()? {
            v.push(member(self)?);
            while self.next_member()? {
                v.push(member(self)?);
            }
        }
        Ok(v)
    }

    fn multipoint_member(&mut self) -> Result<Point<f64>, GeomError> {
        self.ws();
        match self.peek() {
            Some(b'(') => {
                self.pos += 1;
                let c = self.coord()?;
                self.expect(b')')?;
                Ok(Point(c))
            }
            Some(b) if b.is_ascii_alphabetic() => {
                Err(self.err("empty points in a MULTIPOINT are not supported"))
            }
            _ => Ok(Point(self.coord()?)),
        }
    }

    /// A tagged geometry; `depth` = enclosing collections.
    fn geometry(&mut self, depth: usize) -> Result<(GeomType, Geometry<f64>), GeomError> {
        self.ws();
        let at = self.pos;
        let word = self.word().to_ascii_uppercase();
        if word.is_empty() {
            return Err(self.err("expected a geometry type"));
        }
        let (name, attached) = split_dimension(&word);
        let ty = match name {
            "POINT" => GeomType::Point,
            "LINESTRING" => GeomType::LineString,
            "POLYGON" => GeomType::Polygon,
            "MULTIPOINT" => GeomType::MultiPoint,
            "MULTILINESTRING" => GeomType::MultiLineString,
            "MULTIPOLYGON" => GeomType::MultiPolygon,
            "GEOMETRYCOLLECTION" => GeomType::GeometryCollection,
            "LINEARRING" => GeomType::LinearRing,
            "TRIANGLE" => GeomType::Triangle,
            "TIN" => GeomType::Tin,
            "POLYHEDRALSURFACE" => GeomType::PolyhedralSurface,
            "CIRCULARSTRING" | "COMPOUNDCURVE" | "CURVEPOLYGON" | "MULTICURVE" | "MULTISURFACE"
            | "CURVE" | "SURFACE" | "GEOMETRY" => {
                return Err(GeomError::at(
                    at,
                    format!("unsupported geometry type {name}"),
                ));
            }
            _ => return Err(GeomError::at(at, format!("unknown geometry type {word}"))),
        };
        // a separate dimension tag: `POINT Z (…)`
        self.ws();
        let mut tag = attached;
        let save = self.pos;
        match dim_tag(self.word()) {
            Some(l) if tag.is_none() => tag = Some(l),
            _ => self.pos = save,
        }
        if let Some(l) = tag {
            self.acc.layout(l).map_err(|m| GeomError::at(at, m))?;
        }
        let g: Geometry<f64> = match ty {
            GeomType::Point => {
                if self.open_or_empty()? {
                    let c = self.coord()?;
                    self.expect(b')')?;
                    Point(c).into()
                } else {
                    MultiPoint::new(vec![]).into()
                }
            }
            GeomType::LineString => self.line(2)?.into(),
            GeomType::LinearRing => {
                let start = self.pos;
                let l = self.line(0)?;
                if !l.0.is_empty() {
                    check_ring(&l.0).map_err(|m| GeomError::at(start, m))?;
                }
                l.into()
            }
            GeomType::Polygon => self.polygon(false)?.into(),
            GeomType::Triangle => self.polygon(true)?.into(),
            GeomType::MultiPoint => MultiPoint::new(self.list(Self::multipoint_member)?).into(),
            GeomType::MultiLineString => MultiLineString::new(self.list(|p| p.line(2))?).into(),
            GeomType::MultiPolygon | GeomType::PolyhedralSurface => {
                MultiPolygon::new(self.list(|p| p.polygon(false))?).into()
            }
            GeomType::Tin => MultiPolygon::new(self.list(|p| p.polygon(true))?).into(),
            GeomType::GeometryCollection => {
                if depth + 1 > MAX_DEPTH {
                    return Err(GeomError::at(
                        at,
                        format!("geometry collections nested deeper than {MAX_DEPTH} levels"),
                    ));
                }
                let members = self.list(|p| p.geometry(depth + 1).map(|(_, g)| g))?;
                Geometry::GeometryCollection(GeometryCollection::new_from(members))
            }
        };
        Ok((ty, g))
    }
}

/// `POINTZ` → (`POINT`, Z); a type name without a suffix → (name, None).
fn split_dimension(word: &str) -> (&str, Option<Layout>) {
    const TYPES: [&str; 11] = [
        "POINT",
        "LINESTRING",
        "POLYGON",
        "MULTIPOINT",
        "MULTILINESTRING",
        "MULTIPOLYGON",
        "GEOMETRYCOLLECTION",
        "LINEARRING",
        "TRIANGLE",
        "TIN",
        "POLYHEDRALSURFACE",
    ];
    for suffix in ["ZM", "Z", "M"] {
        if let Some(name) = word.strip_suffix(suffix)
            && TYPES.contains(&name)
        {
            return (name, dim_tag(suffix));
        }
    }
    (word, None)
}

/// A dimension tag (`Z`, `M`, `ZM`, any case).
fn dim_tag(word: &str) -> Option<Layout> {
    if word.eq_ignore_ascii_case("Z") {
        Some(Layout::Xyz)
    } else if word.eq_ignore_ascii_case("M") {
        Some(Layout::Xym)
    } else if word.eq_ignore_ascii_case("ZM") {
        Some(Layout::Xyzm)
    } else {
        None
    }
}

// ---------------------------------------------------------------------------------------
// GeoJSON

fn parse_geojson(lex: &str, max_vertices: u32) -> Result<Geom, GeomError> {
    let crs = CrsRef::Known(CRS84);
    let text = lex.trim();
    if text.is_empty() || text == "null" {
        return Ok(Geom::empty(crs, GeomType::GeometryCollection));
    }
    let v: serde_json::Value = serde_json::from_str(lex).map_err(|e| {
        let offset = lex
            .split_inclusive('\n')
            .take(e.line().saturating_sub(1))
            .map(str::len)
            .sum::<usize>()
            + e.column().saturating_sub(1);
        GeomError::at(offset.min(lex.len()), format!("invalid JSON: {e}"))
    })?;
    let mut acc = Acc::new(max_vertices);
    let (declared, g) = json_geometry(&v, 0, &mut acc).map_err(GeomError::new)?;
    Ok(acc.finish(crs, declared, g))
}

fn json_geometry(
    v: &serde_json::Value,
    depth: usize,
    acc: &mut Acc,
) -> Result<(GeomType, Geometry<f64>), String> {
    let obj = v
        .as_object()
        .ok_or("a GeoJSON geometry must be an object")?;
    let ty = obj
        .get("type")
        .and_then(|t| t.as_str())
        .ok_or("a GeoJSON geometry needs a \"type\"")?;
    if obj.contains_key("crs") {
        return Err("a GeoJSON \"crs\" member is not supported (GeoJSON is always CRS84)".into());
    }
    if ty == "GeometryCollection" {
        if depth + 1 > MAX_DEPTH {
            return Err(format!(
                "geometry collections nested deeper than {MAX_DEPTH} levels"
            ));
        }
        let members = obj
            .get("geometries")
            .and_then(|g| g.as_array())
            .ok_or("a GeometryCollection needs a \"geometries\" array")?;
        let gs = members
            .iter()
            .map(|m| json_geometry(m, depth + 1, acc).map(|(_, g)| g))
            .collect::<Result<Vec<_>, _>>()?;
        return Ok((
            GeomType::GeometryCollection,
            Geometry::GeometryCollection(GeometryCollection::new_from(gs)),
        ));
    }
    let declared = match ty {
        "Point" => GeomType::Point,
        "LineString" => GeomType::LineString,
        "Polygon" => GeomType::Polygon,
        "MultiPoint" => GeomType::MultiPoint,
        "MultiLineString" => GeomType::MultiLineString,
        "MultiPolygon" => GeomType::MultiPolygon,
        "Feature" | "FeatureCollection" => {
            return Err(format!("a GeoJSON {ty} is not a geometry"));
        }
        _ => return Err(format!("unknown GeoJSON geometry type {ty:?}")),
    };
    let cv = obj
        .get("coordinates")
        .ok_or("a GeoJSON geometry needs a \"coordinates\" array")?;
    let c = json_array(cv)?;
    let g: Geometry<f64> = match declared {
        GeomType::Point => {
            if c.is_empty() {
                MultiPoint::new(vec![]).into()
            } else {
                Point(json_position(cv, acc)?).into()
            }
        }
        GeomType::LineString => json_line(c, acc)?.into(),
        GeomType::Polygon => json_polygon(c, acc)?.into(),
        GeomType::MultiPoint => MultiPoint::new(
            c.iter()
                .map(|p| json_position(p, acc).map(Point))
                .collect::<Result<_, _>>()?,
        )
        .into(),
        GeomType::MultiLineString => MultiLineString::new(
            c.iter()
                .map(|l| json_line(json_array(l)?, acc))
                .collect::<Result<_, _>>()?,
        )
        .into(),
        _ => MultiPolygon::new(
            c.iter()
                .map(|p| json_polygon(json_array(p)?, acc))
                .collect::<Result<_, _>>()?,
        )
        .into(),
    };
    Ok((declared, g))
}

fn json_array(v: &serde_json::Value) -> Result<&Vec<serde_json::Value>, String> {
    v.as_array()
        .ok_or_else(|| "GeoJSON coordinates must be arrays".into())
}

fn json_position(v: &serde_json::Value, acc: &mut Acc) -> Result<Coord<f64>, String> {
    let a = json_array(v)?;
    if a.len() < 2 || a.len() > 3 {
        return Err(format!(
            "a GeoJSON position has {} elements, 2 or 3 needed",
            a.len()
        ));
    }
    let mut ords = [0.0; 3];
    for (o, x) in ords.iter_mut().zip(a) {
        *o = x
            .as_f64()
            .filter(|f| f.is_finite())
            .ok_or("a GeoJSON position must hold numbers")?;
    }
    acc.coord(&ords[..a.len()])
}

fn json_positions(a: &[serde_json::Value], acc: &mut Acc) -> Result<Vec<Coord<f64>>, String> {
    a.iter().map(|p| json_position(p, acc)).collect()
}

fn json_line(a: &[serde_json::Value], acc: &mut Acc) -> Result<LineString<f64>, String> {
    let v = json_positions(a, acc)?;
    if v.len() == 1 {
        return Err("a line has 1 point, at least 2 needed".into());
    }
    Ok(LineString::new(v))
}

fn json_polygon(a: &[serde_json::Value], acc: &mut Acc) -> Result<Polygon<f64>, String> {
    let mut rings = a.iter().map(|r| {
        let v = json_positions(json_array(r)?, acc)?;
        check_ring(&v)?;
        Ok::<_, String>(LineString::new(v))
    });
    let Some(exterior) = rings.next() else {
        return Ok(Polygon::new(LineString::new(vec![]), vec![]));
    };
    Ok(Polygon::new(exterior?, rings.collect::<Result<_, _>>()?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geo::crs::{CrsId, EPSG_4326};
    use georust::CoordsIter;

    fn wkt(s: &str) -> Geom {
        parse(s, WKT_LITERAL).unwrap_or_else(|e| panic!("{s}: {e}"))
    }

    fn wkt_err(s: &str) -> GeomError {
        match parse(s, WKT_LITERAL) {
            Ok(g) => panic!("{s} parsed as {:?}", g.g),
            Err(e) => e,
        }
    }

    fn json(s: &str) -> Geom {
        parse(s, GEOJSON_LITERAL).unwrap_or_else(|e| panic!("{s}: {e}"))
    }

    fn json_err(s: &str) -> GeomError {
        match parse(s, GEOJSON_LITERAL) {
            Ok(g) => panic!("{s} parsed as {:?}", g.g),
            Err(e) => e,
        }
    }

    #[test]
    fn every_wkt_type() {
        let cases = [
            ("POINT(1 2)", GeomType::Point, 1, 0),
            ("point (1 2)", GeomType::Point, 1, 0),
            ("LineString(0 0, 1 1, 2 0)", GeomType::LineString, 3, 1),
            ("POLYGON((0 0, 1 0, 1 1, 0 0))", GeomType::Polygon, 4, 2),
            (
                "POLYGON((0 0, 10 0, 10 10, 0 10, 0 0), (1 1, 2 1, 2 2, 1 1))",
                GeomType::Polygon,
                9,
                2,
            ),
            ("MULTIPOINT((1 1), (2 2))", GeomType::MultiPoint, 2, 0),
            ("MULTIPOINT(1 1, 2 2)", GeomType::MultiPoint, 2, 0),
            ("MULTIPOINT((1 1), 2 2)", GeomType::MultiPoint, 2, 0),
            (
                "MULTILINESTRING((0 0, 1 1), (2 2, 3 3))",
                GeomType::MultiLineString,
                4,
                1,
            ),
            (
                "MULTIPOLYGON(((0 0, 1 0, 1 1, 0 0)), ((5 5, 6 5, 6 6, 5 5)))",
                GeomType::MultiPolygon,
                8,
                2,
            ),
            (
                "GEOMETRYCOLLECTION(POINT(1 2), LINESTRING(0 0, 1 1))",
                GeomType::GeometryCollection,
                3,
                1,
            ),
            ("LINEARRING(0 0, 1 0, 1 1, 0 0)", GeomType::LinearRing, 4, 1),
            ("TRIANGLE((0 0, 1 0, 0 1, 0 0))", GeomType::Triangle, 4, 2),
            (
                "TIN(((0 0, 1 0, 0 1, 0 0)), ((1 0, 1 1, 0 1, 1 0)))",
                GeomType::Tin,
                8,
                2,
            ),
            (
                "POLYHEDRALSURFACE(((0 0, 1 0, 1 1, 0 1, 0 0)))",
                GeomType::PolyhedralSurface,
                5,
                2,
            ),
        ];
        for (s, ty, n, dim) in cases {
            let g = wkt(s);
            assert_eq!(g.declared, ty, "{s}");
            assert_eq!(g.vertices, n, "{s}");
            assert_eq!(g.g.coords_count(), n as usize, "{s}");
            assert_eq!(g.dim(), dim, "{s}");
            assert!(!g.empty && g.layout == Layout::Xy && g.z.is_none(), "{s}");
            assert_eq!(g.crs, CrsRef::Known(CRS84));
        }
        assert!(matches!(
            wkt("TIN(((0 0, 1 0, 0 1, 0 0)))").g,
            Geometry::MultiPolygon(_)
        ));
        assert!(matches!(
            wkt("TRIANGLE((0 0, 1 0, 0 1, 0 0))").g,
            Geometry::Polygon(_)
        ));
        assert!(matches!(
            wkt("LINEARRING(0 0, 1 0, 1 1, 0 0)").g,
            Geometry::LineString(_)
        ));
    }

    #[test]
    fn dimensions() {
        let g = wkt("POINT Z (1 2 3)");
        assert_eq!((g.layout, g.z), (Layout::Xyz, Some((3.0, 3.0))));
        assert_eq!(g.g, Geometry::Point(Point::new(1.0, 2.0)));
        let g = wkt("LINESTRINGZ(1 2 3, 4 5 -6)");
        assert_eq!((g.layout, g.z), (Layout::Xyz, Some((-6.0, 3.0))));
        let g = wkt("POINT M (1 2 3)");
        assert_eq!((g.layout, g.z), (Layout::Xym, None));
        let g = wkt("POINT ZM (1 2 3 4)");
        assert_eq!((g.layout, g.z), (Layout::Xyzm, Some((3.0, 3.0))));
        // untagged 3D and 4D coordinates
        assert_eq!(wkt("POINT(1 2 3)").layout, Layout::Xyz);
        assert_eq!(wkt("POINT(1 2 3 4)").layout, Layout::Xyzm);
        let g = wkt("GEOMETRYCOLLECTION Z (POINT(1 2 3), POINT Z(4 5 6))");
        assert_eq!((g.layout, g.z), (Layout::Xyz, Some((3.0, 6.0))));
        // mixed Z
        for s in [
            "LINESTRING(0 0, 1 1 1)",
            "LINESTRING(0 0 0, 1 1)",
            "POINT Z (1 2)",
            "POINT M (1 2 3 4)",
            "GEOMETRYCOLLECTION(POINT Z(1 2 3), POINT(1 2))",
            "GEOMETRYCOLLECTION(POINT Z(1 2 3), POINT M(1 2 3))",
            "POINT Z M (1 2 3 4)",
        ] {
            wkt_err(s);
        }
        assert!(wkt_err("LINESTRING(0 0, 1 1 1)").msg.contains("mixed"));
        wkt_err("POINT(1)");
        wkt_err("POINT(1 2 3 4 5)");
    }

    #[test]
    fn empty_geometries() {
        for (s, ty, dim) in [
            ("", GeomType::GeometryCollection, -1),
            ("  \n\t", GeomType::GeometryCollection, -1),
            ("POINT EMPTY", GeomType::Point, 0),
            ("point empty", GeomType::Point, 0),
            ("LINESTRING EMPTY", GeomType::LineString, 1),
            ("POLYGON EMPTY", GeomType::Polygon, 2),
            ("MULTIPOINT EMPTY", GeomType::MultiPoint, 0),
            ("MULTIPOLYGON EMPTY", GeomType::MultiPolygon, 2),
            ("GEOMETRYCOLLECTION EMPTY", GeomType::GeometryCollection, -1),
            ("TIN EMPTY", GeomType::Tin, 2),
        ] {
            let g = wkt(s);
            assert!(g.empty, "{s}");
            assert_eq!((g.declared, g.dim(), g.vertices), (ty, dim, 0), "{s}");
        }
        let g = wkt("POINT Z EMPTY");
        assert!(g.empty && g.layout == Layout::Xyz && g.z.is_none());
        let g = wkt("<http://www.opengis.net/def/crs/EPSG/0/4326> POINT EMPTY");
        assert_eq!(
            (g.declared, g.crs),
            (GeomType::Point, CrsRef::Known(EPSG_4326))
        );
        let g = wkt("<http://www.opengis.net/def/crs/EPSG/0/4326>");
        assert!(g.empty && g.crs == CrsRef::Known(EPSG_4326));
        // a collection of empty members is empty
        assert!(wkt("GEOMETRYCOLLECTION(POINT EMPTY)").empty);
        assert!(!wkt("MULTILINESTRING(EMPTY, (0 0, 1 1))").empty);
    }

    #[test]
    fn crs_prefix() {
        let g = wkt("<http://www.opengis.net/def/crs/EPSG/0/4326> POINT(48.8606 2.3376)");
        assert_eq!(g.crs, CrsRef::Known(EPSG_4326));
        // swapped into (lon, lat)
        assert_eq!(g.g, Geometry::Point(Point::new(2.3376, 48.8606)));
        for s in [
            "\t<http://www.opengis.net/def/crs/EPSG/0/4326>\n\n  POINT(48.8606 2.3376)  ",
            "<http://www.opengis.net/def/crs/EPSG/0/4326>POINT(48.8606 2.3376)",
            "<urn:ogc:def:crs:EPSG::4326> POINT(48.8606 2.3376)",
        ] {
            let h = wkt(s);
            assert_eq!(
                (h.crs.clone(), h.g),
                (CrsRef::Known(EPSG_4326), g.g.clone()),
                "{s}"
            );
        }
        let g = wkt("<http://www.opengis.net/def/crs/OGC/1.3/CRS84> POINT(1 2)");
        assert_eq!(g.crs, CrsRef::Known(CRS84));
        assert_eq!(g.g, Geometry::Point(Point::new(1.0, 2.0)));
        // Jena's legacy alias is longitude first
        let g = wkt("<http://www.opengis.net/def/crs/EPSG/4326> POINT(1 2)");
        assert_eq!(g.g, Geometry::Point(Point::new(1.0, 2.0)));
        assert_eq!(g.crs.iri(), "http://www.opengis.net/def/crs/EPSG/4326");
        let g = wkt("<http://example.org/crs/mars> POINT(1 1)");
        assert_eq!(g.crs, CrsRef::Unknown("http://example.org/crs/mars".into()));
        let g = wkt("<http://www.opengis.net/def/crs/EPSG/0/4979> POINT Z(1 2 3)");
        assert_eq!(g.crs, CrsRef::Known(CrsId(3)));
        assert_eq!(g.g, Geometry::Point(Point::new(2.0, 1.0)));
        assert_eq!(wkt_err("<relative> POINT(1 2)").offset, Some(1));
        assert_eq!(wkt_err("<http://x.org/crs POINT(1 2)").offset, Some(0));
    }

    #[test]
    fn whitespace_and_numbers() {
        let g = wkt("  POINT\t(\n1.5e3   -2.25E-1 )  ");
        assert_eq!(g.g, Geometry::Point(Point::new(1500.0, -0.225)));
        let g = wkt("LINESTRING(+1 .5,2. -0)");
        assert_eq!(g.vertices, 2);
        for s in [
            "POINT(NaN 1)",
            "POINT(1 nan)",
            "POINT(inf 1)",
            "POINT(Infinity 1)",
            "POINT(0x1F 2)",
            "POINT(1e999 2)",
            "POINT(1 2e)",
            "POINT(1-2)",
            "POINT(1,2)",
            "POINT(- 2)",
            "POINT(1 2",
            "POINT 1 2",
            "POINT(1 2) x",
            "POINT(1 2))",
            "(1 2)",
            "PONT(1 2)",
        ] {
            wkt_err(s);
        }
    }

    #[test]
    fn structure_errors() {
        // a missing member, and unbalanced parentheses
        let e = wkt_err("MULTIPOINT((1 2),)");
        assert_eq!(e.offset, Some(17));
        wkt_err("MULTIPOINT((1 2)");
        wkt_err("MULTIPOINT(1 2,)");
        wkt_err("MULTIPOINT(EMPTY)");
        wkt_err("MULTILINESTRING((0 0, 1 1),)");
        wkt_err("GEOMETRYCOLLECTION(POINT(1 2),)");
        // point counts and closure
        wkt_err("LINESTRING(0 0)");
        wkt_err("POLYGON((0 0, 1 0, 0 0))");
        let e = wkt_err("POLYGON((0 0, 1 0, 1 1, 0 1))");
        assert!(e.msg.contains("not closed"), "{e}");
        wkt_err("POLYGON((0 0, 10 0, 10 10, 0 0), (1 1, 2 1, 2 2))");
        wkt_err("LINEARRING(0 0, 1 0, 1 1)");
        wkt_err("TRIANGLE((0 0, 1 0, 1 1, 0 1, 0 0))");
        wkt_err("TRIANGLE((0 0, 1 0, 0 1, 0 0), (0 0, 1 0, 0 1, 0 0))");
        wkt_err("POLYGON(EMPTY)");
        // curved and unknown types
        let e = wkt_err("CIRCULARSTRING(0 0, 1 1, 2 0)");
        assert_eq!(e.msg, "unsupported geometry type CIRCULARSTRING");
        wkt_err("COMPOUNDCURVE((0 0, 1 1))");
        wkt_err("CURVEPOLYGON((0 0, 1 0, 1 1, 0 0))");
        wkt_err("BLOB(1 2)");
    }

    #[test]
    fn error_offsets() {
        // an unterminated polygon in a query constant: the offset is the end
        let s = "POLYGON((0 0, 1 1";
        assert_eq!(wkt_err(s).offset, Some(s.len()));
        assert_eq!(wkt_err("POINT(1 x)").offset, Some(8));
        assert_eq!(wkt_err("  BLOB(1 2)").offset, Some(2));
        let e = wkt_err("POINT(1 x)");
        assert_eq!(e.to_string(), "at offset 8: expected a number");
    }

    #[test]
    fn nesting_depth() {
        let nest = |n: usize| {
            format!(
                "{}POINT(1 2){}",
                "GEOMETRYCOLLECTION(".repeat(n),
                ")".repeat(n)
            )
        };
        assert_eq!(wkt(&nest(32)).vertices, 1);
        assert!(wkt_err(&nest(33)).msg.contains("nested"));
        let json_nest = |n: usize| {
            format!(
                "{}{{\"type\":\"Point\",\"coordinates\":[1,2]}}{}",
                "{\"type\":\"GeometryCollection\",\"geometries\":[".repeat(n),
                "]}".repeat(n)
            )
        };
        assert_eq!(json(&json_nest(32)).vertices, 1);
        assert!(json_err(&json_nest(33)).msg.contains("nested"));
    }

    #[test]
    fn vertex_limit() {
        let s = "LINESTRING(0 0, 1 1, 2 2)";
        assert!(parse_limited(s, WKT_LITERAL, 3).is_ok());
        let e = parse_limited(s, WKT_LITERAL, 2).unwrap_err();
        assert!(e.msg.contains("too complex"), "{e}");
        let j = r#"{"type":"MultiPoint","coordinates":[[0,0],[1,1]]}"#;
        assert!(parse_limited(j, GEOJSON_LITERAL, 1).is_err());
        assert!(parse_limited(j, GEOJSON_LITERAL, 2).is_ok());
    }

    #[test]
    fn geojson() {
        let g = json(r#"{"type":"Point","coordinates":[30,30]}"#);
        assert_eq!(
            (g.declared, g.crs.clone()),
            (GeomType::Point, CrsRef::Known(CRS84))
        );
        assert_eq!(g.g, Geometry::Point(Point::new(30.0, 30.0)));
        let g =
            json(r#"{"type":"LineString","coordinates":[[2.25,48.84],[2.30,48.86],[2.36,48.85]]}"#);
        assert_eq!((g.declared, g.vertices), (GeomType::LineString, 3));
        let g = json(
            r#"{"type":"Polygon","coordinates":[[[0,0],[1,0],[1,1],[0,0]]],"bbox":[0,0,1,1]}"#,
        );
        assert_eq!(g.declared, GeomType::Polygon);
        let g = json(r#"{"type":"MultiPoint","coordinates":[[1,1],[2,2]]}"#);
        assert_eq!(g.vertices, 2);
        json(r#"{"type":"MultiLineString","coordinates":[[[0,0],[1,1]],[[2,2],[3,3]]]}"#);
        let g = json(r#"{"type":"MultiPolygon","coordinates":[[[[0,0],[1,0],[1,1],[0,0]]]]}"#);
        assert_eq!(g.dim(), 2);
        let g = json(
            r#"{"type":"GeometryCollection","geometries":[{"type":"Point","coordinates":[1,2]}]}"#,
        );
        assert_eq!(g.declared, GeomType::GeometryCollection);
        let g = json(r#"{"type":"Point","coordinates":[1,2,3]}"#);
        assert_eq!((g.layout, g.z), (Layout::Xyz, Some((3.0, 3.0))));
        // empties
        for s in ["", " ", "null", " null "] {
            let g = json(s);
            assert!(g.empty && g.dim() == -1, "{s:?}");
        }
        let g = json(r#"{"type":"Point","coordinates":[]}"#);
        assert!(g.empty && g.declared == GeomType::Point);
        assert!(json(r#"{"type":"GeometryCollection","geometries":[]}"#).empty);
        // ill-typed
        for s in [
            r#"{"type":"Point","coordinates":[1,2],"crs":{"type":"name","properties":{"name":"EPSG:4326"}}}"#,
            r#"{"type":"Feature","geometry":{"type":"Point","coordinates":[1,2]},"properties":{}}"#,
            r#"{"type":"FeatureCollection","features":[]}"#,
            r#"{"type":"Point","coordinates":[1,2,3,4]}"#,
            r#"{"type":"Point","coordinates":[1]}"#,
            r#"{"type":"Point","coordinates":["1",2]}"#,
            r#"{"type":"LineString","coordinates":[[1,2],[3,4,5]]}"#,
            r#"{"type":"LineString","coordinates":[[1,2]]}"#,
            r#"{"type":"Polygon","coordinates":[[[0,0],[1,0],[1,1],[0,1]]]}"#,
            r#"{"type":"Polygon","coordinates":[[[0,0],[1,0],[0,0]]]}"#,
            r#"{"type":"Circle","coordinates":[1,2]}"#,
            r#"{"type":"Point"}"#,
            r#"{"coordinates":[1,2]}"#,
            r#"[1,2]"#,
            r#"{"type":"Point","coordinates":[1,2]"#,
        ] {
            json_err(s);
        }
        assert!(
            json_err(r#"{"type":"Feature","geometry":null}"#)
                .msg
                .contains("Feature")
        );
        assert!(
            json_err(r#"{"type":"Point","coordinates":[1,2],"crs":null}"#)
                .msg
                .contains("crs")
        );
        let e = json_err("{\"type\":\"Point\",\n\"coordinates\":[1,2]");
        assert!(e.offset.is_some());
    }

    #[test]
    fn other_datatypes() {
        assert!(parse("POINT(1 2)", GML_LITERAL).is_err());
        assert!(parse("<Point><coordinates>1,2</coordinates></Point>", KML_LITERAL).is_ok());
        assert!(parse("POINT(1 2)", "http://www.w3.org/2001/XMLSchema#string").is_err());
        assert!(is_geometry_datatype(WKT_LITERAL) && is_geometry_datatype(GEOJSON_LITERAL));
    }
}
