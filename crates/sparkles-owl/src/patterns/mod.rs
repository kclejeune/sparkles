//! Pattern-based OWL 2 axiom generation from spreadsheet tables (CSV/TSV).
//!
//! Supports two complementary specification modes in the manner of ROBOT template,
//! DOSDP and Cellfie:
//!
//! - [`TemplateMode::Robot`]: column directives (`ID`, `LABEL`, `TYPE`, `SC %`, `EC %`,
//!   `DC %`, `SP %`, `C %`, `I %`, `I <prop>`, `TI %`, `A <prop>`, `AL <prop>@lang`,
//!   `AT <prop>^^<dt>`, `AI <prop>`, `SPLIT=<sep>`, and axiom annotations `>A`/`>AL`/
//!   `>AT`/`>AI`), read from row 2 of the spreadsheet, from explicit column directive
//!   mappings, or merged with explicit overrides on top of row 2.
//! - [`TemplateMode::Pattern`]: parameterized axiom templates in Manchester syntax or
//!   functional-style syntax with `${col}` and `${col|filter}` placeholders, optional
//!   entity declarations, multi-valued cell splitting, and axiom annotations.
//!
//! Evaluation runs in two passes so entities and labels declared by one row are visible
//! to Manchester expressions on other rows in the same table. Output axioms are staged
//! per row, validated with [`crate::validate::axiom`], and deduplicated deterministically
//! with 1-based source line provenance. Any row with an error discards all staged
//! declarations and axioms from that row.

mod robot;
mod rules;
mod table;

pub use robot::ColumnDirective;
pub use rules::{ColumnDeclaration, PatternRule, PatternSyntax, RuleAnnotation};
pub use table::{Delimiter, Table, TableError, TableRow, parse_table};

use crate::manchester::{self, Vocabulary};
use crate::model::*;
use crate::write;
use oxrdf::Triple;
use rustc_hash::{FxHashMap, FxHashSet, FxHasher};
use std::collections::{BTreeMap, BTreeSet};
use std::hash::{Hash, Hasher};

/// Common vocabulary prefixes available as fallbacks when neither the dataset nor the
/// pattern specification binds them.
const DEFAULT_PATTERN_PREFIXES: [(&str, &str); 6] = [
    ("skos", "http://www.w3.org/2004/02/skos/core#"),
    ("dc", "http://purl.org/dc/elements/1.1/"),
    ("dcterms", "http://purl.org/dc/terms/"),
    ("foaf", "http://xmlns.com/foaf/0.1/"),
    ("schema", "https://schema.org/"),
    ("obo", "http://purl.obolibrary.org/obo/"),
];

/// Maximum byte length of a diagnostic column or rule label.
pub(crate) const MAX_DIAGNOSTIC_COLUMN_BYTES: usize = 256;

/// Maximum vocabulary-exclusion convergence rounds before refusing a deep dependency chain.
pub(crate) const MAX_VOCAB_ROUNDS: usize = 16;

/// Non-hierarchical URI schemes accepted without angle brackets when not bound as prefixes.
const ALLOWED_NON_HIERARCHICAL_SCHEMES: &[&str] = &[
    "urn", "mailto", "tag", "doi", "info", "data", "did", "uuid", "ni", "geo", "tel", "ark", "hdl",
    "isbn", "issn",
];

/// Cooperative stop reason returned by cancellation / deadline hooks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CancelReason {
    Cancelled,
    Timeout,
}

/// Error returned when parsing or evaluating a table stops before producing a result.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EvalError {
    Table(TableError),
    Cancelled,
    Timeout,
}

impl std::fmt::Display for EvalError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Table(e) => e.fmt(f),
            Self::Cancelled => f.write_str("pattern evaluation cancelled"),
            Self::Timeout => f.write_str("pattern evaluation timed out"),
        }
    }
}

impl std::error::Error for EvalError {}

impl From<TableError> for EvalError {
    fn from(e: TableError) -> Self {
        Self::Table(e)
    }
}

impl From<CancelReason> for EvalError {
    fn from(r: CancelReason) -> Self {
        match r {
            CancelReason::Cancelled => Self::Cancelled,
            CancelReason::Timeout => Self::Timeout,
        }
    }
}

/// Resource bounds enforced while parsing and evaluating spreadsheet patterns.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    /// Maximum size of raw CSV/TSV input in bytes (default: 2 MiB).
    pub max_input_bytes: usize,
    /// Maximum number of data rows (default: 5,000).
    pub max_rows: usize,
    /// Maximum number of columns (default: 128).
    pub max_cols: usize,
    /// Maximum number of unique generated axioms (default: 50,000).
    pub max_axioms: usize,
    /// Maximum number of recorded row diagnostics (default: 1,000).
    pub max_diagnostics: usize,
    /// Maximum Cartesian expansion combinations per rule per row (default: 256).
    pub max_rule_expansion_per_row: usize,
    /// Maximum number of rules in a pattern spec (default: 128).
    pub max_rules: usize,
    /// Maximum number of declarations in a pattern spec (default: 128).
    pub max_declarations: usize,
    /// Maximum number of explicit column directives in a ROBOT spec (default: 256).
    pub max_columns_spec: usize,
    /// Maximum number of annotations per rule spec (default: 64).
    pub max_annotations_per_rule: usize,
    /// Maximum number of extra prefix bindings in a spec (default: 256).
    pub max_prefixes: usize,
    /// Maximum number of split tokens extracted from one cell (default: 256).
    pub max_split_values_per_cell: usize,
    /// Maximum number of axiom annotations attached to one axiom (default: 64).
    pub max_annotations_per_axiom: usize,
    /// Maximum expanded template text length in bytes per evaluation (default: 64 KiB).
    pub max_expanded_bytes_per_template: usize,
    /// Maximum cumulative expanded text bytes across one evaluation (default: 32 MiB).
    pub max_total_expanded_bytes: usize,
    /// Maximum total axiom/annotation evaluation attempts across a request (default: 100,000).
    pub max_axiom_attempts: usize,
    /// Maximum projected RDF triples (default: 250,000).
    pub max_triples: usize,
    /// Maximum byte length of each recorded diagnostic message (default: 1,024).
    pub max_diagnostic_message_bytes: usize,
    /// Maximum vocabulary entries (entities + labels) added during Pass 1 (default: 100,000).
    pub max_vocab_entries_added: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_input_bytes: 2 * 1024 * 1024,
            max_rows: 5_000,
            max_cols: 128,
            max_axioms: 50_000,
            max_diagnostics: 1_000,
            max_rule_expansion_per_row: 256,
            max_rules: 128,
            max_declarations: 128,
            max_columns_spec: 256,
            max_annotations_per_rule: 64,
            max_prefixes: 256,
            max_split_values_per_cell: 256,
            max_annotations_per_axiom: 64,
            max_expanded_bytes_per_template: 64 * 1024,
            max_total_expanded_bytes: 32 * 1024 * 1024,
            max_axiom_attempts: 100_000,
            max_triples: 250_000,
            max_diagnostic_message_bytes: 1_024,
            max_vocab_entries_added: 100_000,
        }
    }
}

