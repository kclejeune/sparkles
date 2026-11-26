//! Bounded SPARQL 1.1 serializer for SPIN RDF syntax (`http://spinrdf.org/sp#`).

use super::template::substitute_sparql_text;
use super::vocab::{
    SP_NS, aggregate_fn, binary_op, builtin_fn, is_sp_expression_type, is_structural_sp_class, sp,
    spin, unary_op,
};
use crate::shapes::{G, as_subject, literal_bool};
use crate::vocab::rdf;
use anyhow::{Context as _, Result, anyhow, bail};
use oxrdf::vocab::xsd;
use oxrdf::{BlankNode, Literal, NamedNode, NamedNodeRef, Term};
use rustc_hash::{FxHashMap, FxHashSet};

/// Maximum nesting depth for patterns, expressions, property paths, subqueries, and templates.
pub(crate) const MAX_DEPTH: usize = 64;
/// Maximum RDF nodes visited while serializing one SPIN query (prevents DAG blowup).
pub(crate) const MAX_NODES: usize = 16_384;
/// Maximum serialized SPARQL query length in bytes.
pub(crate) const MAX_OUTPUT_BYTES: usize = 1_048_576;
/// Maximum length of any RDF list traversed in SPIN syntax.
pub(crate) const MAX_LIST_LEN: usize = 4_096;
/// Maximum fixed repetition count unrolled from `sp:ModPath` into SPARQL 1.1 paths.
const MAX_MOD_PATH_UNROLL: usize = 16;

/// Whether a query node has SPIN RDF query structure (`sp:where`, `sp:templates`, or
/// `sp:resultNodes`).
pub(crate) fn has_rdf_query_structure(g: &G<'_>, node: &Term) -> bool {
    g.has(node, sp::WHERE) || g.has(node, sp::TEMPLATES) || g.has(node, sp::RESULT_NODES)
}

/// Serialize a SPIN RDF syntax query rooted at `query` with optional template argument
/// bindings substituted in place.
pub(crate) fn serialize_query_with_bindings(
    g: &G<'_>,
    query: &Term,
    bindings: &FxHashMap<String, Option<Term>>,
) -> Result<String> {
    let mut used_var_names = FxHashSet::default();
    for triple in g.g.triples_for_predicate(sp::VAR_NAME) {
        if let oxrdf::TermRef::Literal(lit) = triple.object {
            used_var_names.insert(lit.value().to_string());
        }
    }
    let mut ser = Serializer {
        g: *g,
        bindings,
        out: String::new(),
        nodes_visited: 0,
        ancestors: FxHashSet::default(),
        anon_vars: FxHashMap::default(),
        template_bnodes: FxHashMap::default(),
        used_var_names,
    };
    ser.write_query(query, 0)?;
    Ok(ser.out)
}

