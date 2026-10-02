//! `geo:gmlLiteral` and `geo:kmlLiteral`: reading GML and KML geometries into the
//! geometry model, and writing them back.
//!
//! GML covers levels 0 and 1 of the GML 3.2 Simple Features profile (`Point`,
//! `LineString`, `LinearRing`, `Polygon`, `MultiPoint`, `MultiCurve`, `MultiSurface`,
//! `MultiGeometry`), with `Curve`, `Surface`, `Ring`, `PolyhedralSurface`, `Tin` and
//! `Envelope` when their segments and patches are linear, and the GML 2 forms
//! (`coordinates`, `outerBoundaryIs`, `MultiLineString`, `MultiPolygon`). The root
//! element's `srsName` names the CRS (CRS84 when it has none), and coordinates are in that
//! CRS's axis order, as in WKT. `srsDimension` on the root or on a `posList` gives the
//! ordinates per position (2 by default, 3 for a 3D CRS).
//!
//! KML covers the KML 2.2 geometries `Point`, `LineString`, `LinearRing`, `Polygon` and
//! `MultiGeometry`. KML is always longitude, latitude and an optional altitude on WGS 84
//! (CRS84).
//!
//! Elements are matched by their local names, whatever their namespace, so GML written
//! with an outdated or missing namespace still reads.

use super::crs::{CRS84, CrsKind, CrsRef};
use super::geom::{Geom, GeomError, GeomType};
use super::parse::{Acc, MAX_DEPTH, check_ring};
use super::write::number;
use georust::{
    Coord, Geometry, GeometryCollection, LineString, MultiLineString, MultiPoint, MultiPolygon,
    Point, Polygon,
};
use quick_xml::events::{BytesStart, Event};

/// The GML 3.2 namespace, written on GML output.
pub const GML_NS: &str = "http://www.opengis.net/gml/3.2";
/// The KML 2.2 namespace, written on KML output.
pub const KML_NS: &str = "http://www.opengis.net/kml/2.2";

/// Deepest element nesting read.
const MAX_ELEMENT_DEPTH: usize = 4 * MAX_DEPTH;

// ---------------------------------------------------------------------------------------
// A small element tree

/// An element: its local name, attributes (local names), text and children.
struct El {
    name: String,
    attrs: Vec<(String, String)>,
    text: String,
    children: Vec<El>,
}

impl El {
    fn attr(&self, name: &str) -> Option<&str> {
        self.attrs
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }

    fn child(&self, name: &str) -> Option<&El> {
        self.children.iter().find(|c| c.name == name)
    }

    fn children_named<'a>(&'a self, names: &'a [&str]) -> impl Iterator<Item = &'a El> + 'a {
        self.children
            .iter()
            .filter(move |c| names.contains(&c.name.as_str()))
    }
}

fn local(name: &[u8]) -> String {
    let n = name.rsplit(|&b| b == b':').next().unwrap_or(name);
    String::from_utf8_lossy(n).into_owned()
}

fn element(e: &BytesStart<'_>, at: usize) -> Result<El, GeomError> {
    let mut attrs = Vec::new();
    for a in e.attributes() {
        let a = a.map_err(|err| GeomError::at(at, format!("invalid XML attribute: {err}")))?;
        let key = a.key.as_ref();
        if key.starts_with(b"xmlns") {
            continue;
        }
        let v = a
            .unescape_value()
            .map_err(|err| GeomError::at(at, format!("invalid XML attribute: {err}")))?;
        attrs.push((local(key), v.into_owned()));
    }
    Ok(El {
        name: local(e.name().as_ref()),
        attrs,
        text: String::new(),
        children: Vec::new(),
    })
}

/// The root element of an XML fragment (offsets in errors are into `lex`).
fn read(lex: &str) -> Result<El, GeomError> {
    let mut r = quick_xml::Reader::from_str(lex);
    r.config_mut().trim_text(true);
    let mut stack: Vec<El> = Vec::new();
    let mut root: Option<El> = None;
    let attach = |el: El, stack: &mut Vec<El>, root: &mut Option<El>, at: usize| {
        match stack.last_mut() {
            Some(parent) => parent.children.push(el),
            None if root.is_none() => *root = Some(el),
            None => return Err(GeomError::at(at, "more than one root element")),
        }
        Ok(())
    };
    loop {
        let at = usize::try_from(r.buffer_position()).unwrap_or(0);
        let ev = r.read_event().map_err(|e| {
            GeomError::at(
                usize::try_from(r.error_position()).unwrap_or(at),
                format!("invalid XML: {e}"),
            )
        })?;
        match ev {
            Event::Start(e) => {
                if stack.len() >= MAX_ELEMENT_DEPTH {
                    return Err(GeomError::at(
                        at,
                        format!("elements nested deeper than {MAX_ELEMENT_DEPTH} levels"),
                    ));
                }
                if root.is_some() {
                    return Err(GeomError::at(at, "more than one root element"));
                }
                stack.push(element(&e, at)?);
            }
            Event::Empty(e) => {
                let el = element(&e, at)?;
                attach(el, &mut stack, &mut root, at)?;
            }
            Event::End(_) => {
                let el = stack
                    .pop()
                    .ok_or_else(|| GeomError::at(at, "unbalanced end tag"))?;
                attach(el, &mut stack, &mut root, at)?;
            }
            Event::Text(t) => {
                let s = t
                    .unescape()
                    .map_err(|e| GeomError::at(at, format!("invalid XML text: {e}")))?;
                match stack.last_mut() {
                    Some(el) => el.text.push_str(&s),
                    None if s.trim().is_empty() => {}
                    None => return Err(GeomError::at(at, "text outside the root element")),
                }
            }
            Event::CData(t) => {
                let s = t
                    .decode()
                    .map_err(|e| GeomError::at(at, format!("invalid XML text: {e}")))?;
                if let Some(el) = stack.last_mut() {
                    el.text.push_str(&s);
                }
            }
            Event::Eof => break,
            Event::Decl(_) | Event::PI(_) | Event::Comment(_) | Event::DocType(_) => {}
        }
    }
    if !stack.is_empty() {
        return Err(GeomError::at(lex.len(), "unclosed element"));
    }
    root.ok_or_else(|| GeomError::at(0, "no root element"))
}

/// Numbers separated by whitespace.
fn numbers(text: &str) -> Result<Vec<f64>, String> {
    text.split_ascii_whitespace()
        .map(|t| {
            t.parse::<f64>()
                .ok()
                .filter(|v| v.is_finite())
                .ok_or_else(|| format!("{t:?} is not a number"))
        })
        .collect()
}

