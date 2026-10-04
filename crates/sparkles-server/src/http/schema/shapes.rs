//! `GET /$/schema/{ds}/shapes`: SHACL shapes and a ShEx schema drafted from the data
//! (`sparkles::schema::draft`), as JSON with the counts, as Turtle, as SHACLC or as
//! ShExC.

use super::super::{ApiResult, INFERRED_GRAPH, Params, St, blocking, dataset, negotiate};
use super::{bad, schema_error};
use crate::auth::Principal;
use axum::Extension;
use axum::Json;
use axum::extract::Path;
use axum::http::{HeaderMap, Uri, header};
use axum::response::IntoResponse;
use sparkles::schema::draft::{
    DEFAULT_MAX_COUNT, DEFAULT_MAX_IN, DraftOptions, TRACKED_VALUES, default_base,
};
use sparkles::schema::{GraphSelection, SchemaOptions};
use std::time::{Duration, Instant};

/// What a draft request answers with.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Format {
    Json,
    Turtle,
    Shaclc,
    ShexC,
}

fn format(params: &Params, headers: &HeaderMap) -> ApiResult<Format> {
    if let Some(f) = params.get("format") {
        return match f {
            "json" => Ok(Format::Json),
            "turtle" | "ttl" | "shacl" => Ok(Format::Turtle),
            "shaclc" => Ok(Format::Shaclc),
            "shexc" | "shex" => Ok(Format::ShexC),
            f => Err(bad(format!(
                "unknown format '{f}': json, turtle, shaclc or shexc"
            ))),
        };
    }
    const OFFERS: [&str; 4] = [
        "application/json",
        "text/turtle",
        "text/shex",
        "text/shaclc",
    ];
    let accept = headers
        .get(header::ACCEPT)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("*/*");
    Ok(match negotiate(accept, &OFFERS) {
        Some(1) => Format::Turtle,
        Some(2) => Format::ShexC,
        Some(3) => Format::Shaclc,
        _ => Format::Json,
    })
}

fn flag(params: &Params, k: &str, default: bool) -> ApiResult<bool> {
    match params.get(k) {
        None => Ok(default),
        Some("true") => Ok(true),
        Some("false") => Ok(false),
        Some(v) => Err(bad(format!("{k} must be true or false, not '{v}'"))),
    }
}

fn number<T: std::str::FromStr>(params: &Params, k: &str, default: T) -> ApiResult<T> {
    match params.get(k) {
        None => Ok(default),
        Some(v) => v
            .parse()
            .map_err(|_| bad(format!("{k} must be a non-negative integer, not '{v}'"))),
    }
}

/// The draft options of a request.
fn options(st: &crate::state::AppState, name: &str, params: &Params) -> ApiResult<DraftOptions> {
    let graph = params
        .get("graph")
        .map(|v| GraphSelection::parse(v).map_err(|e| bad(format!("graph: {e}"))))
        .transpose()?
        .unwrap_or(GraphSelection::Default);
    let support = match params.get("support") {
        None => 1.0,
        Some(v) => v
            .parse::<f64>()
            .ok()
            .filter(|s| *s > 0.0 && *s <= 1.0)
            .ok_or_else(|| bad(format!("support must be a number in (0, 1], not '{v}'")))?,
    };
    let max_in: usize = number(params, "maxIn", DEFAULT_MAX_IN)?;
    if max_in > TRACKED_VALUES {
        return Err(bad(format!("maxIn must be at most {TRACKED_VALUES}")));
    }
    let mut classes = Vec::new();
    for c in params.all("class") {
        let c = c.trim();
        let iri = c
            .strip_prefix('<')
            .and_then(|i| i.strip_suffix('>'))
            .unwrap_or(c);
        oxrdf::NamedNode::new(iri).map_err(|e| bad(format!("class: invalid IRI '{iri}': {e}")))?;
        classes.push(iri.to_string());
    }
    let base = match params.get("base") {
        None => default_base(name),
        Some(b) => {
            oxrdf::NamedNode::new(format!("{b}X"))
                .map_err(|e| bad(format!("base: invalid IRI '{b}': {e}")))?;
            b.to_string()
        }
    };
    Ok(DraftOptions {
        schema: SchemaOptions {
            graph,
            inferred_graph: Some(INFERRED_GRAPH.to_string()),
            include_inferred: flag(params, "reasoning", false)?,
            max_entries: st.schema_max_entries,
            ..Default::default()
        },
        dataset: name.to_string(),
        support,
        classes,
        min_instances: number(params, "minInstances", 1u64)?,
        max_in,
        max_count: number(params, "maxCount", DEFAULT_MAX_COUNT)?,
        closed: flag(params, "closed", false)?,
        base,
        prefixes: Vec::new(),
    })
}

/// The request timeout: `timeout=` seconds, capped as for queries.
fn timeout(st: &crate::state::AppState, params: &Params) -> ApiResult<Duration> {
    match params.get("timeout") {
        None => Ok(st.default_timeout),
        Some(v) => v
            .parse::<f64>()
            .ok()
            .filter(|t| t.is_finite() && *t > 0.0)
            .and_then(|t| Duration::try_from_secs_f64(t).ok())
            .map(|t| st.limits.cap_timeout(t, Some(st.default_timeout)))
            .ok_or_else(|| bad("timeout must be a positive number of seconds")),
    }
}

pub(in crate::http) async fn shapes(
    st: St,
    Path(name): Path<String>,
    Extension(p): Extension<Principal>,
    uri: Uri,
    headers: HeaderMap,
) -> ApiResult {
    let ds = dataset(&st, &name)?;
    let params = Params::from_query(&uri);
    let format = format(&params, &headers)?;
    let mut opts = options(&st, &ds.name, &params)?;
    let timeout = timeout(&st, &params)?;
    let at = super::super::history::at_param(&params)?;
    // a caller limited to some graphs gets a draft of those only
    opts.schema.graphs = p.view(&ds.name, crate::auth::Endpoint::Info);
    let mut prefixes = sparkles::io::standard_prefixes();
    prefixes.extend(ds.store.prefixes());
    opts.prefixes = prefixes.into_iter().collect();
    blocking(move || {
        opts.schema.deadline = Some(Instant::now() + timeout);
        let draft = ds
            .dataset
            .schema()
            .draft_shapes_at(&opts, at.as_ref())
            .map_err(|e| schema_error(e, timeout))?;
        Ok(match format {
            Format::Json => Json(draft).into_response(),
            Format::Turtle => (
                [(header::CONTENT_TYPE, "text/turtle; charset=utf-8")],
                draft.shacl,
            )
                .into_response(),
            Format::Shaclc => (
                [(header::CONTENT_TYPE, "text/shaclc; charset=utf-8")],
                draft.shaclc,
            )
                .into_response(),
            Format::ShexC => (
                [(header::CONTENT_TYPE, "text/shex; charset=utf-8")],
                draft.shex,
            )
                .into_response(),
        })
    })
    .await
}
