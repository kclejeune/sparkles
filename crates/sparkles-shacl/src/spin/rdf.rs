//! Bounded SPARQL 1.1 serializer for SPIN RDF syntax (`http://spinrdf.org/sp#`).

use super::template::{
    KnownPrefixes, collect_sparql_text_vars, substitute_sparql_text_with_exported,
    validate_subquery_text,
};
use super::vocab::{
    SP_NS, aggregate_fn, binary_op, builtin_fn, is_sp_expression_type, is_structural_sp_class, sp,
    spin, unary_op,
};
use crate::shapes::{G, as_subject};
use crate::vocab::{rdf, rdfs};
use anyhow::{Context as _, Result, anyhow, bail};
use oxrdf::vocab::xsd;
use oxrdf::{
    BlankNode, LiteralRef, NamedNode, NamedNodeRef, NamedOrBlankNode, NamedOrBlankNodeRef, Term,
    TermRef,
};
use rustc_hash::{FxHashMap, FxHashSet};
use std::fmt;

/// Maximum nesting depth for patterns, expressions, property paths, subqueries, and templates.
pub(crate) const MAX_DEPTH: usize = 64;
/// Maximum RDF nodes / graph steps visited while translating one SPIN query (prevents DAG
/// and wide-hierarchy blowup).
pub(crate) const MAX_NODES: usize = 16_384;
/// Maximum serialized SPARQL query length in bytes.
pub(crate) const MAX_OUTPUT_BYTES: usize = 1_048_576;
/// Maximum cumulative bytes of owned RDF terms retained while translating one SPIN query.
pub(crate) const MAX_RETAINED_BYTES: usize = MAX_OUTPUT_BYTES * 2;
/// Maximum length of any RDF list traversed in SPIN syntax.
pub(crate) const MAX_LIST_LEN: usize = 4_096;
/// Maximum fixed repetition count unrolled from `sp:ModPath` into SPARQL 1.1 paths.
const MAX_MOD_PATH_UNROLL: usize = 16;

/// Marker error for resource-budget, depth, size, and cycle rejections. Dual-form
/// `sp:text` compatibility fallback never masks these errors.
#[derive(Debug)]
pub(crate) struct SpinLimitError(pub(crate) String);

impl fmt::Display for SpinLimitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for SpinLimitError {}

pub(crate) fn limit_err(msg: impl Into<String>) -> anyhow::Error {
    anyhow::Error::new(SpinLimitError(msg.into()))
}

pub(crate) fn is_limit_or_cycle_error(err: &anyhow::Error) -> bool {
    err.chain().any(|e| e.is::<SpinLimitError>())
}

/// Map a `sparkles_core::Error` returned by query parsing/validation, preserving typed
/// resource-limit and nesting/depth refusals as [`SpinLimitError`] so dual-form `sp:text`
/// fallback cannot hide them.
pub(crate) fn map_core_parse_error(err: sparkles_core::Error, context: &str) -> anyhow::Error {
    let is_limit = match &err {
        sparkles_core::Error::SparqlSyntax(e) => e.is_too_deep(),
        sparkles_core::Error::BudgetExceeded(_)
        | sparkles_core::Error::Timeout
        | sparkles_core::Error::Cancelled => true,
        sparkles_core::Error::Invalid(msg) => msg.contains("nested deeper than"),
        _ => false,
    };
    if is_limit {
        limit_err(format!("{context}: {err}"))
    } else {
        anyhow!("{context}: {err}")
    }
}

/// Shared step and retained-byte budget bounding graph reads, hierarchy walks, template
/// resolution, text scanning, and AST serialization for one SPIN query.
#[derive(Clone, Debug)]
pub(crate) struct WorkBudget {
    steps: usize,
    limit: usize,
    bytes: usize,
    byte_limit: usize,
}

impl WorkBudget {
    pub(crate) fn new(limit: usize) -> Self {
        Self {
            steps: 0,
            limit,
            bytes: 0,
            byte_limit: MAX_RETAINED_BYTES,
        }
    }

    pub(crate) fn step(&mut self) -> Result<()> {
        self.steps = self.steps.saturating_add(1);
        if self.steps > self.limit {
            return Err(limit_err(format!(
                "SPIN query exceeds maximum node budget ({})",
                self.limit
            )));
        }
        Ok(())
    }

    pub(crate) fn charge_bytes(&mut self, bytes: usize) -> Result<()> {
        self.bytes = self.bytes.saturating_add(bytes);
        if self.bytes > self.byte_limit {
            return Err(limit_err(format!(
                "SPIN query exceeds maximum retained term size ({} bytes)",
                self.byte_limit
            )));
        }
        Ok(())
    }

    pub(crate) fn remaining_bytes(&self) -> usize {
        self.byte_limit.saturating_sub(self.bytes)
    }
}

/// Validate a borrowed [`TermRef`] against [`MAX_DEPTH`] and [`MAX_OUTPUT_BYTES`] without
/// cloning it, returning its estimated byte size.
pub(crate) fn term_ref_size_and_depth(
    term: TermRef<'_>,
    depth: usize,
    budget: &mut WorkBudget,
) -> Result<usize> {
    if depth > MAX_DEPTH {
        return Err(limit_err(format!(
            "embedded RDF-star triple term exceeds maximum nesting depth ({MAX_DEPTH})"
        )));
    }
    let bytes = match term {
        TermRef::NamedNode(n) => n.as_str().len(),
        TermRef::BlankNode(b) => b.as_str().len(),
        TermRef::Literal(l) => {
            let extra = if let Some(lang) = l.language() {
                lang.len()
            } else if l.datatype() != xsd::STRING {
                l.datatype().as_str().len()
            } else {
                0
            };
            l.value().len().saturating_add(extra)
        }
        TermRef::Triple(t) => {
            budget.step()?;
            let s = term_ref_size_and_depth(TermRef::from(t.subject.as_ref()), depth + 1, budget)?;
            let p = t.predicate.as_str().len();
            let o = term_ref_size_and_depth(t.object.as_ref(), depth + 1, budget)?;
            s.saturating_add(p).saturating_add(o).saturating_add(10)
        }
    };
    if bytes > MAX_OUTPUT_BYTES {
        return Err(limit_err(format!(
            "SPIN term exceeds maximum output size ({MAX_OUTPUT_BYTES} bytes)"
        )));
    }
    Ok(bytes)
}

/// Validate a borrowed [`TermRef`] for depth and byte size, charge its bytes to `budget`,
/// and convert to an owned [`Term`].
pub(crate) fn check_and_own_term(term: TermRef<'_>, budget: &mut WorkBudget) -> Result<Term> {
    let bytes = term_ref_size_and_depth(term, 0, budget)?;
    budget.charge_bytes(bytes)?;
    Ok(term.into_owned())
}

/// Validate a borrowed [`NamedOrBlankNodeRef`] and charge its bytes before cloning.
pub(crate) fn check_and_own_subject(
    sub: NamedOrBlankNodeRef<'_>,
    budget: &mut WorkBudget,
) -> Result<NamedOrBlankNode> {
    let bytes = term_ref_size_and_depth(TermRef::from(sub), 0, budget)?;
    budget.charge_bytes(bytes)?;
    Ok(sub.into_owned())
}

/// Validate a borrowed [`NamedNodeRef`] and charge its bytes before cloning.
pub(crate) fn check_and_own_iri(n: NamedNodeRef<'_>, budget: &mut WorkBudget) -> Result<NamedNode> {
    let bytes = n.as_str().len();
    if bytes > MAX_OUTPUT_BYTES {
        return Err(limit_err(format!(
            "SPIN IRI exceeds maximum output size ({MAX_OUTPUT_BYTES} bytes)"
        )));
    }
    budget.charge_bytes(bytes)?;
    Ok(n.into_owned())
}

fn single_object_ref_with_bytes<'g>(
    g: &G<'g>,
    node: &Term,
    pred: NamedNodeRef<'_>,
    budget: &mut WorkBudget,
) -> Result<Option<(TermRef<'g>, usize)>> {
    budget.step()?;
    let Some(sub) = as_subject(node) else {
        return Ok(None);
    };
    let mut iter = g.g.objects_for_subject_predicate(sub, pred);
    let Some(first) = iter.next() else {
        return Ok(None);
    };
    if iter.next().is_some() {
        bail!(
            "SPIN node {node} has multiple values for <{}>",
            pred.as_str()
        );
    }
    let bytes = term_ref_size_and_depth(first, 0, budget)?;
    Ok(Some((first, bytes)))
}

/// Read a singleton property from `g` as a borrowed [`TermRef`] after checking
/// cardinality, depth, and byte-size limits without cloning.
pub(crate) fn single_object_ref<'g>(
    g: &G<'g>,
    node: &Term,
    pred: NamedNodeRef<'_>,
    budget: &mut WorkBudget,
) -> Result<Option<TermRef<'g>>> {
    Ok(single_object_ref_with_bytes(g, node, pred, budget)?.map(|(t, _)| t))
}

/// Read a singleton property from `g` inspecting at most two borrowed objects (no `Vec`
/// allocation), validating depth and byte limits and charging `budget` before cloning.
pub(crate) fn single_object(
    g: &G<'_>,
    node: &Term,
    pred: NamedNodeRef<'_>,
    budget: &mut WorkBudget,
) -> Result<Option<Term>> {
    let Some((first, bytes)) = single_object_ref_with_bytes(g, node, pred, budget)? else {
        return Ok(None);
    };
    budget.charge_bytes(bytes)?;
    Ok(Some(first.into_owned()))
}

/// Bounded `rdf:type / rdfs:subClassOf*` instance test in `g`.
pub(crate) fn is_instance_bounded(
    g: &G<'_>,
    node: &Term,
    class: NamedNodeRef<'_>,
    budget: &mut WorkBudget,
) -> Result<bool> {
    budget.step()?;
    let Some(sub) = as_subject(node) else {
        return Ok(false);
    };
    let target = TermRef::NamedNode(class);
    let mut stack: Vec<Term> = Vec::new();
    for obj in g.g.objects_for_subject_predicate(sub, rdf::TYPE) {
        budget.step()?;
        if obj == target {
            return Ok(true);
        }
        if matches!(obj, TermRef::NamedNode(_) | TermRef::BlankNode(_)) {
            stack.push(check_and_own_term(obj, budget)?);
        }
    }
    let mut seen: FxHashSet<Term> = FxHashSet::default();
    while let Some(c) = stack.pop() {
        if !seen.insert(c.clone()) {
            continue;
        }
        let Some(c_sub) = as_subject(&c) else {
            continue;
        };
        for sup in g.g.objects_for_subject_predicate(c_sub, rdfs::SUB_CLASS_OF) {
            budget.step()?;
            if sup == target {
                return Ok(true);
            }
            if matches!(sup, TermRef::NamedNode(_) | TermRef::BlankNode(_)) {
                stack.push(check_and_own_term(sup, budget)?);
            }
        }
    }
    Ok(false)
}

