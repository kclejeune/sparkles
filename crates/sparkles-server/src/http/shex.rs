//! `POST /{ds}/shex`: ShEx validation of a dataset's data graph. The schema is the body
//! (`text/shex`, or ShExJ) with the shape map in the query string (`map`, or `node` with
//! `shape`), or the body is a JSON envelope (`{schema, schemaFormat?, map, externs?,
//! imports?, base?}`). The parameters `graph`, `reasoning`, `results`, `format`,
//! `timeout` and `semact-trace` are those of `/{ds}/shacl` where they overlap.

#[cfg(feature = "shex")]
pub(super) use enabled::shex;

#[cfg(not(feature = "shex"))]
pub(super) async fn shex() -> super::ApiResult {
    Err(super::err(
        axum::http::StatusCode::NOT_IMPLEMENTED,
        "built without the `shex` feature",
    ))
}

/// Output formats of a ShEx result map.
#[cfg_attr(not(feature = "shex"), allow(dead_code))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ShexFormat {
    /// the Sparkles JSON report
    Json,
    /// the ShapeMap JSON result map
    ShapeMap,
    /// the compact result map (`<n>@<S>`, `<n>@!<S>`)
    Smap,
    /// Jena's text report
    Text,
}

// (the validation that writes the formats is not wired in yet)
#[allow(dead_code)]
impl ShexFormat {
    /// From the `format` parameter (`json`, `shapemap`, `smap`, `text`) or a media type.
    pub fn from_name(s: &str) -> Option<ShexFormat> {
        match s.trim().to_ascii_lowercase().as_str() {
            "json" | "application/json" => Some(ShexFormat::Json),
            "shapemap" => Some(ShexFormat::ShapeMap),
            "smap" => Some(ShexFormat::Smap),
            "text" | "txt" | "text/plain" => Some(ShexFormat::Text),
            _ => None,
        }
    }

    /// Accept-header offers, in preference order.
    pub const OFFERS: [&'static str; 2] = ["application/json", "text/plain"];

    pub fn media_type(self) -> &'static str {
        match self {
            ShexFormat::Json | ShexFormat::ShapeMap => "application/json",
            ShexFormat::Smap | ShexFormat::Text => "text/plain; charset=utf-8",
        }
    }
}

#[cfg(feature = "shex")]
mod enabled {
    use super::super::{
        ApiResult, Params, QueryBody, St, content_type, dataset, err, negotiate, timeout_param,
    };
    use super::ShexFormat;
    use crate::state::AppState;
    use crate::validation_common::{GraphParam, graph_exists};
    use axum::extract::Path;
    use axum::http::{HeaderMap, StatusCode, Uri, header};
    use std::time::Duration;

    /// The query parameters of a validation.
    #[allow(dead_code)]
    pub(crate) struct ShexParams {
        pub graph: GraphParam,
        /// merge the materialized inferences into the data graph (`reasoning`)
        pub use_inferred: bool,
        /// `results=nonconformant`
        pub only_nonconformant: bool,
        pub format: ShexFormat,
        pub timeout: Duration,
        pub semact_trace: bool,
        /// a compact shape map
        pub map: Option<String>,
        /// a node in compact term syntax, with `shape` (START when absent)
        pub node: Option<String>,
        pub shape: Option<String>,
    }

    fn flag(params: &Params, name: &str) -> ApiResult<bool> {
        match params.get(name) {
            None => Ok(false),
            Some("true") => Ok(true),
            Some("false") => Ok(false),
            Some(v) => Err(err(
                StatusCode::BAD_REQUEST,
                format!("invalid {name} '{v}' (true or false)"),
            )),
        }
    }

    pub(crate) fn params(
        st: &AppState,
        params: &Params,
        headers: &HeaderMap,
    ) -> ApiResult<ShexParams> {
        let bad = |m: String| err(StatusCode::BAD_REQUEST, m);
        let graph = GraphParam::parse(params.get("graph").unwrap_or("default"))
            .map_err(|e| bad(format!("{e:#}")))?;
        let only_nonconformant = match params.get("results") {
            None | Some("all") => false,
            Some("nonconformant") => true,
            Some(r) => {
                return Err(bad(format!("invalid results '{r}' (all or nonconformant)")));
            }
        };
        let format = match params.get("format") {
            Some(f) => ShexFormat::from_name(f).ok_or_else(|| {
                bad(format!(
                    "unknown report format '{f}' (json, shapemap, smap or text)"
                ))
            })?,
            None => {
                let accept = headers
                    .get(header::ACCEPT)
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or("*/*");
                negotiate(accept, &ShexFormat::OFFERS)
                    .and_then(|i| ShexFormat::from_name(ShexFormat::OFFERS[i]))
                    .unwrap_or(ShexFormat::Json)
            }
        };
        let (map, node, shape) = (
            params.get("map").map(str::to_string),
            params.get("node").map(str::to_string),
            params.get("shape").map(str::to_string),
        );
        if map.is_some() && node.is_some() {
            return Err(bad("give either map or node, not both".into()));
        }
        if shape.is_some() && node.is_none() {
            return Err(bad("shape needs node".into()));
        }
        Ok(ShexParams {
            graph,
            use_inferred: params.get("reasoning").is_none_or(|v| v != "false"),
            only_nonconformant,
            format,
            timeout: timeout_param(st, params),
            semact_trace: flag(params, "semact-trace")?,
            map,
            node,
            shape,
        })
    }

    /// `POST /{ds}/shex`.
    pub(crate) async fn shex(
        axum::extract::State(st): St,
        Path(name): Path<String>,
        uri: Uri,
        headers: HeaderMap,
        QueryBody(body): QueryBody,
    ) -> ApiResult {
        let ds = dataset(&st, &name)?;
        let p = params(&st, &Params::from_query(&uri), &headers)?;
        let _ = (content_type(&headers), body);
        if let GraphParam::Named(iri) = &p.graph
            && !graph_exists(&ds.store.snapshot(), iri)
        {
            return Err(err(
                StatusCode::NOT_FOUND,
                format!("no such graph: <{iri}>"),
            ));
        }
        Err(err(
            StatusCode::NOT_IMPLEMENTED,
            "ShEx validation is not implemented yet",
        ))
    }
}

#[cfg(all(test, feature = "shex"))]
#[path = "shex_tests.rs"]
mod tests;
