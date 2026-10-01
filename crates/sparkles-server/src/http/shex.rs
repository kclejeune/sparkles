//! `POST /{ds}/shex`: ShEx validation of a dataset's data graph. The schema is the body
//! (`text/shex`, or ShExJ) with the shape map in the query string (`map`, or `node` with
//! `shape`), or the body is a JSON envelope (`{schema, schemaFormat?, map, externs?,
//! imports?, base?}`). The parameters `graph`, `reasoning`, `results`, `format`,
//! `timeout` and `semact-trace` are those of `/{ds}/shacl` where they overlap; `base`
//! resolves the schema's relative IRIs and `stats=true` adds the typing's counters to
//! the JSON report.
//!
//! Imports resolve from the envelope's inline bodies, then `file:` IRIs under
//! `--load-dir` (none without it), then http(s) through the server's outbound policy,
//! with one request budget per validation.

#[cfg(feature = "shex")]
pub(super) use enabled::shex;

#[cfg(not(feature = "shex"))]
pub(super) async fn shex() -> super::ApiResult {
    Err(super::err(
        axum::http::StatusCode::NOT_IMPLEMENTED,
        "built without the `shex` feature",
    ))
}

#[cfg(feature = "shex")]
mod enabled {
    use super::super::{
        ApiError, ApiResult, INFERRED_GRAPH, Params, QueryBody, St, blocking, cancel_on_drop,
        content_type, dataset, err, negotiate, timeout_param, with_commit, with_inferences,
    };
    use crate::shex_cmd::{ShexFormat, write_report};
    use crate::state::AppState;
    use crate::validation_common::{GraphParam, graph_exists};
    use axum::extract::Path;
    use axum::http::{HeaderMap, StatusCode, Uri, header};
    use axum::response::IntoResponse;
    use serde_json::{Value as J, json};
    use sparkles::{BudgetKind, Error};
    use sparkles_shex::{
        FileResolver, ParseError, Schema, SchemaError, SchemaFormat, ShapeMap, TooManyResults,
        ValidateOptions,
    };
    use std::collections::HashMap;
    use std::time::Duration;

    /// The query parameters of a validation.
    pub(crate) struct ShexParams {
        pub graph: GraphParam,
        /// merge the materialized inferences into the data graph (`reasoning`)
        pub use_inferred: bool,
        /// `results=nonconformant`
        pub only_nonconformant: bool,
        pub format: ShexFormat,
        pub timeout: Duration,
        pub semact_trace: bool,
        /// the typing's counters in the JSON report (`stats`)
        pub stats: bool,
        /// a compact shape map
        pub map: Option<String>,
        /// a node in compact term syntax, with `shape` (START when absent)
        pub node: Option<String>,
        pub shape: Option<String>,
        /// the base IRI of the schema (and of the shape map)
        pub base: Option<String>,
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
        let base = params.get("base").map(str::to_string);
        if let Some(b) = &base {
            oxrdf::NamedNode::new(b.as_str())
                .map_err(|e| bad(format!("invalid base <{b}>: {e}")))?;
        }
        Ok(ShexParams {
            graph,
            use_inferred: params.get("reasoning").is_none_or(|v| v != "false"),
            only_nonconformant,
            format,
            timeout: timeout_param(st, params),
            semact_trace: flag(params, "semact-trace")?,
            stats: flag(params, "stats")?,
            map,
            node,
            shape,
            base,
        })
    }

    /// The JSON envelope: the schema and everything that goes with it in one body.
    #[derive(serde::Deserialize)]
    #[serde(deny_unknown_fields, rename_all = "camelCase")]
    struct Envelope {
        schema: String,
        schema_format: Option<String>,
        /// a compact shape map, or a JSON one
        map: Option<J>,
        externs: Option<String>,
        /// import bodies by IRI
        #[serde(default)]
        imports: HashMap<String, String>,
        base: Option<String>,
    }

    /// A 400 for a syntax error, with its line and column.
    fn syntax(what: &str, e: &ParseError) -> ApiError {
        ApiError(
            StatusCode::BAD_REQUEST,
            json!({
                "error": format!(
                    "{what} syntax error at line {}, column {}: {}",
                    e.line, e.column, e.message
                ),
                "line": e.line,
                "column": e.column,
            }),
        )
    }

