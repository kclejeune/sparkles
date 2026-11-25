//! Pattern-based OWL 2 axiom generation over HTTP (spec U12, roadmap item 623):
//!
//! - `POST /$/ontology/{ds}/patterns/preview`: parse CSV/TSV rows and a declarative
//!   ROBOT-template or DOSDP/Cellfie pattern specification against the ontology's
//!   vocabulary and prefixes, returning generated axioms, diff counts against the open
//!   ontology, estimated RDF triple count, auto-declarations, and per-row diagnostics.
//! - `POST /$/ontology/{ds}/patterns/apply`: evaluate the same specification and commit
//!   all generated axioms in one atomic ontology edit, with dataset write authorization,
//!   protected-branch enforcement, `If-Match` preconditions, and `dryRun=true` preview
//!   parity.

use super::*;
use sparkles::owl::manchester;
use sparkles::owl::patterns::{
    self, ColumnDeclaration, ColumnDirective, Delimiter, Limits, PatternResult, PatternRule,
    PatternSpec, PatternSyntax, RuleAnnotation, TemplateMode,
};
use sparkles::owl::{Axiom, Entity, EntityKind, write as owl_write};
use std::collections::{BTreeMap, BTreeSet, HashSet};

const DEFAULT_PREVIEW_LIMIT: usize = 200;
const MAX_PREVIEW_LIMIT: usize = 2_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum OnError {
    Abort,
    Skip,
}

struct ParsedInput {
    table_data: String,
    delimiter: Delimiter,
    spec: PatternSpec,
    declare: bool,
    target: Option<GraphRef>,
    limit: usize,
    message: Option<String>,
    on_error: OnError,
}

fn parse_entity_kind(s: &str) -> ApiResult<EntityKind> {
    match s.trim() {
        "Class" | "class" => Ok(EntityKind::Class),
        "ObjectProperty" | "objectProperty" => Ok(EntityKind::ObjectProperty),
        "DataProperty" | "dataProperty" => Ok(EntityKind::DataProperty),
        "AnnotationProperty" | "annotationProperty" => Ok(EntityKind::AnnotationProperty),
        "NamedIndividual" | "namedIndividual" | "individual" => Ok(EntityKind::NamedIndividual),
        "Datatype" | "datatype" => Ok(EntityKind::Datatype),
        other => Err(bad(format!("unknown entity kind '{other}'"))),
    }
}

