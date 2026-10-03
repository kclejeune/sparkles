//! Geometry literals as CRS84 GeoJSON geometries, for clients that draw result columns
//! (`POST /$/geo/convert`).
//!
//! Each literal converts on its own: a malformed literal, an unknown datatype or a CRS
//! without a transform to CRS84 is an error for that item only, with the reason.
//! Empty geometries are an empty `GeometryCollection`. The output is the canonical
//! GeoJSON writer's (longitude, latitude; exterior rings counter-clockwise).

use super::memo::DEFAULT_MAX_VERTICES;
use super::parse::{is_geometry_datatype, parse_limited};
use crate::error::{Error, Result};
use serde::{Deserialize, Serialize};

/// Most literals in one request.
pub const MAX_ITEMS: usize = 10_000;

/// A literal to convert.
#[derive(Clone, Debug, Deserialize)]
pub struct ConvertItem {
    /// the lexical form
    pub value: String,
    /// the datatype IRI (`geo:wktLiteral`, `geo:geoJSONLiteral`)
    pub datatype: String,
}

/// The outcome for one literal: a GeoJSON geometry in CRS84 (longitude, latitude), or
/// why there is none (the message of a malformed literal, an unknown CRS, …).
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Converted {
    Geometry(serde_json::Value),
    Error(String),
}

/// Convert each literal, in order (at most [`MAX_ITEMS`]).
pub fn convert(items: &[ConvertItem]) -> Result<Vec<Converted>> {
    if items.len() > MAX_ITEMS {
        return Err(Error::invalid(format!(
            "literals: at most {MAX_ITEMS} per request"
        )));
    }
    Ok(items.iter().map(one).collect())
}

fn one(item: &ConvertItem) -> Converted {
    if !is_geometry_datatype(&item.datatype) {
        return Converted::Error(format!(
            "not a geometry literal datatype: <{}>",
            item.datatype
        ));
    }
    let g = match parse_limited(&item.value, &item.datatype, DEFAULT_MAX_VERTICES) {
        Ok(g) => g,
        // "malformed literal at offset 6: …" or "malformed literal: …"
        Err(e) if e.offset.is_some() => return Converted::Error(format!("malformed literal {e}")),
        Err(e) => return Converted::Error(format!("malformed literal: {e}")),
    };
    if g.crs.known().is_none() {
        return Converted::Error(format!(
            "unknown CRS <{}>: no transform to CRS84",
            g.crs_iri()
        ));
    }
    if g.bbox84().is_none() && !g.empty {
        return Converted::Error("coordinates outside the CRS's domain".into());
    }
    match serde_json::from_str(&super::write::to_geojson(&g)) {
        Ok(v) => Converted::Geometry(v),
        Err(e) => Converted::Error(format!("not representable as GeoJSON: {e}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geo::vocab::{GEOJSON_LITERAL, WKT_LITERAL};
    use serde_json::json;

    fn item(value: &str, datatype: &str) -> ConvertItem {
        ConvertItem {
            value: value.into(),
            datatype: datatype.into(),
        }
    }

    #[test]
    fn literals_to_crs84() {
        let out = convert(&[
            item("POINT(2 3)", WKT_LITERAL),
            // latitude first: swapped
            item(
                "<http://www.opengis.net/def/crs/EPSG/0/4326> POINT(48.8566 2.3522)",
                WKT_LITERAL,
            ),
            // projected: back to longitude, latitude
            item(
                "<http://www.opengis.net/def/crs/EPSG/0/3857> POINT(0 0)",
                WKT_LITERAL,
            ),
            item(
                r#"{"type":"LineString","coordinates":[[0,0],[1,1]]}"#,
                GEOJSON_LITERAL,
            ),
            item("POLYGON EMPTY", WKT_LITERAL),
            item("POINT(1)", WKT_LITERAL),
            item("<http://example.org/crs/mars> POINT(1 1)", WKT_LITERAL),
            item("POINT(1 1)", "http://www.w3.org/2001/XMLSchema#string"),
        ])
        .unwrap();
        assert_eq!(
            out[0],
            Converted::Geometry(json!({"type": "Point", "coordinates": [2, 3]}))
        );
        assert_eq!(
            out[1],
            Converted::Geometry(json!({"type": "Point", "coordinates": [2.3522, 48.8566]}))
        );
        assert_eq!(
            out[2],
            Converted::Geometry(json!({"type": "Point", "coordinates": [0, 0]}))
        );
        assert_eq!(
            out[3],
            Converted::Geometry(json!({"type": "LineString", "coordinates": [[0, 0], [1, 1]]}))
        );
        assert_eq!(
            out[4],
            Converted::Geometry(json!({"type": "GeometryCollection", "geometries": []}))
        );
        for (i, want) in [
            (5, "malformed literal at offset"),
            (6, "unknown CRS <http://example.org/crs/mars>"),
            (7, "not a geometry literal datatype"),
        ] {
            match &out[i] {
                Converted::Error(e) => assert!(e.starts_with(want), "{i}: {e}"),
                other => panic!("{i}: {other:?}"),
            }
        }
        // the wire shape: {"geometry": …} or {"error": …}
        assert_eq!(
            serde_json::to_value(&out[5..6]).unwrap()[0]
                .as_object()
                .unwrap()
                .keys()
                .collect::<Vec<_>>(),
            ["error"]
        );
        assert!(serde_json::to_value(&out[0]).unwrap()["geometry"]["type"] == "Point");
        let many = vec![item("POINT(0 0)", WKT_LITERAL); MAX_ITEMS + 1];
        assert!(convert(&many).is_err());
        assert_eq!(convert(&many[..MAX_ITEMS]).unwrap().len(), MAX_ITEMS);
    }
}
