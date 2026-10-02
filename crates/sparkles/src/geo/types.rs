//! The geometry type a serialization literal declares, read from its lexical form
//! without parsing the coordinates: the WKT keyword, the GeoJSON `type`, or the root
//! element of GML and KML. The reasoner's geometry-type rules (`infer --vocab
//! geosparql`) type a geometry `sf:Polygon`, `gml:Polygon` and so on from it. Pure
//! data, compiled with or without the `geo` feature.

use super::vocab::{GEOJSON_LITERAL, GML_LITERAL, KML_LITERAL, WKT_LITERAL};

/// The GML ontology of GeoSPARQL (`gml:`), whose classes are named after GML's
/// geometry elements.
pub const GML_ONT: &str = "http://www.opengis.net/ont/gml#";

/// The Simple Features geometry types (`sf:` local names) with their WKT keywords.
const WKT_TYPES: [(&str, &str); 12] = [
    ("POINT", "Point"),
    ("LINESTRING", "LineString"),
    ("POLYGON", "Polygon"),
    ("MULTIPOINT", "MultiPoint"),
    ("MULTILINESTRING", "MultiLineString"),
    ("MULTIPOLYGON", "MultiPolygon"),
    ("GEOMETRYCOLLECTION", "GeometryCollection"),
    ("LINEARRING", "LinearRing"),
    ("TRIANGLE", "Triangle"),
    ("TIN", "TIN"),
    ("POLYHEDRALSURFACE", "PolyhedralSurface"),
    ("LINE", "Line"),
];

/// The `sf:` local name of the geometry a literal of datatype `dt` declares. `None` for
/// another datatype, an empty WKT literal (which declares no type) and a literal whose
/// type cannot be read.
pub fn sf_type(lex: &str, dt: &str) -> Option<&'static str> {
    if dt == WKT_LITERAL {
        wkt_type(lex)
    } else if dt == GEOJSON_LITERAL {
        geojson_type(lex)
    } else if dt == GML_LITERAL {
        gml_sf_type(root_element(lex)?)
    } else if dt == KML_LITERAL {
        kml_sf_type(root_element(lex)?)
    } else {
        None
    }
}

/// The `gml:` local name of a `geo:gmlLiteral`'s root element, when it is a GML
/// geometry element.
pub fn gml_type(lex: &str, dt: &str) -> Option<&'static str> {
    if dt != GML_LITERAL {
        return None;
    }
    let root = root_element(lex)?;
    GML_TYPES.iter().copied().find(|&t| t == root)
}

fn wkt_type(lex: &str) -> Option<&'static str> {
    let mut s = lex.trim_start();
    if let Some(rest) = s.strip_prefix('<') {
        s = rest[rest.find('>')? + 1..].trim_start();
    }
    let word: String = s
        .chars()
        .take_while(|c| c.is_ascii_alphabetic())
        .collect::<String>()
        .to_ascii_uppercase();
    if word.is_empty() {
        return None;
    }
    // a dimension suffix written together with the keyword (`POINTZ`, `POINTZM`)
    let bare = ["ZM", "Z", "M"]
        .iter()
        .filter_map(|suffix| word.strip_suffix(suffix))
        .find(|w| WKT_TYPES.iter().any(|(k, _)| k == w))
        .unwrap_or(&word);
    WKT_TYPES
        .iter()
        .find(|(k, _)| *k == bare)
        .map(|&(_, sf)| sf)
}

fn geojson_type(lex: &str) -> Option<&'static str> {
    let v: serde_json::Value = serde_json::from_str(lex).ok()?;
    let t = v.get("type")?.as_str()?;
    [
        "Point",
        "LineString",
        "Polygon",
        "MultiPoint",
        "MultiLineString",
        "MultiPolygon",
        "GeometryCollection",
    ]
    .into_iter()
    .find(|&x| x == t)
}

/// GML's geometry elements that are classes of the GML ontology: the GML 3.2 Simple
/// Features profile and the curves and surfaces made of linear segments.
const GML_TYPES: [&str; 17] = [
    "Point",
    "LineString",
    "LinearRing",
    "Curve",
    "OrientableCurve",
    "CompositeCurve",
    "Polygon",
    "Surface",
    "OrientableSurface",
    "CompositeSurface",
    "PolyhedralSurface",
    "TriangulatedSurface",
    "Tin",
    "MultiPoint",
    "MultiCurve",
    "MultiSurface",
    "MultiGeometry",
];

