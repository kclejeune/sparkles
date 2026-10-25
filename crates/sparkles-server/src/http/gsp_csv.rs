//! CSV and TSV bodies in the Graph Store protocol (spec F19 §2.3). `PUT`, `POST` and
//! `DELETE` with a `text/csv` or `text/tab-separated-values` body on `/{ds}/data` map
//! the table with a stored mapping (`mapping=NAME`) or the default mapping (`base`, and
//! optionally `key`), and write the triples into the one graph the request addresses.
//! `PUT` replaces the graph, `POST` adds the triples, and `DELETE` removes them.
//!
//! The body is the table a mapping reads. Its name for the mapping is `source=NAME`, or
//! the mapping's only file source, or the CSVW metadata's only table, so a mapping
//! written for a file works unchanged on a Graph Store request.

use super::tabular::{self, Ctx, Mappings};
use super::{
    ApiResult, BodyBudget, Params, Spooled, Target, blocking, conditional, dry_run, err, err_code,
    receipt_wanted, spool, write_error, write_report, write_response,
};
use crate::obs::Op;
use crate::state::{AppState, Dataset};
use axum::http::{HeaderMap, Method, StatusCode};
use axum::response::Response;
use serde_json::json;
use sparkles::Error;
use sparkles::access::GraphAccess;
use sparkles::mapping::MappingKind;
use sparkles::store::ReplaceTarget;
use sparkles::tabular::TabularKind;
use std::sync::Arc;

/// Whether a write's body is a table this module takes.
pub(super) fn is_table(ct: &str) -> Option<TabularKind> {
    sparkles::tabular::tabular_media_type(ct)
}

/// The name the body has for the mapping.
fn body_name(m: &Mappings, params: &Params, kind: TabularKind) -> ApiResult<String> {
    if let Some(s) = params.get("source") {
        let s = s.rsplit(['/', '\\']).next().unwrap_or(s);
        if s.is_empty() {
            return Err(err(StatusCode::BAD_REQUEST, "source must name a file"));
        }
        return Ok(s.to_string());
    }
    let default = || {
        if kind == TabularKind::Tsv {
            "body.tsv".to_string()
        } else {
            "body.csv".to_string()
        }
    };
    let Some(text) = m.mapping.as_deref() else {
        return Ok(default());
    };
    let last = |s: &str| s.rsplit(['/', '\\']).next().unwrap_or(s).to_string();
    match m.kind {
        Some(k @ (MappingKind::Rml | MappingKind::Yarrrml)) => {
            let mapping = sparkles::mapping::parse(text, k, None)?;
            let mut files: Vec<String> = mapping
                .sources
                .iter()
                .filter_map(|s| s.kind.file().map(last))
                .collect();
            files.dedup();
            match files.as_slice() {
                [one] => Ok(one.clone()),
                [] => Err(err(
                    StatusCode::BAD_REQUEST,
                    "the mapping reads no file, so a request body has nothing to map",
                )),
                _ => Err(err(
                    StatusCode::BAD_REQUEST,
                    format!(
                        "the mapping reads {} files ({}); name the body's with source=NAME, and the request maps that one alone",
                        files.len(),
                        files.join(", ")
                    ),
                )),
            }
        }
        Some(MappingKind::Csvw) => {
            let meta = sparkles::tabular::csvw::parse(text, None)
                .map_err(|e| err(StatusCode::BAD_REQUEST, format!("mapping: {e}")))?;
            match meta.tables.as_slice() {
                [t] => Ok(t.url_text.as_deref().map(last).unwrap_or_else(default)),
                _ => Err(err(
                    StatusCode::BAD_REQUEST,
                    "the CSVW metadata describes several tables; name the body's with source=NAME",
                )),
            }
        }
        _ => Ok(default()),
    }
}

