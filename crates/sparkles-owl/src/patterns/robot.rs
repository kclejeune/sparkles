//! ROBOT-template-style column directives (`ID`, `LABEL`, `TYPE`, `SC %`, `EC %`, `DC %`,
//! `SP %`, `C %`, `I %`, `I <prop>`, `TI %`, `A <prop>`, `AL <prop>@lang`, `AT <prop>^^<dt>`,
//! `AI <prop>`, `SPLIT=<sep>`, and axiom annotations `>A`, `>AL`, `>AT`, `>AI`).

use super::{
    CancelReason, Context, ConvergenceStep, Emitter, EvalError, Limits, MAX_VOCAB_ROUNDS,
    PatternSpec, ResolveError, RowCommit, RowDiagnostic, Severity, StructuralDiags, Table,
    TableRow, VocabConvergence, annotation_bytes, bounded_col, parse_cell_literal_charged,
    parse_class_expr_fallback_charged, resolve_iri_charged,
};
use crate::manchester::Vocabulary;
use crate::model::*;
use crate::vocab;
use rustc_hash::FxHashMap;
use std::collections::BTreeSet;

/// Explicit mapping from a column name to a ROBOT directive string.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ColumnDirective {
    /// Column header name to bind to.
    pub column: String,
    /// Directive string, e.g. `ID`, `LABEL`, `SC %`, `SC ex:partOf some % SPLIT=|`, `A rdfs:comment`.
    pub directive: String,
    /// Optional explicit value split delimiter (overrides `SPLIT=` inside `directive` if set).
    pub split: Option<String>,
}

/// Parsed form of one column directive.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ParsedColumn {
    pub col_index: usize,
    pub col_name: String,
    pub kind: DirectiveKind,
    pub split: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum AnnotationFormat {
    /// Plain string literal, or IRI if cell is wrapped in `<...>`.
    Auto,
    /// Language-tagged literal (`@lang`).
    Lang(String),
    /// Typed literal (`^^datatype`).
    Typed(String),
    /// IRI / CURIE value.
    Iri,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum DirectiveKind {
    Ignore,
    Id,
    Label {
        lang: Option<String>,
    },
    Type,
    SubClassOf(String),
    EquivalentClasses(String),
    DisjointClasses(String),
    SubPropertyOf(String),
    ClassTemplate(String),
    Individual(String),
    /// `TI <expr>`: class assertion on the row's named individual.
    IndividualType(String),
    Annotation {
        property: String,
        format: AnnotationFormat,
    },
    AxiomAnnotation {
        property: String,
        format: AnnotationFormat,
    },
}

/// Extract trailing `SPLIT=<delim>` (case-insensitive keyword) from a directive string.
fn extract_split(raw: &str) -> (&str, Option<String>) {
    let trimmed = raw.trim();
    // ASCII case folding preserves UTF-8 byte offsets 1:1.
    let upper = trimmed.to_ascii_uppercase();
    if let Some(pos) = upper.rfind(" SPLIT=") {
        let rest = &trimmed[pos + 7..];
        if !rest.is_empty() && !rest.contains(char::is_whitespace) {
            return (trimmed[..pos].trim_end(), Some(rest.to_string()));
        }
    }
    (trimmed, None)
}

/// Parse one ROBOT directive expression.
pub(crate) fn parse_directive(raw: &str) -> Result<(DirectiveKind, Option<String>), String> {
    let (body, split) = extract_split(raw);
    if body.is_empty() || body == "-" || body.eq_ignore_ascii_case("IGNORE") {
        return Ok((DirectiveKind::Ignore, split));
    }
    if body.eq_ignore_ascii_case("ID") {
        return Ok((DirectiveKind::Id, split));
    }
    if body.eq_ignore_ascii_case("TYPE") {
        return Ok((DirectiveKind::Type, split));
    }
    if body.eq_ignore_ascii_case("LABEL") {
        return Ok((DirectiveKind::Label { lang: None }, split));
    }
    if let Some(rest) = strip_prefix_ci(body, "LABEL@") {
        let lang = rest.trim();
        if lang.is_empty() {
            return Err("LABEL@ requires a language tag".to_string());
        }
        return Ok((
            DirectiveKind::Label {
                lang: Some(lang.to_string()),
            },
            split,
        ));
    }

    // Axiom annotations: >A, >AL, >AT, >AI
    if let Some(after_gt) = body.strip_prefix('>') {
        let (prop, fmt) = parse_annotation_directive(after_gt.trim())?;
        return Ok((
            DirectiveKind::AxiomAnnotation {
                property: prop,
                format: fmt,
            },
            split,
        ));
    }

    // Split head keyword and argument.
    let (head, arg) = match body.split_once(char::is_whitespace) {
        Some((h, a)) => (h.trim(), a.trim()),
        None => (body, ""),
    };

    match head.to_ascii_uppercase().as_str() {
        "SC" => {
            let tmpl = if arg.is_empty() { "%" } else { arg };
            Ok((DirectiveKind::SubClassOf(tmpl.to_string()), split))
        }
        "EC" => {
            let tmpl = if arg.is_empty() { "%" } else { arg };
            Ok((DirectiveKind::EquivalentClasses(tmpl.to_string()), split))
        }
        "DC" => {
            let tmpl = if arg.is_empty() { "%" } else { arg };
            Ok((DirectiveKind::DisjointClasses(tmpl.to_string()), split))
        }
        "SP" => {
            let tmpl = if arg.is_empty() { "%" } else { arg };
            Ok((DirectiveKind::SubPropertyOf(tmpl.to_string()), split))
        }
        "C" => {
            let tmpl = if arg.is_empty() { "%" } else { arg };
            Ok((DirectiveKind::ClassTemplate(tmpl.to_string()), split))
        }
        "I" => {
            let tmpl = if arg.is_empty() { "%" } else { arg };
            Ok((DirectiveKind::Individual(tmpl.to_string()), split))
        }
        "TI" => {
            let tmpl = if arg.is_empty() { "%" } else { arg };
            Ok((DirectiveKind::IndividualType(tmpl.to_string()), split))
        }
        "A" | "AL" | "AT" | "AI" => {
            let (prop, fmt) = parse_annotation_directive(body)?;
            Ok((
                DirectiveKind::Annotation {
                    property: prop,
                    format: fmt,
                },
                split,
            ))
        }
        _ => Err(format!("unknown ROBOT template directive '{body}'")),
    }
}

/// Safely strip an ASCII prefix case-insensitively without risking mid-UTF-8-char slices.
fn strip_prefix_ci<'a>(s: &'a str, prefix: &str) -> Option<&'a str> {
    debug_assert!(prefix.is_ascii());
    let bytes = s.as_bytes();
    if bytes.len() >= prefix.len() && bytes[..prefix.len()].eq_ignore_ascii_case(prefix.as_bytes())
    {
        Some(&s[prefix.len()..])
    } else {
        None
    }
}