// ---------------------------------------------------------------------------------------
// GML

struct Gml {
    acc: Acc,
    /// ordinates per `posList` position, unless the element says
    dim: usize,
}

/// Parse a `geo:gmlLiteral`.
pub(super) fn parse_gml(lex: &str, max_vertices: u32) -> Result<Geom, GeomError> {
    if lex.trim().is_empty() {
        return Ok(Geom::empty(
            CrsRef::Known(CRS84),
            GeomType::GeometryCollection,
        ));
    }
    let root = read(lex)?;
    let crs = match root.attr("srsName").map(str::trim) {
        Some(s) if !s.is_empty() => CrsRef::from_iri(Some(s)),
        _ => CrsRef::Known(CRS84),
    };
    let crs_dim = match crs.known().map(|c| c.kind()) {
        Some(CrsKind::Geographic3D) => 3,
        _ => 2,
    };
    let dim = match root.attr("srsDimension") {
        Some(d) => dimension(d).map_err(GeomError::new)?,
        None => crs_dim,
    };
    let mut acc = Acc::new(max_vertices);
    acc.swap = crs.known().is_some_and(|id| id.lat_first());
    let mut p = Gml { acc, dim };
    let (declared, g) = p.geometry(&root, 0).map_err(GeomError::new)?;
    Ok(p.acc.finish(crs, declared, g))
}

fn dimension(d: &str) -> Result<usize, String> {
    match d.trim().parse::<usize>() {
        Ok(n @ 2..=4) => Ok(n),
        _ => Err(format!("srsDimension {d:?} is not 2, 3 or 4")),
    }
}

impl Gml {
    fn geometry(&mut self, el: &El, depth: usize) -> Result<(GeomType, Geometry<f64>), String> {
        if depth > MAX_DEPTH {
            return Err(format!("geometries nested deeper than {MAX_DEPTH} levels"));
        }
        Ok(match el.name.as_str() {
            "Point" => {
                let cs = self.positions(el)?;
                match cs.as_slice() {
                    [] => (GeomType::Point, MultiPoint::new(vec![]).into()),
                    [c] => (GeomType::Point, Point(*c).into()),
                    _ => return Err("a gml:Point has more than one position".into()),
                }
            }
            "LineString" => (GeomType::LineString, self.line(el)?.into()),
            "LinearRing" => (GeomType::LinearRing, self.ring(el)?.into()),
            "Ring" => (GeomType::LinearRing, self.ring(el)?.into()),
            "Curve" | "OrientableCurve" | "CompositeCurve" => {
                (GeomType::LineString, self.curve(el, depth)?.into())
            }
            "Polygon" | "PolygonPatch" | "Triangle" => {
                (GeomType::Polygon, self.polygon(el, depth)?.into())
            }
            "Envelope" => (GeomType::Polygon, self.envelope(el)?.into()),
            "Surface" | "OrientableSurface" | "CompositeSurface" => {
                let ps = self.patches(el, depth)?;
                if ps.len() == 1 {
                    let p = ps.into_iter().next().expect("one patch");
                    (GeomType::Polygon, p.into())
                } else {
                    (GeomType::MultiPolygon, MultiPolygon::new(ps).into())
                }
            }
            "PolyhedralSurface" => (
                GeomType::PolyhedralSurface,
                MultiPolygon::new(self.patches(el, depth)?).into(),
            ),
            "Tin" | "TriangulatedSurface" => (
                GeomType::Tin,
                MultiPolygon::new(self.patches(el, depth)?).into(),
            ),
            "MultiPoint" => {
                let mut ps = Vec::new();
                for m in self.members(el, &["pointMember", "pointMembers"]) {
                    match self.geometry(m, depth + 1)? {
                        (_, Geometry::Point(p)) => ps.push(p),
                        (_, Geometry::MultiPoint(e)) if e.0.is_empty() => {}
                        _ => return Err("a gml:MultiPoint member is not a point".into()),
                    }
                }
                (GeomType::MultiPoint, MultiPoint::new(ps).into())
            }
            "MultiCurve" | "MultiLineString" => {
                let mut ls = Vec::new();
                for m in self.members(el, &["curveMember", "curveMembers", "lineStringMember"]) {
                    match self.geometry(m, depth + 1)? {
                        (_, Geometry::LineString(l)) => ls.push(l),
                        _ => return Err("a gml:MultiCurve member is not a curve".into()),
                    }
                }
                (GeomType::MultiLineString, MultiLineString::new(ls).into())
            }
            "MultiSurface" | "MultiPolygon" => {
                let mut ps = Vec::new();
                for m in self.members(el, &["surfaceMember", "surfaceMembers", "polygonMember"]) {
                    match self.geometry(m, depth + 1)? {
                        (_, Geometry::Polygon(p)) => ps.push(p),
                        (_, Geometry::MultiPolygon(mp)) => ps.extend(mp.0),
                        _ => return Err("a gml:MultiSurface member is not a surface".into()),
                    }
                }
                (GeomType::MultiPolygon, MultiPolygon::new(ps).into())
            }
            "MultiGeometry" => {
                let mut gs = Vec::new();
                for m in self.members(el, &["geometryMember", "geometryMembers"]) {
                    gs.push(self.geometry(m, depth + 1)?.1);
                }
                (
                    GeomType::GeometryCollection,
                    Geometry::GeometryCollection(GeometryCollection::new_from(gs)),
                )
            }
            other => return Err(format!("gml:{other} is not a supported GML geometry")),
        })
    }

    /// The geometries of the member properties `names` of a multi-geometry: one per
    /// `…Member`, any number per `…Members`.
    fn members<'a>(&self, el: &'a El, names: &'a [&str]) -> Vec<&'a El> {
        el.children_named(names)
            .flat_map(|m| m.children.iter())
            .collect()
    }

