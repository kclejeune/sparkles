//! Pattern-based OWL 2 axiom generation over HTTP (spec U12, roadmap item 623):
//!
//! - `POST /$/ontology/{ds}/patterns/preview`: parse CSV/TSV rows and a declarative
//!   ROBOT-template or DOSDP/Cellfie pattern specification against the ontology's
//!   vocabulary and prefixes, returning generated axioms, diff counts against the open
//!   ontology, estimated RDF triple count, auto-declarations, and per-row diagnostics.
//! - `POST /$/ontology/{ds}/patterns/apply`: evaluate the same specification under the
//!   writer lock against the head snapshot and commit all generated axioms in one atomic
//!   ontology edit, with dataset write authorization, protected-branch enforcement,
//!   `If-Match` preconditions (including import-target digest parity), and `dryRun=true`
//!   preview parity.

use super::*;
use sparkles::handles::ontology::{EditFromHeadError, ImportTargets, Opened};
use sparkles::owl::manchester::{self, Vocabulary};
use sparkles::owl::patterns::{
    self, CancelReason, ColumnDeclaration, ColumnDirective, Delimiter, EvalError, Limits,
    PatternResult, PatternRule, PatternSpec, PatternSyntax, RuleAnnotation, TemplateMode,
};
use sparkles::owl::{Entity, EntityKind, write as owl_write};
use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

const DEFAULT_PREVIEW_LIMIT: usize = 200;
const MAX_PREVIEW_LIMIT: usize = 2_000;
/// Maximum serialized JSON bytes accumulated across preview or apply responses.
const MAX_PATTERN_RESPONSE_BYTES: usize = 16 * 1024 * 1024;

#[cfg(test)]
fn pattern_limits_and_response_cap(st: &AppState) -> (Limits, usize) {
    st.pattern_test_limits
        .lock()
        .unwrap_or((Limits::default(), MAX_PATTERN_RESPONSE_BYTES))
}

#[cfg(not(test))]
fn pattern_limits_and_response_cap(_st: &AppState) -> (Limits, usize) {
    (Limits::default(), MAX_PATTERN_RESPONSE_BYTES)
}

/// Snapshot of mutable resolution metadata (import targets + dataset prefixes) captured
/// once so precondition checking, ontology opening, vocabulary construction, and dry-run
/// reporting all observe the exact same resolution context.
#[derive(Clone)]
struct PatternResolutionContext {
    import_targets: ImportTargets,
    prefixes: BTreeMap<String, String>,
    import_digest: Option<String>,
    pattern_digest: String,
}

fn pattern_context_digest(
    imports_mode: ImportMode,
    import_targets: &ImportTargets,
    prefixes: &BTreeMap<String, String>,
) -> String {
    // Deterministic FNV-1a 64-bit hash over imports mode, import targets, and sorted prefixes.
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    let mut eat = |s: &str| {
        for b in s.bytes().chain([0xff]) {
            h ^= u64::from(b);
            h = h.wrapping_mul(0x0100_0000_01b3);
        }
    };
    match imports_mode {
        ImportMode::None => eat("none"),
        ImportMode::Dataset | ImportMode::Fetch => {
            eat("dataset");
            if !import_targets.is_empty() {
                eat(&format!("{:016x}", import_targets.digest()));
            }
        }
    }
    eat("prefixes");
    for (k, v) in prefixes {
        eat(k);
        eat(v);
    }
    format!("{h:016x}")
}

fn capture_resolution_context(
    st: &AppState,
    ds: &Arc<Dataset>,
    imports_mode: ImportMode,
) -> PatternResolutionContext {
    let import_targets = if imports_mode == ImportMode::None {
        ImportTargets::default()
    } else {
        imports::targets(st, ds)
    };
    let prefixes = ds.store.prefixes();
    let import_digest = (imports_mode != ImportMode::None && !import_targets.is_empty())
        .then(|| format!("{:016x}", import_targets.digest()));
    let pattern_digest = pattern_context_digest(imports_mode, &import_targets, &prefixes);
    PatternResolutionContext {
        import_targets,
        prefixes,
        import_digest,
        pattern_digest,
    }
}