fn parse_annotation_directive(body: &str) -> Result<(String, AnnotationFormat), String> {
    let (head, rest) = body
        .split_once(char::is_whitespace)
        .ok_or_else(|| format!("annotation directive '{body}' requires a property"))?;
    let arg = rest.trim();
    if arg.is_empty() {
        return Err(format!("annotation directive '{body}' requires a property"));
    }
    match head.to_ascii_uppercase().as_str() {
        "A" => {
            if let Some((prop, lang)) = arg.rsplit_once('@')
                && !prop.is_empty()
                && !lang.is_empty()
                && !lang.contains(['/', '#'])
            {
                return Ok((
                    prop.trim().to_string(),
                    AnnotationFormat::Lang(lang.trim().to_string()),
                ));
            }
            if let Some((prop, dt)) = arg.rsplit_once("^^")
                && !prop.is_empty()
                && !dt.is_empty()
            {
                return Ok((
                    prop.trim().to_string(),
                    AnnotationFormat::Typed(dt.trim().to_string()),
                ));
            }
            Ok((arg.to_string(), AnnotationFormat::Auto))
        }
        "AL" => {
            let (prop, lang) = arg
                .rsplit_once('@')
                .ok_or_else(|| format!("AL directive '{body}' must include '@<lang>'"))?;
            if prop.trim().is_empty() || lang.trim().is_empty() {
                return Err(format!(
                    "AL directive '{body}' must specify property and language"
                ));
            }
            Ok((
                prop.trim().to_string(),
                AnnotationFormat::Lang(lang.trim().to_string()),
            ))
        }
        "AT" => {
            let (prop, dt) = arg
                .rsplit_once("^^")
                .ok_or_else(|| format!("AT directive '{body}' must include '^^<datatype>'"))?;
            if prop.trim().is_empty() || dt.trim().is_empty() {
                return Err(format!(
                    "AT directive '{body}' must specify property and datatype"
                ));
            }
            Ok((
                prop.trim().to_string(),
                AnnotationFormat::Typed(dt.trim().to_string()),
            ))
        }
        "AI" => Ok((arg.to_string(), AnnotationFormat::Iri)),
        _ => Err(format!("unknown annotation directive '{body}'")),
    }
}

/// `(col_index, directive_string, explicit_split, source_line)`
type TemplateCell = (usize, String, Option<String>, usize);

#[derive(Clone, Debug)]
enum LastColTarget {
    None,
    Axioms(std::ops::Range<usize>),
    EquivalentClasses,
}