/// Strict XML Schema 1.1 `xsd:boolean` parser for SPIN flags (`sp:distinct`, `sp:reduced`,
/// `sp:silent`, `spl:optional`). Accepts `"true"`, `"1"`, `"false"`, `"0"` and refuses
/// invalid lexical forms or non-boolean terms.
pub(crate) fn parse_spin_bool_ref(t: TermRef<'_>, context: &str) -> Result<bool> {
    if let TermRef::Literal(l) = t
        && l.datatype() == xsd::BOOLEAN
    {
        match l.value() {
            "true" | "1" => return Ok(true),
            "false" | "0" => return Ok(false),
            _ => {}
        }
    }
    bail!(
        "{context} expects a valid xsd:boolean literal (\"true\", \"false\", \"1\", or \"0\"), found {t}"
    )
}

/// Whether a query node has SPIN RDF query structure (`sp:where`, `sp:templates`, or
/// `sp:resultNodes`).
pub(crate) fn has_rdf_query_structure(g: &G<'_>, node: &Term) -> bool {
    g.has(node, sp::WHERE) || g.has(node, sp::TEMPLATES) || g.has(node, sp::RESULT_NODES)
}

/// Serialize a SPIN RDF syntax query rooted at `query` with optional template argument
/// bindings substituted in place, using `budget`.
pub(crate) fn serialize_query_with_bindings(
    g: &G<'_>,
    query: &Term,
    bindings: &FxHashMap<String, Option<Term>>,
    known_prefixes: Option<&KnownPrefixes<'_>>,
    budget: &mut WorkBudget,
) -> Result<String> {
    let mut ser = Serializer {
        g: *g,
        bindings,
        known_prefixes,
        budget,
        segments: Vec::new(),
        segments_bytes: 0,
        out: String::new(),
        reserved_out_bytes: 0,
        #[cfg(test)]
        peak_serialization_output_bytes: 0,
        #[cfg(test)]
        peak_live_output_bytes: 0,
        ancestors: FxHashSet::default(),
        anon_vars: FxHashMap::default(),
        anon_var_spans: Vec::new(),
        template_bnodes: FxHashMap::default(),
        used_var_names: FxHashSet::default(),
    };
    ser.write_query(query, 0, false)?;
    ser.resolve_anon_var_collisions()
}

/// Logical live-output byte watermarks recorded across owned serializer buffers in tests.
///
/// - `serialization_live`: peak logical bytes across `segments + out + reserved_out_bytes`
///   during `write_query`, strictly enforced `<= MAX_OUTPUT_BYTES`.
/// - `finalization_live`: peak logical bytes across serialization and finalization,
///   including the transient coexistence of the single destination buffer and unconsumed
///   segments during multi-segment flattening (`final_len + (total - max_segment_len) <=
///   MAX_RETAINED_BYTES`). This measures logical owned bytes and does not observe
///   allocator-internal transient overlap if `reserve_exact` moves the surviving buffer.
#[cfg(test)]
#[derive(Clone, Copy, Debug)]
pub(crate) struct SerializationPeakBytes {
    pub(crate) serialization_live: usize,
    pub(crate) finalization_live: usize,
}

#[cfg(test)]
pub(crate) fn serialize_query_with_bindings_measured(
    g: &G<'_>,
    query: &Term,
    bindings: &FxHashMap<String, Option<Term>>,
    known_prefixes: Option<&KnownPrefixes<'_>>,
    budget: &mut WorkBudget,
) -> Result<(String, SerializationPeakBytes)> {
    let mut ser = Serializer {
        g: *g,
        bindings,
        known_prefixes,
        budget,
        segments: Vec::new(),
        segments_bytes: 0,
        out: String::new(),
        reserved_out_bytes: 0,
        peak_serialization_output_bytes: 0,
        peak_live_output_bytes: 0,
        ancestors: FxHashSet::default(),
        anon_vars: FxHashMap::default(),
        anon_var_spans: Vec::new(),
        template_bnodes: FxHashMap::default(),
        used_var_names: FxHashSet::default(),
    };
    ser.write_query(query, 0, false)?;
    let sparql = ser.resolve_anon_var_collisions()?;
    Ok((
        sparql,
        SerializationPeakBytes {
            serialization_live: ser.peak_serialization_output_bytes,
            finalization_live: ser.peak_live_output_bytes,
        },
    ))
}

/// Append `s` to `out`, refusing output larger than `limit`.
fn push_str_bounded_max(out: &mut String, s: &str, limit: usize) -> Result<()> {
    let Some(next) = out.len().checked_add(s.len()) else {
        return Err(limit_err(format!(
            "serialized SPIN query exceeds maximum output size ({MAX_OUTPUT_BYTES} bytes)"
        )));
    };
    if next > limit {
        return Err(limit_err(format!(
            "serialized SPIN query exceeds maximum output size ({MAX_OUTPUT_BYTES} bytes)"
        )));
    }
    out.push_str(s);
    Ok(())
}

/// Append `s` to `out`, refusing output larger than [`MAX_OUTPUT_BYTES`].
pub(crate) fn push_str_bounded(out: &mut String, s: &str) -> Result<()> {
    push_str_bounded_max(out, s, MAX_OUTPUT_BYTES)
}

/// Validate a SPARQL 1.1 `VARNAME` (without leading `?` or `$`).
pub(crate) fn validate_var_name(name: &str) -> Result<()> {
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        bail!("empty SPIN variable name");
    };
    let valid_first = crate::sparql::varname_char(first)
        && !matches!(
            first,
            '\u{B7}' | '\u{0300}'..='\u{036F}' | '\u{203F}'..='\u{2040}'
        );
    if !valid_first || !chars.all(crate::sparql::varname_char) {
        bail!("invalid SPARQL variable name {name:?}");
    }
    Ok(())
}

/// Serialize an IRI reference `<...>` after checking for forbidden SPARQL `IRIREF` characters.
fn write_iri_max(out: &mut String, iri: &str, limit: usize) -> Result<()> {
    if out.len().saturating_add(iri.len()).saturating_add(2) > limit {
        return Err(limit_err(format!(
            "serialized SPIN query exceeds maximum output size ({MAX_OUTPUT_BYTES} bytes)"
        )));
    }
    if iri
        .chars()
        .any(|c| c <= ' ' || matches!(c, '<' | '>' | '"' | '{' | '}' | '|' | '^' | '`' | '\\'))
    {
        bail!("IRI {iri:?} contains characters illegal in a SPARQL IRIREF");
    }
    push_str_bounded_max(out, "<", limit)?;
    push_str_bounded_max(out, iri, limit)?;
    push_str_bounded_max(out, ">", limit)
}

fn validate_lang_tag(lang: &str) -> Result<()> {
    let mut parts = lang.split('-');
    let Some(first) = parts.next() else {
        bail!("empty language tag");
    };
    if first.is_empty() || !first.chars().all(|c| c.is_ascii_alphabetic()) {
        bail!("invalid language tag {lang:?}");
    }
    for part in parts {
        if part.is_empty() || !part.chars().all(|c| c.is_ascii_alphanumeric()) {
            bail!("invalid language tag {lang:?}");
        }
    }
    Ok(())
}

/// Serialize an RDF literal in SPARQL 1.1 syntax, enforcing `limit` before scanning or allocating.
fn write_literal_ref_max(out: &mut String, lit: LiteralRef<'_>, limit: usize) -> Result<()> {
    let val = lit.value();
    if out.len().saturating_add(val.len()).saturating_add(2) > limit {
        return Err(limit_err(format!(
            "serialized SPIN query exceeds maximum output size ({MAX_OUTPUT_BYTES} bytes)"
        )));
    }
    push_str_bounded_max(out, "\"", limit)?;
    let mut last = 0usize;
    for (idx, c) in val.char_indices() {
        let esc = match c {
            '\\' => "\\\\",
            '"' => "\\\"",
            '\n' => "\\n",
            '\r' => "\\r",
            '\t' => "\\t",
            '\u{08}' => "\\b",
            '\u{0C}' => "\\f",
            _ => continue,
        };
        if idx > last {
            push_str_bounded_max(out, &val[last..idx], limit)?;
        }
        push_str_bounded_max(out, esc, limit)?;
        last = idx + c.len_utf8();
    }
    if last < val.len() {
        push_str_bounded_max(out, &val[last..], limit)?;
    }
    push_str_bounded_max(out, "\"", limit)?;
    if let Some(lang) = lit.language() {
        validate_lang_tag(lang)?;
        push_str_bounded_max(out, "@", limit)?;
        push_str_bounded_max(out, lang, limit)?;
    } else if lit.datatype() != xsd::STRING {
        push_str_bounded_max(out, "^^", limit)?;
        write_iri_max(out, lit.datatype().as_str(), limit)?;
    }
    Ok(())
}

/// Serialize a borrowed ground RDF term (IRI, literal, or RDF 1.2 triple term `<<( ... )>>`)
/// into SPARQL syntax under [`MAX_DEPTH`] and `limit`.
fn write_term_ref_value_max(
    out: &mut String,
    term: TermRef<'_>,
    depth: usize,
    limit: usize,
) -> Result<()> {
    if depth > MAX_DEPTH {
        return Err(limit_err(format!(
            "embedded RDF-star triple term exceeds maximum nesting depth ({MAX_DEPTH})"
        )));
    }
    match term {
        TermRef::NamedNode(n) => write_iri_max(out, n.as_str(), limit),
        TermRef::Literal(l) => write_literal_ref_max(out, l, limit),
        TermRef::BlankNode(_) => {
            bail!("cannot serialize blank node {term} as a constant SPARQL term")
        }
        TermRef::Triple(t) => {
            push_str_bounded_max(out, "<<( ", limit)?;
            write_term_ref_value_max(out, TermRef::from(t.subject.as_ref()), depth + 1, limit)?;
            push_str_bounded_max(out, " ", limit)?;
            write_iri_max(out, t.predicate.as_str(), limit)?;
            push_str_bounded_max(out, " ", limit)?;
            write_term_ref_value_max(out, t.object.as_ref(), depth + 1, limit)?;
            push_str_bounded_max(out, " )>>", limit)
        }
    }
}

/// Serialize a ground RDF term (IRI, literal, or RDF 1.2 triple term) into SPARQL syntax
/// under [`MAX_DEPTH`].
pub(crate) fn write_term_value(out: &mut String, term: &Term, depth: usize) -> Result<()> {
    write_term_ref_value_max(out, term.as_ref(), depth, MAX_OUTPUT_BYTES)
}