fn capture_resolution_context_once(
    slot: &parking_lot::Mutex<Option<PatternResolutionContext>>,
    st: &AppState,
    ds: &Arc<Dataset>,
    imports_mode: ImportMode,
) -> PatternResolutionContext {
    let mut guard = slot.lock();
    if let Some(existing) = guard.as_ref() {
        return existing.clone();
    }
    let fresh = capture_resolution_context(st, ds, imports_mode);
    *guard = Some(fresh.clone());
    fresh
}

struct PlannedPatternWrite {
    new_indices: Vec<usize>,
    statuses: Vec<&'static str>,
    existing_count: usize,
    auto_declarations: Vec<Entity>,
    estimated_triples: usize,
}

enum PlanError {
    Cancel(CancelReason),
    Ontology(Error),
    TripleLimit(String),
}

/// Shared planning and pre-allocation budget boundary used by BOTH `preview()` and
/// `apply()` (including `dryRun=true`). Computes exact new vs existing axioms,
/// auto-declarations, and zero-allocation projected RDF triple + byte costs before
/// materializing any triples or response payloads.
fn plan_pattern_write(
    opened: &Opened,
    result: &PatternResult,
    declare: bool,
    limits: &Limits,
    cancel_fn: &mut dyn FnMut() -> Result<(), CancelReason>,
) -> std::result::Result<PlannedPatternWrite, PlanError> {
    let mut new_indices: Vec<usize> = Vec::new();
    let mut existing_count = 0usize;
    let mut statuses: Vec<&'static str> = Vec::with_capacity(result.axioms.len());
    for (i, item) in result.axioms.iter().enumerate() {
        if i.is_multiple_of(64) {
            cancel_fn().map_err(PlanError::Cancel)?;
        }
        let present = opened
            .contains_axiom(&item.axiom)
            .map_err(PlanError::Ontology)?;
        if present {
            existing_count += 1;
            statuses.push("existing");
        } else {
            new_indices.push(i);
            statuses.push("new");
        }
    }

    let auto_declarations: Vec<Entity> = if declare {
        let declared_existing: HashSet<&Entity> = opened
            .ontology
            .axiom_set()
            .filter_map(|a| match &a.kind {
                sparkles::owl::AxiomKind::Declaration(e) => Some(e),
                _ => None,
            })
            .collect();
        let mut auto = BTreeSet::new();
        for (i, &idx) in new_indices.iter().enumerate() {
            if i.is_multiple_of(64) {
                cancel_fn().map_err(PlanError::Cancel)?;
            }
            let ax = &result.axioms[idx].axiom;
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
        for &idx in &new_indices {
            if let sparkles::owl::AxiomKind::Declaration(e) = &result.axioms[idx].axiom.kind {
                auto.remove(e);
            }
        }
        auto.into_iter().collect()
    } else {
        Vec::new()
    };

    let max_projected_bytes = limits.max_total_expanded_bytes.saturating_mul(2);
    let mut estimated_triples = 0usize;
    let mut estimated_bytes = 0usize;
    for (i, &idx) in new_indices.iter().enumerate() {
        if i.is_multiple_of(64) {
            cancel_fn().map_err(PlanError::Cancel)?;
        }
        let (triples, bytes) = owl_write::axiom_projection_cost(&result.axioms[idx].axiom);
        estimated_triples = estimated_triples.saturating_add(triples);
        estimated_bytes = estimated_bytes.saturating_add(bytes);
        if estimated_triples > limits.max_triples {
            return Err(PlanError::TripleLimit(format!(
                "projected RDF triples exceeded the limit of {}",
                limits.max_triples
            )));
        }
        if estimated_bytes > max_projected_bytes {
            return Err(PlanError::TripleLimit(format!(
                "projected RDF payload exceeded the {max_projected_bytes} byte limit"
            )));
        }
    }
    for (i, e) in auto_declarations.iter().enumerate() {
        if i.is_multiple_of(64) {
            cancel_fn().map_err(PlanError::Cancel)?;
        }
        estimated_triples = estimated_triples.saturating_add(1);
        estimated_bytes = estimated_bytes.saturating_add(e.iri().as_str().len().saturating_add(64));
        if estimated_triples > limits.max_triples {
            return Err(PlanError::TripleLimit(format!(
                "projected RDF triples exceeded the limit of {}",
                limits.max_triples
            )));
        }
        if estimated_bytes > max_projected_bytes {
            return Err(PlanError::TripleLimit(format!(
                "projected RDF payload exceeded the {max_projected_bytes} byte limit"
            )));
        }
    }

    Ok(PlannedPatternWrite {
        new_indices,
        statuses,
        existing_count,
        auto_declarations,
        estimated_triples,
    })
}

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

fn parse_input(
    o: &serde_json::Map<String, J>,
    allow_apply_fields: bool,
    limits: &Limits,
) -> ApiResult<ParsedInput> {
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
            if m.len() > limits.max_prefixes {
                return Err(bad(format!(
                    "spec.prefixes defines {} entries, exceeding the limit of {}",
                    m.len(),
                    limits.max_prefixes
                )));
            }
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
                    if arr.len() > limits.max_columns_spec {
                        return Err(bad(format!(
                            "spec.columns defines {} entries, exceeding the limit of {}",
                            arr.len(),
                            limits.max_columns_spec
                        )));
                    }
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
                    if arr.len() > limits.max_rules {
                        return Err(bad(format!(
                            "spec.rules defines {} entries, exceeding the limit of {}",
                            arr.len(),
                            limits.max_rules
                        )));
                    }
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
                                if reqs.len() > limits.max_cols {
                                    return Err(bad(format!(
                                        "spec.rules[{i}].requiredColumns exceeds the {} column limit",
                                        limits.max_cols
                                    )));
                                }
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
                                if sm.len() > limits.max_cols {
                                    return Err(bad(format!(
                                        "spec.rules[{i}].split exceeds the {} column limit",
                                        limits.max_cols
                                    )));
                                }
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
                                if anns.len() > limits.max_annotations_per_rule {
                                    return Err(bad(format!(
                                        "spec.rules[{i}].annotations defines {} entries, exceeding the limit of {}",
                                        anns.len(),
                                        limits.max_annotations_per_rule
                                    )));
                                }
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
                                    if iri && (lang.is_some() || datatype.is_some()) {
                                        return Err(bad(format!(
                                            "spec.rules[{i}].annotations[{j}]: cannot combine iri=true with lang or datatype"
                                        )));
                                    }
                                    if lang.is_some() && datatype.is_some() {
                                        return Err(bad(format!(
                                            "spec.rules[{i}].annotations[{j}]: cannot combine lang and datatype"
                                        )));
                                    }
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
                    if arr.len() > limits.max_declarations {
                        return Err(bad(format!(
                            "spec.declarations defines {} entries, exceeding the limit of {}",
                            arr.len(),
                            limits.max_declarations
                        )));
                    }
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

fn check_cancel(
    cancel: Option<&Arc<AtomicBool>>,
    deadline: Option<Instant>,
) -> Result<(), CancelReason> {
    if cancel.is_some_and(|c| c.load(Ordering::Relaxed)) {
        return Err(CancelReason::Cancelled);
    }
    if deadline.is_some_and(|d| Instant::now() >= d) {
        return Err(CancelReason::Timeout);
    }
    Ok(())
}

fn table_error(e: patterns::TableError) -> ApiError {
    let status = match e.code {
        "input-too-large" => StatusCode::PAYLOAD_TOO_LARGE,
        _ => StatusCode::BAD_REQUEST,
    };
    err_code(status, e.code, e.to_string())
}

fn eval_error(e: EvalError) -> ApiError {
    match e {
        EvalError::Table(te) => table_error(te),
        EvalError::Cancelled => ontology_error(Error::Cancelled),
        EvalError::Timeout => ontology_error(Error::Timeout),
    }
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

fn pattern_errors_response(result: &PatternResult) -> Response {
    let message = if result.budget_exceeded {
        format!(
            "pattern evaluation exceeded a request budget ({} error(s)); no changes were committed",
            result.error_count(),
        )
    } else if result.structural_error_count > 0 && result.rows_with_errors == 0 {
        format!(
            "pattern specification has {} error(s); no changes were committed",
            result.error_count(),
        )
    } else {
        format!(
            "pattern evaluation found {} error(s) across {} row(s); no changes were committed",
            result.error_count(),
            result.rows_with_errors,
        )
    };
    let err_body = json!({
        "error": message,
        "code": "pattern-errors",
        "rowsProcessed": result.rows_processed,
        "rowsSkipped": result.rows_skipped,
        "rowsWithErrors": result.rows_with_errors,
        "diagnostics": diagnostics_json(result),
        "diagnosticsTruncated": result.diagnostics_truncated,
    });
    (StatusCode::UNPROCESSABLE_ENTITY, Json(err_body)).into_response()
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
    let req = request_body(&o, request_params(&params)?)?;
    if req.inferences {
        return Err(bad(
            "pattern preview reads the asserted axioms: leave out 'inferences'",
        ));
    }
    let (limits, max_response_bytes) = pattern_limits_and_response_cap(&st);
    let parsed = parse_input(&o, false, &limits)?;
    let mut opts = CallerContext::for_caller(&st, &ds, &p).options(&params, Endpoint::Query);
    let (cancel, _cancel_on_drop) = cancel_on_drop();
    opts.cancel = Some(cancel);
    let st_eval = st.clone();
    blocking(move || {
        let mut req = req;
        // Capture import targets and dataset prefixes ONCE so opening, evaluation,
        // rendering, and the emitted preview entity tag all observe one consistent state.
        let res_ctx = capture_resolution_context(&st_eval, &ds, req.imports);
        req.import_targets = res_ctx.import_targets.clone();
        let handle = ds.dataset.ontology();
        let opts = handle.anchored(&opts).map_err(ontology_error)?;
        let opened = handle.open(&req, &opts).map_err(ontology_error)?;
        let vocab = Vocabulary::of_ontology(&opened.ontology, res_ctx.prefixes.clone());
        let cancel_ref = opts.cancel.clone();
        let deadline = opts.deadline;
        let mut cancel_fn = || check_cancel(cancel_ref.as_ref(), deadline);
        let result = patterns::evaluate_with_cancel(
            &parsed.table_data,
            parsed.delimiter,
            &parsed.spec,
            &vocab,
            &limits,
            &mut cancel_fn,
        )
        .map_err(eval_error)?;

        let planned = plan_pattern_write(&opened, &result, parsed.declare, &limits, &mut cancel_fn)
            .map_err(|e| match e {
                PlanError::Cancel(r) => eval_error(r.into()),
                PlanError::Ontology(err) => ontology_error(err),
                PlanError::TripleLimit(msg) => {
                    err_code(StatusCode::INSUFFICIENT_STORAGE, "triple-limit", msg)
                }
            })?;

        // Merge captured dataset prefixes with spec prefixes for clean OFN and Manchester rendering.
        let mut render_prefixes: Vec<(String, String)> = res_ctx
            .prefixes
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
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

        // Account for fixed envelope, headers, diagnostics, and auto-declarations BEFORE
        // rendering axioms, using actual JSON-serialized byte lengths so escaping is never
        // undercounted and even the first axiom respects `max_response_bytes`.
        let diagnostics_val = diagnostics_json(&result);
        let headers_bytes = serde_json::to_string(&result.headers).map_or(0, |s| s.len());
        let diagnostics_bytes = serde_json::to_string(&diagnostics_val).map_or(0, |s| s.len());
        let mut rendered_bytes = 512usize
            .saturating_add(headers_bytes)
            .saturating_add(diagnostics_bytes);

        let mut truncated = result.axioms.len() > parsed.limit;
        let mut auto_decl_json: Vec<J> = Vec::with_capacity(planned.auto_declarations.len());
        for (i, e) in planned.auto_declarations.iter().enumerate() {
            if i.is_multiple_of(64) {
                cancel_fn().map_err(|r| eval_error(r.into()))?;
            }
            if rendered_bytes >= max_response_bytes {
                truncated = true;
                break;
            }
            let entry = json!({
                "iri": e.iri().as_str(),
                "kind": e.kind().name(),
            });
            let entry_bytes = serde_json::to_string(&entry)
                .map_or(usize::MAX, |s| s.len())
                .saturating_add(1);
            if rendered_bytes.saturating_add(entry_bytes) > max_response_bytes {
                truncated = true;
                break;
            }
            rendered_bytes = rendered_bytes.saturating_add(entry_bytes);
            auto_decl_json.push(entry);
        }

        let mut axioms_json: Vec<J> = Vec::with_capacity(result.axioms.len().min(parsed.limit));
        for (i, (item, status)) in result
            .axioms
            .iter()
            .zip(planned.statuses)
            .take(parsed.limit)
            .enumerate()
        {
            if i.is_multiple_of(64) {
                cancel_fn().map_err(|r| eval_error(r.into()))?;
            }
            if rendered_bytes >= max_response_bytes {
                truncated = true;
                break;
            }
            let ofn = ofn_r.axiom(&item.axiom);
            let manchester = manch_r.axiom(&item.axiom);
            let ax_json = sparkles::owl::json::axiom(&item.axiom);
            let entry = json!({
                "type": item.axiom.axiom_type().name(),
                "ofn": ofn,
                "manchester": manchester,
                "axiom": ax_json,
                "rows": item.rows,
                "status": status,
            });
            let entry_bytes = serde_json::to_string(&entry)
                .map_or(usize::MAX, |s| s.len())
                .saturating_add(1);
            if rendered_bytes.saturating_add(entry_bytes) > max_response_bytes {
                truncated = true;
                break;
            }
            rendered_bytes = rendered_bytes.saturating_add(entry_bytes);
            axioms_json.push(entry);
        }

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
            "newAxioms": planned.new_indices.len(),
            "existingAxioms": planned.existing_count,
            "autoDeclarations": auto_decl_json,
            "estimatedTriples": planned.estimated_triples,
            "axioms": axioms_json,
            "axiomsTruncated": truncated,
            "diagnostics": diagnostics_val,
            "diagnosticsTruncated": result.diagnostics_truncated,
        });

        let seq = opened.snapshot.commit;
        let tag = conditional::etag(
            ds.store.dataset_id(),
            seq,
            &format!("owl-patterns.{}", res_ctx.pattern_digest),
        );
        let resp = conditional::with_etag(Json(body).into_response(), &tag, false);
        Ok(with_commit(resp, &ds, seq))
    })
    .await
}

enum ApplyBuildError {
    Eval(EvalError),
    PatternErrors(Box<PatternResult>),
    TripleLimit(String),
    Ontology(Error),
}

/// `POST /$/ontology/{ds}/patterns/apply`: evaluate spreadsheet rows and a pattern
/// specification under the writer lock and commit all generated axioms in one atomic
/// commit, honoring write authorization, protected branches, `If-Match` preconditions
/// (with import-target and dataset-prefix digest parity), projected triple limits, and
/// `dryRun=true`.
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
    let req = request_body(&o, request_params(&params)?)?;
    if req.inferences {
        return Err(bad(
            "a pattern commit changes the asserted axioms: leave out 'inferences'",
        ));
    }
    let (limits, max_response_bytes) = pattern_limits_and_response_cap(&st);
    let parsed = parse_input(&o, true, &limits)?;
    let mut opts = CallerContext::for_caller(&st, &ds, &p).options(&params, Endpoint::Update);
    let restricted = opts.graphs.is_some();
    let wanted = receipt_wanted(&params, &headers);
    let (mut wopts, _cancel_on_drop) = body_write_options(&st, &params, &headers, Some(&p))?;
    wopts.opts.graphs = opts.graphs.clone();

    // Shared resolution-context slot populated ONCE under the writer lock and reused by
    // the precondition check, ontology opening, vocabulary construction, and dry-run
    // report so import-map and prefix changes can never split the check from evaluation.
    let shared_ctx: Arc<parking_lot::Mutex<Option<PatternResolutionContext>>> =
        Arc::new(parking_lot::Mutex::new(None));
    let imports_mode = req.imports;

    if wopts.opts.precondition.is_none() {
        let st_pre = st.clone();
        let ds_pre = ds.clone();
        let slot_pre = shared_ctx.clone();
        wopts.opts.precondition = conditional::write_precondition_with_pattern_context(
            &headers,
            ds.store.dataset_id(),
            &Target::Dataset,
            move || {
                let ctx =
                    capture_resolution_context_once(&slot_pre, &st_pre, &ds_pre, imports_mode);
                (ctx.import_digest, ctx.pattern_digest)
            },
        );
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

    let st_eval = st.clone();
    blocking(move || {
        let handle = ds.dataset.ontology();
        let commit_msg = parsed
            .message
            .clone()
            .unwrap_or_else(|| "Generate axioms from pattern".to_string());
        let eval_cancel = opts.cancel.clone();
        let eval_deadline = opts.deadline;

        let slot_prepare = shared_ctx.clone();
        let st_prepare = st_eval.clone();
        let ds_prepare = ds.clone();
        let slot_build = shared_ctx.clone();
        let st_build = st_eval.clone();
        let ds_build = ds.clone();

        let target_for_head = parsed.target.clone();
        let msg_for_head = commit_msg.clone();

        let res = handle.edit_from_head(
            &req,
            target_for_head.as_ref(),
            Some(&msg_for_head),
            &opts,
            move |_head, locked_req| {
                let ctx = capture_resolution_context_once(
                    &slot_prepare,
                    &st_prepare,
                    &ds_prepare,
                    locked_req.imports,
                );
                locked_req.import_targets = ctx.import_targets;
            },
            move |opened| {
                let ctx = capture_resolution_context_once(
                    &slot_build,
                    &st_build,
                    &ds_build,
                    imports_mode,
                );
                let vocab = Vocabulary::of_ontology(&opened.ontology, ctx.prefixes.clone());
                let mut cancel_fn = || check_cancel(eval_cancel.as_ref(), eval_deadline);
                let result = patterns::evaluate_with_cancel(
                    &parsed.table_data,
                    parsed.delimiter,
                    &parsed.spec,
                    &vocab,
                    &limits,
                    &mut cancel_fn,
                )
                .map_err(ApplyBuildError::Eval)?;

                // Request-wide budget failures and structural spec errors always refuse
                // the write, under both `abort` and `skip`.
                if result.has_fatal_error()
                    || (parsed.on_error == OnError::Abort && !result.is_valid())
                {
                    return Err(ApplyBuildError::PatternErrors(Box::new(result)));
                }

                // Enforce the exact same projected triple and byte ceilings as preview()
                // before mutating the transaction, under both `abort` and `skip` and in
                // both ordinary apply and `dryRun=true`.
                plan_pattern_write(opened, &result, parsed.declare, &limits, &mut cancel_fn)
                    .map_err(|e| match e {
                        PlanError::Cancel(r) => ApplyBuildError::Eval(r.into()),
                        PlanError::Ontology(err) => ApplyBuildError::Ontology(err),
                        PlanError::TripleLimit(msg) => ApplyBuildError::TripleLimit(msg),
                    })?;

                let edit = EditRequest {
                    add: result.axioms.iter().map(|g| g.axiom.clone()).collect(),
                    remove: Vec::new(),
                    declare: Some(parsed.declare),
                    graph: parsed.target.clone(),
                    message: Some(commit_msg.clone()),
                };
                Ok((edit, (result, ctx.prefixes)))
            },
        );

        let (e, (result, used_prefixes)) = match (res, dry) {
            (Ok(pair), _) => pair,
            (Err(EditFromHeadError::Build(ApplyBuildError::PatternErrors(result))), _) => {
                return Ok(pattern_errors_response(&result));
            }
            (Err(EditFromHeadError::Build(ApplyBuildError::TripleLimit(msg))), _) => {
                return Err(err_code(
                    StatusCode::INSUFFICIENT_STORAGE,
                    "triple-limit",
                    msg,
                ));
            }
            (Err(EditFromHeadError::Build(ApplyBuildError::Ontology(err))), _) => {
                return Err(ontology_error(err));
            }
            (Err(EditFromHeadError::Build(ApplyBuildError::Eval(err))), _) => {
                return Err(eval_error(err));
            }
            (Err(EditFromHeadError::Store(Error::DryRun(preview))), Some(d)) => {
                return Ok(d.respond(&ds, &preview));
            }
            (Err(EditFromHeadError::Store(e @ Error::Invalid(_))), _) => {
                return Err(ontology_error(e));
            }
            (Err(EditFromHeadError::Store(e)), _) => {
                return Err(write_error(e, restricted));
            }
        };

        let pattern_json = pattern_summary_json(&result);
        let pattern_bytes = serde_json::to_string(&pattern_json).map_or(0, |s| s.len());
        let remaining_bytes = max_response_bytes.saturating_sub(pattern_bytes);
        let render = Renderer::new(used_prefixes.iter().map(|(p, n)| (p.as_str(), n.as_str())));
        let mut body = edit_body_bounded(&e, &render, remaining_bytes);
        if let Some(map) = body.as_object_mut() {
            map.insert("pattern".to_string(), pattern_json);
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