/// Resolve the parsed column directives and data rows for ROBOT mode.
#[allow(clippy::too_many_arguments)]
pub(crate) fn evaluate_robot(
    table: &Table,
    header_template_row: bool,
    explicit_columns: &[ColumnDirective],
    base_vocab: &Vocabulary,
    spec: &PatternSpec,
    limits: &Limits,
    initial_diagnostics: &[RowDiagnostic],
    initial_truncated: bool,
    emitter: &mut Emitter,
    check_cancel: &mut dyn FnMut() -> Result<(), CancelReason>,
) -> Result<(), EvalError> {
    let mut header_counts: FxHashMap<&str, usize> = FxHashMap::default();
    let mut first_header_idx: FxHashMap<&str, usize> = FxHashMap::default();
    for (i, h) in table.headers.iter().enumerate() {
        *header_counts.entry(h.as_str()).or_insert(0) += 1;
        first_header_idx.entry(h.as_str()).or_insert(i);
    }

    let mut structural_diags = StructuralDiags::new(limits);
    let (template_cells, data_rows): (Vec<TemplateCell>, &[TableRow]) = if header_template_row {
        let Some((first, rest)) = table.rows.split_first() else {
            emitter.structural_error(RowDiagnostic {
                row: 1,
                column: None,
                severity: Severity::Error,
                code: "missing-template-row".to_string(),
                message: "headerTemplateRow is enabled, but the table has no template row"
                    .to_string(),
            });
            return Ok(());
        };
        let mut cols: Vec<TemplateCell> = first
            .cells
            .iter()
            .enumerate()
            .map(|(i, c)| (i, c.clone(), None, first.line))
            .collect();
        // Merge explicit overrides/supplements in spreadsheet column order.
        for cd in explicit_columns {
            match header_counts.get(cd.column.as_str()).copied().unwrap_or(0) {
                0 => {
                    let col_snip = bounded_col(&cd.column);
                    structural_diags.push(1, Some(&col_snip), "unknown-column", || {
                        format!(
                            "template column '{col_snip}' does not match any header in the table"
                        )
                    });
                }
                1 => {
                    let idx = first_header_idx[cd.column.as_str()];
                    if let Some(slot) = cols.get_mut(idx) {
                        *slot = (idx, cd.directive.clone(), cd.split.clone(), 1);
                    }
                }
                _ => {
                    let col_snip = bounded_col(&cd.column);
                    structural_diags.push(1, Some(&col_snip), "ambiguous-column", || {
                        format!("template column '{col_snip}' matches multiple duplicate headers")
                    });
                }
            }
        }
        (cols, rest)
    } else if !explicit_columns.is_empty() {
        let mut cols = Vec::with_capacity(explicit_columns.len());
        for cd in explicit_columns {
            match header_counts.get(cd.column.as_str()).copied().unwrap_or(0) {
                0 => {
                    let col_snip = bounded_col(&cd.column);
                    structural_diags.push(1, Some(&col_snip), "unknown-column", || {
                        format!(
                            "template column '{col_snip}' does not match any header in the table"
                        )
                    });
                }
                1 => {
                    let idx = first_header_idx[cd.column.as_str()];
                    cols.push((idx, cd.directive.clone(), cd.split.clone(), 1));
                }
                _ => {
                    let col_snip = bounded_col(&cd.column);
                    structural_diags.push(1, Some(&col_snip), "ambiguous-column", || {
                        format!("template column '{col_snip}' matches multiple duplicate headers")
                    });
                }
            }
        }
        (cols, &table.rows[..])
    } else {
        // Treat header names themselves as directives if no explicit columns and no template row.
        let cols = table
            .headers
            .iter()
            .enumerate()
            .map(|(i, h)| (i, h.clone(), None, 1))
            .collect();
        (cols, &table.rows[..])
    };

    if data_rows.len() > limits.max_rows {
        structural_diags.replay_into(emitter);
        emitter.structural_error(RowDiagnostic {
            row: data_rows[limits.max_rows].line,
            column: None,
            severity: Severity::Error,
            code: "too-many-rows".to_string(),
            message: format!(
                "table has {} data rows, exceeding the {} row limit",
                data_rows.len(),
                limits.max_rows
            ),
        });
        return Ok(());
    }

    let mut parsed_cols = Vec::with_capacity(template_cells.len());
    let mut id_col: Option<usize> = None;
    let mut type_cols: Vec<(usize, Option<String>)> = Vec::new();
    let mut ti_cols: Vec<(usize, Option<String>)> = Vec::new();
    let mut label_cols: Vec<(usize, Option<String>)> = Vec::new();

    for (idx, dir_str, explicit_split, dir_line) in template_cells {
        let col_name = table
            .headers
            .get(idx)
            .map(|h| bounded_col(h))
            .unwrap_or_else(|| format!("column_{}", idx + 1));
        match parse_directive(&dir_str) {
            Ok((kind, inline_split)) => {
                let split = explicit_split.or(inline_split);
                if matches!(kind, DirectiveKind::Id) && id_col.is_none() {
                    id_col = Some(idx);
                }
                if matches!(kind, DirectiveKind::Type) {
                    type_cols.push((idx, split.clone()));
                }
                if matches!(kind, DirectiveKind::IndividualType(_)) {
                    ti_cols.push((idx, split.clone()));
                }
                if matches!(kind, DirectiveKind::Label { .. }) {
                    label_cols.push((idx, split.clone()));
                }
                parsed_cols.push(ParsedColumn {
                    col_index: idx,
                    col_name,
                    kind,
                    split,
                });
            }
            Err(msg) => {
                structural_diags.push(dir_line, Some(&col_name), "invalid-directive", || msg);
            }
        }
    }

    let Some(id_idx) = id_col else {
        structural_diags.replay_into(emitter);
        emitter.structural_error(RowDiagnostic {
            row: 1,
            column: None,
            severity: Severity::Error,
            code: "missing-id-column".to_string(),
            message: "ROBOT template requires an ID column directive".to_string(),
        });
        return Ok(());
    };

    let id_diag_col: Option<String> = table.headers.get(id_idx).map(|h| bounded_col(h));
    let n = data_rows.len();
    let mut convergence = VocabConvergence::new(n);

    for round in 0..MAX_VOCAB_ROUNDS {
        if round > 0 {
            emitter.reset_for_retry(initial_diagnostics, initial_truncated);
        }
        structural_diags.replay_into(emitter);

        let mut ctx = Context::build(base_vocab, spec);
        let mut round_contributed = vec![false; n];
        let mut round_failed = vec![false; n];

        // Pass 1a: resolve row subject IRIs, pre-validate Pass 1 splits and entity kinds,
        // and register split labels ONLY for eligible rows with no Pass 1 errors.
        struct RowSubject {
            iri: Iri,
            kind: EntityKind,
            kind_conflict: Option<(EntityKind, EntityKind)>,
        }

        let mut row_subjects: Vec<Option<RowSubject>> = Vec::with_capacity(n);
        let mut row_pass1_ok: Vec<bool> = vec![true; n];
        let id_col_name = id_diag_col.as_deref();

        for (row_i, row) in data_rows.iter().enumerate() {
            if row_i.is_multiple_of(64) {
                check_cancel()?;
            }
            let raw_id = row.cells.get(id_idx).map(|s| s.trim()).unwrap_or("");
            if raw_id.is_empty() {
                row_subjects.push(None);
                continue;
            }
            let subject_iri =
                match resolve_iri_charged(raw_id, &ctx, emitter, row.line, id_col_name) {
                    Ok(iri) => iri,
                    Err(ResolveError::Budget) => return Ok(()),
                    Err(ResolveError::Invalid(_)) => {
                        row_subjects.push(None);
                        continue;
                    }
                };

            let mut declared_kind: Option<EntityKind> = None;
            let mut kind_conflict: Option<(EntityKind, EntityKind)> = None;
            let mut record_kind = |k: EntityKind| match declared_kind {
                None => declared_kind = Some(k),
                Some(prev) if prev == k => {}
                Some(prev) => {
                    if kind_conflict.is_none() {
                        kind_conflict = Some((prev, k));
                    }
                }
            };

            let mut label_vals: Vec<&str> = Vec::new();
            for (l_idx, split) in &label_cols {
                let raw_label = row.cells.get(*l_idx).map(|s| s.trim()).unwrap_or("");
                if raw_label.is_empty() {
                    continue;
                }
                let (vals, truncated) = split_cell_bounded(
                    raw_label,
                    split.as_deref(),
                    limits.max_split_values_per_cell,
                );
                if truncated {
                    row_pass1_ok[row_i] = false;
                } else {
                    label_vals.extend(vals);
                }
            }

            let mut type_tokens: Vec<&str> = Vec::new();
            for (t_idx, split) in &type_cols {
                let cell = row.cells.get(*t_idx).map(|s| s.trim()).unwrap_or("");
                if cell.is_empty() {
                    continue;
                }
                let (vals, truncated) =
                    split_cell_bounded(cell, split.as_deref(), limits.max_split_values_per_cell);
                if truncated {
                    row_pass1_ok[row_i] = false;
                    continue;
                }
                for tok in vals {
                    if let Some(k) = parse_builtin_entity_kind(tok, &ctx) {
                        record_kind(k);
                    } else {
                        record_kind(EntityKind::NamedIndividual);
                        type_tokens.push(tok);
                    }
                }
            }

            for (ti_idx, split) in &ti_cols {
                let cell = row.cells.get(*ti_idx).map(|s| s.trim()).unwrap_or("");
                if cell.is_empty() {
                    continue;
                }
                let (vals, truncated) =
                    split_cell_bounded(cell, split.as_deref(), limits.max_split_values_per_cell);
                if truncated {
                    row_pass1_ok[row_i] = false;
                } else if !vals.is_empty() {
                    record_kind(EntityKind::NamedIndividual);
                }
            }

            if kind_conflict.is_some() {
                row_pass1_ok[row_i] = false;
            }

            let kind = declared_kind.unwrap_or(EntityKind::Class);

            // Register labels for eligible, Pass-1-clean rows now so Pass 1b can resolve
            // class type tokens across rows.
            if convergence.vocab_eligible[row_i] && row_pass1_ok[row_i] {
                for val in label_vals {
                    let vocab_bytes = subject_iri
                        .as_str()
                        .len()
                        .saturating_add(val.len())
                        .saturating_mul(2);
                    if !emitter.charge_vocab_bytes(row.line, vocab_bytes) {
                        return Ok(());
                    }
                    ctx.vocab.add_label(subject_iri.as_str(), val);
                    round_contributed[row_i] = true;
                }
                let subj_bytes = subject_iri.as_str().len().saturating_mul(2);
                if !emitter.charge_vocab_bytes(row.line, subj_bytes) {
                    return Ok(());
                }
                ctx.register_entity(&subject_iri, kind);
                round_contributed[row_i] = true;
            }

            row_subjects.push(Some(RowSubject {
                iri: subject_iri,
                kind,
                kind_conflict,
            }));
            // Defer non-builtin TYPE class registration to Pass 1b after all labels are in.
            let _ = type_tokens;
        }

        // Pass 1b: register simple class IRIs referenced by TYPE columns of eligible rows.
        for (row_i, (row, subj)) in data_rows.iter().zip(&row_subjects).enumerate() {
            if row_i.is_multiple_of(64) {
                check_cancel()?;
            }
            if subj.is_none() || !convergence.vocab_eligible[row_i] || !row_pass1_ok[row_i] {
                continue;
            }
            for (t_idx, split) in &type_cols {
                let cell = row.cells.get(*t_idx).map(|s| s.trim()).unwrap_or("");
                if cell.is_empty() {
                    continue;
                }
                let (vals, truncated) =
                    split_cell_bounded(cell, split.as_deref(), limits.max_split_values_per_cell);
                if truncated {
                    continue;
                }
                for tok in vals {
                    if parse_builtin_entity_kind(tok, &ctx).is_none()
                        && !tok.contains(char::is_whitespace)
                        && !tok.contains(['(', ')', '{', '}'])
                    {
                        match resolve_iri_charged(tok, &ctx, emitter, row.line, None) {
                            Ok(cls_iri) => {
                                let cls_bytes = cls_iri.as_str().len().saturating_mul(2);
                                if !emitter.charge_vocab_bytes(row.line, cls_bytes) {
                                    return Ok(());
                                }
                                ctx.register_entity(&cls_iri, EntityKind::Class);
                                round_contributed[row_i] = true;
                            }
                            Err(ResolveError::Budget) => return Ok(()),
                            Err(ResolveError::Invalid(_)) => {}
                        }
                    }
                }
            }
        }

        // Pass 2: stage and emit axioms and diagnostics per row.
        for (row_i, (row, subject_info)) in data_rows.iter().zip(row_subjects).enumerate() {
            check_cancel()?;

            let raw_id = row.cells.get(id_idx).map(|s| s.trim()).unwrap_or("");
            if raw_id.is_empty() {
                let any_data = row.cells.iter().any(|c| !c.trim().is_empty());
                if any_data {
                    round_failed[row_i] = true;
                    emitter.mark_row_error(row.line);
                    emitter.diag(RowDiagnostic {
                        row: row.line,
                        column: id_diag_col.clone(),
                        severity: Severity::Error,
                        code: "missing-id".to_string(),
                        message: "row has values but its ID cell is empty".to_string(),
                    });
                } else {
                    emitter.rows_skipped += 1;
                }
                continue;
            }

            let Some(RowSubject {
                iri: subject_iri,
                kind: entity_kind,
                kind_conflict,
            }) = subject_info
            else {
                round_failed[row_i] = true;
                emitter.mark_row_error(row.line);
                let msg = match resolve_iri_charged(raw_id, &ctx, emitter, row.line, id_col_name) {
                    Err(ResolveError::Invalid(m)) => m,
                    Err(ResolveError::Budget) => return Ok(()),
                    Ok(_) => format!("cannot resolve ID '{raw_id}' to a valid IRI"),
                };
                emitter.diag(RowDiagnostic {
                    row: row.line,
                    column: id_diag_col.clone(),
                    severity: Severity::Error,
                    code: "invalid-iri".to_string(),
                    message: msg,
                });
                continue;
            };

            emitter.rows_processed += 1;

            let mut row_has_error = false;
            if let Some((k1, k2)) = kind_conflict {
                row_has_error = true;
                emitter.diag(RowDiagnostic {
                    row: row.line,
                    column: None,
                    severity: Severity::Error,
                    code: "conflicting-type".to_string(),
                    message: format!(
                        "row declares incompatible entity types ({} and {})",
                        k1.name(),
                        k2.name()
                    ),
                });
            }

            // Stage entity declaration and all axioms for this row; nothing enters `emitter`
            // until the entire row finishes without errors.
            let subj_len = subject_iri.as_str().len();
            if !emitter.charge_expanded_bytes(subj_len.saturating_mul(2), row.line, id_col_name) {
                return Ok(());
            }
            let entity = make_entity(subject_iri.clone(), entity_kind);
            let mut row_declarations: Vec<Entity> = vec![entity.clone()];
            let mut row_axioms: Vec<(Axiom, Option<&str>)> =
                vec![(Axiom::declaration(entity), None)];
            let mut ec_exprs: Vec<ClassExpression> = Vec::new();
            let mut ec_annotations: BTreeSet<Annotation> = BTreeSet::new();
            let mut ec_col_name: Option<&str> = None;
            let mut last_col_target = LastColTarget::None;

            if !emitter.charge_attempt(1, row.line, None) {
                return Ok(());
            }

            for col in &parsed_cols {
                if matches!(col.kind, DirectiveKind::Ignore | DirectiveKind::Id) {
                    continue;
                }
                let col_str = col.col_name.as_str();
                let raw_cell = row.cells.get(col.col_index).map(|s| s.trim()).unwrap_or("");
                if raw_cell.is_empty() {
                    if !matches!(col.kind, DirectiveKind::AxiomAnnotation { .. }) {
                        last_col_target = LastColTarget::None;
                    }
                    continue;
                }

                let (values, split_truncated) = split_cell_bounded(
                    raw_cell,
                    col.split.as_deref(),
                    limits.max_split_values_per_cell,
                );
                if split_truncated {
                    row_has_error = true;
                    emitter.diag(RowDiagnostic {
                        row: row.line,
                        column: Some(col.col_name.clone()),
                        severity: Severity::Error,
                        code: "expansion-limit".to_string(),
                        message: format!(
                            "cell split in column '{col_str}' exceeds the limit of {} values",
                            limits.max_split_values_per_cell
                        ),
                    });
                    if !matches!(col.kind, DirectiveKind::AxiomAnnotation { .. }) {
                        last_col_target = LastColTarget::None;
                    }
                    continue;
                }

                if let DirectiveKind::AxiomAnnotation { property, format } = &col.kind {
                    let target_count = match &last_col_target {
                        LastColTarget::None => 0,
                        LastColTarget::Axioms(r) => r.len(),
                        LastColTarget::EquivalentClasses => usize::from(!ec_exprs.is_empty()),
                    };
                    if target_count == 0 {
                        emitter.diag(RowDiagnostic {
                            row: row.line,
                            column: Some(col.col_name.clone()),
                            severity: Severity::Warning,
                            code: "orphan-axiom-annotation".to_string(),
                            message: "axiom annotation column has no preceding axiom on this row"
                                .to_string(),
                        });
                        continue;
                    }

                    // Charge total annotation work BEFORE looping to prevent quadratic blowup.
                    let work = target_count.saturating_mul(values.len());
                    if !emitter.charge_attempt(work, row.line, Some(col_str)) {
                        return Ok(());
                    }

                    let prop_iri =
                        match resolve_iri_charged(property, &ctx, emitter, row.line, Some(col_str))
                        {
                            Ok(p) => p,
                            Err(ResolveError::Budget) => return Ok(()),
                            Err(ResolveError::Invalid(msg)) => {
                                row_has_error = true;
                                emitter.diag(RowDiagnostic {
                                    row: row.line,
                                    column: Some(col.col_name.clone()),
                                    severity: Severity::Error,
                                    code: "invalid-iri".to_string(),
                                    message: msg,
                                });
                                continue;
                            }
                        };

                    // Charge the per-value property IRI clones BEFORE building `built_anns`.
                    let prop_clone_bytes = prop_iri.as_str().len().saturating_mul(values.len());
                    if !emitter.charge_expanded_bytes(prop_clone_bytes, row.line, Some(col_str)) {
                        return Ok(());
                    }

                    let mut built_anns: Vec<Annotation> = Vec::with_capacity(values.len());
                    for val in values {
                        match build_annotation_value_charged(
                            val, format, &ctx, emitter, row.line, col_str,
                        ) {
                            Ok(ann_val) => {
                                built_anns.push(Annotation {
                                    property: AnnotationProperty(prop_iri.clone()),
                                    value: ann_val,
                                    annotations: Default::default(),
                                });
                            }
                            Err(ResolveError::Budget) => return Ok(()),
                            Err(ResolveError::Invalid(msg)) => {
                                row_has_error = true;
                                emitter.diag(RowDiagnostic {
                                    row: row.line,
                                    column: Some(col.col_name.clone()),
                                    severity: Severity::Error,
                                    code: "invalid-annotation-value".to_string(),
                                    message: msg,
                                });
                            }
                        }
                    }

                    // Charge replicated annotation bytes BEFORE cloning into every target.
                    let total_ann_bytes = built_anns
                        .iter()
                        .map(annotation_bytes)
                        .fold(0usize, usize::saturating_add);
                    let replicated_bytes = total_ann_bytes.saturating_mul(target_count);
                    if !emitter.charge_expanded_bytes(replicated_bytes, row.line, Some(col_str)) {
                        return Ok(());
                    }

                    match &last_col_target {
                        LastColTarget::None => {}
                        LastColTarget::EquivalentClasses => {
                            if ec_annotations.len().saturating_add(built_anns.len())
                                > limits.max_annotations_per_axiom
                            {
                                row_has_error = true;
                                emitter.diag(RowDiagnostic {
                                    row: row.line,
                                    column: Some(col.col_name.clone()),
                                    severity: Severity::Error,
                                    code: "annotation-limit".to_string(),
                                    message: format!(
                                        "axiom annotations on this row exceed the limit of {} per axiom",
                                        limits.max_annotations_per_axiom
                                    ),
                                });
                            } else {
                                ec_annotations.extend(built_anns);
                            }
                        }
                        LastColTarget::Axioms(range) => {
                            let max_existing = row_axioms[range.clone()]
                                .iter()
                                .map(|(ax, _)| ax.annotations.len())
                                .max()
                                .unwrap_or(0);
                            if max_existing.saturating_add(built_anns.len())
                                > limits.max_annotations_per_axiom
                            {
                                row_has_error = true;
                                emitter.diag(RowDiagnostic {
                                    row: row.line,
                                    column: Some(col.col_name.clone()),
                                    severity: Severity::Error,
                                    code: "annotation-limit".to_string(),
                                    message: format!(
                                        "axiom annotations on this row exceed the limit of {} per axiom",
                                        limits.max_annotations_per_axiom
                                    ),
                                });
                            } else {
                                for (ax, _) in &mut row_axioms[range.clone()] {
                                    for ann in &built_anns {
                                        ax.annotations.insert(ann.clone());
                                    }
                                }
                            }
                        }
                    }
                    continue;
                }

                if matches!(col.kind, DirectiveKind::EquivalentClasses(_)) {
                    let DirectiveKind::EquivalentClasses(tmpl) = &col.kind else {
                        unreachable!()
                    };
                    let before_ec = ec_exprs.len();
                    for (val_i, val) in values.into_iter().enumerate() {
                        if val_i.is_multiple_of(64) {
                            check_cancel()?;
                        }
                        if !emitter.charge_attempt(1, row.line, Some(col_str)) {
                            return Ok(());
                        }
                        let Some(expanded) = expand_percent_bounded(
                            tmpl,
                            val,
                            limits.max_expanded_bytes_per_template,
                            emitter,
                            row.line,
                            col_str,
                            &mut row_has_error,
                        ) else {
                            if emitter.budget_exceeded {
                                return Ok(());
                            }
                            continue;
                        };
                        match parse_class_expr_fallback_charged(
                            &expanded,
                            &ctx,
                            emitter,
                            row.line,
                            Some(col_str),
                        ) {
                            Ok(other) => {
                                ec_exprs.push(other);
                                if ec_col_name.is_none() {
                                    ec_col_name = Some(col_str);
                                }
                            }
                            Err(ResolveError::Budget) => return Ok(()),
                            Err(ResolveError::Invalid(msg)) => {
                                row_has_error = true;
                                emitter.diag(RowDiagnostic {
                                    row: row.line,
                                    column: Some(col.col_name.clone()),
                                    severity: Severity::Error,
                                    code: "invalid-expression".to_string(),
                                    message: msg,
                                });
                            }
                        }
                    }
                    last_col_target = if ec_exprs.len() > before_ec {
                        LastColTarget::EquivalentClasses
                    } else {
                        LastColTarget::None
                    };
                    continue;
                }

                let start_idx = row_axioms.len();
                for (val_i, val) in values.into_iter().enumerate() {
                    if val_i.is_multiple_of(64) {
                        check_cancel()?;
                    }
                    if !emitter.charge_attempt(1, row.line, Some(col_str)) {
                        return Ok(());
                    }
                    if row_axioms.len() >= emitter.remaining_axiom_capacity().saturating_add(1)
                        && row_axioms.len() > limits.max_axioms
                    {
                        emitter.budget_error(
                            row.line,
                            Some(col_str),
                            "axiom-limit",
                            format!(
                                "generated axioms exceeded the limit of {}",
                                limits.max_axioms
                            ),
                        );
                        return Ok(());
                    }

                    match &col.kind {
                        DirectiveKind::Label { lang } => {
                            let bytes = val
                                .len()
                                .saturating_add(lang.as_deref().map_or(0, str::len))
                                .saturating_add(subj_len);
                            if !emitter.charge_expanded_bytes(bytes, row.line, Some(col_str)) {
                                return Ok(());
                            }
                            let lit = match lang {
                                Some(l) => match Literal::lang(val, l) {
                                    Ok(lit) => lit,
                                    Err(e) => {
                                        row_has_error = true;
                                        emitter.diag(RowDiagnostic {
                                            row: row.line,
                                            column: Some(col.col_name.clone()),
                                            severity: Severity::Error,
                                            code: "invalid-language-tag".to_string(),
                                            message: e.0,
                                        });
                                        continue;
                                    }
                                },
                                None => Literal::simple(val),
                            };
                            row_axioms.push((
                                Axiom::new(AxiomKind::AnnotationAssertion(
                                    AnnotationProperty::new(vocab::RDFS_LABEL),
                                    AnnotationSubject::Iri(subject_iri.clone()),
                                    AnnotationValue::Literal(lit),
                                )),
                                Some(col_str),
                            ));
                        }
                        DirectiveKind::Type => {
                            if parse_builtin_entity_kind(val, &ctx).is_some() {
                                // Already handled in declaration.
                            } else {
                                match parse_class_expr_fallback_charged(
                                    val,
                                    &ctx,
                                    emitter,
                                    row.line,
                                    Some(col_str),
                                ) {
                                    Ok(ce) => {
                                        if !emitter.charge_expanded_bytes(
                                            subj_len,
                                            row.line,
                                            Some(col_str),
                                        ) {
                                            return Ok(());
                                        }
                                        row_axioms.push((
                                            Axiom::class_assertion(
                                                ce,
                                                Individual::Named(NamedIndividual(
                                                    subject_iri.clone(),
                                                )),
                                            ),
                                            Some(col_str),
                                        ));
                                    }
                                    Err(ResolveError::Budget) => return Ok(()),
                                    Err(ResolveError::Invalid(msg)) => {
                                        row_has_error = true;
                                        emitter.diag(RowDiagnostic {
                                            row: row.line,
                                            column: Some(col.col_name.clone()),
                                            severity: Severity::Error,
                                            code: "invalid-type".to_string(),
                                            message: msg,
                                        });
                                    }
                                }
                            }
                        }
                        DirectiveKind::SubClassOf(tmpl) => {
                            let Some(expanded) = expand_percent_bounded(
                                tmpl,
                                val,
                                limits.max_expanded_bytes_per_template,
                                emitter,
                                row.line,
                                col_str,
                                &mut row_has_error,
                            ) else {
                                if emitter.budget_exceeded {
                                    return Ok(());
                                }
                                continue;
                            };
                            match parse_class_expr_fallback_charged(
                                &expanded,
                                &ctx,
                                emitter,
                                row.line,
                                Some(col_str),
                            ) {
                                Ok(sup) => {
                                    if !emitter.charge_expanded_bytes(
                                        subj_len,
                                        row.line,
                                        Some(col_str),
                                    ) {
                                        return Ok(());
                                    }
                                    row_axioms.push((
                                        Axiom::sub_class_of(Class(subject_iri.clone()), sup),
                                        Some(col_str),
                                    ));
                                }
                                Err(ResolveError::Budget) => return Ok(()),
                                Err(ResolveError::Invalid(msg)) => {
                                    row_has_error = true;
                                    emitter.diag(RowDiagnostic {
                                        row: row.line,
                                        column: Some(col.col_name.clone()),
                                        severity: Severity::Error,
                                        code: "invalid-expression".to_string(),
                                        message: msg,
                                    });
                                }
                            }
                        }
                        DirectiveKind::DisjointClasses(tmpl) => {
                            let Some(expanded) = expand_percent_bounded(
                                tmpl,
                                val,
                                limits.max_expanded_bytes_per_template,
                                emitter,
                                row.line,
                                col_str,
                                &mut row_has_error,
                            ) else {
                                if emitter.budget_exceeded {
                                    return Ok(());
                                }
                                continue;
                            };
                            match parse_class_expr_fallback_charged(
                                &expanded,
                                &ctx,
                                emitter,
                                row.line,
                                Some(col_str),
                            ) {
                                Ok(other) => {
                                    if !emitter.charge_expanded_bytes(
                                        subj_len,
                                        row.line,
                                        Some(col_str),
                                    ) {
                                        return Ok(());
                                    }
                                    row_axioms.push((
                                        Axiom::disjoint_classes([
                                            ClassExpression::Class(Class(subject_iri.clone())),
                                            other,
                                        ]),
                                        Some(col_str),
                                    ));
                                }
                                Err(ResolveError::Budget) => return Ok(()),
                                Err(ResolveError::Invalid(msg)) => {
                                    row_has_error = true;
                                    emitter.diag(RowDiagnostic {
                                        row: row.line,
                                        column: Some(col.col_name.clone()),
                                        severity: Severity::Error,
                                        code: "invalid-expression".to_string(),
                                        message: msg,
                                    });
                                }
                            }
                        }
                        DirectiveKind::SubPropertyOf(tmpl) => {
                            let Some(expanded) = expand_percent_bounded(
                                tmpl,
                                val,
                                limits.max_expanded_bytes_per_template,
                                emitter,
                                row.line,
                                col_str,
                                &mut row_has_error,
                            ) else {
                                if emitter.budget_exceeded {
                                    return Ok(());
                                }
                                continue;
                            };
                            match resolve_iri_charged(
                                &expanded,
                                &ctx,
                                emitter,
                                row.line,
                                Some(col_str),
                            ) {
                                Ok(sup_iri) => {
                                    if !emitter.charge_expanded_bytes(
                                        subj_len,
                                        row.line,
                                        Some(col_str),
                                    ) {
                                        return Ok(());
                                    }
                                    let ax = match entity_kind {
                                        EntityKind::DataProperty => {
                                            Axiom::new(AxiomKind::SubDataPropertyOf(
                                                DataProperty(subject_iri.clone()),
                                                DataProperty(sup_iri),
                                            ))
                                        }
                                        EntityKind::AnnotationProperty => {
                                            Axiom::new(AxiomKind::SubAnnotationPropertyOf(
                                                AnnotationProperty(subject_iri.clone()),
                                                AnnotationProperty(sup_iri),
                                            ))
                                        }
                                        _ => Axiom::new(AxiomKind::SubObjectPropertyOf(
                                            SubObjectPropertyExpression::Expression(
                                                ObjectPropertyExpression::Property(ObjectProperty(
                                                    subject_iri.clone(),
                                                )),
                                            ),
                                            ObjectPropertyExpression::Property(ObjectProperty(
                                                sup_iri,
                                            )),
                                        )),
                                    };
                                    row_axioms.push((ax, Some(col_str)));
                                }
                                Err(ResolveError::Budget) => return Ok(()),
                                Err(ResolveError::Invalid(msg)) => {
                                    row_has_error = true;
                                    emitter.diag(RowDiagnostic {
                                        row: row.line,
                                        column: Some(col.col_name.clone()),
                                        severity: Severity::Error,
                                        code: "invalid-iri".to_string(),
                                        message: msg,
                                    });
                                }
                            }
                        }
                        DirectiveKind::ClassTemplate(tmpl) => {
                            let Some(expanded) = expand_percent_bounded(
                                tmpl,
                                val,
                                limits.max_expanded_bytes_per_template,
                                emitter,
                                row.line,
                                col_str,
                                &mut row_has_error,
                            ) else {
                                if emitter.budget_exceeded {
                                    return Ok(());
                                }
                                continue;
                            };
                            match parse_class_expr_fallback_charged(
                                &expanded,
                                &ctx,
                                emitter,
                                row.line,
                                Some(col_str),
                            ) {
                                Ok(ce) => {
                                    if !emitter.charge_expanded_bytes(
                                        subj_len,
                                        row.line,
                                        Some(col_str),
                                    ) {
                                        return Ok(());
                                    }
                                    if entity_kind == EntityKind::NamedIndividual {
                                        row_axioms.push((
                                            Axiom::class_assertion(
                                                ce,
                                                Individual::Named(NamedIndividual(
                                                    subject_iri.clone(),
                                                )),
                                            ),
                                            Some(col_str),
                                        ));
                                    } else {
                                        row_axioms.push((
                                            Axiom::sub_class_of(Class(subject_iri.clone()), ce),
                                            Some(col_str),
                                        ));
                                    }
                                }
                                Err(ResolveError::Budget) => return Ok(()),
                                Err(ResolveError::Invalid(msg)) => {
                                    row_has_error = true;
                                    emitter.diag(RowDiagnostic {
                                        row: row.line,
                                        column: Some(col.col_name.clone()),
                                        severity: Severity::Error,
                                        code: "invalid-expression".to_string(),
                                        message: msg,
                                    });
                                }
                            }
                        }
                        DirectiveKind::IndividualType(tmpl) => {
                            let Some(expanded) = expand_percent_bounded(
                                tmpl,
                                val,
                                limits.max_expanded_bytes_per_template,
                                emitter,
                                row.line,
                                col_str,
                                &mut row_has_error,
                            ) else {
                                if emitter.budget_exceeded {
                                    return Ok(());
                                }
                                continue;
                            };
                            match parse_class_expr_fallback_charged(
                                &expanded,
                                &ctx,
                                emitter,
                                row.line,
                                Some(col_str),
                            ) {
                                Ok(ce) => {
                                    if !emitter.charge_expanded_bytes(
                                        subj_len,
                                        row.line,
                                        Some(col_str),
                                    ) {
                                        return Ok(());
                                    }
                                    row_axioms.push((
                                        Axiom::class_assertion(
                                            ce,
                                            Individual::Named(NamedIndividual(subject_iri.clone())),
                                        ),
                                        Some(col_str),
                                    ));
                                }
                                Err(ResolveError::Budget) => return Ok(()),
                                Err(ResolveError::Invalid(msg)) => {
                                    row_has_error = true;
                                    emitter.diag(RowDiagnostic {
                                        row: row.line,
                                        column: Some(col.col_name.clone()),
                                        severity: Severity::Error,
                                        code: "invalid-expression".to_string(),
                                        message: msg,
                                    });
                                }
                            }
                        }
                        DirectiveKind::Individual(tmpl) => {
                            if tmpl == "%" {
                                // `I %`: cell names an individual of the row's class.
                                match resolve_iri_charged(
                                    val,
                                    &ctx,
                                    emitter,
                                    row.line,
                                    Some(col_str),
                                ) {
                                    Ok(ind_iri) => {
                                        let clone_bytes = ind_iri
                                            .as_str()
                                            .len()
                                            .saturating_mul(2)
                                            .saturating_add(if entity_kind == EntityKind::Class {
                                                subj_len
                                            } else {
                                                0
                                            });
                                        if !emitter.charge_expanded_bytes(
                                            clone_bytes,
                                            row.line,
                                            Some(col_str),
                                        ) {
                                            return Ok(());
                                        }
                                        let ind = NamedIndividual(ind_iri);
                                        row_declarations.push(Entity::NamedIndividual(ind.clone()));
                                        row_axioms.push((
                                            Axiom::declaration(Entity::NamedIndividual(
                                                ind.clone(),
                                            )),
                                            Some(col_str),
                                        ));
                                        if entity_kind == EntityKind::Class {
                                            row_axioms.push((
                                                Axiom::class_assertion(
                                                    Class(subject_iri.clone()),
                                                    Individual::Named(ind),
                                                ),
                                                Some(col_str),
                                            ));
                                        }
                                    }
                                    Err(ResolveError::Budget) => return Ok(()),
                                    Err(ResolveError::Invalid(msg)) => {
                                        row_has_error = true;
                                        emitter.diag(RowDiagnostic {
                                            row: row.line,
                                            column: Some(col.col_name.clone()),
                                            severity: Severity::Error,
                                            code: "invalid-iri".to_string(),
                                            message: msg,
                                        });
                                    }
                                }
                            } else {
                                // `I <prop>`: property assertion on the row's individual.
                                match resolve_iri_charged(
                                    tmpl,
                                    &ctx,
                                    emitter,
                                    row.line,
                                    Some(col_str),
                                ) {
                                    Ok(prop_iri) => {
                                        let is_data_prop = ctx
                                            .vocab
                                            .kinds(prop_iri.as_str())
                                            .contains(&EntityKind::DataProperty);
                                        if is_data_prop {
                                            if !emitter.charge_expanded_bytes(
                                                val.len().saturating_add(subj_len),
                                                row.line,
                                                Some(col_str),
                                            ) {
                                                return Ok(());
                                            }
                                            let lit = match parse_cell_literal_charged(
                                                val,
                                                &ctx,
                                                emitter,
                                                row.line,
                                                Some(col_str),
                                            ) {
                                                Ok(l) => l,
                                                Err(ResolveError::Budget) => return Ok(()),
                                                Err(ResolveError::Invalid(msg)) => {
                                                    row_has_error = true;
                                                    emitter.diag(RowDiagnostic {
                                                        row: row.line,
                                                        column: Some(col.col_name.clone()),
                                                        severity: Severity::Error,
                                                        code: "expansion-bytes-limit".to_string(),
                                                        message: msg,
                                                    });
                                                    continue;
                                                }
                                            };
                                            row_axioms.push((
                                                Axiom::new(AxiomKind::DataPropertyAssertion(
                                                    DataProperty(prop_iri),
                                                    Individual::Named(NamedIndividual(
                                                        subject_iri.clone(),
                                                    )),
                                                    lit,
                                                )),
                                                Some(col_str),
                                            ));
                                        } else {
                                            match resolve_iri_charged(
                                                val,
                                                &ctx,
                                                emitter,
                                                row.line,
                                                Some(col_str),
                                            ) {
                                                Ok(obj_iri) => {
                                                    if !emitter.charge_expanded_bytes(
                                                        subj_len,
                                                        row.line,
                                                        Some(col_str),
                                                    ) {
                                                        return Ok(());
                                                    }
                                                    row_axioms.push((
                                                        Axiom::new(
                                                            AxiomKind::ObjectPropertyAssertion(
                                                                ObjectPropertyExpression::Property(
                                                                    ObjectProperty(prop_iri),
                                                                ),
                                                                Individual::Named(NamedIndividual(
                                                                    subject_iri.clone(),
                                                                )),
                                                                Individual::Named(NamedIndividual(
                                                                    obj_iri,
                                                                )),
                                                            ),
                                                        ),
                                                        Some(col_str),
                                                    ));
                                                }
                                                Err(ResolveError::Budget) => return Ok(()),
                                                Err(ResolveError::Invalid(msg)) => {
                                                    row_has_error = true;
                                                    emitter.diag(RowDiagnostic {
                                                        row: row.line,
                                                        column: Some(col.col_name.clone()),
                                                        severity: Severity::Error,
                                                        code: "invalid-iri".to_string(),
                                                        message: msg,
                                                    });
                                                }
                                            }
                                        }
                                    }
                                    Err(ResolveError::Budget) => return Ok(()),
                                    Err(ResolveError::Invalid(msg)) => {
                                        row_has_error = true;
                                        emitter.diag(RowDiagnostic {
                                            row: row.line,
                                            column: Some(col.col_name.clone()),
                                            severity: Severity::Error,
                                            code: "invalid-iri".to_string(),
                                            message: msg,
                                        });
                                    }
                                }
                            }
                        }
                        DirectiveKind::Annotation { property, format } => {
                            match resolve_iri_charged(
                                property,
                                &ctx,
                                emitter,
                                row.line,
                                Some(col_str),
                            ) {
                                Ok(prop_iri) => match build_annotation_value_charged(
                                    val, format, &ctx, emitter, row.line, col_str,
                                ) {
                                    Ok(ann_val) => {
                                        if !emitter.charge_expanded_bytes(
                                            subj_len,
                                            row.line,
                                            Some(col_str),
                                        ) {
                                            return Ok(());
                                        }
                                        row_axioms.push((
                                            Axiom::new(AxiomKind::AnnotationAssertion(
                                                AnnotationProperty(prop_iri),
                                                AnnotationSubject::Iri(subject_iri.clone()),
                                                ann_val,
                                            )),
                                            Some(col_str),
                                        ));
                                    }
                                    Err(ResolveError::Budget) => return Ok(()),
                                    Err(ResolveError::Invalid(msg)) => {
                                        row_has_error = true;
                                        emitter.diag(RowDiagnostic {
                                            row: row.line,
                                            column: Some(col.col_name.clone()),
                                            severity: Severity::Error,
                                            code: "invalid-annotation-value".to_string(),
                                            message: msg,
                                        });
                                    }
                                },
                                Err(ResolveError::Budget) => return Ok(()),
                                Err(ResolveError::Invalid(msg)) => {
                                    row_has_error = true;
                                    emitter.diag(RowDiagnostic {
                                        row: row.line,
                                        column: Some(col.col_name.clone()),
                                        severity: Severity::Error,
                                        code: "invalid-iri".to_string(),
                                        message: msg,
                                    });
                                }
                            }
                        }
                        DirectiveKind::Ignore
                        | DirectiveKind::Id
                        | DirectiveKind::EquivalentClasses(_)
                        | DirectiveKind::AxiomAnnotation { .. } => {}
                    }
                }
                last_col_target = if row_axioms.len() > start_idx {
                    LastColTarget::Axioms(start_idx..row_axioms.len())
                } else {
                    LastColTarget::None
                };
            }

            // Combine all `EC` expressions on this row into one EquivalentClasses axiom
            // (using ObjectIntersectionOf when two or more distinct expressions are present).
            if !ec_exprs.is_empty() {
                if !emitter.charge_expanded_bytes(subj_len, row.line, ec_col_name) {
                    return Ok(());
                }
                let unique: BTreeSet<ClassExpression> = ec_exprs.into_iter().collect();
                let rhs = if unique.len() == 1 {
                    unique.into_iter().next().unwrap()
                } else {
                    ClassExpression::ObjectIntersectionOf(unique)
                };
                let mut ax = Axiom::equivalent_classes([
                    ClassExpression::Class(Class(subject_iri.clone())),
                    rhs,
                ]);
                ax.annotations = ec_annotations;
                row_axioms.push((ax, ec_col_name));
            }

            if row_has_error {
                round_failed[row_i] = true;
                emitter.mark_row_error(row.line);
                continue;
            }
            match emitter.commit_row(row.line, row_declarations, row_axioms) {
                RowCommit::Committed => {}
                RowCommit::Invalid => {
                    round_failed[row_i] = true;
                }
                RowCommit::BudgetExceeded => return Ok(()),
            }
        }

        match convergence.advance(round, &round_contributed, &round_failed, &row_pass1_ok) {
            ConvergenceStep::Done => return Ok(()),
            ConvergenceStep::Continue => {}
            ConvergenceStep::LimitExceeded => {
                emitter.budget_error(
                    1,
                    None,
                    "vocab-dependency-limit",
                    format!(
                        "failed-row vocabulary dependency chain did not converge within {MAX_VOCAB_ROUNDS} passes"
                    ),
                );
                return Ok(());
            }
        }
    }
    Ok(())
}

