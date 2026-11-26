//! Bounded SPARQL 1.1 serializer for SPIN RDF syntax (`http://spinrdf.org/sp#`).

use super::template::{collect_sparql_text_vars, substitute_sparql_text};
use super::vocab::{
    SP_NS, aggregate_fn, binary_op, builtin_fn, is_sp_expression_type, is_structural_sp_class, sp,
    spin, unary_op,
};
use crate::shapes::{G, as_subject};
use crate::vocab::{rdf, rdfs};
use anyhow::{Context as _, Result, anyhow, bail};
use oxrdf::vocab::xsd;
use oxrdf::{BlankNode, Literal, NamedNode, NamedNodeRef, Term};
use rustc_hash::{FxHashMap, FxHashSet};
use std::fmt;

/// Maximum nesting depth for patterns, expressions, property paths, subqueries, and templates.
pub(crate) const MAX_DEPTH: usize = 64;
/// Maximum RDF nodes / graph steps visited while translating one SPIN query (prevents DAG
/// and wide-hierarchy blowup).
pub(crate) const MAX_NODES: usize = 16_384;
/// Maximum serialized SPARQL query length in bytes.
pub(crate) const MAX_OUTPUT_BYTES: usize = 1_048_576;
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

/// Shared step budget bounding graph reads, hierarchy walks, template resolution, and AST
/// serialization for one SPIN query (or an overall translation pass).
#[derive(Clone, Debug)]
pub(crate) struct WorkBudget {
    steps: usize,
    limit: usize,
}

impl WorkBudget {
    pub(crate) fn new(limit: usize) -> Self {
        Self { steps: 0, limit }
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
}

/// Read a singleton property from `g` inspecting at most two borrowed objects (no `Vec`
/// allocation), charging `budget`.
pub(crate) fn single_object(
    g: &G<'_>,
    node: &Term,
    pred: NamedNodeRef<'_>,
    budget: &mut WorkBudget,
) -> Result<Option<Term>> {
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
    let target = Term::from(class.into_owned());
    let mut stack: Vec<Term> = Vec::new();
    for obj in g.g.objects_for_subject_predicate(sub, rdf::TYPE) {
        budget.step()?;
        if obj == target.as_ref() {
            return Ok(true);
        }
        stack.push(obj.into_owned());
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
            if sup == target.as_ref() {
                return Ok(true);
            }
            stack.push(sup.into_owned());
        }
    }
    Ok(false)
}

/// Strict XML Schema 1.1 `xsd:boolean` parser for SPIN flags (`sp:distinct`, `sp:reduced`,
/// `sp:silent`, `spl:optional`). Accepts `"true"`, `"1"`, `"false"`, `"0"` and refuses
/// invalid lexical forms or non-boolean terms.
pub(crate) fn parse_spin_bool(t: &Term, context: &str) -> Result<bool> {
    if let Term::Literal(l) = t
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
    budget: &mut WorkBudget,
) -> Result<String> {
    let mut ser = Serializer {
        g: *g,
        bindings,
        budget,
        out: String::new(),
        ancestors: FxHashSet::default(),
        anon_vars: FxHashMap::default(),
        anon_var_spans: Vec::new(),
        template_bnodes: FxHashMap::default(),
        used_var_names: FxHashSet::default(),
    };
    ser.write_query(query, 0, false)?;
    ser.resolve_anon_var_collisions()
}

