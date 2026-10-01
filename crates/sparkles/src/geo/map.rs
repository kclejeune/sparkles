//! Indexed geometries in a CRS84 box as a GeoJSON `FeatureCollection`, for map views
//! (`GET /{ds}/geo`): one feature per indexed row and feature linked to it, simplified
//! for the box's scale.
//!
//! Rows come from the index's window search (a scan of the serialization predicates
//! when the index is building, failed or disabled: the same rows, slower), and each
//! geometry is tested exactly against the box. Geometries are transformed to CRS84 and
//! simplified with Douglas–Peucker; a ring the simplification would collapse is kept as
//! it is. Without an index the default configuration's predicates and feature links are
//! read.

use super::config::GeoConfig;
use super::crs::CRS84;
use super::geom::Geom;
use super::search::{self, Hit, SearchStats};
use super::vocab::Relation;
use crate::error::{Error, Result};
use crate::id::Id;
use crate::index::Perm;
use crate::sparql::ctx::Ctx;
use crate::sparql::plan::GraphFilter;
use crate::store::{Chunk, Snapshot};
use georust::Simplify;
use serde_json::{Value, json};
use std::sync::Arc;

/// Default and largest number of features of one answer.
pub const DEFAULT_LIMIT: usize = 5_000;
pub const MAX_LIMIT: usize = 50_000;

/// What `GET /{ds}/geo` asks for.
#[derive(Clone, Debug)]
pub struct BoxQuery {
    /// `[minLon, minLat, maxLon, maxLat]` in CRS84 degrees
    pub bbox: [f64; 4],
    /// only rows of this graph (an IRI)
    pub graph: Option<String>,
    /// only rows of this serialization predicate (an IRI)
    pub predicate: Option<String>,
    /// at most this many features (`truncated: true` when there are more)
    pub limit: usize,
    /// Douglas–Peucker tolerance in degrees (`None`: the box's width / 1024)
    pub tolerance: Option<f64>,
}

/// The `FeatureCollection` of `snap`'s indexed geometries meeting `q.bbox`: `id` and
/// `properties.subject` the row's subject, `properties.feature` a feature linked to it
/// (one feature per link; none when nothing links the geometry), `properties.graph`
/// (`null` for the default graph) and `properties.predicate`, and a top-level
/// `truncated`.
pub fn features_in_box(snap: &Snapshot, q: &BoxQuery) -> Result<Value> {
    let empty = |truncated: bool| json!({"type": "FeatureCollection", "features": [], "truncated": truncated});
    let cfg = snap
        .geo
        .as_ref()
        .map_or_else(|| Arc::new(GeoConfig::default()), |v| v.config.clone());
    let preds: Vec<&String> = match &q.predicate {
        Some(p) => cfg.predicates.iter().filter(|x| *x == p).collect(),
        None => cfg.predicates.iter().collect(),
    };
    let pred_ids: Vec<Id> = preds.iter().filter_map(|p| snap.lookup_iri(p)).collect();
    let graph = match &q.graph {
        None => GraphFilter::All,
        Some(g) if g == crate::text::DEFAULT_GRAPH_IRI => GraphFilter::Default,
        Some(g) => match snap.lookup_iri(g) {
            Some(id) => GraphFilter::One(id.0),
            None => return Ok(empty(false)),
        },
    };
    if pred_ids.is_empty() || q.limit == 0 {
        return Ok(empty(false));
    }
    let links: Vec<Id> = cfg
        .feature_links
        .iter()
        .filter_map(|l| snap.lookup_iri(l))
        .collect();
    let tolerance = q
        .tolerance
        .unwrap_or((q.bbox[2] - q.bbox[0]) / 1024.0)
        .max(0.0);
    let window = super::exec::envelope_geom(q.bbox);
    let ctx = Ctx::new(Arc::new(snap.clone()));
    let mut out = Writer {
        snap,
        links: &links,
        window: &window,
        tolerance,
        limit: q.limit,
        features: Vec::new(),
        truncated: false,
    };
    let mut st = SearchStats::default();
    let r = search::window(&ctx, &pred_ids, &[q.bbox], &graph, &mut st, &mut |hits| {
        for h in hits {
            out.add(h)?;
        }
        Ok(())
    });
    match r {
        Ok(()) => {}
        // the limit was reached
        Err(Error::Cancelled) if out.truncated => {}
        Err(e) => return Err(e),
    }
    Ok(json!({
        "type": "FeatureCollection",
        "features": out.features,
        "truncated": out.truncated,
    }))
}