/// A Graph Store write with a table body.
#[allow(clippy::too_many_arguments)]
pub(super) async fn write(
    st: Arc<AppState>,
    ds: Arc<Dataset>,
    p: &crate::auth::Principal,
    method: Method,
    params: Params,
    headers: HeaderMap,
    body: axum::body::Body,
    target: Target,
    view: Option<Arc<GraphAccess>>,
    kind: TabularKind,
) -> ApiResult<Response> {
    if st.read_only {
        return Err(err(StatusCode::FORBIDDEN, "server is read-only"));
    }
    super::history::reject_at(&params)?;
    if params.get("mapping").is_none() && params.get("base").is_none() {
        return Err(err(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "a CSV or TSV body needs a mapping: mapping=NAME names a stored one, and base=IRI (with key=COLUMN) uses the default mapping",
        ));
    }
    let graph = match &target {
        Target::Named(iri) => Some(
            oxrdf::NamedNode::new(iri.clone())
                .map_err(|e| err(StatusCode::BAD_REQUEST, e.to_string()))?,
        ),
        Target::Default => None,
        _ => {
            return Err(err(
                StatusCode::BAD_REQUEST,
                "a CSV or TSV body goes into one graph: name it with graph=IRI, or default",
            ));
        }
    };
    let restricted = view.is_some();
    let mut mappings = Mappings::default();
    mappings.resolve(&params, &ds)?;
    let engine = matches!(mappings.kind, Some(MappingKind::Rml | MappingKind::Yarrrml));
    // the default graph has no IRI to write into, so the mapping's graph maps must
    // all name the default graph
    if engine && graph.is_none() {
        let k = mappings.kind.expect("engine");
        let m = sparkles::mapping::parse(mappings.mapping.as_deref().unwrap_or_default(), k, None)?;
        if m.constant_graphs()
            .is_none_or(|gs| gs.iter().any(Option::is_some))
        {
            return Err(err_code(
                StatusCode::BAD_REQUEST,
                "mapping-graph-conflict",
                "the mapping writes into named graphs, and the request addresses the default graph",
            ));
        }
    }
    let name = body_name(&mappings, &params, kind)?;
    let wanted = receipt_wanted(&params, &headers);
    let (mut wopts, _cancel_on_drop) = super::body_write_options(&st, &params, &headers, Some(p))?;
    wopts.opts.precondition =
        conditional::write_precondition(&headers, ds.store.dataset_id(), &target);
    wopts.opts.graphs = view.clone();
    let dry = wopts
        .opts
        .dry_run
        .as_ref()
        .map(|d| {
            dry_run::Request::for_write(
                &st,
                &headers,
                &params,
                d,
                restricted,
                Some(p),
                &ds,
                &wopts.opts,
            )
        })
        .transpose()?;
    let lookup = super::lookup_param(&params)?;
    let tmp = tempfile::Builder::new()
        .prefix("sparkles-gsp-")
        .tempdir()
        .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let path = tmp.path().join(format!("0-{name}"));
    let io = |e: std::io::Error| err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string());
    match spool(body, &mut BodyBudget::new(&st.limits)).await? {
        Spooled::File(f) => {
            f.persist(&path).map_err(|e| io(e.error))?;
        }
        Spooled::Memory(b) => std::fs::write(&path, b).map_err(io)?,
    }
    // a table named by the mapping keeps its extension; the body's media type decides
    // how it is read
    let path = if sparkles::tabular::tabular_kind(&path) == Some(kind) {
        path
    } else {
        let ext = if kind == TabularKind::Tsv {
            "tsv"
        } else {
            "csv"
        };
        let p2 = tmp.path().join(format!("0-{name}.{ext}"));
        std::fs::rename(&path, &p2).map_err(io)?;
        p2
    };
    let (wopts, timeout) = wopts.start();
    let dry = dry.map(|request| request.started(&wopts));
    let grant = crate::server_memory::current_grant();
    blocking(move || {
        let lookup = match lookup {
            true => Some(super::tabular_lookup(&ds, view.clone())?),
            false => None,
        };
        let cx = Ctx {
            st: &st,
            wopts: &wopts,
            grant: grant.as_ref(),
            lookup,
            only_graph: graph.clone(),
            dir: tmp.path(),
        };
        let mut files = vec![path];
        let reports = tabular::convert(&mut files, &mappings, &params, &cx)?;
        let out = files.pop().expect("one output");
        let triples: u64 = reports
            .iter()
            .map(|r| {
                r["quads"]
                    .as_u64()
                    .or_else(|| r["triples"].as_u64())
                    .unwrap_or(0)
            })
            .sum();
        use sparkles::commit::CommitKind;
        let src = || -> ApiResult<sparkles::io::Source> {
            // the mapping engine's N-Quads already name the graph
            let g = if out.extension().is_some_and(|e| e == "nq") {
                None
            } else {
                graph.clone()
            };
            let mut s = sparkles::io::Source::from_path(&out, g)?;
            s.max_decompressed = st.limits.max_decompressed_bytes;
            Ok(s)
        };
        let (status, count, result) = match method {
            Method::PUT => {
                let t = match &graph {
                    Some(g) => ReplaceTarget::Named(g.clone()),
                    None => ReplaceTarget::Default,
                };
                let r = ds
                    .store
                    .replace_with(t, &[src()?], CommitKind::GspPut, &wopts);
                match r {
                    Ok((n, r)) => (StatusCode::OK, n, Ok(r)),
                    Err(e) => (StatusCode::OK, 0, Err(e)),
                }
            }
            Method::POST => {
                let r = ds.store.load_with(&[src()?], CommitKind::GspPost, &wopts);
                let n = r
                    .as_ref()
                    .map(|r| if r.committed { r.commit.inserted } else { 0 })
                    .unwrap_or(0);
                (StatusCode::CREATED, n, r)
            }
            _ => {
                let r = remove(&ds, &out, graph.as_ref(), wopts.clone());
                let n = r.as_ref().map(|(n, _)| *n).unwrap_or(0);
                (StatusCode::OK, n, r.map(|(_, r)| r))
            }
        };
        let receipt = match (result, dry) {
            (Err(Error::DryRun(p)), Some(d)) => return Ok(d.respond(&ds, &p)),
            (r, _) => r.map_err(|e| write_error(e, restricted))?,
        };
        drop(tmp);
        let body = json!({
            "count": count,
            "tripleCount": count,
            "quadCount": count,
            "mapped": triples,
            "tables": reports,
        });
        Ok(write_report(Op::Gsp, count).attach(write_response(
            &ds,
            status,
            Some(body),
            &receipt,
            wanted,
            restricted,
        )))
    })
    .await
    .map_err(|e| super::with_timeout(e, timeout))
}