    /// The positions of an element: a `posList`, `pos` elements, GML 2 `coordinates`,
    /// or `pointProperty`/`pointRep` points.
    fn positions(&mut self, el: &El) -> Result<Vec<Coord<f64>>, String> {
        let mut out = Vec::new();
        for c in &el.children {
            match c.name.as_str() {
                "posList" => {
                    let dim = match c.attr("srsDimension") {
                        Some(d) => dimension(d)?,
                        None => self.dim,
                    };
                    let ns = numbers(&c.text)?;
                    if ns.len() % dim != 0 {
                        return Err(format!(
                            "a gml:posList has {} numbers, not a multiple of {dim}",
                            ns.len()
                        ));
                    }
                    for p in ns.chunks(dim) {
                        out.push(self.acc.coord(p)?);
                    }
                }
                "pos" => {
                    let ns = numbers(&c.text)?;
                    if !ns.is_empty() {
                        out.push(self.acc.coord(&ns)?);
                    }
                }
                "coordinates" => {
                    let cs = c.attr("cs").unwrap_or(",");
                    let decimal = c.attr("decimal").unwrap_or(".");
                    let tuples: Vec<&str> = match c.attr("ts") {
                        Some(ts) if !ts.trim().is_empty() => c.text.split(ts).collect(),
                        _ => c.text.split_ascii_whitespace().collect(),
                    };
                    for t in tuples.into_iter().map(str::trim).filter(|t| !t.is_empty()) {
                        let ords = t
                            .split(cs)
                            .map(|o| {
                                let o = o.trim().replace(decimal, ".");
                                o.parse::<f64>()
                                    .ok()
                                    .filter(|v| v.is_finite())
                                    .ok_or_else(|| format!("{o:?} is not a number"))
                            })
                            .collect::<Result<Vec<f64>, String>>()?;
                        out.push(self.acc.coord(&ords)?);
                    }
                }
                "pointProperty" | "pointRep" => {
                    for p in &c.children {
                        out.extend(self.positions(p)?);
                    }
                }
                _ => {}
            }
        }
        Ok(out)
    }

    fn line(&mut self, el: &El) -> Result<LineString<f64>, String> {
        let cs = self.positions(el)?;
        if cs.len() == 1 {
            return Err("a line has 1 point, at least 2 needed".into());
        }
        Ok(LineString::new(cs))
    }

    /// A `LinearRing`, or a `Ring` of curve members.
    fn ring(&mut self, el: &El) -> Result<LineString<f64>, String> {
        let cs = if el.name == "Ring" {
            let mut cs: Vec<Coord<f64>> = Vec::new();
            for m in self.members(el, &["curveMember"]) {
                let part = self.curve_or_line(m, 1)?;
                join(&mut cs, part.0);
            }
            cs
        } else {
            self.positions(el)?
        };
        if !cs.is_empty() {
            check_ring(&cs)?;
        }
        Ok(LineString::new(cs))
    }

    fn curve_or_line(&mut self, el: &El, depth: usize) -> Result<LineString<f64>, String> {
        match self.geometry(el, depth)? {
            (_, Geometry::LineString(l)) => Ok(l),
            _ => Err(format!("gml:{} is not a curve", el.name)),
        }
    }

    /// A `Curve` of linear segments (`LineStringSegment`), or the curve members of an
    /// orientable or composite curve, joined end to end.
    fn curve(&mut self, el: &El, depth: usize) -> Result<LineString<f64>, String> {
        let mut cs: Vec<Coord<f64>> = Vec::new();
        if let Some(segs) = el.child("segments") {
            for s in &segs.children {
                if s.name != "LineStringSegment" {
                    return Err(format!(
                        "gml:{} segments are not supported (only gml:LineStringSegment)",
                        s.name
                    ));
                }
                let part = self.positions(s)?;
                join(&mut cs, part);
            }
        } else {
            for m in self.members(el, &["baseCurve", "curveMember"]) {
                let part = self.curve_or_line(m, depth + 1)?;
                join(&mut cs, part.0);
            }
        }
        if cs.len() == 1 {
            return Err("a curve has 1 point, at least 2 needed".into());
        }
        Ok(LineString::new(cs))
    }

    /// A `Polygon` or polygon patch: its exterior and interior rings (GML 3) or outer and
    /// inner boundaries (GML 2).
    fn polygon(&mut self, el: &El, _depth: usize) -> Result<Polygon<f64>, String> {
        let mut exterior = LineString::new(vec![]);
        let mut interiors = Vec::new();
        for c in &el.children {
            let outer = match c.name.as_str() {
                "exterior" | "outerBoundaryIs" => true,
                "interior" | "innerBoundaryIs" => false,
                _ => continue,
            };
            for r in &c.children {
                let ring = self.ring(r)?;
                if outer {
                    exterior = ring;
                } else if !ring.0.is_empty() {
                    interiors.push(ring);
                }
            }
        }
        if exterior.0.is_empty() && !interiors.is_empty() {
            return Err("a gml:Polygon has interior rings but no exterior".into());
        }
        Ok(Polygon::new(exterior, interiors))
    }

    /// The polygon patches of a surface (`patches`, `polygonPatches`, `trianglePatches`),
    /// or the surface members of an orientable or composite surface.
    fn patches(&mut self, el: &El, depth: usize) -> Result<Vec<Polygon<f64>>, String> {
        let mut out = Vec::new();
        for c in el.children_named(&["patches", "polygonPatches", "trianglePatches"]) {
            for p in &c.children {
                if !matches!(p.name.as_str(), "PolygonPatch" | "Triangle" | "Rectangle") {
                    return Err(format!("gml:{} patches are not supported", p.name));
                }
                let poly = self.polygon(p, depth + 1)?;
                if !poly.exterior().0.is_empty() {
                    out.push(poly);
                }
            }
        }
        for m in self.members(el, &["baseSurface", "surfaceMember"]) {
            match self.geometry(m, depth + 1)? {
                (_, Geometry::Polygon(p)) => out.push(p),
                (_, Geometry::MultiPolygon(mp)) => out.extend(mp.0),
                _ => return Err(format!("a gml:{} member is not a surface", el.name)),
            }
        }
        Ok(out)
    }

    /// An `Envelope` as the rectangle of its corners.
    fn envelope(&mut self, el: &El) -> Result<Polygon<f64>, String> {
        let corner = |name: &str| -> Result<Vec<f64>, String> {
            numbers(
                &el.child(name)
                    .ok_or(format!("a gml:Envelope needs gml:{name}"))?
                    .text,
            )
        };
        let (lo, hi) = (corner("lowerCorner")?, corner("upperCorner")?);
        if lo.len() != hi.len() || lo.len() < 2 {
            return Err("the corners of a gml:Envelope do not match".into());
        }
        let a = self.acc.coord(&lo)?;
        let b = self.acc.coord(&hi)?;
        let (lo, hi) = (
            Coord {
                x: a.x.min(b.x),
                y: a.y.min(b.y),
            },
            Coord {
                x: a.x.max(b.x),
                y: a.y.max(b.y),
            },
        );
        let ring = vec![
            lo,
            Coord { x: hi.x, y: lo.y },
            hi,
            Coord { x: lo.x, y: hi.y },
            lo,
        ];
        Ok(Polygon::new(LineString::new(ring), vec![]))
    }
}