/// Expand `%` placeholders in `template` with `value`, checking per-template and
/// request-wide expanded byte budgets BEFORE allocating the expanded string.
fn expand_percent_bounded(
    template: &str,
    value: &str,
    max_per_template: usize,
    emitter: &mut Emitter,
    row: usize,
    col_name: &str,
    row_has_error: &mut bool,
) -> Option<String> {
    let pct_count = template.bytes().filter(|&b| b == b'%').count();
    let expected_len = if pct_count > 0 {
        template
            .len()
            .saturating_sub(pct_count)
            .saturating_add(pct_count.saturating_mul(value.len()))
    } else {
        template.len().saturating_add(1).saturating_add(value.len())
    };
    if expected_len > max_per_template {
        *row_has_error = true;
        emitter.diag(RowDiagnostic {
            row,
            column: Some(bounded_col(col_name)),
            severity: Severity::Error,
            code: "expansion-bytes-limit".to_string(),
            message: format!(
                "expanded directive text ({expected_len} bytes) exceeds the {max_per_template} byte limit"
            ),
        });
        return None;
    }
    if !emitter.charge_expanded_bytes(expected_len, row, Some(col_name)) {
        return None;
    }
    if pct_count > 0 {
        Some(template.replace('%', value))
    } else {
        Some(format!("{template} {value}"))
    }
}