/// `DELETE` with a table body: remove the mapped triples from the graph, read from the
/// conversion's file as a stream, in one commit. Triples with blank nodes match nothing,
/// since a mapping's blank nodes are new ones.
fn remove(
    ds: &Dataset,
    file: &std::path::Path,
    graph: Option<&oxrdf::NamedNode>,
    wopts: sparkles::guard::WriteOptions,
) -> sparkles::Result<(u64, sparkles::commit::Receipt)> {
    use oxrdf::{GraphNameRef, QuadRef};
    let quads = file.extension().is_some_and(|e| e == "nq");
    let input = std::io::BufReader::new(std::fs::File::open(file)?);
    let format = if quads {
        oxrdfio::RdfFormat::NQuads
    } else {
        oxrdfio::RdfFormat::NTriples
    };
    let parser = oxrdfio::RdfParser::from_format(format).for_reader(input);
    let (n, receipt) = ds.dataset.transaction_receipt_with(wopts, |tx| {
        let mut n = 0u64;
        for q in parser {
            let q = q.map_err(|e| Error::invalid(e.to_string()))?;
            if q.subject.is_blank_node() || q.object.is_blank_node() {
                continue;
            }
            let g = match graph {
                Some(g) => GraphNameRef::NamedNode(g.as_ref()),
                None => GraphNameRef::DefaultGraph,
            };
            if tx.remove(QuadRef::new(&q.subject, &q.predicate, &q.object, g))? {
                n += 1;
            }
        }
        Ok(n)
    })?;
    Ok((n, receipt))
}