/// Append a curve part, dropping its first position when it repeats the last one.
fn join(cs: &mut Vec<Coord<f64>>, part: Vec<Coord<f64>>) {
    let skip = usize::from(!cs.is_empty() && cs.last() == part.first());
    cs.extend(part.into_iter().skip(skip));
}

// ---------------------------------------------------------------------------------------
// KML

/// Parse a `geo:kmlLiteral`.
pub(super) fn parse_kml(lex: &str, max_vertices: u32) -> Result<Geom, GeomError> {
    let crs = CrsRef::Known(CRS84);
    if lex.trim().is_empty() {
        return Ok(Geom::empty(crs, GeomType::GeometryCollection));
    }
    let root = read(lex)?;
    let mut acc = Acc::new(max_vertices);
    let (declared, g) = kml_geometry(&root, 0, &mut acc).map_err(GeomError::new)?;
    Ok(acc.finish(crs, declared, g))
}

fn kml_coordinates(el: &El, acc: &mut Acc) -> Result<Vec<Coord<f64>>, String> {
    let Some(c) = el.child("coordinates") else {
        return Ok(Vec::new());
    };
    c.text
        .split_ascii_whitespace()
        .map(|t| {
            let ords = t
                .split(',')
                .map(|o| {
                    o.parse::<f64>()
                        .ok()
                        .filter(|v| v.is_finite())
                        .ok_or_else(|| format!("{o:?} is not a number"))
                })
                .collect::<Result<Vec<f64>, String>>()?;
            acc.coord(&ords)
        })
        .collect()
}

fn kml_ring(el: &El, acc: &mut Acc) -> Result<LineString<f64>, String> {
    let cs = kml_coordinates(el, acc)?;
    if !cs.is_empty() {
        check_ring(&cs)?;
    }
    Ok(LineString::new(cs))
}

fn kml_geometry(el: &El, depth: usize, acc: &mut Acc) -> Result<(GeomType, Geometry<f64>), String> {
    if depth > MAX_DEPTH {
        return Err(format!("geometries nested deeper than {MAX_DEPTH} levels"));
    }
    Ok(match el.name.as_str() {
        "Point" => {
            let cs = kml_coordinates(el, acc)?;
            match cs.as_slice() {
                [] => (GeomType::Point, MultiPoint::new(vec![]).into()),
                [c] => (GeomType::Point, Point(*c).into()),
                _ => return Err("a KML Point has more than one position".into()),
            }
        }
        "LineString" => {
            let cs = kml_coordinates(el, acc)?;
            if cs.len() == 1 {
                return Err("a line has 1 point, at least 2 needed".into());
            }
            (GeomType::LineString, LineString::new(cs).into())
        }
        "LinearRing" => (GeomType::LinearRing, kml_ring(el, acc)?.into()),
        "Polygon" => {
            let mut exterior = LineString::new(vec![]);
            let mut interiors = Vec::new();
            for c in &el.children {
                let outer = match c.name.as_str() {
                    "outerBoundaryIs" => true,
                    "innerBoundaryIs" => false,
                    _ => continue,
                };
                for r in c.children.iter().filter(|r| r.name == "LinearRing") {
                    let ring = kml_ring(r, acc)?;
                    if outer {
                        exterior = ring;
                    } else if !ring.0.is_empty() {
                        interiors.push(ring);
                    }
                }
            }
            if exterior.0.is_empty() && !interiors.is_empty() {
                return Err("a KML Polygon has inner boundaries but no outer one".into());
            }
            (GeomType::Polygon, Polygon::new(exterior, interiors).into())
        }
        "MultiGeometry" => {
            let mut gs = Vec::new();
            for c in &el.children {
                gs.push(kml_geometry(c, depth + 1, acc)?.1);
            }
            (
                GeomType::GeometryCollection,
                Geometry::GeometryCollection(GeometryCollection::new_from(gs)),
            )
        }
        other => return Err(format!("{other} is not a supported KML geometry")),
    })
}

// ---------------------------------------------------------------------------------------
// Writing

/// GML 3.2 in the geometry's CRS and axis order, with `srsName` on the root element
/// (the Simple Features profile's elements; `MultiLineString` and `MultiPolygon` as
/// `MultiCurve` and `MultiSurface`). Coordinates are 2D.
pub fn to_gml(g: &Geom) -> String {
    let swap = g.crs.known().is_some_and(|id| id.lat_first());
    let mut w = GmlWriter {
        out: String::new(),
        swap,
        root: Some(g.crs.iri().to_string()),
    };
    if g.empty {
        let name = match g.declared {
            GeomType::Point => "Point",
            GeomType::LineString => "LineString",
            GeomType::LinearRing => "LinearRing",
            GeomType::Polygon | GeomType::Triangle => "Polygon",
            GeomType::MultiPoint => "MultiPoint",
            GeomType::MultiLineString => "MultiCurve",
            GeomType::MultiPolygon => "MultiSurface",
            GeomType::PolyhedralSurface => "PolyhedralSurface",
            GeomType::Tin => "Tin",
            GeomType::GeometryCollection => "MultiGeometry",
        };
        w.open(name);
        match name {
            "Point" => w.out.push_str("<gml:pos/>"),
            "LineString" | "LinearRing" => w.out.push_str("<gml:posList/>"),
            _ => {}
        }
        w.close(name);
    } else {
        w.write(&g.g, Some(g.declared));
    }
    w.out
}

struct GmlWriter {
    out: String,
    swap: bool,
    /// the root's `srsName`, until the root is written
    root: Option<String>,
}

impl GmlWriter {
    fn open(&mut self, name: &str) {
        self.out.push_str("<gml:");
        self.out.push_str(name);
        if let Some(srs) = self.root.take() {
            self.out.push_str(" xmlns:gml=\"");
            self.out.push_str(GML_NS);
            self.out.push_str("\" srsName=\"");
            escape_into(&mut self.out, &srs);
            self.out.push('"');
        }
        self.out.push('>');
    }

    fn close(&mut self, name: &str) {
        self.out.push_str("</gml:");
        self.out.push_str(name);
        self.out.push('>');
    }

    fn coord(&mut self, c: Coord<f64>) {
        let (a, b) = if self.swap { (c.y, c.x) } else { (c.x, c.y) };
        number(&mut self.out, a);
        self.out.push(' ');
        number(&mut self.out, b);
    }

    fn pos_list(&mut self, cs: &[Coord<f64>]) {
        self.out.push_str("<gml:posList>");
        for (i, c) in cs.iter().enumerate() {
            if i > 0 {
                self.out.push(' ');
            }
            self.coord(*c);
        }
        self.out.push_str("</gml:posList>");
    }