    fn schema_error(e: &SchemaError) -> ApiError {
        err(StatusCode::BAD_REQUEST, e.message.clone())
    }

    /// What the request asks to validate, read from the body and the parameters.
    struct Request {
        schema: Schema,
        externs: Option<Schema>,
        imports: HashMap<String, Schema>,
        /// a compact shape map, or a JSON one
        map: MapText,
        base: Option<String>,
    }

    enum MapText {
        Compact(String),
        Json(String),
    }

    fn read_request(ct: &str, body: &[u8], p: &ShexParams) -> ApiResult<Request> {
        let bad = |m: &str| err(StatusCode::BAD_REQUEST, m);
        let text = std::str::from_utf8(body).map_err(|_| bad("the request body is not UTF-8"))?;
        let query_map = match (&p.map, &p.node) {
            (Some(m), _) => Some(MapText::Compact(m.clone())),
            (None, Some(n)) => Some(MapText::Compact(format!(
                "{n}@{}",
                p.shape.as_deref().unwrap_or("START")
            ))),
            (None, None) => None,
        };
        let json_ct = matches!(ct, "application/json" | "application/ld+json");
        if json_ct && !sparkles_shex::shexj::is_shexj(text) {
            let env: Envelope = serde_json::from_str(text)
                .map_err(|e| bad(&format!("invalid request envelope: {e}")))?;
            let base = env.base.or_else(|| p.base.clone());
            let hint =
                match &env.schema_format {
                    Some(f) => Some(SchemaFormat::from_name(f).ok_or_else(|| {
                        bad(&format!("unknown schemaFormat '{f}' (shexc or shexj)"))
                    })?),
                    None => None,
                };
            let schema = sparkles_shex::parse_schema(&env.schema, base.as_deref(), hint)
                .map_err(|e| syntax("schema", &e))?;
            let externs = match &env.externs {
                Some(x) => Some(
                    sparkles_shex::parse_schema(x, base.as_deref(), None)
                        .map_err(|e| syntax("externs", &e))?,
                ),
                None => None,
            };
            let mut imports = HashMap::new();
            for (iri, body) in &env.imports {
                let s = sparkles_shex::parse_schema(body, Some(iri), None)
                    .map_err(|e| syntax(&format!("import <{iri}>"), &e))?;
                imports.insert(iri.clone(), s);
            }
            let body_map = match env.map {
                None => None,
                Some(J::String(s)) => Some(MapText::Compact(s)),
                Some(a @ J::Array(_)) => Some(MapText::Json(a.to_string())),
                Some(_) => {
                    return Err(bad(
                        "map must be a compact shape map (a string) or a JSON shape map (an array)",
                    ));
                }
            };
            let map = match (body_map, query_map) {
                (Some(_), Some(_)) => {
                    return Err(bad(
                        "give the shape map in the envelope or in the query string, not both",
                    ));
                }
                (Some(m), None) | (None, Some(m)) => m,
                (None, None) => return Err(bad(NO_MAP)),
            };
            return Ok(Request {
                schema,
                externs,
                imports,
                map,
                base,
            });
        }
        let hint = match ct {
            "text/shex" => Some(SchemaFormat::ShExC),
            "application/shex+json" | "application/json" | "application/ld+json" => {
                Some(SchemaFormat::ShExJ)
            }
            // anything else (curl's default form type included): sniffed
            _ => None,
        };
        let schema = sparkles_shex::parse_schema(text, p.base.as_deref(), hint)
            .map_err(|e| syntax("schema", &e))?;
        Ok(Request {
            schema,
            externs: None,
            imports: HashMap::new(),
            map: query_map.ok_or_else(|| bad(NO_MAP))?,
            base: p.base.clone(),
        })
    }