/// Severity of a [`RowDiagnostic`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Severity {
    Error,
    Warning,
}

impl Severity {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Error => "error",
            Self::Warning => "warning",
        }
    }
}

/// One row- or template-level validation diagnostic.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RowDiagnostic {
    /// 1-based source line number.
    pub row: usize,
    /// Column name or rule label, when applicable.
    pub column: Option<String>,
    pub severity: Severity,
    pub code: String,
    pub message: String,
}

/// One deduplicated axiom generated by a pattern run, with the source lines that
/// produced it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GeneratedAxiom {
    pub axiom: Axiom,
    /// 1-based source line numbers that generated this axiom (sorted, deduplicated).
    pub rows: Vec<usize>,
}

/// Specification mode of a [`PatternSpec`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TemplateMode {
    /// ROBOT-template column directives.
    Robot {
        /// When true, row 2 of the spreadsheet carries the directive strings.
        header_template_row: bool,
        /// Explicit column directives overriding or supplementing row 2.
        columns: Vec<ColumnDirective>,
    },
    /// DOSDP/Cellfie-style parameterized axiom rules.
    Pattern {
        rules: Vec<PatternRule>,
        declarations: Vec<ColumnDeclaration>,
    },
}

/// Full declarative specification for spreadsheet-to-axiom generation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PatternSpec {
    pub mode: TemplateMode,
    /// Optional base IRI used to resolve bare local names when no prefix matches.
    pub base_iri: Option<String>,
    /// Extra prefix bindings (`prefix -> namespace`) applied on top of dataset prefixes.
    pub prefixes: BTreeMap<String, String>,
}

/// Result of evaluating a spreadsheet against a [`PatternSpec`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PatternResult {
    pub delimiter: Delimiter,
    pub headers: Vec<String>,
    pub rows_processed: usize,
    pub rows_skipped: usize,
    pub rows_with_errors: usize,
    /// Total error-level diagnostics observed (even when `diagnostics` is truncated).
    pub error_count: usize,
    /// Total spec/structural error diagnostics observed.
    pub structural_error_count: usize,
    /// True when evaluation hit a request-wide budget limit.
    pub budget_exceeded: bool,
    /// Deduplicated generated axioms in deterministic first-seen order.
    pub axioms: Vec<GeneratedAxiom>,
    /// Distinct entities explicitly declared by the template.
    pub declarations: Vec<Entity>,
    pub diagnostics: Vec<RowDiagnostic>,
    pub diagnostics_truncated: bool,
}

impl PatternResult {
    /// Whether evaluation completed with zero error-level diagnostics and within budget.
    pub fn is_valid(&self) -> bool {
        self.error_count == 0 && self.rows_with_errors == 0 && !self.budget_exceeded
    }

    /// Total count of error-level diagnostics (including those omitted after truncation).
    pub fn error_count(&self) -> usize {
        self.error_count
    }

    /// Whether a request-wide budget failure or structural specification error occurred
    /// (these refuse writes even under `onError: "skip"`).
    pub fn has_fatal_error(&self) -> bool {
        self.structural_error_count > 0 || self.budget_exceeded
    }

    /// Deterministically map all generated axioms to RDF triples via the OWL 2 forward
    /// mapping (`sparkles_owl::write`), refusing before allocation if the output exceeds
    /// `max_triples`.
    pub fn try_triples(&self, max_triples: usize) -> Result<Vec<Triple>, String> {
        let mut total = 0usize;
        for item in &self.axioms {
            total = total.saturating_add(write::axiom_triple_count(&item.axiom));
            if total > max_triples {
                return Err(format!(
                    "projected RDF triples exceeded the limit of {max_triples}"
                ));
            }
        }
        let mut fresh = write::counter("pat");
        let mut out = Vec::with_capacity(total);
        for item in &self.axioms {
            out.extend(write::axiom_triples(&item.axiom, &mut fresh));
        }
        Ok(out)
    }

    /// Deterministically map all generated axioms to RDF triples via the OWL 2 forward
    /// mapping (`sparkles_owl::write`).
    pub fn triples(&self) -> Vec<Triple> {
        let mut fresh = write::counter("pat");
        let mut out = Vec::new();
        for item in &self.axioms {
            out.extend(write::axiom_triples(&item.axiom, &mut fresh));
        }
        out
    }
}

/// Internal evaluation context shared across ROBOT and Pattern evaluators.
pub(crate) struct Context {
    pub vocab: Vocabulary,
    pub prefixes: Vec<(String, String)>,
    pub base_iri: Option<String>,
}

impl Context {
    pub(crate) fn build(base_vocab: &Vocabulary, spec: &PatternSpec) -> Self {
        // Merge prefixes: spec prefixes take highest precedence, then base_vocab prefixes,
        // then standard/default pattern prefixes.
        let mut merged: BTreeMap<String, String> = BTreeMap::new();
        for (p, ns) in DEFAULT_PATTERN_PREFIXES {
            merged.insert(p.to_string(), ns.to_string());
        }
        for (p, ns) in base_vocab.prefixes() {
            merged.insert(p.clone(), ns.clone());
        }
        for (p, ns) in &spec.prefixes {
            let p_clean = normalize_prefix_key(p);
            if is_valid_prefix_name(p_clean) && !ns.trim().is_empty() {
                merged.insert(p_clean.to_string(), ns.trim().to_string());
            }
        }

        let mut vocab = base_vocab.clone();
        for (p, ns) in &merged {
            vocab.add_prefix(p.as_str(), ns.as_str());
        }

        let prefixes = merged.into_iter().collect();
        let base_iri = spec
            .base_iri
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(ToString::to_string);

        Self {
            vocab,
            prefixes,
            base_iri,
        }
    }

    pub(crate) fn register_entity(&mut self, iri: &Iri, kind: EntityKind) {
        let e = match kind {
            EntityKind::Class => Entity::Class(Class(iri.clone())),
            EntityKind::Datatype => Entity::Datatype(Datatype(iri.clone())),
            EntityKind::ObjectProperty => Entity::ObjectProperty(ObjectProperty(iri.clone())),
            EntityKind::DataProperty => Entity::DataProperty(DataProperty(iri.clone())),
            EntityKind::AnnotationProperty => {
                Entity::AnnotationProperty(AnnotationProperty(iri.clone()))
            }
            EntityKind::NamedIndividual => Entity::NamedIndividual(NamedIndividual(iri.clone())),
        };
        self.vocab.add_entity(e);
    }
}