fn parse_input(o: &serde_json::Map<String, J>, allow_apply_fields: bool) -> ApiResult<ParsedInput> {
    let table_obj = match o.get("table") {
        Some(J::Object(m)) => m,
        _ => return Err(bad("table: an object with 'data' and optional 'format'")),
    };
    for k in table_obj.keys() {
        if !matches!(k.as_str(), "data" | "format") {
            return Err(bad(format!("table: unknown member '{k}'")));
        }
    }
    let table_data = match table_obj.get("data") {
        Some(J::String(s)) => s.clone(),
        _ => return Err(bad("table.data: a CSV or TSV string")),
    };
    let delimiter = match table_obj.get("format") {
        None | Some(J::Null) => Delimiter::Auto,
        Some(J::String(s)) => Delimiter::parse(s)
            .ok_or_else(|| bad(format!("table.format must be auto, csv or tsv, not '{s}'")))?,
        Some(_) => return Err(bad("table.format: auto, csv or tsv")),
    };

    let spec_obj = match o.get("spec") {
        Some(J::Object(m)) => m,
        _ => return Err(bad("spec: a pattern specification object")),
    };
    for k in spec_obj.keys() {
        if !matches!(
            k.as_str(),
            "mode"
                | "baseIri"
                | "prefixes"
                | "headerTemplateRow"
                | "columns"
                | "rules"
                | "declarations"
        ) {
            return Err(bad(format!("spec: unknown member '{k}'")));
        }
    }

    let base_iri = match spec_obj.get("baseIri") {
        None | Some(J::Null) => None,
        Some(J::String(s)) => Some(s.clone()),
        Some(_) => return Err(bad("spec.baseIri: a string")),
    };
    let mut prefixes = BTreeMap::new();
    match spec_obj.get("prefixes") {
        None | Some(J::Null) => {}
        Some(J::Object(m)) => {
            for (k, v) in m {
                let ns = v
                    .as_str()
                    .ok_or_else(|| bad(format!("spec.prefixes.{k}: a namespace IRI string")))?;
                prefixes.insert(k.clone(), ns.to_string());
            }
        }
        Some(_) => {
            return Err(bad(
                "spec.prefixes: an object mapping prefixes to namespaces",
            ));
        }
    }

    let mode_str = match spec_obj.get("mode") {
        Some(J::String(s)) => s.as_str(),
        _ => return Err(bad("spec.mode: 'robot' or 'pattern'")),
    };
    let mode = match mode_str {
        "robot" => {
            let mut columns = Vec::new();
            match spec_obj.get("columns") {
                None | Some(J::Null) => {}
                Some(J::Array(arr)) => {
                    for (i, item) in arr.iter().enumerate() {
                        let J::Object(m) = item else {
                            return Err(bad(format!("spec.columns[{i}]: an object")));
                        };
                        for k in m.keys() {
                            if !matches!(k.as_str(), "column" | "directive" | "split") {
                                return Err(bad(format!(
                                    "spec.columns[{i}]: unknown member '{k}'"
                                )));
                            }
                        }
                        let column = m
                            .get("column")
                            .and_then(J::as_str)
                            .ok_or_else(|| bad(format!("spec.columns[{i}].column: a string")))?
                            .to_string();
                        let directive = m
                            .get("directive")
                            .and_then(J::as_str)
                            .ok_or_else(|| bad(format!("spec.columns[{i}].directive: a string")))?
                            .to_string();
                        let split = match m.get("split") {
                            None | Some(J::Null) => None,
                            Some(J::String(s)) => Some(s.clone()),
                            Some(_) => {
                                return Err(bad(format!("spec.columns[{i}].split: a string")));
                            }
                        };
                        columns.push(ColumnDirective {
                            column,
                            directive,
                            split,
                        });
                    }
                }
                Some(_) => return Err(bad("spec.columns: an array of column directives")),
            }
            let header_template_row = match spec_obj.get("headerTemplateRow") {
                None | Some(J::Null) => columns.is_empty(),
                Some(J::Bool(b)) => *b,
                Some(_) => return Err(bad("spec.headerTemplateRow: true or false")),
            };
            TemplateMode::Robot {
                header_template_row,
                columns,
            }
        }
        "pattern" => {
            let mut rules = Vec::new();
            match spec_obj.get("rules") {
                None | Some(J::Null) => {}
                Some(J::Array(arr)) => {
                    for (i, item) in arr.iter().enumerate() {
                        let J::Object(m) = item else {
                            return Err(bad(format!("spec.rules[{i}]: an object")));
                        };
                        for k in m.keys() {
                            if !matches!(
                                k.as_str(),
                                "name"
                                    | "template"
                                    | "syntax"
                                    | "skipIfEmpty"
                                    | "requiredColumns"
                                    | "split"
                                    | "annotations"
                            ) {
                                return Err(bad(format!("spec.rules[{i}]: unknown member '{k}'")));
                            }
                        }
                        let name = match m.get("name") {
                            None | Some(J::Null) => None,
                            Some(J::String(s)) => Some(s.clone()),
                            Some(_) => return Err(bad(format!("spec.rules[{i}].name: a string"))),
                        };
                        let template = m
                            .get("template")
                            .and_then(J::as_str)
                            .ok_or_else(|| bad(format!("spec.rules[{i}].template: a string")))?
                            .to_string();
                        let syntax = match m.get("syntax") {
                            None | Some(J::Null) => PatternSyntax::Manchester,
                            Some(J::String(s)) => PatternSyntax::parse(s).ok_or_else(|| {
                                bad(format!(
                                    "spec.rules[{i}].syntax must be manchester or functional, not '{s}'"
                                ))
                            })?,
                            Some(_) => {
                                return Err(bad(format!(
                                    "spec.rules[{i}].syntax: manchester or functional"
                                )));
                            }
                        };
                        let skip_if_empty = match m.get("skipIfEmpty") {
                            None | Some(J::Null) => true,
                            Some(J::Bool(b)) => *b,
                            Some(_) => {
                                return Err(bad(format!(
                                    "spec.rules[{i}].skipIfEmpty: true or false"
                                )));
                            }
                        };
                        let mut required_columns = Vec::new();
                        match m.get("requiredColumns") {
                            None | Some(J::Null) => {}
                            Some(J::Array(reqs)) => {
                                for r in reqs {
                                    let s = r.as_str().ok_or_else(|| {
                                        bad(format!("spec.rules[{i}].requiredColumns: strings"))
                                    })?;
                                    required_columns.push(s.to_string());
                                }
                            }
                            Some(_) => {
                                return Err(bad(format!(
                                    "spec.rules[{i}].requiredColumns: an array of strings"
                                )));
                            }
                        }
                        let mut split = BTreeMap::new();
                        match m.get("split") {
                            None | Some(J::Null) => {}
                            Some(J::Object(sm)) => {
                                for (k, v) in sm {
                                    let d = v.as_str().ok_or_else(|| {
                                        bad(format!("spec.rules[{i}].split.{k}: a string"))
                                    })?;
                                    split.insert(k.clone(), d.to_string());
                                }
                            }
                            Some(_) => {
                                return Err(bad(format!(
                                    "spec.rules[{i}].split: an object mapping columns to delimiters"
                                )));
                            }
                        }
                        let mut annotations = Vec::new();
                        match m.get("annotations") {
                            None | Some(J::Null) => {}
                            Some(J::Array(anns)) => {
                                for (j, a) in anns.iter().enumerate() {
                                    let J::Object(am) = a else {
                                        return Err(bad(format!(
                                            "spec.rules[{i}].annotations[{j}]: an object"
                                        )));
                                    };
                                    for k in am.keys() {
                                        if !matches!(
                                            k.as_str(),
                                            "property" | "value" | "lang" | "datatype" | "iri"
                                        ) {
                                            return Err(bad(format!(
                                                "spec.rules[{i}].annotations[{j}]: unknown member '{k}'"
                                            )));
                                        }
                                    }
                                    let property = am
                                        .get("property")
                                        .and_then(J::as_str)
                                        .ok_or_else(|| {
                                            bad(format!(
                                                "spec.rules[{i}].annotations[{j}].property: a string"
                                            ))
                                        })?
                                        .to_string();
                                    let value = am
                                        .get("value")
                                        .and_then(J::as_str)
                                        .ok_or_else(|| {
                                            bad(format!(
                                                "spec.rules[{i}].annotations[{j}].value: a string"
                                            ))
                                        })?
                                        .to_string();
                                    let lang = match am.get("lang") {
                                        None | Some(J::Null) => None,
                                        Some(J::String(s)) => Some(s.clone()),
                                        Some(_) => {
                                            return Err(bad(format!(
                                                "spec.rules[{i}].annotations[{j}].lang: a string"
                                            )));
                                        }
                                    };
                                    let datatype = match am.get("datatype") {
                                        None | Some(J::Null) => None,
                                        Some(J::String(s)) => Some(s.clone()),
                                        Some(_) => {
                                            return Err(bad(format!(
                                                "spec.rules[{i}].annotations[{j}].datatype: a string"
                                            )));
                                        }
                                    };
                                    let iri = match am.get("iri") {
                                        None | Some(J::Null) => false,
                                        Some(J::Bool(b)) => *b,
                                        Some(_) => {
                                            return Err(bad(format!(
                                                "spec.rules[{i}].annotations[{j}].iri: true or false"
                                            )));
                                        }
                                    };
                                    annotations.push(RuleAnnotation {
                                        property,
                                        value,
                                        lang,
                                        datatype,
                                        iri,
                                    });
                                }
                            }
                            Some(_) => {
                                return Err(bad(format!(
                                    "spec.rules[{i}].annotations: an array of annotations"
                                )));
                            }
                        }
                        rules.push(PatternRule {
                            name,
                            template,
                            syntax,
                            skip_if_empty,
                            required_columns,
                            split,
                            annotations,
                        });
                    }
                }
                Some(_) => return Err(bad("spec.rules: an array of pattern rules")),
            }

            let mut declarations = Vec::new();
            match spec_obj.get("declarations") {
                None | Some(J::Null) => {}
                Some(J::Array(arr)) => {
                    for (i, item) in arr.iter().enumerate() {
                        let J::Object(m) = item else {
                            return Err(bad(format!("spec.declarations[{i}]: an object")));
                        };
                        for k in m.keys() {
                            if !matches!(k.as_str(), "column" | "kind" | "split") {
                                return Err(bad(format!(
                                    "spec.declarations[{i}]: unknown member '{k}'"
                                )));
                            }
                        }
                        let column = m
                            .get("column")
                            .and_then(J::as_str)
                            .ok_or_else(|| bad(format!("spec.declarations[{i}].column: a string")))?
                            .to_string();
                        let kind = m
                            .get("kind")
                            .and_then(J::as_str)
                            .ok_or_else(|| bad(format!("spec.declarations[{i}].kind: a string")))
                            .and_then(parse_entity_kind)?;
                        let split = match m.get("split") {
                            None | Some(J::Null) => None,
                            Some(J::String(s)) => Some(s.clone()),
                            Some(_) => {
                                return Err(bad(format!("spec.declarations[{i}].split: a string")));
                            }
                        };
                        declarations.push(ColumnDeclaration {
                            column,
                            kind,
                            split,
                        });
                    }
                }
                Some(_) => return Err(bad("spec.declarations: an array of column declarations")),
            }

            TemplateMode::Pattern {
                rules,
                declarations,
            }
        }
        other => {
            return Err(bad(format!(
                "spec.mode must be 'robot' or 'pattern', not '{other}'"
            )));
        }
    };

    let declare = match o.get("declare") {
        None | Some(J::Null) => true,
        Some(J::Bool(b)) => *b,
        Some(_) => return Err(bad("declare: true or false")),
    };
    let target = match o.get("target") {
        None | Some(J::Null) => None,
        Some(J::String(g)) => Some(graph_ref(g)?),
        Some(_) => return Err(bad("target: the graph that takes the added axioms")),
    };
    let limit = match o.get("limit") {
        None | Some(J::Null) => DEFAULT_PREVIEW_LIMIT,
        Some(v) => v
            .as_u64()
            .filter(|n| (1..=MAX_PREVIEW_LIMIT as u64).contains(n))
            .ok_or_else(|| bad(format!("limit: an integer from 1 to {MAX_PREVIEW_LIMIT}")))?
            as usize,
    };
    let message = if allow_apply_fields {
        match o.get("message") {
            None | Some(J::Null) => None,
            Some(J::String(m)) => Some(m.clone()),
            Some(_) => return Err(bad("message: a string")),
        }
    } else {
        None
    };
    let on_error = if allow_apply_fields {
        match o.get("onError") {
            None | Some(J::Null) => OnError::Abort,
            Some(J::String(s)) => match s.as_str() {
                "abort" => OnError::Abort,
                "skip" => OnError::Skip,
                other => {
                    return Err(bad(format!(
                        "onError must be 'abort' or 'skip', not '{other}'"
                    )));
                }
            },
            Some(_) => return Err(bad("onError: 'abort' or 'skip'")),
        }
    } else {
        OnError::Abort
    };

    Ok(ParsedInput {
        table_data,
        delimiter,
        spec: PatternSpec {
            mode,
            base_iri,
            prefixes,
        },
        declare,
        target,
        limit,
        message,
        on_error,
    })
}

