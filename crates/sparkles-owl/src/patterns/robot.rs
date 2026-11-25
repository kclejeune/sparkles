//! ROBOT-template-style column directives (`ID`, `LABEL`, `TYPE`, `SC %`, `EC %`, `DC %`,
//! `SP %`, `C %`, `I %`, `I <prop>`, `A <prop>`, `AL <prop>@lang`, `AT <prop>^^<dt>`,
//! `AI <prop>`, `SPLIT=<sep>`, and axiom annotations `>A`, `>AL`, `>AT`, `>AI`).

use super::{
    Context, Emitter, Limits, RowDiagnostic, Severity, Table, TableRow, parse_class_expr_fallback,
    resolve_iri,
};
use crate::model::*;
use crate::vocab;

/// Explicit mapping from a column name (or 0-based index) to a ROBOT directive string.
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
    // Look for ` SPLIT=` (whitespace-delimited token).
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
        "I" | "CI" | "TI" => {
            let tmpl = if arg.is_empty() { "%" } else { arg };
            Ok((DirectiveKind::Individual(tmpl.to_string()), split))
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

fn strip_prefix_ci<'a>(s: &'a str, prefix: &str) -> Option<&'a str> {
    if s.len() >= prefix.len() && s[..prefix.len()].eq_ignore_ascii_case(prefix) {
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

type TemplateCell = (usize, String, Option<String>);

/// Resolve the parsed column directives and data rows for ROBOT mode.
pub(crate) fn evaluate_robot(
    table: &Table,
    header_template_row: bool,
    explicit_columns: &[ColumnDirective],
    ctx: &mut Context,
    limits: &Limits,
    emitter: &mut Emitter,
) {
    let (template_cells, data_rows): (Vec<TemplateCell>, &[TableRow]) =
        if !explicit_columns.is_empty() {
            let mut cols = Vec::new();
            for cd in explicit_columns {
                if let Some(idx) = table.headers.iter().position(|h| h == &cd.column) {
                    cols.push((idx, cd.directive.clone(), cd.split.clone()));
                } else {
                    emitter.diag(RowDiagnostic {
                        row: 1,
                        column: Some(cd.column.clone()),
                        severity: Severity::Error,
                        code: "unknown-column".to_string(),
                        message: format!(
                            "template column '{}' does not match any header in the table",
                            cd.column
                        ),
                    });
                }
            }
            let rows = if header_template_row && !table.rows.is_empty() {
                &table.rows[1..]
            } else {
                &table.rows[..]
            };
            (cols, rows)
        } else if header_template_row {
            let Some((first, rest)) = table.rows.split_first() else {
                emitter.diag(RowDiagnostic {
                    row: 1,
                    column: None,
                    severity: Severity::Error,
                    code: "missing-template-row".to_string(),
                    message: "headerTemplateRow is enabled, but the table has no template row"
                        .to_string(),
                });
                return;
            };
            let cols = first
                .cells
                .iter()
                .enumerate()
                .map(|(i, c)| (i, c.clone(), None))
                .collect();
            (cols, rest)
        } else {
            // Treat header names themselves as directives if no explicit columns and no template row.
            let cols = table
                .headers
                .iter()
                .enumerate()
                .map(|(i, h)| (i, h.clone(), None))
                .collect();
            (cols, &table.rows[..])
        };

    if data_rows.len() > limits.max_rows {
        emitter.diag(RowDiagnostic {
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
        return;
    }

    let mut parsed_cols = Vec::with_capacity(template_cells.len());
    let mut id_col: Option<usize> = None;
    let mut type_cols: Vec<usize> = Vec::new();
    let mut label_cols: Vec<(usize, Option<String>)> = Vec::new();

    for (idx, dir_str, explicit_split) in template_cells {
        let col_name = table
            .headers
            .get(idx)
            .cloned()
            .unwrap_or_else(|| format!("column_{}", idx + 1));
        match parse_directive(&dir_str) {
            Ok((kind, inline_split)) => {
                let split = explicit_split.or(inline_split);
                if matches!(kind, DirectiveKind::Id) && id_col.is_none() {
                    id_col = Some(idx);
                }
                if matches!(kind, DirectiveKind::Type) {
                    type_cols.push(idx);
                }
                if let DirectiveKind::Label { ref lang } = kind {
                    label_cols.push((idx, lang.clone()));
                }
                parsed_cols.push(ParsedColumn {
                    col_index: idx,
                    col_name,
                    kind,
                    split,
                });
            }
            Err(msg) => {
                emitter.diag(RowDiagnostic {
                    row: if header_template_row && explicit_columns.is_empty() {
                        table.rows.first().map_or(2, |r| r.line)
                    } else {
                        1
                    },
                    column: Some(col_name),
                    severity: Severity::Error,
                    code: "invalid-directive".to_string(),
                    message: msg,
                });
            }
        }
    }

    let Some(id_idx) = id_col else {
        emitter.diag(RowDiagnostic {
            row: 1,
            column: None,
            severity: Severity::Error,
            code: "missing-id-column".to_string(),
            message: "ROBOT template requires an ID column directive".to_string(),
        });
        return;
    };

    // Pass 1: register entities and labels introduced by rows in the working vocabulary
    // so Manchester expressions on any row can reference classes/properties/labels from
    // other rows in the same spreadsheet.
    let mut row_subjects: Vec<Option<(Iri, EntityKind, Vec<Iri>)>> =
        Vec::with_capacity(data_rows.len());
    for row in data_rows {
        let raw_id = row.cells.get(id_idx).map(|s| s.trim()).unwrap_or("");
        if raw_id.is_empty() {
            row_subjects.push(None);
            continue;
        }
        let Ok(subject_iri) = resolve_iri(raw_id, ctx) else {
            row_subjects.push(None);
            continue;
        };

        let mut kind = EntityKind::Class;
        let mut class_types: Vec<Iri> = Vec::new();
        for &t_idx in &type_cols {
            let cell = row.cells.get(t_idx).map(|s| s.trim()).unwrap_or("");
            if cell.is_empty() {
                continue;
            }
            if let Some(k) = parse_builtin_entity_kind(cell, ctx) {
                kind = k;
            } else if let Ok(cls_iri) = resolve_iri(cell, ctx) {
                kind = EntityKind::NamedIndividual;
                class_types.push(cls_iri);
            }
        }
        ctx.register_entity(&subject_iri, kind);
        for cls in &class_types {
            ctx.register_entity(cls, EntityKind::Class);
        }
        for &(l_idx, _) in &label_cols {
            let val = row.cells.get(l_idx).map(|s| s.trim()).unwrap_or("");
            if !val.is_empty() {
                ctx.vocab.add_label(subject_iri.as_str(), val);
            }
        }
        row_subjects.push(Some((subject_iri, kind, class_types)));
    }

    // Pass 2: emit axioms and diagnostics per row.
    for (row, subject_info) in data_rows.iter().zip(row_subjects) {
        let raw_id = row.cells.get(id_idx).map(|s| s.trim()).unwrap_or("");
        if raw_id.is_empty() {
            // If all cells are empty or ID is empty, skip row (warn if other cells non-empty).
            let any_data = row.cells.iter().any(|c| !c.trim().is_empty());
            if any_data {
                emitter.mark_row_error(row.line);
                emitter.diag(RowDiagnostic {
                    row: row.line,
                    column: table.headers.get(id_idx).cloned(),
                    severity: Severity::Error,
                    code: "missing-id".to_string(),
                    message: "row has values but its ID cell is empty".to_string(),
                });
            } else {
                emitter.rows_skipped += 1;
            }
            continue;
        }

        let Some((subject_iri, entity_kind, _)) = subject_info else {
            emitter.mark_row_error(row.line);
            emitter.diag(RowDiagnostic {
                row: row.line,
                column: table.headers.get(id_idx).cloned(),
                severity: Severity::Error,
                code: "invalid-iri".to_string(),
                message: format!("cannot resolve ID '{raw_id}' to a valid IRI"),
            });
            continue;
        };

        emitter.rows_processed += 1;

        // Emit entity declaration for the row subject.
        let entity = make_entity(subject_iri.clone(), entity_kind);
        emitter.declare(entity.clone());
        let mut row_axioms: Vec<Axiom> = vec![Axiom::declaration(entity)];
        // Track indices in `row_axioms` produced by the immediately preceding directive column
        // so `>A`/`>AL`/`>AT`/`>AI` columns can annotate them.
        let mut last_col_range: std::ops::Range<usize> = 0..0;

        for col in &parsed_cols {
            if matches!(col.kind, DirectiveKind::Ignore | DirectiveKind::Id) {
                continue;
            }
            let raw_cell = row.cells.get(col.col_index).map(|s| s.trim()).unwrap_or("");
            if raw_cell.is_empty() {
                if !matches!(col.kind, DirectiveKind::AxiomAnnotation { .. }) {
                    last_col_range = row_axioms.len()..row_axioms.len();
                }
                continue;
            }

            let values = split_cell(raw_cell, col.split.as_deref());
            if let DirectiveKind::AxiomAnnotation { property, format } = &col.kind {
                if last_col_range.is_empty() {
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
                match resolve_iri(property, ctx) {
                    Ok(prop_iri) => {
                        for val in values {
                            match build_annotation_value(val, format, ctx) {
                                Ok(ann_val) => {
                                    let ann = Annotation {
                                        property: AnnotationProperty(prop_iri.clone()),
                                        value: ann_val,
                                        annotations: Default::default(),
                                    };
                                    for ax in &mut row_axioms[last_col_range.clone()] {
                                        ax.annotations.insert(ann.clone());
                                    }
                                }
                                Err(msg) => {
                                    emitter.mark_row_error(row.line);
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
                    }
                    Err(msg) => {
                        emitter.mark_row_error(row.line);
                        emitter.diag(RowDiagnostic {
                            row: row.line,
                            column: Some(col.col_name.clone()),
                            severity: Severity::Error,
                            code: "invalid-iri".to_string(),
                            message: msg,
                        });
                    }
                }
                continue;
            }

            let start_idx = row_axioms.len();
            for val in values {
                match &col.kind {
                    DirectiveKind::Label { lang } => {
                        let lit = match lang {
                            Some(l) => match Literal::lang(val, l) {
                                Ok(lit) => lit,
                                Err(e) => {
                                    emitter.mark_row_error(row.line);
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
                        row_axioms.push(Axiom::new(AxiomKind::AnnotationAssertion(
                            AnnotationProperty::new(vocab::RDFS_LABEL),
                            AnnotationSubject::Iri(subject_iri.clone()),
                            AnnotationValue::Literal(lit),
                        )));
                    }
                    DirectiveKind::Type => {
                        if parse_builtin_entity_kind(val, ctx).is_some() {
                            // Already handled in declaration.
                        } else {
                            match parse_class_expr_fallback(val, ctx) {
                                Ok(ce) => {
                                    row_axioms.push(Axiom::class_assertion(
                                        ce,
                                        Individual::Named(NamedIndividual(subject_iri.clone())),
                                    ));
                                }
                                Err(msg) => {
                                    emitter.mark_row_error(row.line);
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
                        let expanded = expand_percent(tmpl, val);
                        match parse_class_expr_fallback(&expanded, ctx) {
                            Ok(sup) => {
                                row_axioms
                                    .push(Axiom::sub_class_of(Class(subject_iri.clone()), sup));
                            }
                            Err(msg) => {
                                emitter.mark_row_error(row.line);
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
                    DirectiveKind::EquivalentClasses(tmpl) => {
                        let expanded = expand_percent(tmpl, val);
                        match parse_class_expr_fallback(&expanded, ctx) {
                            Ok(other) => {
                                row_axioms.push(Axiom::equivalent_classes([
                                    ClassExpression::Class(Class(subject_iri.clone())),
                                    other,
                                ]));
                            }
                            Err(msg) => {
                                emitter.mark_row_error(row.line);
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
                        let expanded = expand_percent(tmpl, val);
                        match parse_class_expr_fallback(&expanded, ctx) {
                            Ok(other) => {
                                row_axioms.push(Axiom::disjoint_classes([
                                    ClassExpression::Class(Class(subject_iri.clone())),
                                    other,
                                ]));
                            }
                            Err(msg) => {
                                emitter.mark_row_error(row.line);
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
                        let expanded = expand_percent(tmpl, val);
                        match resolve_iri(&expanded, ctx) {
                            Ok(sup_iri) => {
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
                                        ObjectPropertyExpression::Property(ObjectProperty(sup_iri)),
                                    )),
                                };
                                row_axioms.push(ax);
                            }
                            Err(msg) => {
                                emitter.mark_row_error(row.line);
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
                        let expanded = expand_percent(tmpl, val);
                        match parse_class_expr_fallback(&expanded, ctx) {
                            Ok(ce) => {
                                if entity_kind == EntityKind::NamedIndividual {
                                    row_axioms.push(Axiom::class_assertion(
                                        ce,
                                        Individual::Named(NamedIndividual(subject_iri.clone())),
                                    ));
                                } else {
                                    row_axioms
                                        .push(Axiom::sub_class_of(Class(subject_iri.clone()), ce));
                                }
                            }
                            Err(msg) => {
                                emitter.mark_row_error(row.line);
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
                            // `I %`: cell names an individual of the row's class (or if the row
                            // is already an individual, same-as / named individual declaration).
                            match resolve_iri(val, ctx) {
                                Ok(ind_iri) => {
                                    let ind = NamedIndividual(ind_iri);
                                    emitter.declare(Entity::NamedIndividual(ind.clone()));
                                    row_axioms.push(Axiom::declaration(Entity::NamedIndividual(
                                        ind.clone(),
                                    )));
                                    if entity_kind == EntityKind::Class {
                                        row_axioms.push(Axiom::class_assertion(
                                            Class(subject_iri.clone()),
                                            Individual::Named(ind),
                                        ));
                                    }
                                }
                                Err(msg) => {
                                    emitter.mark_row_error(row.line);
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
                            match resolve_iri(tmpl, ctx) {
                                Ok(prop_iri) => {
                                    let is_data_prop = ctx
                                        .vocab
                                        .kinds(prop_iri.as_str())
                                        .contains(&EntityKind::DataProperty);
                                    if is_data_prop {
                                        let lit = parse_cell_literal(val, ctx);
                                        row_axioms.push(Axiom::new(
                                            AxiomKind::DataPropertyAssertion(
                                                DataProperty(prop_iri),
                                                Individual::Named(NamedIndividual(
                                                    subject_iri.clone(),
                                                )),
                                                lit,
                                            ),
                                        ));
                                    } else {
                                        match resolve_iri(val, ctx) {
                                            Ok(obj_iri) => {
                                                row_axioms.push(Axiom::new(
                                                    AxiomKind::ObjectPropertyAssertion(
                                                        ObjectPropertyExpression::Property(
                                                            ObjectProperty(prop_iri),
                                                        ),
                                                        Individual::Named(NamedIndividual(
                                                            subject_iri.clone(),
                                                        )),
                                                        Individual::Named(NamedIndividual(obj_iri)),
                                                    ),
                                                ));
                                            }
                                            Err(msg) => {
                                                emitter.mark_row_error(row.line);
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
                                Err(msg) => {
                                    emitter.mark_row_error(row.line);
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
                        match resolve_iri(property, ctx) {
                            Ok(prop_iri) => match build_annotation_value(val, format, ctx) {
                                Ok(ann_val) => {
                                    row_axioms.push(Axiom::new(AxiomKind::AnnotationAssertion(
                                        AnnotationProperty(prop_iri),
                                        AnnotationSubject::Iri(subject_iri.clone()),
                                        ann_val,
                                    )));
                                }
                                Err(msg) => {
                                    emitter.mark_row_error(row.line);
                                    emitter.diag(RowDiagnostic {
                                        row: row.line,
                                        column: Some(col.col_name.clone()),
                                        severity: Severity::Error,
                                        code: "invalid-annotation-value".to_string(),
                                        message: msg,
                                    });
                                }
                            },
                            Err(msg) => {
                                emitter.mark_row_error(row.line);
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
                    | DirectiveKind::AxiomAnnotation { .. } => {}
                }
            }
            last_col_range = start_idx..row_axioms.len();
        }

        for ax in row_axioms {
            if !emitter.emit(ax, row.line, None) {
                return;
            }
        }
    }
}

fn expand_percent(template: &str, value: &str) -> String {
    if template.contains('%') {
        template.replace('%', value)
    } else {
        format!("{template} {value}")
    }
}

fn split_cell<'a>(cell: &'a str, sep: Option<&str>) -> Vec<&'a str> {
    match sep {
        Some(s) if !s.is_empty() => cell
            .split(s)
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .collect(),
        _ => vec![cell],
    }
}

fn parse_builtin_entity_kind(val: &str, ctx: &Context) -> Option<EntityKind> {
    let lower = val.trim().to_ascii_lowercase();
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
    if let Ok(iri) = resolve_iri(val, ctx) {
        match iri.as_str() {
            vocab::OWL_CLASS | vocab::RDFS_CLASS => Some(EntityKind::Class),
            vocab::OWL_OBJECT_PROPERTY => Some(EntityKind::ObjectProperty),
            vocab::OWL_DATATYPE_PROPERTY => Some(EntityKind::DataProperty),
            vocab::OWL_ANNOTATION_PROPERTY => Some(EntityKind::AnnotationProperty),
            vocab::OWL_NAMED_INDIVIDUAL => Some(EntityKind::NamedIndividual),
            vocab::RDFS_DATATYPE => Some(EntityKind::Datatype),
            _ => None,
        }
    } else {
        None
    }
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

fn build_annotation_value(
    val: &str,
    format: &AnnotationFormat,
    ctx: &Context,
) -> Result<AnnotationValue, String> {
    match format {
        AnnotationFormat::Auto => {
            if val.starts_with('<') && val.ends_with('>') && val.len() > 2 {
                let iri = resolve_iri(val, ctx)?;
                Ok(AnnotationValue::Iri(iri))
            } else {
                Ok(AnnotationValue::Literal(Literal::simple(val)))
            }
        }
        AnnotationFormat::Lang(lang) => {
            let lit = Literal::lang(val, lang).map_err(|e| e.0)?;
            Ok(AnnotationValue::Literal(lit))
        }
        AnnotationFormat::Typed(dt) => {
            let dt_iri = resolve_iri(dt, ctx)?;
            Ok(AnnotationValue::Literal(Literal::typed(
                val,
                dt_iri.as_str(),
            )))
        }
        AnnotationFormat::Iri => {
            let iri = resolve_iri(val, ctx)?;
            Ok(AnnotationValue::Iri(iri))
        }
    }
}

fn parse_cell_literal(val: &str, ctx: &Context) -> Literal {
    if let Ok(lit) = crate::manchester::literal(val, &ctx.vocab) {
        return lit;
    }
    Literal::simple(val)
}