fn hash_axiom(ax: &Axiom) -> u64 {
    let mut h = FxHasher::default();
    ax.hash(&mut h);
    h.finish()
}

/// Deterministic deduplicating accumulator for generated axioms and diagnostics.
pub(crate) struct Emitter {
    limits: Limits,
    axioms: Vec<GeneratedAxiom>,
    /// Hash-bucket index into `self.axioms`, avoiding cloning every retained [`Axiom`].
    index: FxHashMap<u64, Vec<usize>>,
    declarations: BTreeSet<Entity>,
    diagnostics: Vec<RowDiagnostic>,
    diagnostics_truncated: bool,
    error_rows: FxHashSet<usize>,
    total_errors: usize,
    structural_errors: usize,
    budget_exceeded: bool,
    attempts: usize,
    total_expanded_bytes: usize,
    vocab_entries_added: usize,
    limit_reported: bool,
    pub rows_processed: usize,
    pub rows_skipped: usize,
}

impl Emitter {
    fn new(limits: &Limits, initial_truncated: bool) -> Self {
        Self {
            limits: *limits,
            axioms: Vec::new(),
            index: FxHashMap::default(),
            declarations: BTreeSet::new(),
            diagnostics: Vec::new(),
            diagnostics_truncated: initial_truncated,
            error_rows: FxHashSet::default(),
            total_errors: 0,
            structural_errors: 0,
            budget_exceeded: false,
            attempts: 0,
            total_expanded_bytes: 0,
            vocab_entries_added: 0,
            limit_reported: false,
            rows_processed: 0,
            rows_skipped: 0,
        }
    }

    fn lookup_index(&self, ax: &Axiom, hash: u64) -> Option<usize> {
        self.index.get(&hash).and_then(|bucket| {
            bucket
                .iter()
                .copied()
                .find(|&pos| self.axioms[pos].axiom == *ax)
        })
    }

    /// Clear per-round outputs and diagnostics before a vocabulary-exclusion retry while
    /// keeping cumulative work, expanded-byte, and vocabulary budgets intact.
    pub(crate) fn reset_for_retry(
        &mut self,
        initial_diagnostics: &[RowDiagnostic],
        initial_truncated: bool,
    ) {
        self.axioms.clear();
        self.index.clear();
        self.declarations.clear();
        self.diagnostics.clear();
        self.diagnostics_truncated = initial_truncated;
        self.error_rows.clear();
        self.total_errors = 0;
        self.structural_errors = 0;
        self.rows_processed = 0;
        self.rows_skipped = 0;
        for d in initial_diagnostics {
            self.diag(d.clone());
        }
    }

    pub(crate) fn mark_row_error(&mut self, row: usize) {
        self.error_rows.insert(row);
    }

    pub(crate) fn diag(&mut self, mut diag: RowDiagnostic) {
        if diag.severity == Severity::Error {
            self.total_errors = self.total_errors.saturating_add(1);
        }
        if self.diagnostics.len() < self.limits.max_diagnostics {
            if let Some(col) = diag.column.as_mut() {
                truncate_utf8(col, MAX_DIAGNOSTIC_COLUMN_BYTES);
            }
            truncate_utf8(&mut diag.message, self.limits.max_diagnostic_message_bytes);
            self.diagnostics.push(diag);
        } else {
            self.diagnostics_truncated = true;
        }
    }

    pub(crate) fn structural_error(&mut self, diag: RowDiagnostic) {
        self.structural_errors = self.structural_errors.saturating_add(1);
        self.diag(diag);
    }

    pub(crate) fn record_omitted_structural_errors(&mut self, omitted: usize) {
        if omitted > 0 {
            self.structural_errors = self.structural_errors.saturating_add(omitted);
            self.total_errors = self.total_errors.saturating_add(omitted);
            self.diagnostics_truncated = true;
        }
    }

    pub(crate) fn budget_error(
        &mut self,
        row: usize,
        column: Option<&str>,
        code: &'static str,
        message: String,
    ) {
        self.budget_exceeded = true;
        if !self.limit_reported {
            self.limit_reported = true;
            self.diag(RowDiagnostic {
                row,
                column: bounded_opt_col(column),
                severity: Severity::Error,
                code: code.to_string(),
                message,
            });
        }
    }

    pub(crate) fn charge_attempt(
        &mut self,
        count: usize,
        row: usize,
        column: Option<&str>,
    ) -> bool {
        self.attempts = self.attempts.saturating_add(count);
        if self.attempts > self.limits.max_axiom_attempts {
            self.budget_error(
                row,
                column,
                "work-limit",
                format!(
                    "pattern evaluation work exceeded the limit of {} attempts",
                    self.limits.max_axiom_attempts
                ),
            );
            return false;
        }
        true
    }

    pub(crate) fn remaining_expanded_bytes(&self) -> usize {
        self.limits
            .max_total_expanded_bytes
            .saturating_sub(self.total_expanded_bytes)
    }

    pub(crate) fn charge_expanded_bytes(
        &mut self,
        bytes: usize,
        row: usize,
        column: Option<&str>,
    ) -> bool {
        self.total_expanded_bytes = self.total_expanded_bytes.saturating_add(bytes);
        if self.total_expanded_bytes > self.limits.max_total_expanded_bytes {
            self.budget_error(
                row,
                column,
                "expansion-bytes-limit",
                format!(
                    "expanded template text exceeded the {} byte request budget",
                    self.limits.max_total_expanded_bytes
                ),
            );
            return false;
        }
        true
    }

    pub(crate) fn charge_vocab_bytes(&mut self, row: usize, bytes: usize) -> bool {
        self.vocab_entries_added = self.vocab_entries_added.saturating_add(1);
        if self.vocab_entries_added > self.limits.max_vocab_entries_added {
            self.budget_error(
                row,
                None,
                "vocab-limit",
                format!(
                    "vocabulary additions exceeded the limit of {} entries",
                    self.limits.max_vocab_entries_added
                ),
            );
            return false;
        }
        self.charge_expanded_bytes(bytes, row, None)
    }

    pub(crate) fn remaining_axiom_capacity(&self) -> usize {
        self.limits.max_axioms.saturating_sub(self.axioms.len())
    }