fn table_error(e: patterns::TableError) -> ApiError {
    let status = match e.code {
        "input-too-large" => StatusCode::PAYLOAD_TOO_LARGE,
        _ => StatusCode::BAD_REQUEST,
    };
    err_code(status, e.code, e.to_string())
}

fn diagnostics_json(res: &PatternResult) -> Vec<J> {
    res.diagnostics
        .iter()
        .map(|d| {
            json!({
                "row": d.row,
                "column": d.column,
                "severity": d.severity.as_str(),
                "code": d.code,
                "message": d.message,
            })
        })
        .collect()
}

fn pattern_summary_json(res: &PatternResult) -> J {
    json!({
        "delimiter": res.delimiter.as_str(),
        "headers": res.headers,
        "rowsProcessed": res.rows_processed,
        "rowsSkipped": res.rows_skipped,
        "rowsWithErrors": res.rows_with_errors,
        "generated": res.axioms.len(),
        "valid": res.is_valid(),
        "diagnostics": diagnostics_json(res),
        "diagnosticsTruncated": res.diagnostics_truncated,
    })
}

/// `POST /$/ontology/{ds}/patterns/preview`: parse spreadsheet rows and a pattern
/// specification against the ontology, returning generated axioms, diff counts, estimated
/// triples, auto-declarations and per-row diagnostics without modifying the dataset.
pub(crate) async fn preview(
    State(st): St,
    Path(name): Path<String>,
    Extension(p): Extension<Principal>,
    uri: Uri,
    bytes: Bytes,
) -> ApiResult {
    let ds = dataset(&st, &name)?;
    let params = Params::from_query(&uri);
    let o = body_object(
        &bytes,
        &[
            "graph", "imports", "mode", "table", "spec", "declare", "target", "limit",
        ],
    )?;
    let req = request_body(&o, request_of(&st, &ds, &params)?)?;
    if req.inferences {
        return Err(bad(
            "pattern preview reads the asserted axioms: leave out 'inferences'",
        ));
    }
    let parsed = parse_input(&o, false)?;
    let opts = CallerContext::for_caller(&st, &ds, &p).options(&params, Endpoint::Query);
    blocking(move || {
        let handle = ds.dataset.ontology();
        let opts = handle.anchored(&opts).map_err(ontology_error)?;
        let opened = handle.open(&req, &opts).map_err(ontology_error)?;
        let vocab = handle.vocabulary(&opened);
        let result = patterns::evaluate(
            &parsed.table_data,
            parsed.delimiter,
            &parsed.spec,
            &vocab,
            &Limits::default(),
        )
        .map_err(table_error)?;

        // Merge dataset prefixes with spec prefixes for clean OFN and Manchester rendering.
        let mut render_prefixes: Vec<(String, String)> = ds.store.prefixes().into_iter().collect();
        for (k, v) in &parsed.spec.prefixes {
            let clean = k.trim().trim_end_matches(':');
            if !clean.is_empty() && !v.trim().is_empty() {
                if let Some((_, existing)) = render_prefixes.iter_mut().find(|(p, _)| p == clean) {
                    *existing = v.trim().to_string();
                } else {
                    render_prefixes.push((clean.to_string(), v.trim().to_string()));
                }
            }
        }
        let ofn_r = Renderer::new(
            render_prefixes
                .iter()
                .map(|(p, n)| (p.as_str(), n.as_str())),
        );
        let mut render_vocab = vocab.clone();
        for (p, n) in &render_prefixes {
            render_vocab.add_prefix(p.as_str(), n.as_str());
        }
        let manch_r = manchester::Renderer::of_vocabulary(&render_vocab);

        // Compute exact new vs existing status and auto-declarations matching `ontology::edit`.
        let mut new_axioms: Vec<&Axiom> = Vec::new();
        let mut existing_count = 0usize;
        let mut statuses: Vec<&'static str> = Vec::with_capacity(result.axioms.len());
        for item in &result.axioms {
            let present = item
                .axiom
                .normal_forms()
                .iter()
                .all(|form| opened.ontology.contains(form));
            if present {
                existing_count += 1;
                statuses.push("existing");
            } else {
                new_axioms.push(&item.axiom);
                statuses.push("new");
            }
        }

        let auto_declarations: Vec<Entity> = if parsed.declare {
            let declared_existing: HashSet<&Entity> = opened
                .ontology
                .axiom_set()
                .filter_map(|a| match &a.kind {
                    sparkles::owl::AxiomKind::Declaration(e) => Some(e),
                    _ => None,
                })
                .collect();
            let mut auto = BTreeSet::new();
            for ax in &new_axioms {
                if matches!(ax.kind, sparkles::owl::AxiomKind::Declaration(_)) {
                    continue;
                }
                for e in ax.signature() {
                    if e.is_builtin()
                        || declared_existing.contains(&e)
                        || e.kind() == EntityKind::NamedIndividual
                    {
                        continue;
                    }
                    auto.insert(e);
                }
            }
            for ax in &new_axioms {
                if let sparkles::owl::AxiomKind::Declaration(e) = &ax.kind {
                    auto.remove(e);
                }
            }
            auto.into_iter().collect()
        } else {
            Vec::new()
        };

        let mut fresh = owl_write::counter("pat");
        let mut estimated_triples = 0usize;
        for ax in &new_axioms {
            estimated_triples += owl_write::axiom_triples(ax, &mut fresh).len();
        }
        for e in &auto_declarations {
            estimated_triples +=
                owl_write::axiom_triples(&Axiom::declaration(e.clone()), &mut fresh).len();
        }

        let truncated = result.axioms.len() > parsed.limit;
        let axioms_json: Vec<J> = result
            .axioms
            .iter()
            .zip(statuses)
            .take(parsed.limit)
            .map(|(item, status)| {
                json!({
                    "type": item.axiom.axiom_type().name(),
                    "ofn": ofn_r.axiom(&item.axiom),
                    "manchester": manch_r.axiom(&item.axiom),
                    "axiom": sparkles::owl::json::axiom(&item.axiom),
                    "rows": item.rows,
                    "status": status,
                })
            })
            .collect();

        let target_graph = parsed
            .target
            .as_ref()
            .or_else(|| opened.graphs.first())
            .map_or("default", GraphRef::as_str);

        let body = json!({
            "dataset": ds.name,
            "commit": opened.snapshot.commit,
            "graphs": opened.graphs.iter().map(GraphRef::as_str).collect::<Vec<_>>(),
            "target": target_graph,
            "delimiter": result.delimiter.as_str(),
            "headers": result.headers,
            "valid": result.is_valid(),
            "rowsProcessed": result.rows_processed,
            "rowsSkipped": result.rows_skipped,
            "rowsWithErrors": result.rows_with_errors,
            "generated": result.axioms.len(),
            "newAxioms": new_axioms.len(),
            "existingAxioms": existing_count,
            "autoDeclarations": auto_declarations.iter().map(|e| json!({
                "iri": e.iri().as_str(),
                "kind": e.kind().name(),
            })).collect::<Vec<_>>(),
            "estimatedTriples": estimated_triples,
            "axioms": axioms_json,
            "axiomsTruncated": truncated,
            "diagnostics": diagnostics_json(&result),
            "diagnosticsTruncated": result.diagnostics_truncated,
        });

        let seq = opened.snapshot.commit;
        let tag = conditional::etag(ds.store.dataset_id(), seq, &tag_format("owl", &req));
        let resp = conditional::with_etag(Json(body).into_response(), &tag, false);
        Ok(with_commit(resp, &ds, seq))
    })
    .await
}