/// Lazily split `cell` by `sep`, capping at `max_values` and reporting truncation.
fn split_cell_bounded<'a>(
    cell: &'a str,
    sep: Option<&str>,
    max_values: usize,
) -> (Vec<&'a str>, bool) {
    match sep {
        Some(s) if !s.is_empty() => {
            let mut out = Vec::new();
            for v in cell.split(s).map(str::trim).filter(|v| !v.is_empty()) {
                if out.len() >= max_values {
                    return (out, true);
                }
                out.push(v);
            }
            (out, false)
        }
        _ => (vec![cell], false),
    }
}

fn parse_builtin_entity_kind(val: &str, ctx: &Context) -> Option<EntityKind> {
    let trimmed = val.trim();
    let lower = trimmed.to_ascii_lowercase();
    match lower.as_str() {
        "class" | "owl:class" => return Some(EntityKind::Class),
        "objectproperty" | "object property" | "owl:objectproperty" => {
            return Some(EntityKind::ObjectProperty);
        }
        "dataproperty" | "datatypeproperty" | "data property" | "owl:datatypeproperty" => {
            return Some(EntityKind::DataProperty);
        }
        "annotationproperty" | "annotation property" | "owl:annotationproperty" => {
            return Some(EntityKind::AnnotationProperty);
        }
        "individual" | "namedindividual" | "named individual" | "owl:namedindividual" => {
            return Some(EntityKind::NamedIndividual);
        }
        "datatype" | "rdfs:datatype" => return Some(EntityKind::Datatype),
        _ => {}
    }
    let bare = trimmed
        .strip_prefix('<')
        .and_then(|s| s.strip_suffix('>'))
        .unwrap_or(trimmed);
    let match_iri = |iri: &str| match iri {
        vocab::OWL_CLASS | vocab::RDFS_CLASS => Some(EntityKind::Class),
        vocab::OWL_OBJECT_PROPERTY => Some(EntityKind::ObjectProperty),
        vocab::OWL_DATATYPE_PROPERTY => Some(EntityKind::DataProperty),
        vocab::OWL_ANNOTATION_PROPERTY => Some(EntityKind::AnnotationProperty),
        vocab::OWL_NAMED_INDIVIDUAL => Some(EntityKind::NamedIndividual),
        vocab::RDFS_DATATYPE => Some(EntityKind::Datatype),
        _ => None,
    };
    if let Some(k) = match_iri(bare) {
        return Some(k);
    }
    if let Some((prefix, local)) = trimmed.split_once(':')
        && let Some((_, ns)) = ctx.prefixes.iter().find(|(p, _)| p == prefix)
    {
        if ns == vocab::OWL {
            return match local {
                "Class" => Some(EntityKind::Class),
                "ObjectProperty" => Some(EntityKind::ObjectProperty),
                "DatatypeProperty" => Some(EntityKind::DataProperty),
                "AnnotationProperty" => Some(EntityKind::AnnotationProperty),
                "NamedIndividual" => Some(EntityKind::NamedIndividual),
                _ => None,
            };
        }
        if ns == vocab::RDFS {
            return match local {
                "Class" => Some(EntityKind::Class),
                "Datatype" => Some(EntityKind::Datatype),
                _ => None,
            };
        }
    }
    None
}