    /// Validate all staged axioms for one row and commit declarations + axioms atomically.
    /// If any axiom fails `validate::axiom` or exceeds `max_annotations_per_axiom`, records
    /// the diagnostic(s), marks the row as erroneous, and discards every staged declaration
    /// and axiom from this row.
    pub(crate) fn commit_row(
        &mut self,
        row: usize,
        declarations: Vec<Entity>,
        axioms: Vec<(Axiom, Option<&str>)>,
    ) -> RowCommit {
        let mut any_invalid = false;
        for (ax, col) in &axioms {
            if count_annotations(&ax.annotations) > self.limits.max_annotations_per_axiom {
                any_invalid = true;
                self.mark_row_error(row);
                self.diag(RowDiagnostic {
                    row,
                    column: bounded_opt_col(*col),
                    severity: Severity::Error,
                    code: "annotation-limit".to_string(),
                    message: format!(
                        "axiom carries more than {} annotations",
                        self.limits.max_annotations_per_axiom
                    ),
                });
                continue;
            }
            if let Err(e) = crate::validate::axiom(ax) {
                any_invalid = true;
                self.mark_row_error(row);
                self.diag(RowDiagnostic {
                    row,
                    column: bounded_opt_col(*col),
                    severity: Severity::Error,
                    code: "invalid-axiom".to_string(),
                    message: e.0,
                });
            }
        }
        if any_invalid {
            return RowCommit::Invalid;
        }

        // Check whether adding the new unique axioms from this row would exceed `max_axioms`
        // before mutating state so a partially committed row never leaks on limit overflow.
        let mut new_unique: FxHashSet<&Axiom> = FxHashSet::default();
        for (ax, _) in &axioms {
            let h = hash_axiom(ax);
            if self.lookup_index(ax, h).is_none() {
                new_unique.insert(ax);
            }
        }
        if self.axioms.len().saturating_add(new_unique.len()) > self.limits.max_axioms {
            let col = axioms.first().and_then(|(_, c)| *c);
            self.budget_error(
                row,
                col,
                "axiom-limit",
                format!(
                    "generated axioms exceeded the limit of {}",
                    self.limits.max_axioms
                ),
            );
            return RowCommit::BudgetExceeded;
        }

        for entity in declarations {
            self.declarations.insert(entity);
        }
        for (axiom, _) in axioms {
            let h = hash_axiom(&axiom);
            if let Some(pos) = self.lookup_index(&axiom, h) {
                let entry = &mut self.axioms[pos];
                if entry.rows.last() != Some(&row) && !entry.rows.contains(&row) {
                    entry.rows.push(row);
                }
            } else {
                let pos = self.axioms.len();
                self.index.entry(h).or_default().push(pos);
                self.axioms.push(GeneratedAxiom {
                    axiom,
                    rows: vec![row],
                });
            }
        }
        RowCommit::Committed
    }