/// Collects the features of the matching rows.
struct Writer<'a> {
    snap: &'a Snapshot,
    links: &'a [Id],
    window: &'a Geom,
    tolerance: f64,
    limit: usize,
    features: Vec<Value>,
    truncated: bool,
}

impl Writer<'_> {
    /// Add the features of a candidate row (`Err(Cancelled)` once past the limit).
    fn add(&mut self, h: &Hit) -> Result<()> {
        // tested in CRS84, where the box is (a projection may not reach the poles)
        let Some(g) = h
            .entry
            .geom(self.snap)
            .ok()
            .and_then(|g| g.transformed(CRS84))
        else {
            return Ok(());
        };
        if !super::ops::relate::relation(&g, self.window, Relation::SfIntersects).unwrap_or(false) {
            return Ok(());
        }
        let Some(geometry) = drawn(g, self.tolerance) else {
            return Ok(());
        };
        let (Some(subject), Some(predicate)) = (self.name(h.s), self.name(h.p)) else {
            return Ok(());
        };
        let graph = if h.g == Id::DEFAULT_GRAPH {
            Value::Null
        } else {
            self.name(h.g).map_or(Value::Null, Value::String)
        };
        let mut feats: Vec<String> = Vec::new();
        for &l in self.links {
            self.snap.scan(Perm::Pos, &[l.0, h.s.0], |c| {
                let mut each = |k: [u64; 4]| {
                    if k[3] == h.g.0
                        && let Some(f) = self.name(Id(k[2]))
                    {
                        feats.push(f);
                    }
                };
                match c {
                    Chunk::Block(b, s, e) => (s..e).for_each(|i| each(b.key(i))),
                    Chunk::Row(k) => each(k),
                }
                Ok(true)
            })?;
        }
        feats.sort();
        feats.dedup();
        let feats: Vec<Option<String>> = if feats.is_empty() {
            vec![None]
        } else {
            feats.into_iter().map(Some).collect()
        };
        for f in feats {
            if self.features.len() == self.limit {
                self.truncated = true;
                return Err(Error::Cancelled);
            }
            let mut props = json!({
                "subject": subject,
                "graph": graph,
                "predicate": predicate,
            });
            if let Some(f) = f {
                props["feature"] = Value::String(f);
            }
            self.features.push(json!({
                "type": "Feature",
                "id": subject,
                "geometry": geometry,
                "properties": props,
            }));
        }
        Ok(())
    }

    /// The IRI of `id`, or `_:label` for a blank node.
    fn name(&self, id: Id) -> Option<String> {
        match self.snap.term(id)? {
            oxrdf::Term::NamedNode(n) => Some(n.into_string()),
            oxrdf::Term::BlankNode(b) => Some(format!("_:{}", b.as_str())),
            _ => None,
        }
    }
}

/// `g` (in CRS84) simplified with `tolerance` (degrees), as a GeoJSON geometry.
fn drawn(mut g: Geom, tolerance: f64) -> Option<Value> {
    if tolerance > 0.0 {
        g.g = simplified(&g.g, tolerance);
    }
    serde_json::from_str(&super::write::to_geojson(&g)).ok()
}