    fn ring(&mut self, r: &LineString<f64>) {
        self.open("LinearRing");
        self.pos_list(&r.0);
        self.close("LinearRing");
    }

    fn polygon_body(&mut self, p: &Polygon<f64>) {
        if p.exterior().0.is_empty() {
            return;
        }
        self.out.push_str("<gml:exterior>");
        self.ring(p.exterior());
        self.out.push_str("</gml:exterior>");
        for r in p.interiors() {
            self.out.push_str("<gml:interior>");
            self.ring(r);
            self.out.push_str("</gml:interior>");
        }
    }

    fn polygon(&mut self, name: &str, p: &Polygon<f64>) {
        self.open(name);
        self.polygon_body(p);
        self.close(name);
    }

    fn write(&mut self, g: &Geometry<f64>, declared: Option<GeomType>) {
        use Geometry as G;
        match g {
            G::Point(p) => {
                self.open("Point");
                self.out.push_str("<gml:pos>");
                self.coord(p.0);
                self.out.push_str("</gml:pos>");
                self.close("Point");
            }
            G::Line(l) => self.write(&LineString::new(vec![l.start, l.end]).into(), None),
            G::LineString(l) => {
                let name = if declared == Some(GeomType::LinearRing) {
                    "LinearRing"
                } else {
                    "LineString"
                };
                self.open(name);
                self.pos_list(&l.0);
                self.close(name);
            }
            G::Polygon(p) => self.polygon("Polygon", p),
            G::Rect(r) => self.polygon("Polygon", &r.to_polygon()),
            G::Triangle(t) => self.polygon("Polygon", &t.to_polygon()),
            G::MultiPoint(m) => {
                self.open("MultiPoint");
                for p in &m.0 {
                    self.out.push_str("<gml:pointMember>");
                    self.write(&(*p).into(), None);
                    self.out.push_str("</gml:pointMember>");
                }
                self.close("MultiPoint");
            }
            G::MultiLineString(m) => {
                self.open("MultiCurve");
                for l in &m.0 {
                    self.out.push_str("<gml:curveMember>");
                    self.write(&l.clone().into(), None);
                    self.out.push_str("</gml:curveMember>");
                }
                self.close("MultiCurve");
            }
            G::MultiPolygon(m) => {
                let (name, patch) = match declared {
                    Some(GeomType::PolyhedralSurface) => ("PolyhedralSurface", "PolygonPatch"),
                    Some(GeomType::Tin) => ("Tin", "Triangle"),
                    _ => ("MultiSurface", ""),
                };
                self.open(name);
                if patch.is_empty() {
                    for p in &m.0 {
                        self.out.push_str("<gml:surfaceMember>");
                        self.polygon("Polygon", p);
                        self.out.push_str("</gml:surfaceMember>");
                    }
                } else {
                    self.out.push_str("<gml:patches>");
                    for p in &m.0 {
                        self.out.push_str("<gml:");
                        self.out.push_str(patch);
                        self.out.push('>');
                        self.polygon_body(p);
                        self.close(patch);
                    }
                    self.out.push_str("</gml:patches>");
                }
                self.close(name);
            }
            G::GeometryCollection(c) => {
                self.open("MultiGeometry");
                for m in &c.0 {
                    self.out.push_str("<gml:geometryMember>");
                    self.write(m, None);
                    self.out.push_str("</gml:geometryMember>");
                }
                self.close("MultiGeometry");
            }
        }
    }
}

fn escape_into(out: &mut String, s: &str) {
    for ch in s.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            c => out.push(c),
        }
    }
}

/// KML 2.2 (longitude, latitude on WGS 84: a projected geometry is transformed; callers
/// check that the CRS is known). Multi-geometries are `MultiGeometry`, and an empty
/// geometry is an empty `MultiGeometry`.
pub fn to_kml(g: &Geom) -> String {
    let to84 = match g.crs.known() {
        Some(id) if !id.is_geographic() => Some(id),
        _ => None,
    };
    let mut w = KmlWriter {
        out: String::new(),
        to84,
        root: true,
    };
    if g.empty {
        w.open("MultiGeometry");
        w.close("MultiGeometry");
    } else {
        w.write(&g.g, Some(g.declared));
    }
    w.out
}

struct KmlWriter {
    out: String,
    to84: Option<super::crs::CrsId>,
    root: bool,
}

impl KmlWriter {
    fn open(&mut self, name: &str) {
        self.out.push('<');
        self.out.push_str(name);
        if std::mem::take(&mut self.root) {
            self.out.push_str(" xmlns=\"");
            self.out.push_str(KML_NS);
            self.out.push('"');
        }
        self.out.push('>');
    }

    fn close(&mut self, name: &str) {
        self.out.push_str("</");
        self.out.push_str(name);
        self.out.push('>');
    }

    fn coordinates(&mut self, cs: &[Coord<f64>]) {
        self.out.push_str("<coordinates>");
        for (i, c) in cs.iter().enumerate() {
            if i > 0 {
                self.out.push(' ');
            }
            let (x, y) = match self.to84 {
                Some(id) => super::crs::to_lonlat(id, c.x, c.y).unwrap_or((c.x, c.y)),
                None => (c.x, c.y),
            };
            number(&mut self.out, x);
            self.out.push(',');
            number(&mut self.out, y);
        }
        self.out.push_str("</coordinates>");
    }

    fn ring(&mut self, r: &LineString<f64>) {
        self.open("LinearRing");
        self.coordinates(&r.0);
        self.close("LinearRing");
    }

    fn polygon(&mut self, p: &Polygon<f64>) {
        self.open("Polygon");
        if !p.exterior().0.is_empty() {
            self.out.push_str("<outerBoundaryIs>");
            self.ring(p.exterior());
            self.out.push_str("</outerBoundaryIs>");
            for r in p.interiors() {
                self.out.push_str("<innerBoundaryIs>");
                self.ring(r);
                self.out.push_str("</innerBoundaryIs>");
            }
        }
        self.close("Polygon");
    }