/// Append `s` to `out`, refusing output larger than [`MAX_OUTPUT_BYTES`].
pub(crate) fn push_str_bounded(out: &mut String, s: &str) -> Result<()> {
    let Some(next) = out.len().checked_add(s.len()) else {
        bail!("serialized SPIN query exceeds maximum output size ({MAX_OUTPUT_BYTES} bytes)");
    };
    if next > MAX_OUTPUT_BYTES {
        bail!("serialized SPIN query exceeds maximum output size ({MAX_OUTPUT_BYTES} bytes)");
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

/// Serialize an RDF literal in SPARQL 1.1 syntax.
pub(crate) fn write_literal(out: &mut String, lit: &Literal) -> Result<()> {
    push_str_bounded(out, "\"")?;
    let mut escaped = String::with_capacity(lit.value().len());
    for c in lit.value().chars() {
        match c {
            '\\' => escaped.push_str("\\\\"),
            '"' => escaped.push_str("\\\""),
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            '\t' => escaped.push_str("\\t"),
            '\u{08}' => escaped.push_str("\\b"),
            '\u{0C}' => escaped.push_str("\\f"),
            _ => escaped.push(c),
        }
    }
    push_str_bounded(out, &escaped)?;
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

/// Serialize a ground RDF term (IRI, literal, or RDF-star triple term) into SPARQL 1.1 syntax.
pub(crate) fn write_term_value(out: &mut String, term: &Term) -> Result<()> {
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
            write_term_value(out, &s)?;
            push_str_bounded(out, " ")?;
            write_term_value(out, &p)?;
            push_str_bounded(out, " ")?;
            write_term_value(out, &t.object)?;
            push_str_bounded(out, " >>")
        }
    }
}

/// Return the SPARQL variable name if `node` denotes a named SPIN variable (`sp:varName`,
/// `spin:_this`, or `spin:_arg<N>`).
pub(crate) fn spin_var_name(g: &G<'_>, node: &Term) -> Result<Option<String>> {
    let names = g.objects(node, sp::VAR_NAME);
    match names.as_slice() {
        [] => {}
        [Term::Literal(lit)] => {
            let name = lit.value();
            validate_var_name(name)
                .with_context(|| format!("invalid sp:varName on SPIN variable {node}"))?;
            return Ok(Some(name.to_string()));
        }
        [other] => bail!("sp:varName of {node} must be a string literal, found {other}"),
        _ => bail!("SPIN variable {node} has multiple sp:varName values"),
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

struct Serializer<'g, 'b> {
    g: G<'g>,
    bindings: &'b FxHashMap<String, Option<Term>>,
    out: String,
    nodes_visited: usize,
    ancestors: FxHashSet<Term>,
    anon_vars: FxHashMap<BlankNode, String>,
    template_bnodes: FxHashMap<BlankNode, String>,
    used_var_names: FxHashSet<String>,
}

impl Serializer<'_, '_> {
    fn push(&mut self, s: &str) -> Result<()> {
        push_str_bounded(&mut self.out, s)
    }

    fn count_node(&mut self) -> Result<()> {
        self.nodes_visited += 1;
        if self.nodes_visited > MAX_NODES {
            bail!("SPIN query exceeds maximum node budget ({MAX_NODES})");
        }
        Ok(())
    }

    fn with_node<T>(
        &mut self,
        node: &Term,
        depth: usize,
        kind: &str,
        f: impl FnOnce(&mut Self) -> Result<T>,
    ) -> Result<T> {
        if depth > MAX_DEPTH {
            bail!("SPIN {kind} exceeds maximum nesting depth ({MAX_DEPTH}) at {node}");
        }
        self.count_node()?;
        if !self.ancestors.insert(node.clone()) {
            bail!("cyclic SPIN {kind} at {node}");
        }
        let res = f(self);
        self.ancestors.remove(node);
        res
    }

    fn single_object(&self, node: &Term, pred: NamedNodeRef<'_>) -> Result<Option<Term>> {
        let mut vals = self.g.objects(node, pred);
        match vals.len() {
            0 => Ok(None),
            1 => Ok(vals.pop()),
            _ => bail!(
                "SPIN node {node} has multiple values for <{}>",
                pred.as_str()
            ),
        }
    }

    fn required_object(&self, node: &Term, pred: NamedNodeRef<'_>, what: &str) -> Result<Term> {
        self.single_object(node, pred)?
            .ok_or_else(|| anyhow!("{what} {node} is missing <{}>", pred.as_str()))
    }

    fn optional_bool(&self, node: &Term, pred: NamedNodeRef<'_>) -> Result<bool> {
        match self.single_object(node, pred)? {
            None => Ok(false),
            Some(ref t) => literal_bool(t).ok_or_else(|| {
                anyhow!(
                    "SPIN node {node} expects an xsd:boolean literal for <{}>, found {t}",
                    pred.as_str()
                )
            }),
        }
    }

    fn optional_u64(&self, node: &Term, pred: NamedNodeRef<'_>) -> Result<Option<u64>> {
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

    fn optional_i64(&self, node: &Term, pred: NamedNodeRef<'_>) -> Result<Option<i64>> {
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
                bail!("cyclic RDF list in {what} at {head}");
            }
            if items.len() >= MAX_LIST_LEN {
                bail!("RDF list in {what} exceeds maximum length ({MAX_LIST_LEN}) at {head}");
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

    fn anon_var_name(&mut self, b: &BlankNode) -> String {
        if let Some(name) = self.anon_vars.get(b) {
            return name.clone();
        }
        let mut n = self.anon_vars.len() + 1;
        loop {
            let candidate = format!("_spin_v{n}");
            if !self.used_var_names.contains(&candidate) && !self.bindings.contains_key(&candidate)
            {
                self.used_var_names.insert(candidate.clone());
                self.anon_vars.insert(b.clone(), candidate.clone());
                return candidate;
            }
            n += 1;
        }
    }

    fn template_bnode_label(&mut self, b: &BlankNode) -> String {
        if let Some(label) = self.template_bnodes.get(b) {
            return label.clone();
        }
        let label = format!("spin_b{}", self.template_bnodes.len() + 1);
        self.template_bnodes.insert(b.clone(), label.clone());
        label
    }

    fn query_kind(&self, query: &Term) -> Result<QueryKind> {
        let mut kinds = Vec::new();
        if self.g.is_instance(query, sp::CONSTRUCT) {
            kinds.push(QueryKind::Construct);
        }
        if self.g.is_instance(query, sp::SELECT) {
            kinds.push(QueryKind::Select);
        }
        if self.g.is_instance(query, sp::ASK) {
            kinds.push(QueryKind::Ask);
        }
        if self.g.is_instance(query, sp::DESCRIBE) {
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

    fn write_query(&mut self, query: &Term, depth: usize) -> Result<()> {
        self.with_node(query, depth, "query", |this| {
            let kind = this.query_kind(query)?;
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
                    this.write_dataset_clauses(query)?;
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
        let mut from = self.g.objects(query, sp::FROM);
        from.sort_by_cached_key(ToString::to_string);
        for g in from {
            let Term::NamedNode(iri) = g else {
                bail!("sp:from of {query} must be an IRI, found {g}");
            };
            self.push(" FROM ")?;
            write_iri(&mut self.out, iri.as_str())?;
        }
        let mut from_named = self.g.objects(query, sp::FROM_NAMED);
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
        if let Some(expr) = self.single_object(item, sp::EXPRESSION)? {
            let Some(name) = spin_var_name(&self.g, item)? else {
                bail!("projected expression {item} in sp:resultVariables is missing sp:varName");
            };
            if self.bindings.get(&name).is_some_and(Option::is_some) {
                bail!("projected expression assigns to bound template argument ?{name}");
            }
            self.push("(")?;
            self.write_expr(&expr, depth + 1)?;
            self.push(" AS ?")?;
            self.push(&name)?;
            self.push(")")?;
            return Ok(());
        }
        if let Some(name) = spin_var_name(&self.g, item)? {
            if let Some(Some(bound)) = self.bindings.get(&name) {
                self.push("(")?;
                write_term_value(&mut self.out, bound)?;
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
                    if let Some(expr) = self.single_object(item, sp::EXPRESSION)? {
                        self.push("(")?;
                        self.write_expr(&expr, depth + 1)?;
                        if let Some(name) = spin_var_name(&self.g, item)? {
                            if self.bindings.get(&name).is_some_and(Option::is_some) {
                                bail!("GROUP BY assigns to bound template argument ?{name}");
                            }
                            self.push(" AS ?")?;
                            self.push(&name)?;
                        }
                        self.push(")")?;
                    } else if let Some(name) = spin_var_name(&self.g, item)?
                        && !self.bindings.get(&name).is_some_and(Option::is_some)
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
                    if self.g.is_instance(item, sp::ASC) {
                        let expr = self.required_object(item, sp::EXPRESSION, "sp:Asc")?;
                        self.push("ASC(")?;
                        self.write_expr(&expr, depth + 1)?;
                        self.push(")")?;
                    } else if self.g.is_instance(item, sp::DESC) {
                        let expr = self.required_object(item, sp::EXPRESSION, "sp:Desc")?;
                        self.push("DESC(")?;
                        self.write_expr(&expr, depth + 1)?;
                        self.push(")")?;
                    } else if let Some(name) = spin_var_name(&self.g, item)?
                        && !self.bindings.get(&name).is_some_and(Option::is_some)
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
        if self.g.is_instance(node, sp::ELEMENT_GROUP)
            && let Some(elems) = self.single_object(node, sp::ELEMENTS)?
        {
            return self.write_element_list(&elems, depth);
        }
        self.write_element(node, depth)
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
            if this.g.is_instance(node, sp::FILTER) {
                let expr = this.required_object(node, sp::EXPRESSION, "sp:Filter")?;
                this.push("FILTER (")?;
                this.write_expr(&expr, depth + 1)?;
                this.push(")")?;
                return Ok(());
            }
            if this.g.is_instance(node, sp::BIND) || this.g.is_instance(node, sp::LET) {
                let expr = this.required_object(node, sp::EXPRESSION, "sp:Bind")?;
                let var_node = this.required_object(node, sp::VARIABLE, "sp:Bind")?;
                let Some(name) = spin_var_name(&this.g, &var_node)? else {
                    bail!("sp:Bind {node} has sp:variable {var_node} without sp:varName");
                };
                if this.bindings.get(&name).is_some_and(Option::is_some) {
                    bail!("sp:Bind {node} assigns to bound template argument ?{name}");
                }
                this.push("BIND (")?;
                this.write_expr(&expr, depth + 1)?;
                this.push(" AS ?")?;
                this.push(&name)?;
                this.push(")")?;
                return Ok(());
            }
            if this.g.is_instance(node, sp::OPTIONAL) {
                let elems = this.required_object(node, sp::ELEMENTS, "sp:Optional")?;
                this.push("OPTIONAL { ")?;
                this.write_element_list(&elems, depth + 1)?;
                this.push(" }")?;
                return Ok(());
            }
            if this.g.is_instance(node, sp::UNION) {
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
            if this.g.is_instance(node, sp::SUB_QUERY) {
                let inner = this.required_object(node, sp::QUERY_PROP, "sp:SubQuery")?;
                this.push("{ ")?;
                if has_rdf_query_structure(&this.g, &inner) {
                    this.write_query(&inner, depth + 1)?;
                } else if let Some(text_term) = this.single_object(&inner, sp::TEXT)? {
                    let Term::Literal(lit) = text_term else {
                        bail!("sp:text of subquery {inner} must be a string literal");
                    };
                    if this.bindings.is_empty() {
                        this.push(lit.value())?;
                    } else {
                        let substituted = substitute_sparql_text(lit.value(), this.bindings)?;
                        this.push(&substituted)?;
                    }
                } else {
                    bail!("sp:SubQuery {node} points to {inner} which has neither SPIN RDF syntax nor sp:text");
                }
                this.push(" }")?;
                return Ok(());
            }
            if this.g.is_instance(node, sp::MINUS) {
                let elems = this.required_object(node, sp::ELEMENTS, "sp:Minus")?;
                this.push("MINUS { ")?;
                this.write_element_list(&elems, depth + 1)?;
                this.push(" }")?;
                return Ok(());
            }
            if this.g.is_instance(node, sp::NAMED_GRAPH) {
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
            if this.g.is_instance(node, sp::SERVICE) {
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
            if this.g.is_instance(node, sp::VALUES_CLS) {
                this.write_values(node, depth + 1)?;
                return Ok(());
            }
            if this.g.is_instance(node, sp::NOT_EXISTS) {
                let elems = this.required_object(node, sp::ELEMENTS, "sp:NotExists")?;
                this.push("FILTER NOT EXISTS { ")?;
                this.write_element_list(&elems, depth + 1)?;
                this.push(" }")?;
                return Ok(());
            }
            if this.g.is_instance(node, sp::EXISTS) {
                let elems = this.required_object(node, sp::ELEMENTS, "sp:Exists")?;
                this.push("FILTER EXISTS { ")?;
                this.write_element_list(&elems, depth + 1)?;
                this.push(" }")?;
                return Ok(());
            }
            if this.g.is_instance(node, sp::ELEMENT_GROUP) {
                let elems = this.required_object(node, sp::ELEMENTS, "sp:ElementGroup")?;
                this.push("{ ")?;
                this.write_element_list(&elems, depth + 1)?;
                this.push(" }")?;
                return Ok(());
            }
            if this.g.is_instance(node, sp::TRIPLE_PATH) || this.g.has(node, sp::PATH) {
                let s = this.required_object(node, sp::SUBJECT, "sp:TriplePath")?;
                let p = this.required_object(node, sp::PATH, "sp:TriplePath")?;
                let o = this.required_object(node, sp::OBJECT, "sp:TriplePath")?;
                this.write_node_or_var(&s, false, NodePos::Subject)?;
                this.push(" ")?;
                this.write_path(&p, depth + 1)?;
                this.push(" ")?;
                this.write_node_or_var(&o, false, NodePos::Object)?;
                this.push(" .")?;
                return Ok(());
            }
            if this.g.is_instance(node, sp::TRIPLE_PATTERN)
                || this.g.is_instance(node, sp::TRIPLE_TEMPLATE)
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
            bail!("SPIN triple pattern exceeds maximum nesting depth ({MAX_DEPTH}) at {node}");
        }
        self.count_node()?;
        let s = self.required_object(node, sp::SUBJECT, "sp:TriplePattern")?;
        let p = self.required_object(node, sp::PREDICATE, "sp:TriplePattern")?;
        let o = self.required_object(node, sp::OBJECT, "sp:TriplePattern")?;
        self.write_node_or_var(&s, in_template, NodePos::Subject)?;
        self.push(" ")?;
        self.write_node_or_var(&p, in_template, NodePos::Predicate)?;
        self.push(" ")?;
        self.write_node_or_var(&o, in_template, NodePos::Object)?;
        self.push(" .")
    }

    fn write_var_or_iri(&mut self, node: &Term, what: &str) -> Result<()> {
        if let Some(name) = spin_var_name(&self.g, node)? {
            if let Some(Some(bound)) = self.bindings.get(&name) {
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

    fn write_node_or_var(&mut self, node: &Term, in_template: bool, pos: NodePos) -> Result<()> {
        if let Some(name) = spin_var_name(&self.g, node)? {
            if let Some(Some(bound)) = self.bindings.get(&name) {
                if pos == NodePos::Predicate && !matches!(bound, Term::NamedNode(_)) {
                    bail!(
                        "template argument ?{name} bound to {bound} cannot be used as a predicate"
                    );
                }
                return write_term_value(&mut self.out, bound);
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
                if pos == NodePos::Predicate {
                    bail!("blank node {b} cannot be used as a predicate in SPIN triple pattern");
                }
                if in_template && !self.g.is_instance(node, sp::VARIABLE_CLS) {
                    let label = self.template_bnode_label(b);
                    self.push("_:")?;
                    self.push(&label)
                } else {
                    let var = self.anon_var_name(b);
                    self.push("?")?;
                    self.push(&var)
                }
            }
            Term::Triple(_) => {
                if pos == NodePos::Predicate {
                    bail!("triple term {node} cannot be used as a predicate");
                }
                write_term_value(&mut self.out, node)
            }
        }
    }

    fn write_values(&mut self, node: &Term, depth: usize) -> Result<()> {
        if depth > MAX_DEPTH {
            bail!("SPIN sp:Values exceeds maximum nesting depth ({MAX_DEPTH}) at {node}");
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
                if self.bindings.get(name).is_some_and(Option::is_some) {
                    bail!("sp:Values {node} mentions bound template argument ?{name}");
                }
                out.push(name.to_string());
            }
            out
        } else if let Some(head) = self.single_object(node, sp::VARIABLES)? {
            let items = self.read_list(&head, "sp:variables")?;
            let mut out = Vec::with_capacity(items.len());
            for item in items {
                let Some(name) = spin_var_name(&self.g, &item)? else {
                    bail!("sp:variables entry {item} in sp:Values {node} has no sp:varName");
                };
                if self.bindings.get(&name).is_some_and(Option::is_some) {
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
                } else if let Some(name) = spin_var_name(&self.g, val)? {
                    match self.bindings.get(&name) {
                        Some(Some(bound)) => write_term_value(&mut self.out, bound)?,
                        _ => bail!(
                            "sp:Values {node} contains unbound variable ?{name} in bindings row"
                        ),
                    }
                } else {
                    write_term_value(&mut self.out, val)?;
                }
            }
            self.push(")")?;
        }
        self.push(" }")
    }

    fn write_path(&mut self, node: &Term, depth: usize) -> Result<()> {
        self.with_node(node, depth, "property path", |this| {
            // Template variable bound to a predicate IRI
            if let Some(name) = spin_var_name(&this.g, node)? {
                match this.bindings.get(&name) {
                    Some(Some(Term::NamedNode(iri))) => {
                        return write_iri(&mut this.out, iri.as_str());
                    }
                    Some(Some(other)) => bail!(
                        "template argument ?{name} bound to {other} cannot be used as a property path"
                    ),
                    _ => bail!(
                        "unbound variable ?{name} cannot be used inside a SPARQL property path"
                    ),
                }
            }
            if this.g.is_instance(node, sp::SEQ_PATH) {
                let p1 = this.required_object(node, sp::PATH1, "sp:SeqPath")?;
                let p2 = this.required_object(node, sp::PATH2, "sp:SeqPath")?;
                this.push("(")?;
                this.write_path(&p1, depth + 1)?;
                this.push(" / ")?;
                this.write_path(&p2, depth + 1)?;
                return this.push(")");
            }
            if this.g.is_instance(node, sp::ALT_PATH) {
                let p1 = this.required_object(node, sp::PATH1, "sp:AltPath")?;
                let p2 = this.required_object(node, sp::PATH2, "sp:AltPath")?;
                this.push("(")?;
                this.write_path(&p1, depth + 1)?;
                this.push(" | ")?;
                this.write_path(&p2, depth + 1)?;
                return this.push(")");
            }
            if this.g.is_instance(node, sp::REVERSE_PATH) {
                let sub = this.required_object(node, sp::SUB_PATH, "sp:ReversePath")?;
                this.push("^(")?;
                this.write_path(&sub, depth + 1)?;
                return this.push(")");
            }
            if this.g.is_instance(node, sp::REVERSE_LINK_PATH) {
                let target = this.required_object(node, sp::NODE, "sp:ReverseLinkPath")?;
                this.push("^")?;
                return this.write_path(&target, depth + 1);
            }
            if this.g.is_instance(node, sp::MOD_PATH) {
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
                let unbounded = matches!(max, None | Some(-2));
                if !unbounded {
                    let max_v = max.unwrap();
                    if max_v < 0 || max_v < min_v {
                        bail!("sp:ModPath {node} has invalid bounds ({min_v}, {max_v})");
                    }
                    if max_v == 0 {
                        bail!("sp:ModPath {node} with bounds (0, 0) is not expressible in SPARQL 1.1");
                    }
                    let min_u = usize::try_from(min_v).unwrap_or(usize::MAX);
                    let max_u = usize::try_from(max_v).unwrap_or(usize::MAX);
                    if max_u > MAX_MOD_PATH_UNROLL {
                        bail!(
                            "sp:ModPath {node} repetition bound {max_u} exceeds limit ({MAX_MOD_PATH_UNROLL})"
                        );
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
                    bail!(
                        "sp:ModPath {node} minimum repetition {min_u} exceeds limit ({MAX_MOD_PATH_UNROLL})"
                    );
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

    fn collect_args(&self, node: &Term) -> Result<Vec<Term>> {
        let Some(sub) = as_subject(node) else {
            return Ok(Vec::new());
        };
        let mut by_index: Vec<(usize, Term)> = Vec::new();
        for triple in self.g.g.triples_for_subject(sub) {
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

    fn expr_call_type(&self, node: &Term, args: &[Term]) -> Result<Option<NamedNode>> {
        let mut types: Vec<NamedNode> = self
            .g
            .objects(node, rdf::TYPE)
            .into_iter()
            .filter_map(|t| match t {
                Term::NamedNode(n) => Some(n),
                _ => None,
            })
            .filter(|n| {
                !matches!(
                    n.as_str(),
                    "http://spinrdf.org/sp#Expression"
                        | "http://spinrdf.org/sp#Function"
                        | "http://spinrdf.org/sp#Aggregation"
                        | "http://www.w3.org/2000/01/rdf-schema#Resource"
                        | "http://www.w3.org/2002/07/owl#Thing"
                )
            })
            .collect();
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
            if let Some(name) = spin_var_name(&this.g, node)? {
                if let Some(Some(bound)) = this.bindings.get(&name) {
                    return write_term_value(&mut this.out, bound);
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
                return write_term_value(&mut this.out, node);
            }
            // 4. Anonymous sp:Variable blank node
            if let Term::BlankNode(b) = node
                && this.g.is_instance(node, sp::VARIABLE_CLS)
            {
                let var = this.anon_var_name(b);
                this.push("?")?;
                return this.push(&var);
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

            if iri == sp::EXISTS.as_str() {
                let elems = this.required_object(node, sp::ELEMENTS, "sp:Exists")?;
                this.push("EXISTS { ")?;
                this.write_element_list(&elems, depth + 1)?;
                return this.push(" }");
            }
            if iri == sp::NOT_EXISTS.as_str() {
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
                    let Term::Literal(lit) = sep_term else {
                        bail!("sp:separator on {node} must be a string literal");
                    };
                    this.push(" ; SEPARATOR = ")?;
                    write_literal(&mut this.out, &lit)?;
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
                if let Some(name) = spin_var_name(&this.g, &args[0])? {
                    if this.bindings.get(&name).is_some_and(Option::is_some) {
                        return this.push("true");
                    }
                    this.push("BOUND(?")?;
                    this.push(&name)?;
                    return this.push(")");
                }
                if let Term::BlankNode(b) = &args[0]
                    && this.g.is_instance(&args[0], sp::VARIABLE_CLS)
                {
                    let var = this.anon_var_name(b);
                    this.push("BOUND(?")?;
                    this.push(&var)?;
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