/// Append `s` to `out`, refusing output larger than [`MAX_OUTPUT_BYTES`].
pub(crate) fn push_str_bounded(out: &mut String, s: &str) -> Result<()> {
    let Some(next) = out.len().checked_add(s.len()) else {
        return Err(limit_err(format!(
            "serialized SPIN query exceeds maximum output size ({MAX_OUTPUT_BYTES} bytes)"
        )));
    };
    if next > MAX_OUTPUT_BYTES {
        return Err(limit_err(format!(
            "serialized SPIN query exceeds maximum output size ({MAX_OUTPUT_BYTES} bytes)"
        )));
    }
    out.push_str(s);
    Ok(())
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
pub(crate) fn write_iri(out: &mut String, iri: &str) -> Result<()> {
    if out.len().saturating_add(iri.len()).saturating_add(2) > MAX_OUTPUT_BYTES {
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
    push_str_bounded(out, "<")?;
    push_str_bounded(out, iri)?;
    push_str_bounded(out, ">")
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

/// Serialize an RDF literal in SPARQL 1.1 syntax, enforcing [`MAX_OUTPUT_BYTES`] before
/// scanning or allocating.
pub(crate) fn write_literal(out: &mut String, lit: &Literal) -> Result<()> {
    let val = lit.value();
    if out.len().saturating_add(val.len()).saturating_add(2) > MAX_OUTPUT_BYTES {
        return Err(limit_err(format!(
            "serialized SPIN query exceeds maximum output size ({MAX_OUTPUT_BYTES} bytes)"
        )));
    }
    push_str_bounded(out, "\"")?;
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
            push_str_bounded(out, &val[last..idx])?;
        }
        push_str_bounded(out, esc)?;
        last = idx + c.len_utf8();
    }
    if last < val.len() {
        push_str_bounded(out, &val[last..])?;
    }
    push_str_bounded(out, "\"")?;
    if let Some(lang) = lit.language() {
        validate_lang_tag(lang)?;
        push_str_bounded(out, "@")?;
        push_str_bounded(out, lang)?;
    } else if lit.datatype() != xsd::STRING {
        push_str_bounded(out, "^^")?;
        write_iri(out, lit.datatype().as_str())?;
    }
    Ok(())
}

/// Serialize a ground RDF term (IRI, literal, or RDF-star triple term) into SPARQL 1.1
/// syntax under [`MAX_DEPTH`].
pub(crate) fn write_term_value(out: &mut String, term: &Term, depth: usize) -> Result<()> {
    if depth > MAX_DEPTH {
        return Err(limit_err(format!(
            "embedded RDF-star triple term exceeds maximum nesting depth ({MAX_DEPTH})"
        )));
    }
    match term {
        Term::NamedNode(n) => write_iri(out, n.as_str()),
        Term::Literal(l) => write_literal(out, l),
        Term::BlankNode(_) => {
            bail!("cannot serialize blank node {term} as a constant SPARQL term")
        }
        Term::Triple(t) => {
            push_str_bounded(out, "<< ")?;
            let s = Term::from(t.subject.clone());
            let p = Term::NamedNode(t.predicate.clone());
            write_term_value(out, &s, depth + 1)?;
            push_str_bounded(out, " ")?;
            write_term_value(out, &p, depth + 1)?;
            push_str_bounded(out, " ")?;
            write_term_value(out, &t.object, depth + 1)?;
            push_str_bounded(out, " >>")
        }
    }
}

/// Return the SPARQL variable name if `node` denotes a named SPIN variable (`sp:varName`,
/// `spin:_this`, or `spin:_arg<N>`).
pub(crate) fn spin_var_name(
    g: &G<'_>,
    node: &Term,
    budget: &mut WorkBudget,
) -> Result<Option<String>> {
    match single_object(g, node, sp::VAR_NAME, budget)? {
        None => {}
        Some(Term::Literal(lit)) => {
            let name = lit.value();
            validate_var_name(name)
                .with_context(|| format!("invalid sp:varName on SPIN variable {node}"))?;
            return Ok(Some(name.to_string()));
        }
        Some(other) => bail!("sp:varName of {node} must be a string literal, found {other}"),
    }
    if let Term::NamedNode(n) = node {
        if n.as_ref() == spin::THIS {
            return Ok(Some("this".to_string()));
        }
        if let Some(rest) = n.as_str().strip_prefix("http://spinrdf.org/spin#_arg")
            && !rest.is_empty()
            && rest.chars().all(|c| c.is_ascii_digit())
        {
            return Ok(Some(format!("arg{rest}")));
        }
    }
    Ok(None)
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

struct Serializer<'g, 'b, 'w> {
    g: G<'g>,
    bindings: &'b FxHashMap<String, Option<Term>>,
    budget: &'w mut WorkBudget,
    out: String,
    ancestors: FxHashSet<Term>,
    /// Anonymous blank-node variable -> sequential id `0..anon_vars.len()`.
    anon_vars: FxHashMap<BlankNode, usize>,
    /// Byte spans `(name_start, name_end, anon_id)` of provisionally emitted `_spin_v{id+1}`
    /// names in `out`, rewritten once at the end only if any name collides with query vars.
    anon_var_spans: Vec<(usize, usize, usize)>,
    template_bnodes: FxHashMap<BlankNode, String>,
    /// Variable names seen anywhere in this query (named SPIN vars, `sp:Values`, `sp:SubQuery` `sp:text`).
    used_var_names: FxHashSet<String>,
}

impl Serializer<'_, '_, '_> {
    fn push(&mut self, s: &str) -> Result<()> {
        push_str_bounded(&mut self.out, s)
    }

    fn count_node(&mut self) -> Result<()> {
        self.budget.step()
    }

    fn record_var(&mut self, name: &str) {
        if !self.used_var_names.contains(name) {
            self.used_var_names.insert(name.to_string());
        }
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
            self.record_var(n);
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
        match self.single_object(node, pred)? {
            None => Ok(false),
            Some(ref t) => parse_spin_bool(t, &format!("SPIN node {node} <{}>", pred.as_str())),
        }
    }

    fn optional_u64(&mut self, node: &Term, pred: NamedNodeRef<'_>) -> Result<Option<u64>> {
        match self.single_object(node, pred)? {
            None => Ok(None),
            Some(Term::Literal(lit)) => {
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
        match self.single_object(node, pred)? {
            None => Ok(None),
            Some(Term::Literal(lit)) => {
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
        let start = self.out.len();
        let provisional = format!("_spin_v{}", id + 1);
        self.push(&provisional)?;
        let end = self.out.len();
        self.anon_var_spans.push((start, end, id));
        Ok(())
    }

    fn resolve_anon_var_collisions(mut self) -> Result<String> {
        if self.anon_vars.is_empty() {
            return Ok(self.out);
        }
        let count = self.anon_vars.len();
        let has_collision = (0..count).any(|id| {
            let name = format!("_spin_v{}", id + 1);
            self.used_var_names.contains(&name) || self.bindings.contains_key(&name)
        });
        if !has_collision {
            return Ok(self.out);
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
        let mut rebuilt = String::with_capacity(self.out.len().min(MAX_OUTPUT_BYTES));
        let mut cursor = 0usize;
        for (start, end, id) in self.anon_var_spans {
            push_str_bounded(&mut rebuilt, &self.out[cursor..start])?;
            push_str_bounded(&mut rebuilt, &final_names[id])?;
            cursor = end;
        }
        push_str_bounded(&mut rebuilt, &self.out[cursor..])?;
        Ok(rebuilt)
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

    fn write_query(&mut self, query: &Term, depth: usize, is_subquery: bool) -> Result<()> {
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
                    this.write_element_list(&where_head, depth + 1)?;
                    this.push(" }")?;
                    this.write_solution_modifiers(query, depth + 1)?;
                }
                QueryKind::Select => {
                    let distinct = this.optional_bool(query, sp::DISTINCT)?;
                    let reduced = this.optional_bool(query, sp::REDUCED)?;
                    if distinct && reduced {
                        bail!(
                            "SPIN SELECT query {query} cannot set both sp:distinct and sp:reduced"
                        );
                    }
                    this.push("SELECT ")?;
                    if distinct {
                        this.push("DISTINCT ")?;
                    } else if reduced {
                        this.push("REDUCED ")?;
                    }
                    match this.single_object(query, sp::RESULT_VARIABLES)? {
                        None => this.push("*")?,
                        Some(head) => {
                            let vars = this.read_list(&head, "sp:resultVariables")?;
                            if vars.is_empty() {
                                this.push("*")?;
                            } else {
                                for (i, v) in vars.iter().enumerate() {
                                    if i > 0 {
                                        this.push(" ")?;
                                    }
                                    this.write_projection_item(v, depth + 1)?;
                                }
                            }
                        }
                    }
                    if !is_subquery {
                        this.write_dataset_clauses(query)?;
                    }
                    let where_head = this.required_object(query, sp::WHERE, "sp:Select")?;
                    this.push(" WHERE { ")?;
                    this.write_element_list(&where_head, depth + 1)?;
                    this.push(" }")?;
                    this.write_solution_modifiers(query, depth + 1)?;
                }
                QueryKind::Ask => {
                    this.push("ASK")?;
                    this.write_dataset_clauses(query)?;
                    let where_head = this.required_object(query, sp::WHERE, "sp:Ask")?;
                    this.push(" WHERE { ")?;
                    this.write_element_list(&where_head, depth + 1)?;
                    this.push(" }")?;
                    if let Some(values) = this.single_object(query, sp::VALUES)? {
                        this.push(" ")?;
                        this.write_values(&values, depth + 1)?;
                    }
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
                    if let Some(where_head) = this.single_object(query, sp::WHERE)? {
                        this.push(" WHERE { ")?;
                        this.write_element_list(&where_head, depth + 1)?;
                        this.push(" }")?;
                    }
                    this.write_solution_modifiers(query, depth + 1)?;
                }
            }
            Ok(())
        })
    }

    fn write_dataset_clauses(&mut self, query: &Term) -> Result<()> {
        let Some(sub) = as_subject(query) else {
            return Ok(());
        };
        let mut from = Vec::new();
        for g in self.g.g.objects_for_subject_predicate(sub, sp::FROM) {
            self.count_node()?;
            from.push(g.into_owned());
        }
        from.sort_by_cached_key(ToString::to_string);
        for g in from {
            let Term::NamedNode(iri) = g else {
                bail!("sp:from of {query} must be an IRI, found {g}");
            };
            self.push(" FROM ")?;
            write_iri(&mut self.out, iri.as_str())?;
        }
        let mut from_named = Vec::new();
        for g in self.g.g.objects_for_subject_predicate(sub, sp::FROM_NAMED) {
            self.count_node()?;
            from_named.push(g.into_owned());
        }
        from_named.sort_by_cached_key(ToString::to_string);
        for g in from_named {
            let Term::NamedNode(iri) = g else {
                bail!("sp:fromNamed of {query} must be an IRI, found {g}");
            };
            self.push(" FROM NAMED ")?;
            write_iri(&mut self.out, iri.as_str())?;
        }
        Ok(())
    }

    fn write_projection_item(&mut self, item: &Term, depth: usize) -> Result<()> {
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
            return Ok(());
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
            return Ok(());
        }
        if let Some(name) = self.var_name(item)? {
            if let Some(bound) = self.bound_term(&name).cloned() {
                self.push("(")?;
                write_term_value(&mut self.out, &bound, depth + 1)?;
                self.push(" AS ?")?;
                self.push(&name)?;
                self.push(")")?;
            } else {
                self.push("?")?;
                self.push(&name)?;
            }
            return Ok(());
        }
        bail!("invalid sp:resultVariables entry {item}: expected variable or projected expression")
    }

    fn write_solution_modifiers(&mut self, query: &Term, depth: usize) -> Result<()> {
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
                        self.push("(")?;
                        self.write_expr(&expr, depth + 1)?;
                        if let Some(name) = self.var_name(item)? {
                            if self.is_bound(&name) {
                                bail!("GROUP BY assigns to bound template argument ?{name}");
                            }
                            self.push(" AS ?")?;
                            self.push(&name)?;
                        }
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

    fn write_element_list(&mut self, head: &Term, depth: usize) -> Result<()> {
        let elements = self.read_list(head, "sp:where / sp:elements")?;
        for (i, elem) in elements.iter().enumerate() {
            if i > 0 {
                self.push(" ")?;
            }
            self.write_element(elem, depth + 1)?;
        }
        Ok(())
    }

    fn write_element_block(&mut self, node: &Term, depth: usize) -> Result<()> {
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

    fn write_subquery_text(&mut self, inner: &Term, text_term: Term) -> Result<()> {
        let Term::Literal(lit) = text_term else {
            bail!("sp:text of subquery {inner} must be a string literal");
        };
        let raw = lit.value();
        if raw.len() > MAX_OUTPUT_BYTES {
            return Err(limit_err(format!(
                "SPIN sp:text exceeds maximum size ({MAX_OUTPUT_BYTES} bytes)"
            )));
        }
        if self.bindings.is_empty() {
            collect_sparql_text_vars(raw, &mut self.used_var_names);
            self.push(raw)
        } else {
            let substituted = substitute_sparql_text(raw, self.bindings)?;
            collect_sparql_text_vars(&substituted, &mut self.used_var_names);
            self.push(&substituted)
        }
    }

    fn write_element(&mut self, node: &Term, depth: usize) -> Result<()> {
        self.with_node(node, depth, "graph pattern", |this| {
            // Nested RDF list of elements represents a `{ ... }` group
            if matches!(node, Term::NamedNode(n) if n.as_ref() == rdf::NIL)
                || this.g.has(node, rdf::FIRST)
            {
                this.push("{ ")?;
                this.write_element_list(node, depth + 1)?;
                this.push(" }")?;
                return Ok(());
            }
            if this.is_instance(node, sp::FILTER)? {
                let expr = this.required_object(node, sp::EXPRESSION, "sp:Filter")?;
                this.push("FILTER (")?;
                this.write_expr(&expr, depth + 1)?;
                this.push(")")?;
                return Ok(());
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
                return Ok(());
            }
            if this.is_instance(node, sp::OPTIONAL)? {
                let elems = this.required_object(node, sp::ELEMENTS, "sp:Optional")?;
                this.push("OPTIONAL { ")?;
                this.write_element_list(&elems, depth + 1)?;
                this.push(" }")?;
                return Ok(());
            }
            if this.is_instance(node, sp::UNION)? {
                let elems = this.required_object(node, sp::ELEMENTS, "sp:Union")?;
                let branches = this.read_list(&elems, "sp:Union")?;
                if branches.is_empty() {
                    bail!("sp:Union {node} has no branches");
                }
                for (i, branch) in branches.iter().enumerate() {
                    if i > 0 {
                        this.push(" UNION ")?;
                    }
                    this.push("{ ")?;
                    this.write_element_block(branch, depth + 1)?;
                    this.push(" }")?;
                }
                return Ok(());
            }
            if this.is_instance(node, sp::SUB_QUERY)? {
                let inner = this.required_object(node, sp::QUERY_PROP, "sp:SubQuery")?;
                this.push("{ ")?;
                if has_rdf_query_structure(&this.g, &inner) {
                    let saved_len = this.out.len();
                    let saved_spans = this.anon_var_spans.len();
                    match this.write_query(&inner, depth + 1, true) {
                        Ok(()) => {}
                        Err(rdf_err) => {
                            if !is_limit_or_cycle_error(&rdf_err)
                                && let Some(text_term) = this.single_object(&inner, sp::TEXT)?
                            {
                                this.out.truncate(saved_len);
                                this.anon_var_spans.truncate(saved_spans);
                                this.write_subquery_text(&inner, text_term)?;
                            } else {
                                return Err(rdf_err);
                            }
                        }
                    }
                } else if let Some(text_term) = this.single_object(&inner, sp::TEXT)? {
                    this.write_subquery_text(&inner, text_term)?;
                } else {
                    bail!(
                        "sp:SubQuery {node} points to {inner} which has neither SPIN RDF syntax nor sp:text"
                    );
                }
                this.push(" }")?;
                return Ok(());
            }
            if this.is_instance(node, sp::MINUS)? {
                let elems = this.required_object(node, sp::ELEMENTS, "sp:Minus")?;
                this.push("MINUS { ")?;
                this.write_element_list(&elems, depth + 1)?;
                this.push(" }")?;
                return Ok(());
            }
            if this.is_instance(node, sp::NAMED_GRAPH)? {
                let graph_node =
                    this.required_object(node, sp::GRAPH_NAME_NODE, "sp:NamedGraph")?;
                let elems = this.required_object(node, sp::ELEMENTS, "sp:NamedGraph")?;
                this.push("GRAPH ")?;
                this.write_var_or_iri(&graph_node, "sp:graphNameNode")?;
                this.push(" { ")?;
                this.write_element_list(&elems, depth + 1)?;
                this.push(" }")?;
                return Ok(());
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
                this.write_element_list(&elems, depth + 1)?;
                this.push(" }")?;
                return Ok(());
            }
            if this.is_instance(node, sp::VALUES_CLS)? {
                this.write_values(node, depth + 1)?;
                return Ok(());
            }
            if this.is_instance(node, sp::NOT_EXISTS)? {
                let elems = this.required_object(node, sp::ELEMENTS, "sp:NotExists")?;
                this.push("FILTER NOT EXISTS { ")?;
                this.write_element_list(&elems, depth + 1)?;
                this.push(" }")?;
                return Ok(());
            }
            if this.is_instance(node, sp::EXISTS)? {
                let elems = this.required_object(node, sp::ELEMENTS, "sp:Exists")?;
                this.push("FILTER EXISTS { ")?;
                this.write_element_list(&elems, depth + 1)?;
                this.push(" }")?;
                return Ok(());
            }
            if this.is_instance(node, sp::ELEMENT_GROUP)? {
                let elems = this.required_object(node, sp::ELEMENTS, "sp:ElementGroup")?;
                this.push("{ ")?;
                this.write_element_list(&elems, depth + 1)?;
                this.push(" }")?;
                return Ok(());
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
                return Ok(());
            }
            if this.is_instance(node, sp::TRIPLE_PATTERN)?
                || this.is_instance(node, sp::TRIPLE_TEMPLATE)?
                || (this.g.has(node, sp::SUBJECT)
                    && this.g.has(node, sp::PREDICATE)
                    && this.g.has(node, sp::OBJECT))
            {
                return this.write_triple_pattern(node, false, depth + 1);
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
                return write_iri(&mut self.out, iri.as_str());
            }
            self.push("?")?;
            return self.push(&name);
        }
        match node {
            Term::NamedNode(n) => write_iri(&mut self.out, n.as_str()),
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
                return write_term_value(&mut self.out, &bound, depth);
            }
            self.push("?")?;
            return self.push(&name);
        }
        match node {
            Term::NamedNode(n) => write_iri(&mut self.out, n.as_str()),
            Term::Literal(l) => {
                if pos == NodePos::Predicate {
                    bail!("literal {l} cannot be used as a predicate in SPIN triple pattern");
                }
                write_literal(&mut self.out, l)
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
                write_term_value(&mut self.out, node, depth)
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
                self.record_var(name);
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
                        Some(bound) => write_term_value(&mut self.out, &bound, depth + 1)?,
                        None => bail!(
                            "sp:Values {node} contains unbound variable ?{name} in bindings row"
                        ),
                    }
                } else {
                    write_term_value(&mut self.out, val, depth + 1)?;
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
                        return write_iri(&mut this.out, iri.as_str());
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
                return write_iri(&mut this.out, iri.as_str());
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
            by_index.push((idx, triple.object.into_owned()));
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
                types.push(n.into_owned());
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
                    return write_term_value(&mut this.out, &bound, depth + 1);
                }
                this.push("?")?;
                return this.push(&name);
            }
            // 2. Literal constant
            if let Term::Literal(l) = node {
                return write_literal(&mut this.out, l);
            }
            // 3. RDF-star triple term
            if matches!(node, Term::Triple(_)) {
                return write_term_value(&mut this.out, node, depth + 1);
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
                    return write_iri(&mut this.out, n.as_str());
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
                    write_literal(&mut this.out, lit)?;
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
            write_iri(&mut this.out, iri)?;
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