fn simplified(g: &georust::Geometry<f64>, eps: f64) -> georust::Geometry<f64> {
    use georust::{Geometry as G, LineString, MultiLineString, MultiPolygon, Polygon};
    let line = |l: &LineString<f64>| l.simplify(eps);
    // a ring keeps at least four positions (else it stays as it was)
    let ring = |r: &LineString<f64>| {
        let s = r.simplify(eps);
        if s.0.len() >= 4 { s } else { r.clone() }
    };
    let polygon = |p: &Polygon<f64>| {
        Polygon::new(ring(p.exterior()), p.interiors().iter().map(ring).collect())
    };
    match g {
        G::LineString(l) => G::LineString(line(l)),
        G::MultiLineString(m) => {
            G::MultiLineString(MultiLineString(m.0.iter().map(line).collect()))
        }
        G::Polygon(p) => G::Polygon(polygon(p)),
        G::MultiPolygon(m) => G::MultiPolygon(MultiPolygon(m.0.iter().map(polygon).collect())),
        G::GeometryCollection(c) => G::GeometryCollection(georust::GeometryCollection(
            c.0.iter().map(|g| simplified(g, eps)).collect(),
        )),
        other => other.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dataset::Dataset;
    use crate::io::RdfFormat;
    use crate::store::{Store, StoreOptions};

    const DATA: &str = r#"
@prefix ex: <http://example.org/> .
@prefix geo: <http://www.opengis.net/ont/geosparql#> .
ex:A geo:hasDefaultGeometry ex:gA . ex:gA geo:asWKT "POLYGON((0 0, 10 0, 10 10, 0 10, 0 0))"^^geo:wktLiteral .
ex:A2 geo:hasGeometry ex:gA .
ex:p1 geo:hasGeometry ex:g1 .  ex:g1 geo:asWKT "POINT(2 2)"^^geo:wktLiteral .
ex:p2 geo:hasGeometry ex:g2 .  ex:g2 geo:asWKT "<http://www.opengis.net/def/crs/EPSG/0/4326> POINT(2 12)"^^geo:wktLiteral .
ex:g3 geo:asGeoJSON "{\"type\":\"LineString\",\"coordinates\":[[30,30],[30.001,30.0001],[30.002,30],[31,31]]}"^^geo:geoJSONLiteral .
ex:g5 geo:asWKT "<http://www.opengis.net/def/crs/EPSG/0/3857> POINT(222638.98 222684.21)"^^geo:wktLiteral .
ex:G1 { ex:p4 geo:hasGeometry ex:g4 . ex:g4 geo:asWKT "POINT(3 3)"^^geo:wktLiteral . }
"#;

    fn q(bbox: [f64; 4]) -> BoxQuery {
        BoxQuery {
            bbox,
            graph: None,
            predicate: None,
            limit: DEFAULT_LIMIT,
            tolerance: None,
        }
    }

    /// `(subject, feature, graph)` local names of the features, sorted.
    fn rows(v: &Value) -> Vec<(String, String, String)> {
        let local = |v: &Value| {
            v.as_str()
                .map_or("-".into(), |s| s.rsplit('/').next().unwrap().to_string())
        };
        let mut r: Vec<_> = v["features"]
            .as_array()
            .unwrap()
            .iter()
            .map(|f| {
                let p = &f["properties"];
                assert_eq!(f["id"], p["subject"]);
                (
                    local(&p["subject"]),
                    local(&p["feature"]),
                    local(&p["graph"]),
                )
            })
            .collect();
        r.sort();
        r
    }

    fn row(s: &str, f: &str, g: &str) -> (String, String, String) {
        (s.into(), f.into(), g.into())
    }

    #[test]
    fn features_in_a_box() {
        let ds = Dataset::from_store(Store::in_memory(StoreOptions::default()));
        ds.load_str(DATA, RdfFormat::TriG).unwrap();
        let world = [-180.0, -90.0, 180.0, 90.0];
        // without an index: a scan, with the same answers as the index
        let scanned = features_in_box(&ds.snapshot(), &q(world)).unwrap();
        ds.store().enable_geo(GeoConfig::default()).unwrap();
        let v = features_in_box(&ds.snapshot(), &q(world)).unwrap();
        assert_eq!(v["type"], "FeatureCollection");
        assert_eq!(v["truncated"], false);
        assert_eq!(rows(&v), rows(&scanned));
        assert_eq!(
            rows(&v),
            vec![
                row("g1", "p1", "-"),
                row("g2", "p2", "-"),
                row("g3", "-", "-"),
                row("g4", "p4", "G1"),
                row("g5", "-", "-"),
                row("gA", "A", "-"),
                row("gA", "A2", "-"),
            ]
        );
        // EPSG:4326 and EPSG:3857 come out in longitude, latitude
        let f = |v: &Value, s: &str| -> Value {
            v["features"]
                .as_array()
                .unwrap()
                .iter()
                .find(|f| f["id"].as_str().unwrap().ends_with(s))
                .unwrap()["geometry"]
                .clone()
        };
        assert_eq!(
            f(&v, "g2"),
            json!({"type": "Point", "coordinates": [12, 2]})
        );
        let c = f(&v, "g5")["coordinates"].clone();
        assert!((c[0].as_f64().unwrap() - 2.0).abs() < 1e-6, "{c}");
        assert!((c[1].as_f64().unwrap() - 2.0).abs() < 1e-6, "{c}");
        assert_eq!(f(&v, "g1")["coordinates"], json!([2, 2]));
        assert_eq!(
            v["features"][0]["properties"]["predicate"],
            "http://www.opengis.net/ont/geosparql#asWKT"
        );
        // a small box: what meets it exactly
        let v = features_in_box(&ds.snapshot(), &q([1.5, 1.5, 2.5, 2.5])).unwrap();
        assert_eq!(
            rows(&v),
            vec![
                row("g1", "p1", "-"),
                row("g5", "-", "-"),
                row("gA", "A", "-"),
                row("gA", "A2", "-")
            ]
        );
        // graph and predicate filters
        let v = features_in_box(
            &ds.snapshot(),
            &BoxQuery {
                graph: Some("http://example.org/G1".into()),
                ..q(world)
            },
        )
        .unwrap();
        assert_eq!(rows(&v), vec![row("g4", "p4", "G1")]);
        let v = features_in_box(
            &ds.snapshot(),
            &BoxQuery {
                graph: Some(crate::text::DEFAULT_GRAPH_IRI.into()),
                ..q([25.0, 25.0, 35.0, 35.0])
            },
        )
        .unwrap();
        assert_eq!(rows(&v), vec![row("g3", "-", "-")]);
        let v = features_in_box(
            &ds.snapshot(),
            &BoxQuery {
                predicate: Some("http://www.opengis.net/ont/geosparql#asGeoJSON".into()),
                ..q(world)
            },
        )
        .unwrap();
        assert_eq!(rows(&v), vec![row("g3", "-", "-")]);
        for (graph, pred) in [
            (Some("http://example.org/none"), None),
            (None, Some("http://example.org/notIndexed")),
        ] {
            let v = features_in_box(
                &ds.snapshot(),
                &BoxQuery {
                    graph: graph.map(String::from),
                    predicate: pred.map(String::from),
                    ..q(world)
                },
            )
            .unwrap();
            assert!(v["features"].as_array().unwrap().is_empty());
        }
        // the limit
        let v = features_in_box(
            &ds.snapshot(),
            &BoxQuery {
                limit: 3,
                ..q(world)
            },
        )
        .unwrap();
        assert_eq!(v["features"].as_array().unwrap().len(), 3);
        assert_eq!(v["truncated"], true);
        let v = features_in_box(
            &ds.snapshot(),
            &BoxQuery {
                limit: 7,
                ..q(world)
            },
        )
        .unwrap();
        assert_eq!(v["truncated"], false);
        // simplification: the line's small zigzag goes at a coarse tolerance
        let line = |t: Option<f64>| {
            let v = features_in_box(
                &ds.snapshot(),
                &BoxQuery {
                    tolerance: t,
                    ..q([25.0, 25.0, 35.0, 35.0])
                },
            )
            .unwrap();
            f(&v, "g3")["coordinates"].as_array().unwrap().len()
        };
        assert_eq!(line(Some(0.0)), 4);
        assert_eq!(line(Some(0.01)), 2);
        // the default: 10° / 1024 ≈ 0.0098°
        assert_eq!(line(None), 2);
        assert_eq!(
            features_in_box(&ds.snapshot(), &q(world)).unwrap()["features"]
                .as_array()
                .unwrap()
                .len(),
            7
        );
    }

    #[test]
    fn rings_keep_their_shape() {
        let p = crate::geo::parse(
            "POLYGON((0 0, 1 0, 1 1, 0 1, 0 0), (0.2 0.2, 0.3 0.2, 0.3 0.3, 0.2 0.2))",
            crate::geo::WKT_LITERAL,
        )
        .unwrap();
        let v = drawn(p, 10.0).unwrap();
        assert_eq!(v["coordinates"][0].as_array().unwrap().len(), 5);
        assert_eq!(v["coordinates"][1].as_array().unwrap().len(), 4);
    }
}