/// The Simple Features type of a GML root element. A curve or surface of linear
/// segments is a line string or a polygon.
fn gml_sf_type(root: &str) -> Option<&'static str> {
    Some(match root {
        "Point" => "Point",
        "LineString" | "Curve" | "OrientableCurve" | "CompositeCurve" => "LineString",
        "LinearRing" => "LinearRing",
        "Polygon" | "Surface" | "OrientableSurface" => "Polygon",
        "CompositeSurface" => "MultiPolygon",
        "PolyhedralSurface" => "PolyhedralSurface",
        "Tin" | "TriangulatedSurface" => "TIN",
        "Triangle" => "Triangle",
        "MultiPoint" => "MultiPoint",
        "MultiCurve" | "MultiLineString" => "MultiLineString",
        "MultiSurface" | "MultiPolygon" => "MultiPolygon",
        "MultiGeometry" => "GeometryCollection",
        _ => return None,
    })
}

/// The Simple Features type of a KML 2.2 geometry element.
fn kml_sf_type(root: &str) -> Option<&'static str> {
    Some(match root {
        "Point" => "Point",
        "LineString" => "LineString",
        "LinearRing" => "LinearRing",
        "Polygon" => "Polygon",
        "MultiGeometry" => "GeometryCollection",
        _ => return None,
    })
}

/// The local name of an XML document's root element, after an XML declaration,
/// processing instructions, comments and a doctype.
pub fn root_element(lex: &str) -> Option<&str> {
    let mut s = lex.trim_start();
    loop {
        if let Some(rest) = s.strip_prefix("<?") {
            s = rest[rest.find("?>")? + 2..].trim_start();
        } else if let Some(rest) = s.strip_prefix("<!--") {
            s = rest[rest.find("-->")? + 3..].trim_start();
        } else if let Some(rest) = s.strip_prefix("<!") {
            s = rest[rest.find('>')? + 1..].trim_start();
        } else {
            break;
        }
    }
    let rest = s.strip_prefix('<')?;
    let end = rest
        .find(|c: char| c.is_whitespace() || c == '>' || c == '/')
        .unwrap_or(rest.len());
    let name = &rest[..end];
    let local = name.rsplit(':').next()?;
    (!local.is_empty()).then_some(local)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wkt_keywords() {
        assert_eq!(sf_type("POINT(1 2)", WKT_LITERAL), Some("Point"));
        assert_eq!(
            sf_type(
                " <http://www.opengis.net/def/crs/OGC/1.3/CRS84> Polygon((0 0, 1 0, 1 1, 0 0))",
                WKT_LITERAL
            ),
            Some("Polygon")
        );
        assert_eq!(sf_type("pointz (1 2 3)", WKT_LITERAL), Some("Point"));
        assert_eq!(
            sf_type("MULTIPOLYGON EMPTY", WKT_LITERAL),
            Some("MultiPolygon")
        );
        assert_eq!(
            sf_type("TIN Z (((0 0 0, 1 0 0, 0 1 0, 0 0 0)))", WKT_LITERAL),
            Some("TIN")
        );
        assert_eq!(sf_type("", WKT_LITERAL), None);
        assert_eq!(sf_type("CIRCLE(1 2)", WKT_LITERAL), None);
        assert_eq!(sf_type("POINT(1 2)", GEOJSON_LITERAL), None);
    }

    #[test]
    fn geojson_types() {
        assert_eq!(
            sf_type(
                r#"{"type":"LineString","coordinates":[[0,0],[1,1]]}"#,
                GEOJSON_LITERAL
            ),
            Some("LineString")
        );
        assert_eq!(sf_type(r#"{"type":"Feature"}"#, GEOJSON_LITERAL), None);
    }

    #[test]
    fn xml_roots() {
        let gml = r#"<?xml version="1.0"?><!-- a polygon --><gml:Polygon xmlns:gml="http://www.opengis.net/gml/3.2"><gml:exterior/></gml:Polygon>"#;
        assert_eq!(sf_type(gml, GML_LITERAL), Some("Polygon"));
        assert_eq!(gml_type(gml, GML_LITERAL), Some("Polygon"));
        assert_eq!(gml_type(gml, WKT_LITERAL), None);
        assert_eq!(gml_type("<gml:Envelope/>", GML_LITERAL), None);
        assert_eq!(
            sf_type("<MultiGeometry><Point/></MultiGeometry>", KML_LITERAL),
            Some("GeometryCollection")
        );
        assert_eq!(sf_type("not xml", KML_LITERAL), None);
    }
}