/// Return the SPARQL variable name if `node` denotes a named SPIN variable (`spin:_this`,
/// `spin:_arg<N>`, or `sp:varName`). Intrinsic SPIN variables cannot be renamed by
/// conflicting `sp:varName` metadata.
pub(crate) fn spin_var_name(
    g: &G<'_>,
    node: &Term,
    budget: &mut WorkBudget,
) -> Result<Option<String>> {
    if let Term::NamedNode(n) = node {
        let intrinsic = if n.as_ref() == spin::THIS {
            budget.charge_bytes(4)?;
            Some("this".to_string())
        } else if let Some(rest) = n.as_str().strip_prefix("http://spinrdf.org/spin#_arg")
            && !rest.is_empty()
        {
            let expected_len = 3usize.saturating_add(rest.len());
            if expected_len > MAX_OUTPUT_BYTES {
                return Err(limit_err(format!(
                    "SPIN variable name exceeds maximum output size ({MAX_OUTPUT_BYTES} bytes)"
                )));
            }
            budget.charge_bytes(expected_len)?;
            if rest.bytes().all(|b| b.is_ascii_digit()) {
                Some(format!("arg{rest}"))
            } else {
                None
            }
        } else {
            None
        };
        if let Some(expected) = intrinsic {
            match single_object_ref(g, node, sp::VAR_NAME, budget)? {
                None => return Ok(Some(expected)),
                Some(TermRef::Literal(lit)) if lit.value() == expected => {
                    return Ok(Some(expected));
                }
                Some(other) => bail!(
                    "intrinsic SPIN variable {node} cannot be renamed with conflicting sp:varName {other}"
                ),
            }
        }
    }
    match single_object_ref(g, node, sp::VAR_NAME, budget)? {
        None => Ok(None),
        Some(TermRef::Literal(lit)) => {
            let name = lit.value();
            budget.charge_bytes(name.len())?;
            validate_var_name(name)
                .with_context(|| format!("invalid sp:varName on SPIN variable {node}"))?;
            Ok(Some(name.to_string()))
        }
        Some(other) => bail!("sp:varName of {node} must be a string literal, found {other}"),
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum QueryKind {
    Construct,
    Select,
    Ask,
    Describe,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum NodePos {
    Subject,
    Predicate,
    Object,
}

struct OutputCheckpoint {
    segments_len: usize,
    segments_bytes: usize,
    out_len: usize,
    anon_spans_len: usize,
}

struct Serializer<'g, 'b, 'p, 'k, 'w> {
    g: G<'g>,
    bindings: &'b FxHashMap<String, Option<Term>>,
    known_prefixes: Option<&'p KnownPrefixes<'k>>,
    budget: &'w mut WorkBudget,
    /// Completed owned output segments preceding `out`, moved without copying when
    /// joining buffered `SELECT` `WHERE` clauses.
    segments: Vec<String>,
    segments_bytes: usize,
    out: String,
    reserved_out_bytes: usize,
    #[cfg(test)]
    peak_serialization_output_bytes: usize,
    #[cfg(test)]
    peak_live_output_bytes: usize,
    ancestors: FxHashSet<Term>,
    /// Anonymous blank-node variable -> sequential id `0..anon_vars.len()`.
    anon_vars: FxHashMap<BlankNode, usize>,
    /// Logical byte spans `(name_start, name_end, anon_id)` of provisionally emitted
    /// `_spin_v{id+1}` names across `segments` + `out`, rewritten once at the end only
    /// if any name collides with query vars.
    anon_var_spans: Vec<(usize, usize, usize)>,
    template_bnodes: FxHashMap<BlankNode, String>,
    /// Variable names seen anywhere in this query (named SPIN vars, `sp:Values`, `sp:SubQuery` `sp:text`).
    used_var_names: FxHashSet<String>,
}

impl Serializer<'_, '_, '_, '_, '_> {
    fn logical_out_len(&self) -> usize {
        self.segments_bytes.saturating_add(self.out.len())
    }

    fn output_limit(&self) -> usize {
        MAX_OUTPUT_BYTES
            .saturating_sub(self.segments_bytes)
            .saturating_sub(self.reserved_out_bytes)
    }

    fn record_live_output(&mut self) {
        #[cfg(test)]
        {
            let live = self
                .segments_bytes
                .saturating_add(self.out.len())
                .saturating_add(self.reserved_out_bytes);
            if live > self.peak_serialization_output_bytes {
                self.peak_serialization_output_bytes = live;
            }
            if live > self.peak_live_output_bytes {
                self.peak_live_output_bytes = live;
            }
        }
    }

    fn checkpoint_output(&self) -> OutputCheckpoint {
        OutputCheckpoint {
            segments_len: self.segments.len(),
            segments_bytes: self.segments_bytes,
            out_len: self.out.len(),
            anon_spans_len: self.anon_var_spans.len(),
        }
    }

    fn rollback_output(&mut self, cp: OutputCheckpoint) {
        if cp.out_len == 0 {
            self.segments.truncate(cp.segments_len);
            self.segments_bytes = cp.segments_bytes;
            self.out.clear();
        } else if self.segments.len() > cp.segments_len {
            // A nested SELECT flushed the original `out` buffer into `segments[cp.segments_len]`.
            self.segments.truncate(cp.segments_len + 1);
            self.out = self.segments.pop().unwrap_or_default();
            self.out.truncate(cp.out_len);
            self.segments_bytes = cp.segments_bytes;
        } else {
            self.out.truncate(cp.out_len);
        }
        self.anon_var_spans.truncate(cp.anon_spans_len);
    }

    fn push(&mut self, s: &str) -> Result<()> {
        let limit = self.output_limit();
        push_str_bounded_max(&mut self.out, s, limit)?;
        self.record_live_output();
        Ok(())
    }

    fn push_iri(&mut self, iri: &str) -> Result<()> {
        let limit = self.output_limit();
        write_iri_max(&mut self.out, iri, limit)?;
        self.record_live_output();
        Ok(())
    }

    fn push_literal_ref(&mut self, lit: LiteralRef<'_>) -> Result<()> {
        let limit = self.output_limit();
        write_literal_ref_max(&mut self.out, lit, limit)?;
        self.record_live_output();
        Ok(())
    }

    fn push_term_value(&mut self, term: &Term, depth: usize) -> Result<()> {
        let limit = self.output_limit();
        write_term_ref_value_max(&mut self.out, term.as_ref(), depth, limit)?;
        self.record_live_output();
        Ok(())
    }

    fn count_node(&mut self) -> Result<()> {
        self.budget.step()
    }

    fn record_var(&mut self, name: &str) -> Result<()> {
        if !self.used_var_names.contains(name) {
            self.budget.charge_bytes(name.len())?;
            self.used_var_names.insert(name.to_string());
        }
        Ok(())
    }

    fn bound_term(&self, name: &str) -> Option<&Term> {
        if name == "this" {
            return None;
        }
        self.bindings.get(name).and_then(Option::as_ref)
    }

    fn is_bound(&self, name: &str) -> bool {
        self.bound_term(name).is_some()
    }

    fn var_name(&mut self, node: &Term) -> Result<Option<String>> {
        let name = spin_var_name(&self.g, node, self.budget)?;
        if let Some(ref n) = name {
            self.record_var(n)?;
        }
        Ok(name)
    }

    fn is_instance(&mut self, node: &Term, class: NamedNodeRef<'_>) -> Result<bool> {
        is_instance_bounded(&self.g, node, class, self.budget)
    }

    fn with_node<T>(
        &mut self,
        node: &Term,
        depth: usize,
        kind: &str,
        f: impl FnOnce(&mut Self) -> Result<T>,
    ) -> Result<T> {
        if depth > MAX_DEPTH {
            return Err(limit_err(format!(
                "SPIN {kind} exceeds maximum nesting depth ({MAX_DEPTH}) at {node}"
            )));
        }
        self.count_node()?;
        if !self.ancestors.insert(node.clone()) {
            return Err(limit_err(format!("cyclic SPIN {kind} at {node}")));
        }
        let res = f(self);
        self.ancestors.remove(node);
        res
    }

    fn single_object(&mut self, node: &Term, pred: NamedNodeRef<'_>) -> Result<Option<Term>> {
        single_object(&self.g, node, pred, self.budget)
    }

    fn required_object(&mut self, node: &Term, pred: NamedNodeRef<'_>, what: &str) -> Result<Term> {
        self.single_object(node, pred)?
            .ok_or_else(|| anyhow!("{what} {node} is missing <{}>", pred.as_str()))
    }

    fn optional_bool(&mut self, node: &Term, pred: NamedNodeRef<'_>) -> Result<bool> {
        match single_object_ref(&self.g, node, pred, self.budget)? {
            None => Ok(false),
            Some(t) => parse_spin_bool_ref(t, &format!("SPIN node {node} <{}>", pred.as_str())),
        }
    }

    fn optional_u64(&mut self, node: &Term, pred: NamedNodeRef<'_>) -> Result<Option<u64>> {
        match single_object_ref(&self.g, node, pred, self.budget)? {
            None => Ok(None),
            Some(TermRef::Literal(lit)) => {
                let v = lit.value().trim().parse::<u64>().map_err(|_| {
                    anyhow!(
                        "SPIN node {node} expects a non-negative integer for <{}>, found {lit}",
                        pred.as_str()
                    )
                })?;
                Ok(Some(v))
            }
            Some(other) => bail!(
                "SPIN node {node} expects a non-negative integer for <{}>, found {other}",
                pred.as_str()
            ),
        }
    }

    fn optional_i64(&mut self, node: &Term, pred: NamedNodeRef<'_>) -> Result<Option<i64>> {
        match single_object_ref(&self.g, node, pred, self.budget)? {
            None => Ok(None),
            Some(TermRef::Literal(lit)) => {
                let v = lit.value().trim().parse::<i64>().map_err(|_| {
                    anyhow!(
                        "SPIN node {node} expects an integer for <{}>, found {lit}",
                        pred.as_str()
                    )
                })?;
                Ok(Some(v))
            }
            Some(other) => bail!(
                "SPIN node {node} expects an integer for <{}>, found {other}",
                pred.as_str()
            ),
        }
    }

    fn read_list(&mut self, head: &Term, what: &str) -> Result<Vec<Term>> {
        let mut items = Vec::new();
        let mut cur = head.clone();
        let mut seen = FxHashSet::default();
        loop {
            if let Term::NamedNode(n) = &cur
                && n.as_ref() == rdf::NIL
            {
                return Ok(items);
            }
            self.count_node()?;
            if !seen.insert(cur.clone()) {
                return Err(limit_err(format!("cyclic RDF list in {what} at {head}")));
            }
            if items.len() >= MAX_LIST_LEN {
                return Err(limit_err(format!(
                    "RDF list in {what} exceeds maximum length ({MAX_LIST_LEN}) at {head}"
                )));
            }
            let first = self.single_object(&cur, rdf::FIRST)?.ok_or_else(|| {
                anyhow!("malformed RDF list in {what} at {head}: missing rdf:first")
            })?;
            let rest = self.single_object(&cur, rdf::REST)?.ok_or_else(|| {
                anyhow!("malformed RDF list in {what} at {head}: missing rdf:rest")
            })?;
            items.push(first);
            cur = rest;
        }
    }

    fn write_anon_var(&mut self, b: &BlankNode) -> Result<()> {
        let id = if let Some(&id) = self.anon_vars.get(b) {
            id
        } else {
            let id = self.anon_vars.len();
            self.anon_vars.insert(b.clone(), id);
            id
        };
        self.push("?")?;
        let start = self.logical_out_len();
        let provisional = format!("_spin_v{}", id + 1);
        self.push(&provisional)?;
        let end = self.logical_out_len();
        self.anon_var_spans.push((start, end, id));
        Ok(())
    }

    /// Flatten `self.segments` and `self.out` into one owned UTF-8 byte buffer, reserving
    /// `extra_capacity` bytes in the same `reserve_exact` call when anonymous-variable
    /// collision rewriting will expand the buffer in-place.
    ///
    /// Reuses the largest owned segment buffer in-place so flattening a small `SELECT` header
    /// onto a large `WHERE` buffer never allocates a second copy of the dominant segment. For
    /// multiple large segments whose sum fits in `MAX_OUTPUT_BYTES`, the destination buffer
    /// and remaining unconsumed segments briefly coexist for `final_len + (total - max_len)`
    /// logical bytes (`<= MAX_RETAINED_BYTES`). Note that `reserve_exact` on the surviving
    /// buffer may still perform an allocator-internal moving `realloc`, which the logical
    /// byte counter cannot observe.
    fn flatten_segments_into_bytes(&mut self, extra_capacity: usize) -> Vec<u8> {
        let out = std::mem::take(&mut self.out);
        if self.segments.is_empty() {
            let mut bytes = out.into_bytes();
            if extra_capacity > 0 {
                bytes.reserve_exact(extra_capacity);
            }
            #[cfg(test)]
            {
                let live = bytes.len().saturating_add(extra_capacity);
                if live > self.peak_live_output_bytes {
                    self.peak_live_output_bytes = live;
                }
            }
            return bytes;
        }
        let mut parts = std::mem::take(&mut self.segments);
        parts.push(out);
        let total = self
            .segments_bytes
            .saturating_add(parts.last().map_or(0, String::len));
        self.segments_bytes = 0;

        let mut max_idx = 0usize;
        let mut max_len = 0usize;
        for (i, seg) in parts.iter().enumerate() {
            if seg.len() > max_len {
                max_len = seg.len();
                max_idx = i;
            }
        }
        let prefix_len: usize = parts[..max_idx].iter().map(String::len).sum();
        let mut bytes = std::mem::take(&mut parts[max_idx]).into_bytes();
        let orig_len = bytes.len();
        let target_cap = total.saturating_add(extra_capacity);
        if target_cap > orig_len {
            bytes.reserve_exact(target_cap - orig_len);
        }
        let mut remaining_parts_bytes = total.saturating_sub(orig_len);
        #[cfg(test)]
        {
            let live = target_cap.saturating_add(remaining_parts_bytes);
            if live > self.peak_live_output_bytes {
                self.peak_live_output_bytes = live;
            }
        }
        if prefix_len > 0 {
            bytes.resize(prefix_len + orig_len, 0);
            bytes.copy_within(0..orig_len, prefix_len);
            let mut pos = 0usize;
            for seg in &mut parts[..max_idx] {
                let s = std::mem::take(seg);
                let len = s.len();
                bytes[pos..pos + len].copy_from_slice(s.as_bytes());
                pos += len;
                remaining_parts_bytes = remaining_parts_bytes.saturating_sub(len);
            }
        }
        for seg in &mut parts[max_idx + 1..] {
            let s = std::mem::take(seg);
            let len = s.len();
            bytes.extend_from_slice(s.as_bytes());
            remaining_parts_bytes = remaining_parts_bytes.saturating_sub(len);
        }
        debug_assert_eq!(remaining_parts_bytes, 0);
        debug_assert_eq!(bytes.len(), total);
        bytes
    }

    fn resolve_anon_var_collisions(&mut self) -> Result<String> {
        if self.anon_vars.is_empty() {
            let bytes = self.flatten_segments_into_bytes(0);
            return Ok(String::from_utf8(bytes).expect("SPIN output is valid UTF-8"));
        }
        let count = self.anon_vars.len();
        let has_collision = (0..count).any(|id| {
            let name = format!("_spin_v{}", id + 1);
            self.used_var_names.contains(&name) || self.bindings.contains_key(&name)
        });
        if !has_collision {
            let bytes = self.flatten_segments_into_bytes(0);
            return Ok(String::from_utf8(bytes).expect("SPIN output is valid UTF-8"));
        }
        let mut final_names = Vec::with_capacity(count);
        let mut next_n = 1usize;
        for _ in 0..count {
            loop {
                let cand = format!("_spin_v{next_n}");
                next_n += 1;
                if !self.used_var_names.contains(&cand) && !self.bindings.contains_key(&cand) {
                    self.used_var_names.insert(cand.clone());
                    final_names.push(cand);
                    break;
                }
            }
        }
        let total = self.logical_out_len();
        let mut final_len = total;
        for &(start, end, id) in &self.anon_var_spans {
            let orig = end - start;
            let rep = final_names[id].len();
            debug_assert!(rep >= orig);
            final_len = final_len.saturating_add(rep - orig);
        }
        if final_len > MAX_OUTPUT_BYTES {
            return Err(limit_err(format!(
                "serialized SPIN query exceeds maximum output size ({MAX_OUTPUT_BYTES} bytes)"
            )));
        }
        let mut bytes = self.flatten_segments_into_bytes(final_len - total);
        if final_len > total {
            bytes.resize(final_len, 0);
        }
        #[cfg(test)]
        if final_len > self.peak_live_output_bytes {
            self.peak_live_output_bytes = final_len;
        }
        // Rewrite colliding anonymous variable names in-place from right to left so
        // finalization never allocates a second copy of the output buffer.
        let mut read_end = total;
        let mut write_end = final_len;
        for (start, end, id) in std::mem::take(&mut self.anon_var_spans).into_iter().rev() {
            let tail_len = read_end - end;
            write_end -= tail_len;
            if tail_len > 0 && write_end != end {
                bytes.copy_within(end..read_end, write_end);
            }
            let rep = final_names[id].as_bytes();
            write_end -= rep.len();
            bytes[write_end..write_end + rep.len()].copy_from_slice(rep);
            read_end = start;
        }
        debug_assert_eq!(read_end, write_end);
        Ok(String::from_utf8(bytes).expect("SPIN output is valid UTF-8"))
    }

    fn template_bnode_label(&mut self, b: &BlankNode) -> String {
        if let Some(label) = self.template_bnodes.get(b) {
            return label.clone();
        }
        let label = format!("spin_b{}", self.template_bnodes.len() + 1);
        self.template_bnodes.insert(b.clone(), label.clone());
        label
    }

    fn query_kind(&mut self, query: &Term) -> Result<QueryKind> {
        let mut kinds = Vec::new();
        if self.is_instance(query, sp::CONSTRUCT)? {
            kinds.push(QueryKind::Construct);
        }
        if self.is_instance(query, sp::SELECT)? {
            kinds.push(QueryKind::Select);
        }
        if self.is_instance(query, sp::ASK)? {
            kinds.push(QueryKind::Ask);
        }
        if self.is_instance(query, sp::DESCRIBE)? {
            kinds.push(QueryKind::Describe);
        }
        match kinds.as_slice() {
            [k] => Ok(*k),
            [] => {
                if self.g.has(query, sp::TEMPLATES) {
                    Ok(QueryKind::Construct)
                } else if self.g.has(query, sp::RESULT_VARIABLES) {
                    Ok(QueryKind::Select)
                } else if self.g.has(query, sp::RESULT_NODES) {
                    Ok(QueryKind::Describe)
                } else {
                    bail!("cannot determine SPIN query form of {query}")
                }
            }
            _ => bail!("SPIN query {query} has multiple conflicting query types"),
        }
    }

    fn expr_contains_aggregate(&mut self, node: &Term, depth: usize) -> Result<bool> {
        self.with_node(node, depth, "expression", |this| {
            let args = this.collect_args(node)?;
            if let Some(ty) = this.expr_call_type(node, &args)?
                && aggregate_fn(ty.as_str()).is_some()
            {
                return Ok(true);
            }
            if let Some(expr) = this.single_object(node, sp::EXPRESSION)?
                && this.expr_contains_aggregate(&expr, depth + 1)?
            {
                return Ok(true);
            }
            for arg in &args {
                if this.expr_contains_aggregate(arg, depth + 1)? {
                    return Ok(true);
                }
            }
            Ok(false)
        })
    }

    fn select_visible_bound_vars(
        &mut self,
        query: &Term,
        body_exported: &FxHashSet<String>,
        proj_items: &[Term],
        depth: usize,
    ) -> Result<FxHashSet<String>> {
        if body_exported.is_empty() {
            return Ok(FxHashSet::default());
        }
        if let Some(head) = self.single_object(query, sp::GROUP_BY)? {
            let items = self.read_list(&head, "sp:groupBy")?;
            if !items.is_empty() {
                let mut grouped = FxHashSet::default();
                for item in &items {
                    if self.g.has(item, sp::AS) {
                        continue;
                    }
                    if let Some(expr) = self.single_object(item, sp::EXPRESSION)? {
                        if self.var_name(item)?.is_none()
                            && let Some(name) = spin_var_name(&self.g, &expr, self.budget)?
                            && body_exported.contains(&name)
                        {
                            grouped.insert(name);
                        }
                    } else if let Some(name) = spin_var_name(&self.g, item, self.budget)?
                        && body_exported.contains(&name)
                    {
                        grouped.insert(name);
                    }
                }
                return Ok(grouped);
            }
        }
        for item in proj_items {
            if self.g.has(item, sp::AS) {
                if self.expr_contains_aggregate(item, depth + 1)? {
                    return Ok(FxHashSet::default());
                }
            } else if let Some(expr) = self.single_object(item, sp::EXPRESSION)?
                && self.expr_contains_aggregate(&expr, depth + 1)?
            {
                return Ok(FxHashSet::default());
            }
        }
        if let Some(head) = self.single_object(query, sp::HAVING)? {
            let items = self.read_list(&head, "sp:having")?;
            for item in &items {
                if self.expr_contains_aggregate(item, depth + 1)? {
                    return Ok(FxHashSet::default());
                }
            }
        }
        Ok(body_exported.clone())
    }

    fn write_query(
        &mut self,
        query: &Term,
        depth: usize,
        is_subquery: bool,
    ) -> Result<FxHashSet<String>> {
        self.with_node(query, depth, "query", |this| {
            let kind = this.query_kind(query)?;
            if is_subquery {
                if kind != QueryKind::Select {
                    bail!("sp:SubQuery {query} must be a sp:Select query");
                }
                if this.g.has(query, sp::FROM) || this.g.has(query, sp::FROM_NAMED) {
                    bail!("sp:SubQuery {query} cannot declare sp:from or sp:fromNamed");
                }
            }
            match kind {
                QueryKind::Construct => {
                    let templates_head =
                        this.required_object(query, sp::TEMPLATES, "sp:Construct")?;
                    let templates = this.read_list(&templates_head, "sp:templates")?;
                    let where_head = this.required_object(query, sp::WHERE, "sp:Construct")?;
                    this.push("CONSTRUCT { ")?;
                    for t in &templates {
                        this.write_triple_pattern(t, true, depth + 1)?;
                        this.push(" ")?;
                    }
                    this.push("}")?;
                    this.write_dataset_clauses(query)?;
                    this.push(" WHERE { ")?;
                    let body_exported = this.write_element_list(&where_head, depth + 1)?;
                    this.push(" }")?;
                    this.write_solution_modifiers(query, depth + 1, &body_exported)?;
                    Ok(FxHashSet::default())
                }
                QueryKind::Select => {
                    let distinct = this.optional_bool(query, sp::DISTINCT)?;
                    let reduced = this.optional_bool(query, sp::REDUCED)?;
                    if distinct && reduced {
                        bail!(
                            "SPIN SELECT query {query} cannot set both sp:distinct and sp:reduced"
                        );
                    }
                    let where_head = this.required_object(query, sp::WHERE, "sp:Select")?;
                    let proj_vars = match this.single_object(query, sp::RESULT_VARIABLES)? {
                        None => Vec::new(),
                        Some(head) => this.read_list(&head, "sp:resultVariables")?,
                    };
                    if this.bindings.is_empty() {
                        let empty = FxHashSet::default();
                        this.push("SELECT ")?;
                        if distinct {
                            this.push("DISTINCT ")?;
                        } else if reduced {
                            this.push("REDUCED ")?;
                        }
                        if proj_vars.is_empty() {
                            this.push("*")?;
                        } else {
                            for (i, v) in proj_vars.iter().enumerate() {
                                if i > 0 {
                                    this.push(" ")?;
                                }
                                this.write_projection_item(v, depth + 1, &empty)?;
                            }
                        }
                        if !is_subquery {
                            this.write_dataset_clauses(query)?;
                        }
                        this.push(" WHERE { ")?;
                        this.write_element_list(&where_head, depth + 1)?;
                        this.push(" }")?;
                        this.write_solution_modifiers(query, depth + 1, &empty)?;
                        Ok(FxHashSet::default())
                    } else {
                        // Serialize WHERE into owned segments first so `body_exported`
                        // reflects the exact subquery candidates emitted (including any
                        // dual-form `sp:text` fallbacks), then move the owned segments
                        // into place without duplicating `where_buf` across live buffers.
                        let saved_segments = std::mem::take(&mut this.segments);
                        let saved_segments_bytes = std::mem::take(&mut this.segments_bytes);
                        let saved_out = std::mem::take(&mut this.out);
                        let saved_spans = std::mem::take(&mut this.anon_var_spans);
                        let saved_len = saved_segments_bytes.saturating_add(saved_out.len());
                        this.reserved_out_bytes = this.reserved_out_bytes.saturating_add(saved_len);
                        let where_res = this.write_element_list(&where_head, depth + 1);
                        let where_segments = std::mem::replace(&mut this.segments, saved_segments);
                        let where_segments_bytes =
                            std::mem::replace(&mut this.segments_bytes, saved_segments_bytes);
                        let where_out = std::mem::replace(&mut this.out, saved_out);
                        let where_spans = std::mem::replace(&mut this.anon_var_spans, saved_spans);
                        this.reserved_out_bytes = this.reserved_out_bytes.saturating_sub(saved_len);
                        let body_exported = where_res?;

                        let visible_bound = this.select_visible_bound_vars(
                            query,
                            &body_exported,
                            &proj_vars,
                            depth + 1,
                        )?;

                        let where_len = where_segments_bytes.saturating_add(where_out.len());
                        this.reserved_out_bytes = this.reserved_out_bytes.saturating_add(where_len);
                        let header_res = (|| -> Result<FxHashSet<String>> {
                            this.push("SELECT ")?;
                            if distinct {
                                this.push("DISTINCT ")?;
                            } else if reduced {
                                this.push("REDUCED ")?;
                            }
                            let mut select_exported = FxHashSet::default();
                            if proj_vars.is_empty() {
                                this.push("*")?;
                                select_exported = visible_bound.clone();
                            } else {
                                for (i, v) in proj_vars.iter().enumerate() {
                                    if i > 0 {
                                        this.push(" ")?;
                                    }
                                    if let Some(bound_name) =
                                        this.write_projection_item(v, depth + 1, &visible_bound)?
                                    {
                                        select_exported.insert(bound_name);
                                    }
                                }
                            }
                            if !is_subquery {
                                this.write_dataset_clauses(query)?;
                            }
                            this.push(" WHERE { ")?;
                            Ok(select_exported)
                        })();
                        this.reserved_out_bytes = this.reserved_out_bytes.saturating_sub(where_len);
                        let select_exported = header_res?;

                        let base = this.logical_out_len();
                        if !this.out.is_empty() {
                            this.segments_bytes =
                                this.segments_bytes.saturating_add(this.out.len());
                            this.segments.push(std::mem::take(&mut this.out));
                        }
                        if !where_segments.is_empty() {
                            this.segments_bytes =
                                this.segments_bytes.saturating_add(where_segments_bytes);
                            this.segments.extend(where_segments);
                        }
                        this.out = where_out;
                        this.record_live_output();
                        for (s, e, id) in where_spans {
                            this.anon_var_spans.push((base + s, base + e, id));
                        }
                        this.push(" }")?;
                        this.write_solution_modifiers(query, depth + 1, &body_exported)?;
                        Ok(select_exported)
                    }
                }
                QueryKind::Ask => {
                    this.push("ASK")?;
                    this.write_dataset_clauses(query)?;
                    let where_head = this.required_object(query, sp::WHERE, "sp:Ask")?;
                    this.push(" WHERE { ")?;
                    let body_exported = this.write_element_list(&where_head, depth + 1)?;
                    this.push(" }")?;
                    this.write_solution_modifiers(query, depth + 1, &body_exported)?;
                    Ok(FxHashSet::default())
                }
                QueryKind::Describe => {
                    this.push("DESCRIBE ")?;
                    match this.single_object(query, sp::RESULT_NODES)? {
                        None => this.push("*")?,
                        Some(head) => {
                            let nodes = this.read_list(&head, "sp:resultNodes")?;
                            if nodes.is_empty() {
                                this.push("*")?;
                            } else {
                                for (i, n) in nodes.iter().enumerate() {
                                    if i > 0 {
                                        this.push(" ")?;
                                    }
                                    this.write_var_or_iri(n, "sp:resultNodes")?;
                                }
                            }
                        }
                    }
                    this.write_dataset_clauses(query)?;
                    let where_head = this.single_object(query, sp::WHERE)?;
                    let body_exported = if let Some(wh) = where_head {
                        this.push(" WHERE { ")?;
                        let exported = this.write_element_list(&wh, depth + 1)?;
                        this.push(" }")?;
                        exported
                    } else {
                        FxHashSet::default()
                    };
                    this.write_solution_modifiers(query, depth + 1, &body_exported)?;
                    Ok(FxHashSet::default())
                }
            }
        })
    }

    fn write_dataset_clauses(&mut self, query: &Term) -> Result<()> {
        let Some(sub) = as_subject(query) else {
            return Ok(());
        };
        let mut from = Vec::new();
        for g in self.g.g.objects_for_subject_predicate(sub, sp::FROM) {
            self.count_node()?;
            let TermRef::NamedNode(iri) = g else {
                bail!("sp:from of {query} must be an IRI");
            };
            from.push(check_and_own_iri(iri, self.budget)?);
        }
        from.sort_by(|a, b| a.as_str().cmp(b.as_str()));
        for iri in from {
            self.push(" FROM ")?;
            self.push_iri(iri.as_str())?;
        }
        let mut from_named = Vec::new();
        for g in self.g.g.objects_for_subject_predicate(sub, sp::FROM_NAMED) {
            self.count_node()?;
            let TermRef::NamedNode(iri) = g else {
                bail!("sp:fromNamed of {query} must be an IRI");
            };
            from_named.push(check_and_own_iri(iri, self.budget)?);
        }
        from_named.sort_by(|a, b| a.as_str().cmp(b.as_str()));
        for iri in from_named {
            self.push(" FROM NAMED ")?;
            self.push_iri(iri.as_str())?;
        }
        Ok(())
    }

    fn write_projection_item(
        &mut self,
        item: &Term,
        depth: usize,
        visible_bound: &FxHashSet<String>,
    ) -> Result<Option<String>> {
        // Direct aggregate or expression projection with `sp:as` (W3C SPIN SPARQL Syntax §2.3),
        // e.g. `[ a sp:Count ; sp:as [ sp:varName "n" ] ; sp:expression [ sp:varName "o" ] ]`.
        if let Some(as_var) = self.single_object(item, sp::AS)? {
            if self.var_name(item)?.is_some() {
                bail!(
                    "projected expression {item} in sp:resultVariables cannot set both sp:as and sp:varName"
                );
            }
            let Some(name) = self.var_name(&as_var)? else {
                bail!("sp:as target {as_var} in sp:resultVariables is missing sp:varName");
            };
            if self.is_bound(&name) {
                bail!("projected expression assigns to bound template argument ?{name}");
            }
            self.push("(")?;
            self.write_expr(item, depth + 1)?;
            self.push(" AS ?")?;
            self.push(&name)?;
            self.push(")")?;
            return Ok(None);
        }
        if let Some(expr) = self.single_object(item, sp::EXPRESSION)? {
            let Some(name) = self.var_name(item)? else {
                bail!("projected expression {item} in sp:resultVariables is missing sp:varName");
            };
            if self.is_bound(&name) {
                bail!("projected expression assigns to bound template argument ?{name}");
            }
            self.push("(")?;
            self.write_expr(&expr, depth + 1)?;
            self.push(" AS ?")?;
            self.push(&name)?;
            self.push(")")?;
            return Ok(None);
        }
        if let Some(name) = self.var_name(item)? {
            if let Some(bound) = self.bound_term(&name).cloned() {
                if !visible_bound.contains(&name) {
                    self.push("(")?;
                    self.push_term_value(&bound, depth + 1)?;
                    self.push(" AS ?")?;
                    self.push(&name)?;
                    self.push(")")?;
                } else {
                    self.push("?")?;
                    self.push(&name)?;
                }
                return Ok(Some(name));
            }
            self.push("?")?;
            self.push(&name)?;
            return Ok(None);
        }
        bail!("invalid sp:resultVariables entry {item}: expected variable or projected expression")
    }

    fn write_solution_modifiers(
        &mut self,
        query: &Term,
        depth: usize,
        body_exported: &FxHashSet<String>,
    ) -> Result<()> {
        if let Some(head) = self.single_object(query, sp::GROUP_BY)? {
            let items = self.read_list(&head, "sp:groupBy")?;
            if !items.is_empty() {
                self.push(" GROUP BY")?;
                for item in &items {
                    self.push(" ")?;
                    if let Some(as_var) = self.single_object(item, sp::AS)? {
                        if self.var_name(item)?.is_some() {
                            bail!("GROUP BY item {item} cannot set both sp:as and sp:varName");
                        }
                        let Some(name) = self.var_name(&as_var)? else {
                            bail!("sp:as target {as_var} in sp:groupBy is missing sp:varName");
                        };
                        if self.is_bound(&name) {
                            bail!("GROUP BY assigns to bound template argument ?{name}");
                        }
                        self.push("(")?;
                        self.write_expr(item, depth + 1)?;
                        self.push(" AS ?")?;
                        self.push(&name)?;
                        self.push(")")?;
                    } else if let Some(expr) = self.single_object(item, sp::EXPRESSION)? {
                        let item_var = self.var_name(item)?;
                        if item_var.is_none()
                            && let Some(name) = self.var_name(&expr)?
                            && (!self.is_bound(&name) || body_exported.contains(&name))
                        {
                            self.push("(?")?;
                            self.push(&name)?;
                            self.push(")")?;
                        } else {
                            self.push("(")?;
                            self.write_expr(&expr, depth + 1)?;
                            if let Some(name) = item_var {
                                if self.is_bound(&name) {
                                    bail!("GROUP BY assigns to bound template argument ?{name}");
                                }
                                self.push(" AS ?")?;
                                self.push(&name)?;
                            }
                            self.push(")")?;
                        }
                    } else if let Some(name) = self.var_name(item)?
                        && (!self.is_bound(&name) || body_exported.contains(&name))
                    {
                        self.push("?")?;
                        self.push(&name)?;
                    } else {
                        self.push("(")?;
                        self.write_expr(item, depth + 1)?;
                        self.push(")")?;
                    }
                }
            }
        }
        if let Some(head) = self.single_object(query, sp::HAVING)? {
            let items = self.read_list(&head, "sp:having")?;
            if !items.is_empty() {
                self.push(" HAVING")?;
                for item in &items {
                    self.push(" (")?;
                    self.write_expr(item, depth + 1)?;
                    self.push(")")?;
                }
            }
        }
        if let Some(head) = self.single_object(query, sp::ORDER_BY)? {
            let items = self.read_list(&head, "sp:orderBy")?;
            if !items.is_empty() {
                self.push(" ORDER BY")?;
                for item in &items {
                    self.push(" ")?;
                    if self.is_instance(item, sp::ASC)? {
                        let expr = self.required_object(item, sp::EXPRESSION, "sp:Asc")?;
                        self.push("ASC(")?;
                        self.write_expr(&expr, depth + 1)?;
                        self.push(")")?;
                    } else if self.is_instance(item, sp::DESC)? {
                        let expr = self.required_object(item, sp::EXPRESSION, "sp:Desc")?;
                        self.push("DESC(")?;
                        self.write_expr(&expr, depth + 1)?;
                        self.push(")")?;
                    } else if let Some(name) = self.var_name(item)?
                        && !self.is_bound(&name)
                    {
                        self.push("?")?;
                        self.push(&name)?;
                    } else {
                        self.push("(")?;
                        self.write_expr(item, depth + 1)?;
                        self.push(")")?;
                    }
                }
            }
        }
        if let Some(limit) = self.optional_u64(query, sp::LIMIT)? {
            self.push(&format!(" LIMIT {limit}"))?;
        }
        if let Some(offset) = self.optional_u64(query, sp::OFFSET)? {
            self.push(&format!(" OFFSET {offset}"))?;
        }
        if let Some(values) = self.single_object(query, sp::VALUES)? {
            self.push(" ")?;
            self.write_values(&values, depth + 1)?;
        }
        Ok(())
    }

    fn write_element_list(&mut self, head: &Term, depth: usize) -> Result<FxHashSet<String>> {
        let elements = self.read_list(head, "sp:where / sp:elements")?;
        let mut exported = FxHashSet::default();
        for (i, elem) in elements.iter().enumerate() {
            if i > 0 {
                self.push(" ")?;
            }
            exported.extend(self.write_element(elem, depth + 1)?);
        }
        Ok(exported)
    }

    fn write_element_block(&mut self, node: &Term, depth: usize) -> Result<FxHashSet<String>> {
        if matches!(node, Term::NamedNode(n) if n.as_ref() == rdf::NIL)
            || self.g.has(node, rdf::FIRST)
        {
            return self.write_element_list(node, depth);
        }
        if self.is_instance(node, sp::ELEMENT_GROUP)?
            && let Some(elems) = self.single_object(node, sp::ELEMENTS)?
        {
            return self.write_element_list(&elems, depth);
        }
        self.write_element(node, depth)
    }

    fn write_subquery_text(
        &mut self,
        inner: &Term,
        text_term: TermRef<'_>,
    ) -> Result<FxHashSet<String>> {
        let TermRef::Literal(lit) = text_term else {
            bail!("sp:text of subquery {inner} must be a string literal");
        };
        let raw = lit.value();
        if raw.len() > MAX_OUTPUT_BYTES {
            return Err(limit_err(format!(
                "SPIN sp:text exceeds maximum output size ({MAX_OUTPUT_BYTES} bytes)"
            )));
        }
        if self.bindings.is_empty() {
            validate_subquery_text(raw, self.known_prefixes, self.budget)?;
            collect_sparql_text_vars(raw, &mut self.used_var_names, self.budget)?;
            self.push(raw)?;
            self.push("\n")?;
            Ok(FxHashSet::default())
        } else {
            let (substituted, exported) =
                substitute_sparql_text_with_exported(raw, self.bindings, self.budget)?;
            validate_subquery_text(&substituted, self.known_prefixes, self.budget)?;
            collect_sparql_text_vars(&substituted, &mut self.used_var_names, self.budget)?;
            self.push(&substituted)?;
            self.push("\n")?;
            Ok(exported)
        }
    }

    fn write_element(&mut self, node: &Term, depth: usize) -> Result<FxHashSet<String>> {
        self.with_node(node, depth, "graph pattern", |this| {
            // Nested RDF list of elements represents a `{ ... }` group
            if matches!(node, Term::NamedNode(n) if n.as_ref() == rdf::NIL)
                || this.g.has(node, rdf::FIRST)
            {
                this.push("{ ")?;
                let exported = this.write_element_list(node, depth + 1)?;
                this.push(" }")?;
                return Ok(exported);
            }
            if this.is_instance(node, sp::FILTER)? {
                let expr = this.required_object(node, sp::EXPRESSION, "sp:Filter")?;
                this.push("FILTER (")?;
                this.write_expr(&expr, depth + 1)?;
                this.push(")")?;
                return Ok(FxHashSet::default());
            }
            if this.is_instance(node, sp::BIND)? || this.is_instance(node, sp::LET)? {
                let expr = this.required_object(node, sp::EXPRESSION, "sp:Bind")?;
                let var_node = this.required_object(node, sp::VARIABLE, "sp:Bind")?;
                let Some(name) = this.var_name(&var_node)? else {
                    bail!("sp:Bind {node} has sp:variable {var_node} without sp:varName");
                };
                if this.is_bound(&name) {
                    bail!("sp:Bind {node} assigns to bound template argument ?{name}");
                }
                this.push("BIND (")?;
                this.write_expr(&expr, depth + 1)?;
                this.push(" AS ?")?;
                this.push(&name)?;
                this.push(")")?;
                return Ok(FxHashSet::default());
            }
            if this.is_instance(node, sp::OPTIONAL)? {
                let elems = this.required_object(node, sp::ELEMENTS, "sp:Optional")?;
                this.push("OPTIONAL { ")?;
                let exported = this.write_element_list(&elems, depth + 1)?;
                this.push(" }")?;
                return Ok(exported);
            }
            if this.is_instance(node, sp::UNION)? {
                let elems = this.required_object(node, sp::ELEMENTS, "sp:Union")?;
                let branches = this.read_list(&elems, "sp:Union")?;
                if branches.is_empty() {
                    bail!("sp:Union {node} has no branches");
                }
                let mut exported = FxHashSet::default();
                for (i, branch) in branches.iter().enumerate() {
                    if i > 0 {
                        this.push(" UNION ")?;
                    }
                    this.push("{ ")?;
                    exported.extend(this.write_element_block(branch, depth + 1)?);
                    this.push(" }")?;
                }
                return Ok(exported);
            }
            if this.is_instance(node, sp::SUB_QUERY)? {
                let inner = this.required_object(node, sp::QUERY_PROP, "sp:SubQuery")?;
                this.push("{ ")?;
                let exported = if has_rdf_query_structure(&this.g, &inner) {
                    let cp = this.checkpoint_output();
                    match this.write_query(&inner, depth + 1, true) {
                        Ok(sub_exported) => sub_exported,
                        Err(rdf_err) => {
                            if !is_limit_or_cycle_error(&rdf_err)
                                && let Some(text_term) =
                                    single_object_ref(&this.g, &inner, sp::TEXT, this.budget)?
                            {
                                this.rollback_output(cp);
                                this.write_subquery_text(&inner, text_term)?
                            } else {
                                return Err(rdf_err);
                            }
                        }
                    }
                } else if let Some(text_term) =
                    single_object_ref(&this.g, &inner, sp::TEXT, this.budget)?
                {
                    this.write_subquery_text(&inner, text_term)?
                } else {
                    bail!(
                        "sp:SubQuery {node} points to {inner} which has neither SPIN RDF syntax nor sp:text"
                    );
                };
                this.push(" }")?;
                return Ok(exported);
            }
            if this.is_instance(node, sp::MINUS)? {
                let elems = this.required_object(node, sp::ELEMENTS, "sp:Minus")?;
                this.push("MINUS { ")?;
                this.write_element_list(&elems, depth + 1)?;
                this.push(" }")?;
                return Ok(FxHashSet::default());
            }
            if this.is_instance(node, sp::NAMED_GRAPH)? {
                let graph_node =
                    this.required_object(node, sp::GRAPH_NAME_NODE, "sp:NamedGraph")?;
                let elems = this.required_object(node, sp::ELEMENTS, "sp:NamedGraph")?;
                this.push("GRAPH ")?;
                this.write_var_or_iri(&graph_node, "sp:graphNameNode")?;
                this.push(" { ")?;
                let exported = this.write_element_list(&elems, depth + 1)?;
                this.push(" }")?;
                return Ok(exported);
            }
            if this.is_instance(node, sp::SERVICE)? {
                let uri = this.required_object(node, sp::SERVICE_URI, "sp:Service")?;
                let silent = this.optional_bool(node, sp::SILENT)?;
                let elems = this.required_object(node, sp::ELEMENTS, "sp:Service")?;
                this.push("SERVICE ")?;
                if silent {
                    this.push("SILENT ")?;
                }
                this.write_var_or_iri(&uri, "sp:serviceURI")?;
                this.push(" { ")?;
                let exported = this.write_element_list(&elems, depth + 1)?;
                this.push(" }")?;
                return Ok(exported);
            }
            if this.is_instance(node, sp::VALUES_CLS)? {
                this.write_values(node, depth + 1)?;
                return Ok(FxHashSet::default());
            }
            if this.is_instance(node, sp::NOT_EXISTS)? {
                let elems = this.required_object(node, sp::ELEMENTS, "sp:NotExists")?;
                this.push("FILTER NOT EXISTS { ")?;
                this.write_element_list(&elems, depth + 1)?;
                this.push(" }")?;
                return Ok(FxHashSet::default());
            }
            if this.is_instance(node, sp::EXISTS)? {
                let elems = this.required_object(node, sp::ELEMENTS, "sp:Exists")?;
                this.push("FILTER EXISTS { ")?;
                this.write_element_list(&elems, depth + 1)?;
                this.push(" }")?;
                return Ok(FxHashSet::default());
            }
            if this.is_instance(node, sp::ELEMENT_GROUP)? {
                let elems = this.required_object(node, sp::ELEMENTS, "sp:ElementGroup")?;
                this.push("{ ")?;
                let exported = this.write_element_list(&elems, depth + 1)?;
                this.push(" }")?;
                return Ok(exported);
            }
            if this.is_instance(node, sp::TRIPLE_PATH)? || this.g.has(node, sp::PATH) {
                let s = this.required_object(node, sp::SUBJECT, "sp:TriplePath")?;
                let p = this.required_object(node, sp::PATH, "sp:TriplePath")?;
                let o = this.required_object(node, sp::OBJECT, "sp:TriplePath")?;
                this.write_node_or_var(&s, false, NodePos::Subject, depth + 1)?;
                this.push(" ")?;
                this.write_path(&p, depth + 1)?;
                this.push(" ")?;
                this.write_node_or_var(&o, false, NodePos::Object, depth + 1)?;
                this.push(" .")?;
                return Ok(FxHashSet::default());
            }
            if this.is_instance(node, sp::TRIPLE_PATTERN)?
                || this.is_instance(node, sp::TRIPLE_TEMPLATE)?
                || (this.g.has(node, sp::SUBJECT)
                    && this.g.has(node, sp::PREDICATE)
                    && this.g.has(node, sp::OBJECT))
            {
                this.write_triple_pattern(node, false, depth + 1)?;
                return Ok(FxHashSet::default());
            }
            bail!("unsupported or malformed SPIN graph pattern element {node}")
        })
    }

    fn write_triple_pattern(&mut self, node: &Term, in_template: bool, depth: usize) -> Result<()> {
        if depth > MAX_DEPTH {
            return Err(limit_err(format!(
                "SPIN triple pattern exceeds maximum nesting depth ({MAX_DEPTH}) at {node}"
            )));
        }
        self.count_node()?;
        let s = self.required_object(node, sp::SUBJECT, "sp:TriplePattern")?;
        let p = self.required_object(node, sp::PREDICATE, "sp:TriplePattern")?;
        let o = self.required_object(node, sp::OBJECT, "sp:TriplePattern")?;
        self.write_node_or_var(&s, in_template, NodePos::Subject, depth + 1)?;
        self.push(" ")?;
        self.write_node_or_var(&p, in_template, NodePos::Predicate, depth + 1)?;
        self.push(" ")?;
        self.write_node_or_var(&o, in_template, NodePos::Object, depth + 1)?;
        self.push(" .")
    }

    fn write_var_or_iri(&mut self, node: &Term, what: &str) -> Result<()> {
        if let Some(name) = self.var_name(node)? {
            if let Some(bound) = self.bound_term(&name).cloned() {
                let Term::NamedNode(iri) = bound else {
                    bail!("template argument ?{name} bound to {bound} is not an IRI for {what}");
                };
                return self.push_iri(iri.as_str());
            }
            self.push("?")?;
            return self.push(&name);
        }
        match node {
            Term::NamedNode(n) => self.push_iri(n.as_str()),
            _ => bail!("{what} expects an IRI or SPIN variable, found {node}"),
        }
    }

    fn write_node_or_var(
        &mut self,
        node: &Term,
        in_template: bool,
        pos: NodePos,
        depth: usize,
    ) -> Result<()> {
        if let Some(name) = self.var_name(node)? {
            if let Some(bound) = self.bound_term(&name).cloned() {
                if pos == NodePos::Predicate && !matches!(bound, Term::NamedNode(_)) {
                    bail!(
                        "template argument ?{name} bound to {bound} cannot be used as a predicate"
                    );
                }
                return self.push_term_value(&bound, depth);
            }
            self.push("?")?;
            return self.push(&name);
        }
        match node {
            Term::NamedNode(n) => self.push_iri(n.as_str()),
            Term::Literal(l) => {
                if pos == NodePos::Predicate {
                    bail!("literal {l} cannot be used as a predicate in SPIN triple pattern");
                }
                self.push_literal_ref(l.as_ref())
            }
            Term::BlankNode(b) => {
                let is_var = self.is_instance(node, sp::VARIABLE_CLS)?;
                if pos == NodePos::Predicate && !is_var {
                    bail!("blank node {b} cannot be used as a predicate in SPIN triple pattern");
                }
                if in_template && !is_var {
                    let label = self.template_bnode_label(b);
                    self.push("_:")?;
                    self.push(&label)
                } else {
                    self.write_anon_var(b)
                }
            }
            Term::Triple(_) => {
                if pos == NodePos::Predicate {
                    bail!("triple term {node} cannot be used as a predicate");
                }
                self.push_term_value(node, depth)
            }
        }
    }

    fn write_values(&mut self, node: &Term, depth: usize) -> Result<()> {
        if depth > MAX_DEPTH {
            return Err(limit_err(format!(
                "SPIN sp:Values exceeds maximum nesting depth ({MAX_DEPTH}) at {node}"
            )));
        }
        let var_names: Vec<String> = if let Some(head) = self.single_object(node, sp::VAR_NAMES)? {
            let items = self.read_list(&head, "sp:varNames")?;
            let mut out = Vec::with_capacity(items.len());
            for item in items {
                let Term::Literal(lit) = item else {
                    bail!("sp:varNames in sp:Values {node} expects string literals, found {item}");
                };
                let name = lit.value();
                validate_var_name(name)?;
                self.record_var(name)?;
                if self.is_bound(name) {
                    bail!("sp:Values {node} mentions bound template argument ?{name}");
                }
                out.push(name.to_string());
            }
            out
        } else if let Some(head) = self.single_object(node, sp::VARIABLES)? {
            let items = self.read_list(&head, "sp:variables")?;
            let mut out = Vec::with_capacity(items.len());
            for item in items {
                let Some(name) = self.var_name(&item)? else {
                    bail!("sp:variables entry {item} in sp:Values {node} has no sp:varName");
                };
                if self.is_bound(&name) {
                    bail!("sp:Values {node} mentions bound template argument ?{name}");
                }
                out.push(name);
            }
            out
        } else {
            bail!("sp:Values {node} is missing sp:varNames");
        };

        let bindings_head = self.required_object(node, sp::BINDINGS, "sp:Values")?;
        let rows = self.read_list(&bindings_head, "sp:bindings")?;
        self.push("VALUES (")?;
        for (i, name) in var_names.iter().enumerate() {
            if i > 0 {
                self.push(" ")?;
            }
            self.push("?")?;
            self.push(name)?;
        }
        self.push(") {")?;
        for row_node in &rows {
            let row = self.read_list(row_node, "sp:bindings row")?;
            if row.len() != var_names.len() {
                bail!(
                    "sp:Values {node} row has {} values for {} variables",
                    row.len(),
                    var_names.len()
                );
            }
            self.push(" (")?;
            for (i, val) in row.iter().enumerate() {
                if i > 0 {
                    self.push(" ")?;
                }
                if matches!(val, Term::NamedNode(n) if n.as_ref() == sp::UNDEF) {
                    self.push("UNDEF")?;
                } else if let Some(name) = self.var_name(val)? {
                    match self.bound_term(&name).cloned() {
                        Some(bound) => self.push_term_value(&bound, depth + 1)?,
                        None => bail!(
                            "sp:Values {node} contains unbound variable ?{name} in bindings row"
                        ),
                    }
                } else {
                    self.push_term_value(val, depth + 1)?;
                }
            }
            self.push(")")?;
        }
        self.push(" }")
    }

    fn write_path(&mut self, node: &Term, depth: usize) -> Result<()> {
        self.with_node(node, depth, "property path", |this| {
            // Template variable bound to a predicate IRI
            if let Some(name) = this.var_name(node)? {
                match this.bound_term(&name).cloned() {
                    Some(Term::NamedNode(iri)) => {
                        return this.push_iri(iri.as_str());
                    }
                    Some(other) => bail!(
                        "template argument ?{name} bound to {other} cannot be used as a property path"
                    ),
                    None => bail!(
                        "unbound variable ?{name} cannot be used inside a SPARQL property path"
                    ),
                }
            }
            if this.is_instance(node, sp::SEQ_PATH)? {
                let p1 = this.required_object(node, sp::PATH1, "sp:SeqPath")?;
                let p2 = this.required_object(node, sp::PATH2, "sp:SeqPath")?;
                this.push("(")?;
                this.write_path(&p1, depth + 1)?;
                this.push(" / ")?;
                this.write_path(&p2, depth + 1)?;
                return this.push(")");
            }
            if this.is_instance(node, sp::ALT_PATH)? {
                let p1 = this.required_object(node, sp::PATH1, "sp:AltPath")?;
                let p2 = this.required_object(node, sp::PATH2, "sp:AltPath")?;
                this.push("(")?;
                this.write_path(&p1, depth + 1)?;
                this.push(" | ")?;
                this.write_path(&p2, depth + 1)?;
                return this.push(")");
            }
            if this.is_instance(node, sp::REVERSE_PATH)? {
                let sub = this.required_object(node, sp::SUB_PATH, "sp:ReversePath")?;
                this.push("^(")?;
                this.write_path(&sub, depth + 1)?;
                return this.push(")");
            }
            if this.is_instance(node, sp::REVERSE_LINK_PATH)? {
                let target = this.required_object(node, sp::NODE, "sp:ReverseLinkPath")?;
                // Parenthesize compound paths so nested reverse paths never emit invalid `^^(...)`
                let is_simple_iri = matches!(target, Term::NamedNode(_))
                    && !this.is_instance(&target, sp::SEQ_PATH)?
                    && !this.is_instance(&target, sp::ALT_PATH)?
                    && !this.is_instance(&target, sp::REVERSE_PATH)?
                    && !this.is_instance(&target, sp::REVERSE_LINK_PATH)?
                    && !this.is_instance(&target, sp::MOD_PATH)?;
                if is_simple_iri {
                    this.push("^")?;
                    return this.write_path(&target, depth + 1);
                }
                this.push("^(")?;
                this.write_path(&target, depth + 1)?;
                return this.push(")");
            }
            if this.is_instance(node, sp::MOD_PATH)? {
                let sub = this.required_object(node, sp::SUB_PATH, "sp:ModPath")?;
                let min = this.optional_i64(node, sp::MOD_MIN)?;
                let max = this.optional_i64(node, sp::MOD_MAX)?;
                if min.is_none() && max.is_none() {
                    bail!("sp:ModPath {node} must specify sp:modMin or sp:modMax");
                }
                let min_v = min.unwrap_or(0);
                if min_v < 0 {
                    bail!("sp:ModPath {node} has negative sp:modMin {min_v}");
                }
                // Upstream ARQ2SPIN encodes zero-or-one (`?`) as `sp:modMin 0; sp:modMax -1`
                if min_v == 0 && max == Some(-1) {
                    this.push("(")?;
                    this.write_path(&sub, depth + 1)?;
                    return this.push(")?");
                }
                let unbounded = matches!(max, None | Some(-2));
                if !unbounded {
                    let max_v = max.unwrap();
                    if max_v < 0 || max_v < min_v {
                        bail!("sp:ModPath {node} has invalid bounds ({min_v}, {max_v})");
                    }
                    if max_v == 0 {
                        bail!(
                            "sp:ModPath {node} with bounds (0, 0) is not expressible in SPARQL 1.1"
                        );
                    }
                    let min_u = usize::try_from(min_v).unwrap_or(usize::MAX);
                    let max_u = usize::try_from(max_v).unwrap_or(usize::MAX);
                    if max_u > MAX_MOD_PATH_UNROLL {
                        return Err(limit_err(format!(
                            "sp:ModPath {node} repetition bound {max_u} exceeds limit ({MAX_MOD_PATH_UNROLL})"
                        )));
                    }
                    if min_u == 0 && max_u == 1 {
                        this.push("(")?;
                        this.write_path(&sub, depth + 1)?;
                        return this.push(")?");
                    }
                    if min_u == 1 && max_u == 1 {
                        this.push("(")?;
                        this.write_path(&sub, depth + 1)?;
                        return this.push(")");
                    }
                    // Unroll fixed bounds (min_u mandatory + (max_u - min_u) optional steps)
                    this.push("(")?;
                    for i in 0..max_u {
                        if i > 0 {
                            this.push(" / ")?;
                        }
                        this.push("(")?;
                        this.write_path(&sub, depth + 1)?;
                        if i < min_u {
                            this.push(")")?;
                        } else {
                            this.push(")?")?;
                        }
                    }
                    return this.push(")");
                }
                let min_u = usize::try_from(min_v).unwrap_or(usize::MAX);
                if min_u == 0 {
                    this.push("(")?;
                    this.write_path(&sub, depth + 1)?;
                    return this.push(")*");
                }
                if min_u == 1 {
                    this.push("(")?;
                    this.write_path(&sub, depth + 1)?;
                    return this.push(")+");
                }
                if min_u > MAX_MOD_PATH_UNROLL {
                    return Err(limit_err(format!(
                        "sp:ModPath {node} minimum repetition {min_u} exceeds limit ({MAX_MOD_PATH_UNROLL})"
                    )));
                }
                this.push("(")?;
                for i in 0..min_u {
                    if i > 0 {
                        this.push(" / ")?;
                    }
                    this.push("(")?;
                    this.write_path(&sub, depth + 1)?;
                    if i + 1 == min_u {
                        this.push(")+")?;
                    } else {
                        this.push(")")?;
                    }
                }
                return this.push(")");
            }
            if let Term::NamedNode(iri) = node {
                return this.push_iri(iri.as_str());
            }
            bail!("unsupported or malformed SPIN property path {node}")
        })
    }

    fn collect_args(&mut self, node: &Term) -> Result<Vec<Term>> {
        let Some(sub) = as_subject(node) else {
            return Ok(Vec::new());
        };
        let mut by_index: Vec<(usize, Term)> = Vec::new();
        for triple in self.g.g.triples_for_subject(sub) {
            self.count_node()?;
            let pred = triple.predicate.as_str();
            let Some(rest) = pred.strip_prefix("http://spinrdf.org/sp#arg") else {
                continue;
            };
            if rest.is_empty() || rest.starts_with('0') || !rest.chars().all(|c| c.is_ascii_digit())
            {
                continue;
            }
            let idx = rest
                .parse::<usize>()
                .map_err(|_| anyhow!("invalid SPIN argument index sp:arg{rest} on {node}"))?;
            by_index.push((idx, check_and_own_term(triple.object, self.budget)?));
        }
        by_index.sort_by_key(|(idx, _)| *idx);
        let mut out = Vec::with_capacity(by_index.len());
        for (pos, (idx, val)) in by_index.into_iter().enumerate() {
            let expected = pos + 1;
            if idx == pos {
                bail!("SPIN expression {node} has duplicate sp:arg{idx}");
            }
            if idx != expected {
                bail!(
                    "SPIN expression {node} has non-contiguous arguments: expected sp:arg{expected}, found sp:arg{idx}"
                );
            }
            out.push(val);
        }
        Ok(out)
    }

    fn expr_call_type(&mut self, node: &Term, args: &[Term]) -> Result<Option<NamedNode>> {
        let Some(sub) = as_subject(node) else {
            return Ok(None);
        };
        let mut types: Vec<NamedNode> = Vec::new();
        for t in self.g.g.objects_for_subject_predicate(sub, rdf::TYPE) {
            self.count_node()?;
            if let oxrdf::TermRef::NamedNode(n) = t
                && !matches!(
                    n.as_str(),
                    "http://spinrdf.org/sp#Expression"
                        | "http://spinrdf.org/sp#Function"
                        | "http://spinrdf.org/sp#Aggregation"
                        | "http://www.w3.org/2000/01/rdf-schema#Resource"
                        | "http://www.w3.org/2002/07/owl#Thing"
                )
            {
                types.push(check_and_own_iri(n, self.budget)?);
            }
        }
        types.sort_by(|a, b| a.as_str().cmp(b.as_str()));
        types.dedup();

        // A named node in expression position is normally an IRI constant (e.g. `ex:Person`
        // typed `a rdfs:Class`) unless it carries `sp:argN` predicates or a known `sp:`
        // expression/aggregate type.
        if matches!(node, Term::NamedNode(_)) {
            if let Some(sp_ty) = types.iter().find(|t| is_sp_expression_type(t.as_str())) {
                return Ok(Some(sp_ty.clone()));
            }
            if args.is_empty() {
                return Ok(None);
            }
        }

        match types.as_slice() {
            [] => Ok(None),
            [only] => Ok(Some(only.clone())),
            _ => {
                // Prefer a specific `sp:` expression type if present alongside a superclass
                let sp_types: Vec<_> = types
                    .iter()
                    .filter(|t| is_sp_expression_type(t.as_str()))
                    .collect();
                if sp_types.len() == 1 {
                    return Ok(Some(sp_types[0].clone()));
                }
                bail!("SPIN expression {node} has ambiguous rdf:type values")
            }
        }
    }

    fn write_expr(&mut self, node: &Term, depth: usize) -> Result<()> {
        self.with_node(node, depth, "expression", |this| {
            // 1. Named variable
            if let Some(name) = this.var_name(node)? {
                if let Some(bound) = this.bound_term(&name).cloned() {
                    return this.push_term_value(&bound, depth + 1);
                }
                this.push("?")?;
                return this.push(&name);
            }
            // 2. Literal constant
            if let Term::Literal(l) = node {
                return this.push_literal_ref(l.as_ref());
            }
            // 3. RDF-star triple term
            if matches!(node, Term::Triple(_)) {
                return this.push_term_value(node, depth + 1);
            }
            // 4. Anonymous sp:Variable blank node
            if let Term::BlankNode(b) = node
                && this.is_instance(node, sp::VARIABLE_CLS)?
            {
                return this.write_anon_var(b);
            }
            // 5. Function / operator / aggregate / EXISTS call vs IRI constant
            let args = this.collect_args(node)?;
            let Some(ty) = this.expr_call_type(node, &args)? else {
                if let Term::NamedNode(n) = node {
                    return this.push_iri(n.as_str());
                }
                bail!("malformed SPIN expression node {node}");
            };
            let iri = ty.as_str();

            if iri == sp::EXISTS.as_str() || iri == sp::EXISTS_FN.as_str() {
                let elems = this.required_object(node, sp::ELEMENTS, "sp:Exists")?;
                this.push("EXISTS { ")?;
                this.write_element_list(&elems, depth + 1)?;
                return this.push(" }");
            }
            if iri == sp::NOT_EXISTS.as_str() || iri == sp::NOT_EXISTS_FN.as_str() {
                let elems = this.required_object(node, sp::ELEMENTS, "sp:NotExists")?;
                this.push("NOT EXISTS { ")?;
                this.write_element_list(&elems, depth + 1)?;
                return this.push(" }");
            }
            if let Some(agg) = aggregate_fn(iri) {
                let distinct = this.optional_bool(node, sp::DISTINCT)?;
                let expr = this.single_object(node, sp::EXPRESSION)?;
                let sep = this.single_object(node, sp::SEPARATOR)?;
                if sep.is_some() && iri != sp::GROUP_CONCAT.as_str() {
                    bail!("sp:separator is only valid on sp:GroupConcat, not {ty}");
                }
                this.push(agg)?;
                this.push("(")?;
                if distinct {
                    this.push("DISTINCT ")?;
                }
                match expr {
                    Some(e) => this.write_expr(&e, depth + 1)?,
                    None if iri == sp::COUNT.as_str() => this.push("*")?,
                    None => bail!("aggregate {ty} at {node} is missing sp:expression"),
                }
                if let Some(sep_term) = sep {
                    let resolved = if let Some(name) = this.var_name(&sep_term)? {
                        match this.bound_term(&name).cloned() {
                            Some(bound) => bound,
                            None => bail!(
                                "sp:separator on {node} references unbound variable ?{name}"
                            ),
                        }
                    } else {
                        sep_term
                    };
                    let Term::Literal(lit) = &resolved else {
                        bail!("sp:separator on {node} must be a plain string literal, found {resolved}");
                    };
                    if lit.language().is_some() || lit.datatype() != xsd::STRING {
                        bail!(
                            "sp:separator on {node} must be a plain xsd:string literal, found {lit}"
                        );
                    }
                    this.push(" ; SEPARATOR = ")?;
                    this.push_literal_ref(lit.as_ref())?;
                }
                return this.push(")");
            }
            if let Some(op) = binary_op(iri) {
                if args.len() != 2 {
                    bail!(
                        "SPIN binary operator {ty} at {node} expects 2 arguments, found {}",
                        args.len()
                    );
                }
                this.push("(")?;
                this.write_expr(&args[0], depth + 1)?;
                this.push(" ")?;
                this.push(op)?;
                this.push(" ")?;
                this.write_expr(&args[1], depth + 1)?;
                return this.push(")");
            }
            if let Some(op) = unary_op(iri) {
                if args.len() != 1 {
                    bail!(
                        "SPIN unary operator {ty} at {node} expects 1 argument, found {}",
                        args.len()
                    );
                }
                this.push("(")?;
                this.push(op)?;
                this.push("(")?;
                this.write_expr(&args[0], depth + 1)?;
                return this.push("))");
            }
            if iri == sp::IN.as_str() || iri == sp::NOT_IN.as_str() {
                if args.is_empty() {
                    bail!("SPIN operator {ty} at {node} requires at least sp:arg1");
                }
                this.push("(")?;
                this.write_expr(&args[0], depth + 1)?;
                this.push(if iri == sp::IN.as_str() {
                    " IN ("
                } else {
                    " NOT IN ("
                })?;
                for (i, rhs) in args[1..].iter().enumerate() {
                    if i > 0 {
                        this.push(", ")?;
                    }
                    this.write_expr(rhs, depth + 1)?;
                }
                return this.push("))");
            }
            if iri == sp::BOUND.as_str() {
                if args.len() != 1 {
                    bail!(
                        "sp:bound at {node} expects 1 argument, found {}",
                        args.len()
                    );
                }
                if let Some(name) = this.var_name(&args[0])? {
                    if this.is_bound(&name) {
                        return this.push("true");
                    }
                    this.push("BOUND(?")?;
                    this.push(&name)?;
                    return this.push(")");
                }
                if let Term::BlankNode(b) = &args[0]
                    && this.is_instance(&args[0], sp::VARIABLE_CLS)?
                {
                    this.push("BOUND(")?;
                    this.write_anon_var(b)?;
                    return this.push(")");
                }
                bail!(
                    "sp:bound at {node} requires a variable argument, found {}",
                    args[0]
                );
            }
            if let Some((kw, min_a, max_a)) = builtin_fn(iri) {
                if args.len() < min_a || max_a.is_some_and(|m| args.len() > m) {
                    bail!(
                        "SPIN built-in {ty} at {node} got {} arguments (expected {}..={})",
                        args.len(),
                        min_a,
                        max_a.map_or_else(|| "inf".into(), |m| m.to_string())
                    );
                }
                this.push(kw)?;
                this.push("(")?;
                for (i, arg) in args.iter().enumerate() {
                    if i > 0 {
                        this.push(", ")?;
                    }
                    this.write_expr(arg, depth + 1)?;
                }
                return this.push(")");
            }
            if is_structural_sp_class(iri) {
                bail!("SPIN structural class {ty} cannot be called as an expression at {node}");
            }
            if iri.starts_with(SP_NS) {
                bail!("unsupported SPIN operator or built-in {ty} at {node}");
            }
            // Custom IRI function or XSD cast: <iri>(arg1, arg2, ...)
            this.push_iri(iri)?;
            this.push("(")?;
            for (i, arg) in args.iter().enumerate() {
                if i > 0 {
                    this.push(", ")?;
                }
                this.write_expr(arg, depth + 1)?;
            }
            this.push(")")
        })
    }
}