    fn write(&mut self, g: &Geometry<f64>, declared: Option<GeomType>) {
        use Geometry as G;
        match g {
            G::Point(p) => {
                self.open("Point");
                self.coordinates(&[p.0]);
                self.close("Point");
            }
            G::Line(l) => self.write(&LineString::new(vec![l.start, l.end]).into(), None),
            G::LineString(l) if declared == Some(GeomType::LinearRing) => self.ring(l),
            G::LineString(l) => {
                self.open("LineString");
                self.coordinates(&l.0);
                self.close("LineString");
            }
            G::Polygon(p) => self.polygon(p),
            G::Rect(r) => self.polygon(&r.to_polygon()),
            G::Triangle(t) => self.polygon(&t.to_polygon()),
            G::MultiPoint(m) => {
                self.open("MultiGeometry");
                for p in &m.0 {
                    self.write(&(*p).into(), None);
                }
                self.close("MultiGeometry");
            }
            G::MultiLineString(m) => {
                self.open("MultiGeometry");
                for l in &m.0 {
                    self.write(&l.clone().into(), None);
                }
                self.close("MultiGeometry");
            }
            G::MultiPolygon(m) => {
                self.open("MultiGeometry");
                for p in &m.0 {
                    self.polygon(p);
                }
                self.close("MultiGeometry");
            }
            G::GeometryCollection(c) => {
                self.open("MultiGeometry");
                for m in &c.0 {
                    self.write(m, None);
                }
                self.close("MultiGeometry");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geo::crs::EPSG_4326;
    use crate::geo::parse::parse;
    use crate::geo::vocab::{GML_LITERAL, KML_LITERAL, WKT_LITERAL};

    fn gml(s: &str) -> Geom {
        parse(s, GML_LITERAL).unwrap_or_else(|e| panic!("{s}: {e}"))
    }

    fn kml(s: &str) -> Geom {
        parse(s, KML_LITERAL).unwrap_or_else(|e| panic!("{s}: {e}"))
    }

    fn wkt(g: &Geom) -> String {
        crate::geo::write::to_wkt(g)
    }

    /// The Annex B example of the GeoSPARQL 1.1 specification in its four serializations.
    pub(crate) const SPEC_WKT_POLYGON: &str = "<http://www.opengis.net/def/crs/EPSG/0/4326>
            POLYGON ((
                153.3610112 -27.0621757,
                153.3658177 -27.1990606,
                153.421436 -27.3406573,
                153.4269292 -27.3607835,
                153.4434087 -27.3315078,
                153.4183848 -27.2913403,
                153.4189391 -27.2039578,
                153.4673476 -27.0267166,
                153.3610112 -27.0621757
            ))";
    pub(crate) const SPEC_GML_POLYGON: &str = r#"<gml:Polygon
                srsName="http://www.opengis.net/def/crs/EPSG/0/4326">
                <gml:exterior>
                    <gml:LinearRing>
                        <gml:posList>
                            -27.0621757 153.3610112
                            -27.1990606 153.3658177
                            -27.3406573 153.421436
                            -27.3607835 153.4269292
                            -27.3315078 153.4434087
                            -27.2913403 153.4183848
                            -27.2039578 153.4189391
                            -27.0267166 153.4673476
                            -27.0621757 153.3610112
                        </gml:posList>
                    </gml:LinearRing>
                </gml:exterior>
            </gml:Polygon>"#;
    pub(crate) const SPEC_KML_POLYGON: &str = "<Polygon>
                <outerBoundaryIs>
                    <LinearRing>
                        <coordinates>
                        153.3610112,-27.0621757
                        153.3658177,-27.1990606
                        153.421436,-27.3406573
                        153.4269292,-27.3607835
                        153.4434087,-27.3315078
                        153.4183848,-27.2913403
                        153.4189391,-27.2039578
                        153.4673476,-27.0267166
                        153.3610112,-27.0621757
                        </coordinates>
                    </LinearRing>
                </outerBoundaryIs>
            </Polygon>";
    pub(crate) const SPEC_GEOJSON_POLYGON: &str = r#"{
                "type": "Polygon",
                "coordinates": [[
                    [153.3610112, -27.0621757],
                    [153.3658177, -27.1990606],
                    [153.421436, -27.3406573],
                    [153.4269292, -27.3607835],
                    [153.4434087, -27.3315078],
                    [153.4183848, -27.2913403],
                    [153.4189391, -27.2039578],
                    [153.4673476, -27.0267166],
                    [153.3610112, -27.0621757]
                ]]
            }"#;

    /// The GML and KML examples of the GeoSPARQL 1.1 specification (OGC 22-047r1,
    /// §10.8.2.1 and §10.8.4.1, and the Annex B geometry with one literal of each
    /// serialization), as written there.
    #[test]
    fn specification_examples() {
        let g = gml(r#"<gml:Point
        srsName="http://www.opengis.net/def/crs/OGC/1.3/CRS84"
        xmlns:gml="http://www.opengis.net/gml/3.2">
    <gml:pos>-83.38 33.95</gml:pos>
</gml:Point>"#);
        assert_eq!(wkt(&g), "POINT(-83.38 33.95)");
        let k = kml(r#"<Point xmlns="http://www.opengis.net/kml/2.2">
    <coordinates>-83.38,33.95</coordinates>
</Point>"#);
        assert_eq!(wkt(&k), "POINT(-83.38 33.95)");
        // Annex B: EPSG:4326 in GML is latitude first, KML is longitude first
        let g = gml(SPEC_GML_POLYGON);
        assert_eq!(g.crs, CrsRef::Known(EPSG_4326));
        assert_eq!(g.declared, GeomType::Polygon);
        assert_eq!(g.vertices, 9);
        assert_eq!(g.bbox84().unwrap()[0], 153.3610112);
        let k = kml(SPEC_KML_POLYGON);
        let j = parse(SPEC_GEOJSON_POLYGON, crate::geo::vocab::GEOJSON_LITERAL).unwrap();
        for other in [&k, &j] {
            assert_eq!(other.g, g.g);
        }
        // The example's WKT names EPSG:4326 but writes longitude first, against the
        // standard's own axis-order requirement, so it reads as latitude 153: the
        // same coordinates swapped.
        let w = parse(SPEC_WKT_POLYGON, WKT_LITERAL).unwrap();
        assert_eq!(
            w.g,
            georust::MapCoords::map_coords(&g.g, |c| Coord { x: c.y, y: c.x })
        );
    }

    /// Points and polygons in GML 3.2 and KML 2.2.
    #[test]
    fn more_examples() {
        // a point in WGS 84 latitude, longitude order
        let g = gml(r#"<gml:Point xmlns:gml="http://www.opengis.net/gml/3.2"
                 srsName="http://www.opengis.net/def/crs/EPSG/0/4326">
                 <gml:pos>33.95 -83.38</gml:pos>
               </gml:Point>"#);
        assert_eq!(g.crs, CrsRef::Known(EPSG_4326));
        assert_eq!(
            wkt(&g),
            "<http://www.opengis.net/def/crs/EPSG/0/4326> POINT(33.95 -83.38)"
        );
        // internal order is longitude, latitude
        assert_eq!(g.bbox84().unwrap()[0], -83.38);
        // a polygon with a posList
        let g = gml(r#"<gml:Polygon xmlns:gml="http://www.opengis.net/gml/3.2"
                 srsName="http://www.opengis.net/def/crs/OGC/1.3/CRS84">
                 <gml:exterior><gml:LinearRing>
                   <gml:posList srsDimension="2">-83.6 34.1 -83.2 34.1 -83.2 34.5 -83.6 34.5 -83.6 34.1</gml:posList>
                 </gml:LinearRing></gml:exterior>
               </gml:Polygon>"#);
        assert_eq!(
            wkt(&g),
            "POLYGON((-83.6 34.1, -83.2 34.1, -83.2 34.5, -83.6 34.5, -83.6 34.1))"
        );
        assert_eq!(g.declared, GeomType::Polygon);
        // KML: longitude, latitude, altitude
        let g = kml(
            r#"<Point xmlns="http://www.opengis.net/kml/2.2"><coordinates>-83.38,33.95,0</coordinates></Point>"#,
        );
        assert_eq!(wkt(&g), "POINT(-83.38 33.95)");
        assert!(g.layout.has_z());
        let g = kml(
            "<Polygon xmlns=\"http://www.opengis.net/kml/2.2\"><outerBoundaryIs><LinearRing>\
             <coordinates>-83.6,34.1 -83.2,34.1 -83.2,34.5 -83.6,34.5 -83.6,34.1</coordinates>\
             </LinearRing></outerBoundaryIs></Polygon>",
        );
        assert_eq!(
            wkt(&g),
            "POLYGON((-83.6 34.1, -83.2 34.1, -83.2 34.5, -83.6 34.5, -83.6 34.1))"
        );
    }

    #[test]
    fn gml_forms() {
        let cases = [
            (
                "<gml:LineString><gml:pos>0 0</gml:pos><gml:pos>1 1</gml:pos></gml:LineString>",
                "LINESTRING(0 0, 1 1)",
            ),
            (
                "<LineString xmlns=\"http://www.opengis.net/gml\"><coordinates>0,0 1,1</coordinates></LineString>",
                "LINESTRING(0 0, 1 1)",
            ),
            (
                "<gml:Curve><gml:segments><gml:LineStringSegment><gml:posList>0 0 1 1</gml:posList></gml:LineStringSegment>\
                 <gml:LineStringSegment><gml:posList>1 1 2 0</gml:posList></gml:LineStringSegment></gml:segments></gml:Curve>",
                "LINESTRING(0 0, 1 1, 2 0)",
            ),
            (
                "<gml:Polygon><gml:outerBoundaryIs><gml:LinearRing><gml:coordinates>0,0 4,0 4,4 0,0</gml:coordinates>\
                 </gml:LinearRing></gml:outerBoundaryIs><gml:innerBoundaryIs><gml:LinearRing>\
                 <gml:coordinates>1,1 2,1 2,2 1,1</gml:coordinates></gml:LinearRing></gml:innerBoundaryIs></gml:Polygon>",
                "POLYGON((0 0, 4 0, 4 4, 0 0), (1 1, 2 1, 2 2, 1 1))",
            ),
            (
                "<gml:Surface><gml:patches><gml:PolygonPatch><gml:exterior><gml:LinearRing>\
                 <gml:posList>0 0 1 0 1 1 0 0</gml:posList></gml:LinearRing></gml:exterior></gml:PolygonPatch>\
                 </gml:patches></gml:Surface>",
                "POLYGON((0 0, 1 0, 1 1, 0 0))",
            ),
            (
                "<gml:MultiPoint><gml:pointMember><gml:Point><gml:pos>1 2</gml:pos></gml:Point></gml:pointMember>\
                 <gml:pointMembers><gml:Point><gml:pos>3 4</gml:pos></gml:Point><gml:Point><gml:pos>5 6</gml:pos></gml:Point>\
                 </gml:pointMembers></gml:MultiPoint>",
                "MULTIPOINT((1 2), (3 4), (5 6))",
            ),
            (
                "<gml:MultiCurve><gml:curveMember><gml:LineString><gml:posList>0 0 1 1</gml:posList></gml:LineString>\
                 </gml:curveMember></gml:MultiCurve>",
                "MULTILINESTRING((0 0, 1 1))",
            ),
            (
                "<gml:MultiSurface><gml:surfaceMember><gml:Polygon><gml:exterior><gml:LinearRing>\
                 <gml:posList>0 0 1 0 1 1 0 0</gml:posList></gml:LinearRing></gml:exterior></gml:Polygon>\
                 </gml:surfaceMember></gml:MultiSurface>",
                "MULTIPOLYGON(((0 0, 1 0, 1 1, 0 0)))",
            ),
            (
                "<gml:MultiGeometry><gml:geometryMember><gml:Point><gml:pos>1 2</gml:pos></gml:Point></gml:geometryMember>\
                 <gml:geometryMember><gml:LineString><gml:posList>0 0 1 1</gml:posList></gml:LineString>\
                 </gml:geometryMember></gml:MultiGeometry>",
                "GEOMETRYCOLLECTION(POINT(1 2), LINESTRING(0 0, 1 1))",
            ),
            (
                "<gml:Envelope><gml:lowerCorner>0 1</gml:lowerCorner><gml:upperCorner>2 3</gml:upperCorner></gml:Envelope>",
                "POLYGON((0 1, 2 1, 2 3, 0 3, 0 1))",
            ),
            (
                "<gml:Point srsDimension=\"3\"><gml:pos>1 2 3</gml:pos></gml:Point>",
                "POINT(1 2)",
            ),
            (
                "<gml:LineString><gml:posList srsDimension=\"3\">0 0 5 1 1 6</gml:posList></gml:LineString>",
                "LINESTRING(0 0, 1 1)",
            ),
        ];
        for (src, want) in cases {
            assert_eq!(wkt(&gml(src)), want, "{src}");
        }
        // empty geometries, as the Compliance Benchmark writes them
        for (src, ty) in [
            (
                "\n  <LineString xmlns=\"https://www.opengis.net/gml\"><posList></posList></LineString>\n",
                GeomType::LineString,
            ),
            (
                "<Point xmlns=\"https://www.opengis.net/gml\"><posList></posList></Point>",
                GeomType::Point,
            ),
            ("<gml:Polygon/>", GeomType::Polygon),
            ("", GeomType::GeometryCollection),
        ] {
            let g = gml(src);
            assert!(g.empty, "{src}");
            assert_eq!(g.declared, ty, "{src}");
        }
    }

    #[test]
    fn gml_errors() {
        for (src, msg) in [
            ("<gml:Point><gml:pos>1</gml:pos></gml:Point>", "ordinates"),
            (
                "<gml:LineString><gml:posList>0 0 1</gml:posList></gml:LineString>",
                "multiple of 2",
            ),
            (
                "<gml:LineString><gml:posList>0 0</gml:posList></gml:LineString>",
                "1 point",
            ),
            (
                "<gml:LinearRing><gml:posList>0 0 1 0 1 1 0 1</gml:posList></gml:LinearRing>",
                "not closed",
            ),
            ("<gml:Circle/>", "not a supported GML geometry"),
            (
                "<gml:Point><gml:pos>a b</gml:pos></gml:Point>",
                "not a number",
            ),
            ("<gml:Point>", "unclosed"),
            ("<gml:Point></gml:Pointy>", "invalid XML"),
            ("<a/><b/>", "more than one root"),
            ("POINT(1 2)", "root element"),
            (
                "<gml:Curve><gml:segments><gml:Arc><gml:posList>0 0 1 1 2 0</gml:posList></gml:Arc></gml:segments></gml:Curve>",
                "only gml:LineStringSegment",
            ),
            (
                "<gml:Point srsDimension=\"7\"><gml:pos>1 2</gml:pos></gml:Point>",
                "srsDimension",
            ),
        ] {
            let e = parse(src, GML_LITERAL).unwrap_err();
            assert!(e.msg.contains(msg), "{src}: {e}");
        }
        let e = parse("<gml:Point><gml:pos>1 2</gml:pos></gml:Point", GML_LITERAL).unwrap_err();
        assert!(e.offset.is_some(), "{e}");
        assert!(parse_limited_vertices(
            "<gml:LineString><gml:posList>0 0 1 1 2 2</gml:posList></gml:LineString>",
            2
        ));
    }

    fn parse_limited_vertices(src: &str, max: u32) -> bool {
        crate::geo::parse::parse_limited(src, GML_LITERAL, max).is_err()
    }

    #[test]
    fn kml_forms() {
        let g = kml(
            "<MultiGeometry><Point><coordinates>1,2</coordinates></Point>\
             <LineString><coordinates>0,0 1,1</coordinates></LineString></MultiGeometry>",
        );
        assert_eq!(
            wkt(&g),
            "GEOMETRYCOLLECTION(POINT(1 2), LINESTRING(0 0, 1 1))"
        );
        assert_eq!(g.declared, GeomType::GeometryCollection);
        let g = kml("<LinearRing><coordinates>0,0 1,0 1,1 0,0</coordinates></LinearRing>");
        assert_eq!(wkt(&g), "LINEARRING(0 0, 1 0, 1 1, 0 0)");
        assert!(kml("<Point/>").empty);
        assert!(parse("<Model/>", KML_LITERAL).is_err());
        assert!(parse("<Point><coordinates>1;2</coordinates></Point>", KML_LITERAL).is_err());
    }

    #[test]
    fn writers_round_trip() {
        for s in [
            "POINT(1 2)",
            "<http://www.opengis.net/def/crs/EPSG/0/4326> POINT(33.95 -83.38)",
            "LINESTRING(0 0, 1 1, 2 0)",
            "LINEARRING(0 0, 1 0, 1 1, 0 0)",
            "POLYGON((0 0, 4 0, 4 4, 0 0), (1 1, 2 1, 2 2, 1 1))",
            "MULTIPOINT((1 2), (3 4))",
            "MULTILINESTRING((0 0, 1 1), (2 2, 3 3))",
            "MULTIPOLYGON(((0 0, 1 0, 1 1, 0 0)), ((5 5, 6 5, 6 6, 5 5)))",
            "GEOMETRYCOLLECTION(POINT(1 2), LINESTRING(0 0, 1 1))",
            "TIN(((0 0, 1 0, 0 1, 0 0)))",
            "POLYHEDRALSURFACE(((0 0, 1 0, 1 1, 0 0)))",
            "POINT EMPTY",
            "LINESTRING EMPTY",
            "POLYGON EMPTY",
            "MULTIPOLYGON EMPTY",
            "GEOMETRYCOLLECTION EMPTY",
        ] {
            let g = parse(s, WKT_LITERAL).unwrap();
            let back = gml(&to_gml(&g));
            assert_eq!(wkt(&back), wkt(&g), "GML of {s}: {}", to_gml(&g));
            assert_eq!(back.declared, g.declared, "GML of {s}");
            assert_eq!(back.crs, g.crs);
        }
        assert_eq!(
            to_gml(&parse("POINT(1 2)", WKT_LITERAL).unwrap()),
            "<gml:Point xmlns:gml=\"http://www.opengis.net/gml/3.2\" \
             srsName=\"http://www.opengis.net/def/crs/OGC/1.3/CRS84\"><gml:pos>1 2</gml:pos></gml:Point>"
        );
        for s in [
            "POINT(1 2)",
            "LINESTRING(0 0, 1 1)",
            "POLYGON((0 0, 4 0, 4 4, 0 0), (1 1, 2 1, 2 2, 1 1))",
            "GEOMETRYCOLLECTION(POINT(1 2), LINESTRING(0 0, 1 1))",
        ] {
            let g = parse(s, WKT_LITERAL).unwrap();
            assert_eq!(wkt(&kml(&to_kml(&g))), s, "KML of {s}");
        }
        assert_eq!(
            to_kml(&parse("POINT(1 2)", WKT_LITERAL).unwrap()),
            "<Point xmlns=\"http://www.opengis.net/kml/2.2\"><coordinates>1,2</coordinates></Point>"
        );
        // KML is longitude, latitude whatever the CRS
        let g = parse(
            "<http://www.opengis.net/def/crs/EPSG/0/4326> POINT(33.95 -83.38)",
            WKT_LITERAL,
        )
        .unwrap();
        assert!(to_kml(&g).contains("<coordinates>-83.38,33.95</coordinates>"));
        let m = parse("MULTIPOINT((1 2), (3 4))", WKT_LITERAL).unwrap();
        assert_eq!(
            wkt(&kml(&to_kml(&m))),
            "GEOMETRYCOLLECTION(POINT(1 2), POINT(3 4))"
        );
        assert!(kml(&to_kml(&parse("POINT EMPTY", WKT_LITERAL).unwrap())).empty);
    }
}