    fn finish(self, delimiter: Delimiter, headers: Vec<String>) -> PatternResult {
        PatternResult {
            delimiter,
            headers,
            rows_processed: self.rows_processed,
            rows_skipped: self.rows_skipped,
            rows_with_errors: self.error_rows.len(),
            error_count: self.total_errors,
            structural_error_count: self.structural_errors,
            budget_exceeded: self.budget_exceeded,
            axioms: self.axioms,
            declarations: self.declarations.into_iter().collect(),
            diagnostics: self.diagnostics,
            diagnostics_truncated: self.diagnostics_truncated,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RowCommit {
    Committed,
    Invalid,
    BudgetExceeded,
}

pub(crate) fn truncate_utf8(s: &mut String, max_bytes: usize) {
    if s.len() <= max_bytes {
        return;
    }
    let suffix = "…";
    let target = max_bytes.saturating_sub(suffix.len());
    let mut end = target.min(s.len());
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    s.truncate(end);
    if max_bytes >= suffix.len() {
        s.push_str(suffix);
    }
    s.shrink_to_fit();
}

pub(crate) fn truncated_snippet(s: &str, max_bytes: usize) -> String {
    if s.len() <= max_bytes {
        return s.to_string();
    }
    let mut out = String::with_capacity(max_bytes);
    let suffix = "…";
    let target = max_bytes.saturating_sub(suffix.len());
    let mut end = target.min(s.len());
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    out.push_str(&s[..end]);
    if max_bytes >= suffix.len() {
        out.push_str(suffix);
    }
    out
}

pub(crate) fn bounded_col(col: &str) -> String {
    truncated_snippet(col, MAX_DIAGNOSTIC_COLUMN_BYTES)
}

pub(crate) fn bounded_opt_col(col: Option<&str>) -> Option<String> {
    col.map(bounded_col)
}

/// Bounded staging buffer for spec/structural diagnostics collected before evaluation
/// rounds begin. Enforces `max_diagnostics`, `MAX_DIAGNOSTIC_COLUMN_BYTES`, and
/// `max_diagnostic_message_bytes` as items are staged so oversized spec labels cannot
/// accumulate unbounded heap memory before replay.
pub(crate) struct StructuralDiags {
    diags: Vec<RowDiagnostic>,
    total: usize,
    max_diags: usize,
    max_msg_bytes: usize,
}

impl StructuralDiags {
    pub(crate) fn new(limits: &Limits) -> Self {
        Self {
            diags: Vec::new(),
            total: 0,
            max_diags: limits.max_diagnostics,
            max_msg_bytes: limits.max_diagnostic_message_bytes,
        }
    }

    pub(crate) fn push(
        &mut self,
        row: usize,
        column: Option<&str>,
        code: &'static str,
        message: impl FnOnce() -> String,
    ) {
        self.total = self.total.saturating_add(1);
        if self.diags.len() < self.max_diags {
            let mut msg = message();
            truncate_utf8(&mut msg, self.max_msg_bytes);
            self.diags.push(RowDiagnostic {
                row,
                column: bounded_opt_col(column),
                severity: Severity::Error,
                code: code.to_string(),
                message: msg,
            });
        }
    }

    pub(crate) fn replay_into(&self, emitter: &mut Emitter) {
        for d in &self.diags {
            emitter.structural_error(d.clone());
        }
        let omitted = self.total.saturating_sub(self.diags.len());
        emitter.record_omitted_structural_errors(omitted);
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ConvergenceStep {
    Done,
    Continue,
    LimitExceeded,
}

/// Bounded fixpoint state machine for excluding failing rows from Pass 1 vocabulary.
/// When mutually dependent clean rows oscillate after a failing duplicate-label row is
/// dropped, attempts the union of rows eligible across the cycle before falling back to
/// monotonic exclusion.
pub(crate) struct VocabConvergence {
    n: usize,
    pub vocab_eligible: Vec<bool>,
    monotonic_excluded: Vec<bool>,
    monotonic_mode: bool,
    state_history: Vec<Vec<bool>>,
    seen_states: FxHashMap<Vec<bool>, usize>,
}

impl VocabConvergence {
    pub(crate) fn new(n: usize) -> Self {
        let initial = vec![true; n];
        let mut seen_states = FxHashMap::default();
        seen_states.insert(initial.clone(), 0);
        Self {
            n,
            vocab_eligible: initial.clone(),
            monotonic_excluded: vec![false; n],
            monotonic_mode: false,
            state_history: vec![initial],
            seen_states,
        }
    }

    pub(crate) fn advance(
        &mut self,
        round: usize,
        round_contributed: &[bool],
        round_failed: &[bool],
        row_pass1_ok: &[bool],
    ) -> ConvergenceStep {
        let failed_contributor = (0..self.n).any(|i| round_contributed[i] && round_failed[i]);
        let recovered_row = (0..self.n).any(|i| {
            !self.vocab_eligible[i]
                && row_pass1_ok[i]
                && !round_failed[i]
                && !self.monotonic_excluded[i]
        });
        if !failed_contributor && !recovered_row {
            return ConvergenceStep::Done;
        }

        let next_eligible: Vec<bool> = if self.monotonic_mode {
            for (i, &failed) in round_failed.iter().enumerate().take(self.n) {
                if failed {
                    self.monotonic_excluded[i] = true;
                }
            }
            (0..self.n).map(|i| !self.monotonic_excluded[i]).collect()
        } else {
            (0..self.n).map(|i| !round_failed[i]).collect()
        };

        if next_eligible == self.vocab_eligible {
            return ConvergenceStep::Done;
        }

        if round + 1 == MAX_VOCAB_ROUNDS {
            return ConvergenceStep::LimitExceeded;
        }

        if let Some(&cycle_start) = self.seen_states.get(&next_eligible) {
            if !self.monotonic_mode {
                let cycle_union: Vec<bool> = (0..self.n)
                    .map(|i| self.state_history[cycle_start..].iter().any(|s| s[i]))
                    .collect();
                if cycle_union != self.vocab_eligible
                    && !self.seen_states.contains_key(&cycle_union)
                {
                    let idx = self.state_history.len();
                    self.seen_states.insert(cycle_union.clone(), idx);
                    self.state_history.push(cycle_union.clone());
                    self.vocab_eligible = cycle_union;
                    return ConvergenceStep::Continue;
                }
            }
            self.monotonic_mode = true;
            for i in 0..self.n {
                if round_failed[i] || self.state_history[cycle_start..].iter().any(|s| !s[i]) {
                    self.monotonic_excluded[i] = true;
                }
            }
            self.vocab_eligible = (0..self.n).map(|i| !self.monotonic_excluded[i]).collect();
        } else {
            let idx = self.state_history.len();
            self.seen_states.insert(next_eligible.clone(), idx);
            self.state_history.push(next_eligible.clone());
            self.vocab_eligible = next_eligible;
        }
        ConvergenceStep::Continue
    }
}

/// Recursively count all annotations (including nested annotations) in `anns`.
pub(crate) fn count_annotations(anns: &BTreeSet<Annotation>) -> usize {
    let mut total = 0usize;
    for a in anns {
        total = total
            .saturating_add(1)
            .saturating_add(count_annotations(&a.annotations));
    }
    total
}

/// Approximate heap footprint of an [`Annotation`] in bytes for pre-clone budget charging.
pub(crate) fn annotation_bytes(a: &Annotation) -> usize {
    let val_bytes = match &a.value {
        AnnotationValue::Iri(i) => i.as_str().len(),
        AnnotationValue::Literal(l) => l
            .value()
            .len()
            .saturating_add(l.language().map_or(0, str::len))
            .saturating_add(l.datatype().len()),
        AnnotationValue::Anonymous(b) => b.label().len(),
    };
    let mut total = a
        .property
        .as_str()
        .len()
        .saturating_add(val_bytes)
        .saturating_add(64);
    for inner in &a.annotations {
        total = total.saturating_add(annotation_bytes(inner));
    }
    total
}

/// Evaluate raw CSV/TSV spreadsheet text against `spec` using `base_vocab` (the existing
/// ontology's vocabulary and prefixes) under `limits`.
pub fn evaluate(
    raw_table: &str,
    delimiter: Delimiter,
    spec: &PatternSpec,
    base_vocab: &Vocabulary,
    limits: &Limits,
) -> Result<PatternResult, TableError> {
    let mut noop = || Ok(());
    match evaluate_with_cancel(raw_table, delimiter, spec, base_vocab, limits, &mut noop) {
        Ok(r) => Ok(r),
        Err(EvalError::Table(e)) => Err(e),
        Err(EvalError::Cancelled | EvalError::Timeout) => unreachable!(),
    }
}

/// Evaluate raw CSV/TSV spreadsheet text against `spec`, periodically invoking
/// `check_cancel` so callers can enforce deadlines or client disconnect cancellation.
pub fn evaluate_with_cancel(
    raw_table: &str,
    delimiter: Delimiter,
    spec: &PatternSpec,
    base_vocab: &Vocabulary,
    limits: &Limits,
    check_cancel: &mut dyn FnMut() -> Result<(), CancelReason>,
) -> Result<PatternResult, EvalError> {
    let extra_rows = usize::from(matches!(
        spec.mode,
        TemplateMode::Robot {
            header_template_row: true,
            ..
        }
    ));
    let mut table_diags = Vec::new();
    let mut table_truncated = false;
    let table = table::parse_table_with_options(
        raw_table,
        delimiter,
        limits,
        extra_rows,
        &mut table_diags,
        &mut table_truncated,
        check_cancel,
    )?;
    evaluate_table_with_cancel(
        &table,
        spec,
        base_vocab,
        limits,
        table_diags,
        table_truncated,
        check_cancel,
    )
}

/// Evaluate an already-parsed [`Table`] against `spec`.
pub fn evaluate_table(
    table: &Table,
    spec: &PatternSpec,
    base_vocab: &Vocabulary,
    limits: &Limits,
    initial_diagnostics: Vec<RowDiagnostic>,
) -> PatternResult {
    let mut noop = || Ok(());
    evaluate_table_with_cancel(
        table,
        spec,
        base_vocab,
        limits,
        initial_diagnostics,
        false,
        &mut noop,
    )
    .expect("noop cancel never fails")
}

/// Evaluate an already-parsed [`Table`] against `spec` with cooperative cancellation.
pub fn evaluate_table_with_cancel(
    table: &Table,
    spec: &PatternSpec,
    base_vocab: &Vocabulary,
    limits: &Limits,
    initial_diagnostics: Vec<RowDiagnostic>,
    initial_truncated: bool,
    check_cancel: &mut dyn FnMut() -> Result<(), CancelReason>,
) -> Result<PatternResult, EvalError> {
    check_cancel()?;
    let mut emitter = Emitter::new(limits, initial_truncated);
    for d in &initial_diagnostics {
        emitter.diag(d.clone());
    }

    if !check_spec_limits(spec, limits, &mut emitter) {
        return Ok(emitter.finish(table.delimiter, table.headers.clone()));
    }

    if !validate_spec_prefixes(spec, &mut emitter) {
        return Ok(emitter.finish(table.delimiter, table.headers.clone()));
    }

    if let Some(base) = spec
        .base_iri
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        && parse_iri(base).is_err()
    {
        emitter.structural_error(RowDiagnostic {
            row: 1,
            column: None,
            severity: Severity::Error,
            code: "invalid-base-iri".to_string(),
            message: format!(
                "base IRI '{}' is not a valid absolute IRI",
                truncated_snippet(base, 128)
            ),
        });
        return Ok(emitter.finish(table.delimiter, table.headers.clone()));
    }

    match &spec.mode {
        TemplateMode::Robot {
            header_template_row,
            columns,
        } => robot::evaluate_robot(
            table,
            *header_template_row,
            columns,
            base_vocab,
            spec,
            limits,
            &initial_diagnostics,
            initial_truncated,
            &mut emitter,
            check_cancel,
        )?,
        TemplateMode::Pattern {
            rules,
            declarations,
        } => rules::evaluate_rules(
            table,
            rules,
            declarations,
            base_vocab,
            spec,
            limits,
            &initial_diagnostics,
            initial_truncated,
            &mut emitter,
            check_cancel,
        )?,
    }

    Ok(emitter.finish(table.delimiter, table.headers.clone()))
}

fn validate_spec_prefixes(spec: &PatternSpec, emitter: &mut Emitter) -> bool {
    let mut ok = true;
    for (raw_key, raw_ns) in &spec.prefixes {
        let trimmed_key = raw_key.trim();
        let clean = normalize_prefix_key(trimmed_key);
        let single_trailing_colon = trimmed_key
            .strip_suffix(':')
            .is_none_or(|rest| !rest.contains(':'));
        if !single_trailing_colon || !is_valid_prefix_name(clean) {
            ok = false;
            emitter.structural_error(RowDiagnostic {
                row: 1,
                column: None,
                severity: Severity::Error,
                code: "invalid-prefix".to_string(),
                message: format!(
                    "prefix '{}' is not a valid ASCII prefix name",
                    truncated_snippet(raw_key, 64)
                ),
            });
            continue;
        }
        let ns = raw_ns.trim();
        if ns.is_empty() || parse_iri(ns).is_err() {
            ok = false;
            emitter.structural_error(RowDiagnostic {
                row: 1,
                column: None,
                severity: Severity::Error,
                code: "invalid-prefix".to_string(),
                message: format!(
                    "namespace '{}' for prefix '{clean}' is not a valid absolute IRI",
                    truncated_snippet(ns, 128)
                ),
            });
        }
    }
    ok
}

fn check_spec_limits(spec: &PatternSpec, limits: &Limits, emitter: &mut Emitter) -> bool {
    if spec.prefixes.len() > limits.max_prefixes {
        emitter.budget_error(
            1,
            None,
            "spec-limit",
            format!(
                "spec defines {} prefixes, exceeding the limit of {}",
                spec.prefixes.len(),
                limits.max_prefixes
            ),
        );
        return false;
    }
    for (raw_key, raw_ns) in &spec.prefixes {
        let ns = raw_ns.trim();
        if ns.len() > limits.max_expanded_bytes_per_template {
            emitter.budget_error(
                1,
                None,
                "spec-limit",
                format!(
                    "namespace for prefix '{}' ({} bytes) exceeds the {} byte limit",
                    truncated_snippet(raw_key, 64),
                    ns.len(),
                    limits.max_expanded_bytes_per_template
                ),
            );
            return false;
        }
    }
    match &spec.mode {
        TemplateMode::Robot { columns, .. } => {
            if columns.len() > limits.max_columns_spec {
                emitter.budget_error(
                    1,
                    None,
                    "spec-limit",
                    format!(
                        "spec defines {} column directives, exceeding the limit of {}",
                        columns.len(),
                        limits.max_columns_spec
                    ),
                );
                return false;
            }
        }
        TemplateMode::Pattern {
            rules,
            declarations,
        } => {
            if rules.len() > limits.max_rules {
                emitter.budget_error(
                    1,
                    None,
                    "spec-limit",
                    format!(
                        "spec defines {} rules, exceeding the limit of {}",
                        rules.len(),
                        limits.max_rules
                    ),
                );
                return false;
            }
            if declarations.len() > limits.max_declarations {
                emitter.budget_error(
                    1,
                    None,
                    "spec-limit",
                    format!(
                        "spec defines {} declarations, exceeding the limit of {}",
                        declarations.len(),
                        limits.max_declarations
                    ),
                );
                return false;
            }
            for (idx, rule) in rules.iter().enumerate() {
                if rule.annotations.len() > limits.max_annotations_per_rule {
                    let fallback = format!("rule_{}", idx + 1);
                    let col = rule.name.as_deref().unwrap_or(&fallback);
                    emitter.budget_error(
                        1,
                        Some(col),
                        "spec-limit",
                        format!(
                            "rule defines {} annotations, exceeding the limit of {}",
                            rule.annotations.len(),
                            limits.max_annotations_per_rule
                        ),
                    );
                    return false;
                }
            }
        }
    }
    true
}

pub(crate) fn normalize_prefix_key(raw: &str) -> &str {
    let trimmed = raw.trim();
    trimmed.strip_suffix(':').unwrap_or(trimmed)
}

/// ASCII prefix name (`PN_PREFIX` subset) or empty default prefix, matching the store's
/// prefix grammar so `resolve_iri`, Manchester syntax, and Functional-Style syntax agree.
pub(crate) fn is_valid_prefix_name(prefix: &str) -> bool {
    let b = prefix.as_bytes();
    prefix.is_empty()
        || (b[0].is_ascii_alphabetic()
            && b.iter()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'-' | b'.'))
            && !prefix.ends_with('.'))
}

pub(crate) enum ResolveError {
    Invalid(String),
    Budget,
}

/// Resolve an IRI, `<IRI>`, CURIE (`prefix:local`), or unqualified bare local name (when
/// `base_iri` is configured) into a validated [`Iri`], charging the resolved byte length
/// against `emitter` BEFORE allocating expanded strings.
pub(crate) fn resolve_iri_charged(
    raw: &str,
    ctx: &Context,
    emitter: &mut Emitter,
    row: usize,
    column: Option<&str>,
) -> Result<Iri, ResolveError> {
    let s = raw.trim();
    if s.is_empty() {
        return Err(ResolveError::Invalid("empty IRI".to_string()));
    }
    let max_per_template = emitter.limits.max_expanded_bytes_per_template;
    let charge = |len: usize, emitter: &mut Emitter| -> Result<(), ResolveError> {
        if len > max_per_template {
            return Err(ResolveError::Invalid(format!(
                "resolved IRI ({len} bytes) exceeds the {max_per_template} byte limit"
            )));
        }
        if !emitter.charge_expanded_bytes(len, row, column) {
            return Err(ResolveError::Budget);
        }
        Ok(())
    };

    if s.starts_with('<') && s.ends_with('>') && s.len() > 2 {
        let inner = &s[1..s.len() - 1];
        charge(inner.len(), emitter)?;
        return parse_iri(inner).map_err(|e| ResolveError::Invalid(e.0));
    }
    if !s.contains(char::is_whitespace)
        && let Some((prefix, local)) = s.split_once(':')
    {
        // Hierarchical full IRIs such as http://..., https://..., file://...
        if local.starts_with("//") {
            charge(s.len(), emitter)?;
            return parse_iri(s).map_err(|e| ResolveError::Invalid(e.0));
        }
        if is_valid_prefix_name(prefix) {
            if let Some((_, ns)) = ctx.prefixes.iter().find(|(p, _)| p == prefix) {
                let est_len = ns.len().saturating_add(local.len());
                charge(est_len, emitter)?;
                let mut full = String::with_capacity(est_len);
                full.push_str(ns);
                full.push_str(local);
                return parse_iri(&full).map_err(|e| ResolveError::Invalid(e.0));
            }
            if !prefix.is_empty()
                && ALLOWED_NON_HIERARCHICAL_SCHEMES
                    .iter()
                    .any(|scheme| prefix.eq_ignore_ascii_case(scheme))
            {
                charge(s.len(), emitter)?;
                return parse_iri(s).map_err(|e| ResolveError::Invalid(e.0));
            }
            return Err(ResolveError::Invalid(format!(
                "unknown prefix '{}' in '{}' (bind the prefix or wrap full IRIs in <...>)",
                truncated_snippet(prefix, 64),
                truncated_snippet(s, 128)
            )));
        }
        // Any other non-whitespace token containing ':' is a malformed qualified name
        // and must NOT fall through to base_iri expansion.
        return Err(ResolveError::Invalid(format!(
            "invalid prefix '{}' in '{}' (use an ASCII prefix name or wrap full IRIs in <...>)",
            truncated_snippet(prefix, 64),
            truncated_snippet(s, 128)
        )));
    }
    // Unqualified bare local name resolved against base_iri if configured.
    if let Some(base) = &ctx.base_iri
        && !s.contains(char::is_whitespace)
        && !s.contains(':')
    {
        let needs_hash = !(base.ends_with('#') || base.ends_with('/') || base.ends_with(':'));
        let est_len = base
            .len()
            .saturating_add(usize::from(needs_hash))
            .saturating_add(s.len());
        charge(est_len, emitter)?;
        let mut joined = String::with_capacity(est_len);
        joined.push_str(base);
        if needs_hash {
            joined.push('#');
        }
        joined.push_str(s);
        return parse_iri(&joined).map_err(|e| ResolveError::Invalid(e.0));
    }
    // Fall back to unique label lookup in the vocabulary.
    let mut matches = ctx.vocab.named(s);
    if let Some(first) = matches.next() {
        if matches.next().is_none() {
            charge(first.len(), emitter)?;
            return parse_iri(first).map_err(|e| ResolveError::Invalid(e.0));
        }
        return Err(ResolveError::Invalid(format!(
            "name '{}' is ambiguous in the ontology vocabulary",
            truncated_snippet(s, 128)
        )));
    }
    Err(ResolveError::Invalid(format!(
        "cannot resolve '{}' as an IRI, prefixed name, or known label",
        truncated_snippet(s, 128)
    )))
}

/// Charge estimated namespace expansion bytes for prefixed names appearing in a Manchester
/// or Functional-Style expression BEFORE invoking the parser, honoring each syntax's
/// comment and quote rules. Returns the number of extra bytes charged so bounded parsers
/// can charge only any additional resolved bytes (labels, bare names, empty-prefix
/// fallbacks, full IRIs, or escaped OFN names).
pub(crate) fn charge_prefixed_expansions(
    text: &str,
    syntax: PatternSyntax,
    ctx: &Context,
    emitter: &mut Emitter,
    row: usize,
    column: Option<&str>,
) -> Result<usize, ResolveError> {
    let bytes = text.as_bytes();
    let mut extra_bytes = 0usize;
    let mut i = 0usize;
    let mut in_double_quote = false;
    let mut in_single_quote = false;
    let mut in_angle = false;
    let mut escaped = false;
    let single_quotes = syntax == PatternSyntax::Manchester;
    let hash_comments = syntax == PatternSyntax::Functional;

    while i < bytes.len() {
        let b = bytes[i];
        if in_double_quote {
            if escaped {
                escaped = false;
            } else if b == b'\\' {
                escaped = true;
            } else if b == b'"' {
                in_double_quote = false;
            }
            i += 1;
            continue;
        }
        if in_single_quote {
            if escaped {
                escaped = false;
            } else if b == b'\\' {
                escaped = true;
            } else if b == b'\'' {
                in_single_quote = false;
            }
            i += 1;
            continue;
        }
        if in_angle {
            if b == b'>' || b.is_ascii_whitespace() {
                in_angle = false;
            }
            i += 1;
            continue;
        }
        if hash_comments && b == b'#' {
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        match b {
            b'"' => in_double_quote = true,
            b'\'' if single_quotes => in_single_quote = true,
            b'<' => in_angle = true,
            b':' if bytes.get(i + 1..i + 3) != Some(b"//") => {
                // Skip hierarchical URI schemes (`scheme://...`).
                let mut start = i;
                while start > 0 {
                    let prev = bytes[start - 1];
                    if prev.is_ascii_alphanumeric() || matches!(prev, b'_' | b'-' | b'.') {
                        start -= 1;
                    } else {
                        break;
                    }
                }
                let prefix = &text[start..i];
                if is_valid_prefix_name(prefix)
                    && let Some((_, ns)) = ctx.prefixes.iter().find(|(p, _)| p == prefix)
                {
                    extra_bytes = extra_bytes.saturating_add(ns.len());
                }
            }
            _ => {}
        }
        i += 1;
    }

    if extra_bytes == 0 {
        return Ok(0);
    }
    let max_per_template = emitter.limits.max_expanded_bytes_per_template;
    let total_est = text.len().saturating_add(extra_bytes);
    if total_est > max_per_template {
        return Err(ResolveError::Invalid(format!(
            "expanded prefixed terms ({total_est} bytes) exceed the {max_per_template} byte limit"
        )));
    }
    if !emitter.charge_expanded_bytes(extra_bytes, row, column) {
        return Err(ResolveError::Budget);
    }
    Ok(extra_bytes)
}

fn finalize_parser_budget(
    used_bytes: usize,
    precharged: usize,
    emitter: &mut Emitter,
    row: usize,
    column: Option<&str>,
) -> Result<(), ResolveError> {
    let extra = used_bytes.saturating_sub(precharged);
    if extra > 0 && !emitter.charge_expanded_bytes(extra, row, column) {
        return Err(ResolveError::Budget);
    }
    Ok(())
}

fn handle_parser_budget_exceeded(
    used: usize,
    attempted: usize,
    precharged: usize,
    emitter: &mut Emitter,
    row: usize,
    column: Option<&str>,
) -> ResolveError {
    let per_template = emitter.limits.max_expanded_bytes_per_template;
    let total_rem = emitter.remaining_expanded_bytes();
    let extra_attempted = attempted.saturating_sub(precharged);
    if extra_attempted > total_rem {
        let _ = emitter.charge_expanded_bytes(extra_attempted, row, column);
        return ResolveError::Budget;
    }
    if attempted > per_template {
        if let Err(err) = finalize_parser_budget(used, precharged, emitter, row, column) {
            return err;
        }
        return ResolveError::Invalid(format!(
            "resolved terms ({attempted} bytes) exceed the {per_template} byte limit"
        ));
    }
    let _ = emitter.charge_expanded_bytes(extra_attempted.max(1), row, column);
    ResolveError::Budget
}

fn parser_byte_cap(precharged: usize, emitter: &Emitter) -> usize {
    let per_template = emitter.limits.max_expanded_bytes_per_template;
    let total_avail = precharged.saturating_add(emitter.remaining_expanded_bytes());
    per_template.min(total_avail)
}

/// Parse a Manchester class expression after charging prefixed-name expansions and
/// vocabulary-resolved term bytes, falling back to resolving a simple class IRI/CURIE
/// when the class is not yet declared in `ctx.vocab`.
pub(crate) fn parse_class_expr_fallback_charged(
    text: &str,
    ctx: &Context,
    emitter: &mut Emitter,
    row: usize,
    column: Option<&str>,
) -> Result<ClassExpression, ResolveError> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Err(ResolveError::Invalid("empty class expression".to_string()));
    }
    let precharged = charge_prefixed_expansions(
        trimmed,
        PatternSyntax::Manchester,
        ctx,
        emitter,
        row,
        column,
    )?;
    let cap = parser_byte_cap(precharged, emitter);
    match manchester::class_expression_bounded(trimmed, &ctx.vocab, cap) {
        Ok((ce, used)) => {
            finalize_parser_budget(used, precharged, emitter, row, column)?;
            Ok(ce)
        }
        Err(manchester::BoundedError::Budget { used, attempted }) => Err(
            handle_parser_budget_exceeded(used, attempted, precharged, emitter, row, column),
        ),
        Err(manchester::BoundedError::Parse { error, used }) => {
            finalize_parser_budget(used, precharged, emitter, row, column)?;
            // If the expression is a single token (IRI or CURIE) not yet in vocab, accept it
            // as a named Class so spreadsheets can reference external or undeclared superclasses.
            if !trimmed.contains(char::is_whitespace) && !trimmed.contains(['(', ')', '{', '}']) {
                match resolve_iri_charged(trimmed, ctx, emitter, row, column) {
                    Ok(iri) => return Ok(ClassExpression::Class(Class(iri))),
                    Err(ResolveError::Budget) => return Err(ResolveError::Budget),
                    Err(ResolveError::Invalid(_)) => {}
                }
            }
            Err(ResolveError::Invalid(error.message))
        }
    }
}

/// Parse one Manchester or Functional-Style axiom with pre-lexing and parser-level byte
/// charging so prefixed names, labels, bare names, empty-prefix fallbacks, and OFN
/// comments/escapes are all accounted for before allocation.
pub(crate) fn parse_axiom_charged(
    text: &str,
    syntax: PatternSyntax,
    ctx: &Context,
    emitter: &mut Emitter,
    row: usize,
    column: Option<&str>,
) -> Result<Axiom, ResolveError> {
    let precharged = charge_prefixed_expansions(text, syntax, ctx, emitter, row, column)?;
    let cap = parser_byte_cap(precharged, emitter);
    match syntax {
        PatternSyntax::Manchester => match manchester::axiom_bounded(text, &ctx.vocab, cap) {
            Ok((ax, used)) => {
                finalize_parser_budget(used, precharged, emitter, row, column)?;
                Ok(ax)
            }
            Err(manchester::BoundedError::Budget { used, attempted }) => Err(
                handle_parser_budget_exceeded(used, attempted, precharged, emitter, row, column),
            ),
            Err(manchester::BoundedError::Parse { error, used }) => {
                finalize_parser_budget(used, precharged, emitter, row, column)?;
                Err(ResolveError::Invalid(error.message))
            }
        },
        PatternSyntax::Functional => {
            match crate::ofn_read::parse_axiom_bounded(text, ctx.vocab.prefixes(), cap) {
                Ok((ax, used)) => {
                    finalize_parser_budget(used, precharged, emitter, row, column)?;
                    Ok(ax)
                }
                Err(crate::ofn_read::BoundedParseError::Budget { used, attempted }) => {
                    Err(handle_parser_budget_exceeded(
                        used, attempted, precharged, emitter, row, column,
                    ))
                }
                Err(crate::ofn_read::BoundedParseError::Parse { error, used }) => {
                    finalize_parser_budget(used, precharged, emitter, row, column)?;
                    Err(ResolveError::Invalid(error.message))
                }
            }
        }
    }
}

/// Parse a ROBOT `I <data-prop>` literal cell with parser-level datatype IRI byte
/// charging (only when the parser reaches a typed literal), falling back to
/// `Literal::simple(val)` on ordinary syntax misses.
pub(crate) fn parse_cell_literal_charged(
    val: &str,
    ctx: &Context,
    emitter: &mut Emitter,
    row: usize,
    column: Option<&str>,
) -> Result<Literal, ResolveError> {
    let cap = parser_byte_cap(0, emitter);
    match manchester::literal_bounded(val, &ctx.vocab, cap) {
        Ok((lit, used)) => {
            finalize_parser_budget(used, 0, emitter, row, column)?;
            Ok(lit)
        }
        Err(manchester::BoundedError::Budget { used, attempted }) => Err(
            handle_parser_budget_exceeded(used, attempted, 0, emitter, row, column),
        ),
        Err(manchester::BoundedError::Parse { used, .. }) => {
            finalize_parser_budget(used, 0, emitter, row, column)?;
            Ok(Literal::simple(val))
        }
    }
}

#[cfg(test)]
mod tests;