    const NO_MAP: &str = "no shape map: give map, or node (with shape, or the schema's START)";

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
        let ct = content_type(&headers);
        let has_inferred = ds.reasoning.read().is_some();
        // a client that disconnects stops the validation at its next check
        let (cancel, _cancel_on_drop) = cancel_on_drop();
        let max_bytes = st.limits.max_result_bytes;
        let max_results = crate::validation_common::max_results(&st.limits);
        let too_large = move |requested: u64| -> ApiError {
            Error::BudgetExceeded(sparkles::Budget {
                kind: BudgetKind::ResultBytes,
                limit: max_bytes.unwrap_or(u64::MAX),
                requested,
            })
            .into()
        };
        let policy = st.outbound.clone();
        let files = st.file_loads.clone();
        blocking(move || {
            let req = read_request(&ct, &body, &p)?;
            // one resolver, and one outbound request budget, per validation
            let budget = sparkles::outbound::RequestBudget::new(&policy);
            let resolver = FileResolver {
                inline: req.imports,
                files,
                outbound: Some((policy.clone(), budget.clone())),
                externs: req.externs,
                ..Default::default()
            };
            let schema = {
                let _compile = tracing::info_span!("shex.compile").entered();
                sparkles_shex::compile(&req.schema, &resolver).map_err(|e| {
                    // the imports spent the request's outbound bytes
                    if budget.bytes() > policy.max_request_bytes {
                        return Error::BudgetExceeded(sparkles::Budget {
                            kind: BudgetKind::OutboundBytes,
                            limit: policy.max_request_bytes,
                            requested: budget.bytes(),
                        })
                        .into();
                    }
                    schema_error(&e)
                })?
            };
            let map = match &req.map {
                MapText::Compact(m) => {
                    ShapeMap::parse(m, schema.prefixes(), req.base.as_deref().or(schema.base()))
                        .map_err(|e| syntax("shape map", &e))?
                }
                MapText::Json(m) => ShapeMap::from_json(m).map_err(|e| syntax("shape map", &e))?,
            };
            let snap = ds.store.snapshot();
            if let GraphParam::Named(iri) = &p.graph
                && !graph_exists(&snap, iri)
            {
                return Err(err(
                    StatusCode::NOT_FOUND,
                    format!("no such graph: <{iri}>"),
                ));
            }
            let inferred = has_inferred.then_some(INFERRED_GRAPH);
            let inputs =
                crate::validation_common::inputs(&snap, &p.graph, inferred, p.use_inferred)
                    .map_err(|e| err(StatusCode::BAD_REQUEST, format!("{e:#}")))?;
            let opts = ValidateOptions {
                data_graph: inputs.data_graph,
                extra_graphs: inputs.extra_graphs,
                exclude_graphs: inputs.exclude_graphs,
                pool: crate::validation_common::validation_pool(),
                timeout: Some(p.timeout),
                cancel: Some(cancel),
                max_results,
                only_nonconformant: p.only_nonconformant,
                semact_trace: p.semact_trace,
                ..Default::default()
            };
            let t = std::time::Instant::now();
            let _validate = tracing::info_span!("shex.validate").entered();
            let results = sparkles_shex::validate(&snap, &schema, &map, &opts).map_err(|e| {
                if let Some(r) = e.downcast_ref::<TooManyResults>() {
                    // the report would be at least this large
                    return too_large(
                        (r.limit as u64 + 1) * crate::validation_common::MIN_RESULT_BYTES,
                    );
                }
                if let Some(s) = e.downcast_ref::<SchemaError>() {
                    return schema_error(s);
                }
                let msg = format!("{e:#}");
                match e.downcast::<Error>() {
                    Ok(e) => ApiError::from(e),
                    Err(_) => err(StatusCode::BAD_REQUEST, msg),
                }
            })?;
            tracing::debug!(
                "ShEx validation of /{} in {:?}: {} results",
                ds.name,
                t.elapsed(),
                results.results.len()
            );
            let buf = write_report(&results, p.format, p.stats);
            drop(results);
            if max_bytes.is_some_and(|m| buf.len() as u64 > m) {
                return Err(too_large(buf.len() as u64));
            }
            let resp = ([(header::CONTENT_TYPE, p.format.media_type())], buf).into_response();
            let resp = with_commit(resp, &ds, snap.commit);
            Ok(with_inferences(
                resp,
                &ds,
                has_inferred && p.use_inferred,
                snap.commit,
            ))
        })
        .await
    }
}

#[cfg(all(test, feature = "shex"))]
#[path = "shex_tests.rs"]
mod tests;
