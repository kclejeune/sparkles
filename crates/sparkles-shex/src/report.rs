//! Result-map writers: the Sparkles JSON report, the ShapeMap JSON result map, the
//! compact result map and Jena's text report.

use crate::ResultMap;
use crate::error::NOT_IMPLEMENTED;

/// See [`ResultMap::to_json`].
pub fn to_json(r: &ResultMap) -> serde_json::Value {
    let _ = r;
    unimplemented!("JSON reports: {NOT_IMPLEMENTED}")
}

/// See [`ResultMap::to_shapemap_json`].
pub fn to_shapemap_json(r: &ResultMap) -> serde_json::Value {
    let _ = r;
    unimplemented!("ShapeMap JSON result maps: {NOT_IMPLEMENTED}")
}

/// See [`ResultMap::to_smap`].
pub fn to_smap(r: &ResultMap) -> String {
    let _ = r;
    unimplemented!("compact result maps: {NOT_IMPLEMENTED}")
}

/// See [`ResultMap::to_text`].
pub fn to_text(r: &ResultMap) -> String {
    let _ = r;
    unimplemented!("text reports: {NOT_IMPLEMENTED}")
}