fn make_entity(iri: Iri, kind: EntityKind) -> Entity {
    match kind {
        EntityKind::Class => Entity::Class(Class(iri)),
        EntityKind::Datatype => Entity::Datatype(Datatype(iri)),
        EntityKind::ObjectProperty => Entity::ObjectProperty(ObjectProperty(iri)),
        EntityKind::DataProperty => Entity::DataProperty(DataProperty(iri)),
        EntityKind::AnnotationProperty => Entity::AnnotationProperty(AnnotationProperty(iri)),
        EntityKind::NamedIndividual => Entity::NamedIndividual(NamedIndividual(iri)),
    }
}

fn build_annotation_value_charged(
    val: &str,
    format: &AnnotationFormat,
    ctx: &Context,
    emitter: &mut Emitter,
    row: usize,
    col_name: &str,
) -> Result<AnnotationValue, ResolveError> {
    match format {
        AnnotationFormat::Auto => {
            if val.starts_with('<') && val.ends_with('>') && val.len() > 2 {
                let iri = resolve_iri_charged(val, ctx, emitter, row, Some(col_name))?;
                Ok(AnnotationValue::Iri(iri))
            } else {
                if !emitter.charge_expanded_bytes(val.len(), row, Some(col_name)) {
                    return Err(ResolveError::Budget);
                }
                Ok(AnnotationValue::Literal(Literal::simple(val)))
            }
        }
        AnnotationFormat::Lang(lang) => {
            if !emitter.charge_expanded_bytes(
                val.len().saturating_add(lang.len()),
                row,
                Some(col_name),
            ) {
                return Err(ResolveError::Budget);
            }
            let lit = Literal::lang(val, lang).map_err(|e| ResolveError::Invalid(e.0))?;
            Ok(AnnotationValue::Literal(lit))
        }
        AnnotationFormat::Typed(dt) => {
            let dt_iri = resolve_iri_charged(dt, ctx, emitter, row, Some(col_name))?;
            if !emitter.charge_expanded_bytes(val.len(), row, Some(col_name)) {
                return Err(ResolveError::Budget);
            }
            Ok(AnnotationValue::Literal(Literal::typed(
                val,
                dt_iri.as_str(),
            )))
        }
        AnnotationFormat::Iri => {
            let iri = resolve_iri_charged(val, ctx, emitter, row, Some(col_name))?;
            Ok(AnnotationValue::Iri(iri))
        }
    }
}