/// `POST /$/ontology/{ds}/patterns/apply`: evaluate spreadsheet rows and a pattern
/// specification and commit all generated axioms in one atomic commit, honoring write
/// authorization, protected branches, `If-Match` preconditions, and `dryRun=true`.
pub(crate) async fn apply(
    State(st): St,
    Path(name): Path<String>,
    Extension(p): Extension<Principal>,
    uri: Uri,
    headers: HeaderMap,
    bytes: Bytes,
) -> ApiResult {
    if st.read_only {
        return Err(err(StatusCode::FORBIDDEN, "server is read-only"));
    }
    let ds = dataset(&st, &name)?;
    let params = Params::from_query(&uri);
    history::reject_at(&params)?;
    let o = body_object(
        &bytes,
        &[
            "graph", "imports", "mode", "table", "spec", "declare", "target", "message", "onError",
        ],
    )?;
    let req = request_body(&o, request_of(&st, &ds, &params)?)?;
    if req.inferences {
        return Err(bad(
            "a pattern commit changes the asserted axioms: leave out 'inferences'",
        ));
    }
    let parsed = parse_input(&o, true)?;
    let mut opts = CallerContext::for_caller(&st, &ds, &p).options(&params, Endpoint::Update);
    let restricted = opts.graphs.is_some();
    let wanted = receipt_wanted(&params, &headers);
    let (mut wopts, _cancel_on_drop) = body_write_options(&st, &params, &headers, Some(&p))?;
    wopts.opts.graphs = opts.graphs.clone();
    if wopts.opts.precondition.is_none() {
        wopts.opts.precondition =
            conditional::write_precondition(&headers, ds.store.dataset_id(), &Target::Dataset);
    }
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
                Some(&p),
                &ds,
                &wopts.opts,
            )
        })
        .transpose()?;
    let (wopts, timeout) = wopts.start();
    let dry = dry.map(|request| request.started(&wopts));
    opts.cancel = wopts.cancel.clone();
    opts.deadline = wopts.deadline;
    opts.timeout = None;
    opts.write = wopts;

    blocking(move || {
        let handle = ds.dataset.ontology();
        let opened = handle.open(&req, &opts).map_err(ontology_error)?;
        let vocab = handle.vocabulary(&opened);
        let result = patterns::evaluate(
            &parsed.table_data,
            parsed.delimiter,
            &parsed.spec,
            &vocab,
            &Limits::default(),
        )
        .map_err(table_error)?;

        if parsed.on_error == OnError::Abort && !result.is_valid() {
            let err_body = json!({
                "error": format!(
                    "pattern evaluation found {} error(s) across {} row(s); no changes were committed",
                    result.error_count(),
                    result.rows_with_errors,
                ),
                "code": "pattern-errors",
                "rowsProcessed": result.rows_processed,
                "rowsSkipped": result.rows_skipped,
                "rowsWithErrors": result.rows_with_errors,
                "diagnostics": diagnostics_json(&result),
                "diagnosticsTruncated": result.diagnostics_truncated,
            });
            return Ok((StatusCode::UNPROCESSABLE_ENTITY, Json(err_body)).into_response());
        }

        let edit = EditRequest {
            add: result.axioms.iter().map(|g| g.axiom.clone()).collect(),
            remove: Vec::new(),
            declare: Some(parsed.declare),
            graph: parsed.target,
            message: parsed
                .message
                .or_else(|| Some("Generate axioms from pattern".to_string())),
        };
        let r = handle.edit(&req, &edit, &opts);
        let e = match (r, dry) {
            (Err(Error::DryRun(preview)), Some(d)) => return Ok(d.respond(&ds, &preview)),
            (Err(e @ Error::Invalid(_)), _) => return Err(ontology_error(e)),
            (r, _) => r.map_err(|e| write_error(e, restricted))?,
        };
        let mut body = edit_body(&e, &renderer(&ds));
        if let Some(map) = body.as_object_mut() {
            map.insert("pattern".to_string(), pattern_summary_json(&result));
        }
        Ok(write_response(
            &ds,
            StatusCode::OK,
            Some(body),
            &e.receipt,
            wanted,
            restricted,
        ))
    })
    .await
    .map_err(|e| with_timeout(e, timeout))
}
